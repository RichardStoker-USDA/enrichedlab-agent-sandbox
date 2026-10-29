//! Answer documents (migration 072): a person's request for an Excel workbook, a Word document or
//! PowerPoint slides of a research run's answer, a document's request with its receipts, and the documents made from
//! one run. The worker writes the file; the result package downloads like any other result.
use super::*;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct DocumentRequest {
    run_id: Uuid,
    kind: String,
    request_id: Uuid,
}

/// A person's request for a document of a run's answer in the project. The run must have ended on a decision for a
/// person's review or a pause; the database pins that decision and every result it cites.
pub(super) async fn request(
    State(state): State<ApiState>,
    headers: HeaderMap,
    AxumPath(project): AxumPath<Uuid>,
    Json(request): Json<DocumentRequest>,
) -> Result<impl IntoResponse, ApiError> {
    require_mutation_intent(&headers)?;
    if !["xlsx", "docx", "pptx"].contains(&request.kind.as_str()) {
        return Err(ApiError::invalid(
            "document_request_invalid",
            "A document is an xlsx workbook, a docx document or pptx slides",
        ));
    }
    let actor = authenticate(&state, &headers).await?;
    let task = state
        .store
        .request_document(
            actor,
            project,
            request.run_id,
            &request.kind,
            request.request_id,
        )
        .await?;
    Ok((StatusCode::ACCEPTED, Json(json!({ "task_id": task }))))
}

/// A document's request and every receipt; null for a task that is not a document.
pub(super) async fn detail(
    State(state): State<ApiState>,
    headers: HeaderMap,
    AxumPath(task): AxumPath<Uuid>,
) -> Result<impl IntoResponse, ApiError> {
    let actor = authenticate(&state, &headers).await?;
    Ok(Json(
        json!({ "document": state.store.task_document_detail(actor, task).await? }),
    ))
}

/// The documents made from one run that the reader may open, newest first.
pub(super) async fn for_run(
    State(state): State<ApiState>,
    headers: HeaderMap,
    AxumPath(run): AxumPath<Uuid>,
) -> Result<impl IntoResponse, ApiError> {
    let actor = authenticate(&state, &headers).await?;
    Ok(Json(
        json!({ "documents": state.store.run_documents(actor, run).await? }),
    ))
}
