#![cfg(unix)]
use enrichedlab_agent_sandbox::broker::{
    artifact_retainer::{ExportFile, ExportLimits},
    export_transport::{
        ExportClient, ExportScope, ExportServer, ExportServerConfig, HttpsArtifactRetainer,
        RemoteExportConfig,
    },
    model::ClaimedTask,
    sandbox::{ArtifactRetainer, SandboxBinding, SandboxHandle},
};
use rcgen::{
    BasicConstraints, CertificateParams, ExtendedKeyUsagePurpose, IsCa, Issuer, KeyPair,
    KeyUsagePurpose,
};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    fs,
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    time::Duration,
};
use tempfile::TempDir;
use tokio::net::TcpListener;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;
const RUN: &str = "run-20260909-0001";
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

fn private(path: &Path, bytes: &[u8]) {
    fs::write(path, bytes).unwrap();
    fs::set_permissions(path, fs::Permissions::from_mode(0o600)).unwrap();
}
fn private_dir(path: &Path) {
    fs::create_dir_all(path).unwrap();
    fs::set_permissions(path, fs::Permissions::from_mode(0o700)).unwrap();
}
fn uid() -> u32 {
    unsafe { libc::geteuid() }
}
fn scope() -> ExportScope {
    ExportScope::new(task().id, task().attempt, task().lease_generation).unwrap()
}

struct Fixture {
    _temp: TempDir,
    root: PathBuf,
    original: Vec<u8>,
    manifest: Vec<u8>,
    fingerprint: String,
}
impl Fixture {
    fn new() -> Self {
        let temp = tempfile::Builder::new()
            .prefix("enrichedlab-export-tls-")
            .tempdir_in("/tmp")
            .unwrap();
        let root = fs::canonicalize(temp.path()).unwrap();
        for directory in [
            "server",
            "client",
            "other_client",
            "records",
            "output/outputs",
            "durable",
        ] {
            private_dir(&root.join(directory));
        }
        let original: Vec<u8> = (0..200_000).map(|i| (i % 251) as u8).collect();
        private(&root.join("output/outputs/result.bin"), &original);
        let mut manifest: Value =
            serde_json::from_str(include_str!("../examples/terminal_export.json")).unwrap();
        manifest["files"][0]["relativePath"] = json!("outputs/result.bin");
        manifest["files"][0]["sizeBytes"] = json!(original.len());
        manifest["files"][0]["sha256"] = json!(hex::encode(Sha256::digest(&original)));
        let manifest = serde_json::to_vec(&manifest).unwrap();
        private(&root.join("records/terminal.json"), &manifest);
        let mut params = CertificateParams::new(Vec::<String>::new()).unwrap();
        params.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
        params.key_usages = vec![
            KeyUsagePurpose::DigitalSignature,
            KeyUsagePurpose::KeyCertSign,
            KeyUsagePurpose::CrlSign,
        ];
        let key = KeyPair::generate().unwrap();
        let certificate = params.self_signed(&key).unwrap();
        let issuer = Issuer::from_params(&params, &key);
        let mut fingerprint = String::new();
        for name in ["server", "client", "other_client"] {
            private(
                &root.join(name).join("ca.pem"),
                certificate.pem().as_bytes(),
            );
            let mut params = CertificateParams::new(if name == "server" {
                vec!["127.0.0.1".into()]
            } else {
                Vec::<String>::new()
            })
            .unwrap();
            params.extended_key_usages = vec![if name == "server" {
                ExtendedKeyUsagePurpose::ServerAuth
            } else {
                ExtendedKeyUsagePurpose::ClientAuth
            }];
            params.key_usages = vec![KeyUsagePurpose::DigitalSignature];
            let key = KeyPair::generate().unwrap();
            let cert = params.signed_by(&key, &issuer).unwrap();
            if name == "server" {
                private(&root.join(name).join("server.pem"), cert.pem().as_bytes());
                private(
                    &root.join(name).join("server.key"),
                    key.serialize_pem().as_bytes(),
                );
            } else {
                private(
                    &root.join(name).join("client.pem"),
                    format!("{}{}", cert.pem(), key.serialize_pem()).as_bytes(),
                );
            }
            if name == "client" {
                fingerprint = hex::encode(Sha256::digest(cert.der()));
            }
        }
        Self {
            _temp: temp,
            root,
            original,
            manifest,
            fingerprint,
        }
    }
    fn server(&self) -> ExportServer {
        ExportServer::new(ExportServerConfig {
            schema_version: "sandbox.enrichedlab.org/export-server/v1".into(),
            bind: "127.0.0.1:0".parse().unwrap(),
            record_path: self.root.join("records/terminal.json"),
            artifacts_root: self.root.join("output"),
            credential_root: self.root.join("server"),
            writer_uid: uid(),
            client_sha256: self.fingerprint.clone(),
            max_files: 8,
            max_bytes: 4 * 1024 * 1024,
            max_seconds: 30,
        })
        .unwrap()
    }
    fn client(&self, origin: reqwest::Url, name: &str, selected: ExportScope) -> ExportClient {
        ExportClient::new(origin, &self.root.join(name), uid(), selected).unwrap()
    }
    fn raw_client(&self, name: Option<&str>) -> reqwest::Client {
        let mut client = reqwest::Client::builder()
            .no_proxy()
            .redirect(reqwest::redirect::Policy::none())
            .timeout(Duration::from_secs(3))
            .tls_certs_only(
                reqwest::Certificate::from_pem_bundle(
                    &fs::read(self.root.join("client/ca.pem")).unwrap(),
                )
                .unwrap(),
            );
        if let Some(name) = name {
            client = client.identity(
                reqwest::Identity::from_pem(
                    &fs::read(self.root.join(name).join("client.pem")).unwrap(),
                )
                .unwrap(),
            );
        }
        client.build().unwrap()
    }
    fn selected(&self) -> Vec<ExportFile> {
        vec![ExportFile {
            relative_path: "outputs/result.bin".into(),
            visibility: "researcher".into(),
            role: "result".into(),
            label: "Tool result".into(),
            media_type: "application/octet-stream".into(),
            size_bytes: self.original.len() as u64,
            sha256: hex::encode(Sha256::digest(&self.original)),
        }]
    }
    fn retainer(&self, client: ExportClient, manifest: &[u8]) -> HttpsArtifactRetainer {
        HttpsArtifactRetainer::for_task(
            client,
            manifest,
            &task(),
            &binding(),
            &handle(),
            self.root.join("durable"),
            self.selected(),
            ExportLimits {
                max_files: 8,
                max_bytes: 4 * 1024 * 1024,
            },
        )
        .unwrap()
    }
    fn target(&self) -> PathBuf {
        self.root.join(format!(
            "durable/tasks/{}/attempts/2-7/exports/outputs/result.bin",
            Uuid::nil()
        ))
    }
}

#[tokio::test]
async fn private_tls_export_retains_original_bytes_and_rejects_foreign_clients_and_runs() {
    let f = Fixture::new();
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let origin: reqwest::Url = format!("https://{}/", listener.local_addr().unwrap())
        .parse()
        .unwrap();
    let cancel = CancellationToken::new();
    let serving = tokio::spawn(f.server().serve_listener(listener, cancel.clone()));
    let client = f.client(origin.clone(), "client", scope());
    let manifest = client
        .manifest(&CancellationToken::new())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(manifest, f.manifest);
    let retained = f
        .retainer(client, &manifest)
        .retain(&handle(), &CancellationToken::new())
        .await
        .unwrap();
    assert_eq!(retained.len(), 1);
    assert_eq!(fs::read(f.target()).unwrap(), f.original);
    assert_eq!(retained[0].sha256, hex::encode(Sha256::digest(&f.original)));
    let base = format!("{}v1/exports/{}/2/7", origin, Uuid::nil());
    for name in [None, Some("other_client")] {
        assert!(
            f.raw_client(name)
                .get(format!("{base}/manifest"))
                .send()
                .await
                .is_err()
        );
    }
    let client = f.raw_client(Some("client"));
    assert_eq!(
        client
            .get(format!("{}v1/exports/{}/2/8/manifest", origin, Uuid::nil()))
            .send()
            .await
            .unwrap()
            .status(),
        404
    );
    assert_eq!(
        client
            .get(format!("{base}/files/0"))
            .send()
            .await
            .unwrap()
            .status(),
        412
    );
    assert_eq!(
        client
            .get(format!("{base}/files/100"))
            .header(
                "if-match",
                format!("\"{}\"", hex::encode(Sha256::digest(&manifest)))
            )
            .send()
            .await
            .unwrap()
            .status(),
        404
    );
    assert_eq!(
        client
            .post(format!("{base}/manifest"))
            .send()
            .await
            .unwrap()
            .status(),
        405
    );
    let other_trust = Fixture::new();
    private(
        &f.root.join("client/ca.pem"),
        &fs::read(other_trust.root.join("client/ca.pem")).unwrap(),
    );
    let untrusted = f
        .client(origin.clone(), "client", scope())
        .manifest(&CancellationToken::new())
        .await;
    assert!(untrusted.is_err() || untrusted.unwrap().is_none());
    private(
        &f.root.join("client/ca.pem"),
        &fs::read(f.root.join("server/ca.pem")).unwrap(),
    );
    let foreign = ExportScope::new(Uuid::nil(), 2, 8).unwrap();
    assert!(
        HttpsArtifactRetainer::for_task(
            f.client(origin, "client", foreign),
            &manifest,
            &task(),
            &binding(),
            &handle(),
            f.root.join("durable"),
            f.selected(),
            ExportLimits {
                max_files: 8,
                max_bytes: 4 * 1024 * 1024
            }
        )
        .is_err()
    );
    cancel.cancel();
    tokio::time::timeout(Duration::from_secs(2), serving)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
}

#[tokio::test]
async fn changed_bytes_manifest_and_cancellation_cannot_publish_a_network_result() {
    let f = Fixture::new();
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let origin: reqwest::Url = format!("https://{}/", listener.local_addr().unwrap())
        .parse()
        .unwrap();
    let cancel = CancellationToken::new();
    let serving = tokio::spawn(f.server().serve_listener(listener, cancel.clone()));
    let client = f.client(origin, "client", scope());
    let manifest = client
        .manifest(&CancellationToken::new())
        .await
        .unwrap()
        .unwrap();
    let mut altered = f.original.clone();
    altered[0] ^= 1;
    private(&f.root.join("output/outputs/result.bin"), &altered);
    assert!(
        f.retainer(client.clone(), &manifest)
            .retain(&handle(), &CancellationToken::new())
            .await
            .is_err()
    );
    assert!(!f.target().exists());
    private(&f.root.join("output/outputs/result.bin"), &f.original);
    assert!(
        f.retainer(client.clone(), b"changed manifest")
            .retain(&handle(), &CancellationToken::new())
            .await
            .is_err()
    );
    let stopped = CancellationToken::new();
    stopped.cancel();
    assert!(
        f.retainer(client, &manifest)
            .retain(&handle(), &stopped)
            .await
            .is_err()
    );
    assert!(!f.target().exists());
    assert!(
        walkdir::WalkDir::new(f.root.join("durable"))
            .into_iter()
            .flatten()
            .all(|entry| !entry.file_name().to_string_lossy().ends_with(".partial"))
    );
    cancel.cancel();
    tokio::time::timeout(Duration::from_secs(2), serving)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
}

#[test]
fn remote_configuration_is_operator_bound_and_legacy_configuration_remains_explicit() {
    use enrichedlab_agent_sandbox::broker::terminal_exports::TerminalExportConfig;
    let https = json!({"https":{"dnsSuffix":"svc.cluster.local", "port":8443,"credentialRoot":"/run/export-clients","writerUid":10001}});
    serde_json::from_value::<TerminalExportConfig>(https.clone())
        .unwrap()
        .validate()
        .unwrap();
    for (field, value) in [
        ("dnsSuffix", json!("untrusted.example/path")),
        ("port", json!(0)),
        ("credentialRoot", json!("/run/../secrets")),
    ] {
        let mut bad = https.clone();
        bad["https"][field] = value;
        assert!(
            serde_json::from_value::<TerminalExportConfig>(bad)
                .unwrap()
                .validate()
                .is_err()
        );
    }
    let mut mixed = https;
    mixed["recordRoot"] = json!("/run/records");
    assert!(serde_json::from_value::<TerminalExportConfig>(mixed).is_err());
    let name = scope().service_name(&binding().namespace).unwrap();
    assert!(name.len() <= 63);
    assert_ne!(
        name,
        ExportScope::new(Uuid::nil(), 2, 8)
            .unwrap()
            .service_name(&binding().namespace)
            .unwrap()
    );
    let config = RemoteExportConfig {
        dns_suffix: "svc.cluster.local".into(),
        port: 8443,
        credential_root: "/run/export-clients".into(),
        writer_uid: uid(),
    };
    assert!(ExportClient::for_task(&config, &task(), "../../outside").is_err());
}
