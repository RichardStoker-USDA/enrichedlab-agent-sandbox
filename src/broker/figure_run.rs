//! The worker's render route. A figure version runs here, not in a sandbox: no model-written code runs,
//! only a reviewed plot template in the R render service. The worker reads the table from the retained package the
//! request names and checks both digests, sends the table's columns and rows with the chosen plot and settings to the
//! service, and packages what the service returns: the figure files, the template and shared R code, the exact
//! settings the template read, a record of the request, a README and `SHA256SUMS`. The table itself is not copied; the
//! package names it by digest. One receipt is recorded per attempt. A refusal by the service finishes the version with
//! the broker's approved words for the service's refusal code, so the person can change a setting; the service's own
//! words are never passed on, since they may quote what the request sent or what a template read.
use std::{
    fs::OpenOptions,
    io::{self, Read, Write},
    os::unix::fs::OpenOptionsExt,
    path::{Component, Path},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};

use base64::Engine;
use flate2::{Compression, GzBuilder};
use reqwest::{Client, StatusCode, redirect::Policy};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use time::OffsetDateTime;
use tokio::time::sleep;
use tokio_util::sync::CancellationToken;

use super::csv_records::{Record, Records};
use super::{
    config::WorkerConfig,
    digest,
    evidence_package::{Package, PackageError, normalize_member_path},
    kinds::Declaration,
    model::ClaimedTask,
    store::{FigureReceipt, PreviewOutcome, Store, TaskFigure},
};

/// The profile route that selects this path.
pub const ROUTE: &str = "server-render";
const PACKAGE_LABEL: &str = "Figure and its reproducibility package";
/// The kind this path declares for the drawings it packages.
pub const FIGURE_KIND: &str = "figure.r";

/// The drawings a figure version declares as R figures: every file a completed receipt lists for the package whose
/// SHA-256 is `package_sha256`. This path wrote both the package and the receipt, so the declaration is the broker's
/// own; `detail` is the version's record as `Store::task_figure_detail` returns it, or null for any other task.
pub fn declared_figures(detail: &Value, package_sha256: &str) -> Vec<Declaration> {
    detail["receipts"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|receipt| {
            receipt["status"] == "completed" && receipt["package_sha256"] == package_sha256
        })
        .flat_map(|receipt| receipt["files"].as_array().into_iter().flatten())
        .filter_map(|file| {
            Some(Declaration {
                kind: FIGURE_KIND,
                path: file["path"].as_str()?.to_owned(),
                sha256: file["sha256"].as_str()?.to_owned(),
            })
        })
        .collect()
}
const MAX_TABLE_BYTES: u64 = 64 * 1024 * 1024;
const MAX_ROWS: usize = 50_000;
const MAX_COLUMNS: usize = 64;
const MAX_RESPONSE_BYTES: usize = 160 * 1024 * 1024;
/// The parts of the service's answer a figure keeps, and what holding them may cost (`budgeted_json`). The rest of the
/// answer, such as the table it echoes back and its reproduction page, is read past without being held.
const ANSWER_FIELDS: &[&str] = &[
    "error",
    "refusal",
    "spec",
    "service_version",
    "image_digest",
    "png",
    "svg",
    "pdf",
    "tiff",
    "code",
    "common_code",
    "notes",
    "font_face",
];
const MAX_ANSWER_HELD: usize = 128 * 1024 * 1024;
const MAX_CATALOG_BYTES: usize = 1024 * 1024;
/// A message, a note and a column name in a message are cut at these many characters.
const MAX_MESSAGE_CHARS: usize = 300;
const MAX_SHOWN_NAME_CHARS: usize = 48;
const MAX_NOTES: usize = 32;
/// A receipt's notes as PostgreSQL stores them (`notes::text`) may hold at most this many bytes (migration 040).
const MAX_NOTES_BYTES: usize = 8192;
/// A kept refusal names at most this many columns and counts, in at most this many bytes as PostgreSQL stores it
/// (migration 058); a larger one gets the general words.
const MAX_REFUSAL_ITEMS: usize = 8;
const MAX_REFUSAL_BYTES: usize = 2048;
const RENDER_TIMEOUT: Duration = Duration::from_secs(150);
/// A preview's drawing must arrive within this; the API waits a little longer, and the preview expires after a minute.
const PREVIEW_RENDER_TIMEOUT: Duration = Duration::from_secs(35);
/// How often an idle worker looks for a waiting preview, and how often it ends expired ones and deletes old ones.
const PREVIEW_IDLE: Duration = Duration::from_millis(400);
const PREVIEW_SWEEP: Duration = Duration::from_secs(10);
/// The largest drawing a preview hands over.
pub const MAX_PREVIEW_SVG: usize = 2 * 1024 * 1024;

/// Why an attempt did not complete: `refused` when the request or its source stops it, `failed` otherwise. A stop the
/// render service gave carries the broker's message for it and, when the service named one the broker could check, its
/// refusal: `{"code", "columns", "counts"}`.
#[derive(Debug)]
pub struct Stop {
    pub(super) status: &'static str,
    pub(super) code: &'static str,
    pub(super) message: Option<String>,
    pub(super) refusal: Option<Value>,
}

impl Stop {
    pub fn code(&self) -> &'static str {
        self.code
    }
}

pub(super) fn refused(code: &'static str) -> Stop {
    Stop {
        status: "refused",
        code,
        message: None,
        refusal: None,
    }
}

pub(super) fn failed(code: &'static str) -> Stop {
    Stop {
        status: "failed",
        code,
        message: None,
        refusal: None,
    }
}

/// The render service's refusal codes (its README, "Refusals") and the broker's words for each. `{column}` takes the
/// refusal's next column, `{columns}` all its remaining columns and `{count}` its next count.
const REFUSALS: &[(&str, &str)] = &[
    (
        "column_missing",
        "A column this figure needs is not in the table.",
    ),
    (
        "column_not_in_table",
        "A column named in the settings is not in the table.",
    ),
    (
        "figure_unknown",
        "The figure service does not offer this figure.",
    ),
    (
        "matrix_too_small",
        "This figure needs at least {count} features and {count} samples with values.",
    ),
    (
        "no_complete_rows",
        "No row has numbers in all of {columns}.",
    ),
    (
        "no_numeric_values",
        "No numbers this figure can draw in {columns}.",
    ),
    ("no_rows", "The table has no rows to draw."),
    (
        "not_a_scan",
        "A Manhattan plot needs a genome-wide scan: this table has {count} rows and a scan has at least {count}.",
    ),
    (
        "render_timed_out",
        "Drawing took longer than {count} seconds.",
    ),
    (
        "repeated_pairs",
        "Each pair of {column} and {column} must appear once (repeated rows: {count}); summarize them in the analysis \
         first.",
    ),
    (
        "setting_invalid",
        "The figure service did not accept one of these settings.",
    ),
    ("setting_missing", "A setting this figure needs is missing."),
    (
        "surface_too_few_values",
        "A surface needs at least two values of {column} and two of {column}.",
    ),
    (
        "surface_too_many_values",
        "A surface draws at most {count} values along each axis; this table has {count} of {column} and {count} of \
         {column}.",
    ),
    (
        "too_few_complete_rows",
        "This figure needs at least {count} complete rows.",
    ),
    (
        "too_few_groups",
        "This figure needs at least {count} groups with values.",
    ),
    (
        "too_few_samples",
        "This figure needs at least {count} samples with values.",
    ),
    (
        "too_few_varying_columns",
        "This figure needs at least {count} numeric columns whose values vary.",
    ),
    (
        "too_many_rows",
        "The table has {count} rows, more than the {count} the figure service takes.",
    ),
];
const SERVICE_REFUSED: &str = "The figure service refused these settings.";
const NOT_DRAWN: &str = "The figure service could not draw this table with these settings.";

/// A stop for a render service answer that carries an error. The service's words may quote what the
/// request sent, or a value a template read, so they are never passed on. Its refusal, when the broker knows the code
/// and every column it names is a column of `columns` and every count a whole number up to 10^9, chooses the broker's
/// message and fills it; otherwise the general message for the answer is used. `request_refused` is the service's
/// HTTP 400, which refuses the request before anything is drawn.
fn service_stop(out: &Value, columns: &[String], request_refused: bool) -> Stop {
    let approved = approved_refusal(&out["refusal"], columns);
    let (status, code, general) = match (request_refused, approved.is_some()) {
        (true, _) => ("refused", "figure_refused", SERVICE_REFUSED),
        (false, true) => ("refused", "figure_refused", NOT_DRAWN),
        (false, false) => ("failed", "figure_render_failed", NOT_DRAWN),
    };
    let (message, refusal) = match approved {
        Some((message, refusal)) => (message, Some(refusal)),
        None => (general.to_owned(), None),
    };
    Stop {
        status,
        code,
        message: Some(message),
        refusal,
    }
}

/// The broker's message for a refusal and the refusal as checked, or none when it is not one the broker can show.
fn approved_refusal(refusal: &Value, columns: &[String]) -> Option<(String, Value)> {
    approve(REFUSALS, refusal, columns)
}

/// A refusal as `approved_refusal` checks it, with the words from `table`: none unless the code is in the table, every
/// column it names is one of `columns` and every count a whole number up to 10^9, they fill the words exactly, and the
/// refusal fits what a receipt keeps (at most eight columns and eight counts, 2,048 bytes as stored).
pub(super) fn approve(
    table: &[(&str, &str)],
    refusal: &Value,
    columns: &[String],
) -> Option<(String, Value)> {
    let code = refusal["code"].as_str()?;
    let words = table.iter().find(|(known, _)| *known == code)?.1;
    let named: Vec<&str> = refusal["columns"]
        .as_array()
        .filter(|named| named.len() <= MAX_REFUSAL_ITEMS)?
        .iter()
        .map(|name| {
            name.as_str()
                .filter(|name| columns.iter().any(|c| c == name))
        })
        .collect::<Option<_>>()?;
    let counts: Vec<u64> = refusal["counts"]
        .as_array()
        .filter(|counts| counts.len() <= MAX_REFUSAL_ITEMS)?
        .iter()
        .map(|count| count.as_u64().filter(|count| *count <= 1_000_000_000))
        .collect::<Option<_>>()?;
    let message = fill(words, &named, &counts)?;
    let kept = json!({"code": code, "columns": named, "counts": counts});
    (stored_bytes(&kept) <= MAX_REFUSAL_BYTES).then_some((message, kept))
}

/// The bytes PostgreSQL's text form of a `jsonb` value takes (`value::text`): the compact JSON with a space after each
/// `:` and `,` between items, which is how it prints them. Both escape the same characters the same way.
pub fn stored_bytes(value: &Value) -> usize {
    fn separators(value: &Value) -> usize {
        match value {
            Value::Array(items) => {
                items.len().saturating_sub(1) + items.iter().map(separators).sum::<usize>()
            }
            Value::Object(map) => {
                map.len()
                    + map.len().saturating_sub(1)
                    + map.values().map(separators).sum::<usize>()
            }
            _ => 0,
        }
    }
    serde_json::to_string(value).map_or(usize::MAX, |text| text.len()) + separators(value)
}

/// Notes as a receipt keeps them: in order, at most 32, stopping before the one that would take the stored list past
/// `max_bytes`.
pub fn kept_notes(notes: impl IntoIterator<Item = String>, max_bytes: usize) -> Vec<String> {
    let mut kept: Vec<String> = Vec::new();
    for note in notes.into_iter().take(MAX_NOTES) {
        kept.push(note);
        if stored_bytes(&json!(kept)) > max_bytes {
            kept.pop();
            break;
        }
    }
    kept
}

/// A message with its places filled. None when the columns or counts do not fit the places exactly.
fn fill(words: &str, columns: &[&str], counts: &[u64]) -> Option<String> {
    let (mut columns, mut counts) = (columns.iter(), counts.iter());
    let (mut out, mut rest) = (String::new(), words);
    while let Some(at) = rest.find('{') {
        out.push_str(&rest[..at]);
        rest = &rest[at..];
        if let Some(after) = rest.strip_prefix("{columns}") {
            let names: Vec<String> = columns.by_ref().map(|name| shown_name(name)).collect();
            out.push_str(&listed(&names)?);
            rest = after;
        } else if let Some(after) = rest.strip_prefix("{column}") {
            out.push_str(&shown_name(columns.next()?));
            rest = after;
        } else if let Some(after) = rest.strip_prefix("{count}") {
            out.push_str(&counts.next()?.to_string());
            rest = after;
        } else {
            return None;
        }
    }
    out.push_str(rest);
    if columns.next().is_some() || counts.next().is_some() {
        return None;
    }
    Some(cut(&out, MAX_MESSAGE_CHARS))
}

/// "a", "a and b", "a, b and c"; none for no names.
fn listed(names: &[String]) -> Option<String> {
    let (last, first) = names.split_last()?;
    Some(if first.is_empty() {
        last.clone()
    } else {
        format!("{} and {last}", first.join(", "))
    })
}

/// A column name as a message shows it: without control or `Bidi_Control` characters, and cut at 48 characters.
pub(super) fn shown_name(name: &str) -> String {
    let kept: String = name
        .chars()
        .filter(|&c| !c.is_control() && !super::live_output::is_bidi_control(c))
        .collect();
    cut(&kept, MAX_SHOWN_NAME_CHARS)
}

/// Text cut to `max` characters, the last of them an ellipsis when it was cut.
pub(super) fn cut(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        return text.to_owned();
    }
    let mut out: String = text.chars().take(max - 1).collect();
    out.push('\u{2026}');
    out
}

struct Done {
    files: Value,
    notes: Value,
    service_version: String,
    image_digest: Option<String>,
    package_sha256: String,
}

/// What the service drew, as its response names it.
struct Drawn {
    png: Vec<u8>,
    svg: Vec<u8>,
    pdf: Option<Vec<u8>>,
    tiff: Option<Vec<u8>>,
    template: String,
    common: String,
    settings: Value,
    service_version: String,
    image_digest: Option<String>,
    notes: Vec<String>,
    /// The typeface R drew the text with, as the service reports it.
    font_face: Option<String>,
}

const MAX_FONT_FACE_CHARS: usize = 120;

/// A typeface name short enough to show and without control characters; any other is left out.
fn plain_face(face: &str) -> bool {
    face.chars().count() <= MAX_FONT_FACE_CHARS && !face.chars().any(char::is_control)
}

/// The settings the template read, and the typeface it drew with when the service names one. `code/params.json` keeps
/// only what the template read.
fn settings_used(drawn: &Drawn) -> Value {
    let mut used = drawn.settings["params"].clone();
    if let (Some(face), Some(map)) = (&drawn.font_face, used.as_object_mut()) {
        map.entry("font_face").or_insert_with(|| json!(face));
    }
    used
}

pub(super) fn client() -> Result<Client, Stop> {
    let _ = rustls::crypto::ring::default_provider().install_default();
    Client::builder()
        .redirect(Policy::none())
        .connect_timeout(Duration::from_secs(5))
        .build()
        .map_err(|_| failed("render_service_unavailable"))
}

/// The service's `/features` answer: every feature it offers, with its version and image digest.
pub(super) async fn fetch_catalog(config: &WorkerConfig) -> Result<Value, Stop> {
    let base = config
        .render_url
        .as_deref()
        .ok_or_else(|| failed("render_service_unconfigured"))?;
    let response = client()?
        .get(format!("{base}/features"))
        .timeout(Duration::from_secs(20))
        .send()
        .await
        .map_err(|_| failed("render_service_unavailable"))?;
    if response.status() != StatusCode::OK {
        return Err(failed("render_service_unavailable"));
    }
    let body = limited(response, MAX_CATALOG_BYTES).await?;
    serde_json::from_slice(&body).map_err(|_| failed("render_catalog_unreadable"))
}

/// Read the service's figure catalog and keep it for the API and the editor. Only figure features are kept, each with
/// its label, description and declared settings.
pub async fn refresh_catalog(store: &Store, config: &WorkerConfig) -> Result<String, Stop> {
    let catalog = fetch_catalog(config).await?;
    let (version, image_digest, features) = catalog_features(&catalog)?;
    store
        .record_render_catalog(
            &config.worker_id,
            &version,
            image_digest.as_deref(),
            &features,
        )
        .await
        .map_err(|_| failed("render_catalog_not_recorded"))
}

/// The figure features of a `/features` response in the stored shape, with the service's version and image digest.
fn catalog_features(catalog: &Value) -> Result<(String, Option<String>, Value), Stop> {
    let version = service_version(catalog).ok_or_else(|| failed("render_service_outdated"))?;
    let image_digest = catalog["image_digest"]
        .as_str()
        .filter(|d| is_image_digest(d))
        .map(str::to_owned);
    let features = catalog["features"]
        .as_array()
        .ok_or_else(|| failed("render_catalog_unreadable"))?
        .iter()
        .filter(|f| f["kind"].as_str().is_none_or(|k| k == "figure"))
        .map(|f| {
            json!({"id": f["id"], "label": f["label"], "description": f["description"].as_str().unwrap_or(""),
                "params": f["params"]})
        })
        .collect::<Vec<_>>();
    if features.is_empty() {
        return Err(failed("render_catalog_unreadable"));
    }
    Ok((version, image_digest, Value::Array(features)))
}

pub(super) fn service_version(value: &Value) -> Option<String> {
    value["service_version"]
        .as_str()
        .filter(|v| {
            (1..=32).contains(&v.len())
                && v.bytes().next().is_some_and(|b| b.is_ascii_alphanumeric())
                && v.bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b"+-.".contains(&b))
        })
        .map(str::to_owned)
}

pub(super) fn is_image_digest(value: &str) -> bool {
    value.len() == 71
        && value.starts_with("sha256:")
        && value[7..]
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

/// Read a response body, refusing one larger than the limit.
pub(super) async fn limited(
    mut response: reqwest::Response,
    limit: usize,
) -> Result<Vec<u8>, Stop> {
    let mut body = Vec::new();
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|_| failed("render_service_unavailable"))?
    {
        if body.len() + chunk.len() > limit {
            return Err(failed("render_response_too_large"));
        }
        body.extend_from_slice(&chunk);
    }
    Ok(body)
}

/// Run one claimed figure version to a finished state, or leave it to lease recovery when the lease or the worker stops.
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
        super::feature_events::Feature::Figure,
        &task,
    );
    let Some(request) = store
        .task_figure(task.id, worker, task.attempt, generation)
        .await?
    else {
        store
            .finish_task(
                task.id,
                worker,
                generation,
                false,
                "figure_not_drawn",
                "figure_not_recorded",
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
    let (receipt, passed, result_class, error_code) = match outcome {
        Ok(done) => (
            FigureReceipt {
                status: "completed",
                error_code: None,
                message: None,
                service_version: Some(done.service_version),
                image_digest: done.image_digest,
                files: Some(done.files),
                notes: done.notes,
                package_sha256: Some(done.package_sha256),
                started_at: started,
                finished_at: finished,
            },
            true,
            "figure_completed",
            String::new(),
        ),
        Err(stop) => (
            FigureReceipt {
                status: stop.status,
                error_code: Some(stop.code.to_owned()),
                message: stop.message,
                service_version: None,
                image_digest: None,
                files: None,
                notes: json!([]),
                package_sha256: None,
                started_at: started,
                finished_at: finished,
            },
            false,
            "figure_not_completed",
            stop.code.to_owned(),
        ),
    };
    store
        .record_figure_receipt(task.id, worker, task.attempt, generation, &receipt)
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
    tracing::info!(task_id=%task.id, lease_generation=generation, event="figure_finished", status=receipt.status,
        code=error_code.as_str());
    let telemetry_outcome = match receipt.status {
        "completed" => super::feature_events::Outcome::Success,
        "refused" => super::feature_events::Outcome::Refused,
        _ => super::feature_events::Outcome::Failure,
    };
    feature_telemetry.finish(
        telemetry_outcome,
        (!passed).then_some(super::feature_events::ErrorCode::FigureFailed),
        u64::from(passed),
        u64::from(!passed),
    );
    Ok(())
}

async fn run(
    store: &Store,
    config: &WorkerConfig,
    task: &ClaimedTask,
    request: &TaskFigure,
) -> Result<Done, Stop> {
    let worker = config.worker_id.as_str();
    let base = config
        .render_url
        .clone()
        .ok_or_else(|| failed("render_service_unconfigured"))?;
    let root = config.artifact_root.clone();
    let source = SourceTable::from(request);
    let table = tokio::task::spawn_blocking(move || read_table(&root, &source))
        .await
        .map_err(|_| failed("figure_source_unreadable"))??;
    let (columns, rows) = parse_table(&table)?;
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
    // The catalog is read again with each version so the editor offers what the running service declares.
    if let Err(stop) = refresh_catalog(store, config).await {
        tracing::warn!(task_id=%task.id, event="render_catalog_not_read", code=stop.code);
    }
    let drawn = draw(&base, request, &columns, rows, RENDER_TIMEOUT).await?;
    let (package, files, notes) =
        package(task, request, &drawn).map_err(|_| failed("figure_package_failed"))?;
    retain(store, config, task, &package, PACKAGE_LABEL).await?;
    if let Some(image) = drawn.image_digest.as_deref() {
        let template = format!(
            "sha256:{}",
            hex::encode(Sha256::digest(drawn.template.as_bytes()))
        );
        let template_ref = format!("render-service/templates/{}.R", request.feature);
        let tool_ref = format!("render-service@{}", drawn.service_version);
        let settings = format!(
            "sha256:{}",
            hex::encode(Sha256::digest(settings_bytes(&drawn.settings)))
        );
        if let Err(e) = store
            .record_attempt_execution(
                task.id,
                worker,
                task.lease_generation,
                image,
                Some((template_ref.as_str(), template.as_str())),
                Some(tool_ref.as_str()),
                &settings,
            )
            .await
        {
            tracing::warn!(task_id=%task.id, event="figure_execution_not_recorded", code=e.code);
        }
    }
    Ok(Done {
        files,
        notes,
        service_version: drawn.service_version,
        image_digest: drawn.image_digest,
        package_sha256: hex::encode(Sha256::digest(&package)),
    })
}

/// Where a request's table is: the source package's place, size and digest, and the table's path and digest in it.
#[derive(Clone, Debug)]
pub(super) struct SourceTable {
    pub(super) package_path: String,
    pub(super) package_bytes: i64,
    pub(super) package_sha256: String,
    pub(super) table_path: String,
    pub(super) table_sha256: String,
}

impl From<&TaskFigure> for SourceTable {
    fn from(request: &TaskFigure) -> Self {
        SourceTable {
            package_path: request.package_path.clone(),
            package_bytes: request.package_bytes,
            package_sha256: request.package_sha256.clone(),
            table_path: request.table_path.clone(),
            table_sha256: request.table_sha256.clone(),
        }
    }
}

/// The table's bytes from the source package, after checking the package's size and digest and the table's digest.
pub(super) fn read_table(root: &Path, request: &SourceTable) -> Result<Vec<u8>, Stop> {
    read_member(
        root,
        &request.package_path,
        request.package_bytes,
        &request.package_sha256,
        &request.table_path,
        MAX_TABLE_BYTES,
    )
    .and_then(|bytes| {
        if hex::encode(Sha256::digest(&bytes)) != request.table_sha256 {
            Err(MemberError::Changed)
        } else {
            Ok(bytes)
        }
    })
    .map_err(|e| match e {
        MemberError::Unreadable { refused: true } => refused("figure_source_unreadable"),
        MemberError::Unreadable { refused: false } => failed("figure_source_unreadable"),
        MemberError::Changed => refused("figure_source_changed"),
        MemberError::TooLarge => refused("figure_table_too_large"),
        MemberError::Missing => refused("figure_table_missing"),
    })
}

/// Why a package member could not be read: `refused` when the request named something the store cannot serve.
/// `TooLarge` is a member past its own limit or the members read together past theirs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum MemberError {
    Unreadable { refused: bool },
    Changed,
    TooLarge,
    Missing,
}

/// One member's bytes from a retained package under `root`, after checking the package's size and digest. The member
/// may hold at most `max` bytes.
pub(super) fn read_member(
    root: &Path,
    package_path: &str,
    package_bytes: i64,
    package_sha256: &str,
    member: &str,
    max: u64,
) -> Result<Vec<u8>, MemberError> {
    let wanted = [(member.to_owned(), max)];
    let budget = Budget {
        total: u64::MAX,
        scan: u64::MAX,
        stop: None,
    };
    let (mut read, _) = read_members(
        root,
        package_path,
        package_bytes,
        package_sha256,
        &wanted,
        &budget,
    )?;
    read.remove(member).ok_or(MemberError::Missing)
}

/// What one pass over a package may do: hold at most `total` member bytes, decompress at most `scan` bytes of the
/// package's stream to reach them (every header, extension body, padding and skipped body counts), and end at its next
/// read once `stop` is set. A stopped pass's error is never reported: its caller has stopped waiting for it.
pub(super) struct Budget<'a> {
    pub(super) total: u64,
    pub(super) scan: u64,
    pub(super) stop: Option<&'a AtomicBool>,
}

/// A reader that fails once its stop flag is set.
struct Stoppable<'a, R>(R, Option<&'a AtomicBool>);

impl<R: Read> Read for Stoppable<'_, R> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if self.1.is_some_and(|stop| stop.load(Ordering::Relaxed)) {
            return Err(io::Error::other("stopped"));
        }
        self.0.read(buf)
    }
}

/// Several members' bytes from one retained package under `root`, in one pass after checking the package's size and
/// digest once, with how many expanded bytes the pass went through. Each member may hold at most its own limit and the
/// pass stays within `budget`; every member named must be there.
pub(super) fn read_members(
    root: &Path,
    package_path: &str,
    package_bytes: i64,
    package_sha256: &str,
    wanted: &[(String, u64)],
    budget: &Budget<'_>,
) -> Result<(std::collections::HashMap<String, Vec<u8>>, u64), MemberError> {
    let relative = Path::new(package_path);
    if relative.is_absolute()
        || relative
            .components()
            .any(|c| !matches!(c, Component::Normal(_)))
    {
        return Err(MemberError::Unreadable { refused: true });
    }
    let path = root.join(relative);
    let open = || {
        OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW)
            .open(&path)
            .map_err(|_| MemberError::Unreadable { refused: false })
    };
    let mut file = open()?;
    let size = file
        .metadata()
        .map_err(|_| MemberError::Unreadable { refused: false })?
        .len();
    if i64::try_from(size).ok() != Some(package_bytes) {
        return Err(MemberError::Changed);
    }
    let mut hasher = Sha256::new();
    io::copy(
        &mut Stoppable(&mut file, budget.stop),
        &mut HashWriter(&mut hasher),
    )
    .map_err(|_| MemberError::Unreadable { refused: false })?;
    if hex::encode(hasher.finalize()) != package_sha256 {
        return Err(MemberError::Changed);
    }
    // The decompressed stream stops at the budget; a read past it fails as too large.
    let mut package = Package::within(Stoppable(open()?, budget.stop), budget.scan);
    let mut entries = package
        .members()
        .map_err(|_| MemberError::Unreadable { refused: true })?;
    let failed = |error: PackageError| match error {
        PackageError::TooLarge => MemberError::TooLarge,
        _ => MemberError::Unreadable { refused: true },
    };
    let (mut read, mut left) = (std::collections::HashMap::new(), budget.total);
    while let Some((raw_path, mut entry)) = entries.next_member().map_err(failed)? {
        // An entry is expanded to reach the next one, so it is charged before any of it is read.
        if entries.expanded().saturating_add(entry.size()) > budget.scan {
            return Err(MemberError::TooLarge);
        }
        if !entry.header().entry_type().is_file() {
            continue;
        }
        let Some(path) = normalize_member_path(&raw_path) else {
            continue;
        };
        // The first member of a name is the one read, as a single read takes it.
        let Some(&(_, max)) = wanted.iter().find(|(name, _)| *name == path) else {
            continue;
        };
        if read.contains_key(&path) {
            continue;
        }
        if entry.size() > max {
            return Err(MemberError::TooLarge);
        }
        left = left
            .checked_sub(entry.size())
            .ok_or(MemberError::TooLarge)?;
        let mut bytes = Vec::with_capacity(entry.size() as usize);
        entry
            .read_to_end(&mut bytes)
            .map_err(|_| failed(entries.failure()))?;
        read.insert(path, bytes);
        if wanted.iter().all(|(name, _)| read.contains_key(name)) {
            return Ok((read, entries.expanded()));
        }
    }
    Err(MemberError::Missing)
}

struct HashWriter<'a>(&'a mut Sha256);

impl Write for HashWriter<'_> {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.0.update(bytes);
        Ok(bytes.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

/// The table's column names and rows as the service takes them: every cell as the text the file holds, an empty one as
/// empty text. The service writes the cells into the `data.csv` its template reads with R's `read.csv`, so the template
/// reads what `read.csv` reads from the table itself: `NA` as missing, and an empty cell as missing in a numeric column
/// and as empty text in a text column. A header wider than the service draws is refused before its fields are read
/// further.
pub(super) fn parse_table(bytes: &[u8]) -> Result<(Vec<String>, Vec<Vec<Value>>), Stop> {
    let unreadable = |_| refused("figure_table_unreadable");
    let text = |field: &[u8]| {
        std::str::from_utf8(field)
            .map(str::to_owned)
            .map_err(|_| refused("figure_table_unreadable"))
    };
    let mut records = Records::new(bytes, b',', MAX_COLUMNS, bytes.len());
    let columns: Vec<String> = match records.read_record().map_err(unreadable)? {
        Some(Record::Fields(_)) => records.fields().map(text).collect::<Result<_, _>>()?,
        None | Some(_) => return Err(refused("figure_table_unsuitable")),
    };
    let mut rows = Vec::new();
    while let Some(record) = records.read_record().map_err(unreadable)? {
        if record != Record::Fields(columns.len()) {
            return Err(refused("figure_table_unreadable"));
        }
        if rows.len() == MAX_ROWS {
            return Err(refused("figure_table_unsuitable"));
        }
        rows.push(
            records
                .fields()
                .map(|cell| Ok(Value::String(text(cell)?)))
                .collect::<Result<_, Stop>>()?,
        );
    }
    if rows.is_empty() {
        return Err(refused("figure_table_unsuitable"));
    }
    Ok((columns, rows))
}

/// Ask the service to draw the figure and check what it returned.
async fn draw(
    base: &str,
    request: &TaskFigure,
    columns: &[String],
    rows: Vec<Vec<Value>>,
    timeout: Duration,
) -> Result<Drawn, Stop> {
    let mut body = json!({"feature_id": request.feature, "params": request.controls, "columns": columns, "rows": rows});
    if !request.title.is_empty() {
        body["title"] = json!(request.title);
    }
    let response = client()?
        .post(format!("{base}/render"))
        .timeout(timeout)
        .json(&body)
        .send()
        .await
        .map_err(|_| failed("render_service_unavailable"))?;
    let status = response.status();
    let bytes = limited(response, MAX_RESPONSE_BYTES).await?;
    let mut out =
        super::budgeted_json::read(&bytes, ANSWER_FIELDS, MAX_ANSWER_HELD).map_err(|refused| {
            failed(match refused {
                super::budgeted_json::Refused::TooLarge => "render_response_too_large",
                super::budgeted_json::Refused::Unreadable => "render_response_unreadable",
            })
        })?;
    drop(bytes);
    if status == StatusCode::BAD_REQUEST {
        return Err(service_stop(&out, columns, true));
    }
    if status != StatusCode::OK {
        return Err(failed("render_service_unavailable"));
    }
    if out["error"].as_str().is_some_and(|m| !m.trim().is_empty()) || out["refusal"].is_object() {
        return Err(service_stop(&out, columns, false));
    }
    let settings = out
        .get_mut("spec")
        .map(Value::take)
        .filter(Value::is_object)
        .ok_or_else(|| failed("render_service_outdated"))?;
    let text = |key: &str| {
        out[key]
            .as_str()
            .filter(|v| !v.is_empty())
            .map(str::to_owned)
    };
    let decode = |key: &str| -> Result<Option<Vec<u8>>, Stop> {
        match out[key].as_str().filter(|v| !v.is_empty()) {
            None => Ok(None),
            Some(v) => base64::engine::general_purpose::STANDARD
                .decode(v)
                .map(Some)
                .map_err(|_| failed("render_response_unreadable")),
        }
    };
    let service_version = service_version(&out).ok_or_else(|| failed("render_service_outdated"))?;
    let png = decode("png")?.ok_or_else(|| failed("render_response_unreadable"))?;
    let svg = text("svg").ok_or_else(|| failed("render_response_unreadable"))?;
    let pdf = decode("pdf")?;
    let tiff = decode("tiff")?;
    if !png.starts_with(b"\x89PNG\r\n\x1a\n")
        || !svg.contains("<svg")
        || pdf.as_ref().is_some_and(|b| !b.starts_with(b"%PDF-"))
        || tiff
            .as_ref()
            .is_some_and(|b| !b.starts_with(b"II*\0") && !b.starts_with(b"MM\0*"))
    {
        return Err(failed("render_response_unreadable"));
    }
    Ok(Drawn {
        png,
        svg: svg.into_bytes(),
        pdf,
        tiff,
        template: text("code").ok_or_else(|| failed("render_response_unreadable"))?,
        common: text("common_code").ok_or_else(|| failed("render_service_outdated"))?,
        settings,
        service_version,
        image_digest: text("image_digest").filter(|d| is_image_digest(d)),
        notes: kept_notes(
            out["notes"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(Value::as_str)
                .map(|t| t.chars().take(MAX_MESSAGE_CHARS).collect()),
            MAX_NOTES_BYTES,
        ),
        font_face: text("font_face").filter(|face| plain_face(face)),
    })
}

/// Draws one claimed preview: the table from its retained package, then the drawing from the render
/// service, of which a preview keeps only the SVG. Nothing is written: no package, receipt or task.
pub async fn draw_preview(config: &WorkerConfig, request: &TaskFigure) -> PreviewOutcome {
    let drawn = async {
        let base = config
            .render_url
            .clone()
            .ok_or_else(|| failed("render_service_unconfigured"))?;
        let root = config.artifact_root.clone();
        let source = SourceTable::from(request);
        let table = tokio::task::spawn_blocking(move || read_table(&root, &source))
            .await
            .map_err(|_| failed("figure_source_unreadable"))??;
        let (columns, rows) = parse_table(&table)?;
        draw(&base, request, &columns, rows, PREVIEW_RENDER_TIMEOUT).await
    }
    .await;
    match drawn {
        Ok(drawn) if drawn.svg.len() > MAX_PREVIEW_SVG => PreviewOutcome {
            state: "failed",
            svg: None,
            result: json!({"code": "figure_preview_too_large"}),
        },
        Ok(drawn) => PreviewOutcome {
            state: "drawn",
            result: json!({"notes": drawn.notes, "settings_used": settings_used(&drawn),
                "service_version": drawn.service_version, "image_digest": drawn.image_digest,
                "catalog_sha256": request.catalog_sha256, "table_sha256": request.table_sha256}),
            svg: Some(drawn.svg),
        },
        // A stop with a message to show, the service's refusal or a drawing it could not make, reads as refused.
        Err(stop) if stop.status == "refused" || stop.message.is_some() => PreviewOutcome {
            state: "refused",
            svg: None,
            result: json!({"code": stop.code, "refused": stop.message, "refusal": stop.refusal}),
        },
        Err(stop) => PreviewOutcome {
            state: "failed",
            svg: None,
            result: json!({"code": stop.code}),
        },
    }
}

/// Serves previews until `shutdown`: claims the oldest waiting one, draws it and records what it came to, and every ten
/// seconds ends expired previews and deletes old ones. Only codes and states are logged, never a drawing or a setting.
pub async fn serve_previews(store: Store, config: Arc<WorkerConfig>, shutdown: CancellationToken) {
    let mut swept: Option<tokio::time::Instant> = None;
    loop {
        if shutdown.is_cancelled() {
            break;
        }
        if swept.is_none_or(|at| at.elapsed() >= PREVIEW_SWEEP) {
            if let Err(error) = store.sweep_figure_previews().await {
                tracing::warn!(event = "figure_previews_not_swept", code = error.code);
            }
            swept = Some(tokio::time::Instant::now());
        }
        let wait = match store.claim_figure_preview(&config.worker_id).await {
            Ok(Some((preview, request))) => {
                let outcome = tokio::select! {
                    outcome = draw_preview(&config, &request) => outcome,
                    () = shutdown.cancelled() => break,
                };
                let code = outcome.result["code"]
                    .as_str()
                    .unwrap_or_default()
                    .to_owned();
                match store
                    .finish_figure_preview(&config.worker_id, preview, &outcome)
                    .await
                {
                    Ok(true) => tracing::info!(
                        event = "figure_preview_finished",
                        state = outcome.state,
                        code = code.as_str()
                    ),
                    // The person left or the preview expired; the drawing is not kept.
                    Ok(false) => tracing::info!(
                        event = "figure_preview_ended_before_recording",
                        state = outcome.state
                    ),
                    Err(error) => {
                        tracing::warn!(event = "figure_preview_not_recorded", code = error.code)
                    }
                }
                Duration::ZERO
            }
            Ok(None) => PREVIEW_IDLE,
            Err(error) => {
                tracing::warn!(event = "figure_preview_claim_failed", code = error.code);
                Duration::from_secs(5)
            }
        };
        if !wait.is_zero() {
            tokio::select! {
                () = shutdown.cancelled() => break,
                () = sleep(wait) => {}
            }
        }
    }
}

/// The exact settings the template read, as the file a local rerun gives it.
pub(super) fn settings_bytes(settings: &Value) -> Vec<u8> {
    let mut bytes = serde_json::to_vec_pretty(settings).unwrap_or_default();
    bytes.push(b'\n');
    bytes
}

/// A deterministic tar+gzip of the figure, its code and records: sorted members, fixed owner, mode and time.
fn package(
    task: &ClaimedTask,
    request: &TaskFigure,
    drawn: &Drawn,
) -> anyhow::Result<(Vec<u8>, Value, Value)> {
    let template_path = format!("code/{}.R", request.feature);
    let mut figures: Vec<(String, Vec<u8>, &str)> = vec![
        ("figures/figure.png".into(), drawn.png.clone(), "image/png"),
        (
            "figures/figure.svg".into(),
            drawn.svg.clone(),
            "image/svg+xml",
        ),
    ];
    if let Some(pdf) = &drawn.pdf {
        figures.push(("figures/figure.pdf".into(), pdf.clone(), "application/pdf"));
    }
    if let Some(tiff) = &drawn.tiff {
        figures.push(("figures/figure.tiff".into(), tiff.clone(), "image/tiff"));
    }
    let files: Vec<Value> = figures
        .iter()
        .map(|(path, bytes, media)| {
            json!({"path": path, "sha256": hex::encode(Sha256::digest(bytes)), "size_bytes": bytes.len(), "media_type": media})
        })
        .collect();
    let settings = settings_bytes(&drawn.settings);
    let notes = Value::Array(drawn.notes.iter().map(|n| json!(n)).collect());
    let spec = json!({
        "schema": "enrichedlab.figure-spec.v1",
        "task_id": task.id,
        "feature": request.feature,
        "title": request.title,
        "controls": request.controls,
        "settings_used": settings_used(drawn),
        "notes": notes,
        "source": {"task_id": request.source_task, "package_sha256": request.package_sha256,
            "table_path": request.table_path, "table_sha256": request.table_sha256, "rows": request.table_rows},
        "service": {"version": drawn.service_version, "image_digest": drawn.image_digest},
        "code": {"template": {"path": template_path, "sha256": hex::encode(Sha256::digest(drawn.template.as_bytes()))},
            "shared": {"path": "code/_common.R", "sha256": hex::encode(Sha256::digest(drawn.common.as_bytes()))},
            "settings": {"path": "code/params.json", "sha256": hex::encode(Sha256::digest(&settings))}},
        "files": files,
        "request_sha256": request.request_sha256,
    });
    let mut members: Vec<(String, Vec<u8>)> = figures
        .into_iter()
        .map(|(path, bytes, _)| (path, bytes))
        .collect();
    members.push((template_path.clone(), drawn.template.clone().into_bytes()));
    members.push(("code/_common.R".into(), drawn.common.clone().into_bytes()));
    members.push(("code/params.json".into(), settings));
    members.push(("spec.json".into(), serde_json::to_vec_pretty(&spec)?));
    members.push((
        "README.md".into(),
        readme(request, &template_path, &drawn.service_version).into_bytes(),
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
    Ok((gz.finish()?, Value::Array(files), notes))
}

fn readme(request: &TaskFigure, template_path: &str, version: &str) -> String {
    let title = if request.title.is_empty() {
        "Figure".to_owned()
    } else {
        request.title.clone()
    };
    format!(
        "# {title}\n\nThis package holds one version of a figure drawn by the reviewed R render service ({version}) from \
         the table `{table}` of an earlier analysis. The table is not copied here: it is in that analysis's package, \
         and its SHA-256 is `{table_sha}`.\n\n- `figures/` holds the figure as PNG and SVG, and as PDF or TIFF when \
         one was asked for.\n- `code/` holds the plot template (`{template_path}`), the shared R code it reads \
         (`code/_common.R`) and the exact settings it read (`code/params.json`).\n- `spec.json` records the request, \
         the settings as asked for and as used, anything the service changed or left out, the source table and the \
         service's version.\n- `SHA256SUMS` lists every file's SHA-256.\n\n## Check the files\n\nmacOS: `shasum -a 256 -c \
         SHA256SUMS`  \nLinux: `sha256sum -c SHA256SUMS`\n\n## Draw it again\n\nCopy the table from the earlier \
         analysis's package into a new folder as `data.csv`, and `code/params.json` into the same folder as \
         `params.json`. Then run `Rscript {template_path} <that folder>` with R 4.4 and the packages ggplot2, ragg, \
         svglite, scales and jsonlite. The figure is written to that folder as `figure.png` and `figure.svg`.\n",
        table = request.table_path,
        table_sha = request.table_sha256,
    )
}

/// Write the package and its checksum under the attempt's export folder, register both under `label`, and record the
/// digest.
pub(super) async fn retain(
    store: &Store,
    config: &WorkerConfig,
    task: &ClaimedTask,
    package: &[u8],
    label: &str,
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
    write_private(&root.join(&payload), package).map_err(|_| failed("figure_package_failed"))?;
    write_private(&root.join(&checksum), sum_line.as_bytes())
        .map_err(|_| failed("figure_package_failed"))?;
    store
        .register_artifact(
            task.id,
            worker,
            generation,
            "researcher",
            "result",
            label,
            &payload,
            "application/gzip",
            package.len() as i64,
            &sha,
        )
        .await
        .map_err(|_| failed("figure_package_not_registered"))?;
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
        .map_err(|_| failed("figure_package_not_registered"))?;
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
    .map_err(|_| failed("figure_digest_failed"))?;
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
        // A digest that cannot be recorded leaves a gap the page reports; the figure itself is kept.
        tracing::warn!(task_id=%task.id, lease_generation=generation, event="result_digest_not_recorded", code=e.code);
    }
    Ok(())
}

pub(super) fn write_private(path: &Path, bytes: &[u8]) -> anyhow::Result<()> {
    if let Some(parent) = path.parent() {
        crate::config::create_private_dir(parent)?;
    }
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)?;
    file.write_all(bytes)?;
    file.sync_all()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_completed_receipt_declares_the_drawings_of_its_own_package() {
        let file =
            |path: &str, sha: char| json!({"path": path, "sha256": sha.to_string().repeat(64)});
        let detail = json!({"receipts": [
            {"status": "refused", "files": null, "package_sha256": null},
            {"status": "failed", "package_sha256": "a".repeat(64), "files": [file("figures/figure.tiff", 'f')]},
            {"status": "completed", "package_sha256": "a".repeat(64),
                "files": [file("figures/figure.svg", 'b'), file("figures/figure.png", 'c')]},
            {"status": "completed", "package_sha256": "d".repeat(64), "files": [file("figures/figure.pdf", 'e')]},
        ]});
        let declared = declared_figures(&detail, &"a".repeat(64));
        assert_eq!(
            declared
                .iter()
                .map(|d| (d.kind, d.path.as_str(), &d.sha256[..1]))
                .collect::<Vec<_>>(),
            [
                (FIGURE_KIND, "figures/figure.svg", "b"),
                (FIGURE_KIND, "figures/figure.png", "c")
            ]
        );
        assert!(declared_figures(&Value::Null, &"a".repeat(64)).is_empty());
        assert!(declared_figures(&detail, &"f".repeat(64)).is_empty());
    }

    fn request() -> TaskFigure {
        TaskFigure {
            source_task: uuid::Uuid::nil(),
            package_path: "tasks/x/attempts/1-1/exports/payload".into(),
            package_sha256: "a".repeat(64),
            package_bytes: 1,
            table_path: "results/query_result.csv".into(),
            table_sha256: "b".repeat(64),
            table_rows: 2,
            feature: "scatter".into(),
            controls: json!({"x": "dose", "y": "response", "x_label": "Dose (mg)", "legend_position": "bottom"}),
            title: "Response by dose".into(),
            request_sha256: "c".repeat(64),
            catalog_sha256: "d".repeat(64),
        }
    }

    fn task() -> ClaimedTask {
        ClaimedTask {
            id: uuid::Uuid::nil(),
            principal_id: uuid::Uuid::nil(),
            group_key: "lab".into(),
            profile_key: "figure-render".into(),
            objective: "o".into(),
            logical_agent: "a".into(),
            lease_generation: 1,
            attempt: 1,
            max_task_attempts: 1,
            max_provider_attempts: 1,
            harness: "pi".into(),
            thinking: None,
            route: ROUTE.into(),
            controller_profile: "disabled-legacy".into(),
            gateway_profile: "disabled-legacy".into(),
            image_ref: "render-service@sha256:x".into(),
            cpu_limit: "1".into(),
            memory_limit: "1Gi".into(),
            state_profile: "s".into(),
            role: "r".into(),
            max_run_seconds: 300,
        }
    }

    fn drawn() -> Drawn {
        Drawn {
            png: b"\x89PNG\r\n\x1a\nrest".to_vec(),
            svg: b"<svg xmlns='http://www.w3.org/2000/svg'></svg>".to_vec(),
            pdf: Some(b"%PDF-1.7".to_vec()),
            tiff: None,
            template: "source(file.path(dirname(.tf), \"_common.R\"))\n".into(),
            common: "WORK <- commandArgs(TRUE)[1]\n".into(),
            settings: json!({"params": {"x": "dose", "y": "response", "x_label": "Dose (mg)", "legend_position": "bottom"},
                "title": "Response by dose", "feature_id": "scatter"}),
            service_version: "0.5.20".into(),
            image_digest: Some(format!("sha256:{}", "e".repeat(64))),
            notes: vec!["'dpi' was 1200, above the 600 maximum; 600 was used".into()],
            font_face: Some("DejaVu Sans".into()),
        }
    }

    #[test]
    fn a_service_refusal_is_shown_in_the_brokers_words_with_only_columns_and_counts() {
        let columns = [
            "dose".to_owned(),
            "time".to_owned(),
            "a\u{202e}b\u{7}c".to_owned(),
        ];
        let stop = |out: Value, request_refused: bool| {
            let stop = service_stop(&out, &columns, request_refused);
            (stop.status, stop.code, stop.message.unwrap(), stop.refusal)
        };
        let words = "the value 'Patient 0042' repeats";
        let refusal = json!({"code": "repeated_pairs", "columns": ["dose", "time"], "counts": [2]});
        assert_eq!(
            stop(json!({"error": words, "refusal": refusal}), false),
            (
                "refused",
                "figure_refused",
                "Each pair of dose and time must appear once (repeated rows: 2); summarize them in the analysis first."
                    .to_owned(),
                Some(refusal.clone())
            )
        );
        // The service's own words never appear, whatever its refusal holds.
        for bad in [
            json!({"code": "repeated_pairs", "columns": ["Patient 0042", "time"], "counts": [2]}),
            json!({"code": "repeated_pairs", "columns": ["dose", "time"], "counts": [2, 3]}),
            json!({"code": "repeated_pairs", "columns": ["dose"], "counts": [2]}),
            json!({"code": "repeated_pairs", "columns": ["dose", "time"], "counts": [2.0]}),
            json!({"code": "repeated_pairs", "columns": ["dose", "time"], "counts": [-2]}),
            json!({"code": "repeated_pairs", "columns": ["dose", "time"], "counts": [1_000_000_001_u64]}),
            json!({"code": "a code the broker does not know", "columns": [], "counts": []}),
            json!({"code": "repeated_pairs", "columns": "dose", "counts": [2]}),
            json!("repeated_pairs"),
            Value::Null,
        ] {
            assert_eq!(
                stop(json!({"error": words, "refusal": bad.clone()}), false),
                ("failed", "figure_render_failed", NOT_DRAWN.to_owned(), None),
                "{bad}"
            );
            assert_eq!(
                stop(json!({"error": words, "refusal": bad.clone()}), true),
                (
                    "refused",
                    "figure_refused",
                    SERVICE_REFUSED.to_owned(),
                    None
                ),
                "{bad}"
            );
        }
        // Every place filled from its own list; a name without its controls and cut at 48 characters.
        let (_, _, message, _) = stop(
            json!({"error": words, "refusal": {"code": "no_complete_rows", "columns": ["dose", "time", "a\u{202e}b\u{7}c"], "counts": []}}),
            false,
        );
        assert_eq!(message, "No row has numbers in all of dose, time and abc.");
        let long = "n".repeat(60);
        let columns = [long.clone()];
        let refusal = json!({"code": "no_numeric_values", "columns": [long], "counts": []});
        assert_eq!(
            service_stop(
                &json!({"error": words, "refusal": refusal}),
                &columns,
                false
            )
            .message,
            Some(format!(
                "No numbers this figure can draw in {}\u{2026}.",
                "n".repeat(47)
            ))
        );
        // Every approved message fills from its places alone and has no other braces.
        for (code, message) in REFUSALS {
            let places = message.matches("{column}").count();
            let counts = message.matches("{count}").count();
            let all = usize::from(message.contains("{columns}"));
            let names: Vec<&str> = ["dose", "time"]
                .iter()
                .cycle()
                .take(places + all)
                .copied()
                .collect();
            let filled =
                fill(message, &names, &vec![3; counts]).unwrap_or_else(|| panic!("{code}"));
            assert!(!filled.contains(['{', '}']), "{code}");
            assert!(
                code.bytes().all(|b| b.is_ascii_lowercase() || b == b'_'),
                "{code}"
            );
        }
    }

    #[test]
    fn a_kept_refusal_fits_what_a_receipt_stores() {
        let table = &[(
            "no_complete_rows",
            "No row has numbers in all of {columns}.",
        )];
        let names = |n: usize, len: usize| -> Vec<String> {
            (0..n)
                .map(|i| format!("{i}{}", "c".repeat(len - 1)))
                .collect()
        };
        let refusal = |columns: &[String]| json!({"code": "no_complete_rows", "columns": columns, "counts": []});
        let eight = names(MAX_REFUSAL_ITEMS, 4);
        let (_, kept) = approve(table, &refusal(&eight), &eight).unwrap();
        assert_eq!(kept["columns"].as_array().unwrap().len(), MAX_REFUSAL_ITEMS);
        let nine = names(MAX_REFUSAL_ITEMS + 1, 4);
        assert!(approve(table, &refusal(&nine), &nine).is_none());
        // Long names: the stored refusal at most 2,048 bytes, measured as PostgreSQL prints it.
        let fits = |len: usize| {
            let long = names(MAX_REFUSAL_ITEMS, len);
            approve(table, &refusal(&long), &long).map(|(_, kept)| stored_bytes(&kept))
        };
        let at = (1..=400).rev().find(|len| fits(*len).is_some()).unwrap();
        assert!(fits(at).unwrap() <= MAX_REFUSAL_BYTES);
        assert!(fits(at).unwrap() + 2 * MAX_REFUSAL_ITEMS > MAX_REFUSAL_BYTES);
        assert!(fits(at + 1).is_none());
    }

    #[test]
    fn stored_bytes_are_counted_as_postgresql_prints_jsonb() {
        // `SELECT '{"code":"x","counts":[1,2],"columns":["a\\"b","é"]}'::jsonb::text` prints this text.
        let printed = r#"{"code": "x", "counts": [1, 2], "columns": ["a\"b", "é"]}"#;
        let value = json!({"code": "x", "counts": [1, 2], "columns": ["a\"b", "é"]});
        assert_eq!(stored_bytes(&value), printed.len());
        assert_eq!(stored_bytes(&json!([])), 2);
        assert_eq!(
            stored_bytes(&json!(["a", "b", "c"])),
            r#"["a", "b", "c"]"#.len()
        );
    }

    #[test]
    fn kept_notes_stop_before_the_stored_limit() {
        let long: Vec<String> = (0..40).map(|_| "é".repeat(MAX_MESSAGE_CHARS)).collect();
        let kept = kept_notes(long.clone(), MAX_NOTES_BYTES);
        assert!(stored_bytes(&json!(kept)) <= MAX_NOTES_BYTES);
        let mut one_more = kept.clone();
        one_more.push(long[0].clone());
        assert!(stored_bytes(&json!(one_more)) > MAX_NOTES_BYTES);
        let short: Vec<String> = (0..40).map(|i| format!("note {i}")).collect();
        assert_eq!(kept_notes(short, MAX_NOTES_BYTES).len(), MAX_NOTES);
    }

    #[test]
    fn a_reported_typeface_is_kept_only_when_plain_and_short() {
        assert!(plain_face("DejaVu Sans"));
        assert!(plain_face(&"f".repeat(MAX_FONT_FACE_CHARS)));
        assert!(!plain_face(&"f".repeat(MAX_FONT_FACE_CHARS + 1)));
        assert!(!plain_face("DejaVu\nSans"));
        assert!(!plain_face("DejaVu\u{1b}[31mSans"));
        // A setting the template read keeps its value when the service reports a face under the same name.
        let mut read = drawn();
        read.settings["params"]["font_face"] = json!("as read");
        assert_eq!(settings_used(&read)["font_face"], "as read");
        let mut unnamed = drawn();
        unnamed.font_face = None;
        assert!(settings_used(&unnamed).get("font_face").is_none());
    }

    #[test]
    fn tables_keep_every_cell_as_the_text_the_file_holds() {
        let (columns, rows) =
            parse_table(b"dose,response,group\n1,2.5,\"A, first\"\n2,,\"\"\n3,NA,\"NA\"\n")
                .unwrap();
        assert_eq!(columns, ["dose", "response", "group"]);
        assert_eq!(rows[0], [json!("1"), json!("2.5"), json!("A, first")]);
        // The service writes these back into the CSV its template reads, where R reads them as the file reads.
        assert_eq!(rows[1], [json!("2"), json!(""), json!("")]);
        assert_eq!(rows[2], [json!("3"), json!("NA"), json!("NA")]);
        assert_eq!(
            parse_table(b"dose\n").unwrap_err().code,
            "figure_table_unsuitable"
        );
        assert_eq!(
            parse_table(b"a,b\n1\n").unwrap_err().code,
            "figure_table_unreadable"
        );
        let widest = format!(
            "{}\n{}\n",
            vec!["c"; MAX_COLUMNS].join(","),
            vec!["1"; MAX_COLUMNS].join(",")
        );
        assert_eq!(parse_table(widest.as_bytes()).unwrap().0.len(), MAX_COLUMNS);
        let wider = format!("{}\n1\n", vec!["c"; MAX_COLUMNS + 1].join(","));
        assert_eq!(
            parse_table(wider.as_bytes()).unwrap_err().code,
            "figure_table_unsuitable"
        );
        // A header of empty fields filling the table bound is refused when the field room fills.
        let commas = ",".repeat(MAX_TABLE_BYTES as usize - 1);
        assert_eq!(
            parse_table(commas.as_bytes()).unwrap_err().code,
            "figure_table_unsuitable"
        );
        let row = format!("a,b\n1,2\n{}\n", ",".repeat(1_000_000));
        assert_eq!(
            parse_table(row.as_bytes()).unwrap_err().code,
            "figure_table_unreadable"
        );
    }

    #[test]
    fn packages_are_byte_identical_and_hold_the_code_settings_and_records() {
        let (a, files, notes) = package(&task(), &request(), &drawn()).unwrap();
        let (b, _, _) = package(&task(), &request(), &drawn()).unwrap();
        assert_eq!(a, b);
        assert_eq!(files.as_array().unwrap().len(), 3);
        assert_eq!(notes.as_array().unwrap().len(), 1);
        let mut archive = tar::Archive::new(flate2::read::GzDecoder::new(a.as_slice()));
        let mut names = Vec::new();
        let mut spec = Value::Null;
        for entry in archive.entries().unwrap() {
            let mut entry = entry.unwrap();
            let name = entry.path().unwrap().to_string_lossy().into_owned();
            if name == "spec.json" {
                let mut text = String::new();
                entry.read_to_string(&mut text).unwrap();
                spec = serde_json::from_str(&text).unwrap();
            }
            names.push(name);
        }
        assert_eq!(
            names,
            [
                "README.md",
                "code/_common.R",
                "code/params.json",
                "code/scatter.R",
                "figures/figure.pdf",
                "figures/figure.png",
                "figures/figure.svg",
                "spec.json",
                "SHA256SUMS"
            ]
        );
        assert_eq!(spec["settings_used"]["legend_position"], "bottom");
        assert_eq!(spec["settings_used"]["font_face"], "DejaVu Sans");
        assert_eq!(
            serde_json::from_slice::<Value>(&settings_bytes(&drawn().settings)).unwrap(),
            drawn().settings,
            "params.json keeps only what the template read"
        );
        assert_eq!(spec["source"]["table_sha256"], "b".repeat(64));
        assert_eq!(spec["service"]["version"], "0.5.20");
        // The digest of a figure package names its figures and finds no result table.
        let dir = std::env::temp_dir().join(format!(
            "figure_package_codetestfile_{}",
            uuid::Uuid::new_v4()
        ));
        let path = format!("tasks/{}/attempts/1-1/exports/payload", uuid::Uuid::nil());
        write_private(&dir.join(&path), &a).unwrap();
        let built = digest::build(
            &dir,
            uuid::Uuid::nil(),
            &path,
            a.len() as u64,
            &hex::encode(Sha256::digest(&a)),
        );
        std::fs::remove_dir_all(&dir).unwrap();
        assert_eq!(built.status, "no_result_tables", "{}", built.value);
        assert_eq!(built.value["package"]["all_matched"], true);
    }

    #[test]
    fn catalogs_keep_figure_features_only_and_need_a_version() {
        let catalog = json!({"service_version": "0.5.20", "image_digest": format!("sha256:{}", "f".repeat(64)),
            "features": [{"id": "scatter", "label": "Scatter plot", "kind": "figure", "params": {"x": {"type": "column"}}},
                {"id": "anova_emmeans", "label": "ANOVA", "kind": "analysis", "params": {}}]});
        let (version, image, features) = catalog_features(&catalog).unwrap();
        assert_eq!(version, "0.5.20");
        assert!(image.is_some());
        assert_eq!(features.as_array().unwrap().len(), 1);
        assert_eq!(features[0]["description"], "");
        assert_eq!(
            catalog_features(&json!({"features": []})).unwrap_err().code,
            "render_service_outdated"
        );
    }

    #[test]
    fn a_pass_stays_within_its_scan_budget_and_ends_once_stopped() {
        let dir = tempfile::tempdir().unwrap();
        let mut builder = tar::Builder::new(flate2::write::GzEncoder::new(
            Vec::new(),
            Compression::default(),
        ));
        for (name, bytes) in [
            ("results/skipped.bin", vec![0u8; 4096]),
            ("results/table.csv", b"a,b\n1,2\n".to_vec()),
        ] {
            let mut header = tar::Header::new_gnu();
            header.set_size(bytes.len() as u64);
            header.set_mode(0o644);
            header.set_entry_type(tar::EntryType::Regular);
            builder
                .append_data(&mut header, name, bytes.as_slice())
                .unwrap();
        }
        let archive = builder.into_inner().unwrap().finish().unwrap();
        std::fs::create_dir_all(dir.path().join("tasks")).unwrap();
        std::fs::write(dir.path().join("tasks/payload"), &archive).unwrap();
        let sha = hex::encode(Sha256::digest(&archive));
        let wanted = [("results/table.csv".to_owned(), 1024)];
        let read = |scan: u64, stop: Option<&AtomicBool>| {
            let budget = Budget {
                total: 1024,
                scan,
                stop,
            };
            read_members(
                dir.path(),
                "tasks/payload",
                archive.len() as i64,
                &sha,
                &wanted,
                &budget,
            )
        };
        let (members, scanned) = read(u64::MAX, None).unwrap();
        assert_eq!(members["results/table.csv"], b"a,b\n1,2\n");
        // Two 512-byte headers, the skipped body and the table: every byte of the stream up to the table's end.
        assert_eq!(scanned, 512 + 4096 + 512 + 8);
        assert_eq!(read(scanned - 1, None).unwrap_err(), MemberError::TooLarge);
        assert!(read(scanned, None).is_ok());
        // A set flag ends the pass at its first read.
        let stop = AtomicBool::new(true);
        assert_eq!(
            read(u64::MAX, Some(&stop)).unwrap_err(),
            MemberError::Unreadable { refused: false }
        );
    }

    #[test]
    fn a_pass_charges_extension_headers_to_its_budget() {
        // The review's probe: a PAX header of about 60 KiB before a small table counts as the stream it is.
        let dir = tempfile::tempdir().unwrap();
        let mut builder = tar::Builder::new(flate2::write::GzEncoder::new(
            Vec::new(),
            Compression::default(),
        ));
        let mut record = String::new();
        for length in 60_000..60_100 {
            let candidate = format!("{length} comment={}\n", "x".repeat(60_000));
            if candidate.len() == length {
                record = candidate;
            }
        }
        assert!(!record.is_empty());
        let mut pax = tar::Header::new_ustar();
        pax.set_entry_type(tar::EntryType::XHeader);
        pax.set_size(record.len() as u64);
        pax.set_mode(0o644);
        pax.set_path("PaxHeaders/table.csv").unwrap();
        pax.set_cksum();
        builder.append(&pax, record.as_bytes()).unwrap();
        let mut header = tar::Header::new_ustar();
        header.set_size(8);
        header.set_mode(0o644);
        header.set_entry_type(tar::EntryType::Regular);
        builder
            .append_data(&mut header, "results/table.csv", &b"a,b\n1,2\n"[..])
            .unwrap();
        let archive = builder.into_inner().unwrap().finish().unwrap();
        std::fs::create_dir_all(dir.path().join("tasks")).unwrap();
        std::fs::write(dir.path().join("tasks/payload"), &archive).unwrap();
        let sha = hex::encode(Sha256::digest(&archive));
        let wanted = [("results/table.csv".to_owned(), 1024)];
        let read = |scan: u64| {
            read_members(
                dir.path(),
                "tasks/payload",
                archive.len() as i64,
                &sha,
                &wanted,
                &Budget {
                    total: 1024,
                    scan,
                    stop: None,
                },
            )
        };
        let (_, scanned) = read(u64::MAX).unwrap();
        // The PAX header, its body padded to 512 bytes, the table's header and the table.
        assert_eq!(
            scanned,
            512 + (record.len() as u64).div_ceil(512) * 512 + 512 + 8
        );
        assert_eq!(read(4096).unwrap_err(), MemberError::TooLarge);
        assert_eq!(read(scanned - 1).unwrap_err(), MemberError::TooLarge);
        assert!(read(scanned).is_ok());
    }
}
