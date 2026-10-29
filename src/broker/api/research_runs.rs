use super::*;
use crate::broker::coordinator::StartRun;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct ReviewQuery {
    format: Option<String>,
}

pub(super) async fn literature_review(
    State(s): State<ApiState>,
    headers: HeaderMap,
    AxumPath((run, digest)): AxumPath<(Uuid, String)>,
    Query(query): Query<ReviewQuery>,
) -> Result<Response, ApiError> {
    if digest.len() != 71
        || !digest.starts_with("sha256:")
        || !digest[7..]
            .bytes()
            .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
        || !matches!(query.format.as_deref(), None | Some("json" | "markdown"))
    {
        return Err(ApiError::invalid(
            "context_invalid",
            "Name a checked review and JSON or Markdown format",
        ));
    }
    let actor = authenticate(&s, &headers).await?;
    let record = s.store.literature_review(actor, run, &digest).await?;
    if query.format.as_deref() == Some("markdown") {
        Ok((
            [("content-type", "text/markdown; charset=utf-8")],
            crate::broker::literature_review::markdown(&record["review"]),
        )
            .into_response())
    } else {
        Ok(Json(record).into_response())
    }
}

/// A run's checked orientation, for a project reader who is a member of its lab: the
/// question restated, the data, tools and papers the lead expected to use, what it did not yet know and its profiling
/// steps, as the coordinator kept it before the run's first plan. The view leaves out the record's receipts, queries and
/// expectations.
pub(super) async fn orientation(
    State(s): State<ApiState>,
    headers: HeaderMap,
    AxumPath(run): AxumPath<Uuid>,
) -> Result<Response, ApiError> {
    let actor = authenticate(&s, &headers).await?;
    let record = s.store.research_orientation(actor, run).await?;
    Ok(Json(crate::broker::coordinator::orientation_display(&record)).into_response())
}

pub(super) async fn start(
    State(s): State<ApiState>,
    headers: HeaderMap,
    AxumPath(project): AxumPath<Uuid>,
    Json(request): Json<StartRun>,
) -> Result<Response, ApiError> {
    require_mutation_intent(&headers)?;
    request.validate().map_err(|_| {
        ApiError::invalid(
            "research_run_invalid",
            "Choose authorized tool profiles and bounded run limits",
        )
    })?;
    let actor = authenticate(&s, &headers).await?;
    let run = s.store.start_research_run(actor, project, &request).await?;
    Ok((StatusCode::ACCEPTED, Json(json!({"run_id":run}))).into_response())
}
/// Until when each analysis keeps its working copy, or when it was cleared.
pub(super) async fn working_copies(
    State(s): State<ApiState>,
    headers: HeaderMap,
    AxumPath(project): AxumPath<Uuid>,
) -> Result<impl IntoResponse, ApiError> {
    let actor = authenticate(&s, &headers).await?;
    Ok(Json(
        json!({"working_copies": s.store.project_working_copies(actor, project).await?}),
    ))
}
pub(super) async fn list(
    State(s): State<ApiState>,
    headers: HeaderMap,
    AxumPath(project): AxumPath<Uuid>,
) -> Result<impl IntoResponse, ApiError> {
    let actor = authenticate(&s, &headers).await?;
    Ok(Json(
        json!({"runs":s.store.research_runs(actor,project).await?}),
    ))
}
pub(super) async fn pause(
    State(s): State<ApiState>,
    headers: HeaderMap,
    AxumPath(run): AxumPath<Uuid>,
) -> Result<impl IntoResponse, ApiError> {
    require_mutation_intent(&headers)?;
    let actor = authenticate(&s, &headers).await?;
    s.store.pause_research_run(actor, run).await?;
    Ok(StatusCode::NO_CONTENT)
}
#[derive(Deserialize)]
pub(super) struct TurnsQuery {
    #[serde(default)]
    after: i32,
    #[serde(default = "default_task_limit")]
    limit: i32,
}
pub(super) async fn turns(
    State(s): State<ApiState>,
    headers: HeaderMap,
    AxumPath(run): AxumPath<Uuid>,
    Query(q): Query<TurnsQuery>,
) -> Result<impl IntoResponse, ApiError> {
    let actor = authenticate(&s, &headers).await?;
    Ok(Json(
        json!({"turns":s.store.research_turns(actor,run,q.after,q.limit).await?}),
    ))
}
/// The exact text a decision's models were given, for a project reader who
/// is a member of its lab. It is shown in the workspace and never offered as a download.
pub(super) async fn context(
    State(s): State<ApiState>,
    headers: HeaderMap,
    AxumPath((run, sha256)): AxumPath<(Uuid, String)>,
) -> Result<impl IntoResponse, ApiError> {
    if sha256.len() != 71
        || !sha256.starts_with("sha256:")
        || !sha256[7..]
            .bytes()
            .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
    {
        return Err(ApiError::invalid(
            "context_invalid",
            "Name a kept text by its SHA-256",
        ));
    }
    let actor = authenticate(&s, &headers).await?;
    Ok(Json(s.store.research_context(actor, run, &sha256).await?))
}
