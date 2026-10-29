//! Trusted terminal custody connects approved output selections to the existing
//! authenticated exporter. Notices are lookup keys, never task authority.
use super::{
    admission::open_bounded_record,
    admission_publication::publish_private,
    export_identity::{ExportCredentialLifetime, issue_export_credentials},
    export_publication::{
        ExportObjectReceipt, ExportPublicationConfig, ExportPublicationTransport,
        ObservedLeaseGuard, PreparedExportPublication,
    },
    export_transport::ExportScope,
    input_custodian::InputCustodianKubernetesConfig,
    input_request::{InputPreparationConfig, PreparationTaskIdentity},
    model::ClaimedTask,
    output_workload::{
        OutputInventoryGuard, OutputInventoryJobRunner, OutputInventoryTransport,
        OutputInventoryWorkloadConfig, OutputPvc,
    },
    publication_journal::{PublicationJournal, valid_private_root},
    sandbox::{
        BrokerLeaseFence, RuntimeCompletion, RuntimeObservation, SandboxBinding, SandboxRuntime,
        SandboxTransport,
    },
    store::Store,
    terminal_exports::encode_terminal_export,
    terminal_request::TerminalPreparationRequest,
};
use crate::output_inventory::OutputInventoryRequest;
use anyhow::{Result, anyhow, ensure};
use async_trait::async_trait;
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use serde_json::json;
use sha2::{Digest, Sha256};
use std::{
    io::Read,
    path::{Path, PathBuf},
    time::Duration,
};
use tokio_util::sync::CancellationToken;

const RECORD_LIMIT: u64 = 1024 * 1024;

#[derive(Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct OutputCustodianConfig {
    pub requests: InputPreparationConfig,
    pub plans: InputPreparationConfig,
    pub custody_root: PathBuf,
    pub journal_root: PathBuf,
    pub inventory: OutputInventoryWorkloadConfig,
    pub publication: ExportPublicationConfig,
    pub lease_seconds: i32,
    pub max_wait_seconds: u32,
    pub poll_interval_millis: u32,
    pub export_seconds: u32,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct OutputPlan {
    schema_version: String,
    task: PreparationTaskIdentity,
    binding: SandboxBinding,
    run_id: String,
    output_pvc_uid: String,
    selection: OutputInventoryRequest,
}

#[async_trait]
pub trait TerminalCustodyTaskSource: BrokerLeaseFence {
    async fn lookup_terminal_task(
        &self,
        request: &TerminalPreparationRequest,
    ) -> Result<Option<ClaimedTask>>;
}

#[async_trait]
impl TerminalCustodyTaskSource for Store {
    async fn lookup_terminal_task(
        &self,
        request: &TerminalPreparationRequest,
    ) -> Result<Option<ClaimedTask>> {
        let t = request.task();
        self.terminal_custody_task(
            t.task_id,
            &request.handle().worker_id,
            t.attempt,
            t.lease_generation,
        )
        .await
        .map_err(|_| anyhow!("output_custodian_task_lookup_failed"))
    }
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct OutputCustodianReceipt {
    pub schema_version: String,
    pub task_id: uuid::Uuid,
    pub attempt: i32,
    pub lease_generation: i64,
    pub inventory_job_uid: String,
    pub inventory_pod_uid: String,
    pub inventory_sha256: String,
    pub terminal_sha256: String,
    pub file_count: usize,
    pub size_bytes: u64,
    pub export_objects: Vec<ExportObjectReceipt>,
}

pub struct OutputCustodian {
    config: OutputCustodianConfig,
}

impl OutputCustodian {
    pub fn new(config: OutputCustodianConfig) -> Result<Self> {
        let roots = [
            &config.requests.root,
            &config.plans.root,
            &config.custody_root,
            &config.journal_root,
            &config.inventory.journal_root,
            &config.publication.journal_root,
            &config.publication.remote.credential_root,
        ];
        for (i, root) in roots.iter().enumerate() {
            ensure!(valid_private_root(root), "output_custodian_root_invalid");
            for other in &roots[..i] {
                ensure!(
                    !root.starts_with(other) && !other.starts_with(root),
                    "output_custodian_roots_overlap"
                );
            }
        }
        config.publication.remote.validate()?;
        config.publication.workload.validate()?;
        config.inventory.validate()?;
        ensure!(
            (30..=3600).contains(&config.lease_seconds)
                && (1..=3600).contains(&config.max_wait_seconds)
                && (1..=5000).contains(&config.poll_interval_millis)
                && config.poll_interval_millis <= config.max_wait_seconds * 1000
                && config.export_seconds as u64 > config.publication.workload.max_seconds + 120
                && config.export_seconds <= 82_800
                && config.inventory.writer_uid == config.publication.remote.writer_uid,
            "output_custodian_config_invalid"
        );
        Ok(Self { config })
    }

    /// One approved run. Existing journals, including successful ones, require
    /// explicit custody reconciliation; retries never issue a second identity.
    pub async fn run<L, S, J, E>(
        &self,
        request_name: &str,
        ledger: &L,
        runtime: &SandboxRuntime<S>,
        inventory_transport: J,
        exporter: &E,
        cancellation: &CancellationToken,
    ) -> Result<OutputCustodianReceipt>
    where
        L: TerminalCustodyTaskSource,
        S: SandboxTransport,
        J: OutputInventoryTransport,
        E: ExportPublicationTransport,
    {
        tokio::select! {
            biased;
            _ = cancellation.cancelled() => Err(anyhow!("output_custodian_cancelled")),
            result = tokio::time::timeout(
                Duration::from_secs(self.config.max_wait_seconds.into()),
                self.execute(request_name, ledger, runtime, inventory_transport, exporter, cancellation),
            ) => result.map_err(|_| anyhow!("output_custodian_deadline"))?,
        }
    }

    async fn execute<L, S, J, E>(
        &self,
        request_name: &str,
        ledger: &L,
        runtime: &SandboxRuntime<S>,
        inventory_transport: J,
        exporter: &E,
        cancellation: &CancellationToken,
    ) -> Result<OutputCustodianReceipt>
    where
        L: TerminalCustodyTaskSource,
        S: SandboxTransport,
        J: OutputInventoryTransport,
        E: ExportPublicationTransport,
    {
        let notice = TerminalPreparationRequest::load(
            &self.config.requests.root,
            request_name,
            self.config.requests.writer_uid,
        )?;
        let task = ledger
            .lookup_terminal_task(&notice)
            .await?
            .ok_or_else(|| anyhow!("output_custodian_task_not_current"))?;
        ensure!(
            notice.task().matches(&task),
            "output_custodian_task_mismatch"
        );
        ensure!(
            task.max_run_seconds > 0
                && (task.max_run_seconds as u64)
                    .checked_add(self.config.export_seconds.into())
                    .is_some_and(|seconds| seconds <= 86_400),
            "output_custodian_task_duration_invalid"
        );
        let plan_bytes = read_private(
            &self.config.plans.root,
            request_name,
            self.config.plans.writer_uid,
        )?;
        let plan: OutputPlan = decode(&plan_bytes)?;
        ensure!(
            plan.schema_version == "sandbox.enrichedlab.org/output-preparation-plan/v1"
                && plan.task == *notice.task()
                && plan.binding == *notice.binding()
                && plan.run_id == notice.handle().sandbox_name
                && plan.selection.mounted_root == Path::new("/artifacts")
                && plan.selection.max_files <= self.config.publication.workload.max_files
                && plan.selection.max_bytes <= self.config.publication.workload.max_bytes,
            "output_custodian_plan_mismatch"
        );
        let h = notice.handle();
        let observed = RuntimeObservation {
            sandbox_uid: h.sandbox_uid.clone(),
            sandbox_generation: h.sandbox_generation,
            pod_name: notice.completion().pod_name.clone(),
            pod_uid: notice.completion().pod_uid.clone(),
            runtime_class_name: notice.completion().runtime_class_name.clone(),
        };
        let completion = runtime
            .observe_terminal(&task, h, &observed)
            .await?
            .ok_or_else(|| anyhow!("output_custodian_runtime_not_terminal"))?;
        ensure!(
            notice.completion().matches(&completion),
            "output_custodian_completion_changed"
        );
        let scope = ExportScope::new(task.id, task.attempt, task.lease_generation)?;
        let key = scope.credential_name();
        let guard = CustodianGuard {
            ledger,
            runtime,
            task: &task,
            notice: &notice,
            completion: &completion,
            lease_seconds: self.config.lease_seconds,
        };
        ensure!(guard.check().await?, "output_custodian_authority_lost");
        let mut journal = PublicationJournal::create(
            &self.config.journal_root,
            &format!("{key}.jsonl"),
            self.config.publication.remote.writer_uid,
        )
        .await?;
        journal
            .append(json!({"phase":"started", "taskId":task.id,
            "attempt":task.attempt,"leaseGeneration":task.lease_generation,
            "sandboxUid":h.sandbox_uid,"podUid":observed.pod_uid}))
            .await?;
        self.retain(&format!("{key}.plan.json"), plan_bytes).await?;
        let runner = OutputInventoryJobRunner::new(
            self.config.inventory.clone(),
            inventory_transport,
            Duration::from_millis(self.config.poll_interval_millis.into()),
        )?;
        let secret_name = format!("{}-inventory", scope.service_name(&h.namespace)?);
        let measured = runner
            .run(
                &task,
                notice.binding(),
                h,
                &completion,
                &plan.selection,
                OutputPvc {
                    name: format!("artifacts-{}", h.sandbox_name),
                    uid: plan.output_pvc_uid,
                },
                &secret_name,
                &guard,
                cancellation,
            )
            .await?;
        ensure!(guard.check().await?, "output_custodian_authority_lost");
        let original_inventory = measured.original_inventory();
        let inventory_sha256 = hex::encode(Sha256::digest(original_inventory));
        self.retain(
            &format!("{key}.inventory.json"),
            original_inventory.to_vec(),
        )
        .await?;
        journal
            .append(
                json!({"phase":"inventory_retained", "jobUid":measured.job_uid(),
            "podUid":measured.pod_uid(),"inventorySha256":inventory_sha256}),
            )
            .await?;
        let terminal = encode_terminal_export(
            &task,
            notice.binding(),
            h,
            &completion,
            &plan.selection,
            measured.inventory(),
        )?;
        let terminal_sha256 = hex::encode(Sha256::digest(&terminal));
        self.retain(&format!("{key}.terminal.json"), terminal.clone())
            .await?;
        let size_bytes = measured.inventory().files.iter().try_fold(0u64, |n, f| {
            n.checked_add(f.size_bytes)
                .ok_or_else(|| anyhow!("output_custodian_size_overflow"))
        })?;
        ensure!(guard.check().await?, "output_custodian_authority_lost");
        // Reserve this scope before creating ephemeral identity. A crash leaves
        // retained originals and a journal for recovery, never blind reissuance.
        journal
            .append(json!({"phase":"export_identity_intent","terminalSha256":terminal_sha256}))
            .await?;
        let credentials = issue_export_credentials(
            &self.config.publication.remote,
            scope,
            &h.namespace,
            ExportCredentialLifetime {
                task_seconds: task
                    .max_run_seconds
                    .try_into()
                    .map_err(|_| anyhow!("output_custodian_task_duration_invalid"))?,
                export_seconds: self.config.export_seconds.into(),
            },
        )
        .map_err(|_| anyhow!("output_custodian_identity_failed"))?;
        let publication = PreparedExportPublication::prepare(
            self.config.publication.clone(),
            &task,
            notice.binding(),
            h,
            &completion,
            &terminal,
            credentials,
        )?;
        let export_objects = publication
            .publish(
                &ObservedLeaseGuard {
                    ledger,
                    runtime,
                    lease_seconds: self.config.lease_seconds,
                },
                exporter,
                cancellation,
            )
            .await?;
        let receipt = OutputCustodianReceipt {
            schema_version: "sandbox.enrichedlab.org/output-custodian-receipt/v1".into(),
            task_id: task.id,
            attempt: task.attempt,
            lease_generation: task.lease_generation,
            inventory_job_uid: measured.job_uid().into(),
            inventory_pod_uid: measured.pod_uid().into(),
            inventory_sha256,
            terminal_sha256,
            file_count: measured.inventory().files.len(),
            size_bytes,
            export_objects,
        };
        self.retain(
            &format!("{key}.receipt.json"),
            serde_json::to_vec(&receipt)
                .map_err(|_| anyhow!("output_custodian_receipt_invalid"))?,
        )
        .await?;
        journal
            .append(json!({"phase":"published","fileCount":receipt.file_count,
            "sizeBytes":receipt.size_bytes,"terminalSha256":receipt.terminal_sha256}))
            .await?;
        Ok(receipt)
    }

    async fn retain(&self, name: &str, bytes: Vec<u8>) -> Result<()> {
        let root = self.config.custody_root.clone();
        let name = name.to_owned();
        let uid = self.config.publication.remote.writer_uid;
        tokio::task::spawn_blocking(move || publish_private(&root, &name, uid, &bytes))
            .await
            .map_err(|_| anyhow!("output_custodian_retention_failed"))?
            .map_err(|_| anyhow!("output_custodian_retention_failed"))
    }
}

struct CustodianGuard<'a, L, S> {
    ledger: &'a L,
    runtime: &'a SandboxRuntime<S>,
    task: &'a ClaimedTask,
    notice: &'a TerminalPreparationRequest,
    completion: &'a RuntimeCompletion,
    lease_seconds: i32,
}
#[async_trait]
impl<L: TerminalCustodyTaskSource, S: SandboxTransport> OutputInventoryGuard
    for CustodianGuard<'_, L, S>
{
    async fn check(&self) -> Result<bool> {
        let h = self.notice.handle();
        let Some(task) = self.ledger.lookup_terminal_task(self.notice).await? else {
            return Ok(false);
        };
        if !self.notice.task().matches(&task)
            || self
                .ledger
                .cancel_requested(task.id, &h.worker_id, task.lease_generation)
                .await?
            || !self
                .ledger
                .renew_lease(
                    task.id,
                    &h.worker_id,
                    task.lease_generation,
                    self.lease_seconds,
                )
                .await?
        {
            return Ok(false);
        }
        Ok(self
            .runtime
            .observe_terminal(self.task, h, self.completion.observation())
            .await?
            .is_some_and(|current| self.notice.completion().matches(&current)))
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct OutputCustodianServiceConfig {
    pub schema_version: String,
    pub database_tenant: String,
    pub database_dsn_file: PathBuf,
    pub kubernetes: InputCustodianKubernetesConfig,
    pub custodian: OutputCustodianConfig,
    pub bindings: Vec<SandboxBinding>,
}
impl OutputCustodianServiceConfig {
    pub fn load(path: &Path, owner_uid: u32) -> Result<Self> {
        let parent = path
            .parent()
            .ok_or_else(|| anyhow!("output_custodian_config_path_invalid"))?;
        let name = path
            .file_name()
            .and_then(|v| v.to_str())
            .ok_or_else(|| anyhow!("output_custodian_config_path_invalid"))?;
        let c: Self = decode(&read_private(parent, name, owner_uid)?)?;
        ensure!(
            c.schema_version == "sandbox.enrichedlab.org/output-custodian/v1"
                && c.database_dsn_file.is_absolute()
                && (2..=64).contains(&c.database_tenant.len())
                && c.database_tenant.bytes().all(|b| b.is_ascii_lowercase()
                    || b.is_ascii_digit()
                    || matches!(b, b'-' | b'_'))
                && !c.bindings.is_empty()
                && c.bindings
                    .iter()
                    .all(|b| b.namespace == c.kubernetes.namespace),
            "output_custodian_service_config_invalid"
        );
        Ok(c)
    }
}
fn read_private(root: &Path, name: &str, uid: u32) -> Result<Vec<u8>> {
    let f = open_bounded_record(root, name, uid, RECORD_LIMIT)
        .map_err(|_| anyhow!("output_custodian_record_unavailable"))?;
    let mut bytes = Vec::new();
    f.take(RECORD_LIMIT + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| anyhow!("output_custodian_record_unavailable"))?;
    ensure!(
        bytes.len() as u64 <= RECORD_LIMIT,
        "output_custodian_record_too_large"
    );
    Ok(bytes)
}
fn decode<T: DeserializeOwned>(bytes: &[u8]) -> Result<T> {
    serde_json::from_slice(bytes).map_err(|_| anyhow!("output_custodian_record_invalid"))
}
