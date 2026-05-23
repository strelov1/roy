//! `roy` CLI: a thin trigger over the `roy serve` daemon.
//!
//! Subcommands defined per `docs/superpowers/specs/2026-05-22-roy-cli-design.md`.
//! Each subcommand is a separate iteration; this file currently only wires up
//! the clap structure so the binary compiles and `roy --help` is correct.

use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(name = "roy", about = "Spawn and orchestrate coding-agent sessions")]
struct Cli {
    #[command(subcommand)]
    command: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Run the daemon that owns the SessionManager.
    Serve(ServeArgs),
    /// Spawn one session, send one prompt, stream events to stdout.
    Run(RunArgs),
    /// Attach to an existing session and stream its journal to stdout.
    Attach(AttachArgs),
    /// List live sessions known to the daemon.
    List,
    /// Ask the daemon to close a session.
    Close(CloseArgs),
}

#[derive(clap::Args)]
struct ServeArgs {
    #[arg(long)]
    socket: Option<String>,
    #[arg(long)]
    port: Option<u16>,
    #[arg(long)]
    journal_dir: Option<String>,
}

#[derive(clap::Args)]
struct RunArgs {
    agent: String,
    task: String,
    #[arg(long)]
    cwd: Option<String>,
    #[arg(long)]
    model: Option<String>,
    #[arg(long)]
    permission: Option<String>,
    #[arg(long)]
    detach: bool,
    #[arg(long)]
    resume: Option<String>,
    #[arg(long)]
    pretty: bool,
    #[arg(long)]
    with_seq: bool,
}

#[derive(clap::Args)]
struct AttachArgs {
    session: String,
    #[arg(long)]
    from_seq: Option<u64>,
    #[arg(long)]
    pretty: bool,
    #[arg(long)]
    with_seq: bool,
}

#[derive(clap::Args)]
struct CloseArgs {
    session: String,
}

fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();
    match cli.command {
        Cmd::Serve(_) | Cmd::Run(_) | Cmd::Attach(_) | Cmd::List | Cmd::Close(_) => {
            anyhow::bail!("not yet wired up — see iteration plan");
        }
    }
}
