import assert from "node:assert/strict";
import { test } from "node:test";
import { fileRole, kindOf } from "./file_role.ts";

test("known files say what they are for", () => {
  assert.equal(fileRole("README.md"), "How to rerun this analysis on your own computer");
  assert.equal(fileRole("run.json"), "The run's record: its method, settings and seed");
  assert.equal(fileRole("code/template.R"), "The reviewed recipe the code ran inside");
  assert.equal(fileRole("code/block.R"), "The block of code that ran inside the recipe");
  assert.equal(fileRole("results/stdout.log"), "The run's log, for troubleshooting");
  assert.equal(fileRole("rerun/verify.py"), "Checks every file and can rerun each analysis");
  assert.equal(fileRole("SHA256SUMS"), "Checksums that show these files are unchanged");
  assert.equal(fileRole("exit_code.txt"), "How the run ended: 0 means without an error");
  assert.equal(fileRole("method/query.sql"), "The database query that ran");
  assert.equal(fileRole("receipt.json"), "What the query read and returned: snapshot, columns and checksums");
});

test("folders and reader kinds name the rest", () => {
  assert.equal(fileRole("results/condition_summary.csv", "table"), "A results table");
  assert.equal(fileRole("results/response_by_diet.pdf", "figure"), "A figure from the results");
  assert.equal(fileRole("input/reference/inputs/observations.csv", "table"), "A copy of the input data, as the analysis read it");
  assert.equal(fileRole("input/contract.dcf"), "Which columns the code could read");
  assert.equal(fileRole("logs/child_input.txt", "text"), "A log from the run, for troubleshooting");
  assert.equal(fileRole("notes/extra.bin", "data"), "A data file");
  assert.equal(fileRole("other/figure.svg"), "A figure");
});

test("the reader kind follows the broker's rules", () => {
  assert.equal(kindOf("results/a.CSV"), "table");
  assert.equal(kindOf("payload.sha256"), "checksum");
  assert.equal(kindOf("code/x.R"), "code");
  assert.equal(kindOf("run.json"), "record");
  assert.equal(kindOf("README"), "data");
  assert.equal(fileRole("tasks/x/payload.sha256"), "The checksum that shows the package is unchanged");
});
