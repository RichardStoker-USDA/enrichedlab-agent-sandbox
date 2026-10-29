#![cfg(unix)]
use anyhow::{Result, bail};
use async_trait::async_trait;
use enrichedlab_agent_sandbox::broker::{
    dispatch::{AdmissionWait, DirectDispatchCatalog, admission_record_name},
    input_request::InputPreparationRequest,
    model::ClaimedTask,
    sandbox::{BrokerLeaseFence, RetainedArtifact},
};
use serde_json::{Value, json};
use std::{
    fs,
    os::unix::fs::{PermissionsExt, symlink},
    path::PathBuf,
    sync::atomic::{AtomicBool, AtomicUsize, Ordering},
    time::Duration,
};
use tempfile::TempDir;
use tokio::sync::Notify;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;
const RECORD: &[u8] = include_bytes!("fixtures/admission/staged_admission.json");
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

struct Lease {
    cancel: AtomicBool,
    lose: AtomicBool,
    reads: AtomicUsize,
    renewals: AtomicUsize,
    pending: Notify,
}
impl Lease {
    fn new() -> Self {
        Self {
            cancel: AtomicBool::new(false),
            lose: AtomicBool::new(false),
            reads: AtomicUsize::new(0),
            renewals: AtomicUsize::new(0),
            pending: Notify::new(),
        }
    }
}
#[async_trait]
impl BrokerLeaseFence for Lease {
    async fn mark_running(&self, _: Uuid, _: &str, _: i64) -> Result<bool> {
        bail!("preparation_must_not_start_compute")
    }
    async fn cancel_requested(&self, id: Uuid, worker: &str, generation: i64) -> Result<bool> {
        assert_eq!((id, worker, generation), (Uuid::nil(), "worker-test", 7));
        if self.reads.fetch_add(1, Ordering::SeqCst) == 1 {
            self.pending.notify_one();
        }
        Ok(self.cancel.load(Ordering::SeqCst))
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
            (Uuid::nil(), "worker-test", 7, 30)
        );
        self.renewals.fetch_add(1, Ordering::SeqCst);
        Ok(!self.lose.load(Ordering::SeqCst))
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
    root: PathBuf,
    catalog: DirectDispatchCatalog,
}
impl Fixture {
    fn new(wait: u32) -> Self {
        let temp = TempDir::new().unwrap();
        let parent = fs::canonicalize(temp.path()).unwrap();
        let root = parent.join("admissions");
        let config = parent.join("catalog");
        for path in [&root, &config] {
            fs::create_dir(path).unwrap();
            fs::set_permissions(path, fs::Permissions::from_mode(0o700)).unwrap();
        }
        let record: Value = serde_json::from_slice(RECORD).unwrap();
        let document = json!({"schemaVersion":"sandbox.enrichedlab.org/direct-dispatch/v1",
            "kubernetes":{"origin":"https://kubernetes.example.invalid","caPath":"/example/ca.crt","tokenPath":"/example/token"},
            "admission":{"root":root,"writerUid":unsafe{libc::geteuid()},"waitSeconds":wait},
            "bindings":[{"principalId":Uuid::nil(),"principalHash":record["authorization"]["principalHash"],
                "groupKey":"research","profileKey":"reference","profileId":"example-kata", "labRef":"lab://example/research",
                "namespace":"sandbox-example-001","runtimeClassName":"kata-clh"}]});
        let path = config.join("dispatch.json");
        fs::write(&path, serde_json::to_vec(&document).unwrap()).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
        let catalog = DirectDispatchCatalog::load(&path, unsafe { libc::geteuid() }).unwrap();
        Self {
            _temp: temp,
            root,
            catalog,
        }
    }
    fn name(&self) -> PathBuf {
        self.root.join(admission_record_name(&task()).unwrap())
    }
    fn enable_preparation(&mut self) -> PathBuf {
        let parent = self.root.parent().unwrap();
        let requests = parent.join("requests");
        fs::create_dir(&requests).unwrap();
        fs::set_permissions(&requests, fs::Permissions::from_mode(0o700)).unwrap();
        let path = parent.join("catalog/dispatch.json");
        let mut document: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        document["preparation"] = json!({"root":requests,"writerUid":unsafe{libc::geteuid()}});
        fs::write(&path, serde_json::to_vec(&document).unwrap()).unwrap();
        self.catalog = DirectDispatchCatalog::load(&path, unsafe { libc::geteuid() }).unwrap();
        requests
    }
    fn publish(&self, bytes: &[u8]) {
        let partial = self.root.join("staging.partial");
        fs::write(&partial, bytes).unwrap();
        fs::set_permissions(&partial, fs::Permissions::from_mode(0o600)).unwrap();
        fs::rename(partial, self.name()).unwrap();
    }
    async fn prepare(
        &self,
        lease: &Lease,
        stop: &CancellationToken,
    ) -> Result<Option<enrichedlab_agent_sandbox::broker::dispatch::PreparedDirectDispatch>> {
        self.catalog
            .prepare_for_execution(
                lease,
                &task(),
                AdmissionWait {
                    worker_id: "worker-test",
                    lease_seconds: 30,
                    poll_interval: Duration::from_millis(10),
                    stop,
                },
            )
            .await
    }
}
#[tokio::test]
async fn worker_publishes_private_task_notice_before_accepting_custodian_admission() {
    let mut f = Fixture::new(2);
    let requests = f.enable_preparation();
    let name = admission_record_name(&task()).unwrap();
    let lease = Lease::new();
    let stop = CancellationToken::new();
    let mut claimed = task();
    claimed.objective = "private-research-objective".into();
    claimed.route = "private-model-route".into();
    let custodian = async {
        tokio::time::timeout(Duration::from_secs(1), async {
            while !requests.join(&name).exists() {
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .unwrap();
        let notice =
            InputPreparationRequest::load(&requests, &name, unsafe { libc::geteuid() }).unwrap();
        assert!(notice.task().matches(&claimed));
        assert_eq!(notice.worker_id(), "worker-test");
        assert_eq!(notice.binding().profile_id, "example-kata");
        let bytes = fs::read(requests.join(&name)).unwrap();
        let value: Value = serde_json::from_slice(&bytes).unwrap();
        assert!(value["task"].get("objective").is_none());
        assert!(value["task"].get("route").is_none());
        assert!(
            !String::from_utf8(bytes)
                .unwrap()
                .contains("private-research-objective")
        );
        assert_eq!(
            fs::metadata(requests.join(&name))
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
        assert_eq!(fs::read_dir(&requests).unwrap().count(), 1);
        f.publish(RECORD);
    };
    let worker = f.catalog.prepare_for_execution(
        &lease,
        &claimed,
        AdmissionWait {
            worker_id: "worker-test",
            lease_seconds: 30,
            poll_interval: Duration::from_millis(5),
            stop: &stop,
        },
    );
    let (result, ()) = tokio::join!(worker, custodian);
    assert_eq!(result.unwrap().unwrap().binding.profile_id, "example-kata");
    assert!(
        InputPreparationRequest::load(&requests, "../other.json", unsafe { libc::geteuid() })
            .is_err()
    );
    let mut wrong: Value =
        serde_json::from_slice(&fs::read(requests.join(&name)).unwrap()).unwrap();
    wrong["task"]["leaseGeneration"] = json!(8);
    fs::write(requests.join(&name), serde_json::to_vec(&wrong).unwrap()).unwrap();
    assert_eq!(
        InputPreparationRequest::load(&requests, &name, unsafe { libc::geteuid() })
            .unwrap_err()
            .to_string(),
        "input_preparation_name_mismatch"
    );
}

#[tokio::test]
async fn preparation_notices_preserve_existing_records_and_require_private_roots() {
    for conflict in [false, true] {
        let mut f = Fixture::new(1);
        let requests = f.enable_preparation();
        let name = admission_record_name(&task()).unwrap();
        if conflict {
            fs::write(requests.join(&name), b"preserve-existing-notice").unwrap();
            fs::set_permissions(requests.join(&name), fs::Permissions::from_mode(0o600)).unwrap();
        } else {
            fs::set_permissions(&requests, fs::Permissions::from_mode(0o755)).unwrap();
        }
        assert_eq!(
            f.prepare(&Lease::new(), &CancellationToken::new())
                .await
                .unwrap_err()
                .to_string(),
            "input_preparation_publish_failed"
        );
        if conflict {
            assert_eq!(
                fs::read(requests.join(&name)).unwrap(),
                b"preserve-existing-notice"
            );
        } else {
            assert_eq!(fs::read_dir(requests).unwrap().count(), 0);
        }
        assert!(!f.name().exists());
    }
    let mut f = Fixture::new(1);
    let requests = f.enable_preparation();
    let lease = Lease::new();
    lease.cancel.store(true, Ordering::SeqCst);
    assert!(f.prepare(&lease, &CancellationToken::new()).await.is_err());
    assert_eq!(fs::read_dir(&requests).unwrap().count(), 0);
    f.publish(RECORD);
    assert!(
        f.prepare(&Lease::new(), &CancellationToken::new())
            .await
            .unwrap()
            .is_some()
    );
    assert_eq!(fs::read_dir(&requests).unwrap().count(), 0);
}
#[tokio::test]
async fn waits_for_atomic_admission_then_rechecks_authority_before_handoff() {
    let f = Fixture::new(1);
    let lease = Lease::new();
    let stop = CancellationToken::new();
    let writer = async {
        lease.pending.notified().await;
        f.publish(RECORD);
    };
    let (prepared, ()) = tokio::join!(f.prepare(&lease, &stop), writer);
    let prepared = prepared.unwrap().unwrap();
    assert_eq!(prepared.binding.profile_id, "example-kata");
    assert_eq!(lease.renewals.load(Ordering::SeqCst), 2);
    assert!(lease.reads.load(Ordering::SeqCst) >= 3);
    assert_eq!(fs::read(f.name()).unwrap(), RECORD);
}
#[tokio::test]
async fn pending_tasks_stop_for_shutdown_cancel_lease_loss_and_deadline() {
    let f = Fixture::new(1);
    let lease = Lease::new();
    let stopped = CancellationToken::new();
    stopped.cancel();
    assert_eq!(
        f.prepare(&lease, &stopped).await.unwrap_err().to_string(),
        "direct_dispatch_admission_stopped"
    );
    assert_eq!(lease.reads.load(Ordering::SeqCst), 0);
    let stop = CancellationToken::new();
    let canceller = async {
        lease.pending.notified().await;
        lease.cancel.store(true, Ordering::SeqCst);
    };
    let (cancelled, ()) = tokio::join!(f.prepare(&lease, &stop), canceller);
    assert_eq!(
        cancelled.unwrap_err().to_string(),
        "direct_dispatch_admission_cancelled"
    );
    let lease = Lease::new();
    lease.lose.store(true, Ordering::SeqCst);
    assert_eq!(
        f.prepare(&lease, &stop).await.unwrap_err().to_string(),
        "direct_dispatch_admission_lease_lost"
    );
    assert_eq!(
        f.prepare(&Lease::new(), &stop)
            .await
            .unwrap_err()
            .to_string(),
        "direct_dispatch_admission_timeout"
    );
    assert!(!f.name().exists());
}
#[tokio::test]
async fn unsafe_records_fail_instead_of_waiting_or_falling_back() {
    let stop = CancellationToken::new();
    for kind in 0..4 {
        let f = Fixture::new(1);
        match kind {
            0 => f.publish(b"invalid record"),
            1 => {
                symlink(f.root.join("absent"), f.name()).unwrap();
            }
            2 => {
                fs::remove_dir(&f.root).unwrap();
            }
            _ => {
                let mut value: Value = serde_json::from_slice(RECORD).unwrap();
                value["leaseGeneration"] = json!(6);
                f.publish(&serde_json::to_vec(&value).unwrap());
            }
        }
        assert_eq!(
            f.prepare(&Lease::new(), &stop)
                .await
                .unwrap_err()
                .to_string(),
            "direct_dispatch_admission_invalid"
        );
    }
    let f = Fixture::new(0);
    let lease = Lease::new();
    assert_eq!(
        f.prepare(&lease, &stop).await.unwrap_err().to_string(),
        "direct_dispatch_admission_invalid"
    );
    assert_eq!(lease.reads.load(Ordering::SeqCst), 0);
    let f = Fixture::new(1);
    let mut other = task();
    other.group_key = "unmatched".into();
    assert!(
        f.catalog
            .prepare_for_execution(
                &lease,
                &other,
                AdmissionWait {
                    worker_id: "worker-test",
                    lease_seconds: 30,
                    poll_interval: Duration::from_millis(10),
                    stop: &stop
                }
            )
            .await
            .unwrap()
            .is_none()
    );
    assert_eq!(lease.reads.load(Ordering::SeqCst), 0);
}
