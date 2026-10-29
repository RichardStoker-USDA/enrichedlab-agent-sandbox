# Scientific method contract

The language model plans work, selects reviewed tools, prepares parameters, checks intermediate results, and explains
the record. It is not assumed to be the instrument that computes a scientific result.

## Data inspection

A worker may inspect bounded samples for:

- schema discovery and type checks;
- encoding, missingness, range, duplicate, and unit checks;
- choosing or configuring a tool;
- checking that a transformation or query produced the expected shape;
- finding a small set of records for manual review.

The study protocol lists the purpose, schema references, sample references, and QC check references. Samples stay in
the evidence plane unless a specific scoped view needs them. Large tables should be profiled with SQL, Arrow, R, or
Python and returned as bounded summaries with row counts and digests. The agent can request more rows when a check
needs them; it should not copy an entire dataset into the conversation by default.

This is a method rule, not an absolute content filter. The controller does not claim that no tool can print a large
input. Tool output and prompt budgets must still be set for each deployment and data class.

## Accepted computation classes

Every completed `result.json` declares one method class:

| Class | Expected authority |
|---|---|
| `deterministic-tool` | pinned code or executable with input and output evidence |
| `statistical-model` | named model, parameters, environment, input digest, seed or deterministic declaration |
| `scientific-model` | domain model with version, configuration, input digest, and validation references |
| `human-reviewed-interpretation` | named review method and evidence, with the human decision kept separate |
| `llm-method` | explicit research use of a pinned LLM under the disclosure below |

For the first three classes, the LLM may write or adapt a script, but the recorded tool execution produces the result.
The broker's bounded form of this is a reviewed template with one model-written block: the template supplies the
data and writes every file, the block is admitted before it is stored and again inside the sandbox, and its results
are labelled unvalidated until a scientist reviews the code ([research coordination](research_coordinator.md)).
Collection checks that evidence paths exist inside the bounded worker output and that the input manifest still matches
the study snapshot.

An `llm-method` is allowed only when the protocol declares the LLM itself as part of the method. The result must name
the model, exact revision, inference engine, prompt reference, sampling reference, repeat count, and evaluation
references. It must use at least two repeats. A language-model answer without that disclosure cannot pass the result
schema as an LLM method.

## Required custody

Keep these for every attempt, including an attempt that the worker rejects:

- immutable input manifest and source references;
- query text, script bytes, tool or model version, parameters, and environment;
- random seed or an explicit deterministic declaration;
- stdout, stderr, exit status, tool event, and output digest;
- intermediate and discarded bytes under unique paths;
- state mutation, checkpoint, segment, model route, and harness metadata;
- final result, figures, limitations, validation references, and rerun command.

Do not overwrite a failed attempt and call the new file the same evidence. The controller versions changed output
bytes by content digest. `output/discarded` is retained for attempts that are no longer part of the selected result.

## Reproduction and validation

Reproduction means rerunning the declared method against the same input snapshot and environment constraints.
Validation needs an external criterion such as a known answer, held-out measurement, numerical tolerance, statistical
test, independent implementation, or domain review. Agreement among agents is useful evidence about consistency, but
it is not validation by itself.

Returned reasoning can help a reviewer understand which choices the model considered. It remains restricted review
material and cannot replace scripts, queries, parameters, inputs, outputs, and runtime evidence. A short recorded
decision should cite those artifacts so later agents do not need the full reasoning transcript in context.
