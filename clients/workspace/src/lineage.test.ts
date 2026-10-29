import assert from "node:assert/strict";
import test from "node:test";
import type { Lineage, TaskReview } from "./contracts.ts";
import { when } from "./language.ts";
import { describeLineage, gapSentence, imageName, lineageSummary, templateName } from "./lineage.ts";

const reader = "11111111-1111-4111-8111-111111111111";
const colleague = "22222222-2222-4222-8222-222222222222";

function lineage(overrides: Partial<Lineage> = {}): Lineage {
  return {
    schema: "enrichedlab.task-lineage.v1",
    task: { task_id: "33333333-3333-4333-8333-333333333333", project_id: "44444444-4444-4444-8444-444444444444",
      action_key: "fit-model", objective: "Fit the declared model.", status: "completed", result_class: "sandbox_completed",
      error_code: null, attempt: 1, submitted_at: "2026-09-24T18:17:00Z", started_at: "2026-09-24T18:18:00Z",
      finished_at: "2026-09-24T18:20:00Z" },
    origin: { origin: "research_run", basis: "submission", principal_id: reader, principal_name: null,
      principal_kind: "person", run_id: "55555555-5555-4555-8555-555555555555", turn: 2 },
    plan_step: { action_key: "fit-model", title: "Compare tooth length by supplement", status: "completed", rationale: null },
    inputs: { workspace: { input_refs: ["input://public/toothgrowth"], source_digest: `sha256:${"b".repeat(64)}` },
      checkpoint_revision: 3, platform_tool: null },
    method: { profile: "generated-r-analysis-v2b", profile_name: "Model-written R analysis", tool_class: "agent_built",
      template_ref: "template://r-generated-analysis/v2", template_sha256: `sha256:${"c".repeat(64)}`, harness: null,
      role: null, tool_bundle: null },
    code: { path: "generated/block.R", sha256: `sha256:${"f".repeat(64)}`, size_bytes: 1391,
      admission: { schema: "x", result: "accepted", template_ref: "template://r-generated-analysis/v2", block_sha256: "",
        size_bytes: 1391, lines: 40, reasons: [], functions_used: [], namespaces: [], numeric_literals: 3,
        string_literals: 13, uses_random_numbers: false, allowlist: `sha256:${"9".repeat(64)}`, scientific_validation: "unvalidated" },
      run_id: "55555555-5555-4555-8555-555555555555", turn: 2, created_at: "2026-09-24T18:17:00Z" },
    execution: { image_ref: `registry.example/release/agent-sandbox/site@sha256:${"7".repeat(64)}`, cpu_limit: "2",
      memory_limit: "4Gi", max_run_seconds: 600, attempts: 1, scientific_history: 1 },
    outputs: [{ artifact_id: "a", role: "result", label: "Analysis package", relative_path: "package.tar.gz",
      media_type: "application/gzip", size_bytes: 64587, sha256: "0".repeat(64), created_at: "2026-09-24T18:20:00Z",
      provenance_class: "agent_built_tool_output" }],
    review: { state: "unvalidated" },
    gaps: ["input_class_not_recorded"],
    ...overrides,
  };
}

const row = (rows: ReturnType<typeof describeLineage>, key: string) => rows.find((r) => r.key === key)!;

test("a work session's task names its decision, its plan step and the unrecorded input class", () => {
  const rows = describeLineage(lineage(), reader);
  assert.deepEqual(rows.map((r) => r.key), ["asked", "step", "data", "method", "code", "ran", "results", "review"]);
  assert.equal(row(rows, "asked").text, "The research agent, in decision 2 of a work session you started.");
  assert.equal(row(rows, "step").text, "Compare tooth length by supplement.");
  assert.equal(row(rows, "data").text, "The input the project had selected when it was submitted.");
  assert.deepEqual(row(rows, "data").details, ["Input input://public/toothgrowth", `Source copy ${"b".repeat(12)}`]);
  assert.deepEqual(row(rows, "data").gaps, [gapSentence.input_class_not_recorded]);
  assert.equal(row(rows, "method").text, "Model-written R analysis: model-written R code under the reviewed template r-generated-analysis v2.");
  assert.equal(row(rows, "code").text, "Model-written code (1.4 KB), checked by the platform before it ran.");
  assert.deepEqual(row(rows, "code").details, [`Code ${"f".repeat(12)}`, `Allowed functions list ${"9".repeat(12)}`]);
  assert.equal(row(rows, "ran").text, "A recorded environment, pinned to an exact build.");
  assert.deepEqual(row(rows, "ran").notes, ["1 attempt"]);
  assert.deepEqual(row(rows, "ran").details, ["Image agent-sandbox/site, build 777777777777", "2 CPU, 4Gi memory, up to 10 minutes"]);
  assert.deepEqual(row(rows, "results").notes, ["Made by model-written code."]);
  assert.equal(row(rows, "review").text, "Unvalidated. No one has reviewed this result yet.");
  assert.equal(lineageSummary(lineage()), "One part of this analysis's history is not recorded. It is marked in the margin.");
});

test("who asked is the reader, a named colleague, a service account or not recorded, never a guess", () => {
  const person = (principal_id: string, principal_name: string | null, principal_kind: "person" | "service_account" = "person") =>
    describeLineage(lineage({ origin: { origin: "person", basis: "submission", principal_id, principal_name, principal_kind } }), reader);
  assert.equal(row(person(reader, "Ignored"), "asked").text, "You.");
  assert.equal(row(person(colleague, "Rosa Ortiz"), "asked").text, "Rosa Ortiz.");
  assert.equal(row(person(colleague, null), "asked").text, "A colleague.");
  assert.equal(row(person(colleague, "canary-caller", "service_account"), "asked").text, "The service account canary-caller.");
  const earlier = describeLineage(lineage({
    origin: { origin: "unrecorded", basis: "before_origin_records", principal_id: colleague, principal_name: "Rosa Ortiz", principal_kind: "person" },
    gaps: ["origin_before_records", "input_class_not_recorded"],
  }), reader);
  assert.equal(row(earlier, "asked").text, "Not recorded.");
  assert.deepEqual(row(earlier, "asked").notes, ["It runs under Rosa Ortiz's account."]);
  assert.deepEqual(row(earlier, "asked").gaps, [gapSentence.origin_before_records]);
  const backfilled = describeLineage(lineage({ origin: { origin: "research_run", basis: "turn_record", principal_id: colleague,
    principal_name: null, principal_kind: "person", run_id: "r", turn: 1 } }), reader);
  assert.equal(row(backfilled, "asked").text, "The research agent, in decision 1 of a work session a colleague started.");
  assert.deepEqual(row(backfilled, "asked").notes, ["Recorded later from the run's own record of that turn."]);
});

test("a standalone analysis names its missing project inputs instead of inventing them", () => {
  const rows = describeLineage(lineage({
    task: { ...lineage().task, project_id: null, action_key: null },
    origin: { origin: "person", basis: "standalone_task", principal_id: reader, principal_name: null, principal_kind: "person" },
    plan_step: null, inputs: { workspace: null, checkpoint_revision: null, platform_tool: null },
    method: { ...lineage().method, tool_class: "agent_session", template_ref: null }, code: null, outputs: [],
    gaps: ["not_in_project", "no_outputs_recorded"], review: { state: "unvalidated" },
  }), reader);
  assert.deepEqual(rows.map((r) => r.key), ["asked", "data", "method", "ran", "results", "review"]);
  assert.deepEqual(row(rows, "asked").notes, ["Recorded later: an analysis outside a project can be requested only by its own account."]);
  assert.equal(row(rows, "data").text, "Not recorded.");
  assert.deepEqual(row(rows, "data").gaps, [gapSentence.not_in_project]);
  assert.equal(row(rows, "method").text, "Model-written R analysis: a research agent working with this method's tools.");
  assert.equal(row(rows, "results").text, "None recorded.");
  assert.equal(row(rows, "review").text, "Nothing to review: no results were recorded.");
  assert.deepEqual(row(rows, "results").gaps, [gapSentence.no_outputs_recorded]);
});

test("a template analysis without admitted code says it ran none, and unknown gap codes are still shown", () => {
  // As the broker records it: the template is named only through admitted code.
  const method = { ...lineage().method, template_ref: null, template_sha256: null };
  const rows = describeLineage(lineage({ method, code: null, gaps: ["input_class_not_recorded", "code_not_recorded", "future_gap"] }), reader);
  assert.equal(row(rows, "method").text, "Model-written R analysis: model-written R code under a reviewed template.");
  assert.deepEqual(row(rows, "method").gaps, [gapSentence.code_not_recorded]);
  assert.equal(rows.find((r) => r.key === "code"), undefined);
  const other = row(rows, "other");
  assert.deepEqual(other.details, ["future_gap"]);
  assert.equal(other.gaps.length, 1);
  // Every gap is marked on exactly one row, so the summary's count matches the margin.
  assert.equal(rows.reduce((n, r) => n + r.gaps.length, 0), 3);
});

test("names read plainly and an unpinned image is a stated gap", () => {
  assert.equal(templateName("template://r-generated-analysis/v2"), "r-generated-analysis v2");
  assert.equal(imageName("docker.io/library/example-viewer:latest"), "example-viewer");
  const rows = describeLineage(lineage({ gaps: ["image_not_digest_pinned"] }), reader);
  assert.deepEqual(row(rows, "ran").gaps, [gapSentence.image_not_digest_pinned]);
  assert.equal(lineageSummary(lineage({ gaps: [] })), "Every part of this analysis's history is recorded.");
  assert.equal(lineageSummary(lineage({ gaps: ["plan_step_not_recorded", "input_class_not_recorded"] })),
    "2 parts of this analysis's history are not recorded. Each is marked in the margin.");
});

test("the plan step is the one first recorded, a later revision is noted beside it, and a late method record says so", () => {
  const rows = describeLineage(lineage({
    plan_step: { action_key: "fit-model", title: "Compare tooth length by supplement", status: "completed", rationale: null,
      revision: 2, added_after_submission: false, current: { title: "Refit with an interaction term", rationale: null, revision: 5 } },
    method: { ...lineage().method, recorded: "recorded_later" },
    gaps: ["input_class_not_recorded", "method_recorded_later"],
  }), reader);
  assert.equal(row(rows, "step").text, "Compare tooth length by supplement.");
  assert.ok(row(rows, "step").notes.includes("The plan was revised later to read: Refit with an interaction term."));
  assert.deepEqual(row(rows, "step").details, ["First recorded in saved version 2", "Revised in saved version 5"]);
  assert.deepEqual(row(rows, "method").gaps, [gapSentence.method_recorded_later]);
  const added = describeLineage(lineage({ plan_step: { ...lineage().plan_step!, added_after_submission: true, current: null } }), reader);
  assert.ok(row(added, "step").notes.includes("Added to the plan after the analysis was submitted."));
});

test("identifiers, digests and resource figures stay out of the sentences a scientist reads", () => {
  for (const r of describeLineage(lineage(), reader))
    assert.doesNotMatch(`${r.text} ${r.notes.join(" ")}`, /input:\/\/|sha256|[0-9a-f]{12}|CPU|memory|broker|sandbox/);
});

test("a configured recipe names its chosen settings and says the agent wrote no code", () => {
  const base = lineage();
  const configuration = { schema: "enrichedlab.configured-block.v1", block_sha256: `sha256:${"d".repeat(64)}`,
    settings: { groups: "supplement" },
    described: [{ setting: "groups", label: "Groups", choice: "supplement", choice_label: "each delivery method" }] };
  const rows = describeLineage(lineage({
    method: { ...base.method, profile_name: "Bootstrap interval", tool_class: "configured" },
    code: { ...base.code!, configuration },
    outputs: base.outputs.map((o) => ({ ...o, provenance_class: "configured_tool_output" })),
  }), reader);
  assert.equal(row(rows, "method").text, "Bootstrap interval: a configured recipe run with settings the agent chose; the agent wrote no code.");
  assert.equal(row(rows, "code").text, "The configured recipe with its settings filled in (1.4 KB), checked by the platform before it ran.");
  assert.deepEqual(row(rows, "code").notes, ["Groups: each delivery method"]);
  assert.deepEqual(row(rows, "results").notes, ["Made by a configured recipe with settings the agent chose."]);
});

test("the review row names the newest person's decision, what changed since and what a review is not", () => {
  const at = "2026-09-25T12:00:00Z";
  const review = (patch: Partial<TaskReview>): TaskReview => ({ review_id: "r1", task_id: "t1", attempt: 1, decision: "accepted", note: null,
    reviewer_id: colleague, reviewer_name: "Rosa Ortiz", reviewer_kind: "person", outputs: [], created_at: at, covers_current_outputs: true, ...patch });
  const rows = describeLineage(lineage(), reader, [review({ decision: "needs_changes", note: "Pool the doses.", covers_current_outputs: false }), review({})]);
  assert.equal(row(rows, "review").text, `Changes requested by Rosa Ortiz on ${when(at)}.`);
  assert.deepEqual(row(rows, "review").notes, ["The result files changed after this review.", "1 earlier review.",
    "The results stay unvalidated: a review is a person's reading, not statistical validation."]);
  const mine = describeLineage(lineage({ review: { state: "not_reviewed" } }), reader, [review({ reviewer_id: reader })]);
  assert.equal(row(mine, "review").text, `Accepted by you on ${when(at)}.`);
  assert.deepEqual(row(mine, "review").notes, ["A review is a person's reading, not statistical validation."]);
  const machine = describeLineage(lineage({ review: { state: "not_reviewed" } }), reader,
    [review({ reviewer_kind: "service_account", reviewer_name: "Qualification caller" })]);
  assert.equal(row(machine, "review").text, `Accepted by the service account Qualification caller on ${when(at)}.`);
  assert.deepEqual(row(machine, "review").notes, ["A service account recorded this, so it is not a person's review.",
    "A review is a person's reading, not statistical validation."]);
});

test("a query's data row names its source, its kind of data and its snapshot", () => {
  const base = lineage();
  const rows = describeLineage(lineage({
    method: { ...base.method, profile: "reference-sql-fdc-2026-04", profile_name: "FoodData Central database query",
      tool_class: "guarded_query", template_ref: null, template_sha256: null },
    code: null,
    query: { source: "fdc-sr-foundation-2026-04", sha256: `sha256:${"a".repeat(64)}`, guard: "guarded-sql/v1",
      relations: ["food", "food_nutrient"], row_limit: 10, run_id: null, turn: 1, created_at: "2026-09-26T23:02:00Z",
      title: "USDA FoodData Central: SR Legacy and Foundation Foods",
      release: "SR Legacy (April 2018) and Foundation Foods (April 2026)", snapshot_sha256: "76".repeat(32),
      data_class: "public_reference" },
    inputs: { ...base.inputs, classes: ["public_reference"], classes_recorded_by: "query_source" },
    gaps: [],
  }), reader);
  const data = row(rows, "data");
  assert.equal(data.text, "USDA FoodData Central: SR Legacy and Foundation Foods. Public reference data read from one pinned snapshot.");
  assert.deepEqual(data.notes, ["SR Legacy (April 2018) and Foundation Foods (April 2026)."]);
  assert.deepEqual(data.details, [`Snapshot ${"76".repeat(6)}`, "Source fdc-sr-foundation-2026-04"]);
  assert.deepEqual(data.gaps, []);
  assert.equal(row(rows, "method").text,
    "FoodData Central database query: a read-only database query the agent wrote, checked before it ran; no code ran.");
});

test("the classes recorded when the platform sealed the inputs are named and leave no gap", () => {
  const base = lineage();
  const one = lineage({ inputs: { ...base.inputs, classes: ["public_reference"], classes_recorded_by: "input_custodian" }, gaps: [] });
  assert.deepEqual(row(describeLineage(one, reader), "data").notes,
    ["Public reference data, recorded when the platform sealed the copy this analysis read."]);
  assert.deepEqual(row(describeLineage(one, reader), "data").gaps, []);
  assert.equal(lineageSummary(one), "Every part of this analysis's history is recorded.");
  const three = lineage({ inputs: { ...base.inputs, workspace: { input_refs: ["input://a", "input://b", "input://c"] },
    classes: ["object_data", "paper", "public_reference"], classes_recorded_by: "input_custodian" }, gaps: [] });
  assert.deepEqual(row(describeLineage(three, reader), "data").notes,
    ["Files from lab storage, a paper and public reference data, recorded when the platform sealed the copies this analysis read."]);
});

test("a method class this page does not know reads as such, never as undefined", () => {
  const unknown = lineage({ method: { ...lineage().method, tool_class: "future_class" as never } });
  assert.equal(row(describeLineage(unknown, reader), "method").text,
    "Model-written R analysis: a kind of method this page does not describe yet.");
});

test("a figure version names its table, the render service that drew it and where that ran, with no plan step", () => {
  const rows = describeLineage(lineage({
    origin: { origin: "person", basis: "submission", principal_id: reader, principal_name: null, principal_kind: "person" },
    plan_step: null, code: null,
    inputs: { workspace: null, checkpoint_revision: null, platform_tool: null, classes: ["prior_output"], classes_recorded_by: "render_request" },
    method: { profile: "figure-render", profile_name: "Figure from a result table", tool_class: "vetted_service", template_ref: null,
      template_sha256: null, harness: null, role: null, tool_bundle: null },
    render: { source_task: "66666666-6666-4666-8666-666666666666", table_path: "results/condition_summary.csv", table_sha256: "1".repeat(64),
      source_package_sha256: "2".repeat(64), feature: "bar", controls: { category: "condition", value: "mean" }, title: "Mean by diet",
      previous: null, request_sha256: "3".repeat(64), created_at: "2026-09-27T05:00:00Z", service_version: "0.5.20", image_digest: `sha256:${"4".repeat(64)}` },
    outputs: [{ artifact_id: "a", role: "result", label: "Figure package", relative_path: "payload", media_type: "application/gzip",
      size_bytes: 2048, sha256: "0".repeat(64), created_at: "2026-09-27T05:00:05Z", provenance_class: "vetted_tool_output" }],
    review: { state: "not_reviewed" }, gaps: [],
  }), reader);
  assert.deepEqual(rows.map((r) => r.key), ["asked", "data", "method", "ran", "results", "review"]);
  assert.equal(row(rows, "data").text, "Condition summary, a table from an earlier analysis in this project.");
  assert.deepEqual(row(rows, "data").details, [`Table ${"1".repeat(12)}`, `Package ${"2".repeat(12)}`]);
  assert.equal(row(rows, "method").text, "Figure from a result table: the reviewed R render service drew it with the chosen settings; no code was written for it.");
  assert.equal(row(rows, "ran").text, "The reviewed R render service, release 0.5.20, pinned to an exact build.");
  assert.deepEqual(row(rows, "ran").details, [`Image ${"4".repeat(12)}`]);
  assert.deepEqual(row(rows, "results").notes, ["Drawn by the reviewed R render service."]);
});

test("a figure version that was not drawn says so and names no gap; a drawn one without its build names the service", () => {
  const figure = (status: string, image: string | null, outputs: Lineage["outputs"], gaps: string[]) => describeLineage(lineage({
    task: { task_id: "33333333-3333-4333-8333-333333333333", project_id: "44444444-4444-4444-8444-444444444444", action_key: null,
      objective: "Mean by diet", status, result_class: null,
      error_code: status === "failed" ? "figure_refused" : null, attempt: 1, submitted_at: "2026-09-27T05:00:00Z",
      started_at: "2026-09-27T05:00:01Z", finished_at: "2026-09-27T05:00:02Z" },
    origin: { origin: "person", basis: "submission", principal_id: reader, principal_name: null, principal_kind: "person" },
    plan_step: null, code: null,
    inputs: { workspace: null, checkpoint_revision: null, platform_tool: null, classes: ["prior_output"], classes_recorded_by: "render_request" },
    method: { profile: "figure-render", profile_name: "Figure from a result table", tool_class: "vetted_service", template_ref: null,
      template_sha256: null, harness: null, role: null, tool_bundle: null },
    render: { source_task: "66666666-6666-4666-8666-666666666666", table_path: "results/condition_summary.csv", table_sha256: "1".repeat(64),
      source_package_sha256: "2".repeat(64), feature: "bar", controls: { category: "condition", value: "mean" }, title: "Mean by diet",
      previous: null, request_sha256: "3".repeat(64), created_at: "2026-09-27T05:00:00Z", service_version: image ? "0.5.21" : null, image_digest: image },
    outputs, review: { state: "not_reviewed" }, gaps,
  }), reader);
  const refused = figure("failed", null, [], []);
  assert.equal(row(refused, "results").text, "None: the figure was not drawn.");
  assert.equal(row(refused, "review").text, "Nothing to review: the figure was not drawn.");
  assert.equal(row(refused, "ran").text, "The reviewed R render service.");
  assert.ok(refused.every((r) => r.gaps.length === 0));
  const unnamed = figure("completed", null, [{ artifact_id: "a", role: "result", label: "Figure package", relative_path: "payload",
    media_type: "application/gzip", size_bytes: 2048, sha256: "0".repeat(64), created_at: "2026-09-27T05:00:05Z",
    provenance_class: "vetted_tool_output" }], ["image_not_digest_pinned"]);
  assert.deepEqual(row(unnamed, "ran").gaps, ["The render service did not report the exact build that drew this figure."]);
});

test("a template analysis names its table and the R template that ran; the lead's keeps its plan step", () => {
  const analysis = (step: Lineage["plan_step"]) => describeLineage(lineage({
    origin: step ? { origin: "research_run", basis: "submission", principal_id: reader, principal_name: null, principal_kind: "person", turn: 3 }
      : { origin: "person", basis: "submission", principal_id: reader, principal_name: null, principal_kind: "person" },
    plan_step: step, code: null,
    inputs: { workspace: null, checkpoint_revision: null, platform_tool: null, classes: ["prior_output"], classes_recorded_by: "analysis_request" },
    method: { profile: "template-analysis", profile_name: "Reviewed R analyses", tool_class: "vetted_service", template_ref: null,
      template_sha256: null, harness: null, role: null, tool_bundle: null },
    analysis: { source_task: "66666666-6666-4666-8666-666666666666", table_path: "results/lab_data/glucose.csv", table_sha256: "1".repeat(64),
      source_package_sha256: "2".repeat(64), feature: "group_compare", settings: { group: "diet", value: "auc" }, title: "AUC by diet",
      previous: null, request_sha256: "3".repeat(64), created_at: "2026-10-12T05:00:00Z", service_version: "0.6.0",
      image_digest: `sha256:${"4".repeat(64)}`, method: "Welch two-sample t-test" },
    outputs: [{ artifact_id: "a", role: "result", label: "Analysis package", relative_path: "payload", media_type: "application/gzip",
      size_bytes: 2048, sha256: "0".repeat(64), created_at: "2026-10-12T05:00:05Z", provenance_class: "vetted_tool_output" }],
    review: { state: "unvalidated" }, gaps: [],
  }), reader);
  const asked = analysis(null);
  assert.deepEqual(asked.map((r) => r.key), ["asked", "data", "method", "ran", "results", "review"]);
  assert.equal(row(asked, "data").text, "Glucose, a table of the lab's own data in this project.");
  assert.equal(row(asked, "method").text,
    "Reviewed R analyses: run by a reviewed R analysis template (group compare) with the chosen settings; no code was written for it.");
  assert.deepEqual(row(asked, "method").notes, ["Welch two-sample t-test."]);
  assert.equal(row(asked, "ran").text, "The reviewed R render service, release 0.6.0, pinned to an exact build.");
  assert.deepEqual(row(asked, "results").notes, ["Made by a reviewed R analysis template."]);
  const chosen = analysis({ action_key: "compare", title: "Compare AUC by diet", status: "completed", rationale: null });
  assert.deepEqual(chosen.map((r) => r.key), ["asked", "step", "data", "method", "ran", "results", "review"]);
});

test("lab data names the sent file and the broker's own code, and is not reviewed as an analysis", () => {
  const rows = describeLineage(lineage({
    origin: { origin: "person", basis: "submission", principal_id: reader, principal_name: null, principal_kind: "person" },
    plan_step: null, code: null,
    inputs: { workspace: null, checkpoint_revision: null, platform_tool: null, classes: ["prior_output"], classes_recorded_by: "dataset_request" },
    method: { profile: "lab-dataset", profile_name: "Lab data", tool_class: "vetted_service", template_ref: null,
      template_sha256: null, harness: null, role: null, tool_bundle: null },
    dataset: { dataset: "77777777-7777-4777-8777-777777777777", operation: "preparation", file_name: "GTT week 4.xlsx",
      file_sha256: "5".repeat(64), guard_intake: null, guard_policy: null, grid_sha256: "6".repeat(64), spec_sha256: "7".repeat(64),
      previous: null, title: "Glucose", request_sha256: "8".repeat(64), created_at: "2026-10-12T05:00:00Z",
      executor: "enrichedlab.dataset-prepare", executor_version: 2, table_sha256: "9".repeat(64) },
    outputs: [{ artifact_id: "a", role: "result", label: "Prepared table", relative_path: "payload", media_type: "application/gzip",
      size_bytes: 2048, sha256: "0".repeat(64), created_at: "2026-10-12T05:00:05Z", provenance_class: "vetted_tool_output" }],
    review: { state: "unvalidated" }, gaps: [],
  }), reader);
  assert.deepEqual(rows.map((r) => r.key), ["asked", "data", "method", "ran", "results", "review"]);
  assert.equal(row(rows, "data").text, "GTT week 4.xlsx, as the upload guard checked it.");
  assert.equal(row(rows, "method").text,
    "Lab data: checked by the upload guard and read by the broker's own preparation code; no model touched the values.");
  assert.ok(row(rows, "method").details.includes("enrichedlab.dataset-prepare version 2"));
  assert.deepEqual(row(rows, "results").notes, ["Made by the broker's own preparation code."]);
  assert.match(row(rows, "review").text, /^Not reviewed as an analysis/);
});


test("reviewed query lineage names the template method without claiming validation", () => {
  const base = lineage();
  const rows = describeLineage(lineage({ method: { ...base.method, profile_name: "Synthetic query",
    tool_class: "reviewed_query" }, code: null }), reader);
  assert.equal(row(rows, "method").text,
    "Synthetic query: a reviewed database query with typed parameters, checked before it ran; results await validation.");
  assert.match(row(rows, "review").text, /not.*reviewed|await|validated/i);
});
