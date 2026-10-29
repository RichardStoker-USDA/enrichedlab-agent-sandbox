# Direct Sandbox execution driver

`broker::execution::DirectExecutionDriver` composes an already-admitted task
with the existing direct Sandbox runtime, broker lease fence, and artifact
retainer. It is a trusted server boundary, not a worker route or client API.

The caller provides the admitted record, a worker identity, cancellation and
shutdown tokens, and a `TerminalArtifactRetainerFactory`. The factory runs only
after current terminal evidence and receives the exact handle and completion.
It closes over a server-owned staged export record, approved mounts and frozen
output selection. Task text never selects a mount, artifact root, or export
path.

A worker enables this path only with paired private
`AGENT_SANDBOX_BROKER_DIRECT_DISPATCH_CATALOG` and
`AGENT_SANDBOX_BROKER_DIRECT_DISPATCH_WRITER_UID` settings. The catalog is
loaded before task claims. An unmatched task stays on the existing OpenShell
route. A matched task with invalid admission or no terminal export configuration
fails closed before legacy controller files, state, or compute are created. When
the catalog is configured, startup requires the broker's direct-execution readiness
check, including the runtime-event schema and worker-only event function grant and
the attempt execution record of migration 019, before leader acquisition or task
claims. Each attempt's image digest and the canonical digest of the exact Sandbox
object the worker submits (command, arguments, limits and input volumes) are recorded
under its lease before the Sandbox is created; a refused record stops the attempt there. The terminal record producer and its
per-run PVC mount remain a separate trusted deployment prerequisite.

The driver renews the existing lease before and after creation and while it
waits. Startup, Pod and terminal reads, recorder writes, and finalization are
also bounded by that heartbeat, cancellation/shutdown tokens, and the configured
deadline. An interrupted create is recovery-only because its exact Sandbox handle
is uncertain. It pins the first observed Pod UID, accepts terminal completion only
through the existing current-condition and terminated-runner checks, then uses
the runtime's retention-before-cleanup path. `Finalized` can represent a failed
process completion; callers must inspect `completion.succeeded()` before any
success claim.

The caller must provide the required `RuntimeEventRecorder`. It records only
the created Sandbox handle, first Pod observation, terminal observation and a
restricted recovery code in the existing broker task-event ledger. A recorder
fence rejection stops the driver before cleanup; it is never a no-op telemetry
hook.

Factory preparation is cancellable and renews the same lease while it waits for
the staged export record. A factory must stop and discard partial preparation
when its cancellation token fires; the driver holds a drop-cancellation guard so
aborting the driver also cancels a still-pending preparation worker. The existing
runtime renews the lease again during byte retention and performs cleanup only after retained
artifacts are registered.

Cancellation, shutdown, deadline, lease loss, observation failure, retainer
preparation failure, and finalization failure return `RecoveryNeeded` with the
exact handle when creation succeeded. They do not delete a running Sandbox or
declare task success. `RecoveryNeeded` is not a commissioned recovery service.
Existing broker lease-expiry retry rules apply, and trusted staging must reconcile
prior compute and storage custody before it authorizes another attempt.

## Retained output recovery

An operator can recover completed output from a failed delivery without rerunning analysis or reviving a worker
lease. Preserve the original terminal record, custody plan and receipt, compare the retained PVC UID, and copy only
the manifest's selected files to `tasks/<task>/recovered/<recovery UUID>/<relative path>` under the broker artifact
root. Copy exclusively, with private directory/file modes, and verify size and SHA-256 before database registration.
Do not mount another principal's volume into a research sandbox or grant runtime workers recovery authority.

Migration 013's owner-only function registers those verified files and an append-only recovery receipt in one
transaction. It binds the original runtime event and terminal checksum, checks the current task generation, and
refuses active or successful tasks. Recovery does not change the original failure, review state or scientific
validity. The retained original evidence and operator journal remain the recovery authority. This is an explicit
operator action, not an autonomous retry service. After a successful registration, use the normal authenticated
artifact download; no separate public recovery endpoint is added.
