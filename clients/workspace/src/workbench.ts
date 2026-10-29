// The workbench's view of a project: one tree of everything the project holds, the trace from any file back to the
// question, and the problems a scientist should look at. Pure functions over broker records; nothing here fetches.
import { isPackage, type Identity, type Lineage, type TaskReview } from "./contracts.ts";
import { labOrder, operatorLab } from "./context.ts";
import type { TaskEvidence } from "./evidence.ts";
import type { AuditRecord } from "./history.ts";
import { originLabel } from "./lineage.ts";
import { byPerson } from "./person_review.ts";
import type { IndexFacts } from "./standing.ts";
import { bytes, deferralLabel, fileTitle, isModelRetry, stepStatus, taskStatus, when, type Tone } from "./language.ts";
import { taskTitle, type Checkpoint, type Project, type ProjectMembers, type ProjectSummary, type ResearchRun, type ResearchTurn, type RunEvent } from "./projects.ts";

export interface ProjectModel {
  project: Project;
  runs: { run: ResearchRun; turns: ResearchTurn[]; events: RunEvent[] }[];
  checkpoints: Checkpoint[];
  audit: AuditRecord[];
  members: ProjectMembers | null;
  evidence: Record<string, TaskEvidence>;
  // Analyses whose retained files could not be read on the last load; absent when every read worked.
  evidenceFailed?: string[];
  // The newest person's review of each analysis; absent when they could not be read.
  reviews?: Record<string, TaskReview>;
}

export type NodeKind =
  | "lab" | "project" | "question" | "runs" | "run" | "turn" | "analyses" | "analysis" | "planned" | "folder" | "file"
  | "steps" | "code" | "record" | "people" | "package" | "data" | "files";

/** Where a project's row sits in the explorer (Glass Box, Detailed view): the question and its answer, the steps in plan
 *  order, analyses outside the plan, then the record. */
export type Group = "question" | "steps" | "also" | "lab" | "record";

export interface WbNode {
  id: string;
  kind: NodeKind;
  label: string;
  detail?: string;
  tone?: Tone;
  children?: WbNode[];
  projectId?: string;
  taskId?: string;
  runId?: string;
  turn?: number;
  artifactId?: string;
  memberPath?: string;
  // A figure kept as an R PDF with a lighter SVG copy is one row: memberPath is the copy the page draws and
  // originalPath the PDF, which the figure's own Download PDF saves and search finds by name.
  originalPath?: string;
  group?: Group;
  /** The plan step's number, for a step row. */
  step?: number;
  /** Recorded after the reader's last look at the project. */
  fresh?: boolean;
  /** The formats one row holds, such as "PDF, SVG" for a figure R saved twice. */
  formats?: string;
}

// Identifiers are paths of record IDs, so a node keeps its place when the tree reloads.
export const ids = {
  lab: (group: string) => `lab:${group}`,
  project: (p: string) => `p:${p}`,
  question: (p: string) => `p:${p}/question`,
  runs: (p: string) => `p:${p}/runs`,
  run: (p: string, r: string) => `p:${p}/r:${r}`,
  turn: (p: string, r: string, n: number) => `p:${p}/r:${r}/t:${n}`,
  analyses: (p: string) => `p:${p}/analyses`,
  analysis: (p: string, t: string) => `p:${p}/a:${t}`,
  folder: (p: string, t: string, name: string) => `p:${p}/a:${t}/d:${name}`,
  file: (p: string, t: string, artifact: string, member?: string) =>
    member ? `p:${p}/a:${t}/f:${artifact}/${member}` : `p:${p}/a:${t}/f:${artifact}`,
  steps: (p: string, t: string) => `p:${p}/a:${t}/steps`,
  code: (p: string, t: string) => `p:${p}/a:${t}/code`,
  record: (p: string) => `p:${p}/record`,
  people: (p: string) => `p:${p}/people`,
  package: (p: string) => `p:${p}/package`,
  planned: (p: string, key: string) => `p:${p}/s:${key}`,
  data: (p: string) => `p:${p}/data`,
  files: (p: string) => `p:${p}/files`,
};

// Labs as the reader's research groups, each with the projects the broker lets them read.
export function labTree(list: ProjectSummary[], groups: Identity["groups"]): WbNode[] {
  const names = new Map(groups.map((g) => [g.key, g.name]));
  const byGroup = new Map<string, ProjectSummary[]>();
  for (const project of list) byGroup.set(project.group_key, [...(byGroup.get(project.group_key) ?? []), project]);
  return [...byGroup.entries()]
    .sort(([a], [b]) => (names.get(a) ?? a).localeCompare(names.get(b) ?? b))
    .map(([group, projects]) => ({
      id: ids.lab(group), kind: "lab", label: names.get(group) ?? "Research group",
      detail: `${projects.length} project${projects.length === 1 ? "" : "s"}`,
      children: [...projects].sort((a, b) => b.updated_at.localeCompare(a.updated_at)).map((p) => ({
        id: ids.project(p.project_id), kind: "project" as const, label: p.title, projectId: p.project_id,
        detail: p.visibility === "lab" ? "Shared with the lab" : "Private",
      })),
    }));
}

const FOLDERS: [keyof TaskEvidence["packages"][number]["view"], string][] = [
  ["figures", "Figures"], ["tables", "Tables"], ["code", "Code"], ["inputs", "Data used"], ["notes", "Method notes"],
  ["parameters", "Parameters"], ["environment", "Environment"], ["logs", "Logs"], ["other", "Other files"],
];

/** A project's task by id, an analysis or a lab file's reading or preparation. */
export const taskOf = (model: ProjectModel, taskId: string) =>
  model.project.tasks.find((t) => t.task_id === taskId) ?? model.project.lab_data?.find((t) => t.task_id === taskId);
/** The lab's own files read and the tables prepared from them, oldest first: listed apart from the analyses. */
export const labTasks = (model: ProjectModel) => [...(model.project.lab_data ?? [])].sort((a, b) => a.submitted_at.localeCompare(b.submitted_at));
/** A lab data task's name in the explorer: the reading of a file, or a table prepared from it with its title. */
export function labLabel(task: Project["tasks"][number]): string {
  const named = task.dataset?.title?.trim();
  if (task.dataset?.operation === "intake") return named ? `Lab file read: ${named}` : "Lab file read";
  return named ? `Prepared table: ${named}` : "Prepared table";
}

export function stepTitleFor(model: ProjectModel, profiles: Identity["profiles"]) {
  return (taskId: string) => {
    const task = taskOf(model, taskId);
    if (task?.dataset) return labLabel(task);
    return model.project.checkpoint?.state.plan.find((s) => s.task_id === taskId)?.title
      ?? taskTitle(model.project, taskId)
      ?? profiles.find((p) => p.key === task?.profile)?.name
      ?? "Analysis";
  };
}

const ENDED = ["completed", "review_required", "failed", "cancelled"];
/** Whether an analysis ended after the reader's last look; nothing is new before a first look, when all of it would be. */
export const endedSince = (task: { status: string; updated_at: string }, since: string | null | undefined) =>
  !!since && ENDED.includes(task.status) && Date.parse(task.updated_at) > Date.parse(since);

// One analysis as an explorer row: its files by kind (one row per figure, however many formats R saved), its retained
// packages, its recorded steps and any model-written code.
function analysisNode(model: ProjectModel, task: Project["tasks"][number], label: string, group: Group, fresh: boolean, step?: number): WbNode {
  const p = model.project.project_id, t = task.task_id;
  const status = taskStatus(task.status);
  const children: WbNode[] = [];
  const evidence = model.evidence[t];
  for (const pkg of evidence?.packages ?? []) {
    const mismatched = (path: string | undefined) => !!path && pkg.contents.listing.members.find((x) => x.path === path)?.manifest === "mismatched";
    for (const [key, name] of FOLDERS) {
      // One row per figure, however many formats R saved it in (a PDF and its SVG copy
      // showed as two rows with one name).
      const members = key === "figures"
        ? pkg.view.figures.map((f) => ({ path: f.display.path, original: f.original?.path }))
        : ((pkg.view[key] as { path: string }[] | undefined) ?? []).map((m) => ({ path: m.path, original: undefined as string | undefined }));
      if (!members.length) continue;
      children.push({ id: ids.folder(p, t, `${pkg.artifact.id}:${key}`), kind: "folder", label: name, projectId: p, taskId: t,
        detail: `${members.length}`,
        children: members.map((m) => ({
          id: ids.file(p, t, pkg.artifact.id, m.path), kind: "file" as const, projectId: p, taskId: t,
          artifactId: pkg.artifact.id, memberPath: m.path, originalPath: m.original, label: fileTitle(m.path), detail: m.path.split("/").at(-1),
          formats: m.original ? formatsOf([m.original, m.path]) : undefined, fresh: fresh || undefined,
          tone: mismatched(m.path) || mismatched(m.original) ? "failed" as Tone : undefined,
        })) });
    }
  }
  for (const artifact of evidence?.artifacts ?? [])
    children.push({ id: ids.file(p, t, artifact.id), kind: "file", projectId: p, taskId: t, artifactId: artifact.id,
      label: artifact.label, detail: isPackage(artifact) ? "Complete analysis package" : artifact.relative_path.split("/").at(-1) });
  children.push({ id: ids.steps(p, t), kind: "steps", label: "Recorded steps", projectId: p, taskId: t });
  if (evidence?.packages.some((pkg) => pkg.view.template))
    children.push({ id: ids.code(p, t), kind: "code", label: "Code the agent wrote, and its template", projectId: p, taskId: t, tone: "attention",
      detail: "Model-written or a configured recipe; unvalidated until read" });
  return { id: ids.analysis(p, t), kind: "analysis", projectId: p, taskId: t, label, group, step, fresh: fresh || undefined,
    detail: status.label, tone: status.tone === "quiet" ? undefined : status.tone, children };
}
const formatsOf = (paths: string[]) => paths.map((x) => x.split(".").at(-1)?.toUpperCase() ?? "").filter(Boolean).join(", ");

/** A project's explorer rows (Glass Box, Detailed view): the question and its answer; the plan's steps in order, each
 *  with its files by kind; analyses outside the plan; then the record (data used, every file, the work sessions, people
 *  and the evidence package). `since` is the reader's last look, for New marks. */
export function projectChildren(model: ProjectModel, profiles: Identity["profiles"], since: string | null = null): WbNode[] {
  const p = model.project.project_id;
  const title = stepTitleFor(model, profiles);
  const plan = model.project.checkpoint?.state.plan ?? [];
  const out: WbNode[] = [{ id: ids.question(p), kind: "question", label: "The question and the answer", projectId: p, group: "question",
    detail: model.project.checkpoint ? `Saved version ${model.project.checkpoint.revision}` : "No question saved yet" }];

  const byId = new Map(model.project.tasks.map((t) => [t.task_id, t]));
  const inPlan = new Set<string>();
  plan.forEach((step, i) => {
    const n = i + 1;
    const task = step.task_id ? byId.get(step.task_id) : undefined;
    if (task) {
      inPlan.add(task.task_id);
      const node = analysisNode(model, task, `${n} · ${step.title}`, "steps", endedSince(task, since), n);
      // A step whose result files could not be confirmed is marked for a check, whatever its analysis reported.
      out.push(step.status === "uncertain" ? { ...node, tone: "attention", detail: "Needs a check" } : node);
    } else {
      const status = stepStatus(step.status);
      out.push({ id: ids.planned(p, step.action_key), kind: "planned", label: `${n} · ${step.title}`, projectId: p, group: "steps", step: n,
        detail: status.label, tone: step.status === "failed" ? "failed" : step.status === "uncertain" ? "attention" : undefined });
    }
  });
  // Analyses outside the plan: run by a person, figure versions, or steps since taken out of it.
  const loose = [...model.project.tasks].filter((t) => !inPlan.has(t.task_id)).sort((a, b) => a.submitted_at.localeCompare(b.submitted_at));
  for (const task of loose) out.push(analysisNode(model, task, task.figure ? task.figure.title : title(task.task_id), "also", endedSince(task, since)));
  // Lab data apart from the analyses: each file the lab sent, read, and each table prepared from it, in order.
  for (const task of labTasks(model)) out.push(analysisNode(model, task, labLabel(task), "lab", endedSince(task, since)));

  const fileCount = Object.values(model.evidence).reduce((n, e) => n + e.packages.reduce((m, pkg) => m + pkg.view.figures.length
    + (["tables", "code", "inputs", "notes", "parameters", "environment", "logs", "other"] as const).reduce((k, key) => k + pkg.view[key].length, 0), 0), 0);
  const inputs = Object.values(model.evidence).reduce((n, e) => n + e.packages.reduce((m, pkg) => m + pkg.view.inputs.length, 0), 0);
  out.push({ id: ids.data(p), kind: "data", label: "Data used", projectId: p, group: "record", detail: inputs ? `${inputs}` : undefined });
  out.push({ id: ids.files(p), kind: "files", label: "All files", projectId: p, group: "record", detail: fileCount ? `${fileCount}` : undefined });

  const runs = model.runs.map(({ run, turns, events }) => {
    const rejected = events.filter((e) => isModelRetry(e.code)).length;
    return {
      id: ids.run(p, run.run_id), kind: "run" as const, projectId: p, runId: run.run_id,
      label: `Work session started ${when(run.created_at ?? run.updated_at)}`,
      detail: `${turns.length} decision${turns.length === 1 ? "" : "s"}${rejected ? ` · ${rejected} rejected repl${rejected === 1 ? "y" : "ies"}` : ""}`,
      tone: run.status === "failed" ? "failed" as Tone : run.status === "review_required" ? "attention" as Tone
        : ["queued", "running", "waiting"].includes(run.status) ? "active" as Tone : undefined,
      children: turns.map((t) => ({
        id: ids.turn(p, run.run_id, t.turn), kind: "turn" as const, projectId: p, runId: run.run_id, turn: t.turn,
        label: `Decision ${t.turn}`, detail: t.record.proposal.summary.slice(0, 120),
      })),
    };
  });
  if (runs.length) out.push({ id: ids.runs(p), kind: "runs", label: "Work sessions", projectId: p, group: "record", children: runs, detail: `${runs.length}` });
  out.push({ id: ids.people(p), kind: "people", label: "People and sharing", projectId: p, group: "record",
    detail: model.project.visibility === "lab" ? "Shared with the lab" : "Private" });
  out.push({ id: ids.package(p), kind: "package", label: "Evidence package", projectId: p, group: "record" });
  return out;
}

// Records open in the editor; labs, groups of runs or analyses, and kind folders only open and close.
export const openable = (node: WbNode) => !["lab", "runs", "analyses", "folder"].includes(node.kind);

export function flatten(nodes: WbNode[]): WbNode[] {
  const out: WbNode[] = [];
  const walk = (list: WbNode[]) => { for (const node of list) { out.push(node); if (node.children) walk(node.children); } };
  walk(nodes);
  return out;
}

// The chain from a result back to the question: which code and inputs made it, which analysis ran it, which agent
// turn proposed that analysis, and which plan step and question it serves. Every hop is a recorded link; a hop
// the records do not support is left out rather than guessed.
export interface TraceHop { role: string; label: string; detail?: string; target?: string; tone?: Tone }

// `origins` holds who asked for an analysis as the broker recorded it, read per selected analysis.
export function traceFor(model: ProjectModel, node: WbNode, profiles: Identity["profiles"],
  origins: Record<string, Lineage["origin"]> = {}, reader = ""): TraceHop[] {
  const p = model.project.project_id;
  const hops: TraceHop[] = [];
  const title = stepTitleFor(model, profiles);
  const state = model.project.checkpoint?.state;
  let taskId = node.taskId;
  if (node.kind === "file" && taskId && node.artifactId) {
    const pkg = model.evidence[taskId]?.packages.find((x) => x.artifact.id === node.artifactId);
    const member = pkg?.contents.listing.members.find((m) => m.path === node.memberPath);
    hops.push({ role: "File", label: node.label, detail: node.memberPath ?? node.detail,
      tone: member?.manifest === "mismatched" ? "failed" : undefined, target: node.id });
    if (pkg && member && !member.path.startsWith("code/") && !member.path.startsWith("input/")) {
      if (pkg.view.code.length)
        hops.push({ role: "Made by", label: pkg.view.code.map((m) => fileTitle(m.path)).join(", "),
          detail: pkg.view.code.map((m) => m.path).join(" · "), target: ids.file(p, taskId, pkg.artifact.id, pkg.view.code[0].path) });
      if (pkg.view.inputs.length)
        hops.push({ role: "From inputs", label: pkg.view.inputs.slice(0, 3).map((m) => fileTitle(m.path)).join(", ")
          + (pkg.view.inputs.length > 3 ? ` and ${pkg.view.inputs.length - 3} more` : ""),
          detail: `${pkg.view.inputs.length} input file${pkg.view.inputs.length === 1 ? "" : "s"} copied into the analysis`,
          target: ids.file(p, taskId, pkg.artifact.id, pkg.view.inputs[0].path) });
    }
  }
  if (node.kind === "turn" && node.runId && node.turn !== undefined) taskId = undefined;
  if (taskId) {
    const task = taskOf(model, taskId);
    hops.push({ role: "Analysis", label: title(taskId), detail: task ? `${taskStatus(task.status).label} · requested ${when(task.submitted_at)}` : undefined,
      target: ids.analysis(p, taskId), tone: task ? taskStatus(task.status).tone : undefined });
  }
  const turnFor = (id: string) => {
    for (const { run, turns } of model.runs)
      for (const t of turns) if (t.record.submitted_tasks.some((s) => s.task_id === id)) return { run, turn: t };
    return null;
  };
  const found = taskId ? turnFor(taskId) : node.kind === "turn"
    ? (() => { const r = model.runs.find(({ run }) => run.run_id === node.runId); const t = r?.turns.find((x) => x.turn === node.turn);
      return r && t ? { run: r.run, turn: t } : null; })()
    : null;
  if (found)
    hops.push({ role: taskId ? "Proposed in" : "Agent decision", label: `Decision ${found.turn.turn}: ${found.turn.record.proposal.summary}`,
      detail: `Methods check: ${found.turn.record.method_review.decision}. The agent's reading, not verified.`,
      target: ids.turn(p, found.run.run_id, found.turn.turn) });
  // Absence from the loaded runs does not show who asked: runs are paged and older ones may not be loaded.
  // The broker's own origin record answers instead once it has been read.
  else if (taskId) {
    const origin = origins[taskId];
    hops.push(origin ? { role: "Requested by", ...originLabel(origin, reader) }
      : { role: "Requested by", label: "Not in the loaded record", detail: "No work session loaded here lists this analysis, so the record shown does not say who requested it." });
  }
  const step = taskId ? state?.plan.find((s) => s.task_id === taskId)
    : node.kind === "planned" ? state?.plan.find((s) => ids.planned(p, s.action_key) === node.id) : undefined;
  if (step)
    hops.push({ role: "Plan step", label: step.title, detail: `${stepStatus(step.status).label}${step.rationale ? ` · ${step.rationale}` : ""}`,
      target: ids.question(p) });
  if (state?.goal) hops.push({ role: "Question", label: state.goal, target: ids.question(p) });
  return hops;
}

export type ProblemKind = "failed" | "mismatched" | "missing" | "check" | "unopened" | "unread" | "refused";
export interface Problem { tone: Tone; text: string; target?: string; kind: ProblemKind }

// What went wrong, from recorded facts only: failures, refused replies and files that could not be read or checked.
// An agent that stopped to ask for review is where the project stands, not a problem; the status bar says it once
// (three such stops were listed as three identical problems).
export function problemsFor(model: ProjectModel, profiles: Identity["profiles"]): Problem[] {
  const p = model.project.project_id;
  const title = stepTitleFor(model, profiles);
  const out: Problem[] = [];
  for (const { run, events } of model.runs) {
    const rejected = events.filter((e) => isModelRetry(e.code));
    if (rejected.length)
      out.push({ tone: "quiet", kind: "refused", target: ids.run(p, run.run_id),
        text: `${deferralLabel(rejected[0].code, rejected[0].reason)}. ${rejected.length === 1 ? "This happened once" : `This happened ${rejected.length} times`}; no analysis ran because of it.` });
  }
  for (const step of model.project.checkpoint?.state.plan ?? [])
    if (step.status === "uncertain")
      out.push({ tone: "attention", kind: "check", text: `“${step.title}” needs a check: its result files could not be confirmed.`,
        target: step.task_id ? ids.analysis(p, step.task_id) : ids.planned(p, step.action_key) });
  for (const task of model.project.tasks) {
    if (task.status === "failed") out.push({ tone: "failed", kind: "failed", text: `“${title(task.task_id)}” did not finish.`, target: ids.analysis(p, task.task_id) });
    const evidence = model.evidence[task.task_id];
    for (const pkg of evidence?.packages ?? []) {
      const { mismatched, missing } = pkg.view.integrity;
      if (mismatched.length)
        out.push({ tone: "failed", kind: "mismatched", target: ids.file(p, task.task_id, pkg.artifact.id, mismatched[0]),
          text: `${mismatched.length === 1 ? "1 file" : `${mismatched.length} files`} in “${title(task.task_id)}” ${mismatched.length === 1 ? "does" : "do"} not match the checksums recorded when it ran.` });
      if (missing.length)
        out.push({ tone: "failed", kind: "missing", target: ids.analysis(p, task.task_id),
          text: `${missing.length} listed file${missing.length === 1 ? " is" : "s are"} missing from “${title(task.task_id)}”.` });
    }
    for (const _unreadable of evidence?.unreadable ?? [])
      out.push({ tone: "attention", kind: "unopened", target: ids.analysis(p, task.task_id),
        text: `A package in “${title(task.task_id)}” could not be opened here; it can still be downloaded.` });
    if (model.evidenceFailed?.includes(task.task_id))
      out.push({ tone: "attention", kind: "unread", target: ids.analysis(p, task.task_id),
        text: `The files of “${title(task.task_id)}” could not be read just now. The analysis is unchanged; open it to try again.` });
  }
  return out;
}

export interface AttentionGroup { kind: ProblemKind; title: string; tone: Tone; items: Problem[] }
const ATTENTION: [ProblemKind, string][] = [
  ["failed", "Did not finish"], ["mismatched", "Files that do not match their checksums"], ["missing", "Files missing from a package"],
  ["check", "Steps to check"], ["unopened", "Packages that could not be opened here"], ["unread", "Files that could not be read just now"],
  ["refused", "Replies the system refused"],
];
/** Needs attention (Glass Box, Detailed view): the problems grouped by kind, most serious first, each group with its
 *  count. A normal wait or a stop for review is where the project stands, not a problem. */
export function attentionFor(model: ProjectModel, profiles: Identity["profiles"]): AttentionGroup[] {
  const problems = problemsFor(model, profiles);
  return ATTENTION.map(([kind, title]) => {
    const items = problems.filter((x) => x.kind === kind);
    return { kind, title, tone: items[0]?.tone ?? "quiet", items };
  }).filter((g) => g.items.length);
}

export type FileKind = "figure" | "table" | "code" | "data" | "log" | "other";
export interface FileRow {
  id: string; taskId: string; artifactId: string; path: string; originalPath?: string; title: string; kind: FileKind;
  size: number; manifest: string; fresh: boolean;
}
export interface FileGroup { key: string; title: string; step: number | null; taskId: string; rows: FileRow[] }
const KIND_OF: Record<string, FileKind> = { figures: "figure", tables: "table", code: "code", inputs: "data", logs: "log",
  notes: "other", parameters: "other", environment: "other", other: "other" };
const KIND_ORDER: FileKind[] = ["figure", "table", "code", "data", "log", "other"];
/** Every retained file of a project for the All files view (long runs rule 6): grouped by plan step in order, then the
 *  analyses outside the plan; one row per figure however many formats R saved; New when its analysis ended after the
 *  reader's last look. Read from the package listings already loaded; nothing here fetches. */
export function filesFor(model: ProjectModel, profiles: Identity["profiles"], since: string | null = null): FileGroup[] {
  const p = model.project.project_id;
  const title = stepTitleFor(model, profiles);
  const plan = model.project.checkpoint?.state.plan ?? [];
  const numbered = new Map(plan.flatMap((s, i) => (s.task_id ? [[s.task_id, i + 1] as const] : [])));
  // The analyses by plan step, then the lab's own files and the tables prepared from them.
  const tasks = [...[...model.project.tasks].sort((a, b) => (numbered.get(a.task_id) ?? Infinity) - (numbered.get(b.task_id) ?? Infinity)
    || a.submitted_at.localeCompare(b.submitted_at)), ...labTasks(model)];
  const groups: FileGroup[] = [];
  for (const task of tasks) {
    const fresh = endedSince(task, since);
    const rows: FileRow[] = [];
    for (const pkg of model.evidence[task.task_id]?.packages ?? []) {
      const members = new Map(pkg.contents.listing.members.map((m) => [m.path, m]));
      const row = (key: string, path: string, original?: string): FileRow => {
        const m = members.get(path), o = original ? members.get(original) : undefined;
        return { id: ids.file(p, task.task_id, pkg.artifact.id, path), taskId: task.task_id, artifactId: pkg.artifact.id, path, originalPath: original,
          title: fileTitle(path), kind: KIND_OF[key] ?? "other", size: (m?.size_bytes ?? 0) + (o?.size_bytes ?? 0),
          manifest: [m?.manifest, o?.manifest].includes("mismatched") ? "mismatched" : m?.manifest ?? "unknown", fresh };
      };
      for (const f of pkg.view.figures) rows.push(row("figures", f.display.path, f.original?.path));
      for (const key of ["tables", "code", "inputs", "logs", "notes", "parameters", "environment", "other"] as const)
        for (const m of pkg.view[key]) rows.push(row(key, m.path));
    }
    if (!rows.length) continue;
    rows.sort((a, b) => KIND_ORDER.indexOf(a.kind) - KIND_ORDER.indexOf(b.kind) || a.path.localeCompare(b.path));
    const n = numbered.get(task.task_id) ?? null;
    groups.push({ key: task.task_id, step: n, taskId: task.task_id, rows,
      title: n ? `Step ${n}: ${title(task.task_id)}` : task.figure ? task.figure.title : task.dataset ? labLabel(task) : title(task.task_id) });
  }
  return groups;
}

/** The inspector's details for the open record: plain facts from the loaded records, as label and value pairs. */
export function detailsFor(model: ProjectModel, node: WbNode, profiles: Identity["profiles"], reader = ""): [string, string][] {
  const title = stepTitleFor(model, profiles);
  const state = model.project.checkpoint?.state;
  const task = node.taskId ? taskOf(model, node.taskId) : undefined;
  const method = (t: { profile: string }) => profiles.find((x) => x.key === t.profile)?.name ?? t.profile;
  switch (node.kind) {
    case "question": {
      const plan = state?.plan ?? [];
      const done = plan.filter((s) => s.status === "completed" || s.status === "skipped").length;
      return [["Saved version", model.project.checkpoint ? String(model.project.checkpoint.revision) : "None yet"],
        ["Steps", plan.length ? `${done} of ${plan.length} finished; the plan can change` : "None planned yet"],
        ["Analyses", String(model.project.tasks.filter((t) => !t.figure).length)], ["Updated", when(model.project.updated_at)]];
    }
    case "planned": {
      const step = state?.plan.find((s) => ids.planned(model.project.project_id, s.action_key) === node.id);
      return step ? [["Status", stepStatus(step.status).label], ["The agent's reason", step.rationale || "None recorded"]] : [];
    }
    case "analysis": {
      if (!task) return [];
      const review = model.reviews?.[task.task_id];
      const pkgs = model.evidence[task.task_id]?.packages ?? [];
      const count = (key: "figures" | "tables" | "code" | "inputs") => pkgs.reduce((n, pkg) => n + pkg.view[key].length, 0);
      return [["Status", taskStatus(task.status).label], ["Method", method(task)], ["Requested", when(task.submitted_at)], ["Updated", when(task.updated_at)],
        ["Files", pkgs.length ? `${count("figures")} figures, ${count("tables")} tables, ${count("code")} code, ${count("inputs")} data` : "None retained yet"],
        ["Review", review && (review.reviewer_kind === "person") ? `${review.decision === "accepted" ? "Accepted" : review.decision === "rejected" ? "Rejected" : "Changes requested"} by ${review.reviewer_id === reader ? "you" : review.reviewer_name ?? "a person"}` : "Not yet reviewed by a person"]];
    }
    case "file": {
      if (!task || !node.artifactId) return [];
      const pkg = model.evidence[task.task_id]?.packages.find((x) => x.artifact.id === node.artifactId);
      const member = pkg?.contents.listing.members.find((m) => m.path === node.memberPath);
      const original = node.originalPath ? pkg?.contents.listing.members.find((m) => m.path === node.originalPath) : undefined;
      const artifact = model.evidence[task.task_id]?.artifacts.find((a) => a.id === node.artifactId);
      const checked = (m?: { manifest: string }) => m?.manifest === "matched" ? "Matches the package checksum list" : m?.manifest === "mismatched"
        ? "Does not match the package checksum list" : "Checked against its recorded checksum when read";
      if (member) return [["File", member.path], ...(original ? [["Original", original.path] as [string, string]] : []),
        ["Size", bytes(member.size_bytes + (original?.size_bytes ?? 0))], ["SHA-256", `${member.sha256.slice(0, 16)}…`],
        ["Checksum", checked(original?.manifest === "mismatched" ? original : member)], ["Made in", title(task.task_id)], ["Method", method(task)]];
      return artifact ? [["File", artifact.relative_path.split("/").at(-1) ?? artifact.relative_path], ["Size", bytes(artifact.size_bytes)],
        ["SHA-256", `${artifact.sha256.slice(0, 16)}…`], ["Made in", title(task.task_id)]] : [];
    }
    case "turn": {
      const found = model.runs.find(({ run }) => run.run_id === node.runId)?.turns.find((t) => t.turn === node.turn);
      return found ? [["Recorded", when(found.created_at)], ["Methods check", found.record.method_review.decision === "proceed" ? "Agreed" : "Asked for review"],
        ["Analyses it started", String(found.record.submitted_tasks.length)], ["Saved version", String(found.checkpoint_revision)]] : [];
    }
    case "run": {
      const found = model.runs.find(({ run }) => run.run_id === node.runId);
      return found ? [["Started", when(found.run.created_at ?? found.run.updated_at)], ["Decisions", `${found.turns.length} of ${found.run.max_turns}`],
        ["At a time", `Up to ${found.run.max_parallel} analyses`]] : [];
    }
    case "files": {
      const groups = filesFor(model, profiles);
      const rows = groups.flatMap((g) => g.rows);
      const matched = rows.filter((r) => r.manifest === "matched").length;
      return [["Files", String(rows.length)], ["Steps with files", String(groups.length)], ["Total size", bytes(rows.reduce((n, r) => n + r.size, 0))],
        ["Checksums", rows.length ? `${matched} of ${rows.length} match their package checksum lists` : "No file yet"]];
    }
    case "data": {
      const copies = Object.values(model.evidence).reduce((n, e) => n + e.packages.reduce((m, pkg) => m + pkg.view.inputs.length, 0), 0);
      return [["Data sources", String(state?.workspace.input_refs.length ?? 0)], ["Copies the steps read", String(copies)]];
    }
    case "people": {
      const m = model.members;
      return [["Sharing", model.project.visibility === "lab" ? "Shared with the lab" : "Private"],
        ...(m ? [["People given access", String(m.grants.filter((g) => g.enabled).length)] as [string, string]] : [])];
    }
    default: return [];
  }
}

/** Where the project stands, from the same records the lab index reads: its latest work session, its analyses and
 *  people's current reviews of them. Unread reviews count nothing. */
export function factsOf(model: ProjectModel): IndexFacts {
  const run = [...model.runs].map(({ run }) => run).sort((a, b) => (b.created_at ?? b.updated_at).localeCompare(a.created_at ?? a.updated_at))[0];
  const own = model.project.tasks.filter((t) => !t.figure);
  const done = own.filter((t) => t.status === "completed" || t.status === "review_required");
  const current = done.map((t) => model.reviews?.[t.task_id]).filter((r): r is TaskReview => !!r && byPerson(r) && r.covers_current_outputs);
  return { question: model.project.checkpoint?.state.goal ?? null, capabilities: model.project.capabilities,
    latest_run: run ? { run_id: run.run_id, status: run.status, code: run.code, turn: run.turn, max_turns: run.max_turns, updated_at: run.updated_at } : null,
    task_counts: { total: own.length, active: own.filter((t) => ["queued", "leased", "starting", "running", "cancelling"].includes(t.status)).length,
      finished: done.length, failed: own.filter((t) => t.status === "failed").length,
      ...(model.reviews ? { reviewed: current.length, rejected: current.filter((r) => r.decision === "rejected").length,
        changes: current.filter((r) => r.decision === "needs_changes").length } : {}) } };
}

// The Detailed view follows the lab chosen at the top, as every other page does: operator labs and other labs stay out
// of its default view (context.ts).
const newestFirst = (a: ProjectSummary, b: ProjectSummary) => b.updated_at.localeCompare(a.updated_at) || a.title.localeCompare(b.title);

/** The chosen lab's most recently changed projects, newest first (by title when two changed at once), for the Detailed
 *  view's start page. */
export function recentProjects(list: ProjectSummary[], lab: string, limit = 8): ProjectSummary[] {
  return list.filter((p) => p.group_key === lab).sort(newestFirst).slice(0, limit);
}

/** The project picker's labs: the chosen lab first, then the reader's other research labs, each with the projects the
 *  reader can open, newest first. An operator lab appears only while it is the chosen lab; a lab with no project is
 *  left out. */
export function pickerLabs<G extends { key: string; name: string }>(groups: G[], list: ProjectSummary[], lab: string) {
  const shown = labOrder(groups).filter((g) => g.key === lab || !operatorLab(g.key));
  return [...shown.filter((g) => g.key === lab), ...shown.filter((g) => g.key !== lab)]
    .map((g) => ({ key: g.key, name: g.name, projects: list.filter((p) => p.group_key === g.key).sort(newestFirst) }))
    .filter((g) => g.projects.length);
}

// Palette search: every word of the query must start a word of the label or detail, or appear inside it.
export function search(nodes: WbNode[], query: string, limit = 30): WbNode[] {
  const words = query.toLowerCase().split(/\s+/).filter(Boolean);
  if (!words.length) return [];
  const scored: { node: WbNode; score: number }[] = [];
  for (const node of nodes) {
    if (node.kind === "folder" || node.kind === "lab") continue;
    const label = node.label.toLowerCase();
    const detail = `${node.detail ?? ""} ${node.originalPath?.split("/").at(-1) ?? ""}`.toLowerCase();
    let score = 0;
    for (const word of words) {
      const starts = new RegExp(`(^|[\\s/_.:-])${word.replace(/[.*+?^${}()|[\]\\]/g, "\\$&")}`);
      if (label.startsWith(word)) score += 5;
      else if (starts.test(label)) score += 3;
      else if (label.includes(word)) score += 2;
      else if (detail.includes(word)) score += 1;
      else { score = 0; break; }
    }
    if (score) scored.push({ node, score });
  }
  return scored.sort((a, b) => b.score - a.score || a.node.label.localeCompare(b.node.label)).slice(0, limit).map((s) => s.node);
}

// The IDs of a node's ancestors, outermost first, so the explorer can open the path to it. A file inside a package
// sits in a folder named for its kind, whose ID is not a prefix of the file's, so the tree is walked.
export function pathTo(nodes: WbNode[], id: string): string[] | null {
  for (const node of nodes) {
    if (node.id === id) return [];
    if (node.children) {
      const below = pathTo(node.children, id);
      if (below) return [node.id, ...below];
    }
  }
  return null;
}
