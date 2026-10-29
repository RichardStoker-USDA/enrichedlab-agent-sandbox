import assert from "node:assert/strict";
import test from "node:test";
import { chosenSentence, methodsOf, startsSentence } from "./lab_starts.ts";

const start = (name: string) => ({ key: name, name, description: "", input_refs: [], tool_refs: [], allowed_profiles: [] });

test("the ask field says what the agent can work from, in the lab's own names", () => {
  assert.equal(startsSentence(null, "Open Methods"), "Reading what Open Methods can work from…");
  assert.equal(startsSentence([], "Open Methods"), "No data and method set is registered for Open Methods yet, so the agent can't start research here.");
  assert.equal(startsSentence([start("Simulated diet study")], "Open Methods"), "The agent can work from Simulated diet study.");
  assert.equal(startsSentence([start("A"), start("B")], "Open Methods"), "The agent can work from 2 sets in Open Methods; you choose one next.");
  assert.equal(startsSentence(null, "Open Methods", true), "What Open Methods can work from could not be read. Continue to try again.");
});

test("with a broker that combines sets the ask field offers several, and the chosen sets are summed up plainly", () => {
  assert.equal(startsSentence([start("A"), start("B"), start("C")], "Open Methods", false, true),
    "The agent can work from 3 sets in Open Methods; you choose one, several or all of them next.");
  const a = { ...start("Diet study"), input_refs: ["source://a"], tool_refs: ["tool://x", "tool://y"], allowed_profiles: ["evidence_review", "generated_r"] };
  const b = { ...start("Intake records"), input_refs: ["source://b"], tool_refs: ["tool://x"], allowed_profiles: ["evidence_review", "template_analysis"] };
  assert.equal(chosenSentence([], 2), "Choose at least one set.");
  assert.equal(chosenSentence([a], 2), "The agent works from Diet study.");
  assert.equal(chosenSentence([a], 1), "");
  assert.equal(chosenSentence([a, b], 2), "The agent works across both sets: 2 data sources and 2 methods.");
  assert.equal(chosenSentence([a, b], 3), "The agent works across 2 of the 3 sets: 2 data sources and 2 methods.");
  assert.deepEqual(methodsOf([a, b]), ["evidence_review", "generated_r", "template_analysis"]);
});
