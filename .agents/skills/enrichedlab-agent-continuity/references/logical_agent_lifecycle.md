# Logical-agent lifecycle

## Identity layers

| Layer | Lifetime | Authority |
|---|---|---|
| logical agent ID and alias | until retirement | controller or shared task broker |
| scoped state run | until retention expiry | schema-validated state store |
| workspace and artifact references | until retention expiry | controller and artifact custody store |
| OpenShell sandbox | one allocation generation | OpenShell and selected compute driver |
| provider session and KV cache | one attempt | inference server and harness |

Never use the sandbox name, Pod UID, provider session, or KV cache key as the stable agent identity.

## State transitions

```text
registering -> idle -> active -> idle -> active
                 |        |        |
                 +--------+--------+-> retired
```

- `create` writes the record, seeds profile collections, and materializes the first view.
- `start` holds the per-agent lock, restores the view, reserves a route slot through the common allocator, and creates
  a new runtime generation. The slot remains reserved while the slow runtime create proceeds without the global lock.
- `run` uses a fresh Pi or OpenCode provider session. It records returned reasoning and output as restricted
  provenance, keeps metadata in prompt-independent state events, reconciles model state proposals, and checkpoints.
- `release` pulls a sync workspace when needed, reconciles the outbox, checkpoints, captures runtime evidence, deletes
  the exact sandbox, and clears the active runtime reference. It keeps state and files.
- `retire` performs release, seals the state run, and blocks another allocation.

Create failures can leave a `registering` record. A later `start` reruns the idempotent state seed and finishes
registration. An `active` record with an absent live sandbox is reconciled only after the gateway confirms absence. A
live sandbox with a missing controller record fails closed.

## Profiles

- `cluster-operator` requires `--domain cluster-ops` and exposes execution, cluster-ops, coordination, and bounded
  knowledge.
- `scientific-worker` requires `--domain scientific` and exposes execution, scientific references, and bounded
  knowledge.
- `research-lead` requires `--domain scientific` and adds lead coordination state.
- `software-engineer` requires `--domain software-engineering` and exposes execution, coordination, and bounded
  working knowledge.

Every built-in continuity collection is principal-scoped to the logical agent and owner. State profiles control what
enters the prompt. Provenance, restricted reasoning, artifacts, and full event history remain outside it.

## Retry and cleanup

One attempt is the default. Each retry is a new provider session and provenance segment. Backoff is bounded at 30
seconds and eight attempts. This is process retry, not safe replay of external effects. The calling broker must use
idempotency and fenced ownership for side effects.

One provider session has a 3600-second deadline by default. `--attempt-timeout-seconds` accepts 30 through 86400. A
deadline expiry is recorded as `attempt-timeout`, checkpointed, and eligible for retry only when `--max-attempts`
allows it.

Unless `--keep-runtime` is set, the controller attempts release whether execution passes or returns an internal error.
If state pull or reconciliation fails, it keeps the runtime rather than deleting the only remote copy. Inspect and
recover that runtime before another allocation.
