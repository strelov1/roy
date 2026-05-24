//! webhook subscriber — render a body template against the fire context
//! and POST it to the configured URL.
//!
//! Template engine: minimal `{{key}}` substitution. No conditionals, no
//! loops, no helpers. Unknown placeholders render as empty string.
//! Authors are responsible for JSON-escaping or any encoding around the
//! placeholders.

use std::collections::HashMap;
use std::time::Duration;

use anyhow::{Context, Result};
use async_trait::async_trait;
use serde::Deserialize;

use crate::roy_client::FireSuccess;
use crate::types::Fire;

#[derive(Debug, Deserialize)]
pub struct Config {
    pub url: String,
    #[serde(default)]
    pub method: Option<String>,
    #[serde(default)]
    pub headers: HashMap<String, String>,
    #[serde(default)]
    pub body_template: Option<String>,
}

pub fn parse_config(json: &str) -> Result<Config> {
    serde_json::from_str(json).context("webhook config")
}

/// Spec §4.2 placeholder context. The Fire we get from the store provides
/// the agent-side metadata; the FireSuccess provides the result.
pub fn build_context(
    fire: &Fire,
    agent_name: &str,
    success: Option<&FireSuccess>,
    error_message: Option<&str>,
) -> HashMap<String, String> {
    let mut c = HashMap::new();
    c.insert("agent.id".into(), fire.agent_id.clone());
    c.insert("agent.name".into(), agent_name.into());
    c.insert(
        "trigger.id".into(),
        fire.trigger_id.clone().unwrap_or_default(),
    );
    c.insert("fire.id".into(), fire.id.clone());
    c.insert("fire.started_at".into(), fire.started_at.to_rfc3339());
    c.insert(
        "fire.finished_at".into(),
        fire.finished_at.map(|t| t.to_rfc3339()).unwrap_or_default(),
    );
    let duration_ms = fire
        .finished_at
        .map(|f| (f - fire.started_at).num_milliseconds())
        .unwrap_or(0);
    c.insert("fire.duration_ms".into(), duration_ms.to_string());
    c.insert("fire.status".into(), fire.status.clone());
    c.insert(
        "fire.cost_usd".into(),
        fire.cost_usd.map(|x| x.to_string()).unwrap_or_default(),
    );
    c.insert(
        "fire.stop_reason".into(),
        fire.stop_reason.clone().unwrap_or_default(),
    );
    c.insert(
        "session.id".into(),
        fire.session_id.clone().unwrap_or_default(),
    );
    c.insert(
        "result.assistant_text".into(),
        success
            .map(|s| s.assistant_text.clone())
            .unwrap_or_default(),
    );
    c.insert(
        "result.error_message".into(),
        error_message.unwrap_or("").into(),
    );
    c
}

/// Render `{{key}}` placeholders. Unknown keys render as empty string.
pub fn render(template: &str, ctx: &HashMap<String, String>) -> String {
    let mut out = String::with_capacity(template.len());
    let mut rest = template;
    while !rest.is_empty() {
        if let Some(start) = rest.find("{{") {
            // Copy bytes before the placeholder verbatim.
            out.push_str(&rest[..start]);
            let after_open = &rest[start + 2..];
            if let Some(end_rel) = after_open.find("}}") {
                let key = after_open[..end_rel].trim();
                out.push_str(ctx.get(key).map(String::as_str).unwrap_or_default());
                rest = &after_open[end_rel + 2..];
            } else {
                // Unclosed — copy the rest as-is.
                out.push_str("{{");
                out.push_str(after_open);
                break;
            }
        } else {
            out.push_str(rest);
            break;
        }
    }
    out
}

pub async fn execute_with_cfg(
    client: &reqwest::Client,
    cfg: &Config,
    ctx: &HashMap<String, String>,
) -> super::Outcome {
    let body = cfg
        .body_template
        .as_deref()
        .map(|t| render(t, ctx))
        .unwrap_or_default();

    let method = cfg.method.as_deref().unwrap_or("POST").to_uppercase();
    let mut req = match method.as_str() {
        "POST" => client.post(&cfg.url),
        "PUT" => client.put(&cfg.url),
        "PATCH" => client.patch(&cfg.url),
        _ => return super::Outcome::error(format!("unsupported method: {method}")),
    };
    for (k, v) in &cfg.headers {
        req = req.header(k, v);
    }
    req = req.body(body);

    match req.send().await {
        Ok(resp) => {
            let status_code = resp.status();
            let snippet = match resp.bytes().await {
                Ok(b) => {
                    let take = b.len().min(4096);
                    Some(String::from_utf8_lossy(&b[..take]).into_owned())
                }
                Err(_) => None,
            };
            if status_code.is_success() {
                super::Outcome {
                    status: super::RunStatus::Ok,
                    error_message: None,
                    response_snippet: snippet,
                }
            } else {
                super::Outcome {
                    status: super::RunStatus::Error,
                    error_message: Some(format!("HTTP {status_code}")),
                    response_snippet: snippet,
                }
            }
        }
        Err(e) => super::Outcome::error(format!("send: {e}")),
    }
}

pub fn build(config_json: &str) -> Result<Box<dyn super::Subscriber>> {
    let cfg = parse_config(config_json)?;
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(30))
        .build()
        .context("build reqwest client")?;
    Ok(Box::new(WebhookSubscriber { cfg, client }))
}

pub struct WebhookSubscriber {
    cfg: Config,
    client: reqwest::Client,
}

#[async_trait]
impl super::Subscriber for WebhookSubscriber {
    async fn run(&self, ctx: &super::FireCtx<'_>) -> super::Outcome {
        let render_ctx = build_context(ctx.fire, ctx.agent_name, ctx.success, ctx.error_message);
        execute_with_cfg(&self.client, &self.cfg, &render_ctx).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    fn ctx_with(text: &str) -> HashMap<String, String> {
        let mut c = HashMap::new();
        c.insert("result.assistant_text".into(), text.into());
        c.insert("agent.name".into(), "digest".into());
        c
    }

    #[test]
    fn render_substitutes_known_keys_and_empties_unknown() {
        let mut ctx = HashMap::new();
        ctx.insert("a".into(), "1".into());
        assert_eq!(render("x={{a}} y={{b}}", &ctx), "x=1 y=");
    }

    #[test]
    fn render_handles_no_placeholders() {
        assert_eq!(render("plain", &HashMap::new()), "plain");
    }

    #[test]
    fn render_handles_unclosed_braces() {
        // `{{a` with no closing — passes through verbatim.
        assert_eq!(render("hi {{a", &HashMap::new()), "hi {{a");
    }

    #[test]
    fn render_preserves_multibyte_content_around_placeholders() {
        let mut ctx = HashMap::new();
        ctx.insert("name".into(), "мир".into());
        assert_eq!(render("привет {{name}} 🌍", &ctx), "привет мир 🌍");
    }

    fn test_client() -> reqwest::Client {
        reqwest::Client::builder()
            .timeout(Duration::from_secs(30))
            .build()
            .unwrap()
    }

    #[tokio::test]
    async fn execute_posts_rendered_body_to_url() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/hook"))
            .respond_with(ResponseTemplate::new(200).set_body_string("ok"))
            .mount(&server)
            .await;

        let url = format!("{}/hook", server.uri());
        let cfg_json =
            format!(r#"{{"url":"{url}","body_template":"text={{{{result.assistant_text}}}}"}}"#);
        let cfg = parse_config(&cfg_json).unwrap();
        let out = execute_with_cfg(&test_client(), &cfg, &ctx_with("hello")).await;
        assert_eq!(out.status, super::super::RunStatus::Ok);
        assert_eq!(out.response_snippet.as_deref(), Some("ok"));

        let reqs = server.received_requests().await.unwrap();
        assert_eq!(reqs.len(), 1);
        assert_eq!(String::from_utf8_lossy(&reqs[0].body), "text=hello");
    }

    #[tokio::test]
    async fn execute_records_http_error_with_snippet() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/hook"))
            .respond_with(ResponseTemplate::new(500).set_body_string("boom"))
            .mount(&server)
            .await;

        let url = format!("{}/hook", server.uri());
        let cfg = parse_config(&format!(r#"{{"url":"{url}"}}"#)).unwrap();
        let out = execute_with_cfg(&test_client(), &cfg, &ctx_with("x")).await;
        assert_eq!(out.status, super::super::RunStatus::Error);
        assert!(out.error_message.unwrap().contains("500"));
        assert_eq!(out.response_snippet.as_deref(), Some("boom"));
    }
}
