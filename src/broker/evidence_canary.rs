//! Custody canary: a client of the evidence append service that stores random bytes through `StoreObject`, waits for
//! the claim to be ready, and appends one `custody.canary` record whose lineage binds the claim. It proves the live
//! path from a producer's token to both copies and the record, and holds no research data and no store credentials.
use std::{fs, path::PathBuf, time::Duration};

use anyhow::{Context, Result, bail};
use clap::Args;
use sha2::{Digest, Sha256};
use tonic::{
    Request,
    transport::{Certificate, Channel, ClientTlsConfig},
};

use super::evidence_append::pb::{
    AppendRequest, AwaitClaimRequest, ScopeKind, StoreObjectHeader, StoreObjectRequest,
    evidence_append_service_client::EvidenceAppendServiceClient, store_object_request::Part,
};

/// Largest canary object; the service's own bound is larger.
const MAX_CANARY_BYTES: usize = 16 * 1024 * 1024;
const CHUNK_BYTES: usize = 64 * 1024;

#[derive(Clone, Debug, Args)]
pub struct EvidenceCanaryArgs {
    /// The service's address, as its certificate names it.
    #[arg(long)]
    pub address: String,

    /// Name the server certificate must carry.
    #[arg(long)]
    pub server_name: String,

    /// CA bundle (PEM) that signed the server certificate.
    #[arg(long)]
    pub ca_file: PathBuf,

    /// The producer's projected ServiceAccount token (audience as the service requires).
    #[arg(long)]
    pub token_file: PathBuf,

    /// Tenant the record belongs to.
    #[arg(long)]
    pub tenant: String,

    /// Lab (research group) whose standalone scope the canary stores and records in.
    #[arg(long)]
    pub group: String,

    /// Size of the random object.
    #[arg(long, default_value_t = 256 * 1024)]
    pub bytes: usize,
}

/// Runs the canary and prints one JSON line of ids, digests, class and flags.
pub async fn run(args: EvidenceCanaryArgs) -> Result<()> {
    if args.bytes == 0 || args.bytes > MAX_CANARY_BYTES {
        bail!("the canary size must be 1 byte to 16 MiB");
    }
    let ca = fs::read(&args.ca_file).context("failed to read the CA bundle")?;
    let token = fs::read_to_string(&args.token_file).context("failed to read the token file")?;
    let token = token.trim().to_owned();
    let channel = Channel::from_shared(args.address.clone())
        .context("invalid service address")?
        .tls_config(
            ClientTlsConfig::new()
                .ca_certificate(Certificate::from_pem(ca))
                .domain_name(args.server_name.clone()),
        )
        .context("invalid TLS configuration")?
        .connect_timeout(Duration::from_secs(5))
        .connect()
        .await
        .context("the evidence append service is not reachable")?;
    let mut client = EvidenceAppendServiceClient::new(channel);

    let mut bytes = vec![0_u8; args.bytes];
    getrandom::fill(&mut bytes).context("no system randomness")?;
    let digest = hex::encode(Sha256::digest(&bytes));
    let run = uuid::Uuid::new_v4().simple().to_string();
    let mut messages = vec![StoreObjectRequest {
        part: Some(Part::Header(StoreObjectHeader {
            tenant_id: args.tenant.clone(),
            group_key: args.group.clone(),
            scope_kind: ScopeKind::Standalone as i32,
            project_id: String::new(),
            run_id: String::new(),
            media_type: "application/octet-stream".into(),
            raw_sha256: digest.clone(),
            raw_size: bytes.len() as u64,
            idempotency_key: format!("canary-{run}"),
        })),
    }];
    messages.extend(bytes.chunks(CHUNK_BYTES).map(|chunk| StoreObjectRequest {
        part: Some(Part::Chunk(chunk.to_vec())),
    }));
    let mut request = authorised(&token, Request::new(tokio_stream::iter(messages)))?;
    request.set_timeout(Duration::from_secs(110));
    let mut claim = client
        .store_object(request)
        .await
        .context("StoreObject was refused")?
        .into_inner()
        .claim
        .context("StoreObject answered without a claim")?;
    let waited = !claim.ready;
    if waited {
        let mut request = authorised(
            &token,
            Request::new(AwaitClaimRequest {
                tenant_id: args.tenant.clone(),
                claim_id: claim.claim_id.clone(),
            }),
        )?;
        request.set_timeout(Duration::from_secs(90));
        claim = client
            .await_claim(request)
            .await
            .context("AwaitClaim was refused")?
            .into_inner()
            .claim
            .context("AwaitClaim answered without a claim")?;
    }
    if !claim.ready {
        bail!("the claim is not ready after waiting");
    }
    if claim.raw_sha256 != digest {
        bail!("the claim names other bytes than the canary sent");
    }

    let payload = serde_json::json!({"lineage": {
        "version": 1,
        "activity": {"key": format!("canary-{run}"), "kind": "tool"},
        "entities": [{"key": format!("canary:{run}"), "kind": "artifact", "disclosure": "lab", "claim": claim.claim_id}],
        "generated": [{"entity": format!("canary:{run}"), "role": "output"}],
    }})
    .to_string();
    let now = time::OffsetDateTime::now_utc();
    let micros = now.unix_timestamp_nanos() / 1_000;
    let request = authorised(
        &token,
        Request::new(AppendRequest {
            tenant_id: args.tenant.clone(),
            group_key: args.group.clone(),
            scope_kind: ScopeKind::Standalone as i32,
            project_id: String::new(),
            run_id: String::new(),
            event_type: "custody.canary".into(),
            idempotency_key: format!("canary-record-{run}"),
            source_time: Some(prost_types::Timestamp {
                seconds: i64::try_from(micros / 1_000_000)?,
                nanos: i32::try_from((micros % 1_000_000) * 1_000)?,
            }),
            payload_sha256: hex::encode(Sha256::digest(payload.as_bytes())),
            payload: payload.into_bytes(),
        }),
    )?;
    let receipt = client
        .append(request)
        .await
        .context("the canary record was refused")?
        .into_inner();
    println!(
        "{}",
        serde_json::json!({
            "claim_id": claim.claim_id,
            "raw_sha256": claim.raw_sha256,
            "raw_size": bytes.len(),
            "object_version": claim.object_version,
            "retention_class": claim.retention_class,
            "waited_for_replica": waited,
            "record_id": receipt.record_id,
            "sequence": receipt.sequence,
            "entity": format!("canary:{run}"),
        })
    );
    Ok(())
}

fn authorised<T>(token: &str, mut request: Request<T>) -> Result<Request<T>> {
    request.metadata_mut().insert(
        "authorization",
        format!("Bearer {token}")
            .parse()
            .context("the token is not a header value")?,
    );
    Ok(request)
}
