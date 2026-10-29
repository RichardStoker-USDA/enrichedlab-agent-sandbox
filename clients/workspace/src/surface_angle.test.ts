import assert from "node:assert/strict";
import test from "node:test";
import { perspAngle } from "./views/surface/angle.ts";

// persp() puts the eye at the lowest y for theta 0 and at the highest x for theta 90; the scene keeps the table's y
// along -z, so an eye at +z is at the front.
test("the view's angle is the one persp() draws", () => {
  const at = (position: [number, number, number], height = 1) => perspAngle({ position, target: [0, 0, 0] }, height);
  assert.deepEqual(at([0, 0, 2]), { theta: 0, phi: 0, expand: 0.35 });
  assert.deepEqual(at([2, 0, 0]), { theta: 90, phi: 0, expand: 0.35 });
  assert.deepEqual(at([0, 0, -2]), { theta: 180, phi: 0, expand: 0.35 });
  assert.equal(at([0, 2, 2]).phi, 45);
  // The view's starting angle reads as a classic front-right persp() angle; a ×2 scale doubles expand.
  assert.deepEqual(at([1.05, 0.83, 1.25], 2), { theta: 40, phi: 27, expand: 0.7 });
  // An eye below the floor is drawn from the floor.
  assert.equal(at([0, -1, 1]).phi, 0);
});
