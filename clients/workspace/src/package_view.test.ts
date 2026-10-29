import assert from "node:assert/strict";
import { test } from "node:test";
import type { PackageListing } from "./contracts.ts";
import { organize, packageHolds, queryOnly } from "./package_view.ts";

const listing = (paths: [string, string][]): PackageListing => ({
  format: "tar+gzip", checksum_file: "SHA256SUMS", missing_from_archive: [], skipped: [], expanded_bytes: 0,
  members: paths.map(([path, kind]) => ({ path, kind, size_bytes: 1, sha256: "a".repeat(64), media_type: "text/plain",
    manifest: path === "SHA256SUMS" ? "not_applicable" : "matched" })),
} as unknown as PackageListing);

test("a query's package names its query and no input copy or logs", () => {
  const view = organize(listing([["README.md", "text"], ["method/query.sql", "code"], ["receipt.json", "record"],
    ["results/query_result.csv", "table"], ["run_parameters.json", "record"], ["SHA256SUMS", "checksum"]]));
  assert.equal(packageHolds(view), "the query, the parameters, the results and the checksums");
  assert.equal(queryOnly(view), true);
});

test("an R analysis's package names its code, input copy and logs", () => {
  const view = organize(listing([["code/block.R", "code"], ["input/reference/inputs/observations.csv", "table"],
    ["run_parameters.json", "record"], ["results/summary.csv", "table"], ["logs/run.log", "log"], ["SHA256SUMS", "checksum"]]));
  assert.equal(packageHolds(view), "the code, the input copy, the parameters, the results, the logs and the checksums");
  assert.equal(packageHolds(null), "its code, inputs, parameters, results and checksums");
  assert.equal(queryOnly(view), false);
});
