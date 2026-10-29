# Typed decision candidate

This opt-in Python standard-library example measures typed decisions for event tags,
prompt routing, and extractive manuscript review. It does not grant access, execute
tools, alter evidence, fill manuscript fields, or run inside an untrusted sandbox.
The cases are public synthetic text, not research observations or private prompts.

`evaluate.py` separates provider configuration, versioned rubrics, source state, and
normalized answers. A compatible on-prem service needs a different endpoint and
model in configuration. Another protocol implements `evaluate()` with the same
checked answer shape. No hosted provider is required by the broker or artifact
format. Keep credentials in a separate regular mode-600 file and pass its path.

```sh
python3 examples/typed_decisions/evaluate.py \
  --config examples/typed_decisions/typesafe.json \
  --cases examples/typed_decisions/cases.json \
  --key-file /secure/provider_key \
  --output /private/measurement_directory
```

Measurements record rubric and input hashes, reference, exact returned model,
distribution, latency including retries, match against labelled expectations, and
source revision. They do not repeat input text or credentials. A provider failure
produces an unavailable advisory result. Existing measurements are never replaced.
Unknown answer identifiers, malformed probabilities, and mismatched answer types
are rejected. Bounded timeouts and retry limits keep this experiment contained.

Treat distribution-derived confidence separately from empirical calibration. This
small pilot can expose errors and measure observed latency; it cannot establish
production thresholds. Before a use case controls a meaningful choice, its owner
needs representative held-out examples, false-accept and false-reject costs, and a
versioned calibration record. Thresholds do not transfer automatically across
models, rubrics, languages, domains, or context lengths.

## Integration boundary

Persist the authoritative event and artifact references first. An asynchronous,
bounded enrichment queue may add versioned tags without delaying or rewriting the
event. Human-readable labels are aliases of stable identifiers. Failed enrichment
can be retried from authorized evidence; it cannot make the original event vanish.
Keep high-cardinality tags in the event/search store, not metric labels. Search and
conversational history must enforce the original tenant, lab, and artifact ACLs.

Prompt handling is purpose-specific: routing may use an ephemeral authorized view;
replay or later analysis requires a restricted retained artifact and retention
policy. Prompts, documents, reasoning text, and data rows do not enter shared
telemetry. Redacted operational events and model-generated scientific explanations
are distinct records. A future local model's exposed reasoning is optional private
evidence, not proof that a scientific conclusion is correct.

For manuscript forms, first extract source text with stable page/span references.
Typed questions may classify sections, suggest OCR, flag missing source support,
or prioritize a reviewer. Exact form values still come from cited source spans.
Measure end-to-end latency and extraction errors against the existing path before
claiming a speed or quality improvement. Never use model confidence as a substitute
for matching an extracted field to its source.

As reviewed on 2026-09-22, Jev 1.13 accepts text/structured text and has no image
input. Keep vision capability explicit in a future adapter. Pin the model used for
a measured rubric; moving aliases can change answers. Hosted zero-retention terms
are account-specific, so this example sends public synthetic cases only.

Primary references: [API](https://docs.typesafe.ai/api),
[models](https://docs.typesafe.ai/models),
[confidence](https://docs.typesafe.ai/confidence).
