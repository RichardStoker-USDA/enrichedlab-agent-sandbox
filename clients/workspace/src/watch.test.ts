import assert from "node:assert/strict";
import test from "node:test";
import type { TaskEvent } from "./contracts.ts";
import type { HistoryEntry } from "./history.ts";
import { watchLines, type WatchFiles, type WatchInput } from "./watch.ts";

const T = (n: number) => `10000000-0000-4000-8000-00000000000${n}`;
const at = (minute: number) => new Date(Date.UTC(2026, 9, 12, 9, minute)).toISOString();
const plan = [{ task: T(1), title: "Summarize each diet" }, { task: T(2), title: "Compare plasma response" }];
const files: Record<string, WatchFiles> = {
  [T(1)]: { figures: 1, tables: 2, code: 1, logs: 1, other: 0, intact: true },
  [T(3)]: { figures: 0, tables: 1, code: 0, logs: 0, other: 2, intact: false },
};
const input = (over: Partial<WatchInput>): WatchInput => ({
  history: [], tasks: [], toolSteps: {}, now: null, at: at(30),
  step: (id) => { const i = plan.findIndex((s) => s.task === id); return i >= 0 ? { number: i + 1, title: plan[i].title } : null; },
  title: (id) => plan.find((s) => s.task === id)?.title ?? "Rerun of the box plot",
  files: (id) => files[id] ?? null,
  ...over,
});
const decision = (n: number, minute: number, review = "proceed"): HistoryEntry => ({
  key: `turn-run-${n}`, at: at(minute), source: "agent", unverified: true, tone: "quiet", title: "Research agent started an analysis",
  detail: "Summarize each diet before comparing them.", decision: n, check: { decision: review, reason: "The method fits the question." }, taskId: T(1) });

test("a decision reads as the agent's line with its methods check above it, and the request it made above both", () => {
  const lines = watchLines(input({ history: [decision(1, 0)], tasks: [{ task_id: T(1), status: "running", submitted_at: at(0), updated_at: at(1) }] }));
  assert.deepEqual(lines.map((l) => [l.kind, l.text]), [
    ["system", "Step 1 requested: Summarize each diet"],
    ["check", "Methods check agreed: The method fits the question."],
    ["agent", "Decision 1: Summarize each diet before comparing them."],
  ]);
  // A check that did not agree is marked for attention.
  assert.equal(watchLines(input({ history: [decision(2, 0, "needs_review")] })).find((l) => l.kind === "check")?.tone, "attention");
});

test("an ended analysis names its files by kind and its checksum state; one that did not finish names none", () => {
  const lines = watchLines(input({ tasks: [
    { task_id: T(1), status: "completed", submitted_at: at(0), updated_at: at(9) },
    { task_id: T(2), status: "failed", submitted_at: at(1), updated_at: at(8) },
    { task_id: T(3), status: "completed", submitted_at: at(2), updated_at: at(7) },
  ] }));
  const ended = lines.filter((l) => l.key.startsWith("ended:"));
  assert.deepEqual(ended.map((l) => [l.kind, l.tone, l.text]), [
    ["files", "quiet", "Step 1 finished: 1 figure, 2 tables, 1 code file and 1 log, each matching its checksum list"],
    ["system", "failed", "Step 2 did not finish"],
    ["files", "attention", "Analysis “Rerun of the box plot” finished: 1 table and 2 other files; a package does not match its checksum list"],
  ]);
  assert.equal(lines.find((l) => l.key === `requested:${T(3)}`)?.text, "Analysis requested: Rerun of the box plot");
  // An analysis that ended at the instant of the decision that read it sits below that decision.
  const same = watchLines(input({ history: [decision(3, 9)], tasks: [{ task_id: T(1), status: "completed", submitted_at: at(0), updated_at: at(9) }] }));
  assert.deepEqual(same.slice(0, 3).map((l) => l.kind), ["check", "agent", "files"]);
  // A running analysis has no ended line yet.
  assert.equal(watchLines(input({ tasks: [{ task_id: T(1), status: "queued", submitted_at: at(0), updated_at: at(0) }] })).length, 1);
});

test("recorded tool steps appear once, without restating the request or the end", () => {
  const event = (sequence: number, code: string, minute: number): TaskEvent => ({ sequence, kind: "task", state: "running", code, occurred_at: at(minute) });
  const lines = watchLines(input({ toolSteps: { [T(1)]: [event(1, "task_accepted", 0), event(2, "worker_lease_acquired", 4), event(3, "lease_expired_requeued", 6),
    event(4, "agent_started", 7), event(5, "task_completed", 9)] } }));
  assert.deepEqual(lines.map((l) => [l.tone, l.text]), [
    ["quiet", "Step 1: Analysis started"],
    ["attention", "Step 1: Worker stopped responding; queued again"],
    ["quiet", "Step 1: Assigned to a compute worker"],
  ]);
});

test("people's lines keep their words, the broker's bookkeeping is left out, and what runs now leads", () => {
  const history: HistoryEntry[] = [
    { key: "rev-4", at: at(5), source: "you", tone: "quiet", title: "Instruction to the agent", detail: "Treat site as a grouping factor." },
    { key: "rev-5", at: at(6), source: "system", tone: "quiet", title: "Step status updated from the analysis record" },
    { key: `task-${T(1)}-submitted`, at: at(7), source: "system", tone: "quiet", title: "Analysis requested: Summarize each diet" },
    { key: "audit-1", at: at(8), source: "colleague", tone: "quiet", title: "Access changed: A colleague can now review" },
  ];
  const lines = watchLines(input({ history, now: { text: "Waiting for a free compute slot", time: "6 min on this step" } }));
  assert.deepEqual(lines.map((l) => [l.kind, l.text]), [
    ["now", "Waiting for a free compute slot, 6 min on this step"],
    ["colleague", "Access changed: A colleague can now review"],
    ["you", "Your instruction: “Treat site as a grouping factor.”"],
  ]);
});
