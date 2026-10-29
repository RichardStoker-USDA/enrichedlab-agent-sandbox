import { createHash, randomUUID } from "node:crypto";
import type { IncomingMessage, ServerResponse } from "node:http";
import type { Artifact, FigureFeature, Task, TaskEvent } from "../src/contracts.ts";
import { ANALYSIS_FEATURES } from "./analysis_catalog.ts";
import type { FigureSource } from "./figures.ts";
import { csvRows, listing, syntheticPackage } from "./package.ts";
import type { ProjectTask } from "./projects.ts";
import { runStandIn, type Cell, type Outcome } from "./r_stand_in.ts";
import { answerTable } from "./tables.ts";

// Reviewed R analyses for the loopback preview: the catalog the worker keeps, a person's
// request checked as the broker's `broker_request_analysis` checks it, the request with its receipts, the analyses made
// from one analysis, and each run's package. A stand-in for the render service runs the work (fixtures/r_stand_in.ts);
// a refusal carries the broker's words for its code, filled from the columns and counts the template named, never the
// template's own text. Every value the preview holds is simulated.

type Json = (value: unknown, status?: number) => void;
type Files = { path: string; bytes: Buffer }[];
type Settings = Record<string, unknown>;
const sha = (bytes: Buffer | string) => createHash("sha256").update(bytes).digest("hex");
const VERSION = "0.6.0";
const IMAGE = `sha256:${"6".repeat(64)}`;
const SEEN = new Date(Date.UTC(2026, 9, 12, 9, 0)).toISOString();
const CATALOG_SHA = sha(JSON.stringify(ANALYSIS_FEATURES) + VERSION + IMAGE);

// The broker's words for the refusal codes the stand-in names (`REFUSALS` in the broker's analysis_run.rs). `{column}`
// takes the refusal's next column, `{columns}` its remaining columns and `{count}` its next count.
const WORDS: Record<string, string> = {
  column_missing: "A column this analysis needs is not in the table.",
  no_numeric_values: "No numbers this analysis can use in {columns}.",
  group_count_not_two: "This analysis compares exactly two groups; {column} has {count}.",
  too_few_values_per_group: "Each group in {column} needs at least {count} values.",
  setting_invalid: "The analysis service did not accept one of these settings.",
  time_not_numeric: "This analysis needs numeric times in {column}.",
  factor_varies_within_animal: "{count} animals in {column} have more than one value of {column}; a group or factor here belongs to the animal.",
  repeated_time_rows: "{count} pairs of {column} and {column} have more than one row; keep one reading per animal and time.",
  too_few_time_points: "{column} has readings at {count} times; this analysis needs at least {count}.",
  too_few_animals: "{count} animals in {column} have the readings this analysis needs; it needs at least {count}.",
  repeated_rows_per_animal: "{count} animals in {column} have more than one row; summarize each animal first, so animals and not hours or days are compared.",
};
const SERVICE_REFUSED = "The analysis service refused these settings.";

/** A message with its places filled as the broker fills them; null when the columns or counts do not fit exactly. */
export function brokerWords(code: string, columns: string[], counts: number[]): string | null {
  const words = WORDS[code];
  if (!words) return null;
  const shown = (name: string) => [...name.replace(/[\u0000-\u001f\u007f]/g, "")].slice(0, 48).join("");
  const listed = (names: string[]) => names.length === 1 ? names[0] : `${names.slice(0, -1).join(", ")} and ${names.at(-1)}`;
  let c = 0, n = 0, failed = false;
  const out = words.replace(/\{(columns|column|count)\}/g, (_, place: string) => {
    if (place === "columns") { const rest = columns.slice(c).map(shown); c = columns.length; if (!rest.length) failed = true; return rest.length ? listed(rest) : ""; }
    if (place === "column") { if (c >= columns.length) failed = true; return shown(columns[c++] ?? ""); }
    if (n >= counts.length) failed = true;
    return String(counts[n++] ?? "");
  });
  return failed || c !== columns.length || n !== counts.length ? null : out;
}

// A results file as the broker writes it: NA unquoted for a missing value, logicals TRUE and FALSE, each number as the
// shortest text that reads back as it, and text quoted only when it is empty, is NA or holds a comma, quote or line break.
const csvText = (c: Cell) => c === null || (typeof c === "number" && !Number.isFinite(c)) ? "NA" : typeof c === "boolean" ? (c ? "TRUE" : "FALSE")
  : typeof c === "number" ? String(c) : c === "" || c === "NA" || /[,"\r\n]/.test(c) ? `"${c.replaceAll('"', '""')}"` : c;
const csvOf = (columns: string[], rows: Cell[][]) => [columns.map(csvText).join(","), ...rows.map((r) => r.map(csvText).join(","))].join("\n") + "\n";

// The tolerance test's figure as the template draws it, each group's mean curve with its standard error, without R's
// styling and marked as the preview's stand-in.
function curveDrawing(curves: NonNullable<Outcome["curves"]>, title: string, xLabel: string, yLabel: string): string {
  const w = 720, h = 460, left = 72, right = 150, top = 48, bottom = 56, pw = w - left - right, ph = h - top - bottom;
  const esc = (s: string) => s.replace(/[&<>"]/g, (ch) => ({ "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;" })[ch]!);
  const ts = curves.flatMap((c) => c.points.map((p) => p.t)), ys = curves.flatMap((c) => c.points.flatMap((p) => [p.mean - (p.se ?? 0), p.mean + (p.se ?? 0)]));
  const [t0, t1] = [Math.min(...ts), Math.max(...ts)], [y0, y1] = [Math.min(...ys), Math.max(...ys)];
  const pad = (y1 - y0) * 0.08 || 1;
  const x = (t: number) => left + (t1 === t0 ? pw / 2 : ((t - t0) / (t1 - t0)) * pw), y = (v: number) => top + ph - ((v - (y0 - pad)) / (y1 - y0 + 2 * pad)) * ph;
  const colours = ["#0072B2", "#E69F00", "#009E73", "#CC79A7", "#56B4E9", "#D55E00"];
  const ticks = [...new Set(ts)].sort((a, b) => a - b);
  const raw = (y1 - y0 + 2 * pad) / 5, magnitude = Math.pow(10, Math.floor(Math.log10(raw))), norm = raw / magnitude;
  const step = (norm < 1.5 ? 1 : norm < 3.5 ? 2 : norm < 7.5 ? 5 : 10) * magnitude;
  const yTicks: number[] = [];
  for (let v = Math.ceil((y0 - pad) / step) * step; v <= y1 + pad; v += step) yTicks.push(Number(v.toPrecision(6)));
  const lines = curves.map((c, i) => {
    const colour = colours[i % colours.length];
    const path = c.points.map((p, k) => `${k ? "L" : "M"}${x(p.t).toFixed(1)},${y(p.mean).toFixed(1)}`).join("");
    const bars = c.points.filter((p) => p.se !== null).map((p) => `<line x1="${x(p.t).toFixed(1)}" x2="${x(p.t).toFixed(1)}" y1="${y(p.mean - p.se!).toFixed(1)}" y2="${y(p.mean + p.se!).toFixed(1)}" stroke="${colour}" stroke-width="1.5"/>`).join("");
    const dots = c.points.map((p) => `<circle cx="${x(p.t).toFixed(1)}" cy="${y(p.mean).toFixed(1)}" r="3.5" fill="${colour}"/>`).join("");
    const key = `<rect x="${w - right + 18}" y="${top + 8 + i * 22}" width="12" height="12" fill="${colour}"/><text x="${w - right + 36}" y="${top + 18 + i * 22}">${esc(c.group)}</text>`;
    return `<path d="${path}" fill="none" stroke="${colour}" stroke-width="2"/>${bars}${dots}${key}`;
  }).join("");
  return `<svg xmlns="http://www.w3.org/2000/svg" width="${w}" height="${h}" viewBox="0 0 ${w} ${h}" font-family="Helvetica, Arial, sans-serif" font-size="12" fill="#1a1a1a">`
    + `<rect width="${w}" height="${h}" fill="#ffffff"/>`
    + `<text x="16" y="26" font-size="15" font-weight="600">${esc(title)}</text>`
    + `<line x1="${left}" x2="${left}" y1="${top}" y2="${top + ph}" stroke="#4d4d4d"/><line x1="${left}" x2="${left + pw}" y1="${top + ph}" y2="${top + ph}" stroke="#4d4d4d"/>`
    + ticks.map((t) => `<text x="${x(t).toFixed(1)}" y="${top + ph + 18}" text-anchor="middle">${t}</text>`).join("")
    + yTicks.map((v) => `<text x="${left - 8}" y="${(y(v) + 4).toFixed(1)}" text-anchor="end">${v}</text><line x1="${left}" x2="${left + pw}" y1="${y(v).toFixed(1)}" y2="${y(v).toFixed(1)}" stroke="#ebebeb"/>`).join("")
    + `<text x="${left + pw / 2}" y="${h - 14}" text-anchor="middle">${esc(xLabel)}</text>`
    + `<text transform="translate(18 ${top + ph / 2}) rotate(-90)" text-anchor="middle">${esc(yLabel)}, mean and standard error</text>`
    + lines
    + `<text x="${w - 8}" y="${h - 6}" text-anchor="end" font-size="9" fill="#8c8c8c">Workspace preview stand-in, not drawn by R</text></svg>\n`;
}

type Receipt = {
  attempt: number; status: "completed" | "refused" | "failed"; error_code: string | null; message: string | null;
  refusal: { code: string; columns: string[]; counts: number[] } | null; service_version: string | null; image_digest: string | null;
  method: string | null; files: { path: string; sha256: string; size_bytes: number; media_type: string; role: string }[] | null;
  notes: string[]; package_sha256: string | null; started_at: string; finished_at: string;
};
type Run = {
  task: Task; entry: ProjectTask; src: FigureSource; request_id: string; body: string;
  request: { table_path: string; table_sha256: string; table_rows: number; table_columns: string[]; feature: string; settings: Settings;
    title: string; previous: string | null; request_sha256: string; created_at: string; source_package_sha256: string };
  receipts: Receipt[]; pkg: ReturnType<typeof syntheticPackage> | null; artifacts: Artifact[];
};

const kinds = { column: "string", columns: "array", enum: "enum", number: "number", numbers: "array", string: "string" } as const;
/** The broker's check of a request's settings against the catalog: declared names, values of their kinds, columns of
 *  the table, listed choices, ranges, every required column, at most 40 settings and 8 KiB. */
export function settingsProblem(feature: FigureFeature, settings: Settings, columns: string[]): boolean {
  if (Object.keys(settings).length > 40 || JSON.stringify(settings).length > 8192) return true;
  for (const [name, value] of Object.entries(settings)) {
    const param = feature.params[name];
    if (!param || !(param.type in kinds)) return true;
    if (param.type === "column" && (typeof value !== "string" || !columns.includes(value))) return true;
    if (param.type === "columns" && (!Array.isArray(value) || !value.length || value.some((c) => typeof c !== "string" || !columns.includes(c)))) return true;
    if (param.type === "enum" && !(param.choices ?? []).some((c) => String(c) === String(value))) return true;
    if (param.type === "number" && (typeof value !== "number" || !Number.isFinite(value) || (param.min !== undefined && value < param.min) || (param.max !== undefined && value > param.max))) return true;
    if (param.type === "numbers" && (!Array.isArray(value) || value.some((n) => typeof n !== "number" || !Number.isFinite(n)))) return true;
    if (param.type === "string" && (typeof value !== "string" || value.length > (param.maxlen ?? 200))) return true;
  }
  return Object.entries(feature.params).some(([name, p]) => p.required && (p.type === "column" || p.type === "columns") && settings[name] === undefined);
}

export function analysisFixtures(options: {
  me: string; profile: string; tasks: Task[]; histories: Map<string, TaskEvent[]>;
  /** The tables analyses read: result tables of finished analyses and prepared lab tables. */
  sources: () => FigureSource[];
  /** Analyses other modules serve: the routes answer them as analyses that are not template analyses. */
  knows?: (task: string) => boolean;
  /** The labs whose template-analysis method is enabled. */
  groups: string[];
  delay?: number;
}) {
  const { me, profile, tasks, histories, knows = () => false, groups } = options;
  const delay = options.delay ?? 600;
  const runs: Run[] = [];
  const timers = new Set<ReturnType<typeof setTimeout>>();
  const later = (ms: number, work: () => void) => { const t = setTimeout(() => { timers.delete(t); work(); }, ms); timers.add(t); };
  const refuse = (json: Json, code: string, status = 400) => json({ error: { code, message: "Refused" } }, status);
  const own = (id: string) => runs.find((r) => r.task.id === id);
  // A finished template analysis's results table is itself a source, for a figure or another analysis.
  const results = (): FigureSource[] => runs.filter((r) => r.task.status === "completed" && r.pkg).map((r) => ({
    project: r.src.project, group: r.src.group, sourceTask: r.task.id, source: () => ({ files: r.pkg!.files, artifact: r.artifacts[0] }),
    task: () => ({ status: r.task.status, objective: r.task.objective ?? "" }), addProjectTask: r.src.addProjectTask }));
  const listed = () => [...options.sources(), ...results()];
  const known = (id: string) => tasks.some((t) => t.id === id) || knows(id);
  function event(task: Task, state: string, code: string) {
    const history = histories.get(task.id)!;
    history.push({ sequence: history.length + 1, kind: "task", state, code, occurred_at: new Date().toISOString() });
  }

  function check(project: string, input: Record<string, unknown>) {
    const title = typeof input.title === "string" ? input.title : "";
    const tablePath = typeof input.table_path === "string" ? input.table_path : "";
    if (!tablePath || Buffer.byteLength(tablePath) > 256 || [...title].length > 200) return { refusal: ["analysis_request_invalid", 400] as const };
    const settings = (input.settings ?? {}) as Settings;
    if (typeof settings !== "object" || Array.isArray(settings)) return { refusal: ["analysis_settings_invalid", 400] as const };
    if (!listed().some((x) => x.project === project)) return { refusal: ["project_not_found", 404] as const };
    const src = listed().find((x) => x.project === project && x.sourceTask === input.source_task);
    const task = src?.task();
    if (!src || !task) return { refusal: ["task_not_found", 404] as const };
    if (task.status !== "completed") return { refusal: ["analysis_source_not_ready", 409] as const };
    const table = src.source().files.find((f) => f.path === tablePath && /^results\/(lab_data\/|lab_derived\/)?[^/]+\.csv$/.test(f.path));
    if (!table) return { refusal: ["analysis_table_unknown", 400] as const };
    const [columns = [], ...rows] = csvRows(table.bytes.toString("utf8"));
    if (!rows.length || rows.length > 50_000 || columns.length > 64) return { refusal: ["analysis_table_unsuitable", 400] as const };
    if (!groups.includes(src.group)) return { refusal: ["analysis_method_unavailable", 403] as const };
    const feature = ANALYSIS_FEATURES.find((f) => f.id === input.feature);
    if (!feature) return { refusal: ["analysis_feature_unknown", 400] as const };
    if (settingsProblem(feature, settings, columns)) return { refusal: ["analysis_settings_invalid", 400] as const };
    const previous = typeof input.previous === "string" ? input.previous : null;
    if (previous && !runs.some((r) => r.task.id === previous && r.src.sourceTask === src.sourceTask && r.request.table_path === tablePath))
      return { refusal: ["analysis_previous_invalid", 400] as const };
    return { src, table, columns, rows, feature, settings, title, previous };
  }

  function settle(run: Run, feature: FigureFeature, columns: string[], rows: string[][]) {
    later(delay, () => {
      const now = new Date().toISOString();
      Object.assign(run.task, { status: "running", attempt: 1, started_at: now, updated_at: now, logical_agent: "render-worker" });
      Object.assign(run.entry, { status: "running", attempt: 1, updated_at: now });
      event(run.task, "running", "worker_lease_acquired");
    });
    later(delay * 2.5, () => {
      const outcome = runStandIn(feature.id, { columns, rows }, run.request.settings, feature.label);
      const started = run.task.started_at!, finished = new Date().toISOString();
      const base = { attempt: 1, service_version: VERSION, image_digest: IMAGE, started_at: started, finished_at: finished };
      if (outcome.refusal || !outcome.results) {
        const r = outcome.refusal ?? { code: "", columns: [], counts: [] };
        const words = brokerWords(r.code, r.columns, r.counts);
        run.receipts.push({ ...base, status: "refused", error_code: "analysis_refused", message: words ?? SERVICE_REFUSED, refusal: words ? r : null,
          method: null, files: null, notes: [], package_sha256: null });
        Object.assign(run.task, { status: "failed", result_class: "analysis_refused", error_code: "analysis_refused", finished_at: finished, updated_at: finished });
        Object.assign(run.entry, { status: "failed", updated_at: finished });
        event(run.task, "failed", "task_failed");
        return;
      }
      // An analysis of a lab's own table writes its results under lab_derived.
      const lab = /^results\/(lab_data|lab_derived)\//.test(run.request.table_path);
      const resultsPath = lab ? "results/lab_derived/results.csv" : "results/results.csv";
      const title = run.request.title || run.task.objective || feature.label;
      const stand = (what: string) => Buffer.from(`# Preview stand-in for ${what}.\n# On the cluster this file is the reviewed R code that ran; the preview's stand-in follows its rules in TypeScript.\n`);
      const files: Files = [
        { path: "README.md", bytes: Buffer.from(`# ${title}\n\nA reviewed R analysis run by the workspace preview's stand-in for the render service on \`${run.request.table_path}\` of an earlier analysis. Every value is simulated.\n`) },
        { path: `code/${feature.id}.R`, bytes: stand(`the ${feature.label} template`) },
        { path: "code/_common.R", bytes: stand("the shared analysis code") },
        { path: "code/params.json", bytes: Buffer.from(JSON.stringify(run.request.settings, null, 2) + "\n") },
        { path: resultsPath, bytes: Buffer.from(csvOf(outcome.results.columns, outcome.results.rows)) },
        ...(outcome.statistics ? [{ path: "results/statistics.csv", bytes: Buffer.from(csvOf(["statistic", "value"], outcome.statistics)) }] : []),
        { path: "results/record.json", bytes: Buffer.from(JSON.stringify({ method: outcome.method, notes: outcome.notes, statistics: Object.fromEntries(outcome.statistics ?? []),
          results: outcome.results }, null, 2) + "\n") },
        ...(outcome.curves?.length ? [{ path: "figures/figure.svg", bytes: Buffer.from(curveDrawing(outcome.curves, title,
          String(run.request.settings.time ?? "time"), String(run.request.settings.value ?? "value"))) }] : []),
        { path: "spec.json", bytes: Buffer.from(JSON.stringify({ schema: "enrichedlab.template-analysis-spec.v1", feature: feature.id, title: run.request.title,
          settings_asked: run.request.settings, settings_used: run.request.settings, service_version: VERSION, image_digest: IMAGE,
          table: { path: run.request.table_path, sha256: run.request.table_sha256, rows: run.request.table_rows } }, null, 2) + "\n") },
      ];
      const pkg = syntheticPackage(files, Math.floor(Date.now() / 1000));
      const made = new Date().toISOString();
      run.pkg = pkg;
      run.artifacts = [
        { id: randomUUID(), role: "result", label: "Analysis results and their reproducibility package", relative_path: `tasks/${run.task.id}/payload`,
          media_type: "application/gzip", size_bytes: pkg.archive.length, sha256: pkg.sha256, created_at: made },
        { id: randomUUID(), role: "result", label: "Package checksum", relative_path: `tasks/${run.task.id}/payload.sha256`, media_type: "text/plain",
          size_bytes: pkg.checksum.length, sha256: sha(pkg.checksum), created_at: made },
      ];
      const role = (path: string) => path.endsWith("statistics.csv") ? "statistics" : path.endsWith("record.json") ? "record" : path.startsWith("figures/") ? "figure" : "table";
      run.receipts.push({ ...base, status: "completed", error_code: null, message: null, refusal: null, method: outcome.method ?? null,
        files: listing(files).members.filter((m) => m.path.startsWith("results/") || m.path.startsWith("figures/"))
          .map(({ path, sha256, size_bytes, media_type }) => ({ path, sha256, size_bytes, media_type, role: role(path) })),
        notes: outcome.notes ?? [], package_sha256: pkg.sha256 });
      Object.assign(run.task, { status: "completed", result_class: "success", finished_at: finished, updated_at: finished });
      Object.assign(run.entry, { status: "completed", updated_at: finished });
      event(run.task, "completed", "task_completed");
    });
  }

  async function request(project: string, req: IncomingMessage, json: Json) {
    if (req.headers["x-agent-sandbox-intent"] !== "mutate") return refuse(json, "mutation_intent_required", 403);
    let raw = "";
    for await (const chunk of req) raw += chunk;
    let input: Record<string, unknown>;
    try { input = JSON.parse(raw); } catch { return refuse(json, "analysis_request_invalid"); }
    const requestId = String(input.request_id ?? "");
    if (!/^[0-9a-f-]{36}$/.test(requestId)) return refuse(json, "analysis_request_invalid");
    // The same request id with the same request answers the same analysis; with another request it is refused.
    const same = runs.find((r) => r.request_id === requestId);
    const { request_id: _, ...asked } = input;
    if (same) return same.body === JSON.stringify(asked) ? json({ task_id: same.task.id }, 202) : refuse(json, "idempotency_conflict", 409);
    const checked = check(project, input);
    if ("refusal" in checked && checked.refusal) { const [code, status] = checked.refusal; return refuse(json, code, status); }
    const { src, table, columns, rows, feature, settings, title, previous } = checked as Exclude<typeof checked, { refusal: unknown }>;
    const now = new Date().toISOString();
    const stem = table.path.replace(/^.*\//, "").replace(/\.[A-Za-z0-9]+$/, "").replaceAll("_", " ");
    const task: Task = { id: randomUUID(), group: src.group, profile, objective: title || `${feature.label} of ${stem}`, status: "queued", attempt: 0,
      max_task_attempts: 2, max_provider_attempts: 1, logical_agent: null, result_class: null, error_code: null, submitted_at: now, started_at: null,
      updated_at: now, finished_at: null };
    const entry: ProjectTask = { task_id: task.id, action_key: `analysis-${requestId}`, parent_task_id: src.sourceTask, profile, status: "queued", attempt: 0,
      submitted_at: now, updated_at: now, template_analysis: { title, feature: feature.id, source_task: src.sourceTask, table_path: table.path, previous } };
    tasks.unshift(task);
    histories.set(task.id, [{ sequence: 1, kind: "task", state: "queued", code: "task_accepted", occurred_at: now }]);
    src.addProjectTask(entry);
    const tableSha = sha(table.bytes);
    const record = { schema: "enrichedlab.template-analysis-request.v1", project, source_task: src.sourceTask, table_path: table.path, table_sha256: tableSha,
      feature: feature.id, settings, title, previous, catalog_sha256: CATALOG_SHA };
    const run: Run = { task, entry, src, request_id: requestId, body: JSON.stringify(asked), receipts: [], pkg: null, artifacts: [],
      request: { table_path: table.path, table_sha256: tableSha, table_rows: rows.length, table_columns: columns, feature: feature.id, settings, title, previous,
        request_sha256: sha(JSON.stringify(record)), created_at: now, source_package_sha256: src.source().artifact.sha256 } };
    runs.unshift(run);
    settle(run, feature, columns, rows);
    return json({ task_id: task.id }, 202);
  }

  const detail = (r: Run) => ({ task_id: r.task.id, project_id: r.src.project, source_task: r.src.sourceTask, source_title: r.src.task()?.objective ?? null,
    source_package_sha256: r.request.source_package_sha256, table_path: r.request.table_path, table_sha256: r.request.table_sha256, table_rows: r.request.table_rows,
    table_columns: r.request.table_columns, feature: r.request.feature, settings: r.request.settings, title: r.request.title, previous: r.request.previous,
    catalog_sha256: CATALOG_SHA, request_sha256: r.request.request_sha256, created_at: r.request.created_at, receipts: r.receipts });
  const summary = (r: Run) => ({ task_id: r.task.id, table_path: r.request.table_path, feature: r.request.feature, title: r.request.title, previous: r.request.previous,
    status: r.task.status, created_at: r.request.created_at, finished_at: r.task.finished_at });

  function lineage(run: Run) {
    const t = run.task, receipt = run.receipts.at(-1);
    return {
      schema: "enrichedlab.task-lineage.v1",
      task: { task_id: t.id, project_id: run.src.project, action_key: run.entry.action_key, objective: t.objective, status: t.status, result_class: t.result_class,
        error_code: t.error_code, attempt: t.attempt, submitted_at: t.submitted_at, started_at: t.started_at, finished_at: t.finished_at },
      origin: { origin: "person", basis: "submission", principal_id: me, principal_name: null, principal_kind: "person" },
      plan_step: null,
      inputs: { workspace: null, checkpoint_revision: null, platform_tool: null, classes: ["prior_output"], classes_recorded_by: "analysis_request" },
      method: { profile, profile_name: "Reviewed R analyses", tool_class: "vetted_service", template_ref: null, template_sha256: null, recorded: "submission",
        harness: null, role: null, tool_bundle: null },
      code: null, query: null, render: null, dataset: null,
      analysis: { source_task: run.src.sourceTask, table_path: run.request.table_path, table_sha256: run.request.table_sha256,
        source_package_sha256: run.request.source_package_sha256, feature: run.request.feature, settings: run.request.settings, title: run.request.title,
        previous: run.request.previous, request_sha256: run.request.request_sha256, created_at: run.request.created_at,
        service_version: receipt?.service_version ?? null, image_digest: receipt?.image_digest ?? null, method: receipt?.method ?? null },
      execution: { image_ref: null, cpu_limit: null, memory_limit: null, max_run_seconds: 900, attempts: t.attempt, scientific_history: 0 },
      outputs: run.artifacts.map((a) => ({ artifact_id: a.id, role: a.role, label: a.label, relative_path: a.relative_path, media_type: a.media_type,
        size_bytes: a.size_bytes, sha256: a.sha256, created_at: a.created_at, provenance_class: "vetted_tool_output" })),
      review: { state: "unvalidated" },
      gaps: [],
    };
  }

  return {
    owns: (id: string) => !!own(id),
    /** Finished template analyses' results tables, as sources a figure can be drawn from. */
    results,
    reset() {
      for (const t of timers) clearTimeout(t);
      timers.clear();
      for (const r of runs) { const i = tasks.findIndex((t) => t.id === r.task.id); if (i >= 0) tasks.splice(i, 1); histories.delete(r.task.id); }
      runs.length = 0;
    },
    async handle(req: IncomingMessage, res: ServerResponse, url: URL, json: Json, denied: () => void): Promise<boolean> {
      const path = url.pathname;
      if (path === "/api/v1/template-analyses/catalog" && req.method === "GET") {
        json({ catalog: { catalog_sha256: CATALOG_SHA, service_version: VERSION, image_digest: IMAGE, features: ANALYSIS_FEATURES, seen_at: SEEN, groups } });
        return true;
      }
      const asked = path.match(/^\/api\/v1\/projects\/([0-9a-f-]{36})\/template-analyses$/);
      if (asked) {
        if (req.method !== "POST") { denied(); return true; }
        await request(asked[1], req, json);
        return true;
      }
      const taskRoute = path.match(/^\/api\/v1\/tasks\/([0-9a-f-]{36})(\/.*)$/);
      if (!taskRoute) return false;
      const [, id, suffix] = taskRoute;
      const run = own(id);
      if (suffix === "/template-analysis") {
        if (run) json({ template_analysis: detail(run) });
        else if (known(id)) json({ template_analysis: null });
        else refuse(json, "task_not_found", 404);
        return true;
      }
      if (suffix === "/template-analyses") {
        if (run || known(id)) json({ template_analyses: runs.filter((r) => r.src.sourceTask === id).map(summary) });
        else refuse(json, "task_not_found", 404);
        return true;
      }
      if (!run) return false;
      if (suffix === "/artifacts") { json({ artifacts: run.artifacts }); return true; }
      if (suffix === "/lineage") { json(lineage(run)); return true; }
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
