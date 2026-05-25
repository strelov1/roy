use anyhow::Result;
use clap::Parser;
use tracing_subscriber::EnvFilter;

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| EnvFilter::new("roy_gateway=info,warn")),
        )
        .with_writer(std::io::stderr)
        .init();

    let args = roy_gateway::Args::parse();
    roy_gateway::run(args).await
}
