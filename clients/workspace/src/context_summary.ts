// What a decision's models were given, in plain words. Fixed code reads it from the
// kept text itself, so the summary holds nothing the text does not. It names parts and counts them; it never restates
// a result value. It quotes what people wrote (the question, constraints, instructions and shared notes) and the titles
// of analyses and steps; the agent's account of the question's parts is only counted, and a refusal is named in fixed
// words, never by its code.
import { refusalWords, stepStatus } from "./language.ts";

export interface SummaryItem { label: string; text: string; list?: string[] }

type Obj = Record<string, unknown>;
const obj = (v: unknown): Obj | null => (v !== null && typeof v === "object" && !Array.isArray(v) ? (v as Obj) : null);
const arr = (v: unknown): unknown[] => (Array.isArray(v) ? v : []);
const str = (v: unknown): string => (typeof v === "string" ? v.trim() : "");
// A count the text records either as a number or as the list itself.
const size = (v: unknown): number => (Array.isArray(v) ? v.length : typeof v === "number" && Number.isInteger(v) && v > 0 ? v : 0);
const count = (n: number, one: string, many = `${one}s`) => `${n} ${n === 1 ? one : many}`;
const clip = (text: string, max = 180) => (text.length > max ? `${text.slice(0, max - 1).trimEnd()}…` : text);
const ANSWER: Record<string, string> = {
  answered: "the results answer your question",
  partly_answered: "the results answer part of your question",
  not_answered: "the results do not answer your question",
};
// What an answer said, in the past for the record it read and in the present for the proposal it checks.
const said = (answer: unknown, now = false) => typeof answer === "string" && ANSWER[answer] ? `it ${now ? "says" : "said"} ${ANSWER[answer]}`
  : answer === "not_yet" ? `it ${now ? "does not say" : "had not said"} yet whether the results answer your question` : "";
// The state of each part of the question, counted. The parts are the agent's words, so the summary
// only counts them.
const PART_WORDS: Record<string, string> = {
  supported: "answered", partial: "partly answered", unsupported: "not in this data", open: "still being worked on",
  awaiting: "waiting for your choice",
};
function partCounts(parts: unknown[]): string {
  const by = new Map<string, number>();
  for (const p of parts) {
    const state = str(obj(p)?.state);
    if (PART_WORDS[state]) by.set(state, (by.get(state) ?? 0) + 1);
  }
  const shown = [...by.values()].reduce((a, b) => a + b, 0);
  const words = Object.keys(PART_WORDS).filter((s) => by.get(s)).map((s) => `${by.get(s)} ${PART_WORDS[s]}`);
  return `${count(shown, "part")}${words.length ? `: ${words.join(", ")}` : ""}`;
}
// A refusal in plain words; one the workspace has no words for yet still never shows its code.
const refused = (code: string) => refusalWords(code) ?? "the checks refused it for a reason without plain words yet";
const heldBack = (n: number) => `${n} held back because ${n === 1 ? "it" : "they"} read like instructions to an AI`;

function methodLine(method: Obj): string {
  const name = str(method.name) || str(method.key);
  if (obj(method.settings)) return `${name}, a recipe with settings`;
  if (obj(method.template)) return `${name}, code the agent writes, checked before it runs`;
  return `${name}, a fixed reviewed recipe`;
}

function reviewWords(review: Obj | null): string {
  switch (review?.decision) {
    case "accepted": return "accepted by a person";
    case "needs_changes": return "a person asked for changes";
    case "rejected": return "rejected by a person, so its results were held back";
    default: return "";
  }
}

function leadItems(c: Obj): SummaryItem[] {
  const items: SummaryItem[] = [];
  const question = str(c.question);
  if (question) items.push({ label: "Your question", text: question });
  const constraints = arr(c.constraints).map(str).filter(Boolean);
  if (constraints.length) items.push({ label: "Your constraints", text: count(constraints.length, "constraint"), list: constraints });
  const instructions = arr(c.scientist_instructions).map(str).filter(Boolean);
  if (instructions.length) items.push({ label: "Your newest instructions", text: count(instructions.length, "instruction"), list: instructions });
  const start = obj(c.starting_point);
  if (start) {
    const sets = size(start.data);
    items.push({ label: "Data", text: start.kind === "prepared" ? "A prepared data set" : `The data chosen for this project${sets > 1 ? ` (${count(sets, "data set")})` : ""}` });
  }
  const methods = arr(c.methods).map(obj).filter((m): m is Obj => !!m);
  if (methods.length) items.push({ label: "Methods it could use", text: count(methods.length, "method"), list: methods.map(methodLine) });
  const analyses = arr(c.analyses).map(obj).filter((a): a is Obj => !!a);
  if (analyses.length) {
    const byStatus = new Map<string, number>();
    for (const a of analyses) byStatus.set(stepStatus(str(a.status)).label, (byStatus.get(stepStatus(str(a.status)).label) ?? 0) + 1);
    const read = analyses.filter((a) => arr(a.results).length).length;
    const parts = [[...byStatus].map(([label, n]) => `${n} ${label.toLowerCase()}`).join(", ")];
    if (read) parts.push(`it read the results of ${read}`);
    const omitted = size(c.results_omitted) + size(c.results_omitted_more);
    if (omitted) parts.push(`${count(omitted, "result")} left out to stay within the size limit`);
    items.push({ label: "Analyses so far", text: `${count(analyses.length, "analysis", "analyses")}: ${parts.join("; ")}`,
      list: analyses.map((a) => [str(a.title) || str(a.key), stepStatus(str(a.status)).label.toLowerCase(), reviewWords(obj(a.review))].filter(Boolean).join(", ")) });
  }
  const latest = obj(c.latest_answer);
  if (latest && said(latest.answer)) {
    const findings = size(latest.findings);
    items.push({ label: "Its latest answer", text: `Last time ${said(latest.answer)}${findings ? `, with ${count(findings, "finding")}` : ""}` });
  }
  const ledger = obj(c.answerability);
  if (ledger) {
    const omitted = size(ledger.parts_omitted);
    items.push({ label: "Its earlier account of your question's parts",
      text: omitted ? `${count(omitted, "part")}, left out to stay within the size limit` : `From an earlier reply in this work session, ${partCounts(arr(ledger.parts))}` });
  }
  const attempts = arr(c.attempts).map(obj).filter((a): a is Obj => !!a && !!str(a.refused));
  if (attempts.length)
    items.push({ label: "Refused replies in this work session", text: `${count(attempts.length, "reply", "replies")} refused, newest first`,
      list: attempts.map((a) => refused(str(a.refused))) });
  const earlier = obj(c.earlier_findings);
  const findings = arr(earlier?.findings).map(obj).filter((f): f is Obj => !!f);
  if (findings.length) {
    const withdrawn = findings.filter((f) => f.withdrawn).length;
    items.push({ label: "Earlier findings", text: `${count(findings.length, "finding")} it stated before${withdrawn ? `; ${withdrawn} withdrawn because a person rejected an analysis it read` : ""}` });
  }
  const lessons = size(obj(c.earlier_readings)?.lessons);
  if (lessons) items.push({ label: "Saved lessons", text: `${count(lessons, "lesson")} from earlier steps, not verified` });
  const notes = arr(c.lab_notes).map(obj).filter((n): n is Obj => !!n);
  const notesHeld = size(c.lab_notes_withheld), notesLeft = size(c.lab_notes_omitted);
  if (notes.length || notesHeld || notesLeft) {
    const parts = [count(notes.length, "note")];
    if (notesHeld) parts.push(heldBack(notesHeld));
    if (notesLeft) parts.push(`${notesLeft} left out to stay within the size limit`);
    items.push({ label: "Notes shared in the lab", text: parts.join("; "), list: notes.map((n) => clip(str(n.statement))).filter(Boolean) });
  }
  // The literature specialist's claims replace the passages when it read them.
  const read = Array.isArray(c.literature_claims);
  const passages = arr(read ? c.literature_claims : c.literature).map(obj).filter((p): p is Obj => !!p);
  const unit = read ? "claim" : "passage";
  const passagesHeld = size(read ? c.literature_claims_withheld : c.literature_withheld);
  const passagesLeft = size(c.literature_left_out), offTopic = size(c.literature_off_topic);
  const papers = new Map<string, number>();
  for (const p of passages) {
    const title = `${str(p.paper) || "A paper without a title"}${typeof p.year === "number" ? ` (${p.year})` : ""}`;
    papers.set(title, (papers.get(title) ?? 0) + 1);
  }
  const paperParts = passages.length ? [`${count(passages.length, unit)} from ${count(papers.size, "paper")}`] : ["None"];
  if (read) paperParts.push("read from the passages by the literature specialist");
  if (offTopic) paperParts.push(`${count(offTopic, "passage")} it found off the question`);
  if (passagesHeld) paperParts.push(heldBack(passagesHeld));
  if (passagesLeft) paperParts.push(`${passagesLeft} left out to stay within the size limit`);
  items.push({ label: "Papers from the lab's library", text: paperParts.join("; "),
    list: [...papers].map(([title, n]) => (n > 1 ? `${title}, ${count(n, unit)}` : title)) });
  const prior = obj(c.prior_attempt);
  if (prior && str(prior.refused))
    items.push({ label: "A refused try in this work session", text: `Its previous reply was refused (${refused(str(prior.refused))}), so it had to change it` });
  return items;
}

function proposalItems(c: Obj): SummaryItem[] {
  const proposal = obj(c.proposal);
  if (!proposal) return [];
  const actions = arr(proposal.actions).map(obj).filter((a): a is Obj => !!a);
  const findings = size(proposal.findings);
  const what = proposal.action === "delegate" ? `Run ${count(actions.length, "new analysis", "new analyses")}`
    : proposal.action === "review_required" ? "Stop and ask for your review"
    : proposal.action === "paused" ? "Pause" : "A proposal";
  const answer = said(proposal.answer, true);
  const items: SummaryItem[] = [{ label: "The research agent's proposal",
    text: `${what}${answer ? `; ${answer}` : ""}${findings ? `, with ${count(findings, "finding")}` : ""}`,
    list: actions.map((a) => str(a.title)).filter(Boolean) }];
  const parts = arr(proposal.parts);
  if (parts.length) items.push({ label: "Parts of your question it accounts for", text: partCounts(parts) });
  const values = arr(c.finding_values).reduce<number>((n, v) => n + Object.keys(obj(v) ?? {}).length, 0);
  if (values) items.push({ label: "Values it checked", text: `${count(values, "value")} read from the result tables, to check each finding` });
  return items;
}

// The facts code measured for the query steps against each method's schema card: for the methods
// check, those of the proposal it checks; for the agent, those of the proposal the check sent back.
function factsItems(c: Obj): SummaryItem[] {
  const steps = arr(c.query_facts).map(obj).filter((s): s is Obj => !!s);
  if (!steps.length) return [];
  const facts = steps.flatMap((s) => arr(s.facts).map(obj)).filter((f): f is Obj => !!f);
  const absent = facts.filter((f) => f.found === false || (f.kind === "pattern" && f.matches === 0)).length;
  const parts = [`${count(facts.length, "fact")} from the data's schema card about ${count(steps.length, "query step")}`];
  if (absent) parts.push(`${count(absent, "value")} the data does not hold`);
  const left = size(c.query_facts_omitted);
  if (left) parts.push(`${left} left out to stay within the size limit`);
  return [{ label: c.proposal ? "Facts about its queries" : "Facts about the queries the check sent back",
    text: parts.join("; "), list: steps.map((s) => str(s.title)).filter(Boolean) }];
}

/** A plain summary of a kept decision text, or null when the text is not a decision's context. */
export function contextSummary(text: string): SummaryItem[] | null {
  let parsed: unknown;
  try { parsed = JSON.parse(text); } catch { return null; }
  const c = obj(parsed);
  if (!c || (!("context_schema_version" in c) && !("proposal" in c))) return null;
  return [...proposalItems(c), ...leadItems(c), ...factsItems(c)];
}
