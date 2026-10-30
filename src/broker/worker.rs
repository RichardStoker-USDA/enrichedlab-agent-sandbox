use std::{
    collections::BTreeMap,
    ffi::OsString,
    fs::{self, File, OpenOptions},
    io::{Read, Write},
    path::{Component, Path},
    process::{ExitStatus, Stdio},
    sync::Arc,
    time::Duration,
};

use anyhow::{Context, Result, anyhow, bail};
use serde::Deserialize;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use tokio::{
    process::{Child, Command},
    sync::watch,
    task::{JoinHandle, JoinSet},
    time::{Instant, interval, sleep, timeout},
};
use tokio_util::sync::CancellationToken;
use walkdir::WalkDir;

use uuid::Uuid;

use super::{
    artifact_retainer::ExportLimits,
    config::{WorkerArgs, WorkerConfig},
    dispatch::{AdmissionWait, DirectDispatchCatalog, PreparedDirectDispatch},
    execution::{
        DirectExecutionControl, DirectExecutionDriver, DirectExecutionOutcome,
        DirectExecutionTiming,
    },
    model::ClaimedTask,
    sandbox::{KubernetesHttpTransport, SandboxBindingCatalog, SandboxRuntime},
    store::{ScientificHistoryEvidenceInput, Store, StoreErrorKind},
    terminal_exports::StagedTerminalRetainerFactory,
};

const TASK_MESSAGE: &str = "Complete the registered objective. Read the scoped state view before acting. Use the assigned tools for analysis, retain intermediate outputs, and record decisions and failed attempts in scoped state.";
const CONTROLLER_OPERATION_DEADLINE: Duration = Duration::from_secs(600);

#[derive(Debug)]
struct ProcessOutcome {
    status: Option<ExitStatus>,
    stop: StopReason,
}

#[derive(Debug, Deserialize)]
struct ControllerAgentStatus {
    agent_id: String,
    name: String,
    owner: String,
    domain: String,
    state_profile: String,
    state_run_id: String,
    active_sandbox: Option<String>,
    sessions_started: u64,
    sessions_completed: u64,
    lifecycle: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum StopReason {
    Exited,
    Cancelled,
    Deadline,
    LeaseLost,
    Shutdown,
}

#[derive(Debug)]
struct ArtifactCandidate {
    visibility: &'static str,
    role: &'static str,
    label: String,
    relative_path: String,
    media_type: String,
    size_bytes: i64,
    sha256: String,
}

#[derive(Debug)]
struct ScientificEvidenceDraft {
    event_id: Uuid,
    event_sequence: i64,
    event_hash: String,
    snapshot_event_id: Uuid,
    artifact_sha256: String,
    artifact_size_bytes: i64,
    artifact_role: String,
}

#[derive(Debug, Deserialize)]
struct StateExportDocument {
    schema_version: u32,
    run: StateExportRun,
    events: Vec<StateExportEvent>,
    artifacts: Vec<StateExportArtifact>,
    event_payloads_included: bool,
}

#[derive(Debug, Deserialize)]
struct StateExportRun {
    run_id: String,
    subject: String,
}

#[derive(Debug, Deserialize)]
struct StateExportEvent {
    #[serde(rename = "@id")]
    id: String,
    kind: String,
    actor: String,
    authority: String,
    payload: Option<Value>,
}

#[derive(Debug, Deserialize)]
struct StateExportArtifact {
    #[serde(rename = "@id")]
    id: String,
    sha256: String,
    #[serde(rename = "contentSize")]
    size_bytes: i64,
    sensitivity: String,
    #[serde(rename = "includedInPackage")]
    included: bool,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
#[serde(deny_unknown_fields)]
struct ObservedAfterReference {
    run_id: String,
    event_id: Uuid,
    sequence: i64,
    hash: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct SnapshotPayload {
    segment: u32,
    sequence: u64,
    trigger: String,
    manifest_sha256: String,
    receipt_sha256: String,
    added_files: u64,
    changed_files: u64,
    removed_files: u64,
    artifact_versions: u64,
    observed_after: Vec<ObservedAfterReference>,
}

#[derive(Debug, Deserialize)]
struct WorkspaceDeltaReceipt {
    schema_version: u32,
    trigger: String,
    artifact_versions: Vec<WorkspaceArtifactVersion>,
    observed_after: Vec<ObservedAfterReference>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct WorkspaceArtifactVersion {
    sha256: String,
    size: i64,
    role: String,
    sensitivity: String,
}

struct CustodyMonitor {
    stop: CancellationToken,
    cancellation: CancellationToken,
    reason: watch::Receiver<Option<StopReason>>,
    handle: JoinHandle<()>,
}

impl CustodyMonitor {
    fn cancellation(&self) -> CancellationToken {
        self.cancellation.clone()
    }

    fn reason(&self) -> Option<StopReason> {
        *self.reason.borrow()
    }

    async fn finish(mut self) -> Option<StopReason> {
        self.stop.cancel();
        let _ = (&mut self.handle).await;
        *self.reason.borrow()
    }
}

impl Drop for CustodyMonitor {
    fn drop(&mut self) {
        self.stop.cancel();
        self.cancellation.cancel();
    }
}

pub async fn run(args: WorkerArgs) -> Result<()> {
    let config = Arc::new(args.resolve()?);
    let direct_dispatch = config
        .direct_dispatch
        .as_ref()
        .map(|direct| DirectDispatchCatalog::load(&direct.catalog, direct.writer_uid))
        .transpose()?
        .map(Arc::new);
    let store = Store::connect(
        &config.database_dsn,
        config.tenant.clone(),
        config.database_pool,
    )?;
    store.ready().await.context("broker schema is not ready")?;
    super::kinds::registry().map_err(|error| anyhow::anyhow!("result kind registry: {error}"))?;
    store
        .ready_for_kinds(false)
        .await
        .context("result kind schema is not ready")?;
    if direct_dispatch.is_some() {
        store
            .ready_for_direct_execution()
            .await
            .context("direct Sandbox runtime event schema is not ready")?;
    }
    if config.query_connection_dir.is_some() {
        store
            .ready_for_queries()
            .await
            .context("guarded query schema is not ready")?;
    }
    if config.live_output {
        store
            .ready_for_live_output(false)
            .await
            .context("live output schema is not ready")?;
    }
    if config.lab_objects_config.is_some() {
        store
            .ready_for_lab_objects()
            .await
            .context("lab objects schema is not ready")?;
    }
    if config.dataset_fetcher_config.is_some() {
        store
            .ready_for_lab_datasets(false)
            .await
            .context("lab dataset schema is not ready")?;
    }
    // The broker writes answer documents itself, so every worker needs their schema (migration 072).
    store
        .ready_for_documents()
        .await
        .context("answer document schema is not ready")?;
    if config.render_url.is_some() {
        store
            .ready_for_figures()
            .await
            .context("figure version schema is not ready")?;
        store
            .ready_for_figure_previews(false)
            .await
            .context("figure preview schema is not ready")?;
        store
            .ready_for_template_analyses(false)
            .await
            .context("template analysis schema is not ready")?;
        // The editor offers what the running render service declares; a service that cannot be read now is read again
        // with each figure version and each template analysis.
        if let Err(stop) = super::figure_run::refresh_catalog(&store, &config).await {
            tracing::warn!(event = "render_catalog_not_read", code = stop.code());
        }
        if let Err(stop) = super::analysis_run::refresh_catalog(&store, &config).await {
            tracing::warn!(event = "analysis_catalog_not_read", code = stop.code());
        }
    }
    let _leader = store
        .acquire_worker_leader()
        .await
        .context("another controller worker owns this tenant")?;
    let shutdown = CancellationToken::new();
    let signal_token = shutdown.clone();
    tokio::spawn(async move {
        shutdown_signal().await;
        signal_token.cancel();
    });

    tracing::info!(
        worker = %config.worker_id,
        max_parallel = config.max_parallel,
        "broker worker became tenant leader"
    );
    // The leader records the kind each new researcher-visible artifact holds.
    let kind_recorder = tokio::spawn(super::kind_record::run(
        store.clone(),
        config.artifact_root.clone(),
        config.worker_id.clone(),
        shutdown.clone(),
    ));
    // Previews are drawn by the worker that reaches the render service.
    let previews = config.render_url.is_some().then(|| {
        tokio::spawn(super::figure_run::serve_previews(
            store.clone(),
            Arc::clone(&config),
            shutdown.clone(),
        ))
    });
    let mut tasks = JoinSet::new();
    // The broker task each spawned task runs, so a task that panics is logged by its broker task.
    let mut running: BTreeMap<tokio::task::Id, Uuid> = BTreeMap::new();
    loop {
        while !shutdown.is_cancelled() && tasks.len() < config.max_parallel {
            match store
                .claim_task(&config.worker_id, config.lease_seconds as i32)
                .await
            {
                Ok(Some(task)) => {
                    let task_store = store.clone();
                    let task_config = Arc::clone(&config);
                    let task_dispatch = direct_dispatch.clone();
                    let task_shutdown = shutdown.clone();
                    let task_id = task.id;
                    let spawned = tasks.spawn(async move {
                        if let Err(error) = execute_task(
                            task_store,
                            task_config,
                            task_dispatch,
                            task,
                            task_shutdown,
                        )
                        .await
                        {
                            tracing::error!(task_id = %task_id, error = %error, "broker task execution failed");
                        }
                    });
                    running.insert(spawned.id(), task_id);
                }
                Ok(None) => break,
                Err(error) => {
                    tracing::warn!(code = error.code, "task claim failed");
                    break;
                }
            }
        }

        if shutdown.is_cancelled() && tasks.is_empty() {
            break;
        }
        tokio::select! {
            _ = shutdown.cancelled(), if !shutdown.is_cancelled() => {},
            result = tasks.join_next_with_id(), if !tasks.is_empty() => {
                forget_joined_task(result, &mut running);
            },
            _ = sleep(config.poll_interval) => {},
        }
    }
    while let Some(result) = tasks.join_next_with_id().await {
        forget_joined_task(Some(result), &mut running);
    }
    if let Err(error) = kind_recorder.await {
        super::log_join_failure(&error, "kind_recorder", None);
    }
    if let Some(Err(error)) = match previews {
        Some(previews) => Some(previews.await),
        None => None,
    } {
        super::log_join_failure(&error, "figure_previews", None);
    }
    Ok(())
}

/// Forget a joined task, logging it by its broker task when it ended without returning.
pub fn forget_joined_task(
    result: Option<Result<(tokio::task::Id, ()), tokio::task::JoinError>>,
    running: &mut BTreeMap<tokio::task::Id, Uuid>,
) {
    match result {
        Some(Ok((id, ()))) => {
            running.remove(&id);
        }
        Some(Err(error)) => {
            let task_id = running.remove(&error.id());
            super::log_join_failure(&error, "task", task_id);
        }
        None => {}
    }
}

async fn execute_task(
    store: Store,
    config: Arc<WorkerConfig>,
    direct_dispatch: Option<Arc<DirectDispatchCatalog>>,
    task: ClaimedTask,
    shutdown: CancellationToken,
) -> Result<()> {
    // A source's query runs in the worker itself; no sandbox or model-written code is involved.
    if task.route == super::query_run::ROUTE {
        return super::query_run::execute(store, &config, task, shutdown).await;
    }
    // A figure version also runs in the worker: the reviewed render service draws it from a retained table.
    if task.route == super::figure_run::ROUTE {
        return super::figure_run::execute(store, &config, task, shutdown).await;
    }
    // A template analysis runs in the worker the same way: a reviewed analysis template in the render
    // service, from a retained table.
    if task.route == super::analysis_run::ROUTE {
        return super::analysis_run::execute(store, &config, task, shutdown).await;
    }
    // An answer document runs in the worker the same way: the reviewed render service writes it from
    // the stored decision and the result tables it pins.
    if task.route == super::document_run::ROUTE {
        return super::document_run::execute(store, &config, task, shutdown).await;
    }
    // A lab dataset's intake or preparation runs in the worker too: it reads only the upload guard's
    // rebuilt grid, and the preparation is the broker's own reviewed executor.
    if task.route == super::dataset_run::ROUTE {
        return super::dataset_run::execute(store, &config, task, shutdown).await;
    }
    // A lab objects step runs in the worker too: it reads the lab's bucket with the method's read identity and keeps
    // the files it names, each checked against the collection's pinned manifest.
    if task.route == super::objects_run::ROUTE {
        return super::objects_run::execute(store, &config, task, shutdown).await;
    }
    if let Some(catalog) = direct_dispatch
        && let Some(prepared) = catalog
            .prepare_for_execution(
                &store,
                &task,
                AdmissionWait {
                    worker_id: &config.worker_id,
                    lease_seconds: config.lease_seconds as i32,
                    poll_interval: config.poll_interval,
                    stop: &shutdown,
                },
            )
            .await?
    {
        return execute_direct_sandbox(store, config, task, shutdown, prepared).await;
    }

    let task_root = config.artifact_root.join("tasks").join(task.id.to_string());
    let attempt_root = task_root
        .join("attempts")
        .join(format!("{}-{}", task.attempt, task.lease_generation));
    crate::config::create_private_dir(&task_root)?;
    crate::config::create_private_dir(&attempt_root)?;
    let objective_path = task_root.join("objective.txt");
    crate::config::write_private(&objective_path, task.objective.as_bytes())?;

    if !store
        .mark_running(task.id, &config.worker_id, task.lease_generation)
        .await
        .context("failed to mark task running")?
    {
        bail!("task lease fence was lost before controller start");
    }

    let create_stdout = attempt_root.join("create.stdout.json");
    let create_stderr = attempt_root.join("create.stderr.log");
    let create_args = create_arguments(&config, &task, &objective_path);
    let create = run_controller(
        &store,
        &config,
        &task,
        &shutdown,
        &create_args,
        &create_stdout,
        &create_stderr,
        CONTROLLER_OPERATION_DEADLINE,
    )
    .await?;
    if create.stop != StopReason::Exited {
        release_runtime(&config, &task.logical_agent).await;
        if matches!(create.stop, StopReason::Cancelled | StopReason::Deadline) {
            register_operator_logs(&store, &config, &task, &attempt_root, None).await?;
        }
        return handle_stopped_process(&store, &config, &task, create.stop).await;
    }
    let status_stdout = attempt_root.join("status.stdout.json");
    let status_stderr = attempt_root.join("status.stderr.log");
    let status_args = vec![
        OsString::from("agent"),
        OsString::from("status"),
        OsString::from(&task.logical_agent),
    ];
    let status = run_controller(
        &store,
        &config,
        &task,
        &shutdown,
        &status_args,
        &status_stdout,
        &status_stderr,
        Duration::from_secs(120),
    )
    .await?;
    if status.stop != StopReason::Exited || !status.status.is_some_and(|value| value.success()) {
        release_runtime(&config, &task.logical_agent).await;
        register_operator_logs(&store, &config, &task, &attempt_root, None).await?;
        store
            .finish_task(
                task.id,
                &config.worker_id,
                task.lease_generation,
                false,
                "controller_status_failed",
                "logical_agent_status_unavailable",
            )
            .await?;
        return Ok(());
    }
    let identity = match read_controller_agent_status(&status_stdout, &task) {
        Ok(identity) => identity,
        Err(_) => {
            release_runtime(&config, &task.logical_agent).await;
            register_operator_logs(&store, &config, &task, &attempt_root, None).await?;
            store
                .finish_task(
                    task.id,
                    &config.worker_id,
                    task.lease_generation,
                    false,
                    "scientific_history_binding_failed",
                    "logical_agent_status_invalid",
                )
                .await?;
            return Ok(());
        }
    };
    let first_segment = i64::try_from(identity.sessions_started)
        .ok()
        .and_then(|value| value.checked_add(1))
        .ok_or_else(|| anyhow!("logical agent provenance segment range is invalid"))?;
    if !store
        .bind_scientific_history(
            &task,
            &config.worker_id,
            &identity.state_run_id,
            &identity.agent_id,
            first_segment,
        )
        .await
        .context("failed to bind logical-agent provenance to broker task")?
    {
        release_runtime(&config, &task.logical_agent).await;
        bail!("task lease fence was lost before scientific history binding");
    }

    let run_stdout = attempt_root.join("run.stdout.json");
    let run_stderr = attempt_root.join("run.stderr.log");
    let run_args = run_arguments(&task);
    let run = run_controller(
        &store,
        &config,
        &task,
        &shutdown,
        &run_args,
        &run_stdout,
        &run_stderr,
        Duration::from_secs(task.max_run_seconds as u64),
    )
    .await?;
    if run.stop != StopReason::Exited {
        release_runtime(&config, &task.logical_agent).await;
        if matches!(run.stop, StopReason::Cancelled | StopReason::Deadline) {
            register_operator_logs(&store, &config, &task, &attempt_root, None).await?;
        }
        return handle_stopped_process(&store, &config, &task, run.stop).await;
    }

    let raw_result = fs::read(&run_stdout).context("failed to read controller task result")?;
    let parsed: Value = serde_json::from_slice(&raw_result).unwrap_or_else(|_| json!({}));
    let mut passed = parsed
        .get("passed")
        .and_then(Value::as_bool)
        .unwrap_or(false)
        && run.status.is_some_and(|status| status.success());
    let mut result_class = parsed
        .get("result_class")
        .and_then(Value::as_str)
        .unwrap_or(if passed {
            "passed"
        } else {
            "controller_result_invalid"
        })
        .to_owned();
    let provenance_run = parsed
        .get("provenance_run")
        .and_then(Value::as_str)
        .map(str::to_owned);
    if let Some(last_segment) = parsed.get("segment").and_then(Value::as_i64) {
        if !store
            .finalize_scientific_history(&task, &config.worker_id, last_segment)
            .await
            .context("failed to finalize logical-agent provenance segment boundary")?
        {
            release_runtime(&config, &task.logical_agent).await;
            bail!("task lease fence was lost before scientific history finalization");
        }
    } else {
        passed = false;
        result_class = "scientific_history_segment_missing".into();
    }
    let mut custody_roots = Vec::new();
    if let Some(run_id) = provenance_run
        .as_deref()
        .filter(|run_id| *run_id == identity.state_run_id)
    {
        for (kind, export_root, export_args) in [
            (
                "state",
                attempt_root.join("state-export"),
                vec![
                    OsString::from("state"),
                    OsString::from("export"),
                    OsString::from(run_id),
                    OsString::from("--output-dir"),
                    attempt_root.join("state-export").into_os_string(),
                    OsString::from("--include-restricted"),
                    OsString::from("--include-event-payloads"),
                ],
            ),
            (
                "evidence",
                attempt_root.join("provenance-export"),
                vec![
                    OsString::from("provenance"),
                    OsString::from("export"),
                    OsString::from(run_id),
                    OsString::from("--output-dir"),
                    attempt_root.join("provenance-export").into_os_string(),
                    OsString::from("--include-restricted"),
                ],
            ),
        ] {
            let export_stdout = attempt_root.join(format!("{kind}.stdout.json"));
            let export_stderr = attempt_root.join(format!("{kind}.stderr.log"));
            match run_controller(
                &store,
                &config,
                &task,
                &shutdown,
                &export_args,
                &export_stdout,
                &export_stderr,
                Duration::from_secs(600),
            )
            .await
            {
                Ok(outcome) if outcome.stop == StopReason::Exited => {
                    if outcome.status.is_some_and(|status| status.success()) {
                        custody_roots.push((kind, export_root));
                    } else {
                        passed = false;
                        result_class = format!("{kind}_export_failed");
                    }
                }
                Ok(outcome) => {
                    release_runtime(&config, &task.logical_agent).await;
                    register_operator_logs(&store, &config, &task, &attempt_root, None).await?;
                    return handle_stopped_process(&store, &config, &task, outcome.stop).await;
                }
                Err(error) => {
                    tracing::warn!(task_id = %task.id, export = kind, error = %error, "task custody export failed");
                    passed = false;
                    result_class = format!("{kind}_export_failed");
                }
            }
        }
    } else {
        passed = false;
        result_class = if provenance_run.is_some() {
            "provenance_run_mismatch".into()
        } else {
            "provenance_run_missing".into()
        };
    }
    let sanitized = json!({
        "schema_version": 1,
        "task_id": task.id,
        "logical_agent": task.logical_agent,
        "passed": passed,
        "result_class": result_class,
        "assistant_output": parsed.get("assistant_output").and_then(Value::as_str).unwrap_or(""),
        "provenance_run": parsed.get("provenance_run").and_then(Value::as_str),
        "segment": parsed.get("segment").and_then(Value::as_u64),
        "attempts": parsed.get("attempts").and_then(Value::as_u64),
        "runtime_released": parsed.get("runtime_released").and_then(Value::as_bool).unwrap_or(false)
    });
    let result_path = task_root.join("result.json");
    let mut result_bytes = serde_json::to_vec_pretty(&sanitized)?;
    result_bytes.push(b'\n');
    crate::config::write_private(&result_path, &result_bytes)?;

    let custody = start_custody_monitor(
        store.clone(),
        Arc::clone(&config),
        task.clone(),
        shutdown.clone(),
    );
    let custody_cancel = custody.cancellation();

    let operator_log_result =
        register_operator_logs(&store, &config, &task, &attempt_root, Some(&custody_cancel)).await;
    if let Some(reason) = custody.reason() {
        release_runtime(&config, &task.logical_agent).await;
        let reason = custody.finish().await.unwrap_or(reason);
        return handle_stopped_process(&store, &config, &task, reason).await;
    }
    operator_log_result?;
    register_one(
        &store,
        &config,
        &task,
        &result_path,
        "researcher",
        "result",
        "Agent result",
        "application/json",
    )
    .await?;
    register_one(
        &store,
        &config,
        &task,
        &objective_path,
        "researcher",
        "request",
        "Task objective",
        "text/plain; charset=utf-8",
    )
    .await?;

    for (role, root) in custody_roots {
        let artifact_root = config.artifact_root.clone();
        let cancellation = custody_cancel.clone();
        let max_files = config.max_workspace_files;
        let max_bytes = config.max_workspace_bytes;
        let evidence_root = root.clone();
        let candidates = tokio::task::spawn_blocking(move || {
            collect_artifact_tree(
                &artifact_root,
                &root,
                "researcher",
                role,
                max_files,
                max_bytes,
                &cancellation,
            )
        })
        .await
        .context("artifact custody scan task failed")?;
        if let Some(reason) = custody.reason() {
            release_runtime(&config, &task.logical_agent).await;
            let reason = custody.finish().await.unwrap_or(reason);
            return handle_stopped_process(&store, &config, &task, reason).await;
        }
        let candidates = candidates?;
        let evidence = if role == "state" {
            parse_state_export_scientific_evidence(
                &evidence_root,
                &identity.state_run_id,
                &identity.agent_id,
                &candidates,
            )?
        } else {
            Vec::new()
        };
        let mut retained_by_sha256 = BTreeMap::new();
        for candidate in candidates {
            if let Some(reason) = custody.reason() {
                release_runtime(&config, &task.logical_agent).await;
                let reason = custody.finish().await.unwrap_or(reason);
                return handle_stopped_process(&store, &config, &task, reason).await;
            }
            let artifact_id = register_candidate(&store, &config, &task, &candidate).await?;
            if role == "state" && candidate.label.starts_with("artifacts/sha256/") {
                retained_by_sha256.insert(candidate.sha256.clone(), artifact_id);
            }
        }
        for draft in evidence {
            let artifact_id = retained_by_sha256
                .get(&draft.artifact_sha256)
                .copied()
                .context("state export evidence artifact was not retained")?;
            if !store
                .record_scientific_history_evidence(
                    &task,
                    &config.worker_id,
                    &ScientificHistoryEvidenceInput {
                        event_id: draft.event_id,
                        event_sequence: draft.event_sequence,
                        event_hash: &draft.event_hash,
                        snapshot_event_id: draft.snapshot_event_id,
                        artifact_id,
                        artifact_sha256: &draft.artifact_sha256,
                        artifact_size_bytes: draft.artifact_size_bytes,
                        artifact_role: &draft.artifact_role,
                    },
                )
                .await
                .context("failed to bind retained state evidence to canonical history")?
            {
                bail!(
                    "task lease or canonical evidence fence was lost before history evidence binding"
                );
            }
        }
    }

    let workspace = config
        .controller_root
        .join("agents")
        .join(&task.logical_agent);
    if workspace.is_dir() {
        let snapshot_root = attempt_root.join("workspace");
        let artifact_root = config.artifact_root.clone();
        let source = workspace.clone();
        let destination = snapshot_root.clone();
        let max_files = config.max_workspace_files;
        let max_bytes = config.max_workspace_bytes;
        let cancellation = custody_cancel.clone();
        let candidates = tokio::task::spawn_blocking(move || {
            snapshot_workspace(
                &artifact_root,
                &source,
                &destination,
                max_files,
                max_bytes,
                &cancellation,
            )
        })
        .await
        .context("workspace snapshot task failed")?;
        if let Some(reason) = custody.reason() {
            release_runtime(&config, &task.logical_agent).await;
            let reason = custody.finish().await.unwrap_or(reason);
            return handle_stopped_process(&store, &config, &task, reason).await;
        }
        let candidates = candidates?;
        for candidate in candidates {
            if let Some(reason) = custody.reason() {
                release_runtime(&config, &task.logical_agent).await;
                let reason = custody.finish().await.unwrap_or(reason);
                return handle_stopped_process(&store, &config, &task, reason).await;
            }
            register_candidate(&store, &config, &task, &candidate).await?;
        }
    }

    let error_code = if passed {
        ""
    } else if result_class.ends_with("_export_failed") || result_class == "provenance_run_missing" {
        "task_custody_incomplete"
    } else {
        "agent_run_failed"
    };
    if let Some(reason) = custody.finish().await {
        release_runtime(&config, &task.logical_agent).await;
        return handle_stopped_process(&store, &config, &task, reason).await;
    }
    if !store
        .finish_task(
            task.id,
            &config.worker_id,
            task.lease_generation,
            passed,
            &result_class,
            error_code,
        )
        .await?
    {
        bail!("task lease fence was lost before result commit");
    }
    Ok(())
}

async fn execute_direct_sandbox(
    store: Store,
    config: Arc<WorkerConfig>,
    task: ClaimedTask,
    shutdown: CancellationToken,
    prepared: PreparedDirectDispatch,
) -> Result<()> {
    let terminal_export = prepared
        .terminal_export
        .ok_or_else(|| anyhow!("direct_dispatch_terminal_export_missing"))?;
    let limits = ExportLimits {
        max_files: usize::try_from(config.max_workspace_files)
            .map_err(|_| anyhow!("direct_dispatch_export_limits_invalid"))?,
        max_bytes: config.max_workspace_bytes,
    };
    if limits.max_files > 100_000 || limits.max_bytes > i64::MAX as u64 {
        bail!("direct_dispatch_export_limits_invalid");
    }
    // A second client of the same identity reads the runner's log when live output is on.
    let logs: Option<Arc<dyn super::live_output::PodLogs>> = if config.live_output {
        Some(Arc::new(KubernetesHttpTransport::from_service_account(
            prepared.kubernetes.origin.clone(),
            prepared.binding.namespace.clone(),
            prepared.kubernetes.token_path.clone(),
            &prepared.kubernetes.ca_path,
        )?))
    } else {
        None
    };
    let transport = KubernetesHttpTransport::from_service_account(
        prepared.kubernetes.origin,
        prepared.binding.namespace.clone(),
        prepared.kubernetes.token_path,
        &prepared.kubernetes.ca_path,
    )?;
    let runtime = SandboxRuntime::new(
        transport,
        SandboxBindingCatalog::new(vec![prepared.binding.clone()])?,
        config.lease_seconds as i32,
    )?;
    let factory = StagedTerminalRetainerFactory::new(
        terminal_export,
        &task,
        &prepared.binding,
        &prepared.admission,
        config.artifact_root.clone(),
        limits,
    )?;
    let driver = DirectExecutionDriver::new(DirectExecutionTiming {
        lease_seconds: config.lease_seconds as i32,
        poll_interval: config.poll_interval,
        maximum_wait: Duration::from_secs(task.max_run_seconds as u64),
    })?;
    let cancellation = CancellationToken::new();
    let recorder =
        super::live_output::LiveOutputRecorder::new(&store, Arc::new(store.clone()), logs);
    match driver
        .run(
            &runtime,
            &store,
            &recorder,
            &task,
            &config.worker_id,
            &prepared.admission,
            DirectExecutionControl {
                cancellation: &cancellation,
                shutdown: &shutdown,
            },
            &factory,
        )
        .await?
    {
        DirectExecutionOutcome::Finalized { completion, .. } => {
            tracing::info!(
                task_id = %task.id,
                terminal_succeeded = completion.succeeded(),
                "direct Sandbox task finalized"
            );
        }
        DirectExecutionOutcome::RecoveryNeeded { handle, reason } => {
            tracing::warn!(
                task_id = %task.id,
                handle_present = handle.is_some(),
                recovery = ?reason,
                "direct Sandbox task requires recovery"
            );
        }
    }
    Ok(())
}

fn read_controller_agent_status(path: &Path, task: &ClaimedTask) -> Result<ControllerAgentStatus> {
    let bytes = fs::read(path).context("failed to read controller agent status")?;
    let status: ControllerAgentStatus = serde_json::from_slice(&bytes)
        .context("controller agent status is not a supported JSON record")?;
    let expected_owner = task.principal_id.to_string();
    if status.name != task.logical_agent
        || status.owner != expected_owner
        || status.domain != "scientific"
        || status.state_profile != task.state_profile
        || status.lifecycle != "idle"
        || status.active_sandbox.is_some()
        || status.sessions_completed != status.sessions_started
        || !is_agent_identifier(&status.agent_id)
        || status.state_run_id != status.agent_id
    {
        bail!("logical agent status does not prove a bound scientific provenance identity");
    }
    Ok(status)
}

fn is_agent_identifier(value: &str) -> bool {
    value.len() == 38
        && value.starts_with("agent-")
        && value.as_bytes()[6..]
            .iter()
            .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
}

fn create_arguments(config: &WorkerConfig, task: &ClaimedTask, objective: &Path) -> Vec<OsString> {
    let mut arguments = vec![
        OsString::from("agent"),
        OsString::from("create"),
        OsString::from(&task.logical_agent),
        OsString::from("--owner"),
        OsString::from(task.principal_id.to_string()),
        OsString::from("--domain"),
        OsString::from("scientific"),
        OsString::from("--state-profile"),
        OsString::from(&task.state_profile),
        OsString::from("--objective-file"),
        objective.as_os_str().to_owned(),
        OsString::from("--role"),
        OsString::from(&task.role),
        OsString::from("--harness"),
        OsString::from(&task.harness),
        OsString::from("--route"),
        OsString::from(&task.route),
        OsString::from("--profile"),
        OsString::from(&task.controller_profile),
        OsString::from("--gateway-profile"),
        OsString::from(&task.gateway_profile),
        OsString::from("--image"),
        OsString::from(&task.image_ref),
        OsString::from("--cpu"),
        OsString::from(&task.cpu_limit),
        OsString::from("--memory"),
        OsString::from(&task.memory_limit),
    ];
    if let Some(thinking) = &task.thinking {
        arguments.push(OsString::from("--thinking"));
        arguments.push(OsString::from(thinking));
    }
    let _ = config;
    arguments
}

fn run_arguments(task: &ClaimedTask) -> Vec<OsString> {
    let mut arguments = vec![
        OsString::from("agent"),
        OsString::from("run"),
        OsString::from(&task.logical_agent),
        OsString::from("--harness"),
        OsString::from(&task.harness),
        OsString::from("--max-attempts"),
        OsString::from(task.max_provider_attempts.to_string()),
        OsString::from("--attempt-timeout-seconds"),
        OsString::from(task.max_run_seconds.to_string()),
        OsString::from("--retry-delay-ms"),
        OsString::from("1000"),
    ];
    if let Some(thinking) = &task.thinking {
        arguments.push(OsString::from("--thinking"));
        arguments.push(OsString::from(thinking));
    }
    arguments.push(OsString::from("--"));
    arguments.push(OsString::from(TASK_MESSAGE));
    arguments
}

#[allow(clippy::too_many_arguments)]
async fn run_controller(
    store: &Store,
    config: &WorkerConfig,
    task: &ClaimedTask,
    shutdown: &CancellationToken,
    arguments: &[OsString],
    stdout_path: &Path,
    stderr_path: &Path,
    deadline: Duration,
) -> Result<ProcessOutcome> {
    let stdout = private_output_file(stdout_path)?;
    let stderr = private_output_file(stderr_path)?;
    let mut command = Command::new(&config.controller_bin);
    command
        .arg("--root")
        .arg(&config.controller_root)
        .arg("--state-dir")
        .arg(&config.controller_state)
        .arg("--config-dir")
        .arg(&config.controller_config)
        .arg("--openshell-bin")
        .arg(&config.openshell_bin)
        .arg("--docker-bin")
        .arg(&config.docker_bin)
        .arg("--output")
        .arg("json")
        .args(arguments)
        .stdin(Stdio::null())
        .stdout(Stdio::from(stdout))
        .stderr(Stdio::from(stderr))
        .kill_on_drop(false);
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        command.as_std_mut().process_group(0);
    }
    let mut child = command.spawn().with_context(|| {
        format!(
            "failed to start controller binary {}",
            config.controller_bin.display()
        )
    })?;
    let expires_at = Instant::now() + deadline;
    let mut lease_checks = interval(Duration::from_secs((config.lease_seconds / 3).max(5)));
    lease_checks.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    lease_checks.tick().await;
    let mut cancel_checks = interval(Duration::from_secs(2));
    cancel_checks.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    cancel_checks.tick().await;
    loop {
        tokio::select! {
            status = child.wait() => {
                return Ok(ProcessOutcome { status: Some(status?), stop: StopReason::Exited });
            }
            _ = shutdown.cancelled() => {
                terminate_process(&mut child).await;
                return Ok(ProcessOutcome { status: None, stop: StopReason::Shutdown });
            }
            _ = tokio::time::sleep_until(expires_at) => {
                terminate_process(&mut child).await;
                return Ok(ProcessOutcome { status: None, stop: StopReason::Deadline });
            }
            _ = lease_checks.tick() => {
                let renewed = store.renew_task(
                    task.id,
                    &config.worker_id,
                    task.lease_generation,
                    config.lease_seconds as i32,
                ).await;
                match renewed {
                    Ok(true) => {},
                    Ok(false) => {
                        terminate_process(&mut child).await;
                        return Ok(ProcessOutcome { status: None, stop: StopReason::LeaseLost });
                    }
                    Err(error) => {
                        tracing::warn!(task_id = %task.id, code = error.code, "task lease renewal failed");
                        terminate_process(&mut child).await;
                        return Ok(ProcessOutcome { status: None, stop: StopReason::LeaseLost });
                    }
                }
            }
            _ = cancel_checks.tick() => {
                match store.cancel_requested(
                    task.id,
                    &config.worker_id,
                    task.lease_generation,
                ).await {
                    Ok(true) => {
                        terminate_process(&mut child).await;
                        return Ok(ProcessOutcome { status: None, stop: StopReason::Cancelled });
                    }
                    Ok(false) => {},
                    Err(error) if error.kind == StoreErrorKind::Unavailable => {
                        terminate_process(&mut child).await;
                        return Ok(ProcessOutcome { status: None, stop: StopReason::LeaseLost });
                    }
                    Err(error) => {
                        tracing::warn!(task_id = %task.id, code = error.code, "task cancellation check failed");
                        terminate_process(&mut child).await;
                        return Ok(ProcessOutcome { status: None, stop: StopReason::LeaseLost });
                    }
                }
            }
        }
    }
}

async fn handle_stopped_process(
    store: &Store,
    config: &WorkerConfig,
    task: &ClaimedTask,
    reason: StopReason,
) -> Result<()> {
    match reason {
        StopReason::Cancelled => {
            store
                .finish_task(
                    task.id,
                    &config.worker_id,
                    task.lease_generation,
                    false,
                    "cancelled",
                    "cancelled_by_researcher",
                )
                .await?;
        }
        StopReason::Deadline => {
            store
                .finish_task(
                    task.id,
                    &config.worker_id,
                    task.lease_generation,
                    false,
                    "deadline_exceeded",
                    "task_deadline_exceeded",
                )
                .await?;
        }
        StopReason::LeaseLost | StopReason::Shutdown => {
            // The lease expiry path requeues or closes the task. A stale worker must
            // not commit a result after losing its generation fence.
        }
        StopReason::Exited => {}
    }
    Ok(())
}

async fn release_runtime(config: &WorkerConfig, logical_agent: &str) {
    let output = Command::new(&config.controller_bin)
        .arg("--root")
        .arg(&config.controller_root)
        .arg("--state-dir")
        .arg(&config.controller_state)
        .arg("--config-dir")
        .arg(&config.controller_config)
        .arg("--openshell-bin")
        .arg(&config.openshell_bin)
        .arg("--docker-bin")
        .arg(&config.docker_bin)
        .arg("--output")
        .arg("json")
        .arg("agent")
        .arg("release")
        .arg(logical_agent)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .kill_on_drop(true)
        .status();
    let _ = timeout(Duration::from_secs(60), output).await;
}

fn start_custody_monitor(
    store: Store,
    config: Arc<WorkerConfig>,
    task: ClaimedTask,
    shutdown: CancellationToken,
) -> CustodyMonitor {
    let stop = CancellationToken::new();
    let cancellation = CancellationToken::new();
    let (reason_sender, reason) = watch::channel(None);
    let monitor_stop = stop.clone();
    let monitor_cancellation = cancellation.clone();
    let handle = tokio::spawn(async move {
        let mut lease_checks = interval(Duration::from_secs((config.lease_seconds / 3).max(5)));
        lease_checks.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        lease_checks.tick().await;
        let mut cancel_checks = interval(Duration::from_secs(2));
        cancel_checks.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        cancel_checks.tick().await;
        let stop_reason = loop {
            tokio::select! {
                _ = monitor_stop.cancelled() => break None,
                _ = shutdown.cancelled() => break Some(StopReason::Shutdown),
                _ = lease_checks.tick() => {
                    match store.renew_task(
                        task.id,
                        &config.worker_id,
                        task.lease_generation,
                        config.lease_seconds as i32,
                    ).await {
                        Ok(true) => {},
                        Ok(false) => break Some(StopReason::LeaseLost),
                        Err(error) => {
                            tracing::warn!(task_id = %task.id, code = error.code, "task lease renewal failed during custody");
                            break Some(StopReason::LeaseLost);
                        }
                    }
                }
                _ = cancel_checks.tick() => {
                    match store.cancel_requested(
                        task.id,
                        &config.worker_id,
                        task.lease_generation,
                    ).await {
                        Ok(true) => break Some(StopReason::Cancelled),
                        Ok(false) => {},
                        Err(error) => {
                            tracing::warn!(task_id = %task.id, code = error.code, "task cancellation check failed during custody");
                            break Some(StopReason::LeaseLost);
                        }
                    }
                }
            }
        };
        if let Some(reason) = stop_reason {
            reason_sender.send_replace(Some(reason));
            monitor_cancellation.cancel();
        }
    });
    CustodyMonitor {
        stop,
        cancellation,
        reason,
        handle,
    }
}

async fn register_operator_logs(
    store: &Store,
    config: &WorkerConfig,
    task: &ClaimedTask,
    attempt_root: &Path,
    cancellation: Option<&CancellationToken>,
) -> Result<()> {
    let artifact_root = config.artifact_root.clone();
    let attempt_root = attempt_root.to_path_buf();
    let cancellation = cancellation.cloned().unwrap_or_else(CancellationToken::new);
    let candidates = tokio::task::spawn_blocking(move || {
        collect_operator_log_candidates(&artifact_root, &attempt_root, &cancellation)
    })
    .await
    .context("operator-log custody scan task failed")??;
    for candidate in candidates {
        register_candidate(store, config, task, &candidate).await?;
    }
    Ok(())
}

fn collect_operator_log_candidates(
    artifact_root: &Path,
    attempt_root: &Path,
    cancellation: &CancellationToken,
) -> Result<Vec<ArtifactCandidate>> {
    let mut candidates = Vec::new();
    for entry in fs::read_dir(attempt_root)? {
        ensure_custody_active(cancellation)?;
        let path = entry?.path();
        if !path.is_file() {
            continue;
        }
        let label = path
            .file_name()
            .and_then(|value| value.to_str())
            .unwrap_or("controller log");
        let media_type = if path.extension().and_then(|value| value.to_str()) == Some("json") {
            "application/json"
        } else {
            "text/plain; charset=utf-8"
        };
        candidates.push(artifact_candidate(
            artifact_root,
            &path,
            "operator",
            if label.contains("stderr") {
                "stderr"
            } else {
                "stdout"
            },
            label,
            media_type,
            cancellation,
        )?);
    }
    Ok(candidates)
}

#[allow(clippy::too_many_arguments)]
async fn register_one(
    store: &Store,
    config: &WorkerConfig,
    task: &ClaimedTask,
    path: &Path,
    visibility: &'static str,
    role: &'static str,
    label: &str,
    media_type: &str,
) -> Result<()> {
    let cancellation = CancellationToken::new();
    let candidate = artifact_candidate(
        &config.artifact_root,
        path,
        visibility,
        role,
        label,
        media_type,
        &cancellation,
    )?;
    let _ = register_candidate(store, config, task, &candidate).await?;
    Ok(())
}

async fn register_candidate(
    store: &Store,
    config: &WorkerConfig,
    task: &ClaimedTask,
    candidate: &ArtifactCandidate,
) -> Result<Uuid> {
    let _ = config;
    Ok(store
        .register_artifact(
            task.id,
            &config.worker_id,
            task.lease_generation,
            candidate.visibility,
            candidate.role,
            &candidate.label,
            &candidate.relative_path,
            &candidate.media_type,
            candidate.size_bytes,
            &candidate.sha256,
        )
        .await?)
}

fn snapshot_workspace(
    artifact_root: &Path,
    source: &Path,
    destination: &Path,
    max_files: u64,
    max_bytes: u64,
    cancellation: &CancellationToken,
) -> Result<Vec<ArtifactCandidate>> {
    let source = source
        .canonicalize()
        .context("failed to resolve logical-agent workspace")?;
    crate::config::create_private_dir(destination)?;
    let destination = destination
        .canonicalize()
        .context("failed to resolve workspace snapshot root")?;
    let mut files = 0_u64;
    let mut bytes = 0_u64;
    let mut candidates = Vec::new();
    for entry in WalkDir::new(&source).follow_links(false) {
        ensure_custody_active(cancellation)?;
        let entry = entry?;
        let metadata = fs::symlink_metadata(entry.path())?;
        if metadata.file_type().is_symlink() {
            bail!("workspace snapshot rejected a symbolic link");
        }
        let relative = entry.path().strip_prefix(&source)?;
        if relative.as_os_str().is_empty() {
            continue;
        }
        validate_relative_path(relative)?;
        let target = destination.join(relative);
        if metadata.is_dir() {
            crate::config::create_private_dir(&target)?;
            continue;
        }
        if !metadata.is_file() {
            bail!("workspace snapshot rejected a non-regular file");
        }
        files = files
            .checked_add(1)
            .ok_or_else(|| anyhow!("workspace file count overflow"))?;
        if files > max_files {
            bail!("workspace artifact limits were exceeded");
        }
        if let Some(parent) = target.parent() {
            crate::config::create_private_dir(parent)?;
        }
        let (copied_bytes, sha256) =
            copy_and_hash_file(entry.path(), &target, cancellation, max_bytes - bytes)?;
        bytes = bytes
            .checked_add(copied_bytes)
            .ok_or_else(|| anyhow!("workspace byte count overflow"))?;
        let label = relative.to_string_lossy().to_string();
        let media_type = media_type_for(&target).to_string();
        candidates.push(artifact_candidate_with_digest(
            artifact_root,
            &target,
            "researcher",
            "workspace",
            &label,
            &media_type,
            copied_bytes,
            sha256,
        )?);
    }
    Ok(candidates)
}

fn collect_artifact_tree(
    artifact_root: &Path,
    source: &Path,
    visibility: &'static str,
    role: &'static str,
    max_files: u64,
    max_bytes: u64,
    cancellation: &CancellationToken,
) -> Result<Vec<ArtifactCandidate>> {
    let canonical_artifact_root = artifact_root.canonicalize()?;
    let source = source
        .canonicalize()
        .context("failed to resolve artifact export root")?;
    if !source.starts_with(&canonical_artifact_root) {
        bail!("artifact export is outside the configured artifact root");
    }
    let mut files = 0_u64;
    let mut bytes = 0_u64;
    let mut candidates = Vec::new();
    for entry in WalkDir::new(&source).follow_links(false) {
        ensure_custody_active(cancellation)?;
        let entry = entry?;
        let metadata = fs::symlink_metadata(entry.path())?;
        if metadata.file_type().is_symlink() {
            bail!("artifact export rejected a symbolic link");
        }
        if metadata.is_dir() {
            continue;
        }
        if !metadata.is_file() {
            bail!("artifact export rejected a non-regular file");
        }
        files = files
            .checked_add(1)
            .ok_or_else(|| anyhow!("artifact export file count overflow"))?;
        bytes = bytes
            .checked_add(metadata.len())
            .ok_or_else(|| anyhow!("artifact export byte count overflow"))?;
        if files > max_files || bytes > max_bytes {
            bail!("artifact export limits were exceeded");
        }
        let label = entry.path().strip_prefix(&source)?.to_string_lossy();
        candidates.push(artifact_candidate(
            artifact_root,
            entry.path(),
            visibility,
            role,
            &label,
            media_type_for(entry.path()),
            cancellation,
        )?);
    }
    Ok(candidates)
}

fn parse_state_export_scientific_evidence(
    root: &Path,
    expected_run_id: &str,
    expected_subject: &str,
    candidates: &[ArtifactCandidate],
) -> Result<Vec<ScientificEvidenceDraft>> {
    let manifest = parse_state_export_manifest(root, candidates)?;
    let provenance = read_state_export_file(root, "provenance.json", candidates, &manifest)?;
    let document: StateExportDocument = serde_json::from_slice(&provenance)
        .context("state export provenance document is invalid")?;
    if document.schema_version != 1
        || !document.event_payloads_included
        || document.run.run_id != expected_run_id
        || document.run.subject != expected_subject
    {
        bail!("state export provenance binding is invalid");
    }
    let artifacts = document
        .artifacts
        .into_iter()
        .map(|artifact| (artifact.sha256.clone(), artifact))
        .collect::<BTreeMap<_, _>>();
    let mut drafts = Vec::new();
    for event in document.events {
        if event.kind != "workspace.snapshot.captured"
            || event.actor != "host-observer"
            || event.authority != "machine"
        {
            continue;
        }
        let payload: SnapshotPayload = serde_json::from_value(
            event
                .payload
                .context("machine snapshot event has no closed payload")?,
        )
        .context("machine snapshot event payload is invalid")?;
        if payload.observed_after.is_empty() {
            continue;
        }
        if payload.segment == 0
            || payload.sequence == 0
            || payload.trigger != "tool-completed"
            || !is_sha256(&payload.manifest_sha256)
            || !is_sha256(&payload.receipt_sha256)
        {
            bail!("machine snapshot event does not describe a completed tool observation");
        }
        let versioned_file_count = payload
            .added_files
            .checked_add(payload.changed_files)
            .context("machine snapshot file counts overflow")?;
        let changed_file_count = versioned_file_count
            .checked_add(payload.removed_files)
            .context("machine snapshot file counts overflow")?;
        if changed_file_count == 0 {
            bail!("machine snapshot event has no observed workspace change");
        }
        let snapshot_event_id = export_event_id(&event.id)?;
        validate_observed_after(&payload.observed_after, expected_run_id)?;
        let receipt_entity = artifacts
            .get(&payload.receipt_sha256)
            .filter(|artifact| {
                artifact.included
                    && artifact.sensitivity == "standard"
                    && artifact.id == format!("artifacts/sha256/{}", payload.receipt_sha256)
            })
            .context("machine snapshot receipt is not a retained standard state artifact")?;
        let receipt_path = format!("artifacts/sha256/{}", payload.receipt_sha256);
        let receipt_bytes = read_state_export_file(root, &receipt_path, candidates, &manifest)?;
        if receipt_entity.size_bytes != i64::try_from(receipt_bytes.len())? {
            bail!("machine snapshot receipt size changed after export");
        }
        let receipt: WorkspaceDeltaReceipt = serde_json::from_slice(&receipt_bytes)
            .context("machine snapshot receipt is invalid")?;
        if receipt.schema_version != 2
            || receipt.trigger != "tool-completed"
            || receipt.observed_after != payload.observed_after
            || receipt.artifact_versions.len() as u64 != payload.artifact_versions
            || payload.artifact_versions != versioned_file_count
        {
            bail!("machine snapshot receipt does not match the state event");
        }
        for version in receipt.artifact_versions {
            if version.sensitivity != "standard"
                || !is_sha256(&version.sha256)
                || version.size < 0
                || !valid_evidence_role(&version.role)
            {
                bail!("machine snapshot has an invalid public artifact version");
            }
            let artifact = artifacts
                .get(&version.sha256)
                .filter(|artifact| {
                    artifact.included
                        && artifact.sensitivity == "standard"
                        && artifact.size_bytes == version.size
                        && artifact.id == format!("artifacts/sha256/{}", version.sha256)
                })
                .context("machine snapshot artifact version is not retained")?;
            let artifact_path = format!("artifacts/sha256/{}", version.sha256);
            let retained = read_state_export_file(root, &artifact_path, candidates, &manifest)?;
            if i64::try_from(retained.len())? != artifact.size_bytes {
                bail!("machine snapshot artifact size changed after export");
            }
            for observed in &payload.observed_after {
                drafts.push(ScientificEvidenceDraft {
                    event_id: observed.event_id,
                    event_sequence: observed.sequence,
                    event_hash: observed.hash.clone(),
                    snapshot_event_id,
                    artifact_sha256: version.sha256.clone(),
                    artifact_size_bytes: version.size,
                    artifact_role: version.role.clone(),
                });
            }
        }
    }
    Ok(drafts)
}

fn parse_state_export_manifest(
    root: &Path,
    candidates: &[ArtifactCandidate],
) -> Result<BTreeMap<String, String>> {
    let manifest =
        read_state_export_file_without_manifest(root, "manifest-sha256.txt", candidates)?;
    let mut entries = BTreeMap::new();
    for line in std::str::from_utf8(&manifest)
        .context("state export manifest is not UTF-8")?
        .lines()
    {
        let (sha256, path) = line
            .split_once("  ")
            .context("state export manifest entry is invalid")?;
        if !is_sha256(sha256)
            || !valid_export_path(path)
            || entries.insert(path.into(), sha256.into()).is_some()
        {
            bail!("state export manifest entry is invalid");
        }
    }
    Ok(entries)
}

fn read_state_export_file(
    root: &Path,
    relative: &str,
    candidates: &[ArtifactCandidate],
    manifest: &BTreeMap<String, String>,
) -> Result<Vec<u8>> {
    let expected = manifest
        .get(relative)
        .context("state export file is absent from its manifest")?;
    let bytes = read_state_export_file_without_manifest(root, relative, candidates)?;
    if sha256_bytes(&bytes) != *expected {
        bail!("state export file does not match its manifest");
    }
    Ok(bytes)
}

fn read_state_export_file_without_manifest(
    root: &Path,
    relative: &str,
    candidates: &[ArtifactCandidate],
) -> Result<Vec<u8>> {
    if !valid_export_path(relative) {
        bail!("state export path is invalid");
    }
    let candidate = candidates
        .iter()
        .filter(|candidate| candidate.label == relative && candidate.role == "state")
        .collect::<Vec<_>>();
    let [candidate] = candidate.as_slice() else {
        bail!("state export file was not retained exactly once");
    };
    let canonical_root = root
        .canonicalize()
        .context("state export root is unavailable")?;
    let path = root.join(relative);
    let metadata = fs::symlink_metadata(&path)?;
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        bail!("state export file is not a regular file");
    }
    let canonical = path.canonicalize()?;
    if !canonical.starts_with(&canonical_root) {
        bail!("state export file escapes its root");
    }
    let bytes = fs::read(&canonical)?;
    if i64::try_from(bytes.len())? != candidate.size_bytes
        || sha256_bytes(&bytes) != candidate.sha256
    {
        bail!("state export file changed after custody collection");
    }
    Ok(bytes)
}

fn validate_observed_after(
    references: &[ObservedAfterReference],
    expected_run_id: &str,
) -> Result<()> {
    if references.is_empty()
        || references.iter().any(|reference| {
            reference.run_id != expected_run_id
                || reference.sequence < 1
                || !is_sha256(&reference.hash)
        })
    {
        bail!("machine snapshot observed-after references are invalid");
    }
    Ok(())
}

fn export_event_id(value: &str) -> Result<Uuid> {
    Uuid::parse_str(
        value
            .strip_prefix("#event-")
            .context("state export event identity is invalid")?,
    )
    .context("state export event identity is invalid")
}

fn valid_export_path(path: &str) -> bool {
    !path.is_empty()
        && !path.starts_with('/')
        && path
            .split('/')
            .all(|part| !part.is_empty() && part != "." && part != "..")
}

fn valid_evidence_role(role: &str) -> bool {
    !role.is_empty()
        && role.len() <= 128
        && role
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
}

fn is_sha256(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn sha256_bytes(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}

fn artifact_candidate(
    artifact_root: &Path,
    path: &Path,
    visibility: &'static str,
    role: &'static str,
    label: &str,
    media_type: &str,
    cancellation: &CancellationToken,
) -> Result<ArtifactCandidate> {
    let metadata = fs::symlink_metadata(path)?;
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        bail!("artifact must be a regular non-symlink file");
    }
    let canonical_root = artifact_root.canonicalize()?;
    let canonical = path.canonicalize()?;
    if !canonical.starts_with(&canonical_root) {
        bail!("artifact is outside the configured artifact root");
    }
    let relative = canonical.strip_prefix(&canonical_root)?;
    validate_relative_path(relative)?;
    let (size_bytes, sha256) = hash_file(&canonical, cancellation)?;
    if size_bytes != metadata.len() {
        bail!("artifact changed while it was hashed");
    }
    artifact_candidate_with_digest(
        artifact_root,
        &canonical,
        visibility,
        role,
        label,
        media_type,
        size_bytes,
        sha256,
    )
}

#[allow(clippy::too_many_arguments)]
fn artifact_candidate_with_digest(
    artifact_root: &Path,
    path: &Path,
    visibility: &'static str,
    role: &'static str,
    label: &str,
    media_type: &str,
    size_bytes: u64,
    sha256: String,
) -> Result<ArtifactCandidate> {
    let canonical_root = artifact_root.canonicalize()?;
    let canonical = path.canonicalize()?;
    if !canonical.starts_with(&canonical_root) {
        bail!("artifact is outside the configured artifact root");
    }
    let relative = canonical.strip_prefix(&canonical_root)?;
    validate_relative_path(relative)?;
    Ok(ArtifactCandidate {
        visibility,
        role,
        label: label.chars().take(256).collect(),
        relative_path: relative
            .components()
            .map(|component| component.as_os_str().to_string_lossy())
            .collect::<Vec<_>>()
            .join("/"),
        media_type: media_type.into(),
        size_bytes: i64::try_from(size_bytes).context("artifact is too large")?,
        sha256,
    })
}

fn validate_relative_path(path: &Path) -> Result<()> {
    if path.is_absolute()
        || path.as_os_str().is_empty()
        || path
            .components()
            .any(|component| !matches!(component, Component::Normal(_)))
    {
        bail!("artifact relative path is invalid");
    }
    Ok(())
}

fn hash_file(path: &Path, cancellation: &CancellationToken) -> Result<(u64, String)> {
    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NOFOLLOW);
    }
    let mut file = options.open(path)?;
    if !file.metadata()?.is_file() {
        bail!("artifact must remain a regular file while it is hashed");
    }
    let mut digest = Sha256::new();
    let mut buffer = [0_u8; 64 * 1024];
    let mut bytes = 0_u64;
    loop {
        ensure_custody_active(cancellation)?;
        let read = file.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        digest.update(&buffer[..read]);
        bytes = bytes
            .checked_add(read as u64)
            .ok_or_else(|| anyhow!("artifact byte count overflow"))?;
    }
    Ok((bytes, hex::encode(digest.finalize())))
}

fn copy_and_hash_file(
    source: &Path,
    target: &Path,
    cancellation: &CancellationToken,
    byte_limit: u64,
) -> Result<(u64, String)> {
    ensure_custody_active(cancellation)?;
    let mut source_options = OpenOptions::new();
    source_options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        source_options.custom_flags(libc::O_NOFOLLOW);
    }
    let mut source_file = source_options.open(source)?;
    let source_metadata = source_file.metadata()?;
    if !source_metadata.is_file() {
        bail!("workspace source must remain a regular file");
    }
    if source_metadata.len() > byte_limit {
        bail!("workspace artifact limits were exceeded");
    }

    let mut target_options = OpenOptions::new();
    target_options.create_new(true).write(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        target_options.mode(0o600);
    }
    let mut target_file = target_options.open(target)?;
    let copy_result = (|| -> Result<(u64, String)> {
        let mut digest = Sha256::new();
        let mut buffer = [0_u8; 64 * 1024];
        let mut bytes = 0_u64;
        loop {
            ensure_custody_active(cancellation)?;
            let read = source_file.read(&mut buffer)?;
            if read == 0 {
                break;
            }
            bytes = bytes
                .checked_add(read as u64)
                .ok_or_else(|| anyhow!("workspace byte count overflow"))?;
            if bytes > byte_limit {
                bail!("workspace artifact limits were exceeded");
            }
            target_file.write_all(&buffer[..read])?;
            digest.update(&buffer[..read]);
        }
        target_file.sync_all()?;
        if source_file.metadata()?.len() != bytes {
            bail!("workspace source changed while it was copied");
        }
        Ok((bytes, hex::encode(digest.finalize())))
    })();
    if copy_result.is_err() {
        drop(target_file);
        let _ = fs::remove_file(target);
    }
    copy_result
}

fn ensure_custody_active(cancellation: &CancellationToken) -> Result<()> {
    if cancellation.is_cancelled() {
        bail!("artifact custody operation was cancelled");
    }
    Ok(())
}

fn media_type_for(path: &Path) -> &'static str {
    match path
        .extension()
        .and_then(|value| value.to_str())
        .unwrap_or_default()
    {
        "json" | "jsonl" => "application/json",
        "csv" => "text/csv; charset=utf-8",
        "tsv" => "text/tab-separated-values; charset=utf-8",
        "txt" | "md" | "log" | "py" | "r" | "rs" | "sql" => "text/plain; charset=utf-8",
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "svg" => "image/svg+xml",
        "pdf" => "application/pdf",
        "parquet" => "application/vnd.apache.parquet",
        _ => "application/octet-stream",
    }
}

fn private_output_file(path: &Path) -> Result<File> {
    let mut options = OpenOptions::new();
    options.create(true).truncate(true).write(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(path)?;
    file.flush()?;
    crate::config::set_mode(path, 0o600)?;
    Ok(file)
}

async fn terminate_process(child: &mut Child) {
    #[cfg(unix)]
    if let Some(process_id) = child.id() {
        // The child is placed in a fresh process group before spawn, so this
        // signal cannot target the worker or unrelated host processes.
        unsafe {
            libc::kill(-(process_id as i32), libc::SIGTERM);
        }
        if timeout(Duration::from_secs(10), child.wait()).await.is_ok() {
            return;
        }
        unsafe {
            libc::kill(-(process_id as i32), libc::SIGKILL);
        }
    }
    #[cfg(not(unix))]
    {
        let _ = child.start_kill();
    }
    let _ = timeout(Duration::from_secs(5), child.wait()).await;
}

async fn shutdown_signal() {
    let interrupt = async {
        tokio::signal::ctrl_c().await.ok();
    };
    #[cfg(unix)]
    let terminate = async {
        if let Ok(mut signal) =
            tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
        {
            signal.recv().await;
        } else {
            std::future::pending::<()>().await;
        }
    };
    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();
    tokio::select! {
        _ = interrupt => {},
        _ = terminate => {},
    }
}

#[cfg(test)]
mod tests {
    use std::fs;

    use tempfile::tempdir;
    use tokio_util::sync::CancellationToken;

    use super::{
        ClaimedTask, collect_artifact_tree, media_type_for, read_controller_agent_status,
        snapshot_workspace,
    };
    use uuid::Uuid;

    #[test]
    fn controller_status_binds_only_idle_server_issued_identity() {
        let root = tempdir().expect("status root");
        let task_id = Uuid::new_v4();
        let principal = Uuid::new_v4();
        let agent = format!("task-{}", task_id.simple());
        let run = format!("agent-{}", Uuid::new_v4().simple());
        let task = ClaimedTask {
            id: task_id,
            principal_id: principal,
            group_key: "research".into(),
            profile_key: "reference".into(),
            objective: "synthetic objective".into(),
            logical_agent: agent.clone(),
            lease_generation: 1,
            attempt: 1,
            max_task_attempts: 1,
            max_provider_attempts: 1,
            harness: "pi".into(),
            thinking: None,
            route: "local".into(),
            controller_profile: "controller".into(),
            gateway_profile: "gateway".into(),
            image_ref: "registry.test/controller@sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa".into(),
            cpu_limit: "500m".into(),
            memory_limit: "512Mi".into(),
            state_profile: "scientific-worker".into(),
            role: "researcher".into(),
            max_run_seconds: 30,
        };
        let status = root.path().join("status.json");
        fs::write(
            &status,
            serde_json::json!({
                "agent_id":run,
                "name":agent,
                "owner":principal.to_string(),
                "domain":"scientific",
                "state_profile":"scientific-worker",
                "state_run_id":run,
                "active_sandbox":null,
                "sessions_started":3,
                "sessions_completed":3,
                "lifecycle":"idle",
                "objective":"must stay in the controller status file"
            })
            .to_string(),
        )
        .expect("status fixture");
        let parsed = read_controller_agent_status(&status, &task).expect("bound status");
        assert_eq!(parsed.sessions_started, 3);
        fs::write(
            &status,
            serde_json::json!({
                "agent_id":run,
                "name":agent,
                "owner":principal.to_string(),
                "domain":"scientific",
                "state_profile":"scientific-worker",
                "state_run_id":run,
                "active_sandbox":"sandbox-foreign",
                "sessions_started":3,
                "sessions_completed":3,
                "lifecycle":"active"
            })
            .to_string(),
        )
        .expect("invalid status fixture");
        assert!(read_controller_agent_status(&status, &task).is_err());
    }

    #[test]
    fn workspace_snapshot_is_bounded_and_hashed() {
        let root = tempdir().expect("artifact root");
        let source = tempdir().expect("workspace");
        fs::write(source.path().join("result.csv"), b"x,y\n1,2\n").expect("fixture");
        let destination = root
            .path()
            .join("tasks/00000000-0000-0000-0000-000000000000/workspace");
        let artifacts = snapshot_workspace(
            root.path(),
            source.path(),
            &destination,
            10,
            1024,
            &CancellationToken::new(),
        )
        .expect("snapshot");
        assert_eq!(artifacts.len(), 1);
        assert_eq!(artifacts[0].sha256.len(), 64);
        assert_eq!(
            media_type_for(&destination.join("result.csv")),
            "text/csv; charset=utf-8"
        );
    }

    #[test]
    fn exported_custody_tree_stays_inside_artifact_root() {
        let root = tempdir().expect("artifact root");
        let export = root.path().join("tasks/task-id/attempts/1-1/provenance");
        fs::create_dir_all(&export).expect("export root");
        fs::write(export.join("events.jsonl"), b"{}\n").expect("event fixture");
        let candidates = collect_artifact_tree(
            root.path(),
            &export,
            "researcher",
            "evidence",
            10,
            1024,
            &CancellationToken::new(),
        )
        .expect("collect export");
        assert_eq!(candidates.len(), 1);
        assert_eq!(candidates[0].role, "evidence");
    }

    #[test]
    fn workspace_snapshot_honors_cancellation_before_copy() {
        let root = tempdir().expect("artifact root");
        let source = tempdir().expect("workspace");
        fs::write(source.path().join("result.bin"), [7_u8; 128]).expect("fixture");
        let destination = root.path().join("tasks/task-id/attempts/1-1/workspace");
        let cancellation = CancellationToken::new();
        cancellation.cancel();

        let error = snapshot_workspace(
            root.path(),
            source.path(),
            &destination,
            10,
            1024,
            &cancellation,
        )
        .expect_err("cancelled custody must stop");

        assert!(error.to_string().contains("cancelled"));
        assert!(!destination.join("result.bin").exists());
    }

    #[test]
    fn workspace_snapshot_removes_an_over_limit_partial() {
        let root = tempdir().expect("artifact root");
        let source = tempdir().expect("workspace");
        fs::write(source.path().join("result.bin"), [3_u8; 128]).expect("fixture");
        let destination = root.path().join("tasks/task-id/attempts/1-1/workspace");

        let error = snapshot_workspace(
            root.path(),
            source.path(),
            &destination,
            10,
            64,
            &CancellationToken::new(),
        )
        .expect_err("oversized custody must stop");

        assert!(error.to_string().contains("limits"));
        assert!(!destination.join("result.bin").exists());
    }
}

#[cfg(all(test, unix))]
mod direct_dispatch_tests {
    use super::*;
    use crate::broker::dispatch::admission_record_name;
    use serde_json::json;
    use std::{
        fs,
        os::unix::fs::PermissionsExt,
        path::{Path, PathBuf},
    };
    use tempfile::TempDir;
    use uuid::Uuid;

    const ADMISSION: &str = include_str!("../../tests/fixtures/admission/staged_admission.json");

    fn task() -> ClaimedTask {
        ClaimedTask {
            id: Uuid::nil(),
            principal_id: Uuid::nil(),
            group_key: "research".into(),
            profile_key: "reference".into(),
            objective: "synthetic objective".into(),
            logical_agent: "reference".into(),
            lease_generation: 7,
            attempt: 2,
            max_task_attempts: 3,
            max_provider_attempts: 1,
            harness: String::new(),
            thinking: None,
            route: String::new(),
            controller_profile: String::new(),
            gateway_profile: String::new(),
            image_ref: String::new(),
            cpu_limit: String::new(),
            memory_limit: String::new(),
            state_profile: String::new(),
            role: "research".into(),
            max_run_seconds: 300,
        }
    }

    fn mode(path: &Path, bits: u32) {
        fs::set_permissions(path, fs::Permissions::from_mode(bits)).unwrap();
    }

    fn write_private(path: &Path, contents: impl AsRef<[u8]>) {
        fs::write(path, contents).unwrap();
        mode(path, 0o600);
    }

    fn direct_catalog(temp: &TempDir, include_admission: bool) -> DirectDispatchCatalog {
        let root = temp.path().canonicalize().unwrap();
        let admissions = root.join("admissions");
        let catalogs = root.join("catalogs");
        fs::create_dir(&admissions).unwrap();
        fs::create_dir(&catalogs).unwrap();
        mode(&admissions, 0o700);
        mode(&catalogs, 0o700);
        let task = task();
        if include_admission {
            write_private(
                &admissions.join(admission_record_name(&task).unwrap()),
                ADMISSION,
            );
        }
        let catalog = json!({
            "schemaVersion":"sandbox.enrichedlab.org/direct-dispatch/v1",
            "kubernetes":{
                "origin":"https://kubernetes.example.invalid",
                "caPath":"/var/run/secrets/kubernetes.io/serviceaccount/ca.crt",
                "tokenPath":"/var/run/secrets/kubernetes.io/serviceaccount/token"
            },
            "admission":{"root":admissions,"writerUid":unsafe { libc::geteuid() }},
            "bindings":[{
                "principalId":Uuid::nil(),
                "principalHash":"2f4b66b9706c1ef57d67e2cf9ad98127",
                "groupKey":"research",
                "profileKey":"reference",
                "profileId":"example-kata",
                "labRef":"lab://example/research",
                "namespace":"sandbox-example-001",
                "runtimeClassName":"kata-clh"
            }]
        });
        let path = catalogs.join("direct_dispatch.json");
        write_private(&path, serde_json::to_vec(&catalog).unwrap());
        DirectDispatchCatalog::load(&path, unsafe { libc::geteuid() }).unwrap()
    }

    fn config(root: &Path) -> Arc<WorkerConfig> {
        Arc::new(WorkerConfig {
            database_dsn: "host=127.0.0.1 port=1 dbname=broker sslmode=disable".into(),
            tenant: "broker".into(),
            artifact_root: root.join("artifacts"),
            controller_root: root.join("controller"),
            controller_state: root.join("controller-state"),
            controller_config: root.join("controller-config"),
            controller_bin: PathBuf::from("synthetic-controller"),
            openshell_bin: PathBuf::from("openshell"),
            docker_bin: PathBuf::from("docker"),
            worker_id: "worker-test".into(),
            direct_dispatch: None,
            query_connection_dir: None,
            render_url: None,
            dataset_fetcher_config: None,
            lab_objects_config: None,
            max_parallel: 1,
            lease_seconds: 60,
            poll_interval: Duration::from_millis(750),
            database_pool: 1,
            max_workspace_files: 20_000,
            max_workspace_bytes: 1024,
            live_output: false,
        })
    }

    fn synthetic_store() -> Store {
        Store::connect(
            "host=127.0.0.1 port=1 dbname=broker sslmode=disable",
            "broker".into(),
            1,
        )
        .unwrap()
    }

    fn has_code(error: &anyhow::Error, code: &str) -> bool {
        error.chain().any(|cause| cause.to_string() == code)
    }

    #[tokio::test]
    async fn matched_direct_dispatch_failures_precede_legacy_worker_side_effects() {
        for (include_admission, code) in [
            (false, "direct_dispatch_admission_invalid"),
            (true, "direct_dispatch_terminal_export_missing"),
        ] {
            let temp = TempDir::new().unwrap();
            let task = task();
            let artifact_root = temp.path().canonicalize().unwrap().join("artifacts");
            let legacy_root = artifact_root.join("tasks").join(task.id.to_string());
            let error = execute_task(
                synthetic_store(),
                config(temp.path()),
                Some(Arc::new(direct_catalog(&temp, include_admission))),
                task,
                CancellationToken::new(),
            )
            .await
            .expect_err("matched direct dispatch must fail closed");
            assert!(has_code(&error, code), "expected {code}: {error:#}");
            assert!(
                !legacy_root.exists(),
                "direct rejection must precede legacy artifact/objective/controller work"
            );
        }
    }
}
