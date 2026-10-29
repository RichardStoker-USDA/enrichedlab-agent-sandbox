//! The worker's kind recorder. It checks each researcher-visible artifact that the broker's registry has
//! not checked, newest first, and records the kind its stored bytes hold with the check's receipt. It runs beside task
//! execution in the tenant's leader worker, one artifact at a time, off the async threads.
//!
//! A file that cannot be read, or whose bytes differ from its record, is a custody fault: nothing is recorded, the
//! worker logs `artifact_kind_unchecked` with the reason, and the artifact is retried after `RETRY`. A file that is read
//! and fails its kind's check is recorded as `file.opaque` with the reason; that result is final for those bytes under
//! this registry.
use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    time::Duration,
};

use tokio::time::{Instant, sleep};
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use super::{
    api::resolve_task_file,
    canonical::canonical_json,
    kinds::{self, Registry},
    store::{AwaitingKind, KindRecord, Store, StoreError},
};

const BATCH: i32 = 16;
const IDLE: Duration = Duration::from_secs(15);
const RETRY: Duration = Duration::from_secs(600);
/// Past this many unreadable artifacts the recorder stops remembering new ones and waits `IDLE` between scans.
const SKIP_LIMIT: usize = 10_000;

/// Runs until `shutdown`. A scan that fails is logged and tried again after `IDLE`.
pub async fn run(
    store: Store,
    artifact_root: PathBuf,
    worker: String,
    shutdown: CancellationToken,
) {
    let registry = match kinds::registry() {
        Ok(registry) => registry,
        Err(error) => {
            tracing::error!(event = "kind_registry_not_loaded", %error);
            return;
        }
    };
    let mut skip = HashMap::new();
    loop {
        let wait = match batch(&store, &artifact_root, &worker, registry, &mut skip).await {
            // A full batch that recorded something may have more behind it.
            Ok((found, recorded)) if found == BATCH as usize && recorded > 0 => Duration::ZERO,
            Ok(_) => IDLE,
            Err(error) => {
                tracing::warn!(event = "artifact_kind_scan_failed", code = error.code);
                IDLE
            }
        };
        tokio::select! {
            _ = shutdown.cancelled() => break,
            _ = sleep(wait) => {},
        }
    }
}

/// Checks one batch. Returns how many artifacts were waiting and how many records were written. `skip` holds the
/// artifacts to leave until their retry time.
pub(crate) async fn batch(
    store: &Store,
    artifact_root: &Path,
    worker: &str,
    registry: &'static Registry,
    skip: &mut HashMap<Uuid, Instant>,
) -> Result<(usize, usize), StoreError> {
    let now = Instant::now();
    skip.retain(|_, until| *until > now);
    let skipped: Vec<Uuid> = skip.keys().copied().collect();
    let awaiting = store
        .artifacts_awaiting_kind(&registry.digest, &skipped, BATCH)
        .await?;
    let mut recorded = 0;
    for item in &awaiting {
        match check(artifact_root, registry, item).await {
            Ok(stored) => {
                let receipt = String::from_utf8(canonical_json(&stored.receipt))
                    .expect("canonical JSON is UTF-8");
                let record = KindRecord {
                    artifact: item.artifact,
                    artifact_sha256: &item.sha256,
                    media_type: &item.media_type,
                    registry: &registry.digest,
                    kind: (&stored.recorded.0, stored.recorded.1),
                    checked: (&stored.checked.0, stored.checked.1),
                    passed: stored.outcome.passed,
                    reason: stored.outcome.reason.as_deref(),
                    receipt: &receipt,
                };
                match store.record_artifact_kind(worker, &record).await {
                    Ok(_) => {
                        recorded += 1;
                        tracing::info!(
                            event = "artifact_kind_recorded",
                            artifact = %item.artifact,
                            kind = %stored.recorded.0,
                            checked_as = %stored.checked.0,
                            result = if stored.outcome.passed { "passed" } else { "failed" },
                        );
                    }
                    Err(error) => {
                        tracing::warn!(
                            event = "artifact_kind_not_recorded",
                            artifact = %item.artifact,
                            code = error.code,
                        );
                        defer(skip, item.artifact);
                    }
                }
            }
            Err(reason) => {
                tracing::warn!(event = "artifact_kind_unchecked", artifact = %item.artifact, reason);
                defer(skip, item.artifact);
            }
        }
    }
    Ok((awaiting.len(), recorded))
}

fn defer(skip: &mut HashMap<Uuid, Instant>, artifact: Uuid) {
    if skip.len() < SKIP_LIMIT {
        skip.insert(artifact, Instant::now() + RETRY);
    }
}

async fn check(
    artifact_root: &Path,
    registry: &'static Registry,
    item: &AwaitingKind,
) -> Result<kinds::StoredCheck, &'static str> {
    let path = resolve_task_file(
        artifact_root,
        item.task,
        &item.relative_path,
        item.size_bytes,
    )
    .map_err(|error| error.code())?;
    let media_type = item.media_type.clone();
    let size = u64::try_from(item.size_bytes).map_err(|_| "artifact_size_invalid")?;
    let sha256 = item.sha256.clone();
    tokio::task::spawn_blocking(move || {
        kinds::check_stored(registry, &path, &media_type, size, &sha256)
    })
    .await
    .map_err(|_| "artifact_kind_check_stopped")?
}
