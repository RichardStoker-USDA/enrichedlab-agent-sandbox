# Evidence object custody

Records name bytes (model prompts and completions, artifacts, figures) by digest; the bytes live in object storage.
Custody is how those bytes enter storage so that every record which names them can be trusted to point at exactly
those bytes, held for as long as the record needs them. Migration 079 holds the facts; the evidence append service
(`docs/evidence_append.md`) is the only writer.

## Who does what

- **Producers** stream bytes to `StoreObject` on the append service. They never hold object-store credentials, never
  pick a store, a retention or an owner, and never read objects. A producer can be compromised: the worst it can do
  is store bytes where its grant already lets it append.
- **The append service** holds one writer identity per lab on the primary store and one reader identity per lab on
  the replica. It acts only after the database authorises the producer for the exact scope.
- **The database** decides authority, records every custody fact in immutable rows, and refuses a record that names
  bytes without a ready claim of the same producer and scope.
- **Storage administrators** (the stores' root identities) are inside the confidentiality boundary. Integrity never
  depends on the store: every read is checked against the recorded digests.

## Storing bytes

`StoreObject` (client streaming) takes a header (tenant, lab, scope, media type, the raw SHA-256 and size, an
idempotency key) and then chunks of at most 64 KiB, 64 MiB in all. The service:

1. checks the producer's grant (`custody.store` in that scope, `evidence.custody_authorise`) and gets the lab's two
   stores from `evidence.custody_stores`; a lab with no registered pair cannot store;
2. hashes while receiving and refuses a digest or size that differs from the header (`CUSTODY_DIGEST_MISMATCH`,
   `CUSTODY_SIZE`);
3. compresses with zstd and writes `sha256/<2 hex>/<sha256>.zst` with `If-None-Match: *`; an existing key is read
   back and verified, never trusted;
4. reads the version back in full, decompresses, hashes, and extends its retention to a day past the class floor
   when the bucket default is shorter;
5. issues a claim (`evidence.custody_issue_claim`), then waits up to `replica_wait_seconds` for replication and does the
   same full read-back and extension on the replica copy before appending `claim_ready`.

A claim that is not ready yet is finished by `AwaitClaim` or by the reconciler. Only the producer that stored it can
see or bind it.

## Binding

A lineage entity or model call from an untrusted producer that carries any byte field (`sha256`, `size`, `media_type`,
`uri`, `object_version`) must name a ready claim of that producer and scope; the record fills digest, size, URI and
version from the claim's copies and refuses them from the producer. Binding happens inside `evidence.append`, under a
lock on the object and the project's acceptance state, and re-derives the retention floor at that moment: both pinned
copies must be current (no `copy_missing` or `copy_deleted` names that physical version) and retained to the floor.

## Retention

The floor is 90 days for run and lab objects. A project accepted under an approved records class takes that class's
period: acceptance appends `retention_requested` for every reference in the project, the service extends and reads
back both copies of each, and the acceptance is complete only when every copy is covered
(`evidence.custody_acceptance_state`). Governance retention only extends; no service identity can shorten it, delete
a version, place a hold or bypass governance.

## Closed event schemas

Every event type an untrusted producer appends has a registered schema (`evidence.register_event_schema`, the files in
`schemas/evidence/`): objects closed at every level, strings bounded, no free text. A payload naming a member twice
is refused for every producer. An untrusted producer's lineage carries tokens only (identifiers, kinds, digests,
times). An identical retry is answered with its original receipt even after a newer schema.

## Reconciler

Every five minutes the service finishes waiting claims, applies requested retention (a failed copy moves behind the
others), completes acceptances, re-reads copies not verified for 30 days (recording `copy_missing` for one that is
gone or holds other bytes), and records a database epoch change. It never deletes. Garbage collection and reads are
later releases.

## Configuration

`--custody-file` names a JSON file:

```json
{
  "ca_file": "/run/tls/ca.crt",
  "endpoints": {
    "primary-ev-0123456789abcdef0123": {"url": "https://objects.example:9000", "region": "site",
      "access_key_file": "/run/stores/primary/access_key", "secret_key_file": "/run/stores/primary/secret_key"},
    "replica-ev-0123456789abcdef0123": {"url": "https://replica.example:9000", "region": "site",
      "access_key_file": "/run/stores/replica/access_key", "secret_key_file": "/run/stores/replica/secret_key"}
  },
  "replica_wait_seconds": 60
}
```

Each lab's bucket name is derived by the record (`evidence.custody_bucket_name`: `ev-` and 20 hex characters of
SHA-256 over tenant and lab), and `evidence.custody_register_store` binds the lab's primary and replica to endpoint
names in this file. The writer identity needs `s3:PutObject`, `s3:GetObject`, `s3:GetObjectVersion`,
`s3:GetObjectRetention`, `s3:PutObjectRetention` and `s3:GetObjectLegalHold` on the bucket's objects and
`s3:ListBucket`, `s3:GetBucketVersioning` and `s3:GetBucketObjectLockConfiguration` on the bucket; the replica reader
the same without `s3:PutObject`. Both buckets are versioned, with object lock and a governance default of at least 90
days, and the primary replicates to the replica with existing objects and delete markers but not deletes.
`--custody-in-flight` (default 4) bounds concurrent custody calls; each may hold an object, its stored form and both
read-backs in memory.

## Canary

`agent-sandbox-broker evidence-custody-canary --address <url> --server-name <name> --ca-file <ca> --token-file <token>
--tenant <tenant> --group <lab>` stores random bytes in the lab's standalone scope, waits for the claim, and appends
one `custody.canary` record binding it. It prints ids, digests and the class.

## Qualification

`scripts/qualify_remote_broker.sh evidence` runs the custody fixture on PostgreSQL 18 (authority, claims, binding,
acceptance, retention queues, closed schemas, an upgrade from layout 5 with a live-shape receipt, and migration reruns).
`evidence-service` starts two local RustFS stores with TLS, object lock and replication
(`scripts/custody_stores_local.sh`) and runs StoreObject, AwaitClaim, binding and the canary through the real service;
`tests/evidence_object_stores.rs` checks the S3 adapter against the same stores.
