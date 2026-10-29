import { createHash } from "node:crypto";
import { readdirSync, readFileSync } from "node:fs";
import { join, relative } from "node:path";
import { fileURLToPath } from "node:url";
import type { Artifact } from "../src/contracts.ts";
import { syntheticAtlasEvents } from "./atlas.ts";
import { syntheticPackage } from "./package.ts";

// Files rendered once by `fixtures/r/analysis.R` from `fixtures/r/simulate_inputs.R` output.
// Every value is simulated; nothing here is research data or derived from it.
const rendered = (path: string) => readFileSync(new URL(`./r/${path}`, import.meta.url));

const readme = `# Test diet response: analysis package (simulated preview data)

Every value in this package is simulated for the research workspace preview. It has no scientific meaning.

Contents: \`input/\` holds the exact input copies, \`code/analysis.R\` is the code that ran, \`results/\` holds
the tables and figures it wrote, and \`SHA256SUMS\` lists a checksum for every file.

To check the files: \`shasum -a 256 -c SHA256SUMS\` (macOS) or \`sha256sum -c SHA256SUMS\` (Linux).
To rerun in R or RStudio: \`Rscript code/analysis.R input rerun\`, then compare \`rerun/\` with \`results/\`.
The PDF files are the figure masters; the SVG files are screen copies of the same plots.
`;

// The retained package of the finished preview analysis.
export function fixturePackage(mtime: number) {
  const files = [
    { path: "README.md", bytes: Buffer.from(readme) },
    { path: "method.txt", bytes: rendered("results/method.txt") },
    { path: "run_parameters.json", bytes: Buffer.from(JSON.stringify({ method: "diet_response_review", seed: 20260923,
      groups: ["reference", "diet_a", "diet_b"], feature_contrast: "diet_a vs reference", q_threshold: 0.05,
      log2_fold_change_threshold: 1, simulated: true }, null, 2) + "\n") },
    { path: "environment.txt", bytes: rendered("results/environment.txt") },
    { path: "code/analysis.R", bytes: rendered("analysis.R") },
    { path: "code/simulate_inputs.R", bytes: rendered("simulate_inputs.R") },
    { path: "input/measurements.csv", bytes: rendered("input/measurements.csv") },
    { path: "input/metabolites.csv", bytes: rendered("input/metabolites.csv") },
    ...["condition_summary.csv", "condition_effects.csv", "metabolite_changes.csv", "response_by_diet.pdf", "response_by_diet.svg",
      "feature_volcano.pdf", "feature_volcano.svg", "model_checks.pdf"].map((name) => ({ path: `results/${name}`, bytes: rendered(`results/${name}`) })),
    { path: "stdout.log", bytes: rendered("results/stdout.log") },
    { path: "exit_code.txt", bytes: Buffer.from("0\n") },
  ];
  return syntheticPackage(files, mtime);
}

export function fixtureMaterials(taskId: string, at: (seconds: number) => string) {
  const files: { artifact: Artifact; bytes: Buffer }[] = [];
  const add = (name: string, label: string, media: string, bytes: Buffer, role = "workspace") => {
    const artifact: Artifact = { id: `30000000-0000-4000-8000-${String(files.length + 1).padStart(12, "0")}`, role, label,
      relative_path: `tasks/${taskId}/attempt-1/${name}`, media_type: media, size_bytes: bytes.length,
      sha256: createHash("sha256").update(bytes).digest("hex"), created_at: at(165) };
    files.push({ artifact, bytes }); return artifact;
  };
  const measurements = add("workspace/measurements.csv", "Source measurements", "text/csv", rendered("input/measurements.csv"));
  const table = add("workspace/condition_summary.csv", "Summary by diet", "text/csv", rendered("results/condition_summary.csv"));
  const figure = add("workspace/response_by_diet.svg", "Response by test diet", "image/svg+xml", rendered("results/response_by_diet.svg"));
  const method = add("workspace/analysis.R", "Analysis source", "text/x-r", rendered("analysis.R"));
  const log = add("workspace/analysis.log", "Analysis log", "text/plain", rendered("results/stdout.log"));
  const inspection = add("workspace/input_inspection.txt", "Input inspection", "text/plain", Buffer.from("Input inspection (simulated preview data)\nColumns: sample_id, condition, response\n36 records in 3 diets; no blank fields; unique sample identifiers.\nThese simulated observations have no scientific interpretation.\n"));
  const definitions = [
    { title: "Inspect the source measurements", tool: "inspect_input", input_sha256: measurements.sha256, output_sha256: inspection.sha256 },
    { title: "Summarize each diet", tool: "analyze", input_sha256: measurements.sha256, output_sha256: table.sha256, code_sha256: method.sha256, log_sha256: log.sha256 },
    { title: "Draw the response by diet", tool: "render_figure", input_sha256: table.sha256, output_sha256: figure.sha256, code_sha256: method.sha256, log_sha256: log.sha256 },
  ];
  const events = [
    ...definitions.flatMap(({ title, ...metadata }, index) => ["started", "finished"].map((state, offset) => ({
      sequence: index * 2 + offset + 1, event_id: `synthetic-event-${index}-${state}`, run_id: "synthetic-provenance-run",
      subject: "agent", segment: 1, harness: "native", provider: null, model: null, engine: "fixture", parser: "native-v1", session_id: "synthetic-session",
      kind: `agent.tool.${state}`, evidence_role: "tool-io", visibility: "metadata", actor: "Synthetic research agent",
      occurred_at: at(30 + index * 40 + offset * 15), source_event_id: `synthetic-call-${index}`, parent_source_event_id: null,
      metadata: { ...metadata, label: title, exit_code: state === "finished" ? 0 : undefined, synthetic: true },
      restricted_content: null, content_sha256: null, previous_hash: null, hash: "synthetic-unsealed-event",
    }))),
    ...syntheticAtlasEvents(237, { startSequence: 7, startIndex: 3, runId: "synthetic-provenance-run" }),
  ];
  add("evidence/provenance-export/events.jsonl", "Captured tool steps", "application/x-ndjson", Buffer.from(events.map((event) => JSON.stringify(event)).join("\n") + "\n"), "evidence");
  return files;
}

// The real output of the reviewed template on the simulated measurements (see
// fixtures/r/generated/README.md), and the broker's admitted record for its block.
const generatedRoot = new URL("./r/generated/package/", import.meta.url);
export function fixtureGeneratedPackage(mtime: number) {
  const root = fileURLToPath(generatedRoot);
  const files = readdirSync(root, { recursive: true, withFileTypes: true })
    .filter((entry) => entry.isFile())
    .map((entry) => {
      const full = join(entry.parentPath, entry.name);
      return { path: relative(root, full), bytes: readFileSync(full) };
    })
    .sort((a, b) => a.path.localeCompare(b.path));
  return syntheticPackage(files, mtime);
}
/** The configured-method record the broker stores with a rendered block. */
export const fixtureConfiguration = {
  schema: "enrichedlab.configured-block.v1", block_sha256: `sha256:${"d".repeat(64)}`,
  settings: { groups: "diet", level: "95", resamples: "10000", statistic: "mean" },
  described: [
    { setting: "groups", label: "Groups", choice: "diet", choice_label: "each diet" },
    { setting: "level", label: "Interval level", choice: "95", choice_label: "95%" },
    { setting: "resamples", label: "Resamples per group", choice: "10000", choice_label: "10,000" },
    { setting: "statistic", label: "Statistic", choice: "mean", choice_label: "mean" },
  ],
};
export function fixtureGeneratedCode(taskId: string, runId: string, createdAt: string, configured = false) {
  const content = readFileSync(new URL("code/block.R", generatedRoot), "utf8");
  const run = JSON.parse(readFileSync(new URL("run.json", generatedRoot), "utf8"));
  const admission = JSON.parse(readFileSync(new URL("../admission.json", generatedRoot), "utf8"));
  return { task_id: taskId, profile: configured ? "configured_r" : "generated_r", template_ref: run.template_ref, template_sha256: run.template_sha256,
    path: "generated/block.R", content, size_bytes: Buffer.byteLength(content), sha256: `sha256:${createHash("sha256").update(content).digest("hex")}`,
    admission, ...(configured ? { configuration: fixtureConfiguration } : {}), run_id: runId, turn: 2, created_at: createdAt,
    validation: "unvalidated" };
}

// A database query's package for the preview's query mode: an invented reference table read by an
// invented query. The layout is the worker's: the table under results/, the query, its receipt and a checksum list.
const QUERY_SQL = "SELECT f.description AS food, fn.amount AS amount_per_100_g\nFROM food f\nJOIN food_nutrient fn ON fn.fdc_id = f.fdc_id\nJOIN nutrient n ON n.id = fn.nutrient_id\nWHERE n.name = 'Example nutrient'\nORDER BY fn.amount DESC, f.description\nLIMIT 3";
export function fixtureQueryPackage(mtime: number) {
  const table = Buffer.from("food,amount_per_100_g\nExample food A,48.2\nExample food B,31.7\nExample food C,12.9\n");
  const tableSha = createHash("sha256").update(table).digest("hex");
  const files = [
    { path: "README.md", bytes: Buffer.from("# Database query\n\nAn invented query of invented reference data, for the workspace preview.\n") },
    { path: "method/query.sql", bytes: Buffer.from(`${QUERY_SQL}\n`) },
    { path: "receipt.json", bytes: Buffer.from(JSON.stringify({ schema: "enrichedlab.query-receipt.v1", source: "example-reference",
      release: "Example release", row_count: 3, table_sha256: tableSha, transaction: "repeatable read, read only" }, null, 2)) },
    { path: "results/query_result.csv", bytes: table },
    { path: "run_parameters.json", bytes: Buffer.from(JSON.stringify({ method: "guarded read-only database query", source: "Example reference data",
      release: "Example release", row_limit: 3, guard: "guarded-sql/v1" }, null, 2)) },
  ];
  return syntheticPackage(files, mtime);
}
/** The broker's record of that query, as `GET /api/v1/tasks/{task}/query` returns it. */
export function fixtureQuery(taskId: string, runId: string, createdAt: string, packageSha: string) {
  const sha = `sha256:${createHash("sha256").update(QUERY_SQL).digest("hex")}`;
  return { task_id: taskId, profile: "reference-sql-example", source: "example-reference", title: "Example reference data",
    description: "Invented foods with an invented nutrient's amount per 100 g, for the workspace preview.", release: "Example release",
    snapshot: { manifest_sha256: "d".repeat(64) }, proposed_sql: QUERY_SQL, admitted_sql: QUERY_SQL, sql_sha256: sha, guard: "guarded-sql/v1",
    relations: ["food", "food_nutrient", "nutrient"], row_limit: 3, run_id: runId, turn: 2, created_at: createdAt, validation: "unvalidated",
    receipts: [{ attempt: 1, status: "completed", error_code: null, row_count: 3, columns: [{ name: "food", type: "text" }, { name: "amount_per_100_g", type: "float8" }],
      package_sha256: packageSha, server_version: "18.1", started_at: createdAt, finished_at: createdAt }] };
}
