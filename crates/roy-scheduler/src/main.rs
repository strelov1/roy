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

use anyhow::Context;
use clap::{ArgGroup, Parser, Subcommand};
use roy_scheduler::{db, store};
use sqlx::SqlitePool;

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

    use roy_scheduler::driver::{self, ServeOpts};

    let pid_path = args.pid_file.unwrap_or_else(default_pid_file);
    // The lock is held for the lifetime of this function — when the loop
    // exits (currently only on a panic propagated out of driver::serve)
    // Drop releases the pid file. A SIGINT here lets tokio cancel the
    // future, dropping the lock the same way.
    let _lock = pid_lock::PidLock::acquire(&pid_path)
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
                    sqlx::query_as::<_, roy_scheduler::types::Trigger>(
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
            let kind = roy_scheduler::types::SubscriberKind::parse(&a.kind).ok_or_else(|| {
                anyhow::anyhow!(
                    "unknown subscriber kind: {:?} (expected inject_parent|webhook|notify_native|chain_agent)",
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
                (None, None) => sqlx::query_as::<_, roy_scheduler::types::Subscriber>(
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

    use roy_scheduler::driver;

    let pool = open_pool().await?;
    let socket = default_socket();
    let timeout = Duration::from_secs(args.fire_timeout.unwrap_or(600));

    let fire = driver::fire_agent_ad_hoc(&pool, &socket, &args.agent_id, timeout).await?;

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
