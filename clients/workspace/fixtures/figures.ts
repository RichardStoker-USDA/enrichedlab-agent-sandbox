import { createHash, randomUUID } from "node:crypto";
import type { IncomingMessage, ServerResponse } from "node:http";
import type { Artifact, FigureControls, FigureFeature, FigureParam, FigureReceipt, Task, TaskEvent } from "../src/contracts.ts";
import { csvRows, listing, syntheticPackage } from "./package.ts";
import type { ProjectTask } from "./projects.ts";
import { answerTable } from "./tables.ts";

// Figure versions for the loopback preview: the render service's catalog, a person's request checked the
// way the broker checks it, and a stand-in for the service that draws a simple SVG from the simulated table. The
// stand-in is not R and says so in every drawing and package it writes; on the cluster the reviewed R templates draw the
// figure. Previews are drawn the same way and stored nowhere; one version per
// figure can carry the manuscript mark.

type Json = (value: unknown, status?: number) => void;
type Files = { path: string; bytes: Buffer }[];
const sha = (bytes: Buffer) => createHash("sha256").update(bytes).digest("hex");

// The settings release 0.5.19 declares for every figure (the render service's app.py), with the service's own words,
// and the journal settings (subtitle, legend title, colours per group).
const TEXT: Record<string, FigureParam> = {
  x_label: { type: "string", description: "x-axis title" },
  y_label: { type: "string", description: "y-axis title" },
  subtitle: { type: "string", maxlen: 200, description: "a line under the title" },
};
const SIZE: Record<string, FigureParam> = {
  width: { type: "number", min: 3, max: 20, description: "figure width in inches" },
  height: { type: "number", min: 2, max: 20, description: "figure height in inches" },
  dpi: { type: "number", min: 72, max: 600, description: "PNG and TIFF resolution in dots per inch" },
  format: { type: "enum", choices: ["png", "svg", "pdf", "tiff"], description: "an extra export: pdf or tiff (PNG and SVG are always made)" },
  font: { type: "enum", choices: ["sans", "serif", "mono"], description: "typeface for all figure text" },
  base_size: { type: "number", min: 9, max: 20, description: "overall text size" },
};
const COMMON: Record<string, FigureParam> = {
  ...TEXT,
  log_x: { type: "enum", choices: ["true", "false"], description: "log10 scale on the x axis" },
  log_y: { type: "enum", choices: ["true", "false"], description: "log10 scale on the y axis" },
  x_limits: { type: "numbers", maxlen: 2, description: "x-axis view window [min, max]; zooms, never drops data" },
  y_limits: { type: "numbers", maxlen: 2, description: "y-axis view window [min, max]; zooms, never drops data" },
  hline: { type: "numbers", maxlen: 6, description: "horizontal reference lines" },
  vline: { type: "numbers", maxlen: 6, description: "vertical reference lines" },
  legend_position: { type: "enum", choices: ["right", "left", "top", "bottom", "none"], description: "where the legend sits, or none" },
  legend_title: { type: "string", maxlen: 80, description: "legend title" },
  ...SIZE,
};
// Okabe and Ito's colours and a grey, which read with colour blindness.
export const GROUP_COLOURS = ["#E69F00", "#56B4E9", "#009E73", "#F0E442", "#0072B2", "#D55E00", "#CC79A7", "#000000", "#999999"];
const groupColours = (of: string): FigureParam => ({ type: "colors", of, maxlen: 12, choices: GROUP_COLOURS, description: `a colour for each group of the ${of} column` });
const PALETTE: Record<string, FigureParam> = {
  palette: { type: "enum", choices: ["clinical", "CCcolors", "sexpair", "viridis"], description: "colours for the bars, groups or series" },
};
const column = (description: string, required = true): FigureParam => ({ type: "column", required, description });
export const FIGURE_FEATURES: FigureFeature[] = [
  { id: "scatter", label: "Scatter plot", description: "x vs y points, optional color grouping (e.g. expression vs phenotype, position vs LOD).",
    params: { ...COMMON, ...PALETTE, x: column("x-axis column"), y: column("y-axis column"), color: column("optional column to color points by", false),
      group_colors: groupColours("color") } },
  { id: "bar", label: "Bar chart", description: "one bar per category (e.g. counts or means per study/sex/strain).",
    params: { ...COMMON, ...PALETTE, category: column("category column"), value: column("numeric value column"),
      orientation: { type: "enum", choices: ["vertical", "horizontal"], default: "vertical" }, group_colors: groupColours("category") } },
  { id: "histogram", label: "Histogram", description: "distribution of one numeric column.",
    params: { ...COMMON, ...PALETTE, value: column("numeric column"), bins: { type: "number", default: 30, min: 5, max: 150, description: "number of bins" } } },
  { id: "volcano", label: "Volcano plot", description: "effect size vs significance (e.g. log fold-change vs -log10 p, or effect vs LOD).",
    params: { ...COMMON, effect: column("effect size / log fold-change column"), significance: column("-log10 p (or LOD) column"),
      label: column("optional gene/feature label column", false), sig_threshold: { type: "number", description: "horizontal significance cutoff" },
      effect_threshold: { type: "number", description: "vertical |effect| cutoff" } } },
  // R's persp() of a grid table as render service 0.6.0 declares it; theta and phi are
  // persp()'s viewing angles. It takes only the common settings it draws.
  { id: "surface", label: "Surface", description: "a surface over an x-y grid with z as height (base R persp).",
    params: { ...TEXT, ...SIZE, x: column("x-axis column"), y: column("depth column"), z: column("height column"),
      theta: { type: "number", min: -180, max: 180, default: 30, description: "angle around the surface in degrees (persp theta)" },
      phi: { type: "number", min: 0, max: 90, default: 25, description: "angle above the floor in degrees (persp phi)" },
      expand: { type: "number", min: 0.1, max: 10, default: 1, description: "vertical expansion (persp expand)" },
      z_label: { type: "string", maxlen: 80, description: "z-axis title" },
      palette: { type: "enum", choices: ["viridis", "grey"], description: "colours for the heights" } } },
];
const MEASURED = new Set(["y", "z", "value", "effect", "significance"]);
const VERSION = "0.6.0";
const FACES: Record<string, string> = { sans: "Helvetica", serif: "Times", mono: "Courier" };
const IMAGE = `sha256:${"5".repeat(64)}`;

// The stand-in's drawing: axes, the table's points or bars, the titles and a legend where one was asked for, or a surface
// seen from persp()'s angle. Every drawing says in its corner that the stand-in drew it, not R.
// The service's grey palette for a surface, dark to light.
const GREYS = ["#1a1a1a", "#2e2e2e", "#424242", "#575757", "#6b6b6b", "#808080", "#949494", "#a8a8a8", "#bdbdbd", "#d1d1d1"];
const VIRIDIS = ["#440154", "#482878", "#3e4989", "#31688e", "#26828e", "#1f9e89", "#35b779", "#6ece58", "#b5de2b", "#fde725"];
function drawing(feature: string, used: Record<string, unknown>, columns: string[], rows: string[][], title: string): string {
  const inch = 72, w = Number(used.width ?? 7) * inch, h = Number(used.height ?? 5) * inch;
  const font = { sans: "Helvetica, Arial, sans-serif", serif: "Georgia, serif", mono: "Menlo, monospace" }[String(used.font ?? "sans")];
  const size = Number(used.base_size ?? 12);
  const legend = String(used.legend_position ?? "right");
  const subtitle = String(used.subtitle ?? "");
  const at = (name: string) => columns.indexOf(String(used[name] ?? ""));
  const num = (row: string[], i: number) => Number(row[i]);
  const esc = (s: string) => s.replace(/[&<>"]/g, (c) => ({ "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;" })[c]!);
  const chosen = (used.group_colors ?? {}) as Record<string, string>;
  const head = (title ? `<text x="16" y="26" font-size="${size + 3}" font-weight="600">${esc(title)}</text>` : "")
    + (subtitle ? `<text x="16" y="${title ? 44 : 26}" font-size="${size - 1}" fill="#4d4d4d">${esc(subtitle)}</text>` : "");
  const top0 = (title ? 40 : 16) + (subtitle ? 18 : 0);
  const mark = `<text x="${w - 8}" y="${h - 6}" text-anchor="end" font-size="9" fill="#8c8c8c">Workspace preview stand-in, not drawn by R</text>`;
  const open = `<svg xmlns="http://www.w3.org/2000/svg" width="${w}" height="${h}" viewBox="0 0 ${w} ${h}" font-family="${font}" font-size="${size}" fill="#1a1a1a">`
    + `<rect width="${w}" height="${h}" fill="#ffffff"/>`;
  if (feature === "surface") return open + head + surface(used, columns, rows, w, h, top0) + mark + "</svg>\n";
  const groupCol = feature === "scatter" ? at("color") : -1;
  const groups = groupCol >= 0 ? [...new Set(rows.map((r) => r[groupCol]))] : [];
  const colours = ["#022851", "#008eaa", "#266041", "#6884a3", "#79242f"];
  const colourOf = (group: string, i: number) => chosen[group] ?? colours[i % colours.length];
  const side = groups.length && legend !== "none" ? legend : "none";
  const keyTitle = String(used.legend_title ?? used.color ?? "");
  const left = 64 + (side === "left" ? 120 : 0), right = 20 + (side === "right" ? 120 : 0);
  const top = top0 + (side === "top" ? 28 : 0), bottom = 52 + (side === "bottom" ? 28 : 0);
  const pw = w - left - right, ph = h - top - bottom;
  let xs: number[] = [], ys: number[] = [], marks = "";
  const xLabel = String(used.x_label ?? (feature === "bar" ? used.category : feature === "volcano" ? used.effect : feature === "histogram" ? used.value : used.x));
  const yLabel = String(used.y_label ?? (feature === "bar" ? used.value : feature === "volcano" ? used.significance : feature === "histogram" ? "count" : used.y));
  const scale = (v: number, lo: number, hi: number, len: number) => (hi === lo ? len / 2 : ((v - lo) / (hi - lo)) * len);
  const range = (values: number[], limits: unknown) => Array.isArray(limits) ? [Number(limits[0]), Number(limits[1])]
    : [Math.min(...values), Math.max(...values)];
  if (feature === "bar") {
    const c = at("category"), v = at("value"), values = rows.map((r) => num(r, v));
    const [, hi] = range([0, ...values], used.y_limits), band = pw / Math.max(rows.length, 1);
    marks = rows.map((r, i) => {
      const bh = scale(num(r, v), 0, hi, ph);
      return `<rect x="${(left + i * band + band * 0.15).toFixed(1)}" y="${(top + ph - bh).toFixed(1)}" width="${(band * 0.7).toFixed(1)}" height="${bh.toFixed(1)}" fill="${chosen[r[c]] ?? "#022851"}"/>`
        + `<text x="${(left + i * band + band / 2).toFixed(1)}" y="${top + ph + size + 4}" text-anchor="middle">${esc(r[c])}</text>`;
    }).join("");
    ys = [0, hi];
  } else if (feature === "histogram") {
    const v = at("value"), values = rows.map((r) => num(r, v)).filter(Number.isFinite), bins = Number(used.bins ?? 30);
    const [lo, hi] = range(values, used.x_limits), counts = new Array(bins).fill(0);
    for (const value of values) counts[Math.min(bins - 1, Math.max(0, Math.floor(((value - lo) / (hi - lo || 1)) * bins)))] += 1;
    const most = Math.max(...counts), band = pw / bins;
    marks = counts.map((n, i) => { const bh = scale(n, 0, most, ph);
      return `<rect x="${(left + i * band).toFixed(1)}" y="${(top + ph - bh).toFixed(1)}" width="${(band - 1).toFixed(1)}" height="${bh.toFixed(1)}" fill="#022851"/>`; }).join("");
    xs = [lo, hi]; ys = [0, most];
  } else {
    const xi = feature === "volcano" ? at("effect") : at("x"), yi = feature === "volcano" ? at("significance") : at("y");
    const points = rows.map((r) => [num(r, xi), num(r, yi), groupCol >= 0 ? groups.indexOf(r[groupCol]) : 0] as const)
      .filter(([x, y]) => Number.isFinite(x) && Number.isFinite(y));
    const [x0, x1] = range(points.map((p) => p[0]), used.x_limits), [y0, y1] = range(points.map((p) => p[1]), used.y_limits);
    marks = points.map(([x, y, g]) => `<circle cx="${(left + scale(x, x0, x1, pw)).toFixed(1)}" cy="${(top + ph - scale(y, y0, y1, ph)).toFixed(1)}" r="3.2" fill="${colourOf(groups[g] ?? "", g)}" fill-opacity="0.85"/>`).join("");
    for (const line of (used.hline as number[] | undefined) ?? []) marks += `<line x1="${left}" x2="${left + pw}" y1="${(top + ph - scale(line, y0, y1, ph)).toFixed(1)}" y2="${(top + ph - scale(line, y0, y1, ph)).toFixed(1)}" stroke="#79242f" stroke-dasharray="4 3"/>`;
    for (const line of (used.vline as number[] | undefined) ?? []) marks += `<line y1="${top}" y2="${top + ph}" x1="${(left + scale(line, x0, x1, pw)).toFixed(1)}" x2="${(left + scale(line, x0, x1, pw)).toFixed(1)}" stroke="#79242f" stroke-dasharray="4 3"/>`;
    xs = [x0, x1]; ys = [y0, y1];
  }
  const tick = (v: number) => (Math.abs(v) >= 1000 || (Math.abs(v) < 0.01 && v !== 0) ? v.toExponential(1) : Number(v.toPrecision(3)).toString());
  const ticks = (lo: number, hi: number) => [0, 0.25, 0.5, 0.75, 1].map((f) => lo + f * (hi - lo));
  const xTicks = xs.length ? ticks(xs[0], xs[1]).map((v, i) => `<text x="${(left + (i / 4) * pw).toFixed(1)}" y="${top + ph + size + 4}" text-anchor="middle">${tick(v)}</text>`).join("") : "";
  const yTicks = ticks(ys[0], ys[1]).map((v, i) => `<text x="${left - 6}" y="${(top + ph - (i / 4) * ph + 4).toFixed(1)}" text-anchor="end">${tick(v)}</text>`).join("");
  const keyAt = (i: number) => side === "right" ? [w - right + 14, top + 28 + i * 18] : side === "left" ? [14, top + 28 + i * 18]
    : side === "top" ? [left + 90 + i * 110, top0 + 8] : [left + 90 + i * 110, h - 14];
  const key = (keyTitle ? (() => { const [x, y] = side === "right" || side === "left" ? [keyAt(0)[0], top + 10] : [left, keyAt(0)[1] + 4];
      return `<text x="${x}" y="${y}" font-weight="600">${esc(keyTitle)}</text>`; })() : "")
    + groups.map((g, i) => { const [x, y] = keyAt(i);
      return `<circle cx="${x + 5}" cy="${y}" r="4" fill="${colourOf(g, i)}"/><text x="${x + 14}" y="${y + 4}">${esc(g)}</text>`; }).join("");
  return open + head
    + `<line x1="${left}" y1="${top + ph}" x2="${left + pw}" y2="${top + ph}" stroke="#333"/><line x1="${left}" y1="${top}" x2="${left}" y2="${top + ph}" stroke="#333"/>`
    + marks + xTicks + yTicks + (side === "none" ? "" : key)
    + `<text x="${left + pw / 2}" y="${h - (side === "bottom" ? 34 : 10)}" text-anchor="middle">${esc(xLabel)}</text>`
    + `<text transform="translate(16 ${top + ph / 2}) rotate(-90)" text-anchor="middle">${esc(yLabel)}</text>` + mark + "</svg>\n";
}

// The stand-in's surface: each grid cell with all four corners as a quadrilateral, far cells first, coloured by height,
// seen from theta around and phi above as persp() takes them (theta 0 from the lowest y, theta 90 from the highest x).
function surface(used: Record<string, unknown>, columns: string[], rows: string[][], w: number, h: number, top: number): string {
  const [xi, yi, zi] = ["x", "y", "z"].map((name) => columns.indexOf(String(used[name] ?? "")));
  const points = rows.map((r) => [Number(r[xi]), Number(r[yi]), Number(r[zi])]).filter((p) => p.every(Number.isFinite));
  const xs = [...new Set(points.map((p) => p[0]))].sort((a, b) => a - b), ys = [...new Set(points.map((p) => p[1]))].sort((a, b) => a - b);
  const z = new Map(points.map(([x, y, v]) => [`${x}|${y}`, v]));
  const zs = points.map((p) => p[2]), lo = Math.min(...zs), hi = Math.max(...zs);
  const theta = (Number(used.theta ?? 30) * Math.PI) / 180, phi = (Number(used.phi ?? 25) * Math.PI) / 180, expand = Number(used.expand ?? 1);
  const shade = (level: number) => (used.palette === "grey" ? GREYS : VIRIDIS)[Math.min(9, Math.floor(level * 10))];
  const unit = (v: number, a: number, b: number) => (b === a ? 0 : ((v - a) / (b - a)) * 2 - 1);
  const project = (x: number, y: number, v: number) => {
    const X = unit(x, xs[0], xs.at(-1)!), Y = unit(y, ys[0], ys.at(-1)!), Z = unit(v, lo, hi) * expand;
    const sx = X * Math.cos(theta) + Y * Math.sin(theta), depth = -X * Math.sin(theta) + Y * Math.cos(theta);
    return { sx, sy: Z * Math.cos(phi) + depth * Math.sin(phi), depth };
  };
  const cells: { depth: number; pts: { sx: number; sy: number }[]; level: number }[] = [];
  for (let i = 0; i + 1 < xs.length; i++) for (let j = 0; j + 1 < ys.length; j++) {
    const corners = [[xs[i], ys[j]], [xs[i + 1], ys[j]], [xs[i + 1], ys[j + 1]], [xs[i], ys[j + 1]]];
    const values = corners.map(([x, y]) => z.get(`${x}|${y}`));
    if (values.some((v) => v === undefined)) continue;
    const pts = corners.map(([x, y], k) => project(x, y, values[k]!));
    cells.push({ depth: pts.reduce((sum, p) => sum + p.depth, 0) / 4, pts, level: (values.reduce((a, b) => a! + b!, 0)! / 4 - lo) / (hi - lo || 1) });
  }
  cells.sort((a, b) => b.depth - a.depth);
  const pad = 36, span = Math.max(1.6, ...cells.flatMap((c) => c.pts.map((p) => Math.max(Math.abs(p.sx), Math.abs(p.sy)))));
  const s = Math.min((w - 2 * pad) / (2 * span), (h - top - 2 * pad) / (2 * span)), cx = w / 2, cy = top + (h - top) / 2;
  return cells.map((c) => `<polygon points="${c.pts.map((p) => `${(cx + p.sx * s).toFixed(1)},${(cy - p.sy * s).toFixed(1)}`).join(" ")}" fill="${shade(c.level)}" stroke="#1a1a1a" stroke-width="0.3"/>`).join("")
    + `<text x="${pad}" y="${h - 22}" font-size="10" fill="#4d4d4d">theta ${Number(used.theta ?? 30)}, phi ${Number(used.phi ?? 25)}, expand ${expand}</text>`;
}

// What the stand-in does with the settings: clamp numbers to their range and reorder a reversed range, with notes in the
// service's words, and refuse a measured column that holds text as the broker words a known refusal:
// its own sentence, filled only with the table's columns, beside the refusal's code, columns and counts.
function serve(feature: FigureFeature, controls: FigureControls, columns: string[], rows: string[][]) {
  const used: Record<string, unknown> = {};
  const notes: string[] = [];
  const measured = feature.id === "surface" ? new Set(["x", "y", "z"]) : MEASURED;
  for (const [name, value] of Object.entries(controls)) {
    const param = feature.params[name];
    // A colour for a group the grouping column does not hold is left out, with a note.
    if (param.type === "colors" && value && typeof value === "object" && !Array.isArray(value)) {
      const of = columns.indexOf(String(controls[param.of ?? ""] ?? ""));
      const held = new Set(of >= 0 ? rows.map((r) => r[of]) : []);
      for (const group of Object.keys(value)) if (!held.has(group)) notes.push(`'${name}' names ${group}, which the table does not hold; it was left out`);
      const kept = Object.fromEntries(Object.entries(value).filter(([group]) => held.has(group)));
      if (Object.keys(kept).length) used[name] = kept;
      continue;
    }
    if (param.type === "number" && typeof value === "number") {
      const n = Math.min(param.max ?? value, Math.max(param.min ?? value, value));
      if (n !== value) notes.push(`'${name}' ${value} is outside ${param.min} to ${param.max}; ${n} was used`);
      used[name] = n;
    } else if (param.type === "numbers" && Array.isArray(value) && /_limits$/.test(name) && Number(value[0]) > Number(value[1])) {
      notes.push(`'${name}' was given as maximum then minimum and was reordered`);
      used[name] = [value[1], value[0]];
    } else used[name] = value;
    // R reads an empty cell or NA as missing; a measured column with no number left cannot be drawn.
    if (param.type === "column" && measured.has(name)) {
      const i = columns.indexOf(String(value));
      if (!rows.some((r) => r[i] !== "" && r[i] !== "NA" && Number.isFinite(Number(r[i]))))
        return { refused: `No numbers this figure can draw in ${value}.`, refusal: { code: "no_numeric_values", columns: [String(value)], counts: [] as number[] }, used, notes };
    }
  }
  if (feature.params.font) used.font_face = FACES[String(used.font ?? "sans")];
  return { refused: null, refusal: null, used, notes };
}

/** A finished analysis whose result tables can be drawn, in its project and lab. */
export interface FigureSource {
  project: string; group: string; sourceTask: string;
  source: () => { files: Files; artifact: Artifact };
  /** The source analysis as its own task record holds it. */
  task: () => { status: string; objective: string } | null;
  addProjectTask: (entry: ProjectTask) => void;
}

export function figureFixtures(options: {
  /** The tables figures are drawn from; a function when tables arrive while the preview runs (prepared lab data). */
  me: string; profile: string; sources: FigureSource[] | (() => FigureSource[]);
  tasks: Task[]; histories: Map<string, TaskEvent[]>;
  /** Analyses other modules serve: the figure routes answer them as analyses with no figure. */
  knows?: (task: string) => boolean;
  /** How long a preview waits for its drawing, as a compute worker takes a moment to draw it. */
  previewDelay?: number;
}) {
  const { me, profile, tasks, histories, knows = () => false, previewDelay = 300 } = options;
  const listed = () => (typeof options.sources === "function" ? options.sources() : options.sources);
  type Version = { src: FigureSource; task: Task; entry: ProjectTask; request: { table_path: string; table_sha256: string; table_rows: number; table_columns: string[];
    feature: string; controls: FigureControls; title: string; previous: string | null; request_sha256: string; created_at: string; request_id: string; body: string };
    receipts: FigureReceipt[]; pkg: ReturnType<typeof syntheticPackage> | null; artifacts: Artifact[] };
  const versions: Version[] = [];
  // The version marked for the manuscript, by figure; a figure is named by its first version.
  const marks = new Map<string, { figure: string; version: string; marked_by: string; marked_at: string }>();
  // When previews were asked for in the last minute, for the limit of 30. A compute worker draws each preview, so one
  // takes a moment, and a person has at most one waiting.
  const previewTimes: number[] = [];
  let previewWaiting = false, previewAnswer: Json | null = null;
  const timers = new Set<ReturnType<typeof setTimeout>>();
  const features = FIGURE_FEATURES;
  const catalogSha = sha(Buffer.from(JSON.stringify(features)));
  const seenAt = new Date().toISOString();
  const body = async (req: IncomingMessage) => { let text = ""; for await (const chunk of req) text += chunk; return text; };
  const refuse = (json: Json, code: string, status = 400) => json({ error: { code, message: "Refused" } }, status);
  const own = (id: string) => versions.find((v) => v.task.id === id);
  const known = (id: string) => tasks.some((t) => t.id === id) || listed().some((x) => x.sourceTask === id) || knows(id);
  const rootOf = (v: Version): string => { let at = v; while (at.request.previous) { const up = own(at.request.previous); if (!up) break; at = up; } return at.task.id; };
  const markOf = (v: Version) => marks.get(rootOf(v)) ?? null;

  function event(task: Task, state: string, code: string) {
    const history = histories.get(task.id)!;
    history.push({ sequence: history.length + 1, kind: "task", state, code, occurred_at: new Date().toISOString() });
  }
  function settle(v: Version) {
    const later = (delay: number, work: () => void) => { const t = setTimeout(() => { timers.delete(t); work(); }, delay); timers.add(t); };
    later(500, () => {
      const now = new Date().toISOString();
      Object.assign(v.task, { status: "running", attempt: 1, started_at: now, updated_at: now, logical_agent: "render-worker" });
      Object.assign(v.entry, { status: "running", attempt: 1, updated_at: now });
      event(v.task, "running", "worker_lease_acquired");
    });
    later(1400, () => {
      const table = v.src.source().files.find((f) => f.path === v.request.table_path)!;
      const [columns = [], ...rows] = csvRows(table.bytes.toString("utf8"));
      const feature = features.find((f) => f.id === v.request.feature)!;
      const outcome = serve(feature, v.request.controls, columns, rows);
      const started = v.task.started_at!, finished = new Date().toISOString();
      if (outcome.refused) {
        v.receipts.push({ attempt: 1, status: "refused", error_code: "figure_refused", message: outcome.refused, service_version: VERSION,
          image_digest: IMAGE, files: null, notes: outcome.notes, package_sha256: null, started_at: started, finished_at: finished });
        Object.assign(v.task, { status: "failed", result_class: "figure_not_drawn", error_code: "figure_refused", finished_at: finished, updated_at: finished });
        Object.assign(v.entry, { status: "failed", updated_at: finished });
        event(v.task, "failed", "task_failed");
        return;
      }
      const svg = Buffer.from(drawing(feature.id, outcome.used, columns, rows, v.request.title));
      const stand = (what: string) => Buffer.from(`# Preview stand-in for ${what}.\n# On the cluster this file is the reviewed R code that drew the figure; the preview draws a simple SVG instead.\n`);
      const files: Files = [
        { path: "README.md", bytes: Buffer.from(`# ${v.request.title || "Figure"}\n\nA figure version drawn by the workspace preview's stand-in for the render service from \`${v.request.table_path}\` of an earlier analysis. Every value is simulated.\n`) },
        { path: `code/${feature.id}.R`, bytes: stand(`the ${feature.label.toLowerCase()} template`) },
        { path: "code/_common.R", bytes: stand("the shared figure code") },
        { path: "code/params.json", bytes: Buffer.from(JSON.stringify(outcome.used, null, 2) + "\n") },
        { path: "figures/figure.svg", bytes: svg },
        { path: "spec.json", bytes: Buffer.from(JSON.stringify({ schema: "enrichedlab.figure-spec.v1", feature: feature.id, title: v.request.title,
          settings_asked: v.request.controls, settings_used: outcome.used, notes: outcome.notes, service_version: VERSION,
          table: { path: v.request.table_path, sha256: v.request.table_sha256, rows: v.request.table_rows } }, null, 2) + "\n") },
      ];
      const pkg = syntheticPackage(files, Math.floor(Date.now() / 1000));
      const made = new Date().toISOString();
      v.pkg = pkg;
      v.artifacts = [
        { id: randomUUID(), role: "result", label: "Figure and its reproducibility package", relative_path: `tasks/${v.task.id}/payload`, media_type: "application/gzip",
          size_bytes: pkg.archive.length, sha256: pkg.sha256, created_at: made },
        { id: randomUUID(), role: "result", label: "Package checksum", relative_path: `tasks/${v.task.id}/payload.sha256`, media_type: "text/plain",
          size_bytes: pkg.checksum.length, sha256: sha(pkg.checksum), created_at: made },
      ];
      v.receipts.push({ attempt: 1, status: "completed", error_code: null, message: null, service_version: VERSION, image_digest: IMAGE,
        files: listing(files).members.filter((m) => m.path.startsWith("figures/")).map(({ path, sha256, size_bytes, media_type }) => ({ path, sha256, size_bytes, media_type })),
        notes: outcome.notes, package_sha256: pkg.sha256, started_at: started, finished_at: finished });
      Object.assign(v.task, { status: "completed", result_class: "success", finished_at: finished, updated_at: finished });
      Object.assign(v.entry, { status: "completed", updated_at: finished });
      event(v.task, "completed", "task_completed");
    });
  }

  // The broker's checks (migration 040), shared by versions and previews: the project and a finished source in it, a
  // result table the source's digest lists, a chart in the catalog, and each setting one the chart declares with a
  // value of its kind. Each failure is a refusal code and status.
  function check(project: string, input: Record<string, unknown>) {
    const src = listed().find((x) => x.project === project && x.sourceTask === input.source_task);
    const task = src?.task();
    if (!src || !task) return { refusal: ["task_not_found", 404] as const };
    if (task.status !== "completed") return { refusal: ["figure_source_not_ready", 409] as const };
    // A result table, or a lab table a preparation wrote.
    const table = src.source().files.find((f) => f.path === input.table_path && /^results\/(lab_data\/|lab_derived\/)?[^/]+\.csv$/.test(f.path));
    if (!table) return { refusal: ["figure_table_unknown", 400] as const };
    const [columns = [], ...rows] = csvRows(table.bytes.toString("utf8"));
    if (!rows.length || rows.length > 50_000 || columns.length > 64) return { refusal: ["figure_table_unsuitable", 400] as const };
    const feature = features.find((f) => f.id === input.feature);
    if (!feature) return { refusal: ["figure_feature_unknown", 400] as const };
    const controls = (input.controls ?? {}) as FigureControls;
    const title = String(input.title ?? "").trim();
    for (const [name, value] of Object.entries(controls)) {
      const param = feature.params[name];
      const ok = param && (param.type === "column" ? typeof value === "string" && columns.includes(value)
        : param.type === "number" ? typeof value === "number"
        : param.type === "numbers" ? Array.isArray(value) && value.every((n) => typeof n === "number")
        : param.type === "enum" ? (param.choices ?? []).some((c) => String(c).toLowerCase() === String(value).toLowerCase())
        : param.type === "string" ? typeof value === "string" && value.length <= 200
        : param.type === "colors" ? !!value && typeof value === "object" && !Array.isArray(value) && Object.keys(value).length <= (param.maxlen ?? 12)
          && Object.values(value).every((hex) => (param.choices ?? []).some((c) => String(c).toLowerCase() === String(hex).toLowerCase()))
        : false);
      if (!ok) return { refusal: ["figure_settings_invalid", 400] as const };
    }
    if (Object.entries(feature.params).some(([name, p]) => p.required && p.type === "column" && !(name in controls))) return { refusal: ["figure_settings_invalid", 400] as const };
    return { src, task, table, columns, rows, feature, controls, title };
  }

  // A version: the checks, then a previous version of the same table, and a request id that makes a repeat one version.
  function request(project: string, input: Record<string, unknown>, raw: string, json: Json) {
    const checked = check(project, input);
    if (checked.refusal) return refuse(json, checked.refusal[0], checked.refusal[1]);
    const { src, table, columns, rows, feature, controls, title } = checked;
    const previous = typeof input.previous === "string" ? input.previous : null;
    // The same analysis of the same project; a listing builds its sources again, so they are compared by name.
    if (previous && !versions.some((v) => v.task.id === previous && v.src.project === src.project && v.src.sourceTask === src.sourceTask
      && v.request.table_path === table.path)) return refuse(json, "figure_previous_invalid");
    const requestId = String(input.request_id ?? "");
    const same = versions.find((v) => v.request.request_id === requestId);
    if (same) return same.request.body === raw ? json({ task_id: same.task.id }, 202) : refuse(json, "idempotency_conflict", 409);
    const now = new Date().toISOString();
    const objective = title || `${feature.label} of ${table.path.replace(/^.*\//, "").replace(/\.[a-z0-9]+$/i, "").replace(/_/g, " ")}`;
    const created: Task = { id: randomUUID(), group: src.group, profile, objective, status: "queued", attempt: 0, max_task_attempts: 2, max_provider_attempts: 1,
      logical_agent: null, result_class: null, error_code: null, submitted_at: now, started_at: null, updated_at: now, finished_at: null };
    const entry: ProjectTask = { task_id: created.id, action_key: `figure-${requestId}`, parent_task_id: src.sourceTask, profile, status: "queued", attempt: 0,
      submitted_at: now, updated_at: now, figure: { title: objective, feature: feature.id, source_task: src.sourceTask, table_path: table.path, previous } };
    const record = { table_path: table.path, table_sha256: sha(table.bytes), table_rows: rows.length, table_columns: columns, feature: feature.id, controls,
      title, previous, created_at: now, request_id: requestId, body: raw,
      request_sha256: sha(Buffer.from(JSON.stringify({ project, source_task: src.sourceTask, table_path: table.path, feature: feature.id, controls, title, previous }))) };
    const version: Version = { src, task: created, entry, request: record, receipts: [], pkg: null, artifacts: [] };
    versions.push(version);
    tasks.unshift(created);
    histories.set(created.id, [{ sequence: 1, kind: "task", state: "queued", code: "task_accepted", occurred_at: now }]);
    src.addProjectTask(entry);
    settle(version);
    return json({ task_id: created.id }, 202);
  }

  // A preview: the same checks and drawing as a version, answered at once and stored nowhere.
  function preview(project: string, input: Record<string, unknown>, json: Json) {
    const now = Date.now();
    while (previewTimes.length && now - previewTimes[0] > 60_000) previewTimes.shift();
    if (previewWaiting || previewTimes.length >= 30) return refuse(json, "capacity_reached", 429);
    previewTimes.push(now);
    const checked = check(project, input);
    if (checked.refusal) return refuse(json, checked.refusal[0], checked.refusal[1]);
    previewWaiting = true;
    previewAnswer = json;
    const t = setTimeout(() => { timers.delete(t); previewWaiting = false; previewAnswer = null; drawPreview(checked, json); }, previewDelay);
    timers.add(t);
  }
  function drawPreview(checked: Exclude<ReturnType<typeof check>, { refusal: unknown }>, json: Json) {
    const { table, columns, rows, feature, controls, title } = checked;
    const outcome = serve(feature, controls, columns, rows);
    const common = { notes: outcome.notes, settings_used: outcome.used, service_version: VERSION, image_digest: IMAGE, catalog_sha256: catalogSha,
      table_sha256: sha(table.bytes) };
    if (outcome.refused) return json({ preview: null, refused: outcome.refused, refusal: outcome.refusal, ...common });
    const svg = Buffer.from(drawing(feature.id, outcome.used, columns, rows, title));
    if (svg.length > 2 * 1024 * 1024) return refuse(json, "figure_preview_too_large", 413);
    return json({ preview: { media_type: "image/svg+xml", sha256: sha(svg), size_bytes: svg.length, svg_base64: svg.toString("base64") }, refused: null, refusal: null, ...common });
  }

  // The manuscript mark: one drawn version per figure.
  function mark(project: string, figure: string, input: Record<string, unknown>, json: Json) {
    const root = own(figure);
    if (!root || root.src.project !== project || root.request.previous !== null) return refuse(json, "figure_not_found", 404);
    const chosen = own(String(input.version ?? ""));
    if (!chosen || rootOf(chosen) !== figure) return refuse(json, "version_not_in_figure", 409);
    if (chosen.task.status !== "completed") return refuse(json, "version_not_drawn", 409);
    const record = { figure, version: chosen.task.id, marked_by: me, marked_by_name: null, marked_at: new Date().toISOString() };
    marks.set(figure, record);
    event(chosen.task, "completed", "figure_manuscript_marked");
    return json(record);
  }

  function detail(v: Version) {
    return { task_id: v.task.id, project_id: v.src.project, source_task: v.src.sourceTask,
      source_title: v.src.task()?.objective ?? null, source_package_sha256: v.src.source().artifact.sha256,
      table_path: v.request.table_path, table_sha256: v.request.table_sha256, table_rows: v.request.table_rows, table_columns: v.request.table_columns,
      feature: v.request.feature, controls: v.request.controls, title: v.request.title, previous: v.request.previous, catalog_sha256: catalogSha,
      request_sha256: v.request.request_sha256, created_at: v.request.created_at, receipts: v.receipts, manuscript: markOf(v), figure: rootOf(v) };
  }

  // Lineage version 1 for a figure version, as migration 041 reads it: a person's request, the prior output it drew
  // and the vetted render service; no plan step or workspace of its own. Only a drawn version without files is a gap.
  function lineage(v: Version) {
    const t = v.task;
    return {
      schema: "enrichedlab.task-lineage.v1",
      task: { task_id: t.id, project_id: v.src.project, action_key: v.entry.action_key, objective: t.objective, status: t.status, result_class: t.result_class,
        error_code: t.error_code, attempt: t.attempt, submitted_at: t.submitted_at, started_at: t.started_at, finished_at: t.finished_at },
      origin: { origin: "person", basis: "submission", principal_id: me, principal_name: "Example researcher", principal_kind: "person", recorded_at: t.submitted_at },
      plan_step: null,
      inputs: { workspace: null, checkpoint_revision: null, platform_tool: null, classes: ["prior_output"], classes_recorded_by: "render_request" },
      method: { profile, profile_name: "Figure from a result table", tool_class: "vetted_service", template_ref: null, template_sha256: null,
        recorded: "submission", harness: null, role: null, tool_bundle: null },
      code: null, query: null,
      render: { source_task: v.src.sourceTask, table_path: v.request.table_path, table_sha256: v.request.table_sha256, source_package_sha256: v.src.source().artifact.sha256,
        feature: v.request.feature, controls: v.request.controls, title: v.request.title, previous: v.request.previous, request_sha256: v.request.request_sha256,
        created_at: v.request.created_at, service_version: v.receipts.at(-1)?.service_version ?? null, image_digest: v.receipts.at(-1)?.image_digest ?? null },
      execution: { image_ref: null, cpu_limit: null, memory_limit: null, max_run_seconds: 120, attempts: t.attempt, scientific_history: 0 },
      outputs: v.artifacts.map((a) => ({ artifact_id: a.id, role: a.role, label: a.label, relative_path: a.relative_path, media_type: a.media_type,
        size_bytes: a.size_bytes, sha256: a.sha256, created_at: a.created_at, provenance_class: "vetted_tool_output" })),
      review: { state: "not_reviewed" },
      gaps: !v.artifacts.length && t.status === "completed" ? ["no_outputs_recorded"] : [],
    };
  }

  return {
    owns: (id: string) => !!own(id),
    // Preview reset: the versions leave the task list with their histories and pending drawings.
    reset() {
      for (const t of timers) clearTimeout(t);
      // A preview still waiting is answered as one no worker drew, so its request does not hang.
      if (previewAnswer) refuse(previewAnswer, "figure_service_unavailable", 503);
      previewWaiting = false;
      previewAnswer = null;
      for (const v of versions) {
        const i = tasks.indexOf(v.task);
        if (i >= 0) tasks.splice(i, 1);
        histories.delete(v.task.id);
      }
      versions.length = 0;
      marks.clear();
      previewTimes.length = 0;
    },
    async handle(req: IncomingMessage, res: ServerResponse, url: URL, json: Json, denied: () => void): Promise<boolean> {
      const path = url.pathname;
      if (path === "/api/v1/figures/catalog" && req.method === "GET") {
        json({ catalog: { catalog_sha256: catalogSha, service_version: VERSION, image_digest: IMAGE, features, seen_at: seenAt,
          groups: [...new Set(listed().map((x) => x.group))] } });
        return true;
      }
      const asked = path.match(/^\/api\/v1\/projects\/([0-9a-f-]{36})\/figures(\/previews|\/([0-9a-f-]{36})\/manuscript)?$/);
      if (asked) {
        const [, project, rest, figure] = asked;
        if (!listed().some((x) => x.project === project)) { refuse(json, "project_not_found", 404); return true; }
        if (figure && req.method === "DELETE") {
          if (!own(figure) || own(figure)!.src.project !== project) { refuse(json, "figure_not_found", 404); return true; }
          const was = marks.get(figure);
          if (was) event(own(was.version)!.task, "completed", "figure_manuscript_cleared");
          marks.delete(figure);
          res.statusCode = 204; res.end();
          return true;
        }
        if (req.method !== (figure ? "PUT" : "POST")) { denied(); return true; }
        const raw = await body(req);
        let input: Record<string, unknown>;
        try { input = JSON.parse(raw); } catch { refuse(json, "invalid_request"); return true; }
        if (figure) mark(project, figure, input, json);
        else if (rest === "/previews") preview(project, input, json);
        else request(project, input, raw, json);
        return true;
      }
      const taskRoute = path.match(/^\/api\/v1\/tasks\/([0-9a-f-]{36})(\/.*)$/);
      if (!taskRoute) return false;
      const [, id, suffix] = taskRoute;
      const v = own(id);
      if (suffix === "/figure" && req.method === "GET") {
        if (!v && !known(id)) { denied(); return true; }
        json({ figure: v ? detail(v) : null });
        return true;
      }
      if (suffix === "/figures" && req.method === "GET") {
        if (!known(id)) { denied(); return true; }
        json({ versions: versions.filter((x) => x.src.sourceTask === id).map((x) => ({ task_id: x.task.id, table_path: x.request.table_path, feature: x.request.feature,
          title: x.request.title, previous: x.request.previous, status: x.task.status, created_at: x.request.created_at, finished_at: x.task.finished_at,
          manuscript: markOf(x), figure: rootOf(x) }))
          .sort((a, b) => b.created_at.localeCompare(a.created_at)) });
        return true;
      }
      if (!v) return false;
      if (suffix === "/artifacts") { json({ artifacts: v.artifacts }); return true; }
      if (suffix === "/lineage") { json(lineage(v)); return true; }
      if (suffix === "/generated-code") { json({ generated_code: null }); return true; }
      if (suffix === "/query") { json({ query: null }); return true; }
      const contents = suffix.match(/^\/artifacts\/([0-9a-f-]{36})\/(contents(?:\/file)?|download|pages|summary)$/);
      if (contents && v.pkg) {
        const [packaged, checksum] = v.artifacts;
        const artifact = v.artifacts.find((a) => a.id === contents[1]);
        if (!artifact) { denied(); return true; }
        if (contents[2] === "pages" || contents[2] === "summary") {
          answerTable(contents[2], url, artifact === packaged ? v.pkg.files : null, { path: artifact.relative_path, bytes: v.pkg.checksum }, json);
          return true;
        }
        if (contents[2] === "contents") {
          if (artifact !== packaged) { refuse(json, "artifact_not_a_package", 415); return true; }
          json({ artifact_id: artifact.id, sha256: artifact.sha256, size_bytes: artifact.size_bytes, listing: listing(v.pkg.files) });
          return true;
        }
        const bytes = contents[2] === "download" ? (artifact === checksum ? v.pkg.checksum : v.pkg.archive)
          : v.pkg.files.find((f) => f.path === url.searchParams.get("path"))?.bytes;
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
