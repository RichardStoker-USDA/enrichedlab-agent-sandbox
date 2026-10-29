# Input finalization

`broker::input_finalization::InputFinalizer` connects an already published
input-copy Job to the existing staged-admission publisher. It accepts only a
`PublishedInputCopy`, an `ObservedInputSeal`, a server-owned binding, and the
trusted canonical Sandbox plus authorization. It reuses the canonical renderer
contract before deleting the completed copy Job or changing a PVC.

The finalizer first reads the exact input and artifact PVC UIDs, constructs an
expected sealed input view, and validates that view with
`PreparedAdmissionPublication`. It stores the original one-line custodian
receipt and a metadata-only Job/Pod/PVC observation in private custody before
cleanup. Its private journal records ordered intents and acknowledgements.

It deletes only the completed Job with its exact UID and foreground propagation.
It then polls within the existing lease-bound deadline while only that exact
Job UID remains terminating. It proceeds only after both the Job name and the
observed original Pod UID are absent; a same-name replacement, malformed UID,
ambiguity, timeout, cancellation, or failed operation is left for recovery and
never adopted. The only promotion mutation is
an opaque UID/resourceVersion CAS that sets `seal-state=sealed` and the four
server-owned input digest annotations. Fresh PVC reads must retain both created
volume identities before `PreparedAdmissionPublication` writes the admission.

Every destructive or publish boundary is fenced with the task lease and
cancellation check. This source does not provide a Kubernetes adapter, site
writer exclusion, dispatcher/worker wiring, retry executor, or compute creation.
