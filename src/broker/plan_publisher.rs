//! Atomically publish one operator-approved same-name custodian plan.
//!
//! This module only moves a descriptor-validated private plan into the fixed
//! same-name plan root. Custodians still validate task, notice, binding, and
//! plan authority before any side effect.
use super::{admission::open_bounded_record, admission_publication::publish_private};
use anyhow::{Result, anyhow, ensure};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    fmt,
    io::Read,
    path::{Component, Path, PathBuf},
};
use uuid::Uuid;

const PLAN_LIMIT: u64 = 1024 * 1024;

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum CustodyPlanKind {
    Input,
    Output,
}
impl CustodyPlanKind {
    fn schema(self) -> &'static str {
        match self {
            Self::Input => "sandbox.enrichedlab.org/input-preparation-plan/v2",
            Self::Output => "sandbox.enrichedlab.org/output-preparation-plan/v1",
        }
    }

    fn label(self) -> &'static str {
        match self {
            Self::Input => "input",
            Self::Output => "output",
        }
    }
}
impl std::str::FromStr for CustodyPlanKind {
    type Err = anyhow::Error;

    fn from_str(value: &str) -> Result<Self> {
        match value {
            "input" => Ok(Self::Input),
            "output" => Ok(Self::Output),
            _ => Err(anyhow!("plan_publisher_kind_invalid")),
        }
    }
}
impl fmt::Display for CustodyPlanKind {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.label())
    }
}
impl fmt::Debug for CustodyPlanKind {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.label())
    }
}

/// Configuration comes from a trusted operator invocation. It is intentionally
/// not an authorization record and never appears in a publication receipt.
#[derive(Clone)]
pub struct PlanPublicationConfig {
    pub source_root: PathBuf,
    pub source_name: String,
    pub source_writer_uid: u32,
    pub plan_root: PathBuf,
    pub plan_writer_uid: u32,
    pub kind: CustodyPlanKind,
    pub task_id: Uuid,
    pub attempt: i32,
    pub lease_generation: i64,
    /// Exact lowercase `sha256:<64 hex>` digest of the original approved bytes.
    pub expected_sha256: String,
}
impl fmt::Debug for PlanPublicationConfig {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("PlanPublicationConfig")
            .field("source_writer_uid", &self.source_writer_uid)
            .field("plan_writer_uid", &self.plan_writer_uid)
            .field("kind", &self.kind)
            .field("task_id", &self.task_id)
            .field("attempt", &self.attempt)
            .field("lease_generation", &self.lease_generation)
            .field("expected_sha256", &self.expected_sha256)
            .finish()
    }
}

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct PlanPublicationReceipt {
    pub plan_kind: String,
    pub task_id: Uuid,
    pub attempt: i32,
    pub lease_generation: i64,
    pub plan_name: String,
    pub sha256: String,
    pub size_bytes: u64,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct PlanEnvelope {
    schema_version: String,
    task: PlanTask,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct PlanTask {
    task_id: Uuid,
    attempt: i32,
    lease_generation: i64,
}

/// Copy the exact original bytes only after checking the limited envelope that
/// routes this plan. Full plan and task authority remain custodian-owned.
pub fn publish(config: &PlanPublicationConfig) -> Result<PlanPublicationReceipt> {
    validate_config(config)?;
    let source = open_bounded_record(
        &config.source_root,
        &config.source_name,
        config.source_writer_uid,
        PLAN_LIMIT,
    )
    .map_err(|_| anyhow!("plan_publisher_source_unavailable"))?;
    let mut bytes = Vec::new();
    source
        .take(PLAN_LIMIT + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| anyhow!("plan_publisher_source_read_failed"))?;
    ensure!(
        bytes.len() as u64 <= PLAN_LIMIT,
        "plan_publisher_source_oversize"
    );
    let observed = format!("sha256:{}", hex::encode(Sha256::digest(&bytes)));
    ensure!(
        observed == config.expected_sha256,
        "plan_publisher_digest_mismatch"
    );
    let envelope: PlanEnvelope =
        serde_json::from_slice(&bytes).map_err(|_| anyhow!("plan_publisher_plan_decode_failed"))?;
    ensure!(
        envelope.schema_version == config.kind.schema(),
        "plan_publisher_schema_mismatch"
    );
    ensure!(
        envelope.task.task_id == config.task_id
            && envelope.task.attempt == config.attempt
            && envelope.task.lease_generation == config.lease_generation,
        "plan_publisher_task_scope_mismatch"
    );
    let plan_name = plan_name(config.task_id, config.attempt, config.lease_generation)?;
    publish_private(
        &config.plan_root,
        &plan_name,
        config.plan_writer_uid,
        &bytes,
    )
    .map_err(map_destination_error)?;
    Ok(PlanPublicationReceipt {
        plan_kind: config.kind.label().into(),
        task_id: config.task_id,
        attempt: config.attempt,
        lease_generation: config.lease_generation,
        plan_name,
        sha256: observed,
        size_bytes: bytes.len() as u64,
    })
}

fn validate_config(config: &PlanPublicationConfig) -> Result<()> {
    ensure!(
        private_root(&config.source_root)
            && private_root(&config.plan_root)
            && !config.source_root.starts_with(&config.plan_root)
            && !config.plan_root.starts_with(&config.source_root),
        "plan_publisher_root_invalid"
    );
    ensure!(
        safe_name(&config.source_name)
            && config.attempt > 0
            && config.lease_generation > 0
            && valid_sha256(&config.expected_sha256),
        "plan_publisher_config_invalid"
    );
    Ok(())
}

fn plan_name(task_id: Uuid, attempt: i32, lease_generation: i64) -> Result<String> {
    ensure!(
        attempt > 0 && lease_generation > 0,
        "plan_publisher_scope_invalid"
    );
    Ok(format!("{task_id}-{attempt}-{lease_generation}.json"))
}

fn private_root(path: &Path) -> bool {
    path.is_absolute()
        && path != Path::new("/")
        && path
            .components()
            .all(|part| matches!(part, Component::RootDir | Component::Normal(_)))
}

fn safe_name(name: &str) -> bool {
    !name.is_empty() && !name.contains(['/', '\\', '\0']) && !matches!(name, "." | "..")
}

fn valid_sha256(value: &str) -> bool {
    value.len() == 71
        && value.starts_with("sha256:")
        && value[7..]
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn map_destination_error(error: anyhow::Error) -> anyhow::Error {
    let has = |code: &str| error.chain().any(|cause| cause.to_string() == code);
    if has("admission_existing_record_conflict") || has("admission_existing_record_invalid") {
        anyhow!("plan_publisher_destination_conflict")
    } else if has("admission_private_root_invalid") || has("admission_private_root_mode") {
        anyhow!("plan_publisher_destination_root_invalid")
    } else if has("admission_private_record_name") {
        anyhow!("plan_publisher_destination_name_invalid")
    } else {
        anyhow!("plan_publisher_destination_unavailable")
    }
}
