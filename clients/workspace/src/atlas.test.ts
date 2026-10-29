import assert from "node:assert/strict";
import test from "node:test";
import { atlasPage, atlasOverview, initialAtlasFilter } from "./atlas.ts";
import { parseTrajectory } from "./trajectory.ts";
import { syntheticAtlasExport } from "../fixtures/atlas.ts";

test("research atlas keeps a late synthetic step reachable across actor groups", () => {
  const trajectory = parseTrajectory(syntheticAtlasExport(240));
  assert.equal(trajectory.steps.length, 240);
  assert.equal(atlasOverview(trajectory).capturedEvents, 480);
  const finalPage = atlasPage(trajectory.steps, initialAtlasFilter(), 5);
  assert.equal(finalPage.page, 5);
  assert.equal(finalPage.steps.at(-1)?.sequence, 479);
  assert.ok(finalPage.groups.length > 1);
  const filtered = atlasPage(trajectory.steps, { ...initialAtlasFilter(), actor: "synthetic-review", segment: "5" }, 1);
  assert.ok(filtered.total > 0);
  assert.ok(filtered.steps.every((step) => step.actor === "synthetic-review" && step.segment === 5));
});
