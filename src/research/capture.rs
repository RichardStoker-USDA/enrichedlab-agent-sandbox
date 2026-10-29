//! Run captures, segment checkpoints and the result contract.
use super::*;

pub fn event_stream_complete(
    observations: &ResearchObservationSummary,
    stdout_truncated: bool,
    stderr_truncated: bool,
    observer_failed: bool,
    postprocess_failed: bool,
) -> bool {
    !stdout_truncated
        && !stderr_truncated
        && !observer_failed
        && !postprocess_failed
        && observations.json_lines > 0
        && observations.malformed_lines == 0
        && observations.step_finishes > 0
}

pub(super) fn result_contract(worker: &str) -> serde_json::Value {
    json!({
        "path": "/sandbox/work/output/results/result.json",
        "additional_properties": false,
        "instructions": [
            "Use every key in template exactly once and do not add keys.",
            "answer must be one bounded JSON string, not an object or array.",
            "claim kind must be deterministic or interpretive.",
            "claim evidence paths must be relative to the output root and name existing regular files.",
            "confidence must be a finite number from 0 through 1.",
            "limitations must be an array of strings.",
            "rerun.environment and every method, quality-control, and LLM-disclosure reference must be relative to the output root and name an existing regular file.",
            "The LLM may inspect bounded samples for schema discovery and quality control, but deterministic or statistical claims must come from the declared tool or model outputs.",
            "Set llm_is_scientific_instrument to true only when class is llm-method and the full LLM disclosure is present."
        ],
        "template": {
            "schema_version": 2,
            "worker": worker,
            "status": "complete",
            "answer": "replace with one bounded answer string",
            "claims": [{
                "statement": "replace with one supported claim",
                "kind": "deterministic",
                "evidence": ["scripts/replace-with-existing-file"],
                "confidence": 0.0
            }],
            "limitations": ["replace with one measured or known limit"],
            "rerun": {
                "command": "replace with a reviewed rerun command",
                "environment": "environment.txt"
            },
            "method": {
                "class": "deterministic-tool",
                "tool_or_model": "replace with the scientific tool or model name",
                "version": "replace with the pinned version or digest",
                "input_manifest_sha256": "replace with research-assignment input_manifest_sha256",
                "code_refs": ["scripts/replace-with-existing-file"],
                "parameter_refs": ["intermediate/replace-with-existing-parameters-file"],
                "output_refs": ["intermediate/replace-with-existing-tool-output"],
                "validation_refs": ["intermediate/replace-with-existing-validation-record"],
                "seeds": [],
                "llm_disclosure": null
            },
            "quality_control": {
                "purpose": "schema-and-quality-control",
                "schema_refs": ["intermediate/replace-with-existing-schema-record"],
                "sample_refs": ["intermediate/replace-with-existing-bounded-sample-record"],
                "check_refs": ["intermediate/replace-with-existing-quality-check-record"],
                "llm_is_scientific_instrument": false
            }
        }
    })
}

#[allow(clippy::too_many_arguments)]
pub fn store_run_capture(
    config: &RuntimeConfig,
    study_name: &str,
    worker_name: &str,
    segment: u32,
    passed: bool,
    result_class: &str,
    result_complete: bool,
    exit_code: Option<i32>,
    stdout: &[u8],
    stderr: &[u8],
    stdout_truncated: bool,
    stderr_truncated: bool,
    observations: ResearchObservationSummary,
    context_pressure_ratio: f64,
    context_action: &str,
) -> Result<ResearchRunCapture> {
    validate_result_class(result_class)?;
    if segment == 0
        || !context_pressure_ratio.is_finite()
        || !(0.0..=1.0).contains(&context_pressure_ratio)
    {
        bail!("research segment capture metadata is invalid");
    }
    validate_result_class(context_action)?;
    let study = load(config, study_name)?;
    let worker = study
        .workers
        .get(worker_name)
        .context("unknown research worker")?;
    let directory = study.root.join("controller-logs").join(worker_name);
    create_private_dir(&directory)?;
    let segment_directory = segment_log_dir(&study, worker_name, segment);
    create_private_dir(&segment_directory)?;
    write_private(&segment_directory.join("stdout.jsonl"), stdout)?;
    write_private(&segment_directory.join("stderr.log"), stderr)?;
    let capture = ResearchRunCapture {
        schema_version: 1,
        study: study_name.into(),
        worker: worker_name.into(),
        segment,
        captured_at: now()?,
        passed,
        result_class: result_class.into(),
        result_complete,
        fresh_provider_session: true,
        exit_code,
        stdout_bytes: stdout.len(),
        stderr_bytes: stderr.len(),
        stdout_truncated,
        stderr_truncated,
        observations,
        context_pressure_ratio,
        context_action: context_action.into(),
        checkpoint_epoch: None,
    };
    write_json(&segment_directory.join("run.json"), &capture)?;
    write_json(&directory.join("latest-run.json"), &capture)?;
    if let Some(run_id) = &worker.state_run_id {
        agent_state::append_event(
            config,
            run_id,
            worker_name,
            "agent.segment.finished",
            "host-controller",
            agent_state::StateAuthority::Machine,
            json!({
                "segment": segment,
                "passed": passed,
                "result_class": result_class,
                "result_complete": result_complete,
                "fresh_provider_session": true,
                "exit_code": exit_code,
                "stdout_bytes": stdout.len(),
                "stderr_bytes": stderr.len(),
                "stdout_sha256": hash_bytes(stdout),
                "stderr_sha256": hash_bytes(stderr),
                "stdout_truncated": stdout_truncated,
                "stderr_truncated": stderr_truncated,
                "context_pressure_ratio": context_pressure_ratio,
                "context_action": context_action,
                "tool_calls": capture.observations.tool_calls,
                "error_events": capture.observations.error_events,
                "reasoning_tokens": capture.observations.reasoning_tokens,
                "peak_context_tokens": capture.observations.peak_context_tokens,
                "workspace_snapshots": capture.observations.workspace_snapshots,
                "artifact_versions": capture.observations.artifact_versions
            }),
        )?;
    }
    Ok(capture)
}

pub fn bind_capture_checkpoint(
    config: &RuntimeConfig,
    study_name: &str,
    worker_name: &str,
    capture: &mut ResearchRunCapture,
    epoch: u64,
) -> Result<()> {
    if capture.study != study_name
        || capture.worker != worker_name
        || capture.checkpoint_epoch.is_some()
    {
        bail!("research capture cannot be rebound to a checkpoint");
    }
    let study = load(config, study_name)?;
    capture.checkpoint_epoch = Some(epoch);
    let segment_directory = segment_log_dir(&study, worker_name, capture.segment);
    write_json(&segment_directory.join("run.json"), capture)?;
    write_json(
        &study
            .root
            .join("controller-logs")
            .join(worker_name)
            .join("latest-run.json"),
        capture,
    )?;
    Ok(())
}

pub fn mark_capture_checkpoint_failed(
    config: &RuntimeConfig,
    study_name: &str,
    worker_name: &str,
    capture: &mut ResearchRunCapture,
    detail: &str,
) -> Result<()> {
    const MAX_DETAIL_BYTES: usize = 64 * 1024;
    if capture.study != study_name || capture.worker != worker_name {
        bail!("research checkpoint failure capture identity changed");
    }
    let study = load(config, study_name)?;
    let directory = segment_log_dir(&study, worker_name, capture.segment);
    let detail_bytes = detail.as_bytes();
    let stored_detail = if detail_bytes.len() <= MAX_DETAIL_BYTES {
        detail_bytes
    } else {
        &detail_bytes[..MAX_DETAIL_BYTES]
    };
    write_private(&directory.join("checkpoint-error.log"), stored_detail)?;
    capture.passed = false;
    capture.result_class = "controller-checkpoint".into();
    write_json(&directory.join("run.json"), capture)?;
    write_json(
        &study
            .root
            .join("controller-logs")
            .join(worker_name)
            .join("latest-run.json"),
        capture,
    )?;
    if let Some(run_id) = study
        .workers
        .get(worker_name)
        .and_then(|worker| worker.state_run_id.as_deref())
    {
        agent_state::append_event(
            config,
            run_id,
            worker_name,
            "agent.segment.checkpoint-failed",
            "host-controller",
            agent_state::StateAuthority::Machine,
            json!({
                "segment": capture.segment,
                "detail_bytes": detail_bytes.len(),
                "stored_detail_bytes": stored_detail.len(),
                "detail_sha256": hash_bytes(detail_bytes),
                "raw_content_custody": "restricted-controller-log"
            }),
        )?;
    }
    Ok(())
}

pub fn result_is_complete(
    config: &RuntimeConfig,
    study_name: &str,
    worker_name: &str,
) -> Result<bool> {
    let study = load(config, study_name)?;
    let worker = study
        .workers
        .get(worker_name)
        .context("unknown research worker")?;
    let result_path = worker.output.join("results/result.json");
    if !result_path.exists() {
        return Ok(false);
    }
    let result: ResearchResult = read_json(&result_path)?;
    validate_result(&result, worker_name, &worker.output)?;
    validate_method_input_manifest(&result, &study.input_manifest_sha256)?;
    require_nonempty_file(&worker.output.join("methods.md"), "methods.md")?;
    require_nonempty_file(&worker.output.join("environment.txt"), "environment.txt")?;
    Ok(true)
}

pub fn checkpoint_segment(
    config: &RuntimeConfig,
    study_name: &str,
    worker_name: &str,
    segment: u32,
    result_complete: bool,
    observations: &ResearchObservationSummary,
    context_action: &str,
) -> Result<agent_state::StateCheckpoint> {
    let study = load(config, study_name)?;
    let worker = study
        .workers
        .get(worker_name)
        .context("unknown research worker")?
        .clone();
    if worker.state != ResearchWorkerState::Running || segment == 0 {
        bail!("only a running research worker can checkpoint a positive segment");
    }
    let run_id = worker
        .state_run_id
        .as_deref()
        .context("research worker has no state run")?;
    agent_state::reconcile_outbox(config, run_id, worker_name, &worker.workspace)?;
    let pending_outbox = fs::read_dir(worker.workspace.join(".agent-state/outbox"))?
        .filter_map(|entry| entry.ok())
        .filter(|entry| entry.path().extension().and_then(|value| value.to_str()) == Some("json"))
        .count();
    if pending_outbox != 0 {
        bail!("research state outbox is not empty at the checkpoint boundary");
    }
    let head: WorkspaceHead = read_json(&workspace_head_path(&study, worker_name))?;
    if head.segment != segment {
        bail!("research segment has no final workspace snapshot");
    }
    let current_manifest = inventory_tree(&worker.output)?;
    let current_sha256 = hash_bytes(&serde_json::to_vec(&current_manifest)?);
    if head.manifest != current_manifest || head.manifest_sha256 != current_sha256 {
        bail!("research output changed after the final machine snapshot");
    }
    let segment_directory = segment_log_dir(&study, worker_name, segment);
    for name in ["stdout.jsonl", "stderr.log", "run.json"] {
        let path = segment_directory.join(name);
        let metadata = fs::symlink_metadata(&path)
            .with_context(|| format!("missing segment custody file {name}"))?;
        if metadata.file_type().is_symlink() || !metadata.is_file() {
            bail!("segment custody files must be regular files");
        }
    }
    let readiness = agent_state::append_event(
        config,
        run_id,
        worker_name,
        "agent.segment.checkpoint-ready",
        "host-controller",
        agent_state::StateAuthority::Machine,
        json!({
            "segment": segment,
            "result_complete": result_complete,
            "outbox_pending": pending_outbox,
            "workspace_manifest_sha256": current_sha256,
            "workspace_snapshot_sequence": head.sequence,
            "tool_calls": observations.tool_calls,
            "json_lines": observations.json_lines,
            "malformed_lines": observations.malformed_lines,
            "total_tokens": observations.total_tokens,
            "peak_context_tokens": observations.peak_context_tokens,
            "reasoning_tokens": observations.reasoning_tokens,
            "error_events": observations.error_events,
            "context_action": context_action,
            "fresh_session_next": !result_complete
        }),
    )?;
    let execution = agent_state::load_collection(config, run_id, worker_name, "execution")?;
    let phase = if result_complete {
        "result-ready"
    } else {
        "checkpointed"
    };
    let next_action = if result_complete {
        Value::Null
    } else {
        json!("continue from the bounded state in a fresh provider session")
    };
    agent_state::apply_mutation(
        config,
        &agent_state::StateMutation {
            schema_version: 1,
            run_id: run_id.into(),
            subject: worker_name.into(),
            collection: "execution".into(),
            base_version: execution.version,
            set: BTreeMap::from([
                ("/phase".into(), json!(phase)),
                ("/next_action".into(), next_action),
                (
                    "/last_observation_ref".into(),
                    json!(readiness.event_id),
                ),
                (
                    format!("/completed/segment-{segment}"),
                    json!({
                        "summary": "The provider session ended and controller evidence passed the checkpoint boundary.",
                        "evidence": [readiness.event_id]
                    }),
                ),
            ]),
            remove: Vec::new(),
            rationale: Some(agent_state::DecisionRationale {
                summary: "The controller verified the state and evidence boundary before discarding session context.".into(),
                alternatives: Vec::new(),
                evidence: vec![readiness.event_id.clone(), current_sha256],
                confidence: Some(1.0),
            }),
        },
        "host-controller",
        agent_state::StateAuthority::Machine,
    )?;
    agent_state::materialize_view(config, run_id, worker_name, &worker.workspace)?;
    let checkpoint = agent_state::checkpoint(
        config,
        run_id,
        worker_name,
        &format!("segment {segment} flushed; old provider session and KV state must not be reused"),
        "host-controller",
    )?;
    agent_state::materialize_view(config, run_id, worker_name, &worker.workspace)?;
    update_worker(config, study_name, worker_name, |current| {
        if current.state != ResearchWorkerState::Running {
            bail!("research worker stopped during segment checkpoint");
        }
        current.segments_completed = current.segments_completed.max(segment);
        current.last_segment_at = Some(checkpoint.created_at.clone());
        Ok(())
    })?;
    Ok(checkpoint)
}
