import assert from "node:assert/strict";
import { createHash, randomUUID } from "node:crypto";
import type { IncomingMessage, ServerResponse } from "node:http";
import { Readable } from "node:stream";
import test from "node:test";
import { figureFixtures } from "../fixtures/figures.ts";
import type { ProjectTask } from "../fixtures/projects.ts";
import type { Artifact, Task, TaskEvent } from "./contracts.ts";

// The preview's figure routes answer as the broker's shapes do (previews, colours per group, the manuscript mark),
// so the editor and the record are tested against the refusals they will meet.
const project = randomUUID(), sourceTask = randomUUID();
const table = "site,dose,response\nnorth,1,2.5\nnorth,2,3.1\nsouth,1,1.9\nsouth,2,2.2\neast,1,2.0\n";

function setup(listedEachTime = false) {
  const tasks: Task[] = [], histories = new Map<string, TaskEvent[]>(), added: ProjectTask[] = [];
  const artifact = { id: randomUUID(), sha256: "0".repeat(64) } as Artifact;
  const source = () => ({
    project, group: "lab", sourceTask, source: () => ({ files: [{ path: "results/trial.csv", bytes: Buffer.from(table) }], artifact }),
    task: () => ({ status: "completed", objective: "Trial" }), addProjectTask: (entry: ProjectTask) => added.push(entry) });
  // The preview lists its sources again on each request when tables arrive while it runs (prepared lab data).
  const fx = figureFixtures({ me: "reader", profile: "figure_render", tasks, histories, previewDelay: 0,
    sources: listedEachTime ? () => [source()] : [source()] });
  async function call(method: string, path: string, body?: unknown) {
    const req = Object.assign(Readable.from(body === undefined ? [] : [Buffer.from(JSON.stringify(body))]), { method }) as unknown as IncomingMessage;
    let out: { status: number; body: unknown } = { status: 0, body: null };
    const res = { statusCode: 200, setHeader() {}, end() { out = { status: (res as { statusCode: number }).statusCode, body: null }; } };
    const json = (value: unknown, status = 200) => { out = { status, body: value }; };
    await fx.handle(req, res as unknown as ServerResponse, new URL(`http://fixture${path}`), json, () => { out = { status: 404, body: "denied" }; });
    // A preview's drawing comes a moment later, as a compute worker draws it.
    for (let waited = 0; out.status === 0 && waited < 2000; waited += 5) await new Promise((resolve) => setTimeout(resolve, 5));
    return out as { status: number; body: any };
  }
  return { fx, tasks, histories, added, call };
}
const ask = (controls: Record<string, unknown>, feature = "scatter") => ({ source_task: sourceTask, table_path: "results/trial.csv", feature, controls, title: "" });
const settle = () => new Promise((resolve) => setTimeout(resolve, 1600));

test("a preview is drawn from the table, checked by its digest and stored nowhere", async () => {
  const { fx, tasks, added, call } = setup();
  const out = await call("POST", `/api/v1/projects/${project}/figures/previews`, ask({ x: "dose", y: "response", color: "site", font: "serif" }));
  assert.equal(out.status, 200);
  const bytes = Buffer.from(out.body.preview.svg_base64, "base64");
  assert.equal(bytes.length, out.body.preview.size_bytes);
  assert.equal(createHash("sha256").update(bytes).digest("hex"), out.body.preview.sha256);
  assert.match(bytes.toString(), /not drawn by R/);
  assert.equal(out.body.settings_used.font_face, "Times");
  assert.deepEqual([tasks.length, added.length], [0, 0]);
  fx.reset();
});

test("a text column for a measured role is refused in words, and a colour for a group the table lacks is a note", async () => {
  const { fx, call } = setup();
  const refused = await call("POST", `/api/v1/projects/${project}/figures/previews`, ask({ x: "dose", y: "site" }));
  assert.equal(refused.body.preview, null);
  assert.equal(refused.body.refused, "No numbers this figure can draw in site.");
  assert.deepEqual(refused.body.refusal, { code: "no_numeric_values", columns: ["site"], counts: [] });
  const noted = await call("POST", `/api/v1/projects/${project}/figures/previews`, ask({ x: "dose", y: "response", color: "site", group_colors: { north: "#0072B2", west: "#E69F00" } }));
  assert.ok(noted.body.preview);
  assert.deepEqual(noted.body.settings_used.group_colors, { north: "#0072B2" });
  assert.match(noted.body.notes[0], /names west, which the table does not hold/);
  const offList = await call("POST", `/api/v1/projects/${project}/figures/previews`, ask({ x: "dose", y: "response", color: "site", group_colors: { north: "#123456" } }));
  assert.deepEqual([offList.status, offList.body.error.code], [400, "figure_settings_invalid"]);
  fx.reset();
});

test("previews pause past thirty a minute", async () => {
  const { fx, call } = setup();
  for (let i = 0; i < 30; i++) assert.equal((await call("POST", `/api/v1/projects/${project}/figures/previews`, ask({ value: "response" }, "histogram"))).status, 200);
  const paused = await call("POST", `/api/v1/projects/${project}/figures/previews`, ask({ value: "response" }, "histogram"));
  assert.deepEqual([paused.status, paused.body.error.code], [429, "capacity_reached"]);
  fx.reset();
});

test("a person has one preview waiting at a time", async () => {
  const { fx, call } = setup();
  const both = await Promise.all([call("POST", `/api/v1/projects/${project}/figures/previews`, ask({ value: "response" }, "histogram")),
    call("POST", `/api/v1/projects/${project}/figures/previews`, ask({ value: "response", bins: 10 }, "histogram"))]);
  assert.deepEqual(both.map((answer) => answer.status), [200, 429]);
  assert.equal(both[1].body.error.code, "capacity_reached");
  assert.equal((await call("POST", `/api/v1/projects/${project}/figures/previews`, ask({ value: "response" }, "histogram"))).status, 200);
  fx.reset();
});

test("one drawn version per figure carries the manuscript mark", async () => {
  const { fx, call, histories } = setup();
  const first = (await call("POST", `/api/v1/projects/${project}/figures`, { ...ask({ x: "dose", y: "response" }), request_id: randomUUID() })).body.task_id;
  const early = await call("PUT", `/api/v1/projects/${project}/figures/${first}/manuscript`, { version: first });
  assert.deepEqual([early.status, early.body.error.code], [409, "version_not_drawn"]);
  await settle();
  const second = (await call("POST", `/api/v1/projects/${project}/figures`, { ...ask({ x: "dose", y: "response", width: 3.3465 }), previous: first, request_id: randomUUID() })).body.task_id;
  const other = (await call("POST", `/api/v1/projects/${project}/figures`, { ...ask({ value: "response" }, "histogram"), request_id: randomUUID() })).body.task_id;
  await settle();
  assert.equal((await call("PUT", `/api/v1/projects/${project}/figures/${second}/manuscript`, { version: second })).body.error.code, "figure_not_found");
  assert.equal((await call("PUT", `/api/v1/projects/${project}/figures/${first}/manuscript`, { version: other })).body.error.code, "version_not_in_figure");
  const marked = await call("PUT", `/api/v1/projects/${project}/figures/${first}/manuscript`, { version: second });
  assert.deepEqual([marked.status, marked.body.figure, marked.body.version, marked.body.marked_by], [200, first, second, "reader"]);
  const listed = (await call("GET", `/api/v1/tasks/${sourceTask}/figures`)).body.versions as { task_id: string; manuscript: { version: string } | null }[];
  assert.deepEqual(Object.fromEntries(listed.map((v) => [v.task_id, v.manuscript?.version ?? null])), { [first]: second, [second]: second, [other]: null });
  assert.equal((await call("GET", `/api/v1/tasks/${first}/figure`)).body.figure.manuscript.version, second);
  assert.equal(histories.get(second)!.at(-1)!.code, "figure_manuscript_marked");
  assert.equal((await call("DELETE", `/api/v1/projects/${project}/figures/${first}/manuscript`)).status, 204);
  assert.equal(histories.get(second)!.at(-1)!.code, "figure_manuscript_cleared");
  assert.equal((await call("GET", `/api/v1/tasks/${second}/figure`)).body.figure.manuscript, null);
  fx.reset();
});

test("a version changes one made from the same analysis when the sources are listed again for each request", async () => {
  const { fx, call } = setup(true);
  const first = (await call("POST", `/api/v1/projects/${project}/figures`, { ...ask({ x: "dose", y: "response" }), request_id: randomUUID() })).body.task_id;
  const second = await call("POST", `/api/v1/projects/${project}/figures`, { ...ask({ x: "dose", y: "response", width: 3.3465 }), previous: first, request_id: randomUUID() });
  assert.equal(second.status, 202);
  const elsewhere = await call("POST", `/api/v1/projects/${project}/figures`, { ...ask({ x: "dose", y: "response" }), previous: randomUUID(), request_id: randomUUID() });
  assert.deepEqual([elsewhere.status, elsewhere.body.error.code], [400, "figure_previous_invalid"]);
  await settle();
  fx.reset();
});
