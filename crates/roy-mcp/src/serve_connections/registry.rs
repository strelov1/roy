//! Registry of started upstreams. Owns tool aggregation and tool-call routing.
//!
//! Tools are namespaced as `<slug>__<tool_name>` to avoid collisions across
//! upstreams. The route map is built once at startup; `tools/call` looks up
//! the (upstream, original_name) pair without splitting the prefix string at
//! call time.
//!
//! Upstreams that fail to start are skipped with a warning — one bad config
//! does not abort the whole session, the agent just won't see those tools.

use anyhow::{anyhow, Result};
use serde_json::Value;
use std::collections::HashMap;
use std::sync::Arc;

use super::spec::Bundle;
use super::upstream::Upstream;

pub struct Registry {
    upstreams: HashMap<String, Arc<Upstream>>,
    /// `<slug>__<tool>` -> (upstream slug, original tool name). Avoids
    /// splitting the prefixed name on every call.
    routes: HashMap<String, (String, String)>,
}

impl Registry {
    pub async fn start(bundle: Bundle) -> Result<Self> {
        let mut upstreams: HashMap<String, Arc<Upstream>> = HashMap::new();
        let mut routes: HashMap<String, (String, String)> = HashMap::new();
        for spec in &bundle.connections {
            let up = match Upstream::start(spec).await {
                Ok(u) => Arc::new(u),
                Err(e) => {
                    tracing::warn!(
                        slug = %spec.slug,
                        error = %e,
                        "upstream failed to start"
                    );
                    continue;
                }
            };
            for tool in &up.tools {
                let name = tool.get("name").and_then(Value::as_str).unwrap_or("");
                if name.is_empty() {
                    continue;
                }
                let prefixed = format!("{}__{}", spec.slug, name);
                routes.insert(prefixed, (spec.slug.clone(), name.to_string()));
            }
            upstreams.insert(spec.slug.clone(), up);
        }
        Ok(Self { upstreams, routes })
    }

    /// Build the aggregated `tools/list` payload — each upstream tool with
    /// its name rewritten to `<slug>__<name>`. Descriptions, inputSchema,
    /// and any other fields pass through unchanged.
    pub fn tools_list(&self) -> Vec<Value> {
        let mut out = Vec::new();
        for (slug, up) in &self.upstreams {
            for tool in &up.tools {
                if let Some(obj) = tool.as_object() {
                    let mut prefixed = obj.clone();
                    let original = obj.get("name").and_then(Value::as_str).unwrap_or("");
                    prefixed.insert("name".into(), Value::String(format!("{slug}__{original}")));
                    out.push(Value::Object(prefixed));
                }
            }
        }
        out
    }

    /// Route a `tools/call` to the right upstream. Strips the `<slug>__`
    /// prefix and forwards `arguments` unchanged.
    pub async fn call_tool(&self, params: Value) -> Result<Value> {
        let name = params
            .get("name")
            .and_then(Value::as_str)
            .ok_or_else(|| anyhow!("missing tool name"))?;
        let arguments = params
            .get("arguments")
            .cloned()
            .unwrap_or(Value::Object(Default::default()));
        let (slug, original) = self
            .routes
            .get(name)
            .ok_or_else(|| anyhow!("unknown tool '{name}'"))?;
        let up = self
            .upstreams
            .get(slug)
            .ok_or_else(|| anyhow!("upstream '{slug}' is gone"))?;
        up.call_tool(original, arguments).await
    }

    pub async fn shutdown(self) {
        for (_, up) in self.upstreams {
            // Each Arc::try_unwrap should normally succeed (registry is the
            // only owner once dispatch loop exits), but if a slow tools/call
            // is still in flight, fall through and let kill_on_drop clean
            // up when the Arc drops.
            if let Ok(up) = Arc::try_unwrap(up) {
                up.shutdown().await;
            }
        }
    }
}
