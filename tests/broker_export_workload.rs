use enrichedlab_agent_sandbox::broker::{
    export_workload::{ExportIdentity, ExportWorkloadConfig, ExportWorkloadRenderer},
    model::ClaimedTask,
    sandbox::{SandboxBinding, SandboxHandle},
};
use serde_json::json;
use std::{
    collections::BTreeMap,
    fs,
    os::unix::fs::{MetadataExt, PermissionsExt},
    process::Command,
};
use tempfile::TempDir;
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
fn handle() -> SandboxHandle {
    SandboxHandle {
        task_id: task().id,
        worker_id: "worker-example".into(),
        lease_generation: 7,
        namespace: binding().namespace,
        sandbox_name: "run-20260909-0001".into(),
        sandbox_uid: Uuid::nil().to_string(),
        sandbox_generation: 3,
        lab_ref: binding().lab_ref,
    }
}
fn config() -> ExportWorkloadConfig {
    ExportWorkloadConfig {
        site: Default::default(),
        runtime_class_name: "kata-clh".into(),
        image: "registry.example.invalid/enrichedlab/agent-sandbox@sha256:1111111111111111111111111111111111111111111111111111111111111111".into(),
        broker_namespace: "broker-system".into(),
        dns_suffix: "svc.cluster.test".into(),
        pool_selector: BTreeMap::from([("sandbox.enrichedlab.org/pool".into(), "export".into())]),
        cpu_request_millis: 100,
        cpu_limit_millis: 500,
        memory_request_mib: 128,
        memory_limit_mib: 256,
        active_deadline_seconds: 120,
        max_files: 100,
        max_bytes: 100 * 1024 * 1024,
        max_seconds: 90,
        identity_memory_mib: 32,
    }
}
fn identity() -> ExportIdentity {
    ExportIdentity {
        secret_name: "terminal-export-run-20260909-0001".into(),
        client_sha256: "a".repeat(64),
    }
}

#[test]
fn renders_only_trusted_identity_output_mount_and_broker_ingress() {
    let mut config = config();
    config
        .site
        .labels
        .insert("research.example.org/owner".into(), "science".into());
    config.site.image_pull_secrets.push("registry-pull".into());
    let rendered = ExportWorkloadRenderer::new(config)
        .unwrap()
        .render(
            &task(),
            &binding(),
            &handle(),
            "run-20260909-0001",
            &identity(),
        )
        .unwrap();
    assert_eq!(
        rendered.pod["metadata"]["labels"]["research.example.org/owner"],
        "science"
    );
    assert_eq!(
        rendered.service["metadata"]["labels"]["research.example.org/owner"],
        "science"
    );
    assert_eq!(
        rendered.pod["spec"]["imagePullSecrets"][0]["name"],
        "registry-pull"
    );
    assert!(rendered.service_name.starts_with("export-"));
    assert_eq!(rendered.service["spec"]["type"], "ClusterIP");
    assert_eq!(rendered.service["spec"]["ports"][0]["port"], 8443);
    assert_eq!(
        rendered.client_metadata["dnsName"],
        format!(
            "{}.sandbox-example-001.svc.cluster.test",
            rendered.service_name
        )
    );
    assert_eq!(rendered.client_metadata["port"], 8443);
    assert_eq!(rendered.pod["spec"]["runtimeClassName"], "kata-clh");
    assert_eq!(rendered.pod["spec"]["activeDeadlineSeconds"], 120);
    assert_eq!(rendered.pod["spec"]["automountServiceAccountToken"], false);
    assert!(rendered.pod.pointer("/spec/serviceAccountName").is_none());
    assert_eq!(
        rendered.pod["spec"]["containers"][0]["command"],
        json!(["/usr/local/bin/agent-sandbox-exporter"])
    );
    assert_eq!(
        rendered.pod["spec"]["containers"][0]["args"],
        json!([
            "--config",
            "/runtime/identity/config.json",
            "--writer-uid",
            "65532"
        ])
    );
    assert_eq!(
        rendered.pod["spec"]["containers"][0]["volumeMounts"],
        json!([
            {"name":"staging-private","mountPath":"/runtime","readOnly":true},
            {"name":"artifacts","mountPath":"/artifacts","readOnly":true}
        ])
    );
    assert_eq!(
        rendered.pod["spec"]["initContainers"][0]["volumeMounts"],
        json!([
            {"name":"identity-projected","mountPath":"/projected","readOnly":true},
            {"name":"staging-private","mountPath":"/runtime"}
        ])
    );
    assert_eq!(
        rendered.pod["spec"]["initContainers"][0]["securityContext"]["runAsUser"],
        65532
    );
    assert_eq!(
        rendered.pod["spec"]["containers"][0]["securityContext"]["runAsUser"],
        65532
    );
    assert_eq!(
        rendered.pod["spec"]["volumes"][0]["projected"]["defaultMode"],
        288
    );
    assert_eq!(
        rendered.pod["spec"]["containers"][0]["volumeMounts"][0],
        json!({"name":"staging-private","mountPath":"/runtime","readOnly":true})
    );
    assert_eq!(
        rendered.pod["spec"]["initContainers"][0]["securityContext"]["runAsNonRoot"],
        true
    );
    assert_eq!(
        rendered.pod["spec"]["initContainers"][0]["securityContext"]["capabilities"],
        json!({"drop":["ALL"]})
    );
    let copy = rendered.pod["spec"]["initContainers"][0]["args"][0]
        .as_str()
        .unwrap();
    assert!(
        !copy.contains("chown")
            && !copy.contains("chmod")
            && !copy.contains(" -o ")
            && !copy.contains(" -g ")
    );
    assert_eq!(
        rendered.pod["spec"]["volumes"][2],
        json!({"name":"artifacts","persistentVolumeClaim":{"claimName":"artifacts-run-20260909-0001","readOnly":true}})
    );
    assert!(
        rendered
            .pod
            .to_string()
            .contains("terminal-export-run-20260909-0001")
    );
    assert!(!rendered.pod.to_string().contains("hostPath"));
    assert!(!rendered.pod.to_string().contains("ownerReferences"));
    assert_eq!(
        rendered.exporter_config["recordPath"],
        "/runtime/identity/terminal.json"
    );
    assert_eq!(
        rendered.exporter_config["credentialRoot"],
        "/runtime/identity"
    );
    assert_eq!(rendered.exporter_config["artifactsRoot"], "/artifacts");
    assert_eq!(rendered.exporter_config["clientSha256"], "a".repeat(64));
    assert_eq!(rendered.network_policy["spec"]["egress"], json!([]));
    assert_eq!(
        rendered.network_policy["spec"]["ingress"][0]["ports"][0]["port"],
        8443
    );
    assert_eq!(
        rendered.network_policy["spec"]["ingress"][0]["from"][0]["namespaceSelector"]["matchLabels"]
            ["kubernetes.io/metadata.name"],
        "broker-system"
    );
    assert_eq!(
        rendered.network_policy["spec"]["ingress"][0]["from"][0]["podSelector"]["matchLabels"],
        json!({"app.kubernetes.io/name":"enrichedlab-agent-broker","app.kubernetes.io/component":"worker"})
    );
}

#[test]
fn nonroot_identity_copy_creates_private_subpath() {
    let temp = TempDir::new().unwrap();
    let projected = temp.path().join("projected");
    let staging = temp.path().join("staging");
    fs::create_dir(&projected).unwrap();
    fs::create_dir(&staging).unwrap();
    fs::set_permissions(&staging, fs::Permissions::from_mode(0o770)).unwrap();
    for name in [
        "config.json",
        "terminal.json",
        "ca.pem",
        "server.pem",
        "server.key",
    ] {
        let path = projected.join(name);
        fs::write(&path, b"synthetic").unwrap();
        fs::set_permissions(path, fs::Permissions::from_mode(0o440)).unwrap();
    }
    let rendered = ExportWorkloadRenderer::new(config())
        .unwrap()
        .render(
            &task(),
            &binding(),
            &handle(),
            "run-20260909-0001",
            &identity(),
        )
        .unwrap();
    let copy = rendered.pod["spec"]["initContainers"][0]["args"][0]
        .as_str()
        .unwrap();
    assert!(
        Command::new("/bin/sh")
            .args(["-ec", copy])
            .env("PROJECTED_ROOT", &projected)
            .env("IDENTITY_ROOT", &staging)
            .status()
            .unwrap()
            .success()
    );
    let private = staging.join("identity");
    assert_eq!(fs::metadata(&private).unwrap().mode() & 0o777, 0o700);
    assert_eq!(fs::metadata(&private).unwrap().uid(), unsafe {
        libc::geteuid()
    });
    for name in [
        "config.json",
        "terminal.json",
        "ca.pem",
        "server.pem",
        "server.key",
    ] {
        let metadata = fs::metadata(private.join(name)).unwrap();
        assert_eq!(metadata.mode() & 0o777, 0o600, "{name}");
        assert_eq!(metadata.uid(), unsafe { libc::geteuid() }, "{name}");
    }
}

#[test]
fn rejects_unpinned_or_mismatched_trusted_inputs() {
    let mut bad = config();
    bad.image = "registry.example.invalid/exporter:latest".into();
    assert!(ExportWorkloadRenderer::new(bad).is_err());
    let mut bad = config();
    bad.pool_selector.clear();
    assert!(ExportWorkloadRenderer::new(bad).is_err());
    let mut bad = config();
    bad.max_seconds = 121;
    assert!(ExportWorkloadRenderer::new(bad).is_err());
    let renderer = ExportWorkloadRenderer::new(config()).unwrap();
    let mut wrong = handle();
    wrong.lease_generation += 1;
    assert!(
        renderer
            .render(
                &task(),
                &binding(),
                &wrong,
                "run-20260909-0001",
                &identity()
            )
            .is_err()
    );
    let mut wrong = identity();
    wrong.secret_name = "../not-a-secret".into();
    assert!(
        renderer
            .render(&task(), &binding(), &handle(), "run-20260909-0001", &wrong)
            .is_err()
    );
    assert!(
        renderer
            .render(&task(), &binding(), &handle(), "other-run", &identity())
            .is_err()
    );
}
