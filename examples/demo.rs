use std::sync::Arc;

use roy::provider::{ClaudeProvider, Provider};
use roy::session::Session;
use roy::transport::{PrintTransport, Transport};
use tokio_stream::StreamExt;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let provider: Arc<dyn Provider> = Arc::new(ClaudeProvider::new(Some(
        "claude-haiku-4-5-20251001".to_string(),
    )));
    let transport: Arc<dyn Transport> = Arc::new(PrintTransport::new());
    let mut session = Session::new(provider, transport, std::env::current_dir()?);

    for prompt in ["reply with exactly: hello", "now reply with exactly: world"] {
        println!("\n>>> {prompt}");
        let mut stream = session.send(prompt).await?;
        while let Some(ev) = stream.next().await {
            println!("  {ev:?}");
        }
    }
    session.close().await?;
    Ok(())
}
