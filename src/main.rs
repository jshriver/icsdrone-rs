mod app;
mod board;
mod book;
mod config;
mod engine;
mod ics;

use anyhow::Result;
use clap::Parser;
use tracing_subscriber::EnvFilter;

use app::App;
use config::Config;

#[tokio::main]
async fn main() -> Result<()> {
    let config = Config::parse();

    // Logs go to stderr so they don't interleave with the interactive
    // `>` prompt / its output on stdout.
    tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| config.log_level.clone().into()),
        )
        .init();

    let mut app = App::connect_and_login(&config).await?;

    let result = app.run().await;
    if let Err(e) = &result {
        tracing::error!("fatal error: {e:#}");
    }
    app.shutdown().await?;
    result
}
