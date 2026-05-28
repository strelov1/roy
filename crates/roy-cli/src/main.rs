//! `roy` CLI: a thin trigger over the `roy serve` daemon.
//!
//! Subcommands defined per `docs/wire-protocol.md`.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::Arc;

use anyhow::{anyhow, Context};
use clap::{Args, Parser, Subcommand};
#[cfg(test)]
use roy::AgentPreset;
use roy::{
    daemon::{Daemon, DefaultTransportFactory},
    AgentsConfigStatus, ClientCommand, JournalEntry, ServeOpts, ServerEvent, TurnEvent,
};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixStream;

mod auth;
mod management;

#[derive(Parser)]
#[command(
    name = "roy",
    about = "Spawn and orchestrate coding-agent sessions",
    version
)]
struct Cli {
    #[command(subcommand)]
    command: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Run the daemon that owns the SessionManager.
    Serve(ServeArgs),
    /// Health-probe the daemon. Connects to `$ROY_SOCKET`, prints one JSON
    /// line and exits 0 if reachable, 2 otherwise. Use this in scripts and
    /// skills instead of `pgrep`-ing for the binary.
    Status,
    /// Spawn one session, send one prompt, stream events to stdout.
    Run(RunArgs),
    /// Attach to an existing session and stream its journal to stdout.
    Attach(AttachArgs),
    /// Resurrect a previously-closed session (reads its on-disk metadata,
    /// rebuilds the engine with the same id and journal).
    Resume(ResumeArgs),
    /// List live sessions known to the daemon.
    List,
    /// List sessions whose journals exist on disk but are not live (closed
    /// sessions, restart survivors).
    ListArchived,
    /// Ask the daemon to close a session.
    Close(CloseArgs),
    /// Long-poll for the next terminal Result on a session.
    Wait(WaitArgs),
    /// One-shot fire: spawn (or resume) a session, send a prompt, wait for the result.
    Fire(FireArgs),
    /// Inject a message into a live session as a background note (no input
    /// lease needed). A background agent calls this to notify a session.
    Inject(InjectArgs),
    /// Run an MCP server (stdio JSON-RPC) that exposes roy daemon operations
    /// as MCP tools. Spawn this from an MCP-aware client (Claude Desktop,
    /// IDE plugin) which talks to it over stdio.
    Mcp(McpArgs),
    /// Run the chat-platform gateway (Telegram + WebSocket relay).
    /// Long-running process; talks to a running `roy serve` daemon.
    Gateway(roy_gateway::Args),
    /// Cron + one-shot fire dispatcher for roy. Has its own subcommands
    /// (`serve`, `status`, `migrate`, `agents`, `triggers`, `subscribers`,
    /// `fires`, `fire-now`).
    Scheduler(roy_scheduler::cli::Cli),
    /// Run the management HTTP API (agent CRUD + spawn endpoints).
    Management(roy_management::Args),
    /// Start the inbound event bus (axum webhook server + dispatcher).
    Inbound(roy_inbound::cli::Args),
    /// Inspect configured engines at `~/.config/roy/agents.toml`.
    Engines {
        #[command(subcommand)]
        cmd: EnginesCmd,
    },
    /// Manage projects (HTTP-routed through roy-management).
    Projects {
        #[command(subcommand)]
        cmd: ProjectsCmd,
    },
    /// Set the tag map for a session (HTTP-routed through roy-management).
    SetTags(SetTagsArgs),
    /// User auth: log in, show current user, or reset a password.
    Auth(AuthArgs),
}

#[derive(clap::Args)]
struct AuthArgs {
    #[command(subcommand)]
    cmd: AuthCmd,
    /// roy-management base URL. Overrides $ROY_MANAGEMENT_URL.
    #[arg(
        long,
        env = "ROY_MANAGEMENT_URL",
        default_value = "http://127.0.0.1:8079"
    )]
    api: String,
}

#[derive(Subcommand)]
enum AuthCmd {
    /// Prompt for username and password, POST to /auth/login, save the cookie.
    Login,
    /// Show the currently-authenticated user (calls /auth/me with saved cookie).
    Whoami,
    /// Provision a new user directly in the agents DB. No server / no current
    /// session needed — local-DB-access is the credential. Password resolution
    /// order: --password, $ROY_NEW_PASSWORD, piped stdin, interactive prompt.
    Create {
        username: String,
        /// Display name shown in UIs. Defaults to the username.
        #[arg(long)]
        display_name: Option<String>,
        /// Password (visible in `ps` — prefer piping or interactive prompt).
        #[arg(long)]
        password: Option<String>,
    },
    /// Reset a user's password directly in the agents DB. No login required —
    /// this is the local escape hatch when no one can sign in.
    Reset {
        username: String,
        /// Password (visible in `ps` — prefer piping or interactive prompt).
        #[arg(long)]
        password: Option<String>,
    },
}

#[derive(Subcommand)]
enum ProjectsCmd {
    /// List all projects.
    List,
    /// Create a new project with the given name.
    Create { name: String },
    /// Delete a project by id.
    Delete { id: String },
}

#[derive(clap::Args)]
struct SetTagsArgs {
    /// Session id to update.
    session: String,
    /// Replace the tag map with these `--tag k=v` pairs (repeatable).
    #[arg(long = "tag", value_parser = parse_tag_kv)]
    tags: Vec<(String, String)>,
}

#[derive(clap::Args)]
struct ServeArgs {
    #[arg(long)]
    socket: Option<PathBuf>,
    #[arg(long)]
    journal_dir: Option<PathBuf>,
    /// Root directory where roy creates project and orphan session dirs.
    /// Defaults to `~/.roy/workspace/` or `ROY_WORKSPACE`.
    #[arg(long)]
    workspace_dir: Option<PathBuf>,
    /// On startup, resume every archived session found in journal_dir.
    #[arg(long)]
    resume_all: bool,
    /// Auto-close sessions quiet for more than this many seconds. 0 disables.
    #[arg(long)]
    idle_timeout: Option<u64>,
}

#[derive(clap::Args)]
struct RunArgs {
    /// claude | gemini | opencode | codex | pi
    agent: String,
    task: String,
    /// Working directory to spawn the agent in. Omit to create an orphan
    /// session in the daemon's workspace.
    #[arg(long)]
    cwd: Option<PathBuf>,
    #[arg(long)]
    model: Option<String>,
    /// allow | deny (ACP agents only)
    #[arg(long)]
    permission: Option<String>,
    /// Spawn the session and exit immediately, leaving it running on the daemon.
    #[arg(long)]
    detach: bool,
    #[arg(long)]
    resume: Option<String>,
    /// Prefix journal entries with their seq.
    #[arg(long)]
    with_seq: bool,
    /// Inline system/persona prompt for the session.
    #[arg(long)]
    system_prompt: Option<String>,
    /// Read the system/persona prompt from a file (overrides --system-prompt).
    #[arg(long)]
    system_prompt_file: Option<std::path::PathBuf>,
    /// Project to spawn into (routes through roy-management). If absent,
    /// `--cwd` is used directly against the daemon.
    #[arg(long)]
    project: Option<String>,
    /// Tag map: `--tag k=v` (repeatable). Routes through roy-management.
    #[arg(long = "tag", value_parser = parse_tag_kv)]
    tags: Vec<(String, String)>,
    /// Friendly display name for the session in roy-management.
    #[arg(long)]
    agent_name: Option<String>,
}

#[derive(clap::Args)]
struct AttachArgs {
    session: String,
    #[arg(long)]
    from_seq: Option<u64>,
    #[arg(long)]
    with_seq: bool,
}

#[derive(clap::Args)]
struct ResumeArgs {
    session: String,
}

#[derive(clap::Args)]
struct CloseArgs {
    session: String,
}

#[derive(clap::Args)]
struct InjectArgs {
    /// The live session to inject into.
    session: String,
    /// The message text.
    text: String,
    /// Optional source session id to link the note back to (e.g. the child
    /// background session that produced this message).
    #[arg(long)]
    source: Option<String>,
}

#[derive(clap::Args)]
struct WaitArgs {
    session: String,
    #[arg(long)]
    since_seq: Option<u64>,
    /// Default 600_000 (10 min).
    #[arg(long)]
    timeout_ms: Option<u64>,
}

#[derive(clap::Args)]
struct FireArgs {
    /// The prompt to send to the agent.
    prompt: String,
    /// Preset to spawn: claude | gemini | opencode | codex | pi. Required
    /// when `--resume` is absent.
    #[arg(long, conflicts_with = "resume", required_unless_present = "resume")]
    agent: Option<String>,
    /// Resume an existing session id instead of spawning a new one.
    #[arg(long)]
    resume: Option<String>,
    #[arg(long = "tag", value_parser = parse_tag_kv)]
    tags: Vec<(String, String)>,
    #[arg(long)]
    timeout_ms: Option<u64>,
}

#[derive(clap::Args)]
struct McpArgs {
    /// Override the daemon socket the MCP tools connect to.
    #[arg(long)]
    socket: Option<PathBuf>,
}

#[derive(Args, Debug)]
struct MgmtBaseArgs {
    /// roy-management base URL. Overrides $ROY_MANAGEMENT_URL.
    #[arg(
        long,
        env = "ROY_MANAGEMENT_URL",
        default_value = "http://127.0.0.1:8079"
    )]
    mgmt_url: String,
}

#[derive(Subcommand)]
enum EnginesCmd {
    /// List configured engines (and optionally their models).
    List(EnginesListArgs),
}

#[derive(clap::Args)]
struct EnginesListArgs {
    /// One row per (engine, model) instead of summary per engine.
    #[arg(long)]
    models: bool,
    /// Machine-readable JSON output — the full EnginesList event.
    #[arg(long)]
    json: bool,
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
            eprintln!("roy: failed to start tokio runtime: {e}");
            return ExitCode::from(2);
        }
    };
    rt.block_on(async {
        match dispatch(cli).await {
            Ok(code) => code,
            Err(e) => {
                eprintln!("roy: {e:#}");
                ExitCode::from(2)
            }
        }
    })
}

async fn dispatch(cli: Cli) -> anyhow::Result<ExitCode> {
    match cli.command {
        Cmd::Serve(args) => cmd_serve(args).await.map(|()| ExitCode::SUCCESS),
        Cmd::Status => Ok(cmd_status().await),
        Cmd::Run(args) => cmd_run(args).await,
        Cmd::Attach(args) => cmd_attach(args).await,
        Cmd::Resume(args) => cmd_resume(args).await.map(|()| ExitCode::SUCCESS),
        Cmd::List => cmd_list(false).await.map(|()| ExitCode::SUCCESS),
        Cmd::ListArchived => cmd_list(true).await.map(|()| ExitCode::SUCCESS),
        Cmd::Close(args) => cmd_close(args).await.map(|()| ExitCode::SUCCESS),
        Cmd::Wait(args) => cmd_wait(args).await,
        Cmd::Fire(args) => cmd_fire(args).await,
        Cmd::Inject(args) => cmd_inject(args).await,
        Cmd::Mcp(args) => {
            let socket = args.socket.unwrap_or_else(default_socket);
            roy_mcp::run(socket).await.map(|()| ExitCode::SUCCESS)
        }
        Cmd::Gateway(args) => roy_gateway::run(args).await.map(|()| ExitCode::SUCCESS),
        Cmd::Scheduler(args) => roy_scheduler::cli::run(args).await,
        Cmd::Management(args) => roy_management::run(args).await.map(|()| ExitCode::SUCCESS),
        Cmd::Inbound(args) => roy_inbound::cli::run(args)
            .await
            .map(|()| ExitCode::SUCCESS),
        Cmd::Engines { cmd } => cmd_engines(cmd).await,
        Cmd::Projects { cmd } => cmd_projects(cmd).await,
        Cmd::SetTags(args) => cmd_set_tags(args).await,
        Cmd::Auth(args) => cmd_auth(args).await,
    }
}

async fn cmd_auth(args: AuthArgs) -> anyhow::Result<ExitCode> {
    match args.cmd {
        AuthCmd::Login => crate::auth::login(&args.api).await?,
        AuthCmd::Whoami => crate::auth::whoami(&args.api).await?,
        AuthCmd::Create {
            username,
            display_name,
            password,
        } => {
            crate::auth::create_user(&username, display_name.as_deref(), password.as_deref())
                .await?
        }
        AuthCmd::Reset { username, password } => {
            crate::auth::reset_password(&username, password.as_deref()).await?
        }
    }
    Ok(ExitCode::SUCCESS)
}

async fn cmd_projects(cmd: ProjectsCmd) -> anyhow::Result<ExitCode> {
    match cmd {
        ProjectsCmd::List => {
            let projects = crate::management::list_projects().await?;
            for p in projects {
                println!("{}\t{}\t{}", p.id, p.name, p.path);
            }
            Ok(ExitCode::SUCCESS)
        }
        ProjectsCmd::Create { name } => {
            let p = crate::management::create_project(&name).await?;
            println!("{}\t{}\t{}", p.id, p.name, p.path);
            Ok(ExitCode::SUCCESS)
        }
        ProjectsCmd::Delete { id } => {
            crate::management::delete_project(&id).await?;
            Ok(ExitCode::SUCCESS)
        }
    }
}

async fn cmd_set_tags(args: SetTagsArgs) -> anyhow::Result<ExitCode> {
    let tags: BTreeMap<String, String> = args.tags.into_iter().collect();
    crate::management::put_tags(&args.session, &tags).await?;
    Ok(ExitCode::SUCCESS)
}

/// Set up tracing on stderr so `roy run`/`roy mcp` keep stdout reserved for
/// their JSON payload. `RUST_LOG` overrides the default ("info" for roy and
/// every linked-in adapter crate, "warn" for everything else).
fn init_tracing() {
    use tracing_subscriber::EnvFilter;
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| {
        EnvFilter::new(
            "roy=info,roy_cli=info,roy_mcp=info,roy_gateway=info,roy_scheduler=info,roy_management=info,warn",
        )
    });
    let _ = tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_writer(std::io::stderr)
        .with_target(true)
        .try_init();
}

fn default_socket() -> PathBuf {
    if let Ok(s) = std::env::var("ROY_SOCKET") {
        return PathBuf::from(s);
    }
    let home = std::env::var_os("HOME").unwrap_or_default();
    PathBuf::from(home).join(".roy/daemon.sock")
}

fn default_journal_dir() -> PathBuf {
    if let Ok(s) = std::env::var("ROY_JOURNAL_DIR") {
        return PathBuf::from(s);
    }
    let home = std::env::var_os("HOME").unwrap_or_default();
    PathBuf::from(home).join(".roy/journals")
}

fn default_workspace_dir() -> PathBuf {
    if let Ok(s) = std::env::var("ROY_WORKSPACE") {
        return PathBuf::from(s);
    }
    let home = std::env::var_os("HOME").unwrap_or_default();
    PathBuf::from(home).join(".roy/workspace")
}

async fn cmd_serve(args: ServeArgs) -> anyhow::Result<()> {
    let socket = args.socket.unwrap_or_else(default_socket);
    let journal_dir = args.journal_dir.unwrap_or_else(default_journal_dir);
    let workspace_dir = args.workspace_dir.unwrap_or_else(default_workspace_dir);
    let daemon = Arc::new(
        Daemon::new(
            journal_dir,
            workspace_dir,
            Arc::new(DefaultTransportFactory),
        )
        .await?,
    );
    eprintln!("roy serve: listening on {}", socket.display());
    let idle_timeout = args
        .idle_timeout
        .filter(|n| *n > 0)
        .map(std::time::Duration::from_secs);
    daemon
        .run_with_opts(ServeOpts {
            socket_path: socket.clone(),
            idle_timeout,
            resume_all: args.resume_all,
        })
        .await
        .with_context(|| format!("listening on {}", socket.display()))?;
    Ok(())
}

/// Open a Unix-socket connection to the daemon, or bail with a hint when no
/// daemon is running. The default socket path is `~/.roy/daemon.sock` and
/// `ROY_SOCKET` overrides it.
async fn connect() -> anyhow::Result<UnixStream> {
    let path = default_socket();
    UnixStream::connect(&path).await.map_err(|e| {
        anyhow!(
            "no daemon at {} ({e}) — start it with `roy serve`",
            path.display()
        )
    })
}

/// Open a daemon connection and split it into a writer + a line-framed reader,
/// ready for one command/response cycle. Mirrors the same helper in `roy-mcp`.
async fn open_daemon() -> anyhow::Result<(
    tokio::net::unix::OwnedWriteHalf,
    tokio::io::Lines<BufReader<tokio::net::unix::OwnedReadHalf>>,
)> {
    let stream = connect().await?;
    let (reader, writer) = stream.into_split();
    let events = BufReader::new(reader).lines();
    Ok((writer, events))
}

/// Print a one-line JSON health report and return the matching exit code.
/// `status` is `"up"` when the socket accepts a connection, `"down"` otherwise.
/// `pid` is the value in `<socket>.pid` if present — useful for diagnostics
/// when `status="down"` (a stale pid file means a crashed daemon).
async fn cmd_status() -> ExitCode {
    let socket = default_socket();
    let pid_path = roy::pid_lock::pid_path_for_socket(&socket);
    let pid = roy::pid_lock::peek_pid(&pid_path);
    let (status, exit, error) = match UnixStream::connect(&socket).await {
        Ok(_) => ("up", ExitCode::SUCCESS, None),
        Err(e) => ("down", ExitCode::from(2), Some(e.to_string())),
    };
    let payload = serde_json::json!({
        "status": status,
        "socket": socket.display().to_string(),
        "pid_file": pid_path.display().to_string(),
        "pid": pid,
        "error": error,
    });
    println!("{payload}");
    exit
}

async fn send_cmd<W: AsyncWriteExt + Unpin>(w: &mut W, cmd: &ClientCommand) -> anyhow::Result<()> {
    let line = serde_json::to_string(cmd)?;
    w.write_all(line.as_bytes()).await?;
    w.write_all(b"\n").await?;
    w.flush().await?;
    Ok(())
}

async fn cmd_run(args: RunArgs) -> anyhow::Result<ExitCode> {
    validate_flags(&args)?;

    let system_prompt = match (args.system_prompt_file.clone(), args.system_prompt.clone()) {
        (Some(path), _) => Some(
            std::fs::read_to_string(&path)
                .with_context(|| format!("reading --system-prompt-file {}", path.display()))?,
        ),
        (None, inline) => inline,
    };

    let needs_mgmt = args.project.is_some() || !args.tags.is_empty() || args.agent_name.is_some();

    // Phase 1: create the session. Two paths, same outcome: a session id
    // (and, for the direct path, the agent's resume_cursor — management
    // does not currently surface it).
    let (session, resume_cursor) = if needs_mgmt {
        let req = crate::management::CreateSessionReq {
            agent: args.agent.clone(),
            project_id: args.project.clone(),
            cwd: args.cwd.as_ref().map(|p| p.to_string_lossy().into_owned()),
            model: args.model.clone(),
            permission: args.permission.clone(),
            system_prompt: system_prompt.clone(),
            agent_name: args.agent_name.clone(),
            tags: args.tags.iter().cloned().collect(),
        };
        let created = crate::management::create_session(req).await?;
        eprintln!("roy run: session {} (via management)", created.session_id);
        if args.detach {
            let payload = serde_json::json!({
                "type": "session",
                "id": created.session_id,
                "resume_cursor": serde_json::Value::Null,
            });
            println!("{payload}");
            return Ok(ExitCode::SUCCESS);
        }
        (created.session_id, None)
    } else {
        let (mut writer, mut events) = open_daemon().await?;
        send_cmd(
            &mut writer,
            &ClientCommand::Spawn {
                agent: args.agent.clone(),
                cwd: args.cwd.clone(),
                model: args.model.clone(),
                permission: args.permission.clone(),
                resume: args.resume.clone(),
                system_prompt,
                extra_env: Default::default(),
            },
        )
        .await?;
        loop {
            match read_event(&mut events).await? {
                ServerEvent::Spawning { agent } => {
                    if let Some(cwd) = args.cwd.as_ref() {
                        eprintln!("roy run: spawning {agent} in {}…", cwd.display());
                    } else {
                        eprintln!("roy run: spawning {agent}…");
                    }
                }
                ServerEvent::Spawned {
                    session,
                    resume_cursor,
                    ..
                } => {
                    eprintln!("roy run: session {session}");
                    if args.detach {
                        let payload = serde_json::json!({
                            "type": "session",
                            "id": session,
                            "resume_cursor": resume_cursor,
                        });
                        println!("{payload}");
                        return Ok(ExitCode::SUCCESS);
                    }
                    break (session, Some(resume_cursor));
                }
                ServerEvent::Error { code, message, .. } => {
                    anyhow::bail!("spawn failed: {code}: {message}");
                }
                other => anyhow::bail!("unexpected response to Spawn: {other:?}"),
            }
        }
    };

    // Phase 2: attach, acquire input, send, drain. Both paths use a fresh
    // daemon connection to keep the flow uniform — the management path
    // already closed its HTTP connection.
    let (mut writer, mut events) = open_daemon().await?;

    // Attach BEFORE sending so we never miss frames.
    send_cmd(
        &mut writer,
        &ClientCommand::Attach {
            session: session.clone(),
            from_seq: None,
        },
    )
    .await?;
    match read_event(&mut events).await? {
        ServerEvent::Attached { .. } => {}
        ServerEvent::Error { code, message, .. } => {
            anyhow::bail!("attach failed: {code}: {message}");
        }
        other => anyhow::bail!("unexpected response to Attach: {other:?}"),
    }

    // Acquire input + send the task.
    send_cmd(
        &mut writer,
        &ClientCommand::AcquireInput {
            session: session.clone(),
        },
    )
    .await?;
    match read_event(&mut events).await? {
        ServerEvent::InputAcquired { acquired: true, .. } => {}
        ServerEvent::InputAcquired {
            acquired: false, ..
        } => {
            anyhow::bail!("input lease already held by another client");
        }
        other => anyhow::bail!("unexpected response to AcquireInput: {other:?}"),
    }
    send_cmd(
        &mut writer,
        &ClientCommand::Send {
            session: session.clone(),
            text: args.task,
        },
    )
    .await?;

    let exit_code = drain_until_terminal_result(&mut events, args.with_seq).await?;

    // Final session line so the caller can resume later.
    let payload = serde_json::json!({
        "type": "session",
        "id": session,
        "resume_cursor": resume_cursor,
    });
    println!("{payload}");

    // Close the session (it's a one-shot `run`; the daemon keeps the
    // session only if `--detach` was given, which we already returned above).
    let _ = send_cmd(
        &mut writer,
        &ClientCommand::Close {
            session: session.clone(),
        },
    )
    .await;
    let _ = read_event(&mut events).await;

    Ok(exit_code)
}

async fn cmd_attach(args: AttachArgs) -> anyhow::Result<ExitCode> {
    let (mut writer, mut events) = open_daemon().await?;

    send_cmd(
        &mut writer,
        &ClientCommand::Attach {
            session: args.session.clone(),
            from_seq: args.from_seq,
        },
    )
    .await?;
    match read_event(&mut events).await? {
        ServerEvent::Attached { .. } => {}
        ServerEvent::Error { code, message, .. } => {
            anyhow::bail!("attach failed: {code}: {message}");
        }
        other => anyhow::bail!("unexpected response to Attach: {other:?}"),
    }

    drain_until_terminal_result(&mut events, args.with_seq).await
}

async fn cmd_list(archived: bool) -> anyhow::Result<()> {
    let (mut writer, mut events) = open_daemon().await?;

    let cmd = if archived {
        ClientCommand::ListArchived
    } else {
        ClientCommand::List
    };
    send_cmd(&mut writer, &cmd).await?;
    match read_event(&mut events).await? {
        ServerEvent::Listed { sessions } | ServerEvent::ListedArchived { sessions } => {
            for s in sessions {
                println!("{}", s.session);
            }
        }
        other => anyhow::bail!("unexpected response to List: {other:?}"),
    }
    Ok(())
}

async fn cmd_resume(args: ResumeArgs) -> anyhow::Result<()> {
    let (mut writer, mut events) = open_daemon().await?;

    send_cmd(
        &mut writer,
        &ClientCommand::Resume {
            session: args.session.clone(),
        },
    )
    .await?;
    loop {
        match read_event(&mut events).await? {
            ServerEvent::Resuming { session } => {
                eprintln!("roy resume: resuming {session}…");
            }
            ServerEvent::Resumed {
                session,
                resume_cursor,
            } => {
                let payload = serde_json::json!({
                    "type": "session",
                    "id": session,
                    "resume_cursor": resume_cursor,
                });
                println!("{payload}");
                return Ok(());
            }
            ServerEvent::Error { code, message, .. } => {
                anyhow::bail!("resume failed: {code}: {message}")
            }
            other => anyhow::bail!("unexpected response to Resume: {other:?}"),
        }
    }
}

async fn cmd_close(args: CloseArgs) -> anyhow::Result<()> {
    let (mut writer, mut events) = open_daemon().await?;

    send_cmd(
        &mut writer,
        &ClientCommand::Close {
            session: args.session.clone(),
        },
    )
    .await?;
    match read_event(&mut events).await? {
        ServerEvent::Closed { .. } => Ok(()),
        ServerEvent::Error { code, message, .. } => {
            anyhow::bail!("close failed: {code}: {message}")
        }
        other => anyhow::bail!("unexpected response to Close: {other:?}"),
    }
}

async fn cmd_inject(args: InjectArgs) -> anyhow::Result<ExitCode> {
    let (mut writer, mut events) = open_daemon().await?;

    send_cmd(
        &mut writer,
        &ClientCommand::Inject {
            session: args.session.clone(),
            text: args.text,
            source_session: args.source,
        },
    )
    .await?;
    match read_event(&mut events).await? {
        ServerEvent::Injected { session, seq } => {
            let payload = serde_json::json!({
                "type": "injected",
                "session": session,
                "seq": seq,
            });
            println!("{payload}");
            Ok(ExitCode::SUCCESS)
        }
        ServerEvent::Error { code, message, .. } => {
            eprintln!("roy inject: {code}: {message}");
            Ok(ExitCode::from(2))
        }
        other => anyhow::bail!("unexpected response to Inject: {other:?}"),
    }
}

async fn cmd_wait(args: WaitArgs) -> anyhow::Result<ExitCode> {
    let (mut writer, mut events) = open_daemon().await?;

    send_cmd(
        &mut writer,
        &ClientCommand::WaitForResult {
            session: args.session.clone(),
            since_seq: args.since_seq,
            timeout_ms: args.timeout_ms,
        },
    )
    .await?;

    match read_event(&mut events).await? {
        ServerEvent::ResultReady {
            session,
            seq,
            result,
            assistant_text,
        } => {
            let TurnEvent::Result {
                cost_usd,
                stop_reason,
            } = &result
            else {
                anyhow::bail!("daemon sent non-Result in ResultReady: {result:?}");
            };
            let payload = serde_json::json!({
                "type": "result_ready",
                "session": session,
                "seq": seq,
                "stop_reason": format!("{stop_reason:?}"),
                "cost_usd": cost_usd,
                "assistant_text": assistant_text,
            });
            println!("{payload}");
            Ok(if stop_reason.is_error() {
                ExitCode::from(1)
            } else {
                ExitCode::SUCCESS
            })
        }
        ServerEvent::WaitTimeout { session } => {
            let payload = serde_json::json!({
                "type": "wait_timeout",
                "session": session,
            });
            println!("{payload}");
            Ok(ExitCode::from(2))
        }
        ServerEvent::Error { code, message, .. } => {
            anyhow::bail!("wait failed: {code}: {message}");
        }
        other => anyhow::bail!("unexpected response to WaitForResult: {other:?}"),
    }
}

async fn cmd_fire(args: FireArgs) -> anyhow::Result<ExitCode> {
    use roy::FireTarget;

    let target = match (args.agent, args.resume) {
        (Some(agent), None) => FireTarget::Spawn {
            preset: agent,
            system_prompt: None,
        },
        (None, Some(session_id)) => FireTarget::Resume { session_id },
        (Some(_), Some(_)) => anyhow::bail!("--agent conflicts with --resume"),
        (None, None) => anyhow::bail!("provide either --agent or --resume"),
    };

    let tags: BTreeMap<String, String> = args.tags.into_iter().collect();

    let (mut writer, mut events) = open_daemon().await?;

    send_cmd(
        &mut writer,
        &ClientCommand::Fire {
            target,
            prompt: args.prompt,
            tags,
            timeout_ms: args.timeout_ms,
        },
    )
    .await?;

    match read_event(&mut events).await? {
        ServerEvent::FireDone {
            session,
            seq_range,
            result,
            assistant_text,
        } => {
            let TurnEvent::Result {
                cost_usd,
                stop_reason,
            } = &result
            else {
                anyhow::bail!("daemon sent non-Result in FireDone: {result:?}");
            };
            let payload = serde_json::json!({
                "type": "fire_done",
                "session": session,
                "seq_range": seq_range,
                "stop_reason": format!("{stop_reason:?}"),
                "cost_usd": cost_usd,
                "assistant_text": assistant_text,
            });
            println!("{payload}");
            Ok(if stop_reason.is_error() {
                ExitCode::from(1)
            } else {
                ExitCode::SUCCESS
            })
        }
        ServerEvent::FireTimeout {
            session,
            partial_seq_range,
        } => {
            let payload = serde_json::json!({
                "type": "fire_timeout",
                "session": session,
                "partial_seq_range": partial_seq_range,
            });
            println!("{payload}");
            Ok(ExitCode::from(2))
        }
        ServerEvent::FireError {
            session,
            code,
            message,
        } => {
            let payload = serde_json::json!({
                "type": "fire_error",
                "session": session,
                "code": code.to_string(),
                "message": message,
            });
            println!("{payload}");
            Ok(ExitCode::from(2))
        }
        other => anyhow::bail!("unexpected response to Fire: {other:?}"),
    }
}

async fn cmd_engines(cmd: EnginesCmd) -> anyhow::Result<ExitCode> {
    match cmd {
        EnginesCmd::List(args) => cmd_engines_list(args).await,
    }
}

async fn cmd_engines_list(args: EnginesListArgs) -> anyhow::Result<ExitCode> {
    let (mut writer, mut events) = open_daemon().await?;

    send_cmd(&mut writer, &ClientCommand::ListAgents).await?;
    let ev = read_event(&mut events).await?;
    let ServerEvent::AgentsList {
        agents,
        config_path,
        status,
    } = ev
    else {
        anyhow::bail!("unexpected response to ListAgents: {ev:?}");
    };

    if args.json {
        let payload = serde_json::json!({
            "agents": agents,
            "config_path": config_path,
            "status": status,
        });
        println!("{}", serde_json::to_string(&payload)?);
        return Ok(ExitCode::SUCCESS);
    }

    match &status {
        AgentsConfigStatus::Created => {
            eprintln!("created sample at {}", config_path.display());
        }
        AgentsConfigStatus::Invalid { reason } => {
            eprintln!("config invalid ({}): {reason}", config_path.display());
            return Ok(ExitCode::from(1));
        }
        AgentsConfigStatus::Ok if agents.is_empty() => {
            eprintln!("no engines configured in {}", config_path.display());
        }
        AgentsConfigStatus::Ok => {}
    }

    if args.models {
        for a in &agents {
            for m in &a.models {
                let mark = if m.default { "*default" } else { "" };
                println!("{}\t{}\t{}\t{}", a.preset, m.id, m.label, mark);
            }
        }
    } else {
        for a in &agents {
            let default = a
                .models
                .iter()
                .find(|m| m.default)
                .map(|m| m.id.as_str())
                .unwrap_or("-");
            println!(
                "{}\t{} models\t(default: {})",
                a.preset,
                a.models.len(),
                default
            );
        }
    }
    Ok(ExitCode::SUCCESS)
}

/// Parse a CLI `--tag k=v` argument. Empty key is rejected. The first `=`
/// is the separator; subsequent `=` characters are part of the value.
pub(crate) fn parse_tag_kv(s: &str) -> anyhow::Result<(String, String)> {
    let (key, value) = s
        .split_once('=')
        .ok_or_else(|| anyhow!("expected k=v, got `{s}`"))?;
    if key.is_empty() {
        anyhow::bail!("tag key must not be empty (got `{s}`)");
    }
    Ok((key.to_string(), value.to_string()))
}

fn validate_flags(args: &RunArgs) -> anyhow::Result<()> {
    // Only claude-code-acp accepts a per-spawn `--model` switch over ACP
    // (via its slash-command). For other presets the model is fixed by the
    // CLI's own config or agents.toml, so a runtime `--model` would be
    // silently ignored — bail loudly instead.
    if args.model.is_some() && args.agent != "claude" {
        anyhow::bail!("--model only applies to claude");
    }
    if let Some(p) = args.permission.as_deref() {
        if !matches!(p, "allow" | "deny") {
            anyhow::bail!("--permission must be 'allow' or 'deny'");
        }
    }
    Ok(())
}

fn print_entry(entry: &JournalEntry, with_seq: bool) {
    let line = if with_seq {
        serde_json::to_string(entry).expect("serialize")
    } else {
        serde_json::to_string(&entry.event).expect("serialize")
    };
    println!("{line}");
}

/// Local helper around `next_event` that yields a single-line ServerEvent.
async fn read_event<R: AsyncBufReadExt + Unpin>(
    lines: &mut tokio::io::Lines<R>,
) -> anyhow::Result<ServerEvent> {
    let line = lines
        .next_line()
        .await?
        .ok_or_else(|| anyhow!("daemon hung up"))?;
    Ok(serde_json::from_str(line.trim())?)
}

async fn drain_until_terminal_result<R: AsyncBufReadExt + Unpin>(
    events: &mut tokio::io::Lines<R>,
    with_seq: bool,
) -> anyhow::Result<ExitCode> {
    loop {
        match read_event(events).await? {
            ServerEvent::Frame { entry, .. } => {
                print_entry(&entry, with_seq);
                if let TurnEvent::Result {
                    ref stop_reason, ..
                } = entry.event
                {
                    return Ok(if stop_reason.is_error() {
                        ExitCode::from(1)
                    } else {
                        ExitCode::SUCCESS
                    });
                }
            }
            ServerEvent::Error { code, message, .. } => {
                anyhow::bail!("agent error: {code}: {message}");
            }
            other => {
                eprintln!("roy: skipping unexpected event: {other:?}");
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build a `RunArgs` with sensible defaults; only override what each test
    /// case needs to vary.
    fn args(agent: &str) -> RunArgs {
        RunArgs {
            agent: agent.into(),
            task: "noop".into(),
            cwd: None,
            model: None,
            permission: None,
            detach: false,
            resume: None,
            with_seq: false,
            system_prompt: None,
            system_prompt_file: None,
            project: None,
            tags: Vec::new(),
            agent_name: None,
        }
    }

    #[test]
    fn validate_flags_accepts_acp_agents_without_optional_args() {
        for preset in AgentPreset::ALL {
            let agent = preset.as_str();
            validate_flags(&args(agent)).unwrap_or_else(|e| panic!("{agent}: {e}"));
        }
    }

    #[test]
    fn validate_flags_rejects_model_on_non_claude() {
        for preset in AgentPreset::ALL {
            if *preset == AgentPreset::Claude {
                continue;
            }
            let agent = preset.as_str();
            let mut a = args(agent);
            a.model = Some("gpt-x".into());
            let err = validate_flags(&a).unwrap_err().to_string();
            assert!(
                err.contains("--model"),
                "{agent}: unexpected error message: {err}"
            );
        }
    }

    #[test]
    fn validate_flags_rejects_unknown_permission_value() {
        let mut a = args("opencode");
        a.permission = Some("maybe".into());
        let err = validate_flags(&a).unwrap_err().to_string();
        assert!(err.contains("'allow' or 'deny'"), "unexpected: {err}");
    }

    #[test]
    fn validate_flags_accepts_allow_and_deny_on_acp_agents() {
        for value in ["allow", "deny"] {
            let mut a = args("gemini");
            a.permission = Some(value.into());
            validate_flags(&a).unwrap_or_else(|e| panic!("{value}: {e}"));
        }
    }
}

#[cfg(test)]
mod tag_parser_tests {
    use super::parse_tag_kv;

    #[test]
    fn parses_simple_kv() {
        assert_eq!(
            parse_tag_kv("foo=bar").unwrap(),
            ("foo".to_string(), "bar".to_string())
        );
    }

    #[test]
    fn allows_equals_inside_value() {
        assert_eq!(
            parse_tag_kv("k=a=b=c").unwrap(),
            ("k".to_string(), "a=b=c".to_string())
        );
    }

    #[test]
    fn rejects_empty_key() {
        assert!(parse_tag_kv("=value").is_err());
    }

    #[test]
    fn rejects_no_equals() {
        assert!(parse_tag_kv("no-equals").is_err());
    }
}

#[cfg(test)]
mod fire_args_tests {
    use super::Cli;
    use clap::Parser;

    #[test]
    fn fire_with_agent_and_prompt_parses() {
        let cli = Cli::try_parse_from(["roy", "fire", "hello world", "--agent", "claude"]);
        assert!(cli.is_ok(), "expected success, got {:?}", cli.err());
    }

    #[test]
    fn fire_with_resume_and_prompt_parses() {
        let cli = Cli::try_parse_from(["roy", "fire", "hello world", "--resume", "abc-123"]);
        assert!(cli.is_ok(), "expected success, got {:?}", cli.err());
    }

    #[test]
    fn fire_without_agent_or_resume_rejected() {
        let cli = Cli::try_parse_from(["roy", "fire", "hello world"]);
        assert!(
            cli.is_err(),
            "expected error when neither --agent nor --resume given"
        );
    }

    #[test]
    fn fire_with_agent_and_resume_rejected() {
        let cli = Cli::try_parse_from(["roy", "fire", "p", "--agent", "claude", "--resume", "abc"]);
        assert!(
            cli.is_err(),
            "expected error: --agent conflicts with --resume"
        );
    }

    #[test]
    fn fire_with_project_and_resume_rejected() {
        let cli =
            Cli::try_parse_from(["roy", "fire", "p", "--resume", "abc", "--project", "myproj"]);
        assert!(
            cli.is_err(),
            "expected error: --project conflicts with --resume"
        );
    }
}
