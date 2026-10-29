#![cfg(unix)]

use anyhow::{Result, bail};
use async_trait::async_trait;
use enrichedlab_agent_sandbox::broker::{
    artifact_retainer::{
        ExportFile, ExportLimits, ExportRoots, FilesystemArtifactRetainer, StreamArtifactRetainer,
        open_selected_file,
    },
    model::ClaimedTask,
    sandbox::{
        ArtifactRetainer, BrokerLeaseFence, RetainedArtifact, SandboxBinding,
        SandboxBindingCatalog, SandboxHandle, SandboxRuntime, SandboxTransport,
    },
};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::{
    fs,
    io::Read,
    os::unix::{
        ffi::OsStrExt,
        fs::{PermissionsExt, symlink},
    },
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    },
    task::{Context, Poll},
    time::Duration,
};
use tempfile::TempDir;
use tokio::io::{AsyncRead, AsyncWriteExt, DuplexStream, ReadBuf, duplex};
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

const BYTES: &[u8] = b"sample,value\nsynthetic,12.500\n";

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
        sandbox_name: "run-example".into(),
        sandbox_uid: Uuid::nil().to_string(),
        sandbox_generation: 1,
        lab_ref: binding().lab_ref,
    }
}
fn limits() -> ExportLimits {
    ExportLimits {
        max_files: 10,
        max_bytes: 1024,
    }
}
fn selected() -> ExportFile {
    ExportFile {
        relative_path: "step/result.csv".into(),
        visibility: "researcher".into(),
        role: "result".into(),
        label: "Synthetic result".into(),
        media_type: "text/csv".into(),
        size_bytes: BYTES.len() as u64,
        sha256: hex::encode(Sha256::digest(BYTES)),
    }
}
fn mode(path: &Path, value: u32) {
    fs::set_permissions(path, fs::Permissions::from_mode(value)).unwrap();
}
fn fixture() -> (TempDir, ExportRoots) {
    let temp = TempDir::new().unwrap();
    let root = temp.path().canonicalize().unwrap();
    let source = root.join("source");
    let destination = root.join("durable");
    fs::create_dir_all(source.join("step")).unwrap();
    fs::create_dir(&destination).unwrap();
    mode(&destination, 0o700);
    fs::write(source.join(selected().relative_path), BYTES).unwrap();
    (
        temp,
        ExportRoots {
            mounted_artifacts: source,
            durable_artifacts: destination,
        },
    )
}
fn plan(roots: &ExportRoots, files: Vec<ExportFile>) -> FilesystemArtifactRetainer {
    FilesystemArtifactRetainer::for_task(
        &task(),
        &binding(),
        &handle(),
        roots.clone(),
        files,
        limits(),
    )
    .unwrap()
}
fn stream_plan(roots: &ExportRoots, files: Vec<ExportFile>) -> StreamArtifactRetainer {
    StreamArtifactRetainer::for_task(
        &task(),
        &binding(),
        &handle(),
        roots.durable_artifacts.clone(),
        files,
        limits(),
    )
    .unwrap()
}
async fn bytes_input(bytes: &[u8]) -> DuplexStream {
    let (mut sender, receiver) = duplex(64 * 1024);
    sender.write_all(bytes).await.unwrap();
    drop(sender);
    receiver
}
fn published_path(roots: &ExportRoots) -> PathBuf {
    roots.durable_artifacts.join(format!(
        "tasks/{}/attempts/2-7/exports/step/result.csv",
        task().id
    ))
}
fn assert_no_partial_files(root: &Path) {
    assert!(walkdir::WalkDir::new(root).into_iter().all(|entry| {
        !entry
            .unwrap()
            .file_name()
            .to_string_lossy()
            .ends_with(".partial")
    }));
}

#[tokio::test]
async fn retains_original_bytes_after_source_cleanup_and_retries_without_overwrite() {
    let (_temp, roots) = fixture();
    let retainer = plan(&roots, vec![selected()]);
    let artifacts = retainer
        .retain(&handle(), &CancellationToken::new())
        .await
        .unwrap();
    assert_eq!(artifacts.len(), 1);
    assert_eq!(artifacts[0].visibility, "researcher");
    assert_eq!(artifacts[0].sha256, selected().sha256);
    assert_eq!(artifacts[0].size_bytes, BYTES.len() as i64);
    assert_eq!(fs::read(published_path(&roots)).unwrap(), BYTES);
    assert_eq!(
        retainer
            .retain(&handle(), &CancellationToken::new())
            .await
            .unwrap(),
        artifacts
    );
    fs::remove_dir_all(&roots.mounted_artifacts).unwrap();
    assert_eq!(fs::read(published_path(&roots)).unwrap(), BYTES);
    assert_no_partial_files(&roots.durable_artifacts);
}

#[tokio::test]
async fn stream_retention_preserves_original_bytes_and_reuses_only_verified_result() {
    let (_temp, roots) = fixture();
    let retainer = stream_plan(&roots, vec![selected()]);
    assert_eq!(retainer.files().len(), 1);
    assert!(retainer.check_handle(&handle()).is_ok());
    let mut rebound = handle();
    rebound.sandbox_uid = Uuid::new_v4().to_string();
    assert!(retainer.check_handle(&rebound).is_err());

    let artifact = retainer
        .retain_stream(0, bytes_input(BYTES).await, &CancellationToken::new())
        .await
        .unwrap();
    assert_eq!(
        artifact.relative_path,
        published_path(&roots)
            .strip_prefix(&roots.durable_artifacts)
            .unwrap()
            .to_str()
            .unwrap()
    );
    assert_eq!(fs::read(published_path(&roots)).unwrap(), BYTES);

    // A retry never accepts a divergent result and does not need to consume a
    // replacement source after the retained descriptor verifies.
    let retried = retainer
        .retain_stream(0, bytes_input(b"not read").await, &CancellationToken::new())
        .await
        .unwrap();
    assert_eq!(retried, artifact);
    fs::write(published_path(&roots), vec![b'x'; BYTES.len()]).unwrap();
    assert!(
        retainer
            .retain_stream(0, bytes_input(BYTES).await, &CancellationToken::new())
            .await
            .is_err()
    );
}

struct CancellingInput {
    first: Vec<u8>,
    emitted: bool,
    cancellation: CancellationToken,
}
impl AsyncRead for CancellingInput {
    fn poll_read(
        mut self: std::pin::Pin<&mut Self>,
        _: &mut Context<'_>,
        buffer: &mut ReadBuf<'_>,
    ) -> Poll<std::io::Result<()>> {
        if !self.emitted {
            self.emitted = true;
            buffer.put_slice(&self.first);
        } else {
            self.cancellation.cancel();
        }
        Poll::Ready(Ok(()))
    }
}

#[tokio::test]
async fn stream_retention_rejects_wrong_bytes_and_cleans_cancelled_partial() {
    let (_temp, roots) = fixture();
    let retainer = stream_plan(&roots, vec![selected()]);
    assert!(
        retainer
            .retain_stream(
                0,
                bytes_input(&vec![b'x'; BYTES.len()]).await,
                &CancellationToken::new(),
            )
            .await
            .is_err()
    );
    assert!(!published_path(&roots).exists());
    assert_no_partial_files(&roots.durable_artifacts);
    assert!(
        retainer
            .retain_stream(
                0,
                bytes_input(&BYTES[..BYTES.len() - 1]).await,
                &CancellationToken::new(),
            )
            .await
            .is_err()
    );
    assert!(!published_path(&roots).exists());
    assert_no_partial_files(&roots.durable_artifacts);

    let cancellation = CancellationToken::new();
    assert!(
        retainer
            .retain_stream(
                0,
                CancellingInput {
                    first: BYTES[..8].to_vec(),
                    emitted: false,
                    cancellation: cancellation.clone(),
                },
                &cancellation,
            )
            .await
            .is_err()
    );
    assert!(!published_path(&roots).exists());
    assert_no_partial_files(&roots.durable_artifacts);
}

#[test]
fn selected_file_reader_uses_held_root_and_rejects_invalid_relative_path() {
    let (_temp, roots) = fixture();
    let mut opened = open_selected_file(&roots.mounted_artifacts, &selected()).unwrap();
    let mut bytes = Vec::new();
    opened.read_to_end(&mut bytes).unwrap();
    assert_eq!(bytes, BYTES);
    let mut invalid = selected();
    invalid.relative_path = "../outside".into();
    assert!(open_selected_file(&roots.mounted_artifacts, &invalid).is_err());
}

#[test]
fn rejects_cross_task_bindings_unsafe_paths_and_untrusted_metadata() {
    let (_temp, roots) = fixture();
    let mut other = task();
    other.principal_id = Uuid::new_v4();
    assert!(
        FilesystemArtifactRetainer::for_task(
            &other,
            &binding(),
            &handle(),
            roots.clone(),
            vec![selected()],
            limits()
        )
        .is_err()
    );
    other = task();
    other.lease_generation += 1;
    assert!(
        FilesystemArtifactRetainer::for_task(
            &other,
            &binding(),
            &handle(),
            roots.clone(),
            vec![selected()],
            limits()
        )
        .is_err()
    );
    for path in [
        "../outside",
        "step/../../outside",
        "/outside",
        "step//result",
        "./result",
        "step\\result",
        "bad\0path",
    ] {
        let mut file = selected();
        file.relative_path = path.into();
        assert!(
            FilesystemArtifactRetainer::for_task(
                &task(),
                &binding(),
                &handle(),
                roots.clone(),
                vec![file],
                limits()
            )
            .is_err(),
            "{path:?}"
        );
    }
    for field in ["visibility", "role", "sha256"] {
        let mut file = selected();
        match field {
            "visibility" => file.visibility = "owner".into(),
            "role" => file.role = "unknown".into(),
            _ => file.sha256 = format!("sha256:{}", file.sha256),
        }
        assert!(
            FilesystemArtifactRetainer::for_task(
                &task(),
                &binding(),
                &handle(),
                roots.clone(),
                vec![file],
                limits()
            )
            .is_err()
        );
    }
    assert!(
        FilesystemArtifactRetainer::for_task(
            &task(),
            &binding(),
            &handle(),
            roots.clone(),
            vec![selected(), selected()],
            limits()
        )
        .is_err()
    );
    assert!(
        FilesystemArtifactRetainer::for_task(
            &task(),
            &binding(),
            &handle(),
            roots,
            vec![selected()],
            ExportLimits {
                max_files: 1,
                max_bytes: 1
            }
        )
        .is_err()
    );
}

#[tokio::test]
async fn refuses_symlinks_hard_links_and_special_files_without_hanging() {
    for shape in [
        "file_symlink",
        "parent_symlink",
        "root_symlink",
        "hard_link",
        "fifo",
        "destination_symlink",
        "public_destination",
    ] {
        let (_temp, mut roots) = fixture();
        let input = roots.mounted_artifacts.join(selected().relative_path);
        match shape {
            "file_symlink" => {
                fs::rename(&input, roots.mounted_artifacts.join("original")).unwrap();
                symlink("../original", &input).unwrap();
            }
            "parent_symlink" => {
                fs::rename(
                    roots.mounted_artifacts.join("step"),
                    roots.mounted_artifacts.join("original"),
                )
                .unwrap();
                symlink("original", roots.mounted_artifacts.join("step")).unwrap();
            }
            "root_symlink" => {
                let alias = roots.mounted_artifacts.with_file_name("alias");
                symlink(&roots.mounted_artifacts, &alias).unwrap();
                roots.mounted_artifacts = alias;
            }
            "hard_link" => fs::hard_link(&input, roots.mounted_artifacts.join("alias")).unwrap(),
            "fifo" => {
                fs::remove_file(&input).unwrap();
                let name = std::ffi::CString::new(input.as_os_str().as_bytes()).unwrap();
                assert_eq!(unsafe { libc::mkfifo(name.as_ptr(), 0o600) }, 0);
            }
            "destination_symlink" => symlink(
                &roots.mounted_artifacts,
                roots.durable_artifacts.join("tasks"),
            )
            .unwrap(),
            _ => mode(&roots.durable_artifacts, 0o755),
        }
        let retainer = plan(&roots, vec![selected()]);
        assert!(
            tokio::time::timeout(
                Duration::from_secs(2),
                retainer.retain(&handle(), &CancellationToken::new())
            )
            .await
            .unwrap()
            .is_err(),
            "{shape}"
        );
        assert_no_partial_files(&roots.durable_artifacts);
    }
}

#[tokio::test]
async fn refuses_changed_bytes_cancelled_copy_and_rebound_handle() {
    let (_temp, roots) = fixture();
    let retainer = plan(&roots, vec![selected()]);
    let mut other = handle();
    other.sandbox_uid = Uuid::new_v4().to_string();
    assert!(
        retainer
            .retain(&other, &CancellationToken::new())
            .await
            .is_err()
    );
    let cancellation = CancellationToken::new();
    cancellation.cancel();
    assert!(retainer.retain(&handle(), &cancellation).await.is_err());
    fs::write(
        roots.mounted_artifacts.join(selected().relative_path),
        vec![b'x'; BYTES.len()],
    )
    .unwrap();
    assert!(
        retainer
            .retain(&handle(), &CancellationToken::new())
            .await
            .is_err()
    );
    assert!(!published_path(&roots).exists());
    assert_no_partial_files(&roots.durable_artifacts);
    fs::write(
        roots.mounted_artifacts.join(selected().relative_path),
        BYTES,
    )
    .unwrap();
    retainer
        .retain(&handle(), &CancellationToken::new())
        .await
        .unwrap();
    fs::write(published_path(&roots), vec![b'x'; BYTES.len()]).unwrap();
    assert!(
        retainer
            .retain(&handle(), &CancellationToken::new())
            .await
            .is_err()
    );
    assert_eq!(
        fs::read(published_path(&roots)).unwrap(),
        vec![b'x'; BYTES.len()]
    );
}

#[derive(Clone)]
struct Boundary {
    events: Arc<Mutex<Vec<&'static str>>>,
    renewals: Arc<AtomicUsize>,
    fail_renewal: usize,
    cancel_requested: bool,
    durable: PathBuf,
}
impl Boundary {
    fn new(roots: &ExportRoots, fail_renewal: usize) -> Self {
        Self {
            events: Arc::default(),
            renewals: Arc::default(),
            fail_renewal,
            cancel_requested: false,
            durable: roots.durable_artifacts.clone(),
        }
    }
    fn event(&self, event: &'static str) {
        self.events.lock().unwrap().push(event);
    }
    fn observed(&self) -> Vec<&'static str> {
        self.events.lock().unwrap().clone()
    }
}
#[async_trait]
impl SandboxTransport for Boundary {
    async fn create_sandbox(&self, _: &str, _: &Value) -> Result<Value> {
        bail!("unexpected_create")
    }
    async fn get_sandbox(&self, _: &str, _: &str) -> Result<Value> {
        bail!("unexpected_get")
    }
    async fn get_pod(&self, _: &str, _: &str) -> Result<Option<Value>> {
        bail!("unexpected_get")
    }
    async fn get_pvc(&self, _: &str, _: &str) -> Result<Value> {
        bail!("unexpected_get")
    }
    async fn delete_sandbox(&self, namespace: &str, name: &str, uid: &str) -> Result<()> {
        assert_eq!(
            (namespace, name, uid),
            (
                handle().namespace.as_str(),
                handle().sandbox_name.as_str(),
                handle().sandbox_uid.as_str()
            )
        );
        self.event("delete_exact_sandbox");
        Ok(())
    }
}
#[async_trait]
impl BrokerLeaseFence for Boundary {
    async fn mark_running(&self, _: Uuid, _: &str, _: i64) -> Result<bool> {
        bail!("unexpected_start")
    }
    async fn cancel_requested(&self, _: Uuid, _: &str, _: i64) -> Result<bool> {
        Ok(self.cancel_requested)
    }
    async fn renew_lease(&self, _: Uuid, _: &str, _: i64, _: i32) -> Result<bool> {
        let count = self.renewals.fetch_add(1, Ordering::SeqCst) + 1;
        self.event("renew_lease");
        Ok(count != self.fail_renewal)
    }
    async fn register_artifact(
        &self,
        id: Uuid,
        worker: &str,
        generation: i64,
        artifact: &RetainedArtifact,
    ) -> Result<()> {
        assert_eq!(
            (id, worker, generation),
            (
                task().id,
                handle().worker_id.as_str(),
                task().lease_generation
            )
        );
        assert_eq!(fs::read(self.durable.join(&artifact.relative_path))?, BYTES);
        self.event("register_verified_file");
        Ok(())
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
        self.event("finish_task");
        Ok(true)
    }
}
fn runtime(boundary: &Boundary) -> SandboxRuntime<Boundary> {
    SandboxRuntime::new(
        boundary.clone(),
        SandboxBindingCatalog::new(vec![binding()]).unwrap(),
        60,
    )
    .unwrap()
}

#[tokio::test]
async fn cleanup_requires_retained_bytes_registered_under_a_current_lease() {
    for failure in ["none", "cancelled", "bad_digest", "lease_lost"] {
        let (_temp, roots) = fixture();
        let mut boundary = Boundary::new(&roots, if failure == "lease_lost" { 2 } else { 0 });
        boundary.cancel_requested = failure == "cancelled";
        if failure == "bad_digest" {
            fs::write(
                roots.mounted_artifacts.join(selected().relative_path),
                vec![b'x'; BYTES.len()],
            )
            .unwrap();
        }
        let result = runtime(&boundary)
            .finish(
                &boundary,
                &plan(&roots, vec![selected()]),
                &task(),
                &handle(),
                true,
                "success",
                "complete",
            )
            .await;
        if matches!(failure, "none" | "cancelled") {
            result.unwrap();
            assert_eq!(
                boundary.observed(),
                [
                    "renew_lease",
                    "register_verified_file",
                    "renew_lease",
                    "delete_exact_sandbox",
                    "finish_task"
                ]
            );
        } else {
            assert!(result.is_err());
            assert!(!boundary.observed().contains(&"delete_exact_sandbox"));
            assert!(!boundary.observed().contains(&"finish_task"));
            if failure == "lease_lost" {
                assert_eq!(fs::read(published_path(&roots)).unwrap(), BYTES);
            }
        }
    }
}

struct WaitingRetainer(Boundary);
#[async_trait]
impl ArtifactRetainer for WaitingRetainer {
    async fn retain(
        &self,
        _: &SandboxHandle,
        cancellation: &CancellationToken,
    ) -> Result<Vec<RetainedArtifact>> {
        self.0.event("retainer_started");
        cancellation.cancelled().await;
        self.0.event("retainer_cancelled");
        bail!("export_cancelled")
    }
}
#[tokio::test]
async fn long_exports_renew_custody_and_cancel_before_cleanup_on_lease_loss() {
    let (_temp, roots) = fixture();
    let boundary = Boundary::new(&roots, 2);
    let retainer = WaitingRetainer(boundary.clone());
    let task = task();
    let handle = handle();
    let runtime = runtime(&boundary);
    let result = tokio::time::timeout(
        Duration::from_secs(25),
        runtime.finish(
            &boundary, &retainer, &task, &handle, true, "success", "complete",
        ),
    )
    .await
    .unwrap();
    assert!(
        result
            .unwrap_err()
            .to_string()
            .contains("custody was lost during artifact retention")
    );
    assert_eq!(
        boundary.observed(),
        [
            "renew_lease",
            "retainer_started",
            "renew_lease",
            "retainer_cancelled"
        ]
    );
}
