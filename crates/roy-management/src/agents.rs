//! Filesystem-based agent discovery. Single source:
//!
//!   - `~/.roy/agents/<name>.md` — top-level markdown files, one per agent.
//!
//! Each file starts with a YAML frontmatter block. Required keys: `name`,
//! `description`, `engine`. Optional: `model`. The body (after the second
//! `---`) becomes the session's `system_prompt` when the agent is run.
//!
//! Files without `engine` are silently dropped — that is what distinguishes
//! an agent file from a stray markdown note in the same directory.

use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use serde::Serialize;

#[derive(Debug, Clone, Serialize)]
pub struct AgentFile {
    pub name: String,
    pub description: String,
    pub engine: String,
    pub model: Option<String>,
    pub body: String,
}

const CACHE_TTL: Duration = Duration::from_secs(30);

#[derive(Default)]
pub struct AgentsCache {
    inner: Mutex<Option<(Instant, Vec<AgentFile>)>>,
}

impl AgentsCache {
    pub async fn get(&self) -> Vec<AgentFile> {
        {
            let g = self.inner.lock().unwrap();
            if let Some((ts, ref v)) = *g {
                if ts.elapsed() < CACHE_TTL {
                    return v.clone();
                }
            }
        }
        let home = dirs::home_dir().unwrap_or_else(|| PathBuf::from("/"));
        let v = list_agents_from(&home).await;
        let mut g = self.inner.lock().unwrap();
        *g = Some((Instant::now(), v.clone()));
        v
    }

    pub fn invalidate(&self) {
        *self.inner.lock().unwrap() = None;
    }
}

pub fn roy_agents_dir(home: &Path) -> PathBuf {
    home.join(".roy/agents")
}

/// Build the per-user/per-team env-var map that the daemon sets on the
/// spawned ACP child so chat-level skills can locate the right agents
/// directory. `workspace_dir` is the daemon's `$ROY_WORKSPACE_DIR` root.
pub fn spawn_env_for(
    workspace_dir: &Path,
    user_id: &str,
    teams: &[roy_auth::types::TeamMembership],
) -> std::collections::HashMap<String, String> {
    let mut env = std::collections::HashMap::new();
    let user_dir = workspace_dir
        .join("users")
        .join(user_id)
        .join(".roy/agents");
    env.insert(
        "ROY_AGENTS_DIR_USER".to_string(),
        user_dir.to_string_lossy().into_owned(),
    );
    let mut slugs: Vec<String> = Vec::with_capacity(teams.len());
    for t in teams {
        let slug = slugify_team(&t.name);
        let key = format!(
            "ROY_AGENTS_DIR_TEAM_{}",
            slug.to_ascii_uppercase().replace('-', "_"),
        );
        let dir = workspace_dir
            .join("teams")
            .join(&t.id)
            .join(".roy/agents");
        env.insert(key, dir.to_string_lossy().into_owned());
        slugs.push(slug);
    }
    if !slugs.is_empty() {
        env.insert("ROY_TEAMS".to_string(), slugs.join(","));
    }
    env
}

fn slugify_team(name: &str) -> String {
    let raw: String = name
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() {
                c.to_ascii_lowercase()
            } else {
                '-'
            }
        })
        .collect();
    raw.trim_matches('-').to_string()
}

pub async fn list_agents_from(home: &Path) -> Vec<AgentFile> {
    let dir = roy_agents_dir(home);
    let mut out = Vec::new();
    let Ok(mut rd) = tokio::fs::read_dir(&dir).await else {
        return out;
    };
    while let Ok(Some(entry)) = rd.next_entry().await {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("md") {
            continue;
        }
        let stem = match path.file_stem().and_then(|s| s.to_str()) {
            Some(s) if is_safe_agent_name(s) => s.to_string(),
            _ => continue,
        };
        let Ok(contents) = tokio::fs::read_to_string(&path).await else {
            continue;
        };
        let Some(parsed) = parse_agent_md(&contents) else {
            continue;
        };
        let Some(engine) = parsed.engine else { continue };
        out.push(AgentFile {
            name: parsed.name.unwrap_or(stem.clone()),
            description: parsed.description.unwrap_or_default(),
            engine,
            model: parsed.model,
            body: parsed.body,
        });
    }
    out.sort_by(|a, b| a.name.cmp(&b.name));
    out
}

struct ParsedAgent {
    name: Option<String>,
    description: Option<String>,
    engine: Option<String>,
    model: Option<String>,
    body: String,
}

fn parse_agent_md(s: &str) -> Option<ParsedAgent> {
    let s = s.strip_prefix("---\n")?;
    let end = s.find("\n---")?;
    let front = &s[..end];
    let after = &s[end + 4..];
    let body = after.strip_prefix('\n').unwrap_or(after).to_string();
    let (mut name, mut desc, mut engine, mut model) = (None, None, None, None);
    for line in front.lines() {
        if let Some(rest) = line.strip_prefix("name:") {
            name = Some(rest.trim().trim_matches('"').to_string());
        } else if let Some(rest) = line.strip_prefix("description:") {
            desc = Some(rest.trim().trim_matches('"').to_string());
        } else if let Some(rest) = line.strip_prefix("engine:") {
            engine = Some(rest.trim().trim_matches('"').to_string());
        } else if let Some(rest) = line.strip_prefix("model:") {
            model = Some(rest.trim().trim_matches('"').to_string());
        }
    }
    Some(ParsedAgent { name, description: desc, engine, model, body })
}

fn is_safe_agent_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 64
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn write(dir: &Path, name: &str, body: &str) {
        std::fs::create_dir_all(dir).unwrap();
        std::fs::write(dir.join(name), body).unwrap();
    }

    #[tokio::test]
    async fn lists_agents_in_alphabetical_order() {
        let home = TempDir::new().unwrap();
        let dir = roy_agents_dir(home.path());
        write(
            &dir,
            "pirate.md",
            "---\nname: pirate\ndescription: pirate coder\nengine: codex\n---\n\nArr.\n",
        );
        write(
            &dir,
            "marketing.md",
            "---\nname: marketing\ndescription: gtm helper\nengine: claude\nmodel: claude-opus-4-7\n---\n\nYou are a marketer.\n",
        );
        let list = list_agents_from(home.path()).await;
        assert_eq!(list.len(), 2);
        assert_eq!(list[0].name, "marketing");
        assert_eq!(list[0].engine, "claude");
        assert_eq!(list[0].model.as_deref(), Some("claude-opus-4-7"));
        assert!(list[0].body.contains("You are a marketer"));
        assert_eq!(list[1].name, "pirate");
        assert_eq!(list[1].engine, "codex");
        assert_eq!(list[1].model, None);
    }

    #[tokio::test]
    async fn skips_files_without_engine_field() {
        let home = TempDir::new().unwrap();
        let dir = roy_agents_dir(home.path());
        write(
            &dir,
            "skill-only.md",
            "---\nname: notes\ndescription: just a note\n---\n\nbody\n",
        );
        let list = list_agents_from(home.path()).await;
        assert_eq!(list.len(), 0);
    }

    #[tokio::test]
    async fn rejects_unsafe_names() {
        let home = TempDir::new().unwrap();
        let dir = roy_agents_dir(home.path());
        write(
            &dir,
            "../escape.md",
            "---\nname: x\nengine: claude\n---\n\nx",
        );
        let list = list_agents_from(home.path()).await;
        assert_eq!(list.len(), 0);
    }

    #[test]
    fn spawn_env_for_personal_only() {
        let env = spawn_env_for(
            std::path::Path::new("/ws"),
            "user-uuid",
            &[],
        );
        assert_eq!(env["ROY_AGENTS_DIR_USER"], "/ws/users/user-uuid/.roy/agents");
        assert!(!env.contains_key("ROY_TEAMS"));
    }

    #[test]
    fn spawn_env_for_with_teams() {
        use roy_auth::types::{Role, TeamMembership};
        let teams = vec![
            TeamMembership {
                id: "tid-1".into(),
                name: "GTM Team".into(),
                role: Role::Member,
            },
            TeamMembership {
                id: "tid-2".into(),
                name: "Eng".into(),
                role: Role::Owner,
            },
        ];
        let env = spawn_env_for(std::path::Path::new("/ws"), "u", &teams);
        assert_eq!(env["ROY_AGENTS_DIR_TEAM_GTM_TEAM"], "/ws/teams/tid-1/.roy/agents");
        assert_eq!(env["ROY_AGENTS_DIR_TEAM_ENG"], "/ws/teams/tid-2/.roy/agents");
        assert_eq!(env["ROY_TEAMS"], "gtm-team,eng");
    }

    #[test]
    fn parses_minimal_frontmatter() {
        let p = parse_agent_md(
            "---\nname: x\nengine: claude\n---\n\nhello\n",
        )
        .unwrap();
        assert_eq!(p.name.as_deref(), Some("x"));
        assert_eq!(p.engine.as_deref(), Some("claude"));
        assert_eq!(p.body.trim(), "hello");
        assert!(p.description.is_none());
        assert!(p.model.is_none());
    }
}
