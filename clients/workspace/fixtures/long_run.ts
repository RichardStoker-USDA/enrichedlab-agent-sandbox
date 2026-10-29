import { createHash, randomUUID } from "node:crypto";
import { readFileSync } from "node:fs";
import type { IncomingMessage, ServerResponse } from "node:http";
import type { Artifact, TaskReview } from "../src/contracts.ts";
import type { FigureSource } from "./figures.ts";
import { GENERIC_SCRIPT, residualScript, serveLog } from "./log_stream.ts";
import { fixtureDigest, listing, syntheticPackage, type MemberResult } from "./package.ts";
import type { ProjectTask } from "./projects.ts";
import { answerTable } from "./tables.ts";

// A long run for the loopback preview (Glass Box, "Long runs"): the agent has worked through most of a thirty-step plan,
// the project holds more analyses than one project read returns, and its packages hold a few hundred files. Every
// reading is simulated by the seeded generator below, so a reset writes the same bytes. The comparison step also keeps
// a block-by-hour table with R's figure of it (`r/season/`), declared as views of one result: values, a
// 3D surface and the R figure. Names and values are invented.

type Json = (value: unknown, status?: number) => void;
const sha = (bytes: Buffer) => createHash("sha256").update(bytes).digest("hex");
const pad = (n: number) => String(n).padStart(2, "0");
const uuid = /^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/;

// mulberry32: a small seeded generator; the simulated readings depend only on the block number.
function generator(seed: number) {
  let a = seed >>> 0;
  return () => {
    a = (a + 0x6d2b79f5) >>> 0;
    let t = a;
    t = Math.imul(t ^ (t >>> 15), t | 1);
    t ^= t + Math.imul(t ^ (t >>> 7), t | 61);
    return ((t ^ (t >>> 14)) >>> 0) / 4294967296;
  };
}

const BLOCKS = 24, FAILED_BLOCK = 19, UNCERTAIN_BLOCK = 15, RETRIED_BLOCK = 7;
const DAYS = ["2026-07-14", "2026-07-15", "2026-07-16"];
type Reading = { time: string; hour: number; sensor: string; canopy: number; air: number };

// Three days of hourly readings from a block's two canopy loggers. Block 19's second logger lost most of a day.
function readings(block: number): Reading[] {
  const random = generator(1000 + block);
  const offset = (random() - 0.5) * 3;
  const out: Reading[] = [];
  DAYS.forEach((day, d) => {
    for (let hour = 0; hour < 24; hour++) for (const sensor of ["a", "b"]) {
      const air = 19 + 8.5 * Math.sin((2 * Math.PI * (hour - 9)) / 24) + (random() - 0.5) * 1.2;
      const sun = Math.max(0, Math.sin((Math.PI * (hour - 6)) / 14));
      const canopy = air + offset + 2.6 * sun + (random() - 0.5) * 0.8;
      if (block === FAILED_BLOCK && d === 1 && sensor === "b" && hour >= 2) continue;
      out.push({ time: `${day}T${pad(hour)}:00:00`, hour, sensor, canopy: Number(canopy.toFixed(2)), air: Number(air.toFixed(2)) });
    }
  });
  return out;
}
const csv = (header: string[], rows: (string | number)[][]) => Buffer.from([header.join(","), ...rows.map((r) => r.join(","))].join("\n") + "\n");
const mean = (xs: number[]) => xs.reduce((a, b) => a + b, 0) / xs.length;

function hourly(list: Reading[]) {
  return Array.from({ length: 24 }, (_, hour) => {
    const at = list.filter((r) => r.hour === hour);
    const canopy = mean(at.map((r) => r.canopy)), air = mean(at.map((r) => r.air));
    return { hour, canopy, air, difference: canopy - air, n: at.length };
  });
}
/** The comparison step's block-by-hour table: each summarized block's hourly canopy-to-air difference as its hourly
 *  table writes it. R draws the season figure from these bytes (`r/season/`), and a test holds the two together. */
export function seasonDifferenceCsv(): Buffer {
  const rows: (string | number)[][] = [];
  for (let block = 1; block <= BLOCKS; block++) {
    if (block === FAILED_BLOCK) continue;
    for (const h of hourly(readings(block))) rows.push([block, h.hour, h.difference.toFixed(3)]);
  }
  return csv(["block", "hour", "difference_c"], rows);
}
function summaryOf(list: Reading[]) {
  const hours = hourly(list);
  const afternoon = hours.filter((h) => h.hour >= 12 && h.hour <= 17);
  const top = hours.reduce((a, b) => (b.difference > a.difference ? b : a));
  return { readings: list.length, missing: DAYS.length * 24 * 2 - list.length, afternoon: mean(afternoon.map((h) => h.difference)),
    max: top.difference, hourOfMax: top.hour };
}

const SUMMARIZE_R = `# Summarize one block's canopy and air temperature by hour of day (reviewed block summary, simulated preview).
args <- commandArgs(trailingOnly = TRUE)
readings <- read.csv(args[[1]])
readings$hour <- as.integer(substr(readings$time, 12, 13))
hourly <- aggregate(cbind(canopy_temp_c, air_temp_c) ~ hour, data = readings, FUN = mean)
hourly$difference_c <- hourly$canopy_temp_c - hourly$air_temp_c
hourly$n <- as.vector(table(readings$hour))
afternoon <- hourly[hourly$hour >= 12 & hourly$hour <= 17, ]
summary <- data.frame(statistic = c("readings", "missing_readings", "afternoon_difference_c", "max_difference_c", "hour_of_max"),
  value = c(nrow(readings), 144 - nrow(readings), mean(afternoon$difference_c), max(hourly$difference_c),
    hourly$hour[which.max(hourly$difference_c)]))
write.csv(hourly, args[[2]], row.names = FALSE)
write.csv(summary, args[[3]], row.names = FALSE)
`;
const GAPS_R = `# Count each logger's readings against the expected hourly record (reviewed gap check, simulated preview).
index <- read.csv("input/logger_index.csv")
index$missing <- index$expected - index$recorded
write.csv(index, "results/gaps.csv", row.names = FALSE)
`;
const MODEL_R = `# Afternoon canopy-to-air difference by block, with each day's block mean as one observation (simulated preview).
days <- read.csv("input/block_days.csv")
fit <- lm(afternoon_difference_c ~ 0 + factor(block), data = days)
coefficients <- data.frame(term = names(coef(fit)), estimate = unname(coef(fit)))
write.csv(coefficients, "results/model_coefficients.csv", row.names = FALSE)
write.csv(data.frame(statistic = c("blocks", "observations", "residual_sd_c"), value = c(nlevels(factor(days$block)), nrow(days), sigma(fit))),
  "results/model_fit.csv", row.names = FALSE)
`;
const COMPARE_R = `# Compare the blocks' afternoon canopy-to-air differences (reviewed comparison, simulated preview).
files <- list.files("input", pattern = "_summary\\\\.csv$", full.names = TRUE)
rows <- lapply(files, function(f) { s <- read.csv(f); data.frame(block = as.integer(sub(".*block_(\\\\d+)_summary.*", "\\\\1", f)),
  afternoon_difference_c = s$value[s$statistic == "afternoon_difference_c"], max_difference_c = s$value[s$statistic == "max_difference_c"]) })
comparison <- do.call(rbind, rows)
comparison$rank <- rank(-comparison$afternoon_difference_c, ties.method = "first")
write.csv(comparison[order(comparison$block), ], "results/block_comparison.csv", row.names = FALSE)
`;

type Pkg = ReturnType<typeof syntheticPackage>;
type Step = { action_key: string; title: string; rationale: string };
const STEPS: Step[] = [
  { action_key: "check-gaps", title: "Check the logger records for gaps", rationale: "A block with missing hours would bias its afternoon mean, so the gaps are counted first." },
  ...Array.from({ length: BLOCKS }, (_, i) => ({ action_key: `summarize-block-${pad(i + 1)}`, title: `Summarize block ${i + 1}`,
    rationale: "The reviewed block summary gives hourly canopy and air means for one block, with its code and tables kept." })),
  { action_key: "compare-blocks", title: "Compare the blocks", rationale: "The block summaries are compared on their afternoon difference, with any missing block named." },
  { action_key: "fit-afternoon-model", title: "Fit the afternoon temperature model", rationale: "A model of the afternoon difference by block separates the block effect from day-to-day weather." },
  { action_key: "check-residuals", title: "Check the model's residuals", rationale: "The residuals show whether one block or one day drives the fit." },
  { action_key: "draw-season-figure", title: "Draw the season figure", rationale: "One R figure of the difference by block and hour, for the season report." },
  { action_key: "write-season-summary", title: "Write the season summary", rationale: "A short summary of what the blocks show, with each value bound to its table." },
];
// The steps that have started, by plan position (1-based); the browser suite can let the model finish and the next
// step start while a page is open (advance below).
const RAN = 27;
const COMPARE_STEP = 26;
// The residual check, which gets its compute slot while a page follows the run and writes its output live.
const RESIDUAL_STEP = 28;
// R's season figure and its script, as drawn from `seasonDifferenceCsv()` (see `r/season/README.md`).
const season = (path: string) => readFileSync(new URL(`./r/season/${path}`, import.meta.url));
// The comparison method's declared outputs: the block-by-hour table and R's figure hold the same numbers, so they are
// one result with three views. It is also the figure-companion fixture: three numeric columns, with the exact mapping,
// labels and units the R render request retained. The surface draws hour across, block deep and the difference as height.
const SEASON = "season-difference";
const seasonResults: Record<string, MemberResult> = {
  "results/season_difference.csv": { result_kind: { id: "table.delimited", version: 1 }, same_values_group: SEASON,
    views: [{ view: "values" }, { view: "surface", encoding: { x: "hour", y: "block", z: "difference_c", palette: "viridis",
      x_label: "Hour of day", x_unit: "hour", y_label: "Field block", z_label: "Canopy minus air", z_unit: "°C" } }] },
  "results/season_difference.pdf": { result_kind: { id: "figure.r", version: 1 }, same_values_group: SEASON, views: [{ view: "figure" }] },
  "results/season_difference.svg": { result_kind: { id: "figure.r", version: 1 }, same_values_group: SEASON, views: [{ view: "figure" }] },
};

export function longRunFixtures(options: { me: string }) {
  const { me } = options;
  const t0 = Date.now() - 260 * 60_000;
  const at = (minutes: number) => new Date(t0 + minutes * 60_000).toISOString();
  const projectId = "50000000-0000-4000-8000-000000000007";
  const runId = "60000000-0000-4000-8000-000000000007";
  const taskId = (step: number) => `11000000-0000-4000-8000-0000000000${pad(step)}`;
  const packageId = (step: number) => `32000000-0000-4000-8000-0000000000${pad(step)}`;
  const checksumId = (step: number) => `33000000-0000-4000-8000-0000000000${pad(step)}`;
  const blockOf = (step: number) => (step >= 2 && step <= BLOCKS + 1 ? step - 1 : 0);

  // When each step's analysis was asked for, started and ended, in minutes from the first save: the blocks ran in
  // three batches of eight, four at a time; the model has been running for about twenty minutes.
  // The minute the model finished and the residual check was asked for, and the minute a compute slot took it, once the
  // suite lets the run go on.
  let advancedAt: number | null = null, slotAt: number | null = null;
  // When the residual check's output began: its slot, or a preview control's restart for the browser suite.
  let logStart: number | null = null;
  const timing = (step: number): { submitted: number; started: number | null; finished: number | null } => {
    if (step === 1) return { submitted: 5, started: 6, finished: 12 };
    const block = blockOf(step);
    if (block) {
      const batch = Math.floor((block - 1) / 8), within = (block - 1) % 8;
      const start = 20 + batch * 60 + Math.floor(within / 4) * 15 + (within % 4);
      return { submitted: 20 + batch * 60, started: start, finished: start + 11 + ((block * 7) % 3) + (block === RETRIED_BLOCK ? 9 : 0) };
    }
    if (step === 26) return { submitted: 215, started: 216, finished: 232 };
    if (step === 27) return { submitted: 240, started: 241, finished: advancedAt };
    return { submitted: advancedAt ?? 0, started: slotAt, finished: null };
  };
  const statusOf = (step: number) => {
    const t = timing(step);
    return t.started === null ? "queued" : t.finished === null ? "running" : blockOf(step) === FAILED_BLOCK ? "failed" : "completed";
  };

  // Each finished step's package, built once from the simulated readings.
  const blockReadings = new Map(Array.from({ length: BLOCKS }, (_, i) => [i + 1, readings(i + 1)]));
  const common = (title: string, method: string, log: string[], parameters: Record<string, unknown>) => [
    { path: "README.md", bytes: Buffer.from(`# ${title}\n\nSimulated package for the workspace preview. Rerun with the command in the run log.\n`) },
    { path: "method.txt", bytes: Buffer.from(`${method}\n`) },
    { path: "run_parameters.json", bytes: Buffer.from(JSON.stringify(parameters, null, 2) + "\n") },
    { path: "environment.txt", bytes: Buffer.from("Simulated environment record for the preview.\nR version 4.5.1\nPackages: base, stats, utils\n") },
    { path: "stdout.log", bytes: Buffer.from(log.join("\n") + "\n") },
    { path: "exit_code.txt", bytes: Buffer.from("0\n") },
  ];
  const packages = new Map<number, Pkg>();
  const mtime = Math.floor(t0 / 1000);
  for (let step = 1; step <= 26; step++) {
    const block = blockOf(step);
    if (block === FAILED_BLOCK) continue;
    let files: { path: string; bytes: Buffer }[];
    if (step === 1) {
      const index = [...blockReadings.entries()].flatMap(([b, list]) => ["a", "b"].map((s) => [b, s, DAYS.length * 24, list.filter((r) => r.sensor === s).length]));
      files = [
        { path: "input/logger_index.csv", bytes: csv(["block", "sensor", "expected", "recorded"], index) },
        { path: "results/gaps.csv", bytes: csv(["block", "sensor", "expected", "recorded", "missing"], index.map((r) => [...r, Number(r[2]) - Number(r[3])])) },
        { path: "code/check_gaps.R", bytes: Buffer.from(GAPS_R) },
        ...common("Check the logger records for gaps", "Counts each logger's hourly readings against the 72 expected over three days.",
          ["Rscript code/check_gaps.R", `Read ${index.length} logger records from input/logger_index.csv`, "Wrote results/gaps.csv"], { expected_per_logger: 72 }),
      ];
    } else if (block) {
      const list = blockReadings.get(block)!;
      const s = summaryOf(list);
      const b = pad(block);
      files = [
        { path: `input/canopy_block_${b}.csv`, bytes: csv(["time", "sensor", "canopy_temp_c", "air_temp_c"], list.map((r) => [r.time, r.sensor, r.canopy.toFixed(2), r.air.toFixed(2)])) },
        { path: `results/block_${b}_hourly.csv`, bytes: csv(["hour", "canopy_temp_c", "air_temp_c", "difference_c", "n"],
          hourly(list).map((h) => [h.hour, h.canopy.toFixed(3), h.air.toFixed(3), h.difference.toFixed(3), h.n])) },
        { path: `results/block_${b}_summary.csv`, bytes: csv(["statistic", "value"], [["readings", s.readings], ["missing_readings", s.missing],
          ["afternoon_difference_c", s.afternoon.toFixed(3)], ["max_difference_c", s.max.toFixed(3)], ["hour_of_max", s.hourOfMax]]) },
        { path: "code/summarize_block.R", bytes: Buffer.from(SUMMARIZE_R) },
        ...common(`Summarize block ${block}`, "Hourly means of canopy and air temperature for one block, and their difference; the afternoon is noon to 5 PM.",
          [`Rscript code/summarize_block.R input/canopy_block_${b}.csv results/block_${b}_hourly.csv results/block_${b}_summary.csv`,
            `Read ${s.readings} readings (${s.missing} missing)`, `Wrote results/block_${b}_hourly.csv (24 rows)`, `Wrote results/block_${b}_summary.csv (5 rows)`],
          { block, input: `input/canopy_block_${b}.csv`, afternoon_hours: [12, 17] }),
      ];
    } else {
      const compared = [...blockReadings.entries()].filter(([b]) => b !== FAILED_BLOCK).map(([b, list]) => ({ block: b, ...summaryOf(list) }));
      const ranked = [...compared].sort((x, y) => y.afternoon - x.afternoon).map((c) => c.block);
      const field = mean(compared.map((c) => c.afternoon));
      const sd = Math.sqrt(mean(compared.map((c) => (c.afternoon - field) ** 2)) * compared.length / (compared.length - 1));
      files = [
        ...compared.map((c) => ({ path: `input/block_${pad(c.block)}_summary.csv`, bytes: packages.get(c.block + 1)!.files.find((f) => f.path.endsWith(`block_${pad(c.block)}_summary.csv`))!.bytes })),
        { path: "results/block_comparison.csv", bytes: csv(["block", "afternoon_difference_c", "max_difference_c", "rank"],
          compared.map((c) => [c.block, c.afternoon.toFixed(3), c.max.toFixed(3), ranked.indexOf(c.block) + 1])) },
        { path: "results/field_summary.csv", bytes: csv(["statistic", "value"], [["blocks_compared", compared.length], ["blocks_missing", FAILED_BLOCK],
          ["field_afternoon_difference_c", field.toFixed(3)], ["between_block_sd_c", sd.toFixed(3)]]) },
        { path: "code/compare_blocks.R", bytes: Buffer.from(COMPARE_R) },
        { path: "results/season_difference.csv", bytes: seasonDifferenceCsv() },
        { path: "results/season_difference.pdf", bytes: season("results/season_difference.pdf") },
        { path: "results/season_difference.svg", bytes: season("results/season_difference.svg") },
        { path: "code/draw_season_difference.R", bytes: season("draw_season_difference.R") },
        ...common("Compare the blocks", "Compares the blocks' afternoon canopy-to-air differences; a block without a summary is named, not filled in.",
          ["Rscript code/compare_blocks.R", `Read ${compared.length} block summaries`, `Block ${FAILED_BLOCK} has no summary and is left out`,
            "Wrote results/block_comparison.csv", "Wrote results/field_summary.csv",
            "Rscript code/draw_season_difference.R results/season_difference.csv results", "Wrote results/season_difference.pdf and results/season_difference.svg"],
          { blocks: compared.map((c) => c.block) }),
      ];
    }
    packages.set(step, syntheticPackage(files, mtime + step * 60));
  }
  const artifactsOf = (step: number): Artifact[] => {
    const pkg = packages.get(step);
    if (!pkg) return [];
    const done = at(timing(step).finished ?? 0);
    return [{ id: packageId(step), role: "result", label: "Analysis and reproducibility package", relative_path: `tasks/${taskId(step)}/payload`,
      media_type: "application/gzip", size_bytes: pkg.archive.length, sha256: pkg.sha256, created_at: done },
    { id: checksumId(step), role: "result", label: "Package checksum", relative_path: `tasks/${taskId(step)}/payload.sha256`,
      media_type: "text/plain", size_bytes: pkg.checksum.length, sha256: sha(pkg.checksum), created_at: done }];
  };

  const taskRecord = (step: number) => {
    const t = timing(step), status = statusOf(step);
    return { step, task_id: taskId(step), action_key: STEPS[step - 1].action_key, parent_task_id: null, profile: "evidence_review", status,
      attempt: blockOf(step) === RETRIED_BLOCK ? 2 : 1, submitted_at: at(t.submitted), updated_at: at(t.finished ?? t.started ?? t.submitted),
      started_at: t.started === null ? null : at(t.started), finished_at: t.finished === null ? null : at(t.finished),
      // The broker records a queued analysis's place (0361eda): here one analysis from another project was submitted
      // first and the lab's slots are taken. Any other analysis has none.
      queue: status === "queued" ? { ahead: 1, lab_full: true, person_full: false } : null };
  };
  const tasks = Array.from({ length: RAN }, (_, i) => taskRecord(i + 1));
  const listed = (t: (typeof tasks)[number]) => (({ step: _s, started_at: _a, finished_at: _b, ...rest }) => rest)(t);

  // The plan as each saved version recorded it: the gap check, then the blocks in three batches, then the comparison and
  // the model. Block 15's files could not be confirmed and block 19 did not finish.
  const statusAt = (revision: number, step: number) => {
    const block = blockOf(step);
    const batch = block ? Math.floor((block - 1) / 8) : -1;
    const startedAt = step === 1 ? 2 : block ? 3 + batch : step === 26 ? 6 : step === 27 ? 7 : step === 28 && advancedAt !== null ? 8 : Infinity;
    if (revision < startedAt) return { status: "pending", task: false };
    if (revision === startedAt) return { status: "active", task: true };
    if (block === FAILED_BLOCK) return { status: "failed", task: true };
    if (block === UNCERTAIN_BLOCK) return { status: "uncertain", task: true };
    return { status: "completed", task: true };
  };
  const goal = "How much warmer than the air does the canopy run on summer afternoons, and does that difference vary across the 24 field blocks? Keep each block's inputs, code and tables so any block can be rerun on its own.";
  const workspace = { schema_version: 1, profile: "evidence_review", image_digest: `sha256:${"d".repeat(64)}`, source_digest: `sha256:${"e".repeat(64)}`,
    input_refs: ["input://synthetic/canopy-loggers"], tool_refs: ["tool://reviewed/block-summary"], preparation_receipt: null };
  const lesson = { statement: `Block ${FAILED_BLOCK}'s second logger lost most of a day, so its summary did not finish; the comparison covers the other ${BLOCKS - 1} blocks.`,
    kind: "observation" as const, evidence_refs: [`artifact://tasks/${taskId(1)}/${packageId(1)}`] };
  const stateAt = (revision: number) => ({ schema_version: 1, goal, workspace,
    constraints: ["Treat each block as the experimental unit.", "Use only the retained logger records."],
    plan: revision === 1 ? [] : STEPS.map((s, i) => {
      const { status, task } = statusAt(revision, i + 1);
      return { ...s, status, task_id: task ? taskId(i + 1) : null,
        evidence_refs: task && status !== "active" && packages.has(i + 1) ? [`artifact://tasks/${taskId(i + 1)}/${packageId(i + 1)}`] : [] };
    }),
    lessons: revision >= 6 ? [lesson] : [],
    next_action: revision >= 8 ? "Check the model's residuals by block and by day before drawing the season figure."
      : revision >= 7 ? `Fit the afternoon model on the ${BLOCKS - 1} summarized blocks, then check its residuals before drawing the season figure.`
      : "Summarize the next batch of blocks with the reviewed method." });
  const SAVED = [0, 4, 18, 78, 138, 213, 238];
  const history = SAVED.map((minute, i) => ({ revision: i + 1, author_id: me, created_at: at(minute), state: stateAt(i + 1) }));
  const project = { project_id: projectId, group_key: "field_trials", owner_id: me, title: "Canopy temperature across the season",
    visibility: "lab" as const, revision: history.length, created_at: at(-10), updated_at: at(238) };
  // The reader last looked after the second batch of blocks, so the third batch and the comparison are new to them.
  let look: { revision: number | null; at: string | null } = { revision: 5, at: at(139) };

  const run = { run_id: runId, project_id: projectId, actor_id: me, initial_revision: 1, allowed_profiles: ["evidence_review"], max_turns: 40, max_parallel: 4,
    status: "waiting", turn: 6, generation: 8, failures: 0, code: "waiting_for_tools", created_at: at(1), updated_at: at(241), available_at: at(241) };
  const decision = (turn: number, minute: number, summary: string, steps: number[], reason: string) => ({
    turn, generation: turn + 1, context_revision: turn, checkpoint_revision: turn + 1, created_at: at(minute),
    record: { schema_version: 1, outcome: "delegate", scientific_validation: "not_performed",
      proposal: { summary, actions: steps.map((s) => ({ action_key: STEPS[s - 1].action_key, title: STEPS[s - 1].title, rationale: STEPS[s - 1].rationale })) },
      method_review: { decision: "proceed", reason }, lead: { reported_model: "synthetic-model" },
      submitted_tasks: steps.map((s) => ({ task_id: taskId(s), action_key: STEPS[s - 1].action_key })) } });
  const range = (from: number, to: number) => Array.from({ length: to - from + 1 }, (_, i) => from + i);
  const turns = [
    decision(1, 4, "Plan the season in thirty steps and check the logger records for gaps first.", [1], "Counting gaps before any summary is the right first step for logger data."),
    decision(2, 18, "No logger is missing more than a few readings so far. Summarize blocks 1 to 8, four at a time.", range(2, 9), "The reviewed block summary fits one block per analysis."),
    decision(3, 78, "Blocks 1 to 8 finished; block 7 needed a second attempt. Summarize blocks 9 to 16.", range(10, 17), "Same method and limits as the first batch."),
    decision(4, 138, "Blocks 9 to 16 finished; block 15's files could not be confirmed and are marked for a check. Summarize blocks 17 to 24.", range(18, 25), "Same method and limits."),
    decision(5, 213, `Block ${FAILED_BLOCK} did not finish: its second logger lost most of a day. Compare the other ${BLOCKS - 1} blocks and name block ${FAILED_BLOCK} as missing.`, [26],
      "Leaving the block out and naming it keeps the comparison honest; filling it in would invent readings."),
    decision(6, 238, `The comparison finished. Fit the afternoon temperature model on the ${BLOCKS - 1} summarized blocks.`, [27], "A model by block and day fits the comparison's design."),
  ];
  const runEvents = [
    { run_id: runId, sequence: 1, code: "model_unavailable", reason: "model_content_not_json", generation: 1, turn: 0, failures: 1, status_after: "waiting", occurred_at: at(2) },
    ...turns.map((t, i) => ({ run_id: runId, sequence: i + 2, code: "waiting_for_tools", reason: null, generation: t.generation, turn: t.turn, failures: 0,
      status_after: "waiting", occurred_at: at(Number(SAVED[t.turn]) + 1) })),
  ];
  const audit = [{ event_id: randomUUID(), actor_id: me, kind: "research_started", occurred_at: at(1), detail: { run_id: runId } }];
  const reviews: (TaskReview & { request_id: string })[] = [];
  const outputsOf = (step: number) => artifactsOf(step).map((a) => ({ artifact_id: a.id, relative_path: a.relative_path, sha256: a.sha256 }));

  // Each analysis's record of how it moved through the cluster. Block 7's first worker stopped responding and the
  // system ran it again; block 19's analysis could not read its input.
  const eventsOf = (step: number) => {
    const t = timing(step), block = blockOf(step);
    const list: { kind: string; state: string; code: string; occurred_at: string }[] = [
      { kind: "task", state: "queued", code: "task_accepted", occurred_at: at(t.submitted) }];
    if (t.started === null) return list.map((e, i) => ({ sequence: i + 1, ...e }));
    list.push({ kind: "lease", state: "leased", code: "worker_lease_acquired", occurred_at: at(t.started) },
      { kind: "runtime", state: "running", code: "agent_started", occurred_at: at(t.started + 0.2) });
    if (block === RETRIED_BLOCK) list.push(
      { kind: "lease", state: "queued", code: "lease_expired_requeued", occurred_at: at(t.started + 6) },
      { kind: "lease", state: "leased", code: "worker_lease_acquired", occurred_at: at(t.started + 8) },
      { kind: "runtime", state: "running", code: "agent_started", occurred_at: at(t.started + 8.2) });
    if (t.finished !== null) {
      if (block === FAILED_BLOCK) list.push({ kind: "task", state: "failed", code: "agent_failed", occurred_at: at(t.finished - 0.2) },
        { kind: "task", state: "failed", code: "task_failed", occurred_at: at(t.finished) });
      else list.push({ kind: "artifact", state: "running", code: "evidence_collected", occurred_at: at(t.finished - 0.3) },
        { kind: "task", state: "completed", code: "task_completed", occurred_at: at(t.finished) });
    }
    return list.map((e, i) => ({ sequence: i + 1, ...e }));
  };
  const taskView = (step: number) => {
    const t = tasks[step - 1];
    return { id: t.task_id, group: "field_trials", profile: t.profile, status: t.status, objective: STEPS[step - 1].title, attempt: t.attempt,
      max_task_attempts: 2, max_provider_attempts: 2, logical_agent: "synthetic_preview_agent",
      result_class: t.status === "completed" ? "success" : null, error_code: t.status === "failed" ? "input_unreadable" : null,
      submitted_at: t.submitted_at, updated_at: t.updated_at, started_at: t.started_at, finished_at: t.finished_at };
  };
  const lineage = (step: number) => {
    const t = tasks[step - 1];
    const turn = turns.find((x) => x.record.submitted_tasks.some((s) => s.task_id === t.task_id))!;
    const first = history.find((h) => h.state.plan.some((s) => s.task_id === t.task_id))!;
    const now = history.at(-1)!.state.plan[step - 1];
    return {
      schema: "enrichedlab.task-lineage.v1",
      task: { task_id: t.task_id, project_id: projectId, action_key: t.action_key, objective: STEPS[step - 1].title, status: t.status,
        result_class: t.status === "completed" ? "success" : null, error_code: t.status === "failed" ? "input_unreadable" : null, attempt: t.attempt,
        submitted_at: t.submitted_at, started_at: t.started_at, finished_at: t.finished_at },
      origin: { origin: "research_run", basis: "submission", principal_id: me, principal_name: "Example researcher", principal_kind: "person",
        run_id: runId, turn: turn.turn, recorded_at: turn.created_at },
      plan_step: { action_key: now.action_key, title: now.title, status: now.status, rationale: now.rationale, revision: first.revision, added_after_submission: false, current: null },
      inputs: { workspace, checkpoint_revision: turn.checkpoint_revision, platform_tool: null, classes: ["public_reference"], classes_recorded_by: "input_custodian" },
      method: { profile: "evidence_review", profile_name: "Evidence review", tool_class: "agent_session", template_ref: null, template_sha256: null,
        recorded: "submission", harness: null, role: null, tool_bundle: null },
      code: null, query: null,
      execution: { image_ref: `registry.example.invalid/release/agent-sandbox/site@sha256:${"7".repeat(64)}`, cpu_limit: "2", memory_limit: "4Gi",
        max_run_seconds: 3600, attempts: t.attempt, scientific_history: 0 },
      outputs: artifactsOf(step).map((a) => ({ artifact_id: a.id, role: a.role, label: a.label, relative_path: a.relative_path, media_type: a.media_type,
        size_bytes: a.size_bytes, sha256: a.sha256, created_at: a.created_at, provenance_class: "agent_session_output" })),
      review: { state: "unvalidated" },
      gaps: artifactsOf(step).length || t.status === "running" || t.status === "queued" ? [] : ["no_outputs_recorded"],
    };
  };

  // The plan ledger as the broker records it: each saved version against the one before it.
  const ledger = () => ({
    revisions: history.map((saved, index) => {
      const before = new Map((history[index - 1]?.state.plan ?? []).map((s, i) => [s.action_key, { s, n: i + 1 }]));
      const steps = saved.state.plan.flatMap((s, i) => {
        const a = before.get(s.action_key)?.s;
        const changes = !a ? ["planned"] : [...(a.task_id !== s.task_id ? ["task"] : []), ...(a.status !== s.status ? ["status"] : []),
          ...(a.evidence_refs.length !== s.evidence_refs.length ? ["results"] : [])];
        if (!changes.length) return [];
        return [{ action_key: s.action_key, step_number: i + 1, changes, status_before: a?.status ?? null, status_after: s.status, task_id: s.task_id,
          result_count: s.evidence_refs.length, title: a ? null : s.title, rationale: a ? null : s.rationale, task_status: null, error_code: null }];
      });
      const turn = turns.find((t) => t.checkpoint_revision === saved.revision);
      return { revision: saved.revision, recorded_at: saved.created_at, source: turn ? "lead_turn" : "person", run_id: turn ? runId : null,
        turn: turn?.turn ?? null, principal_id: turn ? null : me, steps };
    }),
    unrecorded_revisions: 0, truncated: false,
  });

  const own = () => tasks.filter((t) => t.status !== "running" && t.status !== "queued");
  const summary = () => ({ ...project, owner: { principal_id: me, display_name: "Example researcher", kind: "person" }, question: goal,
    capabilities: ["read", "review", "contribute", "steer", "cancel", "manage"], last_look: look,
    latest_run: { run_id: runId, status: run.status, code: run.code, turn: run.turn, max_turns: run.max_turns, updated_at: run.updated_at },
    task_counts: { total: tasks.length, active: tasks.length - own().length,
      finished: own().filter((t) => t.status === "completed").length, failed: own().filter((t) => t.status === "failed").length,
      reviewed: new Set(reviews.filter((r) => r.decision === "accepted").map((r) => r.task_id)).size, rejected: 0, changes: 0 } });
  // The project read keeps its newest analyses and a cursor for the rest. The preview pages
  // ten at a time, fewer than the broker's 200, so a reader of this project follows the cursor through several pages.
  const PAGE = 10;
  // Figure versions a person draws from the comparison table join the project's analyses.
  const figures: ProjectTask[] = [];
  const total = () => tasks.length + figures.length;
  const newest = () => [...tasks.map(listed), ...figures].sort((a, b) => b.submitted_at.localeCompare(a.submitted_at) || b.task_id.localeCompare(a.task_id));
  const projectView = () => ({ ...summary(), inspection_expires_at: null, checkpoint: history.at(-1), tasks: newest().slice(0, PAGE),
    task_count: total(), analyses_total: total(), analyses_next: total() > PAGE ? String(PAGE) : null });

  // The run moves on while a page is open: first the model finishes with its package and the residual check waits for a
  // compute slot; the next call gives it one.
  function advance() {
    if (advancedAt !== null) {
      if (slotAt !== null) return;
      slotAt = (Date.now() - t0) / 60_000;
      logStart = Date.now();
      Object.assign(tasks[27], taskRecord(28));
      return;
    }
    advancedAt = (Date.now() - t0) / 60_000;
    const days = [...blockReadings.entries()].filter(([b]) => b !== FAILED_BLOCK).flatMap(([b, list]) => DAYS.map((day) => {
      const afternoon = list.filter((r) => r.time.startsWith(day) && r.hour >= 12 && r.hour <= 17);
      return { block: b, day, difference: mean(afternoon.map((r) => r.canopy - r.air)) };
    }));
    const byBlock = new Map<number, number>();
    for (const b of new Set(days.map((d) => d.block))) byBlock.set(b, mean(days.filter((d) => d.block === b).map((d) => d.difference)));
    const residuals = days.map((d) => d.difference - byBlock.get(d.block)!);
    const sigma = Math.sqrt(residuals.reduce((n, r) => n + r * r, 0) / (days.length - byBlock.size));
    packages.set(27, syntheticPackage([
      { path: "input/block_days.csv", bytes: csv(["block", "day", "afternoon_difference_c"], days.map((d) => [d.block, d.day, d.difference.toFixed(3)])) },
      { path: "results/model_coefficients.csv", bytes: csv(["term", "estimate"], [...byBlock.entries()].map(([b, v]) => [`factor(block)${b}`, v.toFixed(3)])) },
      { path: "results/model_fit.csv", bytes: csv(["statistic", "value"], [["blocks", byBlock.size], ["observations", days.length], ["residual_sd_c", sigma.toFixed(3)]]) },
      { path: "code/fit_afternoon_model.R", bytes: Buffer.from(MODEL_R) },
      ...common("Fit the afternoon temperature model", "A linear model of each day's afternoon difference by block; block 19 has no summary and is not in it.",
        ["Rscript code/fit_afternoon_model.R", `Read ${days.length} block-days`, "Wrote results/model_coefficients.csv", "Wrote results/model_fit.csv"],
        { blocks: [...byBlock.keys()], afternoon_hours: [12, 17] }),
    ], mtime + 27 * 60));
    Object.assign(tasks[26], taskRecord(27));
    tasks.push(taskRecord(28));
    const turn = decision(7, advancedAt, "The model finished. Check its residuals by block and by day before drawing the season figure.", [28],
      "Residuals by block and day show whether one block or one hot day drives the fit.");
    turns.push(turn);
    history.push({ revision: 8, author_id: me, created_at: at(advancedAt), state: stateAt(8) });
    project.revision = 8; project.updated_at = at(advancedAt);
    Object.assign(run, { turn: 7, generation: 9, updated_at: at(advancedAt), available_at: at(advancedAt) });
    runEvents.push({ run_id: runId, sequence: runEvents.length + 1, code: "waiting_for_tools", reason: null, generation: 9, turn: 7, failures: 0,
      status_after: "waiting", occurred_at: at(advancedAt) });
  }

  const body = async (req: IncomingMessage) => { let text = ""; for await (const chunk of req) text += chunk; return text ? JSON.parse(text) : {}; };
  const stepOf = (task: string) => tasks.findIndex((t) => t.task_id === task) + 1;

  const pulseOf = () => sha(Buffer.from(JSON.stringify([project.revision, [...tasks, ...figures].map((t) => [t.task_id, t.status])]))).slice(0, 32);

  return {
    /** The project as the list shows it. */
    summaries: () => [summary()],
    /** Whether work runs on the long run's project; its session works throughout. */
    busy: (id: string) => (id === project.project_id ? true : undefined),
    advance,
    owns: (task: string) => stepOf(task) > 0,
    /** Preview-only: the residual check's output starts again from its first line, for the browser suite. */
    restartLog: () => { if (logStart !== null) logStart = Date.now(); return logStart !== null; },
    /** The comparison step's tables, from which a person can draw figures. */
    figureSource: (): FigureSource => ({
      project: projectId, group: project.group_key, sourceTask: taskId(COMPARE_STEP),
      source: () => ({ files: packages.get(COMPARE_STEP)!.files, artifact: artifactsOf(COMPARE_STEP)[0] }),
      task: () => ({ status: tasks[COMPARE_STEP - 1].status, objective: STEPS[COMPARE_STEP - 1].title }),
      addProjectTask: (entry) => { figures.push(entry); },
    }),
    async handle(req: IncomingMessage, res: ServerResponse, url: URL, json: Json, denied: () => void): Promise<boolean> {
      const path = url.pathname;
      const projectMatch = path.match(/^\/api\/v1\/projects\/([0-9a-f-]{36})(\/.*)?$/);
      if (projectMatch && projectMatch[1] === projectId) {
        const suffix = projectMatch[2] ?? "";
        if (!suffix) { json(projectView()); return true; }
        // Figures drawn from the comparison table are the figure module's.
        if (suffix.startsWith("/figures")) return false;
        if (suffix === "/analyses" && req.method === "GET") {
          const cursor = url.searchParams.get("cursor") ?? "0", limit = Number(url.searchParams.get("limit") ?? 200);
          if (!/^\d+$/.test(cursor) || Number(cursor) > total()) { json({ error: { code: "analyses_cursor_invalid" } }, 400); return true; }
          if (!Number.isInteger(limit) || limit < 1 || limit > 200) { json({ error: { code: "list_limit_invalid" } }, 400); return true; }
          const from = Number(cursor), page = newest().slice(from, from + Math.min(limit, PAGE));
          json({ analyses: page, next: from + page.length < total() ? String(from + page.length) : null, total: total() }); return true;
        }
        if (suffix === "/events/stream" && req.method === "GET") {
          res.setHeader("Content-Type", "text/event-stream");
          res.setHeader("Connection", "keep-alive");
          res.flushHeaders();
          let last = pulseOf();
          res.write(`event: project_state\ndata: ${JSON.stringify({ pulse: last, busy: true })}\n\n`);
          // Each change is sent as it happens, read twice a second here, as the main project's stream does.
          const tick = setInterval(() => {
            const next = pulseOf();
            if (next !== last) { last = next; res.write(`event: project_changed\ndata: ${JSON.stringify({ pulse: next, busy: true })}\n\n`); }
          }, 500);
          req.on("close", () => clearInterval(tick));
          return true;
        }
        if (suffix === "/look" && req.method === "POST") {
          const input = await body(req);
          if (!Number.isInteger(input.revision) || input.revision < 0 || input.revision > project.revision) { json({ error: { code: "look_revision_invalid" } }, 400); return true; }
          // Forward-only: a lower revision leaves the stored look; the same or a newer one moves it to now.
          if (look.revision === null || input.revision >= look.revision) look = { revision: input.revision, at: new Date().toISOString() };
          json(look); return true;
        }
        if (suffix === "/checkpoints" && req.method === "GET") { json({ checkpoints: [...history].reverse() }); return true; }
        if (suffix === "/runs" && req.method === "GET") { json({ runs: [run] }); return true; }
        if (suffix === "/runs" && req.method === "POST") { json({ error: { code: "research_run_active" } }, 409); return true; }
        if (suffix === "/audit") { json({ events: [...audit].reverse() }); return true; }
        if (suffix === "/members") {
          json({ owner: { principal_id: me, display_name: "Example researcher", email: "researcher@example.invalid" }, grants: [],
            eligible: [{ principal_id: "20000000-0000-4000-8000-000000000002", display_name: "Example colleague", email: "colleague@example.invalid" }] });
          return true;
        }
        if (suffix === "/reviews" && req.method === "GET") {
          json({ reviews: [...reviews].reverse().filter((r, i, all) => all.findIndex((x) => x.task_id === r.task_id) === i).map(({ request_id: _, ...r }) => r) });
          return true;
        }
        if (suffix === "/plan-ledger" && req.method === "GET") { json(ledger()); return true; }
        if (suffix === "/acknowledgements" && req.method === "GET") { json({ acknowledgements: [] }); return true; }
        if (suffix === "/working-copies" && req.method === "GET") { json({ working_copies: [] }); return true; }
        if (suffix === "/workspaces") { json({ workspaces: [] }); return true; }
        if (suffix === "/package/preview") {
          const included = [...packages.keys()];
          json({ root: "research-evidence-canopy-temperature-across-the-season", snapshot_at: new Date().toISOString(),
            files: included.reduce((n, s) => n + packages.get(s)!.files.length, 0) + 12,
            uncompressed_bytes: included.reduce((n, s) => n + packages.get(s)!.files.reduce((m, f) => m + f.bytes.length, 0), 0),
            counts: { plan_revisions: history.length, research_runs: 1, agent_decisions: turns.length, agent_deferrals: runEvents.length, access_events: audit.length,
              tasks: tasks.length, artifacts_included: included.length * 2, artifacts_missing: 0,
              artifact_bytes_included: included.reduce((n, s) => n + packages.get(s)!.archive.length + packages.get(s)!.checksum.length, 0), rerun_analyses: included.length },
            coverage: { database_records: "complete", artifact_bytes: "complete", scientific_validation: "not_performed" }, missing: [] });
          return true;
        }
        denied(); return true;
      }
      const runMatch = path.match(/^\/api\/v1\/research-runs\/([0-9a-f-]{36})\/(turns|events|pause)$/);
      if (runMatch && runMatch[1] === runId) {
        const after = Number(url.searchParams.get("after") ?? 0);
        if (runMatch[2] === "turns") json({ turns: turns.filter((t) => t.turn > after) });
        else if (runMatch[2] === "events") json({ events: runEvents.filter((e) => e.sequence > after) });
        else if (req.method !== "POST") denied();
        else { run.status = "paused"; run.code = "user_paused"; run.updated_at = new Date().toISOString(); res.statusCode = 204; res.end(); }
        return true;
      }
      const taskMatch = path.match(/^\/api\/v1\/tasks\/([0-9a-f-]{36})(\/.*)?$/);
      const step = taskMatch ? stepOf(taskMatch[1]) : 0;
      if (!taskMatch || !step) return false;
      const suffix = taskMatch[2] ?? "";
      if (!suffix) { json(taskView(step)); return true; }
      if (suffix === "/events") { json({ events: eventsOf(step).filter((e) => e.sequence > Number(url.searchParams.get("after") ?? 0)) }); return true; }
      if (suffix === "/log/stream" && req.method === "GET") {
        const t = tasks[step - 1];
        serveLog(req, res, { attempt: t.attempt, ended: !["queued", "running"].includes(t.status),
          started: step === RESIDUAL_STEP ? logStart : t.started_at ? Date.parse(t.started_at) : null,
          script: step === RESIDUAL_STEP ? residualScript([...blockReadings.keys()].filter((b) => b !== FAILED_BLOCK), DAYS.length, FAILED_BLOCK) : GENERIC_SCRIPT });
        return true;
      }
      if (suffix === "/events/stream") {
        res.setHeader("Content-Type", "text/event-stream");
        res.setHeader("Connection", "keep-alive");
        res.flushHeaders();
        const after = Number(url.searchParams.get("after") ?? 0);
        for (const event of eventsOf(step).filter((e) => e.sequence > after)) res.write(`id: ${event.sequence}\nevent: task_event\ndata: ${JSON.stringify(event)}\n\n`);
        if (tasks[step - 1].status !== "running" && tasks[step - 1].status !== "queued") { res.write(`event: stream_complete\ndata: ${JSON.stringify({ status: tasks[step - 1].status })}\n\n`); res.end(); }
        else { const keep = setInterval(() => res.write(": still open\n\n"), 15_000); req.on("close", () => clearInterval(keep)); }
        return true;
      }
      if (suffix === "/artifacts") { json({ artifacts: artifactsOf(step) }); return true; }
      if (suffix === "/lineage") { json(lineage(step)); return true; }
      if (suffix === "/generated-code") { json({ generated_code: null }); return true; }
      if (suffix === "/query") { json({ query: null }); return true; }
      // The figure module answers which figures were drawn from a step.
      if (suffix === "/figure" || suffix === "/figures") return false;
      if (suffix === "/scientific-history") { json({ coverage: "pending", attempt: null, lease_generation: null, events: [], next_cursor: null }); return true; }
      if (suffix === "/reviews" && req.method === "GET") {
        json({ reviews: reviews.filter((r) => r.task_id === taskId(step)).reverse().map(({ request_id: _, ...r }) => r) }); return true;
      }
      if (suffix === "/reviews" && req.method === "POST") {
        const input = await body(req);
        const note = typeof input.note === "string" ? input.note.trim() || null : null;
        const same = reviews.find((r) => r.request_id === input.request_id);
        if (same) { json({ review: (({ request_id: _, ...r }) => r)(same) }, 201); return true; }
        if (!uuid.test(input.request_id ?? "") || !["accepted", "needs_changes", "rejected"].includes(input.decision) || (input.decision !== "accepted" && !note)) {
          json({ error: { code: "review_invalid" } }, 400); return true;
        }
        if (tasks[step - 1].status === "running" || tasks[step - 1].status === "queued") { json({ error: { code: "review_not_finished" } }, 409); return true; }
        if (input.decision === "accepted" && !artifactsOf(step).length) { json({ error: { code: "review_nothing_to_accept" } }, 409); return true; }
        const saved = { review_id: randomUUID(), request_id: input.request_id, task_id: taskId(step), attempt: tasks[step - 1].attempt, decision: input.decision, note,
          reviewer_id: me, reviewer_name: "Example researcher", reviewer_kind: "person" as const, outputs: outputsOf(step), created_at: new Date().toISOString(),
          covers_current_outputs: true };
        reviews.push(saved);
        json({ review: (({ request_id: _, ...r }) => r)(saved) }, 201); return true;
      }
      const artifactMatch = suffix.match(/^\/artifacts\/([0-9a-f-]{36})\/(contents(?:\/file)?|download|digest|pages|summary)$/);
      const pkg = packages.get(step);
      const artifact = artifactsOf(step).find((a) => a.id === artifactMatch?.[1]);
      if (!artifactMatch || !pkg || !artifact) { denied(); return true; }
      if (artifactMatch[2] === "digest") {
        json(artifact.id === packageId(step) ? fixtureDigest(pkg, artifact.id, artifact.sha256, artifact.created_at)
          : { artifact_id: artifact.id, artifact_sha256: artifact.sha256, status: "not_recorded", digest_sha256: null, recorded_at: null, digest: null });
        return true;
      }
      if (artifactMatch[2] === "pages" || artifactMatch[2] === "summary") {
        answerTable(artifactMatch[2], url, artifact.id === packageId(step) ? pkg.files : null, { path: artifact.relative_path, bytes: pkg.checksum }, json);
        return true;
      }
      if (artifactMatch[2] === "contents") {
        if (artifact.id !== packageId(step)) { json({ error: { code: "artifact_not_a_package" } }, 415); return true; }
        json({ artifact_id: artifact.id, sha256: artifact.sha256, size_bytes: artifact.size_bytes, listing: listing(pkg.files, step === COMPARE_STEP ? seasonResults : {}) });
        return true;
      }
      const bytes = artifactMatch[2] === "download" ? (artifact.id === packageId(step) ? pkg.archive : pkg.checksum)
        : pkg.files.find((f) => f.path === url.searchParams.get("path"))?.bytes;
      if (!bytes) { denied(); return true; }
      res.setHeader("Content-Type", artifactMatch[2] === "download" ? artifact.media_type : "application/octet-stream");
      res.setHeader("Content-Length", bytes.length);
      res.setHeader("X-Artifact-Sha256", sha(bytes));
      res.end(bytes);
      return true;
    },
  };
}
