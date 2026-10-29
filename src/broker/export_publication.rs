//! Trusted, create-only publication of one terminal run's export service.
//! A durable intent precedes each request. An interrupted publication is custody
//! work, never permission to overwrite objects or issue another run identity.
use super::{
    artifact_retainer::{ExportLimits, validate_export_files, validate_task_binding},
    export_client_staging::stage_export_client,
    export_identity::IssuedExportCredentials,
    export_kubernetes::{valid_label, valid_uid},
    export_transport::RemoteExportConfig,
    export_workload::{ExportIdentity, ExportWorkloadConfig, ExportWorkloadRenderer},
    model::ClaimedTask,
    publication_journal::{PublicationJournal, valid_private_root},
    sandbox::{
        BrokerLeaseFence, RuntimeCompletion, RuntimeObservation, SandboxBinding, SandboxHandle,
        SandboxRuntime, SandboxTransport,
    },
    terminal_exports::validated_export_selection,
};
use anyhow::{Result, anyhow, ensure};
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{fmt, path::PathBuf, sync::Arc};
use time::{Duration, OffsetDateTime};
use tokio_util::sync::CancellationToken;

const SECRET_LIMIT: usize = 1024 * 1024;

/// Operator configuration. Neither roots nor templates are accepted from a task.
#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ExportPublicationConfig {
    pub remote: RemoteExportConfig,
    pub workload: ExportWorkloadConfig,
    pub journal_root: PathBuf,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExportObjectKind {
    NetworkPolicy,
    Secret,
    Service,
    Pod,
}
impl ExportObjectKind {
    pub(crate) fn resource(self) -> &'static str {
        match self {
            Self::NetworkPolicy => "networkpolicies",
            Self::Secret => "secrets",
            Self::Service => "services",
            Self::Pod => "pods",
        }
    }
}

/// The private request body has no public serializer or accessor.
pub struct ExportObject {
    kind: ExportObjectKind,
    namespace: String,
    name: String,
    digest: String,
    body: Value,
}
impl fmt::Debug for ExportObject {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ExportObject")
            .field("kind", &self.kind)
            .field("name", &self.name)
            .finish_non_exhaustive()
    }
}
impl ExportObject {
    pub fn kind(&self) -> ExportObjectKind {
        self.kind
    }
    pub fn namespace(&self) -> &str {
        &self.namespace
    }
    pub fn name(&self) -> &str {
        &self.name
    }
    pub fn digest(&self) -> &str {
        &self.digest
    }
    pub(crate) fn body(&self) -> &Value {
        &self.body
    }
    fn new(kind: ExportObjectKind, namespace: &str, name: &str, body: Value) -> Result<Self> {
        ensure!(
            valid_label(namespace) && valid_label(name),
            "export_object_identity_invalid"
        );
        ensure!(
            body.pointer("/metadata/name").and_then(Value::as_str) == Some(name)
                && body.pointer("/metadata/namespace").and_then(Value::as_str) == Some(namespace),
            "export_object_identity_mismatch"
        );
        let bytes = serde_json::to_vec(&body).map_err(|_| anyhow!("export_object_invalid"))?;
        Ok(Self {
            kind,
            namespace: namespace.into(),
            name: name.into(),
            digest: hex::encode(Sha256::digest(bytes)),
            body,
        })
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ExportObjectReceipt {
    pub kind: ExportObjectKind,
    pub name: String,
    pub uid: String,
    pub request_sha256: String,
}

#[async_trait]
pub trait ExportPublicationTransport: Send + Sync {
    fn namespace(&self) -> &str;
    async fn create(
        &self,
        object: &ExportObject,
        cancellation: &CancellationToken,
    ) -> Result<ExportObjectReceipt>;
}

/// An authority implemented by trusted site code, never a plugin callback.
#[async_trait]
pub trait ExportPublicationGuard: Send + Sync {
    async fn check(&self, publication: &PreparedExportPublication) -> Result<()>;
}

/// Reuses the broker's existing lease fence and actual Sandbox/Pod observer.
/// Site admission must additionally prevent other writers mounting the run PVC.
pub struct ObservedLeaseGuard<'a, L, T> {
    pub ledger: &'a L,
    pub runtime: &'a SandboxRuntime<T>,
    pub lease_seconds: i32,
}
#[async_trait]
impl<L: BrokerLeaseFence, T: SandboxTransport> ExportPublicationGuard
    for ObservedLeaseGuard<'_, L, T>
{
    async fn check(&self, p: &PreparedExportPublication) -> Result<()> {
        ensure!(
            (30..=3600).contains(&self.lease_seconds),
            "export_lease_duration_invalid"
        );
        let (id, worker, generation) = (
            p.task.id,
            p.handle.worker_id.as_str(),
            p.task.lease_generation,
        );
        ensure!(
            !self.ledger.cancel_requested(id, worker, generation).await?,
            "export_task_cancelled"
        );
        ensure!(
            self.ledger
                .renew_lease(id, worker, generation, self.lease_seconds)
                .await?,
            "export_lease_lost"
        );
        let current = self
            .runtime
            .observe_terminal(&p.task, &p.handle, &p.observation)
            .await?
            .ok_or_else(|| anyhow!("export_runtime_not_terminal"))?;
        ensure!(
            current.exit_code() == p.exit_code && current.finished_at() == p.finished_at,
            "export_completion_changed"
        );
        Ok(())
    }
}

/// Prepared only from broker identity, a trusted terminal record, observed
/// process completion and issued private credentials. No raw-body accessor.
pub struct PreparedExportPublication {
    config: ExportPublicationConfig,
    task: ClaimedTask,
    handle: SandboxHandle,
    observation: RuntimeObservation,
    exit_code: i32,
    finished_at: OffsetDateTime,
    credentials: Arc<IssuedExportCredentials>,
    manifest_sha256: String,
    objects: Vec<ExportObject>,
}
impl fmt::Debug for PreparedExportPublication {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("PreparedExportPublication")
            .field("task_id", &self.task.id)
            .field("attempt", &self.task.attempt)
            .field("lease_generation", &self.task.lease_generation)
            .field("objects", &self.objects)
            .finish_non_exhaustive()
    }
}
impl PreparedExportPublication {
    #[allow(clippy::too_many_arguments)]
    pub fn prepare(
        config: ExportPublicationConfig,
        task: &ClaimedTask,
        binding: &SandboxBinding,
        handle: &SandboxHandle,
        completion: &RuntimeCompletion,
        terminal: &[u8],
        credentials: IssuedExportCredentials,
    ) -> Result<Self> {
        config.remote.validate()?;
        config.workload.validate()?;
        ensure!(
            valid_private_root(&config.journal_root)
                && !config
                    .journal_root
                    .starts_with(&config.remote.credential_root)
                && !config
                    .remote
                    .credential_root
                    .starts_with(&config.journal_root),
            "export_journal_root_invalid"
        );
        ensure!(
            config.remote.dns_suffix == config.workload.dns_suffix && config.remote.port == 8443,
            "export_endpoint_configuration_mismatch"
        );
        validate_task_binding(task, binding, handle)?;
        let files = validated_export_selection(terminal, task, handle, completion)?;
        validate_export_files(
            &files,
            ExportLimits {
                max_files: config.workload.max_files,
                max_bytes: config.workload.max_bytes,
            },
        )?;
        let metadata = credentials.metadata();
        ensure!(
            metadata.scope.task_id == task.id
                && metadata.scope.attempt == task.attempt
                && metadata.scope.lease_generation == task.lease_generation,
            "export_credential_scope_mismatch"
        );
        let name = metadata.scope.service_name(&binding.namespace)?;
        ensure!(
            metadata.server_dns
                == format!("{name}.{}.{}", binding.namespace, config.remote.dns_suffix),
            "export_credential_dns_mismatch"
        );
        ensure!(
            metadata.expires_at
                > OffsetDateTime::now_utc()
                    + Duration::seconds(config.workload.max_seconds as i64 + 60),
            "export_credentials_expire_too_soon"
        );
        let renderer = ExportWorkloadRenderer::new(config.workload.clone())?;
        let workload = renderer.render(
            task,
            binding,
            handle,
            &handle.sandbox_name,
            &ExportIdentity {
                secret_name: name.clone(),
                client_sha256: metadata.client_fingerprint_sha256.clone(),
            },
        )?;
        let exporter_config = serde_json::to_vec(&workload.exporter_config)
            .map_err(|_| anyhow!("export_config_invalid"))?;
        let server = credentials.server_files();
        let selected = [
            ("config.json", exporter_config.as_slice()),
            ("terminal.json", terminal),
            ("ca.pem", server.ca_pem),
            ("server.pem", server.server_pem),
            ("server.key", server.server_key_pem),
        ];
        // Kubernetes limits the sum of decoded Secret values, not base64 text.
        let size = selected
            .iter()
            .try_fold(0_usize, |sum, (_, bytes)| sum.checked_add(bytes.len()))
            .ok_or_else(|| anyhow!("export_secret_too_large"))?;
        ensure!(size <= SECRET_LIMIT, "export_secret_too_large");
        let mut values = serde_json::Map::new();
        for (key, bytes) in selected {
            values.insert(
                key.into(),
                Value::String(
                    std::str::from_utf8(bytes)
                        .map_err(|_| anyhow!("export_secret_encoding_invalid"))?
                        .into(),
                ),
            );
        }
        let secret = json!({"apiVersion":"v1", "kind":"Secret", "type":"Opaque", "immutable":true,
            "metadata": {"name":name, "namespace":binding.namespace, "labels":workload.pod["metadata"]["labels"]}, "stringData":values});
        let objects = [
            (ExportObjectKind::NetworkPolicy, workload.network_policy),
            (ExportObjectKind::Secret, secret),
            (ExportObjectKind::Service, workload.service),
            (ExportObjectKind::Pod, workload.pod),
        ]
        .into_iter()
        .map(|(kind, body)| ExportObject::new(kind, &binding.namespace, &name, body))
        .collect::<Result<Vec<_>>>()?;
        Ok(Self {
            config,
            task: task.clone(),
            handle: handle.clone(),
            observation: completion.observation().clone(),
            exit_code: completion.exit_code(),
            finished_at: completion.finished_at(),
            credentials: Arc::new(credentials),
            manifest_sha256: hex::encode(Sha256::digest(terminal)),
            objects,
        })
    }

    /// Returns publication receipts, not a successful research-task result.
    /// Receipts and staged credentials remain for UID-fenced cleanup or recovery.
    pub async fn publish<G: ExportPublicationGuard, T: ExportPublicationTransport>(
        &self,
        guard: &G,
        transport: &T,
        cancellation: &CancellationToken,
    ) -> Result<Vec<ExportObjectReceipt>> {
        ensure!(
            transport.namespace() == self.handle.namespace,
            "export_publisher_namespace_mismatch"
        );
        self.check(guard, cancellation).await?;
        let name = format!(
            "{}.jsonl",
            self.credentials.metadata().scope.credential_name()
        );
        let mut journal = PublicationJournal::create(
            &self.config.journal_root,
            &name,
            self.config.remote.writer_uid,
        )
        .await?;
        let metadata = self.credentials.metadata();
        journal.append(json!({"phase":"prepared", "taskId":self.task.id, "attempt":self.task.attempt,
            "leaseGeneration":self.task.lease_generation, "namespace":self.handle.namespace,
            "service":self.objects[0].name(), "manifestSha256":self.manifest_sha256,
            "clientSha256":metadata.client_fingerprint_sha256, "expiresAt":metadata.expires_at.unix_timestamp()})).await?;
        let remote = self.config.remote.clone();
        let credentials = Arc::clone(&self.credentials);
        let staging =
            tokio::task::spawn_blocking(move || stage_export_client(&remote, &credentials)).await;
        if !matches!(staging, Ok(Ok(_))) {
            journal
                .append(json!({"phase":"stopped", "reason":"credential_staging_failed"}))
                .await?;
            return Err(anyhow!("export_client_staging_failed"));
        }
        let mut receipts = Vec::with_capacity(self.objects.len());
        for object in &self.objects {
            if self.check(guard, cancellation).await.is_err() {
                journal
                    .append(
                        json!({"phase":"stopped", "reason":"authority_or_cancellation_changed"}),
                    )
                    .await?;
                return Err(anyhow!("export_publication_authority_lost"));
            }
            journal.append(json!({"phase":"create_intent", "kind":object.kind(), "name":object.name(), "requestSha256":object.digest()})).await?;
            let created = tokio::select! {
                biased;
                _ = cancellation.cancelled() => Err(anyhow!("export_publication_cancelled")),
                result = tokio::time::timeout(std::time::Duration::from_secs(20), transport.create(object, cancellation)) =>
                    result.map_err(|_| anyhow!("export_create_deadline")).and_then(|result| result),
            };
            let receipt = match created {
                Ok(receipt)
                    if receipt.kind == object.kind()
                        && receipt.name == object.name()
                        && valid_uid(&receipt.uid)
                        && receipt.request_sha256 == object.digest() =>
                {
                    receipt
                }
                _ => {
                    journal
                        .append(json!({"phase":"stopped", "reason":"create_requires_recovery"}))
                        .await?;
                    return Err(anyhow!("export_publication_requires_recovery"));
                }
            };
            journal
                .append(json!({"phase":"created", "object":receipt}))
                .await?;
            receipts.push(receipt);
        }
        if self.check(guard, cancellation).await.is_err() {
            journal
                .append(json!({"phase":"stopped", "reason":"authority_or_cancellation_changed"}))
                .await?;
            return Err(anyhow!("export_publication_authority_lost"));
        }
        journal.append(json!({"phase":"published"})).await?;
        Ok(receipts)
    }

    async fn check<G: ExportPublicationGuard>(
        &self,
        guard: &G,
        cancellation: &CancellationToken,
    ) -> Result<()> {
        ensure!(
            self.credentials.metadata().expires_at
                > OffsetDateTime::now_utc()
                    + Duration::seconds(self.config.workload.max_seconds as i64 + 60),
            "export_credentials_expire_too_soon"
        );
        tokio::select! {
            biased;
            _ = cancellation.cancelled() => Err(anyhow!("export_publication_cancelled")),
            result = tokio::time::timeout(std::time::Duration::from_secs(20), guard.check(self)) =>
                result.map_err(|_| anyhow!("export_authority_deadline"))?
                    .map_err(|_| anyhow!("export_publication_authority_lost")),
        }
    }
}
