import assert from "node:assert/strict";
import test from "node:test";
import { draftForPrincipal } from "./draft_scope.ts";

const current = { draft: { group: "group", profile: "profile", objective: "private draft" }, requestKey: "retained-key", requestInput: "payload" };

test("draft state is never carried across principals", () => {
  assert.deepEqual(draftForPrincipal("researcher-a", "researcher-b", current), { draft: { group: "", profile: "", objective: "" }, requestKey: null, requestInput: "" });
  assert.deepEqual(draftForPrincipal("researcher-a", "researcher-a", current), current);
});
