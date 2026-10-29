import assert from "node:assert/strict";
import test from "node:test";
import { questionText, titleFrom } from "./ask.ts";

test("a title is the question's first sentence without its end mark", () => {
  assert.equal(titleFrom("Does vitamin C change tooth growth in guinea pigs? Use the reviewed method."), "Does vitamin C change tooth growth in guinea pigs");
  assert.equal(titleFrom("  plasma   response by diet.  "), "Plasma response by diet");
});

test("a long opening clause stands alone before its comma", () => {
  assert.equal(titleFrom("Which test diet raised plasma response the most, and by how much?"), "Which test diet raised plasma response the most");
  assert.equal(titleFrom("Diet A, B or C: which one?"), "Diet A, B or C: which one");
});

test("a long title is cut at a word boundary without a dangling small word", () => {
  const title = titleFrom("How does the abundance of the fecal microbiome in adults change after eight weeks of the high fibre test diet compared with the reference diet");
  assert.ok(title.length <= 72, title);
  assert.equal(title, "How does the abundance of the fecal microbiome in adults change after");
  assert.ok(!/ (of|the|and|in|to)$/.test(title));
});

test("an empty question gives an empty title, and the question keeps its own wording", () => {
  assert.equal(titleFrom("   "), "");
  assert.equal(questionText("  Keep   my spacing?  "), "Keep   my spacing?");
});
