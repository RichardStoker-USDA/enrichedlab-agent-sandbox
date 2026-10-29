import assert from "node:assert/strict";
import { test } from "node:test";
import { contextSummary, type SummaryItem } from "./context_summary.ts";

// A synthetic lead context in the version 7 shape the coordinator compiles.
const lead = {
  context_schema_version: 7,
  question: "Does the test diet change plasma response?",
  constraints: ["Use the reference cohort only"],
  scientist_instructions: ["Compare medians as well"],
  starting_point: { data: ["research-source://reference/a"], kind: "chosen_from_catalog" },
  methods: [
    { key: "reference-r-mean", name: "Mean by group", description: "d" },
    { key: "configured-r-bootstrap-v2", name: "Bootstrap interval", settings: { grouping: {} } },
    { key: "generated-r-analysis-v2", name: "Written R analysis", template: { contract: {} } },
  ],
  earlier_readings: { lessons: [{ statement: "s", kind: "hypothesis" }] },
  analyses: [
    { key: "a", title: "Summarize each diet", status: "completed", results: [{ ref: "artifact://x", digest_sha256: "d" }], review: { decision: "accepted" } },
    { key: "b", title: "Bootstrap the difference", status: "failed" },
    { key: "c", title: "Median check", status: "completed", review: { decision: "rejected" }, results_withheld: "a person rejected this analysis" },
  ],
  results_omitted: ["artifact://y"],
  results_omitted_more: 2,
  latest_answer: { recorded_at: "2026-09-26T00:00:00Z", answer: "partly_answered", findings: 2 },
  earlier_findings: { findings: [{ statement: "One {a}.", bindings: {}, values: { a: "12.5" } }, { statement: "Two.", withdrawn: "a person rejected an analysis this finding reads" }] },
  lab_notes: [{ note: "n1", statement: "Samples thawed twice read lower." }],
  lab_notes_withheld: 1,
  literature: [
    { ref: "library://p1/c1", paper: "Synthetic handling study", year: 2019, pages: "2", text: "t" },
    { ref: "library://p1/c2", paper: "Synthetic handling study", year: 2019, pages: "3", text: "t" },
    { ref: "library://p2/c1", paper: "Second synthetic paper", pages: "1", text: "t" },
  ],
  literature_withheld: 2,
  prior_attempt: { refused: "proposal_prose_number", failures: 1 },
};
const item = (items: SummaryItem[] | null, label: string) => items?.find((i) => i.label === label);

test("the lead's input is summarized part by part, in plain words", () => {
  const items = contextSummary(JSON.stringify(lead));
  assert.ok(items);
  assert.deepEqual(items.map((i) => i.label), ["Your question", "Your constraints", "Your newest instructions", "Data", "Methods it could use",
    "Analyses so far", "Its latest answer", "Earlier findings", "Saved lessons", "Notes shared in the lab", "Papers from the lab's library",
    "A refused try in this work session"]);
  assert.equal(item(items, "Your question")?.text, "Does the test diet change plasma response?");
  assert.deepEqual(item(items, "Your constraints")?.list, ["Use the reference cohort only"]);
  assert.equal(item(items, "Data")?.text, "The data chosen for this project");
  assert.deepEqual(item(items, "Methods it could use")?.list, ["Mean by group, a fixed reviewed recipe", "Bootstrap interval, a recipe with settings",
    "Written R analysis, code the agent writes, checked before it runs"]);
  assert.equal(item(items, "Analyses so far")?.text,
    "3 analyses: 2 finished, 1 did not finish; it read the results of 1; 3 results left out to stay within the size limit");
  assert.deepEqual(item(items, "Analyses so far")?.list, ["Summarize each diet, finished, accepted by a person", "Bootstrap the difference, did not finish",
    "Median check, finished, rejected by a person, so its results were held back"]);
  assert.equal(item(items, "Its latest answer")?.text, "Last time it said the results answer part of your question, with 2 findings");
  assert.equal(item(items, "Earlier findings")?.text, "2 findings it stated before; 1 withdrawn because a person rejected an analysis it read");
  assert.equal(item(items, "Notes shared in the lab")?.text, "1 note; 1 held back because it read like instructions to an AI");
  assert.equal(item(items, "Papers from the lab's library")?.text, "3 passages from 2 papers; 2 held back because they read like instructions to an AI");
  assert.deepEqual(item(items, "Papers from the lab's library")?.list, ["Synthetic handling study (2019), 2 passages", "Second synthetic paper"]);
  assert.equal(item(items, "A refused try in this work session")?.text,
    "Its previous reply was refused (the reply wrote a number that did not come from a result file), so it had to change it");
});

test("a summary never restates a result value", () => {
  const text = JSON.stringify(contextSummary(JSON.stringify({ ...lead, proposal: { action: "delegate", answer: "answered", findings: [{}], actions: [] },
    finding_values: [{ a: "12.5", b: "0.031" }] })));
  assert.ok(!text.includes("12.5") && !text.includes("0.031"));
});

test("the methods check's input leads with the proposal it checked", () => {
  const items = contextSummary(JSON.stringify({ ...lead, proposal: { action: "delegate", answer: "not_yet", findings: [], actions: [{ title: "Median by diet" }, { title: "Range check" }] },
    finding_values: [{ a: "1", b: "2" }, { c: "3" }] }));
  assert.equal(items?.[0].label, "The research agent's proposal");
  assert.equal(items?.[0].text, "Run 2 new analyses; it does not say yet whether the results answer your question");
  assert.deepEqual(items?.[0].list, ["Median by diet", "Range check"]);
  assert.equal(item(items, "Values it checked")?.text, "3 values read from the result tables, to check each finding");
  assert.equal(item(items, "Your question")?.text, lead.question);
});

test("a lab not listed for papers reads none, and a prepared starting point says so", () => {
  const items = contextSummary(JSON.stringify({ context_schema_version: 7, question: "q", starting_point: { data: [], kind: "prepared", prepared_reference: "r" } }));
  assert.equal(item(items, "Papers from the lab's library")?.text, "None");
  assert.equal(item(items, "Data")?.text, "A prepared data set");
  assert.equal(item(items, "Analyses so far"), undefined);
});

test("text that is not a decision's input has no summary", () => {
  assert.equal(contextSummary("Synthetic preview instructions for the research lead."), null);
  assert.equal(contextSummary(JSON.stringify({ unrelated: true })), null);
  assert.equal(contextSummary("[1,2]"), null);
});

test("the agent's account of the question's parts and its refused replies are counted, never quoted", () => {
  // The parts are the agent's own words, so the summary counts them by state.
  const items = contextSummary(JSON.stringify({ context_schema_version: 7, question: "q",
    answerability: { turn: 2, answer: "partly_answered", parts: [{ part: "Which diet raised it most", state: "supported" },
      { part: "In people", state: "unsupported", reason: "Only animal data." }, { part: "By dose", state: "open" }] },
    attempts: [{ turn: 3, refused: "proposal_parts_invalid" }, { turn: 2, refused: "proposal_prose_number" }],
    proposal: { action: "review_required", answer: "partly_answered", findings: [{}],
      parts: [{ part: "Which diet raised it most", state: "supported" }, { part: "In people", state: "unsupported" }] } }));
  assert.equal(item(items, "Its earlier account of your question's parts")?.text,
    "From an earlier reply in this work session, 3 parts: 1 answered, 1 not in this data, 1 still being worked on");
  assert.equal(item(items, "Refused replies in this work session")?.text, "2 replies refused, newest first");
  assert.equal(item(items, "Refused replies in this work session")?.list?.length, 2);
  assert.equal(item(items, "Parts of your question it accounts for")?.text, "2 parts: 1 answered, 1 not in this data");
  const text = JSON.stringify(items);
  assert.ok(!text.includes("Which diet") && !text.includes("Only animal data") && !text.includes("In people"));
  const counted = contextSummary(JSON.stringify({ context_schema_version: 7, question: "q",
    answerability: { turn: 2, answer: "not_yet", parts_omitted: 6 } }));
  assert.equal(item(counted, "Its earlier account of your question's parts")?.text, "6 parts, left out to stay within the size limit");
});

test("a refusal is named in plain words, never by its code", () => {
  // A refusal the workspace had no words for showed its code.
  const codes = ["proposal_query_repeated", "sql_refused_unordered", "proposal_some_future_rule", "code_dynamic_call"];
  const items = contextSummary(JSON.stringify({ context_schema_version: 9, question: "q",
    attempts: codes.map((refused, i) => ({ turn: i + 1, refused })), prior_attempt: { refused: "proposal_some_future_rule", failures: 1 } }));
  assert.deepEqual(item(items, "Refused replies in this work session")?.list, [
    "the reply asked to run a database query an analysis already ran", "the platform's check refused the database query",
    "the checks refused it for a reason without plain words yet", "the platform's check refused the model-written code"]);
  assert.ok(!JSON.stringify(items).includes("proposal_some_future_rule") && !JSON.stringify(items).includes("sql_refused"));
});

test("the literature specialist's claims count as what the lead read from the library", () => {
  // The claims replace the passages in the lead's context.
  const { literature: _passages, literature_withheld: _held, ...rest } = lead;
  const items = contextSummary(JSON.stringify({ ...rest,
    literature_claims: [
      { claim: "Boiling lowered it.", ref: "library://p1/c1", paper: "Synthetic handling study", year: 2019, relevance: "direct", confidence: "high" },
      { claim: "Steaming kept more.", ref: "library://p1/c2", paper: "Synthetic handling study", year: 2019, relevance: "related", confidence: "medium" },
    ],
    literature_off_topic: 1, literature_claims_withheld: 1 }));
  assert.equal(item(items, "Papers from the lab's library")?.text,
    "2 claims from 1 paper; read from the passages by the literature specialist; 1 passage it found off the question; 1 held back because it read like instructions to an AI");
  assert.deepEqual(item(items, "Papers from the lab's library")?.list, ["Synthetic handling study (2019), 2 claims"]);
});

test("the facts about the queries are counted, with the values the data does not hold", () => {
  // The methods check reads them with the proposal; the agent reads those of the proposal sent back.
  const query_facts = [{ step: "q-1", title: "Vitamin C by food", facts: [
    { kind: "entry", column: "nutrient.name", value: "Vitamin C", found: false, nearest: [] },
    { kind: "join_drop", from: "food.food_category_id", to: "food_category.id", rows: 3, matched: 2, dropped: 1 }] }];
  const forCheck = contextSummary(JSON.stringify({ ...lead, proposal: { action: "delegate", findings: [], actions: [] }, query_facts }));
  assert.equal(item(forCheck, "Facts about its queries")?.text, "2 facts from the data's schema card about 1 query step; 1 value the data does not hold");
  assert.deepEqual(item(forCheck, "Facts about its queries")?.list, ["Vitamin C by food"]);
  const forAgent = contextSummary(JSON.stringify({ ...lead, query_facts, query_facts_omitted: 2 }));
  assert.equal(item(forAgent, "Facts about the queries the check sent back")?.text,
    "2 facts from the data's schema card about 1 query step; 1 value the data does not hold; 2 left out to stay within the size limit");
});
