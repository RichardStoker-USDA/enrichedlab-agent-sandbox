import assert from "node:assert/strict";
import { createHash } from "node:crypto";
import test from "node:test";
import { fixtureMaterials, fixturePackage } from "../fixtures/materials.ts";
import { parseTrajectory, relatedSteps, stepArtifacts } from "./trajectory.ts";

const sha = (bytes: Buffer) => createHash("sha256").update(bytes).digest("hex");

test("The synthetic R package lists a matching checksum for every file", () => {
  const pkg = fixturePackage(0);
  const sums = pkg.files.find((f) => f.path === "SHA256SUMS")!.bytes.toString().trim().split("\n");
  assert.equal(sums.length, pkg.files.length - 1);
  for (const line of sums) {
    const [hash, path] = line.split("  ");
    assert.equal(sha(pkg.files.find((f) => f.path === path)!.bytes), hash, path);
  }
  for (const path of ["code/analysis.R", "input/measurements.csv", "results/response_by_diet.pdf", "results/feature_volcano.pdf", "results/model_checks.pdf"])
    assert.ok(pkg.files.some((f) => f.path === path), path);
});

test("The retained summary table follows from the retained input", () => {
  const pkg = fixturePackage(0);
  const text = (path: string) => pkg.files.find((f) => f.path === path)!.bytes.toString();
  const unquote = (cell: string) => cell.replace(/^"|"$/g, "");
  const groups = new Map<string, number[]>();
  for (const line of text("input/measurements.csv").trim().split("\n").slice(1)) {
    const [, condition, response] = line.split(",").map(unquote);
    groups.set(condition, [...(groups.get(condition) ?? []), Number(response)]);
  }
  const rows = text("results/condition_summary.csv").trim().split("\n").slice(1).map((line) => line.split(",").map(unquote));
  assert.equal(rows.length, groups.size);
  for (const [condition, n, mean] of rows) {
    const values = groups.get(condition)!;
    assert.equal(Number(n), values.length, condition);
    assert.ok(Math.abs(Number(mean) - values.reduce((a, b) => a + b, 0) / values.length) < 5e-4, condition);
  }
});

test("Recorded steps link the retained files they were recorded with", () => {
  const files = fixtureMaterials("10000000-0000-4000-8000-000000000001", () => "2026-01-01T00:00:00Z");
  const trajectory = parseTrajectory(files.find(({ artifact }) => artifact.relative_path.endsWith("/events.jsonl"))!.bytes.toString());
  assert.equal(trajectory.steps.length, 240);
  assert.equal(stepArtifacts(trajectory.steps[2], "output", files.map((file) => file.artifact))[0].media_type, "image/svg+xml");
  assert.deepEqual(relatedSteps(trajectory.steps[2], "input", trajectory.steps, files.map((file) => file.artifact)).map((step) => step.id), [trajectory.steps[1].id]);
  assert.deepEqual(relatedSteps(trajectory.steps[2], "input", trajectory.steps, []), []);
});
