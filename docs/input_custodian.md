# Trusted input custodian

`agent-sandbox-input-custodian` connects a worker's private preparation notice to
the fixed copy workload, original seal observation, canonical site renderer, and
atomic admission publication. It handles one request and exits. The worker keeps
waiting on its existing task lease; the custodian never starts compute or marks a
scientific task successful.

The database lookup in [custodian_task_lookup.md](custodian_task_lookup.md) supplies
current task authority. The notice identifies the task and worker but grants no
data access. An operator-approved private plan must match the exact principal,
group, profile, attempt, lease generation, and sandbox binding. The plan is read
by descriptor from its configured private root, with no symlink or hard-link
adoption. Copy requests and real data move through deterministic code, never
through the model's output tokens.

## Invocation and configuration

```sh
agent-sandbox-input-custodian \
  --config /run/custodian/config/service.json \
  --config-owner-uid 65532 \
  --request '<task-uuid>-<attempt>-<lease-generation>.json'
```

The service JSON is closed to unknown fields. It contains:

| Field | Purpose |
|---|---|
| `schemaVersion` | `sandbox.enrichedlab.org/input-custodian/v1` |
| `databaseTenant` | Site broker tenant; separate from the sandbox tenant in the approved run |
| `databaseDsnFile` | Absolute runtime secret reference, never a DSN value |
| `kubernetes` | `origin`, `namespace`, `tokenFile`, and `caFile`; exact HTTPS namespace-bound client |
| `custodian.requests`, `custodian.plans` | Each has private absolute `root` and `writerUid` |
| `custodian.publication` | `workload`, `journalRoot`, `custodyRoot`, `writerUid`; see [input_publication.md](input_publication.md) |
| `custodian.admission` | `admissionRoot`, `custodyRoot`, `writerUid`, `leaseSeconds` |
| `custodian.finalizationJournalRoot` | Private finalization journal directory |
| `custodian.maxWaitSeconds` | Copy and cleanup wait bound, 1 through 3600 seconds each |
| `custodian.pollIntervalMillis` | Observation interval, 1 through 5000 milliseconds |
| `custodian.inputSources` | Optional list of `pvcUid`, `class` and `source` for approved source volumes; see below |
| `renderer` | Fixed `program`, entrypoint `sha256` in lowercase hex, `ownerUid`, optional `arguments`, `timeoutSeconds` from 1 through 15 |

All seven custody/request/plan/journal directories must be distinct, non-overlapping
private directories. Runtime-created files use mode 0600. The publication and
admission writer UIDs must match the process's effective UID. Lease duration is
30 through 3600 seconds. A practical starting point is a 120-second lease and a
1000-millisecond poll interval. Limits are configuration, not throughput promises.

The publication workload uses the closed camelCase fields of
`InputWorkloadConfig`: runtime, pinned helper image, pool selector, CPU/memory
requests and limits, deadline, memory custody bound, input/artifact storage
classes and sizes, closed `custodyPvcAccessMode`, and request size bound. An
omitted access mode stays `readWriteOnce`; a site-qualified custody StorageClass
may select `readWriteOncePod` for both newly created custody PVCs. This does not
change shared notice/control storage or provide source/output writer exclusion.
The existing renderer validates these fields before any Kubernetes create.

## Approved plan and renderer

A plan has the same filename as the notice. Its closed JSON fields are
`schemaVersion: sandbox.enrichedlab.org/input-preparation-plan/v2`, `task`,
`binding`, `source`, `copyRequestSecretName`, `copy`, `custodyRetentionUntil`, and `rendererPlan`.
`task` and `binding` exactly match the notice and database task. `source` includes
namespace, PVC name and UID, source run, tenant and principal hash. `copy` is the
existing `CopySealRequest`, including authoritative manifest entries and bounds.
The site data authority supplies this plan. A notice, task objective, or model
response cannot supply it.

For a reviewed template, the worker's notice adds a digest-only `admittedCode` reference (template, path, size and
SHA-256) read from the broker for its lease, and the plan repeats it as `admittedCode`. The custodian requires the two
to match, reads the bytes with `broker_custody_admitted_code` for the same current lease, checks them against the
reference and the declared manifest entry, and attaches them to the copy request as `admittedFiles`. The notice and
plan never carry the bytes; the copy request that holds them lives only in the private request Secret and the
custodian's retained request journal.

`custodyRetentionUntil` is an approved RFC 3339 timestamp for both the input and
artifact PVCs. The site author derives it from the reviewed run's retention
policy, which must outlast its compute deadline. The custodian rejects a missing
or invalid timestamp, or one that does not leave the task's full runtime budget
at preparation time, before creating any objects. Rendering preserves the exact
timestamp and never derives expiry from the clock. Input plan v1 is rejected;
upgrade the plan author, publisher and input custodian together. Output plans
remain v1.

The fixed renderer receives bounded JSON on stdin:

```json
{
  "schemaVersion": "sandbox.enrichedlab.org/custodian-render-request/v1",
  "task": {},
  "binding": {},
  "sealedInput": {},
  "plan": {}
}
```

The empty objects above denote typed documents, not usable authorizations. The
task carries identity and lease metadata only. `sealedInput` comes from the
original accepted helper receipt. The renderer returns closed JSON with
`schemaVersion: sandbox.enrichedlab.org/custodian-render-result/v1`,
`authorization` (the existing `SandboxAuthorization`) and `sandbox` (canonical
renderer output). Input and output are each bounded to 1 MiB. Existing admission
validation binds that output to the actual task, seal and freshly observed PVC
UIDs before deleting the copy workload or promoting the seal.

The executable and arguments are operator configuration. The process clears its
inherited environment, uses a fixed system PATH, suppresses stderr, and bounds
execution time and output. The entrypoint checksum is checked before execution.
The whole renderer and dependency tree must also be pinned in an immutable,
read-only image; an entrypoint digest alone does not pin an interpreter or its
imports. A site adapter should call its existing reviewed renderer instead of
implementing another Kubernetes manifest generator.

## Input class record

Each attempt records what it read. `custodian.inputSources` gives an approved source volume, by its PVC
UID, a class (`object_data`, `paper`, `public_reference` or `prior_output`) and a flat `source` object of strings; an
`object_data` source must name `source_version` and `source_sha256`. `lab_database` is refused here: a database
extract records its own query digest, snapshot and WAL position where it is taken, which a static entry cannot know.
The UIDs are unique and `source_pvc_uid` and `source_run_id` are reserved.

When the plan's source volume, whose UID the source guard verifies before the copy, has an entry, the custodian
checks the guard again after the seal and calls `broker_record_attempt_input` for the notice's lease: the sealed
input reference, the class, the sealed manifest digest, the declared bytes, the staging identifier as the custody
transaction, and the entry's source with the verified volume UID and source run added. The class comes from this
configuration, never from the plan, the notice or a name. A refused record stops the scope before rendering, so no
admission is published; the journal records `input_recorded` otherwise. A source without an entry is sealed as
before and its attempt names `attempt_inputs_not_recorded`. The field is optional, so an older configuration loads
unchanged, but an older custodian refuses a configuration that has it; release the custodian first.

## Retention, interruption and deployment

Before create, retain the exact private plan and copy request/manifest. Before
rendering or cleanup, retain the original helper receipt. Keep the renderer input
and output privately, and journal operation phases with metadata and digests.
Finalization separately retains evidence, deletes by exact Job UID, waits for
cleanup, updates the input seal by UID/resource version, rereads both volumes,
and publishes the worker's admission record.

A failed, expired, cancelled, or partial scope keeps its journal and evidence for
recovery. Exclusive journals refuse blind reruns, including a request whose
admission was already published. There is no automatic recursive watcher or
second queue in this binary. A site dispatcher owns invoking an approved request
once and reconciling interrupted scopes before retry. Signal cancellation stops
forward progress; it does not delete evidence or manufacture a completed task.

Source writer observations are not locks. Site policy must exclude replacement
and new writers for the full copy. Deployment also needs migration 005, narrowly
granted custody/lease functions, approved private plans, fixed renderer packaging,
and the existing namespace-bound Kubernetes custody permissions. This source
interface does not install those grants or establish a live service. Ordinary
research workers must not acquire them.

The focused regression connects private notice and plan loading, authoritative
lookup, actual file copy/sealing, a pinned renderer process, exact finalization,
and loadable admission. It covers stale/cancelled authority, plan mismatch or
symlink substitution, failed copy, entrypoint changes and refused replay. The
separate PostgreSQL and Kubernetes transport checks cover their real protocol
boundaries; a fixture path is not evidence of a deployed cluster task.
