//! Collecting a worker's outputs and indexing its artifacts and references.
use super::*;

pub fn collect(
    config: &RuntimeConfig,
    study_name: &str,
    worker_name: &str,
) -> Result<ResearchSeal> {
    let mut study = load(config, study_name)?;
    let worker = study
        .workers
        .get(worker_name)
        .context("unknown research worker")?
        .clone();
    if worker.state == ResearchWorkerState::Running {
        bail!("stop or finish the research worker before collection");
    }
    if worker.state == ResearchWorkerState::Prepared {
        bail!("launch or explicitly stop the research worker before collection");
    }
    if worker.state == ResearchWorkerState::Deleted {
        bail!("deleted research worker has no live collection path");
    }
    if worker.state == ResearchWorkerState::Sealed {
        verify_worker_seal(&study, &worker)?;
        if let Some(run_id) = &worker.state_run_id {
            agent_state::seal_run(
                config,
                run_id,
                worker_name,
                "research output seal already verified",
                "host-controller",
            )?;
        }
        return read_json(&seal_path(&study, worker_name));
    }
    verify_worker_materials(&study, &worker)?;
    stage_controller_logs(&study, &worker)?;
    if let Some(run_id) = &worker.state_run_id {
        agent_state::reconcile_outbox(config, run_id, worker_name, &worker.workspace)?;
    }
    let methods = worker.output.join("methods.md");
    require_nonempty_file(&methods, "methods.md")?;
    let result_path = worker.output.join("results/result.json");
    let result: ResearchResult = read_json(&result_path)?;
    validate_result(&result, worker_name, &worker.output)?;
    validate_method_input_manifest(&result, &study.input_manifest_sha256)?;
    if let Some(run_id) = &worker.state_run_id {
        record_research_artifacts(config, &worker, run_id)?;
    }
    let (output_sha256, output_files, output_bytes) = hash_path_with_limits(
        &worker.output,
        study.max_output_files,
        study.max_output_bytes,
    )?;
    let result_sha256 = hash_file(&result_path)?;
    let seal = ResearchSeal {
        schema_version: 2,
        study: study_name.into(),
        worker: worker_name.into(),
        sealed_at: now()?,
        question_sha256: study.question_sha256.clone(),
        protocol_sha256: study.protocol_sha256.clone(),
        input_manifest_sha256: study.input_manifest_sha256.clone(),
        perspective_sha256: worker.perspective_sha256.clone(),
        assignment_sha256: worker.assignment_sha256.clone(),
        route: worker.route.clone(),
        harness: worker.harness,
        route_context_limit: worker.route_context_limit,
        route_output_limit: worker.route_output_limit,
        route_pool_cap: worker.route_pool_cap,
        thinking: worker.thinking,
        tool_digests: worker
            .tools
            .iter()
            .map(|tool| (tool.name.clone(), tool.sha256.clone()))
            .collect(),
        result_sha256: result_sha256.clone(),
        output_sha256: output_sha256.clone(),
        output_files,
        output_bytes,
        scientific_validation: false,
    };
    write_json(&seal_path(&study, worker_name), &seal)?;
    let current = study.workers.get_mut(worker_name).expect("worker exists");
    current.state = ResearchWorkerState::Sealed;
    current.sealed_at = Some(seal.sealed_at.clone());
    current.result_sha256 = Some(result_sha256);
    current.output_sha256 = Some(output_sha256);
    save(config, &study)?;
    if let Some(run_id) = &worker.state_run_id {
        agent_state::seal_run(
            config,
            run_id,
            worker_name,
            "research output sealed",
            "host-controller",
        )?;
    }
    audit::append(
        config,
        "host-controller",
        "research.worker.seal",
        &format!("research/{study_name}/worker/{worker_name}"),
        "passed",
        json!({
            "output_sha256": seal.output_sha256,
            "result_sha256": seal.result_sha256,
            "output_files": seal.output_files,
            "output_bytes": seal.output_bytes,
            "scientific_validation": false
        }),
    )?;
    Ok(seal)
}

pub(super) fn record_research_artifacts(
    config: &RuntimeConfig,
    worker: &ResearchWorker,
    run_id: &str,
) -> Result<()> {
    let mut files = WalkDir::new(&worker.output)
        .follow_links(false)
        .into_iter()
        .collect::<std::result::Result<Vec<_>, _>>()?;
    files.sort_by(|left, right| left.path().cmp(right.path()));

    let mut references = Vec::new();
    for entry in files {
        let metadata = entry.metadata()?;
        if metadata.file_type().is_symlink() || !metadata.is_file() {
            continue;
        }
        let relative = entry
            .path()
            .strip_prefix(&worker.output)
            .context("research artifact escaped the worker output root")?;
        let label = relative
            .to_str()
            .context("research artifact path is not valid UTF-8")?
            .replace('\\', "/");
        let (collection_key, role, sensitivity) = classify_research_artifact(relative);
        let artifact = agent_state::add_artifact(
            config,
            run_id,
            &worker.name,
            entry.path(),
            research_media_type(relative),
            role,
            &label,
            sensitivity,
            "host-controller",
        )?;
        references.push((
            collection_key.to_string(),
            label,
            artifact.sha256,
            role.to_string(),
        ));
    }

    index_scientific_references(config, worker, run_id, &references)?;

    Ok(())
}

pub(super) fn index_scientific_references(
    config: &RuntimeConfig,
    worker: &ResearchWorker,
    run_id: &str,
    references: &[(String, String, String, String)],
) -> Result<()> {
    const MAX_PROMPT_REFERENCES: usize = 10_000;
    let total = references.len();
    for chunk in references
        .iter()
        .take(MAX_PROMPT_REFERENCES)
        .collect::<Vec<_>>()
        .chunks(2_000)
    {
        let set: BTreeMap<String, Value> = chunk
            .iter()
            .map(|(collection_key, label, sha256, role)| {
                let label_digest = hash_bytes(label.as_bytes());
                let key = format!("{}-{}", &sha256[..16], &label_digest[..8]);
                (
                    format!("/{collection_key}/{key}"),
                    json!({
                        "sha256": sha256,
                        "role": role,
                        "label": label
                    }),
                )
            })
            .collect();
        let rationale = agent_state::DecisionRationale {
            summary: format!(
                "Host recorded {} immutable research output references.",
                chunk.len()
            ),
            alternatives: Vec::new(),
            evidence: chunk
                .iter()
                .take(64)
                .map(|(_, _, sha256, _)| sha256.clone())
                .collect(),
            confidence: Some(1.0),
        };
        for attempt in 0..8 {
            let collection =
                agent_state::load_collection(config, run_id, &worker.name, "scientific")?;
            let result = agent_state::apply_mutation(
                config,
                &agent_state::StateMutation {
                    schema_version: 1,
                    run_id: run_id.into(),
                    subject: worker.name.clone(),
                    collection: "scientific".into(),
                    base_version: collection.version,
                    set: set.clone(),
                    remove: Vec::new(),
                    rationale: Some(rationale.clone()),
                },
                "host-controller",
                agent_state::StateAuthority::Machine,
            );
            match result {
                Ok(_) => break,
                Err(error)
                    if attempt < 7 && error.to_string().contains("state version conflict") =>
                {
                    continue;
                }
                Err(error) => return Err(error),
            }
        }
    }
    if total > MAX_PROMPT_REFERENCES {
        agent_state::append_event(
            config,
            run_id,
            &worker.name,
            "artifact.prompt-index.truncated",
            "host-controller",
            agent_state::StateAuthority::Machine,
            json!({
                "recorded_artifacts": total,
                "prompt_reference_limit": MAX_PROMPT_REFERENCES,
                "all_artifacts_remain_available_in_the_provenance_ledger": true
            }),
        )?;
    }
    Ok(())
}

pub(super) fn classify_research_artifact(
    relative: &Path,
) -> (&'static str, &'static str, agent_state::ArtifactSensitivity) {
    let first = relative
        .components()
        .next()
        .and_then(|component| match component {
            std::path::Component::Normal(value) => value.to_str(),
            _ => None,
        });
    let lower = relative.to_string_lossy().to_ascii_lowercase();
    match first {
        Some("scripts") => (
            "method_refs",
            "method",
            agent_state::ArtifactSensitivity::Standard,
        ),
        Some("intermediate") => (
            "intermediate_refs",
            "intermediate",
            agent_state::ArtifactSensitivity::Standard,
        ),
        Some("discarded") => (
            "intermediate_refs",
            "failed-attempt",
            agent_state::ArtifactSensitivity::Standard,
        ),
        Some("results") => (
            "claim_refs",
            "result",
            agent_state::ArtifactSensitivity::Standard,
        ),
        Some("figures") => (
            "claim_refs",
            "figure",
            agent_state::ArtifactSensitivity::Standard,
        ),
        Some("logs") => (
            "validation_refs",
            "execution-log",
            agent_state::ArtifactSensitivity::Restricted,
        ),
        _ if lower.contains("reasoning") || lower.contains("trace") => (
            "validation_refs",
            "returned-reasoning",
            agent_state::ArtifactSensitivity::Restricted,
        ),
        _ => (
            "method_refs",
            "method",
            agent_state::ArtifactSensitivity::Standard,
        ),
    }
}

pub(super) fn research_media_type(path: &Path) -> &'static str {
    match path
        .extension()
        .and_then(|value| value.to_str())
        .map(str::to_ascii_lowercase)
        .as_deref()
    {
        Some("json") | Some("jsonl") => "application/json",
        Some("csv") => "text/csv",
        Some("tsv") => "text/tab-separated-values",
        Some("md") => "text/markdown",
        Some("txt") | Some("log") | Some("r") | Some("py") | Some("sh") => "text/plain",
        Some("png") => "image/png",
        Some("jpg") | Some("jpeg") => "image/jpeg",
        Some("svg") => "image/svg+xml",
        Some("pdf") => "application/pdf",
        Some("parquet") => "application/vnd.apache.parquet",
        Some("arrow") | Some("feather") => "application/vnd.apache.arrow.file",
        _ => "application/octet-stream",
    }
}
