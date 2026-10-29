import assert from "node:assert/strict";
import { test } from "node:test";
import type { FigureCatalog, FigureFeature } from "./contracts.ts";
import { canDraw, chartName, choiceLabel, controlsFrom, draftFrom, drawable, figureGroups, figureRoot, identifierName, manuscriptOf, OKABE_ITO,
  offeredChoices, outcomeSentence, previewProblem, requestProblem, sections, settingLabel, settingsDiff, settingsLines, plainNote, shownVersion,
  startingColumns, suits, toInches, toMm, versionStatus, withFiguresOnce } from "./figure.ts";

const scatter: FigureFeature = {
  id: "scatter", label: "Scatter plot", description: "One point per row.",
  params: {
    x: { type: "column", required: true }, y: { type: "column", required: true }, color: { type: "column" },
    x_label: { type: "string" }, legend_position: { type: "enum", choices: ["right", "left", "top", "bottom", "none"] },
    width: { type: "number", min: 3, max: 20 }, height: { type: "number", min: 2, max: 20 }, dpi: { type: "number", min: 72, max: 600 },
    format: { type: "enum", choices: ["png", "svg", "pdf", "tiff"] }, x_limits: { type: "numbers", maxlen: 2 },
    log_y: { type: "enum", choices: ["true", "false"] },
  },
};
const box: FigureFeature = { id: "box", label: "Box plot", description: "", params: { group: { type: "column", required: true },
  value: { type: "column", required: true }, extra: { type: "column", required: true } } };
const catalog: FigureCatalog = { catalog_sha256: "a".repeat(64), service_version: "0.5.20", image_digest: null,
  features: [scatter, box], seen_at: "2026-09-27T05:00:00Z", groups: ["pilot-lab"] };

test("a chart is offered only when the table has a column for each required role", () => {
  assert.deepEqual(drawable(catalog, 2).map((f) => f.id), ["scatter"]);
  assert.deepEqual(drawable(catalog, 3).map((f) => f.id), ["scatter", "box"]);
  assert.deepEqual(drawable(null, 3), []);
  assert.equal(canDraw(0, 2), false);
  assert.equal(canDraw(50_001, 2), false);
  assert.equal(canDraw(10, 65), false);
  assert.equal(canDraw(10, 2), true);
});

test("the settings fall into the editor's sections and the extra export offers only PDF and TIFF", () => {
  const s = sections(scatter);
  assert.deepEqual(s.columns.map(([n]) => n), ["x", "y", "color"]);
  assert.deepEqual(s.titles.map(([n]) => n), ["x_label"]);
  assert.deepEqual(s.legend.map(([n]) => n), ["legend_position"]);
  assert.deepEqual(s.size.map(([n]) => n), ["width", "height", "dpi", "format"]);
  assert.deepEqual(s.more.map(([n]) => n), ["x_limits", "log_y"]);
  assert.deepEqual(offeredChoices("format", scatter.params.format), ["pdf", "tiff"]);
  assert.equal(choiceLabel("legend_position", "bottom"), "Below");
});

test("starting columns put numbers where the chart measures and use each column once", () => {
  assert.deepEqual(startingColumns(scatter, ["food", "amount", "energy"], [false, true, true]), { x: "amount", y: "energy" });
  assert.deepEqual(startingColumns(box, ["diet", "weight", "sex"], [false, true, false]), { group: "diet", value: "weight", extra: "sex" });
});

test("an identifier holds numbers but is not a measure: measured roles start on measures, and a new figure starts on a chart the table suits", () => {
  for (const name of ["fdc_id", "Patient ID", "sampleId", "id", "grid_no", "icd_code", "sort key"]) assert.equal(identifierName(name), true, name);
  for (const name of ["vitamin_c_mg_per_100g", "Grid", "acid", "no2_ppm", "width", "n"]) assert.equal(identifierName(name), false, name);
  // A test run's table: an identifier, a food's name and one measure.
  const columns = ["fdc_id", "food_name", "vitamin_c_mg_per_100g"], numeric = [true, false, true];
  assert.equal(startingColumns(scatter, columns, numeric).x, "vitamin_c_mg_per_100g");
  assert.equal(suits(scatter, columns, numeric), false);
  assert.equal(suits(box, ["diet", "weight", "sex"], [false, true, false]), true);
  assert.equal(suits(scatter, ["food", "amount", "energy"], [false, true, true]), true);
  const bar: FigureFeature = { id: "bar", label: "Bar chart", description: "", params: {
    category: { type: "column", required: true }, value: { type: "column", required: true } } };
  assert.equal(suits(bar, columns, numeric), true);
  assert.deepEqual(startingColumns(bar, columns, numeric), { category: "food_name", value: "vitamin_c_mg_per_100g" });
});

test("a version that failed reads as not drawn; any other status in the usual words", () => {
  assert.deepEqual(versionStatus("failed"), { label: "Not drawn", tone: "failed" });
  assert.equal(versionStatus("completed").label, "Finished");
  assert.equal(versionStatus("running").label, "Running");
});

test("the form becomes settings, and what cannot be sent is named in plain words", () => {
  const draft = draftFrom(scatter, { x: "amount", y: "energy", x_limits: [0, 50], legend_position: "bottom", width: 8 });
  assert.equal(draft.x_limits, "0, 50");
  const good = controlsFrom(scatter, { ...draft, x_label: "  Amount per 100 g ", log_y: "true" }, ["amount", "energy"]);
  assert.deepEqual(good.problems, []);
  assert.deepEqual(good.controls, { x: "amount", y: "energy", x_label: "Amount per 100 g", legend_position: "bottom",
    width: 8, x_limits: [0, 50], log_y: "true" });
  const bad = controlsFrom(scatter, { x: "missing", width: "wide", height: "30", x_limits: "5", legend_position: "middle" }, ["amount"]);
  assert.deepEqual(bad.problems, [
    "Across: the table has no column named missing.",
    "Choose a column for up.",
    "Legend: choose one of the offered values.",
    "Width needs a number of millimetres.",
    "Height must be between 50.8 and 508 mm.",
    "Horizontal axis range needs two numbers: the lowest and the highest.",
  ]);
});

test("a version's settings and outcome read as short sentences", () => {
  assert.deepEqual(settingsLines(scatter, { x: "amount", y: "energy", legend_position: "none", width: 8, height: 5, dpi: 300, log_y: "true" }),
    ["Across: amount", "Up: energy", "Legend: No legend", "Size: 203.2 by 127 mm", "Resolution: 300 dots per inch", "Logarithmic vertical axis: yes"]);
  const base = { attempt: 1, error_code: null, message: null, service_version: "0.5.20", image_digest: null, files: [],
    package_sha256: "b".repeat(64), started_at: "", finished_at: "" };
  assert.equal(outcomeSentence({ ...base, status: "completed", notes: [] }, "completed"), "Drawn exactly as asked.");
  assert.equal(outcomeSentence({ ...base, status: "refused", error_code: "figure_refused", message: "'x' is not a column", notes: [] }, "failed"),
    "Not drawn. Across is not a column.");
  // A kept version's failure carries the broker's own words.
  assert.equal(outcomeSentence({ ...base, status: "failed", error_code: "figure_render_failed",
    message: "The figure service could not draw this table with these settings.", notes: [] }, "failed"),
  "Not drawn. The figure service could not draw this table with these settings.");
  assert.equal(outcomeSentence(undefined, "running"), "The figure is being drawn.");
});

test("versions join into figures by their edits, and each figure is listed once by its newest drawn version", () => {
  const v = (task_id: string, previous: string | null, minute: number, status = "completed") =>
    ({ task_id, previous, created_at: `2026-09-27T05:${String(minute).padStart(2, "0")}:00Z`, status });
  const groups = figureGroups([v("a1", null, 1), v("b1", null, 2), v("a2", "a1", 3), v("a3", "a2", 4, "failed"), v("b2", "b1", 5, "running")]);
  assert.deepEqual(groups.map((g) => g.map((x) => x.task_id)), [["b2", "b1"], ["a3", "a2", "a1"]]);
  assert.equal(shownVersion(groups[1]).task_id, "a2");
  assert.equal(shownVersion([v("c1", null, 1, "failed")]).task_id, "c1");
  const tasks = [
    { task_id: "s", status: "completed", submitted_at: "2026-09-27T05:00:00Z", figure: null },
    { task_id: "a1", status: "completed", submitted_at: "2026-09-27T05:01:00Z", figure: { previous: null } },
    { task_id: "a2", status: "completed", submitted_at: "2026-09-27T05:03:00Z", figure: { previous: "a1" } },
    { task_id: "a3", status: "failed", submitted_at: "2026-09-27T05:04:00Z", figure: { previous: "a2" } },
  ];
  assert.deepEqual(withFiguresOnce(tasks).map((t) => t.task_id), ["s", "a2"]);
  assert.equal(chartName(catalog, "scatter"), "Scatter plot");
  assert.equal(chartName(null, "violin_plot"), "Violin plot");
});

test("the service's notes name settings in the editor's words and keep column names as written", () => {
  assert.equal(plainNote("'width' 30 is outside 3 to 20; 20 was used"), "Width 30 is outside 3 to 20; 20 was used");
  assert.equal(plainNote("column 'dose' (for 'x') is not in the result columns"), "Column 'dose' (for Across) is not in the result columns");
  assert.equal(plainNote("'x_limits' was given as maximum then minimum and was reordered"), "Horizontal axis range was given as maximum then minimum and was reordered");
});

test("sizes read in millimetres and travel in inches to four decimals", () => {
  assert.equal(toInches(85), 3.3465);
  assert.equal(toMm(3.3465), "85");
  assert.equal(toMm(7), "177.8");
  const draft = draftFrom(scatter, { x: "amount", y: "energy", width: 3.3465, height: 2.3622 });
  assert.equal(draft.width, "85");
  assert.equal(draft.height, "60");
  assert.deepEqual(controlsFrom(scatter, { ...draft, width: "180" }, ["amount", "energy"]).controls, { x: "amount", y: "energy", width: 7.0866, height: 2.3622 });
  assert.deepEqual(controlsFrom(scatter, { ...draft, width: "600" }, ["amount", "energy"]).problems, ["Width must be between 76.2 and 508 mm."]);
  assert.deepEqual(settingsLines(scatter, { width: 3.3465, height: 2.3622 }), ["Size: 85 by 60 mm"]);
});

test("each group can take one of the offered colours, named in words", () => {
  const coloured: FigureFeature = { ...scatter, params: { ...scatter.params,
    group_colors: { type: "colors", of: "color", maxlen: 2, choices: OKABE_ITO.map((c) => c.hex) } } };
  assert.equal(sections(coloured).legend.map(([name]) => name).join(), "legend_position,group_colors");
  const draft = draftFrom(coloured, { x: "amount", y: "energy", group_colors: { control: "#56B4E9", test: "#D55E00" } });
  assert.equal(draft.group_colors, '{"control":"#56B4E9","test":"#D55E00"}');
  const sent = controlsFrom(coloured, draft, ["amount", "energy"]);
  assert.deepEqual(sent.controls.group_colors, { control: "#56B4E9", test: "#D55E00" });
  assert.deepEqual(settingsLines(coloured, { group_colors: { control: "#56B4E9", test: "#D55E00" } }), ["Colours per group: control sky blue, test vermillion"]);
  // A colour outside the set, or more groups than the service takes, is named instead of sent.
  assert.deepEqual(controlsFrom(coloured, { ...draft, group_colors: '{"control":"#123456"}' }, ["amount", "energy"]).problems,
    ["Colours per group: choose colours from the offered set."]);
  assert.deepEqual(controlsFrom(coloured, { ...draft, group_colors: '{"a":"#000000","b":"#000000","c":"#000000"}' }, ["amount", "energy"]).problems,
    ["Colours per group takes at most 2 groups."]);
  // An empty choice sends nothing.
  assert.equal("group_colors" in controlsFrom(coloured, { ...draft, group_colors: "{}" }, ["amount", "energy"]).controls, false);
});

test("a surface names its second column as the one running front to back", () => {
  assert.equal(settingLabel("y", "surface"), "Deep");
  assert.equal(settingLabel("y", "scatter"), "Up");
  assert.equal(settingLabel("z"), "Height");
  const v = (task_id: string, previous: string | null, minute: number) => ({ task_id, previous, created_at: `2026-10-12T08:${String(minute).padStart(2, "0")}:00Z`, status: "completed" });
  assert.equal(figureRoot(figureGroups([v("a1", null, 1), v("a2", "a1", 2), v("a3", "a2", 3)])[0]), "a1");
});

test("a figure is named by the first version the broker names, when it names one", () => {
  const v = (task_id: string, previous: string | null, minute: number, figure?: string) =>
    ({ task_id, previous, created_at: `2026-10-12T08:${String(minute).padStart(2, "0")}:00Z`, status: "completed", ...(figure ? { figure } : {}) });
  // Only the newer versions are loaded; the broker still names the figure by its first version.
  assert.equal(figureRoot(figureGroups([v("a2", "a1", 2, "a1"), v("a3", "a2", 3, "a1")])[0]), "a1");
  assert.equal(figureRoot(figureGroups([v("a2", "a1", 2), v("a3", "a2", 3)])[0]), "a2");
});

test("a preview that did not come says why, and offers another try only when one can help", () => {
  assert.deepEqual(previewProblem(429, "capacity_reached").again, true);
  assert.match(previewProblem(429, "capacity_reached").words, /one is still being drawn/);
  assert.deepEqual(previewProblem(503, "figure_service_unavailable"), { words: "No preview came back in time, or the render service failed.", again: true });
  // Access lost while the preview waited is not worth another try.
  assert.deepEqual(previewProblem(404, "project_not_found"),
    { words: "Your account can no longer add to this project, or the analysis has left it. Nothing was kept.", again: false });
  assert.equal(previewProblem(404, "task_not_found").again, false);
  assert.deepEqual(previewProblem(413, "figure_preview_too_large"), { words: "This drawing is larger than a preview can be (2 MB). Keep a version to see it.", again: false });
  for (const code of ["figure_source_changed", "figure_source_unreadable", "figure_table_missing", "figure_table_too_large", "figure_table_unreadable", "figure_table_unsuitable"]) {
    const problem = previewProblem(409, code);
    assert.equal(problem.again, false, code);
    assert.equal(problem.words, requestProblem(code), code);
    assert.notEqual(problem.words, requestProblem(""), code);
  }
  assert.deepEqual(previewProblem(409, "figure_preview_refused"), { words: "R could not draw a preview from this table with these settings.", again: false });
  assert.deepEqual(previewProblem(0, ""), { words: "The preview could not be drawn just now.", again: true });
});

test("a text setting takes as many characters as its chart allows", () => {
  const feature: FigureFeature = { id: "surface", label: "Surface", description: "", params: {
    x: { type: "column", required: true }, y: { type: "column", required: true }, z: { type: "column", required: true },
    z_label: { type: "string", maxlen: 80 }, subtitle: { type: "string" } } };
  const draft = { x: "hour", y: "block", z: "difference_c", z_label: "x".repeat(81), subtitle: "s".repeat(200) };
  assert.deepEqual(controlsFrom(feature, draft, ["hour", "block", "difference_c"]).problems, ["Height axis title is longer than 80 characters."]);
  assert.deepEqual(controlsFrom(feature, { ...draft, z_label: "Difference, °C", subtitle: "s".repeat(201) }, ["hour", "block", "difference_c"]).problems,
    ["Subtitle is longer than 200 characters."]);
});

test("the version marked for the manuscript stands for its figure, and a broker without marks keeps the newest drawn", () => {
  const v = (task_id: string, created_at: string, status = "completed", manuscript?: { version: string } | null) =>
    ({ task_id, previous: task_id === "a" ? null : "a", created_at, status, ...(manuscript === undefined ? {} : { manuscript }) });
  const marked = [v("c", "3", "completed", { version: "b" }), v("b", "2", "completed", { version: "b" }), v("a", "1", "completed", { version: "b" })];
  assert.equal(shownVersion(marked).task_id, "b");
  assert.equal(manuscriptOf(marked)?.version, "b");
  const none = [v("b", "2", "completed", null), v("a", "1", "completed", null)];
  assert.deepEqual([shownVersion(none).task_id, manuscriptOf(none)], ["b", null]);
  const older = [v("b", "2"), v("a", "1")];
  assert.deepEqual([shownVersion(older).task_id, manuscriptOf(older)], ["b", undefined]);
});

test("two versions' settings line up with each difference marked, and a setting left to R reads Default", () => {
  const rows = settingsDiff(catalog, { feature: "scatter", title: "Dose response", controls: { x: "dose", y: "response", width: 3.3465, height: 2.3622 } },
    { feature: "scatter", title: "", controls: { x: "dose", y: "response", color: "site", width: 7.0866, height: 2.3622 } });
  assert.deepEqual(rows.map((r) => [r.label, r.a, r.b, r.differs]), [
    ["Chart", "Scatter plot", "Scatter plot", false], ["Title", "Dose response", "Default", true],
    ["Across", "dose", "dose", false], ["Up", "response", "response", false], ["Colour by", "Default", "site", true],
    ["Size", "85 by 60 mm", "180 by 60 mm", true]]);
});

test("a note about a surface names its columns as the surface does", () => {
  assert.equal(plainNote("'y' needs a column of numbers; 'block' holds text", "surface"), "Deep needs a column of numbers; 'block' holds text");
});
