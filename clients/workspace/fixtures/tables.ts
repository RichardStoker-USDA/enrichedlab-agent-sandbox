import { createHash } from "node:crypto";
import { canonicalJson } from "../src/canonical.ts";

// Server pages and column summaries of a stored table for the loopback preview. They
// follow the broker's rules: values are the stored strings; types are inferred from every stored value; sorting orders
// integers, numbers and logicals by value and strings by byte order, with missing values (empty or NA) last in both
// orders and ties in file order. A table the rules cannot read is refused with the broker's codes, never repaired.
// Limits and codes follow the broker's contract (agent-sandbox `docs/result_tables.md`, October 12).

type Json = (value: unknown, status?: number) => void;
export interface StoredTable { sha256: string; columns: string[]; rows: { index: number; line: number; values: string[] }[] }
export interface Refused { code: string; status: number }
const LIMITS = { bytes: 16 * 1024 * 1024, rows: 1_000_000, columns: 1000, cells: 2_000_000 };
const sha = (bytes: Buffer | string) => createHash("sha256").update(bytes).digest("hex");
const missing = (value: string) => value === "" || value === "NA";

/** RFC 4180 records with the line each starts on; quoted fields may hold the delimiter, doubled quotes and line breaks.
 *  Lines end at LF, CRLF or a lone CR, and blank lines count. */
export function readTable(bytes: Buffer, delimiter: string): StoredTable | Refused {
  if (bytes.length > LIMITS.bytes) return { code: "table_too_large", status: 413 };
  let text: string;
  try { text = new TextDecoder("utf-8", { fatal: true }).decode(bytes); } catch { return { code: "table_unreadable", status: 422 }; }
  const records: { line: number; fields: string[] }[] = [];
  let fields: string[] = [], field = "", quoted = false, line = 1, start = 1, started = false;
  for (let i = 0; i < text.length; i++) {
    const c = text[i];
    if (!started) { started = true; start = line; }
    if (quoted) {
      if (c === '"' && text[i + 1] === '"') { field += '"'; i++; }
      else if (c === '"') quoted = false;
      else { if (c === "\n" || (c === "\r" && text[i + 1] !== "\n")) line++; field += c; }
    } else if (c === '"') quoted = true;
    else if (c === delimiter) { fields.push(field); field = ""; }
    else if (c === "\n" || (c === "\r" && text[i + 1] !== "\n")) { fields.push(field); records.push({ line: start, fields }); fields = []; field = ""; line++; started = false; }
    else if (c !== "\r") field += c;
  }
  if (started && (field || fields.length)) { fields.push(field); records.push({ line: start, fields }); }
  const [header, ...body] = records.filter((r) => !(r.fields.length === 1 && r.fields[0] === ""));
  if (!header || header.fields.some((name) => !name.trim()) || new Set(header.fields).size !== header.fields.length) return { code: "table_unreadable", status: 422 };
  if (header.fields.length > LIMITS.columns || body.length > LIMITS.rows || header.fields.length * body.length > LIMITS.cells) return { code: "table_too_large", status: 413 };
  if (body.some((r) => r.fields.length !== header.fields.length)) return { code: "table_unreadable", status: 422 };
  return { sha256: sha(bytes), columns: header.fields, rows: body.map((r, index) => ({ index, line: r.line, values: r.fields })) };
}

const INTEGER = /^[-+]?\d+$/, NUMBER = /^[-+]?(\d+\.?\d*|\.\d+)([eE][-+]?\d+)?$/, LOGICAL = /^(TRUE|FALSE)$/;
/** Each column's type from every stored value: integer, number, logical or string. */
export function typesOf(table: StoredTable): string[] {
  return table.columns.map((_, c) => {
    const present = table.rows.map((r) => r.values[c]).filter((v) => !missing(v));
    if (!present.length) return "string";
    if (present.every((v) => INTEGER.test(v))) return "integer";
    if (present.every((v) => NUMBER.test(v))) return "number";
    if (present.every((v) => LOGICAL.test(v))) return "logical";
    return "string";
  });
}

const compare = (type: string) => (a: string, b: string) => type === "integer" || type === "number" ? Number(a) - Number(b)
  : type === "logical" ? Number(a === "TRUE") - Number(b === "TRUE") : Buffer.compare(Buffer.from(a), Buffer.from(b));

export function pageOf(table: StoredTable, query: URLSearchParams): Record<string, unknown> | Refused {
  const offset = Number(query.get("offset") ?? 0), limit = Number(query.get("limit") ?? 100);
  if (!Number.isInteger(offset) || offset < 0) return { code: "page_offset_invalid", status: 400 };
  if (!Number.isInteger(limit) || limit < 1 || limit > 1000) return { code: "page_limit_invalid", status: 400 };
  const column = query.get("sort"), orderParam = query.get("order");
  if (orderParam !== null && (column === null || !["asc", "desc"].includes(orderParam))) return { code: "page_order_invalid", status: 400 };
  const types = typesOf(table), order = orderParam === "desc" ? "desc" : "asc";
  let rows = table.rows;
  if (column !== null) {
    const c = table.columns.indexOf(column);
    if (c < 0) return { code: "sort_column_unknown", status: 400 };
    const by = compare(types[c]);
    rows = [...rows].sort((a, b) => {
      const [x, y] = [a.values[c], b.values[c]];
      if (missing(x) || missing(y)) return missing(x) === missing(y) ? a.index - b.index : missing(x) ? 1 : -1;
      return (order === "asc" ? by(x, y) : by(y, x)) || a.index - b.index;
    });
  }
  return { sha256: table.sha256, columns: table.columns, types, rows: rows.slice(offset, offset + limit), total: rows.length, offset, limit,
    sort: column === null ? null : { column, order } };
}


export function summaryOf(table: StoredTable): Record<string, unknown> {
  const types = typesOf(table);
  const columns = table.columns.map((name, c) => {
    const present = table.rows.map((r) => r.values[c]).filter((v) => !missing(v));
    const sorted = [...present].sort(compare(types[c]));
    return { name, type: types[c], type_source: "inferred", count: present.length, missing: table.rows.length - present.length,
      min: sorted[0] ?? null, max: sorted.at(-1) ?? null, distinct: new Set(present).size };
  });
  return { columns, receipt: { routine: "enrichedlab.table-summary", version: 1, input_sha256: table.sha256, output_sha256: sha(canonicalJson(columns)) } };
}

/** Answers `pages` or `summary` for a package member named by `?path=`, or for an artifact that is itself a table. */
export function answerTable(route: "pages" | "summary", url: URL, files: { path: string; bytes: Buffer }[] | null, own: { path: string; bytes: Buffer } | null, json: Json) {
  const path = url.searchParams.get("path");
  const file = files ? (path === null ? null : files.find((f) => f.path === path)) : own;
  if (files && path === null) return json({ error: { code: "table_member_required" } }, 400);
  if (!files && path !== null) return json({ error: { code: "artifact_not_a_package" } }, 415);
  if (!file) return json({ error: { code: "package_member_not_found" } }, 404);
  if (!/\.(csv|tsv)$/i.test(file.path)) return json({ error: { code: "not_a_table" } }, 415);
  const table = readTable(file.bytes, /\.tsv$/i.test(file.path) ? "\t" : ",");
  if ("code" in table) return json({ error: { code: table.code } }, table.status);
  const answer = route === "pages" ? pageOf(table, url.searchParams) : summaryOf(table);
  if ("code" in answer && "status" in answer) return json({ error: { code: answer.code } }, answer.status as number);
  json({ path: files ? path : null, sha256: table.sha256, ...answer });
}
