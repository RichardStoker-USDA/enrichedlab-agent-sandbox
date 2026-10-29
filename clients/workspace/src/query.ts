// A database query the research agent wrote, in plain words: what it read, what came back, and why an
// attempt did not finish. The broker's receipt codes are fixed; a code this page does not know reads as not finished.
import type { QueryReceipt, TaskQuery } from "./contracts.ts";

// Why a query did not finish, for the codes a scientist or the agent can act on.
export const queryStopWords: Record<string, string> = {
  query_column_unknown: "the database has no column the query named",
  query_relation_unknown: "the database has no table the query named",
  query_function_unknown: "the database has no function the query called",
  query_column_ambiguous: "the query named a column that two of its tables share without saying which",
  query_type_mismatch: "the query compared or combined values of different kinds",
  query_grouping_invalid: "the query mixed grouped and ungrouped columns",
  query_division_by_zero: "the query divided by zero",
  query_data_error: "the database could not read a value the query produced",
  query_invalid_for_source: "the database refused the query as written",
  query_timeout: "the query ran longer than the source allows",
  query_too_many_rows: "the query returned more rows than the source allows",
  query_result_too_large: "the query's result was larger than the source allows",
  query_refused_by_database: "the database refused the reader access to what the query asked for",
  source_unavailable: "the database could not be reached",
  query_connection_unavailable: "the database's connection is not set up for analyses",
  snapshot_mismatch: "the database no longer holds the snapshot the source names",
  snapshot_not_found: "the database no longer holds the snapshot the source names",
  snapshot_not_recorded: "the source does not name its snapshot",
  source_version_unreadable: "the lab database did not state one version for the query to read at",
  source_version_too_large: "the lab database's statement of its version was larger than the platform keeps",
  guard_mismatch: "the check before it ran did not admit the stored query",
  query_not_recorded: "no query was recorded for this analysis",
};

export function stopWords(code: string | null | undefined): string {
  if (!code) return "It did not finish.";
  if (code.startsWith("sql_refused")) return "The check before it ran refused the query.";
  const words = queryStopWords[code];
  return words ? `It did not finish: ${words}.` : "It did not finish.";
}

/** The receipt of the newest attempt, if any. */
export const newestReceipt = (q: TaskQuery): QueryReceipt | null => q.receipts.at(-1) ?? null;

/** One sentence on what came back, from the newest receipt. */
export function resultSentence(q: TaskQuery): string {
  const r = newestReceipt(q);
  if (!r) return "It has not run yet.";
  if (r.status !== "completed") return stopWords(r.error_code);
  const rows = r.row_count ?? 0;
  const columns = r.columns?.length ?? 0;
  return `It returned ${rows === 1 ? "one row" : `${rows} rows`} with ${columns === 1 ? "one column" : `${columns} columns`}.`;
}

/** A live lab database's statement of its version, as words: each field the database stated, in its order, at most six,
 *  each value cut to 60 characters. */
export function versionWords(version: Record<string, unknown> | null | undefined): string {
  if (!version) return "";
  return Object.entries(version).slice(0, 6).map(([key, value]) => {
    const text = typeof value === "string" ? value : JSON.stringify(value);
    return `${key.replaceAll("_", " ")} ${text.length > 60 ? `${text.slice(0, 59)}…` : text}`;
  }).join(", ");
}

/** True when the broker added the row cap to the stored proposed text; the two texts are otherwise identical. */
export const capAdded = (q: TaskQuery): boolean => q.admitted_sql !== q.proposed_sql.trim().replace(/;+$/, "").trim();
