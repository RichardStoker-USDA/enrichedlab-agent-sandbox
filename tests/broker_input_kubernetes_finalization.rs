#![cfg(unix)]

use axum::{
    Router,
    body::Body,
    extract::State,
    http::{HeaderMap, Response, StatusCode, Uri},
    routing::get,
};
use enrichedlab_agent_sandbox::broker::{
    input_finalization::InputFinalizationTransport, input_kubernetes::InputKubernetesClient,
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
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    },
};
use tempfile::TempDir;
use tokio::net::{TcpListener, TcpStream};
use tokio_rustls::{TlsAcceptor, server::TlsStream};
use tokio_util::sync::CancellationToken;

const NS: &str = "sandbox-test";
const JOB: &str = "input-copy-test";
const JOB_UID: &str = "job-uid";
const POD_UID: &str = "pod-uid";
const POD_NAME: &str = "input-copy-test-pod";
const INPUT: &str = "input-run-test";

struct StateData {
    token: String,
    requests: AtomicUsize,
    replacement: Mutex<bool>,
    delete_conflict: Mutex<bool>,
    pod_uid: Mutex<Option<&'static str>>,
}
fn response(status: StatusCode, value: Value) -> Response<Body> {
    Response::builder()
        .status(status)
        .header("content-type", "application/json")
        .body(Body::from(serde_json::to_vec(&value).unwrap()))
        .unwrap()
}
fn authorized(headers: &HeaderMap, state: &StateData) -> bool {
    headers.get("authorization").and_then(|v| v.to_str().ok())
        == Some(&format!("Bearer {}", state.token))
}
async fn api_get(
    State(state): State<Arc<StateData>>,
    uri: Uri,
    headers: HeaderMap,
) -> Response<Body> {
    if !authorized(&headers, &state) {
        return response(StatusCode::FORBIDDEN, json!({}));
    }
    state.requests.fetch_add(1, Ordering::SeqCst);
    match uri.path() {
        "/api/v1/namespaces/sandbox-test/persistentvolumeclaims/input-run-test" => response(
            StatusCode::OK,
            json!({"metadata":{"name":INPUT,"namespace":NS,"uid":"input-uid","resourceVersion":"1"}}),
        ),
        "/apis/batch/v1/namespaces/sandbox-test/jobs/input-copy-test"
            if *state.replacement.lock().unwrap() =>
        {
            response(
                StatusCode::OK,
                json!({"metadata":{"name":JOB,"namespace":NS,"uid":"replacement-uid"}}),
            )
        }
        "/apis/batch/v1/namespaces/sandbox-test/jobs/input-copy-test" => {
            response(StatusCode::NOT_FOUND, json!({}))
        }
        "/api/v1/namespaces/sandbox-test/pods/input-copy-test-pod" => {
            assert!(uri.query().is_none());
            match *state.pod_uid.lock().unwrap() {
                Some(uid) => response(
                    StatusCode::OK,
                    json!({"metadata":{"name":POD_NAME,"namespace":NS,"uid":uid}}),
                ),
                None => response(StatusCode::NOT_FOUND, json!({})),
            }
        }
        _ => response(StatusCode::NOT_FOUND, json!({})),
    }
}
async fn api_delete(
    State(state): State<Arc<StateData>>,
    uri: Uri,
    headers: HeaderMap,
    axum::Json(value): axum::Json<Value>,
) -> Response<Body> {
    if !authorized(&headers, &state) {
        return response(StatusCode::FORBIDDEN, json!({}));
    }
    state.requests.fetch_add(1, Ordering::SeqCst);
    assert_eq!(
        uri.path(),
        "/apis/batch/v1/namespaces/sandbox-test/jobs/input-copy-test"
    );
    assert_eq!(
        value,
        json!({"apiVersion":"v1","kind":"DeleteOptions","propagationPolicy":"Foreground","preconditions":{"uid":JOB_UID}})
    );
    if *state.delete_conflict.lock().unwrap() {
        return response(StatusCode::CONFLICT, json!({}));
    }
    response(StatusCode::ACCEPTED, json!({"kind":"Status"}))
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
struct Fixture {
    _temp: TempDir,
    state: Arc<StateData>,
    client: InputKubernetesClient,
    stop: CancellationToken,
    server: tokio::task::JoinHandle<()>,
}
impl Fixture {
    async fn new() -> Self {
        let temp = TempDir::new().unwrap();
        let root = fs::canonicalize(temp.path()).unwrap();
        let CertifiedKey { cert, signing_key } =
            generate_simple_self_signed(vec!["127.0.0.1".into()]).unwrap();
        let ca = root.join("ca.pem");
        fs::write(&ca, cert.pem()).unwrap();
        let token = root.join("token");
        fs::write(&token, b"test-token\n").unwrap();
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
        let state = Arc::new(StateData {
            token: "test-token".into(),
            requests: AtomicUsize::new(0),
            replacement: Mutex::new(false),
            delete_conflict: Mutex::new(false),
            pod_uid: Mutex::new(None),
        });
        let app = Router::new()
            .route("/{*path}", get(api_get).delete(api_delete))
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
        let client =
            InputKubernetesClient::from_service_account(origin, NS.into(), token, &ca).unwrap();
        Self {
            _temp: temp,
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
async fn reads_exact_resources_and_deletes_only_the_observed_job() {
    let fixture = Fixture::new().await;
    assert_eq!(
        fixture.client.get_pvc(NS, INPUT).await.unwrap()["metadata"]["uid"],
        "input-uid"
    );
    fixture
        .client
        .delete_completed_job(NS, JOB, JOB_UID, &CancellationToken::new())
        .await
        .unwrap();
    assert!(
        fixture
            .client
            .get_job_after_cleanup(NS, JOB)
            .await
            .unwrap()
            .is_none()
    );
    assert!(
        fixture
            .client
            .get_pod_after_cleanup(NS, POD_NAME, POD_UID)
            .await
            .unwrap()
            .is_none()
    );
    assert_eq!(fixture.state.requests.load(Ordering::SeqCst), 4);
    fixture.close().await;
}

#[tokio::test]
async fn cancellation_and_replacement_fail_closed() {
    let fixture = Fixture::new().await;
    let cancelled = CancellationToken::new();
    cancelled.cancel();
    assert!(
        fixture
            .client
            .delete_completed_job(NS, JOB, JOB_UID, &cancelled)
            .await
            .is_err()
    );
    assert_eq!(fixture.state.requests.load(Ordering::SeqCst), 0);
    *fixture.state.delete_conflict.lock().unwrap() = true;
    assert!(
        fixture
            .client
            .delete_completed_job(NS, JOB, JOB_UID, &CancellationToken::new())
            .await
            .is_err()
    );
    *fixture.state.replacement.lock().unwrap() = true;
    let replacement = fixture
        .client
        .get_job_after_cleanup(NS, JOB)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(replacement["metadata"]["uid"], "replacement-uid");
    *fixture.state.pod_uid.lock().unwrap() = Some(POD_UID);
    assert!(
        fixture
            .client
            .get_pod_after_cleanup(NS, POD_NAME, POD_UID)
            .await
            .unwrap()
            .is_some()
    );
    *fixture.state.pod_uid.lock().unwrap() = Some("replacement-pod-uid");
    assert!(
        fixture
            .client
            .get_pod_after_cleanup(NS, POD_NAME, POD_UID)
            .await
            .is_err()
    );
    fixture.close().await;
}
