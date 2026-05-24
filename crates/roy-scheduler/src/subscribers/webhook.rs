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
    let bytes = template.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if i + 1 < bytes.len() && bytes[i] == b'{' && bytes[i + 1] == b'{' {
            // find matching }}
            if let Some(end) = find_close(&bytes[i + 2..]) {
                let key = std::str::from_utf8(&bytes[i + 2..i + 2 + end])
                    .unwrap_or("")
                    .trim();
                out.push_str(ctx.get(key).map(String::as_str).unwrap_or(""));
                i += 2 + end + 2;
                continue;
            }
        }
        out.push(bytes[i] as char);
        i += 1;
    }
    out
}

fn find_close(haystack: &[u8]) -> Option<usize> {
    let mut j = 0;
    while j + 1 < haystack.len() {
        if haystack[j] == b'}' && haystack[j + 1] == b'}' {
            return Some(j);
        }
        j += 1;
    }
    None
}

pub struct ExecOutcome {
    pub status: &'static str,
    pub error_message: Option<String>,
    pub response_snippet: Option<String>,
}

pub async fn execute(config_json: &str, ctx: &HashMap<String, String>) -> ExecOutcome {
    let cfg = match parse_config(config_json) {
        Ok(c) => c,
        Err(e) => {
            return ExecOutcome {
                status: "error",
                error_message: Some(format!("config: {e}")),
                response_snippet: None,
            };
        }
    };

    let body = cfg
        .body_template
        .as_deref()
        .map(|t| render(t, ctx))
        .unwrap_or_default();

    let client = match reqwest::Client::builder()
        .timeout(Duration::from_secs(30))
        .build()
    {
        Ok(c) => c,
        Err(e) => {
            return ExecOutcome {
                status: "error",
                error_message: Some(format!("http client: {e}")),
                response_snippet: None,
            };
        }
    };

    let method = cfg.method.as_deref().unwrap_or("POST").to_uppercase();
    let mut req = match method.as_str() {
        "POST" => client.post(&cfg.url),
        "PUT" => client.put(&cfg.url),
        "PATCH" => client.patch(&cfg.url),
        _ => {
            return ExecOutcome {
                status: "error",
                error_message: Some(format!("unsupported method: {method}")),
                response_snippet: None,
            };
        }
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
                ExecOutcome {
                    status: "ok",
                    error_message: None,
                    response_snippet: snippet,
                }
            } else {
                ExecOutcome {
                    status: "error",
                    error_message: Some(format!("HTTP {status_code}")),
                    response_snippet: snippet,
                }
            }
        }
        Err(e) => ExecOutcome {
            status: "error",
            error_message: Some(format!("send: {e}")),
            response_snippet: None,
        },
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

    #[tokio::test]
    async fn execute_posts_rendered_body_to_url() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/hook"))
            .respond_with(ResponseTemplate::new(200).set_body_string("ok"))
            .mount(&server)
            .await;

        let url = format!("{}/hook", server.uri());
        let cfg =
            format!(r#"{{"url":"{url}","body_template":"text={{{{result.assistant_text}}}}"}}"#);
        let out = execute(&cfg, &ctx_with("hello")).await;
        assert_eq!(out.status, "ok");
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
        let cfg = format!(r#"{{"url":"{url}"}}"#);
        let out = execute(&cfg, &ctx_with("x")).await;
        assert_eq!(out.status, "error");
        assert!(out.error_message.unwrap().contains("500"));
        assert_eq!(out.response_snippet.as_deref(), Some("boom"));
    }
}
