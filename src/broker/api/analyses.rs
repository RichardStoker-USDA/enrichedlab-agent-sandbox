//! Template analyses: the render service's catalog of analysis templates as the
//! worker last read it, a person's request for an analysis of one result table, an analysis's request with its receipts,
//! and the template analyses made from one analysis.
use super::*;
use crate::broker::store::AnalysisRequest;
use serde_json::Value;

const MAX_TITLE_CHARS: usize = 200;
const MAX_TABLE_PATH_BYTES: usize = 256;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct TemplateAnalysisRequest {
    source_task: Uuid,
    table_path: String,
    feature: String,
    #[serde(default)]
    settings: Value,
    #[serde(default)]
    title: String,
    #[serde(default)]
    previous: Option<Uuid>,
    request_id: Uuid,
}

/// Settings as an object of names and values; none is an empty object.
fn settings(settings: Value) -> Result<Value, ApiError> {
    match settings {
        Value::Null => Ok(json!({})),
        Value::Object(_) => Ok(settings),
        _ => Err(ApiError::invalid(
            "analysis_settings_invalid",
            "Settings must be an object of names and values",
        )),
    }
}

fn check_text(table_path: &str, title: &str) -> Result<(), ApiError> {
    if table_path.is_empty()
        || table_path.len() > MAX_TABLE_PATH_BYTES
        || table_path.contains('\0')
        || title.chars().count() > MAX_TITLE_CHARS
        || title.contains('\0')
    {
        return Err(ApiError::invalid(
            "analysis_request_invalid",
            "The table path or the title is not valid",
        ));
    }
    Ok(())
}

/// The analysis templates the render service declares, with each one's settings and the labs that may run them.
pub(super) async fn catalog(
    State(state): State<ApiState>,
    headers: HeaderMap,
) -> Result<impl IntoResponse, ApiError> {
    let actor = authenticate(&state, &headers).await?;
    Ok(Json(
        json!({ "catalog": state.store.analysis_catalog(actor).await? }),
    ))
}

/// `POST /api/v1/projects/{project}/template-analyses`: a person's request for a template analysis of one result table
/// of a finished analysis in the project. The broker checks the table, the template and each setting's name and kind;
/// the render service checks the values when it runs the template.
pub(super) async fn request(
    State(state): State<ApiState>,
    headers: HeaderMap,
    AxumPath(project): AxumPath<Uuid>,
    Json(request): Json<TemplateAnalysisRequest>,
) -> Result<impl IntoResponse, ApiError> {
    require_mutation_intent(&headers)?;
    let settings = settings(request.settings)?;
    check_text(&request.table_path, &request.title)?;
    let actor = authenticate(&state, &headers).await?;
    let task = state
        .store
        .request_analysis(
            actor,
            &AnalysisRequest {
                project,
                source_task: request.source_task,
                table_path: &request.table_path,
                feature: &request.feature,
                settings: &settings,
                title: request.title.trim(),
                previous: request.previous,
                request_id: request.request_id,
            },
        )
        .await?;
    Ok((StatusCode::ACCEPTED, Json(json!({ "task_id": task }))))
}

/// A template analysis's request, its source analysis and every receipt; null for a task that is not one.
pub(super) async fn detail(
    State(state): State<ApiState>,
    headers: HeaderMap,
    AxumPath(task): AxumPath<Uuid>,
) -> Result<impl IntoResponse, ApiError> {
    let actor = authenticate(&state, &headers).await?;
    Ok(Json(
        json!({ "template_analysis": state.store.task_analysis_detail(actor, task).await? }),
    ))
}

/// The template analyses made from one analysis that the reader may open, newest first.
pub(super) async fn versions(
    State(state): State<ApiState>,
    headers: HeaderMap,
    AxumPath(task): AxumPath<Uuid>,
) -> Result<impl IntoResponse, ApiError> {
    let actor = authenticate(&state, &headers).await?;
    Ok(Json(
        json!({ "template_analyses": state.store.analysis_versions(actor, task).await? }),
    ))
}
