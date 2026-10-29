use anyhow::{Result, anyhow};
use clap::Parser;
use enrichedlab_agent_sandbox::broker::{
    config::migration_dsn,
    export_kubernetes::ExportKubernetesClient,
    output_custodian::{OutputCustodian, OutputCustodianServiceConfig},
    output_kubernetes::OutputInventoryKubernetesClient,
    sandbox::{KubernetesHttpTransport, SandboxBindingCatalog, SandboxRuntime},
    store::Store,
};
use std::path::PathBuf;
use tokio_util::sync::CancellationToken;

#[derive(Parser)]
#[command(about = "Publish verified original outputs for one completed sandbox")]
struct Args {
    #[arg(long)]
    config: PathBuf,
    #[arg(long)]
    config_owner_uid: u32,
    /// Filename in the configured private request and approved plan roots.
    #[arg(long)]
    request: String,
}
#[tokio::main]
async fn main() {
    if let Err(error) = run(Args::parse()).await {
        eprintln!(
            "{}",
            enrichedlab_agent_sandbox::broker::workload_site::safe_error_code(&error)
        );
        std::process::exit(1);
    }
}
async fn run(args: Args) -> Result<()> {
    let config = OutputCustodianServiceConfig::load(&args.config, args.config_owner_uid)?;
    let lease = config.custodian.lease_seconds;
    let custodian = OutputCustodian::new(config.custodian)?;
    let k = config.kubernetes;
    let origin: reqwest::Url = k
        .origin
        .parse()
        .map_err(|_| anyhow!("output_custodian_kubernetes_config_invalid"))?;
    let inventory = OutputInventoryKubernetesClient::from_service_account(
        origin.clone(),
        k.namespace.clone(),
        k.token_file.clone(),
        &k.ca_file,
    )
    .map_err(|_| anyhow!("output_custodian_kubernetes_config_invalid"))?;
    let exporter = ExportKubernetesClient::from_service_account(
        origin.clone(),
        k.namespace.clone(),
        k.token_file.clone(),
        &k.ca_file,
    )
    .map_err(|_| anyhow!("output_custodian_kubernetes_config_invalid"))?;
    let runtime = SandboxRuntime::new(
        KubernetesHttpTransport::from_service_account(
            origin,
            k.namespace,
            k.token_file,
            &k.ca_file,
        )
        .map_err(|_| anyhow!("output_custodian_kubernetes_config_invalid"))?,
        SandboxBindingCatalog::new(config.bindings)?,
        lease,
    )?;
    let dsn = migration_dsn(
        &Some(config.database_dsn_file),
        "OUTPUT_CUSTODIAN_DATABASE_URL",
    )
    .map_err(|_| anyhow!("output_custodian_database_config_invalid"))?;
    let store = Store::connect(&dsn, config.database_tenant, 2)
        .map_err(|_| anyhow!("output_custodian_database_unavailable"))?;
    drop(dsn);
    let cancellation = CancellationToken::new();
    let interrupt = cancellation.clone();
    let signals = tokio::spawn(async move {
        #[cfg(unix)]
        {
            if let Ok(mut terminate) =
                tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
            {
                tokio::select! { _=tokio::signal::ctrl_c()=>{}, _=terminate.recv()=>{} }
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
        .run(
            &args.request,
            &store,
            &runtime,
            inventory,
            &exporter,
            &cancellation,
        )
        .await;
    signals.abort();
    let receipt = result?;
    println!(
        "{}",
        serde_json::to_string(&receipt).map_err(|_| anyhow!("output_custodian_receipt_invalid"))?
    );
    Ok(())
}
