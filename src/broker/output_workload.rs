//! Restricted one-shot output inventory workload and guarded observer.
//!
//! This module only measures an approved output selection. It never creates a
//! terminal export record, binds a lease, or authorizes a PVC.

use crate::{
    broker::{
        model::ClaimedTask,
        publication_journal::PublicationJournal,
        sandbox::{RuntimeCompletion, SandboxBinding, SandboxHandle},
    },
    custody::canonical_document_digest,
    output_inventory::{INVENTORY_SCHEMA, OutputInventory, OutputInventoryRequest, SelectedOutput},
};
use anyhow::{Result, anyhow, bail, ensure};
use async_trait::async_trait;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{collections::BTreeMap, time::Duration};
use tokio::time::{Instant, sleep};
use tokio_util::sync::CancellationToken;

pub const OUTPUT_INVENTORY_CONTAINER: &str = "output-inventory";
pub const OUTPUT_INVENTORY_COMMAND: &str = "/usr/local/bin/agent-sandbox-output-inventory";
const MATERIALIZE_REQUEST_CONTAINER: &str = "materialize-request";
const REQUEST_COPY_COMMAND: &str = "umask 077; install -d -m 0700 /request/private; install -m 0600 /projected/request.json /request/private/request.json";
const REQUEST_LIMIT: usize = 768 * 1024;
const STDOUT_LIMIT: usize = 1024 * 1024;
const STARTUP_GRACE: Duration = Duration::from_secs(15);

#[derive(Clone, Debug, serde::Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct OutputInventoryWorkloadConfig {
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
    pub max_request_bytes: usize,
    pub request_memory_mib: u32,
    pub journal_root: std::path::PathBuf,
    pub writer_uid: u32,
}
impl OutputInventoryWorkloadConfig {
    pub fn validate(&self) -> Result<()> {
        self.site.validate()?;
        ensure!(
            dns(&self.runtime_class_name),
            "output_inventory_runtime_invalid"
        );
        ensure!(pinned_image(&self.image), "output_inventory_image_unpinned");
        ensure!(
            !self.pool_selector.is_empty()
                && self
                    .pool_selector
                    .iter()
                    .all(|(key, value)| label_key(key) && label_value(value)),
            "output_inventory_pool_selector_invalid"
        );
        ensure!(
            self.cpu_request_millis > 0
                && self.cpu_request_millis <= self.cpu_limit_millis
                && self.memory_request_mib > 0
                && self.memory_request_mib <= self.memory_limit_mib,
            "output_inventory_resources_invalid"
        );
        ensure!(
            (1..=86_400).contains(&self.active_deadline_seconds),
            "output_inventory_deadline_invalid"
        );
        ensure!(
            (1..=REQUEST_LIMIT).contains(&self.max_request_bytes)
                && self.request_memory_mib > 0
                && self.request_memory_mib <= self.memory_limit_mib,
            "output_inventory_request_bound_invalid"
        );
        ensure!(
            crate::broker::publication_journal::valid_private_root(&self.journal_root)
                && self.writer_uid > 0,
            "output_inventory_journal_config_invalid"
        );
        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OutputPvc {
    pub name: String,
    pub uid: String,
}

#[derive(Clone, Debug)]
pub struct OutputInventoryWorkload {
    pub job_name: String,
    pub job: Value,
    pub network_policy: Value,
    request_bytes: Vec<u8>,
}
impl OutputInventoryWorkload {
    pub(crate) fn request_bytes(&self) -> &[u8] {
        &self.request_bytes
    }
}

pub struct OutputInventoryRenderer {
    config: OutputInventoryWorkloadConfig,
}
impl OutputInventoryRenderer {
    pub fn new(config: OutputInventoryWorkloadConfig) -> Result<Self> {
        config.validate()?;
        Ok(Self { config })
    }
    pub fn render(
        &self,
        task: &ClaimedTask,
        binding: &SandboxBinding,
        handle: &SandboxHandle,
        request: &OutputInventoryRequest,
        output_pvc: &OutputPvc,
        request_secret_name: &str,
    ) -> Result<OutputInventoryWorkload> {
        validate_request(
            task,
            binding,
            handle,
            request,
            output_pvc,
            request_secret_name,
            &self.config,
        )?;
        let request_bytes = private_request(request)?;
        ensure!(
            request_bytes.len() <= self.config.max_request_bytes,
            "output_inventory_request_too_large"
        );
        let job_name = job_name(task, handle)?;
        let mut labels = labels(task, binding, handle, &job_name);
        self.config.site.extend_labels(&mut labels);
        Ok(OutputInventoryWorkload {
            job: job(
                &self.config,
                &binding.namespace,
                &job_name,
                &labels,
                &output_pvc.name,
                request_secret_name,
            ),
            network_policy: network_policy(&binding.namespace, &job_name, &labels),
            job_name,
            request_bytes,
        })
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum OutputInventoryObjectKind {
    NetworkPolicy,
    Secret,
    Job,
}
#[derive(Clone)]
pub struct OutputInventoryObject {
    kind: OutputInventoryObjectKind,
    namespace: String,
    name: String,
    body: Value,
}
impl OutputInventoryObject {
    fn new(kind: OutputInventoryObjectKind, namespace: &str, name: &str, body: Value) -> Self {
        Self {
            kind,
            namespace: namespace.into(),
            name: name.into(),
            body,
        }
    }
    pub fn kind(&self) -> &OutputInventoryObjectKind {
        &self.kind
    }
    pub fn namespace(&self) -> &str {
        &self.namespace
    }
    pub fn name(&self) -> &str {
        &self.name
    }
    pub(crate) fn body(&self) -> &Value {
        &self.body
    }
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OutputInventoryObjectReceipt {
    pub kind: OutputInventoryObjectKind,
    pub namespace: String,
    pub name: String,
    pub uid: String,
}

#[async_trait]
pub trait OutputInventoryTransport: Send + Sync {
    fn namespace(&self) -> &str;
    async fn create(
        &self,
        object: &OutputInventoryObject,
        cancellation: &CancellationToken,
    ) -> Result<OutputInventoryObjectReceipt>;
    async fn get_pvc(&self, namespace: &str, name: &str) -> Result<Value>;
    async fn has_active_writer(&self, namespace: &str, pvc_name: &str) -> Result<bool>;
    async fn get_job(&self, namespace: &str, name: &str) -> Result<Value>;
    async fn get_controlled_pod(&self, namespace: &str, job_name: &str) -> Result<Option<Value>>;
    async fn helper_stdout(
        &self,
        namespace: &str,
        pod_name: &str,
        container: &str,
    ) -> Result<Vec<u8>>;
}
#[async_trait]
pub trait OutputInventoryGuard: Send + Sync {
    async fn check(&self) -> Result<bool>;
}

pub struct OutputInventoryJobRunner<T> {
    renderer: OutputInventoryRenderer,
    transport: T,
    poll_interval: Duration,
}
impl<T: OutputInventoryTransport> OutputInventoryJobRunner<T> {
    pub fn new(
        config: OutputInventoryWorkloadConfig,
        transport: T,
        poll_interval: Duration,
    ) -> Result<Self> {
        ensure!(
            !poll_interval.is_zero() && poll_interval <= Duration::from_secs(30),
            "output_inventory_poll_interval_invalid"
        );
        Ok(Self {
            renderer: OutputInventoryRenderer::new(config)?,
            transport,
            poll_interval,
        })
    }
    #[expect(
        clippy::too_many_arguments,
        reason = "the output inventory boundary retains separate task, binding, completion, PVC, request, and guard authority"
    )]
    pub async fn run<G: OutputInventoryGuard>(
        &self,
        task: &ClaimedTask,
        binding: &SandboxBinding,
        handle: &SandboxHandle,
        completion: &RuntimeCompletion,
        request: &OutputInventoryRequest,
        output_pvc: OutputPvc,
        request_secret_name: &str,
        guard: &G,
        cancellation: &CancellationToken,
    ) -> Result<OutputInventoryJobReceipt> {
        let total = Duration::from_secs(self.renderer.config.active_deadline_seconds.into())
            + STARTUP_GRACE;
        tokio::select! {
            biased;
            _ = cancellation.cancelled() => bail!("output_inventory_cancelled"),
            result = tokio::time::timeout(total, self.run_inner(task,binding,handle,completion,request,output_pvc,request_secret_name,guard,cancellation)) => result.map_err(|_| anyhow!("output_inventory_deadline_exceeded"))?,
        }
    }
    #[expect(
        clippy::too_many_arguments,
        reason = "the bounded runner passes the same independently validated authority inputs to its single attempt"
    )]
    async fn run_inner<G: OutputInventoryGuard>(
        &self,
        task: &ClaimedTask,
        binding: &SandboxBinding,
        handle: &SandboxHandle,
        completion: &RuntimeCompletion,
        request: &OutputInventoryRequest,
        output_pvc: OutputPvc,
        request_secret_name: &str,
        guard: &G,
        cancellation: &CancellationToken,
    ) -> Result<OutputInventoryJobReceipt> {
        validate_completion(task, binding, handle, completion)?;
        ensure!(
            self.transport.namespace() == binding.namespace,
            "output_inventory_namespace_mismatch"
        );
        let workload = self.renderer.render(
            task,
            binding,
            handle,
            request,
            &output_pvc,
            request_secret_name,
        )?;
        let mut journal = PublicationJournal::create(
            &self.renderer.config.journal_root,
            &format!("{}.jsonl", workload.job_name),
            self.renderer.config.writer_uid,
        )
        .await
        .map_err(|_| anyhow!("output_inventory_journal_unavailable"))?;
        journal.append(json!({"phase":"prepared","job":workload.job_name,"pvc":output_pvc.name,"pvcUid":output_pvc.uid})).await.map_err(|_| anyhow!("output_inventory_journal_failed"))?;
        self.check(guard, cancellation).await?;
        self.verify_pvc(&output_pvc, binding, handle).await?;
        let secret = secret(
            &binding.namespace,
            request_secret_name,
            workload.request_bytes(),
        )?;
        let network = OutputInventoryObject::new(
            OutputInventoryObjectKind::NetworkPolicy,
            &binding.namespace,
            &workload.job_name,
            workload.network_policy,
        );
        let secret = OutputInventoryObject::new(
            OutputInventoryObjectKind::Secret,
            &binding.namespace,
            request_secret_name,
            secret,
        );
        let job = OutputInventoryObject::new(
            OutputInventoryObjectKind::Job,
            &binding.namespace,
            &workload.job_name,
            workload.job,
        );
        journal
            .append(json!({"phase":"create_intent","kind":"NetworkPolicy","name":network.name()}))
            .await
            .map_err(|_| anyhow!("output_inventory_journal_failed"))?;
        let network_policy = self
            .transport
            .create(&network, cancellation)
            .await
            .map_err(|_| anyhow!("output_inventory_network_create_failed"))?;
        journal
            .append(json!({"phase":"created","kind":"NetworkPolicy","uid":network_policy.uid}))
            .await
            .map_err(|_| anyhow!("output_inventory_journal_failed"))?;
        self.check(guard, cancellation).await?;
        journal
            .append(json!({"phase":"create_intent","kind":"Secret","name":secret.name()}))
            .await
            .map_err(|_| anyhow!("output_inventory_journal_failed"))?;
        let request_secret = self
            .transport
            .create(&secret, cancellation)
            .await
            .map_err(|_| anyhow!("output_inventory_secret_create_failed"))?;
        journal
            .append(json!({"phase":"created","kind":"Secret","uid":request_secret.uid}))
            .await
            .map_err(|_| anyhow!("output_inventory_journal_failed"))?;
        self.check(guard, cancellation).await?;
        journal
            .append(json!({"phase":"create_intent","kind":"Job","name":job.name()}))
            .await
            .map_err(|_| anyhow!("output_inventory_journal_failed"))?;
        let job_receipt = self
            .transport
            .create(&job, cancellation)
            .await
            .map_err(|_| anyhow!("output_inventory_job_create_failed"))?;
        journal
            .append(json!({"phase":"created","kind":"Job","uid":job_receipt.uid}))
            .await
            .map_err(|_| anyhow!("output_inventory_journal_failed"))?;
        ensure!(
            job_receipt.uid.len() >= 8,
            "output_inventory_job_create_invalid"
        );
        let deadline = Instant::now()
            + Duration::from_secs(self.renderer.config.active_deadline_seconds.into());
        loop {
            self.check(guard, cancellation).await?;
            let job = self
                .transport
                .get_job(&binding.namespace, &workload.job_name)
                .await
                .map_err(|_| anyhow!("output_inventory_job_unavailable"))?;
            if terminal_success(
                &job,
                &binding.namespace,
                &workload.job_name,
                &job_receipt.uid,
            )? {
                let pod = self
                    .transport
                    .get_controlled_pod(&binding.namespace, &workload.job_name)
                    .await
                    .map_err(|_| anyhow!("output_inventory_pod_unavailable"))?
                    .ok_or_else(|| anyhow!("output_inventory_pod_absent"))?;
                let (pod_name, pod_uid) = terminal_pod(
                    &pod,
                    &binding.namespace,
                    &job_receipt.uid,
                    &self.renderer.config,
                    &output_pvc.name,
                    request_secret_name,
                )?;
                self.check(guard, cancellation).await?;
                let original = self
                    .transport
                    .helper_stdout(&binding.namespace, &pod_name, OUTPUT_INVENTORY_CONTAINER)
                    .await
                    .map_err(|_| anyhow!("output_inventory_stdout_unavailable"))?;
                let inventory = validate_inventory(&original, request)?;
                self.check(guard, cancellation).await?;
                self.verify_pvc(&output_pvc, binding, handle).await?;
                journal
                    .append(json!({"phase":"observed","jobUid":job_receipt.uid,"podUid":pod_uid}))
                    .await
                    .map_err(|_| anyhow!("output_inventory_journal_failed"))?;
                return Ok(OutputInventoryJobReceipt {
                    network_policy,
                    request_secret,
                    job: job_receipt,
                    pod_name,
                    pod_uid,
                    inventory,
                    original_inventory: original,
                });
            }
            if job
                .pointer("/status/failed")
                .and_then(Value::as_i64)
                .unwrap_or(0)
                > 0
            {
                bail!("output_inventory_job_failed")
            }
            tokio::select! { biased; _=cancellation.cancelled()=>bail!("output_inventory_cancelled"), _=sleep(self.poll_interval.min(deadline.saturating_duration_since(Instant::now())))=>{} }
            if Instant::now() >= deadline {
                bail!("output_inventory_deadline_exceeded")
            }
        }
    }
    async fn check<G: OutputInventoryGuard>(
        &self,
        guard: &G,
        cancellation: &CancellationToken,
    ) -> Result<()> {
        ensure!(!cancellation.is_cancelled(), "output_inventory_cancelled");
        ensure!(
            guard
                .check()
                .await
                .map_err(|_| anyhow!("output_inventory_guard_unavailable"))?,
            "output_inventory_guard_lost"
        );
        Ok(())
    }
    async fn verify_pvc(
        &self,
        pvc: &OutputPvc,
        binding: &SandboxBinding,
        handle: &SandboxHandle,
    ) -> Result<()> {
        let observed = self
            .transport
            .get_pvc(&binding.namespace, &pvc.name)
            .await
            .map_err(|_| anyhow!("output_inventory_pvc_unavailable"))?;
        ensure!(
            observed
                .pointer("/metadata/namespace")
                .and_then(Value::as_str)
                == Some(binding.namespace.as_str())
                && observed.pointer("/metadata/name").and_then(Value::as_str)
                    == Some(pvc.name.as_str())
                && observed.pointer("/metadata/uid").and_then(Value::as_str)
                    == Some(pvc.uid.as_str())
                && pvc.name == format!("artifacts-{}", handle.sandbox_name),
            "output_inventory_pvc_mismatch"
        );
        ensure!(
            !self
                .transport
                .has_active_writer(&binding.namespace, &pvc.name)
                .await
                .map_err(|_| anyhow!("output_inventory_writer_observation_unavailable"))?,
            "output_inventory_active_writer"
        );
        Ok(())
    }
}

// Renderer-only source-inclusion tests do not construct the terminal custodian
// that consumes the complete receipt. Keep every created-object identity for
// that production custody boundary without weakening test compilation.
#[cfg_attr(test, allow(dead_code))]
pub struct OutputInventoryJobReceipt {
    pub network_policy: OutputInventoryObjectReceipt,
    pub request_secret: OutputInventoryObjectReceipt,
    pub job: OutputInventoryObjectReceipt,
    pub pod_name: String,
    pub pod_uid: String,
    inventory: OutputInventory,
    original_inventory: Vec<u8>,
}
impl OutputInventoryJobReceipt {
    pub fn job_uid(&self) -> &str {
        &self.job.uid
    }
    pub fn pod_uid(&self) -> &str {
        &self.pod_uid
    }
    pub fn inventory(&self) -> &OutputInventory {
        &self.inventory
    }
    #[cfg_attr(test, allow(dead_code))]
    pub fn original_inventory(&self) -> &[u8] {
        &self.original_inventory
    }
}

fn validate_request(
    task: &ClaimedTask,
    b: &SandboxBinding,
    h: &SandboxHandle,
    r: &OutputInventoryRequest,
    p: &OutputPvc,
    secret: &str,
    c: &OutputInventoryWorkloadConfig,
) -> Result<()> {
    ensure!(
        task.id == h.task_id
            && task.lease_generation == h.lease_generation
            && task.principal_id == b.principal_id
            && task.group_key == b.group_key
            && task.profile_key == b.profile_key
            && h.namespace == b.namespace
            && h.lab_ref == b.lab_ref
            && task.attempt > 0
            && task.lease_generation > 0
            && b.runtime_class_name == c.runtime_class_name
            && r.schema_version == crate::output_inventory::REQUEST_SCHEMA
            && r.mounted_root == std::path::Path::new("/artifacts")
            && !r.files.is_empty()
            && r.files.len() <= r.max_files
            && r.max_files <= 100_000
            && r.max_bytes > 0
            && r.max_bytes <= 64 * 1024 * 1024 * 1024
            && r.files.iter().all(safe_selected)
            && p.name == format!("artifacts-{}", h.sandbox_name)
            && valid_uid(&p.uid)
            && dns(secret),
        "output_inventory_binding_invalid"
    );
    Ok(())
}
fn safe_selected(file: &crate::output_inventory::SelectedOutput) -> bool {
    !file.relative_path.is_empty()
        && !file.relative_path.contains(['\\', '\0'])
        && file
            .relative_path
            .split('/')
            .all(|part| !matches!(part, "" | "." | ".."))
        && matches!(file.visibility.as_str(), "researcher" | "operator")
        && matches!(
            file.role.as_str(),
            "request" | "stdout" | "stderr" | "result" | "workspace" | "state" | "evidence"
        )
        && (1..=256).contains(&file.label.chars().count())
        && (1..=256).contains(&file.media_type.chars().count())
        && !file.label.contains('\0')
        && !file.media_type.contains('\0')
}

fn validate_completion(
    task: &ClaimedTask,
    b: &SandboxBinding,
    h: &SandboxHandle,
    c: &RuntimeCompletion,
) -> Result<()> {
    ensure!(
        c.succeeded()
            && c.exit_code() == 0
            && c.observation().sandbox_uid == h.sandbox_uid
            && c.observation().sandbox_generation == h.sandbox_generation
            && c.observation().runtime_class_name == b.runtime_class_name
            && task.id == h.task_id,
        "output_inventory_completion_invalid"
    );
    Ok(())
}
fn private_request(r: &OutputInventoryRequest) -> Result<Vec<u8>> {
    let value = json!({"schemaVersion":r.schema_version,"mountedRoot":r.mounted_root,"files":r.files,"maxFiles":r.max_files,"maxBytes":r.max_bytes});
    serde_json::to_vec(&value).map_err(|_| anyhow!("output_inventory_request_encode_failed"))
}
fn secret(ns: &str, name: &str, bytes: &[u8]) -> Result<Value> {
    ensure!(
        bytes.len() <= REQUEST_LIMIT,
        "output_inventory_request_too_large"
    );
    Ok(
        json!({"apiVersion":"v1","kind":"Secret","immutable":true,"metadata":{"namespace":ns,"name":name},"type":"Opaque","stringData":{"request.json":std::str::from_utf8(bytes).map_err(|_| anyhow!("output_inventory_request_encode_failed"))?}}),
    )
}
fn job_name(t: &ClaimedTask, h: &SandboxHandle) -> Result<String> {
    ensure!(
        t.attempt > 0 && t.lease_generation > 0,
        "output_inventory_task_invalid"
    );
    Ok(format!(
        "output-inventory-{}",
        &hex::encode(Sha256::digest(
            format!(
                "{}:{}:{}:{}",
                t.id, t.attempt, t.lease_generation, h.sandbox_name
            )
            .as_bytes()
        ))[..32]
    ))
}
fn labels(
    t: &ClaimedTask,
    b: &SandboxBinding,
    h: &SandboxHandle,
    job: &str,
) -> BTreeMap<String, String> {
    BTreeMap::from([
        (
            "app.kubernetes.io/name".into(),
            "enrichedlab-agent-output-inventory".into(),
        ),
        ("app.kubernetes.io/component".into(), "custodian".into()),
        (
            "sandbox.enrichedlab.org/contract-version".into(),
            "v3".into(),
        ),
        (
            "sandbox.enrichedlab.org/principal-hash".into(),
            b.principal_hash.clone(),
        ),
        (
            "sandbox.enrichedlab.org/profile".into(),
            b.profile_id.clone(),
        ),
        (
            "sandbox.enrichedlab.org/output-inventory-job".into(),
            job.into(),
        ),
        (
            "sandbox.enrichedlab.org/output-scope".into(),
            hex::encode(Sha256::digest(
                format!("{}:{}:{}", t.id, t.attempt, t.lease_generation).as_bytes(),
            ))[..32]
                .into(),
        ),
        (
            "sandbox.enrichedlab.org/run-id".into(),
            h.sandbox_name.clone(),
        ),
    ])
}
fn job(
    c: &OutputInventoryWorkloadConfig,
    ns: &str,
    name: &str,
    labels: &BTreeMap<String, String>,
    pvc: &str,
    secret: &str,
) -> Value {
    let security = json!({"runAsNonRoot":true,"runAsUser":65532,"runAsGroup":65532,"allowPrivilegeEscalation":false,"capabilities":{"drop":["ALL"]},"seccompProfile":{"type":"RuntimeDefault"}});
    json!({"apiVersion":"batch/v1","kind":"Job","metadata":{"name":name,"namespace":ns,"labels":labels},"spec":{"backoffLimit":0,"activeDeadlineSeconds":c.active_deadline_seconds,"template":{"metadata":{"labels":labels},"spec":{"runtimeClassName":c.runtime_class_name,"imagePullSecrets":c.site.pull_references(),"automountServiceAccountToken":false,"enableServiceLinks":false,"restartPolicy":"Never","terminationGracePeriodSeconds":10,"nodeSelector":c.pool_selector,"securityContext":{"runAsNonRoot":true,"runAsUser":65532,"runAsGroup":65532,"fsGroup":65532,"fsGroupChangePolicy":"OnRootMismatch","seccompProfile":{"type":"RuntimeDefault"}},"initContainers":[{"name":MATERIALIZE_REQUEST_CONTAINER,"image":c.image,"imagePullPolicy":"IfNotPresent","command":["/bin/sh","-ec"],"args":[REQUEST_COPY_COMMAND],"securityContext":security,"resources":resources(c),"volumeMounts":init_mounts()}],"containers":[{"name":OUTPUT_INVENTORY_CONTAINER,"image":c.image,"imagePullPolicy":"IfNotPresent","command":[OUTPUT_INVENTORY_COMMAND],"args":["--request-root","/request/private","--request","request.json","--writer-uid","65532"],"securityContext":security,"resources":resources(c),"volumeMounts":main_mounts()}],"volumes":expected_volumes(c,pvc,secret)}}}})
}

fn init_mounts() -> Value {
    json!([
        {"name":"request-projected","mountPath":"/projected","readOnly":true},
        {"name":"request-private","mountPath":"/request"}
    ])
}

fn main_mounts() -> Value {
    json!([
        {"name":"artifacts","mountPath":"/artifacts","readOnly":true},
        {"name":"request-private","mountPath":"/request","readOnly":true}
    ])
}

fn expected_volumes(config: &OutputInventoryWorkloadConfig, pvc: &str, secret: &str) -> Value {
    json!([
        {"name":"artifacts","persistentVolumeClaim":{"claimName":pvc,"readOnly":true}},
        {"name":"request-projected","projected":{"defaultMode":288,"sources":[{"secret":{"name":secret,"items":[{"key":"request.json","path":"request.json"}]}}]}},
        {"name":"request-private","emptyDir":{"medium":"Memory","sizeLimit":format!("{}Mi",config.request_memory_mib)}}
    ])
}

fn resources(config: &OutputInventoryWorkloadConfig) -> Value {
    json!({
        "requests":{"cpu":format!("{}m",config.cpu_request_millis),"memory":format!("{}Mi",config.memory_request_mib)},
        "limits":{"cpu":format!("{}m",config.cpu_limit_millis),"memory":format!("{}Mi",config.memory_limit_mib)}
    })
}

fn network_policy(ns: &str, name: &str, labels: &BTreeMap<String, String>) -> Value {
    json!({"apiVersion":"networking.k8s.io/v1","kind":"NetworkPolicy","metadata":{"name":name,"namespace":ns,"labels":labels},"spec":{"podSelector":{"matchLabels":labels},"policyTypes":["Ingress","Egress"],"ingress":[],"egress":[]}})
}
fn terminal_success(v: &Value, ns: &str, name: &str, uid: &str) -> Result<bool> {
    ensure!(
        v.pointer("/metadata/namespace").and_then(Value::as_str) == Some(ns)
            && v.pointer("/metadata/name").and_then(Value::as_str) == Some(name)
            && v.pointer("/metadata/uid").and_then(Value::as_str) == Some(uid),
        "output_inventory_job_replaced"
    );
    let empty_conditions = Vec::new();
    let conditions = v
        .pointer("/status/conditions")
        .and_then(Value::as_array)
        .unwrap_or(&empty_conditions);
    ensure!(
        !conditions.iter().any(
            |x| matches!(x["type"].as_str(), Some("Failed" | "FailureTarget"))
                && x["status"] == "True"
        ),
        "output_inventory_job_failed"
    );
    Ok(conditions
        .iter()
        .any(|x| x["type"] == "Complete" && x["status"] == "True")
        && v.pointer("/status/succeeded").and_then(Value::as_i64) == Some(1)
        && v.pointer("/status/active")
            .and_then(Value::as_i64)
            .unwrap_or(0)
            == 0
        && v.pointer("/status/failed")
            .and_then(Value::as_i64)
            .unwrap_or(0)
            == 0)
}
fn terminal_pod(
    v: &Value,
    ns: &str,
    job_uid: &str,
    config: &OutputInventoryWorkloadConfig,
    pvc: &str,
    secret: &str,
) -> Result<(String, String)> {
    let name = v
        .pointer("/metadata/name")
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow!("output_inventory_pod_invalid"))?
        .to_string();
    let uid = v
        .pointer("/metadata/uid")
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow!("output_inventory_pod_invalid"))?
        .to_string();
    let container = v
        .pointer("/spec/containers")
        .and_then(Value::as_array)
        .filter(|x| x.len() == 1)
        .and_then(|x| x.first())
        .ok_or_else(|| anyhow!("output_inventory_pod_invalid"))?;
    let init = v
        .pointer("/spec/initContainers")
        .and_then(Value::as_array)
        .filter(|x| x.len() == 1)
        .and_then(|x| x.first())
        .ok_or_else(|| anyhow!("output_inventory_pod_invalid"))?;
    let status = v
        .pointer("/status/containerStatuses")
        .and_then(Value::as_array)
        .filter(|x| x.len() == 1)
        .and_then(|x| x.first())
        .ok_or_else(|| anyhow!("output_inventory_pod_invalid"))?;
    ensure!(
        v.pointer("/metadata/namespace").and_then(Value::as_str) == Some(ns)
            && valid_uid(&uid)
            && v.pointer("/status/phase").and_then(Value::as_str) == Some("Succeeded")
            && v.pointer("/spec/runtimeClassName").and_then(Value::as_str)
                == Some(config.runtime_class_name.as_str())
            && v.pointer("/spec/automountServiceAccountToken")
                .and_then(Value::as_bool)
                == Some(false)
            && v.pointer("/spec/enableServiceLinks")
                .and_then(Value::as_bool)
                == Some(false)
            && v.pointer("/spec/ephemeralContainers")
                .is_none_or(|x| x.as_array().is_some_and(Vec::is_empty))
            && v.pointer("/metadata/ownerReferences")
                .and_then(Value::as_array)
                .is_some_and(|xs| xs
                    .iter()
                    .any(|x| x["uid"] == job_uid && x["controller"] == true))
            && container["name"] == OUTPUT_INVENTORY_CONTAINER
            && container["image"] == config.image
            && container["command"] == json!([OUTPUT_INVENTORY_COMMAND])
            && container["args"]
                == json!([
                    "--request-root",
                    "/request/private",
                    "--request",
                    "request.json",
                    "--writer-uid",
                    "65532"
                ])
            && container.get("volumeMounts") == Some(&main_mounts())
            && init["name"] == MATERIALIZE_REQUEST_CONTAINER
            && init["image"] == config.image
            && init["imagePullPolicy"] == "IfNotPresent"
            && init["command"] == json!(["/bin/sh", "-ec"])
            && init["args"] == json!([REQUEST_COPY_COMMAND])
            && restricted_security(&init["securityContext"])
            && init.get("resources") == Some(&resources(config))
            && init.get("volumeMounts") == Some(&init_mounts())
            && restricted_security(&container["securityContext"])
            && container.get("resources") == Some(&resources(config))
            && v.get("spec").and_then(|spec| spec.get("volumes"))
                == Some(&expected_volumes(config, pvc, secret))
            && status["name"] == OUTPUT_INVENTORY_CONTAINER
            && status["restartCount"] == 0
            && status
                .pointer("/state/terminated/exitCode")
                .and_then(Value::as_i64)
                == Some(0)
            && status
                .pointer("/state/terminated/finishedAt")
                .and_then(Value::as_str)
                .is_some_and(|x| !x.is_empty()),
        "output_inventory_pod_replaced"
    );
    Ok((name, uid))
}

fn restricted_security(value: &Value) -> bool {
    value["runAsNonRoot"] == true
        && value["runAsUser"] == 65532
        && value["runAsGroup"] == 65532
        && value["allowPrivilegeEscalation"] == false
        && value
            .pointer("/capabilities/drop")
            .and_then(Value::as_array)
            .is_some_and(|values| values.len() == 1 && values[0] == "ALL")
        && value
            .pointer("/seccompProfile/type")
            .and_then(Value::as_str)
            == Some("RuntimeDefault")
}

fn validate_inventory(bytes: &[u8], r: &OutputInventoryRequest) -> Result<OutputInventory> {
    ensure!(
        !bytes.is_empty()
            && bytes.len() <= STDOUT_LIMIT
            && bytes.ends_with(b"\n")
            && !bytes[..bytes.len() - 1].contains(&b'\n'),
        "output_inventory_stdout_invalid"
    );
    let inventory: OutputInventory = serde_json::from_slice(&bytes[..bytes.len() - 1])
        .map_err(|_| anyhow!("output_inventory_stdout_invalid"))?;
    ensure!(
        inventory.schema_version == INVENTORY_SCHEMA
            && inventory.selection_digest == expected_selection_digest(r)?
            && inventory.files.len() == r.files.len()
            && inventory
                .files
                .iter()
                .zip(&r.files)
                .all(|(m, s)| m.relative_path == s.relative_path
                    && m.visibility == s.visibility
                    && m.role == s.role
                    && m.label == s.label
                    && m.media_type == s.media_type
                    && m.sha256.len() == 64
                    && m.sha256
                        .bytes()
                        .all(|x| x.is_ascii_digit() || (b'a'..=b'f').contains(&x))),
        "output_inventory_stdout_binding_invalid"
    );
    Ok(inventory)
}
fn expected_selection_digest(request: &OutputInventoryRequest) -> Result<String> {
    #[derive(serde::Serialize)]
    #[serde(rename_all = "camelCase")]
    struct Selection<'a> {
        schema_version: &'a str,
        files: &'a [SelectedOutput],
        max_files: usize,
        max_bytes: u64,
    }
    canonical_document_digest(&Selection {
        schema_version: &request.schema_version,
        files: &request.files,
        max_files: request.max_files,
        max_bytes: request.max_bytes,
    })
    .map_err(|_| anyhow!("output_inventory_stdout_binding_invalid"))
}

fn dns(v: &str) -> bool {
    !v.is_empty()
        && v.len() <= 63
        && !v.starts_with('-')
        && !v.ends_with('-')
        && v.bytes()
            .all(|x| x.is_ascii_lowercase() || x.is_ascii_digit() || x == b'-')
}
fn valid_uid(v: &str) -> bool {
    v.len() >= 8 && v.len() <= 128 && v.bytes().all(|x| x.is_ascii_alphanumeric() || x == b'-')
}
fn pinned_image(v: &str) -> bool {
    v.contains("@sha256:")
        && v.rsplit_once("@sha256:").is_some_and(|(_, d)| {
            d.len() == 64
                && d.bytes()
                    .all(|x| x.is_ascii_hexdigit() && !x.is_ascii_uppercase())
        })
}
fn label_key(v: &str) -> bool {
    !v.is_empty() && v.len() <= 253 && !v.contains([' ', '\0'])
}
fn label_value(v: &str) -> bool {
    !v.is_empty()
        && v.len() <= 63
        && v.bytes()
            .all(|x| x.is_ascii_alphanumeric() || matches!(x, b'.' | b'_' | b'-'))
}
