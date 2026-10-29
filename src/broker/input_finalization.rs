//! Fence one completed input copy into a sealed PVC and a staged admission.
//!
//! The caller supplies only records produced by the fixed publication, observer,
//! and canonical renderer paths. This module never creates compute.

use super::{
    admission_publication::{
        AdmissionPublicationConfig, AdmissionPublicationReceipt, PreparedAdmissionPublication,
        publish_private,
    },
    export_kubernetes::valid_uid,
    input_completion::ObservedInputSeal,
    input_publication::PublishedInputCopy,
    publication_journal::{PublicationJournal, valid_private_root},
    sandbox::{
        BrokerLeaseFence, CanonicalSandbox, SandboxAuthorization, SandboxBinding, build_sandbox,
        validate_custody_pvc,
    },
};
use anyhow::{Result, anyhow, ensure};
use async_trait::async_trait;
use serde::Serialize;
use serde_json::{Value, json};
use std::{fmt, path::PathBuf, time::Duration};
use tokio::time::{Instant, sleep, timeout, timeout_at};
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

const EFFECT_TIMEOUT: Duration = Duration::from_secs(20);
const DIGEST_ANNOTATIONS: [&str; 4] = [
    "input-digest",
    "input-staging-digest",
    "input-manifest-digest",
    "input-attestation-digest",
];

#[derive(Clone, Debug)]
pub struct InputFinalizationConfig {
    pub admission: AdmissionPublicationConfig,
    pub journal_root: PathBuf,
    pub poll_interval: Duration,
    pub max_wait: Duration,
}

/// Closed, UID- and resourceVersion-bound input seal mutation. The Kubernetes
/// adapter can borrow its fixed body but cannot add caller-provided patch paths.
pub struct InputSealMutation {
    namespace: String,
    name: String,
    uid: String,
    resource_version: String,
    body: Value,
}
impl fmt::Debug for InputSealMutation {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("InputSealMutation")
            .field("namespace", &self.namespace)
            .field("name", &self.name)
            .field("uid", &self.uid)
            .field("resource_version", &self.resource_version)
            .finish_non_exhaustive()
    }
}
impl InputSealMutation {
    pub fn namespace(&self) -> &str {
        &self.namespace
    }
    pub fn name(&self) -> &str {
        &self.name
    }
    pub fn uid(&self) -> &str {
        &self.uid
    }
    pub fn resource_version(&self) -> &str {
        &self.resource_version
    }
    #[allow(dead_code)] // consumed by the in-crate site Kubernetes adapter
    pub(crate) fn body(&self) -> &Value {
        &self.body
    }
}

/// Namespace-bound operations required only after a trusted copy completion.
/// Implementations must use the exact UID precondition and foreground deletion.
#[async_trait]
pub trait InputFinalizationTransport: Send + Sync {
    fn namespace(&self) -> &str;
    async fn get_pvc(&self, namespace: &str, name: &str) -> Result<Value>;
    async fn delete_completed_job(
        &self,
        namespace: &str,
        name: &str,
        uid: &str,
        cancellation: &CancellationToken,
    ) -> Result<()>;
    /// A same-name object after deletion is a replacement or incomplete cleanup,
    /// never a record to adopt.
    async fn get_job_after_cleanup(&self, namespace: &str, name: &str) -> Result<Option<Value>>;
    /// Looks up only the observed original Pod incarnation.
    async fn get_pod_after_cleanup(
        &self,
        namespace: &str,
        name: &str,
        uid: &str,
    ) -> Result<Option<Value>>;
    async fn apply_input_seal(
        &self,
        mutation: &InputSealMutation,
        cancellation: &CancellationToken,
    ) -> Result<Value>;
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct FinalizationObservation {
    task_id: Uuid,
    attempt: i32,
    lease_generation: i64,
    namespace: String,
    input_pvc_uid: String,
    artifact_pvc_uid: String,
    copy_job_uid: String,
    copy_pod_uid: String,
    input_digest: String,
    staging_digest: String,
    manifest_digest: String,
    attestation_digest: String,
}

pub struct InputFinalizer {
    config: InputFinalizationConfig,
}
impl InputFinalizer {
    pub fn new(config: InputFinalizationConfig) -> Result<Self> {
        ensure!(
            valid_private_root(&config.journal_root)
                && config.poll_interval >= Duration::from_millis(1)
                && config.poll_interval <= Duration::from_secs(5)
                && config.max_wait >= config.poll_interval
                && config.max_wait <= Duration::from_secs(3600),
            "input_finalization_config_invalid"
        );
        Ok(Self { config })
    }

    #[allow(clippy::too_many_arguments)]
    pub async fn finalize<L: BrokerLeaseFence, T: InputFinalizationTransport>(
        &self,
        task: &super::model::ClaimedTask,
        worker_id: &str,
        binding: &SandboxBinding,
        published: &PublishedInputCopy,
        seal: &ObservedInputSeal,
        canonical: &CanonicalSandbox,
        authorization: &SandboxAuthorization,
        lease: &L,
        transport: &T,
        cancellation: &CancellationToken,
    ) -> Result<AdmissionPublicationReceipt> {
        self.validate_binding(
            task,
            worker_id,
            binding,
            published,
            seal,
            canonical,
            authorization,
            transport,
        )?;
        self.fence(task, worker_id, lease, cancellation).await?;

        let (input_before, artifacts_before) = self.read_volumes(published, transport).await?;
        let expected_input = promoted_view(&input_before, authorization)?;
        // Reject a malformed canonical authorization before destructive cleanup.
        PreparedAdmissionPublication::prepare(
            self.config.admission.clone(),
            task,
            binding,
            authorization,
            canonical,
            seal,
            published.volumes(),
            &expected_input,
            &artifacts_before,
        )
        .map_err(|_| anyhow!("input_finalization_prevalidation_failed"))?;

        let scope = scope(task);
        let mut journal = PublicationJournal::create(
            &self.config.journal_root,
            &format!("{scope}.jsonl"),
            self.config.admission.writer_uid,
        )
        .await?;
        journal.append(json!({"phase":"prepared","taskId":task.id,"attempt":task.attempt,
            "leaseGeneration":task.lease_generation,"copyJobUid":seal.job_uid(),"copyPodUid":seal.pod_uid(),
            "inputPvcUid":published.volumes().input_uid,"artifactPvcUid":published.volumes().artifact_uid})).await?;

        self.fence(task, worker_id, lease, cancellation).await?;
        self.persist_evidence(&scope, task, published, seal, authorization)
            .await?;
        journal.append(json!({"phase":"evidence_retained","copyJobUid":seal.job_uid(),"copyPodUid":seal.pod_uid()})).await?;
        self.fence(task, worker_id, lease, cancellation).await?;

        journal
            .append(
                json!({"phase":"delete_intent","jobName":published.expectation().job_name,
            "jobUid":published.expectation().job_uid,"propagation":"Foreground"}),
            )
            .await?;
        self.effect(
            cancellation,
            transport.delete_completed_job(
                &published.expectation().namespace,
                &published.expectation().job_name,
                &published.expectation().job_uid,
                cancellation,
            ),
            "input_finalization_delete_failed",
        )
        .await?;
        journal
            .append(json!({"phase":"delete_requested","jobUid":published.expectation().job_uid}))
            .await?;
        self.wait_for_cleanup(
            task,
            worker_id,
            lease,
            published,
            seal,
            transport,
            cancellation,
        )
        .await?;
        journal.append(json!({"phase":"copy_cleanup_confirmed","jobUid":seal.job_uid(),"podUid":seal.pod_uid()})).await?;
        self.fence(task, worker_id, lease, cancellation).await?;

        let (input_staging, _artifacts_current) = self.read_volumes(published, transport).await?;
        let mutation = seal_mutation(&input_staging, authorization)?;
        journal.append(json!({"phase":"seal_intent","inputPvcUid":mutation.uid(),
            "resourceVersion":mutation.resource_version(),"inputDigest":authorization.input_digest,
            "stagingDigest":authorization.staging_digest,"manifestDigest":authorization.manifest_digest,
            "attestationDigest":authorization.attestation_digest})).await?;
        let sealed = self
            .effect(
                cancellation,
                transport.apply_input_seal(&mutation, cancellation),
                "input_finalization_seal_failed",
            )
            .await?;
        validate_promoted_result(&sealed, &mutation, authorization)?;
        journal.append(json!({"phase":"sealed","inputPvcUid":mutation.uid(),
            "resourceVersion":text(&sealed, "/metadata/resourceVersion", "input_finalization_seal_result_invalid")?})).await?;
        self.fence(task, worker_id, lease, cancellation).await?;

        let (input_after, artifacts_after) = self.read_volumes(published, transport).await?;
        let prepared = PreparedAdmissionPublication::prepare(
            self.config.admission.clone(),
            task,
            binding,
            authorization,
            canonical,
            seal,
            published.volumes(),
            &input_after,
            &artifacts_after,
        )
        .map_err(|_| anyhow!("input_finalization_admission_preparation_failed"))?;
        journal
            .append(
                json!({"phase":"admission_intent","copyJobUid":seal.job_uid(),
            "copyPodUid":seal.pod_uid(),"inputPvcUid":published.volumes().input_uid,
            "artifactPvcUid":published.volumes().artifact_uid}),
            )
            .await?;
        let receipt = prepared
            .publish(lease, worker_id, cancellation)
            .await
            .map_err(|_| anyhow!("input_finalization_admission_publish_failed"))?;
        journal
            .append(
                json!({"phase":"admission_published","admissionSha256":receipt.admission_sha256,
            "originalSealSha256":receipt.original_seal_sha256}),
            )
            .await?;
        self.fence(task, worker_id, lease, cancellation).await?;
        Ok(receipt)
    }

    #[expect(
        clippy::too_many_arguments,
        reason = "pre-cleanup validation must bind task, copy receipt, seal, canonical authorization, and transport separately"
    )]
    fn validate_binding<T: InputFinalizationTransport>(
        &self,
        task: &super::model::ClaimedTask,
        worker_id: &str,
        binding: &SandboxBinding,
        published: &PublishedInputCopy,
        seal: &ObservedInputSeal,
        canonical: &CanonicalSandbox,
        authorization: &SandboxAuthorization,
        transport: &T,
    ) -> Result<()> {
        ensure!(
            !worker_id.is_empty()
                && worker_id.len() <= 128
                && worker_id
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
                && transport.namespace() == binding.namespace
                && published.receipt().task_id == task.id
                && published.receipt().attempt == task.attempt
                && published.receipt().lease_generation == task.lease_generation
                && published.expectation().namespace == binding.namespace
                && published.expectation().run_id == authorization.run_id
                && published.expectation().principal_hash == binding.principal_hash
                && published.expectation().job_uid == seal.job_uid()
                && published.volumes().input_uid != published.volumes().artifact_uid
                && valid_uid(&published.volumes().input_uid)
                && valid_uid(&published.volumes().artifact_uid),
            "input_finalization_binding_invalid"
        );
        // This reuses the canonical renderer's full task/binding/authorization contract.
        build_sandbox(task, binding, canonical, authorization)
            .map_err(|_| anyhow!("input_finalization_canonical_binding_invalid"))?;
        let a = seal.attestation();
        ensure!(
            a.run_id == authorization.run_id
                && a.namespace == authorization.namespace
                && a.principal_hash == authorization.principal_hash
                && a.input_digest == authorization.input_digest
                && a.manifest_digest == authorization.manifest_digest
                && a.attestation_digest == authorization.attestation_digest,
            "input_finalization_seal_authorization_mismatch"
        );
        Ok(())
    }

    async fn persist_evidence(
        &self,
        scope: &str,
        task: &super::model::ClaimedTask,
        published: &PublishedInputCopy,
        seal: &ObservedInputSeal,
        authorization: &SandboxAuthorization,
    ) -> Result<()> {
        let observation = FinalizationObservation {
            task_id: task.id,
            attempt: task.attempt,
            lease_generation: task.lease_generation,
            namespace: authorization.namespace.clone(),
            input_pvc_uid: published.volumes().input_uid.clone(),
            artifact_pvc_uid: published.volumes().artifact_uid.clone(),
            copy_job_uid: seal.job_uid().into(),
            copy_pod_uid: seal.pod_uid().into(),
            input_digest: authorization.input_digest.clone(),
            staging_digest: authorization.staging_digest.clone(),
            manifest_digest: authorization.manifest_digest.clone(),
            attestation_digest: authorization.attestation_digest.clone(),
        };
        let observation = serde_json::to_vec(&observation)
            .map_err(|_| anyhow!("input_finalization_observation_invalid"))?;
        let root = self.config.admission.custody_root.clone();
        let uid = self.config.admission.writer_uid;
        let receipt = seal.original_receipt().to_vec();
        let receipt_name = format!("{scope}.original-seal.json");
        let observation_name = format!("{scope}.copy-observation.json");
        timeout(
            EFFECT_TIMEOUT,
            tokio::task::spawn_blocking(move || -> Result<()> {
                publish_private(&root, &receipt_name, uid, &receipt)?;
                publish_private(&root, &observation_name, uid, &observation)
            }),
        )
        .await
        .map_err(|_| anyhow!("input_finalization_evidence_deadline"))?
        .map_err(|_| anyhow!("input_finalization_evidence_failed"))??;
        Ok(())
    }

    async fn read_volumes<T: InputFinalizationTransport>(
        &self,
        published: &PublishedInputCopy,
        transport: &T,
    ) -> Result<(Value, Value)> {
        let run = &published.expectation().run_id;
        let ns = &published.expectation().namespace;
        let input = self
            .effect_no_cancel(
                transport.get_pvc(ns, &format!("input-{run}")),
                "input_finalization_input_pvc_unavailable",
            )
            .await?;
        let artifacts = self
            .effect_no_cancel(
                transport.get_pvc(ns, &format!("artifacts-{run}")),
                "input_finalization_artifact_pvc_unavailable",
            )
            .await?;
        validate_volume(
            &input,
            run,
            "immutable-input",
            &published.volumes().input_uid,
            false,
        )?;
        validate_volume(
            &artifacts,
            run,
            "durable-artifacts",
            &published.volumes().artifact_uid,
            false,
        )?;
        Ok((input, artifacts))
    }

    #[expect(
        clippy::too_many_arguments,
        reason = "cleanup polling keeps lease, published identity, observed seal, transport, and cancellation explicit"
    )]
    async fn wait_for_cleanup<L: BrokerLeaseFence, T: InputFinalizationTransport>(
        &self,
        task: &super::model::ClaimedTask,
        worker_id: &str,
        lease: &L,
        published: &PublishedInputCopy,
        seal: &ObservedInputSeal,
        transport: &T,
        cancellation: &CancellationToken,
    ) -> Result<()> {
        let deadline = Instant::now() + self.config.max_wait;
        timeout_at(deadline, async {
            loop {
                self.fence(task, worker_id, lease, cancellation).await?;
                if cancellation.is_cancelled() {
                    return Err(anyhow!("input_finalization_cancelled"));
                }
                let job = self
                    .effect_no_cancel(
                        transport.get_job_after_cleanup(
                            &published.expectation().namespace,
                            &published.expectation().job_name,
                        ),
                        "input_finalization_cleanup_observation_failed",
                    )
                    .await?;
                if let Some(job) = job {
                    ensure!(
                        text(
                            &job,
                            "/metadata/uid",
                            "input_finalization_cleanup_job_identity_invalid"
                        )? == published.expectation().job_uid,
                        "input_finalization_job_replaced_or_remaining"
                    );
                    tokio::select! {
                        biased;
                        _ = cancellation.cancelled() => return Err(anyhow!("input_finalization_cancelled")),
                        _ = sleep(self.config.poll_interval) => {}
                    }
                    continue;
                }
                let pod = self
                    .effect_no_cancel(
                        transport.get_pod_after_cleanup(&published.expectation().namespace, &seal.metadata().pod_name, seal.pod_uid()),
                        "input_finalization_cleanup_observation_failed",
                    )
                    .await?;
                if pod.is_none() {
                    return Ok(());
                }
                tokio::select! {
                    biased;
                    _ = cancellation.cancelled() => return Err(anyhow!("input_finalization_cancelled")),
                    _ = sleep(self.config.poll_interval) => {}
                }
            }
        })
        .await
        .map_err(|_| anyhow!("input_finalization_cleanup_deadline"))?
    }

    async fn fence<L: BrokerLeaseFence>(
        &self,
        task: &super::model::ClaimedTask,
        worker: &str,
        lease: &L,
        cancellation: &CancellationToken,
    ) -> Result<()> {
        let result = async {
            ensure!(
                !lease
                    .cancel_requested(task.id, worker, task.lease_generation)
                    .await?,
                "input_finalization_cancelled"
            );
            ensure!(
                lease
                    .renew_lease(
                        task.id,
                        worker,
                        task.lease_generation,
                        self.config.admission.lease_seconds
                    )
                    .await?,
                "input_finalization_lease_lost"
            );
            Ok(())
        };
        tokio::select! {
            biased;
            _ = cancellation.cancelled() => Err(anyhow!("input_finalization_cancelled")),
            result = timeout(EFFECT_TIMEOUT, result) => result.map_err(|_| anyhow!("input_finalization_authority_deadline"))?
                .map_err(|_| anyhow!("input_finalization_authority_lost")),
        }
    }

    async fn effect<T, U>(
        &self,
        cancellation: &CancellationToken,
        future: T,
        error: &'static str,
    ) -> Result<U>
    where
        T: std::future::Future<Output = Result<U>>,
    {
        tokio::select! {
            biased;
            _ = cancellation.cancelled() => Err(anyhow!("input_finalization_cancelled")),
            result = timeout(EFFECT_TIMEOUT, future) => result.map_err(|_| anyhow!("input_finalization_effect_deadline"))?
                .map_err(|_| anyhow!(error)),
        }
    }
    async fn effect_no_cancel<T, U>(&self, future: T, error: &'static str) -> Result<U>
    where
        T: std::future::Future<Output = Result<U>>,
    {
        timeout(EFFECT_TIMEOUT, future)
            .await
            .map_err(|_| anyhow!("input_finalization_effect_deadline"))?
            .map_err(|_| anyhow!(error))
    }
}

fn scope(task: &super::model::ClaimedTask) -> String {
    format!(
        "input-finalize-{}-{}-{}",
        task.id, task.attempt, task.lease_generation
    )
}
fn text<'a>(value: &'a Value, pointer: &str, error: &'static str) -> Result<&'a str> {
    value
        .pointer(pointer)
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .ok_or_else(|| anyhow!(error))
}
fn validate_volume(pvc: &Value, run: &str, role: &str, uid: &str, sealed: bool) -> Result<()> {
    let phase = text(
        pvc,
        "/status/phase",
        "input_finalization_volume_state_invalid",
    )?;
    // The output volume has no consumer until the Sandbox is admitted. A
    // WaitForFirstConsumer storage class must be allowed to bind at scheduling.
    // The input has already been written and must remain bound before sealing.
    let usable = phase == "Bound" || (role == "durable-artifacts" && phase == "Pending");
    ensure!(
        text(
            pvc,
            "/metadata/uid",
            "input_finalization_volume_identity_invalid"
        )? == uid
            && pvc
                .pointer("/metadata/deletionTimestamp")
                .is_none_or(Value::is_null)
            && usable,
        "input_finalization_volume_identity_invalid"
    );
    validate_custody_pvc(pvc, run, role, sealed, None)
        .map_err(|_| anyhow!("input_finalization_volume_contract_invalid"))
}
fn promoted_view(staging: &Value, authorization: &SandboxAuthorization) -> Result<Value> {
    // Build the exact expected post-CAS view without changing the live object.
    // This preserves the renderer-created role/run/retention fields for the same
    // admission validation used after the real mutation.
    seal_mutation(staging, authorization)?;
    let mut promoted = staging.clone();
    let labels = promoted
        .pointer_mut("/metadata/labels")
        .and_then(Value::as_object_mut)
        .ok_or_else(|| anyhow!("input_finalization_input_identity_invalid"))?;
    labels.insert("sandbox.enrichedlab.org/seal-state".into(), json!("sealed"));
    let annotations = promoted
        .pointer_mut("/metadata/annotations")
        .and_then(Value::as_object_mut)
        .ok_or_else(|| anyhow!("input_finalization_input_identity_invalid"))?;
    for (key, value) in [
        ("input-digest", &authorization.input_digest),
        ("input-staging-digest", &authorization.staging_digest),
        ("input-manifest-digest", &authorization.manifest_digest),
        (
            "input-attestation-digest",
            &authorization.attestation_digest,
        ),
    ] {
        annotations.insert(format!("sandbox.enrichedlab.org/{key}"), json!(value));
    }
    Ok(promoted)
}
fn seal_mutation(
    staging: &Value,
    authorization: &SandboxAuthorization,
) -> Result<InputSealMutation> {
    validate_volume(
        staging,
        &authorization.run_id,
        "immutable-input",
        text(
            staging,
            "/metadata/uid",
            "input_finalization_input_identity_invalid",
        )?,
        false,
    )?;
    ensure!(
        text(
            staging,
            "/metadata/namespace",
            "input_finalization_input_identity_invalid"
        )? == authorization.namespace
            && staging
                .pointer("/metadata/labels/sandbox.enrichedlab.org~1seal-state")
                .and_then(Value::as_str)
                == Some("staging"),
        "input_finalization_input_identity_invalid"
    );
    let uid = text(
        staging,
        "/metadata/uid",
        "input_finalization_input_identity_invalid",
    )?
    .to_owned();
    let resource_version = text(
        staging,
        "/metadata/resourceVersion",
        "input_finalization_resource_version_invalid",
    )?
    .to_owned();
    let name = text(
        staging,
        "/metadata/name",
        "input_finalization_input_identity_invalid",
    )?
    .to_owned();
    let namespace = authorization.namespace.clone();
    let body = json!({"apiVersion":"v1","kind":"PersistentVolumeClaim","metadata":{
        "name":name,"namespace":namespace,"uid":uid,"resourceVersion":resource_version,
        "labels":{"sandbox.enrichedlab.org/seal-state":"sealed"},
        "annotations":{
            "sandbox.enrichedlab.org/input-digest":authorization.input_digest,
            "sandbox.enrichedlab.org/input-staging-digest":authorization.staging_digest,
            "sandbox.enrichedlab.org/input-manifest-digest":authorization.manifest_digest,
            "sandbox.enrichedlab.org/input-attestation-digest":authorization.attestation_digest
        }
    }});
    Ok(InputSealMutation {
        namespace,
        name,
        uid,
        resource_version,
        body,
    })
}
fn validate_promoted_result(
    value: &Value,
    mutation: &InputSealMutation,
    authorization: &SandboxAuthorization,
) -> Result<()> {
    ensure!(
        text(
            value,
            "/metadata/name",
            "input_finalization_seal_result_invalid"
        )? == mutation.name
            && text(
                value,
                "/metadata/namespace",
                "input_finalization_seal_result_invalid"
            )? == mutation.namespace
            && text(
                value,
                "/metadata/uid",
                "input_finalization_seal_result_invalid"
            )? == mutation.uid,
        "input_finalization_seal_result_invalid"
    );
    validate_volume(
        value,
        &authorization.run_id,
        "immutable-input",
        &mutation.uid,
        true,
    )?;
    for (key, expected) in [
        (DIGEST_ANNOTATIONS[0], &authorization.input_digest),
        (DIGEST_ANNOTATIONS[1], &authorization.staging_digest),
        (DIGEST_ANNOTATIONS[2], &authorization.manifest_digest),
        (DIGEST_ANNOTATIONS[3], &authorization.attestation_digest),
    ] {
        ensure!(
            value
                .pointer(&format!(
                    "/metadata/annotations/sandbox.enrichedlab.org~1{key}"
                ))
                .and_then(Value::as_str)
                == Some(expected.as_str()),
            "input_finalization_seal_result_invalid"
        );
    }
    Ok(())
}
