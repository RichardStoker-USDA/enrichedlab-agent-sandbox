//! Parsing, identifier and text checks, bounded reads, hashing and time.
use super::*;

pub(super) fn parse_domain(value: &str) -> Result<StateDomain> {
    match value {
        "generic" => Ok(StateDomain::Generic),
        "scientific" => Ok(StateDomain::Scientific),
        "cluster-ops" => Ok(StateDomain::ClusterOps),
        "software-engineering" => Ok(StateDomain::SoftwareEngineering),
        _ => bail!("unknown state domain {value}"),
    }
}

pub(super) fn parse_authority(value: &str) -> Result<StateAuthority> {
    match value {
        "machine" => Ok(StateAuthority::Machine),
        "model" => Ok(StateAuthority::Model),
        "operator" => Ok(StateAuthority::Operator),
        _ => bail!("unknown state authority {value}"),
    }
}

pub(super) fn parse_artifact_sensitivity(value: &str) -> Result<ArtifactSensitivity> {
    match value {
        "standard" => Ok(ArtifactSensitivity::Standard),
        "restricted" => Ok(ArtifactSensitivity::Restricted),
        _ => bail!("unknown artifact sensitivity {value}"),
    }
}

pub(super) fn validate_identifier(value: &str, label: &str) -> Result<()> {
    if value.is_empty()
        || value.len() > 128
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
    {
        bail!("{label} must use 1-128 ASCII letters, digits, hyphens, or underscores");
    }
    Ok(())
}

pub(super) fn validate_event_kind(value: &str) -> Result<()> {
    if value.is_empty()
        || value.len() > 128
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'-' | b'_'))
    {
        bail!("event kind is invalid");
    }
    Ok(())
}

pub(super) fn validate_text(value: &str, label: &str, max: usize) -> Result<()> {
    if value.trim().is_empty()
        || value.len() > max
        || value.contains(['\0', '\r'])
        || value
            .chars()
            .any(|character| character.is_control() && character != '\n' && character != '\t')
    {
        bail!("{label} is invalid");
    }
    Ok(())
}

pub(super) fn state_like_pattern(value: &str) -> String {
    let escaped = value
        .replace('\\', "\\\\")
        .replace('%', "\\%")
        .replace('_', "\\_");
    format!("%{escaped}%")
}

pub(super) fn validate_outbox_name(value: &str) -> Result<()> {
    let path = Path::new(value);
    if value.len() > 256
        || !value.ends_with(".json")
        || path
            .components()
            .any(|component| !matches!(component, Component::Normal(_)))
        || value.contains(['\0', '\n', '\r'])
    {
        bail!("state outbox filename is invalid");
    }
    Ok(())
}

pub(super) fn read_bounded_regular(path: &Path, max: u64) -> Result<Vec<u8>> {
    let metadata = fs::symlink_metadata(path)
        .with_context(|| format!("failed to inspect {}", path.display()))?;
    if metadata.file_type().is_symlink() || !metadata.is_file() || metadata.len() > max {
        bail!("input must be a bounded regular file: {}", path.display());
    }
    Ok(fs::read(path)?)
}

pub(super) fn copy_private_atomic(
    source: &Path,
    destination: &Path,
    expected_sha256: &str,
) -> Result<()> {
    let pending = destination.with_file_name(format!(
        ".{}.{}.pending",
        destination
            .file_name()
            .and_then(|value| value.to_str())
            .context("invalid artifact filename")?,
        Uuid::new_v4()
    ));
    let mut options = fs::OpenOptions::new();
    options.create_new(true).write(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut input = fs::File::open(source)?;
    let mut output = options.open(&pending)?;
    std::io::copy(&mut input, &mut output)?;
    output.sync_all()?;
    set_mode(&pending, 0o600)?;
    let (observed, _) = hash_file(&pending)?;
    if observed != expected_sha256 {
        let _ = fs::remove_file(&pending);
        bail!("artifact changed while it was copied");
    }
    fs::rename(&pending, destination)?;
    Ok(())
}

pub(super) fn hash_file(path: &Path) -> Result<(String, u64)> {
    let metadata = fs::symlink_metadata(path)?;
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        bail!("hash target must be a regular file");
    }
    let mut file = fs::File::open(path)?;
    let mut hasher = Sha256::new();
    let mut buffer = [0u8; 1024 * 1024];
    let mut size = 0u64;
    loop {
        let read = file.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        size += read as u64;
        hasher.update(&buffer[..read]);
    }
    Ok((hex::encode(hasher.finalize()), size))
}

pub(super) fn hash_value(value: &Value) -> Result<String> {
    Ok(hash_bytes(&serde_json::to_vec(value)?))
}

pub(super) fn hash_bytes(value: &[u8]) -> String {
    hex::encode(Sha256::digest(value))
}

pub(super) fn estimate_tokens(bytes: usize) -> u64 {
    (bytes as u64).div_ceil(4)
}

pub(super) fn bounded_workspace_error(
    config: &RuntimeConfig,
    workspace: &Path,
    error: &anyhow::Error,
) -> String {
    let mut value = format!("{error:#}");
    for (path, replacement) in [
        (workspace, "<workspace>"),
        (config.state_dir.as_path(), "<controller-state>"),
        (config.config_dir.as_path(), "<controller-config>"),
        (config.root.as_path(), "<controller-root>"),
    ] {
        if let Some(path) = path.to_str() {
            value = value.replace(path, replacement);
        }
    }
    value.chars().take(4096).collect()
}

pub(super) fn now() -> Result<String> {
    Ok(OffsetDateTime::now_utc().format(&Rfc3339)?)
}
