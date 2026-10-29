import assert from "node:assert/strict";
import test from "node:test";
import type { TaskReview } from "./contracts.ts";
import { coverageSentence, findingFlag, reviewerName, reviewMark, reviewProblem } from "./person_review.ts";

const review = (patch: Partial<TaskReview> = {}): TaskReview => ({
  review_id: "r1", task_id: "t1", attempt: 1, decision: "accepted", note: null, reviewer_id: "p2", reviewer_name: "Reviewer One", reviewer_kind: "person",
  outputs: [{ artifact_id: "a1", relative_path: "tasks/t1/payload", sha256: "a".repeat(64) }],
  created_at: "2026-09-25T12:00:00Z", covers_current_outputs: true, ...patch,
});

test("a review is named by its decision and the reviewer, and the reader is you", () => {
  assert.deepEqual(reviewMark(review(), "p1"), { tone: "done", label: "Accepted by Reviewer One" });
  assert.deepEqual(reviewMark(review({ decision: "needs_changes" }), "p2"), { tone: "attention", label: "Changes requested by you" });
  assert.deepEqual(reviewMark(review({ decision: "rejected", reviewer_name: "  " }), "p1"), { tone: "failed", label: "Rejected by a colleague" });
});

test("a row a service account recorded is named as such and never marked like a person's decision", () => {
  const machine = review({ reviewer_kind: "service_account", reviewer_name: "Qualification caller", reviewer_id: "p1" });
  assert.equal(reviewerName(machine, "p1"), "the service account Qualification caller");
  assert.deepEqual(reviewMark(machine, "p1"), { tone: "quiet", label: "Accepted by the service account Qualification caller" });
  assert.equal(reviewerName(review({ reviewer_kind: "service_account", reviewer_name: null }), "p1"), "a service account");
});

test("a review says which result files it covers and when they changed after it", () => {
  assert.equal(coverageSentence(review()), "It covers the analysis's result file as it is now, by its checksum.");
  const two = review({ outputs: [...review().outputs, { artifact_id: "a2", relative_path: "tasks/t1/sums", sha256: "b".repeat(64) }] });
  assert.equal(coverageSentence(two), "It covers the analysis's 2 result files as they are now, by their checksums.");
  assert.equal(coverageSentence(review({ covers_current_outputs: false })), "The result files changed after this review; it covered the earlier set.");
  assert.equal(coverageSentence(review({ outputs: [], decision: "rejected" })), "The analysis had no result files when this review was recorded.");
});

test("a decision other than acceptance needs a note, and acceptance needs results", () => {
  assert.equal(reviewProblem(null, "", true), "Choose a decision.");
  assert.equal(reviewProblem("accepted", "", true), "");
  assert.equal(reviewProblem("accepted", "", false), "Only an analysis that finished with result files can be accepted.");
  assert.equal(reviewProblem("needs_changes", " \n\t", true), "Say what should change.");
  assert.equal(reviewProblem("rejected", "", true), "Say why these results should not be used.");
  assert.equal(reviewProblem("rejected", "x".repeat(2001), true), "Keep the note to 2,000 characters or fewer.");
  assert.equal(reviewProblem("rejected", "The input was the wrong file.", false), "");
});

test("a finding is marked when a person rejected or asked for changes to an analysis it reads", () => {
  const changes = review({ task_id: "t1", decision: "needs_changes", note: "Pool the doses." });
  const rejected = review({ task_id: "t2", decision: "rejected", note: "Wrong input." });
  const machine = review({ task_id: "t3", decision: "rejected", note: "x", reviewer_kind: "service_account" });
  const reviews = { t1: changes, t2: rejected, t3: machine };
  assert.deepEqual(findingFlag(["t1"], reviews), { tone: "attention", label: "A person asked for changes to an analysis this finding reads" });
  assert.deepEqual(findingFlag(["t1", "t2"], reviews), { tone: "failed", label: "A person rejected an analysis this finding reads" });
  assert.equal(findingFlag(["t3"], reviews), null);
  assert.equal(findingFlag(["t4"], reviews), null);
  // An answer table carries the same marks, worded for a table.
  assert.deepEqual(findingFlag(["t1"], reviews, "table"), { tone: "attention", label: "A person asked for changes to an analysis this table reads" });
  assert.deepEqual(findingFlag(["t1", "t2"], reviews, "table"), { tone: "failed", label: "A person rejected an analysis this table reads" });
});
