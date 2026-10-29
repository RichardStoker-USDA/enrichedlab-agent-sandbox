use std::{
    collections::VecDeque,
    convert::Infallible,
    fs::{File as StdFile, OpenOptions},
    io::{Read, Seek, SeekFrom},
    os::unix::fs::OpenOptionsExt,
    path::{Component, Path, PathBuf},
    sync::{Arc, Mutex},
    time::Duration,
};

use anyhow::{Context, Result};
use axum::{
    Json, Router,
    body::Body,
    extract::{DefaultBodyLimit, Path as AxumPath, Query, State},
    http::{HeaderMap, HeaderValue, StatusCode, header},
    response::{IntoResponse, Response, Sse, sse::Event, sse::KeepAlive},
    routing::{get, post, put},
};
use futures_util::stream;
use reqwest::Url;
use serde::{Deserialize, Serialize};
use serde_json::json;
use sha2::{Digest, Sha256};
use tokio::{fs::File, net::TcpListener, time::sleep};
use tokio_util::io::ReaderStream;
use tower_http::{
    request_id::{MakeRequestUuid, PropagateRequestIdLayer, SetRequestIdLayer},
    trace::TraceLayer,
};
use uuid::Uuid;

use super::{
    auth::{
        AuthError, AuthVerifier, BrokerAuthVerifier, CloudflareAccessVerifier,
        ServiceAccountOnlyVerifier, ServiceAccountTokenVerifier,
    },
    config::{ApiAuthentication, ServeArgs},
    model::{ArtifactView, SubmitTaskRequest, TaskEventView},
    store::{Store, StoreError, StoreErrorKind},
};

const IDEMPOTENCY_KEY: &str = "idempotency-key";
const MUTATION_INTENT: &str = "x-agent-sandbox-intent";
const REQUEST_ID: &str = "x-request-id";
const ARTIFACT_SHA256: &str = "x-artifact-sha256";
const MAX_OBJECTIVE_BYTES: usize = 8_192;

mod analyses;
mod campaigns;
mod changes;
mod datasets;
mod documents;
mod figures;
mod knowledge;
mod lab_notes;
mod lab_tools;
mod library;
mod live_output;
mod permits;
mod projects;
mod research_runs;
mod result_kinds;
mod review;
mod table_reads;
#[cfg(test)]
mod test_support;

#[derive(Clone)]
pub struct ApiState {
    workspace_ui: bool,
    site: super::model::SiteView,
    workspace_catalog: Arc<super::workspace_catalog::WorkspaceCatalog>,
    store: Store,
    verifier: Arc<dyn AuthVerifier>,
    artifact_root: PathBuf,
    public_url: Option<Url>,
    listings: Arc<Mutex<super::evidence_package::ListingCache>>,
    tables: Arc<Mutex<super::tables::TableCache>>,
    package_permits: permits::Permits,
    /// Large response bodies, such as a table page, built at once.
    body_permits: permits::Permits,
    /// Live output streams open now.
    output_streams: Arc<live_output::OutputStreams>,
    library: Option<Arc<super::library::LibraryService>>,
    library_permits: Arc<tokio::sync::Semaphore>,
    knowledge_person: Option<Arc<super::knowledge_person::KnowledgePerson>>,
    /// The upload guard for lab datasets (use case `lab-dataset`); uploads share `library_permits` as upload memory.
    dataset_guard: Option<Arc<super::guard_client::GuardClient>>,
    /// This replica's change listener and its streams' shared limits; none when the stream is off.
    changes: Option<Arc<changes::ChangeHub>>,
    /// Whether a project may start from several starting points (`--combine-starting-points`).
    combine_starting_points: bool,
}

#[derive(Debug)]
pub struct ApiError {
    status: StatusCode,
    code: &'static str,
    message: &'static str,
}

#[derive(Debug, Serialize)]
struct ErrorEnvelope {
    error: ErrorBody,
}

#[derive(Debug, Serialize)]
struct ErrorBody {
    code: &'static str,
    message: &'static str,
}

#[derive(Debug, Deserialize)]
struct ListQuery {
    #[serde(default = "default_task_limit")]
    limit: i32,
}

#[derive(Debug, Deserialize)]
struct EventQuery {
    #[serde(default)]
    after: i64,
    #[serde(default = "default_event_limit")]
    limit: i32,
}

#[derive(Debug, Deserialize)]
struct ScientificHistoryQuery {
    #[serde(default)]
    cursor: Option<String>,
    #[serde(default = "default_scientific_history_limit")]
    limit: i32,
}

#[derive(Debug, Deserialize)]
struct ScientificHistoryEvidenceQuery {
    attempt: i32,
    generation: i64,
    #[serde(default)]
    after: Option<Uuid>,
    #[serde(default = "default_scientific_history_limit")]
    limit: i32,
}

#[derive(Clone, Copy, Debug)]
struct ScientificHistoryCursor {
    attempt: i32,
    generation: i64,
    sequence: i64,
}

#[derive(Debug, Serialize)]
struct AcceptedTask {
    task_id: Uuid,
    status: &'static str,
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        (
            self.status,
            Json(ErrorEnvelope {
                error: ErrorBody {
                    code: self.code,
                    message: self.message,
                },
            }),
        )
            .into_response()
    }
}

pub async fn serve(args: ServeArgs) -> Result<()> {
    let config = args.resolve()?;
    super::kinds::registry().map_err(|error| anyhow::anyhow!("result kind registry: {error}"))?;
    let store = Store::connect(&config.database_dsn, config.tenant, config.database_pool)?;
    let store_for_changes = store.clone();
    let verifier: Arc<dyn AuthVerifier> = match config.authentication {
        ApiAuthentication::Cloudflare {
            issuer,
            audience,
            jwks_url,
            jwks_ca_pem,
            service_account_auth,
        } => {
            let access_verifier: Arc<dyn AuthVerifier> = Arc::new(
                CloudflareAccessVerifier::new(issuer, audience, jwks_url, jwks_ca_pem.as_deref())
                    .context("failed to configure Cloudflare Access verification")?,
            );
            let service_account_verifier: Option<Arc<dyn AuthVerifier>> = service_account_auth
                .map(ServiceAccountTokenVerifier::new)
                .transpose()
                .context("failed to configure Kubernetes ServiceAccount authentication")?
                .map(|verifier| Arc::new(verifier) as Arc<dyn AuthVerifier>);
            Arc::new(BrokerAuthVerifier::new(
                access_verifier,
                service_account_verifier,
            ))
        }
        ApiAuthentication::ServiceAccountOnly {
            service_account_auth,
        } => Arc::new(ServiceAccountOnlyVerifier::new(Arc::new(
            ServiceAccountTokenVerifier::new(service_account_auth)
                .context("failed to configure Kubernetes ServiceAccount authentication")?,
        ))),
    };
    let state = ApiState {
        workspace_ui: config.workspace_ui,
        site: super::model::SiteView {
            organization: config.site_organization,
        },
        workspace_catalog: Arc::new(match &config.workspace_catalog {
            Some(path) => super::workspace_catalog::WorkspaceCatalog::load(path)?,
            None => super::workspace_catalog::WorkspaceCatalog::default(),
        }),
        store,
        verifier,
        artifact_root: config.artifact_root,
        public_url: config.public_url,
        listings: Arc::default(),
        tables: Arc::default(),
        package_permits: permits::Permits::new(2),
        body_permits: permits::Permits::new(4),
        output_streams: Arc::default(),
        library: super::library::load_optional(config.library_config.as_deref())
            .context("failed to load the lab library configuration")?
            .map(Arc::new),
        // The memory library uploads may hold at once, in KiB; each upload takes its weight (api/library.rs).
        library_permits: Arc::new(tokio::sync::Semaphore::new(library::UPLOAD_BUDGET_KIB)),
        knowledge_person: super::knowledge_person::load_optional(
            config.knowledge_person_config.as_deref(),
        )
        .context("failed to load the knowledge person configuration")?
        .map(Arc::new),
        dataset_guard: config
            .dataset_guard_config
            .as_deref()
            .map(datasets::load_guard)
            .transpose()
            .context("failed to load the dataset guard configuration")?
            .map(Arc::new),
        changes: config
            .change_stream
            .then(|| changes::ChangeHub::start(store_for_changes)),
        combine_starting_points: config.combine_starting_points,
    };
    let app = router(state);
    let listener = TcpListener::bind(config.bind)
        .await
        .with_context(|| format!("failed to bind broker listener {}", config.bind))?;
    tracing::info!(address = %config.bind, "broker API listening");
    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown_signal())
        .await
        .context("broker API stopped unexpectedly")
}

pub fn router(state: ApiState) -> Router {
    let router = if state.workspace_ui {
        workspace_routes(Path::new("/usr/local/share/enrichedlab/workspace"))
    } else {
        Router::new()
    };
    router
        .route("/health/live", get(live))
        .route("/health/ready", get(ready))
        .route("/api/v1/me", get(me))
        .route(
            "/api/v1/labs/{lab}/campaigns",
            get(campaigns::list).post(campaigns::create),
        )
        .route("/api/v1/campaigns/{campaign}", get(campaigns::get_one))
        .route("/api/v1/campaigns/{campaign}/start", post(campaigns::start))
        .route("/api/v1/campaigns/{campaign}/pause", post(campaigns::pause))
        .route(
            "/api/v1/campaigns/{campaign}/resume",
            post(campaigns::resume),
        )
        .route("/api/v1/campaigns/{campaign}/stop", post(campaigns::stop))
        .route(
            "/api/v1/campaigns/{campaign}/events",
            get(campaigns::events),
        )
        .route(
            "/api/v1/campaigns/{campaign}/records",
            get(campaigns::records),
        )
        .route(
            "/api/v1/campaigns/{campaign}/planner-calls",
            get(campaigns::planner_calls),
        )
        .route(
            "/api/v1/campaigns/{campaign}/planner-texts/{sha256}",
            get(campaigns::planner_text),
        )
        .route("/api/v1/changes/stream", get(changes::stream))
        .route(
            "/api/v1/projects",
            get(projects::list).post(projects::create),
        )
        .route("/api/v1/projects/{project}", get(projects::get))
        .route("/api/v1/projects/{project}/look", post(projects::look))
        .route(
            "/api/v1/projects/{project}/analyses",
            get(projects::analyses),
        )
        .route(
            "/api/v1/projects/{project}/workspaces",
            get(projects::workspaces),
        )
        .route(
            "/api/v1/labs/{lab}/workspaces",
            get(projects::lab_workspaces),
        )
        .route(
            "/api/v1/projects/{project}/prepare",
            post(projects::prepare),
        )
        .route(
            "/api/v1/projects/{project}/runs",
            get(research_runs::list).post(research_runs::start),
        )
        .route(
            "/api/v1/research-runs/{run}/pause",
            post(research_runs::pause),
        )
        .route(
            "/api/v1/research-runs/{run}/turns",
            get(research_runs::turns),
        )
        .route(
            "/api/v1/research-runs/{run}/literature-reviews/{sha256}",
            get(research_runs::literature_review),
        )
        .route(
            "/api/v1/research-runs/{run}/orientation",
            get(research_runs::orientation),
        )
        .route(
            "/api/v1/research-runs/{run}/contexts/{sha256}",
            get(research_runs::context),
        )
        .route(
            "/api/v1/research-runs/{run}/events",
            get(review::run_events),
        )
        .route("/api/v1/projects/{project}/grants", post(projects::grant))
        .route("/api/v1/projects/{project}/members", get(review::members))
        .route(
            "/api/v1/projects/{project}/inspect",
            post(projects::inspect).delete(review::end_inspection),
        )
        .route("/api/v1/projects/{project}/package", get(review::package))
        .route(
            "/api/v1/projects/{project}/package/preview",
            get(review::package_preview),
        )
        .route("/api/v1/admin/projects", get(review::admin_projects))
        .route(
            "/api/v1/projects/{project}/checkpoints",
            post(projects::checkpoint).get(projects::history),
        )
        .route("/api/v1/projects/{project}/context", get(projects::context))
        .route("/api/v1/projects/{project}/audit", get(projects::audit))
        .route(
            "/api/v1/projects/{project}/plan-ledger",
            get(projects::plan_ledger),
        )
        .route(
            "/api/v1/projects/{project}/acknowledgements",
            get(projects::acknowledgements),
        )
        .route(
            "/api/v1/projects/{project}/events/stream",
            get(projects::stream),
        )
        .route("/api/v1/knowledge/commons", get(knowledge::commons))
        .route(
            "/api/v1/knowledge/commons/collections",
            get(knowledge::collections),
        )
        .route(
            "/api/v1/knowledge/commons/collections/baseline",
            post(knowledge::collection_baseline),
        )
        .route(
            "/api/v1/knowledge/resources/{resource}/baseline",
            post(knowledge::baseline),
        )
        .route(
            "/api/v1/knowledge/labs/{lab}/proposals",
            get(knowledge::proposals),
        )
        .route(
            "/api/v1/knowledge/relations/{relation}/review",
            post(knowledge::review),
        )
        .route("/api/v1/inbox", get(projects::inbox))
        .route(
            "/api/v1/inbox/acknowledgements",
            post(projects::acknowledge),
        )
        .route("/api/v1/tasks", post(submit_task).get(list_tasks))
        .route("/api/v1/tasks/{task}", get(get_task))
        .route("/api/v1/tasks/{task}/cancel", post(cancel_task))
        .route("/api/v1/tasks/{task}/events", get(list_events))
        .route("/api/v1/tasks/{task}/events/stream", get(stream_events))
        .route(
            "/api/v1/tasks/{task}/log/stream",
            get(live_output::output_stream),
        )
        .route(
            "/api/v1/tasks/{task}/scientific-history",
            get(scientific_history),
        )
        .route(
            "/api/v1/tasks/{task}/scientific-history/events/{event}/evidence",
            get(scientific_history_evidence),
        )
        .route("/api/v1/kinds", get(result_kinds::kinds_registry))
        .route("/api/v1/tasks/{task}/artifacts", get(list_artifacts))
        .route(
            "/api/v1/tasks/{task}/artifacts/{artifact}/kind",
            get(result_kinds::artifact_kind),
        )
        .route(
            "/api/v1/tasks/{task}/artifacts/{artifact}/download",
            get(download_artifact),
        )
        .route(
            "/api/v1/tasks/{task}/artifacts/{artifact}/contents",
            get(review::artifact_contents),
        )
        .route(
            "/api/v1/tasks/{task}/artifacts/{artifact}/contents/file",
            get(review::artifact_member),
        )
        .route(
            "/api/v1/tasks/{task}/artifacts/{artifact}/digest",
            get(review::artifact_digest),
        )
        .route(
            "/api/v1/tasks/{task}/artifacts/{artifact}/pages",
            get(table_reads::artifact_pages),
        )
        .route(
            "/api/v1/tasks/{task}/artifacts/{artifact}/summary",
            get(table_reads::artifact_summary),
        )
        .route(
            "/api/v1/tasks/{task}/reviews",
            get(review::task_reviews).post(review::record_task_review),
        )
        .route(
            "/api/v1/projects/{project}/reviews",
            get(review::project_reviews),
        )
        .route(
            "/api/v1/projects/{project}/working-copies",
            get(research_runs::working_copies),
        )
        .route(
            "/api/v1/tasks/{task}/generated-code",
            get(review::generated_code),
        )
        .route("/api/v1/tasks/{task}/query", get(review::query))
        .route("/api/v1/tasks/{task}/figure", get(figures::detail))
        .route("/api/v1/tasks/{task}/document", get(documents::detail))
        .route(
            "/api/v1/projects/{project}/documents",
            post(documents::request),
        )
        .route(
            "/api/v1/research-runs/{run}/documents",
            get(documents::for_run),
        )
        .route("/api/v1/tasks/{task}/figures", get(figures::versions))
        .route("/api/v1/figures/catalog", get(figures::catalog))
        .route("/api/v1/projects/{project}/figures", post(figures::request))
        .route(
            "/api/v1/projects/{project}/figures/previews",
            post(figures::preview),
        )
        .route(
            "/api/v1/projects/{project}/figures/{figure}/manuscript",
            put(figures::mark_manuscript).delete(figures::clear_manuscript),
        )
        .route(
            "/api/v1/tasks/{task}/template-analysis",
            get(analyses::detail),
        )
        .route(
            "/api/v1/tasks/{task}/template-analyses",
            get(analyses::versions),
        )
        .route("/api/v1/template-analyses/catalog", get(analyses::catalog))
        .route(
            "/api/v1/projects/{project}/template-analyses",
            post(analyses::request),
        )
        .route(
            "/api/v1/projects/{project}/datasets",
            get(datasets::list).post(datasets::upload),
        )
        .route(
            "/api/v1/projects/{project}/datasets/{dataset}",
            get(datasets::detail),
        )
        .route(
            "/api/v1/projects/{project}/datasets/{dataset}/intake",
            post(datasets::start_intake),
        )
        .route(
            "/api/v1/projects/{project}/datasets/{dataset}/preparations",
            post(datasets::prepare),
        )
        .route("/api/v1/tasks/{task}/lineage", get(review::lineage))
        .route("/api/v1/labs/{lab}/notes", get(lab_notes::lab_notes))
        .route("/api/v1/labs/{lab}/tools", get(lab_tools::lab_tools))
        .route(
            "/api/v1/projects/{project}/notes/share",
            post(lab_notes::share),
        )
        .route("/api/v1/notes/{note}/retire", post(lab_notes::retire))
        .route("/api/v1/notes/{note}/renew", post(lab_notes::renew))
        .route("/api/v1/labs/{lab}/library", get(library::library))
        .route(
            "/api/v1/labs/{lab}/library/papers",
            post(library::add_paper).layer(DefaultBodyLimit::max(
                super::library::MAX_PAPER_BYTES + 64 * 1024,
            )),
        )
        .route("/api/v1/labs/{lab}/library/search", get(library::search))
        .route(
            "/api/v1/labs/{lab}/library/papers/{paper}",
            get(library::paper),
        )
        .route(
            "/api/v1/labs/{lab}/library/papers/{paper}/remove",
            post(library::remove),
        )
        .route(
            "/api/v1/labs/{lab}/library/papers/{paper}/acceptance",
            post(library::accept),
        )
        .route(
            "/api/v1/labs/{lab}/library/papers/{paper}/labels",
            post(library::label),
        )
        .with_state(state)
        .layer(axum::middleware::from_fn(conditional_get))
        .layer(DefaultBodyLimit::max(1024 * 1024))
        .layer(PropagateRequestIdLayer::new(
            header::HeaderName::from_static(REQUEST_ID),
        ))
        .layer(SetRequestIdLayer::new(
            header::HeaderName::from_static(REQUEST_ID),
            MakeRequestUuid,
        ))
        .layer(TraceLayer::new_for_http().make_span_with(request_span))
        .layer(axum::middleware::from_fn(security_headers))
}

/// The span for one request: its method and path only, since a query string can name a table column or a package
/// member.
fn request_span<B>(request: &axum::http::Request<B>) -> tracing::Span {
    tracing::debug_span!(
        "request",
        method = %request.method(),
        path = %request.uri().path()
    )
}

fn workspace_routes<S: Clone + Send + Sync + 'static>(root: &Path) -> Router<S> {
    use tower_http::services::{ServeDir, ServeFile};
    Router::new()
        .route_service("/", ServeFile::new(root.join("index.html")))
        .nest_service(
            "/assets",
            ServeDir::new(root.join("assets")).append_index_html_on_directories(false),
        )
        .layer(axum::middleware::from_fn(workspace_security_headers))
}

async fn workspace_security_headers(
    request: axum::extract::Request,
    next: axum::middleware::Next,
) -> Response {
    let mut response = next.run(request).await;
    response.headers_mut().insert("content-security-policy", HeaderValue::from_static(
        "default-src 'none'; script-src 'self'; style-src 'self' 'unsafe-inline'; img-src 'self' blob: data:; font-src 'self'; connect-src 'self'; base-uri 'none'; form-action 'self'; frame-ancestors 'none'; object-src 'none'"
    ));
    response
}

async fn live() -> impl IntoResponse {
    (StatusCode::OK, Json(json!({"status": "live"})))
}

async fn ready(State(state): State<ApiState>) -> Result<impl IntoResponse, ApiError> {
    state
        .store
        .ready_for_review_api()
        .await
        .map_err(ApiError::from)?;
    state
        .store
        .ready_for_campaign_api()
        .await
        .map_err(ApiError::from)?;
    state
        .store
        .ready_for_kinds(true)
        .await
        .map_err(ApiError::from)?;
    state
        .store
        .ready_for_live_output(true)
        .await
        .map_err(ApiError::from)?;
    state
        .store
        .ready_for_figure_previews(true)
        .await
        .map_err(ApiError::from)?;
    state
        .store
        .ready_for_template_analyses(true)
        .await
        .map_err(ApiError::from)?;
    state
        .store
        .ready_for_lab_datasets(true)
        .await
        .map_err(ApiError::from)?;
    state
        .store
        .ready_for_change_feed()
        .await
        .map_err(ApiError::from)?;
    Ok((StatusCode::OK, Json(json!({"status": "ready"}))))
}

async fn me(
    State(state): State<ApiState>,
    headers: HeaderMap,
) -> Result<impl IntoResponse, ApiError> {
    let principal = authenticate(&state, &headers).await?;
    Ok(Json(super::model::MeResponse {
        me: state.store.me(principal).await.map_err(ApiError::from)?,
        site: state.site.clone(),
        change_stream: state.changes.is_some(),
    }))
}

async fn submit_task(
    State(state): State<ApiState>,
    headers: HeaderMap,
    Json(request): Json<SubmitTaskRequest>,
) -> Result<Response, ApiError> {
    require_mutation_intent(&headers)?;
    validate_key(&request.group)?;
    validate_key(&request.profile)?;
    if request.project_id.is_some() != request.action_key.is_some()
        || request
            .action_key
            .as_ref()
            .is_some_and(|s| !super::project::action_key(s))
        || (request.parent_task_id.is_some() && request.project_id.is_none())
    {
        return Err(ApiError::invalid(
            "project_binding_invalid",
            "Project and action key must be supplied together",
        ));
    }
    if request.objective.trim().is_empty()
        || request.objective.len() > MAX_OBJECTIVE_BYTES
        || request.objective.chars().any(|character| character == '\0')
    {
        return Err(ApiError::invalid(
            "objective_invalid",
            "Objective must contain 1-8192 bytes",
        ));
    }
    let idempotency = headers
        .get(IDEMPOTENCY_KEY)
        .ok_or_else(|| ApiError::invalid("idempotency_key_missing", "Idempotency-Key is required"))?
        .to_str()
        .map_err(|_| ApiError::invalid("idempotency_key_invalid", "Idempotency-Key is invalid"))?;
    if idempotency.is_empty()
        || idempotency.len() > 128
        || idempotency
            .chars()
            .any(|character| character.is_control() || character.is_whitespace())
    {
        return Err(ApiError::invalid(
            "idempotency_key_invalid",
            "Idempotency-Key is invalid",
        ));
    }
    let principal = authenticate(&state, &headers).await?;
    let request_sha256 = request_digest(&request);
    let task = if request.project_id.is_some() {
        state
            .store
            .submit_project_task(principal, &request, idempotency, &request_sha256)
            .await
    } else {
        state
            .store
            .submit_task(
                principal,
                &request.group,
                &request.profile,
                &request.objective,
                idempotency,
                &request_sha256,
            )
            .await
    }
    .map_err(ApiError::from)?;
    let location = task_location(state.public_url.as_ref(), task)?;
    let mut response = (
        StatusCode::ACCEPTED,
        Json(AcceptedTask {
            task_id: task,
            status: "queued",
        }),
    )
        .into_response();
    response.headers_mut().insert(header::LOCATION, location);
    Ok(response)
}

async fn list_tasks(
    State(state): State<ApiState>,
    headers: HeaderMap,
    Query(query): Query<ListQuery>,
) -> Result<impl IntoResponse, ApiError> {
    if !(1..=200).contains(&query.limit) {
        return Err(ApiError::invalid(
            "list_limit_invalid",
            "Limit must be between 1 and 200",
        ));
    }
    let principal = authenticate(&state, &headers).await?;
    let tasks = state
        .store
        .list_tasks(principal, query.limit)
        .await
        .map_err(ApiError::from)?;
    Ok(Json(json!({"tasks": tasks})))
}

async fn get_task(
    State(state): State<ApiState>,
    headers: HeaderMap,
    AxumPath(task): AxumPath<Uuid>,
) -> Result<impl IntoResponse, ApiError> {
    let principal = authenticate(&state, &headers).await?;
    Ok(Json(
        state
            .store
            .get_task(principal, task)
            .await
            .map_err(ApiError::from)?,
    ))
}

async fn cancel_task(
    State(state): State<ApiState>,
    headers: HeaderMap,
    AxumPath(task): AxumPath<Uuid>,
) -> Result<impl IntoResponse, ApiError> {
    require_mutation_intent(&headers)?;
    let principal = authenticate(&state, &headers).await?;
    let status = state
        .store
        .cancel_task(principal, task)
        .await
        .map_err(ApiError::from)?;
    Ok((
        StatusCode::ACCEPTED,
        Json(json!({"task_id": task, "status": status})),
    ))
}

async fn list_events(
    State(state): State<ApiState>,
    headers: HeaderMap,
    AxumPath(task): AxumPath<Uuid>,
    Query(query): Query<EventQuery>,
) -> Result<impl IntoResponse, ApiError> {
    validate_event_query(&query)?;
    let principal = authenticate(&state, &headers).await?;
    let events = state
        .store
        .list_events(principal, task, query.after, query.limit)
        .await
        .map_err(ApiError::from)?;
    Ok(Json(json!({"events": events})))
}

async fn scientific_history(
    State(state): State<ApiState>,
    headers: HeaderMap,
    AxumPath(task): AxumPath<Uuid>,
    Query(query): Query<ScientificHistoryQuery>,
) -> Result<impl IntoResponse, ApiError> {
    let cursor = parse_scientific_history_cursor(query.cursor.as_deref())?;
    if !(1..=100).contains(&query.limit) {
        return Err(ApiError::invalid(
            "scientific_history_cursor_invalid",
            "Scientific history cursor or limit is invalid",
        ));
    }
    let principal = authenticate(&state, &headers).await?;
    let page = state
        .store
        .scientific_history_page(
            principal,
            task,
            cursor.map(|cursor| (cursor.attempt, cursor.generation, cursor.sequence)),
            query.limit,
        )
        .await
        .map_err(ApiError::from)?;
    let next_cursor = match (
        page.attempt,
        page.lease_generation,
        page.events.last().map(|event| event.sequence),
    ) {
        (Some(attempt), Some(generation), Some(sequence)) => {
            Some(format_scientific_history_cursor(ScientificHistoryCursor {
                attempt,
                generation,
                sequence,
            }))
        }
        _ => None,
    };
    Ok(Json(json!({
        "coverage": page.coverage,
        "attempt": page.attempt,
        "lease_generation": page.lease_generation,
        "events": page.events,
        "next_cursor": next_cursor,
    })))
}

async fn scientific_history_evidence(
    State(state): State<ApiState>,
    headers: HeaderMap,
    AxumPath((task, event)): AxumPath<(Uuid, Uuid)>,
    Query(query): Query<ScientificHistoryEvidenceQuery>,
) -> Result<impl IntoResponse, ApiError> {
    if query.attempt < 0 || query.generation < 1 || !(1..=100).contains(&query.limit) {
        return Err(ApiError::invalid(
            "scientific_history_evidence_cursor_invalid",
            "Scientific history evidence cursor or limit is invalid",
        ));
    }
    let principal = authenticate(&state, &headers).await?;
    let page = state
        .store
        .scientific_history_evidence_page(
            principal,
            task,
            query.attempt,
            query.generation,
            event,
            query.after,
            query.limit,
        )
        .await
        .map_err(ApiError::from)?;
    let next_cursor = page.evidence.last().map(|evidence| evidence.artifact_id);
    Ok(Json(json!({
        "attempt": page.attempt,
        "lease_generation": page.lease_generation,
        "evidence": page.evidence,
        "next_cursor": next_cursor,
    })))
}

async fn stream_events(
    State(state): State<ApiState>,
    headers: HeaderMap,
    AxumPath(task): AxumPath<Uuid>,
    Query(query): Query<EventQuery>,
) -> Result<impl IntoResponse, ApiError> {
    validate_event_query(&query)?;
    let principal = authenticate(&state, &headers).await?;
    state
        .store
        .get_task(principal, task)
        .await
        .map_err(ApiError::from)?;
    struct Cursor {
        state: ApiState,
        principal: Uuid,
        task: Uuid,
        after: i64,
        pending: VecDeque<TaskEventView>,
        done: bool,
    }
    let cursor = Cursor {
        state,
        principal,
        task,
        after: query.after,
        pending: VecDeque::new(),
        done: false,
    };
    let events = stream::unfold(cursor, |mut cursor| async move {
        loop {
            if cursor.done {
                return None;
            }
            if let Some(event) = cursor.pending.pop_front() {
                cursor.after = event.sequence;
                let payload = serde_json::to_string(&event).unwrap_or_else(|_| "{}".into());
                return Some((
                    Ok::<Event, Infallible>(
                        Event::default()
                            .id(event.sequence.to_string())
                            .event("task_event")
                            .data(payload),
                    ),
                    cursor,
                ));
            }
            match cursor
                .state
                .store
                .list_events(cursor.principal, cursor.task, cursor.after, 100)
                .await
            {
                Ok(events) if events.is_empty() => {
                    match cursor
                        .state
                        .store
                        .get_task(cursor.principal, cursor.task)
                        .await
                    {
                        Ok(task) if is_terminal_status(&task.status) => {
                            cursor.done = true;
                            return Some((
                                Ok(Event::default()
                                    .event("stream_complete")
                                    .data(format!("{{\"status\":\"{}\"}}", task.status))),
                                cursor,
                            ));
                        }
                        Ok(_) => sleep(Duration::from_secs(1)).await,
                        Err(_) => {
                            cursor.done = true;
                            return Some((
                                Ok(Event::default()
                                    .event("stream_closed")
                                    .data("{\"code\":\"event_stream_unavailable\"}")),
                                cursor,
                            ));
                        }
                    }
                }
                Ok(events) => cursor.pending.extend(events),
                Err(_) => {
                    cursor.done = true;
                    return Some((
                        Ok(Event::default()
                            .event("stream_closed")
                            .data("{\"code\":\"event_stream_unavailable\"}")),
                        cursor,
                    ));
                }
            }
        }
    });
    Ok(Sse::new(events).keep_alive(
        KeepAlive::new()
            .interval(Duration::from_secs(15))
            .text("keepalive"),
    ))
}

async fn list_artifacts(
    State(state): State<ApiState>,
    headers: HeaderMap,
    AxumPath(task): AxumPath<Uuid>,
) -> Result<impl IntoResponse, ApiError> {
    let principal = authenticate(&state, &headers).await?;
    let artifacts = state
        .store
        .list_artifacts(principal, task)
        .await
        .map_err(ApiError::from)?;
    Ok(Json(
        result_kinds::list_with_kinds(&state, principal, task, artifacts).await?,
    ))
}

async fn download_artifact(
    State(state): State<ApiState>,
    headers: HeaderMap,
    AxumPath((task, artifact)): AxumPath<(Uuid, Uuid)>,
) -> Result<Response, ApiError> {
    let principal = authenticate(&state, &headers).await?;
    let record = state
        .store
        .get_artifact(principal, task, artifact)
        .await
        .map_err(ApiError::from)?;
    let path = resolve_artifact_path(&state.artifact_root, task, &record)?;
    let expected_hash = record.sha256.clone();
    let expected_size = record.size_bytes as u64;
    let range = match table_reads::range_request(&headers, expected_size) {
        table_reads::RangeRequest::Unsatisfiable => {
            return table_reads::range_not_satisfiable(expected_size);
        }
        table_reads::RangeRequest::Part(first, last) => Some((first, last)),
        table_reads::RangeRequest::Whole => None,
    };
    let open_path = path.clone();
    let file = tokio::task::spawn_blocking(move || {
        // The whole file matches its digest on this descriptor before any byte, or range, is sent.
        let mut file = open_verified_artifact(&open_path, expected_size, &expected_hash)?;
        if let Some((first, _)) = range {
            file.seek(SeekFrom::Start(first))?;
        }
        Ok::<_, std::io::Error>(file)
    })
    .await
    .map_err(|_| ApiError::internal())?
    .map_err(|_| ApiError {
        status: StatusCode::CONFLICT,
        code: "artifact_integrity_failed",
        message: "Artifact integrity check failed",
    })?;
    let file = File::from_std(file);
    let body = match range {
        Some((first, last)) => Body::from_stream(ReaderStream::new(tokio::io::AsyncReadExt::take(
            file,
            last - first + 1,
        ))),
        None => Body::from_stream(ReaderStream::new(file)),
    };
    let file_name = path
        .file_name()
        .and_then(|value| value.to_str())
        .unwrap_or("artifact")
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() || matches!(character, '.' | '-' | '_') {
                character
            } else {
                '_'
            }
        })
        .collect::<String>();
    let mut response = Response::new(body);
    response.headers_mut().insert(
        header::CONTENT_TYPE,
        HeaderValue::from_str(&record.media_type)
            .unwrap_or_else(|_| HeaderValue::from_static("application/octet-stream")),
    );
    response.headers_mut().insert(
        header::CONTENT_LENGTH,
        HeaderValue::from_str(&record.size_bytes.to_string()).map_err(|_| ApiError::internal())?,
    );
    response.headers_mut().insert(
        header::CONTENT_DISPOSITION,
        HeaderValue::from_str(&format!("attachment; filename=\"{file_name}\""))
            .map_err(|_| ApiError::internal())?,
    );
    response.headers_mut().insert(
        ARTIFACT_SHA256,
        HeaderValue::from_str(&record.sha256).map_err(|_| ApiError::internal())?,
    );
    response
        .headers_mut()
        .insert(header::ACCEPT_RANGES, HeaderValue::from_static("bytes"));
    if let Some((first, last)) = range {
        table_reads::mark_partial(&mut response, first, last, expected_size)?;
    }
    Ok(response)
}

async fn authenticate(state: &ApiState, headers: &HeaderMap) -> Result<Uuid, ApiError> {
    let identity = state
        .verifier
        .verify(headers)
        .await
        .map_err(ApiError::from)?;
    state
        .store
        .resolve_principal(&identity)
        .await
        .map_err(ApiError::from)
}

fn task_location(public_url: Option<&Url>, task: Uuid) -> Result<HeaderValue, ApiError> {
    let location = match public_url {
        Some(url) => url
            .join(&format!("api/v1/tasks/{task}"))
            .map_err(|_| ApiError::internal())?
            .to_string(),
        None => format!("/api/v1/tasks/{task}"),
    };
    HeaderValue::from_str(&location).map_err(|_| ApiError::internal())
}

pub(crate) fn request_digest(request: &SubmitTaskRequest) -> String {
    let mut digest = Sha256::new();
    digest.update(request.group.as_bytes());
    digest.update([0]);
    digest.update(request.profile.as_bytes());
    digest.update([0]);
    digest.update(request.objective.as_bytes());
    if let Some(project) = request.project_id {
        digest.update([0]);
        digest.update(project.as_bytes());
        digest.update([0]);
        digest.update(request.action_key.as_deref().unwrap_or_default().as_bytes());
        digest.update([0]);
        if let Some(parent) = request.parent_task_id {
            digest.update(parent.as_bytes());
        }
    }
    hex::encode(digest.finalize())
}

fn require_mutation_intent(headers: &HeaderMap) -> Result<(), ApiError> {
    if headers
        .get(MUTATION_INTENT)
        .and_then(|value| value.to_str().ok())
        == Some("mutate")
    {
        return Ok(());
    }
    Err(ApiError {
        status: StatusCode::FORBIDDEN,
        code: "mutation_intent_required",
        message: "A mutation intent header is required",
    })
}

fn resolve_artifact_path(
    root: &Path,
    task: Uuid,
    artifact: &ArtifactView,
) -> Result<PathBuf, ApiError> {
    resolve_task_file(root, task, &artifact.relative_path, artifact.size_bytes)
}

/// A registered artifact lives under `tasks/<task>/` in the artifact root. Refuse any
/// other location, a symlink, or a file whose size differs from its record.
pub(crate) fn resolve_task_file(
    root: &Path,
    task: Uuid,
    relative_path: &str,
    size_bytes: i64,
) -> Result<PathBuf, ApiError> {
    let task_id = task.to_string();
    let relative = Path::new(relative_path);
    if relative.is_absolute()
        || relative
            .components()
            .any(|component| !matches!(component, Component::Normal(_)))
        || relative.components().next().and_then(|value| match value {
            Component::Normal(value) => value.to_str(),
            _ => None,
        }) != Some("tasks")
        || relative.components().nth(1).and_then(|value| match value {
            Component::Normal(value) => value.to_str(),
            _ => None,
        }) != Some(task_id.as_str())
    {
        return Err(ApiError::not_found("artifact_not_found"));
    }
    let root = root.canonicalize().map_err(|_| ApiError::internal())?;
    let candidate = root.join(relative);
    let metadata = std::fs::symlink_metadata(&candidate)
        .map_err(|_| ApiError::not_found("artifact_not_found"))?;
    if metadata.file_type().is_symlink()
        || !metadata.is_file()
        || metadata.len() != size_bytes as u64
    {
        return Err(ApiError {
            status: StatusCode::CONFLICT,
            code: "artifact_integrity_failed",
            message: "Artifact integrity check failed",
        });
    }
    let canonical = candidate
        .canonicalize()
        .map_err(|_| ApiError::not_found("artifact_not_found"))?;
    if !canonical.starts_with(&root) {
        return Err(ApiError::not_found("artifact_not_found"));
    }
    Ok(canonical)
}

fn open_verified_artifact(
    path: &Path,
    expected_size: u64,
    expected_hash: &str,
) -> std::io::Result<StdFile> {
    let mut options = OpenOptions::new();
    options.read(true).custom_flags(libc::O_NOFOLLOW);
    let mut file = options.open(path)?;
    let metadata = file.metadata()?;
    if !metadata.is_file() || metadata.len() != expected_size {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "artifact metadata changed before open",
        ));
    }
    let mut digest = Sha256::new();
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        let read = file.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        digest.update(&buffer[..read]);
    }
    if hex::encode(digest.finalize()) != expected_hash {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "artifact digest changed before open",
        ));
    }
    file.seek(SeekFrom::Start(0))?;
    Ok(file)
}

fn validate_key(value: &str) -> Result<(), ApiError> {
    if value.len() < 2
        || value.len() > 64
        || !value.as_bytes()[0].is_ascii_lowercase()
        || !value.bytes().all(|byte| {
            byte.is_ascii_lowercase() || byte.is_ascii_digit() || matches!(byte, b'-' | b'_')
        })
    {
        return Err(ApiError::invalid(
            "key_invalid",
            "Group or profile key is invalid",
        ));
    }
    Ok(())
}

fn default_scientific_history_limit() -> i32 {
    100
}

fn parse_scientific_history_cursor(
    value: Option<&str>,
) -> Result<Option<ScientificHistoryCursor>, ApiError> {
    let Some(value) = value else {
        return Ok(None);
    };
    let mut fields = value.split('.');
    let valid = matches!(fields.next(), Some("v1"));
    let attempt = fields.next().and_then(|field| field.parse::<i32>().ok());
    let generation = fields.next().and_then(|field| field.parse::<i64>().ok());
    let sequence = fields.next().and_then(|field| field.parse::<i64>().ok());
    if !valid
        || fields.next().is_some()
        || !matches!(attempt, Some(value) if value >= 0)
        || !matches!(generation, Some(value) if value > 0)
        || !matches!(sequence, Some(value) if value >= 0)
    {
        return Err(ApiError::invalid(
            "scientific_history_cursor_invalid",
            "Scientific history cursor or limit is invalid",
        ));
    }
    Ok(Some(ScientificHistoryCursor {
        attempt: attempt.expect("validated attempt"),
        generation: generation.expect("validated generation"),
        sequence: sequence.expect("validated sequence"),
    }))
}

fn format_scientific_history_cursor(cursor: ScientificHistoryCursor) -> String {
    format!(
        "v1.{}.{}.{}",
        cursor.attempt, cursor.generation, cursor.sequence
    )
}

fn validate_event_query(query: &EventQuery) -> Result<(), ApiError> {
    if query.after < 0 || !(1..=1000).contains(&query.limit) {
        return Err(ApiError::invalid(
            "event_cursor_invalid",
            "Event cursor or limit is invalid",
        ));
    }
    Ok(())
}

fn is_terminal_status(status: &str) -> bool {
    matches!(
        status,
        "completed" | "failed" | "cancelled" | "review_required"
    )
}

/// The largest JSON answer that carries an ETag; a larger one is sent whole every time.
const MAX_TAGGED_BODY: u64 = 8 * 1024 * 1024;

/// A JSON read under `/api/v1/` answers with a weak ETag of its body, and a matching `If-None-Match` with 304 and no
/// body. The tag hashes the body, so two readers whose answers differ never share a tag, and a page
/// that reads again after a change notice transfers nothing when its answer is the same.
async fn conditional_get(
    request: axum::extract::Request,
    next: axum::middleware::Next,
) -> Response {
    let eligible =
        request.method() == axum::http::Method::GET && request.uri().path().starts_with("/api/v1/");
    let asked = request.headers().get(header::IF_NONE_MATCH).cloned();
    let response = next.run(request).await;
    let json = response
        .headers()
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|v| v.starts_with("application/json"));
    if !eligible
        || !json
        || response.status() != StatusCode::OK
        || response.headers().contains_key(header::ETAG)
    {
        return response;
    }
    let (mut parts, body) = response.into_parts();
    let size = axum::body::HttpBody::size_hint(&body).upper();
    if size.is_none_or(|n| n > MAX_TAGGED_BODY) {
        return Response::from_parts(parts, body);
    }
    let Ok(bytes) = axum::body::to_bytes(body, MAX_TAGGED_BODY as usize).await else {
        return ApiError::internal().into_response();
    };
    let tag = format!("W/\"{}\"", &hex::encode(Sha256::digest(&bytes))[..32]);
    let opaque = |t: &str| t.trim().trim_start_matches("W/").to_owned();
    let matched = asked
        .as_ref()
        .and_then(|v| v.to_str().ok())
        .is_some_and(|v| {
            v.split(',')
                .any(|t| t.trim() == "*" || opaque(t) == opaque(&tag))
        });
    parts.headers.insert(
        header::ETAG,
        HeaderValue::from_str(&tag).expect("a hex tag is a header value"),
    );
    if matched {
        parts.status = StatusCode::NOT_MODIFIED;
        parts.headers.remove(header::CONTENT_TYPE);
        parts.headers.remove(header::CONTENT_LENGTH);
        return Response::from_parts(parts, Body::empty());
    }
    Response::from_parts(parts, Body::from(bytes))
}

async fn security_headers(
    request: axum::extract::Request,
    next: axum::middleware::Next,
) -> Response {
    let mut response = next.run(request).await;
    let headers = response.headers_mut();
    headers.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    headers.insert(
        "x-content-type-options",
        HeaderValue::from_static("nosniff"),
    );
    headers.insert("referrer-policy", HeaderValue::from_static("no-referrer"));
    headers.insert(
        "strict-transport-security",
        HeaderValue::from_static("max-age=63072000; includeSubDomains"),
    );
    headers.insert(
        "cross-origin-opener-policy",
        HeaderValue::from_static("same-origin"),
    );
    headers.insert(
        "cross-origin-resource-policy",
        HeaderValue::from_static("same-origin"),
    );
    headers
        .entry(header::HeaderName::from_static("content-security-policy"))
        .or_insert(HeaderValue::from_static(
            "default-src 'none'; frame-ancestors 'none'",
        ));
    headers.insert(
        "permissions-policy",
        HeaderValue::from_static("camera=(), microphone=(), geolocation=()"),
    );
    response
}

fn default_task_limit() -> i32 {
    50
}

fn default_event_limit() -> i32 {
    200
}

impl From<AuthError> for ApiError {
    fn from(error: AuthError) -> Self {
        let status = match error {
            AuthError::KeyServiceUnavailable => StatusCode::SERVICE_UNAVAILABLE,
            _ => StatusCode::UNAUTHORIZED,
        };
        Self {
            status,
            code: error.code(),
            message: if status == StatusCode::SERVICE_UNAVAILABLE {
                "Identity verification is temporarily unavailable"
            } else {
                "Authentication is required"
            },
        }
    }
}

impl From<StoreError> for ApiError {
    fn from(error: StoreError) -> Self {
        match error.kind {
            StoreErrorKind::Invalid => Self::invalid(error.code, "Request is invalid"),
            StoreErrorKind::Unauthorized => Self {
                status: StatusCode::FORBIDDEN,
                code: error.code,
                message: "This verified identity has no active invitation",
            },
            StoreErrorKind::Forbidden => Self {
                status: StatusCode::FORBIDDEN,
                code: error.code,
                message: "The requested group or profile is not assigned to this identity",
            },
            StoreErrorKind::NotFound => Self::not_found(error.code),
            StoreErrorKind::Conflict => Self {
                status: StatusCode::CONFLICT,
                code: error.code,
                message: "The request conflicts with existing state",
            },
            StoreErrorKind::Quota => Self {
                status: StatusCode::TOO_MANY_REQUESTS,
                code: error.code,
                message: "The research group queue is full",
            },
            StoreErrorKind::Unavailable => Self {
                status: StatusCode::SERVICE_UNAVAILABLE,
                code: error.code,
                message: "The broker data store is unavailable",
            },
            StoreErrorKind::LeaseLost | StoreErrorKind::Internal => Self::internal(),
        }
    }
}

impl ApiError {
    /// The stable refusal code, for callers outside the API that reuse its checks.
    pub(crate) fn code(&self) -> &'static str {
        self.code
    }

    fn invalid(code: &'static str, message: &'static str) -> Self {
        Self {
            status: StatusCode::BAD_REQUEST,
            code,
            message,
        }
    }

    fn not_found(code: &'static str) -> Self {
        Self {
            status: StatusCode::NOT_FOUND,
            code,
            message: "The requested resource was not found",
        }
    }

    fn internal() -> Self {
        Self {
            status: StatusCode::INTERNAL_SERVER_ERROR,
            code: "internal_error",
            message: "The request could not be completed",
        }
    }
}

async fn shutdown_signal() {
    let interrupt = async {
        tokio::signal::ctrl_c().await.ok();
    };
    #[cfg(unix)]
    let terminate = async {
        if let Ok(mut signal) =
            tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
        {
            signal.recv().await;
        } else {
            std::future::pending::<()>().await;
        }
    };
    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();
    tokio::select! {
        _ = interrupt => {},
        _ = terminate => {},
    }
}

#[cfg(test)]
mod tests {
    #[tokio::test]
    async fn workspace_serves_only_packaged_assets_and_keeps_api_policy() {
        let _ = rustls::crypto::ring::default_provider().install_default();
        use super::{
            Json, Router, StatusCode, TcpListener, get, json, security_headers, workspace_routes,
        };
        let root = tempfile::tempdir().unwrap();
        std::fs::create_dir(root.path().join("assets")).unwrap();
        std::fs::write(
            root.path().join("index.html"),
            "<title>Research workspace</title>",
        )
        .unwrap();
        std::fs::write(
            root.path().join("assets/app.js"),
            "export const ready=true;",
        )
        .unwrap();
        std::fs::write(root.path().join("private.txt"), "must not be served").unwrap();
        let app: Router = workspace_routes(root.path())
            .route("/api/probe", get(|| async { Json(json!({"ok":true})) }))
            .layer(axum::middleware::from_fn(security_headers));
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let origin = format!("http://{}", listener.local_addr().unwrap());
        let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let client = reqwest::Client::new();
        let page = client.get(format!("{origin}/")).send().await.unwrap();
        assert_eq!(page.status(), StatusCode::OK);
        assert!(
            page.headers()["content-security-policy"]
                .to_str()
                .unwrap()
                .contains("script-src 'self'")
        );
        let asset = client
            .get(format!("{origin}/assets/app.js"))
            .send()
            .await
            .unwrap();
        assert_eq!(asset.status(), StatusCode::OK);
        let api = client
            .get(format!("{origin}/api/probe"))
            .send()
            .await
            .unwrap();
        assert_eq!(
            api.headers()["content-security-policy"],
            "default-src 'none'; frame-ancestors 'none'"
        );
        for path in [
            "/private.txt",
            "/assets/%2e%2e%2fprivate.txt",
            "/api/missing",
        ] {
            let response = client.get(format!("{origin}{path}")).send().await.unwrap();
            assert_eq!(response.status(), StatusCode::NOT_FOUND, "{path}");
        }
        server.abort();
    }
    use std::{fs, io::Read as _, os::unix::fs::symlink};

    use axum::http::{HeaderMap, HeaderValue};
    use sha2::{Digest, Sha256};
    use uuid::Uuid;

    use super::{
        MUTATION_INTENT, SubmitTaskRequest, is_terminal_status, open_verified_artifact,
        request_digest, require_mutation_intent, task_location, validate_key,
    };

    #[test]
    fn task_locations_are_relative_without_a_public_origin() {
        let task = Uuid::nil();
        assert_eq!(
            task_location(None, task).unwrap(),
            HeaderValue::from_static("/api/v1/tasks/00000000-0000-0000-0000-000000000000")
        );
        assert_eq!(
            task_location(
                Some(&"https://broker.example.invalid".parse().unwrap()),
                task
            )
            .unwrap(),
            HeaderValue::from_static(
                "https://broker.example.invalid/api/v1/tasks/00000000-0000-0000-0000-000000000000"
            )
        );
    }

    #[test]
    fn request_digest_has_field_boundaries() {
        let first = SubmitTaskRequest {
            group: "group-a".into(),
            profile: "profile-a".into(),
            objective: "bc".into(),
            project_id: None,
            action_key: None,
            parent_task_id: None,
        };
        let second = SubmitTaskRequest {
            group: "group-a".into(),
            profile: "profile-ab".into(),
            objective: "c".into(),
            project_id: None,
            action_key: None,
            parent_task_id: None,
        };
        assert_ne!(request_digest(&first), request_digest(&second));
    }

    #[test]
    fn public_keys_are_dns_safe() {
        assert!(validate_key("research_group").is_ok());
        assert!(validate_key("../research").is_err());
    }

    #[test]
    fn only_terminal_tasks_close_event_streams() {
        assert!(is_terminal_status("completed"));
        assert!(is_terminal_status("review_required"));
        assert!(!is_terminal_status("running"));
        assert!(!is_terminal_status("cancelling"));
    }

    #[test]
    fn mutations_require_an_explicit_non_simple_request_header() {
        let mut headers = HeaderMap::new();
        assert!(require_mutation_intent(&headers).is_err());
        headers.insert(MUTATION_INTENT, HeaderValue::from_static("read"));
        assert!(require_mutation_intent(&headers).is_err());
        headers.insert(MUTATION_INTENT, HeaderValue::from_static("mutate"));
        assert!(require_mutation_intent(&headers).is_ok());
    }

    /// A log written to memory.
    #[derive(Clone, Default)]
    struct Buffer(std::sync::Arc<std::sync::Mutex<Vec<u8>>>);

    impl std::io::Write for Buffer {
        fn write(&mut self, data: &[u8]) -> std::io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(data);
            Ok(data.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for Buffer {
        type Writer = Buffer;
        fn make_writer(&'a self) -> Buffer {
            self.clone()
        }
    }

    #[test]
    fn the_router_logs_requests_without_their_query_strings() {
        let buffer = Buffer::default();
        let subscriber = tracing_subscriber::fmt()
            .with_max_level(tracing::Level::DEBUG)
            .with_writer(buffer.clone())
            .finish();
        let root = tempfile::tempdir().unwrap();
        // Path parameters that are not ids are refused before any database call, so no database is needed.
        let state = super::test_support::api(
            "host=127.0.0.1 port=9 user=nobody dbname=nothing",
            super::test_support::person("span", "span@example.invalid"),
            root.path(),
        );
        let app = super::router(state);
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let status = tracing::subscriber::with_default(subscriber, || {
            runtime.block_on(async {
                let request = axum::http::Request::get(
                    "/api/v1/tasks/some-task/artifacts/some-artifact/pages?sort=private_column&path=results/secret.csv",
                )
                .body(axum::body::Body::empty())
                .unwrap();
                tower::ServiceExt::oneshot(app, request)
                    .await
                    .unwrap()
                    .status()
            })
        });
        assert_eq!(status, super::StatusCode::BAD_REQUEST);
        let logged = String::from_utf8(buffer.0.lock().unwrap().clone()).unwrap();
        assert!(
            logged.contains("/api/v1/tasks/some-task/artifacts/some-artifact/pages"),
            "{logged}"
        );
        assert!(
            !logged.contains("private_column") && !logged.contains("secret.csv"),
            "{logged}"
        );
    }

    #[test]
    fn request_spans_leave_the_query_string_out() {
        let buffer = Buffer::default();
        let subscriber = tracing_subscriber::fmt()
            .with_max_level(tracing::Level::DEBUG)
            .with_writer(buffer.clone())
            .finish();
        tracing::subscriber::with_default(subscriber, || {
            let request = axum::http::Request::get(
                "/api/v1/tasks/t/artifacts/a/pages?sort=private_column&path=results/secret.csv",
            )
            .body(())
            .unwrap();
            super::request_span(&request)
                .in_scope(|| tracing::debug!("finished processing request"));
        });
        let logged = String::from_utf8(buffer.0.lock().unwrap().clone()).unwrap();
        assert!(
            logged.contains("/api/v1/tasks/t/artifacts/a/pages"),
            "{logged}"
        );
        assert!(
            !logged.contains("private_column") && !logged.contains("secret.csv"),
            "{logged}"
        );
    }

    #[test]
    fn artifact_download_keeps_one_verified_non_symlink_file_open() {
        let directory = tempfile::tempdir().unwrap();
        let artifact = directory.path().join("result.txt");
        let alias = directory.path().join("result-link.txt");
        let expected = b"verified result";
        fs::write(&artifact, expected).unwrap();
        symlink(&artifact, &alias).unwrap();
        let hash = hex::encode(Sha256::digest(expected));

        let mut file = open_verified_artifact(&artifact, expected.len() as u64, &hash).unwrap();
        let mut body = String::new();
        file.read_to_string(&mut body).unwrap();
        assert_eq!(body, "verified result");
        assert!(open_verified_artifact(&alias, expected.len() as u64, &hash).is_err());
        assert!(open_verified_artifact(&artifact, 1, &hash).is_err());
        assert!(open_verified_artifact(&artifact, expected.len() as u64, &"0".repeat(64)).is_err());
    }
}
