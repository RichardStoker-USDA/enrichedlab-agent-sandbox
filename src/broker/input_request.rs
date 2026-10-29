//! Private worker-to-custodian preparation notices. A notice identifies an
//! existing claimed task; it never grants data access or creates a second task.
use super::{
    admission::open_bounded_record,
    admission_publication::publish_private,
    model::ClaimedTask,
    sandbox::{SandboxBinding, SandboxBindingCatalog},
};
use anyhow::{Result, anyhow, ensure};
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::{
    fmt,
    io::Read,
    path::{Component, Path, PathBuf},
};
use uuid::Uuid;

const SCHEMA: &str = "sandbox.enrichedlab.org/input-preparation-request/v1";
const LIMIT: u64 = 16 * 1024;

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct InputPreparationConfig {
    pub root: PathBuf,
    pub writer_uid: u32,
}
impl InputPreparationConfig {
    pub(crate) fn validate(&self, admission_root: &Path, wait_seconds: u32) -> Result<()> {
        ensure!(
            wait_seconds > 0
                && self.root.is_absolute()
                && self.root != Path::new("/")
                && self
                    .root
                    .components()
                    .all(|part| matches!(part, Component::RootDir | Component::Normal(_)))
                && !self.root.starts_with(admission_root)
                && !admission_root.starts_with(&self.root),
            "input_preparation_config_invalid"
        );
        Ok(())
    }
}

/// Only the fields needed to find and verify the current claimed task. It
/// contains no objective, prompts, model routes, executable, data or file paths.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PreparationTaskIdentity {
    pub task_id: Uuid,
    pub principal_id: Uuid,
    pub group_key: String,
    pub profile_key: String,
    pub attempt: i32,
    pub lease_generation: i64,
    pub max_run_seconds: i32,
}
impl PreparationTaskIdentity {
    pub(crate) fn from_task(task: &ClaimedTask) -> Self {
        Self {
            task_id: task.id,
            principal_id: task.principal_id,
            group_key: task.group_key.clone(),
            profile_key: task.profile_key.clone(),
            attempt: task.attempt,
            lease_generation: task.lease_generation,
            max_run_seconds: task.max_run_seconds,
        }
    }
    pub fn matches(&self, task: &ClaimedTask) -> bool {
        *self == Self::from_task(task)
    }
    fn name(&self) -> Result<String> {
        ensure!(
            self.attempt > 0 && self.lease_generation > 0 && self.max_run_seconds > 0,
            "input_preparation_task_invalid"
        );
        Ok(format!(
            "{}-{}-{}.json",
            self.task_id, self.attempt, self.lease_generation
        ))
    }
}

/// Digest-only reference to model-generated code the broker admitted for this
/// task. The bytes never enter the notice or plan: the input custodian reads
/// them for the current lease, and the copy seal binds them to this digest.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AdmittedCodeRef {
    pub template_ref: String,
    pub path: String,
    pub size_bytes: u64,
    pub sha256: String,
}
pub const ADMITTED_CODE_PATH: &str = "generated/block.R";
pub const ADMITTED_CODE_LIMIT: u64 = 16 * 1024;
impl AdmittedCodeRef {
    pub fn validate(&self) -> Result<()> {
        let template = self.template_ref.strip_prefix("template://").unwrap_or("");
        ensure!(
            !template.is_empty()
                && template.len() <= 128
                && template
                    .bytes()
                    .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b"._/-".contains(&b))
                && !template.contains("..")
                && self.path == ADMITTED_CODE_PATH
                && (1..=ADMITTED_CODE_LIMIT).contains(&self.size_bytes)
                && self.sha256.len() == 71
                && self.sha256.starts_with("sha256:")
                && self.sha256[7..]
                    .bytes()
                    .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)),
            "admitted_code_reference_invalid"
        );
        Ok(())
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct RequestDocument {
    schema_version: String,
    task: PreparationTaskIdentity,
    worker_id: String,
    binding: SandboxBinding,
    #[serde(default)]
    admitted_code: Option<AdmittedCodeRef>,
}

/// Reading a valid notice is not acceptance of its lease. The custodian must
/// recheck the database fence and resolve approved inputs from site authority.
pub struct InputPreparationRequest {
    document: RequestDocument,
}
impl fmt::Debug for InputPreparationRequest {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("InputPreparationRequest")
            .field("task_id", &self.document.task.task_id)
            .field("attempt", &self.document.task.attempt)
            .field("lease_generation", &self.document.task.lease_generation)
            .finish_non_exhaustive()
    }
}
impl InputPreparationRequest {
    pub fn task(&self) -> &PreparationTaskIdentity {
        &self.document.task
    }
    pub fn worker_id(&self) -> &str {
        &self.document.worker_id
    }
    pub fn binding(&self) -> &SandboxBinding {
        &self.document.binding
    }
    pub fn admitted_code(&self) -> Option<&AdmittedCodeRef> {
        self.document.admitted_code.as_ref()
    }

    /// The scanner supplies only a filename under its configured private root.
    /// The record is descriptor-validated and must match that exact filename.
    pub fn load(root: &Path, name: &str, writer_uid: u32) -> Result<Self> {
        ensure!(
            !name.is_empty() && !name.contains(['/', '\\', '\0']) && name != "." && name != "..",
            "input_preparation_name_invalid"
        );
        let file = open_bounded_record(root, name, writer_uid, LIMIT)
            .map_err(|_| anyhow!("input_preparation_record_unavailable"))?;
        let mut bytes = Vec::new();
        file.take(LIMIT + 1)
            .read_to_end(&mut bytes)
            .map_err(|_| anyhow!("input_preparation_read_failed"))?;
        ensure!(
            bytes.len() as u64 <= LIMIT,
            "input_preparation_record_too_large"
        );
        let document: RequestDocument = serde_json::from_slice(&bytes)
            .map_err(|_| anyhow!("input_preparation_decode_failed"))?;
        validate(&document)?;
        ensure!(
            document.task.name()? == name,
            "input_preparation_name_mismatch"
        );
        Ok(Self { document })
    }
}

pub(crate) async fn publish(
    config: &InputPreparationConfig,
    task: &ClaimedTask,
    worker_id: &str,
    binding: &SandboxBinding,
    admitted_code: Option<&AdmittedCodeRef>,
) -> Result<()> {
    let identity = PreparationTaskIdentity::from_task(task);
    let name = identity.name()?;
    let mut document = json!({"schemaVersion":SCHEMA,"task":identity,"workerId":worker_id,
        "binding":{"principalId":binding.principal_id,"principalHash":binding.principal_hash,
            "groupKey":binding.group_key,"profileKey":binding.profile_key,"profileId":binding.profile_id,
            "labRef":binding.lab_ref,"namespace":binding.namespace,"runtimeClassName":binding.runtime_class_name}});
    // Fixed recipes keep their exact prior notice bytes.
    if let Some(code) = admitted_code {
        document["admittedCode"] = json!(code);
    }
    let bytes =
        serde_json::to_vec(&document).map_err(|_| anyhow!("input_preparation_encode_failed"))?;
    ensure!(
        bytes.len() as u64 <= LIMIT,
        "input_preparation_record_too_large"
    );
    let checked: RequestDocument =
        serde_json::from_slice(&bytes).map_err(|_| anyhow!("input_preparation_encode_failed"))?;
    validate(&checked)?;
    let root = config.root.clone();
    let uid = config.writer_uid;
    tokio::task::spawn_blocking(move || publish_private(&root, &name, uid, &bytes))
        .await
        .map_err(|_| anyhow!("input_preparation_publish_failed"))?
        .map_err(|_| anyhow!("input_preparation_publish_failed"))
}

fn validate(document: &RequestDocument) -> Result<()> {
    ensure!(
        document.schema_version == SCHEMA,
        "input_preparation_schema_invalid"
    );
    document.task.name()?;
    ensure!(
        !document.worker_id.is_empty()
            && document.worker_id.len() <= 128
            && document
                .worker_id
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.')),
        "input_preparation_worker_invalid"
    );
    let binding = &document.binding;
    ensure!(
        binding.principal_id == document.task.principal_id
            && binding.group_key == document.task.group_key
            && binding.profile_key == document.task.profile_key,
        "input_preparation_binding_mismatch"
    );
    SandboxBindingCatalog::new(vec![binding.clone()])
        .map_err(|_| anyhow!("input_preparation_binding_invalid"))?;
    if let Some(code) = &document.admitted_code {
        code.validate()
            .map_err(|_| anyhow!("input_preparation_admitted_code_invalid"))?;
    }
    Ok(())
}
