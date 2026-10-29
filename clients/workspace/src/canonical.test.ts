import assert from "node:assert/strict";
import test from "node:test";
import { canonicalJson, sha256Hex } from "./canonical.ts";

// The broker digests a column summary as RFC 8785 canonical JSON; Python writes the same bytes with
// json.dumps(columns, sort_keys=True, separators=(",", ":"), ensure_ascii=False). The expected text and digest below
// were made that way, so the workspace's check agrees with the broker's routine, not only with itself.
test("a summary digests to the bytes the broker's routine writes", async () => {
  const columns = [
    { name: "block", type: "integer", type_source: "inferred", count: 3, missing: 0, min: "1", max: "4", distinct: 3 },
    { name: "note", type: "string", type_source: "inferred", count: 2, missing: 1, min: "Ä", max: "zé", distinct: 2 },
  ];
  const text = canonicalJson(columns);
  assert.equal(text, '[{"count":3,"distinct":3,"max":"4","min":"1","missing":0,"name":"block","type":"integer","type_source":"inferred"},'
    + '{"count":2,"distinct":2,"max":"zé","min":"Ä","missing":1,"name":"note","type":"string","type_source":"inferred"}]');
  assert.equal(await sha256Hex(text), "ef62512a8da721a22c3820933952c08cb583fd9377d41288254b0ab8b0bf0720");
  assert.equal(canonicalJson({ b: null, a: [1, "x"] }), '{"a":[1,"x"],"b":null}');
});
