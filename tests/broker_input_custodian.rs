#![cfg(unix)]
use anyhow::{Result, bail};
use async_trait::async_trait;
use enrichedlab_agent_sandbox::{
    broker::{
        admission,
        admission_publication::AdmissionPublicationConfig,
        input_completion::InputCopyObservationTransport,
        input_custodian::{
            CommandRenderer, CommandRendererConfig, CustodianRenderer, CustodyTaskSource,
            InputCustodian, InputCustodianConfig, InputSourceClass,
        },
        input_finalization::{InputFinalizationTransport, InputSealMutation},
        input_publication::{
            InputObject, InputObjectKind, InputObjectReceipt, InputPublicationConfig,
            InputPublicationTransport, InputSourceObservationTransport,
        },
        input_request::{InputPreparationConfig, InputPreparationRequest},
        input_workload::{
            AuthorizedInputCopy, CustodyPvcAccessMode, InputCopyAuthorization, InputWorkloadConfig,
            InputWorkloadRenderer,
        },
        model::ClaimedTask,
        sandbox::{BrokerLeaseFence, RetainedArtifact, SandboxBinding},
        store::AttemptInput,
    },
    custody::{CopySealRequest, InputManifest, SealAttestation, copy_and_seal},
};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    fs,
    os::unix::fs::{PermissionsExt, symlink},
    path::{Path, PathBuf},
    sync::{
        Mutex,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
};
use tempfile::TempDir;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

const IMAGE: &str = "registry.test/custodian@sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
const RENDERER: &str = r#"#!/usr/bin/ruby
require 'json'
require 'digest'
request = JSON.parse(STDIN.read)
abort unless request.fetch('schemaVersion') == 'sandbox.enrichedlab.org/custodian-render-request/v1'
abort unless request.fetch('task').keys.sort == %w[attempt groupKey leaseGeneration maxRunSeconds principalId profileKey taskId]
record = JSON.parse(File.read(ARGV.fetch(0)))
seal = request.fetch('sealedInput')
auth = record.fetch('authorization')
auth['stagingDigest'] = 'sha256:' + Digest::SHA256.hexdigest(JSON.generate(seal))
auth['attestationDigest'] = seal.fetch('attestationDigest')
sandbox = record.fetch('sandbox')
sandbox['metadata']['annotations']['sandbox.enrichedlab.org/input-staging-digest'] = auth['stagingDigest']
sandbox['metadata']['annotations']['sandbox.enrichedlab.org/input-attestation-digest'] = auth['attestationDigest']
STDOUT.write(JSON.generate({'schemaVersion'=>'sandbox.enrichedlab.org/custodian-render-result/v1', 'authorization'=>auth, 'sandbox'=>sandbox}))
"#;

fn private(path: &Path, bytes: &[u8]) {
    fs::write(path, bytes).unwrap();
    fs::set_permissions(path, fs::Permissions::from_mode(0o600)).unwrap();
}
fn write_json(path: &Path, value: &Value) {
    private(path, &serde_json::to_vec(value).unwrap());
}
struct Fixture {
    _temp: TempDir,
    root: PathBuf,
    config: InputCustodianConfig,
    task: ClaimedTask,
    binding: SandboxBinding,
    copy: CopySealRequest,
    notice: String,
}
impl Fixture {
    fn new() -> Self {
        let temp = TempDir::new().unwrap();
        let root = fs::canonicalize(temp.path()).unwrap();
        for part in [
            "requests",
            "plans",
            "journal",
            "custody",
            "final_journal",
            "admissions",
            "final_custody",
            "source",
            "destination",
            "helper",
        ] {
            fs::create_dir(root.join(part)).unwrap();
            fs::set_permissions(root.join(part), fs::Permissions::from_mode(0o700)).unwrap();
        }
        let record: Value =
            serde_json::from_str(include_str!("fixtures/admission/staged_admission.json")).unwrap();
        let seal: SealAttestation = serde_json::from_value(record["sealedInput"].clone()).unwrap();
        let manifest: InputManifest =
            serde_json::from_str(include_str!("fixtures/admission/input_manifest.json")).unwrap();
        let copy = CopySealRequest {
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
            max_bytes: Some(1024),
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
        let binding_json = json!({"principalId":task.principal_id,"principalHash":copy.principal_hash,
            "groupKey":task.group_key,"profileKey":task.profile_key,"profileId":"example-kata",
            "labRef":"lab://example/research","namespace":copy.namespace,"runtimeClassName":"kata-clh"});
        let binding = serde_json::from_value(binding_json.clone()).unwrap();
        let identity = json!({"taskId":task.id,"principalId":task.principal_id,"groupKey":task.group_key,
            "profileKey":task.profile_key,"attempt":task.attempt,"leaseGeneration":task.lease_generation,
            "maxRunSeconds":task.max_run_seconds});
        let notice = format!(
            "{}-{}-{}.json",
            task.id, task.attempt, task.lease_generation
        );
        write_json(
            &root.join("requests").join(&notice),
            &json!({"schemaVersion":"sandbox.enrichedlab.org/input-preparation-request/v1",
            "task":identity,"workerId":"worker-example","binding":binding_json}),
        );
        write_json(
            &root.join("plans").join(&notice),
            &json!({"schemaVersion":"sandbox.enrichedlab.org/input-preparation-plan/v2",
            "task":identity,"binding":binding_json,"source":{"namespace":copy.namespace,"pvcName":"input-source-run",
                "pvcUid":"approved-source-uid","runId":"source-run","tenantId":copy.tenant_id,"principalHash":copy.principal_hash},
            "copyRequestSecretName":"copy-request","copy":{
                "schemaVersion":copy.schema_version,"stagingId":copy.staging_id,"runId":copy.run_id,
                "tenantId":copy.tenant_id,"namespace":copy.namespace,"principalHash":copy.principal_hash,
                "inputRef":copy.input_ref,"inputDigest":copy.input_digest,"pvcName":copy.pvc_name,
                "manifestRef":copy.manifest_ref,"attestationRef":copy.attestation_ref,
                "sourceRoot":copy.source_root,"destinationRoot":copy.destination_root,"custodyRoot":copy.custody_root,
                "files":copy.files,"maxFiles":copy.max_files,"maxBytes":copy.max_bytes},
                "custodyRetentionUntil":"2099-01-01T00:00:00Z",
                "rendererPlan":{"purpose":"synthetic"}}),
        );
        private(&root.join("renderer.rb"), RENDERER.as_bytes());
        fs::set_permissions(root.join("renderer.rb"), fs::Permissions::from_mode(0o700)).unwrap();
        write_json(&root.join("renderer_fixture.json"), &record);
        let uid = unsafe { libc::geteuid() };
        let config = InputCustodianConfig {
            requests: InputPreparationConfig {
                root: root.join("requests"),
                writer_uid: uid,
            },
            plans: InputPreparationConfig {
                root: root.join("plans"),
                writer_uid: uid,
            },
            publication: InputPublicationConfig {
                journal_root: root.join("journal"),
                custody_root: root.join("custody"),
                writer_uid: uid,
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
            },
            admission: AdmissionPublicationConfig {
                admission_root: root.join("admissions"),
                custody_root: root.join("final_custody"),
                writer_uid: uid,
                lease_seconds: 60,
            },
            finalization_journal_root: root.join("final_journal"),
            max_wait_seconds: 1,
            poll_interval_millis: 1,
            input_sources: vec![],
        };
        Self {
            _temp: temp,
            root,
            config,
            task,
            binding,
            copy,
            notice,
        }
    }
    fn renderer(&self) -> CommandRenderer {
        CommandRenderer::new(CommandRendererConfig {
            program: self.root.join("renderer.rb"),
            sha256: hex::encode(Sha256::digest(RENDERER.as_bytes())),
            owner_uid: unsafe { libc::geteuid() },
            arguments: vec![
                self.root
                    .join("renderer_fixture.json")
                    .to_str()
                    .unwrap()
                    .into(),
            ],
            timeout_seconds: 2,
        })
        .unwrap()
    }
    fn source(&self) -> Value {
        json!({"metadata":{"name":"input-source-run","namespace":self.binding.namespace,"uid":"approved-source-uid",
            "labels":{"sandbox.enrichedlab.org/run-id":"source-run","sandbox.enrichedlab.org/tenant":self.copy.tenant_id,
            "sandbox.enrichedlab.org/principal-hash":self.copy.principal_hash,"sandbox.enrichedlab.org/contract-version":"v3",
            "sandbox.enrichedlab.org/volume-role":"immutable-input","sandbox.enrichedlab.org/seal-state":"sealed","sandbox.enrichedlab.org/materialization-mode":"checksummed-copy"},
            "annotations":{"sandbox.enrichedlab.org/input-materialization":"checksummed-copy"}},"status":{"phase":"Bound"}})
    }
}
struct State {
    creates: usize,
    job: Value,
    pod: Value,
    input: Value,
    artifacts: Value,
    seal: Vec<u8>,
    deleted: bool,
}
struct Cluster<'a> {
    fixture: &'a Fixture,
    state: Mutex<State>,
    current: AtomicBool,
    cancelled: AtomicBool,
    reads: AtomicUsize,
    failed: bool,
    stalled: bool,
    inputs: Mutex<Vec<Value>>,
    refuse_input: bool,
}
impl<'a> Cluster<'a> {
    fn new(fixture: &'a Fixture) -> Self {
        Self {
            fixture,
            state: Mutex::new(State {
                creates: 0,
                job: Value::Null,
                pod: Value::Null,
                input: Value::Null,
                artifacts: Value::Null,
                seal: Vec::new(),
                deleted: false,
            }),
            current: AtomicBool::new(true),
            cancelled: AtomicBool::new(false),
            reads: AtomicUsize::new(0),
            failed: false,
            stalled: false,
            inputs: Mutex::new(Vec::new()),
            refuse_input: false,
        }
    }
}
#[async_trait]
impl CustodyTaskSource for Cluster<'_> {
    async fn lookup_custody_task(
        &self,
        r: &InputPreparationRequest,
    ) -> Result<Option<ClaimedTask>> {
        assert!(r.task().matches(&self.fixture.task));
        Ok(self
            .current
            .load(Ordering::SeqCst)
            .then(|| self.fixture.task.clone()))
    }
    async fn record_sealed_input(
        &self,
        r: &InputPreparationRequest,
        i: &AttemptInput<'_>,
    ) -> Result<()> {
        let f = self.fixture;
        // Recorded for the notice's own lease, after the copy is sealed and before admission.
        assert!(r.task().matches(&f.task));
        assert!(!self.state.lock().unwrap().seal.is_empty());
        assert!(!f.config.admission.admission_root.join(&f.notice).exists());
        if self.refuse_input {
            bail!("broker:lease_fence_lost")
        }
        self.inputs.lock().unwrap().push(json!({"inputRef":i.input_ref,"class":i.class,
            "sealedSha256":i.sealed_sha256,"sizeBytes":i.size_bytes,"custodyTransaction":i.custody_transaction,
            "source":i.source}));
        Ok(())
    }
}
#[async_trait]
impl BrokerLeaseFence for Cluster<'_> {
    async fn mark_running(&self, _: Uuid, _: &str, _: i64) -> Result<bool> {
        bail!("unexpected_compute")
    }
    async fn cancel_requested(&self, _: Uuid, _: &str, _: i64) -> Result<bool> {
        Ok(self.cancelled.load(Ordering::SeqCst))
    }
    async fn renew_lease(&self, _: Uuid, _: &str, _: i64, _: i32) -> Result<bool> {
        Ok(self.current.load(Ordering::SeqCst))
    }
    async fn register_artifact(
        &self,
        _: Uuid,
        _: &str,
        _: i64,
        _: &RetainedArtifact,
    ) -> Result<()> {
        bail!("unexpected_output")
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
#[async_trait]
impl InputSourceObservationTransport for Cluster<'_> {
    async fn get_source_pvc(&self, _: &str, _: &str) -> Result<Value> {
        Ok(self.fixture.source())
    }
    async fn source_has_active_writer(&self, _: &str, _: &str) -> Result<bool> {
        Ok(false)
    }
}
#[async_trait]
impl InputPublicationTransport for Cluster<'_> {
    fn namespace(&self) -> &str {
        &self.fixture.binding.namespace
    }
    async fn create(&self, o: &InputObject, _: &CancellationToken) -> Result<InputObjectReceipt> {
        let f = self.fixture;
        let mut state = self.state.lock().unwrap();
        state.creates += 1;
        let uid = format!("created-{}", state.creates);
        if o.kind() == InputObjectKind::Job {
            let path = f
                .root
                .join("custody")
                .join(format!("input-{}-2-7.request.json", f.task.id));
            let mut copy: CopySealRequest =
                serde_json::from_slice(&fs::read(path).unwrap()).unwrap();
            copy.source_root = f.root.join("source");
            copy.destination_root = f.root.join("destination");
            copy.custody_root = f.root.join("helper");
            fs::write(
                copy.source_root.join("fixture"),
                include_bytes!("fixtures/admission/fixture"),
            )
            .unwrap();
            let seal = copy_and_seal(&copy)?;
            state.seal = serde_json::to_vec(&seal)?;
            state.seal.push(b'\n');
            let auth = InputCopyAuthorization {
                task_id: f.task.id,
                attempt: 2,
                lease_generation: 7,
                run_id: f.copy.run_id.clone(),
                tenant_id: f.copy.tenant_id.clone(),
                source_run_name: "source-run".into(),
                source_pvc_name: "input-source-run".into(),
                principal_hash: f.binding.principal_hash.clone(),
                profile_id: f.binding.profile_id.clone(),
                namespace: f.binding.namespace.clone(),
                custody_retention_until: "2099-01-01T00:00:00Z".into(),
                input_digest: f.copy.input_digest.clone(),
            };
            let request = AuthorizedInputCopy::new(
                "source-run".into(),
                "input-source-run".into(),
                "copy-request".into(),
                f.copy.clone(),
            )?;
            let workload = InputWorkloadRenderer::new(f.config.publication.workload.clone())?
                .render(&f.task, &f.binding, &auth, &request)?;
            state.input = workload.input_pvc;
            state.artifacts = workload.artifact_pvc;
            state.input["metadata"]["uid"] = json!("created-2");
            state.input["metadata"]["resourceVersion"] = json!("1");
            state.input["status"] = json!({"phase":"Bound"});
            state.artifacts["metadata"]["uid"] = json!("created-3");
            state.artifacts["metadata"]["resourceVersion"] = json!("1");
            state.artifacts["status"] = json!({"phase":"Bound"});
            state.job = workload.job;
            state.job["metadata"]["uid"] = json!(uid);
            state.job["metadata"]["generation"] = json!(1);
            state.job["spec"]["parallelism"] = json!(1);
            state.job["spec"]["completions"] = json!(1);
            state.job["status"] = json!({"succeeded":1,"active":0,"failed":0,"conditions":[{"type":"Complete","status":"True"}]});
            state.pod = json!({"metadata":{"name":"copy-pod","namespace":f.binding.namespace,"uid":"copy-pod-uid",
                "ownerReferences":[{"apiVersion":"batch/v1","kind":"Job","uid":uid,"controller":true}]},
                "spec":state.job["spec"]["template"]["spec"],"status":{"phase":"Succeeded","containerStatuses":[{
                "name":"custodian","restartCount":0,"state":{"terminated":{"exitCode":0,"finishedAt":seal.sealed_at}}}]}});
        }
        Ok(InputObjectReceipt {
            kind: o.kind(),
            namespace: o.namespace().into(),
            name: o.name().into(),
            uid,
            request_sha256: o.digest().into(),
        })
    }
}
#[async_trait]
impl InputCopyObservationTransport for Cluster<'_> {
    async fn get_job(&self, _: &str, _: &str) -> Result<Value> {
        if self.stalled {
            tokio::time::sleep(std::time::Duration::from_secs(5)).await;
        }
        let mut job = self.state.lock().unwrap().job.clone();
        if self.failed {
            job["status"] = json!({"conditions":[{"type":"Failed","status":"True"}]});
        } else if self.reads.fetch_add(1, Ordering::SeqCst) == 0 {
            job["status"] = json!({"active":1});
        }
        Ok(job)
    }
    async fn get_controlled_pod(&self, _: &str, _: &str) -> Result<Option<Value>> {
        Ok(Some(self.state.lock().unwrap().pod.clone()))
    }
    async fn helper_stdout(&self, _: &str, _: &str, _: &str) -> Result<Vec<u8>> {
        Ok(self.state.lock().unwrap().seal.clone())
    }
}
#[async_trait]
impl InputFinalizationTransport for Cluster<'_> {
    fn namespace(&self) -> &str {
        &self.fixture.binding.namespace
    }
    async fn get_pvc(&self, _: &str, name: &str) -> Result<Value> {
        let s = self.state.lock().unwrap();
        Ok(if name.starts_with("input-") {
            s.input.clone()
        } else {
            s.artifacts.clone()
        })
    }
    async fn delete_completed_job(
        &self,
        _: &str,
        _: &str,
        uid: &str,
        _: &CancellationToken,
    ) -> Result<()> {
        assert_eq!(uid, "created-5");
        let retained = self.fixture.root.join("custody").join(format!(
            "input-custodian-{}-2-7.original-seal.json",
            self.fixture.task.id
        ));
        assert_eq!(fs::read(retained).unwrap(), self.state.lock().unwrap().seal);
        self.state.lock().unwrap().deleted = true;
        Ok(())
    }
    async fn get_job_after_cleanup(&self, _: &str, _: &str) -> Result<Option<Value>> {
        assert!(self.state.lock().unwrap().deleted);
        Ok(None)
    }
    async fn get_pod_after_cleanup(&self, _: &str, _: &str, _: &str) -> Result<Option<Value>> {
        Ok(None)
    }
    async fn apply_input_seal(
        &self,
        m: &InputSealMutation,
        _: &CancellationToken,
    ) -> Result<Value> {
        let path = self.fixture.root.join("custody").join(format!(
            "input-custodian-{}-2-7.render-result.json",
            self.fixture.task.id
        ));
        let record: Value = serde_json::from_slice(&fs::read(path).unwrap()).unwrap();
        let mut s = self.state.lock().unwrap();
        assert_eq!(s.input["metadata"]["uid"], m.uid());
        s.input["metadata"]["labels"]["sandbox.enrichedlab.org/seal-state"] = json!("sealed");
        for (key, field) in [
            ("input-digest", "inputDigest"),
            ("input-staging-digest", "stagingDigest"),
            ("input-manifest-digest", "manifestDigest"),
            ("input-attestation-digest", "attestationDigest"),
        ] {
            s.input["metadata"]["annotations"][format!("sandbox.enrichedlab.org/{key}")] =
                record["authorization"][field].clone();
        }
        s.input["metadata"]["resourceVersion"] = json!("2");
        Ok(s.input.clone())
    }
}

#[tokio::test]
async fn private_notice_to_real_copy_command_renderer_and_loadable_admission() {
    let f = Fixture::new();
    let cluster = Cluster::new(&f);
    let renderer = f.renderer();
    let custodian = InputCustodian::new(f.config.clone()).unwrap();
    let cancel = CancellationToken::new();
    let receipt = custodian
        .run(&f.notice, &cluster, &cluster, &renderer, &cancel)
        .await
        .unwrap();
    assert_eq!(receipt.task_id, f.task.id);
    assert_eq!(cluster.state.lock().unwrap().creates, 5);
    assert!(cluster.reads.load(Ordering::SeqCst) >= 3);
    let admission = admission::load(
        &f.config.admission.admission_root,
        &f.notice,
        f.config.admission.writer_uid,
        &f.task,
        &f.binding,
    )
    .unwrap();
    assert_eq!(admission.authorization.input_digest, f.copy.input_digest);
    assert_eq!(
        fs::read(f.root.join("destination/payload/fixture")).unwrap(),
        include_bytes!("fixtures/admission/fixture")
    );
    // No class is configured for this source, so no input record is written.
    assert!(cluster.inputs.lock().unwrap().is_empty());
    // Exclusive publication scopes do not silently repeat a completed operation.
    assert!(
        custodian
            .run(&f.notice, &cluster, &cluster, &renderer, &cancel)
            .await
            .is_err()
    );
    assert_eq!(cluster.state.lock().unwrap().creates, 5);
}

#[tokio::test]
async fn stale_authority_private_plan_and_failed_copy_never_publish_admission() {
    for mode in [
        "stale",
        "cancelled",
        "plan_mismatch",
        "plan_symlink",
        "copy_failed",
        "copy_stalled",
    ] {
        let f = Fixture::new();
        let mut cluster = Cluster::new(&f);
        let renderer = f.renderer();
        let plan = f.root.join("plans").join(&f.notice);
        match mode {
            "stale" => cluster.current.store(false, Ordering::SeqCst),
            "cancelled" => cluster.cancelled.store(true, Ordering::SeqCst),
            "plan_mismatch" => {
                let mut p: Value = serde_json::from_slice(&fs::read(&plan).unwrap()).unwrap();
                p["task"]["attempt"] = json!(3);
                write_json(&plan, &p);
            }
            "plan_symlink" => {
                fs::rename(&plan, f.root.join("outside.json")).unwrap();
                symlink(f.root.join("outside.json"), &plan).unwrap();
            }
            "copy_stalled" => cluster.stalled = true,
            _ => cluster.failed = true,
        }
        let started = std::time::Instant::now();
        let error = InputCustodian::new(f.config.clone())
            .unwrap()
            .run(
                &f.notice,
                &cluster,
                &cluster,
                &renderer,
                &CancellationToken::new(),
            )
            .await
            .unwrap_err();
        assert!(
            !f.config.admission.admission_root.join(&f.notice).exists(),
            "{mode}"
        );
        if mode == "copy_stalled" {
            assert_eq!(error.to_string(), "input_custodian_copy_deadline");
            assert!(started.elapsed() < std::time::Duration::from_secs(3));
        }
        let s = cluster.state.lock().unwrap();
        assert!(!s.deleted, "{mode}");
        assert_eq!(
            s.creates,
            if mode.starts_with("copy_") { 5 } else { 0 },
            "{mode}: {error}"
        );
    }
}

#[tokio::test]
async fn command_entrypoint_change_is_refused_before_execution() {
    let f = Fixture::new();
    let renderer = f.renderer();
    fs::write(
        f.root.join("renderer.rb"),
        b"#!/usr/bin/ruby\nabort 'private response'\n",
    )
    .unwrap();
    let error = renderer
        .render(b"{}", &CancellationToken::new())
        .await
        .unwrap_err();
    assert_eq!(
        error.to_string(),
        "input_custodian_renderer_digest_mismatch"
    );
}

#[tokio::test]
async fn invalid_retention_and_legacy_plans_stop_before_publication() {
    for mode in ["missing", "malformed", "expired", "short", "legacy"] {
        let f = Fixture::new();
        let cluster = Cluster::new(&f);
        let path = f.root.join("plans").join(&f.notice);
        let mut plan: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        match mode {
            "missing" => {
                plan.as_object_mut()
                    .unwrap()
                    .remove("custodyRetentionUntil");
            }
            "malformed" => plan["custodyRetentionUntil"] = json!("not-a-timestamp"),
            "expired" => plan["custodyRetentionUntil"] = json!("2000-01-01T00:00:00Z"),
            "short" => {
                plan["custodyRetentionUntil"] = json!(
                    (time::OffsetDateTime::now_utc() + time::Duration::seconds(30))
                        .format(&time::format_description::well_known::Rfc3339)
                        .unwrap()
                )
            }
            "legacy" => {
                plan["schemaVersion"] = json!("sandbox.enrichedlab.org/input-preparation-plan/v1")
            }
            _ => unreachable!(),
        }
        write_json(&path, &plan);
        let result = InputCustodian::new(f.config.clone())
            .unwrap()
            .run(
                &f.notice,
                &cluster,
                &cluster,
                &f.renderer(),
                &CancellationToken::new(),
            )
            .await;
        assert!(result.is_err(), "{mode}");
        assert_eq!(cluster.state.lock().unwrap().creates, 0, "{mode}");
        assert_eq!(
            fs::read_dir(&f.config.publication.journal_root)
                .unwrap()
                .count(),
            0,
            "{mode}"
        );
    }
}

fn classified(f: &Fixture) -> InputCustodianConfig {
    let mut config = f.config.clone();
    config.input_sources = vec![InputSourceClass {
        pvc_uid: "approved-source-uid".into(),
        class: "public_reference".into(),
        source: serde_json::from_value(json!({"collection":"synthetic fixture"})).unwrap(),
    }];
    config
}

#[tokio::test]
async fn a_classified_source_is_recorded_for_its_attempt_after_the_seal_and_before_admission() {
    let f = Fixture::new();
    let cluster = Cluster::new(&f);
    InputCustodian::new(classified(&f))
        .unwrap()
        .run(
            &f.notice,
            &cluster,
            &cluster,
            &f.renderer(),
            &CancellationToken::new(),
        )
        .await
        .unwrap();
    let seal: SealAttestation =
        serde_json::from_slice(&cluster.state.lock().unwrap().seal).unwrap();
    let bytes: u64 = f.copy.files.iter().map(|x| x.size_bytes).sum();
    assert_eq!(
        *cluster.inputs.lock().unwrap(),
        [
            json!({"inputRef":f.copy.input_ref,"class":"public_reference",
            "sealedSha256":seal.manifest_digest.strip_prefix("sha256:").unwrap(),"sizeBytes":bytes,
            "custodyTransaction":f.copy.staging_id,
            "source":{"collection":"synthetic fixture","source_pvc_uid":"approved-source-uid","source_run_id":"source-run"}})
        ]
    );
    let journal = fs::read_to_string(
        f.config
            .publication
            .journal_root
            .join(format!("input-custodian-{}-2-7.jsonl", f.task.id)),
    )
    .unwrap();
    let phases: Vec<String> = journal
        .lines()
        .map(|l| {
            serde_json::from_str::<Value>(l).unwrap()["phase"]
                .as_str()
                .unwrap()
                .to_owned()
        })
        .collect();
    assert_eq!(
        phases,
        ["authorized", "copy_published", "input_recorded", "admitted"]
    );
}

#[tokio::test]
async fn a_refused_input_record_publishes_no_admission() {
    let f = Fixture::new();
    let mut cluster = Cluster::new(&f);
    cluster.refuse_input = true;
    let result = InputCustodian::new(classified(&f))
        .unwrap()
        .run(
            &f.notice,
            &cluster,
            &cluster,
            &f.renderer(),
            &CancellationToken::new(),
        )
        .await;
    assert!(result.is_err());
    assert!(!f.config.admission.admission_root.join(&f.notice).exists());
    assert!(cluster.inputs.lock().unwrap().is_empty());
    assert!(!cluster.state.lock().unwrap().deleted);
}

#[test]
fn input_source_classes_are_closed_and_never_a_database_extract() {
    let f = Fixture::new();
    let entry = |uid: &str, class: &str, source: Value| InputSourceClass {
        pvc_uid: uid.into(),
        class: class.into(),
        source: serde_json::from_value(source).unwrap(),
    };
    let object = json!({"source_version":"v1","source_sha256":"a".repeat(64)});
    for (name, entries, ok) in [
        (
            "object with version",
            vec![entry("uid-1", "object_data", object.clone())],
            true,
        ),
        (
            "paper and reference",
            vec![
                entry("uid-1", "paper", json!({})),
                entry("uid-2", "public_reference", json!({})),
            ],
            true,
        ),
        (
            "database extract",
            vec![entry(
                "uid-1",
                "lab_database",
                json!({"query_sha256":"q","snapshot":"s","wal_position":"w"}),
            )],
            false,
        ),
        (
            "unknown class",
            vec![entry("uid-1", "spreadsheet", json!({}))],
            false,
        ),
        (
            "object without version",
            vec![entry("uid-1", "object_data", json!({"source_sha256":"a"}))],
            false,
        ),
        (
            "duplicate source",
            vec![
                entry("uid-1", "paper", json!({})),
                entry("uid-1", "prior_output", json!({})),
            ],
            false,
        ),
        (
            "resolved key supplied",
            vec![entry("uid-1", "paper", json!({"source_pvc_uid":"other"}))],
            false,
        ),
        (
            "structured value",
            vec![entry("uid-1", "paper", json!({"doi":{"nested":true}}))],
            false,
        ),
        ("empty uid", vec![entry("", "paper", json!({}))], false),
    ] {
        let mut config = f.config.clone();
        config.input_sources = entries;
        assert_eq!(InputCustodian::new(config).is_ok(), ok, "{name}");
    }
}
