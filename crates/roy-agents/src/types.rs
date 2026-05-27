use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// A stored agent. `prompt` is the persona/system prompt (used by management's
/// interactive runs); `task` is the standing instruction for scheduled fires
/// (used by the scheduler). Either may be empty/None depending on how the
/// agent is used.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, sqlx::FromRow)]
pub struct Agent {
    pub id: String,
    pub name: String,
    pub slug: String,
    pub description: Option<String>,
    pub preset: String,
    pub model: Option<String>,
    pub prompt: String,
    pub task: Option<String>,
    pub persistent: bool,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// Fields accepted when creating an agent. `slug` is derived from `name` by the
/// store (with collision suffixing), not supplied by the caller.
#[derive(Debug, Clone, Deserialize)]
pub struct NewAgent {
    pub name: String,
    #[serde(default)]
    pub description: Option<String>,
    pub preset: String,
    #[serde(default)]
    pub model: Option<String>,
    #[serde(default)]
    pub prompt: String,
    #[serde(default)]
    pub task: Option<String>,
    #[serde(default)]
    pub persistent: bool,
}

/// Partial update. Every field is optional; an absent field leaves the column
/// unchanged. `name` change does NOT re-slug (the slug is stable once minted).
///
/// Nullable columns (`description`, `model`, `task`) use the double-Option
/// pattern to distinguish three states on the wire:
///
/// - field absent in JSON → outer `None` → leave alone
/// - field present as `null` → `Some(None)` → clear the column to NULL
/// - field present with value → `Some(Some(x))` → set the column to `x`
#[derive(Debug, Clone, Default, Deserialize)]
pub struct AgentUpdate {
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default, deserialize_with = "deserialize_optional_field")]
    pub description: Option<Option<String>>,
    #[serde(default)]
    pub preset: Option<String>,
    #[serde(default, deserialize_with = "deserialize_optional_field")]
    pub model: Option<Option<String>>,
    #[serde(default)]
    pub prompt: Option<String>,
    #[serde(default, deserialize_with = "deserialize_optional_field")]
    pub task: Option<Option<String>>,
    #[serde(default)]
    pub persistent: Option<bool>,
}

/// Forces serde to call the inner `Option::deserialize` even when the JSON
/// value is `null`, so we can distinguish "field absent" (handled by
/// `#[serde(default)]` returning the outer `None`) from "field set to null"
/// (this function returning `Some(None)`).
pub fn deserialize_optional_field<'de, T, D>(deserializer: D) -> Result<Option<Option<T>>, D::Error>
where
    T: serde::Deserialize<'de>,
    D: serde::Deserializer<'de>,
{
    Ok(Some(Option::deserialize(deserializer)?))
}
