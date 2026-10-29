//! Private, run-scoped output transfer. The broker owns task identity and custody;
//! this adapter moves original bytes between independently mounted volumes.
use super::{
    admission::open_bounded_record,
    artifact_retainer::{ExportFile, ExportLimits, StreamArtifactRetainer, open_selected_file},
    model::ClaimedTask,
    sandbox::{ArtifactRetainer, RetainedArtifact, SandboxBinding, SandboxHandle, valid_artifact},
    terminal_exports::export_selection,
};
use anyhow::{Result, anyhow, ensure};
use async_trait::async_trait;
use axum::{
    Router,
    body::Body,
    extract::{Path as RoutePath, State},
    http::{HeaderMap, StatusCode, header},
    response::Response,
    routing::get,
    serve::Listener,
};
use futures_util::TryStreamExt;
use reqwest::{Client, Url};
use rustls::{
    RootCertStore, ServerConfig,
    pki_types::{CertificateDer, PrivateKeyDer, pem::PemObject},
};
use serde::Deserialize;
use sha2::{Digest, Sha256};
use std::{
    collections::HashSet,
    future::Future,
    io::Read,
    net::SocketAddr,
    path::{Component, Path, PathBuf},
    pin::Pin,
    sync::Arc,
    task::{Context, Poll},
    time::Duration,
};
use tokio::{
    io::{AsyncRead, AsyncReadExt, AsyncWrite, ReadBuf},
    net::{TcpListener, TcpStream},
    sync::{OwnedSemaphorePermit, Semaphore},
};
use tokio_rustls::{TlsAcceptor, server::TlsStream};
use tokio_util::{
    io::{ReaderStream, StreamReader},
    sync::CancellationToken,
};
use uuid::Uuid;

const MANIFEST_LIMIT: u64 = 16 * 1024 * 1024;
const CREDENTIAL_LIMIT: u64 = 64 * 1024;

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RemoteExportConfig {
    pub dns_suffix: String,
    pub port: u16,
    pub credential_root: PathBuf,
    pub writer_uid: u32,
}

impl RemoteExportConfig {
    pub fn validate(&self) -> Result<()> {
        ensure!(
            dns_name(&self.dns_suffix) && self.port != 0,
            "export_transport_endpoint_invalid"
        );
        absolute_root(&self.credential_root)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ExportScope {
    pub task_id: Uuid,
    pub attempt: i32,
    pub lease_generation: i64,
}

impl ExportScope {
    pub fn new(task_id: Uuid, attempt: i32, lease_generation: i64) -> Result<Self> {
        ensure!(attempt > 0 && lease_generation > 0, "export_scope_invalid");
        Ok(Self {
            task_id,
            attempt,
            lease_generation,
        })
    }

    pub fn credential_name(&self) -> String {
        format!(
            "{}-{}-{}",
            self.task_id, self.attempt, self.lease_generation
        )
    }

    pub fn service_name(&self, namespace: &str) -> Result<String> {
        ensure!(dns_label(namespace), "export_namespace_invalid");
        let input = format!("{namespace}/{}", self.credential_name());
        Ok(format!(
            "export-{}",
            &hex::encode(Sha256::digest(input.as_bytes()))[..40]
        ))
    }

    fn prefix(&self) -> String {
        format!(
            "/v1/exports/{}/{}/{}",
            self.task_id, self.attempt, self.lease_generation
        )
    }

    fn matches(&self, task: &str, attempt: i32, generation: i64) -> bool {
        task == self.task_id.to_string()
            && attempt == self.attempt
            && generation == self.lease_generation
    }
}

#[derive(Clone)]
pub struct ExportClient {
    client: Client,
    origin: Url,
    scope: ExportScope,
}

impl ExportClient {
    pub fn for_task(
        config: &RemoteExportConfig,
        task: &ClaimedTask,
        namespace: &str,
    ) -> Result<Self> {
        config.validate()?;
        let scope = ExportScope::new(task.id, task.attempt, task.lease_generation)?;
        let service = scope.service_name(namespace)?;
        let origin = Url::parse(&format!(
            "https://{service}.{namespace}.{}:{}/",
            config.dns_suffix, config.port
        ))
        .map_err(|_| anyhow!("export_transport_endpoint_invalid"))?;
        Self::new(
            origin,
            &config.credential_root.join(scope.credential_name()),
            config.writer_uid,
            scope,
        )
    }

    /// Operator-only constructor. Do not accept these values from an agent tool
    /// request. Credentials contain a dedicated run client identity and trust root.
    pub fn new(
        origin: Url,
        credentials: &Path,
        writer_uid: u32,
        scope: ExportScope,
    ) -> Result<Self> {
        ensure!(
            origin.scheme() == "https"
                && origin.host_str().is_some()
                && origin.username().is_empty()
                && origin.password().is_none()
                && origin.path() == "/"
                && origin.query().is_none()
                && origin.fragment().is_none(),
            "export_transport_endpoint_invalid"
        );
        ExportScope::new(scope.task_id, scope.attempt, scope.lease_generation)?;
        let _ = rustls::crypto::ring::default_provider().install_default();
        let ca = read_private(credentials, "ca.pem", writer_uid, CREDENTIAL_LIMIT)?;
        let identity = read_private(credentials, "client.pem", writer_uid, CREDENTIAL_LIMIT)?;
        let roots = reqwest::Certificate::from_pem_bundle(&ca)
            .map_err(|_| anyhow!("export_trust_invalid"))?;
        ensure!(!roots.is_empty(), "export_trust_invalid");
        let identity = reqwest::Identity::from_pem(&identity)
            .map_err(|_| anyhow!("export_identity_invalid"))?;
        let client = Client::builder()
            .tls_certs_only(roots)
            .identity(identity)
            .https_only(true)
            .no_proxy()
            .redirect(reqwest::redirect::Policy::none())
            .connect_timeout(Duration::from_secs(5))
            .read_timeout(Duration::from_secs(30))
            .pool_max_idle_per_host(1)
            .build()
            .map_err(|_| anyhow!("export_client_invalid"))?;
        Ok(Self {
            client,
            origin,
            scope,
        })
    }

    fn url(&self, suffix: &str) -> Result<Url> {
        self.origin
            .join(&format!("{}{suffix}", self.scope.prefix()))
            .map_err(|_| anyhow!("export_transport_endpoint_invalid"))
    }

    /// `None` means the short-lived export service has not become ready yet.
    /// The direct driver supplies the overall deadline and lease fence.
    pub async fn manifest(&self, cancellation: &CancellationToken) -> Result<Option<Vec<u8>>> {
        let request = self.client.get(self.url("/manifest")?);
        let response = tokio::select! {
            _ = cancellation.cancelled() => return Err(anyhow!("export_cancelled")),
            response = request.send() => response,
        };
        let mut response = match response {
            Ok(response) => response,
            Err(error) if error.is_connect() || error.is_timeout() => return Ok(None),
            Err(_) => return Err(anyhow!("export_manifest_unavailable")),
        };
        if matches!(
            response.status(),
            StatusCode::NOT_FOUND | StatusCode::SERVICE_UNAVAILABLE
        ) {
            return Ok(None);
        }
        ensure!(
            response.status() == StatusCode::OK,
            "export_manifest_refused"
        );
        ensure!(
            response
                .content_length()
                .is_none_or(|length| length <= MANIFEST_LIMIT),
            "export_manifest_too_large"
        );
        let mut bytes = Vec::new();
        loop {
            let chunk = tokio::select! {
                _ = cancellation.cancelled() => return Err(anyhow!("export_cancelled")),
                chunk = response.chunk() => chunk.map_err(|_| anyhow!("export_manifest_unavailable"))?,
            };
            let Some(chunk) = chunk else {
                break;
            };
            ensure!(
                (bytes.len() as u64).saturating_add(chunk.len() as u64) <= MANIFEST_LIMIT,
                "export_manifest_too_large"
            );
            bytes.extend_from_slice(&chunk);
        }
        Ok(Some(bytes))
    }
}

pub struct HttpsArtifactRetainer {
    client: ExportClient,
    manifest_digest: String,
    retainer: StreamArtifactRetainer,
}

impl std::fmt::Debug for HttpsArtifactRetainer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HttpsArtifactRetainer")
            .finish_non_exhaustive()
    }
}

impl HttpsArtifactRetainer {
    #[allow(clippy::too_many_arguments)]
    pub fn for_task(
        client: ExportClient,
        manifest: &[u8],
        task: &ClaimedTask,
        binding: &SandboxBinding,
        handle: &SandboxHandle,
        durable_artifacts: PathBuf,
        files: Vec<ExportFile>,
        limits: ExportLimits,
    ) -> Result<Self> {
        ensure!(
            client.scope == ExportScope::new(task.id, task.attempt, task.lease_generation)?,
            "export_scope_mismatch"
        );
        ensure!(
            manifest.len() as u64 <= MANIFEST_LIMIT,
            "export_manifest_too_large"
        );
        Ok(Self {
            client,
            manifest_digest: format!("\"{}\"", hex::encode(Sha256::digest(manifest))),
            retainer: StreamArtifactRetainer::for_task(
                task,
                binding,
                handle,
                durable_artifacts,
                files,
                limits,
            )?,
        })
    }
}

#[async_trait]
impl ArtifactRetainer for HttpsArtifactRetainer {
    async fn retain(
        &self,
        handle: &SandboxHandle,
        cancellation: &CancellationToken,
    ) -> Result<Vec<RetainedArtifact>> {
        self.retainer.check_handle(handle)?;
        let mut retained = Vec::with_capacity(self.retainer.files().len());
        for (index, file) in self.retainer.files().iter().enumerate() {
            let request = self
                .client
                .client
                .get(self.client.url(&format!("/files/{index}"))?)
                .header(header::IF_MATCH, &self.manifest_digest);
            let response = tokio::select! {
                _ = cancellation.cancelled() => return Err(anyhow!("export_cancelled")),
                response = request.send() => response.map_err(|_| anyhow!("export_file_unavailable"))?,
            };
            ensure!(response.status() == StatusCode::OK, "export_file_refused");
            ensure!(
                response.content_length() == Some(file.size_bytes),
                "export_size_mismatch"
            );
            let stream = response
                .bytes_stream()
                .map_err(|_| std::io::Error::other("export_stream_failed"));
            let artifact = self
                .retainer
                .retain_stream(index, StreamReader::new(stream), cancellation)
                .await?;
            tracing::info!(event = "export_file_retained", task_id = %self.client.scope.task_id,
                attempt = self.client.scope.attempt, lease_generation = self.client.scope.lease_generation,
                file_index = index, size_bytes = file.size_bytes);
            retained.push(artifact);
        }
        ensure!(!cancellation.is_cancelled(), "export_cancelled");
        Ok(retained)
    }
    fn retained_root(&self) -> Option<&Path> {
        Some(self.retainer.durable_root())
    }
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ExportServerConfig {
    pub schema_version: String,
    pub bind: SocketAddr,
    pub record_path: PathBuf,
    pub artifacts_root: PathBuf,
    pub credential_root: PathBuf,
    pub writer_uid: u32,
    pub client_sha256: String,
    pub max_files: usize,
    pub max_bytes: u64,
    pub max_seconds: u64,
}

pub struct ExportServer {
    bind: SocketAddr,
    acceptor: TlsAcceptor,
    client_fingerprint: String,
    state: Arc<ExportState>,
    maximum_lifetime: Duration,
}

struct ExportState {
    scope: ExportScope,
    manifest: Vec<u8>,
    digest: String,
    artifacts_root: PathBuf,
    files: Vec<ExportFile>,
}

impl ExportServer {
    /// Configuration and terminal record are supplied by the trusted custodian,
    /// after observed termination and removal of all output writers.
    pub fn load(config_path: &Path, writer_uid: u32) -> Result<Self> {
        let (root, name) = split_private_path(config_path)?;
        let bytes = read_private(root, name, writer_uid, CREDENTIAL_LIMIT)?;
        let config: ExportServerConfig =
            serde_json::from_slice(&bytes).map_err(|_| anyhow!("export_config_invalid"))?;
        Self::new(config)
    }

    pub fn new(config: ExportServerConfig) -> Result<Self> {
        ensure!(
            config.schema_version == "sandbox.enrichedlab.org/export-server/v1",
            "export_config_invalid"
        );
        ensure!(
            config.max_seconds > 0 && config.max_seconds <= 86_400,
            "export_lifetime_invalid"
        );
        ensure!(
            config.max_files > 0
                && config.max_files <= 100_000
                && config.max_bytes > 0
                && config.max_bytes <= i64::MAX as u64,
            "export_limits_invalid"
        );
        ensure!(
            config.client_sha256.len() == 64
                && config
                    .client_sha256
                    .bytes()
                    .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)),
            "export_client_fingerprint_invalid"
        );
        absolute_root(&config.artifacts_root)?;
        absolute_root(&config.credential_root)?;
        let (root, name) = split_private_path(&config.record_path)?;
        for private in [root, config.credential_root.as_path()] {
            ensure!(
                !private.starts_with(&config.artifacts_root)
                    && !config.artifacts_root.starts_with(private),
                "export_roots_overlap"
            );
        }
        let manifest = read_private(root, name, config.writer_uid, MANIFEST_LIMIT)?;
        let (scope, files) = export_selection(&manifest)?;
        ensure!(
            !files.is_empty() && files.len() <= config.max_files,
            "export_limits_invalid"
        );
        let mut paths = HashSet::new();
        let mut total = 0_u64;
        for file in &files {
            ensure!(paths.insert(&file.relative_path), "export_duplicate_path");
            total = total
                .checked_add(file.size_bytes)
                .ok_or_else(|| anyhow!("export_size_overflow"))?;
            ensure!(
                total <= config.max_bytes && file.size_bytes <= i64::MAX as u64,
                "export_limits_exceeded"
            );
            valid_artifact(&RetainedArtifact {
                visibility: file.visibility.clone(),
                role: file.role.clone(),
                label: file.label.clone(),
                relative_path: file.relative_path.clone(),
                media_type: file.media_type.clone(),
                size_bytes: file.size_bytes as i64,
                sha256: file.sha256.clone(),
            })?;
            open_selected_file(&config.artifacts_root, file)?;
        }
        let _ = rustls::crypto::ring::default_provider().install_default();
        let ca = read_private(
            &config.credential_root,
            "ca.pem",
            config.writer_uid,
            CREDENTIAL_LIMIT,
        )?;
        let certificate = read_private(
            &config.credential_root,
            "server.pem",
            config.writer_uid,
            CREDENTIAL_LIMIT,
        )?;
        let key = read_private(
            &config.credential_root,
            "server.key",
            config.writer_uid,
            CREDENTIAL_LIMIT,
        )?;
        let mut roots = RootCertStore::empty();
        for certificate in CertificateDer::pem_slice_iter(&ca) {
            roots
                .add(certificate.map_err(|_| anyhow!("export_trust_invalid"))?)
                .map_err(|_| anyhow!("export_trust_invalid"))?;
        }
        let verifier = rustls::server::WebPkiClientVerifier::builder(Arc::new(roots))
            .build()
            .map_err(|_| anyhow!("export_trust_invalid"))?;
        let certificates = CertificateDer::pem_slice_iter(&certificate)
            .collect::<std::result::Result<Vec<_>, _>>()
            .map_err(|_| anyhow!("export_identity_invalid"))?;
        let key =
            PrivateKeyDer::from_pem_slice(&key).map_err(|_| anyhow!("export_identity_invalid"))?;
        let mut tls = ServerConfig::builder()
            .with_client_cert_verifier(verifier)
            .with_single_cert(certificates, key)
            .map_err(|_| anyhow!("export_identity_invalid"))?;
        tls.alpn_protocols = vec![b"http/1.1".to_vec()];
        Ok(Self {
            bind: config.bind,
            acceptor: TlsAcceptor::from(Arc::new(tls)),
            client_fingerprint: config.client_sha256,
            state: Arc::new(ExportState {
                scope,
                digest: format!("\"{}\"", hex::encode(Sha256::digest(&manifest))),
                manifest,
                artifacts_root: config.artifacts_root,
                files,
            }),
            maximum_lifetime: Duration::from_secs(config.max_seconds),
        })
    }

    pub async fn serve(self, cancellation: CancellationToken) -> Result<()> {
        let listener = TcpListener::bind(self.bind)
            .await
            .map_err(|_| anyhow!("export_listener_unavailable"))?;
        self.serve_listener(listener, cancellation).await
    }

    pub async fn serve_listener(
        self,
        listener: TcpListener,
        cancellation: CancellationToken,
    ) -> Result<()> {
        let listener = AuthenticatedListener {
            listener,
            acceptor: self.acceptor,
            fingerprint: self.client_fingerprint,
            slots: Arc::new(Semaphore::new(4)),
            cancellation: cancellation.clone(),
        };
        let router = Router::new()
            .route(
                "/v1/exports/{task}/{attempt}/{generation}/manifest",
                get(manifest_response),
            )
            .route(
                "/v1/exports/{task}/{attempt}/{generation}/files/{index}",
                get(file_response),
            )
            .with_state(self.state);
        let shutdown = cancellation.clone();
        let lifetime = self.maximum_lifetime;
        axum::serve(listener, router).with_graceful_shutdown(async move {
            tokio::select! { _ = shutdown.cancelled() => {}, _ = tokio::time::sleep(lifetime) => {} }
            shutdown.cancel();
        }).await.map_err(|_| anyhow!("export_server_failed"))
    }
}

async fn manifest_response(
    State(state): State<Arc<ExportState>>,
    RoutePath((task, attempt, generation)): RoutePath<(String, i32, i64)>,
) -> Response {
    if !state.scope.matches(&task, attempt, generation) {
        return response(StatusCode::NOT_FOUND);
    }
    Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, "application/json")
        .header(header::ETAG, &state.digest)
        .header(header::CACHE_CONTROL, "no-store")
        .header(header::CONTENT_LENGTH, state.manifest.len())
        .body(Body::from(state.manifest.clone()))
        .unwrap()
}

async fn file_response(
    State(state): State<Arc<ExportState>>,
    RoutePath((task, attempt, generation, index)): RoutePath<(String, i32, i64, usize)>,
    headers: HeaderMap,
) -> Response {
    if !state.scope.matches(&task, attempt, generation) {
        return response(StatusCode::NOT_FOUND);
    }
    if headers
        .get(header::IF_MATCH)
        .and_then(|value| value.to_str().ok())
        != Some(&state.digest)
    {
        return response(StatusCode::PRECONDITION_FAILED);
    }
    let Some(file) = state.files.get(index) else {
        return response(StatusCode::NOT_FOUND);
    };
    let root = state.artifacts_root.clone();
    let selected = file.clone();
    let input =
        match tokio::task::spawn_blocking(move || open_selected_file(&root, &selected)).await {
            Ok(Ok(file)) => file,
            _ => return response(StatusCode::CONFLICT),
        };
    let reader = tokio::fs::File::from_std(input).take(file.size_bytes);
    Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, "application/octet-stream")
        .header(header::CACHE_CONTROL, "no-store")
        .header(header::CONTENT_LENGTH, file.size_bytes)
        .body(Body::from_stream(ReaderStream::with_capacity(
            reader,
            64 * 1024,
        )))
        .unwrap()
}

fn response(status: StatusCode) -> Response {
    Response::builder()
        .status(status)
        .body(Body::empty())
        .unwrap()
}

struct AuthenticatedListener {
    listener: TcpListener,
    acceptor: TlsAcceptor,
    fingerprint: String,
    slots: Arc<Semaphore>,
    cancellation: CancellationToken,
}

struct ExportConnection {
    stream: TlsStream<TcpStream>,
    _permit: OwnedSemaphorePermit,
    stop: Pin<Box<dyn Future<Output = ()> + Send>>,
}

impl Listener for AuthenticatedListener {
    type Io = ExportConnection;
    type Addr = SocketAddr;
    async fn accept(&mut self) -> (Self::Io, Self::Addr) {
        loop {
            let permit = self
                .slots
                .clone()
                .acquire_owned()
                .await
                .expect("export slots remain open");
            let (socket, address) = match self.listener.accept().await {
                Ok(connection) => connection,
                Err(_) => {
                    tokio::time::sleep(Duration::from_millis(100)).await;
                    continue;
                }
            };
            let stream =
                match tokio::time::timeout(Duration::from_secs(3), self.acceptor.accept(socket))
                    .await
                {
                    Ok(Ok(stream)) => stream,
                    _ => {
                        tracing::warn!(
                            event = "export_client_denied",
                            reason = "tls_verification_failed"
                        );
                        continue;
                    }
                };
            let Some(leaf) = stream
                .get_ref()
                .1
                .peer_certificates()
                .and_then(|chain| chain.first())
            else {
                continue;
            };
            if hex::encode(Sha256::digest(leaf.as_ref())) != self.fingerprint {
                tracing::warn!(
                    event = "export_client_denied",
                    reason = "client_fingerprint_mismatch"
                );
                continue;
            }
            return (
                ExportConnection {
                    stream,
                    _permit: permit,
                    stop: Box::pin(self.cancellation.clone().cancelled_owned()),
                },
                address,
            );
        }
    }
    fn local_addr(&self) -> std::io::Result<Self::Addr> {
        self.listener.local_addr()
    }
}

impl AsyncRead for ExportConnection {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buffer: &mut ReadBuf<'_>,
    ) -> Poll<std::io::Result<()>> {
        if self.stop.as_mut().poll(cx).is_ready() {
            return Poll::Ready(Err(std::io::Error::other("export_stopped")));
        }
        Pin::new(&mut self.stream).poll_read(cx, buffer)
    }
}
impl AsyncWrite for ExportConnection {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        bytes: &[u8],
    ) -> Poll<std::io::Result<usize>> {
        if self.stop.as_mut().poll(cx).is_ready() {
            return Poll::Ready(Err(std::io::Error::other("export_stopped")));
        }
        Pin::new(&mut self.stream).poll_write(cx, bytes)
    }
    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        Pin::new(&mut self.stream).poll_flush(cx)
    }
    fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        Pin::new(&mut self.stream).poll_shutdown(cx)
    }
}

fn read_private(root: &Path, name: &str, uid: u32, limit: u64) -> Result<Vec<u8>> {
    absolute_root(root)?;
    let file = open_bounded_record(root, name, uid, limit)
        .map_err(|_| anyhow!("export_private_file_unavailable"))?;
    let mut bytes = Vec::new();
    file.take(limit + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| anyhow!("export_private_file_unavailable"))?;
    ensure!(bytes.len() as u64 <= limit, "export_private_file_too_large");
    Ok(bytes)
}

fn split_private_path(path: &Path) -> Result<(&Path, &str)> {
    absolute_root(path)?;
    Ok((
        path.parent()
            .ok_or_else(|| anyhow!("export_path_invalid"))?,
        path.file_name()
            .and_then(|name| name.to_str())
            .ok_or_else(|| anyhow!("export_path_invalid"))?,
    ))
}

fn absolute_root(path: &Path) -> Result<()> {
    ensure!(
        path.is_absolute()
            && path != Path::new("/")
            && path
                .components()
                .all(|part| matches!(part, Component::RootDir | Component::Normal(_))),
        "export_root_invalid"
    );
    Ok(())
}
fn dns_label(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 63
        && value.starts_with(|c: char| c.is_ascii_alphanumeric())
        && value.ends_with(|c: char| c.is_ascii_alphanumeric())
        && value
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
}
fn dns_name(value: &str) -> bool {
    value.len() <= 180 && value.split('.').all(dns_label)
}
