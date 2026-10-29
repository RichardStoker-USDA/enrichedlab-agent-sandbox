//! Render a restricted, one-shot input-copy Job from an already authorized
//! custody request. This module returns Kubernetes object specifications but
//! never creates them or accepts task-selected files, commands, roots, PVCs,
//! or Secret contents.

use crate::{
    broker::{model::ClaimedTask, sandbox::SandboxBinding},
    custody::CopySealRequest,
};
use anyhow::{Result, ensure};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use time::{OffsetDateTime, format_description::well_known::Rfc3339};
use uuid::Uuid;

pub const INPUT_COPY_JOB_PREFIX: &str = "input-copy-";
pub const CUSTODIAN_CONTAINER_NAME: &str = "custodian";
pub const CUSTODIAN_COMMAND: &str = "/usr/local/bin/agent-sandbox-custodian";
const CONTRACT: &str = "sandbox.enrichedlab.org/input-staging/v1";
const UID: u32 = 65_532;
const SECRET_SAFE_BYTES: usize = 768 * 1024;

/// Closed operator-owned access modes for the two newly created custody PVCs.
/// This selection does not apply to shared control-plane or notice storage.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum CustodyPvcAccessMode {
    /// Backward-compatible Kubernetes ReadWriteOnce behavior.
    #[default]
    ReadWriteOnce,
    /// Kubernetes ReadWriteOncePod for a site-qualified isolated custody class.
    ReadWriteOncePod,
}

impl CustodyPvcAccessMode {
    fn kubernetes_access_mode(self) -> &'static str {
        match self {
            Self::ReadWriteOnce => "ReadWriteOnce",
            Self::ReadWriteOncePod => "ReadWriteOncePod",
        }
    }
}

/// Operator-owned limits and image references for the fixed custodian Job.
/// This type is not populated from a task or model request.
#[derive(Clone, Debug, serde::Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct InputWorkloadConfig {
    #[serde(default)]
    pub site: super::workload_site::WorkloadSiteConfig,
    pub runtime_class_name: String,
    pub image: String,
    pub pool_selector: BTreeMap<String, String>,
    pub cpu_request_millis: u32,
    pub cpu_limit_millis: u32,
    pub memory_request_mib: u32,
    pub memory_limit_mib: u32,
    pub active_deadline_seconds: u32,
    pub custody_memory_mib: u32,
    pub input_storage_class_name: String,
    pub artifact_storage_class_name: String,
    pub input_storage_mib: u32,
    pub artifact_storage_mib: u32,
    /// Access mode for only the new per-run input and artifact PVCs. Missing
    /// configuration keeps the accepted ReadWriteOnce behavior.
    #[serde(default)]
    pub custody_pvc_access_mode: CustodyPvcAccessMode,
    pub max_request_bytes: usize,
}

impl InputWorkloadConfig {
    pub fn validate(&self) -> Result<()> {
        self.site.validate()?;
        ensure!(
            dns_label(&self.runtime_class_name),
            "input_copy_runtime_invalid"
        );
        ensure!(pinned_image(&self.image), "input_copy_image_unpinned");
        ensure!(
            !self.pool_selector.is_empty()
                && self
                    .pool_selector
                    .iter()
                    .all(|(key, value)| label_key(key) && label_value(value)),
            "input_copy_pool_selector_invalid"
        );
        ensure!(
            self.cpu_request_millis > 0
                && self.cpu_request_millis <= self.cpu_limit_millis
                && self.memory_request_mib > 0
                && self.memory_request_mib <= self.memory_limit_mib
                && self.custody_memory_mib > 0
                && self.custody_memory_mib <= self.memory_limit_mib,
            "input_copy_resources_invalid"
        );
        ensure!(
            self.active_deadline_seconds > 0 && self.active_deadline_seconds <= 86_400,
            "input_copy_deadline_invalid"
        );
        ensure!(
            dns_name(&self.input_storage_class_name)
                && dns_name(&self.artifact_storage_class_name)
                && self.input_storage_mib > 0
                && self.artifact_storage_mib > 0,
            "input_copy_storage_invalid"
        );
        ensure!(
            self.max_request_bytes > 0 && self.max_request_bytes <= SECRET_SAFE_BYTES,
            "input_copy_request_bound_invalid"
        );
        Ok(())
    }
}

/// Server-owned pre-seal authorization for a copy Job. It contains no output
/// attestation or manifest digest because the helper has not run yet.
#[derive(Clone, Debug)]
pub struct InputCopyAuthorization {
    pub task_id: Uuid,
    pub attempt: i32,
    pub lease_generation: i64,
    pub run_id: String,
    pub tenant_id: String,
    pub source_run_name: String,
    pub source_pvc_name: String,
    pub principal_hash: String,
    pub profile_id: String,
    pub namespace: String,
    pub input_digest: String,
    /// Exact operator-approved expiry, shared by input and artifact custody.
    pub custody_retention_until: String,
}

/// A trusted publisher constructs this after authorizing its source PVC and
/// producing the private Secret that contains the returned request bytes.
/// `CopySealRequest` is deliberately kept private because its declared paths
/// and opaque references are not workload metadata.
#[derive(Clone)]
pub struct AuthorizedInputCopy {
    source_run_name: String,
    source_pvc_name: String,
    request_secret_name: String,
    request: CopySealRequest,
}

impl std::fmt::Debug for AuthorizedInputCopy {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("AuthorizedInputCopy")
            .field("source_run_name", &self.source_run_name)
            .field("source_pvc_name", &self.source_pvc_name)
            .field("request_secret_name", &self.request_secret_name)
            .field("request_schema", &self.request.schema_version)
            .finish_non_exhaustive()
    }
}

impl AuthorizedInputCopy {
    pub fn new(
        source_run_name: String,
        source_pvc_name: String,
        request_secret_name: String,
        request: CopySealRequest,
    ) -> Result<Self> {
        ensure!(dns_label(&source_run_name), "input_copy_source_run_invalid");
        ensure!(
            source_pvc_name == format!("input-{source_run_name}")
                || source_pvc_name == format!("artifacts-{source_run_name}"),
            "input_copy_source_pvc_invalid"
        );
        ensure!(
            dns_label(&request_secret_name),
            "input_copy_request_secret_invalid"
        );
        // An approved plan names admitted files by size and digest only. Their
        // bytes come from the broker record through `with_admitted_file`.
        ensure!(
            request.admitted_files.is_empty(),
            "input_copy_plan_admitted_bytes_forbidden"
        );
        Ok(Self {
            source_run_name,
            source_pvc_name,
            request_secret_name,
            request,
        })
    }

    /// Attach admitted-code bytes the custodian read for the current lease. The
    /// plan's declaration must already name the path, size and SHA-256.
    pub fn with_admitted_file(mut self, path: String, content: String) -> Result<Self> {
        ensure!(
            self.request.admitted_files.is_empty(),
            "input_copy_admitted_already_attached"
        );
        self.request
            .admitted_files
            .push(crate::custody::AdmittedFile { path, content });
        crate::custody::validate_admitted_files(&self.request)
            .map_err(|_| anyhow::anyhow!("input_copy_request_admitted_invalid"))?;
        Ok(self)
    }
}

#[derive(Clone)]
pub struct InputCopyWorkload {
    pub job_name: String,
    pub job: Value,
    pub input_pvc: Value,
    pub artifact_pvc: Value,
    pub network_policy: Value,
    request_bytes: Vec<u8>,
}

impl std::fmt::Debug for InputCopyWorkload {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("InputCopyWorkload")
            .field("job_name", &self.job_name)
            .field("job", &self.job)
            .field("input_pvc", &self.input_pvc)
            .field("artifact_pvc", &self.artifact_pvc)
            .field("network_policy", &self.network_policy)
            .field(
                "request_bytes",
                &format_args!("<redacted:{} bytes>", self.request_bytes.len()),
            )
            .finish()
    }
}

impl InputCopyWorkload {
    /// Private request bytes for a trusted publisher to place in the immutable
    /// per-run Secret. They must never be emitted as workload metadata or logs.
    pub(crate) fn request_bytes(&self) -> &[u8] {
        &self.request_bytes
    }
}

pub struct InputWorkloadRenderer {
    config: InputWorkloadConfig,
}

impl InputWorkloadRenderer {
    pub fn new(config: InputWorkloadConfig) -> Result<Self> {
        config.validate()?;
        Ok(Self { config })
    }

    /// The caller must resolve and authorize the source PVC before rendering.
    /// This renderer binds only its fixed name to the task, binding, and
    /// pre-seal authorization. It never treats a rendered Job as a sealed PVC.
    pub fn render(
        &self,
        task: &ClaimedTask,
        binding: &SandboxBinding,
        authorization: &InputCopyAuthorization,
        copy: &AuthorizedInputCopy,
    ) -> Result<InputCopyWorkload> {
        validate_binding(task, binding, authorization, copy, &self.config)?;
        let request = request_json(&copy.request)?;
        ensure!(
            request.len() <= self.config.max_request_bytes,
            "input_copy_request_too_large"
        );
        let job_name = job_name(task, authorization)?;
        let mut selector = labels(task, binding, authorization, &job_name);
        self.config.site.extend_labels(&mut selector);
        let annotations = annotations(authorization, &request);
        let destination_pvc = format!("input-{}", authorization.run_id);
        ensure!(
            copy.source_pvc_name != destination_pvc,
            "input_copy_source_destination_conflict"
        );
        Ok(InputCopyWorkload {
            job: job(
                &self.config,
                &binding.namespace,
                &job_name,
                &selector,
                &annotations,
                &copy.source_pvc_name,
                &destination_pvc,
                &copy.request_secret_name,
            ),
            input_pvc: input_pvc(
                &self.config,
                &binding.namespace,
                authorization,
                task,
                &annotations,
            ),
            artifact_pvc: artifact_pvc(&self.config, &binding.namespace, authorization, task),
            network_policy: network_policy(&binding.namespace, &job_name, &selector),
            job_name,
            request_bytes: request,
        })
    }
}

fn validate_binding(
    task: &ClaimedTask,
    binding: &SandboxBinding,
    authorization: &InputCopyAuthorization,
    copy: &AuthorizedInputCopy,
    config: &InputWorkloadConfig,
) -> Result<()> {
    parse_retention_until(&authorization.custody_retention_until)?;
    ensure!(
        task.principal_id == binding.principal_id
            && task.group_key == binding.group_key
            && task.profile_key == binding.profile_key
            && task.attempt > 0
            && task.lease_generation > 0
            && task.id == authorization.task_id
            && task.attempt == authorization.attempt
            && task.lease_generation == authorization.lease_generation
            && binding.namespace == authorization.namespace
            && binding.profile_id == authorization.profile_id
            && binding.principal_hash == authorization.principal_hash
            && binding.runtime_class_name == config.runtime_class_name,
        "input_copy_task_binding_mismatch"
    );
    ensure!(
        authorization.run_id == copy.request.run_id
            && authorization.tenant_id == copy.request.tenant_id
            && authorization.namespace == copy.request.namespace
            && authorization.principal_hash == copy.request.principal_hash
            && authorization.input_digest == copy.request.input_digest
            && authorization.source_run_name == copy.source_run_name
            && authorization.source_pvc_name == copy.source_pvc_name
            && copy.request.pvc_name == format!("input-{}", authorization.run_id)
            && copy.request.schema_version == CONTRACT
            && copy.request.source_root == std::path::Path::new("/source")
            && copy.request.destination_root == std::path::Path::new("/destination")
            && copy.request.custody_root == std::path::Path::new("/custody"),
        "input_copy_authorization_mismatch"
    );
    ensure!(
        digest(&authorization.input_digest),
        "input_copy_authorization_digest_invalid"
    );
    ensure!(
        dns_label(&authorization.run_id)
            && authorization.run_id.len() <= 53
            && dns_label(&authorization.tenant_id)
            && dns_label(&authorization.namespace)
            && dns_label(&authorization.source_run_name)
            && (authorization.source_pvc_name
                == format!("input-{}", authorization.source_run_name)
                || authorization.source_pvc_name
                    == format!("artifacts-{}", authorization.source_run_name))
            && copy.source_run_name != authorization.run_id,
        "input_copy_run_invalid"
    );
    validate_declared_request(&copy.request)
}

pub(crate) fn parse_retention_until(value: &str) -> Result<OffsetDateTime> {
    OffsetDateTime::parse(value, &Rfc3339)
        .map_err(|_| anyhow::anyhow!("input_copy_retention_invalid"))
}

fn validate_declared_request(request: &CopySealRequest) -> Result<()> {
    ensure!(
        dns_label(&request.staging_id)
            && dns_label(&request.run_id)
            && dns_label(&request.tenant_id)
            && dns_label(&request.namespace)
            && request.run_id.len() <= 53
            && request.principal_hash.len() == 32
            && request
                .principal_hash
                .bytes()
                .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
            && digest(&request.input_digest)
            && opaque(&request.input_ref, "input")
            && opaque(&request.manifest_ref, "artifact")
            && opaque(&request.attestation_ref, "receipt"),
        "input_copy_request_invalid"
    );
    ensure!(!request.files.is_empty(), "input_copy_request_invalid");
    let max_files = request.max_files.unwrap_or(100_000);
    let max_bytes = request.max_bytes.unwrap_or(64 * 1024 * 1024 * 1024);
    ensure!(
        max_files > 0
            && max_files <= 100_000
            && max_bytes > 0
            && max_bytes <= 64 * 1024 * 1024 * 1024,
        "input_copy_request_bounds_invalid"
    );
    ensure!(
        request.files.len() as u64 <= max_files,
        "input_copy_request_bounds_invalid"
    );
    let mut paths = std::collections::BTreeSet::new();
    let mut total = 0_u64;
    for file in &request.files {
        ensure!(
            safe_relative(&file.path) && digest(&file.sha256),
            "input_copy_request_file_invalid"
        );
        ensure!(
            paths.insert(&file.path),
            "input_copy_request_duplicate_path"
        );
        total = total
            .checked_add(file.size_bytes)
            .ok_or_else(|| anyhow::anyhow!("input_copy_request_bounds_invalid"))?;
    }
    ensure!(total <= max_bytes, "input_copy_request_bounds_invalid");
    crate::custody::validate_admitted_files(request)
        .map_err(|_| anyhow::anyhow!("input_copy_request_admitted_invalid"))?;
    Ok(())
}

fn request_json(request: &CopySealRequest) -> Result<Vec<u8>> {
    let mut value = json!({
        "schemaVersion": request.schema_version,
        "stagingId": request.staging_id,
        "runId": request.run_id,
        "tenantId": request.tenant_id,
        "namespace": request.namespace,
        "principalHash": request.principal_hash,
        "inputRef": request.input_ref,
        "inputDigest": request.input_digest,
        "pvcName": request.pvc_name,
        "manifestRef": request.manifest_ref,
        "attestationRef": request.attestation_ref,
        "sourceRoot": request.source_root,
        "destinationRoot": request.destination_root,
        "custodyRoot": request.custody_root,
        "files": request.files,
        "maxFiles": request.max_files,
        "maxBytes": request.max_bytes,
    });
    // Fixed recipes keep their exact prior request bytes.
    if !request.admitted_files.is_empty() {
        value["admittedFiles"] = json!(request.admitted_files);
    }
    serde_json::to_vec(&value).map_err(Into::into)
}

fn job_name(task: &ClaimedTask, authorization: &InputCopyAuthorization) -> Result<String> {
    ensure!(
        task.attempt > 0 && task.lease_generation > 0,
        "input_copy_task_invalid"
    );
    let material = format!(
        "{}:{}:{}:{}",
        task.id, task.attempt, task.lease_generation, authorization.run_id
    );
    Ok(format!(
        "{INPUT_COPY_JOB_PREFIX}{}",
        &hex::encode(Sha256::digest(material.as_bytes()))[..32]
    ))
}

fn labels(
    task: &ClaimedTask,
    binding: &SandboxBinding,
    authorization: &InputCopyAuthorization,
    job_name: &str,
) -> BTreeMap<String, String> {
    let mut labels = BTreeMap::new();
    labels.insert(
        "app.kubernetes.io/name".into(),
        "enrichedlab-agent-input-copy".into(),
    );
    labels.insert("app.kubernetes.io/component".into(), "custodian".into());
    labels.insert(
        "sandbox.enrichedlab.org/contract-version".into(),
        "v3".into(),
    );
    labels.insert(
        "sandbox.enrichedlab.org/run-id".into(),
        authorization.run_id.clone(),
    );
    labels.insert(
        "sandbox.enrichedlab.org/tenant".into(),
        authorization.tenant_id.clone(),
    );
    labels.insert(
        "sandbox.enrichedlab.org/principal-hash".into(),
        authorization.principal_hash.clone(),
    );
    labels.insert(
        "sandbox.enrichedlab.org/profile".into(),
        authorization.profile_id.clone(),
    );
    labels.insert(
        "sandbox.enrichedlab.org/runtime".into(),
        binding.runtime_class_name.clone(),
    );
    labels.insert(
        "sandbox.enrichedlab.org/lab-hash".into(),
        hex::encode(Sha256::digest(binding.lab_ref.as_bytes()))[..32].into(),
    );
    labels.insert(
        "sandbox.enrichedlab.org/copy-scope".into(),
        hex::encode(Sha256::digest(
            format!("{}:{}:{}", task.id, task.attempt, task.lease_generation).as_bytes(),
        ))[..32]
            .into(),
    );
    labels.insert(
        "sandbox.enrichedlab.org/input-copy-job".into(),
        job_name.into(),
    );
    labels
}

fn annotations(authorization: &InputCopyAuthorization, request: &[u8]) -> BTreeMap<String, String> {
    BTreeMap::from([
        (
            "sandbox.enrichedlab.org/input-digest".into(),
            authorization.input_digest.clone(),
        ),
        (
            "sandbox.enrichedlab.org/input-materialization".into(),
            "checksummed-copy".into(),
        ),
        (
            "sandbox.enrichedlab.org/input-copy-request-digest".into(),
            format!("sha256:{}", hex::encode(Sha256::digest(request))),
        ),
    ])
}

#[expect(
    clippy::too_many_arguments,
    reason = "the rendered Job binds each independently named source, destination, request, selector, and annotation input"
)]
fn job(
    config: &InputWorkloadConfig,
    namespace: &str,
    name: &str,
    selector: &BTreeMap<String, String>,
    annotations: &BTreeMap<String, String>,
    source_pvc: &str,
    destination_pvc: &str,
    request_secret: &str,
) -> Value {
    let security = security_context();
    let source_mount = if source_pvc.starts_with("input-") {
        json!({"name": "source", "mountPath": "/source", "subPath": "payload", "readOnly": true})
    } else {
        json!({"name": "source", "mountPath": "/source", "readOnly": true})
    };
    json!({
        "apiVersion": "batch/v1",
        "kind": "Job",
        "metadata": {"name": name, "namespace": namespace, "labels": selector, "annotations": annotations},
        "spec": {
            "backoffLimit": 0,
            "activeDeadlineSeconds": config.active_deadline_seconds,
            "template": {
                "metadata": {"labels": selector, "annotations": annotations},
                "spec": {
                    "runtimeClassName": config.runtime_class_name,
                    "imagePullSecrets": config.site.pull_references(),
                    "automountServiceAccountToken": false,
                    "enableServiceLinks": false,
                    "restartPolicy": "Never",
                    "terminationGracePeriodSeconds": 10,
                    "nodeSelector": config.pool_selector,
                    "securityContext": {
                        "runAsNonRoot": true,
                        "runAsUser": UID,
                        "runAsGroup": UID,
                        "fsGroup": UID,
                        "fsGroupChangePolicy": "OnRootMismatch",
                        "seccompProfile": {"type": "RuntimeDefault"}
                    },
                    "containers": [{
                        "name": CUSTODIAN_CONTAINER_NAME,
                        "image": config.image,
                        "imagePullPolicy": "IfNotPresent",
                        "command": [CUSTODIAN_COMMAND],
                        "args": ["--request", "/request/request.json"],
                        "securityContext": security,
                        "resources": resources(config),
                        "volumeMounts": [
                            source_mount,
                            {"name": "destination", "mountPath": "/destination", "readOnly": false},
                            {"name": "custody", "mountPath": "/custody", "readOnly": false},
                            {"name": "request", "mountPath": "/request", "readOnly": true}
                        ]
                    }],
                    "volumes": [
                        {"name": "source", "persistentVolumeClaim": {"claimName": source_pvc, "readOnly": true}},
                        {"name": "destination", "persistentVolumeClaim": {"claimName": destination_pvc, "readOnly": false}},
                        {"name": "custody", "emptyDir": {"medium": "Memory", "sizeLimit": format!("{}Mi", config.custody_memory_mib)}},
                        {"name": "request", "projected": {"defaultMode": 288, "sources": [{"secret": {"name": request_secret, "items": [{"key": "request.json", "path": "request.json"}]}}]}}
                    ]
                }
            }
        }
    })
}

fn pvc_labels(
    task: &ClaimedTask,
    authorization: &InputCopyAuthorization,
    kind: &str,
) -> BTreeMap<String, String> {
    let (component, volume_role) = match kind {
        "input" => ("input", "immutable-input"),
        "artifacts" => ("artifacts", "durable-artifacts"),
        _ => unreachable!("PVC kinds are fixed by the renderer"),
    };
    let mut labels = BTreeMap::from([
        (
            "app.kubernetes.io/name".into(),
            "enrichedlab-agent-sandbox".into(),
        ),
        ("app.kubernetes.io/component".into(), component.into()),
        (
            "sandbox.enrichedlab.org/custody-owner".into(),
            "durable-task-custody".into(),
        ),
        (
            "sandbox.enrichedlab.org/contract-version".into(),
            "v3".into(),
        ),
        (
            "sandbox.enrichedlab.org/volume-role".into(),
            volume_role.into(),
        ),
        (
            "sandbox.enrichedlab.org/task-id".into(),
            task.id.to_string(),
        ),
        (
            "sandbox.enrichedlab.org/run-id".into(),
            authorization.run_id.clone(),
        ),
        (
            "sandbox.enrichedlab.org/tenant".into(),
            authorization.tenant_id.clone(),
        ),
        (
            "sandbox.enrichedlab.org/principal-hash".into(),
            authorization.principal_hash.clone(),
        ),
    ]);
    if kind == "input" {
        labels.insert(
            "sandbox.enrichedlab.org/seal-state".into(),
            "staging".into(),
        );
        labels.insert(
            "sandbox.enrichedlab.org/materialization-mode".into(),
            "checksummed-copy".into(),
        );
    }
    labels
}

fn input_pvc(
    config: &InputWorkloadConfig,
    namespace: &str,
    authorization: &InputCopyAuthorization,
    task: &ClaimedTask,
    annotations: &BTreeMap<String, String>,
) -> Value {
    pvc(
        namespace,
        &format!("input-{}", authorization.run_id),
        &pvc_labels(task, authorization, "input"),
        Some(&pvc_annotations(authorization, Some(annotations))),
        &config.input_storage_class_name,
        config.input_storage_mib,
        config.custody_pvc_access_mode,
    )
}

fn artifact_pvc(
    config: &InputWorkloadConfig,
    namespace: &str,
    authorization: &InputCopyAuthorization,
    task: &ClaimedTask,
) -> Value {
    pvc(
        namespace,
        &format!("artifacts-{}", authorization.run_id),
        &pvc_labels(task, authorization, "artifacts"),
        Some(&pvc_annotations(authorization, None)),
        &config.artifact_storage_class_name,
        config.artifact_storage_mib,
        config.custody_pvc_access_mode,
    )
}

fn pvc_annotations(
    authorization: &InputCopyAuthorization,
    input_annotations: Option<&BTreeMap<String, String>>,
) -> BTreeMap<String, String> {
    let mut annotations = input_annotations.cloned().unwrap_or_default();
    annotations.insert(
        "sandbox.enrichedlab.org/retention-owner".into(),
        "durable-task-custody".into(),
    );
    annotations.insert(
        "sandbox.enrichedlab.org/retention-until".into(),
        authorization.custody_retention_until.clone(),
    );
    annotations
}

fn pvc(
    namespace: &str,
    name: &str,
    labels: &BTreeMap<String, String>,
    annotations: Option<&BTreeMap<String, String>>,
    storage_class_name: &str,
    storage_mib: u32,
    access_mode: CustodyPvcAccessMode,
) -> Value {
    let mut metadata = json!({"name": name, "namespace": namespace, "labels": labels});
    if let Some(annotations) = annotations {
        metadata["annotations"] =
            serde_json::to_value(annotations).expect("BTreeMap<String, String> always serializes");
    }
    json!({
        "apiVersion": "v1",
        "kind": "PersistentVolumeClaim",
        "metadata": metadata,
        "spec": {
            "accessModes": [access_mode.kubernetes_access_mode()],
            "storageClassName": storage_class_name,
            "resources": {"requests": {"storage": format!("{storage_mib}Mi")}}
        }
    })
}

fn network_policy(namespace: &str, name: &str, selector: &BTreeMap<String, String>) -> Value {
    json!({
        "apiVersion": "networking.k8s.io/v1",
        "kind": "NetworkPolicy",
        "metadata": {"name": name, "namespace": namespace, "labels": selector},
        "spec": {"podSelector": {"matchLabels": selector}, "policyTypes": ["Ingress", "Egress"], "ingress": [], "egress": []}
    })
}

fn resources(config: &InputWorkloadConfig) -> Value {
    json!({
        "requests": {"cpu": format!("{}m", config.cpu_request_millis), "memory": format!("{}Mi", config.memory_request_mib)},
        "limits": {"cpu": format!("{}m", config.cpu_limit_millis), "memory": format!("{}Mi", config.memory_limit_mib)}
    })
}

fn security_context() -> Value {
    json!({
        "runAsNonRoot": true,
        "runAsUser": UID,
        "runAsGroup": UID,
        "allowPrivilegeEscalation": false,
        "readOnlyRootFilesystem": true,
        "capabilities": {"drop": ["ALL"]},
        "seccompProfile": {"type": "RuntimeDefault"}
    })
}

fn safe_relative(path: &str) -> bool {
    !path.is_empty()
        && path.len() <= 4096
        && !path.starts_with('/')
        && !path.bytes().any(|byte| byte == 0)
        && path
            .split('/')
            .all(|part| !part.is_empty() && part != "." && part != "..")
}

fn pinned_image(image: &str) -> bool {
    image.len() <= 512
        && image.contains('@')
        && image
            .rsplit_once("@sha256:")
            .is_some_and(|(reference, digest)| !reference.is_empty() && sha256(digest))
        && image.bytes().all(|byte| {
            byte.is_ascii_lowercase()
                || byte.is_ascii_digit()
                || matches!(byte, b'.' | b'_' | b'-' | b'/' | b':' | b'@')
        })
        && !image.chars().any(char::is_whitespace)
}

fn sha256(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn digest(value: &str) -> bool {
    value.len() == 71 && value.starts_with("sha256:") && sha256(&value[7..])
}

fn opaque(value: &str, scheme: &str) -> bool {
    value.starts_with(&format!("{scheme}://"))
        && value.len() <= 460
        && !value.contains(['?', '#', '@'])
}

fn dns_name(value: &str) -> bool {
    value.len() <= 253 && value.split('.').all(dns_label)
}

pub(crate) fn dns_label(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 63
        && value.starts_with(|byte: char| byte.is_ascii_alphanumeric())
        && value.ends_with(|byte: char| byte.is_ascii_alphanumeric())
        && value
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
}

pub(crate) fn label_key(value: &str) -> bool {
    let (prefix, name) = value
        .rsplit_once('/')
        .map_or((None, value), |(prefix, name)| (Some(prefix), name));
    dns_label(name)
        && prefix.is_none_or(|prefix| prefix.len() <= 253 && prefix.split('.').all(dns_label))
}

pub(crate) fn label_value(value: &str) -> bool {
    value.is_empty()
        || (value.len() <= 63
            && value.starts_with(|byte: char| byte.is_ascii_alphanumeric())
            && value.ends_with(|byte: char| byte.is_ascii_alphanumeric())
            && value
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.')))
}
