//! Name and text checks, record paths, JSON files, hashes and time.
use super::*;

pub(super) fn validate_text(value: &str, label: &str) -> Result<()> {
    if value.trim().is_empty() || value.len() > MAX_TEXT_BYTES || value.contains(['\0', '\r']) {
        bail!("{label} must be nonempty UTF-8 text under 1 MiB without NUL or CR");
    }
    Ok(())
}

pub(super) fn validate_name(value: &str, label: &str) -> Result<()> {
    if value.is_empty()
        || value.len() > 32
        || value.starts_with('-')
        || value.ends_with('-')
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
    {
        bail!("{label} name must be a lowercase DNS label of at most 32 characters");
    }
    Ok(())
}

pub(super) fn validate_component(value: &str, label: &str) -> Result<()> {
    if value.is_empty()
        || value.len() > 255
        || value == "."
        || value == ".."
        || value.contains(['/', '\0', '\n', '\r'])
    {
        bail!("{label} filename is invalid");
    }
    Ok(())
}

pub(super) fn validate_result_class(value: &str) -> Result<()> {
    if value.is_empty()
        || value.len() > 64
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
    {
        bail!("research result class is invalid");
    }
    Ok(())
}

pub(super) fn require_nonempty_file(path: &Path, label: &str) -> Result<()> {
    let metadata = fs::symlink_metadata(path)
        .with_context(|| format!("research output is missing {label}"))?;
    if metadata.file_type().is_symlink() || !metadata.is_file() || metadata.len() == 0 {
        bail!("research output {label} must be a nonempty regular file");
    }
    if metadata.len() as usize > MAX_TEXT_BYTES * 4 {
        bail!("research output {label} exceeds the 4 MiB limit");
    }
    Ok(())
}

pub(super) fn normalize_answer(value: &str) -> String {
    value.split_whitespace().collect::<Vec<_>>().join(" ")
}

pub(super) fn default_protocol(mode: ResearchMode) -> String {
    format!(
        "# Study protocol\n\nMode: {:?}\n\nKeep inputs unchanged. The agent may read bounded samples for schema discovery, quality control, and tool preparation. Use a pinned deterministic tool, statistical model, or scientific model for scientific computation. Do not treat language-model decoding as the scientific instrument unless this protocol is edited to declare an llm-method, its model revision, engine, prompt, sampling settings, repeats, and evaluation. Record methods, scripts, parameters, environment, seeds when applicable, intermediate data, failed attempts, results, figures, evidence paths, validation, limitations, and rerun instructions. Work independently until the host seals the result.\n",
        mode
    )
}

pub(super) fn sandbox_name(study: &str, worker: &str) -> String {
    let digest = hex::encode(Sha256::digest(format!("{study}/{worker}")));
    let study = name_prefix(study);
    let worker = name_prefix(worker);
    format!("r-{study}-{worker}-{}", &digest[..4])
        .chars()
        .take(19)
        .collect()
}

pub(super) fn name_prefix(value: &str) -> String {
    value
        .chars()
        .filter(char::is_ascii_alphanumeric)
        .take(5)
        .collect()
}

pub(super) fn save(config: &RuntimeConfig, study: &ResearchStudy) -> Result<()> {
    validate_study(config, study)?;
    write_json(&record_path(config, &study.name), study)
}

pub(super) fn record_path(config: &RuntimeConfig, name: &str) -> PathBuf {
    config.research_dir().join(format!("{name}.json"))
}

pub(super) fn seal_path(study: &ResearchStudy, worker: &str) -> PathBuf {
    study.root.join("seals").join(format!("{worker}.json"))
}

pub(super) fn write_json(path: &Path, value: &impl Serialize) -> Result<()> {
    let mut body = serde_json::to_vec_pretty(value)?;
    body.push(b'\n');
    write_private(path, &body)
}

pub(super) fn read_json<T: for<'de> Deserialize<'de>>(path: &Path) -> Result<T> {
    let metadata =
        fs::symlink_metadata(path).with_context(|| format!("missing file {}", path.display()))?;
    if metadata.file_type().is_symlink() || !metadata.is_file() || metadata.len() > 16 * 1024 * 1024
    {
        bail!(
            "JSON record is not a bounded regular file: {}",
            path.display()
        );
    }
    Ok(serde_json::from_slice(&fs::read(path)?)?)
}

pub(super) fn hash_file(path: &Path) -> Result<String> {
    let metadata = fs::symlink_metadata(path)?;
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        bail!("hash target must be a regular file: {}", path.display());
    }
    let mut file = fs::File::open(path)?;
    let mut hasher = Sha256::new();
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        let read = file.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    Ok(hex::encode(hasher.finalize()))
}

pub(super) fn hash_bytes(value: &[u8]) -> String {
    hex::encode(Sha256::digest(value))
}

pub(super) fn now() -> Result<String> {
    Ok(OffsetDateTime::now_utc().format(&Rfc3339)?)
}
