// Where a project stands, in the scientist's words, from recorded facts only: the latest work
// session's status and code, the analysis counts and whether a question is saved. Nothing here
// says a question was answered; no record says so.
import type { Tone } from "./language.ts";

// A stop for review waits for the reader when they may steer or review the project, and for someone else otherwise:
// it is never "Finished", because the agent has not finished.
export type Standing = "working" | "waiting" | "waiting_other" | "paused" | "finished" | "not_finished" | "not_started";

export const standingOrder: Standing[] = ["waiting", "working", "waiting_other", "paused", "finished", "not_finished", "not_started"];

export const standingNames: Record<Standing, string> = {
  waiting: "Waiting for you",
  working: "Working now",
  waiting_other: "Waiting for review",
  paused: "Paused",
  finished: "Finished",
  not_finished: "Did not finish",
  not_started: "Not started",
};

export const standingTone: Record<Standing, Tone> = {
  waiting: "attention", working: "active", waiting_other: "quiet", paused: "quiet", finished: "done", not_finished: "failed", not_started: "quiet",
};

export interface LatestRun { run_id: string; status: string; code: string | null; turn: number; max_turns: number; updated_at: string }
export interface TaskCounts {
  total: number; active: number; finished: number; failed: number;
  /** Finished analyses whose newest review by a person still covers their result files, and of those the rejected
   *  ones and those with changes requested. Absent from older brokers. */
  reviewed?: number; rejected?: number; changes?: number;
}

// The facts one index row needs. The broker's list supplies them (migration 020); a project's
// own record fills them in when the list comes from an older broker.
export interface IndexFacts {
  question: string | null;
  capabilities: string[];
  latest_run: LatestRun | null;
  task_counts: TaskCounts;
}

const activeRun = (run: LatestRun | null) => !!run && ["queued", "running", "waiting"].includes(run.status);
const canAct = (capabilities: string[]) => capabilities.includes("steer") || capabilities.includes("review");

// A stop for review is answered once a person's current review covers every finished analysis and none of them asks for
// changes or rejects it (the broker's counts): nothing waits for anyone then.
const acceptedAll = (c: TaskCounts) => c.finished > 0 && c.reviewed !== undefined && c.reviewed >= c.finished && !c.rejected && !c.changes;

export function standingOf(facts: IndexFacts): Standing {
  const { latest_run: run, task_counts: counts } = facts;
  if (counts.active > 0 || activeRun(run)) return "working";
  if (!facts.question) return "not_started";
  if (run?.status === "review_required" && acceptedAll(counts)) return "finished";
  if (run?.status === "review_required" || (run?.status === "paused" && run.code !== "user_paused"))
    return canAct(facts.capabilities) ? "waiting" : "waiting_other";
  if (run?.status === "paused") return "paused";
  if (run?.status === "failed") return "not_finished";
  if (counts.finished > 0) return "finished";
  if (counts.failed > 0) return "not_finished";
  return run ? "finished" : "not_started";
}

const plural = (n: number, one: string, many: string) => `${n} ${n === 1 ? one : many}`;

// Whether a person has checked a project's finished analyses; "" when nothing finished or the broker does not say.
export function reviewSentence(c: TaskCounts): string {
  if (c.reviewed === undefined || !c.finished) return "";
  if (!c.reviewed) return "Not yet reviewed by a person.";
  const parts = [c.rejected ? `${c.rejected} rejected` : "", c.changes ? `${c.changes} with changes requested` : ""].filter(Boolean);
  return `Reviewed by a person: ${c.reviewed} of ${c.finished}${parts.length ? ` (${parts.join(", ")})` : ""}.`;
}

// One plain sentence for an index row or the rail.
export function standingSentence(facts: IndexFacts): string {
  const s = standingOf(facts);
  const { latest_run: run, task_counts: c } = facts;
  const tally = [c.finished ? plural(c.finished, "analysis finished", "analyses finished") : "",
    c.failed ? plural(c.failed, "did not finish", "did not finish") : ""].filter(Boolean).join(", ");
  switch (s) {
    case "working": return c.active ? `Working now: ${plural(c.active, "analysis", "analyses")} running.` : "Working now: the agent is choosing its next step.";
    case "waiting": return run?.status === "review_required"
      ? `The agent stopped and asks you to review what it found${tally ? ` (${tally})` : ""}.`
      : "Paused after replies from the planning model were rejected. Review the record before continuing.";
    case "waiting_other": return run?.status === "review_required"
      ? `The agent stopped for review by someone who can steer this project${tally ? ` (${tally})` : ""}.`
      : "Paused after replies from the planning model were rejected. Someone who can steer this project reviews it next.";
    case "paused": return `Paused by a person. Nothing new starts until someone continues it${tally ? ` (${tally})` : ""}.`;
    case "finished": return !tally ? "Finished."
      : run?.status === "review_required" ? `Finished: ${tally}. A person accepted ${c.failed ? (c.finished === 1 ? "the one that finished" : "each one that finished") : c.finished === 1 ? "it" : "each one"}.`
      : `Finished: ${tally}.`;
    case "not_finished": return run?.status === "failed" ? "The work session stopped with an error." : `Did not finish: ${tally}.`;
    case "not_started": return facts.question ? "The question is saved. The agent has not started." : "No question saved yet.";
  }
}

// The step line on a project: one short rule per plan step, in plan order.
export type StepMark = "done" | "current" | "later" | "failed" | "check";
export function stepLine(plan: { status: string; title: string }[]): { mark: StepMark; title: string }[] {
  return plan.map((step) => ({
    title: step.title,
    mark: step.status === "completed" || step.status === "skipped" ? "done"
      : step.status === "active" ? "current"
      : step.status === "failed" ? "failed"
      : step.status === "uncertain" ? "check" : "later",
  }));
}

// A scientist's instruction is saved as a constraint with this prefix, so the research agent
// reads it with the question and it survives every later saved version.
export const INSTRUCTION = "Scientist's instruction: ";
export const MAX_INSTRUCTION = 900;
export const isInstruction = (constraint: string) => constraint.startsWith(INSTRUCTION);

export interface Instruction { text: string; revision: number; author_id: string; at: string }

// Each instruction with the saved version that first carried it, oldest first.
export function instructionsFrom(checkpoints: { revision: number; author_id: string; created_at: string; state: { constraints: string[] } }[]): Instruction[] {
  const seen = new Map<string, Instruction>();
  for (const c of [...checkpoints].sort((a, b) => a.revision - b.revision))
    for (const constraint of c.state.constraints)
      if (isInstruction(constraint) && !seen.has(constraint))
        seen.set(constraint, { text: constraint.slice(INSTRUCTION.length), revision: c.revision, author_id: c.author_id, at: c.created_at });
  return [...seen.values()];
}

// An instruction's life, read from the saved versions and the agent's decisions: the version that added
// it, the first decision whose context held it, and the version that took it out. The same text given again after a
// withdrawal is a new instruction.
export interface InstructionHistory extends Instruction {
  read: { run: string; turn: number; at: string } | null;
  withdrawn: { revision: number; author_id: string; at: string } | null;
}
export interface Decision { run: string; turn: number; context_revision: number; created_at: string }

export function instructionHistory(checkpoints: { revision: number; author_id: string; created_at: string; state: { constraints: string[] } }[],
                                   decisions: Decision[]): InstructionHistory[] {
  const ordered = [...checkpoints].sort((a, b) => a.revision - b.revision);
  const held = new Map(ordered.map((c) => [c.revision, new Set(c.state.constraints)]));
  const all: InstructionHistory[] = [], open = new Map<string, InstructionHistory>();
  let previous = new Set<string>();
  for (const c of ordered) {
    const now = new Set(c.state.constraints.filter(isInstruction));
    for (const constraint of now) if (!previous.has(constraint)) {
      const entry: InstructionHistory = { text: constraint.slice(INSTRUCTION.length), revision: c.revision, author_id: c.author_id, at: c.created_at, read: null, withdrawn: null };
      open.set(constraint, entry); all.push(entry);
    }
    for (const constraint of previous) if (!now.has(constraint)) {
      const entry = open.get(constraint);
      if (entry) { entry.withdrawn = { revision: c.revision, author_id: c.author_id, at: c.created_at }; open.delete(constraint); }
    }
    previous = now;
  }
  const sorted = [...decisions].sort((a, b) => a.created_at.localeCompare(b.created_at) || a.turn - b.turn);
  for (const entry of all) {
    const constraint = INSTRUCTION + entry.text, end = entry.withdrawn?.revision ?? Number.POSITIVE_INFINITY;
    const first = sorted.find((d) => d.context_revision >= entry.revision && d.context_revision < end && held.get(d.context_revision)?.has(constraint));
    entry.read = first ? { run: first.run, turn: first.turn, at: first.created_at } : null;
  }
  return all;
}

// The state saved when a person withdraws an instruction: the version keeps everything else, and the newest remaining
// instruction leads the agent's next step again when the withdrawn one led it.
export function withoutInstruction<T extends { constraints: string[]; next_action: string }>(state: T, text: string): T | null {
  const constraint = INSTRUCTION + text;
  if (!state.constraints.includes(constraint)) return null;
  const constraints = state.constraints.filter((c) => c !== constraint);
  const newest = [...constraints].reverse().find(isInstruction);
  const led = state.next_action === `Follow the scientist's newest instruction: ${text}`.slice(0, 2048);
  const next_action = !led ? state.next_action
    : newest ? `Follow the scientist's newest instruction: ${newest.slice(INSTRUCTION.length)}`.slice(0, 2048)
    : "Continue with the question and constraints as saved.";
  return { ...state, constraints, next_action };
}

// The state saved when a scientist asks for changes: the question stays, the instruction joins
// the constraints and leads the agent's next step.
export function withInstruction<T extends { constraints: string[]; next_action: string }>(state: T, text: string): T | null {
  const clean = text.replace(/\s+/g, " ").trim();
  if (!clean || clean.length > MAX_INSTRUCTION || state.constraints.length >= 32) return null;
  return { ...state, constraints: [...state.constraints, `${INSTRUCTION}${clean}`], next_action: `Follow the scientist's newest instruction: ${clean}`.slice(0, 2048) };
}

// A readable description of an input reference, without its identifier.
export function inputWords(ref: string): { kind: string; name: string } {
  const match = /^input:\/\/([a-z_-]+)\/(.+)$/i.exec(ref);
  if (!match) return { kind: "Input", name: ref };
  const [, scheme, rest] = match;
  // A trailing date names the copy, as in toothgrowth-20260922.
  const leaf = rest.split("/").at(-1)!;
  const dated = /^(.*?)[_-](\d{4})(\d{2})(\d{2})$/.exec(leaf);
  const base = (dated ? dated[1] : leaf).replace(/[_-]+/g, " ");
  const name = dated ? `${base}, copy of ${dated[2]}-${dated[3]}-${dated[4]}` : base;
  const kind = scheme === "public" || scheme === "reference" ? "Public reference data"
    : scheme === "synthetic" ? "Synthetic example data"
    : scheme === "lab" ? "Lab data"
    : scheme === "paper" || scheme === "papers" ? "Paper"
    : "Input";
  return { kind, name: name.charAt(0).toUpperCase() + name.slice(1) };
}
