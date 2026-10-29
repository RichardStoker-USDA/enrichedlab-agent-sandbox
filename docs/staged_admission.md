# Trusted Sandbox admission

`broker::admission::load` reads a server-owned handoff for one claimed task attempt.
It returns the canonical Sandbox, authorization and custody attestation. It does
not start a worker, provision storage, seal a PVC, or grant Kubernetes access.

The producer runs the checked copy-and-seal helper, renders the reviewed run with
that exact attestation, then atomically publishes a private JSON file using the
`sandbox.enrichedlab.org/broker-staged-admission/v1` schema. The fixture in
`tests/fixtures/admission/staged_admission.json` is an executable contract example
with synthetic data. Its companion provenance file records the producing source
revisions and file digests.

The caller supplies a configured absolute directory, expected writer UID, one
server-selected filename, the current claimed task, and a binding resolved from
the server catalog. Neither a prompt nor a client request chooses these paths,
the namespace, RuntimeClass, executable, or PVCs. Use a dedicated service identity,
a directory mode of 0700 and record mode of 0600, with no extra ACL grants. Publish
with a temporary file and an atomic rename; do not rewrite a published record.

Admission checks:

- Exact task ID, attempt, lease generation, principal, group and profile.
- Copy-and-seal schema, run, tenant, namespace, principal, input PVC and digest
  bindings, including both the self-digest and complete staging document digest.
- The existing direct Sandbox adapter's workload and authorization checks.
- Descriptor-based traversal from the filesystem root, with no symlink component;
  private root and file ownership/modes; a regular file with no hard links.
- A 256 KiB file and read bound. FIFOs use a nonblocking open before rejection.

The checksums detect changed content. They are not a substitute for authenticating
the producer: trust comes from the service identity and private admission directory.
Filesystem admission does not prove that a staging writer has detached from a PVC.
The custody service must finish and verify that step before publishing the record
and marking its input PVC sealed. Before creating or deleting compute, the runtime
must check the current lease again and enforce the live policy and PVC bindings.

The optional direct broker worker route uses this admission loader. Runtime activation still needs
the trusted staging dispatcher, detach-and-seal lifecycle, artifact export and
retention service, and the site's isolated controller deployment. Keep those
components separate from the researcher API and from untrusted agent processes.

The concrete filesystem export boundary is described in
[Artifact retention](artifact_retention.md). Its trusted mount and selection still
come from the site's dispatcher.

The direct adapter separates Pod observation from readiness. The dispatcher uses
`observe_pod` to capture the first exact Pod UID, including a short task that has
already exited, and retains that observation for `observe_terminal`. Completion
requires the same Sandbox UID/generation and Pod identity, the current `Finished`
condition with its matching outcome, and exit code/time from the terminated single
runner. This follows the
[v1.0.1 controller's terminal condition](https://github.com/kubernetes-sigs/agent-sandbox/blob/v1.0.1/controllers/sandbox_controller.go#L632).

A private `RuntimeCompletion` carries that observation into `finish_observed`,
which derives the broker outcome and uses the existing retention-before-cleanup
path. A replaced Pod, missing termination evidence or conflicting success/exit code
cannot authorize cleanup. A stale controller condition remains pending. Process
success does not validate scientific results, grant access to files, or attest that
a PVC writer has detached. The optional direct worker route requires the trusted dispatcher configuration,
permissions and input/output handoffs described here before a site enables it.

## Private direct-dispatch catalog

`broker::dispatch::DirectDispatchCatalog` reads one private, server-owned JSON
catalog. It carries a versioned HTTPS Kubernetes origin, CA and token path
references, a descriptor-checked admission root and writer UID, and exact
principal/group/profile bindings. The catalog rejects unknown fields, unsafe
origins or paths, duplicate selectors, symlinks, non-private files, and files
larger than 256 KiB. It contains path references only, never credential values.

The optional `admission.waitSeconds` field enables asynchronous staging for up to
3600 seconds. An absent field or zero preserves immediate admission lookup. For
an enabled route, the worker uses `prepare_for_execution` before either direct
compute or legacy controller side effects. It waits only when the exact record
is absent under a valid private root. Corrupt records, changed bindings, unsafe
files and missing or unsafe roots fail immediately and never select a fallback.

The worker checks cancellation while waiting, renews its existing claimed lease
at one-third of its duration, and checks authority again before returning the
admission. Shutdown, lease loss and the separate staging deadline end the wait.
Only task IDs, attempt/generation and state classes appear in pending/ready
telemetry. The eventual compute deadline starts in the direct execution driver.
This wait does not provision input or grant a retry authority over prior storage;
the site's trusted custodian and recovery lifecycle still own those actions.

An optional `preparation` object adds the worker-to-custodian handoff:

```json
"preparation": {"root": "/var/lib/broker/preparation", "writerUid": 65532}
```

It requires a nonzero admission wait and a dedicated private directory separate
from the admission root. On the first confirmed absence of the exact admission,
the worker checks its lease/cancellation and atomically publishes one private
`<task-uuid>-<attempt>-<lease-generation>.json` notice. It then keeps waiting on the
existing admission path. Already admitted tasks and unmatched routes emit no notice.
An existing identical notice is durable replay; a changed record, unsafe directory
or failed publication stops the task without selecting a fallback.

The notice contains the current task identity, attempt, lease generation, worker,
maximum run time and the exact server binding. It contains no research objective,
model route, command, file list, credential or research bytes. The custodian uses
`InputPreparationRequest::load` to check private descriptors, bounded closed JSON,
filename and binding. It must then recheck the database lease and resolve approved
source data and workload configuration from site authority. A stale notice never
authorizes a retry or a new task. This durable handoff is not a second task ledger;
the trusted custodian service still owns preparation and recovery.

Startup configuration supplies the catalog writer UID explicitly. It is not a
catalog field. The catalog reader uses the same descriptor-held no-follow path
walk as admission records, so the configured parent and catalog file must be
owned by that UID and private. The example is only JSON shape; deployment owns
the matching startup UID and calls `DirectDispatchCatalog::load(path, uid)`.

For one claimed task, `prepare` deterministically derives
`<task-uuid>-<attempt>-<lease-generation>.json`. An unmatched task returns no
direct result so a later worker can preserve its existing runtime decision. A
matched task must load and validate that exact admission record; absence or any
binding mismatch is an error and cannot fall back. This still does not start a
Sandbox, attach a PVC, call Kubernetes, or supply local model/tool services.
