# Terminal output custody

`agent-sandbox-output-custodian` connects a worker's terminal notice to an
approved file selection, a fixed inventory Job, and the existing authenticated
export publisher. It is a trusted site service. A sandbox, model or researcher
cannot supply its configuration or executable.

The worker uses `terminalExport.preparation` and keeps renewing its task lease
while awaiting the per-run client directory. A site dispatcher invokes:

```sh
agent-sandbox-output-custodian --config /run/operator/output.json \
  --config-owner-uid 65532 --request <task-attempt-generation>.json
```

The request name selects one file inside each configured private root. It is
never a path or shell command. Both the notice and independently approved plan
must identify the same current running task, principal, group, profile, attempt
and lease generation. The actual database lookup supplies task authority. The
custodian observes the exact Sandbox and Pod again before inventory and export.

## Site configuration

The closed service document uses
`schemaVersion: sandbox.enrichedlab.org/output-custodian/v1`,
`databaseTenant`, `databaseDsnFile`, `kubernetes`, `bindings` and `custodian`.
The database credential stays in a private file. `kubernetes` uses an HTTPS API
origin, one namespace, and projected token and CA file references. All declared
bindings belong to that namespace. The runtime role needs only its assigned
terminal lookup and lease functions; the migration-owner credential is not a
runtime credential.

`custodian` declares:

- `requests` and `plans`: separate private roots and their `writerUid`.
- `custodyRoot` and `journalRoot`: original records and exclusive run journals.
- `inventory`: the fixed output inventory workload, pinned image, resource
  limits, selected node pool, private journal and writer UID. `requestMemoryMib`
  bounds the helper's private memory volume for its materialized request.
- `publication`: the existing exporter workload, private client credential
  root and separate publication journal.
- `leaseSeconds`, `maxWaitSeconds` and `pollIntervalMillis`: bounded authority
  renewal and total operation duration.
- `exportSeconds`: the bounded export credential allowance, greater than the
  exporter maximum duration plus startup margin. The issuer also includes the
  task's configured duration and enforces its existing one-day ceiling.

All seven private roots must be distinct and non-overlapping. Site policy must
prevent other workloads from writing the run's output PVC. The helper also
checks the exact PVC UID and observed writers. A namespace grant alone cannot
enforce that workload policy. Inventory and exporter images are fixed and
digest-pinned; their workloads do not receive a Kubernetes token.

This synthetic plan shows the closed shape. The site dispatcher fills its
identities from the approved operation and observed PVC:

```json
{
  "schemaVersion": "sandbox.enrichedlab.org/output-preparation-plan/v1",
  "task": {
    "taskId": "00000000-0000-0000-0000-000000000000",
    "principalId": "00000000-0000-0000-0000-000000000000",
    "groupKey": "research", "profileKey": "reference",
    "attempt": 2, "leaseGeneration": 7, "maxRunSeconds": 300
  },
  "binding": {
    "principalId": "00000000-0000-0000-0000-000000000000",
    "principalHash": "2f4b66b9706c1ef57d67e2cf9ad98127",
    "groupKey": "research", "profileKey": "reference",
    "profileId": "example-kata", "labRef": "lab://example/research",
    "namespace": "sandbox-example-001", "runtimeClassName": "kata-clh"
  },
  "runId": "run-20260909-0001",
  "outputPvcUid": "output-example-uid",
  "selection": {
    "schemaVersion": "sandbox.enrichedlab.org/output-inventory-request/v1",
    "mountedRoot": "/artifacts",
    "files": [{
      "relativePath": "result.txt", "visibility": "researcher",
      "role": "result", "label": "Tool output", "mediaType": "text/plain"
    }],
    "maxFiles": 20, "maxBytes": 10000
  }
}
```

`selection.mountedRoot` is always `/artifacts`.
Selected relative paths, visibility, role, label and media type
come from the approved tool or template contract. The agent does not choose
which private files to expose. Limits cannot exceed the exporter policy.

## Retained evidence and failure behavior

The custodian reserves an exclusive scope before any helper or identity side
effect. It retains the original plan, original measured inventory, generated
terminal record and publication receipt under that scope. Each successful
effect is linked by task, attempt, lease, Sandbox, Pod and helper UIDs. File
sizes and hashes come from reading the selected original files, with no
scientific values reconstructed in model text.

The existing exporter authenticates the worker using a short-lived per-run
identity. The worker verifies the terminal record, streams selected files,
checks size and SHA-256 and registers the retained originals. Scientists see
their authorized artifacts through the broker; they never receive Kubernetes
or exporter credentials.

Cancellation, a lost lease, a changed runtime, an unexpected writer, a changed
PVC, a failed helper or a deadline stops publication. Partial resources and
private journals remain for explicit custody recovery. The custodian does not
delete scientific data, adopt another attempt's resources or automatically
issue replacement credentials. Reinvoking a completed or partial scope is
also refused. Cancellation may still leave artifacts that require recovery;
it does not authorize discarding them.

This command does not install its dispatcher, database grants, namespace
policy, storage allocator or controller. Those are site deployment inputs.
Its source tests do not establish multi-user cluster capacity or acceptance
of a scientist-facing interface.

The inventory and publication workload configurations accept the same optional `site`
labels and image-pull Secret references as input-copy workloads. Inventory Job and Pod
labels match; exporter Pod, Service and NetworkPolicy selectors match. The trusted site
configuration owns these values. A task cannot supply or override them.
