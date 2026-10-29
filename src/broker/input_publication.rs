//! Trusted publication of one fixed input-copy workload. Original private
//! request bytes precede the Job; durable create intents and exact UID receipts
//! survive interruptions. Partial scopes require recovery, never blind replay.
use super::{
    admission_publication::{CreatedVolumeIdentities, publish_private},
    export_kubernetes::{valid_label, valid_uid},
    input_completion::{
        InputCopyContainerExpectation, InputCopyJobExpectation, InputCopyObservationTransport,
        InputCopyVolumeExpectation, InputCopyVolumeSource, ObservedInputSeal, expected_manifest,
        observe_input_seal,
    },
    input_workload::{
        AuthorizedInputCopy, InputCopyAuthorization, InputWorkloadConfig, InputWorkloadRenderer,
    },
    model::ClaimedTask,
    publication_journal::{PublicationJournal, valid_private_root},
    sandbox::{BrokerLeaseFence, SandboxBinding, validate_custody_pvc},
};
use crate::custody::{CopySealRequest, canonical_document_digest};
use anyhow::{Result, anyhow, ensure};
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{fmt, path::PathBuf, time::Duration};
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct InputPublicationConfig {
    pub workload: InputWorkloadConfig,
    pub journal_root: PathBuf,
    pub custody_root: PathBuf,
    pub writer_uid: u32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InputObjectKind {
    NetworkPolicy,
    PersistentVolumeClaim,
    Secret,
    Job,
}

/// No public body accessor or serializer: the request Secret contains private
/// declared file paths and references, which must not become debug output.
pub struct InputObject {
    kind: InputObjectKind,
    namespace: String,
    name: String,
    digest: String,
    body: Value,
}
impl fmt::Debug for InputObject {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("InputObject")
            .field("kind", &self.kind)
            .field("name", &self.name)
            .finish_non_exhaustive()
    }
}
impl InputObject {
    pub fn kind(&self) -> InputObjectKind {
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
    fn new(kind: InputObjectKind, namespace: &str, body: Value) -> Result<Self> {
        let name = string(&body, "/metadata/name")?;
        ensure!(
            valid_label(namespace)
                && valid_label(name)
                && string(&body, "/metadata/namespace")? == namespace,
            "input_publication_object_identity_invalid"
        );
        let bytes =
            serde_json::to_vec(&body).map_err(|_| anyhow!("input_publication_object_invalid"))?;
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
pub struct InputObjectReceipt {
    pub kind: InputObjectKind,
    pub namespace: String,
    pub name: String,
    pub uid: String,
    pub request_sha256: String,
}

#[async_trait]
pub trait InputPublicationTransport: Send + Sync {
    fn namespace(&self) -> &str;
    async fn create(
        &self,
        object: &InputObject,
        cancellation: &CancellationToken,
    ) -> Result<InputObjectReceipt>;
}

/// Source identity approved by the site's data authority before rendering.
/// The site must keep the source's namespace and writer policy in force for the
/// whole copy. Neither this identity nor a PVC name grants access by itself.
#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct InputSourceIdentity {
    pub namespace: String,
    pub pvc_name: String,
    pub pvc_uid: String,
    pub run_id: String,
    pub tenant_id: String,
    pub principal_hash: String,
}

#[async_trait]
pub trait InputSourceObservationTransport: Send + Sync {
    async fn get_source_pvc(&self, namespace: &str, name: &str) -> Result<Value>;
    /// Must inspect every relevant Pod in the namespace, including init and
    /// ephemeral containers. Missing or ambiguous observations must fail closed.
    async fn source_has_active_writer(&self, namespace: &str, pvc_name: &str) -> Result<bool>;
}

#[async_trait]
pub trait InputPublicationGuard: Send + Sync {
    async fn check(&self, publication: &PreparedInputPublication) -> Result<()>;
}

/// Refreshes the existing task lease, cancellation, approved PVC UID and source
/// writer state before each create. Site admission must prevent a new source
/// writer or replacement while the copy is running; observation is not a lock.
pub struct InputSourceLeaseGuard<'a, L, T> {
    pub ledger: &'a L,
    pub source: &'a T,
    pub lease_seconds: i32,
}
#[async_trait]
impl<L: BrokerLeaseFence, T: InputSourceObservationTransport> InputPublicationGuard
    for InputSourceLeaseGuard<'_, L, T>
{
    async fn check(&self, p: &PreparedInputPublication) -> Result<()> {
        ensure!(
            (30..=3600).contains(&self.lease_seconds),
            "input_publication_lease_invalid"
        );
        ensure!(
            !self
                .ledger
                .cancel_requested(p.task.id, &p.worker_id, p.task.lease_generation)
                .await?,
            "input_publication_task_cancelled"
        );
        ensure!(
            self.ledger
                .renew_lease(
                    p.task.id,
                    &p.worker_id,
                    p.task.lease_generation,
                    self.lease_seconds
                )
                .await?,
            "input_publication_lease_lost"
        );
        let s = &p.source;
        let pvc = self
            .source
            .get_source_pvc(&s.namespace, &s.pvc_name)
            .await?;
        ensure!(
            string(&pvc, "/metadata/name")? == s.pvc_name
                && string(&pvc, "/metadata/namespace")? == s.namespace
                && string(&pvc, "/metadata/uid")? == s.pvc_uid
                && pvc
                    .pointer("/metadata/deletionTimestamp")
                    .is_none_or(Value::is_null)
                && string(&pvc, "/status/phase")? == "Bound",
            "input_publication_source_changed"
        );
        for (key, expected) in [
            ("run-id", s.run_id.as_str()),
            ("tenant", s.tenant_id.as_str()),
            ("principal-hash", s.principal_hash.as_str()),
            ("contract-version", "v3"),
        ] {
            ensure!(
                string(
                    &pvc,
                    &format!("/metadata/labels/sandbox.enrichedlab.org~1{key}")
                )? == expected,
                "input_publication_source_scope_mismatch"
            );
        }
        let sealed = s.pvc_name == format!("input-{}", s.run_id);
        let role = if sealed {
            "immutable-input"
        } else {
            "durable-artifacts"
        };
        validate_custody_pvc(&pvc, &s.run_id, role, sealed, None)
            .map_err(|_| anyhow!("input_publication_source_custody_invalid"))?;
        ensure!(
            !self
                .source
                .source_has_active_writer(&s.namespace, &s.pvc_name)
                .await?,
            "input_publication_source_writer_active"
        );
        Ok(())
    }
}

pub struct PreparedInputPublication {
    config: InputPublicationConfig,
    task: ClaimedTask,
    worker_id: String,
    source: InputSourceIdentity,
    request: CopySealRequest,
    request_bytes: Vec<u8>,
    manifest_bytes: Vec<u8>,
    objects: Vec<InputObject>,
    expectation: InputCopyJobExpectation,
}
impl fmt::Debug for PreparedInputPublication {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("PreparedInputPublication")
            .field("task_id", &self.task.id)
            .field("attempt", &self.task.attempt)
            .field("lease_generation", &self.task.lease_generation)
            .field("objects", &self.objects)
            .finish_non_exhaustive()
    }
}

impl PreparedInputPublication {
    #[allow(clippy::too_many_arguments)]
    pub fn prepare(
        config: InputPublicationConfig,
        task: &ClaimedTask,
        worker_id: String,
        binding: &SandboxBinding,
        authorization: &InputCopyAuthorization,
        copy: &AuthorizedInputCopy,
        source: InputSourceIdentity,
    ) -> Result<Self> {
        ensure!(
            valid_private_root(&config.journal_root)
                && valid_private_root(&config.custody_root)
                && !config.journal_root.starts_with(&config.custody_root)
                && !config.custody_root.starts_with(&config.journal_root),
            "input_publication_roots_invalid"
        );
        ensure!(
            !worker_id.is_empty()
                && worker_id.len() <= 128
                && worker_id
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.')),
            "input_publication_worker_invalid"
        );
        ensure!(
            source.namespace == binding.namespace
                && valid_uid(&source.pvc_uid)
                && source.run_id == authorization.source_run_name
                && source.pvc_name == authorization.source_pvc_name
                && source.tenant_id == authorization.tenant_id
                && source.principal_hash == binding.principal_hash,
            "input_publication_source_identity_invalid"
        );
        let renderer = InputWorkloadRenderer::new(config.workload.clone())?;
        let workload = renderer.render(task, binding, authorization, copy)?;
        let request_bytes = workload.request_bytes().to_vec();
        ensure!(
            request_bytes.len() <= 1024 * 1024,
            "input_publication_secret_too_large"
        );
        let request: CopySealRequest = serde_json::from_slice(&request_bytes)
            .map_err(|_| anyhow!("input_publication_request_invalid"))?;
        let manifest = expected_manifest(&request)?;
        ensure!(
            canonical_document_digest(&manifest)? == request.input_digest,
            "input_publication_manifest_digest_mismatch"
        );
        let manifest_bytes = serde_json::to_vec(&manifest)
            .map_err(|_| anyhow!("input_publication_manifest_invalid"))?;
        let expectation = job_expectation(&workload.job, authorization)?;
        let secret_name = expectation
            .volumes
            .iter()
            .find_map(|v| match &v.source {
                InputCopyVolumeSource::ProjectedSecret { secret_name } => Some(secret_name.clone()),
                _ => None,
            })
            .ok_or_else(|| anyhow!("input_publication_request_volume_missing"))?;
        let secret = json!({"apiVersion":"v1", "kind":"Secret", "type":"Opaque", "immutable":true,
            "metadata":{"name":secret_name,"namespace":binding.namespace,"labels":workload.job["metadata"]["labels"]},
            "stringData":{"request.json":std::str::from_utf8(&request_bytes)
                .map_err(|_| anyhow!("input_publication_request_encoding_invalid"))?}});
        let objects = [
            (InputObjectKind::NetworkPolicy, workload.network_policy),
            (InputObjectKind::PersistentVolumeClaim, workload.input_pvc),
            (
                InputObjectKind::PersistentVolumeClaim,
                workload.artifact_pvc,
            ),
            (InputObjectKind::Secret, secret),
            (InputObjectKind::Job, workload.job),
        ]
        .into_iter()
        .map(|(kind, body)| InputObject::new(kind, &binding.namespace, body))
        .collect::<Result<Vec<_>>>()?;
        Ok(Self {
            config,
            task: task.clone(),
            worker_id,
            source,
            request,
            request_bytes,
            manifest_bytes,
            objects,
            expectation,
        })
    }

    pub fn task(&self) -> &ClaimedTask {
        &self.task
    }
    pub fn worker_id(&self) -> &str {
        &self.worker_id
    }
    pub fn source(&self) -> &InputSourceIdentity {
        &self.source
    }

    pub async fn publish<G: InputPublicationGuard, T: InputPublicationTransport>(
        &self,
        guard: &G,
        transport: &T,
        cancellation: &CancellationToken,
    ) -> Result<PublishedInputCopy> {
        ensure!(
            transport.namespace() == self.source.namespace,
            "input_publication_namespace_mismatch"
        );
        self.check(guard, cancellation).await?;
        let scope = format!(
            "input-{}-{}-{}",
            self.task.id, self.task.attempt, self.task.lease_generation
        );
        let mut journal = PublicationJournal::create(
            &self.config.journal_root,
            &format!("{scope}.jsonl"),
            self.config.writer_uid,
        )
        .await?;
        let request_sha256 = hex::encode(Sha256::digest(&self.request_bytes));
        journal
            .append(
                json!({"phase":"prepared", "taskId":self.task.id, "attempt":self.task.attempt,
            "leaseGeneration":self.task.lease_generation, "namespace":self.source.namespace,
            "sourcePvcUid":self.source.pvc_uid, "requestSha256":request_sha256}),
            )
            .await?;
        let root = self.config.custody_root.clone();
        let uid = self.config.writer_uid;
        let staged_scope = scope.clone();
        let request = self.request_bytes.clone();
        let manifest = self.manifest_bytes.clone();
        let staged = tokio::task::spawn_blocking(move || -> Result<()> {
            publish_private(
                &root,
                &format!("{staged_scope}.request.json"),
                uid,
                &request,
            )?;
            publish_private(
                &root,
                &format!("{staged_scope}.manifest.json"),
                uid,
                &manifest,
            )
        })
        .await;
        if !matches!(staged, Ok(Ok(()))) {
            journal
                .append(json!({"phase":"stopped","reason":"private_request_staging_failed"}))
                .await?;
            return Err(anyhow!("input_publication_request_staging_failed"));
        }
        journal
            .append(json!({"phase":"request_retained","requestSha256":request_sha256}))
            .await?;
        let mut receipts = Vec::with_capacity(self.objects.len());
        for object in &self.objects {
            if self.check(guard, cancellation).await.is_err() {
                journal
                    .append(json!({"phase":"stopped","reason":"authority_or_cancellation_changed"}))
                    .await?;
                return Err(anyhow!("input_publication_authority_lost"));
            }
            journal
                .append(json!({"phase":"create_intent", "kind":object.kind(),
                "name":object.name(), "requestSha256":object.digest()}))
                .await?;
            let result = tokio::select! {
                biased;
                _ = cancellation.cancelled() => Err(anyhow!("input_publication_cancelled")),
                result = tokio::time::timeout(Duration::from_secs(20), transport.create(object, cancellation)) =>
                    result.map_err(|_| anyhow!("input_publication_create_deadline")).and_then(|r| r),
            };
            let receipt = match result {
                Ok(r)
                    if r.kind == object.kind()
                        && r.namespace == object.namespace()
                        && r.name == object.name()
                        && valid_uid(&r.uid)
                        && r.request_sha256 == object.digest() =>
                {
                    r
                }
                // The tenant has no room for this run's volumes. Objects created before it stay for the
                // recovery path; the record names the cause.
                Err(error) if error.to_string() == "input_capacity_exhausted" => {
                    journal
                        .append(json!({"phase":"stopped","reason":"capacity_exhausted",
                            "kind":object.kind(),"createdBefore":receipts.len()}))
                        .await?;
                    return Err(anyhow!("input_capacity_exhausted"));
                }
                _ => {
                    journal
                        .append(json!({"phase":"stopped","reason":"create_requires_recovery"}))
                        .await?;
                    return Err(anyhow!("input_publication_requires_recovery"));
                }
            };
            journal
                .append(json!({"phase":"created","object":receipt}))
                .await?;
            receipts.push(receipt);
        }
        if self.check(guard, cancellation).await.is_err() {
            journal
                .append(json!({"phase":"stopped","reason":"authority_or_cancellation_changed"}))
                .await?;
            return Err(anyhow!("input_publication_authority_lost"));
        }
        let mut expectation = self.expectation.clone();
        expectation.job_uid = receipts[4].uid.clone();
        let volumes = CreatedVolumeIdentities {
            input_uid: receipts[1].uid.clone(),
            artifact_uid: receipts[2].uid.clone(),
        };
        ensure!(
            volumes.input_uid != volumes.artifact_uid,
            "input_publication_volume_identity_conflict"
        );
        journal.append(json!({"phase":"published"})).await?;
        Ok(PublishedInputCopy {
            receipt: InputPublicationReceipt {
                task_id: self.task.id,
                attempt: self.task.attempt,
                lease_generation: self.task.lease_generation,
                request_sha256,
                objects: receipts,
            },
            request: self.request.clone(),
            expectation,
            volumes,
        })
    }

    async fn check<G: InputPublicationGuard>(
        &self,
        guard: &G,
        cancellation: &CancellationToken,
    ) -> Result<()> {
        tokio::select! {
            biased;
            _ = cancellation.cancelled() => Err(anyhow!("input_publication_cancelled")),
            result = tokio::time::timeout(Duration::from_secs(20), guard.check(self)) =>
                result.map_err(|_| anyhow!("input_publication_authority_deadline"))?
                    .map_err(|_| anyhow!("input_publication_authority_lost")),
        }
    }
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct InputPublicationReceipt {
    pub task_id: Uuid,
    pub attempt: i32,
    pub lease_generation: i64,
    pub request_sha256: String,
    pub objects: Vec<InputObjectReceipt>,
}

pub struct PublishedInputCopy {
    receipt: InputPublicationReceipt,
    request: CopySealRequest,
    expectation: InputCopyJobExpectation,
    volumes: CreatedVolumeIdentities,
}
impl fmt::Debug for PublishedInputCopy {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("PublishedInputCopy")
            .field("receipt", &self.receipt)
            .finish_non_exhaustive()
    }
}
impl PublishedInputCopy {
    pub fn receipt(&self) -> &InputPublicationReceipt {
        &self.receipt
    }
    pub fn volumes(&self) -> &CreatedVolumeIdentities {
        &self.volumes
    }
    pub fn expectation(&self) -> &InputCopyJobExpectation {
        &self.expectation
    }
    /// The completion observer consumes the very request published with the
    /// workload, not a later caller-supplied replacement manifest.
    pub async fn observe_seal<T: InputCopyObservationTransport>(
        &self,
        transport: &T,
    ) -> Result<ObservedInputSeal> {
        observe_input_seal(transport, &self.expectation, &self.request).await
    }
}

fn string<'a>(value: &'a Value, pointer: &str) -> Result<&'a str> {
    value
        .pointer(pointer)
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow!("input_publication_object_field_invalid"))
}
fn strings(value: &Value, pointer: &str) -> Result<Vec<String>> {
    value
        .pointer(pointer)
        .and_then(Value::as_array)
        .ok_or_else(|| anyhow!("input_publication_object_field_invalid"))?
        .iter()
        .map(|v| {
            v.as_str()
                .map(str::to_owned)
                .ok_or_else(|| anyhow!("input_publication_object_field_invalid"))
        })
        .collect()
}
fn job_expectation(
    job: &Value,
    authorization: &InputCopyAuthorization,
) -> Result<InputCopyJobExpectation> {
    let pod = &job["spec"]["template"]["spec"];
    let helper = &pod["containers"][0];
    let volumes = pod["volumes"]
        .as_array()
        .ok_or_else(|| anyhow!("input_publication_volumes_invalid"))?;
    let mounts = helper["volumeMounts"]
        .as_array()
        .ok_or_else(|| anyhow!("input_publication_volumes_invalid"))?;
    let mut expected = Vec::new();
    for volume in volumes {
        let name = string(volume, "/name")?;
        let mount = mounts
            .iter()
            .find(|m| m["name"].as_str() == Some(name))
            .ok_or_else(|| anyhow!("input_publication_volumes_invalid"))?;
        let source = if let Some(pvc) = volume.get("persistentVolumeClaim") {
            InputCopyVolumeSource::PersistentVolumeClaim {
                claim_name: string(pvc, "/claimName")?.into(),
            }
        } else if volume.pointer("/emptyDir/medium").and_then(Value::as_str) == Some("Memory") {
            InputCopyVolumeSource::EmptyDirMemory
        } else {
            InputCopyVolumeSource::ProjectedSecret {
                secret_name: string(volume, "/projected/sources/0/secret/name")?.into(),
            }
        };
        expected.push(InputCopyVolumeExpectation {
            role: name.into(),
            name: name.into(),
            mount_path: string(mount, "/mountPath")?.into(),
            sub_path: mount["subPath"].as_str().map(str::to_owned),
            read_only: mount["readOnly"].as_bool().unwrap_or(false),
            source,
        });
    }
    Ok(InputCopyJobExpectation {
        namespace: authorization.namespace.clone(),
        job_name: string(job, "/metadata/name")?.into(),
        job_uid: String::new(),
        run_id: authorization.run_id.clone(),
        principal_hash: authorization.principal_hash.clone(),
        runtime_class_name: string(pod, "/runtimeClassName")?.into(),
        helper: InputCopyContainerExpectation {
            name: string(helper, "/name")?.into(),
            image: string(helper, "/image")?.into(),
            command: strings(helper, "/command")?,
            args: strings(helper, "/args")?,
        },
        volumes: expected,
    })
}
