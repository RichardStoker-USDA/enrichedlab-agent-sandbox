import assert from "node:assert/strict";
import { test } from "node:test";
import { BrokerError } from "./api.ts";
import type { LibraryPaper } from "./contracts.ts";
import { acceptAllWords, acceptanceWords, addedWords, batchItemWords, batchWords, byline, libraryOutcomeUnknown, settleAcceptance, settleUploads, checkedWords, fileProblem, labelProblem, labelTitle, library, libraryFailure, paperReadWords, papersCited, papersRead, parseWords, queryProblem, unacceptedWords } from "./library.ts";
import type { ResearchTurn } from "./projects.ts";

const paper = (over: Partial<LibraryPaper> = {}): LibraryPaper => ({
  paper_id: "80000000-0000-4000-8000-000000000001", lab: "open_methods", title: "A paper", authors: ["Ada Example"], year: 2025,
  doi: null, page_count: 9, byte_size: 1000, source_sha256: "1".repeat(64), parse: { chunks: 42, figures: 3 },
  added_by: { name: "Lab Lead", kind: "person", you: false }, added_at: "2026-09-25T10:00:00Z", labels: [], removed: null,
  acceptance: null, can_remove: false, can_accept: false, ...over,
});

test("only a PDF within the size limit can be added", () => {
  assert.equal(fileProblem({ name: "paper.pdf", type: "application/pdf", size: 10 }), "");
  assert.equal(fileProblem({ name: "paper.pdf", type: "", size: 10 }), "");
  assert.equal(fileProblem({ name: "notes.docx", type: "application/vnd.openxmlformats-officedocument.wordprocessingml.document", size: 10 }), "Choose a PDF file.");
  assert.equal(fileProblem({ name: "paper.pdf", type: "application/pdf", size: 0 }), "This file is empty.");
  assert.match(fileProblem({ name: "paper.pdf", type: "application/pdf", size: 101 * 1024 * 1024 }), /larger than the library accepts/);
});

test("a byline names at most three authors and the year", () => {
  assert.equal(byline(paper()), "Ada Example · 2025");
  assert.equal(byline(paper({ authors: ["A", "B", "C", "D", "E"], year: null })), "A, B, C and 2 more");
  assert.equal(byline(paper({ authors: [], year: null })), "");
});

test("who added a paper is said plainly, and a service account is named as one", () => {
  const day = (v: string) => v.slice(0, 10);
  assert.equal(addedWords(paper(), day), "Added by Lab Lead on 2026-09-25.");
  assert.equal(addedWords(paper({ added_by: { name: "Me", kind: "person", you: true } }), day), "Added by you on 2026-09-25.");
  assert.equal(addedWords(paper({ added_by: { name: "Qualification caller", kind: "service_account", you: false } }), day),
    "Added by a service account on 2026-09-25.");
});

test("a paper's acceptance for the research agent names who accepted it and when", () => {
  const day = (v: string) => v.slice(0, 10);
  assert.equal(acceptanceWords(paper(), day), "Not accepted for the research agent yet.");
  assert.equal(acceptanceWords(paper({ acceptance: { accepted_at: "2026-09-26T08:00:00Z", by: "Lab Lead" } }), day),
    "Accepted for the research agent by Lab Lead on 2026-09-26.");
});

test("the papers waiting for acceptance are counted in plain words", () => {
  assert.equal(unacceptedWords(1), "1 paper is not accepted for the research agent yet.");
  assert.equal(unacceptedWords(3), "3 papers are not accepted for the research agent yet.");
});

test("a proposed label says a person can set it", () => {
  assert.equal(labelTitle({ namespace: "design", value: "crossover trial", state: "proposed", since: "" }),
    "Study design: crossover trial. Proposed by a service account; a person can set it.");
  assert.equal(labelTitle({ namespace: "topic", value: "satiety", state: "set", since: "" }), "Topic: satiety.");
});

test("what the parse found is counts, never text", () => {
  assert.equal(parseWords(paper()), "9 pages · 3 figures found · 42 searchable passages");
  assert.equal(parseWords(paper({ page_count: 1, parse: { chunks: 1, figures: 1 } })), "1 page · 1 figure found · 1 searchable passage");
  assert.equal(parseWords(paper({ page_count: null, parse: {} })), "");
});

test("labels and searches are checked before they are sent", () => {
  assert.equal(labelProblem("  "), "Write the label.");
  assert.match(labelProblem("x".repeat(81)), /80 characters/);
  assert.equal(labelProblem("tooth length"), "");
  assert.equal(queryProblem("a"), "Search for at least two characters.");
  assert.equal(queryProblem("tooth"), "");
});

test("a library request never names a malformed lab or paper", () => {
  assert.throws(() => library.list("Bad Lab"), BrokerError);
  assert.throws(() => library.paper("open_methods", "not-a-uuid"), BrokerError);
});

test("each refusal code has its own plain sentence and nothing is said to have changed", () => {
  const refused = (reason: string) => new BrokerError("request_failed", 409, reason);
  assert.match(libraryFailure(refused("library_not_configured"), "add"), /not set up for this lab/);
  assert.match(libraryFailure(refused("library_parse_failed"), "add"), /could not be read as a paper/);
  assert.match(libraryFailure(refused("library_forbidden"), "remove"), /Only a lead of the lab or the person who added/);
  assert.match(libraryFailure(refused("label_person_only"), "label"), /Only a person's account/);
  assert.match(libraryFailure(refused("paper_removed"), "label"), /restore it/);
  assert.match(libraryFailure(refused("library_forbidden"), "accept"), /Only a lead or reviewer of the lab can accept/);
  assert.equal(libraryFailure(refused("library_invalid"), "accept"), "Say why the acceptance is withdrawn.");
  assert.equal(libraryFailure(new Error("x"), "accept"), "The acceptance was not saved. Try again shortly.");
  assert.equal(libraryFailure(new BrokerError("connection_unavailable"), "search"),
    "The connection was lost before the library answered. Nothing is lost; try again.");
  assert.equal(libraryFailure(new Error("x"), "add"), "The paper was not added. Try again shortly.");
});

// A decision that read three passages from two papers and cited one passage for its step.
const P1 = "80000000-0000-4000-8000-000000000001", P2 = "80000000-0000-4000-8000-000000000002";
const C = (n: number) => `90000000-0000-4000-8000-00000000000${n}`;
const T1 = "a0000000-0000-4000-8000-000000000001", T2 = "a0000000-0000-4000-8000-000000000002";
const turn = (refs: string[]): ResearchTurn => ({
  turn: 1, context_revision: 2, checkpoint_revision: 3, created_at: "2026-09-25T20:00:00Z",
  record: {
    outcome: "delegate", scientific_validation: "not_performed", method_review: { decision: "proceed", reason: "Fits." },
    lead: { reported_model: "reasoner" },
    proposal: { summary: "Compare the two purees.", actions: [
      { action_key: "compare", title: "Compare", rationale: "A paper used this design.", literature_refs: refs },
      { action_key: "describe", title: "Describe", rationale: "Describe the data." }] },
    submitted_tasks: [{ task_id: T1, action_key: "compare" }, { task_id: T2, action_key: "describe" }],
    context_inputs: { literature: { status: "shown", passages: [
      { paper_id: P1, chunk_id: C(1), pages: "page 2", text_sha256: "sha256:a" },
      { paper_id: P2, chunk_id: C(2), pages: "pages 3-4", text_sha256: "sha256:b" },
      { paper_id: P1, chunk_id: C(3), pages: "page 5", text_sha256: "sha256:c" }], left_out: 0 } },
  },
});

test("the papers read at a decision are listed once each, with the pages read", () => {
  const read = papersRead(turn([]).record, [paper(), paper({ paper_id: P2, title: "Second paper" })]);
  assert.deepEqual(read.map((r) => [r.paper_id, r.state, r.pages]), [[P1, "listed", ["page 2", "page 5"]], [P2, "listed", ["pages 3-4"]]]);
  assert.equal(paperReadWords(read[0]), "A paper, page 2, page 5");
  // A paper the list no longer holds was removed after the agent read it; without the list it is only in the library.
  const gone = papersRead(turn([]).record, [paper()]);
  assert.equal(gone[1].state, "gone");
  assert.equal(paperReadWords(gone[1]), "a paper since removed from the lab's library");
  assert.equal(paperReadWords(papersRead(turn([]).record, null)[0]), "a paper in the lab's library");
  // A decision before version 6 read no papers.
  assert.deepEqual(papersRead({ ...turn([]).record, context_inputs: {} }, [paper()]), []);
});

test("an analysis names only the papers cited for its own step", () => {
  const decision = turn([`library://${P2}/${C(2)}`, "library://not-a-ref", `library://${P1}/${C(9)}`]);
  const cited = papersCited(decision, T1, [paper(), paper({ paper_id: P2, title: "Second paper" })]);
  assert.deepEqual(cited.map((c) => [c.paper_id, c.pages]), [[P2, ["pages 3-4"]], [P1, []]]);
  assert.deepEqual(papersCited(decision, T2, [paper()]), []);
  assert.deepEqual(papersCited(decision, "a0000000-0000-4000-8000-000000000009", [paper()]), []);
  assert.deepEqual(papersCited(undefined, T1, [paper()]), []);
});

test("a section the lab's knowledge search found is cited by its record, like a chunk", () => {
  const S = "b0000000-0000-4000-8000-000000000001";
  const decision = turn([`library://${P2}/${S}`]);
  decision.record.context_inputs!.literature!.passages!.push(
    { paper_id: P2, section_id: S, section_key: "cccccccccccccccc-1", pages: "pages 6 to 7", text_sha256: "sha256:d" });
  const cited = papersCited(decision, T1, [paper(), paper({ paper_id: P2, title: "Second paper" })]);
  assert.deepEqual(cited.map((c) => [c.paper_id, c.state, c.pages]), [[P2, "listed", ["pages 6 to 7"]]]);
});

test("a commons literature passage names its Commons paper by the title the decision recorded", () => {
  const K = "c0000000-0000-4000-8000-000000000001", S = "b0000000-0000-4000-8000-000000000002";
  const decision = turn([`literature://${K}/${S}`, `literature://${K}/not-a-section`]);
  decision.record.context_inputs!.literature!.passages!.push({ paper_id: K, section_id: S, section_key: "dddddddddddddddd-1",
    pages: "page 4", text_sha256: "sha256:e", origin: "commons", title: "A marked commons paper" });
  // Listed with the lab's papers even when the lab's list is unknown, and never called removed.
  const read = papersRead(decision.record, [paper()]);
  assert.deepEqual(read.map((r) => [r.paper_id, r.state, r.pages]),
    [[P1, "listed", ["page 2", "page 5"]], [P2, "gone", ["pages 3-4"]], [K, "commons", ["page 4"]]]);
  assert.equal(paperReadWords(read[2]), "A marked commons paper, from the Commons, page 4");
  assert.equal(papersRead(decision.record, null)[2].state, "commons");
  // Cited by its paper record; a ref the decision did not show still names the Commons.
  const cited = papersCited(decision, T1, [paper()]);
  assert.deepEqual(cited.map((c) => [c.paper_id, c.state, c.title, c.pages]), [[K, "commons", "A marked commons paper", ["page 4"]]]);
  const unshown = turn([`literature://${K}/${C(9)}`]);
  assert.equal(paperReadWords(papersCited(unshown, T1, [paper()])[0]), "A paper, from the Commons");
});

test("a citation's scheme must agree with the passage the decision recorded", () => {
  const K = "c0000000-0000-4000-8000-000000000001", S = "b0000000-0000-4000-8000-000000000002";
  // A library passage and a commons passage recorded under the same identifiers.
  const library = turn([`literature://${P2}/${C(2)}`]);
  const cited = papersCited(library, T1, [paper(), paper({ paper_id: P2, title: "Second paper" })]);
  assert.deepEqual(cited.map((c) => [c.paper_id, c.state, c.pages]), [[P2, "commons", []]]);
  const commons = turn([`library://${K}/${S}`]);
  commons.record.context_inputs!.literature!.passages!.push({ paper_id: K, section_id: S, section_key: "dddddddddddddddd-1",
    pages: "page 4", text_sha256: "sha256:e", origin: "commons", title: "A marked commons paper" });
  const asLibrary = papersCited(commons, T1, [paper()]);
  assert.deepEqual(asLibrary.map((c) => [c.paper_id, c.state, c.pages]), [[K, "gone", []]]);
});

test("an upload the check held or refused is explained from its finding codes", () => {
  const held = new BrokerError("request_failed", 422, "library_upload_quarantined", ["unicode_tags", "hidden_text_white", "polyglot"], "0199b0f5-0000-7000-8000-000000000001");
  assert.equal(libraryFailure(held, "add"),
    "This file was held for review and not added: its text holds hidden characters that can carry instructions and it also contains the structure of another file format. A lab lead can ask the platform operator to review it. Its check reference is 0199b0f5.");
  const refused = new BrokerError("request_failed", 422, "library_upload_blocked", ["malware_signature"]);
  assert.equal(libraryFailure(refused, "add"), "This file was refused and not added: the malware scanner matched a known threat.");
  assert.match(libraryFailure(new BrokerError("request_failed", 422, "library_upload_quarantined", ["something_new"]), "add"), /it needs a person's review/);
  assert.match(libraryFailure(new BrokerError("request_failed", 503, "library_guard_not_configured"), "add"), /Uploads are closed/);
  assert.match(libraryFailure(new BrokerError("request_failed", 429, "library_upload_quota"), "add"), /upload limit/);
  assert.match(libraryFailure(new BrokerError("request_failed", 409, "library_paper_unchecked"), "add"), /before uploads were checked/);
  assert.match(libraryFailure(new BrokerError("request_failed", 429, "library_busy"), "add"), /adding other papers/);
  assert.match(libraryFailure(new BrokerError("request_failed", 400, "library_upload_incomplete"), "add"), /did not arrive whole/);
  assert.match(libraryFailure(new BrokerError("request_failed", 411, "library_upload_length_required"), "add"), /state its size/);
  assert.match(libraryFailure(new BrokerError("request_failed", 422, "library_upload_quarantined", ["image_too_large"]), "add"),
    /images too large to open safely/);
  assert.match(libraryFailure(new BrokerError("request_failed", 422, "library_upload_quarantined", ["disarm_text_changed"]), "add"),
    /its text changed when it was rebuilt/);
  assert.match(libraryFailure(new BrokerError("request_failed", 422, "library_upload_quarantined", ["embedded_archive"]), "add"),
    /carries an archive inside its data/);
  assert.match(libraryFailure(new BrokerError("request_failed", 422, "library_upload_quarantined", ["render_failed"]), "add"),
    /pages could not be drawn/);
  assert.match(libraryFailure(new BrokerError("request_failed", 503, "library_guard_unavailable"), "add"), /upload check is unavailable/);
  assert.match(libraryFailure(new BrokerError("capacity_reached", 429, "library_guard_busy"), "add"), /busy with other files/);
});

test("a paper the check passed says what its kept copy lost and what was noted", () => {
  assert.equal(checkedWords(paper()), "", "a paper added before the check makes no claim");
  assert.equal(checkedWords(paper({ parse: { guard: { findings: [] } } })), "Checked on upload.");
  assert.equal(checkedWords(paper({ parse: { guard: { findings: ["active_javascript", "active_open_action", "hidden_text_white"] } } })),
    "Checked on upload; the copy the library keeps has JavaScript and an action that ran when it opened removed. The check noted text drawn in white.");
  assert.equal(checkedWords(paper({ parse: { guard: { findings: ["private_streams"] } } })),
    "Checked on upload; the copy the library keeps has data no page uses removed.");
  assert.equal(checkedWords(paper({ parse: { guard: { findings: ["private_streams", "content_irregular"] } } })),
    "Checked on upload; the copy the library keeps has data no page uses and page content no viewer draws removed.");
});

test("a batch of files is summed up in one sentence, each file in a few words", () => {
  assert.equal(batchWords(["added", "added", "present", "failed"]), "2 papers added; 1 was already in the library; 1 file was not added.");
  assert.equal(batchWords(["restored"]), "1 paper added.");
  assert.equal(batchWords(["present", "present"]), "No paper added; 2 were already in the library.");
  assert.equal(batchWords(["added", "skipped", "skipped"]), "1 paper added; 2 files were not read.");
  assert.equal(batchItemWords("failed", "Choose a PDF file."), "Choose a PDF file.");
  assert.equal(batchItemWords("failed"), "Not added.");
  assert.equal(batchItemWords("present"), "Already in the library.");
});

test("an outcome is unknown unless the answer says nothing changed", () => {
  assert.equal(libraryOutcomeUnknown(new BrokerError("connection_unavailable")), true);
  assert.equal(libraryOutcomeUnknown(new BrokerError("request_failed", 504)), true);
  assert.equal(libraryOutcomeUnknown(new BrokerError("invalid_response")), true, "a success whose body was cut off");
  assert.equal(libraryOutcomeUnknown(new BrokerError("request_failed", 503, "database_unavailable")), true, "a database that failed mid-write");
  assert.equal(libraryOutcomeUnknown(new Error("aborted")), true);
  assert.equal(libraryOutcomeUnknown(new BrokerError("request_failed", 503, "library_unavailable")), false);
  assert.equal(libraryOutcomeUnknown(new BrokerError("request_failed", 400, "library_not_pdf")), false);
  assert.equal(libraryOutcomeUnknown(new BrokerError("capacity_reached", 429, "library_upload_quota")), false);
  assert.equal(libraryOutcomeUnknown(new BrokerError("access_required", 401)), false);
  assert.equal(batchWords(["added", "unknown"]), "1 paper added; 1 file's answer was lost.");
});

test("an unknown upload settles only from a fresh list, by its bytes", () => {
  const states = ["added", "unknown", "unknown", "failed"] as const;
  const hashes = [null, "a".repeat(64), "b".repeat(64), null];
  assert.deepEqual(settleUploads([...states], hashes, new Set(["a".repeat(64)])), ["added", "added", "failed", "failed"]);
  assert.deepEqual(settleUploads([...states], hashes, null), [...states], "without a fresh list nothing is guessed");
});

test("accepting all settles only from a fresh list", () => {
  const refused = new Map([["p3", "Only a lead or reviewer of the lab can accept a paper."]]);
  const lost = new Set(["p2"]);
  assert.deepEqual(settleAcceptance(["p1", "p2", "p3"], refused, lost, new Set(["p1", "p2"])), { accepted: 2, refused: ["p3"], unknown: [] });
  assert.deepEqual(settleAcceptance(["p1", "p2", "p3"], refused, lost, null), { accepted: 1, refused: ["p3"], unknown: ["p2"] });
});

test("accepting every waiting paper says how many it accepted", () => {
  assert.equal(acceptAllWords(3, 3), "3 papers accepted for the research agent.");
  assert.equal(acceptAllWords(1, 1), "1 paper accepted for the research agent.");
  assert.equal(acceptAllWords(2, 3), "2 of 3 papers accepted for the research agent; 1 still waits.");
  assert.equal(acceptAllWords(1, 4, 2), "1 of 4 papers accepted for the research agent; 1 still waits; 2 answers were lost and the library could not be read again, so reload it to see.");
  assert.equal(acceptAllWords(0, 2, 2), "0 of 2 papers accepted for the research agent; 2 answers were lost and the library could not be read again, so reload it to see.");
});
