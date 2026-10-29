use std::collections::HashSet;

use super::*;
use crate::broker::{
    campaign::CreateCampaign,
    workspace_catalog::{CombineRefusal, combine},
};

#[derive(Deserialize)]
pub(super) struct CampaignListQuery {
    #[serde(default = "default_campaign_limit")]
    limit: i32,
}

#[derive(Deserialize)]
pub(super) struct CampaignPageQuery {
    #[serde(default)]
    after: i64,
    #[serde(default = "default_campaign_page_limit")]
    limit: i32,
}

fn default_campaign_limit() -> i32 {
    50
}
fn default_campaign_page_limit() -> i32 {
    100
}

pub(super) async fn create(
    State(state): State<ApiState>,
    headers: HeaderMap,
    AxumPath(lab): AxumPath<String>,
    Json(request): Json<CreateCampaign>,
) -> Result<Response, ApiError> {
    require_mutation_intent(&headers)?;
    validate_key(&lab)?;
    validate_create(&request)?;
    let actor = authenticate(&state, &headers).await?;
    let identity = state.store.me(actor).await?;
    let available = state.workspace_catalog.available(&lab, &identity);
    let keys: HashSet<&str> = request
        .scope
        .starting_points
        .iter()
        .map(String::as_str)
        .collect();
    if keys.len() != request.scope.starting_points.len() || !(1..=16).contains(&keys.len()) {
        return Err(ApiError::invalid(
            "campaign_scope_invalid",
            "Campaign scope is invalid",
        ));
    }
    let chosen = request
        .scope
        .starting_points
        .iter()
        .map(|key| available.iter().copied().find(|entry| &entry.key == key))
        .collect::<Option<Vec<_>>>()
        .ok_or_else(|| ApiError::not_found("workspace_unavailable"))?;
    let (workspace, available_profiles) = match chosen.as_slice() {
        [entry] => (entry.workspace.clone(), entry.allowed_profiles.clone()),
        _ => combine(&chosen).map_err(|refusal| match refusal {
            CombineRefusal::TooLarge => ApiError::invalid(
                "workspace_combination_too_large",
                "Campaign starting points are too large",
            ),
            CombineRefusal::Invalid | CombineRefusal::Unsupported(_) => ApiError::invalid(
                "workspace_combination_unsupported",
                "Campaign starting points cannot be combined",
            ),
        })?,
    };
    let profiles: HashSet<&str> = request.scope.profiles.iter().map(String::as_str).collect();
    if profiles.len() != request.scope.profiles.len()
        || request.scope.profiles.is_empty()
        || request.scope.profiles.len() > 16
        || request
            .scope
            .profiles
            .iter()
            .any(|profile| !available_profiles.contains(profile))
    {
        return Err(ApiError::invalid(
            "campaign_scope_forbidden",
            "Campaign profiles are outside the selected scope",
        ));
    }
    let scope = json!({
        "starting_points": request.scope.starting_points,
        "profiles": request.scope.profiles,
        "workspace": workspace
    });
    let budgets = serde_json::to_value(&request.budgets).map_err(|_| ApiError::internal())?;
    let stop_rules = serde_json::to_value(&request.stop_rules).map_err(|_| ApiError::internal())?;
    let campaign = state
        .store
        .create_campaign(
            actor,
            request.campaign_id,
            &lab,
            &request.goal,
            &request.constraints,
            &scope,
            &budgets,
            &stop_rules,
        )
        .await?;
    Ok((
        StatusCode::CREATED,
        Json(json!({"campaign_id":campaign,"state":"planned"})),
    )
        .into_response())
}

fn validate_create(request: &CreateCampaign) -> Result<(), ApiError> {
    let goal = request.goal.chars().count();
    let constraints = request.constraints.chars().count();
    if !(1..=4_000).contains(&goal)
        || constraints > 4_000
        || request.goal.chars().any(char::is_control)
        || request.constraints.chars().any(char::is_control)
    {
        return Err(ApiError::invalid(
            "campaign_text_invalid",
            "Campaign text is invalid",
        ));
    }
    request
        .budgets
        .validate()
        .map_err(|code| ApiError::invalid(code, "Campaign budget is invalid"))?;
    request
        .stop_rules
        .validate()
        .map_err(|code| ApiError::invalid(code, "Campaign stop rules are invalid"))?;
    Ok(())
}

pub(super) async fn list(
    State(state): State<ApiState>,
    headers: HeaderMap,
    AxumPath(lab): AxumPath<String>,
    Query(query): Query<CampaignListQuery>,
) -> Result<impl IntoResponse, ApiError> {
    let actor = authenticate(&state, &headers).await?;
    Ok(Json(
        json!({"campaigns":state.store.campaigns(actor,&lab,query.limit).await?}),
    ))
}

pub(super) async fn get_one(
    State(state): State<ApiState>,
    headers: HeaderMap,
    AxumPath(campaign): AxumPath<Uuid>,
) -> Result<impl IntoResponse, ApiError> {
    let actor = authenticate(&state, &headers).await?;
    Ok(Json(state.store.campaign(actor, campaign).await?))
}

pub(super) async fn start(
    State(state): State<ApiState>,
    headers: HeaderMap,
    AxumPath(campaign): AxumPath<Uuid>,
) -> Result<impl IntoResponse, ApiError> {
    transition(state, headers, campaign, "start").await
}

pub(super) async fn resume(
    State(state): State<ApiState>,
    headers: HeaderMap,
    AxumPath(campaign): AxumPath<Uuid>,
) -> Result<impl IntoResponse, ApiError> {
    transition(state, headers, campaign, "resume").await
}

pub(super) async fn pause(
    State(state): State<ApiState>,
    headers: HeaderMap,
    AxumPath(campaign): AxumPath<Uuid>,
) -> Result<impl IntoResponse, ApiError> {
    transition(state, headers, campaign, "pause").await
}

pub(super) async fn stop(
    State(state): State<ApiState>,
    headers: HeaderMap,
    AxumPath(campaign): AxumPath<Uuid>,
) -> Result<impl IntoResponse, ApiError> {
    transition(state, headers, campaign, "stop").await
}

async fn transition(
    state: ApiState,
    headers: HeaderMap,
    campaign: Uuid,
    action: &'static str,
) -> Result<impl IntoResponse, ApiError> {
    require_mutation_intent(&headers)?;
    let actor = authenticate(&state, &headers).await?;
    Ok(Json(
        state
            .store
            .campaign_transition(action, actor, campaign, None)
            .await?,
    ))
}

pub(super) async fn events(
    State(state): State<ApiState>,
    headers: HeaderMap,
    AxumPath(campaign): AxumPath<Uuid>,
    Query(query): Query<CampaignPageQuery>,
) -> Result<impl IntoResponse, ApiError> {
    let actor = authenticate(&state, &headers).await?;
    let page = state
        .store
        .campaign_events(actor, campaign, query.after, query.limit)
        .await?;
    Ok(Json(
        json!({"events":page.items,"next_after":page.next_after}),
    ))
}

pub(super) async fn records(
    State(state): State<ApiState>,
    headers: HeaderMap,
    AxumPath(campaign): AxumPath<Uuid>,
    Query(query): Query<CampaignPageQuery>,
) -> Result<impl IntoResponse, ApiError> {
    let actor = authenticate(&state, &headers).await?;
    let page = state
        .store
        .campaign_records(actor, campaign, query.after, query.limit)
        .await?;
    Ok(Json(
        json!({"records":page.items,"next_after":page.next_after}),
    ))
}

pub(super) async fn planner_calls(
    State(state): State<ApiState>,
    headers: HeaderMap,
    AxumPath(campaign): AxumPath<Uuid>,
    Query(query): Query<CampaignPageQuery>,
) -> Result<impl IntoResponse, ApiError> {
    let actor = authenticate(&state, &headers).await?;
    let page = state
        .store
        .campaign_planner_calls(actor, campaign, query.after, query.limit)
        .await?;
    Ok(Json(
        json!({"calls":page.items,"next_after":page.next_after}),
    ))
}

pub(super) async fn planner_text(
    State(state): State<ApiState>,
    headers: HeaderMap,
    AxumPath((campaign, sha256)): AxumPath<(Uuid, String)>,
) -> Result<impl IntoResponse, ApiError> {
    if sha256.len() != 71
        || !sha256.starts_with("sha256:")
        || !sha256[7..]
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
    {
        return Err(ApiError::invalid(
            "campaign_planner_text_invalid",
            "Name a planner text by its SHA-256",
        ));
    }
    let actor = authenticate(&state, &headers).await?;
    let text = state
        .store
        .campaign_planner_text(actor, campaign, &sha256)
        .await?;
    Ok(Json(text))
}
