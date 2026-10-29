use anyhow::Result;
use clap::Parser;
use enrichedlab_agent_sandbox::broker::export_transport::ExportServer;
use tokio_util::sync::CancellationToken;

#[derive(Parser)]
#[command(
    name = "agent-sandbox-exporter",
    about = "Serve one completed run's approved original files over mutual TLS"
)]
struct Args {
    #[arg(long)]
    config: std::path::PathBuf,
    #[arg(long)]
    writer_uid: u32,
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_target(false)
        .with_writer(std::io::stderr)
        .init();
    let args = Args::parse();
    let server = ExportServer::load(&args.config, args.writer_uid)?;
    let cancellation = CancellationToken::new();
    let shutdown = cancellation.clone();
    tokio::spawn(async move {
        #[cfg(unix)]
        {
            match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
                Ok(mut termination) => {
                    tokio::select! { _ = termination.recv() => {}, _ = tokio::signal::ctrl_c() => {} }
                }
                Err(_) => {
                    let _ = tokio::signal::ctrl_c().await;
                }
            }
        }
        #[cfg(not(unix))]
        {
            let _ = tokio::signal::ctrl_c().await;
        }
        shutdown.cancel();
    });
    server.serve(cancellation).await
}
