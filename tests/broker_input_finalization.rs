#![cfg(unix)]

use anyhow::{Result, bail, ensure};
use async_trait::async_trait;
use enrichedlab_agent_sandbox::{
    broker::{
        admission::load,
        admission_publication::AdmissionPublicationConfig,
        input_completion::{InputCopyObservationTransport, ObservedInputSeal},
        input_finalization::{
            InputFinalizationConfig, InputFinalizationTransport, InputFinalizer, InputSealMutation,
        },
        input_publication::{
            InputObject, InputObjectKind, InputObjectReceipt, InputPublicationConfig,
            InputPublicationTransport, InputSourceIdentity, InputSourceLeaseGuard,
            InputSourceObservationTransport, PreparedInputPublication,
        },
        input_workload::{
            AuthorizedInputCopy, CustodyPvcAccessMode, InputCopyAuthorization, InputWorkloadConfig,
            InputWorkloadRenderer,
        },
        model::ClaimedTask,
        sandbox::{
            BrokerLeaseFence, CanonicalSandbox, RetainedArtifact, SandboxAuthorization,
            SandboxBinding,
        },
    },
    custody::{
        CopySealRequest, InputManifest, SealAttestation, canonical_document_digest, copy_and_seal,
    },
};
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    fs,
    os::unix::fs::PermissionsExt,
    path::PathBuf,
    sync::{
        Mutex,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
};
use tempfile::TempDir;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

const IMAGE: &str = "registry.test/custodian@sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
const PAYLOAD: &[u8] = include_bytes!("fixtures/admission/fixture");

struct Fixture {
    _temp: TempDir,
    config: InputPublicationConfig,
    task: ClaimedTask,
    binding: SandboxBinding,
    authorization: InputCopyAuthorization,
    request: CopySealRequest,
    source: InputSourceIdentity,
}
impl Fixture {
    fn new() -> Self {
        let temp = TempDir::new().unwrap();
        let root = fs::canonicalize(temp.path()).unwrap();
        for child in ["journal", "custody", "source", "destination", "helper"] {
            fs::create_dir(root.join(child)).unwrap();
            fs::set_permissions(root.join(child), fs::Permissions::from_mode(0o700)).unwrap();
        }
        let record: Value =
            serde_json::from_str(include_str!("fixtures/admission/staged_admission.json")).unwrap();
        let seal: SealAttestation = serde_json::from_value(record["sealedInput"].clone()).unwrap();
        let manifest: InputManifest =
            serde_json::from_str(include_str!("fixtures/admission/input_manifest.json")).unwrap();
        let request = CopySealRequest {
            schema_version: seal.schema_version,
            staging_id: seal.staging_id,
            run_id: seal.run_id,
            tenant_id: seal.tenant_id,
            namespace: seal.namespace,
            principal_hash: seal.principal_hash,
            input_ref: seal.input_ref,
            input_digest: seal.input_digest,
            pvc_name: seal.pvc_name,
            manifest_ref: seal.manifest_ref,
            attestation_ref: seal.attestation_ref,
            source_root: "/source".into(),
            destination_root: "/destination".into(),
            custody_root: "/custody".into(),
            files: manifest.files,
            max_files: Some(1),
            max_bytes: Some(PAYLOAD.len() as u64),
            admitted_files: vec![],
        };
        let task = ClaimedTask {
            id: Uuid::nil(),
            principal_id: Uuid::nil(),
            group_key: "research".into(),
            profile_key: "reference".into(),
            objective: String::new(),
            logical_agent: "reference".into(),
            lease_generation: 7,
            attempt: 2,
            max_task_attempts: 3,
            max_provider_attempts: 1,
            harness: String::new(),
            thinking: None,
            route: String::new(),
            controller_profile: String::new(),
            gateway_profile: String::new(),
            image_ref: String::new(),
            cpu_limit: String::new(),
            memory_limit: String::new(),
            state_profile: String::new(),
            role: "research".into(),
            max_run_seconds: 300,
        };
        let binding = SandboxBinding {
            principal_id: task.principal_id,
            principal_hash: request.principal_hash.clone(),
            group_key: task.group_key.clone(),
            profile_key: task.profile_key.clone(),
            profile_id: "example-kata".into(),
            lab_ref: "lab://example/research".into(),
            namespace: request.namespace.clone(),
            runtime_class_name: "kata-clh".into(),
        };
        let authorization = InputCopyAuthorization {
            task_id: task.id,
            attempt: task.attempt,
            lease_generation: task.lease_generation,
            run_id: request.run_id.clone(),
            tenant_id: request.tenant_id.clone(),
            source_run_name: "source-run".into(),
            source_pvc_name: "input-source-run".into(),
            principal_hash: request.principal_hash.clone(),
            profile_id: binding.profile_id.clone(),
            namespace: binding.namespace.clone(),
            custody_retention_until: "2099-01-01T00:00:00Z".into(),
            input_digest: request.input_digest.clone(),
        };
        let source = InputSourceIdentity {
            namespace: request.namespace.clone(),
            pvc_name: "input-source-run".into(),
            pvc_uid: "approved-source-uid".into(),
            run_id: "source-run".into(),
            tenant_id: request.tenant_id.clone(),
            principal_hash: request.principal_hash.clone(),
        };
        Self {
            config: InputPublicationConfig {
                workload: InputWorkloadConfig {
                    site: Default::default(),
                    runtime_class_name: "kata-clh".into(),
                    image: IMAGE.into(),
                    pool_selector: BTreeMap::from([("workload".into(), "sandbox".into())]),
                    cpu_request_millis: 100,
                    cpu_limit_millis: 500,
                    memory_request_mib: 128,
                    memory_limit_mib: 512,
                    active_deadline_seconds: 300,
                    custody_memory_mib: 16,
                    input_storage_class_name: "example-input".into(),
                    artifact_storage_class_name: "example-output".into(),
                    input_storage_mib: 128,
                    artifact_storage_mib: 128,
                    custody_pvc_access_mode: CustodyPvcAccessMode::ReadWriteOnce,
                    max_request_bytes: 768 * 1024,
                },
                journal_root: root.join("journal"),
                custody_root: root.join("custody"),
                writer_uid: unsafe { libc::geteuid() },
            },
            _temp: temp,
            task,
            binding,
            authorization,
            request,
            source,
        }
    }
    fn copy(&self) -> AuthorizedInputCopy {
        AuthorizedInputCopy::new(
            self.source.run_id.clone(),
            self.source.pvc_name.clone(),
            "copy-request".into(),
            self.request.clone(),
        )
        .unwrap()
    }
    fn prepare(&self) -> Result<PreparedInputPublication> {
        PreparedInputPublication::prepare(
            self.config.clone(),
            &self.task,
            "worker-example".into(),
            &self.binding,
            &self.authorization,
            &self.copy(),
            self.source.clone(),
        )
    }
    fn scope(&self) -> String {
        format!(
            "input-{}-{}-{}",
            self.task.id, self.task.attempt, self.task.lease_generation
        )
    }
    fn request_path(&self) -> PathBuf {
        self.config
            .custody_root
            .join(format!("{}.request.json", self.scope()))
    }
    fn events(&self) -> Vec<Value> {
        fs::read_to_string(
            self.config
                .journal_root
                .join(format!("{}.jsonl", self.scope())),
        )
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect()
    }
    fn source_object(&self) -> Value {
        json!({"metadata":{"name":self.source.pvc_name,"namespace":self.source.namespace,
        "uid":self.source.pvc_uid,"labels":{"sandbox.enrichedlab.org/run-id":self.source.run_id,
        "sandbox.enrichedlab.org/tenant":self.source.tenant_id,"sandbox.enrichedlab.org/principal-hash":self.source.principal_hash,
        "sandbox.enrichedlab.org/contract-version":"v3","sandbox.enrichedlab.org/volume-role":"immutable-input",
        "sandbox.enrichedlab.org/seal-state":"sealed","sandbox.enrichedlab.org/materialization-mode":"checksummed-copy"}},"status":{"phase":"Bound"}})
    }
}

struct Authority {
    pvc: Value,
    writer: AtomicBool,
    renewals: AtomicUsize,
    reject_at: usize,
    cancel: bool,
}
#[async_trait]
impl InputSourceObservationTransport for Authority {
    async fn get_source_pvc(&self, ns: &str, name: &str) -> Result<Value> {
        assert_eq!(self.pvc["metadata"]["namespace"], ns);
        assert_eq!(self.pvc["metadata"]["name"], name);
        Ok(self.pvc.clone())
    }
    async fn source_has_active_writer(&self, _: &str, _: &str) -> Result<bool> {
        Ok(self.writer.load(Ordering::SeqCst))
    }
}
#[async_trait]
impl BrokerLeaseFence for Authority {
    async fn mark_running(&self, _: Uuid, _: &str, _: i64) -> Result<bool> {
        bail!("must_not_start_compute")
    }
    async fn cancel_requested(&self, id: Uuid, worker: &str, generation: i64) -> Result<bool> {
        assert_eq!((id, worker, generation), (Uuid::nil(), "worker-example", 7));
        Ok(self.cancel)
    }
    async fn renew_lease(&self, _: Uuid, _: &str, _: i64, seconds: i32) -> Result<bool> {
        assert_eq!(seconds, 60);
        Ok(self.renewals.fetch_add(1, Ordering::SeqCst) + 1 != self.reject_at)
    }
    async fn register_artifact(
        &self,
        _: Uuid,
        _: &str,
        _: i64,
        _: &RetainedArtifact,
    ) -> Result<()> {
        bail!("unexpected_artifact")
    }
    async fn finish_task(
        &self,
        _: Uuid,
        _: &str,
        _: i64,
        _: bool,
        _: &str,
        _: &str,
    ) -> Result<bool> {
        bail!("unexpected_finish")
    }
}
impl Authority {
    fn new(f: &Fixture) -> Self {
        Self {
            pvc: f.source_object(),
            writer: AtomicBool::new(false),
            renewals: AtomicUsize::new(0),
            reject_at: usize::MAX,
            cancel: false,
        }
    }
}

struct Creates<'a> {
    fixture: &'a Fixture,
    created: Mutex<Vec<InputObjectKind>>,
    fail_at: usize,
    bad_uid: bool,
}
#[async_trait]
impl InputPublicationTransport for Creates<'_> {
    fn namespace(&self) -> &str {
        &self.fixture.binding.namespace
    }
    async fn create(
        &self,
        object: &InputObject,
        _: &CancellationToken,
    ) -> Result<InputObjectReceipt> {
        let events = self.fixture.events();
        assert_eq!(events.last().unwrap()["phase"], "create_intent");
        assert_eq!(events.last().unwrap()["requestSha256"], object.digest());
        assert!(self.fixture.request_path().exists());
        let mut created = self.created.lock().unwrap();
        if created.len() == self.fail_at {
            bail!("synthetic_private_failure_body")
        }
        created.push(object.kind());
        Ok(InputObjectReceipt {
            kind: object.kind(),
            namespace: object.namespace().into(),
            name: object.name().into(),
            uid: if self.bad_uid {
                "/invalid".into()
            } else {
                format!("created-{}", created.len())
            },
            request_sha256: object.digest().into(),
        })
    }
}
impl<'a> Creates<'a> {
    fn new(f: &'a Fixture) -> Self {
        Self {
            fixture: f,
            created: Mutex::new(Vec::new()),
            fail_at: usize::MAX,
            bad_uid: false,
        }
    }
}

struct Completed {
    job: Value,
    pod: Value,
    receipt: Vec<u8>,
}
#[async_trait]
impl InputCopyObservationTransport for Completed {
    async fn get_job(&self, _: &str, _: &str) -> Result<Value> {
        Ok(self.job.clone())
    }
    async fn get_controlled_pod(&self, _: &str, _: &str) -> Result<Option<Value>> {
        Ok(Some(self.pod.clone()))
    }
    async fn helper_stdout(&self, _: &str, _: &str, _: &str) -> Result<Vec<u8>> {
        Ok(self.receipt.clone())
    }
}

struct FinalizationTransport {
    namespace: String,
    input: Mutex<Value>,
    artifacts: Mutex<Value>,
    job_present: AtomicBool,
    pod_present: AtomicBool,
    pending_original_job_observations: AtomicUsize,
    cleanup_job_delay: Mutex<std::time::Duration>,
    replace_job: bool,
    malformed_job: bool,
    seal_fail: bool,
    sealed: AtomicBool,
    authorization: SandboxAuthorization,
}
impl FinalizationTransport {
    fn new(
        f: &Fixture,
        published: &enrichedlab_agent_sandbox::broker::input_publication::PublishedInputCopy,
        replace_job: bool,
        malformed_job: bool,
        seal_fail: bool,
        authorization: SandboxAuthorization,
    ) -> Self {
        let workload = InputWorkloadRenderer::new(f.config.workload.clone())
            .unwrap()
            .render(&f.task, &f.binding, &f.authorization, &f.copy())
            .unwrap();
        let mut input = workload.input_pvc;
        let mut artifacts = workload.artifact_pvc;
        input["metadata"]["uid"] = json!(published.volumes().input_uid);
        input["metadata"]["resourceVersion"] = json!("1");
        input["status"] = json!({"phase":"Bound"});
        artifacts["metadata"]["uid"] = json!(published.volumes().artifact_uid);
        artifacts["metadata"]["resourceVersion"] = json!("1");
        // No output consumer exists yet with WaitForFirstConsumer storage.
        artifacts["status"] = json!({"phase":"Pending"});
        Self {
            namespace: f.binding.namespace.clone(),
            input: Mutex::new(input),
            artifacts: Mutex::new(artifacts),
            job_present: AtomicBool::new(true),
            pod_present: AtomicBool::new(true),
            pending_original_job_observations: AtomicUsize::new(0),
            cleanup_job_delay: Mutex::new(std::time::Duration::ZERO),
            replace_job,
            malformed_job,
            seal_fail,
            sealed: AtomicBool::new(false),
            authorization,
        }
    }
}
#[async_trait]
impl InputFinalizationTransport for FinalizationTransport {
    fn namespace(&self) -> &str {
        &self.namespace
    }
    async fn get_pvc(&self, namespace: &str, name: &str) -> Result<Value> {
        if namespace != self.namespace {
            bail!("foreign_namespace")
        }
        if name.starts_with("input-") {
            Ok(self.input.lock().unwrap().clone())
        } else if name.starts_with("artifacts-") {
            Ok(self.artifacts.lock().unwrap().clone())
        } else {
            bail!("unexpected_pvc")
        }
    }
    async fn delete_completed_job(
        &self,
        namespace: &str,
        _: &str,
        uid: &str,
        _: &CancellationToken,
    ) -> Result<()> {
        assert_eq!(namespace, self.namespace);
        assert_eq!(uid, "created-5");
        self.job_present.store(false, Ordering::SeqCst);
        self.pod_present.store(false, Ordering::SeqCst);
        Ok(())
    }
    async fn get_job_after_cleanup(&self, _: &str, _: &str) -> Result<Option<Value>> {
        let delay = *self.cleanup_job_delay.lock().unwrap();
        if !delay.is_zero() {
            tokio::time::sleep(delay).await;
        }
        if self.replace_job {
            return Ok(Some(json!({"metadata":{"uid":"replacement-job"}})));
        }
        if self.malformed_job {
            return Ok(Some(json!({"metadata":{}})));
        }
        if self
            .pending_original_job_observations
            .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |remaining| {
                remaining.checked_sub(1)
            })
            .is_ok()
        {
            return Ok(Some(json!({"metadata":{"uid":"created-5"}})));
        }
        Ok(self
            .job_present
            .load(Ordering::SeqCst)
            .then(|| json!({"metadata":{"uid":"created-5"}})))
    }
    async fn get_pod_after_cleanup(&self, _: &str, _: &str, _: &str) -> Result<Option<Value>> {
        Ok(self
            .pod_present
            .load(Ordering::SeqCst)
            .then(|| json!({"metadata":{"uid":"copy-pod-uid"}})))
    }
    async fn apply_input_seal(
        &self,
        mutation: &InputSealMutation,
        _: &CancellationToken,
    ) -> Result<Value> {
        assert_eq!(mutation.namespace(), self.namespace);
        if self.seal_fail {
            bail!("synthetic_seal_transport_failure");
        }
        let mut input = self.input.lock().unwrap();
        ensure!(
            input["metadata"]["uid"] == mutation.uid()
                && input["metadata"]["resourceVersion"] == mutation.resource_version(),
            "cas_failed"
        );
        input["metadata"]["labels"]["sandbox.enrichedlab.org/seal-state"] = json!("sealed");
        for key in [
            "input-digest",
            "input-staging-digest",
            "input-manifest-digest",
            "input-attestation-digest",
        ] {
            input["metadata"]["annotations"][format!("sandbox.enrichedlab.org/{key}")] = match key {
                "input-digest" => json!(self.authorization.input_digest),
                "input-staging-digest" => {
                    json!(self.authorization.staging_digest)
                }
                "input-manifest-digest" => {
                    json!(self.authorization.manifest_digest)
                }
                _ => json!(self.authorization.attestation_digest),
            };
        }
        input["metadata"]["resourceVersion"] = json!("2");
        self.sealed.store(true, Ordering::SeqCst);
        Ok(input.clone())
    }
}

async fn observed_and_canonical(
    f: &Fixture,
    published: &enrichedlab_agent_sandbox::broker::input_publication::PublishedInputCopy,
) -> (ObservedInputSeal, SandboxAuthorization, CanonicalSandbox) {
    let retained = fs::read(f.request_path()).unwrap();
    let mut request: CopySealRequest = serde_json::from_slice(&retained).unwrap();
    let root = f.config.custody_root.parent().unwrap();
    request.source_root = root.join("source");
    request.destination_root = root.join("destination");
    request.custody_root = root.join("helper");
    fs::write(request.source_root.join("fixture"), PAYLOAD).unwrap();
    let copied = copy_and_seal(&request).unwrap();
    let mut receipt = serde_json::to_vec(&copied).unwrap();
    receipt.push(b'\n');
    let mut job = InputWorkloadRenderer::new(f.config.workload.clone())
        .unwrap()
        .render(&f.task, &f.binding, &f.authorization, &f.copy())
        .unwrap()
        .job;
    job["metadata"]["uid"] = json!(published.expectation().job_uid);
    job["metadata"]["generation"] = json!(1);
    job["spec"]["parallelism"] = json!(1);
    job["spec"]["completions"] = json!(1);
    job["status"] = json!({"succeeded":1,"active":0,"failed":0,"conditions":[{"type":"Complete","status":"True"}]});
    let pod = json!({"metadata":{"name":"copy-pod","namespace":f.binding.namespace,"uid":"copy-pod-uid",
      "ownerReferences":[{"apiVersion":"batch/v1","kind":"Job","uid":published.expectation().job_uid,"controller":true}]},
      "spec":job["spec"]["template"]["spec"],"status":{"phase":"Succeeded","containerStatuses":[{
        "name":"custodian","restartCount":0,"state":{"terminated":{"exitCode":0,"finishedAt":copied.sealed_at}}}]}});
    let observed = published
        .observe_seal(&Completed { job, pod, receipt })
        .await
        .unwrap();
    let record: Value =
        serde_json::from_str(include_str!("fixtures/admission/staged_admission.json")).unwrap();
    let mut auth: SandboxAuthorization =
        serde_json::from_value(record["authorization"].clone()).unwrap();
    auth.staging_digest = canonical_document_digest(observed.attestation()).unwrap();
    auth.attestation_digest = observed.attestation().attestation_digest.clone();
    let mut canonical = record["sandbox"].clone();
    canonical["metadata"]["annotations"]["sandbox.enrichedlab.org/input-staging-digest"] =
        json!(auth.staging_digest);
    canonical["metadata"]["annotations"]["sandbox.enrichedlab.org/input-attestation-digest"] =
        json!(auth.attestation_digest);
    (
        observed,
        auth.clone(),
        CanonicalSandbox::from_renderer(canonical, auth.input_digest.clone()).unwrap(),
    )
}

fn finalizer_config(f: &Fixture) -> InputFinalizationConfig {
    let root = f.config.custody_root.parent().unwrap();
    for child in ["final_journal", "admissions", "final_receipts"] {
        let path = root.join(child);
        fs::create_dir(&path).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o700)).unwrap();
    }
    InputFinalizationConfig {
        admission: AdmissionPublicationConfig {
            admission_root: root.join("admissions"),
            custody_root: root.join("final_receipts"),
            writer_uid: unsafe { libc::geteuid() },
            lease_seconds: 60,
        },
        journal_root: root.join("final_journal"),
        poll_interval: std::time::Duration::from_millis(1),
        max_wait: std::time::Duration::from_secs(1),
    }
}

#[tokio::test]
async fn finalized_copy_persists_original_receipt_before_cleanup_and_loads_admission() {
    let f = Fixture::new();
    let authority = Authority::new(&f);
    let creates = Creates::new(&f);
    let guard = InputSourceLeaseGuard {
        ledger: &authority,
        source: &authority,
        lease_seconds: 60,
    };
    let published = f
        .prepare()
        .unwrap()
        .publish(&guard, &creates, &CancellationToken::new())
        .await
        .unwrap();
    let (seal, authorization, canonical) = observed_and_canonical(&f, &published).await;
    let transport =
        FinalizationTransport::new(&f, &published, false, false, false, authorization.clone());
    let config = finalizer_config(&f);
    let receipt = InputFinalizer::new(config.clone())
        .unwrap()
        .finalize(
            &f.task,
            "worker-example",
            &f.binding,
            &published,
            &seal,
            &canonical,
            &authorization,
            &authority,
            &transport,
            &CancellationToken::new(),
        )
        .await
        .unwrap();
    assert!(transport.sealed.load(Ordering::SeqCst));
    assert!(
        !transport.job_present.load(Ordering::SeqCst)
            && !transport.pod_present.load(Ordering::SeqCst)
    );
    let name = enrichedlab_agent_sandbox::broker::dispatch::admission_record_name(&f.task).unwrap();
    let loaded = load(
        &config.admission.admission_root,
        &name,
        config.admission.writer_uid,
        &f.task,
        &f.binding,
    )
    .unwrap();
    assert_eq!(loaded.sealed_input, *seal.attestation());
    let scope = format!(
        "input-finalize-{}-{}-{}",
        f.task.id, f.task.attempt, f.task.lease_generation
    );
    assert_eq!(
        fs::read(
            config
                .admission
                .custody_root
                .join(format!("{scope}.original-seal.json"))
        )
        .unwrap(),
        seal.original_receipt()
    );
    assert_eq!(receipt.copy_job_uid, "created-5");
}

#[tokio::test]
async fn foreground_cleanup_waits_for_the_original_job_then_accepts_absence() {
    let f = Fixture::new();
    let authority = Authority::new(&f);
    let creates = Creates::new(&f);
    let guard = InputSourceLeaseGuard {
        ledger: &authority,
        source: &authority,
        lease_seconds: 60,
    };
    let published = f
        .prepare()
        .unwrap()
        .publish(&guard, &creates, &CancellationToken::new())
        .await
        .unwrap();
    let (seal, authorization, canonical) = observed_and_canonical(&f, &published).await;
    let transport =
        FinalizationTransport::new(&f, &published, false, false, false, authorization.clone());
    transport.artifacts.lock().unwrap()["status"]["phase"] = json!("Bound");
    transport
        .pending_original_job_observations
        .store(1, Ordering::SeqCst);
    let receipt = InputFinalizer::new(finalizer_config(&f))
        .unwrap()
        .finalize(
            &f.task,
            "worker-example",
            &f.binding,
            &published,
            &seal,
            &canonical,
            &authorization,
            &authority,
            &transport,
            &CancellationToken::new(),
        )
        .await
        .unwrap();
    assert_eq!(receipt.copy_job_uid, "created-5");
    assert!(transport.sealed.load(Ordering::SeqCst));
    assert_eq!(
        transport
            .pending_original_job_observations
            .load(Ordering::SeqCst),
        0
    );
}

#[tokio::test]
async fn delayed_cleanup_observation_respects_the_overall_deadline() {
    let f = Fixture::new();
    let authority = Authority::new(&f);
    let creates = Creates::new(&f);
    let guard = InputSourceLeaseGuard {
        ledger: &authority,
        source: &authority,
        lease_seconds: 60,
    };
    let published = f
        .prepare()
        .unwrap()
        .publish(&guard, &creates, &CancellationToken::new())
        .await
        .unwrap();
    let (seal, authorization, canonical) = observed_and_canonical(&f, &published).await;
    let transport =
        FinalizationTransport::new(&f, &published, false, false, false, authorization.clone());
    *transport.cleanup_job_delay.lock().unwrap() = std::time::Duration::from_millis(50);
    let mut config = finalizer_config(&f);
    config.max_wait = std::time::Duration::from_millis(5);
    let error = InputFinalizer::new(config.clone())
        .unwrap()
        .finalize(
            &f.task,
            "worker-example",
            &f.binding,
            &published,
            &seal,
            &canonical,
            &authorization,
            &authority,
            &transport,
            &CancellationToken::new(),
        )
        .await
        .unwrap_err();
    assert_eq!(error.to_string(), "input_finalization_cleanup_deadline");
    assert!(!transport.sealed.load(Ordering::SeqCst));
    assert!(
        !config
            .admission
            .admission_root
            .join(
                enrichedlab_agent_sandbox::broker::dispatch::admission_record_name(&f.task)
                    .unwrap()
            )
            .exists()
    );
}

#[tokio::test]
async fn replacement_cancellation_and_partial_seal_preserve_recovery_without_admission() {
    for (cancelled, replacement, malformed_job, seal_fail) in [
        (false, true, false, false),
        (false, false, true, false),
        (true, false, false, false),
        (false, false, false, true),
    ] {
        let f = Fixture::new();
        let authority = Authority::new(&f);
        let creates = Creates::new(&f);
        let guard = InputSourceLeaseGuard {
            ledger: &authority,
            source: &authority,
            lease_seconds: 60,
        };
        let published = f
            .prepare()
            .unwrap()
            .publish(&guard, &creates, &CancellationToken::new())
            .await
            .unwrap();
        let (seal, authorization, canonical) = observed_and_canonical(&f, &published).await;
        let transport = FinalizationTransport::new(
            &f,
            &published,
            replacement,
            malformed_job,
            seal_fail,
            authorization.clone(),
        );
        let config = finalizer_config(&f);
        let token = CancellationToken::new();
        if cancelled {
            token.cancel();
        }
        assert!(
            InputFinalizer::new(config.clone())
                .unwrap()
                .finalize(
                    &f.task,
                    "worker-example",
                    &f.binding,
                    &published,
                    &seal,
                    &canonical,
                    &authorization,
                    &authority,
                    &transport,
                    &token
                )
                .await
                .is_err()
        );
        assert!(!transport.sealed.load(Ordering::SeqCst));
        assert!(
            !config
                .admission
                .admission_root
                .join(
                    enrichedlab_agent_sandbox::broker::dispatch::admission_record_name(&f.task)
                        .unwrap()
                )
                .exists()
        );
    }
}

#[tokio::test]
async fn unbound_input_and_lost_output_stop_before_cleanup_or_sealing() {
    for (input_phase, output_phase) in [("Pending", "Pending"), ("Bound", "Lost")] {
        let f = Fixture::new();
        let authority = Authority::new(&f);
        let creates = Creates::new(&f);
        let guard = InputSourceLeaseGuard {
            ledger: &authority,
            source: &authority,
            lease_seconds: 60,
        };
        let published = f
            .prepare()
            .unwrap()
            .publish(&guard, &creates, &CancellationToken::new())
            .await
            .unwrap();
        let (seal, authorization, canonical) = observed_and_canonical(&f, &published).await;
        let transport =
            FinalizationTransport::new(&f, &published, false, false, false, authorization.clone());
        transport.input.lock().unwrap()["status"]["phase"] = json!(input_phase);
        transport.artifacts.lock().unwrap()["status"]["phase"] = json!(output_phase);
        let error = InputFinalizer::new(finalizer_config(&f))
            .unwrap()
            .finalize(
                &f.task,
                "worker-example",
                &f.binding,
                &published,
                &seal,
                &canonical,
                &authorization,
                &authority,
                &transport,
                &CancellationToken::new(),
            )
            .await
            .unwrap_err();
        assert_eq!(
            error.to_string(),
            "input_finalization_volume_identity_invalid"
        );
        assert!(transport.job_present.load(Ordering::SeqCst));
        assert!(!transport.sealed.load(Ordering::SeqCst));
    }
}
