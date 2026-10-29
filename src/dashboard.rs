use std::{
    collections::BTreeMap, convert::Infallible, fmt::Write, net::SocketAddr, sync::Arc,
    sync::Mutex, sync::RwLock, sync::mpsc, time::Duration,
};

use anyhow::{Context, Result, bail};
use axum::{
    Json, Router,
    body::Body,
    extract::{Path, Query, State},
    http::{HeaderMap, HeaderValue, Request, StatusCode, header},
    middleware::{self, Next},
    response::{Html, IntoResponse, Response, Sse, sse::Event, sse::KeepAlive},
    routing::{get, post},
};
use futures_util::StreamExt;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use time::{OffsetDateTime, format_description::well_known::Rfc3339};
use tokio_stream::wrappers::IntervalStream;

use crate::{
    agent_continuity::{self, LogicalAgentLifecycle, LogicalAgentRecord},
    agent_state::{self, StateDomain, StateRunSummary, StateTraceSummary},
    allocation::{self, AllocationReservationSummary},
    audit::{self, AuditRecord},
    catalog::{self, CatalogEntry},
    config::{ControllerSettings, RuntimeConfig},
    evidence,
    lease::{self, LeaseState, ServiceLease},
    openshell, platform, policy, provenance,
    request::{self, NormalizedRequest, RequestDecision},
    research::{self, ResearchStudySummary},
    state::{self, SandboxRecord},
    team::{self, TeamRecord},
    workspace_sync::{self, DecisionDeliveryReceipt, SyncReport},
};

const INDEX_HTML: &str = include_str!("../web/index.html");
const APP_CSS: &str = include_str!("../web/app.css");
const APP_JS: &str = include_str!("../web/app.js");
const DASHBOARD_WORKER_COUNT: usize = 4;
const DASHBOARD_WORK_QUEUE: usize = 128;

type DashboardJob = Box<dyn FnOnce() + Send + 'static>;

#[derive(Clone)]
struct DashboardWorkers {
    sender: mpsc::SyncSender<DashboardJob>,
}

impl DashboardWorkers {
    fn new(worker_count: usize, queue_depth: usize) -> Result<Self> {
        if worker_count == 0 || queue_depth == 0 {
            bail!("dashboard worker count and queue depth must be nonzero");
        }
        let (sender, receiver) = mpsc::sync_channel::<DashboardJob>(queue_depth);
        let receiver = Arc::new(Mutex::new(receiver));
        for index in 0..worker_count {
            let receiver = receiver.clone();
            std::thread::Builder::new()
                .name(format!("agent-dashboard-{index}"))
                .spawn(move || {
                    loop {
                        let job = {
                            let Ok(receiver) = receiver.lock() else {
                                return;
                            };
                            receiver.recv()
                        };
                        match job {
                            Ok(job) => job(),
                            Err(_) => return,
                        }
                    }
                })
                .context("failed to start a dashboard worker")?;
        }
        Ok(Self { sender })
    }

    async fn run<T, F>(&self, task: F) -> Result<T, ApiError>
    where
        T: Send + 'static,
        F: FnOnce() -> Result<T> + Send + 'static,
    {
        let (sender, receiver) = tokio::sync::oneshot::channel();
        let job = Box::new(move || {
            let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(task))
                .map_err(|_| anyhow::anyhow!("dashboard worker task panicked"))
                .and_then(|result| result);
            let _ = sender.send(result);
        });
        match self.sender.try_send(job) {
            Ok(()) => {}
            Err(mpsc::TrySendError::Full(_)) => {
                return Err(ApiError::unavailable("dashboard worker queue is full"));
            }
            Err(mpsc::TrySendError::Disconnected(_)) => {
                return Err(ApiError::unavailable("dashboard workers are unavailable"));
            }
        }
        Ok(receiver
            .await
            .context("dashboard worker stopped before returning a result")??)
    }
}

#[derive(Clone)]
struct AppState {
    config: RuntimeConfig,
    bind: SocketAddr,
    workers: DashboardWorkers,
    cache: Arc<RwLock<DashboardCache>>,
    shutdown: tokio::sync::watch::Receiver<bool>,
}

#[derive(Debug)]
struct DashboardCache {
    value: Value,
    digest: String,
    revision: u64,
    observed_at_unix: u64,
    healthy: bool,
    last_error_at_unix: Option<u64>,
}

#[derive(Clone, Debug, Serialize)]
struct DashboardStreamStatus {
    schema_version: u32,
    revision: u64,
    observed_at_unix: u64,
    state_digest: String,
    healthy: bool,
    last_error_at_unix: Option<u64>,
}

#[derive(Debug, Serialize)]
struct DashboardState {
    schema_version: u32,
    generated_at_unix: u64,
    settings: ControllerSettings,
    sandboxes: Vec<SandboxRecord>,
    sandbox_status: BTreeMap<String, Value>,
    gateway_health: BTreeMap<String, Value>,
    runtime_proofs: BTreeMap<String, platform::RuntimeProof>,
    sync_reports: BTreeMap<String, SyncReport>,
    decision_receipts: Vec<DecisionDeliveryReceipt>,
    evidence_summaries: BTreeMap<String, evidence::EvidenceSummary>,
    audit_chain: Value,
    leases: Vec<ServiceLease>,
    requests: Vec<NormalizedRequest>,
    proposals: BTreeMap<String, String>,
    active_policies: BTreeMap<String, Value>,
    ocsf_events: Vec<Value>,
    teams: Vec<TeamRecord>,
    research: Vec<ResearchStudySummary>,
    logical_agents: Vec<LogicalAgentSummary>,
    allocation_reservations: Vec<AllocationReservationSummary>,
    agent_state: Vec<StateRunSummary>,
    agent_state_traces: BTreeMap<String, StateTraceSummary>,
    catalog: Vec<CatalogEntry>,
    audit: Vec<AuditRecord>,
}

#[derive(Debug, Serialize)]
struct LogicalAgentSummary {
    name: String,
    domain: StateDomain,
    state_profile: String,
    runtime_generation: u64,
    active_sandbox: Option<String>,
    sessions_started: u64,
    sessions_completed: u64,
    failed_sessions: u64,
    last_result_class: Option<String>,
    lifecycle: LogicalAgentLifecycle,
    updated_at_unix: u64,
}

impl From<&LogicalAgentRecord> for LogicalAgentSummary {
    fn from(record: &LogicalAgentRecord) -> Self {
        Self {
            name: record.name.clone(),
            domain: record.domain,
            state_profile: record.state_profile.clone(),
            runtime_generation: record.runtime_generation,
            active_sandbox: record.active_sandbox.clone(),
            sessions_started: record.sessions_started,
            sessions_completed: record.sessions_completed,
            failed_sessions: record.failed_sessions,
            last_result_class: record.last_result_class.clone(),
            lifecycle: record.lifecycle,
            updated_at_unix: record.updated_at_unix,
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RejectBody {
    reason: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct SettingsPatch {
    custom_auto_approve_loopback_services: bool,
    custom_auto_approve_catalog_entries: bool,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct EvidenceLeaseBody {
    purpose: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct EvidenceSearchBody {
    query: String,
    #[serde(default = "default_search_limit")]
    limit: usize,
    #[serde(default)]
    cursor: usize,
}

#[derive(Debug, Deserialize)]
struct GraphQuery {
    #[serde(default = "default_graph_limit")]
    limit: usize,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct DashboardSearchQuery {
    #[serde(default)]
    q: String,
    #[serde(default = "default_search_scope")]
    scope: String,
    #[serde(default = "default_search_limit")]
    limit: usize,
    #[serde(default)]
    cursor: usize,
}

#[derive(Clone, Debug, Serialize)]
struct DashboardSearchHit {
    id: String,
    target_id: Option<String>,
    scope: String,
    kind: String,
    title: String,
    subtitle: Option<String>,
    detail: Option<String>,
    occurred_at: Option<String>,
    run_id: Option<String>,
    view: String,
    status: Option<String>,
}

#[derive(Debug, Serialize)]
struct DashboardSearchPage {
    schema_version: u32,
    query: String,
    scope: String,
    total: u64,
    cursor: usize,
    next_cursor: Option<usize>,
    results: Vec<DashboardSearchHit>,
}

fn default_graph_limit() -> usize {
    5_000
}

fn default_search_scope() -> String {
    "all".into()
}

fn default_search_limit() -> usize {
    40
}

#[derive(Debug, Serialize)]
struct ApiResult<T: Serialize> {
    ok: bool,
    data: T,
}

pub async fn serve(config: RuntimeConfig, bind_override: Option<&str>) -> Result<()> {
    let configured = config.load_settings()?;
    let bind: SocketAddr = bind_override
        .unwrap_or(&configured.dashboard_bind)
        .parse()
        .context("dashboard bind must be an IP socket address")?;
    if !bind.ip().is_loopback() {
        bail!("the operator dashboard must remain on loopback");
    }
    let workers = DashboardWorkers::new(DASHBOARD_WORKER_COUNT, DASHBOARD_WORK_QUEUE)?;
    let initial_config = config.clone();
    let initial = workers
        .run(move || build_dashboard_state(&initial_config))
        .await
        .map_err(|error| anyhow::anyhow!(error.message))?;
    let cache = Arc::new(RwLock::new(new_dashboard_cache(initial)?));
    let (shutdown_sender, shutdown_receiver) = tokio::sync::watch::channel(false);
    let state = Arc::new(AppState {
        config,
        bind,
        workers,
        cache,
        shutdown: shutdown_receiver.clone(),
    });
    let app = Router::new()
        .route("/", get(index))
        .route("/app.css", get(css))
        .route("/app.js", get(js))
        .route("/api/v1/health", get(api_health))
        .route("/api/v1/state", get(api_state))
        .route("/api/v1/state/stream", get(api_state_stream))
        .route("/api/v1/search", get(api_search))
        .route("/api/v1/provenance-status", get(api_provenance_status))
        .route("/api/v1/provenance/{run}", get(api_provenance_graph))
        .route(
            "/api/v1/provenance/{run}/stream",
            get(api_provenance_stream),
        )
        .route("/api/v1/provenance/{run}/lease", post(api_provenance_lease))
        .route(
            "/api/v1/provenance/{run}/event/{event_id}/evidence",
            post(api_provenance_evidence),
        )
        .route(
            "/api/v1/provenance/{run}/evidence/search",
            post(api_provenance_evidence_search),
        )
        .route(
            "/api/v1/provenance/{run}/artifact/{sha256}/content",
            post(api_provenance_artifact),
        )
        .route("/metrics", get(metrics))
        .route("/api/v1/request-schema", get(api_request_schema))
        .route(
            "/api/v1/request/{sandbox}/{request_id}/approve",
            post(api_request_approve),
        )
        .route(
            "/api/v1/request/{sandbox}/{request_id}/reject",
            post(api_request_reject),
        )
        .route(
            "/api/v1/proposal/{sandbox}/{chunk_id}/approve",
            post(api_proposal_approve),
        )
        .route(
            "/api/v1/proposal/{sandbox}/{chunk_id}/reject",
            post(api_proposal_reject),
        )
        .route("/api/v1/lease/{lease_id}/close", post(api_lease_close))
        .route(
            "/api/v1/evidence/{sandbox}/capture",
            post(api_evidence_capture),
        )
        .route("/api/v1/settings", post(api_settings))
        .layer(middleware::from_fn(security_headers))
        .with_state(state.clone());
    let listener = tokio::net::TcpListener::bind(bind).await?;
    println!("Operator dashboard: http://{bind}/");
    let reconciler_config = state.config.clone();
    let reconciler = tokio::spawn(async move {
        auto_reconcile(reconciler_config).await;
    });
    let feed_state = state.clone();
    let feed = tokio::spawn(async move {
        refresh_dashboard_feed(feed_state).await;
    });
    let signal = tokio::spawn(async move {
        shutdown_signal().await;
        let _ = shutdown_sender.send(true);
    });
    let mut server_shutdown = shutdown_receiver;
    let result = axum::serve(listener, app)
        .with_graceful_shutdown(async move {
            let _ = server_shutdown.changed().await;
        })
        .await;
    reconciler.abort();
    feed.abort();
    signal.abort();
    result.map_err(Into::into)
}

async fn auto_reconcile(config: RuntimeConfig) {
    let mut interval = tokio::time::interval(std::time::Duration::from_secs(2));
    interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let mut ticks = 0_u64;
    loop {
        interval.tick().await;
        ticks += 1;
        let task_config = config.clone();
        let refresh_runtime = ticks % 5 == 1;
        let result = tokio::task::spawn_blocking(move || {
            let requests = request::process_auto(&task_config);
            if refresh_runtime {
                let _ = platform::refresh_all(&task_config);
            }
            requests
        })
        .await;
        let error = match result {
            Ok(Ok(_)) => None,
            Ok(Err(error)) => Some(error.to_string()),
            Err(error) => Some(error.to_string()),
        };
        if error.is_some() {
            let _ = audit::append(
                &config,
                "dashboard-reconciler",
                "request.process-auto",
                "controller/reconciler",
                "failed",
                json!({"error_type": "auto_reconcile_failed"}),
            );
        }
    }
}

async fn refresh_dashboard_feed(state: Arc<AppState>) {
    let mut interval = tokio::time::interval(Duration::from_secs(2));
    interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    loop {
        interval.tick().await;
        let config = state.config.clone();
        match state
            .workers
            .run(move || build_dashboard_state(&config))
            .await
        {
            Ok(snapshot) => {
                if update_dashboard_cache(&state.cache, snapshot).is_err() {
                    mark_dashboard_cache_unhealthy(&state.cache);
                    let _ = audit::append(
                        &state.config,
                        "dashboard-feed",
                        "dashboard.snapshot.update",
                        "controller/dashboard-cache",
                        "failed",
                        json!({"error_type": "snapshot_serialization_failed"}),
                    );
                }
            }
            Err(_) => mark_dashboard_cache_unhealthy(&state.cache),
        }
    }
}

fn new_dashboard_cache(snapshot: DashboardState) -> Result<DashboardCache> {
    let (value, digest, observed_at_unix) = dashboard_snapshot_value(snapshot)?;
    Ok(DashboardCache {
        value,
        digest,
        revision: 1,
        observed_at_unix,
        healthy: true,
        last_error_at_unix: None,
    })
}

fn update_dashboard_cache(
    cache: &Arc<RwLock<DashboardCache>>,
    snapshot: DashboardState,
) -> Result<()> {
    let (value, digest, observed_at_unix) = dashboard_snapshot_value(snapshot)?;
    let mut current = cache
        .write()
        .map_err(|_| anyhow::anyhow!("dashboard cache lock is unavailable"))?;
    if current.digest != digest {
        current.revision = current.revision.saturating_add(1);
    }
    current.value = value;
    current.digest = digest;
    current.observed_at_unix = observed_at_unix;
    current.healthy = true;
    current.last_error_at_unix = None;
    Ok(())
}

fn mark_dashboard_cache_unhealthy(cache: &Arc<RwLock<DashboardCache>>) {
    let observed = unix_now_u64().ok();
    if let Ok(mut current) = cache.write() {
        current.healthy = false;
        current.last_error_at_unix = observed;
    }
}

fn dashboard_snapshot_value(snapshot: DashboardState) -> Result<(Value, String, u64)> {
    let observed_at_unix = snapshot.generated_at_unix;
    let value = serde_json::to_value(snapshot)?;
    let mut digest_value = value.clone();
    normalize_dashboard_digest_value(&mut digest_value);
    let digest = hash_dashboard_value(&serde_json::to_string(&digest_value)?);
    Ok((value, digest, observed_at_unix))
}

fn normalize_dashboard_digest_value(value: &mut Value) {
    let Some(object) = value.as_object_mut() else {
        return;
    };
    object.insert("generated_at_unix".into(), json!(0));
    let Some(gateways) = object
        .get_mut("gateway_health")
        .and_then(Value::as_object_mut)
    else {
        return;
    };
    for gateway in gateways.values_mut().filter_map(Value::as_object_mut) {
        if gateway.contains_key("observed_at_unix") {
            gateway.insert("observed_at_unix".into(), json!(0));
        }
    }
}

fn dashboard_stream_status(cache: &DashboardCache) -> DashboardStreamStatus {
    DashboardStreamStatus {
        schema_version: 1,
        revision: cache.revision,
        observed_at_unix: cache.observed_at_unix,
        state_digest: cache.digest.clone(),
        healthy: cache.healthy,
        last_error_at_unix: cache.last_error_at_unix,
    }
}

fn unix_now_u64() -> Result<u64> {
    Ok(std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)?
        .as_secs())
}

async fn index() -> Html<&'static str> {
    Html(INDEX_HTML)
}
async fn css() -> impl IntoResponse {
    ([(header::CONTENT_TYPE, "text/css; charset=utf-8")], APP_CSS)
}
async fn js() -> impl IntoResponse {
    (
        [(header::CONTENT_TYPE, "text/javascript; charset=utf-8")],
        APP_JS,
    )
}

async fn api_health() -> Json<ApiResult<Value>> {
    Json(ApiResult {
        ok: true,
        data: json!({"schema_version": 1, "status": "ready"}),
    })
}

async fn api_state_stream(
    State(state): State<Arc<AppState>>,
) -> Sse<impl tokio_stream::Stream<Item = Result<Event, Infallible>>> {
    let mut interval = tokio::time::interval(Duration::from_secs(1));
    interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let mut shutdown = state.shutdown.clone();
    let stream = IntervalStream::new(interval)
        .map(move |_| {
            let status = state
                .cache
                .read()
                .map(|cache| dashboard_stream_status(&cache))
                .unwrap_or(DashboardStreamStatus {
                    schema_version: 1,
                    revision: 0,
                    observed_at_unix: 0,
                    state_digest: String::new(),
                    healthy: false,
                    last_error_at_unix: None,
                });
            Event::default()
                .event("state")
                .json_data(ApiResult {
                    ok: true,
                    data: status,
                })
                .unwrap_or_else(|_| Event::default().event("error").data("serialization"))
        })
        .take_until(async move {
            let _ = shutdown.changed().await;
        });
    Sse::new(stream.map(Ok)).keep_alive(
        KeepAlive::new()
            .interval(Duration::from_secs(15))
            .text("dashboard-state"),
    )
}

async fn api_search(
    State(state): State<Arc<AppState>>,
    Query(query): Query<DashboardSearchQuery>,
) -> Result<Json<ApiResult<DashboardSearchPage>>, ApiError> {
    let config = state.config.clone();
    let page = state
        .workers
        .run(move || dashboard_search(&config, &query))
        .await?;
    Ok(Json(ApiResult {
        ok: true,
        data: page,
    }))
}

async fn api_provenance_status(
    State(state): State<Arc<AppState>>,
) -> Result<Json<ApiResult<provenance::ProvenanceStorageStatus>>, ApiError> {
    let config = state.config.clone();
    let status = state
        .workers
        .run(move || provenance::status(&config))
        .await?;
    Ok(Json(ApiResult {
        ok: true,
        data: status,
    }))
}

async fn api_provenance_graph(
    State(state): State<Arc<AppState>>,
    Path(run): Path<String>,
    Query(query): Query<GraphQuery>,
) -> Result<Json<ApiResult<provenance::ProvenanceGraph>>, ApiError> {
    let config = state.config.clone();
    let graph = state
        .workers
        .run(move || provenance::graph(&config, &run, query.limit))
        .await?;
    Ok(Json(ApiResult {
        ok: true,
        data: graph,
    }))
}

async fn api_provenance_stream(
    State(state): State<Arc<AppState>>,
    Path(run): Path<String>,
    Query(query): Query<GraphQuery>,
) -> Sse<impl tokio_stream::Stream<Item = Result<Event, Infallible>>> {
    let mut interval = tokio::time::interval(Duration::from_secs(2));
    interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let mut shutdown = state.shutdown.clone();
    let stream = IntervalStream::new(interval)
        .then(move |_| {
            let config = state.config.clone();
            let workers = state.workers.clone();
            let run = run.clone();
            async move {
                match workers
                    .run(move || provenance::graph(&config, &run, query.limit))
                    .await
                {
                    Ok(graph) => Event::default()
                        .event("graph")
                        .json_data(ApiResult {
                            ok: true,
                            data: graph,
                        })
                        .unwrap_or_else(|_| Event::default().event("error").data("serialization")),
                    Err(error) => Event::default()
                        .event("error")
                        .json_data(json!({"ok": false, "error": error.message}))
                        .unwrap_or_else(|_| Event::default().event("error").data("unavailable")),
                }
            }
        })
        .take_until(async move {
            let _ = shutdown.changed().await;
        });
    Sse::new(stream.map(Ok)).keep_alive(
        KeepAlive::new()
            .interval(Duration::from_secs(15))
            .text("provenance"),
    )
}

async fn api_provenance_lease(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(run): Path<String>,
    Json(body): Json<EvidenceLeaseBody>,
) -> Result<Json<ApiResult<provenance::EvidenceLeaseReceipt>>, ApiError> {
    authorize_mutation(&headers, state.bind, "open-evidence")?;
    let config = state.config.clone();
    let receipt = state
        .workers
        .run(move || {
            let receipt =
                provenance::issue_evidence_lease(&config, &run, "dashboard-local", &body.purpose)?;
            audit::append(
                &config,
                "dashboard-local",
                "provenance.evidence-lease.issue",
                &format!("agent-state/{run}/provenance"),
                "passed",
                json!({
                    "lease_id": receipt.lease_id,
                    "expires_at": receipt.expires_at,
                    "purpose_sha256": hash_dashboard_value(&receipt.purpose)
                }),
            )?;
            Ok(receipt)
        })
        .await?;
    Ok(Json(ApiResult {
        ok: true,
        data: receipt,
    }))
}

async fn api_provenance_evidence(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path((run, event_id)): Path<(String, String)>,
) -> Result<Json<ApiResult<provenance::EvidenceEventDetail>>, ApiError> {
    authorize_mutation(&headers, state.bind, "read-evidence")?;
    let token = evidence_token(&headers)?.to_owned();
    let config = state.config.clone();
    let detail = state
        .workers
        .run(move || {
            let detail = provenance::event_evidence(&config, &run, &event_id, &token)?;
            audit::append(
                &config,
                "dashboard-local",
                "provenance.evidence.read",
                &format!("agent-state/{run}/provenance/{event_id}"),
                "passed",
                json!({
                    "source": detail.source,
                    "kind": detail.kind,
                    "content_sha256": detail.content_sha256,
                    "content_truncated": detail.content_truncated
                }),
            )?;
            Ok(detail)
        })
        .await?;
    Ok(Json(ApiResult {
        ok: true,
        data: detail,
    }))
}

async fn api_provenance_evidence_search(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(run): Path<String>,
    Json(body): Json<EvidenceSearchBody>,
) -> Result<Json<ApiResult<provenance::EvidenceSearchPage>>, ApiError> {
    authorize_mutation(&headers, state.bind, "search-evidence")?;
    let token = evidence_token(&headers)?.to_owned();
    let query_digest = hash_dashboard_value(body.query.trim());
    let config = state.config.clone();
    let run_for_audit = run.clone();
    let page = state
        .workers
        .run(move || {
            let page = provenance::search_evidence(
                &config,
                &run,
                &token,
                &body.query,
                body.limit,
                body.cursor,
            )?;
            audit::append(
                &config,
                "dashboard-local",
                "provenance.evidence.search",
                &format!("agent-state/{run_for_audit}/provenance"),
                "passed",
                json!({
                    "query_sha256": query_digest,
                    "cursor": page.cursor,
                    "returned": page.hits.len(),
                    "total": page.total
                }),
            )?;
            Ok(page)
        })
        .await?;
    Ok(Json(ApiResult {
        ok: true,
        data: page,
    }))
}

async fn api_provenance_artifact(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path((run, sha256)): Path<(String, String)>,
) -> Result<Response, ApiError> {
    authorize_mutation(&headers, state.bind, "read-evidence")?;
    let token = evidence_token(&headers)?.to_owned();
    let config = state.config.clone();
    let requested_sha256 = sha256.clone();
    let (artifact, bytes) = state
        .workers
        .run(move || {
            provenance::validate_evidence_lease(&config, &run, &token)?;
            let result = agent_state::read_artifact_bounded(
                &config,
                &run,
                &requested_sha256,
                256 * 1024 * 1024,
            )?;
            audit::append(
                &config,
                "dashboard-local",
                "provenance.artifact.read",
                &format!("agent-state/{run}/artifact/{requested_sha256}"),
                "passed",
                json!({"sha256": result.0.sha256, "bytes": result.0.size, "role": result.0.role}),
            )?;
            Ok(result)
        })
        .await?;
    let mut response = Response::new(Body::from(bytes));
    *response.status_mut() = StatusCode::OK;
    response.headers_mut().insert(
        header::CONTENT_TYPE,
        HeaderValue::from_str(&artifact.media_type)
            .unwrap_or_else(|_| HeaderValue::from_static("application/octet-stream")),
    );
    response.headers_mut().insert(
        header::CONTENT_DISPOSITION,
        HeaderValue::from_str(&format!("attachment; filename=artifact-{sha256}.bin"))
            .map_err(|_| anyhow::anyhow!("invalid artifact response header"))?,
    );
    response.headers_mut().insert(
        "x-content-sha256",
        HeaderValue::from_str(&artifact.sha256)
            .map_err(|_| anyhow::anyhow!("invalid artifact digest header"))?,
    );
    Ok(response)
}

async fn metrics(State(state): State<Arc<AppState>>) -> Result<impl IntoResponse, ApiError> {
    let config = state.config.clone();
    let body = tokio::task::spawn_blocking(move || prometheus_metrics(&config))
        .await
        .context("metrics collection task failed")??;
    Ok((
        [(
            header::CONTENT_TYPE,
            "text/plain; version=0.0.4; charset=utf-8",
        )],
        body,
    ))
}

fn prometheus_metrics(config: &RuntimeConfig) -> Result<String> {
    let settings = config.load_settings()?;
    let sandboxes = state::list(config)?;
    let leases = lease::list(config)?;
    let requests = request::list_pending(config, None)?;
    let catalog = catalog::list(config)?;
    let teams = team::list(config)?;
    let research = research::summaries(config)?;
    let logical_agents = agent_continuity::list(config)?;
    let allocation_reservations = allocation::summaries(config)?;
    let agent_state = agent_state::list_summaries(config)?;
    let receipts = workspace_sync::load_decision_receipts(config)?;
    let evidence = evidence::latest_summaries(config)?;
    let audit_records = audit::verify(config);
    let active_leases = leases
        .iter()
        .filter(|lease| lease.state == LeaseState::Active)
        .count();
    let team_workers = teams.iter().map(|team| team.workers.len()).sum::<usize>();
    let research_workers = research
        .iter()
        .map(|study| study.workers.len())
        .sum::<usize>();
    let research_segments = research
        .iter()
        .flat_map(|study| study.workers.iter())
        .map(|worker| worker.segments_completed as usize)
        .sum::<usize>();
    let active_logical_agents = logical_agents
        .iter()
        .filter(|agent| agent.lifecycle == LogicalAgentLifecycle::Active)
        .count();
    let unresolved_allocations = allocation_reservations
        .iter()
        .filter(|reservation| {
            matches!(
                reservation.status.as_str(),
                "unowned-live" | "custody-unknown"
            )
        })
        .count();
    let state_events = agent_state.iter().map(|run| run.events).sum::<u64>();
    let state_artifacts = agent_state.iter().map(|run| run.artifacts).sum::<u64>();
    let verified_receipts = receipts.iter().filter(|receipt| receipt.verified).count();
    let unverified_receipts = receipts.len().saturating_sub(verified_receipts);
    let decision_integrity_snapshots = evidence
        .values()
        .filter(|summary| summary.decision_integrity_verified)
        .count();
    let enabled_gateways = settings
        .gateway_profiles
        .values()
        .filter(|profile| profile.enabled)
        .count();
    let (audit_chain_valid, audit_record_count) = match audit_records {
        Ok(records) => (1_u8, records.len()),
        Err(_) => (0_u8, 0),
    };
    let values = [
        ("managed_sandboxes", sandboxes.len()),
        ("pending_requests", requests.len()),
        ("active_service_leases", active_leases),
        ("team_workers", team_workers),
        ("research_studies", research.len()),
        ("research_workers", research_workers),
        ("research_context_segments", research_segments),
        ("logical_agents", logical_agents.len()),
        ("logical_agents_active", active_logical_agents),
        ("allocation_reservations", allocation_reservations.len()),
        ("allocation_custody_unresolved", unresolved_allocations),
        ("state_runs", agent_state.len()),
        ("catalog_entries", catalog.len()),
        ("gateway_profiles_enabled", enabled_gateways),
        ("latest_evidence_snapshots", evidence.len()),
        ("decision_integrity_snapshots", decision_integrity_snapshots),
        ("audit_records", audit_record_count),
    ];
    let mut body = String::new();
    for (name, value) in values {
        writeln!(
            &mut body,
            "# TYPE enrichedlab_agent_sandbox_{name} gauge\nenrichedlab_agent_sandbox_{name} {value}"
        )?;
    }
    writeln!(
        &mut body,
        "# TYPE enrichedlab_agent_sandbox_state_events gauge\nenrichedlab_agent_sandbox_state_events {state_events}"
    )?;
    writeln!(
        &mut body,
        "# TYPE enrichedlab_agent_sandbox_state_artifact_links gauge\nenrichedlab_agent_sandbox_state_artifact_links {state_artifacts}"
    )?;
    writeln!(
        &mut body,
        "# TYPE enrichedlab_agent_sandbox_audit_chain_valid gauge\nenrichedlab_agent_sandbox_audit_chain_valid {audit_chain_valid}"
    )?;
    writeln!(
        &mut body,
        "# TYPE enrichedlab_agent_sandbox_decision_receipts gauge\nenrichedlab_agent_sandbox_decision_receipts{{verified=\"true\"}} {verified_receipts}\nenrichedlab_agent_sandbox_decision_receipts{{verified=\"false\"}} {unverified_receipts}"
    )?;
    Ok(body)
}

async fn api_state(State(state): State<Arc<AppState>>) -> Result<Json<ApiResult<Value>>, ApiError> {
    let cache = state
        .cache
        .read()
        .map_err(|_| ApiError::unavailable("dashboard state cache is unavailable"))?;
    let mut value = cache.value.clone();
    if let Some(object) = value.as_object_mut() {
        object.insert("live_revision".into(), json!(cache.revision));
        object.insert("state_digest".into(), json!(cache.digest));
        object.insert("stream_healthy".into(), json!(cache.healthy));
        object.insert(
            "last_stream_error_at_unix".into(),
            json!(cache.last_error_at_unix),
        );
    }
    Ok(Json(ApiResult {
        ok: true,
        data: value,
    }))
}

fn build_dashboard_state(config: &RuntimeConfig) -> Result<DashboardState> {
    let sandboxes = state::list(config)?;
    let settings = config.load_settings()?;
    let mut proposals = BTreeMap::new();
    let mut active_policies = BTreeMap::new();
    let mut ocsf_events = Vec::new();
    for sandbox in &sandboxes {
        let pending = openshell::proposal_get(&config.openshell_bin, sandbox, Some("pending"))
            .unwrap_or_else(|error| format!("unavailable: {error}"));
        proposals.insert(sandbox.name.clone(), pending);
        let active = openshell::active_policy(&config.openshell_bin, sandbox)
            .and_then(|body| {
                serde_yaml_ng::from_str::<serde_yaml_ng::Value>(&body).map_err(Into::into)
            })
            .and_then(|value| serde_json::to_value(value).map_err(Into::into))
            .unwrap_or_else(|error| json!({"error": error.to_string()}));
        active_policies.insert(sandbox.name.clone(), active);
        match evidence::ocsf_tail(config, &sandbox.name, 25) {
            Ok(mut events) => ocsf_events.append(&mut events),
            Err(error) => ocsf_events.push(json!({
                "controller_sandbox": sandbox.name,
                "error": error.to_string()
            })),
        }
    }
    let mut sandbox_status = BTreeMap::new();
    for sandbox in &sandboxes {
        sandbox_status
            .entry(sandbox.runtime.openshell_gateway.clone())
            .or_insert_with(|| {
                openshell::live_sandboxes(&config.openshell_bin, &sandbox.runtime.openshell_gateway)
                    .unwrap_or_else(|error| json!({"error": error.to_string()}))
            });
    }
    let mut gateway_health = BTreeMap::new();
    for (name, profile) in &settings.gateway_profiles {
        let value = if profile.enabled {
            platform::observe_gateway(config, name, profile)
                .and_then(|health| serde_json::to_value(health).map_err(Into::into))
                .unwrap_or_else(|error| {
                    json!({
                        "profile": name,
                        "gateway": profile.openshell_gateway,
                        "expected_driver": profile.driver,
                        "passed": false,
                        "error": error.to_string()
                    })
                })
        } else {
            json!({"profile": name, "passed": false, "disabled": true})
        };
        gateway_health.insert(name.clone(), value);
    }
    let audit_chain = audit::verify(config)
        .map(|records| json!({"valid": true, "records": records.len()}))
        .unwrap_or_else(|error| json!({"valid": false, "error": error.to_string()}));
    let agent_state = agent_state::list_summaries(config)?;
    let mut agent_state_traces = BTreeMap::new();
    for run in &agent_state {
        agent_state_traces.insert(
            run.run_id.clone(),
            agent_state::trace_summary(config, &run.run_id, 100, 100)?,
        );
    }
    let logical_agents = agent_continuity::list(config)?;
    let allocation_reservations = allocation::summaries(config)?;
    Ok(DashboardState {
        schema_version: 8,
        generated_at_unix: unix_now_u64()?,
        settings,
        sandbox_status,
        gateway_health,
        runtime_proofs: platform::load_proofs(config)?,
        sync_reports: workspace_sync::load_latest_reports(config)?,
        decision_receipts: workspace_sync::load_decision_receipts(config)?,
        evidence_summaries: evidence::latest_summaries(config)?,
        audit_chain,
        leases: lease::list(config)?,
        requests: request::list_pending(config, None)?,
        teams: team::list(config)?,
        research: research::summaries(config)?,
        logical_agents: logical_agents.iter().map(Into::into).collect(),
        allocation_reservations,
        agent_state,
        agent_state_traces,
        catalog: catalog::list(config)?,
        audit: audit::tail(config, 100)?,
        proposals,
        active_policies,
        ocsf_events,
        sandboxes,
    })
}

fn dashboard_search(
    config: &RuntimeConfig,
    query: &DashboardSearchQuery,
) -> Result<DashboardSearchPage> {
    const SCOPES: &[&str] = &[
        "all",
        "provenance",
        "state",
        "artifacts",
        "agents",
        "requests",
        "audit",
        "research",
        "teams",
        "services",
        "catalog",
    ];
    let term = query.q.trim();
    if term.len() > 128
        || term
            .chars()
            .any(|character| character == '\0' || character == '\r' || character.is_control())
    {
        bail!("dashboard search query is invalid");
    }
    if !SCOPES.contains(&query.scope.as_str()) {
        bail!("dashboard search scope is invalid");
    }
    if !(1..=100).contains(&query.limit) {
        bail!("dashboard search limit must be between one and 100");
    }
    let end = query
        .cursor
        .checked_add(query.limit)
        .context("dashboard search cursor overflow")?;
    if end > 10_000 {
        bail!("dashboard search window exceeds 10000 records");
    }

    let mut total = 0_u64;
    let mut results = Vec::new();

    if search_scope(&query.scope, "provenance") {
        let page = provenance::search_metadata(config, term, end)?;
        total = total.saturating_add(page.total);
        results.extend(page.hits.into_iter().map(|hit| DashboardSearchHit {
            id: hit.id.clone(),
            target_id: Some(hit.id),
            scope: "provenance".into(),
            kind: hit.kind,
            title: hit.subject,
            subtitle: Some(format!("{} · {}", hit.evidence_role, hit.actor)),
            detail: search_detail(&hit.summary),
            occurred_at: Some(hit.occurred_at),
            run_id: Some(hit.run_id),
            view: "provenance".into(),
            status: Some(hit.harness),
        }));
    }

    if search_scope(&query.scope, "state") || search_scope(&query.scope, "artifacts") {
        let category = match query.scope.as_str() {
            "state" => Some("state"),
            "artifacts" => Some("artifact"),
            _ => None,
        };
        let page = agent_state::search_metadata(config, term, category, end)?;
        let state_total = page
            .totals
            .iter()
            .filter(|(category, _)| query.scope != "artifacts" || category.as_str() == "artifact")
            .filter(|(category, _)| query.scope != "state" || category.as_str() != "artifact")
            .map(|(_, count)| *count)
            .sum::<u64>();
        total = total.saturating_add(state_total);
        results.extend(page.hits.into_iter().filter_map(|hit| {
            let scope = if hit.category == "artifact" {
                "artifacts"
            } else {
                "state"
            };
            if !search_scope(&query.scope, scope) {
                return None;
            }
            let target_id =
                matches!(hit.category.as_str(), "event" | "artifact").then_some(hit.target_id);
            Some(DashboardSearchHit {
                id: hit.id,
                target_id,
                scope: scope.into(),
                kind: hit.category,
                title: hit.title,
                subtitle: Some(hit.detail),
                detail: hit.digest,
                occurred_at: Some(hit.occurred_at),
                run_id: Some(hit.run_id),
                view: "provenance".into(),
                status: None,
            })
        }));
    }

    if search_scope(&query.scope, "agents") {
        let mut agents = agent_continuity::list(config)?;
        agents.sort_by_key(|agent| std::cmp::Reverse(agent.updated_at_unix));
        for agent in agents {
            let lifecycle = json_label(&agent.lifecycle);
            let domain = json_label(&agent.domain);
            let result = agent.last_result_class.clone().unwrap_or_default();
            if !dashboard_search_match(
                term,
                &[
                    &agent.name,
                    &domain,
                    &agent.state_profile,
                    &lifecycle,
                    &result,
                ],
            ) {
                continue;
            }
            total = total.saturating_add(1);
            results.push(DashboardSearchHit {
                id: format!("agent:{}", agent.name),
                target_id: None,
                scope: "agents".into(),
                kind: "logical-agent".into(),
                title: agent.name,
                subtitle: Some(format!("{domain} · {}", agent.state_profile)),
                detail: agent
                    .last_result_class
                    .map(|value| format!("last result {value}")),
                occurred_at: unix_rfc3339(agent.updated_at_unix),
                run_id: Some(agent.state_run_id),
                view: "runtime".into(),
                status: Some(lifecycle),
            });
        }
    }

    if search_scope(&query.scope, "requests") {
        for item in request::list_pending(config, None)?.into_iter().rev() {
            let action = serde_json::to_value(&item.request.action)?;
            let kind = action
                .get("kind")
                .and_then(Value::as_str)
                .unwrap_or("request")
                .to_owned();
            if !dashboard_search_match(
                term,
                &[
                    &item.sandbox,
                    &item.request.request_id,
                    &kind,
                    &item.request.reason,
                ],
            ) {
                continue;
            }
            total = total.saturating_add(1);
            results.push(DashboardSearchHit {
                id: format!("request:{}:{}", item.sandbox, item.request.request_id),
                target_id: None,
                scope: "requests".into(),
                kind,
                title: item.request.reason,
                subtitle: Some(item.sandbox),
                detail: None,
                occurred_at: Some(item.observed_at),
                run_id: None,
                view: "access".into(),
                status: Some("pending".into()),
            });
        }
    }

    if search_scope(&query.scope, "audit") {
        let records = audit::verify(config)?;
        for record in records.into_iter().rev() {
            if !dashboard_search_match(
                term,
                &[
                    &record.actor,
                    &record.action,
                    &record.resource,
                    &record.outcome,
                ],
            ) {
                continue;
            }
            total = total.saturating_add(1);
            if results.iter().filter(|hit| hit.scope == "audit").count() >= end {
                continue;
            }
            results.push(DashboardSearchHit {
                id: format!("audit:{}", record.sequence),
                target_id: None,
                scope: "audit".into(),
                kind: record.action.clone(),
                title: record.action,
                subtitle: Some(record.actor),
                detail: Some(record.resource),
                occurred_at: Some(record.observed_at),
                run_id: None,
                view: "audit".into(),
                status: Some(record.outcome),
            });
        }
    }

    if search_scope(&query.scope, "research") {
        for study in research::summaries(config)? {
            let mode = json_label(&study.mode);
            let worker_names = study
                .workers
                .iter()
                .map(|worker| worker.name.as_str())
                .collect::<Vec<_>>()
                .join(" ");
            if !dashboard_search_match(term, &[&study.name, &mode, &worker_names]) {
                continue;
            }
            total = total.saturating_add(1);
            results.push(DashboardSearchHit {
                id: format!("research:{}", study.name),
                target_id: None,
                scope: "research".into(),
                kind: "study".into(),
                title: study.name,
                subtitle: Some(format!("{mode} · {} workers", study.workers.len())),
                detail: None,
                occurred_at: None,
                run_id: None,
                view: "research".into(),
                status: study
                    .comparison
                    .map(|comparison| comparison.scientific_validation),
            });
        }
    }

    if search_scope(&query.scope, "teams") {
        for team in team::list(config)? {
            let worker_names = team.workers.keys().cloned().collect::<Vec<_>>().join(" ");
            let branches = team
                .workers
                .values()
                .map(|worker| worker.branch.as_str())
                .collect::<Vec<_>>()
                .join(" ");
            if !dashboard_search_match(term, &[&team.name, &worker_names, &branches]) {
                continue;
            }
            total = total.saturating_add(1);
            results.push(DashboardSearchHit {
                id: format!("team:{}", team.name),
                target_id: None,
                scope: "teams".into(),
                kind: "team".into(),
                title: team.name,
                subtitle: Some(format!("{} workers", team.workers.len())),
                detail: Some(format!("base {}", short_digest(&team.base_commit))),
                occurred_at: Some(team.created_at),
                run_id: None,
                view: "research".into(),
                status: None,
            });
        }
    }

    if search_scope(&query.scope, "services") {
        for sandbox in state::list(config)? {
            let driver = json_label(&sandbox.runtime.driver);
            if dashboard_search_match(
                term,
                &[
                    &sandbox.name,
                    &sandbox.route,
                    &sandbox.runtime.gateway_profile,
                    &driver,
                ],
            ) {
                total = total.saturating_add(1);
                results.push(DashboardSearchHit {
                    id: format!("sandbox:{}", sandbox.name),
                    target_id: None,
                    scope: "services".into(),
                    kind: "sandbox".into(),
                    title: sandbox.name,
                    subtitle: Some(format!("{} · {driver}", sandbox.route)),
                    detail: Some(sandbox.runtime.gateway_profile),
                    occurred_at: unix_rfc3339(sandbox.created_at_unix),
                    run_id: None,
                    view: "runtime".into(),
                    status: Some("managed".into()),
                });
            }
        }
        for lease in lease::list(config)? {
            let kind = json_label(&lease.kind);
            let status = json_label(&lease.state);
            if !dashboard_search_match(
                term,
                &[&lease.id, &lease.label, &lease.sandbox, &kind, &status],
            ) {
                continue;
            }
            total = total.saturating_add(1);
            results.push(DashboardSearchHit {
                id: format!("lease:{}", lease.id),
                target_id: None,
                scope: "services".into(),
                kind,
                title: lease.label,
                subtitle: Some(lease.sandbox),
                detail: Some(format!("target port {}", lease.target_port)),
                occurred_at: unix_rfc3339(lease.created_at_unix),
                run_id: None,
                view: "runtime".into(),
                status: Some(status),
            });
        }
    }

    if search_scope(&query.scope, "catalog") {
        for entry in catalog::list(config)? {
            let entrypoint = entry.entrypoint.clone().unwrap_or_default();
            if !dashboard_search_match(
                term,
                &[&entry.name, &entry.kind, &entrypoint, &entry.sha256],
            ) {
                continue;
            }
            total = total.saturating_add(1);
            results.push(DashboardSearchHit {
                id: format!("catalog:{}", entry.name),
                target_id: None,
                scope: "catalog".into(),
                kind: entry.kind,
                title: entry.name,
                subtitle: entry.entrypoint,
                detail: Some(format!(
                    "{} files · {}",
                    entry.file_count,
                    short_digest(&entry.sha256)
                )),
                occurred_at: Some(entry.registered_at),
                run_id: None,
                view: "research".into(),
                status: Some("verified".into()),
            });
        }
    }

    results.sort_by(|left, right| {
        right
            .occurred_at
            .as_deref()
            .unwrap_or("")
            .cmp(left.occurred_at.as_deref().unwrap_or(""))
            .then_with(|| left.id.cmp(&right.id))
    });
    let page = results
        .into_iter()
        .skip(query.cursor)
        .take(query.limit)
        .collect::<Vec<_>>();
    let next_cursor =
        (query.cursor + page.len() < total as usize).then_some(query.cursor + page.len());
    Ok(DashboardSearchPage {
        schema_version: 1,
        query: term.into(),
        scope: query.scope.clone(),
        total,
        cursor: query.cursor,
        next_cursor,
        results: page,
    })
}

fn search_scope(requested: &str, candidate: &str) -> bool {
    requested == "all" || requested == candidate
}

fn dashboard_search_match(term: &str, values: &[&str]) -> bool {
    if term.is_empty() {
        return true;
    }
    let normalized = term.to_lowercase();
    values
        .iter()
        .any(|value| value.to_lowercase().contains(&normalized))
}

fn search_detail(value: &Value) -> Option<String> {
    if value.as_object().is_some_and(|object| object.is_empty()) {
        return None;
    }
    serde_json::to_string(value).ok()
}

fn json_label<T: Serialize>(value: &T) -> String {
    serde_json::to_value(value)
        .ok()
        .and_then(|value| value.as_str().map(str::to_owned))
        .unwrap_or_else(|| "unknown".into())
}

fn unix_rfc3339(value: u64) -> Option<String> {
    OffsetDateTime::from_unix_timestamp(i64::try_from(value).ok()?)
        .ok()?
        .format(&Rfc3339)
        .ok()
}

fn short_digest(value: &str) -> String {
    value.chars().take(12).collect()
}

async fn api_request_schema() -> Json<ApiResult<Value>> {
    Json(ApiResult {
        ok: true,
        data: request::schema(),
    })
}

async fn api_request_approve(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path((sandbox, request_id)): Path<(String, String)>,
) -> Result<Json<ApiResult<RequestDecision>>, ApiError> {
    authorize_mutation(&headers, state.bind, "approve-request")?;
    let decision = request::approve(
        &state.config,
        &sandbox,
        &request_id,
        "dashboard-local",
        None,
    )?;
    Ok(Json(ApiResult {
        ok: true,
        data: decision,
    }))
}

async fn api_request_reject(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path((sandbox, request_id)): Path<(String, String)>,
    Json(body): Json<RejectBody>,
) -> Result<Json<ApiResult<RequestDecision>>, ApiError> {
    authorize_mutation(&headers, state.bind, "reject-request")?;
    let decision = request::reject(
        &state.config,
        &sandbox,
        &request_id,
        "dashboard-local",
        &body.reason,
    )?;
    Ok(Json(ApiResult {
        ok: true,
        data: decision,
    }))
}

async fn api_proposal_approve(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path((sandbox, chunk_id)): Path<(String, String)>,
) -> Result<Json<ApiResult<Value>>, ApiError> {
    authorize_mutation(&headers, state.bind, "approve-proposal")?;
    let record = state::load(&state.config, &sandbox)?;
    let output =
        openshell::proposal_decide(&state.config.openshell_bin, &record, &chunk_id, true, None)?;
    audit::append(
        &state.config,
        "dashboard-local",
        "proposal.approve",
        &format!("sandbox/{sandbox}/proposal/{chunk_id}"),
        "passed",
        json!({}),
    )?;
    Ok(Json(ApiResult {
        ok: true,
        data: json!({"output": output}),
    }))
}

async fn api_proposal_reject(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path((sandbox, chunk_id)): Path<(String, String)>,
    Json(body): Json<RejectBody>,
) -> Result<Json<ApiResult<Value>>, ApiError> {
    authorize_mutation(&headers, state.bind, "reject-proposal")?;
    let record = state::load(&state.config, &sandbox)?;
    let output = openshell::proposal_decide(
        &state.config.openshell_bin,
        &record,
        &chunk_id,
        false,
        Some(&body.reason),
    )?;
    policy::sync_active(&state.config, &record)
        .context("proposal was approved live, but its active policy could not be persisted")?;
    audit::append(
        &state.config,
        "dashboard-local",
        "proposal.reject",
        &format!("sandbox/{sandbox}/proposal/{chunk_id}"),
        "passed",
        json!({"reason": body.reason}),
    )?;
    Ok(Json(ApiResult {
        ok: true,
        data: json!({"output": output}),
    }))
}

async fn api_lease_close(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(lease_id): Path<String>,
) -> Result<Json<ApiResult<ServiceLease>>, ApiError> {
    authorize_mutation(&headers, state.bind, "close-lease")?;
    let closed = lease::close(&state.config, &lease_id, "closed from dashboard")?;
    Ok(Json(ApiResult {
        ok: true,
        data: closed,
    }))
}

async fn api_evidence_capture(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(sandbox): Path<String>,
) -> Result<Json<ApiResult<evidence::EvidenceManifest>>, ApiError> {
    authorize_mutation(&headers, state.bind, "capture-evidence")?;
    state::validate_name(&sandbox)?;
    let config = state.config.clone();
    let manifest = tokio::task::spawn_blocking(move || evidence::snapshot(&config, &sandbox))
        .await
        .context("evidence capture task failed")??;
    Ok(Json(ApiResult {
        ok: true,
        data: manifest,
    }))
}

async fn api_settings(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Json(patch): Json<SettingsPatch>,
) -> Result<Json<ApiResult<ControllerSettings>>, ApiError> {
    authorize_mutation(&headers, state.bind, "update-settings")?;
    let mut settings = state.config.load_settings()?;
    settings.custom_auto_approve_loopback_services = patch.custom_auto_approve_loopback_services;
    settings.custom_auto_approve_catalog_entries = patch.custom_auto_approve_catalog_entries;
    state.config.save_settings(&settings)?;
    audit::append(
        &state.config,
        "dashboard-local",
        "settings.update",
        "controller/settings",
        "passed",
        json!({"custom_auto_approve_loopback_services": settings.custom_auto_approve_loopback_services, "custom_auto_approve_catalog_entries": settings.custom_auto_approve_catalog_entries}),
    )?;
    Ok(Json(ApiResult {
        ok: true,
        data: settings,
    }))
}

fn authorize_mutation(headers: &HeaderMap, bind: SocketAddr, intent: &str) -> Result<(), ApiError> {
    let host = headers
        .get(header::HOST)
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default();
    let origin = headers
        .get(header::ORIGIN)
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default();
    let fetch_site = headers
        .get("sec-fetch-site")
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default();
    let supplied_intent = headers
        .get("x-agent-sandbox-intent")
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default();
    let allowed_hosts = [
        bind.to_string(),
        format!("localhost:{}", bind.port()),
        format!("127.0.0.1:{}", bind.port()),
    ];
    if !allowed_hosts.iter().any(|allowed| host == allowed)
        || origin != format!("http://{host}")
        || fetch_site != "same-origin"
        || supplied_intent != intent
    {
        return Err(ApiError::forbidden(
            "mutation rejected by loopback same-origin policy",
        ));
    }
    Ok(())
}

fn evidence_token(headers: &HeaderMap) -> Result<&str, ApiError> {
    headers
        .get("x-agent-sandbox-evidence-lease")
        .and_then(|value| value.to_str().ok())
        .filter(|value| !value.is_empty())
        .ok_or_else(|| ApiError::forbidden("a scoped evidence lease is required"))
}

fn hash_dashboard_value(value: &str) -> String {
    use sha2::{Digest, Sha256};
    hex::encode(Sha256::digest(value.as_bytes()))
}

async fn security_headers(request: Request<Body>, next: Next) -> Response {
    let mut response = next.run(request).await;
    let headers = response.headers_mut();
    headers.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    headers.insert(header::CONTENT_SECURITY_POLICY, HeaderValue::from_static("default-src 'self'; script-src 'self'; style-src 'self'; connect-src 'self'; img-src 'self'; object-src 'none'; frame-ancestors 'none'; base-uri 'none'; form-action 'self'"));
    headers.insert(
        header::X_CONTENT_TYPE_OPTIONS,
        HeaderValue::from_static("nosniff"),
    );
    headers.insert(header::X_FRAME_OPTIONS, HeaderValue::from_static("DENY"));
    headers.insert(
        header::REFERRER_POLICY,
        HeaderValue::from_static("no-referrer"),
    );
    headers.insert(
        "permissions-policy",
        HeaderValue::from_static("camera=(), microphone=(), geolocation=()"),
    );
    response
}

async fn shutdown_signal() {
    let ctrl_c = async {
        let _ = tokio::signal::ctrl_c().await;
    };
    #[cfg(unix)]
    let terminate = async {
        if let Ok(mut signal) =
            tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
        {
            signal.recv().await;
        }
    };
    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();
    tokio::select! { _ = ctrl_c => {}, _ = terminate => {} }
}

#[derive(Debug)]
struct ApiError {
    status: StatusCode,
    message: String,
}

impl ApiError {
    fn forbidden(message: &str) -> Self {
        Self {
            status: StatusCode::FORBIDDEN,
            message: message.into(),
        }
    }

    fn unavailable(message: &str) -> Self {
        Self {
            status: StatusCode::SERVICE_UNAVAILABLE,
            message: message.into(),
        }
    }
}

impl From<anyhow::Error> for ApiError {
    fn from(error: anyhow::Error) -> Self {
        Self {
            status: StatusCode::BAD_REQUEST,
            message: error.to_string(),
        }
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        (
            self.status,
            Json(json!({"ok": false, "error": self.message})),
        )
            .into_response()
    }
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::*;
    use crate::{
        cli::RuntimeArgs,
        config::{ControllerSettings, RuntimeConfig},
        harness::HarnessKind,
    };
    use tempfile::tempdir;

    #[test]
    fn dashboard_workers_do_not_inherit_the_tokio_runtime() {
        let workers = DashboardWorkers::new(1, 4).unwrap();
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let outside_runtime = runtime
            .block_on(workers.run(|| Ok(tokio::runtime::Handle::try_current().is_err())))
            .unwrap();
        assert!(outside_runtime);
    }

    #[test]
    fn dashboard_revision_digest_ignores_probe_clock_but_not_health() {
        let mut first = json!({
            "generated_at_unix": 10,
            "gateway_health": {
                "local": {"passed": true, "observed_at_unix": 11}
            },
            "requests": []
        });
        let mut second = json!({
            "generated_at_unix": 20,
            "gateway_health": {
                "local": {"passed": true, "observed_at_unix": 21}
            },
            "requests": []
        });
        normalize_dashboard_digest_value(&mut first);
        normalize_dashboard_digest_value(&mut second);
        assert_eq!(first, second);

        second["gateway_health"]["local"]["passed"] = json!(false);
        assert_ne!(first, second);
    }

    #[test]
    fn logical_agent_dashboard_summary_omits_task_and_host_data() {
        let record = LogicalAgentRecord {
            schema_version: 1,
            agent_id: "agent-1234".into(),
            name: "worker-one".into(),
            owner: "owner-one".into(),
            domain: StateDomain::Scientific,
            state_profile: "scientific-worker".into(),
            state_run_id: "agent-1234".into(),
            objective: "private objective".into(),
            role: "private role".into(),
            workspace: "/private/workspace".into(),
            harness: HarnessKind::Pi,
            thinking: None,
            runtime: RuntimeArgs::default(),
            runtime_generation: 2,
            active_sandbox: None,
            sessions_started: 3,
            sessions_completed: 2,
            failed_sessions: 1,
            last_result_class: Some("passed".into()),
            lifecycle: LogicalAgentLifecycle::Idle,
            revision: 4,
            created_at_unix: 1,
            updated_at_unix: 2,
        };
        let value = serde_json::to_value(LogicalAgentSummary::from(&record)).unwrap();
        for field in [
            "agent_id",
            "owner",
            "state_run_id",
            "objective",
            "role",
            "workspace",
            "harness",
            "thinking",
            "runtime",
        ] {
            assert!(value.get(field).is_none(), "dashboard exposed {field}");
        }
        assert_eq!(value["sessions_started"], 3);
        assert_eq!(value["sessions_completed"], 2);
    }

    #[test]
    fn audit_search_is_bounded_and_omits_metadata() {
        let temp = tempdir().unwrap();
        let config = RuntimeConfig {
            root: temp.path().join("root"),
            state_dir: temp.path().join("state"),
            config_dir: temp.path().join("config"),
            openshell_bin: "openshell".into(),
            docker_bin: "docker".into(),
        };
        for path in [&config.root, &config.state_dir, &config.config_dir] {
            fs::create_dir_all(path).unwrap();
        }
        fs::create_dir_all(config.audit_dir()).unwrap();
        config
            .save_settings(&ControllerSettings::default())
            .unwrap();
        audit::append(
            &config,
            "operator",
            "policy.review",
            "sandbox/one",
            "passed",
            json!({"private_note": "must-not-appear"}),
        )
        .unwrap();
        audit::append(
            &config,
            "operator",
            "policy.review",
            "sandbox/two",
            "passed",
            json!({}),
        )
        .unwrap();
        let page = dashboard_search(
            &config,
            &DashboardSearchQuery {
                q: "policy".into(),
                scope: "audit".into(),
                limit: 1,
                cursor: 0,
            },
        )
        .unwrap();
        assert_eq!(page.total, 2);
        assert_eq!(page.results.len(), 1);
        assert_eq!(page.next_cursor, Some(1));
        assert!(
            !serde_json::to_string(&page)
                .unwrap()
                .contains("must-not-appear")
        );
    }
}
