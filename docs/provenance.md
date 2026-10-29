# Provenance timeline and evidence

The provenance service records what the controller observed without putting the full history back into an agent's
prompt. The Svelte dashboard renders that record as a live directed graph and a time slider. Selecting a step shows
its actor, segment, harness, model route, inference engine, reasoning parser, hashes, parents, and linked artifacts.
Restricted bytes need a short, run-scoped evidence lease and an explicit purpose.

## Capture paths

| Source | Automatic path | What is recorded |
|---|---|---|
| OpenCode | `agent-sandbox run --harness open-code` and `research launch` | lifecycle, tool I/O, usage, compaction, returned output, returned reasoning, errors |
| Pi | `agent-sandbox run --harness pi` and `research launch` | lifecycle, tool I/O, usage, compaction, retries, returned output, returned thinking, errors |
| OpenAI-compatible stream | `provenance ingest --harness open-ai-compatible` | JSONL or standard SSE data lines with content, reasoning fields, tool arguments, usage, errors |
| EnrichedLab native JSONL | `provenance ingest --harness native` | caller-defined typed events after strict schema validation |
| Codex or Claude Code | host controller actions and native JSONL from a reviewed adapter | no unreviewed transcript parsing |

Pi and OpenCode are the supported automatic sandbox runners. A custom scientific harness should emit the native
event schema or pass its inference stream through the OpenAI-compatible adapter. This keeps the research scheduler,
model client, and user interface independent from one harness vendor.

Bootstrap or run Pi explicitly:

```bash
agent-sandbox bootstrap <sandbox> --harness pi
agent-sandbox run <sandbox> --harness pi --thinking xhigh -- 'Complete one bounded task.'
```

The same commands default to OpenCode when `--harness` is omitted. A direct run returns its provenance run ID.

## Custom harness event schema

Write one JSON object per line. Unknown fields and unsupported schema versions fail closed.

```json
{
  "schema_version": 1,
  "kind": "agent.tool.finished",
  "evidence_role": "tool-io",
  "visibility": "restricted",
  "source_event_id": "step-7-tool-2",
  "parent_source_event_id": "step-7",
  "metadata": {
    "tool": "analysis-script",
    "exit_code": 0,
    "input_sha256": "replace-with-digest",
    "output_sha256": "replace-with-digest"
  },
  "restricted_content": null
}
```

Ingest it from a file or stdin:

```bash
agent-sandbox provenance ingest run-001 worker-001 \
  --harness native \
  --input events.jsonl \
  --segment 1 \
  --provider local \
  --model model-name \
  --engine custom-harness \
  --session provider-session-id
```

Each accepted event receives a controller UUID, timestamp, tenant, sequence, previous hash, and event hash. Source IDs
express the caller's lineage; controller IDs and hashes express storage custody.

## Local-model reasoning

The controller records reasoning that the serving stack returns as a separate field:

- [vLLM](https://docs.vllm.ai/en/latest/features/reasoning_outputs/) can return `reasoning_content` when the selected
  model has a matching reasoning parser;
- [SGLang](https://github.com/sgl-project/sglang/blob/main/docs/docs/advanced_features/separate_reasoning.mdx)
  can separate `reasoning_content` when started with its reviewed reasoning parser;
- [llama.cpp](https://github.com/ggml-org/llama.cpp/blob/master/tools/server/README.md) can expose separated reasoning
  content when its reasoning format and parser support the model;
- Pi and OpenCode translate those provider fields into their own JSON event streams.

Pin `model_revision`, `inference_engine`, and `reasoning_parser` in the private route. Parser changes are method changes
because they can alter what text is labeled reasoning or output.

Provider-returned reasoning is restricted evidence. It is not inserted into prompt-facing state, used as a scientific
result, or presented as a complete account of model computation. The controller cannot capture hidden activations,
unemitted internal tokens, or reasoning that a provider suppresses. A short decision record with evidence references
is the durable working memory.

## Provenance replay

Start the loopback dashboard, open **Provenance**, and select a run. The surface provides:

- an SSE-updated Svelte Flow graph with step, tool, output, reasoning, state, checkpoint, and artifact nodes;
- lane labels inside the graph coordinate system, so labels remain attached while panning and zooming;
- start, previous, next, and latest controls plus a time slider;
- baseline comparison between two graph positions;
- filters for event kind, evidence role, actor, segment, model, and text;
- artifact custody, content hashes, model route metadata, and parent edges;
- a labeled minimap that can pan the same graph;
- short evidence leases for restricted event or artifact bytes;
- lease-gated search across bounded restricted evidence excerpts.

The initial fit keeps the lane labels and first graph steps readable. The metadata filter works only within the loaded
graph. The global search in the application header searches all bounded controller metadata and can route to an exact
run or state event. Audit has its own server-side search and 50-row pages, so a long ledger does not create one large
DOM or response.

The graph response is metadata-only. Reading restricted content is a separate POST, needs a current lease for that run,
and adds a host audit record. The API rejects cross-origin requests and the server refuses a non-loopback bind. The
dashboard is an operator surface, not a public or sandbox-facing service.

For scientific worker snapshots, the graph may add an `observed-after` edge only when a machine-owned
`workspace.snapshot.captured` state event has the closed version-2 receipt shape and its reference exactly matches a
stored Pi or OpenCode terminal tool event by run ID, event ID, sequence, and hash. A separate `snapshot-receipt` edge
is emitted only for the matching standard `workspace-delta` artifact recorded by the same state ledger. These edges
show observation order, not that a tool caused a file change. Invalid, model-owned, or unselected records produce
no link; a truncated response keeps a link only when both endpoints are present. The projection remains metadata-only and does not read artifact or restricted provenance bytes.

This relation currently covers `ResearchEventObserver` snapshots only. It does not make direct Sandbox lifecycle
metadata, legacy logical-agent records, or arbitrary harness metadata into scientific tool evidence.

Graph responses cap the number of events and mark `truncated`. The browser also caps rendered nodes to protect the UI.
Use an export or a narrower run/segment for full offline review.

Only reasoning text emitted by the provider or harness can appear in replay. For a local inference stack this can be
the separated reasoning field returned by vLLM, SGLang, llama.cpp, or the harness adapter. Hidden activations,
unemitted tokens, and suppressed reasoning are not observable evidence. A later summary can cite the recorded event
IDs and digests, but it must not replace the underlying event and artifact custody records.

## Storage modes

Embedded mode uses SQLite WAL with full synchronization. It is the default for a laptop, one host controller, or one
controller replica with persistent storage.

PostgreSQL mode stores the append-only event plane and evidence leases in PostgreSQL. It adds tenant row-level
security, per-run advisory serialization, separate migration and runtime identities, and forced update/delete blocks
on event rows. The local agent-state projection and artifact bytes still belong to one controller. PostgreSQL does not
turn those files into active-active shared state.

```bash
agent-sandbox provenance configure --backend postgres --tenant research-group-a
agent-sandbox provenance prepare
agent-sandbox --output json provenance graph <run-id>
agent-sandbox --output json provenance export <run-id> \
  --output-dir /absolute/new-provenance-export
```

`provenance export` writes a private JSONL event stream and SHA-256 manifest. It omits restricted content by default.
Use `--include-restricted` only for an owner-approved evidence destination. The remote worker uses that option inside
the owner-only task artifact tree so provider-returned reasoning and tool I/O survive sandbox release. Exports stop at
100,000 events instead of silently truncating.

The DSNs come from the environment names in `controller.toml`; their values are never saved there. See
[`migrations/postgres/README.md`](../migrations/postgres/README.md) for the role and grant boundary.

For a multi-user cluster, shard controller ownership by tenant or run, keep one writer for each local state projection,
put the shared event plane in PostgreSQL, and put large immutable artifacts in a content-addressed object store. Milvus
or another vector index may index an approved projection, but it is derived data rather than the custody record.

## Integrity limits

- The hash chain detects edits and internal gaps. Detecting clean tail truncation needs an external signed anchor.
- The record proves what this controller accepted, not that an input, tool, model, or scientific claim was correct.
- A compromised host, migration owner, database owner, or storage administrator can change the evidence boundary.
- An event stream can omit work done outside the observed process. Filesystem snapshots and tool-service telemetry are
  separate evidence sources and should be linked by digest.
- Evidence leases limit routine disclosure; they are not DRM against the trusted host user.

## Incremental logical-agent capture

For supported logical-agent Pi and OpenCode runs, each validated harness line is appended to the canonical provenance ledger while OpenShell capture is still active. The controller appends the terminal lifecycle event once after capture and adapter finalization. A malformed tail, timeout, or later persistence failure preserves already accepted events; a persistence failure marks the attempt failed and does not report captured output as a successful completion. Direct Sandbox runtime events remain a separate metadata plane until that runtime has a canonical provenance writer.
