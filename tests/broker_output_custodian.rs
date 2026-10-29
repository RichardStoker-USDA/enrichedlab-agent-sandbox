#![cfg(unix)]
use anyhow::{Result, bail};
use async_trait::async_trait;
use enrichedlab_agent_sandbox::{
    broker::{
        export_publication::{ExportObject, ExportObjectReceipt, ExportPublicationTransport},
        export_transport::{ExportClient, ExportScope},
        model::ClaimedTask,
        output_custodian::{OutputCustodian, OutputCustodianConfig, TerminalCustodyTaskSource},
        output_workload::{
            OutputInventoryObject, OutputInventoryObjectKind, OutputInventoryObjectReceipt,
            OutputInventoryRenderer, OutputInventoryTransport, OutputPvc,
        },
        sandbox::{
            BrokerLeaseFence, RetainedArtifact, SandboxBinding, SandboxBindingCatalog,
            SandboxHandle, SandboxRuntime, SandboxTransport,
        },
        terminal_request::TerminalPreparationRequest,
    },
    output_inventory::{OutputInventoryRequest, inventory},
};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    fs,
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    sync::{
        Mutex,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};
use tempfile::TempDir;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

const RUN: &str = "run-20260909-0001";
const OUTPUT: &[u8] = b"synthetic original tool output\n";
fn uid() -> u32 {
    unsafe { libc::geteuid() }
}
fn private(path: &Path, bytes: &[u8]) {
    fs::write(path, bytes).unwrap();
    fs::set_permissions(path, fs::Permissions::from_mode(0o600)).unwrap();
}
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
fn handle() -> SandboxHandle {
    SandboxHandle {
        task_id: task().id,
        worker_id: "worker-example".into(),
        lease_generation: 7,
        namespace: binding().namespace,
        sandbox_name: RUN.into(),
        sandbox_uid: "sandbox-example-uid".into(),
        sandbox_generation: 3,
        lab_ref: binding().lab_ref,
    }
}
struct TerminalBoundary;
#[async_trait]
impl SandboxTransport for TerminalBoundary {
    async fn create_sandbox(&self, _: &str, _: &Value) -> Result<Value> {
        bail!("unexpected_create")
    }
    async fn get_sandbox(&self, _: &str, _: &str) -> Result<Value> {
        Ok(
            json!({"metadata":{"name":RUN,"namespace":binding().namespace,"uid":handle().sandbox_uid,"generation":3},
            "status":{"conditions":[{"type":"Finished","status":"True","observedGeneration":3,"reason":"PodSucceeded"}]}}),
        )
    }
    async fn get_pod(&self, _: &str, _: &str) -> Result<Option<Value>> {
        Ok(Some(
            json!({"metadata":{"name":RUN,"namespace":binding().namespace,"uid":"runner-example-uid",
            "ownerReferences":[{"apiVersion":"agents.x-k8s.io/v1beta1","kind":"Sandbox","uid":handle().sandbox_uid,"controller":true}]},
            "spec":{"runtimeClassName":"kata-clh","restartPolicy":"Never","containers":[{"name":"runner"}]},
            "status":{"phase":"Succeeded","containerStatuses":[{"name":"runner","state":{"terminated":{"exitCode":0,"finishedAt":"2026-09-09T00:00:01Z"}}}]}}),
        ))
    }
    async fn get_pvc(&self, _: &str, _: &str) -> Result<Value> {
        bail!("unexpected_pvc")
    }
    async fn delete_sandbox(&self, _: &str, _: &str, _: &str) -> Result<()> {
        bail!("unexpected_delete")
    }
}
fn runtime() -> SandboxRuntime<TerminalBoundary> {
    SandboxRuntime::new(
        TerminalBoundary,
        SandboxBindingCatalog::new(vec![binding()]).unwrap(),
        60,
    )
    .unwrap()
}

#[derive(Default)]
struct Ledger {
    lost: AtomicBool,
}
#[async_trait]
impl TerminalCustodyTaskSource for Ledger {
    async fn lookup_terminal_task(
        &self,
        _: &TerminalPreparationRequest,
    ) -> Result<Option<ClaimedTask>> {
        Ok((!self.lost.load(Ordering::SeqCst)).then(task))
    }
}
#[async_trait]
impl BrokerLeaseFence for Ledger {
    async fn mark_running(&self, _: Uuid, _: &str, _: i64) -> Result<bool> {
        bail!("unexpected_mark")
    }
    async fn cancel_requested(&self, _: Uuid, _: &str, _: i64) -> Result<bool> {
        Ok(false)
    }
    async fn renew_lease(&self, _: Uuid, _: &str, _: i64, _: i32) -> Result<bool> {
        Ok(!self.lost.load(Ordering::SeqCst))
    }
    async fn register_artifact(
        &self,
        _: Uuid,
        _: &str,
        _: i64,
        _: &RetainedArtifact,
    ) -> Result<()> {
        bail!("unexpected_registration")
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
    root: PathBuf,
    config: OutputCustodianConfig,
    plan: Value,
    name: String,
}
impl Fixture {
    fn new() -> Self {
        let temp = tempfile::Builder::new()
            .prefix("enrichedlab-output-custodian-")
            .tempdir_in("/tmp")
            .unwrap();
        let root = temp.path().canonicalize().unwrap();
        for part in [
            "requests",
            "plans",
            "custody",
            "journal",
            "inventory_journal",
            "export_journal",
            "clients",
            "artifacts",
        ] {
            fs::create_dir(root.join(part)).unwrap();
            fs::set_permissions(root.join(part), fs::Permissions::from_mode(0o700)).unwrap();
        }
        private(&root.join("artifacts/result.txt"), OUTPUT);
        let image = format!(
            "registry.example.invalid/custodian@sha256:{}",
            "a".repeat(64)
        );
        let config=serde_json::from_value(json!({
            "requests":{"root":root.join("requests"),"writerUid":uid()},"plans":{"root":root.join("plans"),"writerUid":uid()},
            "custodyRoot":root.join("custody"),"journalRoot":root.join("journal"),
            "inventory":{"runtimeClassName":"kata-clh","image":image,"poolSelector":{"example.org/pool":"sandbox"},
                "cpuRequestMillis":100,"cpuLimitMillis":500,"memoryRequestMib":128,"memoryLimitMib":256,
                "activeDeadlineSeconds":10,"maxRequestBytes":65536,"requestMemoryMib":4,"journalRoot":root.join("inventory_journal"),"writerUid":uid()},
            "publication":{"remote":{"dnsSuffix":"svc.cluster.test","port":8443,"credentialRoot":root.join("clients"),"writerUid":uid()},
                "journalRoot":root.join("export_journal"),"workload":{"runtimeClassName":"kata-clh","image":image,"brokerNamespace":"broker-example",
                "dnsSuffix":"svc.cluster.test","poolSelector":{"example.org/pool":"sandbox"},"cpuRequestMillis":100,"cpuLimitMillis":500,
                "memoryRequestMib":128,"memoryLimitMib":256,"activeDeadlineSeconds":120,"maxFiles":20,"maxBytes":10000,"maxSeconds":60,"identityMemoryMib":4}},
            "leaseSeconds":60,"maxWaitSeconds":10,"pollIntervalMillis":5,"exportSeconds":300
        })).unwrap();
        let identity = json!({"taskId":task().id,"principalId":task().principal_id,"groupKey":"research","profileKey":"reference",
            "attempt":2,"leaseGeneration":7,"maxRunSeconds":300});
        let binding = json!({"principalId":task().principal_id,"principalHash":binding().principal_hash,"groupKey":"research","profileKey":"reference",
            "profileId":"example-kata","labRef":"lab://example/research","namespace":"sandbox-example-001","runtimeClassName":"kata-clh"});
        let name = format!("{}-2-7.json", task().id);
        let notice = json!({"schemaVersion":"sandbox.enrichedlab.org/terminal-preparation-request/v1","task":identity,"binding":binding,
            "handle":{"workerId":"worker-example","sandboxName":RUN,"sandboxUid":handle().sandbox_uid,"sandboxGeneration":3},
            "completion":{"podName":RUN,"podUid":"runner-example-uid","runtimeClassName":"kata-clh","exitCode":0,"finishedAt":"2026-09-09T00:00:01Z"}});
        private(
            &root.join("requests").join(&name),
            &serde_json::to_vec(&notice).unwrap(),
        );
        let plan = json!({"schemaVersion":"sandbox.enrichedlab.org/output-preparation-plan/v1","task":identity,"binding":binding,
            "runId":RUN,"outputPvcUid":"output-example-uid","selection":{"schemaVersion":"sandbox.enrichedlab.org/output-inventory-request/v1",
                "mountedRoot":"/artifacts","files":[{"relativePath":"result.txt","visibility":"researcher","role":"result","label":"Tool output","mediaType":"text/plain"}],
                "maxFiles":20,"maxBytes":10000}});
        private(
            &root.join("plans").join(&name),
            &serde_json::to_vec(&plan).unwrap(),
        );
        Self {
            _temp: temp,
            root,
            config,
            plan,
            name,
        }
    }
    fn inventory(&self, mode: &str) -> InventoryBoundary {
        let request: OutputInventoryRequest =
            serde_json::from_value(self.plan["selection"].clone()).unwrap();
        let scope = ExportScope::new(task().id, 2, 7).unwrap();
        let workload = OutputInventoryRenderer::new(self.config.inventory.clone())
            .unwrap()
            .render(
                &task(),
                &binding(),
                &handle(),
                &request,
                &OutputPvc {
                    name: format!("artifacts-{RUN}"),
                    uid: "output-example-uid".into(),
                },
                &format!(
                    "{}-inventory",
                    scope.service_name(&binding().namespace).unwrap()
                ),
            )
            .unwrap();
        InventoryBoundary {
            job: workload.job,
            request,
            root: self.root.join("artifacts"),
            mode: mode.into(),
        }
    }
}
struct InventoryBoundary {
    job: Value,
    request: OutputInventoryRequest,
    root: PathBuf,
    mode: String,
}
#[async_trait]
impl OutputInventoryTransport for InventoryBoundary {
    fn namespace(&self) -> &str {
        "sandbox-example-001"
    }
    async fn create(
        &self,
        o: &OutputInventoryObject,
        _: &CancellationToken,
    ) -> Result<OutputInventoryObjectReceipt> {
        Ok(OutputInventoryObjectReceipt {
            kind: o.kind().clone(),
            namespace: o.namespace().into(),
            name: o.name().into(),
            uid: match o.kind() {
                OutputInventoryObjectKind::Job => "inventory-job-example-uid",
                OutputInventoryObjectKind::Secret => "request-example-uid",
                OutputInventoryObjectKind::NetworkPolicy => "policy-example-uid",
            }
            .into(),
        })
    }
    async fn get_pvc(&self, _: &str, name: &str) -> Result<Value> {
        Ok(
            json!({"metadata":{"namespace":self.namespace(),"name":name,"uid":"output-example-uid"}}),
        )
    }
    async fn has_active_writer(&self, _: &str, _: &str) -> Result<bool> {
        Ok(self.mode == "writer")
    }
    async fn get_job(&self, _: &str, _: &str) -> Result<Value> {
        if self.mode == "stall" {
            tokio::time::sleep(Duration::from_secs(60)).await;
        }
        let mut j = self.job.clone();
        j["metadata"]["uid"] = json!("inventory-job-example-uid");
        j["status"] = json!({"conditions":[{"type":"Complete","status":"True"}],"succeeded":1,"active":0,"failed":0});
        Ok(j)
    }
    async fn get_controlled_pod(&self, _: &str, name: &str) -> Result<Option<Value>> {
        let mut p = self.job["spec"]["template"].clone();
        p["metadata"]["name"] = json!(format!("{name}-pod"));
        p["metadata"]["namespace"] = json!(self.namespace());
        p["metadata"]["uid"] = json!("inventory-pod-example-uid");
        p["metadata"]["ownerReferences"] = json!([{"apiVersion":"batch/v1","kind":"Job","name":name,"uid":"inventory-job-example-uid","controller":true}]);
        p["status"] = json!({"phase":"Succeeded","containerStatuses":[{"name":"output-inventory","restartCount":0,"image":p["spec"]["containers"][0]["image"],
            "imageID":p["spec"]["containers"][0]["image"],"state":{"terminated":{"exitCode":0,"finishedAt":"2026-09-09T00:00:02Z"}}}]});
        if let Some(init) = p["spec"].get("initContainers").and_then(Value::as_array) {
            p["status"]["initContainerStatuses"]=json!(init.iter().map(|c|json!({"name":c["name"],"image":c["image"],"imageID":c["image"],"state":{"terminated":{"exitCode":0,"finishedAt":"2026-09-09T00:00:01Z"}}})).collect::<Vec<_>>());
        }
        Ok(Some(p))
    }
    async fn helper_stdout(&self, _: &str, _: &str, _: &str) -> Result<Vec<u8>> {
        // This is the real file-measurement code. Only the test mount coordinate
        // differs; the selected paths and their digest remain the approved ones.
        let mut request = self.request.clone();
        request.mounted_root = self.root.clone();
        let measured = inventory(&request, &CancellationToken::new())?;
        let mut original = serde_json::to_vec(&measured)?;
        original.push(b'\n');
        Ok(original)
    }
}
#[derive(Default)]
struct ExportBoundary {
    created: Mutex<Vec<String>>,
}
#[async_trait]
impl ExportPublicationTransport for ExportBoundary {
    fn namespace(&self) -> &str {
        "sandbox-example-001"
    }
    async fn create(&self, o: &ExportObject, _: &CancellationToken) -> Result<ExportObjectReceipt> {
        self.created.lock().unwrap().push(format!("{:?}", o.kind()));
        Ok(ExportObjectReceipt {
            kind: o.kind(),
            name: o.name().into(),
            uid: format!("export-{:?}-example-uid", o.kind()),
            request_sha256: o.digest().into(),
        })
    }
}

#[tokio::test]
async fn observed_terminal_to_real_inventory_retained_record_and_private_export_client() {
    let f = Fixture::new();
    let custodian = OutputCustodian::new(f.config.clone()).unwrap();
    let ledger = Ledger::default();
    let exports = ExportBoundary::default();
    let runtime = runtime();
    let receipt = custodian
        .run(
            &f.name,
            &ledger,
            &runtime,
            f.inventory("ok"),
            &exports,
            &CancellationToken::new(),
        )
        .await
        .unwrap();
    assert_eq!(receipt.file_count, 1);
    assert_eq!(receipt.size_bytes, OUTPUT.len() as u64);
    assert_eq!(
        *exports.created.lock().unwrap(),
        vec!["NetworkPolicy", "Secret", "Service", "Pod"]
    );
    let key = ExportScope::new(task().id, 2, 7).unwrap().credential_name();
    let terminal = fs::read(f.root.join("custody").join(format!("{key}.terminal.json"))).unwrap();
    assert_eq!(
        hex::encode(Sha256::digest(&terminal)),
        receipt.terminal_sha256
    );
    let record: Value = serde_json::from_slice(&terminal).unwrap();
    assert_eq!(
        record["files"][0]["sha256"],
        hex::encode(Sha256::digest(OUTPUT))
    );
    assert_eq!(record["podUid"], "runner-example-uid");
    assert!(
        ExportClient::for_task(&f.config.publication.remote, &task(), &binding().namespace).is_ok()
    );
    for suffix in [
        "plan.json",
        "inventory.json",
        "terminal.json",
        "receipt.json",
    ] {
        assert_eq!(
            fs::metadata(f.root.join("custody").join(format!("{key}.{suffix}")))
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
    }
    let error = custodian
        .run(
            &f.name,
            &ledger,
            &runtime,
            f.inventory("ok"),
            &exports,
            &CancellationToken::new(),
        )
        .await
        .unwrap_err();
    assert_eq!(
        error.to_string(),
        "publication_journal_exists_requires_recovery"
    );
    assert_eq!(exports.created.lock().unwrap().len(), 4);
}

#[tokio::test]
async fn wrong_authority_plan_writer_cancel_and_deadline_never_publish_credentials() {
    for mode in ["authority", "plan", "writer", "cancel", "stall"] {
        let mut f = Fixture::new();
        let ledger = Ledger::default();
        let cancel = CancellationToken::new();
        if mode == "authority" {
            ledger.lost.store(true, Ordering::SeqCst);
        }
        if mode == "cancel" {
            cancel.cancel();
        }
        if mode == "plan" {
            f.plan["runId"] = json!("run-unapproved");
            private(
                &f.root.join("plans").join(&f.name),
                &serde_json::to_vec(&f.plan).unwrap(),
            );
        }
        if mode == "stall" {
            f.config.max_wait_seconds = 1;
        }
        let custodian = OutputCustodian::new(f.config.clone()).unwrap();
        let exports = ExportBoundary::default();
        let started = std::time::Instant::now();
        let result = custodian
            .run(
                &f.name,
                &ledger,
                &runtime(),
                f.inventory(mode),
                &exports,
                &cancel,
            )
            .await;
        assert!(result.is_err(), "{mode}");
        if mode == "stall" {
            assert_eq!(result.unwrap_err().to_string(), "output_custodian_deadline");
            assert!(started.elapsed() < Duration::from_secs(3));
        }
        assert!(exports.created.lock().unwrap().is_empty(), "{mode}");
        assert_eq!(fs::read_dir(f.root.join("clients")).unwrap().count(), 0);
    }
}
