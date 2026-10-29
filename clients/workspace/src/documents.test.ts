import assert from "node:assert/strict";
import test from "node:test";
import type { RunDocument } from "./contracts.ts";
import { anyWriting, canWriteDown, documentName, documentRefusal, documentState, kindLabel } from "./documents.ts";

const doc = (over: Partial<RunDocument> = {}): RunDocument => ({
  task_id: "01920000-0000-7000-8000-000000000001", kind: "docx", turn: 3, status: "queued",
  created_at: "2026-10-17T10:00:00Z", finished_at: null, receipt: null, ...over,
});
const file = { kind: "docx" as const, path: "document.docx", media_type: "x", sha256: "a".repeat(64), size_bytes: 10 };
const done = { status: "completed" as const, error_code: null, message: null, file, package_sha256: "b".repeat(64) };

test("an answer can be written down once the run stopped on it", () => {
  assert.equal(canWriteDown("review_required"), true);
  assert.equal(canWriteDown("paused"), true);
  assert.equal(canWriteDown("running"), false);
  assert.equal(canWriteDown(undefined), false);
});

test("a document stands as writing, ready or stopped from its receipt", () => {
  assert.deepEqual(documentState(doc()), { state: "writing" });
  assert.deepEqual(documentState(doc({ status: "completed", receipt: done })), { state: "ready", file });
  const words = "The answer holds more than this kind of document can; try a workbook.";
  assert.deepEqual(documentState(doc({ status: "failed", receipt: { status: "refused", error_code: "render_document_too_large",
    message: words, file: null, package_sha256: null } })), { state: "stopped", words });
  assert.deepEqual(documentState(doc({ status: "cancelled" })), { state: "stopped", words: "It could not be written." });
  assert.equal(anyWriting([doc(), doc({ status: "completed", receipt: done })]), true);
  assert.equal(anyWriting([doc({ status: "completed", receipt: done })]), false);
});

test("a document is named from the project title, turn and kind", () => {
  assert.equal(documentName("Vitamin D & bone density (2026)", doc()), "vitamin-d-bone-density-2026-turn-3.docx");
  assert.equal(documentName("   ", doc({ kind: "xlsx", turn: 12 })), "answer-turn-12.xlsx");
  assert.equal(documentName("Café über", doc()), "cafe-uber-turn-3.docx");
  assert.equal(kindLabel("pptx"), "PowerPoint slides");
});

test("a refused request reads in plain words only when the person can act on it", () => {
  assert.equal(documentRefusal("document_run_not_ended"), "The agent's newest step did not end on this answer, so it cannot be written down.");
  assert.equal(documentRefusal("document_method_unavailable"), "This lab has no document method yet.");
  assert.equal(documentRefusal("project_not_found"), null);
});
