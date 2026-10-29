import type { Checkpoint, ResearchRun, ResearchTurn, RunEvent } from "./projects.ts";
import { auditLabel, deferralLabel, isModelRetry, lessonLabel, methodsCheckWords, reviewFindingLine, taskStatus, type Tone } from "./language.ts";
import { INSTRUCTION, isInstruction } from "./standing.ts";

// Who produced a history entry. Only `system` entries are recorded facts; agent entries
// are its saved reading of the facts.
export type Source = "you" | "colleague" | "agent" | "system" | "administrator";

export interface HistoryEntry {
  key: string;
  at: string;
  source: Source;
  title: string;
  detail?: string;
  secondary?: string;
  tone: Tone;
  unverified?: boolean;
  taskId?: string;
  revision?: number;
  /** A decision's kept inputs: the run and the SHA-256 of what the lead and the methods check were given. */
  input?: DecisionInputs;
  /** A decision's number in its work session and what the methods check said about it. */
  decision?: number;
  check?: { decision: string; reason: string };
}

export interface DecisionInputs { run: string; lead?: string; review?: string }

export interface ProjectTaskRecord {
  task_id: string; action_key: string; status: string; submitted_at: string; updated_at: string; profile: string;
}

export interface AuditRecord {
  event_id: string; kind: string; occurred_at: string; actor_id?: string;
  detail: { reason?: string; expires_at?: string; capabilities?: string[]; member_id?: string; bytes?: number };
}

export interface HistoryInput {
  me: string;
  checkpoints: Checkpoint[];
  runs: { run: ResearchRun; turns: ResearchTurn[]; events: RunEvent[] }[];
  tasks: ProjectTaskRecord[];
  audit: AuditRecord[];
  stepTitle: (taskId: string) => string | undefined;
  personName?: (principalId: string | undefined) => string;
}

// Two revisions differ only by recorded task statuses and evidence: the broker's own
// reconciliation, not an edit by a person or a decision by the agent.
export function reconciliationOnly(before: Checkpoint["state"], after: Checkpoint["state"]): boolean {
  const strip = (state: Checkpoint["state"]) => JSON.stringify({
    ...state,
    plan: state.plan.map(({ status: _s, evidence_refs: _e, ...rest }) => rest),
  });
  return strip(before) === strip(after);
}

export function buildHistory(input: HistoryInput): HistoryEntry[] {
  const entries: HistoryEntry[] = [];
  const agentRevisions = new Set<number>();
  for (const { turns } of input.runs) for (const turn of turns) agentRevisions.add(turn.checkpoint_revision);
  const ordered = [...input.checkpoints].sort((a, b) => a.revision - b.revision);
  ordered.forEach((checkpoint, index) => {
    if (agentRevisions.has(checkpoint.revision)) return;
    const previous = index > 0 ? ordered[index - 1] : undefined;
    const author: Source = checkpoint.author_id === input.me ? "you" : "colleague";
    if (!previous) {
      entries.push({ key: `rev-${checkpoint.revision}`, at: checkpoint.created_at, source: author, tone: "quiet",
        title: "Question and starting point saved", detail: checkpoint.state.goal, revision: checkpoint.revision });
    } else if (reconciliationOnly(previous.state, checkpoint.state)) {
      entries.push({ key: `rev-${checkpoint.revision}`, at: checkpoint.created_at, source: "system", tone: "quiet",
        title: "Step status updated from the analysis record", revision: checkpoint.revision });
    } else if (checkpoint.state.constraints.some((c) => isInstruction(c) && !previous.state.constraints.includes(c))) {
      // Asking for changes saves the instruction with the question; the notebook shows it as the scientist's own entry.
      const added = checkpoint.state.constraints.filter((c) => isInstruction(c) && !previous.state.constraints.includes(c));
      entries.push({ key: `rev-${checkpoint.revision}`, at: checkpoint.created_at, source: author, tone: "quiet",
        title: "Instruction to the agent", detail: added.map((c) => c.slice(INSTRUCTION.length)).join(" "), revision: checkpoint.revision });
    } else {
      entries.push({ key: `rev-${checkpoint.revision}`, at: checkpoint.created_at, source: author, tone: "quiet",
        title: previous.state.goal !== checkpoint.state.goal ? "Research question edited" : "Steps or constraints edited",
        detail: previous.state.goal !== checkpoint.state.goal ? checkpoint.state.goal : undefined, revision: checkpoint.revision });
    }
  });
  input.runs.forEach(({ run, turns, events }, runIndex) => {
    for (const turn of turns) {
      const outcome = turn.record.outcome;
      entries.push({
        key: `turn-${run.run_id}-${turn.turn}`,
        at: turn.created_at,
        source: "agent",
        unverified: true,
        tone: outcome === "review_required" ? "attention" : "quiet",
        title: outcome === "delegate" ? `Research agent started ${turn.record.submitted_tasks.length === 1 ? "an analysis" : `${turn.record.submitted_tasks.length} analyses`}`
          : outcome === "review_required" ? "Research agent stopped for your review"
          : outcome === "revise" ? "The methods check sent the agent's proposal back" : "Research agent paused",
        detail: turn.record.proposal.summary,
        secondary: [`Methods check (${methodsCheckWords(turn.record.method_review.decision)}): ${turn.record.method_review.reason}`,
          ...(turn.record.method_review.findings ?? []).map((f) => reviewFindingLine(f, turn.record.proposal.actions))].join(" "),
        taskId: turn.record.submitted_tasks[0]?.task_id,
        revision: turn.checkpoint_revision,
        decision: turn.turn,
        check: { decision: turn.record.method_review.decision, reason: turn.record.method_review.reason },
        input: turn.record.lead?.context_sha256
          ? { run: run.run_id, lead: turn.record.lead.context_sha256, review: turn.record.methods_specialist?.context_sha256 }
          : undefined,
      });
    }
    // Rejected model replies are grouped by run position so repeated retries read as one record.
    let retries: RunEvent[] = [];
    const flush = () => {
      if (!retries.length) return;
      const last = retries[retries.length - 1];
      entries.push({
        key: `retry-${run.run_id}-${retries[0].sequence}`,
        at: last.occurred_at,
        source: "system",
        tone: last.status_after === "paused" ? "attention" : "quiet",
        title: retries.length === 1 ? "A reply from the planning model was rejected" : `${retries.length} replies from the planning model were rejected`,
        detail: retries.map((e) => deferralLabel(e.code, e.reason)).filter((v, i, a) => a.indexOf(v) === i).join(". ") + ".",
        secondary: last.status_after === "paused" ? "The run paused after repeated rejections. No analysis ran because of these replies." : "No analysis ran because of these replies. The agent tried again.",
      });
      retries = [];
    };
    for (const event of events) {
      if (isModelRetry(event.code)) { retries.push(event); continue; }
      flush();
      if (event.code === "waiting_for_tools") continue;
      entries.push({ key: `defer-${run.run_id}-${event.sequence}`, at: event.occurred_at, source: "system", tone: event.code === "review_required" || event.code === "turn_limit" ? "attention" : "quiet",
        title: deferralLabel(event.code, event.reason) });
    }
    flush();
    void runIndex;
  });
  for (const task of input.tasks) {
    const title = input.stepTitle(task.task_id) ?? "Analysis";
    const status = taskStatus(task.status);
    entries.push({ key: `task-${task.task_id}-submitted`, at: task.submitted_at, source: "system", tone: "quiet",
      title: `Analysis requested: ${title}`, taskId: task.task_id });
    if (!["queued", "leased", "starting", "running"].includes(task.status))
      entries.push({ key: `task-${task.task_id}-ended`, at: task.updated_at, source: "system", tone: status.tone,
        title: `Analysis ${status.label.toLowerCase()}: ${title}`, taskId: task.task_id });
  }
  for (const event of input.audit) {
    if (event.kind === "research_started" || event.kind === "research_paused") {
      entries.push({ key: `audit-${event.event_id}`, at: event.occurred_at, source: event.actor_id === input.me ? "you" : "colleague", tone: "quiet", title: auditLabel(event.kind) });
    } else if (event.kind === "admin_inspection" || event.kind === "admin_inspection_ended") {
      entries.push({ key: `audit-${event.event_id}`, at: event.occurred_at, source: "administrator", tone: "attention", title: auditLabel(event.kind), detail: event.detail.reason });
    } else if (event.kind === "access_changed") {
      const who = input.personName?.(event.detail.member_id) ?? "A person";
      const granted = event.detail.capabilities ?? [];
      const what = !granted.length ? "no longer has access"
        : granted.includes("steer") ? "can now steer the research"
        : granted.includes("contribute") ? "can now add analyses"
        : granted.includes("review") ? "can now review" : "can now view";
      entries.push({ key: `audit-${event.event_id}`, at: event.occurred_at, source: event.actor_id === input.me ? "you" : "colleague", tone: "quiet", title: `Access changed: ${who} ${what}` });
    } else {
      entries.push({ key: `audit-${event.event_id}`, at: event.occurred_at, source: event.actor_id === input.me ? "you" : "colleague", tone: "quiet", title: auditLabel(event.kind) });
    }
  }
  return entries.sort((a, b) => b.at.localeCompare(a.at) || b.key.localeCompare(a.key));
}

export interface AgentNote { statement: string; kind: string; label: string; firstRevision: number; lastRevision: number; current: boolean; evidence: number }

// Saved agent notes with the revision that introduced them. A note absent from the
// newest revision is shown as withdrawn instead of disappearing.
export function agentNotes(checkpoints: Checkpoint[]): AgentNote[] {
  const ordered = [...checkpoints].sort((a, b) => a.revision - b.revision);
  const latest = ordered.at(-1)?.revision ?? 0;
  const notes = new Map<string, AgentNote>();
  for (const checkpoint of ordered) {
    for (const lesson of checkpoint.state.lessons) {
      const key = `${lesson.kind}\u0000${lesson.statement}`;
      const note = notes.get(key);
      if (note) note.lastRevision = checkpoint.revision;
      else notes.set(key, { statement: lesson.statement, kind: lesson.kind, label: lessonLabel(lesson.kind), firstRevision: checkpoint.revision, lastRevision: checkpoint.revision, current: false, evidence: lesson.evidence_refs.length });
    }
  }
  for (const note of notes.values()) note.current = note.lastRevision === latest;
  return [...notes.values()].sort((a, b) => b.firstRevision - a.firstRevision);
}
