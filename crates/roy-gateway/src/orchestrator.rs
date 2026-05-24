//! The pipeline that turns one inbound chat message into one outbound reply.
//!
//! Stateless except for the `SessionBinder`. The two traits below are the
//! seams against which we mock for unit tests; production wires
//! `DaemonClient` to `Fire` and `TeloxideReplier` to `Replier`.

use std::collections::BTreeMap;
use std::time::Duration;

use anyhow::Result;
use async_trait::async_trait;

use crate::binder::SessionBinder;
use crate::daemon::FireOutcome;

#[async_trait]
pub trait Fire: Send + Sync {
    async fn fire_spawn(
        &self,
        preset: &str,
        project_id: Option<String>,
        prompt: String,
        tags: BTreeMap<String, String>,
        timeout: Duration,
    ) -> Result<FireOutcome>;

    async fn fire_resume(
        &self,
        session_id: &str,
        prompt: String,
        tags: BTreeMap<String, String>,
        timeout: Duration,
    ) -> Result<FireOutcome>;
}

#[async_trait]
pub trait Replier: Send + Sync {
    async fn send(&self, chat_id: i64, text: &str) -> Result<()>;
}

#[derive(Debug, Clone)]
pub struct OrchestratorConfig {
    pub preset: String,
    pub project_id: Option<String>,
    pub turn_timeout: Duration,
}

pub async fn handle_message<F, R>(
    cfg: &OrchestratorConfig,
    binder: &SessionBinder,
    fire: &F,
    replier: &R,
    chat_id: i64,
    prompt: String,
) -> Result<()>
where
    F: Fire,
    R: Replier,
{
    let mut tags = BTreeMap::new();
    tags.insert("channel".into(), "telegram".into());
    tags.insert("chat_id".into(), chat_id.to_string());

    let outcome = match binder.get(chat_id).await {
        Some(session_id) => {
            fire.fire_resume(&session_id, prompt, tags, cfg.turn_timeout)
                .await?
        }
        None => {
            fire.fire_spawn(
                &cfg.preset,
                cfg.project_id.clone(),
                prompt,
                tags,
                cfg.turn_timeout,
            )
            .await?
        }
    };

    match outcome {
        FireOutcome::Done {
            session,
            assistant_text,
        } => {
            binder.set(chat_id, session).await?;
            let text = if assistant_text.is_empty() {
                "(empty reply)".to_string()
            } else {
                assistant_text
            };
            replier.send(chat_id, &text).await?;
        }
        FireOutcome::Timeout { session } => {
            if let Some(s) = session {
                binder.set(chat_id, s).await?;
            }
            replier
                .send(
                    chat_id,
                    "⏱ turn timed out — send another message to continue",
                )
                .await?;
        }
        FireOutcome::Error {
            session,
            code,
            message,
        } => {
            if let Some(s) = session {
                binder.set(chat_id, s).await?;
            }
            replier
                .send(chat_id, &format!("⚠ {code}: {message}"))
                .await?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use roy::control::ErrorCode;
    use std::sync::Mutex;
    use tempfile::TempDir;

    struct MockFire {
        on_spawn: Mutex<Option<FireOutcome>>,
        on_resume: Mutex<Option<FireOutcome>>,
        last_spawn: Mutex<Option<(String, Option<String>, String)>>,
        last_resume: Mutex<Option<(String, String)>>,
    }

    impl MockFire {
        fn new() -> Self {
            Self {
                on_spawn: Mutex::new(None),
                on_resume: Mutex::new(None),
                last_spawn: Mutex::new(None),
                last_resume: Mutex::new(None),
            }
        }
    }

    #[async_trait]
    impl Fire for MockFire {
        async fn fire_spawn(
            &self,
            preset: &str,
            project_id: Option<String>,
            prompt: String,
            _tags: BTreeMap<String, String>,
            _timeout: Duration,
        ) -> Result<FireOutcome> {
            *self.last_spawn.lock().unwrap() = Some((preset.into(), project_id, prompt));
            Ok(self
                .on_spawn
                .lock()
                .unwrap()
                .take()
                .expect("on_spawn not set"))
        }
        async fn fire_resume(
            &self,
            session_id: &str,
            prompt: String,
            _tags: BTreeMap<String, String>,
            _timeout: Duration,
        ) -> Result<FireOutcome> {
            *self.last_resume.lock().unwrap() = Some((session_id.into(), prompt));
            Ok(self
                .on_resume
                .lock()
                .unwrap()
                .take()
                .expect("on_resume not set"))
        }
    }

    #[derive(Default)]
    struct MockReplier {
        sent: tokio::sync::Mutex<Vec<(i64, String)>>,
    }

    #[async_trait]
    impl Replier for MockReplier {
        async fn send(&self, chat_id: i64, text: &str) -> Result<()> {
            self.sent.lock().await.push((chat_id, text.into()));
            Ok(())
        }
    }

    async fn fresh_binder(dir: &TempDir) -> SessionBinder {
        SessionBinder::load(dir.path().join("b.json"))
            .await
            .unwrap()
    }

    fn cfg() -> OrchestratorConfig {
        OrchestratorConfig {
            preset: "claude".into(),
            project_id: Some("proj-id".into()),
            turn_timeout: Duration::from_secs(60),
        }
    }

    #[tokio::test]
    async fn unbound_chat_spawns_and_persists_session() {
        let dir = tempfile::tempdir().unwrap();
        let binder = fresh_binder(&dir).await;
        let fire = MockFire::new();
        *fire.on_spawn.lock().unwrap() = Some(FireOutcome::Done {
            session: "sess-new".into(),
            assistant_text: "hi".into(),
        });
        let replier = MockReplier::default();

        handle_message(&cfg(), &binder, &fire, &replier, 42, "hello".into())
            .await
            .unwrap();

        let last = fire.last_spawn.lock().unwrap().clone().unwrap();
        assert_eq!(last.0, "claude");
        assert_eq!(last.1.as_deref(), Some("proj-id"));
        assert_eq!(last.2, "hello");
        assert_eq!(binder.get(42).await.as_deref(), Some("sess-new"));
        let sent = replier.sent.lock().await.clone();
        assert_eq!(sent, vec![(42, "hi".to_string())]);
    }

    #[tokio::test]
    async fn bound_chat_resumes_existing_session() {
        let dir = tempfile::tempdir().unwrap();
        let binder = fresh_binder(&dir).await;
        binder.set(42, "sess-old".into()).await.unwrap();

        let fire = MockFire::new();
        *fire.on_resume.lock().unwrap() = Some(FireOutcome::Done {
            session: "sess-old".into(),
            assistant_text: "continued".into(),
        });
        let replier = MockReplier::default();

        handle_message(&cfg(), &binder, &fire, &replier, 42, "more".into())
            .await
            .unwrap();

        let last = fire.last_resume.lock().unwrap().clone().unwrap();
        assert_eq!(last.0, "sess-old");
        assert_eq!(last.1, "more");
        let sent = replier.sent.lock().await.clone();
        assert_eq!(sent, vec![(42, "continued".to_string())]);
    }

    #[tokio::test]
    async fn fire_error_is_reported_to_chat() {
        let dir = tempfile::tempdir().unwrap();
        let binder = fresh_binder(&dir).await;
        let fire = MockFire::new();
        *fire.on_spawn.lock().unwrap() = Some(FireOutcome::Error {
            session: None,
            code: ErrorCode::SpawnFailed,
            message: "boom".into(),
        });
        let replier = MockReplier::default();

        handle_message(&cfg(), &binder, &fire, &replier, 42, "hi".into())
            .await
            .unwrap();

        let sent = replier.sent.lock().await.clone();
        assert_eq!(sent.len(), 1);
        assert!(sent[0].1.contains("spawn_failed"));
        assert!(sent[0].1.contains("boom"));
        assert!(binder.get(42).await.is_none());
    }

    #[tokio::test]
    async fn empty_assistant_text_falls_back_to_placeholder() {
        let dir = tempfile::tempdir().unwrap();
        let binder = fresh_binder(&dir).await;
        let fire = MockFire::new();
        *fire.on_spawn.lock().unwrap() = Some(FireOutcome::Done {
            session: "s".into(),
            assistant_text: "".into(),
        });
        let replier = MockReplier::default();

        handle_message(&cfg(), &binder, &fire, &replier, 1, "hi".into())
            .await
            .unwrap();

        let sent = replier.sent.lock().await.clone();
        assert_eq!(sent, vec![(1, "(empty reply)".to_string())]);
    }
}
