//! `roy-scheduler` CLI — clap-derive entry per spec §5.2.
//!
//! Output convention: one JSON line on stdout per successful command,
//! tracing on stderr (`RUST_LOG` overrides). Exit codes follow the same
//! shape as the roy CLI:
//!
//! | Code | Meaning                                                    |
//! |------|------------------------------------------------------------|
//! | 0    | Success.                                                   |
//! | 1    | Agent-side error (e.g. `fire-now` produced a `Result` with |
//! |      | `stop_reason.is_error()` or the `Fire` outcome was Error). |
//! | 2    | Transport / CLI / DB error (no daemon, bad flag, etc.).    |

mod pid_lock;

use std::path::PathBuf;
use std::process::ExitCode;

use clap::{ArgGroup, Parser, Subcommand};

#[derive(Parser)]
#[command(
    name = "roy-scheduler",
    about = "Cron + one-shot fire dispatcher for roy"
)]
struct Cli {
    #[command(subcommand)]
    command: Top,
}

#[derive(Subcommand)]
enum Top {
    /// Run the driver loop (poll triggers + dispatch fires + subscribers).
    Serve(ServeArgs),
    /// Apply the bundled SQLite migrations and exit.
    Migrate,
    /// Manage agents (add / list / show / rm).
    Agents {
        #[command(subcommand)]
        cmd: AgentsCmd,
    },
    /// Manage triggers (add cron|oneshot / list / rm / pause / resume).
    Triggers {
        #[command(subcommand)]
        cmd: TriggersCmd,
    },
    /// Manage subscribers (add / list / rm).
    Subscribers {
        #[command(subcommand)]
        cmd: SubscribersCmd,
    },
    /// Inspect fires (list / show).
    Fires {
        #[command(subcommand)]
        cmd: FiresCmd,
    },
    /// Ad-hoc fire — bypasses scheduling and fires the named agent NOW.
    FireNow(FireNowArgs),
}

#[derive(clap::Args)]
struct ServeArgs {
    /// SQLite DB path. Overrides `ROY_SCHEDULER_DB`.
    #[arg(long)]
    db: Option<PathBuf>,
    /// roy daemon socket. Overrides `ROY_SOCKET`.
    #[arg(long)]
    socket: Option<PathBuf>,
    /// Polling cadence in milliseconds.
    #[arg(long)]
    poll_ms: Option<u64>,
    /// Max triggers claimed per tick.
    #[arg(long)]
    batch_limit: Option<i64>,
    /// Max concurrent in-flight fires.
    #[arg(long)]
    max_fires: Option<usize>,
    /// Per-fire timeout (seconds).
    #[arg(long)]
    fire_timeout: Option<u64>,
    /// PidLock path. Defaults to `~/.local/state/roy-scheduler/serve.pid`.
    #[arg(long)]
    pid_file: Option<PathBuf>,
}

#[derive(Subcommand)]
enum AgentsCmd {
    /// Register a new agent.
    Add(AgentAddArgs),
    /// List all agents.
    List,
    /// Show one agent by id.
    Show { id: String },
    /// Delete one agent by id (FK cascade drops its triggers, fires, subscribers).
    Rm { id: String },
}

#[derive(clap::Args)]
struct AgentAddArgs {
    #[arg(long)]
    name: String,
    /// claude | gemini | opencode | codex
    #[arg(long)]
    preset: String,
    /// Prompt sent to the agent on every fire.
    #[arg(long)]
    task: String,
    /// Project id to fire under. Omit to fire as orphan.
    #[arg(long)]
    project: Option<String>,
    /// Optional model override.
    #[arg(long)]
    model: Option<String>,
    /// Persistent agent — every fire resumes the same session id.
    #[arg(long)]
    persistent: bool,
}

#[derive(Subcommand)]
enum TriggersCmd {
    /// Add a new trigger. Exactly one of `--cron` or `--oneshot` is required.
    Add(TriggerAddArgs),
    /// List triggers (optionally filtered by agent).
    List {
        #[arg(long)]
        agent: Option<String>,
    },
    /// Delete a trigger by id.
    Rm { id: String },
    /// Pause a trigger (driver skips it until resumed).
    Pause { id: String },
    /// Resume a paused trigger.
    Resume { id: String },
}

#[derive(clap::Args)]
#[command(group(ArgGroup::new("when").args(["cron", "oneshot"]).required(true)))]
struct TriggerAddArgs {
    /// Agent id this trigger fires.
    #[arg(long)]
    agent: String,
    /// 5-field cron expression (e.g. `0 9 * * *`). Validated at parse time.
    #[arg(long)]
    cron: Option<String>,
    /// One-shot RFC-3339 instant (e.g. `2026-05-25T10:00:00+03:00`).
    #[arg(long)]
    oneshot: Option<String>,
    /// IANA timezone for cron (e.g. `Europe/Moscow`). Defaults to `UTC`.
    #[arg(long, default_value = "UTC")]
    tz: String,
}

#[derive(Subcommand)]
enum SubscribersCmd {
    /// Register a new subscriber. Exactly one of `--trigger` or `--agent` is required.
    Add(SubscriberAddArgs),
    /// List subscribers, optionally filtered by agent or trigger.
    List {
        #[arg(long)]
        agent: Option<String>,
        #[arg(long)]
        trigger: Option<String>,
    },
    /// Delete a subscriber by id.
    Rm { id: String },
}

#[derive(clap::Args)]
#[command(group(ArgGroup::new("scope").args(["trigger", "agent"]).required(true)))]
struct SubscriberAddArgs {
    /// Trigger id to attach to (XOR with `--agent`).
    #[arg(long)]
    trigger: Option<String>,
    /// Agent id to attach to (XOR with `--trigger`).
    #[arg(long)]
    agent: Option<String>,
    /// inject_parent | webhook | notify_native | chain_agent
    #[arg(long)]
    kind: String,
    /// JSON config blob (per-kind shape). Stored verbatim.
    #[arg(long)]
    config: String,
    /// Optional order index within the fire's subscriber list (lower runs first).
    #[arg(long, default_value_t = 0)]
    order: i64,
}

#[derive(Subcommand)]
enum FiresCmd {
    /// List fires for an agent (newest first).
    List {
        #[arg(long)]
        agent: String,
        #[arg(long, default_value_t = 20)]
        limit: i64,
    },
    /// Show one fire row by id.
    Show { id: String },
}

#[derive(clap::Args)]
struct FireNowArgs {
    /// Agent id to fire ad-hoc.
    agent_id: String,
    /// Per-fire timeout (seconds). Defaults to 600.
    #[arg(long)]
    fire_timeout: Option<u64>,
}

fn main() -> ExitCode {
    init_tracing();
    let cli = Cli::parse();
    let rt = match tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
    {
        Ok(rt) => rt,
        Err(e) => {
            eprintln!("roy-scheduler: failed to start tokio runtime: {e}");
            return ExitCode::from(2);
        }
    };
    rt.block_on(async {
        match dispatch(cli).await {
            Ok(code) => code,
            Err(e) => {
                eprintln!("roy-scheduler: {e:#}");
                ExitCode::from(2)
            }
        }
    })
}

async fn dispatch(cli: Cli) -> anyhow::Result<ExitCode> {
    match cli.command {
        Top::Serve(args) => cmd_serve(args).await.map(|()| ExitCode::SUCCESS),
        Top::Migrate => cmd_migrate().await.map(|()| ExitCode::SUCCESS),
        Top::Agents { cmd } => cmd_agents(cmd).await.map(|()| ExitCode::SUCCESS),
        Top::Triggers { cmd } => cmd_triggers(cmd).await.map(|()| ExitCode::SUCCESS),
        Top::Subscribers { cmd } => cmd_subscribers(cmd).await.map(|()| ExitCode::SUCCESS),
        Top::Fires { cmd } => cmd_fires(cmd).await.map(|()| ExitCode::SUCCESS),
        Top::FireNow(args) => cmd_fire_now(args).await,
    }
}

fn init_tracing() {
    use tracing_subscriber::EnvFilter;
    let filter = EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| EnvFilter::new("roy_scheduler=info,warn"));
    let _ = tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_writer(std::io::stderr)
        .with_target(true)
        .try_init();
}

async fn cmd_serve(_args: ServeArgs) -> anyhow::Result<()> {
    Ok(())
}

async fn cmd_migrate() -> anyhow::Result<()> {
    Ok(())
}

async fn cmd_agents(_cmd: AgentsCmd) -> anyhow::Result<()> {
    Ok(())
}

async fn cmd_triggers(_cmd: TriggersCmd) -> anyhow::Result<()> {
    Ok(())
}

async fn cmd_subscribers(_cmd: SubscribersCmd) -> anyhow::Result<()> {
    Ok(())
}

async fn cmd_fires(_cmd: FiresCmd) -> anyhow::Result<()> {
    Ok(())
}

async fn cmd_fire_now(_args: FireNowArgs) -> anyhow::Result<ExitCode> {
    Ok(ExitCode::SUCCESS)
}
