use std::sync::Arc;

use roy::session::Session;
use roy::transport::{AcpConfig, AcpTransport, Transport};
use tokio_stream::StreamExt;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let transport: Arc<dyn Transport> = Arc::new(AcpTransport::new(AcpConfig::codex()));
    let mut session = Session::new(transport, std::env::current_dir()?);

    for prompt in ["reply with exactly: hello", "now reply with exactly: world"] {
        println!("\n>>> {prompt}");
        let mut stream = session.send(prompt).await?;
        while let Some(ev) = stream.next().await {
            println!("  {ev:?}");
        }
    }
    println!(
        "\nresume_cursor (ACP sessionId) = {:?}",
        session.resume_cursor()
    );
    session.close().await?;
    Ok(())
}
