import assert from "node:assert/strict";
import test from "node:test";
import type { InboxItem } from "./inbox.ts";
import { landing, landingWords, RECENT_MS, type LandingProject } from "./landing.ts";

const now = Date.parse("2026-10-12T12:00:00Z");
const P = (n: number) => `50000000-0000-4000-8000-00000000000${n}`;
const project = (over: Partial<LandingProject>): LandingProject => ({
  project_id: P(1), standing: "finished", updated_at: "2026-10-12T11:00:00Z", mine: true, last_look_at: null, ...over });
const item = (over: Partial<InboxItem>): InboxItem => ({ kind: "review_request", ref: "r", version: "v", at: "2026-10-12T10:00:00Z",
  seen_at: null, lab: "open_methods", project_id: P(9), ...over });
const input = (over: Partial<Parameters<typeof landing>[0]>) => ({ waiting: [], projects: [], opened: {}, now, ...over });

test("an unseen request that stops an agent comes first, newest first", () => {
  const result = landing(input({
    waiting: [item({ ref: "a", at: "2026-10-12T09:00:00Z", project_id: P(7) }), item({ ref: "b", at: "2026-10-12T10:00:00Z", project_id: P(8) })],
    projects: [project({ standing: "working" })],
  }));
  assert.deepEqual(result, { route: { name: "project", project: P(8) }, reason: "needs_you", project_id: P(8) });
});

test("seen requests and library upkeep do not take over the first screen", () => {
  const result = landing(input({
    waiting: [item({ seen_at: "2026-10-12T10:05:00Z" }), item({ kind: "paper_acceptance", project_id: undefined }), item({ kind: "analysis_review" })],
  }));
  assert.equal(result, null);
});

test("a finished project opens when it changed after the reader's last look", () => {
  const looked = project({ last_look_at: "2026-10-12T10:00:00Z", updated_at: "2026-10-12T11:00:00Z" });
  assert.equal(landing(input({ projects: [looked] }))?.reason, "finished");
  const already = project({ last_look_at: "2026-10-12T11:30:00Z", updated_at: "2026-10-12T11:00:00Z" });
  assert.equal(landing(input({ projects: [already] })), null);
});

test("the browser's own record stands in for the broker's last look", () => {
  const p = project({ updated_at: "2026-10-12T11:00:00Z" });
  assert.equal(landing(input({ projects: [p], opened: { [P(1)]: "2026-10-12T11:10:00Z" } })), null);
  assert.equal(landing(input({ projects: [p], opened: { [P(1)]: "2026-10-12T10:10:00Z" } }))?.reason, "finished");
});

test("with no record of a look, only a recent finish counts", () => {
  const recent = project({ updated_at: new Date(now - RECENT_MS + 60_000).toISOString() });
  const old = project({ updated_at: new Date(now - RECENT_MS - 60_000).toISOString() });
  assert.equal(landing(input({ projects: [recent] }))?.reason, "finished");
  assert.equal(landing(input({ projects: [old] })), null);
});

test("a colleague's projects are never opened for the reader", () => {
  assert.equal(landing(input({ projects: [project({ mine: false }), project({ project_id: P(2), mine: false, standing: "working" })] })), null);
});

test("a working project opens when nothing waits or finished, the most recently opened first", () => {
  const result = landing(input({
    projects: [project({ project_id: P(1), standing: "working", updated_at: "2026-10-12T11:59:00Z" }),
      project({ project_id: P(2), standing: "working", updated_at: "2026-10-12T08:00:00Z" })],
    opened: { [P(2)]: "2026-10-12T11:00:00Z", [P(1)]: "2026-10-11T09:00:00Z" },
  }));
  assert.deepEqual(result, { route: { name: "project", project: P(2) }, reason: "working", project_id: P(2) });
});

test("a finished project leads a working one, and an unreadable time never lands", () => {
  const result = landing(input({ projects: [project({ project_id: P(1), standing: "working" }), project({ project_id: P(2) })] }));
  assert.equal(result?.project_id, P(2));
  assert.equal(landing(input({ projects: [project({ updated_at: "not a time" })] })), null);
});

test("the notice says why the workspace opened a project", () => {
  assert.equal(landingWords("finished", "Test diet response study"), "Opened Test diet response study: it finished since you last looked. Your research is in the sidebar.");
  assert.equal(landingWords("needs_you", "A"), "Opened A: the agent asks for your review. Your research is in the sidebar.");
  assert.equal(landingWords("working", "A"), "Opened A: the agent is working on it. Your research is in the sidebar.");
});
