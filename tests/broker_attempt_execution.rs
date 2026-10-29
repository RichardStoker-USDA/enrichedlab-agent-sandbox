//! The worker records the image an attempt runs and the digest of the exact Sandbox object it
//! submits, under its lease, before the Sandbox exists, and an attempt whose record is refused never starts.
use anyhow::{Result, bail};
use async_trait::async_trait;
use enrichedlab_agent_sandbox::{
    broker::{
        model::ClaimedTask,
        sandbox::{
            BrokerLeaseFence, CanonicalSandbox, RetainedArtifact, SandboxAuthorization,
            SandboxBinding, SandboxBindingCatalog, SandboxRuntime, SandboxTransport, build_sandbox,
        },
    },
    custody::canonical_document_digest,
};
use serde_json::{Value, json};
use std::sync::Mutex;
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};
use uuid::Uuid;
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

#[derive(Clone)]
struct Volumes {
    input: Value,
    output: Value,
    creates: Arc<AtomicUsize>,
    created: Arc<Mutex<Option<Value>>>,
}
#[async_trait]
impl SandboxTransport for Volumes {
    async fn create_sandbox(&self, namespace: &str, object: &Value) -> Result<Value> {
        self.creates.fetch_add(1, Ordering::SeqCst);
        *self.created.lock().unwrap() = Some(object.clone());
        Ok(
            json!({"metadata":{"namespace":namespace,"name":object["metadata"]["name"],"uid":"sandbox-uid","generation":1}}),
        )
    }
    async fn get_pvc(&self, _: &str, name: &str) -> Result<Value> {
        Ok(if name.starts_with("input-") {
            self.input.clone()
        } else {
            self.output.clone()
        })
    }
    async fn get_sandbox(&self, _: &str, _: &str) -> Result<Value> {
        bail!("unexpected_sandbox_read")
    }
    async fn get_pod(&self, _: &str, _: &str) -> Result<Option<Value>> {
        bail!("unexpected_pod_read")
    }
    async fn delete_sandbox(&self, _: &str, _: &str, _: &str) -> Result<()> {
        bail!("unexpected_delete")
    }
}
/// Records what the worker's lease saw, in order, and can refuse the execution record.
struct Lease {
    calls: Mutex<Vec<String>>,
    refuse_record: bool,
}
#[async_trait]
impl BrokerLeaseFence for Lease {
    async fn mark_running(&self, _: Uuid, _: &str, g: i64) -> Result<bool> {
        self.calls.lock().unwrap().push(format!("mark_running {g}"));
        Ok(true)
    }
    async fn record_execution(
        &self,
        _: Uuid,
        w: &str,
        g: i64,
        image_digest: &str,
        sandbox_digest: &str,
    ) -> Result<()> {
        self.calls
            .lock()
            .unwrap()
            .push(format!("record {w} {g} {image_digest} {sandbox_digest}"));
        if self.refuse_record {
            bail!("broker:lease_fence_lost")
        }
        Ok(())
    }
    async fn cancel_requested(&self, _: Uuid, _: &str, _: i64) -> Result<bool> {
        bail!("unexpected_cancel_read")
    }
    async fn renew_lease(&self, _: Uuid, _: &str, _: i64, _: i32) -> Result<bool> {
        bail!("unexpected_renewal")
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
fn fixture() -> (Value, SandboxAuthorization, Volumes) {
    let r: Value =
        serde_json::from_str(include_str!("fixtures/admission/staged_admission.json")).unwrap();
    let a: SandboxAuthorization = serde_json::from_value(r["authorization"].clone()).unwrap();
    let mut s = r["sandbox"].clone();
    s["metadata"]["annotations"]["sandbox.enrichedlab.org/input-pvc-uid"] =
        json!("original-input-uid");
    s["metadata"]["annotations"]["sandbox.enrichedlab.org/artifact-pvc-uid"] =
        json!("original-output-uid");
    let pvc = |role: &str, prefix: &str, uid: &str| {
        json!({"metadata":{"name":format!("{prefix}-{}",a.run_id),"namespace":binding().namespace,"uid":uid,
        "labels":{"sandbox.enrichedlab.org/volume-role":role,"sandbox.enrichedlab.org/run-id":a.run_id,
            "sandbox.enrichedlab.org/seal-state":"sealed","sandbox.enrichedlab.org/materialization-mode":"checksummed-copy"},
        "annotations":s["metadata"]["annotations"]}})
    };
    let v = Volumes {
        input: pvc("immutable-input", "input", "original-input-uid"),
        output: pvc("durable-artifacts", "artifacts", "original-output-uid"),
        creates: Arc::new(AtomicUsize::new(0)),
        created: Arc::new(Mutex::new(None)),
    };
    (s, a, v)
}

#[tokio::test]
async fn an_attempt_records_its_image_and_sandbox_before_the_sandbox_exists() {
    let (s, a, v) = fixture();
    let creates = v.creates.clone();
    let created = v.created.clone();
    let runtime =
        SandboxRuntime::new(v, SandboxBindingCatalog::new(vec![binding()]).unwrap(), 60).unwrap();
    let c = CanonicalSandbox::from_renderer(s, a.input_digest.clone()).unwrap();
    let lease = Lease {
        calls: Mutex::new(Vec::new()),
        refuse_record: false,
    };
    runtime
        .start(&lease, &task(), "worker-example", &c, &a)
        .await
        .unwrap();
    // The parameter record is the object the worker submitted, not the input manifest, which
    // belongs to the custodian's input record.
    let submitted = created.lock().unwrap().clone().unwrap();
    let digest = canonical_document_digest(&submitted).unwrap();
    assert_eq!(
        digest,
        canonical_document_digest(&build_sandbox(&task(), &binding(), &c, &a).unwrap()).unwrap()
    );
    assert_ne!(digest, a.manifest_digest);
    assert_eq!(
        *lease.calls.lock().unwrap(),
        [
            "mark_running 7".to_owned(),
            format!("record worker-example 7 {} {digest}", a.executable_digest),
        ]
    );
    assert_eq!(creates.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn an_attempt_whose_execution_is_not_recorded_does_not_run() {
    let (s, a, v) = fixture();
    let creates = v.creates.clone();
    let runtime =
        SandboxRuntime::new(v, SandboxBindingCatalog::new(vec![binding()]).unwrap(), 60).unwrap();
    let c = CanonicalSandbox::from_renderer(s, a.input_digest.clone()).unwrap();
    let lease = Lease {
        calls: Mutex::new(Vec::new()),
        refuse_record: true,
    };
    assert!(
        runtime
            .start(&lease, &task(), "worker-example", &c, &a)
            .await
            .is_err()
    );
    assert_eq!(lease.calls.lock().unwrap().len(), 2);
    assert_eq!(creates.load(Ordering::SeqCst), 0);
}
