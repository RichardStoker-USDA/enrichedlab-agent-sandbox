import assert from "node:assert/strict";
import { randomUUID } from "node:crypto";
import type { IncomingMessage, ServerResponse } from "node:http";
import { Readable } from "node:stream";
import test from "node:test";
import { analysisFixtures, brokerWords } from "../fixtures/analyses.ts";
import { ANALYSIS_FEATURES } from "../fixtures/analysis_catalog.ts";
import { csvRows } from "../fixtures/package.ts";
import type { ProjectTask } from "../fixtures/projects.ts";
import { groupCompare, toleranceTest } from "../fixtures/r_stand_in.ts";
import type { Artifact, Task, TaskEvent } from "./contracts.ts";
import {
  analysisStatus, byQuestion, fit, labTableStart, nextStep, offered, outcomeWords, refusedSettings, requestProblem, rounded, settingLines,
  settingsFrom, shownValue, startingSettings, type AnalysisCatalog, type AnalysisReceipt, type TemplateAnalysis,
} from "./template_analysis.ts";

const catalog: AnalysisCatalog = { catalog_sha256: "0".repeat(64), service_version: "0.6.0", image_digest: null, features: ANALYSIS_FEATURES,
  seen_at: "2026-10-12T09:00:00Z", groups: ["lab"] };
const feature = (id: string) => ANALYSIS_FEATURES.find((f) => f.id === id)!;
// A prepared lab table in the long layout, as the lab-table preparation writes it: one row per animal and time.
const longColumns = ["mouse_id", "diet", "sex", "insulin_0_min", "insulin_0_min_censored", "insulin_0_min_limit", "minutes", "glucose", "glucose_censored", "glucose_limit"];
const longNumeric = [false, false, false, true, false, true, true, true, false, true];

test("numbers read rounded, never as 0, and every digit stays on request", () => {
  for (const [text, shown] of [["0.000012345", "1.23 × 10⁻⁵"], ["0", "0"], ["12.3456", "12.35"], ["123456.789", "123456.8"], ["42", "42"],
    ["-0.0004", "-4 × 10⁻⁴"], ["0.0213116", "0.02131"], ["2.5e-7", "2.5 × 10⁻⁷"], ["Welch t-test", "Welch t-test"], ["NA", "NA"]])
    assert.equal(shownValue(text), shown, text);
  assert.equal(rounded("0.0213116437"), true);
  assert.equal(rounded("42"), false);
  assert.equal(rounded("incremental_auc"), false);
});

test("analyses are listed by question, those a table fits first in their words, with what the rest need", () => {
  const questions = byQuestion(catalog).map((q) => q.title);
  assert.deepEqual(questions, ["Describe a column", "Compare groups", "Animals over time", "Relate columns", "Genetic scans", "Plan a study"]);
  const extra = { ...catalog, features: [...ANALYSIS_FEATURES, { id: "new_test", label: "A new test", description: "does something", params: {} }] };
  assert.deepEqual(byQuestion(extra).at(-1)?.features.map((f) => f.id), ["new_test"]);
  assert.equal(fit(feature("tolerance_test_auc"), longColumns, longNumeric).fits, true);
  assert.equal(fit(feature("group_compare"), longColumns, longNumeric).fits, true);
  const narrow = fit(feature("correlation"), ["mouse_id", "diet", "glucose"], [false, false, true]);
  assert.deepEqual(narrow, { fits: false, why: "Needs 2 numeric columns this table does not have." });
  assert.deepEqual(fit(feature("correlation_fdr_matrix"), ["mouse_id", "glucose"], [false, true]), { fits: false, why: "Needs at least 2 numeric columns; this table has 1." });
  assert.equal(fit(feature("power_calc"), ["n"], [true]).fits, true, "a power calculation reads no column");
  const listed = offered(catalog, longColumns, longNumeric).find((q) => q.title === "Animals over time")!;
  assert.deepEqual(listed.features.map((f) => f.feature.id), ["tolerance_test_auc", "repeated_measures_mixed", "animal_window_summary", "body_composition_delta"]);
});

test("an analysis starts with an ID for the animal, a time-like column for time and measures for values", () => {
  assert.deepEqual(startingSettings(feature("tolerance_test_auc"), longColumns, longNumeric), { id: "mouse_id", time: "minutes", value: "insulin_0_min" });
  assert.deepEqual(startingSettings(feature("group_compare"), longColumns, longNumeric), { value: "insulin_0_min", group: "diet" });
  assert.deepEqual(startingSettings(feature("anova_emmeans"), longColumns, longNumeric), { outcome: "insulin_0_min", factors: ["diet"] });
  // A prepared table in the long layout starts on the tolerance test with its own animal, points, values and groups.
  const spec = { long: { name: "minutes", values: [0, 15, 30, 60, 120], value: { name: "glucose" } }, key: ["mouse_id", "minutes"],
    columns: [{ name: "mouse_id", type: "text" }, { name: "diet", type: "category" }, { name: "sex", type: "category" }, { name: "insulin_0_min", type: "number" }] };
  assert.deepEqual(labTableStart(spec, catalog, "GTT week 4"), { feature: "tolerance_test_auc",
    settings: { id: "mouse_id", time: "minutes", value: "glucose", group: "diet" }, title: "Tolerance test areas of GTT week 4", previous: null });
  assert.deepEqual(labTableStart({ ...spec, long: { ...spec.long, values: ["baseline", "week 4"] } }, catalog, "")?.settings,
    { id: "mouse_id", time: "minutes", value: "glucose", factors: ["diet", "sex"] });
  assert.equal(labTableStart({ ...spec, long: null }, catalog, ""), null);
});

test("the form becomes settings the broker checks again, and each value it cannot send is named", () => {
  const tolerance = feature("tolerance_test_auc");
  assert.deepEqual(settingsFrom(tolerance, { id: "mouse_id", time: "minutes", value: "glucose", upper_limit: "600", test: "" }, longColumns),
    { settings: { id: "mouse_id", time: "minutes", value: "glucose", upper_limit: 600 }, problems: [] });
  assert.deepEqual(settingsFrom(tolerance, { id: "mouse_id", time: "minutes", upper_limit: "0" }, longColumns).problems,
    ["Choose a column for reading.", "Meter's highest reading must be between 1 and 100000."]);
  assert.deepEqual(settingsFrom(feature("anova_emmeans"), { outcome: "glucose", factors: ["diet", "nowhere"], interaction: "false" }, longColumns).settings,
    { outcome: "glucose", factors: ["diet"], interaction: "false" });
  assert.deepEqual(settingsFrom(feature("anova_emmeans"), { outcome: "glucose", factors: [] }, longColumns).problems, ["Choose at least one column for factors."]);
  assert.deepEqual(settingLines(feature("group_compare"), { value: "glucose", group: "diet", test: "t", experimental_unit: "mouse_id" }).map((l) => [l.label, l.value]),
    [["Values", "glucose"], ["Groups", "diet"], ["One row for each", "mouse_id"], ["Test", "Welch t-test"]]);
});

test("a refusal points at the settings whose columns it names, in the broker's words", () => {
  const settings = { value: "glucose", group: "diet", experimental_unit: "mouse_id" };
  assert.deepEqual(refusedSettings(feature("group_compare"), settings, { code: "repeated_rows_per_animal", columns: ["mouse_id"], counts: [8] }), ["experimental_unit"]);
  assert.deepEqual(refusedSettings(feature("group_compare"), settings, { code: "no_rows", columns: [], counts: [] }), []);
  const refused: AnalysisReceipt = { attempt: 1, status: "refused", error_code: "analysis_refused", refusal: null, service_version: "0.6.0", image_digest: null,
    message: "8 animals in mouse_id have more than one row; summarize each animal first, so animals and not hours or days are compared.", method: null,
    files: null, notes: [], package_sha256: null, started_at: "", finished_at: "" };
  assert.equal(outcomeWords(refused, "failed"), `Not run. ${refused.message}`);
  assert.deepEqual(analysisStatus("failed", refused), { label: "Not run", tone: "failed" });
  assert.equal(outcomeWords({ ...refused, status: "failed", error_code: "render_service_outdated", message: null }, "failed"),
    "Not run: the analysis service is older than the release that keeps every digit of the results.");
  assert.equal(requestProblem(403, "analysis_method_unavailable"), "Reviewed R analyses are not set up for this lab yet.");
  assert.equal(requestProblem(429, "group_queue_full"), "The lab's queue is full. Try again in a few minutes.");
  assert.equal(brokerWords("repeated_rows_per_animal", ["mouse_id"], [8]), refused.message);
  assert.equal(brokerWords("no_numeric_values", ["a", "b", "c"], []), "No numbers this analysis can use in a, b and c.");
  assert.equal(brokerWords("too_few_time_points", ["minutes"], [2]), null, "a place left empty gives the general words");
});

test("a tolerance test's table leads to the comparison it is made for, of animals and not readings", () => {
  const analysis = { feature: "tolerance_test_auc", settings: { id: "mouse_id", time: "minutes", value: "glucose", group: "diet" } } as unknown as TemplateAnalysis;
  const stats = new Map([["primary_endpoint", "incremental_auc"], ["groups", "2"]]);
  assert.deepEqual(nextStep(analysis, stats, catalog, "results/lab_derived/results.csv"), { label: "Compare the 2 groups on incremental auc",
    table: "results/lab_derived/results.csv", feature: "group_compare", settings: { value: "incremental_auc", group: "diet", experimental_unit: "mouse_id" },
    title: "Incremental auc by diet" });
  assert.equal(nextStep(analysis, new Map([["primary_endpoint", "incremental_auc"], ["groups", "3"]]), catalog, "results/results.csv")?.feature, "anova_emmeans");
  assert.equal(nextStep({ ...analysis, settings: { id: "mouse_id", time: "minutes", value: "glucose" } }, stats, catalog, "results/results.csv"), null, "no groups, no comparison");
});

// A small long table: five animals in two diets, read at 0, 30 and 120 minutes; one animal misses its last reading.
const gtt = { columns: ["mouse_id", "diet", "minutes", "glucose"], rows: [
  ["M1", "Control", "0", "100"], ["M1", "Control", "30", "200"], ["M1", "Control", "120", "120"],
  ["M2", "Control", "0", "90"], ["M2", "Control", "30", "180"], ["M2", "Control", "120", "NA"],
  ["M5", "Control", "0", "95"], ["M5", "Control", "30", "190"], ["M5", "Control", "120", "110"],
  ["M3", "High fat", "0", "110"], ["M3", "High fat", "30", "300"], ["M3", "High fat", "120", "150"],
  ["M4", "High fat", "0", "120"], ["M4", "High fat", "30", "60"], ["M4", "High fat", "120", "130"]] };

test("the stand-in follows the tolerance template: trapezoid areas per animal, split where a curve crosses its first reading", () => {
  const out = toleranceTest(gtt, { id: "mouse_id", time: "minutes", value: "glucose", group: "diet" });
  assert.equal(out.refusal, undefined);
  const rows = out.results!.rows;
  assert.deepEqual(rows.map((r) => [r[0], r[2], r[3]]), [["M1", true, null], ["M2", false, "no reading at the last time"], ["M5", true, null],
    ["M3", true, null], ["M4", true, null]], "sorted by group, then animal");
  const col = (name: string) => out.results!.columns.indexOf(name);
  // M1: (30 x (100 + 200) / 2) + (90 x (200 + 120) / 2) = 4500 + 14400; above 100: 30 x 100 / 2 + 90 x (100 + 20) / 2.
  assert.equal(rows[0][col("total_auc")], 18900);
  assert.equal(rows[0][col("incremental_auc")], 6900);
  // M4 dips below its first reading, 120, and comes back: the segments split where they cross it.
  assert.equal(rows[4][col("area_below_baseline")], (30 * 60 * 60) / (2 * 60) + (90 * 60 * 60) / (2 * 70));
  assert.equal(rows[4][col("incremental_auc")], (90 * 10 * 10) / (2 * 70));
  assert.deepEqual(out.statistics, [["test", "glucose tolerance test"], ["primary_endpoint", "incremental_auc"], ["animals", 5], ["animals_used", 4],
    ["times", 3], ["first_time", 0], ["last_time", 120], ["readings_bridged", 0], ["groups", 2]]);
  assert.match(out.notes![0], /^4 of 5 animals used\. Listed and not used: 1 without one at the last time\./);
  assert.deepEqual(toleranceTest(gtt, { id: "mouse_id", time: "minutes", value: "glucose", group: "minutes" }).refusal,
    { code: "factor_varies_within_animal", columns: ["mouse_id", "minutes"], counts: [5] });
});

test("the stand-in's Welch test matches R's t.test, and readings of one animal are refused as animals", () => {
  const out = groupCompare({ columns: ["g", "v"], rows: [["a", "1"], ["a", "2"], ["a", "3"], ["b", "4"], ["b", "5"], ["b", "6"]] }, { value: "v", group: "g", test: "t" });
  const stat = Object.fromEntries(out.statistics!) as Record<string, number>;
  // R 4.4: t.test(c(1, 2, 3), c(4, 5, 6)) gives t = -3.6742, df = 4, p-value = 0.02131, 95 percent interval -5.266958 to -0.733042.
  assert.equal(stat.statistic.toFixed(6), "-3.674235");
  assert.equal(stat.df, 4);
  assert.equal(stat.p_value.toFixed(7), "0.0213116");
  assert.equal(stat.ci_low.toFixed(5), "-5.26696");
  assert.equal(stat.ci_high.toFixed(5), "-0.73304");
  assert.equal(stat.cohens_d, -3);
  assert.deepEqual(groupCompare(gtt, { value: "glucose", group: "diet", experimental_unit: "mouse_id" }).refusal,
    { code: "repeated_rows_per_animal", columns: ["mouse_id"], counts: [5] });
  assert.deepEqual(groupCompare(gtt, { value: "glucose", group: "minutes" }).refusal, { code: "group_count_not_two", columns: ["minutes"], counts: [3] });
});

function setup() {
  const tasks: Task[] = [], histories = new Map<string, TaskEvent[]>(), added: ProjectTask[] = [];
  const project = randomUUID(), sourceTask = randomUUID(), otherProject = randomUUID(), otherTask = randomUUID();
  const csv = (t: typeof gtt) => Buffer.from([t.columns.join(","), ...t.rows.map((r) => r.join(","))].join("\n") + "\n");
  const artifact = { id: randomUUID(), sha256: "0".repeat(64) } as Artifact;
  let status = "completed";
  const fx = analysisFixtures({ me: "reader", profile: "template_analysis", tasks, histories, groups: ["lab"], delay: 5, sources: () => [
    { project, group: "lab", sourceTask, source: () => ({ files: [{ path: "results/lab_data/gtt.csv", bytes: csv(gtt) }], artifact }),
      task: () => ({ status, objective: "GTT" }), addProjectTask: (entry) => added.push(entry) },
    { project: otherProject, group: "field", sourceTask: otherTask, source: () => ({ files: [{ path: "results/plots.csv", bytes: csv(gtt) }], artifact }),
      task: () => ({ status: "completed", objective: "Plots" }), addProjectTask: (entry) => added.push(entry) },
  ] });
  async function call(method: string, path: string, body?: unknown) {
    const req = Object.assign(Readable.from(body === undefined ? [] : [Buffer.from(JSON.stringify(body))]), { method, headers: { "x-agent-sandbox-intent": "mutate" } }) as unknown as IncomingMessage;
    let out: { status: number; body: any } = { status: 0, body: null };
    const res = { setHeader() {}, end(bytes: Buffer) { out = { status: 200, body: bytes }; } };
    await fx.handle(req, res as unknown as ServerResponse, new URL(`http://fixture${path}`), (value, code = 200) => { out = { status: code, body: value }; }, () => { out = { status: 404, body: "denied" }; });
    return out;
  }
  const settle = async (task: string) => {
    for (let i = 0; i < 200; i++) {
      const d = (await call("GET", `/api/v1/tasks/${task}/template-analysis`)).body.template_analysis;
      if (d?.receipts.length) return d as TemplateAnalysis;
      await new Promise((r) => setTimeout(r, 5));
    }
    throw new Error("the analysis did not finish");
  };
  return { fx, tasks, added, project, sourceTask, otherProject, otherTask, call, settle, setStatus: (s: string) => { status = s; } };
}

test("the preview checks a request as the broker does, once per request id", async () => {
  const { fx, project, sourceTask, otherProject, otherTask, call, setStatus } = setup();
  const ask = (more: Record<string, unknown> = {}) => ({ source_task: sourceTask, table_path: "results/lab_data/gtt.csv", feature: "tolerance_test_auc",
    settings: { id: "mouse_id", time: "minutes", value: "glucose", group: "diet" }, title: "", previous: null, request_id: randomUUID(), ...more });
  const catalogRead = (await call("GET", "/api/v1/template-analyses/catalog")).body.catalog;
  assert.deepEqual([catalogRead.groups, catalogRead.features.length], [["lab"], 18]);
  const code = async (body: unknown, at = project) => { const out = await call("POST", `/api/v1/projects/${at}/template-analyses`, body); return [out.status, out.body?.error?.code ?? null]; };
  assert.deepEqual(await code(ask({ title: "t".repeat(201) })), [400, "analysis_request_invalid"]);
  assert.deepEqual(await code(ask({ table_path: "results/other.csv" })), [400, "analysis_table_unknown"]);
  assert.deepEqual(await code(ask({ feature: "no_such" })), [400, "analysis_feature_unknown"]);
  assert.deepEqual(await code(ask({ settings: { id: "mouse_id", time: "minutes", value: "weight" } })), [400, "analysis_settings_invalid"]);
  assert.deepEqual(await code(ask({ settings: { id: "mouse_id", time: "minutes" } })), [400, "analysis_settings_invalid"]);
  assert.deepEqual(await code(ask({ settings: { id: "mouse_id", time: "minutes", value: "glucose", colour: "red" } })), [400, "analysis_settings_invalid"]);
  assert.deepEqual(await code(ask({ previous: randomUUID() })), [400, "analysis_previous_invalid"]);
  assert.deepEqual(await code(ask({ source_task: otherTask, table_path: "results/plots.csv" }), otherProject), [403, "analysis_method_unavailable"]);
  assert.deepEqual(await code(ask(), randomUUID()), [404, "project_not_found"]);
  setStatus("running");
  assert.deepEqual(await code(ask()), [409, "analysis_source_not_ready"]);
  setStatus("completed");
  const body = ask();
  const first = await call("POST", `/api/v1/projects/${project}/template-analyses`, body);
  assert.equal(first.status, 202);
  assert.equal((await call("POST", `/api/v1/projects/${project}/template-analyses`, body)).body.task_id, first.body.task_id);
  assert.deepEqual(await code({ ...body, title: "Another" }), [409, "idempotency_conflict"]);
  fx.reset();
});

test("a run keeps its results under lab_derived for a lab table, its statistics and receipt, and refuses in the broker's words", async () => {
  const { fx, tasks, added, project, sourceTask, call, settle } = setup();
  const sent = await call("POST", `/api/v1/projects/${project}/template-analyses`, { source_task: sourceTask, table_path: "results/lab_data/gtt.csv",
    feature: "tolerance_test_auc", settings: { id: "mouse_id", time: "minutes", value: "glucose", group: "diet" }, title: "", previous: null, request_id: randomUUID() });
  const done = await settle(sent.body.task_id);
  assert.equal(tasks[0].objective, "Glucose or insulin tolerance test: areas per animal of gtt");
  assert.deepEqual(added[0].template_analysis, { title: "", feature: "tolerance_test_auc", source_task: sourceTask, table_path: "results/lab_data/gtt.csv", previous: null });
  const receipt = done.receipts[0];
  assert.equal(receipt.status, "completed");
  assert.deepEqual(receipt.files?.map((f) => [f.path, f.role]), [["figures/figure.svg", "figure"], ["results/lab_derived/results.csv", "table"],
    ["results/record.json", "record"], ["results/statistics.csv", "statistics"]]);
  assert.match(receipt.method ?? "", /^glucose tolerance test per animal: trapezoid areas/);
  const artifacts = (await call("GET", `/api/v1/tasks/${done.task_id}/artifacts`)).body.artifacts as Artifact[];
  const file = async (path: string) => (await call("GET", `/api/v1/tasks/${done.task_id}/artifacts/${artifacts[0].id}/contents/file?path=${encodeURIComponent(path)}`)).body.toString();
  const results = csvRows(await file("results/lab_derived/results.csv"));
  assert.deepEqual(results[0].slice(0, 4), ["mouse_id", "diet", "included", "excluded_reason"]);
  assert.deepEqual(results[2].slice(0, 5), ["M2", "Control", "FALSE", "no reading at the last time", "NA"], "NA unquoted, logicals TRUE and FALSE");
  assert.match(await file("results/statistics.csv"), /^statistic,value\ntest,glucose tolerance test\nprimary_endpoint,incremental_auc\n/);
  // An analysis of the results: the group comparison of animals, then the same on readings, which the template refuses.
  const compare = await call("POST", `/api/v1/projects/${project}/template-analyses`, { source_task: done.task_id, table_path: "results/lab_derived/results.csv",
    feature: "group_compare", settings: { value: "incremental_auc", group: "diet", experimental_unit: "mouse_id" }, title: "Incremental area by diet", previous: null,
    request_id: randomUUID() });
  assert.equal(compare.status, 202);
  assert.equal((await settle(compare.body.task_id)).receipts[0].status, "completed");
  assert.deepEqual((await call("GET", `/api/v1/tasks/${done.task_id}/template-analyses`)).body.template_analyses.map((v: { task_id: string }) => v.task_id), [compare.body.task_id]);
  const readings = await call("POST", `/api/v1/projects/${project}/template-analyses`, { source_task: sourceTask, table_path: "results/lab_data/gtt.csv",
    feature: "group_compare", settings: { value: "glucose", group: "diet", experimental_unit: "mouse_id" }, title: "", previous: done.task_id, request_id: randomUUID() });
  assert.equal(readings.status, 202);
  const refused = (await settle(readings.body.task_id)).receipts[0];
  assert.deepEqual([refused.status, refused.error_code, refused.message, refused.refusal], ["refused", "analysis_refused",
    "5 animals in mouse_id have more than one row; summarize each animal first, so animals and not hours or days are compared.",
    { code: "repeated_rows_per_animal", columns: ["mouse_id"], counts: [5] }]);
  assert.deepEqual((await call("GET", `/api/v1/tasks/${randomUUID()}/template-analysis`)).status, 404);
  fx.reset();
  assert.equal(tasks.length, 0);
});
