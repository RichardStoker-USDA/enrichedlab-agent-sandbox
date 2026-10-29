// The plan ledger: what happened to each planned step, recorded by the writer of each saved revision.
// These helpers turn its rows into plain sentences; the rows themselves stay the record.
import { queryStopWords } from "./query.ts";

export type PlanSource = "lead_turn" | "analysis_record" | "plan_archive" | "person" | "service_account" | "recorded_before_ledger";

export interface PlanStepChange {
  action_key: string;
  step_number: number;
  changes: string[];
  status_before: string | null;
  status_after: string | null;
  task_id: string | null;
  result_count: number;
  title: string | null;
  rationale: string | null;
  task_status: string | null;
  error_code: string | null;
}

export interface PlanRevision {
  revision: number;
  recorded_at: string;
  source: PlanSource;
  run_id: string | null;
  turn: number | null;
  principal_id: string | null;
  steps: PlanStepChange[];
}

export interface PlanLedger {
  revisions: PlanRevision[];
  unrecorded_revisions: number;
  truncated: boolean;
}

export interface StepMoment { at: string; text: string }
export interface EarlierReason { at: string; rationale: string }
// `by` is who removed it, as it reads after "by"; null when no writer was recorded.
export interface RemovedStep { key: string; title: string; at: string; by: string | null; task_id: string | null }

const capital = (text: string) => text.charAt(0).toUpperCase() + text.slice(1);

// Who wrote a revision, as the start of a sentence; null when no writer was recorded.
function writer(revision: PlanRevision, personName: (id: string | undefined) => string): string | null {
  switch (revision.source) {
    case "lead_turn": return "the agent";
    case "analysis_record": return "the system";
    case "plan_archive": return "the system";
    case "person": return personName(revision.principal_id ?? undefined);
    case "service_account": return "an automated caller";
    default: return null;
  }
}

// Why an analysis stopped, in words, for the failure codes a scientist can act on.
const stopped: Record<string, string> = {
  worker_lease_expired: "its compute worker stopped responding",
  ...queryStopWords,
};

function statusWords(change: PlanStepChange, revision: PlanRevision, who: string | null): string {
  // Reconciliation and the agent's turn both take statuses from the analysis records; neither sets one by choice.
  const from = revision.source === "analysis_record" || revision.source === "lead_turn";
  switch (change.status_after) {
    case "active": return "Its analysis started";
    case "completed": return from ? "Its analysis finished" : `${capital(who ?? "someone")} marked it finished`;
    case "failed": {
      if (!from) return who ? `${capital(who)} marked it as not finished` : "It was marked as not finished";
      const reason = change.error_code ? stopped[change.error_code] : undefined;
      return reason ? `Its analysis did not finish: ${reason}` : "Its analysis did not finish";
    }
    case "uncertain": {
      const reason = change.error_code ? stopped[change.error_code] : undefined;
      return reason ? `Its results need a check: ${reason}` : "Its results need a check";
    }
    case "skipped": return who ? `${capital(who)} marked it skipped` : "It was marked skipped";
    case "pending": return who ? `${capital(who)} set it back to planned` : "It was set back to planned";
    default: return "Its status changed";
  }
}

/** What happened to one step, oldest first. */
export function stepMoments(ledger: PlanLedger, key: string, personName: (id: string | undefined) => string): StepMoment[] {
  const moments: StepMoment[] = [];
  for (const revision of ledger.revisions) {
    const change = revision.steps.find((s) => s.action_key === key);
    if (!change || change.changes.includes("removed")) continue;
    const who = writer(revision, personName);
    const at = revision.recorded_at;
    if (change.changes.includes("planned")) {
      if (revision.source === "lead_turn") {
        const started = change.status_after === "active" && change.task_id;
        moments.push({ at, text: `The agent planned it${started ? " and started its analysis" : ""}` });
      } else moments.push({ at, text: who ? `${capital(who)} added it` : "Planned" });
      continue;
    }
    if (change.changes.includes("text")) moments.push({ at, text: who ? `${capital(who)} revised its reason` : "Its reason was revised" });
    if (change.changes.includes("status")) moments.push({ at, text: statusWords(change, revision, who) });
    else if (change.changes.includes("task")) moments.push({ at, text: "It was linked to another analysis" });
  }
  return moments;
}

/** The reasons a step carried before its current one, oldest first. */
export function earlierReasons(ledger: PlanLedger, key: string): EarlierReason[] {
  const texts: EarlierReason[] = [];
  for (const revision of ledger.revisions) {
    const change = revision.steps.find((s) => s.action_key === key);
    if (change?.rationale != null && (change.changes.includes("planned") || change.changes.includes("text")))
      texts.push({ at: revision.recorded_at, rationale: change.rationale });
  }
  return texts.slice(0, -1);
}

/** Steps a revision removed from the plan and no later revision planned again, oldest first. */
export function removedSteps(ledger: PlanLedger, current: string[], personName: (id: string | undefined) => string): RemovedStep[] {
  const titles = new Map<string, string>();
  const removed = new Map<string, RemovedStep>();
  for (const revision of ledger.revisions) {
    for (const change of revision.steps) {
      if (change.title != null) titles.set(change.action_key, change.title);
      if (change.changes.includes("planned")) removed.delete(change.action_key);
      if (change.changes.includes("removed")) {
        // A finished step moved into the plan's archive was not taken out; `archivedStepCount` counts it.
        if (revision.source === "plan_archive") {
          removed.delete(change.action_key);
          continue;
        }
        removed.set(change.action_key, { key: change.action_key, title: titles.get(change.action_key) ?? change.action_key,
          at: revision.recorded_at, by: writer(revision, personName), task_id: change.task_id });
      }
    }
  }
  return [...removed.values()].filter((step) => !current.includes(step.key));
}

/** How many finished steps the system moved out of the working plan into its archive and that are not planned again:
 * one plan holds 128 steps, so a long project keeps its older finished steps there. The ledger read
 * holds the newest 500 revisions, so when it is truncated this is a lower bound and the page says "at least". */
export function archivedStepCount(ledger: PlanLedger, current: string[]): number {
  const archived = new Set<string>();
  for (const revision of ledger.revisions) {
    for (const change of revision.steps) {
      if (change.changes.includes("planned")) archived.delete(change.action_key);
      if (revision.source === "plan_archive" && change.changes.includes("removed")) archived.add(change.action_key);
    }
  }
  return [...archived].filter((key) => !current.includes(key)).length;
}
