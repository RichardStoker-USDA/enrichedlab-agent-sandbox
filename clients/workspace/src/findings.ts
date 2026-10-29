// The agent's answer and findings. A finding is the agent's sentence with its result
// values left as placeholders, short lowercase names in braces such as {a} or {oj_mean}; each names one cell of a table in a retained
// result package. The values shown come from the stored digest of that package, never from the
// agent's text, and a value that cannot be found is shown as missing.
export type Answer = "answered" | "partly_answered" | "not_answered" | "not_yet";
export interface Binding { ref: string; table: string; row: number; column: string }
export interface Finding { statement: string; bindings: Record<string, Binding> }
export interface DigestTable { path: string; rows: number; columns: string[]; cells?: string[][]; cells_omitted?: string }
export interface ArtifactDigest {
  artifact_id: string; artifact_sha256: string; status: string; digest_sha256: string | null; recorded_at: string | null;
  digest: { tables?: DigestTable[] } | null;
}
export type Part = { text: string } | { slot: string; value: string | null; binding: Binding };

export const answerWords: Record<Answer, string> = {
  answered: "It says the results answer your question.",
  partly_answered: "It says the results answer part of your question.",
  not_answered: "It says the results do not answer your question.",
  not_yet: "It has not said yet whether the results answer your question.",
};

const REF = /^artifact:\/\/tasks\/([0-9a-f-]{36})\/([0-9a-f-]{36})$/i;
export function artifactOf(ref: string): { task: string; artifact: string } | null {
  const m = REF.exec(ref);
  return m ? { task: m[1], artifact: m[2] } : null;
}

/** The cell a binding names, as written in the retained file, or null. */
export function cell(binding: Binding, digests: Record<string, ArtifactDigest | undefined>): string | null {
  const digest = digests[binding.ref];
  const table = digest?.digest?.tables?.find((t) => t.path === binding.table);
  const column = table?.columns.indexOf(binding.column) ?? -1;
  const row = table?.cells?.[binding.row - 1];
  return row && column >= 0 && column < row.length ? row[column] : null;
}

/** The sentence split into text and filled placeholders. */
export function parts(finding: Finding, digests: Record<string, ArtifactDigest | undefined>): Part[] {
  const out: Part[] = [];
  const pattern = /\{([a-z][a-z0-9_]{0,23})\}/g;
  let last = 0;
  for (const m of finding.statement.matchAll(pattern)) {
    if (m.index > last) out.push({ text: finding.statement.slice(last, m.index) });
    const binding = finding.bindings[m[1]];
    out.push(binding ? { slot: m[1], binding, value: cell(binding, digests) } : { text: m[0] });
    last = m.index + m[0].length;
  }
  if (last < finding.statement.length) out.push({ text: finding.statement.slice(last) });
  return out;
}

/** A finding's parts with each value glued to the punctuation that touches it, such as the brackets in "(12.38)," so a
 *  line never breaks between a value and its bracket. Text keeps every character; only where it is split moves. */
export type Glued = { text: string } | { before: string; value: Extract<Part, { slot: string }>; after: string };
export function glued(list: Part[]): Glued[] {
  const out: Glued[] = [];
  for (let i = 0; i < list.length; i++) {
    const part = list[i];
    if ("text" in part) { out.push({ text: part.text }); continue; }
    const previous = out.at(-1);
    let before = "";
    if (previous && "text" in previous) {
      before = /\S*$/.exec(previous.text)?.[0] ?? "";
      previous.text = previous.text.slice(0, previous.text.length - before.length);
      if (!previous.text) out.pop();
    }
    const next = list[i + 1];
    let after = "";
    if (next && "text" in next) {
      after = /^\S*/.exec(next.text)?.[0] ?? "";
      list = [...list.slice(0, i + 1), { text: next.text.slice(after.length) }, ...list.slice(i + 2)];
    }
    out.push({ before, value: part, after });
  }
  return out.filter((g) => !("text" in g) || g.text);
}

/** A value as shown in a sentence: a decimal with more than four significant digits is shown to four;
 *  whole numbers and anything that is not a plain number are shown as written. The exact text from the
 *  file stays in the element's value and title. */
export function display(value: string): string {
  const text = value.trim();
  if (!/^-?(\d+\.?\d*|\.\d+)(e[-+]?\d+)?$/i.test(text)) return value;
  const n = Number(text);
  // Whole numbers are counts or codes: show them as written. Other values show at most
  // four significant digits; the exact text stays in the value's title.
  if (!Number.isFinite(n) || /^-?\d+$/.test(text)) return value;
  const digits = text.replace(/^-/, "").replace(/e.*$/i, "").replace(".", "").replace(/^0+/, "").length;
  if (digits <= 4) return value;
  const shown = Math.abs(n) >= 1e-4 && Math.abs(n) < 1e6 ? Number(n.toPrecision(4)).toString() : n.toPrecision(4);
  return shown.replace(/e\+?/, "e");
}

/** Where a value came from, in words. */
export function sourceWords(binding: Binding): string {
  const file = binding.table.split("/").at(-1) ?? binding.table;
  return `From ${file}, row ${binding.row}, column “${binding.column}”`;
}

/** One part of the question as the agent accounts for it. */
export type PartState = "supported" | "partial" | "unsupported" | "open" | "awaiting";
export interface LedgerPart { part: string; state: PartState; reason?: string | null }
/** The state of a part in plain words; the agent's reading, never a check. */
export const partStateWords: Record<PartState, string> = {
  supported: "Answered",
  partial: "Partly answered",
  unsupported: "Not in this data",
  open: "Still being worked on",
  awaiting: "Waiting for your choice",
};
const PART_STATES = new Set(Object.keys(partStateWords));

interface TurnLike { created_at: string; record: { outcome?: string; proposal: { answer?: Answer; findings?: Finding[]; tables?: unknown; parts?: unknown } } }
export interface Stated { answer: Answer; findings: Finding[]; tables: AnswerTable[]; at: string; parts: LedgerPart[] }

/** A whole result table the agent gave as part of its answer: it named the table, the columns to show and
 *  the words that open it, and code counted its rows. Every row shown comes from the retained file, never from the
 *  agent's text. */
export interface AnswerTable { ref: string; table: string; columns: string[]; lead_in: string; rows: number }

/** A recorded answer's tables that are in shape; anything else is left out. */
export function answerTables(tables: unknown): AnswerTable[] {
  if (!Array.isArray(tables)) return [];
  return tables.flatMap((t) => {
    if (t === null || typeof t !== "object") return [];
    const { ref, table, columns, lead_in, rows } = t as Record<string, unknown>;
    if (typeof ref !== "string" || !artifactOf(ref) || typeof table !== "string" || !table || typeof lead_in !== "string"
      || !Array.isArray(columns) || !columns.length || !columns.every((c) => typeof c === "string")
      || typeof rows !== "number" || !Number.isInteger(rows) || rows < 0) return [];
    return [{ ref, table, columns: columns as string[], lead_in, rows }];
  });
}

/** What an answer table shows from its result's stored digest: the named columns of every row when the digest holds
 *  the table's cells, or, when it does not, why (the table is larger than a digest holds, or its cells are a lab's
 *  and are read from the analysis itself), with the row count the agent's record gives. A column the digest does not
 *  list leaves the table unshown rather than shown in part. */
export type TableShown = { columns: string[]; rows: string[][] } | { omitted: string; rows: number };
export function answerTableRows(table: AnswerTable, digests: Record<string, ArtifactDigest | undefined>): TableShown {
  const stored = digests[table.ref]?.digest?.tables?.find((t) => t.path === table.table);
  if (!stored) return { omitted: "not_loaded", rows: table.rows };
  const at = table.columns.map((c) => stored.columns.indexOf(c));
  if (!stored.cells || at.some((i) => i < 0)) return { omitted: stored.cells_omitted ?? "columns", rows: stored.rows };
  return { columns: table.columns, rows: stored.cells.map((row) => at.map((i) => row[i] ?? "")) };
}

/** A recorded ledger's parts with a known state and words; anything else is left out. */
export function ledgerParts(parts: unknown): LedgerPart[] {
  if (!Array.isArray(parts)) return [];
  return parts.flatMap((p) => {
    if (p === null || typeof p !== "object") return [];
    const { part, state, reason } = p as Record<string, unknown>;
    if (typeof part !== "string" || !part.trim() || typeof state !== "string" || !PART_STATES.has(state)) return [];
    return [{ part, state: state as PartState, reason: typeof reason === "string" && reason.trim() ? reason : null }];
  });
}

/** The agent's current answer and the findings it stated before it, across every work
 *  session of the project. The newest decision that states an answer decides, even when it states no
 *  findings; earlier decisions that stated findings are dated history, newest first, and never stand
 *  for the current answer. */
export function answerHistory(runs: { turns: TurnLike[] }[], keep = 3): { current: Stated | null; earlier: Stated[] } {
  // A proposal the methods check sent back was never the agent's answer.
  const stated = runs.flatMap(({ turns }) => turns)
    .filter((t) => t.record.outcome !== "revise")
    .filter((t) => t.record.proposal.answer !== undefined || !!t.record.proposal.findings?.length
      || answerTables(t.record.proposal.tables).length > 0)
    .sort((a, b) => b.created_at.localeCompare(a.created_at))
    .map((t) => ({ answer: t.record.proposal.answer ?? "not_yet", findings: t.record.proposal.findings ?? [],
      tables: answerTables(t.record.proposal.tables), at: t.created_at, parts: ledgerParts(t.record.proposal.parts) }));
  const [current = null, ...rest] = stated;
  return { current, earlier: rest.filter((s) => s.findings.length || s.tables.length).slice(0, keep) };
}

/** Every finding the page shows: the current answer's and the earlier ones kept as history. */
export function shownFindings(history: { current: Stated | null; earlier: Stated[] }): Finding[] {
  return [...(history.current?.findings ?? []), ...history.earlier.flatMap((s) => s.findings)];
}

/** Every answer table the page shows: the current answer's and the earlier ones kept as history. */
export function shownTables(history: { current: Stated | null; earlier: Stated[] }): AnswerTable[] {
  return [...(history.current?.tables ?? []), ...history.earlier.flatMap((s) => s.tables)];
}

/** The distinct result packages a list of findings, and of answer tables, reads. */
export function referencedArtifacts(findings: Finding[], tables: AnswerTable[] = []): { ref: string; task: string; artifact: string }[] {
  const seen = new Map<string, { ref: string; task: string; artifact: string }>();
  const refs = [...findings.flatMap((f) => Object.values(f.bindings).map((b) => b.ref)), ...tables.map((t) => t.ref)];
  for (const ref of refs) {
    const a = artifactOf(ref);
    if (a && !seen.has(ref)) seen.set(ref, { ref, ...a });
  }
  return [...seen.values()];
}
