//! Lab datasets (migration 059). A person sends a table the lab keeps; the upload
//! guard decides on it, and the broker records the decision with the dataset and, for an allowed file, its intake task.
//! A person approves a preparation of a profiled dataset. Readers list a project's datasets and read one in full. The
//! API never keeps the file: its bytes go to the guard, and only their digest, size and name stay.
use super::*;
use crate::broker::{
    dataset_prepare::{Spec, SpecError},
    guard_client::{GuardClient, GuardError, GuardOutcome, Upload},
    store::{DatasetRecord, PreparationRequest},
};
use axum::body::Body;
use futures_util::StreamExt;
use serde_json::Value;
use std::time::Duration;

/// The guard's limit for the use case `lab-dataset`.
pub(super) const MAX_DATASET_BYTES: usize = 32 * 1024 * 1024;
/// The file's name, percent-encoded UTF-8, as a browser's file picker gives it.
const DATASET_NAME: &str = "x-dataset-name";
const MAX_NAME_CHARS: usize = 255;
const MAX_TITLE_CHARS: usize = 200;
const PERMIT_WAIT: Duration = Duration::from_secs(30);
const BODY_WAIT: Duration = Duration::from_secs(300);

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct UploadQuery {
    request_id: Uuid,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct PreparationBody {
    spec: Value,
    #[serde(default)]
    title: String,
    #[serde(default)]
    previous: Option<Uuid>,
    request_id: Uuid,
}

/// The guard caller for lab datasets: its configuration must name the use case `lab-dataset`, whose policy reads
/// tables, so a caller set up for another use case never sends a lab's file under that one's policy.
pub(super) fn load_guard(path: &std::path::Path) -> anyhow::Result<GuardClient> {
    let guard = GuardClient::load(path)?;
    anyhow::ensure!(
        guard.use_case() == "lab-dataset",
        "the dataset guard caller must use the use case lab-dataset"
    );
    Ok(guard)
}

fn refusal(status: StatusCode, code: &'static str, message: &'static str) -> ApiError {
    ApiError {
        status,
        code,
        message,
    }
}

/// The media type the guard takes for a file name's extension; none for a file it does not take as a table. A text
/// table sent under a workbook's name is still read as text by the guard.
pub(super) fn media_type_for_name(name: &str) -> Option<&'static str> {
    let extension = name.rsplit_once('.')?.1.to_ascii_lowercase();
    Some(match extension.as_str() {
        "csv" => "text/csv",
        "tsv" | "tab" | "txt" | "dat" => "text/tab-separated-values",
        "xlsx" => "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet",
        "xlsm" => "application/vnd.ms-excel.sheet.macroenabled.12",
        "xls" => "application/vnd.ms-excel",
        _ => return None,
    })
}

fn percent_decoded(text: &str) -> Option<String> {
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' {
            let hex = std::str::from_utf8(bytes.get(i + 1..i + 3)?).ok()?;
            out.push(u8::from_str_radix(hex, 16).ok()?);
            i += 3;
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    String::from_utf8(out).ok()
}

/// The file's name from `X-Dataset-Name`: one path component, without control or direction characters.
fn file_name(headers: &HeaderMap) -> Result<String, ApiError> {
    let invalid = || {
        ApiError::invalid(
            "dataset_name_invalid",
            "The file needs a name of at most 255 characters",
        )
    };
    let raw = headers
        .get(DATASET_NAME)
        .and_then(|v| v.to_str().ok())
        .ok_or_else(invalid)?;
    let decoded = percent_decoded(raw).ok_or_else(invalid)?;
    if decoded
        .chars()
        .any(|c| c.is_control() || crate::broker::live_output::is_bidi_control(c))
    {
        return Err(invalid());
    }
    let name = decoded.trim();
    if name.is_empty()
        || name.chars().count() > MAX_NAME_CHARS
        || name.contains(['/', '\\'])
        || name == "."
        || name == ".."
    {
        return Err(invalid());
    }
    Ok(name.to_owned())
}

fn guard_error(error: GuardError) -> ApiError {
    match error {
        GuardError::Unavailable => refusal(
            StatusCode::SERVICE_UNAVAILABLE,
            "dataset_guard_unavailable",
            "The upload check is not available now; try again later",
        ),
        GuardError::Busy => refusal(
            StatusCode::TOO_MANY_REQUESTS,
            "dataset_guard_busy",
            "The upload check is busy; try again in a minute",
        ),
        GuardError::Quota => refusal(
            StatusCode::TOO_MANY_REQUESTS,
            "dataset_upload_quota",
            "Today's upload allowance is used; try again tomorrow",
        ),
        GuardError::TooLarge => refusal(
            StatusCode::PAYLOAD_TOO_LARGE,
            "dataset_too_large",
            "A dataset may be at most 32 MiB",
        ),
    }
}

async fn read_body(body: Body, limit: usize) -> Result<Vec<u8>, ApiError> {
    let mut stream = body.into_data_stream();
    let mut out = Vec::new();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|_| {
            ApiError::invalid("dataset_incomplete", "The file did not arrive whole")
        })?;
        if out.len() + chunk.len() > limit {
            return Err(refusal(
                StatusCode::PAYLOAD_TOO_LARGE,
                "dataset_too_large",
                "A dataset may be at most 32 MiB",
            ));
        }
        out.extend_from_slice(&chunk);
    }
    Ok(out)
}

/// `POST /api/v1/projects/{project}/datasets?request_id=`: the file is the body and its name is `X-Dataset-Name`. No
/// body is read before the caller may contribute, the lab has its dataset method and the upload holds its weight of
/// the API's upload memory. The guard's decision is recorded whatever it is; a held or blocked file has no intake, and
/// an allowed file's intake waits (`intake_waiting`) when the lab's queue is full. The same request id with the same
/// file under the same name returns the same dataset, and starts an intake that waited, without sending the file to
/// the guard again; another file under that id is refused.
pub(super) async fn upload(
    State(state): State<ApiState>,
    headers: HeaderMap,
    AxumPath(project): AxumPath<Uuid>,
    Query(query): Query<UploadQuery>,
    body: Body,
) -> Result<Response, ApiError> {
    require_mutation_intent(&headers)?;
    let name = file_name(&headers)?;
    let media_type = media_type_for_name(&name).ok_or_else(|| {
        refusal(
            StatusCode::UNSUPPORTED_MEDIA_TYPE,
            "dataset_type_unsupported",
            "Send a CSV or tab-separated text file or an Excel workbook (.xlsx)",
        )
    })?;
    let actor = authenticate(&state, &headers).await?;
    let check = state
        .store
        .dataset_upload_check(actor, project, query.request_id)
        .await?;
    let existing = check.get("existing").cloned();
    let guard = match existing {
        Some(_) => None,
        None => Some(state.dataset_guard.clone().ok_or_else(|| {
            refusal(
                StatusCode::SERVICE_UNAVAILABLE,
                "dataset_guard_unconfigured",
                "Lab datasets are not set up on this server",
            )
        })?),
    };
    let Some(length) = headers
        .get(header::CONTENT_LENGTH)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.parse::<usize>().ok())
    else {
        return Err(refusal(
            StatusCode::LENGTH_REQUIRED,
            "dataset_length_required",
            "The upload must state its length",
        ));
    };
    if length == 0 {
        return Err(ApiError::invalid("dataset_empty", "The file is empty"));
    }
    if length > MAX_DATASET_BYTES {
        return Err(refusal(
            StatusCode::PAYLOAD_TOO_LARGE,
            "dataset_too_large",
            "A dataset may be at most 32 MiB",
        ));
    }
    let weight = u32::try_from(length.max(1024 * 1024).div_ceil(1024)).unwrap_or(u32::MAX);
    let _permit = tokio::time::timeout(PERMIT_WAIT, state.library_permits.acquire_many(weight))
        .await
        .map_err(|_| {
            refusal(
                StatusCode::TOO_MANY_REQUESTS,
                "dataset_upload_busy",
                "Other uploads are using the server's room; try again in a minute",
            )
        })?
        .map_err(|_| ApiError::internal())?;
    let body = tokio::time::timeout(BODY_WAIT, read_body(body, length))
        .await
        .map_err(|_| ApiError::invalid("dataset_incomplete", "The file did not arrive whole"))??;
    if body.len() != length {
        return Err(ApiError::invalid(
            "dataset_incomplete",
            "The file did not arrive whole",
        ));
    }
    let sha256 = hex::encode(Sha256::digest(&body));
    if let Some(existing) = existing {
        return retried(
            &state,
            actor,
            project,
            existing,
            (&name, media_type, &sha256),
            length,
        )
        .await;
    }
    let (Some(guard), Some(lab)) = (guard, check["group"].as_str()) else {
        return Err(ApiError::internal());
    };
    let lab = lab.to_owned();
    let person = actor.to_string();
    let project_text = project.to_string();
    let upload = Upload {
        media_type,
        lab: &lab,
        person: &person,
        project: Some(&project_text),
        file_name: Some(&name),
    };
    let verdict = guard.check(&upload, body).await.map_err(guard_error)?;
    let findings = json!(verdict.findings);
    let grid = match (verdict.outcome, &verdict.rebuilt) {
        (GuardOutcome::Allowed, Some((sha, size))) => Some((sha.as_str(), *size as i64)),
        (GuardOutcome::Allowed, None) => return Err(guard_error(GuardError::Unavailable)),
        _ => None,
    };
    let dataset = state
        .store
        .record_dataset(
            actor,
            &DatasetRecord {
                project,
                request_id: query.request_id,
                file_name: &name,
                media_type,
                size_bytes: length as i64,
                sha256: &sha256,
                guard_intake: verdict.intake,
                outcome: verdict.outcome.as_str(),
                findings: &findings,
                policy: &verdict.policy,
                grid,
            },
        )
        .await?;
    Ok((
        StatusCode::CREATED,
        Json(json!({"dataset": dataset, "created": true})),
    )
        .into_response())
}

/// A retry under a used request id: the same file under the same name, compared with what was recorded and not sent to
/// the guard again. It starts an intake that waited for room; while the queue is still full the dataset keeps waiting.
async fn retried(
    state: &ApiState,
    actor: Uuid,
    project: Uuid,
    existing: Value,
    (name, media_type, sha256): (&str, &str, &str),
    length: usize,
) -> Result<Response, ApiError> {
    let same = existing["file_name"] == name
        && existing["media_type"] == media_type
        && existing["size_bytes"].as_u64() == Some(length as u64)
        && existing["sha256"] == sha256;
    if !same {
        return Err(refusal(
            StatusCode::CONFLICT,
            "idempotency_conflict",
            "This request id was used for another file",
        ));
    }
    let dataset = if existing["intake_waiting"] == true {
        let id = existing["dataset_id"]
            .as_str()
            .and_then(|id| Uuid::parse_str(id).ok())
            .ok_or_else(ApiError::internal)?;
        match state.store.start_dataset_intake(actor, project, id).await {
            Ok(dataset) => dataset,
            Err(error) if error.code == "group_queue_full" => existing,
            Err(error) => return Err(error.into()),
        }
    } else {
        existing
    };
    Ok((
        StatusCode::OK,
        Json(json!({"dataset": dataset, "created": false})),
    )
        .into_response())
}

/// `POST /api/v1/projects/{project}/datasets/{dataset}/intake`: a person starts the intake of an allowed dataset that
/// waited for room in the lab's queue. A dataset whose intake has started is returned as it is.
pub(super) async fn start_intake(
    State(state): State<ApiState>,
    headers: HeaderMap,
    AxumPath((project, dataset)): AxumPath<(Uuid, Uuid)>,
) -> Result<impl IntoResponse, ApiError> {
    require_mutation_intent(&headers)?;
    let actor = authenticate(&state, &headers).await?;
    Ok(Json(json!({
        "dataset": state.store.start_dataset_intake(actor, project, dataset).await?
    })))
}

/// The project's datasets, newest first, each with its intake and preparations.
pub(super) async fn list(
    State(state): State<ApiState>,
    headers: HeaderMap,
    AxumPath(project): AxumPath<Uuid>,
) -> Result<impl IntoResponse, ApiError> {
    let actor = authenticate(&state, &headers).await?;
    Ok(Json(
        json!({"datasets": state.store.project_datasets(actor, project).await?}),
    ))
}

/// One dataset with its intake's receipts, including the profile, and each preparation with its specification and
/// receipts.
pub(super) async fn detail(
    State(state): State<ApiState>,
    headers: HeaderMap,
    AxumPath((project, dataset)): AxumPath<(Uuid, Uuid)>,
) -> Result<impl IntoResponse, ApiError> {
    let actor = authenticate(&state, &headers).await?;
    Ok(Json(
        json!({"dataset": state.store.dataset_detail(actor, project, dataset).await?}),
    ))
}

fn spec_refusal(error: SpecError) -> Response {
    (
        StatusCode::BAD_REQUEST,
        Json(json!({"error": {"code": "dataset_preparation_invalid",
            "message": "The preparation cannot be followed as written", "path": error.path, "reason": error.reason}})),
    )
        .into_response()
}

/// `POST /api/v1/projects/{project}/datasets/{dataset}/preparations`: a person approves a preparation of a profiled
/// dataset. The broker checks the specification as written before it records it; the worker checks it against the
/// sheet when it runs.
pub(super) async fn prepare(
    State(state): State<ApiState>,
    headers: HeaderMap,
    AxumPath((project, dataset)): AxumPath<(Uuid, Uuid)>,
    Json(body): Json<PreparationBody>,
) -> Result<Response, ApiError> {
    require_mutation_intent(&headers)?;
    let title = body.title.trim();
    if title.chars().count() > MAX_TITLE_CHARS || title.chars().any(char::is_control) {
        return Err(ApiError::invalid(
            "dataset_preparation_invalid",
            "The title may hold at most 200 characters",
        ));
    }
    if let Err(error) = Spec::parse(&body.spec) {
        return Ok(spec_refusal(error));
    }
    let actor = authenticate(&state, &headers).await?;
    let task = state
        .store
        .request_dataset_preparation(
            actor,
            &PreparationRequest {
                project,
                dataset,
                spec: &body.spec,
                title,
                previous: body.previous,
                request_id: body.request_id,
            },
        )
        .await?;
    Ok((StatusCode::ACCEPTED, Json(json!({"task_id": task}))).into_response())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn file_names_choose_the_type_the_guard_reads() {
        for (name, expected) in [
            ("GTT cohort 3.csv", Some("text/csv")),
            ("plate.TSV", Some("text/tab-separated-values")),
            ("export.txt", Some("text/tab-separated-values")),
            (
                "weights.xlsx",
                Some("application/vnd.openxmlformats-officedocument.spreadsheetml.sheet"),
            ),
            (
                "macro.xlsm",
                Some("application/vnd.ms-excel.sheet.macroenabled.12"),
            ),
            ("old.xls", Some("application/vnd.ms-excel")),
            ("paper.pdf", None),
            ("noextension", None),
        ] {
            assert_eq!(media_type_for_name(name), expected, "{name}");
        }
    }

    #[test]
    fn a_file_name_is_one_plain_component() {
        let named = |raw: &str| {
            let mut headers = HeaderMap::new();
            headers.insert(DATASET_NAME, raw.parse().unwrap());
            file_name(&headers)
        };
        assert_eq!(named("GTT%20cohort%203.csv").unwrap(), "GTT cohort 3.csv");
        assert_eq!(named("%C2%B5g.csv").unwrap(), "µg.csv");
        for bad in [
            "..%2Fsecret.csv",
            "a%5Cb.csv",
            "%0Aline.csv",
            "%E2%80%AEcsv.exe",
            "%ZZ.csv",
            "",
            "%FF.csv",
        ] {
            assert!(named(bad).is_err(), "{bad}");
        }
        assert!(named(&format!("{}.csv", "a".repeat(252))).is_err());
        assert!(named(&format!("{}.csv", "a".repeat(251))).is_ok());
        assert!(file_name(&HeaderMap::new()).is_err());
    }

    /// A stand-in for the upload guard's decision: it records what the broker sent and holds a file whose name starts
    /// with `held`.
    #[derive(Clone, Default)]
    struct Seen(Arc<std::sync::Mutex<Vec<Value>>>);

    async fn guard(seen: Seen) -> String {
        async fn intake(
            State(seen): State<Seen>,
            headers: HeaderMap,
            body: axum::body::Bytes,
        ) -> (StatusCode, Json<Value>) {
            let header = |name: &str| {
                headers
                    .get(name)
                    .and_then(|v| v.to_str().ok())
                    .unwrap_or_default()
                    .to_owned()
            };
            if header("authorization") != "Bearer caller-token" {
                return (
                    StatusCode::UNAUTHORIZED,
                    Json(json!({"error": "unauthorized"})),
                );
            }
            seen.0.lock().unwrap().push(json!({"content_type": header("content-type"),
                "use_case": header("x-guard-use-case"), "tenant": header("x-guard-tenant"), "lab": header("x-guard-lab"),
                "person": header("x-guard-person"), "project": header("x-guard-project"),
                "file_name": header("x-guard-file-name"), "bytes": body.len()}));
            let rebuilt = b"a rebuilt grid";
            let decision = if header("x-guard-file-name").starts_with("held") {
                json!({"intake_id": Uuid::new_v4(), "outcome": "quarantined", "policy_version": "policy-1",
                    "findings": [{"code": "yara_medium", "action": "quarantine"}]})
            } else {
                json!({"intake_id": Uuid::new_v4(), "outcome": "allowed", "policy_version": "policy-1",
                    "findings": [{"code": "formulas_without_values", "action": "annotate"},
                        {"code": "checked", "action": "allow"}],
                    "disarmed": {"sha256": hex::encode(Sha256::digest(rebuilt)), "bytes": rebuilt.len()}})
            };
            (StatusCode::OK, Json(decision))
        }
        let app = Router::new()
            .route("/v1/intake", axum::routing::post(intake))
            .with_state(seen);
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        url
    }

    fn private_file(path: &std::path::Path, text: &str) {
        use std::os::unix::fs::PermissionsExt;
        std::fs::write(path, text).unwrap();
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600)).unwrap();
    }

    #[tokio::test]
    #[ignore = "isolated PostgreSQL through qualify_remote_broker.sh lab-datasets"]
    async fn a_person_sends_a_table_and_the_guard_decides_before_anything_is_kept() {
        use super::super::test_support::{api, person, serve};
        use crate::broker::project::{CreateProject, Visibility};
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
                "INSERT INTO enrichedlab_agent.broker_principal_invitations(tenant_id,email,group_key,role)
                   VALUES ('research-site','api-outsider@example.invalid','pilot-lab','researcher') ON CONFLICT DO NOTHING;
                 INSERT INTO enrichedlab_agent.broker_agent_profiles(tenant_id,profile_key,name,description,harness,
                   thinking,route,controller_profile,gateway_profile,image_ref,cpu_limit,memory_limit,state_profile,role,
                   tool_bundle,max_run_seconds,max_task_attempts,max_provider_attempts)
                 SELECT tenant_id,'lab-dataset','Read a lab dataset',description,harness,thinking,'server-dataset',
                   controller_profile,gateway_profile,image_ref,cpu_limit,memory_limit,state_profile,role,tool_bundle,
                   max_run_seconds,1,max_provider_attempts
                 FROM enrichedlab_agent.broker_agent_profiles
                 WHERE tenant_id='research-site' AND profile_key='general-research' ON CONFLICT DO NOTHING;
                 INSERT INTO enrichedlab_agent.broker_group_profile_grants(tenant_id,group_key,profile_key)
                   VALUES ('research-site','pilot-lab','lab-dataset') ON CONFLICT DO NOTHING;",
            )
            .await
            .unwrap();
        let api_dsn = dsn.replace("user=postgres", "user=broker_api_qual");
        let root = tempfile::tempdir().unwrap();
        let me = person("owner", "researcher@example.invalid");
        let mut state = api(&api_dsn, me.clone(), root.path());
        state.library_permits = Arc::new(tokio::sync::Semaphore::new(
            super::super::library::UPLOAD_BUDGET_KIB,
        ));
        let actor = state.store.resolve_principal(&me).await.unwrap();
        let project = Uuid::new_v4();
        state
            .store
            .create_project(
                actor,
                &CreateProject {
                    project_id: project,
                    group: "pilot-lab".into(),
                    title: "Dataset uploads".into(),
                    visibility: Visibility::Private,
                },
            )
            .await
            .unwrap();

        // The caller must be set up for lab datasets.
        let seen = Seen::default();
        let guard_url = guard(seen.clone()).await;
        private_file(&root.path().join("caller.token"), "caller-token\n");
        for (name, use_case) in [
            ("paper.json", "library-paper"),
            ("caller.json", "lab-dataset"),
        ] {
            private_file(
                &root.path().join(name),
                &json!({"schema": "enrichedlab.guard-caller.v1", "service_url": guard_url,
                    "token_file": root.path().join("caller.token"), "use_case": use_case,
                    "tenant": "research-site"})
                .to_string(),
            );
        }
        assert!(load_guard(&root.path().join("paper.json")).is_err());
        state.dataset_guard = Some(Arc::new(
            load_guard(&root.path().join("caller.json")).unwrap(),
        ));
        let origin = serve(state.clone()).await;
        let client = reqwest::Client::new();
        let send = |origin: &str, name: &str, body: Vec<u8>, request: Uuid, intent: bool| {
            let mut sent = client
                .post(format!(
                    "{origin}/api/v1/projects/{project}/datasets?request_id={request}"
                ))
                .header(DATASET_NAME, name)
                // A browser states the length of every file it sends, an empty one included.
                .header(header::CONTENT_LENGTH, body.len())
                .body(body);
            if intent {
                sent = sent.header(MUTATION_INTENT, "mutate");
            }
            sent.send()
        };
        let error = |body: &Value| {
            body["error"]["code"]
                .as_str()
                .unwrap_or_default()
                .to_owned()
        };

        // A table is sent to the guard with its lab, person, project and name, and recorded with the guard's decision.
        let table = b"Mouse ID,Glucose\nM-1,142\n".to_vec();
        let first = Uuid::new_v4();
        let response = send(&origin, "GTT%20cohort%203.csv", table.clone(), first, true)
            .await
            .unwrap();
        assert_eq!(response.status(), 201);
        let body: Value = response.json().await.unwrap();
        let dataset = body["dataset"].clone();
        assert_eq!(body["created"], true);
        assert_eq!(
            (
                &dataset["file_name"],
                &dataset["media_type"],
                &dataset["size_bytes"],
                &dataset["sha256"]
            ),
            (
                &json!("GTT cohort 3.csv"),
                &json!("text/csv"),
                &json!(table.len()),
                &json!(hex::encode(Sha256::digest(&table)))
            )
        );
        assert_eq!(
            dataset["guard"]["findings"],
            json!(["formulas_without_values"])
        );
        assert!(dataset["intake_task"].is_string());
        assert_eq!(
            seen.0.lock().unwrap().clone(),
            [
                json!({"content_type": "text/csv", "use_case": "lab-dataset", "tenant": "research-site",
                "lab": "pilot-lab", "person": actor.to_string(), "project": project.to_string(),
                "file_name": "GTT%20cohort%203.csv", "bytes": table.len()})
            ]
        );
        // The same request again answers the same dataset without sending the file again.
        let response = send(&origin, "GTT%20cohort%203.csv", table.clone(), first, true)
            .await
            .unwrap();
        assert_eq!(response.status(), 200);
        let body: Value = response.json().await.unwrap();
        assert_eq!(
            (&body["created"], &body["dataset"]),
            (&json!(false), &dataset)
        );
        // A retry is compared with what was recorded: another file, or the same file under another name, is refused
        // under a used request id, and neither reaches the guard.
        for (name, body) in [
            (
                "GTT%20cohort%203.csv",
                b"Mouse ID,Glucose\nM-1,143\n".to_vec(),
            ),
            ("GTT%20cohort%204.csv", table.clone()),
        ] {
            let response = send(&origin, name, body, first, true).await.unwrap();
            assert_eq!(response.status(), 409, "{name}");
            assert_eq!(
                error(&response.json().await.unwrap()),
                "idempotency_conflict",
                "{name}"
            );
        }

        // What is not a table, has no intent, names a path or is empty is refused before the guard sees it.
        for (name, body, intent, status, code) in [
            (
                "paper.pdf",
                table.clone(),
                true,
                415,
                "dataset_type_unsupported",
            ),
            (
                "GTT.csv",
                table.clone(),
                false,
                403,
                "mutation_intent_required",
            ),
            (
                "..%2Fescape.csv",
                table.clone(),
                true,
                400,
                "dataset_name_invalid",
            ),
            ("empty.csv", Vec::new(), true, 400, "dataset_empty"),
        ] {
            let response = send(&origin, name, body, Uuid::new_v4(), intent)
                .await
                .unwrap();
            assert_eq!(response.status(), status, "{name}");
            assert_eq!(error(&response.json().await.unwrap()), code, "{name}");
        }
        assert_eq!(seen.0.lock().unwrap().len(), 1);

        // A file the guard holds is recorded with its finding and has no intake.
        let response = send(
            &origin,
            "held%20plate.csv",
            table.clone(),
            Uuid::new_v4(),
            true,
        )
        .await
        .unwrap();
        assert_eq!(response.status(), 201);
        let held: Value = response.json().await.unwrap();
        assert_eq!(
            (
                &held["dataset"]["guard"]["outcome"],
                &held["dataset"]["guard"]["findings"],
                &held["dataset"]["intake_task"]
            ),
            (&json!("quarantined"), &json!(["yara_medium"]), &Value::Null)
        );

        // Readers see the project's datasets, newest first; a person outside the project sees nothing.
        let read = |origin: String, path: String| {
            let client = client.clone();
            async move {
                let response = client.get(format!("{origin}{path}")).send().await.unwrap();
                (
                    response.status().as_u16(),
                    response.json::<Value>().await.unwrap(),
                )
            }
        };
        let list = format!("/api/v1/projects/{project}/datasets");
        let (status, listing) = read(origin.clone(), list.clone()).await;
        assert_eq!(status, 200);
        let names: Vec<&str> = listing["datasets"]
            .as_array()
            .unwrap()
            .iter()
            .map(|d| d["file_name"].as_str().unwrap())
            .collect();
        assert_eq!(names, ["held plate.csv", "GTT cohort 3.csv"]);
        let one = format!(
            "/api/v1/projects/{project}/datasets/{}",
            dataset["dataset_id"].as_str().unwrap()
        );
        let (status, detail) = read(origin.clone(), one.clone()).await;
        assert_eq!(
            (status, &detail["dataset"]["dataset_id"]),
            (200, &dataset["dataset_id"])
        );
        let stranger = serve(api(
            &api_dsn,
            person("api-outsider", "api-outsider@example.invalid"),
            root.path(),
        ))
        .await;
        for path in [list, one] {
            let (status, body) = read(stranger.clone(), path).await;
            assert_eq!((status, error(&body)), (404, "project_not_found".into()));
        }

        // A preparation is checked as written before anything is recorded, and waits for the profile.
        let prepare = |spec: Value| {
            client
                .post(format!(
                    "{origin}/api/v1/projects/{project}/datasets/{}/preparations",
                    dataset["dataset_id"].as_str().unwrap()
                ))
                .header(MUTATION_INTENT, "mutate")
                .json(&json!({"spec": spec, "request_id": Uuid::new_v4()}))
                .send()
        };
        let spec = json!({"schema": "enrichedlab.dataset-preparation.v1", "table_name": "Glucose!",
            "sheet": 0, "rows": {"first": 2, "last": 2}, "header_rows": [1], "missing": ["NA"],
            "columns": [{"from": "A", "name": "mouse_id", "type": "text"}]});
        let response = prepare(spec.clone()).await.unwrap();
        assert_eq!(response.status(), 400);
        let body: Value = response.json().await.unwrap();
        assert_eq!(
            (
                &body["error"]["code"],
                &body["error"]["path"],
                &body["error"]["reason"]
            ),
            (
                &json!("dataset_preparation_invalid"),
                &json!("table_name"),
                &json!("not_a_name")
            )
        );
        let mut named = spec;
        named["table_name"] = json!("glucose");
        let response = prepare(named).await.unwrap();
        assert_eq!(response.status(), 409);
        assert_eq!(
            error(&response.json().await.unwrap()),
            "dataset_not_profiled"
        );

        // With the lab's queue full the guard's decision is kept and the intake waits. A retry keeps it waiting while
        // the queue is full; once there is room the intake endpoint starts it, once.
        let limit: i32 = owner
            .query_one(
                "SELECT max_queued_tasks FROM enrichedlab_agent.broker_research_groups
                 WHERE tenant_id='research-site' AND group_key='pilot-lab'",
                &[],
            )
            .await
            .unwrap()
            .get(0);
        owner
            .batch_execute(
                "UPDATE enrichedlab_agent.broker_research_groups SET max_queued_tasks=(SELECT count(*)
               FROM enrichedlab_agent.broker_tasks WHERE tenant_id='research-site' AND group_key='pilot-lab'
               AND status='queued') WHERE tenant_id='research-site' AND group_key='pilot-lab'",
            )
            .await
            .unwrap();
        // Two files wait: one is started by its upload's retry, the other by the intake endpoint.
        let (by_retry, by_endpoint) = (Uuid::new_v4(), Uuid::new_v4());
        let mut waiting = Vec::new();
        for (request, name) in [(by_retry, "waiting.csv"), (by_endpoint, "waiting%202.csv")] {
            let response = send(&origin, name, table.clone(), request, true)
                .await
                .unwrap();
            assert_eq!(response.status(), 201);
            let dataset = response.json::<Value>().await.unwrap()["dataset"].clone();
            assert_eq!(
                (
                    &dataset["guard"]["outcome"],
                    &dataset["intake_waiting"],
                    &dataset["intake_task"]
                ),
                (&json!("allowed"), &json!(true), &Value::Null)
            );
            waiting.push(dataset);
        }
        let response = send(&origin, "waiting.csv", table.clone(), by_retry, true)
            .await
            .unwrap();
        assert_eq!(response.status(), 200);
        assert_eq!(
            response.json::<Value>().await.unwrap()["dataset"],
            waiting[0]
        );
        let start = |dataset: &Value, intent: bool| {
            let mut sent = client.post(format!(
                "{origin}/api/v1/projects/{project}/datasets/{}/intake",
                dataset["dataset_id"].as_str().unwrap()
            ));
            if intent {
                sent = sent.header(MUTATION_INTENT, "mutate");
            }
            sent.send()
        };
        let response = start(&waiting[1], true).await.unwrap();
        assert_eq!(response.status(), 429);
        assert_eq!(error(&response.json().await.unwrap()), "group_queue_full");
        owner
            .batch_execute(&format!(
                "UPDATE enrichedlab_agent.broker_research_groups SET max_queued_tasks={limit}
                 WHERE tenant_id='research-site' AND group_key='pilot-lab'"
            ))
            .await
            .unwrap();
        let response = start(&waiting[1], false).await.unwrap();
        assert_eq!(response.status(), 403);
        let response = send(&origin, "waiting.csv", table.clone(), by_retry, true)
            .await
            .unwrap();
        assert_eq!(response.status(), 200);
        let by_retry = response.json::<Value>().await.unwrap()["dataset"].clone();
        let response = start(&waiting[1], true).await.unwrap();
        assert_eq!(response.status(), 200);
        let by_endpoint = response.json::<Value>().await.unwrap()["dataset"].clone();
        for (started, before) in [(&by_retry, &waiting[0]), (&by_endpoint, &waiting[1])] {
            assert_eq!(
                (&started["dataset_id"], &started["intake_waiting"]),
                (&before["dataset_id"], &json!(false))
            );
            assert!(started["intake_task"].is_string());
            let response = start(started, true).await.unwrap();
            assert_eq!(&response.json::<Value>().await.unwrap()["dataset"], started);
        }
        assert_ne!(by_retry["intake_task"], by_endpoint["intake_task"]);
        let response = client
            .post(format!(
                "{origin}/api/v1/projects/{project}/datasets/{}/intake",
                held["dataset"]["dataset_id"].as_str().unwrap()
            ))
            .header(MUTATION_INTENT, "mutate")
            .send()
            .await
            .unwrap();
        assert_eq!(
            (
                response.status().as_u16(),
                error(&response.json().await.unwrap())
            ),
            (409, "dataset_not_allowed".into())
        );

        // Without a guard nothing is sent anywhere.
        let unguarded = serve(api(&api_dsn, me, root.path())).await;
        let response = send(&unguarded, "GTT.csv", table, Uuid::new_v4(), true)
            .await
            .unwrap();
        assert_eq!(response.status(), 503);
        assert_eq!(
            error(&response.json().await.unwrap()),
            "dataset_guard_unconfigured"
        );
        assert_eq!(seen.0.lock().unwrap().len(), 4);
    }
}
