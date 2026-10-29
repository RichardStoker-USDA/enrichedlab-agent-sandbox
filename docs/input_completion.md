# Observed input copy completion

`broker::input_completion::observe_input_seal` accepts only a completed trusted
copy-and-seal Job. Its expected name, UID, namespace, run, principal hash,
runtime, helper image/command and PVC mounts come from trusted rendering and
create evidence. It re-reads the Job and controller-owned Pod, requiring the
standard `Complete=True` Job condition with no true `Failed` or
`FailureTarget`, a single succeeded helper with exit code zero, and no
unaccounted init, ephemeral, running, or restarted containers. `batch/v1`
`JobCondition` has no per-condition observed generation field; the observer
instead binds the exact Job UID, a positive Job metadata generation, terminal
counts, and the controller-owned Pod UID.

The fixed renderer supplies four exact volume roles: the read-only source PVC,
the writable destination input PVC, memory-backed custody directory, and a
read-only projected request Secret. The observer verifies their names, volume
classes, PVC or Secret references, mounts, and source subpath. Input-source
copies mount only `payload`; artifact-source copies mount their source root. A
pre-copy authorization does not prove that an input PVC is sealed.

The helper stdout must be one bounded JSON `SealAttestation` line. The observer
checks the original request fields, sorted declared-file manifest digest, sealed
PVC name, attestation self-digest, and a positive sealed timestamp. It retains
the original bytes privately for custody persistence. Public metadata and the
borrowed typed attestation contain no copied source bytes.

`ObservedInputSeal::metadata`, `job_uid`, `pod_uid`, `attestation`, and
`original_receipt` expose only accepted provenance and receipt bytes to the
trusted dispatcher. It stores the original receipt before deriving post-seal
admission and input-PVC annotations.

This observer does not seal or patch PVCs, delete Jobs or Pods, update leases,
or prove writer detachment. A later trusted dispatcher must still verify the
live sealed PVC metadata and UID, the active lease, retention, and detachment
before creating compute.

`InputKubernetesClient` is the read-only site adapter for this observer. Its
operator-owned service-account constructor binds one HTTPS API origin,
namespace, projected rotating token path, and configured CA. It reads only the
exact batch/v1 Job, a single `batch.kubernetes.io/job-name` labelled Pod with
`limit=2` and no pagination, and the current `custodian` stdout using bounded
Kubernetes log parameters. It rejects mismatched namespaces, resource names, container names,
redirects, ambiguous Pod lists, malformed bodies, and oversized responses. It
does not create, patch, delete, grant, or deploy Kubernetes resources. The
trusted dispatcher remains responsible for wiring the adapter and supplying the
site's read-only service identity.

`InputKubernetesClient` also supplies the input publisher's create-only transport
for a NetworkPolicy, PVCs, immutable Secret, and fixed copy Job. It binds one
operator-owned API origin and namespace, re-reads the rotating projected token
for each request, allows only typed resource endpoints and `201 Created`, and
stops on conflicts for recovery. Its source observation reads one exact PVC and
bounded pages of every Pod in the namespace. It treats any active writable PVC
mount or device attachment in normal, init, or ephemeral containers as a
writer; the copy Job's read-only source mount does not count. Malformed,
unknown, or incomplete writer observations fail closed. This observation is not
a lock: the site admission policy must prevent a new source writer or PVC
replacement throughout the copy and publication interval.

For finalization, the same client reads only the named input and artifact PVCs,
deletes the completed copy Job with its exact UID and foreground propagation,
and then observes the exact Job name and original Pod UID. A replacement or a
remaining object is returned to the finalizer for recovery. The only PVC write
is a UID and resourceVersion-tested JSON Patch with the staged seal label and
the four accepted digest annotations. It accepts no force delete, grace-period
override, retry, or object adoption. The site still owns the service-account
grant, source-writer exclusion, and the dispatcher that invokes finalization.
