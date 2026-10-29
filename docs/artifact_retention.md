# Retaining a run's original outputs

The direct worker retains approved output bytes in the broker's durable artifact
store before registering success or deleting compute. The custodian selects files
and their visibility. The model cannot authorize a path, mount, identity or endpoint.

Two adapters implement this boundary. `FilesystemArtifactRetainer` copies from a
locally mounted run volume. `HttpsArtifactRetainer` pulls from a temporary trusted
exporter beside a run volume in another namespace or storage location. The HTTPS
adapter uses `StreamArtifactRetainer` to publish verified bytes into the same broker
store. Adding the adapter does not change researcher download authorization.

## Identity and terminal observation

`StagedTerminalRetainerFactory` runs after the direct driver observes the exact
Sandbox and Pod terminate. Its terminal record binds the task, principal, group,
profile, attempt, lease generation, worker, Sandbox name/UID/generation, Pod
name/UID/runtime, artifact PVC, exit code and timestamp. The same closed record
schema and comparison apply to both adapters. See
[`terminal_export.json`](../examples/terminal_export.json).

The worker supplies its own destination and user/lab export limits. The terminal
record carries selected relative paths, original byte counts, SHA-256 digests,
roles, labels, media types and `researcher` or `operator` visibility. It has a
16 MiB bound. Admission and dispatch catalog records retain their 256 KiB bound.
Unreviewed reasoning, unrestricted logs and operational payloads must not be
selected as researcher artifacts.

## HTTPS transfer across namespaces

Set the private dispatch catalog's `terminalExport` to the object in
[`remote_terminal_export.json`](../examples/remote_terminal_export.json).
`dnsSuffix`, port, private credential root and writer UID come from operator
configuration. The client derives the Service name from namespace, task, attempt
and lease generation. The complete address is
`https://<service>.<namespace>.<dnsSuffix>:<port>/`. No URL or credential path is
accepted from the researcher, model or terminal record.

The trusted dispatcher stages a private directory named
`<task-id>-<attempt>-<lease-generation>` beneath the configured credential root.
It contains `ca.pem` and `client.pem`, the latter containing that run's client
certificate and private key. Only configured trust roots are accepted. The client
checks the server's DNS identity, disables proxies and redirects, and never sends
client credentials through a public tunnel. Use dedicated short-lived identities
for each run. Do not grant the exporter a CA signing key.

`agent-sandbox-exporter --config /identity/config.json --writer-uid 65532` serves
one frozen terminal record and its selected original files. Its configuration
follows [`export_server.json`](../examples/export_server.json). A custodian stages
`server.pem`, `server.key`, `ca.pem`, the configuration and terminal record as
private regular files. Kubernetes projected Secret symlinks must first be copied
by a trusted init container into a private memory volume. The main exporter gets
only that volume and the single run's output PVC mounted read-only.

The exporter requires both a CA-verified client certificate and the exact configured
client-leaf SHA-256 fingerprint. Its two read-only routes contain the task, attempt
and lease generation. File routes use a frozen selection index, never a requested
filesystem path, and require the original manifest digest. Other runs, certificates,
methods and changed selections are refused. There is no command execution, upload,
file listing or deletion route. The service allows at most four active connections
and has a configured lifetime. TLS handshakes, client connections and stalled
reads are bounded; cancellation interrupts active connections and transfers.

The broker polls for a not-yet-ready exporter under the direct driver's overall
deadline, cancellation and lease supervision. Invalid records fail closed.
Authentication errors never trigger a plaintext, filesystem or legacy fallback.
Metadata logs record client rejection classes and successfully retained file
indices and sizes. They exclude credentials, paths, research bytes and model text.
The durable broker task/artifact ledger remains the authoritative completion record.

## Terminal preparation notice

Sites can request just-in-time export preparation by adding `preparation` beside
`https` in the existing private `terminalExport` configuration:

```json
{
  "https": {
    "dnsSuffix": "svc.cluster.local",
    "port": 8443,
    "credentialRoot": "/var/lib/broker/export_credentials",
    "writerUid": 65532
  },
  "preparation": {
    "root": "/var/lib/broker/export_requests",
    "writerUid": 65532,
    "maxWaitSeconds": 120
  }
}
```

The notice is published only after the worker has observed the exact terminal
Sandbox and Pod. It binds task, principal, group, profile, attempt, lease, worker,
Sandbox UID/generation, Pod UID, exit status and observed finish time. It contains
no objective, prompts, selected file paths, data bytes, executable or credentials.
The private filename is `<task-id>-<attempt>-<lease-generation>.json`. Publication
is atomic and exclusive; identical requests are idempotent, changed records fail.

The custodian must verify its current database lease and independently re-observe
that runtime. A parsed notice cannot mint a `RuntimeCompletion` or authorize data
access. The data authority supplies the approved file selection separately.

While the driver keeps the lease, the worker waits only for the exact per-run
credential directory to appear beneath a verified private root. A missing or
unsafe root, symlink, wrong owner, malformed bundle or rejected certificate fails
closed. Once the directory exists, the existing HTTPS client validates it and
waits for the exporter. The whole preparation has `maxWaitSeconds` (1 through
3600), the driver's deadline, and cancellation. Preparation, credential and durable
artifact roots must not overlap. Existing HTTPS configurations still require
staged credentials; mounted-volume configurations keep their existing behavior.

This source creates the notification and waiting path. It does not supply a
subscriber, grant custody authority, install an output inventory Job, or claim
that files have been retained. The existing driver records actual retention and
completion after those operations succeed.

## Durable byte contract

Both adapters walk all local path components through held no-follow directory
descriptors. Only declared regular files are read. Symbolic links, hard links,
special files, traversal, duplicates and limit violations are rejected. The
destination is private and owned by the service UID. Fixed-size buffers provide
bounded streaming with backpressure; each received byte contributes to size and
SHA-256 verification.

Files are synchronized and published atomically without replacing an existing
file. A retry reuses only a byte-verified matching destination. Errors, dropped
transfers and cancellation remove partial files. Complete files remain available
for recovery if later registration fails. The layout is
`tasks/<task-id>/attempts/<attempt>-<lease-generation>/exports/<original-path>`.
Broker downloads authorize the artifact and verify its stored digest again.

The driver fences task custody before retention, renews its lease during transfer,
registers the retained artifacts under that lease and fences again before deleting
the exact Sandbox UID. Losing custody cancels the transfer. Cancelling a research
task does not erase its output-retention obligation once compute has stopped.
No adapter deletes a PVC.

## Existing mounted-volume configuration

The original dispatch object with `recordRoot`, `writerUid` and
`mountedArtifactsRoot` remains supported. The custodian atomically publishes
`<task-id>-<attempt>-<lease-generation>.json` below the private record root and
mounts the run output at `<mounted-artifact-root>/<admitted-run-id>`. Record,
source and destination roots must not overlap. Missing roots, unsafe permissions
and mismatched identity are rejected. Late records are awaited under the driver
supervision described above.

## Site integration boundary

The exporter is a trusted data-plane component. Its identity must not be exposed
to the untrusted runner. The site dispatcher still owns PVC creation, exact binding,
immutable Secret publication, terminal/quiescence verification, scheduling, retry
custody and eventual resource cleanup. A Service plus network policy must restrict
exporter ingress to the broker worker and deny other egress; broker egress needs
its own explicit site policy. The broker's durable PVC remains in its own namespace.

Neither adapter proves storage topology, volume detachment, node-failure recovery,
file classification or container-log capture. Local TLS and original-byte checks
prove the transport boundary. A real site task with its controller, storage,
policy and authenticated download path is still required before activation.
