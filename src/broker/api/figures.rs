//! Figure versions: the render service's catalog of plot features as the worker last read it, a
//! person's request for a version drawn from one result table, a version's request with its receipts, and the
//! versions made from one analysis, plus previews, which make no version, and the manuscript mark.
use super::*;
use crate::broker::store::FigureRequest;
use serde_json::Value;

const MAX_TITLE_CHARS: usize = 200;
const MAX_TABLE_PATH_BYTES: usize = 256;
/// How long a preview request takes at most, the person's sign-in, its database waits and abandoning a preview that was
/// not drawn included, and how often it looks for the drawing. The tests use shorter times.
const PREVIEW_WAIT: Duration = if cfg!(test) {
    Duration::from_secs(6)
} else {
    Duration::from_secs(40)
};
const PREVIEW_POLL: Duration = Duration::from_millis(150);
/// The end of that time, kept for abandoning a preview that was not drawn. One the database has not abandoned by the
/// end is abandoned after the answer goes out.
const ABANDON_RESERVE: Duration = if cfg!(test) {
    Duration::from_secs(2)
} else {
    Duration::from_secs(5)
};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct FigurePreviewRequest {
    source_task: Uuid,
    table_path: String,
    feature: String,
    #[serde(default)]
    controls: Value,
    #[serde(default)]
    title: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct ManuscriptMark {
    version: Uuid,
}

/// Settings as an object of names and values; none is an empty object.
fn settings(controls: Value) -> Result<Value, ApiError> {
    match controls {
        Value::Null => Ok(json!({})),
        Value::Object(_) => Ok(controls),
        _ => Err(ApiError::invalid(
            "figure_settings_invalid",
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
            "figure_request_invalid",
            "The table path or the title is not valid",
        ));
    }
    Ok(())
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct FigureVersionRequest {
    source_task: Uuid,
    table_path: String,
    feature: String,
    #[serde(default)]
    controls: Value,
    #[serde(default)]
    title: String,
    #[serde(default)]
    previous: Option<Uuid>,
    request_id: Uuid,
}

/// The plot features the render service declares, with each one's settings, for the figure editor.
pub(super) async fn catalog(
    State(state): State<ApiState>,
    headers: HeaderMap,
) -> Result<impl IntoResponse, ApiError> {
    let actor = authenticate(&state, &headers).await?;
    Ok(Json(
        json!({ "catalog": state.store.render_catalog(actor).await? }),
    ))
}

/// A person's request for a figure version from one result table of a finished analysis in the project. The broker
/// checks the table, the plot and each setting's name and kind; the render service checks the values when it draws.
pub(super) async fn request(
    State(state): State<ApiState>,
    headers: HeaderMap,
    AxumPath(project): AxumPath<Uuid>,
    Json(request): Json<FigureVersionRequest>,
) -> Result<impl IntoResponse, ApiError> {
    require_mutation_intent(&headers)?;
    let controls = settings(request.controls)?;
    check_text(&request.table_path, &request.title)?;
    let actor = authenticate(&state, &headers).await?;
    let task = state
        .store
        .request_figure(
            actor,
            &FigureRequest {
                project,
                source_task: request.source_task,
                table_path: &request.table_path,
                feature: &request.feature,
                controls: &controls,
                title: request.title.trim(),
                previous: request.previous,
                request_id: request.request_id,
            },
        )
        .await?;
    Ok((StatusCode::ACCEPTED, Json(json!({ "task_id": task }))))
}

/// A figure version's request, its source analysis and every receipt; null for a task that is not a figure version.
pub(super) async fn detail(
    State(state): State<ApiState>,
    headers: HeaderMap,
    AxumPath(task): AxumPath<Uuid>,
) -> Result<impl IntoResponse, ApiError> {
    let actor = authenticate(&state, &headers).await?;
    Ok(Json(
        json!({ "figure": state.store.task_figure_detail(actor, task).await? }),
    ))
}

/// The figure versions made from one analysis that the reader may open, newest first.
pub(super) async fn versions(
    State(state): State<ApiState>,
    headers: HeaderMap,
    AxumPath(task): AxumPath<Uuid>,
) -> Result<impl IntoResponse, ApiError> {
    let actor = authenticate(&state, &headers).await?;
    Ok(Json(
        json!({ "versions": state.store.figure_versions(actor, task).await? }),
    ))
}

/// A refusal the broker made before the service drew anything, by its code.
fn source_refusal(code: &str) -> ApiError {
    let code = match code {
        "figure_source_changed" => "figure_source_changed",
        "figure_source_unreadable" => "figure_source_unreadable",
        "figure_table_missing" => "figure_table_missing",
        "figure_table_too_large" => "figure_table_too_large",
        "figure_table_unreadable" => "figure_table_unreadable",
        "figure_table_unsuitable" => "figure_table_unsuitable",
        _ => "figure_preview_refused",
    };
    ApiError {
        status: StatusCode::CONFLICT,
        code,
        message: "The result table could not be read for a preview",
    }
}

/// `POST /api/v1/projects/{project}/figures/previews`: the drawing a version with these settings would have, as SVG,
/// without making a version, task, package or history entry. The request passes the checks a version request passes;
/// the worker that reaches the render service draws it (the render service is reachable from worker
/// Pods only), and this answers within 40 seconds, the person's sign-in, database waits and abandoning a preview that
/// was not drawn included. One preview at a time and 30 a minute per person.
pub(super) async fn preview(
    State(state): State<ApiState>,
    headers: HeaderMap,
    AxumPath(project): AxumPath<Uuid>,
    Json(request): Json<FigurePreviewRequest>,
) -> Result<Response, ApiError> {
    require_mutation_intent(&headers)?;
    let controls = settings(request.controls)?;
    check_text(&request.table_path, &request.title)?;
    // One deadline covers the whole answer. Everything but abandoning an undrawn preview ends by `drawn_by`, which keeps
    // the end of the wait for that.
    let deadline = tokio::time::Instant::now() + PREVIEW_WAIT;
    let drawn_by = deadline - ABANDON_RESERVE;
    let unavailable = || ApiError {
        status: StatusCode::SERVICE_UNAVAILABLE,
        code: "figure_service_unavailable",
        message: "The figure service did not draw the preview; try again",
    };
    let actor = tokio::time::timeout_at(drawn_by, authenticate(&state, &headers))
        .await
        .map_err(|_| unavailable())??;
    let figure = FigureRequest {
        project,
        source_task: request.source_task,
        table_path: &request.table_path,
        feature: &request.feature,
        controls: &controls,
        title: request.title.trim(),
        previous: None,
        request_id: Uuid::nil(),
    };
    let asked = state.store.request_figure_preview(actor, &figure);
    // A request that waits past the deadline for the database may still be recorded; the preview it made expires.
    let preview = tokio::time::timeout_at(drawn_by, asked)
        .await
        .map_err(|_| unavailable())?
        .map_err(|error| match error.code {
            "capacity_reached" => ApiError {
                status: StatusCode::TOO_MANY_REQUESTS,
                code: "capacity_reached",
                message: "One preview at a time and at most 30 a minute; try again shortly",
            },
            _ => error.into(),
        })?;
    let mut open = OpenPreview {
        store: state.store.clone(),
        actor,
        preview: Some(preview),
    };
    loop {
        let Ok(taken) =
            tokio::time::timeout_at(drawn_by, state.store.take_figure_preview(actor, preview))
                .await
        else {
            open.abandon(deadline).await;
            return Err(unavailable());
        };
        let taken = taken?;
        if !matches!(taken["state"].as_str(), Some("waiting" | "expired")) {
            open.preview = None;
        }
        let result = &taken["result"];
        let answer = |preview: Value, refused: Value| {
            Json(json!({
                "preview": preview, "refused": refused,
                "refusal": result.get("refusal").cloned().unwrap_or(Value::Null),
                "notes": result.get("notes").cloned().unwrap_or(json!([])),
                "settings_used": result.get("settings_used").cloned().unwrap_or(Value::Null),
                "service_version": result.get("service_version").cloned().unwrap_or(Value::Null),
                "image_digest": result.get("image_digest").cloned().unwrap_or(Value::Null),
                "catalog_sha256": taken["request"]["catalog_sha256"],
                "table_sha256": taken["request"]["table_sha256"],
            }))
            .into_response()
        };
        match taken["state"].as_str() {
            Some("waiting") if tokio::time::Instant::now() < drawn_by => sleep(PREVIEW_POLL).await,
            // The person lost the project while the preview waited or was drawn; it ended with nothing kept.
            Some("not_permitted") => return Err(ApiError::not_found("project_not_found")),
            Some("drawn") => {
                return Ok(answer(
                    json!({"media_type": "image/svg+xml", "sha256": taken["svg_sha256"],
                        "size_bytes": taken["svg_bytes"], "svg_base64": taken["svg_base64"]}),
                    Value::Null,
                ));
            }
            Some("refused") if result["refused"].is_string() => {
                return Ok(answer(Value::Null, result["refused"].clone()));
            }
            Some("refused") => {
                return Err(source_refusal(result["code"].as_str().unwrap_or_default()));
            }
            Some("failed") if result["code"] == "figure_preview_too_large" => {
                return Err(ApiError {
                    status: StatusCode::PAYLOAD_TOO_LARGE,
                    code: "figure_preview_too_large",
                    message: "The drawing is larger than 2 MiB; keep a version to see it",
                });
            }
            _ => {
                open.abandon(deadline).await;
                return Err(unavailable());
            }
        }
    }
}

/// A person's preview whose drawing has not been handed over. It is abandoned when its request ends first: the wait ran
/// out, or the person closed the editor and the server dropped the request. Their next preview then need not wait for
/// this one to expire, and a worker drawing it can no longer record it.
struct OpenPreview {
    store: Store,
    actor: Uuid,
    preview: Option<Uuid>,
}

impl OpenPreview {
    /// Abandons it before the answer goes out, so a retry that follows the answer is not refused as a second preview.
    /// A database that has not answered by `deadline` leaves it to the drop, after the answer.
    async fn abandon(&mut self, deadline: tokio::time::Instant) {
        if let Some(preview) = self.preview {
            let abandoned = abandon_preview(self.store.clone(), self.actor, preview);
            if tokio::time::timeout_at(deadline, abandoned).await.is_ok() {
                self.preview = None;
            }
        }
    }
}

impl Drop for OpenPreview {
    fn drop(&mut self) {
        if let (Some(preview), Ok(runtime)) =
            (self.preview.take(), tokio::runtime::Handle::try_current())
        {
            runtime.spawn(abandon_preview(self.store.clone(), self.actor, preview));
        }
    }
}

async fn abandon_preview(store: Store, actor: Uuid, preview: Uuid) {
    if let Err(error) = store.abandon_figure_preview(actor, preview).await {
        tracing::warn!(event = "figure_preview_not_abandoned", code = error.code);
    }
}

/// `PUT /api/v1/projects/{project}/figures/{figure}/manuscript`: a person who may steer the project marks one completed
/// version of the figure (named by its first version) as the one in the manuscript. Answers the mark.
pub(super) async fn mark_manuscript(
    State(state): State<ApiState>,
    headers: HeaderMap,
    AxumPath((project, figure)): AxumPath<(Uuid, Uuid)>,
    Json(mark): Json<ManuscriptMark>,
) -> Result<impl IntoResponse, ApiError> {
    require_mutation_intent(&headers)?;
    let actor = authenticate(&state, &headers).await?;
    let mark = state
        .store
        .mark_figure_manuscript(actor, project, figure, mark.version)
        .await?;
    Ok(Json(mark))
}

/// `DELETE /api/v1/projects/{project}/figures/{figure}/manuscript`: clears the mark. 204 whether or not one was set.
pub(super) async fn clear_manuscript(
    State(state): State<ApiState>,
    headers: HeaderMap,
    AxumPath((project, figure)): AxumPath<(Uuid, Uuid)>,
) -> Result<impl IntoResponse, ApiError> {
    require_mutation_intent(&headers)?;
    let actor = authenticate(&state, &headers).await?;
    state
        .store
        .clear_figure_manuscript(actor, project, figure)
        .await?;
    Ok(StatusCode::NO_CONTENT)
}

#[cfg(test)]
mod tests {
    use super::super::test_support::{api, person, serve, sha, tar_gz};
    use super::*;
    use crate::broker::{config::WorkerConfig, figure_run, store::PreviewOutcome};
    use base64::Engine;
    use tokio_util::sync::CancellationToken;

    /// A stand-in render service: an SVG for any request, and for the axis title "refuse me" a refusal of the request,
    /// for "repeat pairs" a template's refusal, for "cannot draw" an error with no refusal and for "draw big" a drawing
    /// past the preview limit. Each error's words hold a made-up cell value, which must never reach the person.
    async fn render_service() -> String {
        async fn render(Json(body): Json<Value>) -> (StatusCode, Json<Value>) {
            let label = body["params"]["x_label"].as_str().unwrap_or_default();
            let words = "the cell 'Patient 0042' was refused";
            let refusal = |status, refusal: Value| {
                (status, Json(json!({"error": words, "refusal": refusal})))
            };
            match label {
                "refuse me" => {
                    return refusal(
                        StatusCode::BAD_REQUEST,
                        json!({"code": "setting_invalid", "columns": [], "counts": []}),
                    );
                }
                "repeat pairs" => {
                    return refusal(
                        StatusCode::OK,
                        json!({"code": "repeated_pairs", "columns": ["dose", "len"], "counts": [2]}),
                    );
                }
                "cannot draw" => return (StatusCode::OK, Json(json!({"error": words}))),
                _ => {}
            }
            let svg = if label == "draw big" {
                format!(
                    "<svg xmlns='http://www.w3.org/2000/svg'><!--{}--></svg>",
                    "x".repeat(2 * 1024 * 1024)
                )
            } else {
                "<svg xmlns='http://www.w3.org/2000/svg'><circle r='1'/></svg>".to_owned()
            };
            let b64 = |bytes: &[u8]| base64::engine::general_purpose::STANDARD.encode(bytes);
            (
                StatusCode::OK,
                Json(
                    json!({"png": b64(b"\x89PNG\r\n\x1a\nstand-in"), "svg": svg, "code": "plot()\n",
                    "common_code": "WORK <- 1\n", "service_version": "0.5.22",
                    "image_digest": format!("sha256:{}", "1".repeat(64)),
                    "spec": {"params": body["params"]}, "font_face": "DejaVu Sans", "notes": ["a stand-in note"]}),
                ),
            )
        }
        let app = Router::new().route("/render", post(render));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        url
    }

    #[tokio::test]
    #[ignore = "isolated PostgreSQL through qualify_remote_broker.sh figure-previews"]
    async fn a_person_previews_a_figure_and_marks_the_version_in_the_manuscript() {
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
                 VALUES ('research-site','preview-outsider@example.invalid','pilot-lab','researcher');",
            )
            .await
            .unwrap();
        let api_dsn = dsn.replace("user=postgres", "user=broker_api_qual");
        let worker_dsn = dsn.replace("user=postgres", "user=broker_worker_qual");
        let root = tempfile::tempdir().unwrap();
        let me = person("preview-maker", "researcher@example.invalid");
        let state = api(&api_dsn, me.clone(), root.path());
        let actor = state.store.resolve_principal(&me).await.unwrap();
        let outsider = person("preview-outsider", "preview-outsider@example.invalid");
        let stranger = api(&api_dsn, outsider.clone(), root.path());
        let outsider_id = stranger.store.resolve_principal(&outsider).await.unwrap();
        state.store.ready_for_figure_previews(true).await.unwrap();

        // A project with a finished analysis whose package holds one synthetic table, and the lab's figure method.
        let submit = |key: &'static str| {
            let store = state.store.clone();
            async move {
                store
                    .submit_task(
                        actor,
                        "pilot-lab",
                        "general-research",
                        "Synthetic doses.",
                        key,
                        &"b".repeat(64),
                    )
                    .await
                    .unwrap()
            }
        };
        let (source, version, unfinished, other, stale) = (
            submit("preview-source").await,
            submit("preview-version").await,
            submit("preview-unfinished").await,
            submit("preview-other-figure").await,
            submit("preview-stale-source").await,
        );
        let project = Uuid::new_v4();
        let table = b"dose,len\n0.5,4.2\n1.0,11.5\n2.0,26.1\n";
        let package = tar_gz(&[("results/table.csv", table)]);
        let relative = format!("tasks/{source}/outputs/result.tar.gz");
        std::fs::create_dir_all(root.path().join(format!("tasks/{source}/outputs"))).unwrap();
        std::fs::write(root.path().join(&relative), &package).unwrap();
        let digest = json!({"tables": [{"path": "results/table.csv", "rows": 3, "columns": ["dose", "len"], "sha256": sha(table)}]});
        let features = json!([{"id": "scatter", "label": "Scatter plot", "kind": "figure",
            "params": {"x": {"type": "column", "required": true}, "y": {"type": "column", "required": true},
              "x_label": {"type": "string", "required": false}, "subtitle": {"type": "string", "maxlen": 10.0},
              "group_colors": {"type": "colors", "of": "x", "choices": ["#E69F00", "#56B4E9", "#999999"]}}}]);
        owner
            .execute(
                "INSERT INTO enrichedlab_agent.broker_projects(tenant_id,project_id,group_key,owner_id,title,visibility) \
                 VALUES ('research-site',$1,'pilot-lab',$2,'Preview qualification','private')",
                &[&project, &actor],
            )
            .await
            .unwrap();
        for (task, key) in [
            (source, "source"),
            (version, "version"),
            (unfinished, "unfinished"),
            (other, "other"),
            (stale, "stale"),
        ] {
            owner
                .execute(
                    "INSERT INTO enrichedlab_agent.broker_project_tasks(tenant_id,project_id,task_id,action_key) \
                     VALUES ('research-site',$1,$2,$3)",
                    &[&project, &task, &key],
                )
                .await
                .unwrap();
        }
        owner
            .execute(
                "UPDATE enrichedlab_agent.broker_tasks SET status='completed', finished_at=clock_timestamp() \
                 WHERE task_id IN ($1,$2,$3,$4)",
                &[&source, &version, &other, &stale],
            )
            .await
            .unwrap();
        // The stale source's digest names other table bytes than its package holds.
        let stale_relative = format!("tasks/{stale}/outputs/result.tar.gz");
        std::fs::create_dir_all(root.path().join(format!("tasks/{stale}/outputs"))).unwrap();
        std::fs::write(root.path().join(&stale_relative), &package).unwrap();
        let stale_digest = json!({"tables": [{"path": "results/table.csv", "rows": 3,
            "columns": ["dose", "len"], "sha256": "0".repeat(64)}]});
        let mut artifacts = Vec::new();
        for (task, path, digest) in [
            (source, &relative, &digest),
            (stale, &stale_relative, &stale_digest),
        ] {
            let artifact: Uuid = owner
                .query_one(
                    "INSERT INTO enrichedlab_agent.broker_artifacts(tenant_id,task_id,role,label,relative_path,media_type,\
                     size_bytes,sha256) \
                     VALUES ('research-site',$1,'result','Result',$2,'application/gzip',$3,$4) RETURNING artifact_id",
                    &[&task, path, &(package.len() as i64), &sha(&package)],
                )
                .await
                .unwrap()
                .get(0);
            owner
                .execute(
                    "INSERT INTO enrichedlab_agent.broker_artifact_digests(tenant_id,artifact_id,artifact_sha256,task_id,\
                     attempt,lease_generation,schema_version,status,digest,digest_sha256) \
                     VALUES ('research-site',$1,$2,$3,1,1,'enrichedlab.result-digest.v1','summarized',$4,$5)",
                    &[&artifact, &sha(&package), &task, digest, &"d".repeat(64)],
                )
                .await
                .unwrap();
            artifacts.push(artifact);
        }
        let artifact = artifacts[0];
        owner
            .batch_execute(
                "INSERT INTO enrichedlab_agent.broker_agent_profiles(tenant_id,profile_key,name,description,harness,thinking,\
                   route,controller_profile,gateway_profile,image_ref,cpu_limit,memory_limit,state_profile,role,tool_bundle,\
                   max_run_seconds,max_task_attempts,max_provider_attempts)
                 SELECT tenant_id,'figure-render','Draw a figure','Draw a figure from a result table',harness,thinking,\
                   'server-render',controller_profile,gateway_profile,image_ref,cpu_limit,memory_limit,state_profile,role,\
                   tool_bundle,max_run_seconds,1,max_provider_attempts
                 FROM enrichedlab_agent.broker_agent_profiles WHERE tenant_id='research-site' AND profile_key='general-research';
                 INSERT INTO enrichedlab_agent.broker_group_profile_grants(tenant_id,group_key,profile_key)
                   VALUES ('research-site','pilot-lab','figure-render');",
            )
            .await
            .unwrap();

        // The worker that reaches the render service records its catalog and draws previews. A colors setting must name
        // a column setting of the same chart and list the colors it accepts.
        let worker = Store::connect(&worker_dsn, "research-site".into(), 2).unwrap();
        // The worker keeps a feature's id, label, description and settings, as it does with the service's catalog.
        let mut features = features;
        features[0].as_object_mut().unwrap().remove("kind");
        let mut malformed = Vec::new();
        for (key, value) in [
            ("of", json!("x_label")),
            ("of", json!("no_such_setting")),
            ("of", Value::Null),
            ("choices", json!([])),
            ("choices", json!(["red"])),
            ("choices", json!("#E69F00")),
        ] {
            let mut bad = features.clone();
            bad[0]["params"]["group_colors"][key] = value;
            malformed.push(bad);
        }
        // `of` must be a string: a boolean reads as the text "true", which a column setting could be named.
        let mut not_text = features.clone();
        not_text[0]["params"]["true"] = json!({"type": "column", "required": false});
        not_text[0]["params"]["group_colors"]["of"] = json!(true);
        malformed.push(not_text);
        let mut unknown_type = features.clone();
        unknown_type[0]["params"]["x_label"]["type"] = json!("text");
        malformed.push(unknown_type);
        // A limit is a whole number from 1 to 9999, however it is written; 10.0 above is 10.
        for maxlen in [
            json!(10.5),
            json!("10"),
            json!(0),
            json!(10_000),
            Value::Null,
        ] {
            let mut bad = features.clone();
            bad[0]["params"]["subtitle"]["maxlen"] = maxlen;
            malformed.push(bad);
        }
        for bad in malformed {
            assert_eq!(
                worker
                    .record_render_catalog("render-worker", "0.5.22", None, &bad)
                    .await
                    .unwrap_err()
                    .code,
                "render_catalog_invalid",
                "{bad}"
            );
        }
        let catalog = worker
            .record_render_catalog("render-worker", "0.5.22", None, &features)
            .await
            .unwrap();
        worker.ready_for_figure_previews(false).await.unwrap();
        let shutdown = CancellationToken::new();
        let serving = tokio::spawn(figure_run::serve_previews(
            worker.clone(),
            Arc::new(WorkerConfig {
                database_dsn: worker_dsn.clone(),
                tenant: "research-site".into(),
                artifact_root: root.path().to_path_buf(),
                controller_root: root.path().to_path_buf(),
                controller_state: root.path().to_path_buf(),
                controller_config: root.path().to_path_buf(),
                controller_bin: "/bin/false".into(),
                openshell_bin: "/bin/false".into(),
                docker_bin: "/bin/false".into(),
                worker_id: "render-worker".into(),
                direct_dispatch: None,
                query_connection_dir: None,
                render_url: Some(render_service().await),
                dataset_fetcher_config: None,
                lab_objects_config: None,
                live_output: false,
                max_parallel: 1,
                lease_seconds: 120,
                poll_interval: Duration::from_millis(200),
                database_pool: 2,
                max_workspace_files: 10,
                max_workspace_bytes: 1024,
            }),
            shutdown.clone(),
        ));
        let origin = serve(state.clone()).await;
        let elsewhere = serve(stranger.clone()).await;
        let client = reqwest::Client::new();
        let body = |label: &str| {
            json!({"source_task": source, "table_path": "results/table.csv", "feature": "scatter",
                "controls": {"x": "dose", "y": "len", "x_label": label}, "title": "Length by dose"})
        };
        let post = |origin: &str, body: Value, intent: bool| {
            let mut request = client
                .post(format!(
                    "{origin}/api/v1/projects/{project}/figures/previews"
                ))
                .json(&body);
            if intent {
                request = request.header(MUTATION_INTENT, "mutate");
            }
            request.send()
        };
        let code = |value: &Value| {
            value["error"]["code"]
                .as_str()
                .unwrap_or_default()
                .to_owned()
        };

        // A drawing, checked against its digest, with what the service used and noted; no task is made.
        let tasks = || async {
            owner
                .query_one("SELECT count(*) FROM enrichedlab_agent.broker_tasks", &[])
                .await
                .unwrap()
                .get::<_, i64>(0)
        };
        let before = tasks().await;
        let response = post(&origin, body("Dose (mg)"), true).await.unwrap();
        assert_eq!(response.status(), 200);
        let drawn: Value = response.json().await.unwrap();
        let svg = base64::engine::general_purpose::STANDARD
            .decode(drawn["preview"]["svg_base64"].as_str().unwrap())
            .unwrap();
        assert_eq!(
            svg,
            b"<svg xmlns='http://www.w3.org/2000/svg'><circle r='1'/></svg>"
        );
        assert_eq!(
            (
                &drawn["preview"]["media_type"],
                &drawn["preview"]["sha256"],
                &drawn["preview"]["size_bytes"]
            ),
            (
                &json!("image/svg+xml"),
                &json!(sha(&svg)),
                &json!(svg.len())
            )
        );
        assert_eq!(drawn["refused"], Value::Null);
        assert_eq!(drawn["notes"], json!(["a stand-in note"]));
        assert_eq!(drawn["settings_used"]["x_label"], "Dose (mg)");
        assert_eq!(drawn["settings_used"]["font_face"], "DejaVu Sans");
        assert_eq!(
            (
                &drawn["service_version"],
                &drawn["catalog_sha256"],
                &drawn["table_sha256"]
            ),
            (&json!("0.5.22"), &json!(catalog), &json!(sha(table)))
        );
        assert_eq!(tasks().await, before, "a preview makes no task");

        // A refusal in the broker's words for its code, never the service's own; a drawing past the limit; a stale
        // table read.
        for (label, refused, refusal) in [
            (
                "refuse me",
                "The figure service did not accept one of these settings.",
                json!({"code": "setting_invalid", "columns": [], "counts": []}),
            ),
            (
                "repeat pairs",
                "Each pair of dose and len must appear once (repeated rows: 2); summarize them in the analysis first.",
                json!({"code": "repeated_pairs", "columns": ["dose", "len"], "counts": [2]}),
            ),
            (
                "cannot draw",
                "The figure service could not draw this table with these settings.",
                Value::Null,
            ),
        ] {
            let response = post(&origin, body(label), true).await.unwrap();
            assert_eq!(response.status(), 200, "{label}");
            let text = response.text().await.unwrap();
            assert!(!text.contains("Patient 0042"), "{label}: {text}");
            let answer: Value = serde_json::from_str(&text).unwrap();
            assert_eq!(
                (&answer["preview"], &answer["refused"], &answer["refusal"]),
                (&Value::Null, &json!(refused), &refusal),
                "{label}"
            );
        }
        let response = post(&origin, body("draw big"), true).await.unwrap();
        assert_eq!(response.status(), 413);
        assert_eq!(
            code(&response.json().await.unwrap()),
            "figure_preview_too_large"
        );

        // Refusals before anything is drawn.
        for (origin, body, intent, status, expected) in [
            (&origin, body("x"), false, 403, "mutation_intent_required"),
            (&elsewhere, body("x"), true, 404, "project_not_found"),
            (
                &origin,
                json!({"source_task": source, "table_path": "results/table.csv", "feature": "scatter",
                    "controls": {"x": "dose", "y": "no such column"}}),
                true,
                400,
                "figure_settings_invalid",
            ),
        ] {
            let response = post(origin, body, intent).await.unwrap();
            assert_eq!(response.status(), status, "{expected}");
            assert_eq!(code(&response.json().await.unwrap()), expected);
        }

        // Colors per group and a string with a declared limit pass the broker's checks only within them.
        let settings = |extra: Value| {
            let mut controls = json!({"x": "dose", "y": "len"});
            controls
                .as_object_mut()
                .unwrap()
                .extend(extra.as_object().unwrap().clone());
            json!({"source_task": source, "table_path": "results/table.csv", "feature": "scatter", "controls": controls})
        };
        let response = post(
            &origin,
            settings(json!({"group_colors": {"0.5": "#e69f00", "1.0": "#56B4E9"}, "subtitle": "Ten chars!"})),
            true,
        )
        .await
        .unwrap();
        assert_eq!(response.status(), 200);
        let thirteen: serde_json::Map<String, Value> = (0..13)
            .map(|n| (format!("g{n}"), json!("#999999")))
            .collect();
        for extra in [
            json!({"group_colors": {"0.5": "#FF0000"}}),
            json!({"group_colors": {}}),
            json!({"group_colors": thirteen}),
            json!({"group_colors": ["#E69F00"]}),
            json!({"subtitle": "Eleven char"}),
        ] {
            let response = post(&origin, settings(extra.clone()), true).await.unwrap();
            assert_eq!(response.status(), 400, "{extra}");
            assert_eq!(
                code(&response.json().await.unwrap()),
                "figure_settings_invalid"
            );
        }
        // A table whose bytes differ from what the analysis's digest names is refused by the worker, before drawing.
        let response = post(
            &origin,
            json!({"source_task": stale, "table_path": "results/table.csv", "feature": "scatter",
                "controls": {"x": "dose", "y": "len"}}),
            true,
        )
        .await
        .unwrap();
        assert_eq!(response.status(), 409);
        assert!(code(&response.json().await.unwrap()).starts_with("figure_"));

        // Thirty in a minute is the limit: five were made above.
        owner
            .execute(
                "INSERT INTO enrichedlab_agent.broker_figure_previews(tenant_id,principal_id,project_id,source_artifact_id,\
                 request,request_sha256,state,result,finished_at) \
                 SELECT 'research-site',$1,$2,$3,'{}',repeat('0',64),'delivered','{}',clock_timestamp() \
                 FROM generate_series(1,25)",
                &[&actor, &project, &artifact],
            )
            .await
            .unwrap();
        let response = post(&origin, body("x"), true).await.unwrap();
        assert_eq!(response.status(), 429);
        assert_eq!(code(&response.json().await.unwrap()), "capacity_reached");

        // A preview that no worker draws in time is unavailable, not a hung request.
        shutdown.cancel();
        serving.await.unwrap();
        owner
            .execute("DELETE FROM enrichedlab_agent.broker_figure_previews", &[])
            .await
            .unwrap();
        let waiting = tokio::spawn(post(&origin, body("x"), true));
        let mut expired = 0;
        while expired == 0 {
            tokio::time::sleep(Duration::from_millis(100)).await;
            expired = owner
                .execute(
                    "UPDATE enrichedlab_agent.broker_figure_previews SET expires_at = clock_timestamp() \
                     WHERE state = 'pending'",
                    &[],
                )
                .await
                .unwrap();
        }
        let response = waiting.await.unwrap().unwrap();
        assert_eq!(response.status(), 503);
        assert_eq!(
            code(&response.json().await.unwrap()),
            "figure_service_unavailable"
        );

        // A preview whose request ends before the drawing is handed over is abandoned at once: the wait above ran out,
        // and a person who closes the editor drops the request. Their next preview is not refused as a second one.
        let previews = || async {
            let row = owner
                .query_one(
                    "SELECT count(*) FILTER (WHERE state IN ('pending', 'drawing'))::int, \
                     count(*) FILTER (WHERE state = 'failed' AND result->>'code' = 'figure_preview_abandoned')::int \
                     FROM enrichedlab_agent.broker_figure_previews",
                    &[],
                )
                .await
                .unwrap();
            (row.get::<_, i32>(0), row.get::<_, i32>(1))
        };
        assert_eq!(previews().await, (0, 1));
        let impatient = reqwest::Client::builder()
            .timeout(Duration::from_millis(500))
            .build()
            .unwrap();
        for abandoned in 2..4 {
            let closed = impatient
                .post(format!(
                    "{origin}/api/v1/projects/{project}/figures/previews"
                ))
                .header(MUTATION_INTENT, "mutate")
                .json(&body("x"))
                .send()
                .await;
            // No worker draws it, so the request waits until the client gives up.
            assert!(closed.is_err_and(|error| error.is_timeout()), "{abandoned}");
            let mut seen = previews().await;
            for _ in 0..50 {
                if seen == (0, abandoned) {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(100)).await;
                seen = previews().await;
            }
            assert_eq!(seen, (0, abandoned));
        }
        // The whole answer ends within the preview wait, whatever it waits on: a sign-in that cannot read its table, or a
        // preview the database cannot yet abandon. That one is abandoned after the answer, once the table is free. The
        // qualification roles wait 5 s for a lock and then fail with a database error (500); the tests' 4 s for the
        // sign-in and the drawing ends first, so a 503 here means the deadline, not the lock wait, ended them.
        let in_time = PREVIEW_WAIT + Duration::from_secs(1);
        owner
            .batch_execute(
                "BEGIN; LOCK TABLE enrichedlab_agent.broker_principals IN ACCESS EXCLUSIVE MODE;",
            )
            .await
            .unwrap();
        let answered = tokio::time::timeout(in_time, post(&origin, body("x"), true)).await;
        owner.batch_execute("ROLLBACK").await.unwrap();
        let response = answered.expect("the sign-in held the answer").unwrap();
        assert_eq!(response.status(), 503);
        assert_eq!(
            code(&response.json().await.unwrap()),
            "figure_service_unavailable"
        );
        let started = tokio::time::Instant::now();
        let waiting = tokio::spawn(post(&origin, body("x"), true));
        while previews().await.0 == 0 {
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        owner
            .batch_execute(
                "BEGIN; LOCK TABLE enrichedlab_agent.broker_figure_previews IN ACCESS EXCLUSIVE MODE;",
            )
            .await
            .unwrap();
        let answered = tokio::time::timeout_at(started + in_time, waiting).await;
        owner.batch_execute("ROLLBACK").await.unwrap();
        let response = answered
            .expect("abandoning the preview held the answer")
            .unwrap()
            .unwrap();
        assert_eq!(response.status(), 503);
        let mut seen = previews().await;
        for _ in 0..50 {
            if seen == (0, 4) {
                break;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
            seen = previews().await;
        }
        assert_eq!(seen, (0, 4));
        // Only the person who asked ends a preview; a drawing not yet handed over is erased; a delivered one is kept.
        let made = |made_state: &'static str| {
            let owner = &owner;
            async move {
                owner
                    .query_one(
                        "INSERT INTO enrichedlab_agent.broker_figure_previews(tenant_id,principal_id,project_id,\
                         source_artifact_id,request,request_sha256,state,svg,svg_sha256,result,finished_at) \
                         SELECT 'research-site',$1,$2,$3,'{}',repeat('0',64),$4::text,\
                         CASE WHEN $4::text = 'drawn' THEN '\\x3c7376672f3e'::bytea END,\
                         CASE WHEN $4::text = 'drawn' THEN repeat('a',64) END,\
                         CASE WHEN $4::text NOT IN ('pending','drawing') THEN '{}'::jsonb END,\
                         CASE WHEN $4::text NOT IN ('pending','drawing') THEN clock_timestamp() END \
                         RETURNING preview_id",
                        &[&actor, &project, &artifact, &made_state],
                    )
                    .await
                    .unwrap()
                    .get::<_, Uuid>(0)
            }
        };
        let pending = made("pending").await;
        assert!(
            !state
                .store
                .abandon_figure_preview(Uuid::new_v4(), pending)
                .await
                .unwrap()
        );
        assert!(
            state
                .store
                .abandon_figure_preview(actor, pending)
                .await
                .unwrap()
        );
        assert!(
            !state
                .store
                .abandon_figure_preview(actor, pending)
                .await
                .unwrap()
        );
        let drawn = made("drawn").await;
        assert!(
            state
                .store
                .abandon_figure_preview(actor, drawn)
                .await
                .unwrap()
        );
        let erased = owner
            .query_one(
                "SELECT state, svg IS NULL AND svg_sha256 IS NULL FROM enrichedlab_agent.broker_figure_previews \
                 WHERE preview_id = $1",
                &[&drawn],
            )
            .await
            .unwrap();
        assert_eq!(
            (erased.get::<_, String>(0), erased.get::<_, bool>(1)),
            ("failed".to_owned(), true)
        );
        let delivered = made("delivered").await;
        assert!(
            !state
                .store
                .abandon_figure_preview(actor, delivered)
                .await
                .unwrap()
        );

        // Access is checked again when a worker claims a preview and when its drawing is handed over: a person who
        // loses the project while a preview waits or is drawn gets nothing, and the drawing is erased.
        let access = |sql: &'static str| {
            let owner = &owner;
            async move { owner.execute(sql, &[&project, &outsider_id]).await.unwrap() }
        };
        let grant = || {
            access(
                "INSERT INTO enrichedlab_agent.broker_project_grants(tenant_id,project_id,principal_id,capabilities) \
                 VALUES ('research-site',$1,$2,ARRAY['contribute'])",
            )
        };
        let revoke = || {
            access(
                "DELETE FROM enrichedlab_agent.broker_project_grants WHERE project_id = $1 AND principal_id = $2",
            )
        };
        let controls = json!({"x": "dose", "y": "len"});
        let figure = FigureRequest {
            project,
            source_task: source,
            table_path: "results/table.csv",
            feature: "scatter",
            controls: &controls,
            title: "",
            previous: None,
            request_id: Uuid::nil(),
        };
        let ended = |preview: Uuid| {
            let owner = &owner;
            async move {
                let row = owner
                    .query_one(
                        "SELECT state, result->>'code', svg IS NULL AND svg_sha256 IS NULL \
                         FROM enrichedlab_agent.broker_figure_previews WHERE preview_id = $1",
                        &[&preview],
                    )
                    .await
                    .unwrap();
                (
                    row.get::<_, String>(0),
                    row.get::<_, Option<String>>(1),
                    row.get::<_, bool>(2),
                )
            }
        };
        let not_permitted = (
            "failed".to_owned(),
            Some("figure_preview_not_permitted".to_owned()),
            true,
        );
        grant().await;
        let waiting = stranger
            .store
            .request_figure_preview(outsider_id, &figure)
            .await
            .unwrap();
        revoke().await;
        assert!(
            worker
                .claim_figure_preview("render-worker")
                .await
                .unwrap()
                .is_none()
        );
        assert_eq!(ended(waiting).await, not_permitted);
        grant().await;
        let drawn = stranger
            .store
            .request_figure_preview(outsider_id, &figure)
            .await
            .unwrap();
        let (claimed, _) = worker
            .claim_figure_preview("render-worker")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(claimed, drawn);
        assert!(
            worker
                .finish_figure_preview(
                    "render-worker",
                    drawn,
                    &PreviewOutcome {
                        state: "drawn",
                        svg: Some(b"<svg/>".to_vec()),
                        result: json!({}),
                    },
                )
                .await
                .unwrap()
        );
        revoke().await;
        assert_eq!(
            stranger
                .store
                .take_figure_preview(outsider_id, drawn)
                .await
                .unwrap(),
            json!({"state": "not_permitted"})
        );
        assert_eq!(ended(drawn).await, not_permitted);
        // Through the API: the request is accepted, the grant goes while it waits, and the answer is not found.
        grant().await;
        let asked = tokio::spawn(post(&elsewhere, body("x"), true));
        let mut pending = 0;
        while pending == 0 {
            tokio::time::sleep(Duration::from_millis(50)).await;
            pending = owner
                .query_one(
                    "SELECT count(*) FROM enrichedlab_agent.broker_figure_previews \
                     WHERE principal_id = $1 AND state = 'pending'",
                    &[&outsider_id],
                )
                .await
                .unwrap()
                .get::<_, i64>(0);
        }
        revoke().await;
        let response = asked.await.unwrap().unwrap();
        assert_eq!(response.status(), 404);
        assert_eq!(code(&response.json().await.unwrap()), "project_not_found");

        // The manuscript mark: a figure is its first version; only a completed version can be marked.
        owner
            .execute(
                "INSERT INTO enrichedlab_agent.broker_figure_requests(tenant_id,task_id,project_id,source_task_id,\
                 source_artifact_id,source_package_sha256,table_path,table_sha256,table_rows,table_columns,feature,\
                 controls,title,previous_task_id,catalog_sha256,request,request_sha256,principal_id) \
                 SELECT 'research-site',t,$1,$2,$3,$4,'results/table.csv',$5,3,'[\"dose\",\"len\"]','scatter','{}',\
                 'Length by dose',p,$6,'{}',$5,$7 \
                 FROM (VALUES ($8::uuid, NULL::uuid), ($9::uuid, $8::uuid), ($10::uuid, NULL::uuid)) v(t,p)",
                &[&project, &source, &artifact, &sha(&package), &sha(table), &catalog, &actor, &version, &unfinished, &other],
            )
            .await
            .unwrap();
        owner
            .execute(
                "INSERT INTO enrichedlab_agent.broker_figure_receipts(tenant_id,task_id,attempt,lease_generation,status,\
                 service_version,files,notes,package_sha256,started_at,finished_at) \
                 SELECT 'research-site',t,1,1,'completed','0.5.22','[{\"path\":\"figures/figure.svg\"}]','[]',$3,now(),now() \
                 FROM unnest(ARRAY[$1::uuid,$2::uuid]) t",
                &[&version, &other, &"e".repeat(64)],
            )
            .await
            .unwrap();
        let mark = |origin: &str, figure: Uuid, version: Value| {
            client
                .put(format!(
                    "{origin}/api/v1/projects/{project}/figures/{figure}/manuscript"
                ))
                .header(MUTATION_INTENT, "mutate")
                .json(&json!({"version": version}))
                .send()
        };
        let response = mark(&origin, version, json!(version)).await.unwrap();
        assert_eq!(response.status(), 200);
        let marked: Value = response.json().await.unwrap();
        assert_eq!(
            (&marked["figure"], &marked["version"], &marked["marked_by"]),
            (&json!(version), &json!(version), &json!(actor))
        );
        assert!(marked.get("marked_by_name").is_some() && marked["marked_at"].is_string());
        for (origin, figure, chosen, status, expected) in [
            (
                &origin,
                version,
                json!(unfinished),
                409,
                "version_not_drawn",
            ),
            (
                &origin,
                version,
                json!(source),
                409,
                "version_not_in_figure",
            ),
            (&origin, version, json!(other), 409, "version_not_in_figure"),
            (
                &origin,
                unfinished,
                json!(unfinished),
                404,
                "figure_not_found",
            ),
            (
                &elsewhere,
                version,
                json!(version),
                404,
                "project_not_found",
            ),
        ] {
            let response = mark(origin, figure, chosen).await.unwrap();
            assert_eq!(response.status(), status, "{expected}");
            assert_eq!(code(&response.json().await.unwrap()), expected);
        }
        let figure_record: Value = client
            .get(format!("{origin}/api/v1/tasks/{version}/figure"))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        assert_eq!(
            figure_record["figure"]["manuscript"]["version"],
            json!(version)
        );
        let clear = || {
            client
                .delete(format!(
                    "{origin}/api/v1/projects/{project}/figures/{version}/manuscript"
                ))
                .header(MUTATION_INTENT, "mutate")
                .send()
        };
        assert_eq!(clear().await.unwrap().status(), 204);
        assert_eq!(
            clear().await.unwrap().status(),
            204,
            "clearing nothing is not an error"
        );
        let figure_record: Value = client
            .get(format!("{origin}/api/v1/tasks/{version}/figure"))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        assert_eq!(figure_record["figure"]["manuscript"], Value::Null);
    }
}
