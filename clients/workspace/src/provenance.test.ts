import assert from "node:assert/strict";
import test from "node:test";
import type { RetainedPackage } from "./evidence.ts";
import { figureSource, valueSource, type Made } from "./provenance.ts";

const task = "10000000-0000-4000-8000-000000000001", artifact = "20000000-0000-4000-8000-000000000001";
const ref = `artifact://tasks/${task}/${artifact}`;
const made = (t: string): Made => ({ task: t, step: 1, title: "Summarize each diet", method: "Reviewed summary method" });
const member = (path: string, manifest: string, media_type = "text/csv") => ({ path, size_bytes: 10, sha256: "a".repeat(64), media_type, kind: "result", manifest });
const pkg = (members: ReturnType<typeof member>[]) => [{ artifact: { id: artifact }, contents: { listing: { members } } }] as unknown as RetainedPackage[];
const binding = { ref, table: "results/group_summary.csv", row: 2, column: "mean" };

test("a value names its cell, the analysis that wrote it and its file's checksum state", () => {
  const source = valueSource(binding, "12.3791666666667", {}, pkg([member("results/group_summary.csv", "matched")]), made);
  assert.deepEqual(source && { shown: source.shown, exact: source.exact, place: source.place, file: source.file, check: source.check.state, step: source.made.step },
    { shown: "12.38", exact: "12.3791666666667", place: "Row 2, column “mean”", file: "results/group_summary.csv", check: "matched", step: 1 });
});

test("a value whose package listing was not read says what its check rests on", () => {
  const digests = { [ref]: { artifact_id: artifact, artifact_sha256: "b", status: "summarized", digest_sha256: "c", recorded_at: null,
    digest: { tables: [{ path: "results/group_summary.csv", rows: 3, columns: ["mean"] }] } } };
  const source = valueSource(binding, "12.3791666666667", digests, undefined, made);
  assert.equal(source?.check.state, "unknown");
  assert.match(source?.check.words ?? "", /stored digest/);
  assert.equal(valueSource({ ...binding, ref: "not a reference" }, "1", {}, undefined, made), null);
});

test("a mismatched file is never called intact", () => {
  assert.equal(valueSource(binding, "1", {}, pkg([member("results/group_summary.csv", "mismatched")]), made)?.check.state, "mismatched");
  const figure = { title: "Response by diet", display: member("results/response.svg", "matched", "image/svg+xml"), original: member("results/response.pdf", "mismatched", "application/pdf") };
  const source = figureSource(task, figure as never, made);
  assert.deepEqual([source.file, source.place, source.check.state], ["results/response.pdf", "SVG and PDF", "mismatched"]);
});
