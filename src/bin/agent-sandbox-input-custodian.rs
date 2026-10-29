use anyhow::{Result, anyhow};
use clap::Parser;
use enrichedlab_agent_sandbox::broker::{
    config::migration_dsn,
    input_custodian::{CommandRenderer, InputCustodian, InputCustodianServiceConfig},
    input_kubernetes::InputKubernetesClient,
    store::Store,
};
use std::path::PathBuf;
use tokio_util::sync::CancellationToken;

#[derive(Parser)]
#[command(about = "Prepare one approved input request and publish its private admission")]
struct Args {
    #[arg(long)]
    config: PathBuf,
    #[arg(long)]
    config_owner_uid: u32,
    /// A filename inside the configured request and plan directories.
    #[arg(long)]
    request: String,
}

#[tokio::main]
async fn main() {
    let args = Args::parse();
    if let Err(error) = run(args).await {
        eprintln!(
            "{}",
            enrichedlab_agent_sandbox::broker::workload_site::safe_error_code(&error)
        );
        std::process::exit(1);
    }
}

async fn run(args: Args) -> Result<()> {
    let config = InputCustodianServiceConfig::load(&args.config, args.config_owner_uid)?;
    let custodian = InputCustodian::new(config.custodian)?;
    let renderer = CommandRenderer::new(config.renderer)?;
    let k = config.kubernetes;
    let transport = InputKubernetesClient::from_service_account(
        k.origin
            .parse()
            .map_err(|_| anyhow!("input_custodian_kubernetes_config_invalid"))?,
        k.namespace,
        k.token_file,
        &k.ca_file,
    )
    .map_err(|_| anyhow!("input_custodian_kubernetes_config_invalid"))?;
    let dsn = migration_dsn(
        &Some(config.database_dsn_file),
        "INPUT_CUSTODIAN_DATABASE_URL",
    )
    .map_err(|_| anyhow!("input_custodian_database_config_invalid"))?;
    let store = Store::connect(&dsn, config.database_tenant, 2)
        .map_err(|_| anyhow!("input_custodian_database_unavailable"))?;
    drop(dsn);
    let cancellation = CancellationToken::new();
    let interrupt = cancellation.clone();
    let signals = tokio::spawn(async move {
        #[cfg(unix)]
        {
            if let Ok(mut terminate) =
                tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
            {
                tokio::select! { _ = tokio::signal::ctrl_c() => {}, _ = terminate.recv() => {} }
            } else {
                let _ = tokio::signal::ctrl_c().await;
            }
        }
        #[cfg(not(unix))]
        {
            let _ = tokio::signal::ctrl_c().await;
        }
        interrupt.cancel();
    });
    let result = custodian
        .run(&args.request, &store, &transport, &renderer, &cancellation)
        .await;
    signals.abort();
    let receipt = result?;
    println!(
        "{}",
        serde_json::to_string(&receipt).map_err(|_| anyhow!("input_custodian_receipt_invalid"))?
    );
    Ok(())
}
