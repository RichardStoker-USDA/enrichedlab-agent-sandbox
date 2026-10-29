# Same-name custody plan publisher

`agent-sandbox-plan-publisher` copies one already approved input or output plan
from a private operator root to the same-name private custodian plan root. It
accepts a closed source filename, expected `sha256:<64 lowercase hex>` digest,
task UUID, attempt, lease generation, and plan kind. The destination name is
fixed as `<task-id>-<attempt>-<lease-generation>.json`.

Both roots are held-descriptor private directories. The source must be one
regular, single-link file owned by `--source-writer-uid`; the destination root
must be mode `0700` and owned by `--plan-writer-uid`. Publication writes a
mode-`0600`, single-link temporary record, fsyncs it, and atomically publishes
without replacement. An already identical record is safe to reuse; a different
same-name record is rejected.

The publisher checks only the plan schema and task UUID/attempt/lease envelope:

- `sandbox.enrichedlab.org/input-preparation-plan/v2`
- `sandbox.enrichedlab.org/output-preparation-plan/v1`

Input v2 requires approved `custodyRetentionUntil` in the full custodian plan.
Upgrade the input author, publisher and custodian together; the publisher rejects
input v1. Output plan compatibility is unchanged.

It does not authorize the plan, validate its binding, query a task ledger,
create Kubernetes objects, or start a custodian. Input and output custodians
must still independently read their notices, check the current task lease and
binding, validate their complete private plan, and enforce source/output writer
fencing before any side effect.

The receipt contains only plan kind, task scope, destination filename, digest,
and byte count. It never includes plan bytes, roots, credentials, or source
errors.
