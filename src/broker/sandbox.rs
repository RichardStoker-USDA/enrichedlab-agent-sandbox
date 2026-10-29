//! Direct `sandbox.enrichedlab.org/run/v3` adapter.  The broker accepts only the
//! reviewed renderer output; it never derives a Kubernetes workload from task text.
use super::{digest, input_request::AdmittedCodeRef, model::ClaimedTask, store::Store};
use crate::custody::canonical_document_digest;
use anyhow::{Context, Result, anyhow, bail};
use async_trait::async_trait;
use reqwest::{Client, Method, Url};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    collections::HashSet,
    path::{Path, PathBuf},
    time::Duration,
};
use tokio_util::sync::CancellationToken;
use uuid::Uuid;
const API: &str = "agents.x-k8s.io/v1beta1";
const KIND: &str = "Sandbox";
const CONNECT: Duration = Duration::from_secs(5);
const TIMEOUT: Duration = Duration::from_secs(15);
/// A followed Pod log with no new output for this long is reopened from its last line.
const LOG_READ: Duration = Duration::from_secs(120);
#[derive(Clone, Debug, PartialEq, Eq, serde::Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SandboxBinding {
    pub principal_id: Uuid,
    pub principal_hash: String,
    pub group_key: String,
    pub profile_key: String,
    pub profile_id: String,
    pub lab_ref: String,
    pub namespace: String,
    pub runtime_class_name: String,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CanonicalSandbox {
    manifest: Value,
    input_digest: String,
}
impl CanonicalSandbox {
    pub fn from_renderer(manifest: Value, input_digest: String) -> Result<Self> {
        if !sha(&input_digest) {
            bail!("canonical Sandbox input digest is invalid")
        }
        Ok(Self {
            manifest,
            input_digest,
        })
    }
    pub fn manifest(&self) -> &Value {
        &self.manifest
    }
}
#[derive(Clone, Debug, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SandboxAuthorization {
    pub run_id: String,
    pub principal_hash: String,
    pub profile_id: String,
    pub namespace: String,
    pub executable_digest: String,
    pub input_digest: String,
    pub staging_digest: String,
    pub manifest_digest: String,
    pub attestation_digest: String,
}
#[derive(Clone, Debug, Default)]
pub struct SandboxBindingCatalog {
    bindings: Vec<SandboxBinding>,
}
impl SandboxBindingCatalog {
    pub fn new(bindings: Vec<SandboxBinding>) -> Result<Self> {
        let mut selectors = HashSet::new();
        for b in &bindings {
            valid_binding(b)?;
            if !selectors.insert((b.principal_id, b.group_key.clone(), b.profile_key.clone())) {
                bail!("duplicate server-owned Sandbox binding selector")
            }
        }
        Ok(Self { bindings })
    }
    pub fn find(&self, t: &ClaimedTask) -> Option<&SandboxBinding> {
        self.bindings.iter().find(|b| {
            b.principal_id == t.principal_id
                && b.group_key == t.group_key
                && b.profile_key == t.profile_key
        })
    }
    pub fn resolve(&self, t: &ClaimedTask) -> Result<&SandboxBinding> {
        self.find(t)
            .context("no server-owned direct Sandbox binding matches this broker task")
    }
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SandboxHandle {
    pub task_id: Uuid,
    pub worker_id: String,
    pub lease_generation: i64,
    pub namespace: String,
    pub sandbox_name: String,
    pub sandbox_uid: String,
    pub sandbox_generation: i64,
    pub lab_ref: String,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RuntimeObservation {
    pub sandbox_uid: String,
    pub sandbox_generation: i64,
    pub pod_name: String,
    pub pod_uid: String,
    pub runtime_class_name: String,
}
/// Minted only from a current controller condition and the exact terminated Pod.
/// It proves process completion, not scientific validity or PVC writer detachment.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RuntimeCompletion {
    handle: SandboxHandle,
    observation: RuntimeObservation,
    exit_code: i32,
    finished_at: time::OffsetDateTime,
    succeeded: bool,
}
impl RuntimeCompletion {
    pub fn observation(&self) -> &RuntimeObservation {
        &self.observation
    }
    pub fn exit_code(&self) -> i32 {
        self.exit_code
    }
    pub fn finished_at(&self) -> time::OffsetDateTime {
        self.finished_at
    }
    pub fn succeeded(&self) -> bool {
        self.succeeded
    }
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RetainedArtifact {
    pub visibility: String,
    pub role: String,
    pub label: String,
    pub relative_path: String,
    pub media_type: String,
    pub size_bytes: i64,
    pub sha256: String,
}
#[async_trait]
pub trait SandboxTransport: Send + Sync {
    async fn create_sandbox(&self, ns: &str, obj: &Value) -> Result<Value>;
    async fn get_sandbox(&self, ns: &str, name: &str) -> Result<Value>;
    async fn get_pod(&self, ns: &str, name: &str) -> Result<Option<Value>>;
    async fn get_pvc(&self, ns: &str, name: &str) -> Result<Value>;
    async fn delete_sandbox(&self, ns: &str, name: &str, uid: &str) -> Result<()>;
}
pub struct KubernetesHttpTransport {
    client: Client,
    /// For Pod logs: no total timeout, since a followed log stays open while the runner works.
    logs: Client,
    api: Url,
    namespace: String,
    token: PathBuf,
}
impl KubernetesHttpTransport {
    pub fn from_service_account(
        api: Url,
        namespace: String,
        token: PathBuf,
        ca: &Path,
    ) -> Result<Self> {
        dns(&namespace, "namespace")?;
        if api.scheme() != "https"
            || api.host_str().is_none()
            || !api.username().is_empty()
            || api.password().is_some()
            || api.path() != "/"
            || api.query().is_some()
            || api.fragment().is_some()
        {
            bail!("Kubernetes API server reference must be an HTTPS origin")
        }
        let _ = rustls::crypto::ring::default_provider().install_default();
        let cert = reqwest::Certificate::from_pem(
            &std::fs::read(ca).map_err(|_| anyhow!("Kubernetes CA reference is unreadable"))?,
        )
        .map_err(|_| anyhow!("Kubernetes CA reference is invalid"))?;
        if !token.is_absolute() {
            bail!("Kubernetes service-account token reference must be absolute")
        }
        Ok(Self {
            client: Client::builder()
                .connect_timeout(CONNECT)
                .timeout(TIMEOUT)
                .redirect(reqwest::redirect::Policy::none())
                .add_root_certificate(cert.clone())
                .build()
                .map_err(|_| anyhow!("Kubernetes HTTP client configuration is invalid"))?,
            logs: Client::builder()
                .connect_timeout(CONNECT)
                .read_timeout(LOG_READ)
                .redirect(reqwest::redirect::Policy::none())
                .add_root_certificate(cert)
                .build()
                .map_err(|_| anyhow!("Kubernetes HTTP client configuration is invalid"))?,
            api,
            namespace,
            token,
        })
    }
    fn path(&self, r: &str, n: Option<&str>) -> Result<String> {
        dns(&self.namespace, "namespace")?;
        if let Some(x) = n {
            dns(x, "resource")?
        }
        let prefix = match r {
            "sandboxes" => "apis/agents.x-k8s.io/v1beta1",
            "pods" | "persistentvolumeclaims" => "api/v1",
            _ => bail!("unsupported Kubernetes resource"),
        };
        let p = format!("{prefix}/namespaces/{}/{}", self.namespace, r);
        Ok(n.map_or(p.clone(), |x| format!("{p}/{x}")))
    }
    fn ns(&self, n: &str) -> Result<()> {
        if n != self.namespace {
            bail!("Kubernetes transport namespace differs from the bound namespace")
        }
        Ok(())
    }
    async fn token(&self) -> Result<String> {
        let x = tokio::fs::read_to_string(&self.token)
            .await
            .map_err(|_| anyhow!("Kubernetes service-account token reference is unreadable"))?;
        let x = x.trim();
        if x.is_empty() || x.len() > 16384 {
            bail!("Kubernetes service-account token is invalid")
        }
        Ok(x.into())
    }
    async fn request(
        &self,
        m: Method,
        p: String,
        b: Option<&Value>,
        optional: bool,
    ) -> Result<Option<Value>> {
        let u = self
            .api
            .join(&p)
            .map_err(|_| anyhow!("Kubernetes API path is invalid"))?;
        let mut q = self.client.request(m, u).bearer_auth(self.token().await?);
        if let Some(b) = b {
            q = q.json(b)
        }
        let r = q
            .send()
            .await
            .map_err(|_| anyhow!("Kubernetes API request failed"))?;
        if optional && r.status() == reqwest::StatusCode::NOT_FOUND {
            return Ok(None);
        }
        if !r.status().is_success() {
            bail!(
                "Kubernetes API request failed with HTTP {}",
                r.status().as_u16()
            )
        }
        let b = r
            .bytes()
            .await
            .map_err(|_| anyhow!("Kubernetes API response read failed"))?;
        serde_json::from_slice(&b)
            .map(Some)
            .map_err(|_| anyhow!("Kubernetes API response JSON is invalid"))
    }
}
impl KubernetesHttpTransport {
    /// The runner's log as it is written: timestamped lines from the start of `since`'s second, following while the
    /// container runs when `follow` is set. Needs `get` on `pods/log` in the bound namespace.
    pub async fn pod_log(
        &self,
        n: &str,
        pod: &str,
        container: &str,
        since: Option<time::OffsetDateTime>,
        follow: bool,
    ) -> Result<super::live_output::LogStream> {
        use futures_util::StreamExt;
        self.ns(n)?;
        dns(container, "container")?;
        let mut url = self
            .api
            .join(&format!("{}/log", self.path("pods", Some(pod))?))
            .map_err(|_| anyhow!("Kubernetes API path is invalid"))?;
        url.query_pairs_mut()
            .append_pair("container", container)
            .append_pair("timestamps", "true")
            .append_pair("follow", if follow { "true" } else { "false" });
        if let Some(since) = since {
            let since = since
                .to_offset(time::UtcOffset::UTC)
                .replace_nanosecond(0)
                .map_err(|_| anyhow!("Pod log start is invalid"))?
                .format(&time::format_description::well_known::Rfc3339)
                .map_err(|_| anyhow!("Pod log start is invalid"))?;
            url.query_pairs_mut().append_pair("sinceTime", &since);
        }
        let response = self
            .logs
            .get(url)
            .bearer_auth(self.token().await?)
            .send()
            .await
            .map_err(|_| anyhow!("Kubernetes Pod log request failed"))?;
        if !response.status().is_success() {
            bail!(
                "Kubernetes Pod log request failed with HTTP {}",
                response.status().as_u16()
            )
        }
        Ok(Box::pin(response.bytes_stream().map(|chunk| {
            chunk
                .map(|bytes| bytes.to_vec())
                .map_err(|_| anyhow!("Kubernetes Pod log read failed"))
        })))
    }
}
#[async_trait]
impl SandboxTransport for KubernetesHttpTransport {
    async fn create_sandbox(&self, n: &str, o: &Value) -> Result<Value> {
        self.ns(n)?;
        Ok(self
            .request(Method::POST, self.path("sandboxes", None)?, Some(o), false)
            .await?
            .unwrap())
    }
    async fn get_sandbox(&self, n: &str, x: &str) -> Result<Value> {
        self.ns(n)?;
        Ok(self
            .request(Method::GET, self.path("sandboxes", Some(x))?, None, false)
            .await?
            .unwrap())
    }
    async fn get_pod(&self, n: &str, x: &str) -> Result<Option<Value>> {
        self.ns(n)?;
        self.request(Method::GET, self.path("pods", Some(x))?, None, true)
            .await
    }
    async fn get_pvc(&self, n: &str, x: &str) -> Result<Value> {
        self.ns(n)?;
        Ok(self
            .request(
                Method::GET,
                self.path("persistentvolumeclaims", Some(x))?,
                None,
                false,
            )
            .await?
            .unwrap())
    }
    async fn delete_sandbox(&self, n: &str, x: &str, u: &str) -> Result<()> {
        self.ns(n)?;
        if u.is_empty() {
            bail!("Sandbox UID is missing for deletion")
        }
        self.request(
            Method::DELETE,
            self.path("sandboxes", Some(x))?,
            Some(&json!({"apiVersion":"v1","kind":"DeleteOptions","preconditions":{"uid":u}})),
            false,
        )
        .await?;
        Ok(())
    }
}
#[async_trait]
pub trait BrokerLeaseFence: Send + Sync {
    async fn mark_running(&self, t: Uuid, w: &str, g: i64) -> Result<bool>;
    async fn cancel_requested(&self, t: Uuid, w: &str, g: i64) -> Result<bool>;
    async fn renew_lease(&self, t: Uuid, w: &str, g: i64, s: i32) -> Result<bool>;
    async fn register_artifact(&self, t: Uuid, w: &str, g: i64, a: &RetainedArtifact)
    -> Result<()>;
    async fn finish_task(
        &self,
        t: Uuid,
        w: &str,
        g: i64,
        p: bool,
        c: &str,
        e: &str,
    ) -> Result<bool>;
    /// Records, under the current lease, the image this attempt is about to run and the
    /// canonical digest of the exact Sandbox object it submits. A fence that
    /// keeps no attempt records accepts it.
    async fn record_execution(
        &self,
        _t: Uuid,
        _w: &str,
        _g: i64,
        _image_digest: &str,
        _sandbox_digest: &str,
    ) -> Result<()> {
        Ok(())
    }
    /// Records a result package's digest for this lease. It never fails the
    /// task: a digest that cannot be recorded leaves a gap the coordinator reports.
    async fn record_digest(
        &self,
        _t: Uuid,
        _w: &str,
        _g: i64,
        _root: &Path,
        _a: &RetainedArtifact,
    ) {
    }
    /// Digest-only reference to code the broker admitted for this lease. Fixed
    /// recipes have none; the site preparer refuses a template without one.
    async fn admitted_code(
        &self,
        _t: Uuid,
        _w: &str,
        _attempt: i32,
        _g: i64,
    ) -> Result<Option<AdmittedCodeRef>> {
        Ok(None)
    }
}
#[async_trait]
impl BrokerLeaseFence for Store {
    async fn mark_running(&self, t: Uuid, w: &str, g: i64) -> Result<bool> {
        self.mark_running(t, w, g).await.map_err(Into::into)
    }
    async fn record_execution(
        &self,
        t: Uuid,
        w: &str,
        g: i64,
        image_digest: &str,
        sandbox_digest: &str,
    ) -> Result<()> {
        // The Sandbox object binds the command, arguments, limits and input volumes the
        // attempt runs with; its digest is the attempt's parameter record.
        self.record_attempt_execution(t, w, g, image_digest, None, None, sandbox_digest)
            .await
            .map_err(Into::into)
    }
    async fn admitted_code(
        &self,
        t: Uuid,
        w: &str,
        attempt: i32,
        g: i64,
    ) -> Result<Option<AdmittedCodeRef>> {
        self.task_admitted_code(t, w, attempt, g)
            .await
            .map_err(Into::into)
    }
    async fn cancel_requested(&self, t: Uuid, w: &str, g: i64) -> Result<bool> {
        self.cancel_requested(t, w, g).await.map_err(Into::into)
    }
    async fn record_digest(&self, t: Uuid, w: &str, g: i64, root: &Path, a: &RetainedArtifact) {
        let (root, path, size, sha) = (
            root.to_path_buf(),
            a.relative_path.clone(),
            a.size_bytes as u64,
            a.sha256.clone(),
        );
        let built =
            tokio::task::spawn_blocking(move || digest::build(&root, t, &path, size, &sha)).await;
        let Ok(built) = built else {
            tracing::warn!(task_id=%t, lease_generation=g, event="result_digest_not_recorded", code="digest_build_failed");
            return;
        };
        match self
            .record_artifact_digest(
                t,
                w,
                g,
                &a.relative_path,
                &a.sha256,
                built.status,
                &built.value,
            )
            .await
        {
            Ok(_) => {
                tracing::info!(task_id=%t, lease_generation=g, event="result_digest_recorded", status=built.status)
            }
            Err(e) => {
                tracing::warn!(task_id=%t, lease_generation=g, event="result_digest_not_recorded", code=e.code)
            }
        }
    }
    async fn renew_lease(&self, t: Uuid, w: &str, g: i64, s: i32) -> Result<bool> {
        self.renew_task(t, w, g, s).await.map_err(Into::into)
    }
    async fn register_artifact(
        &self,
        t: Uuid,
        w: &str,
        g: i64,
        a: &RetainedArtifact,
    ) -> Result<()> {
        self.register_artifact(
            t,
            w,
            g,
            &a.visibility,
            &a.role,
            &a.label,
            &a.relative_path,
            &a.media_type,
            a.size_bytes,
            &a.sha256,
        )
        .await
        .map(|_| ())
        .map_err(Into::into)
    }
    async fn finish_task(
        &self,
        t: Uuid,
        w: &str,
        g: i64,
        p: bool,
        c: &str,
        e: &str,
    ) -> Result<bool> {
        self.finish_task(t, w, g, p, c, e).await.map_err(Into::into)
    }
}
#[async_trait]
pub trait ArtifactRetainer: Send + Sync {
    async fn retain(
        &self,
        h: &SandboxHandle,
        cancellation: &CancellationToken,
    ) -> Result<Vec<RetainedArtifact>>;
    /// The durable artifact root the retained paths are relative to, when this retainer
    /// wrote the bytes locally. The worker reads result packages from it for their digests.
    fn retained_root(&self) -> Option<&Path> {
        None
    }
}
pub struct SandboxRuntime<T> {
    transport: T,
    bindings: SandboxBindingCatalog,
    lease_seconds: i32,
}
impl<T: SandboxTransport> SandboxRuntime<T> {
    pub fn new(transport: T, bindings: SandboxBindingCatalog, lease_seconds: i32) -> Result<Self> {
        if !(60..=900).contains(&lease_seconds) {
            bail!("Sandbox lease seconds are invalid")
        }
        Ok(Self {
            transport,
            bindings,
            lease_seconds,
        })
    }
    pub async fn start<L: BrokerLeaseFence>(
        &self,
        lease: &L,
        t: &ClaimedTask,
        w: &str,
        c: &CanonicalSandbox,
        a: &SandboxAuthorization,
    ) -> Result<SandboxHandle> {
        let b = self.bindings.resolve(t)?;
        let obj = build_sandbox(t, b, c, a)?;
        let input = self
            .transport
            .get_pvc(&b.namespace, &format!("input-{}", a.run_id))
            .await
            .context("sealed input PVC metadata read failed")?;
        validate_custody_pvc(&input, &a.run_id, "immutable-input", true, Some(&obj))?;
        let artifacts = self
            .transport
            .get_pvc(&b.namespace, &format!("artifacts-{}", a.run_id))
            .await
            .context("artifact PVC metadata read failed")?;
        validate_custody_pvc(
            &artifacts,
            &a.run_id,
            "durable-artifacts",
            false,
            Some(&obj),
        )?;
        if !lease.mark_running(t.id, w, t.lease_generation).await? {
            bail!("broker task lease fence was lost before Sandbox creation")
        }
        // What this attempt runs is recorded before it can run; without the record it does not.
        lease
            .record_execution(
                t.id,
                w,
                t.lease_generation,
                &a.executable_digest,
                &canonical_document_digest(&obj)?,
            )
            .await
            .context("attempt execution record failed before Sandbox creation")?;
        let sandbox_name = name(&obj, "Sandbox")?;
        let created = self
            .transport
            .create_sandbox(&b.namespace, &obj)
            .await
            .context("Sandbox creation failed")?;
        if name(&created, "Sandbox")? != sandbox_name || ns(&created, "Sandbox")? != b.namespace {
            bail!("created Sandbox identity differs from server-owned request")
        }
        Ok(SandboxHandle {
            task_id: t.id,
            worker_id: w.into(),
            lease_generation: t.lease_generation,
            namespace: b.namespace.clone(),
            sandbox_name,
            sandbox_uid: uid(&created, "Sandbox")?,
            sandbox_generation: generation(&created, "Sandbox")?,
            lab_ref: b.lab_ref.clone(),
        })
    }
    pub async fn observe_ready(
        &self,
        t: &ClaimedTask,
        h: &SandboxHandle,
    ) -> Result<RuntimeObservation> {
        let (s, pod) = self.sandbox_and_pod(t, h).await?;
        ready(&s, h.sandbox_generation, "Sandbox")?;
        let pod = pod.context("Sandbox Pod is absent")?;
        if ptr(&pod, "/status/phase", "Pod phase")? != "Running" {
            bail!("Sandbox Pod phase differs")
        }
        pod_observation(&pod, h, self.bindings.resolve(t)?)
    }
    /// Capture the first Pod UID even when a short task has already completed.
    /// A dispatcher retains this observation and never silently rebinds its Pod.
    pub async fn observe_pod(
        &self,
        t: &ClaimedTask,
        h: &SandboxHandle,
    ) -> Result<Option<RuntimeObservation>> {
        let (_, pod) = self.sandbox_and_pod(t, h).await?;
        pod.as_ref()
            .map(|pod| pod_observation(pod, h, self.bindings.resolve(t)?))
            .transpose()
    }
    async fn sandbox_and_pod(
        &self,
        t: &ClaimedTask,
        h: &SandboxHandle,
    ) -> Result<(Value, Option<Value>)> {
        let binding = self.bindings.resolve(t)?;
        valid_handle(t, binding, h)?;
        let sandbox = self
            .transport
            .get_sandbox(&h.namespace, &h.sandbox_name)
            .await?;
        if uid(&sandbox, "Sandbox")? != h.sandbox_uid
            || generation(&sandbox, "Sandbox")? != h.sandbox_generation
            || name(&sandbox, "Sandbox")? != h.sandbox_name
            || ns(&sandbox, "Sandbox")? != h.namespace
        {
            bail!("Sandbox identity or generation changed")
        }
        let pod = self
            .transport
            .get_pod(&h.namespace, &h.sandbox_name)
            .await?;
        if let Some(pod) = &pod {
            pod_observation(pod, h, binding)?;
        }
        Ok((sandbox, pod))
    }
    /// Wait for Finished at the current Sandbox generation and a terminated
    /// single runner. Missing exit evidence must not authorize output cleanup.
    pub async fn observe_terminal(
        &self,
        t: &ClaimedTask,
        h: &SandboxHandle,
        expected: &RuntimeObservation,
    ) -> Result<Option<RuntimeCompletion>> {
        let (sandbox, pod) = self.sandbox_and_pod(t, h).await?;
        let pod = pod.context("previously observed Sandbox Pod is absent")?;
        let observation = pod_observation(&pod, h, self.bindings.resolve(t)?)?;
        if &observation != expected {
            bail!("Sandbox Pod identity changed after observation")
        }
        let phase = ptr(&pod, "/status/phase", "Pod phase")?;
        let (succeeded, reason) = match phase.as_str() {
            "Succeeded" => (true, "PodSucceeded"),
            "Failed" => (false, "PodFailed"),
            "Pending" | "Running" | "Unknown" => return Ok(None),
            _ => bail!("Sandbox Pod phase is unsupported"),
        };
        let finished = sandbox
            .pointer("/status/conditions")
            .and_then(Value::as_array)
            .is_some_and(|conditions| {
                conditions.iter().any(|condition| {
                    condition.get("type").and_then(Value::as_str) == Some("Finished")
                        && condition.get("status").and_then(Value::as_str) == Some("True")
                        && condition.get("observedGeneration").and_then(Value::as_i64)
                            == Some(h.sandbox_generation)
                        && condition.get("reason").and_then(Value::as_str) == Some(reason)
                })
            });
        if !finished {
            return Ok(None);
        }
        let containers = pod
            .pointer("/spec/containers")
            .and_then(Value::as_array)
            .context("Sandbox runner specification is missing")?;
        if containers.len() != 1
            || containers[0].get("name").and_then(Value::as_str) != Some("runner")
            || pod.pointer("/spec/restartPolicy").and_then(Value::as_str) != Some("Never")
            || ["/spec/initContainers", "/spec/ephemeralContainers"]
                .iter()
                .any(|path| {
                    pod.pointer(path)
                        .is_some_and(|value| !value.as_array().is_some_and(Vec::is_empty))
                })
        {
            bail!("Sandbox completion has unaccounted containers")
        }
        let statuses = pod
            .pointer("/status/containerStatuses")
            .and_then(Value::as_array)
            .context("Sandbox runner exit evidence is missing")?;
        if statuses.len() != 1 || statuses[0].get("name").and_then(Value::as_str) != Some("runner")
        {
            bail!("Sandbox runner exit evidence differs")
        }
        let state = statuses[0]
            .get("state")
            .and_then(Value::as_object)
            .context("Sandbox runner state is missing")?;
        if state.len() != 1 || !state.contains_key("terminated") {
            bail!("Sandbox runner termination is unproven")
        }
        let terminated = &state["terminated"];
        let exit_code = terminated
            .get("exitCode")
            .and_then(Value::as_i64)
            .and_then(|code| i32::try_from(code).ok())
            .context("Sandbox runner exit code is missing")?;
        let finished_at = terminated
            .get("finishedAt")
            .and_then(Value::as_str)
            .and_then(|value| {
                time::OffsetDateTime::parse(value, &time::format_description::well_known::Rfc3339)
                    .ok()
            })
            .filter(|value| *value > time::OffsetDateTime::UNIX_EPOCH)
            .context("Sandbox runner completion timestamp is missing")?;
        if succeeded && exit_code != 0 {
            bail!("Sandbox success conflicts with runner exit code")
        }
        Ok(Some(RuntimeCompletion {
            handle: h.clone(),
            observation,
            exit_code,
            finished_at,
            succeeded,
        }))
    }
    /// The dispatcher uses this entry point instead of inferring success from
    /// model text, process output, or an upstream Ready condition.
    pub async fn finish_observed<L: BrokerLeaseFence, R: ArtifactRetainer>(
        &self,
        lease: &L,
        ret: &R,
        t: &ClaimedTask,
        h: &SandboxHandle,
        completion: &RuntimeCompletion,
    ) -> Result<()> {
        if &completion.handle != h {
            bail!("Sandbox completion belongs to a different task or lease")
        }
        self.finish(
            lease,
            ret,
            t,
            h,
            completion.succeeded,
            if completion.succeeded {
                "sandbox_completed"
            } else {
                "sandbox_failed"
            },
            if completion.succeeded {
                ""
            } else {
                "sandbox_run_failed"
            },
        )
        .await
    }
    #[expect(
        clippy::too_many_arguments,
        reason = "artifact retention completion binds the lease, task, Sandbox handle, outcome class, and code independently"
    )]
    pub async fn finish<L: BrokerLeaseFence, R: ArtifactRetainer>(
        &self,
        lease: &L,
        ret: &R,
        t: &ClaimedTask,
        h: &SandboxHandle,
        passed: bool,
        class: &str,
        code: &str,
    ) -> Result<()> {
        let b = self.bindings.resolve(t)?;
        valid_handle(t, b, h)?;
        // A cancelled run still needs its files retained. Only loss of the
        // worker's lease revokes its authority to complete artifact custody.
        if !lease
            .renew_lease(t.id, &h.worker_id, h.lease_generation, self.lease_seconds)
            .await?
        {
            bail!("broker task custody was lost before artifact retention")
        }
        let cancellation = CancellationToken::new();
        let _cancel_on_drop = cancellation.clone().drop_guard();
        let retention = ret.retain(h, &cancellation);
        tokio::pin!(retention);
        let mut interval =
            tokio::time::interval(Duration::from_secs((self.lease_seconds / 3) as u64));
        interval.tick().await;
        let a = loop {
            tokio::select! {
                result = &mut retention => break result.context("artifact retention failed before Sandbox cleanup")?,
                _ = interval.tick() => {
                    let active = lease.renew_lease(t.id, &h.worker_id, h.lease_generation, self.lease_seconds).await;
                    if !matches!(active, Ok(true)) {
                        cancellation.cancel();
                        // Retainers must stop and discard partial files cooperatively.
                        let _ = (&mut retention).await;
                        bail!("broker task custody was lost during artifact retention")
                    }
                }
            }
        };
        if a.is_empty() {
            bail!("artifact retainer returned no custody metadata; refusing Sandbox cleanup")
        }
        for x in &a {
            valid_artifact(x)?;
            lease
                .register_artifact(t.id, &h.worker_id, h.lease_generation, x)
                .await?
        }
        if let Some(root) = ret.retained_root() {
            for x in a
                .iter()
                .filter(|x| x.role == "result" && x.visibility == "researcher")
            {
                lease
                    .record_digest(t.id, &h.worker_id, h.lease_generation, root, x)
                    .await;
            }
        }
        if !lease
            .renew_lease(t.id, &h.worker_id, h.lease_generation, self.lease_seconds)
            .await?
        {
            bail!("broker task lease fence was lost before Sandbox cleanup")
        }
        self.transport
            .delete_sandbox(&h.namespace, &h.sandbox_name, &h.sandbox_uid)
            .await?;
        if !lease
            .finish_task(t.id, &h.worker_id, h.lease_generation, passed, class, code)
            .await?
        {
            bail!("broker terminal lease fence was lost after artifact retention")
        }
        Ok(())
    }
}
pub fn build_sandbox(
    t: &ClaimedTask,
    b: &SandboxBinding,
    c: &CanonicalSandbox,
    a: &SandboxAuthorization,
) -> Result<Value> {
    valid_binding(b)?;
    if b.principal_id != t.principal_id
        || b.group_key != t.group_key
        || b.profile_key != t.profile_key
    {
        bail!("Sandbox binding does not match broker task")
    }
    valid_auth(a, b)?;
    let s = c.manifest();
    if s.get("apiVersion").and_then(Value::as_str) != Some(API)
        || s.get("kind").and_then(Value::as_str) != Some(KIND)
    {
        bail!("canonical Sandbox has unsupported API contract")
    }
    if name(s, "Sandbox")? != a.run_id || ns(s, "Sandbox")? != a.namespace {
        bail!("canonical Sandbox run identity differs from trusted dispatch")
    }
    contract(s, c, a, b)?;
    Ok(s.clone())
}
fn contract(
    s: &Value,
    c: &CanonicalSandbox,
    a: &SandboxAuthorization,
    b: &SandboxBinding,
) -> Result<()> {
    let l = s
        .pointer("/metadata/labels")
        .and_then(Value::as_object)
        .context("Sandbox labels are missing")?;
    let lab_hash = hex::encode(Sha256::digest(b.lab_ref.as_bytes()));
    for (k, v) in [
        ("sandbox.enrichedlab.org/contract-version", "v3"),
        ("sandbox.enrichedlab.org/run-id", a.run_id.as_str()),
        (
            "sandbox.enrichedlab.org/principal-hash",
            b.principal_hash.as_str(),
        ),
        ("sandbox.enrichedlab.org/profile", b.profile_id.as_str()),
        ("sandbox.enrichedlab.org/lab-hash", &lab_hash[..32]),
        (
            "sandbox.enrichedlab.org/runtime",
            b.runtime_class_name.as_str(),
        ),
    ] {
        if l.get(k).and_then(Value::as_str) != Some(v) {
            bail!("canonical Sandbox label binding differs")
        }
    }
    let an = s
        .pointer("/metadata/annotations")
        .and_then(Value::as_object)
        .context("Sandbox annotations are missing")?;
    for (k, v) in [
        (
            "sandbox.enrichedlab.org/executable-digest",
            &a.executable_digest,
        ),
        ("sandbox.enrichedlab.org/input-digest", &a.input_digest),
        (
            "sandbox.enrichedlab.org/input-staging-digest",
            &a.staging_digest,
        ),
        (
            "sandbox.enrichedlab.org/input-manifest-digest",
            &a.manifest_digest,
        ),
        (
            "sandbox.enrichedlab.org/input-attestation-digest",
            &a.attestation_digest,
        ),
    ] {
        if an.get(k).and_then(Value::as_str) != Some(v.as_str()) {
            bail!("canonical Sandbox sealed input binding differs")
        }
    }
    if c.input_digest != a.input_digest || s.pointer("/spec/volumeClaimTemplates").is_some() {
        bail!("canonical Sandbox input or compute-owned PVC contract differs")
    }
    if s.pointer("/spec/service").and_then(Value::as_bool) != Some(false)
        || s.pointer("/spec/shutdownPolicy").and_then(Value::as_str) != Some("Delete")
        || s.pointer("/spec/operatingMode").and_then(Value::as_str) != Some("Running")
        || s.pointer("/spec/shutdownTime")
            .and_then(Value::as_str)
            .is_none()
    {
        bail!("canonical Sandbox lifecycle differs")
    }
    let p = s
        .pointer("/spec/podTemplate/spec")
        .context("Sandbox Pod template is missing")?;
    if ptr(p, "/runtimeClassName", "Sandbox runtime")? != b.runtime_class_name
        || p.pointer("/automountServiceAccountToken")
            .and_then(Value::as_bool)
            != Some(false)
        || p.pointer("/activeDeadlineSeconds")
            .and_then(Value::as_i64)
            .filter(|x| *x > 0 && *x <= 14400)
            .is_none()
    {
        bail!("canonical Sandbox Pod contract differs")
    }
    let v = p
        .pointer("/volumes")
        .and_then(Value::as_array)
        .filter(|x| x.len() == 2)
        .context("Sandbox PVC references differ")?;
    let run = &a.run_id;
    for (n, claim, ro) in [
        ("input", format!("input-{run}"), true),
        ("artifacts", format!("artifacts-{run}"), false),
    ] {
        let x = v
            .iter()
            .find(|x| x.get("name").and_then(Value::as_str) == Some(n))
            .context("Sandbox required PVC reference is missing")?;
        if x.pointer("/persistentVolumeClaim/claimName")
            .and_then(Value::as_str)
            != Some(claim.as_str())
            || x.pointer("/persistentVolumeClaim/readOnly")
                .and_then(Value::as_bool)
                .unwrap_or(false)
                != ro
        {
            bail!("Sandbox PVC reference differs")
        }
    }
    Ok(())
}
pub(crate) fn validate_custody_pvc(
    pvc: &Value,
    run: &str,
    role: &str,
    sealed: bool,
    sandbox: Option<&Value>,
) -> Result<()> {
    let expected = if role == "immutable-input" {
        format!("input-{run}")
    } else {
        format!("artifacts-{run}")
    };
    if name(pvc, "custody PVC")? != expected {
        bail!("custody PVC name differs from direct Sandbox binding")
    }
    let labels = pvc
        .pointer("/metadata/labels")
        .and_then(Value::as_object)
        .context("custody PVC labels are missing")?;
    if labels
        .get("sandbox.enrichedlab.org/volume-role")
        .and_then(Value::as_str)
        != Some(role)
        || labels
            .get("sandbox.enrichedlab.org/run-id")
            .and_then(Value::as_str)
            != Some(run)
    {
        bail!("custody PVC role or run binding differs")
    }
    if sealed
        && (labels
            .get("sandbox.enrichedlab.org/seal-state")
            .and_then(Value::as_str)
            != Some("sealed")
            || labels
                .get("sandbox.enrichedlab.org/materialization-mode")
                .and_then(Value::as_str)
                .filter(|x| *x == "checksummed-copy" || *x == "csi-snapshot")
                .is_none())
    {
        bail!("input PVC is not sealed by an approved materialization mode")
    }
    if pvc
        .pointer("/metadata/ownerReferences")
        .and_then(Value::as_array)
        .is_some_and(|xs| {
            xs.iter().any(|x| {
                matches!(
                    x.get("kind").and_then(Value::as_str),
                    Some("Sandbox") | Some("Pod")
                )
            })
        })
    {
        bail!("custody PVC is compute-owned")
    }
    if let Some(s) = sandbox {
        // New staged admissions bind volume incarnations, not just names. Older
        // operator-staged records remain readable until their route is migrated.
        let annotations = s.pointer("/metadata/annotations");
        let input_uid = annotations.and_then(|a| a.get("sandbox.enrichedlab.org/input-pvc-uid"));
        let output_uid =
            annotations.and_then(|a| a.get("sandbox.enrichedlab.org/artifact-pvc-uid"));
        if input_uid.is_some() || output_uid.is_some() {
            let input_uid = input_uid
                .and_then(Value::as_str)
                .filter(|s| super::export_kubernetes::valid_uid(s))
                .context("bound input PVC UID is invalid")?;
            let output_uid = output_uid
                .and_then(Value::as_str)
                .filter(|s| super::export_kubernetes::valid_uid(s))
                .context("bound artifact PVC UID is invalid")?;
            let expected_uid = if sealed { input_uid } else { output_uid };
            if uid(pvc, "custody PVC")? != expected_uid
                || ns(pvc, "custody PVC")? != ns(s, "Sandbox")?
                || pvc
                    .pointer("/metadata/deletionTimestamp")
                    .is_some_and(|v| !v.is_null())
            {
                bail!("custody PVC incarnation differs from staged admission")
            }
            if sealed {
                for key in [
                    "sandbox.enrichedlab.org/input-digest",
                    "sandbox.enrichedlab.org/input-staging-digest",
                    "sandbox.enrichedlab.org/input-manifest-digest",
                    "sandbox.enrichedlab.org/input-attestation-digest",
                ] {
                    if pvc
                        .pointer("/metadata/annotations")
                        .and_then(|a| a.get(key))
                        .and_then(Value::as_str)
                        != annotations.and_then(|a| a.get(key)).and_then(Value::as_str)
                    {
                        bail!("custody PVC sealed digest differs from staged admission")
                    }
                }
            }
        }
        let volume = if sealed { "input" } else { "artifacts" };
        if !s
            .pointer("/spec/podTemplate/spec/volumes")
            .and_then(Value::as_array)
            .is_some_and(|xs| {
                xs.iter().any(|x| {
                    x.get("name").and_then(Value::as_str) == Some(volume)
                        && x.pointer("/persistentVolumeClaim/claimName")
                            .and_then(Value::as_str)
                            == Some(expected.as_str())
                })
            })
        {
            bail!("Sandbox does not reference custody PVC")
        }
    }
    Ok(())
}
fn valid_binding(b: &SandboxBinding) -> Result<()> {
    for (x, n) in [
        (&b.group_key, "group"),
        (&b.profile_key, "profile"),
        (&b.profile_id, "Sandbox profile"),
        (&b.namespace, "namespace"),
        (&b.runtime_class_name, "RuntimeClass"),
    ] {
        dns(x, n)?
    }
    if b.principal_hash.len() != 32
        || !b
            .principal_hash
            .bytes()
            .all(|x| x.is_ascii_hexdigit() && !x.is_ascii_uppercase())
        || !b.lab_ref.starts_with("lab://")
    {
        bail!("invalid server-owned Sandbox binding")
    }
    Ok(())
}
fn valid_auth(a: &SandboxAuthorization, b: &SandboxBinding) -> Result<()> {
    dns(&a.run_id, "run")?;
    if a.principal_hash != b.principal_hash
        || a.profile_id != b.profile_id
        || a.namespace != b.namespace
        || ![
            &a.executable_digest,
            &a.input_digest,
            &a.staging_digest,
            &a.manifest_digest,
            &a.attestation_digest,
        ]
        .into_iter()
        .all(|x| sha(x))
    {
        bail!("trusted canonical Sandbox authorization is invalid")
    }
    Ok(())
}
fn valid_handle(t: &ClaimedTask, b: &SandboxBinding, h: &SandboxHandle) -> Result<()> {
    if h.task_id != t.id
        || h.lease_generation != t.lease_generation
        || h.namespace != b.namespace
        || h.lab_ref != b.lab_ref
    {
        bail!("Sandbox handle is not bound to broker lease")
    }
    Ok(())
}
pub(crate) fn valid_artifact(a: &RetainedArtifact) -> Result<()> {
    if !matches!(a.visibility.as_str(), "researcher" | "operator")
        || !matches!(
            a.role.as_str(),
            "request" | "stdout" | "stderr" | "result" | "workspace" | "state" | "evidence"
        )
        || !(1..=256).contains(&a.label.chars().count())
        || a.label.contains('\0')
        || !(1..=1024).contains(&a.relative_path.chars().count())
        || a.relative_path.contains(['\\', '\0'])
        || a.relative_path
            .split('/')
            .any(|s| matches!(s, "" | "." | ".."))
        || !(1..=256).contains(&a.media_type.chars().count())
        || a.media_type.contains('\0')
        || a.size_bytes < 0
        || a.sha256.len() != 64
        || !a
            .sha256
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    {
        bail!("invalid retained artifact metadata")
    }
    Ok(())
}
fn sha(x: &str) -> bool {
    x.len() == 71
        && x.starts_with("sha256:")
        && x[7..]
            .bytes()
            .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
}
fn dns(x: &str, n: &str) -> Result<()> {
    if x.is_empty()
        || x.len() > 63
        || x.starts_with('-')
        || x.ends_with('-')
        || !x
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
    {
        bail!("invalid {n}")
    }
    Ok(())
}
fn ptr(x: &Value, p: &str, n: &str) -> Result<String> {
    x.pointer(p)
        .and_then(Value::as_str)
        .filter(|x| !x.is_empty())
        .map(str::to_owned)
        .with_context(|| format!("{n} is missing"))
}
fn uid(x: &Value, n: &str) -> Result<String> {
    ptr(x, "/metadata/uid", &format!("{n} UID"))
}
fn name(x: &Value, n: &str) -> Result<String> {
    ptr(x, "/metadata/name", &format!("{n} name"))
}
fn ns(x: &Value, n: &str) -> Result<String> {
    ptr(x, "/metadata/namespace", &format!("{n} namespace"))
}
fn generation(x: &Value, n: &str) -> Result<i64> {
    x.pointer("/metadata/generation")
        .and_then(Value::as_i64)
        .filter(|x| *x > 0)
        .with_context(|| format!("{n} generation is missing"))
}
fn ready(x: &Value, g: i64, n: &str) -> Result<()> {
    if !x
        .pointer("/status/conditions")
        .and_then(Value::as_array)
        .is_some_and(|xs| {
            xs.iter().any(|x| {
                x.get("type").and_then(Value::as_str) == Some("Ready")
                    && x.get("status").and_then(Value::as_str) == Some("True")
                    && x.get("observedGeneration").and_then(Value::as_i64) == Some(g)
            })
        })
    {
        bail!("{n} has no current Ready condition")
    }
    Ok(())
}
fn owner(x: &Value, k: &str, u: &str) -> bool {
    x.pointer("/metadata/ownerReferences")
        .and_then(Value::as_array)
        .is_some_and(|xs| {
            xs.iter().any(|x| {
                x.get("kind").and_then(Value::as_str) == Some(k)
                    && x.get("apiVersion").and_then(Value::as_str) == Some(API)
                    && x.get("uid").and_then(Value::as_str) == Some(u)
                    && x.get("controller").and_then(Value::as_bool) == Some(true)
            })
        })
}
fn pod_observation(
    pod: &Value,
    handle: &SandboxHandle,
    binding: &SandboxBinding,
) -> Result<RuntimeObservation> {
    if !owner(pod, "Sandbox", &handle.sandbox_uid)
        || name(pod, "Pod")? != handle.sandbox_name
        || ns(pod, "Pod")? != handle.namespace
        || ptr(pod, "/spec/runtimeClassName", "Pod runtime class")? != binding.runtime_class_name
    {
        bail!("Sandbox Pod identity, owner or runtime differs")
    }
    Ok(RuntimeObservation {
        sandbox_uid: handle.sandbox_uid.clone(),
        sandbox_generation: handle.sandbox_generation,
        pod_name: name(pod, "Pod")?,
        pod_uid: uid(pod, "Pod")?,
        runtime_class_name: binding.runtime_class_name.clone(),
    })
}
#[cfg(test)]
mod tests {
    use super::*;
    const CANONICAL: &str = r#"{"apiVersion":"agents.x-k8s.io/v1beta1","kind":"Sandbox","metadata":{"name":"run-20260731-0001","namespace":"sandbox-example-001","labels":{"sandbox.enrichedlab.org/contract-version":"v3","sandbox.enrichedlab.org/run-id":"run-20260731-0001","sandbox.enrichedlab.org/tenant":"example-001","sandbox.enrichedlab.org/principal-hash":"2f4b66b9706c1ef57d67e2cf9ad98127","sandbox.enrichedlab.org/lab-hash":"6de1b27ec512a286c893e3e6d3962455","sandbox.enrichedlab.org/tool-hash":"d5d66025fe1f8a97cd317d595df9d571","sandbox.enrichedlab.org/profile":"example-kata","sandbox.enrichedlab.org/runtime":"kata-clh","sandbox.enrichedlab.org/binding-hash":"65e79ccdba91abfff64a54dbec86ef3e","kueue.x-k8s.io/queue-name":"sandbox-runs"},"annotations":{"sandbox.enrichedlab.org/action-ref-hash":"1b86cdf870ef1fd9828c6e15ccea4cab","sandbox.enrichedlab.org/approval-ref-hash":"a966a2a901be71e681f65e0536b78f2a","sandbox.enrichedlab.org/ledger-ref-hash":"02985a8d1e14069c406f3c92f3d649ae","sandbox.enrichedlab.org/rollback-ref-hash":"9e2f9427aad943b88a6019de40561e84","sandbox.enrichedlab.org/evidence-ref-hash":"ef1b284d5d94c83c3bfa7ae5d39bfda0","sandbox.enrichedlab.org/trace-hash":"fce35a177a37a56edd273b461c0a2d37","sandbox.enrichedlab.org/executable-digest":"sha256:1111111111111111111111111111111111111111111111111111111111111111","sandbox.enrichedlab.org/input-digest":"sha256:3333333333333333333333333333333333333333333333333333333333333333","sandbox.enrichedlab.org/input-materialization":"checksummed-copy","sandbox.enrichedlab.org/input-staging-digest":"sha256:99809b43a45960e40b50596f177fdae04e24b399f0545c686018e72006b38b04","sandbox.enrichedlab.org/input-manifest-digest":"sha256:5555555555555555555555555555555555555555555555555555555555555555","sandbox.enrichedlab.org/input-attestation-digest":"sha256:6666666666666666666666666666666666666666666666666666666666666666","sandbox.enrichedlab.org/artifact-retention-until":"2026-08-01T00:00:00Z"}},"spec":{"service":false,"shutdownTime":"2026-07-31T23:05:00Z","shutdownPolicy":"Delete","operatingMode":"Running","podTemplate":{"metadata":{"labels":{"sandbox.enrichedlab.org/contract-version":"v3","sandbox.enrichedlab.org/run-id":"run-20260731-0001","sandbox.enrichedlab.org/tenant":"example-001","sandbox.enrichedlab.org/principal-hash":"2f4b66b9706c1ef57d67e2cf9ad98127","sandbox.enrichedlab.org/lab-hash":"6de1b27ec512a286c893e3e6d3962455","sandbox.enrichedlab.org/tool-hash":"d5d66025fe1f8a97cd317d595df9d571","sandbox.enrichedlab.org/profile":"example-kata","sandbox.enrichedlab.org/runtime":"kata-clh","sandbox.enrichedlab.org/binding-hash":"65e79ccdba91abfff64a54dbec86ef3e","kueue.x-k8s.io/queue-name":"sandbox-runs"}},"spec":{"runtimeClassName":"kata-clh","automountServiceAccountToken":false,"enableServiceLinks":false,"restartPolicy":"Never","activeDeadlineSeconds":300,"terminationGracePeriodSeconds":10,"securityContext":{"runAsNonRoot":true,"runAsUser":65532,"runAsGroup":65532,"fsGroup":65532,"fsGroupChangePolicy":"OnRootMismatch","seccompProfile":{"type":"RuntimeDefault"}},"containers":[{"name":"runner","image":"docker.io/library/busybox@sha256:7a3ebe5bfd1a4a19797d20b0c0bb39d44393e9a03fd852c0865b0f540d868df0","imagePullPolicy":"IfNotPresent","command":["/bin/sh"],"args":["-c","test -r /input/fixture && printf qualification > /artifacts/result"],"workingDir":"/tmp","resources":{"requests":{"cpu":"100m","memory":"128Mi"},"limits":{"cpu":"500m","memory":"512Mi"}},"securityContext":{"runAsNonRoot":true,"runAsUser":65532,"runAsGroup":65532,"allowPrivilegeEscalation":false,"readOnlyRootFilesystem":true,"capabilities":{"drop":["ALL"]},"seccompProfile":{"type":"RuntimeDefault"}},"volumeMounts":[{"name":"input","mountPath":"/input","readOnly":true},{"name":"artifacts","mountPath":"/artifacts"}]}],"volumes":[{"name":"input","persistentVolumeClaim":{"claimName":"input-run-20260731-0001","readOnly":true}},{"name":"artifacts","persistentVolumeClaim":{"claimName":"artifacts-run-20260731-0001"}}]}}}}"#;
    fn bind() -> SandboxBinding {
        SandboxBinding {
            principal_id: Uuid::nil(),
            principal_hash: "2f4b66b9706c1ef57d67e2cf9ad98127".into(),
            group_key: "research".into(),
            profile_key: "reference".into(),
            profile_id: "example-kata".into(),
            lab_ref: "lab://example/research".into(),
            namespace: "sandbox-example-001".into(),
            runtime_class_name: "kata-clh".into(),
        }
    }
    fn auth() -> SandboxAuthorization {
        SandboxAuthorization {
            run_id: "run-20260731-0001".into(),
            principal_hash: bind().principal_hash,
            profile_id: "example-kata".into(),
            namespace: "sandbox-example-001".into(),
            executable_digest: format!("sha256:{}", "1".repeat(64)),
            input_digest: format!("sha256:{}", "3".repeat(64)),
            staging_digest:
                "sha256:99809b43a45960e40b50596f177fdae04e24b399f0545c686018e72006b38b04".into(),
            manifest_digest: format!("sha256:{}", "5".repeat(64)),
            attestation_digest: format!("sha256:{}", "6".repeat(64)),
        }
    }
    #[test]
    fn canonical_renderer_direct_sandbox_binds_sealed_pvcs() {
        let v: Value = serde_json::from_str(CANONICAL).unwrap();
        let c = CanonicalSandbox::from_renderer(v.clone(), auth().input_digest.clone()).unwrap();
        let t = ClaimedTask {
            id: Uuid::nil(),
            principal_id: Uuid::nil(),
            group_key: "research".into(),
            profile_key: "reference".into(),
            objective: "".into(),
            logical_agent: "".into(),
            lease_generation: 1,
            attempt: 1,
            max_task_attempts: 1,
            max_provider_attempts: 1,
            harness: "".into(),
            thinking: None,
            route: "".into(),
            controller_profile: "".into(),
            gateway_profile: "".into(),
            image_ref: "".into(),
            cpu_limit: "".into(),
            memory_limit: "".into(),
            state_profile: "".into(),
            role: "".into(),
            max_run_seconds: 1,
        };
        assert_eq!(build_sandbox(&t, &bind(), &c, &auth()).unwrap(), v)
    }
    #[test]
    fn sealed_custody_metadata_rejects_compute_ownership() {
        let pvc = json!({"metadata":{"name":"input-run-20260731-0001","labels":{"sandbox.enrichedlab.org/volume-role":"immutable-input","sandbox.enrichedlab.org/run-id":"run-20260731-0001","sandbox.enrichedlab.org/seal-state":"sealed","sandbox.enrichedlab.org/materialization-mode":"checksummed-copy"}}});
        let sandbox: Value = serde_json::from_str(CANONICAL).unwrap();
        assert!(
            validate_custody_pvc(
                &pvc,
                "run-20260731-0001",
                "immutable-input",
                true,
                Some(&sandbox)
            )
            .is_ok()
        );
        let mut owned = pvc;
        owned["metadata"]["ownerReferences"] = json!([{"kind":"Sandbox"}]);
        assert!(
            validate_custody_pvc(
                &owned,
                "run-20260731-0001",
                "immutable-input",
                true,
                Some(&sandbox)
            )
            .is_err()
        );
    }
    #[test]
    fn rejects_compute_owned_pvc() {
        let mut v: Value = serde_json::from_str(CANONICAL).unwrap();
        v["spec"]["volumeClaimTemplates"] = json!([]);
        let c = CanonicalSandbox::from_renderer(v, auth().input_digest.clone()).unwrap();
        assert!(contract(c.manifest(), &c, &auth(), &bind()).is_err())
    }
}
