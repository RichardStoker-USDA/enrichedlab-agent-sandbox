// Lab data: a person sends a table the lab keeps, the upload guard checks it
// and rebuilds it as a cell grid, the broker profiles the grid, and the person approves a preparation that the broker's
// own code applies. No model reads or writes a cell. This module holds the shapes, the requests, the starting
// specification a profile suggests, the edits the page makes to it and the words; the pages draw them.
import { BrokerError, request } from "./api.ts";

export type GuardOutcome = "allowed" | "quarantined" | "blocked";
export interface DatasetRecord {
  dataset_id: string; project_id: string; request_id: string; file_name: string; media_type: string; size_bytes: number; sha256: string;
  guard: { intake: string; outcome: GuardOutcome; findings: string[]; policy: string };
  grid_sha256: string | null; grid_bytes: number | null; intake_task: string | null; principal_id: string; created_at: string;
  /** The lab's queue was full when the file came in; the intake starts with `startIntake`. */
  intake_waiting?: boolean;
}
export interface ListedPreparation {
  task_id: string; title: string | null; table_name: string; previous: string | null; status: string; created_at: string;
  receipt: { status: string; error_code: string | null; table_path: string | null; table_sha256: string | null; table_rows: number | null;
    finished_at: string } | null;
}
export interface ListedDataset extends DatasetRecord {
  intake: { task_id: string; status: string; receipt: { status: string; error_code: string | null; sheets: number; finished_at: string } | null } | null;
  preparations: ListedPreparation[];
}
export interface DatasetReceipt {
  attempt: number; status: "profiled" | "prepared" | "refused" | "failed"; error_code: string | null; profile: Profile | null;
  summary: PreparationSummary | null; grid_sha256: string | null; table_path: string | null; table_sha256: string | null; table_rows: number | null;
  table_columns: string[] | null; package_sha256: string | null; executor: string | null; executor_version: number | null;
  started_at: string; finished_at: string;
}
export interface Preparation {
  task_id: string; title: string | null; spec: Spec; spec_sha256: string; previous: string | null; status: string;
  error_code: string | null; request_sha256: string; principal_id: string; created_at: string; receipts: DatasetReceipt[];
}
export interface DatasetDetail extends DatasetRecord {
  intake: { task_id: string; status: string; error_code: string | null; receipts: DatasetReceipt[] } | null;
  preparations: Preparation[];
}

export type ColumnKind = "number" | "integer" | "date" | "datetime" | "time" | "duration" | "logical" | "text" | "empty" | "mixed";
export interface ProfileColumn {
  index: number; letter: string; name: string; name_from?: "header" | "letter"; suggested_name: string; unit: string | null;
  unit_from?: "units_row" | "header" | null; type: ColumnKind; date_order: "day_first" | "month_first" | "ambiguous" | null;
  present: number; missing: Record<string, number>; censored: { below_limit?: number; above_limit?: number; not_detected?: number };
  other: number; distinct: number | null; issues: string[];
}
export interface TableShape {
  header_rows: number[]; header_basis: string | null; units_row: number | null; title_rows: number[]; first_data_row: number;
  last_data_row: number; data_rows: number; blank_rows: number; summary_rows: number[]; note_rows: number[];
}
export interface ProfileSheet {
  index: number; name: string | null; hidden: boolean; row_count: number; column_count: number; columns_omitted: number;
  table: TableShape | null; columns: ProfileColumn[]; issues: { code: string; columns: string[]; count: number }[];
}
export interface Profile {
  schema: string; source: { type: string; sha256: string; bytes: number; encoding: string | null; delimiter: string | null };
  sheets: ProfileSheet[];
}
/** The profile's detail (`profile_detail.json` in the intake's package): the person's own cells, shown to people only. */
export interface SheetDetail {
  index: number; preview: string[][]; title_text: string[]; note_text: string[];
  columns: { letter: string; values: [string, number][]; min: string | null; max: string | null; examples: Record<string, string[]> }[];
}
export interface ProfileDetail { schema: string; sheets: SheetDetail[] }

const strings = (value: unknown) => (Array.isArray(value) ? value.filter((x): x is string => typeof x === "string") : []);
/** The profile's detail as read from its checked package, kept to the shape the page draws: anything else is dropped. */
export function detailFrom(value: unknown): ProfileDetail | null {
  const v = value as { schema?: unknown; sheets?: unknown } | null;
  if (!v || typeof v !== "object" || !Array.isArray(v.sheets)) return null;
  const sheets: SheetDetail[] = [];
  for (const raw of v.sheets as Record<string, unknown>[]) {
    if (!raw || !Number.isInteger(raw.index) || !Array.isArray(raw.preview) || !Array.isArray(raw.columns)) continue;
    const preview = (raw.preview as unknown[]).slice(0, 40).map((row) => strings(row).slice(0, 512));
    const columns = (raw.columns as Record<string, unknown>[]).filter((c) => c && typeof c.letter === "string" && /^[A-Z]{1,3}$/.test(c.letter as string)).map((c) => {
      const examples: Record<string, string[]> = {};
      if (c.examples && typeof c.examples === "object") for (const [k, list] of Object.entries(c.examples as Record<string, unknown>)) examples[k] = strings(list).slice(0, 12);
      const values = Array.isArray(c.values) ? (c.values as unknown[]).filter((pair): pair is [string, number] => Array.isArray(pair) && typeof pair[0] === "string"
        && typeof pair[1] === "number").slice(0, 50) : [];
      return { letter: c.letter as string, values, min: typeof c.min === "string" ? c.min : null, max: typeof c.max === "string" ? c.max : null, examples };
    });
    sheets.push({ index: raw.index as number, preview, title_text: strings(raw.title_text), note_text: strings(raw.note_text), columns });
  }
  return { schema: typeof v.schema === "string" ? v.schema : "", sheets };
}

export type OutputType = "text" | "category" | "number" | "integer" | "logical" | "date" | "datetime" | "time";
export type CensorRule = "missing" | "limit" | "half_limit" | "limit_over_sqrt2";
export interface ColumnSpec {
  from: string; name: string; type: OutputType; unit?: string | null; decimal?: "point" | "comma"; thousands?: "none" | "comma" | "point" | "space";
  recode?: Record<string, string>; censored?: { rule: CensorRule; limit?: number | null; upper_limit?: number | null };
  strip_percent?: boolean; true_values?: string[]; false_values?: string[]; other?: "refuse" | "missing";
}
export type ValueSpec = Omit<ColumnSpec, "from">;
export interface Spec {
  schema: "enrichedlab.dataset-preparation.v1"; table_name: string; sheet: number; header_rows: number[]; units_row: number | null;
  rows: { first: number; last: number }; drop_rows: [number, number][]; missing: string[]; decimal: "point" | "comma";
  thousands: "none" | "comma" | "point" | "space"; date_order?: "day_first" | "month_first"; trim: boolean; columns: ColumnSpec[];
  long?: { columns: string[]; name: string; values: (string | number)[]; value: ValueSpec };
  key: string[];
}
export interface ColumnCounts {
  name: string; from: string; type: string; values: number; missing: Record<string, number>; trimmed: number; recoded: Record<string, number>;
  recoded_unlisted?: number; decimal_commas: number; thousands_separators: number; percent_signs: number; plus_signs?: number; dates_from_text: number;
  censored: Record<string, number>; censored_rule: string | null; unread_as_missing: number;
}
export interface Unread { row: number; column: string; reason: string; text: string }
export interface PreparationSummary {
  schema: string; executor: { name: string; version: number }; status: string; grid_sha256: string; spec_sha256: string;
  rows: { read: number; blank: number; dropped: number; kept: number; written: number };
  cells?: { filled: number; outside_rows: number; in_dropped_rows: number; in_unread_columns: number; read: number };
  columns: ColumnCounts[];
  outputs: { name: string; from: string; type: string; unit: string | null }[];
  failures: Unread[]; failure_count: number; read_as_missing: Unread[];
  repeated_keys: { row: number; first_row: number }[]; repeated_key_count: number;
  table: { path: string; sha256: string; bytes: number; rows: number } | null;
  spreadsheet_copy: { path: string; sha256: string; bytes: number; rows: number } | null;
  recodes_left_out?: boolean;
}

const UUID = /^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/i;
const id = (value: string) => { if (!UUID.test(value)) throw new BrokerError("invalid_reference"); return value; };
const root = (project: string) => `/api/v1/projects/${id(project)}/datasets`;
const mutate = { "X-Agent-Sandbox-Intent": "mutate" };

/** The file types the guard reads; .xls is blocked by name, so the picker still offers it to say why. */
export const ACCEPT = ".csv,.tsv,.tab,.txt,.dat,.xlsx,.xlsm,.xls";
export const MAX_LAB_FILE = 32 * 1024 * 1024;

export const labData = {
  list: async (project: string, signal?: AbortSignal) => (await request<{ datasets: ListedDataset[] }>(root(project), { signal })).datasets,
  get: async (project: string, dataset: string, signal?: AbortSignal) =>
    (await request<{ dataset: DatasetDetail }>(`${root(project)}/${id(dataset)}`, { signal })).dataset,
  /** Sends one file. Its request id stays with the chosen file, so a retry answers the same dataset
   *  without the guard reading it again. The browser sets the length for a File. */
  upload: async (project: string, file: File, requestId: string) =>
    (await request<{ dataset: DatasetRecord; created: boolean }>(`${root(project)}?request_id=${id(requestId)}`, {
      method: "POST", body: file, headers: { ...mutate, "X-Dataset-Name": encodeURIComponent(file.name) },
      signal: AbortSignal.timeout(5 * 60 * 1000) })),
  startIntake: async (project: string, dataset: string) =>
    (await request<{ dataset: DatasetRecord }>(`${root(project)}/${id(dataset)}/intake`, { method: "POST", headers: mutate })).dataset,
  prepare: async (project: string, dataset: string, body: { spec: Spec; title: string; previous: string | null; request_id: string }) =>
    (await request<{ task_id: string }>(`${root(project)}/${id(dataset)}/preparations`, {
      method: "POST", headers: { ...mutate, "Content-Type": "application/json" }, body: JSON.stringify(body) })).task_id,
};

export const NAME = /^[a-z][a-z0-9_]{0,62}$/;
/** A name R and the analyses take as written: lower case, starting with a letter, letters, digits and underscores. */
export function tableName(text: string): string {
  const name = text.toLowerCase().normalize("NFKD").replace(/[^a-z0-9]+/g, "_").replace(/^_+|_+$/g, "").replace(/^(?=[0-9_])/, "t_");
  return (name || "lab_table").slice(0, 63).replace(/_+$/, "");
}
const letterIndex = (l: string) => [...l].reduce((n, c) => n * 26 + c.charCodeAt(0) - 64, 0) - 1;
const readsAsMissing = (text: string) => !text.trim() || text === "NA";

/** The table's columns in order, as the broker writes them: each column, a censored one followed by its direction and
 *  limit, then a long layout's point and value. */
export function outputsOf(spec: Spec): { name: string; type: string; unit: string | null; from: string }[] {
  const out: { name: string; type: string; unit: string | null; from: string }[] = [];
  const push = (col: ValueSpec, from: string) => {
    out.push({ name: col.name, type: col.type, unit: col.unit ?? null, from });
    if (col.censored) out.push({ name: `${col.name}_censored`, type: "category", unit: null, from: col.name },
      { name: `${col.name}_limit`, type: "number", unit: col.unit ?? null, from: col.name });
  };
  for (const col of spec.columns) push(col, col.from);
  if (spec.long) {
    out.push({ name: spec.long.name, type: spec.long.values.every((v) => typeof v === "number") ? "number" : "category", unit: null, from: "long" });
    push(spec.long.value, "long");
  }
  return out;
}

/** The type a profiled column starts as: text with few different values starts as a category; durations, empty
 *  columns and columns of mixed kinds start left out or as text. */
function typeOf(c: ProfileColumn): OutputType | null {
  if (c.type === "empty" || c.type === "duration") return null;
  if (c.type === "mixed") return "text";
  if (c.type === "text") return c.distinct !== null && c.distinct < c.present && c.distinct <= Math.max(12, Math.floor(c.present / 2)) ? "category" : "text";
  return c.type;
}
const spellingKey = (text: string) => text.split(/\s+/).filter(Boolean).join(" ").replace(/\.+$/, "").toLowerCase();

/** One spelling for each category written several ways: the most frequent one as the sheet writes it (trimmed), else
 *  lower case. Keys are the trimmed texts, as the broker reads them. */
export function suggestedRecode(detail: SheetDetail["columns"][number] | null): Record<string, string> {
  const counts = new Map<string, number>();
  for (const [text, n] of detail?.values ?? []) counts.set(text.trim(), (counts.get(text.trim()) ?? 0) + n);
  const groups = new Map<string, string[]>();
  for (const text of counts.keys()) groups.set(spellingKey(text), [...(groups.get(spellingKey(text)) ?? []), text]);
  const recode: Record<string, string> = {};
  for (const [key, spellings] of groups) {
    if (spellings.length < 2) continue;
    const chosen = [...spellings].sort((a, b) => (counts.get(b) ?? 0) - (counts.get(a) ?? 0) || (a === key ? -1 : b === key ? 1 : a < b ? -1 : 1))[0];
    for (const s of spellings) if (s !== chosen && !readsAsMissing(chosen)) recode[s] = chosen;
  }
  return recode;
}

/** The starting specification a profile suggests: the sheet's table as the profile found
 *  it, its summary rows left out, one column per profiled column with its suggested name, unit and type, the missing
 *  words the columns hold, one spelling per category, and the long layout for one measure across columns. Values
 *  reported against a limit start as missing, which invents no number. The person changes it, then approves. */
export function startingSpec(sheet: ProfileSheet, fileName: string, detail: SheetDetail | null = null): Spec {
  const t = sheet.table;
  const first = t?.first_data_row ?? 1, last = t ? Math.max(first, t.last_data_row) : Math.max(1, sheet.row_count);
  const inside = (t?.summary_rows ?? []).filter((r) => r >= first && r <= last);
  const missing = [...new Set(sheet.columns.flatMap((c) => Object.keys(c.missing)).filter((token) => token !== ""))].sort();
  const wide = sheet.issues.find((i) => i.code === "wide_time_points" && i.columns.length >= 2);
  const detailOf = (letter: string) => detail?.columns.find((c) => c.letter === letter) ?? null;
  const columnOf = (c: ProfileColumn): ColumnSpec | null => {
    const type = typeOf(c);
    if (!type) return null;
    const out: ColumnSpec = { from: c.letter, name: c.suggested_name, type };
    if (c.unit) out.unit = c.unit;
    if (c.issues.includes("decimal_commas") && (type === "number" || type === "integer")) out.decimal = "comma";
    if (c.issues.includes("censored_values") && (type === "number" || type === "integer")) out.censored = { rule: "missing" };
    if (c.issues.includes("category_spellings") && (type === "category" || type === "text")) {
      const recode = suggestedRecode(detailOf(c.letter));
      if (Object.keys(recode).length) out.recode = recode;
    }
    return out;
  };
  const longLetters = new Set(wide?.columns ?? []);
  const columns = sheet.columns.filter((c) => !longLetters.has(c.letter)).map(columnOf).filter((c): c is ColumnSpec => !!c);
  const spec: Spec = {
    schema: "enrichedlab.dataset-preparation.v1", table_name: tableName(fileName.replace(/\.[a-z0-9]+$/i, "")), sheet: sheet.index,
    header_rows: t?.header_rows ?? [], units_row: t?.units_row ?? null, rows: { first, last }, drop_rows: rangesOf(inside),
    missing, decimal: "point", thousands: "none", trim: true, columns, key: [],
  };
  const dated = sheet.columns.find((c) => c.date_order === "day_first" || c.date_order === "month_first");
  if (dated?.date_order === "day_first" || dated?.date_order === "month_first") spec.date_order = dated.date_order;
  if (wide) setLong(spec, sheet, wide.columns);
  const idColumn = sheet.columns.find((c) => c.type === "text" && c.distinct === c.present && c.present > 1 && /\b(id|mouse|animal|subject|sample|participant)\b/i.test(c.name)
    && spec.columns.some((s) => s.from === c.letter));
  if (idColumn) spec.key = [spec.columns.find((s) => s.from === idColumn.letter)!.name, ...(spec.long ? [spec.long.name] : [])];
  return spec;
}

/** Row numbers as the fewest ranges: 13 and 14 become 13 to 14. */
export function rangesOf(rows: number[]): [number, number][] {
  const sorted = [...new Set(rows)].sort((a, b) => a - b);
  const out: [number, number][] = [];
  for (const r of sorted) {
    const prev = out[out.length - 1];
    if (prev && r === prev[1] + 1) prev[1] = r; else out.push([r, r]);
  }
  return out;
}
const dropped = (spec: Spec, row: number) => spec.drop_rows.some(([a, b]) => row >= a && row <= b);

/** What a sheet's point column says: minutes for "min", hours for "h" or "hr", days for "d" or "day", else point. */
function pointName(unit: string): string {
  const u = unit.toLowerCase();
  return u === "min" || u === "mins" || u === "minute" || u === "minutes" ? "minutes" : u === "h" || u === "hr" || u === "hrs" || u === "hour" || u === "hours" ? "hours"
    : u === "d" || u === "day" || u === "days" ? "days" : u === "wk" || u === "week" || u === "weeks" ? "weeks" : u === "s" || u === "sec" ? "seconds" : "point";
}
const POINT = /^((?:[a-z]+[\s_-]+)*?[a-z]{0,12})[\s_=-]*(\d+(?:\.\d+)?)\s*([a-z]{0,7})$/i;

/** One measure across several columns as one row per point: the columns leave the column list and become the long
 *  layout's value, with the header's numbers as its points. */
export function setLong(spec: Spec, sheet: ProfileSheet, letters: string[]): Spec {
  const parts = letters.map((l) => sheet.columns.find((c) => c.letter === l)).filter((c): c is ProfileColumn => !!c);
  if (parts.length < 2) return spec;
  const parsed = parts.map((c) => c.name.trim().match(POINT));
  const values = parsed.map((m, i) => (m ? Number(m[2]) : parts[i].name));
  const numbers = values.every((v) => typeof v === "number") && new Set(values).size === values.length;
  const stem = parsed[0]?.[1]?.trim() || parts[0].name.replace(/\s*-?\d+(\.\d+)?\s*[a-z]*$/i, "").trim() || "value";
  const used = new Set(spec.columns.filter((c) => !letters.includes(c.from)).map((c) => c.name));
  let valueName = tableName(stem);
  if (used.has(valueName)) valueName = `${valueName}_value`.slice(0, 63);
  let pointColumn = pointName(parsed[0]?.[3] ?? "");
  if (used.has(pointColumn) || pointColumn === valueName) pointColumn = `${pointColumn}_point`.slice(0, 63);
  const valueType: OutputType = parts.every((c) => c.type === "integer") ? "integer" : "number";
  const value: ValueSpec = { name: valueName, type: valueType };
  if (parts[0].unit) value.unit = parts[0].unit;
  if (parts.some((c) => c.issues.includes("censored_values"))) value.censored = { rule: "missing" };
  spec.columns = spec.columns.filter((c) => !letters.includes(c.from));
  spec.long = { columns: parts.map((c) => c.letter), name: pointColumn, values: numbers ? values : parts.map((c) => c.name.trim()), value };
  spec.key = spec.key.length ? [...spec.key.filter((k) => outputsOf(spec).some((o) => o.name === k)), ...(spec.key.includes(pointColumn) ? [] : [pointColumn])] : [];
  return spec;
}

/** The long layout undone: each of its columns read again on its own, by its profiled name. */
export function setWide(spec: Spec, sheet: ProfileSheet): Spec {
  const long = spec.long;
  if (!long) return spec;
  delete spec.long;
  const back = long.columns.map((l) => sheet.columns.find((c) => c.letter === l)).filter((c): c is ProfileColumn => !!c)
    .map((c): ColumnSpec => ({ from: c.letter, name: c.suggested_name, type: long.value.type, ...(c.unit ? { unit: c.unit } : {}),
      ...(long.value.censored ? { censored: { ...long.value.censored } } : {}), ...(long.value.other ? { other: long.value.other } : {}) }));
  spec.columns = [...spec.columns, ...back].sort((a, b) => letterIndex(a.from) - letterIndex(b.from));
  spec.key = spec.key.filter((k) => k !== long.name);
  return spec;
}

/** A column's rule, wherever it sits: its own entry, or the long layout's value for one of the layout's columns. */
export function ruleFor(spec: Spec, letter: string): ValueSpec | null {
  return spec.columns.find((c) => c.from === letter) ?? (spec.long?.columns.includes(letter) ? spec.long.value : null);
}

/** A data row in or out. Rows outside the table's range are not data, so they are not toggled here. */
export function toggleRow(spec: Spec, row: number): Spec {
  if (row < spec.rows.first || row > spec.rows.last) return spec;
  const rows = new Set(spec.drop_rows.flatMap(([a, b]) => Array.from({ length: b - a + 1 }, (_, i) => a + i)));
  if (rows.has(row)) rows.delete(row); else rows.add(row);
  spec.drop_rows = rangesOf([...rows]);
  return spec;
}

/** A column read or left out. A column of the long layout is left out by undoing the layout first. */
export function toggleColumn(spec: Spec, sheet: ProfileSheet, letter: string): Spec {
  if (spec.long?.columns.includes(letter)) return spec;
  const at = spec.columns.findIndex((c) => c.from === letter);
  if (at >= 0) {
    const name = spec.columns[at].name;
    spec.columns.splice(at, 1);
    spec.key = spec.key.filter((k) => k !== name);
    return spec;
  }
  const c = sheet.columns.find((x) => x.letter === letter);
  if (!c) return spec;
  const taken = new Set(outputsOf(spec).map((o) => o.name));
  let name = c.suggested_name;
  for (let n = 2; taken.has(name); n++) name = `${c.suggested_name.slice(0, 59)}_${n}`;
  spec.columns.push({ from: letter, name, type: typeOf(c) ?? "text", ...(c.unit ? { unit: c.unit } : {}) });
  spec.columns.sort((a, b) => letterIndex(a.from) - letterIndex(b.from));
  return spec;
}

/** A row's part in the table as the specification reads it, in the sheet's own numbering. */
export type RowRole = "title" | "names" | "units" | "data" | "left_out" | "summary" | "blank" | "note" | "outside";
export function rowRole(spec: Spec, table: TableShape | null, row: number, blank: boolean): RowRole {
  if (spec.header_rows.includes(row)) return "names";
  if (spec.units_row === row) return "units";
  if (row >= spec.rows.first && row <= spec.rows.last) {
    if (dropped(spec, row)) return table?.summary_rows.includes(row) ? "summary" : "left_out";
    return blank ? "blank" : "data";
  }
  if (table?.title_rows.includes(row)) return "title";
  if (table?.note_rows.includes(row)) return "note";
  return "outside";
}

/** The places in a specification the page checks before sending it; the broker checks it again. */
export function specProblems(spec: Spec): { place: string; words: string }[] {
  const problems: { place: string; words: string }[] = [];
  if (!NAME.test(spec.table_name)) problems.push({ place: "table_name", words: "The table's name must start with a lower-case letter and use only lower-case letters, digits and underscores." });
  for (const c of spec.columns) if (!NAME.test(c.name)) problems.push({ place: `column:${c.from}`, words: `Column ${c.from}'s name must start with a lower-case letter and use only lower-case letters, digits and underscores.` });
  if (spec.long && !NAME.test(spec.long.name)) problems.push({ place: "long.name", words: "The name of the point column must start with a lower-case letter and use only lower-case letters, digits and underscores." });
  if (spec.long && !NAME.test(spec.long.value.name)) problems.push({ place: "long.value", words: "The name of the measure must start with a lower-case letter and use only lower-case letters, digits and underscores." });
  const names = outputsOf(spec).map((o) => o.name);
  const twice = [...new Set(names.filter((n, i) => names.indexOf(n) !== i))];
  for (const n of twice) problems.push({ place: `name:${n}`, words: `Two columns would both be named ${n}.` });
  for (const c of [...spec.columns, ...(spec.long ? [spec.long.value] : [])]) {
    const name = c.name;
    for (const [from, to] of Object.entries(c.recode ?? {})) if (readsAsMissing(to)) problems.push({ place: `recode:${name}:${from}`, words: `“${from}” cannot become “${to}”: R reads an empty text or NA as missing.` });
  }
  if (!spec.columns.length) problems.push({ place: "columns", words: "Read at least one column outside the time points." });
  return problems;
}

/** A first figure for a prepared table in the long layout: its points across, its values up and the first category
 *  as the colour, as a scatter plot when the catalog has one with those settings. Other tables start as the editor
 *  chooses. */
export function figureStart(spec: Spec, features: readonly { id: string; params: Record<string, { type: string }> }[], title: string)
  : { feature: string; controls: Record<string, string>; title: string; previous: null } | null {
  const scatter = features.find((f) => f.id === "scatter" && f.params.x?.type === "column" && f.params.y?.type === "column");
  if (!spec.long || !scatter) return null;
  const controls: Record<string, string> = { x: spec.long.name, y: spec.long.value.name };
  const category = spec.columns.find((c) => c.type === "category");
  if (category && scatter.params.color?.type === "column") controls.color = category.name;
  return { feature: "scatter", controls, title, previous: null };
}

/** What the guard's decision means for the person. */
export function guardWords(guard: DatasetRecord["guard"]): { taken: boolean; text: string; notes: string[] } {
  const words = guard.findings.map(findingWords);
  if (guard.outcome === "allowed") return { taken: true, text: "Checked by the upload guard and taken.", notes: words };
  const held = guard.outcome === "quarantined" ? "The upload guard held this file, so it was not taken" : "The upload guard blocked this file, so it was not taken";
  return { taken: false, text: `${held}${words.length ? `: ${words.join("; ")}` : ""}.`, notes: [] };
}
const FINDINGS: Record<string, string> = {
  type_not_allowed: "Excel 97-2003 files (.xls) are not read; save it as .xlsx or CSV and send that",
  active_macros: "it holds macros, which are never run here; save it without macros as .xlsx or CSV",
  formulas_without_values: "some formulas have no saved values, so those cells are empty here",
  external_links: "it links to other files, which are not followed",
  encrypted: "it is password protected; save an unprotected copy",
};
export const findingWords = (code: string) => FINDINGS[code] ?? (/^yara_/.test(code) ? "a malware check flagged it" : code.replaceAll("_", " "));

/** The dataset's state in a few words, for lists. */
export function datasetState(d: ListedDataset): { words: string; tone: "done" | "active" | "attention" | "failed" | "quiet" } {
  if (d.guard.outcome !== "allowed") return { words: d.guard.outcome === "quarantined" ? "Held by the upload guard" : "Blocked by the upload guard", tone: "failed" };
  if (!d.intake) return { words: "Waiting to be read", tone: "attention" };
  const r = d.intake.receipt;
  if (!r) return { words: d.intake.status === "running" ? "Being read" : "Waiting to be read", tone: "active" };
  if (r.status !== "profiled") return { words: "Not read", tone: "failed" };
  const ready = d.preparations.find((p) => p.receipt?.status === "prepared");
  const busy = d.preparations.find((p) => !p.receipt && (p.status === "queued" || p.status === "running"));
  if (busy) return { words: "Being prepared", tone: "active" };
  if (ready) return { words: `Prepared: ${ready.receipt!.table_rows ?? 0} rows`, tone: "done" };
  if (d.preparations.length) return { words: "Not prepared yet", tone: "attention" };
  return { words: "Read; ready to prepare", tone: "attention" };
}

/** The intake's state in words. */
export function intakeWords(d: ListedDataset | DatasetDetail): string {
  if (d.guard.outcome !== "allowed") return guardWords(d.guard).text;
  if (!d.intake) return d.intake_waiting ? "Waiting to be read: the lab's queue was full when it came in." : "Waiting to be read.";
  const listed = "receipt" in d.intake ? d.intake.receipt : d.intake.receipts.at(-1) ?? null;
  if (!listed) return d.intake.status === "running" ? "Being read." : "Waiting for a compute slot to be read.";
  if (listed.status === "profiled") return "Read.";
  return intakeFailure(listed.error_code);
}
export function intakeFailure(code: string | null): string {
  switch (code) {
    case "dataset_guard_unavailable": return "The checked copy could not be fetched from the upload guard. Send the file again.";
    case "dataset_guard_unconfigured": return "Lab data is not set up on this site yet.";
    case "dataset_grid_unreadable": return "The file could not be read as a table.";
    case "dataset_grid_too_large": return "This file is too large to read as one table: over 4,194,304 rows in all.";
    default: return "The file was not read. Nothing about the project changed.";
  }
}

/** Why an upload was refused, in words, and whether sending it again can help. */
export function uploadProblem(code: string, status = 0): { words: string; again: boolean; unavailable?: boolean } {
  switch (code) {
    case "dataset_type_unsupported": return { words: "This kind of file is not read here. Send a CSV, a tab-separated text file or an Excel workbook (.xlsx).", again: false };
    case "dataset_too_large": return { words: "This file is larger than the 32 MB a lab table can be.", again: false };
    case "dataset_empty": return { words: "This file is empty.", again: false };
    case "dataset_name_invalid": return { words: "The file's name cannot be used. Rename it and send it again.", again: false };
    case "dataset_incomplete": case "dataset_length_required": return { words: "The file did not arrive whole. Send it again.", again: true };
    case "idempotency_conflict": return { words: "This upload's request belongs to another file. Choose the file again to start a new upload.", again: false };
    case "dataset_method_unavailable": return { words: "Lab data is not set up for this lab yet.", again: false, unavailable: true };
    case "person_required": return { words: "Only a person can send lab data.", again: false };
    case "project_not_found": return { words: "Your account cannot add to this project.", again: false };
    case "dataset_upload_busy": case "dataset_guard_busy": return { words: "The upload guard is busy. Try again in a moment.", again: true };
    case "dataset_upload_quota": return { words: "You have sent as much lab data as a day allows. Try again tomorrow.", again: false };
    case "dataset_guard_unavailable": return { words: "The upload guard did not answer. Try again in a moment.", again: true };
    case "dataset_guard_unconfigured": return { words: "Lab data is not set up on this site yet.", again: false, unavailable: true };
    default: return { words: status === 0 ? "The file could not be sent. Check the connection and try again." : status === 403 ? "Your account cannot add to this project." : "The file was not taken. Nothing was changed.", again: status === 0 || status >= 500 };
  }
}

/** A file the picker gave that cannot be a lab table, before it is sent. */
export function fileProblem(file: File): string {
  const ext = file.name.split(".").pop()?.toLowerCase() ?? "";
  if (!file.name.includes(".") || !ACCEPT.split(",").includes(`.${ext}`)) return "This kind of file is not read here. Send a CSV, a tab-separated text file or an Excel workbook (.xlsx).";
  if (!file.size) return "This file is empty.";
  if (file.size > MAX_LAB_FILE) return "This file is larger than the 32 MB a lab table can be.";
  return "";
}

const n = (value: number) => value.toLocaleString("en-US");
const plural = (count: number, one: string, many: string) => `${n(count)} ${count === 1 ? one : many}`;
export function listWords(items: string[]): string {
  return items.length < 2 ? items.join("") : `${items.slice(0, -1).join(", ")} and ${items[items.length - 1]}`;
}
/** Rows as a sheet's reader says them: "row 13", "rows 13 and 14", "rows 5 to 12". */
export function rowsWords(rows: number[]): string {
  const ranges = rangesOf(rows);
  if (!ranges.length) return "";
  if (ranges.length === 1 && ranges[0][0] === ranges[0][1]) return `row ${n(ranges[0][0])}`;
  return `rows ${listWords(ranges.map(([a, b]) => (a === b ? n(a) : b === a + 1 ? `${n(a)} and ${n(b)}` : `${n(a)} to ${n(b)}`)))}`;
}
/** Columns as letters: "column F", "columns D to H", "columns F and I". */
export function lettersWords(letters: string[]): string {
  if (letters.length === 1) return `column ${letters[0]}`;
  const consecutive = letters.every((l, i) => !i || letterIndex(l) === letterIndex(letters[i - 1]) + 1);
  return consecutive && letters.length > 2 ? `columns ${letters[0]} to ${letters[letters.length - 1]}` : `columns ${listWords(letters)}`;
}
/** A cell's text as the page shows it: quoted, so spaces around it can be seen. */
export const cellText = (text: string) => `“${text}”`;

const TYPE_WORDS: Record<string, string> = {
  text: "text", category: "categories", number: "numbers", integer: "whole numbers", logical: "TRUE or FALSE", date: "dates",
  datetime: "dates and times", time: "times", duration: "durations", empty: "empty", mixed: "a mix of numbers and text",
};
export const typeWords = (type: string) => TYPE_WORDS[type] ?? type;

/** What a profiled column holds in its data rows, in a line. */
export function columnContents(c: ProfileColumn): string {
  const parts = [`${plural(c.present, "value", "values")}${c.type !== "empty" ? ` read as ${typeWords(c.type)}` : ""}`];
  const missing = Object.entries(c.missing);
  const empty = c.missing[""] ?? 0;
  const tokens = missing.filter(([k]) => k !== "");
  if (empty) parts.push(`${plural(empty, "empty cell", "empty cells")}`);
  if (tokens.length) parts.push(`${listWords(tokens.map(([k, v]) => `${n(v)} ${cellText(k)}`))} for missing`);
  const below = c.censored.below_limit ?? 0, above = c.censored.above_limit ?? 0, nd = c.censored.not_detected ?? 0;
  if (below) parts.push(`${n(below)} below a limit`);
  if (above) parts.push(`${n(above)} above a limit`);
  if (nd) parts.push(`${n(nd)} not detected`);
  if (c.other) parts.push(`${plural(c.other, "cell", "cells")} not ${typeWords(c.type)}`);
  if (c.distinct !== null && c.type === "text") parts.push(`${plural(c.distinct, "different text", "different texts")}`);
  return parts.join(" · ");
}

const UNREAD: Record<string, string> = {
  not_a_number: "is not a number",
  not_an_integer: "is not a whole number",
  not_a_date: "is not a date",
  not_a_datetime: "is not a date and time",
  not_a_time: "is not a time",
  not_a_logical: "is not TRUE or FALSE",
  censored_without_rule: "is reported against a limit, and the column has no rule for those",
  limit_unknown: "names no limit, and the column's rule needs one",
  spreadsheet_error: "is a spreadsheet error",
  date_out_of_range: "is a date out of range",
  text_reads_as_missing: "would be written as NA, which R reads as missing",
  point_in_comma_decimal: "has a point, but the column reads a comma as the decimal mark",
  comma_in_number: "has a comma; say how this column writes decimals",
};
/** A cell no rule read, in a sentence: where it is, what it says and why. */
export function unreadWords(u: Unread): string {
  return `Row ${n(u.row)}, column ${u.column}: ${cellText(u.text)} ${UNREAD[u.reason] ?? `could not be read (${u.reason.replaceAll("_", " ")})`}.`;
}

/** Why a preparation was not made, as its receipt's code says. */
export function preparationRefusal(code: string | null, summary: PreparationSummary | null): string {
  switch (code) {
    case "dataset_values_unread": {
      const count = summary?.failure_count ?? 0;
      return `Not prepared: ${plural(count, "cell", "cells")} no rule could read. Nothing was written.`;
    }
    case "dataset_key_repeated": return `Not prepared: ${plural(summary?.repeated_key_count ?? 0, "row repeats", "rows repeat")} another row's key. Nothing was written.`;
    case "dataset_no_rows": return "Not prepared: no row was left to write.";
    case "dataset_spec_outside_sheet": return "Not prepared: the choices name rows or columns the sheet does not have.";
    case "dataset_sheet_missing": return "Not prepared: the sheet is not in the file.";
    case "dataset_table_too_large": return "Not prepared: the table would pass the broker's limits (16 MB, a million rows or two million cells).";
    case "dataset_spec_changed": return "Not prepared: the choices changed while it waited. Prepare it again.";
    case "dataset_grid_unreadable": return "Not prepared: the stored copy of the sheet could not be read.";
    default: return "Not prepared. Nothing was written.";
  }
}

/** Every filled cell of the sheet in one line that adds up (executor version 2). */
export function cellsLine(cells: NonNullable<PreparationSummary["cells"]>): string {
  return `${plural(cells.filled, "filled cell", "filled cells")} in the sheet: ${n(cells.outside_rows)} outside the table, ${n(cells.in_dropped_rows)} in rows left out, `
    + `${n(cells.in_unread_columns)} in columns not read, ${n(cells.read)} read.`;
}
export function rowsLine(summary: PreparationSummary, spec: Spec | null): string {
  const r = summary.rows;
  const range = spec ? ` (${spec.rows.first} to ${spec.rows.last})` : "";
  const out = [`${plural(r.read, "row", "rows")} read${range}`];
  if (r.dropped) out.push(`${n(r.dropped)} left out`);
  if (r.blank) out.push(`${n(r.blank)} blank`);
  out.push(`${n(r.kept)} kept`);
  const per = spec?.long ? spec.long.columns.length : 1;
  return `${out.join(", ")}. ${plural(r.written, "row", "rows")} written${per > 1 && r.kept ? `, ${n(per)} for each kept row` : ""}.`;
}

const CENSOR_RULE: Record<string, string> = { missing: "written as missing", limit: "written as the limit", half_limit: "written as half the limit", limit_over_sqrt2: "written as the limit over √2" };
/** What a preparation changed in one column, as counts. Nothing changed that the counts do not show. */
export function columnChanges(c: ColumnCounts): string[] {
  const out: string[] = [`${plural(c.values, "value", "values")}`];
  const missing = Object.entries(c.missing);
  if (missing.length) out.push(`${n(missing.reduce((s, [, v]) => s + v, 0))} missing (${listWords(missing.map(([k, v]) => `${k === "" ? "empty" : cellText(k)} ${n(v)}`))})`);
  if (c.trimmed) out.push(`${n(c.trimmed)} trimmed of spaces`);
  const recoded = Object.entries(c.recoded);
  const recodedCount = recoded.reduce((s, [, v]) => s + v, 0) + (c.recoded_unlisted ?? 0);
  if (recodedCount) out.push(`${n(recodedCount)} recoded${recoded.length ? ` (${listWords(recoded.map(([k, v]) => `${k.split(" -> ").map(cellText).join(" to ")} ${n(v)}`))}${c.recoded_unlisted ? ` and ${n(c.recoded_unlisted)} more` : ""})` : ""}`);
  if (c.decimal_commas) out.push(`${n(c.decimal_commas)} decimal commas read as points`);
  if (c.thousands_separators) out.push(`${n(c.thousands_separators)} thousands separators removed`);
  if (c.percent_signs) out.push(`${n(c.percent_signs)} percent signs removed`);
  if (c.plus_signs) out.push(`${n(c.plus_signs)} plus signs read past`);
  if (c.dates_from_text) out.push(`${n(c.dates_from_text)} dates read from text`);
  const censored = Object.entries(c.censored);
  const rule = c.censored_rule ? CENSOR_RULE[c.censored_rule] ?? c.censored_rule : "";
  for (const [direction, count] of censored) {
    const what = direction === "below_limit" ? "below a limit" : direction === "above_limit" ? "above a limit" : "not detected";
    out.push(`${n(count)} ${what}${rule ? `, ${direction === "above_limit" && c.censored_rule !== "missing" ? "written as the limit" : rule}` : ""}`);
  }
  if (c.unread_as_missing) out.push(`${n(c.unread_as_missing)} unreadable cells written as missing`);
  return out;
}

/** The 400 a specification can meet, where it points: the place in the page's words. */
export function specRefusalWords(path: string, reason: string, spec: Spec): string {
  const column = path.match(/^columns\[(\d+)\]/);
  const where = path === "" ? "The preparation"
    : path === "table_name" ? "The table's name"
    : path.startsWith("name:") ? `The name ${path.slice(5)}`
    : column ? `Column ${spec.columns[Number(column[1])]?.from ?? Number(column[1]) + 1}${path.includes(".name") ? "'s name" : path.includes(".recode") ? "'s spellings" : path.includes(".censored") ? "'s rule for limits" : path.includes(".unit") ? "'s unit" : path.includes(".type") ? "'s type" : ""}`
    : path.startsWith("long.value") ? "The measure across columns"
    : path.startsWith("long") ? "The time points"
    : path.startsWith("key") ? "The key"
    : path.startsWith("drop_rows") || path === "rows" || path === "header_rows" || path === "units_row" ? "The rows"
    : path.startsWith("missing") ? "The missing words"
    : "The preparation";
  const why: Record<string, string> = {
    not_a_name: "is not a name R takes: lower-case letters, digits and underscores, starting with a letter",
    name_repeated: "is used for two columns",
    not_a_column: "names a column the table does not have",
    column_repeated: "reads one column twice",
    not_for_this_type: "does not fit the column's type",
    value_reads_as_missing: "would write a value R reads as missing",
    not_an_integer: "needs a whole number",
    not_a_limit: "needs a limit above zero",
    rows_out_of_order: "are out of order",
    too_many: "has too many entries",
    values_repeated: "repeat a point",
    values_mixed: "mix numbers and text",
    unknown_choice: "has a choice the broker does not know",
    not_a_unit: "is not a unit the broker takes",
    not_a_token: "has a word the broker does not take",
  };
  return `${where} ${why[reason] ?? `was refused (${reason.replaceAll("_", " ")})`}. Nothing was prepared.`;
}
