//! The guard's HTTP interface.
//!
//! Callers: `POST /v1/intake` (the file as the body; headers name the use case, tenant, lab, project, person and the
//! declared media type), `GET /v1/intake/{id}` (the record's metadata) and `GET /v1/intake/{id}/disarmed` (the rebuilt
//! copy of an allowed item, to the caller that sent it). A fetcher reads the same two for the callers and use cases it
//! names, and sends nothing. Operators: `POST /v1/exceptions`. A second listener serves
//! `/healthz`, `/readyz` and `/metrics` and is reachable only by the kubelet and the metrics collector.
use crate::config::{Callers, Principal};
use crate::detect::FileType;
use crate::pipeline::{self, Engines, Exception, Outcome};
use crate::policy::{self, Action, Policy};
use crate::store::{self, Disarmed, Record, Refusal, Retention, Store, Usage};
use axum::Json;
use axum::Router;
use axum::body::{Body, Bytes};
use axum::extract::{DefaultBodyLimit, Path as UrlPath, State};
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use http_body_util::BodyExt;
use serde::Deserialize;
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, RwLock};
use std::time::{Duration, Instant};
use tokio::sync::Semaphore;
use uuid::Uuid;

/// How long the same bytes from the same person return the earlier intake instead of a new one.
const RETRY_WINDOW_MINUTES: i64 = 60;
pub const CALLERS_RELOAD: Duration = Duration::from_secs(10);
/// How long a caller may take to send a file once it holds a permit.
pub const BODY_WAIT: Duration = Duration::from_secs(300);

pub struct App {
    pub engines: Engines,
    pub policy: Policy,
    pub callers_file: PathBuf,
    pub callers: RwLock<(Instant, Callers)>,
    /// How often the callers file is read again, so a removed digest stops working.
    pub callers_reload: Duration,
    pub store: Store,
    pub permits: Semaphore,
    pub queue_wait: Duration,
    pub body_wait: Duration,
    pub metrics: Metrics,
}

#[derive(Default)]
pub struct Metrics {
    counters: Mutex<BTreeMap<String, u64>>,
}

impl Metrics {
    pub fn add(&self, name: &str, labels: &[(&str, &str)], value: u64) {
        let rendered: Vec<String> = labels.iter().map(|(k, v)| format!("{k}=\"{v}\"")).collect();
        let key = if rendered.is_empty() {
            name.to_string()
        } else {
            format!("{name}{{{}}}", rendered.join(","))
        };
        *self.counters.lock().unwrap().entry(key).or_insert(0) += value;
    }

    pub fn render(&self) -> String {
        let counters = self.counters.lock().unwrap();
        let mut out = String::new();
        let mut typed = std::collections::BTreeSet::new();
        for (key, value) in counters.iter() {
            let name = key.split('{').next().unwrap_or(key);
            if typed.insert(name.to_string()) {
                out.push_str(&format!("# TYPE {name} counter\n"));
            }
            out.push_str(&format!("{key} {value}\n"));
        }
        out
    }
}

fn error(status: StatusCode, code: &str, message: &str) -> Response {
    (status, Json(json!({"error": code, "message": message}))).into_response()
}

/// A store write that failed: full (507) when the cap refused it, otherwise an internal failure.
fn store_error(e: &anyhow::Error, message: &str) -> Response {
    if e.downcast_ref::<store::StoreFull>().is_some() {
        error(
            StatusCode::INSUFFICIENT_STORAGE,
            "store_full",
            "the guard's quarantine store is full",
        )
    } else {
        error(StatusCode::INTERNAL_SERVER_ERROR, "store_failed", message)
    }
}

impl App {
    fn principal(&self, headers: &HeaderMap) -> Option<Principal> {
        let token = headers
            .get(header::AUTHORIZATION)?
            .to_str()
            .ok()?
            .strip_prefix("Bearer ")?;
        {
            let guard = self.callers.read().unwrap();
            if guard.0.elapsed() < self.callers_reload {
                return guard.1.authenticate(token);
            }
        }
        let mut guard = self.callers.write().unwrap();
        if guard.0.elapsed() >= self.callers_reload {
            match Callers::load(&self.callers_file) {
                Ok(fresh) => *guard = (Instant::now(), fresh),
                Err(e) => {
                    tracing::error!(target: "guard.callers", error = %e, "callers file unreadable; refusing every token");
                    *guard = (
                        Instant::now(),
                        Callers {
                            schema: String::new(),
                            callers: vec![],
                            fetchers: vec![],
                            operators: vec![],
                        },
                    );
                }
            }
        }
        guard.1.authenticate(token)
    }
}

fn header_text(headers: &HeaderMap, name: &str) -> Option<String> {
    headers
        .get(name)
        .and_then(|v| v.to_str().ok())
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

/// A percent-encoded UTF-8 file name, without control characters or path separators, at most 255 characters.
fn file_name(headers: &HeaderMap) -> Option<String> {
    let raw = header_text(headers, "x-guard-file-name")?;
    let mut bytes = Vec::new();
    let mut chars = raw.bytes();
    while let Some(b) = chars.next() {
        if b == b'%' {
            let hi = chars.next()?;
            let lo = chars.next()?;
            bytes.push(u8::from_str_radix(std::str::from_utf8(&[hi, lo]).ok()?, 16).ok()?);
        } else {
            bytes.push(b);
        }
    }
    let text = String::from_utf8(bytes).ok()?;
    let clean: String = text
        .chars()
        .filter(|c| !c.is_control() && *c != '/' && *c != '\\')
        .take(255)
        .collect();
    (!clean.trim().is_empty()).then_some(clean)
}

pub fn view(record: &Record, retry: bool) -> Value {
    let reasons: Vec<&str> = record
        .findings
        .iter()
        .filter(|f| f.action > Action::Allow)
        .map(|f| f.explanation.as_str())
        .collect();
    json!({
        "intake_id": record.intake_id,
        "outcome": record.outcome,
        "refusal": record.refusal,
        "findings": record.findings,
        "reasons": reasons,
        "disarmed": record.disarmed,
        "policy_version": record.policy_version,
        "detected_type": record.detected_type,
        "sha256": record.sha256,
        "retry": retry,
    })
}

struct Intake {
    caller: String,
    tenant: String,
    lab: String,
    project: Option<String>,
    person: String,
    use_case: String,
    declared: FileType,
    file_name: Option<String>,
}

fn parse_intake(app: &App, headers: &HeaderMap) -> Result<Intake, Box<Response>> {
    let Some(principal) = app.principal(headers) else {
        return Err(Box::new(error(
            StatusCode::UNAUTHORIZED,
            "unauthorized",
            "a registered caller token is required",
        )));
    };
    let Principal::Caller { id, use_cases } = principal else {
        return Err(Box::new(error(
            StatusCode::FORBIDDEN,
            "caller_required",
            "only callers send files",
        )));
    };
    let use_case = header_text(headers, "x-guard-use-case").unwrap_or_default();
    if !use_cases.contains(&use_case) || app.policy.use_case(&use_case).is_none() {
        return Err(Box::new(error(
            StatusCode::FORBIDDEN,
            "use_case_not_permitted",
            "this caller may not use that use case",
        )));
    }
    let name = |h: &str| header_text(headers, h).filter(|v| policy::valid_name(v));
    let (Some(tenant), Some(lab)) = (name("x-guard-tenant"), name("x-guard-lab")) else {
        return Err(Box::new(error(
            StatusCode::UNPROCESSABLE_ENTITY,
            "scope_invalid",
            "tenant and lab must be short lowercase names",
        )));
    };
    let project = match header_text(headers, "x-guard-project") {
        None => None,
        Some(p) if policy::valid_name(&p) || Uuid::parse_str(&p).is_ok() => Some(p),
        Some(_) => {
            return Err(Box::new(error(
                StatusCode::UNPROCESSABLE_ENTITY,
                "scope_invalid",
                "the project is not a name or UUID",
            )));
        }
    };
    let person = header_text(headers, "x-guard-person")
        .filter(|p| p.len() <= 256 && p.bytes().all(|b| b.is_ascii_graphic()));
    let Some(person) = person else {
        return Err(Box::new(error(
            StatusCode::UNPROCESSABLE_ENTITY,
            "person_invalid",
            "the person must be a printable identifier",
        )));
    };
    let declared = header_text(headers, "content-type").and_then(|t| FileType::from_media_type(&t));
    let Some(declared) = declared else {
        return Err(Box::new(error(
            StatusCode::UNSUPPORTED_MEDIA_TYPE,
            "type_not_accepted",
            "send a PDF, PNG or JPEG file, or a table as text/csv, text/tab-separated-values or an .xlsx workbook",
        )));
    };
    let person = store::person_digest(&tenant, &person);
    Ok(Intake {
        caller: id,
        tenant,
        lab,
        project,
        person,
        use_case,
        declared,
        file_name: file_name(headers),
    })
}

/// What is known about an intake before it is evaluated.
struct Received<'a> {
    id: &'a str,
    at: time::OffsetDateTime,
    digest: &'a str,
    bytes: u64,
    usage: Usage,
}

fn refusal_record(app: &App, intake: &Intake, received: &Received, reason: &str) -> Record {
    let Received {
        id,
        at,
        digest,
        bytes,
        ..
    } = *received;
    let usage = received.usage.clone();
    let case = app.policy.use_case(&intake.use_case).expect("checked");
    Record {
        schema: store::RECORD_SCHEMA.into(),
        intake_id: id.into(),
        received_at: store::stamp(at),
        completed_at: store::stamp(store::now()),
        caller: intake.caller.clone(),
        tenant: intake.tenant.clone(),
        lab: intake.lab.clone(),
        project: intake.project.clone(),
        person: intake.person.clone(),
        use_case: intake.use_case.clone(),
        file_name: intake.file_name.clone(),
        bytes,
        sha256: digest.into(),
        declared_type: intake.declared,
        detected_type: FileType::Unknown,
        policy_version: app.policy.version.clone(),
        versions: app.engines.versions(None),
        findings: vec![],
        outcome: Outcome::Refused,
        refusal: Some(reason.into()),
        disarmed: None,
        exception: None,
        retention: Retention {
            original_until: store::stamp(at),
            record_until: store::stamp(
                at + time::Duration::days(case.retention.record_days.into()),
            ),
        },
        usage,
        retries: vec![],
        retry_count: 0,
    }
}

fn decision_log(record: &Record, started: Instant) {
    let codes: Vec<&str> = record.findings.iter().map(|f| f.code.as_str()).collect();
    tracing::info!(target: "guard.decision", intake_id = %record.intake_id, caller = %record.caller,
        use_case = %record.use_case, outcome = ?record.outcome, refusal = record.refusal.as_deref().unwrap_or(""),
        findings = %codes.join(","), bytes = record.bytes, detected = ?record.detected_type,
        duration_ms = started.elapsed().as_millis() as u64, policy = %record.policy_version,
        clamav = record.versions.clamav.as_deref().unwrap_or(""), rules = %record.versions.yara_rules);
}

enum Upload {
    Read(Vec<u8>),
    TooLarge,
    Broken,
}

/// Reads the request body up to `limit` bytes, stopping as soon as it passes the limit.
async fn read_body(body: Body, limit: u64) -> Upload {
    let mut body = body;
    let mut out = Vec::new();
    while let Some(frame) = body.frame().await {
        let Ok(frame) = frame else {
            return Upload::Broken;
        };
        if let Ok(data) = frame.into_data() {
            if out.len() as u64 + data.len() as u64 > limit {
                return Upload::TooLarge;
            }
            out.extend_from_slice(&data);
        }
    }
    Upload::Read(out)
}

async fn intake(State(app): State<Arc<App>>, headers: HeaderMap, body: Body) -> Response {
    let started = Instant::now();
    let intake = match parse_intake(&app, &headers) {
        Ok(i) => i,
        Err(response) => {
            app.metrics
                .add("guard_rejected_requests_total", &[("reason", "request")], 1);
            return *response;
        }
    };
    let case = app
        .policy
        .use_case(&intake.use_case)
        .expect("checked")
        .clone();
    // Refused before anything is stored: logged and counted, never recorded.
    let turn_away = |reason: &str, status: StatusCode, message: &str| {
        tracing::info!(target: "guard.decision", caller = %intake.caller, use_case = %intake.use_case,
            outcome = "refused", refusal = reason, duration_ms = started.elapsed().as_millis() as u64);
        app.metrics
            .add("guard_refusals_total", &[("reason", reason)], 1);
        let mut response = error(status, reason, message);
        if status == StatusCode::TOO_MANY_REQUESTS {
            response
                .headers_mut()
                .insert(header::RETRY_AFTER, HeaderValue::from_static("5"));
        }
        response
    };
    let too_large = || {
        turn_away(
            "too_large",
            StatusCode::PAYLOAD_TOO_LARGE,
            "the file is larger than this upload accepts",
        )
    };
    // The body is read only for a known caller holding a permit, so at most `max_concurrent` bodies, each within its
    // use case's size limit, are in memory at once.
    let declared = headers
        .get(header::CONTENT_LENGTH)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.parse::<u64>().ok());
    if declared.is_some_and(|n| n > case.max_bytes) {
        return too_large();
    }
    let Ok(Ok(_permit)) = tokio::time::timeout(app.queue_wait, app.permits.acquire()).await else {
        return turn_away(
            "busy",
            StatusCode::TOO_MANY_REQUESTS,
            "the guard is checking other files; retry shortly",
        );
    };
    let bytes = match tokio::time::timeout(app.body_wait, read_body(body, case.max_bytes)).await {
        Ok(Upload::Read(bytes)) => Arc::new(bytes),
        Ok(Upload::TooLarge) => return too_large(),
        Ok(Upload::Broken) => {
            return turn_away(
                "body_unreadable",
                StatusCode::BAD_REQUEST,
                "the file could not be read",
            );
        }
        Err(_) => {
            return turn_away(
                "body_timeout",
                StatusCode::REQUEST_TIMEOUT,
                "the file was not sent in time",
            );
        }
    };
    let at = store::now();
    let digest = store::sha256(&bytes);
    let store = &app.store;
    // A retry returns the earlier decision, unless it was a quarantine an exception has since released.
    let earlier = store
        .recent(
            &intake.tenant,
            &intake.person,
            &intake.use_case,
            &digest,
            time::Duration::minutes(RETRY_WINDOW_MINUTES),
            at,
        )
        .filter(|r| {
            r.outcome != Outcome::Quarantined
                || store.exception(&intake.use_case, &digest, at).is_none()
        });
    if let Some(mut earlier) = earlier {
        earlier.note_retry(at);
        if let Err(e) = store.write_record(&earlier) {
            tracing::error!(target: "guard.store", error = %e, "retry not recorded");
            return store_error(&e, "the intake could not be recorded");
        }
        app.metrics
            .add("guard_retries_total", &[("use_case", &intake.use_case)], 1);
        return (StatusCode::OK, Json(view(&earlier, true))).into_response();
    }
    let reservation = match store.reserve(
        &intake.tenant,
        &intake.person,
        at,
        bytes.len() as u64,
        &case.quota,
    ) {
        Ok(reservation) => reservation,
        Err(Refusal::Quota) => {
            return turn_away(
                "quota_exceeded",
                StatusCode::TOO_MANY_REQUESTS,
                "this person's daily upload quota is used",
            );
        }
        Err(Refusal::Full) => {
            return turn_away(
                "store_full",
                StatusCode::INSUFFICIENT_STORAGE,
                "the guard's quarantine store is full",
            );
        }
    };
    let id = Uuid::now_v7().to_string();
    // Refused after the reservation: recorded and counted against the person's quota, without the original.
    let refuse = |reason: &str, status: StatusCode, message: &str| {
        let received = Received {
            id: &id,
            at,
            digest: &digest,
            bytes: bytes.len() as u64,
            usage: reservation.usage.clone(),
        };
        let record = refusal_record(&app, &intake, &received, reason);
        if let Err(e) = store.commit(&reservation, &record, None, None) {
            tracing::error!(target: "guard.store", error = %e, "refusal not recorded");
        }
        decision_log(&record, started);
        app.metrics
            .add("guard_refusals_total", &[("reason", reason)], 1);
        (
            status,
            Json(json!({"error": reason, "message": message, "intake_id": id})),
        )
            .into_response()
    };
    let exception = store.exception(&intake.use_case, &digest, at);
    let work = match store.work_dir(&id) {
        Ok(w) => w,
        Err(e) => {
            tracing::error!(target: "guard.store", error = %e, "work directory not created");
            return error(
                StatusCode::INTERNAL_SERVER_ERROR,
                "store_failed",
                "the intake could not start",
            );
        }
    };
    // Boxed: the evaluation's state is large, and a connection task polls it. The work directory goes when `work`
    // is dropped, here or when a cancelled request drops this future.
    let result = Box::pin(pipeline::evaluate(
        &app.engines,
        &case,
        intake.declared,
        Arc::clone(&bytes),
        work.path(),
        exception.as_ref(),
    ))
    .await;
    drop(work);
    let evaluation = match result {
        Ok(Ok(evaluation)) => evaluation,
        Ok(Err(unavailable)) => {
            tracing::warn!(target: "guard.scanner", reason = %unavailable.0, "scanner unavailable");
            return refuse(
                "scanner_unavailable",
                StatusCode::SERVICE_UNAVAILABLE,
                "malware scanning is unavailable; retry later",
            );
        }
        Err(e) => {
            tracing::error!(target: "guard.pipeline", error = %e, "evaluation failed");
            return refuse(
                "evaluation_failed",
                StatusCode::INTERNAL_SERVER_ERROR,
                "the file could not be checked",
            );
        }
    };
    let disarmed = evaluation.released.as_ref().map(|copy| Disarmed {
        sha256: store::sha256(copy),
        bytes: copy.len() as u64,
        media_type: evaluation.detected.rebuilt_media_type().into(),
        expires_at: store::stamp(
            at + time::Duration::minutes(case.retention.disarmed_minutes.into()),
        ),
    });
    let applied = evaluation
        .findings
        .iter()
        .any(|f| f.code == "exception_applied");
    let record = Record {
        schema: store::RECORD_SCHEMA.into(),
        intake_id: id.clone(),
        received_at: store::stamp(at),
        completed_at: store::stamp(store::now()),
        caller: intake.caller.clone(),
        tenant: intake.tenant.clone(),
        lab: intake.lab.clone(),
        project: intake.project.clone(),
        person: intake.person.clone(),
        use_case: intake.use_case.clone(),
        file_name: intake.file_name.clone(),
        bytes: bytes.len() as u64,
        sha256: digest.clone(),
        declared_type: intake.declared,
        detected_type: evaluation.detected,
        policy_version: app.policy.version.clone(),
        versions: evaluation.versions.clone(),
        findings: evaluation.findings.clone(),
        outcome: evaluation.outcome,
        refusal: None,
        disarmed,
        exception: if applied { exception } else { None },
        retention: Retention {
            original_until: store::stamp(
                at + time::Duration::days(case.retention.original_days.into()),
            ),
            record_until: store::stamp(
                at + time::Duration::days(case.retention.record_days.into()),
            ),
        },
        usage: reservation.usage.clone(),
        retries: vec![],
        retry_count: 0,
    };
    if let Err(e) = store.commit(
        &reservation,
        &record,
        Some(&bytes),
        evaluation.released.as_deref(),
    ) {
        tracing::error!(target: "guard.store", error = %e, "intake not recorded");
        return error(
            StatusCode::INTERNAL_SERVER_ERROR,
            "store_failed",
            "the intake could not be recorded",
        );
    }
    decision_log(&record, started);
    let outcome = format!("{:?}", record.outcome).to_lowercase();
    app.metrics.add(
        "guard_intakes_total",
        &[("use_case", &record.use_case), ("outcome", &outcome)],
        1,
    );
    for finding in &record.findings {
        app.metrics
            .add("guard_findings_total", &[("code", &finding.code)], 1);
    }
    app.metrics.add(
        "guard_intake_bytes_total",
        &[("use_case", &record.use_case)],
        record.bytes,
    );
    (StatusCode::OK, Json(view(&record, false))).into_response()
}

fn owned_record(
    app: &App,
    headers: &HeaderMap,
    id: &str,
) -> Result<(Record, Principal), Box<Response>> {
    let Some(principal) = app.principal(headers) else {
        return Err(Box::new(error(
            StatusCode::UNAUTHORIZED,
            "unauthorized",
            "a registered token is required",
        )));
    };
    let not_found = || {
        error(
            StatusCode::NOT_FOUND,
            "not_found",
            "no such intake for this caller",
        )
    };
    if Uuid::parse_str(id).is_err() {
        return Err(Box::new(not_found()));
    }
    let record = match app.store.read_record(id) {
        Ok(Some(r)) => r,
        _ => return Err(Box::new(not_found())),
    };
    if !matches!(principal, Principal::Operator { .. })
        && !principal.reads(&record.caller, &record.use_case)
    {
        return Err(Box::new(not_found()));
    }
    Ok((record, principal))
}

async fn intake_record(
    State(app): State<Arc<App>>,
    headers: HeaderMap,
    UrlPath(id): UrlPath<String>,
) -> Response {
    match owned_record(&app, &headers, &id) {
        Ok((record, _)) => (StatusCode::OK, Json(view(&record, false))).into_response(),
        Err(response) => *response,
    }
}

async fn disarmed(
    State(app): State<Arc<App>>,
    headers: HeaderMap,
    UrlPath(id): UrlPath<String>,
) -> Response {
    let (record, principal) = match owned_record(&app, &headers, &id) {
        Ok(found) => found,
        Err(response) => return *response,
    };
    if matches!(principal, Principal::Operator { .. }) || record.outcome != Outcome::Allowed {
        return error(
            StatusCode::NOT_FOUND,
            "not_found",
            "no rebuilt copy for this caller",
        );
    }
    match app.store.read_disarmed(&record, store::now()) {
        Ok(Some(bytes)) => {
            let disarmed = record.disarmed.as_ref().expect("read only with a record");
            let mut response = (StatusCode::OK, bytes).into_response();
            let headers = response.headers_mut();
            headers.insert(
                header::CONTENT_TYPE,
                HeaderValue::from_str(&disarmed.media_type)
                    .unwrap_or(HeaderValue::from_static("application/octet-stream")),
            );
            headers.insert(
                "x-guard-sha256",
                HeaderValue::from_str(&disarmed.sha256).expect("hex"),
            );
            headers.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
            response
        }
        Ok(None) => error(
            StatusCode::NOT_FOUND,
            "expired",
            "the rebuilt copy has expired; send the file again",
        ),
        Err(e) => {
            tracing::error!(target: "guard.store", error = %e, "rebuilt copy unreadable");
            error(
                StatusCode::INTERNAL_SERVER_ERROR,
                "store_failed",
                "the rebuilt copy could not be read",
            )
        }
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ExceptionRequest {
    sha256: String,
    use_case: String,
    approved_by: String,
    reason: String,
    expires_at: String,
}

async fn exception(State(app): State<Arc<App>>, headers: HeaderMap, body: Bytes) -> Response {
    let Some(Principal::Operator { id }) = app.principal(&headers) else {
        return error(
            StatusCode::FORBIDDEN,
            "operator_required",
            "only an operator records an exception",
        );
    };
    let Ok(request) = serde_json::from_slice::<ExceptionRequest>(&body) else {
        return error(
            StatusCode::UNPROCESSABLE_ENTITY,
            "exception_invalid",
            "the exception request is not valid",
        );
    };
    let at = store::now();
    let expires = store::parse(&request.expires_at);
    let valid = request.sha256.len() == 64
        && request
            .sha256
            .bytes()
            .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
        && app.policy.use_case(&request.use_case).is_some()
        && (1..=128).contains(&request.approved_by.trim().len())
        && (1..=500).contains(&request.reason.trim().len())
        && expires.is_some_and(|t| t > at && t <= at + time::Duration::days(90));
    if !valid {
        return error(
            StatusCode::UNPROCESSABLE_ENTITY,
            "exception_invalid",
            "name the SHA-256, a use case, who approved it, why, and an expiry within 90 days",
        );
    }
    let exception = Exception {
        sha256: request.sha256,
        use_case: request.use_case,
        approved_by: request.approved_by,
        reason: request.reason,
        expires_at: request.expires_at,
        recorded_at: store::stamp(at),
    };
    if let Err(e) = app.store.put_exception(&exception) {
        tracing::error!(target: "guard.store", error = %e, "exception not recorded");
        return store_error(&e, "the exception could not be recorded");
    }
    tracing::info!(target: "guard.exception", operator = %id, use_case = %exception.use_case, sha256 = %exception.sha256,
        expires_at = %exception.expires_at, "exception recorded");
    (StatusCode::CREATED, Json(json!({"recorded": exception}))).into_response()
}

async fn readyz(State(app): State<Arc<App>>) -> Response {
    let scanner = app.engines.scanner_version().await;
    let writable = app.store.writable();
    match (&scanner, writable) {
        (Ok(version), true) => (
            StatusCode::OK,
            Json(json!({
                "ready": true,
                "clamav": version.as_ref().map(|v| v.text.clone()),
                "yara_rules": app.engines.rules.version,
                "rule_count": app.engines.rules.count,
                "qpdf": app.engines.qpdf_version,
                "policy_version": app.policy.version,
                "store_used_bytes": app.store.used(),
            })),
        )
            .into_response(),
        _ => (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(json!({
                "ready": false,
                "scanner": scanner.err().map(|u| u.0),
                "store_writable": writable,
            })),
        )
            .into_response(),
    }
}

async fn metrics(State(app): State<Arc<App>>) -> Response {
    let mut text = app.metrics.render();
    text.push_str(&format!(
        "# TYPE guard_store_used_bytes gauge\nguard_store_used_bytes {}\n",
        app.store.used()
    ));
    text.push_str(&format!(
        "# TYPE guard_free_permits gauge\nguard_free_permits {}\n",
        app.permits.available_permits()
    ));
    ([(header::CONTENT_TYPE, "text/plain; version=0.0.4")], text).into_response()
}

pub fn router(app: Arc<App>) -> Router {
    Router::new()
        // The intake reads its own body, within its use case's limit, after authentication and admission.
        .route("/v1/intake", post(intake))
        .route("/v1/intake/{id}", get(intake_record))
        .route("/v1/intake/{id}/disarmed", get(disarmed))
        .route(
            "/v1/exceptions",
            post(exception).layer(DefaultBodyLimit::max(16 * 1024)),
        )
        .with_state(app)
}

pub fn ops_router(app: Arc<App>) -> Router {
    Router::new()
        .route("/healthz", get(|| async { "ok" }))
        .route("/readyz", get(readyz))
        .route("/metrics", get(metrics))
        .with_state(app)
}
