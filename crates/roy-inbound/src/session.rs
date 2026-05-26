//! Session strategy + resolver. Resolver impl lands in Task 6.
use serde::Deserialize;
use std::time::Duration;

// Manual Deserialize below — do NOT also derive(Deserialize) on this enum.
#[derive(Debug, Clone)]
pub enum SessionStrategyConfig {
    Ephemeral,
    PersistentOne,
    PerSenderSticky { idle_timeout_secs: u64 },
}

// Accepts both compact form (`session = "ephemeral"`) and tagged map form.
impl<'de> serde::Deserialize<'de> for SessionStrategyConfig {
    fn deserialize<D>(de: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        #[derive(Deserialize)]
        #[serde(untagged)]
        enum Helper {
            Short(String),
            Tagged {
                kind: String,
                idle_timeout_secs: Option<u64>,
            },
        }
        match Helper::deserialize(de)? {
            Helper::Short(s) => match s.as_str() {
                "ephemeral" => Ok(Self::Ephemeral),
                "persistent_one" => Ok(Self::PersistentOne),
                other => Err(serde::de::Error::custom(format!(
                    "unknown session strategy '{other}' (use tagged form for per_sender_sticky)"
                ))),
            },
            Helper::Tagged {
                kind,
                idle_timeout_secs,
            } => match kind.as_str() {
                "ephemeral" => Ok(Self::Ephemeral),
                "persistent_one" => Ok(Self::PersistentOne),
                "per_sender_sticky" => {
                    let secs = idle_timeout_secs.ok_or_else(|| {
                        serde::de::Error::custom("per_sender_sticky requires idle_timeout_secs")
                    })?;
                    Ok(Self::PerSenderSticky {
                        idle_timeout_secs: secs,
                    })
                }
                other => Err(serde::de::Error::custom(format!("unknown kind '{other}'"))),
            },
        }
    }
}

impl SessionStrategyConfig {
    pub fn idle_timeout(&self) -> Option<Duration> {
        match self {
            Self::PerSenderSticky { idle_timeout_secs } => {
                Some(Duration::from_secs(*idle_timeout_secs))
            }
            _ => None,
        }
    }
}

use anyhow::Result;
use roy::FireTarget;
use std::sync::Arc;

use crate::store::bindings::BindingStore;

#[derive(Debug, Clone)]
pub struct PendingBinding {
    pub source_id: String,
    pub sender_id: String,
    pub agent_id: String,
    pub strategy_db_label: &'static str,
}

#[derive(Debug, Clone, Copy)]
pub enum SessionStrategy {
    Ephemeral,
    PersistentOne,
    PerSenderSticky { idle_timeout: Duration },
}

impl From<&SessionStrategyConfig> for SessionStrategy {
    fn from(c: &SessionStrategyConfig) -> Self {
        match c {
            SessionStrategyConfig::Ephemeral => Self::Ephemeral,
            SessionStrategyConfig::PersistentOne => Self::PersistentOne,
            SessionStrategyConfig::PerSenderSticky { idle_timeout_secs } => Self::PerSenderSticky {
                idle_timeout: Duration::from_secs(*idle_timeout_secs),
            },
        }
    }
}

pub struct SessionResolver {
    bindings: Arc<BindingStore>,
    preset: String,
}

impl SessionResolver {
    pub fn new(bindings: Arc<BindingStore>, preset: String) -> Self {
        Self { bindings, preset }
    }

    pub async fn resolve(
        &self,
        source_id: &str,
        sender_id: &str,
        agent_id: &str,
        strategy: SessionStrategy,
    ) -> Result<(FireTarget, Option<PendingBinding>)> {
        let spawn_target = || FireTarget::Spawn {
            preset: self.preset.clone(),
            system_prompt: None,
        };

        let pending = |label: &'static str| PendingBinding {
            source_id: source_id.to_string(),
            sender_id: sender_id.to_string(),
            agent_id: agent_id.to_string(),
            strategy_db_label: label,
        };

        match strategy {
            SessionStrategy::Ephemeral => Ok((spawn_target(), None)),
            SessionStrategy::PersistentOne => {
                if let Some(b) = self.bindings.lookup(source_id, "*").await? {
                    Ok((
                        FireTarget::Resume {
                            session_id: b.session_id,
                        },
                        None,
                    ))
                } else {
                    Ok((
                        spawn_target(),
                        Some(PendingBinding {
                            sender_id: "*".into(),
                            ..pending("persistent_one")
                        }),
                    ))
                }
            }
            SessionStrategy::PerSenderSticky { idle_timeout } => {
                if let Some(b) = self.bindings.lookup(source_id, sender_id).await? {
                    let age = chrono::Utc::now() - b.last_active_at;
                    if age.to_std().map(|d| d > idle_timeout).unwrap_or(false) {
                        Ok((spawn_target(), Some(pending("per_sender_sticky"))))
                    } else {
                        Ok((
                            FireTarget::Resume {
                                session_id: b.session_id,
                            },
                            None,
                        ))
                    }
                } else {
                    Ok((spawn_target(), Some(pending("per_sender_sticky"))))
                }
            }
        }
    }
}

#[cfg(test)]
mod resolver_tests {
    use super::*;
    use crate::store::db;
    use tempfile::tempdir;

    async fn resolver() -> (tempfile::TempDir, SessionResolver) {
        let dir = tempdir().unwrap();
        let pool = db::open(&dir.path().join("s.db")).await.unwrap();
        let r = SessionResolver::new(Arc::new(BindingStore::new(pool)), "claude".into());
        (dir, r)
    }

    #[tokio::test]
    async fn ephemeral_always_spawn_no_binding() {
        let (_d, r) = resolver().await;
        let (t, pb) = r
            .resolve("src", "alice", "agent-1", SessionStrategy::Ephemeral)
            .await
            .unwrap();
        assert!(matches!(t, FireTarget::Spawn { .. }));
        assert!(pb.is_none());
    }

    #[tokio::test]
    async fn sticky_miss_returns_spawn_plus_pending() {
        let (_d, r) = resolver().await;
        let strat = SessionStrategy::PerSenderSticky {
            idle_timeout: Duration::from_secs(3600),
        };
        let (t, pb) = r.resolve("src", "alice", "agent-1", strat).await.unwrap();
        assert!(matches!(t, FireTarget::Spawn { .. }));
        let pb = pb.unwrap();
        assert_eq!(pb.source_id, "src");
        assert_eq!(pb.sender_id, "alice");
    }

    #[tokio::test]
    async fn sticky_hit_returns_resume_no_pending() {
        let (_d, r) = resolver().await;
        r.bindings
            .upsert("src", "alice", "agent-1", "per_sender_sticky", "sid-old")
            .await
            .unwrap();
        let strat = SessionStrategy::PerSenderSticky {
            idle_timeout: Duration::from_secs(3600),
        };
        let (t, pb) = r.resolve("src", "alice", "agent-1", strat).await.unwrap();
        assert!(matches!(t, FireTarget::Resume { ref session_id } if session_id == "sid-old"));
        assert!(pb.is_none());
    }

    #[tokio::test]
    async fn sticky_expired_returns_spawn_plus_pending() {
        let (_d, r) = resolver().await;
        r.bindings
            .upsert("src", "alice", "agent-1", "per_sender_sticky", "sid-old")
            .await
            .unwrap();
        sqlx::query(
            "UPDATE bindings SET last_active_at = ?1 WHERE source_id='src' AND sender_id='alice'",
        )
        .bind(chrono::Utc::now() - chrono::Duration::seconds(7200))
        .execute(r.bindings.pool_for_test())
        .await
        .unwrap();
        let strat = SessionStrategy::PerSenderSticky {
            idle_timeout: Duration::from_secs(3600),
        };
        let (t, pb) = r.resolve("src", "alice", "agent-1", strat).await.unwrap();
        assert!(matches!(t, FireTarget::Spawn { .. }));
        assert!(pb.is_some());
    }

    #[tokio::test]
    async fn persistent_one_uses_wildcard_sender() {
        let (_d, r) = resolver().await;
        r.bindings
            .upsert("src", "*", "agent-1", "persistent_one", "sid-pone")
            .await
            .unwrap();
        let (t, pb) = r
            .resolve("src", "anything", "agent-1", SessionStrategy::PersistentOne)
            .await
            .unwrap();
        assert!(matches!(t, FireTarget::Resume { ref session_id } if session_id == "sid-pone"));
        assert!(pb.is_none());
    }
}
