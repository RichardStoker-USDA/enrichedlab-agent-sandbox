use anyhow::{Result, bail};
use async_trait::async_trait;
use enrichedlab_agent_sandbox::broker::{
    model::ClaimedTask,
    sandbox::{
        ArtifactRetainer, BrokerLeaseFence, RetainedArtifact, SandboxBinding,
        SandboxBindingCatalog, SandboxHandle, SandboxRuntime, SandboxTransport,
    },
};
use serde_json::{Value, json};
use std::{
    path::Path,
    sync::{Arc, Mutex},
};
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

fn task() -> ClaimedTask {
    ClaimedTask {
        id: Uuid::nil(),
        principal_id: Uuid::nil(),
        group_key: "research".into(),
        profile_key: "reference".into(),
        objective: "Untrusted task text is not exit evidence".into(),
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
        sandbox_name: "run-example".into(),
        sandbox_uid: Uuid::nil().to_string(),
        sandbox_generation: 3,
        lab_ref: binding().lab_ref,
    }
}
#[derive(Clone)]
struct Boundary {
    sandbox: Arc<Mutex<Value>>,
    pod: Arc<Mutex<Option<Value>>>,
    events: Arc<Mutex<Vec<Value>>>,
}
fn fixture(success: bool) -> (Boundary, SandboxRuntime<Boundary>) {
    let h = handle();
    let sandbox = json!({"metadata":{"name":h.sandbox_name,"namespace":h.namespace,"uid":h.sandbox_uid,"generation":3},
        "status":{"conditions":[{"type":"Finished","status":"True","observedGeneration":3,"reason":if success {"PodSucceeded"} else {"PodFailed"}}]}});
    let pod = json!({"metadata":{"name":h.sandbox_name,"namespace":h.namespace,"uid":"example-pod-uid", "ownerReferences":[{"apiVersion":"agents.x-k8s.io/v1beta1","kind":"Sandbox","uid":h.sandbox_uid,"controller":true}]},
        "spec":{"runtimeClassName":"kata-clh","restartPolicy":"Never","containers":[{"name":"runner"}]},
        "status":{"phase":if success {"Succeeded"} else {"Failed"},"containerStatuses":[{"name":"runner","state":{"terminated":{"exitCode":if success {0} else {137},"finishedAt":"2026-09-09T00:00:01Z"}}}]}});
    let boundary = Boundary {
        sandbox: Arc::new(Mutex::new(sandbox)),
        pod: Arc::new(Mutex::new(Some(pod))),
        events: Arc::default(),
    };
    let runtime = SandboxRuntime::new(
        boundary.clone(),
        SandboxBindingCatalog::new(vec![binding()]).unwrap(),
        60,
    )
    .unwrap();
    (boundary, runtime)
}
#[async_trait]
impl SandboxTransport for Boundary {
    async fn create_sandbox(&self, _: &str, _: &Value) -> Result<Value> {
        bail!("unexpected_create")
    }
    async fn get_sandbox(&self, namespace: &str, name: &str) -> Result<Value> {
        assert_eq!(
            (namespace, name),
            (handle().namespace.as_str(), handle().sandbox_name.as_str())
        );
        Ok(self.sandbox.lock().unwrap().clone())
    }
    async fn get_pod(&self, namespace: &str, name: &str) -> Result<Option<Value>> {
        assert_eq!(
            (namespace, name),
            (handle().namespace.as_str(), handle().sandbox_name.as_str())
        );
        Ok(self.pod.lock().unwrap().clone())
    }
    async fn get_pvc(&self, _: &str, _: &str) -> Result<Value> {
        bail!("unexpected_pvc")
    }
    async fn delete_sandbox(&self, _: &str, _: &str, uid: &str) -> Result<()> {
        assert_eq!(uid, handle().sandbox_uid);
        self.events.lock().unwrap().push(json!("deleted"));
        Ok(())
    }
}
#[async_trait]
impl BrokerLeaseFence for Boundary {
    async fn mark_running(&self, _: Uuid, _: &str, _: i64) -> Result<bool> {
        bail!("unexpected_start")
    }
    async fn cancel_requested(&self, _: Uuid, _: &str, _: i64) -> Result<bool> {
        Ok(false)
    }
    async fn renew_lease(&self, _: Uuid, _: &str, _: i64, _: i32) -> Result<bool> {
        Ok(true)
    }
    async fn register_artifact(
        &self,
        _: Uuid,
        _: &str,
        _: i64,
        _: &RetainedArtifact,
    ) -> Result<()> {
        Ok(())
    }
    async fn record_digest(
        &self,
        _: Uuid,
        worker: &str,
        g: i64,
        root: &Path,
        a: &RetainedArtifact,
    ) {
        self.events.lock().unwrap().push(
            json!({"digest": a.relative_path, "root": root.to_str(), "worker": worker, "generation": g}),
        );
    }
    async fn finish_task(
        &self,
        _: Uuid,
        _: &str,
        _: i64,
        passed: bool,
        class: &str,
        code: &str,
    ) -> Result<bool> {
        self.events
            .lock()
            .unwrap()
            .push(json!({"passed":passed,"class":class,"code":code}));
        Ok(true)
    }
}
struct Retainer;
#[async_trait]
impl ArtifactRetainer for Retainer {
    async fn retain(
        &self,
        _: &SandboxHandle,
        _: &CancellationToken,
    ) -> Result<Vec<RetainedArtifact>> {
        Ok(vec![RetainedArtifact {
            visibility: "operator".into(),
            role: "evidence".into(),
            label: "mock receipt".into(),
            relative_path: "test/receipt.json".into(),
            media_type: "application/json".into(),
            size_bytes: 0,
            sha256: "a".repeat(64),
        }])
    }
}

/// A retainer that wrote the bytes locally and returns a result package beside other files.
struct LocalRetainer;
#[async_trait]
impl ArtifactRetainer for LocalRetainer {
    async fn retain(
        &self,
        _: &SandboxHandle,
        _: &CancellationToken,
    ) -> Result<Vec<RetainedArtifact>> {
        let artifact = |visibility: &str, role: &str, path: &str| RetainedArtifact {
            visibility: visibility.into(),
            role: role.into(),
            label: "retained".into(),
            relative_path: path.into(),
            media_type: "application/octet-stream".into(),
            size_bytes: 1,
            sha256: "a".repeat(64),
        };
        Ok(vec![
            artifact(
                "researcher",
                "result",
                "tasks/x/attempts/2-7/exports/payload",
            ),
            artifact(
                "researcher",
                "evidence",
                "tasks/x/attempts/2-7/exports/payload.sha256",
            ),
            artifact(
                "operator",
                "result",
                "tasks/x/attempts/2-7/exports/operator",
            ),
        ])
    }
    fn retained_root(&self) -> Option<&Path> {
        Some(Path::new("/durable"))
    }
}

#[tokio::test]
async fn only_a_researcher_result_package_is_digested_and_before_cleanup() {
    let (boundary, runtime) = fixture(true);
    let observed = runtime
        .observe_pod(&task(), &handle())
        .await
        .unwrap()
        .unwrap();
    let completion = runtime
        .observe_terminal(&task(), &handle(), &observed)
        .await
        .unwrap()
        .unwrap();
    runtime
        .finish_observed(&boundary, &LocalRetainer, &task(), &handle(), &completion)
        .await
        .unwrap();
    let events = boundary.events.lock().unwrap().clone();
    assert_eq!(
        events[0],
        json!({"digest":"tasks/x/attempts/2-7/exports/payload","root":"/durable","worker":"worker-example","generation":7})
    );
    assert_eq!(events[1], json!("deleted"));
    assert_eq!(events.len(), 3);
}

#[tokio::test]
async fn fast_success_and_failed_exit_use_authoritative_completion_without_readiness() {
    for success in [true, false] {
        let (boundary, runtime) = fixture(success);
        let observed = runtime
            .observe_pod(&task(), &handle())
            .await
            .unwrap()
            .unwrap();
        assert!(runtime.observe_ready(&task(), &handle()).await.is_err());
        let completion = runtime
            .observe_terminal(&task(), &handle(), &observed)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(completion.succeeded(), success);
        assert_eq!(completion.exit_code(), if success { 0 } else { 137 });
        assert_eq!(completion.observation(), &observed);
        assert_eq!(completion.finished_at().unix_timestamp(), 1788912001);
        runtime
            .finish_observed(&boundary, &Retainer, &task(), &handle(), &completion)
            .await
            .unwrap();
        assert_eq!(
            *boundary.events.lock().unwrap(),
            vec![
                json!("deleted"),
                json!({"passed":success,"class":if success {"sandbox_completed"} else {"sandbox_failed"},"code":if success {""} else {"sandbox_run_failed"}})
            ]
        );
    }
}

#[tokio::test]
async fn pending_or_stale_controller_evidence_does_not_complete_a_task() {
    let (boundary, runtime) = fixture(true);
    let observed = runtime
        .observe_pod(&task(), &handle())
        .await
        .unwrap()
        .unwrap();
    for phase in ["Pending", "Running", "Unknown"] {
        boundary.pod.lock().unwrap().as_mut().unwrap()["status"]["phase"] = json!(phase);
        assert!(
            runtime
                .observe_terminal(&task(), &handle(), &observed)
                .await
                .unwrap()
                .is_none()
        );
    }
    boundary.pod.lock().unwrap().as_mut().unwrap()["status"]["phase"] = json!("Succeeded");
    boundary.sandbox.lock().unwrap()["status"]["conditions"][0]["observedGeneration"] = json!(2);
    assert!(
        runtime
            .observe_terminal(&task(), &handle(), &observed)
            .await
            .unwrap()
            .is_none()
    );
    boundary.sandbox.lock().unwrap()["status"]["conditions"][0]["observedGeneration"] = json!(3);
    boundary.sandbox.lock().unwrap()["status"]["conditions"][0]["reason"] = json!("PodFailed");
    assert!(
        runtime
            .observe_terminal(&task(), &handle(), &observed)
            .await
            .unwrap()
            .is_none()
    );
    assert!(boundary.events.lock().unwrap().is_empty());
}

#[tokio::test]
async fn readiness_and_first_pod_capture_preserve_the_existing_running_path() {
    let (boundary, runtime) = fixture(true);
    let mut pod = boundary.pod.lock().unwrap().take().unwrap();
    assert!(
        runtime
            .observe_pod(&task(), &handle())
            .await
            .unwrap()
            .is_none()
    );
    pod["status"]["phase"] = json!("Running");
    pod["status"]["containerStatuses"][0]["state"] =
        json!({"running":{"startedAt":"2026-09-09T00:00:00Z"}});
    *boundary.pod.lock().unwrap() = Some(pod);
    boundary.sandbox.lock().unwrap()["status"]["conditions"] =
        json!([{"type":"Ready","status":"True","observedGeneration":3}]);
    let observed = runtime
        .observe_pod(&task(), &handle())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        runtime.observe_ready(&task(), &handle()).await.unwrap(),
        observed
    );
    assert!(
        runtime
            .observe_terminal(&task(), &handle(), &observed)
            .await
            .unwrap()
            .is_none()
    );
}

#[tokio::test]
async fn refuses_replaced_pods_cross_namespace_identity_and_changed_generations() {
    for field in [
        "pod_uid",
        "namespace",
        "owner_uid",
        "owner_api",
        "runtime",
        "sandbox_generation",
        "missing_pod",
    ] {
        let (boundary, runtime) = fixture(true);
        let observed = runtime
            .observe_pod(&task(), &handle())
            .await
            .unwrap()
            .unwrap();
        match field {
            "sandbox_generation" => {
                boundary.sandbox.lock().unwrap()["metadata"]["generation"] = json!(4)
            }
            "missing_pod" => *boundary.pod.lock().unwrap() = None,
            _ => {
                let mut lock = boundary.pod.lock().unwrap();
                let pod = lock.as_mut().unwrap();
                match field {
                    "pod_uid" => pod["metadata"]["uid"] = json!("replacement"),
                    "namespace" => pod["metadata"]["namespace"] = json!("another-lab"),
                    "owner_uid" => {
                        pod["metadata"]["ownerReferences"][0]["uid"] = json!("another-sandbox")
                    }
                    "owner_api" => {
                        pod["metadata"]["ownerReferences"][0]["apiVersion"] =
                            json!("different.example/v1")
                    }
                    _ => pod["spec"]["runtimeClassName"] = json!("different-runtime"),
                }
            }
        }
        assert!(
            runtime
                .observe_terminal(&task(), &handle(), &observed)
                .await
                .is_err(),
            "{field}"
        );
        assert!(boundary.events.lock().unwrap().is_empty());
    }
}

#[tokio::test]
async fn missing_or_conflicting_exit_evidence_cannot_authorize_cleanup() {
    for field in [
        "statuses_missing",
        "runner_running",
        "extra_container",
        "restart_policy",
        "exit_mismatch",
        "timestamp_missing",
        "timestamp_invalid",
    ] {
        let (boundary, runtime) = fixture(true);
        let observed = runtime
            .observe_pod(&task(), &handle())
            .await
            .unwrap()
            .unwrap();
        {
            let mut lock = boundary.pod.lock().unwrap();
            let pod = lock.as_mut().unwrap();
            match field {
                "statuses_missing" => pod["status"]["containerStatuses"] = json!([]),
                "runner_running" => {
                    pod["status"]["containerStatuses"][0]["state"] = json!({"running":{}})
                }
                "extra_container" => pod["spec"]["ephemeralContainers"] = json!([{"name":"debug"}]),
                "restart_policy" => pod["spec"]["restartPolicy"] = json!("Always"),
                "exit_mismatch" => {
                    pod["status"]["containerStatuses"][0]["state"]["terminated"]["exitCode"] =
                        json!(1)
                }
                "timestamp_missing" => {
                    pod["status"]["containerStatuses"][0]["state"]["terminated"]["finishedAt"] =
                        Value::Null
                }
                _ => {
                    pod["status"]["containerStatuses"][0]["state"]["terminated"]["finishedAt"] =
                        json!("0001-01-01T00:00:00Z")
                }
            }
        }
        assert!(
            runtime
                .observe_terminal(&task(), &handle(), &observed)
                .await
                .is_err(),
            "{field}"
        );
        assert!(boundary.events.lock().unwrap().is_empty());
    }
}

#[tokio::test]
async fn completion_is_not_reusable_for_another_worker_or_lease() {
    let (boundary, runtime) = fixture(true);
    let observed = runtime
        .observe_pod(&task(), &handle())
        .await
        .unwrap()
        .unwrap();
    let completion = runtime
        .observe_terminal(&task(), &handle(), &observed)
        .await
        .unwrap()
        .unwrap();
    let mut other = handle();
    other.worker_id = "other-worker".into();
    assert!(
        runtime
            .finish_observed(&boundary, &Retainer, &task(), &other, &completion)
            .await
            .is_err()
    );
    other = handle();
    other.lease_generation += 1;
    assert!(
        runtime
            .finish_observed(&boundary, &Retainer, &task(), &other, &completion)
            .await
            .is_err()
    );
    assert!(boundary.events.lock().unwrap().is_empty());
}
