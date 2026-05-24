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
use async_trait::async_trait;
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

pub async fn execute(
    socket_path: &Path,
    cfg: &Config,
    fire_result: &FireSuccess,
) -> super::Outcome {
    let body = match &cfg.prefix {
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
        Ok(FireOutcome::Done(_)) => super::Outcome::ok(),
        Ok(FireOutcome::Timeout { .. }) => super::Outcome::error("parent stayed busy past 5min"),
        Ok(FireOutcome::Error { code, message, .. }) => {
            super::Outcome::error(format!("{code}: {message}"))
        }
        Err(e) => super::Outcome::error(format!("roy_client: {e:#}")),
    }
}

pub fn build(config_json: &str) -> Result<Box<dyn super::Subscriber>> {
    let cfg = parse_config(config_json)?;
    Ok(Box::new(InjectParentSubscriber { cfg }))
}

pub struct InjectParentSubscriber {
    cfg: Config,
}

#[async_trait]
impl super::Subscriber for InjectParentSubscriber {
    async fn run(&self, ctx: &super::FireCtx<'_>) -> super::Outcome {
        let Some(success) = ctx.success else {
            return super::Outcome::skipped("inject_parent skipped (fire did not succeed)");
        };
        execute(ctx.socket_path, &self.cfg, success).await
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

        let cfg = parse_config(r#"{"session_id":"parent-sid"}"#).unwrap();
        let out = execute(&path, &cfg, &fake_success()).await;
        assert_eq!(out.status, super::super::RunStatus::Ok);
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

        let cfg = parse_config(r#"{"session_id":"parent-sid"}"#).unwrap();
        let out = execute(&path, &cfg, &fake_success()).await;
        assert_eq!(out.status, super::super::RunStatus::Error);
        assert!(out.error_message.unwrap().contains("no_session"));
    }
}
