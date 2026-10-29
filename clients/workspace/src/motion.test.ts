import { test } from "node:test";
import assert from "node:assert/strict";
import { ease } from "./motion.ts";

test("the ease curve starts and ends in place, never runs backward, and leads fast as the design's curve does", () => {
  assert.equal(ease(0), 0);
  assert.equal(ease(1), 1);
  let last = 0;
  for (let i = 1; i <= 100; i++) { const y = ease(i / 100); assert.ok(y >= last - 1e-9, `monotonic at ${i}`); last = y; }
  assert.ok(ease(0.25) > 0.6, "most of the way there in the first quarter");
  assert.ok(Math.abs(ease(0.5) - 0.9) < 0.08);
});
