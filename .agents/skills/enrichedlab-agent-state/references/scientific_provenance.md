# Scientific provenance

## Required custody

For each attempt, keep links to:

- frozen inputs and their digests;
- method text, executable scripts, parameters, package lock or environment inventory, random seeds, and runtime facts;
- intermediate files used by later steps;
- failed or superseded outputs under `output/discarded`;
- final result files and figures;
- validation runs, tolerances, and reviewer decisions.

The agent may inspect bounded samples for schema discovery, QC, and tool preparation. Prefer SQL, Arrow, R, Python, or
another reviewed tool to profile large inputs and return bounded summaries. The agent plans and edits the tool call;
the declared tool or scientific model produces the result.

If an LLM is itself the research method, declare `llm-method` and record its exact model revision, inference engine,
prompt, sampling settings, repeats, seeds or deterministic declaration, and evaluation references. Otherwise, LLM
decoding cannot be the source of a scientific computation claim.

With bind transport, the controller records changed regular output files after each completed or failed Pi or OpenCode tool event and
at segment boundaries. VM and Kubernetes sync transport records them after the segment pull. Collection records the
final output tree plus all bounded controller segment logs. `methods.md`, `environment.txt`, and `scripts` become method
references. `intermediate` and `discarded` become intermediate or failed-attempt references. `results` and `figures`
become claim references. Logs become restricted validation references. More than 10,000 files remain in the artifact
ledger but are not all copied into prompt-facing reference maps.

For version 2 workspace snapshots, use the controller-issued `observed_after` references to resolve the exact
provenance run, event ID, sequence and hash. The same references are in the state-ledger snapshot event. Resolve
file versions from the retained snapshot and keep the original visibility policy. These are observed-after links,
not causal claims. Do not infer links for older snapshots, segment-only captures, or unrelated files with matching
hashes. A failed tool can still produce a retained file version; its existence is not validation of that file.

A file created and removed entirely inside one tool call has no recoverable file version. The tool event and output
digest remain, but reproducible work must use unique intermediate or discarded paths when the bytes matter.

## Review package

`state export` writes:

```text
<new-output-directory>/
  ro-crate-metadata.json
  provenance.json
  manifest-sha256.txt
  artifacts/sha256/<digest>
```

The metadata uses RO-Crate and W3C PROV terms. The manifest covers every package file created before the manifest.
Standard exports leave restricted artifact bytes out while keeping their digest metadata. Event payloads are also off
by default. This supports selective sharing but does not classify data, choose a retention period, or supply a digital
signature.

Independent workers keep separate state and cannot read sibling outputs during the blind phase. The host compares
sealed results after collection. Agreement can raise confidence in repeatability, but validation still needs a known
answer, held-out data, independent measurement, declared tolerance, statistical test, or domain review.
