// Where the workspace opens (Glass Box rule 1): on the reader's own work when there is some. The order is
// one rule in one place, so the owner can change it later without touching the pages: what needs the reader, then their
// project that finished since they last looked, then their project that is working, most recently opened first. With
// none of these, home opens on the question field.
import type { InboxItem } from "./inbox.ts";
import type { Route } from "./route.ts";
import type { Standing } from "./standing.ts";

export interface LandingProject {
  project_id: string;
  standing: Standing;
  updated_at: string;
  /** The reader started it. Only the reader's own projects are opened for them; a colleague's stays in its lists. */
  mine: boolean;
  /** When the reader last opened it, from the broker's record when it keeps one. */
  last_look_at: string | null;
}

export interface LandingInput {
  waiting: InboxItem[];
  projects: LandingProject[];
  /** When this browser last opened each project, a convenience record for brokers without a last look. */
  opened: Record<string, string>;
  now: number;
}

export type Landing = { route: Route; reason: "needs_you" | "finished" | "working"; project_id: string } | null;

/** Without any record of a look, "since they last looked" means within this window. */
export const RECENT_MS = 24 * 3_600_000;

const time = (value: string | null | undefined) => {
  const t = value ? Date.parse(value) : NaN;
  return Number.isFinite(t) ? t : null;
};

export function landing(input: LandingInput): Landing {
  // What stops an agent until the reader acts, newest first. A seen request still waits, but the reader already knows
  // about it, so it stays in Needs you on home instead of taking over the first screen.
  const asking = input.waiting
    .filter((i) => !i.seen_at && (i.kind === "review_request" || i.kind === "step_check") && i.project_id)
    .sort((a, b) => (time(b.at) ?? 0) - (time(a.at) ?? 0));
  if (asking.length) {
    const project = asking[0].project_id as string;
    return { route: { name: "project", project }, reason: "needs_you", project_id: project };
  }
  const mine = input.projects.filter((p) => p.mine);
  const looked = (p: LandingProject) => time(p.last_look_at) ?? time(input.opened[p.project_id]);
  const fresh = (p: LandingProject) => {
    const changed = time(p.updated_at);
    if (changed === null) return false;
    const seen = looked(p);
    return seen === null ? input.now - changed < RECENT_MS : changed > seen;
  };
  const finished = mine.filter((p) => p.standing === "finished" && fresh(p))
    .sort((a, b) => (time(b.updated_at) ?? 0) - (time(a.updated_at) ?? 0));
  if (finished.length) {
    const project = finished[0].project_id;
    return { route: { name: "project", project }, reason: "finished", project_id: project };
  }
  const working = mine.filter((p) => p.standing === "working")
    .sort((a, b) => (looked(b) ?? 0) - (looked(a) ?? 0) || (time(b.updated_at) ?? 0) - (time(a.updated_at) ?? 0));
  if (working.length) {
    const project = working[0].project_id;
    return { route: { name: "project", project }, reason: "working", project_id: project };
  }
  return null;
}

/** One sentence for the notice that says why the workspace opened a project instead of home. */
export function landingWords(reason: NonNullable<Landing>["reason"], title: string): string {
  switch (reason) {
    case "needs_you": return `Opened ${title}: the agent asks for your review. Your research is in the sidebar.`;
    case "finished": return `Opened ${title}: it finished since you last looked. Your research is in the sidebar.`;
    case "working": return `Opened ${title}: the agent is working on it. Your research is in the sidebar.`;
  }
}
