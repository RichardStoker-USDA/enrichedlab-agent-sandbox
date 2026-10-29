use anyhow::{Context, Result};
use clap::Parser;
use enrichedlab_agent_sandbox::custody::{CopySealRequest, copy_and_seal};

#[derive(Parser)]
#[command(
    name = "agent-sandbox-custodian",
    about = "Copy and attest sealed direct-Sandbox inputs"
)]
struct Args {
    #[arg(long)]
    request: std::path::PathBuf,
}

fn main() -> Result<()> {
    let args = Args::parse();
    let request: CopySealRequest =
        serde_json::from_slice(&std::fs::read(&args.request).context("request_read_failed")?)
            .context("request_decode_failed")?;
    let attestation = copy_and_seal(&request)?;
    println!(
        "{}",
        serde_json::to_string(&attestation).context("attestation_encode_failed")?
    );
    Ok(())
}
