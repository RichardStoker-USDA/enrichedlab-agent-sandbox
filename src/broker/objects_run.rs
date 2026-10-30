//! The worker's lab objects route (migration 083). A step of a lab's objects method names one collection of the lab's
//! bucket, by the copy generation and manifest digest the knowledge index recorded, and the files it wants from it. No
//! model reads a file here and nothing runs model-written code: the worker reads the pinned manifest with the method's
//! read-only identity, checks its SHA-256, finds each named file's row, gets that exact object version, checks its size
//! and SHA-256 against the row, and keeps a package in an analysis's layout. Every file is kept under `input/objects/`;
//! a delimited text file (`.csv`, `.tsv`, or `.txt` whose first line holds a tab) of at most 50,000 rows is also kept as
//! a table under `results/lab_data/<study>/`, which the result digest lists with its columns and counts and never its
//! cells, so a template analysis can read it as it reads a query result. One receipt is recorded per attempt.
use std::{
    collections::{BTreeMap, BTreeSet},
    io::{BufReader, Cursor, Write},
    path::{Path, PathBuf},
};

use anyhow::{Context, Result, anyhow};
use flate2::{Compression, GzBuilder};
use serde::Deserialize;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use time::OffsetDateTime;
use tokio_util::sync::CancellationToken;

use super::{
    config::WorkerConfig,
    csv_records::{Record, Records},
    digest,
    evidence_objects::{EndpointConfig, S3Store, StoreError as ObjectError, valid_object_key},
    model::ClaimedTask,
    store::{ObjectReceipt, Store, TaskObjects},
};

/// The profile route that selects this path.
pub const ROUTE: &str = "server-objects";
const PACKAGE_LABEL: &str = "Lab files with their manifest rows";
/// The largest manifest read; a lab's full copy of thousands of files writes a manifest well under this.
const MAX_MANIFEST_BYTES: u64 = 16 * 1024 * 1024;
/// What one step may read in all, its manifest included. The step holds the files once (they move into the package),
/// its converted tables (at most MAX_TABLE_BYTES), the compressed package and the digest's read of it, so its peak
/// stays near four times this: about 300 MiB beside a 4 GiB server-route worker.
const MAX_STEP_BYTES: u64 = 64 * 1024 * 1024;
/// What one step's converted tables may hold in all; a file past it is kept as a file only.
const MAX_TABLE_BYTES: usize = 64 * 1024 * 1024;
/// A table an analysis can read: at most this many rows (`broker_check_analysis_request` takes 1 to 50,000).
const MAX_TABLE_ROWS: usize = 50_000;
const MAX_TABLE_COLUMNS: usize = 2_000;
const MAX_RECORD_BYTES: usize = 1024 * 1024;
const MAX_CONFIG_BYTES: u64 = 64 * 1024;

/// The worker's lab objects configuration: for each objects method (profile key), its bucket, the store's endpoint and
/// read identity, and the CA that signed the store's certificate.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LabObjectsConfig {
    pub methods: BTreeMap<String, MethodStore>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MethodStore {
    pub bucket: String,
    pub endpoint: EndpointConfig,
    pub ca_file: PathBuf,
}

/// One manifest row as the copy tool writes it.
#[derive(Debug, Clone, Deserialize, serde::Serialize)]
#[serde(deny_unknown_fields)]
struct ManifestRow {
    key: String,
    version_id: String,
    size: u64,
    sha256: String,
    sha1: String,
    box_id: String,
    study: String,
    kind: String,
    stage: String,
    rel_path: String,
    census: String,
}

/// Why an attempt did not complete: `refused` for what the request or the bucket's records stop, `failed` for what
/// may pass on another attempt.
#[derive(Debug)]
struct Stop {
    status: &'static str,
    code: &'static str,
}

fn refused(code: &'static str) -> Stop {
    Stop {
        status: "refused",
        code,
    }
}

fn failed(code: &'static str) -> Stop {
    Stop {
        status: "failed",
        code,
    }
}

struct Done {
    files: i32,
    bytes: i64,
    tables: Value,
    package_sha256: String,
}

/// Run one claimed objects task to a finished state, or leave it to lease recovery when the lease or the worker stops.
pub async fn execute(
    store: Store,
    config: &WorkerConfig,
    task: ClaimedTask,
    shutdown: CancellationToken,
) -> Result<()> {
    let worker = config.worker_id.as_str();
    let generation = task.lease_generation;
    if !store.mark_running(task.id, worker, generation).await? {
        return Ok(());
    }
    let Some(request) = store
        .task_objects(task.id, worker, task.attempt, generation)
        .await?
    else {
        store
            .finish_task(
                task.id,
                worker,
                generation,
                false,
                "objects_not_run",
                "objects_not_recorded",
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
    let (receipt, passed, class, code) = match outcome {
        Ok(done) => (
            ObjectReceipt {
                status: "completed",
                error_code: None,
                files: Some(done.files),
                bytes: Some(done.bytes),
                tables: Some(done.tables),
                package_sha256: Some(done.package_sha256),
                started_at: started,
                finished_at: finished,
            },
            true,
            "objects_completed",
            String::new(),
        ),
        Err(stop) => (
            ObjectReceipt {
                status: stop.status,
                error_code: Some(stop.code.to_owned()),
                files: None,
                bytes: None,
                tables: None,
                package_sha256: None,
                started_at: started,
                finished_at: finished,
            },
            false,
            "objects_not_completed",
            stop.code.to_owned(),
        ),
    };
    store
        .record_object_receipt(task.id, worker, task.attempt, generation, &receipt)
        .await?;
    store
        .finish_task(task.id, worker, generation, passed, class, &code)
        .await?;
    // Fixed codes and counts only: a key or file name is research metadata and stays out of logs.
    tracing::info!(task_id=%task.id, lease_generation=generation, event="objects_finished", status=receipt.status,
        code=code.as_str(), files=receipt.files.unwrap_or(0));
    Ok(())
}

fn load_config(path: &Path) -> Result<LabObjectsConfig> {
    let bytes = std::fs::read(path).with_context(|| format!("cannot read {}", path.display()))?;
    anyhow::ensure!(
        bytes.len() as u64 <= MAX_CONFIG_BYTES,
        "the lab objects configuration is too large"
    );
    Ok(serde_json::from_slice(&bytes)?)
}

async fn run(
    store: &Store,
    config: &WorkerConfig,
    task: &ClaimedTask,
    request: &TaskObjects,
) -> Result<Done, Stop> {
    let path = config
        .lab_objects_config
        .as_ref()
        .ok_or_else(|| refused("objects_unconfigured"))?;
    let objects = load_config(path).map_err(|_| failed("objects_config_invalid"))?;
    let method = objects
        .methods
        .get(&request.profile)
        .ok_or_else(|| refused("objects_method_unconfigured"))?;
    // The bucket is the method's own; a request for any other bucket is refused before the store is reached.
    if method.bucket != request.bucket {
        return Err(refused("objects_bucket_not_the_methods"));
    }
    let ca = std::fs::read(&method.ca_file).map_err(|_| failed("objects_config_invalid"))?;
    let s3 =
        S3Store::connect(&method.endpoint, &ca).map_err(|_| failed("objects_config_invalid"))?;
    let manifest_key = format!("manifests/{}.jsonl", request.generation);
    let manifest = s3
        .get_object(&request.bucket, &manifest_key, None, MAX_MANIFEST_BYTES)
        .await
        .map_err(|e| match e {
            ObjectError::NotFound => refused("objects_manifest_missing"),
            ObjectError::Malformed(_) => refused("objects_manifest_invalid"),
            _ => failed("objects_store_unavailable"),
        })?;
    if hex::encode(Sha256::digest(&manifest)) != request.manifest_sha256 {
        return Err(refused("objects_manifest_changed"));
    }
    let rows = rows_for(&manifest, &request.prefix, &request.files)?;
    let total: u64 = rows.iter().map(|r| r.size).sum();
    if within_budget(manifest.len() as u64, total).is_err() {
        return Err(refused("objects_too_large"));
    }
    drop(manifest);
    let mut files = Vec::with_capacity(rows.len());
    for row in &rows {
        let bytes = s3
            .get_object(&request.bucket, &row.key, Some(&row.version_id), row.size)
            .await
            .map_err(|e| match e {
                ObjectError::NotFound => refused("objects_file_missing"),
                ObjectError::Malformed(_) => failed("objects_file_changed"),
                _ => failed("objects_store_unavailable"),
            })?;
        if bytes.len() as u64 != row.size || hex::encode(Sha256::digest(&bytes)) != row.sha256 {
            return Err(failed("objects_file_changed"));
        }
        files.push(bytes);
    }
    let (package, tables) =
        package(task, request, &rows, files).map_err(|_| failed("objects_package_failed"))?;
    retain(store, config, task, &package).await?;
    Ok(Done {
        files: rows.len() as i32,
        bytes: total as i64,
        tables,
        package_sha256: hex::encode(Sha256::digest(&package)),
    })
}

/// The manifest rows of the named files, in the request's order. Refuses a manifest that does not parse, a file no row
/// names under the collection's prefix, or a row that names a different file than its own key says.
fn rows_for(manifest: &[u8], prefix: &str, files: &[String]) -> Result<Vec<ManifestRow>, Stop> {
    let text = std::str::from_utf8(manifest).map_err(|_| refused("objects_manifest_invalid"))?;
    let mut by_key: BTreeMap<String, ManifestRow> = BTreeMap::new();
    for line in text.lines().filter(|l| !l.trim().is_empty()) {
        let row: ManifestRow =
            serde_json::from_str(line).map_err(|_| refused("objects_manifest_invalid"))?;
        let own = format!("{}/{}/{}/{}", row.study, row.kind, row.stage, row.rel_path);
        if row.key != own
            || !valid_object_key(&row.key)
            || row.sha256.len() != 64
            || !row.sha256.bytes().all(|b| b.is_ascii_hexdigit())
        {
            return Err(refused("objects_manifest_invalid"));
        }
        if by_key.insert(row.key.clone(), row).is_some() {
            return Err(refused("objects_manifest_invalid"));
        }
    }
    let mut seen = BTreeSet::new();
    files
        .iter()
        .map(|file| {
            let key = format!("{prefix}{file}");
            if !valid_object_key(&key) || !seen.insert(key.clone()) {
                return Err(refused("objects_request_invalid"));
            }
            by_key
                .get(&key)
                .cloned()
                .ok_or_else(|| refused("objects_file_not_in_manifest"))
        })
        .collect()
}

/// The delimiter a file's table is read with, or none when it is not kept as a table.
fn delimiter(rel_path: &str, bytes: &[u8]) -> Option<u8> {
    let name = rel_path
        .rsplit('/')
        .next()
        .unwrap_or(rel_path)
        .to_ascii_lowercase();
    if name.ends_with(".csv") {
        Some(b',')
    } else if name.ends_with(".tsv") {
        Some(b'\t')
    } else if name.ends_with(".txt") {
        let first = bytes.split(|b| *b == b'\n').next().unwrap_or_default();
        first.contains(&b'\t').then_some(b'\t')
    } else {
        None
    }
}

/// A delimited file rewritten as RFC 4180 CSV with its header, or the reason it is kept as a file only.
fn table(bytes: &[u8], delimiter: u8) -> Result<(Vec<u8>, usize, usize), &'static str> {
    let mut records = Records::new(
        BufReader::new(Cursor::new(bytes)),
        delimiter,
        MAX_TABLE_COLUMNS,
        MAX_RECORD_BYTES,
    );
    let mut out = csv::WriterBuilder::new()
        .flexible(false)
        .from_writer(Vec::new());
    let mut columns = None;
    let mut rows = 0usize;
    loop {
        match records.read_record().map_err(|_| "unreadable")? {
            None => break,
            Some(Record::TooManyFields) => return Err("too_many_columns"),
            Some(Record::TooLong) => return Err("record_too_long"),
            Some(Record::Fields(n)) => {
                match columns {
                    None => columns = Some(n),
                    Some(c) if c != n => return Err("ragged"),
                    Some(_) => rows += 1,
                }
                if rows > MAX_TABLE_ROWS {
                    return Err("too_many_rows");
                }
                let fields: Vec<&[u8]> = records.fields().collect();
                out.write_record(fields).map_err(|_| "unwritable")?;
            }
        }
    }
    let columns = columns.ok_or("empty")?;
    if rows == 0 {
        return Err("no_rows");
    }
    let bytes = out.into_inner().map_err(|_| "unwritable")?;
    Ok((bytes, rows, columns))
}

/// A table's path: `results/lab_data/<study>/<file name without extension>.csv`, numbered when two share a name.
fn table_path(row: &ManifestRow, taken: &mut BTreeSet<String>) -> String {
    let name = row.rel_path.rsplit('/').next().unwrap_or(&row.rel_path);
    let stem = name.rsplit_once('.').map_or(name, |(stem, _)| stem);
    let safe: String = stem
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '_'
            }
        })
        .take(80)
        .collect();
    let base = format!(
        "{}{}/{}",
        digest::LAB_DATA,
        row.study,
        if safe.is_empty() { "table" } else { &safe }
    );
    let mut path = format!("{base}.csv");
    let mut n = 2;
    while !taken.insert(path.clone()) {
        path = format!("{base}_{n}.csv");
        n += 1;
    }
    path
}

/// Whether a step's manifest and files fit its read budget.
fn within_budget(manifest: u64, files: u64) -> Result<(), ()> {
    match manifest.checked_add(files) {
        Some(total) if total <= MAX_STEP_BYTES => Ok(()),
        _ => Err(()),
    }
}

/// A deterministic tar+gzip: sorted members, fixed owner, mode and time, so equal files give equal bytes. The files
/// move into the package; converted tables stop at MAX_TABLE_BYTES in all.
fn package(
    task: &ClaimedTask,
    request: &TaskObjects,
    rows: &[ManifestRow],
    files: Vec<Vec<u8>>,
) -> Result<(Vec<u8>, Value)> {
    let mut members: Vec<(String, Vec<u8>)> = Vec::new();
    let mut tables = Vec::new();
    let mut taken = BTreeSet::new();
    let mut converted = 0usize;
    for (row, bytes) in rows.iter().zip(files) {
        let Some(delimiter) = delimiter(&row.rel_path, &bytes) else {
            members.push((format!("input/objects/{}", row.key), bytes));
            continue;
        };
        let made = table(&bytes, delimiter);
        members.push((format!("input/objects/{}", row.key), bytes));
        let made = match made {
            Ok((csv, ..)) if converted + csv.len() > MAX_TABLE_BYTES => Err("tables_budget"),
            other => other,
        };
        match made {
            Ok((csv, rows_n, columns)) => {
                converted += csv.len();
                let path = table_path(row, &mut taken);
                tables.push(
                    json!({"path": path, "source_key": row.key, "rows": rows_n, "columns": columns,
                    "sha256": hex::encode(Sha256::digest(&csv))}),
                );
                members.push((path, csv));
            }
            Err(reason) => {
                tables.push(
                    json!({"path": null, "source_key": row.key, "kept_as_file_only": reason}),
                );
            }
        }
    }
    let manifest_rows: String = rows
        .iter()
        .map(|r| serde_json::to_string(r).map(|s| s + "\n"))
        .collect::<Result<_, _>>()?;
    let receipt = json!({
        "schema": "enrichedlab.lab-objects-receipt.v1",
        "task_id": task.id,
        "collection": request.collection,
        "bucket": request.bucket,
        "prefix": request.prefix,
        "generation": request.generation,
        "manifest_key": format!("manifests/{}.jsonl", request.generation),
        "manifest_sha256": request.manifest_sha256,
        "request_sha256": request.request_sha256,
        "files": rows.iter().map(|r| json!({"key": r.key, "version_id": r.version_id, "size": r.size,
            "sha256": r.sha256, "sha1": r.sha1, "box_id": r.box_id, "census": r.census})).collect::<Vec<_>>(),
        "tables": tables,
    });
    let parameters = json!({
        "method": "lab objects by version from the lab's bucket",
        "bucket": request.bucket,
        "generation": request.generation,
        "files": rows.len(),
    });
    members.push((
        "README.md".into(),
        readme(request, rows.len(), tables.len()).into_bytes(),
    ));
    members.push(("manifest/rows.jsonl".into(), manifest_rows.into_bytes()));
    members.push(("receipt.json".into(), serde_json::to_vec_pretty(&receipt)?));
    members.push((
        "run_parameters.json".into(),
        serde_json::to_vec_pretty(&parameters)?,
    ));
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
    let tables = Value::Array(receipt["tables"].as_array().cloned().unwrap_or_default());
    Ok((gz.finish()?, tables))
}

fn readme(request: &TaskObjects, files: usize, tables: usize) -> String {
    format!(
        "# Lab files\n\nThis package holds {files} file(s) of one collection of the lab's object bucket \
         `{bucket}` (copy generation `{generation}`), each read by its exact version and checked against the \
         collection's manifest (SHA-256 `{manifest}`).\n\n- `input/objects/` keeps every file as stored, under its \
         bucket key.\n- `results/lab_data/` keeps each delimited text file of at most 50,000 rows as a CSV table \
         ({tables} file(s) were considered; `receipt.json` says which became tables and why any did not).\n- \
         `manifest/rows.jsonl` is each file's manifest row: version, size, SHA-256, SHA-1, Box ID and census.\n- \
         `receipt.json` names the collection, generation, manifest digest and every file.\n- `SHA256SUMS` lists every \
         file's SHA-256.\n\n## Check the files\n\nmacOS: `shasum -a 256 -c SHA256SUMS`  \nLinux: `sha256sum -c \
         SHA256SUMS`\n\n## Read them again\n\nEach file's bucket key and version are in `receipt.json`; the bucket's \
         read identity gets the same bytes by version while its retention lasts.\n",
        bucket = request.bucket,
        generation = request.generation,
        manifest = request.manifest_sha256,
    )
}

async fn retain(
    store: &Store,
    config: &WorkerConfig,
    task: &ClaimedTask,
    package: &[u8],
) -> Result<(), Stop> {
    let worker = config.worker_id.as_str();
    let generation = task.lease_generation;
    let folder = format!(
        "tasks/{}/attempts/{}-{}/exports",
        task.id, task.attempt, generation
    );
    let payload = format!("{folder}/payload");
    let checksum = format!("{folder}/payload.sha256");
    let sha = hex::encode(Sha256::digest(package));
    let sum_line = format!("{sha}  payload\n");
    let root = config.artifact_root.clone();
    write_private(&root.join(&payload), package).map_err(|_| failed("objects_package_failed"))?;
    write_private(&root.join(&checksum), sum_line.as_bytes())
        .map_err(|_| failed("objects_package_failed"))?;
    store
        .register_artifact(
            task.id,
            worker,
            generation,
            "researcher",
            "result",
            PACKAGE_LABEL,
            &payload,
            "application/gzip",
            package.len() as i64,
            &sha,
        )
        .await
        .map_err(|_| failed("objects_package_not_registered"))?;
    store
        .register_artifact(
            task.id,
            worker,
            generation,
            "researcher",
            "evidence",
            "Package checksum",
            &checksum,
            "text/plain",
            sum_line.len() as i64,
            &hex::encode(Sha256::digest(sum_line.as_bytes())),
        )
        .await
        .map_err(|_| failed("objects_package_not_registered"))?;
    let (digest_root, digest_path, size, digest_sha, id) = (
        root.clone(),
        payload.clone(),
        package.len() as u64,
        sha.clone(),
        task.id,
    );
    let built = tokio::task::spawn_blocking(move || {
        digest::build(&digest_root, id, &digest_path, size, &digest_sha)
    })
    .await
    .map_err(|_| failed("objects_digest_failed"))?;
    if let Err(e) = store
        .record_artifact_digest(
            task.id,
            worker,
            generation,
            &payload,
            &sha,
            built.status,
            &built.value,
        )
        .await
    {
        // A digest that cannot be recorded leaves a gap the coordinator reports; the result itself is kept.
        tracing::warn!(task_id=%task.id, lease_generation=generation, event="result_digest_not_recorded", code=e.code);
    }
    Ok(())
}

fn write_private(path: &Path, bytes: &[u8]) -> Result<()> {
    use std::os::unix::fs::OpenOptionsExt;
    if let Some(parent) = path.parent() {
        crate::config::create_private_dir(parent)?;
    }
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)
        .map_err(|e| anyhow!("{e}"))?;
    file.write_all(bytes)?;
    file.sync_all()?;
    Ok(())
}

#[cfg(test)]
#[path = "objects_run_tests.rs"]
mod tests;
