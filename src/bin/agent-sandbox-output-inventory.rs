use anyhow::{Result, anyhow};
use clap::Parser;
use enrichedlab_agent_sandbox::output_inventory::{OutputInventoryRequest, inventory};
use std::path::PathBuf;
use tokio_util::sync::CancellationToken;

#[derive(Parser)]
#[command(about = "Measure one operator-approved output selection")]
struct Args {
    /// Private descriptor root containing the closed request record.
    #[arg(long)]
    request_root: PathBuf,
    /// One private request filename beneath request_root.
    #[arg(long)]
    request: String,
    #[arg(long)]
    writer_uid: u32,
}

#[tokio::main]
async fn main() {
    if let Err(error) = run(Args::parse()).await {
        let code = error.to_string();
        if code.starts_with("output_inventory_")
            && code.bytes().all(|x| x.is_ascii_lowercase() || x == b'_')
        {
            eprintln!("{code}");
        } else {
            eprintln!("output_inventory_failed");
        }
        std::process::exit(1);
    }
}

async fn run(args: Args) -> Result<()> {
    let request = OutputInventoryRequest::load(&args.request_root, &args.request, args.writer_uid)?;
    let cancellation = CancellationToken::new();
    let signal = cancellation.clone();
    let signals = tokio::spawn(async move {
        #[cfg(unix)]
        {
            if let Ok(mut term) =
                tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
            {
                tokio::select! { _ = tokio::signal::ctrl_c() => {}, _ = term.recv() => {} }
            } else {
                let _ = tokio::signal::ctrl_c().await;
            }
        }
        #[cfg(not(unix))]
        {
            let _ = tokio::signal::ctrl_c().await;
        }
        signal.cancel();
    });
    let result = tokio::task::spawn_blocking(move || inventory(&request, &cancellation))
        .await
        .map_err(|_| anyhow!("output_inventory_worker_failed"))?;
    signals.abort();
    let output = result?;
    println!(
        "{}",
        serde_json::to_string(&output).map_err(|_| anyhow!("output_inventory_encode_failed"))?
    );
    Ok(())
}
