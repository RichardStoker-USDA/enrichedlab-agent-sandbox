import assert from "node:assert/strict";
import test from "node:test";
import { BrokerError } from "./api.ts";
import type { CommonsRecord } from "./contracts.ts";
import { collectionMarkedWords, collectionName, collectionWords, commonsFailure, countWords, knowledge, linkWords, markedWords, outcomeUnknown,
  partsWaitingWords, queueWords, reachWords, recordFacts, reviewedWords, roleWords, waitingWords } from "./knowledge.ts";

const record = (baseline: boolean, extra: Partial<CommonsRecord> = {}): CommonsRecord => ({
  resource_id: "01920000-0000-7000-8000-00000000d001", kind: "dataset", scope: "commons", baseline, title: "A reference table",
  summary: null, data_class: "public_reference", rights: "public", lifecycle: "active", source_uri: null,
  created_at: "2026-09-20T10:00:00Z", ...extra,
});

test("the count sentence says how many records every lab reads", () => {
  assert.equal(countWords({ kind: "dataset", records: [], complete: true }), "The commons holds no dataset yet.");
  assert.equal(countWords({ kind: "dataset", records: [record(true)], complete: true }), "One dataset, marked baseline.");
  assert.equal(countWords({ kind: "paper", records: [record(true), record(false), record(false)], complete: true }), "3 papers, 1 marked baseline.");
  assert.equal(countWords({ kind: "tool", records: [record(false), record(false)], complete: false }), "The newest 2 tools shown, none marked baseline yet; more follow.");
  assert.equal(countWords({ kind: "object_collection", records: [record(true), record(true)], complete: true }), "2 object collections, all marked baseline.");
});

test("only a person who leads the commons is offered the mark, and everyone else is told why", () => {
  assert.equal(roleWords({ role: "lead", can_mark: true }), "");
  assert.equal(roleWords({ role: null, can_mark: false }), "Only a person who leads the commons can change what every lab receives.");
  assert.match(roleWords({ role: "curator", can_mark: false }), /^You curate the commons\. Only/);
  assert.match(roleWords({ role: "lead", can_mark: false }), /does not count as you acting directly/);
  assert.equal(waitingWords(0), "");
  assert.equal(waitingWords(1), "One record is not marked. Mark it if every lab should read it.");
  assert.equal(reachWords(record(true)), "Every lab reads it");
  assert.equal(reachWords(record(false)), "Commons only");
});

test("record facts and outcomes are words, and unknown values are shown as they are", () => {
  assert.match(recordFacts(record(false)), /^Public reference data · rights public · added /);
  assert.match(recordFacts(record(false, { data_class: "new_class", lifecycle: "superseded" })), /^new_class · rights public · superseded · added /);
  assert.equal(markedWords(record(true)), "Marked baseline: every lab's research agent now reads “A reference table”.");
  assert.equal(markedWords(record(false)), "Mark withdrawn: “A reference table” is read only through the commons now.");
  assert.equal(markedWords({ ...record(true), parts_changed: 42 }), "Marked baseline with 42 sections, figures and tables: every lab's research agent now reads “A reference table”.");
  assert.equal(markedWords({ ...record(false), parts_changed: 1 }), "Mark withdrawn with one section, figure or table: “A reference table” is read only through the commons now.");
});

test("a marked paper says how many newer parts wait, and a collection says what it holds and what every lab reads", () => {
  assert.equal(partsWaitingWords({ baseline: true, parts_waiting: 0 }), "");
  assert.equal(partsWaitingWords({ baseline: false, parts_waiting: 4 }), "", "an unmarked paper's parts are not waiting for anything");
  assert.equal(partsWaitingWords({ baseline: true, parts_waiting: 1 }), "One section, figure or table was added since the mark and is read through the commons only.");
  assert.match(partsWaitingWords({ baseline: true, parts_waiting: 1200 }), /^1,200 sections, figures or tables were added/);
  assert.equal(collectionName("literature://papers/"), "papers literature");
  assert.equal(collectionName("literature://longcollection/"), "longcollection literature");
  assert.equal(collectionName("https://fdc.example.invalid/"), "Records from fdc.example.invalid");
  const papers = { prefix: "literature://papers/", parts_waiting: 0, kinds: {
    tool: { active: 24, marked: 0 }, person: { active: 868, marked: 0 }, paper: { active: 811, marked: 0 }, dataset: { active: 126, marked: 0 } } };
  assert.deepEqual(collectionWords(papers), { holds: "811 papers, 126 datasets, 24 tools, 868 authors.", reach: "Read through the commons only.", waiting: "" });
  const marked = { ...papers, parts_waiting: 3, kinds: { paper: { active: 2, marked: 2 } } };
  assert.deepEqual(collectionWords(marked), { holds: "2 papers.", reach: "Every lab's research agent reads all of it.",
    waiting: "3 sections, figures or tables of its marked papers were added since the mark." });
  assert.equal(collectionWords({ ...papers, kinds: { paper: { active: 4, marked: 1 } } }).reach, "Every lab reads 1 of its 4 records.");
  assert.equal(collectionMarkedWords("literature://papers/", { prefix: "literature://papers/", baseline: true, records_changed: 1829, parts_changed: 9278 }),
    "Marked 1,829 records and 9,278 sections, figures and tables of papers literature baseline, in your name. Every lab's research agent now reads them.");
  assert.equal(collectionMarkedWords("literature://papers/", { prefix: "literature://papers/", baseline: true, records_changed: 0, parts_changed: 0 }),
    "papers literature was already marked baseline.");
  assert.match(collectionMarkedWords("literature://papers/", { prefix: "literature://papers/", baseline: false, records_changed: 2, parts_changed: 0 }),
    /^Withdrew the mark from 2 records of papers literature\./);
  assert.throws(() => knowledge.markCollection("literature://", true), /invalid_reference/);
  assert.throws(() => knowledge.commons("paper", undefined, "not-a-cursor"), /invalid_reference/);
});

test("a refusal says nothing changed, and a change the service did not confirm never does", () => {
  assert.equal(commonsFailure(new BrokerError("access_required", 403, "baseline_refused"), "mark"), "Only a person who leads the commons can change this.");
  assert.equal(commonsFailure(new BrokerError("request_conflict", 409, "knowledge_not_configured"), "load"), "The knowledge service is not connected to this workspace yet.");
  assert.equal(commonsFailure(new Error("x"), "load"), "The commons could not be loaded. Nothing changed; reload to try again.");
  assert.match(commonsFailure(new BrokerError("access_required", 403, "person_sign_in_stale"), "collection"), /collection needs a sign-in from the last 15 minutes.*Nothing changed\.$/);
  assert.match(commonsFailure(new BrokerError("access_required", 403, "person_sign_in_stale"), "mark"), /sign-in from the last hour.*Nothing changed\.$/);
  assert.equal(commonsFailure(new BrokerError("request_failed", 503, "knowledge_unavailable"), "mark"), "The knowledge service is unavailable. Nothing changed; try again in a moment.");
  const unknown = "The knowledge service did not confirm the change, so it may or may not have been made. The list was reloaded to show what the service holds now.";
  for (const cause of [new BrokerError("request_failed", 504, "knowledge_outcome_unknown"), new BrokerError("connection_unavailable"),
    new BrokerError("request_failed", 502, "knowledge_refused"), new Error("x")]) {
    assert.equal(outcomeUnknown(cause), true, String(cause));
    assert.equal(commonsFailure(cause, "mark"), unknown);
    assert.equal(commonsFailure(cause, "review"), unknown);
    assert.equal(commonsFailure(cause, "collection"), unknown);
  }
  assert.equal(outcomeUnknown(new BrokerError("access_required", 403, "baseline_refused")), false);
  assert.equal(commonsFailure(new BrokerError("connection_unavailable"), "load"), "The knowledge service is unavailable. Nothing changed; try again in a moment.",
    "a failed read changed nothing");
  assert.throws(() => knowledge.mark("../x", true), /invalid_reference/);
  assert.throws(() => knowledge.commons("section" as never), /invalid_reference/);
});

test("a proposed link says what it claims and why it was proposed", () => {
  const link = { relation_id: "01920000-0000-7000-8000-00000000e001", kind: "uses_tool", created_at: "2026-09-28T05:00:00Z",
    source: { id: "a", kind: "paper", title: "A paper" }, target: { id: "b", kind: "tool", title: "Mean bootstrap" },
    terms: ["bootstrap", "bootstrapped"], sections: 2 };
  assert.deepEqual(linkWords(link), {
    claim: "“A paper” may use the lab's tool “Mean bootstrap”.",
    why: "Proposed because “bootstrap”, “bootstrapped” appears in 2 sections.",
  });
  assert.equal(linkWords({ ...link, kind: "uses_dataset", sections: 1 }).why, "Proposed because “bootstrap”, “bootstrapped” appears in one section.");
  assert.equal(queueWords({ proposals: [link], complete: true, can_review: true }), "One proposed link waits for your review.");
  assert.equal(queueWords({ proposals: [link, link], complete: false, can_review: false }),
    "2 proposed links wait for a person who curates or leads the lab to review them. These are the oldest; more exist.");
  assert.equal(queueWords({ proposals: [], complete: true, can_review: true }), "");
  assert.equal(reviewedWords("rejected"), "Link rejected. It will not be proposed again.");
  assert.equal(commonsFailure(new BrokerError("record_unavailable", 404, "relation_not_found"), "review"),
    "This link is no longer waiting, or your role in the lab does not review links.");
  assert.equal(commonsFailure(new Error("x"), "links"), "The proposed links could not be loaded. Nothing changed; reload to try again.");
  assert.throws(() => knowledge.links("Lab A"), /invalid_reference/);
  assert.throws(() => knowledge.review("x", true, ""), /invalid_reference/);
});
