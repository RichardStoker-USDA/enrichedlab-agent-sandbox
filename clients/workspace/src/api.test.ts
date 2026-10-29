import { test, afterEach } from "node:test";
import assert from "node:assert/strict";
import { broker, BrokerError, forgetKept, request, verifiedArtifact } from "./api.ts";
import { live } from "./live.ts";
import { mergeEvents } from "./contracts.ts";
import type { Artifact } from "./contracts.ts";

const originalFetch = globalThis.fetch;
afterEach(() => {
  globalThis.fetch = originalFetch;
});
const task = "10000000-0000-4000-8000-000000000001";
test("submissions keep a caller-stable idempotency key and mutation intent", async () => {
  const calls: RequestInit[] = [];
  globalThis.fetch = async (_path, init) => {
    calls.push(init!);
    return Response.json({ task_id: task, status: "queued" }, { status: 202 });
  };
  const input = {
    group: "example",
    profile: "reference",
    objective: "Synthetic task",
  };
  await broker.submit(input, "same-request");
  await broker.submit(input, "same-request");
  assert.equal(
    (calls[0].headers as Record<string, string>)["Idempotency-Key"],
    (calls[1].headers as Record<string, string>)["Idempotency-Key"],
  );
  assert.equal(
    (calls[0].headers as Record<string, string>)["X-Agent-Sandbox-Intent"],
    "mutate",
  );
  assert.equal(calls[0].credentials, "same-origin");
  assert.equal(calls[0].redirect, "error");
});
test("caller-selected hosts and invalid artifact references never reach fetch", async () => {
  let calls = 0;
  globalThis.fetch = async () => {
    calls++;
    return Response.json({});
  };
  await assert.rejects(
    request("https://example.invalid/api/v1/me"),
    BrokerError,
  );
  await assert.rejects(async () => broker.task("../another-user"), BrokerError);
  assert.equal(calls, 0);
});
test("the step drill-down asks for lineage version 3", async () => {
  let path = "";
  globalThis.fetch = async (asked) => {
    path = String(asked);
    return Response.json({});
  };
  await broker.stepLineage(task);
  assert.equal(path, `/api/v1/tasks/${task}/lineage?version=3`);
});
test("authorization errors do not expose response bodies", async () => {
  globalThis.fetch = async () =>
    Response.json({ message: "restricted server detail" }, { status: 403 });
  await assert.rejects(
    broker.task(task),
    (error: unknown) =>
      error instanceof BrokerError &&
      error.code === "access_required" &&
      !error.message.includes("restricted"),
  );
});
test("downloads require matching metadata and exact bytes before returning a file", async () => {
  const bytes = new TextEncoder().encode("synthetic artifact\n");
  const sha256 = [
    ...new Uint8Array(await crypto.subtle.digest("SHA-256", bytes)),
  ]
    .map((x) => x.toString(16).padStart(2, "0"))
    .join("");
  const artifact: Artifact = {
    id: "30000000-0000-4000-8000-000000000001",
    label: "Fixture",
    role: "provenance",
    relative_path: "fixture.txt",
    media_type: "text/plain",
    size_bytes: bytes.length,
    sha256,
    created_at: "2026-01-01T00:00:00Z",
  };
  const response = (body: Uint8Array, transport: Record<string, string> = { "Content-Length": String(bytes.length) }) =>
    new Response(body as Uint8Array<ArrayBuffer>, {
      headers: { ...transport, "X-Artifact-Sha256": sha256 },
    });
  globalThis.fetch = async () => response(bytes);
  assert.equal(
    await (await verifiedArtifact(task, artifact)).text(),
    "synthetic artifact\n",
  );
  const transports: Record<string, string>[] = [{}, { "Content-Encoding": "zstd" },
    { "Content-Encoding": "gzip", "Content-Length": "12" }];
  for (const transport of transports) {
    globalThis.fetch = async () => response(bytes, transport);
    assert.equal(await (await verifiedArtifact(task, artifact)).text(), "synthetic artifact\n");
    globalThis.fetch = async () => response(bytes.slice(1), transport);
    await assert.rejects(verifiedArtifact(task, artifact), BrokerError);
    const changed = bytes.slice(); changed[0] ^= 1;
    globalThis.fetch = async () => response(changed, transport);
    await assert.rejects(verifiedArtifact(task, artifact), BrokerError);
  }
  globalThis.fetch = async () => response(bytes, { "Content-Length": "999" });
  await assert.rejects(verifiedArtifact(task, artifact), BrokerError);
  const corrupt = bytes.slice();
  corrupt[0] ^= 1;
  globalThis.fetch = async () => response(corrupt);
  await assert.rejects(
    verifiedArtifact(task, artifact),
    (e: unknown) =>
      e instanceof BrokerError && e.code === "artifact_integrity_failed",
  );
  globalThis.fetch = async () => response(new Uint8Array(bytes.length + 1));
  await assert.rejects(verifiedArtifact(task, artifact), BrokerError);
});
test("replayed events are deduplicated without changing sequence order", () => {
  const event = {
    sequence: 2,
    kind: "task",
    code: "recorded",
    state: "running",
    occurred_at: "2026-01-01T00:00:00Z",
  };
  assert.deepEqual(
    mergeEvents([event], [event, { ...event, sequence: 1 }]).map(
      (e) => e.sequence,
    ),
    [1, 2],
  );
});
test("scientific history keeps the server cursor and evidence attempt binding", async () => {
  const event = "40000000-0000-4000-8000-000000000001";
  const artifact = "30000000-0000-4000-8000-000000000001";
  const paths: string[] = [];
  globalThis.fetch = async (path) => {
    paths.push(String(path));
    return Response.json({
      coverage: "available", attempt: 1, lease_generation: 9999,
      events: [], evidence: [], next_cursor: null,
    });
  };
  await broker.scientificHistory(task, "opaque-cursor");
  await broker.scientificHistoryEvidence(task, event, 1, 9999, artifact);
  assert.match(paths[0], /scientific-history\?limit=100&cursor=opaque-cursor$/);
  assert.match(paths[1], new RegExp(`/scientific-history/events/${event}/evidence\\?attempt=1&generation=9999&limit=100&after=${artifact}$`));
  globalThis.fetch = async () => Response.json({ error: { code: "scientific_history_cursor_stale" } }, { status: 409 });
  await assert.rejects(broker.scientificHistory(task, "stale"), (error: unknown) => error instanceof BrokerError && error.code === "request_conflict");
});

test("a refusal keeps the broker's fixed code and never its message text", async () => {
  globalThis.fetch = async () => Response.json({ error: { code: "note_evidence_unaccepted", message: "Free text" } }, { status: 409 });
  await assert.rejects(request("/api/v1/me"), (e: unknown) => e instanceof BrokerError && e.code === "request_conflict"
    && e.reason === "note_evidence_unaccepted" && !e.message.includes("Free text"));
  for (const body of [{ error: { code: "Not A Code" } }, { error: { code: 7 } }, { error: "x" }, {}]) {
    globalThis.fetch = async () => Response.json(body, { status: 409 });
    await assert.rejects(request("/api/v1/me"), (e: unknown) => e instanceof BrokerError && e.reason === "");
  }
  globalThis.fetch = async () => new Response("<html>", { status: 502, headers: { "content-type": "text/html" } });
  await assert.rejects(request("/api/v1/me"), (e: unknown) => e instanceof BrokerError && e.code === "request_failed" && e.reason === "");
});

test("empty mutation responses require an explicit 204 contract", async () => {
  globalThis.fetch = async () => new Response(null, { status: 204 });
  assert.equal(await request("/api/v1/research-runs/example/pause", {}, true), undefined);
  await assert.rejects(request("/api/v1/me"), (e: unknown) => e instanceof BrokerError && e.code === "invalid_response");
  globalThis.fetch = async () => Response.json({});
  await assert.rejects(request("/api/v1/research-runs/example/pause", {}, true), (e: unknown) => e instanceof BrokerError && e.code === "invalid_response");
});
test("a project read follows the analyses cursor until every analysis is listed once", async () => {
  // The read keeps the newest analyses; the rest page from /analyses with an opaque cursor.
  const project = "50000000-0000-4000-8000-000000000007";
  const analysis = (n: number) => ({ task_id: `11000000-0000-4000-8000-0000000000${String(n).padStart(2, "0")}`, action_key: `a${n}`,
    parent_task_id: null, profile: "reference", status: "completed", attempt: 1, submitted_at: "2026-10-11T10:00:00Z", updated_at: "2026-10-11T10:00:00Z" });
  const asked: string[] = [];
  globalThis.fetch = async (path) => {
    const url = new URL(String(path), "http://localhost");
    asked.push(`${url.pathname}${url.search}`);
    if (url.pathname.endsWith("/analyses")) {
      const from = Number(url.searchParams.get("cursor"));
      // The second page repeats one analysis, as a page boundary can while new analyses arrive.
      const page = from === 3 ? [analysis(3), analysis(4), analysis(5)] : [analysis(6)];
      return Response.json({ analyses: page, next: from === 3 ? "6" : null, total: 6 });
    }
    return Response.json({ project_id: project, group_key: "lab", owner_id: "u", title: "Long run", visibility: "lab", revision: 1,
      created_at: "t", updated_at: "t", capabilities: ["read"], checkpoint: null, tasks: [analysis(1), analysis(2), analysis(3)],
      task_count: 6, analyses_total: 6, analyses_next: "3" });
  };
  const { projects } = await import("./projects.ts");
  const read = await projects.get(project);
  assert.deepEqual(read.tasks.map((t) => t.action_key), ["a1", "a2", "a3", "a4", "a5", "a6"]);
  assert.equal(read.analyses_next, null);
  assert.deepEqual(asked.slice(1), [`/api/v1/projects/${project}/analyses?cursor=3&limit=200`, `/api/v1/projects/${project}/analyses?cursor=6&limit=200`]);
  // A cursor that does not move ends the reading instead of asking again forever.
  asked.length = 0;
  globalThis.fetch = async (path) => {
    const url = new URL(String(path), "http://localhost");
    asked.push(url.pathname);
    return url.pathname.endsWith("/analyses") ? Response.json({ analyses: [], next: "3", total: 6 })
      : Response.json({ project_id: project, capabilities: [], tasks: [], task_count: 6, analyses_next: "3" });
  };
  await projects.get(project);
  assert.equal(asked.length, 2);
});
test("a project read keeps lab data apart from its analyses and names tasks by their recorded titles", async () => {
  // A template analysis is an analysis; a lab dataset's intake and preparation are not.
  const project = "50000000-0000-4000-8000-000000000007";
  const task = (n: number, extra: Record<string, unknown>) => ({ task_id: `12000000-0000-4000-8000-0000000000${n}`, action_key: `a${n}`,
    parent_task_id: null, profile: "p", status: "completed", attempt: 1, submitted_at: "2026-10-12T10:00:00Z", updated_at: "2026-10-12T10:00:00Z", ...extra });
  const prepared = task(11, { dataset: { title: "Glucose", operation: "preparation", dataset: "d", previous: null } });
  const compared = task(12, { template_analysis: { title: "AUC by diet", feature: "group_compare", source_task: prepared.task_id,
    table_path: "results/lab_data/glucose.csv", previous: null } });
  globalThis.fetch = async () => Response.json({ project_id: project, group_key: "lab", owner_id: "u", title: "Lab data", visibility: "lab",
    revision: 1, created_at: "t", updated_at: "t", capabilities: ["read"], checkpoint: null,
    tasks: [task(10, { dataset: { title: null, operation: "intake", dataset: "d", previous: null } }), prepared, compared], task_count: 3 });
  const { projects, taskTitle } = await import("./projects.ts");
  const read = await projects.get(project);
  assert.deepEqual(read.tasks.map((t) => t.action_key), ["a12"]);
  assert.deepEqual(read.lab_data?.map((t) => t.action_key), ["a10", "a11"]);
  assert.equal(taskTitle(read, compared.task_id), "AUC by diet");
  assert.equal(taskTitle(read, prepared.task_id), "Glucose");
  assert.equal(taskTitle(read, "12000000-0000-4000-8000-000000000099"), null);
});

test("a read sends the kept ETag and a 304 reuses the kept answer; an answer without an ETag is not kept", async () => {
  forgetKept();
  const sent: (string | null)[] = [];
  let body = { tasks: [{ id: task }] };
  globalThis.fetch = async (_path, init) => {
    const match = new Headers(init!.headers).get("If-None-Match");
    sent.push(match);
    if (match === '"v1"') return new Response(null, { status: 304 });
    return Response.json(body, { headers: { ETag: '"v1"' } });
  };
  const first = await broker.tasks();
  body = { tasks: [] };
  const second = await broker.tasks();
  assert.deepEqual(sent, [null, '"v1"']);
  assert.deepEqual(second, first);
  assert.notEqual(second, first, "each answer is its own copy");
  globalThis.fetch = async () => Response.json({ tasks: [] });
  await broker.tasks();
  sent.length = 0;
  globalThis.fetch = async (_path, init) => { sent.push(new Headers(init!.headers).get("If-None-Match")); return Response.json({ tasks: [] }); };
  await broker.tasks();
  assert.deepEqual(sent, [null], "a broker that stopped sending ETags is not asked with an old one");
});

test("a change the reader makes reads again the views that show it, also after a conflict; a read changes nothing", async () => {
  const seen: string[] = [];
  const stop = live().watch(["inbox", `task:${task}`], () => { seen.push("read"); });
  globalThis.fetch = async () => Response.json({ review: { review_id: task } }, { status: 201 });
  await broker.review(task, { decision: "rejected", note: "Synthetic note", request_id: task });
  await new Promise((resolve) => setImmediate(resolve));
  assert.equal(seen.length, 1);
  globalThis.fetch = async () => Response.json({ error: { code: "request_conflict" } }, { status: 409 });
  await assert.rejects(broker.review(task, { decision: "rejected", note: "Synthetic note", request_id: task }), BrokerError);
  await new Promise((resolve) => setImmediate(resolve));
  assert.equal(seen.length, 2);
  globalThis.fetch = async () => Response.json({ reviews: [] });
  await broker.reviews(task);
  await new Promise((resolve) => setImmediate(resolve));
  assert.equal(seen.length, 2);
  stop();
});
