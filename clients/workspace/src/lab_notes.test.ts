import assert from "node:assert/strict";
import { test } from "node:test";
import type { LabNote, TaskReview } from "./contracts.ts";
import { noteActions, noteStatus, notesRead, reasonProblem, sharedAs, shareBlocker } from "./lab_notes.ts";

const task = "10000000-0000-4000-8000-000000000001";
const other = "10000000-0000-4000-8000-000000000002";
const project = "50000000-0000-4000-8000-000000000001";
const ref = (t: string) => `artifact://tasks/${t}/31000000-0000-4000-8000-000000000001`;
const note = (over: Partial<LabNote> = {}): LabNote => ({
  note_id: "70000000-0000-4000-8000-000000000001", lab: "open_methods", project_id: project, project_title: "Study", revision: 4,
  kind: "observation", statement: "Diet A raised the response.", statement_sha256: "a".repeat(64),
  evidence: [{ task_id: task, refs: [ref(task)], title: "Summary", method: "evidence_review", outputs: [], review_id: "r1" }],
  methods: ["evidence_review"], data: ["input://synthetic/diet-study"],
  shared_by: { principal_id: "p1", name: "Lab Lead", kind: "person" }, reason: null, shared_at: "2026-09-25T10:00:00Z",
  status: { state: "active", review_by: "2027-03-24T10:00:00Z" }, events: [], ...over,
});
const review = (decision: TaskReview["decision"], over: Partial<TaskReview> = {}): TaskReview => ({
  review_id: "r", task_id: task, attempt: 1, decision, note: decision === "accepted" ? null : "Why.", reviewer_id: "p2",
  reviewer_name: "Reviewer", reviewer_kind: "person", outputs: [], created_at: "2026-09-25T09:00:00Z", covers_current_outputs: true, ...over,
});

test("a note's state is the broker's, in words", () => {
  assert.equal(noteStatus(note()).label, "In use");
  assert.match(noteStatus(note()).sentence, /^The agent reads it in matching projects until /);
  const paused = noteStatus(note({ status: { state: "paused", review_by: "2027-03-24T10:00:00Z", cause: "evidence_files_changed" } }));
  assert.equal(paused.tone, "attention");
  assert.match(paused.sentence, /result files .* changed/);
  assert.match(noteStatus(note({ status: { state: "paused", review_by: "x", cause: "evidence_review_changed" } })).sentence, /not an acceptance/);
  assert.match(noteStatus(note({ status: { state: "expired", review_by: "2026-01-01T00:00:00Z" } })).sentence, /Renew it/);
  const retired = noteStatus(note({ status: { state: "retired", review_by: "x", retired_at: "2026-09-26T00:00:00Z", reason: "Superseded." } }));
  assert.equal(retired.label, "Retired");
  assert.match(retired.sentence, /: Superseded\.$/);
});

test("a note counts as shared until it is retired", () => {
  const notes = [note({ note_id: "old", status: { state: "retired", review_by: "x" } }), note({ note_id: "new" })];
  assert.equal(sharedAs("Diet A raised the response.", project, notes)?.note_id, "new");
  assert.equal(sharedAs("Diet A raised the response.", project, [notes[0]]), null);
  assert.equal(sharedAs("Another note.", project, notes), null);
  assert.equal(sharedAs("Diet A raised the response.", "50000000-0000-4000-8000-000000000009", notes), null);
});

test("a note is offered for sharing only when a person accepted every analysis it rests on", () => {
  const title = (t: string) => (t === task ? "Summary" : "Comparison");
  const lesson = { kind: "observation", evidence_refs: [ref(task), ref(other), "input://synthetic/diet-study"] };
  assert.match(shareBlocker({ ...lesson, kind: "hypothesis" }, {}, title), /Only an observation/);
  assert.match(shareBlocker({ kind: "observation", evidence_refs: ["input://synthetic/diet-study"] }, {}, title), /rests on an analysis/);
  assert.equal(shareBlocker(lesson, { [task]: review("accepted") }, title), "A person must accept “Comparison” before this note can be shared with the lab.");
  assert.equal(shareBlocker(lesson, {}, title), "A person must accept “Summary” and “Comparison” before this note can be shared with the lab.");
  const accepted = { [task]: review("accepted"), [other]: review("accepted", { task_id: other }) };
  assert.equal(shareBlocker(lesson, accepted, title), "");
  // A request for changes, a service account's acceptance and an acceptance of earlier files do not count.
  for (const r of [review("needs_changes"), review("accepted", { reviewer_kind: "service_account" }), review("accepted", { covers_current_outputs: false })])
    assert.notEqual(shareBlocker(lesson, { ...accepted, [task]: r }, title), "");
});

test("the notes a decision read are matched to the lab's list", () => {
  const record = { context_inputs: { lab_notes: [{ note_id: "70000000-0000-4000-8000-000000000001", statement_sha256: "a" }, { note_id: "gone", statement_sha256: "b" }] } };
  const read = notesRead(record, [note()]);
  assert.equal(read.length, 2);
  assert.equal(read[0].note?.statement, "Diet A raised the response.");
  assert.equal(read[1].note, null);
  assert.deepEqual(notesRead(undefined, [note()]), []);
  assert.deepEqual(notesRead({}, [note()]), []);
});

test("only the sharer or a lead or reviewer retires, only a lead or reviewer renews, and never a retired note", () => {
  assert.deepEqual(noteActions(note(), "p1", false), { retire: true, renew: false });
  assert.deepEqual(noteActions(note(), "p9", false), { retire: false, renew: false });
  assert.deepEqual(noteActions(note(), "p9", true), { retire: true, renew: true });
  assert.deepEqual(noteActions(note({ status: { state: "retired", review_by: "x" } }), "p1", true), { retire: false, renew: false });
  assert.equal(reasonProblem("  ", true), "Say why it should no longer be used.");
  assert.equal(reasonProblem("", false), "");
  assert.match(reasonProblem("x".repeat(601), false), /600 characters/);
});
