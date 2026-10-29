use anyhow::Result;
use clap::Parser;
use enrichedlab_agent_sandbox::broker::client::{RemoteCli, run};

#[tokio::main]
async fn main() -> Result<()> {
    let _ = rustls::crypto::ring::default_provider().install_default();
    run(RemoteCli::parse()).await
}
