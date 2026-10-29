import assert from "node:assert/strict";
import test from "node:test";
import { duration, nowLine, placeWords, paceWords, tally, tallyWords, type StepTimes } from "./live_words.ts";
import { sinceLooked, stepNumbers, type SinceInput } from "./since.ts";

const at = "2026-10-12T08:00:00Z";
const T = (n: number) => `10000000-0000-4000-8000-00000000000${n}`;
const input = (over: Partial<SinceInput>): SinceInput => ({
  at, plan: [{ title: "Summarize each diet", task_id: T(1) }, { title: "Compare plasma response", task_id: T(2) }, { title: "Check units", task_id: T(3) }],
  tasks: [], files: [], turns: [], instructions: [], answer: null, ...over });
const pkg = (task: number, created_at: string, intact = true) => ({ task_id: T(task), created_at, figures: 2, tables: 3, code: 1, logs: 1, other: 0, intact });

test("step numbers read as a person says them", () => {
  assert.equal(stepNumbers([3]), "Step 3");
  assert.equal(stepNumbers([13, 9, 10, 11, 12]), "Steps 9 to 13");
  assert.equal(stepNumbers([2, 7, 4]), "Steps 2, 4 and 7");
});

test("nothing newer than the last look gives no rows", () => {
  const rows = sinceLooked(input({
    tasks: [{ task_id: T(1), status: "completed", updated_at: "2026-10-12T07:00:00Z" }],
    files: [pkg(1, "2026-10-12T07:00:00Z")],
    answer: { answer: "partly_answered", at: "2026-10-12T07:30:00Z", findings: 2, parts: [] },
  }));
  assert.deepEqual(rows, []);
});

test("finished steps, new files by kind and their checks are counted from the records", () => {
  const rows = sinceLooked(input({
    tasks: [{ task_id: T(1), status: "completed", updated_at: "2026-10-12T09:00:00Z" }, { task_id: T(2), status: "completed", updated_at: "2026-10-12T10:00:00Z" },
      { task_id: T(3), status: "failed", updated_at: "2026-10-12T10:30:00Z" }],
    files: [pkg(1, "2026-10-12T09:00:00Z"), pkg(2, "2026-10-12T10:00:00Z")],
    turns: [{ turn: 2, created_at: "2026-10-12T08:30:00Z", outcome: "proceed", review: "proceed", proposed: 1 },
      { turn: 3, created_at: "2026-10-12T09:30:00Z", outcome: "revise", review: "revise", proposed: 0 },
      { turn: 4, created_at: "2026-10-12T09:40:00Z", outcome: "proceed", review: "proceed", proposed: 1 }],
  }));
  assert.deepEqual(rows.map((r) => r.key), ["steps", "stopped", "files", "checks", "answer"]);
  assert.equal(rows[0].title, "2 steps finished");
  assert.equal(rows[0].detail, "Steps 1 and 2. Every file they made matches its checksum.");
  assert.equal(rows[1].title, "1 step did not finish");
  assert.equal(rows[1].detail, "Step 3: Check units. Its record says why.");
  assert.equal(rows[2].title, "14 new files");
  assert.equal(rows[2].detail, "4 figures, 6 tables, 2 code files and 2 logs");
  assert.equal(rows[3].title, "4 checks passed; 1 sent back and redone");
  assert.equal(rows[3].detail, "The methods check agreed with 2 proposals and sent one back to the agent. Both new result packages match their checksum lists.");
  assert.deepEqual([rows[4].title, rows[4].tone], ["No answer yet", "quiet"]);
});

test("a package that does not match its checksum list is said plainly", () => {
  const rows = sinceLooked(input({
    tasks: [{ task_id: T(2), status: "completed", updated_at: "2026-10-12T09:00:00Z" }],
    files: [pkg(2, "2026-10-12T09:00:00Z", false)],
  }));
  assert.equal(rows[0].detail, "Step 2: Compare plasma response. 1 package does not match its checksum list.");
  assert.equal(rows[0].tone, "attention");
  assert.equal(rows.find((r) => r.key === "checks")?.title, "1 file check failed");
});

test("a new answer leads, with its findings and the parts it settled", () => {
  const rows = sinceLooked(input({
    answer: { answer: "partly_answered", at: "2026-10-12T11:00:00Z", findings: 2,
      parts: [{ state: "supported" }, { state: "open" }, { state: "unsupported" }, { state: "awaiting" }] },
    instructions: [{ text: "Treat site as a grouping factor in every comparison.", read: { turn: 27, at: "2026-10-12T09:00:00Z" } }],
  }));
  assert.deepEqual(rows.map((r) => r.key), ["answer", "instructions"]);
  assert.equal(rows[0].title, "It says the results answer part of your question");
  assert.equal(rows[0].detail, "2 findings, each value read from a result file. It has settled 2 of the question's 4 parts.");
  assert.equal(rows[1].title, "It read an instruction at decision 27");
  assert.equal(rows[1].detail, "“Treat site as a grouping factor in every comparison.”");
});

test("an older answer still stands when other things changed", () => {
  const rows = sinceLooked(input({
    tasks: [{ task_id: T(1), status: "completed", updated_at: "2026-10-12T09:00:00Z" }],
    answer: { answer: "answered", at: "2026-10-12T07:00:00Z", findings: 1, parts: [] },
  }));
  assert.deepEqual(rows.at(-1), { key: "answer", title: "No new answer", detail: "Its earlier answer still stands.", tone: "quiet" });
});

test("durations read in minutes, hours and days", () => {
  assert.equal(duration(30_000), "under a minute");
  assert.equal(duration(6 * 60_000), "6 min");
  assert.equal(duration(72 * 60_000), "1 h 12 min");
  assert.equal(duration(120 * 60_000), "2 h");
  assert.equal(duration(27 * 3600_000), "1 day 3 h");
});

test("the now line names only recorded states and counts time on the step", () => {
  const now = Date.parse("2026-10-12T10:06:00Z");
  // A broker that does not record the queue leaves a note that says so instead of leaving it out.
  assert.deepEqual(nowLine(undefined, [{ status: "queued", submitted_at: "2026-10-12T10:00:00Z" }], now),
    { text: "Waiting for a free compute slot", time: "6 min on this step", note: "Its place in line is not recorded yet." });
  assert.equal(nowLine(undefined, [{ status: "queued", submitted_at: "2026-10-12T10:00:00Z", queue: { ahead: 3, lab_full: false, person_full: true } }], now)?.note,
    "3 analyses submitted earlier are ahead of it. The person who asked for it is using all of their slots.");
  assert.deepEqual(nowLine(undefined, [{ status: "running", submitted_at: "2026-10-12T09:00:00Z" }, { status: "queued", submitted_at: "2026-10-12T10:00:00Z" }], now),
    { text: "Running the analysis, and 1 more analysis at the same time", time: "1 h 6 min on this step" });
  assert.deepEqual(nowLine({ status: "running", code: null, updated_at: "2026-10-12T10:05:00Z" }, [], now), { text: "Choosing the next step", time: "1 min so far" });
  assert.equal(nowLine({ status: "review_required", code: null, updated_at: "2026-10-12T10:05:00Z" }, [], now), null);
  assert.equal(nowLine(undefined, [{ status: "mystery", submitted_at: "2026-10-12T10:00:00Z" }], now), null);
});

test("the tally counts steps against the plan", () => {
  assert.deepEqual(tally(["done", "done", "current", "later", "failed"]), { done: 2, now: 1, later: 1, failed: 1, check: 0 });
});

test("a long plan's counts name every state, so a step that failed or needs a check is never folded into finished", () => {
  assert.deepEqual(tallyWords(["done", "done", "check", "failed", "current", "later", "later"]),
    ["2 finished", "1 working", "1 to check", "1 did not finish", "2 planned"]);
  assert.deepEqual(tallyWords(["done"]), ["1 finished"]);
  assert.deepEqual(tallyWords([]), []);
});

test("the pace of recent analyses comes from their recorded times, newest five against the five before", () => {
  // An analysis asked for at minute s, started after w minutes in the queue, ran r minutes.
  const timed = (s: number, w: number, r: number): StepTimes => ({ submitted_at: new Date(Date.UTC(2026, 9, 12, 8, s)).toISOString(),
    started_at: new Date(Date.UTC(2026, 9, 12, 8, s + w)).toISOString(), finished_at: new Date(Date.UTC(2026, 9, 12, 8, s + w + r)).toISOString() });
  const earlier = [timed(0, 1, 12), timed(2, 2, 11), timed(4, 0, 13), timed(6, 3, 12), timed(8, 1, 12)];
  const recent = [timed(60, 16, 12), timed(62, 17, 13), timed(64, 15, 11), timed(66, 18, 12), timed(68, 1, 16)];
  assert.equal(paceWords([...recent, ...earlier]),
    "The last 5 analyses waited 16 min for a compute slot and ran 12 min, at the median. The 5 before them waited 1 min and ran 12 min.");
  // The order the records arrive in does not matter: the newest are the ones that ended last.
  assert.equal(paceWords([...earlier, ...recent].reverse()), paceWords([...recent, ...earlier]));
  // Without five earlier analyses to compare, only the recent ones are described; with fewer than three, nothing.
  assert.equal(paceWords([...recent, earlier[0]]), "The last 5 analyses waited 16 min for a compute slot and ran 12 min, at the median.");
  assert.equal(paceWords(recent.slice(0, 2)), null);
  // An analysis without a recorded start or end is left out rather than guessed.
  assert.equal(paceWords([...recent.slice(0, 2), { submitted_at: recent[2].submitted_at, started_at: null, finished_at: null }]), null);
  assert.equal(paceWords([timed(0, 0, 0.5), timed(1, 0, 0.5), timed(2, 0, 0.5)]),
    "The last 3 analyses waited under a minute for a compute slot and ran under a minute, at the median.");
});

test("a queued analysis's place reads in the broker's terms", () => {
  assert.equal(placeWords(undefined), "Its place in line is not recorded yet.");
  assert.equal(placeWords(null), "Its place in line is not recorded yet.");
  assert.equal(placeWords({ ahead: 0, lab_full: false, person_full: false }), "It is next in line.");
  assert.equal(placeWords({ ahead: 1, lab_full: false, person_full: false }), "1 analysis submitted earlier is ahead of it.");
  assert.equal(placeWords({ ahead: 1200, lab_full: false, person_full: false }), "1,200 analyses submitted earlier are ahead of it.");
  // Next in line can still wait: a limit says why.
  assert.equal(placeWords({ ahead: 0, lab_full: true, person_full: false }), "It is next in line. The lab's compute slots are all in use.");
  assert.equal(placeWords({ ahead: null, lab_full: false, person_full: false }),
    "It cannot start as things stand: its access, method or attempts hold it back.");
});
