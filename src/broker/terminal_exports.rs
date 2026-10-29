//! Read a trusted terminal export selection, never a model-chosen path or mount.
use super::{
    admission::{AdmittedSandbox, open_bounded_record},
    artifact_retainer::{
        ExportFile, ExportLimits, ExportRoots, FilesystemArtifactRetainer, validate_task_binding,
    },
    execution::TerminalArtifactRetainerFactory,
    export_client_staging::export_client_scope_exists,
    export_transport::{ExportClient, ExportScope, HttpsArtifactRetainer, RemoteExportConfig},
    model::ClaimedTask,
    sandbox::{
        ArtifactRetainer, RetainedArtifact, RuntimeCompletion, SandboxBinding, SandboxHandle,
    },
    terminal_request::{self, TerminalPreparationConfig},
};
use crate::output_inventory::{OutputInventory, OutputInventoryRequest};
use anyhow::{Result, anyhow, ensure};
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use std::{
    io::{ErrorKind, Read},
    path::{Component, Path, PathBuf},
    time::Duration,
};
use time::format_description::well_known::Rfc3339;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

const SCHEMA: &str = "sandbox.enrichedlab.org/terminal-export/v1";
const RECORD_LIMIT: u64 = 16 * 1024 * 1024;

/// Encodes the existing terminal-export/v1 record from a closed output request and
/// exact measured inventory. The caller still owns helper pinning, output-PVC
/// identity, writer detachment, and its current lease fence.
pub fn encode_terminal_export(
    task: &ClaimedTask,
    binding: &SandboxBinding,
    handle: &SandboxHandle,
    completion: &RuntimeCompletion,
    approved: &OutputInventoryRequest,
    inventory: &OutputInventory,
) -> Result<Vec<u8>> {
    approved.validate_inventory(inventory)?;
    validate_task_binding(task, binding, handle)
        .map_err(|_| anyhow!("terminal_export_binding_mismatch"))?;
    ensure!(
        task.principal_id == binding.principal_id
            && task.group_key == binding.group_key
            && task.profile_key == binding.profile_key
            && completion.observation().sandbox_uid == handle.sandbox_uid
            && completion.observation().sandbox_generation == handle.sandbox_generation,
        "terminal_export_binding_mismatch"
    );
    let observation = completion.observation();
    let record = TerminalExportRecord {
        schema_version: SCHEMA.into(),
        task_id: task.id,
        principal_id: task.principal_id,
        group_key: task.group_key.clone(),
        profile_key: task.profile_key.clone(),
        attempt: task.attempt,
        lease_generation: task.lease_generation,
        worker_id: handle.worker_id.clone(),
        namespace: handle.namespace.clone(),
        sandbox_name: handle.sandbox_name.clone(),
        sandbox_uid: handle.sandbox_uid.clone(),
        sandbox_generation: handle.sandbox_generation,
        pod_name: observation.pod_name.clone(),
        pod_uid: observation.pod_uid.clone(),
        runtime_class_name: observation.runtime_class_name.clone(),
        artifact_pvc_name: format!("artifacts-{}", handle.sandbox_name),
        exit_code: completion.exit_code(),
        finished_at: completion
            .finished_at()
            .format(&Rfc3339)
            .map_err(|_| anyhow!("terminal_export_record_time_invalid"))?,
        files: inventory
            .files
            .iter()
            .map(|file| SelectedFile {
                relative_path: file.relative_path.clone(),
                visibility: file.visibility.clone(),
                role: file.role.clone(),
                label: file.label.clone(),
                media_type: file.media_type.clone(),
                size_bytes: file.size_bytes,
                sha256: file.sha256.clone(),
            })
            .collect(),
    };
    record.validate(task, handle, completion, &handle.sandbox_name)?;
    let bytes =
        serde_json::to_vec(&record).map_err(|_| anyhow!("terminal_export_record_encode_failed"))?;
    ensure!(
        bytes.len() as u64 <= RECORD_LIMIT,
        "terminal_export_record_too_large"
    );
    validated_export_selection(&bytes, task, handle, completion)?;
    Ok(bytes)
}

#[derive(Clone, Debug, Deserialize)]
#[serde(untagged, deny_unknown_fields)]
pub enum TerminalExportConfig {
    Mounted {
        #[serde(rename = "recordRoot")]
        record_root: PathBuf,
        #[serde(rename = "writerUid")]
        writer_uid: u32,
        #[serde(rename = "mountedArtifactsRoot")]
        mounted_artifacts_root: PathBuf,
    },
    Https {
        https: RemoteExportConfig,
    },
    PreparedHttps {
        https: RemoteExportConfig,
        preparation: TerminalPreparationConfig,
    },
}

impl TerminalExportConfig {
    pub fn validate(&self) -> Result<()> {
        match self {
            Self::Mounted {
                record_root,
                mounted_artifacts_root,
                ..
            } => {
                for root in [record_root, mounted_artifacts_root] {
                    ensure!(absolute_directory(root), "terminal_export_root_invalid");
                }
                ensure!(
                    !overlap(record_root, mounted_artifacts_root),
                    "terminal_export_roots_overlap"
                );
                Ok(())
            }
            Self::Https { https } => https.validate(),
            Self::PreparedHttps { https, preparation } => {
                https.validate()?;
                preparation.validate(&https.credential_root)
            }
        }
    }

    fn validate_destination(&self, destination: &Path) -> Result<()> {
        ensure!(
            absolute_directory(destination),
            "terminal_export_root_invalid"
        );
        let roots = match self {
            Self::Mounted {
                record_root,
                mounted_artifacts_root,
                ..
            } => vec![record_root, mounted_artifacts_root],
            Self::Https { https } => vec![&https.credential_root],
            Self::PreparedHttps { https, preparation } => {
                vec![&https.credential_root, &preparation.root]
            }
        };
        ensure!(
            roots.iter().all(|root| !overlap(destination, root)),
            "terminal_export_roots_overlap"
        );
        Ok(())
    }
}

#[derive(Debug)]
pub enum TerminalArtifactRetainer {
    Mounted(FilesystemArtifactRetainer),
    Https(HttpsArtifactRetainer),
}

#[async_trait]
impl ArtifactRetainer for TerminalArtifactRetainer {
    async fn retain(
        &self,
        handle: &SandboxHandle,
        cancellation: &CancellationToken,
    ) -> Result<Vec<RetainedArtifact>> {
        match self {
            Self::Mounted(retainer) => retainer.retain(handle, cancellation).await,
            Self::Https(retainer) => retainer.retain(handle, cancellation).await,
        }
    }
    fn retained_root(&self) -> Option<&Path> {
        match self {
            Self::Mounted(retainer) => retainer.retained_root(),
            Self::Https(retainer) => retainer.retained_root(),
        }
    }
}

/// This configuration belongs to the worker deployment. The record producer and
/// its PVC mount/detachment authority remain a separate trusted service.
pub struct StagedTerminalRetainerFactory {
    config: TerminalExportConfig,
    task_id: Uuid,
    principal_id: Uuid,
    attempt: i32,
    lease_generation: i64,
    binding: SandboxBinding,
    run_id: String,
    durable_artifacts: PathBuf,
    limits: ExportLimits,
}

impl StagedTerminalRetainerFactory {
    pub fn new(
        config: TerminalExportConfig,
        task: &ClaimedTask,
        binding: &SandboxBinding,
        admitted: &AdmittedSandbox,
        durable_artifacts: PathBuf,
        limits: ExportLimits,
    ) -> Result<Self> {
        config.validate()?;
        config.validate_destination(&durable_artifacts)?;
        ensure!(
            task.principal_id == binding.principal_id
                && task.group_key == binding.group_key
                && task.profile_key == binding.profile_key
                && task.attempt > 0
                && task.lease_generation > 0
                && binding.namespace == admitted.authorization.namespace
                && binding.profile_id == admitted.authorization.profile_id
                && binding.principal_hash == admitted.authorization.principal_hash,
            "terminal_export_binding_mismatch"
        );
        let run_id = &admitted.authorization.run_id;
        ensure!(one_component(run_id), "terminal_export_run_invalid");
        ensure!(
            limits.max_files > 0
                && limits.max_files <= 100_000
                && limits.max_bytes > 0
                && limits.max_bytes <= i64::MAX as u64,
            "terminal_export_limits_invalid"
        );
        Ok(Self {
            config,
            task_id: task.id,
            principal_id: task.principal_id,
            attempt: task.attempt,
            lease_generation: task.lease_generation,
            binding: binding.clone(),
            run_id: run_id.clone(),
            durable_artifacts,
            limits,
        })
    }
}

#[async_trait]
impl TerminalArtifactRetainerFactory for StagedTerminalRetainerFactory {
    type Retainer = TerminalArtifactRetainer;

    async fn prepare_terminal_retainer(
        &self,
        task: &ClaimedTask,
        handle: &SandboxHandle,
        completion: &RuntimeCompletion,
        cancellation: &CancellationToken,
    ) -> Result<Self::Retainer> {
        ensure!(
            task.id == self.task_id
                && task.principal_id == self.principal_id
                && task.attempt == self.attempt
                && task.lease_generation == self.lease_generation
                && task.group_key == self.binding.group_key
                && task.profile_key == self.binding.profile_key
                && handle.task_id == task.id
                && handle.lease_generation == task.lease_generation
                && handle.namespace == self.binding.namespace
                && handle.sandbox_name == self.run_id
                && handle.lab_ref == self.binding.lab_ref
                && completion.observation().sandbox_uid == handle.sandbox_uid
                && completion.observation().sandbox_generation == handle.sandbox_generation,
            "terminal_export_binding_mismatch"
        );
        match &self.config {
            TerminalExportConfig::Mounted {
                record_root,
                writer_uid,
                mounted_artifacts_root,
            } => {
                let name = format!(
                    "{}-{}-{}.json",
                    task.id, task.attempt, task.lease_generation
                );
                let record = loop {
                    ensure!(!cancellation.is_cancelled(), "terminal_export_cancelled");
                    let root = record_root.clone();
                    let uid = *writer_uid;
                    let name = name.clone();
                    let record =
                        tokio::task::spawn_blocking(move || read_record(&root, uid, &name))
                            .await
                            .map_err(|_| anyhow!("terminal_export_reader_failed"))??;
                    ensure!(!cancellation.is_cancelled(), "terminal_export_cancelled");
                    if let Some(record) = record {
                        break record;
                    }
                    wait_for_export(cancellation).await?;
                };
                record.validate(task, handle, completion, &self.run_id)?;
                Ok(TerminalArtifactRetainer::Mounted(
                    FilesystemArtifactRetainer::for_task(
                        task,
                        &self.binding,
                        handle,
                        ExportRoots {
                            mounted_artifacts: mounted_artifacts_root.join(&self.run_id),
                            durable_artifacts: self.durable_artifacts.clone(),
                        },
                        record.files.into_iter().map(ExportFile::from).collect(),
                        self.limits,
                    )?,
                ))
            }
            TerminalExportConfig::Https { https }
            | TerminalExportConfig::PreparedHttps { https, .. } => {
                let preparation = match &self.config {
                    TerminalExportConfig::PreparedHttps { preparation, .. } => Some(preparation),
                    _ => None,
                };
                let prepare = async {
                    if let Some(preparation) = preparation {
                        terminal_request::publish(
                            preparation,
                            task,
                            &self.binding,
                            handle,
                            completion,
                            cancellation,
                        )
                        .await?;
                        let scope = ExportScope::new(task.id, task.attempt, task.lease_generation)?;
                        loop {
                            ensure!(!cancellation.is_cancelled(), "terminal_export_cancelled");
                            let remote = https.clone();
                            let scope = scope.clone();
                            let present = tokio::task::spawn_blocking(move || {
                                export_client_scope_exists(&remote, &scope)
                            })
                            .await
                            .map_err(|_| anyhow!("terminal_export_reader_failed"))??;
                            if present {
                                break;
                            }
                            wait_for_export(cancellation).await?;
                        }
                    }
                    let config = https.clone();
                    let task_owned = task.clone();
                    let namespace = self.binding.namespace.clone();
                    let client = tokio::task::spawn_blocking(move || {
                        ExportClient::for_task(&config, &task_owned, &namespace)
                    })
                    .await
                    .map_err(|_| anyhow!("terminal_export_reader_failed"))??;
                    let bytes = loop {
                        if let Some(bytes) = client.manifest(cancellation).await? {
                            break bytes;
                        }
                        wait_for_export(cancellation).await?;
                    };
                    let record: TerminalExportRecord = serde_json::from_slice(&bytes)
                        .map_err(|_| anyhow!("terminal_export_record_invalid"))?;
                    record.validate(task, handle, completion, &self.run_id)?;
                    Ok(TerminalArtifactRetainer::Https(
                        HttpsArtifactRetainer::for_task(
                            client,
                            &bytes,
                            task,
                            &self.binding,
                            handle,
                            self.durable_artifacts.clone(),
                            record.files.into_iter().map(ExportFile::from).collect(),
                            self.limits,
                        )?,
                    ))
                };
                if let Some(preparation) = preparation {
                    tokio::time::timeout(
                        Duration::from_secs(preparation.max_wait_seconds.into()),
                        prepare,
                    )
                    .await
                    .map_err(|_| anyhow!("terminal_preparation_deadline"))?
                } else {
                    prepare.await
                }
            }
        }
    }
}

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct TerminalExportRecord {
    schema_version: String,
    task_id: Uuid,
    principal_id: Uuid,
    group_key: String,
    profile_key: String,
    attempt: i32,
    lease_generation: i64,
    worker_id: String,
    namespace: String,
    sandbox_name: String,
    sandbox_uid: String,
    sandbox_generation: i64,
    pod_name: String,
    pod_uid: String,
    runtime_class_name: String,
    artifact_pvc_name: String,
    exit_code: i32,
    finished_at: String,
    files: Vec<SelectedFile>,
}

impl TerminalExportRecord {
    fn validate(
        &self,
        task: &ClaimedTask,
        handle: &SandboxHandle,
        completion: &RuntimeCompletion,
        run_id: &str,
    ) -> Result<()> {
        let observation = completion.observation();
        let finished_at = time::OffsetDateTime::parse(
            &self.finished_at,
            &time::format_description::well_known::Rfc3339,
        )
        .map_err(|_| anyhow!("terminal_export_record_invalid"))?;
        ensure!(
            self.schema_version == SCHEMA
                && self.task_id == task.id
                && self.principal_id == task.principal_id
                && self.group_key == task.group_key
                && self.profile_key == task.profile_key
                && self.attempt == task.attempt
                && self.lease_generation == task.lease_generation
                && self.worker_id == handle.worker_id
                && self.namespace == handle.namespace
                && self.sandbox_name == handle.sandbox_name
                && self.sandbox_uid == handle.sandbox_uid
                && self.sandbox_generation == handle.sandbox_generation
                && self.pod_name == observation.pod_name
                && self.pod_uid == observation.pod_uid
                && self.runtime_class_name == observation.runtime_class_name
                && self.artifact_pvc_name == format!("artifacts-{run_id}")
                && self.exit_code == completion.exit_code()
                && finished_at == completion.finished_at(),
            "terminal_export_record_mismatch"
        );
        Ok(())
    }
}

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct SelectedFile {
    relative_path: String,
    visibility: String,
    role: String,
    label: String,
    media_type: String,
    size_bytes: u64,
    sha256: String,
}

pub(crate) fn export_selection(bytes: &[u8]) -> Result<(ExportScope, Vec<ExportFile>)> {
    let record: TerminalExportRecord =
        serde_json::from_slice(bytes).map_err(|_| anyhow!("terminal_export_record_invalid"))?;
    ensure!(
        record.schema_version == SCHEMA,
        "terminal_export_record_invalid"
    );
    let scope = ExportScope::new(record.task_id, record.attempt, record.lease_generation)?;
    Ok((
        scope,
        record.files.into_iter().map(ExportFile::from).collect(),
    ))
}

/// The trusted publisher uses the same terminal identity contract as retention.
pub(crate) fn validated_export_selection(
    bytes: &[u8],
    task: &ClaimedTask,
    handle: &SandboxHandle,
    completion: &RuntimeCompletion,
) -> Result<Vec<ExportFile>> {
    ensure!(
        bytes.len() as u64 <= RECORD_LIMIT,
        "terminal_export_record_too_large"
    );
    let record: TerminalExportRecord =
        serde_json::from_slice(bytes).map_err(|_| anyhow!("terminal_export_record_invalid"))?;
    record.validate(task, handle, completion, &handle.sandbox_name)?;
    Ok(record.files.into_iter().map(ExportFile::from).collect())
}

impl From<SelectedFile> for ExportFile {
    fn from(value: SelectedFile) -> Self {
        Self {
            relative_path: value.relative_path,
            visibility: value.visibility,
            role: value.role,
            label: value.label,
            media_type: value.media_type,
            size_bytes: value.size_bytes,
            sha256: value.sha256,
        }
    }
}

fn read_record(root: &Path, writer_uid: u32, name: &str) -> Result<Option<TerminalExportRecord>> {
    let file = match open_bounded_record(root, name, writer_uid, RECORD_LIMIT) {
        Ok(file) => file,
        Err(error)
            if error.chain().any(|cause| {
                cause
                    .downcast_ref::<std::io::Error>()
                    .is_some_and(|error| error.kind() == ErrorKind::NotFound)
            }) =>
        {
            return Ok(None);
        }
        Err(_) => return Err(anyhow!("terminal_export_record_unavailable")),
    };
    let mut bytes = Vec::new();
    file.take(RECORD_LIMIT + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| anyhow!("terminal_export_record_unavailable"))?;
    ensure!(
        bytes.len() as u64 <= RECORD_LIMIT,
        "terminal_export_record_too_large"
    );
    serde_json::from_slice(&bytes)
        .map(Some)
        .map_err(|_| anyhow!("terminal_export_record_invalid"))
}

fn absolute_directory(path: &Path) -> bool {
    path.is_absolute()
        && path != Path::new("/")
        && path
            .components()
            .all(|part| matches!(part, Component::RootDir | Component::Normal(_)))
}
fn overlap(a: &Path, b: &Path) -> bool {
    a.starts_with(b) || b.starts_with(a)
}
fn one_component(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 253
        && value.bytes().all(|byte| {
            byte.is_ascii_lowercase() || byte.is_ascii_digit() || matches!(byte, b'-' | b'.')
        })
        && !matches!(value, "." | "..")
}

async fn wait_for_export(cancellation: &CancellationToken) -> Result<()> {
    tokio::select! {
        _ = cancellation.cancelled() => Err(anyhow!("terminal_export_cancelled")),
        _ = tokio::time::sleep(Duration::from_millis(250)) => Ok(()),
    }
}
