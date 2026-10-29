# Direct Sandbox input custody

`agent-sandbox-custodian` is a fixed, local checksummed-copy helper for the direct Sandbox `run/v3` and `input-staging/v1` boundary. It is not a Kubernetes controller, broker, worker, exporter, network client, or general-purpose script runner.

A trusted server-side custodian supplies one typed JSON request and three already-authorized filesystem roots: a read-only source tree, an empty input-PVC mount, and private custody state. Each root must already exist, be absolute and canonical with no symlinked component, and be distinct from the other two. The helper holds those approved roots for the operation and opens declared files with no-follow semantics. The request names the tenant, run, principal hash, input reference and digest, PVC name, opaque manifest and attestation references, limits, and every expected relative file with size and SHA-256. It carries no command, shell, URL, SQL, credential, or data value.

```text
agent-sandbox-custodian --request /trusted/request.json
```

The helper rejects symlinks, special files, absolute or traversal paths, missing or extra files, duplicate declarations, oversized trees, changed source bytes, invalid identifiers, and incomplete prior staging. Every hash and copy stream enforces both the declared file size and the configured aggregate byte bound until EOF. It inventories the source in sorted order, verifies each declared hash, copies to a private staging path, re-verifies copied bytes, inventories the source again, then atomically publishes `payload` inside the input mount. It writes and syncs a private deterministic manifest and typed seal attestation only after the copied payload passes the same verification.

A retry can reuse only an exact existing attestation, its retained manifest with the recorded canonical digest, and the verified payload. A previous staging directory or payload without an attestation is rejected. A crash, signal, or failed copy therefore leaves no seal attestation and must be reconciled by the trusted custodian before a replacement PVC is prepared. The helper does not remove a partial input tree.

The attestation's `attestationDigest` hashes the same attestation with that self field blank. The run/v3 `inputBundle.stagingDigest` is different: it is the canonical document digest of the complete emitted attestation, with object keys sorted recursively, matching `SandboxContracts.document_digest` in the canonical renderer. The helper exposes `canonical_document_digest` so a trusted admission path can calculate that binding without inventing another serialization.

The external trusted custodian must next detach the copier and seal the PVC metadata with the returned manifest and attestation digests. Only then may the direct-Sandbox renderer and broker observe the PVC. The broker and worker receive no PVC write permission. The helper proves local byte copying and attestation generation; Kubernetes admission, same-namespace PVC creation, input-PVC metadata sealing, compute creation, artifact retention, and export remain separate authorities.

## Admitted files

A reviewed template also needs its one model-written block staged beside the source. `files` still declares every
file with size and SHA-256; `admittedFiles` supplies the exact UTF-8 bytes for declared paths that must not exist in
the source tree (at most four, 64 KiB each). Only the trusted input custodian adds them, from the broker's admitted
record for the current lease; a plan that carries bytes is refused. The helper checks each admitted file against its
declaration before copying, writes it exclusively into the staged payload, and seals one manifest that covers the
source and admitted files together, so the run's input digest binds the code. Without admitted files the request,
manifest and digest are unchanged.

The CSI snapshot materialization route remains a separate candidate. This helper intentionally does not create or depend on Snapshot resources.
