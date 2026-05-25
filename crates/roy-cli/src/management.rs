//! Thin HTTP client to roy-management for project/tag-aware commands.

use anyhow::{anyhow, Context, Result};
use reqwest::Response;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

pub fn url() -> String {
    std::env::var("ROY_MANAGEMENT_URL").unwrap_or_else(|_| "http://127.0.0.1:8079".to_string())
}

async fn ensure_success(resp: Response) -> Result<Response> {
    if resp.status().is_success() {
        Ok(resp)
    } else {
        Err(anyhow!(
            "management {}: {}",
            resp.status(),
            resp.text().await?
        ))
    }
}

#[derive(Debug, Deserialize)]
pub struct Project {
    pub id: String,
    pub name: String,
    pub path: String,
}

#[derive(Debug, Serialize, Default)]
pub struct CreateSessionReq {
    pub agent: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub project_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub permission: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub system_prompt: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub agent_name: Option<String>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub tags: BTreeMap<String, String>,
}

#[derive(Debug, Deserialize)]
pub struct CreatedSession {
    pub session_id: String,
}

pub async fn list_projects() -> Result<Vec<Project>> {
    let resp = reqwest::Client::new()
        .get(format!("{}/projects", url()))
        .send()
        .await
        .context("GET /projects")?;
    Ok(ensure_success(resp).await?.json().await?)
}

pub async fn create_project(name: &str) -> Result<Project> {
    let resp = reqwest::Client::new()
        .post(format!("{}/projects", url()))
        .json(&serde_json::json!({ "name": name }))
        .send()
        .await?;
    Ok(ensure_success(resp).await?.json().await?)
}

pub async fn delete_project(id: &str) -> Result<()> {
    let resp = reqwest::Client::new()
        .delete(format!("{}/projects/{}", url(), id))
        .send()
        .await?;
    ensure_success(resp).await?;
    Ok(())
}

pub async fn create_session(req: CreateSessionReq) -> Result<CreatedSession> {
    let resp = reqwest::Client::new()
        .post(format!("{}/sessions", url()))
        .json(&req)
        .send()
        .await?;
    Ok(ensure_success(resp).await?.json().await?)
}

pub async fn put_tags(session_id: &str, tags: &BTreeMap<String, String>) -> Result<()> {
    let resp = reqwest::Client::new()
        .put(format!("{}/sessions/{}/tags", url(), session_id))
        .json(&serde_json::json!({ "tags": tags }))
        .send()
        .await?;
    ensure_success(resp).await?;
    Ok(())
}
