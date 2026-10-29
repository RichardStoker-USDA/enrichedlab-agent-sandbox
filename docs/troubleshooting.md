# Troubleshooting

These are failures reproduced during the macOS, OrbStack, and OpenShell 0.0.116 qualification.

## OrbStack is running but Docker does not answer

`docker context show` proves only which socket the client selected. Before installing or qualifying the Docker profile, require both `docker info` and `agent-sandbox doctor` to pass.

If OrbStack reports `running` or `starting` while its Docker socket times out:

1. Capture `~/.orbstack/log`, the output of `orbctl version`, and a local `orbctl report` when that command returns.
2. Stop current qualification work. The controller cannot prove runtime identity or cleanup while Docker is unavailable.
3. Try OrbStack's data-preserving `orbctl restart docker`, then rerun `docker info` and `agent-sandbox doctor`.
4. If the CLI and Docker socket remain unresponsive, leave controller state intact and use OrbStack's recovery or support path. Do not use `orbctl reset`; it deletes Linux and Docker data.

The dashboard can remain healthy because it reads host state, but that does not make the Docker adapter ready. A successful sandbox test followed by an OrbStack VM I/O exit is a host-runtime incident and blocks a current Docker qualification until the engine recovers and cleanup is rechecked.

## Wrong target port

`openshell forward start <port>` uses that number for both sides. Use the service form when the host and app ports differ:

```bash
openshell forward service <sandbox> \
  --target-host 127.0.0.1 \
  --target-port <app-port> \
  --local 127.0.0.1:<host-port>
```

The controller's `service open` command generates this form and supervises its lifetime.

## Consecutive hyphens in a sandbox name

OpenShell returns `name must not contain consecutive hyphens`. The controller validates user names and removes hyphens from generated team-name prefixes before adding separators. Do not hand-edit a state record to bypass that check.

## Reserved workspace mount

OpenShell returns `mount target '/sandbox/work' is reserved for the OpenShell workspace` when an image sets its image-level working directory to that path while the controller also supplies the project mount. Keep the image `WORKDIR` at `/sandbox` or another non-reserved directory. The repository contract test checks the provided Containerfile, and release qualification must create a sandbox from the built image.

## Peer address assumptions on OrbStack

OrbStack routes `host.openshell.internal` through its VM and host NAT. A host process sees a loopback source, not a stable sandbox IP. Do not use that address as peer authentication and do not force `host_gateway_ip` into a package-managed macOS gateway. The controller uses a source-only OpenShell rule and runs the relay inside the approved source sandbox.

For HTTP through the source-local peer port, set `Host` to the lease's `source_endpoint`. A direct request to `source_endpoint` sets the authority without extra client configuration.

## OpenShell log download scope

OpenShell's download command rejects `/var/log` because downloads are limited to the sandbox workspace. Evidence capture lists and reads only `openshell*.log` through authenticated execution, applies file and total size limits, and keeps an incomplete `capture-status.json` if capture stops early.

Immediately after a host-runtime recovery, the first authenticated log read can fail while the sandbox remains Ready. `scripts/qualify_sandbox.sh` refreshes the runtime proof and retries the complete evidence snapshot once. A second failure stops qualification and keeps the incomplete snapshot for diagnosis; it does not skip evidence or delete controller state.

## Failed sandbox creation

The controller asks OpenShell to delete a failed resource and verifies the exact sandbox name is absent before returning. A failed team worker is removed from the team record and its clone is moved to the team's `recovery` directory. Check the OpenShell gateway logs and the recovery assignment before retrying. Never reuse the recovery clone as a different worker without reviewing its branch and task.

## Logical agent says its runtime is still active

Run `agent-sandbox agent status <name>`, `agent-sandbox list`, and the gateway's sandbox list. If both controller and
gateway records exist, release the logical agent. If the controller record exists and the gateway proves the sandbox
is absent, the next `agent start` reconciles the stale runtime record and creates a new generation. If the gateway has
a live sandbox but its controller record is missing, the controller fails closed. Recover or delete that exact runtime
under operator custody before another allocation.

Do not edit the logical-agent JSON to clear `active_sandbox`. That can leave an unowned runtime and allow overlapping
work under one identity.

## Logical-agent release keeps the runtime

For a sync profile, release pulls the remote workspace and reconciles state before deletion. A failed pull,
outbox reconciliation, checkpoint, or evidence capture blocks deletion because remote work may be the only copy. Fix
the sync or state error, capture a private snapshot if possible, then rerun `agent release`. Do not use ordinary
`delete --skip-evidence` to bypass named-agent custody.

## Route is at its concurrency cap

The common allocator counts all managed sandboxes and active slot reservations on the route, including ordinary,
team, research, and logical-agent runtimes. An unlocked reservation is checked against controller state and the
gateway before its slot can be reused. If gateway custody cannot be checked, allocation fails closed. Release idle
work or raise the route cap only after checking model concurrency and memory. Kueue admission and Kubernetes node
capacity do not override the inference route cap.

The Runtime dashboard lists current reservations. `allocating` is an active creator, and
`committed-cleanup-pending` is a short record-save boundary. `unowned-live` means a creator exited after OpenShell
allocated compute but before the controller saved custody. `custody-unknown` means the gateway could not be checked.
Both unresolved states fail `agent-sandbox doctor`; recover or delete the exact OpenShell sandbox before allocating
more work on that route.

## Research worker fails before its first segment

The controller installs the selected pinned Pi or OpenCode runner before a research worker starts. The supplied
`development` and `data-science` policies allow read-only access to `registry.npmjs.org` for `/usr/bin/npm`, its
`/usr/bin/node` interpreter, and the two pinned harness binaries, so a fresh sandbox can install that exact version. A
custom policy or gateway that removes this endpoint cannot use the runner unless its image already contains the pinned
binary.

Bootstrap failures use the `controller-bootstrap` result class. The controller stores the bounded error in the
worker's private segment log instead of returning it through the dashboard. Fix the policy, gateway, or image, then
reset the failed worker before relaunching it. Do not mark a worker complete from a Ready sandbox alone.

## PostgreSQL provenance fails to start

Run `agent-sandbox provenance prepare` with the migration DSN present and inspect the error without printing either
DSN. Then remove the migration DSN and use `provenance graph` or the dashboard with the runtime identity. `prepare`
never falls back to the runtime DSN. Common failures are:

- the runtime role was not granted through `migrations/postgres/grant_runtime.psql`;
- the runtime role owns the schema or tables, which invalidates the intended least-privilege boundary;
- a remote DSN does not use TLS;
- the tenant identifier contains unsupported characters;
- row-level security was disabled or a role has `BYPASSRLS`;
- an older migration owner did not create the append-only triggers.

Do not make the runtime role a database owner or superuser to bypass an error. Restore the migration identity, apply the
versioned migration and grants, then repeat the tenant-crossing and update/delete negative tests.

## Remote broker login succeeds but the API returns 401

The workstation token goes to Cloudflare as `Cf-Access-Token`. The broker reads the Access-injected
`Cf-Access-Jwt-Assertion`. Do not change the client to send the origin header directly and do not make the broker trust
an email header. Check the configured broker origin, Access application audience, team-domain issuer, JWKS reachability,
session expiry, and tunnel-side Access validation without printing the token.

Run `agent-sandbox-remote login`, then `agent-sandbox-remote me`. If `cloudflared` is missing or its session expired,
fix that client dependency. If the broker reports `access_keys_unavailable`, restore its exact HTTPS JWKS route. Do not
open general internet egress to hide a relay or NetworkPolicy error.

## Verified remote user has no broker access

Cloudflare authentication and broker invitation are separate checks. `not_invited` means the verified lowercase email
is absent from the active broker catalog. `principal_disabled` means a prior principal no longer has an active
invitation. The operator reviews the private catalog, reconciles the seed Job, and tests the intended group. Do not
insert a membership through the API role or trust a client-supplied group.

An invitation removal is a revocation. It disables a principal with no remaining invitation and deletes its group
membership while retaining task and evidence custody. Re-adding the same verified email restores current catalog
membership on seed.

## Remote task stays queued

Check the task events, group total active limit, per-principal active limit, enabled profile grant, worker readiness,
worker leader lock, route capacity, OpenShell gateway, and eligible sandbox nodes. `group_queue_full` applies at
submission. A queued task waiting behind active work is expected and survives client disconnect.

Do not raise the catalog quota until the sandbox nodes, inference route, tool services, database, and artifact volume
pass the same concurrent load. Kueue and SandboxWarmPool are not connected to the broker in this release.

## Remote task requeues after a worker restart

The worker owns a timed lease and generation. On shutdown it stops its controller process group. An expired lease
requeues within `max_task_attempts`; a stale worker cannot register or finish after another generation claims it.
Repeated `lease_expired_requeued` events point to worker termination, storage stalls, database reachability, or a lease
shorter than final custody work. Keep one worker over one RWO controller tree and inspect operator-only logs.

`max_provider_attempts` controls retries inside one controller run. It is separate so task recovery does not multiply
provider calls without a visible bound.

## Remote artifact download fails integrity checks

The API resolves a task-owned regular file under the configured artifact root, checks its stored size and SHA-256, and
adds the hash to the response. The client verifies both again and removes a partial file. An
`artifact_integrity_failed` response means storage bytes no longer match PostgreSQL custody. Stop downloads for that
task, preserve the database row and affected volume, and investigate restore, mutation, or filesystem failure. Do not
edit the digest to make the download pass.

## Provenance dashboard returns 503

The Rust PostgreSQL client is synchronous, so provenance queries run in a bounded worker pool rather than on Tokio's
async threads. A 503 means that pool is saturated or a worker failed. Check request rate, graph limits, database
latency, and controller logs. Do not increase the queue without measuring memory and dashboard latency. Large offline
reviews should use export rather than polling one unbounded graph.

## VM guest has no Landlock

Some OpenShell VM guest kernels report Landlock unavailable. A create with the default hard requirement can remain in setup until the gateway returns its failure. Confirm the guest kernel capability through OpenShell, delete the failed sandbox, and use `--landlock best-effort` only after accepting that specific limit. The microVM boundary is separate from Landlock and does not make the hard-Landlock check pass.

## Kubernetes runtime proof is partial or failed

Run `agent-sandbox platform refresh <sandbox>` and inspect the observer error. A partial proof means no observer was configured. A failed proof commonly means the requested RuntimeClass was not set on the Pod, its RuntimeClass object or handler is missing, or the Sandbox CR/Pod labels did not select exactly one resource. Check the configured namespace and read-only observer identity. Do not edit the controller proof file or mark the profile qualified from gateway health alone.

## Kubernetes Sandbox Pod lacks OpenShell labels

OpenShell 0.0.116 can put its identity labels on the Sandbox CR while the Agent Sandbox controller puts only `agents.x-k8s.io/sandbox-name-hash` on the Pod. A NetworkPolicy that selects `openshell.ai/managed-by=openshell` then matches no sandbox Pods, and the supervisor cannot fetch policy from the gateway.

Select the controller Pod label in namespace policy for this pinned pair. The runtime observer finds candidate Pods with that label and accepts only the Pod whose controller owner reference has the exact Sandbox UID. Recheck both objects on every OpenShell or Agent Sandbox upgrade. Remove the compatibility selector when the live Pod carries stable OpenShell labels and the replacement policy has passed an isolation probe.

## Kubernetes peer cannot resolve the host alias

A remote Kubernetes Pod may not have a `host.openshell.internal` mapping, and default-deny policy should block arbitrary node or LAN access anyway. Do not add a broad host alias or private-subnet exception for peer links. The Kubernetes peer adapter uses the target Pod IP, a source-specific NetworkPolicy pair, TLS, and a per-lease capability. Enable the bounded NetworkPolicy mutation setting for that profile and give its observer identity the matching namespace-scoped RBAC.

## Sync pull did not change host work

`agent-sandbox sync pull <sandbox>` is intentionally non-applying. It writes a private snapshot and receipt. Review them, then rerun with `--apply`; the controller creates a private backup before replacing the host tree. File-count, byte, unsafe-target, or escaping-symlink failures stop before apply.

## Lease supervisor was killed

Run `agent-sandbox service list`. For an active host or peer record whose supervisor is gone, the controller checks the stored child PID, expected command fields, and process-group ownership before sending a signal. A matched orphan is stopped and the lease becomes `failed`. Dependent LAN listeners, peer source policy, source relays, and controller-owned parent forwards are then closed.

## Non-interactive exec does not return

Non-TTY controller commands must close stdin before starting OpenShell. An SSH runner or host harness can otherwise leave its input channel open after the sandbox command exits, which keeps `openshell sandbox exec` waiting for more input. The controller sends `/dev/null` to non-TTY commands and inherits stdin only for an interactive shell.

To distinguish this case from a gateway or sandbox failure, run the same OpenShell command once with stdin redirected from `/dev/null`. If it returns, update the controller. Do not force a TTY for automation.

If the lease failure says that orphan cleanup failed, do not delete or edit its state file. Compare the reported PID with host process and listener evidence, then close the process only after its ownership is known. The controller refuses to signal a PID whose command or process group does not match the lease.

## Git reports an unknown committer identity

Controller 0.2.1 and later use `git_committer_name` and `git_committer_email` from private `controller.toml` for host-created team merge commits. They do not read global Git configuration or inherited author variables. If this error appears on an older binary, upgrade before retrying the merge; the failed merge is aborted and the collected worker commit remains available for review.
