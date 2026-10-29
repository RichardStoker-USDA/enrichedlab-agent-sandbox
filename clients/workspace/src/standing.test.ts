import assert from "node:assert/strict";
import test from "node:test";
import { INSTRUCTION, inputWords, instructionHistory, instructionsFrom, reviewSentence, standingOf, standingSentence, stepLine, withInstruction, withoutInstruction, type IndexFacts } from "./standing.ts";

const facts = (over: Partial<IndexFacts> & { run?: Partial<NonNullable<IndexFacts["latest_run"]>> | null } = {}): IndexFacts => {
  const { run, ...rest } = over;
  return {
    question: "Does dose change tooth length?",
    capabilities: ["read", "review", "contribute", "steer", "cancel", "manage"],
    latest_run: run === null || run === undefined ? null : { run_id: "r", status: "running", code: null, turn: 1, max_turns: 6, updated_at: "", ...run },
    task_counts: { total: 0, active: 0, finished: 0, failed: 0 },
    ...rest,
  };
};

test("work in progress wins over everything else", () => {
  assert.equal(standingOf(facts({ run: { status: "waiting" } })), "working");
  assert.equal(standingOf(facts({ run: { status: "review_required" }, task_counts: { total: 2, active: 1, finished: 1, failed: 0 } })), "working");
});

test("a stopped session waits for the reader only when the reader can act on it", () => {
  assert.equal(standingOf(facts({ run: { status: "review_required", code: "turn_limit" } })), "waiting");
  assert.equal(standingOf(facts({ run: { status: "paused", code: "model_unavailable" } })), "waiting");
  assert.equal(standingOf(facts({ run: { status: "paused", code: "user_paused" } })), "paused");
});

// A read-only reader's project was once marked "Finished" beside "The agent stopped for
// review by someone who can steer this project": a stop for someone else's review is waiting, not finished.
test("a stop a reader cannot act on waits for someone else's review and never reads as finished", () => {
  const readOnly = (run: Partial<NonNullable<IndexFacts["latest_run"]>>) =>
    facts({ run, capabilities: ["read"], task_counts: { total: 1, active: 0, finished: 1, failed: 0 } });
  assert.equal(standingOf(readOnly({ status: "review_required" })), "waiting_other");
  assert.equal(standingOf(readOnly({ status: "paused", code: "model_unavailable" })), "waiting_other");
  assert.equal(standingSentence(readOnly({ status: "review_required" })),
    "The agent stopped for review by someone who can steer this project (1 analysis finished).");
  assert.doesNotMatch(standingSentence(readOnly({ status: "review_required" })), /^Finished/);
});

test("finished, did not finish and not started come from the records, never from a claim of an answer", () => {
  assert.equal(standingOf(facts({ task_counts: { total: 2, active: 0, finished: 1, failed: 1 } })), "finished");
  assert.equal(standingOf(facts({ run: { status: "failed" } })), "not_finished");
  assert.equal(standingOf(facts({ task_counts: { total: 1, active: 0, finished: 0, failed: 1 } })), "not_finished");
  assert.equal(standingOf(facts({ question: null })), "not_started");
  assert.equal(standingOf(facts()), "not_started");
  for (const f of [facts({ run: { status: "review_required" } }), facts({ task_counts: { total: 1, active: 0, finished: 1, failed: 0 } })])
    assert.doesNotMatch(standingSentence(f), /answer/i);
});

test("a stop for review reads as finished once a person accepted every finished analysis", () => {
  const counts = (reviewed: number, extra = {}) => ({ total: 2, active: 0, finished: 2, failed: 0, reviewed, ...extra });
  assert.equal(standingOf(facts({ run: { status: "review_required" }, task_counts: counts(1) })), "waiting");
  assert.equal(standingOf(facts({ run: { status: "review_required" }, task_counts: counts(2, { changes: 1 }) })), "waiting");
  assert.equal(standingOf(facts({ run: { status: "review_required" }, task_counts: counts(2, { rejected: 1 }) })), "waiting");
  assert.equal(standingOf(facts({ run: { status: "review_required" }, task_counts: { total: 2, active: 0, finished: 2, failed: 0 } })), "waiting");
  const accepted = facts({ run: { status: "review_required" }, task_counts: counts(2) });
  assert.equal(standingOf(accepted), "finished");
  assert.equal(standingSentence(accepted), "Finished: 2 analyses finished. A person accepted each one.");
  // With a failed analysis beside them, the sentence says which ones a person accepted.
  assert.equal(standingSentence({ ...accepted, task_counts: { ...accepted.task_counts, failed: 1 } }),
    "Finished: 2 analyses finished, 1 did not finish. A person accepted each one that finished.");
  assert.equal(standingOf(facts({ run: { status: "review_required" }, capabilities: ["read"], task_counts: counts(2) })), "finished");
});

test("each standing reads as one plain sentence", () => {
  assert.equal(standingSentence(facts({ task_counts: { total: 3, active: 2, finished: 1, failed: 0 } })), "Working now: 2 analyses running.");
  assert.equal(standingSentence(facts({ run: { status: "review_required" }, task_counts: { total: 2, active: 0, finished: 2, failed: 0 } })),
    "The agent stopped and asks you to review what it found (2 analyses finished).");
  assert.equal(standingSentence(facts({ task_counts: { total: 3, active: 0, finished: 2, failed: 1 } })), "Finished: 2 analyses finished, 1 did not finish.");
  assert.equal(standingSentence(facts({ question: null })), "No question saved yet.");
});

test("the step line marks finished, current and later steps in plan order", () => {
  assert.deepEqual(stepLine([{ status: "completed", title: "a" }, { status: "active", title: "b" }, { status: "pending", title: "c" }, { status: "uncertain", title: "d" }]).map((s) => s.mark),
    ["done", "current", "later", "check"]);
});

test("an instruction joins the constraints, leads the next step and keeps its first saved version", () => {
  const state = { goal: "g", constraints: ["Treat each animal as the unit."], next_action: "old" };
  const next = withInstruction(state, "  Also test   the interaction. ")!;
  assert.deepEqual(next.constraints, ["Treat each animal as the unit.", `${INSTRUCTION}Also test the interaction.`]);
  assert.equal(next.next_action, "Follow the scientist's newest instruction: Also test the interaction.");
  assert.equal(withInstruction(state, "   "), null);
  assert.equal(withInstruction(state, "x".repeat(901)), null);
  assert.equal(withInstruction({ ...state, constraints: Array(32).fill("c") }, "more"), null);
  const saved = [
    { revision: 3, author_id: "b", created_at: "t3", state: { constraints: [next.constraints[1]] } },
    { revision: 2, author_id: "a", created_at: "t2", state: { constraints: [next.constraints[1]] } },
    { revision: 1, author_id: "a", created_at: "t1", state: { constraints: ["Treat each animal as the unit."] } },
  ];
  assert.deepEqual(instructionsFrom(saved), [{ text: "Also test the interaction.", revision: 2, author_id: "a", at: "t2" }]);
});

test("inputs are named by kind and name, without their identifier", () => {
  assert.deepEqual(inputWords("input://public/toothgrowth"), { kind: "Public reference data", name: "Toothgrowth" });
  assert.deepEqual(inputWords("input://synthetic/diet-study"), { kind: "Synthetic example data", name: "Diet study" });
  assert.deepEqual(inputWords("input://reference/toothgrowth-20260922"), { kind: "Public reference data", name: "Toothgrowth, copy of 2026-09-22" });
});

test("a row says whether a person reviewed the finished analyses, and only when the broker counts them", () => {
  const counts = (patch: object) => ({ total: 3, active: 0, finished: 3, failed: 0, ...patch });
  assert.equal(reviewSentence(counts({})), "");
  assert.equal(reviewSentence(counts({ finished: 0, reviewed: 0 })), "");
  assert.equal(reviewSentence(counts({ reviewed: 0, rejected: 0, changes: 0 })), "Not yet reviewed by a person.");
  assert.equal(reviewSentence(counts({ reviewed: 1, rejected: 0, changes: 0 })), "Reviewed by a person: 1 of 3.");
  assert.equal(reviewSentence(counts({ reviewed: 3, rejected: 1, changes: 1 })), "Reviewed by a person: 3 of 3 (1 rejected, 1 with changes requested).");
});

test("an instruction is read by the first decision whose saved context held it, and a withdrawal closes it", () => {
  const me = "p-me", colleague = "p-colleague";
  const at = (m: number) => `2026-09-26T08:${String(m).padStart(2, "0")}:00Z`;
  const save = (revision: number, author: string, constraints: string[]) => ({ revision, author_id: author, created_at: at(revision), state: { constraints } });
  const a = `${INSTRUCTION}Also show each group's raw values.`, b = `${INSTRUCTION}Use the median.`;
  const checkpoints = [
    save(1, me, ["Use the animal as the unit."]),
    save(2, me, ["Use the animal as the unit.", a]),
    save(3, colleague, ["Use the animal as the unit.", a, b]),
    save(4, me, ["Use the animal as the unit.", b]),
    save(5, me, ["Use the animal as the unit.", b, a]),
  ];
  // Decision 1 read version 1 (neither instruction); decision 2 read version 3 (both); none has read version 5 yet.
  const decisions = [{ run: "r1", turn: 1, context_revision: 1, created_at: at(10) }, { run: "r2", turn: 1, context_revision: 3, created_at: at(20) }];
  const history = instructionHistory(checkpoints, decisions);
  assert.deepEqual(history.map((h) => [h.text, h.revision, h.author_id, h.read?.run ?? null, h.withdrawn?.revision ?? null]), [
    ["Also show each group's raw values.", 2, me, "r2", 4],
    ["Use the median.", 3, colleague, "r2", null],
    // The same text given again after its withdrawal is a new instruction nobody has read yet.
    ["Also show each group's raw values.", 5, me, null, null],
  ]);
  // A decision that read an older version never counts as reading a newer instruction.
  assert.equal(instructionHistory(checkpoints, [decisions[0]]).every((h) => h.read === null), true);
  // Earlier callers still see each instruction once, with the version that first carried it.
  assert.deepEqual(instructionsFrom(checkpoints).map((i) => i.revision), [2, 3]);
});

test("withdrawing an instruction keeps everything else and hands the next step back to the newest remaining one", () => {
  const state = withInstruction(withInstruction({ constraints: ["Use the animal as the unit."], next_action: "Run it." }, "First.")!, "Second.")!;
  assert.equal(state.next_action, "Follow the scientist's newest instruction: Second.");
  const withoutNewest = withoutInstruction(state, "Second.")!;
  assert.deepEqual(withoutNewest.constraints, ["Use the animal as the unit.", `${INSTRUCTION}First.`]);
  assert.equal(withoutNewest.next_action, "Follow the scientist's newest instruction: First.");
  // Withdrawing an older instruction leaves the next step alone.
  assert.equal(withoutInstruction(state, "First.")!.next_action, "Follow the scientist's newest instruction: Second.");
  const none = withoutInstruction(withoutNewest, "First.")!;
  assert.deepEqual(none.constraints, ["Use the animal as the unit."]);
  assert.equal(none.next_action, "Continue with the question and constraints as saved.");
  // An instruction the version no longer holds cannot be withdrawn again.
  assert.equal(withoutInstruction(none, "First."), null);
});
