//! A lab's tools (migration 080): each method the lab may run with how it is granted and its catalog
//! fields, and the tool groups it holds. The database function checks that the caller belongs to the lab.
use super::*;
use lab_notes::lab_key;

/// The lab's tools for one of its members: each method it may run with how it is granted and its catalog fields, and
/// the tool groups it holds (migration 080).
pub(super) async fn lab_tools(
    State(state): State<ApiState>,
    headers: HeaderMap,
    AxumPath(lab): AxumPath<String>,
) -> Result<impl IntoResponse, ApiError> {
    let actor = authenticate(&state, &headers).await?;
    if !lab_key(&lab) {
        return Err(ApiError::not_found("lab_not_found"));
    }
    Ok(Json(state.store.lab_tools(actor, &lab).await?))
}
