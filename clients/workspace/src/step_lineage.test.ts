import assert from "node:assert/strict";
import test from "node:test";
import type { StepLineage, StepLineageNode } from "./contracts.ts";
import { lineageSummary } from "./lineage.ts";
import { shapeStepLineage } from "./step_lineage.ts";

const ROOT = "10000000-0000-4000-8000-000000000001";
const ANALYSIS = "10000000-0000-4000-8000-000000000002";
const QUERY = "10000000-0000-4000-8000-000000000003";
const digest = (letter: string) => letter.repeat(64);

function method(patch: Partial<StepLineageNode["method"]> = {}): StepLineageNode["method"] {
  return {
    profile: "reviewed-method-v1", profile_name: "Reviewed method", tool_class: "vetted_service",
    template_ref: null, template_sha256: null, recorded: "submission", harness: null, role: null, tool_bundle: null,
    ...patch,
  };
}

function node(id: string, title: string, patch: Partial<StepLineageNode> = {}): StepLineageNode {
  return {
    task_id: id, title, method_class: "vetted_service", method: method(), attempts: [], outputs: [],
    review: { state: "not_reviewed" }, gaps: [], ...patch,
  };
}

function response(patch: Partial<StepLineage["upstream"]> = {}): StepLineage {
  return {
    schema: "enrichedlab.task-lineage.v3",
    task: { task_id: ROOT, project_id: null, action_key: null, objective: "Publication figure", status: "completed",
      result_class: "success", error_code: null, attempt: 1, submitted_at: "2026-10-15T00:00:00Z",
      started_at: "2026-10-15T00:00:01Z", finished_at: "2026-10-15T00:00:02Z" },
    origin: { origin: "person", basis: "submission", principal_id: "20000000-0000-4000-8000-000000000001",
      principal_name: "Example researcher", principal_kind: "person" },
    plan_step: null,
    inputs: { workspace: null, checkpoint_revision: null, platform_tool: null, classes: ["prior_output"], classes_recorded_by: "render_request" },
    method: method(), code: null, query: null, execution: { image_ref: null, cpu_limit: null, memory_limit: null,
      max_run_seconds: null, attempts: 1, scientific_history: 0 }, outputs: [], review: { state: "not_reviewed" }, gaps: [], attempts: [],
    upstream: { nodes: [node(ROOT, "Publication figure")], edges: [], sources: [], withheld: 0,
      limits: { max_depth: 16, max_nodes: 200, truncated: false, reason: null }, ...patch },
  };
}

test("a figure opens through a reviewed analysis and query to the recorded FDC release and file", () => {
  const figure = node(ROOT, "Publication figure", { method: method({ render: {
    source_task: ANALYSIS, table_path: "results/summary.csv", table_sha256: digest("a"), source_package_sha256: digest("b"),
    feature: "bar", controls: {}, title: "Publication figure", previous: null, request_sha256: digest("c"),
    created_at: "2026-10-15T00:00:00Z", service_version: "0.6.0", image_digest: `sha256:${digest("d")}`,
  } }) });
  const analysis = node(ANALYSIS, "Summarize food components", { method: method({ profile_name: "Reviewed nutrient summary", analysis: {
    source_task: QUERY, table_path: "results/query.csv", table_sha256: digest("e"), source_package_sha256: digest("f"),
    feature: "summary_stats", settings: {}, title: "Summarize food components", previous: null, request_sha256: digest("1"),
    created_at: "2026-10-15T00:00:00Z", service_version: "1.4.0", image_digest: `sha256:${digest("2")}`,
    method: "Reviewed descriptive summary",
  } }) });
  const query = node(QUERY, "Read the Foundation Foods snapshot", { method_class: "guarded_query", method: method({
    profile_name: "FoodData Central query", tool_class: "guarded_query", query: {
      source: "fdc-foundation-2026-04-30", sha256: digest("3"), guard: "guarded-sql/v1", relations: ["food"], row_limit: 100,
      run_id: null, turn: null, created_at: "2026-10-15T00:00:00Z", source_version: null,
    },
  }) });
  const view = shapeStepLineage(response({
    nodes: [figure, analysis, query],
    edges: [
      { from: ANALYSIS, to: ROOT, output: "results/summary.csv", digest: digest("e"), recorded_by: "render_request" },
      { from: QUERY, to: ANALYSIS, output: "results/query.csv", digest: digest("3"), recorded_by: "analysis_request" },
    ],
    sources: [{ kind: "guarded_query", node: QUERY, source: "fdc-foundation-2026-04-30", profile: "fdc-query-v1",
      schema: "fdc_foundation", snapshot: { dataset: "FoodData Central Foundation Foods", release: "2026-04-30",
        files: [{ file_name: "FoodData_Central_foundation_food_csv_2026-04-30.zip", sha256: digest("4") }], manifest_sha256: digest("5") } }],
  }));

  const made = view.inputs[0];
  assert.deepEqual([made.title, made.kind, made.method?.name, made.method?.version],
    ["Summarize food components", "Reviewed analysis", "Reviewed descriptive summary", "1.4.0"]);
  const queried = made.inputs[0];
  assert.deepEqual([queried.title, queried.kind, queried.method?.version],
    ["Read the Foundation Foods snapshot", "Database query", "guarded-sql/v1"]);
  const source = queried.inputs[0];
  assert.deepEqual(source.path, ["Publication figure", "Summarize food components", "Read the Foundation Foods snapshot", "FoodData Central Foundation Foods"]);
  assert.deepEqual(source.notes, ["Release 2026-04-30.", "Source file FoodData_Central_foundation_food_csv_2026-04-30.zip."]);
});

test("refused producers and attempt gaps stay on the affected step without claiming a complete leaf", () => {
  const producer = node(ANALYSIS, "Earlier preparation", {
    gaps: ["method_not_recorded"],
    attempts: [{
      attempt: 2,
      execution: null,
      outputs: [],
      gaps: ["attempt_inputs_not_recorded", "attempt_execution_not_recorded", "attempt_outputs_not_linked", "future_gap"],
    }],
  });
  const view = shapeStepLineage(response({
    nodes: [node(ROOT, "Publication figure"), producer],
    edges: [
      { from: ANALYSIS, to: ROOT, output: "results/prepared.csv", digest: digest("6"), recorded_by: "dataset_request" },
      { from: QUERY, to: ROOT, output: "results/missing.csv", digest: digest("7"), recorded_by: "input_custodian" },
    ],
    withheld: 1,
  }));
  assert.match(view.incomplete ?? "", /Earlier history is not fully shown/);
  assert.match(view.incomplete ?? "", /1 recorded input comes from a step this account cannot read/);
  assert.match(view.incomplete ?? "", /Where it attaches is not shown/);
  assert.equal(view.inputs[1].type, "missing");
  assert.match(view.inputs[1].notes[0], /not included/);
  assert.match(view.inputs[0].method?.receipt ?? "", /not recorded/);
  assert.ok(view.inputs[0].notes.includes("The method was not recorded."));
  assert.ok(view.inputs[0].notes.includes("Attempt 2: The copies sealed for this attempt were not recorded."));
  assert.ok(view.inputs[0].notes.includes("Attempt 2: What the worker bound for this attempt was not recorded."));
  assert.ok(view.inputs[0].notes.includes("Attempt 2: The attempt's output registrations are not linked here."));
  assert.ok(view.inputs[0].notes.includes("Attempt 2: The record names another missing part that this page does not describe yet."));
  assert.ok(view.inputs[0].notes.includes("No earlier input or terminal source is shown for this step."));
  assert.ok(!view.inputs[0].notes.some((note) => note.includes("future_gap")));
  assert.deepEqual(
    view.inputs[0].technical.filter((fact) => fact.label.toLowerCase().includes("missing-part")),
    [
      { label: "Attempt 2 missing-part codes", value: "attempt_inputs_not_recorded, attempt_execution_not_recorded, attempt_outputs_not_linked, future_gap" },
      { label: "Missing-part codes", value: "method_not_recorded" },
    ],
  );
});

test("aggregate refusals and truncation qualify the root, every leaf and the lineage summary", () => {
  const rootOnly = response({
    withheld: 2,
    limits: { max_depth: 7, max_nodes: 200, truncated: true, reason: "depth" },
  });
  const rootView = shapeStepLineage(rootOnly);
  assert.equal(rootView.empty, "No earlier input or terminal source is shown for this step.");
  assert.match(rootView.incomplete ?? "", /2 recorded inputs come from one or more steps this account cannot read/);
  // The count is of input records; several can share one producer, so no number of steps or producers is claimed.
  assert.doesNotMatch(rootView.incomplete ?? "", /\b2 (withheld )?(producers?|steps?)\b|those steps|that step/i);
  assert.match(rootView.incomplete ?? "", /depth limit of 7/);
  assert.doesNotMatch(lineageSummary(rootOnly), /Every part/);
  assert.match(lineageSummary(rootOnly), /Earlier history is not fully shown.*2 recorded inputs come from one or more steps.*depth limit of 7/);

  const nodeLimited = response({
    limits: { max_depth: 16, max_nodes: 23, truncated: true, reason: "nodes" },
  });
  const nodeLimitedView = shapeStepLineage(nodeLimited);
  assert.equal(nodeLimitedView.empty, "No earlier input or terminal source is shown for this step.");
  assert.match(nodeLimitedView.incomplete ?? "", /node limit of 23 steps/);
  assert.doesNotMatch(lineageSummary(nodeLimited), /Every part/);
});

test("root attempt gaps are named in words and keep unknown codes under Technical details", () => {
  const body = response();
  body.attempts = [{
    attempt: 1,
    inputs: [],
    execution: null,
    outputs: [],
    validation: { state: "not_reviewed" },
    gaps: ["attempt_execution_not_recorded", "future_root_gap"],
  }];
  const view = shapeStepLineage(body);
  assert.deepEqual(view.rootGaps, [
    "Attempt 1: What the worker bound for this attempt was not recorded.",
    "Attempt 1: The record names another missing part that this page does not describe yet.",
  ]);
  assert.deepEqual(view.rootTechnical, [{
    label: "Attempt 1 missing-part codes",
    value: "attempt_execution_not_recorded, future_root_gap",
  }]);
  assert.match(lineageSummary(body), /^2 parts .* are not recorded/);
  assert.doesNotMatch(lineageSummary(body), /Every part/);
});

test("live lab and uploaded sources keep their recorded snapshots and guard outcomes", () => {
  const read = node(ROOT, "Read the lab source", { method_class: "guarded_query", method: method({
    profile_name: "Read the lab source", tool_class: "guarded_query", query: {
      source: "lab-measurements", sha256: digest("b"), guard: "guarded-sql/v1", relations: ["measurement"], row_limit: 100,
      run_id: null, turn: null, created_at: "2026-10-15T00:00:00Z", title: "Lab measurements", release: "Live lab database",
      data_class: "lab_database", snapshot_kind: "live", source_version: { schema_version: "0021", newest_import_id: 7 },
    },
  }) });
  const view = shapeStepLineage(response({
    nodes: [read],
    sources: [
      { kind: "guarded_query", node: ROOT, source: "lab-measurements", profile: "lab-sql-v1", schema: "lab",
        snapshot: { kind: "live", version_relation: "database_version", declaration_sha256: digest("c") } },
      { kind: "lab_dataset", node: ROOT, dataset: "30000000-0000-4000-8000-000000000001", file_name: "observations.csv",
        sha256: digest("d"), size_bytes: 240, guard: { decision: "allowed", policy: "upload-policy-v1" } },
    ],
  }));

  assert.deepEqual([view.inputs[0].title, view.inputs[0].kind], ["Lab measurements", "Live lab source"]);
  assert.ok(view.inputs[0].notes.includes("The query receipt records the live source version used."));
  assert.match(view.inputs[0].technical.find((fact) => fact.label === "Source version")?.value ?? "", /schema_version/);
  assert.deepEqual([view.inputs[1].title, view.inputs[1].kind], ["observations.csv", "Uploaded dataset"]);
  assert.equal(view.inputs[1].notes[0], "The upload guard recorded its decision as allowed.");
});

test("a recorded cycle stops at the repeated step and keeps the visible path back", () => {
  const view = shapeStepLineage(response({
    nodes: [node(ROOT, "Figure"), node(ANALYSIS, "Analysis")],
    edges: [
      { from: ANALYSIS, to: ROOT, output: "results/a.csv", digest: digest("8"), recorded_by: "render_request" },
      { from: ROOT, to: ANALYSIS, output: "results/b.csv", digest: digest("9"), recorded_by: "analysis_request" },
    ],
  }));
  const cycle = view.inputs[0].inputs[0];
  assert.equal(cycle.type, "cycle");
  assert.deepEqual(cycle.path, ["Figure", "Analysis", "Figure"]);
  assert.match(cycle.notes[0], /returns to a step already on this path/);
  assert.deepEqual(cycle.inputs, []);
});

test("client shaping stops a malformed response beyond the broker's depth bound", () => {
  const nodes = [node(ROOT, "Step 0")];
  const edges: StepLineage["upstream"]["edges"] = [];
  let consumer = ROOT;
  for (let depth = 1; depth <= 18; depth++) {
    const id = `11000000-0000-4000-8000-${String(depth).padStart(12, "0")}`;
    nodes.push(node(id, `Step ${depth}`));
    edges.push({ from: id, to: consumer, output: `results/${depth}.csv`, digest: digest("a"), recorded_by: "input_custodian" });
    consumer = id;
  }
  const view = shapeStepLineage(response({ nodes, edges }));
  let item = view.inputs[0];
  let steps = 1;
  while (item.inputs[0]?.type === "step") { item = item.inputs[0]; steps++; }
  assert.equal(steps, 16);
  assert.equal(item.inputs[0].type, "limit");
  assert.match(view.incomplete ?? "", /display limit/);
});
