#![cfg(unix)]

use anyhow::{Result, bail};
use async_trait::async_trait;
use enrichedlab_agent_sandbox::{
    broker::{
        admission::load,
        admission_publication::{
            AdmissionPublicationConfig, CreatedVolumeIdentities, PreparedAdmissionPublication,
        },
        dispatch::admission_record_name,
        input_completion::{
            InputCopyContainerExpectation, InputCopyJobExpectation, InputCopyObservationTransport,
            InputCopyVolumeExpectation, InputCopyVolumeSource, ObservedInputSeal,
            observe_input_seal,
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
    sync::atomic::{AtomicUsize, Ordering},
};
use tempfile::TempDir;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

const IMAGE: &str = "registry.test/custodian@sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
const PREFIX: &str = "sandbox.enrichedlab.org/";
const PAYLOAD: &[u8] = include_bytes!("fixtures/admission/fixture");

fn task() -> ClaimedTask {
    ClaimedTask {
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
    }
}
fn binding() -> SandboxBinding {
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
struct CopyObservation {
    job: Value,
    pod: Value,
    receipt: Vec<u8>,
}
#[async_trait]
impl InputCopyObservationTransport for CopyObservation {
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
struct Lease {
    renewals: AtomicUsize,
    reject_at: usize,
}
impl Lease {
    fn new(reject_at: usize) -> Self {
        Self {
            renewals: AtomicUsize::new(0),
            reject_at,
        }
    }
}
#[async_trait]
impl BrokerLeaseFence for Lease {
    async fn mark_running(&self, _: Uuid, _: &str, _: i64) -> Result<bool> {
        bail!("admission_must_not_start_compute")
    }
    async fn cancel_requested(&self, id: Uuid, worker: &str, generation: i64) -> Result<bool> {
        assert_eq!((id, worker, generation), (Uuid::nil(), "worker-example", 7));
        Ok(false)
    }
    async fn renew_lease(
        &self,
        id: Uuid,
        worker: &str,
        generation: i64,
        seconds: i32,
    ) -> Result<bool> {
        assert_eq!(
            (id, worker, generation, seconds),
            (Uuid::nil(), "worker-example", 7, 60)
        );
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

struct Fixture {
    _temp: TempDir,
    config: AdmissionPublicationConfig,
    authorization: SandboxAuthorization,
    canonical: CanonicalSandbox,
    seal: ObservedInputSeal,
    volumes: CreatedVolumeIdentities,
    input: Value,
    artifacts: Value,
    copied_file: PathBuf,
}
impl Fixture {
    async fn new() -> Self {
        let temp = TempDir::new().unwrap();
        let root = fs::canonicalize(temp.path()).unwrap();
        for child in [
            "source",
            "destination",
            "helper_custody",
            "admissions",
            "receipts",
        ] {
            fs::create_dir(root.join(child)).unwrap();
            fs::set_permissions(root.join(child), fs::Permissions::from_mode(0o700)).unwrap();
        }
        fs::write(root.join("source/fixture"), PAYLOAD).unwrap();
        let record: Value =
            serde_json::from_str(include_str!("fixtures/admission/staged_admission.json")).unwrap();
        let original: SealAttestation =
            serde_json::from_value(record["sealedInput"].clone()).unwrap();
        let manifest: InputManifest =
            serde_json::from_str(include_str!("fixtures/admission/input_manifest.json")).unwrap();
        let mut request = CopySealRequest {
            schema_version: original.schema_version,
            staging_id: original.staging_id,
            run_id: original.run_id,
            tenant_id: original.tenant_id,
            namespace: original.namespace,
            principal_hash: original.principal_hash,
            input_ref: original.input_ref,
            input_digest: original.input_digest,
            pvc_name: original.pvc_name,
            manifest_ref: original.manifest_ref,
            attestation_ref: original.attestation_ref,
            source_root: root.join("source"),
            destination_root: root.join("destination"),
            custody_root: root.join("helper_custody"),
            files: manifest.files,
            max_files: Some(1),
            max_bytes: Some(PAYLOAD.len() as u64),
            admitted_files: vec![],
        };
        let copied = copy_and_seal(&request).unwrap();
        let mut receipt = serde_json::to_vec(&copied).unwrap();
        receipt.push(b'\n');
        request.source_root = "/source".into();
        request.destination_root = "/destination".into();
        request.custody_root = "/custody".into();
        let copy = AuthorizedInputCopy::new(
            "source-run".into(),
            "input-source-run".into(),
            "request-example".into(),
            request.clone(),
        )
        .unwrap();
        let preseal = InputCopyAuthorization {
            task_id: task().id,
            attempt: task().attempt,
            lease_generation: task().lease_generation,
            run_id: request.run_id.clone(),
            tenant_id: request.tenant_id.clone(),
            source_run_name: "source-run".into(),
            source_pvc_name: "input-source-run".into(),
            principal_hash: binding().principal_hash,
            profile_id: binding().profile_id,
            namespace: binding().namespace,
            custody_retention_until: "2099-01-01T00:00:00Z".into(),
            input_digest: request.input_digest.clone(),
        };
        let renderer = InputWorkloadRenderer::new(InputWorkloadConfig {
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
        })
        .unwrap();
        let workload = renderer
            .render(&task(), &binding(), &preseal, &copy)
            .unwrap();
        let mut job = workload.job;
        job["metadata"]["uid"] = json!("copy-job-uid");
        job["metadata"]["generation"] = json!(1);
        // Standard API defaults and terminal status, without invented condition fields.
        job["spec"]["completions"] = json!(1);
        job["spec"]["parallelism"] = json!(1);
        job["status"] = json!({"succeeded":1,"active":0,"failed":0,"conditions":[{"type":"Complete","status":"True"}]});
        let pod = json!({"metadata":{"name":"copy-pod","namespace":request.namespace,"uid":"copy-pod-uid",
        "ownerReferences":[{"apiVersion":"batch/v1","kind":"Job","uid":"copy-job-uid","controller":true}]},
        "spec":job["spec"]["template"]["spec"],"status":{"phase":"Succeeded","containerStatuses":[{
            "name":"custodian","restartCount":0,"state":{"terminated":{"exitCode":0,"finishedAt":copied.sealed_at}}
        }]}});
        let volume = |role: &str, read_only: bool, sub_path: Option<&str>, source| {
            InputCopyVolumeExpectation {
                role: role.into(),
                name: role.into(),
                mount_path: format!("/{role}"),
                sub_path: sub_path.map(str::to_owned),
                read_only,
                source,
            }
        };
        let expected = InputCopyJobExpectation {
            namespace: request.namespace.clone(),
            job_name: workload.job_name,
            job_uid: "copy-job-uid".into(),
            run_id: request.run_id.clone(),
            principal_hash: request.principal_hash.clone(),
            runtime_class_name: "kata-clh".into(),
            helper: InputCopyContainerExpectation {
                name: "custodian".into(),
                image: IMAGE.into(),
                command: vec!["/usr/local/bin/agent-sandbox-custodian".into()],
                args: vec!["--request".into(), "/request/request.json".into()],
            },
            volumes: vec![
                volume(
                    "source",
                    true,
                    Some("payload"),
                    InputCopyVolumeSource::PersistentVolumeClaim {
                        claim_name: "input-source-run".into(),
                    },
                ),
                volume(
                    "destination",
                    false,
                    None,
                    InputCopyVolumeSource::PersistentVolumeClaim {
                        claim_name: request.pvc_name.clone(),
                    },
                ),
                volume(
                    "custody",
                    false,
                    None,
                    InputCopyVolumeSource::EmptyDirMemory,
                ),
                volume(
                    "request",
                    true,
                    None,
                    InputCopyVolumeSource::ProjectedSecret {
                        secret_name: "request-example".into(),
                    },
                ),
            ],
        };
        let seal = observe_input_seal(&CopyObservation { job, pod, receipt }, &expected, &request)
            .await
            .unwrap();
        let mut authorization: SandboxAuthorization =
            serde_json::from_value(record["authorization"].clone()).unwrap();
        authorization.staging_digest = canonical_document_digest(seal.attestation()).unwrap();
        authorization.attestation_digest = seal.attestation().attestation_digest.clone();
        let mut canonical = record["sandbox"].clone();
        canonical["metadata"]["annotations"][format!("{PREFIX}input-staging-digest")] =
            json!(authorization.staging_digest);
        canonical["metadata"]["annotations"][format!("{PREFIX}input-attestation-digest")] =
            json!(authorization.attestation_digest);
        let mut input = workload.input_pvc;
        let mut artifacts = workload.artifact_pvc;
        input["metadata"]["uid"] = json!("input-volume-uid");
        artifacts["metadata"]["uid"] = json!("artifact-volume-uid");
        // Trusted seal promotion changes state and observed hashes, never the creator's role.
        input["metadata"]["labels"][format!("{PREFIX}seal-state")] = json!("sealed");
        for suffix in [
            "input-digest",
            "input-staging-digest",
            "input-manifest-digest",
            "input-attestation-digest",
        ] {
            input["metadata"]["annotations"][format!("{PREFIX}{suffix}")] =
                canonical["metadata"]["annotations"][format!("{PREFIX}{suffix}")].clone();
        }
        Self {
            config: AdmissionPublicationConfig {
                admission_root: root.join("admissions"),
                custody_root: root.join("receipts"),
                writer_uid: unsafe { libc::geteuid() },
                lease_seconds: 60,
            },
            canonical: CanonicalSandbox::from_renderer(
                canonical,
                authorization.input_digest.clone(),
            )
            .unwrap(),
            authorization,
            seal,
            volumes: CreatedVolumeIdentities {
                input_uid: "input-volume-uid".into(),
                artifact_uid: "artifact-volume-uid".into(),
            },
            input,
            artifacts,
            copied_file: root.join("destination/payload/fixture"),
            _temp: temp,
        }
    }
    fn prepare(&self) -> Result<PreparedAdmissionPublication> {
        PreparedAdmissionPublication::prepare(
            self.config.clone(),
            &task(),
            &binding(),
            &self.authorization,
            &self.canonical,
            &self.seal,
            &self.volumes,
            &self.input,
            &self.artifacts,
        )
    }
    fn admission_path(&self) -> PathBuf {
        self.config
            .admission_root
            .join(admission_record_name(&task()).unwrap())
    }
}

#[tokio::test]
async fn original_copy_and_observed_seal_publish_a_readable_exact_volume_admission() {
    let f = Fixture::new().await;
    let prepared = f.prepare().unwrap();
    let lease = Lease::new(usize::MAX);
    let receipt = prepared
        .publish(&lease, "worker-example", &CancellationToken::new())
        .await
        .unwrap();
    assert_eq!(fs::read(&f.copied_file).unwrap(), PAYLOAD);
    let name = admission_record_name(&task()).unwrap();
    assert_eq!(
        fs::read(f.config.custody_root.join(format!("{name}.seal.json"))).unwrap(),
        f.seal.original_receipt()
    );
    let admitted = load(
        &f.config.admission_root,
        &name,
        f.config.writer_uid,
        &task(),
        &binding(),
    )
    .unwrap();
    assert_eq!(admitted.sealed_input, *f.seal.attestation());
    assert_eq!(
        admitted.canonical.manifest()["metadata"]["annotations"][format!("{PREFIX}input-pvc-uid")],
        "input-volume-uid"
    );
    assert_eq!(
        admitted.canonical.manifest()["metadata"]["annotations"]
            [format!("{PREFIX}artifact-pvc-uid")],
        "artifact-volume-uid"
    );
    let original = fs::read(f.admission_path()).unwrap();
    assert_eq!(
        prepared
            .publish(&lease, "worker-example", &CancellationToken::new())
            .await
            .unwrap()
            .admission_sha256,
        receipt.admission_sha256
    );
    assert_eq!(fs::read(f.admission_path()).unwrap(), original);
    assert_eq!(
        fs::metadata(f.admission_path())
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o600
    );
    let metadata = serde_json::to_string(&receipt).unwrap();
    assert!(
        !metadata.contains("input://")
            && !metadata.contains("fixture")
            && !metadata.contains("source/")
    );
    for root in [&f.config.admission_root, &f.config.custody_root] {
        assert!(fs::read_dir(root).unwrap().all(|entry| {
            !entry
                .unwrap()
                .file_name()
                .to_string_lossy()
                .ends_with(".partial")
        }));
    }
}

#[tokio::test]
async fn cancellation_and_lost_lease_do_not_expose_admission_and_conflicts_never_replace_it() {
    let f = Fixture::new().await;
    let prepared = f.prepare().unwrap();
    let cancellation = CancellationToken::new();
    cancellation.cancel();
    assert!(
        prepared
            .publish(&Lease::new(usize::MAX), "worker-example", &cancellation)
            .await
            .is_err()
    );
    assert!(!f.admission_path().exists());
    assert_eq!(fs::read_dir(&f.config.custody_root).unwrap().count(), 0);
    assert!(
        prepared
            .publish(&Lease::new(2), "worker-example", &CancellationToken::new())
            .await
            .is_err()
    );
    assert!(!f.admission_path().exists());
    assert_eq!(fs::read_dir(&f.config.custody_root).unwrap().count(), 2);
    fs::write(f.admission_path(), b"different admission").unwrap();
    fs::set_permissions(f.admission_path(), fs::Permissions::from_mode(0o600)).unwrap();
    let error = prepared
        .publish(
            &Lease::new(usize::MAX),
            "worker-example",
            &CancellationToken::new(),
        )
        .await
        .unwrap_err();
    assert_eq!(error.to_string(), "admission_existing_record_invalid");
    assert_eq!(
        fs::read(f.admission_path()).unwrap(),
        b"different admission"
    );
}

#[tokio::test]
async fn preparation_rejects_a_replaced_volume_or_changed_seal() {
    let mut f = Fixture::new().await;
    f.input["metadata"]["uid"] = json!("replacement");
    assert!(f.prepare().is_err());
    f.input["metadata"]["uid"] = json!("input-volume-uid");
    f.input["metadata"]["annotations"][format!("{PREFIX}input-attestation-digest")] =
        json!("changed");
    assert!(f.prepare().is_err());
    assert!(!f.admission_path().exists());
}
