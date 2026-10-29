use anyhow::Result;
use clap::Parser;
use enrichedlab_agent_sandbox::broker::{self, config::BrokerCli};

#[tokio::main]
async fn main() -> Result<()> {
    let _ = rustls::crypto::ring::default_provider().install_default();
    let command = BrokerCli::parse().command;
    let logs = tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "agent_sandbox_broker=info,tower_http=info".into()),
        )
        .with_target(false)
        .compact();
    // A command that writes JSON lines to standard output logs to standard error, so a log line never enters its results.
    if command.writes_json_lines() {
        logs.with_writer(std::io::stderr).init();
    } else {
        logs.init();
    }
    broker::log_panics_by_location();
    broker::run(command).await
}
