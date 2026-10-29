import Papa from "papaparse";
import type { Artifact } from "./contracts.ts";

export const MAX_TEXT_PREVIEW = 2 * 1024 * 1024;
export const MAX_TABLE_ROWS = 1000;
export type ContentKind = "table" | "image" | "text" | "download";
export function contentKind(artifact: Artifact): ContentKind {
  const type = artifact.media_type.split(";")[0]?.trim().toLowerCase();
  const name = artifact.relative_path.toLowerCase();
  if (["image/png", "image/jpeg", "image/webp", "image/gif", "image/svg+xml"].includes(type)) return "image";
  if (["text/csv", "text/tab-separated-values"].includes(type) || /\.(csv|tsv)$/.test(name)) return "table";
  if (type.startsWith("text/") || ["application/json", "application/x-ndjson"].includes(type) || /\.(r|py|sql|json|jsonl|txt|log|md|yaml|yml)$/.test(name)) return "text";
  return "download";
}

export interface TablePreview {
  columns: string[];
  rows: string[][];
  truncated: boolean;
}

// Scientific values remain strings. Formatting, inference, and floating-point
// conversion never replace the bytes in the original downloadable artifact.
export function tablePreview(text: string, delimiter = ","): TablePreview {
  const parsed = Papa.parse<string[]>(text, {
    delimiter,
    header: false,
    dynamicTyping: false,
    skipEmptyLines: true,
    preview: MAX_TABLE_ROWS + 2,
  });
  if (parsed.errors.length) throw new Error("table_parse_failed");
  const [columns, ...rows] = parsed.data;
  if (!columns?.length || columns.some((column) => !column.trim()) || new Set(columns).size !== columns.length)
    throw new Error("table_header_invalid");
  if (columns.length > 200 || rows.some((row) => row.length !== columns.length))
    throw new Error("table_shape_invalid");
  return { columns, rows: rows.slice(0, MAX_TABLE_ROWS), truncated: rows.length > MAX_TABLE_ROWS || parsed.meta.truncated };
}

export async function exactText(blob: Blob): Promise<string> {
  if (blob.size > MAX_TEXT_PREVIEW) throw new Error("preview_too_large");
  return new TextDecoder("utf-8", { fatal: true }).decode(await blob.arrayBuffer());
}

export interface TableRead extends TablePreview {
  totalRows: number;
}

// Parse the whole verified file so the row count is exact; keep only the first rows for
// display. Values stay strings, exactly as written.
export function tableRead(text: string, delimiter = ","): TableRead {
  const parsed = Papa.parse<string[]>(text, { delimiter, header: false, dynamicTyping: false, skipEmptyLines: true });
  if (parsed.errors.length) throw new Error("table_parse_failed");
  const [columns, ...rows] = parsed.data;
  if (!columns?.length || columns.some((column) => !column.trim()) || new Set(columns).size !== columns.length)
    throw new Error("table_header_invalid");
  if (columns.length > 200 || rows.some((row) => row.length !== columns.length)) throw new Error("table_shape_invalid");
  return { columns, rows: rows.slice(0, MAX_TABLE_ROWS), truncated: rows.length > MAX_TABLE_ROWS, totalRows: rows.length };
}

/** The groups of one column, as stored, in the order they first appear: the values a figure's colours per group name.
 *  Empty and NA cells are no group. `more` says the column holds more groups than `limit`. */
export function groupsIn(text: string, column: string, delimiter = ",", limit = 12): { values: string[]; more: boolean } {
  const { columns, rows } = tableRead(text, delimiter);
  const at = columns.indexOf(column);
  if (at < 0) throw new Error("column_not_found");
  const seen: string[] = [];
  for (const row of rows) {
    const value = row[at];
    if (value === "" || value === "NA" || seen.includes(value)) continue;
    if (seen.length === limit) return { values: seen, more: true };
    seen.push(value);
  }
  return { values: seen, more: false };
}

// Decode verified bytes without replacement characters; a bad byte refuses the preview.
export async function strictText(blob: Blob, limit: number): Promise<string> {
  if (blob.size > limit) throw new Error("preview_too_large");
  return new TextDecoder("utf-8", { fatal: true }).decode(await blob.arrayBuffer());
}
