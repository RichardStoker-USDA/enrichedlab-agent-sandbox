//! The EnrichedLab upload guard: file type and structure checks, malware scanning and content
//! disarm before a file reaches a research service, with a per-person quarantine record.
//!
//! `enrichedlab-guard serve --config FILE` runs the service. `enrichedlab-guard check --policy FILE --use-case NAME
//! --rules DIR [--scanner HOST:PORT] [--write PATH] FILE` evaluates one local file and prints the decision as JSON;
//! without `--scanner` it skips the ClamAV layer and says so. `enrichedlab-guard worker TASK ...` is the bounded
//! process the service starts to parse each file (`worker.rs`).
pub mod api;
pub mod clamd;
pub mod config;
pub mod content;
pub mod dataset;
pub mod detect;
pub mod finding;
pub mod pdf;
pub mod pipeline;
pub mod policy;
pub mod raster;
pub mod rules;
pub mod store;
pub mod subprocess;
pub mod text;
pub mod worker;
pub mod zip;

#[cfg(test)]
pub mod fixtures;

use anyhow::{Context, Result, bail};
use std::os::unix::fs::DirBuilderExt;
use std::path::PathBuf;
use std::sync::{Arc, RwLock};
use std::time::{Duration, Instant};

fn flag(args: &[String], name: &str) -> Option<String> {
    args.iter()
        .position(|a| a == name)
        .and_then(|i| args.get(i + 1))
        .cloned()
}

/// Helper limits before an input is known; `pdf::Tools::for_input` sets the file-size limit for each input.
pub fn helper_limits(timeout: u64, memory_mib: u64) -> subprocess::Limits {
    subprocess::Limits {
        timeout: Duration::from_secs(timeout),
        memory_bytes: memory_mib * 1024 * 1024,
        output_bytes: 64 * 1024 * 1024,
        file_bytes: store::work_file_limit(0),
    }
}

/// This binary, which the service starts again as its parsing worker.
fn this_binary() -> Result<PathBuf> {
    std::env::current_exe().context("the guard's own path is unknown")
}

pub async fn serve(path: PathBuf) -> Result<()> {
    let config = config::Config::load(&path)?;
    let policy = policy::Policy::load(&config.policy_file)?;
    let callers = config::Callers::load(&config.callers_file)?;
    let rules = rules::Rules::load(&config.rules_dir)?;
    let tools = pdf::Tools {
        qpdf: config.qpdf.clone(),
        pdftotext: config.pdftotext.clone(),
        pdftoppm: config.pdftoppm.clone(),
        worker: this_binary()?,
        rules_dir: config.rules_dir.clone(),
        limits: helper_limits(config.helper_timeout_seconds, config.helper_memory_mib),
    };
    let qpdf_version = pdf::qpdf_version(&tools)
        .await
        .context("qpdf is not usable")?;
    let engines = pipeline::Engines::new(
        rules,
        tools,
        Some(config.scanner.clone()),
        Duration::from_secs(config.scanner_timeout_seconds),
        Duration::from_secs(config.signature_max_age_hours * 3600),
        qpdf_version,
    );
    let store = store::Store::open(&config.root, config.max_store_bytes)?;
    tracing::info!(target: "guard.start", policy = %policy.version, rules = %engines.rules.version,
        rule_count = engines.rules.count, qpdf = %engines.qpdf_version, use_cases = policy.use_cases.len(),
        store_used_bytes = store.used(), "guard starting");
    let app = Arc::new(api::App {
        engines,
        policy,
        callers_file: config.callers_file.clone(),
        callers: RwLock::new((Instant::now(), callers)),
        callers_reload: api::CALLERS_RELOAD,
        store,
        permits: tokio::sync::Semaphore::new(config.max_concurrent),
        queue_wait: Duration::from_secs(config.queue_wait_seconds),
        body_wait: api::BODY_WAIT,
        metrics: api::Metrics::default(),
    });
    let sweeper = Arc::clone(&app);
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(Duration::from_secs(3600));
        loop {
            interval.tick().await;
            let at = store::now();
            match tokio::task::block_in_place(|| sweeper.store.sweep(at)) {
                Ok(swept) => {
                    tracing::info!(target: "guard.sweep", originals = swept.originals, disarmed = swept.disarmed,
                    records = swept.records, exceptions = swept.exceptions, index_entries = swept.index_entries,
                    work = swept.work, store_used_bytes = sweeper.store.used(), "sweep")
                }
                Err(e) => tracing::error!(target: "guard.sweep", error = %e, "sweep failed"),
            }
        }
    });
    let main = tokio::net::TcpListener::bind(&config.listen)
        .await
        .context("listen address unavailable")?;
    let ops = tokio::net::TcpListener::bind(&config.ops_listen)
        .await
        .context("ops address unavailable")?;
    let ops_app = api::ops_router(Arc::clone(&app));
    let ops_task = tokio::spawn(async move { axum::serve(ops, ops_app).await });
    let shutdown = async {
        let mut term = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
            .expect("signal handler");
        tokio::select! { _ = term.recv() => {}, _ = tokio::signal::ctrl_c() => {} }
        tracing::info!(target: "guard.stop", "stopping after in-flight intakes");
    };
    axum::serve(main, api::router(app))
        .with_graceful_shutdown(shutdown)
        .await?;
    ops_task.abort();
    Ok(())
}

pub async fn check(args: &[String]) -> Result<()> {
    let file = args
        .last()
        .filter(|a| !a.starts_with("--"))
        .context("name the file to check")?;
    let policy = policy::Policy::load(&PathBuf::from(
        flag(args, "--policy").context("--policy is required")?,
    ))?;
    let use_case = flag(args, "--use-case").context("--use-case is required")?;
    let case = policy
        .use_case(&use_case)
        .context("unknown use case")?
        .clone();
    let rules_dir = PathBuf::from(flag(args, "--rules").context("--rules is required")?);
    let rules = rules::Rules::load(&rules_dir)?;
    let tools = pdf::Tools {
        qpdf: flag(args, "--qpdf").unwrap_or_else(|| "qpdf".into()),
        pdftotext: flag(args, "--pdftotext").unwrap_or_else(|| "pdftotext".into()),
        pdftoppm: flag(args, "--pdftoppm").unwrap_or_else(|| "pdftoppm".into()),
        worker: this_binary()?,
        rules_dir,
        limits: helper_limits(120, 2048),
    };
    let qpdf_version = pdf::qpdf_version(&tools).await?;
    let scanner = flag(args, "--scanner");
    let engines = pipeline::Engines::new(
        rules,
        tools,
        scanner.clone(),
        Duration::from_secs(60),
        Duration::from_secs(72 * 3600),
        qpdf_version,
    );
    let bytes = Arc::new(std::fs::read(file).context("file unreadable")?);
    let declared = match flag(args, "--declared") {
        Some(media) => {
            detect::FileType::from_media_type(&media).context("declared type not accepted")?
        }
        None => detect::detect(&bytes).0,
    };
    let work =
        std::env::temp_dir().join(format!("enrichedlab-guard-check-{}", uuid::Uuid::now_v7()));
    std::fs::DirBuilder::new().mode(0o700).create(&work)?;
    let result =
        pipeline::evaluate(&engines, &case, declared, Arc::clone(&bytes), &work, None).await;
    let _ = std::fs::remove_dir_all(&work);
    let evaluation = match result? {
        Ok(e) => e,
        Err(unavailable) => bail!("scanner unavailable: {}", unavailable.0),
    };
    if let (Some(path), Some(copy)) = (flag(args, "--write"), &evaluation.released) {
        store::write_private(&PathBuf::from(path), copy)?;
    }
    let out = serde_json::json!({
        "outcome": evaluation.outcome,
        "detected_type": evaluation.detected,
        "bytes": bytes.len(),
        "sha256": store::sha256(&bytes),
        "findings": evaluation.findings,
        "released_sha256": evaluation.released.as_ref().map(|c| store::sha256(c)),
        "released_bytes": evaluation.released.as_ref().map(|c| c.len()),
        "versions": evaluation.versions,
        "clamav_layer": if scanner.is_some() { "scanned" } else { "not used in this local check" },
    });
    println!("{}", serde_json::to_string(&out)?);
    Ok(())
}
