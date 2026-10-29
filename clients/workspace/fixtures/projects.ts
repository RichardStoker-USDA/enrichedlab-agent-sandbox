import { createHash, randomUUID } from "node:crypto";
import type { IncomingMessage, ServerResponse } from "node:http";
import { gzipSync } from "node:zlib";
import type { Artifact, LabNote, Task, TaskReview } from "../src/contracts.ts";
import { listing, tar } from "./package.ts";
import { fixtureQuery, fixtureQueryPackage } from "./materials.ts";
import { allowedFor, workspaceFor, type startsFixture } from "./starts.ts";
import { answerTable } from "./tables.ts";

// Synthetic projects, runs, access records and administration for the loopback preview.
// Names, questions and values are invented; nothing here comes from a real project.

type Json = (value: unknown, status?: number) => void;
const sha = (bytes: Buffer) => createHash("sha256").update(bytes).digest("hex");
// The library fixture's two papers and three of their passages.
const PAPER_1 = "80000000-0000-4000-8000-000000000001", PAPER_2 = "80000000-0000-4000-8000-000000000002";
const CHUNK = ["90000000-0000-4000-8000-000000000011", "90000000-0000-4000-8000-000000000012", "90000000-0000-4000-8000-000000000021"];
// The kept inputs of the answered decision: synthetic texts, addressed by their SHA-256.
const KEPT = new Map<string, string>();
const keep = (text: string) => { const address = "sha256:" + sha(Buffer.from(text)); KEPT.set(address, text); return address; };
const LEAD_INPUT = keep(JSON.stringify({ context_schema_version: 7,
  question: "How does plasma response differ between the reference diet and the two test diets?",
  analyses: [{ title: "Compare plasma response across the three diets", status: "completed" }],
  literature: [{ paper: "Synthetic handling study", pages: "page 2", text: "Samples were kept on ice and processed within an hour." }] }));
const REVIEW_INPUT = keep(JSON.stringify({ proposal: { action: "review_required", answer: "partly_answered" },
  question: "How does plasma response differ between the reference diet and the two test diets?" }));
const INSTRUCTIONS = keep("Synthetic preview instructions for the research lead.");

type Retained = { archive: Buffer; sha256: string; files: { path: string; bytes: Buffer }[]; checksum: Buffer };
// A project's task as the broker lists it; a figure version (migration 040) also names its figure.
export type ProjectTask = { task_id: string; action_key: string; parent_task_id: string | null; profile: string; status: string; attempt: number;
  submitted_at: string; updated_at: string; figure?: { title: string; feature: string; source_task: string; table_path: string; previous: string | null } | null;
  /** A lab dataset's intake or preparation; not an analysis. */
  dataset?: { title: string | null; operation: "intake" | "preparation"; dataset: string; previous: string | null } | null;
  /** A reviewed R analysis of an earlier result's table; an analysis like any other. */
  template_analysis?: { title: string; feature: string; source_task: string; table_path: string; previous: string | null } | null };
export function projectFixtures(options: {
  me: string; firstTask: string; secondTask: string; generatedTask: string; at: (seconds: number) => string;
  pkg: Retained; generated: Retained;
  // Projects another module serves, listed with these (fixtures/long_run.ts).
  also?: () => { project_id: string; updated_at: string }[];
  // The labs' starting points (fixtures/starts.ts).
  starts: ReturnType<typeof startsFixture>;
}) {
  const { me, firstTask, secondTask, generatedTask, at, pkg, generated } = options;
  const colleague = { principal_id: "20000000-0000-4000-8000-000000000002", display_name: "Example colleague", email: "colleague@example.invalid" };
  const other = { principal_id: "20000000-0000-4000-8000-000000000003", display_name: "Second colleague", email: "second@example.invalid" };
  const packageArtifact: Artifact = { id: "31000000-0000-4000-8000-000000000001", role: "result", label: "Analysis and reproducibility package",
    relative_path: `tasks/${firstTask}/payload`, media_type: "application/gzip", size_bytes: pkg.archive.length, sha256: pkg.sha256, created_at: at(170) };
  const checksumArtifact: Artifact = { id: "31000000-0000-4000-8000-000000000002", role: "result", label: "Package checksum",
    relative_path: `tasks/${firstTask}/payload.sha256`, media_type: "text/plain", size_bytes: pkg.checksum.length, sha256: sha(pkg.checksum), created_at: at(170) };
  // The model-written analysis's package, written by the reviewed template.
  const generatedArtifacts: Artifact[] = [
    { id: "31000000-0000-4000-8000-000000000003", role: "result", label: "Template analysis package",
      relative_path: `tasks/${generatedTask}/payload`, media_type: "application/gzip", size_bytes: generated.archive.length, sha256: generated.sha256, created_at: at(-20) },
    { id: "31000000-0000-4000-8000-000000000004", role: "result", label: "Package checksum",
      relative_path: `tasks/${generatedTask}/payload.sha256`, media_type: "text/plain", size_bytes: generated.checksum.length, sha256: sha(generated.checksum), created_at: at(-20) },
  ];
  const workspace = { schema_version: 1, profile: "open-methods-reference", image_digest: `sha256:${"a".repeat(64)}`, source_digest: `sha256:${"b".repeat(64)}`,
    input_refs: ["input://synthetic/diet-study"], tool_refs: ["tool://reviewed/diet-response-summary"], preparation_receipt: null };
  const goal = "How does plasma response differ between the reference diet and the two test diets, and which measured features change with diet A? Keep the exact inputs, code and figures so the analysis can be rerun locally.";
  const constraints = ["Treat each sample as the experimental unit.", "Use only the retained synthetic input."];
  const plan = (status: string, task: string | null) => [{ action_key: "summarize-diets", title: "Summarize each diet and test feature changes", status, task_id: task,
    evidence_refs: status === "completed" ? [`artifact://tasks/${firstTask}/${packageArtifact.id}`] : [], rationale: "The reviewed summary method answers both parts of the question and retains its code, tables and figures." },
    ...(status === "completed" ? [{ action_key: "compare-diets-anova", title: "Compare plasma response across the three diets", status: "completed", task_id: generatedTask,
      evidence_refs: [`artifact://tasks/${generatedTask}/${generatedArtifacts[0].id}`],
      rationale: "No reviewed tool compares all three diets at once, so a model-written one-way ANOVA ran in the reviewed template for review." }] : [])];
  const cellOf = (table: string, row: number, column: string) =>
    ({ ref: `artifact://tasks/${generatedTask}/${generatedArtifacts[0].id}`, table, row, column });
  const lesson = { statement: "The retained input has no preparation receipt, so the summary may not be reproducible.", kind: "observation", evidence_refs: [`artifact://tasks/${firstTask}/${packageArtifact.id}`] };
  const state = (revision: number) => ({ schema_version: 1, goal, constraints, workspace,
    plan: revision === 1 ? [] : revision === 2 ? plan("pending", null) : revision === 3 ? plan("active", firstTask) : plan("completed", firstTask),
    lessons: revision >= 3 ? [lesson] : [],
    next_action: revision >= 4 ? "The summary finished with retained files. Review the box plot, the volcano plot and the model checks before drawing any conclusion." : "Run the reviewed summary method once on the retained inputs." });
  const checkpoints = [1, 2, 3, 4].map((revision) => ({ revision, author_id: me, created_at: at(-600 + revision * 60), state: state(revision) }));
  const tasks: ProjectTask[] = [{ task_id: firstTask, action_key: "summarize-diets", parent_task_id: null, profile: "evidence_review", status: "completed", attempt: 1, submitted_at: at(0), updated_at: at(180) },
    { task_id: generatedTask, action_key: "compare-diets-anova", parent_task_id: null, profile: "generated_r", status: "completed", attempt: 1, submitted_at: at(-60), updated_at: at(-20) },
    { task_id: secondTask, action_key: "individual-check", parent_task_id: null, profile: "evidence_review", status: "failed", attempt: 1, submitted_at: at(-420), updated_at: at(-240) }];
  const project = { project_id: "50000000-0000-4000-8000-000000000001", group_key: "open_methods", owner_id: me, title: "Test diet response study",
    visibility: "lab", revision: 4, created_at: at(-700), updated_at: at(240) };
  const second = { project_id: "50000000-0000-4000-8000-000000000002", group_key: "open_methods", owner_id: me, title: "Feature selection for the follow-up",
    visibility: "lab", revision: 0, created_at: at(-900), updated_at: at(-800) };
  const foreign = { project_id: "50000000-0000-4000-8000-000000000003", group_key: "open_methods", owner_id: colleague.principal_id, title: "A colleague's private project",
    visibility: "private", revision: 1, created_at: at(-1200), updated_at: at(-1000) };
  const field = { project_id: "50000000-0000-4000-8000-000000000004", group_key: "field_trials", owner_id: me, title: "Field sampling plan",
    visibility: "lab", revision: 0, created_at: at(-1500), updated_at: at(-1400) };
  // A colleague's lab project the reader may open, and one of the reader's own with a session under way.
  const shared = { project_id: "50000000-0000-4000-8000-000000000005", group_key: "open_methods", owner_id: colleague.principal_id,
    title: "Breakfast fibre and morning fullness", visibility: "lab", revision: 1, created_at: at(-2000), updated_at: at(-50) };
  const sensors = { project_id: "50000000-0000-4000-8000-000000000006", group_key: "field_trials", owner_id: me, title: "Soil moisture sensor comparison",
    visibility: "private", revision: 1, created_at: at(-800), updated_at: at(-5) };
  const run = { run_id: "60000000-0000-4000-8000-000000000001", project_id: project.project_id, actor_id: me, initial_revision: 2, allowed_profiles: ["evidence_review", "generated_r"],
    max_turns: 6, max_parallel: 2, status: "review_required", turn: 1, generation: 5, failures: 0, code: "review_required", created_at: at(-400), updated_at: at(240), available_at: at(240) };
  const sharedRun = { ...run, run_id: "60000000-0000-4000-8000-000000000002", project_id: shared.project_id, actor_id: colleague.principal_id, initial_revision: 1,
    allowed_profiles: ["evidence_review"], status: "review_required", turn: 3, code: "turn_limit", created_at: at(-300), updated_at: at(-50), available_at: at(-50) };
  const sensorRun = { ...run, run_id: "60000000-0000-4000-8000-000000000003", project_id: sensors.project_id, initial_revision: 1,
    allowed_profiles: ["evidence_review"], status: "running", turn: 1, code: "accepted", created_at: at(-60), updated_at: at(-5), available_at: at(-5) };
  const turns = [{ turn: 1, generation: 3, context_revision: 2, checkpoint_revision: 3, created_at: at(-1),
    record: { schema_version: 1, outcome: "delegate", scientific_validation: "not_performed", proposal: { summary: "Run the reviewed summary method once on the retained inputs, then stop for review.", actions: [] },
      method_review: { decision: "proceed", reason: "The method matches the question and the declared experimental unit." }, lead: { reported_model: "synthetic-model" },
      submitted_tasks: [{ task_id: firstTask, action_key: "summarize-diets" }],
      orientation: { status: "read", step_keys: ["summarize-diets"], delegated: true, placed: false } } },
    { turn: 2, generation: 4, context_revision: 3, checkpoint_revision: 4, created_at: at(-60),
      record: { schema_version: 1, outcome: "delegate", scientific_validation: "not_performed",
        proposal: { summary: "Compare all three diets with a one-way ANOVA written for the reviewed template, then stop for review.",
          // The step cites the library passage on sample handling it read.
          actions: [{ action_key: "compare-diets-anova", title: "Compare plasma response across the three diets",
            rationale: "The methods note on sample handling describes the same three-diet comparison.", literature_refs: [`library://${PAPER_2}/${CHUNK[2]}`] }] },
        method_review: { decision: "proceed", reason: "A one-way ANOVA with Tukey comparisons fits three diet groups with one sample per participant." },
        lead: { reported_model: "synthetic-model" }, submitted_tasks: [{ task_id: generatedTask, action_key: "compare-diets-anova" }],
        context_inputs: { lab_notes: [], lab_notes_omitted: 0, literature: { status: "shown", query_sha256: sha(Buffer.from("question")), left_out: 0,
          passages: [{ paper_id: PAPER_2, chunk_id: CHUNK[2], pages: "page 2", text_sha256: sha(Buffer.from("handling")) }] } } } },
    // The decision after both analyses: an answer and findings whose values are placeholders bound
    // to cells of the template package's tables.
    { turn: 3, generation: 5, context_revision: 4, checkpoint_revision: 4, created_at: at(230),
      record: { schema_version: 1, outcome: "review_required", scientific_validation: "not_performed",
        proposal: { summary: "Both analyses finished. The comparison across the three diets answers how the plasma response differs; which features change with diet A still needs your review.",
          actions: [], answer: "partly_answered", findings: [
            { statement: "Diet A gave the highest mean plasma response ({a}), above the reference diet ({b}).",
              bindings: { a: cellOf("results/group_summary.csv", 2, "mean"), b: cellOf("results/group_summary.csv", 1, "mean") } },
            { statement: "The difference between diet A and the reference diet was {a}, with an adjusted p value of {b}.",
              bindings: { a: cellOf("results/diet_comparisons.csv", 1, "diff"), b: cellOf("results/diet_comparisons.csv", 1, "p adj") } }],
          // The whole comparison table as part of the answer. The agent named the table, three of its columns
          // and the opening words; the coordinator counted its rows from the digest.
          tables: [{ ref: cellOf("results/diet_comparisons.csv", 1, "diff").ref, table: "results/diet_comparisons.csv",
            columns: ["comparison", "diff", "p adj"], lead_in: "Every pairwise comparison between the diets", rows: 3 }],
          // The answerability ledger: one part stated by both findings and the table, one waiting for a choice.
          parts: [{ part: "How plasma response differs between the reference diet and the two test diets", state: "supported", findings: [1, 2], tables: [1] },
            { part: "Which measured features change with diet A", state: "awaiting",
              reason: "No analysis covers the measured features yet; choose which features to compare, or a method that screens them all." }] },
        method_review: { decision: "needs_review", reason: "The comparison supports the answer about plasma response. The second part of the question, which features change with diet A, has no analysis yet, so a scientist should choose the next step." },
        lead: { reported_model: "synthetic-model", system_sha256: INSTRUCTIONS, context_sha256: LEAD_INPUT },
        methods_specialist: { reported_model: "synthetic-model", system_sha256: INSTRUCTIONS, context_sha256: REVIEW_INPUT }, submitted_tasks: [],
        // Each result's check against what its step said it must show, as the agent read it.
        step_checks: { omitted: 0, checks: [
          { key: "summarize-diets", task_id: firstTask, ref: `artifact://tasks/${firstTask}/${packageArtifact.id}`, state: "passed",
            table: "results/group_summary.csv" },
          { key: "compare-diets-anova", task_id: generatedTask, ref: `artifact://tasks/${generatedTask}/${generatedArtifacts[0].id}`,
            state: "not_checkable", table: "results/diet_comparisons.csv",
            reasons: ["The result's summary does not count the distinct values of column comparison."] }] },
        // The records this decision's context came from (context version 5): one note shared in the lab.
        context_inputs: { lab_notes: [{ note_id: "70000000-0000-4000-8000-000000000001", statement_sha256: sha(Buffer.from("fibre")) }], lab_notes_omitted: 0,
          // And three passages from the lab's library: two from one paper (context version 6), and one
          // section of a paper in the Commons, which a person marked for every lab.
          literature: { status: "shown", query_sha256: sha(Buffer.from("question")), left_out: 1, via: "knowledge", passages: [
            { paper_id: PAPER_1, chunk_id: CHUNK[0], pages: "page 3", text_sha256: sha(Buffer.from("methods")) },
            { paper_id: PAPER_2, chunk_id: CHUNK[2], pages: "page 2", text_sha256: sha(Buffer.from("handling")) },
            { paper_id: PAPER_1, chunk_id: CHUNK[1], pages: "page 4", text_sha256: sha(Buffer.from("results")) },
            { paper_id: "01920000-0000-7000-8000-00000000c0a1", section_id: "01920000-0000-7000-8000-00000000c0a2",
              section_key: "eeeeeeeeeeeeeeee-1", pages: "page 6", text_sha256: sha(Buffer.from("commons")), origin: "commons",
              title: "Satiety markers after oat breakfasts: a synthetic commons paper" }] } } } }];
  const runEvents = [
    { run_id: run.run_id, sequence: 1, code: "model_unavailable", reason: "model_content_not_json", generation: 1, turn: 0, failures: 1, status_after: "waiting", occurred_at: at(-300) },
    { run_id: run.run_id, sequence: 2, code: "proposal_invalid", reason: "proposal_schema_invalid", generation: 2, turn: 0, failures: 2, status_after: "waiting", occurred_at: at(-200) },
    { run_id: run.run_id, sequence: 3, code: "waiting_for_tools", reason: null, generation: 4, turn: 1, failures: 0, status_after: "waiting", occurred_at: at(20) },
    { run_id: run.run_id, sequence: 4, code: "review_required", reason: null, generation: 5, turn: 1, failures: 0, status_after: "review_required", occurred_at: at(240) },
  ];
  const audit = [
    { event_id: randomUUID(), actor_id: me, kind: "research_started", occurred_at: at(-400), detail: { run_id: run.run_id } },
    { event_id: randomUUID(), actor_id: me, kind: "access_changed", occurred_at: at(-350), detail: { member_id: colleague.principal_id, capabilities: ["read", "review"] } },
    { event_id: randomUUID(), actor_id: "20000000-0000-4000-8000-000000000099", kind: "admin_inspection", occurred_at: at(300), detail: { reason: "Quality review of retained evidence", expires_at: at(2100) } },
  ];
  const grants = new Map<string, string[]>([[colleague.principal_id, ["read", "review"]]]);
  const inspections = new Map<string, string>();
  type Saved = { revision: number; author_id: string; created_at: string; state: ReturnType<typeof state> };
  const history = new Map<string, Saved[]>([
    [project.project_id, checkpoints],
    [second.project_id, []],
    [field.project_id, []],
    [foreign.project_id, [{ revision: 1, author_id: colleague.principal_id, created_at: at(-1100), state: { ...state(1), goal: "A colleague's synthetic question." } }]],
    [shared.project_id, [{ revision: 1, author_id: colleague.principal_id, created_at: at(-1900), state: { ...state(1), goal: "Does a higher-fibre breakfast change reported fullness over the morning in the pilot cohort?" } }]],
    [sensors.project_id, [{ revision: 1, author_id: me, created_at: at(-700), state: { ...state(1), goal: "Do the two soil moisture sensors agree across the field plots, and where do they differ most?" } }]],
  ]);
  const runs: (typeof run)[] = [sensorRun, run, sharedRun];
  // The reader's marks, by kind, reference and version.
  const seen = new Map<string, string>();
  function inboxItems() {
    const lastTurn = [...turns].sort((a, b) => b.turn - a.turn)[0];
    const asked = run.status === "review_required" ? [{ kind: "review_request", ref: run.run_id, version: run.run_id, at: run.updated_at,
      lab: project.group_key, project_id: project.project_id, project_title: project.title, run_id: run.run_id, code: run.code, turn: lastTurn.turn,
      reason_source: lastTurn.record.method_review.decision === "needs_review" ? "methods_check" : "lead",
      reason: lastTurn.record.method_review.decision === "needs_review" ? lastTurn.record.method_review.reason : lastTurn.record.proposal.summary }] : [];
    const unreviewed = tasks.filter((t) => !t.figure && !t.dataset && (t.status === "completed" || t.status === "review_required")
      && !newestFirst(reviews.filter((r) => r.task_id === t.task_id && r.reviewer_kind === "person"))[0]?.covers_current_outputs)
      .map((t) => ({ kind: "analysis_review", ref: t.task_id, version: JSON.stringify(outputsOf(t.task_id)).length.toString(), at: t.updated_at,
        lab: project.group_key, project_id: project.project_id, project_title: project.title, task_id: t.task_id, action_key: t.action_key, status: t.status }));
    // The sensor project's session asks too once the browser suite stops it (fixture/sensor-run-stops).
    const sensorAsked = sensorRun.status === "review_required" ? [{ kind: "review_request", ref: sensorRun.run_id, version: `${sensorRun.run_id}:${sensorRun.turn}`,
      at: sensorRun.updated_at, lab: sensors.group_key, project_id: sensors.project_id, project_title: sensors.title, run_id: sensorRun.run_id, code: sensorRun.code,
      turn: sensorRun.turn, reason_source: "lead", reason: "Both sensors' readings are loaded. Review the agreement table before the next comparison." }] : [];
    // A paper waits in the library of another lab the reader leads (it showed in the wrong lab's menus).
    const papers = [{ kind: "paper_acceptance", ref: "food_composition", version: "paper-1", at: at(-400), lab: "food_composition", papers: 1 }];
    return [...asked, ...sensorAsked, ...unreviewed, ...papers].map((i) => ({ ...i, seen_at: seen.get(`${i.kind}:${i.ref}:${i.version}`) ?? null }));
  }
  // The broker's project fingerprint (src/broker/api/projects.rs, project_pulse) from the same fields: no timestamps
  // that move without a visible change, and a session's running and waiting read as one while an analysis is active.
  function pulseOf(p: typeof project) {
    const view = projectView(p);
    const own = runs.filter((r) => r.project_id === p.project_id);
    const analysis = view.tasks.some((t) => active(t.status));
    const text = JSON.stringify([view.revision, view.updated_at, view.title, view.visibility, [...view.capabilities].sort(), view.inspection_expires_at,
      view.task_count, view.tasks.map((t) => [t.task_id, t.status, t.attempt]).sort(),
      own.map((r) => [r.run_id, analysis && ["running", "waiting"].includes(r.status) ? "active" : r.status, r.turn, r.code, r.failures]).sort()]);
    return { pulse: sha(Buffer.from(text)).slice(0, 32), busy: analysis || own.some((r) => ["queued", "running", "waiting"].includes(r.status)) };
  }
  // The plan ledger as the broker records it: each saved revision against the one before it, labeled
  // the agent's when a decision saved it, the analysis record's when only statuses or results changed, else its author's.
  type Step = { action_key: string; title: string; status: string; task_id: string | null; evidence_refs: string[]; rationale: string };
  function planLedgerOf(projectId: string) {
    const saved = history.get(projectId) ?? [];
    const revisions = saved.map((checkpoint, index) => {
      const before = new Map<string, [Step, number]>(((saved[index - 1]?.state.plan ?? []) as Step[]).map((s, i) => [s.action_key, [s, i + 1]]));
      const after = new Map<string, [Step, number]>((checkpoint.state.plan as Step[]).map((s, i) => [s.action_key, [s, i + 1]]));
      const steps = [...new Set([...before.keys(), ...after.keys()])].flatMap((key) => {
        const a = before.get(key)?.[0], b = after.get(key)?.[0];
        const changes = !a ? ["planned"] : !b ? ["removed"] : [
          ...(a.title !== b.title || a.rationale !== b.rationale ? ["text"] : []), ...(a.task_id !== b.task_id ? ["task"] : []),
          ...(a.status !== b.status ? ["status"] : []), ...(JSON.stringify(a.evidence_refs) !== JSON.stringify(b.evidence_refs) ? ["results"] : [])];
        if (!changes.length) return [];
        const step = (b ?? a)!, text = changes.includes("planned") || changes.includes("text");
        return [{ action_key: key, step_number: (after.get(key) ?? before.get(key))![1], changes, status_before: a?.status ?? null, status_after: b?.status ?? null,
          task_id: step.task_id, result_count: step.evidence_refs.length, title: text ? b!.title : null, rationale: text ? b!.rationale : null,
          task_status: null as string | null, error_code: null as string | null }];
      }).sort((x, y) => x.step_number - y.step_number || x.action_key.localeCompare(y.action_key));
      // The synthetic decisions all belong to the main project's run.
      const turn = projectId === project.project_id ? turns.find((t) => t.checkpoint_revision === checkpoint.revision) : undefined;
      const recordOnly = index > 0 && steps.length > 0 && steps.every((s) => s.changes.every((c) => c === "status" || c === "results" || c === "task"));
      const source = turn ? "lead_turn" : recordOnly ? "analysis_record" : "person";
      if (source === "analysis_record") for (const s of steps) {
        const task = tasks.find((t) => t.task_id === s.task_id);
        if (task && ["completed", "failed", "uncertain"].includes(s.status_after ?? "")) s.task_status = task.status;
      }
      return { revision: checkpoint.revision, recorded_at: checkpoint.created_at, source, run_id: turn || recordOnly ? runs.find((r) => r.project_id === projectId)?.run_id ?? null : null,
        turn: turn?.turn ?? null, principal_id: source === "person" ? checkpoint.author_id : null, steps };
    });
    return { revisions, unrecorded_revisions: 0, truncated: false };
  }
  const runTurns = new Map<string, typeof turns>([[run.run_id, turns]]);
  const runLog = new Map<string, typeof runEvents>([[run.run_id, runEvents]]);
  const uuid = /^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/;

  const body = async (req: IncomingMessage) => { let text = ""; for await (const chunk of req) text += chunk; return text ? JSON.parse(text) : {}; };
  const capabilitiesOf = (p: typeof project) => p.owner_id === me ? ["read", "review", "contribute", "steer", "cancel", "manage"] : ["read"];
  const ownerOf = (p: typeof project) => p.owner_id === me ? { principal_id: me, display_name: "Example researcher", kind: "person" }
    : { principal_id: colleague.principal_id, display_name: colleague.display_name, kind: "person" };
  const active = (status: string) => ["queued", "leased", "starting", "running", "cancelling"].includes(status);
  // The list's summary, shaped like broker migration 020's broker_list_projects.
  const summary = (p: typeof project) => {
    const saved = history.get(p.project_id)?.at(-1);
    const latest = runs.filter((r) => r.project_id === p.project_id).sort((a, b) => b.created_at.localeCompare(a.created_at))[0];
    // Figure versions are drawings and lab data is the lab's own table, not analyses (migrations 040 and 059): the
    // counts leave them out.
    const own = p === project ? tasks.filter((t) => !t.figure && !t.dataset) : [];
    return { ...p, owner: ownerOf(p), question: saved?.state.goal ?? null, capabilities: capabilitiesOf(p), last_look: lookOf(p),
      latest_run: latest ? { run_id: latest.run_id, status: latest.status, code: latest.code, turn: latest.turn, max_turns: latest.max_turns, updated_at: latest.updated_at } : null,
      task_counts: { total: own.length, active: own.filter((t) => active(t.status)).length,
        finished: own.filter((t) => t.status === "completed" || t.status === "review_required").length, failed: own.filter((t) => t.status === "failed").length,
        ...reviewCounts(own) } };
  };
  // Projects created while the preview runs; a fixture reset forgets them.
  const made: (typeof project)[] = [];
  // The reader's last look at each project: forward-only, kept per reader.
  // The reader last looked at the main project before its two analyses finished, so its page has news to show.
  const looks = new Map<string, { revision: number; at: string }>([[project.project_id, { revision: 3, at: at(-30) }]]);
  const preparedBy = new Map<string, { revision: number; allowed_profiles: string[] }>();
  const lookOf = (p: typeof project) => looks.get(p.project_id) ?? { revision: null, at: null };
  const projectView = (p: typeof project) => ({ ...p, capabilities: capabilitiesOf(p), last_look: lookOf(p),
    inspection_expires_at: p.owner_id === me ? null : inspections.get(p.project_id) ?? null,
    checkpoint: history.get(p.project_id)?.at(-1) ?? null,
    tasks: p === project ? tasks : [], task_count: p === project ? tasks.length : 0 });

  // Where each synthetic analysis came from, shaped like the broker's task lineage: the two
  // run tasks name their turns, the failed check predates origin records, and anything
  // outside the project says it has no project inputs.
  // Preview-only: the browser suite switches the model-written analysis to a configured recipe, or to a database
  // query whose package keeps the same artifact ids so the plan's references still resolve.
  const mode = { configured: false, query: false };
  const original = { pkg: { ...generated }, artifacts: generatedArtifacts.map((a) => ({ ...a })) };
  function queryMode(on: boolean, mtime: number) {
    mode.query = on;
    const next = on ? fixtureQueryPackage(mtime) : original.pkg;
    Object.assign(generated, next);
    generatedArtifacts[0] = on ? { ...original.artifacts[0], label: "Database query and reproducibility package", size_bytes: next.archive.length, sha256: next.sha256 }
      : { ...original.artifacts[0] };
    generatedArtifacts[1] = on ? { ...original.artifacts[1], size_bytes: next.checksum.length, sha256: sha(next.checksum) } : { ...original.artifacts[1] };
  }
  function lineage(task: Task) {
    const inProject = tasks.find((x) => x.task_id === task.id);
    const turn = turns.find((x) => x.record.submitted_tasks.some((s) => s.task_id === task.id));
    // As the broker reads it: the step first recorded for the task, its current status, and any later wording.
    const containing = checkpoints.filter((c) => c.state.plan.some((s) => s.task_id === task.id));
    const step = containing[0]?.state.plan.find((s) => s.task_id === task.id);
    const now = containing.at(-1)?.state.plan.find((s) => s.task_id === task.id);
    const generatedRun = task.id === generatedTask;
    const queryRun = generatedRun && mode.query;
    const configuredRun = generatedRun && mode.configured && !queryRun;
    const outputs = (task.id === firstTask ? [packageArtifact, checksumArtifact] : generatedRun ? generatedArtifacts : [])
      .map((a) => ({ artifact_id: a.id, role: a.role, label: a.label, relative_path: a.relative_path, media_type: a.media_type,
        size_bytes: a.size_bytes, sha256: a.sha256, created_at: a.created_at,
        provenance_class: queryRun ? "query_output" : configuredRun ? "configured_tool_output" : generatedRun ? "agent_built_tool_output" : "agent_session_output" }));
    // As migration 039 reads it: a query's source states its kind of data, the model-written analysis's copy was sealed
    // with its class recorded, and the earlier analyses predate those records.
    const classes = queryRun || generatedRun ? ["public_reference"] : [];
    const gaps = [
      ...(!inProject ? ["not_in_project"] : !turn ? ["origin_before_records"] : []),
      ...(inProject && !step ? ["plan_step_not_recorded"] : []),
      ...(inProject && !classes.length ? ["input_class_not_recorded"] : []),
      ...(!outputs.length && ["completed", "failed", "cancelled", "review_required"].includes(task.status) ? ["no_outputs_recorded"] : []),
    ];
    return {
      schema: "enrichedlab.task-lineage.v1",
      task: { task_id: task.id, project_id: inProject ? project.project_id : null, action_key: inProject?.action_key ?? null,
        objective: task.objective ?? "", status: task.status, result_class: task.result_class, error_code: task.error_code,
        attempt: task.attempt, submitted_at: task.submitted_at, started_at: task.started_at, finished_at: task.finished_at },
      origin: turn ? { origin: "research_run", basis: "submission", principal_id: me, principal_name: "Example researcher",
          principal_kind: "person", run_id: run.run_id, turn: turn.turn, recorded_at: turn.created_at }
        : inProject ? { origin: "unrecorded", basis: "before_origin_records", principal_id: me, principal_name: "Example researcher", principal_kind: "person" }
        : { origin: "person", basis: "submission", principal_id: me, principal_name: "Example researcher", principal_kind: "person", recorded_at: task.submitted_at },
      plan_step: step && now ? { action_key: step.action_key, title: step.title, status: now.status, rationale: step.rationale,
        revision: containing[0].revision, added_after_submission: false,
        current: now.title !== step.title || now.rationale !== step.rationale
          ? { title: now.title, rationale: now.rationale, revision: containing.at(-1)!.revision } : null } : null,
      inputs: { workspace: inProject ? workspace : null, checkpoint_revision: inProject ? turn?.checkpoint_revision ?? 4 : null, platform_tool: null,
        classes, classes_recorded_by: queryRun ? "query_source" : generatedRun ? "input_custodian" : null },
      method: { profile: queryRun ? "reference-sql-example" : task.profile,
        profile_name: queryRun ? "Query the example reference data" : configuredRun ? "Bootstrap interval with settings" : generatedRun ? "Model-written R analysis" : "Evidence review",
        tool_class: queryRun ? "guarded_query" : configuredRun ? "configured" : generatedRun ? "agent_built" : "agent_session",
        template_ref: generatedRun && !queryRun ? "template://r-generated-analysis/v2" : null,
        template_sha256: generatedRun && !queryRun ? `sha256:${"c".repeat(64)}` : null, recorded: "submission", harness: null, role: null, tool_bundle: null },
      code: null,
      query: queryRun ? (({ source, sql_sha256, guard, relations, row_limit, run_id, turn, created_at, title, release, snapshot }) =>
        ({ source, sha256: sql_sha256, guard, relations, row_limit, run_id, turn, created_at, title, release,
          snapshot_sha256: snapshot.manifest_sha256, data_class: "public_reference" }))(fixtureQuery(task.id, run.run_id, at(-60), generated.sha256)) : null,
      execution: { image_ref: `registry.example.invalid/release/agent-sandbox/site@sha256:${"7".repeat(64)}`, cpu_limit: "2",
        memory_limit: "4Gi", max_run_seconds: 900, attempts: task.attempt, scientific_history: task.id === firstTask ? 1 : 0 },
      outputs, review: { state: "unvalidated" }, gaps,
    };
  }

  // People's reviews, shaped like broker migration 023's rows: the colleague asked for changes
  // to the model-written comparison; the browser suite records the rest.
  const outputsOf = (id: string) => (id === firstTask ? [packageArtifact, checksumArtifact] : id === generatedTask ? generatedArtifacts : [])
    .map((a) => ({ artifact_id: a.id, relative_path: a.relative_path, sha256: a.sha256 }));
  const reviews: (TaskReview & { request_id: string })[] = [{ review_id: randomUUID(), request_id: randomUUID(), task_id: generatedTask, attempt: 1,
    decision: "needs_changes", note: "Report each diet's group size beside its mean, so a reader can judge the spread.",
    reviewer_id: colleague.principal_id, reviewer_name: colleague.display_name, reviewer_kind: "person", outputs: outputsOf(generatedTask),
    created_at: at(-10), covers_current_outputs: true }];
  const reviewView = ({ request_id: _, ...r }: (typeof reviews)[number]): TaskReview =>
    ({ ...r, covers_current_outputs: JSON.stringify(r.outputs) === JSON.stringify(outputsOf(r.task_id)) });
  const newestFirst = (list: typeof reviews) => [...list].sort((a, b) => b.created_at.localeCompare(a.created_at)).map(reviewView);
  // As migration 026 counts them: each finished analysis's newest review by a person that still covers its files.
  function reviewCounts(own: typeof tasks) {
    const newest = own.filter((t) => t.status === "completed" || t.status === "review_required")
      .map((t) => newestFirst(reviews.filter((r) => r.task_id === t.task_id && r.reviewer_kind === "person"))[0])
      .filter((r): r is TaskReview => !!r && r.covers_current_outputs);
    return { reviewed: newest.length, rejected: newest.filter((r) => r.decision === "rejected").length,
      changes: newest.filter((r) => r.decision === "needs_changes").length };
  }

  // Lab memory, shaped like broker migration 027's records: a colleague's note in use and one retired,
  // both from the colleague's lab project; the browser suite shares, renews and retires the reader's own.
  type NoteEvent = LabNote["events"][number] & { actor_id: string; request_id: string };
  type NoteRecord = Omit<LabNote, "status" | "events" | "project_title"> & { review_by: string; request_id: string; events: NoteEvent[] };
  const days = (n: number) => new Date(Date.now() + n * 86400000).toISOString();
  const colleagueNote = (id: string, statement: string, sharedAt: string, reason: string | null): NoteRecord => ({
    note_id: id, lab: "open_methods", project_id: shared.project_id, revision: 1, kind: "observation", statement,
    statement_sha256: sha(Buffer.from(statement)),
    evidence: [{ task_id: "10000000-0000-4000-8000-0000000000a1", refs: ["artifact://tasks/10000000-0000-4000-8000-0000000000a1/31000000-0000-4000-8000-0000000000a1"],
      title: "Summarize fullness scores by breakfast", method: "evidence_review", outputs: [], review_id: randomUUID() }],
    methods: ["evidence_review"], data: ["input://synthetic/breakfast-pilot"],
    shared_by: { principal_id: colleague.principal_id, name: colleague.display_name, kind: "person" }, reason, shared_at: sharedAt,
    review_by: days(150), request_id: randomUUID(), events: [] });
  const notes: NoteRecord[] = [
    colleagueNote("70000000-0000-4000-8000-000000000001", "Participants reported feeling full for longer after the higher-fibre breakfast than after the standard one.",
      at(-40), "Checked against the paper questionnaires."),
    { ...colleagueNote("70000000-0000-4000-8000-000000000002", "The first morning's fullness scores ran higher than later mornings.", at(-900), null),
      events: [{ kind: "retired", actor_id: colleague.principal_id, actor_name: colleague.display_name, reason: "A later analysis traced this to one participant.",
        review_by: null, recorded_at: at(-100), request_id: randomUUID() }] },
  ];
  const own = (task: string) => tasks.some((t) => t.task_id === task);
  // The broker's fixed rule: retired, then past its review date, then paused by a newer review or changed files.
  function noteStatus(n: NoteRecord): LabNote["status"] {
    const retired = n.events.find((e) => e.kind === "retired");
    const due = [...n.events].reverse().find((e) => e.kind === "renewed")?.review_by ?? n.review_by;
    if (retired) return { state: "retired", review_by: due, retired_at: retired.recorded_at, reason: retired.reason ?? undefined };
    if (Date.now() >= Date.parse(due)) return { state: "expired", review_by: due };
    for (const e of n.evidence.filter((x) => own(x.task_id))) {
      if (JSON.stringify(outputsOf(e.task_id)) !== JSON.stringify(e.outputs)) return { state: "paused", review_by: due, cause: "evidence_files_changed", task_id: e.task_id };
      const newest = newestFirst(reviews.filter((r) => r.task_id === e.task_id && r.reviewer_kind === "person"))[0];
      if (newest?.decision !== "accepted") return { state: "paused", review_by: due, cause: "evidence_review_changed", task_id: e.task_id };
    }
    return { state: "active", review_by: due };
  }
  const noteView = ({ review_by: _, request_id: __, events, ...n }: NoteRecord): LabNote => ({ ...n,
    project_title: [project, shared].find((p) => p.project_id === n.project_id)?.title ?? null,
    status: noteStatus({ ...n, review_by: _, request_id: __, events }),
    events: events.map(({ actor_id: ___, request_id: ____, ...e }) => e) });
  const noteRefused = (json: Json, code: string, status: number) => json({ error: { code, message: "Refused" } }, status);

  return {
    get configured() { return mode.configured; },
    get query() { return mode.query; },
    queryMode,
    // The review routes of one task; the task server has already found the task.
    async taskReviews(task: Task, req: IncomingMessage, json: Json) {
      if (req.method === "GET") { json({ reviews: newestFirst(reviews.filter((r) => r.task_id === task.id)) }); return; }
      const input = await body(req);
      if (!tasks.some((t) => t.task_id === task.id)) { json({ error: { code: "review_forbidden" } }, 403); return; }
      const note = typeof input.note === "string" ? input.note.trim() || null : null;
      const same = reviews.find((r) => r.reviewer_id === me && r.request_id === input.request_id);
      if (same) {
        if (same.decision !== input.decision || same.note !== note) { json({ error: { code: "idempotency_conflict" } }, 409); return; }
        json({ review: reviewView(same) }, 201); return;
      }
      if (!uuid.test(input.request_id ?? "") || !["accepted", "needs_changes", "rejected"].includes(input.decision)
        || (input.decision !== "accepted" && !note) || (note?.length ?? 0) > 2000) { json({ error: { code: "review_invalid" } }, 400); return; }
      if (!["completed", "failed", "cancelled", "review_required"].includes(task.status)) { json({ error: { code: "review_not_finished" } }, 409); return; }
      if (input.decision === "accepted" && (!["completed", "review_required"].includes(task.status) || !outputsOf(task.id).length)) {
        json({ error: { code: "review_nothing_to_accept" } }, 409); return;
      }
      const saved = { review_id: randomUUID(), request_id: input.request_id, task_id: task.id, attempt: task.attempt, decision: input.decision, note,
        reviewer_id: me, reviewer_name: "Example researcher", reviewer_kind: "person" as const, outputs: outputsOf(task.id),
        created_at: new Date().toISOString(), covers_current_outputs: true };
      reviews.push(saved);
      json({ review: reviewView(saved) }, 201);
    },
    configure(on: boolean) { mode.configured = on; },
    // Every project this module serves, and whether work runs on one, for the preview's change stream.
    ids() { return [project, second, foreign, field, shared, sensors, ...made].map((p) => p.project_id); },
    busy(id: string): boolean | undefined {
      const p = [project, second, foreign, field, shared, sensors, ...made].find((x) => x.project_id === id);
      return p ? pulseOf(p).busy : undefined;
    },
    // Preview-only: the sensor project's session stops for review while its page is open, as a long run would.
    stopSensorRun() {
      const now = new Date().toISOString();
      Object.assign(sensorRun, { status: "review_required", code: "review_required", turn: 2, updated_at: now, available_at: now });
    },
    artifacts: [packageArtifact, checksumArtifact],
    // The first analysis's package, the source of the preview's figure versions, and a way to list a new task.
    pkg,
    addTask(entry: ProjectTask) { tasks.push(entry); },
    generatedArtifacts,
    generated,
    runId: run.run_id,
    // The preview's research runs, for the answer documents fixture.
    runs: () => runs.map((r) => ({ run_id: r.run_id, project_id: r.project_id, turn: r.turn })),
    lineage,
    async handle(req: IncomingMessage, res: ServerResponse, url: URL, json: Json, denied: () => void): Promise<boolean> {
      const path = url.pathname;
      // What waits for the reader, as the broker computes it for these records: the main project's
      // stop for review and its finished analyses without a person's review covering their files.
      if (path === "/api/v1/inbox" && req.method === "GET") { json({ can_acknowledge: true, items: inboxItems() }); return true; }
      if (path === "/api/v1/inbox/acknowledgements" && req.method === "POST") {
        const input = await body(req);
        const item = inboxItems().find((i) => i.kind === input.kind && i.ref === input.ref && i.version === input.version);
        if (!item || !uuid.test(input.request_id ?? "")) { json({ error: { code: "inbox_item_not_found" } }, 404); return true; }
        const key = `${item.kind}:${item.ref}:${item.version}`;
        if (!seen.has(key)) seen.set(key, new Date().toISOString());
        json({ kind: item.kind, ref: item.ref, version: item.version, seen_at: seen.get(key) }); return true;
      }
      const labMatch = path.match(/^\/api\/v1\/labs\/([a-z][a-z0-9_-]{1,63})\/notes$/);
      if (labMatch && req.method === "GET") {
        if (!["open_methods", "field_trials"].includes(labMatch[1])) { denied(); return true; }
        json({ lab: labMatch[1], can_curate: labMatch[1] === "open_methods",
          notes: notes.filter((n) => n.lab === labMatch[1]).sort((a, b) => b.shared_at.localeCompare(a.shared_at)).map(noteView) });
        return true;
      }
      const noteMatch = path.match(/^\/api\/v1\/notes\/([0-9a-f-]{36})\/(retire|renew)$/);
      if (noteMatch && req.method === "POST") {
        const n = notes.find((x) => x.note_id === noteMatch[1]);
        if (!n) { denied(); return true; }
        const input = await body(req);
        if (n.events.some((e) => e.actor_id === me && e.request_id === input.request_id)) { json({ note: noteView(n) }); return true; }
        const now = new Date().toISOString();
        if (n.events.some((e) => e.kind === "retired")) { noteRefused(json, "note_retired", 409); return true; }
        if (noteMatch[2] === "retire") {
          const reason = typeof input.reason === "string" ? input.reason.trim() : "";
          if (!uuid.test(input.request_id ?? "") || !reason || reason.length > 600) { noteRefused(json, "note_invalid", 400); return true; }
          n.events.push({ kind: "retired", actor_id: me, actor_name: "Example researcher", reason, review_by: null, recorded_at: now, request_id: input.request_id });
        } else {
          if (!uuid.test(input.request_id ?? "") || !Number.isInteger(input.review_days) || input.review_days < 30 || input.review_days > 730) { noteRefused(json, "note_invalid", 400); return true; }
          n.events.push({ kind: "renewed", actor_id: me, actor_name: "Example researcher", reason: null, review_by: days(input.review_days), recorded_at: now, request_id: input.request_id });
        }
        if (n.project_id === project.project_id)
          audit.push({ event_id: randomUUID(), actor_id: me, kind: noteMatch[2] === "retire" ? "note_retired" : "note_renewed", occurred_at: now, detail: { note_id: n.note_id } as never });
        json({ note: noteView(n) }); return true;
      }
      const all = [project, second, foreign, field, shared, sensors, ...made];
      const readable = (p: typeof project) => p.owner_id === me || p.visibility === "lab" || inspections.has(p.project_id);
      if (path === "/api/v1/projects" && req.method === "GET") {
        json({ projects: [...all.filter(readable).map(summary), ...(options.also?.() ?? [])].sort((a, b) => b.updated_at.localeCompare(a.updated_at)) }); return true;
      }
      if (path === "/api/v1/projects" && req.method === "POST") {
        const input = await body(req);
        // The project identifier is the request's idempotency key: the same request again returns the same project.
        if (all.some((x) => x.project_id === input.project_id)) { json({ project_id: input.project_id }); return true; }
        if (!uuid.test(input.project_id ?? "") || !input.title?.trim() || !["private", "lab"].includes(input.visibility)) { json({ error: { code: "invalid_request" } }, 400); return true; }
        const created = { project_id: input.project_id, group_key: input.group, owner_id: me, title: input.title, visibility: input.visibility, revision: 0, created_at: new Date().toISOString(), updated_at: new Date().toISOString() };
        made.push(created); json({ project_id: created.project_id }, 201); return true;
      }
      if (path === "/api/v1/admin/projects") {
        json({ projects: all.map((p) => ({ project_id: p.project_id, title: p.title, group_key: p.group_key, group_name: p.group_key === "field_trials" ? "Example Field Lab" : "Example Nutrition Lab", visibility: p.visibility,
          owner: { ...(p.owner_id === me ? { principal_id: me, display_name: "Example researcher", email: "researcher@example.invalid" } : colleague), kind: "person" },
          created_at: p.created_at, updated_at: p.updated_at, revision: p.revision, task_count: p === project ? tasks.length : 0,
          inspection_expires_at: inspections.get(p.project_id) ?? null, readable: readable(p) })) });
        return true;
      }
      const match = path.match(/^\/api\/v1\/projects\/([0-9a-f-]{36})(\/.*)?$/);
      if (match) {
        const p = all.find((x) => x.project_id === match[1]);
        const suffix = match[2] ?? "";
        if (!p) { denied(); return true; }
        if (suffix === "/inspect" && req.method === "POST") { const expires = new Date(Date.now() + 30 * 60000).toISOString(); inspections.set(p.project_id, expires); json({ expires_at: expires, capabilities: ["read"] }); return true; }
        if (suffix === "/inspect" && req.method === "DELETE") { json({ ended: inspections.delete(p.project_id) }); return true; }
        if (!readable(p)) { denied(); return true; }
        if (!suffix) { json(projectView(p)); return true; }
        // The project stream: the fingerprint on connect, then each change, read twice a second here.
        if (suffix === "/events/stream" && req.method === "GET") {
          res.setHeader("Content-Type", "text/event-stream");
          res.setHeader("Connection", "keep-alive");
          res.flushHeaders();
          let last = pulseOf(p);
          res.write(`event: project_state\ndata: ${JSON.stringify(last)}\n\n`);
          const tick = setInterval(() => {
            if (!readable(p)) { clearInterval(tick); res.end(`event: stream_closed\ndata: {"code":"project_unavailable"}\n\n`); return; }
            const next = pulseOf(p);
            if (next.pulse !== last.pulse) { last = next; res.write(`event: project_changed\ndata: ${JSON.stringify(next)}\n\n`); }
          }, 500);
          req.on("close", () => clearInterval(tick));
          return true;
        }
        const now = new Date().toISOString();
        const saveRevision = (value: Saved["state"]) => {
          p.revision += 1; p.updated_at = now;
          if (!history.has(p.project_id)) history.set(p.project_id, []);
          history.get(p.project_id)!.push({ revision: p.revision, author_id: me, created_at: now, state: value });
        };
        if (suffix === "/checkpoints" && req.method === "GET") { json({ checkpoints: [...(history.get(p.project_id) ?? [])].reverse() }); return true; }
        if (suffix === "/look" && req.method === "POST") {
          const input = await body(req);
          if (!Number.isInteger(input.revision) || input.revision < 0 || input.revision > p.revision) { json({ error: { code: "look_revision_invalid" } }, 400); return true; }
          const before = looks.get(p.project_id);
          // Forward-only: a lower revision leaves the stored look; the same or a newer one moves it to now.
          const next = before && before.revision > input.revision ? before : { revision: input.revision, at: new Date().toISOString() };
          looks.set(p.project_id, next); json(next); return true;
        }
        if (suffix === "/checkpoints" && req.method === "POST") {
          const input = await body(req);
          if (p.owner_id !== me) { denied(); return true; }
          if (!uuid.test(input.request_id ?? "") || !input.state?.goal?.trim()) { json({ error: { code: "invalid_request" } }, 400); return true; }
          if (input.expected_revision !== p.revision) { json({ error: { code: "revision_conflict" } }, 409); return true; }
          saveRevision(input.state); json({ revision: p.revision }); return true;
        }
        if (suffix === "/prepare" && req.method === "POST") {
          const input = await body(req);
          if (p.owner_id !== me) { denied(); return true; }
          // One set as `workspace_key`, or several as `workspace_keys` where the broker combines them (fixtures/starts.ts).
          const sets = options.starts.chosen(p.group_key, input);
          if (typeof sets === "string" || input.workspace !== undefined || !uuid.test(input.request_id ?? "") || !input.goal?.trim()) {
            json({ error: { code: typeof sets === "string" ? sets : "invalid_request" } }, typeof sets === "string" && sets === "workspace_unavailable" ? 404 : 400); return true;
          }
          // The request identifier makes a repeated request return its first answer.
          const again = preparedBy.get(input.request_id);
          if (again) { json(again); return true; }
          if (input.expected_revision !== p.revision) { json({ error: { code: "revision_conflict" } }, 409); return true; }
          saveRevision({ schema_version: 1, goal: input.goal, constraints: input.constraints ?? [], workspace: workspaceFor(sets, workspace), plan: [], lessons: [],
            next_action: sets.length > 1 ? `Plan across the ${sets.length} chosen sets.` : "Plan the requested analysis." });
          const answer = { revision: p.revision, allowed_profiles: allowedFor(sets) };
          preparedBy.set(input.request_id, answer); json(answer); return true;
        }
        if (suffix === "/plan-ledger" && req.method === "GET") { json(planLedgerOf(p.project_id)); return true; }
        if (suffix === "/acknowledgements" && req.method === "GET") {
          json({ acknowledgements: p === project && seen.has(`review_request:${run.run_id}:${run.run_id}`)
            ? [{ run_id: run.run_id, principal_id: me, display_name: null, seen_at: seen.get(`review_request:${run.run_id}:${run.run_id}`) }] : [] });
          return true;
        }
        if (suffix === "/working-copies" && req.method === "GET") {
          // The finished summary keeps its working copy for about a month; the failed check's was cleared.
          const day = 86_400_000;
          json({ working_copies: p === project ? [
            { task_id: firstTask, kept_until: new Date(Date.now() + 29.5 * day).toISOString(), cleared_at: null, volumes: 2 },
            { task_id: secondTask, kept_until: new Date(Date.now() - 3 * day).toISOString(), cleared_at: new Date(Date.now() - 2 * day).toISOString(), volumes: 2 },
          ] : [] });
          return true;
        }
        if (suffix === "/reviews" && req.method === "GET") {
          const own = p === project ? tasks.map((t) => t.task_id) : [];
          json({ reviews: newestFirst(reviews.filter((r) => own.includes(r.task_id))).filter((r, i, all) => all.findIndex((x) => x.task_id === r.task_id) === i) });
          return true;
        }
        if (suffix === "/notes/share" && req.method === "POST") {
          const input = await body(req);
          if (p.group_key !== "open_methods") { noteRefused(json, "note_forbidden", 403); return true; }
          const same = notes.find((n) => n.request_id === input.request_id);
          if (same) { json({ note: noteView(same) }, 201); return true; }
          if (p.visibility !== "lab") { noteRefused(json, "note_project_private", 409); return true; }
          if (input.revision !== p.revision) { noteRefused(json, "note_stale", 409); return true; }
          const saved = history.get(p.project_id)?.at(-1)?.state;
          const lesson = saved?.lessons.find((l) => l.kind === "observation" && l.statement === input.statement
            && JSON.stringify(l.evidence_refs) === JSON.stringify(input.evidence_refs));
          const keep = input.review_days ?? 180;
          const tasksOf = [...new Set((lesson?.evidence_refs ?? []).map((r) => /^artifact:\/\/tasks\/([0-9a-f-]{36})\//.exec(r)?.[1]).filter((t): t is string => !!t))];
          if (!uuid.test(input.request_id ?? "") || !lesson || !tasksOf.length || !Number.isInteger(keep) || keep < 30 || keep > 730
            || (typeof input.reason === "string" && input.reason.length > 600)) { noteRefused(json, "note_invalid", 400); return true; }
          const accepted = tasksOf.map((t) => newestFirst(reviews.filter((r) => r.task_id === t && r.reviewer_kind === "person"))[0]);
          if (accepted.some((r) => !r || r.decision !== "accepted" || !r.covers_current_outputs)) { noteRefused(json, "note_evidence_unaccepted", 409); return true; }
          if (notes.some((n) => n.project_id === p.project_id && n.statement === input.statement && !n.events.some((e) => e.kind === "retired"))) {
            noteRefused(json, "note_already_shared", 409); return true;
          }
          const now = new Date().toISOString();
          const record: NoteRecord = { note_id: randomUUID(), lab: p.group_key, project_id: p.project_id, revision: p.revision, kind: "observation",
            statement: input.statement, statement_sha256: sha(Buffer.from(input.statement)),
            evidence: tasksOf.map((t, i) => ({ task_id: t, refs: lesson!.evidence_refs.filter((r) => r.startsWith(`artifact://tasks/${t}/`)),
              title: saved!.plan.find((s) => s.task_id === t)?.title ?? null, method: tasks.find((x) => x.task_id === t)?.profile ?? "evidence_review",
              outputs: outputsOf(t), review_id: accepted[i]!.review_id })),
            methods: [...new Set(tasksOf.map((t) => tasks.find((x) => x.task_id === t)?.profile ?? "evidence_review"))].sort(),
            data: [...saved!.workspace.input_refs].sort(), shared_by: { principal_id: me, name: "Example researcher", kind: "person" },
            reason: typeof input.reason === "string" && input.reason.trim() ? input.reason.trim() : null, shared_at: now,
            review_by: days(keep), request_id: input.request_id, events: [] };
          notes.push(record);
          audit.push({ event_id: randomUUID(), actor_id: me, kind: "note_shared", occurred_at: now, detail: { note_id: record.note_id } as never });
          json({ note: noteView(record) }, 201); return true;
        }
        if (suffix === "/audit") { json({ events: p === project ? [...audit].reverse() : [] }); return true; }
        if (suffix === "/runs" && req.method === "GET") { json({ runs: runs.filter((r) => r.project_id === p.project_id) }); return true; }
        if (suffix === "/runs" && req.method === "POST") {
          const input = await body(req);
          if (p.owner_id !== me) { denied(); return true; }
          // The run identifier is the idempotency key: the same start again answers with the same run.
          if (runs.some((r) => r.run_id === input.run_id && r.project_id === p.project_id)) { json({ run_id: input.run_id }, 202); return true; }
          if (!uuid.test(input.run_id ?? "") || !Array.isArray(input.allowed_profiles) || !input.allowed_profiles.length
            || input.allowed_profiles.some((k: string) => !["evidence_review", "generated_r"].includes(k))) { json({ error: { code: "invalid_request" } }, 400); return true; }
          if (input.expected_revision !== p.revision) { json({ error: { code: "revision_conflict" } }, 409); return true; }
          if (runs.some((r) => r.project_id === p.project_id && ["queued", "running", "waiting"].includes(r.status))) { json({ error: { code: "research_run_active" } }, 409); return true; }
          runs.unshift({ ...run, run_id: input.run_id, project_id: p.project_id, initial_revision: p.revision, allowed_profiles: input.allowed_profiles,
            max_turns: input.max_turns, max_parallel: input.max_parallel, status: "running", turn: 0, generation: 1, failures: 0, code: "accepted",
            created_at: now, updated_at: now, available_at: now });
          runTurns.set(input.run_id, []); runLog.set(input.run_id, []);
          if (p === project) audit.push({ event_id: randomUUID(), actor_id: me, kind: "research_started", occurred_at: now, detail: { run_id: input.run_id } });
          json({ run_id: input.run_id }, 202); return true;
        }
        if (suffix === "/workspaces") { json(options.starts.answer(p.group_key)); return true; }
        if (suffix === "/members") {
          json({ owner: { principal_id: me, display_name: "Example researcher", email: "researcher@example.invalid" },
            grants: [colleague, other].filter((m) => grants.get(m.principal_id)?.length).map((m) => ({ ...m, kind: "person", enabled: true, capabilities: grants.get(m.principal_id) })),
            eligible: [colleague, other].filter((m) => !grants.get(m.principal_id)?.length) });
          return true;
        }
        if (suffix === "/grants" && req.method === "POST") {
          const input = await body(req); grants.set(input.principal_id, input.capabilities);
          audit.push({ event_id: randomUUID(), actor_id: me, kind: "access_changed", occurred_at: new Date().toISOString(), detail: { member_id: input.principal_id, capabilities: input.capabilities } });
          res.statusCode = 204; res.end(); return true;
        }
        if (suffix === "/package/preview") {
          json({ root: "research-evidence-test-diet-response-study", snapshot_at: new Date().toISOString(), files: 24, uncompressed_bytes: pkg.archive.length + 40000,
            counts: { plan_revisions: 4, research_runs: 1, agent_decisions: turns.length, agent_deferrals: runEvents.length, access_events: audit.length, tasks: tasks.length, artifacts_included: 4, artifacts_missing: 0,
              artifact_bytes_included: pkg.archive.length + pkg.checksum.length + generated.archive.length + generated.checksum.length, generated_code_blocks: 1,
              rerun_analyses: 2 },
            coverage: { database_records: "complete", artifact_bytes: "complete", scientific_validation: "not_performed" }, missing: [],
            // The shape the broker's hpc_handoff writes; the steps themselves are tested in the broker.
            hpc_handoff: { checked: "2026-10-04", sources: [{ title: "SCINet: Open OnDemand", url: "https://scinet.usda.gov/guides/use/open-ondemand", updated: "2025-01-29" }],
              sites: [["atlas", "Atlas", "https://atlas-ood.hpc.msstate.edu/"], ["ceres", "Ceres", "https://ceres-ondemand.scinet.usda.gov/"]].map(([key, name, url]) => ({ key, name, open_ondemand: url,
                steps: [{ text: `Sign in to ${name} Open OnDemand at ${url} with your SCINet account.` },
                  { text: "Open a shell and unpack the package in your project's folder.", command: "cd /project/YOUR_PROJECT && tar -xzf research-evidence-test-diet-response-study.tar.gz && cd research-evidence-test-diet-response-study" },
                  { text: "Submit the rerun with your Slurm account.", command: "sbatch -A YOUR_PROJECT rerun/slurm/rerun.sbatch" }] })) } });
          return true;
        }
        if (suffix === "/package") {
          const readme = Buffer.from("# Test diet response study\n\nSimulated evidence package for the workspace preview.\n");
          const archive = gzipSync(tar([{ path: "research-evidence-test-diet-response-study/README.md", bytes: readme },
            { path: "research-evidence-test-diet-response-study/tasks/files/payload.tar.gz", bytes: pkg.archive }], Math.floor(Date.now() / 1000)));
          res.setHeader("Content-Type", "application/gzip"); res.setHeader("Content-Length", archive.length);
          res.setHeader("X-Artifact-Sha256", sha(archive)); res.setHeader("X-Evidence-Manifest-Sha256", "e".repeat(64));
          res.setHeader("Content-Disposition", 'attachment; filename="research-evidence-test-diet-response-study.tar.gz"');
          audit.push({ event_id: randomUUID(), actor_id: me, kind: "evidence_package_exported", occurred_at: new Date().toISOString(), detail: { bytes: archive.length } as never });
          res.end(archive); return true;
        }
        denied(); return true;
      }
      const keptMatch = path.match(/^\/api\/v1\/research-runs\/([0-9a-f-]{36})\/contexts\/(sha256:[0-9a-f]{64})$/);
      if (keptMatch) {
        const named = (runTurns.get(keptMatch[1]) ?? []).some((t) => [t.record.lead, (t.record as { methods_specialist?: typeof t.record.lead }).methods_specialist]
          .some((r) => r && (r.context_sha256 === keptMatch[2] || r.system_sha256 === keptMatch[2])));
        const text = KEPT.get(keptMatch[2]);
        if (!named || text === undefined) { json({ error: { code: "context_not_found" } }, 404); return true; }
        json({ sha256: keptMatch[2], bytes: Buffer.byteLength(text), text }); return true;
      }
      // The first run's orientation: synthetic, as the coordinator keeps it before the first plan.
      if (path === `/api/v1/research-runs/${run.run_id}/orientation` && req.method === "GET") {
        json({ status: "read", label: "Written by the research lead before this run's first plan, from the same context; keys and refs checked; not results.",
          step_keys: ["summarize-diets"], orientation: {
            question: "Does plasma response differ across the three test diets?",
            sources: [], tools: [{ key: "evidence_review", why: "Summarizes each diet's retained measurements." }],
            papers: [{ ref: `library://${PAPER_2}/${CHUNK[2]}`, establishes: "How the samples were handled before measurement." }],
            earlier: [{ ref: "knowledge://synthetic-earlier-run", establishes: "The lab compared two of these diets in an earlier project." }],
            unknowns: ["Whether every participant has a sample for each diet."],
            profiles: [{ action_key: "summarize-diets", title: "Summarize each diet and test feature changes", profile: "evidence_review", rationale: "Shows which diets have retained samples." }] } });
        return true;
      }
      const runMatch = path.match(/^\/api\/v1\/research-runs\/([0-9a-f-]{36})\/(turns|events|pause)$/);
      if (runMatch) {
        const found = runs.find((r) => r.run_id === runMatch[1]);
        if (!found) { denied(); return true; }
        const after = Number(url.searchParams.get("after") ?? 0);
        if (runMatch[2] === "turns") json({ turns: (runTurns.get(found.run_id) ?? []).filter((t) => t.turn > after) });
        else if (runMatch[2] === "events") json({ events: (runLog.get(found.run_id) ?? []).filter((e) => e.sequence > after) });
        else {
          if (req.method !== "POST") { denied(); return true; }
          found.status = "paused"; found.code = "user_paused"; found.updated_at = new Date().toISOString();
          if (found.project_id === project.project_id) audit.push({ event_id: randomUUID(), actor_id: me, kind: "research_paused", occurred_at: found.updated_at, detail: { run_id: found.run_id } });
          res.statusCode = 204; res.end();
        }
        return true;
      }
      const contents = path.match(/^\/api\/v1\/tasks\/([0-9a-f-]{36})\/artifacts\/([0-9a-f-]{36})\/(contents(?:\/file)?|download|pages|summary)$/);
      const retainedFor = contents?.[1] === firstTask ? { artifacts: [packageArtifact, checksumArtifact], pkg }
        : contents?.[1] === generatedTask ? { artifacts: generatedArtifacts, pkg: generated } : null;
      if (contents && retainedFor && retainedFor.artifacts.some((a) => a.id === contents[2])) {
        const [packaged, checksum] = retainedFor.artifacts;
        const artifact = contents[2] === packaged.id ? packaged : checksum;
        const source = retainedFor.pkg;
        if (contents[3] === "pages" || contents[3] === "summary") {
          answerTable(contents[3], url, artifact === packaged ? source.files : null, { path: artifact.relative_path, bytes: source.checksum }, json); return true;
        }
        if (contents[3] === "contents") { if (artifact !== packaged) { json({ error: { code: "artifact_not_a_package" } }, 415); return true; } json({ artifact_id: artifact.id, sha256: artifact.sha256, size_bytes: artifact.size_bytes, listing: listing(source.files) }); return true; }
        if (contents[3] === "download") {
          const bytes = artifact === packaged ? source.archive : source.checksum;
          res.setHeader("Content-Type", artifact.media_type); res.setHeader("Content-Length", bytes.length); res.setHeader("X-Artifact-Sha256", artifact.sha256);
          res.end(bytes); return true;
        }
        const file = source.files.find((f) => f.path === url.searchParams.get("path"));
        if (!file) { denied(); return true; }
        res.setHeader("Content-Type", "application/octet-stream"); res.setHeader("Content-Length", file.bytes.length); res.setHeader("X-Artifact-Sha256", sha(file.bytes));
        res.end(file.bytes); return true;
      }
      return false;
    },
  };
}
