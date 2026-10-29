---
name: enrichedlab-agent-state
description: Configure, operate, inspect, checkpoint, verify, and export scoped EnrichedLab agent state. Use when an agent needs durable working state without replaying a transcript, different state visibility by agent or domain, context-pressure resets, structured lessons and decisions, scientific provenance, failed-attempt custody, or a reviewable run package.
license: MIT
---

# EnrichedLab agent state

State is a host-controlled input to an agent, not a transcript and not an authorization record. Read
[references/state_contract.md](references/state_contract.md) before changing a schema, profile, audience, authority, or
retention rule. For scientific work, also read
[references/scientific_provenance.md](references/scientific_provenance.md).

## Operate a run

For a named agent that must survive runtime deletion, use `../enrichedlab-agent-continuity/SKILL.md`; it owns lifecycle
and calls this state contract. Use the commands below for direct state administration and custom harness integration.

1. Choose one domain and profile. Use `scientific-worker` for an independent research worker. Do not reuse one run ID
   across subjects.
2. Seed state from operator or machine facts. Treat model content as proposed state until the controller validates an
   explicit mutation. For `research launch`, rely on the controller's Pi or OpenCode stream and file observer for
   execution facts; do not ask the model to report its own tool success or file hashes.
3. Give the agent only `.agent-state/view.json`. Do not add the database, event payloads, artifact store, other
   audiences, or a full transcript to its prompt.
4. Have the agent write a temporary mutation file, then atomically rename it to a unique `.json` file in
   `.agent-state/outbox`. Reconcile it through the controller. On rejection, inspect the bounded error, reread the
   current view, and submit a new version-based mutation.
5. Check context pressure before the route's output reserve is consumed. `checkpoint` records an epoch. Research mode
   does this at every completed segment and starts a new Pi or OpenCode session with no continuation ID. On a
   manual `reset`, end the old model session and KV cache, then start a fresh session from the immutable task contract,
   new state view, and current observation.
6. Record files through the artifact command. Keep logs and provider-returned reasoning `restricted`. Never store a
   secret, credential, reasoning transcript, or controlled data in a prompt-facing collection.
7. Run `state verify` before sealing or export. Export without restricted bytes and event payloads unless the operator
   approved those exact data classes.

```bash
agent-sandbox state init <run> \
  --domain scientific \
  --profile scientific-worker \
  --subject <worker> \
  --objective '<bounded objective>'

agent-sandbox state view <run> <worker>
agent-sandbox state reconcile <run> <worker> --workspace /absolute/worker-workspace
agent-sandbox state context --profile scientific-worker \
  --window-tokens <window> --used-tokens <used> --output-reserve-tokens <reserve>
agent-sandbox state checkpoint <run> <worker> --reason '<why the context epoch changed>'
agent-sandbox state seal <run> <worker> --reason '<why this run is complete>'
agent-sandbox state verify
agent-sandbox state export <run> --output-dir /absolute/new-package-directory
```

Use `--include-restricted` or `--include-event-payloads` only after checking the export destination and retention
policy. The output directory must not already exist.

## Boundaries

- SQLite WAL is the embedded single-controller backend. It supports concurrent local workers but not active-active
  controller replicas over the same state tree. PostgreSQL is available for the tenant-scoped provenance event plane
  and evidence leases; it does not make local prompt state or artifact files active-active.
- Bind workspaces get live outbox reconciliation during a research run. VM and Kubernetes sync workspaces reconcile
  after a segment pull; do not describe that path as live state streaming. Bind output versions are captured after
  completed tool events; sync output versions are captured at segment boundaries.
- Automatic event parsing applies to pinned Pi and OpenCode direct runs and research segments. A custom harness uses
  reviewed OpenAI-compatible or native JSONL ingestion before making an automatic-capture claim.
- Global and Audit search are metadata-only. They cannot retrieve collection values, event payloads, artifact bytes,
  or returned reasoning. Restricted evidence search and reads need a current lease for the exact run; every search and
  read adds a host audit record. Do not copy search excerpts into prompt-facing state by default.
- A local-model adapter records only reasoning text emitted by the inference server or harness. It cannot claim hidden
  activations or provider-suppressed reasoning. Keep emitted reasoning outside future prompts.
- A file created and removed inside one tool call cannot be recovered by the filesystem observer. Keep meaningful
  attempts under unique intermediate or discarded paths.
- Milvus or another vector database may index approved state or artifacts for retrieval. It is derived data, never the
  state authority or provenance ledger.
- OpenTelemetry describes execution. It does not replace the hash-chained state event and artifact records.
- Worker agreement is evidence for comparison, not scientific validation.
