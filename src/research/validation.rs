//! Checking results, studies and input sources.
use super::*;

pub(super) fn validate_result(result: &ResearchResult, worker: &str, output: &Path) -> Result<()> {
    if result.schema_version != 2 || result.worker != worker || result.status != "complete" {
        bail!("research result identity or status is invalid");
    }
    validate_text(&result.answer, "answer")?;
    if result.claims.len() > MAX_CLAIMS || result.limitations.len() > MAX_CLAIMS {
        bail!("research result exceeds the claim or limitation cap");
    }
    for claim in &result.claims {
        validate_text(&claim.statement, "claim statement")?;
        if !matches!(claim.kind.as_str(), "deterministic" | "interpretive") {
            bail!("research claim kind must be deterministic or interpretive");
        }
        if !claim.confidence.is_finite() || !(0.0..=1.0).contains(&claim.confidence) {
            bail!("research claim confidence must be between 0 and 1");
        }
        if claim.evidence.is_empty() || claim.evidence.len() > 64 {
            bail!("research claim needs between 1 and 64 evidence paths");
        }
        for relative in &claim.evidence {
            require_output_file(output, relative, "claim evidence")?;
        }
    }
    for limitation in &result.limitations {
        validate_text(limitation, "limitation")?;
    }
    validate_text(&result.rerun.command, "rerun command")?;
    require_output_file(output, &result.rerun.environment, "rerun environment")?;
    let method = result
        .method
        .as_ref()
        .context("research result schema 2 requires a method contract")?;
    let quality_control = result
        .quality_control
        .as_ref()
        .context("research result schema 2 requires a quality-control contract")?;
    if !matches!(
        method.class.as_str(),
        "deterministic-tool"
            | "statistical-model"
            | "scientific-model"
            | "human-reviewed-interpretation"
            | "llm-method"
    ) {
        bail!("research method class is not supported");
    }
    validate_text(&method.tool_or_model, "method tool or model")?;
    validate_text(&method.version, "method version")?;
    if method.input_manifest_sha256.len() != 64
        || !method
            .input_manifest_sha256
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit())
    {
        bail!("method input manifest digest must be SHA-256");
    }
    require_result_refs(output, &method.code_refs, "method code", false)?;
    require_result_refs(output, &method.parameter_refs, "method parameters", false)?;
    require_result_refs(output, &method.output_refs, "method output", false)?;
    require_result_refs(output, &method.validation_refs, "method validation", false)?;
    for seed in &method.seeds {
        validate_text(seed, "method seed")?;
    }
    if matches!(
        method.class.as_str(),
        "statistical-model" | "scientific-model" | "llm-method"
    ) && method.seeds.is_empty()
    {
        bail!(
            "stochastic or model-based methods must record at least one seed or deterministic seed declaration"
        );
    }
    validate_text(&quality_control.purpose, "quality-control purpose")?;
    require_result_refs(
        output,
        &quality_control.schema_refs,
        "quality-control schema",
        true,
    )?;
    require_result_refs(
        output,
        &quality_control.sample_refs,
        "quality-control sample",
        true,
    )?;
    require_result_refs(
        output,
        &quality_control.check_refs,
        "quality-control check",
        false,
    )?;
    match (&method.llm_disclosure, method.class.as_str()) {
        (Some(disclosure), "llm-method") => {
            if !quality_control.llm_is_scientific_instrument {
                bail!("an llm-method must declare that the LLM is the scientific instrument");
            }
            validate_text(&disclosure.model, "LLM method model")?;
            validate_text(&disclosure.revision, "LLM method revision")?;
            validate_text(&disclosure.engine, "LLM method engine")?;
            if disclosure.repeat_count < 2 || disclosure.repeat_count > 10_000 {
                bail!("an llm-method repeat count must be between 2 and 10000");
            }
            require_output_file(output, &disclosure.prompt_ref, "LLM method prompt")?;
            require_output_file(output, &disclosure.sampling_ref, "LLM method sampling")?;
            require_result_refs(
                output,
                &disclosure.evaluation_refs,
                "LLM method evaluation",
                false,
            )?;
        }
        (None, "llm-method") => bail!("an llm-method requires a complete LLM disclosure"),
        (Some(_), _) => bail!("LLM disclosure is accepted only for an llm-method"),
        (None, _) if quality_control.llm_is_scientific_instrument => {
            bail!("the LLM cannot be marked as the scientific instrument for this method class")
        }
        (None, _) => {}
    }
    Ok(())
}

pub(super) fn require_result_refs(
    output: &Path,
    references: &[String],
    label: &str,
    allow_empty: bool,
) -> Result<()> {
    if references.len() > 64 || (!allow_empty && references.is_empty()) {
        bail!(
            "{label} must contain {} through 64 paths",
            if allow_empty { 0 } else { 1 }
        );
    }
    for reference in references {
        require_output_file(output, reference, label)?;
    }
    Ok(())
}

pub(super) fn validate_method_input_manifest(
    result: &ResearchResult,
    expected: &str,
) -> Result<()> {
    let observed = result
        .method
        .as_ref()
        .context("research result has no scientific method contract")?
        .input_manifest_sha256
        .as_str();
    if observed != expected {
        bail!("scientific method input digest does not match the immutable study input snapshot");
    }
    Ok(())
}

pub(super) fn require_output_file(root: &Path, value: &str, label: &str) -> Result<()> {
    let relative = safe_relative_path(value, label)?;
    let candidate = root.join(relative);
    let metadata = fs::symlink_metadata(&candidate)
        .with_context(|| format!("{label} does not exist: {value}"))?;
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        bail!("{label} must be a regular file: {value}");
    }
    let canonical_root = fs::canonicalize(root)?;
    let canonical = fs::canonicalize(&candidate)?;
    if !canonical.starts_with(canonical_root) {
        bail!("{label} escapes the output root: {value}");
    }
    Ok(())
}

pub(super) fn safe_relative_path<'a>(value: &'a str, label: &str) -> Result<&'a Path> {
    let path = Path::new(value);
    if value.is_empty()
        || value.len() > 1024
        || path.is_absolute()
        || value.contains(['\0', '\n', '\r'])
        || path.components().any(|component| {
            matches!(
                component,
                Component::ParentDir | Component::RootDir | Component::Prefix(_)
            )
        })
    {
        bail!("{label} must be a bounded relative path");
    }
    Ok(path)
}

pub(super) fn validate_study(config: &RuntimeConfig, study: &ResearchStudy) -> Result<()> {
    if study.schema_version != 1 {
        bail!("unsupported research study schema");
    }
    validate_name(&study.name, "study")?;
    let expected_root = config.root.join("studies").join(&study.name);
    if study.root != expected_root
        || study.question_path != expected_root.join("control/question.txt")
        || study.protocol_path != expected_root.join("control/protocol.md")
        || study.input_manifest_path != expected_root.join("control/input-manifest.json")
    {
        bail!("research study paths are outside the controller-owned study tree");
    }
    let root_metadata = fs::symlink_metadata(&study.root)?;
    if root_metadata.file_type().is_symlink() || !root_metadata.is_dir() {
        bail!("research study root must be a controller-owned directory");
    }
    if study.max_output_files == 0
        || study.max_output_files > MAX_SNAPSHOT_FILES
        || study.max_output_bytes == 0
        || study.max_output_bytes > MAX_SNAPSHOT_BYTES
    {
        bail!("research study output limits are outside controller safety caps");
    }
    for (name, worker) in &study.workers {
        validate_name(name, "worker")?;
        let expected_workspace = expected_root.join("workers").join(name);
        if worker.name != *name
            || worker.sandbox != sandbox_name(&study.name, name)
            || worker.workspace != expected_workspace
            || worker.output != expected_workspace.join("output")
            || worker.perspective_path != expected_workspace.join("perspective.txt")
        {
            bail!("research worker identity or paths are outside its study tree");
        }
        if worker.route_context_limit == 0
            || worker.route_output_limit == 0
            || worker.route_pool_cap == 0
            || worker.route_pool_cap > 64
        {
            bail!("research worker route limits are invalid");
        }
        if worker.segments_completed > 1_000_000
            || (worker.segments_completed == 0) != worker.last_segment_at.is_none()
            || worker
                .last_segment_at
                .as_ref()
                .is_some_and(|value| OffsetDateTime::parse(value, &Rfc3339).is_err())
        {
            bail!("research worker segment custody is invalid");
        }
        match (&worker.state_run_id, &worker.state_profile) {
            (Some(run_id), Some(profile)) => {
                let expected_run = format!("{}-state", sandbox_name(&study.name, name));
                if run_id != &expected_run || profile != "scientific-worker" {
                    bail!("research worker state identity is invalid");
                }
            }
            (None, None) => {}
            _ => bail!("research worker state run and profile must be set together"),
        }
        let mut tool_names = BTreeSet::new();
        for tool in &worker.tools {
            validate_component(&tool.name, "research tool")?;
            if !tool_names.insert(&tool.name)
                || tool.sandbox_path != format!("/sandbox/work/tools/{}", tool.name)
            {
                bail!("research worker tool identity is invalid");
            }
        }
    }
    if let Some(comparison) = &study.comparison
        && (comparison.schema_version != 1
            || comparison.study != study.name
            || comparison.scientific_validation != "not-established-by-controller"
            || !comparison.external_validation_required
            || comparison.sha256 != comparison_digest(comparison)?)
    {
        bail!("research comparison record is invalid");
    }
    Ok(())
}

pub(super) fn validate_input_sources(inputs: &[PathBuf]) -> Result<()> {
    let mut names = BTreeSet::new();
    for source in inputs {
        if !source.is_absolute() {
            bail!(
                "research inputs must use absolute paths: {}",
                source.display()
            );
        }
        let source_metadata = fs::symlink_metadata(source)?;
        if source_metadata.file_type().is_symlink()
            || (!source_metadata.is_file() && !source_metadata.is_dir())
        {
            bail!("research input must be a regular file or directory");
        }
        let canonical = fs::canonicalize(source)?;
        let metadata = fs::symlink_metadata(&canonical)?;
        if metadata.file_type().is_symlink() || (!metadata.is_file() && !metadata.is_dir()) {
            bail!("research input did not resolve to a regular file or directory");
        }
        let name = canonical
            .file_name()
            .and_then(|value| value.to_str())
            .context("research input has no UTF-8 filename")?;
        validate_component(name, "research input")?;
        if !names.insert(name.to_string()) {
            bail!("research input basenames must be unique: {name}");
        }
    }
    Ok(())
}
