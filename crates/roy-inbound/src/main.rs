use clap::Parser;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "roy_inbound=info,warn".into()),
        )
        .init();
    let args = roy_inbound::cli::Args::parse();
    roy_inbound::cli::run(args).await
}
