//! Filesystem-based discovery of slash commands. Three sources, merged:
//!
//!   - `~/.roy/skills/<name>/SKILL.md`       (source = `roy`, harness-agnostic)
//!   - `~/.claude/skills/<name>/SKILL.md`    (source = `claude`, legacy)
//!   - `~/.claude/plugins/marketplaces/<m>/(plugins|external_plugins)/<p>/skills/<name>/SKILL.md`
//!     (source = `<p>@<m>`, gated by enabledPlugins in `~/.claude/settings.json`)
//!
//! Each SKILL.md starts with YAML frontmatter (`name`, `description`) and is
//! followed by the markdown body. The body is what roy-web injects into the
//! prompt when the user picks the command — so it works for any harness, not
//! just Claude.
//!
//! Writes go to `~/.roy/skills/` only (POST /commands). The Claude-side
//! directory and plugin marketplaces are read-only as far as roy-management
//! is concerned.

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
        let plugins = read_enabled_plugins(&home).unwrap_or_default();
        let v = list_commands_from(&home, &plugins).await;
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

pub fn claude_skills_dir(home: &Path) -> PathBuf {
    home.join(".claude/skills")
}

pub async fn list_commands_from(home: &Path, enabled_plugins: &[String]) -> Vec<CommandInfo> {
    let mut out = scan_dir(&roy_skills_dir(home), "roy").await;
    out.extend(scan_dir(&claude_skills_dir(home), "claude").await);
    for plugin in enabled_plugins {
        let Some((p, m)) = plugin.split_once('@') else {
            continue;
        };
        let dir = home
            .join(".claude/plugins/marketplaces")
            .join(m)
            .join("plugins")
            .join(p)
            .join("skills");
        out.extend(scan_dir(&dir, plugin).await);
        let dir2 = home
            .join(".claude/plugins/marketplaces")
            .join(m)
            .join("external_plugins")
            .join(p)
            .join("skills");
        out.extend(scan_dir(&dir2, plugin).await);
    }
    // Stable ordering. Same name from two sources (e.g. `roy` shadowing
    // `claude`) keeps both visible — the popover surfaces the source tag.
    out.sort_by(|a, b| a.name.cmp(&b.name).then_with(|| a.source.cmp(&b.source)));
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

/// Read a single skill's body (the markdown after the frontmatter). Searches
/// every source in the same precedence as `list_commands_from`; the first
/// match wins, so a `~/.roy/skills/<name>` entry shadows a same-name Claude
/// or plugin skill.
pub async fn read_command_body(home: &Path, name: &str) -> Option<String> {
    // Validate the name so a request like `/commands/..%2F..%2Fetc%2Fpasswd`
    // can't escape the skills tree.
    if !is_safe_skill_name(name) {
        return None;
    }
    let candidates = [roy_skills_dir(home), claude_skills_dir(home)];
    for dir in &candidates {
        let path = dir.join(name).join("SKILL.md");
        if let Ok(contents) = tokio::fs::read_to_string(&path).await {
            if let Some((_, _, body)) = parse_skill_md(&contents) {
                return Some(body);
            }
        }
    }
    // Plugin marketplaces — we don't know the plugin name from the skill
    // name alone, so walk the marketplaces tree until we hit a match.
    let plugins = read_enabled_plugins(home).unwrap_or_default();
    for plugin in &plugins {
        let Some((p, m)) = plugin.split_once('@') else {
            continue;
        };
        for sub in ["plugins", "external_plugins"] {
            let path = home
                .join(".claude/plugins/marketplaces")
                .join(m)
                .join(sub)
                .join(p)
                .join("skills")
                .join(name)
                .join("SKILL.md");
            if let Ok(contents) = tokio::fs::read_to_string(&path).await {
                if let Some((_, _, body)) = parse_skill_md(&contents) {
                    return Some(body);
                }
            }
        }
    }
    None
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

fn read_enabled_plugins(home: &Path) -> Option<Vec<String>> {
    let raw = std::fs::read_to_string(home.join(".claude/settings.json")).ok()?;
    let v: serde_json::Value = serde_json::from_str(&raw).ok()?;
    let map = v.get("enabledPlugins")?.as_object()?;
    Some(
        map.iter()
            .filter(|(_, v)| v.as_bool() == Some(true))
            .map(|(k, _)| k.clone())
            .collect(),
    )
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
    async fn lists_roy_then_claude() {
        let home = TempDir::new().unwrap();
        write_skill(
            &roy_skills_dir(home.path()),
            "shared",
            "---\nname: shared\ndescription: from roy\n---\n\nROY BODY\n",
        );
        write_skill(
            &claude_skills_dir(home.path()),
            "shared",
            "---\nname: shared\ndescription: from claude\n---\n\nCLAUDE BODY\n",
        );
        let list = list_commands_from(home.path(), &[]).await;
        // Both entries surface, sorted by (name, source).
        assert_eq!(list.len(), 2);
        assert_eq!(list[0].source, "claude");
        assert_eq!(list[1].source, "roy");
    }

    #[tokio::test]
    async fn body_lookup_prefers_roy() {
        let home = TempDir::new().unwrap();
        write_skill(
            &roy_skills_dir(home.path()),
            "shared",
            "---\nname: shared\ndescription: r\n---\n\nROY BODY\n",
        );
        write_skill(
            &claude_skills_dir(home.path()),
            "shared",
            "---\nname: shared\ndescription: c\n---\n\nCLAUDE BODY\n",
        );
        let body = read_command_body(home.path(), "shared").await.unwrap();
        assert!(body.contains("ROY BODY"));
        assert!(!body.contains("CLAUDE BODY"));
    }

    #[tokio::test]
    async fn create_then_list_then_delete() {
        let home = TempDir::new().unwrap();
        create_command(home.path(), "review", "Review code", "Body text")
            .await
            .unwrap();
        let list = list_commands_from(home.path(), &[]).await;
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].name, "review");
        assert_eq!(list[0].source, "roy");

        let body = read_command_body(home.path(), "review").await.unwrap();
        assert!(body.contains("Body text"));

        assert!(delete_command(home.path(), "review").await.unwrap());
        let list = list_commands_from(home.path(), &[]).await;
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
