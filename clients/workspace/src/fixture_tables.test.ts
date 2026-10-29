import assert from "node:assert/strict";
import test from "node:test";
import { pageOf, readTable, summaryOf, type StoredTable } from "../fixtures/tables.ts";
import { canonicalJson, sha256Hex } from "./canonical.ts";

// The preview's table pages follow the broker's rules, so the workspace is tested
// against the behaviour it will meet: stored strings, types from every value, missing values last, ties in file order.
const table = (text: string) => readTable(Buffer.from(text), ",") as StoredTable;
const query = (q: Record<string, string>) => new URLSearchParams(q);

test("each row keeps its index and the line it starts on, through quoted line breaks, blank lines and lone CRs", () => {
  const read = table('id,note\n1,"two\nlines"\n\n2,plain\n');
  assert.deepEqual(read.rows.map((r) => [r.index, r.line, r.values]), [[0, 2, ["1", "two\nlines"]], [1, 5, ["2", "plain"]]]);
  assert.deepEqual(table("id,v\r1,a\r2,b\r").rows.map((r) => r.line), [2, 3]);
});

test("sorting is by type, missing values last in both orders, ties in file order", () => {
  const read = table("n,s\n10,b\n9,NA\n,a\n10,a\n2,B\n");
  const order = (q: Record<string, string>) => (pageOf(read, query(q)) as { rows: { index: number }[] }).rows.map((r) => r.index);
  assert.deepEqual(order({ sort: "n", order: "asc" }), [4, 1, 0, 3, 2]);
  assert.deepEqual(order({ sort: "n", order: "desc" }), [0, 3, 1, 4, 2]);
  // Strings sort by byte order ("B" before "a"); NA is missing.
  assert.deepEqual(order({ sort: "s" }), [4, 2, 3, 0, 1]);
  assert.deepEqual((pageOf(read, query({})) as { types: string[] }).types, ["integer", "string"]);
});

test("pages refuse what the broker refuses", () => {
  const read = table("a\n1\n");
  const code = (q: Record<string, string>) => (pageOf(read, query(q)) as { code?: string }).code;
  assert.equal(code({ sort: "b" }), "sort_column_unknown");
  assert.equal(code({ order: "asc" }), "page_order_invalid");
  assert.equal(code({ sort: "a", order: "up" }), "page_order_invalid");
  assert.equal(code({ limit: "0" }), "page_limit_invalid");
  assert.equal(code({ limit: "1001" }), "page_limit_invalid");
  assert.equal(code({ offset: "-1" }), "page_offset_invalid");
  assert.equal((readTable(Buffer.from("a,b\n1\n"), ",") as { code: string }).code, "table_unreadable");
  assert.equal((readTable(Buffer.from([0x61, 0x0a, 0xff, 0x0a]), ",") as { code: string }).code, "table_unreadable");
});

test("a summary's receipt digests its own columns from the stored bytes", async () => {
  const read = table("x,y\n1,a\nNA,b\n3,b\n");
  const summary = summaryOf(read) as { columns: { name: string; type: string; count: number; missing: number; min: string; max: string; distinct: number }[]; receipt: { input_sha256: string; output_sha256: string } };
  assert.deepEqual(summary.columns.map((c) => [c.name, c.type, c.count, c.missing, c.min, c.max, c.distinct]), [["x", "integer", 2, 1, "1", "3", 2], ["y", "string", 3, 0, "a", "b", 2]]);
  assert.equal(summary.receipt.input_sha256, read.sha256);
  assert.equal(summary.receipt.output_sha256, await sha256Hex(canonicalJson(summary.columns)));
});
