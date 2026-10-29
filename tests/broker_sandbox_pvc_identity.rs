use anyhow::{Result, bail};
use async_trait::async_trait;
use enrichedlab_agent_sandbox::broker::{
    model::ClaimedTask,
    sandbox::{
        BrokerLeaseFence, CanonicalSandbox, RetainedArtifact, SandboxAuthorization, SandboxBinding,
        SandboxBindingCatalog, SandboxRuntime, SandboxTransport,
    },
};
use serde_json::{Value, json};
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
}
#[async_trait]
impl SandboxTransport for Volumes {
    async fn create_sandbox(&self, namespace: &str, object: &Value) -> Result<Value> {
        self.creates.fetch_add(1, Ordering::SeqCst);
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
struct Lease;
#[async_trait]
impl BrokerLeaseFence for Lease {
    async fn mark_running(&self, _: Uuid, _: &str, _: i64) -> Result<bool> {
        Ok(true)
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
    };
    (s, a, v)
}

#[tokio::test]
async fn same_name_volume_replacement_wrong_namespace_or_seal_stops_before_compute() {
    for variant in 0..6 {
        let (s, a, mut v) = fixture();
        match variant {
            0 => v.input["metadata"]["uid"] = json!("replacement-input"),
            1 => v.output["metadata"]["uid"] = json!("replacement-output"),
            2 => v.input["metadata"]["namespace"] = json!("foreign-lab"),
            3 => {
                v.input["metadata"]["annotations"]["sandbox.enrichedlab.org/input-digest"] =
                    json!("different-input")
            }
            4 => v.output["metadata"]["deletionTimestamp"] = json!("2026-09-09T00:00:01Z"),
            _ => v.input["metadata"]
                .as_object_mut()
                .unwrap()
                .remove("uid")
                .map(|_| ())
                .unwrap(),
        }
        let creates = v.creates.clone();
        let runtime =
            SandboxRuntime::new(v, SandboxBindingCatalog::new(vec![binding()]).unwrap(), 60)
                .unwrap();
        let c = CanonicalSandbox::from_renderer(s, a.input_digest.clone()).unwrap();
        assert!(
            runtime
                .start(&Lease, &task(), "worker-example", &c, &a)
                .await
                .is_err()
        );
        assert_eq!(creates.load(Ordering::SeqCst), 0);
    }
}

#[tokio::test]
async fn accepts_both_exact_volume_identities_and_refuses_a_partial_uid_binding() {
    for complete in [true, false] {
        let (mut s, a, v) = fixture();
        if !complete {
            s["metadata"]["annotations"]
                .as_object_mut()
                .unwrap()
                .remove("sandbox.enrichedlab.org/artifact-pvc-uid");
        }
        let creates = v.creates.clone();
        let runtime =
            SandboxRuntime::new(v, SandboxBindingCatalog::new(vec![binding()]).unwrap(), 60)
                .unwrap();
        let c = CanonicalSandbox::from_renderer(s, a.input_digest.clone()).unwrap();
        assert_eq!(
            runtime
                .start(&Lease, &task(), "worker-example", &c, &a)
                .await
                .is_ok(),
            complete
        );
        assert_eq!(creates.load(Ordering::SeqCst), usize::from(complete));
    }
}
