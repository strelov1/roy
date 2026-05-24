//! notify_native subscriber — macOS native notification via osascript,
//! Linux via notify-send. Falls back to a tracing warn on other platforms
//! so the run still reports an outcome.

use std::process::Command;

use anyhow::{Context, Result};
use async_trait::async_trait;
use serde::Deserialize;

use crate::roy_client::FireSuccess;

#[derive(Debug, Deserialize)]
pub struct Config {
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub sound: Option<String>,
}

pub fn parse_config(json: &str) -> Result<Config> {
    serde_json::from_str(json).context("notify_native config")
}

pub struct ExecOutcome {
    pub status: &'static str,
    pub error_message: Option<String>,
}

pub async fn execute_with_cfg(
    cfg: &Config,
    agent_name: &str,
    success: &FireSuccess,
) -> ExecOutcome {
    let title = cfg
        .title
        .clone()
        .unwrap_or_else(|| format!("roy-scheduler: {agent_name}"));
    let body = first_line_or_summary(&success.assistant_text);

    // `std::process::Command::status()` is a blocking syscall. With
    // max_fires=8 several fire tasks could block tokio workers
    // simultaneously and starve the scheduler poll loop. Move the syscall
    // off the runtime via spawn_blocking; the cfg-gated platform branches
    // are unchanged.
    tokio::task::spawn_blocking(move || run_blocking(title, body))
        .await
        .unwrap_or_else(|_| err("spawn_blocking panicked".into()))
}

pub fn build(config_json: &str) -> Result<Box<dyn super::Subscriber>> {
    let cfg = parse_config(config_json)?;
    Ok(Box::new(NotifyNativeSubscriber { cfg }))
}

pub struct NotifyNativeSubscriber {
    cfg: Config,
}

#[async_trait]
impl super::Subscriber for NotifyNativeSubscriber {
    async fn run(&self, ctx: &super::FireCtx<'_>) -> super::Outcome {
        let Some(success) = ctx.success else {
            return super::Outcome::skipped("notify_native skipped (fire did not succeed)");
        };
        let exec = execute_with_cfg(&self.cfg, ctx.agent_name, success).await;
        match exec.status {
            "ok" => super::Outcome::ok(),
            _ => super::Outcome::error(
                exec.error_message
                    .unwrap_or_else(|| "notify_native failed".into()),
            ),
        }
    }
}

fn run_blocking(title: String, body: String) -> ExecOutcome {
    #[cfg(target_os = "macos")]
    {
        let script = format!(
            "display notification \"{}\" with title \"{}\"",
            escape_applescript(&body),
            escape_applescript(&title),
        );
        match Command::new("osascript").arg("-e").arg(script).status() {
            Ok(s) if s.success() => return ok(),
            Ok(s) => return err(format!("osascript exited {s}")),
            Err(e) => return err(format!("osascript spawn: {e}")),
        }
    }

    #[cfg(target_os = "linux")]
    {
        let mut cmd = Command::new("notify-send");
        cmd.arg(&title).arg(&body);
        match cmd.status() {
            Ok(s) if s.success() => return ok(),
            Ok(s) => return err(format!("notify-send exited {s}")),
            Err(e) => return err(format!("notify-send spawn: {e}")),
        }
    }

    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    {
        tracing::warn!(
            target = "roy_scheduler::subscribers::notify_native",
            "no native notifier on this platform; title={title} body={body}"
        );
        ok()
    }
}

fn ok() -> ExecOutcome {
    ExecOutcome {
        status: "ok",
        error_message: None,
    }
}

fn err(msg: String) -> ExecOutcome {
    ExecOutcome {
        status: "error",
        error_message: Some(msg),
    }
}

#[allow(dead_code)] // used only when cfg(target_os="macos")
fn escape_applescript(s: &str) -> String {
    s.replace('\\', "\\\\").replace('"', "\\\"")
}

fn first_line_or_summary(text: &str) -> String {
    let line = text.lines().next().unwrap_or("").trim();
    if line.is_empty() {
        return "(empty)".into();
    }
    // Slice at a char boundary so multi-byte (cyrillic / CJK / emoji)
    // content does not panic the fire task.
    let cut = line
        .char_indices()
        .nth(200)
        .map(|(i, _)| i)
        .unwrap_or(line.len());
    if cut == line.len() {
        line.into()
    } else {
        format!("{}…", &line[..cut])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[allow(dead_code)]
    fn fake_success(text: &str) -> FireSuccess {
        FireSuccess {
            session_id: "s".into(),
            seq_range: (0, 1),
            cost_usd: None,
            stop_reason: "EndTurn".into(),
            assistant_text: text.into(),
        }
    }

    #[test]
    fn first_line_strips_trailing_chunks() {
        assert_eq!(first_line_or_summary("hello\nworld"), "hello");
    }

    #[test]
    fn first_line_empty_input_yields_placeholder() {
        assert_eq!(first_line_or_summary(""), "(empty)");
    }

    #[test]
    fn first_line_truncates_long_lines() {
        let long: String = std::iter::repeat('x').take(300).collect();
        let out = first_line_or_summary(&long);
        assert!(out.ends_with('…'));
        // 200 'x' + '…' = 201 chars total (in display width). Char count check.
        assert_eq!(out.chars().count(), 201);
    }

    #[test]
    fn first_line_handles_multibyte_codepoints_without_panic() {
        // 250 cyrillic chars (each 2 bytes in UTF-8) — used to panic on &line[..200].
        let s: String = std::iter::repeat('я').take(250).collect();
        let out = first_line_or_summary(&s);
        assert!(out.ends_with('…'));
        assert_eq!(out.chars().count(), 201);
    }

    #[test]
    fn escape_applescript_escapes_quotes_and_backslashes() {
        assert_eq!(escape_applescript("a\"b\\c"), "a\\\"b\\\\c");
    }

    #[test]
    fn parse_config_accepts_empty_object() {
        assert!(parse_config("{}").is_ok());
    }

    // No live-execute test — we don't want CI to fire desktop notifications.
    // The cfg-gated platform branch is exercised manually during smoke.
}
