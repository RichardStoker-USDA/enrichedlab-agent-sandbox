# Cluster deployment

The cluster path has two control surfaces. A researcher uses the authenticated remote broker. A site operator keeps
Kubernetes, OpenShell, database, storage, and identity configuration in a private GitOps repository. Raw OpenShell and
Kubernetes access are not user APIs.

The first broker release targets a small research site with one tenant, one controller state tree, one API Pod, one
worker Pod, and up to 16 concurrent child agents after load qualification. PostgreSQL stores shared identity, research
groups, profile grants, task state, leases, lifecycle events, and artifact metadata. Controller state and artifact
bytes use RWO persistent volumes on one selected controller-storage node.

Read [remote broker](remote-broker.md) for client commands and the full rollout order.

## Request and compute path

```text
Pi, OpenCode, or remote CLI
        |
Cloudflare Access user login and MFA
        |
Cloudflare Tunnel
        |
broker API ClusterIP
        |---------------- PostgreSQL
        |
single fenced worker leader
        |
OpenShell Kubernetes gateway
        |
one on-demand sandbox per admitted task
        |
approved inference and tool services
```

The laptop is a client. After the API commits an accepted task, closing the laptop does not stop it. The worker renews
a fenced lease while the controller runs a bounded Pi or OpenCode session. It releases sandbox compute only after
state, provider-returned reasoning, tool I/O, result, and workspace custody are exported or the task records a custody
failure.

## Identity and research groups

Cloudflare Access runs the selected identity-provider and MFA policy. The broker independently verifies the signed
origin assertion against the exact team-domain issuer, application audience, RS256 key, expiry, and current JWKS. The
stable principal is issuer plus subject. The verified lowercase email can consume an operator-created invitation on
first use.

The declarative catalog owns invitations and membership. Removing an invitation on the next seed disables that
principal when no other invitation remains and removes its group membership. Re-adding it restores the current
principal by its verified email. Test removal and re-entry with synthetic users before adding a live group.

Each group sets total active, per-user active, and queued-task limits and receives an explicit set of profiles. A
profile fixes the model route, harness, reasoning setting, controller and gateway policies, digest-pinned sandbox
image, CPU, memory, state view, role prompt, task disruption attempts, provider attempts, and run deadline. The task
stores a profile snapshot so later catalog changes cannot change an in-flight attempt.

Roles named `researcher`, `lead`, and `reviewer` are catalog metadata in this release. API reads are owner-only. Group
oversight and delegated review are not implemented, so do not treat role names as authorization.

## Placement and scale

The reference worker remains on the node that owns both RWO volumes. `max_parallel` controls simultaneous controller
children. OpenShell creates and removes sandbox Pods through its Kubernetes compute driver, so sandbox compute scales
from zero to the admitted task count without scaling the worker Deployment.

Label exactly one qualified storage node `enrichedlab.ai/agent-control-node=selected` for this release. The API,
worker, migration, and seed Pods use that selector. Giving the label to several nodes can cause an RWO multi-attach
failure and is outside this release's qualification. Sandbox placement is separate and remains governed by the
OpenShell compute profile, RuntimeClass, node affinity, and taints.

Use OpenShell policy, sandbox resource requests, node affinity, taints, RuntimeClass, namespace limits, and inference
route capacity together. Do not place untrusted agents on a control-plane, database, or GPU inference node unless the
site has qualified that placement. A sandbox can call a private inference Service without receiving a GPU.

The current broker calls OpenShell's sandbox lifecycle. It does not create Kubernetes SIG Agent Sandbox
`SandboxClaim` objects, consume a `SandboxWarmPool`, or submit external Kueue workloads. Those systems can reduce cold
start or add physical admission only after an adapter maps their ownership and lifecycle into broker leases. Kueue is
not an autoscaler, and a RuntimeClass is not proof that its handler ran.

NVIDIA marks the OpenShell Kubernetes path experimental. A passing K3s/Kata site test qualifies only the recorded
OpenShell, Kubernetes, CRD, CNI, RuntimeClass handler, storage, node, image, and policy versions.

## Kubernetes packages

The public source has composable Kustomize inputs:

| Path | Contents |
|---|---|
| `deploy/kubernetes/broker/foundation` | namespace, tokenless ServiceAccounts, RWO claims, default-deny policy |
| `deploy/kubernetes/broker/workloads` | API Deployment, worker Deployment, private ClusterIP Service |
| `deploy/kubernetes/broker/base` | foundation and workloads for source validation |
| `deploy/kubernetes/broker/example` | reserved-value workload overlay, not deployable unchanged; apply foundation separately |
| `deploy/kubernetes/broker/operations/migrate` | versioned one-shot schema migration Job |
| `deploy/kubernetes/broker/operations/seed` | versioned one-shot catalog reconciliation Job |

The example contains a zero image digest and reserved addresses. It contains no Secret. A private overlay must replace
the image, storage, edge, database, JWKS, and OpenShell selectors with exact site values, and the site must label one
qualified controller-storage node as selected. Expose only the broker
API ClusterIP through the Access-protected Tunnel. Do not add a NodePort or public LoadBalancer.

The example JWKS path uses an internal HTTPS relay. Its private overlay must create `agent-broker-jwks-ca`, put the
relay CA bundle in `ca.pem`, and keep `access_jwks_ca_file` pointed at `/run/trust/jwks/ca.pem`. An init container
copies the projected ConfigMap entry into a private regular-file volume before the API starts. The broker rejects a
symlink at that path and never disables TLS or hostname checks. If the site uses Cloudflare's public cert endpoint
instead, remove that setting and provide a reviewed egress path.

The long-running API and worker use separate function-only PostgreSQL roles. The migration Job alone receives the
migration-owner DSN. The worker receives an OpenShell client bundle through an owner-managed Secret and has no
Kubernetes ServiceAccount token.

## Flux order

Use separate Flux children and `dependsOn` edges for this order:

1. publish and scan the source-matched controller image;
2. reconcile foundation and wait for storage and policy readiness;
3. apply migration 002 with the migration owner;
4. apply the fixed API and worker function grants;
5. reconcile the encrypted tenant, groups, profiles, grants, and invitations catalog;
6. reconcile API and worker workloads;
7. activate the Cloudflare Tunnel route and run the live negative tests.

Do not activate a Flux-owned successor before its source and image exist. Do not leave two same-name operation Jobs in
the graph. Keep a prior image digest and Git revision for rollback.

## Storage and recovery

PostgreSQL is the durable authority for task admission and ownership. Task artifact bytes, complete state exports,
provenance events including provider-returned reasoning, and workspace snapshots stay on the artifact volume. The
controller registry and prompt-state projection stay on the controller volume. Deleting a sandbox frees compute but
does not delete those records.

`retention_days` is catalog metadata. No automatic deletion runs in this release. A site must add a reviewed retention
job only after backup, legal, research-data, user notice, and failed-deletion tests exist.

Stop admission before restoring. Restore PostgreSQL and both volumes from one recovery point, verify migrations and
artifact hashes, start one worker leader, then reopen the API. The API can restart without owning an agent process. A
worker loss expires its fenced lease and requeues the task within its task-attempt limit.

The RWO release is not active-active. Moving to several API and worker replicas needs shared or object-store artifact
custody, a shared prompt-state projection, one logical-agent writer, tenant storage quotas, and tested failover. Do not
increase replicas over one local state tree.

## Qualification

Run `scripts/qualify_remote_broker.sh` for the migration, role, invitation, revocation, owner isolation, quotas,
generation fence, cancellation, events, and artifact metadata contract, and `scripts/qualify_migration_drafts.sh` for
the upgrade of every committed draft of a migration no release carries yet. Render every Kustomize package and run a
client-side apply dry-run. Then run the site matrix in the
[cluster broker skill](../.agents/skills/enrichedlab-agent-broker/SKILL.md) against the exact live release.

Source qualification does not prove the Cloudflare policy, CNI, RuntimeClass handler, OpenShell gateway, storage
recovery, model capacity, or live five-agent latency. Those claims need recorded tests on the target cluster.
