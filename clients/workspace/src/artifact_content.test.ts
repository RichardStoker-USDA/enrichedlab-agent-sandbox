import assert from "node:assert/strict";
import test from "node:test";
import { exactText, tablePreview } from "./artifact_content.ts";
import { parseTrajectory, stepArtifacts } from "./trajectory.ts";
import type { Artifact } from "./contracts.ts";

test("CSV retains scientific precision, quoted fields, line breaks, and empty values", () => {
  const table = tablePreview('\ufeffid,value,note\r\n001,9007199254740993,"a,b"\r\n002,0.000000000000000001,"two\nlines"\r\n003,,missing\r\n');
  assert.deepEqual(table.columns, ["id", "value", "note"]);
  assert.deepEqual(table.rows, [["001", "9007199254740993", "a,b"], ["002", "0.000000000000000001", "two\nlines"], ["003", "", "missing"]]);
  assert.equal(table.truncated, false);
});
test("Malformed tables and undecodable bytes have no manufactured table preview", async () => {
  for (const source of ['a,a\n1,2', 'a,b\n1,2,3', 'a,b\n"unfinished,2']) assert.throws(() => tablePreview(source));
  await assert.rejects(exactText(new Blob([new Uint8Array([255])])));
});
test("Controller steps link only exact authorized artifact hashes and omit restricted content", () => {
  const event = { sequence: 1, event_id: "step-1", run_id: "run-1", actor: "agent-1", segment: 1, harness: "native", session_id: null,
    kind: "agent.tool.finished", occurred_at: "2026-01-01T00:00:00Z", source_event_id: "call-1", parent_source_event_id: null,
    metadata: { tool: "analysis", exit_code: 0, output_sha256: "sha256:" + "a".repeat(64), input_sha256: "b".repeat(64) }, restricted_content: "private emitted text" };
  const trajectory = parseTrajectory(JSON.stringify(event));
  const artifact = { id: "owned", sha256: "a".repeat(64) } as Artifact;
  assert.deepEqual(stepArtifacts(trajectory.steps[0], "output", [artifact]), [artifact]);
  assert.deepEqual(stepArtifacts(trajectory.steps[0], "input", [artifact]), []);
  assert.ok(!JSON.stringify(trajectory).includes("private emitted text"));
  assert.throws(() => parseTrajectory(JSON.stringify(event) + '\n' + JSON.stringify(event)));
  assert.throws(() => parseTrajectory(JSON.stringify(event) + '\n' + JSON.stringify({ ...event, sequence: 2, run_id: "other" })));
  assert.equal(parseTrajectory(JSON.stringify({ ...event, metadata: { exit_code: 1 } })).steps[0].state, "failed");
  const start = { ...event, source_event_id: null, kind: "agent.tool.started", metadata: { tool: "inspect", call_id_sha256: "c".repeat(64), is_error: false } };
  const finish = { ...start, event_id: "step-2", sequence: 2, kind: "agent.tool.finished", metadata: { ...start.metadata, is_error: true } };
  const paired = parseTrajectory(JSON.stringify(start) + "\n" + JSON.stringify(finish));
  assert.equal(paired.steps.length, 1);
  assert.equal(paired.steps[0].state, "failed");
});
