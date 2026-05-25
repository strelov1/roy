//! Session strategy + resolver. Resolver impl lands in Task 6.
use std::time::Duration;
use serde::Deserialize;

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
            Tagged { kind: String, idle_timeout_secs: Option<u64> },
        }
        match Helper::deserialize(de)? {
            Helper::Short(s) => match s.as_str() {
                "ephemeral" => Ok(Self::Ephemeral),
                "persistent_one" => Ok(Self::PersistentOne),
                other => Err(serde::de::Error::custom(format!(
                    "unknown session strategy '{other}' (use tagged form for per_sender_sticky)"
                ))),
            },
            Helper::Tagged { kind, idle_timeout_secs } => match kind.as_str() {
                "ephemeral" => Ok(Self::Ephemeral),
                "persistent_one" => Ok(Self::PersistentOne),
                "per_sender_sticky" => {
                    let secs = idle_timeout_secs.ok_or_else(|| {
                        serde::de::Error::custom("per_sender_sticky requires idle_timeout_secs")
                    })?;
                    Ok(Self::PerSenderSticky { idle_timeout_secs: secs })
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
