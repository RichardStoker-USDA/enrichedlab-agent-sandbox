import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";
import { seasonDifferenceCsv } from "../fixtures/long_run.ts";

// The preview's season figure is R's drawing of the long run's block-by-hour table. The table R read must be the bytes
// the preview serves, or the R figure would show other numbers than its values and 3D views.
test("the R season figure was drawn from the table the preview serves", () => {
  assert.ok(seasonDifferenceCsv().equals(readFileSync(new URL("../fixtures/r/season/input/season_difference.csv", import.meta.url))));
});
