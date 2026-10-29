//! Lab memory routes: the lab's shared notes, and sharing, retiring and renewing
//! one. Each database function authorizes the caller; only a person may change lab memory.
use super::*;
use serde_json::Value;

/// Share an observation from the project's current saved version with its lab.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct ShareNoteRequest {
    revision: i64,
    statement: String,
    evidence_refs: Vec<String>,
    #[serde(default)]
    reason: Option<String>,
    #[serde(default)]
    review_days: Option<i32>,
    request_id: Uuid,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct RetireNoteRequest {
    reason: String,
    request_id: Uuid,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct RenewNoteRequest {
    review_days: i32,
    request_id: Uuid,
}

pub(super) fn lab_key(value: &str) -> bool {
    let mut bytes = value.bytes();
    (2..=64).contains(&value.len())
        && bytes.next().is_some_and(|b| b.is_ascii_lowercase())
        && bytes.all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || matches!(b, b'_' | b'-'))
}

/// Every shared note of a lab the caller belongs to, newest first.
pub(super) async fn lab_notes(
    State(state): State<ApiState>,
    headers: HeaderMap,
    AxumPath(lab): AxumPath<String>,
) -> Result<impl IntoResponse, ApiError> {
    let actor = authenticate(&state, &headers).await?;
    if !lab_key(&lab) {
        return Err(ApiError::not_found("lab_not_found"));
    }
    Ok(Json(state.store.lab_notes(actor, &lab).await?))
}

pub(super) async fn share(
    State(state): State<ApiState>,
    headers: HeaderMap,
    AxumPath(project): AxumPath<Uuid>,
    Json(request): Json<ShareNoteRequest>,
) -> Result<impl IntoResponse, ApiError> {
    require_mutation_intent(&headers)?;
    let actor = authenticate(&state, &headers).await?;
    let refs: Value = json!(request.evidence_refs);
    let note = state
        .store
        .share_lab_note(
            actor,
            project,
            request.revision,
            &request.statement,
            &refs,
            request.reason.as_deref(),
            request.review_days,
            request.request_id,
        )
        .await?;
    Ok((StatusCode::CREATED, Json(json!({ "note": note }))))
}

pub(super) async fn retire(
    State(state): State<ApiState>,
    headers: HeaderMap,
    AxumPath(note): AxumPath<Uuid>,
    Json(request): Json<RetireNoteRequest>,
) -> Result<impl IntoResponse, ApiError> {
    require_mutation_intent(&headers)?;
    let actor = authenticate(&state, &headers).await?;
    let note = state
        .store
        .retire_lab_note(actor, note, &request.reason, request.request_id)
        .await?;
    Ok(Json(json!({ "note": note })))
}

pub(super) async fn renew(
    State(state): State<ApiState>,
    headers: HeaderMap,
    AxumPath(note): AxumPath<Uuid>,
    Json(request): Json<RenewNoteRequest>,
) -> Result<impl IntoResponse, ApiError> {
    require_mutation_intent(&headers)?;
    let actor = authenticate(&state, &headers).await?;
    let note = state
        .store
        .renew_lab_note(actor, note, request.review_days, request.request_id)
        .await?;
    Ok(Json(json!({ "note": note })))
}

#[cfg(test)]
mod tests {
    use super::lab_key;

    #[test]
    fn lab_keys_follow_the_group_key_rule() {
        assert!(lab_key("retained-byte-qualification"));
        assert!(lab_key("example_lab"));
        assert!(!lab_key("B"));
        assert!(!lab_key("1lab"));
        assert!(!lab_key("lab/../x"));
        assert!(!lab_key(&"a".repeat(65)));
    }
}
