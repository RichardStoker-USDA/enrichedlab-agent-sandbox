//! The worker's query route. A task whose method is a source's query runs here, not in a sandbox: no
//! model-written code executes, only the SELECT the broker's guard admitted. The worker checks the guard again, opens a
//! read-only repeatable-read transaction under the source's reader login, confirms the database holds the snapshot the
//! source names, and lets PostgreSQL write the result with COPY, so the table's bytes are the server's own CSV output. A
//! NULL is written `NA`, as R writes a missing value, and a one-column result quotes every other value, so no row is
//! ever a blank line that a reader would skip.
//! A live lab source (migration 053) names no snapshot: in the same transaction the worker reads the one row its database
//! states about its version and keeps it with the receipt and in the package, so every result says what it read.
//! It retains a package in the same layout as an R analysis's (tables under `results/`, `run_parameters.json`,
//! `SHA256SUMS`), records its digest, one receipt per attempt, and finishes the task. A failure the lead can act on (an
//! unknown column, a type mismatch, a timeout) finishes the task with a fixed code the next decision reads.
use std::{collections::BTreeSet, io::Write, path::Path, str::FromStr, time::Duration};

use anyhow::{Context, Result, anyhow, bail};
#[path = "query_package.rs"]
mod query_package;
use futures_util::StreamExt;
use native_tls::{Certificate, TlsConnector};
use postgres_native_tls::MakeTlsConnector;
use query_package::package;
#[path = "query_admission.rs"]
mod query_admission;
use query_admission::admit_stored;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use time::OffsetDateTime;
use tokio_postgres::{
    Client, Config as PostgresConfig, IsolationLevel, NoTls, config::SslMode, error::SqlState,
};
use tokio_util::sync::CancellationToken;

use super::{
    config::{ReaderConnection, WorkerConfig},
    digest,
    model::ClaimedTask,
    sql_guard::{self, Allowed},
    store::{QueryReceipt, Store, TaskQuery},
};

/// The profile route that selects this path.
pub const ROUTE: &str = "server-query";
const MAX_RESULT_BYTES: usize = 64 * 1024 * 1024;
const PACKAGE_LABEL: &str = "Database query and reproducibility package";
const TABLE_PATH: &str = "results/query_result.csv";

/// Why an attempt did not complete: `refused` when the guard or the source's own records stop it, `failed` otherwise.
#[derive(Debug)]
struct Stop {
    status: &'static str,
    code: String,
}

fn refused(code: &str) -> Stop {
    Stop {
        status: "refused",
        code: code.into(),
    }
}

fn failed(code: &str) -> Stop {
    Stop {
        status: "failed",
        code: code.into(),
    }
}

struct Done {
    rows: i32,
    columns: Value,
    package_sha256: String,
    server_version: String,
    source_version: Option<Value>,
}

/// What the worker confirms about the source in the result's own transaction.
#[derive(Clone, Copy, Debug)]
enum Expected<'a> {
    /// A pinned snapshot: the database must hold this manifest digest.
    Manifest(&'a str),
    /// A live lab database: the relation in which it states its version.
    Live(&'a str),
}

/// The largest version row a live source may state, as JSON.
const MAX_VERSION_BYTES: usize = 2048;

/// A snapshot without a kind names a manifest digest; a live one names its version relation and no manifest (migration
/// 053), so a worker that only knows manifests refuses it before it connects.
fn expected(snapshot: &Value) -> Result<Expected<'_>, Stop> {
    let relation = |r: &&str| {
        let b = r.as_bytes();
        (1..=63).contains(&b.len())
            && b[0].is_ascii_lowercase()
            && b.iter()
                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || *c == b'_')
    };
    match snapshot.get("kind").and_then(Value::as_str) {
        None => snapshot["manifest_sha256"]
            .as_str()
            .filter(|v| v.len() == 64 && v.bytes().all(|b| b.is_ascii_hexdigit()))
            .map(Expected::Manifest),
        Some("live") if snapshot.get("manifest_sha256").is_none() => snapshot["version_relation"]
            .as_str()
            .filter(relation)
            .map(Expected::Live),
        Some(_) => None,
    }
    .ok_or_else(|| refused("snapshot_not_recorded"))
}

/// Run one claimed query task to a finished state, or leave it to lease recovery when the lease or the worker stops.
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
    let feature_telemetry =
        super::feature_events::AttemptGuard::for_task(super::feature_events::Feature::Query, &task);
    let Some(query) = store
        .task_query(task.id, worker, task.attempt, generation)
        .await?
    else {
        store
            .finish_task(
                task.id,
                worker,
                generation,
                false,
                "query_not_run",
                "query_not_recorded",
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
        outcome = run(&store, config, &task, &query) => outcome,
        () = shutdown.cancelled() => {
            feature_telemetry.finish(
                super::feature_events::Outcome::Cancelled,
                Some(super::feature_events::ErrorCode::Cancelled), 0, 0);
            return Ok(())
        },
    };
    let finished = OffsetDateTime::now_utc();
    let (receipt, passed, result_class, error_code) = match outcome {
        Ok(done) => (
            QueryReceipt {
                status: "completed",
                error_code: None,
                row_count: Some(done.rows),
                columns: Some(done.columns),
                package_sha256: Some(done.package_sha256),
                server_version: Some(done.server_version),
                started_at: started,
                finished_at: finished,
                source_version: done.source_version,
            },
            true,
            "query_completed",
            String::new(),
        ),
        Err(stop) => (
            QueryReceipt {
                status: stop.status,
                error_code: Some(stop.code.clone()),
                row_count: None,
                columns: None,
                package_sha256: None,
                server_version: None,
                started_at: started,
                finished_at: finished,
                source_version: None,
            },
            false,
            "query_not_completed",
            stop.code,
        ),
    };
    store
        .record_query_receipt(task.id, worker, task.attempt, generation, &receipt)
        .await?;
    store
        .finish_task(
            task.id,
            worker,
            generation,
            passed,
            result_class,
            &error_code,
        )
        .await?;
    tracing::info!(task_id=%task.id, lease_generation=generation, event="query_finished", status=receipt.status,
        code=error_code.as_str());
    let telemetry_outcome = match receipt.status {
        "completed" => super::feature_events::Outcome::Success,
        "refused" => super::feature_events::Outcome::Refused,
        _ => super::feature_events::Outcome::Failure,
    };
    let telemetry_error = match receipt.status {
        "completed" => None,
        "refused" => Some(super::feature_events::ErrorCode::QueryRefused),
        _ => Some(super::feature_events::ErrorCode::QueryFailed),
    };
    feature_telemetry.finish(
        telemetry_outcome,
        telemetry_error,
        receipt.row_count.unwrap_or(0).max(0) as u64,
        u64::from(!passed),
    );
    Ok(())
}

async fn run(
    store: &Store,
    config: &WorkerConfig,
    task: &ClaimedTask,
    query: &TaskQuery,
) -> Result<Done, Stop> {
    let worker = config.worker_id.as_str();
    let admitted = admit_stored(query)?;
    let expected = expected(&query.snapshot)?;
    if store
        .cancel_requested(task.id, worker, task.lease_generation)
        .await
        .map_err(|_| failed("lease_lost"))?
    {
        return Err(failed("cancelled"));
    }
    let connection = config
        .query_connection(&query.connection_ref)
        .map_err(|_| failed("query_connection_unavailable"))?;
    let client = connect(&connection)
        .await
        .map_err(|_| failed("source_unavailable"))?;
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
    let (csv, columns, server_version, source_version) =
        read(client, query, expected, &admitted.field_names).await?;
    let rows = count_rows(&csv, columns.len()).ok_or_else(|| failed("query_result_unreadable"))?;
    // Source-schema overloads can return a set even when their names passed the single-row grammar.
    if admitted.single_row && rows > 1 {
        return Err(refused("query_not_single_row"));
    }
    if rows as u64 > query.row_limit {
        return Err(failed("query_too_many_rows"));
    }
    let columns = Value::Array(
        columns
            .into_iter()
            .map(|(name, kind)| json!({"name": name, "type": kind}))
            .collect(),
    );
    let package = package(
        task,
        query,
        &csv,
        rows,
        &columns,
        &server_version,
        source_version.as_ref(),
    )
    .map_err(|_| failed("query_package_failed"))?;
    retain(store, config, task, &package).await?;
    Ok(Done {
        rows: rows as i32,
        columns,
        package_sha256: hex::encode(Sha256::digest(&package)),
        server_version,
        source_version,
    })
}

/// A reader's connection settings. With its own certificate authority (a lab's cell) TLS is required:
/// `disable` is refused, and `prefer`, also the default when the string names no mode, becomes `require`, so a server
/// that answers "no TLS" ends the connection instead of continuing in plain text. Without one, the settings are the
/// string's own.
fn reader_config(reader: &ReaderConnection) -> Result<PostgresConfig> {
    let mut config = PostgresConfig::from_str(&reader.dsn).context("invalid query connection")?;
    config.connect_timeout(Duration::from_secs(10));
    config.application_name("enrichedlab-query");
    if reader.ca.is_some() {
        if config.get_ssl_mode() == SslMode::Disable {
            bail!("a connection with its own certificate authority must use TLS");
        }
        config.ssl_mode(SslMode::Require);
    }
    Ok(config)
}

/// A reader's connection. With its own certificate authority the connection trusts that authority alone, not the
/// process's, and checks the server's host name: a lab database's certificate can never be one another authority
/// signed, and the broker's own authority never vouches for a lab's server.
pub(crate) async fn connect(reader: &ReaderConnection) -> Result<Client> {
    let config = reader_config(reader)?;
    let client = if config.get_ssl_mode() == SslMode::Disable {
        let (client, connection) = config.connect(NoTls).await?;
        tokio::spawn(async move {
            let _ = connection.await;
        });
        client
    } else {
        let mut builder = TlsConnector::builder();
        if let Some(pem) = &reader.ca {
            builder.disable_built_in_roots(true);
            for block in super::config::pem_certificate_blocks(pem)? {
                builder.add_root_certificate(Certificate::from_pem(&block)?);
            }
        }
        let connector = MakeTlsConnector::new(builder.build()?);
        let (client, connection) = config.connect(connector).await?;
        tokio::spawn(async move {
            let _ = connection.await;
        });
        client
    };
    Ok(client)
}

/// Read the result inside one read-only repeatable-read transaction: the snapshot check or a live source's version, the
/// column types and the rows come from the same database snapshot.
async fn read(
    mut client: Client,
    query: &TaskQuery,
    expected: Expected<'_>,
    field_names: &[String],
) -> Result<(Vec<u8>, Vec<(String, String)>, String, Option<Value>), Stop> {
    let tx = client
        .build_transaction()
        .isolation_level(IsolationLevel::RepeatableRead)
        .read_only(true)
        .start()
        .await
        .map_err(database)?;
    let timeout = query.statement_timeout_ms;
    // The guard parsed the query with standard_conforming_strings on; pin it so a role or database default of off
    // cannot make the server end a string literal where the guard did not.
    tx.batch_execute(&format!(
        "SET LOCAL standard_conforming_strings = on; \
         SET LOCAL statement_timeout = {timeout}; SET LOCAL lock_timeout = 5000; \
         SET LOCAL idle_in_transaction_session_timeout = {}; SET LOCAL search_path = \"{}\"",
        timeout + 10_000,
        query.schema
    ))
    .await
    .map_err(database)?;
    // PostgreSQL reads `q.name` or `(expr).name` as the call `name(q)` when `name` is not a column, so a name the
    // guard collected after a qualifier must not name an unlisted function that one row argument reaches: a plain
    // function, aggregate or window function in pg_catalog or the source's schema whose first parameter takes a row
    // and whose others have defaults. The guard already left out the names the source lists.
    if !field_names.is_empty() {
        let called = tx
            .query_opt(FIELD_CALLS, &[&field_names, &query.schema])
            .await
            .map_err(database)?;
        if called.is_some() {
            return Err(refused("sql_refused_function"));
        }
    }
    let source_version = match expected {
        Expected::Manifest(expected_manifest) => {
            let manifest: String = tx
                .query_one("SELECT manifest_sha256 FROM snapshot_manifest", &[])
                .await
                .map_err(|_| refused("snapshot_not_found"))?
                .get(0);
            if manifest != expected_manifest {
                return Err(refused("snapshot_mismatch"));
            }
            None
        }
        Expected::Live(relation) => {
            // Exactly one row, read before the result in the same snapshot; it is data the database states, so the
            // database itself withholds a row over the bound, and the row is kept as an object, never interpreted.
            let rows = tx
                .query(
                    &format!(
                        "SELECT CASE WHEN octet_length(t) <= {MAX_VERSION_BYTES} THEN t END \
                         FROM (SELECT row_to_json(v)::text AS t FROM \"{relation}\" v LIMIT 2) x"
                    ),
                    &[],
                )
                .await
                .map_err(|_| refused("source_version_unreadable"))?;
            let [row] = rows.as_slice() else {
                return Err(refused("source_version_unreadable"));
            };
            let Some(text) = row.get::<_, Option<String>>(0) else {
                return Err(refused("source_version_too_large"));
            };
            let value: Value =
                serde_json::from_str(&text).map_err(|_| refused("source_version_unreadable"))?;
            if !value.is_object() {
                return Err(refused("source_version_unreadable"));
            }
            Some(value)
        }
    };
    let version: String = tx
        .query_one("SHOW server_version", &[])
        .await
        .map_err(database)?
        .get(0);
    let statement = tx.prepare(&query.admitted_sql).await.map_err(database)?;
    let columns: Vec<(String, String)> = statement
        .columns()
        .iter()
        .map(|c| (c.name().to_owned(), c.type_().name().to_owned()))
        .collect();
    let stream = tx
        .copy_out(&copy_statement(&query.admitted_sql, columns.len()))
        .await
        .map_err(database)?;
    futures_util::pin_mut!(stream);
    let mut csv = Vec::new();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(database)?;
        if csv.len() + chunk.len() > MAX_RESULT_BYTES {
            return Err(failed("query_result_too_large"));
        }
        csv.extend_from_slice(&chunk);
    }
    tx.rollback().await.map_err(database)?;
    Ok((csv, columns, version, source_version))
}

/// A function among `$1` that PostgreSQL could call with one row argument in field notation.
const FIELD_CALLS: &str = "SELECT p.proname::text FROM pg_catalog.pg_proc p \
     WHERE p.proname::text = ANY ($1::text[]) AND p.prokind <> 'p' \
       AND p.pronamespace IN ('pg_catalog'::regnamespace, \
                              (SELECT n.oid FROM pg_catalog.pg_namespace n WHERE n.nspname = $2)) \
       AND p.pronargs >= 1 AND p.pronargs - p.pronargdefaults <= 1 \
       AND (p.proargtypes[0] = ANY (ARRAY['record', '\"any\"', 'anyelement', 'anynonarray', 'anycompatible', \
                                          'anycompatiblenonarray']::regtype[]) \
            OR EXISTS (SELECT 1 FROM pg_catalog.pg_type t WHERE t.oid = p.proargtypes[0] AND t.typtype = 'c') \
            OR EXISTS (SELECT 1 FROM pg_catalog.pg_cast c JOIN pg_catalog.pg_type r ON r.oid = c.castsource \
                       WHERE c.casttarget = p.proargtypes[0] AND c.castcontext = 'i' AND r.typtype = 'c')) \
     LIMIT 1";

/// The code for a database error the lead can act on; anything else is a plain failure.
fn database(error: tokio_postgres::Error) -> Stop {
    let code = match error.code() {
        Some(c) if *c == SqlState::UNDEFINED_COLUMN => "query_column_unknown",
        Some(c) if *c == SqlState::UNDEFINED_TABLE => "query_relation_unknown",
        Some(c) if *c == SqlState::UNDEFINED_FUNCTION => "query_function_unknown",
        Some(c)
            if *c == SqlState::DATATYPE_MISMATCH || *c == SqlState::INVALID_TEXT_REPRESENTATION =>
        {
            "query_type_mismatch"
        }
        Some(c) if *c == SqlState::DIVISION_BY_ZERO => "query_division_by_zero",
        Some(c) if *c == SqlState::QUERY_CANCELED => "query_timeout",
        Some(c) if *c == SqlState::INSUFFICIENT_PRIVILEGE => "query_refused_by_database",
        Some(c) if *c == SqlState::GROUPING_ERROR => "query_grouping_invalid",
        Some(c) if *c == SqlState::AMBIGUOUS_COLUMN => "query_column_ambiguous",
        Some(c) if c.code().starts_with("42") => "query_invalid_for_source",
        Some(c) if c.code().starts_with("22") => "query_data_error",
        _ => "query_failed",
    };
    failed(code)
}

/// The COPY that writes a result as the package keeps it. A NULL is `NA` (unquoted) and a text value that is `NA` is
/// quoted, since PostgreSQL quotes a value equal to its null string. With one column an empty text value would be a
/// blank line, which CSV readers skip, so every value but a NULL is quoted there.
pub fn copy_statement(admitted_sql: &str, width: usize) -> String {
    let quote_all = if width == 1 { ", FORCE_QUOTE *" } else { "" };
    format!(
        "COPY (\n{admitted_sql}\n) TO STDOUT WITH (FORMAT csv, HEADER true, NULL 'NA'{quote_all})"
    )
}

/// Data rows in PostgreSQL's CSV output, after its header; every row must have the header's width.
pub fn count_rows(csv: &[u8], width: usize) -> Option<usize> {
    let mut reader = csv::ReaderBuilder::new().has_headers(true).from_reader(csv);
    if reader.headers().ok()?.len() != width {
        return None;
    }
    let mut rows = 0;
    for record in reader.records() {
        if record.ok()?.len() != width {
            return None;
        }
        rows += 1;
    }
    Some(rows)
}

/// Write the package and its checksum under the attempt's export folder, register both, and record the digest.
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
    write_private(&root.join(&payload), package).map_err(|_| failed("query_package_failed"))?;
    write_private(&root.join(&checksum), sum_line.as_bytes())
        .map_err(|_| failed("query_package_failed"))?;
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
        .map_err(|_| failed("query_package_not_registered"))?;
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
        .map_err(|_| failed("query_package_not_registered"))?;
    let (digest_root, digest_path, size, digest_sha) = (
        root.clone(),
        payload.clone(),
        package.len() as u64,
        sha.clone(),
    );
    let built = tokio::task::spawn_blocking(move || {
        digest::build(
            &digest_root,
            task_id(&digest_path),
            &digest_path,
            size,
            &digest_sha,
        )
    })
    .await
    .map_err(|_| failed("query_digest_failed"))?;
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

fn task_id(relative_path: &str) -> uuid::Uuid {
    relative_path
        .split('/')
        .nth(1)
        .and_then(|v| uuid::Uuid::parse_str(v).ok())
        .unwrap_or_default()
}

fn write_private(path: &Path, bytes: &[u8]) -> Result<()> {
    if let Some(parent) = path.parent() {
        crate::config::create_private_dir(parent)?;
    }
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode_private()
        .open(path)
        .map_err(|e| anyhow!("{e}"))?;
    file.write_all(bytes)?;
    file.sync_all()?;
    Ok(())
}

trait PrivateMode {
    fn mode_private(&mut self) -> &mut Self;
}

impl PrivateMode for std::fs::OpenOptions {
    fn mode_private(&mut self) -> &mut Self {
        use std::os::unix::fs::OpenOptionsExt;
        self.mode(0o600)
    }
}

#[cfg(test)]
#[path = "query_run_tests.rs"]
mod tests;
