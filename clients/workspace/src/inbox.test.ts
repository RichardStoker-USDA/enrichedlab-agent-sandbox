import assert from "node:assert/strict";
import test from "node:test";
import { acrossLabs, forLab, itemAction, itemHeading, itemKey, itemLink, itemSentence, kindOrder, newlyWaiting, noticeWords, ordered, tabTitle, unseen, unseenByLab, type InboxItem } from "./inbox.ts";

const item = (over: Partial<InboxItem>): InboxItem => ({ kind: "review_request", ref: "r", version: "v", at: "2026-09-26T09:00:00Z",
  seen_at: null, lab: "open_methods", project_title: "Test diet response study", ...over });
const labName = (key: string) => (key === "open_methods" ? "Example Nutrition Lab" : "a lab");

test("each kind reads as one plain sentence from the records, never inventing a reason", () => {
  assert.equal(itemSentence(item({ turn: 3 }), labName), "Test diet response study: the agent stopped at decision 3 and asks you to review before it continues.");
  assert.equal(itemSentence(item({ turn: 2, code: "turn_limit" }), labName), "Test diet response study: the agent reached the decision limit set for its work session.");
  assert.equal(itemSentence(item({ turn: 0 }), labName), "Test diet response study: the agent stopped before its first decision and asks you to review.");
  assert.equal(itemSentence(item({ kind: "step_check", title: "Check the units" }), labName),
    "Test diet response study: \"Check the units\" needs a check; the system could not confirm its result files.");
  assert.equal(itemSentence(item({ kind: "analysis_review" }), labName), "Test diet response study: an analysis finished and no person has reviewed its current results.");
  assert.equal(itemSentence(item({ kind: "paper_acceptance", papers: 1 }), labName), "One paper waits in the library of Example Nutrition Lab before the research agent may read it.");
  assert.equal(itemSentence(item({ kind: "paper_acceptance", papers: 3 }), labName), "3 papers wait in the library of Example Nutrition Lab before the research agent may read them.");
  assert.equal(itemSentence(item({ kind: "note_attention", state: "expired" }), labName), "A note shared in the lab passed its review date, so agents no longer read it.");
  assert.equal(itemSentence(item({ kind: "note_attention", state: "paused" }), labName), "A shared note's evidence changed since it was shared, so agents no longer read it.");
  assert.deepEqual(kindOrder, ["review_request", "step_check", "analysis_review", "paper_acceptance", "note_attention"]);
});

test("unseen items come first, newest first, and only they count", () => {
  const list = [item({ ref: "a", at: "2026-09-26T08:00:00Z" }), item({ ref: "b", at: "2026-09-26T10:00:00Z", seen_at: "2026-09-26T10:05:00Z" }),
    item({ ref: "c", at: "2026-09-26T09:00:00Z" })];
  assert.deepEqual(ordered(list).map((i) => i.ref), ["c", "a", "b"]);
  assert.equal(unseen(list), 2);
});

test("a lab lists only its own items; other labs' unseen items are only counted", () => {
  // In one lab, the FoodData Central library's paper waited in the menus.
  const items = [item({ lab: "fooddata-central", ref: "a", kind: "paper_acceptance", papers: 1 }), item({ lab: "plant-lab", ref: "b" }),
    item({ lab: "plant-lab", ref: "c", seen_at: "2026-09-26T10:00:00Z" }), item({ lab: "retained-byte-qualification", ref: "d" })];
  assert.deepEqual(forLab(items, "plant-lab").map((i) => i.ref), ["b", "c"]);
  assert.deepEqual(forLab(items, "fooddata-central").map((i) => i.ref), ["a"]);
  // An operator lab's items count only while that lab is chosen.
  assert.deepEqual(acrossLabs(items, "plant-lab").map((i) => i.ref), ["a", "b", "c"]);
  assert.deepEqual(acrossLabs(items, "retained-byte-qualification").map((i) => i.ref), ["a", "b", "c", "d"]);
  assert.deepEqual([...unseenByLab(acrossLabs(items, "plant-lab"))], [["fooddata-central", 1], ["plant-lab", 1]]);
});

// The home page and the Waiting page send a reader to the same place for an item.
test("each item links to where it is dealt with", () => {
  const base = { ref: "r", version: "1", at: "2026-10-11T00:00:00Z", seen_at: null, lab: "lab-a" };
  const project = "50000000-0000-4000-8000-000000000001", task = "10000000-0000-4000-8000-000000000001";
  assert.deepEqual(itemLink({ ...base, kind: "review_request", project_id: project }), { href: `#/projects/${project}`, label: "Open the project" });
  assert.deepEqual(itemLink({ ...base, kind: "step_check", project_id: project }), { href: `#/projects/${project}/steps`, label: "Open its steps" });
  assert.deepEqual(itemLink({ ...base, kind: "analysis_review", project_id: project, task_id: task }), { href: `#/projects/${project}/analyses/${task}`, label: "Open the analysis" });
  assert.equal(itemLink({ ...base, kind: "review_request" }), null);
  assert.equal(itemLink({ ...base, kind: "paper_acceptance" })?.label, "Open the library");
  assert.equal(itemLink({ ...base, kind: "note_attention" })?.label, "Open the shared notes");
});

test("only items that arrive after the first read are new, and a new version of a request is a new item", () => {
  const first = [item({ ref: "a" }), item({ ref: "b", seen_at: "2026-09-26T10:00:00Z" })];
  assert.deepEqual(newlyWaiting(null, first), []);
  const known = new Set(first.map(itemKey));
  const later = [...first, item({ ref: "c" }), item({ ref: "a", version: "v2" }), item({ ref: "d", seen_at: "2026-09-26T11:00:00Z" })];
  assert.deepEqual(newlyWaiting(known, later).map(itemKey), ["review_request:c:v", "review_request:a:v2"]);
});

test("a notice covers only what stops an agent, names no project or lab, and says when the agent is in another lab", () => {
  assert.equal(noticeWords([item({ kind: "analysis_review" }), item({ kind: "paper_acceptance" }), item({ kind: "note_attention" })], "open_methods"), null);
  const one = noticeWords([item({ turn: 3 }), item({ kind: "analysis_review" })], "open_methods");
  assert.deepEqual(one, { title: "Waiting for you", body: "A research agent stopped and asks you to review before it continues.", lab: "open_methods" });
  assert.equal(noticeWords([item({ kind: "step_check", title: "Plot the plasma markers" })], "open_methods")?.body,
    "A step needs your check: the system could not confirm its result files.");
  const many = noticeWords([item({ ref: "a" }), item({ ref: "b", kind: "step_check" })], "open_methods");
  assert.equal(many?.body, "2 requests from the research agents are waiting for you.");
  // An agent stopped in another lab still raises the notice, which says so and opens that lab.
  const away = noticeWords([item({ lab: "field_trials", turn: 2 })], "open_methods");
  assert.deepEqual(away, { title: "Waiting for you", body: "A research agent in another of your labs stopped and asks you to review before it continues.", lab: "field_trials" });
  const mixed = noticeWords([item({ ref: "a" }), item({ ref: "b", lab: "field_trials", at: "2026-09-26T11:00:00Z" })], "open_methods");
  assert.deepEqual(mixed, { title: "Waiting for you", body: "2 requests from the research agents are waiting for you. 1 of them is in your other labs.", lab: "open_methods" });
  const allAway = noticeWords([item({ ref: "a", lab: "field_trials" }), item({ ref: "b", lab: "soil_lab", at: "2026-09-26T11:00:00Z" })], "open_methods");
  assert.deepEqual(allAway, { title: "Waiting for you", body: "2 requests from the research agents are waiting for you. They are in your other labs.", lab: "soil_lab" });
  for (const words of [one, many, away, mixed, allAway]) {
    assert.ok(!JSON.stringify(words?.body).includes("Test diet response study") && !/field_trials|soil_lab|Field/.test(words?.body ?? ""));
  }
});

test("the tab title leads with the unseen count, then whether the agent works on the open project", () => {
  assert.equal(tabTitle("Soil moisture · Example Field Lab", 0, false), "Soil moisture · Example Field Lab");
  assert.equal(tabTitle("Soil moisture · Example Field Lab", 2, true), "(2) Working · Soil moisture · Example Field Lab");
  assert.equal(tabTitle("Example Field Lab", 1, false), "(1) Example Field Lab");
});

test("home's rows head each item with its project and say the rest without repeating it", () => {
  assert.equal(itemHeading(item({ turn: 3 }), labName), "Test diet response study");
  assert.equal(itemSentence(item({ turn: 3 }), labName, false), "The agent stopped at decision 3 and asks you to review before it continues.");
  assert.equal(itemSentence(item({ turn: 2, code: "turn_limit" }), labName, false), "The agent reached the decision limit set for its work session.");
  assert.equal(itemSentence(item({ kind: "step_check", title: "Check the units" }), labName, false), "\"Check the units\" needs a check; the system could not confirm its result files.");
  assert.equal(itemSentence(item({ kind: "analysis_review" }), labName, false), "An analysis finished and no person has reviewed its current results.");
  assert.equal(itemHeading(item({ kind: "paper_acceptance", papers: 2 }), labName), "Papers for the library of Example Nutrition Lab");
  assert.equal(itemHeading(item({ kind: "note_attention" }), labName), "A shared note");
  assert.deepEqual(itemAction(item({})), { label: "Review", primary: true });
  assert.deepEqual(itemAction(item({ kind: "step_check" })), { label: "Check", primary: true });
  assert.deepEqual(itemAction(item({ kind: "paper_acceptance" })), { label: "Open", primary: false });
});
