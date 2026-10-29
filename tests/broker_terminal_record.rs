#![cfg(unix)]

use anyhow::{Result, bail};
use async_trait::async_trait;
use enrichedlab_agent_sandbox::{
    broker::{
        admission,
        artifact_retainer::ExportLimits,
        execution::TerminalArtifactRetainerFactory,
        model::ClaimedTask,
        sandbox::{
            ArtifactRetainer, RuntimeCompletion, SandboxBinding, SandboxBindingCatalog,
            SandboxHandle, SandboxRuntime, SandboxTransport,
        },
        terminal_exports::{
            StagedTerminalRetainerFactory, TerminalExportConfig, encode_terminal_export,
        },
    },
    output_inventory::{OutputInventoryRequest, REQUEST_SCHEMA, SelectedOutput, inventory},
};
use serde_json::{Value, json};
use std::{
    fs,
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
};
use tempfile::TempDir;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

const RUN: &str = "run-20260909-0001";
const OUTPUT: &[u8] = b"synthetic terminal output\n";

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
        lease_generation: task().lease_generation,
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

fn private(path: &Path) {
    fs::set_permissions(path, fs::Permissions::from_mode(0o700)).unwrap();
}

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

struct Fixture {
    _temp: TempDir,
    records: PathBuf,
    mounted: PathBuf,
    durable: PathBuf,
}

fn fixture() -> Fixture {
    let temp = tempfile::Builder::new()
        .prefix("enrichedlab-terminal-record-")
        .tempdir_in("/tmp")
        .unwrap();
    let root = temp.path().canonicalize().unwrap();
    let records = root.join("records");
    let mounted = root.join("mounted");
    let durable = root.join("durable");
    for directory in [&records, &mounted, &durable] {
        fs::create_dir(directory).unwrap();
        private(directory);
    }
    let admission = records.join("admission.json");
    fs::write(
        &admission,
        include_bytes!("fixtures/admission/staged_admission.json"),
    )
    .unwrap();
    fs::set_permissions(&admission, fs::Permissions::from_mode(0o600)).unwrap();
    let output = mounted.join(RUN).join("outputs/result.txt");
    fs::create_dir_all(output.parent().unwrap()).unwrap();
    fs::write(output, OUTPUT).unwrap();
    Fixture {
        _temp: temp,
        records,
        mounted,
        durable,
    }
}

#[tokio::test]
async fn encodes_measured_selection_for_existing_terminal_reader_and_rejects_mismatches() {
    let fixture = fixture();
    let approved = OutputInventoryRequest {
        schema_version: REQUEST_SCHEMA.into(),
        mounted_root: fixture.mounted.join(RUN),
        files: vec![SelectedOutput {
            relative_path: "outputs/result.txt".into(),
            visibility: "researcher".into(),
            role: "result".into(),
            label: "Synthetic terminal output".into(),
            media_type: "text/plain".into(),
        }],
        max_files: 1,
        max_bytes: 1024,
    };
    let cancel = CancellationToken::new();
    let measured = inventory(&approved, &cancel).unwrap();
    let completion = completion().await;
    let bytes = encode_terminal_export(
        &task(),
        &binding(),
        &handle(),
        &completion,
        &approved,
        &measured,
    )
    .unwrap();

    let record = fixture.records.join(format!("{}-2-7.json", Uuid::nil()));
    fs::write(&record, &bytes).unwrap();
    fs::set_permissions(&record, fs::Permissions::from_mode(0o600)).unwrap();
    let admitted = admission::load(
        &fixture.records,
        "admission.json",
        uid(),
        &task(),
        &binding(),
    )
    .unwrap();
    let retainer = StagedTerminalRetainerFactory::new(
        TerminalExportConfig::Mounted {
            record_root: fixture.records.clone(),
            writer_uid: uid(),
            mounted_artifacts_root: fixture.mounted.clone(),
        },
        &task(),
        &binding(),
        &admitted,
        fixture.durable.clone(),
        ExportLimits {
            max_files: 1,
            max_bytes: 1024,
        },
    )
    .unwrap()
    .prepare_terminal_retainer(&task(), &handle(), &completion, &cancel)
    .await
    .unwrap();
    let retained = retainer.retain(&handle(), &cancel).await.unwrap();
    assert_eq!(retained.len(), 1);
    assert_eq!(
        fs::read(fixture.durable.join(&retained[0].relative_path)).unwrap(),
        OUTPUT
    );

    let mut wrong_digest = measured.clone();
    wrong_digest.selection_digest =
        "sha256:0000000000000000000000000000000000000000000000000000000000000000".into();
    assert_eq!(
        encode_terminal_export(
            &task(),
            &binding(),
            &handle(),
            &completion,
            &approved,
            &wrong_digest,
        )
        .unwrap_err()
        .to_string(),
        "output_inventory_selection_mismatch"
    );
    let mut wrong_task = task();
    wrong_task.lease_generation += 1;
    assert_eq!(
        encode_terminal_export(
            &wrong_task,
            &binding(),
            &handle(),
            &completion,
            &approved,
            &measured,
        )
        .unwrap_err()
        .to_string(),
        "terminal_export_binding_mismatch"
    );
}
