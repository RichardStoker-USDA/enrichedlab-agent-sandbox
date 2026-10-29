# Agent state and provenance

The controller keeps prompt-facing working state separate from event history, artifact bytes, and telemetry. An agent
gets a bounded view selected for its job. The host keeps the broader record needed for review.

This follows the main result in [SKILL.state](https://arxiv.org/abs/2608.26263): a task can continue from an immutable
prompt, a compact structured state, and the current observation instead of replaying a growing transcript. The paper
also names the limit that matters for research: history cannot be discarded when history is itself the audit target.
This controller therefore keeps an append-only provenance plane outside prompt state.

NVIDIA's [AVO paper](https://arxiv.org/abs/2603.24517) supports keeping progress, measured feedback, failed candidate
lineage, and a supervisor that detects stalled work. NVIDIA's related
[ARC-AGI-3 report](https://developer.nvidia.com/blog/nvidia-avo-reaches-100-on-arc-agi-3-demonstrating-a-frontier-level-general-purpose-architecture-for-long-horizon-autonomous-agents/)
reports 100 on the public set, not the private benchmark, and does not isolate memory as a controlled variable. This
project uses those workflow ideas without treating that score as proof of a general research system.

## Data planes

```text
fixed task + scoped state view + current observation
                         |
                         v
                  model or harness
                         |
             explicit mutation proposal
                         v
schema + authority + audience + version checks
                         |
       +-----------------+-----------------+
       |                 |                 |
canonical state    event hash chain   artifact CAS
prompt selected    out of prompt      out of prompt
```

State collections hold execution progress, facts, hypotheses, decisions, lessons, scientific references, and
coordination data. Profiles choose collections and JSON pointers, set an audience, and cap bytes and estimated tokens.
Collection access rules can further limit a view to named principals. The backend can retain fields that one worker is
not allowed to see.

Events retain machine observations and accepted mutations in sequence. Artifacts retain file bytes by SHA-256. OCSF,
OpenShell evidence, and OpenTelemetry remain separate observations. They can link to state events, but they are not the
state authority. The [OpenTelemetry GenAI conventions](https://opentelemetry.io/docs/specs/semconv/registry/attributes/gen-ai/)
also warn that tool arguments and results can contain sensitive data, so content capture stays off unless a deployment
has a data rule for it.

## Start and inspect a run

```bash
agent-sandbox state init analysis-one \
  --domain scientific \
  --profile scientific-worker \
  --subject worker-one \
  --objective 'Run the fixed protocol and retain every attempt.'

agent-sandbox state list
agent-sandbox state view analysis-one worker-one
agent-sandbox state trace analysis-one
agent-sandbox state verify
```

The standard scientific run creates `execution`, `knowledge`, `scientific`, and `coordination` collections.
Coordination is lead-only and not prompt eligible for a worker. Built-in JSON Schemas reject unrecognized fixed fields,
oversized maps, malformed references, and invalid types.

Add reviewed profiles and schemas without rebuilding:

```bash
agent-sandbox state profile install --file /absolute/profile.json
agent-sandbox state schema install domain-name --file /absolute/schema.json
```

Use `--replace` only after reviewing the current file and all profiles or runs that depend on it. A schema replacement
does not rewrite old state.

## Named-agent continuity

`agent-sandbox agent create` binds one stable logical-agent ID to one scoped state run and managed workspace. Each
`agent run` starts a fresh provider session and may create a new OpenShell runtime generation. A normal run reconciles
state, checkpoints, captures evidence, and releases the runtime. The next run materializes the same current view into
a different sandbox. See [named-agent continuity](continuity.md).

Built-in continuity profiles are domain-bound: `cluster-operator` uses `cluster-ops`; `scientific-worker` and
`research-lead` use `scientific`; `software-engineer` uses `software-engineering`. Their collections are
principal-scoped to the logical agent and declared owner. Separate processes cannot run the same logical agent
concurrently. Different agents can run concurrently up to the route's configured sandbox cap.

## Proposal loop

The worker reads `.agent-state/view.json`. It writes a complete mutation to a temporary file and atomically renames it
under `.agent-state/outbox`. A mutation names its base collection version and explicit JSON-pointer `set` and `remove`
operations. Omitted state remains unchanged.

For bind-mounted Docker workspaces, the research runner checks the outbox every 250 ms. It moves accepted proposals
and receipts to `accepted`, rejected proposals and bounded errors to `rejected`, then writes a fresh view. This supports
a live plan, observe, patch, reread loop.

Kubernetes and VM profiles use workspace sync. The controller cannot see their remote outbox until it pulls the
workspace, so the current path reconciles after pull and during collection. A deployment that needs live state on
those profiles needs an authenticated in-cluster state service or a separately qualified shared-control-volume path.
Neither is claimed by this release.

Machine facts should not depend on model memory. The Pi and OpenCode runners emit JSONL while a direct run or research
segment is active. The controller records tool status, input and output sizes and hashes, token classes, returned-text
or returned-reasoning hashes, output-file versions, exit status, capture limits, and context epochs. Raw tool content
and returned text stay in restricted provenance evidence. The model may propose semantic facts, hypotheses, decisions,
progress, and lessons.

`agent-sandbox run --harness pi|open-code` automatically creates a provenance run. `research launch` also links those
events to the worker's state run. OpenAI-compatible and native JSONL use `provenance ingest`. Do not describe another
harness's output as captured until its adapter has fixtures, a field map, and fail-closed malformed and truncation
tests. See [provenance](provenance.md).

## Context pressure and reasoning

Check pressure using the route's observed token use and an output reserve:

```bash
agent-sandbox state context \
  --profile scientific-worker \
  --window-tokens 131072 \
  --used-tokens 76000 \
  --output-reserve-tokens 16000
```

The result is `continue`, `checkpoint`, or `reset`. A checkpoint increments the context epoch and records the state-set
digest and prior event head:

```bash
agent-sandbox state checkpoint analysis-one worker-one \
  --reason 'hard context threshold reached'
```

`research launch --max-segments <count>` enforces this boundary. Every segment uses a new Pi or OpenCode session with
no continuation argument. Before another segment starts, the controller reconciles the outbox, verifies the machine
snapshot and controller logs, records a checkpoint, increments the epoch, and materializes a new view. Sending a
compacted message into the same provider thread is not a verified reset.

The selected harness reports tokens per model step or turn. The controller sums those fields for usage and uses the
largest observed total for context pressure. Missing terminal usage, malformed JSONL, a truncated stream, or an
observer error prevents a checkpoint from being accepted. A successful process exit is not enough.

The canonical record stores a short rationale, considered alternatives, evidence references, and confidence. It does
not need a reasoning transcript. If a local vLLM, SGLang, llama.cpp, or remote provider emits separate reasoning text,
the adapter stores it as restricted evidence and keeps it out of future prompts. It does not claim access to hidden
activations or reasoning the provider did not return.

Mark a completed generic run with `state seal <run> <subject> --reason '<reason>'`. Research collection does this
after it writes the worker seal. Sealing is idempotent, changes lifecycle reporting, and appends a state event. It does
not make the evidence correct or block later verification and export. Research deletion also seals the state run,
even when the worker failed or never produced a result, because a deleted runtime cannot make another state update.

## Scientific custody

Research workers have an `output/discarded` directory. Failed or superseded attempts stay there rather than being
deleted. With bind transport, the controller inventories output after every completed Pi or OpenCode tool event and at
each segment boundary. Changed bytes get a new content-addressed version, so later overwrites do not erase the earlier
version. VM and Kubernetes sync profiles capture at segment boundaries after the workspace pull. During collection,
every regular output file and the complete bounded controller-log tree enter the private content-addressed artifact
store. Methods, environment files, scripts, intermediate files, failed attempts, results, figures, and logs get
separate roles. Logs and returned-reasoning files are restricted.

At most 10,000 artifact references enter the prompt-facing scientific map. All recorded artifacts remain in the ledger
when a run has more files. This bounds prompt state without dropping custody.

Export a review package to a new directory:

```bash
agent-sandbox state export analysis-one \
  --output-dir /absolute/path/analysis-one-crate
```

The package contains RO-Crate-shaped JSON-LD, W3C PROV event nodes, a provenance JSON file, standard artifact bytes,
and a SHA-256 manifest. Restricted bytes and event payloads are omitted by default. Add `--include-restricted` or
`--include-event-payloads` only after reviewing the data class and destination. The package is not signed and does not
make a scientific-validity claim.

The metadata is [RO-Crate 1.3](https://www.researchobject.org/ro-crate/specification/1.3/) with the
[Process Run Crate 0.6](https://www.researchobject.org/workflow-run-crate/profiles/process_run_crate/) profile and
[PROV-O](https://www.w3.org/TR/prov-o/) terms. The descriptor names the specification; the root names the profile in
`conformsTo` and has `name`, `description`, `datePublished` (the export time) and `license`, which points to `#usage`:
"No license granted", whose description says use follows the owning project's data agreement. The export chooses no
license for the run's outputs. The run is one `CreateAction`: its `instrument` is `#runtime`, the program that
recorded it (a `SoftwareApplication`; the run record keeps no version, so none is stated), its `object` is `#profile`,
the state-view profile it was recorded under, and its `actionStatus` `CompletedActionStatus` once the run is sealed
(with `endTime`) and `ActiveActionStatus` before. The state store records custody, not production: who registered which
bytes under which role, a word the caller chose. So the run names no `result`, no file is said to be generated by an
event, and each file names the events that recorded it in `recordedByEvent`; the root's `hasPart` lists the files with
their roles in `additionalType`. The export is a second
`CreateAction` (`#export`) whose `instrument` is `#exporter`, this program with its `softwareVersion`, and whose
`result` is the crate. A restricted file left out is a `File` with a `#artifact-sha256-<digest>` identifier, its digest,
size and `includedInPackage: false`, so the root's `hasPart` still lists it without a path to bytes it does not hold.

Every key the export adds is mapped in the crate's context: `startedAtTime` to `prov:startedAtTime` (an
`xsd:dateTime`), and the rest (`recordedByEvent`, `includedInPackage`, `sensitivity`, `sequence`, `epoch`, `kind`,
`actor`, `authority`, `payload`, `payloadSha256`, `previousEventHash`, `eventHash`, `contextEpoch`) to terms under
`urn:uuid:516305d7-526a-4182-91ff-25c8d6195928#`, each defined in the graph as an `rdf:Property` with `rdfs:label` and
`rdfs:comment`. A payload is a JSON literal (`@json`); a payload left out of the export is absent, never a null the
context would read as a recorded JSON null. The export test expands each key through the crate's own context and
compares it with an IRI written out in the test, not taken from the exporter's constants. The UUID URN names the
vocabulary without claiming a domain or an unregistered w3id namespace; when the vocabulary is published at a stable
address, a later export can map the same keys there and state the URN terms as equivalents. Expanded with a JSON-LD
processor and RO-Crate 1.3's context, an export keeps every key, claims no generation, and carries a payload only when
it was included (checked offline with pyld 2.0.4 and the context file of SHA-256 `5a3df1a4…`). It is a
review package, not a complete conformance claim for every domain extension.

## Storage and deployment

SQLite WAL with `synchronous=FULL` is the embedded backend. Database and artifact files use owner-only modes. It handles
concurrent workers under one controller process and is suitable for local macOS/Linux or one controller replica with
persistent storage in Kubernetes.

PostgreSQL is implemented for the shared provenance event plane and evidence leases. It uses tenant row-level security,
forced append-only event controls, one advisory serialization key per run, and separate migration and runtime DSNs.
The prompt-state projection and content-addressed artifact bytes remain single-controller files. Do not point two
controller replicas at the same SQLite or artifact tree.

The authenticated remote broker is separate from the local registry. It stores principal, group, profile, task, lease,
lifecycle event, and artifact metadata in PostgreSQL. One fenced worker owns one local logical-agent registry and RWO
artifact tree. After every task, it exports the complete scoped state and provenance run, including restricted
provider-returned reasoning and tool I/O, into owner-only task artifacts before reporting success.

This does not make prompt-state or artifact bytes active-active. Several workers need a qualified shared state
projection and object-store custody adapter with one logical-agent writer, tenant storage quotas, retention, and
recovery tests. Milvus can index an approved subset for semantic retrieval, but the index is derived and can be
rebuilt. Do not put the only copy of state or provenance in a vector database. See
[cluster deployment](cluster-deployment.md) and [remote broker](remote-broker.md).

Keep inference and Milvus in separate stateful workloads. Run disposable agents in OpenShell sandboxes with gVisor or
Kata where qualified. A runtime class changes isolation; it does not scale a tool. Use queued Jobs for bounded batch
tools and an HTTP serving layer for stateless services. Keep databases, instruments, and expensive warm services under
their own lifecycle and concurrency controls.

## Limits

- Hashes detect byte changes; they do not prove a method or source is correct.
- The host, state database, and artifact store are trusted. Host compromise can rewrite or truncate local evidence.
- The event hash chain detects internal edits and gaps, but clean tail truncation needs an external signed anchor.
- Dashboard state shows metadata and counts, not state values or event payloads.
- A file created and removed entirely inside one tool call cannot be recovered from filesystem snapshots. The tool
  event and result digest remain. Use a unique intermediate or discarded path when the bytes matter.
- Automatic event parsing supports pinned Pi and OpenCode direct runs and research segments. Other harnesses use the
  reviewed OpenAI-compatible or native ingest boundary.
- Bind transport can version output after each completed tool event. Sync transport versions only at segment pull.
- Agent-to-agent transport is separate from state. Use controller-owned team or research coordination locally. Use A2A
  only when separately deployed agents need a network protocol, and keep MCP for agent-to-tool access.
