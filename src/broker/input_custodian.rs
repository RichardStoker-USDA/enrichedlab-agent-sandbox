//! One-shot trusted input preparation. The worker's notice is a lookup key;
//! the database supplies task authority and a private site plan supplies data
//! authority. No model-selected files, commands, or Kubernetes manifests enter.
use super::{
    admission::open_bounded_record,
    admission_publication::{
        AdmissionPublicationConfig, AdmissionPublicationReceipt, publish_private,
    },
    input_completion::{InputCopyObservationTransport, ObservedInputSeal},
    input_finalization::{InputFinalizationConfig, InputFinalizationTransport, InputFinalizer},
    input_publication::{
        InputPublicationConfig, InputPublicationGuard, InputPublicationTransport,
        InputSourceIdentity, InputSourceLeaseGuard, InputSourceObservationTransport,
        PreparedInputPublication, PublishedInputCopy,
    },
    input_request::{
        AdmittedCodeRef, InputPreparationConfig, InputPreparationRequest, PreparationTaskIdentity,
    },
    input_workload::{AuthorizedInputCopy, InputCopyAuthorization, parse_retention_until},
    model::ClaimedTask,
    publication_journal::{PublicationJournal, valid_private_root},
    sandbox::{BrokerLeaseFence, CanonicalSandbox, SandboxAuthorization, SandboxBinding},
    store::{AttemptInput, Store},
};
use crate::custody::CopySealRequest;
use anyhow::{Result, anyhow, ensure};
use async_trait::async_trait;
use serde::{Deserialize, de::DeserializeOwned};
use serde_json::{Map, Value, json};
use sha2::{Digest, Sha256};
use std::{
    io::Read,
    path::{Path, PathBuf},
    process::Stdio,
    time::Duration,
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    process::Command,
    time::{Instant, sleep, timeout},
};
use tokio_util::sync::CancellationToken;

const RECORD_LIMIT: usize = 1024 * 1024;
const EFFECT_TIMEOUT: Duration = Duration::from_secs(20);

#[derive(Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct InputCustodianConfig {
    pub requests: InputPreparationConfig,
    pub plans: InputPreparationConfig,
    pub publication: InputPublicationConfig,
    pub admission: AdmissionPublicationConfig,
    pub finalization_journal_root: PathBuf,
    pub max_wait_seconds: u32,
    pub poll_interval_millis: u32,
    /// The reviewed class and source of each approved source volume, keyed by its UID.
    /// A sealed copy of a listed source is recorded for the attempt before
    /// admission; a source without an entry is sealed as before and its attempt names
    /// `attempt_inputs_not_recorded`.
    #[serde(default)]
    pub input_sources: Vec<InputSourceClass>,
}

/// One approved source volume's input class and source description. `lab_database` is not
/// accepted here: a database extract records its own query, snapshot and WAL position where it
/// is taken, which a static entry cannot know.
#[derive(Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct InputSourceClass {
    pub pvc_uid: String,
    pub class: String,
    #[serde(default)]
    pub source: Map<String, Value>,
}
const INPUT_CLASSES: [&str; 4] = ["object_data", "paper", "public_reference", "prior_output"];
const RESOLVED_SOURCE_KEYS: [&str; 2] = ["source_pvc_uid", "source_run_id"];

/// An independently approved plan must match the exact task lease and binding.
/// Kept private because the copy request and renderer plan contain references.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct InputPlan {
    schema_version: String,
    task: PreparationTaskIdentity,
    binding: SandboxBinding,
    source: InputSourceIdentity,
    copy_request_secret_name: String,
    copy: CopySealRequest,
    custody_retention_until: String,
    renderer_plan: Value,
    /// Digest-only reference for a reviewed template's admitted block. It must
    /// match the notice; the bytes come from the broker for the current lease.
    #[serde(default)]
    admitted_code: Option<AdmittedCodeRef>,
}

#[async_trait]
pub trait CustodyTaskSource: BrokerLeaseFence {
    async fn lookup_custody_task(
        &self,
        request: &InputPreparationRequest,
    ) -> Result<Option<ClaimedTask>>;
    /// Exact admitted bytes for the notice's current lease. A source without
    /// this capability returns none, and a plan that names a block then fails.
    async fn custody_admitted_code(
        &self,
        _request: &InputPreparationRequest,
    ) -> Result<Option<(AdmittedCodeRef, String)>> {
        Ok(None)
    }
    /// Records one sealed input copy for the notice's current lease (migration 019). A
    /// source without attempt records refuses, so a configured class is never dropped.
    async fn record_sealed_input(
        &self,
        _request: &InputPreparationRequest,
        _input: &AttemptInput<'_>,
    ) -> Result<()> {
        Err(anyhow!("input_custodian_input_record_unsupported"))
    }
}
#[async_trait]
impl CustodyTaskSource for Store {
    async fn lookup_custody_task(
        &self,
        request: &InputPreparationRequest,
    ) -> Result<Option<ClaimedTask>> {
        let t = request.task();
        self.custody_task(
            t.task_id,
            request.worker_id(),
            t.attempt,
            t.lease_generation,
        )
        .await
        .map_err(|_| anyhow!("input_custodian_task_lookup_failed"))
    }
    async fn custody_admitted_code(
        &self,
        request: &InputPreparationRequest,
    ) -> Result<Option<(AdmittedCodeRef, String)>> {
        let t = request.task();
        self.custody_admitted_code(
            t.task_id,
            request.worker_id(),
            t.attempt,
            t.lease_generation,
        )
        .await
        .map_err(|_| anyhow!("input_custodian_admitted_code_lookup_failed"))
    }
    async fn record_sealed_input(
        &self,
        request: &InputPreparationRequest,
        input: &AttemptInput<'_>,
    ) -> Result<()> {
        let t = request.task();
        self.record_attempt_input(
            t.task_id,
            request.worker_id(),
            t.attempt,
            t.lease_generation,
            input,
        )
        .await
        .map_err(|_| anyhow!("input_custodian_input_record_failed"))
    }
}

/// This is a trusted operator adapter, never an executable supplied by a task.
/// The caller bounds and validates the result before cleanup or admission.
#[async_trait]
pub trait CustodianRenderer: Send + Sync {
    async fn render(&self, request: &[u8], cancellation: &CancellationToken) -> Result<Vec<u8>>;
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct RenderResult {
    schema_version: String,
    authorization: SandboxAuthorization,
    sandbox: Value,
}

pub struct InputCustodian {
    config: InputCustodianConfig,
}
impl InputCustodian {
    pub fn new(config: InputCustodianConfig) -> Result<Self> {
        let roots = [
            &config.requests.root,
            &config.plans.root,
            &config.publication.journal_root,
            &config.publication.custody_root,
            &config.admission.admission_root,
            &config.admission.custody_root,
            &config.finalization_journal_root,
        ];
        for (i, root) in roots.iter().enumerate() {
            ensure!(valid_private_root(root), "input_custodian_root_invalid");
            for other in &roots[..i] {
                ensure!(
                    !root.starts_with(other) && !other.starts_with(root),
                    "input_custodian_roots_overlap"
                );
            }
        }
        ensure!(
            (1..=5000).contains(&config.poll_interval_millis)
                && (1..=3600).contains(&config.max_wait_seconds)
                && config.poll_interval_millis <= config.max_wait_seconds * 1000
                && (30..=3600).contains(&config.admission.lease_seconds)
                && config.publication.writer_uid == config.admission.writer_uid,
            "input_custodian_config_invalid"
        );
        config.publication.workload.validate()?;
        let mut uids = std::collections::HashSet::new();
        for entry in &config.input_sources {
            ensure!(
                (1..=128).contains(&entry.pvc_uid.len())
                    && entry
                        .pvc_uid
                        .bytes()
                        .all(|c| c.is_ascii_alphanumeric() || c == b'-')
                    && uids.insert(entry.pvc_uid.as_str())
                    && INPUT_CLASSES.contains(&entry.class.as_str())
                    && RESOLVED_SOURCE_KEYS
                        .iter()
                        .all(|k| !entry.source.contains_key(*k))
                    && entry.source.values().all(Value::is_string)
                    && serde_json::to_vec(&entry.source).is_ok_and(|v| v.len() <= 3584)
                    && (entry.class != "object_data"
                        || ["source_version", "source_sha256"]
                            .iter()
                            .all(|k| entry.source.contains_key(*k))),
                "input_custodian_input_source_invalid"
            );
        }
        Ok(Self { config })
    }

    pub async fn run<L, T, R>(
        &self,
        request_name: &str,
        ledger: &L,
        transport: &T,
        renderer: &R,
        cancellation: &CancellationToken,
    ) -> Result<AdmissionPublicationReceipt>
    where
        L: CustodyTaskSource,
        T: InputPublicationTransport
            + InputSourceObservationTransport
            + InputCopyObservationTransport
            + InputFinalizationTransport,
        R: CustodianRenderer,
    {
        ensure!(!cancellation.is_cancelled(), "input_custodian_cancelled");
        let notice = InputPreparationRequest::load(
            &self.config.requests.root,
            request_name,
            self.config.requests.writer_uid,
        )?;
        let task = bounded(cancellation, ledger.lookup_custody_task(&notice))
            .await?
            .ok_or_else(|| anyhow!("input_custodian_task_not_current"))?;
        ensure!(
            notice.task().matches(&task),
            "input_custodian_task_mismatch"
        );
        let plan_bytes = read_private(
            &self.config.plans.root,
            request_name,
            self.config.plans.writer_uid,
        )?;
        let plan: InputPlan = decode(&plan_bytes)?;
        ensure!(
            plan.schema_version == "sandbox.enrichedlab.org/input-preparation-plan/v2"
                && plan.task == *notice.task()
                && plan.binding == *notice.binding(),
            "input_custodian_plan_mismatch"
        );
        ensure!(
            parse_retention_until(&plan.custody_retention_until)?
                > time::OffsetDateTime::now_utc()
                    + time::Duration::seconds(i64::from(task.max_run_seconds)),
            "input_custodian_retention_too_short"
        );
        ensure!(
            InputPublicationTransport::namespace(transport) == notice.binding().namespace
                && InputFinalizationTransport::namespace(transport) == notice.binding().namespace,
            "input_custodian_namespace_mismatch"
        );
        let auth = InputCopyAuthorization {
            task_id: task.id,
            attempt: task.attempt,
            lease_generation: task.lease_generation,
            run_id: plan.copy.run_id.clone(),
            tenant_id: plan.copy.tenant_id.clone(),
            source_run_name: plan.source.run_id.clone(),
            source_pvc_name: plan.source.pvc_name.clone(),
            principal_hash: notice.binding().principal_hash.clone(),
            profile_id: notice.binding().profile_id.clone(),
            namespace: notice.binding().namespace.clone(),
            input_digest: plan.copy.input_digest.clone(),
            custody_retention_until: plan.custody_retention_until,
        };
        ensure!(
            plan.admitted_code.as_ref() == notice.admitted_code(),
            "input_custodian_admitted_code_mismatch"
        );
        // The class comes from this custodian's own configuration for the source volume whose
        // UID the source guard verifies before the copy, never from the plan or a name.
        let input_source = self
            .config
            .input_sources
            .iter()
            .find(|s| s.pvc_uid == plan.source.pvc_uid)
            .cloned();
        let resolved_source = (plan.source.pvc_uid.clone(), plan.source.run_id.clone());
        let sealed_bytes = i64::try_from(plan.copy.files.iter().map(|f| f.size_bytes).sum::<u64>())
            .map_err(|_| anyhow!("input_custodian_input_size_invalid"))?;
        let mut copy = AuthorizedInputCopy::new(
            plan.source.run_id.clone(),
            plan.source.pvc_name.clone(),
            plan.copy_request_secret_name,
            plan.copy,
        )?;
        if let Some(reference) = &plan.admitted_code {
            let (stored, content) = bounded(cancellation, ledger.custody_admitted_code(&notice))
                .await?
                .ok_or_else(|| anyhow!("input_custodian_admitted_code_unavailable"))?;
            ensure!(
                stored == *reference,
                "input_custodian_admitted_code_mismatch"
            );
            // The declared entry's size and SHA-256 are checked again here and in
            // the copy seal; bytes that differ from the plan are never staged.
            copy = copy.with_admitted_file(reference.path.clone(), content)?;
        }
        let prepared = PreparedInputPublication::prepare(
            self.config.publication.clone(),
            &task,
            notice.worker_id().into(),
            notice.binding(),
            &auth,
            &copy,
            plan.source,
        )?;
        let guard = InputSourceLeaseGuard {
            ledger,
            source: transport,
            lease_seconds: self.config.admission.lease_seconds,
        };
        bounded(cancellation, guard.check(&prepared)).await?;
        let scope = format!(
            "input-custodian-{}-{}-{}",
            task.id, task.attempt, task.lease_generation
        );
        let mut journal = PublicationJournal::create(
            &self.config.publication.journal_root,
            &format!("{scope}.jsonl"),
            self.config.publication.writer_uid,
        )
        .await?;
        journal.append(json!({"phase":"authorized","taskId":task.id,"attempt":task.attempt,
            "leaseGeneration":task.lease_generation,"planSha256":hex::encode(Sha256::digest(&plan_bytes))})).await?;
        let result = async {
            self.retain(&format!("{scope}.plan.json"), plan_bytes)
                .await?;
            let published = prepared.publish(&guard, transport, cancellation).await?;
            journal
                .append(json!({"phase":"copy_published","jobUid":published.expectation().job_uid}))
                .await?;
            let seal = self
                .wait_for_copy(&prepared, &published, &guard, transport, cancellation)
                .await?;
            if let Some(entry) = &input_source {
                let a = seal.attestation();
                let sealed = a
                    .manifest_digest
                    .strip_prefix("sha256:")
                    .ok_or_else(|| anyhow!("input_custodian_seal_digest_invalid"))?;
                let mut source = entry.source.clone();
                source.insert("source_pvc_uid".into(), json!(resolved_source.0));
                source.insert("source_run_id".into(), json!(resolved_source.1));
                let source = Value::Object(source);
                bounded(cancellation, guard.check(&prepared)).await?;
                bounded(
                    cancellation,
                    ledger.record_sealed_input(
                        &notice,
                        &AttemptInput {
                            input_ref: &a.input_ref,
                            class: &entry.class,
                            sealed_sha256: sealed,
                            size_bytes: sealed_bytes,
                            custody_transaction: &a.staging_id,
                            source: &source,
                        },
                    ),
                )
                .await?;
                journal
                    .append(json!({"phase":"input_recorded","class":entry.class,"sealedSha256":a.manifest_digest}))
                    .await?;
            }
            let render_request = render_request(&notice, &seal, plan.renderer_plan)?;
            // Retain exact original helper bytes before invoking external code.
            self.retain(
                &format!("{scope}.original-seal.json"),
                seal.original_receipt().to_vec(),
            )
            .await?;
            self.retain(
                &format!("{scope}.render-request.json"),
                render_request.clone(),
            )
            .await?;
            bounded(cancellation, guard.check(&prepared)).await?;
            let output =
                bounded(cancellation, renderer.render(&render_request, cancellation)).await?;
            ensure!(
                output.len() <= RECORD_LIMIT,
                "input_custodian_render_result_too_large"
            );
            let rendered: RenderResult = decode(&output)?;
            ensure!(
                rendered.schema_version == "sandbox.enrichedlab.org/custodian-render-result/v1",
                "input_custodian_render_schema_invalid"
            );
            let canonical = CanonicalSandbox::from_renderer(
                rendered.sandbox,
                rendered.authorization.input_digest.clone(),
            )?;
            self.retain(&format!("{scope}.render-result.json"), output)
                .await?;
            bounded(cancellation, guard.check(&prepared)).await?;
            let finalizer = InputFinalizer::new(InputFinalizationConfig {
                admission: self.config.admission.clone(),
                journal_root: self.config.finalization_journal_root.clone(),
                poll_interval: Duration::from_millis(self.config.poll_interval_millis.into()),
                max_wait: Duration::from_secs(self.config.max_wait_seconds.into()),
            })?;
            let receipt = finalizer
                .finalize(
                    &task,
                    notice.worker_id(),
                    notice.binding(),
                    &published,
                    &seal,
                    &canonical,
                    &rendered.authorization,
                    ledger,
                    transport,
                    cancellation,
                )
                .await?;
            journal
                .append(json!({"phase":"admitted","admissionSha256":receipt.admission_sha256}))
                .await?;
            Ok(receipt)
        }
        .await;
        if result.is_err() {
            journal
                .append(json!({"phase":"stopped","reason":"preparation_failed_recovery_required"}))
                .await?;
        }
        result
    }

    async fn retain(&self, name: &str, bytes: Vec<u8>) -> Result<()> {
        let root = self.config.publication.custody_root.clone();
        let uid = self.config.publication.writer_uid;
        let name = name.to_owned();
        timeout(
            EFFECT_TIMEOUT,
            tokio::task::spawn_blocking(move || publish_private(&root, &name, uid, &bytes)),
        )
        .await
        .map_err(|_| anyhow!("input_custodian_retention_deadline"))?
        .map_err(|_| anyhow!("input_custodian_retention_failed"))?
        .map_err(|_| anyhow!("input_custodian_retention_failed"))
    }

    async fn wait_for_copy<G: InputPublicationGuard, T: InputCopyObservationTransport>(
        &self,
        prepared: &PreparedInputPublication,
        published: &PublishedInputCopy,
        guard: &G,
        transport: &T,
        cancellation: &CancellationToken,
    ) -> Result<ObservedInputSeal> {
        let deadline = Instant::now() + Duration::from_secs(self.config.max_wait_seconds.into());
        tokio::time::timeout_at(deadline, async { loop {
            ensure!(Instant::now() < deadline, "input_custodian_copy_deadline");
            bounded(cancellation, guard.check(prepared)).await?;
            let e = published.expectation();
            let job = bounded(cancellation, transport.get_job(&e.namespace, &e.job_name)).await?;
            ensure!(
                job.pointer("/metadata/uid").and_then(Value::as_str) == Some(&e.job_uid)
                    && job.pointer("/metadata/name").and_then(Value::as_str) == Some(&e.job_name)
                    && job.pointer("/metadata/namespace").and_then(Value::as_str)
                        == Some(&e.namespace)
                    && job
                        .pointer("/metadata/deletionTimestamp")
                        .is_none_or(Value::is_null),
                "input_custodian_copy_replaced_or_deleted"
            );
            if let Some(conditions) = job.pointer("/status/conditions") {
                let conditions = conditions
                    .as_array()
                    .ok_or_else(|| anyhow!("input_custodian_copy_status_invalid"))?;
                ensure!(
                    !conditions.iter().any(|c| c["status"] == "True"
                        && matches!(c["type"].as_str(), Some("Failed" | "FailureTarget"))),
                    "input_custodian_copy_failed"
                );
                if conditions
                    .iter()
                    .any(|c| c["type"] == "Complete" && c["status"] == "True")
                {
                    return bounded(cancellation, published.observe_seal(transport)).await;
                }
            }
            ensure!(
                job.pointer("/status/failed")
                    .is_none_or(|v| v.as_u64() == Some(0)),
                "input_custodian_copy_failed"
            );
            tokio::select! {
                biased;
                _ = cancellation.cancelled() => return Err(anyhow!("input_custodian_cancelled")),
                _ = sleep(Duration::from_millis(self.config.poll_interval_millis.into()).min(deadline.saturating_duration_since(Instant::now()))) => {}
            }
        }}).await.map_err(|_| anyhow!("input_custodian_copy_deadline"))?
    }
}

fn render_request(
    notice: &InputPreparationRequest,
    seal: &ObservedInputSeal,
    plan: Value,
) -> Result<Vec<u8>> {
    let b = notice.binding();
    let original: Value = decode(seal.original_receipt())?;
    let bytes = serde_json::to_vec(&json!({
        "schemaVersion":"sandbox.enrichedlab.org/custodian-render-request/v1", "task":notice.task(),
        "binding":{"principalId":b.principal_id,"principalHash":b.principal_hash,"groupKey":b.group_key,
            "profileKey":b.profile_key,"profileId":b.profile_id,"labRef":b.lab_ref,
            "namespace":b.namespace,"runtimeClassName":b.runtime_class_name},
        "sealedInput":original,"plan":plan,
    })).map_err(|_| anyhow!("input_custodian_render_request_invalid"))?;
    ensure!(
        bytes.len() <= RECORD_LIMIT,
        "input_custodian_render_request_too_large"
    );
    Ok(bytes)
}

async fn bounded<F, T>(cancellation: &CancellationToken, future: F) -> Result<T>
where
    F: std::future::Future<Output = Result<T>>,
{
    tokio::select! {
        biased;
        _ = cancellation.cancelled() => Err(anyhow!("input_custodian_cancelled")),
        result = timeout(EFFECT_TIMEOUT, future) => result.map_err(|_| anyhow!("input_custodian_effect_deadline"))?,
    }
}

fn decode<T: DeserializeOwned>(bytes: &[u8]) -> Result<T> {
    serde_json::from_slice(bytes).map_err(|_| anyhow!("input_custodian_record_invalid"))
}

fn read_private(root: &Path, name: &str, uid: u32) -> Result<Vec<u8>> {
    ensure!(
        !name.is_empty() && !name.contains(['/', '\\', '\0']) && name != "." && name != "..",
        "input_custodian_record_name_invalid"
    );
    let file = open_bounded_record(root, name, uid, RECORD_LIMIT as u64)
        .map_err(|_| anyhow!("input_custodian_record_unavailable"))?;
    let mut bytes = Vec::new();
    file.take(RECORD_LIMIT as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| anyhow!("input_custodian_record_read_failed"))?;
    ensure!(
        bytes.len() <= RECORD_LIMIT,
        "input_custodian_record_too_large"
    );
    Ok(bytes)
}

/// Fixed executable and arguments are site configuration. Pin the entire
/// program/dependency tree in a read-only image; the digest checks its entrypoint.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CommandRendererConfig {
    pub program: PathBuf,
    pub sha256: String,
    pub owner_uid: u32,
    #[serde(default)]
    pub arguments: Vec<String>,
    pub timeout_seconds: u32,
}
pub struct CommandRenderer {
    config: CommandRendererConfig,
}
impl CommandRenderer {
    pub fn new(config: CommandRendererConfig) -> Result<Self> {
        ensure!(
            config.program.is_absolute()
                && (1..=15).contains(&config.timeout_seconds)
                && config.arguments.len() <= 16
                && config
                    .arguments
                    .iter()
                    .all(|v| v.len() <= 4096 && !v.contains('\0')),
            "input_custodian_renderer_config_invalid"
        );
        let renderer = Self { config };
        renderer.verify_program()?;
        Ok(renderer)
    }
    #[cfg(unix)]
    fn verify_program(&self) -> Result<()> {
        use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
        let file = std::fs::OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC)
            .open(&self.config.program)
            .map_err(|_| anyhow!("input_custodian_renderer_unavailable"))?;
        let m = file
            .metadata()
            .map_err(|_| anyhow!("input_custodian_renderer_unavailable"))?;
        ensure!(
            m.is_file()
                && m.uid() == self.config.owner_uid
                && m.mode() & 0o022 == 0
                && m.mode() & 0o111 != 0
                && m.len() <= 8 * 1024 * 1024,
            "input_custodian_renderer_untrusted"
        );
        let mut bytes = Vec::new();
        file.take(8 * 1024 * 1024 + 1)
            .read_to_end(&mut bytes)
            .map_err(|_| anyhow!("input_custodian_renderer_unavailable"))?;
        ensure!(
            bytes.len() <= 8 * 1024 * 1024
                && hex::encode(Sha256::digest(&bytes)) == self.config.sha256,
            "input_custodian_renderer_digest_mismatch"
        );
        Ok(())
    }
    #[cfg(not(unix))]
    fn verify_program(&self) -> Result<()> {
        Err(anyhow!("input_custodian_requires_unix"))
    }
}
#[async_trait]
impl CustodianRenderer for CommandRenderer {
    async fn render(&self, request: &[u8], cancellation: &CancellationToken) -> Result<Vec<u8>> {
        ensure!(
            !cancellation.is_cancelled() && request.len() <= RECORD_LIMIT,
            "input_custodian_render_cancelled_or_oversize"
        );
        self.verify_program()?;
        let mut command = Command::new(&self.config.program);
        command
            .args(&self.config.arguments)
            .env_clear()
            .env("PATH", "/usr/bin:/bin")
            .current_dir("/")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .kill_on_drop(true);
        #[cfg(unix)]
        command.process_group(0);
        let mut child = command
            .spawn()
            .map_err(|_| anyhow!("input_custodian_renderer_spawn_failed"))?;
        #[cfg(unix)]
        let mut descendants = RendererProcessGroup(child.id().map(|id| id as i32));
        let mut stdin = child
            .stdin
            .take()
            .ok_or_else(|| anyhow!("input_custodian_renderer_pipe_failed"))?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| anyhow!("input_custodian_renderer_pipe_failed"))?;
        let exchange = async {
            let write = async move {
                stdin.write_all(request).await?;
                stdin.shutdown().await
            };
            let read = async {
                let mut output = Vec::new();
                stdout
                    .take(RECORD_LIMIT as u64 + 1)
                    .read_to_end(&mut output)
                    .await?;
                ensure!(
                    output.len() <= RECORD_LIMIT,
                    "input_custodian_renderer_output_oversize"
                );
                Ok::<_, anyhow::Error>(output)
            };
            let (_, output) =
                tokio::try_join!(async { write.await.map_err(anyhow::Error::from) }, read)?;
            ensure!(
                child.wait().await?.success(),
                "input_custodian_renderer_failed"
            );
            Ok(output)
        };
        let result = tokio::select! {
            biased;
            _ = cancellation.cancelled() => Err(anyhow!("input_custodian_renderer_cancelled")),
            result = timeout(Duration::from_secs(self.config.timeout_seconds.into()), exchange) =>
                result.map_err(|_| anyhow!("input_custodian_renderer_deadline"))?
                    .map_err(|_| anyhow!("input_custodian_renderer_failed")),
        };
        if result.is_err() {
            let _ = child.kill().await;
        } else {
            #[cfg(unix)]
            {
                descendants.0 = None;
            }
        }
        result
    }
}

// A cancelled render future must also stop the reviewed renderer's subprocess,
// including when an outer deadline drops this future before its own timeout.
#[cfg(unix)]
struct RendererProcessGroup(Option<i32>);
#[cfg(unix)]
impl Drop for RendererProcessGroup {
    fn drop(&mut self) {
        if let Some(id) = self.0 {
            unsafe {
                libc::kill(-id, libc::SIGKILL);
            }
        }
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct InputCustodianServiceConfig {
    pub schema_version: String,
    pub database_tenant: String,
    pub database_dsn_file: PathBuf,
    pub kubernetes: InputCustodianKubernetesConfig,
    pub custodian: InputCustodianConfig,
    pub renderer: CommandRendererConfig,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct InputCustodianKubernetesConfig {
    pub origin: String,
    pub namespace: String,
    pub token_file: PathBuf,
    pub ca_file: PathBuf,
}
impl InputCustodianServiceConfig {
    pub fn load(path: &Path, owner_uid: u32) -> Result<Self> {
        let parent = path
            .parent()
            .ok_or_else(|| anyhow!("input_custodian_config_path_invalid"))?;
        let name = path
            .file_name()
            .and_then(|v| v.to_str())
            .ok_or_else(|| anyhow!("input_custodian_config_path_invalid"))?;
        let config: Self = decode(&read_private(parent, name, owner_uid)?)?;
        ensure!(
            config.schema_version == "sandbox.enrichedlab.org/input-custodian/v1"
                && config.database_dsn_file.is_absolute()
                && (2..=64).contains(&config.database_tenant.len())
                && config
                    .database_tenant
                    .bytes()
                    .all(|c| c.is_ascii_lowercase()
                        || c.is_ascii_digit()
                        || matches!(c, b'-' | b'_')),
            "input_custodian_service_config_invalid"
        );
        Ok(config)
    }
}
