use super::*;
use crate::broker::project::{
    CheckpointRequest, CreateProject, Inspection, ProjectGrant, ResearchState,
};
use crate::broker::workspace_catalog::{CombineRefusal, combine, initial_state};

#[derive(Deserialize)]
pub(super) struct HistoryQuery {
    /// Revisions before this one, newest first; none or 0 starts from the newest.
    before: Option<i64>,
    /// Read forward instead: revisions after this one, oldest first (migration 054).
    after: Option<i64>,
    #[serde(default = "default_task_limit")]
    limit: i32,
}
/// A page of a project's analyses past the newest 200 the project read holds (migration 054).
#[derive(Deserialize)]
pub(super) struct AnalysesQuery {
    cursor: Option<String>,
    #[serde(default = "default_analyses_limit")]
    limit: i32,
}
fn default_analyses_limit() -> i32 {
    200
}
/// The revision a person last looked at (migration 054).
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct LookRequest {
    revision: i64,
}
#[derive(Deserialize)]
pub(super) struct ContextQuery {
    #[serde(default = "default_context_bytes")]
    max_bytes: usize,
}
fn default_context_bytes() -> usize {
    24 * 1024
}

/// One starting point as `workspace_key`, or several as `workspace_keys`, never both.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct PrepareRequest {
    #[serde(default)]
    workspace_key: Option<String>,
    #[serde(default)]
    workspace_keys: Option<Vec<String>>,
    goal: String,
    constraints: Vec<String>,
    request_id: Uuid,
    expected_revision: i64,
}

pub(super) async fn workspaces(
    State(state): State<ApiState>,
    headers: HeaderMap,
    AxumPath(project): AxumPath<Uuid>,
) -> Result<impl IntoResponse, ApiError> {
    let actor = authenticate(&state, &headers).await?;
    let project = state.store.get_project(actor, project).await?;
    let identity = state.store.me(actor).await?;
    let entries = state
        .workspace_catalog
        .available(project["group_key"].as_str().unwrap_or(""), &identity);
    Ok(Json(
        json!({"workspaces": entries.iter().map(|e| e.choice()).collect::<Vec<_>>(),
            "combine": state.combine_starting_points}),
    ))
}

/// A lab's starting points for its welcome view, before any project exists: the same entries a new project in the lab
/// would offer the reader. A lab the reader does not belong to is not found.
pub(super) async fn lab_workspaces(
    State(state): State<ApiState>,
    headers: HeaderMap,
    AxumPath(lab): AxumPath<String>,
) -> Result<impl IntoResponse, ApiError> {
    let actor = authenticate(&state, &headers).await?;
    let identity = state.store.me(actor).await?;
    if !identity.groups.iter().any(|g| g.key == lab) {
        return Err(ApiError::not_found("lab_not_found"));
    }
    let entries = state.workspace_catalog.available(&lab, &identity);
    Ok(Json(
        json!({"lab": lab, "workspaces": entries.iter().map(|e| e.choice()).collect::<Vec<_>>(),
            "combine": state.combine_starting_points}),
    ))
}

pub(super) async fn prepare(
    State(state): State<ApiState>,
    headers: HeaderMap,
    AxumPath(project): AxumPath<Uuid>,
    Json(request): Json<PrepareRequest>,
) -> Result<Response, ApiError> {
    require_mutation_intent(&headers)?;
    if request.expected_revision != 0 {
        return Err(ApiError::invalid(
            "workspace_already_started",
            "Only a new project can select its initial workspace",
        ));
    }
    let actor = authenticate(&state, &headers).await?;
    let saved = state.store.get_project(actor, project).await?;
    let identity = state.store.me(actor).await?;
    let entries = state
        .workspace_catalog
        .available(saved["group_key"].as_str().unwrap_or(""), &identity);
    let keys = match (request.workspace_key, request.workspace_keys) {
        (Some(key), None) => vec![key],
        // Off until the research coordinator reads several starting points; an older one refuses the saved version.
        (None, Some(_)) if !state.combine_starting_points => {
            return Err(ApiError::invalid(
                "workspace_combination_unavailable",
                "Starting from several sets is not available yet",
            ));
        }
        (None, Some(keys)) if keys.len() >= 2 => keys,
        _ => {
            return Err(ApiError::invalid(
                "workspace_selection_invalid",
                "Choose one starting point, or two or more",
            ));
        }
    };
    let chosen = keys
        .iter()
        .map(|key| entries.iter().copied().find(|e| &e.key == key))
        .collect::<Option<Vec<_>>>()
        .ok_or_else(|| ApiError::not_found("workspace_unavailable"))?;
    let (workspace, allowed_profiles) = match chosen.as_slice() {
        [entry] => (entry.workspace.clone(), entry.allowed_profiles.clone()),
        _ => match combine(&chosen) {
            Ok(combined) => combined,
            // The refusal names the sets, so the person sees which ones cannot go together.
            Err(CombineRefusal::Unsupported(keys)) => {
                return Ok((
                    StatusCode::BAD_REQUEST,
                    Json(json!({"error": {"code": "workspace_combination_unsupported",
                        "message": "These starting points need different workspaces and cannot be combined",
                        "keys": keys}})),
                )
                    .into_response());
            }
            Err(CombineRefusal::TooLarge) => {
                return Err(ApiError::invalid(
                    "workspace_combination_too_large",
                    "These starting points hold too much to combine",
                ));
            }
            Err(CombineRefusal::Invalid) => {
                return Err(ApiError::invalid(
                    "workspace_selection_invalid",
                    "Choose one starting point, or two or more",
                ));
            }
        },
    };
    let initial = initial_state(workspace, request.goal, request.constraints);
    initial
        .validate()
        .map_err(|code| ApiError::invalid(code, "Research question or constraints are invalid"))?;
    // The existing locked checkpoint transaction enforces steer permission,
    // initial revision zero and request-id replay. Selection never provisions data.
    let revision = state
        .store
        .checkpoint_project(
            actor,
            project,
            &CheckpointRequest {
                expected_revision: 0,
                request_id: request.request_id,
                state: initial,
            },
        )
        .await?;
    Ok(Json(json!({"revision": revision, "allowed_profiles": allowed_profiles})).into_response())
}

pub(super) async fn create(
    State(state): State<ApiState>,
    headers: HeaderMap,
    Json(request): Json<CreateProject>,
) -> Result<Response, ApiError> {
    require_mutation_intent(&headers)?;
    validate_key(&request.group)?;
    if request.title.trim().is_empty() || request.title.len() > 160 || request.title.contains('\0')
    {
        return Err(ApiError::invalid(
            "project_title_invalid",
            "Project title must contain 1-160 bytes",
        ));
    }
    let actor = authenticate(&state, &headers).await?;
    let id = state.store.create_project(actor, &request).await?;
    Ok((StatusCode::CREATED, Json(json!({"project_id":id}))).into_response())
}
pub(super) async fn list(
    State(state): State<ApiState>,
    headers: HeaderMap,
    Query(query): Query<ListQuery>,
) -> Result<impl IntoResponse, ApiError> {
    let actor = authenticate(&state, &headers).await?;
    Ok(Json(
        json!({"projects":state.store.list_projects(actor,query.limit).await?}),
    ))
}
pub(super) async fn get(
    State(state): State<ApiState>,
    headers: HeaderMap,
    AxumPath(project): AxumPath<Uuid>,
) -> Result<impl IntoResponse, ApiError> {
    let actor = authenticate(&state, &headers).await?;
    Ok(Json(state.store.read_project(actor, project).await?))
}
/// A person marks the revision they have seen. It moves forward only, so a stale page cannot move it back.
pub(super) async fn look(
    State(state): State<ApiState>,
    headers: HeaderMap,
    AxumPath(project): AxumPath<Uuid>,
    Json(request): Json<LookRequest>,
) -> Result<impl IntoResponse, ApiError> {
    require_mutation_intent(&headers)?;
    let actor = authenticate(&state, &headers).await?;
    Ok(Json(
        state
            .store
            .project_look(actor, project, request.revision)
            .await?,
    ))
}
/// The project's analyses past the newest 200 the project read holds, newest first, one page per cursor.
pub(super) async fn analyses(
    State(state): State<ApiState>,
    headers: HeaderMap,
    AxumPath(project): AxumPath<Uuid>,
    Query(query): Query<AnalysesQuery>,
) -> Result<impl IntoResponse, ApiError> {
    if query
        .cursor
        .as_ref()
        .is_some_and(|c| c.is_empty() || c.len() > 128)
    {
        return Err(ApiError::invalid(
            "analyses_cursor_invalid",
            "The cursor is not one this server returned",
        ));
    }
    let actor = authenticate(&state, &headers).await?;
    Ok(Json(
        state
            .store
            .project_analyses(actor, project, query.cursor.as_deref(), query.limit)
            .await?,
    ))
}
pub(super) async fn grant(
    State(state): State<ApiState>,
    headers: HeaderMap,
    AxumPath(project): AxumPath<Uuid>,
    Json(request): Json<ProjectGrant>,
) -> Result<impl IntoResponse, ApiError> {
    require_mutation_intent(&headers)?;
    if request.capabilities.len() > 5 {
        return Err(ApiError::invalid(
            "project_grant_invalid",
            "At most five capabilities are allowed",
        ));
    }
    let actor = authenticate(&state, &headers).await?;
    state.store.grant_project(actor, project, &request).await?;
    Ok(StatusCode::NO_CONTENT)
}
pub(super) async fn inspect(
    State(state): State<ApiState>,
    headers: HeaderMap,
    AxumPath(project): AxumPath<Uuid>,
    Json(request): Json<Inspection>,
) -> Result<impl IntoResponse, ApiError> {
    require_mutation_intent(&headers)?;
    let actor = authenticate(&state, &headers).await?;
    let expires = state
        .store
        .inspect_project(actor, project, &request.reason)
        .await?;
    Ok(Json(json!({"expires_at":expires,"capabilities":["read"]})))
}
pub(super) async fn checkpoint(
    State(state): State<ApiState>,
    headers: HeaderMap,
    AxumPath(project): AxumPath<Uuid>,
    Json(request): Json<CheckpointRequest>,
) -> Result<impl IntoResponse, ApiError> {
    require_mutation_intent(&headers)?;
    request
        .state
        .validate()
        .map_err(|code| ApiError::invalid(code, "Research state is invalid"))?;
    // The same gate as prepare's: an older research coordinator refuses a saved version that names starting points.
    if !state.combine_starting_points && !request.state.workspace.starting_points.is_empty() {
        return Err(ApiError::invalid(
            "workspace_combination_unavailable",
            "Starting from several sets is not available yet",
        ));
    }
    if request.expected_revision < 0 {
        return Err(ApiError::invalid(
            "checkpoint_invalid",
            "Revision must be nonnegative",
        ));
    }
    let actor = authenticate(&state, &headers).await?;
    let revision = state
        .store
        .checkpoint_project(actor, project, &request)
        .await?;
    Ok(Json(json!({"revision":revision})))
}
pub(super) async fn history(
    State(state): State<ApiState>,
    headers: HeaderMap,
    AxumPath(project): AxumPath<Uuid>,
    Query(query): Query<HistoryQuery>,
) -> Result<impl IntoResponse, ApiError> {
    if query.after.is_some() && query.before.is_some() {
        return Err(ApiError::invalid(
            "history_direction_invalid",
            "Read history either before or after a revision",
        ));
    }
    let actor = authenticate(&state, &headers).await?;
    if let Some(after) = query.after {
        return Ok(Json(
            state
                .store
                .project_history_after(actor, project, after, query.limit)
                .await?,
        ));
    }
    Ok(Json(
        json!({"checkpoints":state.store.project_history(actor,project,query.before.unwrap_or(0),query.limit).await?}),
    ))
}
pub(super) async fn audit(
    State(state): State<ApiState>,
    headers: HeaderMap,
    AxumPath(project): AxumPath<Uuid>,
    Query(query): Query<ListQuery>,
) -> Result<impl IntoResponse, ApiError> {
    let actor = authenticate(&state, &headers).await?;
    Ok(Json(
        json!({"events":state.store.project_audit(actor,project,query.limit).await?}),
    ))
}
/// A person marks one of their waiting items as seen.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct AcknowledgeRequest {
    kind: String,
    #[serde(rename = "ref")]
    reference: String,
    version: String,
    request_id: Uuid,
}
/// What waits for the caller across their labs.
pub(super) async fn inbox(
    State(state): State<ApiState>,
    headers: HeaderMap,
) -> Result<impl IntoResponse, ApiError> {
    let actor = authenticate(&state, &headers).await?;
    Ok(Json(state.store.inbox(actor).await?))
}
pub(super) async fn acknowledge(
    State(state): State<ApiState>,
    headers: HeaderMap,
    Json(request): Json<AcknowledgeRequest>,
) -> Result<impl IntoResponse, ApiError> {
    require_mutation_intent(&headers)?;
    if [&request.kind, &request.reference, &request.version]
        .iter()
        .any(|v| v.is_empty() || v.len() > 200)
    {
        return Err(ApiError::invalid(
            "inbox_item_invalid",
            "The item is not recognized",
        ));
    }
    let actor = authenticate(&state, &headers).await?;
    Ok(Json(
        state
            .store
            .inbox_acknowledge(
                actor,
                &request.kind,
                &request.reference,
                &request.version,
                request.request_id,
            )
            .await?,
    ))
}
/// Who saw each of a project's review requests.
pub(super) async fn acknowledgements(
    State(state): State<ApiState>,
    headers: HeaderMap,
    AxumPath(project): AxumPath<Uuid>,
) -> Result<impl IntoResponse, ApiError> {
    let actor = authenticate(&state, &headers).await?;
    Ok(Json(
        json!({"acknowledgements": state.store.project_acknowledgements(actor, project).await?}),
    ))
}
/// The plan ledger for anyone who may read the project.
pub(super) async fn plan_ledger(
    State(state): State<ApiState>,
    headers: HeaderMap,
    AxumPath(project): AxumPath<Uuid>,
) -> Result<impl IntoResponse, ApiError> {
    let actor = authenticate(&state, &headers).await?;
    Ok(Json(state.store.project_plan_ledger(actor, project).await?))
}
pub(super) async fn context(
    State(state): State<ApiState>,
    headers: HeaderMap,
    AxumPath(project): AxumPath<Uuid>,
    Query(query): Query<ContextQuery>,
) -> Result<impl IntoResponse, ApiError> {
    let actor = authenticate(&state, &headers).await?;
    let record = state.store.get_project(actor, project).await?;
    let checkpoint = &record["checkpoint"];
    if checkpoint.is_null() {
        return Err(ApiError::not_found("checkpoint_not_found"));
    }
    let research: ResearchState =
        serde_json::from_value(checkpoint["state"].clone()).map_err(|_| ApiError::internal())?;
    let mut context = research
        .context(
            checkpoint["revision"]
                .as_i64()
                .ok_or_else(ApiError::internal)?,
            query.max_bytes,
        )
        .map_err(|code| {
            ApiError::invalid(
                code,
                "Context budget cannot preserve required research state",
            )
        })?;
    // Task facts are separately queryable and never overwritten by model state.
    context["project_id"] = json!(project);
    if serde_json::to_vec(&context)
        .map_err(|_| ApiError::internal())?
        .len()
        > query.max_bytes
    {
        return Err(ApiError::invalid(
            "context_budget_too_small",
            "Context budget cannot preserve required research state",
        ));
    }
    Ok(Json(context))
}

/// While work runs the stream reads the project again every two seconds, and every ten while it is idle. One connection
/// lasts at most fifteen minutes; the client then reconnects, so a long-lived page never holds a stale authorization.
const PULSE_BUSY: Duration = Duration::from_secs(2);
const PULSE_IDLE: Duration = Duration::from_secs(10);
const STREAM_LIFETIME: Duration = Duration::from_secs(15 * 60);
const ACTIVE_TASKS: [&str; 5] = ["queued", "leased", "starting", "running", "cancelling"];
const ACTIVE_RUNS: [&str; 3] = ["queued", "running", "waiting"];

/// A short fingerprint of what a project page shows: its saved version, title, sharing and the reader's access, each
/// analysis's status and attempt, and each work session's status, decision count, deferral code and retry count. It
/// leaves out the timestamps that move without a visible change: a running analysis's lease renewal, and a work session
/// that is claimed and handed back every few seconds while an analysis works. While any analysis is active a work
/// session's running and waiting read as one state, as they do on the page. The fingerprint carries no research data: a
/// page that sees it change reads the project again through the ordinary endpoints, which check access as always. The
/// second value says whether any analysis or work session is still active.
pub(super) fn project_pulse(
    project: &serde_json::Value,
    runs: &[serde_json::Value],
) -> (String, bool) {
    let text = |value: &serde_json::Value, key: &str| {
        value
            .get(key)
            .map_or_else(|| "null".to_owned(), |v| v.to_string())
    };
    let mut capabilities: Vec<String> = project["capabilities"]
        .as_array()
        .map(|all| all.iter().map(|c| c.to_string()).collect())
        .unwrap_or_default();
    capabilities.sort();
    let mut lines = vec![format!(
        "p|{}|{}|{}|{}|{}|{}|{}",
        text(project, "revision"),
        text(project, "updated_at"),
        text(project, "title"),
        text(project, "visibility"),
        capabilities.join(","),
        text(project, "inspection_expires_at"),
        text(project, "task_count"),
    )];
    let empty = Vec::new();
    let tasks = project["tasks"].as_array().unwrap_or(&empty);
    let analysis_active = tasks.iter().any(|t| {
        t["status"]
            .as_str()
            .is_some_and(|s| ACTIVE_TASKS.contains(&s))
    });
    let mut task_lines: Vec<String> = tasks
        .iter()
        .map(|t| {
            format!(
                "t|{}|{}|{}",
                text(t, "task_id"),
                text(t, "status"),
                text(t, "attempt")
            )
        })
        .collect();
    task_lines.sort();
    let mut run_lines: Vec<String> = runs
        .iter()
        .map(|r| {
            let status = match r["status"].as_str() {
                Some("running" | "waiting") if analysis_active => "\"active\"".to_owned(),
                _ => text(r, "status"),
            };
            format!(
                "r|{}|{}|{}|{}|{}",
                text(r, "run_id"),
                status,
                text(r, "turn"),
                text(r, "code"),
                text(r, "failures")
            )
        })
        .collect();
    run_lines.sort();
    lines.extend(task_lines);
    lines.extend(run_lines);
    let digest = hex::encode(Sha256::digest(lines.join("\n").as_bytes()));
    let busy = analysis_active
        || runs.iter().any(|r| {
            r["status"]
                .as_str()
                .is_some_and(|s| ACTIVE_RUNS.contains(&s))
        });
    (digest[..32].to_owned(), busy)
}

async fn read_pulse(
    state: &ApiState,
    actor: Uuid,
    project: Uuid,
) -> Result<(String, bool), StoreError> {
    let detail = state.store.get_project(actor, project).await?;
    let runs = state.store.research_runs(actor, project).await?;
    Ok(project_pulse(&detail, &runs))
}

fn pulse_event(name: &'static str, pulse: &str, busy: bool) -> Event {
    Event::default()
        .event(name)
        .data(json!({ "pulse": pulse, "busy": busy }).to_string())
}

/// `GET /api/v1/projects/{project}/events/stream`: `project_state` once on connect, then `project_changed` each time the
/// fingerprint changes. `stream_complete` with reason `renew` ends a connection at its lifetime, and `stream_closed`
/// ends one whose project can no longer be read; the client reads the project again and reconnects.
pub(super) async fn stream(
    State(state): State<ApiState>,
    headers: HeaderMap,
    AxumPath(project): AxumPath<Uuid>,
) -> Result<impl IntoResponse, ApiError> {
    let actor = authenticate(&state, &headers).await?;
    let (pulse, busy) = read_pulse(&state, actor, project).await?;
    struct Cursor {
        state: ApiState,
        actor: Uuid,
        project: Uuid,
        pulse: String,
        busy: bool,
        opened: std::time::Instant,
        announced: bool,
        done: bool,
    }
    let cursor = Cursor {
        state,
        actor,
        project,
        pulse,
        busy,
        opened: std::time::Instant::now(),
        announced: false,
        done: false,
    };
    let events = stream::unfold(cursor, |mut cursor| async move {
        if cursor.done {
            return None;
        }
        if !cursor.announced {
            cursor.announced = true;
            let event = pulse_event("project_state", &cursor.pulse, cursor.busy);
            return Some((Ok::<Event, Infallible>(event), cursor));
        }
        loop {
            if cursor.opened.elapsed() >= STREAM_LIFETIME {
                cursor.done = true;
                let event = Event::default()
                    .event("stream_complete")
                    .data("{\"reason\":\"renew\"}");
                return Some((Ok(event), cursor));
            }
            sleep(if cursor.busy { PULSE_BUSY } else { PULSE_IDLE }).await;
            match read_pulse(&cursor.state, cursor.actor, cursor.project).await {
                Ok((pulse, busy)) if pulse != cursor.pulse => {
                    cursor.pulse = pulse;
                    cursor.busy = busy;
                    let event = pulse_event("project_changed", &cursor.pulse, busy);
                    return Some((Ok(event), cursor));
                }
                Ok((_, busy)) => cursor.busy = busy,
                Err(_) => {
                    cursor.done = true;
                    let event = Event::default()
                        .event("stream_closed")
                        .data("{\"code\":\"project_unavailable\"}");
                    return Some((Ok(event), cursor));
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::broker::{
        auth::{AuthError, AuthVerifier},
        knowledge_person::ASSERTION_HEADER,
        model::{PrincipalClass, VerifiedIdentity},
        project::Visibility,
    };
    use async_trait::async_trait;
    use serde_json::json;

    fn project(tasks: serde_json::Value) -> serde_json::Value {
        json!({ "project_id": "p", "revision": 4, "updated_at": "2026-10-12T01:00:00Z", "title": "Diet study",
            "visibility": "lab", "capabilities": ["read", "steer"], "inspection_expires_at": null, "task_count": 2,
            "checkpoint": { "revision": 4, "state": { "goal": "A question" } }, "tasks": tasks })
    }
    fn task(id: &str, status: &str, at: &str) -> serde_json::Value {
        json!({ "task_id": id, "status": status, "attempt": 1, "updated_at": at, "profile": "r" })
    }
    fn run(status: &str, turn: i64, at: &str) -> serde_json::Value {
        json!({ "run_id": "r1", "status": status, "turn": turn, "updated_at": at, "code": null, "failures": 0 })
    }

    #[test]
    fn the_pulse_is_stable_for_the_same_record_whatever_the_order() {
        let a = project(json!([
            task("t1", "completed", "1"),
            task("t2", "running", "2")
        ]));
        let b = project(json!([
            task("t2", "running", "2"),
            task("t1", "completed", "1")
        ]));
        let runs = [run("running", 3, "5")];
        assert_eq!(project_pulse(&a, &runs), project_pulse(&b, &runs));
        assert_eq!(project_pulse(&a, &runs).0.len(), 32);
    }

    #[test]
    fn an_analysis_or_a_decision_changes_the_pulse_and_says_whether_work_runs() {
        let running = project(json!([task("t1", "running", "2")]));
        let done = project(json!([task("t1", "completed", "3")]));
        let (before, busy) = project_pulse(&running, &[run("waiting", 3, "5")]);
        assert!(busy);
        let (after, still) = project_pulse(&done, &[run("waiting", 3, "5")]);
        assert_ne!(before, after);
        assert!(still, "a waiting work session is still active");
        let (next, _) = project_pulse(&done, &[run("waiting", 4, "6")]);
        assert_ne!(after, next, "a new decision changes the pulse");
        let (idle, busy) = project_pulse(&done, &[run("review_required", 4, "7")]);
        assert_ne!(next, idle);
        assert!(!busy, "a stop for review is not work in progress");
    }

    #[test]
    fn a_lease_renewal_and_a_work_session_polling_its_analysis_do_not_change_the_pulse() {
        let working = project(json!([task("t1", "running", "1")]));
        let renewed = project(json!([task("t1", "running", "2")]));
        let (pulse, busy) = project_pulse(&working, &[run("waiting", 3, "5")]);
        assert!(busy);
        assert_eq!(pulse, project_pulse(&renewed, &[run("waiting", 3, "5")]).0);
        // While the analysis works, the session is claimed, finds it unfinished and is handed back every few seconds.
        assert_eq!(pulse, project_pulse(&renewed, &[run("running", 3, "6")]).0);
        assert_eq!(pulse, project_pulse(&renewed, &[run("waiting", 3, "7")]).0);
        // Once nothing is running, the agent choosing its next step is a change the page shows.
        let finished = project(json!([task("t1", "completed", "8")]));
        let (waiting, _) = project_pulse(&finished, &[run("waiting", 3, "8")]);
        let (choosing, busy) = project_pulse(&finished, &[run("running", 3, "9")]);
        assert_ne!(waiting, choosing);
        assert!(busy);
        let mut retried = task("t1", "running", "1");
        retried["attempt"] = json!(2);
        assert_ne!(
            pulse,
            project_pulse(&project(json!([retried])), &[run("waiting", 3, "5")]).0
        );
    }

    #[test]
    fn access_and_sharing_change_the_pulse_and_the_saved_plan_text_does_not() {
        let base = project(json!([]));
        let mut shared = base.clone();
        shared["visibility"] = json!("private");
        let mut reduced = base.clone();
        reduced["capabilities"] = json!(["read"]);
        let mut rewritten = base.clone();
        rewritten["checkpoint"]["state"]["goal"] = json!("Other words with the same saved version");
        let pulse = |p: &serde_json::Value| project_pulse(p, &[]).0;
        assert_ne!(pulse(&base), pulse(&shared));
        assert_ne!(pulse(&base), pulse(&reduced));
        assert_eq!(
            pulse(&base),
            pulse(&rewritten),
            "a saved version changes the revision, which the pulse reads"
        );
    }

    struct Signed(VerifiedIdentity);

    #[async_trait]
    impl AuthVerifier for Signed {
        async fn verify(&self, _headers: &HeaderMap) -> Result<VerifiedIdentity, AuthError> {
            Ok(self.0.clone())
        }
    }

    fn person(subject: &str, email: &str) -> VerifiedIdentity {
        VerifiedIdentity {
            issuer: "https://identity.example.invalid".into(),
            subject: subject.into(),
            email: email.into(),
            display_name: None,
            class: PrincipalClass::Person,
        }
    }

    fn api(dsn: &str, identity: VerifiedIdentity) -> ApiState {
        ApiState {
            workspace_ui: false,
            site: crate::broker::model::SiteView { organization: None },
            workspace_catalog: Arc::default(),
            store: Store::connect(dsn, "research-site".into(), 2).unwrap(),
            verifier: Arc::new(Signed(identity)),
            artifact_root: PathBuf::from("/nonexistent"),
            public_url: None,
            listings: Arc::default(),
            tables: Arc::default(),
            package_permits: crate::broker::api::permits::Permits::new(1),
            body_permits: crate::broker::api::permits::Permits::new(4),
            output_streams: Arc::default(),
            library: None,
            library_permits: Arc::new(tokio::sync::Semaphore::new(1)),
            knowledge_person: None,
            dataset_guard: None,
            changes: None,
            combine_starting_points: false,
        }
    }

    async fn serve(state: ApiState) -> String {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let origin = format!("http://{}", listener.local_addr().unwrap());
        let app = router(state);
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        origin
    }

    /// The next named event on a server-sent stream, skipping keepalive comments.
    async fn next_event(
        response: &mut reqwest::Response,
        buffer: &mut String,
    ) -> (String, serde_json::Value) {
        loop {
            if let Some(end) = buffer.find("\n\n") {
                let frame: String = buffer.drain(..end + 2).collect();
                let field = |name: &str| {
                    frame
                        .lines()
                        .find_map(|line| line.strip_prefix(name))
                        .map(|v| v.trim_start().to_owned())
                };
                if let (Some(event), Some(data)) = (field("event:"), field("data:")) {
                    return (event, serde_json::from_str(&data).unwrap());
                }
                continue;
            }
            let chunk = response.chunk().await.unwrap().expect("the stream ended");
            buffer.push_str(std::str::from_utf8(&chunk).unwrap());
        }
    }

    fn research() -> ResearchState {
        serde_json::from_value(json!({
            "schema_version": 1, "goal": "Estimate a treatment contrast using the retained public input.",
            "constraints": ["Treat the animal as the experimental unit."],
            "plan": [{"action_key": "inspect-input", "title": "Inspect source and units", "status": "uncertain",
                "task_id": null, "evidence_refs": [], "rationale": "Check the task receipt first."}],
            "lessons": [], "next_action": "Inspect the input.",
            "workspace": {"schema_version": 1, "profile": "reviewed-r",
                "image_digest": format!("sha256:{}", "a".repeat(64)), "source_digest": format!("sha256:{}", "b".repeat(64)),
                "input_refs": ["input://public/example"], "tool_refs": ["tool://reviewed/contrast"],
                "preparation_receipt": null}
        }))
        .unwrap()
    }

    /// A lab whose catalog holds two sets sharing a workspace image and one that does not.
    fn starting_points() -> std::sync::Arc<crate::broker::workspace_catalog::WorkspaceCatalog> {
        let entry = |key: &str, image: &str, input: &str| {
            json!({"key": key, "name": format!("Set {key}"), "description": format!("What {key} holds."),
                "group": "pilot-lab", "principals": null, "allowed_profiles": ["general-research"],
                "workspace": {"schema_version": 1, "profile": "general-research",
                    "image_digest": format!("sha256:{}", image.repeat(64)), "source_digest": format!("sha256:{}", "b".repeat(64)),
                    "input_refs": [input], "tool_refs": ["tool://reviewed/query"], "preparation_receipt": null}})
        };
        let catalog = json!({"schema_version": 1, "entries": [entry("foods", "a", "input://reference/foods"),
            entry("survey", "a", "input://reference/survey"), entry("figures", "c", "input://reference/figures")]});
        std::sync::Arc::new(
            crate::broker::workspace_catalog::WorkspaceCatalog::parse(
                &serde_json::to_vec(&catalog).unwrap(),
            )
            .unwrap(),
        )
    }

    #[tokio::test]
    #[ignore = "isolated PostgreSQL through qualify_remote_broker.sh change-feed"]
    async fn a_project_starts_from_several_sets_that_share_a_workspace() {
        let _ = rustls::crypto::ring::default_provider().install_default();
        let dsn = std::fs::read_to_string(
            std::env::var_os("BROKER_QUALIFICATION_MIGRATION_DSN_FILE").unwrap(),
        )
        .unwrap();
        let config: tokio_postgres::Config = dsn.parse().unwrap();
        let (owner, connection) = config.connect(tokio_postgres::NoTls).await.unwrap();
        tokio::spawn(connection);
        owner
            .batch_execute(
                "INSERT INTO enrichedlab_agent.broker_principal_invitations(tenant_id,email,group_key,role) \
                 VALUES ('research-site','starts@example.invalid','pilot-lab','researcher') ON CONFLICT DO NOTHING;",
            )
            .await
            .unwrap();
        let dsn = dsn.replace("user=postgres", "user=broker_api_qual");
        let reader = person("starts-reader", "starts@example.invalid");
        let mut state = api(&dsn, reader.clone());
        state.workspace_catalog = starting_points();
        let actor = state.store.resolve_principal(&reader).await.unwrap();
        // Off by default: the lists say so and a request for several sets is refused before anything is saved.
        let off = serve(state.clone()).await;
        let client = reqwest::Client::new();
        let unchosen = Uuid::new_v4();
        state
            .store
            .create_project(
                actor,
                &CreateProject {
                    project_id: unchosen,
                    group: "pilot-lab".into(),
                    title: "Iron in foods people ate".into(),
                    visibility: Visibility::Private,
                },
            )
            .await
            .unwrap();
        let listed: serde_json::Value = client
            .get(format!("{off}/api/v1/projects/{unchosen}/workspaces"))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        assert_eq!(listed["combine"], false);
        let refused = client
            .post(format!("{off}/api/v1/projects/{unchosen}/prepare"))
            .header(MUTATION_INTENT, "mutate")
            .json(&json!({"workspace_keys": ["survey", "foods"], "goal": "Which foods people ate hold the most iron?",
                "constraints": [], "request_id": Uuid::new_v4(), "expected_revision": 0}))
            .send()
            .await
            .unwrap();
        assert_eq!(refused.status().as_u16(), 400);
        let refused: serde_json::Value = refused.json().await.unwrap();
        assert_eq!(
            refused["error"]["code"],
            "workspace_combination_unavailable"
        );
        let unsaved = state.store.get_project(actor, unchosen).await.unwrap();
        assert!(unsaved["checkpoint"].is_null(), "{unsaved}");
        state.combine_starting_points = true;
        let origin = serve(state.clone()).await;
        let new_project = || async {
            let id = Uuid::new_v4();
            state
                .store
                .create_project(
                    actor,
                    &CreateProject {
                        project_id: id,
                        group: "pilot-lab".into(),
                        title: "Iron in foods people ate".into(),
                        visibility: Visibility::Private,
                    },
                )
                .await
                .unwrap();
            id
        };
        let prepare = |project: Uuid, choice: serde_json::Value| {
            let mut body = json!({"goal": "Which foods people ate hold the most iron?", "constraints": [],
                "request_id": Uuid::new_v4(), "expected_revision": 0});
            body.as_object_mut()
                .unwrap()
                .extend(choice.as_object().unwrap().clone());
            client
                .post(format!("{origin}/api/v1/projects/{project}/prepare"))
                .header(MUTATION_INTENT, "mutate")
                .json(&body)
                .send()
        };
        let project = new_project().await;
        let offered: serde_json::Value = client
            .get(format!("{origin}/api/v1/projects/{project}/workspaces"))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        assert_eq!(offered["combine"], true);
        assert_eq!(offered["workspaces"].as_array().unwrap().len(), 3);

        // Two sets that share a workspace: the union, in the order chosen, and each set named.
        let started = prepare(project, json!({"workspace_keys": ["survey", "foods"]}))
            .await
            .unwrap();
        assert_eq!(started.status().as_u16(), 200);
        let started: serde_json::Value = started.json().await.unwrap();
        assert_eq!(
            (started["revision"].as_i64(), &started["allowed_profiles"]),
            (Some(1), &json!(["general-research"]))
        );
        let saved = state.store.get_project(actor, project).await.unwrap();
        let workspace = &saved["checkpoint"]["state"]["workspace"];
        assert_eq!(
            workspace["input_refs"],
            json!(["input://reference/survey", "input://reference/foods"])
        );
        let names: Vec<&str> = workspace["starting_points"]
            .as_array()
            .unwrap()
            .iter()
            .map(|p| p["name"].as_str().unwrap())
            .collect();
        assert_eq!(names, ["Set survey", "Set foods"]);
        // While the option is off, a saved version that names starting points is refused on the direct route too.
        let direct = client
            .post(format!("{off}/api/v1/projects/{unchosen}/checkpoints"))
            .header(MUTATION_INTENT, "mutate")
            .json(
                &json!({"expected_revision": 0, "request_id": Uuid::new_v4(),
                "state": saved["checkpoint"]["state"].clone()}),
            )
            .send()
            .await
            .unwrap();
        assert_eq!(direct.status().as_u16(), 400);
        let direct: serde_json::Value = direct.json().await.unwrap();
        assert_eq!(direct["error"]["code"], "workspace_combination_unavailable");
        assert!(state.store.get_project(actor, unchosen).await.unwrap()["checkpoint"].is_null());

        // Refusals: different workspaces (naming every set chosen), both fields, one set as a list, an unknown key.
        let refused = prepare(
            new_project().await,
            json!({"workspace_keys": ["foods", "figures"]}),
        )
        .await
        .unwrap();
        assert_eq!(refused.status().as_u16(), 400);
        let refused: serde_json::Value = refused.json().await.unwrap();
        assert_eq!(
            refused["error"]["code"],
            "workspace_combination_unsupported"
        );
        assert_eq!(refused["error"]["keys"], json!(["foods", "figures"]));
        for (choice, status, code) in [
            (
                json!({"workspace_key": "foods", "workspace_keys": ["foods", "survey"]}),
                400,
                "workspace_selection_invalid",
            ),
            (
                json!({"workspace_keys": ["foods"]}),
                400,
                "workspace_selection_invalid",
            ),
            (json!({}), 400, "workspace_selection_invalid"),
            (
                json!({"workspace_keys": ["foods", "elsewhere"]}),
                404,
                "workspace_unavailable",
            ),
            (
                json!({"workspace_keys": ["foods", "foods"]}),
                400,
                "workspace_selection_invalid",
            ),
        ] {
            let answer = prepare(new_project().await, choice.clone()).await.unwrap();
            assert_eq!(answer.status().as_u16(), status, "{choice}");
            let body: serde_json::Value = answer.json().await.unwrap();
            assert_eq!(body["error"]["code"], code, "{choice}");
        }
        // One set as before: no list of sets.
        let single = new_project().await;
        assert_eq!(
            prepare(single, json!({"workspace_key": "figures"}))
                .await
                .unwrap()
                .status()
                .as_u16(),
            200
        );
        let saved = state.store.get_project(actor, single).await.unwrap();
        assert!(
            saved["checkpoint"]["state"]["workspace"]
                .get("starting_points")
                .is_none()
        );
    }

    #[tokio::test]
    async fn history_names_one_direction_before_anything_is_read() {
        let _ = rustls::crypto::ring::default_provider().install_default();
        // No database answers this address: the refusal comes before the first read.
        let state = api(
            "host=127.0.0.1 port=1 user=none dbname=none",
            person("p", "p@example.invalid"),
        );
        let origin = serve(state).await;
        let project = Uuid::new_v4();
        for query in ["before=0&after=1", "before=5&after=1"] {
            let response = reqwest::get(format!(
                "{origin}/api/v1/projects/{project}/checkpoints?{query}"
            ))
            .await
            .unwrap();
            assert_eq!(response.status(), 400, "{query}");
            let body: serde_json::Value = response.json().await.unwrap();
            assert_eq!(
                body["error"]["code"], "history_direction_invalid",
                "{query}"
            );
        }
    }

    #[tokio::test]
    #[ignore = "isolated PostgreSQL through qualify_remote_broker.sh project-stream"]
    async fn a_project_stream_announces_a_saved_version_and_refuses_a_stranger() {
        let _ = rustls::crypto::ring::default_provider().install_default();
        let dsn = std::fs::read_to_string(
            std::env::var_os("BROKER_QUALIFICATION_MIGRATION_DSN_FILE").unwrap(),
        )
        .unwrap();
        let config: tokio_postgres::Config = dsn.parse().unwrap();
        let (owner, connection) = config.connect(tokio_postgres::NoTls).await.unwrap();
        tokio::spawn(connection);
        owner
            .batch_execute(
                "INSERT INTO enrichedlab_agent.broker_principal_invitations(tenant_id,email,group_key,role) \
                 VALUES ('research-site','stranger@example.invalid','pilot-lab','researcher');",
            )
            .await
            .unwrap();
        let dsn = dsn.replace("user=postgres", "user=broker_api_qual");
        let reader = person("stream-reader", "researcher@example.invalid");
        let state = api(&dsn, reader.clone());
        let actor = state.store.resolve_principal(&reader).await.unwrap();
        let project = Uuid::new_v4();
        state
            .store
            .create_project(
                actor,
                &CreateProject {
                    project_id: project,
                    group: "pilot-lab".into(),
                    title: "Stream check".into(),
                    visibility: Visibility::Private,
                },
            )
            .await
            .unwrap();
        let origin = serve(state.clone()).await;
        let client = reqwest::Client::new();
        let path = format!("/api/v1/projects/{project}/events/stream");
        let mut response = client
            .get(format!("{origin}{path}"))
            .header(ASSERTION_HEADER, "signed.assertion.value")
            .send()
            .await
            .unwrap();
        assert_eq!(response.status().as_u16(), 200);
        assert_eq!(
            response.headers()[header::CONTENT_TYPE],
            "text/event-stream"
        );
        let mut buffer = String::new();
        let (name, first) = next_event(&mut response, &mut buffer).await;
        assert_eq!(name, "project_state");
        assert_eq!(first["busy"], json!(false));
        assert_eq!(first["pulse"].as_str().unwrap().len(), 32);
        // A saved version is announced within one idle interval, and the payload names no research content.
        state
            .store
            .checkpoint_project(
                actor,
                project,
                &CheckpointRequest {
                    expected_revision: 0,
                    request_id: Uuid::new_v4(),
                    state: research(),
                },
            )
            .await
            .unwrap();
        let (name, changed) = tokio::time::timeout(
            PULSE_IDLE + Duration::from_secs(10),
            next_event(&mut response, &mut buffer),
        )
        .await
        .expect("a change within one idle interval");
        assert_eq!(name, "project_changed");
        assert_ne!(changed["pulse"], first["pulse"]);
        let mut keys: Vec<&String> = changed.as_object().unwrap().keys().collect();
        keys.sort();
        assert_eq!(keys, ["busy", "pulse"]);
        // Someone who may not read the project is refused before a stream opens.
        let stranger = serve(api(&dsn, person("stranger", "stranger@example.invalid"))).await;
        let refused = client
            .get(format!("{stranger}{path}"))
            .header(ASSERTION_HEADER, "signed.assertion.value")
            .send()
            .await
            .unwrap();
        assert_eq!(refused.status().as_u16(), 404);
    }
}
