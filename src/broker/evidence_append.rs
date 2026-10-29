//! The evidence append service: producers outside the broker (a sandbox runtime's receipt service, the platform, the
//! knowledge pipeline, operations agents) add records to the research record over gRPC
//! (`enrichedlab.evidence.v1.EvidenceAppendService`).
//!
//! Each call carries a bound Kubernetes ServiceAccount token for the service's audience. A TokenReview proves the
//! account, and the private producers file maps it to exactly one evidence producer; the request names no producer.
//! The service then appends through `evidence.append` as its database role, which the record accepts only for the
//! producers bound to that role. Refusals keep the record's reason in `google.rpc.ErrorInfo`; database messages are
//! never returned. An identical retry under the same idempotency key returns the original receipt, so a producer that
//! lost an answer retries safely.
use std::{
    collections::HashMap,
    fs,
    net::SocketAddr,
    path::PathBuf,
    pin::Pin,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    task::{Context as TaskContext, Poll},
    time::Duration,
};

use anyhow::{Context, Result, anyhow, bail};
use clap::Args;
use deadpool_postgres::Pool;
use futures_util::{Stream, StreamExt};
use opentelemetry::{KeyValue, metrics::Counter};
use time::{OffsetDateTime, format_description::well_known::Rfc3339};
use tokio::{
    io::{AsyncRead, AsyncWrite, ReadBuf},
    net::{TcpListener, TcpStream},
    sync::{OwnedSemaphorePermit, Semaphore},
};
use tonic::codegen::http;
use tonic::{
    Code, Request, Response, Status,
    metadata::MetadataMap,
    transport::{
        Certificate, Channel, ClientTlsConfig, Identity, Server, ServerTlsConfig,
        server::{Connected, TcpConnectInfo},
    },
};
use tonic_types::{ErrorDetails, StatusExt};
use uuid::Uuid;

use super::auth::{AuthError, EvidenceProducerAuth};
use super::evidence_custody::{self, Custody};
use super::evidence_telemetry::{self, RpcTelemetryLayer};

#[allow(clippy::all, clippy::pedantic)]
pub mod pb {
    tonic::include_proto!("enrichedlab.evidence.v1");
}

use pb::evidence_append_service_server::{EvidenceAppendService, EvidenceAppendServiceServer};
use pb::{
    AppendRequest, AppendResponse, AwaitClaimRequest, AwaitClaimResponse, ScopeKind,
    StoreObjectRequest, StoreObjectResponse,
};

const MAX_PAYLOAD_BYTES: usize = 65_536;
const ERROR_DOMAIN: &str = "evidence.enrichedlab.org";
/// The evidence schema layouts this service appends to: 5 (migrations 075 to 078) and 6 (079, with custody). Custody
/// calls need layout 6.
const EVIDENCE_LAYOUTS: [&str; 2] = [
    "enrichedlab evidence layout 5",
    "enrichedlab evidence layout 6",
];
const CUSTODY_LAYOUT: &str = "enrichedlab evidence layout 6";
/// Months of partitions kept ahead of the current one; migrations create six, the service keeps at least this many.
const PARTITION_MONTHS_AHEAD: i32 = 3;
#[derive(Clone, Debug, Args)]
pub struct EvidenceAppendArgs {
    /// Address to serve gRPC with TLS on.
    #[arg(
        long,
        env = "AGENT_SANDBOX_EVIDENCE_LISTEN",
        default_value = "0.0.0.0:8443"
    )]
    pub listen: SocketAddr,

    /// Server certificate chain (PEM) from the internal CA.
    #[arg(long, env = "AGENT_SANDBOX_EVIDENCE_TLS_CERT")]
    pub tls_cert: PathBuf,

    /// Server private key (PEM).
    #[arg(long, env = "AGENT_SANDBOX_EVIDENCE_TLS_KEY")]
    pub tls_key: PathBuf,

    /// Private JSON mapping of ServiceAccounts to evidence producers, with the TokenReview settings.
    #[arg(long, env = "AGENT_SANDBOX_EVIDENCE_PRODUCERS_FILE")]
    pub producers_file: PathBuf,

    /// Database DSN environment variable name. Use the evidence adapter's database role.
    #[arg(
        long,
        env = "AGENT_SANDBOX_BROKER_DATABASE_DSN_ENV",
        default_value = "AGENT_SANDBOX_BROKER_DATABASE_DSN"
    )]
    pub database_dsn_env: String,

    /// Preferred mounted Secret path containing the adapter's database DSN.
    #[arg(long, env = "AGENT_SANDBOX_BROKER_DATABASE_DSN_FILE")]
    pub database_dsn_file: Option<PathBuf>,

    /// The site tenant this instance appends for; a request for another tenant is refused.
    #[arg(long, env = "AGENT_SANDBOX_BROKER_TENANT")]
    pub tenant: String,

    #[arg(
        long,
        env = "AGENT_SANDBOX_EVIDENCE_DATABASE_POOL",
        default_value_t = 8
    )]
    pub database_pool: usize,

    /// Open connections the service keeps at once; a connection beyond it is closed on arrival.
    #[arg(
        long,
        env = "AGENT_SANDBOX_EVIDENCE_MAX_CONNECTIONS",
        default_value_t = 256
    )]
    pub max_connections: usize,

    /// Calls in progress across all connections; a call beyond it is refused at once (RESOURCE_EXHAUSTED).
    #[arg(
        long,
        env = "AGENT_SANDBOX_EVIDENCE_MAX_IN_FLIGHT",
        default_value_t = 64
    )]
    pub max_in_flight: usize,

    /// Private JSON naming the object-store endpoints custody writes to, with their identities and CA. Without it the
    /// service appends only, and StoreObject and AwaitClaim are refused.
    #[arg(long, env = "AGENT_SANDBOX_EVIDENCE_CUSTODY_FILE")]
    pub custody_file: Option<PathBuf>,

    /// StoreObject and AwaitClaim calls in progress at once; each may hold one object (at most 64 MiB) in memory.
    #[arg(
        long,
        env = "AGENT_SANDBOX_EVIDENCE_CUSTODY_IN_FLIGHT",
        default_value_t = 4
    )]
    pub custody_in_flight: usize,

    /// Seconds between runs of the capture recorder, which moves the broker's logged changes into the record
    /// (`evidence.capture_record`); 0 turns it off. A database without the capture log is skipped.
    #[arg(
        long,
        env = "AGENT_SANDBOX_EVIDENCE_CAPTURE_INTERVAL_SECONDS",
        default_value_t = 5
    )]
    pub capture_interval_seconds: u64,
}

#[derive(Clone, Debug, Args)]
pub struct EvidenceProbeArgs {
    /// The service's address, as its certificate names it.
    #[arg(long, default_value = "https://127.0.0.1:8443")]
    pub address: String,

    /// Name the server certificate must carry.
    #[arg(long)]
    pub server_name: String,

    /// CA bundle (PEM) that signed the server certificate.
    #[arg(long)]
    pub ca_file: PathBuf,
}

struct AppendService {
    pool: Pool,
    auth: EvidenceProducerAuth,
    tenant: String,
    reviews: Counter<u64>,
    custody: Option<Arc<Custody>>,
}

/// A refusal with the record's reason as ErrorInfo; the message is the reason too, never a database message.
pub(crate) fn refusal(code: Code, reason: &str) -> Status {
    let reason = reason.to_ascii_uppercase();
    Status::with_error_details(
        code,
        reason.to_ascii_lowercase(),
        ErrorDetails::with_error_info(reason, ERROR_DOMAIN, HashMap::<String, String>::new()),
    )
}

fn auth_status(error: AuthError) -> Status {
    match error {
        AuthError::ServiceAccountUnavailable => {
            refusal(Code::Unavailable, "producer_review_unavailable")
        }
        _ => refusal(Code::Unauthenticated, "producer_not_authenticated"),
    }
}

/// Maps a database refusal raised as `evidence:<reason>` to its gRPC code; anything else is internal or unavailable.
pub(crate) fn database_status(error: tokio_postgres::Error) -> Status {
    let Some(db) = error.as_db_error() else {
        return refusal(Code::Unavailable, "record_unavailable");
    };
    let Some(reason) = db.message().strip_prefix("evidence:") else {
        return refusal(Code::Internal, "record_error");
    };
    if !reason
        .bytes()
        .all(|byte| byte.is_ascii_lowercase() || byte == b'_')
        || reason.len() > 64
    {
        return refusal(Code::Internal, "record_error");
    }
    let code = match (db.code().code(), reason) {
        (_, "tenant_disabled") => Code::FailedPrecondition,
        ("42501", _) => Code::PermissionDenied,
        ("22023", _) => Code::InvalidArgument,
        ("23505", _) => Code::AlreadyExists,
        ("55000", _) => Code::FailedPrecondition,
        _ => Code::Internal,
    };
    refusal(code, reason)
}

fn bearer(metadata: &MetadataMap) -> Result<&str, Status> {
    metadata
        .get("authorization")
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.strip_prefix("Bearer "))
        .ok_or_else(|| refusal(Code::Unauthenticated, "producer_not_authenticated"))
}

/// The record's name for a request's scope kind.
pub(crate) fn scope_text(kind: i32) -> Result<&'static str, Status> {
    match ScopeKind::try_from(kind) {
        Ok(ScopeKind::Project) => Ok("project"),
        Ok(ScopeKind::Lab) => Ok("lab"),
        Ok(ScopeKind::Standalone) => Ok("standalone"),
        _ => Err(refusal(Code::InvalidArgument, "scope_invalid")),
    }
}

fn optional_uuid(value: &str) -> Result<Option<Uuid>, Status> {
    if value.is_empty() {
        return Ok(None);
    }
    Uuid::parse_str(value)
        .map(Some)
        .map_err(|_| refusal(Code::InvalidArgument, "scope_invalid"))
}

fn source_time(value: Option<prost_types::Timestamp>) -> Result<OffsetDateTime, Status> {
    let value = value.ok_or_else(|| refusal(Code::InvalidArgument, "argument_missing"))?;
    if !(0..1_000_000_000).contains(&value.nanos) {
        return Err(refusal(Code::InvalidArgument, "source_time_invalid"));
    }
    // The record keeps microseconds; finer times would make different requests look identical to a retry.
    if value.nanos % 1_000 != 0 {
        return Err(refusal(Code::InvalidArgument, "source_time_precision"));
    }
    OffsetDateTime::from_unix_timestamp(value.seconds)
        .map(|time| time + time::Duration::nanoseconds(i64::from(value.nanos)))
        .map_err(|_| refusal(Code::InvalidArgument, "source_time_invalid"))
}

fn timestamp(text: &str) -> Result<prost_types::Timestamp, Status> {
    let time = OffsetDateTime::parse(text, &Rfc3339)
        .map_err(|_| refusal(Code::Internal, "record_error"))?;
    Ok(prost_types::Timestamp {
        seconds: time.unix_timestamp(),
        nanos: i32::try_from(time.nanosecond())
            .map_err(|_| refusal(Code::Internal, "record_error"))?,
    })
}

impl AppendService {
    /// The producer the call's bound ServiceAccount token maps to, after a TokenReview.
    async fn producer(&self, metadata: &MetadataMap) -> Result<String, Status> {
        let reviewed = self.auth.producer(bearer(metadata)?).await;
        let outcome = match &reviewed {
            Ok(_) => "accepted",
            Err(AuthError::ServiceAccountUnavailable) => "unavailable",
            Err(_) => "refused",
        };
        self.reviews.add(1, &[KeyValue::new("outcome", outcome)]);
        reviewed.map_err(auth_status)
    }

    /// Custody, when it is configured and the record carries layout 6.
    async fn custody(&self) -> Result<&Custody, Status> {
        let custody = self
            .custody
            .as_deref()
            .ok_or_else(|| refusal(Code::FailedPrecondition, "custody_not_configured"))?;
        let client = self
            .pool
            .get()
            .await
            .map_err(|_| refusal(Code::Unavailable, "record_unavailable"))?;
        let layout: Option<String> = client
            .query_one(
                "SELECT obj_description(to_regnamespace('evidence'), 'pg_namespace')",
                &[],
            )
            .await
            .map_err(|_| refusal(Code::Unavailable, "record_unavailable"))?
            .get(0);
        if layout.as_deref() != Some(CUSTODY_LAYOUT) {
            return Err(refusal(Code::FailedPrecondition, "custody_not_ready"));
        }
        Ok(custody)
    }
}

#[tonic::async_trait]
impl EvidenceAppendService for AppendService {
    async fn store_object(
        &self,
        request: Request<tonic::Streaming<StoreObjectRequest>>,
    ) -> Result<Response<StoreObjectResponse>, Status> {
        let producer = self.producer(request.metadata()).await?;
        let custody = self.custody().await?;
        let claim = evidence_custody::store_object(
            &self.pool,
            custody,
            &self.tenant,
            &producer,
            request.into_inner(),
        )
        .await?;
        tracing::info!(
            target: "agent_sandbox_broker::evidence",
            producer = %producer,
            ready = claim.ready,
            replayed = claim.replayed,
            "evidence object stored"
        );
        Ok(Response::new(StoreObjectResponse { claim: Some(claim) }))
    }

    async fn await_claim(
        &self,
        request: Request<AwaitClaimRequest>,
    ) -> Result<Response<AwaitClaimResponse>, Status> {
        let producer = self.producer(request.metadata()).await?;
        // Answer before the call layer's own limit (the caller's deadline less a tenth, at most the custody limit), with
        // two seconds to spare for the answer itself.
        let limit = client_deadline_metadata(request.metadata())
            .map(|deadline| deadline.saturating_sub((deadline / 10).max(Duration::from_millis(5))))
            .map_or(CUSTODY_CALL_TIMEOUT, |deadline| {
                deadline.min(CUSTODY_CALL_TIMEOUT)
            });
        let wait = limit.saturating_sub(Duration::from_secs(2));
        let custody = self.custody().await?;
        let request = request.into_inner();
        if request.tenant_id != self.tenant {
            return Err(refusal(Code::PermissionDenied, "tenant_not_served"));
        }
        let claim = evidence_custody::await_claim(
            &self.pool,
            custody,
            &self.tenant,
            &producer,
            &request.claim_id,
            tokio::time::Instant::now() + wait,
        )
        .await?;
        Ok(Response::new(AwaitClaimResponse { claim: Some(claim) }))
    }

    async fn append(
        &self,
        request: Request<AppendRequest>,
    ) -> Result<Response<AppendResponse>, Status> {
        let producer = self.producer(request.metadata()).await?;
        let request = request.into_inner();
        if request.tenant_id != self.tenant {
            return Err(refusal(Code::PermissionDenied, "tenant_not_served"));
        }
        let scope = scope_text(request.scope_kind)?;
        let project = optional_uuid(&request.project_id)?;
        let run = optional_uuid(&request.run_id)?;
        let source_time = source_time(request.source_time)?;
        if request.payload.len() > MAX_PAYLOAD_BYTES {
            return Err(refusal(Code::InvalidArgument, "payload_size"));
        }
        let payload = String::from_utf8(request.payload)
            .map_err(|_| refusal(Code::InvalidArgument, "payload_not_json"))?;
        let client = self
            .pool
            .get()
            .await
            .map_err(|_| refusal(Code::Unavailable, "record_unavailable"))?;
        let row = client
            .query_one(
                "SELECT evidence.append($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11)::text",
                &[
                    &self.tenant,
                    &producer,
                    &request.group_key,
                    &scope,
                    &project,
                    &run,
                    &request.event_type,
                    &request.idempotency_key,
                    &source_time,
                    &payload,
                    &request.payload_sha256,
                ],
            )
            .await
            .map_err(database_status)?;
        let receipt: serde_json::Value = serde_json::from_str(row.get::<_, &str>(0))
            .map_err(|_| refusal(Code::Internal, "record_error"))?;
        let text = |name: &str| {
            receipt
                .get(name)
                .and_then(serde_json::Value::as_str)
                .map(str::to_owned)
                .ok_or_else(|| refusal(Code::Internal, "record_error"))
        };
        tracing::info!(
            target: "agent_sandbox_broker::evidence",
            producer = %producer,
            event_type = %request.event_type,
            replayed = receipt.get("replayed").and_then(serde_json::Value::as_bool).unwrap_or(false),
            "evidence record appended"
        );
        Ok(Response::new(AppendResponse {
            record_id: text("record_id")?,
            stream_id: text("stream_id")?,
            sequence: receipt
                .get("sequence")
                .and_then(serde_json::Value::as_u64)
                .ok_or_else(|| refusal(Code::Internal, "record_error"))?,
            record_hash: text("record_hash")?,
            ingested_at: Some(timestamp(&text("ingested_at")?)?),
            replayed: receipt
                .get("replayed")
                .and_then(serde_json::Value::as_bool)
                .unwrap_or(false),
        }))
    }
}

/// Serves the append service and a gRPC health service (Check only) on one TLS port until SIGTERM.
pub async fn serve(args: EvidenceAppendArgs) -> Result<()> {
    let telemetry = evidence_telemetry::install("evidence-append")?;
    let dsn = super::config::read_secret(&args.database_dsn_file, &args.database_dsn_env)?;
    let pool = super::store::build_pool(&dsn, args.database_pool)?;
    let auth = EvidenceProducerAuth::load(&args.producers_file)
        .map_err(|error| anyhow!("evidence producers configuration: {}", error.code()))?;
    let custody = match &args.custody_file {
        Some(path) => Some(Arc::new(Custody::load(path)?)),
        None => None,
    };
    let mut service = prepare(pool, auth, args.tenant).await?;
    service.custody = custody;
    let identity = Identity::from_pem(
        fs::read(&args.tls_cert).context("failed to read the server certificate")?,
        fs::read(&args.tls_key).context("failed to read the server key")?,
    );
    let listener = TcpListener::bind(args.listen)
        .await
        .with_context(|| format!("failed to listen on {}", args.listen))?;
    tracing::info!(listen = %args.listen, "evidence append service serving");
    let limits = Limits {
        connections: args.max_connections,
        in_flight: args.max_in_flight,
        other_in_flight: 16,
        readiness: Duration::from_secs(15),
        call_timeout: Duration::from_secs(15),
        handshake: Duration::from_secs(5),
        custody_in_flight: args.custody_in_flight,
        capture_every: (args.capture_interval_seconds > 0)
            .then(|| Duration::from_secs(args.capture_interval_seconds)),
    };
    let result = run(
        listener,
        identity,
        service,
        limits,
        RpcTelemetryLayer::new(),
        shutdown_signal(),
    )
    .await;
    telemetry.shutdown();
    result
}

/// What the role can do now, in one query: execute the append function on this binary's layout, the producers the
/// tenant binds to the role (NULL when the tenant is unknown or disabled), and whether this month's partition exists.
const AUTHORITY_SQL: &str = "SELECT has_function_privilege(\
     'evidence.append(text,text,text,text,uuid,uuid,text,text,timestamptz,text,text)', 'EXECUTE') \
     AND obj_description(to_regnamespace('evidence'), 'pg_namespace') = ANY($1), \
     evidence.adapter_producers($2), \
     to_regclass(format('evidence.records_y%sm%s', to_char(now() AT TIME ZONE 'UTC', 'YYYY'), \
     to_char(now() AT TIME ZONE 'UTC', 'MM'))) IS NOT NULL";

/// The authority checks startup and health share: the role can append, the schema carries this binary's layout, the
/// tenant is enabled, and every producer the configuration maps is bound to this role. Returns whether this month's
/// partition exists.
async fn authority(pool: &Pool, auth: &EvidenceProducerAuth, tenant: &str) -> Result<bool> {
    let client = pool
        .get()
        .await
        .context("the evidence record's database is unreachable")?;
    let row = client
        .query_one(AUTHORITY_SQL, &[&EVIDENCE_LAYOUTS.as_slice(), &tenant])
        .await
        .context("the evidence record's authority check failed")?;
    if !row.get::<_, bool>(0) {
        bail!("the evidence record is not ready for this role or carries another layout");
    }
    let bound: Option<Vec<String>> = row.get(1);
    let bound = bound.ok_or_else(|| anyhow!("the tenant is unknown or disabled"))?;
    let unbound = auth
        .producers()
        .filter(|producer| !bound.iter().any(|value| value == producer))
        .count();
    if unbound > 0 {
        bail!("{unbound} configured producer(s) are not bound to this database role");
    }
    Ok(row.get(2))
}

/// Startup: the authority checks, then any missing monthly partition, so the service never reports serving for
/// producers it cannot write as.
async fn prepare(pool: Pool, auth: EvidenceProducerAuth, tenant: String) -> Result<AppendService> {
    authority(&pool, &auth, &tenant).await?;
    ensure_partitions(&pool).await?;
    Ok(AppendService {
        pool,
        auth,
        tenant,
        reviews: evidence_telemetry::counter(
            evidence_telemetry::TOKEN_REVIEWS,
            "Producer token reviews by outcome",
        ),
        custody: None,
    })
}

/// Whether the service can append now: the startup authority holds, this month's partition exists, and the
/// TokenReview API answers.
async fn ready(pool: &Pool, auth: &EvidenceProducerAuth, tenant: &str) -> bool {
    let checks = async {
        matches!(authority(pool, auth, tenant).await, Ok(true)) && auth.reviewer_available().await
    };
    tokio::time::timeout(Duration::from_secs(5), checks)
        .await
        .unwrap_or(false)
}

/// Creates any missing monthly partition from this month to PARTITION_MONTHS_AHEAD months on, so appends never meet
/// a month without one between releases. Returns how many were made.
async fn ensure_partitions(pool: &Pool) -> Result<usize> {
    let made: Vec<String> = pool
        .get()
        .await
        .context("the evidence record's database is unreachable")?
        .query_one(
            "SELECT evidence.ensure_record_partitions($1)",
            &[&PARTITION_MONTHS_AHEAD],
        )
        .await
        .context("evidence partition maintenance failed")?
        .get(0);
    if !made.is_empty() {
        tracing::info!(partitions = made.len(), "evidence partitions created");
    }
    Ok(made.len())
}

/// How much the service takes on at once.
#[derive(Clone, Copy, Debug)]
struct Limits {
    connections: usize,
    /// Append calls in progress across all connections.
    in_flight: usize,
    /// Other calls (health checks, unknown paths) in progress across all connections.
    other_in_flight: usize,
    /// How often health rechecks the record and the TokenReview API.
    readiness: Duration,
    /// The longest a call may take.
    call_timeout: Duration,
    /// The longest a TLS handshake may take; a silent client holds its connection slot no longer.
    handshake: Duration,
    /// StoreObject and AwaitClaim calls in progress across all connections.
    custody_in_flight: usize,
    /// How often the capture recorder runs; None leaves it off.
    capture_every: Option<Duration>,
}

/// A TCP connection that holds one of the service's connection slots until it closes.
struct Slot {
    stream: TcpStream,
    _permit: OwnedSemaphorePermit,
}

impl Connected for Slot {
    type ConnectInfo = TcpConnectInfo;

    fn connect_info(&self) -> Self::ConnectInfo {
        self.stream.connect_info()
    }
}

impl AsyncRead for Slot {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut TaskContext<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<std::io::Result<()>> {
        Pin::new(&mut self.stream).poll_read(cx, buf)
    }
}

impl AsyncWrite for Slot {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut TaskContext<'_>,
        buf: &[u8],
    ) -> Poll<std::io::Result<usize>> {
        Pin::new(&mut self.stream).poll_write(cx, buf)
    }

    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut TaskContext<'_>) -> Poll<std::io::Result<()>> {
        Pin::new(&mut self.stream).poll_flush(cx)
    }

    fn poll_shutdown(
        mut self: Pin<&mut Self>,
        cx: &mut TaskContext<'_>,
    ) -> Poll<std::io::Result<()>> {
        Pin::new(&mut self.stream).poll_shutdown(cx)
    }
}

/// Accepted connections, each holding a slot; a connection that arrives with every slot taken is closed at once.
/// Accepted sockets get TCP keepalive, so a peer that vanishes releases its slot.
fn limited(listener: TcpListener, slots: usize) -> impl Stream<Item = std::io::Result<Slot>> {
    let semaphore = Arc::new(Semaphore::new(slots.max(1)));
    let connections: Counter<u64> =
        evidence_telemetry::counter(evidence_telemetry::CONNECTIONS, "Connections by outcome");
    tokio_stream::wrappers::TcpListenerStream::new(listener).filter_map(move |accepted| {
        let semaphore = semaphore.clone();
        let connections = connections.clone();
        async move {
            match accepted {
                Ok(stream) => match semaphore.try_acquire_owned() {
                    Ok(permit) => {
                        connections.add(1, &[KeyValue::new("outcome", "accepted")]);
                        let keepalive =
                            socket2::TcpKeepalive::new().with_time(Duration::from_secs(60));
                        let _ = socket2::SockRef::from(&stream).set_tcp_keepalive(&keepalive);
                        Some(Ok(Slot {
                            stream,
                            _permit: permit,
                        }))
                    }
                    Err(_) => {
                        connections.add(1, &[KeyValue::new("outcome", "refused_at_limit")]);
                        drop(stream);
                        None
                    }
                },
                Err(error) => Some(Err(error)),
            }
        }
    })
}

const SERVICE_NAME: &str = "enrichedlab.evidence.v1.EvidenceAppendService";
/// A health request names at most a service; anything larger is refused before it is kept.
const HEALTH_MAX_MESSAGE_BYTES: usize = 1024;
const APPEND_PATH: &str = "/enrichedlab.evidence.v1.EvidenceAppendService/Append";
const CUSTODY_PATHS: [&str; 2] = [
    "/enrichedlab.evidence.v1.EvidenceAppendService/StoreObject",
    "/enrichedlab.evidence.v1.EvidenceAppendService/AwaitClaim",
];
/// The longest a custody call may take: up to 64 MiB in, two full reads back and the replica's arrival.
const CUSTODY_CALL_TIMEOUT: Duration = Duration::from_secs(120);

/// The service-wide limits, inside the telemetry layer so every outcome is counted, and applied before a request
/// body is read: an Append call beyond the in-flight limit, or any other call (health checks, unknown paths) beyond
/// its own small budget, is refused at once with RESOURCE_EXHAUSTED; any call past its deadline answers
/// DEADLINE_EXCEEDED. Health has its own budget, so appends never fail a probe and probes never crowd out appends.
#[derive(Clone)]
struct CallLimitLayer {
    appends: Arc<Semaphore>,
    custody: Arc<Semaphore>,
    others: Arc<Semaphore>,
    timeout: Duration,
}

impl<S> tower::Layer<S> for CallLimitLayer {
    type Service = CallLimit<S>;

    fn layer(&self, inner: S) -> Self::Service {
        CallLimit {
            inner,
            appends: self.appends.clone(),
            custody: self.custody.clone(),
            others: self.others.clone(),
            timeout: self.timeout,
        }
    }
}

#[derive(Clone)]
struct CallLimit<S> {
    inner: S,
    appends: Arc<Semaphore>,
    custody: Arc<Semaphore>,
    others: Arc<Semaphore>,
    timeout: Duration,
}

type BoxFuture<T> = Pin<Box<dyn std::future::Future<Output = T> + Send>>;

impl<S, B, R> tower::Service<http::Request<B>> for CallLimit<S>
where
    S: tower::Service<http::Request<B>, Response = http::Response<R>> + Clone + Send + 'static,
    S::Future: Send + 'static,
    S::Error: Into<tower::BoxError>,
    B: Send + 'static,
    R: Default + Send + 'static,
{
    type Response = http::Response<R>;
    type Error = tower::BoxError;
    type Future = BoxFuture<Result<Self::Response, Self::Error>>;

    fn poll_ready(&mut self, cx: &mut TaskContext<'_>) -> Poll<Result<(), Self::Error>> {
        self.inner.poll_ready(cx).map_err(Into::into)
    }

    fn call(&mut self, request: http::Request<B>) -> Self::Future {
        let custody = CUSTODY_PATHS.contains(&request.uri().path());
        let budget = if request.uri().path() == APPEND_PATH {
            &self.appends
        } else if custody {
            &self.custody
        } else {
            &self.others
        };
        let limit = if custody {
            CUSTODY_CALL_TIMEOUT
        } else {
            self.timeout
        };
        let Ok(permit) = budget.clone().try_acquire_owned() else {
            return Box::pin(async {
                Ok(refusal(Code::ResourceExhausted, "service_at_limit").into_http())
            });
        };
        // The caller's own deadline (grpc-timeout) counts too, answered a little before it so this layer, not the
        // transport's outer timer, ends the call with its code and telemetry.
        let timeout = client_deadline(request.headers())
            .map(|deadline| deadline.saturating_sub((deadline / 10).max(Duration::from_millis(5))))
            .map_or(limit, |deadline| deadline.min(limit));
        // Call the instance that was polled ready and keep a fresh clone for the next request.
        let clone = self.inner.clone();
        let mut inner = std::mem::replace(&mut self.inner, clone);
        let future = inner.call(request);
        Box::pin(async move {
            let _permit = permit;
            match tokio::time::timeout(timeout, future).await {
                Ok(result) => result.map_err(Into::into),
                Err(_) => Ok(refusal(Code::DeadlineExceeded, "call_timed_out").into_http()),
            }
        })
    }
}

/// The caller's deadline as tonic hands it to a method.
fn client_deadline_metadata(metadata: &MetadataMap) -> Option<Duration> {
    client_deadline(&metadata.clone().into_headers())
}

/// The caller's deadline from the `grpc-timeout` header (up to eight digits and a unit), if it sent one.
fn client_deadline(headers: &http::HeaderMap) -> Option<Duration> {
    let value = headers.get("grpc-timeout")?.to_str().ok()?;
    if value.len() < 2 || value.len() > 9 {
        return None;
    }
    let (digits, unit) = value.split_at(value.len() - 1);
    let amount: u64 = digits.parse().ok()?;
    Some(match unit {
        "H" => Duration::from_secs(amount.saturating_mul(3600)),
        "M" => Duration::from_secs(amount.saturating_mul(60)),
        "S" => Duration::from_secs(amount),
        "m" => Duration::from_millis(amount),
        "u" => Duration::from_micros(amount),
        "n" => Duration::from_nanos(amount),
        _ => return None,
    })
}

/// gRPC health with Check only: SERVING while the last readiness check passed. Watch is refused, so no
/// unauthenticated stream outlives its call.
#[derive(Clone)]
struct Health(Arc<AtomicBool>);

#[tonic::async_trait]
impl tonic_health::pb::health_server::Health for Health {
    async fn check(
        &self,
        request: Request<tonic_health::pb::HealthCheckRequest>,
    ) -> Result<Response<tonic_health::pb::HealthCheckResponse>, Status> {
        let service = request.into_inner().service;
        if !service.is_empty() && service != SERVICE_NAME {
            return Err(Status::not_found("unknown service"));
        }
        let status = if self.0.load(Ordering::Relaxed) {
            tonic_health::pb::health_check_response::ServingStatus::Serving
        } else {
            tonic_health::pb::health_check_response::ServingStatus::NotServing
        };
        Ok(Response::new(tonic_health::pb::HealthCheckResponse {
            status: status as i32,
        }))
    }

    type WatchStream =
        Pin<Box<dyn Stream<Item = Result<tonic_health::pb::HealthCheckResponse, Status>> + Send>>;

    async fn watch(
        &self,
        _request: Request<tonic_health::pb::HealthCheckRequest>,
    ) -> Result<Response<Self::WatchStream>, Status> {
        Err(Status::unimplemented(
            "health watch is not served; use Check",
        ))
    }
}

/// The gRPC server: TLS, the append service and health, until `shutdown` completes. Health rechecks the startup
/// authority, this month's partition and the TokenReview API on an interval and reports NOT_SERVING while any fails.
async fn run(
    listener: TcpListener,
    identity: Identity,
    service: AppendService,
    limits: Limits,
    telemetry: RpcTelemetryLayer,
    shutdown: impl std::future::Future<Output = ()>,
) -> Result<()> {
    // Not serving until the first full check passes, including TokenReview.
    let serving = Arc::new(AtomicBool::new(false));
    let (pool, auth, tenant) = (
        service.pool.clone(),
        service.auth.clone(),
        service.tenant.clone(),
    );
    let maintenance_pool = pool.clone();
    // Custody's reconciler: waiting claims, acceptance retention, old copies re-read, the database epoch.
    let reconciler = service.custody.clone().map(|custody| {
        let (pool, tenant) = (pool.clone(), tenant.clone());
        tokio::spawn(async move {
            let mut interval = tokio::time::interval(Duration::from_secs(300));
            loop {
                interval.tick().await;
                let layout6 = match pool.get().await {
                    Ok(client) => client
                        .query_one(
                            "SELECT obj_description(to_regnamespace('evidence'), 'pg_namespace')",
                            &[],
                        )
                        .await
                        .ok()
                        .and_then(|row| row.get::<_, Option<String>>(0))
                        .is_some_and(|layout| layout == CUSTODY_LAYOUT),
                    Err(_) => false,
                };
                if layout6 {
                    evidence_custody::reconcile(&pool, &custody, &tenant).await;
                }
            }
        })
    });
    // The capture recorder: the broker's logged changes into the record, each batch in its own transaction.
    let recorder = limits.capture_every.map(|every| {
        let pool = pool.clone();
        tokio::spawn(async move {
            let mut interval = tokio::time::interval(every);
            interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
            let mut watch = RecorderWatch::default();
            let oldest = evidence_telemetry::gauge(
                evidence_telemetry::CAPTURE_LOG_OLDEST,
                "Age of the oldest change the capture log holds, seconds (0 when empty)",
            );
            let stalled = evidence_telemetry::gauge(
                evidence_telemetry::CAPTURE_STALLED_RUNS,
                "Recorder runs in a row that recorded nothing while the log held entries",
            );
            loop {
                interval.tick().await;
                let run = record_captures(&pool).await;
                let warnings = watch.observe(run, std::time::Instant::now());
                if let Some(seconds) = run.oldest_seconds {
                    oldest.record(seconds, &[]);
                }
                stalled.record(u64::from(watch.stalled_runs), &[]);
                for warning in warnings {
                    match warning {
                        RecorderWarning::Stalled(runs, reason) => tracing::warn!(
                            target: "agent_sandbox_broker::evidence",
                            stalled_runs = runs,
                            reason,
                            "evidence capture recorder has recorded nothing for a minute; the log keeps its entries"
                        ),
                        RecorderWarning::Behind(seconds) => tracing::warn!(
                            target: "agent_sandbox_broker::evidence",
                            oldest_seconds = seconds,
                            "evidence capture log is behind"
                        ),
                    }
                }
            }
        })
    });
    let maintenance = tokio::spawn(async move {
        let mut interval = tokio::time::interval(Duration::from_secs(24 * 3600));
        interval.tick().await;
        loop {
            interval.tick().await;
            if let Err(error) = ensure_partitions(&maintenance_pool).await {
                tracing::warn!(error = %error, "evidence partition maintenance failed; health follows this month");
            }
        }
    });
    let flag = serving.clone();
    let readiness = tokio::spawn(async move {
        let mut interval = tokio::time::interval(limits.readiness);
        loop {
            interval.tick().await;
            let now = ready(&pool, &auth, &tenant).await;
            if flag.swap(now, Ordering::Relaxed) != now {
                if now {
                    tracing::info!("evidence append service serving again");
                } else {
                    tracing::warn!(
                        "evidence append service not serving: the record, its authority or TokenReview failed"
                    );
                }
            }
        }
    });
    let result = Server::builder()
        .tls_config(
            ServerTlsConfig::new()
                .identity(identity)
                .timeout(limits.handshake),
        )
        .context("invalid server TLS configuration")?
        .max_concurrent_streams(32)
        .max_connection_age(Duration::from_secs(1800))
        .http2_keepalive_interval(Some(Duration::from_secs(30)))
        .http2_keepalive_timeout(Some(Duration::from_secs(10)))
        .layer(telemetry)
        .layer(CallLimitLayer {
            appends: Arc::new(Semaphore::new(limits.in_flight.max(1))),
            custody: Arc::new(Semaphore::new(limits.custody_in_flight.max(1))),
            others: Arc::new(Semaphore::new(limits.other_in_flight.max(1))),
            timeout: limits.call_timeout,
        })
        .add_service(
            tonic_health::pb::health_server::HealthServer::new(Health(serving))
                .max_decoding_message_size(HEALTH_MAX_MESSAGE_BYTES),
        )
        .add_service(
            EvidenceAppendServiceServer::new(service)
                .max_decoding_message_size(MAX_PAYLOAD_BYTES + 16 * 1024),
        )
        .serve_with_incoming_shutdown(limited(listener, limits.connections), shutdown)
        .await
        .context("evidence append service stopped with an error");
    readiness.abort();
    maintenance.abort();
    if let Some(reconciler) = reconciler {
        reconciler.abort();
    }
    if let Some(recorder) = recorder {
        recorder.abort();
    }
    result
}

/// Entries the recorder takes per transaction, and the most batches one run takes before waiting for the next tick.
const CAPTURE_BATCH: i32 = 100;
const CAPTURE_BATCHES_PER_RUN: usize = 200;
/// Recorder calls run under this statement bound, below the database's 1 s deadlock_timeout less the recorder's 200 ms
/// lock wait, so no broker transaction ever waits behind the recorder long enough to be chosen as a deadlock victim; the
/// recorder refuses a call without it. Each call is a procedure that commits before and after recording, so it runs in
/// a transaction of its own and keeps no lock once it returns.
const CAPTURE_STATEMENT_BOUND: &str = "SET statement_timeout = '800ms'";
const CAPTURE_STATEMENT_RESET: &str = "RESET statement_timeout";
const CAPTURE_CALL: &str = "CALL evidence.capture_record_now($1, NULL)";

/// Runs in a row without progress before a warning, and the age of the oldest logged entry that counts as behind; each
/// warning repeats at most once a minute while its condition lasts.
const CAPTURE_STALLED_RUNS: u32 = 12;
const CAPTURE_BEHIND_SECONDS: u64 = 300;
const CAPTURE_WARNING_EVERY: Duration = Duration::from_secs(60);

/// How a recorder run ended: why it recorded nothing while entries waited (another recorder held the lock, a lock it
/// needed was held so the batch was deferred, or the call failed), and the oldest entry's age when it could be read.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct RecorderRun {
    stalled: Option<&'static str>,
    oldest_seconds: Option<u64>,
}

#[derive(Debug, PartialEq, Eq)]
enum RecorderWarning {
    Stalled(u32, &'static str),
    Behind(u64),
}

/// Turns recorder runs into warnings: no progress for a minute, or the log behind.
#[derive(Default)]
struct RecorderWatch {
    stalled_runs: u32,
    stalled_warned: Option<std::time::Instant>,
    behind_warned: Option<std::time::Instant>,
}

impl RecorderWatch {
    fn observe(&mut self, run: RecorderRun, now: std::time::Instant) -> Vec<RecorderWarning> {
        let due = |last: Option<std::time::Instant>| {
            last.is_none_or(|at| now.duration_since(at) >= CAPTURE_WARNING_EVERY)
        };
        let mut warnings = Vec::new();
        match run.stalled {
            Some(reason) => {
                self.stalled_runs = self.stalled_runs.saturating_add(1);
                if self.stalled_runs >= CAPTURE_STALLED_RUNS && due(self.stalled_warned) {
                    self.stalled_warned = Some(now);
                    warnings.push(RecorderWarning::Stalled(self.stalled_runs, reason));
                }
            }
            None => {
                self.stalled_runs = 0;
                self.stalled_warned = None;
            }
        }
        match run.oldest_seconds {
            Some(seconds) if seconds >= CAPTURE_BEHIND_SECONDS => {
                if due(self.behind_warned) {
                    self.behind_warned = Some(now);
                    warnings.push(RecorderWarning::Behind(seconds));
                }
            }
            Some(_) => self.behind_warned = None,
            None => {}
        }
        warnings
    }
}

/// One bounded recorder call; its JSON result. The session's statement bound is restored afterwards, and a connection
/// whose bound could not be restored leaves the pool rather than carrying it to another caller.
async fn record_batch(pool: &Pool) -> Result<serde_json::Value, Option<tokio_postgres::Error>> {
    let client = pool.get().await.map_err(|_| None)?;
    client
        .batch_execute(CAPTURE_STATEMENT_BOUND)
        .await
        .map_err(Some)?;
    let called = client.query_one(CAPTURE_CALL, &[&CAPTURE_BATCH]).await;
    if client.batch_execute(CAPTURE_STATEMENT_RESET).await.is_err() {
        drop(deadpool_postgres::Object::take(client));
    }
    let row = called.map_err(Some)?;
    let result: serde_json::Value = row.try_get(0).map_err(Some)?;
    Ok(result)
}

/// The capture log's age read on its own, for a run whose recorder call failed.
async fn log_age(pool: &Pool) -> Option<u64> {
    let client = pool.get().await.ok()?;
    let row = client
        .query_one("SELECT evidence.capture_log_age()", &[])
        .await
        .ok()?;
    u64::try_from(row.get::<_, i32>(0)).ok()
}

/// One recorder run: batches until the log holds fewer than a batch, the recorder is held or deferred, a call fails, or
/// the run's bound is reached. Counts only are logged; a database without the capture log (migration 081 not applied)
/// is skipped.
async fn record_captures(pool: &Pool) -> RecorderRun {
    let mut run = RecorderRun::default();
    let mut progressed = false;
    for _ in 0..CAPTURE_BATCHES_PER_RUN {
        let result = match record_batch(pool).await {
            Ok(result) => result,
            Err(error) => {
                let code = error.as_ref().and_then(tokio_postgres::Error::code);
                if code == Some(&tokio_postgres::error::SqlState::UNDEFINED_FUNCTION) {
                    return RecorderRun::default();
                }
                tracing::warn!(
                    target: "agent_sandbox_broker::evidence",
                    reason = error
                        .as_ref()
                        .and_then(|e| e.as_db_error())
                        .and_then(|db| db.message().strip_prefix("evidence:"))
                        .unwrap_or("record_unavailable"),
                    "evidence capture recorder failed; the log keeps its entries"
                );
                let oldest_seconds = log_age(pool).await;
                let stalled = (!progressed && oldest_seconds != Some(0)).then_some("failed");
                return RecorderRun {
                    stalled,
                    oldest_seconds,
                };
            }
        };
        let count = |name: &str| {
            result
                .get(name)
                .and_then(serde_json::Value::as_u64)
                .unwrap_or(0)
        };
        let flag = |name: &str| {
            result
                .get(name)
                .and_then(serde_json::Value::as_bool)
                .unwrap_or(false)
        };
        run.oldest_seconds = Some(count("oldest_seconds"));
        if flag("busy") || flag("deferred") {
            // A run that recorded nothing counts as stalled only while entries wait.
            if !progressed && count("oldest_seconds") > 0 {
                run.stalled = Some(if flag("busy") { "held" } else { "deferred" });
            }
            return run;
        }
        if count("taken") > 0 {
            progressed = true;
            tracing::info!(
                target: "agent_sandbox_broker::evidence",
                taken = count("taken"),
                appended = count("appended"),
                gaps = count("gaps"),
                discarded = count("discarded"),
                already_recorded = count("already_recorded"),
                "evidence capture recorded"
            );
        }
        if count("taken") < CAPTURE_BATCH as u64 || count("oldest_seconds") == 0 {
            return run;
        }
    }
    run
}

/// Exit 0 when the local service answers the gRPC health check as serving; a Pod's exec probe runs this, since kubelet
/// network probes cannot present the internal CA's TLS.
pub async fn probe(args: EvidenceProbeArgs) -> Result<()> {
    let ca = fs::read(&args.ca_file).context("failed to read the probe's CA bundle")?;
    let channel = Channel::from_shared(args.address.clone())
        .context("invalid probe address")?
        .tls_config(
            ClientTlsConfig::new()
                .ca_certificate(Certificate::from_pem(ca))
                .domain_name(args.server_name),
        )
        .context("invalid probe TLS configuration")?
        .connect_timeout(Duration::from_secs(3))
        .timeout(Duration::from_secs(3))
        .connect()
        .await
        .context("the evidence append service is not reachable")?;
    let mut client = tonic_health::pb::health_client::HealthClient::new(channel);
    let status = client
        .check(tonic_health::pb::HealthCheckRequest {
            service: "enrichedlab.evidence.v1.EvidenceAppendService".into(),
        })
        .await
        .context("health check failed")?
        .into_inner()
        .status;
    if status == tonic_health::pb::health_check_response::ServingStatus::Serving as i32 {
        Ok(())
    } else {
        bail!("the evidence append service is not serving")
    }
}

async fn shutdown_signal() {
    #[cfg(unix)]
    {
        if let Ok(mut signal) =
            tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
        {
            tokio::select! {
                _ = signal.recv() => {}
                _ = tokio::signal::ctrl_c() => {}
            }
            return;
        }
    }
    tokio::signal::ctrl_c().await.ok();
}

#[cfg(test)]
mod recorder_watch_tests {
    use super::*;
    use std::time::Instant;

    fn run(stalled: Option<&'static str>, oldest_seconds: Option<u64>) -> RecorderRun {
        RecorderRun {
            stalled,
            oldest_seconds,
        }
    }

    #[test]
    fn a_stalled_recorder_warns_after_a_minute_of_runs_and_then_once_a_minute() {
        let mut watch = RecorderWatch::default();
        let start = Instant::now();
        for i in 1..CAPTURE_STALLED_RUNS {
            let at = start + Duration::from_secs(5 * u64::from(i));
            assert!(watch.observe(run(Some("held"), Some(5)), at).is_empty());
        }
        let at = start + Duration::from_secs(60);
        assert_eq!(
            watch.observe(run(Some("deferred"), Some(60)), at),
            vec![RecorderWarning::Stalled(CAPTURE_STALLED_RUNS, "deferred")]
        );
        assert!(
            watch
                .observe(run(Some("failed"), None), at + Duration::from_secs(5))
                .is_empty()
        );
        assert_eq!(
            watch.observe(run(Some("failed"), None), at + Duration::from_secs(60)),
            vec![RecorderWarning::Stalled(CAPTURE_STALLED_RUNS + 2, "failed")]
        );
        // A run with progress starts the count again.
        assert!(
            watch
                .observe(run(None, Some(0)), at + Duration::from_secs(65))
                .is_empty()
        );
        assert_eq!(watch.stalled_runs, 0);
        assert!(
            watch
                .observe(run(Some("held"), Some(5)), at + Duration::from_secs(70))
                .is_empty()
        );
    }

    #[test]
    fn a_log_behind_warns_at_once_and_then_once_a_minute_even_while_calls_fail() {
        let mut watch = RecorderWatch::default();
        let now = Instant::now();
        assert!(
            watch
                .observe(run(None, Some(CAPTURE_BEHIND_SECONDS - 1)), now)
                .is_empty()
        );
        assert_eq!(
            watch.observe(run(None, Some(CAPTURE_BEHIND_SECONDS)), now),
            vec![RecorderWarning::Behind(CAPTURE_BEHIND_SECONDS)]
        );
        assert!(
            watch
                .observe(
                    run(Some("failed"), Some(CAPTURE_BEHIND_SECONDS + 5)),
                    now + Duration::from_secs(5)
                )
                .is_empty()
        );
        // An unknown age neither warns nor clears the last warning.
        assert!(
            watch
                .observe(run(Some("failed"), None), now + Duration::from_secs(30))
                .is_empty()
        );
        assert_eq!(
            watch.observe(
                run(Some("failed"), Some(400)),
                now + Duration::from_secs(60)
            ),
            vec![RecorderWarning::Behind(400)]
        );
    }
}

#[cfg(test)]
mod tests {
    use super::pb::{
        AppendRequest, ScopeKind, evidence_append_service_client::EvidenceAppendServiceClient,
    };
    use super::{EvidenceProbeArgs, Limits, limited, prepare, refusal, run, source_time};
    use crate::broker::auth::EvidenceProducerAuth;
    use crate::broker::evidence_telemetry::RpcTelemetryLayer;
    use sha2::{Digest, Sha256};
    use std::sync::{Arc, atomic::AtomicBool};
    use tonic::codegen::http;
    use tonic::{
        Code, Request,
        transport::{Certificate, Channel, ClientTlsConfig, Identity},
    };
    use tonic_types::StatusExt;

    #[test]
    fn refusals_carry_the_reason_and_no_database_text() {
        let status = refusal(Code::PermissionDenied, "grant_missing");
        assert_eq!(status.code(), Code::PermissionDenied);
        assert_eq!(status.message(), "grant_missing");
        let info = status.get_details_error_info().expect("error info");
        assert_eq!(info.reason, "GRANT_MISSING");
        assert_eq!(info.domain, "evidence.enrichedlab.org");
    }

    #[test]
    fn source_times_must_be_present_and_well_formed() {
        assert!(source_time(None).is_err());
        assert!(
            source_time(Some(prost_types::Timestamp {
                seconds: 1_761_220_800,
                nanos: -1
            }))
            .is_err()
        );
        // The record keeps microseconds: finer times are refused, not rounded.
        let refused = source_time(Some(prost_types::Timestamp {
            seconds: 1_761_220_800,
            nanos: 5,
        }))
        .unwrap_err();
        assert_eq!(refused.message(), "source_time_precision");
        let time = source_time(Some(prost_types::Timestamp {
            seconds: 1_761_220_800,
            nanos: 5_000,
        }))
        .unwrap();
        assert_eq!(time.unix_timestamp(), 1_761_220_800);
        assert_eq!(time.nanosecond(), 5_000);
    }

    #[tokio::test]
    async fn every_call_takes_a_budget_before_its_body_is_read() {
        use tower::{Layer, Service};
        // An inner service that never answers: a held call keeps its permit, as a request withholding its body
        // would. The budget is taken before the inner service (where decoding happens) runs.
        let held = tower::service_fn(|_: http::Request<()>| {
            std::future::pending::<Result<http::Response<()>, std::convert::Infallible>>()
        });
        let mut service = super::CallLimitLayer {
            appends: Arc::new(tokio::sync::Semaphore::new(1)),
            custody: Arc::new(tokio::sync::Semaphore::new(1)),
            others: Arc::new(tokio::sync::Semaphore::new(2)),
            timeout: std::time::Duration::from_secs(30),
        }
        .layer(held);
        let call = |path: &str| http::Request::builder().uri(path).body(()).unwrap();
        let code = |response: http::Response<()>| {
            response
                .headers()
                .get("grpc-status")
                .map(|value| value.to_str().unwrap().to_string())
        };
        let health = "/grpc.health.v1.Health/Check";
        let append = "/enrichedlab.evidence.v1.EvidenceAppendService/Append";
        let first = service.call(call(health));
        let second = service.call(call(health));
        // The third health call and a call to an unknown path share the exhausted budget.
        for path in [health, "/grpc.health.v1.Health/Watch", "/anything/else"] {
            let refused = service.call(call(path)).await.unwrap();
            assert_eq!(code(refused), Some("8".to_string()), "{path}");
        }
        // Appends have their own budget, untouched by health.
        let third = service.call(call(append));
        let refused = service.call(call(append)).await.unwrap();
        assert_eq!(code(refused), Some("8".to_string()));
        // Custody calls have theirs too, untouched by appends and health.
        let store = "/enrichedlab.evidence.v1.EvidenceAppendService/StoreObject";
        let fourth = service.call(call(store));
        for path in [
            store,
            "/enrichedlab.evidence.v1.EvidenceAppendService/AwaitClaim",
        ] {
            let refused = service.call(call(path)).await.unwrap();
            assert_eq!(code(refused), Some("8".to_string()), "{path}");
        }
        drop((first, second, third, fourth));
        // Released permits admit calls again.
        let again = service.call(call(health));
        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(50), again)
                .await
                .is_err(),
            "a call within budget reaches the inner service"
        );
    }

    #[tokio::test]
    async fn connections_beyond_the_limit_are_closed_on_arrival() {
        use futures_util::StreamExt;
        use tokio::io::AsyncReadExt;
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let mut accepted = Box::pin(limited(listener, 1));
        let _first = tokio::net::TcpStream::connect(address).await.unwrap();
        let held = accepted.next().await.unwrap().unwrap();
        let mut second = tokio::net::TcpStream::connect(address).await.unwrap();
        let next =
            tokio::time::timeout(std::time::Duration::from_millis(500), accepted.next()).await;
        assert!(
            next.is_err(),
            "a connection beyond the limit must not be yielded"
        );
        let mut byte = [0u8; 1];
        let read =
            tokio::time::timeout(std::time::Duration::from_secs(2), second.read(&mut byte)).await;
        assert!(
            matches!(read, Ok(Ok(0)) | Ok(Err(_))),
            "the extra connection must be closed"
        );
        drop(held);
        let _third = tokio::net::TcpStream::connect(address).await.unwrap();
        let freed = tokio::time::timeout(std::time::Duration::from_secs(2), accepted.next()).await;
        assert!(
            matches!(freed, Ok(Some(Ok(_)))),
            "a freed slot must take the next connection"
        );
    }

    const PROJECT: &str = "00000000-0000-4000-8000-0000000000a1";
    const RUN: &str = "00000000-0000-4000-8000-00000000a0a1";
    const SIBLING_RUN: &str = "00000000-0000-4000-8000-00000000a0a2";

    /// The finished run broker_evidence_custody_qualification.sql grants custody in.
    const CUSTODY_RUN: &str = "00000000-0000-4000-8000-00000000a0a4";

    fn request(
        token: Option<&str>,
        key: &str,
        payload: &str,
        run: &str,
        event: &str,
        tenant: &str,
    ) -> Request<AppendRequest> {
        let mut request = Request::new(AppendRequest {
            tenant_id: tenant.into(),
            group_key: "lab-a".into(),
            scope_kind: ScopeKind::Project as i32,
            project_id: PROJECT.into(),
            run_id: run.into(),
            event_type: event.into(),
            idempotency_key: key.into(),
            source_time: Some(prost_types::Timestamp {
                seconds: 1_761_220_800,
                nanos: 0,
            }),
            payload: payload.as_bytes().to_vec(),
            payload_sha256: hex::encode(Sha256::digest(payload.as_bytes())),
        });
        if let Some(token) = token {
            request
                .metadata_mut()
                .insert("authorization", format!("Bearer {token}").parse().unwrap());
        }
        request
    }

    fn health_client(channel: &Channel) -> tonic_health::pb::health_client::HealthClient<Channel> {
        tonic_health::pb::health_client::HealthClient::new(channel.clone())
    }

    fn reason(status: &tonic::Status) -> String {
        status
            .get_details_error_info()
            .map(|info| info.reason)
            .unwrap_or_default()
    }

    /// The service end to end over TLS against the qualification database (scripts/qualify_remote_broker.sh
    /// evidence-service sets EVIDENCE_QUAL_DSN to the adapter role the evidence fixtures bind to the qualification
    /// sandbox producer). Only the Kubernetes TokenReview call is replaced, by a fixed token table.
    /// Custody through the real service: needs the evidence qualification database (EVIDENCE_QUAL_DSN and
    /// EVIDENCE_QUAL_OWNER_DSN) and scripts/custody_stores_local.sh's two stores (CUSTODY_*), with lab A's bucket.
    #[tokio::test]
    #[ignore = "needs the evidence qualification database and the local custody stores"]
    async fn custody_end_to_end() {
        use super::pb::{
            AwaitClaimRequest, StoreObjectHeader, StoreObjectRequest, store_object_request::Part,
        };
        let _ = rustls::crypto::ring::default_provider().install_default();
        let env = |name: &str| std::env::var(name).unwrap_or_else(|_| panic!("{name}"));
        let pool = crate::broker::store::build_pool(&env("EVIDENCE_QUAL_DSN"), 4).unwrap();
        let owner = crate::broker::store::build_pool(&env("EVIDENCE_QUAL_OWNER_DSN"), 1).unwrap();
        // The reviewed receipt schema is the newest for this test (service_end_to_end leaves an open one).
        owner
            .get()
            .await
            .unwrap()
            .query_one(
                "SELECT evidence.register_event_schema('evidence-site', 'sandbox.receipt', $1::text::jsonb)",
                &[&include_str!("../../schemas/evidence/sandbox.receipt.v1.json")],
            )
            .await
            .unwrap();
        let dir = std::env::temp_dir().join(format!(
            "evidence-custody-{}",
            uuid::Uuid::new_v4().simple()
        ));
        std::fs::create_dir(&dir).unwrap();
        let endpoint = |role: &str| {
            let upper = role.to_ascii_uppercase();
            serde_json::json!({
                "url": env(&format!("CUSTODY_{upper}_URL")),
                "region": env("CUSTODY_REGION"),
                "access_key_file": env(&format!("CUSTODY_{upper}_ACCESS_KEY_FILE")),
                "secret_key_file": env(&format!("CUSTODY_{upper}_SECRET_KEY_FILE")),
            })
        };
        let config = serde_json::json!({
            "ca_file": env("CUSTODY_CA"),
            "endpoints": {"store-primary": endpoint("primary"), "store-replica": endpoint("replica")},
            "replica_wait_seconds": 30,
        });
        std::fs::write(dir.join("custody.json"), config.to_string()).unwrap();
        let custody = std::sync::Arc::new(super::Custody::load(&dir.join("custody.json")).unwrap());

        let sandbox = "system:serviceaccount:qual:sandbox-receipts";
        let gateway = "system:serviceaccount:qual:inference-gateway";
        let canary = "system:serviceaccount:qual:custody-canary";
        let auth = EvidenceProducerAuth::for_tests(
            "enrichedlab-evidence",
            &[
                ("good-token", sandbox),
                ("gateway-token", gateway),
                ("canary-token", canary),
            ],
            &[
                (sandbox, "openshell/sandbox-qual"),
                (gateway, "gateway/inference-qual"),
                (canary, "ops/custody-canary"),
            ],
            Arc::new(AtomicBool::new(false)),
        );
        let mut service = prepare(pool.clone(), auth, "evidence-site".into())
            .await
            .unwrap();
        service.custody = Some(custody.clone());
        let ca = rcgen::KeyPair::generate().unwrap();
        let mut ca_params = rcgen::CertificateParams::new(Vec::<String>::new()).unwrap();
        ca_params.is_ca = rcgen::IsCa::Ca(rcgen::BasicConstraints::Unconstrained);
        let ca_cert = ca_params.self_signed(&ca).unwrap();
        let key = rcgen::KeyPair::generate().unwrap();
        let server = rcgen::CertificateParams::new(vec!["localhost".to_string()])
            .unwrap()
            .signed_by(&key, &rcgen::Issuer::new(ca_params, ca))
            .unwrap();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let (stop, stopped) = tokio::sync::oneshot::channel::<()>();
        let limits = Limits {
            connections: 8,
            in_flight: 4,
            other_in_flight: 4,
            readiness: std::time::Duration::from_millis(200),
            call_timeout: std::time::Duration::from_secs(5),
            handshake: std::time::Duration::from_secs(2),
            custody_in_flight: 2,
            capture_every: None,
        };
        let server_task = tokio::spawn(run(
            listener,
            Identity::from_pem(server.pem(), key.serialize_pem()),
            service,
            limits,
            RpcTelemetryLayer::new(),
            async {
                stopped.await.ok();
            },
        ));
        let channel = loop {
            let attempt = Channel::from_shared(format!("https://{address}"))
                .unwrap()
                .tls_config(
                    ClientTlsConfig::new()
                        .ca_certificate(Certificate::from_pem(ca_cert.pem()))
                        .domain_name("localhost"),
                )
                .unwrap()
                .connect()
                .await;
            if let Ok(channel) = attempt {
                break channel;
            }
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        };
        let mut client = EvidenceAppendServiceClient::new(channel);
        fn authorised<T>(token: &str, mut request: Request<T>) -> Request<T> {
            request
                .metadata_mut()
                .insert("authorization", format!("Bearer {token}").parse().unwrap());
            request
        }
        let scoped = |key: &str, bytes: &[u8], digest: Option<String>, standalone: bool| {
            StoreObjectRequest {
                part: Some(Part::Header(StoreObjectHeader {
                    tenant_id: "evidence-site".into(),
                    group_key: "lab-a".into(),
                    scope_kind: if standalone {
                        ScopeKind::Standalone
                    } else {
                        ScopeKind::Project
                    } as i32,
                    project_id: if standalone {
                        String::new()
                    } else {
                        PROJECT.into()
                    },
                    run_id: if standalone {
                        String::new()
                    } else {
                        CUSTODY_RUN.into()
                    },
                    media_type: "text/plain".into(),
                    raw_sha256: digest.unwrap_or_else(|| hex::encode(Sha256::digest(bytes))),
                    raw_size: bytes.len() as u64,
                    idempotency_key: key.into(),
                })),
            }
        };
        let header =
            |key: &str, bytes: &[u8], digest: Option<String>| scoped(key, bytes, digest, false);
        let chunks = |bytes: &[u8], size: usize| -> Vec<StoreObjectRequest> {
            bytes
                .chunks(size)
                .map(|chunk| StoreObjectRequest {
                    part: Some(Part::Chunk(chunk.to_vec())),
                })
                .collect()
        };
        let prefix = uuid::Uuid::new_v4().simple().to_string();
        let bytes = format!("custody end to end {prefix} ")
            .repeat(4000)
            .into_bytes();

        // Stored, both copies read back, retention extended from the bucket's one-day default to the floor: ready.
        let mut messages = vec![header(&format!("{prefix}-a"), &bytes, None)];
        messages.extend(chunks(&bytes, 60 * 1024));
        let claim = client
            .store_object(authorised(
                "good-token",
                Request::new(tokio_stream::iter(messages)),
            ))
            .await
            .unwrap()
            .into_inner()
            .claim
            .unwrap();
        assert!(claim.ready, "{claim:?}");
        // The custody qualification accepted project A under a ten-year records class, so the floor is that period.
        assert_eq!(claim.retention_class, "records");
        assert_eq!(claim.raw_sha256, hex::encode(Sha256::digest(&bytes)));
        let bucket = std::env::var("CUSTODY_BUCKET").unwrap();
        let object_key = crate::broker::evidence_objects::object_key(&claim.raw_sha256).unwrap();
        for role in ["primary", "replica"] {
            let store = custody.store(&format!("store-{role}")).unwrap();
            let facts = store
                .head(&bucket, &object_key, Some(&claim.object_version))
                .await
                .unwrap();
            assert!(
                facts.retain_until.unwrap()
                    > time::OffsetDateTime::now_utc() + time::Duration::days(3650),
                "{role} copy not retained to the records floor"
            );
        }

        // A run-class claim (standalone scope, no accepted project): ready, both copies kept past the 90-day floor.
        let small = format!("run class {prefix}").into_bytes();
        let mut messages = vec![scoped(&format!("{prefix}-run"), &small, None, true)];
        messages.extend(chunks(&small, 1024));
        let run_claim = client
            .store_object(authorised(
                "good-token",
                Request::new(tokio_stream::iter(messages)),
            ))
            .await
            .unwrap()
            .into_inner()
            .claim
            .unwrap();
        assert!(
            run_claim.ready && run_claim.retention_class == "run",
            "{run_claim:?}"
        );
        let run_key = crate::broker::evidence_objects::object_key(&run_claim.raw_sha256).unwrap();
        for role in ["primary", "replica"] {
            let store = custody.store(&format!("store-{role}")).unwrap();
            let facts = store
                .head(&bucket, &run_key, Some(&run_claim.object_version))
                .await
                .unwrap();
            assert!(
                facts.retain_until.unwrap()
                    > time::OffsetDateTime::now_utc() + time::Duration::days(90),
                "{role} copy not retained past the run floor"
            );
        }

        // The same bytes under a new key: the stored object again, verified again.
        let mut messages = vec![header(&format!("{prefix}-b"), &bytes, None)];
        messages.extend(chunks(&bytes, 64 * 1024));
        let again = client
            .store_object(authorised(
                "good-token",
                Request::new(tokio_stream::iter(messages)),
            ))
            .await
            .unwrap()
            .into_inner()
            .claim
            .unwrap();
        assert!(
            again.ready
                && again.claim_id != claim.claim_id
                && again.object_version == claim.object_version
        );

        // Refusals: wrong digest, no header first, a chunk past its bound.
        let mut messages = vec![header(&format!("{prefix}-c"), &bytes, Some("0".repeat(64)))];
        messages.extend(chunks(&bytes, 60 * 1024));
        let status = client
            .store_object(authorised(
                "good-token",
                Request::new(tokio_stream::iter(messages)),
            ))
            .await
            .unwrap_err();
        assert_eq!(reason(&status), "CUSTODY_DIGEST_MISMATCH");
        let status = client
            .store_object(authorised(
                "good-token",
                Request::new(tokio_stream::iter(chunks(&bytes, 1024))),
            ))
            .await
            .unwrap_err();
        assert_eq!(reason(&status), "ARGUMENT_MISSING");
        let big = vec![b'x'; 70 * 1024];
        let mut messages = vec![header(&format!("{prefix}-d"), &big, None)];
        messages.extend(chunks(&big, 70 * 1024));
        let status = client
            .store_object(authorised(
                "good-token",
                Request::new(tokio_stream::iter(messages)),
            ))
            .await
            .unwrap_err();
        assert_eq!(reason(&status), "CUSTODY_SIZE");

        // AwaitClaim answers the owner's ready claim and nobody else's.
        let mut waiting = Request::new(AwaitClaimRequest {
            tenant_id: "evidence-site".into(),
            claim_id: claim.claim_id.clone(),
        });
        waiting.set_timeout(std::time::Duration::from_secs(10));
        let awaited = client
            .await_claim(authorised("good-token", waiting))
            .await
            .unwrap()
            .into_inner()
            .claim
            .unwrap();
        assert!(awaited.ready && awaited.claim_id == claim.claim_id);
        let status = client
            .await_claim(authorised(
                "gateway-token",
                Request::new(AwaitClaimRequest {
                    tenant_id: "evidence-site".into(),
                    claim_id: claim.claim_id.clone(),
                }),
            ))
            .await
            .unwrap_err();
        assert_eq!(reason(&status), "CUSTODY_CLAIM_UNKNOWN");

        // The record binds the claim: the entity carries the server's digest, location and version.
        let mut receipt: serde_json::Value = serde_json::from_str(include_str!(
            "../../tests/fixtures/sandbox_receipt_sample.json"
        ))
        .unwrap();
        receipt["lineage"] = serde_json::from_str(&format!(
            r#"{{"version":1,"activity":{{"key":"e2e-{prefix}","kind":"sandbox_run"}},"entities":[{{"key":"out:{prefix}","kind":"artifact","disclosure":"project","claim":"{}"}}],"generated":[{{"entity":"out:{prefix}","role":"output"}}]}}"#,
            claim.claim_id
        ))
        .unwrap();
        let payload = receipt.to_string();
        let status = client
            .append(request(
                Some("good-token"),
                &format!("{prefix}-free"),
                r#"{"receipt":"free form"}"#,
                CUSTODY_RUN,
                "sandbox.receipt",
                "evidence-site",
            ))
            .await
            .unwrap_err();
        assert_eq!(reason(&status), "PAYLOAD_NOT_METADATA");
        client
            .append(request(
                Some("good-token"),
                &format!("{prefix}-rec"),
                &payload,
                CUSTODY_RUN,
                "sandbox.receipt",
                "evidence-site",
            ))
            .await
            .unwrap();
        let row = owner
            .get()
            .await
            .unwrap()
            .query_one(
                "SELECT content_sha256, object_uri, object_version FROM evidence.entities \
                 WHERE tenant_id = 'evidence-site' AND entity_key = $1",
                &[&format!("out:{prefix}")],
            )
            .await
            .unwrap();
        assert_eq!(row.get::<_, String>(0), claim.raw_sha256);
        assert_eq!(
            row.get::<_, String>(1),
            format!("s3://{bucket}/{object_key}")
        );
        assert_eq!(row.get::<_, String>(2), claim.object_version);

        // The canary client: random bytes stored, ready, and bound by one custody.canary record.
        std::fs::write(dir.join("ca.pem"), ca_cert.pem()).unwrap();
        std::fs::write(dir.join("canary-token"), "canary-token\n").unwrap();
        super::super::evidence_canary::run(super::super::evidence_canary::EvidenceCanaryArgs {
            address: format!("https://{address}"),
            server_name: "localhost".into(),
            ca_file: dir.join("ca.pem"),
            token_file: dir.join("canary-token"),
            tenant: "evidence-site".into(),
            group: "lab-a".into(),
            bytes: 200 * 1024,
        })
        .await
        .unwrap();
        let bound: i64 = owner
            .get()
            .await
            .unwrap()
            .query_one(
                "SELECT count(*) FROM evidence.entities e JOIN evidence.custody_references r \
                   ON r.tenant_id = e.tenant_id AND r.entity_id = e.entity_id \
                 WHERE e.tenant_id = 'evidence-site' AND e.entity_key LIKE 'canary:%' AND e.object_uri LIKE $1",
                &[&format!("s3://{bucket}/sha256/%")],
            )
            .await
            .unwrap()
            .get(0);
        assert!(bound >= 1, "the canary's entity is not bound to custody");

        // One reconciler pass runs cleanly.
        super::evidence_custody::reconcile(&pool, &custody, "evidence-site").await;
        stop.send(()).ok();
        server_task.await.unwrap().unwrap();
        std::fs::remove_dir_all(&dir).ok();
    }

    #[tokio::test]
    #[ignore = "needs the evidence qualification database"]
    async fn service_end_to_end() {
        let _ = rustls::crypto::ring::default_provider().install_default();
        let dsn = std::env::var("EVIDENCE_QUAL_DSN").expect("EVIDENCE_QUAL_DSN");
        let owner = std::env::var("EVIDENCE_QUAL_OWNER_DSN").expect("EVIDENCE_QUAL_OWNER_DSN");
        let pool = crate::broker::store::build_pool(&dsn, 4).unwrap();
        let subject = "system:serviceaccount:qual:sandbox-receipts";
        let tokens = [
            ("good-token", subject),
            ("unmapped-token", "system:serviceaccount:qual:someone-else"),
        ];
        let reviewer_down = Arc::new(AtomicBool::new(false));
        let auth = EvidenceProducerAuth::for_tests(
            "enrichedlab-evidence",
            &tokens,
            &[(subject, "openshell/sandbox-qual")],
            reviewer_down.clone(),
        );

        // Startup refuses a producer the database does not bind to this role, and an unknown tenant.
        let unbound = EvidenceProducerAuth::for_tests(
            "enrichedlab-evidence",
            &tokens,
            &[
                (subject, "openshell/sandbox-qual"),
                ("system:serviceaccount:qual:other", "openshell/unbound-qual"),
            ],
            Arc::new(AtomicBool::new(false)),
        );
        let error = prepare(pool.clone(), unbound, "evidence-site".into())
            .await
            .err()
            .unwrap();
        assert!(error.to_string().contains("not bound"), "{error}");
        let error = prepare(pool.clone(), auth.clone(), "no-such-site".into())
            .await
            .err()
            .unwrap();
        assert!(error.to_string().contains("unknown or disabled"), "{error}");
        // Startup makes any missing month: drop next month's records partition (empty) and watch it come back.
        let owner_pool = crate::broker::store::build_pool(&owner, 1).unwrap();
        let next_month: String = owner_pool
            .get()
            .await
            .unwrap()
            .query_one(
                "SELECT format('records_y%sm%s', to_char(d, 'YYYY'), to_char(d, 'MM')) \
                 FROM (SELECT date_trunc('month', now() AT TIME ZONE 'UTC') + interval '1 month' AS d) m",
                &[],
            )
            .await
            .unwrap()
            .get(0);
        owner_pool
            .get()
            .await
            .unwrap()
            .batch_execute(&format!("DROP TABLE evidence.{next_month}"))
            .await
            .unwrap();
        // The custody qualification registered the closed receipt schema; this test's payloads are free-form, so it
        // registers an open schema as the newest version for the length of the test.
        owner_pool
            .get()
            .await
            .unwrap()
            .query_one(
                "SELECT evidence.register_event_schema('evidence-site', 'sandbox.receipt', $1::text::jsonb)",
                &[&format!(r#"{{"type":"object","open":true,"test":"{}"}}"#, uuid::Uuid::new_v4())],
            )
            .await
            .unwrap();
        let service = prepare(pool, auth, "evidence-site".into()).await.unwrap();
        let back: bool = owner_pool
            .get()
            .await
            .unwrap()
            .query_one(
                "SELECT to_regclass('evidence.' || $1) IS NOT NULL",
                &[&next_month],
            )
            .await
            .unwrap()
            .get(0);
        assert!(back, "startup must recreate a missing month");
        let ca = rcgen::KeyPair::generate().unwrap();
        let mut ca_params = rcgen::CertificateParams::new(Vec::<String>::new()).unwrap();
        ca_params.is_ca = rcgen::IsCa::Ca(rcgen::BasicConstraints::Unconstrained);
        let ca_cert = ca_params.self_signed(&ca).unwrap();
        let key = rcgen::KeyPair::generate().unwrap();
        let server = rcgen::CertificateParams::new(vec!["localhost".to_string()])
            .unwrap()
            .signed_by(&key, &rcgen::Issuer::new(ca_params, ca))
            .unwrap();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let (stop, stopped) = tokio::sync::oneshot::channel::<()>();
        let limits = Limits {
            connections: 4,
            in_flight: 1,
            other_in_flight: 4,
            readiness: std::time::Duration::from_millis(500),
            call_timeout: std::time::Duration::from_secs(1),
            handshake: std::time::Duration::from_secs(1),
            custody_in_flight: 1,
            capture_every: None,
        };
        // Telemetry is recorded through the server stack into an in-memory exporter.
        let exporter = opentelemetry_sdk::metrics::InMemoryMetricExporter::default();
        let provider = opentelemetry_sdk::metrics::SdkMeterProvider::builder()
            .with_periodic_exporter(exporter.clone())
            .build();
        let meter = opentelemetry::metrics::MeterProvider::meter(&provider, "test");
        // TokenReview is down when the service starts: health must not report serving until it answers.
        reviewer_down.store(true, std::sync::atomic::Ordering::Relaxed);
        let server_task = tokio::spawn(run(
            listener,
            Identity::from_pem(server.pem(), key.serialize_pem()),
            service,
            limits,
            RpcTelemetryLayer::with_meter(&meter),
            async {
                stopped.await.ok();
            },
        ));

        let dir =
            std::env::temp_dir().join(format!("evidence-probe-{}", uuid::Uuid::new_v4().simple()));
        std::fs::create_dir(&dir).unwrap();
        std::fs::write(dir.join("ca.crt"), ca_cert.pem()).unwrap();
        let probe = EvidenceProbeArgs {
            address: format!("https://{address}"),
            server_name: "localhost".into(),
            ca_file: dir.join("ca.crt"),
        };
        for _ in 0..15 {
            assert!(
                super::probe(probe.clone()).await.is_err(),
                "health served before TokenReview answered"
            );
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        }
        reviewer_down.store(false, std::sync::atomic::Ordering::Relaxed);
        let mut serving = false;
        for _ in 0..50 {
            if super::probe(probe.clone()).await.is_ok() {
                serving = true;
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        }
        assert!(serving, "the health probe never saw the service serving");

        let channel = Channel::from_shared(format!("https://{address}"))
            .unwrap()
            .tls_config(
                ClientTlsConfig::new()
                    .ca_certificate(Certificate::from_pem(ca_cert.pem()))
                    .domain_name("localhost"),
            )
            .unwrap()
            .connect()
            .await
            .unwrap();
        let mut client = EvidenceAppendServiceClient::new(channel.clone());
        let client_channel = channel;
        let key_prefix = format!("svc-{}", uuid::Uuid::new_v4().simple());
        let payload = r#"{"receipt":"service-1"}"#;

        let first = client
            .append(request(
                Some("good-token"),
                &key_prefix,
                payload,
                RUN,
                "sandbox.receipt",
                "evidence-site",
            ))
            .await
            .expect("append")
            .into_inner();
        assert!(first.sequence >= 1 && !first.replayed && first.record_hash.len() == 64);
        let again = client
            .append(request(
                Some("good-token"),
                &key_prefix,
                payload,
                RUN,
                "sandbox.receipt",
                "evidence-site",
            ))
            .await
            .expect("retry")
            .into_inner();
        assert!(
            again.replayed
                && again.record_id == first.record_id
                && again.sequence == first.sequence
        );

        let conflict = client
            .append(request(
                Some("good-token"),
                &key_prefix,
                r#"{"receipt":"other"}"#,
                RUN,
                "sandbox.receipt",
                "evidence-site",
            ))
            .await
            .unwrap_err();
        assert_eq!(
            (conflict.code(), reason(&conflict).as_str()),
            (Code::AlreadyExists, "IDEMPOTENCY_CONFLICT")
        );

        for token in [None, Some("bad-token"), Some("unmapped-token")] {
            let status = client
                .append(request(
                    token,
                    &format!("{key_prefix}-auth"),
                    payload,
                    RUN,
                    "sandbox.receipt",
                    "evidence-site",
                ))
                .await
                .unwrap_err();
            assert_eq!(status.code(), Code::Unauthenticated, "{token:?}");
        }

        let cases = [
            (
                SIBLING_RUN,
                "sandbox.receipt",
                "evidence-site",
                Code::PermissionDenied,
                "GRANT_MISSING",
            ),
            (
                RUN,
                "review.accepted",
                "evidence-site",
                Code::PermissionDenied,
                "EVENT_TYPE_RESERVED",
            ),
            (
                RUN,
                "sandbox.receipt",
                "another-site",
                Code::PermissionDenied,
                "TENANT_NOT_SERVED",
            ),
        ];
        for (index, (run, event, tenant, code, expected)) in cases.into_iter().enumerate() {
            let status = client
                .append(request(
                    Some("good-token"),
                    &format!("{key_prefix}-case{index}"),
                    payload,
                    run,
                    event,
                    tenant,
                ))
                .await
                .unwrap_err();
            assert_eq!(
                (status.code(), reason(&status).as_str()),
                (code, expected),
                "case {index}"
            );
            assert!(!status.message().contains("ERROR") && !status.message().contains("evidence."));
        }

        let mut wrong = request(
            Some("good-token"),
            &format!("{key_prefix}-digest"),
            payload,
            RUN,
            "sandbox.receipt",
            "evidence-site",
        );
        wrong.get_mut().payload_sha256 = "0".repeat(64);
        let status = client.append(wrong).await.unwrap_err();
        assert_eq!(
            (status.code(), reason(&status).as_str()),
            (Code::InvalidArgument, "PAYLOAD_DIGEST_MISMATCH")
        );

        let lineage = r#"{"lineage":{"version":1,"activity":{"key":"svc-run","kind":"sandbox_run","event":"ended"},"agents":[{"kind":"sandbox","identity":"openshell/sandbox-qual","role":"runtime"}]}}"#;
        let receipt = client
            .append(request(
                Some("good-token"),
                &format!("{key_prefix}-lineage"),
                lineage,
                RUN,
                "sandbox.receipt",
                "evidence-site",
            ))
            .await
            .expect("lineage append")
            .into_inner();
        assert_eq!(receipt.sequence, again.sequence + 1);

        // Microsecond times only: a finer time would make two different requests look like one retry.
        let mut fine = request(
            Some("good-token"),
            &format!("{key_prefix}-fine"),
            payload,
            RUN,
            "sandbox.receipt",
            "evidence-site",
        );
        fine.get_mut().source_time = Some(prost_types::Timestamp {
            seconds: 1_761_220_800,
            nanos: 100,
        });
        let status = client.append(fine).await.unwrap_err();
        assert_eq!(
            (status.code(), reason(&status).as_str()),
            (Code::InvalidArgument, "SOURCE_TIME_PRECISION")
        );

        // One append in flight across the service: a second is refused at once as resource exhausted, and the first,
        // held past the one-second call deadline, answers DEADLINE_EXCEEDED.
        let mut slow_client = client.clone();
        let slow = tokio::spawn(async move {
            slow_client
                .append(request(
                    Some("slow-good-token"),
                    &format!("svc-slow-{}", uuid::Uuid::new_v4().simple()),
                    r#"{"receipt":"slow"}"#,
                    RUN,
                    "sandbox.receipt",
                    "evidence-site",
                ))
                .await
        });
        tokio::time::sleep(std::time::Duration::from_millis(300)).await;
        let shed = client
            .append(request(
                Some("good-token"),
                &format!("{key_prefix}-shed"),
                payload,
                RUN,
                "sandbox.receipt",
                "evidence-site",
            ))
            .await
            .unwrap_err();
        assert_eq!(
            (shed.code(), reason(&shed).as_str()),
            (Code::ResourceExhausted, "SERVICE_AT_LIMIT")
        );
        let late = slow.await.unwrap().unwrap_err();
        assert_eq!(
            (late.code(), reason(&late).as_str()),
            (Code::DeadlineExceeded, "CALL_TIMED_OUT")
        );
        // A caller's own deadline shorter than the service's is answered inside the service, with its code.
        let mut hurried = request(
            Some("slow-good-token"),
            &format!("{key_prefix}-hurried"),
            payload,
            RUN,
            "sandbox.receipt",
            "evidence-site",
        );
        hurried.set_timeout(std::time::Duration::from_millis(300));
        let hurried = client.append(hurried).await.unwrap_err();
        assert_eq!(
            (hurried.code(), reason(&hurried).as_str()),
            (Code::DeadlineExceeded, "CALL_TIMED_OUT")
        );

        // Health follows the record: a disabled tenant makes the service report not serving, and appends answer
        // FAILED_PRECONDITION; enabling it again restores both.
        let toggle = |enabled: bool| {
            let owner_pool = owner_pool.clone();
            async move {
                owner_pool
                    .get()
                    .await
                    .unwrap()
                    .execute(
                        "UPDATE enrichedlab_agent.broker_tenants SET enabled = $1 WHERE tenant_id = 'evidence-site'",
                        &[&enabled],
                    )
                    .await
                    .unwrap();
            }
        };
        let wait_for = |want: bool| {
            let probe = probe.clone();
            async move {
                for _ in 0..40 {
                    if super::probe(probe.clone()).await.is_ok() == want {
                        return true;
                    }
                    tokio::time::sleep(std::time::Duration::from_millis(250)).await;
                }
                false
            }
        };
        toggle(false).await;
        assert!(
            wait_for(false).await,
            "health kept serving with the tenant disabled"
        );
        let status = client
            .append(request(
                Some("good-token"),
                &format!("{key_prefix}-disabled"),
                payload,
                RUN,
                "sandbox.receipt",
                "evidence-site",
            ))
            .await
            .unwrap_err();
        assert_eq!(
            (status.code(), reason(&status).as_str()),
            (Code::FailedPrecondition, "TENANT_DISABLED")
        );
        toggle(true).await;
        assert!(
            wait_for(true).await,
            "health did not return after the tenant was enabled"
        );

        // Health also follows the producer's authority, the role's privilege and the TokenReview API.
        let owner_sql = |sql: &'static str| {
            let owner_pool = owner_pool.clone();
            async move {
                owner_pool
                    .get()
                    .await
                    .unwrap()
                    .batch_execute(sql)
                    .await
                    .unwrap();
            }
        };
        owner_sql("UPDATE evidence.producers SET enabled = false WHERE identity = 'openshell/sandbox-qual'").await;
        assert!(
            wait_for(false).await,
            "health kept serving with the producer disabled"
        );
        owner_sql("UPDATE evidence.producers SET enabled = true WHERE identity = 'openshell/sandbox-qual'").await;
        assert!(
            wait_for(true).await,
            "health did not return after the producer was enabled"
        );
        owner_sql(
            "REVOKE EXECUTE ON FUNCTION evidence.append(text,text,text,text,uuid,uuid,text,text,timestamptz,text,text) \
             FROM evidence_adapter_qual",
        )
        .await;
        assert!(
            wait_for(false).await,
            "health kept serving without the append privilege"
        );
        owner_sql(
            "GRANT EXECUTE ON FUNCTION evidence.append(text,text,text,text,uuid,uuid,text,text,timestamptz,text,text) \
             TO evidence_adapter_qual",
        )
        .await;
        assert!(
            wait_for(true).await,
            "health did not return after the privilege was granted"
        );
        reviewer_down.store(true, std::sync::atomic::Ordering::Relaxed);
        assert!(
            wait_for(false).await,
            "health kept serving with TokenReview unavailable"
        );
        reviewer_down.store(false, std::sync::atomic::Ordering::Relaxed);
        assert!(
            wait_for(true).await,
            "health did not return after TokenReview recovered"
        );

        // A health request larger than a service name is refused before it is kept.
        let oversized = health_client(&client_channel)
            .check(tonic_health::pb::HealthCheckRequest {
                service: "x".repeat(4096),
            })
            .await
            .unwrap_err();
        assert_eq!(oversized.code(), Code::OutOfRange, "{oversized:?}");

        // Health is Check only: no Watch stream outlives its call.
        let mut health = health_client(&client_channel);
        let watch = health
            .watch(tonic_health::pb::HealthCheckRequest {
                service: String::new(),
            })
            .await
            .unwrap_err();
        assert_eq!(watch.code(), Code::Unimplemented);

        // Silent clients hold a connection slot only until the TLS handshake deadline. Every slot is taken (the
        // client's connection and silent sockets; those past the limit are closed on arrival), so a probe cannot
        // connect; once the deadline passes, it can.
        let silent: Vec<_> = futures_util::future::join_all(
            (0..limits.connections).map(|_| tokio::net::TcpStream::connect(address)),
        )
        .await;
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
        assert!(
            super::probe(probe.clone()).await.is_err(),
            "a probe connected with every slot taken"
        );
        tokio::time::sleep(std::time::Duration::from_millis(1500)).await;
        assert!(
            super::probe(probe.clone()).await.is_ok(),
            "silent clients kept their slots past the handshake deadline"
        );
        drop(silent);

        // Every outcome reached telemetry with its code, including refusals at the limit and past the deadline.
        provider.force_flush().unwrap();
        // (method, code) -> calls, from the cumulative exports.
        let mut counts = std::collections::BTreeMap::<(String, String), u64>::new();
        for resource in exporter.get_finished_metrics().unwrap() {
            for scope in resource.scope_metrics() {
                for metric in scope
                    .metrics()
                    .filter(|metric| metric.name() == "rpc.server.requests")
                {
                    if let opentelemetry_sdk::metrics::data::AggregatedMetrics::U64(
                        opentelemetry_sdk::metrics::data::MetricData::Sum(sum),
                    ) = metric.data()
                    {
                        for point in sum.data_points() {
                            let value = |key: &str| {
                                point
                                    .attributes()
                                    .find(|attribute| attribute.key.as_str() == key)
                                    .map(|attribute| attribute.value.to_string())
                                    .unwrap_or_default()
                            };
                            let entry = counts
                                .entry((value("rpc.method"), value("rpc.grpc.status_code")))
                                .or_default();
                            *entry = (*entry).max(point.value());
                        }
                    }
                }
            }
        }
        let codes: std::collections::BTreeSet<_> =
            counts.keys().map(|(_, code)| code.clone()).collect();
        for code in ["0", "3", "4", "6", "7", "8", "9", "12", "16"] {
            assert!(
                codes.contains(code),
                "status code {code} missing from telemetry: {counts:?}"
            );
        }
        // The service deadline and the caller's deadline both reached telemetry.
        assert_eq!(
            counts.get(&("Append".into(), "4".into())),
            Some(&2),
            "{counts:?}"
        );

        stop.send(()).ok();
        server_task.await.unwrap().unwrap();
        std::fs::remove_dir_all(dir).ok();
    }
}
