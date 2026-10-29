//! The worker's document route (migration 072). An answer document runs here, not in a sandbox:
//! no model is called and no model-written code runs. The worker reads every table and figure its request pinned from
//! the retained packages (each package hashed and read once, all of them within one budget of stored bytes hashed,
//! expanded bytes read through, and member bytes, rows and cells kept), checks their digests, composes the request body
//! by code (`document_compose`), writes the file with the broker's own writer (`document_write`), and writes a package
//! with the file, the body, the writer's receipt, each answer table as CSV, a spec naming every value's cell, a README
//! and `SHA256SUMS`. The reading and the writing run off the async threads while the lease is renewed and
//! cancellation watched; when either stops the task, the work stops at its next read. One transaction then registers
//! the package, records the completed receipt and finishes the task (`broker_complete_document`), so a cancellation
//! lands before all of it or after all of it. A refusal or failure records its receipt with the broker's own words for
//! its code.
use std::{
    collections::{BTreeMap, HashMap},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};

use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use time::OffsetDateTime;
use tokio::time::{MissedTickBehavior, interval};
use tokio_util::sync::CancellationToken;

use super::{
    config::WorkerConfig,
    digest,
    document_compose::{self, Composed, Inputs},
    document_write::{self, WriteError},
    figure_run::{Budget, MemberError, Stop, failed, read_members, refused, write_private},
    model::ClaimedTask,
    store::{DocumentReceipt, RetainedPackage, Store, StoreErrorKind, TaskDocument},
    tables,
};

/// The profile route that selects this path.
pub const ROUTE: &str = "server-document";
const MAX_FIGURE_BYTES: u64 = 8 * 1024 * 1024;
/// The result tables one document reads in all, as stored bytes, parsed rows and parsed cells. A table may hold
/// `tables::MAX_TABLE_BYTES` and a request pins up to 32, so the worker bounds the sums, which keeps a document's
/// parsed tables near 100 MB of the worker's memory whatever their shape.
const MAX_SOURCE_BYTES: u64 = 48 * 1024 * 1024;
const MAX_SOURCE_ROWS: usize = 2_000_000;
const MAX_SOURCE_CELLS: usize = 4_000_000;
/// The stored package bytes one document hashes, and the expanded bytes it reads through to reach its members, in all.
/// A package may expand to `MAX_PACKAGE_EXPANDED_BYTES` and a request pins up to 32, so without these one document
/// could hash and expand tens of GiB on the worker's one execution slot.
const MAX_PACKAGES_BYTES: u64 = 1 << 30;
const MAX_SCAN_BYTES: u64 = 1 << 30;

/// A written document: the receipt fields and the package retained under the attempt's folder, not yet registered.
struct Done {
    file: Value,
    counts: Value,
    notes: Value,
    retained: RetainedPackage,
}

fn media_type(kind: &str) -> &'static str {
    match kind {
        "xlsx" => "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet",
        "docx" => "application/vnd.openxmlformats-officedocument.wordprocessingml.document",
        _ => "application/vnd.openxmlformats-officedocument.presentationml.presentation",
    }
}

/// Run one claimed document to a finished state, or leave it to lease recovery when the lease or the worker stops.
pub async fn execute(
    store: Store,
    config: &WorkerConfig,
    task: ClaimedTask,
    shutdown: CancellationToken,
) -> anyhow::Result<()> {
    let worker = config.worker_id.as_str();
    let generation = task.lease_generation;
    if !store.mark_running(task.id, worker, generation).await? {
        return Ok(());
    }
    let Some(request) = store
        .task_document(task.id, worker, task.attempt, generation)
        .await?
    else {
        store
            .finish_task(
                task.id,
                worker,
                generation,
                false,
                "document_not_written",
                "document_not_recorded",
            )
            .await?;
        return Ok(());
    };
    let started = OffsetDateTime::now_utc();
    let outcome = tokio::select! {
        outcome = run(&store, config, &task, &request) => outcome,
        () = shutdown.cancelled() => return Ok(()),
    };
    let finished = OffsetDateTime::now_utc();
    let stop = match outcome {
        Ok(done) => {
            let receipt = DocumentReceipt {
                status: "completed",
                error_code: None,
                message: None,
                service_version: Some(document_write::writer_version()),
                image_digest: None,
                file: Some(done.file),
                counts: Some(done.counts),
                notes: done.notes,
                package_sha256: Some(done.retained.package_sha256.clone()),
                started_at: started,
                finished_at: finished,
            };
            match store
                .complete_document(
                    task.id,
                    worker,
                    task.attempt,
                    generation,
                    &receipt,
                    &done.retained,
                )
                .await
            {
                Ok(()) => {
                    tracing::info!(task_id=%task.id, lease_generation=generation, event="document_finished",
                        status="completed", code="");
                    return Ok(());
                }
                // Cancelled or no longer this worker's: whoever holds the task now finishes it. The completing
                // transaction refused, so nothing names the package written under this attempt's folder, and it is
                // removed. Any other error leaves it: the transaction may have committed.
                Err(e) if e.kind == StoreErrorKind::LeaseLost => {
                    tracing::warn!(task_id=%task.id, lease_generation=generation, event="document_not_completed",
                        code=e.code);
                    discard(config.artifact_root.clone(), &done.retained).await;
                    return Ok(());
                }
                Err(e) => {
                    tracing::warn!(task_id=%task.id, lease_generation=generation, event="document_not_completed",
                        code=e.code);
                    failed("document_package_not_registered")
                }
            }
        }
        Err(stop) => stop,
    };
    let receipt = DocumentReceipt {
        status: stop.status,
        error_code: Some(stop.code().to_owned()),
        message: Some(message(stop.code()).to_owned()),
        service_version: None,
        image_digest: None,
        file: None,
        counts: None,
        notes: json!([]),
        package_sha256: None,
        started_at: started,
        finished_at: finished,
    };
    store
        .record_document_receipt(task.id, worker, task.attempt, generation, &receipt)
        .await?;
    store
        .finish_task(
            task.id,
            worker,
            generation,
            false,
            "document_not_written",
            stop.code(),
        )
        .await?;
    tracing::info!(task_id=%task.id, lease_generation=generation, event="document_finished", status=receipt.status,
        code=stop.code());
    Ok(())
}

/// The broker's words for each way a document stops. They name what to do, never what the request held.
pub fn message(code: &str) -> &'static str {
    match code {
        "document_source_changed" => {
            "A result this document cites changed after it was asked for. Ask again."
        }
        "document_source_unreadable" | "document_table_missing" => {
            "A result this document cites cannot be read from its stored package."
        }
        "document_sources_too_large" => {
            "This answer cites more result data than one document reads; try fewer or smaller tables."
        }
        "document_value_missing" => "A value the answer cites is not in its stored result table.",
        "document_record_invalid" => {
            "The decision this document restates is not in the expected form."
        }
        "document_too_large" => {
            "The answer holds more than this kind of document can; try a workbook."
        }
        "document_too_large_for_slides" => {
            "A slide could not hold its content; try a Word document or a workbook."
        }
        _ => "The document could not be written. Try again later.",
    }
}

fn member_stop(error: MemberError) -> Stop {
    match error {
        MemberError::Unreadable { refused: true } => refused("document_source_unreadable"),
        MemberError::Unreadable { refused: false } => failed("document_source_unreadable"),
        MemberError::Changed => refused("document_source_changed"),
        MemberError::TooLarge => refused("document_sources_too_large"),
        MemberError::Missing => refused("document_table_missing"),
    }
}

fn write_stop(error: WriteError) -> Stop {
    match error.0 {
        "document_too_large" | "figure_too_large" => refused("document_too_large"),
        "table_too_large_for_slide" | "slide_content_invalid" => {
            refused("document_too_large_for_slides")
        }
        // Compose writes within the writer's limits and checks each figure, so any other refusal is the broker's fault.
        _ => failed("document_write_failed"),
    }
}

/// Every pinned table parsed and every pinned figure, after checking each package's and member's digest.
type Read = (HashMap<(String, String), tables::Table>, Vec<Vec<u8>>);

fn read_sources(
    root: &std::path::Path,
    request: &TaskDocument,
    stop: &AtomicBool,
) -> Result<Read, Stop> {
    // Each package is hashed and read once, for every member the request pins in it.
    type Package = (String, i64, String);
    let mut wanted: BTreeMap<Package, Vec<(String, u64)>> = BTreeMap::new();
    for source in &request.sources {
        let package = (
            source.package_path.clone(),
            source.package_bytes,
            source.package_sha256.clone(),
        );
        wanted
            .entry(package)
            .or_default()
            .push((source.table_path.clone(), tables::MAX_TABLE_BYTES));
    }
    for figure in &request.figures {
        let package = (
            figure.package_path.clone(),
            figure.package_bytes,
            figure.package_sha256.clone(),
        );
        wanted
            .entry(package)
            .or_default()
            .push((figure.path.clone(), MAX_FIGURE_BYTES));
    }
    // The stored sizes are the registered ones, which each read checks against the file before hashing it.
    let stored = wanted
        .keys()
        .map(|(_, bytes, _)| u64::try_from(*bytes).unwrap_or(u64::MAX))
        .fold(0u64, u64::saturating_add);
    if stored > MAX_PACKAGES_BYTES {
        return Err(refused("document_sources_too_large"));
    }
    let (mut members, mut left) = (HashMap::new(), MAX_SOURCE_BYTES + 8 * MAX_FIGURE_BYTES);
    let mut scan = MAX_SCAN_BYTES;
    for ((path, bytes, sha), list) in &wanted {
        let budget = Budget {
            total: left,
            scan,
            stop: Some(stop),
        };
        let (read, scanned) =
            read_members(root, path, *bytes, sha, list, &budget).map_err(member_stop)?;
        scan = scan.saturating_sub(scanned);
        for (member, content) in read {
            left -= content.len() as u64;
            members.insert((path.clone(), member), content);
        }
    }
    let (mut tables, mut read, mut rows, mut cells) = (HashMap::new(), 0u64, 0usize, 0usize);
    for source in &request.sources {
        let bytes = members
            .get(&(source.package_path.clone(), source.table_path.clone()))
            .ok_or_else(|| refused("document_table_missing"))?;
        read += bytes.len() as u64;
        if read > MAX_SOURCE_BYTES {
            return Err(refused("document_sources_too_large"));
        }
        if hex::encode(Sha256::digest(bytes)) != source.table_sha256 {
            return Err(refused("document_source_changed"));
        }
        let delimiter = if source.table_path.ends_with(".tsv") {
            b'\t'
        } else {
            b','
        };
        let table = tables::parse(bytes, delimiter, source.table_sha256.clone())
            .map_err(|_| refused("document_source_unreadable"))?;
        if i64::try_from(table.rows()).ok() != Some(source.rows) {
            return Err(refused("document_source_changed"));
        }
        rows += table.rows();
        cells += table.rows() * table.columns.len();
        if rows > MAX_SOURCE_ROWS || cells > MAX_SOURCE_CELLS {
            return Err(refused("document_sources_too_large"));
        }
        tables.insert((source.reference.clone(), source.table_path.clone()), table);
    }
    let mut figures = Vec::with_capacity(request.figures.len());
    for figure in &request.figures {
        let bytes = members
            .get(&(figure.package_path.clone(), figure.path.clone()))
            .ok_or_else(|| refused("document_table_missing"))?;
        if hex::encode(Sha256::digest(bytes)) != figure.sha256 {
            return Err(refused("document_source_changed"));
        }
        figures.push(bytes.clone());
    }
    Ok((tables, figures))
}

async fn run(
    store: &Store,
    config: &WorkerConfig,
    task: &ClaimedTask,
    request: &TaskDocument,
) -> Result<Done, Stop> {
    if !document_write::KINDS.contains(&request.kind.as_str()) {
        return Err(refused("document_record_invalid"));
    }
    // A pinned source whose artifact no longer holds the pinned bytes is left out of the lease read.
    if request.sources.len() != request.source_count
        || request.figures.len() != request.figure_count
    {
        return Err(refused("document_source_changed"));
    }
    let (root, owned) = (config.artifact_root.clone(), request.clone());
    let (tables, figures) = leased(
        store,
        config,
        task,
        "document_source_unreadable",
        move |stop| read_sources(&root, &owned, stop),
    )
    .await??;
    let (owned, claimed) = (request.clone(), task.clone());
    let (composed, written, package) =
        leased(store, config, task, "document_write_failed", move |_| {
            let composed = document_compose::compose(&Inputs {
                request: &owned,
                tables: &tables,
                figures: &figures,
            })
            .map_err(|refusal| refused(refusal.0))?;
            drop(tables);
            let written = document_write::write(&composed.body).map_err(write_stop)?;
            // The writer reads the body compose wrote; their counts must agree.
            if written.counts != composed.counts {
                return Err(failed("document_write_failed"));
            }
            let package = package(&claimed, &owned, &composed, &written.file)
                .map_err(|_| failed("document_package_failed"))?;
            Ok::<_, Stop>((composed, written, package))
        })
        .await??;
    // Writing takes time: keep the package only while this lease still holds and no one has cancelled. The completing
    // transaction checks both again.
    still_held(store, config, task).await?;
    let retained = retain(config, task, package).await?;
    let file_record = json!({"kind": request.kind, "path": format!("document.{}", request.kind),
        "media_type": media_type(&request.kind), "sha256": hex::encode(Sha256::digest(&written.file)),
        "size_bytes": written.file.len()});
    Ok(Done {
        file: file_record,
        counts: written.counts,
        notes: Value::Array(composed.notes.iter().map(|n| json!(n)).collect()),
        retained,
    })
}

/// Run blocking `work` off the async threads while renewing the lease every third of its length and checking for a
/// cancellation every two seconds. When the lease is lost or the task is cancelled, the flag `work` reads is set and
/// the route waits for the work to end before it gives up the task, so a cancelled document never leaves work running
/// beside the next one; when the caller stops waiting (the worker shutting down), the flag is set as it goes. `failure`
/// is the code when the work itself panics.
async fn leased<T: Send + 'static>(
    store: &Store,
    config: &WorkerConfig,
    task: &ClaimedTask,
    failure: &'static str,
    work: impl FnOnce(&AtomicBool) -> T + Send + 'static,
) -> Result<T, Stop> {
    struct StopOnDrop(Arc<AtomicBool>);
    impl Drop for StopOnDrop {
        fn drop(&mut self) {
            self.0.store(true, Ordering::Relaxed);
        }
    }
    let stop = Arc::new(AtomicBool::new(false));
    let flag = Arc::clone(&stop);
    let handle = tokio::task::spawn_blocking(move || work(&flag));
    let _stop = StopOnDrop(Arc::clone(&stop));
    tokio::pin!(handle);
    let worker = config.worker_id.as_str();
    let mut renewals = interval(Duration::from_secs((config.lease_seconds / 3).max(5)));
    renewals.set_missed_tick_behavior(MissedTickBehavior::Delay);
    renewals.tick().await;
    let mut cancels = interval(Duration::from_secs(2));
    cancels.set_missed_tick_behavior(MissedTickBehavior::Delay);
    cancels.tick().await;
    let stopped = loop {
        tokio::select! {
            done = &mut handle => return done.map_err(|_| failed(failure)),
            _ = renewals.tick() => {
                let renewed = store
                    .renew_task(task.id, worker, task.lease_generation, config.lease_seconds as i32)
                    .await;
                if !matches!(renewed, Ok(true)) {
                    break failed("lease_lost");
                }
            }
            _ = cancels.tick() => {
                match store.cancel_requested(task.id, worker, task.lease_generation).await {
                    Ok(false) => {}
                    Ok(true) => break failed("cancelled"),
                    Err(_) => break failed("lease_lost"),
                }
            }
        }
    };
    stop.store(true, Ordering::Relaxed);
    let _ = (&mut handle).await;
    Err(stopped)
}

/// Write the package and its checksum file privately under the attempt's folder and build its digest. Registration is
/// the completing transaction's.
async fn retain(
    config: &WorkerConfig,
    task: &ClaimedTask,
    package: Vec<u8>,
) -> Result<RetainedPackage, Stop> {
    let folder = format!(
        "tasks/{}/attempts/{}-{}/exports",
        task.id, task.attempt, task.lease_generation
    );
    let (root, id) = (config.artifact_root.clone(), task.id);
    tokio::task::spawn_blocking(move || {
        let payload = format!("{folder}/payload");
        let checksum = format!("{folder}/payload.sha256");
        let sha = hex::encode(Sha256::digest(&package));
        let line = format!("{sha}  payload\n");
        write_private(&root.join(&payload), &package)
            .map_err(|_| failed("document_package_failed"))?;
        write_private(&root.join(&checksum), line.as_bytes())
            .map_err(|_| failed("document_package_failed"))?;
        let built = digest::build(&root, id, &payload, package.len() as u64, &sha);
        Ok(RetainedPackage {
            package_bytes: package.len() as i64,
            package_sha256: sha,
            package_path: payload,
            checksum_bytes: line.len() as i64,
            checksum_sha256: hex::encode(Sha256::digest(line.as_bytes())),
            checksum_path: checksum,
            digest_status: Some(built.status.to_owned()),
            digest: Some(built.value),
        })
    })
    .await
    .map_err(|_| failed("document_package_failed"))?
}

/// Remove a retained package and its checksum file that the completing transaction refused to register.
async fn discard(root: std::path::PathBuf, retained: &RetainedPackage) {
    let paths = [
        retained.package_path.clone(),
        retained.checksum_path.clone(),
    ];
    let removed = tokio::task::spawn_blocking(move || {
        paths
            .iter()
            .all(|path| std::fs::remove_file(root.join(path)).is_ok())
    })
    .await;
    if !matches!(removed, Ok(true)) {
        tracing::warn!(event = "document_package_not_removed");
    }
}

/// Stop when the task was cancelled or this worker's lease no longer holds; otherwise renew the lease.
async fn still_held(store: &Store, config: &WorkerConfig, task: &ClaimedTask) -> Result<(), Stop> {
    let worker = config.worker_id.as_str();
    if store
        .cancel_requested(task.id, worker, task.lease_generation)
        .await
        .map_err(|_| failed("lease_lost"))?
    {
        return Err(failed("cancelled"));
    }
    if !store
        .renew_task(
            task.id,
            worker,
            task.lease_generation,
            config.lease_seconds as i32,
        )
        .await
        .map_err(|_| failed("lease_lost"))?
    {
        return Err(failed("lease_lost"));
    }
    Ok(())
}

/// The package around the written file, with the writer's own receipt for it.
fn package(
    task: &ClaimedTask,
    request: &TaskDocument,
    composed: &Composed,
    file: &[u8],
) -> anyhow::Result<Vec<u8>> {
    let receipt = json!({
        "schema": "enrichedlab.document-receipt.v1",
        "kind": request.kind,
        "request_sha256": hex::encode(Sha256::digest(&composed.body)),
        "file_sha256": hex::encode(Sha256::digest(file)),
        "size_bytes": file.len(),
        "counts": composed.counts,
        "writer": {"name": "enrichedlab-agent-broker document writer", "version": document_write::writer_version()},
    });
    document_package::package(task, request, composed, file, &receipt)
}

#[path = "document_package.rs"]
mod document_package;

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn a_package_the_completion_refused_is_removed() {
        let root = tempfile::tempdir().unwrap();
        let folder = "tasks/t/attempts/1-1/exports";
        std::fs::create_dir_all(root.path().join(folder)).unwrap();
        for name in ["payload", "payload.sha256"] {
            std::fs::write(root.path().join(folder).join(name), b"x").unwrap();
        }
        let retained = RetainedPackage {
            package_bytes: 1,
            package_sha256: "a".repeat(64),
            package_path: format!("{folder}/payload"),
            checksum_bytes: 1,
            checksum_sha256: "b".repeat(64),
            checksum_path: format!("{folder}/payload.sha256"),
            digest_status: None,
            digest: None,
        };
        discard(root.path().to_path_buf(), &retained).await;
        assert!(!root.path().join(folder).join("payload").exists());
        assert!(!root.path().join(folder).join("payload.sha256").exists());
    }
}
