---
name: enrichedlab-research-team
description: Run concurrent, isolated, reproducible scientific or data-analysis workers with EnrichedLab Agent Sandbox. Use to turn one research question and bounded input snapshot into independent worker analyses, route workers to local or approved API-backed models, provide verified tool snapshots, seal methods and results, compare outputs, and hand evidence to a host lead for validation.
license: MIT
---

# EnrichedLab research team

This skill needs an installed `agent-sandbox` controller, one or more configured inference routes, and enough route
capacity for the requested workers.

The host lead owns the question, protocol, worker roles, access, sealing, comparison, and final scientific judgment.
Workers stay isolated until their outputs are sealed. Read [references/research_protocol.md](references/research_protocol.md)
before running a study. Read `../enrichedlab-agent-state/SKILL.md` when the study needs durable lessons, context resets,
or a review package.

## Prepare

1. Classify the task as `deterministic` or `exploratory`.
2. Write one fixed question. Put it in a file when shell quoting could change it.
3. Name every input. The controller copies a bounded snapshot, rejects symlinks and special files, and records hashes.
   Large datasets should stay in a governed data service or reviewed read-only volume with their own digest and access
   policy rather than being copied through model context.
4. Choose distinct worker perspectives before any worker sees another result. For deterministic work, use independent
   methods or implementations, not role-play labels alone.
5. Select routes within each route's declared `pool_cap`. Local models and separately billed API-backed routes use the
   same OpenShell inference boundary. A consumer subscription is not assumed to grant API capacity.
6. Select the pinned `opencode` or `pi` harness per worker. Use different harnesses only when method independence or
   client behavior needs testing; a harness label alone does not make a result independent.
7. Verify each requested catalog tool by digest. A catalog hash detects changes; it does not make code safe. Review and
   qualify executable tools before giving them to workers.

## Run

```bash
agent-sandbox research create <study> \
  --mode deterministic \
  --question-file /absolute/question.txt \
  --input /absolute/bounded-input \
  --max-output-files <count> \
  --max-output-bytes <bytes>

agent-sandbox research add <study> <worker> \
  --perspective '<method and failure checks>' \
  --route <route> \
  --harness <open-code-or-pi> \
  --tool <verified-catalog-entry>

agent-sandbox research launch <study> --max-parallel <count> --max-segments <count>
```

Each worker receives the same question, protocol, and input manifest plus its own perspective. It writes only under:

```text
/sandbox/work/output/methods.md
/sandbox/work/output/scripts/
/sandbox/work/output/intermediate/
/sandbox/work/output/discarded/
/sandbox/work/output/results/result.json
/sandbox/work/output/figures/
/sandbox/work/output/logs/
```

The worker must include rerun instructions, limitations, evidence paths, claim types, a scientific method contract, and
a data-QC contract. Bounded samples may be used for schema checks and tool preparation. A declared tool or scientific
model produces the result unless the protocol explicitly declares a repeated, version-pinned `llm-method`.

Logs and intermediate files remain organized but are not treated as claims by themselves.
Every new worker also gets a scoped state view and mutation outbox. Keep failed or superseded work under `discarded`.
Do not put a reasoning transcript in state. Store short rationale and evidence references instead.

Every segment is a new provider session. The controller observes Pi or OpenCode JSONL, records tool and token metadata,
versions output files, reconciles model proposals, and verifies a checkpoint before another segment starts. A malformed,
truncated, or incomplete stream fails closed. Raw output and provider-returned reasoning remain in restricted controller
logs and never enter the next prompt.

## Seal and compare

```bash
agent-sandbox research collect <study> <worker>
agent-sandbox research compare <study>
agent-sandbox research verify <study>
agent-sandbox state verify
```

Collection checks the canonical input, question, protocol, perspective, assignment, tool digests, required result
schema, worker identity, regular-file boundary, output caps, and a host-written seal outside the sandbox. Comparison
groups exact normalized answers and reports method and artifact completeness. It does not decide that a shared answer
is true.

For deterministic results, the host lead reruns at least one sealed method outside the worker that created it and
checks declared numerical tolerances. The recorded rerun command is untrusted worker output; inspect its sealed script,
environment, and inputs before running it. For exploratory results, compare evidence coverage, assumptions, and
conflicts; do not convert consensus into a deterministic claim.

## Long horizon and scaling

- Stage a long run into fixed protocol revisions. Do not silently change the question after workers begin.
- Set `max-segments` from the stop rule and route budget. Reaching it with no valid result is `segment-limit`, not a
  partial pass.
- Use different routes or model families when correlated failure matters.
- Add workers only while route, CPU, memory, storage, API budget, and tool-service limits remain within declared caps.
- Keep raw provider credentials in OpenShell/provider infrastructure. Workers receive only the `inference.local`
  route or a brokered tool path.
- Use MCP or API tools through registered, policy-bound services. Tool descriptions and prompts are not authorization.
- Register reviewed reusable outputs in the host catalog only after collection and verification.
- Stop or delete worker sandboxes after evidence capture. Keep the study directory and seals for reruns.
- Bind transport versions files after completed tool events. VM and Kubernetes sync transport versions files at the
  segment pull. A file created and removed inside one tool call is not recoverable, so keep meaningful attempts under
  unique paths.

The dashboard shows study state, worker route and phase, seal status, comparison status, and a live provenance graph
without publishing the question, research data, prompts, or restricted result bodies. A selected evidence read needs a
short run-scoped lease and adds a host audit record.
