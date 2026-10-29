import type { InputClass, Lineage, LineageGap, StepLineage, TaskReview } from "./contracts.ts";
import { bytes, fileTitle, stepStatus, when } from "./language.ts";
import { byPerson, decisionWords, notAPerson, reviewerName } from "./person_review.ts";
import { lineageCoverageNotice } from "./step_lineage.ts";

// "Where this came from" as plain rows, in the order a methods note reads. Every row the
// broker's records cannot fill says so in words; nothing is filled in by guessing. Identifiers,
// digests and resource figures go in `details`, which the page keeps under Technical details.
export interface LineageRow {
  key: "asked" | "step" | "data" | "method" | "code" | "query" | "ran" | "results" | "review" | "other";
  label: string;
  text: string;
  /** Plain sentences that qualify the text. */
  notes: string[];
  /** Identifiers, digests and limits, shown only under Technical details. */
  details: string[];
  gaps: string[];
}

export const gapSentence: Record<LineageGap, string> = {
  origin_not_recorded: "Who asked for this analysis was not recorded.",
  origin_before_records:
    "It ran before the broker recorded who asked for each analysis, and no work session's record lists it.",
  not_in_project: "It is not part of a project, so no project recorded its data selection.",
  plan_step_not_recorded: "No step in the project plan names this analysis.",
  inputs_not_recorded: "The project's data selection at that time was not recorded.",
  input_class_not_recorded:
    "What kind of data this is (a lab database, files from lab storage, a paper or public reference data) was not recorded.",
  image_not_digest_pinned: "The sandbox image was not pinned to an exact build.",
  no_outputs_recorded: "No result files were recorded.",
  code_not_recorded: "No admitted code was recorded for this analysis, so it ran no model-written code.",
  method_recorded_later:
    "Its method was recorded after it ran, from the catalog's dated records; the method's name is the one in use then.",
  method_not_recorded: "The method this analysis was submitted under was not recorded.",
};

const short = (digest: string | null | undefined) => (digest ?? "").replace(/^sha256:/, "").slice(0, 12);
const named = (text: string) => text.replace(/[.\s]+$/, "");

// "template://r-generated-analysis/v2" reads as "r-generated-analysis v2".
export function templateName(ref: string | null): string {
  const path = (ref ?? "").replace(/^template:\/\//, "");
  const parts = path.split("/");
  return parts.length > 1 ? `${parts.slice(0, -1).join("/")} ${parts.at(-1)}` : path;
}

// "registry.example/release/agent-sandbox/site@sha256:abc…" reads as "agent-sandbox/site, build abc…".
export function imageName(ref: string | null): string {
  if (!ref) return "";
  const [name, digest] = ref.split("@");
  // The last two path parts name the image; Docker Hub's "library" prefix is dropped.
  const parts = name.replace(/:[^:/]*$/, "").split("/").slice(1).filter((part) => part !== "library");
  const base = parts.slice(-2).join("/") || name;
  return digest ? `${base}, build ${short(digest)}` : base;
}

function account(lineage: Lineage | StepLineage, reader: string, possessive: boolean): string {
  const o = lineage.origin;
  if (o.principal_id === reader) return possessive ? "your" : "you";
  const name = o.principal_name?.trim();
  if (o.principal_kind === "service_account") return name ? `the service account ${name}${possessive ? "'s" : ""}` : `a service account${possessive ? "'s" : ""}`;
  return name ? `${name}${possessive ? "'s" : ""}` : `a colleague${possessive ? "'s" : ""}`;
}

const capital = (text: string) => text.charAt(0).toUpperCase() + text.slice(1);

const classWords: Record<InputClass, string> = {
  public_reference: "public reference data",
  lab_database: "a lab database",
  object_data: "files from lab storage",
  paper: "a paper",
  prior_output: "an earlier result",
};

// ["object_data", "paper"] reads as "files from lab storage and a paper"; a class this page does not know is named as recorded.
function kinds(classes: readonly string[]): string {
  const words = classes.map((c) => classWords[c as InputClass] ?? c.replaceAll("_", " "));
  return words.length < 3 ? words.join(" and ") : `${words.slice(0, -1).join(", ")} and ${words.at(-1)}`;
}

// `reviews` are the analysis's recorded reviews, newest first, read separately from the lineage.
export function describeLineage(lineage: Lineage | StepLineage, reader: string, reviews: TaskReview[] = []): LineageRow[] {
  const gaps = new Set<string>(lineage.gaps);
  const noted = (...codes: LineageGap[]) => codes.filter((code) => gaps.has(code)).map((code) => gapSentence[code]);
  const rows: LineageRow[] = [];
  const o = lineage.origin;

  if (o.origin === "research_run") {
    const starter = account(lineage, reader, false);
    rows.push({ key: "asked", label: "Asked for by",
      text: `The research agent, in decision ${o.turn} of a work session ${starter} started.`,
      notes: o.basis === "turn_record" ? ["Recorded later from the run's own record of that turn."] : [], details: [], gaps: [] });
  } else if (o.origin === "person") {
    rows.push({ key: "asked", label: "Asked for by", text: `${capital(account(lineage, reader, false))}.`,
      notes: o.basis === "standalone_task" ? ["Recorded later: an analysis outside a project can be requested only by its own account."] : [],
      details: [], gaps: [] });
  } else {
    rows.push({ key: "asked", label: "Asked for by", text: "Not recorded.",
      notes: [`It runs under ${account(lineage, reader, true)} account.`], details: [],
      gaps: noted("origin_before_records", "origin_not_recorded") });
  }

  // A figure version, a person's template analysis and lab data (v10) are requests
  // made from an earlier result or a sent file, not steps of the plan; a template analysis the research lead chose
  // keeps its plan step.
  const r = lineage.render;
  const a = lineage.analysis ?? null;
  const d = lineage.dataset ?? null;
  if (lineage.task.project_id && !r && !d && !(a && !lineage.plan_step)) {
    const step = lineage.plan_step;
    if (step) {
      const notes = [stepStatus(step.status).label];
      if (step.added_after_submission) notes.push("Added to the plan after the analysis was submitted.");
      if (step.current) notes.push(`The plan was revised later to read: ${named(step.current.title)}.`);
      const details = [step.revision ? `First recorded in saved version ${step.revision}` : "",
        step.current ? `Revised in saved version ${step.current.revision}` : ""].filter(Boolean);
      rows.push({ key: "step", label: "Plan step", text: `${named(step.title)}.`, notes, details, gaps: [] });
    } else {
      rows.push({ key: "step", label: "Plan step", text: "Not in the project plan.", notes: [], details: [], gaps: noted("plan_step_not_recorded") });
    }
  }

  const workspace = lineage.inputs.workspace;
  const tool = lineage.inputs.platform_tool;
  const classes = lineage.inputs.classes ?? [];
  const q = lineage.query;
  const asked = r ?? a;
  if (asked) {
    // A table under results/lab_data or results/lab_derived holds the lab's own values.
    const lab = /^results\/lab_(data|derived)\//.test(asked.table_path);
    rows.push({ key: "data", label: "Data",
      text: `${fileTitle(asked.table_path)}, ${lab ? "a table of the lab's own data" : "a table from an earlier analysis"} in this project.`,
      notes: ["Read from that analysis's retained package and checked against its checksum."],
      details: [`Table ${short(asked.table_sha256)}`, `Package ${short(asked.source_package_sha256)}`],
      gaps: noted("input_class_not_recorded") });
  } else if (d) {
    rows.push({ key: "data", label: "Data",
      text: d.operation === "intake" ? `${d.file_name}, a file sent to this project.` : `${d.file_name}, as the upload guard checked it.`,
      notes: [], details: [`File ${short(d.file_sha256)}`, d.grid_sha256 ? `Cell grid ${short(d.grid_sha256)}` : "",
        d.spec_sha256 ? `Preparation ${short(d.spec_sha256)}` : ""].filter(Boolean),
      gaps: [] });
  } else if (q?.title) {
    // A query reads one registered source, whatever the project had selected; the source states its kind of data.
    rows.push({ key: "data", label: "Data",
      text: `${named(q.title)}. ${capital(kinds(q.data_class ? [q.data_class] : classes) || "data")} read from one pinned snapshot.`,
      notes: q.release ? [`${named(q.release)}.`] : [],
      details: [q.snapshot_sha256 ? `Snapshot ${short(q.snapshot_sha256)}` : "", `Source ${q.source}`].filter(Boolean),
      gaps: noted("input_class_not_recorded") });
  } else if (workspace || tool) {
    const refs = workspace?.input_refs ?? [];
    const details = refs.map((ref) => `Input ${ref}`);
    if (workspace?.source_digest) details.push(`Source copy ${short(workspace.source_digest)}`);
    if (tool) details.push(`Tool input ${short(tool.input_sha256)}`);
    const sealed = lineage.inputs.classes_recorded_by === "input_custodian" && classes.length
      ? [`${capital(kinds(classes))}, recorded when the platform sealed ${refs.length > 1 ? "the copies" : "the copy"} this analysis read.`]
      : [];
    rows.push({ key: "data", label: "Data",
      text: refs.length ? `${refs.length === 1 ? "The input" : `The ${refs.length} inputs`} the project had selected when it was submitted.` : "The tool's recorded input.",
      notes: sealed, details, gaps: noted("input_class_not_recorded") });
  } else {
    rows.push({ key: "data", label: "Data", text: "Not recorded.", notes: [], details: [], gaps: noted("not_in_project", "inputs_not_recorded") });
  }

  const m = lineage.method;
  const profile = m.profile_name ?? m.profile;
  // The record names a template only through admitted code or the method the task was submitted under.
  const how = {
    agent_built: m.template_ref
      ? `model-written R code under the reviewed template ${templateName(m.template_ref)}`
      : "model-written R code under a reviewed template",
    vetted_service: "a reviewed service tool",
    configured: "a configured recipe run with settings the agent chose; the agent wrote no code",
    reviewed_query: "a reviewed database query with typed parameters, checked before it ran; results await validation",
    guarded_query: "a read-only database query the agent wrote, checked before it ran; no code ran",
    agent_session: "a research agent working with this method's tools",
  }[m.tool_class] ?? "a kind of method this page does not describe yet";
  const drawn = r ? "the reviewed R render service drew it with the chosen settings; no code was written for it"
    : a ? `run by a reviewed R analysis template (${a.feature.replaceAll("_", " ")}) with the chosen settings; no code was written for it`
    : d ? "checked by the upload guard and read by the broker's own preparation code; no model touched the values" : "";
  const methodDetails = [tool ? `Tool ${tool.tool_id}, catalog ${short(tool.catalog_digest)}` : "",
    (r ?? a)?.service_version ? `Render service ${(r ?? a)!.service_version}` : "",
    d?.executor ? `${d.executor}${d.executor_version ? ` version ${d.executor_version}` : ""}` : "",
    m.template_sha256 ? `Template ${short(m.template_sha256)}` : ""].filter(Boolean);
  rows.push({ key: "method", label: "Method", text: `${profile}: ${drawn || how}.`,
    // The template's own statement of what ran, such as "Welch two-sample t-test".
    notes: a?.method?.trim() ? [`${named(a.method)}.`] : [], details: methodDetails,
    gaps: noted("code_not_recorded", "method_recorded_later", "method_not_recorded") });

  if (lineage.code) {
    const allowlist = lineage.code.admission.allowlist;
    const settings = lineage.code.configuration?.described.map((s) => `${s.label}: ${s.choice_label}`) ?? [];
    rows.push({ key: "code", label: "Code",
      text: lineage.code.configuration
        ? `The configured recipe with its settings filled in (${bytes(lineage.code.size_bytes)}), checked by the platform before it ran.`
        : `Model-written code (${bytes(lineage.code.size_bytes)}), checked by the platform before it ran.`,
      notes: settings, details: [`Code ${short(lineage.code.sha256)}`, allowlist ? `Allowed functions list ${short(allowlist)}` : ""].filter(Boolean),
      gaps: [] });
  }

  if (lineage.query) {
    const q = lineage.query;
    rows.push({ key: "query", label: "Query",
      text: `A read-only query of ${q.relations.length === 1 ? "one table" : `${q.relations.length} tables`}, at most ${q.row_limit} rows, checked by the platform before it ran.`,
      notes: [], details: [`Query ${short(q.sha256)}`, `Tables ${q.relations.join(", ")}`], gaps: [] });
  }

  const e = lineage.execution;
  const limits = [e.cpu_limit && `${e.cpu_limit} CPU`, e.memory_limit && `${e.memory_limit} memory`,
    e.max_run_seconds && `up to ${Math.round(e.max_run_seconds / 60)} minutes`].filter(Boolean).join(", ");
  // A figure version and a template analysis ran in the render service; its receipt names the release and the exact
  // image.
  const rs = r ?? a;
  const service = rs ? `The reviewed R render service${rs.service_version ? `, release ${rs.service_version}` : ""}` : "";
  rows.push({ key: "ran", label: "Ran in",
    text: rs ? (rs.image_digest ? `${service}, pinned to an exact build.` : `${service}.`)
      : !e.image_ref ? "Not recorded." : gaps.has("image_not_digest_pinned") ? "A recorded environment." : "A recorded environment, pinned to an exact build.",
    notes: [`${e.attempts} ${e.attempts === 1 ? "attempt" : "attempts"}`],
    details: [rs ? (rs.image_digest ? `Image ${short(rs.image_digest)}` : "") : e.image_ref ? `Image ${imageName(e.image_ref)}` : "", rs ? "" : limits].filter(Boolean),
    // The render service names the build it ran in its receipt; it has no sandbox image.
    gaps: rs && gaps.has("image_not_digest_pinned") ? [r ? "The render service did not report the exact build that drew this figure."
      : "The render service did not report the exact build that ran this analysis."]
      : noted("image_not_digest_pinned") });

  const n = lineage.outputs.length;
  const madeBy = {
    agent_built_tool_output: "Made by model-written code.",
    vetted_tool_output: r ? "Drawn by the reviewed R render service." : a ? "Made by a reviewed R analysis template."
      : d ? "Made by the broker's own preparation code." : "Made by a reviewed service tool.",
    configured_tool_output: "Made by a configured recipe with settings the agent chose.",
    query_output: "Made by a read-only database query.",
    agent_session_output: "Made by the research agent's work.",
  }[lineage.outputs[0]?.provenance_class ?? ""] ?? "";
  // A figure version that was not drawn has no files by design; the figure's own record says why.
  const undrawn = !!r && !n && ["failed", "cancelled"].includes(lineage.task.status);
  rows.push({ key: "results", label: "Results",
    text: n ? `${n} ${n === 1 ? "file" : "files"}, each with a recorded checksum.` : gaps.has("no_outputs_recorded") ? "None recorded."
      : undrawn ? "None: the figure was not drawn." : "Not finished yet.",
    notes: n && madeBy ? [madeBy] : [], details: [], gaps: noted("no_outputs_recorded") });

  const newest = reviews[0];
  const earlier = reviews.length - 1;
  rows.push({ key: "review", label: "Review",
    text: newest ? `${decisionWords[newest.decision].label} by ${reviewerName(newest, reader)} on ${when(newest.created_at)}.`
      : undrawn ? "Nothing to review: the figure was not drawn."
      : d ? "Not reviewed as an analysis: the lab's data is read as the person approved, and its receipt counts every change."
      : !n ? "Nothing to review: no results were recorded."
      : lineage.review.state === "unvalidated" ? "Unvalidated. No one has reviewed this result yet." : "Not yet reviewed by a person.",
    notes: newest ? [
      ...(byPerson(newest) ? [] : [notAPerson]),
      ...(newest.covers_current_outputs ? [] : ["The result files changed after this review."]),
      ...(earlier ? [`${earlier} earlier ${earlier === 1 ? "review" : "reviews"}.`] : []),
      lineage.review.state === "unvalidated" ? "The results stay unvalidated: a review is a person's reading, not statistical validation."
        : "A review is a person's reading, not statistical validation.",
    ] : [],
    details: [], gaps: [] });

  // A gap code this page does not know still counts, so it is listed rather than dropped.
  const unknown = lineage.gaps.filter((code) => !(code in gapSentence));
  if (unknown.length)
    rows.push({ key: "other", label: "Also not recorded", text: "The broker named parts of this record that this page does not describe yet.",
      notes: [], details: unknown, gaps: unknown.map(() => "Not recorded.") });
  return rows;
}

// Who asked, as a short label: the reader, a named account, a work session's decision or "Not recorded".
export function originLabel(origin: Lineage["origin"], reader: string): { label: string; detail: string } {
  if (origin.origin === "research_run")
    return { label: `A work session, decision ${origin.turn}`, detail: origin.basis === "turn_record"
      ? "Recorded later from the run's own record of that turn." : "Recorded when the run submitted it." };
  if (origin.origin === "unrecorded")
    return { label: "Not recorded", detail: origin.basis === "before_origin_records"
      ? gapSentence.origin_before_records : gapSentence.origin_not_recorded };
  const name = origin.principal_name?.trim();
  const label = origin.principal_id === reader ? "You"
    : origin.principal_kind === "service_account" ? (name ? `The service account ${name}` : "A service account")
    : name || "A colleague";
  return { label, detail: origin.basis === "standalone_task"
    ? "Recorded later: an analysis outside a project can be requested only by its own account." : "Recorded when it was submitted." };
}

export function lineageSummary(lineage: Lineage | StepLineage): string {
  if (lineage.schema !== "enrichedlab.task-lineage.v3") {
    const n = lineage.gaps.length;
    if (n === 0) return "Every part of this analysis's history is recorded.";
    return `${n === 1 ? "One part" : `${n} parts`} of this analysis's history ${n === 1 ? "is" : "are"} not recorded. ${n === 1 ? "It is" : "Each is"} marked in the margin.`;
  }

  // The root node repeats version 2's root record, so count its top-level and attempt gaps only once.
  const upstream = lineage.upstream.nodes.filter((node) => node.task_id !== lineage.task.task_id);
  const n = lineage.gaps.length
    + lineage.attempts.reduce((count, attempt) => count + attempt.gaps.length, 0)
    + upstream.reduce((count, node) => count + node.gaps.length
      + node.attempts.reduce((attemptCount, attempt) => attemptCount + attempt.gaps.length, 0), 0);
  const missing = n === 0 ? "" : `${n === 1 ? "One part" : `${n} parts`} of this analysis's history ${n === 1 ? "is" : "are"} not recorded. ${n === 1 ? "It is" : "Each is"} named where that step is shown.`;
  const coverage = lineageCoverageNotice(lineage) ?? "";
  return [missing, coverage].filter(Boolean).join(" ")
    || "Every part of this analysis's history is recorded.";
}
