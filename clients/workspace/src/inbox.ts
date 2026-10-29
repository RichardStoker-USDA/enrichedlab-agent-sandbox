// What waits for each person: the broker computes the reader's items across their labs; a person
// marks one as seen. Words for each kind live here so the page and its tests read the same sentences.
import { request } from "./api.ts";
import { operatorLab } from "./context.ts";
import { href } from "./route.ts";

export type InboxKind = "review_request" | "analysis_review" | "step_check" | "paper_acceptance" | "note_attention";

export interface InboxItem {
  kind: InboxKind;
  ref: string;
  version: string;
  at: string;
  seen_at: string | null;
  lab: string;
  project_id?: string;
  project_title?: string;
  run_id?: string;
  code?: string;
  turn?: number;
  reason_source?: "methods_check" | "lead" | null;
  reason?: string | null;
  task_id?: string;
  action_key?: string;
  title?: string;
  papers?: number;
  note_id?: string;
  state?: string;
  cause?: string | null;
  review_by?: string;
}

export interface Inbox { can_acknowledge: boolean; items: InboxItem[] }

/** The items the workspace lists and counts in the lab a reader is in: that lab's own. Another lab's items show only
 *  as a count beside its name in the lab menu (an FDC paper once waited in another lab's menus). */
export const forLab = (items: InboxItem[], current: string) => items.filter((i) => i.lab === current);

/** What all of a reader's labs hold for them: every research lab's items, and an operator lab's only while it is the
 *  chosen lab, so qualification runs never crowd a scientist's counts. The lab menu counts these per lab, and a
 *  stopped agent in any of them may raise a notice. */
export const acrossLabs = (items: InboxItem[], current: string) => items.filter((i) => !operatorLab(i.lab) || i.lab === current);

/** Unseen items per lab, for the lab menu. */
export function unseenByLab(items: InboxItem[]): Map<string, number> {
  const counts = new Map<string, number>();
  for (const i of items) if (!i.seen_at) counts.set(i.lab, (counts.get(i.lab) ?? 0) + 1);
  return counts;
}
export interface Seen { run_id: string; principal_id: string; display_name: string | null; seen_at: string }

export const inbox = {
  list: (signal?: AbortSignal) => request<Inbox>("/api/v1/inbox", { signal }),
  acknowledge: (item: InboxItem, requestId: string, signal?: AbortSignal) =>
    request<{ kind: string; ref: string; version: string; seen_at: string }>("/api/v1/inbox/acknowledgements", {
      method: "POST", headers: { "Content-Type": "application/json", "X-Agent-Sandbox-Intent": "mutate" },
      body: JSON.stringify({ kind: item.kind, ref: item.ref, version: item.version, request_id: requestId }), signal }),
  // Who saw each of a project's review requests.
  seen: async (project: string, signal?: AbortSignal) =>
    (await request<{ acknowledgements: Seen[] }>(`/api/v1/projects/${project}/acknowledgements`, { signal })).acknowledgements,
};

// The order the page lists kinds in: what the agent is waiting on first, then what the lab is waiting on.
export const kindOrder: InboxKind[] = ["review_request", "step_check", "analysis_review", "paper_acceptance", "note_attention"];

export const kindHeading: Record<InboxKind, string> = {
  review_request: "The agent asks for your review",
  step_check: "Steps that need a check",
  analysis_review: "Results no person has reviewed",
  paper_acceptance: "Papers waiting for your acceptance",
  note_attention: "Shared notes to renew or retire",
};

// One plain sentence per item. Titles come from the records; the agent's own words are shown apart, labelled. Home's
// rows carry the project's title as their heading, so they ask for the sentence without it.
export function itemSentence(item: InboxItem, labName: (key: string) => string, withProject = true): string {
  const project = item.project_title ?? "A project";
  const agent = withProject ? `${project}: the agent` : "The agent";
  switch (item.kind) {
    case "review_request":
      return item.code === "turn_limit" ? `${agent} reached the decision limit set for its work session.`
        : item.turn ? `${agent} stopped at decision ${item.turn} and asks you to review before it continues.`
        : `${agent} stopped before its first decision and asks you to review.`;
    case "step_check":
      return `${withProject ? `${project}: ` : ""}"${item.title ?? "A step"}" needs a check; the system could not confirm its result files.`;
    case "analysis_review":
      return withProject ? `${project}: an analysis finished and no person has reviewed its current results.`
        : "An analysis finished and no person has reviewed its current results.";
    case "paper_acceptance": {
      const n = item.papers ?? 0;
      return `${n === 1 ? "One paper waits" : `${n} papers wait`} in the library of ${labName(item.lab)} before the research agent may read ${n === 1 ? "it" : "them"}.`;
    }
    case "note_attention":
      return item.state === "expired" ? "A note shared in the lab passed its review date, so agents no longer read it."
        : "A shared note's evidence changed since it was shared, so agents no longer read it.";
  }
}

// The heading of an item's row on home: its project, or the place the item belongs to.
export function itemHeading(item: InboxItem, labName: (key: string) => string): string {
  if (item.kind === "paper_acceptance") return `Papers for the library of ${labName(item.lab)}`;
  if (item.kind === "note_attention") return "A shared note";
  return item.project_title ?? "A project";
}

// The word on an item's button: what the reader does there. What stops an agent is the one filled button.
export const itemAction = (item: InboxItem): { label: string; primary: boolean } =>
  item.kind === "review_request" ? { label: "Review", primary: true }
  : item.kind === "step_check" ? { label: "Check", primary: true }
  : item.kind === "analysis_review" ? { label: "Review", primary: false }
  : { label: "Open", primary: false };

// Where each item is dealt with, so the home page and the Waiting page send a reader to the same place.
export function itemLink(item: InboxItem): { href: string; label: string } | null {
  switch (item.kind) {
    case "review_request": return item.project_id ? { href: href({ name: "project", project: item.project_id }), label: "Open the project" } : null;
    case "step_check": return item.project_id ? { href: href({ name: "project", project: item.project_id, section: "steps" }), label: "Open its steps" } : null;
    case "analysis_review": return item.project_id && item.task_id ? { href: href({ name: "analysis", project: item.project_id, task: item.task_id }), label: "Open the analysis" } : null;
    case "paper_acceptance": return { href: href({ name: "library" }), label: "Open the library" };
    case "note_attention": return { href: href({ name: "notes" }), label: "Open the shared notes" };
  }
}

// Unseen items first, newest first; seen items after them.
export function ordered(items: InboxItem[]): InboxItem[] {
  return [...items].sort((a, b) => Number(!!a.seen_at) - Number(!!b.seen_at) || b.at.localeCompare(a.at) || a.ref.localeCompare(b.ref));
}

export const unseen = (items: InboxItem[]) => items.filter((i) => !i.seen_at).length;

// An item's identity: a new version of the same request is a new item.
export const itemKey = (item: InboxItem) => `${item.kind}:${item.ref}:${item.version}`;

// Unseen items that were not waiting at the last read. The first read only sets the baseline, so a page opened on a
// full list announces nothing.
export function newlyWaiting(before: ReadonlySet<string> | null, items: InboxItem[]): InboxItem[] {
  if (!before) return [];
  return items.filter((i) => !i.seen_at && !before.has(itemKey(i)));
}

// A browser notice names no project or lab, because a lock screen shows it to anyone nearby. It covers only what stops
// an agent until a person acts, in any of the reader's labs, since an agent waits until someone answers; finished
// results and library or note upkeep wait in the list and the tab title. A request from another lab says so, and `lab`
// is where a click takes the reader: the lab in view when a request waits there, else the newest request's lab.
export function noticeWords(fresh: InboxItem[], current: string): { title: string; body: string; lab: string } | null {
  const asking = fresh.filter((i) => i.kind === "review_request" || i.kind === "step_check");
  if (!asking.length) return null;
  const away = asking.filter((i) => i.lab !== current);
  const lab = away.length < asking.length ? current : [...away].sort((a, b) => b.at.localeCompare(a.at))[0].lab;
  if (asking.length > 1) {
    const where = !away.length ? "" : away.length === asking.length ? " They are in your other labs."
      : ` ${away.length} of them ${away.length === 1 ? "is" : "are"} in your other labs.`;
    return { title: "Waiting for you", body: `${asking.length} requests from the research agents are waiting for you.${where}`, lab };
  }
  const elsewhere = asking[0].lab !== current ? " in another of your labs" : "";
  return { title: "Waiting for you", lab, body: asking[0].kind === "review_request"
    ? `A research agent${elsewhere} stopped and asks you to review before it continues.`
    : `A step${elsewhere} needs your check: the system could not confirm its result files.` };
}

// The tab leads with the unseen count, then whether the agent works on the open project, so both read from the tab
// strip while the person is elsewhere.
export const tabTitle = (base: string, waiting: number, working: boolean) =>
  `${waiting > 0 ? `(${waiting}) ` : ""}${working ? "Working · " : ""}${base}`;
