#![cfg(unix)]
use anyhow::{Result, bail};
use async_trait::async_trait;
use axum::{
    Json, Router,
    extract::State,
    http::{HeaderMap, StatusCode, Uri},
    response::IntoResponse,
    routing::post,
};
use enrichedlab_agent_sandbox::broker::{
    export_identity::{ExportCredentialLifetime, issue_export_credentials},
    export_kubernetes::ExportKubernetesClient,
    export_publication::{ExportPublicationConfig, ObservedLeaseGuard, PreparedExportPublication},
    export_transport::ExportScope,
    model::ClaimedTask,
    sandbox::{
        BrokerLeaseFence, RetainedArtifact, RuntimeCompletion, SandboxBinding,
        SandboxBindingCatalog, SandboxHandle, SandboxRuntime, SandboxTransport,
    },
};
use rcgen::{CertifiedKey, generate_simple_self_signed};
use rustls::{
    ServerConfig,
    pki_types::{CertificateDer, PrivatePkcs8KeyDer},
};
use serde_json::{Value, json};
use std::{
    fs,
    net::SocketAddr,
    os::unix::fs::PermissionsExt,
    path::PathBuf,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};
use tempfile::TempDir;
use tokio::net::{TcpListener, TcpStream};
use tokio_rustls::{TlsAcceptor, server::TlsStream};
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

struct Fixture {
    _temp: TempDir,
    root: PathBuf,
    config: ExportPublicationConfig,
}
impl Fixture {
    fn new() -> Self {
        let temp = tempfile::Builder::new()
            .prefix("enrichedlab-publication-")
            .tempdir_in("/tmp")
            .unwrap();
        let root = fs::canonicalize(temp.path()).unwrap();
        for name in ["clients", "journal", "api"] {
            fs::create_dir(root.join(name)).unwrap();
            fs::set_permissions(root.join(name), fs::Permissions::from_mode(0o700)).unwrap();
        }
        let config = serde_json::from_value(json!({
            "remote": {"dnsSuffix":"svc.cluster.test", "port":8443, "credentialRoot":root.join("clients"), "writerUid":unsafe {libc::geteuid()}},
            "journalRoot":root.join("journal"),
            "workload": {"runtimeClassName":"kata-clh", "image":format!("registry.example/controller@sha256:{}", "a".repeat(64)),
                "brokerNamespace":"broker-example", "dnsSuffix":"svc.cluster.test", "poolSelector":{"example.org/pool":"sandbox"},
                "cpuRequestMillis":100,"cpuLimitMillis":500,"memoryRequestMib":128,"memoryLimitMib":256,
                "activeDeadlineSeconds":120,"maxFiles":20,"maxBytes":10000,"maxSeconds":60,"identityMemoryMib":4}
        })).unwrap();
        Self {
            _temp: temp,
            root,
            config,
        }
    }
    fn record(&self) -> Vec<u8> {
        include_bytes!("../examples/terminal_export.json").to_vec()
    }
    async fn prepared(&self) -> PreparedExportPublication {
        let credentials = issue_export_credentials(
            &self.config.remote,
            ExportScope::new(task().id, 2, 7).unwrap(),
            &binding().namespace,
            ExportCredentialLifetime {
                task_seconds: 300,
                export_seconds: 60,
            },
        )
        .unwrap();
        PreparedExportPublication::prepare(
            self.config.clone(),
            &task(),
            &binding(),
            &handle(),
            &completion().await,
            &self.record(),
            credentials,
        )
        .unwrap()
    }
    fn journal(&self) -> Vec<Value> {
        let path = self
            .root
            .join("journal")
            .join(format!("{}-2-7.jsonl", task().id));
        fs::read_to_string(path)
            .unwrap()
            .lines()
            .map(|s| serde_json::from_str(s).unwrap())
            .collect()
    }
}

#[derive(Default)]
struct Ledger {
    cancelled: AtomicBool,
    lost: AtomicBool,
}
#[async_trait]
impl BrokerLeaseFence for Ledger {
    async fn mark_running(&self, _: Uuid, _: &str, _: i64) -> Result<bool> {
        bail!("unexpected_mark")
    }
    async fn cancel_requested(&self, _: Uuid, _: &str, _: i64) -> Result<bool> {
        Ok(self.cancelled.load(Ordering::SeqCst))
    }
    async fn renew_lease(&self, _: Uuid, _: &str, _: i64, _: i32) -> Result<bool> {
        Ok(!self.lost.load(Ordering::SeqCst))
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
fn runtime() -> SandboxRuntime<TerminalBoundary> {
    SandboxRuntime::new(
        TerminalBoundary,
        SandboxBindingCatalog::new(vec![binding()]).unwrap(),
        60,
    )
    .unwrap()
}

#[derive(Default)]
struct ApiState {
    seen: Mutex<Vec<String>>,
    reject_secret: AtomicBool,
    lose_after_secret: AtomicBool,
    lose_after_pod: AtomicBool,
    ledger: Arc<Ledger>,
}
async fn api_create(
    State(s): State<Arc<ApiState>>,
    uri: Uri,
    headers: HeaderMap,
    Json(value): Json<Value>,
) -> impl IntoResponse {
    assert!(
        headers
            .get("authorization")
            .is_some_and(|h| h == "Bearer synthetic-service-account")
    );
    let kind = value["kind"].as_str().unwrap();
    let resource = match kind {
        "NetworkPolicy" => "networkpolicies",
        "Secret" => "secrets",
        "Service" => "services",
        "Pod" => "pods",
        _ => panic!("unexpected object kind"),
    };
    assert!(
        uri.path()
            .ends_with(&format!("/namespaces/{}/{resource}", binding().namespace))
    );
    s.seen.lock().unwrap().push(kind.into());
    if kind == "Secret" {
        assert!(value["immutable"] == true);
        assert!(value["stringData"].as_object().unwrap().len() == 5);
        assert!(
            value["stringData"]["server.key"]
                .as_str()
                .unwrap()
                .contains("PRIVATE KEY")
        );
        if s.reject_secret.load(Ordering::SeqCst) {
            // Simulates an unsafe API response. It must never reach a receipt/error.
            return (
                StatusCode::CONFLICT,
                Json(json!({"message":"sensitive-response-must-not-escape"})),
            );
        }
        if s.lose_after_secret.load(Ordering::SeqCst) {
            s.ledger.lost.store(true, Ordering::SeqCst);
        }
    }
    if kind == "Pod" {
        if s.lose_after_pod.load(Ordering::SeqCst) {
            s.ledger.lost.store(true, Ordering::SeqCst);
        }
        assert!(value["spec"]["automountServiceAccountToken"] == false);
        assert!(value["spec"]["volumes"][2]["persistentVolumeClaim"]["readOnly"] == true);
    }
    (
        StatusCode::CREATED,
        Json(
            json!({"metadata":{"name":value["metadata"]["name"],"namespace":binding().namespace,"uid":Uuid::new_v4().to_string()}}),
        ),
    )
}
struct TlsListener {
    tcp: TcpListener,
    tls: TlsAcceptor,
}
impl axum::serve::Listener for TlsListener {
    type Io = TlsStream<TcpStream>;
    type Addr = SocketAddr;
    async fn accept(&mut self) -> (Self::Io, Self::Addr) {
        loop {
            let (socket, address) = self.tcp.accept().await.unwrap();
            if let Ok(stream) = self.tls.accept(socket).await {
                return (stream, address);
            }
        }
    }
    fn local_addr(&self) -> std::io::Result<SocketAddr> {
        self.tcp.local_addr()
    }
}
async fn api(
    f: &Fixture,
    state: Arc<ApiState>,
) -> (
    ExportKubernetesClient,
    CancellationToken,
    tokio::task::JoinHandle<()>,
) {
    let _ = rustls::crypto::ring::default_provider().install_default();
    let CertifiedKey { cert, signing_key } =
        generate_simple_self_signed(vec!["127.0.0.1".into()]).unwrap();
    let ca = f.root.join("api/ca.pem");
    fs::write(&ca, cert.pem()).unwrap();
    let token = f.root.join("api/token");
    fs::write(&token, b"synthetic-service-account").unwrap();
    fs::set_permissions(&token, fs::Permissions::from_mode(0o600)).unwrap();
    let config = ServerConfig::builder()
        .with_no_client_auth()
        .with_single_cert(
            vec![CertificateDer::from(cert.der().to_vec())],
            PrivatePkcs8KeyDer::from(signing_key.serialize_der()).into(),
        )
        .unwrap();
    let tcp = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("https://{}/", tcp.local_addr().unwrap())
        .parse()
        .unwrap();
    let client =
        ExportKubernetesClient::from_service_account(url, binding().namespace, token, &ca).unwrap();
    let app = Router::new()
        .route("/{*path}", post(api_create))
        .with_state(state);
    let cancel = CancellationToken::new();
    let stopped = cancel.clone();
    let serving = tokio::spawn(async move {
        axum::serve(
            TlsListener {
                tcp,
                tls: TlsAcceptor::from(Arc::new(config)),
            },
            app,
        )
        .with_graceful_shutdown(stopped.cancelled_owned())
        .await
        .unwrap();
    });
    (client, cancel, serving)
}

#[tokio::test]
async fn publishes_over_tls_with_private_staging_durable_intents_and_exact_order() {
    let f = Fixture::new();
    let p = f.prepared().await;
    let state = Arc::new(ApiState::default());
    let (client, stop, server) = api(&f, state.clone()).await;
    let runtime = runtime();
    let guard = ObservedLeaseGuard {
        ledger: state.ledger.as_ref(),
        runtime: &runtime,
        lease_seconds: 60,
    };
    let receipts = p
        .publish(&guard, &client, &CancellationToken::new())
        .await
        .unwrap();
    assert_eq!(receipts.len(), 4);
    assert_eq!(
        *state.seen.lock().unwrap(),
        vec!["NetworkPolicy", "Secret", "Service", "Pod"]
    );
    let journal = f.journal();
    assert_eq!(
        journal
            .iter()
            .map(|v| v["phase"].as_str().unwrap())
            .collect::<Vec<_>>(),
        vec![
            "prepared",
            "create_intent",
            "created",
            "create_intent",
            "created",
            "create_intent",
            "created",
            "create_intent",
            "created",
            "published"
        ]
    );
    assert!(
        journal
            .iter()
            .enumerate()
            .all(|(i, v)| v["sequence"].as_u64() == Some(i as u64 + 1))
    );
    let client_dir = f
        .config
        .remote
        .credential_root
        .join(format!("{}-2-7", task().id));
    assert_eq!(
        fs::metadata(&client_dir).unwrap().permissions().mode() & 0o777,
        0o700
    );
    assert_eq!(
        fs::metadata(client_dir.join("client.pem"))
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o600
    );
    let safe = serde_json::to_string(&journal).unwrap() + &format!("{p:?}{receipts:?}");
    assert!(
        !safe.contains("PRIVATE KEY")
            && !safe.contains("BEGIN CERTIFICATE")
            && !safe.contains("stringData")
    );
    assert!(
        p.publish(&guard, &client, &CancellationToken::new())
            .await
            .unwrap_err()
            .to_string()
            .contains("requires_recovery")
    );
    assert_eq!(state.seen.lock().unwrap().len(), 4);
    stop.cancel();
    drop(client);
    tokio::time::timeout(Duration::from_secs(2), server)
        .await
        .unwrap()
        .unwrap();
}

#[tokio::test]
async fn partial_create_and_lost_lease_preserve_custody_without_later_objects_or_silent_retry() {
    for mode in 0..3 {
        let f = Fixture::new();
        let p = f.prepared().await;
        let state = Arc::new(ApiState::default());
        state.reject_secret.store(mode == 0, Ordering::SeqCst);
        state.lose_after_secret.store(mode == 1, Ordering::SeqCst);
        state.lose_after_pod.store(mode == 2, Ordering::SeqCst);
        let (client, stop, server) = api(&f, state.clone()).await;
        let runtime = runtime();
        let guard = ObservedLeaseGuard {
            ledger: state.ledger.as_ref(),
            runtime: &runtime,
            lease_seconds: 60,
        };
        let error = p
            .publish(&guard, &client, &CancellationToken::new())
            .await
            .unwrap_err()
            .to_string();
        assert!(!error.contains("sensitive-response"));
        let expected = if mode == 2 {
            vec!["NetworkPolicy", "Secret", "Service", "Pod"]
        } else {
            vec!["NetworkPolicy", "Secret"]
        };
        assert_eq!(*state.seen.lock().unwrap(), expected);
        let journal = f.journal();
        assert_eq!(journal.last().unwrap()["phase"], "stopped");
        assert!(
            !serde_json::to_string(&journal)
                .unwrap()
                .contains("sensitive-response")
        );
        state.ledger.lost.store(false, Ordering::SeqCst);
        assert!(
            p.publish(&guard, &client, &CancellationToken::new())
                .await
                .is_err()
        );
        assert_eq!(state.seen.lock().unwrap().len(), expected.len());
        stop.cancel();
        drop(client);
        tokio::time::timeout(Duration::from_secs(2), server)
            .await
            .unwrap()
            .unwrap();
    }
}

#[tokio::test]
async fn rejects_foreign_terminal_identity_oversize_secret_and_cancelled_task_before_publication() {
    let f = Fixture::new();
    let completion = completion().await;
    let mut wrong: Value = serde_json::from_slice(&f.record()).unwrap();
    wrong["podUid"] = json!("foreign-pod");
    let mut large = vec![b' '; 1024 * 1024];
    large.extend(f.record());
    for record in [serde_json::to_vec(&wrong).unwrap(), large] {
        let credentials = issue_export_credentials(
            &f.config.remote,
            ExportScope::new(task().id, 2, 7).unwrap(),
            &binding().namespace,
            ExportCredentialLifetime {
                task_seconds: 300,
                export_seconds: 60,
            },
        )
        .unwrap();
        assert!(
            PreparedExportPublication::prepare(
                f.config.clone(),
                &task(),
                &binding(),
                &handle(),
                &completion,
                &record,
                credentials
            )
            .is_err()
        );
    }
    let p = f.prepared().await;
    let state = Arc::new(ApiState::default());
    state.ledger.cancelled.store(true, Ordering::SeqCst);
    let (client, stop, server) = api(&f, state.clone()).await;
    let runtime = runtime();
    let guard = ObservedLeaseGuard {
        ledger: state.ledger.as_ref(),
        runtime: &runtime,
        lease_seconds: 60,
    };
    assert!(
        p.publish(&guard, &client, &CancellationToken::new())
            .await
            .is_err()
    );
    assert!(state.seen.lock().unwrap().is_empty());
    assert_eq!(fs::read_dir(&f.config.journal_root).unwrap().count(), 0);
    stop.cancel();
    drop(client);
    tokio::time::timeout(Duration::from_secs(2), server)
        .await
        .unwrap()
        .unwrap();
}
