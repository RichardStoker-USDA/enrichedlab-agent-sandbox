use anyhow::{Result, anyhow};
use clap::Parser;
use enrichedlab_agent_sandbox::broker::custody_dispatch::{
    CustodyDispatchConfig, CustodyDispatcher,
};
use std::path::PathBuf;
use tokio_util::sync::CancellationToken;

#[derive(Parser)]
#[command(about = "Dispatch private custody notices through fixed one-shot custodians")]
struct Args {
    #[arg(long)]
    config: PathBuf,
    #[arg(long)]
    config_owner_uid: u32,
}

#[tokio::main]
async fn main() {
    if let Err(error) = run(Args::parse()).await {
        let code = error.to_string();
        if code.starts_with("custody_dispatch_")
            && code.bytes().all(|v| v.is_ascii_lowercase() || v == b'_')
        {
            eprintln!("{code}");
        } else {
            eprintln!("custody_dispatch_failed_recovery_required");
        }
        std::process::exit(1);
    }
}
async fn run(args: Args) -> Result<()> {
    let config = CustodyDispatchConfig::load(&args.config, args.config_owner_uid)?;
    let dispatcher = CustodyDispatcher::new(config)?;
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
    let result = dispatcher.run(&cancellation).await;
    signals.abort();
    result.map_err(|_| anyhow!("custody_dispatch_failed_recovery_required"))
}
