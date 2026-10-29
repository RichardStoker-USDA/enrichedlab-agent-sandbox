# Site deployment procedure

## Read-only discovery

Record the exact source commit, Kubernetes and OpenShell versions, Sandbox CRDs, CNI, RuntimeClasses and handlers,
eligible controller and sandbox nodes, storage classes, database operator and version, Flux parent and child graph,
container registry path, Cloudflare Tunnel namespace, and Access application status. Confirm whether NetworkPolicy is
enforced on every selected node. For the RWO release, prove exactly one qualified node has
`enrichedlab.ai/agent-control-node=selected`.

Do not infer a microVM boundary from a RuntimeClass name. Prove the Pod uses the intended handler on the selected node.
Do not continue when the only available placement would put untrusted sandboxes on an inference or control node against
site policy.

## Private inputs

The site overlay supplies these at runtime:

- immutable controller image digest and digest-pinned sandbox images;
- controller config and route catalog;
- tenant, group quota, profile grant, and invitation catalog;
- exact Access public origin, issuer, application audience, and HTTPS JWKS route;
- a read-only CA ConfigMap when the JWKS route uses a site-issued HTTPS certificate; the Pod stages its projected
  `ca.pem` as a private regular file before the API starts;
- separate migration, API, and worker PostgreSQL Secret references;
- OpenShell client bundle Secret reference;
- node labels, storage classes, resource limits, and exact NetworkPolicy peer labels.

Reference each Secret by name or mounted path. Never print, decode, commit, or put a credential in a command.

## Ordered rollout

1. Run source tests, the public-boundary scan, skill validation, migration qualification, and Kubernetes client dry-run.
2. Build `images/controller.Containerfile`, scan it, verify its non-root identity and binary versions, publish it, and
   record the immutable digest.
3. Create a private overlay from `deploy/kubernetes/broker/example`. Replace all reserved values and zero digests.
4. Reconcile `deploy/kubernetes/broker/foundation` through its Flux child. Wait for namespace, ServiceAccounts,
   NetworkPolicies, and RWO claims to become Ready.
5. Provision the database and distinct roles. Reconcile `operations/migrate`, apply
   `grant_broker_runtime.psql`, then reconcile `operations/seed`. Remove completed one-shot operations from the active
   graph before reusing their names.
6. Stage the controller config, route catalog, API and worker DSN Secrets, OpenShell client bundle, and broker catalog.
7. Reconcile the workloads child. Keep one API and one worker replica in the first RWO release. Set worker
   `max_parallel` no higher than the tested sandbox, inference, storage, and database capacity.
8. Route the Access-protected Tunnel hostname only to the API ClusterIP. Keep the Service private and require Access
   validation in the tunnel and broker.
9. Run the live matrix below and record private evidence before inviting users.

Use Flux `dependsOn` for source, foundation, migration, seed, and workloads. Back up an exact live object before a
replacement. Publication precedes activation.

## Live qualification matrix

- intended identity with required MFA;
- Access edge denial without policy satisfaction;
- broker denial for uninvited, disabled, and wrong-audience identities;
- invitation removal revokes broker membership;
- cross-user list, get, cancel, event, artifact, and download denials;
- group queue, group active, and per-user active limits;
- idempotent submit and conflicting replay;
- one, three, and five concurrent tasks when those limits are offered;
- client disconnect, worker restart, lease expiry, generation fencing, bounded retry, and cancellation;
- sandbox creation on the intended node and RuntimeClass, then compute release after custody;
- state, reasoning, tool I/O, workspace, result, and failed-attempt artifact hashes;
- database backup and restore, controller and artifact volume restore, and catalog rollback;
- default-deny ingress and egress, denied raw ClusterIP from user devices, and no researcher path to OpenShell,
  Kubernetes, PostgreSQL, or persistent storage.

The reference release uses on-demand OpenShell creation. It does not integrate Kueue or SandboxWarmPool. Do not claim
warm-start or queue-fairness behavior that was not measured.

## Recovery

Stop admission before changing shared custody. A failed API can restart without owning an agent process. A worker
restart terminates its process group; an expired fenced lease is requeued until its task-attempt limit. Do not run two
workers over one controller RWO tree. Restore PostgreSQL and both persistent volumes from one consistent recovery
point, verify hashes and migrations, then reopen admission. Keep the prior image and Flux revision reachable for
rollback.
