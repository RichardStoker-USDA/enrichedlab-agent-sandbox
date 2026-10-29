import { BrokerError, request } from "./api.ts";
import type { CollectionMarked, CommonsCollection, CommonsCollections, CommonsKind, CommonsMarked, CommonsRecord, CommonsView, LabLink, LabLinkQueue } from "./contracts.ts";
import { day } from "./lab_notes.ts";

// The commons in plain words. The knowledge service decides who may mark a record baseline and
// records each mark in that person's name; the workspace forwards the person's own sign-in and words the answers.
export const commonsClaim =
  "Public reference data, papers and tools the labs share. Every lab's research agent reads a record marked baseline; an unmarked record is read only by people and agents with access to the commons.";

export const commonsKinds: { key: CommonsKind; one: string; many: string; heading: string }[] = [
  { key: "dataset", one: "dataset", many: "datasets", heading: "Datasets" },
  { key: "paper", one: "paper", many: "papers", heading: "Papers" },
  { key: "tool", one: "tool", many: "tools", heading: "Tools" },
  { key: "object_collection", one: "object collection", many: "object collections", heading: "Object collections" },
];
const kindOf = (key: CommonsKind) => commonsKinds.find((k) => k.key === key) ?? commonsKinds[0];

const UUID = /^[a-f0-9]{8}-[a-f0-9]{4}-[a-f0-9]{4}-[a-f0-9]{4}-[a-f0-9]{12}$/i;
const CURSOR = /^[0-9]{1,19}\.[a-f0-9]{8}-[a-f0-9]{4}-[a-f0-9]{4}-[a-f0-9]{4}-[a-f0-9]{12}$/i;
const PREFIX = /^[a-z][a-z0-9+.-]*:\/\/[a-z0-9][a-z0-9._-]*\/[^\s]{0,160}$/;
const mutate = { "Content-Type": "application/json", "X-Agent-Sandbox-Intent": "mutate" };

export const knowledge = {
  commons: (kind: CommonsKind, signal?: AbortSignal, after?: string) => {
    if (!commonsKinds.some((k) => k.key === kind)) throw new BrokerError("invalid_reference");
    if (after !== undefined && !CURSOR.test(after)) throw new BrokerError("invalid_reference");
    return request<CommonsView>(`/api/v1/knowledge/commons?kind=${kind}${after ? `&after=${after}` : ""}`, { signal });
  },
  mark: (record: string, baseline: boolean) => {
    if (!UUID.test(record)) throw new BrokerError("invalid_reference");
    return request<CommonsMarked>(`/api/v1/knowledge/resources/${record}/baseline`, {
      method: "POST", headers: mutate, body: JSON.stringify({ baseline }),
    });
  },
  collections: (signal?: AbortSignal) => request<CommonsCollections>("/api/v1/knowledge/commons/collections", { signal }),
  markCollection: (prefix: string, baseline: boolean) => {
    if (!PREFIX.test(prefix)) throw new BrokerError("invalid_reference");
    return request<CollectionMarked>("/api/v1/knowledge/commons/collections/baseline", {
      method: "POST", headers: mutate, body: JSON.stringify({ prefix, baseline }),
    });
  },
  links: (lab: string, signal?: AbortSignal) => {
    if (!/^[a-z0-9][a-z0-9._-]{0,62}$/.test(lab)) throw new BrokerError("invalid_reference");
    return request<LabLinkQueue>(`/api/v1/knowledge/labs/${lab}/proposals`, { signal });
  },
  review: (link: string, accept: boolean, note: string) => {
    if (!UUID.test(link)) throw new BrokerError("invalid_reference");
    return request<{ relation_id: string; outcome: "accepted" | "rejected" }>(`/api/v1/knowledge/relations/${link}/review`, {
      method: "POST", headers: mutate, body: JSON.stringify({ accept, note: note.trim() || null }),
    });
  },
};

export const MAX_REVIEW_NOTE = 500;

const linkKinds: Record<string, string> = { uses_tool: "may use the lab's tool", uses_dataset: "may use the dataset" };

// What a proposed link claims, and why it was proposed, in one sentence each.
export function linkWords(link: LabLink): { claim: string; why: string } {
  const verb = linkKinds[link.kind] ?? `may be linked (${link.kind}) to`;
  const claim = `“${link.source.title}” ${verb} “${link.target.title}”.`;
  const names = link.terms.map((t) => `“${t}”`).join(", ");
  const where = link.sections === 1 ? "one section" : link.sections ? `${link.sections} sections` : "its text";
  return { claim, why: names ? `Proposed because ${names} appears in ${where}.` : "Proposed by the knowledge service." };
}

// The queue's lead sentence: how many links wait and who may decide them.
export function queueWords(queue: Pick<LabLinkQueue, "proposals" | "complete" | "can_review">): string {
  const n = queue.proposals.length;
  if (!n) return "";
  const count = n === 1 ? "One proposed link waits" : `${n} proposed links wait`;
  const who = queue.can_review ? "for your review." : "for a person who curates or leads the lab to review them.";
  return `${count} ${who}${queue.complete ? "" : " These are the oldest; more exist."}`;
}

export function reviewedWords(outcome: "accepted" | "rejected"): string {
  return outcome === "accepted" ? "Link accepted and recorded in your name." : "Link rejected. It will not be proposed again.";
}

// How many records of the kind the commons holds and how many every lab receives, in one sentence. Before the last
// page the counts are of the records shown so far.
export function countWords(view: Pick<CommonsView, "kind" | "records" | "complete">): string {
  const kind = kindOf(view.kind);
  const total = view.records.length;
  if (!total) return `The commons holds no ${kind.one} yet.`;
  const marked = view.records.filter((r) => r.baseline).length;
  const held = total === 1 ? `One ${kind.one}` : `${total.toLocaleString("en-US")} ${kind.many}`;
  const reach = marked === 0 ? "none marked baseline yet" : marked === total ? (total === 1 ? "marked baseline" : "all marked baseline") : `${marked.toLocaleString("en-US")} marked baseline`;
  return view.complete ? `${held}, ${reach}.` : `The newest ${total.toLocaleString("en-US")} ${total === 1 ? kind.one : kind.many} shown, ${reach}; more follow.`;
}

// A paper's newer parts that the mark does not cover yet, for the line beside its reach.
export function partsWaitingWords(record: Pick<CommonsRecord, "baseline" | "parts_waiting">): string {
  const n = record.parts_waiting ?? 0;
  if (!record.baseline || n < 1) return "";
  return n === 1 ? "One section, figure or table was added since the mark and is read through the commons only."
    : `${n.toLocaleString("en-US")} sections, figures or tables were added since the mark and are read through the commons only.`;
}

const kindNames: Record<string, [string, string]> = {
  paper: ["paper", "papers"], dataset: ["dataset", "datasets"], tool: ["tool", "tools"], person: ["author", "authors"],
  object_collection: ["object collection", "object collections"], entity: ["entity", "entities"], lesson: ["note", "notes"],
  analysis: ["analysis", "analyses"],
};
const plural = (n: number, kind: string) => {
  const [one, many] = kindNames[kind] ?? [kind.replaceAll("_", " "), `${kind.replaceAll("_", " ")} records`];
  return `${n.toLocaleString("en-US")} ${n === 1 ? one : many}`;
};

// A collection's name from its source prefix: `literature://papers/` reads "papers literature" and a web address "Records from"
// its host. A short authority is read as an acronym; the full prefix stays in the technical details.
export function collectionName(prefix: string): string {
  const match = /^([a-z][a-z0-9+.-]*):\/\/([a-z0-9][a-z0-9._-]*)\//.exec(prefix);
  if (!match) return prefix;
  const [, scheme, authority] = match;
  if (scheme === "http" || scheme === "https") return `Records from ${authority}`;
  const who = authority.length <= 5 ? authority.toUpperCase() : authority;
  return `${who} ${scheme.replaceAll(/[+.-]/g, " ")}`;
}

// What a collection holds and how much of it every lab reads, in one sentence.
export function collectionWords(collection: CommonsCollection): { holds: string; reach: string; waiting: string } {
  const kinds = Object.entries(collection.kinds).filter(([, c]) => c.active > 0).sort(([a], [b]) => {
    const order = ["paper", "dataset", "tool", "person"];
    return (order.indexOf(a) + 1 || 99) - (order.indexOf(b) + 1 || 99) || a.localeCompare(b);
  });
  const active = kinds.reduce((n, [, c]) => n + c.active, 0);
  const marked = kinds.reduce((n, [, c]) => n + c.marked, 0);
  const holds = kinds.length ? kinds.map(([kind, c]) => plural(c.active, kind)).join(", ") : "No active records";
  const reach = !active ? "" : marked === 0 ? "Read through the commons only." : marked === active ? "Every lab's research agent reads all of it."
    : `Every lab reads ${marked.toLocaleString("en-US")} of its ${active.toLocaleString("en-US")} records.`;
  const n = collection.parts_waiting;
  const waiting = n < 1 ? "" : `${n.toLocaleString("en-US")} ${n === 1 ? "section, figure or table" : "sections, figures or tables"} of its marked papers ${n === 1 ? "was" : "were"} added since the mark.`;
  return { holds: `${holds}.`, reach, waiting };
}

export function collectionMarkedWords(prefix: string, marked: CollectionMarked): string {
  const name = collectionName(prefix);
  const parts = marked.parts_changed ? ` and ${marked.parts_changed.toLocaleString("en-US")} ${marked.parts_changed === 1 ? "section, figure or table" : "sections, figures and tables"}` : "";
  const records = `${marked.records_changed.toLocaleString("en-US")} ${marked.records_changed === 1 ? "record" : "records"}${parts}`;
  if (!marked.records_changed && !marked.parts_changed) return marked.baseline ? `${name} was already marked baseline.` : `${name} had no mark to withdraw.`;
  return marked.baseline ? `Marked ${records} of ${name} baseline, in your name. Every lab's research agent now reads them.`
    : `Withdrew the mark from ${records} of ${name}. They are read through the commons only now.`;
}

// For a person who leads the commons: how many records still wait for their decision.
export function waitingWords(count: number): string {
  return count === 0 ? "" : count === 1 ? "One record is not marked. Mark it if every lab should read it." : `${count} records are not marked. Mark those every lab should read.`;
}

// For everyone else: why no control is offered.
export function roleWords(view: Pick<CommonsView, "role" | "can_mark">): string {
  if (view.can_mark) return "";
  const role = view.role === "curator" ? "You curate the commons. " : view.role === "member" ? "You are a member of the commons. " : view.role === "lead" ? "You lead the commons, but this sign-in does not count as you acting directly. " : "";
  return `${role}Only a person who leads the commons can change what every lab receives.`;
}

// A record's reach, as the state word beside it.
export function reachWords(record: Pick<CommonsRecord, "baseline">): string {
  return record.baseline ? "Every lab reads it" : "Commons only";
}

const classes: Record<string, string> = {
  public_reference: "Public reference data", lab_database: "Lab database", object_data: "Object data", paper: "Paper",
  prior_output: "Earlier result", synthetic: "Synthetic",
};
const rights: Record<string, string> = { public: "public", licensed: "licensed", lab_internal: "lab internal", restricted: "restricted" };

// The record's class, rights and date in words; an unknown value is shown as it is, never guessed.
export function recordFacts(record: Pick<CommonsRecord, "data_class" | "rights" | "lifecycle" | "created_at">): string {
  const parts = [
    classes[record.data_class] ?? record.data_class,
    `rights ${rights[record.rights] ?? record.rights}`,
    record.lifecycle === "active" ? "" : record.lifecycle === "superseded" ? "superseded" : record.lifecycle === "withdrawn" ? "withdrawn" : record.lifecycle,
    day(record.created_at) ? `added ${day(record.created_at)}` : "",
  ].filter(Boolean);
  return parts.join(" · ");
}

export function markedWords(record: Pick<CommonsRecord, "baseline" | "title"> & { parts_changed?: number }): string {
  const n = record.parts_changed ?? 0;
  const parts = n === 1 ? " with one section, figure or table" : n > 1 ? ` with ${n.toLocaleString("en-US")} sections, figures and tables` : "";
  return record.baseline ? `Marked baseline${parts}: every lab's research agent now reads “${record.title}”.` : `Mark withdrawn${parts}: “${record.title}” is read only through the commons now.`;
}

// Whether a failed change may still have been made: the broker could not confirm it, the connection dropped after it
// was sent, or the failure is one the workspace does not know. The page then reloads what the service holds.
export function outcomeUnknown(cause: unknown): boolean {
  if (!(cause instanceof BrokerError)) return true;
  const reason = cause.reason || cause.code;
  return reason === "knowledge_outcome_unknown" || reason === "connection_unavailable" || !knownRefusals.has(reason);
}
// Refusals that come before anything changes.
const knownRefusals = new Set(["knowledge_not_configured", "baseline_refused", "baseline_not_active", "relation_not_found",
  "relation_not_proposed", "relation_review_refused", "knowledge_note_invalid", "resource_not_found", "person_not_enrolled",
  "person_assertion_invalid", "knowledge_person_required", "person_keys_unavailable", "knowledge_unavailable",
  "person_sign_in_stale", "collection_invalid", "knowledge_collection_invalid", "invalid_reference", "mutation_intent_required",
  "access_required", "knowledge_kind_invalid", "knowledge_cursor_invalid"]);

// A failed read or change in words. The knowledge service's refusal codes come through the broker unchanged. A change
// whose outcome is unknown never says that nothing changed.
export function commonsFailure(cause: unknown, action: "load" | "mark" | "links" | "review" | "collection"): string {
  const reason = cause instanceof BrokerError ? cause.reason || cause.code : "";
  const change = action === "mark" || action === "review" || action === "collection";
  if (change && outcomeUnknown(cause)) {
    return "The knowledge service did not confirm the change, so it may or may not have been made. The list was reloaded to show what the service holds now.";
  }
  switch (reason) {
    case "knowledge_not_configured": return "The knowledge service is not connected to this workspace yet.";
    case "baseline_refused": return "Only a person who leads the commons can change this.";
    case "baseline_not_active": return "Only an active record can be marked baseline.";
    case "relation_not_found": return "This link is no longer waiting, or your role in the lab does not review links.";
    case "relation_not_proposed": return "Someone has already reviewed this link. Reload to see the current list.";
    case "relation_review_refused": return "Only a person signed in directly can review a link.";
    case "knowledge_note_invalid": return "The note is at most 500 characters of plain text.";
    case "resource_not_found": return "This record is no longer in the commons. Reload to see the current list.";
    case "person_not_enrolled": return "The knowledge service does not know your sign-in yet. Ask the site's administrator to add you.";
    case "person_assertion_invalid": return "The knowledge service did not accept your sign-in. Sign in again, then retry.";
    case "person_sign_in_stale": return action === "collection"
      ? "Marking a whole collection needs a sign-in from the last 15 minutes. Sign out, sign in again, then retry. Nothing changed."
      : "Changes that reach every lab need a sign-in from the last hour. Sign out, sign in again, then retry. Nothing changed.";
    case "collection_invalid":
    case "knowledge_collection_invalid": return "This collection's address is not one the commons can mark. Nothing changed.";
    case "knowledge_person_required": return "Only a person signed in directly can do this.";
    case "person_keys_unavailable":
    case "knowledge_unavailable":
    case "connection_unavailable": return "The knowledge service is unavailable. Nothing changed; try again in a moment.";
  }
  return action === "load" ? "The commons could not be loaded. Nothing changed; reload to try again."
    : action === "links" ? "The proposed links could not be loaded. Nothing changed; reload to try again."
    : action === "review" ? "The review was refused. Nothing changed."
    : "The mark was refused. Nothing changed.";
}
