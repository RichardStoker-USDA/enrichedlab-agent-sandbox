use anyhow::Result;
use clap::Parser;
use enrichedlab_agent_sandbox::{app, cli::Cli};

fn main() -> Result<()> {
    app::run(Cli::parse())
}
