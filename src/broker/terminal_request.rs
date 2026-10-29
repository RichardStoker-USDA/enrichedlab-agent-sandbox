//! A terminal notice transfers observed identities to trusted output custody.
//! It grants no access and cannot mint a RuntimeCompletion: the custodian must
//! read the current task and independently observe its exact runtime again.
use super::{
    admission::open_bounded_record,
    admission_publication::publish_private,
    export_kubernetes::{valid_label, valid_uid},
    input_request::PreparationTaskIdentity,
    model::ClaimedTask,
    sandbox::{RuntimeCompletion, SandboxBinding, SandboxBindingCatalog, SandboxHandle},
};
use anyhow::{Result, anyhow, ensure};
use serde::Deserialize;
use serde_json::{Value, json};
use std::{
    fmt,
    io::Read,
    path::{Path, PathBuf},
};
use time::{OffsetDateTime, format_description::well_known::Rfc3339};
use tokio_util::sync::CancellationToken;

const SCHEMA: &str = "sandbox.enrichedlab.org/terminal-preparation-request/v1";
const LIMIT: u64 = 16 * 1024;

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct TerminalPreparationConfig {
    pub root: PathBuf,
    pub writer_uid: u32,
    pub max_wait_seconds: u32,
}
impl TerminalPreparationConfig {
    pub(crate) fn validate(&self, credentials: &Path) -> Result<()> {
        ensure!(
            super::publication_journal::valid_private_root(&self.root)
                && !self.root.starts_with(credentials)
                && !credentials.starts_with(&self.root)
                && (1..=3600).contains(&self.max_wait_seconds),
            "terminal_preparation_config_invalid"
        );
        Ok(())
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct HandleDocument {
    worker_id: String,
    sandbox_name: String,
    sandbox_uid: String,
    sandbox_generation: i64,
}
#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct TerminalCompletionIdentity {
    pub pod_name: String,
    pub pod_uid: String,
    pub runtime_class_name: String,
    pub exit_code: i32,
    pub finished_at: String,
}
impl TerminalCompletionIdentity {
    pub fn matches(&self, completion: &RuntimeCompletion) -> bool {
        let observed = completion.observation();
        self.pod_name == observed.pod_name
            && self.pod_uid == observed.pod_uid
            && self.runtime_class_name == observed.runtime_class_name
            && self.exit_code == completion.exit_code()
            && OffsetDateTime::parse(&self.finished_at, &Rfc3339)
                .is_ok_and(|t| t == completion.finished_at())
    }
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Document {
    schema_version: String,
    task: PreparationTaskIdentity,
    binding: SandboxBinding,
    handle: HandleDocument,
    completion: TerminalCompletionIdentity,
}

pub struct TerminalPreparationRequest {
    task: PreparationTaskIdentity,
    binding: SandboxBinding,
    handle: SandboxHandle,
    completion: TerminalCompletionIdentity,
}
impl fmt::Debug for TerminalPreparationRequest {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("TerminalPreparationRequest")
            .field("task_id", &self.task.task_id)
            .field("attempt", &self.task.attempt)
            .field("lease_generation", &self.task.lease_generation)
            .finish_non_exhaustive()
    }
}
impl TerminalPreparationRequest {
    pub fn task(&self) -> &PreparationTaskIdentity {
        &self.task
    }
    pub fn binding(&self) -> &SandboxBinding {
        &self.binding
    }
    pub fn handle(&self) -> &SandboxHandle {
        &self.handle
    }
    pub fn completion(&self) -> &TerminalCompletionIdentity {
        &self.completion
    }

    pub fn load(root: &Path, name: &str, writer_uid: u32) -> Result<Self> {
        ensure!(
            !name.is_empty() && !name.contains(['/', '\\', '\0']) && name != "." && name != "..",
            "terminal_preparation_name_invalid"
        );
        let file = open_bounded_record(root, name, writer_uid, LIMIT)
            .map_err(|_| anyhow!("terminal_preparation_record_unavailable"))?;
        let mut bytes = Vec::new();
        file.take(LIMIT + 1)
            .read_to_end(&mut bytes)
            .map_err(|_| anyhow!("terminal_preparation_read_failed"))?;
        ensure!(
            bytes.len() as u64 <= LIMIT,
            "terminal_preparation_size_exceeded"
        );
        let request = Self::decode(&bytes)?;
        ensure!(name == request.name(), "terminal_preparation_name_mismatch");
        Ok(request)
    }
    fn name(&self) -> String {
        format!(
            "{}-{}-{}.json",
            self.task.task_id, self.task.attempt, self.task.lease_generation
        )
    }
    fn decode(bytes: &[u8]) -> Result<Self> {
        let d: Document = serde_json::from_slice(bytes)
            .map_err(|_| anyhow!("terminal_preparation_record_invalid"))?;
        ensure!(
            d.schema_version == SCHEMA
                && d.task.attempt > 0
                && d.task.lease_generation > 0
                && d.task.max_run_seconds > 0
                && d.task.principal_id == d.binding.principal_id
                && d.task.group_key == d.binding.group_key
                && d.task.profile_key == d.binding.profile_key,
            "terminal_preparation_binding_invalid"
        );
        SandboxBindingCatalog::new(vec![d.binding.clone()])
            .map_err(|_| anyhow!("terminal_preparation_binding_invalid"))?;
        ensure!(
            !d.handle.worker_id.is_empty()
                && d.handle.worker_id.len() <= 128
                && d.handle
                    .worker_id
                    .bytes()
                    .all(|v| v.is_ascii_alphanumeric() || matches!(v, b'-' | b'_' | b'.'))
                && valid_label(&d.handle.sandbox_name)
                && valid_uid(&d.handle.sandbox_uid)
                && d.handle.sandbox_generation > 0
                && valid_label(&d.completion.pod_name)
                && valid_uid(&d.completion.pod_uid)
                && d.completion.runtime_class_name == d.binding.runtime_class_name
                && OffsetDateTime::parse(&d.completion.finished_at, &Rfc3339).is_ok(),
            "terminal_preparation_runtime_invalid"
        );
        let handle = SandboxHandle {
            task_id: d.task.task_id,
            worker_id: d.handle.worker_id,
            lease_generation: d.task.lease_generation,
            namespace: d.binding.namespace.clone(),
            sandbox_name: d.handle.sandbox_name,
            sandbox_uid: d.handle.sandbox_uid,
            sandbox_generation: d.handle.sandbox_generation,
            lab_ref: d.binding.lab_ref.clone(),
        };
        Ok(Self {
            task: d.task,
            binding: d.binding,
            handle,
            completion: d.completion,
        })
    }
}

pub(crate) async fn publish(
    config: &TerminalPreparationConfig,
    task: &ClaimedTask,
    binding: &SandboxBinding,
    handle: &SandboxHandle,
    completion: &RuntimeCompletion,
    cancellation: &CancellationToken,
) -> Result<()> {
    ensure!(
        !cancellation.is_cancelled(),
        "terminal_preparation_cancelled"
    );
    ensure!(
        handle.task_id == task.id
            && handle.lease_generation == task.lease_generation
            && handle.namespace == binding.namespace
            && handle.lab_ref == binding.lab_ref
            && completion.observation().sandbox_uid == handle.sandbox_uid
            && completion.observation().sandbox_generation == handle.sandbox_generation,
        "terminal_preparation_runtime_mismatch"
    );
    let o = completion.observation();
    let value: Value = json!({"schemaVersion":SCHEMA,"task":PreparationTaskIdentity::from_task(task),
        "binding":{"principalId":binding.principal_id,"principalHash":binding.principal_hash,
            "groupKey":binding.group_key,"profileKey":binding.profile_key,"profileId":binding.profile_id,
            "labRef":binding.lab_ref,"namespace":binding.namespace,"runtimeClassName":binding.runtime_class_name},
        "handle":{"workerId":handle.worker_id,"sandboxName":handle.sandbox_name,
            "sandboxUid":handle.sandbox_uid,"sandboxGeneration":handle.sandbox_generation},
        "completion":{"podName":o.pod_name,"podUid":o.pod_uid,"runtimeClassName":o.runtime_class_name,
            "exitCode":completion.exit_code(),"finishedAt":completion.finished_at().format(&Rfc3339)
                .map_err(|_|anyhow!("terminal_preparation_time_invalid"))?}});
    let bytes =
        serde_json::to_vec(&value).map_err(|_| anyhow!("terminal_preparation_encode_failed"))?;
    ensure!(
        bytes.len() as u64 <= LIMIT,
        "terminal_preparation_size_exceeded"
    );
    let checked = TerminalPreparationRequest::decode(&bytes)?;
    let name = checked.name();
    let root = config.root.clone();
    let uid = config.writer_uid;
    tokio::task::spawn_blocking(move || publish_private(&root, &name, uid, &bytes))
        .await
        .map_err(|_| anyhow!("terminal_preparation_publish_failed"))?
        .map_err(|_| anyhow!("terminal_preparation_publish_failed"))?;
    ensure!(
        !cancellation.is_cancelled(),
        "terminal_preparation_cancelled"
    );
    Ok(())
}
