#![cfg(unix)]

use axum::{
    Router,
    body::Body,
    extract::State,
    http::{HeaderMap, Response, StatusCode, Uri},
    routing::get,
};
use enrichedlab_agent_sandbox::broker::{
    input_completion::InputCopyObservationTransport, input_kubernetes::InputKubernetesClient,
};
use rcgen::{CertifiedKey, generate_simple_self_signed};
use rustls::{
    ServerConfig,
    pki_types::{CertificateDer, PrivatePkcs8KeyDer},
};
use serde_json::json;
use std::{
    fs,
    net::SocketAddr,
    os::unix::fs::PermissionsExt,
    path::PathBuf,
    sync::{Arc, Mutex},
};
use tempfile::TempDir;
use tokio::net::{TcpListener, TcpStream};
use tokio_rustls::{TlsAcceptor, server::TlsStream};
use tokio_util::sync::CancellationToken;

const NAMESPACE: &str = "sandbox-example-001";
const JOB: &str = "input-copy-run-1";
const POD: &str = "input-copy-run-1-abcde";
const LOG_LIMIT: usize = 1024 * 1024 + 1;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Mode {
    Normal,
    RedirectJob,
    OversizedJob,
    AmbiguousPods,
    OversizedLog,
}

struct ApiState {
    expected_token: Mutex<String>,
    requests: Mutex<Vec<(String, Option<String>, String)>>,
    mode: Mutex<Mode>,
}

impl ApiState {
    fn new() -> Self {
        Self {
            expected_token: Mutex::new("first-synthetic-token".into()),
            requests: Mutex::new(Vec::new()),
            mode: Mutex::new(Mode::Normal),
        }
    }
}

async fn api_get(
    State(state): State<Arc<ApiState>>,
    uri: Uri,
    headers: HeaderMap,
) -> Response<Body> {
    let authorization = headers
        .get("authorization")
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default()
        .to_owned();
    state.requests.lock().unwrap().push((
        uri.path().to_owned(),
        uri.query().map(str::to_owned),
        authorization.clone(),
    ));
    if authorization != format!("Bearer {}", state.expected_token.lock().unwrap()) {
        return Response::builder()
            .status(StatusCode::FORBIDDEN)
            .body(Body::empty())
            .unwrap();
    }
    let mode = *state.mode.lock().unwrap();
    let path = uri.path();
    if path == format!("/apis/batch/v1/namespaces/{NAMESPACE}/jobs/{JOB}") {
        if mode == Mode::RedirectJob {
            return Response::builder()
                .status(StatusCode::FOUND)
                .header("location", "/must-not-follow")
                .body(Body::empty())
                .unwrap();
        }
        if mode == Mode::OversizedJob {
            return Response::builder()
                .status(StatusCode::OK)
                .body(Body::from(vec![b'x'; 2 * 1024 * 1024 + 1]))
                .unwrap();
        }
        return json_response(json!({"metadata":{"name":JOB,"namespace":NAMESPACE}}));
    }
    if path == format!("/api/v1/namespaces/{NAMESPACE}/pods") {
        assert_eq!(
            uri.query(),
            Some("labelSelector=batch.kubernetes.io%2Fjob-name%3Dinput-copy-run-1&limit=2")
        );
        let pod = json!({"metadata":{"name":POD,"labels":{"batch.kubernetes.io/job-name":JOB}}});
        let items = if mode == Mode::AmbiguousPods {
            vec![pod.clone(), pod]
        } else {
            vec![pod]
        };
        return json_response(json!({"metadata":{"continue":""},"items":items}));
    }
    if path == format!("/api/v1/namespaces/{NAMESPACE}/pods/{POD}/log") {
        assert_eq!(
            uri.query(),
            Some("container=custodian&limitBytes=1048577&timestamps=false&previous=false")
        );
        let body = if mode == Mode::OversizedLog {
            vec![b'x'; LOG_LIMIT + 1]
        } else {
            b"sealed-receipt\n".to_vec()
        };
        return Response::builder()
            .status(StatusCode::OK)
            .body(Body::from(body))
            .unwrap();
    }
    Response::builder()
        .status(StatusCode::NOT_FOUND)
        .body(Body::empty())
        .unwrap()
}

fn json_response(value: serde_json::Value) -> Response<Body> {
    Response::builder()
        .status(StatusCode::OK)
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
            let (socket, address) = self.tcp.accept().await.unwrap();
            if let Ok(stream) = self.tls.accept(socket).await {
                return (stream, address);
            }
        }
    }

    fn local_addr(&self) -> std::io::Result<Self::Addr> {
        self.tcp.local_addr()
    }
}

struct Fixture {
    _temp: TempDir,
    token: PathBuf,
    state: Arc<ApiState>,
    client: InputKubernetesClient,
    stop: CancellationToken,
    server: tokio::task::JoinHandle<()>,
}

impl Fixture {
    async fn new() -> Self {
        let _ = rustls::crypto::ring::default_provider().install_default();
        let temp = TempDir::new().unwrap();
        let api_dir = temp.path().join("api");
        fs::create_dir(&api_dir).unwrap();
        fs::set_permissions(&api_dir, fs::Permissions::from_mode(0o700)).unwrap();
        let CertifiedKey { cert, signing_key } =
            generate_simple_self_signed(vec!["127.0.0.1".into()]).unwrap();
        let ca = api_dir.join("ca.pem");
        fs::write(&ca, cert.pem()).unwrap();
        let token = api_dir.join("token");
        fs::write(&token, b"first-synthetic-token\n").unwrap();
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
        let state = Arc::new(ApiState::new());
        let app = Router::new()
            .route("/{*path}", get(api_get))
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
            NAMESPACE.into(),
            token.clone(),
            &ca,
        )
        .unwrap();
        Self {
            _temp: temp,
            token,
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
async fn reads_exact_paths_with_rotating_token_and_bounded_log_parameters() {
    let fixture = Fixture::new().await;
    let job = fixture.client.get_job(NAMESPACE, JOB).await.unwrap();
    assert_eq!(job["metadata"]["name"], JOB);
    fs::write(&fixture.token, b"rotated-synthetic-token\n").unwrap();
    *fixture.state.expected_token.lock().unwrap() = "rotated-synthetic-token".into();
    let pod = fixture
        .client
        .get_controlled_pod(NAMESPACE, JOB)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(pod["metadata"]["name"], POD);
    assert_eq!(
        fixture
            .client
            .helper_stdout(NAMESPACE, POD, "custodian")
            .await
            .unwrap(),
        b"sealed-receipt\n"
    );
    assert_eq!(
        *fixture.state.requests.lock().unwrap(),
        vec![
            (
                format!("/apis/batch/v1/namespaces/{NAMESPACE}/jobs/{JOB}"),
                None,
                "Bearer first-synthetic-token".into(),
            ),
            (
                format!("/api/v1/namespaces/{NAMESPACE}/pods"),
                Some(
                    "labelSelector=batch.kubernetes.io%2Fjob-name%3Dinput-copy-run-1&limit=2"
                        .into()
                ),
                "Bearer rotated-synthetic-token".into(),
            ),
            (
                format!("/api/v1/namespaces/{NAMESPACE}/pods/{POD}/log"),
                Some(
                    "container=custodian&limitBytes=1048577&timestamps=false&previous=false".into()
                ),
                "Bearer rotated-synthetic-token".into(),
            ),
        ]
    );
    fixture.close().await;
}

#[tokio::test]
async fn rejects_redirect_ambiguous_pods_and_oversized_logs() {
    let fixture = Fixture::new().await;
    *fixture.state.mode.lock().unwrap() = Mode::RedirectJob;
    assert_eq!(
        fixture
            .client
            .get_job(NAMESPACE, JOB)
            .await
            .unwrap_err()
            .to_string(),
        "input_kubernetes_job_unavailable"
    );
    assert_eq!(fixture.state.requests.lock().unwrap().len(), 1);
    *fixture.state.mode.lock().unwrap() = Mode::OversizedJob;
    assert_eq!(
        fixture
            .client
            .get_job(NAMESPACE, JOB)
            .await
            .unwrap_err()
            .to_string(),
        "input_kubernetes_job_unavailable"
    );
    *fixture.state.mode.lock().unwrap() = Mode::AmbiguousPods;
    assert_eq!(
        fixture
            .client
            .get_controlled_pod(NAMESPACE, JOB)
            .await
            .unwrap_err()
            .to_string(),
        "input_kubernetes_pod_ambiguous"
    );
    *fixture.state.mode.lock().unwrap() = Mode::OversizedLog;
    assert_eq!(
        fixture
            .client
            .helper_stdout(NAMESPACE, POD, "custodian")
            .await
            .unwrap_err()
            .to_string(),
        "input_kubernetes_log_unavailable"
    );
    fixture.close().await;
}

#[tokio::test]
async fn rejects_unbound_references_before_any_request() {
    let fixture = Fixture::new().await;
    assert!(
        fixture
            .client
            .get_job("other-namespace", JOB)
            .await
            .is_err()
    );
    assert!(
        fixture
            .client
            .get_controlled_pod(NAMESPACE, "Not-Dns")
            .await
            .is_err()
    );
    assert!(
        fixture
            .client
            .helper_stdout(NAMESPACE, POD, "runner")
            .await
            .is_err()
    );
    assert!(fixture.state.requests.lock().unwrap().is_empty());
    fixture.close().await;
}
