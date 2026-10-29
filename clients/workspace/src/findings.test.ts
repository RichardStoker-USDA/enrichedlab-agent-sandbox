import assert from "node:assert/strict";
import test from "node:test";
import { glued, answerHistory, answerTableRows, answerTables, answerWords, cell, display, ledgerParts, partStateWords, parts, referencedArtifacts, shownFindings, shownTables, sourceWords, type AnswerTable, type ArtifactDigest, type Finding } from "./findings.ts";

const task = "40000000-0000-4000-8000-000000000002";
const artifact = "31000000-0000-4000-8000-000000000003";
const ref = `artifact://tasks/${task}/${artifact}`;
const digest: ArtifactDigest = { artifact_id: artifact, artifact_sha256: "a".repeat(64), status: "summarized", digest_sha256: "b".repeat(64), recorded_at: "t",
  digest: { tables: [{ path: "results/group_summary.csv", rows: 2, columns: ["condition", "mean"], cells: [["reference", "8.86"], ["diet_a", "12.38"]] },
    { path: "results/resamples.csv", rows: 9000, columns: ["mean"], cells_omitted: "too_large" }] } };
const bind = (row: number, column: string, table = "results/group_summary.csv") => ({ ref, table, row, column });
const finding: Finding = { statement: "Diet A raised the mean response ({a}) above the reference ({b}).", bindings: { a: bind(2, "mean"), b: bind(1, "mean") } };

test("values come from the stored digest cell by cell, never from the sentence", () => {
  const p = parts(finding, { [ref]: digest });
  assert.deepEqual(p.map((x) => ("text" in x ? x.text : `[${x.value}]`)).join(""), "Diet A raised the mean response ([12.38]) above the reference ([8.86]).");
  assert.equal(cell(bind(3, "mean"), { [ref]: digest }), null);
  assert.equal(cell(bind(1, "median"), { [ref]: digest }), null);
  assert.equal(cell(bind(1, "mean", "results/resamples.csv"), { [ref]: digest }), null);
  // Without the digest a value is missing, not guessed.
  assert.deepEqual(parts(finding, {}).filter((x) => "slot" in x).map((x) => ("slot" in x ? x.value : "")), [null, null]);
  // A placeholder without a binding stays as written.
  assert.deepEqual(parts({ statement: "Unbound {c}.", bindings: {} }, {}), [{ text: "Unbound " }, { text: "{c}" }, { text: "." }]);
  // A descriptive name works the same way as a letter.
  const named = parts({ statement: "Diet A mean {diet_a_mean}.", bindings: { diet_a_mean: bind(2, "mean") } }, { [ref]: digest });
  assert.deepEqual(named.map((x) => ("text" in x ? x.text : `[${x.value}]`)).join(""), "Diet A mean [12.38].");
});

test("long decimals are shown briefly and everything else as written", () => {
  assert.equal(display("12.3791666666667"), "12.38");
  assert.equal(display("3.07933549770212e-08"), "3.079e-8");
  assert.equal(display("-1.64083333333333"), "-1.641");
  assert.equal(display("20.66"), "20.66");
  // One sentence reads alike whatever rounding the analysis wrote; counts keep every digit.
  assert.equal(display("22.9135"), "22.91");
  assert.equal(display("123456"), "123456");
  assert.equal(display("0.5"), "0.5");
  assert.equal(display("12"), "12");
  assert.equal(display("diet_a"), "diet_a");
  assert.equal(display("NA"), "NA");
  assert.equal(display("0.000844881867429592"), "0.0008449");
});

test("the newest decision that states an answer is the project's answer; earlier findings are history", () => {
  const turn = (at: string, findings: Finding[], answer?: "answered" | "not_answered" | "not_yet") =>
    ({ created_at: at, record: { proposal: { answer, findings } } });
  assert.deepEqual(answerHistory([]), { current: null, earlier: [] });
  // A decision from before answers were recorded, with no findings, says nothing.
  assert.deepEqual(answerHistory([{ turns: [{ created_at: "2", record: { proposal: {} } }] }]), { current: null, earlier: [] });
  // An earlier "answered" with a finding, then a later "not_answered" with none, in
  // another work session. The newer answer is current; the finding is dated history.
  const corrected = answerHistory([{ turns: [turn("1", [finding], "answered")] }, { turns: [turn("3", [], "not_answered")] }]);
  assert.equal(corrected.current?.answer, "not_answered");
  assert.equal(corrected.current?.at, "3");
  assert.deepEqual(corrected.current?.findings, []);
  assert.deepEqual(corrected.earlier.map((s) => [s.at, s.answer]), [["1", "answered"]]);
  // A later delegation that has not said yet keeps the history but never relabels it as current.
  const working = answerHistory([{ turns: [turn("1", [finding], "answered"), turn("2", [], "not_yet")] }]);
  assert.equal(working.current?.answer, "not_yet");
  assert.equal(working.earlier.length, 1);
  // Newest first, at most three kept.
  const many = answerHistory([{ turns: ["1", "2", "3", "4", "5"].map((at) => turn(at, [finding], "answered")) }]);
  assert.equal(many.current?.at, "5");
  assert.deepEqual(many.earlier.map((s) => s.at), ["4", "3", "2"]);
  assert.equal(shownFindings(many).length, 4);
  assert.deepEqual(referencedArtifacts([finding, finding]), [{ ref, task, artifact }]);
  assert.equal(sourceWords(bind(2, "mean")), "From group_summary.csv, row 2, column “mean”");
  for (const words of Object.values(answerWords)) assert.match(words, /^It (says|has not said)/);
});

test("the newest answer carries the agent's account of the question's parts, known states only", () => {
  // Each part with a known state and words is kept with its reason; anything else is left out.
  const parts = [{ part: "Which diet raised it most", state: "supported", findings: [1] },
    { part: "In people", state: "unsupported", reason: "Only animal data." },
    { part: "By dose", state: "maybe" }, { part: " ", state: "open" }, "not a part", null,
    { part: "Adjust for baseline", state: "awaiting", reason: "  " }];
  const current = answerHistory([{ turns: [{ created_at: "4", record: { proposal: { answer: "partly_answered", findings: [finding], parts } } }] }]).current;
  assert.deepEqual(current?.parts, [{ part: "Which diet raised it most", state: "supported", reason: null },
    { part: "In people", state: "unsupported", reason: "Only animal data." },
    { part: "Adjust for baseline", state: "awaiting", reason: null }]);
  assert.deepEqual(ledgerParts(undefined), []);
  assert.deepEqual(Object.keys(partStateWords).sort(), ["awaiting", "open", "partial", "supported", "unsupported"]);
});

test("a proposal the methods check sent back is never the agent's answer", () => {
  const stated = { created_at: "1", record: { outcome: "review_required", proposal: { answer: "answered" as const, findings: [] } } };
  const sentBack = { created_at: "2", record: { outcome: "revise", proposal: { answer: "partly_answered" as const, findings: [] } } };
  assert.equal(answerHistory([{ turns: [stated, sentBack] }]).current?.answer, "answered");
});

test("a value keeps the punctuation that touches it, and the text keeps every character", () => {
  const binding = { ref: "artifact://tasks/x/y", table: "t.csv", row: 1, column: "mean" };
  const list = [{ text: "the reference diet (" }, { slot: "a", value: "8.857", binding }, { text: "). Then " }, { slot: "b", value: "3.5", binding }, { text: ", done" }];
  const out = glued(list);
  assert.deepEqual(out.map((g) => ("text" in g ? g.text : `[${g.before}|${g.value.value}|${g.after}]`)), ["the reference diet ", "[(|8.857|).]", " Then ", "[|3.5|,]", " done"]);
  const flat = out.map((g) => ("text" in g ? g.text : g.before + (g.value.value ?? "") + g.after)).join("");
  assert.equal(flat, "the reference diet (8.857). Then 3.5, done");
});

test("an answer table shows the named columns of every stored row, or says why it cannot", () => {
  const table: AnswerTable = { ref, table: "results/group_summary.csv", columns: ["mean", "condition"], lead_in: "The two groups:", rows: 2 };
  // The agent chose the columns and their order; every cell is the digest's, exactly as stored.
  assert.deepEqual(answerTableRows(table, { [ref]: digest }), { columns: ["mean", "condition"], rows: [["8.86", "reference"], ["12.38", "diet_a"]] });
  // A table the digest does not hold is not shown in part: the digest's reason and its row count stand instead.
  assert.deepEqual(answerTableRows({ ...table, table: "results/resamples.csv", columns: ["mean"], rows: 9000 }, { [ref]: digest }),
    { omitted: "too_large", rows: 9000 });
  assert.deepEqual(answerTableRows({ ...table, columns: ["mean", "median"] }, { [ref]: digest }), { omitted: "columns", rows: 2 });
  assert.deepEqual(answerTableRows({ ...table, table: "results/other.csv" }, { [ref]: digest }), { omitted: "not_loaded", rows: 2 });
  assert.deepEqual(answerTableRows(table, {}), { omitted: "not_loaded", rows: 2 });
  // Only well-formed records are kept; a row count is the coordinator's and must be there.
  assert.deepEqual(answerTables([table, { ...table, rows: undefined }, { ...table, ref: "https://example.invalid/x" }, { ...table, columns: [] },
    { ...table, columns: [1] }, { ...table, rows: -1 }, { ...table, rows: 1.5 }, null, "table"]), [table]);
  assert.deepEqual(answerTables(undefined), []);
  // An answer may rest on tables alone; such a decision is the answer, and is kept as history once a newer one replaces it.
  const turn = (at: string, tables: unknown, answer: "answered" | "not_answered") => ({ created_at: at, record: { proposal: { answer, findings: [], tables } } });
  const current = answerHistory([{ turns: [turn("1", [table], "answered")] }]).current;
  assert.deepEqual(current?.tables, [table]);
  assert.deepEqual(current?.findings, []);
  const replaced = answerHistory([{ turns: [turn("1", [table], "answered"), turn("2", [], "not_answered")] }]);
  assert.equal(replaced.current?.answer, "not_answered");
  assert.deepEqual(replaced.earlier.map((e) => [e.at, e.tables.length]), [["1", 1]]);
  assert.deepEqual(shownTables(replaced), [table]);
  // The digest of a table's result is fetched like a finding's.
  assert.deepEqual(referencedArtifacts([], [table, table]), [{ ref, task, artifact }]);
  assert.deepEqual(referencedArtifacts([finding], [table]), [{ ref, task, artifact }]);
});
