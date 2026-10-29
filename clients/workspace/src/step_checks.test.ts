import { test } from "node:test";
import assert from "node:assert/strict";
import { checkLabel, checkSentence, checkTone, checksFor, type StepChecks } from "./step_checks.ts";

const task = "40000000-0000-4000-8000-000000000002";
const ref = (n: number) => `artifact://tasks/${task}/3100000${n}-0000-4000-8000-000000000003`;
const turn = (n: number, step_checks?: StepChecks) => ({ turn: n, record: { step_checks } });

test("each result shows its newest check across the run's decisions", () => {
  const turns = [
    turn(1, { checks: [{ key: "rank", task_id: task, ref: ref(1), state: "not_checkable", reasons: ["The result's summary does not cover column protein_g."] }], omitted: 0 }),
    turn(3, { checks: [{ key: "rank", task_id: task, ref: ref(1), state: "failed", reasons: ["Column protein_g goes up to 131.5, above the 100 expected (grams in 100 grams)."] },
      { key: "other", task_id: "50000000-0000-4000-8000-000000000002", ref: "artifact://tasks/x/y", state: "passed" }], omitted: 0 }),
    turn(2),
  ];
  const checks = checksFor(turns, task);
  assert.equal(checks.length, 1);
  assert.equal(checks[0].state, "failed");
  assert.deepEqual(checksFor([turn(1), turn(2)], task), []);
});

test("each state reads as a plain mark and sentence", () => {
  assert.deepEqual([checkTone("passed"), checkTone("failed"), checkTone("not_checkable")], ["done", "failed", "attention"]);
  assert.deepEqual([checkLabel("passed"), checkLabel("failed"), checkLabel("not_checkable")], ["Checked", "Check failed", "Not fully checked"]);
  assert.match(checkSentence("failed"), /no finding or later step rests on it/);
});
