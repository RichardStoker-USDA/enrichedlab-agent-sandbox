// The live log (Glass Box results rule 6). A running analysis's output
// arrives as numbered pieces. The page filters each piece again, so only colour and weight codes, line feeds, carriage
// returns and tabs reach it; nothing in a log can move a cursor, set a link or reach the clipboard. Pieces become lines
// here, and when an attempt ends the text the page was sent is checked against the broker's record of it.
import { sha256Hex } from "./canonical.ts";

export type LogStream = "stdout" | "stderr" | "combined";
export interface LogPiece { seq: number; attempt: number; stream: LogStream; text: string; bytes: number; at: string; dropped: number }
/** The end of the stream. A step that ended without a closure record has `reason: "no_closure"` and
 *  no counts: its pieces were never checked. */
export interface LogClosed {
  attempt: number | null; recorded: boolean; reason: "no_closure" | null;
  pieces: number | null; size: number | null; sha256: string | null; dropped: number | null;
  artifacts: { artifact: string; role: "stdout" | "stderr"; sha256: string; size: number }[];
}

const count = (v: unknown): v is number => Number.isSafeInteger(v) && (v as number) >= 0;
const STREAMS = new Set(["stdout", "stderr", "combined"]);

/** A piece as the broker sent it, or null when a field is not what the shape names. */
export function pieceFrom(value: unknown): LogPiece | null {
  const v = value as Partial<LogPiece> | null;
  if (!v || !count(v.seq) || v.seq < 1 || !count(v.attempt) || !STREAMS.has(v.stream as string) || typeof v.text !== "string"
    || !count(v.bytes) || typeof v.at !== "string" || Number.isNaN(Date.parse(v.at)) || !count(v.dropped)) return null;
  return { seq: v.seq, attempt: v.attempt, stream: v.stream as LogStream, text: v.text, bytes: v.bytes, at: v.at, dropped: v.dropped };
}

/** The end of the stream as the broker sent it, or null when a field is not what the shape names. A closure without a
 *  record carries nulls for every count and is never recorded. */
export function closedFrom(value: unknown): LogClosed | null {
  const v = value as (Partial<Omit<LogClosed, "reason">> & { reason?: unknown }) | null;
  if (!v || !(v.attempt === null || count(v.attempt)) || typeof v.recorded !== "boolean" || !Array.isArray(v.artifacts)) return null;
  const artifacts = v.artifacts.filter((a) => a && typeof a.artifact === "string" && (a.role === "stdout" || a.role === "stderr")
    && typeof a.sha256 === "string" && count(a.size));
  if (artifacts.length !== v.artifacts.length) return null;
  const base = { attempt: v.attempt ?? null, recorded: v.recorded, artifacts };
  if (v.reason === "no_closure") {
    if (v.recorded || v.pieces !== null || v.size !== null || v.sha256 !== null || v.dropped !== null) return null;
    return { ...base, reason: "no_closure", pieces: null, size: null, sha256: null, dropped: null };
  }
  if (v.reason !== undefined || !count(v.pieces) || !count(v.size) || typeof v.sha256 !== "string" || !count(v.dropped)) return null;
  return { ...base, reason: null, pieces: v.pieces, size: v.size, sha256: v.sha256, dropped: v.dropped };
}

// Text direction overrides can make a line read in another order than it was written, so they are dropped too.
const BIDI = /[‪-‮⁦-⁩]/;

/** One piece's text with every control except colour and weight codes, line feeds, carriage returns and tabs removed.
 *  An escape cut off by the end of the piece comes back as `rest`, to be read with the next piece of the same stream. */
export function clean(text: string): { text: string; rest: string } {
  let out = "";
  for (let i = 0; i < text.length; i++) {
    const c = text[i], code = text.charCodeAt(i);
    if (c === "\x1b") {
      const next = text[i + 1];
      if (next === undefined) return { text: out, rest: text.slice(i) };
      if (next === "[") {
        let j = i + 2;
        while (j < text.length && /[\x30-\x3f]/.test(text[j])) j++;
        while (j < text.length && /[\x20-\x2f]/.test(text[j])) j++;
        if (j >= text.length) return text.length - i > 64 ? { text: out, rest: "" } : { text: out, rest: text.slice(i) };
        const sequence = text.slice(i, j + 1);
        if (/^\x1b\[[0-9;:]*m$/.test(sequence)) out += sequence;
        i = j;
        continue;
      }
      if ("]PX^_".includes(next)) {
        // A string command (a title, a link, a clipboard request) runs to BEL or ESC backslash and is dropped whole.
        let j = i + 2;
        while (j < text.length && text[j] !== "\x07" && !(text[j] === "\x1b" && text[j + 1] === "\\")) j++;
        if (j >= text.length) return text.length - i > 512 ? { text: out, rest: "" } : { text: out, rest: text.slice(i) };
        i = text[j] === "\x07" ? j : j + 1;
        continue;
      }
      i += 1;
      continue;
    }
    if (c === "\n" || c === "\r" || c === "\t") { out += c; continue; }
    if (code < 0x20 || code === 0x7f || (code >= 0x80 && code <= 0x9f) || BIDI.test(c)) continue;
    out += c;
  }
  return { text: out, rest: "" };
}

/** The tones a log can ask for, by meaning: red and magenta read as alarm, blue and cyan as a note, yellow and green as
 *  emphasis. Black, white and the 256-colour and direct colours fall back to the page's ink. */
export type Tone = "alarm" | "note" | "strong";
export interface Style { bold?: boolean; dim?: boolean; italic?: boolean; underline?: boolean; tone?: Tone }
export interface Span { text: string; style: Style }

const TONES: Record<number, Tone | undefined> = { 1: "alarm", 2: "strong", 3: "strong", 4: "note", 5: "alarm", 6: "note" };

/** The style after one colour and weight code. A parameter may carry colon subparameters (`38:5:196`, `4:3`). */
export function applySgr(style: Style, params: string): Style {
  const codes: number[] = [];
  for (const part of params === "" ? ["0"] : params.split(";")) {
    if (!part.includes(":")) { codes.push(part === "" ? 0 : Number(part)); continue; }
    const [head, ...rest] = part.split(":").map((x) => (x === "" ? 0 : Number(x)));
    // A colour as 38:5:n or 38:2:[space]:r:g:b reads as its ;-separated form; an underline style (4:n) as underline or none.
    if (head === 38 || head === 48) codes.push(head, rest[0], ...(rest[0] === 2 && rest.length === 5 ? rest.slice(2) : rest.slice(1)));
    else if (head === 4) codes.push(rest[0] === 0 ? 24 : 4);
    else codes.push(head);
  }
  let next: Style = { ...style };
  for (let k = 0; k < codes.length; k++) {
    const n = codes[k];
    if (n === 0) next = {};
    else if (n === 1) next.bold = true;
    else if (n === 2) next.dim = true;
    else if (n === 3) next.italic = true;
    else if (n === 4) next.underline = true;
    else if (n === 22) { delete next.bold; delete next.dim; }
    else if (n === 23) delete next.italic;
    else if (n === 24) delete next.underline;
    else if ((n >= 30 && n <= 37) || (n >= 90 && n <= 97)) { const tone = TONES[n % 10]; if (tone) next.tone = tone; else delete next.tone; }
    else if (n === 39) delete next.tone;
    else if (n === 38 || n === 48) {
      // 256-colour and direct colours: the first sixteen indexes keep their tone; the rest use the page's ink.
      if (codes[k + 1] === 5) {
        const index = codes[k + 2];
        if (n === 38 && index !== undefined && index < 16) { const tone = TONES[index % 8]; if (tone) next.tone = tone; else delete next.tone; }
        else if (n === 38) delete next.tone;
        k += 2;
      } else if (codes[k + 1] === 2) { if (n === 38) delete next.tone; k += 4; }
    }
  }
  return next;
}

export type LogRow =
  | { kind: "line"; n: number; attempt: number; stream: LogStream; at: string; seq: number; spans: Span[]; plain: string; open?: boolean }
  | { kind: "attempt"; attempt: number; at: string }
  | { kind: "system"; at: string; text: string };

type Open = { spans: Span[]; at: string; seq: number; cr: boolean };
const same = (a: Style, b: Style) => a.bold === b.bold && a.dim === b.dim && a.italic === b.italic && a.underline === b.underline && a.tone === b.tone;

/** Lines from pieces, as they arrive. A piece already seen is ignored; a carriage return starts its line again, as a
 *  progress counter does; each stream keeps its own unfinished line and style. `removed` counts the bytes the broker's
 *  filter took out of the pieces (control codes the page would not show anyway). */
export class LogBuilder {
  rows: LogRow[] = [];
  last = 0;
  attempt = 0;
  removed = 0;
  /** The texts of each attempt's pieces in the order they came, for the check against the broker's record. */
  readonly sent = new Map<number, { seq: number; text: string }[]>();
  private lines = 0;
  private open = new Map<LogStream, Open>();
  private rest = new Map<LogStream, string>();
  private style = new Map<LogStream, Style>();

  add(piece: LogPiece): boolean {
    if (piece.seq <= this.last) return false;
    this.last = piece.seq;
    if (piece.attempt !== this.attempt) {
      if (this.attempt) {
        this.finishAll();
        this.rows.push({ kind: "attempt", attempt: piece.attempt, at: piece.at });
      }
      this.attempt = piece.attempt;
      this.rest.clear(); this.style.clear();
    }
    this.sent.set(piece.attempt, [...(this.sent.get(piece.attempt) ?? []), { seq: piece.seq, text: piece.text }]);
    this.removed += piece.dropped;
    const { text, rest } = clean((this.rest.get(piece.stream) ?? "") + piece.text);
    this.rest.set(piece.stream, rest);
    let style = this.style.get(piece.stream) ?? {};
    for (let i = 0; i < text.length; i++) {
      const c = text[i];
      if (c === "\x1b") {
        const end = text.indexOf("m", i);
        style = applySgr(style, text.slice(i + 2, end));
        i = end;
        continue;
      }
      const line = this.lineOf(piece);
      if (c === "\n") { line.cr = false; this.finish(piece.stream); continue; }
      if (c === "\r") { line.cr = true; continue; }
      if (line.cr) { line.spans = []; line.cr = false; }
      const tail = line.spans.at(-1);
      if (tail && same(tail.style, style)) tail.text += c;
      else line.spans.push({ text: c, style });
    }
    this.style.set(piece.stream, style);
    return true;
  }

  /** The finished lines and markers, then each stream's unfinished line. */
  view(): LogRow[] {
    const open = [...this.open.entries()].filter(([, line]) => line.spans.length).map(([stream, line]) =>
      ({ kind: "line" as const, n: 0, attempt: this.attempt, stream, at: line.at, seq: line.seq, spans: line.spans.map((s) => ({ ...s })),
        plain: line.spans.map((s) => s.text).join(""), open: true }));
    // Always a new array: a view that hands back the same array is equal to the last one, so a page that redraws on a
    // changed value would keep showing the old lines.
    return [...this.rows, ...open];
  }

  private lineOf(piece: LogPiece): Open {
    let line = this.open.get(piece.stream);
    if (!line) { line = { spans: [], at: piece.at, seq: piece.seq, cr: false }; this.open.set(piece.stream, line); }
    return line;
  }
  private finish(stream: LogStream) {
    const line = this.open.get(stream);
    if (!line) return;
    this.open.delete(stream);
    this.rows.push({ kind: "line", n: ++this.lines, attempt: this.attempt, stream, at: line.at, seq: line.seq, spans: line.spans,
      plain: line.spans.map((s) => s.text).join("") });
  }
  private finishAll() { for (const stream of [...this.open.keys()]) this.finish(stream); }
}

export type RecordCheck =
  | { state: "checked"; pieces: number; size: number; sha256: string }
  | { state: "differs" | "incomplete"; missing: number }
  | { state: "not_recorded" };

/** Whether the text the page was sent for the last attempt is the text the broker kept: the same pieces, and the same
 *  size and SHA-256 over their texts joined in order. */
export async function checkRecord(sent: Map<number, { seq: number; text: string }[]>, closed: LogClosed): Promise<RecordCheck> {
  if (!closed.recorded || closed.pieces === null || closed.size === null || closed.sha256 === null) return { state: "not_recorded" };
  const pieces = [...(closed.attempt === null ? [] : sent.get(closed.attempt) ?? [])].sort((a, b) => a.seq - b.seq);
  if (pieces.length < closed.pieces) return { state: "incomplete", missing: closed.pieces - pieces.length };
  const text = pieces.map((p) => p.text).join("");
  const size = new TextEncoder().encode(text).length;
  const digest = await sha256Hex(text);
  const expected = closed.sha256.replace(/^sha256:/, "").toLowerCase();
  return pieces.length === closed.pieces && size === closed.size && digest === expected
    ? { state: "checked", pieces: pieces.length, size, sha256: digest } : { state: "differs", missing: 0 };
}

/** Where a query appears in the rows' plain text, ignoring case: one entry per match. */
export function findIn(rows: LogRow[], query: string): { row: number; start: number; end: number }[] {
  const q = query.toLowerCase();
  if (!q) return [];
  const out: { row: number; start: number; end: number }[] = [];
  rows.forEach((r, row) => {
    const text = r.kind === "line" ? r.plain.toLowerCase() : r.kind === "system" ? r.text.toLowerCase() : "";
    for (let at = text.indexOf(q); at >= 0; at = text.indexOf(q, at + q.length)) out.push({ row, start: at, end: at + q.length });
  });
  return out;
}

/** A line's spans cut at the matches, each piece marked as a match, the current match, or neither. */
export function marked(spans: Span[], matches: { start: number; end: number; current: boolean }[]): (Span & { hit?: "match" | "current" })[] {
  if (!matches.length) return spans;
  const out: (Span & { hit?: "match" | "current" })[] = [];
  let at = 0;
  for (const span of spans) {
    const from = at, to = at + span.text.length;
    const cuts = new Set([from, to]);
    for (const m of matches) { if (m.start > from && m.start < to) cuts.add(m.start); if (m.end > from && m.end < to) cuts.add(m.end); }
    const points = [...cuts].sort((a, b) => a - b);
    for (let k = 0; k + 1 < points.length; k++) {
      const hit = matches.find((m) => m.start <= points[k] && m.end >= points[k + 1]);
      out.push({ text: span.text.slice(points[k] - from, points[k + 1] - from), style: span.style, ...(hit ? { hit: hit.current ? "current" : "match" } : {}) });
    }
    at = to;
  }
  return out;
}

/** How long a running step's output has been quiet, in words. */
export function quietWords(ms: number): string {
  const s = Math.max(0, Math.floor(ms / 1000));
  if (s < 60) return `${s} second${s === 1 ? "" : "s"}`;
  const m = Math.floor(s / 60);
  if (m < 60) return `${m} minute${m === 1 ? "" : "s"}`;
  const h = Math.floor(m / 60), rest = m % 60;
  return `${h} hour${h === 1 ? "" : "s"}${rest ? ` ${rest} minute${rest === 1 ? "" : "s"}` : ""}`;
}

/** The rows as plain text, as copied: markers in brackets. */
export function plainText(rows: LogRow[]): string {
  return rows.map((r) => r.kind === "line" ? r.plain : r.kind === "system" ? `[${r.text}]` : `[Attempt ${r.attempt} started]`).join("\n");
}

/** The step's own lifecycle lines (System) placed among the log's rows by time. */
export function withSystem(rows: LogRow[], system: { at: string; text: string }[]): LogRow[] {
  if (!system.length) return rows;
  const out: LogRow[] = [];
  const queue = [...system].sort((a, b) => Date.parse(a.at) - Date.parse(b.at));
  for (const row of rows) {
    while (queue.length && Date.parse(queue[0].at) <= Date.parse(row.at)) out.push({ kind: "system", ...queue.shift()! });
    out.push(row);
  }
  for (const rest of queue) out.push({ kind: "system", ...rest });
  return out;
}

/** A row's time of day to the second, as the Times switch shows it. */
export function clock(at: string): string {
  return new Date(at).toLocaleTimeString(undefined, { hour: "2-digit", minute: "2-digit", second: "2-digit", hour12: false });
}
