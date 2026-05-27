//! `roy mcp serve-connections`: a proxying MCP server.
//!
//! Speaks JSON-RPC 2.0 over its own stdio (acts as the MCP server for its
//! parent — the ACP agent), and spawns each upstream MCP as a child process.
//! `tools/list` aggregates all upstream tools with a `<slug>__<tool>` prefix;
//! `tools/call` strips the prefix and routes to the owning upstream.

use anyhow::{Context, Result};
use clap::Args;
use std::path::PathBuf;
use tokio::io::{BufReader, Stdin};

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
    // We must own a single BufReader over stdin from start to finish — if we
    // created one to read the bundle and another for dispatch, the first one
    // would silently eat any JSON-RPC lines that arrived in the same read
    // and they'd be dropped on the floor when its buffer was discarded.
    let mut stdin = BufReader::new(tokio::io::stdin());
    let bundle = load_bundle(&args, &mut stdin)
        .await
        .context("loading spec bundle")?;
    tracing::info!(
        session = %bundle.session_id,
        connections = bundle.connections.len(),
        "serve-connections starting"
    );
    let registry = registry::Registry::start(bundle).await?;
    dispatch::run(registry, stdin).await
}

async fn load_bundle(
    args: &ServeConnectionsArgs,
    stdin: &mut BufReader<Stdin>,
) -> Result<spec::Bundle> {
    if let Some(path) = &args.specs {
        let text = tokio::fs::read_to_string(path)
            .await
            .with_context(|| format!("reading {}", path.display()))?;
        Ok(serde_json::from_str(&text)?)
    } else if args.specs_stdin {
        use tokio::io::AsyncBufReadExt;
        let mut first = String::new();
        let n = stdin.read_line(&mut first).await?;
        if n == 0 {
            anyhow::bail!("EOF before spec bundle");
        }
        Ok(serde_json::from_str(first.trim())?)
    } else {
        anyhow::bail!("either --specs <path> or --specs-stdin is required")
    }
}
