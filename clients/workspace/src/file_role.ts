// What each retained file is for, in one plain line. It is read from the file's path
// and reader kind by fixed rules, never from its contents; the path, type and checksum stay in the file record.
import type { PackageKind } from "./contracts.ts";

const NAMED: Record<string, string> = {
  "readme.md": "How to rerun this analysis on your own computer",
  "run.json": "The run's record: its method, settings and seed",
  "run_parameters.json": "The settings this analysis ran with",
  "admission.json": "What the code check allowed before the code ran",
  "boundary.json": "The limits the run was held to",
  "session_info.txt": "The R version and packages that ran",
  "environment.txt": "The R version and packages that ran",
  "method.txt": "The method in words, as the analysis wrote it",
  "stdout.log": "The run's log, for troubleshooting",
  "exit_code.txt": "How the run ended: 0 means without an error",
  "sha256sums": "Checksums that show these files are unchanged",
  "payload.sha256": "The checksum that shows the package is unchanged",
  "analyses.json": "Each analysis in this package, with its method and reviews",
  "verify.py": "Checks every file and can rerun each analysis",
  "rerun.sbatch": "Reruns the analyses on a computing cluster",
  "import_all.sas": "Opens the result tables in SAS",
  "open_all.jsl": "Opens the result tables in JMP",
  "template.r": "The reviewed recipe the code ran inside",
  "block.r": "The block of code that ran inside the recipe",
  "block.diff": "How that block differs from the recipe's own",
  "contract.dcf": "Which columns the code could read",
  "query.sql": "The database query that ran",
  "receipt.json": "What the query read and returned: snapshot, columns and checksums",
};

const KIND: Record<PackageKind, string> = {
  figure: "A figure",
  table: "A table",
  code: "Code that ran",
  log: "A log from the run, for troubleshooting",
  record: "A record of the run",
  text: "A text file",
  checksum: "Checksums that show these files are unchanged",
  data: "A data file",
};

/** The reader kind for a path, by the same rules as the broker's package listing. */
export function kindOf(path: string): PackageKind {
  const name = (path.split("/").at(-1) ?? path).toLowerCase();
  const extension = name.includes(".") ? name.slice(name.lastIndexOf(".") + 1) : "";
  if (["pdf", "svg", "png", "jpg", "jpeg", "webp", "gif"].includes(extension)) return "figure";
  if (["csv", "tsv", "tab"].includes(extension)) return "table";
  if (["r", "py", "sql", "sh"].includes(extension)) return "code";
  if (extension === "log") return "log";
  if (["json", "jsonl", "ndjson", "yaml", "yml", "toml"].includes(extension)) return "record";
  if (extension === "md" || extension === "txt") return "text";
  if (name === "sha256sums" || name.endsWith(".sha256")) return "checksum";
  return "data";
}

/** One plain line saying what a retained file is for. */
export function fileRole(path: string, kind: PackageKind = kindOf(path)): string {
  const parts = path.toLowerCase().split("/");
  const name = parts.at(-1) ?? "";
  if (NAMED[name]) return NAMED[name];
  const folders = parts.slice(0, -1);
  if (folders.includes("input") || folders.includes("inputs"))
    return kind === "table" ? "A copy of the input data, as the analysis read it" : "Part of the input, as the analysis read it";
  if (folders.includes("logs")) return "A log from the run, for troubleshooting";
  if (folders.includes("results")) {
    if (kind === "table") return "A results table";
    if (kind === "figure") return "A figure from the results";
    if (kind === "text") return "A note the analysis wrote with its results";
  }
  return KIND[kind] ?? KIND.data;
}
