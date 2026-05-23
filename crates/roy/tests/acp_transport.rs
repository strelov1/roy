use std::sync::Arc;
use std::time::Duration;

use futures::StreamExt;
use roy::error::RoyError;
use roy::event::{StopReason, TurnEvent};
use roy::session::Session;
use roy::transport::{AcpConfig, AcpTransport, PermissionPolicy, Transport};

fn fake_config(extra: &[&str]) -> AcpConfig {
    let mut args = vec!["tests/scripts/fake-acp-agent.py".to_string()];
    args.extend(extra.iter().map(|s| s.to_string()));
    AcpConfig {
        command: "python3".to_string(),
        args,
        mode_id: Some("yolo".to_string()),
        permission_policy: PermissionPolicy::AllowAll,
        open_timeout: Duration::from_secs(5),
    }
}

fn fake_config_with_timeout(extra: &[&str], open_timeout: Duration) -> AcpConfig {
    let mut config = fake_config(extra);
    config.open_timeout = open_timeout;
    config
}

#[tokio::test]
async fn open_send_streams_until_result() {
    let transport = AcpTransport::new(fake_config(&[]));
    let mut handle = transport
        .open("ignored", None, std::env::current_dir().unwrap())
        .await
        .unwrap();

    let mut events = Vec::new();
    {
        let mut stream = handle.send("hello").await.unwrap();
        while let Some(ev) = stream.next().await {
            events.push(ev);
        }
    }
    assert!(events
        .iter()
        .any(|e| matches!(e, TurnEvent::AssistantText { text } if text == "ack")));
    assert!(matches!(
        events.last(),
        Some(TurnEvent::Result {
            stop_reason: StopReason::EndTurn,
            ..
        })
    ));

    // ACP sessionId is exposed as the resume cursor.
    assert_eq!(handle.resume_cursor(), Some("fake-acp-sid".to_string()));

    // Multi-turn on the same live process.
    let mut events2 = Vec::new();
    {
        let mut stream = handle.send("again").await.unwrap();
        while let Some(ev) = stream.next().await {
            events2.push(ev);
        }
    }
    assert!(matches!(events2.last(), Some(TurnEvent::Result { .. })));

    handle.close().await.unwrap();
}

#[tokio::test]
async fn auto_allows_permission_requests() {
    let transport = AcpTransport::new(fake_config(&["--permission"]));
    let mut handle = transport
        .open("ignored", None, std::env::current_dir().unwrap())
        .await
        .unwrap();

    // The fake agent only completes the turn after the client auto-allows the
    // permission request; reaching Result proves the auto-allow happened.
    let mut events = Vec::new();
    {
        let mut stream = handle.send("do a thing").await.unwrap();
        while let Some(ev) = stream.next().await {
            events.push(ev);
        }
    }
    assert!(matches!(events.last(), Some(TurnEvent::Result { .. })));
    handle.close().await.unwrap();
}

#[tokio::test]
async fn session_via_transport_records_acp_cursor() {
    let transport: Arc<dyn Transport> = Arc::new(AcpTransport::new(fake_config(&[])));
    let mut session = Session::new(transport, std::env::current_dir().unwrap());

    {
        let mut stream = session.send("hi").await.unwrap();
        while stream.next().await.is_some() {}
    }
    // Session stored the ACP sessionId (NOT its own uuid) as the resume cursor.
    assert_eq!(session.resume_cursor(), Some("fake-acp-sid"));
    session.close().await.unwrap();
}

#[tokio::test]
async fn open_without_mode_skips_set_mode() {
    // OpenCode has no ACP modes, so AcpConfig sets mode_id = None and open()
    // must skip session/set_mode. The fake agent never receives set_mode here.
    let config = AcpConfig {
        command: "python3".to_string(),
        args: vec!["tests/scripts/fake-acp-agent.py".to_string()],
        mode_id: None,
        permission_policy: PermissionPolicy::Deny,
        open_timeout: Duration::from_secs(5),
    };
    let transport = AcpTransport::new(config);
    let mut handle = transport
        .open("ignored", None, std::env::current_dir().unwrap())
        .await
        .unwrap();

    let mut events = Vec::new();
    {
        let mut stream = handle.send("hello").await.unwrap();
        while let Some(ev) = stream.next().await {
            events.push(ev);
        }
    }
    assert!(matches!(events.last(), Some(TurnEvent::Result { .. })));
    handle.close().await.unwrap();
}

#[tokio::test]
async fn open_surfaces_error_when_agent_dies_during_initialize() {
    let transport = AcpTransport::new(fake_config_with_timeout(
        &["--exit-on-initialize"],
        Duration::from_secs(5),
    ));

    let err = match transport
        .open("ignored", None, std::env::current_dir().unwrap())
        .await
    {
        Ok(_) => panic!("open should fail when the agent exits during initialize"),
        Err(err) => err,
    };

    // The SDK surfaces a dead agent as a connection/protocol error.
    assert!(matches!(
        err,
        RoyError::Protocol(_) | RoyError::ProcessExited
    ));
}

#[tokio::test]
async fn open_times_out_when_agent_never_replies() {
    let timeout = Duration::from_millis(50);
    let transport = AcpTransport::new(fake_config_with_timeout(
        &["--no-initialize-reply"],
        timeout,
    ));

    let err = match transport
        .open("ignored", None, std::env::current_dir().unwrap())
        .await
    {
        Ok(_) => panic!("open should fail when initialize times out"),
        Err(err) => err,
    };

    assert!(matches!(err, RoyError::Timeout(d) if d == timeout));
}

#[tokio::test]
async fn open_surfaces_json_rpc_errors() {
    let transport = AcpTransport::new(fake_config_with_timeout(
        &["--jsonrpc-error"],
        Duration::from_secs(5),
    ));

    let err = match transport
        .open("ignored", None, std::env::current_dir().unwrap())
        .await
    {
        Ok(_) => panic!("open should surface protocol errors"),
        Err(err) => err,
    };

    assert!(matches!(err, RoyError::Protocol(message) if message.contains("auth required")));
}

#[tokio::test]
async fn mid_turn_exit_emits_error_result() {
    let transport = AcpTransport::new(fake_config(&["--exit-mid-turn"]));
    let mut handle = transport
        .open("ignored", None, std::env::current_dir().unwrap())
        .await
        .unwrap();

    let mut events = Vec::new();
    {
        let mut stream = handle.send("hello").await.unwrap();
        while let Some(ev) = stream.next().await {
            events.push(ev);
        }
    }

    assert!(events
        .iter()
        .any(|e| matches!(e, TurnEvent::AssistantText { text } if text == "partial")));
    assert!(matches!(
        events.last(),
        Some(TurnEvent::Result {
            stop_reason: StopReason::Error,
            ..
        })
    ));
}

#[tokio::test]
async fn dropping_a_turn_cancels_it_and_the_next_turn_proceeds() {
    // The fake `--cancellable` agent only finishes a turn after it receives
    // session/cancel. So turn 2 can only make progress if dropping turn 1's
    // stream actually cancelled it and freed the actor.
    let transport = AcpTransport::new(fake_config(&["--cancellable"]));
    let mut handle = transport
        .open("ignored", None, std::env::current_dir().unwrap())
        .await
        .unwrap();

    {
        let mut stream = handle.send("one").await.unwrap();
        let first = stream.next().await;
        assert!(matches!(first, Some(TurnEvent::AssistantText { text }) if text == "working"));
        // Drop the stream -> cancel turn 1.
    }

    let first2 = tokio::time::timeout(Duration::from_secs(5), async {
        let mut stream = handle.send("two").await.unwrap();
        stream.next().await
    })
    .await
    .expect("turn 2 must not hang; turn 1 should have been cancelled");

    assert!(matches!(first2, Some(TurnEvent::AssistantText { text }) if text == "working"));
    handle.close().await.unwrap();
}

// Real gemini. Ignored by default: needs the `gemini` binary, logged in.
// Run with: cargo test --test acp_transport -- --ignored real_gemini
#[tokio::test]
#[ignore]
async fn real_gemini_spawn_and_turn() {
    if which_gemini().is_none() {
        eprintln!("skipping: gemini not on PATH");
        return;
    }
    let transport: Arc<dyn Transport> = Arc::new(AcpTransport::new(AcpConfig::gemini()));
    let mut session = Session::new(transport, std::env::current_dir().unwrap());

    let mut answer = String::new();
    {
        let mut stream = session
            .send("reply with exactly the word: hello")
            .await
            .unwrap();
        while let Some(ev) = stream.next().await {
            if let TurnEvent::AssistantText { text } = ev {
                answer.push_str(&text);
            }
        }
    }
    assert!(answer.to_lowercase().contains("hello"), "got: {answer:?}");
    assert!(session.resume_cursor().is_some());
    session.close().await.unwrap();
}

fn which_gemini() -> Option<()> {
    which("gemini")
}

// Real opencode. Ignored by default: needs the `opencode` binary, logged in.
// Run with: cargo test --test acp_transport -- --ignored real_opencode
#[tokio::test]
#[ignore]
async fn real_opencode_spawn_and_turn() {
    if which("opencode").is_none() {
        eprintln!("skipping: opencode not on PATH");
        return;
    }
    let transport: Arc<dyn Transport> = Arc::new(AcpTransport::new(AcpConfig::opencode()));
    let mut session = Session::new(transport, std::env::current_dir().unwrap());

    let mut answer = String::new();
    {
        let mut stream = session
            .send("reply with exactly the word: hello")
            .await
            .unwrap();
        while let Some(ev) = stream.next().await {
            if let TurnEvent::AssistantText { text } = ev {
                answer.push_str(&text);
            }
        }
    }
    assert!(answer.to_lowercase().contains("hello"), "got: {answer:?}");
    assert!(session.resume_cursor().is_some());
    session.close().await.unwrap();
}

// Real codex via the codex-acp adapter. Ignored by default: needs the
// `codex-acp` binary on PATH (npm i -g @zed-industries/codex-acp) and a usable
// codex auth/quota. Run with: cargo test --test acp_transport -- --ignored real_codex
#[tokio::test]
#[ignore]
async fn real_codex_spawn_and_turn() {
    if which("codex-acp").is_none() {
        eprintln!("skipping: codex-acp not on PATH");
        return;
    }
    let transport: Arc<dyn Transport> = Arc::new(AcpTransport::new(AcpConfig::codex()));
    let mut session = Session::new(transport, std::env::current_dir().unwrap());

    let mut answer = String::new();
    {
        let mut stream = session
            .send("reply with exactly the word: hello")
            .await
            .unwrap();
        while let Some(ev) = stream.next().await {
            if let TurnEvent::AssistantText { text } = ev {
                answer.push_str(&text);
            }
        }
    }
    assert!(answer.to_lowercase().contains("hello"), "got: {answer:?}");
    assert!(session.resume_cursor().is_some());
    session.close().await.unwrap();
}

// Real Claude Agent via the claude-code-acp adapter. Ignored by default: needs
// the `claude-code-acp` binary on PATH (npm i -g @zed-industries/claude-code-acp)
// and API auth (CLAUDE_CODE_OAUTH_TOKEN or ANTHROPIC_API_KEY) in the env.
// Run with: cargo test --test acp_transport -- --ignored real_claude_agent
#[tokio::test]
#[ignore]
async fn real_claude_agent_spawn_and_turn() {
    if which("claude-code-acp").is_none() {
        eprintln!("skipping: claude-code-acp not on PATH");
        return;
    }
    let transport: Arc<dyn Transport> = Arc::new(AcpTransport::new(AcpConfig::claude_agent()));
    let mut session = Session::new(transport, std::env::current_dir().unwrap());

    let mut answer = String::new();
    {
        let mut stream = session
            .send("reply with exactly the word: hello")
            .await
            .unwrap();
        while let Some(ev) = stream.next().await {
            if let TurnEvent::AssistantText { text } = ev {
                answer.push_str(&text);
            }
        }
    }
    assert!(answer.to_lowercase().contains("hello"), "got: {answer:?}");
    assert!(session.resume_cursor().is_some());
    session.close().await.unwrap();
}

fn which(bin: &str) -> Option<()> {
    // `--help` rather than `--version`: codex-acp has no --version flag.
    std::process::Command::new(bin)
        .arg("--help")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .ok()
        .filter(|s| s.success())
        .map(|_| ())
}
