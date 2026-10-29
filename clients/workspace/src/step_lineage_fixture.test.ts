import assert from "node:assert/strict";
import type { IncomingMessage, ServerResponse } from "node:http";
import { Readable } from "node:stream";
import test from "node:test";
import { fixtureApi } from "../fixtures/server.ts";
import { STEP_LINEAGE_TASK, stepLineageFixture } from "../fixtures/step_lineage.ts";
import type { StepLineage, Task } from "./contracts.ts";
import { lineageSummary } from "./lineage.ts";
import { shapeStepLineage } from "./step_lineage.ts";

const at = (seconds: number) => new Date(Date.UTC(2026, 9, 15, 12, 0, seconds)).toISOString();

async function fixtureRead(
  fixture: ReturnType<typeof stepLineageFixture>,
  path: string,
): Promise<unknown> {
  const req = Object.assign(Readable.from([]), { method: "GET" }) as unknown as IncomingMessage;
  const res = { setHeader() {}, end() {} } as unknown as ServerResponse;
  let received: unknown;
  const handled = await fixture.handle(
    req,
    res,
    new URL(`http://fixture${path}`),
    (value) => { received = value; },
    () => assert.fail("the fixture route was refused"),
  );
  assert.equal(handled, true);
  assert.ok(received);
  return received;
}

test("the preview fixture serves the three-level lineage and one withheld input", async () => {
  const fixture = stepLineageFixture({ me: "20000000-0000-4000-8000-000000000001", at });
  const body = await fixtureRead(
    fixture,
    `/api/v1/tasks/${STEP_LINEAGE_TASK}/lineage?version=3`,
  ) as StepLineage;
  assert.equal(body.schema, "enrichedlab.task-lineage.v3");
  assert.equal(body.upstream.nodes.length, 3);
  assert.equal(body.upstream.withheld, 1);
  const shaped = shapeStepLineage(body);
  assert.deepEqual(
    [shaped.inputs[0].kind, shaped.inputs[0].inputs[0].kind, shaped.inputs[0].inputs[0].inputs[0].kind],
    ["Reviewed analysis", "Database query", "Reference source"],
  );
  assert.match(shaped.inputs[0].inputs[0].inputs[0].notes.join(" "), /April 2026.*FoodData_Central_foundation_food_csv_2026-04-30\.zip/);
  assert.match(shaped.incomplete ?? "", /Earlier history is not fully shown.*1 recorded input comes from a step/);
  assert.doesNotMatch(lineageSummary(body), /Every part/);
  assert.match(lineageSummary(body), /Earlier history is not fully shown.*1 recorded input comes from a step/);
});

test("the demo task, events and artifacts are served by address", async () => {
  const fixture = stepLineageFixture({ me: "20000000-0000-4000-8000-000000000001", at });
  const task = await fixtureRead(fixture, `/api/v1/tasks/${STEP_LINEAGE_TASK}`) as Task;
  assert.equal(task.id, STEP_LINEAGE_TASK);
  const history = await fixtureRead(fixture, `/api/v1/tasks/${STEP_LINEAGE_TASK}/events?after=1`) as { events: { sequence: number }[] };
  assert.deepEqual(history.events.map((event) => event.sequence), [2, 3]);
  const artifacts = await fixtureRead(fixture, `/api/v1/tasks/${STEP_LINEAGE_TASK}/artifacts`) as { artifacts: unknown[] };
  assert.equal(artifacts.artifacts.length, 2);
  // Records the demo task does not have answer present and empty, as the broker does, so no section shows a read error.
  assert.deepEqual(await fixtureRead(fixture, `/api/v1/tasks/${STEP_LINEAGE_TASK}/reviews`), { reviews: [] });
  assert.deepEqual(await fixtureRead(fixture, `/api/v1/tasks/${STEP_LINEAGE_TASK}/generated-code`), { generated_code: null });
  assert.deepEqual(await fixtureRead(fixture, `/api/v1/tasks/${STEP_LINEAGE_TASK}/template-analyses`), { template_analyses: [] });
});

test("the address-only demo does not change shared task or project listings", async () => {
  const plugin = fixtureApi();
  type Middleware = (req: IncomingMessage, res: ServerResponse, next: () => void) => Promise<void>;
  let middleware: Middleware | undefined;
  assert.equal(typeof plugin.configureServer, "function");
  (plugin.configureServer as (server: unknown) => void)({
    httpServer: null,
    middlewares: { use(value: Middleware) { middleware = value; } },
  });
  assert.ok(middleware);

  const read = async (path: string): Promise<Record<string, unknown>> => {
    const req = Object.assign(Readable.from([]), { method: "GET", url: path, headers: {} }) as unknown as IncomingMessage;
    let body = "";
    const res = {
      statusCode: 200,
      setHeader() { return this; },
      end(value?: string | Buffer) { body = value?.toString() ?? ""; return this; },
    } as unknown as ServerResponse;
    await middleware!(req, res, () => assert.fail(`fixture did not handle ${path}`));
    assert.equal(res.statusCode, 200);
    return JSON.parse(body) as Record<string, unknown>;
  };

  const collection = await read("/api/v1/tasks") as { tasks: Task[] };
  assert.equal(collection.tasks.length, 4);
  assert.ok(!collection.tasks.some((task) => task.id === STEP_LINEAGE_TASK));
  const project = await read("/api/v1/projects/50000000-0000-4000-8000-000000000001") as {
    tasks: { task_id: string }[];
  };
  assert.ok(!project.tasks.some((task) => task.task_id === STEP_LINEAGE_TASK));
});
