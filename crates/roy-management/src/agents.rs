//! Filesystem-based agent discovery. Three sources:
//!
//!   - `<builtin_dir>/` — built-in agents shipped with the daemon image.
//!   - `<workspace>/users/<uid>/.roy/agents/` — personal agents.
//!   - `<workspace>/teams/<tid>/.roy/agents/` — team-shared agents.
//!
//! Each file starts with a YAML frontmatter block. Required keys: `name`,
//! `description`, `harness`. Optional: `model`. The body (after the second
//! `---`) becomes the session's `system_prompt` when the agent is run.
//!
//! Files without `harness` are silently dropped — that is what distinguishes
//! an agent file from a stray markdown note in the same directory.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use serde::Serialize;

#[derive(Debug, Clone, Serialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum AgentScope {
    Builtin,
    Personal,
    Team { team_id: String },
}

#[derive(Debug, Clone, Serialize)]
pub struct AgentFile {
    /// File stem (`<slug>.md`). Stable id used by channel bindings.
    pub slug: String,
    pub name: String,
    pub description: String,
    pub harness: String,
    pub model: Option<String>,
    pub body: String,
    pub scope: AgentScope,
}

const CACHE_TTL: Duration = Duration::from_secs(30);

type CacheKey = (String, Vec<String>);

#[derive(Default)]
pub struct AgentsCache {
    inner: Mutex<HashMap<CacheKey, (Instant, Vec<AgentFile>)>>,
}

impl AgentsCache {
    pub async fn get(
        &self,
        builtin_dir: &Path,
        workspace_dir: &Path,
        user_id: &str,
        team_ids: &[String],
    ) -> Vec<AgentFile> {
        let mut tids: Vec<String> = team_ids.to_vec();
        tids.sort();
        let key = (user_id.to_string(), tids.clone());
        {
            let g = self.inner.lock().unwrap();
            if let Some((ts, ref v)) = g.get(&key) {
                if ts.elapsed() < CACHE_TTL {
                    return v.clone();
                }
            }
        }
        let v = list_all_agents(builtin_dir, workspace_dir, user_id, &tids).await;
        let mut g = self.inner.lock().unwrap();
        g.insert(key, (Instant::now(), v.clone()));
        v
    }

    pub fn invalidate(&self) {
        self.inner.lock().unwrap().clear();
    }
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
        // Slugify the team name. If the result is empty (e.g. the name is
        // entirely non-ASCII like "Маркетинг"), fall back to a deterministic
        // id-derived slug so the env-var key is always valid shell syntax.
        let slug = {
            let s = slugify_team(&t.name);
            if s.is_empty() {
                format!("team-{}", &t.id[..t.id.len().min(8)])
            } else {
                s
            }
        };
        let key = format!(
            "ROY_AGENTS_DIR_TEAM_{}",
            slug.to_ascii_uppercase().replace('-', "_"),
        );
        let dir = workspace_dir.join("teams").join(&t.id).join(".roy/agents");
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

async fn list_dir(dir: &Path, scope: AgentScope) -> Vec<AgentFile> {
    let mut out = Vec::new();
    let Ok(mut rd) = tokio::fs::read_dir(dir).await else {
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
        let Some(harness) = parsed.harness else {
            continue;
        };
        out.push(AgentFile {
            slug: stem.clone(),
            name: parsed.name.unwrap_or(stem),
            description: parsed.description.unwrap_or_default(),
            harness,
            model: parsed.model,
            body: parsed.body,
            scope: scope.clone(),
        });
    }
    out.sort_by(|a, b| a.name.cmp(&b.name));
    out
}

pub async fn list_all_agents(
    builtin_dir: &Path,
    workspace_dir: &Path,
    user_id: &str,
    team_ids: &[String],
) -> Vec<AgentFile> {
    let mut out = list_dir(builtin_dir, AgentScope::Builtin).await;
    let user_dir = workspace_dir
        .join("users")
        .join(user_id)
        .join(".roy/agents");
    out.extend(list_dir(&user_dir, AgentScope::Personal).await);
    for tid in team_ids {
        let team_dir = workspace_dir.join("teams").join(tid).join(".roy/agents");
        out.extend(
            list_dir(
                &team_dir,
                AgentScope::Team {
                    team_id: tid.clone(),
                },
            )
            .await,
        );
    }
    out
}

/// Resolve a single agent file `<dir>/<slug>.md` to its persona, returning
/// `(harness, model, system_prompt_body)`. `None` if the slug is unsafe, the
/// file is missing/unparseable, or it lacks the required `harness` field.
pub async fn read_agent_persona(
    dir: &Path,
    slug: &str,
) -> Option<(String, Option<String>, String)> {
    if !is_safe_agent_name(slug) {
        return None;
    }
    let path = dir.join(format!("{slug}.md"));
    let contents = tokio::fs::read_to_string(&path).await.ok()?;
    let parsed = parse_agent_md(&contents)?;
    let harness = parsed.harness?;
    Some((harness, parsed.model, parsed.body))
}

/// Resolve the agent directory a channel binding's `agent_scope` points at:
/// `"user"` → `<workspace>/users/<owner_id>/.roy/agents`, `"team:<team_id>"` →
/// `<workspace>/teams/<team_id>/.roy/agents`. `None` for an unrecognized scope.
/// Mirrors the directory layout in `list_all_agents`, so both live in one file.
pub fn agent_scope_dir(workspace_dir: &Path, owner_id: &str, scope: &str) -> Option<PathBuf> {
    if scope == "user" {
        Some(
            workspace_dir
                .join("users")
                .join(owner_id)
                .join(".roy/agents"),
        )
    } else if let Some(team_id) = scope.strip_prefix("team:") {
        (!team_id.is_empty()).then(|| {
            workspace_dir
                .join("teams")
                .join(team_id)
                .join(".roy/agents")
        })
    } else {
        None
    }
}

struct ParsedAgent {
    name: Option<String>,
    description: Option<String>,
    harness: Option<String>,
    model: Option<String>,
    body: String,
}

fn parse_agent_md(s: &str) -> Option<ParsedAgent> {
    let s = s.strip_prefix("---\n")?;
    let end = s.find("\n---")?;
    let front = &s[..end];
    let after = &s[end + 4..];
    let body = after.strip_prefix('\n').unwrap_or(after).to_string();
    let (mut name, mut desc, mut harness, mut model) = (None, None, None, None);
    for line in front.lines() {
        if let Some(rest) = line.strip_prefix("name:") {
            name = Some(rest.trim().trim_matches('"').to_string());
        } else if let Some(rest) = line.strip_prefix("description:") {
            desc = Some(rest.trim().trim_matches('"').to_string());
        } else if let Some(rest) = line.strip_prefix("harness:") {
            harness = Some(rest.trim().trim_matches('"').to_string());
        } else if let Some(rest) = line.strip_prefix("model:") {
            model = Some(rest.trim().trim_matches('"').to_string());
        }
    }
    Some(ParsedAgent {
        name,
        description: desc,
        harness,
        model,
        body,
    })
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
        let dir = home.path().join("workspace/users/u1/.roy/agents");
        write(
            &dir,
            "pirate.md",
            "---\nname: pirate\ndescription: pirate coder\nharness: codex\n---\n\nArr.\n",
        );
        write(
            &dir,
            "marketing.md",
            "---\nname: marketing\ndescription: gtm helper\nharness: claude\nmodel: claude-opus-4-7\n---\n\nYou are a marketer.\n",
        );
        let list = list_all_agents(
            &home.path().join("builtin"),
            &home.path().join("workspace"),
            "u1",
            &[],
        )
        .await;
        assert_eq!(list.len(), 2);
        assert_eq!(list[0].name, "marketing");
        assert_eq!(list[0].harness, "claude");
        assert_eq!(list[0].model.as_deref(), Some("claude-opus-4-7"));
        assert!(list[0].body.contains("You are a marketer"));
        assert!(matches!(list[0].scope, AgentScope::Personal));
        assert_eq!(list[1].name, "pirate");
        assert_eq!(list[1].harness, "codex");
        assert_eq!(list[1].model, None);
        assert!(matches!(list[1].scope, AgentScope::Personal));
    }

    #[tokio::test]
    async fn skips_files_without_harness_field() {
        let home = TempDir::new().unwrap();
        let dir = home.path().join("workspace/users/u1/.roy/agents");
        write(
            &dir,
            "skill-only.md",
            "---\nname: notes\ndescription: just a note\n---\n\nbody\n",
        );
        let list = list_all_agents(
            &home.path().join("builtin"),
            &home.path().join("workspace"),
            "u1",
            &[],
        )
        .await;
        assert_eq!(list.len(), 0);
    }

    #[tokio::test]
    async fn rejects_unsafe_names() {
        let home = TempDir::new().unwrap();
        let dir = home.path().join("workspace/users/u1/.roy/agents");
        write(
            &dir,
            "../escape.md",
            "---\nname: x\nharness: claude\n---\n\nx",
        );
        let list = list_all_agents(
            &home.path().join("builtin"),
            &home.path().join("workspace"),
            "u1",
            &[],
        )
        .await;
        assert_eq!(list.len(), 0);
    }

    #[tokio::test]
    async fn lists_builtin_personal_and_team() {
        let home = TempDir::new().unwrap();
        // builtin
        write(
            &home.path().join("builtin"),
            "roy-coder.md",
            "---\nname: roy-coder\ndescription: bi\nharness: claude\n---\nbody",
        );
        // personal
        write(
            &home.path().join("workspace/users/u1/.roy/agents"),
            "pirate.md",
            "---\nname: pirate\ndescription: arr\nharness: codex\n---\nbody",
        );
        // team
        write(
            &home.path().join("workspace/teams/tid-1/.roy/agents"),
            "gtm.md",
            "---\nname: gtm\ndescription: gtm\nharness: codex\n---\nbody",
        );
        let list = list_all_agents(
            &home.path().join("builtin"),
            &home.path().join("workspace"),
            "u1",
            &["tid-1".to_string()],
        )
        .await;
        assert_eq!(list.len(), 3);
        // Built-in first, then personal, then team — by source order in list_all_agents
        assert_eq!(list[0].name, "roy-coder");
        assert!(matches!(list[0].scope, AgentScope::Builtin));
        assert_eq!(list[1].name, "pirate");
        assert!(matches!(list[1].scope, AgentScope::Personal));
        assert_eq!(list[2].name, "gtm");
        assert!(matches!(&list[2].scope, AgentScope::Team { team_id } if team_id == "tid-1"));
    }

    #[test]
    fn spawn_env_for_personal_only() {
        let env = spawn_env_for(std::path::Path::new("/ws"), "user-uuid", &[]);
        assert_eq!(
            env["ROY_AGENTS_DIR_USER"],
            "/ws/users/user-uuid/.roy/agents"
        );
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
        assert_eq!(
            env["ROY_AGENTS_DIR_TEAM_GTM_TEAM"],
            "/ws/teams/tid-1/.roy/agents"
        );
        assert_eq!(
            env["ROY_AGENTS_DIR_TEAM_ENG"],
            "/ws/teams/tid-2/.roy/agents"
        );
        assert_eq!(env["ROY_TEAMS"], "gtm-team,eng");
    }

    #[test]
    fn spawn_env_for_non_ascii_team_name_falls_back_to_id() {
        use roy_auth::types::{Role, TeamMembership};
        let teams = vec![TeamMembership {
            id: "abcd1234-ef56-7890-1234-567890abcdef".into(),
            name: "Маркетинг".into(),
            role: Role::Owner,
        }];
        let env = spawn_env_for(std::path::Path::new("/ws"), "u", &teams);
        // Non-ASCII name slugifies to empty, so we fall back to team-<id-prefix>.
        assert_eq!(
            env["ROY_AGENTS_DIR_TEAM_TEAM_ABCD1234"],
            "/ws/teams/abcd1234-ef56-7890-1234-567890abcdef/.roy/agents",
        );
        assert_eq!(env["ROY_TEAMS"], "team-abcd1234");
    }

    #[test]
    fn parses_minimal_frontmatter() {
        let p = parse_agent_md("---\nname: x\nharness: claude\n---\n\nhello\n").unwrap();
        assert_eq!(p.name.as_deref(), Some("x"));
        assert_eq!(p.harness.as_deref(), Some("claude"));
        assert_eq!(p.body.trim(), "hello");
        assert!(p.description.is_none());
        assert!(p.model.is_none());
    }

    #[tokio::test]
    async fn exposes_file_stem_as_slug() {
        let home = TempDir::new().unwrap();
        let dir = home.path().join("workspace/users/u1/.roy/agents");
        // Frontmatter name deliberately differs from the file stem.
        write(
            &dir,
            "support-l1.md",
            "---\nname: Support L1\ndescription: d\nharness: claude\n---\nbody\n",
        );
        let list = list_all_agents(
            &home.path().join("builtin"),
            &home.path().join("workspace"),
            "u1",
            &[],
        )
        .await;
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].slug, "support-l1");
        assert_eq!(list[0].name, "Support L1");
    }

    #[tokio::test]
    async fn read_persona_by_slug() {
        let home = TempDir::new().unwrap();
        let dir = home.path().join("agents");
        write(
            &dir,
            "support-l1.md",
            "---\nname: Support\ndescription: d\nharness: claude\nmodel: claude-opus-4-8\n---\n\nYou are support.\n",
        );
        let (harness, model, body) = read_agent_persona(&dir, "support-l1").await.unwrap();
        assert_eq!(harness, "claude");
        assert_eq!(model.as_deref(), Some("claude-opus-4-8"));
        assert!(body.contains("You are support."));

        // unsafe slug / missing file / no harness → None
        assert!(read_agent_persona(&dir, "../escape").await.is_none());
        assert!(read_agent_persona(&dir, "missing").await.is_none());
    }
}
