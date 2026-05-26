//! Filesystem-based discovery of slash commands. Single source:
//!
//!   - `~/.roy/skills/<name>/SKILL.md`  — the canonical, harness-agnostic store.
//!
//! Each SKILL.md starts with YAML frontmatter (`name`, `description`) and is
//! followed by the markdown body. The body is what roy-web injects into the
//! prompt when the user picks the command — so it works for any harness.
//!
//! `~/.claude/skills/` and plugin marketplaces are deliberately NOT scanned:
//! roy owns its catalog. Existing Claude-side skills must be copied (or
//! symlinked) into `~/.roy/skills/` to surface in the popover.

use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use serde::Serialize;

#[derive(Debug, Clone, Serialize)]
pub struct CommandInfo {
    pub name: String,
    pub description: String,
    pub source: String,
}

const CACHE_TTL: Duration = Duration::from_secs(30);

#[derive(Default)]
pub struct CommandsCache {
    inner: Mutex<Option<(Instant, Vec<CommandInfo>)>>,
}

impl CommandsCache {
    pub async fn get(&self) -> Vec<CommandInfo> {
        {
            let g = self.inner.lock().unwrap();
            if let Some((ts, ref v)) = *g {
                if ts.elapsed() < CACHE_TTL {
                    return v.clone();
                }
            }
        }
        let home = dirs::home_dir().unwrap_or_else(|| PathBuf::from("/"));
        let v = list_commands_from(&home).await;
        let mut g = self.inner.lock().unwrap();
        *g = Some((Instant::now(), v.clone()));
        v
    }

    /// Force a refresh on the next `get()` call. Use after POST /commands so
    /// the new entry shows up before the 30s TTL elapses.
    pub fn invalidate(&self) {
        *self.inner.lock().unwrap() = None;
    }
}

pub fn roy_skills_dir(home: &Path) -> PathBuf {
    home.join(".roy/skills")
}

pub async fn list_commands_from(home: &Path) -> Vec<CommandInfo> {
    let mut out = scan_dir(&roy_skills_dir(home), "roy").await;
    out.sort_by(|a, b| a.name.cmp(&b.name));
    out
}

async fn scan_dir(dir: &Path, source: &str) -> Vec<CommandInfo> {
    let mut out = Vec::new();
    let Ok(mut rd) = tokio::fs::read_dir(dir).await else {
        return out;
    };
    while let Ok(Some(entry)) = rd.next_entry().await {
        let skill_md = entry.path().join("SKILL.md");
        let Ok(contents) = tokio::fs::read_to_string(&skill_md).await else {
            continue;
        };
        let Some((name, desc, _body)) = parse_skill_md(&contents) else {
            continue;
        };
        out.push(CommandInfo {
            name,
            description: desc,
            source: source.into(),
        });
    }
    out
}

/// Read a single skill's body (the markdown after the frontmatter) from
/// `~/.roy/skills/<name>/SKILL.md`.
pub async fn read_command_body(home: &Path, name: &str) -> Option<String> {
    // Validate the name so a request like `/commands/..%2F..%2Fetc%2Fpasswd`
    // can't escape the skills tree.
    if !is_safe_skill_name(name) {
        return None;
    }
    let path = roy_skills_dir(home).join(name).join("SKILL.md");
    let contents = tokio::fs::read_to_string(&path).await.ok()?;
    parse_skill_md(&contents).map(|(_, _, body)| body)
}

#[derive(Debug, thiserror::Error)]
pub enum CommandWriteError {
    #[error("invalid name")]
    InvalidName,
    #[error("already exists")]
    AlreadyExists,
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
}

/// Create a new skill under `~/.roy/skills/<name>/SKILL.md`. Refuses to
/// overwrite an existing file; deletes go through `delete_command`.
pub async fn create_command(
    home: &Path,
    name: &str,
    description: &str,
    body: &str,
) -> Result<(), CommandWriteError> {
    if !is_safe_skill_name(name) {
        return Err(CommandWriteError::InvalidName);
    }
    let dir = roy_skills_dir(home).join(name);
    let file = dir.join("SKILL.md");
    if tokio::fs::metadata(&file).await.is_ok() {
        return Err(CommandWriteError::AlreadyExists);
    }
    tokio::fs::create_dir_all(&dir).await?;
    let contents = render_skill_md(name, description, body);
    tokio::fs::write(&file, contents).await?;
    Ok(())
}

/// Delete a roy-owned skill. Claude-side and plugin skills are read-only —
/// the endpoint returns 404 if the name doesn't exist under `~/.roy/skills/`.
pub async fn delete_command(home: &Path, name: &str) -> Result<bool, CommandWriteError> {
    if !is_safe_skill_name(name) {
        return Err(CommandWriteError::InvalidName);
    }
    let dir = roy_skills_dir(home).join(name);
    match tokio::fs::remove_dir_all(&dir).await {
        Ok(()) => Ok(true),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(e) => Err(CommandWriteError::Io(e)),
    }
}

fn render_skill_md(name: &str, description: &str, body: &str) -> String {
    // YAML frontmatter is line-oriented; we strip CR and quote nothing
    // (the parser already accepts plain values). Description on one line.
    let desc_single = description.replace(['\n', '\r'], " ");
    let trimmed_body = body.trim_end_matches('\n');
    format!("---\nname: {name}\ndescription: {desc_single}\n---\n\n{trimmed_body}\n")
}

fn parse_skill_md(s: &str) -> Option<(String, String, String)> {
    let s = s.strip_prefix("---\n")?;
    let end = s.find("\n---")?;
    let front = &s[..end];
    // Skip past `\n---` plus an optional trailing newline.
    let after = &s[end + 4..];
    let body = after.strip_prefix('\n').unwrap_or(after).to_string();
    let (mut name, mut desc) = (None, None);
    for line in front.lines() {
        if let Some(rest) = line.strip_prefix("name:") {
            name = Some(rest.trim().trim_matches('"').to_string());
        } else if let Some(rest) = line.strip_prefix("description:") {
            desc = Some(rest.trim().trim_matches('"').to_string());
        }
    }
    Some((name?, desc?, body))
}

fn is_safe_skill_name(name: &str) -> bool {
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

    fn write_skill(dir: &Path, name: &str, body: &str) {
        let d = dir.join(name);
        std::fs::create_dir_all(&d).unwrap();
        std::fs::write(d.join("SKILL.md"), body).unwrap();
    }

    #[tokio::test]
    async fn lists_roy_skills_only() {
        let home = TempDir::new().unwrap();
        write_skill(
            &roy_skills_dir(home.path()),
            "first",
            "---\nname: first\ndescription: r1\n---\n\nbody1\n",
        );
        // Anything outside `~/.roy/skills/` is ignored — even a same-name
        // SKILL.md under `~/.claude/skills/` does not surface.
        let claude = home.path().join(".claude/skills");
        write_skill(
            &claude,
            "first",
            "---\nname: first\ndescription: c1\n---\n\nfromclaude\n",
        );
        write_skill(
            &claude,
            "claude-only",
            "---\nname: claude-only\ndescription: x\n---\n\nx\n",
        );
        let list = list_commands_from(home.path()).await;
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].name, "first");
        assert_eq!(list[0].source, "roy");
        assert_eq!(list[0].description, "r1");

        // Body read also only sees the roy version.
        let body = read_command_body(home.path(), "first").await.unwrap();
        assert!(body.contains("body1"));
        assert!(read_command_body(home.path(), "claude-only")
            .await
            .is_none());
    }

    #[tokio::test]
    async fn create_then_list_then_delete() {
        let home = TempDir::new().unwrap();
        create_command(home.path(), "review", "Review code", "Body text")
            .await
            .unwrap();
        let list = list_commands_from(home.path()).await;
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].name, "review");
        assert_eq!(list[0].source, "roy");

        let body = read_command_body(home.path(), "review").await.unwrap();
        assert!(body.contains("Body text"));

        assert!(delete_command(home.path(), "review").await.unwrap());
        let list = list_commands_from(home.path()).await;
        assert_eq!(list.len(), 0);
        assert!(!delete_command(home.path(), "review").await.unwrap());
    }

    #[tokio::test]
    async fn rejects_unsafe_names() {
        let home = TempDir::new().unwrap();
        assert!(matches!(
            create_command(home.path(), "../escape", "x", "y").await,
            Err(CommandWriteError::InvalidName)
        ));
        assert!(read_command_body(home.path(), "../escape").await.is_none());
    }

    #[tokio::test]
    async fn refuses_to_overwrite() {
        let home = TempDir::new().unwrap();
        create_command(home.path(), "x", "first", "first body")
            .await
            .unwrap();
        assert!(matches!(
            create_command(home.path(), "x", "second", "second body").await,
            Err(CommandWriteError::AlreadyExists)
        ));
    }
}
