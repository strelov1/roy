//! inject_parent subscriber — resume the parent session and send the
//! formatted fire result as the next user turn.
//!
//! Behaviour on parent state:
//! - Live and idle  → send immediately.
//! - Live and busy  → WaitForResult on the parent (5 min cap), then send.
//! - Not live       → SessionNotFound bubbles up as a subscriber error.
//!
//! v1 config:
//!   { "session_id": "<roy session id>", "prefix": "optional string" }

use std::path::Path;
use std::time::Duration;

use anyhow::{Context, Result};
use serde::Deserialize;

use crate::roy_client::{self, FireOutcome, FireSuccess};

#[derive(Debug, Deserialize)]
pub struct Config {
    pub session_id: String,
    #[serde(default)]
    pub prefix: Option<String>,
}

pub fn parse_config(json: &str) -> Result<Config> {
    serde_json::from_str(json).context("inject_parent config")
}

pub struct ExecOutcome {
    pub status: &'static str, // "ok" | "error"
    pub error_message: Option<String>,
}

pub async fn execute(
    socket_path: &Path,
    config_json: &str,
    fire_result: &FireSuccess,
) -> ExecOutcome {
    let cfg = match parse_config(config_json) {
        Ok(c) => c,
        Err(e) => {
            return ExecOutcome {
                status: "error",
                error_message: Some(format!("config: {e}")),
            };
        }
    };

    let body = match cfg.prefix {
        Some(p) => format!("{p}{}", fire_result.assistant_text),
        None => fire_result.assistant_text.clone(),
    };

    // Wait for parent to be idle (cheap if it already is), then Fire-Resume
    // to inject. We use Fire here rather than separate Resume + Send so the
    // round-trip is one call and we get an explicit success/timeout/error
    // back from the daemon.
    let outcome = roy_client::fire(
        socket_path,
        roy::FireTarget::Resume {
            session_id: cfg.session_id.clone(),
        },
        body,
        std::collections::BTreeMap::new(),
        Duration::from_secs(5 * 60),
    )
    .await;

    match outcome {
        Ok(FireOutcome::Done(_)) => ExecOutcome {
            status: "ok",
            error_message: None,
        },
        Ok(FireOutcome::Timeout { .. }) => ExecOutcome {
            status: "error",
            error_message: Some("parent stayed busy past 5min".into()),
        },
        Ok(FireOutcome::Error { code, message, .. }) => ExecOutcome {
            status: "error",
            error_message: Some(format!("{code}: {message}")),
        },
        Err(e) => ExecOutcome {
            status: "error",
            error_message: Some(format!("roy_client: {e:#}")),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
    use tokio::net::UnixListener;

    async fn spawn_mock(path: std::path::PathBuf, reply: roy::ServerEvent) {
        let listener = UnixListener::bind(&path).unwrap();
        tokio::spawn(async move {
            let (sock, _) = listener.accept().await.unwrap();
            let (rd, mut wr) = sock.into_split();
            let mut lines = BufReader::new(rd).lines();
            let _ = lines.next_line().await.unwrap();
            let out = serde_json::to_string(&reply).unwrap();
            wr.write_all(out.as_bytes()).await.unwrap();
            wr.write_all(b"\n").await.unwrap();
        });
    }

    fn fake_success() -> FireSuccess {
        FireSuccess {
            session_id: "child-sid".into(),
            seq_range: (0, 5),
            cost_usd: None,
            stop_reason: "EndTurn".into(),
            assistant_text: "the digest".into(),
        }
    }

    #[tokio::test]
    async fn parses_config_with_prefix() {
        let c = parse_config(r#"{"session_id":"sid","prefix":"[bg] "}"#).unwrap();
        assert_eq!(c.session_id, "sid");
        assert_eq!(c.prefix.as_deref(), Some("[bg] "));
    }

    #[tokio::test]
    async fn parses_config_without_prefix() {
        let c = parse_config(r#"{"session_id":"sid"}"#).unwrap();
        assert!(c.prefix.is_none());
    }

    #[tokio::test]
    async fn execute_ok_when_daemon_returns_fire_done() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("sock");
        spawn_mock(
            path.clone(),
            roy::ServerEvent::FireDone {
                session: "parent-sid".into(),
                seq_range: (100, 110),
                result: roy::TurnEvent::Result {
                    cost_usd: None,
                    stop_reason: roy::StopReason::EndTurn,
                },
                assistant_text: "".into(),
            },
        )
        .await;

        let out = execute(&path, r#"{"session_id":"parent-sid"}"#, &fake_success()).await;
        assert_eq!(out.status, "ok");
    }

    #[tokio::test]
    async fn execute_error_when_daemon_returns_fire_error() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("sock");
        spawn_mock(
            path.clone(),
            roy::ServerEvent::FireError {
                session: Some("parent-sid".into()),
                code: roy::ErrorCode::NoSession,
                message: "gone".into(),
            },
        )
        .await;

        let out = execute(&path, r#"{"session_id":"parent-sid"}"#, &fake_success()).await;
        assert_eq!(out.status, "error");
        assert!(out.error_message.unwrap().contains("no_session"));
    }
}
