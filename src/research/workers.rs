//! Creating a study, preparing and launching its workers, and their prompts.
use super::*;

pub fn create(config: &RuntimeConfig, spec: ResearchStudySpec<'_>) -> Result<ResearchStudy> {
    let ResearchStudySpec {
        name,
        mode,
        question,
        question_file,
        protocol,
        inputs,
        max_output_files,
        max_output_bytes,
    } = spec;
    validate_name(name, "study")?;
    let question = read_required_text(question, question_file, "question")?;
    let protocol_body = match protocol {
        Some(path) => read_absolute_text(path, "protocol")?,
        None => default_protocol(mode),
    };
    let studies_root = config.root.join("studies");
    create_private_dir(&studies_root)?;
    let root = studies_root.join(name);
    if root.exists() || record_path(config, name).exists() {
        bail!("research study already exists: {name}");
    }
    validate_input_sources(inputs)?;
    if max_output_files == 0
        || max_output_files > MAX_SNAPSHOT_FILES
        || max_output_bytes == 0
        || max_output_bytes > MAX_SNAPSHOT_BYTES
    {
        bail!("research output limits must be nonzero and within controller safety caps");
    }
    let pending = studies_root.join(format!(".{name}.{}.pending", Uuid::new_v4()));
    create_private_dir(&pending)?;
    let control = pending.join("control");
    let input_root = control.join("inputs");
    create_private_dir(&input_root)?;
    let question_pending = control.join("question.txt");
    let protocol_pending = control.join("protocol.md");
    write_private(&question_pending, question.as_bytes())?;
    write_private(&protocol_pending, protocol_body.as_bytes())?;
    for source in inputs {
        copy_named_source(source, &input_root)?;
    }
    let input_manifest = inventory_tree(&input_root)?;
    let manifest_pending = control.join("input-manifest.json");
    write_json(&manifest_pending, &input_manifest)?;
    create_private_dir(&pending.join("workers"))?;
    create_private_dir(&pending.join("seals"))?;
    create_private_dir(&pending.join("recovery"))?;
    create_private_dir(&pending.join("controller-logs"))?;
    fs::rename(&pending, &root).with_context(|| {
        format!(
            "failed to activate study directory; recovery bytes remain at {}",
            pending.display()
        )
    })?;
    let question_path = root.join("control/question.txt");
    let protocol_path = root.join("control/protocol.md");
    let input_manifest_path = root.join("control/input-manifest.json");
    let study = ResearchStudy {
        schema_version: 1,
        name: name.into(),
        mode,
        root,
        question_sha256: hash_file(&question_path)?,
        question_path,
        protocol_sha256: hash_file(&protocol_path)?,
        protocol_path,
        input_manifest_sha256: hash_file(&input_manifest_path)?,
        input_manifest_path,
        input_file_count: input_manifest.files.len() as u64,
        input_bytes: input_manifest.total_bytes,
        max_output_files,
        max_output_bytes,
        created_at: now()?,
        workers: BTreeMap::new(),
        comparison: None,
    };
    save(config, &study)?;
    audit::append(
        config,
        "host-controller",
        "research.create",
        &format!("research/{name}"),
        "passed",
        json!({
            "mode": mode,
            "question_sha256": study.question_sha256,
            "protocol_sha256": study.protocol_sha256,
            "input_manifest_sha256": study.input_manifest_sha256,
            "input_files": study.input_file_count,
            "input_bytes": study.input_bytes,
            "max_output_files": study.max_output_files,
            "max_output_bytes": study.max_output_bytes
        }),
    )?;
    Ok(study)
}

pub fn prepare_worker(
    config: &RuntimeConfig,
    spec: ResearchWorkerSpec<'_>,
) -> Result<ResearchWorker> {
    let ResearchWorkerSpec {
        study_name,
        worker_name,
        perspective,
        perspective_file,
        tool_names,
        thinking,
        harness,
        runtime,
    } = spec;
    validate_name(worker_name, "worker")?;
    if !matches!(harness, HarnessKind::OpenCode | HarnessKind::Pi) {
        bail!(
            "research workers support the open-code and pi sandbox runners; use provenance ingest for a custom harness"
        );
    }
    let perspective = read_required_text(perspective, perspective_file, "perspective")?;
    let mut study = load(config, study_name)?;
    if study.comparison.is_some() {
        bail!("cannot add a worker after the study comparison is sealed");
    }
    if study.workers.contains_key(worker_name) {
        bail!("research worker already exists: {worker_name}");
    }
    let settings = config.load_settings()?;
    let routes = RouteCatalog::load(config)?;
    let route_name = runtime.route.as_deref().unwrap_or(&settings.default_route);
    let route = routes.get(route_name)?;
    if route.model.is_none() {
        bail!("research workers need an inference route");
    }
    route.resolve_variant(thinking)?;
    let route_workers = study
        .workers
        .values()
        .filter(|worker| worker.route == route_name && worker.state != ResearchWorkerState::Deleted)
        .count();
    if route_workers >= route.pool_cap {
        bail!(
            "route {route_name} is capped at {} research workers",
            route.pool_cap
        );
    }
    let gateway_profile = runtime
        .gateway_profile
        .as_deref()
        .unwrap_or(&settings.default_gateway_profile);
    settings.gateway_profile(gateway_profile)?;
    let tool_entries = verified_tools(config, tool_names)?;
    let workspace = study.root.join("workers").join(worker_name);
    let state_run_id = format!("{}-state", sandbox_name(study_name, worker_name));
    let pending = study
        .root
        .join("workers")
        .join(format!(".{worker_name}.{}.pending", Uuid::new_v4()));
    create_private_dir(&pending)?;
    let input = pending.join("input");
    copy_tree(&study.root.join("control/inputs"), &input)?;
    let copied_inputs = inventory_tree(&input)?;
    let canonical_inputs: SnapshotManifest = read_json(&study.input_manifest_path)?;
    if copied_inputs != canonical_inputs {
        bail!(
            "worker input copy does not match the study snapshot; recovery bytes remain at {}",
            pending.display()
        );
    }
    let output = pending.join("output");
    create_output_tree(&output)?;
    let tools_root = pending.join("tools");
    create_private_dir(&tools_root)?;
    let mut tools = Vec::new();
    for entry in tool_entries {
        let destination = tools_root.join(&entry.name);
        copy_tree(&entry.path, &destination)?;
        let (copied_digest, copied_files, _) = hash_path(&destination)?;
        if copied_digest != entry.sha256 || copied_files != entry.file_count {
            bail!(
                "tool {} changed while it was copied; recovery bytes remain at {}",
                entry.name,
                pending.display()
            );
        }
        tools.push(ResearchTool {
            name: entry.name.clone(),
            kind: entry.kind,
            sha256: entry.sha256,
            file_count: entry.file_count,
            sandbox_path: format!("/sandbox/work/tools/{}", entry.name),
            entrypoint: entry.entrypoint,
        });
    }
    let perspective_pending = pending.join("perspective.txt");
    write_private(&perspective_pending, perspective.as_bytes())?;
    fs::copy(&study.question_path, pending.join("question.txt"))?;
    fs::copy(&study.protocol_path, pending.join("protocol.md"))?;
    fs::copy(
        &study.input_manifest_path,
        pending.join("input-manifest.json"),
    )?;
    let assignment = json!({
        "schema_version": 1,
        "study": study.name,
        "mode": study.mode,
        "worker": worker_name,
        "route": route_name,
        "route_context_limit": route.context_limit,
        "route_output_limit": route.output_limit,
        "route_pool_cap": route.pool_cap,
        "question": fs::read_to_string(&study.question_path)?,
        "perspective": perspective,
        "protocol": "/sandbox/work/protocol.md",
        "input_manifest": "/sandbox/work/input-manifest.json",
        "input_root": "/sandbox/work/input",
        "output_root": "/sandbox/work/output",
        "state": {
            "run_id": state_run_id,
            "profile": "scientific-worker",
            "view": "/sandbox/work/.agent-state/view.json",
            "outbox": "/sandbox/work/.agent-state/outbox",
            "accepted": "/sandbox/work/.agent-state/accepted",
            "rejected": "/sandbox/work/.agent-state/rejected",
            "mutation_contract": {
                "schema_version": 1,
                "required": ["schema_version", "run_id", "subject", "collection", "base_version", "set", "remove", "rationale"],
                "rule": "write to a temporary file, then atomically rename it to a unique .json file in outbox"
            }
        },
        "result_contract": result_contract(worker_name),
        "tools": tools,
        "rules": [
            "Work independently. Do not seek or read another worker's result.",
            "Do not edit the input snapshot. Hash validation runs before sealing.",
            "Write methods, scripts, intermediate data, results, figures, logs, and rerun metadata in the declared output tree.",
            "The controller records tool metadata, context token counts, and immutable output versions independently of model-authored state.",
            "Keep semantic working state current with explicit outbox proposals. Omission never deletes state; use remove only when deletion is intended.",
            "Use unique intermediate and discarded paths. Record short decision rationale, alternatives, evidence, failed attempts, and lessons. Do not put private chain-of-thought in state.",
            "Request access through the controller. Do not bypass policy or place credentials in output.",
            "Agreement with another worker is not scientific validation."
        ]
    });
    let assignment_pending = pending.join("research-assignment.json");
    write_json(&assignment_pending, &assignment)?;
    let assignment_sha256 = hash_file(&assignment_pending)?;
    fs::rename(&pending, &workspace).with_context(|| {
        format!(
            "failed to activate worker workspace; recovery bytes remain at {}",
            pending.display()
        )
    })?;
    if let Err(error) = agent_state::seed_scientific_worker(
        config,
        &state_run_id,
        worker_name,
        &fs::read_to_string(&study.question_path)?,
        Some(study_name),
    )
    .and_then(|_| agent_state::materialize_view(config, &state_run_id, worker_name, &workspace))
    {
        let recovered = study.root.join("recovery").join(format!(
            "{}-{}-state-setup",
            worker_name,
            OffsetDateTime::now_utc().unix_timestamp()
        ));
        fs::rename(&workspace, &recovered)?;
        return Err(error).with_context(|| {
            format!(
                "research state setup failed; worker files were retained at {}",
                recovered.display()
            )
        });
    }
    let worker = ResearchWorker {
        name: worker_name.into(),
        sandbox: sandbox_name(study_name, worker_name),
        workspace: workspace.clone(),
        output: workspace.join("output"),
        perspective_path: workspace.join("perspective.txt"),
        perspective_sha256: hash_bytes(perspective.as_bytes()),
        assignment_sha256,
        route: route_name.into(),
        route_context_limit: route.context_limit,
        route_output_limit: route.output_limit,
        route_pool_cap: route.pool_cap,
        gateway_profile: gateway_profile.into(),
        thinking,
        harness,
        tools,
        state_run_id: Some(state_run_id),
        state_profile: Some("scientific-worker".into()),
        segments_completed: 0,
        last_segment_at: None,
        state: ResearchWorkerState::Prepared,
        created_at: now()?,
        started_at: None,
        finished_at: None,
        sealed_at: None,
        result_sha256: None,
        output_sha256: None,
        last_result_class: None,
        sandbox_deleted: false,
    };
    study.workers.insert(worker_name.into(), worker.clone());
    save(config, &study)?;
    audit::append(
        config,
        "host-controller",
        "research.worker.prepare",
        &format!("research/{study_name}/worker/{worker_name}"),
        "passed",
        json!({
            "sandbox": worker.sandbox,
            "route": worker.route,
            "gateway_profile": worker.gateway_profile,
            "tool_digests": worker.tools.iter().map(|tool| (&tool.name, &tool.sha256)).collect::<BTreeMap<_, _>>()
        }),
    )?;
    Ok(worker)
}

pub fn attach_sandbox(
    config: &RuntimeConfig,
    study_name: &str,
    worker_name: &str,
    sandbox: &str,
) -> Result<ResearchStudy> {
    let study = load(config, study_name)?;
    let worker = study
        .workers
        .get(worker_name)
        .context("unknown research worker")?;
    if worker.sandbox != sandbox {
        bail!("research worker sandbox identity mismatch");
    }
    Ok(study)
}

pub fn rollback_worker(
    config: &RuntimeConfig,
    study_name: &str,
    worker_name: &str,
    reason: &str,
) -> Result<PathBuf> {
    validate_text(reason, "reason")?;
    let mut study = load(config, study_name)?;
    let worker = study
        .workers
        .get(worker_name)
        .context("unknown research worker")?
        .clone();
    if worker.state == ResearchWorkerState::Sealed {
        bail!("refusing to roll back a sealed research worker");
    }
    let recovered = study.root.join("recovery").join(format!(
        "{}-{}",
        worker_name,
        OffsetDateTime::now_utc().unix_timestamp()
    ));
    fs::rename(&worker.workspace, &recovered)?;
    study.workers.remove(worker_name);
    save(config, &study)?;
    audit::append(
        config,
        "host-controller",
        "research.worker.rollback",
        &format!("research/{study_name}/worker/{worker_name}"),
        "passed",
        json!({"reason": reason, "recovery_path": recovered}),
    )?;
    Ok(recovered)
}

pub fn load(config: &RuntimeConfig, name: &str) -> Result<ResearchStudy> {
    validate_name(name, "study")?;
    let path = record_path(config, name);
    let study: ResearchStudy = read_json(&path).with_context(|| format!("unknown study {name}"))?;
    validate_study(config, &study)?;
    Ok(study)
}

pub fn list(config: &RuntimeConfig) -> Result<Vec<ResearchStudy>> {
    let mut studies = Vec::new();
    for entry in fs::read_dir(config.research_dir())? {
        let path = entry?.path();
        if path.extension().and_then(|value| value.to_str()) == Some("json") {
            let study: ResearchStudy = read_json(&path)?;
            validate_study(config, &study)?;
            if path != record_path(config, &study.name) {
                bail!("research record filename does not match its study identity");
            }
            studies.push(study);
        }
    }
    studies.sort_by(|left, right| left.name.cmp(&right.name));
    Ok(studies)
}

pub fn summaries(config: &RuntimeConfig) -> Result<Vec<ResearchStudySummary>> {
    Ok(list(config)?
        .into_iter()
        .map(|study| study.summary())
        .collect())
}

pub fn launch_candidates(config: &RuntimeConfig, study_name: &str) -> Result<Vec<ResearchWorker>> {
    let study = load(config, study_name)?;
    let workers = study
        .workers
        .values()
        .filter(|worker| {
            matches!(
                worker.state,
                ResearchWorkerState::Prepared | ResearchWorkerState::Stopped
            ) && !worker.sandbox_deleted
        })
        .cloned()
        .collect::<Vec<_>>();
    if workers.is_empty() {
        bail!("study has no prepared workers to launch");
    }
    Ok(workers)
}

pub fn mark_running(
    config: &RuntimeConfig,
    study_name: &str,
    worker_name: &str,
) -> Result<ResearchWorker> {
    update_worker(config, study_name, worker_name, |worker| {
        if !matches!(
            worker.state,
            ResearchWorkerState::Prepared | ResearchWorkerState::Stopped
        ) || worker.sandbox_deleted
        {
            bail!("research worker is not launchable");
        }
        worker.state = ResearchWorkerState::Running;
        worker.started_at = Some(now()?);
        worker.last_result_class = None;
        Ok(())
    })
}

pub fn mark_finished(
    config: &RuntimeConfig,
    study_name: &str,
    worker_name: &str,
    passed: bool,
    result_class: &str,
) -> Result<ResearchWorker> {
    validate_result_class(result_class)?;
    let worker = update_worker(config, study_name, worker_name, |worker| {
        if worker.state != ResearchWorkerState::Running {
            bail!("research worker is not running");
        }
        worker.state = if passed {
            ResearchWorkerState::Completed
        } else {
            ResearchWorkerState::Failed
        };
        worker.finished_at = Some(now()?);
        worker.last_result_class = Some(result_class.into());
        Ok(())
    })?;
    audit::append(
        config,
        "host-controller",
        "research.worker.run",
        &format!("research/{study_name}/worker/{worker_name}"),
        if passed { "passed" } else { "failed" },
        json!({"sandbox": worker.sandbox, "route": worker.route}),
    )?;
    Ok(worker)
}

pub fn worker_prompt(config: &RuntimeConfig, study: &str, worker: &str) -> Result<String> {
    let study = load(config, study)?;
    let worker = study
        .workers
        .get(worker)
        .context("unknown research worker")?;
    Ok(worker_segment_prompt(&study, worker, 1, 1))
}

pub fn worker_segment_prompt(
    study: &ResearchStudy,
    worker: &ResearchWorker,
    segment: u32,
    max_segments: u32,
) -> String {
    format!(
        "This is bounded segment {segment} of at most {max_segments}, in a fresh provider session with no prior transcript or KV state. Read /sandbox/work/research-assignment.json, /sandbox/work/protocol.md, /sandbox/work/input-manifest.json, and /sandbox/work/.agent-state/view.json. Treat the state view and immutable artifacts as the only carried working state. Complete one coherent unit of the assigned research work independently, then exit. Before each material plan or action, reread the state view. After a material observation, decision, failed attempt, completed step, or lesson, propose one explicit state mutation by writing a temporary file and atomically renaming it to a unique .json file under /sandbox/work/.agent-state/outbox. The controller separately records tool and file evidence, so model proposals are not the audit authority. Wait for an accepted receipt or inspect the rejected error, then reread the view. Do not write private chain-of-thought into working state; keep a short rationale, alternatives, evidence, and confidence. Provider-emitted reasoning is retained separately as restricted review evidence and is never a scientific authority. Use unique output paths for intermediate and failed attempts instead of overwriting or deleting them. Keep all result files under /sandbox/work/output. Required final files are methods.md, environment.txt, and results/result.json. Put runnable code under scripts, source-linked intermediate data under intermediate, failed or superseded attempts under discarded, figures under figures, and bounded execution notes under logs. You may inspect bounded input samples for schema discovery, quality control, and tool preparation. Do not use language-model decoding as the scientific computation unless the protocol explicitly declares an llm-method. Deterministic, statistical, and scientific-model claims must come from the declared tool or model and bind input digest, code, parameters, environment, seeds when applicable, outputs, and validation. The hashed assignment's result_contract is authoritative. Create the final result only when it is ready for validation. Copy its template and replace the placeholder strings. Use exactly those object keys with no additions. answer and every limitation must be strings. Each claim must use exactly statement, kind, evidence, and confidence. All result references are relative to /sandbox/work/output and must name existing regular files. Set worker to {:?} and status to complete. Do not read sibling workers, change input, expose credentials, or claim that agreement proves correctness. Study mode: {:?}.",
        worker.name, study.mode
    )
}
