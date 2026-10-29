import assert from "node:assert/strict";
import test from "node:test";
import type { Artifact, Lineage, PackageListing, PackageMember } from "./contracts.ts";
import { organize } from "./package_view.ts";
import type { ProjectModel } from "./workbench.ts";
import { attentionFor, detailsFor, endedSince, factsOf, filesFor, flatten, ids, labTree, pathTo, pickerLabs, problemsFor, projectChildren, recentProjects, search, traceFor } from "./workbench.ts";

const P = "10000000-0000-4000-8000-000000000001";
const T1 = "30000000-0000-4000-8000-000000000001";
const T2 = "30000000-0000-4000-8000-000000000002";
const R = "40000000-0000-4000-8000-000000000001";
const A = "31000000-0000-4000-8000-000000000001";
const at = (m: number) => new Date(Date.UTC(2026, 8, 20, 10, m)).toISOString();
const member = (path: string, kind: PackageMember["kind"], media: string, manifest: PackageMember["manifest"] = "matched"): PackageMember =>
  ({ path, kind, media_type: media, size_bytes: 10, sha256: "a".repeat(64), manifest });
const listing: PackageListing = { format: "tar+gzip", checksum_file: "SHA256SUMS", missing_from_archive: [], skipped: [], expanded_bytes: 0, members: [
  member("SHA256SUMS", "checksum", "text/plain", "not_applicable"),
  member("results/growth_by_dose.svg", "figure", "image/svg+xml"),
  member("results/growth_by_dose.pdf", "figure", "application/pdf"),
  member("results/summary.csv", "table", "text/csv", "mismatched"),
  member("code/analysis.R", "code", "text/plain"),
  member("input/observations.csv", "table", "text/csv"),
] };
const artifact: Artifact = { id: A, role: "result", label: "Analysis and reproducibility package", relative_path: `tasks/${T1}/payload`,
  media_type: "application/gzip", size_bytes: 100, sha256: "b".repeat(64), created_at: at(5) };
const profiles = [{ key: "r-analysis", name: "Reviewed R analysis", description: "", tool_bundle: "", max_run_seconds: 60 }];

function model(): ProjectModel {
  return {
    project: {
      project_id: P, group_key: "nutrition", owner_id: "u1", title: "Vitamin C and growth", visibility: "lab", revision: 3,
      created_at: at(0), updated_at: at(9), capabilities: ["read"], task_count: 2,
      checkpoint: { revision: 3, author_id: "u1", created_at: at(9), state: {
        schema_version: 1, goal: "Does vitamin C dose change growth?", constraints: [], next_action: "",
        plan: [{ action_key: "a1", title: "Growth by dose", status: "uncertain", task_id: T1, evidence_refs: [], rationale: "Compare doses" }],
        lessons: [], workspace: { schema_version: 1, profile: "", image_digest: "", source_digest: "", input_refs: [], tool_refs: [], preparation_receipt: null } } },
      tasks: [
        { task_id: T1, action_key: "a1", parent_task_id: null, profile: "r-analysis", status: "completed", attempt: 1, submitted_at: at(2), updated_at: at(5) },
        { task_id: T2, action_key: "manual", parent_task_id: null, profile: "r-analysis", status: "failed", attempt: 1, submitted_at: at(6), updated_at: at(7) },
      ],
    },
    runs: [{
      run: { run_id: R, project_id: P, status: "review_required", turn: 2, max_turns: 8, max_parallel: 1, allowed_profiles: [], code: "turn_limit", updated_at: at(8), created_at: at(1) },
      turns: [{ turn: 1, context_revision: 1, checkpoint_revision: 2, created_at: at(2), record: {
        outcome: "submitted", scientific_validation: "unvalidated", proposal: { summary: "Run the dose comparison", actions: [] },
        method_review: { decision: "approved", reason: "" }, lead: { reported_model: "reasoner" }, submitted_tasks: [{ task_id: T1, action_key: "a1" }] } }],
      events: [
        { run_id: R, sequence: 1, code: "proposal_invalid", reason: "schema", generation: 1, turn: 1, failures: 1, status_after: "waiting", occurred_at: at(3) },
        { run_id: R, sequence: 2, code: "proposal_invalid", reason: "schema", generation: 1, turn: 1, failures: 2, status_after: "waiting", occurred_at: at(4) },
      ],
    }],
    checkpoints: [], audit: [], members: null,
    evidence: { [T1]: { taskId: T1, artifacts: [artifact], packages: [{ artifact, contents: { artifact_id: A, sha256: artifact.sha256, size_bytes: 100, listing }, view: organize(listing) }], unreadable: [] } },
  };
}

test("labs group the reader's projects under their research group names", () => {
  const tree = labTree([
    { project_id: P, group_key: "nutrition", owner_id: "u1", title: "Older", visibility: "private", revision: 1, created_at: at(0), updated_at: at(1) },
    { project_id: T2, group_key: "nutrition", owner_id: "u1", title: "Newer", visibility: "lab", revision: 1, created_at: at(0), updated_at: at(5) },
  ], [{ key: "nutrition", name: "Nutrition lab", role: "member", max_active_agents: 1, max_active_agents_per_principal: 1, max_queued_tasks: 1, retention_days: 30 }]);
  assert.equal(tree.length, 1);
  assert.equal(tree[0].label, "Nutrition lab");
  assert.deepEqual(tree[0].children?.map((c) => c.label), ["Newer", "Older"]);
  assert.equal(tree[0].children?.[0].detail, "Shared with the lab");
});

test("a project opens on its question, then its steps in plan order, analyses outside the plan, and its record", () => {
  const children = projectChildren(model(), profiles);
  assert.deepEqual(children.map((c) => [c.kind, c.group]), [["question", "question"], ["analysis", "steps"], ["analysis", "also"],
    ["data", "record"], ["files", "record"], ["runs", "record"], ["people", "record"], ["package", "record"]]);
  const [question, step, other] = children;
  assert.equal(question.label, "The question and the answer");
  assert.deepEqual([step.label, step.step, step.detail, step.tone], ["1 · Growth by dose", 1, "Needs a check", "attention"],
    "a step whose files could not be confirmed is marked for a check");
  assert.deepEqual([other.label, other.detail], ["Reviewed R analysis", "Did not finish"]);
  assert.deepEqual(step.children?.map((c) => c.label), ["Figures", "Tables", "Code", "Data used", "Analysis and reproducibility package", "Recorded steps"]);
  const tables = step.children!.find((c) => c.label === "Tables")!;
  assert.equal(tables.children?.[0].tone, "failed", "a file that fails its recorded checksum is marked");
  // An R PDF and its SVG copy were two rows with one name. They are one figure, which names
  // both of its formats.
  const figures = step.children!.find((c) => c.label === "Figures")!;
  assert.equal(figures.detail, "1");
  assert.deepEqual(figures.children?.map((f) => [f.memberPath, f.originalPath, f.formats]), [["results/growth_by_dose.svg", "results/growth_by_dose.pdf", "PDF, SVG"]]);
  assert.deepEqual(children.filter((c) => c.group === "record").map((c) => [c.label, c.detail]), [["Data used", "1"], ["All files", "4"],
    ["Work sessions", "1"], ["People and sharing", "Shared with the lab"], ["Evidence package", undefined]]);
  const run = children.find((c) => c.kind === "runs")!.children![0];
  assert.match(run.detail ?? "", /1 decision · 2 rejected replies/);
  assert.equal(run.tone, "attention");
});

test("the lab's own files and the tables prepared from them are listed apart from the analyses, with their files", () => {
  const m = model();
  const L1 = "32000000-0000-4000-8000-000000000001", L2 = "32000000-0000-4000-8000-000000000002", L3 = "32000000-0000-4000-8000-000000000003";
  const lab = (task_id: string, minute: number, operation: "intake" | "preparation", title: string | null) => ({ task_id, action_key: `dataset-${minute}`,
    parent_task_id: null, profile: "lab_dataset", status: "completed", attempt: 1, submitted_at: at(minute), updated_at: at(minute + 1),
    dataset: { title, operation, dataset: "d1", previous: null } });
  m.project.lab_data = [lab(L3, 8, "preparation", "GTT week 4"), lab(L1, 3, "intake", null), lab(L2, 6, "preparation", null)];
  m.evidence[L3] = { ...m.evidence[T1], taskId: L3 };
  const children = projectChildren(m, profiles);
  assert.deepEqual(children.filter((c) => c.group === "lab").map((c) => [c.label, c.taskId]),
    [["Lab file read", L1], ["Prepared table", L2], ["Prepared table: GTT week 4", L3]], "oldest first, apart from the analyses");
  assert.deepEqual(children.map((c) => c.group).filter((g, i, all) => all.indexOf(g) === i), ["question", "steps", "also", "lab", "record"]);
  assert.deepEqual(filesFor(m, profiles).map((g) => [g.title, g.rows.length]), [["Step 1: Growth by dose", 4], ["Prepared table: GTT week 4", 4]],
    "all files lists the prepared table's files after the analyses'");
  const prepared = children.find((c) => c.taskId === L3)!;
  assert.deepEqual(detailsFor(m, prepared, [...profiles, { key: "lab_dataset", name: "Lab data", description: "", tool_bundle: "", max_run_seconds: 60 }])
    .slice(0, 2), [["Status", "Finished"], ["Method", "Lab data"]], "a lab data task's record opens like an analysis's");
});

test("a planned step with no analysis yet is its own row, and it traces to the question", () => {
  const m = model();
  m.project.checkpoint!.state.plan.push({ action_key: "a2", title: "Check the residuals", status: "pending", task_id: null, evidence_refs: [], rationale: "Look for outliers" });
  const children = projectChildren(m, profiles);
  const planned = children.find((c) => c.kind === "planned")!;
  assert.deepEqual([planned.id, planned.label, planned.step, planned.group], [ids.planned(P, "a2"), "2 · Check the residuals", 2, "steps"]);
  assert.deepEqual(children.filter((c) => c.group === "steps").map((c) => c.step), [1, 2], "steps keep plan order");
  assert.deepEqual(traceFor(m, planned, profiles).map((h) => h.role), ["Plan step", "Question"]);
  assert.deepEqual(detailsFor(m, planned, profiles), [["Status", "Planned"], ["The agent's reason", "Look for outliers"]]);
});

test("a step that ended after the reader's last look is new, with its files; nothing is new before a first look", () => {
  const m = model();
  const newer = flatten(projectChildren(m, profiles, at(4)));
  assert.equal(newer.find((n) => n.id === ids.analysis(P, T1))?.fresh, true, "it ended at minute 5, after a look at minute 4");
  assert.ok(newer.filter((n) => n.kind === "file" && n.taskId === T1 && n.memberPath).every((n) => n.fresh), "its files are new too");
  assert.equal(flatten(projectChildren(m, profiles, at(8))).some((n) => n.fresh), false, "a later look leaves nothing new");
  assert.equal(flatten(projectChildren(m, profiles, null)).some((n) => n.fresh), false);
  assert.equal(endedSince({ status: "running", updated_at: at(9) }, at(1)), false, "a running analysis has not ended");
});

test("needs attention groups the problems by kind, most serious first, each with its count", () => {
  const groups = attentionFor({ ...model(), evidenceFailed: [T2] }, profiles);
  assert.deepEqual(groups.map((g) => [g.title, g.items.length]), [["Did not finish", 1], ["Files that do not match their checksums", 1],
    ["Steps to check", 1], ["Files that could not be read just now", 1], ["Replies the system refused", 1]]);
  assert.ok(groups.every((g) => g.items.every((p) => p.kind === g.kind && p.target)));
});

test("all files lists every retained file by step, one row per figure, with kinds, sizes and checksum state", () => {
  const groups = filesFor(model(), profiles, at(4));
  assert.deepEqual(groups.map((g) => [g.title, g.step, g.rows.length]), [["Step 1: Growth by dose", 1, 4]]);
  const rows = groups[0].rows;
  assert.deepEqual(rows.map((r) => [r.kind, r.path]), [["figure", "results/growth_by_dose.svg"], ["table", "results/summary.csv"],
    ["code", "code/analysis.R"], ["data", "input/observations.csv"]], "figures first, then tables, code and data");
  assert.deepEqual([rows[0].originalPath, rows[0].size], ["results/growth_by_dose.pdf", 20], "a figure's size counts both of its formats");
  assert.equal(rows[1].manifest, "mismatched");
  assert.ok(rows.every((r) => r.fresh && r.id === ids.file(P, T1, A, r.path)));
  assert.equal(filesFor(model(), profiles, at(8))[0].rows.some((r) => r.fresh), false);
});

test("the inspector's details come from the loaded records only", () => {
  const m = { ...model(), reviews: { [T1]: { review_id: "r", task_id: T1, attempt: 1, decision: "accepted" as const, note: null, reviewer_id: "u2",
    reviewer_name: "Rosa Ortiz", reviewer_kind: "person" as const, outputs: [], created_at: at(9), covers_current_outputs: true } } };
  const nodes = flatten(projectChildren(m, profiles));
  const details = (id: string) => Object.fromEntries(detailsFor(m, nodes.find((n) => n.id === id)!, profiles, "u1"));
  assert.deepEqual(details(ids.question(P)), { "Saved version": "3", Steps: "0 of 1 finished; the plan can change", Analyses: "2", Updated: details(ids.question(P)).Updated });
  const analysis = details(ids.analysis(P, T1));
  assert.equal(analysis.Method, "Reviewed R analysis");
  assert.equal(analysis.Files, "1 figures, 1 tables, 1 code, 1 data");
  assert.equal(analysis.Review, "Accepted by Rosa Ortiz");
  const file = details(ids.file(P, T1, A, "results/growth_by_dose.svg"));
  assert.deepEqual([file.File, file.Original, file.Size, file.Checksum], ["results/growth_by_dose.svg", "results/growth_by_dose.pdf", "20 bytes",
    "Matches the package checksum list"]);
  assert.deepEqual(details(ids.files(P)), { Files: "4", "Steps with files": "1", "Total size": "50 bytes", Checksums: "3 of 4 match their package checksum lists" });
  assert.deepEqual(details(ids.data(P)), { "Data sources": "0", "Copies the steps read": "1" });
});

test("where the project stands is read from the same counts as the lab index", () => {
  const facts = factsOf(model());
  assert.deepEqual(facts.task_counts, { total: 2, active: 0, finished: 1, failed: 1 }, "no review counts when reviews were not read");
  assert.equal(facts.latest_run?.status, "review_required");
  assert.equal(facts.question, "Does vitamin C dose change growth?");
  const reviewed = factsOf({ ...model(), reviews: {} });
  assert.deepEqual([reviewed.task_counts.reviewed, reviewed.task_counts.rejected, reviewed.task_counts.changes], [0, 0, 0]);
});

test("a figure traces back through its code, inputs, analysis and agent turn to the question", () => {
  const m = model();
  const nodes = flatten(projectChildren(m, profiles));
  const figure = nodes.find((n) => n.memberPath === "results/growth_by_dose.svg")!;
  const hops = traceFor(m, figure, profiles);
  assert.deepEqual(hops.map((h) => h.role), ["File", "Made by", "From inputs", "Analysis", "Proposed in", "Plan step", "Question"]);
  assert.equal(hops[1].target, ids.file(P, T1, A, "code/analysis.R"));
  assert.equal(hops[4].target, ids.turn(P, R, 1));
  assert.match(hops[4].detail ?? "", /not verified/);
  assert.equal(hops.at(-1)?.label, "Does vitamin C dose change growth?");
  for (const hop of hops) if (hop.target) assert.ok(nodes.some((n) => n.id === hop.target), `${hop.role} points at a real node`);
});

test("an analysis no loaded run lists is not attributed to anyone", () => {
  // Runs are paged, so a missing match is not evidence that a person asked.
  const m = model();
  const failed = flatten(projectChildren(m, profiles)).find((n) => n.id === ids.analysis(P, T2))!;
  const hops = traceFor(m, failed, profiles);
  assert.deepEqual(hops.map((h) => h.role), ["Analysis", "Requested by", "Question"]);
  const requested = hops[1];
  assert.equal(requested.label, "Not in the loaded record");
  assert.doesNotMatch(`${requested.label} ${requested.detail}`, /\bperson\b|outside a research run/i);
});

test("the broker's origin record says who asked once it is read, and never a guess", () => {
  const m = model();
  const failed = flatten(projectChildren(m, profiles)).find((n) => n.id === ids.analysis(P, T2))!;
  const me = "50000000-0000-4000-8000-000000000001";
  const requested = (origin: Lineage["origin"]) => traceFor(m, failed, profiles, { [T2]: origin }, me)[1];
  const base = { basis: "submission" as const, principal_name: null, principal_kind: "person" as const };
  assert.deepEqual(requested({ ...base, origin: "person", principal_id: me }),
    { role: "Requested by", label: "You", detail: "Recorded when it was submitted." });
  assert.equal(requested({ ...base, origin: "person", principal_id: "x", principal_name: "Rosa Ortiz" }).label, "Rosa Ortiz");
  assert.equal(requested({ ...base, origin: "research_run", principal_id: me, run_id: R, turn: 3 }).label, "A work session, decision 3");
  const earlier = requested({ ...base, origin: "unrecorded", basis: "before_origin_records", principal_id: me });
  assert.equal(earlier.label, "Not recorded");
  assert.match(earlier.detail ?? "", /before the broker recorded who asked/);
  // Another analysis's origin does not answer for this one.
  assert.equal(traceFor(m, failed, profiles, { [T1]: { ...base, origin: "person", principal_id: me } }, me)[1].label,
    "Not in the loaded record");
});

test("an analysis whose files could not be read is listed under problems", () => {
  // A failed evidence read must not disappear behind a finished status.
  const m = { ...model(), evidenceFailed: [T2] };
  const problems = problemsFor(m, profiles);
  const failedRead = problems.find((p) => /could not be read just now/.test(p.text));
  assert.ok(failedRead, "the failed read is listed");
  assert.equal(failedRead.target, ids.analysis(P, T2));
});

test("problems come only from recorded facts, rejected replies grouped", () => {
  const problems = problemsFor(model(), profiles);
  const texts = problems.map((p) => p.text).join("\n");
  // A stop for review is where the project stands, not a problem.
  assert.doesNotMatch(texts, /waiting for your review/);
  assert.match(texts, /This happened 2 times; no analysis ran because of it/);
  assert.match(texts, /“Growth by dose” needs a check/);
  assert.match(texts, /did not finish/);
  assert.match(texts, /1 file in “Growth by dose” does not match/);
  assert.ok(problems.every((p) => p.target));
});

test("search finds a figure by the name of its PDF original as well as its copy", () => {
  const nodes = flatten(projectChildren(model(), profiles));
  for (const name of ["growth_by_dose.pdf", "growth_by_dose.svg"])
    assert.deepEqual(search(nodes, name).map((n) => n.memberPath), ["results/growth_by_dose.svg"], name);
});

test("search needs every word and ranks label starts first", () => {
  const nodes = flatten(projectChildren(model(), profiles));
  assert.deepEqual(search(nodes, "growth dose").map((n) => n.label).slice(0, 1), ["Growth by dose"]);
  assert.equal(search(nodes, "growth zebra").length, 0);
  assert.ok(search(nodes, "tables").every((n) => n.kind !== "folder"));
  assert.ok(search(nodes, "analysis").length >= 2);
});

test("the path to a file includes its step and its kind folder", () => {
  const children = projectChildren(model(), profiles);
  const path = pathTo(children, ids.file(P, T1, A, "code/analysis.R"));
  assert.equal(path?.[0], ids.analysis(P, T1));
  assert.match(path?.[1] ?? "", /\/d:/);
  assert.equal(path?.length, 2);
  assert.equal(pathTo(children, "p:missing"), null);
});

test("workbench addresses carry only a project identifier and round-trip", async () => {
  const { href, parseRoute } = await import("./route.ts");
  assert.deepEqual(parseRoute("#/workbench"), { name: "workbench" });
  assert.deepEqual(parseRoute(`#/workbench/projects/${P.toUpperCase()}`), { name: "workbench", project: P });
  assert.equal(href({ name: "workbench", project: P }), `#/workbench/projects/${P}`);
  assert.deepEqual(parseRoute(href({ name: "workbench", project: P })), { name: "workbench", project: P });
  assert.deepEqual(parseRoute("#/workbench/projects/not-an-id"), { name: "home" });
});

test("the start page lists the chosen lab's most recently changed projects first, at most eight", () => {
  const project = (n: number, minute: number, title = `Project ${n}`, group_key = "lab") => ({ project_id: `10000000-0000-4000-8000-00000000000${n}`,
    group_key, owner_id: P, title, visibility: "lab" as const, revision: 1, created_at: at(0), updated_at: at(minute) });
  const list = [project(1, 5), project(2, 30), project(3, 30, "Alpha"), ...Array.from({ length: 7 }, (_, i) => project(4 + (i % 6), 10 + i)),
    project(9, 59, "Suite: newest of all", "nutrition-qualification"), project(8, 58, "Another lab's newest", "field")];
  const recent = recentProjects(list, "lab");
  assert.equal(recent.length, 8);
  assert.deepEqual(recent.slice(0, 2).map((p) => p.title), ["Alpha", "Project 2"], "newest first, then by title");
  assert.ok(!recent.some((p) => p.updated_at === at(5)), "the oldest project falls off the end");
  assert.ok(recent.every((p) => p.group_key === "lab"), "an operator lab's or another lab's newer projects stay out");
  assert.deepEqual(recentProjects(list, "lab", 2).map((p) => p.title), ["Alpha", "Project 2"]);
  assert.deepEqual(recentProjects(list, "nutrition-qualification").map((p) => p.title), ["Suite: newest of all"], "an operator lab's projects show once it is chosen");
  assert.equal(list.length, 12, "the reader's list is not reordered");
  assert.deepEqual(recentProjects([], "lab"), []);
  assert.deepEqual(recentProjects(list, ""), [], "no lab chosen yet lists nothing");
});

test("the project picker puts the chosen lab first and shows an operator lab only while it is chosen", () => {
  const groups = [{ key: "nutrition-qualification", name: "Operator qualification" }, { key: "fdc", name: "FoodData Central" },
    { key: "plant", name: "Plant Lab" }, { key: "empty", name: "A new lab" }];
  const project = (n: number, minute: number, group_key: string) => ({ project_id: `10000000-0000-4000-8000-00000000000${n}`,
    group_key, owner_id: P, title: `Project ${n}`, visibility: "lab" as const, revision: 1, created_at: at(0), updated_at: at(minute) });
  const list = [project(1, 50, "nutrition-qualification"), project(2, 10, "fdc"), project(3, 20, "plant"), project(4, 30, "plant")];
  const labs = (chosen: string) => pickerLabs(groups, list, chosen).map((g) => [g.key, g.projects.map((p) => p.title)]);
  assert.deepEqual(labs("plant"), [["plant", ["Project 4", "Project 3"]], ["fdc", ["Project 2"]]],
    "the chosen lab first with its newest project first, the other research lab next, the operator and empty labs left out");
  assert.deepEqual(labs("fdc"), [["fdc", ["Project 2"]], ["plant", ["Project 4", "Project 3"]]]);
  assert.deepEqual(labs("nutrition-qualification")[0], ["nutrition-qualification", ["Project 1"]], "a chosen operator lab shows its projects first");
  assert.deepEqual(labs("empty").map(([key]) => key), ["fdc", "plant"], "a chosen lab with no project is left out of the picker");
});
