import { createHash, randomUUID } from "node:crypto";
import type { IncomingMessage, ServerResponse } from "node:http";
import type { Artifact, Task, TaskEvent } from "../src/contracts.ts";
import { csvRows, listing, syntheticPackage } from "./package.ts";
import type { ProjectTask } from "./projects.ts";
import { answerTable } from "./tables.ts";

// Lab datasets for the loopback preview: a person sends a table the lab keeps, a stand-in
// for the upload guard reads it, a stand-in for the broker's profiler says where the table is and what to decide, and a
// stand-in for the broker's preparation code applies an approved specification and counts every change. The stand-ins
// follow the rules of the broker's `dataset_profile.rs` and `dataset_prepare.rs` for text tables (CSV and
// tab-separated) only, and say so; on the cluster the upload guard rebuilds workbooks too. Every value the preview holds
// is simulated.

type Json = (value: unknown, status?: number) => void;
type Files = { path: string; bytes: Buffer }[];
const sha = (bytes: Buffer | string) => createHash("sha256").update(bytes).digest("hex");

// The broker's missing-value words, a number as a cell may write it and as the table writes it, a value reported against
// a limit, a not-detected word and the labels of summary rows.
const MISSING_TOKENS = ["NA", "N/A", "n/a", "na", "#N/A", "NaN", "nan", "null", "NULL", "Null", "-", "--", ".", "?", "missing", "Missing", "MISSING", "#NULL!"];
const LOOKS_NUMBER = /^[-+]?(?:\d+\.?\d*|\.\d+)(?:[eE][-+]?\d+)?$/;
const NUMBER = /^-?(?:\d+\.?\d*|\.\d+)(?:[eE][-+]?\d+)?$/;
const INTEGER = /^-?\d+$/;
const LIMIT = /^([<>≤≥])\s*=?\s*([-+]?(?:\d+(?:[.,]\d*)?|[.,]\d+)(?:[eE][-+]?\d+)?)\s*([A-Za-zµμ%/][A-Za-zµμ%/0-9.]*)?$/;
const NOT_DETECTED = /^(?:nd|n\.d\.|bdl|blq|bql|<lod|<loq|not detected)$/i;
const SUMMARY = /^(?:mean|average|avg|sd|s\.d\.|std|stdev|sem|se|s\.e\.m\.|median|total|sum|n|count|min|max|cv|cv%)$/i;
const POINT = /^((?:[a-z]+[\s_-]+)*?[a-z]{0,12})[\s_=-]*(\d+(?:\.\d+)?)\s*([a-z]{0,7})$/i;
const NAME = /^[a-z][a-z0-9_]{0,62}$/;
const letter = (i: number): string => (i >= 26 ? letter(Math.floor(i / 26) - 1) : "") + String.fromCharCode(65 + (i % 26));
const indexOf = (l: string) => /^[A-Z]{1,3}$/.test(l) ? [...l].reduce((n, c) => n * 26 + c.charCodeAt(0) - 64, 0) - 1 : -1;
const range = (from: number, to: number) => Array.from({ length: Math.max(0, to - from + 1) }, (_, i) => from + i);
const readsAsMissing = (text: string) => !text.trim() || text === "NA";

/** A name R and the analyses take as written, as the broker suggests one (`suggested_name`). */
export function suggestedName(name: string, at: string): string {
  let out = "";
  for (const ch of name) {
    const c = ch === "µ" || ch === "μ" ? "u" : ch;
    if (c === "%") { out += "_pct_"; continue; }
    if (/^[A-Za-z0-9]$/.test(c)) out += c.toLowerCase();
    else if (!out.endsWith("_")) out += "_";
  }
  out = out.replace(/^_+|_+$/g, "").replaceAll("__", "_");
  if (!out) out = at.toLowerCase();
  if (!/^[a-z]/.test(out)) out = `x_${out}`;
  return out.slice(0, 63).replace(/_+$/, "");
}

export type Grid = string[][];

/** A text table as the stand-in guard reads it: rows of cells, each row cut after its last filled cell. */
export function gridOf(bytes: Buffer, delimiter: "," | "\t"): Grid {
  let text = bytes.toString("utf8");
  if (text.charCodeAt(0) === 0xfeff) text = text.slice(1);
  const rows = delimiter === "," ? csvRows(text) : text.split(/\r?\n/).map((line) => line.split("\t"));
  const cut = (row: string[]) => { let n = row.length; while (n && !row[n - 1].trim()) n--; return row.slice(0, n); };
  const out = rows.map(cut);
  while (out.length && !out[out.length - 1].length) out.pop();
  return out;
}

const cellAt = (grid: Grid, row: number, col: number) => grid[row - 1]?.[col] ?? "";
const filledIn = (grid: Grid, row: number) => (grid[row - 1] ?? []).filter((c) => c.trim() !== "");
const spellingKey = (text: string) => text.split(/\s+/).filter(Boolean).join(" ").replace(/\.+$/, "").toLowerCase();
const quoted = (text: string) => JSON.stringify(text);

/** The profile and its detail, by the broker's rules for a text table:
 *  the header is the first row filled across most of the sheet with text only, a shorter row of short texts under it is
 *  the units row, summary rows (Mean, SD) inside the table are listed, and rows of text after it are notes. */
export function profileGrid(grid: Grid, source: { type: string; sha256: string; bytes: number; delimiter: string | null }, name: string | null) {
  const rowCount = grid.length, width = Math.min(512, Math.max(0, ...grid.map((r) => r.length)));
  const textOnly = (r: number) => filledIn(grid, r).every((c) => !LOOKS_NUMBER.test(c.trim()));
  let header = 0;
  for (let r = 1; r <= rowCount && !header; r++) if (filledIn(grid, r).length >= Math.max(2, Math.ceil(width * 0.6)) && textOnly(r)) header = r;
  const titleRows = header ? range(1, header - 1).filter((r) => filledIn(grid, r).length > 0) : [];
  const below = header + 1;
  const unitsRow = header && below <= rowCount && filledIn(grid, below).length > 0 && filledIn(grid, below).length < filledIn(grid, header).length
    && textOnly(below) && filledIn(grid, below).every((c) => c.trim().length <= 12) ? below : null;
  const first = (unitsRow ?? header) + 1;
  const kindOf = (r: number) => {
    const cells = filledIn(grid, r);
    if (!cells.length) return "blank";
    if (SUMMARY.test(cells[0].trim())) return "summary";
    if (cells.length === 1 && !LOOKS_NUMBER.test(cells[0].trim()) && cells[0].trim().length > 24) return "note";
    return "data";
  };
  let last = first - 1;
  for (let r = first; r <= rowCount; r++) if (kindOf(r) === "data" || kindOf(r) === "summary") last = r;
  const summaryRows = range(first, last).filter((r) => kindOf(r) === "summary");
  const blank = range(first, last).filter((r) => kindOf(r) === "blank").length;
  const noteRows = range(last + 1, rowCount).filter((r) => kindOf(r) !== "blank");
  const dataRows = range(first, last).filter((r) => kindOf(r) === "data");

  const columns: Record<string, unknown>[] = [];
  const details: Record<string, unknown>[] = [];
  const suggested = new Map<string, number>();
  for (let c = 0; c < width; c++) {
    const at = letter(c);
    const head = header ? cellAt(grid, header, c).trim() : "";
    const inHead = head.match(/^(.*?)\s*[([]\s*([^)\]]{1,16})\s*[)\]]$/);
    const unitText = unitsRow ? cellAt(grid, unitsRow, c).trim() : "";
    const label = inHead ? inHead[1].trim() : head;
    const unit = unitText || (inHead ? inHead[2].trim() : "") || null;
    let suggestion = suggestedName(label, at);
    const seen = suggested.get(suggestion) ?? 0;
    suggested.set(suggestion, seen + 1);
    if (seen) suggestion = `${suggestion.slice(0, 60)}_${seen + 1}`;
    const missing: Record<string, number> = {};
    const censored: Record<string, number> = {};
    const present: string[] = [];
    const censoredTexts: string[] = [];
    for (const r of dataRows) {
      const raw = cellAt(grid, r, c), t = raw.trim();
      if (!t) { missing[""] = (missing[""] ?? 0) + 1; continue; }
      if (MISSING_TOKENS.includes(t)) { missing[t] = (missing[t] ?? 0) + 1; continue; }
      const limit = t.match(LIMIT);
      const direction = limit ? (limit[1] === "<" || limit[1] === "≤" ? "below_limit" : "above_limit") : NOT_DETECTED.test(t) ? "not_detected" : null;
      if (direction) { censored[direction] = (censored[direction] ?? 0) + 1; censoredTexts.push(t); continue; }
      present.push(raw);
    }
    const numbers = present.filter((t) => LOOKS_NUMBER.test(t.trim()));
    const words = present.length - numbers.length;
    const type = !present.length ? "empty"
      : numbers.length * 5 >= present.length * 4 ? (numbers.every((t) => /^[-+]?\d+$/.test(t.trim())) ? "integer" : "number")
      : words * 5 >= present.length * 4 ? "text" : "mixed";
    const numeric = type === "number" || type === "integer";
    const counts = new Map<string, number>();
    for (const t of present) counts.set(t, (counts.get(t) ?? 0) + 1);
    const groups = new Map<string, string[]>();
    for (const t of counts.keys()) groups.set(spellingKey(t), [...(groups.get(spellingKey(t)) ?? []), t]);
    const spellings = [...groups.entries()].filter(([, g]) => g.length > 1).sort(([a], [b]) => (a < b ? -1 : a > b ? 1 : 0))
      .map(([, g]) => g.sort().map(quoted).join(", "));
    const idLike = /\b(id|mouse|animal|subject|sample|participant)\b/i.test(label);
    const repeatedIds = idLike && type !== "empty" ? [...counts.entries()].filter(([, n]) => n > 1).map(([t]) => t.trim()) : [];
    const tokens = Object.keys(missing).filter((k) => k !== "");
    const issues: string[] = [];
    const examples: Record<string, string[]> = {};
    if (tokens.length > 1 || (tokens.length === 1 && "" in missing)) issues.push("missing_tokens_mixed");
    if (censoredTexts.length) { issues.push("censored_values"); examples.censored_values = censoredTexts.slice(0, 4); }
    if (numeric && words) { issues.push("text_in_numbers"); examples.text_in_numbers = present.filter((t) => !LOOKS_NUMBER.test(t.trim())).slice(0, 4); }
    if (type === "text" && spellings.length) { issues.push("category_spellings"); examples.category_spellings = spellings.slice(0, 8); }
    if (repeatedIds.length) { issues.push("duplicate_ids"); examples.duplicate_ids = repeatedIds.slice(0, 4); }
    if (type === "mixed") issues.push("mixed_types");
    const ordered = (map: Record<string, number>) => Object.fromEntries(Object.entries(map).sort(([a], [b]) => (a < b ? -1 : a > b ? 1 : 0)));
    columns.push({ index: c, letter: at, name: label || at, name_from: label ? "header" : "letter", suggested_name: suggestion, unit,
      unit_from: unitText ? "units_row" : inHead ? "header" : null, type, date_order: null, present: present.length, missing: ordered(missing), censored: ordered(censored),
      other: numeric ? words : 0, distinct: type === "text" ? counts.size : null, issues });
    const sorted = numbers.map((t) => t.trim()).sort((a, b) => Number(a) - Number(b));
    details.push({ letter: at, values: type === "text" ? [...counts.entries()].sort((a, b) => b[1] - a[1] || (a[0] < b[0] ? -1 : 1)).slice(0, 10) : [],
      min: numeric && sorted.length ? sorted[0] : null, max: numeric && sorted.length ? sorted[sorted.length - 1] : null, examples });
  }
  // The sheet's issues: each column issue with its columns in the order the issues first appear, one measure across
  // columns, then where the table sits.
  const issues: { code: string; columns: string[]; count: number }[] = [];
  const gathered = new Map<string, string[]>();
  for (const col of columns) for (const code of col.issues as string[]) gathered.set(code, [...(gathered.get(code) ?? []), col.letter as string]);
  for (const [code, letters] of gathered) issues.push({ code, columns: letters, count: letters.length });
  const points = columns.map((col) => {
    if (col.type !== "number" && col.type !== "integer") return null;
    const m = String(col.name).trim().match(POINT);
    return m ? { word: m[1].trim().toLowerCase(), n: Number(m[2]), unit: m[3].toLowerCase(), at: col.index as number } : null;
  });
  let run: number[] = [];
  const close = () => { if (run.length >= 3) issues.push({ code: "wide_time_points", columns: run.map((i) => columns[i].letter as string), count: run.length }); run = []; };
  points.forEach((point, i) => {
    const prev = run.length ? points[run[run.length - 1]] : null;
    const continues = !!(prev && point && prev.word === point.word && prev.unit === point.unit && point.n > prev.n && point.at === prev.at + 1);
    if (!continues) close();
    if (point) run.push(i);
  });
  close();
  const shape = (code: string, when: boolean, count: number) => { if (when) issues.push({ code, columns: [], count }); };
  shape("no_header", !header, 1);
  shape("header_not_first", header > 1, 1);
  shape("title_rows", titleRows.length > 0, titleRows.length);
  shape("units_row", unitsRow !== null, 1);
  shape("blank_rows", blank > 0, blank);
  shape("summary_rows", summaryRows.length > 0, summaryRows.length);
  shape("notes_below_data", noteRows.length > 0, noteRows.length);
  const unitsInNames = unitsRow === null ? columns.filter((col) => col.unit_from === "header").map((col) => col.letter as string) : [];
  if (unitsInNames.length) issues.push({ code: "units_in_names", columns: unitsInNames, count: unitsInNames.length });

  const sheet = { index: 0, name, hidden: false, row_count: rowCount, column_count: width, columns_omitted: 0, columns, issues,
    table: rowCount ? { header_rows: header ? [header] : [], header_basis: header ? (columns.some((col) => col.type === "number" || col.type === "integer") ? "labels_over_numbers" : "first_text_row") : null,
      units_row: unitsRow, title_rows: titleRows, first_data_row: first, last_data_row: last, data_rows: dataRows.length, blank_rows: blank,
      summary_rows: summaryRows, note_rows: noteRows } : null };
  const profile = { schema: "enrichedlab.dataset-profile.v1", source: { type: source.type, sha256: source.sha256, bytes: source.bytes,
    encoding: "utf-8", delimiter: source.delimiter }, sheets: [sheet] };
  const detail = { schema: "enrichedlab.dataset-profile-detail.v1", sheets: [{ index: 0, preview: grid.slice(0, 30).map((r) => r.slice(0, 30)),
    title_text: titleRows.map((r) => filledIn(grid, r).join(" ")), note_text: noteRows.map((r) => filledIn(grid, r).join(" ")), columns: details }] };
  return { profile, detail };
}

type Spec = {
  schema: string; table_name: string; sheet: number; header_rows?: number[]; units_row?: number | null; rows: { first: number; last: number };
  drop_rows?: [number, number][]; missing: string[]; decimal?: "point" | "comma"; thousands?: "none" | "comma" | "point" | "space";
  date_order?: "day_first" | "month_first" | null; trim?: boolean; columns: ColumnSpec[];
  long?: { columns: string[]; name: string; values: (string | number)[]; value: ColumnSpec } | null; key?: string[];
};
type ColumnSpec = { from?: string; name: string; type: string; unit?: string | null; decimal?: "point" | "comma" | null; thousands?: string | null;
  recode?: Record<string, string>; censored?: { rule: string; limit?: number | null; upper_limit?: number | null } | null; strip_percent?: boolean;
  true_values?: string[]; false_values?: string[]; other?: "refuse" | "missing" };
type Output = { name: string; type: string; unit: string | null; from: string };

const SPEC_FIELDS = ["schema", "table_name", "sheet", "header_rows", "units_row", "rows", "drop_rows", "missing", "decimal", "thousands", "date_order", "trim", "columns", "long", "key"];
const COLUMN_FIELDS = ["from", "name", "type", "unit", "decimal", "thousands", "recode", "censored", "strip_percent", "true_values", "false_values", "other"];
const TYPES = ["text", "category", "number", "integer", "logical", "date", "datetime", "time"];
const RULES = ["missing", "limit", "half_limit", "limit_over_sqrt2"];
const plain = (text: unknown, max: number) => typeof text === "string" && text.length <= max && !/[\x00-\x1f\x7f]/.test(text);

/** The table's columns in order, as the broker writes them: each column, a censored one followed by its direction and
 *  limit, then a long layout's point and value. */
function outputs(spec: Spec): Output[] {
  const out: Output[] = [];
  const push = (col: ColumnSpec, from: string) => {
    out.push({ name: col.name, type: col.type, unit: col.unit ?? null, from });
    if (col.censored) out.push({ name: `${col.name}_censored`, type: "category", unit: null, from: col.name },
      { name: `${col.name}_limit`, type: "number", unit: col.unit ?? null, from: col.name });
  };
  for (const col of spec.columns) push(col, col.from ?? "");
  if (spec.long) {
    out.push({ name: spec.long.name, type: spec.long.values.every((v) => typeof v === "number") ? "number" : "category", unit: null, from: "long" });
    push(spec.long.value, "long");
  }
  return out;
}

function columnProblem(col: Record<string, unknown>, path: string): { path: string; reason: string } | null {
  if (Object.keys(col).some((k) => !COLUMN_FIELDS.includes(k))) return { path, reason: "unknown_field" };
  if (typeof col.name !== "string" || !NAME.test(col.name)) return { path: `${path}.name`, reason: "not_a_name" };
  if (!TYPES.includes(col.type as string)) return { path: `${path}.type`, reason: "unknown_choice" };
  if (col.unit != null && (col.unit === "" || !plain(col.unit, 32))) return { path: `${path}.unit`, reason: "not_a_unit" };
  const text = col.type === "text" || col.type === "category", numeric = col.type === "number" || col.type === "integer";
  const recode = (col.recode ?? {}) as Record<string, unknown>;
  if (Object.keys(recode).length && !text) return { path: `${path}.recode`, reason: "not_for_this_type" };
  if (Object.entries(recode).some(([from, to]) => !plain(from, 200) || !plain(to, 200))) return { path: `${path}.recode`, reason: "not_a_recode" };
  if (Object.values(recode).some((to) => readsAsMissing(to as string))) return { path: `${path}.recode`, reason: "value_reads_as_missing" };
  const censored = col.censored as Record<string, unknown> | null | undefined;
  if (censored) {
    if (!numeric) return { path: `${path}.censored`, reason: "not_for_this_type" };
    if (!RULES.includes(censored.rule as string)) return { path: `${path}.censored.rule`, reason: "unknown_choice" };
    if (col.type === "integer" && (censored.rule === "half_limit" || censored.rule === "limit_over_sqrt2")) return { path: `${path}.censored.rule`, reason: "not_for_this_type" };
    const positive = (v: unknown) => v == null || (typeof v === "number" && Number.isFinite(v) && v > 0);
    if (!positive(censored.limit) || !positive(censored.upper_limit)) return { path: `${path}.censored`, reason: "not_a_limit" };
    if (col.type === "integer" && censored.limit != null && !Number.isInteger(censored.limit)) return { path: `${path}.censored.limit`, reason: "not_an_integer" };
    if (col.type === "integer" && censored.upper_limit != null && !Number.isInteger(censored.upper_limit)) return { path: `${path}.censored.upper_limit`, reason: "not_an_integer" };
  }
  if ((col.decimal != null || col.thousands != null || col.strip_percent) && !numeric) return { path, reason: "not_for_this_type" };
  if (((col.true_values as unknown[] | undefined)?.length || (col.false_values as unknown[] | undefined)?.length) && col.type !== "logical") return { path, reason: "not_for_this_type" };
  return null;
}

/** The broker's check of a specification as written: a place and a fixed reason, never the person's own text. */
export function specProblem(spec: unknown): { path: string; reason: string } | null {
  const s = spec as Record<string, unknown> | null;
  if (!s || typeof s !== "object" || Array.isArray(s)) return { path: "", reason: "wrong_type" };
  if (Object.keys(s).some((k) => !SPEC_FIELDS.includes(k))) return { path: "", reason: "unknown_field" };
  for (const field of ["schema", "table_name", "rows", "missing", "columns"]) if (!(field in s)) return { path: field, reason: "missing_field" };
  if (s.schema !== "enrichedlab.dataset-preparation.v1") return { path: "schema", reason: "unknown_schema" };
  if (typeof s.table_name !== "string" || !NAME.test(s.table_name)) return { path: "table_name", reason: "not_a_name" };
  if (!Number.isInteger(s.sheet ?? 0) || (s.sheet as number ?? 0) < 0) return { path: "sheet", reason: "wrong_type" };
  if ((s.sheet as number ?? 0) >= 64) return { path: "sheet", reason: "no_such_sheet" };
  const rows = s.rows as { first?: unknown; last?: unknown };
  if (!rows || !Number.isInteger(rows.first) || !Number.isInteger(rows.last)) return { path: "rows", reason: "wrong_type" };
  const first = rows.first as number, last = rows.last as number;
  if (first < 1 || last < first || last > 1_048_576) return { path: "rows", reason: "rows_out_of_order" };
  const headers = (s.header_rows ?? []) as number[];
  if (headers.length > 2 || headers.some((r) => r < 1 || r >= first) || headers.some((r, i) => i && headers[i - 1] >= r)) return { path: "header_rows", reason: "rows_out_of_order" };
  if (s.units_row != null && ((s.units_row as number) < 1 || (s.units_row as number) >= first)) return { path: "units_row", reason: "rows_out_of_order" };
  const drops = (s.drop_rows ?? []) as [number, number][];
  if (drops.length > 1000) return { path: "drop_rows", reason: "too_many" };
  const badDrop = drops.findIndex(([a, b]) => a > b || a < first || b > last);
  if (badDrop >= 0) return { path: `drop_rows[${badDrop}]`, reason: "rows_out_of_order" };
  const missing = s.missing as unknown[];
  if (!Array.isArray(missing)) return { path: "missing", reason: "wrong_type" };
  if (missing.length > 32) return { path: "missing", reason: "too_many" };
  const badToken = missing.findIndex((t) => !plain(t, 64) || (t as string).trim() !== t);
  if (badToken >= 0) return { path: `missing[${badToken}]`, reason: "not_a_token" };
  const columns = s.columns as Record<string, unknown>[];
  if (!Array.isArray(columns) || !columns.length || columns.length > 512) return { path: "columns", reason: "too_many" };
  const sources = new Set<string>();
  for (const [i, col] of columns.entries()) {
    if (!col || typeof col !== "object") return { path: `columns[${i}]`, reason: "wrong_type" };
    if (indexOf(String(col.from ?? "")) < 0) return { path: `columns[${i}].from`, reason: "not_a_column" };
    if (sources.has(col.from as string)) return { path: `columns[${i}].from`, reason: "column_repeated" };
    sources.add(col.from as string);
    const problem = columnProblem(col, `columns[${i}]`);
    if (problem) return problem;
  }
  const long = s.long as Record<string, unknown> | null | undefined;
  if (long) {
    const letters = long.columns as unknown[];
    if (!Array.isArray(letters) || letters.length < 2 || letters.length > 512) return { path: "long.columns", reason: "too_many" };
    for (const [i, l] of letters.entries()) {
      if (indexOf(String(l)) < 0) return { path: `long.columns[${i}]`, reason: "not_a_column" };
      if (sources.has(l as string)) return { path: `long.columns[${i}]`, reason: "column_repeated" };
      sources.add(l as string);
    }
    const values = long.values as unknown[];
    if (!Array.isArray(values) || values.length !== letters.length) return { path: "long.values", reason: "values_do_not_match_columns" };
    const numbers = values.every((v) => typeof v === "number"), texts = values.every((v) => typeof v === "string" && v !== "" && plain(v, 64));
    if (!numbers && !texts) return { path: "long.values", reason: "values_mixed" };
    if (texts && values.some((v) => readsAsMissing(v as string))) return { path: "long.values", reason: "value_reads_as_missing" };
    if (new Set(values.map((v) => JSON.stringify(v))).size !== values.length) return { path: "long.values", reason: "values_repeated" };
    if (typeof long.name !== "string" || !NAME.test(long.name)) return { path: "long.name", reason: "not_a_name" };
    const value = long.value as Record<string, unknown> | undefined;
    if (!value || typeof value !== "object") return { path: "long.value", reason: "missing_field" };
    if (value.from) return { path: "long.value.from", reason: "not_allowed" };
    const problem = columnProblem(value, "long.value");
    if (problem) return problem;
  }
  const names = outputs(s as unknown as Spec).map((o) => o.name);
  if (names.length > 1000) return { path: "columns", reason: "too_many" };
  const repeated = names.find((n, i) => names.indexOf(n) !== i);
  if (repeated) return { path: `name:${repeated}`, reason: "name_repeated" };
  const key = (s.key ?? []) as string[];
  if (key.length > 8) return { path: "key", reason: "too_many" };
  const missingKey = key.findIndex((k) => !names.includes(k));
  if (missingKey >= 0) return { path: `key[${missingKey}]`, reason: "not_a_column" };
  return null;
}

type Out = { kind: "missing" } | { kind: "value"; text: string } | { kind: "censored"; direction: string; limit: string | null; value: string | null };
type Unread = { reason: string; text: string };
type Counts = { name: string; from: string; type: string; values: number; missing: Record<string, number>; trimmed: number; recoded: Record<string, number>;
  recoded_unlisted: number; decimal_commas: number; thousands_separators: number; percent_signs: number; plus_signs: number; dates_from_text: number;
  censored: Record<string, number>; censored_rule: string | null; unread_as_missing: number };

/** The stand-in for the broker's preparation code (`enrichedlab.dataset-prepare` version 2): each cell read by its
 *  column's rule in the broker's order (empty, a missing word, the type with its recode or limit), every change
 *  counted, every filled cell of the sheet accounted for, and a cell no rule reads stopping it unless its column says
 *  missing. Numbers keep the digits the cell wrote; text cells are quoted as the broker quotes them. */
export function prepare(grid: Grid, spec: Spec) {
  if (spec.sheet !== 0) return { refusal: "dataset_sheet_missing", receipt: null };
  const width = Math.max(0, ...grid.map((r) => r.length));
  const letters = [...spec.columns.map((c) => c.from ?? ""), ...(spec.long?.columns ?? [])];
  if (letters.some((l) => indexOf(l) < 0 || indexOf(l) >= Math.max(1, width)) || spec.rows.first > Math.max(1, grid.length) || spec.rows.last > grid.length)
    return { refusal: "dataset_spec_outside_sheet", receipt: null };
  const selected = new Set(letters.map(indexOf));
  const missingTokens = new Set(spec.missing);
  const trim = spec.trim ?? true;
  const counts = (col: ColumnSpec, from: string): Counts => ({ name: col.name, from, type: col.type, values: 0, missing: {}, trimmed: 0, recoded: {},
    recoded_unlisted: 0, decimal_commas: 0, thousands_separators: 0, percent_signs: 0, plus_signs: 0, dates_from_text: 0, censored: {},
    censored_rule: col.censored?.rule ?? null, unread_as_missing: 0 });
  const bump = (map: Record<string, number>, key: string) => { map[key] = (map[key] ?? 0) + 1; };

  // One cell by one column's rule: a value, missing, a censored value, or a reason no rule reads it.
  function convert(col: ColumnSpec, k: Counts, raw: string): Out | Unread {
    const t = raw.trim();
    if (!t) { bump(k.missing, ""); return { kind: "missing" }; }
    if (missingTokens.has(t)) { bump(k.missing, t); return { kind: "missing" }; }
    if (col.type === "text" || col.type === "category") {
      const trims = trim && t !== raw;
      const written = col.recode?.[t] ?? (trims ? t : raw);
      if (written === "NA") return { reason: "text_reads_as_missing", text: raw };
      let text = trims ? t : raw;
      if (trims) k.trimmed++;
      const to = col.recode?.[t];
      if (to !== undefined) { if (to !== text) bump(k.recoded, `${t} -> ${to}`); text = to; }
      return { kind: "value", text };
    }
    let out: Out | Unread;
    if (col.type === "number" || col.type === "integer") out = number(col, k, t);
    else if (col.type === "logical") {
      const upper = t.toUpperCase();
      out = upper === "TRUE" || (col.true_values ?? []).includes(t) ? { kind: "value", text: "TRUE" }
        : upper === "FALSE" || (col.false_values ?? []).includes(t) ? { kind: "value", text: "FALSE" } : { reason: "not_a_logical", text: raw };
    } else out = { reason: col.type === "date" ? "not_a_date" : col.type === "datetime" ? "not_a_datetime" : "not_a_time", text: raw };
    if (t !== raw && "kind" in out) k.trimmed++;
    return out;
  }
  function number(col: ColumnSpec, k: Counts, t: string): Out | Unread {
    const decimal = col.decimal ?? spec.decimal ?? "point";
    const limit = t.match(LIMIT);
    if (limit || NOT_DETECTED.test(t)) {
      const direction = !limit ? "not_detected" : limit[1] === "<" || limit[1] === "≤" ? "below" : "above";
      if (!col.censored) return { reason: "censored_without_rule", text: t };
      const stated = limit ? (decimal === "comma" ? limit[2].replace(",", ".") : limit[2]).replace(/^\+/, "") : null;
      const fallback = direction === "above" ? col.censored.upper_limit : col.censored.limit;
      const limitText = stated ?? (fallback == null ? null : String(fallback));
      const rule = col.censored.rule;
      let value: string | null = null;
      if (rule !== "missing") {
        if (limitText === null) return { reason: "limit_unknown", text: t };
        value = direction === "above" || rule === "limit" ? limitText : String(Number(limitText) / (rule === "half_limit" ? 2 : Math.SQRT2));
      }
      if (col.type === "integer" && value !== null && !INTEGER.test(value)) return { reason: "not_an_integer", text: t };
      bump(k.censored, direction === "below" ? "below_limit" : direction === "above" ? "above_limit" : "not_detected");
      return { kind: "censored", direction, limit: limitText, value };
    }
    let n = t;
    if (col.strip_percent && n.endsWith("%")) { n = n.slice(0, -1).trimEnd(); k.percent_signs++; }
    if (decimal === "comma") {
      if (n.includes(".")) return { reason: "point_in_comma_decimal", text: t };
      if (n.includes(",")) { n = n.replace(",", "."); k.decimal_commas++; }
    }
    if (n.startsWith("+")) { n = n.slice(1); k.plus_signs++; }
    if (!NUMBER.test(n) && n !== "Inf" && n !== "-Inf") return { reason: n.includes(",") ? "comma_in_number" : "not_a_number", text: t };
    if (col.type === "integer" && !INTEGER.test(n)) return { reason: "not_an_integer", text: t };
    return { kind: "value", text: n };
  }

  const cells = { filled: 0, outside_rows: 0, in_dropped_rows: 0, in_unread_columns: 0, read: 0 };
  const rows = { read: 0, blank: 0, dropped: 0, kept: 0, written: 0 };
  const fixed = spec.columns.map((col) => ({ col, at: indexOf(col.from ?? ""), k: counts(col, col.from ?? "") }));
  const long = spec.long ?? null;
  const varying = (long?.columns ?? []).map((l) => ({ col: long!.value, at: indexOf(l), letter: l, k: counts(long!.value, l) }));
  const out = outputs(spec);
  const keyAt = (spec.key ?? []).map((name) => out.findIndex((o) => o.name === name)).filter((i) => i >= 0);
  const failures: { row: number; column: string; reason: string; text: string }[] = [];
  const asMissing: typeof failures = [];
  let failureCount = 0;
  const keys = new Map<string, number>();
  const repeated: { row: number; first_row: number }[] = [];
  let repeatedCount = 0;
  type Field = { kind: "missing" } | { kind: "plain"; text: string } | { kind: "text"; text: string };
  const lines: Field[][] = [];
  const fieldsOf = (col: ColumnSpec, o: Out): Field[] => {
    const text = col.type === "text" || col.type === "category";
    const value: Field = o.kind === "value" ? (text ? { kind: "text", text: o.text } : { kind: "plain", text: o.text })
      : o.kind === "censored" && o.value !== null ? { kind: "plain", text: o.value } : { kind: "missing" };
    if (!col.censored) return [value];
    if (o.kind === "censored") return [value, { kind: "text", text: o.direction }, o.limit === null ? { kind: "missing" } : { kind: "plain", text: o.limit }];
    return [value, { kind: "missing" }, { kind: "missing" }];
  };
  for (let r = 1; r <= grid.length; r++) {
    const row = grid[r - 1];
    const filled = row.filter((c) => c.trim() !== "").length;
    cells.filled += filled;
    if (r < spec.rows.first || r > spec.rows.last) { cells.outside_rows += filled; continue; }
    rows.read++;
    if ((spec.drop_rows ?? []).some(([a, b]) => r >= a && r <= b)) { rows.dropped++; cells.in_dropped_rows += filled; continue; }
    if (!filled) { rows.blank++; continue; }
    rows.kept++;
    row.forEach((c, i) => { if (c.trim()) { if (selected.has(i)) cells.read++; else cells.in_unread_columns++; } });
    let failed = false;
    const read = (col: ColumnSpec, k: Counts, at: number, column: string): Out => {
      const got = convert(col, k, row[at] ?? "");
      if ("kind" in got) { if (got.kind === "value") k.values++; return got; }
      const noted = { row: r, column, reason: got.reason, text: got.text.slice(0, 120) };
      if (col.other === "missing") { k.unread_as_missing++; if (asMissing.length < 20) asMissing.push(noted); return { kind: "missing" }; }
      failed = true; failureCount++;
      if (failures.length < 20) failures.push(noted);
      return { kind: "missing" };
    };
    const base = fixed.map(({ col, k, at }) => ({ col, o: read(col, k, at, col.from ?? "") }));
    const each = varying.map(({ col, k, at, letter: l }) => read(col, k, at, l));
    if (failed) continue;
    const points = long ? long.values.map((v, i) => ({ point: v, o: each[i] })) : [null];
    for (const p of points) {
      const fields = base.flatMap(({ col, o }) => fieldsOf(col, o));
      if (p && long) fields.push(typeof p.point === "number" ? { kind: "plain", text: String(p.point) } : { kind: "text", text: p.point }, ...fieldsOf(long.value, p.o));
      if (keyAt.length) {
        const key = JSON.stringify(keyAt.map((i) => fields[i]));
        const seen = keys.get(key);
        if (seen !== undefined) { repeatedCount++; if (repeated.length < 20) repeated.push({ row: r, first_row: seen }); } else keys.set(key, r);
      }
      lines.push(fields);
    }
  }
  if (rows.read !== spec.rows.last - spec.rows.first + 1) return { refusal: "dataset_grid_unreadable", receipt: null };
  const columnCounts = fixed.map(({ k }) => k);
  if (long) {
    const merged = counts(long.value, long.columns.join(","));
    for (const { k } of varying) {
      merged.values += k.values; merged.trimmed += k.trimmed; merged.recoded_unlisted += k.recoded_unlisted; merged.decimal_commas += k.decimal_commas;
      merged.thousands_separators += k.thousands_separators; merged.percent_signs += k.percent_signs; merged.plus_signs += k.plus_signs;
      merged.dates_from_text += k.dates_from_text; merged.unread_as_missing += k.unread_as_missing;
      for (const [t, n] of Object.entries(k.missing)) merged.missing[t] = (merged.missing[t] ?? 0) + n;
      for (const [t, n] of Object.entries(k.recoded)) merged.recoded[t] = (merged.recoded[t] ?? 0) + n;
      for (const [t, n] of Object.entries(k.censored)) merged.censored[t] = (merged.censored[t] ?? 0) + n;
    }
    columnCounts.push(merged);
  }
  rows.written = lines.length;
  // The broker's maps are ordered by key, as a BTreeMap serializes them.
  const ordered = (map: Record<string, number>) => Object.fromEntries(Object.entries(map).sort(([a], [b]) => (a < b ? -1 : a > b ? 1 : 0)));
  for (const k of columnCounts) { k.missing = ordered(k.missing); k.recoded = ordered(k.recoded); k.censored = ordered(k.censored); }
  const quotedField = (text: string) => !text || text === "NA" || /[,"\r\n]/.test(text);
  const put = (text: string, quote: boolean) => (quote ? `"${text.replaceAll('"', '""')}"` : text);
  const header = out.map((o) => put(o.name, quotedField(o.name))).join(",");
  const tableLines = lines.map((fields) => fields.map((f) => (f.kind === "missing" ? "NA" : f.kind === "plain" ? f.text : put(f.text, quotedField(f.text)))).join(","));
  const sheetLines = lines.map((fields) => fields.map((f) => (f.kind === "missing" ? "NA" : f.kind === "plain" ? f.text
    : /^[=+\-@\t\r]/.test(f.text) ? put(`'${f.text}`, true) : put(f.text, quotedField(f.text)))).join(","));
  const table = Buffer.from([header, ...tableLines].join("\n") + "\n");
  const spreadsheet = Buffer.concat([Buffer.from([0xef, 0xbb, 0xbf]), Buffer.from([header, ...sheetLines].join("\n") + "\n")]);
  const receipt = { schema: "enrichedlab.dataset-preparation-receipt.v1", executor: { name: "enrichedlab.dataset-prepare", version: 2 },
    status: "refused", grid_sha256: "", spec_sha256: "", rows, cells, columns: columnCounts, outputs: out,
    failures, failure_count: failureCount, read_as_missing: asMissing, repeated_keys: repeated, repeated_key_count: repeatedCount,
    table: null as null | { path: string; sha256: string; bytes: number; rows: number },
    spreadsheet_copy: null as null | { path: string; sha256: string; bytes: number; rows: number } };
  if (failureCount) return { refusal: "dataset_values_unread", receipt };
  if (repeatedCount) return { refusal: "dataset_key_repeated", receipt };
  if (!lines.length) return { refusal: "dataset_no_rows", receipt };
  receipt.status = "prepared";
  receipt.table = { path: `results/lab_data/${spec.table_name}.csv`, sha256: sha(table), bytes: table.length, rows: lines.length };
  receipt.spreadsheet_copy = { path: `exports/${spec.table_name}.spreadsheet.csv`, sha256: sha(spreadsheet), bytes: spreadsheet.length, rows: lines.length };
  return { refusal: null, receipt, table, spreadsheet };
}

const TYPES_BY_EXTENSION: Record<string, string> = {
  csv: "text/csv", tsv: "text/tab-separated-values", tab: "text/tab-separated-values", txt: "text/tab-separated-values", dat: "text/tab-separated-values",
  xlsx: "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet", xlsm: "application/vnd.ms-excel.sheet.macroenabled.12", xls: "application/vnd.ms-excel",
};
const POLICY = "lab-dataset-preview-1";
const MAX_BYTES = 32 * 1024 * 1024;

type Receipt = Record<string, unknown> & { attempt: number; status: string; error_code: string | null };
interface Run { task: Task; entry: ProjectTask; receipts: Receipt[]; pkg: ReturnType<typeof syntheticPackage> | null; artifacts: Artifact[] }
interface Prep extends Run { title: string; spec: Spec; spec_sha256: string; previous: string | null; request_id: string; body: string; request_sha256: string; table_name: string }
interface Dataset {
  dataset_id: string; project_id: string; request_id: string; file_name: string; media_type: string; size_bytes: number; sha256: string;
  guard: { intake: string; outcome: "allowed" | "quarantined" | "blocked"; findings: string[]; policy: string };
  grid_sha256: string | null; grid_bytes: number | null; principal_id: string; created_at: string;
  grid: Grid | null; intake: Run | null; preparations: Prep[];
}

export function datasetFixtures(options: {
  me: string; profile: string; group: string; project: string; tasks: Task[]; histories: Map<string, TaskEvent[]>;
  addProjectTask: (entry: ProjectTask) => void; delay?: number;
}) {
  const { me, profile, group, project, tasks, histories } = options;
  const delay = options.delay ?? 500;
  const datasets: Dataset[] = [];
  const timers = new Set<ReturnType<typeof setTimeout>>();
  let queueFull = false;
  const refuse = (json: Json, code: string, status = 400, extra: Record<string, unknown> = {}) => json({ error: { code, message: "Refused", ...extra } }, status);
  const later = (ms: number, work: () => void) => { const t = setTimeout(() => { timers.delete(t); work(); }, ms); timers.add(t); };
  const runOf = (id: string) => {
    for (const d of datasets) {
      if (d.intake?.task.id === id) return { dataset: d, run: d.intake as Run, prep: null };
      const prep = d.preparations.find((p) => p.task.id === id);
      if (prep) return { dataset: d, run: prep as Run, prep };
    }
    return null;
  };
  function event(task: Task, state: string, code: string) {
    const history = histories.get(task.id)!;
    history.push({ sequence: history.length + 1, kind: "task", state, code, occurred_at: new Date().toISOString() });
  }
  function newRun(d: Dataset, operation: "intake" | "preparation", title: string | null, previous: string | null, parent: string | null): Run {
    const now = new Date().toISOString();
    const task: Task = { id: randomUUID(), group, profile, objective: null, status: "queued", attempt: 0, max_task_attempts: 2, max_provider_attempts: 1,
      logical_agent: null, result_class: null, error_code: null, submitted_at: now, started_at: null, updated_at: now, finished_at: null };
    const entry = { task_id: task.id, action_key: `dataset-${operation}-${task.id.slice(0, 8)}`, parent_task_id: parent, profile, status: "queued", attempt: 0,
      submitted_at: now, updated_at: now, dataset: { title, operation, dataset: d.dataset_id, previous } } as ProjectTask;
    tasks.unshift(task);
    histories.set(task.id, [{ sequence: 1, kind: "task", state: "queued", code: "task_accepted", occurred_at: now }]);
    options.addProjectTask(entry);
    return { task, entry, receipts: [], pkg: null, artifacts: [] };
  }
  function finish(run: Run, status: "completed" | "failed", files: Files | null, receipt: Receipt) {
    const now = new Date().toISOString();
    if (files) {
      const pkg = syntheticPackage(files, Math.floor(Date.now() / 1000));
      run.pkg = pkg;
      run.artifacts = [
        { id: randomUUID(), role: "result", label: "Lab data and its record", relative_path: `tasks/${run.task.id}/payload`, media_type: "application/gzip",
          size_bytes: pkg.archive.length, sha256: pkg.sha256, created_at: now },
        { id: randomUUID(), role: "result", label: "Package checksum", relative_path: `tasks/${run.task.id}/payload.sha256`, media_type: "text/plain",
          size_bytes: pkg.checksum.length, sha256: sha(pkg.checksum), created_at: now },
      ];
      receipt.package_sha256 = pkg.sha256;
    }
    run.receipts.push(receipt);
    Object.assign(run.task, { status, result_class: status === "completed" ? "success" : "dataset_refused", error_code: receipt.error_code, finished_at: now, updated_at: now });
    Object.assign(run.entry, { status, updated_at: now });
    event(run.task, status, status === "completed" ? "task_completed" : "task_failed");
  }
  function start(run: Run) {
    const now = new Date().toISOString();
    Object.assign(run.task, { status: "running", attempt: 1, started_at: now, updated_at: now, logical_agent: "dataset-worker" });
    Object.assign(run.entry, { status: "running", attempt: 1, updated_at: now });
    event(run.task, "running", "worker_lease_acquired");
  }
  const readme = (what: string) => Buffer.from(`# ${what}\n\nWritten by the workspace preview's stand-in for the broker's lab data code. The preview reads text tables only, and every value in it is simulated.\n`);
  function intake(d: Dataset) {
    const run = newRun(d, "intake", null, null, null);
    d.intake = run;
    later(delay, () => start(run));
    later(delay * 2, () => {
      const started = run.task.started_at!, finished = new Date().toISOString();
      const base = { attempt: 1, grid_sha256: d.grid_sha256, table_path: null, table_sha256: null, table_rows: null, table_columns: null,
        executor: "enrichedlab.dataset-profile", executor_version: 1, started_at: started, finished_at: finished, summary: null };
      if (!d.grid || !d.grid.length) { finish(run, "failed", null, { ...base, status: "refused", error_code: "dataset_grid_unreadable", profile: null, package_sha256: null }); return; }
      const delimiter = d.media_type === "text/csv" ? "," : "\t";
      const { profile, detail } = profileGrid(d.grid, { type: d.media_type === "text/csv" ? "csv" : "tsv", sha256: d.sha256, bytes: d.size_bytes, delimiter }, null);
      const files: Files = [
        { path: "README.md", bytes: readme(`Profile of ${d.file_name}`) },
        { path: "profile.json", bytes: Buffer.from(JSON.stringify(profile, null, 2) + "\n") },
        { path: "profile_detail.json", bytes: Buffer.from(JSON.stringify(detail, null, 2) + "\n") },
        { path: "spec.json", bytes: Buffer.from(JSON.stringify({ schema: "enrichedlab.dataset-intake.v1", dataset: d.dataset_id, grid_sha256: d.grid_sha256,
          profile_sha256: sha(JSON.stringify(profile)) }, null, 2) + "\n") },
      ];
      finish(run, "completed", files, { ...base, status: "profiled", error_code: null, profile, package_sha256: null });
    });
  }
  function preparation(d: Dataset, input: Record<string, unknown>, raw: string, json: Json) {
    const problem = specProblem(input.spec);
    if (problem) return refuse(json, "dataset_preparation_invalid", 400, problem);
    const title = typeof input.title === "string" ? input.title : "";
    if (title.length > 200) return refuse(json, "dataset_preparation_invalid", 400, { path: "title", reason: "too_long" });
    const done = d.intake?.receipts.at(-1);
    if (!done || done.status !== "profiled") return refuse(json, "dataset_not_profiled", 409);
    const previous = typeof input.previous === "string" ? input.previous : null;
    if (previous && !d.preparations.some((p) => p.task.id === previous)) return refuse(json, "dataset_previous_invalid");
    const requestId = String(input.request_id ?? "");
    const same = d.preparations.find((p) => p.request_id === requestId);
    if (same) return same.body === raw ? json({ task_id: same.task.id }, 202) : refuse(json, "idempotency_conflict", 409);
    const spec = input.spec as Spec;
    const run = newRun(d, "preparation", title || spec.table_name.replaceAll("_", " "), previous, d.intake!.task.id);
    const specText = JSON.stringify(spec);
    const prep: Prep = { ...run, title, spec, spec_sha256: sha(specText), previous, request_id: requestId, body: raw,
      request_sha256: sha(JSON.stringify({ dataset: d.dataset_id, spec, title, previous })), table_name: spec.table_name };
    d.preparations.unshift(prep);
    later(delay, () => start(prep));
    later(delay * 2, () => {
      const outcome = prepare(d.grid!, spec);
      const started = prep.task.started_at!, finished = new Date().toISOString();
      const summary = outcome.receipt ? { ...outcome.receipt, grid_sha256: d.grid_sha256, spec_sha256: prep.spec_sha256 } : null;
      const base = { attempt: 1, grid_sha256: d.grid_sha256, profile: null, executor: "enrichedlab.dataset-prepare", executor_version: 2,
        started_at: started, finished_at: finished, summary };
      if (outcome.refusal || !("table" in outcome) || !outcome.table) {
        finish(prep, "failed", null, { ...base, status: "refused", error_code: outcome.refusal ?? "dataset_no_rows", table_path: null, table_sha256: null,
          table_rows: null, table_columns: null, package_sha256: null });
        return;
      }
      const path = `results/lab_data/${spec.table_name}.csv`;
      const [header = []] = csvRows(outcome.table.toString("utf8"));
      const files: Files = [
        { path: "README.md", bytes: readme(`${prep.entry.dataset?.title ?? spec.table_name}, prepared from ${d.file_name}`) },
        { path, bytes: outcome.table },
        { path: `exports/${spec.table_name}.spreadsheet.csv`, bytes: outcome.spreadsheet! },
        { path: "preparation.json", bytes: Buffer.from(specText) },
        { path: "receipt.json", bytes: Buffer.from(JSON.stringify(summary, null, 2) + "\n") },
        { path: "spec.json", bytes: Buffer.from(JSON.stringify({ schema: "enrichedlab.dataset-preparation-record.v1", dataset: d.dataset_id, spec_sha256: prep.spec_sha256,
          grid_sha256: d.grid_sha256, table: { path, sha256: sha(outcome.table) } }, null, 2) + "\n") },
      ];
      finish(prep, "completed", files, { ...base, status: "prepared", error_code: null, table_path: path, table_sha256: sha(outcome.table),
        table_rows: outcome.receipt.rows.written, table_columns: header, package_sha256: null });
    });
    return json({ task_id: prep.task.id }, 202);
  }
  const intakeListed = (d: Dataset) => d.intake ? { task_id: d.intake.task.id, status: d.intake.task.status,
    receipt: d.intake.receipts.length ? (({ status, error_code, profile, finished_at }) => ({ status, error_code, sheets: (profile as { sheets?: unknown[] } | null)?.sheets?.length ?? 0, finished_at }))(d.intake.receipts.at(-1)! as never) : null } : null;
  const prepListed = (p: Prep) => ({ task_id: p.task.id, title: p.title || null, table_name: p.table_name, previous: p.previous, status: p.task.status,
    created_at: p.task.submitted_at, receipt: p.receipts.length ? (({ status, error_code, table_path, table_sha256, table_rows, finished_at }) =>
      ({ status, error_code, table_path, table_sha256, table_rows, finished_at }))(p.receipts.at(-1)! as never) : null });
  const record = (d: Dataset) => ({ dataset_id: d.dataset_id, project_id: d.project_id, request_id: d.request_id, file_name: d.file_name, media_type: d.media_type,
    size_bytes: d.size_bytes, sha256: d.sha256, guard: d.guard, grid_sha256: d.grid_sha256, grid_bytes: d.grid_bytes, intake_task: d.intake?.task.id ?? null,
    intake_waiting: d.guard.outcome === "allowed" && !d.intake, principal_id: d.principal_id, created_at: d.created_at });
  const listed = (d: Dataset) => ({ ...record(d), intake: intakeListed(d), preparations: d.preparations.map(prepListed) });
  const detail = (d: Dataset) => ({ ...record(d),
    intake: d.intake ? { task_id: d.intake.task.id, status: d.intake.task.status, error_code: d.intake.task.error_code, receipts: d.intake.receipts } : null,
    preparations: d.preparations.map((p) => ({ task_id: p.task.id, title: p.title || null, spec: p.spec, spec_sha256: p.spec_sha256, previous: p.previous,
      status: p.task.status, error_code: p.task.error_code, request_sha256: p.request_sha256, principal_id: me, created_at: p.task.submitted_at, receipts: p.receipts })) });

  async function upload(req: IncomingMessage, url: URL, json: Json) {
    if (req.headers["x-agent-sandbox-intent"] !== "mutate") return refuse(json, "mutation_intent_required", 403);
    let name = "";
    try { name = decodeURIComponent(String(req.headers["x-dataset-name"] ?? "")); } catch { name = ""; }
    if (!name || name.length > 255 || /[\/\\\u0000-\u001f\u007f\u200e\u200f\u202a-\u202e\u2066-\u2069]/.test(name)) return refuse(json, "dataset_name_invalid");
    const media = TYPES_BY_EXTENSION[name.split(".").pop()!.toLowerCase()];
    if (!media || !name.includes(".")) return refuse(json, "dataset_type_unsupported", 415);
    const requestId = url.searchParams.get("request_id") ?? "";
    if (!/^[0-9a-f-]{36}$/.test(requestId)) return refuse(json, "dataset_request_invalid");
    const chunks: Buffer[] = [];
    let size = 0;
    for await (const chunk of req) { size += (chunk as Buffer).length; if (size > MAX_BYTES) return refuse(json, "dataset_too_large", 413); chunks.push(chunk as Buffer); }
    const bytes = Buffer.concat(chunks);
    if (!bytes.length) return refuse(json, "dataset_empty");
    const digest = sha(bytes);
    const again = datasets.find((d) => d.request_id === requestId);
    if (again) {
      if (again.file_name !== name || again.media_type !== media || again.size_bytes !== bytes.length || again.sha256 !== digest) return refuse(json, "idempotency_conflict", 409);
      if (again.guard.outcome === "allowed" && !again.intake && !queueFull) intake(again);
      return json({ dataset: record(again), created: false }, 200);
    }
    // The stand-in guard: an Excel 97-2003 file is blocked, a workbook with macros held, and a text table read as a grid.
    const outcome = media === "application/vnd.ms-excel" ? "blocked" : media.includes("macroenabled") ? "quarantined" : "allowed";
    const findings = outcome === "blocked" ? ["type_not_allowed"] : outcome === "quarantined" ? ["active_macros"] : [];
    const text = media.startsWith("text/");
    const grid = outcome === "allowed" && text ? gridOf(bytes, media === "text/csv" ? "," : "\t") : outcome === "allowed" ? [] : null;
    const gridBytes = grid ? Buffer.from(grid.map((r) => JSON.stringify(r)).join("\n")) : null;
    const d: Dataset = { dataset_id: randomUUID(), project_id: project, request_id: requestId, file_name: name, media_type: media, size_bytes: bytes.length,
      sha256: digest, guard: { intake: randomUUID(), outcome, findings, policy: POLICY }, grid_sha256: gridBytes ? sha(gridBytes) : null,
      grid_bytes: gridBytes?.length ?? null, principal_id: me, created_at: new Date().toISOString(), grid, intake: null, preparations: [] };
    datasets.unshift(d);
    if (outcome === "allowed" && !queueFull) intake(d);
    return json({ dataset: record(d), created: true }, 201);
  }

  function lineage(dataset: Dataset, run: Run, prep: Prep | null) {
    const t = run.task;
    const receipt = run.receipts.at(-1);
    return {
      schema: "enrichedlab.task-lineage.v1",
      task: { task_id: t.id, project_id: project, action_key: run.entry.action_key, objective: "", status: t.status, result_class: t.result_class,
        error_code: t.error_code, attempt: t.attempt, submitted_at: t.submitted_at, started_at: t.started_at, finished_at: t.finished_at },
      origin: { origin: "person", basis: "submission", principal_id: me, principal_name: null, principal_kind: "person" },
      plan_step: null,
      inputs: { workspace: null, checkpoint_revision: null, platform_tool: null, classes: [prep ? "prior_output" : "object_data"], classes_recorded_by: "dataset_request" },
      method: { profile, profile_name: "Lab data", tool_class: "vetted_service", template_ref: null, template_sha256: null, recorded: "submission",
        harness: null, role: null, tool_bundle: null },
      code: null, query: null, render: null,
      dataset: { dataset: dataset.dataset_id, operation: prep ? "preparation" : "intake", file_name: dataset.file_name, file_sha256: dataset.sha256,
        guard_intake: dataset.guard.intake, guard_policy: dataset.guard.policy, grid_sha256: dataset.grid_sha256, spec_sha256: prep?.spec_sha256 ?? null,
        previous: prep?.previous ?? null, title: prep?.entry.dataset?.title ?? null, request_sha256: prep?.request_sha256 ?? sha(dataset.request_id),
        created_at: t.submitted_at, executor: prep ? "enrichedlab.dataset-prepare" : "enrichedlab.dataset-profile", executor_version: prep ? 2 : 1,
        table_sha256: (receipt?.table_sha256 as string | null) ?? null },
      execution: { image_ref: null, cpu_limit: null, memory_limit: null, max_run_seconds: 300, attempts: t.attempt, scientific_history: 0 },
      outputs: run.artifacts.map((a) => ({ artifact_id: a.id, role: a.role, label: a.label, relative_path: a.relative_path, media_type: a.media_type,
        size_bytes: a.size_bytes, sha256: a.sha256, created_at: a.created_at, provenance_class: "vetted_tool_output" })),
      review: { state: "unvalidated" },
      gaps: [],
    };
  }

  return {
    owns: (id: string) => !!runOf(id),
    /** The prepared tables, as sources a figure or a template analysis can read. */
    prepared: () => datasets.flatMap((d) => d.preparations.filter((p) => p.task.status === "completed" && p.pkg).map((p) => ({
      project, group, sourceTask: p.task.id, source: () => ({ files: p.pkg!.files, artifact: p.artifacts[0] }),
      task: () => ({ status: p.task.status, objective: p.entry.dataset?.title ?? p.table_name }), addProjectTask: options.addProjectTask }))),
    reset() {
      for (const t of timers) clearTimeout(t);
      timers.clear();
      const gone = new Set<string>();
      for (const d of datasets) for (const run of [d.intake, ...d.preparations]) if (run) gone.add(run.task.id);
      for (const id of gone) { const i = tasks.findIndex((t) => t.id === id); if (i >= 0) tasks.splice(i, 1); histories.delete(id); }
      datasets.length = 0;
      queueFull = false;
    },
    async handle(req: IncomingMessage, res: ServerResponse, url: URL, json: Json, denied: () => void): Promise<boolean> {
      const path = url.pathname;
      if (path === "/api/v1/fixture/dataset-queue-full" && req.method === "POST") {
        queueFull = url.searchParams.get("full") !== "false";
        json({ queue_full: queueFull });
        return true;
      }
      const route = path.match(/^\/api\/v1\/projects\/([0-9a-f-]{36})\/datasets(?:\/([0-9a-f-]{36})(\/intake|\/preparations)?)?$/);
      if (route) {
        const [, asked, id, action] = route;
        // The preview's other projects are in labs without the dataset method: nothing listed, and an upload refused.
        if (asked !== project) {
          if (!id && req.method === "GET") json({ datasets: [] });
          else if (!id && req.method === "POST") refuse(json, "dataset_method_unavailable", 403);
          else refuse(json, "dataset_not_found", 404);
          return true;
        }
        if (!id) {
          if (req.method === "POST") await upload(req, url, json);
          else if (req.method === "GET") json({ datasets: datasets.map(listed) });
          else denied();
          return true;
        }
        const d = datasets.find((x) => x.dataset_id === id);
        if (!d) { refuse(json, "dataset_not_found", 404); return true; }
        if (!action && req.method === "GET") { json({ dataset: detail(d) }); return true; }
        if (req.method !== "POST" || !action) { denied(); return true; }
        if (req.headers["x-agent-sandbox-intent"] !== "mutate") { refuse(json, "mutation_intent_required", 403); return true; }
        if (action === "/intake") {
          if (d.guard.outcome !== "allowed") { refuse(json, "dataset_not_allowed", 409); return true; }
          if (!d.intake) { if (queueFull) { refuse(json, "group_queue_full", 429); return true; } intake(d); }
          json({ dataset: record(d) });
          return true;
        }
        let raw = "";
        for await (const chunk of req) raw += chunk;
        let input: Record<string, unknown>;
        try { input = JSON.parse(raw); } catch { refuse(json, "dataset_preparation_invalid", 400, { path: "", reason: "wrong_type" }); return true; }
        preparation(d, input, raw, json);
        return true;
      }
      const taskRoute = path.match(/^\/api\/v1\/tasks\/([0-9a-f-]{36})(\/.*)$/);
      if (!taskRoute) return false;
      const [, id, suffix] = taskRoute;
      const found = runOf(id);
      if (!found) return false;
      const { dataset, run, prep } = found;
      if (suffix === "/artifacts") { json({ artifacts: run.artifacts }); return true; }
      if (suffix === "/lineage") { json(lineage(dataset, run, prep)); return true; }
      if (suffix === "/generated-code") { json({ generated_code: null }); return true; }
      if (suffix === "/query") { json({ query: null }); return true; }
      const contents = suffix.match(/^\/artifacts\/([0-9a-f-]{36})\/(contents(?:\/file)?|download|pages|summary)$/);
      if (contents && run.pkg) {
        const [packaged, checksum] = run.artifacts;
        const artifact = run.artifacts.find((a) => a.id === contents[1]);
        if (!artifact) { denied(); return true; }
        if (contents[2] === "pages" || contents[2] === "summary") {
          answerTable(contents[2], url, artifact === packaged ? run.pkg.files : null, { path: artifact.relative_path, bytes: run.pkg.checksum }, json);
          return true;
        }
        if (contents[2] === "contents") {
          if (artifact !== packaged) { refuse(json, "artifact_not_a_package", 415); return true; }
          json({ artifact_id: artifact.id, sha256: artifact.sha256, size_bytes: artifact.size_bytes, listing: listing(run.pkg.files) });
          return true;
        }
        const bytes = contents[2] === "download" ? (artifact === checksum ? run.pkg.checksum : run.pkg.archive)
          : run.pkg.files.find((f) => f.path === url.searchParams.get("path"))?.bytes;
        if (!bytes) { denied(); return true; }
        res.setHeader("Content-Type", contents[2] === "download" ? artifact.media_type : "application/octet-stream");
        res.setHeader("Content-Length", bytes.length);
        res.setHeader("X-Artifact-Sha256", sha(bytes));
        res.end(bytes);
        return true;
      }
      return false;
    },
  };
}
