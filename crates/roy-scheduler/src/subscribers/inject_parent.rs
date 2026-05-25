//! inject_parent subscriber — drop the fire's result into the parent session.
//!
//! Default (`respond: false`): append a `Note` event referencing the child
//! session. No input lease needed, so it lands even while an interactive
//! client (roy-web) is holding the parent session's lease.
//!
//! `respond: true`: deliver the result as a real user turn the parent agent
//! answers. The daemon waits for any in-flight turn first; a session the user
//! is actively typing into may still race.
//!
//! v1 config:
//!   { "session_id": "<roy session id>", "prefix": "optional", "respond": false }

use std::path::Path;
use std::time::Duration;

use anyhow::{Context, Result};
use async_trait::async_trait;
use serde::Deserialize;

use crate::roy_client::{self, FireSuccess, InjectOutcome};

#[derive(Debug, Deserialize)]
pub struct Config {
    pub session_id: String,
    #[serde(default)]
    pub prefix: Option<String>,
    #[serde(default)]
    pub respond: bool,
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

    let outcome = roy_client::inject(
        socket_path,
        cfg.session_id.clone(),
        body,
        Some(fire_result.session_id.clone()),
        cfg.respond,
        Duration::from_secs(5 * 60),
    )
    .await;

    match outcome {
        Ok(InjectOutcome::Noted { .. }) | Ok(InjectOutcome::Done(_)) => super::Outcome::ok(),
        Ok(InjectOutcome::Timeout { .. }) => super::Outcome::error("parent stayed busy past 5min"),
        Ok(InjectOutcome::Error { code, message, .. }) => {
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

    #[tokio::test]
    async fn parses_config_with_respond() {
        let c = parse_config(r#"{"session_id":"sid","respond":true}"#).unwrap();
        assert_eq!(c.session_id, "sid");
        assert!(c.respond);
    }

    #[tokio::test]
    async fn respond_defaults_false() {
        let c = parse_config(r#"{"session_id":"sid"}"#).unwrap();
        assert!(!c.respond);
    }

    #[tokio::test]
    async fn execute_ok_when_daemon_returns_injected() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("sock");
        spawn_mock(
            path.clone(),
            roy::ServerEvent::Injected {
                session: "parent-sid".into(),
                seq: 12,
            },
        )
        .await;

        let cfg = parse_config(r#"{"session_id":"parent-sid"}"#).unwrap();
        let out = execute(&path, &cfg, &fake_success()).await;
        assert_eq!(out.status, super::super::RunStatus::Ok);
    }
}
