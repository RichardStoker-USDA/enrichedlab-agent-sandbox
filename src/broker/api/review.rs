//! Scientist review routes: retained package contents, the project evidence package,
//! collaborator listing, administrator inspection and the research-run deferral record.
use super::*;
use crate::broker::evidence_package::{
    ArtifactSource, PackageError, PackageListing, WriteError, hpc_handoff, list_package,
    normalize_member_path, open_package, plan_package, stream_member, verify_archive,
    verify_artifact_sources, write_package,
};
use crate::broker::package_registry::{
    PackageRevision, build_inventory, disclosure_scope, inventory_sha256,
};
use crate::broker::package_stream::{PackageStream, PackageStreamMessage};
use axum::body::Bytes;
use serde_json::Value;
use tokio_stream::wrappers::ReceiverStream;

const PACKAGE_MEDIA_TYPES: [&str; 4] = [
    "application/gzip",
    "application/x-gzip",
    "application/x-tar+gzip",
    "application/x-compressed-tar",
];

#[derive(Deserialize)]
pub(super) struct AfterQuery {
    #[serde(default)]
    after: i64,
    #[serde(default = "default_event_limit")]
    limit: i32,
}

#[derive(Deserialize)]
pub(super) struct MemberQuery {
    path: String,
}

#[derive(Deserialize)]
pub(super) struct LineageQuery {
    #[serde(default = "default_lineage_version")]
    version: u8,
}
fn default_lineage_version() -> u8 {
    1
}

#[derive(Deserialize)]
pub(super) struct DirectoryQuery {
    #[serde(default = "default_directory_limit")]
    limit: i32,
}
fn default_directory_limit() -> i32 {
    200
}

pub(super) fn package_error(error: PackageError) -> ApiError {
    match error {
        PackageError::NotAPackage => ApiError {
            status: StatusCode::UNSUPPORTED_MEDIA_TYPE,
            code: "artifact_not_a_package",
            message: "This file is not a readable retained package",
        },
        PackageError::TooLarge => ApiError {
            status: StatusCode::PAYLOAD_TOO_LARGE,
            code: "package_too_large_to_inspect",
            message: "This package is too large to read in the browser; download the original",
        },
        PackageError::Integrity => ApiError {
            status: StatusCode::CONFLICT,
            code: "artifact_integrity_failed",
            message: "Artifact integrity check failed",
        },
        PackageError::Io => ApiError::not_found("artifact_not_found"),
    }
}

/// Whether an artifact's recorded media type names a retained tar+gzip package.
pub(super) fn is_package(record: &ArtifactView) -> bool {
    let media = record
        .media_type
        .split(';')
        .next()
        .unwrap_or("")
        .trim()
        .to_ascii_lowercase();
    PACKAGE_MEDIA_TYPES.contains(&media.as_str())
}

pub(super) async fn verified_listing(
    state: &ApiState,
    task: Uuid,
    record: &ArtifactView,
) -> Result<Arc<PackageListing>, ApiError> {
    if !is_package(record) {
        return Err(package_error(PackageError::NotAPackage));
    }
    let cached = || -> Result<Option<Arc<PackageListing>>, ApiError> {
        Ok(state
            .listings
            .lock()
            .map_err(|_| ApiError::internal())?
            .get(&record.sha256))
    };
    if let Some(hit) = cached()? {
        return Ok(hit);
    }
    // Listing hashes the whole package and checks every member's kind, a table's within the table bounds, so it runs
    // under one of the permits that bound the API's heavy reads. A listing made while it waited is taken from the cache,
    // and one made for a request whose client has left is still kept.
    let path = resolve_artifact_path(&state.artifact_root, task, record)?;
    let size = record.size_bytes as u64;
    let sha = record.sha256.clone();
    let listings = state.listings.clone();
    state
        .package_permits
        .run(move || {
            if let Some(hit) = listings.lock().map_err(|_| ApiError::internal())?.get(&sha) {
                return Ok(hit);
            }
            let listing = Arc::new(
                list_package(&path, size, &sha, super::super::kinds::registry().ok())
                    .map_err(package_error)?,
            );
            listings
                .lock()
                .map_err(|_| ApiError::internal())?
                .insert(sha, listing.clone());
            Ok(listing)
        })
        .await?
}

/// The listing with the kinds its writer declared: a figure version or a template analysis names the drawings in its
/// package as R figures.
async fn declared_listing(
    state: &ApiState,
    actor: Uuid,
    task: Uuid,
    record: &ArtifactView,
    listing: Arc<PackageListing>,
) -> Result<Arc<PackageListing>, ApiError> {
    let detail = state.store.task_figure_detail(actor, task).await?;
    let mut declarations = super::super::figure_run::declared_figures(&detail, &record.sha256);
    if declarations.is_empty() {
        let detail = state.store.task_analysis_detail(actor, task).await?;
        declarations = super::super::analysis_run::declared_figures(&detail, &record.sha256);
    }
    let Ok(registry) = super::super::kinds::registry() else {
        return Ok(listing);
    };
    if declarations.is_empty() {
        return Ok(listing);
    }
    let mut declared = (*listing).clone();
    declared.declare(registry, &declarations);
    Ok(Arc::new(declared))
}

pub(super) async fn artifact_contents(
    State(state): State<ApiState>,
    headers: HeaderMap,
    AxumPath((task, artifact)): AxumPath<(Uuid, Uuid)>,
) -> Result<impl IntoResponse, ApiError> {
    let actor = authenticate(&state, &headers).await?;
    let record = state.store.get_artifact(actor, task, artifact).await?;
    let listing = verified_listing(&state, task, &record).await?;
    let listing = declared_listing(&state, actor, task, &record, listing).await?;
    Ok(Json(json!({
        "artifact_id": record.id,
        "sha256": record.sha256,
        "size_bytes": record.size_bytes,
        "listing": &*listing,
    })))
}

pub(super) async fn artifact_member(
    State(state): State<ApiState>,
    headers: HeaderMap,
    AxumPath((task, artifact)): AxumPath<(Uuid, Uuid)>,
    Query(query): Query<MemberQuery>,
) -> Result<Response, ApiError> {
    let actor = authenticate(&state, &headers).await?;
    let record = state.store.get_artifact(actor, task, artifact).await?;
    let listing = verified_listing(&state, task, &record).await?;
    let member_path = normalize_member_path(query.path.as_bytes())
        .ok_or_else(|| ApiError::not_found("package_member_not_found"))?;
    let member = listing
        .member(&member_path)
        .cloned()
        .ok_or_else(|| ApiError::not_found("package_member_not_found"))?;
    let path = resolve_artifact_path(&state.artifact_root, task, &record)?;
    let size = record.size_bytes as u64;
    let range = match table_reads::range_request(&headers, member.size_bytes) {
        table_reads::RangeRequest::Unsatisfiable => {
            return table_reads::range_not_satisfiable(member.size_bytes);
        }
        table_reads::RangeRequest::Part(first, last) => Some((first, last)),
        table_reads::RangeRequest::Whole => None,
    };
    let sha = record.sha256.clone();
    let file = tokio::task::spawn_blocking(move || match range {
        // The client cannot check part of a member against its digest, so the package is checked first, on the
        // descriptor the part is then read from.
        Some(_) => verify_archive(&path, size, &sha),
        None => open_package(&path, size),
    })
    .await
    .map_err(|_| ApiError::internal())?
    .map_err(package_error)?;
    let (sender, receiver) = tokio::sync::mpsc::channel::<Result<Bytes, std::io::Error>>(8);
    let streamed = member.clone();
    tokio::task::spawn_blocking(move || {
        let result = stream_member(file, &streamed, range, |chunk| {
            sender.blocking_send(Ok(Bytes::from(chunk))).is_ok()
        });
        if result.is_err() {
            let _ = sender.blocking_send(Err(std::io::Error::other(
                "package_member_integrity_failed",
            )));
        }
    });
    let mut response = Response::new(Body::from_stream(ReceiverStream::new(receiver)));
    let name = member
        .path
        .rsplit('/')
        .next()
        .unwrap_or("file")
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_') {
                c
            } else {
                '_'
            }
        })
        .collect::<String>();
    let headers = response.headers_mut();
    // Bytes are delivered as an attachment. The client chooses the reader from the
    // verified listing, so a figure or document never renders from this route.
    headers.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("application/octet-stream"),
    );
    headers.insert(
        header::CONTENT_LENGTH,
        HeaderValue::from_str(&member.size_bytes.to_string()).map_err(|_| ApiError::internal())?,
    );
    headers.insert(
        header::CONTENT_DISPOSITION,
        HeaderValue::from_str(&format!("attachment; filename=\"{name}\""))
            .map_err(|_| ApiError::internal())?,
    );
    headers.insert(
        ARTIFACT_SHA256,
        HeaderValue::from_str(&member.sha256).map_err(|_| ApiError::internal())?,
    );
    headers.insert(header::ACCEPT_RANGES, HeaderValue::from_static("bytes"));
    if let Some((first, last)) = range {
        table_reads::mark_partial(&mut response, first, last, member.size_bytes)?;
    }
    Ok(response)
}

/// The stored digest of a readable result package for its current bytes, from which
/// the workspace fills a finding's values. A package without one says `not_recorded`.
pub(super) async fn artifact_digest(
    State(state): State<ApiState>,
    headers: HeaderMap,
    AxumPath((task, artifact)): AxumPath<(Uuid, Uuid)>,
) -> Result<impl IntoResponse, ApiError> {
    let actor = authenticate(&state, &headers).await?;
    Ok(Json(
        state.store.artifact_digest(actor, task, artifact).await?,
    ))
}

/// The admitted block, its template and admission record, for a readable task. A
/// task that ran a fixed method answers `null`; an unreadable task is not found.
pub(super) async fn generated_code(
    State(state): State<ApiState>,
    headers: HeaderMap,
    AxumPath(task): AxumPath<Uuid>,
) -> Result<impl IntoResponse, ApiError> {
    let actor = authenticate(&state, &headers).await?;
    let code = state.store.task_generated_code(actor, task).await?;
    Ok(Json(json!({ "generated_code": code })))
}

/// A readable task's database query: the text the lead proposed, the text that ran, its source and
/// release as a scientist reads them, and each attempt's receipt. A task that ran no query answers `null`.
pub(super) async fn query(
    State(state): State<ApiState>,
    headers: HeaderMap,
    AxumPath(task): AxumPath<Uuid>,
) -> Result<impl IntoResponse, ApiError> {
    let actor = authenticate(&state, &headers).await?;
    let query = state.store.task_query_detail(actor, task).await?;
    Ok(Json(json!({ "query": query })))
}

/// Where a readable task came from: who asked, its inputs, method, code, outputs and
/// review state, from the broker's records, with every part they do not hold named.
/// `?version=2` adds each attempt's sealed inputs, bound execution and registered outputs;
/// `?version=3` follows those recorded inputs to readable upstream tasks
/// and terminal sources.
pub(super) async fn lineage(
    State(state): State<ApiState>,
    headers: HeaderMap,
    AxumPath(task): AxumPath<Uuid>,
    Query(query): Query<LineageQuery>,
) -> Result<impl IntoResponse, ApiError> {
    let actor = authenticate(&state, &headers).await?;
    match query.version {
        1 => Ok(Json(state.store.task_lineage(actor, task).await?)),
        2 => Ok(Json(state.store.task_lineage_v2(actor, task).await?)),
        3 => Ok(Json(state.store.task_lineage_v3(actor, task).await?)),
        _ => Err(ApiError::invalid(
            "lineage_version_invalid",
            "lineage version must be 1, 2 or 3",
        )),
    }
}

pub(super) async fn run_events(
    State(state): State<ApiState>,
    headers: HeaderMap,
    AxumPath(run): AxumPath<Uuid>,
    Query(query): Query<AfterQuery>,
) -> Result<impl IntoResponse, ApiError> {
    let actor = authenticate(&state, &headers).await?;
    Ok(Json(json!({
        "events": state.store.research_run_events(actor, run, query.after, query.limit).await?,
    })))
}

pub(super) async fn members(
    State(state): State<ApiState>,
    headers: HeaderMap,
    AxumPath(project): AxumPath<Uuid>,
) -> Result<impl IntoResponse, ApiError> {
    let actor = authenticate(&state, &headers).await?;
    Ok(Json(state.store.project_members(actor, project).await?))
}

pub(super) async fn admin_projects(
    State(state): State<ApiState>,
    headers: HeaderMap,
    Query(query): Query<DirectoryQuery>,
) -> Result<impl IntoResponse, ApiError> {
    let actor = authenticate(&state, &headers).await?;
    Ok(Json(json!({
        "projects": state.store.admin_project_directory(actor, query.limit).await?,
    })))
}

pub(super) async fn end_inspection(
    State(state): State<ApiState>,
    headers: HeaderMap,
    AxumPath(project): AxumPath<Uuid>,
) -> Result<impl IntoResponse, ApiError> {
    require_mutation_intent(&headers)?;
    let actor = authenticate(&state, &headers).await?;
    Ok(Json(
        json!({"ended": state.store.end_project_inspection(actor, project).await?}),
    ))
}

fn snapshot_error(error: StoreError) -> ApiError {
    if error.code == "project_package_too_large" {
        return ApiError {
            status: StatusCode::PAYLOAD_TOO_LARGE,
            code: "project_package_too_large",
            message: "This project's record exceeds the browser package limit",
        };
    }
    ApiError::from(error)
}

/// Resolve every registered artifact to a safe path. A missing or unsafe file is
/// recorded as missing; it never stops the rest of the record from being packaged.
fn artifact_sources(root: &Path, snapshot: &Value) -> Result<Vec<ArtifactSource>, ApiError> {
    let mut sources = Vec::new();
    for task in snapshot["tasks"]
        .as_array()
        .ok_or_else(ApiError::internal)?
    {
        let task_id: Uuid = task["task_id"]
            .as_str()
            .and_then(|v| v.parse::<Uuid>().ok())
            .ok_or_else(ApiError::internal)?;
        for artifact in task["artifacts"]
            .as_array()
            .ok_or_else(ApiError::internal)?
        {
            let artifact_id: Uuid = artifact["artifact_id"]
                .as_str()
                .and_then(|v| v.parse::<Uuid>().ok())
                .ok_or_else(ApiError::internal)?;
            let relative_path = artifact["relative_path"]
                .as_str()
                .ok_or_else(ApiError::internal)?
                .to_owned();
            let size_bytes = artifact["size_bytes"]
                .as_i64()
                .filter(|v| *v >= 0)
                .ok_or_else(ApiError::internal)?;
            let sha256 = artifact["sha256"]
                .as_str()
                .ok_or_else(ApiError::internal)?
                .to_owned();
            let path = resolve_task_file(root, task_id, &relative_path, size_bytes).ok();
            sources.push(ArtifactSource {
                task_id: task_id.to_string(),
                artifact_id: artifact_id.to_string(),
                relative_path,
                media_type: artifact["media_type"]
                    .as_str()
                    .unwrap_or("application/octet-stream")
                    .to_owned(),
                label: artifact["label"]
                    .as_str()
                    .unwrap_or("Retained file")
                    .to_owned(),
                size_bytes: size_bytes as u64,
                sha256,
                path,
                mismatch: false,
            });
        }
    }
    Ok(sources)
}

pub(super) async fn package_preview(
    State(state): State<ApiState>,
    headers: HeaderMap,
    AxumPath(project): AxumPath<Uuid>,
) -> Result<impl IntoResponse, ApiError> {
    let actor = authenticate(&state, &headers).await?;
    let snapshot = state
        .store
        .project_package_snapshot(actor, project)
        .await
        .map_err(snapshot_error)?;
    let telemetry = crate::broker::feature_events::AttemptGuard::new(
        crate::broker::feature_events::Feature::Package,
        0,
        actor,
        crate::broker::feature_events::PrincipalKind::Person,
        snapshot["project"]["group_key"].as_str().unwrap_or("other"),
        crate::broker::feature_events::TraceContext::from_traceparent(
            headers
                .get("traceparent")
                .and_then(|value| value.to_str().ok()),
        ),
        None,
    );
    let permit = state.package_permits.try_take().ok_or(ApiError {
        status: StatusCode::TOO_MANY_REQUESTS,
        code: "package_builder_busy",
        message: "Evidence packages are being prepared for other requests; try again shortly",
    })?;
    let sources = artifact_sources(&state.artifact_root, &snapshot)?;
    let inventory_snapshot = snapshot.clone();
    let (prepared, permit) = super::permits::Permits::run_held(permit, move || {
        let mut sources = sources;
        verify_artifact_sources(&mut sources);
        let inventory =
            build_inventory(&inventory_snapshot, &sources).map_err(|_| ApiError::internal())?;
        let digest = inventory_sha256(&inventory).map_err(|_| ApiError::internal())?;
        Ok::<_, ApiError>((sources, inventory, digest))
    })
    .await?;
    let (sources, inventory, digest) = prepared?;
    let scope = disclosure_scope(&snapshot).map_err(|_| ApiError::internal())?;
    let record = state
        .store
        .register_project_package(actor, project, scope, &digest, &inventory)
        .await?;
    let revision =
        PackageRevision::from_record(&record, inventory).map_err(|_| ApiError::internal())?;
    let planning = snapshot.clone();
    let (planned, _permit) = super::permits::Permits::run_held(permit, move || {
        plan_package(&planning, sources, &revision).map_err(|_| ApiError::internal())
    })
    .await?;
    let plan = planned?;
    let manifest = &plan.manifest;
    let response = Json(json!({
        "root": plan.root,
        "snapshot_at": plan.snapshot_at,
        "disclosure_scope": manifest["disclosure_scope"],
        "revision": manifest["revision"],
        "content_sha256": manifest["content_sha256"],
        "contents": manifest["contents"],
        "changes_since": manifest["changes_since"],
        "counts": manifest["counts"],
        "coverage": manifest["coverage"],
        "missing": manifest["missing"],
        "files": manifest["files"].as_array().map_or(0, Vec::len),
        "uncompressed_bytes": manifest["files"].as_array().map_or(0, |files| files.iter().filter_map(|f| f["size_bytes"].as_u64()).sum::<u64>()),
        // The same steps the package's rerun/README.md gives, for the dialog to show before the download.
        "hpc_handoff": hpc_handoff(&plan.root),
    }));
    telemetry.finish(
        crate::broker::feature_events::Outcome::Success,
        None,
        1,
        plan.missing.len() as u64,
    );
    Ok(response)
}

/// Writes the package once to measure it, records the export, then streams the same
/// bytes. Two packages build at a time so a large export cannot exhaust the API.
pub(super) async fn package(
    State(state): State<ApiState>,
    headers: HeaderMap,
    AxumPath(project): AxumPath<Uuid>,
) -> Result<Response, ApiError> {
    let actor = authenticate(&state, &headers).await?;
    let snapshot = state
        .store
        .project_package_snapshot(actor, project)
        .await
        .map_err(snapshot_error)?;
    let telemetry = crate::broker::feature_events::AttemptGuard::new(
        crate::broker::feature_events::Feature::Package,
        0,
        actor,
        crate::broker::feature_events::PrincipalKind::Person,
        snapshot["project"]["group_key"].as_str().unwrap_or("other"),
        crate::broker::feature_events::TraceContext::from_traceparent(
            headers
                .get("traceparent")
                .and_then(|value| value.to_str().ok()),
        ),
        None,
    );
    let permit = state.package_permits.try_take().ok_or(ApiError {
        status: StatusCode::TOO_MANY_REQUESTS,
        code: "package_builder_busy",
        message: "Evidence packages are being prepared for other requests; try again shortly",
    })?;
    let sources = artifact_sources(&state.artifact_root, &snapshot)?;
    let inventory_snapshot = snapshot.clone();
    let (prepared, permit) = super::permits::Permits::run_held(permit, move || {
        let mut sources = sources;
        verify_artifact_sources(&mut sources);
        let inventory =
            build_inventory(&inventory_snapshot, &sources).map_err(|_| ApiError::internal())?;
        let digest = inventory_sha256(&inventory).map_err(|_| ApiError::internal())?;
        Ok::<_, ApiError>((sources, inventory, digest))
    })
    .await?;
    let (sources, inventory, digest) = prepared?;
    let scope = disclosure_scope(&snapshot).map_err(|_| ApiError::internal())?;
    let record = state
        .store
        .register_project_package(actor, project, scope, &digest, &inventory)
        .await?;
    let revision =
        PackageRevision::from_record(&record, inventory).map_err(|_| ApiError::internal())?;
    let measuring = snapshot.clone();
    // The permit moves with each blocking step, so a client that leaves while the package is measured does not free it.
    let (measured, permit) = super::permits::Permits::run_held(permit, move || {
        let plan =
            plan_package(&measuring, sources, &revision).map_err(|_| ApiError::internal())?;
        match write_package(&plan, std::io::sink()) {
            Ok(summary) => Ok((plan, summary)),
            // The sources were verified before revision allocation. A change in
            // this narrow window is refused so the registered revision can never
            // describe different bytes.
            Err(WriteError::Artifact(_)) => Err(package_error(PackageError::Integrity)),
            Err(WriteError::Io) => Err(ApiError::internal()),
        }
    })
    .await?;
    let (plan, summary) = measured?;
    state
        .store
        .record_project_export(
            actor,
            project,
            &json!({
                "package_sha256": summary.sha256, "manifest_sha256": plan.manifest_sha256,
                "bytes": summary.bytes, "files": summary.files, "snapshot_at": plan.snapshot_at, "format": "tar+gzip",
            }),
        )
        .await?;
    let (sender, receiver) = tokio::sync::mpsc::channel::<PackageStreamMessage>(8);
    let expected = summary.sha256.clone();
    let streaming = plan.clone();
    tokio::task::spawn_blocking(move || {
        let _permit = permit;
        let writer = ChannelWriter {
            sender: sender.clone(),
            buffer: Vec::with_capacity(CHUNK),
        };
        let terminal = match write_package(&streaming, writer) {
            Ok(result) if result.sha256 == expected => PackageStreamMessage::Complete,
            Ok(_) | Err(WriteError::Artifact(_)) => PackageStreamMessage::Changed,
            Err(WriteError::Io) => PackageStreamMessage::Failed,
        };
        let _ = sender.blocking_send(terminal);
    });
    let refused = plan.missing.len() as u64;
    let stream = PackageStream::new(receiver, move |outcome| {
        let (outcome, error, returned, refused) = outcome.feature_event(refused);
        telemetry.finish(outcome, error, returned, refused);
    });
    let mut response = Response::new(Body::from_stream(stream));
    let headers = response.headers_mut();
    headers.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("application/gzip"),
    );
    headers.insert(
        header::CONTENT_LENGTH,
        HeaderValue::from_str(&summary.bytes.to_string()).map_err(|_| ApiError::internal())?,
    );
    headers.insert(
        header::CONTENT_DISPOSITION,
        HeaderValue::from_str(&format!("attachment; filename=\"{}.tar.gz\"", plan.root))
            .map_err(|_| ApiError::internal())?,
    );
    headers.insert(
        ARTIFACT_SHA256,
        HeaderValue::from_str(&summary.sha256).map_err(|_| ApiError::internal())?,
    );
    headers.insert(
        "x-evidence-manifest-sha256",
        HeaderValue::from_str(&plan.manifest_sha256).map_err(|_| ApiError::internal())?,
    );
    Ok(response)
}

const CHUNK: usize = 64 * 1024;

/// Sends package bytes to the response in bounded chunks. A closed receiver means
/// the client left; the write fails and the builder stops.
struct ChannelWriter {
    sender: tokio::sync::mpsc::Sender<PackageStreamMessage>,
    buffer: Vec<u8>,
}

impl std::io::Write for ChannelWriter {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.buffer.extend_from_slice(buf);
        if self.buffer.len() >= CHUNK {
            self.flush()?;
        }
        Ok(buf.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        if self.buffer.is_empty() {
            return Ok(());
        }
        let chunk = std::mem::replace(&mut self.buffer, Vec::with_capacity(CHUNK));
        self.sender
            .blocking_send(PackageStreamMessage::Chunk(Bytes::from(chunk)))
            .map_err(|_| std::io::Error::new(std::io::ErrorKind::BrokenPipe, "client_disconnected"))
    }
}

/// A reviewer's decision on an analysis as it stands.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct TaskReviewRequest {
    decision: String,
    #[serde(default)]
    note: Option<String>,
    request_id: Uuid,
}

/// Records a person's review of a readable analysis. Only a principal with the project's
/// `review` capability may; the broker binds the analysis's current outputs.
pub(super) async fn record_task_review(
    State(state): State<ApiState>,
    headers: HeaderMap,
    AxumPath(task): AxumPath<Uuid>,
    Json(request): Json<TaskReviewRequest>,
) -> Result<impl IntoResponse, ApiError> {
    require_mutation_intent(&headers)?;
    let actor = authenticate(&state, &headers).await?;
    let review = state
        .store
        .record_task_review(
            actor,
            task,
            &request.decision,
            request.note.as_deref(),
            request.request_id,
        )
        .await?;
    Ok((StatusCode::CREATED, Json(json!({ "review": review }))))
}

/// Every review of a readable analysis, newest first.
pub(super) async fn task_reviews(
    State(state): State<ApiState>,
    headers: HeaderMap,
    AxumPath(task): AxumPath<Uuid>,
) -> Result<impl IntoResponse, ApiError> {
    let actor = authenticate(&state, &headers).await?;
    Ok(Json(
        json!({ "reviews": state.store.task_reviews(actor, task).await? }),
    ))
}

/// The newest review of each analysis in a readable project.
pub(super) async fn project_reviews(
    State(state): State<ApiState>,
    headers: HeaderMap,
    AxumPath(project): AxumPath<Uuid>,
) -> Result<impl IntoResponse, ApiError> {
    let actor = authenticate(&state, &headers).await?;
    Ok(Json(
        json!({ "reviews": state.store.project_reviews(actor, project).await? }),
    ))
}
