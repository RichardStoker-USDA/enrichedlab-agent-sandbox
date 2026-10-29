//! Knowledge actions a signed-in person takes through the workspace. The workspace checks the
//! person's Access sign-in as for every route, then forwards that same assertion to the knowledge service, which checks
//! it again and acts as the person. A service account never reaches the service through these routes, and a response
//! carries only the listed record fields and known refusal codes.
use super::*;
use crate::broker::{
    knowledge_person::{ASSERTION_HEADER, KnowledgePerson, PersonCallError, refusal_code},
    model::PrincipalClass,
};
use reqwest::Method;
use serde_json::Value;

/// The record kinds a person may mark baseline as a whole. A paper's mark covers its sections, figures and tables as
/// they stand when it is made; the service counts later parts as waiting until the paper is marked again.
const MARKABLE_KINDS: [&str; 4] = ["dataset", "paper", "tool", "object_collection"];
const RECORD_FIELDS: [&str; 11] = [
    "resource_id",
    "kind",
    "scope",
    "baseline",
    "title",
    "summary",
    "data_class",
    "rights",
    "lifecycle",
    "source_uri",
    "created_at",
];
/// The first page asked for. A record can reach about 53 KB (a 4,000-character summary and other bounded fields,
/// escaped), so a full page may pass the response limit; `commons` then halves the page until it fits.
const LIST_LIMIT: usize = 100;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct CommonsQuery {
    #[serde(default)]
    kind: Option<String>,
    /// The `next` of the page before, to continue the listing.
    #[serde(default)]
    after: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct CollectionRequest {
    prefix: String,
    baseline: bool,
}

/// A listing cursor in the knowledge service's form: microseconds, a dot and a record's UUID.
fn cursor_shaped(value: &str) -> bool {
    value.split_once('.').is_some_and(|(micros, id)| {
        (1..=19).contains(&micros.len())
            && micros.bytes().all(|b| b.is_ascii_digit())
            && id.parse::<Uuid>().is_ok()
    })
}

/// A collection prefix in the knowledge schema's form: a scheme, `://`, an authority and a slash, at most 180 bytes.
fn prefix_shaped(value: &str) -> bool {
    let Some((scheme, rest)) = value.split_once("://") else {
        return false;
    };
    let Some((authority, _)) = rest.split_once('/') else {
        return false;
    };
    value.len() <= 180
        && scheme
            .bytes()
            .next()
            .is_some_and(|b| b.is_ascii_lowercase())
        && scheme.bytes().all(|b| {
            b.is_ascii_lowercase() || b.is_ascii_digit() || matches!(b, b'+' | b'.' | b'-')
        })
        && authority
            .bytes()
            .next()
            .is_some_and(|b| b.is_ascii_lowercase() || b.is_ascii_digit())
        && authority.bytes().all(|b| {
            b.is_ascii_lowercase() || b.is_ascii_digit() || matches!(b, b'.' | b'_' | b'-')
        })
        && !value.chars().any(char::is_control)
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct BaselineRequest {
    baseline: bool,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct ReviewRequest {
    accept: bool,
    #[serde(default)]
    note: Option<String>,
}

const REVIEWER_ROLES: [&str; 2] = ["curator", "lead"];
const QUEUE_LIMIT: usize = 100;

fn service(state: &ApiState) -> Result<&KnowledgePerson, ApiError> {
    state.knowledge_person.as_deref().ok_or(ApiError {
        status: StatusCode::CONFLICT,
        code: "knowledge_not_configured",
        message: "Knowledge actions are not set up for this workspace",
    })
}

fn person_required() -> ApiError {
    ApiError {
        status: StatusCode::FORBIDDEN,
        code: "knowledge_person_required",
        message: "Only a signed-in person can act on knowledge",
    }
}

/// The knowledge client and the signed-in person's assertion, once the workspace's own checks pass: a verified Access
/// sign-in of a person the workspace knows and has not disabled. Nothing reaches the service before this returns.
async fn signed_in_person<'a>(
    state: &'a ApiState,
    headers: &HeaderMap,
) -> Result<(&'a KnowledgePerson, String), ApiError> {
    let identity = state
        .verifier
        .verify(headers)
        .await
        .map_err(ApiError::from)?;
    if identity.class != PrincipalClass::Person {
        return Err(person_required());
    }
    let service = service(state)?;
    state
        .store
        .resolve_principal(&identity)
        .await
        .map_err(ApiError::from)?;
    let assertion = headers
        .get(ASSERTION_HEADER)
        .and_then(|value| value.to_str().ok())
        .map(str::to_owned)
        .ok_or_else(person_required)?;
    Ok((service, assertion))
}

fn call_error(error: PersonCallError) -> ApiError {
    let (status, message) = match error {
        PersonCallError::Unavailable => (
            StatusCode::SERVICE_UNAVAILABLE,
            "The knowledge service is unavailable",
        ),
        PersonCallError::Invalid => (
            StatusCode::BAD_GATEWAY,
            "The knowledge service answered out of contract",
        ),
        PersonCallError::OutcomeUnknown => (
            StatusCode::GATEWAY_TIMEOUT,
            "The knowledge service did not confirm the change; it may or may not have been made",
        ),
        PersonCallError::TooLarge => (
            StatusCode::BAD_GATEWAY,
            "The knowledge service's answer was larger than the workspace accepts",
        ),
    };
    ApiError {
        status,
        code: error.code(),
        message,
    }
}

/// The service's refusal as the workspace's: a refusal of the person keeps its status, and anything that says the two
/// services disagree (the workspace's own token, its forwarder role, the signing keys, a server fault) is a gateway
/// failure, not the person's.
fn refusal(status: reqwest::StatusCode, value: &Value) -> ApiError {
    let code = refusal_code(value);
    let (status, message) = match (status.as_u16(), code) {
        (403, "baseline_refused") => (
            StatusCode::FORBIDDEN,
            "Only a person who leads the commons can change its baseline",
        ),
        (403, "person_not_enrolled") => (
            StatusCode::FORBIDDEN,
            "The knowledge service does not know this person",
        ),
        (401, "person_assertion_invalid") => (
            StatusCode::FORBIDDEN,
            "The knowledge service did not accept this sign-in",
        ),
        (404, "resource_not_found") => (StatusCode::NOT_FOUND, "The record was not found"),
        (404, "relation_not_found") => (
            StatusCode::NOT_FOUND,
            "The link was not found, or this person cannot review it",
        ),
        (403, "relation_review_refused") => (
            StatusCode::FORBIDDEN,
            "Only a person acting directly can review a link",
        ),
        (400 | 409, "relation_not_proposed") => {
            (StatusCode::CONFLICT, "The link was already reviewed")
        }
        (400 | 409, "baseline_not_active") => (
            StatusCode::CONFLICT,
            "Only an active record can be marked baseline",
        ),
        (503, "person_keys_unavailable") => (
            StatusCode::SERVICE_UNAVAILABLE,
            "The knowledge service cannot check sign-ins now",
        ),
        (401, "person_sign_in_stale") => (
            StatusCode::FORBIDDEN,
            "A change needs a more recent sign-in",
        ),
        (400, "collection_invalid") => (
            StatusCode::BAD_REQUEST,
            "The collection prefix is not valid",
        ),
        _ => (
            StatusCode::BAD_GATEWAY,
            "The knowledge service refused the request",
        ),
    };
    ApiError {
        status,
        code,
        message,
    }
}

/// A refused change. A server fault may come after the change was made, so it reads as an unknown outcome; the
/// sign-in keys are checked before anything changes.
fn change_refusal(status: reqwest::StatusCode, value: &Value) -> ApiError {
    if status.is_server_error() && refusal_code(value) != "person_keys_unavailable" {
        return call_error(PersonCallError::OutcomeUnknown);
    }
    refusal(status, value)
}

/// A record reduced to the fields the workspace shows, each of its expected type.
fn record(value: &Value) -> Result<Value, ApiError> {
    let invalid = || call_error(PersonCallError::Invalid);
    let object = value.as_object().ok_or_else(invalid)?;
    let mut shown = serde_json::Map::new();
    for field in RECORD_FIELDS {
        let item = object.get(field).cloned().unwrap_or(Value::Null);
        let fits = match field {
            "baseline" => item.is_boolean(),
            "summary" | "source_uri" => item.is_null() || item.is_string(),
            _ => item.is_string(),
        };
        if !fits {
            return Err(invalid());
        }
        shown.insert(field.to_owned(), item);
    }
    Ok(Value::Object(shown))
}

/// The commons records of one kind (datasets unless another markable kind is named) that the signed-in person can
/// see, each with its baseline mark, so a person who leads the commons sees what waits for the mark.
pub(super) async fn commons(
    State(state): State<ApiState>,
    headers: HeaderMap,
    Query(query): Query<CommonsQuery>,
) -> Result<impl IntoResponse, ApiError> {
    let kind = query.kind.as_deref().unwrap_or("dataset");
    if !MARKABLE_KINDS.contains(&kind) {
        return Err(ApiError::invalid(
            "knowledge_kind_invalid",
            "The record kind cannot be marked baseline",
        ));
    }
    if query.after.as_deref().is_some_and(|a| !cursor_shaped(a)) {
        return Err(ApiError::invalid(
            "knowledge_cursor_invalid",
            "The page to continue from is not valid",
        ));
    }
    let (service, assertion) = signed_in_person(&state, &headers).await?;
    let (status, value) = commons_page(service, kind, query.after.as_deref(), &assertion).await?;
    if !status.is_success() {
        return Err(refusal(status, &value));
    }
    let mut shown = commons_records(&value, service.commons_scope(), kind)?;
    let (status, role) = service
        .call(
            Method::GET,
            &format!("v1/scopes/{}/role", service.commons_scope()),
            &assertion,
            None,
        )
        .await
        .map_err(call_error)?;
    if !status.is_success() {
        return Err(refusal(status, &role));
    }
    let (role, person) = commons_role(&role, service.commons_scope())?;
    // The service still decides each mark when it is made; this only says whether to offer it.
    shown["role"] = json!(role);
    shown["can_mark"] = json!(person && role == Some("lead"));
    Ok(Json(shown))
}

/// The person's role in the commons, one of the knowledge schema's three or none, and whether the service counts the
/// caller as a person acting directly.
/// One page of the commons' records of one kind. The scope filter applies before the service's limit, so lab records
/// never crowd commons records out. A page whose answer passes the response limit is asked for again at half the size
/// (a read, so asking again is safe); the cursor that page returns continues from where it ended.
async fn commons_page(
    service: &KnowledgePerson,
    kind: &str,
    after: Option<&str>,
    assertion: &str,
) -> Result<(StatusCode, Value), ApiError> {
    let mut limit = LIST_LIMIT;
    loop {
        let mut path = format!(
            "v1/resources?kind={kind}&scope={}&limit={limit}",
            service.commons_scope()
        );
        if let Some(after) = after {
            path.push_str(&format!("&after={after}"));
        }
        match service.call(Method::GET, &path, assertion, None).await {
            Err(PersonCallError::TooLarge) if limit > 1 => limit /= 2,
            answer => return answer.map_err(call_error),
        }
    }
}

fn commons_role<'a>(value: &'a Value, scope: &str) -> Result<(Option<&'a str>, bool), ApiError> {
    let invalid = || call_error(PersonCallError::Invalid);
    let role = match &value["role"] {
        Value::Null => None,
        Value::String(role) if ["member", "curator", "lead"].contains(&role.as_str()) => {
            Some(role.as_str())
        }
        _ => return Err(invalid()),
    };
    let person = value["person"].as_bool().ok_or_else(invalid)?;
    if value["scope"] != scope {
        return Err(invalid());
    }
    Ok((role, person))
}

/// One page of the commons records of the asked kind, reduced to the fields the workspace shows, with `next` for the
/// page after it; `complete` is true on the last page. A marked paper carries how many of its parts wait for a mark.
fn commons_records(value: &Value, scope: &str, kind: &str) -> Result<Value, ApiError> {
    let invalid = || call_error(PersonCallError::Invalid);
    let listed = value["resources"].as_array().ok_or_else(invalid)?;
    let next = match &value["next"] {
        Value::Null => None,
        Value::String(next) if cursor_shaped(next) => Some(next.clone()),
        _ => return Err(invalid()),
    };
    let waiting = match &value["parts_waiting"] {
        Value::Null => serde_json::Map::new(),
        Value::Object(map) => map.clone(),
        _ => return Err(invalid()),
    };
    let mut records = Vec::new();
    for item in listed {
        let mut shown = record(item)?;
        if shown["scope"] != scope || shown["kind"] != kind {
            return Err(invalid());
        }
        if kind == "paper" {
            let id = shown["resource_id"].as_str().unwrap_or_default();
            let parts = match waiting.get(id) {
                None => 0,
                Some(count) => count.as_u64().ok_or_else(invalid)?,
            };
            shown["parts_waiting"] = json!(parts);
        }
        records.push(shown);
    }
    Ok(json!({
        "scope": scope,
        "kind": kind,
        "records": records,
        "complete": next.is_none(),
        "next": next,
    }))
}

/// One collection as the workspace shows it: its prefix, each kind's active and marked counts, and the parts of marked
/// papers that wait. Anything out of shape refuses the whole answer.
fn collection(value: &Value) -> Result<Value, ApiError> {
    let invalid = || call_error(PersonCallError::Invalid);
    let prefix = value["prefix"]
        .as_str()
        .filter(|p| prefix_shaped(p))
        .ok_or_else(invalid)?;
    let mut kinds = serde_json::Map::new();
    for (kind, count) in value["kinds"].as_object().ok_or_else(invalid)? {
        let shaped = (1..=32).contains(&kind.len())
            && kind.bytes().all(|b| b.is_ascii_lowercase() || b == b'_');
        let active = count["active"].as_u64().ok_or_else(invalid)?;
        let marked = count["marked"].as_u64().ok_or_else(invalid)?;
        if !shaped || marked > active {
            return Err(invalid());
        }
        kinds.insert(kind.clone(), json!({"active": active, "marked": marked}));
    }
    let waiting = value["parts_waiting"].as_u64().ok_or_else(invalid)?;
    Ok(json!({"prefix": prefix, "kinds": kinds, "parts_waiting": waiting}))
}

/// The commons collections the signed-in person can see, by their records' source addresses, with what each kind has
/// marked, how many parts of marked papers wait, and whether this person may mark a collection.
pub(super) async fn collections(
    State(state): State<ApiState>,
    headers: HeaderMap,
) -> Result<impl IntoResponse, ApiError> {
    let (service, assertion) = signed_in_person(&state, &headers).await?;
    let scope = service.commons_scope();
    let (status, value) = service
        .call(
            Method::GET,
            &format!("v1/scopes/{scope}/collections"),
            &assertion,
            None,
        )
        .await
        .map_err(call_error)?;
    if !status.is_success() {
        return Err(refusal(status, &value));
    }
    let listed = value["collections"]
        .as_array()
        .filter(|_| value["scope"] == scope)
        .ok_or_else(|| call_error(PersonCallError::Invalid))?;
    let shown = listed
        .iter()
        .map(collection)
        .collect::<Result<Vec<_>, _>>()?;
    let (status, role) = service
        .call(
            Method::GET,
            &format!("v1/scopes/{scope}/role"),
            &assertion,
            None,
        )
        .await
        .map_err(call_error)?;
    if !status.is_success() {
        return Err(refusal(status, &role));
    }
    let (role, person) = commons_role(&role, scope)?;
    Ok(Json(json!({
        "scope": scope,
        "collections": shown,
        "role": role,
        "can_mark": person && role == Some("lead"),
    })))
}

/// Marks every commons record of a collection baseline, with each paper's parts, or withdraws the mark, as the
/// signed-in person. The knowledge service decides whether this person may and records each change in their name.
pub(super) async fn collection_baseline(
    State(state): State<ApiState>,
    headers: HeaderMap,
    Json(request): Json<CollectionRequest>,
) -> Result<impl IntoResponse, ApiError> {
    require_mutation_intent(&headers)?;
    if !prefix_shaped(&request.prefix) {
        return Err(ApiError::invalid(
            "knowledge_collection_invalid",
            "The collection prefix is not valid",
        ));
    }
    let (service, assertion) = signed_in_person(&state, &headers).await?;
    let (status, value) = service
        .call(
            Method::POST,
            "v1/collections/baseline",
            &assertion,
            Some(&json!({"prefix": request.prefix, "baseline": request.baseline})),
        )
        .await
        .map_err(call_error)?;
    if !status.is_success() {
        return Err(change_refusal(status, &value));
    }
    let unknown = || call_error(PersonCallError::OutcomeUnknown);
    if value["prefix"] != request.prefix.as_str() || value["baseline"] != request.baseline {
        return Err(unknown());
    }
    let records = value["records_changed"].as_u64().ok_or_else(unknown)?;
    let parts = value["parts_changed"].as_u64().ok_or_else(unknown)?;
    Ok(Json(json!({
        "prefix": request.prefix,
        "baseline": request.baseline,
        "records_changed": records,
        "parts_changed": parts,
    })))
}

/// Marks a commons record baseline for every lab, or withdraws the mark, as the signed-in person. The knowledge
/// service decides whether this person may, and records the decision in the person's name.
pub(super) async fn baseline(
    State(state): State<ApiState>,
    headers: HeaderMap,
    AxumPath(resource): AxumPath<Uuid>,
    Json(request): Json<BaselineRequest>,
) -> Result<impl IntoResponse, ApiError> {
    require_mutation_intent(&headers)?;
    let (service, assertion) = signed_in_person(&state, &headers).await?;
    let (status, value) = service
        .call(
            Method::POST,
            &format!("v1/resources/{resource}/baseline"),
            &assertion,
            Some(&json!({"baseline": request.baseline})),
        )
        .await
        .map_err(call_error)?;
    if !status.is_success() {
        return Err(change_refusal(status, &value));
    }
    // The service answered success, so the change was made; an answer out of shape still leaves its extent unknown.
    let unknown = || call_error(PersonCallError::OutcomeUnknown);
    let mut shown = record(&value).map_err(|_| unknown())?;
    if shown["resource_id"] != resource.to_string() || shown["baseline"] != request.baseline {
        return Err(unknown());
    }
    shown["parts_changed"] = json!(value["parts_changed"].as_u64().ok_or_else(unknown)?);
    Ok(Json(shown))
}

/// A lab's key in the knowledge schema's scope form.
fn lab_scope(lab: &str) -> Result<(), ApiError> {
    let shaped = (1..=63).contains(&lab.len())
        && lab
            .bytes()
            .next()
            .is_some_and(|b| b.is_ascii_lowercase() || b.is_ascii_digit())
        && lab.bytes().all(|b| {
            b.is_ascii_lowercase() || b.is_ascii_digit() || matches!(b, b'.' | b'_' | b'-')
        });
    if shaped {
        Ok(())
    } else {
        Err(ApiError::not_found("lab_not_found"))
    }
}

/// A proposed link reduced to what the workspace shows: its two ends by kind and title, the names found and how many
/// sections hold them. Anything out of shape refuses the whole answer.
fn proposal(value: &Value) -> Result<Value, ApiError> {
    let invalid = || call_error(PersonCallError::Invalid);
    let text = |field: &str| value[field].as_str().map(str::to_owned).ok_or_else(invalid);
    let id = |field: &str| {
        value[field]
            .as_str()
            .and_then(|v| v.parse::<Uuid>().ok())
            .ok_or_else(invalid)
    };
    let evidence = &value["evidence"];
    let terms = match &evidence["terms"] {
        Value::Null => Vec::new(),
        Value::Array(items) => items
            .iter()
            .map(|t| t.as_str().map(str::to_owned).ok_or_else(invalid))
            .collect::<Result<Vec<_>, _>>()?,
        _ => return Err(invalid()),
    };
    let sections = match &evidence["section_count"] {
        Value::Null => None,
        count => Some(count.as_u64().ok_or_else(invalid)?),
    };
    Ok(json!({
        "relation_id": id("relation_id")?, "kind": text("kind")?,
        "source": {"id": id("source_id")?, "kind": text("source_kind")?, "title": text("source_title")?},
        "target": {"id": id("target_id")?, "kind": text("target_kind")?, "title": text("target_title")?},
        "terms": terms, "sections": sections, "created_at": text("created_at")?,
    }))
}

/// The links proposed from a lab's records that the signed-in person can read, oldest first, and whether the person
/// may review them: a person acting directly who curates or leads the lab.
pub(super) async fn proposals(
    State(state): State<ApiState>,
    headers: HeaderMap,
    AxumPath(lab): AxumPath<String>,
) -> Result<impl IntoResponse, ApiError> {
    lab_scope(&lab)?;
    let (service, assertion) = signed_in_person(&state, &headers).await?;
    let (status, value) = service
        .call(
            Method::GET,
            &format!("v1/scopes/{lab}/proposals?limit={QUEUE_LIMIT}"),
            &assertion,
            None,
        )
        .await
        .map_err(call_error)?;
    if !status.is_success() {
        return Err(refusal(status, &value));
    }
    let listed = value["proposals"]
        .as_array()
        .filter(|_| value["scope"] == lab.as_str())
        .ok_or_else(|| call_error(PersonCallError::Invalid))?;
    let shown = listed.iter().map(proposal).collect::<Result<Vec<_>, _>>()?;
    let (status, role) = service
        .call(
            Method::GET,
            &format!("v1/scopes/{lab}/role"),
            &assertion,
            None,
        )
        .await
        .map_err(call_error)?;
    if !status.is_success() {
        return Err(refusal(status, &role));
    }
    let (role, person) = commons_role(&role, &lab)?;
    Ok(Json(json!({
        "lab": lab,
        "proposals": shown,
        "complete": listed.len() < QUEUE_LIMIT,
        "role": role,
        "can_review": person && role.is_some_and(|r| REVIEWER_ROLES.contains(&r)),
    })))
}

/// Accepts a proposed link, or rejects it with an optional note, as the signed-in person. The knowledge service
/// decides whether this person may, and records the review in the person's name.
pub(super) async fn review(
    State(state): State<ApiState>,
    headers: HeaderMap,
    AxumPath(relation): AxumPath<Uuid>,
    Json(request): Json<ReviewRequest>,
) -> Result<impl IntoResponse, ApiError> {
    require_mutation_intent(&headers)?;
    let note = request
        .note
        .map(|n| n.trim().to_owned())
        .filter(|n| !n.is_empty());
    if note
        .as_deref()
        .is_some_and(|n| n.chars().count() > 500 || n.chars().any(char::is_control))
    {
        return Err(ApiError::invalid(
            "knowledge_note_invalid",
            "The note is at most 500 characters of plain text",
        ));
    }
    let (service, assertion) = signed_in_person(&state, &headers).await?;
    let (status, value) = service
        .call(
            Method::POST,
            &format!("v1/relations/{relation}/review"),
            &assertion,
            Some(&json!({"accept": request.accept, "note": note})),
        )
        .await
        .map_err(call_error)?;
    let outcome = match (request.accept, status.as_u16()) {
        (true, 200)
            if value["relation_id"] == relation.to_string() && value["assertion"] == "reviewed" =>
        {
            "accepted"
        }
        (false, 204) => "rejected",
        (_, 200 | 204) => return Err(call_error(PersonCallError::OutcomeUnknown)),
        _ => return Err(change_refusal(status, &value)),
    };
    Ok(Json(json!({"relation_id": relation, "outcome": outcome})))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::broker::{
        auth::{AuthError, AuthVerifier},
        knowledge_person::load_optional,
        model::VerifiedIdentity,
    };
    use async_trait::async_trait;
    use axum::routing::any;

    type Seen = Arc<Mutex<Vec<(String, String, Option<String>, Option<String>, Value)>>>;

    /// A stand-in knowledge service: it records each request and gives the next queued answer.
    async fn stand_in(answers: Vec<(u16, Value)>) -> (String, Seen) {
        let seen: Seen = Arc::default();
        let queue = Arc::new(Mutex::new(VecDeque::from(answers)));
        let log = seen.clone();
        let app = Router::new().fallback(any(
            move |uri: axum::http::Uri,
                  method: axum::http::Method,
                  headers: HeaderMap,
                  body: axum::body::Bytes| {
                let (log, queue) = (log.clone(), queue.clone());
                async move {
                    let get = |name: &str| {
                        headers
                            .get(name)
                            .and_then(|v| v.to_str().ok())
                            .map(str::to_owned)
                    };
                    log.lock().unwrap().push((
                        format!("{method} {uri}"),
                        get("authorization").unwrap_or_default(),
                        get(ASSERTION_HEADER),
                        get("x-knowledge-intent"),
                        serde_json::from_slice(&body).unwrap_or(Value::Null),
                    ));
                    let (status, value) = queue
                        .lock()
                        .unwrap()
                        .pop_front()
                        .unwrap_or((500, Value::Null));
                    (StatusCode::from_u16(status).unwrap(), Json(value))
                }
            },
        ));
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let origin = format!("http://{}/", listener.local_addr().unwrap());
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        (origin, seen)
    }

    struct Signed(VerifiedIdentity);

    #[async_trait]
    impl AuthVerifier for Signed {
        async fn verify(&self, _headers: &HeaderMap) -> Result<VerifiedIdentity, AuthError> {
            Ok(self.0.clone())
        }
    }

    fn identity(class: PrincipalClass, email: &str) -> VerifiedIdentity {
        VerifiedIdentity {
            issuer: "https://identity.example.invalid".into(),
            subject: format!("subject-{email}"),
            email: email.into(),
            display_name: None,
            class,
        }
    }

    fn state(dsn: &str, identity: VerifiedIdentity, knowledge: Option<(&Path, &str)>) -> ApiState {
        let knowledge_person = knowledge.map(|(dir, origin)| {
            std::fs::write(dir.join("token"), "workspace-token\n").unwrap();
            let config = json!({"schema": "enrichedlab.knowledge-person.v1", "service_url": origin,
                "token_file": dir.join("token"), "commons_scope": "commons"});
            std::fs::write(dir.join("knowledge.json"), config.to_string()).unwrap();
            Arc::new(
                load_optional(Some(&dir.join("knowledge.json")))
                    .unwrap()
                    .unwrap(),
            )
        });
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
            knowledge_person,
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

    async fn call(origin: &str, path: &str, body: Option<Value>, intent: bool) -> (u16, Value) {
        let client = reqwest::Client::new();
        let mut request = match body {
            Some(body) => client.post(format!("{origin}{path}")).json(&body),
            None => client.get(format!("{origin}{path}")),
        };
        request = request.header(ASSERTION_HEADER, "signed.assertion.value");
        if intent {
            request = request.header(MUTATION_INTENT, "mutate");
        }
        let response = request.send().await.unwrap();
        let status = response.status().as_u16();
        (status, response.json().await.unwrap_or(Value::Null))
    }

    fn resource(id: &str, scope: &str, kind: &str, baseline: bool) -> Value {
        json!({"resource_id": id, "kind": kind, "scope": scope, "baseline": baseline, "owner": "o",
            "title": "A record", "summary": null, "data_class": "public", "rights": "open", "allowed_use": [],
            "source_uri": null, "source_revision": null, "content_sha256": null, "lifecycle": "active",
            "created_at": "2026-09-28T00:00:00Z", "locator": null})
    }

    #[tokio::test]
    async fn a_commons_page_too_large_for_the_limit_is_asked_for_again_at_half_the_size() {
        let _ = rustls::crypto::ring::default_provider().install_default();
        let dir = tempfile::tempdir().unwrap();
        let oversized = json!({"resources": [{"summary": "x".repeat(600 * 1024)}], "next": null});
        let fitting = json!({"resources": [resource("01920000-0000-7000-8000-000000000001", "commons", "dataset", false)],
            "next": "cursor-after-the-first"});
        let (origin, seen) = stand_in(vec![(200, oversized), (200, fitting.clone())]).await;
        let knowledge = state(
            NO_DATABASE,
            identity(PrincipalClass::Person, "rich@example.invalid"),
            Some((dir.path(), &origin)),
        )
        .knowledge_person
        .unwrap();
        let (status, value) = commons_page(&knowledge, "dataset", None, "signed.assertion.value")
            .await
            .unwrap();
        assert_eq!((status, value), (StatusCode::OK, fitting));
        let asked: Vec<String> = seen.lock().unwrap().iter().map(|r| r.0.clone()).collect();
        assert_eq!(asked.len(), 2, "{asked:?}");
        assert!(
            asked[0].contains("limit=100") && asked[1].contains("limit=50"),
            "{asked:?}"
        );
    }

    /// No database answers here, so a route that reached the person's principal lookup would fail with the store
    /// unavailable; each case below must refuse before that, and none may reach the knowledge service.
    const NO_DATABASE: &str = "host=127.0.0.1 port=9 user=nobody dbname=none connect_timeout=1";

    #[tokio::test]
    async fn only_a_signed_in_person_reaches_the_knowledge_service() {
        let _ = rustls::crypto::ring::default_provider().install_default();
        let dir = tempfile::tempdir().unwrap();
        let (knowledge, seen) = stand_in(vec![]).await;
        let workload = serve(state(
            NO_DATABASE,
            identity(PrincipalClass::ServiceAccount, "workload@example.invalid"),
            Some((dir.path(), &knowledge)),
        ))
        .await;
        let resource = "01920000-0000-7000-8000-000000000001";
        let baseline = format!("/api/v1/knowledge/resources/{resource}/baseline");
        for (path, body) in [
            ("/api/v1/knowledge/commons", None),
            ("/api/v1/knowledge/commons/collections", None),
            (baseline.as_str(), Some(json!({"baseline": true}))),
            (
                "/api/v1/knowledge/commons/collections/baseline",
                Some(json!({"prefix": "literature://papers/", "baseline": true})),
            ),
        ] {
            let (status, value) = call(&workload, path, body, true).await;
            assert_eq!(
                (status, value["error"]["code"].clone()),
                (403, json!("knowledge_person_required")),
                "{path}"
            );
        }
        let person = identity(PrincipalClass::Person, "researcher@example.invalid");
        let signed_in = serve(state(
            NO_DATABASE,
            person.clone(),
            Some((dir.path(), &knowledge)),
        ))
        .await;
        let (status, value) = call(
            &signed_in,
            &baseline,
            Some(json!({"baseline": true})),
            false,
        )
        .await;
        assert_eq!(
            (status, value["error"]["code"].clone()),
            (403, json!("mutation_intent_required"))
        );
        let review = "/api/v1/knowledge/relations/01920000-0000-7000-8000-00000000e001/review";
        let (status, value) = call(&signed_in, review, Some(json!({"accept": true})), false).await;
        assert_eq!(
            (status, value["error"]["code"].clone()),
            (403, json!("mutation_intent_required"))
        );
        let long = "x".repeat(501);
        let (status, value) = call(
            &signed_in,
            review,
            Some(json!({"accept": false, "note": long})),
            true,
        )
        .await;
        assert_eq!(
            (status, value["error"]["code"].clone()),
            (400, json!("knowledge_note_invalid"))
        );
        let (status, _) = call(
            &signed_in,
            "/api/v1/knowledge/labs/Lab%20A/proposals",
            None,
            false,
        )
        .await;
        assert_eq!(status, 404);
        let (status, value) = call(
            &signed_in,
            "/api/v1/knowledge/commons?kind=section",
            None,
            false,
        )
        .await;
        assert_eq!(
            (status, value["error"]["code"].clone()),
            (400, json!("knowledge_kind_invalid"))
        );
        for (path, body, code) in [
            (
                "/api/v1/knowledge/commons?after=not-a-cursor",
                None,
                "knowledge_cursor_invalid",
            ),
            (
                "/api/v1/knowledge/commons/collections/baseline",
                Some(json!({"prefix": "literature://", "baseline": true})),
                "knowledge_collection_invalid",
            ),
            (
                "/api/v1/knowledge/commons/collections/baseline",
                Some(json!({"prefix": "Literature://papers/", "baseline": true})),
                "knowledge_collection_invalid",
            ),
        ] {
            let (status, value) = call(&signed_in, path, body, true).await;
            assert_eq!(
                (status, value["error"]["code"].clone()),
                (400, json!(code)),
                "{path}"
            );
        }
        let unconfigured = serve(state(NO_DATABASE, person, None)).await;
        let (status, value) = call(&unconfigured, "/api/v1/knowledge/commons", None, false).await;
        assert_eq!(
            (status, value["error"]["code"].clone()),
            (409, json!("knowledge_not_configured"))
        );
        // A person the workspace cannot look up is not forwarded either.
        let (status, _) = call(&signed_in, "/api/v1/knowledge/commons", None, false).await;
        assert_eq!(status, 503);
        assert!(
            seen.lock().unwrap().is_empty(),
            "no request may reach the service"
        );
    }

    #[test]
    fn answers_carry_only_known_codes_and_the_listed_fields() {
        let refused = |status, code: &str| {
            let error = refusal(
                reqwest::StatusCode::from_u16(status).unwrap(),
                &json!({"error": {"code": code}}),
            );
            (error.status.as_u16(), error.code)
        };
        assert_eq!(refused(403, "baseline_refused"), (403, "baseline_refused"));
        assert_eq!(
            refused(404, "resource_not_found"),
            (404, "resource_not_found")
        );
        assert_eq!(
            refused(400, "baseline_not_active"),
            (409, "baseline_not_active")
        );
        assert_eq!(
            refused(401, "person_assertion_invalid"),
            (403, "person_assertion_invalid")
        );
        // The workspace's own token or forwarder role refused is a fault between the services, not the person's.
        assert_eq!(
            refused(401, "unauthenticated"),
            (502, "knowledge_unauthenticated")
        );
        assert_eq!(
            refused(403, "forwarder_not_allowed"),
            (502, "forwarder_not_allowed")
        );
        assert_eq!(refused(400, "<b>injected</b>"), (502, "knowledge_refused"));
        assert_eq!(
            refused(403, "forwarder_not_active"),
            (502, "forwarder_not_active"),
            "a disabled forwarder is the site's fault, not the person's"
        );
        assert_eq!(
            refused(401, "person_sign_in_stale"),
            (403, "person_sign_in_stale")
        );
        assert_eq!(
            refused(400, "collection_invalid"),
            (400, "collection_invalid")
        );
        // A server fault after a change was sent leaves its outcome unknown; the sign-in keys fail before any change.
        let change = |status, code: &str| {
            let error = change_refusal(
                reqwest::StatusCode::from_u16(status).unwrap(),
                &json!({"error": {"code": code}}),
            );
            (error.status.as_u16(), error.code)
        };
        assert_eq!(
            change(500, "store_error"),
            (504, "knowledge_outcome_unknown")
        );
        assert_eq!(
            change(503, "person_keys_unavailable"),
            (503, "person_keys_unavailable")
        );
        assert_eq!(change(403, "baseline_refused"), (403, "baseline_refused"));
        let next = "1759111111123456.01920000-0000-7000-8000-000000000009";
        let listed = json!({"resources": [resource("a", "commons", "dataset", false)], "next": next,
            "parts_waiting": {}});
        let shown = commons_records(&listed, "commons", "dataset").unwrap();
        assert_eq!(shown["records"].as_array().unwrap().len(), 1);
        assert_eq!(shown["records"][0]["resource_id"], "a");
        assert!(
            shown["records"][0].get("owner").is_none()
                && shown["records"][0].get("locator").is_none()
                && shown["records"][0].get("parts_waiting").is_none()
        );
        assert_eq!(
            (shown["complete"].clone(), shown["next"].clone()),
            (json!(false), json!(next))
        );
        let papers = json!({"resources": [resource("p", "commons", "paper", true), resource("q", "commons", "paper", false)],
            "next": null, "parts_waiting": {"p": 2}});
        let shown = commons_records(&papers, "commons", "paper").unwrap();
        assert_eq!(
            (
                shown["records"][0]["parts_waiting"].clone(),
                shown["records"][1]["parts_waiting"].clone(),
                shown["complete"].clone()
            ),
            (json!(2), json!(0), json!(true))
        );
        let mut wrong = resource("a", "commons", "dataset", false);
        wrong["baseline"] = json!("yes");
        for bad in [
            json!({"resources": [wrong], "next": null}),
            json!({"records": []}),
            json!({"resources": [resource("b", "lab-a", "dataset", false)], "next": null}),
            json!({"resources": [resource("c", "commons", "paper", true)], "next": null}),
            json!({"resources": [], "next": "../../v1/grants"}),
        ] {
            assert!(
                commons_records(&bad, "commons", "dataset").is_err(),
                "{bad}"
            );
        }
        let one = json!({"prefix": "literature://papers/", "kinds": {"paper": {"active": 811, "marked": 0},
            "dataset": {"active": 126, "marked": 0}}, "parts_waiting": 0});
        assert_eq!(collection(&one).unwrap(), one);
        for (field, value) in [
            ("prefix", json!("javascript:alert(1)")),
            ("kinds", json!({"paper": {"active": 1, "marked": 2}})),
            ("kinds", json!({"<b>": {"active": 1, "marked": 0}})),
            ("parts_waiting", json!(-1)),
        ] {
            let mut bad = one.clone();
            bad[field] = value;
            assert!(collection(&bad).is_err(), "{field}");
        }
        assert!(
            cursor_shaped(next)
                && !cursor_shaped("1.2")
                && !cursor_shaped(".01920000-0000-7000-8000-000000000009")
        );
        assert!(
            prefix_shaped("literature://papers/") && prefix_shaped("literature://papers/paper/")
        );
        assert!(
            !prefix_shaped("literature://")
                && !prefix_shaped("literature://papers")
                && !prefix_shaped("://papers/")
        );
        let role = |value: Value| {
            commons_role(&value, "commons").map(|(role, person)| (role.map(str::to_owned), person))
        };
        assert_eq!(
            role(json!({"scope": "commons", "role": "lead", "person": true})).unwrap(),
            (Some("lead".into()), true)
        );
        assert_eq!(
            role(json!({"scope": "commons", "role": null, "person": false})).unwrap(),
            (None, false)
        );
        let listed = json!({"relation_id": "01920000-0000-7000-8000-00000000e001", "kind": "uses_tool",
            "source_id": "01920000-0000-7000-8000-00000000e002", "source_kind": "paper", "source_title": "A paper",
            "target_id": "01920000-0000-7000-8000-00000000e003", "target_kind": "tool", "target_title": "A tool",
            "evidence": {"source": "mention", "terms": ["bootstrap"], "sections": ["x"], "section_count": 3},
            "created_by": "01920000-0000-7000-8000-00000000e004", "created_at": "2026-09-28T05:00:00Z"});
        let shown = proposal(&listed).unwrap();
        assert_eq!(
            (
                shown["terms"].clone(),
                shown["sections"].clone(),
                shown["target"]["title"].clone()
            ),
            (json!(["bootstrap"]), json!(3), json!("A tool"))
        );
        assert!(shown.get("created_by").is_none() && shown["source"].get("scope").is_none());
        for (field, value) in [
            ("relation_id", json!("not-a-uuid")),
            ("target_title", json!(7)),
            ("evidence", json!({"terms": "bootstrap"})),
        ] {
            let mut bad = listed.clone();
            bad[field] = value;
            assert!(proposal(&bad).is_err(), "{field}");
        }
        for bad in [
            json!({"scope": "commons", "role": "owner", "person": true}),
            json!({"scope": "commons", "role": "lead"}),
            json!({"scope": "lab-a", "role": "lead", "person": true}),
        ] {
            assert!(role(bad.clone()).is_err(), "{bad}");
        }
    }

    /// The positive path needs the workspace's principal lookup, so it runs against isolated PostgreSQL through
    /// `scripts/qualify_remote_broker.sh knowledge-person`.
    #[tokio::test]
    #[ignore = "isolated PostgreSQL through qualify_remote_broker.sh knowledge-person"]
    async fn a_person_marks_a_commons_record_through_the_workspace() {
        let _ = rustls::crypto::ring::default_provider().install_default();
        let dsn = std::fs::read_to_string(
            std::env::var_os("BROKER_QUALIFICATION_MIGRATION_DSN_FILE").unwrap(),
        )
        .unwrap()
        .replace("user=postgres", "user=broker_api_qual");
        let dir = tempfile::tempdir().unwrap();
        let id = "01920000-0000-7000-8000-00000000d001";
        let marked = |baseline: bool, parts: Option<u64>| {
            let mut value = resource(id, "commons", "dataset", baseline);
            if let Some(parts) = parts {
                value["parts_changed"] = json!(parts);
            }
            value
        };
        let (knowledge, seen) = stand_in(vec![
            (200, json!({"resources": [resource(id, "commons", "dataset", false)], "next": null, "parts_waiting": {}})),
            (200, json!({"scope": "commons", "role": "lead", "person": true})),
            (200, marked(true, Some(0))),
            (403, json!({"error": {"code": "baseline_refused", "message": "Not allowed for this caller"}})),
            (200, marked(false, Some(0))),
            (500, json!({"error": {"code": "store_error", "message": "The record store failed"}})),
            (200, json!({"scope": "commons", "collections": [{"prefix": "literature://papers/",
                "kinds": {"paper": {"active": 2, "marked": 0}}, "parts_waiting": 0}]})),
            (200, json!({"scope": "commons", "role": "lead", "person": true})),
            (200, json!({"prefix": "literature://papers/", "baseline": true, "records_changed": 2, "parts_changed": 9})),
        ])
        .await;
        let person = identity(PrincipalClass::Person, "researcher@example.invalid");
        let workspace = serve(state(&dsn, person, Some((dir.path(), &knowledge)))).await;
        let (status, listed) = call(&workspace, "/api/v1/knowledge/commons", None, false).await;
        assert_eq!(status, 200);
        assert_eq!(listed["records"].as_array().unwrap().len(), 1);
        assert_eq!(
            (listed["role"].clone(), listed["can_mark"].clone()),
            (json!("lead"), json!(true))
        );
        let path = format!("/api/v1/knowledge/resources/{id}/baseline");
        let (status, marked) = call(&workspace, &path, Some(json!({"baseline": true})), true).await;
        assert_eq!((status, marked["baseline"].clone()), (200, json!(true)));
        let (status, refused) =
            call(&workspace, &path, Some(json!({"baseline": false})), true).await;
        assert_eq!(
            (status, refused["error"]["code"].clone()),
            (403, json!("baseline_refused"))
        );
        // A success about another record, or with the other mark, leaves the change's extent unknown, and so does a
        // server fault after the change was sent.
        for _ in 0..2 {
            let (status, value) =
                call(&workspace, &path, Some(json!({"baseline": true})), true).await;
            assert_eq!(
                (status, value["error"]["code"].clone()),
                (504, json!("knowledge_outcome_unknown"))
            );
        }
        let (status, collections) = call(
            &workspace,
            "/api/v1/knowledge/commons/collections",
            None,
            false,
        )
        .await;
        assert_eq!(
            (
                status,
                collections["collections"][0]["prefix"].clone(),
                collections["can_mark"].clone()
            ),
            (200, json!("literature://papers/"), json!(true))
        );
        let (status, whole) = call(
            &workspace,
            "/api/v1/knowledge/commons/collections/baseline",
            Some(json!({"prefix": "literature://papers/", "baseline": true})),
            true,
        )
        .await;
        assert_eq!(
            (
                status,
                whole["records_changed"].clone(),
                whole["parts_changed"].clone()
            ),
            (200, json!(2), json!(9))
        );
        let log = seen.lock().unwrap().clone();
        assert_eq!(log.len(), 9);
        assert_eq!(
            log[0].0,
            "GET /v1/resources?kind=dataset&scope=commons&limit=100"
        );
        assert_eq!(log[6].0, "GET /v1/scopes/commons/collections");
        assert_eq!(
            (log[8].0.clone(), log[8].3.clone(), log[8].4.clone()),
            (
                "POST /v1/collections/baseline".into(),
                Some("mutate".into()),
                json!({"prefix": "literature://papers/", "baseline": true})
            )
        );
        assert_eq!(log[1].0, "GET /v1/scopes/commons/role");
        assert_eq!(log[2].0, format!("POST /v1/resources/{id}/baseline"));
        for (request, authorization, assertion, _, _) in &log {
            assert_eq!(authorization, "Bearer workspace-token", "{request}");
            assert_eq!(
                assertion.as_deref(),
                Some("signed.assertion.value"),
                "{request}"
            );
        }
        assert_eq!(
            (log[0].3.clone(), log[1].3.clone(), log[2].3.clone()),
            (None, None, Some("mutate".into()))
        );
        assert_eq!(
            (log[2].4.clone(), log[3].4.clone()),
            (json!({"baseline": true}), json!({"baseline": false}))
        );
        // A person the workspace has not enrolled is refused by the workspace and never forwarded.
        let outsider = serve(state(
            &dsn,
            identity(PrincipalClass::Person, "outsider@example.invalid"),
            Some((dir.path(), &knowledge)),
        ))
        .await;
        let (status, _) = call(&outsider, "/api/v1/knowledge/commons", None, false).await;
        assert_eq!(status, 403);
        assert_eq!(seen.lock().unwrap().len(), 9);
    }

    /// A lab's proposed links and a person's reviews, through the workspace, against isolated PostgreSQL for the
    /// workspace's principal lookup.
    #[tokio::test]
    #[ignore = "isolated PostgreSQL through qualify_remote_broker.sh knowledge-person"]
    async fn a_person_reviews_a_lab_link_through_the_workspace() {
        let _ = rustls::crypto::ring::default_provider().install_default();
        let dsn = std::fs::read_to_string(
            std::env::var_os("BROKER_QUALIFICATION_MIGRATION_DSN_FILE").unwrap(),
        )
        .unwrap()
        .replace("user=postgres", "user=broker_api_qual");
        let dir = tempfile::tempdir().unwrap();
        let link = "01920000-0000-7000-8000-00000000e001";
        let listed = json!({"relation_id": link, "kind": "uses_tool",
            "source_id": "01920000-0000-7000-8000-00000000e002", "source_kind": "paper", "source_title": "A paper",
            "target_id": "01920000-0000-7000-8000-00000000e003", "target_kind": "tool", "target_title": "A tool",
            "evidence": {"source": "mention", "terms": ["bootstrap"], "section_count": 2},
            "created_by": "01920000-0000-7000-8000-00000000e004", "created_at": "2026-09-28T05:00:00Z"});
        let (knowledge, seen) = stand_in(vec![
            (
                200,
                json!({"scope": "pilot-lab", "proposals": [listed], "complete": true}),
            ),
            (
                200,
                json!({"scope": "pilot-lab", "role": "curator", "person": true}),
            ),
            (200, json!({"relation_id": link, "assertion": "reviewed"})),
            (204, Value::Null),
            (
                404,
                json!({"error": {"code": "relation_not_found", "message": "Not found"}}),
            ),
            (200, json!({"relation_id": link, "assertion": "proposed"})),
        ])
        .await;
        let person = identity(PrincipalClass::Person, "researcher@example.invalid");
        let workspace = serve(state(&dsn, person, Some((dir.path(), &knowledge)))).await;
        let (status, queue) = call(
            &workspace,
            "/api/v1/knowledge/labs/pilot-lab/proposals",
            None,
            false,
        )
        .await;
        assert_eq!(status, 200, "{queue}");
        assert_eq!(
            (
                queue["proposals"][0]["source"]["title"].clone(),
                queue["can_review"].clone(),
                queue["role"].clone()
            ),
            (json!("A paper"), json!(true), json!("curator"))
        );
        let path = format!("/api/v1/knowledge/relations/{link}/review");
        let (status, accepted) = call(
            &workspace,
            &path,
            Some(json!({"accept": true, "note": " Checked. "})),
            true,
        )
        .await;
        assert_eq!(
            (status, accepted["outcome"].clone()),
            (200, json!("accepted"))
        );
        let (status, rejected) =
            call(&workspace, &path, Some(json!({"accept": false})), true).await;
        assert_eq!(
            (status, rejected["outcome"].clone()),
            (200, json!("rejected"))
        );
        let (status, refused) = call(&workspace, &path, Some(json!({"accept": false})), true).await;
        assert_eq!(
            (status, refused["error"]["code"].clone()),
            (404, json!("relation_not_found"))
        );
        // An acceptance the service answered but did not show as reviewed leaves the outcome unknown.
        let (status, odd) = call(&workspace, &path, Some(json!({"accept": true})), true).await;
        assert_eq!(
            (status, odd["error"]["code"].clone()),
            (504, json!("knowledge_outcome_unknown"))
        );
        let log = seen.lock().unwrap().clone();
        assert_eq!(log[0].0, "GET /v1/scopes/pilot-lab/proposals?limit=100");
        assert_eq!(log[1].0, "GET /v1/scopes/pilot-lab/role");
        assert_eq!(log[2].0, format!("POST /v1/relations/{link}/review"));
        assert_eq!(
            (log[2].3.clone(), log[2].4.clone(), log[3].4.clone()),
            (
                Some("mutate".into()),
                json!({"accept": true, "note": "Checked."}),
                json!({"accept": false, "note": null})
            )
        );
        for (request, authorization, assertion, _, _) in &log {
            assert_eq!(authorization, "Bearer workspace-token", "{request}");
            assert_eq!(
                assertion.as_deref(),
                Some("signed.assertion.value"),
                "{request}"
            );
        }
    }
}
