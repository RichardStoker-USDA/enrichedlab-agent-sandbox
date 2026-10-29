import { BrokerError, request } from "./api.ts";
import type { LiteratureRead, TaskReview } from "./contracts.ts";
import type { Answer, ArtifactDigest, Finding } from "./findings.ts";
import type { TaskQueue } from "./live_words.ts";
import type { PlanLedger } from "./plan_ledger.ts";
import type { StepChecks } from "./step_checks.ts";
import type { LatestRun, TaskCounts } from "./standing.ts";
import type { WorkingCopy } from "./working_copy.ts";

export interface ProjectSummary {
  project_id: string; group_key: string; owner_id: string; title: string;
  visibility: "private" | "lab"; revision: number; created_at: string; updated_at: string;
  // The list's summary (broker migration 020): who started it, its question, the reader's
  // own access and where its latest work session and analyses stand. Absent from older brokers.
  owner?: { principal_id: string; display_name: string | null; kind: "person" | "service_account" };
  question?: string | null;
  capabilities?: string[];
  latest_run?: LatestRun | null;
  task_counts?: TaskCounts;
  /** The reader's last look at the project: the revision they last saw and when. Absent from
   *  older brokers; both fields are null before the reader's first look. */
  last_look?: { revision: number | null; at: string | null };
}
export interface ResearchState {
  schema_version: number; goal: string; constraints: string[]; next_action: string;
  plan: { action_key: string; title: string; status: string; task_id: string | null; evidence_refs: string[]; rationale: string }[];
  lessons: { statement: string; kind: "observation" | "hypothesis" | "constraint"; evidence_refs: string[] }[];
  workspace: { schema_version: number; profile: string; image_digest: string; source_digest: string; input_refs: string[]; tool_refs: string[]; preparation_receipt: string | null;
    /** The sets the project started from, when it started from several (proposed broker shape). */
    starting_points?: { key: string; name: string }[] };
}
export interface Project extends ProjectSummary {
  capabilities: string[];
  checkpoint: { revision: number; author_id: string; created_at: string; state: ResearchState } | null;
  tasks: { task_id: string; action_key: string; parent_task_id: string | null; profile: string; status: string; attempt: number; submitted_at: string; updated_at: string;
    /** A figure version's figure (broker migration 040); null for an analysis and absent from older brokers. */
    figure?: { title: string; feature: string; source_task: string; table_path: string; previous: string | null } | null;
    /** A template analysis's request: a reviewed R analysis of an earlier result's table. It is an
     *  analysis and counts as one. Absent from older brokers. */
    template_analysis?: { title: string; feature: string; source_task: string; table_path: string; previous: string | null } | null;
    /** A lab dataset's intake or preparation. It is not an analysis, so the project read keeps it
     *  in `lab_data`, apart from `tasks`. Absent from older brokers. */
    dataset?: { title: string | null; operation: "intake" | "preparation"; dataset: string; previous: string | null } | null;
    /** An answer document (broker migration 072): a workbook, document or slides of a run's answer. It is not an
     *  analysis, so the read keeps it in `documents`, apart from `tasks`. Absent from older brokers. */
    document?: { kind: "xlsx" | "docx" | "pptx"; run: string; turn: number } | null;
    /** A queued analysis's place (broker 0361eda); null for any other analysis and absent from older brokers. */
    queue?: TaskQueue | null }[];
  task_count: number;
  /** The project's lab dataset tasks, which the read takes out of `tasks` (they are not analyses). */
  lab_data?: Project["tasks"];
  /** The project's answer document tasks, which the read takes out of `tasks` (broker migration 072). */
  documents?: Project["tasks"];
  /** The read holds the newest analyses; the rest page from `/analyses` with this cursor. */
  analyses_total?: number;
  analyses_next?: string | null;
  // Set only while the caller reads this project through an administrator inspection.
  inspection_expires_at?: string | null;
}
export interface Checkpoint { revision: number; author_id: string; created_at: string; state: ResearchState }
export interface WorkspaceChoice {
  key: string; name: string; description: string; input_refs: string[]; tool_refs: string[]; allowed_profiles: string[];
}
export interface ResearchRun {
  run_id: string; project_id: string; status: string; turn: number; max_turns: number;
  max_parallel: number; allowed_profiles: string[]; code: string | null; updated_at: string;
  created_at?: string; failures?: number; actor_id?: string;
}
export interface RunEvent {
  run_id: string; sequence: number; code: string; reason: string | null; generation: number;
  turn: number; failures: number; status_after: string; occurred_at: string;
}
export interface Person { principal_id: string; display_name: string | null; email: string }
export interface ProjectMembers {
  owner: Person;
  grants: (Person & { kind: "person" | "service_account"; enabled: boolean; capabilities: string[] })[];
  eligible: Person[];
}
export interface DirectoryEntry {
  project_id: string; title: string; group_key: string; group_name: string; visibility: "private" | "lab";
  owner: Person & { kind: "person" | "service_account" }; created_at: string; updated_at: string; revision: number;
  task_count: number; inspection_expires_at: string | null; readable: boolean;
}
export interface PackagePreview {
  root: string; snapshot_at: string; files: number; uncompressed_bytes: number;
  counts: { plan_revisions: number; research_runs: number; agent_decisions: number; agent_deferrals: number;
    access_events: number; tasks: number; artifacts_included: number; artifacts_missing: number; artifact_bytes_included: number;
    generated_code_blocks?: number; person_reviews?: number; rerun_analyses?: number };
  coverage: { database_records: string; artifact_bytes: string; scientific_validation: string };
  missing: { task_id: string; artifact_id: string; relative_path: string; label: string; reason: string }[];
  // The package's rerun kit, step by step on SCINet's Atlas and Ceres, written by the broker from its checked site
  // table (the same steps as the package's rerun/README.md); absent from an API that predates it.
  hpc_handoff?: HpcHandoff;
}
export interface HpcHandoff {
  checked: string;
  sites: { key: string; name: string; open_ondemand: string; steps: { text: string; command?: string }[] }[];
  sources: { title: string; url: string; updated: string }[];
}
// answer and findings; absent from earlier decisions.
// literature_refs name the library passages that shaped a step; absent before it.
export interface TurnProposal { summary: string; actions: { action_key?: string; title: string; rationale: string; literature_refs?: string[] }[]; answer?: Answer; findings?: Finding[] }
// A mistake the methods check found: the part of the proposal it is in, its kind, what is wrong and
// the results it read.
export interface ReviewFinding { about: string; problem: string; detail?: string; refs?: string[] }
export interface ResearchTurn {
  turn: number; context_revision: number; checkpoint_revision: number; created_at: string;
  record: { outcome: string; scientific_validation: string;
    proposal: TurnProposal;
    // A proposal the methods check sent back (outcome `revise`) is stored here instead of in
    // `proposal`, so no reader of answers takes it for one; `readableTurn` shows it as the decision's proposal.
    revised_proposal?: TurnProposal;
    revision?: { used_before: number; budget: number; sent_back: boolean };
    // Each model call's receipt names the SHA-256 of the fixed instructions and of the context it was given; since
    // the texts themselves are kept and a lab member can read them.
    method_review: { decision: string; reason: string; findings?: ReviewFinding[] }; lead: ModelReceipt; methods_specialist?: ModelReceipt;
    submitted_tasks: { task_id: string; action_key: string }[];
    // The run's orientation as the turn held it: its status, its profiling steps' keys,
    // whether this turn ran them and whether it fitted the call; the text is read with `orientation`.
    orientation?: { status: string; step_keys: string[]; delegated: boolean; placed: boolean };
    // The records the step's context came from (context version 5; library passages since version 6);
    // absent from earlier decisions.
    context_inputs?: { lab_notes?: { note_id: string; statement_sha256: string }[]; lab_notes_omitted?: number; literature?: LiteratureRead };
    // Each result's check as the lead read it in this decision.
    step_checks?: StepChecks };
}
export interface ModelReceipt { reported_model: string; context_sha256?: string; system_sha256?: string }
export interface KeptText { sha256: string; bytes: number; text: string }
/** A decision as the pages read it: a proposal the methods check sent back reads as the decision's
 *  proposal, with its outcome `revise`; the readers of answers skip that outcome. */
// What the agent wrote before a run's first plan, for a project reader in the lab: its own reading of
// the question, checked by code against what it was shown; never a result.
export interface RunOrientation {
  status: "read"; label: string; step_keys: string[];
  orientation: {
    question: string;
    sources: { key: string; why: string }[];
    tools: { key: string; why: string }[];
    papers: { ref: string; establishes: string }[];
    earlier: { ref: string; establishes: string }[];
    unknowns: string[];
    profiles: { action_key: string; title: string; profile: string; rationale: string }[];
  };
}
export function readableTurn(turn: ResearchTurn): ResearchTurn {
  if (turn.record.proposal || !turn.record.revised_proposal) return turn;
  return { ...turn, record: { ...turn.record, proposal: turn.record.revised_proposal } };
}
const kept = /^sha256:[0-9a-f]{64}$/;
const path = (id: string) => {
  if (!/^[a-f0-9]{8}-[a-f0-9]{4}-[a-f0-9]{4}-[a-f0-9]{4}-[a-f0-9]{12}$/i.test(id)) throw new BrokerError("invalid_reference");
  return `/api/v1/projects/${id}`;
};
const uuid = /^[a-f0-9]{8}-[a-f0-9]{4}-[a-f0-9]{4}-[a-f0-9]{4}-[a-f0-9]{12}$/i;
// A long run holds more analyses than one read returns: the read keeps the newest and names a
// cursor for the rest, so every page reads them all once. An analysis listed twice while pages move keeps its newest
// record; a cursor that does not move stops the reading instead of looping.
const MAX_ANALYSIS_PAGES = 200;
async function withAllAnalyses(project: Project, signal?: AbortSignal): Promise<Project> {
  let next = project.analyses_next ?? null;
  if (!next) return project;
  const tasks = [...project.tasks];
  const known = new Set(tasks.map((t) => t.task_id));
  for (let page = 0; next && page < MAX_ANALYSIS_PAGES; page++) {
    const read: { analyses: Project["tasks"]; next: string | null; total: number } =
      await request(`${path(project.project_id)}/analyses?cursor=${encodeURIComponent(next)}&limit=200`, { signal });
    for (const task of read.analyses) if (!known.has(task.task_id)) { known.add(task.task_id); tasks.push(task); }
    next = read.next && read.next !== next ? read.next : null;
  }
  return { ...project, tasks, analyses_next: null };
}
// Lab dataset tasks are not analyses: they leave the analysis lists and counts, and are kept apart
// for the lab data view and for naming a table prepared from one.
function withLabDataApart(project: Project): Project {
  return { ...project, tasks: project.tasks.filter((t) => !t.dataset), lab_data: project.tasks.filter((t) => t.dataset) };
}
// Answer documents are not analyses either (broker migration 072): each run lists its own documents.
export function withDocumentsApart(project: Project): Project {
  return { ...project, tasks: project.tasks.filter((t) => !t.document), documents: project.tasks.filter((t) => t.document) };
}

/** A task's own title when the broker records one: a figure's, a template analysis's or a lab table's. An empty title
 *  is no title; the caller names the task another way. */
export function taskTitle(project: Project | null | undefined, taskId: string): string | null {
  const task = project?.tasks.find((t) => t.task_id === taskId) ?? project?.lab_data?.find((t) => t.task_id === taskId);
  return task?.figure?.title?.trim() || task?.template_analysis?.title?.trim() || task?.dataset?.title?.trim() || null;
}

export const projects = {
  // The stored digest of a result package for its current bytes.
  digest: (task: string, artifact: string, signal?: AbortSignal) => {
    if (!uuid.test(task) || !uuid.test(artifact)) throw new BrokerError("invalid_reference");
    return request<ArtifactDigest>(`/api/v1/tasks/${task}/artifacts/${artifact}/digest`, { signal });
  },
  list: (signal?: AbortSignal) => request<{ projects: ProjectSummary[] }>("/api/v1/projects?limit=200", { signal }),
  get: (id: string, signal?: AbortSignal) => request<Project>(path(id), { signal })
    .then((project) => withAllAnalyses(project, signal)).then(withLabDataApart).then(withDocumentsApart),
  // `combine` is true when the broker lets a project start from several of these sets (proposed broker shape).
  workspaces: (id: string, signal?: AbortSignal) => request<{ workspaces: WorkspaceChoice[]; combine?: boolean }>(`${path(id)}/workspaces`, { signal }),
  // A lab's starting points before any project exists (the lab's welcome view); older brokers answer not found.
  labWorkspaces: (lab: string, signal?: AbortSignal) =>
    request<{ lab: string; workspaces: WorkspaceChoice[]; combine?: boolean }>(`/api/v1/labs/${encodeURIComponent(lab)}/workspaces`, { signal }),
  // One set as `workspace_key`; several as `workspace_keys`, only where the broker says it combines them.
  prepare: (id: string, body: ({ workspace_key: string } | { workspace_keys: string[] }) & { goal: string; constraints: string[]; request_id: string; expected_revision: number }, signal?: AbortSignal) =>
    request<{ revision: number; allowed_profiles: string[] }>(`${path(id)}/prepare`, { method: "POST", headers: { "Content-Type": "application/json", "X-Agent-Sandbox-Intent": "mutate" }, body: JSON.stringify(body), signal }),
  create: (body: { project_id: string; group: string; title: string; visibility: string }, signal?: AbortSignal) =>
    request<{ project_id: string }>("/api/v1/projects", { method: "POST", headers: { "Content-Type": "application/json", "X-Agent-Sandbox-Intent": "mutate" }, body: JSON.stringify(body), signal }),
  history: (id: string, before=0, signal?: AbortSignal) => request<{ checkpoints: Checkpoint[] }>(`${path(id)}/checkpoints?before=${before}&limit=20`, { signal }),
  audit: (id: string, signal?: AbortSignal) => request<{ events: {event_id: string;kind: string;occurred_at: string;detail: {reason?: string;expires_at?: string;capabilities?: string[]}}[] }>(`${path(id)}/audit?limit=100`, { signal }),
  // Records that the reader has seen the project up to this revision; the broker keeps it forward-only.
  look: (id: string, revision: number, signal?: AbortSignal) =>
    request<{ revision: number; at: string }>(`${path(id)}/look`, { method: "POST", headers: { "Content-Type": "application/json", "X-Agent-Sandbox-Intent": "mutate" }, body: JSON.stringify({ revision }), signal }),
  checkpoint: (id: string, body: { expected_revision: number; request_id: string; state: ResearchState }, signal?: AbortSignal) =>
    request<{ revision: number }>(`${path(id)}/checkpoints`, { method: "POST", headers: { "Content-Type": "application/json", "X-Agent-Sandbox-Intent": "mutate" }, body: JSON.stringify(body), signal }),
  runs: (id: string, signal?: AbortSignal) => request<{ runs: ResearchRun[] }>(`${path(id)}/runs`, { signal }),
  start: (id: string, body: { run_id: string; expected_revision: number; allowed_profiles: string[]; max_turns: number; max_parallel: number }, signal?: AbortSignal) =>
    request<{ run_id: string }>(`${path(id)}/runs`, { method: "POST", headers: { "Content-Type": "application/json", "X-Agent-Sandbox-Intent": "mutate" }, body: JSON.stringify(body), signal }),
  turns: async (run: string, signal?: AbortSignal) => {
    const root = `${path(run).replace("/projects/", "/research-runs/")}/turns`;
    const first = await request<{ turns: ResearchTurn[] }>(`${root}?after=0&limit=100`, { signal });
    // Runs are capped at 128 decisions; a second page retains the full record.
    if (first.turns.length < 100) return { turns: first.turns.map(readableTurn) };
    const last = first.turns[first.turns.length - 1].turn;
    const next = await request<{ turns: ResearchTurn[] }>(`${root}?after=${last}&limit=100`, { signal });
    return { turns: [...first.turns, ...next.turns].map(readableTurn) };
  },
  // The run's orientation before its first plan, for a project reader who is a member of the lab.
  orientation: (run: string, signal?: AbortSignal) =>
    request<RunOrientation>(`${path(run).replace("/projects/", "/research-runs/")}/orientation`, { signal }),
  // The exact text a decision's model was given, for a project reader who is a member of the lab.
  context: (run: string, sha256: string, signal?: AbortSignal) => {
    if (!kept.test(sha256)) throw new BrokerError("invalid_reference");
    return request<KeptText>(`${path(run).replace("/projects/", "/research-runs/")}/contexts/${sha256}`, { signal });
  },
  pause: (run: string, signal?: AbortSignal) => request<void>(`${path(run).replace("/projects/", "/research-runs/")}/pause`, { method: "POST", headers: { "X-Agent-Sandbox-Intent": "mutate" }, signal }, true),
  // Every deferral of a run, including rejected model replies, oldest first.
  runEvents: async (run: string, signal?: AbortSignal) => {
    const root = `${path(run).replace("/projects/", "/research-runs/")}/events`;
    const events: RunEvent[] = [];
    let after = 0;
    for (let page = 0; page < 50; page += 1) {
      const next = await request<{ events: RunEvent[] }>(`${root}?after=${after}&limit=200`, { signal });
      events.push(...next.events);
      if (next.events.length < 200) break;
      after = next.events[next.events.length - 1].sequence;
    }
    return events;
  },
  // Every saved revision, newest first, following the broker's page cursor.
  allHistory: async (id: string, signal?: AbortSignal) => {
    const all: Checkpoint[] = [];
    let before = 0;
    for (let page = 0; page < 100; page += 1) {
      const next = await request<{ checkpoints: Checkpoint[] }>(`${path(id)}/checkpoints?before=${before}&limit=100`, { signal });
      all.push(...next.checkpoints);
      if (next.checkpoints.length < 100) break;
      before = next.checkpoints[next.checkpoints.length - 1].revision;
    }
    return all;
  },
  members: (id: string, signal?: AbortSignal) => request<ProjectMembers>(`${path(id)}/members`, { signal }),
  grant: (id: string, principal: string, capabilities: string[], signal?: AbortSignal) =>
    request<void>(`${path(id)}/grants`, { method: "POST", headers: { "Content-Type": "application/json", "X-Agent-Sandbox-Intent": "mutate" }, body: JSON.stringify({ principal_id: principal, capabilities }), signal }, true),
  inspect: (id: string, reason: string, signal?: AbortSignal) =>
    request<{ expires_at: string; capabilities: string[] }>(`${path(id)}/inspect`, { method: "POST", headers: { "Content-Type": "application/json", "X-Agent-Sandbox-Intent": "mutate" }, body: JSON.stringify({ reason }), signal }),
  endInspection: (id: string, signal?: AbortSignal) =>
    request<{ ended: boolean }>(`${path(id)}/inspect`, { method: "DELETE", headers: { "X-Agent-Sandbox-Intent": "mutate" }, signal }),
  packagePreview: (id: string, signal?: AbortSignal) => request<PackagePreview>(`${path(id)}/package/preview`, { signal }),
  // Until when each analysis keeps its working copy, or when the daily sweep cleared it.
  workingCopies: async (id: string, signal?: AbortSignal) =>
    (await request<{ working_copies: WorkingCopy[] }>(`${path(id)}/working-copies`, { signal })).working_copies,
  // Each saved revision of the plan with the steps it changed and what wrote it.
  planLedger: (id: string, signal?: AbortSignal) => request<PlanLedger>(`${path(id)}/plan-ledger`, { signal }),
  // The newest person's review of each analysis in the project.
  reviews: async (id: string, signal?: AbortSignal) => (await request<{ reviews: TaskReview[] }>(`${path(id)}/reviews`, { signal })).reviews,
  directory: (signal?: AbortSignal) => request<{ projects: DirectoryEntry[] }>("/api/v1/admin/projects?limit=500", { signal }),
};
