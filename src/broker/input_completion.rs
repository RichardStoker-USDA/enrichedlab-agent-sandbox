//! Accept only the exact completed copy-and-seal Job for a trusted direct run.
//!
//! This observer does not create, delete, patch, seal, or detach Kubernetes
//! resources. It turns a fixed Job/Pod observation and the custodian's one-line
//! stdout into an opaque receipt for a later trusted dispatcher.

use crate::custody::{CopySealRequest, InputManifest, SealAttestation, canonical_document_digest};
use anyhow::{Result, anyhow, ensure};
use async_trait::async_trait;
use serde::Serialize;
use serde_json::Value;
use std::fmt;
use time::{OffsetDateTime, format_description::well_known::Rfc3339};

const MAX_HELPER_STDOUT: usize = 1024 * 1024;
const COPY_SEAL_SCHEMA: &str = "sandbox.enrichedlab.org/input-staging/v1";
const COPY_SEAL_MODE: &str = "checksummed-copy";

/// Fixed operator-derived expectation for one copy-and-seal Job. The caller
/// obtains this from the trusted renderer and create receipt, never from a task
/// or model request.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InputCopyJobExpectation {
    pub namespace: String,
    pub job_name: String,
    pub job_uid: String,
    pub run_id: String,
    pub principal_hash: String,
    pub runtime_class_name: String,
    pub helper: InputCopyContainerExpectation,
    pub volumes: Vec<InputCopyVolumeExpectation>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InputCopyContainerExpectation {
    pub name: String,
    pub image: String,
    pub command: Vec<String>,
    pub args: Vec<String>,
}

/// Each role is fixed by the renderer. The observer validates the associated
/// PVC reference and mount; it does not infer a role from a user-provided path.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InputCopyVolumeExpectation {
    pub role: String,
    pub name: String,
    pub mount_path: String,
    pub sub_path: Option<String>,
    pub read_only: bool,
    pub source: InputCopyVolumeSource,
}

/// Exact volume class admitted by the fixed input-copy renderer. The observer
/// rejects a substituted PVC, Secret, or writable mount instead of accepting a
/// same-named volume of another class.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum InputCopyVolumeSource {
    PersistentVolumeClaim { claim_name: String },
    EmptyDirMemory,
    ProjectedSecret { secret_name: String },
}

/// Minimal Kubernetes boundary needed by the completion observer. The site
/// adapter must select the exact Job-controlled Pod and return bounded stdout
/// only for the fixed helper container.
#[async_trait]
pub trait InputCopyObservationTransport: Send + Sync {
    async fn get_job(&self, namespace: &str, name: &str) -> Result<Value>;
    async fn get_controlled_pod(&self, namespace: &str, job_name: &str) -> Result<Option<Value>>;
    async fn helper_stdout(
        &self,
        namespace: &str,
        pod_name: &str,
        container: &str,
    ) -> Result<Vec<u8>>;
}

/// Safe metadata for the later dispatcher and custody journal.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct InputCompletionMetadata {
    pub job_uid: String,
    pub pod_uid: String,
    pub pod_name: String,
    pub sealed_at: OffsetDateTime,
}

/// Opaque accepted result. The original helper receipt remains available only
/// by borrowed bytes for deterministic private custody persistence.
pub struct ObservedInputSeal {
    metadata: InputCompletionMetadata,
    attestation: SealAttestation,
    original_receipt: Vec<u8>,
}

impl fmt::Debug for ObservedInputSeal {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ObservedInputSeal")
            .field("metadata", &self.metadata)
            .field("staging_id", &self.attestation.staging_id)
            .field("run_id", &self.attestation.run_id)
            .finish_non_exhaustive()
    }
}

impl ObservedInputSeal {
    pub fn metadata(&self) -> &InputCompletionMetadata {
        &self.metadata
    }

    pub fn job_uid(&self) -> &str {
        &self.metadata.job_uid
    }

    pub fn pod_uid(&self) -> &str {
        &self.metadata.pod_uid
    }

    pub fn attestation(&self) -> &SealAttestation {
        &self.attestation
    }

    pub fn original_receipt(&self) -> &[u8] {
        &self.original_receipt
    }
}

/// Observe a completed copy Job and accept the original custodian attestation.
/// All public errors are fixed classes; helper output never enters an error or
/// log message.
pub async fn observe_input_seal<T: InputCopyObservationTransport>(
    transport: &T,
    expected: &InputCopyJobExpectation,
    request: &CopySealRequest,
) -> Result<ObservedInputSeal> {
    validate_expectation(expected, request)?;
    let job = transport
        .get_job(&expected.namespace, &expected.job_name)
        .await
        .map_err(|_| anyhow!("input_copy_job_unavailable"))?;
    validate_job(&job, expected)?;
    let pod = transport
        .get_controlled_pod(&expected.namespace, &expected.job_name)
        .await
        .map_err(|_| anyhow!("input_copy_pod_unavailable"))?
        .ok_or_else(|| anyhow!("input_copy_pod_absent"))?;
    let (pod_name, pod_uid) = validate_pod(&pod, expected)?;
    let stdout = transport
        .helper_stdout(&expected.namespace, &pod_name, &expected.helper.name)
        .await
        .map_err(|_| anyhow!("input_copy_receipt_unavailable"))?;
    let attestation = parse_original_receipt(&stdout)?;
    let sealed_at = validate_attestation(&attestation, request)?;
    Ok(ObservedInputSeal {
        metadata: InputCompletionMetadata {
            job_uid: expected.job_uid.clone(),
            pod_uid,
            pod_name,
            sealed_at,
        },
        attestation,
        original_receipt: stdout,
    })
}

fn validate_expectation(
    expected: &InputCopyJobExpectation,
    request: &CopySealRequest,
) -> Result<()> {
    ensure!(
        dns_label(&expected.namespace)
            && dns_label(&expected.job_name)
            && valid_uid(&expected.job_uid)
            && dns_label(&expected.run_id)
            && valid_hash(&expected.principal_hash)
            && dns_label(&expected.runtime_class_name)
            && expected.run_id == request.run_id
            && expected.principal_hash == request.principal_hash
            && expected.namespace == request.namespace
            && request.schema_version == COPY_SEAL_SCHEMA
            && sha256_digest(&request.input_digest)
            && expected.helper.name == "custodian"
            && pinned_image(&expected.helper.image)
            && !expected.helper.command.is_empty()
            && !expected.volumes.is_empty(),
        "input_copy_expectation_invalid"
    );
    ensure!(
        expected.volumes.iter().all(valid_volume_expectation),
        "input_copy_volume_expectation_invalid"
    );
    let mut names = expected
        .volumes
        .iter()
        .map(|volume| volume.name.as_str())
        .collect::<Vec<_>>();
    names.sort_unstable();
    names.dedup();
    ensure!(
        names.len() == expected.volumes.len(),
        "input_copy_volume_expectation_invalid"
    );
    let mut roles = expected
        .volumes
        .iter()
        .map(|volume| volume.role.as_str())
        .collect::<Vec<_>>();
    roles.sort_unstable();
    ensure!(
        roles.as_slice() == ["custody", "destination", "request", "source"],
        "input_copy_volume_expectation_invalid"
    );
    ensure!(
        expected.volumes.iter().any(|volume| {
            volume.role == "destination"
                && matches!(
                    &volume.source,
                    InputCopyVolumeSource::PersistentVolumeClaim { claim_name }
                        if claim_name == &request.pvc_name
                )
                && !volume.read_only
        }),
        "input_copy_input_pvc_mismatch"
    );
    let manifest = expected_manifest(request)?;
    ensure!(
        canonical_document_digest(&manifest)? == request.input_digest,
        "input_copy_request_manifest_mismatch"
    );
    Ok(())
}

fn validate_job(job: &Value, expected: &InputCopyJobExpectation) -> Result<()> {
    positive_i64(job, "/metadata/generation")?;
    ensure!(
        text(job, "/metadata/name")? == expected.job_name
            && text(job, "/metadata/namespace")? == expected.namespace
            && text(job, "/metadata/uid")? == expected.job_uid,
        "input_copy_job_identity_or_completion_mismatch"
    );
    validate_terminal_conditions(job)?;
    ensure!(
        job.pointer("/spec/completions").and_then(Value::as_i64) == Some(1)
            && job.pointer("/spec/parallelism").and_then(Value::as_i64) == Some(1)
            && job.pointer("/spec/backoffLimit").and_then(Value::as_i64) == Some(0)
            && nonnegative(job, "/status/succeeded")? == 1
            && nonnegative(job, "/status/active")? == 0
            && nonnegative(job, "/status/failed")? == 0,
        "input_copy_job_terminal_state_invalid"
    );
    Ok(())
}

fn validate_pod(pod: &Value, expected: &InputCopyJobExpectation) -> Result<(String, String)> {
    let pod_name = text(pod, "/metadata/name")?.to_owned();
    let pod_uid = text(pod, "/metadata/uid")?.to_owned();
    ensure!(
        text(pod, "/metadata/namespace")? == expected.namespace
            && valid_uid(&pod_uid)
            && owner_is_job(pod, &expected.job_uid)
            && pod.pointer("/status/phase").and_then(Value::as_str) == Some("Succeeded")
            && pod.pointer("/spec/restartPolicy").and_then(Value::as_str) == Some("Never")
            && pod
                .pointer("/spec/runtimeClassName")
                .and_then(Value::as_str)
                == Some(expected.runtime_class_name.as_str())
            && pod
                .pointer("/spec/automountServiceAccountToken")
                .and_then(Value::as_bool)
                == Some(false)
            && pod
                .pointer("/spec/enableServiceLinks")
                .and_then(Value::as_bool)
                == Some(false)
            && fixed_security_context(pod.pointer("/spec/securityContext")),
        "input_copy_pod_identity_or_runtime_mismatch"
    );
    ensure!(
        empty_array_or_absent(pod, "/spec/initContainers")
            && empty_array_or_absent(pod, "/spec/ephemeralContainers")
            && empty_array_or_absent(pod, "/status/initContainerStatuses")
            && empty_array_or_absent(pod, "/status/ephemeralContainerStatuses"),
        "input_copy_pod_unaccounted_containers"
    );
    validate_helper_container(pod, expected)?;
    Ok((pod_name, pod_uid))
}

fn validate_helper_container(pod: &Value, expected: &InputCopyJobExpectation) -> Result<()> {
    let containers = pod
        .pointer("/spec/containers")
        .and_then(Value::as_array)
        .filter(|items| items.len() == 1)
        .ok_or_else(|| anyhow!("input_copy_pod_unaccounted_containers"))?;
    let container = &containers[0];
    ensure!(
        container.get("name").and_then(Value::as_str) == Some(expected.helper.name.as_str())
            && container.get("image").and_then(Value::as_str)
                == Some(expected.helper.image.as_str())
            && string_array(container, "command")? == expected.helper.command
            && string_array(container, "args")? == expected.helper.args
            && fixed_security_context(container.get("securityContext")),
        "input_copy_helper_contract_mismatch"
    );
    validate_volumes(pod, container, &expected.volumes)?;
    let statuses = pod
        .pointer("/status/containerStatuses")
        .and_then(Value::as_array)
        .filter(|items| items.len() == 1)
        .ok_or_else(|| anyhow!("input_copy_pod_unaccounted_containers"))?;
    let status = &statuses[0];
    ensure!(
        status.get("name").and_then(Value::as_str) == Some(expected.helper.name.as_str())
            && status.get("restartCount").and_then(Value::as_i64) == Some(0),
        "input_copy_helper_status_mismatch"
    );
    let state = status
        .get("state")
        .and_then(Value::as_object)
        .filter(|state| state.len() == 1 && state.contains_key("terminated"))
        .ok_or_else(|| anyhow!("input_copy_helper_not_terminated"))?;
    let terminated = &state["terminated"];
    ensure!(
        terminated.get("exitCode").and_then(Value::as_i64) == Some(0)
            && positive_time(terminated.get("finishedAt").and_then(Value::as_str)).is_ok(),
        "input_copy_helper_exit_invalid"
    );
    Ok(())
}

fn fixed_security_context(value: Option<&Value>) -> bool {
    value.is_some_and(|context| {
        context.get("runAsNonRoot").and_then(Value::as_bool) == Some(true)
            && context.get("runAsUser").and_then(Value::as_i64) == Some(65532)
            && context.get("runAsGroup").and_then(Value::as_i64) == Some(65532)
            && context
                .get("allowPrivilegeEscalation")
                .and_then(Value::as_bool)
                .is_none_or(|value| !value)
    })
}

fn validate_volumes(
    pod: &Value,
    container: &Value,
    expected: &[InputCopyVolumeExpectation],
) -> Result<()> {
    let volumes = pod
        .pointer("/spec/volumes")
        .and_then(Value::as_array)
        .filter(|items| items.len() == expected.len())
        .ok_or_else(|| anyhow!("input_copy_volume_contract_mismatch"))?;
    let mounts = container
        .get("volumeMounts")
        .and_then(Value::as_array)
        .filter(|items| items.len() == expected.len())
        .ok_or_else(|| anyhow!("input_copy_volume_contract_mismatch"))?;
    for requirement in expected {
        let volume = volumes
            .iter()
            .find(|item| {
                item.get("name").and_then(Value::as_str) == Some(requirement.name.as_str())
            })
            .ok_or_else(|| anyhow!("input_copy_volume_contract_mismatch"))?;
        let mount = mounts
            .iter()
            .find(|item| {
                item.get("name").and_then(Value::as_str) == Some(requirement.name.as_str())
            })
            .ok_or_else(|| anyhow!("input_copy_volume_contract_mismatch"))?;
        ensure!(
            volume_matches(volume, &requirement.source, requirement.read_only)
                && mount.get("mountPath").and_then(Value::as_str)
                    == Some(requirement.mount_path.as_str())
                && mount
                    .get("readOnly")
                    .and_then(Value::as_bool)
                    .unwrap_or(false)
                    == requirement.read_only
                && sub_path_matches(mount, requirement.sub_path.as_deref()),
            "input_copy_volume_contract_mismatch"
        );
    }
    Ok(())
}

fn volume_matches(volume: &Value, expected: &InputCopyVolumeSource, read_only: bool) -> bool {
    match expected {
        InputCopyVolumeSource::PersistentVolumeClaim { claim_name } => {
            volume
                .pointer("/persistentVolumeClaim/claimName")
                .and_then(Value::as_str)
                == Some(claim_name)
                && volume
                    .pointer("/persistentVolumeClaim/readOnly")
                    .and_then(Value::as_bool)
                    .unwrap_or(false)
                    == read_only
        }
        InputCopyVolumeSource::EmptyDirMemory => {
            volume.pointer("/emptyDir/medium").and_then(Value::as_str) == Some("Memory")
        }
        InputCopyVolumeSource::ProjectedSecret { secret_name } => volume
            .pointer("/projected/sources")
            .and_then(Value::as_array)
            .is_some_and(|sources| {
                sources.len() == 1
                    && sources[0].pointer("/secret/name").and_then(Value::as_str)
                        == Some(secret_name)
                    && sources[0]
                        .pointer("/secret/optional")
                        .and_then(Value::as_bool)
                        .is_none_or(|value| !value)
            }),
    }
}

fn parse_original_receipt(bytes: &[u8]) -> Result<SealAttestation> {
    ensure!(
        !bytes.is_empty()
            && bytes.len() <= MAX_HELPER_STDOUT
            && bytes.ends_with(b"\n")
            && bytes[..bytes.len() - 1]
                .iter()
                .all(|byte| *byte != b'\n' && *byte != b'\r'),
        "input_copy_receipt_invalid"
    );
    serde_json::from_slice(&bytes[..bytes.len() - 1])
        .map_err(|_| anyhow!("input_copy_receipt_invalid"))
}

fn validate_attestation(
    attestation: &SealAttestation,
    request: &CopySealRequest,
) -> Result<OffsetDateTime> {
    let manifest = expected_manifest(request)?;
    ensure!(
        attestation.schema_version == COPY_SEAL_SCHEMA
            && attestation.materialization_mode == COPY_SEAL_MODE
            && attestation.staging_id == request.staging_id
            && attestation.run_id == request.run_id
            && attestation.tenant_id == request.tenant_id
            && attestation.namespace == request.namespace
            && attestation.principal_hash == request.principal_hash
            && attestation.input_ref == request.input_ref
            && attestation.input_digest == request.input_digest
            && attestation.pvc_name == request.pvc_name
            && attestation.manifest_ref == request.manifest_ref
            && attestation.manifest_digest == request.input_digest
            && attestation.attestation_ref == request.attestation_ref
            && canonical_document_digest(&manifest)? == request.input_digest,
        "input_copy_attestation_binding_mismatch"
    );
    let mut unsigned = attestation.clone();
    unsigned.attestation_digest.clear();
    ensure!(
        canonical_document_digest(&unsigned)? == attestation.attestation_digest,
        "input_copy_attestation_digest_mismatch"
    );
    positive_time(Some(&attestation.sealed_at)).map_err(|_| anyhow!("input_copy_sealed_at_invalid"))
}

pub(crate) fn expected_manifest(request: &CopySealRequest) -> Result<InputManifest> {
    let mut files = request.files.clone();
    files.sort_by(|left, right| left.path.cmp(&right.path));
    let total_bytes = files.iter().try_fold(0_u64, |total, file| {
        total
            .checked_add(file.size_bytes)
            .ok_or_else(|| anyhow!("input_copy_manifest_invalid"))
    })?;
    Ok(InputManifest {
        schema_version: COPY_SEAL_SCHEMA.into(),
        staging_id: request.staging_id.clone(),
        run_id: request.run_id.clone(),
        files,
        total_bytes,
    })
}

fn validate_terminal_conditions(job: &Value) -> Result<()> {
    let conditions = job
        .pointer("/status/conditions")
        .and_then(Value::as_array)
        .ok_or_else(|| anyhow!("input_copy_job_completion_missing"))?;
    let complete = conditions.iter().any(|condition| {
        condition.get("type").and_then(Value::as_str) == Some("Complete")
            && condition.get("status").and_then(Value::as_str) == Some("True")
    });
    let conflicting_terminal = conditions.iter().any(|condition| {
        matches!(
            condition.get("type").and_then(Value::as_str),
            Some("Failed") | Some("FailureTarget")
        ) && condition.get("status").and_then(Value::as_str) == Some("True")
    });
    ensure!(
        complete && !conflicting_terminal,
        "input_copy_job_completion_invalid"
    );
    Ok(())
}

fn owner_is_job(pod: &Value, job_uid: &str) -> bool {
    pod.pointer("/metadata/ownerReferences")
        .and_then(Value::as_array)
        .is_some_and(|owners| {
            owners.iter().any(|owner| {
                owner.get("apiVersion").and_then(Value::as_str) == Some("batch/v1")
                    && owner.get("kind").and_then(Value::as_str) == Some("Job")
                    && owner.get("uid").and_then(Value::as_str) == Some(job_uid)
                    && owner.get("controller").and_then(Value::as_bool) == Some(true)
            })
        })
}

fn string_array(value: &Value, field: &str) -> Result<Vec<String>> {
    value
        .get(field)
        .and_then(Value::as_array)
        .ok_or_else(|| anyhow!("input_copy_helper_contract_mismatch"))?
        .iter()
        .map(|part| {
            part.as_str()
                .map(str::to_owned)
                .ok_or_else(|| anyhow!("input_copy_helper_contract_mismatch"))
        })
        .collect()
}

fn text<'a>(value: &'a Value, path: &str) -> Result<&'a str> {
    value
        .pointer(path)
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow!("input_copy_observation_invalid"))
}

fn positive_i64(value: &Value, path: &str) -> Result<i64> {
    value
        .pointer(path)
        .and_then(Value::as_i64)
        .filter(|number| *number > 0)
        .ok_or_else(|| anyhow!("input_copy_observation_invalid"))
}

fn nonnegative(value: &Value, path: &str) -> Result<i64> {
    let number = value.pointer(path).and_then(Value::as_i64).unwrap_or(0);
    ensure!(number >= 0, "input_copy_observation_invalid");
    Ok(number)
}

fn empty_array_or_absent(value: &Value, path: &str) -> bool {
    value
        .pointer(path)
        .is_none_or(|items| items.as_array().is_some_and(Vec::is_empty))
}

fn positive_time(value: Option<&str>) -> Result<OffsetDateTime> {
    value
        .and_then(|text| OffsetDateTime::parse(text, &Rfc3339).ok())
        .filter(|time| *time > OffsetDateTime::UNIX_EPOCH)
        .ok_or_else(|| anyhow!("input_copy_time_invalid"))
}

fn valid_volume_expectation(value: &InputCopyVolumeExpectation) -> bool {
    dns_label(&value.role)
        && dns_label(&value.name)
        && value.mount_path.starts_with('/')
        && value.mount_path.len() <= 255
        && value.sub_path.as_deref().is_none_or(valid_relative_path)
        && match &value.source {
            InputCopyVolumeSource::PersistentVolumeClaim { claim_name } => dns_label(claim_name),
            InputCopyVolumeSource::EmptyDirMemory => true,
            InputCopyVolumeSource::ProjectedSecret { secret_name } => dns_label(secret_name),
        }
}

fn sub_path_matches(mount: &Value, expected: Option<&str>) -> bool {
    match expected {
        Some(path) => mount.get("subPath").and_then(Value::as_str) == Some(path),
        None => mount.get("subPath").is_none(),
    }
}

fn valid_relative_path(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 255
        && !value.starts_with('/')
        && value.split('/').all(|part| {
            !part.is_empty()
                && part != "."
                && part != ".."
                && part
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'_')
        })
}

fn dns_label(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 63
        && value.starts_with(|character: char| {
            character.is_ascii_lowercase() || character.is_ascii_digit()
        })
        && value.ends_with(|character: char| {
            character.is_ascii_lowercase() || character.is_ascii_digit()
        })
        && value
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
}

fn valid_hash(value: &str) -> bool {
    value.len() == 32 && value.bytes().all(|byte| byte.is_ascii_hexdigit())
}

fn sha256_digest(value: &str) -> bool {
    value.strip_prefix("sha256:").is_some_and(|digest| {
        digest.len() == 64 && digest.bytes().all(|byte| byte.is_ascii_hexdigit())
    })
}

fn valid_uid(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 128
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
}

fn pinned_image(value: &str) -> bool {
    value.len() <= 512
        && value
            .rsplit_once("@sha256:")
            .is_some_and(|(reference, digest)| {
                !reference.is_empty()
                    && digest.len() == 64
                    && digest
                        .bytes()
                        .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
            })
        && !value.chars().any(char::is_whitespace)
}
