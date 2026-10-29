# Architecture

## Control flow

```text
person or host agent
        |
        | CLI or loopback dashboard
        v
agent-sandbox controller
        |
        | authenticated OpenShell CLI/gRPC
        v
OpenShell gateway and supervisor
        |
        | filesystem, process, network, inference policy
        v
isolated agent sandbox
```

The harness and model run in the lowest-trust box. They can write a structured request or use OpenShell Policy Advisor. The controller or OpenShell prover decides below them. This follows the authority placement described in NVIDIA's [agent security stack](https://developer.nvidia.com/blog/where-security-fits-in-an-ai-agent-stack/): the model proposes and a lower policy layer decides.

The controller does not replace OpenShell and does not add a second runtime. It records controller ownership, creates an exact driver configuration from a named gateway profile, generates policy, calls OpenShell, supervises temporary host listeners, and stores independent runtime proofs.

## Filesystem contract

Docker bind profiles get these mounts:

| Sandbox path | Host source | Mode |
|---|---|---|
| `/sandbox/work` | one declared workspace | read-write |
| `/sandbox/control/requests` | controller request inbox | read-write |
| `/sandbox/control/decisions` | controller decisions | read-only |
| `/sandbox/control/runtime` | controller-owned helper code | read-only |

Optional mounts appear below `/sandbox/mounts`. The controller canonicalizes each host path and rejects roots, home itself, runtime state, credentials, agent configuration, `.git`, and broad system directories. Docker receives only the resulting mount list.

VM and Kubernetes profiles do not send host paths to the runtime. The controller stages a checked tree, uploads it to `/sandbox/work`, and records a digest, file count, byte count, direction, snapshot, and backup path. Requests and decisions are copied between the host and sandbox. A sync copy is a point-in-time transfer, not a read-only host mount, so the evidence record states that difference.

Filesystem and process rules are static in OpenShell. A mount approval is accepted only for bind transport, captures evidence, and recreates the sandbox. Sync profiles reject host-mount requests. The controller gets the current effective policy first so approved Policy Advisor rules survive a bind-profile recreation.

## Runtime contract

```text
named gateway profile
        |
        | driver + transport + limits + optional Kata placement
        v
OpenShell driver config, SHA-256 digest in sandbox record
        |
        +--> gateway health proof
        +--> Docker identity proof, or
        +--> Kubernetes Sandbox/Pod/RuntimeClass/NetworkPolicy proof
```

The Kubernetes observer can run locally or through batch SSH. Its command tokens, namespace, context, kubeconfig path, and SSH host are checked. Output is bounded and parsed as JSON. It is read-only unless `manage_peer_network_policies` is set. That setting enables only lease-owned NetworkPolicy create, verify, and delete operations in the configured namespace.

## Network contract

Policy profiles start default-deny and name exact public endpoints. OpenShell may hot-reload a reviewed endpoint. Controller-added public domains are normalized DNS names on HTTPS port 443. IP literals, local names, internal names, wildcards, and broad address blocks are rejected.

Inference stays behind OpenShell's `inference.local` route. Public route catalogs describe model names and harness options but contain no credentials.

## Service paths

```text
sandbox app 127.0.0.1:8000
        |
        | openshell forward service
        v
host 127.0.0.1:38080
```

```text
approved LAN client
        |
        | TLS client certificate + source CIDR
        v
host assigned-IP:38443
        |
        | local proxy
        v
host 127.0.0.1:38080 -> sandbox app
```

```text
source sandbox and exact temporary policy
        |
        | direct source_endpoint or source-local 127.0.0.1 port
        v
source-local relay through OpenShell's explicit proxy
        |
        v
host 127.0.0.1 parent lease -> target sandbox app
```

```text
Kubernetes source Pod + exact OpenShell endpoint
        |
        | source-only egress NetworkPolicy + TLS capability
        v
target Pod TLS relay
        |
        | target loopback
        v
target sandbox app
```

The LAN and peer relays accept at most 64 live connections each. Lease TTLs are bounded to seven days, and child leases cannot outlive a parent. A parent close recursively closes its children. Failed host forwards close their dependent listeners. Closing a peer also stops its source-side process, removes the exact source policy endpoint, and closes its owned parent forward.

Host and peer supervisors run in their own process groups and persist the spawned OpenShell PID. Reconciliation verifies the child command and group before cleaning up a child whose supervisor disappeared. It refuses to signal an unrelated reused PID. Parent and peer cleanup then runs from the reconciled lease graph. Docker peers record the source container and network. VM peers record the OpenShell source runtime ID. Kubernetes peers record both OpenShell runtime IDs, both Pod UIDs and IPs, both child PIDs, NetworkPolicy names, and TLS relay custody.

## Teams

The source repository must be clean. The controller freezes `HEAD`, creates a host-only integration clone, removes its remote, and makes each worker clone with `--no-hardlinks`. Every worker receives one branch and non-overlapping literal path prefixes. Workers do not push or merge.

Collection requires a clean worker tree, a commit descended from the frozen base, and a diff whose added, changed, deleted, renamed, and type-changed paths all belong to the worker. Rename detection is disabled during validation so both the old deletion and new addition are checked. The host fetches the checked branch into the integration clone and performs the merge there.

## Research studies

```text
host question + protocol + input snapshot + tool digests
        |
        +--> worker A sandbox, route A, perspective A --> output A
        +--> worker B sandbox, route B, perspective B --> output B
        +--> worker N sandbox, bounded by route pool_cap
        |
        v
host seals --> normalized-answer groups --> external validation
```

The controller copies the same frozen control material into separate worker workspaces and gives each worker its own
perspective. It captures bounded runner output outside the worker tree. Collection rechecks the input, question,
protocol, perspective, assignment, selected tool copies, result schema, evidence paths, and study output limits before
writing a host-owned seal. Comparison records agreement and route diversity. The controller never converts worker vote
or confidence into scientific validation.

One worker may use several bounded segments. Each segment starts a new provider session, receives only the fixed
assignment, current scoped state, and current files, then exits after one unit. The host observes Pi or OpenCode JSONL,
versions output bytes, reconciles semantic proposals, verifies the segment record, and creates the next context epoch.
No provider thread or KV state is reused across that boundary.

## Agent state

```text
immutable task + scoped state view + current observation
                         |
                         v
                    agent step
                   /           \
       model semantic patch   host observation
                         v
       schema, authority, audience, version checks
          |                 |                 |
   canonical state    append-only events    artifact CAS
```

Canonical state is small, typed, and selected by profile. Events, full file bytes, telemetry, and restricted returned
reasoning stay outside the prompt. Machine facts come from the controller's harness stream and filesystem observer.
Model-authored facts and decisions remain marked as model authority. Context resets create numbered epochs tied to a
state-set digest and event head. Automatic parsing covers the pinned Pi and OpenCode runners. Custom harnesses use the
strict native or OpenAI-compatible ingest boundary.

The embedded backend is SQLite WAL for one controller, with owner-only files and a content-addressed artifact tree.
This supports concurrent local workers and one persistent Kubernetes controller replica. PostgreSQL provides a shared,
tenant-scoped append-only provenance plane and evidence leases. It does not make local prompt-state or artifact files
active-active. See [agent state](agent-state.md), [provenance](provenance.md), and
[cluster deployment](cluster-deployment.md).

## Logical identity and runtime generations

A named agent has a stable logical ID, owner, objective, role, state run, and workspace. Each OpenShell sandbox gets a
new generated runtime name. Each retry gets a fresh provider session and provenance segment. Sandbox IDs, Pod UIDs,
provider session IDs, and KV cache keys never become the stable identity.

The local controller holds one per-agent lock. Its common allocator records a file-locked slot reservation while it
checks the route cap, then releases the global lock before the slow runtime create. Separate host processes can create
different agents concurrently without oversubscribing the configured inference slots. If a creator exits, the next
allocation checks the unlocked reservation against both controller state and the gateway before reusing the slot.
Release commits remote workspace and state custody before deleting compute. A pull or reconciliation failure keeps
the runtime for recovery. See [named-agent continuity](continuity.md).

This registry remains a single-writer controller primitive. The separate remote broker adds authenticated canonical
principals, group and per-user quotas, durable tasks, independent provider and task retry limits, fenced leases,
PostgreSQL ownership metadata, and owner-only artifact download. One broker worker owns the RWO registry and artifact
tree. It is not active-active or an object-store custody adapter. The loopback Axum dashboard is not the remote
boundary. See [remote broker](remote-broker.md).

One Rust observer updates a cached controller snapshot every two seconds. The Svelte browser receives semantic
revision, digest, health, and stale-state updates over one server-sent event feed, then reads the cache when a revision
changes. Probe clocks do not change the digest. Provenance runs have a separate bounded graph feed. Both streams stop
through the controller shutdown signal, so open browser connections do not block a clean service stop.

Normal search is metadata-only and paged. Restricted content and artifact bytes use a separate, audited, short-lived
evidence lease; restricted search uses the same exact-run lease and returns bounded excerpts. The Axum service stays on
loopback; Svelte is the view layer, not an authorization boundary.

The browser permission queue and typed Team Lead tools consume the same controller request records. The Team Lead
adapter checks the logical-agent owner, active sandbox, route allowlist, request UUID, and launcher policy before a
decision. LAN services, peers, mounts, write domains, and unlisted read domains remain human-only. This adapter is a
local same-user control, not authenticated multi-user authorization.

## Agent and tool protocols

The controller's team and research records remain the local orchestration authority. MCP connects an agent to a tool.
[A2A](https://a2a-protocol.org/latest/) connects separately deployed, opaque agents. This release has no A2A client or
server adapter. [kagent](https://kagent.dev/) and [Rossoctl](https://github.com/rossoctl/rossoctl), formerly Kagenti,
are upstream Kubernetes-native platforms to evaluate, not dependencies or qualified integrations here. Any future
adapter must pin the A2A version and map tenant, remote identity, task ID, artifact digest, cancellation, and result
class into the same host event and custody records before its output can enter a study. A2A messages are not a custody
ledger and do not replace state authority, access decisions, or the research seal.

## State and recovery

Controller JSON files are written to a private pending file, synced, then renamed. State mutations use an immediate
SQLite transaction and optimistic collection versions. State events use a SHA-256 previous-event chain, and artifact
files use SHA-256 paths. The host audit file uses an exclusive lock and its own previous-record chain. Lease supervisors
run in separate process groups and keep private logs. Runtime proofs and sync reports live in private state and appear
in the loopback dashboard. Deleting a sandbox captures evidence unless the operator explicitly skips it. Host work data
is kept by default.

The controller does not delete catalog artifacts. Failed team and research worker setup moves new work into the owning
project's private recovery directory instead of erasing it. Research runtime deletion keeps the study, outputs, logs,
seals, and comparison.

## Dashboard color contract

The dashboard uses the published [UC Davis digital color palette](https://communicationsguide.ucdavis.edu/brand-guide/colors)
as its color reference. Aggie Blue `#022851` and Aggie Gold `#FFBF00` carry the main visual structure. Published blue
and gold tints provide depth. Rec Pool `#6FCFEB` is limited to small status details, and Double Decker `#C10230` is
limited to destructive controls. This keeps secondary colors as accents and within the guide's two-or-three-color
limit.

Semantic foreground and background pairs are checked against a 4.5:1 text contrast floor and a 3:1 large-text or UI
floor. The light and dark themes share the same semantic roles. The project does not use UC Davis logos or licensed
fonts and is not affiliated with or endorsed by UC Davis.
