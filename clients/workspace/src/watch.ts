// Watch it work (Glass Box, long runs rule 3): the project's record as it is written, newest first, one line each. It
// shows records the page already reads (each decision and its methods check, each analysis requested and ended with
// its files and checksum lists, the recorded steps of the analyses being followed, people's instructions and access
// changes) and never adds to them: it is not a chat and holds no guess. Agent lines are its reading; the rest are
// records. The full record keeps every entry.
import type { TaskEvent } from "./contracts.ts";
import type { HistoryEntry } from "./history.ts";
import { activeTask, eventLabel, methodsCheckWords, taskStatus, type Tone } from "./language.ts";
import { list } from "./since.ts";

export type WatchKind = "now" | "agent" | "check" | "files" | "system" | "you" | "colleague" | "administrator";
export interface WatchLine { key: string; at: string; kind: WatchKind; text: string; tone: Tone; taskId?: string }

export const watchKindNames: Record<WatchKind, string> = {
  now: "System", agent: "Agent", check: "Check", files: "Files", system: "System", you: "You", colleague: "A colleague",
  administrator: "Administrator",
};

/** One analysis's retained files counted by kind, and whether every package matches its checksum list. */
export interface WatchFiles { figures: number; tables: number; code: number; logs: number; other: number; intact: boolean }

export interface WatchInput {
  /** The project's history as `buildHistory` writes it; its analysis entries are restated below with their files. */
  history: HistoryEntry[];
  tasks: { task_id: string; status: string; submitted_at: string; updated_at: string }[];
  /** The plan step an analysis belongs to, numbered from 1, or null for an analysis outside the plan. */
  step: (taskId: string) => { number: number; title: string } | null;
  title: (taskId: string) => string;
  files: (taskId: string) => WatchFiles | null;
  /** The recorded steps of the analyses the page follows, by analysis. */
  toolSteps: Record<string, TaskEvent[]>;
  /** What the system does now, from the status line; null when no work runs. */
  now: { text: string; time: string } | null;
  at: string;
}

// Lifecycle codes the requested and ended lines already state.
const STATED = new Set(["task_submitted", "task_accepted", "task_completed", "task_failed", "agent_completed", "agent_failed", "review_required"]);
// Records with one time read with the later part of the work first: a request follows its check, which follows the
// decision, which follows the end of the analysis it read.
const RANK = { requested: 1, check: 2, decision: 3, ended: 4 } as const;

const counted = (f: WatchFiles) => list(([[f.figures, "figure", "figures"], [f.tables, "table", "tables"], [f.code, "code file", "code files"],
  [f.logs, "log", "logs"], [f.other, "other file", "other files"]] as const).filter(([n]) => n > 0).map(([n, one, many]) => `${n} ${n === 1 ? one : many}`));

export function watchLines(input: WatchInput): WatchLine[] {
  const lines: (WatchLine & { rank: number })[] = [];
  const push = (line: WatchLine, rank = 0) => { lines.push({ ...line, rank }); };
  for (const entry of input.history) {
    // Analyses are stated from their records below; a revision that only restates analysis statuses is bookkeeping.
    if (entry.key.startsWith("task-") || (entry.key.startsWith("rev-") && entry.source === "system")) continue;
    if (entry.decision !== undefined) {
      push({ key: `${entry.key}:decision`, at: entry.at, kind: "agent", tone: entry.tone, taskId: entry.taskId,
        text: `Decision ${entry.decision}: ${entry.detail?.trim() || entry.title}` }, RANK.decision);
      if (entry.check) push({ key: `${entry.key}:check`, at: entry.at, kind: "check", tone: entry.check.decision === "proceed" ? "quiet" : "attention",
        text: `Methods check ${methodsCheckWords(entry.check.decision)}${entry.check.reason ? `: ${entry.check.reason}` : ""}` }, RANK.check);
      continue;
    }
    const text = entry.title === "Instruction to the agent" && entry.detail
      ? `${entry.source === "you" ? "Your instruction" : "A colleague's instruction"}: “${entry.detail}”` : entry.title;
    push({ key: entry.key, at: entry.at, kind: entry.source, tone: entry.tone, text });
  }
  const named = (taskId: string) => {
    const step = input.step(taskId);
    return step ? `Step ${step.number}` : `Analysis “${input.title(taskId)}”`;
  };
  for (const task of input.tasks) {
    const step = input.step(task.task_id);
    push({ key: `requested:${task.task_id}`, at: task.submitted_at, kind: "system", tone: "quiet", taskId: task.task_id,
      text: step ? `Step ${step.number} requested: ${step.title}` : `Analysis requested: ${input.title(task.task_id)}` }, RANK.requested);
    if (activeTask(task.status)) continue;
    const status = taskStatus(task.status);
    const made = task.status === "failed" || task.status === "cancelled" ? null : input.files(task.task_id);
    const what = made ? counted(made) : "";
    const ended = `${named(task.task_id)} ${status.label.toLowerCase()}`;
    push({ key: `ended:${task.task_id}`, at: task.updated_at, kind: what ? "files" : "system", taskId: task.task_id,
      tone: made && !made.intact ? "attention" : status.tone === "done" ? "quiet" : status.tone,
      text: !what ? ended : made!.intact ? `${ended}: ${what}, each matching its checksum list` : `${ended}: ${what}; a package does not match its checksum list` }, RANK.ended);
  }
  for (const [taskId, events] of Object.entries(input.toolSteps)) {
    for (const event of events) {
      if (STATED.has(event.code)) continue;
      push({ key: `step:${taskId}:${event.sequence}`, at: event.occurred_at, kind: "system", taskId,
        tone: event.code.startsWith("lease_") || event.code === "sandbox_recovery_needed" ? "attention" : "quiet",
        text: `${named(taskId)}: ${eventLabel(event.code)}` });
    }
  }
  lines.sort((a, b) => Date.parse(b.at) - Date.parse(a.at) || a.rank - b.rank || b.key.localeCompare(a.key));
  const out = lines.map(({ rank: _rank, ...line }) => line);
  if (input.now) out.unshift({ key: "now", at: input.at, kind: "now", tone: "active", text: `${input.now.text}, ${input.now.time}` });
  return out;
}
