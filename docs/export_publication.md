# Trusted terminal export publication

The publisher connects the existing terminal-record validator, scoped TLS issuer, private client staging and fixed
export renderer. It is site-custodian code. It is not a researcher API, model tool or agent plugin, and it does not
grant the broker worker permission to create Kubernetes Secrets or mount tenant volumes.

`PreparedExportPublication::prepare` accepts operator-owned configuration, the claimed task and lab binding, its
exact Sandbox handle, observed process completion, original terminal-record bytes and an opaque issued identity.
It reuses the retention validators for principal, task, attempt, lease, worker, Pod, PVC, runtime, exit, timestamp and
selected file metadata. It checks the issuer's scope, service DNS and remaining lifetime. Secret values, including
the original manifest, must total at most 1 MiB. Keys and request bodies have no public serialization path.

`publish` uses `ObservedLeaseGuard` to renew the existing broker lease, check cancellation and re-observe the exact
terminal Sandbox and Pod before each create. It privately stages the broker client bundle and publishes, in order:

1. The run's NetworkPolicy, which grants only broker-worker ingress and no egress.
2. The immutable per-run Secret containing the fixed exporter configuration, terminal selection and server identity.
3. The ClusterIP service.
4. The restricted exporter Pod, which reads only the run's output PVC and private identity bundle.

`ExportKubernetesClient` binds one HTTPS API origin and namespace to an operator-configured projected service account.
It trusts only the configured CA, disables redirects and proxies, bounds calls and responses, and returns only object
UIDs and safe request metadata. Server error bodies and Secret values are never copied into errors or receipts.
There is no update, patch, delete, arbitrary resource, arbitrary endpoint or conflict-adoption operation.

`PublicationJournal` is the shared private durability boundary for terminal export and input admission publication.
It reserves a server-selected safe scope filename using an exclusive create through held no-follow directory descriptors.
Every typed create intent is fsynced before its request, followed by the returned UID and digest.
The journal contains ordered timestamps, task scope, manifest digest, certificate fingerprint and object receipts.
It contains no certificates, keys, prompts, research rows or original output data. Existing journals require recovery,
including an empty journal left by an interrupted first write. Client credentials and partial objects remain available
for inspection and UID-fenced cleanup. A lost lease, cancellation, uncertain request or conflict stops further creates;
it never replaces an identity, adopts an existing object or silently starts the next attempt.

A publication receipt is not task completion or artifact retention. The broker still downloads and verifies selected
original bytes through `HttpsArtifactRetainer`, registers them under the task lease, and applies its existing cleanup
order. The site dispatcher must durably correlate these receipts with that task and enforce its quota and cleanup
policy. Unresolved runtime/storage custody must block an expired attempt's readmission.

The focused connected check uses a real local TLS API endpoint, issued run credentials, actual private-file staging,
observed terminal validation and the existing lease interface. It verifies publication order, durable intent/UID
receipts, no silent replay, partial-create and lost-lease stops, Secret size rejection and pre-publication cancellation.
It does not prove Kubernetes scheduling, CSI permissions, network enforcement or live site RBAC.

This source does not activate a controller or dispatcher. Live use still requires the site-owned publisher service
account, admission policy preventing other PVC writers, exact workload/resource budget, controller installation,
input staging and sealed admission, quota-aware recovery and one authenticated original-byte task/download canary.
The publisher's receipts belong to the existing task/custody evidence; they are not a second task ledger.
