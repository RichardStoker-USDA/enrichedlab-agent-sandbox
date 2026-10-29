import assert from "node:assert/strict";
import test from "node:test";
import { archivedStepCount, earlierReasons, removedSteps, stepMoments, type PlanLedger, type PlanRevision, type PlanStepChange } from "./plan_ledger.ts";

const me = "00000000-0000-4000-8000-000000000001";
const task = "00000000-0000-4000-8000-0000000000a1";
const personName = (id: string | undefined) => (id === me ? "you" : "Dana Reviewer");

const change = (over: Partial<PlanStepChange>): PlanStepChange => ({
  action_key: "inspect-source", step_number: 1, changes: ["status"], status_before: "active", status_after: "completed",
  task_id: task, result_count: 0, title: null, rationale: null, task_status: null, error_code: null, ...over,
});
const revision = (n: number, source: PlanRevision["source"], steps: PlanStepChange[], over: Partial<PlanRevision> = {}): PlanRevision => ({
  revision: n, recorded_at: `2026-09-26T08:0${n}:00Z`, source, run_id: source === "lead_turn" || source === "analysis_record" ? "run" : null,
  turn: source === "lead_turn" ? 1 : null, principal_id: source === "person" ? me : null, steps, ...over,
});
const ledger = (revisions: PlanRevision[]): PlanLedger => ({ revisions, unrecorded_revisions: 0, truncated: false });

const planned = change({ changes: ["planned"], status_before: null, status_after: "active", title: "Inspect the input units",
  rationale: "Confirm the experimental unit first." });

test("a step's history names what wrote each change and why its analysis stopped", () => {
  const record = ledger([
    revision(1, "person", []),
    revision(2, "lead_turn", [planned]),
    revision(3, "analysis_record", [change({ status_after: "uncertain", task_status: "failed", error_code: "worker_lease_expired" })]),
    revision(4, "person", [change({ changes: ["text", "status"], status_before: "uncertain", status_after: "failed",
      title: "Inspect the input units", rationale: "The worker stopped before the units were confirmed." })]),
  ]);
  assert.deepEqual(stepMoments(record, "inspect-source", personName).map((m) => m.text), [
    "The agent planned it and started its analysis",
    "Its results need a check: its compute worker stopped responding",
    "You revised its reason",
    "You marked it as not finished",
  ]);
  assert.deepEqual(stepMoments(record, "inspect-source", personName).map((m) => m.at), [
    "2026-09-26T08:02:00Z", "2026-09-26T08:03:00Z", "2026-09-26T08:04:00Z", "2026-09-26T08:04:00Z",
  ]);
  // Only the reasons it carried before its current one; the current reason is on the step itself.
  assert.deepEqual(earlierReasons(record, "inspect-source"), [{ at: "2026-09-26T08:02:00Z", rationale: "Confirm the experimental unit first." }]);
});

test("the system's own records say finished or did not finish; an unknown failure code adds no invented reason", () => {
  const finished = ledger([revision(2, "lead_turn", [planned]), revision(3, "analysis_record", [change({ result_count: 2, changes: ["status", "results"] })])]);
  assert.deepEqual(stepMoments(finished, "inspect-source", personName).map((m) => m.text).slice(1), ["Its analysis finished"]);
  const failed = ledger([revision(2, "lead_turn", [planned]), revision(3, "analysis_record", [change({ status_after: "failed", error_code: "sandbox_exit_nonzero" })])]);
  assert.deepEqual(stepMoments(failed, "inspect-source", personName).map((m) => m.text).slice(1), ["Its analysis did not finish"]);
});

test("changes whose writer was not recorded say what changed without naming anyone", () => {
  const record = ledger([
    revision(1, "recorded_before_ledger", [change({ changes: ["planned"], status_before: null, status_after: "pending", task_id: null, title: "Compare", rationale: "Why." })]),
    revision(2, "recorded_before_ledger", [change({ status_before: "pending", status_after: "skipped", task_id: null })]),
  ]);
  assert.deepEqual(stepMoments(record, "inspect-source", personName).map((m) => m.text), ["Planned", "It was marked skipped"]);
});

test("steps taken out of the plan list who took them out, unless the plan holds them again", () => {
  const record = ledger([
    revision(2, "lead_turn", [planned]),
    revision(3, "service_account", [
      change({ action_key: "compare-groups", changes: ["planned"], status_before: null, status_after: "pending", task_id: null, title: "Compare the groups", rationale: "Why." }),
      change({ changes: ["removed"], status_before: "active", status_after: null }),
    ], { principal_id: "00000000-0000-4000-8000-0000000000c1" }),
    revision(4, "person", [change({ action_key: "compare-groups", changes: ["removed"], status_before: "pending", status_after: null, task_id: null })]),
    revision(5, "person", [change({ action_key: "compare-groups", changes: ["planned"], status_before: null, status_after: "pending", task_id: null, title: "Compare the groups", rationale: "Back." })]),
  ]);
  assert.deepEqual(removedSteps(record, ["compare-groups"], personName), [
    { key: "inspect-source", title: "Inspect the input units", at: "2026-09-26T08:03:00Z", by: "an automated caller", task_id: task },
  ]);
  // A removed step's rows do not become history lines of the plan's current steps.
  assert.deepEqual(stepMoments(record, "inspect-source", personName).map((m) => m.text), ["The agent planned it and started its analysis"]);
});

test("finished steps moved into the plan's archive are counted, not listed as taken out", () => {
  // The system moves finished steps out of a full plan; they were not taken out by anyone.
  const archived = change({ changes: ["removed"], status_before: "completed", status_after: null });
  const other = change({ action_key: "compare-groups", changes: ["removed"], status_before: "failed", status_after: null });
  const record = ledger([
    revision(1, "person", []),
    revision(2, "lead_turn", [planned]),
    revision(3, "plan_archive", [archived], { run_id: "run" }),
    revision(4, "person", [other]),
  ]);
  assert.deepEqual(removedSteps(record, [], personName).map((s) => [s.key, s.by]), [["compare-groups", "you"]]);
  assert.equal(archivedStepCount(record, []), 1);
  // A key planned again after it was archived is in the plan, so it is not counted.
  const again = ledger([...record.revisions, revision(5, "lead_turn", [change({ changes: ["planned"], status_before: null,
    status_after: "active" })])]);
  assert.equal(archivedStepCount(again, ["inspect-source"]), 0);
});
