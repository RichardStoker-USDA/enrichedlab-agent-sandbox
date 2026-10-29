# Broker runtime adapter

The broker's candidate Kubernetes adapter consumes a reviewed
`sandbox.enrichedlab.org/run/v3` renderer result and creates one direct
`agents.x-k8s.io/v1beta1` `Sandbox`. It does not create `SandboxClaim`,
`SandboxTemplate`, `SandboxWarmPool`, Jobs, Pods, or volume claim templates.

The authenticated server-owned binding must match the broker task principal,
group, profile, lab, namespace, runtime, run ID, executable digest, input
digest, staging digest, manifest digest, and input attestation digest. The
adapter rejects a renderer object that varies any of those fields, creates
compute-owned PVCs, or references anything except `input-<run>` read-only and
`artifacts-<run>` writable.

Before creation it reads the two custody PVC metadata objects. The input must
be the exact run's `immutable-input` PVC, have `seal-state=sealed`, and name an
approved materialization mode. The artifact target must be the exact run's
`durable-artifacts` PVC. Neither may be owned by a Sandbox or Pod. This needs
one namespaced RBAC addition for the broker ServiceAccount: `get` on
`persistentvolumeclaims`; it does not need PVC create, patch, delete, list, or
watch. Cluster ownership remains with the renderer and custodian lanes.

Readiness reads the direct Sandbox and its exact controller-owned Pod UID and
generation. Cleanup is lease-fenced, retains artifacts first, then deletes only
the exact Sandbox UID. It never deletes either custody PVC. The next dispatch
integration point is the worker after task admission and input staging have
produced the authenticated binding and canonical renderer output; the worker's
existing task/event/artifact APIs remain unchanged.
