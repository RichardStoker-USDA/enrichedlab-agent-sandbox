# Sealed input admission publication

The trusted site custodian publishes admission only from an accepted input-copy completion. It supplies an
`ObservedInputSeal`, the broker task and lab binding, the canonical workload and authorization, the two volume UIDs
received at creation, and fresh observed PVC metadata. It must finish its storage seal and writer-detachment checks
before this call. Neither a model nor an ordinary researcher supplies these control records.

`PreparedAdmissionPublication::prepare` adds both exact PVC UIDs to the canonical Sandbox metadata and validates the
observed input/output volumes against them. A name reused by a different PVC, another namespace, a deleting volume or
changed seal digests is rejected. The complete record then passes the same closed admission validator the worker uses.
The existing input manifest, attestation and executable checks remain authoritative.

Publication retains the original helper seal bytes and the accepted Job/Pod/PVC identity receipt in a separate
private custody directory before the task's admission appears. All locations come from operator configuration; the
filename is derived from task, attempt and lease generation. Held no-follow descriptors, mode-0700 roots, mode-0600
regular files, fsync and atomic no-replace publication preserve that handoff. Only byte-identical existing records are
reused. Changed, linked, unsafe or incomplete existing records are not overwritten.

The publisher checks cancellation and the broker's existing lease before staging, before publishing admission and
before returning. A stopped task may leave private evidence or its old generation's admission behind. That is retained
custody, not permission to start compute: the direct runtime still checks its lease and fresh volumes before creation.
A later attempt uses a different admission filename and must first resolve the old attempt's runtime/storage custody.

The runtime accepts legacy operator-staged records without UID annotations for the existing route. New records from
this publisher always carry both UID annotations and require fresh matching volume IDs and input seal digests. An
incomplete pair fails. The site migration must retire the legacy path when every admitted request uses this publisher.
This source does not change a live PVC, policy, worker, controller or user-facing application.
