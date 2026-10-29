# Research protocol

## Study record

Record these fields before workers start:

- study ID and mode;
- exact question hash and protocol hash;
- input manifest with relative path, size, and SHA-256;
- worker ID, perspective, route, harness, thinking mode, model revision, inference engine, reasoning parser, route
  limits, and tool digests;
- output size/file caps;
- numerical tolerances or evidence requirements;
- operator-defined stop, timeout, cancellation, and cleanup rules in the protocol;
- data classification and retention owner.

## Worker result

`output/results/result.json` has this exact shape. Use every shown key once and do not add top-level or nested keys.
`answer` is one bounded string. Put tables and structured values in files under `intermediate/` and cite those files
from a claim.

Keep every failed or superseded method, script, input reference, and output under `output/discarded/`. Do not overwrite
it with the successful attempt. The controller versions regular output files after completed tool events on bind
transport, after segment pulls on sync transport, and again during collection. Controller JSONL, stderr, workspace
deltas, and any returned reasoning are retained as restricted logs.

```json
{
  "schema_version": 2,
  "worker": "worker-name",
  "status": "complete",
  "answer": "bounded final answer",
  "claims": [
    {
      "statement": "one claim",
      "kind": "deterministic",
      "evidence": ["scripts/analyze.py", "intermediate/table.csv"],
      "confidence": 0.9
    }
  ],
  "limitations": ["one measured or known limit"],
  "rerun": {
    "command": "python scripts/analyze.py",
    "environment": "environment.txt"
  },
  "method": {
    "class": "deterministic-tool",
    "tool_or_model": "analysis-script",
    "version": "replace-with-version-or-digest",
    "input_manifest_sha256": "replace-with-study-input-manifest-digest",
    "code_refs": ["scripts/analyze.py"],
    "parameter_refs": ["intermediate/parameters.json"],
    "output_refs": ["intermediate/table.csv"],
    "validation_refs": ["intermediate/validation.json"],
    "seeds": [],
    "llm_disclosure": null
  },
  "quality_control": {
    "purpose": "schema-and-quality-control",
    "schema_refs": ["intermediate/schema.json"],
    "sample_refs": ["intermediate/bounded-sample.json"],
    "check_refs": ["intermediate/quality-checks.json"],
    "llm_is_scientific_instrument": false
  }
}
```

Use `interpretive` instead of `deterministic` for claims that cannot be reproduced as the same calculation. Confidence
is a worker statement, not a calibrated probability unless the protocol defines and tests calibration.
The rerun command is untrusted worker output. Review the sealed command, script, environment, and input manifest before
running it in another environment.

Accepted method classes are `deterministic-tool`, `statistical-model`, `scientific-model`,
`human-reviewed-interpretation`, and `llm-method`. Statistical, scientific-model, and LLM methods need a recorded seed
or deterministic declaration. `llm-method` also needs the exact model revision, engine, prompt and sampling files,
repeat count of at least two, and evaluation references. Do not mark `llm_is_scientific_instrument` true for another
method class.

`schema_refs` and `sample_refs` may be empty when no bounded inspection was needed. `check_refs` and all method evidence
lists must name existing regular files under the worker output.

## Comparison

The controller may compute:

- exact normalized final-answer groups;
- missing required files or broken evidence references;
- route and method diversity;
- output and seal digests;
- recorded worker result classes and phases.

It must not compute "validated" from majority vote. Scientific validation needs a declared external criterion: rerun,
known answer, held-out measurement, independent source, statistical test, or domain review.

## Privacy and custody

The reusable repo contains only synthetic examples. Study questions, inputs, outputs, prompts, and provider records
stay in private controller state and the operator-selected workspace. Telemetry records IDs, phases, counts, hashes,
limits, decisions, and result classes. Canonical agent state and the artifact event ledger are separate from telemetry.
The standard state export omits restricted bytes and event payloads; an operator must explicitly widen it. None of
these paths exports raw research data or unrestricted prompts by default.
