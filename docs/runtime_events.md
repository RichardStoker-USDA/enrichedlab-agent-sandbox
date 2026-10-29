# Direct runtime event custody

The trusted direct-execution driver records Sandbox creation, the first observed Pod, terminal process status, and
recovery requests in the existing PostgreSQL `broker_task_events` ledger. Migration 003 adds a worker-only append
function. Apply migrations through 004, runtime grants, then the new broker/worker image. Migration 004 gives API
and worker roles a bounded schema-readiness function so they need no direct table access. Ordinary readiness requires
migration 002; direct worker readiness also requires migration 003 and permission for the current identity to call
the runtime-event function, and since migration 019 the schema and grant for recording each attempt's execution.
Check these before claiming direct work. Older binaries that directly read the migration
table are not made compatible by these grants; replace them after the migration and grant step.

Each event shares the task's ordered sequence with its lifecycle events. A current, unexpired worker lease is
required. Exact replay within that lease generation succeeds without adding a second event; changed replay, a
different Sandbox or Pod, unknown fields, and unrestricted text are rejected. The API role cannot append events or
read the underlying ledger directly. Its existing owner-scoped event list and SSE stream expose sequence, kind,
state, code and timestamp. Internal container identities remain in ledger metadata.

The records contain machine identifiers, attempt and lease numbers, process exit status, and a fixed recovery reason.
They contain no prompts, reasoning, logs, credentials, research rows, or artifact bytes. Actual outputs are retained
through the artifact custodian before cleanup. A process exit code is execution evidence, not scientific validation.

This is the runtime lifecycle part of telemetry. It does not implement tool-step capture, intermediate data exports,
researcher artifact views, long-running team memory, an OpenTelemetry collector, or a live Kubernetes worker. Those
must be connected through their existing contracts. Recovery preserves the exact runtime handle; it is not proof
that a running container has stopped or that its files can safely be deleted.

Run `scripts/qualify_remote_broker.sh runtime-events` for the focused disposable PostgreSQL check. It verifies the
closed schema, owner-scoped reads, role separation, stale lease rejection, event ordering and idempotent replay.
Use `scripts/qualify_remote_broker.sh direct-readiness` for the real restricted-role startup path, missing migration
and revoked-event-grant checks. This does not rerun the independent event replay suite.
