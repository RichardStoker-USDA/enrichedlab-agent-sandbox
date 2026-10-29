pub use application::custody;
use enrichedlab_agent_sandbox as application;
pub mod broker {
    pub use enrichedlab_agent_sandbox::broker::{model, sandbox};
}

use application::broker::workload_site;

#[path = "../src/broker/input_workload.rs"]
mod input_workload;

use application::broker::{model::ClaimedTask, sandbox::SandboxBinding};
use custody::{CopySealRequest, DeclaredFile};
use input_workload::{
    AuthorizedInputCopy, CustodyPvcAccessMode, InputCopyAuthorization, InputWorkloadConfig,
    InputWorkloadRenderer,
};
use serde_json::json;
use std::{collections::BTreeMap, path::PathBuf};
use uuid::Uuid;

fn task() -> ClaimedTask {
    ClaimedTask {
        id: Uuid::nil(),
        principal_id: Uuid::nil(),
        group_key: "research".into(),
        profile_key: "reference".into(),
        objective: "untrusted task text never reaches the renderer".into(),
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

fn authorization() -> InputCopyAuthorization {
    InputCopyAuthorization {
        task_id: task().id,
        attempt: task().attempt,
        lease_generation: task().lease_generation,
        run_id: "run-20260909-0001".into(),
        tenant_id: "tenant-example-001".into(),
        source_run_name: "source-run-20260909-0001".into(),
        source_pvc_name: "artifacts-source-run-20260909-0001".into(),
        principal_hash: binding().principal_hash,
        profile_id: binding().profile_id,
        namespace: binding().namespace,
        custody_retention_until: "2099-01-01T00:00:00Z".into(),
        input_digest: format!("sha256:{}", "b".repeat(64)),
    }
}

fn request() -> CopySealRequest {
    CopySealRequest {
        schema_version: "sandbox.enrichedlab.org/input-staging/v1".into(),
        staging_id: "stage-20260909-0001".into(),
        run_id: authorization().run_id,
        tenant_id: "tenant-example-001".into(),
        namespace: binding().namespace,
        principal_hash: binding().principal_hash,
        input_ref: "input://001".into(),
        input_digest: authorization().input_digest,
        pvc_name: "input-run-20260909-0001".into(),
        manifest_ref: "artifact://001".into(),
        attestation_ref: "receipt://001".into(),
        source_root: PathBuf::from("/source"),
        destination_root: PathBuf::from("/destination"),
        custody_root: PathBuf::from("/custody"),
        files: vec![DeclaredFile {
            path: "selected/source.txt".into(),
            size_bytes: 9,
            sha256: format!("sha256:{}", "f".repeat(64)),
        }],
        max_files: Some(10),
        max_bytes: Some(1024),
        admitted_files: vec![],
    }
}

fn copy() -> AuthorizedInputCopy {
    AuthorizedInputCopy::new(
        "source-run-20260909-0001".into(),
        "artifacts-source-run-20260909-0001".into(),
        "input-copy-run-20260909-0001".into(),
        request(),
    )
    .unwrap()
}

fn config() -> InputWorkloadConfig {
    InputWorkloadConfig {
        site: Default::default(),
        runtime_class_name: "kata-clh".into(),
        image: "registry.example.invalid/enrichedlab/agent-sandbox@sha256:1111111111111111111111111111111111111111111111111111111111111111".into(),
        pool_selector: BTreeMap::from([("sandbox.enrichedlab.org/pool".into(), "custody".into())]),
        cpu_request_millis: 100,
        cpu_limit_millis: 500,
        memory_request_mib: 128,
        memory_limit_mib: 256,
        active_deadline_seconds: 120,
        custody_memory_mib: 32,
        input_storage_class_name: "trusted-input".into(),
        artifact_storage_class_name: "trusted-artifacts".into(),
        input_storage_mib: 512,
        artifact_storage_mib: 1024,
        custody_pvc_access_mode: CustodyPvcAccessMode::ReadWriteOnce,
        max_request_bytes: 128 * 1024,
    }
}

#[test]
fn renders_restricted_copy_job_with_opaque_request_and_fixed_pvcs() {
    let mut config = config();
    config
        .site
        .labels
        .insert("research.example.org/owner".into(), "science".into());
    config.site.image_pull_secrets.push("registry-pull".into());
    let workload = InputWorkloadRenderer::new(config)
        .unwrap()
        .render(&task(), &binding(), &authorization(), &copy())
        .unwrap();
    assert!(workload.job_name.starts_with("input-copy-"));
    assert_eq!(
        workload.job["metadata"]["labels"]["research.example.org/owner"],
        "science"
    );
    assert_eq!(
        workload.job["spec"]["template"]["metadata"]["labels"]["research.example.org/owner"],
        "science"
    );
    assert_eq!(
        workload.job["spec"]["template"]["spec"]["imagePullSecrets"][0]["name"],
        "registry-pull"
    );
    assert_eq!(workload.job["kind"], "Job");
    assert_eq!(
        workload.job["metadata"]["labels"]["sandbox.enrichedlab.org/principal-hash"],
        binding().principal_hash
    );
    assert_eq!(
        workload.job["metadata"]["labels"]["sandbox.enrichedlab.org/profile"],
        binding().profile_id
    );
    assert_eq!(
        workload.job["metadata"]["annotations"]["sandbox.enrichedlab.org/input-digest"],
        authorization().input_digest
    );
    assert_eq!(
        workload.job["metadata"]["annotations"]["sandbox.enrichedlab.org/input-materialization"],
        "checksummed-copy"
    );
    assert!(
        workload.job["metadata"]["annotations"]
            .get("sandbox.enrichedlab.org/input-staging-digest")
            .is_none()
    );
    assert!(
        workload.job["metadata"]["annotations"]
            .get("sandbox.enrichedlab.org/input-manifest-digest")
            .is_none()
    );
    assert!(
        workload.job["metadata"]["annotations"]
            .get("sandbox.enrichedlab.org/input-attestation-digest")
            .is_none()
    );
    assert_eq!(workload.job["spec"]["backoffLimit"], 0);
    assert_eq!(workload.job["spec"]["activeDeadlineSeconds"], 120);
    assert_eq!(
        workload.job["spec"]["template"]["spec"]["runtimeClassName"],
        "kata-clh"
    );
    assert_eq!(
        workload.job["spec"]["template"]["spec"]["automountServiceAccountToken"],
        false
    );
    assert!(
        workload
            .job
            .pointer("/spec/template/spec/serviceAccountName")
            .is_none()
    );
    assert_eq!(
        workload.job["spec"]["template"]["spec"]["containers"][0]["command"],
        json!(["/usr/local/bin/agent-sandbox-custodian"])
    );
    assert_eq!(
        workload.job["spec"]["template"]["spec"]["containers"][0]["args"],
        json!(["--request", "/request/request.json"])
    );
    assert_eq!(
        workload.job["spec"]["template"]["spec"]["containers"][0]["volumeMounts"],
        json!([
            {"name":"source","mountPath":"/source","readOnly":true},
            {"name":"destination","mountPath":"/destination","readOnly":false},
            {"name":"custody","mountPath":"/custody","readOnly":false},
            {"name":"request","mountPath":"/request","readOnly":true}
        ])
    );
    let pod = &workload.job["spec"]["template"]["spec"];
    assert_eq!(pod["securityContext"]["runAsUser"], 65532);
    assert_eq!(pod["containers"][0]["securityContext"]["runAsUser"], 65532);
    assert_eq!(
        pod["containers"][0]["securityContext"]["capabilities"],
        json!({"drop":["ALL"]})
    );
    assert_eq!(
        pod["volumes"][0],
        json!({"name":"source","persistentVolumeClaim":{"claimName":"artifacts-source-run-20260909-0001","readOnly":true}})
    );
    assert_eq!(
        pod["volumes"][1],
        json!({"name":"destination","persistentVolumeClaim":{"claimName":"input-run-20260909-0001","readOnly":false}})
    );
    assert_eq!(pod["volumes"][2]["emptyDir"]["medium"], "Memory");
    assert_eq!(pod["volumes"][3]["projected"]["defaultMode"], 288);
    assert_eq!(workload.input_pvc["kind"], "PersistentVolumeClaim");
    assert_eq!(
        workload.input_pvc["metadata"]["name"],
        "input-run-20260909-0001"
    );
    assert_eq!(
        workload.input_pvc["spec"]["accessModes"],
        json!(["ReadWriteOnce"])
    );
    assert_eq!(
        workload.input_pvc["spec"]["storageClassName"],
        "trusted-input"
    );
    assert_eq!(
        workload.input_pvc["spec"]["resources"]["requests"]["storage"],
        "512Mi"
    );
    assert_eq!(
        workload.input_pvc["metadata"]["labels"]["sandbox.enrichedlab.org/volume-role"],
        "immutable-input"
    );
    assert_eq!(
        workload.input_pvc["metadata"]["labels"]["sandbox.enrichedlab.org/seal-state"],
        "staging"
    );
    assert_eq!(
        workload.input_pvc["metadata"]["labels"]["sandbox.enrichedlab.org/materialization-mode"],
        "checksummed-copy"
    );
    assert!(
        workload.input_pvc["metadata"]["annotations"]
            .get("sandbox.enrichedlab.org/input-attestation-digest")
            .is_none()
    );
    assert_eq!(
        workload.artifact_pvc["metadata"]["name"],
        "artifacts-run-20260909-0001"
    );
    assert_eq!(
        workload.artifact_pvc["spec"]["accessModes"],
        json!(["ReadWriteOnce"])
    );
    assert_eq!(
        workload.artifact_pvc["spec"]["storageClassName"],
        "trusted-artifacts"
    );
    for key in [
        "input-digest",
        "input-staging-digest",
        "input-manifest-digest",
        "input-attestation-digest",
    ] {
        assert!(
            workload.artifact_pvc["metadata"]["annotations"]
                .get(format!("sandbox.enrichedlab.org/{key}"))
                .is_none()
        );
    }
    assert!(
        workload
            .input_pvc
            .pointer("/metadata/ownerReferences")
            .is_none()
    );
    assert!(
        workload
            .artifact_pvc
            .pointer("/metadata/ownerReferences")
            .is_none()
    );
    assert_eq!(workload.network_policy["spec"]["ingress"], json!([]));
    assert_eq!(workload.network_policy["spec"]["egress"], json!([]));
    let rendered = workload.job.to_string();
    let debug = format!("{workload:?}");
    assert!(!debug.contains("selected/source.txt"));
    assert!(!debug.contains("input://001"));
    assert!(debug.contains("<redacted:"));
    assert!(!rendered.contains("hostPath"));
    assert!(!rendered.contains("ownerReferences"));
    assert!(!rendered.contains("selected/source.txt"));
    let reparsed: CopySealRequest = serde_json::from_slice(workload.request_bytes()).unwrap();
    assert_eq!(reparsed.files, request().files);
    assert_eq!(reparsed.source_root, PathBuf::from("/source"));
}

#[test]
fn custody_pvc_access_mode_defaults_and_renders_only_closed_values() {
    let legacy = serde_json::from_value::<InputWorkloadConfig>(json!({
        "runtimeClassName": "kata-clh",
        "image": "registry.example.invalid/enrichedlab/agent-sandbox@sha256:1111111111111111111111111111111111111111111111111111111111111111",
        "poolSelector": {"sandbox.enrichedlab.org/pool": "custody"},
        "cpuRequestMillis": 100,
        "cpuLimitMillis": 500,
        "memoryRequestMib": 128,
        "memoryLimitMib": 256,
        "activeDeadlineSeconds": 120,
        "custodyMemoryMib": 32,
        "inputStorageClassName": "trusted-input",
        "artifactStorageClassName": "trusted-artifacts",
        "inputStorageMib": 512,
        "artifactStorageMib": 1024,
        "maxRequestBytes": 131072
    }))
    .unwrap();
    assert_eq!(
        legacy.custody_pvc_access_mode,
        CustodyPvcAccessMode::ReadWriteOnce
    );
    let legacy_workload = InputWorkloadRenderer::new(legacy.clone())
        .unwrap()
        .render(&task(), &binding(), &authorization(), &copy())
        .unwrap();
    assert_eq!(
        legacy_workload.input_pvc["spec"]["accessModes"],
        json!(["ReadWriteOnce"])
    );
    assert_eq!(
        legacy_workload.artifact_pvc["spec"]["accessModes"],
        json!(["ReadWriteOnce"])
    );

    let mut isolated = legacy;
    isolated.custody_pvc_access_mode = CustodyPvcAccessMode::ReadWriteOncePod;
    let isolated_workload = InputWorkloadRenderer::new(isolated)
        .unwrap()
        .render(&task(), &binding(), &authorization(), &copy())
        .unwrap();
    assert_eq!(
        isolated_workload.input_pvc["spec"]["accessModes"],
        json!(["ReadWriteOncePod"])
    );
    assert_eq!(
        isolated_workload.artifact_pvc["spec"]["accessModes"],
        json!(["ReadWriteOncePod"])
    );

    let error = serde_json::from_value::<InputWorkloadConfig>(json!({
        "runtimeClassName": "kata-clh",
        "image": "registry.example.invalid/enrichedlab/agent-sandbox@sha256:1111111111111111111111111111111111111111111111111111111111111111",
        "poolSelector": {"sandbox.enrichedlab.org/pool": "custody"},
        "cpuRequestMillis": 100,
        "cpuLimitMillis": 500,
        "memoryRequestMib": 128,
        "memoryLimitMib": 256,
        "activeDeadlineSeconds": 120,
        "custodyMemoryMib": 32,
        "inputStorageClassName": "trusted-input",
        "artifactStorageClassName": "trusted-artifacts",
        "inputStorageMib": 512,
        "artifactStorageMib": 1024,
        "custodyPvcAccessMode": "readWriteMany",
        "maxRequestBytes": 131072
    }))
    .unwrap_err();
    assert!(error.to_string().contains("readWriteMany"));
}

#[test]
fn input_pvc_source_mounts_only_sealed_payload() {
    let mut authorization = authorization();
    authorization.source_pvc_name = "input-source-run-20260909-0001".into();
    let copy = AuthorizedInputCopy::new(
        authorization.source_run_name.clone(),
        authorization.source_pvc_name.clone(),
        "input-copy-run-20260909-0001".into(),
        request(),
    )
    .unwrap();
    let workload = InputWorkloadRenderer::new(config())
        .unwrap()
        .render(&task(), &binding(), &authorization, &copy)
        .unwrap();
    assert_eq!(
        workload.job["spec"]["template"]["spec"]["containers"][0]["volumeMounts"][0],
        json!({"name":"source","mountPath":"/source","subPath":"payload","readOnly":true})
    );
}

#[test]
fn custody_volumes_preserve_approved_retention_without_a_render_clock() {
    let renderer = InputWorkloadRenderer::new(config()).unwrap();
    let mut approved = authorization();
    // The renderer preserves a valid approved timestamp, including its offset.
    // Runtime freshness belongs to the custodian before publication.
    approved.custody_retention_until = "2026-09-15T14:30:00-07:00".into();
    let first = renderer
        .render(&task(), &binding(), &approved, &copy())
        .unwrap();
    let repeated = renderer
        .render(&task(), &binding(), &approved, &copy())
        .unwrap();
    assert_eq!(first.input_pvc, repeated.input_pvc);
    assert_eq!(first.artifact_pvc, repeated.artifact_pvc);
    for pvc in [&first.input_pvc, &first.artifact_pvc] {
        assert_eq!(
            pvc["metadata"]["labels"]["sandbox.enrichedlab.org/custody-owner"],
            "durable-task-custody"
        );
        assert_eq!(
            pvc["metadata"]["annotations"]["sandbox.enrichedlab.org/retention-owner"],
            "durable-task-custody"
        );
        assert_eq!(
            pvc["metadata"]["annotations"]["sandbox.enrichedlab.org/retention-until"],
            approved.custody_retention_until
        );
        assert!(pvc["metadata"]["ownerReferences"].is_null());
    }
    assert_eq!(
        first.input_pvc["metadata"]["annotations"]["sandbox.enrichedlab.org/input-digest"],
        approved.input_digest
    );
    assert!(
        first.artifact_pvc["metadata"]["annotations"]["sandbox.enrichedlab.org/input-digest"]
            .is_null()
    );
    for invalid in ["", "2026-02-30T00:00:00Z", "2026-09-15", "unapproved"] {
        approved.custody_retention_until = invalid.into();
        let result = renderer.render(&task(), &binding(), &approved, &copy());
        assert!(
            matches!(result, Err(error) if error.to_string() == "input_copy_retention_invalid")
        );
    }
}

#[test]
fn rejects_untrusted_copy_roots_aliases_and_oversized_private_request() {
    let renderer = InputWorkloadRenderer::new(config()).unwrap();
    assert!(
        AuthorizedInputCopy::new(
            "source-run-20260909-0001".into(),
            "other-pvc".into(),
            "input-copy-run-20260909-0001".into(),
            request()
        )
        .is_err()
    );
    let mut wrong_roots = request();
    wrong_roots.source_root = PathBuf::from("/model-selected");
    let wrong_roots = AuthorizedInputCopy::new(
        "source-run-20260909-0001".into(),
        "artifacts-source-run-20260909-0001".into(),
        "input-copy-run-20260909-0001".into(),
        wrong_roots,
    )
    .unwrap();
    assert!(
        renderer
            .render(&task(), &binding(), &authorization(), &wrong_roots)
            .is_err()
    );
    let mut oversized = request();
    oversized.files[0].path = "x".repeat(200 * 1024);
    let oversized = AuthorizedInputCopy::new(
        "source-run-20260909-0001".into(),
        "artifacts-source-run-20260909-0001".into(),
        "input-copy-run-20260909-0001".into(),
        oversized,
    )
    .unwrap();
    assert!(
        renderer
            .render(&task(), &binding(), &authorization(), &oversized)
            .is_err()
    );
    let source_is_destination = AuthorizedInputCopy::new(
        authorization().run_id.clone(),
        "input-run-20260909-0001".into(),
        "input-copy-run-20260909-0001".into(),
        request(),
    )
    .unwrap();
    assert!(
        renderer
            .render(
                &task(),
                &binding(),
                &authorization(),
                &source_is_destination
            )
            .is_err()
    );
    let mut wrong_lease = authorization();
    wrong_lease.lease_generation += 1;
    assert!(
        renderer
            .render(&task(), &binding(), &wrong_lease, &copy())
            .is_err()
    );
    let mut unprefixed_digest = request();
    unprefixed_digest.files[0].sha256 = "f".repeat(64);
    let unprefixed_digest = AuthorizedInputCopy::new(
        "source-run-20260909-0001".into(),
        "artifacts-source-run-20260909-0001".into(),
        "input-copy-run-20260909-0001".into(),
        unprefixed_digest,
    )
    .unwrap();
    assert!(
        renderer
            .render(&task(), &binding(), &authorization(), &unprefixed_digest)
            .is_err()
    );
}

#[test]
fn admitted_code_travels_only_in_the_private_copy_request() {
    use sha2::{Digest, Sha256};
    const BLOCK: &str = "analyze <- function(data) list()\n";
    let mut declared = request();
    declared.files.push(DeclaredFile {
        path: "generated/block.R".into(),
        size_bytes: BLOCK.len() as u64,
        sha256: format!("sha256:{}", hex::encode(Sha256::digest(BLOCK))),
    });
    let authorized = |request: CopySealRequest| {
        AuthorizedInputCopy::new(
            "source-run-20260909-0001".into(),
            "artifacts-source-run-20260909-0001".into(),
            "copy-request-20260909-0001".into(),
            request,
        )
    };
    // An approved plan never carries the bytes; only the custodian attaches them.
    let mut with_bytes = declared.clone();
    with_bytes.admitted_files.push(custody::AdmittedFile {
        path: "generated/block.R".into(),
        content: BLOCK.into(),
    });
    assert_eq!(
        authorized(with_bytes).unwrap_err().to_string(),
        "input_copy_plan_admitted_bytes_forbidden"
    );
    assert_eq!(
        authorized(declared.clone())
            .unwrap()
            .with_admitted_file("generated/block.R".into(), "different\n".into())
            .unwrap_err()
            .to_string(),
        "input_copy_request_admitted_invalid"
    );
    let generated = authorized(declared)
        .unwrap()
        .with_admitted_file("generated/block.R".into(), BLOCK.into())
        .unwrap();
    let renderer = InputWorkloadRenderer::new(config()).unwrap();
    let workload = renderer
        .render(&task(), &binding(), &authorization(), &generated)
        .unwrap();
    let sent: serde_json::Value = serde_json::from_slice(workload.request_bytes()).unwrap();
    assert_eq!(sent["admittedFiles"][0]["content"], BLOCK);
    // The Job manifest and its arguments never carry the code.
    assert!(
        !serde_json::to_string(&workload.job)
            .unwrap()
            .contains("analyze <-")
    );
    let fixed = renderer
        .render(&task(), &binding(), &authorization(), &copy())
        .unwrap();
    let sent: serde_json::Value = serde_json::from_slice(fixed.request_bytes()).unwrap();
    assert!(sent.get("admittedFiles").is_none());
}
