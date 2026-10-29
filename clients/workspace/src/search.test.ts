import assert from "node:assert/strict";
import test from "node:test";
import { rank, type SearchEntry } from "./search.ts";

const place = (label: string): SearchEntry => ({ id: label, kind: "place", label, words: label.toLowerCase() });
const project = (label: string, question: string, at: string): SearchEntry => ({ id: label, kind: "project", label, words: `${label} ${question}`.toLowerCase(), at });
const entries = [place("Your research"), place("Waiting for you"), place("Results"),
  project("Test diet response study", "How does plasma response differ between diets?", "2026-10-12T10:00:00Z"),
  project("Feature selection for the follow-up", "Which features predict response?", "2026-10-12T11:00:00Z"),
  project("Results archive review", "Old results", "2026-10-01T00:00:00Z")];

test("with nothing typed, places come first, then the newest projects", () => {
  assert.deepEqual(rank(entries, "").map((e) => e.id),
    ["Your research", "Waiting for you", "Results", "Feature selection for the follow-up", "Test diet response study", "Results archive review"]);
  assert.equal(rank(entries, "", 4).length, 4);
});

test("every typed word must match, a label that starts with the query ranks first, and a place leads a project", () => {
  assert.deepEqual(rank(entries, "results").map((e) => e.id), ["Results", "Results archive review"]);
  assert.deepEqual(rank(entries, "plasma diets").map((e) => e.id), ["Test diet response study"]);
  assert.deepEqual(rank(entries, "response").map((e) => e.id), ["Test diet response study", "Feature selection for the follow-up"]);
  assert.deepEqual(rank(entries, "nothing like this"), []);
});
