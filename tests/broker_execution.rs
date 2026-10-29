#![cfg(unix)]

use anyhow::{Result, bail};
use async_trait::async_trait;
use enrichedlab_agent_sandbox::broker::{
    admission,
    artifact_retainer::{ExportFile, ExportLimits, ExportRoots, FilesystemArtifactRetainer},
    execution::{
        DirectExecutionControl, DirectExecutionDriver, DirectExecutionInterruption,
        DirectExecutionOutcome, DirectExecutionTiming, TerminalArtifactRetainerFactory,
    },
    model::ClaimedTask,
    runtime_events::{RuntimeEvent, RuntimeEventRecorder},
    sandbox::{
        ArtifactRetainer, BrokerLeaseFence, RetainedArtifact, SandboxBinding,
        SandboxBindingCatalog, SandboxHandle, SandboxRuntime, SandboxTransport,
    },
};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    collections::VecDeque,
    fs,
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};
use tempfile::TempDir;
use tokio::{sync::Notify, time::timeout};
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

const ADMISSION: &str = include_str!("fixtures/admission/staged_admission.json");
const BYTES: &[u8] = b"synthetic execution artifact\n";

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

fn mode(path: &Path, bits: u32) {
    fs::set_permissions(path, fs::Permissions::from_mode(bits)).unwrap();
}

fn admitted(temp: &TempDir) -> admission::AdmittedSandbox {
    let root = temp.path().canonicalize().unwrap().join("admissions");
    fs::create_dir(&root).unwrap();
    mode(&root, 0o700);
    let name = "00000000-0000-0000-0000-000000000000-2-7.json";
    let path = root.join(name);
    fs::write(&path, ADMISSION).unwrap();
    mode(&path, 0o600);
    admission::load(&root, name, unsafe { libc::geteuid() }, &task(), &binding()).unwrap()
}

fn pod(run: &str, phase: &str, uid: &str, exit: Option<i32>) -> Value {
    let state = match exit {
        Some(exit_code) => {
            json!({"terminated":{"exitCode":exit_code,"finishedAt":"2026-09-09T00:00:01Z"}})
        }
        None => json!({"running":{"startedAt":"2026-09-09T00:00:00Z"}}),
    };
    json!({
        "metadata":{"name":run,"namespace":"sandbox-example-001","uid":uid,
          "ownerReferences":[{"apiVersion":"agents.x-k8s.io/v1beta1","kind":"Sandbox","uid":"sandbox-uid","controller":true}]},
        "spec":{"runtimeClassName":"kata-clh","restartPolicy":"Never","containers":[{"name":"runner"}]},
        "status":{"phase":phase,"containerStatuses":[{"name":"runner","state":state}]}
    })
}

#[derive(Clone)]
struct Boundary {
    run: String,
    sandbox: Arc<Mutex<Option<Value>>>,
    pods: Arc<Mutex<VecDeque<Value>>>,
    events: Arc<Mutex<Vec<&'static str>>>,
    renewals: Arc<AtomicUsize>,
    fail_renewal: usize,
    durable: Option<PathBuf>,
    terminal_succeeded: bool,
    stalled_start: Option<Arc<Notify>>,
}

impl Boundary {
    fn new(
        run: String,
        pods: Vec<Value>,
        fail_renewal: usize,
        durable: Option<PathBuf>,
        terminal_succeeded: bool,
    ) -> Self {
        Self {
            run,
            sandbox: Arc::new(Mutex::new(None)),
            pods: Arc::new(Mutex::new(pods.into())),
            events: Arc::default(),
            renewals: Arc::default(),
            fail_renewal,
            durable,
            terminal_succeeded,
            stalled_start: None,
        }
    }

    fn with_stalled_start(mut self, stalled_start: Arc<Notify>) -> Self {
        self.stalled_start = Some(stalled_start);
        self
    }

    fn event(&self, event: &'static str) {
        self.events.lock().unwrap().push(event);
    }

    fn events(&self) -> Vec<&'static str> {
        self.events.lock().unwrap().clone()
    }
}

#[async_trait]
impl SandboxTransport for Boundary {
    async fn create_sandbox(&self, namespace: &str, object: &Value) -> Result<Value> {
        assert_eq!(namespace, "sandbox-example-001");
        let mut created = object.clone();
        created["metadata"]["uid"] = json!("sandbox-uid");
        created["metadata"]["generation"] = json!(1);
        created["status"] = json!({"conditions":[{"type":"Finished","status":"True","observedGeneration":1,"reason":if self.terminal_succeeded {"PodSucceeded"} else {"PodFailed"}}]});
        *self.sandbox.lock().unwrap() = Some(created.clone());
        self.event("created");
        Ok(created)
    }

    async fn get_sandbox(&self, _: &str, _: &str) -> Result<Value> {
        self.sandbox
            .lock()
            .unwrap()
            .clone()
            .ok_or_else(|| anyhow::anyhow!("sandbox_missing"))
    }

    async fn get_pod(&self, _: &str, _: &str) -> Result<Option<Value>> {
        let mut pods = self.pods.lock().unwrap();
        Ok(pods.pop_front().or_else(|| pods.back().cloned()))
    }

    async fn get_pvc(&self, _: &str, name: &str) -> Result<Value> {
        if let Some(stalled_start) = &self.stalled_start {
            stalled_start.notified().await;
            bail!("stalled_start_released");
        }
        let (role, sealed) = if name.starts_with("input-") {
            ("immutable-input", true)
        } else {
            ("durable-artifacts", false)
        };
        Ok(json!({"metadata":{"name":name,"labels":{
            "sandbox.enrichedlab.org/volume-role":role,
            "sandbox.enrichedlab.org/run-id":self.run,
            "sandbox.enrichedlab.org/seal-state":if sealed {"sealed"} else {"active"},
            "sandbox.enrichedlab.org/materialization-mode":"checksummed-copy"
        }}}))
    }

    async fn delete_sandbox(&self, _: &str, _: &str, uid: &str) -> Result<()> {
        assert_eq!(uid, "sandbox-uid");
        if let Some(path) = &self.durable {
            assert_eq!(
                fs::read(path.join(format!(
                    "tasks/{}/attempts/2-7/exports/outputs/result.txt",
                    task().id
                )))?,
                BYTES
            );
        }
        self.event("deleted_after_retention");
        Ok(())
    }
}

#[async_trait]
impl BrokerLeaseFence for Boundary {
    async fn mark_running(&self, _: Uuid, _: &str, _: i64) -> Result<bool> {
        self.event("marked_running");
        Ok(true)
    }

    async fn cancel_requested(&self, _: Uuid, _: &str, _: i64) -> Result<bool> {
        Ok(false)
    }

    async fn renew_lease(&self, _: Uuid, _: &str, _: i64, _: i32) -> Result<bool> {
        let current = self.renewals.fetch_add(1, Ordering::SeqCst) + 1;
        self.event("renewed");
        Ok(current != self.fail_renewal)
    }

    async fn register_artifact(
        &self,
        _: Uuid,
        _: &str,
        _: i64,
        artifact: &RetainedArtifact,
    ) -> Result<()> {
        if let Some(root) = &self.durable {
            assert_eq!(fs::read(root.join(&artifact.relative_path))?, BYTES);
        }
        self.event("retained");
        Ok(())
    }

    async fn finish_task(
        &self,
        _: Uuid,
        _: &str,
        _: i64,
        passed: bool,
        _: &str,
        _: &str,
    ) -> Result<bool> {
        assert_eq!(passed, self.terminal_succeeded);
        self.event("finished");
        Ok(true)
    }
}

#[derive(Default)]
struct Recorder {
    events: AtomicUsize,
    accepted: bool,
}

#[async_trait]
impl RuntimeEventRecorder for Recorder {
    async fn record_runtime_event(
        &self,
        task: &ClaimedTask,
        worker_id: &str,
        event: &RuntimeEvent,
    ) -> Result<bool> {
        event.metadata(task, worker_id)?;
        self.events.fetch_add(1, Ordering::SeqCst);
        Ok(self.accepted)
    }
}

fn runtime(boundary: Boundary) -> SandboxRuntime<Boundary> {
    SandboxRuntime::new(
        boundary,
        SandboxBindingCatalog::new(vec![binding()]).unwrap(),
        60,
    )
    .unwrap()
}

fn driver() -> DirectExecutionDriver {
    DirectExecutionDriver::new(DirectExecutionTiming {
        lease_seconds: 60,
        poll_interval: Duration::from_millis(1),
        maximum_wait: Duration::from_secs(5),
    })
    .unwrap()
}

#[tokio::test]
async fn admitted_execution_retains_real_bytes_before_exact_cleanup() {
    let temp = TempDir::new().unwrap();
    let admitted = admitted(&temp);
    let source = temp.path().canonicalize().unwrap().join("mounted");
    let durable = temp.path().canonicalize().unwrap().join("durable");
    fs::create_dir_all(source.join("outputs")).unwrap();
    fs::create_dir(&durable).unwrap();
    mode(&durable, 0o700);
    fs::write(source.join("outputs/result.txt"), BYTES).unwrap();
    let retained = durable.join(format!(
        "tasks/{}/attempts/2-7/exports/outputs/result.txt",
        task().id
    ));
    let run = admitted.authorization.run_id.clone();
    let boundary = Boundary::new(
        run.clone(),
        vec![
            pod(&run, "Running", "first-pod", None),
            pod(&run, "Succeeded", "first-pod", Some(0)),
        ],
        0,
        Some(temp.path().canonicalize().unwrap().join("durable")),
        true,
    );
    let runtime = runtime(boundary.clone());
    let cancellation = CancellationToken::new();
    let shutdown = CancellationToken::new();
    let recorder = Recorder {
        accepted: true,
        ..Default::default()
    };
    let factory = FilesystemFactory {
        roots: ExportRoots {
            mounted_artifacts: source,
            durable_artifacts: durable,
        },
    };
    let outcome = driver()
        .run(
            &runtime,
            &boundary,
            &recorder,
            &task(),
            "trusted-worker",
            &admitted,
            DirectExecutionControl {
                cancellation: &cancellation,
                shutdown: &shutdown,
            },
            &factory,
        )
        .await
        .unwrap();
    let DirectExecutionOutcome::Finalized { completion, .. } = outcome else {
        panic!("expected finalized execution: {outcome:?}")
    };
    assert!(completion.succeeded());
    assert_eq!(fs::read(retained).unwrap(), BYTES);
    let events = boundary.events();
    assert!(
        events.iter().position(|event| *event == "retained")
            < events
                .iter()
                .position(|event| *event == "deleted_after_retention")
    );
    assert_eq!(recorder.events.load(Ordering::SeqCst), 3);
}

#[tokio::test]
async fn failure_cancellation_lease_loss_and_replacement_preserve_completion_boundaries() {
    for scenario in ["failure", "cancel", "lease", "replacement"] {
        let temp = TempDir::new().unwrap();
        let admitted = admitted(&temp);
        let run = admitted.authorization.run_id.clone();
        let pods = if scenario == "replacement" {
            vec![
                pod(&run, "Running", "first-pod", None),
                pod(&run, "Succeeded", "replacement-pod", Some(0)),
            ]
        } else if scenario == "failure" {
            vec![
                pod(&run, "Running", "first-pod", None),
                pod(&run, "Failed", "first-pod", Some(137)),
            ]
        } else {
            vec![pod(&run, "Running", "first-pod", None)]
        };
        let boundary = Boundary::new(
            run,
            pods,
            if scenario == "lease" { 3 } else { 0 },
            None,
            scenario != "failure",
        );
        let runtime = runtime(boundary.clone());
        let cancellation = CancellationToken::new();
        if scenario == "cancel" {
            cancellation.cancel();
        }
        let shutdown = CancellationToken::new();
        let recorder = Recorder {
            accepted: true,
            ..Default::default()
        };
        let outcome = driver()
            .run(
                &runtime,
                &boundary,
                &recorder,
                &task(),
                "trusted-worker",
                &admitted,
                DirectExecutionControl {
                    cancellation: &cancellation,
                    shutdown: &shutdown,
                },
                &MockRetainerFactory,
            )
            .await
            .unwrap();
        if scenario == "failure" {
            let DirectExecutionOutcome::Finalized { completion, .. } = outcome else {
                panic!("failed terminal must be finalized as failure")
            };
            assert!(!completion.succeeded());
            assert!(boundary.events().contains(&"deleted_after_retention"));
            assert!(boundary.events().contains(&"finished"));
        } else {
            let DirectExecutionOutcome::RecoveryNeeded { handle, reason } = outcome else {
                panic!("{scenario} must need recovery")
            };
            assert_eq!(
                reason,
                match scenario {
                    "cancel" => DirectExecutionInterruption::CancellationRequested,
                    "lease" => DirectExecutionInterruption::LeaseLost,
                    _ => DirectExecutionInterruption::ObservationFailed,
                }
            );
            assert_eq!(handle.is_some(), scenario != "cancel");
            assert!(!boundary.events().contains(&"deleted_after_retention"));
            assert!(!boundary.events().contains(&"finished"));
        }
    }
}

#[tokio::test]
async fn pending_start_renews_and_stops_without_creating_compute() {
    let temp = TempDir::new().unwrap();
    let admitted = admitted(&temp);
    let run = admitted.authorization.run_id.clone();
    let stalled_start = Arc::new(Notify::new());
    let boundary = Boundary::new(run, vec![], 0, None, true).with_stalled_start(stalled_start);
    let runtime = runtime(boundary.clone());
    let cancellation = CancellationToken::new();
    let shutdown = CancellationToken::new();
    let recorder = Recorder {
        accepted: true,
        ..Default::default()
    };
    let task = task();
    let execution_driver = driver();
    let mut execution = Box::pin(execution_driver.run(
        &runtime,
        &boundary,
        &recorder,
        &task,
        "trusted-worker",
        &admitted,
        DirectExecutionControl {
            cancellation: &cancellation,
            shutdown: &shutdown,
        },
        &MockRetainerFactory,
    ));
    timeout(Duration::from_secs(1), async {
        tokio::select! {
            _ = async {
                while boundary.renewals.load(Ordering::SeqCst) < 2 {
                    tokio::time::sleep(Duration::from_millis(1)).await;
                }
            } => {}
            outcome = execution.as_mut() => panic!("pending startup ended unexpectedly: {outcome:?}"),
        }
    })
    .await
    .expect("pending startup must receive a lease heartbeat");
    cancellation.cancel();
    let outcome = timeout(Duration::from_secs(1), execution.as_mut())
        .await
        .expect("pending startup must honor cancellation")
        .unwrap();
    let DirectExecutionOutcome::RecoveryNeeded { handle, reason } = outcome else {
        panic!("interrupted startup must need recovery")
    };
    assert_eq!(handle, None);
    assert_eq!(reason, DirectExecutionInterruption::CancellationRequested);
    assert!(!boundary.events().contains(&"created"));
    assert!(!boundary.events().contains(&"deleted_after_retention"));
}

#[tokio::test]
async fn dropping_pending_terminal_preparation_cancels_factory() {
    let temp = TempDir::new().unwrap();
    let admitted = admitted(&temp);
    let run = admitted.authorization.run_id.clone();
    let boundary = Boundary::new(
        run.clone(),
        vec![
            pod(&run, "Running", "first-pod", None),
            pod(&run, "Succeeded", "first-pod", Some(0)),
        ],
        0,
        None,
        true,
    );
    let runtime = runtime(boundary.clone());
    let cancellation = CancellationToken::new();
    let shutdown = CancellationToken::new();
    let recorder = Recorder {
        accepted: true,
        ..Default::default()
    };
    let factory = PendingFactory::default();
    let task = task();
    let execution_driver = driver();
    let mut execution = Box::pin(execution_driver.run(
        &runtime,
        &boundary,
        &recorder,
        &task,
        "trusted-worker",
        &admitted,
        DirectExecutionControl {
            cancellation: &cancellation,
            shutdown: &shutdown,
        },
        &factory,
    ));
    timeout(Duration::from_secs(1), async {
        tokio::select! {
            _ = factory.entered.notified() => {}
            outcome = execution.as_mut() => panic!("terminal preparation ended unexpectedly: {outcome:?}"),
        }
    })
    .await
    .expect("factory must begin after terminal evidence");
    drop(execution);
    timeout(Duration::from_secs(1), factory.cancelled.notified())
        .await
        .expect("dropping the driver must cancel the factory worker");
    assert!(!boundary.events().contains(&"deleted_after_retention"));
}

struct MockRetainer;

#[async_trait]
impl ArtifactRetainer for MockRetainer {
    async fn retain(
        &self,
        _: &SandboxHandle,
        _: &CancellationToken,
    ) -> Result<Vec<RetainedArtifact>> {
        Ok(vec![RetainedArtifact {
            visibility: "operator".into(),
            role: "evidence".into(),
            label: "Synthetic terminal receipt".into(),
            relative_path: "terminal/receipt.json".into(),
            media_type: "application/json".into(),
            size_bytes: 1,
            sha256: "a".repeat(64),
        }])
    }
}

struct MockRetainerFactory;

#[async_trait]
impl TerminalArtifactRetainerFactory for MockRetainerFactory {
    type Retainer = MockRetainer;

    async fn prepare_terminal_retainer(
        &self,
        _: &ClaimedTask,
        _: &SandboxHandle,
        _: &enrichedlab_agent_sandbox::broker::sandbox::RuntimeCompletion,
        _: &CancellationToken,
    ) -> Result<Self::Retainer> {
        Ok(MockRetainer)
    }
}

#[derive(Default)]
struct PendingFactory {
    entered: Arc<Notify>,
    cancelled: Arc<Notify>,
}

#[async_trait]
impl TerminalArtifactRetainerFactory for PendingFactory {
    type Retainer = MockRetainer;

    async fn prepare_terminal_retainer(
        &self,
        _: &ClaimedTask,
        _: &SandboxHandle,
        _: &enrichedlab_agent_sandbox::broker::sandbox::RuntimeCompletion,
        cancellation: &CancellationToken,
    ) -> Result<Self::Retainer> {
        self.entered.notify_one();
        let worker_cancellation = cancellation.clone();
        let cancelled = self.cancelled.clone();
        tokio::spawn(async move {
            worker_cancellation.cancelled().await;
            cancelled.notify_one();
        });
        std::future::pending::<()>().await;
        unreachable!("pending factory cannot complete")
    }
}

struct FilesystemFactory {
    roots: ExportRoots,
}

#[async_trait]
impl TerminalArtifactRetainerFactory for FilesystemFactory {
    type Retainer = FilesystemArtifactRetainer;

    async fn prepare_terminal_retainer(
        &self,
        task: &ClaimedTask,
        handle: &SandboxHandle,
        _: &enrichedlab_agent_sandbox::broker::sandbox::RuntimeCompletion,
        cancellation: &CancellationToken,
    ) -> Result<Self::Retainer> {
        if cancellation.is_cancelled() {
            bail!("factory_cancelled")
        }
        FilesystemArtifactRetainer::for_task(
            task,
            &binding(),
            handle,
            self.roots.clone(),
            vec![ExportFile {
                relative_path: "outputs/result.txt".into(),
                visibility: "researcher".into(),
                role: "result".into(),
                label: "Synthetic result".into(),
                media_type: "text/plain".into(),
                size_bytes: BYTES.len() as u64,
                sha256: hex::encode(Sha256::digest(BYTES)),
            }],
            ExportLimits {
                max_files: 1,
                max_bytes: 1024,
            },
        )
    }
}
