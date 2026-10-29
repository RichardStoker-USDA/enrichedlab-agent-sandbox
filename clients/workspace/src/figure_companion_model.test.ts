import assert from "node:assert/strict";
import test from "node:test";
import { groupVisible, ScreenGridIndex, virtualWindow } from "./figure_companion_model.ts";

test("virtual windows keep large controls and selected-row tables DOM-bounded", () => {
  const start = virtualWindow(50_000, 0, 180, 36, 3);
  assert.deepEqual(start, { first: 0, last: 8, offset: 0, totalHeight: 1_800_000 });
  const middle = virtualWindow(50_000, 900_000, 180, 36, 3);
  assert.ok(middle.last - middle.first <= 11);
  assert.ok(middle.first > 24_000 && middle.last < 26_000);
  const end = virtualWindow(50_000, 1_800_000, 180, 36, 3);
  assert.equal(end.last, 50_000);
  assert.ok(end.last - end.first <= 8);
  assert.deepEqual(virtualWindow(2, 1_000, 180, 36, 3), { first: 2, last: 2, offset: 72, totalHeight: 72 });
});

test("an empty string is a hideable group while null means no grouping channel", () => {
  const hidden = new Set([""]);
  assert.equal(groupVisible("", hidden), false);
  assert.equal(groupVisible("kept", hidden), true);
  assert.equal(groupVisible(null, hidden), true);
});

test("the screen grid checks a local bucket and keeps every mark available", () => {
  const index = new ScreenGridIndex(24);
  for (let row = 0; row < 50_000; row++) {
    const x = (row % 250) * 8, y = Math.floor(row / 250) * 8;
    index.addPoint(row, x, y, row / 50_000, 3);
  }
  assert.equal(index.hit(800, 800), 25_100);
  assert.ok(index.lastCandidateCount < 100, `looked at ${index.lastCandidateCount} candidates`);
  assert.deepEqual(index.rowsIn({ left: 0, top: 0, right: 9, bottom: 9 }), [0, 1, 250, 251]);

  index.clear();
  index.addRectangle(7, 12, 20, 8, 5, 16, 30, 0.2);
  assert.equal(index.hit(10, 24), 7);

  index.clear();
  index.addPolygon(8, 3, 3, [{ x: 0, y: 0 }, { x: 10, y: 0 }, { x: 0, y: 10 }], 0.1);
  assert.equal(index.hit(2, 2), 8);
  assert.equal(index.hit(9, 9), null, "the index rejects empty space inside a projected bounding box");
});
