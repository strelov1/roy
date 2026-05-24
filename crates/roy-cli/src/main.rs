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
    /// Working directory for a new session. Ignored with --resume.
    #[arg(long, conflicts_with = "resume")]
    cwd: Option<PathBuf>,
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
    let daemon = Arc::new(Daemon::new(journal_dir, Arc::new(DefaultTransportFactory))?);
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
            cwd: args.cwd.map(|p| p.to_string_lossy().into_owned()),
            model: args.model.clone(),
            permission: args.permission.clone(),
            resume: args.resume.clone(),
            tags: BTreeMap::default(),
        },
    )
    .await?;
    let (session, resume_cursor) = match read_event(&mut events).await? {
        ServerEvent::Spawned {
            session,
            resume_cursor,
            ..
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
            cwd: args.cwd.map(|p| p.to_string_lossy().into_owned()),
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
            cwd: None,
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
    fn fire_with_cwd_and_resume_rejected() {
        let cli = Cli::try_parse_from(["roy", "fire", "p", "--resume", "abc", "--cwd", "/tmp"]);
        assert!(
            cli.is_err(),
            "expected error: --cwd conflicts with --resume"
        );
    }
}
