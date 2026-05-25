//! HTTP client for roy-management's REST API. Used by the new `roy agents`
//! subcommands. Only the surface roy-cli needs.

use anyhow::{anyhow, Context, Result};
use serde::{Deserialize, Serialize, Serializer};

#[derive(Debug, Clone, Deserialize, Serialize)]
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
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Debug, Default, Serialize)]
pub struct NewAgent {
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    pub preset: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    pub prompt: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub task: Option<String>,
    #[serde(default)]
    pub persistent: bool,
}

/// PATCH body for `PUT /agents/{id}`. Mirrors the server's tri-state semantics
/// for nullable fields: `description` / `model` / `task` use `Option<Option<…>>`
/// so we can express "leave alone" (outer `None`, skipped), "clear" (`Some(None)`,
/// serialized as JSON `null`), and "set" (`Some(Some(value))`).
#[derive(Debug, Default, Serialize)]
pub struct AgentPatch {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(
        skip_serializing_if = "Option::is_none",
        serialize_with = "serialize_nullable_field"
    )]
    pub description: Option<Option<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub preset: Option<String>,
    #[serde(
        skip_serializing_if = "Option::is_none",
        serialize_with = "serialize_nullable_field"
    )]
    pub model: Option<Option<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub prompt: Option<String>,
    #[serde(
        skip_serializing_if = "Option::is_none",
        serialize_with = "serialize_nullable_field"
    )]
    pub task: Option<Option<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub persistent: Option<bool>,
}

/// Emit JSON `null` for `Some(None)` (so the server's `Option<Option<…>>`
/// deserializer treats the field as "clear"), and the inner value for
/// `Some(Some(x))`. The outer `None` case is filtered out by
/// `skip_serializing_if` upstream and never reaches this function.
fn serialize_nullable_field<S: Serializer, T: Serialize>(
    v: &Option<Option<T>>,
    s: S,
) -> Result<S::Ok, S::Error> {
    match v {
        Some(Some(x)) => x.serialize(s),
        Some(None) => s.serialize_none(),
        None => unreachable!("skip_serializing_if filters this case"),
    }
}

#[derive(Debug, Deserialize, Serialize)]
pub struct RunResponse {
    pub session: String,
    pub agent_id: String,
}

pub struct ManagementClient {
    base: String,
    http: reqwest::Client,
}

impl ManagementClient {
    pub fn new(base_url: &str) -> Self {
        Self {
            base: base_url.trim_end_matches('/').to_string(),
            http: reqwest::Client::new(),
        }
    }

    async fn check<T: for<'de> Deserialize<'de>>(
        &self,
        res: reqwest::Response,
        expect: Option<reqwest::StatusCode>,
    ) -> Result<T> {
        let actual = res.status();
        let ok = match expect {
            Some(c) => actual == c,
            None => actual.is_success(),
        };
        if !ok {
            let body = res.text().await.unwrap_or_default();
            return Err(anyhow!("HTTP {actual}: {body}"));
        }
        let bytes = res.bytes().await.context("read body")?;
        serde_json::from_slice(&bytes).context("parse JSON")
    }

    pub async fn list(&self) -> Result<Vec<Agent>> {
        let r = self.http.get(format!("{}/agents", self.base)).send().await?;
        self.check(r, None).await
    }

    pub async fn get(&self, id: &str) -> Result<Agent> {
        let r = self
            .http
            .get(format!("{}/agents/{}", self.base, urlencoding::encode(id)))
            .send()
            .await?;
        self.check(r, None).await
    }

    pub async fn create(&self, body: &NewAgent) -> Result<Agent> {
        let r = self
            .http
            .post(format!("{}/agents", self.base))
            .json(body)
            .send()
            .await?;
        self.check(r, Some(reqwest::StatusCode::CREATED)).await
    }

    pub async fn update(&self, id: &str, patch: &AgentPatch) -> Result<Agent> {
        let r = self
            .http
            .put(format!("{}/agents/{}", self.base, urlencoding::encode(id)))
            .json(patch)
            .send()
            .await?;
        self.check(r, None).await
    }

    pub async fn delete(&self, id: &str) -> Result<()> {
        let r = self
            .http
            .delete(format!("{}/agents/{}", self.base, urlencoding::encode(id)))
            .send()
            .await?;
        let status = r.status();
        if status != reqwest::StatusCode::NO_CONTENT {
            let body = r.text().await.unwrap_or_default();
            return Err(anyhow!("HTTP {}: {body}", status));
        }
        Ok(())
    }

    pub async fn run(&self, id: &str) -> Result<RunResponse> {
        let r = self
            .http
            .post(format!(
                "{}/agents/{}/run",
                self.base,
                urlencoding::encode(id)
            ))
            .send()
            .await?;
        self.check(r, None).await
    }

    /// Client-side slug→id resolution. Since the server doesn't expose
    /// `/agents/by-slug/<slug>`, we list and filter. Cheap for the
    /// management's expected scale.
    pub async fn resolve(&self, id_or_slug: &str) -> Result<String> {
        // Heuristic: if it looks like a UUID (36 chars with hyphens), use as-is.
        // Otherwise list+filter by slug or exact-id match.
        if id_or_slug.len() == 36 && id_or_slug.matches('-').count() == 4 {
            return Ok(id_or_slug.to_string());
        }
        let all = self.list().await?;
        all.into_iter()
            .find(|a| a.slug == id_or_slug || a.id == id_or_slug)
            .map(|a| a.id)
            .ok_or_else(|| anyhow!("agent not found: {id_or_slug}"))
    }
}
