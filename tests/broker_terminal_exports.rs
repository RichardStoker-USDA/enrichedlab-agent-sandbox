#![cfg(unix)]

use anyhow::{Result, bail};
use async_trait::async_trait;
use enrichedlab_agent_sandbox::broker::{
    admission,
    artifact_retainer::ExportLimits,
    execution::TerminalArtifactRetainerFactory,
    model::ClaimedTask,
    sandbox::{
        ArtifactRetainer, RuntimeCompletion, SandboxBinding, SandboxBindingCatalog, SandboxHandle,
        SandboxRuntime, SandboxTransport,
    },
    terminal_exports::{StagedTerminalRetainerFactory, TerminalExportConfig},
};
use serde_json::{Value, json};
use std::{
    fs,
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    time::Duration,
};
use tempfile::TempDir;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

const RUN: &str = "run-20260909-0001";
const OUTPUT: &[u8] = b"Original tool output\n";

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
        sandbox_uid: Uuid::nil().to_string(),
        sandbox_generation: 3,
        lab_ref: binding().lab_ref,
    }
}
fn uid() -> u32 {
    unsafe { libc::geteuid() }
}
fn mode(path: &Path, mode: u32) {
    fs::set_permissions(path, fs::Permissions::from_mode(mode)).unwrap();
}
fn record() -> Value {
    serde_json::from_str(include_str!("../examples/terminal_export.json")).unwrap()
}
fn write_record(path: &Path, value: &Value) {
    fs::write(path, serde_json::to_vec(value).unwrap()).unwrap();
    mode(path, 0o600);
}
struct Fixture {
    _temp: TempDir,
    path: PathBuf,
    output: PathBuf,
    durable: PathBuf,
    factory: StagedTerminalRetainerFactory,
}
fn fixture() -> Fixture {
    let temp = tempfile::Builder::new()
        .prefix("enrichedlab-terminal-test-")
        .tempdir_in("/tmp")
        .unwrap();
    let root = fs::canonicalize(temp.path()).unwrap();
    let records = root.join("records");
    let mounted = root.join("mounted");
    let durable = root.join("durable");
    for directory in [&records, &mounted, &durable] {
        fs::create_dir(directory).unwrap();
        mode(directory, 0o700);
    }
    let path = records.join(format!("{}-2-7.json", Uuid::nil()));
    let admission_path = records.join("admission.json");
    fs::write(
        &admission_path,
        include_bytes!("fixtures/admission/staged_admission.json"),
    )
    .unwrap();
    mode(&admission_path, 0o600);
    let admitted = admission::load(&records, "admission.json", uid(), &task(), &binding()).unwrap();
    let output = mounted.join(RUN).join("outputs/result.txt");
    fs::create_dir_all(output.parent().unwrap()).unwrap();
    fs::write(&output, OUTPUT).unwrap();
    let factory = StagedTerminalRetainerFactory::new(
        TerminalExportConfig::Mounted {
            record_root: records,
            writer_uid: uid(),
            mounted_artifacts_root: mounted,
        },
        &task(),
        &binding(),
        &admitted,
        durable.clone(),
        ExportLimits {
            max_files: 20,
            max_bytes: 1024,
        },
    )
    .unwrap();
    Fixture {
        _temp: temp,
        path,
        output,
        durable,
        factory,
    }
}

fn prepared_https(
    f: &Fixture,
    seconds: u32,
) -> (
    StagedTerminalRetainerFactory,
    enrichedlab_agent_sandbox::broker::terminal_request::TerminalPreparationConfig,
    enrichedlab_agent_sandbox::broker::export_transport::RemoteExportConfig,
) {
    let root = f.path.parent().unwrap().parent().unwrap();
    for child in ["preparation", "credentials"] {
        fs::create_dir(root.join(child)).unwrap();
        mode(&root.join(child), 0o700);
    }
    let config: TerminalExportConfig = serde_json::from_value(json!({
        "https":{"dnsSuffix":"svc.cluster.local","port":8443,
            "credentialRoot":root.join("credentials"),"writerUid":uid()},
        "preparation":{"root":root.join("preparation"),"writerUid":uid(),"maxWaitSeconds":seconds}
    }))
    .unwrap();
    let TerminalExportConfig::PreparedHttps { https, preparation } = &config else {
        panic!("preparation must be recognized, never discarded by legacy deserialization");
    };
    let (https, preparation) = (https.clone(), preparation.clone());
    let admitted = admission::load(
        f.path.parent().unwrap(),
        "admission.json",
        uid(),
        &task(),
        &binding(),
    )
    .unwrap();
    (
        StagedTerminalRetainerFactory::new(
            config,
            &task(),
            &binding(),
            &admitted,
            f.durable.clone(),
            ExportLimits {
                max_files: 20,
                max_bytes: 1024,
            },
        )
        .unwrap(),
        preparation,
        https,
    )
}

#[tokio::test]
async fn terminal_notice_binds_real_observation_and_cancels_pending_credentials() {
    use enrichedlab_agent_sandbox::broker::{
        export_client_staging::export_client_scope_exists, export_transport::ExportScope,
        terminal_request::TerminalPreparationRequest,
    };
    let f = fixture();
    let (factory, preparation, remote) = prepared_https(&f, 10);
    let completion = completion().await;
    let task = task();
    let handle = handle();
    let cancellation = CancellationToken::new();
    let scope = ExportScope::new(task.id, task.attempt, task.lease_generation).unwrap();
    let name = format!("{}.json", scope.credential_name());
    let preparing = factory.prepare_terminal_retainer(&task, &handle, &completion, &cancellation);
    let inspect = async {
        tokio::time::timeout(Duration::from_secs(2), async {
            while !preparation.root.join(&name).exists() {
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .unwrap();
        let notice = TerminalPreparationRequest::load(&preparation.root, &name, uid()).unwrap();
        assert!(notice.task().matches(&task));
        assert_eq!(notice.handle(), &handle);
        assert_eq!(notice.binding(), &binding());
        assert!(notice.completion().matches(&completion));
        let bytes = fs::read(preparation.root.join(&name)).unwrap();
        let doc: Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(doc.as_object().unwrap().len(), 5);
        assert!(doc["task"].get("objective").is_none());
        assert!(doc.get("files").is_none());
        assert!(doc.get("credentials").is_none());
        assert_eq!(
            fs::metadata(preparation.root.join(&name))
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
        assert!(!export_client_scope_exists(&remote, &scope).unwrap());
        cancellation.cancel();
    };
    let (result, ()) = tokio::join!(preparing, inspect);
    assert!(matches!(
        result.unwrap_err().to_string().as_str(),
        "terminal_export_cancelled" | "terminal_preparation_cancelled"
    ));
    assert!(preparation.root.join(name).exists());
}

#[tokio::test]
async fn prepared_https_deadline_and_malformed_scope_never_become_fallback() {
    use enrichedlab_agent_sandbox::broker::{
        export_client_staging::export_client_scope_exists, export_transport::ExportScope,
    };
    for malformed in [false, true] {
        let f = fixture();
        let (factory, _, remote) = prepared_https(&f, 1);
        let scope = ExportScope::new(task().id, 2, 7).unwrap();
        if malformed {
            let directory = remote.credential_root.join(scope.credential_name());
            fs::create_dir(&directory).unwrap();
            mode(&directory, 0o700);
            // Presence does not vouch for any credential bytes. The real client
            // must reject this incomplete bundle immediately, without networking.
            assert!(export_client_scope_exists(&remote, &scope).unwrap());
        }
        let completion = completion().await;
        let start = std::time::Instant::now();
        let error = factory
            .prepare_terminal_retainer(&task(), &handle(), &completion, &CancellationToken::new())
            .await
            .unwrap_err();
        if malformed {
            assert_ne!(error.to_string(), "terminal_preparation_deadline");
        } else {
            assert_eq!(error.to_string(), "terminal_preparation_deadline");
        }
        assert!(start.elapsed() < Duration::from_secs(3));
    }
}

#[test]
fn client_scope_probe_distinguishes_absence_from_unsafe_private_state() {
    use enrichedlab_agent_sandbox::broker::{
        export_client_staging::export_client_scope_exists, export_transport::ExportScope,
    };
    use std::os::unix::fs::symlink;
    let f = fixture();
    let (_, _, remote) = prepared_https(&f, 1);
    let scope = ExportScope::new(task().id, 2, 7).unwrap();
    let path = remote.credential_root.join(scope.credential_name());
    assert!(!export_client_scope_exists(&remote, &scope).unwrap());
    fs::write(&path, b"not a directory").unwrap();
    mode(&path, 0o600);
    assert!(export_client_scope_exists(&remote, &scope).is_err());
    fs::remove_file(&path).unwrap();
    symlink(f.path.parent().unwrap(), &path).unwrap();
    assert!(export_client_scope_exists(&remote, &scope).is_err());
    fs::remove_file(&path).unwrap();
    mode(&remote.credential_root, 0o755);
    assert!(export_client_scope_exists(&remote, &scope).is_err());
    mode(&remote.credential_root, 0o700);
    fs::remove_dir(&remote.credential_root).unwrap();
    assert!(export_client_scope_exists(&remote, &scope).is_err());
}

// Completion stays opaque: obtain it through the real Pod/owner/status validator.
struct TerminalBoundary;
#[async_trait]
impl SandboxTransport for TerminalBoundary {
    async fn create_sandbox(&self, _: &str, _: &Value) -> Result<Value> {
        bail!("unexpected_create")
    }
    async fn get_sandbox(&self, _: &str, _: &str) -> Result<Value> {
        let h = handle();
        Ok(
            json!({"metadata":{"name":RUN,"namespace":h.namespace,"uid":h.sandbox_uid,"generation":3},
            "status":{"conditions":[{"type":"Finished","status":"True","observedGeneration":3,"reason":"PodSucceeded"}]}}),
        )
    }
    async fn get_pod(&self, _: &str, _: &str) -> Result<Option<Value>> {
        let h = handle();
        Ok(Some(
            json!({"metadata":{"name":RUN,"namespace":h.namespace,"uid":"example-pod-uid",
            "ownerReferences":[{"apiVersion":"agents.x-k8s.io/v1beta1","kind":"Sandbox","uid":h.sandbox_uid,"controller":true}]},
            "spec":{"runtimeClassName":"kata-clh","restartPolicy":"Never","containers":[{"name":"runner"}]},
            "status":{"phase":"Succeeded","containerStatuses":[{"name":"runner","state":{"terminated":{
                "exitCode":0,"finishedAt":"2026-09-09T00:00:01Z"}}}]}}),
        ))
    }
    async fn get_pvc(&self, _: &str, _: &str) -> Result<Value> {
        bail!("unexpected_pvc")
    }
    async fn delete_sandbox(&self, _: &str, _: &str, _: &str) -> Result<()> {
        bail!("unexpected_delete")
    }
}
async fn completion() -> RuntimeCompletion {
    let runtime = SandboxRuntime::new(
        TerminalBoundary,
        SandboxBindingCatalog::new(vec![binding()]).unwrap(),
        60,
    )
    .unwrap();
    let observation = runtime
        .observe_pod(&task(), &handle())
        .await
        .unwrap()
        .unwrap();
    runtime
        .observe_terminal(&task(), &handle(), &observation)
        .await
        .unwrap()
        .unwrap()
}

#[tokio::test]
async fn terminal_record_retains_original_bytes_and_uses_its_own_size_bound() {
    let f = fixture();
    let completion = completion().await;
    let cancellation = CancellationToken::new();
    // A large file selection may exceed the unrelated admission's 256 KiB limit.
    let mut bytes = vec![b' '; 300 * 1024];
    bytes.extend(serde_json::to_vec(&record()).unwrap());
    fs::write(&f.path, bytes).unwrap();
    mode(&f.path, 0o600);
    let retainer = f
        .factory
        .prepare_terminal_retainer(&task(), &handle(), &completion, &cancellation)
        .await
        .unwrap();
    let files = retainer.retain(&handle(), &cancellation).await.unwrap();
    assert_eq!(files.len(), 1);
    assert_eq!(files[0].visibility, "researcher");
    assert_eq!(
        fs::read(f.durable.join(&files[0].relative_path)).unwrap(),
        OUTPUT
    );
    assert_eq!(
        files[0].sha256,
        record()["files"][0]["sha256"].as_str().unwrap()
    );
    // Existing admission contracts still reject a record above their own bound.
    assert!(
        admission::load(
            f.path.parent().unwrap(),
            f.path.file_name().unwrap().to_str().unwrap(),
            uid(),
            &task(),
            &binding()
        )
        .unwrap_err()
        .to_string()
        .contains("admission_size_exceeded")
    );
}

#[tokio::test]
async fn rejects_cross_run_identity_unapproved_metadata_and_changed_original_files() {
    let f = fixture();
    let completion = completion().await;
    let cancellation = CancellationToken::new();
    for (field, bad) in [
        ("taskId", json!(Uuid::new_v4())),
        ("principalId", json!(Uuid::new_v4())),
        ("leaseGeneration", json!(8)),
        ("workerId", json!("other-worker")),
        ("podUid", json!("other-pod")),
        ("artifactPvcName", json!("another-pvc")),
        ("finishedAt", json!("2026-09-09T00:00:02Z")),
        ("exitCode", json!(1)),
    ] {
        let mut candidate = record();
        candidate[field] = bad;
        write_record(&f.path, &candidate);
        let error = f
            .factory
            .prepare_terminal_retainer(&task(), &handle(), &completion, &cancellation)
            .await
            .unwrap_err();
        assert_eq!(
            error.to_string(),
            "terminal_export_record_mismatch",
            "{field}"
        );
    }
    for (field, bad) in [("relativePath", "../outside"), ("visibility", "public")] {
        let mut candidate = record();
        candidate["files"][0][field] = json!(bad);
        write_record(&f.path, &candidate);
        assert!(
            f.factory
                .prepare_terminal_retainer(&task(), &handle(), &completion, &cancellation)
                .await
                .is_err()
        );
    }
    let mut candidate = record();
    candidate["mountRoot"] = json!("/untrusted/model/path");
    write_record(&f.path, &candidate);
    let error = f
        .factory
        .prepare_terminal_retainer(&task(), &handle(), &completion, &cancellation)
        .await
        .unwrap_err();
    assert_eq!(error.to_string(), "terminal_export_record_invalid");
    write_record(&f.path, &record());
    mode(&f.path, 0o644);
    assert!(
        f.factory
            .prepare_terminal_retainer(&task(), &handle(), &completion, &cancellation)
            .await
            .is_err()
    );
    mode(&f.path, 0o600);
    fs::write(&f.output, b"Changed tool output\n").unwrap();
    let retainer = f
        .factory
        .prepare_terminal_retainer(&task(), &handle(), &completion, &cancellation)
        .await
        .unwrap();
    assert!(retainer.retain(&handle(), &cancellation).await.is_err());
    assert!(
        !f.durable
            .join(format!(
                "tasks/{}/attempts/2-7/exports/outputs/result.txt",
                Uuid::nil()
            ))
            .exists()
    );
}

#[tokio::test]
async fn awaits_atomic_publication_and_cancels_missing_records_without_waiting_on_bad_roots() {
    let f = fixture();
    let completion = completion().await;
    let cancellation = CancellationToken::new();
    let task = task();
    let handle = handle();
    let preparation =
        f.factory
            .prepare_terminal_retainer(&task, &handle, &completion, &cancellation);
    tokio::pin!(preparation);
    assert!(
        tokio::time::timeout(Duration::from_millis(30), &mut preparation)
            .await
            .is_err()
    );
    let pending = f.path.with_extension("pending");
    write_record(&pending, &record());
    fs::rename(pending, &f.path).unwrap();
    tokio::time::timeout(Duration::from_secs(2), &mut preparation)
        .await
        .unwrap()
        .unwrap();
    fs::remove_file(&f.path).unwrap();
    let cancelled = CancellationToken::new();
    let operation = f
        .factory
        .prepare_terminal_retainer(&task, &handle, &completion, &cancelled);
    tokio::pin!(operation);
    assert!(
        tokio::time::timeout(Duration::from_millis(30), &mut operation)
            .await
            .is_err()
    );
    cancelled.cancel();
    assert_eq!(
        tokio::time::timeout(Duration::from_secs(1), &mut operation)
            .await
            .unwrap()
            .unwrap_err()
            .to_string(),
        "terminal_export_cancelled"
    );
    fs::remove_dir_all(f.path.parent().unwrap()).unwrap();
    let result = tokio::time::timeout(
        Duration::from_secs(1),
        f.factory
            .prepare_terminal_retainer(&task, &handle, &completion, &cancellation),
    )
    .await
    .unwrap();
    assert_eq!(
        result.unwrap_err().to_string(),
        "terminal_export_record_unavailable"
    );
}
