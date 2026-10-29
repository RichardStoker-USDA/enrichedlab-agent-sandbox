# Remote agent broker

The remote broker lets a Pi or OpenCode session submit work to a cluster and disconnect. The task, verified user,
research group, quota decision, lifecycle events, result, state export, provenance export, and copied workspace stay on
the cluster. The OpenShell sandbox is allocated only while the task runs.

This is a different trust boundary from the loopback dashboard. Researchers call the broker API. They never receive a
kubeconfig, OpenShell client bundle, database credential, controller filesystem, or direct gateway connection.

## Request path

```text
Pi or OpenCode
  -> agent-sandbox-remote
  -> Cloudflare Access application and tunnel
  -> ClusterIP broker API
       -> PostgreSQL identity, group, task, lease, event, and artifact metadata
       -> one controller-worker leader
            -> NVIDIA OpenShell gateway
            -> one disposable sandbox per admitted task
       -> persistent controller and artifact volumes
```

Cloudflare Access handles the configured identity provider, including an institutional CILogon flow when the site
chooses it, and the required MFA policy. The API still verifies the signed
`Cf-Access-Jwt-Assertion` against the exact Access issuer, application audience, expiry, and current JWKS. It accepts
RS256 only and fails closed when signing keys cannot be checked. Do not trust `Cf-Access-Authenticated-User-Email`, a
client-supplied tenant, or OpenTelemetry baggage as identity.

The stable principal key is the verified issuer plus JWT subject. The authenticator that verified the identity also
names its class, `person` (the Access assertion) or `service_account` (the TokenReview mapping), and the database
keeps it with the principal (migration 030). A new identity without a class, or with any other class,
is refused (`principal_class_required`, `principal_class_unknown`), and an identity never changes class
(`principal_class_mismatch`). Every rule that needs a person, such as recording a review or sharing a note, reads the
class rather than inferring it from the issuer, so a future authenticator (SPIFFE, OIDC, recovery or peer identities)
must name its class before its principals can exist. An exact lowercase email invitation binds that principal on first
use. Later requests use the stable principal, not an email header. One principal can belong to more
than one research group. The catalog is declarative: removing an invitation disables a principal that has no other
invitation and removes that membership on the next seed. The seed writes only what differs, so running the same catalog again
changes no row.

The same catalog can grant `research.discovery.autonomous` to a verified person in one named group. The seed enables
listed grants and revokes grants removed from the catalog without deleting their history. A service account cannot
hold this capability. It is required for every campaign read and action in the first release.

## What a research group controls

Each group catalog entry sets:

- total active agents for the group;
- active agents per principal;
- queued tasks for the group;
- assigned agent profiles;
- intended retention days.

A profile fixes the harness, reasoning setting, model route, controller policy profile, OpenShell gateway profile,
digest-pinned sandbox image, CPU, memory, state profile, role prompt, run limit, task-attempt limit, and
provider-attempt limit. The task stores a JSON snapshot of those values at submission, so a later catalog edit does not
change an in-flight task.

`max_task_attempts` bounds recovery after a lost worker lease. `max_provider_attempts` bounds model-run retries inside
one claim. They are separate so task recovery cannot silently multiply model calls.

`tool_bundle` is a catalog and UI label. It is not an authorization control by itself. The named bundle must correspond
to the digest-pinned image, controller policy, allowed network routes, and service-side authorization reviewed for that
profile. A group receives a bundle only by receiving that profile grant.

A tool group is a named, versioned set of profiles (`tool_groups`: key, version, title, description and `tools`, the
profile keys), granted to a research group by `tool_group_grants` (group, tool group and version, one version of each
tool group per research group). A group's profile grants are its direct `grants` plus the profiles of every tool group
version it holds, so proposal and the worker keep enforcing one set of rows; the seed removes a profile grant only when
neither a direct grant nor another held version still names it. A stored version never changes: a catalog that gives
an existing key and version other contents is refused, and a change needs a new version. Moving a group to a new
version, or dropping it, revokes the earlier grant with its time and keeps the row. A member reads the group's tools,
how each is granted and its catalog fields through `GET /api/v1/labs/{lab}/tools` (migration 080).

The group role names (`researcher`, `lead`, and `reviewer`) are membership metadata. Unbound tasks remain owner-only;
the release does not give leads or reviewers access to another principal's tasks. Do not describe those names as group
oversight RBAC.

[Research projects](research_projects.md) add explicit collaboration permissions and recorded human inspection.
These project capabilities do not change the meaning of the group role names.

[Research campaigns](campaigns.md) are durable programs above projects and runs. Each campaign gets a reserved,
non-authenticating principal with a `researcher` membership in one lab. The `campaign-director` process uses a
separate function-only database role, has no Kubernetes or OpenShell credential, and drains running runs when a
campaign stops so their existing receipts and evidence remain complete. Its planner uses the operator-owned model
route, while code and generation-fenced database functions enforce scope, budgets, references, and starts. An operator
can stop every campaign through the migration-owner-only database kill switch documented there.

## Task lifecycle

1. The API verifies Access and resolves an invited principal.
2. Submission writes a durable task and append-only `queued` event before returning HTTP 202.
3. The worker claims work with a time-limited lease and monotonically increasing generation fence.
4. Group and per-principal active quotas are checked during the claim transaction.
5. The worker creates one logical agent from the immutable profile snapshot and runs a bounded Pi or OpenCode session.
6. The controller reconciles agent state and deletes the OpenShell sandbox after the run.
7. The worker exports scoped state and provider-returned provenance, including restricted reasoning and tool I/O, into
   the task's owner-only artifact tree. It copies the logical-agent workspace and hashes every exposed file.
8. The fenced worker commits the terminal state. A stale worker cannot commit after losing its lease.

Closing the laptop, terminal, or event stream does not cancel the task. Cancellation is an explicit API operation. A
worker shutdown stops its child process group; the expired lease is then requeued within the profile's task-attempt
limit. Controller and artifact custody stay on persistent volumes.

Submission and cancellation require `X-Agent-Sandbox-Intent: mutate`. The typed client sets this header, and the
broker rejects a mutation without it. Submission separately requires an `Idempotency-Key` so a network retry does not
create a second task.

Long work still uses bounded provider sessions. A week-long research plan should be represented as several durable
tasks or controller segments with checkpoints, not one week-long KV cache. This keeps retries and context resets
observable.

## Researcher setup

The site operator supplies one broker HTTPS origin. Install the client from the same qualified source commit as the
server:

```bash
cargo install --locked --path . --bin agent-sandbox-remote
agent-sandbox-remote configure --server https://agents.example.invalid
agent-sandbox-remote login
agent-sandbox-remote me
```

`login` opens the Cloudflare Access browser flow. The broker client asks `cloudflared access token` for the active
session when it makes a request and sends it to the edge as `Cf-Access-Token`. Cloudflare injects
`Cf-Access-Jwt-Assertion` at the origin. The client does not print or persist that JWT. A human login uses the identity
provider. This release requires a user email claim and does not accept service-token identities.

Submit and reconnect:

```bash
agent-sandbox-remote submit \
  --group pilot-lab \
  --profile general-research \
  --objective-file /absolute/path/objective.txt \
  --idempotency-key study-step-001

agent-sandbox-remote list
agent-sandbox-remote get <task-id>
agent-sandbox-remote events <task-id> --after 0
agent-sandbox-remote artifacts <task-id>
agent-sandbox-remote download <task-id> <artifact-id> --output /absolute/new/file
```

Downloads never overwrite a file. The API checks the stored size and SHA-256 before streaming; the client checks the
received byte count and SHA-256 again and deletes a partial download after a failed transfer.

A person who leads the commons marks a commons record baseline, or withdraws the mark, and a person who curates or
leads a lab reviews the links its papers propose, each as themselves ([knowledge actions](knowledge_person.md)):

```bash
agent-sandbox-remote knowledge commons --kind dataset
agent-sandbox-remote knowledge baseline <resource-id>
agent-sandbox-remote knowledge links <lab>
agent-sandbox-remote knowledge review <link-id>
```

## Pi and OpenCode tools

The host-agent adapters expose typed `remote_agent_*` tools for identity, submit, list, status, cancellation, events,
artifact listing, and bounded download. They do not expose login or arbitrary broker URLs to the model. The launcher
must opt in and controls the binary and download root:

```bash
export ENRICHEDLAB_REMOTE_BROKER_ENABLED=1
export ENRICHEDLAB_AGENT_REMOTE_BIN="$HOME/.cargo/bin/agent-sandbox-remote"
export ENRICHEDLAB_REMOTE_OUTPUT_ROOT="$HOME/agent-results"
mkdir -p "$ENRICHEDLAB_REMOTE_OUTPUT_ROOT"
chmod 700 "$ENRICHEDLAB_REMOTE_OUTPUT_ROOT"
```

Load Pi for one session:

```bash
pi --extension /absolute/repository/scripts/harness/pi_omp_remote_broker.mjs
```

For OpenCode, create one local file under `~/.config/opencode/tools/remote_broker.ts` (or the matching
`XDG_CONFIG_HOME`) and import the reviewed factory with an absolute path:

```typescript
import { tool } from "@opencode-ai/plugin";
import { createOpenCodeRemoteBrokerTools } from "/absolute/repository/scripts/harness/opencode_remote_broker.ts";

const tools = createOpenCodeRemoteBrokerTools(tool);
export const remote_agent_me = tools.remote_agent_me;
export const remote_agent_submit = tools.remote_agent_submit;
export const remote_agent_list = tools.remote_agent_list;
export const remote_agent_get = tools.remote_agent_get;
export const remote_agent_cancel = tools.remote_agent_cancel;
export const remote_agent_events = tools.remote_agent_events;
export const remote_agent_artifacts = tools.remote_agent_artifacts;
export const remote_agent_download = tools.remote_agent_download;
```

Keep shell, Kubernetes, and direct OpenShell tools denied for a researcher profile. If the opt-in environment is
missing, calls fail before the remote binary starts. Artifact filenames are single safe names and land under
`<output-root>/<task-id>/`; the model cannot choose an arbitrary host path.

## Cluster setup order

The public manifests are templates. They contain a zero image digest, reserved hostnames, generic labels, and no
Secrets. Build a private site overlay and keep identity invitations encrypted.

The optional direct-Sandbox worker and custody-dispatcher profile is documented in [direct broker deployment](broker_direct_deployment.md). It requires a site-owned writer-fence storage capability and exact Kubernetes API egress boundary before use.

1. Qualify the intended Kubernetes node, CNI, OpenShell gateway, sandbox CRDs, and microVM RuntimeClass.
2. Label the one controller-storage node and the exact database, edge, JWKS relay, and OpenShell gateway workloads used
   by the NetworkPolicies.
3. Publish and scan `images/controller.Containerfile`; replace every zero digest with that immutable digest. Use the
   example as a workload overlay and keep foundation in its own Flux child.
4. Reconcile `deploy/kubernetes/broker/foundation` through a Flux child and wait for it to become Ready.
5. Provision distinct migration-owner, API, worker, and campaign-director database roles. Use TLS with hostname
   verification for a remote database.
6. Reconcile the versioned migration Job, apply the fixed runtime grants, then reconcile the encrypted catalog seed
   Job. Wait for each predecessor before activating its successor.
7. Stage the controller config, route catalog, API DSN, worker DSN, OpenShell client bundle, Access values, and broker
   catalog in the private overlay.
8. Reconcile the API, worker, and campaign-director workloads. Expose only the API ClusterIP through the Cloudflare
   tunnel.
9. Run browser login, denied-account, wrong-audience, cross-principal, quota, cancellation, disconnect, artifact hash,
   worker restart, OpenShell release, and backup/restore checks.

The migration Secret is mounted only by one-shot database jobs. The API, worker, and campaign director have separate
function-only roles and no table privileges. The director gets only its campaign functions and the schema readiness
probe. The worker ServiceAccount has no Kubernetes token; it reaches the OpenShell gateway with the site-issued client
bundle. Researchers and the director never reach that gateway.

Use Flux `dependsOn` relationships for foundation, database migration, catalog seed, and workloads. Publish the image
and source before activating a Flux-owned successor. Never keep the old and new same-name workload active together.

## Cloudflare Access checks

Create one self-hosted Access application for the broker origin and restrict it to the intended identity-provider
group and MFA policy. Route that hostname through a Cloudflare Tunnel to the broker ClusterIP. Do not create a
NodePort, public LoadBalancer, or public OpenShell gateway.

Set the broker to the exact team-domain issuer and application audience. The example uses an internal HTTPS JWKS relay
because the default NetworkPolicy has no general internet egress. Mount the relay's CA bundle from a ConfigMap and set
`access_jwks_ca_file`; the API Pod stages the projected entry as a private regular file and keeps hostname verification
enabled. A site may instead route the API Pod to the Access cert endpoint through an explicit egress path and omit the
relay and CA overrides. Either path needs a qualification test.

Test at least these cases before inviting users:

- intended account with the required MFA succeeds;
- intended account without satisfying the Access policy is denied at the edge;
- uninvited verified email is denied by the broker;
- a JWT for another Access application is denied;
- one principal cannot read, cancel, or download another principal's task;
- the raw ClusterIP is not reachable from a researcher device;
- OpenShell, Kubernetes, PostgreSQL, and persistent-volume paths have no researcher route.

Cloudflare documents both [JWT validation](https://developers.cloudflare.com/cloudflare-one/access-controls/applications/http-apps/authorization-cookie/validating-json/)
and the [`cloudflared` CLI login flow](https://developers.cloudflare.com/cloudflare-one/tutorials/cli/). Follow the
current provider docs during each site qualification.

## Scaling behavior

The first cluster release uses one controller-worker leader with `max_parallel` from 1 through 16. Five concurrent
tasks is a supported configuration when node and inference capacity pass load tests. The worker starts one OpenShell
sandbox per claimed task, and each normal task run releases that sandbox after state reconciliation. Idle users consume
database and retained-storage records, not sandbox compute.

The API is one replica in the reference manifests. PostgreSQL can serve several API replicas, but artifact access must
first move to qualified RWX or object storage and the identity, SSE, and load tests must pass. Do not increase worker
replicas over one controller state tree; the advisory leader lock rejects a second leader.

Kueue and `SandboxWarmPool` are not connected to this broker. Kueue can be added as a physical admission layer after
an external-workload adapter exists, while broker quotas continue to enforce research-group policy. A warm pool can
reduce start time only after OpenShell allocations can bind to those claims. Current tasks use on-demand OpenShell
creation.

NVIDIA labels its Kubernetes setup path as experimental. A qualified site deployment records evidence for that exact
Kubernetes version, OpenShell release, CRDs, CNI, RuntimeClass handler, storage class, and node pool. It is not a generic
production-Kubernetes claim. See NVIDIA's [Kubernetes setup](https://docs.nvidia.com/openshell/kubernetes/setup),
[access control](https://docs.nvidia.com/openshell/kubernetes/access-control), and
[security guidance](https://docs.nvidia.com/openshell/latest/security/best-practices).

## Retention and current limits

The group catalog records `retention_days`, but this release does not delete artifacts automatically. Site backup and
retention jobs need an explicit legal and research-data policy before activation. Until then, deleting a sandbox frees
compute while PostgreSQL rows, controller state, state exports, provenance exports, and workspace artifacts remain on
persistent storage.

The reference artifact store is one RWO PVC, and the controller projection is embedded SQLite on one RWO PVC. This is
appropriate for a small site controller and a few concurrent agents on one storage node. It is not active-active. A
larger deployment needs an object-store custody adapter, a shared prompt-state projection, tested backup and restore,
and explicit per-tenant storage quotas.

The separate [research workspace](../clients/workspace/README.md) uses these broker APIs for browser task submission,
event history and verified downloads. Its synthetic loopback preview is available in source; a site must configure
authenticated same-origin hosting before exposing it to researchers. Pi, OpenCode and the remote CLI use the same
broker authority. The existing Svelte dashboard remains a loopback operator surface.
