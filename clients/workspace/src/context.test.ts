import assert from "node:assert/strict";
import test from "node:test";
import { chooseLab, labOrder, operatorLab, projectForTask, sharing } from "./context.ts";
import { offeredProfiles, runProfiles, type Identity } from "./contracts.ts";
import type { Project } from "./projects.ts";

const project = (id: string, tasks: string[]) => ({
  project_id: id, group_key: "lab_a", owner_id: "o", title: id, visibility: "lab", revision: 1, created_at: "", updated_at: "",
  capabilities: [], checkpoint: null, task_count: tasks.length,
  tasks: tasks.map((task_id) => ({ task_id, action_key: "a", parent_task_id: null, profile: "p", status: "completed", attempt: 1, submitted_at: "", updated_at: "" })),
}) as Project;

test("the chosen lab stays, then the saved one, then the first", () => {
  assert.equal(chooseLab(["a", "b"], "b", "a"), "b");
  assert.equal(chooseLab(["a", "b"], "", "b"), "b");
  assert.equal(chooseLab(["a", "b"], "gone", "gone-too"), "a");
  assert.equal(chooseLab([], "a", "b"), "");
});

test("a first visit opens a research lab, never an operator lab, and research labs lead every list", () => {
  const keys = ["retained-byte-qualification", "fooddata-central", "plant-lab", "generated-r-qualification"];
  assert.equal(chooseLab(keys, "", ""), "fooddata-central");
  assert.equal(chooseLab(keys, "", "retained-byte-qualification"), "retained-byte-qualification");
  assert.equal(chooseLab(["retained-byte-qualification"], "", ""), "retained-byte-qualification");
  assert.equal(operatorLab("retained-byte-qualification"), true);
  assert.equal(operatorLab("qualification-lab"), false);
  assert.deepEqual(labOrder(keys.map((key) => ({ key }))).map((g) => g.key),
    ["fooddata-central", "plant-lab", "retained-byte-qualification", "generated-r-qualification"]);
});

test("a lab offers only its own methods when the broker names them, and every method when it names none", () => {
  const group = (key: string, profiles?: string[]) => ({ key, name: key, role: "lead", max_active_agents: 2,
    max_active_agents_per_principal: 2, max_queued_tasks: 8, retention_days: 365, ...(profiles ? { profiles } : {}) });
  const method = (key: string, offered = true) => ({ key, name: key, description: "", tool_bundle: "", max_run_seconds: 60, offered });
  const identity = { principal: { id: "p", email: "p@example.invalid", display_name: null },
    groups: [group("fooddata-central", ["reference-sql-fdc-2026-04", "figure-render-v1"]), group("retained-byte-qualification", ["retained-byte-copy"]), group("older")],
    profiles: [method("reference-sql-fdc-2026-04"), method("figure-render-v1", false), method("retained-byte-copy")] } as Identity;
  assert.deepEqual(offeredProfiles(identity, "fooddata-central").map((p) => p.key), ["reference-sql-fdc-2026-04"]);
  assert.deepEqual(offeredProfiles(identity, "retained-byte-qualification").map((p) => p.key), ["retained-byte-copy"]);
  assert.deepEqual(offeredProfiles(identity, "older").map((p) => p.key), ["reference-sql-fdc-2026-04", "retained-byte-copy"]);
  assert.deepEqual(offeredProfiles(identity).map((p) => p.key), ["reference-sql-fdc-2026-04", "retained-byte-copy"]);
  // A run may also list a method a person cannot run directly; one marked out of runs never shows.
  const withRuns = { ...identity, profiles: [...identity.profiles,
    { key: "analysis", name: "Reviewed R analyses", description: "", tool_bundle: "render", max_run_seconds: 300, offered: false, in_runs: true },
    { key: "figure", name: "Figure", description: "", tool_bundle: "render", max_run_seconds: 120, offered: false, in_runs: false }] };
  assert.deepEqual(runProfiles(withRuns).map((p) => p.key), ["reference-sql-fdc-2026-04", "retained-byte-copy", "analysis"]);
  assert.deepEqual(offeredProfiles(withRuns).map((p) => p.key), ["reference-sql-fdc-2026-04", "retained-byte-copy"]);
});

test("an analysis belongs to the project whose record lists it, and to no other", () => {
  const details = { p1: project("p1", ["t1"]), p2: project("p2", ["t2", "t3"]) };
  assert.equal(projectForTask(details, "t3")?.project_id, "p2");
  assert.equal(projectForTask(details, "t9"), null);
});

test("sharing is said in plain words", () => {
  assert.equal(sharing("lab", "Example Lab"), "Shared with Example Lab");
  assert.match(sharing("private", "Example Lab"), /^Private/);
});
