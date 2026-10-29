# Named agents and disposable compute

A logical agent is a stable controller identity with a durable objective, role, scoped state run, and workspace. Its
OpenShell sandbox and model session are replaceable. This lets a Team Lead release idle compute and later assign work
to the same named agent without replaying its transcript or keeping KV cache alive.

## Local lifecycle

Register once:

```bash
agent-sandbox agent create cluster-helper \
  --owner local-operator \
  --domain cluster-ops \
  --state-profile cluster-operator \
  --objective 'Inspect declared cluster state and keep evidence-backed operating lessons.' \
  --role 'read-first cluster operations subagent' \
  --route local-agent \
  --gateway-profile docker-local
```

For a coding worker, pair `--domain software-engineering` with `--state-profile software-engineer`. That profile keeps
execution, path ownership, coordination, facts, decisions, and lessons without adding cluster or scientific state.

Assign work:

```bash
agent-sandbox agent run cluster-helper -- \
  'Inspect the supplied read-only snapshot, identify the cause, and write the evidence paths and proposed fix.'
```

The default sequence is:

1. Lock the logical agent.
2. Reserve one slot under the global allocation lock and check the route's configured sandbox cap.
3. Materialize the profile-scoped state view.
4. Create a new OpenShell sandbox if no runtime is allocated.
5. Start a fresh Pi or OpenCode provider session from the objective, state view, and current task.
6. Record tool, usage, output, error, and provider-returned reasoning events outside the prompt.
7. Reconcile versioned state proposals and checkpoint the run.
8. Capture evidence, delete the runtime, and keep the logical identity, state, artifacts, and workspace.

Use `--keep-runtime` when the next assignment latency matters more than idle capacity. It is an explicit exception:

```bash
agent-sandbox agent run cluster-helper --keep-runtime -- '<task>'
agent-sandbox agent release cluster-helper
```

Other lifecycle commands:

```bash
agent-sandbox agent status cluster-helper
agent-sandbox agent start cluster-helper
agent-sandbox agent checkpoint cluster-helper --reason '<epoch boundary>'
agent-sandbox agent retire cluster-helper --reason '<retention reason>'
```

Retirement seals state but does not delete the record, workspace, artifacts, or provenance. There is no destructive
purge command in this lifecycle.

## What persists

| Data | Across model session | Across sandbox deletion | Prompt-facing |
|---|---:|---:|---:|
| objective, role, logical identity | yes | yes | yes |
| selected state collections | yes | yes | bounded by profile |
| state event chain and checkpoints | yes | yes | metadata tail only |
| provider-returned reasoning | yes | yes | no, restricted evidence |
| artifact bytes and versions | yes | yes | references only |
| workspace files | yes | yes | read through tools |
| provider session, KV cache, Pod/container ID | no | no | no |

The agent is told that its runtime is disposable. Hiding that fact would make stale process, port, mount, and network
assumptions more likely. Continuity comes from the stable identity and verified state, not from pretending the old
process still exists.

## Retry rules

One provider session is the default. `--max-attempts 2` or higher explicitly permits replay in a new provider session.
The controller bounds attempts at eight and exponential waits at 30 seconds. One provider session has a 3600-second
deadline by default. `--attempt-timeout-seconds` accepts 30 through 86400 seconds. A timeout becomes an
`attempt-timeout` result, can enter an explicitly enabled retry, and still goes through checkpoint and runtime release.
This does not make external effects idempotent. A tool that writes a database, changes a cluster, sends a message, or
controls an instrument needs its own idempotency key, fenced lease, or operator recovery rule before task retry is
allowed.

If remote workspace pull or state reconciliation fails, automatic release fails closed and keeps the runtime. This
preserves the only possible copy of remote work. If the gateway confirms that an active runtime is gone, the
controller removes its stale local runtime record and allocates a new generation. A live sandbox without its
controller record is a custody error and blocks replacement.

## Concurrent local agents

Different logical agents can run from separate controller processes that use the same controller state directory. A
per-agent file lock prevents two tasks from using the same identity at once. The common allocator reserves a route
slot under a global lock, then lets independent cold creates proceed concurrently. If the creating process exits, the
next allocator reconciles the unlocked reservation against the controller record and gateway before reusing its slot.
SQLite serializes state writes, and the audit log has its own append lock.

The controller reserves each provenance segment before starting the provider session. A controller crash can leave a
visible gap between started and completed sessions, but it cannot reuse the segment and mix later evidence into it.
The dashboard reports completed and started counts as `completed / started`.

This is one trusted local operator, not a multi-user authorization boundary. The loopback dashboard and local JSON
records must not be placed behind a shared Ingress or copied to multiple controller replicas.

## Shared Kubernetes control plane

A shared scientific web app needs a server-side Team Lead control plane:

```text
browser identity
      |
origin-checked session and canonical principal
      |
Team Lead API and durable task broker
      |                 |                    |
PostgreSQL state   object/artifact store   task/event outbox
      |
SandboxClaim from a reviewed SandboxTemplate
      |
warm capacity when qualified -> gVisor or Kata sandbox -> local inference and tools
```

The browser user does not get Kubernetes RBAC. The API maps the authenticated tenant and user to capabilities, then a
namespace-scoped workload identity creates only the allowed claim or task. PostgreSQL row-level security and server
checks isolate tenant, project, run, logical agent, state, task, and evidence records. Large immutable artifacts belong
in an object store with digests and retention rules. A vector index is derived retrieval data, not the state authority.

The shared broker needs:

- one stable logical-agent ID separate from each SandboxClaim, Pod, and provider session;
- durable desired state, attempts, generations, idempotency keys, and fenced leases;
- bounded retry classes and a dead-letter path;
- per-tenant, per-user, per-route, and cluster quotas;
- revocation and negative cross-tenant tests;
- an append-only event/outbox path for UI updates and recovery;
- template and image digests, RuntimeClass, NetworkPolicy, and ServiceAccount custody;
- a controller that releases claims after state and artifact custody succeeds.

Use HPA for the stateless API and broker workers. Kueue can admit queued work and apply fair quota. It does not create
sandboxes or scale an API. KEDA can scale a stateless queue consumer when its queue semantics are qualified. It should
not own a long-lived logical agent or replay side effects. Agent Sandbox `SandboxWarmPool` can cut allocation latency
when the chosen provider consumes claims from that pool. A warm pool consumes capacity and does not replace admission,
tenant isolation, or a measured cold/warm latency gate.

NVIDIA OpenShell and Kubernetes Agent Sandbox are separate projects. OpenShell's Kubernetes path is upstream alpha as
of this release, and OpenShell gateway method RBAC is not per-sandbox tenant ownership. Use a separate gateway per
trust boundary or an external tenant broker. Do not call a shared gateway a multi-user boundary without negative
ownership tests.

## Team Lead integration

On one Mac or Linux host, a Team Lead can call the typed CLI and parse `--output json`. The controller is the lifecycle
authority; the model should not call raw `openshell`, `docker`, or `kubectl` for allocation.

On a shared cluster, the Team Lead calls the authenticated broker API. The broker writes a durable task before
allocation and returns a task ID. UI status comes from the event/outbox stream, not from holding the request open. A
worker claims one attempt with a fenced lease, restores the logical agent's scoped state, runs it, commits state and
artifact references, and releases compute. A lost worker is recovered from the lease and desired generation.

A2A is optional transport between separately deployed agents. It does not replace task ownership, durable state, or
authorization. MCP is for bounded tool access. Neither should carry unrestricted Kubernetes credentials or the full
state backend.

## Current boundary

Version 0.8.0 implements the local named-agent lifecycle and a separate authenticated multi-user broker for durable
cluster tasks. The broker has PostgreSQL identity, group quota, task, lease, event, and artifact metadata; one fenced
worker; and owner-only state, provenance, result, and workspace exports. Each broker task currently gets its own logical
agent ID. It does not offer a user-named agent reused across several broker tasks.

The exact OpenShell platform still needs its live runtime qualification. The broker's prompt-state projection and
artifact bytes remain RWO single-worker state. It does not ship an object-store adapter, active-active prompt state,
Kueue adapter, or WarmPool allocator. See [remote broker](remote-broker.md).
