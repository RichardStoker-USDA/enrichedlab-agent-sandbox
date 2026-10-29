//! Evidence object custody in the evidence append service (`StoreObject`, `AwaitClaim` and the reconciler).
//!
//! The service is the only custody writer. For a producer it authenticated, it asks the record for the lab's two
//! stores and the scope's retention floor (`evidence.custody_authorise`), takes the producer's bytes in bounded chunks
//! while hashing them, writes the primary copy only under a new key, reads it back in full and checks its digest, and
//! extends its retention to the floor when the bucket default falls short. The record then issues an immutable claim
//! bound to that producer and scope (`evidence.custody_issue_claim`). The replica is the store's own replication of
//! the same version: the service waits for it, reads it back in full, extends it to the floor if needed, records the
//! copy and appends the claim's `ready` event (`evidence.custody_mark_ready`). A record binds only a ready claim.
//!
//! The reconciler finishes waiting claims, re-reads copies whose last verification is old (recording a new
//! verification or `copy_missing`), applies the retention a project's acceptance asked for on both copies, and notes a
//! new database epoch. No identity here can delete an object version.
use std::{collections::HashMap, fs, path::PathBuf, sync::Arc, time::Duration};

use anyhow::{Context, Result, anyhow, bail};
use deadpool_postgres::Pool;
use serde::Deserialize;
use sha2::{Digest, Sha256};
use time::{OffsetDateTime, format_description::well_known::Rfc3339};
use tonic::{Code, Status, Streaming};
use uuid::Uuid;

use super::evidence_append::pb::{
    ClaimState, StoreObjectHeader, StoreObjectRequest, store_object_request::Part,
};
use super::evidence_append::{database_status, refusal, scope_text};
use super::evidence_objects::{
    EndpointConfig, Put, S3Store, StoreError, decode, encode, is_sha256_hex, object_key, sha256_hex,
};

/// The largest object this version stores (raw bytes), and the largest chunk a call may send.
pub const MAX_OBJECT_BYTES: u64 = 64 * 1024 * 1024;
pub const MAX_CHUNK_BYTES: usize = 64 * 1024;
/// zstd never grows data by more than a small bound; reads of stored bytes are refused past this.
fn max_stored(raw_size: u64) -> u64 {
    raw_size + raw_size / 128 + 1024
}

/// The private custody configuration: the CA that signed the stores' certificates and each endpoint the record's
/// `custody_stores` rows name, with its own identity.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CustodyConfig {
    pub ca_file: PathBuf,
    pub endpoints: HashMap<String, EndpointConfig>,
    /// How long `StoreObject` waits for the replica before answering a waiting claim.
    #[serde(default = "default_replica_wait")]
    pub replica_wait_seconds: u64,
}

fn default_replica_wait() -> u64 {
    60
}

pub struct Custody {
    stores: HashMap<String, Arc<S3Store>>,
    replica_wait: Duration,
}

impl Custody {
    pub fn load(path: &PathBuf) -> Result<Self> {
        let text = fs::read_to_string(path).context("cannot read the custody configuration")?;
        let config: CustodyConfig =
            serde_json::from_str(&text).context("the custody configuration is invalid")?;
        let ca = fs::read(&config.ca_file).context("cannot read the custody CA bundle")?;
        if config.endpoints.is_empty() || config.endpoints.len() > 64 {
            bail!("the custody configuration names no endpoint or too many");
        }
        let mut stores = HashMap::new();
        for (name, endpoint) in &config.endpoints {
            if !(2..=63).contains(&name.len())
                || !name
                    .bytes()
                    .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
            {
                bail!("a custody endpoint name is not plain");
            }
            stores.insert(
                name.clone(),
                Arc::new(
                    S3Store::connect(endpoint, &ca)
                        .with_context(|| format!("custody endpoint {name}"))?,
                ),
            );
        }
        Ok(Self {
            stores,
            replica_wait: Duration::from_secs(config.replica_wait_seconds.clamp(1, 110)),
        })
    }

    pub(crate) fn store(&self, endpoint: &str) -> Result<Arc<S3Store>, Status> {
        self.stores
            .get(endpoint)
            .cloned()
            .ok_or_else(|| refusal(Code::FailedPrecondition, "custody_store_missing"))
    }
}

fn store_status(error: StoreError) -> Status {
    match error {
        StoreError::Unavailable => refusal(Code::Unavailable, "custody_store_unavailable"),
        StoreError::NotFound => refusal(Code::Unavailable, "custody_store_unavailable"),
        StoreError::Refused(_) | StoreError::Malformed(_) => {
            refusal(Code::Internal, "custody_store_error")
        }
    }
}

fn parse_time(text: &str) -> Result<OffsetDateTime, Status> {
    OffsetDateTime::parse(text, &Rfc3339).map_err(|_| refusal(Code::Internal, "record_error"))
}

fn timestamp(time: OffsetDateTime) -> prost_types::Timestamp {
    prost_types::Timestamp {
        seconds: time.unix_timestamp(),
        nanos: i32::try_from(time.nanosecond()).unwrap_or(0),
    }
}

/// A claim as the record answers it.
#[derive(Debug, Deserialize)]
pub struct ClaimAnswer {
    pub claim_id: String,
    pub object_id: String,
    pub raw_sha256: String,
    pub raw_size: u64,
    pub class: String,
    pub floor_until: String,
    pub expires_at: String,
    pub primary_version: String,
    pub ready: bool,
    pub replayed: bool,
}

impl ClaimAnswer {
    pub fn state(&self) -> Result<ClaimState, Status> {
        Ok(ClaimState {
            claim_id: self.claim_id.clone(),
            raw_sha256: self.raw_sha256.clone(),
            raw_size: self.raw_size,
            object_version: self.primary_version.clone(),
            ready: self.ready,
            expires_at: Some(timestamp(parse_time(&self.expires_at)?)),
            retention_class: self.class.clone(),
            floor_until: Some(timestamp(parse_time(&self.floor_until)?)),
            replayed: self.replayed,
        })
    }
}

#[derive(Debug, Deserialize)]
struct StoreRef {
    bucket: String,
    endpoint: String,
}

#[derive(Debug, Deserialize)]
struct Authorised {
    primary: StoreRef,
    replica: StoreRef,
    floor_until: String,
}

fn optional_uuid(value: &str) -> Result<Option<Uuid>, Status> {
    if value.is_empty() {
        return Ok(None);
    }
    Uuid::parse_str(value)
        .map(Some)
        .map_err(|_| refusal(Code::InvalidArgument, "scope_invalid"))
}

fn header_valid(header: &StoreObjectHeader) -> Result<(), Status> {
    let media_ok = header.media_type.len() <= 128
        && header
            .media_type
            .split_once('/')
            .is_some_and(|(kind, sub)| {
                !kind.is_empty()
                    && !sub.is_empty()
                    && kind.bytes().all(|b| {
                        b.is_ascii_lowercase() || b.is_ascii_digit() || b"+-.".contains(&b)
                    })
                    && sub
                        .bytes()
                        .all(|b| b.is_ascii_alphanumeric() || b"+-.".contains(&b))
            });
    let key_ok = (8..=128).contains(&header.idempotency_key.len())
        && header
            .idempotency_key
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"._:-".contains(&b));
    if !media_ok || !key_ok || !is_sha256_hex(&header.raw_sha256) {
        return Err(refusal(Code::InvalidArgument, "argument_invalid"));
    }
    if header.raw_size > MAX_OBJECT_BYTES {
        return Err(refusal(Code::InvalidArgument, "custody_size"));
    }
    Ok(())
}

/// Reads the header and every chunk, hashing as it goes. Refuses a stream that does not start with a header, a chunk
/// past its bound, more bytes than the header states, or bytes that do not hash to it.
async fn receive(
    mut stream: Streaming<StoreObjectRequest>,
) -> Result<(StoreObjectHeader, Vec<u8>), Status> {
    let first = stream
        .message()
        .await?
        .ok_or_else(|| refusal(Code::InvalidArgument, "argument_missing"))?;
    let Some(Part::Header(header)) = first.part else {
        return Err(refusal(Code::InvalidArgument, "argument_missing"));
    };
    header_valid(&header)?;
    let mut bytes = Vec::with_capacity(usize::try_from(header.raw_size).unwrap_or(0));
    let mut hasher = Sha256::new();
    while let Some(message) = stream.message().await? {
        let Some(Part::Chunk(chunk)) = message.part else {
            return Err(refusal(Code::InvalidArgument, "argument_invalid"));
        };
        if chunk.len() > MAX_CHUNK_BYTES
            || bytes.len() as u64 + chunk.len() as u64 > header.raw_size
        {
            return Err(refusal(Code::InvalidArgument, "custody_size"));
        }
        hasher.update(&chunk);
        bytes.extend_from_slice(&chunk);
    }
    if bytes.len() as u64 != header.raw_size {
        return Err(refusal(Code::InvalidArgument, "custody_size"));
    }
    if hex::encode(hasher.finalize()) != header.raw_sha256 {
        return Err(refusal(Code::InvalidArgument, "custody_digest_mismatch"));
    }
    Ok((header, bytes))
}

/// One copy read back in full: its version's facts after any retention extension, and its stored bytes' digest.
struct Verified {
    version: String,
    stored_size: u64,
    stored_sha256: String,
    retention_mode: Option<String>,
    retain_until: Option<OffsetDateTime>,
    legal_hold: bool,
}

/// Reads one version back in full, checks it decodes to the expected raw digest and size, and extends its retention
/// to `floor` when it falls short (reading the facts back afterwards).
async fn verify_copy(
    store: &S3Store,
    bucket: &str,
    key: &str,
    version: &str,
    raw_sha256: &str,
    raw_size: u64,
    floor: OffsetDateTime,
) -> Result<Verified, StoreError> {
    let stored = store
        .get_version(bucket, key, version, max_stored(raw_size))
        .await?;
    let raw = decode(&stored, usize::try_from(raw_size).unwrap_or(usize::MAX))
        .map_err(|_| StoreError::Malformed("stored bytes do not decode"))?;
    if raw.len() as u64 != raw_size || sha256_hex(&raw) != raw_sha256 {
        return Err(StoreError::Malformed(
            "stored bytes do not match their digest",
        ));
    }
    let mut facts = store.head(bucket, key, Some(version)).await?;
    // Kept to a day past the floor: the claim's own floor is taken moments later, and a clock difference between here
    // and the store must not leave either copy short.
    let target = floor + time::Duration::days(1);
    if floor > OffsetDateTime::UNIX_EPOCH && facts.retain_until.is_none_or(|until| until < target) {
        store.put_retention(bucket, key, version, target).await?;
        facts = store.head(bucket, key, Some(version)).await?;
    }
    if facts.stored_size != stored.len() as u64 {
        return Err(StoreError::Malformed("the copy's size changed"));
    }
    Ok(Verified {
        version: facts.version_id,
        stored_size: facts.stored_size,
        stored_sha256: sha256_hex(&stored),
        retention_mode: facts.retention_mode,
        retain_until: facts.retain_until,
        legal_hold: facts.legal_hold,
    })
}

/// `StoreObject`: the bytes into the lab's primary copy, a claim, and the replica if it arrives in time.
pub async fn store_object(
    pool: &Pool,
    custody: &Custody,
    tenant: &str,
    producer: &str,
    stream: Streaming<StoreObjectRequest>,
) -> Result<ClaimState, Status> {
    let (header, raw) = receive(stream).await?;
    if header.tenant_id != tenant {
        return Err(refusal(Code::PermissionDenied, "tenant_not_served"));
    }
    let scope = scope_text(header.scope_kind)?;
    let project = optional_uuid(&header.project_id)?;
    let run = optional_uuid(&header.run_id)?;
    let client = pool
        .get()
        .await
        .map_err(|_| refusal(Code::Unavailable, "record_unavailable"))?;
    let row = client
        .query_one(
            "SELECT evidence.custody_authorise($1,$2,$3,$4,$5,$6)::text",
            &[
                &tenant,
                &producer,
                &header.group_key,
                &scope,
                &project,
                &run,
            ],
        )
        .await
        .map_err(database_status)?;
    let authorised: Authorised = serde_json::from_str(row.get::<_, &str>(0))
        .map_err(|_| refusal(Code::Internal, "record_error"))?;
    let floor = parse_time(&authorised.floor_until)?;
    let primary = custody.store(&authorised.primary.endpoint)?;
    // The replica must be configured, and be another store, before anything is written.
    let replica = custody.store(&authorised.replica.endpoint)?;
    if primary.origin() == replica.origin() {
        return Err(refusal(Code::FailedPrecondition, "custody_store_missing"));
    }
    let key = object_key(&header.raw_sha256)
        .map_err(|_| refusal(Code::InvalidArgument, "argument_invalid"))?;
    let stored = encode(&raw).map_err(|_| refusal(Code::Internal, "custody_store_error"))?;
    drop(raw);

    // The primary copy: a new key, or the version already there, read back either way.
    let version = match primary
        .put_new(&authorised.primary.bucket, &key, stored)
        .await
        .map_err(store_status)?
    {
        Put::Created { version_id } => version_id,
        Put::Exists => {
            primary
                .head(&authorised.primary.bucket, &key, None)
                .await
                .map_err(store_status)?
                .version_id
        }
    };
    let copy = match verify_copy(
        &primary,
        &authorised.primary.bucket,
        &key,
        &version,
        &header.raw_sha256,
        header.raw_size,
        floor,
    )
    .await
    {
        Ok(copy) => copy,
        Err(StoreError::Malformed(_)) => {
            return Err(refusal(Code::DataLoss, "custody_object_conflict"));
        }
        Err(other) => return Err(store_status(other)),
    };
    let size = i64::try_from(header.raw_size)
        .map_err(|_| refusal(Code::InvalidArgument, "custody_size"))?;
    let stored_size = i64::try_from(copy.stored_size)
        .map_err(|_| refusal(Code::Internal, "custody_store_error"))?;
    let row = client
        .query_one(
            "SELECT evidence.custody_issue_claim($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,$13,$14,$15,$16)::text",
            &[
                &tenant,
                &producer,
                &header.group_key,
                &scope,
                &project,
                &run,
                &header.idempotency_key,
                &header.raw_sha256,
                &size,
                &header.media_type,
                &copy.version,
                &stored_size,
                &copy.stored_sha256,
                &copy.retention_mode,
                &copy.retain_until,
                &copy.legal_hold,
            ],
        )
        .await
        .map_err(database_status)?;
    let claim: ClaimAnswer = serde_json::from_str(row.get::<_, &str>(0))
        .map_err(|_| refusal(Code::Internal, "record_error"))?;
    drop(client);
    if claim.ready {
        return claim.state();
    }
    let claim_id =
        Uuid::parse_str(&claim.claim_id).map_err(|_| refusal(Code::Internal, "record_error"))?;
    let deadline = tokio::time::Instant::now() + custody.replica_wait;
    finish_until(pool, custody, tenant, producer, claim_id, deadline).await
}

/// `AwaitClaim`: the producer's own claim, finished if its replica has arrived, until `deadline`.
pub async fn await_claim(
    pool: &Pool,
    custody: &Custody,
    tenant: &str,
    producer: &str,
    claim: &str,
    deadline: tokio::time::Instant,
) -> Result<ClaimState, Status> {
    let claim_id = Uuid::parse_str(claim)
        .map_err(|_| refusal(Code::InvalidArgument, "custody_claim_unknown"))?;
    finish_until(pool, custody, tenant, producer, claim_id, deadline).await
}

async fn claim_answer(
    pool: &Pool,
    tenant: &str,
    producer: &str,
    claim: Uuid,
) -> Result<ClaimAnswer, Status> {
    let client = pool
        .get()
        .await
        .map_err(|_| refusal(Code::Unavailable, "record_unavailable"))?;
    let row = client
        .query_one(
            "SELECT evidence.custody_claim($1,$2,$3)::text",
            &[&tenant, &producer, &claim],
        )
        .await
        .map_err(database_status)?;
    serde_json::from_str(row.get::<_, &str>(0)).map_err(|_| refusal(Code::Internal, "record_error"))
}

/// Tries to finish the claim (replica read back, recorded, `ready`) until it is ready or `deadline` passes, then
/// answers it as it stands.
async fn finish_until(
    pool: &Pool,
    custody: &Custody,
    tenant: &str,
    producer: &str,
    claim: Uuid,
    deadline: tokio::time::Instant,
) -> Result<ClaimState, Status> {
    loop {
        let answer = claim_answer(pool, tenant, producer, claim).await?;
        if answer.ready {
            return answer.state();
        }
        match finish_claim(pool, custody, tenant, Some(claim)).await {
            Ok(_) => {}
            Err(error) => tracing::debug!(error = %error, "a claim's replica is not finished yet"),
        }
        let answer = claim_answer(pool, tenant, producer, claim).await?;
        if answer.ready || tokio::time::Instant::now() + Duration::from_millis(500) >= deadline {
            return answer.state();
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
}

/// Finishes waiting claims (one, or up to a batch): reads the replica version back in full, extends it to the
/// claim's floor if needed, records the copy and appends `ready`. A claim that cannot be finished now is left for the
/// next pass and never stops the others. Returns how many became ready.
pub async fn finish_claim(
    pool: &Pool,
    custody: &Custody,
    tenant: &str,
    only: Option<Uuid>,
) -> Result<usize> {
    let client = pool.get().await.context("the record is unreachable")?;
    let rows = client
        .query(
            "SELECT * FROM evidence.custody_waiting($1, 100, $2)",
            &[&tenant, &only],
        )
        .await
        .context("waiting claims")?;
    let mut ready = 0;
    for row in rows {
        let claim: Uuid = row.get("claim_id");
        match finish_one(&client, custody, tenant, &row).await {
            Ok(true) => ready += 1,
            Ok(false) => {}
            Err(error) => {
                tracing::warn!(claim = %claim, error = %error, "a custody claim was not finished")
            }
        }
    }
    Ok(ready)
}

async fn finish_one(
    client: &deadpool_postgres::Object,
    custody: &Custody,
    tenant: &str,
    row: &tokio_postgres::Row,
) -> Result<bool> {
    let claim: Uuid = row.get("claim_id");
    let producer: String = row.get("producer");
    let object: Uuid = row.get("object_id");
    let key: String = row.get("object_key");
    let raw_sha: String = row.get("raw_sha256");
    let raw_size: i64 = row.get("raw_size");
    let version: String = row.get("primary_version");
    let stored_sha: String = row.get("stored_sha256");
    let floor: OffsetDateTime = row.get("floor_until");
    let bucket: String = row.get("replica_bucket");
    let endpoint: String = row.get("replica_endpoint");
    let store = custody
        .store(&endpoint)
        .map_err(|_| anyhow!("the replica endpoint is not configured"))?;
    let raw_size = u64::try_from(raw_size).map_err(|_| anyhow!("negative size"))?;
    match store.head(&bucket, &key, Some(&version)).await {
        Ok(_) => {}
        Err(StoreError::NotFound) => return Ok(false),
        Err(error) => bail!("replica unreachable: {error}"),
    }
    let copy = match verify_copy(&store, &bucket, &key, &version, &raw_sha, raw_size, floor).await {
        Ok(copy) if copy.stored_sha256 == stored_sha => copy,
        Ok(_) | Err(StoreError::Malformed(_)) => {
            bail!("the replica holds other bytes for the claim's version")
        }
        Err(error) => bail!("replica read failed: {error}"),
    };
    let stored_size = i64::try_from(copy.stored_size)?;
    let copy_id: Uuid = client
        .query_one(
            "SELECT evidence.custody_record_copy($1,$2,'replica',$3,$4,$5,$6,$7,$8)",
            &[
                &tenant,
                &object,
                &copy.version,
                &stored_size,
                &copy.stored_sha256,
                &copy.retention_mode,
                &copy.retain_until,
                &copy.legal_hold,
            ],
        )
        .await
        .context("record the replica copy")?
        .get(0);
    client
        .query_one(
            "SELECT evidence.custody_mark_ready($1,$2,$3,$4)::text",
            &[&tenant, &producer, &claim, &copy_id],
        )
        .await
        .context("mark the claim ready")?;
    Ok(true)
}

/// Re-reads copies whose last verification is older than `age`: a full read and digest check records a new
/// verification; a copy that is gone or holds other bytes gets `copy_missing` (which invalidates every verification of
/// that physical version). A copy that cannot be reached now is left for the next pass. Returns (verified, missing).
pub async fn reverify(
    pool: &Pool,
    custody: &Custody,
    tenant: &str,
    age: Duration,
) -> Result<(usize, usize)> {
    let client = pool.get().await.context("the record is unreachable")?;
    let interval = format!("{} seconds", age.as_secs());
    let rows = client
        .query(
            "SELECT * FROM evidence.custody_due_copies($1, $2::text::interval, 50)",
            &[&tenant, &interval],
        )
        .await
        .context("copies due for verification")?;
    let (mut verified, mut missing) = (0, 0);
    for row in rows {
        let object: Uuid = row.get("object_id");
        let copy_id: Uuid = row.get("copy_id");
        let role: String = row.get("copy_role");
        let bucket: String = row.get("bucket");
        let endpoint: String = row.get("endpoint");
        let key: String = row.get("object_key");
        let raw_sha: String = row.get("raw_sha256");
        let raw_size: i64 = row.get("raw_size");
        let version: String = row.get("version_id");
        let stored_sha: String = row.get("stored_sha256");
        let Ok(store) = custody.store(&endpoint) else {
            tracing::warn!(object = %object, role = %role, "a custody copy's endpoint is not configured");
            continue;
        };
        let Ok(raw_size) = u64::try_from(raw_size) else {
            continue;
        };
        // A floor in the past: re-verification reads, it does not extend.
        let outcome = verify_copy(
            &store,
            &bucket,
            &key,
            &version,
            &raw_sha,
            raw_size,
            OffsetDateTime::UNIX_EPOCH,
        )
        .await;
        let recorded = match outcome {
            Ok(copy) if copy.stored_sha256 == stored_sha => {
                let stored_size = i64::try_from(copy.stored_size).unwrap_or(-1);
                verified += 1;
                client
                    .query_one(
                        "SELECT evidence.custody_record_copy($1,$2,$3,$4,$5,$6,$7,$8,$9)",
                        &[
                            &tenant,
                            &object,
                            &role,
                            &copy.version,
                            &stored_size,
                            &copy.stored_sha256,
                            &copy.retention_mode,
                            &copy.retain_until,
                            &copy.legal_hold,
                        ],
                    )
                    .await
                    .map(|_| ())
            }
            Ok(_) | Err(StoreError::NotFound) | Err(StoreError::Malformed(_)) => {
                tracing::warn!(object = %object, role = %role, "a custody copy is missing or holds other bytes");
                missing += 1;
                client
                    .query_one(
                        "SELECT evidence.custody_observe($1,$2,$3,'copy_missing',\
                         jsonb_build_object('read_back','failed'))",
                        &[&tenant, &object, &copy_id],
                    )
                    .await
                    .map(|_| ())
            }
            Err(error) => {
                tracing::warn!(object = %object, role = %role, error = %error, "a custody copy was not re-read");
                Ok(())
            }
        };
        if let Err(error) = recorded {
            tracing::warn!(object = %object, error = %error, "a custody re-read was not recorded");
        }
    }
    Ok((verified, missing))
}

/// Applies retention a project's acceptance asked for: extends each pinned copy, reads it back, and records
/// `retention_applied` with the date the store now holds, or `retention_failed`, which moves that copy behind the
/// others for the next pass, so one unreachable copy never holds up the rest. Then completes every acceptance whose
/// copies are all covered. Returns how many copies were extended.
pub async fn apply_retention(pool: &Pool, custody: &Custody, tenant: &str) -> Result<usize> {
    let client = pool.get().await.context("the record is unreachable")?;
    let rows = client
        .query(
            "SELECT * FROM evidence.custody_retention_pending($1, 50)",
            &[&tenant],
        )
        .await
        .context("pending retention")?;
    let mut applied = 0;
    for row in rows {
        let object: Uuid = row.get("object_id");
        let copy: Uuid = row.get("copy_id");
        let bucket: String = row.get("bucket");
        let endpoint: String = row.get("endpoint");
        let key: String = row.get("object_key");
        let version: String = row.get("version_id");
        let until: OffsetDateTime = row.get("until");
        let outcome = async {
            let store = custody
                .store(&endpoint)
                .map_err(|_| anyhow!("the endpoint is not configured"))?;
            let facts = match store.head(&bucket, &key, Some(&version)).await {
                Ok(facts) if facts.retain_until.is_some_and(|held| held >= until) => facts,
                Ok(_) => {
                    store
                        .put_retention(&bucket, &key, &version, until + time::Duration::days(1))
                        .await
                        .map_err(|error| anyhow!("retention extension failed: {error}"))?;
                    store
                        .head(&bucket, &key, Some(&version))
                        .await
                        .map_err(|error| anyhow!("retention read back failed: {error}"))?
                }
                Err(error) => bail!("copy unreachable: {error}"),
            };
            let held = facts
                .retain_until
                .ok_or_else(|| anyhow!("the copy reports no retention"))?;
            if held < until {
                bail!("the copy holds an earlier retention than asked");
            }
            Ok::<_, anyhow::Error>(held.format(&Rfc3339)?)
        }
        .await;
        let recorded = match outcome {
            Ok(held) => {
                applied += 1;
                client
                    .query_one(
                        "SELECT evidence.custody_observe($1,$2,$3,'retention_applied',\
                         jsonb_build_object('retain_until',$4::text))",
                        &[&tenant, &object, &copy, &held],
                    )
                    .await
                    .map(|_| ())
            }
            Err(error) => {
                tracing::warn!(object = %object, error = %error, "custody retention not applied to a copy");
                client
                    .query_one(
                        "SELECT evidence.custody_observe($1,$2,$3,'retention_failed','{}'::jsonb)",
                        &[&tenant, &object, &copy],
                    )
                    .await
                    .map(|_| ())
            }
        };
        if let Err(error) = recorded {
            tracing::warn!(object = %object, error = %error, "a custody retention outcome was not recorded");
        }
    }
    let completed: i32 = client
        .query_one(
            "SELECT evidence.custody_complete_acceptances($1)",
            &[&tenant],
        )
        .await
        .context("complete acceptances")?
        .get(0);
    if completed > 0 {
        tracing::info!(
            completed,
            "project acceptances complete: retention in place on every copy"
        );
    }
    Ok(applied)
}

/// One reconciler pass: the epoch, waiting claims, retention, and copies not verified for 30 days.
pub async fn reconcile(pool: &Pool, custody: &Custody, tenant: &str) {
    if let Ok(client) = pool.get().await
        && let Err(error) = client
            .query_one("SELECT evidence.custody_epoch_check($1)::text", &[&tenant])
            .await
    {
        tracing::warn!(error = %error, "custody epoch check failed");
    }
    match finish_claim(pool, custody, tenant, None).await {
        Ok(count) if count > 0 => tracing::info!(count, "custody claims made ready"),
        Ok(_) => {}
        Err(error) => tracing::warn!(error = %error, "custody claims not finished"),
    }
    match apply_retention(pool, custody, tenant).await {
        Ok(count) if count > 0 => tracing::info!(count, "custody retention applied"),
        Ok(_) => {}
        Err(error) => tracing::warn!(error = %error, "custody retention not applied"),
    }
    match reverify(pool, custody, tenant, Duration::from_secs(30 * 24 * 3600)).await {
        Ok((verified, missing)) if verified + missing > 0 => {
            tracing::info!(verified, missing, "custody copies re-read");
        }
        Ok(_) => {}
        Err(error) => tracing::warn!(error = %error, "custody copies not re-read"),
    }
}

#[cfg(test)]
mod tests {
    /// Migration 079 registers the receipt schema for tenants already appending receipts; its embedded copy must be
    /// the reviewed schema file.
    #[test]
    fn the_migration_embeds_the_receipt_schema_file() {
        let migration = include_str!("../../migrations/postgres/079_evidence_custody.sql");
        let start = migration
            .find("-- sandbox.receipt.v1:start")
            .expect("start marker");
        let end = migration
            .find("-- sandbox.receipt.v1:end")
            .expect("end marker");
        let embedded = migration[start..end]
            .split_once("$receipt$")
            .and_then(|(_, rest)| rest.rsplit_once("$receipt$"))
            .map(|(json, _)| json)
            .expect("the embedded schema");
        let embedded: serde_json::Value = serde_json::from_str(embedded).unwrap();
        let file: serde_json::Value = serde_json::from_str(include_str!(
            "../../schemas/evidence/sandbox.receipt.v1.json"
        ))
        .unwrap();
        assert_eq!(embedded, file);
    }
}
