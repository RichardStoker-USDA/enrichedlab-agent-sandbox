# Optional direct Sandbox broker deployment profile

`deploy/kubernetes/broker/direct` is a source-only, site-overlay profile for a
direct-Sandbox worker and its one-shot custody dispatcher. It reuses the
foundation resources and is composed with the existing API Service separately;
it does not replace the legacy OpenShell workload templates.

The profile uses the existing API and foundation resources, then adds two
namespace-scoped service accounts:

- `agent-broker-direct-worker` can create, get, and UID-precondition delete a
  direct `agents.x-k8s.io/v1beta1` Sandbox and read only its named Pod and PVC.
- `agent-broker-custodian` can create the fixed input/output/export helper
  objects, observe fixed Pods and logs, patch the sealed input PVC, delete an
  exact completed helper Job, and get a Sandbox identity. It cannot create,
  update, or delete a Sandbox, exec into a Pod, list Secrets/PVCs, or use a
  ClusterRole.

Both workloads run as UID/GID `65532`, disable automatic service-account token
mounting, and use an explicit rotating projected token plus `kube-root-ca.crt`.
The projected private configuration records are copied by non-root init
containers into `0700` `emptyDir` directories as `0600` regular files before a
descriptor-safe loader reads them.

## Required site inputs

The profile intentionally names no real image, namespace, identity, database,
Kubernetes API, storage, binding, or secret value. Before an overlay can be
used, its operator must provide all of the following:

1. A pinned broker image through Kustomize `images`, an existing broker API and
   Service from `deploy/kubernetes/broker/workloads`, an `agent-broker-site`
   ConfigMap, API database Secret, worker database Secret, and separate
   custodian database Secret.
2. A private `agent-broker-direct-dispatch` Secret key `direct-dispatch.json`
   that validates as `sandbox.enrichedlab.org/direct-dispatch/v1`. Its
   `kubernetes.caPath` and `kubernetes.tokenPath` must be the projected paths
   `/var/run/secrets/kubernetes.io/serviceaccount/ca.crt` and `token`; its
   `admission.root`, binding catalog, terminal export configuration, and input
   preparation root must match the mounted directories below. A prepared HTTPS
   terminal export uses `/var/lib/enrichedlab/direct/output-notices` as its
   `TerminalPreparationConfig.root`; input preparation uses
   `/var/lib/enrichedlab/direct/input-notices`.
3. A private `agent-broker-custody-config` Secret with exactly
   `dispatcher.json`, `input-custodian.json`, and `output-custodian.json`.
   They must validate respectively as existing `custody-dispatch/v1`,
   `input-custodian/v1`, and `output-custodian/v1` schemas. Their Kubernetes
   token/CA references use the same projected paths and their child program
   paths remain the packaged fixed binaries.
4. Precreated private subdirectories on `agent-broker-direct-control`:
   `notices/input`, `notices/output`, `plans/input`, `plans/output`,
   `admissions`, `terminal-records`, `clients`, `custody/input`,
   `custody/output`, and `dispatcher-state`. The worker writes input notices
   and terminal-preparation output notices, and reads admissions, terminal
   records, and clients. The dispatcher reads notices and plans while writing
   admission, terminal, client, custody, and state paths. The separate operator
   plan publisher is the only intended writer for plan paths. For a
   kubelet-managed volume, initialize the control PVC root to group `65532`,
   mode `2770` before creating the UID/GID `65532`, mode `0700` private
   descendants. This satisfies `OnRootMismatch` at the volume root and avoids a
   recursive fsGroup permission change widening those descriptor-safe roots.
   CSI-delegated group handling remains a site check.
5. A tenant-local writer-fence-capable storage implementation selected in place
   of `REPLACE_WITH_WRITER_FENCE_STORAGE_CLASS`, plus an admission or storage
   policy that prevents source/output writers and same-name replacements for
   the full copy/observe/seal/export interval. The core input workload can
   select `custodyPvcAccessMode: readWriteOncePod`, rendered as
   `ReadWriteOncePod`, for its new per-run custody PVCs when the site qualifies
   that access mode. With Kubernetes and CSI support it can enforce single-Pod
   access, but rendering this request alone neither proves that enforcement nor
   protects against source/output replacement; `ReadWriteOnce` permits multiple
   same-node Pods. This profile does not install CSI, a lock, or an admission policy.
6. A site overlay that replaces the reserved `192.0.2.0/32` egress placeholder
   with the exact Kubernetes API boundary. The default-deny foundation policy
   otherwise keeps the profile unable to contact the API.
7. If the input custodian uses the canonical site renderer, a separate private
   site overlay for the custody dispatcher. It must stage the selected tenant
   records at `/opt/enrichedlab/agent-sandbox/site/tenants` and accepted or
   candidate profiles at `/opt/enrichedlab/agent-sandbox/profiles/{accepted,candidates}`
   as UID 65532, mode 0600 regular files. The generic profile deliberately does
   not declare a tenant, profile, or source volume for those paths. A trusted
   materializer and the fixed renderer validate the selected paths; the overlay
   must not grant worker access to them.

The direct worker uses `/bin/false` for legacy controller/OpenShell executables
in this profile. Every admitted task therefore needs a matching direct binding;
an unmatched task reaches the existing fallback and fails rather than silently
receiving an unconfigured legacy runtime.

The profile has no cross-namespace PVC mounts. Per-run input and artifact PVCs
are created and observed only by the trusted custodian in the bound namespace.
It does not authorize plans, publish them, stage data, issue export identity,
prove writer exclusion, or make any profile live.
