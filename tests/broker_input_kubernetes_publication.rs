#![cfg(unix)]

use anyhow::{Result, bail};
use async_trait::async_trait;
use axum::{
    Json, Router,
    body::Body,
    extract::State,
    http::{HeaderMap, Response, StatusCode, Uri},
    routing::get,
};
use enrichedlab_agent_sandbox::{
    broker::{
        input_kubernetes::InputKubernetesClient,
        input_publication::{
            InputPublicationConfig, InputSourceIdentity, InputSourceLeaseGuard,
            InputSourceObservationTransport, PreparedInputPublication,
        },
        input_workload::{
            AuthorizedInputCopy, CustodyPvcAccessMode, InputCopyAuthorization, InputWorkloadConfig,
        },
        model::ClaimedTask,
        sandbox::{BrokerLeaseFence, RetainedArtifact, SandboxBinding},
    },
    custody::{CopySealRequest, InputManifest, SealAttestation},
};
use rcgen::{CertifiedKey, generate_simple_self_signed};
use rustls::{
    ServerConfig,
    pki_types::{CertificateDer, PrivatePkcs8KeyDer},
};
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    fs,
    net::SocketAddr,
    os::unix::fs::PermissionsExt,
    path::PathBuf,
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    },
};
use tempfile::TempDir;
use tokio::net::{TcpListener, TcpStream};
use tokio_rustls::{TlsAcceptor, server::TlsStream};
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

const IMAGE: &str = "registry.test/custodian@sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
const PAYLOAD: &[u8] = include_bytes!("fixtures/admission/fixture");

struct Fixture {
    _temp: TempDir,
    root: PathBuf,
    config: InputPublicationConfig,
    task: ClaimedTask,
    binding: SandboxBinding,
    authorization: InputCopyAuthorization,
    request: CopySealRequest,
    source: InputSourceIdentity,
}

impl Fixture {
    fn new() -> Self {
        let temp = TempDir::new().unwrap();
        let root = fs::canonicalize(temp.path()).unwrap();
        for name in ["journal", "custody", "api"] {
            fs::create_dir(root.join(name)).unwrap();
            fs::set_permissions(root.join(name), fs::Permissions::from_mode(0o700)).unwrap();
        }
        let staged: Value =
            serde_json::from_str(include_str!("fixtures/admission/staged_admission.json")).unwrap();
        let seal: SealAttestation = serde_json::from_value(staged["sealedInput"].clone()).unwrap();
        let manifest: InputManifest =
            serde_json::from_str(include_str!("fixtures/admission/input_manifest.json")).unwrap();
        let request = CopySealRequest {
            schema_version: seal.schema_version,
            staging_id: seal.staging_id,
            run_id: seal.run_id,
            tenant_id: seal.tenant_id,
            namespace: seal.namespace,
            principal_hash: seal.principal_hash,
            input_ref: seal.input_ref,
            input_digest: seal.input_digest,
            pvc_name: seal.pvc_name,
            manifest_ref: seal.manifest_ref,
            attestation_ref: seal.attestation_ref,
            source_root: "/source".into(),
            destination_root: "/destination".into(),
            custody_root: "/custody".into(),
            files: manifest.files,
            max_files: Some(1),
            max_bytes: Some(PAYLOAD.len() as u64),
            admitted_files: vec![],
        };
        let task = ClaimedTask {
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
        };
        let binding = SandboxBinding {
            principal_id: task.principal_id,
            principal_hash: request.principal_hash.clone(),
            group_key: task.group_key.clone(),
            profile_key: task.profile_key.clone(),
            profile_id: "example-kata".into(),
            lab_ref: "lab://example/research".into(),
            namespace: request.namespace.clone(),
            runtime_class_name: "kata-clh".into(),
        };
        let authorization = InputCopyAuthorization {
            task_id: task.id,
            attempt: task.attempt,
            lease_generation: task.lease_generation,
            run_id: request.run_id.clone(),
            tenant_id: request.tenant_id.clone(),
            source_run_name: "source-run".into(),
            source_pvc_name: "input-source-run".into(),
            principal_hash: request.principal_hash.clone(),
            profile_id: binding.profile_id.clone(),
            namespace: binding.namespace.clone(),
            custody_retention_until: "2099-01-01T00:00:00Z".into(),
            input_digest: request.input_digest.clone(),
        };
        let source = InputSourceIdentity {
            namespace: binding.namespace.clone(),
            pvc_name: "input-source-run".into(),
            pvc_uid: "approved-source-uid".into(),
            run_id: "source-run".into(),
            tenant_id: request.tenant_id.clone(),
            principal_hash: request.principal_hash.clone(),
        };
        Self {
            _temp: temp,
            root: root.clone(),
            config: InputPublicationConfig {
                workload: InputWorkloadConfig {
                    site: Default::default(),
                    runtime_class_name: "kata-clh".into(),
                    image: IMAGE.into(),
                    pool_selector: BTreeMap::from([("workload".into(), "sandbox".into())]),
                    cpu_request_millis: 100,
                    cpu_limit_millis: 500,
                    memory_request_mib: 128,
                    memory_limit_mib: 512,
                    active_deadline_seconds: 300,
                    custody_memory_mib: 16,
                    input_storage_class_name: "example-input".into(),
                    artifact_storage_class_name: "example-output".into(),
                    input_storage_mib: 128,
                    artifact_storage_mib: 128,
                    custody_pvc_access_mode: CustodyPvcAccessMode::ReadWriteOnce,
                    max_request_bytes: 768 * 1024,
                },
                journal_root: root.join("journal"),
                custody_root: root.join("custody"),
                writer_uid: unsafe { libc::geteuid() },
            },
            task,
            binding,
            authorization,
            request,
            source,
        }
    }

    fn prepare(&self) -> PreparedInputPublication {
        let copy = AuthorizedInputCopy::new(
            self.source.run_id.clone(),
            self.source.pvc_name.clone(),
            "copy-request".into(),
            self.request.clone(),
        )
        .unwrap();
        PreparedInputPublication::prepare(
            self.config.clone(),
            &self.task,
            "worker-example".into(),
            &self.binding,
            &self.authorization,
            &copy,
            self.source.clone(),
        )
        .unwrap()
    }

    fn source_pvc(&self) -> Value {
        json!({"metadata":{"name":self.source.pvc_name,"namespace":self.source.namespace,"uid":self.source.pvc_uid,
          "labels":{"sandbox.enrichedlab.org/run-id":self.source.run_id,"sandbox.enrichedlab.org/tenant":self.source.tenant_id,
          "sandbox.enrichedlab.org/principal-hash":self.source.principal_hash,"sandbox.enrichedlab.org/contract-version":"v3",
          "sandbox.enrichedlab.org/volume-role":"immutable-input","sandbox.enrichedlab.org/seal-state":"sealed",
          "sandbox.enrichedlab.org/materialization-mode":"checksummed-copy"}},"status":{"phase":"Bound"}})
    }

    fn request_path(&self) -> PathBuf {
        self.config.custody_root.join(format!(
            "input-{}-{}-{}.request.json",
            self.task.id, self.task.attempt, self.task.lease_generation
        ))
    }
}

struct Ledger;
#[async_trait]
impl BrokerLeaseFence for Ledger {
    async fn mark_running(&self, _: Uuid, _: &str, _: i64) -> Result<bool> {
        bail!("unexpected")
    }
    async fn cancel_requested(&self, _: Uuid, _: &str, _: i64) -> Result<bool> {
        Ok(false)
    }
    async fn renew_lease(&self, _: Uuid, _: &str, _: i64, seconds: i32) -> Result<bool> {
        Ok(seconds == 60)
    }
    async fn register_artifact(
        &self,
        _: Uuid,
        _: &str,
        _: i64,
        _: &RetainedArtifact,
    ) -> Result<()> {
        bail!("unexpected")
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
        bail!("unexpected")
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Mode {
    Happy,
    Conflict,
    BadUid,
    QuotaFull,
    Forbidden,
    Writer,
    Paginated,
    PaginationBound,
}

struct ApiState {
    pvc: Value,
    namespace: String,
    source_pvc: String,
    token: Mutex<String>,
    mode: Mutex<Mode>,
    post_paths: Mutex<Vec<String>>,
    pod_pages: AtomicUsize,
}

fn active_pod(namespace: &str, pvc: &str, writable: bool) -> Value {
    json!({"metadata":{"name":"copy-pod","namespace":namespace},"status":{"phase":"Running"},
      "spec":{"volumes":[{"name":"source","persistentVolumeClaim":{"claimName":pvc,"readOnly":!writable}}],
      "containers":[{"name":"custodian","volumeMounts":[{"name":"source","mountPath":"/source","readOnly":!writable}]}],
      "initContainers":[],"ephemeralContainers":[]}})
}

async fn api_get(
    State(state): State<Arc<ApiState>>,
    uri: Uri,
    headers: HeaderMap,
) -> Response<Body> {
    let auth = headers
        .get("authorization")
        .and_then(|v| v.to_str().ok())
        .unwrap_or_default();
    if auth != format!("Bearer {}", state.token.lock().unwrap()) {
        return Response::builder()
            .status(StatusCode::FORBIDDEN)
            .body(Body::empty())
            .unwrap();
    }
    let pvc_path = format!(
        "/api/v1/namespaces/{}/persistentvolumeclaims/{}",
        state.namespace, state.source_pvc
    );
    if uri.path() == pvc_path {
        return json_response(state.pvc.clone());
    }
    let pods_path = format!("/api/v1/namespaces/{}/pods", state.namespace);
    if uri.path() == pods_path {
        assert!(uri.query().is_some_and(|q| q.contains("limit=100")));
        let mode = *state.mode.lock().unwrap();
        let page = state.pod_pages.fetch_add(1, Ordering::SeqCst);
        let continuation = match mode {
            Mode::Paginated if page == 0 => "next",
            Mode::PaginationBound => "next",
            _ => "",
        };
        let items = match mode {
            Mode::Writer => vec![active_pod(&state.namespace, &state.source_pvc, true)],
            _ => vec![active_pod(&state.namespace, &state.source_pvc, false)],
        };
        return json_response(json!({"metadata":{"continue":continuation},"items":items}));
    }
    Response::builder()
        .status(StatusCode::NOT_FOUND)
        .body(Body::empty())
        .unwrap()
}

async fn api_post(
    State(state): State<Arc<ApiState>>,
    uri: Uri,
    headers: HeaderMap,
    Json(value): Json<Value>,
) -> Response<Body> {
    let auth = headers
        .get("authorization")
        .and_then(|v| v.to_str().ok())
        .unwrap_or_default();
    if auth != format!("Bearer {}", state.token.lock().unwrap()) {
        return Response::builder()
            .status(StatusCode::FORBIDDEN)
            .body(Body::empty())
            .unwrap();
    }
    assert!(value["metadata"]["namespace"] == state.namespace);
    assert!(
        value
            .get("stringData")
            .is_none_or(|secret| secret.get("request.json").is_some())
    );
    let mut paths = state.post_paths.lock().unwrap();
    paths.push(uri.path().into());
    let index = paths.len();
    match *state.mode.lock().unwrap() {
        Mode::Conflict if index == 1 => Response::builder()
            .status(StatusCode::CONFLICT)
            .body(Body::empty())
            .unwrap(),
        // The input volume is refused after the network policy exists: the tenant's quota is full,
        // or another refusal the custodian must not mistake for capacity.
        Mode::QuotaFull if index == 2 => json_response_status(
            StatusCode::FORBIDDEN,
            json!({"kind":"Status","apiVersion":"v1","status":"Failure","reason":"Forbidden","code":403,
                "message":"persistentvolumeclaims \"input-copy\" is forbidden: exceeded quota: sandbox-tenant, requested: persistentvolumeclaims=1, used: persistentvolumeclaims=96, limited: persistentvolumeclaims=96"}),
        ),
        Mode::Forbidden if index == 2 => json_response_status(
            StatusCode::FORBIDDEN,
            json!({"kind":"Status","apiVersion":"v1","status":"Failure","reason":"Forbidden","code":403,
                "message":"persistentvolumeclaims is forbidden: the admission policy refused the object"}),
        ),
        Mode::BadUid if index == 1 => json_response_status(
            StatusCode::CREATED,
            json!({"metadata":{"name":"wrong","namespace":state.namespace,"uid":"bad"}}),
        ),
        _ => json_response_status(
            StatusCode::CREATED,
            json!({"metadata":{"name":value["metadata"]["name"],"namespace":state.namespace,"uid":format!("created-{index}")}}),
        ),
    }
}

fn json_response(value: Value) -> Response<Body> {
    json_response_status(StatusCode::OK, value)
}
fn json_response_status(status: StatusCode, value: Value) -> Response<Body> {
    Response::builder()
        .status(status)
        .header("content-type", "application/json")
        .body(Body::from(serde_json::to_vec(&value).unwrap()))
        .unwrap()
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
            let (socket, addr) = self.tcp.accept().await.unwrap();
            if let Ok(stream) = self.tls.accept(socket).await {
                return (stream, addr);
            }
        }
    }
    fn local_addr(&self) -> std::io::Result<Self::Addr> {
        self.tcp.local_addr()
    }
}

struct ApiFixture {
    state: Arc<ApiState>,
    client: InputKubernetesClient,
    stop: CancellationToken,
    server: tokio::task::JoinHandle<()>,
}
impl ApiFixture {
    async fn new(f: &Fixture, mode: Mode) -> Self {
        let _ = rustls::crypto::ring::default_provider().install_default();
        let CertifiedKey { cert, signing_key } =
            generate_simple_self_signed(vec!["127.0.0.1".into()]).unwrap();
        let ca = f.root.join("api/ca.pem");
        fs::write(&ca, cert.pem()).unwrap();
        let token = f.root.join("api/token");
        fs::write(&token, b"synthetic-input-token\n").unwrap();
        fs::set_permissions(&token, fs::Permissions::from_mode(0o600)).unwrap();
        let config = ServerConfig::builder()
            .with_no_client_auth()
            .with_single_cert(
                vec![CertificateDer::from(cert.der().to_vec())],
                PrivatePkcs8KeyDer::from(signing_key.serialize_der()).into(),
            )
            .unwrap();
        let tcp = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let origin = format!("https://{}/", tcp.local_addr().unwrap())
            .parse()
            .unwrap();
        let state = Arc::new(ApiState {
            pvc: f.source_pvc(),
            namespace: f.binding.namespace.clone(),
            source_pvc: f.source.pvc_name.clone(),
            token: Mutex::new("synthetic-input-token".into()),
            mode: Mutex::new(mode),
            post_paths: Mutex::new(Vec::new()),
            pod_pages: AtomicUsize::new(0),
        });
        let app = Router::new()
            .route("/{*path}", get(api_get).post(api_post))
            .with_state(state.clone());
        let stop = CancellationToken::new();
        let cancelled = stop.clone();
        let server = tokio::spawn(async move {
            axum::serve(
                TlsListener {
                    tcp,
                    tls: TlsAcceptor::from(Arc::new(config)),
                },
                app,
            )
            .with_graceful_shutdown(cancelled.cancelled_owned())
            .await
            .unwrap();
        });
        let client = InputKubernetesClient::from_service_account(
            origin,
            f.binding.namespace.clone(),
            token,
            &ca,
        )
        .unwrap();
        Self {
            state,
            client,
            stop,
            server,
        }
    }
    async fn close(self) {
        self.stop.cancel();
        self.server.await.unwrap();
    }
}

#[tokio::test]
async fn publishes_ordered_objects_after_private_request_and_accepts_read_only_copy_pod() {
    let f = Fixture::new();
    let api = ApiFixture::new(&f, Mode::Happy).await;
    let ledger = Ledger;
    let guard = InputSourceLeaseGuard {
        ledger: &ledger,
        source: &api.client,
        lease_seconds: 60,
    };
    let published = f
        .prepare()
        .publish(&guard, &api.client, &CancellationToken::new())
        .await
        .unwrap();
    assert_eq!(published.receipt().objects.len(), 5);
    assert_eq!(
        *api.state.post_paths.lock().unwrap(),
        vec![
            format!(
                "/apis/networking.k8s.io/v1/namespaces/{}/networkpolicies",
                f.binding.namespace
            ),
            format!(
                "/api/v1/namespaces/{}/persistentvolumeclaims",
                f.binding.namespace
            ),
            format!(
                "/api/v1/namespaces/{}/persistentvolumeclaims",
                f.binding.namespace
            ),
            format!("/api/v1/namespaces/{}/secrets", f.binding.namespace),
            format!("/apis/batch/v1/namespaces/{}/jobs", f.binding.namespace),
        ]
    );
    assert!(f.request_path().exists());
    api.close().await;
}

#[tokio::test]
async fn stops_conflict_identity_mismatch_and_cancel_before_compute() {
    for mode in [Mode::Conflict, Mode::BadUid] {
        let f = Fixture::new();
        let api = ApiFixture::new(&f, mode).await;
        let ledger = Ledger;
        let guard = InputSourceLeaseGuard {
            ledger: &ledger,
            source: &api.client,
            lease_seconds: 60,
        };
        assert!(
            f.prepare()
                .publish(&guard, &api.client, &CancellationToken::new())
                .await
                .is_err()
        );
        assert_eq!(api.state.post_paths.lock().unwrap().len(), 1);
        api.close().await;
    }
    let f = Fixture::new();
    let api = ApiFixture::new(&f, Mode::Happy).await;
    let ledger = Ledger;
    let guard = InputSourceLeaseGuard {
        ledger: &ledger,
        source: &api.client,
        lease_seconds: 60,
    };
    let cancelled = CancellationToken::new();
    cancelled.cancel();
    assert!(
        f.prepare()
            .publish(&guard, &api.client, &cancelled)
            .await
            .is_err()
    );
    assert!(api.state.post_paths.lock().unwrap().is_empty());
    api.close().await;
}

#[tokio::test]
async fn a_full_tenant_is_named_as_a_capacity_refusal() {
    // A volume refused by the tenant's quota stops publication with a named cause; any
    // other refusal keeps the generic recovery code. Objects created before it stay for recovery.
    for (mode, expected, reason) in [
        (
            Mode::QuotaFull,
            "input_capacity_exhausted",
            "capacity_exhausted",
        ),
        (
            Mode::Forbidden,
            "input_publication_requires_recovery",
            "create_requires_recovery",
        ),
    ] {
        let f = Fixture::new();
        let api = ApiFixture::new(&f, mode).await;
        let ledger = Ledger;
        let guard = InputSourceLeaseGuard {
            ledger: &ledger,
            source: &api.client,
            lease_seconds: 60,
        };
        let error = f
            .prepare()
            .publish(&guard, &api.client, &CancellationToken::new())
            .await
            .unwrap_err();
        assert_eq!(error.to_string(), expected);
        assert_eq!(api.state.post_paths.lock().unwrap().len(), 2);
        let journal = fs::read_dir(f.root.join("journal"))
            .unwrap()
            .filter_map(Result::ok)
            .find(|e| e.path().extension().is_some_and(|x| x == "jsonl"))
            .unwrap();
        let text = fs::read_to_string(journal.path()).unwrap();
        let last: Value = serde_json::from_str(text.lines().last().unwrap()).unwrap();
        assert_eq!(last["phase"], "stopped");
        assert_eq!(last["reason"], reason);
        if reason == "capacity_exhausted" {
            assert_eq!(last["createdBefore"], 1);
        }
        api.close().await;
    }
}

#[tokio::test]
async fn detects_writers_and_fails_closed_at_pagination_boundary() {
    let f = Fixture::new();
    let api = ApiFixture::new(&f, Mode::Writer).await;
    assert!(
        api.client
            .source_has_active_writer(&f.binding.namespace, &f.source.pvc_name)
            .await
            .unwrap()
    );
    assert!(
        api.client
            .get_source_pvc("other-namespace", &f.source.pvc_name)
            .await
            .is_err()
    );
    api.close().await;
    let f = Fixture::new();
    let api = ApiFixture::new(&f, Mode::Paginated).await;
    assert!(
        !api.client
            .source_has_active_writer(&f.binding.namespace, &f.source.pvc_name)
            .await
            .unwrap()
    );
    assert_eq!(api.state.pod_pages.load(Ordering::SeqCst), 2);
    api.close().await;
    let f = Fixture::new();
    let api = ApiFixture::new(&f, Mode::PaginationBound).await;
    assert!(
        api.client
            .source_has_active_writer(&f.binding.namespace, &f.source.pvc_name)
            .await
            .is_err()
    );
    assert_eq!(api.state.pod_pages.load(Ordering::SeqCst), 10);
    api.close().await;
}
