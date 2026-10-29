import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";

const component = readFileSync(new URL("./StepLineage.svelte", import.meta.url), "utf8");
const record = readFileSync(new URL("./LineageRecord.svelte", import.meta.url), "utf8");

test("the step lineage uses closed native disclosures recursively, with a visible path at every opened level", () => {
  assert.match(component, /\{#snippet branch\(item: StepLineageItem\)\}/);
  assert.match(component, /\{@render branch\(child\)\}/);
  assert.match(component, /<details class="input"/);
  assert.match(component, /<summary>/);
  assert.doesNotMatch(component, /<details class="input"[^>]*\sopen(?:\s|=|>)/);
  assert.match(component, /aria-label=\{`Recorded path:/);
  assert.match(component, /item\.path\.join\(" → "\)/);
  assert.doesNotMatch(component, /onclick=/);
});

test("the component keeps machine facts in Technical details and removes disclosure motion when requested", () => {
  assert.match(component, /<details class="technical">/);
  assert.match(component, /@media \(prefers-reduced-motion: reduce\)/);
  assert.match(component, /\.chevron \{ transition: none; \}/);
  assert.match(component, /\.branch-body \{ animation: none; \}/);
  assert.doesNotMatch(component, /badge|gradient|glow|#[0-9a-fA-F]{3,8}(?=[;\s,)])/);
});

test("aggregate omissions lead the drill-down and root attempt gaps keep codes in Technical details", () => {
  assert.ok(component.indexOf("view.incomplete") < component.indexOf("Open an input"));
  assert.match(component, /view\.rootGaps/);
  assert.match(component, /view\.rootTechnical/);
  assert.doesNotMatch(component, /view\.(withheld|truncated)/);
});

test("LineageRecord places the version 3 drill-down inside its Data row", () => {
  assert.match(record, /row\.key === "data" && lineage\.schema === "enrichedlab\.task-lineage\.v3"/);
  assert.match(record, /<StepLineageRecord \{lineage\} \/>/);
});
