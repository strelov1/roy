//! Filesystem-based discovery of slash commands. Two sources:
//!  - <HOME>/.claude/skills/<name>/SKILL.md            ("user" source)
//!  - <HOME>/.claude/plugins/marketplaces/<m>/{plugins,external_plugins}/<p>/skills/<name>/SKILL.md
//!    (source = "<p>@<m>"), gated by enabledPlugins in ~/.claude/settings.json.
//!
//! Each SKILL.md has YAML frontmatter with `name` and `description`.

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
}

pub async fn list_commands_from(home: &Path, enabled_plugins: &[String]) -> Vec<CommandInfo> {
    let mut out = scan_dir(&home.join(".claude/skills"), "user").await;
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
        let Some((name, desc)) = parse_frontmatter(&contents) else {
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

fn parse_frontmatter(s: &str) -> Option<(String, String)> {
    let s = s.strip_prefix("---\n")?;
    let end = s.find("\n---")?;
    let body = &s[..end];
    let (mut name, mut desc) = (None, None);
    for line in body.lines() {
        if let Some(rest) = line.strip_prefix("name:") {
            name = Some(rest.trim().trim_matches('"').to_string());
        } else if let Some(rest) = line.strip_prefix("description:") {
            desc = Some(rest.trim().trim_matches('"').to_string());
        }
    }
    Some((name?, desc?))
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
