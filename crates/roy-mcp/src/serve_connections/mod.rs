//! `roy mcp serve-connections`: a proxying MCP server.
//!
//! Speaks JSON-RPC 2.0 over its own stdio (acts as the MCP server for its
//! parent — the ACP agent), and spawns each upstream MCP as a child process.
//! `tools/list` aggregates all upstream tools with a `<slug>__<tool>` prefix;
//! `tools/call` strips the prefix and routes to the owning upstream.
//!
//! This file is the skeleton — the JSON-RPC dispatcher works, but the
//! upstream-spawning Registry is a stub returning an empty tool list. C3 and
//! C4 fill in the real upstream wrapper and aggregation.

use anyhow::{Context, Result};
use clap::Args;
use std::path::PathBuf;

pub mod dispatch;
pub mod registry;
pub mod spec;
pub mod upstream;

#[derive(Args, Debug)]
pub struct ServeConnectionsArgs {
    /// Path to a JSON file containing a `Bundle` (session_id + connections).
    /// Mutually exclusive with `--specs-stdin`.
    #[arg(long, conflicts_with = "specs_stdin")]
    pub specs: Option<PathBuf>,

    /// Read the spec bundle as the first line on stdin before switching to
    /// JSON-RPC framing for the rest of the conversation. Use when the spec
    /// contains secrets you don't want on disk.
    #[arg(long)]
    pub specs_stdin: bool,
}

pub async fn run(args: ServeConnectionsArgs) -> Result<()> {
    let bundle = load_bundle(&args).await.context("loading spec bundle")?;
    let registry = registry::Registry::start(bundle).await?;
    dispatch::run(registry).await
}

async fn load_bundle(args: &ServeConnectionsArgs) -> Result<spec::Bundle> {
    if let Some(path) = &args.specs {
        let text = tokio::fs::read_to_string(path)
            .await
            .with_context(|| format!("reading {}", path.display()))?;
        Ok(serde_json::from_str(&text)?)
    } else if args.specs_stdin {
        use tokio::io::AsyncBufReadExt;
        let mut lines = tokio::io::BufReader::new(tokio::io::stdin()).lines();
        let first = lines
            .next_line()
            .await?
            .ok_or_else(|| anyhow::anyhow!("EOF before spec bundle"))?;
        Ok(serde_json::from_str(&first)?)
    } else {
        anyhow::bail!("either --specs <path> or --specs-stdin is required")
    }
}
