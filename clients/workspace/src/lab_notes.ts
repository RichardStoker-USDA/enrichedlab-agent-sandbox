import { BrokerError, request } from "./api.ts";
import type { LabNote, LabNotes, TaskReview } from "./contracts.ts";
import type { Tone } from "./language.ts";
import { byPerson } from "./person_review.ts";

// Lab memory in plain words. A shared note is something the agent noted in one project that a lab lead
// or reviewer shared after a person accepted the analyses it rests on. The agent reads active shared notes in the
// lab's other projects that start from the same data or use the same methods, as background and never as their
// results. Whether a note is in use is the broker's fixed rule; this page only words it.
export const noteClaim =
  "A lab lead or reviewer shares a note after a person accepts the analyses it rests on. The agent reads notes in use when another project starts from the same data or uses the same methods, as background, never as that project's results.";
export const MAX_SHARE_REASON = 600;
export const DEFAULT_KEEP_DAYS = 180;
export const keepChoices = [
  { days: 90, label: "3 months" },
  { days: 180, label: "6 months" },
  { days: 365, label: "1 year" },
  { days: 730, label: "2 years" },
] as const;

const UUID = /^[a-f0-9]{8}-[a-f0-9]{4}-[a-f0-9]{4}-[a-f0-9]{4}-[a-f0-9]{12}$/i;
const id = (value: string) => {
  if (!UUID.test(value)) throw new BrokerError("invalid_reference");
  return value;
};
const labPath = (key: string) => {
  if (!/^[a-z][a-z0-9_-]{1,63}$/.test(key)) throw new BrokerError("invalid_reference");
  return `/api/v1/labs/${key}/notes`;
};
const mutate = { "Content-Type": "application/json", "X-Agent-Sandbox-Intent": "mutate" };

export const labNotes = {
  list: (lab: string, signal?: AbortSignal) => request<LabNotes>(labPath(lab), { signal }),
  share: async (project: string, body: { revision: number; statement: string; evidence_refs: string[];
    reason: string | null; review_days: number; request_id: string }) =>
    (await request<{ note: LabNote }>(`/api/v1/projects/${id(project)}/notes/share`, { method: "POST", headers: mutate, body: JSON.stringify(body) })).note,
  retire: async (note: string, body: { reason: string; request_id: string }) =>
    (await request<{ note: LabNote }>(`/api/v1/notes/${id(note)}/retire`, { method: "POST", headers: mutate, body: JSON.stringify(body) })).note,
  renew: async (note: string, body: { review_days: number; request_id: string }) =>
    (await request<{ note: LabNote }>(`/api/v1/notes/${id(note)}/renew`, { method: "POST", headers: mutate, body: JSON.stringify(body) })).note,
};

export function day(value: string | null | undefined): string {
  if (!value) return "";
  const date = new Date(value);
  return Number.isNaN(date.getTime()) ? "" : date.toLocaleDateString(undefined, { month: "short", day: "numeric", year: "numeric" });
}

// A note's state as the broker decided it, in words.
export function noteStatus(note: LabNote): { label: string; tone: Tone; sentence: string } {
  const s = note.status;
  switch (s.state) {
    case "active":
      return { label: "In use", tone: "done", sentence: `The agent reads it in matching projects until ${day(s.review_by)}.` };
    case "paused":
      return { label: "Paused", tone: "attention", sentence: s.cause === "evidence_files_changed"
        ? "The result files of an analysis it rests on changed after it was shared, so the agent does not read it."
        : "A person's newer review of an analysis it rests on is not an acceptance, so the agent does not read it." };
    case "expired":
      return { label: "Past its review date", tone: "quiet", sentence: `Its review date passed on ${day(s.review_by)}. Renew it to let the agent read it again.` };
    case "retired":
      return { label: "Retired", tone: "quiet", sentence: `Retired on ${day(s.retired_at)}${s.reason ? `: ${s.reason}` : "."}` };
  }
}

// The standing share of a project's note: the newest one from this project with the same words that is not retired.
export function sharedAs(statement: string, project: string, notes: LabNote[]): LabNote | null {
  return notes.find((n) => n.project_id === project && n.statement === statement && n.status.state !== "retired") ?? null;
}

const taskOf = (ref: string) => /^artifact:\/\/tasks\/([0-9a-f-]{36})\//i.exec(ref)?.[1] ?? null;

// Why a note cannot be shared yet, or "" when it can: it must be an observation resting on at least one analysis of
// the project, and each such analysis's newest review must be a person's acceptance of its current files. The broker
// checks the same rule and is the authority; this keeps the offer honest.
export function shareBlocker(lesson: { kind: string; evidence_refs: string[] }, reviews: Record<string, TaskReview>,
  titleFor: (task: string) => string): string {
  if (lesson.kind !== "observation") return "Only an observation can be shared with the lab.";
  const tasks = [...new Set(lesson.evidence_refs.map(taskOf).filter((t): t is string => !!t))];
  if (!tasks.length) return "Only a note that rests on an analysis can be shared with the lab.";
  const waiting = tasks.filter((t) => {
    const r = reviews[t];
    return !r || !byPerson(r) || r.decision !== "accepted" || !r.covers_current_outputs;
  });
  if (!waiting.length) return "";
  const names = waiting.map((t) => `“${titleFor(t)}”`);
  const list = names.length === 1 ? names[0] : `${names.slice(0, -1).join(", ")} and ${names.at(-1)}`;
  return `A person must accept ${list} before this note can be shared with the lab.`;
}

// The shared notes a decision's context held, oldest record first as the turn recorded them, each matched to the lab's
// list; a note no longer listed is named as such.
export function notesRead(record: { context_inputs?: { lab_notes?: { note_id: string }[] } } | undefined,
  notes: LabNote[]): { note_id: string; note: LabNote | null }[] {
  return (record?.context_inputs?.lab_notes ?? []).map((r) => ({ note_id: r.note_id, note: notes.find((n) => n.note_id === r.note_id) ?? null }));
}

// What a person may do with a note: retire it (its sharer or a lab lead or reviewer) and renew it (a lead or reviewer),
// never once it is retired.
export function noteActions(note: LabNote, reader: string, canCurate: boolean): { retire: boolean; renew: boolean } {
  const open = note.status.state !== "retired";
  return { retire: open && (canCurate || note.shared_by.principal_id === reader), renew: open && canCurate };
}

export function reasonProblem(reason: string, required: boolean): string {
  const clean = reason.trim();
  if (required && !clean) return "Say why it should no longer be used.";
  if (clean.length > MAX_SHARE_REASON) return `Keep it to ${MAX_SHARE_REASON} characters or fewer.`;
  return "";
}

// Why a share, retirement or renewal was refused, from the broker's fixed code. Nothing changed in each case.
export function noteFailure(error: unknown, action: "share" | "retire" | "renew"): string {
  const reason = error instanceof BrokerError ? error.reason : "";
  switch (reason) {
    case "note_evidence_unaccepted": return "A person's acceptance no longer covers every analysis this note rests on. Reload the page to see the current reviews.";
    case "note_already_shared": return "This note is already shared with the lab.";
    case "note_stale": return "The project changed after this page loaded. Reload the page and share the note again.";
    case "note_project_private": return "Notes from a private project stay in it. Only notes from a project the lab can view can be shared.";
    case "note_person_only": return "Only a person's account can change the lab's shared notes.";
    case "note_forbidden":
      return action === "retire" ? "Only the person who shared this note or a lead or reviewer of the lab can retire it."
        : "Only a lead or reviewer of this lab can do this.";
    case "note_invalid": return action === "share" ? "The note no longer matches the saved project. Reload the page and try again." : "Check the form and try again.";
    case "note_retired": return "This note is already retired.";
    case "note_not_found": case "lab_not_found": return "This note is not available to your account.";
  }
  return `Nothing changed; the ${action === "share" ? "note was not shared" : action === "retire" ? "note was not retired" : "note was not renewed"}. Try again shortly.`;
}
