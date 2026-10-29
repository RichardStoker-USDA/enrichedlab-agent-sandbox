import assert from "node:assert/strict";
import test from "node:test";
import type { LabTool, LabTools } from "./contracts.ts";
import { grantWords, kindName, parameterWords, sectionKey, shortDigest, toolSections, toolsFailure } from "./tools.ts";
import { BrokerError } from "./api.ts";

const tool = (profile: string, via_groups: string[]): LabTool =>
  ({ profile, name: profile, description: "", route: "local-agent", kind: "sandbox_method", via_groups, examples: [] });
const view: LabTools = {
  lab: "pilot-lab",
  tool_groups: [
    { key: "core", version: 2, title: "Core tools", description: "", granted_at: "2026-10-25T00:00:00Z", tools: ["a", "b"] },
    { key: "extra", version: 1, title: "Extra tools", description: "", granted_at: "2026-10-25T00:00:00Z", tools: ["b"] },
  ],
  tools: [tool("a", ["core"]), tool("b", ["core", "extra"]), tool("c", [])],
};

test("tools sit under each tool group the lab holds, then the directly granted ones", () => {
  const sections = toolSections(view);
  assert.deepEqual(sections.map((s) => [s.group?.key ?? null, s.tools.map((t) => t.profile)]),
    [["core", ["a", "b"]], ["extra", ["b"]], [null, ["c"]]]);
  assert.deepEqual(toolSections({ ...view, tools: [tool("a", ["core"])] }).map((s) => s.group?.key ?? null), ["core", "extra"]);
});

test("a tool says how it reached the lab", () => {
  assert.equal(grantWords(view.tools[2], view.tool_groups), "Granted to the lab directly.");
  assert.equal(grantWords(view.tools[0], view.tool_groups), "Granted through Core tools (version 2).");
  assert.equal(grantWords(view.tools[1], view.tool_groups), "Granted through Core tools (version 2) and Extra tools (version 1).");
  assert.equal(grantWords(tool("d", ["gone"]), view.tool_groups), "Granted through gone.");
});

test("parameters read as what they accept", () => {
  assert.equal(parameterWords({ name: "n", type: "enum", required: true, choices: ["fibre", "protein"] }), "one of fibre, protein, required");
  assert.equal(parameterWords({ name: "n", type: "integer", minimum: 1, maximum: 500 }), "a whole number from 1 to 500, optional");
  assert.equal(parameterWords({ name: "n", type: "number", minimum: 0, maximum: 1, required: false }), "a number from 0 to 1, optional");
  assert.equal(parameterWords({ name: "n", type: "text", max_length: 80, required: true }), "text up to 80 characters, required");
  assert.equal(parameterWords({ name: "n" }), "optional");
});

test("kinds, digests and refusals read in plain words", () => {
  assert.equal(kindName("configured_method"), "Configured analysis");
  assert.equal(kindName("query_method"), "Query");
  assert.equal(shortDigest(`sha256:${"ab".repeat(32)}`), "abababababab");
  assert.equal(toolsFailure(new BrokerError("record_unavailable", 404, "lab_not_found")), "This lab's tools are shown only to its members.");
  assert.match(toolsFailure(new Error("x")), /could not be loaded/);
});

test("a tool group keyed direct and the directly granted section never share a key", () => {
  const collide: LabTools = {
    lab: "pilot-lab",
    tool_groups: [{ key: "direct", version: 1, title: "Direct lookups", description: "", granted_at: "2026-10-25T00:00:00Z", tools: ["a"] }],
    tools: [tool("a", ["direct"]), tool("c", [])],
  };
  const keys = toolSections(collide).map(sectionKey);
  assert.deepEqual(keys, ["group-direct", "direct"]);
  assert.equal(new Set(keys).size, keys.length);
});
