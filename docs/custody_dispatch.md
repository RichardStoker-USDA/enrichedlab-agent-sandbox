# Custody dispatcher

`agent-sandbox-custody-dispatcher` is the portable site adapter that starts the
existing one-shot input and output custodians after their worker notices appear.
It has no task, model, Kubernetes, storage, export, or recovery authority.

The private configuration uses `sandbox.enrichedlab.org/custody-dispatch/v1`.
It declares a private state root, one bounded input channel and one bounded output
channel. Each channel fixes its notice root, same-name approved plan root, custodian
binary, immutable `programOwnerUid`, runtime-owned custodian configuration, expected
configuration owner UID and timeout. Paths are absolute. Private roots and records are
held and checked as descriptors; a fixed program must be a regular, non-group/world-
writable executable owned either by root or by the runtime configuration owner. Child environments
are cleared, stderr is discarded, stdout is bounded, and only a typed receipt matching
the notice task UUID, attempt and lease generation is accepted.

Before starting either child, the dispatcher creates and fsyncs an exclusive journal
under the state root. It records channel, scope digest, task UUID, attempt, lease
generation, phase and receipt digest. These are typed correlation metadata only. It
never stores a notice, plan, child stdout, database reference, credential, or child
stderr. A duplicate completed scope is skipped. A started, failed, timed-out, cancelled,
invalid, or otherwise partial scope is `recovery_required` after restart and is never
run again by this program. The explicit site custody-recovery owner must reconcile that
journal, the notice, plan, database authority and any retained resources before a new
attempt can be authorized.

The service holds one `fs2` exclusive lock at the state root. `scanLimit` bounds each channel's pending batch,
not retained history. Any existing journal fences its scope and does not consume pending capacity.
Invalid notices and notices awaiting a manual plan cannot displace runnable work. The directory walk remains
linear in retained names, but the selected batch and child concurrency are bounded. This file adapter does not
claim constant-time discovery at large retention volumes; a future queue adapter must preserve the same journal fence.
The dispatcher starts only the configured bounded number of children at once.
A failed scope does not stop unrelated task scopes. Cancellation sends SIGTERM to the
fixed child process group, reaps it, preserves the journal and returns without deleting
any notice, plan, custody record, or retained output.

The dispatcher does not provision service accounts, grants, Kubernetes policies,
notices, roots, migrations, a recovery worker, or a retry queue. The site must
supply those inputs and account for the input and terminal custody time within its
worker lease/deadline contract.

## Multiple execution environments

The optional top-level `routes` list lets the shared dispatcher select separate
custodian and preparer configurations for enrolled research users. Each entry has
`binding`, `input` and `output`. The binding is the complete server-owned Sandbox
binding: principal ID/hash, group, profile key/ID, lab, namespace and runtime class.
Each channel override supplies `config` and, when the parent channel has a preparer,
`preparerConfig`. It cannot change the executable, UID, notice/plan directories,
timeout or shared concurrency limit. Every referenced file passes the same private
file checks during dispatcher startup.

An empty list retains the single-environment configuration. A nonempty list is a
complete allowlist: an unmatched binding gets a retained `recovery_required` journal
with `custody_dispatch_route_unavailable`, and neither child runs. Duplicate
principal/group/profile selectors are rejected even when their namespaces differ.
The site must enroll its existing binding along with new ones before enabling routes.

This permits one dispatcher to coordinate separate user namespaces and input copies.
It does not grant Kubernetes access, provision a namespace, copy a source dataset or
make one user's retained objects available to another. Each routed custodian still
checks its configured namespace, database scope, current lease and exact binding.
The worker's binding catalog, namespace RBAC, approved source custody and researcher
starting-point catalog must agree before exposing a new analysis to a scientist.

The dispatcher retains the operator's `SSL_CERT_FILE` path when clearing the child
environment, so a private database CA remains usable. It forwards no database value
through the environment. A bounded, typed child error identifier is retained in the
recovery journal; arbitrary stderr and nested transport errors are discarded. Input and
output custodians preserve typed lower-layer error codes without printing their payloads.
Initialize every configured journal/evidence directory as the configured writer, mode
0700. A failed journal is recovery evidence and is never cleared to trigger a retry.

## Catalog preparation

A channel may declare an optional `preparer` with `program`, `programOwnerUid`, `config`,
`configOwnerUid` and `timeoutSeconds`. It uses the same fixed-program, private-file, deadline,
clean-environment and process-group boundary as the custodian. It receives only `--config`,
`--config-owner-uid` and the checked `--request` filename. The model cannot select a program,
path, credential, resource flag or environment through this interface.

When a plan is absent, the dispatcher records its exclusive start before calling the preparer.
The preparer resolves the approved site catalog and atomically publishes the same-name plan.
Its receipt must match the channel, task, attempt, generation, filename, exact size and SHA-256
of the private plan. Only then does the existing custodian run its independent current-lease,
input/source, namespace, identity and policy checks. A preparation receipt is not data access
or scientific approval. Channels without a preparer keep the manual-plan path.

The journal retains the prepared plan digest. Preparation failure or interruption requires
explicit recovery, just like an interrupted custodian. The dispatcher never deletes a failed
journal or retries a partial effect automatically. Catalog bytes and any produced source/code
remain under the site's retained evidence contract. New labs and methods require authorized
catalog and binding entries, not code embedded in model objectives.
