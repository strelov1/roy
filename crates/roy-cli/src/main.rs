//! `roy` CLI: a thin trigger over the `roy serve` daemon.
//!
//! Subcommands defined per `docs/wire-protocol.md`.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::Arc;

use anyhow::{anyhow, Context};
use clap::{Parser, Subcommand};
use roy::{
    daemon::{Daemon, DefaultTransportFactory},
    project::Project,
    AgentsConfigStatus, ClientCommand, JournalEntry, ServeOpts, ServerEvent, TurnEvent,
};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixStream;

mod mcp;

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
    /// Replace the tag map on a live session. Empty `--tag` list clears all tags.
    SetTags(SetTagsArgs),
    /// Long-poll for the next terminal Result on a session.
    Wait(WaitArgs),
    /// One-shot fire: spawn (or resume) a session, send a prompt, wait for the result.
    Fire(FireArgs),
    /// Run an MCP server (stdio JSON-RPC) that exposes roy daemon operations
    /// as MCP tools. Spawn this from an MCP-aware client (Claude Desktop,
    /// IDE plugin) which talks to it over stdio.
    Mcp(McpArgs),
    /// Manage projects (list / create / rename / delete).
    Projects {
        #[command(subcommand)]
        cmd: ProjectsCmd,
    },
    /// Inspect configured agents at `~/.config/roy/agents.toml`.
    Agents {
        #[command(subcommand)]
        cmd: AgentsCmd,
    },
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
    /// Enable WebSocket listener on this port (in addition to the Unix socket).
    #[arg(long)]
    port: Option<u16>,
    /// On startup, resume every archived session found in journal_dir.
    #[arg(long)]
    resume_all: bool,
    /// Auto-close sessions quiet for more than this many seconds. 0 disables.
    #[arg(long)]
    idle_timeout: Option<u64>,
}

#[derive(clap::Args)]
struct RunArgs {
    /// claude | gemini | opencode | codex
    agent: String,
    task: String,
    /// Project name to spawn the session under. Omit to create an orphan session.
    #[arg(long)]
    project: Option<String>,
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
struct SetTagsArgs {
    session: String,
    /// Repeatable: `--tag k=v --tag k2=v2`. Empty list clears all tags.
    #[arg(long = "tag", value_parser = parse_tag_kv)]
    tags: Vec<(String, String)>,
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
    /// Preset to spawn: claude | gemini | opencode | codex. Required when
    /// `--resume` is absent.
    #[arg(long, conflicts_with = "resume", required_unless_present = "resume")]
    agent: Option<String>,
    /// Project name for a new session. Ignored with --resume.
    #[arg(long, conflicts_with = "resume")]
    project: Option<String>,
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

#[derive(Subcommand)]
enum AgentsCmd {
    /// List configured agents (and optionally their models).
    List(AgentsListArgs),
}

#[derive(clap::Args)]
struct AgentsListArgs {
    /// One row per (agent, model) instead of summary per agent.
    #[arg(long)]
    models: bool,
    /// Machine-readable JSON output — the full AgentsList event.
    #[arg(long)]
    json: bool,
}

#[derive(Subcommand)]
enum ProjectsCmd {
    /// List projects.
    List,
    /// Create a new project with the given name. Roy manages the directory at
    /// `<workspace>/<name>/`.
    Create { name: String },
    /// Cascade-delete a project and all its sessions.
    Delete {
        id_or_name: String,
        #[arg(long)]
        yes: bool,
    },
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
        Cmd::Run(args) => cmd_run(args).await,
        Cmd::Attach(args) => cmd_attach(args).await,
        Cmd::Resume(args) => cmd_resume(args).await.map(|()| ExitCode::SUCCESS),
        Cmd::List => cmd_list(false).await.map(|()| ExitCode::SUCCESS),
        Cmd::ListArchived => cmd_list(true).await.map(|()| ExitCode::SUCCESS),
        Cmd::Close(args) => cmd_close(args).await.map(|()| ExitCode::SUCCESS),
        Cmd::SetTags(args) => cmd_set_tags(args).await.map(|()| ExitCode::SUCCESS),
        Cmd::Wait(args) => cmd_wait(args).await,
        Cmd::Fire(args) => cmd_fire(args).await,
        Cmd::Mcp(args) => {
            let socket = args.socket.unwrap_or_else(default_socket);
            mcp::run(socket).await.map(|()| ExitCode::SUCCESS)
        }
        Cmd::Projects { cmd } => cmd_projects(cmd).await.map(|()| ExitCode::SUCCESS),
        Cmd::Agents { cmd } => cmd_agents(cmd).await,
    }
}

/// Set up tracing on stderr so `roy run`/`roy mcp` keep stdout reserved for
/// their JSON payload. `RUST_LOG` overrides the default ("info" for roy,
/// "warn" for everything else).
fn init_tracing() {
    use tracing_subscriber::EnvFilter;
    let filter = EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| EnvFilter::new("roy=info,roy_cli=info,warn"));
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
    let daemon = Arc::new(Daemon::new(
        journal_dir,
        workspace_dir,
        Arc::new(DefaultTransportFactory),
    )?);
    eprintln!("roy serve: listening on {}", socket.display());
    if let Some(port) = args.port {
        eprintln!("roy serve: WebSocket on 127.0.0.1:{port}");
        eprintln!(
            "roy serve: WS auth token at {}",
            socket.with_extension("token").display()
        );
    }
    let idle_timeout = args
        .idle_timeout
        .filter(|n| *n > 0)
        .map(std::time::Duration::from_secs);
    daemon
        .run_with_opts(ServeOpts {
            socket_path: socket.clone(),
            ws_port: args.port,
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

async fn send_cmd<W: AsyncWriteExt + Unpin>(w: &mut W, cmd: &ClientCommand) -> anyhow::Result<()> {
    let line = serde_json::to_string(cmd)?;
    w.write_all(line.as_bytes()).await?;
    w.write_all(b"\n").await?;
    w.flush().await?;
    Ok(())
}

async fn cmd_run(args: RunArgs) -> anyhow::Result<ExitCode> {
    validate_flags(&args)?;

    let stream = connect().await?;
    let (reader, mut writer) = stream.into_split();
    let mut events = BufReader::new(reader).lines();

    // Spawn the session.
    send_cmd(
        &mut writer,
        &ClientCommand::Spawn {
            agent: args.agent.clone(),
            project_id: args.project.clone(),
            model: args.model.clone(),
            permission: args.permission.clone(),
            resume: args.resume.clone(),
            tags: BTreeMap::default(),
        },
    )
    .await?;
    let (session, resume_cursor) = loop {
        match read_event(&mut events).await? {
            ServerEvent::Spawning { agent, project_id } => {
                if let Some(pid) = &project_id {
                    eprintln!("roy run: spawning {agent} in project {pid}…");
                } else {
                    eprintln!("roy run: spawning {agent}…");
                }
            }
            ServerEvent::Spawned {
                session,
                project_id,
                resume_cursor,
                ..
            } => {
                if let Some(pid) = &project_id {
                    eprintln!("roy run: session {session} project {pid}");
                } else {
                    eprintln!("roy run: session {session} (orphan)");
                }
                if args.detach {
                    let payload = serde_json::json!({
                        "type": "session",
                        "id": session,
                        "resume_cursor": resume_cursor,
                    });
                    println!("{payload}");
                    return Ok(ExitCode::SUCCESS);
                }
                break (session, resume_cursor);
            }
            ServerEvent::Error { code, message, .. } => {
                anyhow::bail!("spawn failed: {code}: {message}");
            }
            other => anyhow::bail!("unexpected response to Spawn: {other:?}"),
        }
    };

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
    let stream = connect().await?;
    let (reader, mut writer) = stream.into_split();
    let mut events = BufReader::new(reader).lines();

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
    let stream = connect().await?;
    let (reader, mut writer) = stream.into_split();
    let mut events = BufReader::new(reader).lines();

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
    let stream = connect().await?;
    let (reader, mut writer) = stream.into_split();
    let mut events = BufReader::new(reader).lines();

    send_cmd(
        &mut writer,
        &ClientCommand::Resume {
            session: args.session.clone(),
            tags: None,
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
    let stream = connect().await?;
    let (reader, mut writer) = stream.into_split();
    let mut events = BufReader::new(reader).lines();

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

async fn cmd_set_tags(args: SetTagsArgs) -> anyhow::Result<()> {
    let stream = connect().await?;
    let (reader, mut writer) = stream.into_split();
    let mut events = BufReader::new(reader).lines();

    let tags: BTreeMap<String, String> = args.tags.into_iter().collect();

    send_cmd(
        &mut writer,
        &ClientCommand::SetTags {
            session: args.session.clone(),
            tags: tags.clone(),
        },
    )
    .await?;
    match read_event(&mut events).await? {
        ServerEvent::SessionUpdated {
            session,
            tags: Some(t),
            ..
        } => {
            let payload = serde_json::json!({
                "type": "session_updated",
                "session": session,
                "tags": t,
            });
            println!("{payload}");
            Ok(())
        }
        ServerEvent::Error { code, message, .. } => {
            anyhow::bail!("set-tags failed: {code}: {message}")
        }
        other => anyhow::bail!("unexpected response to SetTags: {other:?}"),
    }
}

async fn cmd_wait(args: WaitArgs) -> anyhow::Result<ExitCode> {
    let stream = connect().await?;
    let (reader, mut writer) = stream.into_split();
    let mut events = BufReader::new(reader).lines();

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
            project_id: args.project,
        },
        (None, Some(session_id)) => FireTarget::Resume { session_id },
        (Some(_), Some(_)) => anyhow::bail!("--agent conflicts with --resume"),
        (None, None) => anyhow::bail!("provide either --agent or --resume"),
    };

    let tags: BTreeMap<String, String> = args.tags.into_iter().collect();

    let stream = connect().await?;
    let (reader, mut writer) = stream.into_split();
    let mut events = BufReader::new(reader).lines();

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

async fn cmd_agents(cmd: AgentsCmd) -> anyhow::Result<ExitCode> {
    match cmd {
        AgentsCmd::List(args) => cmd_agents_list(args).await,
    }
}

async fn cmd_agents_list(args: AgentsListArgs) -> anyhow::Result<ExitCode> {
    let stream = connect().await?;
    let (reader, mut writer) = stream.into_split();
    let mut events = BufReader::new(reader).lines();

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
            eprintln!("no agents configured in {}", config_path.display());
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

async fn cmd_projects(cmd: ProjectsCmd) -> anyhow::Result<()> {
    let stream = connect().await?;
    let (reader, mut writer) = stream.into_split();
    let mut events = BufReader::new(reader).lines();

    match cmd {
        ProjectsCmd::List => {
            send_cmd(&mut writer, &ClientCommand::ListProjects).await?;
            match read_event(&mut events).await? {
                ServerEvent::ProjectsListed { projects } => {
                    for p in projects {
                        println!("{}\t{}\t{}", p.id, p.name, p.path.display());
                    }
                    Ok(())
                }
                ServerEvent::Error { code, message, .. } => Err(anyhow!("{code}: {message}")),
                other => Err(anyhow!("unexpected: {other:?}")),
            }
        }
        ProjectsCmd::Create { name } => {
            send_cmd(&mut writer, &ClientCommand::CreateProject { name }).await?;
            match read_event(&mut events).await? {
                ServerEvent::ProjectCreated { project } => {
                    println!("{}", project.id);
                    eprintln!(
                        "created project '{}' at {}",
                        project.name,
                        project.path.display()
                    );
                    Ok(())
                }
                ServerEvent::Error { code, message, .. } => Err(anyhow!("{code}: {message}")),
                other => Err(anyhow!("unexpected: {other:?}")),
            }
        }
        ProjectsCmd::Delete { id_or_name, yes } => {
            let project_id = resolve_project_id(&mut writer, &mut events, &id_or_name).await?;
            if !yes {
                eprintln!(
                    "This will delete project {project_id} and all its sessions. Re-run with --yes to confirm."
                );
                return Ok(());
            }
            send_cmd(
                &mut writer,
                &ClientCommand::DeleteProject {
                    project_id: project_id.clone(),
                },
            )
            .await?;
            match read_event(&mut events).await? {
                ServerEvent::ProjectDeleted {
                    project_id,
                    deleted_sessions,
                } => {
                    eprintln!(
                        "deleted project {project_id} ({} sessions)",
                        deleted_sessions.len()
                    );
                    Ok(())
                }
                ServerEvent::Error { code, message, .. } => Err(anyhow!("{code}: {message}")),
                other => Err(anyhow!("unexpected: {other:?}")),
            }
        }
    }
}

/// Resolve a user-supplied id-or-name to a project id by listing projects and
/// matching first by id (exact), then by unique name.
async fn resolve_project_id<B: AsyncBufReadExt + Unpin>(
    writer: &mut (impl AsyncWriteExt + Unpin),
    events: &mut tokio::io::Lines<B>,
    query: &str,
) -> anyhow::Result<String> {
    send_cmd(writer, &ClientCommand::ListProjects).await?;
    let projects: Vec<Project> = match read_event(events).await? {
        ServerEvent::ProjectsListed { projects } => projects,
        ServerEvent::Error { code, message, .. } => {
            return Err(anyhow!("{code}: {message}"));
        }
        other => return Err(anyhow!("unexpected: {other:?}")),
    };
    if let Some(p) = projects.iter().find(|p| p.id == query) {
        return Ok(p.id.clone());
    }
    let by_name: Vec<&Project> = projects.iter().filter(|p| p.name == query).collect();
    match by_name.as_slice() {
        [p] => Ok(p.id.clone()),
        [] => Err(anyhow!("no project named or id `{query}`")),
        _ => Err(anyhow!(
            "ambiguous name `{query}` — multiple projects match; specify id"
        )),
    }
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
    let is_acp_only = matches!(args.agent.as_str(), "gemini" | "opencode" | "codex");
    let is_claude_like = matches!(args.agent.as_str(), "claude");

    if args.model.is_some() && !is_claude_like {
        anyhow::bail!("--model only applies to claude");
    }
    if args.permission.is_some() && !(is_acp_only || is_claude_like) {
        anyhow::bail!("--permission requires an ACP agent");
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
        serde_json::to_string(&serde_json::json!({
            "seq": entry.seq,
            "event": entry.event,
        }))
        .expect("serialize")
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
            project: None,
            model: None,
            permission: None,
            detach: false,
            resume: None,
            with_seq: false,
        }
    }

    #[test]
    fn validate_flags_accepts_acp_agents_without_optional_args() {
        for agent in ["claude", "gemini", "opencode", "codex"] {
            validate_flags(&args(agent)).unwrap_or_else(|e| panic!("{agent}: {e}"));
        }
    }

    #[test]
    fn validate_flags_rejects_model_on_non_claude() {
        for agent in ["gemini", "opencode", "codex"] {
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
