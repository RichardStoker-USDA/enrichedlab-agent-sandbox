import assert from "node:assert/strict";
import test from "node:test";
import { agentNotes, buildHistory, reconciliationOnly } from "./history.ts";
import { deferralLabel, eventLabel, fileTitle, runSentence } from "./language.ts";
import { integritySentence, organize } from "./package_view.ts";
import type { PackageListing, PackageMember } from "./contracts.ts";
import type { Checkpoint } from "./projects.ts";

const member = (path: string, kind: PackageMember["kind"], media: string, manifest: PackageMember["manifest"] = "matched"): PackageMember =>
  ({ path, kind, media_type: media, size_bytes: 10, sha256: "a".repeat(64), manifest });

test("a package reads as results first, with a light figure and its R original together", () => {
  const listing: PackageListing = {
    format: "tar+gzip", checksum_file: "SHA256SUMS", missing_from_archive: [], skipped: [], expanded_bytes: 0,
    members: [
      member("SHA256SUMS", "checksum", "text/plain", "not_applicable"),
      member("results/descriptive/figure.pdf", "figure", "application/pdf"),
      member("results/descriptive/figure.svg", "figure", "image/svg+xml"),
      member("results/bootstrap_intervals.pdf", "figure", "application/pdf"),
      member("results/descriptive/summary.csv", "table", "text/csv"),
      member("input/reference/inputs/observations.csv", "table", "text/csv"),
      member("code/bootstrap_replay.R", "code", "text/plain"),
      member("run_parameters.json", "record", "application/json"),
      member("method.txt", "text", "text/plain"),
      member("stdout.log", "log", "text/plain", "mismatched"),
    ],
  };
  const view = organize(listing);
  assert.equal(view.figures.length, 2);
  const descriptive = view.figures.find((f) => f.display.path.endsWith("figure.svg"));
  assert.equal(descriptive?.original?.path, "results/descriptive/figure.pdf");
  assert.equal(descriptive?.title, "Descriptive figure");
  assert.equal(view.figures.find((f) => f.title === "Bootstrap intervals")?.display.media_type, "application/pdf");
  assert.deepEqual(view.tables.map((t) => t.path), ["results/descriptive/summary.csv"]);
  assert.deepEqual(view.inputs.map((t) => t.path), ["input/reference/inputs/observations.csv"]);
  assert.equal(view.parameters[0].path, "run_parameters.json");
  assert.equal(view.integrity.checked, 9);
  assert.deepEqual(view.integrity.mismatched, ["stdout.log"]);
  assert.match(integritySentence(view.integrity), /8 of 9 files match.*1 do not match/);
});

test("machine vocabulary becomes plain language and model retries are never tool work", () => {
  assert.equal(eventLabel("sandbox_pod_observed"), "Isolated workspace running");
  assert.equal(eventLabel("some_new_code"), "Some new code");
  assert.equal(deferralLabel("model_unavailable", "model_content_not_json"), "The planning model did not return a usable reply: the reply was not valid structured data");
  assert.equal(deferralLabel("proposal_invalid", "proposal_finding_rejected"),
    "The research agent's reply was rejected by validation: a finding read an analysis a person rejected");
  assert.equal(deferralLabel("proposal_invalid", "code_forbidden_function"),
    "The research agent's reply was rejected by validation: the platform's check refused the model-written code");
  const retry = runSentence({ status: "waiting", code: "proposal_invalid", turn: 1, max_turns: 6 }, false);
  assert.doesNotMatch(retry.text, /tool/i);
  assert.equal(runSentence(undefined, true).tone, "active");
  // A reader who cannot act is not told the stop waits for them.
  const stopped = { status: "review_required", code: "turn_limit", turn: 3, max_turns: 3 };
  assert.match(runSentence(stopped, false).text, /waiting for your review/);
  assert.doesNotMatch(runSentence(stopped, false, false).text, /your review/);
  assert.match(runSentence({ ...stopped, code: "review_required" }, false, false).text, /someone who can steer this project/);
  assert.equal(fileTitle("results/bootstrap_intervals.pdf"), "Bootstrap intervals");
});

const state = (goal: string, statuses: string[], lessons: { statement: string; kind: "observation" | "hypothesis" | "constraint" }[] = []) => ({
  schema_version: 1, goal, constraints: [], next_action: "Continue.",
  plan: statuses.map((status, i) => ({ action_key: `step-${i}`, title: `Step ${i}`, status, task_id: null, evidence_refs: status === "completed" ? ["artifact://x"] : [], rationale: "Because." })),
  lessons: lessons.map((l) => ({ ...l, evidence_refs: [] })),
  workspace: { schema_version: 1, profile: "p", image_digest: "", source_digest: "", input_refs: [], tool_refs: [], preparation_receipt: null },
});
const checkpoint = (revision: number, author: string, s: ReturnType<typeof state>): Checkpoint => ({ revision, author_id: author, created_at: `2026-09-22T0${revision}:00:00Z`, state: s });

test("history separates people, the agent and system records", () => {
  const me = "me";
  const checkpoints = [
    checkpoint(1, me, state("Q", ["pending"])),
    checkpoint(2, me, state("Q", ["active"], [{ statement: "The receipt is missing.", kind: "observation" }])),
    checkpoint(3, me, state("Q", ["completed"], [{ statement: "The receipt is missing.", kind: "observation" }])),
    checkpoint(4, "colleague", state("Q revised", ["completed"])),
  ];
  assert.equal(reconciliationOnly(checkpoints[1].state, checkpoints[2].state), true);
  const entries = buildHistory({
    me, checkpoints,
    runs: [{
      run: { run_id: "r1", project_id: "p", status: "review_required", turn: 1, max_turns: 6, max_parallel: 1, allowed_profiles: [], code: "review_required", updated_at: "" },
      turns: [{ turn: 1, context_revision: 1, checkpoint_revision: 2, created_at: "2026-09-22T02:00:00Z",
        record: { outcome: "delegate", scientific_validation: "not_performed", proposal: { summary: "Run the method.", actions: [] },
          method_review: { decision: "proceed", reason: "Fits." }, lead: { reported_model: "m" }, submitted_tasks: [{ task_id: "t1", action_key: "step-0" }] } }],
      events: [
        { run_id: "r1", sequence: 1, code: "model_unavailable", reason: "model_content_not_json", generation: 1, turn: 0, failures: 1, status_after: "waiting", occurred_at: "2026-09-22T01:30:00Z" },
        { run_id: "r1", sequence: 2, code: "proposal_invalid", reason: "proposal_schema_invalid", generation: 2, turn: 0, failures: 2, status_after: "waiting", occurred_at: "2026-09-22T01:31:00Z" },
        { run_id: "r1", sequence: 3, code: "waiting_for_tools", reason: null, generation: 3, turn: 1, failures: 0, status_after: "waiting", occurred_at: "2026-09-22T02:10:00Z" },
      ],
    }],
    tasks: [{ task_id: "t1", action_key: "step-0", status: "completed", submitted_at: "2026-09-22T02:00:01Z", updated_at: "2026-09-22T02:30:00Z", profile: "p" }],
    audit: [{ event_id: "e1", kind: "admin_inspection", occurred_at: "2026-09-22T05:00:00Z", actor_id: "admin", detail: { reason: "Quality review" } }],
    stepTitle: () => "Run the method",
  });
  const titles = entries.map((e) => `${e.source}:${e.title}`);
  assert.ok(titles.includes("you:Question and starting point saved"));
  assert.ok(titles.includes("system:Step status updated from the analysis record"));
  assert.ok(titles.includes("colleague:Research question edited"));
  assert.ok(titles.includes("agent:Research agent started an analysis"));
  assert.ok(titles.includes("system:2 replies from the planning model were rejected"));
  assert.ok(titles.includes("administrator:Administrator inspection started"));
  assert.ok(!titles.some((t) => /Waited for an analysis/.test(t)));
  assert.ok(entries.filter((e) => e.source === "agent").every((e) => e.unverified));
  const retry = entries.find((e) => e.title.startsWith("2 replies"));
  assert.match(retry?.secondary ?? "", /No analysis ran because of these replies/);
  const notes = agentNotes(checkpoints);
  assert.equal(notes[0].firstRevision, 2);
  assert.equal(notes[0].current, false);
  assert.equal(notes[0].label, "Agent's note, not verified");
});
