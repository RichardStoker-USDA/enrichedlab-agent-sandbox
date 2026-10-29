import assert from "node:assert/strict";
import { test } from "node:test";
import type { TaskQuery } from "./contracts.ts";
import { capAdded, resultSentence, stopWords, versionWords } from "./query.ts";

const base: TaskQuery = {
  task_id: "t", profile: "reference-sql", source: "example-reference", title: "Example reference data",
  description: "Synthetic foods and nutrients.", release: "Example release", snapshot: { manifest_sha256: "a".repeat(64) },
  proposed_sql: "SELECT name FROM food ORDER BY name;", admitted_sql: "SELECT name FROM food ORDER BY name\nLIMIT 500",
  sql_sha256: `sha256:${"b".repeat(64)}`, guard: "guarded-sql/v1", relations: ["food"], row_limit: 500, run_id: null, turn: null,
  created_at: "2026-09-26T00:00:00Z", validation: "unvalidated", receipts: [],
};

test("says what came back from the newest attempt", () => {
  assert.equal(resultSentence(base), "It has not run yet.");
  const done = { ...base, receipts: [{ attempt: 1, status: "completed" as const, error_code: null, row_count: 12,
    columns: [{ name: "name", type: "text" }], package_sha256: "c".repeat(64), server_version: "18", started_at: "", finished_at: "" }] };
  assert.equal(resultSentence(done), "It returned 12 rows with one column.");
  const failed = { ...base, receipts: [{ ...done.receipts[0], status: "failed" as const, error_code: "query_column_unknown",
    row_count: null, columns: null, package_sha256: null }] };
  assert.equal(resultSentence(failed), "It did not finish: the database has no column the query named.");
});

test("names refusals and unknown codes plainly", () => {
  assert.equal(stopWords("sql_refused_relation"), "The check before it ran refused the query.");
  assert.equal(stopWords("something_new"), "It did not finish.");
  assert.equal(stopWords(null), "It did not finish.");
});

test("tells when the row cap was added to the agent's text", () => {
  assert.equal(capAdded(base), true);
  assert.equal(capAdded({ ...base, proposed_sql: "SELECT name FROM food ORDER BY name LIMIT 5", admitted_sql: "SELECT name FROM food ORDER BY name LIMIT 5" }), false);
  // The guard trims whitespace and every trailing semicolon before it admits a query; that alone adds nothing.
  assert.equal(capAdded({ ...base, proposed_sql: " SELECT name FROM food ORDER BY name LIMIT 5;; ", admitted_sql: "SELECT name FROM food ORDER BY name LIMIT 5" }), false);
});

test("a live lab database's version reads as its own fields, bounded", () => {
  assert.equal(versionWords({ schema_version: "0021", newest_import_id: 7, newest_import_at: "2026-10-06T13:31:28+00:00" }),
    "schema version 0021, newest import id 7, newest import at 2026-10-06T13:31:28+00:00");
  assert.equal(versionWords(null), "");
  assert.equal(versionWords({ note: "x".repeat(80) }), `note ${"x".repeat(59)}…`);
  assert.equal(versionWords(Object.fromEntries(Array.from({ length: 9 }, (_, i) => [`k${i}`, i]))).split(", ").length, 6);
  assert.equal(stopWords("source_version_unreadable"), "It did not finish: the lab database did not state one version for the query to read at.");
  assert.equal(stopWords("source_version_too_large"), "It did not finish: the lab database's statement of its version was larger than the platform keeps.");
});
