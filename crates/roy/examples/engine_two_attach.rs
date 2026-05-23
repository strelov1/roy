//! Acceptance demo: one live session, two concurrent observers, one writer.
//!
//! Spawns an opencode-ACP session via `SessionManager`, attaches two observers
//! that print every event they see with `A:` / `B:` prefixes, and from a third
//! task acquires the input lease and sends two prompts. Both observers must
//! receive identical `seq` streams.
//!
//! Run with: cargo run --example engine_two_attach
//! Requires `opencode` on PATH.

use std::sync::Arc;

use futures::StreamExt;
use roy::transport::{AcpConfig, AcpTransport, Transport};
use roy::{Attach, SessionManager, TurnEvent};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let journal_dir = std::env::temp_dir().join("roy-demo-journals");
    let manager = SessionManager::new(journal_dir.clone());
    let transport: Arc<dyn Transport> = Arc::new(AcpTransport::new(AcpConfig::opencode()));
    let cwd = std::env::current_dir()?;

    let engine = manager.spawn(transport, cwd, 256, 1024).await?;
    let session_id = engine.id().to_string();
    eprintln!(
        "spawned session {session_id}; journal -> {}",
        journal_dir.display()
    );

    let attach_a = engine.attach(None).await?;
    let attach_b = engine.attach(None).await?;

    let h_a = tokio::spawn(print_stream("A", attach_a));
    let h_b = tokio::spawn(print_stream("B", attach_b));

    let lease = engine
        .try_acquire_input()
        .ok_or_else(|| anyhow::anyhow!("input lease already held"))?;
    eprintln!("\n>>> turn 1");
    lease.send("reply with exactly: hello")?;
    eprintln!(">>> turn 2");
    lease.send("now reply with exactly: world")?;

    let (a_count, b_count) = tokio::try_join!(h_a, h_b)?;
    eprintln!("\nA saw {a_count} entries, B saw {b_count} (must match)");

    drop(lease);
    manager.close(&session_id).await?;
    Ok(())
}

async fn print_stream(label: &'static str, attach: Attach) -> usize {
    let mut stream = attach.stream;
    let mut count = 0usize;
    let mut turns_seen = 0;
    while let Some(entry) = stream.next().await {
        count += 1;
        match &entry.event {
            TurnEvent::AssistantText { text } => {
                println!("{label}: [{}] text  {text}", entry.seq);
            }
            TurnEvent::Result {
                stop_reason,
                cost_usd,
            } => {
                println!(
                    "{label}: [{}] result stop={} cost={:?}",
                    entry.seq,
                    stop_reason.as_wire(),
                    cost_usd
                );
                turns_seen += 1;
                if turns_seen == 2 {
                    break;
                }
            }
            other => println!("{label}: [{}] {other:?}", entry.seq),
        }
    }
    count
}
