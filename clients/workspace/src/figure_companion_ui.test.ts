import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";

test("the publication figure stays before the companion on narrow screens", () => {
  const source = readFileSync(new URL("./FigureView.svelte", import.meta.url), "utf8");
  assert.doesNotMatch(source, /phoneView|phone-switch|phone-off/);
  assert.ok(source.indexOf('<div class="figure-panel">') < source.indexOf('<div class="companion-panel">'));
  assert.match(source, /@media \(min-width: 900px\)[\s\S]*\.view-pair\.open \{ grid-template-columns:/);
});

test("large companion collections use virtual windows rather than full each blocks", () => {
  const source = readFileSync(new URL("./FigureCompanion.svelte", import.meta.url), "utf8");
  assert.match(source, /shownFacets/);
  assert.match(source, /shownGroups/);
  assert.match(source, /shownSelectedRows/);
  assert.doesNotMatch(source, /#each table\.encoding\.(?:facet_order|group_order)/);
  assert.doesNotMatch(source, /#each selectedRows as/);
  assert.match(source, /groupVisible\(point\.group, hiddenGroups\)/);
  assert.match(source, /storedNumericTicks\(inFacet/);
  assert.doesNotMatch(source, /prettyTicks/);
});

test("the 3D renderer keeps one base object per mark mode and raycasts the drawn geometry", () => {
  const source = readFileSync(new URL("./companion3d.ts", import.meta.url), "utf8");
  assert.equal(source.match(/new THREE\.InstancedMesh\(/g)?.length, 1);
  assert.equal(source.match(/new THREE\.Points\(/g)?.length, 1);
  assert.doesNotMatch(source, /ScreenGridIndex|rebuildHitIndex|groupObjects|byGroup/);
  assert.match(source, /new THREE\.Raycaster\(\)/);
  assert.match(source, /raycaster\.intersectObject\(columnMesh, false\)/);
  assert.match(source, /raycaster\.intersectObject\(pointObject, false\)/);
  assert.match(source, /raycaster\.params\.Points\.threshold = pointHitRadius \* worldPerPixel/);
  assert.match(source, /Math\.hypot\(px - x, py - y\) <= pointHitRadius/);
  assert.match(source, /columnScale\.set\(0, 0, 0\)/);
  assert.match(source, /pointColours\[index \* 4 \+ 3\] = visibleGroups\.has/);
});
