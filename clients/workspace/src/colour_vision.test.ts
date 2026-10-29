import assert from "node:assert/strict";
import test from "node:test";
import { colorMatrixValues, VISIONS } from "./colour_vision.ts";

test("each simulation keeps white white and black black, and keeps alpha", () => {
  for (const { id, matrix } of VISIONS) {
    if (!matrix) continue;
    for (let r = 0; r < 3; r++) assert.ok(Math.abs(matrix[r * 3] + matrix[r * 3 + 1] + matrix[r * 3 + 2] - 1) < 1e-5, `${id} row ${r} sums to 1`);
    const values = colorMatrixValues(matrix).split(" ").map(Number);
    assert.equal(values.length, 20);
    assert.deepEqual(values.slice(15), [0, 0, 0, 1, 0]);
  }
  assert.equal(VISIONS[0].matrix, null);
});
