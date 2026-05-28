use roy::transport::{AcpConfig, AcpTransport, Transport};
use tokio_stream::StreamExt;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let transport = AcpTransport::new(AcpConfig::claude());
    let mut handle = transport
        .open(
            "demo",
            None,
            std::env::current_dir()?,
            None,
            &Default::default(),
        )
        .await?;

    for prompt in ["reply with exactly: hello", "now reply with exactly: world"] {
        println!("\n>>> {prompt}");
        let (mut stream, _cancel) = handle.send(prompt).await?;
        while let Some(ev) = stream.next().await {
            println!("  {ev:?}");
        }
    }
    println!(
        "\nresume_cursor (ACP sessionId) = {:?}",
        handle.resume_cursor()
    );
    handle.close().await?;
    Ok(())
}
