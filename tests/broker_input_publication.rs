#![cfg(unix)]

use anyhow::{Result, bail};
use async_trait::async_trait;
use enrichedlab_agent_sandbox::{
    broker::{
        input_completion::InputCopyObservationTransport,
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
        sandbox::{BrokerLeaseFence, RetainedArtifact, SandboxBinding},
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

#[tokio::test]
async fn original_request_precedes_ordered_creates_and_drives_copy_completion() {
    let f = Fixture::new();
    let authority = Authority::new(&f);
    let transport = Creates::new(&f);
    let guard = InputSourceLeaseGuard {
        ledger: &authority,
        source: &authority,
        lease_seconds: 60,
    };
    let p = f.prepare().unwrap();
    assert!(!format!("{p:?}").contains("fixture"));
    let published = p
        .publish(&guard, &transport, &CancellationToken::new())
        .await
        .unwrap();
    assert_eq!(
        *transport.created.lock().unwrap(),
        vec![
            InputObjectKind::NetworkPolicy,
            InputObjectKind::PersistentVolumeClaim,
            InputObjectKind::PersistentVolumeClaim,
            InputObjectKind::Secret,
            InputObjectKind::Job
        ]
    );
    assert_eq!(published.volumes().input_uid, "created-2");
    assert_eq!(published.volumes().artifact_uid, "created-3");
    assert_eq!(published.expectation().job_uid, "created-5");
    let retained = fs::read(f.request_path()).unwrap();
    assert_eq!(
        published.receipt().request_sha256,
        hex::encode(sha2::Sha256::digest(&retained))
    );
    assert_eq!(
        fs::metadata(f.request_path()).unwrap().permissions().mode() & 0o777,
        0o600
    );
    let manifest: InputManifest = serde_json::from_slice(
        &fs::read(
            f.config
                .custody_root
                .join(format!("{}.manifest.json", f.scope())),
        )
        .unwrap(),
    )
    .unwrap();
    assert_eq!(
        canonical_document_digest(&manifest).unwrap(),
        f.request.input_digest
    );
    let mut request: CopySealRequest = serde_json::from_slice(&retained).unwrap();
    let root = f.config.custody_root.parent().unwrap();
    request.source_root = root.join("source");
    request.destination_root = root.join("destination");
    request.custody_root = root.join("helper");
    fs::write(request.source_root.join("fixture"), PAYLOAD).unwrap();
    let seal = copy_and_seal(&request).unwrap();
    assert_eq!(
        fs::read(request.destination_root.join("payload/fixture")).unwrap(),
        PAYLOAD
    );
    let mut receipt = serde_json::to_vec(&seal).unwrap();
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
            "name":"custodian","restartCount":0,"state":{"terminated":{"exitCode":0,"finishedAt":seal.sealed_at}}}]}});
    let accepted = published
        .observe_seal(&Completed {
            job,
            pod,
            receipt: receipt.clone(),
        })
        .await
        .unwrap();
    assert_eq!(accepted.original_receipt(), receipt);
    let events = f.events();
    assert_eq!(events.last().unwrap()["phase"], "published");
    for (i, event) in events.iter().enumerate() {
        assert_eq!(event["sequence"], i + 1);
    }
    assert!(!serde_json::to_string(&events).unwrap().contains("fixture"));
    assert!(
        p.publish(&guard, &transport, &CancellationToken::new())
            .await
            .unwrap_err()
            .to_string()
            .contains("requires_recovery")
    );
    assert_eq!(transport.created.lock().unwrap().len(), 5);
}

#[tokio::test]
async fn authority_changes_and_partial_creates_retain_recovery_without_compute() {
    for mode in [
        "lease", "writer", "replaced", "unsealed", "cancel", "create", "uid",
    ] {
        let f = Fixture::new();
        let mut authority = Authority::new(&f);
        let mut transport = Creates::new(&f);
        match mode {
            "lease" => authority.reject_at = 3,
            "writer" => authority.writer.store(true, Ordering::SeqCst),
            "replaced" => authority.pvc["metadata"]["uid"] = json!("replacement-uid"),
            "unsealed" => {
                authority.pvc["metadata"]["labels"]["sandbox.enrichedlab.org/materialization-mode"] =
                    json!("unapproved")
            }
            "cancel" => authority.cancel = true,
            "create" => transport.fail_at = 3,
            "uid" => transport.bad_uid = true,
            _ => unreachable!(),
        }
        let guard = InputSourceLeaseGuard {
            ledger: &authority,
            source: &authority,
            lease_seconds: 60,
        };
        let error = f
            .prepare()
            .unwrap()
            .publish(&guard, &transport, &CancellationToken::new())
            .await
            .unwrap_err();
        assert!(!error.to_string().contains("synthetic_private_failure_body"));
        assert!(
            !transport
                .created
                .lock()
                .unwrap()
                .contains(&InputObjectKind::Job)
        );
        if matches!(mode, "lease" | "create" | "uid") {
            assert_eq!(f.events().last().unwrap()["phase"], "stopped");
            assert!(f.request_path().exists());
        } else {
            assert_eq!(fs::read_dir(&f.config.journal_root).unwrap().count(), 0);
        }
    }
}

#[tokio::test]
async fn invalid_manifest_or_private_staging_cannot_create_a_workload() {
    let mut f = Fixture::new();
    f.request.files[0].sha256 = format!("sha256:{}", "a".repeat(64));
    assert!(
        f.prepare()
            .unwrap_err()
            .to_string()
            .contains("manifest_digest_mismatch")
    );
    let f = Fixture::new();
    let authority = Authority::new(&f);
    let transport = Creates::new(&f);
    let guard = InputSourceLeaseGuard {
        ledger: &authority,
        source: &authority,
        lease_seconds: 60,
    };
    fs::write(f.request_path(), b"existing-private-request").unwrap();
    fs::set_permissions(f.request_path(), fs::Permissions::from_mode(0o600)).unwrap();
    assert!(
        f.prepare()
            .unwrap()
            .publish(&guard, &transport, &CancellationToken::new())
            .await
            .unwrap_err()
            .to_string()
            .contains("staging_failed")
    );
    assert!(transport.created.lock().unwrap().is_empty());
    assert_eq!(
        fs::read(f.request_path()).unwrap(),
        b"existing-private-request"
    );
    assert_eq!(f.events().last().unwrap()["phase"], "stopped");
}

use sha2::Digest;
