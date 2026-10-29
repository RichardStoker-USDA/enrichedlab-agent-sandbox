import Papa from "papaparse";
import type { FigureControls } from "./contracts.ts";
import { axisHasFiniteLayout, coordinatesHaveFiniteLayout, type CompanionMathPoint } from "./companion3d_math.ts";

// A companion is deliberately smaller than the general table reader. It draws every accepted
// row, so the limit protects the page from turning a retained table into an unbounded SVG/canvas
// workload. A table over either bound is refused in full; it is never sampled or thinned.
export const MAX_FIGURE_DATA_BYTES = 8 * 1024 * 1024;
export const MAX_FIGURE_DATA_ROWS = 50_000;
export const MAX_FIGURE_DATA_COLUMNS = 200;

export interface FigureInputFile {
  path: string;
  media_type?: string;
  size_bytes: number;
  sha256?: string;
}

export interface FigureDataSource {
  record: FigureDataRecord;
  /** Retained figure members this source belongs beside, when the record names them. */
  figure_paths?: string[];
  /** Retained output package holding those figures, when the record names it. */
  figure_package_sha256?: string | null;
  /** Resolves the exact package member through the caller's checksum-verified read path. */
  load(signal: AbortSignal): Promise<{ file: FigureInputFile; blob: Blob }>;
}

/** The current broker sends a FigureVersion directly. The nested fields keep this reader usable
 *  with retained render-request records too, without ever inferring a mapping from column names. */
export interface FigureDataRecord {
  feature?: string;
  chart?: string;
  chart_type?: string;
  controls?: FigureControls | Record<string, unknown>;
  mapping?: Record<string, unknown>;
  encoding?: Record<string, unknown>;
  labels?: Record<string, unknown>;
  units?: Record<string, unknown>;
  render?: FigureDataRecord | null;
  request?: FigureDataRecord | null;
}

export interface FigureDataChannel {
  column: string;
  index: number;
  label: string;
  unit: string | null;
}

export interface FigureDataEncoding {
  chart: "scatter" | "line";
  geometry: "points" | "surface";
  dimensions: 2 | 3;
  x: FigureDataChannel;
  y: FigureDataChannel;
  z: FigureDataChannel | null;
  group: FigureDataChannel | null;
  facet: FigureDataChannel | null;
  group_order: string[];
  facet_order: string[];
  group_colours: Record<string, string>;
  /** Palette name and exact stops declared by the retained render request, when present. */
  palette: string | null;
  palette_colours: string[];
}

export interface FigureDataColumn {
  name: string;
  type: "number" | "category";
  /** Category values in first-appearance order. Numeric columns have no category list. */
  categories: string[];
}

export interface FigureDataRow {
  /** Zero-based place among data rows in the retained file. */
  index: number;
  /** One-based row number shown to the reader (the header is not counted). */
  number: number;
  /** Every cell as stored. No display value replaces it. */
  cells: string[];
}

export interface FigureDataPoint {
  row: number;
  x: number;
  y: number;
  z: number | null;
  group: string | null;
  facet: string | null;
}

export interface FigureDataTable {
  file: FigureInputFile;
  columns: FigureDataColumn[];
  rows: FigureDataRow[];
  /** Rows with every mapped numeric coordinate present, in retained-file order. */
  points: FigureDataPoint[];
  encoding: FigureDataEncoding;
  omitted_coordinate_rows: number;
}

export type FigureDataRefusalCode =
  | "bytes"
  | "rows"
  | "unsupported_file"
  | "unreadable"
  | "invalid_table"
  | "mapping"
  | "mapped_column_missing"
  | "mapped_column_not_numeric"
  | "inexact_numeric"
  | "nonfinite_layout"
  | "no_drawable_rows";

export interface FigureDataRefusal {
  ok: false;
  code: FigureDataRefusalCode;
  message: string;
}

export type FigureDataResult = { ok: true; table: FigureDataTable } | FigureDataRefusal;

const refused = (code: FigureDataRefusalCode, message: string): FigureDataRefusal => ({ ok: false, code, message });
const object = (value: unknown): Record<string, unknown> | null =>
  value !== null && typeof value === "object" && !Array.isArray(value) ? value as Record<string, unknown> : null;
const text = (value: unknown): string | null => typeof value === "string" && value.trim() ? value : null;

function requestOf(record: FigureDataRecord): FigureDataRecord {
  return record.render ?? record.request ?? record;
}

function mapsOf(record: FigureDataRecord): { request: FigureDataRecord; mapping: Record<string, unknown>; controls: Record<string, unknown> } {
  const request = requestOf(record);
  const controls = object(request.controls) ?? object(record.controls) ?? {};
  const mapping = object(request.mapping) ?? object(request.encoding) ?? object(record.mapping) ?? object(record.encoding) ?? controls;
  return { request, mapping, controls };
}

function mapped(mapping: Record<string, unknown>, names: string[]): { key: string; column: string; detail: Record<string, unknown> | null } | null {
  for (const key of names) {
    const value = mapping[key];
    const detail = object(value);
    const column = text(value) ?? text(detail?.column) ?? text(detail?.field) ?? text(detail?.name);
    if (column) return { key, column, detail };
  }
  return null;
}

function dictionaries(record: FigureDataRecord, request: FigureDataRecord, name: "labels" | "units"): Record<string, unknown>[] {
  return [object(request[name]), object(record[name])].filter((value): value is Record<string, unknown> => !!value);
}

function channel(
  spec: { key: string; column: string; detail: Record<string, unknown> | null },
  columns: string[],
  record: FigureDataRecord,
  request: FigureDataRecord,
  controls: Record<string, unknown>,
  role: "x" | "y" | "z" | "group" | "facet",
): FigureDataChannel | null {
  const index = columns.indexOf(spec.column);
  if (index < 0) return null;
  const labelKeys = role === "group" ? ["legend_title", `${spec.key}_label`, `${role}_label`] : [`${spec.key}_label`, `${role}_label`];
  const label = text(spec.detail?.label)
    ?? dictionaries(record, request, "labels").map((values) => text(values[spec.key]) ?? text(values[role]) ?? text(values[spec.column])).find(Boolean)
    ?? labelKeys.map((key) => text(controls[key])).find(Boolean)
    ?? spec.column;
  const unit = text(spec.detail?.unit)
    ?? dictionaries(record, request, "units").map((values) => text(values[spec.key]) ?? text(values[role]) ?? text(values[spec.column])).find(Boolean)
    ?? text(controls[`${spec.key}_unit`]) ?? text(controls[`${spec.key}_units`])
    ?? text(controls[`${role}_unit`]) ?? text(controls[`${role}_units`]);
  return { column: spec.column, index, label: label!, unit: unit ?? null };
}

const missing = (value: string) => value.trim() === "" || /^(NA|N\/A|null)$/i.test(value.trim());
const NUMBER = /^[+-]?(?:\d+(?:\.\d*)?|\.\d+)(?:[eE][+-]?\d+)?$/;
const DECIMAL = /^([+-]?)(?:(\d+)(?:\.(\d*))?|\.(\d+))(?:[eE]([+-]?\d+))?$/;

function normalizedDecimal(value: string): string | null {
  const trimmed = value.trim();
  const match = DECIMAL.exec(trimmed);
  if (!match) return null;
  const negative = match[1] === "-";
  const fraction = match[3] ?? match[4] ?? "";
  let digits = `${match[2] ?? "0"}${fraction}`.replace(/^0+/, "");
  if (!digits) return `${negative ? "-" : "+"}:0:0`;
  let exponent = Number(match[5] ?? "0") - fraction.length;
  while (digits.endsWith("0")) { digits = digits.slice(0, -1); exponent++; }
  return `${negative ? "-" : "+"}:${digits}:${exponent}`;
}

function exactNumeric(value: string): number | null {
  const trimmed = value.trim();
  if (missing(trimmed) || !NUMBER.test(trimmed)) return null;
  const parsed = Number(trimmed);
  if (!Number.isFinite(parsed)) return null;
  return normalizedDecimal(trimmed) === normalizedDecimal(String(parsed)) ? parsed : null;
}

function ordered(values: string[]): string[] {
  const out: string[] = [];
  const seen = new Set<string>();
  for (const value of values) {
    if (seen.has(value)) continue;
    seen.add(value);
    out.push(value);
  }
  return out;
}

interface RawTable { columns: string[]; rows: string[][] }

function delimited(source: string, delimiter: string): RawTable {
  const parsed = Papa.parse<string[]>(source, { delimiter, header: false, dynamicTyping: false, skipEmptyLines: true });
  if (parsed.errors.length || !parsed.data.length) throw new Error("parse");
  const [rawColumns, ...rows] = parsed.data;
  const columns = rawColumns.map((value, index) => index === 0 ? value.replace(/^\ufeff/, "") : value);
  return { columns, rows };
}

type JsonNode =
  | { kind: "scalar"; value: string }
  | { kind: "array"; values: JsonNode[] }
  | { kind: "object"; entries: [string, JsonNode][] };

// JSON.parse turns a stored 1.2300 into 1.23. This small, bounded parser keeps a number's
// token while using JSON.parse only to decode strings, so hover and selection show its exact text.
class ExactJson {
  private at = 0;
  private readonly source: string;
  constructor(source: string) { this.source = source; }
  parse(): JsonNode {
    const value = this.value();
    this.space();
    if (this.at !== this.source.length) throw new Error("json_tail");
    return value;
  }
  private space() { while (/\s/.test(this.source[this.at] ?? "")) this.at++; }
  private value(): JsonNode {
    this.space();
    const c = this.source[this.at];
    if (c === '"') return { kind: "scalar", value: this.string() };
    if (c === "[") return this.array();
    if (c === "{") return this.object();
    for (const literal of ["true", "false", "null"] as const) {
      if (this.source.startsWith(literal, this.at)) { this.at += literal.length; return { kind: "scalar", value: literal }; }
    }
    const match = this.source.slice(this.at).match(/^-?(?:0|[1-9]\d*)(?:\.\d+)?(?:[eE][+-]?\d+)?/);
    if (!match) throw new Error("json_value");
    this.at += match[0].length;
    return { kind: "scalar", value: match[0] };
  }
  private string(): string {
    const start = this.at++;
    let escaped = false;
    while (this.at < this.source.length) {
      const c = this.source[this.at++];
      if (!escaped && c === '"') return JSON.parse(this.source.slice(start, this.at)) as string;
      if (!escaped && c === "\\") escaped = true;
      else escaped = false;
    }
    throw new Error("json_string");
  }
  private array(): JsonNode {
    this.at++;
    const values: JsonNode[] = [];
    this.space();
    if (this.source[this.at] === "]") { this.at++; return { kind: "array", values }; }
    while (true) {
      values.push(this.value());
      this.space();
      const c = this.source[this.at++];
      if (c === "]") return { kind: "array", values };
      if (c !== ",") throw new Error("json_array");
    }
  }
  private object(): JsonNode {
    this.at++;
    const entries: [string, JsonNode][] = [];
    const names = new Set<string>();
    this.space();
    if (this.source[this.at] === "}") { this.at++; return { kind: "object", entries }; }
    while (true) {
      this.space();
      if (this.source[this.at] !== '"') throw new Error("json_key");
      const name = this.string();
      if (names.has(name)) throw new Error("json_duplicate_key");
      names.add(name);
      this.space();
      if (this.source[this.at++] !== ":") throw new Error("json_colon");
      entries.push([name, this.value()]);
      this.space();
      const c = this.source[this.at++];
      if (c === "}") return { kind: "object", entries };
      if (c !== ",") throw new Error("json_object");
    }
  }
}

const scalar = (node: JsonNode): string => {
  if (node.kind !== "scalar") throw new Error("json_nested_cell");
  return node.value;
};

function objects(nodes: JsonNode[]): RawTable {
  if (!nodes.length || nodes.some((node) => node.kind !== "object")) throw new Error("json_rows");
  const first = nodes[0] as Extract<JsonNode, { kind: "object" }>;
  const columns = first.entries.map(([name]) => name);
  const rows = nodes.map((node) => {
    const values = new Map((node as Extract<JsonNode, { kind: "object" }>).entries);
    if (values.size !== columns.length || columns.some((name) => !values.has(name))) throw new Error("json_shape");
    return columns.map((name) => scalar(values.get(name)!));
  });
  return { columns, rows };
}

function jsonTable(source: string): RawTable {
  const root = new ExactJson(source.replace(/^\ufeff/, "")).parse();
  if (root.kind === "array") return objects(root.values);
  if (root.kind !== "object") throw new Error("json_root");
  const fields = new Map(root.entries);
  const data = fields.get("data");
  if (data?.kind === "array") return objects(data.values);
  const rowNodes = fields.get("rows"), columnNodes = fields.get("columns");
  if (rowNodes?.kind !== "array") throw new Error("json_rows");
  if (!columnNodes) return objects(rowNodes.values);
  if (columnNodes.kind !== "array") throw new Error("json_columns");
  const columns = columnNodes.values.map(scalar);
  const rows = rowNodes.values.map((row) => {
    if (row.kind === "object") {
      const values = new Map(row.entries);
      if (values.size !== columns.length || columns.some((name) => !values.has(name))) throw new Error("json_shape");
      return columns.map((name) => scalar(values.get(name)!));
    }
    if (row.kind !== "array" || row.values.length !== columns.length) throw new Error("json_shape");
    return row.values.map(scalar);
  });
  return { columns, rows };
}

function formatOf(file: FigureInputFile): "csv" | "tsv" | "json" | null {
  const type = (file.media_type ?? "").split(";")[0].trim().toLowerCase();
  const path = file.path.toLowerCase();
  if (type === "text/csv" || path.endsWith(".csv")) return "csv";
  if (type === "text/tab-separated-values" || /\.(tsv|tab)$/.test(path)) return "tsv";
  if (type === "application/json" || path.endsWith(".json")) return "json";
  return null;
}

function validate(raw: RawTable): FigureDataRefusal | null {
  if (!raw.columns.length || raw.columns.length > MAX_FIGURE_DATA_COLUMNS || raw.columns.some((name) => !name.trim())
    || new Set(raw.columns).size !== raw.columns.length || raw.rows.some((row) => row.length !== raw.columns.length))
    return refused("invalid_table", "The input is not one rectangular table with a unique name for every column, so nothing is drawn.");
  if (raw.rows.length > MAX_FIGURE_DATA_ROWS)
    return refused("rows", `The input has more than ${MAX_FIGURE_DATA_ROWS.toLocaleString()} rows, the interactive-view limit. Nothing is sampled or drawn.`);
  return null;
}

function makeEncoding(record: FigureDataRecord, raw: RawTable, kinds: FigureDataColumn[]): FigureDataEncoding | FigureDataRefusal {
  const { request, mapping, controls } = mapsOf(record);
  const xSpec = mapped(mapping, ["x"]), ySpec = mapped(mapping, ["y"]);
  if (!xSpec || !ySpec) return refused("mapping", "The render request does not map both x and y to input columns, so the companion does not guess them.");
  const zSpec = mapped(mapping, ["z", "height", "third"]);
  const groupSpec = mapped(mapping, ["colour", "color", "group"]);
  const facetSpec = mapped(mapping, ["facet", "panel"]);
  const x = channel(xSpec, raw.columns, record, request, controls, "x");
  const y = channel(ySpec, raw.columns, record, request, controls, "y");
  const z = zSpec ? channel(zSpec, raw.columns, record, request, controls, "z") : null;
  const group = groupSpec ? channel(groupSpec, raw.columns, record, request, controls, "group") : null;
  const facet = facetSpec ? channel(facetSpec, raw.columns, record, request, controls, "facet") : null;
  const missingColumns = [[xSpec, x], [ySpec, y], [zSpec, z], [groupSpec, group], [facetSpec, facet]]
    .filter(([spec, found]) => spec && !found).map(([spec]) => (spec as NonNullable<typeof xSpec>).column);
  if (missingColumns.length) return refused("mapped_column_missing", `The render request names ${missingColumns.join(", ")}, which this input does not have. Nothing is drawn.`);
  for (const axis of [x!, y!, z].filter((value): value is FigureDataChannel => !!value)) {
    if (kinds[axis.index].type !== "number")
      return refused("mapped_column_not_numeric", `${axis.column} is not one numeric input column as the render request requires. Nothing is drawn.`);
  }
  const values = (axis: FigureDataChannel | null) => axis ? raw.rows.map((row) => row[axis.index]) : [];
  const colours = object(controls.group_colours) ?? object(controls.group_colors) ?? {};
  const groupColours = Object.fromEntries(Object.entries(colours).filter((entry): entry is [string, string] => typeof entry[1] === "string"));
  const feature = text(request.feature) ?? text(request.chart_type) ?? text(request.chart) ?? text(record.feature) ?? text(record.chart_type) ?? text(record.chart) ?? "scatter";
  const featureName = feature.toLowerCase();
  const rawPalette = controls.palette;
  const rawStops = Array.isArray(rawPalette) ? rawPalette : controls.palette_colours ?? controls.palette_colors;
  const paletteColours = Array.isArray(rawStops)
    ? rawStops.filter((value): value is string => typeof value === "string" && /^#[0-9a-f]{6}$/i.test(value))
    : [];
  return {
    chart: featureName === "line" ? "line" : "scatter",
    geometry: featureName === "surface" && z ? "surface" : "points",
    dimensions: z ? 3 : 2,
    x: x!, y: y!, z, group, facet,
    group_order: ordered(values(group)), facet_order: ordered(values(facet)), group_colours: groupColours,
    palette: text(rawPalette) ?? null,
    palette_colours: paletteColours,
  };
}

/** Build the bounded companion table from already checksum-verified UTF-8 text. */
export function buildFigureData(record: FigureDataRecord, file: FigureInputFile, source: string): FigureDataResult {
  const actualBytes = new TextEncoder().encode(source).byteLength;
  if (file.size_bytes > MAX_FIGURE_DATA_BYTES || actualBytes > MAX_FIGURE_DATA_BYTES)
    return refused("bytes", `The input is larger than ${Math.floor(MAX_FIGURE_DATA_BYTES / (1024 * 1024))} MiB, the interactive-view limit. Nothing is sampled or drawn.`);
  const format = formatOf(file);
  if (!format) return refused("unsupported_file", "The companion reads retained CSV, tab-separated or JSON tables only. This input is left unchanged and nothing is drawn.");
  let raw: RawTable;
  try { raw = format === "json" ? jsonTable(source) : delimited(source, format === "tsv" ? "\t" : ","); }
  catch { return refused("unreadable", "The retained input could not be read as one table, so nothing is drawn."); }
  const problem = validate(raw);
  if (problem) return problem;
  const columns: FigureDataColumn[] = raw.columns.map((name, index) => {
    const values = raw.rows.map((row) => row[index]);
    const present = values.filter((value) => !missing(value));
    const isNumber = present.length > 0 && present.every((value) => NUMBER.test(value.trim()));
    return { name, type: isNumber ? "number" : "category", categories: isNumber ? [] : ordered(values) };
  });
  const encoding = makeEncoding(record, raw, columns);
  if ("ok" in encoding) return encoding;
  for (const axis of [encoding.x, encoding.y, encoding.z].filter((value): value is FigureDataChannel => !!value)) {
    if (raw.rows.some((row) => !missing(row[axis.index]) && exactNumeric(row[axis.index]) === null))
      return refused("inexact_numeric", `Exact placement not possible: the retained values in ${axis.column} do not round-trip to the same decimal in this browser, so nothing is drawn.`);
    const values = raw.rows
      .map((row) => row[axis.index])
      .filter((value) => !missing(value))
      .map((value) => exactNumeric(value)!);
    if (!axisHasFiniteLayout(values))
      return refused("nonfinite_layout", `Exact placement not possible: the retained range in ${axis.column} cannot be laid out with finite browser coordinates, so nothing is drawn.`);
  }
  const points: FigureDataPoint[] = [];
  let omitted = 0;
  raw.rows.forEach((row, index) => {
    const x = exactNumeric(row[encoding.x.index]), y = exactNumeric(row[encoding.y.index]);
    const z = encoding.z ? exactNumeric(row[encoding.z.index]) : null;
    if (x === null || y === null || (encoding.z && z === null)) { omitted++; return; }
    points.push({ row: index, x, y, z, group: encoding.group ? row[encoding.group.index] : null, facet: encoding.facet ? row[encoding.facet.index] : null });
  });
  if (!points.length) return refused("no_drawable_rows", "No input row has every numeric coordinate in the render request, so nothing is drawn.");
  if (encoding.z) {
    const layoutPoints: CompanionMathPoint[] = points.map((point) => ({
      row: point.row,
      x: point.x,
      y: point.y,
      z: point.z!,
      xText: raw.rows[point.row][encoding.x.index],
      yText: raw.rows[point.row][encoding.y.index],
      zText: raw.rows[point.row][encoding.z!.index],
      group: point.group ?? "",
    }));
    let layouts: CompanionMathPoint[][] = [layoutPoints];
    if (encoding.facet) {
      const byFacet = new Map<string, CompanionMathPoint[]>();
      for (let index = 0; index < layoutPoints.length; index++) {
        const key = points[index].facet ?? "";
        const facetPoints = byFacet.get(key) ?? [];
        facetPoints.push(layoutPoints[index]);
        byFacet.set(key, facetPoints);
      }
      layouts = [...byFacet.values()];
    }
    if (layouts.some((candidate) => !coordinatesHaveFiniteLayout(candidate, encoding.geometry === "surface")))
      return refused("nonfinite_layout", "Exact placement not possible: intermediate coordinates for this retained range are not finite, so nothing is drawn.");
  }
  return { ok: true, table: {
    file, columns, rows: raw.rows.map((cells, index) => ({ index, number: index + 1, cells })), points, encoding,
    omitted_coordinate_rows: omitted,
  } };
}

/** Decode verified bytes without replacement characters, enforce the byte bound, then build the table. */
export async function loadFigureData(record: FigureDataRecord, file: FigureInputFile, blob: Blob): Promise<FigureDataResult> {
  if (file.size_bytes > MAX_FIGURE_DATA_BYTES || blob.size > MAX_FIGURE_DATA_BYTES)
    return refused("bytes", `The input is larger than ${Math.floor(MAX_FIGURE_DATA_BYTES / (1024 * 1024))} MiB, the interactive-view limit. Nothing is sampled or drawn.`);
  try {
    const source = new TextDecoder("utf-8", { fatal: true }).decode(await blob.arrayBuffer());
    return buildFigureData(record, file, source);
  } catch {
    return refused("unreadable", "The retained input is not valid UTF-8 table text, so nothing is drawn.");
  }
}
