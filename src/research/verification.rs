//! Comparing workers and verifying seals, materials, logs and tools.
use super::*;

pub fn compare(config: &RuntimeConfig, study_name: &str) -> Result<ResearchComparison> {
    let mut study = load(config, study_name)?;
    let sealed = study
        .workers
        .values()
        .filter(|worker| worker.state == ResearchWorkerState::Sealed)
        .cloned()
        .collect::<Vec<_>>();
    if sealed.len() < 2 {
        bail!("comparison needs at least two sealed workers");
    }
    let mut answer_groups = BTreeMap::<String, Vec<String>>::new();
    let mut routes = BTreeSet::new();
    let mut method_files_present = true;
    for worker in &sealed {
        verify_worker_seal(&study, worker)?;
        let result: ResearchResult = read_json(&worker.output.join("results/result.json"))?;
        let normalized = normalize_answer(&result.answer);
        let digest = hash_bytes(normalized.as_bytes());
        answer_groups
            .entry(digest)
            .or_default()
            .push(worker.name.clone());
        routes.insert(worker.route.clone());
        method_files_present &= worker.output.join("methods.md").is_file();
    }
    for workers in answer_groups.values_mut() {
        workers.sort();
    }
    let compared_at = now()?;
    let mut comparison = ResearchComparison {
        schema_version: 1,
        study: study_name.into(),
        compared_at,
        sealed_workers: sealed.len(),
        all_answers_agree: answer_groups.len() == 1,
        answer_groups,
        route_diversity: routes.len(),
        method_files_present,
        scientific_validation: "not-established-by-controller".into(),
        external_validation_required: true,
        sha256: String::new(),
    };
    comparison.sha256 = comparison_digest(&comparison)?;
    write_json(&study.root.join("seals/comparison.json"), &comparison)?;
    study.comparison = Some(comparison.clone());
    save(config, &study)?;
    audit::append(
        config,
        "host-controller",
        "research.compare",
        &format!("research/{study_name}"),
        "passed",
        json!({
            "sealed_workers": comparison.sealed_workers,
            "answer_groups": comparison.answer_groups.len(),
            "all_answers_agree": comparison.all_answers_agree,
            "route_diversity": comparison.route_diversity,
            "scientific_validation": comparison.scientific_validation
        }),
    )?;
    Ok(comparison)
}

pub fn verify(config: &RuntimeConfig, study_name: &str) -> Result<ResearchVerification> {
    let study = load(config, study_name)?;
    if hash_file(&study.question_path)? != study.question_sha256
        || hash_file(&study.protocol_path)? != study.protocol_sha256
        || hash_file(&study.input_manifest_path)? != study.input_manifest_sha256
    {
        bail!("study control files no longer match their recorded digests");
    }
    let manifest: SnapshotManifest = read_json(&study.input_manifest_path)?;
    if inventory_tree(&study.root.join("control/inputs"))? != manifest {
        bail!("study input snapshot no longer matches its manifest");
    }
    let mut valid = 0;
    for worker in study.workers.values() {
        if worker.state == ResearchWorkerState::Sealed {
            verify_worker_seal(&study, worker)?;
            valid += 1;
        }
    }
    let comparison_valid = match &study.comparison {
        Some(comparison) => {
            let current: ResearchComparison = read_json(&study.root.join("seals/comparison.json"))?;
            current == *comparison
                && current.study == study.name
                && current.sha256 == comparison_digest(&current)?
        }
        None => true,
    };
    if !comparison_valid {
        bail!("study comparison record does not match controller state");
    }
    audit::append(
        config,
        "host-controller",
        "research.verify",
        &format!("research/{study_name}"),
        "passed",
        json!({"worker_seals": valid, "comparison_valid": comparison_valid}),
    )?;
    Ok(ResearchVerification {
        study: study_name.into(),
        input_manifest_valid: true,
        worker_seals_valid: valid,
        comparison_valid,
    })
}

pub(super) fn verify_worker_seal(study: &ResearchStudy, worker: &ResearchWorker) -> Result<()> {
    let seal: ResearchSeal = read_json(&seal_path(study, &worker.name))?;
    verify_worker_materials(study, worker)?;
    let tool_digests = worker
        .tools
        .iter()
        .map(|tool| (tool.name.clone(), tool.sha256.clone()))
        .collect::<BTreeMap<_, _>>();
    if seal.study != study.name
        || seal.worker != worker.name
        || seal.question_sha256 != study.question_sha256
        || seal.protocol_sha256 != study.protocol_sha256
        || seal.input_manifest_sha256 != study.input_manifest_sha256
        || seal.perspective_sha256 != worker.perspective_sha256
        || seal.assignment_sha256 != worker.assignment_sha256
        || seal.route != worker.route
        || seal.harness != worker.harness
        || seal.route_context_limit != worker.route_context_limit
        || seal.route_output_limit != worker.route_output_limit
        || seal.route_pool_cap != worker.route_pool_cap
        || seal.thinking != worker.thinking
        || seal.tool_digests != tool_digests
        || seal.scientific_validation
    {
        bail!("research seal identity does not match the study");
    }
    let (output_sha256, output_files, output_bytes) = hash_path_with_limits(
        &worker.output,
        study.max_output_files,
        study.max_output_bytes,
    )?;
    let result_sha256 = hash_file(&worker.output.join("results/result.json"))?;
    if output_sha256 != seal.output_sha256
        || output_files != seal.output_files
        || output_bytes != seal.output_bytes
        || result_sha256 != seal.result_sha256
    {
        bail!("research worker output no longer matches its seal");
    }
    Ok(())
}

pub(super) fn comparison_digest(comparison: &ResearchComparison) -> Result<String> {
    let unsigned = json!({
        "schema_version": comparison.schema_version,
        "study": comparison.study,
        "compared_at": comparison.compared_at,
        "sealed_workers": comparison.sealed_workers,
        "answer_groups": comparison.answer_groups,
        "all_answers_agree": comparison.all_answers_agree,
        "route_diversity": comparison.route_diversity,
        "method_files_present": comparison.method_files_present,
        "scientific_validation": comparison.scientific_validation,
        "external_validation_required": comparison.external_validation_required
    });
    Ok(hash_bytes(&serde_json::to_vec(&unsigned)?))
}

pub(super) fn verify_worker_materials(
    study: &ResearchStudy,
    worker: &ResearchWorker,
) -> Result<()> {
    let canonical_inputs: SnapshotManifest = read_json(&study.input_manifest_path)?;
    if inventory_tree(&worker.workspace.join("input"))? != canonical_inputs {
        bail!("worker input snapshot changed");
    }
    if hash_file(&worker.workspace.join("question.txt"))? != study.question_sha256
        || hash_file(&worker.workspace.join("protocol.md"))? != study.protocol_sha256
        || hash_file(&worker.workspace.join("input-manifest.json"))? != study.input_manifest_sha256
        || hash_file(&worker.perspective_path)? != worker.perspective_sha256
        || hash_file(&worker.workspace.join("research-assignment.json"))?
            != worker.assignment_sha256
    {
        bail!("worker assignment material changed");
    }
    for tool in &worker.tools {
        let (digest, file_count, _) = hash_path(&worker.workspace.join("tools").join(&tool.name))?;
        if digest != tool.sha256 || file_count != tool.file_count {
            bail!("worker tool copy changed: {}", tool.name);
        }
    }
    Ok(())
}

pub(super) fn stage_controller_logs(study: &ResearchStudy, worker: &ResearchWorker) -> Result<()> {
    let source = study.root.join("controller-logs").join(&worker.name);
    if !source.is_dir() {
        return Ok(());
    }
    let destination = worker.output.join("logs/controller");
    if destination.exists() {
        if hash_path(&source)? != hash_path(&destination)? {
            bail!("reserved controller research log destination has different bytes");
        }
        return Ok(());
    }
    copy_tree(&source, &destination)?;
    if hash_path(&source)? != hash_path(&destination)? {
        bail!("staged controller research logs failed digest verification");
    }
    Ok(())
}

pub(super) fn verified_tools(
    config: &RuntimeConfig,
    names: &[String],
) -> Result<Vec<catalog::CatalogEntry>> {
    let mut unique = BTreeSet::new();
    let mut entries = Vec::new();
    for name in names {
        if !unique.insert(name.clone()) {
            bail!("research tool was requested more than once: {name}");
        }
        let mut verified = catalog::verify(config, Some(name))?;
        entries.push(
            verified
                .pop()
                .context("catalog verification returned no entry")?,
        );
    }
    Ok(entries)
}
