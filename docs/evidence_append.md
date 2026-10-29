# Evidence append service

`agent-sandbox-broker evidence-append` serves `enrichedlab.evidence.v1.EvidenceAppendService` (the contracts
repository's `append_service.proto`) over gRPC with TLS, beside the standard gRPC health service. It is how producers
outside the broker (a sandbox runtime's receipt service, the platform, the knowledge pipeline, operations agents) add
records to the research record (migrations 075 and 076). With custody configured it also stores the bytes those
records name (`StoreObject`, `AwaitClaim`; migration 079, `docs/evidence_custody.md`).

## Who a caller is

Each call carries `authorization: Bearer <token>`: a bound Kubernetes ServiceAccount token for the service's audience,
from a projected volume. The service sends it to the API server's TokenReview and accepts it only when it is
authenticated for that audience. The private producers file maps each ServiceAccount to exactly one evidence producer;
a request never names its producer. The service appends as its own database role, and the record accepts a producer
only when `evidence.adapter_bindings` binds it to that role, so a misconfigured mapping cannot write as a producer the
database does not also bind.

Producers file (mode 600 in a private folder, beside the API server's CA bundle):

```json
{
  "schemaVersion": "enrichedlab.broker.evidence-producers/v1",
  "apiServer": "https://kubernetes.default.svc/",
  "reviewerTokenRoot": "/var/run/evidence-reviewer",
  "reviewerTokenFile": "token",
  "caFile": "/run/evidence-producers/ca.crt",
  "audience": "enrichedlab-evidence",
  "producers": [
    {"namespace": "openshell", "serviceAccount": "run-receipts", "producer": "k8s:openshell/run-receipts"}
  ]
}
```

The reviewer token comes from the service's own projected ServiceAccount token (its root must be the kubelet's
root-owned projected volume) and needs permission to create TokenReviews.

## Answers

A receipt names the record, its chain and sequence, its hash, the ingestion time, and whether an identical earlier
request was answered again. Refusals carry the record's reason in `google.rpc.ErrorInfo` (domain
`evidence.enrichedlab.org`): `PERMISSION_DENIED` for authority (`GRANT_MISSING`, `PRODUCER_NOT_CALLER`,
`EVENT_TYPE_RESERVED`, `TENANT_NOT_SERVED`), `INVALID_ARGUMENT` for malformed requests (including
`SOURCE_TIME_PRECISION`: the record keeps microseconds, so a finer source time is refused rather than rounded into
a false retry), `ALREADY_EXISTS` for an idempotency conflict, `FAILED_PRECONDITION` for a disabled tenant,
`UNAUTHENTICATED` for a missing or unaccepted token, `RESOURCE_EXHAUSTED` when the service is at its in-flight limit,
`UNAVAILABLE` when the record or the TokenReview cannot be reached. Custody calls add `CUSTODY_NOT_CONFIGURED` and
`CUSTODY_NOT_READY` (`FAILED_PRECONDITION`: no custody file, or a schema before layout 6), `CUSTODY_DIGEST_MISMATCH`
and `CUSTODY_SIZE`, `CUSTODY_STORE_MISSING` for a lab with no stores, and `CUSTODY_CLAIM_UNKNOWN` for a claim of
another producer. A payload that does not match its event type's registered schema is refused with
`PAYLOAD_NOT_METADATA`. Database messages are never returned. A producer
that lost an answer, or was refused with one of the last two codes, retries with the same idempotency key and receives
the original receipt.

## Limits and health

At most `--max-connections` (256) connections are open; one beyond that is closed on arrival, and a TLS handshake
must finish within 5 s, so a silent client holds its slot no longer. At most `--max-in-flight` (64) Append calls run
across all connections, which also bounds concurrent TokenReviews; one beyond it is refused at once
(`RESOURCE_EXHAUSTED`, `SERVICE_AT_LIMIT`). Every other call (health checks, unknown paths) has its own budget of 16, taken before any request body is read, and
a health request is capped at 1 KiB, so appends never fail a probe and probes never crowd out appends. A call
past 15 s, or past the caller's own `grpc-timeout` (answered a little before it), answers `DEADLINE_EXCEEDED`
(`CALL_TIMED_OUT`). Each connection allows 32 concurrent streams, connections
are recycled after 30 minutes, and HTTP/2 and TCP keepalive close dead peers. Requests are capped at 80 KiB (64 KiB
payload).

Custody calls have their own budget (`--custody-in-flight`, 4) and a 120 s limit. The service starts only when its
database role can execute `evidence.append`, the schema carries a layout it was built for (5, or 6 with custody), the tenant is enabled, and every producer in its configuration is bound to its role
(`evidence.adapter_producers`, migration 077). It also creates any missing monthly partition up to three months
ahead, at startup and daily (migrations create six), so appends never meet a month without one between releases.
Health reports `NOT_SERVING` until its first full check passes. After that, it repeats the startup authority checks, this month's records partition and a TokenReview every 15
seconds and reports `NOT_SERVING` while any fails. Health serves Check only; Watch is refused, so no unauthenticated
stream outlives its call.

## Telemetry

With `OTEL_EXPORTER_OTLP_ENDPOINT` set, the service exports over OTLP/gRPC: one server span per call (W3C trace context
from the caller is honored) and the metrics `rpc.server.requests` and `rpc.server.duration` (milliseconds) by service,
method and gRPC status code, `enrichedlab.evidence.connections` by outcome, and `enrichedlab.evidence.token_reviews` by
outcome. The telemetry layer sits outside the limits, so calls refused at the limit or past their deadline are counted
with their codes; a call dropped before it answers is counted as `CANCELLED`. Unknown paths are reported as `other`. No attribute carries request content, tokens or producer identities.

## Running it

```
agent-sandbox-broker evidence-append --listen 0.0.0.0:8443 --tls-cert /tls/tls.crt --tls-key /tls/tls.key \
  --producers-file /run/evidence-producers/producers.json --database-dsn-file /run/evidence-db/dsn --tenant <tenant> \
  [--custody-file /run/evidence-custody/custody.json --custody-in-flight 2]
```

The database role is granted by `grant_broker_runtime.psql` with `-v evidence_adapter_role=<role>` (execute on
`evidence.append`, `evidence.adapter_producers` and `evidence.ensure_record_partitions`, and from migration 079 the
custody functions the service calls, nothing else). The service starts
only when that role can execute `evidence.append` and the schema carries the layout it was built for. Probe it from
inside its Pod with `agent-sandbox-broker evidence-probe --server-name <name> --ca-file <ca>`; kubelet network probes
cannot present the internal CA's TLS.

`scripts/qualify_remote_broker.sh evidence-service` runs the service over TLS against a qualification database:
startup refusals for an unbound producer and an unknown tenant, appends, retries, conflicts, authentication refusals,
grant, reserved-type and tenant refusals, digest mismatches, source-time precision, a lineage record, refusal at the in-flight limit, the call deadline, health turning `NOT_SERVING` and back while the
tenant is disabled, the producer is disabled, the append privilege is revoked or TokenReview is down, Watch refused,
silent clients released at the handshake deadline, and every status code reaching an in-memory metric exporter. With
two local object stores it also runs custody end to end: stored and ready, deduplicated, refused digests and sizes,
AwaitClaim, binding, a run-class claim, and the canary.
