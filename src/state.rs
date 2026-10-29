use std::{
    fs,
    path::{Path, PathBuf},
};

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};

use crate::{
    cli::{ApprovalMode, LandlockMode},
    config::{RuntimeConfig, write_private},
    platform::RuntimeContract,
    workspace::MountRecord,
};

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct SandboxRecord {
    pub schema_version: u32,
    pub name: String,
    pub route: String,
    #[serde(default = "default_profile")]
    pub profile: String,
    #[serde(default)]
    pub runtime: RuntimeContract,
    pub openshell_workspace: String,
    pub host_workspace: PathBuf,
    pub managed_workspace: bool,
    pub mounts: Vec<MountRecord>,
    pub image: String,
    pub cpu: String,
    pub memory: String,
    pub read_hosts: Vec<String>,
    pub write_hosts: Vec<String>,
    pub policy_path: PathBuf,
    #[serde(default = "default_approval_mode")]
    pub approval_mode: ApprovalMode,
    #[serde(default = "default_landlock")]
    pub landlock: LandlockMode,
    #[serde(default)]
    pub policy_advisor_enabled: bool,
    #[serde(default)]
    pub ocsf_json_enabled: bool,
    #[serde(default)]
    pub control_requests: PathBuf,
    #[serde(default)]
    pub control_decisions: PathBuf,
    pub created_at_unix: u64,
}

fn default_profile() -> String {
    "development".into()
}
fn default_approval_mode() -> ApprovalMode {
    ApprovalMode::Manual
}
fn default_landlock() -> LandlockMode {
    LandlockMode::BestEffort
}

pub fn save(config: &RuntimeConfig, record: &SandboxRecord) -> Result<()> {
    validate_record(record)?;
    let body = serde_json::to_vec_pretty(record)?;
    let mut body_with_newline = body;
    body_with_newline.push(b'\n');
    write_private(&record_path(config, &record.name)?, &body_with_newline)
}

pub fn load(config: &RuntimeConfig, name: &str) -> Result<SandboxRecord> {
    let path = record_path(config, name)?;
    let body = fs::read(&path)
        .with_context(|| format!("sandbox {name} is not managed by this controller"))?;
    let record: SandboxRecord = serde_json::from_slice(&body)
        .with_context(|| format!("invalid controller record {}", path.display()))?;
    validate_record(&record)?;
    Ok(record)
}

pub fn list(config: &RuntimeConfig) -> Result<Vec<SandboxRecord>> {
    let mut records = Vec::new();
    for entry in fs::read_dir(config.records_dir())? {
        let entry = entry?;
        let path = entry.path();
        if path.extension().and_then(|value| value.to_str()) != Some("json") {
            continue;
        }
        let body = fs::read(&path)?;
        let record: SandboxRecord = serde_json::from_slice(&body)
            .with_context(|| format!("invalid controller record {}", path.display()))?;
        validate_record(&record)?;
        records.push(record);
    }
    records.sort_by(|left, right| left.name.cmp(&right.name));
    Ok(records)
}

pub fn remove(config: &RuntimeConfig, record: &SandboxRecord) -> Result<()> {
    for path in [
        record_path(config, &record.name)?,
        record.policy_path.clone(),
    ] {
        if path.exists() {
            fs::remove_file(&path)
                .with_context(|| format!("failed to remove {}", path.display()))?;
        }
    }
    Ok(())
}

pub fn record_path(config: &RuntimeConfig, name: &str) -> Result<PathBuf> {
    validate_name(name)?;
    Ok(config.records_dir().join(format!("{name}.json")))
}

pub fn policy_path(config: &RuntimeConfig, name: &str) -> Result<PathBuf> {
    validate_name(name)?;
    Ok(config.policies_dir().join(format!("{name}.yaml")))
}

pub fn control_paths(config: &RuntimeConfig, name: &str) -> Result<(PathBuf, PathBuf, PathBuf)> {
    validate_name(name)?;
    let root = config.controls_dir().join(name);
    Ok((
        root.join("requests"),
        root.join("decisions"),
        root.join("runtime"),
    ))
}

pub fn validate_name(name: &str) -> Result<()> {
    if name.is_empty()
        || name.len() > 19
        || name.starts_with('-')
        || name.ends_with('-')
        || name.contains("--")
        || !name
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
    {
        bail!(
            "sandbox name must be a lowercase DNS label of at most 19 characters without consecutive hyphens"
        );
    }
    Ok(())
}

pub fn other_records_use_workspace(config: &RuntimeConfig, record: &SandboxRecord) -> Result<bool> {
    Ok(list(config)?.iter().any(|candidate| {
        candidate.name != record.name && candidate.host_workspace == record.host_workspace
    }))
}

pub fn is_under(path: &Path, root: &Path) -> bool {
    path.starts_with(root)
}

fn validate_record(record: &SandboxRecord) -> Result<()> {
    validate_name(&record.name)?;
    if !(1..=3).contains(&record.schema_version) {
        bail!(
            "unsupported sandbox record schema {}",
            record.schema_version
        );
    }
    if record.route.trim().is_empty() || record.profile.trim().is_empty() {
        bail!("sandbox route and profile cannot be empty");
    }
    record.runtime.validate(record.schema_version)?;
    if !record.host_workspace.is_absolute() || !record.policy_path.is_absolute() {
        bail!("sandbox record paths must be absolute");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::validate_name;

    #[test]
    fn validates_dns_safe_names() {
        assert!(validate_name("agent-a1b2c3").is_ok());
        for invalid in ["Agent", "agent_name", "-agent", "agent-", "agent--worker"] {
            assert!(validate_name(invalid).is_err());
        }
    }
}
