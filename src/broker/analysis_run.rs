//! The worker's template-analysis route. A template analysis runs here, not in a
//! sandbox: no model-written code runs, only a reviewed analysis template in the R render service. The worker reads the
//! table from the retained package the request names and checks both digests, sends the table's columns and rows with
//! the chosen template and settings to the service, and packages what the service returns: every row of the results at
//! full precision as CSV and JSON, the headline statistics, any figure the template drew, the template and shared R code,
//! the exact settings the template read, a record of the request, a README and `SHA256SUMS`. The source table is not
//! copied; the package names it by digest. One receipt is recorded per attempt. A refusal by the service finishes the
//! analysis with the broker's approved words for the service's refusal code; the service's own words are never passed
//! on, since they may quote what the request sent or what a template read.
use std::{borrow::Cow, time::Duration};

use base64::Engine;
use flate2::{Compression, GzBuilder};
use reqwest::StatusCode;
use serde_json::{Map, Value, json};
use sha2::{Digest, Sha256};
use time::OffsetDateTime;
use tokio::time::{MissedTickBehavior, interval};
use tokio_util::sync::CancellationToken;

use super::{
    budgeted_json::{self, Refused},
    config::WorkerConfig,
    digest,
    figure_run::{self, SourceTable, Stop, failed},
    kinds::Declaration,
    model::ClaimedTask,
    store::{AnalysisReceipt, Store, TaskAnalysis},
};

/// The profile route that selects this path.
pub const ROUTE: &str = "server-analysis";
const PACKAGE_LABEL: &str = "Template analysis results and reproducibility package";
const MAX_RESPONSE_BYTES: usize = 160 * 1024 * 1024;
/// The parts of the service's answer an analysis keeps, and what holding them may cost (`budgeted_json`). The rest of
/// the answer, such as the table it echoes back, is read past without being held.
const ANSWER_FIELDS: &[&str] = &[
    "error",
    "refusal",
    "results_exact",
    "spec",
    "service_version",
    "image_digest",
    "session_info",
    "png",
    "svg",
    "code",
    "common_code",
    "notes",
];
const MAX_ANSWER_HELD: usize = 128 * 1024 * 1024;
const MAX_R_SESSION_BYTES: usize = 64 * 1024;
const RENDER_TIMEOUT: Duration = Duration::from_secs(150);
/// The largest results table a package keeps, in rows, columns, characters of a column name and bytes of CSV.
const MAX_RESULT_ROWS: usize = 200_000;
const MAX_RESULT_COLUMNS: usize = 256;
const MAX_COLUMN_NAME_CHARS: usize = 200;
const MAX_RESULT_CSV_BYTES: usize = 64 * 1024 * 1024;
/// The largest `results/record.json` and `results/statistics.csv`, in bytes.
const MAX_RECORD_BYTES: usize = 96 * 1024 * 1024;
const MAX_STATISTICS_BYTES: usize = 1024 * 1024;
const MAX_STATISTICS: usize = 256;
/// The method text and each note are cut at these many characters; at most this many notes are kept.
const MAX_METHOD_CHARS: usize = 2000;
const MAX_NOTE_CHARS: usize = 300;
const MAX_NOTES: usize = 32;
/// A receipt's notes as PostgreSQL stores them (`notes::text`) may hold at most this many bytes (migration 058).
const MAX_NOTES_BYTES: usize = 16_384;

/// The drawings a template analysis declares as R figures: the figure files a completed receipt lists for the package
/// whose SHA-256 is `package_sha256`. This path wrote both the package and the receipt, so the declaration is the
/// broker's own; `detail` is the analysis's record as `Store::task_analysis_detail` returns it, or null for any other
/// task.
pub fn declared_figures(detail: &Value, package_sha256: &str) -> Vec<Declaration> {
    detail["receipts"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|receipt| {
            receipt["status"] == "completed" && receipt["package_sha256"] == package_sha256
        })
        .flat_map(|receipt| receipt["files"].as_array().into_iter().flatten())
        .filter(|file| file["role"] == "figure")
        .filter_map(|file| {
            Some(Declaration {
                kind: figure_run::FIGURE_KIND,
                path: file["path"].as_str()?.to_owned(),
                sha256: file["sha256"].as_str()?.to_owned(),
            })
        })
        .collect()
}

/// The render service's refusal codes an analysis can carry (its README, "Refusals") and the broker's words for each.
/// `{column}` takes the refusal's next column, `{columns}` all its remaining columns and `{count}` its next count.
const REFUSALS: &[(&str, &str)] = &[
    (
        "column_missing",
        "A column this analysis needs is not in the table.",
    ),
    (
        "column_not_in_table",
        "A column named in the settings is not in the table.",
    ),
    (
        "figure_unknown",
        "The analysis service does not offer this analysis.",
    ),
    (
        "group_count_not_two",
        "This analysis compares exactly two groups; {column} has {count}.",
    ),
    (
        "group_one_level",
        "{column} has only one group; this analysis needs at least two.",
    ),
    ("no_complete_rows", "No row has values in all of {columns}."),
    (
        "no_factor_with_levels",
        "None of {columns} has two or more groups with values.",
    ),
    (
        "no_measure_with_pairs",
        "No measure has at least {count} subjects with both values.",
    ),
    (
        "no_numeric_values",
        "No numbers this analysis can use in {columns}.",
    ),
    ("no_rows", "The table has no rows to analyze."),
    (
        "nothing_above_threshold",
        "No row of {column} reaches the LOD threshold in the settings.",
    ),
    (
        "nothing_to_test",
        "No feature has at least {count} values in each of two groups, so there is nothing to test.",
    ),
    (
        "outcome_not_numeric",
        "{column} is not numeric; choose classification as the task.",
    ),
    ("outcome_one_class", "{column} has only one class."),
    (
        "power_n_or_power_missing",
        "The QTL design needs the cohort size or the target power.",
    ),
    (
        "power_pve_missing",
        "The QTL design needs the proportion of variance the locus explains, between 0 and 1.",
    ),
    (
        "power_settings_incomplete",
        "Give exactly two of the group size, the effect size and the power; the third is solved.",
    ),
    (
        "render_timed_out",
        "The analysis took longer than {count} seconds.",
    ),
    (
        "setting_invalid",
        "The analysis service did not accept one of these settings.",
    ),
    (
        "setting_missing",
        "A setting this analysis needs is missing.",
    ),
    (
        "too_few_complete_rows",
        "This analysis needs at least {count} rows with values in {columns}.",
    ),
    (
        "too_few_samples",
        "This analysis needs at least {count} samples with values in {column}.",
    ),
    (
        "too_few_values_per_group",
        "Each group in {column} needs at least {count} values.",
    ),
    (
        "too_few_varying_columns",
        "This analysis needs at least {count} numeric columns whose values vary.",
    ),
    (
        "too_many_rows",
        "The table has {count} rows, more than the {count} the analysis service takes.",
    ),
    // The analyses of animals (render 0.6.0): readings, hours and visits of one animal are not animals.
    (
        "time_not_numeric",
        "This analysis needs numeric times in {column}.",
    ),
    (
        "factor_varies_within_animal",
        "{count} animals in {column} have more than one value of {column}; a group or factor here belongs to the \
         animal.",
    ),
    (
        "repeated_time_rows",
        "{count} pairs of {column} and {column} have more than one row; keep one reading per animal and time.",
    ),
    (
        "too_few_time_points",
        "{column} has readings at {count} times; this analysis needs at least {count}.",
    ),
    (
        "too_few_animals",
        "{count} animals in {column} have the readings this analysis needs; it needs at least {count}.",
    ),
    (
        "kept_column_varies_within_animal",
        "{count} animals in {column} have more than one value of {column}, so it cannot be carried as one value per \
         animal.",
    ),
    (
        "no_rows_in_window",
        "No row falls inside the window chosen in {columns}.",
    ),
    (
        "repeated_rows_per_animal",
        "{count} animals in {column} have more than one row; summarize each animal first, so animals and not hours \
         or days are compared.",
    ),
    (
        "too_few_animals_per_group",
        "{count} groups in {column} have fewer than {count} animals.",
    ),
    (
        "empty_group_cell",
        "{count} combinations of {column} and {column} have no animal; set interaction to false or add animals.",
    ),
    (
        "model_not_estimable",
        "The model cannot separate the effects of {columns} in these animals.",
    ),
    (
        "model_did_not_converge",
        "The mixed model did not converge for {columns}; try compound symmetry or fewer factors.",
    ),
];
const SERVICE_REFUSED: &str = "The analysis service refused these settings.";
const NOT_RUN: &str =
    "The analysis service could not run this analysis on this table with these settings.";

/// A stop for a render service answer that carries an error. As for figures, the service's words are
/// never passed on: a refusal the broker knows, naming only columns of the table and whole counts, chooses the broker's
/// message; any other answer gets the general one. `request_refused` is the service's HTTP 400.
fn service_stop(out: &Value, columns: &[String], request_refused: bool) -> Stop {
    let approved = figure_run::approve(REFUSALS, &out["refusal"], columns);
    let (status, code, general) = match (request_refused, approved.is_some()) {
        (true, _) => ("refused", "analysis_refused", SERVICE_REFUSED),
        (false, true) => ("refused", "analysis_refused", NOT_RUN),
        (false, false) => ("failed", "analysis_failed", NOT_RUN),
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

/// The shared table and package helpers name their stops for figures; an analysis's receipt names them for analyses.
fn renamed(mut stop: Stop) -> Stop {
    stop.code = match stop.code {
        "figure_source_unreadable" => "analysis_source_unreadable",
        "figure_source_changed" => "analysis_source_changed",
        "figure_table_too_large" => "analysis_table_too_large",
        "figure_table_missing" => "analysis_table_missing",
        "figure_table_unreadable" => "analysis_table_unreadable",
        "figure_table_unsuitable" => "analysis_table_unsuitable",
        "figure_package_failed" => "analysis_package_failed",
        "figure_package_not_registered" => "analysis_package_not_registered",
        "figure_digest_failed" => "analysis_digest_failed",
        other => other,
    };
    stop
}

/// Read the service's analysis catalog and keep it for the API and the workspace. Only analysis features are kept, each
/// with its label, description and declared settings.
pub async fn refresh_catalog(store: &Store, config: &WorkerConfig) -> Result<String, Stop> {
    let catalog = figure_run::fetch_catalog(config).await?;
    let (version, image_digest, features) = catalog_features(&catalog)?;
    store
        .record_analysis_catalog(
            &config.worker_id,
            &version,
            image_digest.as_deref(),
            &features,
        )
        .await
        .map_err(|_| failed("analysis_catalog_not_recorded"))
}

/// The analysis features of a `/features` response in the stored shape, with the service's version and image digest.
fn catalog_features(catalog: &Value) -> Result<(String, Option<String>, Value), Stop> {
    let version =
        figure_run::service_version(catalog).ok_or_else(|| failed("render_service_outdated"))?;
    let image_digest = catalog["image_digest"]
        .as_str()
        .filter(|d| figure_run::is_image_digest(d))
        .map(str::to_owned);
    let features = catalog["features"]
        .as_array()
        .ok_or_else(|| failed("render_catalog_unreadable"))?
        .iter()
        .filter(|f| f["kind"] == "analysis")
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

impl From<&TaskAnalysis> for SourceTable {
    fn from(request: &TaskAnalysis) -> Self {
        SourceTable {
            package_path: request.package_path.clone(),
            package_bytes: request.package_bytes,
            package_sha256: request.package_sha256.clone(),
            table_path: request.table_path.clone(),
            table_sha256: request.table_sha256.clone(),
        }
    }
}

struct Done {
    files: Value,
    notes: Value,
    method: Option<String>,
    service_version: String,
    image_digest: Option<String>,
    package_sha256: String,
}

/// What the service returned for an analysis, as checked.
#[derive(Debug)]
struct Ran {
    /// The full record: columns, every row at full precision, method, notes and statistics.
    record: Value,
    columns: Vec<String>,
    png: Option<Vec<u8>>,
    svg: Option<Vec<u8>>,
    template: String,
    common: String,
    settings: Value,
    service_version: String,
    image_digest: Option<String>,
    r_session: Option<Value>,
    /// The service's own notes on the settings, such as one it did not use.
    notes: Vec<String>,
}

/// Run one claimed template analysis to a finished state, or leave it to lease recovery when the lease or the worker
/// stops.
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
        super::feature_events::Feature::Analysis,
        &task,
    );
    let Some(request) = store
        .task_analysis(task.id, worker, task.attempt, generation)
        .await?
    else {
        store
            .finish_task(
                task.id,
                worker,
                generation,
                false,
                "analysis_not_run",
                "analysis_not_recorded",
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
            AnalysisReceipt {
                status: "completed",
                error_code: None,
                message: None,
                refusal: None,
                service_version: Some(done.service_version),
                image_digest: done.image_digest,
                method: done.method,
                files: Some(done.files),
                notes: done.notes,
                package_sha256: Some(done.package_sha256),
                started_at: started,
                finished_at: finished,
            },
            true,
            "analysis_completed",
            String::new(),
        ),
        Err(stop) => (
            AnalysisReceipt {
                status: stop.status,
                error_code: Some(stop.code.to_owned()),
                message: stop.message,
                refusal: stop.refusal,
                service_version: None,
                image_digest: None,
                method: None,
                files: None,
                notes: json!([]),
                package_sha256: None,
                started_at: started,
                finished_at: finished,
            },
            false,
            "analysis_not_completed",
            stop.code.to_owned(),
        ),
    };
    store
        .record_analysis_receipt(task.id, worker, task.attempt, generation, &receipt)
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
    tracing::info!(task_id=%task.id, lease_generation=generation, event="analysis_finished", status=receipt.status,
        code=error_code.as_str());
    let telemetry_outcome = match receipt.status {
        "completed" => super::feature_events::Outcome::Success,
        "refused" => super::feature_events::Outcome::Refused,
        _ => super::feature_events::Outcome::Failure,
    };
    feature_telemetry.finish(
        telemetry_outcome,
        (!passed).then_some(super::feature_events::ErrorCode::AnalysisFailed),
        u64::from(passed),
        u64::from(!passed),
    );
    Ok(())
}

async fn run(
    store: &Store,
    config: &WorkerConfig,
    task: &ClaimedTask,
    request: &TaskAnalysis,
) -> Result<Done, Stop> {
    let worker = config.worker_id.as_str();
    let base = config
        .render_url
        .clone()
        .ok_or_else(|| failed("render_service_unconfigured"))?;
    let root = config.artifact_root.clone();
    let source = SourceTable::from(request);
    let table = tokio::task::spawn_blocking(move || figure_run::read_table(&root, &source))
        .await
        .map_err(|_| failed("analysis_source_unreadable"))?
        .map_err(renamed)?;
    let (columns, rows) = figure_run::parse_table(&table).map_err(renamed)?;
    if store
        .cancel_requested(task.id, worker, task.lease_generation)
        .await
        .map_err(|_| failed("lease_lost"))?
    {
        return Err(failed("cancelled"));
    }
    // The catalog is read again with each analysis so the workspace offers what the running service declares.
    if let Err(stop) = refresh_catalog(store, config).await {
        tracing::warn!(task_id=%task.id, event="analysis_catalog_not_read", code=stop.code());
    }
    // A template can run for minutes, so the lease is renewed while the service works, and a stop request ends the wait.
    let ran = {
        let call = run_template(&base, request, &columns, rows);
        tokio::pin!(call);
        let mut renewals = interval(Duration::from_secs((config.lease_seconds / 3).max(5)));
        renewals.set_missed_tick_behavior(MissedTickBehavior::Delay);
        loop {
            tokio::select! {
                ran = &mut call => break ran?,
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
    };
    let (package, files, notes) = package(task, request, &ran)?;
    figure_run::retain(store, config, task, &package, PACKAGE_LABEL)
        .await
        .map_err(renamed)?;
    if let Some(image) = ran.image_digest.as_deref() {
        let template = format!(
            "sha256:{}",
            hex::encode(Sha256::digest(ran.template.as_bytes()))
        );
        let template_ref = format!("render-service/templates/{}.R", request.feature);
        let tool_ref = format!("render-service@{}", ran.service_version);
        let settings = format!(
            "sha256:{}",
            hex::encode(Sha256::digest(figure_run::settings_bytes(&ran.settings)))
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
            tracing::warn!(task_id=%task.id, event="analysis_execution_not_recorded", code=e.code);
        }
    }
    Ok(Done {
        files,
        notes,
        method: method_text(&ran.record),
        service_version: ran.service_version,
        image_digest: ran.image_digest,
        package_sha256: hex::encode(Sha256::digest(&package)),
    })
}

/// Text from the service as a receipt keeps it: without control or `Bidi_Control` characters, cut at `max`.
fn plain_text(text: &str, max: usize) -> String {
    let kept: String = text
        .chars()
        .map(|c| if c == '\n' || c == '\t' { ' ' } else { c })
        .filter(|&c| !c.is_control() && !super::live_output::is_bidi_control(c))
        .collect();
    figure_run::cut(kept.trim(), max)
}

/// The method the template stated, as the receipt keeps it; none when it stated none.
fn method_text(record: &Value) -> Option<String> {
    record["method"]
        .as_str()
        .map(|m| plain_text(m, MAX_METHOD_CHARS))
        .filter(|m| !m.is_empty())
}

/// Ask the service to run the template and check what it returned.
async fn run_template(
    base: &str,
    request: &TaskAnalysis,
    columns: &[String],
    rows: Vec<Vec<Value>>,
) -> Result<Ran, Stop> {
    let mut body = json!({"feature_id": request.feature, "params": request.settings, "columns": columns, "rows": rows});
    if !request.title.is_empty() {
        body["title"] = json!(request.title);
    }
    let response = figure_run::client()?
        .post(format!("{base}/render"))
        .timeout(RENDER_TIMEOUT)
        .json(&body)
        .send()
        .await
        .map_err(|_| failed("render_service_unavailable"))?;
    let status = response.status();
    let bytes = figure_run::limited(response, MAX_RESPONSE_BYTES).await?;
    let out = budgeted_json::read(&bytes, ANSWER_FIELDS, MAX_ANSWER_HELD).map_err(|refused| {
        failed(match refused {
            Refused::TooLarge => "analysis_result_too_large",
            Refused::Unreadable => "render_response_unreadable",
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
    checked(out)
}

/// The service's answer for an analysis as a package keeps it, or the stop that says why it cannot be kept. The record
/// and settings are moved out of the answer, not copied.
fn checked(mut out: Value) -> Result<Ran, Stop> {
    let mut take = |key: &str| {
        out.get_mut(key)
            .map(Value::take)
            .filter(Value::is_object)
            .ok_or_else(|| failed("render_service_outdated"))
    };
    // Releases before 0.6.0 return only the rounded display table, which a package must not keep as the result.
    let record = take("results_exact")?;
    let settings = take("spec")?;
    let out = out;
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
    let service_version =
        figure_run::service_version(&out).ok_or_else(|| failed("render_service_outdated"))?;
    let r_session = if service_requires_r_session(&service_version) {
        Some(
            valid_r_session(&out["session_info"])
                .ok_or_else(|| failed("render_session_info_invalid"))?,
        )
    } else {
        None
    };
    let columns = result_columns(&record)?;
    let png = decode("png")?;
    let svg = text("svg").map(String::into_bytes);
    if png
        .as_ref()
        .is_some_and(|b| !b.starts_with(b"\x89PNG\r\n\x1a\n"))
        || svg
            .as_ref()
            .is_some_and(|b| !String::from_utf8_lossy(b).contains("<svg"))
        || png.is_some() != svg.is_some()
    {
        return Err(failed("render_response_unreadable"));
    }
    Ok(Ran {
        record,
        columns,
        png,
        svg,
        template: text("code").ok_or_else(|| failed("render_response_unreadable"))?,
        common: text("common_code").ok_or_else(|| failed("render_service_outdated"))?,
        settings,
        service_version,
        image_digest: text("image_digest").filter(|d| figure_run::is_image_digest(d)),
        r_session,
        notes: out["notes"]
            .as_array()
            .map(|n| {
                n.iter()
                    .filter_map(Value::as_str)
                    .take(MAX_NOTES)
                    .map(|t| plain_text(t, MAX_NOTE_CHARS))
                    .collect()
            })
            .unwrap_or_default(),
    })
}

fn service_requires_r_session(version: &str) -> bool {
    let core = version.split(['+', '-']).next().unwrap_or(version);
    let mut parts = core.split('.');
    let Some(major) = parts.next().and_then(|part| part.parse::<u64>().ok()) else {
        return false;
    };
    let Some(minor) = parts.next().and_then(|part| part.parse::<u64>().ok()) else {
        return false;
    };
    let patch = parts
        .next()
        .and_then(|part| part.parse::<u64>().ok())
        .unwrap_or(0);
    (major, minor, patch) >= (0, 7, 1)
}

fn valid_r_session_text(text: &str) -> bool {
    text.chars().count() <= 400 && !text.chars().any(char::is_control)
}

/// A BLAS or LAPACK file name (never a path), or empty when R reports none.
fn valid_r_library_name(text: &str) -> bool {
    text.is_empty()
        || (text.len() <= 128
            && text
                .as_bytes()
                .first()
                .is_some_and(u8::is_ascii_alphanumeric)
            && text.bytes().all(|byte| {
                byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'+' | b'-')
            }))
}

/// The render service's bounded `sessionInfo()` record: the ten fields its templates write (`templates/_common.R`,
/// `write_session_info`), checked with the render service's own rules (`app.py`, `_read_session_info`). Text is at
/// most 400 characters without control characters and may be empty; BLAS and LAPACK are file names or empty;
/// LAPACK's version is a version or empty; package names and versions are valid R names and versions. Services that
/// promise this record are refused unless it is complete and valid; older services omit it from the retained package.
fn valid_r_session(value: &Value) -> Option<Value> {
    const STRING_FIELDS: [&str; 4] = ["r_version", "platform", "running", "locale"];
    const LIBRARY_FIELDS: [&str; 2] = ["blas", "lapack"];
    const LIST_FIELDS: [&str; 2] = ["base_packages", "attached"];
    let object = value.as_object()?;
    // The listed fields, `lapack_version` and `packages`, and nothing else.
    if object.len() != STRING_FIELDS.len() + LIBRARY_FIELDS.len() + LIST_FIELDS.len() + 2
        || serde_json::to_vec(value).ok()?.len() > MAX_R_SESSION_BYTES
        || STRING_FIELDS.iter().any(|key| {
            !object
                .get(*key)
                .and_then(Value::as_str)
                .is_some_and(valid_r_session_text)
        })
        || LIBRARY_FIELDS.iter().any(|key| {
            !object
                .get(*key)
                .and_then(Value::as_str)
                .is_some_and(valid_r_library_name)
        })
        || !object
            .get("lapack_version")
            .and_then(Value::as_str)
            .is_some_and(|version| version.is_empty() || valid_r_package_version(version))
        || LIST_FIELDS.iter().any(|key| {
            object
                .get(*key)
                .and_then(Value::as_array)
                .is_none_or(|names| {
                    names.len() > 512
                        || names.iter().any(|name| {
                            name.as_str().is_none_or(|name| !valid_r_package_name(name))
                        })
                })
        })
    {
        return None;
    }
    let packages = object.get("packages")?.as_object()?;
    if packages.is_empty()
        || packages.len() > 512
        || packages.iter().any(|(name, version)| {
            !valid_r_package_name(name)
                || version
                    .as_str()
                    .is_none_or(|version| !valid_r_package_version(version))
        })
    {
        return None;
    }
    Some(value.clone())
}

fn valid_r_package_name(name: &str) -> bool {
    (1..=64).contains(&name.len())
        && name.as_bytes().first().is_some_and(u8::is_ascii_alphabetic)
        && name
            .bytes()
            .skip(1)
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'.')
}

fn valid_r_package_version(version: &str) -> bool {
    (1..=32).contains(&version.len())
        && version.as_bytes().first().is_some_and(u8::is_ascii_digit)
        && version
            .bytes()
            .skip(1)
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'-'))
}

/// The record's column names, checked: 1 to 256 distinct names of 1 to 200 characters, and every row an object of at
/// most those columns, at most 200,000 rows.
fn result_columns(record: &Value) -> Result<Vec<String>, Stop> {
    let unreadable = || failed("analysis_result_unreadable");
    let columns: Vec<String> = record["columns"]
        .as_array()
        .ok_or_else(unreadable)?
        .iter()
        .map(|c| {
            c.as_str()
                .filter(|c| (1..=MAX_COLUMN_NAME_CHARS).contains(&c.chars().count()))
                .map(str::to_owned)
                .ok_or_else(unreadable)
        })
        .collect::<Result<_, _>>()?;
    let distinct: std::collections::BTreeSet<&str> = columns.iter().map(String::as_str).collect();
    if columns.is_empty() || columns.len() > MAX_RESULT_COLUMNS || distinct.len() != columns.len() {
        return Err(unreadable());
    }
    let rows = record["rows"].as_array().ok_or_else(unreadable)?;
    if rows.len() > MAX_RESULT_ROWS {
        return Err(failed("analysis_result_too_large"));
    }
    if rows.iter().any(|row| {
        row.as_object()
            .is_none_or(|row| row.keys().any(|k| !distinct.contains(k.as_str())))
    }) {
        return Err(unreadable());
    }
    Ok(columns)
}

/// One CSV field as R's `write.csv` writes it, which the query route's packages also follow (rules version 1 in
/// `docs/result_tables.md`): a missing value is `NA`, unquoted, so no row is ever a blank line; text is quoted when it
/// is empty, is exactly `NA`, or holds a comma, quote, carriage return or line feed; logicals are `TRUE` and `FALSE`;
/// a number is the shortest text that reads back as the same double, so each of the at most 15 significant digits the
/// service writes is kept; a list or object is written as compact JSON text.
enum Field<'a> {
    Missing,
    Plain(Cow<'a, str>),
    Text(Cow<'a, str>),
}

impl<'a> Field<'a> {
    fn of(value: &'a Value) -> Self {
        match value {
            Value::Null => Field::Missing,
            Value::Bool(true) => Field::Plain(Cow::Borrowed("TRUE")),
            Value::Bool(false) => Field::Plain(Cow::Borrowed("FALSE")),
            Value::Number(n) => Field::Plain(Cow::Owned(n.to_string())),
            Value::String(s) => Field::Text(Cow::Borrowed(s)),
            other => Field::Text(Cow::Owned(other.to_string())),
        }
    }

    fn quoted(text: &str) -> bool {
        text.is_empty() || text == "NA" || text.contains([',', '"', '\r', '\n'])
    }

    /// The bytes the field takes once written.
    fn len(&self) -> usize {
        match self {
            Field::Missing => 2,
            Field::Text(text) if Self::quoted(text) => text.len() + 2 + text.matches('"').count(),
            Field::Plain(text) | Field::Text(text) => text.len(),
        }
    }

    fn write(&self, out: &mut String) {
        match self {
            Field::Missing => out.push_str("NA"),
            Field::Text(text) if Self::quoted(text) => {
                out.push('"');
                for (i, part) in text.split('"').enumerate() {
                    if i > 0 {
                        out.push_str("\"\"");
                    }
                    out.push_str(part);
                }
                out.push('"');
            }
            Field::Plain(text) | Field::Text(text) => out.push_str(text),
        }
    }
}

/// CSV text that never holds more than its limit: a line is written only when all of it fits, and the text's room
/// grows no further than the limit, so a large cell is refused before it is copied.
struct CsvText {
    text: String,
    limit: usize,
}

impl CsvText {
    fn new(limit: usize) -> Self {
        CsvText {
            text: String::new(),
            limit,
        }
    }

    /// Append one line; false, with nothing appended, when it would take the text past the limit.
    fn line<'a>(&mut self, fields: impl IntoIterator<Item = Field<'a>>) -> bool {
        let fields: Vec<Field<'a>> = fields.into_iter().collect();
        // The fields, a comma between each two and the line feed.
        let needed = fields.iter().map(Field::len).sum::<usize>() + fields.len().max(1);
        let Some(end) = self
            .text
            .len()
            .checked_add(needed)
            .filter(|end| *end <= self.limit)
        else {
            return false;
        };
        if self.text.capacity() < end {
            let room = (self.text.capacity() * 2).clamp(end, self.limit);
            self.text.reserve_exact(room - self.text.len());
        }
        for (i, field) in fields.iter().enumerate() {
            if i > 0 {
                self.text.push(',');
            }
            field.write(&mut self.text);
        }
        self.text.push('\n');
        true
    }
}

/// The record's rows as CSV with a header row, in the record's column order, in at most `limit` bytes; a row's missing
/// cell is a missing value.
fn results_csv(record: &Value, columns: &[String], limit: usize) -> Result<Vec<u8>, Stop> {
    let mut csv = CsvText::new(limit);
    let empty = Map::new();
    let fits = csv.line(
        columns
            .iter()
            .map(|c| Field::Text(Cow::Borrowed(c.as_str()))),
    ) && record["rows"].as_array().into_iter().flatten().all(|row| {
        let row = row.as_object().unwrap_or(&empty);
        csv.line(
            columns
                .iter()
                .map(|c| row.get(c).map_or(Field::Missing, Field::of)),
        )
    });
    if !fits {
        return Err(failed("analysis_result_too_large"));
    }
    Ok(csv.text.into_bytes())
}

/// The headline statistics as a two-column CSV (`statistic`, `value`) in at most `limit` bytes, so they read as a table
/// beside the results; none when the template stated none.
fn statistics_csv(record: &Value, limit: usize) -> Result<Option<Vec<u8>>, Stop> {
    let Some(stats) = record["stats"].as_object().filter(|s| !s.is_empty()) else {
        return Ok(None);
    };
    if stats.len() > MAX_STATISTICS {
        return Err(failed("analysis_result_unreadable"));
    }
    let mut csv = CsvText::new(limit);
    let fits = csv.line([
        Field::Plain(Cow::Borrowed("statistic")),
        Field::Plain(Cow::Borrowed("value")),
    ]) && stats
        .iter()
        .all(|(name, value)| csv.line([Field::Text(Cow::Borrowed(name)), Field::of(value)]));
    if !fits {
        return Err(failed("analysis_result_too_large"));
    }
    Ok(Some(csv.text.into_bytes()))
}

/// The record as pretty JSON and a final line feed in at most `limit` bytes; none when it would pass the limit, which
/// is found while it is written, before more is allocated.
fn record_json(record: &Value, limit: usize) -> Option<Vec<u8>> {
    struct Bounded {
        bytes: Vec<u8>,
        limit: usize,
    }
    impl std::io::Write for Bounded {
        fn write(&mut self, data: &[u8]) -> std::io::Result<usize> {
            let end = self.bytes.len() + data.len();
            if end > self.limit {
                return Err(std::io::Error::other("record_too_large"));
            }
            if self.bytes.capacity() < end {
                let room = (self.bytes.capacity() * 2).clamp(end, self.limit);
                self.bytes.reserve_exact(room - self.bytes.len());
            }
            self.bytes.extend_from_slice(data);
            Ok(data.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    let mut out = Bounded {
        bytes: Vec::new(),
        limit,
    };
    serde_json::to_writer_pretty(&mut out, record).ok()?;
    std::io::Write::write_all(&mut out, b"\n").ok()?;
    Some(out.bytes)
}

/// Where an analysis's results table goes. One computed from the lab's own data (a table under `results/lab_data/` or
/// `results/lab_derived/`) goes under `results/lab_derived/`, which the result digest lists with its columns and counts
/// but not its cells (row values reach people, not the model); its headline statistics stay in
/// `results/statistics.csv`, which the research lead may read.
pub fn results_table_path(source_table: &str) -> &'static str {
    if source_table.starts_with(digest::LAB_DATA) || source_table.starts_with(digest::LAB_DERIVED) {
        "results/lab_derived/results.csv"
    } else {
        "results/results.csv"
    }
}

/// A deterministic tar+gzip of the results, figure, code and records: sorted members, fixed owner, mode and time.
fn package(
    task: &ClaimedTask,
    request: &TaskAnalysis,
    ran: &Ran,
) -> Result<(Vec<u8>, Value, Value), Stop> {
    let template_path = format!("code/{}.R", request.feature);
    let mut outputs: Vec<(String, Vec<u8>, &str, &str)> = vec![(
        results_table_path(&request.table_path).into(),
        results_csv(&ran.record, &ran.columns, MAX_RESULT_CSV_BYTES)?,
        "text/csv",
        "table",
    )];
    if let Some(stats) = statistics_csv(&ran.record, MAX_STATISTICS_BYTES)? {
        outputs.push((
            "results/statistics.csv".into(),
            stats,
            "text/csv",
            "statistics",
        ));
    }
    let record = record_json(&ran.record, MAX_RECORD_BYTES)
        .ok_or_else(|| failed("analysis_result_too_large"))?;
    outputs.push((
        "results/record.json".into(),
        record,
        "application/json",
        "record",
    ));
    if let (Some(png), Some(svg)) = (&ran.png, &ran.svg) {
        outputs.push((
            "figures/figure.png".into(),
            png.clone(),
            "image/png",
            "figure",
        ));
        outputs.push((
            "figures/figure.svg".into(),
            svg.clone(),
            "image/svg+xml",
            "figure",
        ));
    }
    let files: Vec<Value> = outputs
        .iter()
        .map(|(path, bytes, media, role)| {
            json!({"path": path, "sha256": hex::encode(Sha256::digest(bytes)), "size_bytes": bytes.len(),
                "media_type": media, "role": role})
        })
        .collect();
    let settings = figure_run::settings_bytes(&ran.settings);
    let template_note = ran.record["notes"]
        .as_str()
        .map(|n| plain_text(n, MAX_NOTE_CHARS))
        .filter(|n| !n.is_empty());
    let notes = json!(figure_run::kept_notes(
        template_note.into_iter().chain(ran.notes.iter().cloned()),
        MAX_NOTES_BYTES
    ));
    let mut service = json!({"version": ran.service_version, "image_digest": ran.image_digest});
    if let Some(session) = &ran.r_session {
        service["r_session"] = session.clone();
    }
    let spec = json!({
        "schema": "enrichedlab.template-analysis-spec.v1",
        "task_id": task.id,
        "feature": request.feature,
        "title": request.title,
        "settings": request.settings,
        "settings_used": ran.settings["params"],
        "method": method_text(&ran.record),
        "notes": notes,
        "rows": ran.record["rows"].as_array().map_or(0, Vec::len),
        "source": {"task_id": request.source_task, "package_sha256": request.package_sha256,
            "table_path": request.table_path, "table_sha256": request.table_sha256, "rows": request.table_rows},
        "service": service,
        "code": {"template": {"path": template_path, "sha256": hex::encode(Sha256::digest(ran.template.as_bytes()))},
            "shared": {"path": "code/_common.R", "sha256": hex::encode(Sha256::digest(ran.common.as_bytes()))},
            "settings": {"path": "code/params.json", "sha256": hex::encode(Sha256::digest(&settings))}},
        "files": files,
        "request_sha256": request.request_sha256,
    });
    let mut members: Vec<(String, Vec<u8>)> = outputs
        .into_iter()
        .map(|(path, bytes, _, _)| (path, bytes))
        .collect();
    members.push((template_path.clone(), ran.template.clone().into_bytes()));
    members.push(("code/_common.R".into(), ran.common.clone().into_bytes()));
    members.push(("code/params.json".into(), settings));
    members.push((
        "spec.json".into(),
        serde_json::to_vec_pretty(&spec).map_err(|_| failed("analysis_package_failed"))?,
    ));
    members.push((
        "README.md".into(),
        readme(
            request,
            &template_path,
            &ran.service_version,
            ran.r_session.as_ref(),
            ran.png.is_some(),
        )
        .into_bytes(),
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
    let gz = (|| -> std::io::Result<Vec<u8>> {
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
    .map_err(|_| failed("analysis_package_failed"))?;
    Ok((gz, Value::Array(files), notes))
}

fn readme(
    request: &TaskAnalysis,
    template_path: &str,
    version: &str,
    r_session: Option<&Value>,
    figure: bool,
) -> String {
    let title = if request.title.is_empty() {
        "Template analysis".to_owned()
    } else {
        request.title.clone()
    };
    let r_environment = r_session.map_or_else(String::new, |session| {
        format!(
            "- {} ran with {} loaded packages; `spec.json` records the full R session.\n",
            plain_text(session["r_version"].as_str().unwrap_or_default(), 512),
            session["packages"]
                .as_object()
                .map_or(0, serde_json::Map::len)
        )
    });
    let rerun_environment = if r_session.is_some() {
        "the R version and package versions recorded under `service.r_session` in `spec.json`"
    } else {
        "R 4.4 and the packages the template loads (ggplot2, ragg, svglite, scales and jsonlite, and any the template names with `library()`)"
    };
    format!(
        "# {title}\n\nThis package holds one run of the reviewed R analysis template `{feature}` in the render service \
         ({version}) on the table `{table}` of an earlier analysis. No code here was written by a model. The table is not \
         copied here: it is in that analysis's package, and its SHA-256 is `{table_sha}`.{lab}\n\n- \
         `{results}` holds every row of the results at full precision (15 significant digits). A missing \
         value is `NA`, as R writes it, and `read.csv` reads the file as the template wrote the table; it also reads \
         the text `NA` as missing.\n- `results/statistics.csv`, when present, holds the headline statistics as \
         `statistic,value` rows.\n- `results/record.json` holds the same results with the method and notes as the \
         template wrote them.\n{figures}- `code/` holds the template (`{template_path}`), the shared R code it reads \
         (`code/_common.R`) and the exact settings it read (`code/params.json`).\n- `spec.json` records the request, the \
         settings as asked for and as used, the source table and the service's version and image.\n{r_environment}- `SHA256SUMS` lists \
         every file's SHA-256.\n\n## Check the files\n\nmacOS: `shasum -a 256 -c SHA256SUMS`  \nLinux: `sha256sum -c \
         SHA256SUMS`\n\n## Run it again\n\nCopy the table from the earlier analysis's package into a new folder as \
         `data.csv`, and `code/params.json` into the same folder as `params.json`. Then run `Rscript {template_path} \
         <that folder>` with {rerun_environment}. The template writes `results_exact.json`, the record this \
         package's `results/record.json` holds, and `results.json`, a copy rounded for display, to that folder{drawn}.\n",
        feature = request.feature,
        results = results_table_path(&request.table_path),
        lab = if results_table_path(&request.table_path).starts_with(digest::LAB_DERIVED) {
            " The table holds the lab's own data, so the results table is under `results/lab_derived/`: the research \
             lead is shown its columns and counts, not its rows, and reads the headline statistics."
        } else {
            ""
        },
        table = request.table_path,
        table_sha = request.table_sha256,
        figures = if figure {
            "- `figures/` holds the figure the template drew, as PNG and SVG.\n"
        } else {
            ""
        },
        r_environment = r_environment,
        rerun_environment = rerun_environment,
        drawn = if figure {
            ", and the figure as `figure.png` and `figure.svg`"
        } else {
            ""
        },
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Read;

    fn r_session() -> Value {
        json!({
            "r_version": "R version 4.4.3",
            "platform": "x86_64-pc-linux-gnu",
            "running": "Ubuntu 24.04",
            "locale": "C.UTF-8",
            "blas": "libblas.so",
            "lapack": "liblapack.so",
            "lapack_version": "3.12.0",
            "base_packages": ["stats", "graphics", "base"],
            "attached": ["ggplot2"],
            "packages": {"ggplot2": "3.5.2", "jsonlite": "1.9.1", "stats": "4.4.3"}
        })
    }

    fn current_answer() -> Value {
        json!({
            "results_exact": {"columns": ["a"], "rows": [{"a": 1}]},
            "spec": {"params": {}},
            "service_version": "0.7.1",
            "code": "x",
            "common_code": "y"
        })
    }

    fn package_member(package: &[u8], wanted: &str) -> Option<Vec<u8>> {
        let mut archive = tar::Archive::new(flate2::read::GzDecoder::new(package));
        for entry in archive.entries().ok()? {
            let mut entry = entry.ok()?;
            if entry.path().ok()?.as_ref() == std::path::Path::new(wanted) {
                let mut bytes = Vec::new();
                entry.read_to_end(&mut bytes).ok()?;
                return Some(bytes);
            }
        }
        None
    }

    fn record(rows: Value) -> Value {
        json!({"columns": ["feature", "p_value", "note"], "rows": rows, "method": "Welch t-test",
            "notes": "a vs b", "stats": {"p_value": 6.12498179243933e-29, "n": 30, "test": "t"}, "n": 2})
    }

    #[test]
    fn results_keep_every_digit_and_write_missing_values_as_r_does() {
        let r = record(json!([
            {"feature": "f1", "p_value": 6.12498179243933e-29, "note": ""},
            {"feature": "a,b \"c\"", "p_value": null, "note": "line\nbreak"},
            {"feature": "NA", "p_value": 0.5}
        ]));
        let columns = result_columns(&r).unwrap();
        let csv =
            String::from_utf8(results_csv(&r, &columns, MAX_RESULT_CSV_BYTES).unwrap()).unwrap();
        assert_eq!(
            csv,
            "feature,p_value,note\nf1,6.12498179243933e-29,\"\"\n\"a,b \"\"c\"\"\",NA,\"line\nbreak\"\n\"NA\",0.5,NA\n"
        );
        let stats =
            String::from_utf8(statistics_csv(&r, MAX_STATISTICS_BYTES).unwrap().unwrap()).unwrap();
        assert_eq!(
            stats,
            "statistic,value\nn,30\np_value,6.12498179243933e-29\ntest,t\n"
        );
        assert!(
            statistics_csv(&json!({"stats": null}), MAX_STATISTICS_BYTES)
                .unwrap()
                .is_none()
        );
    }

    /// The results table as the next analysis, a figure and the table view read it back: every row, and each value as
    /// R reads the file.
    #[test]
    fn a_results_table_reads_back_with_every_row() {
        let r = json!({"columns": ["label", "flag", "value"], "rows": [
            {"label": "a, \"b\"\nc", "flag": true, "value": 1.5},
            {"label": "", "flag": false, "value": null},
            {"label": "NA", "value": 2},
            {"label": null, "flag": null, "value": -0.25}]});
        let columns = result_columns(&r).unwrap();
        let csv = results_csv(&r, &columns, MAX_RESULT_CSV_BYTES).unwrap();
        assert_eq!(
            String::from_utf8(csv.clone()).unwrap(),
            "label,flag,value\n\"a, \"\"b\"\"\nc\",TRUE,1.5\n\"\",FALSE,NA\n\"NA\",NA,2\nNA,NA,-0.25\n"
        );
        // The render service: each cell's text, which it writes into the data.csv its template reads with R.
        let (read_columns, rows) = figure_run::parse_table(&csv).unwrap();
        assert_eq!(read_columns, columns);
        assert_eq!(
            rows,
            [
                [json!("a, \"b\"\nc"), json!("TRUE"), json!("1.5")],
                [json!(""), json!("FALSE"), json!("NA")],
                [json!("NA"), json!("NA"), json!("2")],
                [json!("NA"), json!("NA"), json!("-0.25")]
            ]
        );
        // The table view (rules version 1): every row; empty and `NA` are missing.
        let table = super::super::tables::parse(&csv, b',', "x".repeat(64)).unwrap();
        assert_eq!(table.rows(), 4);
        let summary = &table.summary().0;
        let missing: Vec<(&str, usize, usize)> = summary
            .iter()
            .map(|c| (c.name.as_str(), c.count, c.missing))
            .collect();
        assert_eq!(missing, [("label", 1, 3), ("flag", 2, 2), ("value", 3, 1)]);
        // A one-column table of missing and empty values keeps every row.
        let one = json!({"columns": ["x"], "rows": [{"x": null}, {"x": ""}, {}]});
        let csv = results_csv(&one, &result_columns(&one).unwrap(), MAX_RESULT_CSV_BYTES).unwrap();
        assert_eq!(String::from_utf8(csv.clone()).unwrap(), "x\nNA\n\"\"\nNA\n");
        assert_eq!(figure_run::parse_table(&csv).unwrap().1.len(), 3);
        let table = super::super::tables::parse(&csv, b',', "y".repeat(64)).unwrap();
        assert_eq!(table.rows(), 3);
    }

    #[test]
    fn a_number_keeps_the_digits_the_service_wrote() {
        // As the service sends them: Python's shortest text for each double, at most 15 significant digits.
        for token in [
            "0.1",
            "6.12498179243933e-29",
            "123456789012345",
            "1234567890.12345",
            "0.000123456789012345",
            "-9.99999999999999e+307",
            "2.5e-05",
        ] {
            let answer = format!("{{\"results_exact\": {{\"v\": {token}}}}}");
            let read = budgeted_json::read(answer.as_bytes(), ANSWER_FIELDS, 1 << 16).unwrap();
            let mut written = String::new();
            Field::of(&read["results_exact"]["v"]).write(&mut written);
            assert_eq!(
                written.parse::<f64>().unwrap().to_bits(),
                token.parse::<f64>().unwrap().to_bits(),
                "{token}"
            );
            let digits = |text: &str| -> String {
                let mantissa = text.split(['e', 'E']).next().unwrap();
                let digits: String = mantissa.chars().filter(char::is_ascii_digit).collect();
                digits.trim_matches('0').to_owned()
            };
            assert_eq!(digits(&written), digits(token), "{token} as {written}");
        }
        // More digits than a double holds are kept as the nearest double.
        let read = budgeted_json::read(
            br#"{"results_exact": {"v": 0.10000000000000001}}"#,
            ANSWER_FIELDS,
            1 << 16,
        )
        .unwrap();
        let mut written = String::new();
        Field::of(&read["results_exact"]["v"]).write(&mut written);
        assert_eq!(written, "0.1");
    }

    #[test]
    fn a_table_past_its_limit_is_refused_before_it_is_copied() {
        let r = json!({"columns": ["a"], "rows": [{"a": "x".repeat(100)}, {"a": "y"}]});
        let columns = result_columns(&r).unwrap();
        // The header, then the long row with its line feed, then "y" and its line feed.
        let exact = 2 + 101 + 2;
        assert_eq!(results_csv(&r, &columns, exact).unwrap().len(), exact);
        assert_eq!(
            results_csv(&r, &columns, exact - 1).unwrap_err().code(),
            "analysis_result_too_large"
        );
        // A cell larger than the limit is never copied: the text's room stays within the limit.
        let mut csv = CsvText::new(64);
        assert!(csv.line([Field::Plain(Cow::Borrowed("a"))]));
        let big = "z".repeat(1000);
        assert!(!csv.line([Field::Text(Cow::Borrowed(big.as_str()))]));
        assert_eq!(csv.text, "a\n");
        assert!(csv.text.capacity() <= 64);
        let stats = json!({"stats": {"note": "s".repeat(100)}});
        assert!(statistics_csv(&stats, 200).unwrap().is_some());
        assert_eq!(
            statistics_csv(&stats, 100).unwrap_err().code(),
            "analysis_result_too_large"
        );
        let record = json!({"rows": [{"a": "r".repeat(100)}]});
        let pretty = record_json(&record, 1 << 16).unwrap();
        assert_eq!(record_json(&record, pretty.len()).unwrap(), pretty);
        assert!(record_json(&record, pretty.len() - 1).is_none());
    }

    fn task() -> ClaimedTask {
        ClaimedTask {
            id: uuid::Uuid::nil(),
            principal_id: uuid::Uuid::nil(),
            group_key: "lab".into(),
            profile_key: "template-analysis".into(),
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

    fn request() -> TaskAnalysis {
        TaskAnalysis {
            source_task: uuid::Uuid::nil(),
            package_path: "tasks/x/attempts/1-1/exports/payload".into(),
            package_sha256: "a".repeat(64),
            package_bytes: 1,
            table_path: "results/query_result.csv".into(),
            table_sha256: "b".repeat(64),
            table_rows: 2,
            feature: "group_compare".into(),
            settings: json!({"value": "amount", "group": "group"}),
            title: "".into(),
            request_sha256: "c".repeat(64),
            catalog_sha256: "d".repeat(64),
        }
    }

    fn ran(record: Value, notes: Vec<String>) -> Ran {
        let columns = result_columns(&record).unwrap();
        Ran {
            record,
            columns,
            png: None,
            svg: None,
            template: "x".into(),
            common: "y".into(),
            settings: json!({"params": {}}),
            service_version: "0.6.0".into(),
            image_digest: None,
            r_session: None,
            notes,
        }
    }

    #[test]
    fn a_package_keeps_notes_within_the_receipt() {
        let template_note = "t".repeat(MAX_NOTE_CHARS);
        let record = json!({"columns": ["a"], "rows": [{"a": 1}], "notes": template_note});
        let service: Vec<String> = (0..40).map(|_| "é".repeat(MAX_NOTE_CHARS)).collect();
        let (_, _, notes) = package(&task(), &request(), &ran(record, service)).unwrap();
        let kept = notes.as_array().unwrap();
        assert_eq!(kept[0], json!(template_note));
        assert!(figure_run::stored_bytes(&notes) <= MAX_NOTES_BYTES);
        // Each note kept takes 600 bytes or more, so the bound left room for less than one more.
        assert!(figure_run::stored_bytes(&notes) + 604 > MAX_NOTES_BYTES);
    }

    #[test]
    fn results_of_the_labs_own_data_go_where_the_lead_reads_no_cells() {
        let record = json!({"columns": ["a"], "rows": [{"a": 1}], "stats": {"p": 0.5}});
        for (table, results) in [
            ("results/query_result.csv", "results/results.csv"),
            (
                "results/lab_data/gtt.csv",
                "results/lab_derived/results.csv",
            ),
            (
                "results/lab_derived/results.csv",
                "results/lab_derived/results.csv",
            ),
        ] {
            let mut asked = request();
            asked.table_path = table.into();
            let (_, files, _) = package(&task(), &asked, &ran(record.clone(), vec![])).unwrap();
            let paths: Vec<&str> = files
                .as_array()
                .unwrap()
                .iter()
                .filter_map(|f| f["path"].as_str())
                .collect();
            assert!(paths.contains(&results), "{table}: {paths:?}");
            assert!(paths.contains(&"results/statistics.csv"), "{table}");
            assert_eq!(
                readme(&asked, "code/group_compare.R", "0.6.0", None, false)
                    .contains("lab_derived"),
                results.contains("lab_derived"),
                "{table}"
            );
        }
    }

    #[test]
    fn a_package_refuses_results_past_their_limits() {
        let refused = |record: Value| {
            package(&task(), &request(), &ran(record, vec![]))
                .unwrap_err()
                .code()
        };
        let stats = json!({"columns": ["a"], "rows": [{"a": 1}], "stats": {"s": "x".repeat(MAX_STATISTICS_BYTES)}});
        assert_eq!(refused(stats), "analysis_result_too_large");
        let cell = json!({"columns": ["a"], "rows": [{"a": "x".repeat(MAX_RESULT_CSV_BYTES)}]});
        assert_eq!(refused(cell), "analysis_result_too_large");
        let method =
            json!({"columns": ["a"], "rows": [{"a": 1}], "method": "m".repeat(MAX_RECORD_BYTES)});
        assert_eq!(refused(method), "analysis_result_too_large");
        let small =
            json!({"columns": ["a"], "rows": [{"a": 1}], "stats": {"p": 0.5}, "method": "m"});
        assert!(package(&task(), &request(), &ran(small, vec![])).is_ok());
    }

    #[test]
    fn a_record_with_unknown_cells_or_repeated_columns_is_refused() {
        let unknown = record(json!([{"feature": "f1", "other": 1}]));
        assert_eq!(
            result_columns(&unknown).unwrap_err().code(),
            "analysis_result_unreadable"
        );
        let repeated = json!({"columns": ["a", "a"], "rows": []});
        assert_eq!(
            result_columns(&repeated).unwrap_err().code(),
            "analysis_result_unreadable"
        );
        let not_a_row = record(json!([["f1", 0.1, ""]]));
        assert!(result_columns(&not_a_row).is_err());
        let long = json!({"columns": ["x".repeat(MAX_COLUMN_NAME_CHARS + 1)], "rows": []});
        assert!(result_columns(&long).is_err());
        let wide = json!({"columns": (0..=MAX_RESULT_COLUMNS).map(|i| format!("c{i}")).collect::<Vec<_>>(), "rows": []});
        assert!(result_columns(&wide).is_err());
        let fits = json!({"columns": (0..MAX_RESULT_COLUMNS).map(|i| format!("c{i}")).collect::<Vec<_>>(), "rows": []});
        assert!(result_columns(&fits).is_ok());
    }

    #[test]
    fn too_many_result_rows_are_refused_at_the_limit() {
        let at = json!({"columns": ["a"], "rows": vec![json!({"a": 1}); MAX_RESULT_ROWS]});
        assert!(result_columns(&at).is_ok());
        let past = json!({"columns": ["a"], "rows": vec![json!({"a": 1}); MAX_RESULT_ROWS + 1]});
        assert_eq!(
            result_columns(&past).unwrap_err().code(),
            "analysis_result_too_large"
        );
    }

    #[test]
    fn an_answer_without_the_full_record_is_from_an_outdated_service() {
        let answer = json!({"results": {"columns": ["a"], "rows": []}, "spec": {"params": {}},
            "service_version": "0.5.22", "code": "x", "common_code": "y"});
        assert_eq!(
            checked(answer.clone()).unwrap_err().code(),
            "render_service_outdated"
        );
        let mut current = answer.clone();
        current["results_exact"] = json!({"columns": ["a"], "rows": [{"a": 1}]});
        assert!(checked(current.clone()).is_ok());
        // A figure comes as a PNG and an SVG together, or not at all.
        current["svg"] = json!("<svg xmlns='http://www.w3.org/2000/svg'/>");
        assert_eq!(
            checked(current).unwrap_err().code(),
            "render_response_unreadable"
        );
    }

    #[test]
    fn current_render_services_require_a_strictly_valid_r_session() {
        let mut answer = current_answer();
        answer["session_info"] = r_session();
        assert_eq!(checked(answer).unwrap().r_session, Some(r_session()));

        assert_eq!(
            checked(current_answer()).unwrap_err().code(),
            "render_session_info_invalid"
        );

        // The record as render-service 0.7.1's write_session_info writes it (R 4.6.0), and the empty values its
        // _read_session_info accepts when R reports no OS, library or LAPACK version.
        let mut valid = vec![json!({
            "r_version": "R version 4.6.0 (2026-04-24)",
            "platform": "aarch64-apple-darwin23",
            "running": "macOS 27.2",
            "locale": "en_US.UTF-8/en_US.UTF-8/en_US.UTF-8/C/en_US.UTF-8/en_US.UTF-8",
            "blas": "libRblas.0.dylib",
            "lapack": "libRlapack.dylib",
            "lapack_version": "3.12.1",
            "base_packages": ["base", "datasets", "graphics", "grDevices", "methods", "stats", "utils"],
            "attached": ["jsonlite"],
            "packages": {"compiler": "4.6.0", "jsonlite": "2.0.0"}
        })];
        for key in ["platform", "running", "blas", "lapack", "lapack_version"] {
            let mut empty = r_session();
            empty[key] = json!("");
            valid.push(empty);
        }
        let mut longest = r_session();
        longest["locale"] = json!("x".repeat(400));
        valid.push(longest);
        for session in valid {
            let mut answer = current_answer();
            answer["session_info"] = session.clone();
            assert_eq!(checked(answer).unwrap().r_session, Some(session));
        }

        let mut invalid = Vec::new();
        let mut missing = r_session();
        missing.as_object_mut().unwrap().remove("lapack_version");
        invalid.push(missing);
        let mut extra = r_session();
        extra["hostname"] = json!("node1");
        invalid.push(extra);
        let mut lapack_version = r_session();
        lapack_version["lapack_version"] = json!("3.12 beta");
        invalid.push(lapack_version);
        let mut hidden = r_session();
        hidden["lapack"] = json!(".liblapack.so");
        invalid.push(hidden);
        let mut no_packages = r_session();
        no_packages["packages"] = json!({});
        invalid.push(no_packages);
        let mut control = r_session();
        control["running"] = json!("Linux\nforged");
        invalid.push(control);
        let mut path = r_session();
        path["blas"] = json!("/usr/lib/libblas.so");
        invalid.push(path);
        let mut too_many = r_session();
        too_many["packages"] = Value::Object(
            (0..513)
                .map(|index| (format!("Package{index}"), json!("1.0")))
                .collect(),
        );
        invalid.push(too_many);
        let mut too_long = r_session();
        too_long["locale"] = json!("x".repeat(401));
        invalid.push(too_long);
        let mut oversized = r_session();
        let long_names: Vec<Value> = (0..512)
            .map(|index| json!(format!("P{index:03}{}", "x".repeat(59))))
            .collect();
        oversized["base_packages"] = Value::Array(long_names.clone());
        oversized["attached"] = Value::Array(long_names);
        assert!(serde_json::to_vec(&oversized).unwrap().len() > MAX_R_SESSION_BYTES);
        invalid.push(oversized);

        for session in invalid {
            let mut answer = current_answer();
            answer["session_info"] = session;
            assert_eq!(
                checked(answer).unwrap_err().code(),
                "render_session_info_invalid"
            );
        }
    }

    #[test]
    fn older_render_services_omit_the_r_session_and_versions_compare_numerically() {
        let mut older = current_answer();
        older["service_version"] = json!("0.7.0");
        assert!(checked(older).unwrap().r_session.is_none());

        let mut newer = current_answer();
        newer["service_version"] = json!("0.7.10");
        assert_eq!(
            checked(newer).unwrap_err().code(),
            "render_session_info_invalid"
        );
    }

    #[test]
    fn a_package_records_the_valid_r_session_and_older_answers_omit_it() {
        let record = json!({"columns": ["a"], "rows": [{"a": 1}]});
        let mut with_session = ran(record.clone(), vec![]);
        with_session.service_version = "0.7.1".into();
        with_session.r_session = Some(r_session());
        let (package_bytes, _, _) = package(&task(), &request(), &with_session).unwrap();
        let spec: Value =
            serde_json::from_slice(&package_member(&package_bytes, "spec.json").unwrap()).unwrap();
        assert_eq!(spec["service"]["r_session"], r_session());
        let readme =
            String::from_utf8(package_member(&package_bytes, "README.md").unwrap()).unwrap();
        assert!(readme.contains("R version 4.4.3 ran with 3 loaded packages"));
        assert!(readme.contains("`spec.json` records the full R session"));

        let (older, _, _) = package(&task(), &request(), &ran(record, vec![])).unwrap();
        let spec: Value =
            serde_json::from_slice(&package_member(&older, "spec.json").unwrap()).unwrap();
        assert!(spec["service"].get("r_session").is_none());
    }

    #[test]
    fn refusals_take_the_brokers_words_and_never_the_services() {
        let columns = vec!["group".to_owned(), "value".to_owned()];
        let out = json!({"error": "group_compare needs exactly 2 groups in 'group'; found 3 (cell text)",
            "refusal": {"code": "group_count_not_two", "columns": ["group"], "counts": [3]}});
        let stop = service_stop(&out, &columns, false);
        assert_eq!((stop.status, stop.code), ("refused", "analysis_refused"));
        assert_eq!(
            stop.message.as_deref(),
            Some("This analysis compares exactly two groups; group has 3.")
        );
        // A column the table does not have, or an unknown code, gets the general words.
        for refusal in [
            json!({"code": "group_count_not_two", "columns": ["cell text"], "counts": [3]}),
            json!({"code": "not_a_code", "columns": [], "counts": []}),
        ] {
            let stop = service_stop(&json!({"error": "x", "refusal": refusal}), &columns, false);
            assert_eq!(
                (stop.status, stop.message.as_deref()),
                ("failed", Some(NOT_RUN))
            );
            assert!(stop.refusal.is_none());
        }
        let stop = service_stop(&json!({"error": "bad setting"}), &columns, true);
        assert_eq!(
            (stop.status, stop.message.as_deref()),
            ("refused", Some(SERVICE_REFUSED))
        );
    }

    #[test]
    fn every_refusal_the_templates_send_has_words_that_fill() {
        // The columns and counts each template passes with its code (render README, "Refusals").
        let sent: &[(&str, &[&str], &[u64])] = &[
            ("column_missing", &[], &[]),
            ("group_count_not_two", &["group"], &[3]),
            ("group_one_level", &["group"], &[]),
            ("no_complete_rows", &["id", "feature"], &[]),
            ("no_factor_with_levels", &["sex", "diet"], &[]),
            ("no_measure_with_pairs", &[], &[3]),
            ("no_numeric_values", &["value"], &[]),
            ("nothing_above_threshold", &["lod"], &[]),
            ("nothing_to_test", &[], &[3]),
            ("outcome_not_numeric", &["outcome"], &[]),
            ("outcome_one_class", &["outcome"], &[]),
            ("power_n_or_power_missing", &[], &[]),
            ("power_pve_missing", &[], &[]),
            ("power_settings_incomplete", &[], &[]),
            ("setting_invalid", &[], &[]),
            ("setting_missing", &[], &[]),
            ("too_few_complete_rows", &["x", "y"], &[3]),
            ("too_few_samples", &["id"], &[20]),
            ("too_few_values_per_group", &["group"], &[2]),
            ("too_few_varying_columns", &[], &[2]),
            ("time_not_numeric", &["week"], &[]),
            ("factor_varies_within_animal", &["mouse", "diet"], &[1]),
            ("repeated_time_rows", &["mouse", "week"], &[1]),
            ("too_few_time_points", &["week"], &[2, 3]),
            ("too_few_animals", &["mouse"], &[1, 2]),
            (
                "kept_column_varies_within_animal",
                &["mouse", "body_mass_g"],
                &[1],
            ),
            ("no_rows_in_window", &["hour", "phase"], &[]),
            ("no_rows_in_window", &["phase"], &[]),
            ("repeated_rows_per_animal", &["mouse"], &[8]),
            ("too_few_animals_per_group", &["diet"], &[1, 2]),
            ("empty_group_cell", &["diet", "sex"], &[1]),
            ("model_not_estimable", &["ee", "diet", "body_mass_g"], &[]),
            ("model_did_not_converge", &["mouse", "week", "weight"], &[]),
        ];
        let columns: Vec<String> = [
            "group",
            "id",
            "feature",
            "sex",
            "diet",
            "value",
            "lod",
            "outcome",
            "x",
            "y",
            "week",
            "mouse",
            "body_mass_g",
            "hour",
            "phase",
            "ee",
            "weight",
        ]
        .iter()
        .map(|c| c.to_string())
        .collect();
        for (code, named, counts) in sent {
            let refusal = json!({"code": code, "columns": named, "counts": counts});
            assert!(
                figure_run::approve(REFUSALS, &refusal, &columns).is_some(),
                "{code} has no words that fill"
            );
        }
    }

    #[test]
    fn shared_stops_are_named_for_analyses() {
        for code in [
            "figure_source_unreadable",
            "figure_source_changed",
            "figure_table_too_large",
            "figure_table_missing",
            "figure_table_unreadable",
            "figure_table_unsuitable",
            "figure_package_failed",
            "figure_package_not_registered",
            "figure_digest_failed",
        ] {
            let renamed = renamed(figure_run::refused(code)).code();
            assert!(renamed.starts_with("analysis_"), "{code} became {renamed}");
        }
    }

    #[test]
    fn text_from_the_service_loses_controls_and_direction_marks() {
        assert_eq!(plain_text("a\u{202E}b\u{0007}\tc\nd", 100), "ab c d");
        assert_eq!(plain_text(&"x".repeat(10), 5).chars().count(), 5);
    }

    #[test]
    fn only_figure_files_of_the_completed_package_are_declared() {
        let detail = json!({"receipts": [
            {"status": "completed", "package_sha256": "a".repeat(64), "files": [
                {"path": "results/results.csv", "sha256": "1".repeat(64), "role": "table"},
                {"path": "figures/figure.png", "sha256": "2".repeat(64), "role": "figure"},
                {"path": "figures/figure.svg", "sha256": "3".repeat(64), "role": "figure"}]},
            {"status": "failed", "package_sha256": null, "files": null}]});
        let declared = declared_figures(&detail, &"a".repeat(64));
        assert_eq!(
            declared.iter().map(|d| d.path.as_str()).collect::<Vec<_>>(),
            ["figures/figure.png", "figures/figure.svg"]
        );
        assert!(declared_figures(&detail, &"b".repeat(64)).is_empty());
        assert!(declared_figures(&Value::Null, &"a".repeat(64)).is_empty());
    }
}
