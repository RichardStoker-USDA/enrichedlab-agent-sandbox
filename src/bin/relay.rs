use std::path::PathBuf;

use anyhow::Result;
use clap::Parser;
use enrichedlab_agent_sandbox::relay::{RelayConfig, serve};

#[derive(Debug, Parser)]
#[command(
    name = "agent-sandbox-relay",
    version,
    about = "Loopback-only TCP relay for OpenShell inference routes"
)]
struct Cli {
    #[arg(long)]
    config: PathBuf,
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();
    serve(RelayConfig::load(&cli.config)?).await
}
