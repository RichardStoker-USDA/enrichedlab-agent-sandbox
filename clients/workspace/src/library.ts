import { BrokerError, request } from "./api.ts";
import type { Library, LibraryAdded, LibraryLabel, LibraryLabelNamespace, LibraryPaper, LibraryPaperView, LibrarySearch, PassageRead } from "./contracts.ts";
import type { ResearchTurn } from "./projects.ts";

// The lab library in plain words. Each lab keeps its papers in its own corpus of the document service,
// which reads the PDF (sections, pages, figures) and indexes its words. Search ranks passages by the words they share
// with the question, not by meaning. The broker decides who may add, label and remove; this module only words it.
export const libraryClaim =
  "Papers added here are read into the lab's own collection: its sections, pages and figure captions. Only members of the lab can see or search them.";
export const MAX_PDF_BYTES = 100 * 1024 * 1024;
export const MAX_REMOVE_REASON = 600;
export const MAX_LABEL = 80;

export const namespaces: { key: LibraryLabelNamespace; name: string }[] = [
  { key: "topic", name: "Topic" },
  { key: "organism", name: "Organism" },
  { key: "food", name: "Food" },
  { key: "compound", name: "Compound" },
  { key: "method", name: "Method" },
  { key: "design", name: "Study design" },
  { key: "population", name: "Population" },
];
export const namespaceName = (key: LibraryLabelNamespace) => namespaces.find((n) => n.key === key)?.name ?? key;

const UUID = /^[a-f0-9]{8}-[a-f0-9]{4}-[a-f0-9]{4}-[a-f0-9]{4}-[a-f0-9]{12}$/i;
const id = (value: string) => {
  if (!UUID.test(value)) throw new BrokerError("invalid_reference");
  return value;
};
const labPath = (key: string) => {
  if (!/^[a-z][a-z0-9_-]{1,63}$/.test(key)) throw new BrokerError("invalid_reference");
  return `/api/v1/labs/${key}/library`;
};
const mutate = { "Content-Type": "application/json", "X-Agent-Sandbox-Intent": "mutate" };

export const library = {
  list: (lab: string, signal?: AbortSignal) => request<Library>(labPath(lab), { signal }),
  paper: (lab: string, paper: string, signal?: AbortSignal) =>
    request<LibraryPaperView>(`${labPath(lab)}/papers/${id(paper)}`, { signal }),
  search: (lab: string, query: string, signal?: AbortSignal) =>
    request<LibrarySearch>(`${labPath(lab)}/search?q=${encodeURIComponent(query)}`, { signal: signal ?? AbortSignal.timeout(40000) }),
  // Reading a long paper can take a minute or two; the request key makes a repeated click safe.
  add: (lab: string, file: Blob, requestId: string) =>
    request<LibraryAdded>(`${labPath(lab)}/papers?request_id=${id(requestId)}`, {
      method: "POST",
      headers: { "Content-Type": "application/pdf", "X-Agent-Sandbox-Intent": "mutate" },
      body: file,
      signal: AbortSignal.timeout(240000),
    }),
  remove: async (lab: string, paper: string, body: { reason: string; request_id: string }) =>
    (await request<{ paper: LibraryPaper }>(`${labPath(lab)}/papers/${id(paper)}/remove`, { method: "POST", headers: mutate, body: JSON.stringify(body) })).paper,
  label: async (lab: string, paper: string, body: { kind: "added" | "removed"; namespace: LibraryLabelNamespace; value: string; request_id: string }) =>
    (await request<{ paper: LibraryPaper }>(`${labPath(lab)}/papers/${id(paper)}/labels`, { method: "POST", headers: mutate, body: JSON.stringify(body) })).paper,
  // A lab curator accepts a paper for the research agent, or withdraws that with a reason.
  accept: async (lab: string, paper: string, body: { accept: boolean; reason?: string; request_id: string }) =>
    (await request<{ paper: LibraryPaper }>(`${labPath(lab)}/papers/${id(paper)}/acceptance`, { method: "POST", headers: mutate, body: JSON.stringify(body) })).paper,
};

// Why a chosen file cannot be added, or "" when it can. The broker checks the same and more.
export function fileProblem(file: { name: string; type: string; size: number }): string {
  const pdf = file.type === "application/pdf" || (!file.type && /\.pdf$/i.test(file.name));
  if (!pdf) return "Choose a PDF file.";
  if (file.size === 0) return "This file is empty.";
  if (file.size > MAX_PDF_BYTES) return "This file is larger than the library accepts (100 MB).";
  return "";
}

export function byline(paper: Pick<LibraryPaper, "authors" | "year">): string {
  const names = paper.authors ?? [];
  const who = names.length === 0 ? "" : names.length <= 3 ? names.join(", ") : `${names.slice(0, 3).join(", ")} and ${names.length - 3} more`;
  return [who, paper.year ? String(paper.year) : ""].filter(Boolean).join(" · ");
}

export function addedWords(paper: LibraryPaper, day: (value: string) => string): string {
  const who = paper.added_by.you ? "you" : paper.added_by.kind === "service_account" ? "a service account" : paper.added_by.name?.trim() || "a colleague";
  return `Added by ${who} on ${day(paper.added_at)}.`;
}

// How many of the lab's papers still wait for a curator's acceptance, in one sentence.
export function unacceptedWords(count: number): string {
  return count === 1 ? "1 paper is not accepted for the research agent yet." : `${count} papers are not accepted for the research agent yet.`;
}

// One file of a batch added together: where it stands, in a few words.
export type BatchState = "waiting" | "reading" | "added" | "restored" | "present" | "failed" | "skipped" | "unknown";

// The broker's library codes that say nothing changed, whatever the status they come with.
const settledRefusals = new Set(["library_not_configured", "library_unavailable", "library_parse_failed",
  "library_paper_too_large", "library_not_pdf", "library_upload_quarantined", "library_upload_blocked",
  "library_guard_not_configured", "library_guard_unavailable", "library_guard_busy", "library_upload_quota",
  "library_busy", "library_upload_incomplete", "library_upload_length_required", "library_forbidden", "library_invalid",
  "paper_removed", "idempotency_conflict", "lab_not_found", "paper_not_found"]);

// Whether a write's outcome is unknown, so it may or may not have been saved. Only an answer that says nothing changed
// settles it as refused: one of the broker's codes above, or a refusal of the request itself (4xx). No answer, a
// server error without such a code (a database that failed mid-write included) and an answer that could not be read
// stay unknown until the library is read again.
export function libraryOutcomeUnknown(cause: unknown): boolean {
  if (!(cause instanceof BrokerError)) return true;
  if (cause.reason && settledRefusals.has(cause.reason)) return false;
  return !(cause.status >= 400 && cause.status < 500);
}

// Each file of a batch once the library was read again: a file whose outcome was unknown is added exactly when the
// fresh list holds a paper of its bytes, and refused otherwise. Without a fresh list it stays unknown.
export function settleUploads(states: BatchState[], hashes: (string | null)[], listed: Set<string> | null): BatchState[] {
  return states.map((state, i) => {
    const hash = hashes[i];
    if (state !== "unknown" || !listed || !hash) return state;
    return listed.has(hash) ? "added" : "failed";
  });
}

// Accepting every waiting paper, settled from a fresh list: which papers are accepted, which were refused (with the
// broker's reason) and which still have an unknown outcome because the list could not be read again.
export function settleAcceptance(ids: string[], refused: Map<string, string>, lost: Set<string>, accepted: Set<string> | null) {
  if (!accepted) {
    return { accepted: ids.filter((id) => !refused.has(id) && !lost.has(id)).length, refused: [...refused.keys()], unknown: [...lost] };
  }
  return {
    accepted: ids.filter((id) => accepted.has(id)).length,
    refused: ids.filter((id) => !accepted.has(id) && (refused.has(id) || lost.has(id))),
    unknown: [] as string[],
  };
}
export function batchItemWords(state: BatchState, problem = ""): string {
  switch (state) {
    case "waiting": return "Waiting.";
    case "reading": return "Reading its sections, pages and figure captions…";
    case "added": return "Added.";
    case "restored": return "Back in the library.";
    case "present": return "Already in the library.";
    case "skipped": return "Not read: the batch was stopped.";
    case "unknown": return "Its answer was lost, so it may or may not be in the library; reload the library to see.";
    case "failed": return problem || "Not added.";
  }
}

// What a batch did, in one sentence: papers added (or back), files already there, files not added or not read.
export function batchWords(states: BatchState[]): string {
  const count = (...kinds: BatchState[]) => states.filter((s) => kinds.includes(s)).length;
  const added = count("added", "restored");
  const present = count("present");
  const failed = count("failed");
  const skipped = count("skipped");
  const unknown = count("unknown");
  const parts = [
    added ? `${added === 1 ? "1 paper" : `${added} papers`} added` : "No paper added",
    present ? `${present === 1 ? "1 was" : `${present} were`} already in the library` : "",
    failed ? `${failed === 1 ? "1 file was" : `${failed} files were`} not added` : "",
    skipped ? `${skipped === 1 ? "1 file was" : `${skipped} files were`} not read` : "",
    unknown ? `${unknown === 1 ? "1 file's answer was" : `${unknown} files' answers were`} lost` : "",
  ].filter(Boolean);
  return `${parts.join("; ")}.`;
}

// What accepting every waiting paper at once did, in one sentence. Papers whose answer was lost and could not be
// checked are named as unknown, never as still waiting.
export function acceptAllWords(accepted: number, total: number, unknown = 0): string {
  if (accepted === total) return `${total === 1 ? "1 paper" : `${total} papers`} accepted for the research agent.`;
  const waiting = total - accepted - unknown;
  const parts = [`${accepted} of ${total} papers accepted for the research agent`];
  if (waiting) parts.push(`${waiting === 1 ? "1 still waits" : `${waiting} still wait`}`);
  if (unknown) parts.push(`${unknown === 1 ? "1 answer was" : `${unknown} answers were`} lost and the library could not be read again, so reload it to see`);
  return `${parts.join("; ")}.`;
}

// Whether a lab curator accepted the paper for the research agent, in one sentence.
export function acceptanceWords(paper: Pick<LibraryPaper, "acceptance">, day: (value: string) => string): string {
  if (!paper.acceptance) return "Not accepted for the research agent yet.";
  const who = paper.acceptance.by?.trim();
  return `Accepted for the research agent${who ? ` by ${who}` : ""} on ${day(paper.acceptance.accepted_at)}.`;
}

export function labelTitle(label: LibraryLabel): string {
  return label.state === "proposed"
    ? `${namespaceName(label.namespace)}: ${label.value}. Proposed by a service account; a person can set it.`
    : `${namespaceName(label.namespace)}: ${label.value}.`;
}

// What the parse found, in words. Counts only: nothing from the paper's text.
export function parseWords(paper: LibraryPaper): string {
  const parts: string[] = [];
  if (paper.page_count) parts.push(`${paper.page_count} ${paper.page_count === 1 ? "page" : "pages"}`);
  const figures = paper.parse.figures;
  if (typeof figures === "number") parts.push(`${figures} ${figures === 1 ? "figure" : "figures"} found`);
  const chunks = paper.parse.chunks;
  if (typeof chunks === "number") parts.push(`${chunks} searchable ${chunks === 1 ? "passage" : "passages"}`);
  return parts.join(" · ");
}

export function labelProblem(value: string): string {
  const clean = value.trim();
  if (!clean) return "Write the label.";
  if (clean.length > MAX_LABEL) return `Keep a label to ${MAX_LABEL} characters or fewer.`;
  return "";
}

export function queryProblem(value: string): string {
  const clean = value.trim();
  if (clean.length < 2) return "Search for at least two characters.";
  if (clean.length > 200) return "Keep a search to 200 characters or fewer.";
  return "";
}

// Why the upload check held or refused a file, from its finding codes, in a few words each. A code without a phrase
// here is not a reason to hold a file.
const holdWords: Record<string, string> = {
  empty_file: "it is empty",
  size_exceeded: "it is larger than uploads accept",
  type_not_allowed: "its type is not accepted",
  type_mismatch: "it is not the type it was sent as",
  polyglot: "it also contains the structure of another file format",
  structure_invalid: "its structure could not be read",
  encrypted_password: "it is protected with a password",
  unicode_tags: "its text holds hidden characters that can carry instructions",
  malware_signature: "the malware scanner matched a known threat",
  yara_high: "it contains content that matches a malware rule",
  yara_medium: "it contains content that matches a malware rule",
  analysis_failed: "it could not be checked within the upload check's limits",
  streams_unscanned: "it holds data the check could not read",
  disarm_failed: "it could not be rebuilt safely",
  disarm_residual: "active content remained after rebuilding",
  disarm_text_changed: "its text changed when it was rebuilt",
  disarm_text_unavailable: "its text could not be compared after rebuilding",
  image_too_large: "it holds images too large to open safely",
  embedded_archive: "it carries an archive inside its data",
  render_failed: "its pages could not be drawn within the upload check's limits",
};
// What the check removed from the copy the library keeps, and what it noted, for a paper it passed.
const removedWords: Record<string, string> = {
  active_javascript: "JavaScript",
  active_launch: "launch actions",
  active_embedded_file: "embedded files",
  active_open_action: "an action that ran when it opened",
  active_additional_actions: "event actions",
  active_xfa: "an XFA form",
  active_rich_media: "rich media",
  active_form_submit: "form actions that send data",
  active_remote_goto: "links that open other files",
  trailing_data: "data after the end of the file",
  encrypted_owner_only: "owner-password restrictions",
  private_streams: "data no page uses",
  content_irregular: "page content no viewer draws",
};
const notedWords: Record<string, string> = {
  hidden_text_white: "text drawn in white",
  hidden_text_tiny: "text smaller than 1 point",
  hidden_text_offpage: "text placed outside the page",
  hidden_text_invisible: "an invisible text layer",
  hidden_layers: "layers hidden by default",
  unicode_bidi: "characters that reverse text direction",
  unicode_zero_width: "zero-width characters",
  analysis_truncated: "pages too large to check fully",
  yara_low: "a low-severity content rule match",
};

const listWords = (items: string[]) =>
  items.length <= 1 ? (items[0] ?? "") : `${items.slice(0, -1).join(", ")} and ${items[items.length - 1]}`;

function holdReasons(findings: string[]): string {
  const reasons = [...new Set(findings.map((f) => holdWords[f]).filter(Boolean))];
  return reasons.length ? listWords(reasons) : "it needs a person's review";
}

// What the upload check did with a paper it passed, in one or two sentences; "" for a paper added before the check.
export function checkedWords(paper: Pick<LibraryPaper, "parse">): string {
  const guard = paper.parse.guard;
  if (!guard) return "";
  const findings = guard.findings ?? [];
  const removed = [...new Set(findings.map((f) => removedWords[f]).filter(Boolean))];
  const noted = [...new Set(findings.map((f) => notedWords[f]).filter(Boolean))];
  const parts = [removed.length ? `Checked on upload; the copy the library keeps has ${listWords(removed)} removed.` : "Checked on upload."];
  if (noted.length) parts.push(`The check noted ${listWords(noted)}.`);
  return parts.join(" ");
}

// Why a library request was refused, from the broker's fixed code. Nothing changed in each case.
export function libraryFailure(error: unknown, action: "load" | "add" | "search" | "label" | "remove" | "accept"): string {
  const reason = error instanceof BrokerError ? error.reason : "";
  switch (reason) {
    case "library_not_configured": return "The library is not set up for this lab yet. Papers can be added once it is.";
    case "library_unavailable": return "The service that reads papers is unavailable. Nothing changed; try again shortly.";
    case "library_parse_failed": return "This file could not be read as a paper, so it was not added. A scanned or protected PDF may need a text version.";
    case "library_paper_too_large": return "This file is larger than the library accepts (100 MB).";
    case "library_not_pdf": return "Choose a PDF file.";
    case "library_upload_quarantined": {
      const reference = error instanceof BrokerError && error.intake ? ` Its check reference is ${error.intake.slice(0, 8)}.` : "";
      return `This file was held for review and not added: ${holdReasons(error instanceof BrokerError ? error.findings : [])}. A lab lead can ask the platform operator to review it.${reference}`;
    }
    case "library_upload_blocked":
      return `This file was refused and not added: ${holdReasons(error instanceof BrokerError ? error.findings : [])}.`;
    case "library_guard_not_configured": return "Uploads are closed until the upload check is set up. Nothing changed.";
    case "library_guard_unavailable": return "The upload check is unavailable. Nothing changed; try again shortly.";
    case "library_guard_busy": return "The upload check is busy with other files. Nothing changed; try again in a minute.";
    case "library_upload_quota": return "Today's upload limit for your account is used. Nothing changed; try again tomorrow.";
    case "library_busy": return "The library is adding other papers. Nothing changed; try again in a minute.";
    case "library_upload_incomplete": return "The file did not arrive whole. Nothing changed; try again.";
    case "library_upload_length_required": return "The upload did not state its size. Nothing changed; try again.";
    case "library_paper_unchecked":
      return "This file matches a paper added before uploads were checked, which cannot be added again or restored. Nothing changed.";
    case "library_query_invalid": return "Search for 2 to 200 characters.";
    case "library_forbidden": return action === "accept" ? "Only a lead or reviewer of the lab can accept a paper for the research agent."
      : "Only a lead of the lab or the person who added this paper can remove it.";
    case "label_person_only": return "Only a person's account can remove a label.";
    case "label_invalid": return `Check the label: one of the kinds listed, up to ${MAX_LABEL} characters.`;
    case "paper_removed": return "This paper was removed from the library. Add the same file again to restore it.";
    case "library_invalid": return action === "remove" ? "Say why it should be removed."
      : action === "accept" ? "Say why the acceptance is withdrawn." : "Check the form and try again.";
    case "idempotency_conflict": return "This request changed while it was being saved. Try again.";
    case "lab_not_found": case "paper_not_found": return "This paper is not available to your account.";
  }
  if (error instanceof BrokerError && error.code === "connection_unavailable")
    return "The connection was lost before the library answered. Nothing is lost; try again.";
  switch (action) {
    case "load": return "The library could not be loaded. Nothing has changed; reload to try again.";
    case "add": return "The paper was not added. Try again shortly.";
    case "search": return "The search did not complete. Try again shortly.";
    case "label": return "The label was not saved. Try again shortly.";
    case "remove": return "The paper was not removed. Try again shortly.";
    case "accept": return "The acceptance was not saved. Try again shortly.";
  }
}

// What the research lead read from the library. A decision records each passage it was shown by paper,
// chunk or section, and pages; the lab's list names the papers. A paper the list no longer holds was removed after the agent read
// it; without the list (not a member, or not loaded) the paper is only known to be in the lab's library. A passage of the
// commons literature names its Commons paper by the title the decision recorded.
export interface PaperRead {
  paper_id: string; paper: LibraryPaper | null; state: "listed" | "gone" | "unknown" | "commons"; pages: string[]; title?: string;
}

function byPaper(passages: PassageRead[], papers: LibraryPaper[] | null): PaperRead[] {
  const out: PaperRead[] = [];
  for (const passage of passages) {
    let entry = out.find((e) => e.paper_id === passage.paper_id);
    if (!entry && passage.origin === "commons") {
      entry = { paper_id: passage.paper_id, paper: null, state: "commons", pages: [], title: passage.title };
      out.push(entry);
    }
    if (!entry) {
      const paper = papers?.find((p) => p.paper_id === passage.paper_id) ?? null;
      entry = { paper_id: passage.paper_id, paper, state: paper ? "listed" : papers ? "gone" : "unknown", pages: [] };
      out.push(entry);
    }
    if (passage.pages && !entry.pages.includes(passage.pages)) entry.pages.push(passage.pages);
  }
  return out;
}

// The papers behind the passages a decision's context showed, in the order first read.
export function papersRead(record: ResearchTurn["record"] | undefined, papers: LibraryPaper[] | null): PaperRead[] {
  return byPaper(record?.context_inputs?.literature?.passages ?? [], papers);
}

const LIBRARY_REF = /^library:\/\/([0-9a-f-]{36})\/([0-9a-f-]{36})$/i;
const LITERATURE_REF = /^literature:\/\/([0-9a-f-]{36})\/([0-9a-f-]{36})$/i;

// The papers the agent cited when it planned the step that became `taskId`: the decision that submitted the task
// names its step, and the step's literature_refs name passages that decision showed.
export function papersCited(turn: ResearchTurn | undefined, taskId: string, papers: LibraryPaper[] | null): PaperRead[] {
  const key = turn?.record.submitted_tasks?.find((t) => t.task_id === taskId)?.action_key;
  const action = key ? turn?.record.proposal.actions.find((a) => a.action_key === key) : undefined;
  const shown = turn?.record.context_inputs?.literature?.passages ?? [];
  const cited = (action?.literature_refs ?? []).flatMap((ref): PassageRead[] => {
    const commons = LITERATURE_REF.exec(ref);
    const match = commons ?? LIBRARY_REF.exec(ref);
    if (!match) return [];
    const [paper, place] = [match[1].toLowerCase(), match[2].toLowerCase()];
    // The scheme must agree with what the decision recorded: a literature:// ref names only a Commons passage, and a
    // library:// ref only a library one, whatever their identifiers.
    const found = shown.find((p) => p.paper_id === paper && (p.chunk_id === place || p.section_id === place)
      && (p.origin === "commons") === Boolean(commons));
    return [found ?? (commons ? { paper_id: paper, text_sha256: "", origin: "commons" } : { paper_id: paper, text_sha256: "" })];
  });
  return byPaper(cited, papers);
}

// "Title, pages 2-3" for a paper the list holds; plain words for one it does not.
export function paperReadWords(item: PaperRead): string {
  if (item.state === "commons") return [`${item.title?.trim() || "A paper"}, from the Commons`, ...item.pages].join(", ");
  if (item.state === "gone") return "a paper since removed from the lab's library";
  if (item.state === "unknown") return "a paper in the lab's library";
  return [item.paper?.title?.trim() || "An untitled paper", ...item.pages].join(", ");
}
