//! The worker's lab-dataset route (migration 059). Nothing here reads the file a
//! person sent: the upload guard's bounded helper parsed it and rebuilt it as a cell grid, and the worker reads only
//! that grid.
//!
//! An intake fetches the grid with the worker's fetcher token, checks it against the digest and size of the guard's
//! decision the API recorded, profiles it (`dataset_profile`) and keeps a package with the grid, the profile and its
//! detail. A preparation reads the grid from the intake's package, checks both digests, applies the specification the
//! person approved (`dataset_prepare`) and keeps a package with the table, a copy for spreadsheets, the specification
//! and the executor's receipt. One receipt is recorded per attempt; a refused preparation records the executor's
//! receipt too, so the person sees which cells to decide about.
use std::{future::Future, io::Cursor, time::Duration};

use flate2::{Compression, GzBuilder};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use time::OffsetDateTime;
use tokio::time::{MissedTickBehavior, interval};
use tokio_util::sync::CancellationToken;

use super::{
    cell_grid::GridError,
    config::WorkerConfig,
    dataset_prepare::{self, Spec},
    dataset_profile,
    figure_run::{self, MemberError},
    guard_client::{GuardError, GuardFetcher},
    model::ClaimedTask,
    store::{DatasetReceipt, Store, TaskDataset},
};

/// The profile route that selects this path.
pub const ROUTE: &str = "server-dataset";
const INTAKE_LABEL: &str = "Lab dataset: kept grid and profile";
const PREPARATION_LABEL: &str = "Prepared lab table and its receipt";
/// The largest grid the worker takes from the guard: the guard keeps a rebuilt copy at most twice the 32 MiB upload.
const MAX_GRID_COPY: u64 = 96 * 1024 * 1024;
/// What a receipt may hold, as migration 059 checks it.
const MAX_PROFILE_BYTES: usize = 2 * 1024 * 1024;
const MAX_SUMMARY_BYTES: usize = 1024 * 1024;

/// The most bytes PostgreSQL's text of `value` as jsonb can take: its compact JSON with a space after each separator,
/// at most one and a half times as long. Profiles and receipts hold no fractional numbers, which jsonb could print
/// longer.
fn stored_bytes(value: &Value) -> usize {
    let compact = value.to_string().len();
    compact + compact / 2
}
const GRID_MEMBER: &str = "source/grid.ndjson.gz";

/// Why an attempt did not complete: `refused` for what the dataset or its preparation holds, `failed` for what may pass
/// on another attempt. A preparation's refusal keeps the executor's receipt.
struct Stop {
    status: &'static str,
    code: &'static str,
    summary: Option<Value>,
}

fn refused(code: &'static str) -> Stop {
    Stop {
        status: "refused",
        code,
        summary: None,
    }
}

fn failed(code: &'static str) -> Stop {
    Stop {
        status: "failed",
        code,
        summary: None,
    }
}

struct Done {
    status: &'static str,
    profile: Option<Value>,
    summary: Option<Value>,
    grid_sha256: String,
    table: Option<(String, String, i64, Value)>,
    package_sha256: String,
}

/// Run one claimed intake or preparation to a finished state, or leave it to lease recovery when the lease or the
/// worker stops.
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
    let feature_telemetry = super::feature_events::AttemptGuard::for_task(
        super::feature_events::Feature::Dataset,
        &task,
    );
    let Some(request) = store
        .task_dataset(task.id, worker, task.attempt, generation)
        .await?
    else {
        store
            .finish_task(
                task.id,
                worker,
                generation,
                false,
                "dataset_not_run",
                "dataset_not_recorded",
            )
            .await?;
        feature_telemetry.finish(
            super::feature_events::Outcome::Failure,
            Some(super::feature_events::ErrorCode::MissingRequest),
            0,
            1,
        );
        return Ok(());
    };
    let started = OffsetDateTime::now_utc();
    let outcome = tokio::select! {
        outcome = run(&store, config, &task, &request) => outcome,
        () = shutdown.cancelled() => {
            feature_telemetry.finish(
                super::feature_events::Outcome::Cancelled,
                Some(super::feature_events::ErrorCode::Cancelled), 0, 0);
            return Ok(())
        },
    };
    let finished = OffsetDateTime::now_utc();
    let (receipt, passed, code) = match outcome {
        Ok(done) => (
            DatasetReceipt {
                status: done.status,
                error_code: None,
                profile: done.profile,
                summary: done.summary,
                grid_sha256: Some(done.grid_sha256),
                table: done.table,
                package_sha256: Some(done.package_sha256),
                executor: dataset_prepare::EXECUTOR,
                executor_version: dataset_prepare::EXECUTOR_VERSION as i32,
                started_at: started,
                finished_at: finished,
            },
            true,
            String::new(),
        ),
        Err(stop) => (
            DatasetReceipt {
                status: stop.status,
                error_code: Some(stop.code.to_owned()),
                profile: None,
                summary: stop.summary,
                grid_sha256: None,
                table: None,
                package_sha256: None,
                executor: dataset_prepare::EXECUTOR,
                executor_version: dataset_prepare::EXECUTOR_VERSION as i32,
                started_at: started,
                finished_at: finished,
            },
            false,
            stop.code.to_owned(),
        ),
    };
    store
        .record_dataset_receipt(task.id, worker, task.attempt, generation, &receipt)
        .await?;
    let class = if passed {
        "dataset_completed"
    } else {
        "dataset_not_completed"
    };
    store
        .finish_task(task.id, worker, generation, passed, class, &code)
        .await?;
    tracing::info!(task_id=%task.id, lease_generation=generation, event="dataset_finished", status=receipt.status,
        code=code.as_str());
    let telemetry_outcome = match receipt.status {
        "profiled" | "prepared" => super::feature_events::Outcome::Success,
        "refused" => super::feature_events::Outcome::Refused,
        _ => super::feature_events::Outcome::Failure,
    };
    feature_telemetry.finish(
        telemetry_outcome,
        (!passed).then_some(super::feature_events::ErrorCode::DatasetFailed),
        u64::from(passed),
        u64::from(!passed),
    );
    Ok(())
}

async fn run(
    store: &Store,
    config: &WorkerConfig,
    task: &ClaimedTask,
    request: &TaskDataset,
) -> Result<Done, Stop> {
    match request.operation.as_str() {
        "intake" => intake(store, config, task, request).await,
        "preparation" => preparation(store, config, task, request).await,
        _ => Err(failed("dataset_not_recorded")),
    }
}

/// Wait for `work` while renewing the lease, and stop when a cancellation is asked for.
async fn leased<T>(
    store: &Store,
    config: &WorkerConfig,
    task: &ClaimedTask,
    work: impl Future<Output = T>,
) -> Result<T, Stop> {
    let worker = config.worker_id.as_str();
    tokio::pin!(work);
    let mut renewals = interval(Duration::from_secs((config.lease_seconds / 3).max(5)));
    renewals.set_missed_tick_behavior(MissedTickBehavior::Delay);
    renewals.tick().await;
    loop {
        tokio::select! {
            done = &mut work => return Ok(done),
            _ = renewals.tick() => {
                let renewed = store
                    .renew_task(task.id, worker, task.lease_generation, config.lease_seconds as i32)
                    .await
                    .map_err(|_| failed("lease_lost"))?;
                if !renewed {
                    return Err(failed("lease_lost"));
                }
                if store
                    .cancel_requested(task.id, worker, task.lease_generation)
                    .await
                    .map_err(|_| failed("lease_lost"))?
                {
                    return Err(failed("cancelled"));
                }
            }
        }
    }
}

async fn cancelled(store: &Store, config: &WorkerConfig, task: &ClaimedTask) -> Result<(), Stop> {
    if store
        .cancel_requested(task.id, config.worker_id.as_str(), task.lease_generation)
        .await
        .map_err(|_| failed("lease_lost"))?
    {
        return Err(failed("cancelled"));
    }
    Ok(())
}

fn grid_stop(error: GridError) -> Stop {
    match error {
        GridError::Unreadable => refused("dataset_grid_unreadable"),
        GridError::TooLarge => refused("dataset_grid_too_large"),
    }
}

async fn intake(
    store: &Store,
    config: &WorkerConfig,
    task: &ClaimedTask,
    request: &TaskDataset,
) -> Result<Done, Stop> {
    let path = config
        .dataset_fetcher_config
        .clone()
        .ok_or_else(|| failed("dataset_guard_unconfigured"))?;
    let fetcher = GuardFetcher::load(&path).map_err(|_| failed("dataset_guard_unconfigured"))?;
    let size = u64::try_from(request.grid_bytes).map_err(|_| failed("dataset_not_recorded"))?;
    let grid = leased(
        store,
        config,
        task,
        fetcher.rebuilt(
            request.guard_intake,
            &request.grid_sha256,
            size,
            MAX_GRID_COPY,
        ),
    )
    .await?
    .map_err(|e| match e {
        GuardError::TooLarge => refused("dataset_grid_too_large"),
        _ => failed("dataset_guard_unavailable"),
    })?;
    cancelled(store, config, task).await?;
    let (grid, profiled) = leased(
        store,
        config,
        task,
        tokio::task::spawn_blocking(move || {
            let profiled = dataset_profile::profile(|| Ok(Cursor::new(&grid[..])));
            (grid, profiled)
        }),
    )
    .await?
    .map_err(|_| failed("dataset_profile_failed"))?;
    let (profile, detail) = profiled.map_err(grid_stop)?;
    let profile_json =
        serde_json::to_value(&profile).map_err(|_| failed("dataset_package_failed"))?;
    let profile_bytes = pretty(&profile_json)?;
    let detail_bytes =
        pretty(&serde_json::to_value(&detail).map_err(|_| failed("dataset_package_failed"))?)?;
    let spec = json!({
        "schema": "enrichedlab.dataset-intake-package.v1",
        "task_id": task.id,
        "dataset": request.dataset,
        "file": {"name": request.file_name, "media_type": request.media_type, "guard_intake": request.guard_intake},
        "grid": {"path": GRID_MEMBER, "sha256": request.grid_sha256, "bytes": request.grid_bytes},
        "profile": {"path": "profile.json", "sha256": hex::encode(Sha256::digest(&profile_bytes)),
            "detail_path": "profile_detail.json", "detail_sha256": hex::encode(Sha256::digest(&detail_bytes))},
        "request_sha256": request.request_sha256,
    });
    let members = vec![
        (GRID_MEMBER.to_owned(), grid),
        ("profile.json".to_owned(), profile_bytes),
        ("profile_detail.json".to_owned(), detail_bytes),
        ("spec.json".to_owned(), pretty(&spec)?),
        ("README.md".to_owned(), intake_readme(request).into_bytes()),
    ];
    let package = package(members)?;
    figure_run::retain(store, config, task, &package, INTAKE_LABEL)
        .await
        .map_err(|_| failed("dataset_package_not_registered"))?;
    let kept = (stored_bytes(&profile_json) <= MAX_PROFILE_BYTES).then_some(profile_json);
    Ok(Done {
        status: "profiled",
        profile: kept,
        summary: None,
        grid_sha256: request.grid_sha256.clone(),
        table: None,
        package_sha256: hex::encode(Sha256::digest(&package)),
    })
}

async fn preparation(
    store: &Store,
    config: &WorkerConfig,
    task: &ClaimedTask,
    request: &TaskDataset,
) -> Result<Done, Stop> {
    // The specification as the broker stored it, whose digest the request recorded; the package keeps these bytes.
    let spec_text = request
        .spec_text
        .clone()
        .ok_or_else(|| failed("dataset_not_recorded"))?;
    let spec_sha256 = request
        .spec_sha256
        .clone()
        .ok_or_else(|| failed("dataset_not_recorded"))?;
    if hex::encode(Sha256::digest(spec_text.as_bytes())) != spec_sha256 {
        return Err(refused("dataset_spec_changed"));
    }
    let spec_value: Value =
        serde_json::from_str(&spec_text).map_err(|_| refused("dataset_preparation_invalid"))?;
    let spec = Spec::parse(&spec_value).map_err(|_| refused("dataset_preparation_invalid"))?;
    let intake = request
        .intake_package
        .clone()
        .ok_or_else(|| failed("dataset_intake_missing"))?;
    let root = config.artifact_root.clone();
    let (path, bytes, sha) = (intake.path.clone(), intake.bytes, intake.sha256.clone());
    let grid = tokio::task::spawn_blocking(move || {
        figure_run::read_member(&root, &path, bytes, &sha, GRID_MEMBER, MAX_GRID_COPY)
    })
    .await
    .map_err(|_| failed("dataset_intake_unreadable"))?
    .map_err(|e| match e {
        MemberError::Unreadable { refused: false } => failed("dataset_intake_unreadable"),
        MemberError::Unreadable { refused: true } | MemberError::Missing => {
            refused("dataset_intake_unreadable")
        }
        MemberError::Changed => refused("dataset_intake_changed"),
        MemberError::TooLarge => refused("dataset_grid_too_large"),
    })?;
    if hex::encode(Sha256::digest(&grid)) != request.grid_sha256 {
        return Err(refused("dataset_intake_changed"));
    }
    cancelled(store, config, task).await?;
    let (grid_sha, spec_for_run, sha_for_run) = (
        request.grid_sha256.clone(),
        spec.clone(),
        spec_sha256.clone(),
    );
    let prepared = leased(
        store,
        config,
        task,
        tokio::task::spawn_blocking(move || {
            dataset_prepare::prepare(
                Cursor::new(&grid[..]),
                &grid_sha,
                &spec_for_run,
                &sha_for_run,
            )
        }),
    )
    .await?
    .map_err(|_| failed("dataset_prepare_failed"))?;
    let prepared = match prepared {
        Ok(prepared) => prepared,
        Err(refusal) => {
            return Err(Stop {
                status: "refused",
                code: refusal.code,
                summary: refusal.receipt.as_deref().and_then(summary_json),
            });
        }
    };
    let receipt_json =
        serde_json::to_value(&prepared.receipt).map_err(|_| failed("dataset_package_failed"))?;
    let facts = prepared
        .receipt
        .table
        .clone()
        .ok_or_else(|| failed("dataset_package_failed"))?;
    let copy = prepared
        .receipt
        .spreadsheet_copy
        .clone()
        .ok_or_else(|| failed("dataset_package_failed"))?;
    let columns = json!(
        prepared
            .receipt
            .outputs
            .iter()
            .map(|o| o.name.clone())
            .collect::<Vec<_>>()
    );
    let spec_bytes = spec_text.into_bytes();
    let package_spec = json!({
        "schema": "enrichedlab.dataset-preparation-package.v1",
        "task_id": task.id,
        "dataset": request.dataset,
        "title": request.title,
        "file": {"name": request.file_name, "media_type": request.media_type, "guard_intake": request.guard_intake},
        "source": {"intake_task": request.intake_task, "intake_package_sha256": intake.sha256,
            "grid_sha256": request.grid_sha256},
        "preparation": {"path": "preparation.json", "sha256": spec_sha256},
        "executor": {"name": dataset_prepare::EXECUTOR, "version": dataset_prepare::EXECUTOR_VERSION},
        "table": facts,
        "spreadsheet_copy": copy,
        "columns": prepared.receipt.outputs,
        "request_sha256": request.request_sha256,
    });
    let members = vec![
        (facts.path.clone(), prepared.table),
        (copy.path.clone(), prepared.spreadsheet),
        ("preparation.json".to_owned(), spec_bytes),
        ("receipt.json".to_owned(), pretty(&receipt_json)?),
        ("spec.json".to_owned(), pretty(&package_spec)?),
        (
            "README.md".to_owned(),
            preparation_readme(request, &spec, &prepared.receipt).into_bytes(),
        ),
    ];
    let package = package(members)?;
    figure_run::retain(store, config, task, &package, PREPARATION_LABEL)
        .await
        .map_err(|_| failed("dataset_package_not_registered"))?;
    Ok(Done {
        status: "prepared",
        profile: None,
        summary: summary_json(&prepared.receipt),
        grid_sha256: request.grid_sha256.clone(),
        table: Some((facts.path, facts.sha256, facts.rows as i64, columns)),
        package_sha256: hex::encode(Sha256::digest(&package)),
    })
}

/// The executor's receipt as the broker stores it, within the stored bound; past it, each column keeps its counts but
/// not its recode pairs, whose cells move into `recoded_unlisted` so the column's total of recoded cells stays.
fn summary_json(receipt: &dataset_prepare::Receipt) -> Option<Value> {
    let mut value = serde_json::to_value(receipt).ok()?;
    if stored_bytes(&value) > MAX_SUMMARY_BYTES {
        for column in value["columns"].as_array_mut().into_iter().flatten() {
            let listed: u64 = column["recoded"]
                .as_object()
                .into_iter()
                .flatten()
                .filter_map(|(_, n)| n.as_u64())
                .sum();
            let unlisted = column["recoded_unlisted"].as_u64().unwrap_or(0);
            column["recoded_unlisted"] = json!(unlisted + listed);
            column["recoded"] = json!({});
        }
        value["recodes_left_out"] = json!(true);
    }
    (stored_bytes(&value) <= MAX_SUMMARY_BYTES).then_some(value)
}

fn pretty(value: &Value) -> Result<Vec<u8>, Stop> {
    let mut bytes =
        serde_json::to_vec_pretty(value).map_err(|_| failed("dataset_package_failed"))?;
    bytes.push(b'\n');
    Ok(bytes)
}

/// A deterministic gzip-compressed tar of the members in path order, with `SHA256SUMS` last.
fn package(mut members: Vec<(String, Vec<u8>)>) -> Result<Vec<u8>, Stop> {
    members.sort_by(|a, b| a.0.cmp(&b.0));
    let mut sums = String::new();
    for (path, bytes) in &members {
        sums.push_str(&format!(
            "{}  {}\n",
            hex::encode(Sha256::digest(bytes)),
            path
        ));
    }
    members.push(("SHA256SUMS".into(), sums.into_bytes()));
    (|| -> std::io::Result<Vec<u8>> {
        let mut gz = GzBuilder::new()
            .mtime(0)
            .write(Vec::new(), Compression::default());
        {
            let mut tar = tar::Builder::new(&mut gz);
            tar.mode(tar::HeaderMode::Deterministic);
            for (path, bytes) in &members {
                let mut header = tar::Header::new_gnu();
                header.set_size(bytes.len() as u64);
                header.set_mode(0o644);
                header.set_uid(0);
                header.set_gid(0);
                header.set_mtime(0);
                header.set_entry_type(tar::EntryType::Regular);
                header.set_cksum();
                tar.append_data(&mut header, path, bytes.as_slice())?;
            }
            tar.finish()?;
        }
        gz.finish()
    })()
    .map_err(|_| failed("dataset_package_failed"))
}

/// A file name as a README shows it: no control characters, at most 120 characters.
fn shown_name(name: &str) -> String {
    dataset_profile::shown(name)
}

fn intake_readme(request: &TaskDataset) -> String {
    format!(
        "# Lab dataset: {name}\n\n\
         The upload guard checked this file and rebuilt it as a cell grid; the broker never kept the file itself, which\n\
         stays with the guard under its own retention (intake `{intake}`).\n\n\
         - `{grid}`: the grid, `enrichedlab.cell-grid.v1`: gzip-compressed JSON lines, the source and its sheets first,\n  \
         then each sheet's rows as a spreadsheet program numbers them. SHA-256 `{grid_sha}`.\n\
         - `profile.json`: where each sheet's table is, each column's name, unit and type, and what to decide before\n  \
         analysis. It holds no cell of the data rows.\n\
         - `profile_detail.json`: each sheet's first rows, title and note text, frequent values and examples, for the\n  \
         people of the project.\n\
         - `spec.json`: this package's record.\n\n\
         Check the files with `sha256sum -c SHA256SUMS`.\n",
        name = shown_name(&request.file_name),
        intake = request.guard_intake,
        grid = GRID_MEMBER,
        grid_sha = request.grid_sha256,
    )
}

fn preparation_readme(
    request: &TaskDataset,
    spec: &Spec,
    receipt: &dataset_prepare::Receipt,
) -> String {
    let table = format!("results/lab_data/{}.csv", spec.table_name);
    let mut columns = String::new();
    for column in &receipt.outputs {
        let unit = column
            .unit
            .as_deref()
            .map(|u| format!(", {}", shown_name(u)))
            .unwrap_or_default();
        columns.push_str(&format!(
            "| `{}` | {}{} | {} |\n",
            column.name,
            column.kind,
            unit.replace('|', "\\|"),
            shown_name(&column.from).replace('|', "\\|")
        ));
    }
    format!(
        "# Prepared table: {name}\n\n\
         Prepared from `{file}` (the grid with SHA-256 `{grid}`, kept by intake task `{intake}`) with the preparation in\n\
         `preparation.json` (SHA-256 `{spec_sha}`, as the request recorded it), which a person approved, by the broker's\n\
         executor `{executor}` version {version}. No value was changed except by a rule the preparation names;\n\
         `receipt.json` counts each change.\n\n\
         - `{table}`: the table, {rows} rows. Missing values are `NA`. In R: `d <- read.csv(\"{table}\")`.\n\
         - `exports/{stem}.spreadsheet.csv`: the same table for a spreadsheet program; text that starts like a formula\n  \
         starts with `'`.\n\n\
         | Column | Type | From |\n|---|---|---|\n{columns}\n\
         A column `x` reported against a limit (such as `<0.5` or `ND`) has `x_censored` (`below`, `above`,\n\
         `not_detected`) and `x_limit` beside it; `x` itself follows the preparation's censoring rule.\n\n\
         Check the files with `sha256sum -c SHA256SUMS`.\n",
        name = spec.table_name,
        file = shown_name(&request.file_name),
        grid = request.grid_sha256,
        spec_sha = request.spec_sha256.as_deref().unwrap_or_default(),
        intake = request.intake_task,
        executor = dataset_prepare::EXECUTOR,
        version = dataset_prepare::EXECUTOR_VERSION,
        table = table,
        rows = receipt.rows.written,
        stem = spec.table_name,
        columns = columns,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn packages_are_deterministic_and_list_their_checksums() {
        let members = || {
            vec![
                ("b.txt".to_owned(), b"2".to_vec()),
                ("a.txt".to_owned(), b"1".to_vec()),
            ]
        };
        let first = package(members()).ok().unwrap();
        assert_eq!(package(members()).ok().unwrap(), first);
        let mut tar = tar::Archive::new(flate2::read::GzDecoder::new(&first[..]));
        let names: Vec<String> = tar
            .entries()
            .unwrap()
            .map(|e| e.unwrap().path().unwrap().display().to_string())
            .collect();
        assert_eq!(names, ["a.txt", "b.txt", "SHA256SUMS"]);
    }

    #[test]
    fn a_receipt_past_its_bound_keeps_counts_without_recodes() {
        let mut receipt = dataset_prepare::Receipt::default();
        let mut column = dataset_prepare::ColumnCounts::default();
        for i in 0..40_000 {
            column
                .recoded
                .insert(format!("value {i} -> another value {i}"), 1);
        }
        column.values = 7;
        column.recoded_unlisted = 5;
        receipt.columns.push(column);
        let summary = summary_json(&receipt).unwrap();
        assert!(jsonb_text_len(&summary) <= MAX_SUMMARY_BYTES);
        assert_eq!(summary["columns"][0]["values"], 7);
        assert_eq!(summary["columns"][0]["recoded"], json!({}));
        // The pairs go; the count of recoded cells stays.
        assert_eq!(summary["columns"][0]["recoded_unlisted"], 40_005);
        assert_eq!(summary["recodes_left_out"], true);
    }

    /// The length of PostgreSQL's text of `value` as jsonb, which puts a space after each `:` and `,`.
    fn jsonb_text_len(value: &Value) -> usize {
        match value {
            Value::Object(map) => {
                2 + map
                    .iter()
                    .map(|(k, v)| serde_json::to_string(k).unwrap().len() + 2 + jsonb_text_len(v))
                    .sum::<usize>()
                    + 2 * map.len().saturating_sub(1)
            }
            Value::Array(items) => {
                2 + items.iter().map(jsonb_text_len).sum::<usize>()
                    + 2 * items.len().saturating_sub(1)
            }
            other => other.to_string().len(),
        }
    }

    #[test]
    fn a_receipt_is_kept_only_when_its_stored_form_fits() {
        // Columns without recodes, so leaving recodes out makes no room: compact JSON within the bound whose stored
        // text, with a space after each separator, is past it.
        let column = |i: usize| dataset_prepare::ColumnCounts {
            name: format!("column_{i:05}"),
            from: "A".into(),
            ..Default::default()
        };
        let each = serde_json::to_string(&column(0)).unwrap().len() + 1;
        let receipt = dataset_prepare::Receipt {
            columns: (0..(MAX_SUMMARY_BYTES - 4096) / each).map(column).collect(),
            ..Default::default()
        };
        let value = serde_json::to_value(&receipt).unwrap();
        assert!(value.to_string().len() <= MAX_SUMMARY_BYTES);
        assert!(jsonb_text_len(&value) > MAX_SUMMARY_BYTES);
        assert!(
            summary_json(&receipt).is_none_or(|kept| jsonb_text_len(&kept) <= MAX_SUMMARY_BYTES)
        );
    }
}
