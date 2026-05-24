//! `roy-scheduler` binary — CLI entry. Filled in by later tasks.

use anyhow::Result;

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("roy_scheduler=info,warn")),
        )
        .with_writer(std::io::stderr)
        .init();

    eprintln!("roy-scheduler: stub — CLI lands in Task 17");
    Ok(())
}
