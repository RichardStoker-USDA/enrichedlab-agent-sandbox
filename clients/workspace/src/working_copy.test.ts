import assert from "node:assert/strict";
import { test } from "node:test";
import { workingCopyLine, type WorkingCopy } from "./working_copy.ts";

const now = new Date("2026-09-26T01:00:00Z");
const copy = (over: Partial<WorkingCopy> = {}): WorkingCopy =>
  ({ task_id: "10000000-0000-4000-8000-000000000001", kept_until: "2026-10-25T17:26:12Z", cleared_at: null, volumes: 2, ...over });

test("a kept working copy counts whole days to its date", () => {
  assert.equal(workingCopyLine(copy(), now), "Working copy kept until October 25 (in 30 days). Results and the package stay.");
  assert.equal(workingCopyLine(copy({ kept_until: "2026-09-26T12:00:00Z" }), now),
    "Working copy kept until September 26 (in 1 day). Results and the package stay.");
});

test("a date in another year names the year", () => {
  assert.equal(workingCopyLine(copy({ kept_until: "2027-01-05T00:00:00Z" }), now),
    "Working copy kept until January 5, 2027 (in 101 days). Results and the package stay.");
});

test("a passed date says the sweep clears it next, and a cleared copy says when it went", () => {
  assert.equal(workingCopyLine(copy({ kept_until: "2026-09-25T00:00:00Z" }), now),
    "Working copy kept until September 25; the next daily sweep clears it. Results and the package stay.");
  assert.equal(workingCopyLine(copy({ cleared_at: "2026-10-26T03:00:05Z" }), now),
    "Working copy cleared October 26. Results and the package stay.");
});

test("no record or an unreadable date shows nothing", () => {
  assert.equal(workingCopyLine(undefined, now), null);
  assert.equal(workingCopyLine(copy({ kept_until: "soon" }), now), null);
});
