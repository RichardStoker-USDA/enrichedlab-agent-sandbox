import assert from "node:assert/strict";
import { randomUUID } from "node:crypto";
import { readFileSync } from "node:fs";
import type { IncomingMessage, ServerResponse } from "node:http";
import { Readable } from "node:stream";
import test from "node:test";
import { datasetFixtures, gridOf, prepare, profileGrid, specProblem, suggestedName } from "../fixtures/datasets.ts";
import type { ProjectTask } from "../fixtures/projects.ts";
import type { Task, TaskEvent } from "./contracts.ts";
import {
  cellsLine, columnChanges, guardWords, lettersWords, outputsOf, preparationRefusal, rowRole, rowsLine, rowsWords, setLong, setWide,
  specProblems, specRefusalWords, startingSpec, suggestedRecode, toggleColumn, toggleRow, unreadWords, uploadProblem,
  type Profile, type ProfileDetail, type PreparationSummary, type Spec,
} from "./lab_data.ts";

// The preview's simulated glucose tolerance sheet: titles, names and units rows, eight animals, a summary and a note.
const bytes = readFileSync(new URL("../fixtures/lab_data/gtt_week4_simulated.csv", import.meta.url));
const grid = gridOf(bytes, ",");
const read = profileGrid(grid, { type: "csv", sha256: "0".repeat(64), bytes: bytes.length, delimiter: "," }, null) as unknown as { profile: Profile; detail: ProfileDetail };
const sheet = read.profile.sheets[0];

test("the stand-in profiler finds the table, its names and units, and what to decide", () => {
  assert.deepEqual(sheet.table, { header_rows: [3], header_basis: "labels_over_numbers", units_row: 4, title_rows: [1, 2], first_data_row: 5,
    last_data_row: 14, data_rows: 8, blank_rows: 0, summary_rows: [13, 14], note_rows: [15] });
  assert.deepEqual(sheet.columns.map((c) => [c.letter, c.suggested_name, c.type, c.unit, c.unit_from]), [
    ["A", "mouse_id", "text", null, null], ["B", "diet", "text", null, null], ["C", "sex", "text", null, null],
    ["D", "glucose_0_min", "integer", "mg/dL", "units_row"], ["E", "glucose_15_min", "integer", "mg/dL", "units_row"],
    ["F", "glucose_30_min", "integer", "mg/dL", "units_row"], ["G", "glucose_60_min", "integer", "mg/dL", "units_row"],
    ["H", "glucose_120_min", "integer", "mg/dL", "units_row"], ["I", "insulin_0_min", "number", "ng/mL", "units_row"],
  ]);
  assert.deepEqual(sheet.issues.map((i) => [i.code, i.columns]), [
    ["category_spellings", ["C"]], ["censored_values", ["F", "I"]], ["missing_tokens_mixed", ["H"]], ["wide_time_points", ["D", "E", "F", "G", "H"]],
    ["header_not_first", []], ["title_rows", []], ["units_row", []], ["summary_rows", []], ["notes_below_data", []],
  ]);
  const sex = sheet.columns[2];
  assert.equal(sex.distinct, 5);
  assert.deepEqual(read.detail.sheets[0].columns[2].examples.category_spellings, ['"Female", "female"', '"MALE", "Male ", "male"']);
  assert.deepEqual(sheet.columns[5].censored, { above_limit: 1 });
  assert.deepEqual(sheet.columns[7].missing, { NA: 1, "-": 1 });
  assert.equal(suggestedName("Body weight (µg)", "D"), "body_weight_ug");
  assert.equal(suggestedName("2nd dose %", "E"), "x_2nd_dose_pct");
  assert.equal(suggestedName("", "AB"), "ab");
});

test("the starting specification is the profile's reading with every choice filled in", () => {
  const spec = startingSpec(sheet, "GTT week 4.csv", read.detail.sheets[0]);
  assert.equal(spec.table_name, "gtt_week_4");
  assert.deepEqual(spec.rows, { first: 5, last: 14 });
  assert.deepEqual(spec.drop_rows, [[13, 14]]);
  assert.deepEqual(spec.missing, ["-", "NA"]);
  assert.deepEqual(spec.columns.map((c) => [c.from, c.name, c.type]), [["A", "mouse_id", "text"], ["B", "diet", "category"], ["C", "sex", "category"], ["I", "insulin_0_min", "number"]]);
  assert.deepEqual(spec.columns[2].recode, { Female: "female", MALE: "male", Male: "male" });
  assert.deepEqual(spec.columns[3].censored, { rule: "missing" });
  assert.deepEqual(spec.long, { columns: ["D", "E", "F", "G", "H"], name: "minutes", values: [0, 15, 30, 60, 120],
    value: { name: "glucose", type: "integer", unit: "mg/dL", censored: { rule: "missing" } } });
  assert.deepEqual(spec.key, ["mouse_id", "minutes"]);
  assert.deepEqual(outputsOf(spec).map((o) => o.name), ["mouse_id", "diet", "sex", "insulin_0_min", "insulin_0_min_censored", "insulin_0_min_limit",
    "minutes", "glucose", "glucose_censored", "glucose_limit"]);
  assert.equal(specProblem(spec), null);
  assert.deepEqual(specProblems(spec), []);
  // Without the profile's detail the spellings are left for the person.
  assert.deepEqual(suggestedRecode(null), {});
});

test("the stand-in executor writes one row per animal and time and accounts for every filled cell", () => {
  const spec = startingSpec(sheet, "gtt.csv", read.detail.sheets[0]);
  const done = prepare(grid, spec as never) as { refusal: string | null; receipt: PreparationSummary; table?: Buffer };
  assert.equal(done.refusal, null);
  const r = done.receipt;
  assert.deepEqual(r.rows, { read: 10, blank: 0, dropped: 2, kept: 8, written: 40 });
  const c = r.cells!;
  assert.equal(c.filled, c.outside_rows + c.in_dropped_rows + c.in_unread_columns + c.read);
  assert.deepEqual(c, { filled: 98, outside_rows: 18, in_dropped_rows: 8, in_unread_columns: 0, read: 72 });
  assert.equal(cellsLine(c), "98 filled cells in the sheet: 18 outside the table, 8 in rows left out, 0 in columns not read, 72 read.");
  assert.equal(rowsLine(r, spec), "10 rows read (5 to 14), 2 left out, 8 kept. 40 rows written, 5 for each kept row.");
  const sex = r.columns.find((x) => x.name === "sex")!;
  assert.deepEqual(columnChanges(sex), ["8 values", "1 trimmed of spaces", "3 recoded (“Female” to “female” 1, “MALE” to “male” 1 and “Male” to “male” 1)"]);
  const glucose = r.columns.find((x) => x.name === "glucose")!;
  assert.equal(glucose.from, "D,E,F,G,H");
  assert.deepEqual(columnChanges(glucose), ["37 values", "2 missing (“-” 1 and “NA” 1)", "1 above a limit, written as missing"]);
  const lines = done.table!.toString("utf8").trim().split("\n");
  assert.equal(lines[0], "mouse_id,diet,sex,insulin_0_min,insulin_0_min_censored,insulin_0_min_limit,minutes,glucose,glucose_censored,glucose_limit");
  assert.equal(lines[1], "S01,Control,male,0.62,NA,NA,0,98,NA,NA");
  assert.equal(lines[13], "S03,Control,female,NA,below,0.20,30,240,NA,NA");
  assert.equal(lines[28], "S06,High fat,male,2.11,NA,NA,30,NA,above,600");
});

test("a cell no rule reads stops the preparation and is named by row and column", () => {
  const spec = startingSpec(sheet, "gtt.csv", read.detail.sheets[0]);
  delete spec.columns[3].censored;
  const done = prepare(grid, spec as never) as { refusal: string; receipt: PreparationSummary };
  assert.equal(done.refusal, "dataset_values_unread");
  assert.deepEqual(done.receipt.failures, [{ row: 7, column: "I", reason: "censored_without_rule", text: "<0.20" }]);
  assert.equal(unreadWords(done.receipt.failures[0]), "Row 7, column I: “<0.20” is reported against a limit, and the column has no rule for those.");
  assert.equal(preparationRefusal("dataset_values_unread", done.receipt), "Not prepared: 1 cell no rule could read. Nothing was written.");
  // A repeated key stops it too, with both rows.
  const twice = startingSpec(sheet, "gtt.csv", read.detail.sheets[0]);
  twice.key = ["diet", "minutes"];
  const repeated = prepare(grid, twice as never) as { refusal: string; receipt: PreparationSummary };
  assert.equal(repeated.refusal, "dataset_key_repeated");
  assert.deepEqual(repeated.receipt.repeated_keys[0], { row: 6, first_row: 5 });
});

test("the broker's checks of a specification are mirrored where the page meets them", () => {
  const spec = startingSpec(sheet, "gtt.csv", read.detail.sheets[0]);
  const withFrom = structuredClone(spec) as Spec & { long: { value: { from?: string } } };
  withFrom.long.value.from = "D";
  assert.deepEqual(specProblem(withFrom), { path: "long.value.from", reason: "not_allowed" });
  const recodedNumbers = structuredClone(spec);
  recodedNumbers.columns[3].recode = { a: "b" };
  assert.deepEqual(specProblem(recodedNumbers), { path: "columns[3].recode", reason: "not_for_this_type" });
  const halfOfWhole = structuredClone(spec);
  halfOfWhole.long!.value.censored = { rule: "half_limit" };
  assert.deepEqual(specProblem(halfOfWhole), { path: "long.value.censored.rule", reason: "not_for_this_type" });
  const sameName = structuredClone(spec);
  sameName.columns[1].name = "sex";
  assert.deepEqual(specProblem(sameName), { path: "name:sex", reason: "name_repeated" });
  assert.deepEqual(specProblems(sameName), [{ place: "name:sex", words: "Two columns would both be named sex." }]);
  assert.equal(specRefusalWords("columns[1].name", "not_a_name", spec), "Column B's name is not a name R takes: lower-case letters, digits and underscores, starting with a letter. Nothing was prepared.");
  assert.equal(specRefusalWords("name:sex", "name_repeated", spec), "The name sex is used for two columns. Nothing was prepared.");
});

test("the sheet's rows and columns change the specification in place", () => {
  const spec = startingSpec(sheet, "gtt.csv", read.detail.sheets[0]);
  const t = sheet.table;
  assert.deepEqual([1, 3, 4, 5, 13, 15].map((r) => rowRole(spec, t, r, false)), ["title", "names", "units", "data", "summary", "note"]);
  toggleRow(spec, 8);
  assert.deepEqual(spec.drop_rows, [[8, 8], [13, 14]]);
  assert.equal(rowRole(spec, t, 8, false), "left_out");
  toggleRow(spec, 13);
  assert.deepEqual(spec.drop_rows, [[8, 8], [14, 14]]);
  toggleRow(spec, 2);
  assert.deepEqual(spec.drop_rows, [[8, 8], [14, 14]]);
  toggleColumn(spec, sheet, "B");
  assert.deepEqual(spec.columns.map((c) => c.from), ["A", "C", "I"]);
  toggleColumn(spec, sheet, "B");
  assert.deepEqual(spec.columns.map((c) => [c.from, c.name, c.type]), [["A", "mouse_id", "text"], ["B", "diet", "category"], ["C", "sex", "category"], ["I", "insulin_0_min", "number"]]);
  setWide(spec, sheet);
  assert.equal(spec.long, undefined);
  assert.deepEqual(spec.columns.map((c) => c.from), ["A", "B", "C", "D", "E", "F", "G", "H", "I"]);
  assert.deepEqual(spec.key, ["mouse_id"]);
  assert.equal(specProblem(spec), null);
  setLong(spec, sheet, ["D", "E", "F", "G", "H"]);
  assert.deepEqual(spec.key, ["mouse_id", "minutes"]);
  assert.equal(specProblem(spec), null);
});

test("places and refusals are said in the sheet's own terms", () => {
  assert.equal(rowsWords([13, 14]), "rows 13 and 14");
  assert.equal(rowsWords([5, 6, 7, 8, 12]), "rows 5 to 8 and 12");
  assert.equal(rowsWords([9]), "row 9");
  assert.equal(lettersWords(["D", "E", "F", "G", "H"]), "columns D to H");
  assert.equal(lettersWords(["F", "I"]), "columns F and I");
  assert.equal(guardWords({ intake: "x", outcome: "blocked", findings: ["type_not_allowed"], policy: "p" }).text,
    "The upload guard blocked this file, so it was not taken: Excel 97-2003 files (.xls) are not read; save it as .xlsx or CSV and send that.");
  assert.deepEqual(guardWords({ intake: "x", outcome: "allowed", findings: ["formulas_without_values"], policy: "p" }).notes,
    ["some formulas have no saved values, so those cells are empty here"]);
  assert.equal(uploadProblem("idempotency_conflict").again, false);
  assert.equal(uploadProblem("dataset_method_unavailable").unavailable, true);
  assert.equal(uploadProblem("dataset_guard_busy").again, true);
});

// The preview's routes answer as the broker describes them.
test("the preview's routes take a file once, read it, and prepare it on approval", async () => {
  const project = randomUUID(), tasks: Task[] = [], histories = new Map<string, TaskEvent[]>(), added: ProjectTask[] = [];
  const fx = datasetFixtures({ me: "reader", profile: "lab_dataset", group: "open_methods", project, tasks, histories, addProjectTask: (e) => added.push(e), delay: 1 });
  async function call(method: string, path: string, body?: Buffer | unknown, headers: Record<string, string> = {}) {
    const raw = body === undefined ? [] : [Buffer.isBuffer(body) ? body : Buffer.from(JSON.stringify(body))];
    const req = Object.assign(Readable.from(raw), { method, headers: { "x-agent-sandbox-intent": "mutate", ...headers } }) as unknown as IncomingMessage;
    let out: { status: number; body: any } = { status: 0, body: null };
    const res = { setHeader() {}, end() {} };
    await fx.handle(req, res as unknown as ServerResponse, new URL(`http://fixture${path}`), (value, status = 200) => { out = { status, body: value }; }, () => { out = { status: 404, body: null }; });
    return out;
  }
  const settle = () => new Promise((resolve) => setTimeout(resolve, 20));
  const requestId = randomUUID();
  const sent = await call("POST", `/api/v1/projects/${project}/datasets?request_id=${requestId}`, bytes, { "x-dataset-name": "gtt%20week%204.csv" });
  assert.equal(sent.status, 201);
  assert.equal(sent.body.dataset.file_name, "gtt week 4.csv");
  assert.equal(sent.body.dataset.guard.outcome, "allowed");
  const again = await call("POST", `/api/v1/projects/${project}/datasets?request_id=${requestId}`, bytes, { "x-dataset-name": "gtt%20week%204.csv" });
  assert.deepEqual([again.status, again.body.created, again.body.dataset.dataset_id], [200, false, sent.body.dataset.dataset_id]);
  const other = await call("POST", `/api/v1/projects/${project}/datasets?request_id=${requestId}`, Buffer.from("a,b\n1,2\n"), { "x-dataset-name": "other.csv" });
  assert.deepEqual([other.status, other.body.error.code], [409, "idempotency_conflict"]);
  const old = await call("POST", `/api/v1/projects/${project}/datasets?request_id=${randomUUID()}`, Buffer.from("x"), { "x-dataset-name": "old.xls" });
  assert.deepEqual([old.status, old.body.dataset.guard.outcome, old.body.dataset.guard.findings, old.body.dataset.intake_task], [201, "blocked", ["type_not_allowed"], null]);
  await settle();
  const dataset = sent.body.dataset.dataset_id;
  const detail = await call("GET", `/api/v1/projects/${project}/datasets/${dataset}`);
  const intake = detail.body.dataset.intake;
  assert.equal(intake.receipts.at(-1).status, "profiled");
  assert.equal(added.filter((e) => e.dataset?.operation === "intake").length, 1);
  const spec = startingSpec(intake.receipts.at(-1).profile.sheets[0], "gtt week 4.csv", read.detail.sheets[0]);
  const invalid = await call("POST", `/api/v1/projects/${project}/datasets/${dataset}/preparations`, { spec: { ...spec, extra: 1 }, title: "", previous: null, request_id: randomUUID() });
  assert.deepEqual([invalid.status, invalid.body.error.code, invalid.body.error.path, invalid.body.error.reason], [400, "dataset_preparation_invalid", "", "unknown_field"]);
  const prepRequest = { spec, title: "Glucose tolerance, week 4", previous: null, request_id: randomUUID() };
  const asked = await call("POST", `/api/v1/projects/${project}/datasets/${dataset}/preparations`, prepRequest);
  assert.equal(asked.status, 202);
  const same = await call("POST", `/api/v1/projects/${project}/datasets/${dataset}/preparations`, prepRequest);
  assert.equal(same.body.task_id, asked.body.task_id);
  await settle();
  const listed = await call("GET", `/api/v1/projects/${project}/datasets`);
  const prep = listed.body.datasets.find((d: { dataset_id: string }) => d.dataset_id === dataset).preparations[0];
  assert.deepEqual([prep.status, prep.receipt.status, prep.receipt.table_path, prep.receipt.table_rows], ["completed", "prepared", "results/lab_data/gtt_week_4.csv", 40]);
  const lineage = await call("GET", `/api/v1/tasks/${asked.body.task_id}/lineage`);
  assert.equal(lineage.body.dataset.operation, "preparation");
  assert.deepEqual(lineage.body.inputs.classes, ["prior_output"]);
  // A preparation's table is a source figures can draw from.
  assert.equal(fx.prepared()[0].sourceTask, asked.body.task_id);
  const elsewhere = await call("POST", `/api/v1/projects/${randomUUID()}/datasets?request_id=${randomUUID()}`, bytes, { "x-dataset-name": "a.csv" });
  assert.deepEqual([elsewhere.status, elsewhere.body.error.code], [403, "dataset_method_unavailable"]);
  fx.reset();
});
