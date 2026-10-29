# Research workers

`agent-sandbox research` runs several isolated agents against one frozen question and input snapshot. The host controls
the study record, model routes, tool copies, service approvals, result seals, and comparison. Workers cannot see sibling
workspaces unless the operator creates a separate approved peer service.

This is an execution and evidence workflow. It does not replace study design, statistical review, data governance, or
domain validation.

## Plan the study

Decide these before creating workers:

- `deterministic` for a calculation with a declared rerun and tolerance;
- `exploratory` for interpretation, search, synthesis, or hypothesis work;
- one fixed question and optional protocol file;
- bounded input files or directories, with no symlinks or special files;
- distinct methods or failure checks for each worker;
- one inference route and thinking mode per worker;
- reviewed tool-catalog entries, if any;
- output file and byte caps, retention, API budget, and stop rules.

Use different implementations or model families when correlated error matters. Role labels alone do not create
independent evidence.

## Create and run

Prefer files for long questions and protocols so shell quoting cannot change them:

```bash
agent-sandbox research create soil-summary \
  --mode deterministic \
  --question-file /absolute/path/question.txt \
  --protocol /absolute/path/protocol.md \
  --input /absolute/path/data \
  --max-output-files 20000 \
  --max-output-bytes 1073741824

agent-sandbox research add soil-summary direct \
  --perspective-file /absolute/path/direct-method.txt \
  --route local-agent \
  --harness open-code \
  --thinking on

agent-sandbox research add soil-summary cross-check \
  --perspective-file /absolute/path/cross-check-method.txt \
  --route api-agent \
  --harness pi \
  --thinking medium

agent-sandbox research launch soil-summary --max-parallel 2 --max-segments 8
```

`max-parallel` limits host launch concurrency. Each route also has a `pool_cap`; adding workers over that cap fails.
Local and API-backed models use the same named OpenShell inference boundary. Provider keys stay in the gateway. A chat
subscription is not treated as API capacity.

The worker runner is pinned Pi or OpenCode inside each sandbox. The model route comes from private `routes.json`, so
workers may use different local or approved remote providers without getting provider credentials. The selected
harness is part of the host-owned worker seal.

`max-segments` is the most fresh provider sessions allowed for each worker during this launch. A segment completes one
bounded unit, flushes semantic proposals, and exits. If the strict result contract is incomplete, the controller
verifies the machine record, checkpoints state, discards that provider session and its KV state, and starts the next
segment from the fixed assignment and new scoped view. Reaching the limit returns `segment-limit`; it is not reported
as a completed result.

## Tool catalog

Register a reviewed reusable tool before adding it to a study:

```bash
agent-sandbox catalog add table-checker \
  --kind analysis-tool \
  --path /absolute/path/under/the/controller/workspace \
  --entrypoint run.sh
agent-sandbox catalog verify table-checker
```

Then add `--tool table-checker` to a worker. The controller verifies the catalog digest and copies that exact tree into
the worker. A matching hash proves the bytes did not change; it does not prove the tool is correct or safe.

MCP servers and APIs should stay outside the worker or run as separate governed services. Give workers exact domains,
methods, ports, and lease times. A tool description is not authorization.

## Output custody

The host study tree is:

```text
studies/<study>/
  control/                 question, protocol, input snapshot, manifest
  controller-logs/<worker>/
  workers/<worker>/
    research-assignment.json
    perspective.txt
    .agent-state/            scoped view, outbox, accepted and rejected proposals
    input/
    tools/
    output/
      methods.md
      environment.txt
      scripts/
      intermediate/
      discarded/            failed or superseded attempts retained for review
      results/result.json
      figures/
      logs/
  seals/
  recovery/
```

The dashboard exposes names, routes, phases, counts, seal state, and comparison status. It does not return question,
prompt, input, result, or raw log bodies through its API.

`result.json` must match the exact schema in
[the research skill reference](../.agents/skills/enrichedlab-research-team/references/research_protocol.md). Claims point
to regular files inside the worker output. The hashed worker assignment also embeds a strict template so a worker does
not need access to repository docs. Every result records limitations, a rerun command, and an environment file.
Treat the rerun command as untrusted worker output. Review the sealed script, command, environment, and input digest
before running it outside that worker's sandbox.

The result also declares a scientific method and a data-QC contract. Bounded rows may be inspected for schema discovery,
quality checks, and tool preparation. A deterministic tool, statistical model, or scientific model produces the
scientific result. If an LLM is itself part of the research method, the protocol must declare `llm-method` and record
its exact revision, engine, prompt, sampling configuration, repeat count, and evaluation references. See
[scientific method contract](scientific-methods.md).

Each new worker has a `scientific-worker` state run. For bind transport, the host reconciles atomic mutation proposals
during execution and snapshots output after each completed tool event. For VM and Kubernetes sync transport, proposals
and file changes become host-visible after the segment pull. Every segment records a fresh-session event, tool
metadata, token classes, returned-output hashes, workspace deltas, stdout JSONL, stderr, and a checkpoint receipt.
Collection records every regular output and the bounded controller-log tree as content-addressed artifacts. It links
methods, intermediate work, failed attempts, results, figures, and restricted logs to the state event ledger. See
[agent state](agent-state.md).

Version 2 workspace snapshots include `observed_after`, containing the exact provenance run, event ID, sequence,
and event hash returned by the controller when it records a completed or failed tool event. The snapshot and its
state-ledger event carry the same references. Each snapshot retains its own immutable file versions, so a later
overwrite does not replace an earlier attempt. Segment-only snapshots leave `observed_after` empty.

This records that the controller observed those files after the named tool event. It does not prove that the tool
created each file, especially when subprocesses can write concurrently. A consumer must resolve the exact run and
event hash and apply file visibility before presenting a link. Older snapshots have no such association. These
references do not add a broker history reader, authorize downloads, or turn direct Sandbox lifecycle events into
scientific tool steps.

The raw Pi or OpenCode stream may contain tool output or provider-returned reasoning. It stays owner-only and is
classified restricted when collected. Prompt-facing state gets hashes, counts, short rationales, lessons, and evidence
references. Missing terminal usage, malformed JSONL, truncation, a harness error event, or observer failure stops a
passing checkpoint. The controller does not claim access to reasoning the provider did not return.

## Seal and compare

Collect every finished worker separately:

```bash
agent-sandbox research collect soil-summary direct
agent-sandbox research collect soil-summary cross-check
agent-sandbox research compare soil-summary
agent-sandbox research verify soil-summary
```

Collection checks the frozen question, protocol, perspective, assignment, input manifest, tool copies, result schema,
evidence paths, and output caps. It copies bounded controller logs into the output and writes a host-owned seal outside
the worker workspace. Later changes make verification fail.

Comparison groups answers after whitespace normalization and reports method-file presence and route diversity. It does
not score truth, merge prose, or call a majority result validated. Use an external criterion such as a known answer,
held-out measurement, independent source, declared numerical tolerance, statistical test, or domain review.

## Stop, retry, and remove runtimes

```bash
agent-sandbox research stop soil-summary
agent-sandbox research reset-worker soil-summary direct --reason 'method revision approved'
agent-sandbox research delete soil-summary
```

`stop` keeps sandboxes and files. A stopped prepared worker can launch again. `reset-worker` moves the prior output into
the study recovery directory, starts a new context epoch, and recreates an empty output tree; it refuses running,
sealed, or deleted workers.
`delete` captures evidence unless `--skip-evidence` is explicitly used, removes worker sandboxes, and keeps the study,
outputs, controller logs, seals, and comparison. It seals the worker's state run before marking the sandbox deleted,
including failed or unfinished workers. State sealing closes mutation; it does not change the research result status.
Repeating `delete` on an upgraded controller also closes a retained state run from an older deletion.

For long runs, use fixed protocol revisions and separate studies instead of changing control files after workers start.
The launcher can be interrupted through `research stop` from another host session. A stopped or failed worker is never
silently counted as a sealed result.

## Tool placement

Run a cheap, mutable, or untrusted analysis tool inside each worker. Put a costly stateless tool behind a governed HTTP
or MCP service when warm startup and concurrent requests justify sharing it. Keep Milvus, databases, instrument
gateways, and other mutable services under their own lifecycle and client limits. gVisor and Kata isolate workloads;
they do not add replicas or schedule request capacity. See [service access](services.md#tool-placement-and-concurrency).
