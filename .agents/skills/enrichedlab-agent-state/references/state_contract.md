# State contract

## Planes

Keep these separate:

| Plane | Authority | Prompt use |
|---|---|---|
| immutable task contract | host/operator | always, bounded |
| canonical state collections | host-validated single-controller records | selected by profile and JSON pointer |
| event ledger | append-only machine/model/operator events | metadata only unless queried |
| artifact store | content-addressed files | references only |
| telemetry | OpenShell, OCSF, metrics, traces | operational review, not state authority |

The prompt input for a step is the task contract, scoped state view, and current observation. Do not replay the full
conversation to simulate memory.

## Mutations

A mutation has these fields:

```json
{
  "schema_version": 1,
  "run_id": "run-one",
  "subject": "worker-one",
  "collection": "execution",
  "base_version": 0,
  "set": {"/phase": "running"},
  "remove": [],
  "rationale": {
    "summary": "The fixed input passed validation.",
    "alternatives": [],
    "evidence": ["sha256:replace-with-digest"],
    "confidence": 1.0
  }
}
```

Omitted fields remain unchanged. `remove` is explicit. A model cannot replace or remove the collection root. The
controller checks the base version, authority list, size caps, JSON pointers, and collection schema in one immediate
transaction. Accepted and rejected proposals and their receipts remain in separate workspace directories.

Model mutations cannot change the execution objective or the scientific input, method, intermediate, claim, and
validation reference maps. The host records those file facts. A model may update scientific `failed_attempts` and the
semantic knowledge and execution fields allowed by their collection contract.

Machine observations must come from the host or tool adapter. The model may propose facts, hypotheses, decisions,
lessons, and progress. Do not let a model assert that a command ran, a file hash matched, a test passed, or access was
granted when the controller can measure it.

The built-in automatic adapters observe pinned Pi and OpenCode JSONL during direct runs and `research launch`. They
record tool and error events, token classes, returned-content hashes, capture status, and output-file versions. A
research segment with missing terminal usage, malformed JSONL, truncation, or observer failure is rejected. Raw content
remains restricted. A custom harness uses the strict native envelope or reviewed OpenAI-compatible stream and needs a
versioned fixture and fail-closed adapter.

## Profiles and audiences

A profile selects collection names and JSON pointers, orders them by priority, caps bytes per collection and tokens
for the whole view, and sets one audience. A collection also declares allowed authorities, audiences, optional named
principals, and whether it is prompt eligible. Both checks must pass.

Built-in profiles are `scientific-worker`, `research-lead`, `cluster-operator`, and `software-engineer`. Add a schema
under `schemas/agent-state` and a profile under `profiles/state`, or install reviewed JSON at runtime. New documents
use `additionalProperties: false` where a fixed contract is expected. Extension maps use bounded object keys and
values.

The backend may collect more state than one profile exposes. Changing a profile changes the next generated view, not
the underlying event or artifact history.

## Context epochs and reasoning

`state context` uses the profile's soft and hard pressure ratios over:

```text
(used tokens + reserved output tokens) / context window tokens
```

A checkpoint records the collection-set digest and prior event head, increments the epoch, and writes a new event.
Research segments use a new provider session after every verified checkpoint. For a manual hard reset, destroy the old
inference session and KV cache. Do not claim a context reset if the client continues the same provider thread.

Store a short decision record with alternatives, evidence, and confidence. A reasoning transcript is neither needed
nor accepted as canonical state. If vLLM, SGLang, llama.cpp, or another provider returns reasoning text, store it as
restricted evidence outside the prompt. Do not claim access to activations or reasoning the provider did not return.

## Storage ownership

SQLite and the content-addressed artifact tree have one controller writer. PostgreSQL may hold shared provenance events
and evidence leases with forced tenant RLS and one serialized append sequence per run. Use separate migration and
runtime identities. The runtime identity cannot be a schema owner, table owner, superuser, or `BYPASSRLS` role.

## Integrity and privacy

- Database, WAL, SHM, views, receipts, and artifacts use owner-only modes.
- Events include payload digests and a previous-event hash.
- Artifact bytes use a SHA-256 content-addressed path and are rehashed by `state verify`.
- The dashboard returns run counts and custody metadata, not collection values or event payloads.
- The standard export omits restricted bytes and all event payloads. Explicit flags widen the export and need review.
- A hash detects change. It does not prove that source data, code, or a model statement is correct.
