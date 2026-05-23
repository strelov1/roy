//! `roy` CLI: a thin trigger over the `roy serve` daemon.
//!
//! Subcommands defined per `docs/wire-protocol.md`.

use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::Arc;

use anyhow::{anyhow, Context};
use clap::{Parser, Subcommand};
use roy::{
    daemon::{Daemon, DefaultTransportFactory},
    ClientCommand, JournalEntry, ServeOpts, ServerEvent, TurnEvent,
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
    /// Run an MCP server (stdio JSON-RPC) that exposes roy daemon operations
    /// as MCP tools. Spawn this from an MCP-aware client (Claude Desktop,
    /// IDE plugin) which talks to it over stdio.
    Mcp(McpArgs),
}

#[derive(clap::Args)]
struct ServeArgs {
    #[arg(long)]
    socket: Option<PathBuf>,
    #[arg(long)]
    journal_dir: Option<PathBuf>,
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
struct McpArgs {
    /// Override the daemon socket the MCP tools connect to.
    #[arg(long)]
    socket: Option<PathBuf>,
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
        Cmd::Mcp(args) => {
            let socket = args.socket.unwrap_or_else(default_socket);
            mcp::run(socket).await.map(|()| ExitCode::SUCCESS)
        }
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

async fn cmd_serve(args: ServeArgs) -> anyhow::Result<()> {
    let socket = args.socket.unwrap_or_else(default_socket);
    let journal_dir = args.journal_dir.unwrap_or_else(default_journal_dir);
    let daemon = Arc::new(Daemon::new(journal_dir, Arc::new(DefaultTransportFactory)));
    eprintln!("roy serve: listening on {}", socket.display());
    if let Some(port) = args.port {
        eprintln!("roy serve: WebSocket on 127.0.0.1:{port}");
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
            cwd: args.cwd.map(|p| p.to_string_lossy().into_owned()),
            model: args.model.clone(),
            permission: args.permission.clone(),
            resume: args.resume.clone(),
        },
    )
    .await?;
    let (session, resume_cursor) = match read_event(&mut events).await? {
        ServerEvent::Spawned {
            session,
            resume_cursor,
        } => {
            if args.detach {
                let payload = serde_json::json!({
                    "type": "session",
                    "id": session,
                    "resume_cursor": resume_cursor,
                });
                println!("{payload}");
                return Ok(ExitCode::SUCCESS);
            }
            (session, resume_cursor)
        }
        ServerEvent::Error { code, message, .. } => {
            anyhow::bail!("spawn failed: {code}: {message}");
        }
        other => anyhow::bail!("unexpected response to Spawn: {other:?}"),
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
                println!("{s}");
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
        },
    )
    .await?;
    match read_event(&mut events).await? {
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
            Ok(())
        }
        ServerEvent::Error { code, message, .. } => {
            anyhow::bail!("resume failed: {code}: {message}")
        }
        other => anyhow::bail!("unexpected response to Resume: {other:?}"),
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
