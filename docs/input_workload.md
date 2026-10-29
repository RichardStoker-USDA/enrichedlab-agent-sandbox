# Direct Sandbox input-copy workload

`InputWorkloadRenderer` returns a one-shot restricted Kubernetes Job, input PVC, artifact PVC, and NetworkPolicy for an already-authorized `input-staging/v1` request. It is a source-only renderer. It does not create a Job, PVC, Secret, network policy, or ServiceAccount.

The trusted caller supplies an authenticated broker task, its reviewed Sandbox binding, a server-owned pre-seal `InputCopyAuthorization`, an operator-owned renderer configuration, and an `AuthorizedInputCopy`. The latter binds an approved same-namespace source PVC named `input-<source-run>` or `artifacts-<source-run>`, plus a private Secret name. It keeps the `CopySealRequest` and its declared file paths out of rendered metadata. The renderer checks task, attempt, lease, principal hash, profile, namespace, run, input digest, destination PVC, fixed roots, sha256-prefixed declared-file digests and bounds, image digest, resource limits, runtime class, and request-byte cap before returning objects.

A trusted publisher stores `InputCopyWorkload::request_bytes()` as `request.json` in the supplied immutable Secret. The renderer returns `input-<run>` and `artifacts-<run>` PVC specifications with operator-configured storage classes and size budgets. `custodyPvcAccessMode` is a closed operator configuration for both newly created PVCs: omitted configuration preserves `readWriteOnce`; a site-qualified isolated custody class may select `readWriteOncePod`. With Kubernetes and CSI support, `readWriteOncePod` can enforce single-Pod access. Rendering this request alone neither proves that enforcement nor protects against source/output replacement; `readWriteOnce` permits multiple same-node Pods. The setting never applies to shared control-plane or notice PVCs and accepts no task or model override. The input PVC labels it `immutable-input`, `staging`, and `checksummed-copy`; the artifact PVC labels it `durable-artifacts`. Both carry the v3 task, tenant, principal, and run ownership labels. The input PVC has no returned manifest or attestation digest. The Job mounts that Secret read-only at `/request`, an approved `artifacts-<source-run>` PVC read-only at `/source` or the `payload` subpath of an approved `input-<source-run>` PVC, the destination `input-<run>` PVC read-write at `/destination`, and a bounded memory `emptyDir` at `/custody`. It runs only `/usr/local/bin/agent-sandbox-custodian --request /request/request.json` as UID/GID 65532 under restricted Pod Security. It has no service-account token, host path, destination artifact PVC, API credentials, ingress, or egress. An approved artifact PVC may appear only as the read-only source mount. The paired NetworkPolicy denies all ingress and egress.

The Job retains the direct-run contract labels plus only the known input digest, `checksummed-copy` mode, and a hash of the private request bytes. It deliberately omits the staging, manifest, and attestation annotations until the helper result has been accepted. The helper writes a manifest and typed `SealAttestation` to its private `/custody` memory volume and emits only that typed attestation on its exact observed Pod log stream. Rendering or Job completion does not seal a PVC. The dispatcher must fetch the exact helper Pod result, validate and persist the attestation, detach the writer, and then apply the input-PVC sealing contract before admitting compute. It must also authorize the source PVC snapshot and perform writer-detachment checks before rendering. Those duties stay outside this renderer.

The private Secret path is intentionally bounded below the Kubernetes Secret payload limit. The publisher must enforce aggregate encoded Secret size before creation and must not put request bytes in labels, annotations, logs, or receipts.

Both PVCs carry `sandbox.enrichedlab.org/custody-owner: durable-task-custody`,
the matching `retention-owner` annotation, and `retention-until` from the
authorization's approved `custody_retention_until`. The renderer validates RFC
3339 syntax and preserves the timestamp exactly. The input custodian checks
freshness before publication. Neither volume has a task owner reference; task
cleanup must not garbage-collect retained inputs or results.

Input finalization requires the written input PVC to remain `Bound`. The output
PVC may be `Pending` or `Bound` before compute admission: a
`WaitForFirstConsumer` storage class binds it when the Sandbox Pod is scheduled.
Both paths still require the original PVC UID, matching run and role, and no
deletion request. A pending input or lost volume stops finalization before
writer cleanup or sealing. Output collection still requires the completed
compute and the bound output volume.

After foreground deletion, cleanup confirmation fetches the observed copy Pod by
name and checks its UID. A missing Pod confirms detachment; a different UID or
failed API read requires recovery. Pod UID field selectors are unsupported by
Kubernetes and are not used for this check.

## Site metadata

The optional operator-owned `site` field contains `labels` and `imagePullSecrets`. Labels
are attached to the copy Job and Pod and its isolation selector. They cannot replace
`app.kubernetes.io`, `kubernetes.io`, or sandbox identity labels. Registry entries are
Secret names in the same namespace; credentials never enter configuration or receipts.
Missing `site` preserves the existing empty metadata behavior. The site supplies the
corresponding enrollment and retained-volume policy before starting a helper.
