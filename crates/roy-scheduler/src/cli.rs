//! `roy-scheduler` CLI — clap-derive entry per spec §5.2.
//!
//! The types and `run` entry point are public so they can be embedded by
//! `roy-cli` as the `roy scheduler` subcommand. The standalone
//! `roy-scheduler` binary and `roy-cli` share this exact code path.
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

use std::path::PathBuf;
use std::process::ExitCode;

use anyhow::Context;
use clap::{ArgGroup, Parser, Subcommand};
use sqlx::SqlitePool;

use crate::{db, store};

#[derive(Parser)]
#[command(
    name = "roy-scheduler",
    about = "Cron + one-shot fire dispatcher for roy"
)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Top,
}

#[derive(Subcommand)]
pub enum Top {
    /// Run the driver loop (poll triggers + dispatch fires + subscribers).
    Serve(ServeArgs),
    /// Health-probe the scheduler. Reads `<pid_file>`, checks the process is
    /// alive, prints one JSON line and exits 0 if up, 2 otherwise. Use this
    /// in scripts instead of `pgrep`-ing for the binary.
    Status(StatusArgs),
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
pub struct StatusArgs {
    /// PidLock path to probe. Defaults to `~/.local/state/roy-scheduler/serve.pid`.
    #[arg(long)]
    pub pid_file: Option<PathBuf>,
}

#[derive(clap::Args)]
pub struct ServeArgs {
    /// SQLite DB path. Overrides `ROY_SCHEDULER_DB`.
    #[arg(long)]
    pub db: Option<PathBuf>,
    /// roy daemon socket. Overrides `ROY_SOCKET`.
    #[arg(long)]
    pub socket: Option<PathBuf>,
    /// Polling cadence in milliseconds.
    #[arg(long)]
    pub poll_ms: Option<u64>,
    /// Max triggers claimed per tick.
    #[arg(long)]
    pub batch_limit: Option<i64>,
    /// Max concurrent in-flight fires.
    #[arg(long)]
    pub max_fires: Option<usize>,
    /// Per-fire timeout (seconds).
    #[arg(long)]
    pub fire_timeout: Option<u64>,
    /// PidLock path. Defaults to `~/.local/state/roy-scheduler/serve.pid`.
    #[arg(long)]
    pub pid_file: Option<PathBuf>,
}

#[derive(Subcommand)]
pub enum AgentsCmd {
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
pub struct AgentAddArgs {
    #[arg(long)]
    pub name: String,
    /// claude | gemini | opencode | codex
    #[arg(long)]
    pub preset: String,
    /// Prompt sent to the agent on every fire.
    #[arg(long)]
    pub task: String,
    /// Project id to fire under. Omit to fire as orphan.
    #[arg(long)]
    pub project: Option<String>,
    /// Optional model override.
    #[arg(long)]
    pub model: Option<String>,
    /// Persistent agent — every fire resumes the same session id.
    #[arg(long)]
    pub persistent: bool,
    /// Roy session id to notify. When set, the agent's fired prompt gets a
    /// `roy inject <id> ...` instruction so it can self-report findings.
    #[arg(long)]
    pub notify_session: Option<String>,
}

#[derive(Subcommand)]
pub enum TriggersCmd {
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
pub struct TriggerAddArgs {
    /// Agent id this trigger fires.
    #[arg(long)]
    pub agent: String,
    /// 5-field cron expression (e.g. `0 9 * * *`). Validated at parse time.
    #[arg(long)]
    pub cron: Option<String>,
    /// One-shot RFC-3339 instant (e.g. `2026-05-25T10:00:00+03:00`).
    #[arg(long)]
    pub oneshot: Option<String>,
    /// IANA timezone for cron (e.g. `Europe/Moscow`). Defaults to `UTC`.
    #[arg(long, default_value = "UTC")]
    pub tz: String,
}

#[derive(Subcommand)]
pub enum SubscribersCmd {
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
pub struct SubscriberAddArgs {
    /// Trigger id to attach to (XOR with `--agent`).
    #[arg(long)]
    pub trigger: Option<String>,
    /// Agent id to attach to (XOR with `--trigger`).
    #[arg(long)]
    pub agent: Option<String>,
    /// webhook | notify_native
    #[arg(long)]
    pub kind: String,
    /// JSON config blob (per-kind shape). Stored verbatim.
    #[arg(long)]
    pub config: String,
    /// Optional order index within the fire's subscriber list (lower runs first).
    #[arg(long, default_value_t = 0)]
    pub order: i64,
}

#[derive(Subcommand)]
pub enum FiresCmd {
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
pub struct FireNowArgs {
    /// Agent id to fire ad-hoc.
    pub agent_id: String,
    /// Per-fire timeout (seconds). Defaults to 600.
    #[arg(long)]
    pub fire_timeout: Option<u64>,
    /// Session id of the caller. Recorded on the fire's session as the
    /// reserved tag `roy-scheduler:initiated_by_session` so the UI can link
    /// the fire back to its initiator.
    #[arg(long, value_name = "SESSION_ID")]
    pub parent: Option<String>,
}

/// Dispatch the parsed CLI to the matching command. The caller owns the
/// tokio runtime and tracing subscriber.
pub async fn run(cli: Cli) -> anyhow::Result<ExitCode> {
    match cli.command {
        Top::Serve(args) => cmd_serve(args).await.map(|()| ExitCode::SUCCESS),
        Top::Status(args) => Ok(cmd_status(args)),
        Top::Migrate => cmd_migrate().await.map(|()| ExitCode::SUCCESS),
        Top::Agents { cmd } => cmd_agents(cmd).await.map(|()| ExitCode::SUCCESS),
        Top::Triggers { cmd } => cmd_triggers(cmd).await.map(|()| ExitCode::SUCCESS),
        Top::Subscribers { cmd } => cmd_subscribers(cmd).await.map(|()| ExitCode::SUCCESS),
        Top::Fires { cmd } => cmd_fires(cmd).await.map(|()| ExitCode::SUCCESS),
        Top::FireNow(args) => cmd_fire_now(args).await,
    }
}

fn default_db_path() -> PathBuf {
    if let Ok(s) = std::env::var("ROY_SCHEDULER_DB") {
        return PathBuf::from(s);
    }
    let home = std::env::var_os("HOME").unwrap_or_default();
    PathBuf::from(home).join(".local/state/roy-scheduler/state.db")
}

fn default_socket() -> PathBuf {
    if let Ok(s) = std::env::var("ROY_SOCKET") {
        return PathBuf::from(s);
    }
    let home = std::env::var_os("HOME").unwrap_or_default();
    PathBuf::from(home).join(".roy/daemon.sock")
}

fn default_pid_file() -> PathBuf {
    let home = std::env::var_os("HOME").unwrap_or_default();
    PathBuf::from(home).join(".local/state/roy-scheduler/serve.pid")
}

/// Open the SQLite pool at the configured path. `db::open` auto-runs migrations.
async fn open_pool() -> anyhow::Result<SqlitePool> {
    let path = default_db_path();
    db::open(&path)
        .await
        .with_context(|| format!("opening DB at {}", path.display()))
}

/// Print one JSON line on stdout — the project-wide output convention.
fn print_json(v: impl serde::Serialize) -> anyhow::Result<()> {
    let s = serde_json::to_string(&v).context("serializing JSON output")?;
    println!("{s}");
    Ok(())
}

async fn cmd_serve(args: ServeArgs) -> anyhow::Result<()> {
    use std::time::Duration;

    use crate::driver::{self, ServeOpts};

    let pid_path = args.pid_file.unwrap_or_else(default_pid_file);
    // The lock is held for the lifetime of this function — when the loop
    // exits (currently only on a panic propagated out of driver::serve)
    // Drop releases the pid file. A SIGINT here lets tokio cancel the
    // future, dropping the lock the same way.
    let _lock = roy::PidLock::acquire(&pid_path)
        .with_context(|| format!("acquiring pid lock at {}", pid_path.display()))?;

    let mut opts = ServeOpts {
        db_path: args.db.unwrap_or_else(default_db_path),
        socket_path: args.socket.unwrap_or_else(default_socket),
        ..ServeOpts::default()
    };
    if let Some(ms) = args.poll_ms {
        opts.poll_interval = Duration::from_millis(ms);
    }
    if let Some(n) = args.batch_limit {
        opts.batch_limit = n;
    }
    if let Some(n) = args.max_fires {
        opts.max_fires = n;
    }
    if let Some(secs) = args.fire_timeout {
        opts.fire_timeout = Duration::from_secs(secs);
    }

    tracing::info!(
        db = %opts.db_path.display(),
        socket = %opts.socket_path.display(),
        pid_file = %pid_path.display(),
        poll_ms = opts.poll_interval.as_millis() as u64,
        max_fires = opts.max_fires,
        "roy-scheduler serving",
    );

    driver::serve(opts).await
}

/// Print a one-line JSON health report and return the matching exit code.
/// Unlike `roy status`, the scheduler is purely a worker — there is no
/// socket to connect to — so we lean on the PidLock file: present + pid
/// alive → `up`, anything else → `down`.
fn cmd_status(args: StatusArgs) -> ExitCode {
    let pid_path = args.pid_file.unwrap_or_else(default_pid_file);
    let db_path = default_db_path();
    let pid = roy::pid_lock::peek_pid(&pid_path);
    let alive = pid.map(roy::pid_lock::pid_alive).unwrap_or(false);
    let payload = serde_json::json!({
        "status": if alive { "up" } else { "down" },
        "pid_file": pid_path.display().to_string(),
        "pid": pid,
        "db": db_path.display().to_string(),
    });
    println!("{payload}");
    if alive {
        ExitCode::SUCCESS
    } else {
        ExitCode::from(2)
    }
}

async fn cmd_migrate() -> anyhow::Result<()> {
    // `db::open` runs the embedded migrator unconditionally — opening is
    // sufficient. Print a one-line confirmation for scripting.
    let _pool = open_pool().await?;
    print_json(serde_json::json!({ "status": "ok" }))
}

async fn cmd_agents(cmd: AgentsCmd) -> anyhow::Result<()> {
    let pool = open_pool().await?;
    match cmd {
        AgentsCmd::Add(a) => {
            let agent = store::agents::insert(
                &pool,
                store::agents::NewAgent {
                    name: a.name,
                    preset: a.preset,
                    project_id: a.project,
                    task: a.task,
                    model: a.model,
                    persistent: a.persistent,
                    notify_session: a.notify_session,
                },
            )
            .await?;
            print_json(&agent)
        }
        AgentsCmd::List => {
            let agents = store::agents::list(&pool).await?;
            print_json(&agents)
        }
        AgentsCmd::Show { id } => {
            let agent = store::agents::get_by_id(&pool, &id)
                .await?
                .with_context(|| format!("agent {id} not found"))?;
            print_json(&agent)
        }
        AgentsCmd::Rm { id } => {
            let removed = store::agents::delete(&pool, &id).await?;
            print_json(serde_json::json!({ "id": id, "removed": removed }))
        }
    }
}

async fn cmd_triggers(cmd: TriggersCmd) -> anyhow::Result<()> {
    let pool = open_pool().await?;
    match cmd {
        TriggersCmd::Add(a) => cmd_triggers_add(&pool, a).await,
        TriggersCmd::List { agent } => {
            let v = match agent {
                Some(id) => store::triggers::list_for_agent(&pool, &id).await?,
                None => {
                    // store::triggers has no `list_all` helper — do a raw query.
                    sqlx::query_as::<_, crate::types::Trigger>(
                        "SELECT * FROM triggers ORDER BY created_at DESC",
                    )
                    .fetch_all(&pool)
                    .await?
                }
            };
            print_json(&v)
        }
        TriggersCmd::Rm { id } => {
            let removed = store::triggers::delete(&pool, &id).await?;
            print_json(serde_json::json!({ "id": id, "removed": removed }))
        }
        TriggersCmd::Pause { id } => {
            store::triggers::pause_outside_txn(&pool, &id).await?;
            print_json(serde_json::json!({ "id": id, "paused": true }))
        }
        TriggersCmd::Resume { id } => {
            store::triggers::unpause(&pool, &id).await?;
            print_json(serde_json::json!({ "id": id, "paused": false }))
        }
    }
}

async fn cmd_triggers_add(pool: &SqlitePool, a: TriggerAddArgs) -> anyhow::Result<()> {
    use chrono::Utc;

    // Validate that the referenced agent exists — clearer error than a
    // SQLite FK constraint failure at INSERT time.
    if store::agents::get_by_id(pool, &a.agent).await?.is_none() {
        anyhow::bail!("agent {} not found", a.agent);
    }

    let trigger = match (a.cron.as_deref(), a.oneshot.as_deref()) {
        (Some(expr), None) => {
            // Parse-time validation. The ArgGroup already enforces XOR; we
            // additionally verify croner accepts the expression and that the
            // timezone is a valid IANA name BEFORE inserting the row.
            croner::Cron::new(expr)
                .parse()
                .with_context(|| format!("invalid cron expression: {expr:?}"))?;
            let tz: chrono_tz::Tz =
                a.tz.parse()
                    .with_context(|| format!("invalid timezone: {:?}", a.tz))?;
            let next = compute_next_cron(expr, &tz)
                .ok_or_else(|| anyhow::anyhow!("cron expression has no future occurrence"))?;
            store::triggers::insert_cron(
                pool,
                store::triggers::NewCronTrigger {
                    agent_id: a.agent,
                    cron_expr: expr.to_string(),
                    timezone: a.tz,
                    next_fire_at: next,
                },
            )
            .await?
        }
        (None, Some(when)) => {
            let fire_at = chrono::DateTime::parse_from_rfc3339(when)
                .with_context(|| format!("invalid RFC-3339 instant: {when:?}"))?
                .with_timezone(&Utc);
            store::triggers::insert_oneshot(
                pool,
                store::triggers::NewOneshotTrigger {
                    agent_id: a.agent,
                    fire_at,
                },
            )
            .await?
        }
        // ArgGroup guarantees exactly one of cron/oneshot is set.
        _ => unreachable!("clap ArgGroup enforces --cron|--oneshot XOR"),
    };
    print_json(&trigger)
}

fn compute_next_cron(expr: &str, tz: &chrono_tz::Tz) -> Option<chrono::DateTime<chrono::Utc>> {
    let cron = croner::Cron::new(expr).parse().ok()?;
    let now = chrono::Utc::now().with_timezone(tz);
    cron.find_next_occurrence(&now, false)
        .ok()
        .map(|t| t.with_timezone(&chrono::Utc))
}

async fn cmd_subscribers(cmd: SubscribersCmd) -> anyhow::Result<()> {
    let pool = open_pool().await?;
    match cmd {
        SubscribersCmd::Add(a) => {
            let kind = crate::types::SubscriberKind::parse(&a.kind).ok_or_else(|| {
                anyhow::anyhow!(
                    "unknown subscriber kind: {:?} (expected webhook|notify_native)",
                    a.kind
                )
            })?;
            // Verify the --config string is well-formed JSON. The dispatcher
            // parses per-kind later; rejecting garbage here gives a clearer
            // error.
            serde_json::from_str::<serde_json::Value>(&a.config)
                .with_context(|| format!("--config is not valid JSON: {:?}", a.config))?;

            // ArgGroup already enforces XOR; validate target existence so we
            // don't insert a row that immediately violates the FK.
            if let Some(ref aid) = a.agent {
                if store::agents::get_by_id(&pool, aid).await?.is_none() {
                    anyhow::bail!("agent {aid} not found");
                }
            }
            if let Some(ref tid) = a.trigger {
                if store::triggers::get_by_id(&pool, tid).await?.is_none() {
                    anyhow::bail!("trigger {tid} not found");
                }
            }

            let sub = store::subscribers::insert(
                &pool,
                store::subscribers::NewSubscriber {
                    agent_id: a.agent,
                    trigger_id: a.trigger,
                    kind,
                    config_json: a.config,
                    order_index: a.order,
                },
            )
            .await?;
            print_json(&sub)
        }
        SubscribersCmd::List { agent, trigger } => {
            let v = match (agent, trigger) {
                (Some(a), None) => store::subscribers::list_for_agent(&pool, &a).await?,
                (None, Some(t)) => store::subscribers::list_for_trigger(&pool, &t).await?,
                (Some(_), Some(_)) => {
                    anyhow::bail!("--agent and --trigger are mutually exclusive")
                }
                (None, None) => sqlx::query_as::<_, crate::types::Subscriber>(
                    "SELECT * FROM fire_subscribers ORDER BY created_at DESC",
                )
                .fetch_all(&pool)
                .await
                .context("listing subscribers")?,
            };
            print_json(&v)
        }
        SubscribersCmd::Rm { id } => {
            let removed = store::subscribers::delete(&pool, &id).await?;
            print_json(serde_json::json!({ "id": id, "removed": removed }))
        }
    }
}

async fn cmd_fires(cmd: FiresCmd) -> anyhow::Result<()> {
    let pool = open_pool().await?;
    match cmd {
        FiresCmd::List { agent, limit } => {
            let v = store::fires::list_for_agent(&pool, &agent, limit).await?;
            print_json(&v)
        }
        FiresCmd::Show { id } => {
            // v1: dump the `fires` row JSON. Streaming the journal via
            // ClientCommand::ReadJournal (fires.session_id) is future work —
            // gated on a roy-side decision about whether roy-scheduler is
            // allowed to use control commands beyond Fire (the boundary doc
            // currently allows the protocol types but suggests we only call
            // Fire from this crate).
            let fire = store::fires::get_by_id(&pool, &id)
                .await?
                .with_context(|| format!("fire {id} not found"))?;
            print_json(&fire)
        }
    }
}

async fn cmd_fire_now(args: FireNowArgs) -> anyhow::Result<ExitCode> {
    use std::time::Duration;

    use crate::driver;

    let pool = open_pool().await?;
    let socket = default_socket();
    let timeout = Duration::from_secs(args.fire_timeout.unwrap_or(600));

    let fire =
        driver::fire_agent_ad_hoc(&pool, &socket, &args.agent_id, timeout, args.parent).await?;

    // Map terminal status to exit code. The "ok" / "error" / "timeout"
    // strings come from FireStatus's column representation.
    let exit = match fire.status.as_str() {
        "ok" => ExitCode::SUCCESS,
        // A transport-level failure (no daemon, hang-up) surfaces here
        // as an "error" row with the `roy_client:` prefix in
        // error_message — exit 2 in that case, exit 1 for genuine
        // agent-side errors and timeouts.
        "error" => {
            if fire
                .error_message
                .as_deref()
                .is_some_and(|m| m.starts_with("roy_client:"))
            {
                ExitCode::from(2)
            } else {
                ExitCode::from(1)
            }
        }
        _ => ExitCode::from(1),
    };

    print_json(&fire)?;
    Ok(exit)
}

#[cfg(test)]
mod fire_now_args_tests {
    use super::Cli;
    use clap::Parser;

    #[test]
    fn fire_now_accepts_parent_flag() {
        let cli = Cli::try_parse_from([
            "roy-scheduler",
            "fire-now",
            "agent-uuid",
            "--parent",
            "parent-sid",
        ]);
        assert!(cli.is_ok(), "expected success, got {:?}", cli.err());
    }

    #[test]
    fn fire_now_parent_is_optional() {
        let cli = Cli::try_parse_from(["roy-scheduler", "fire-now", "agent-uuid"]);
        assert!(cli.is_ok(), "expected success, got {:?}", cli.err());
    }
}
