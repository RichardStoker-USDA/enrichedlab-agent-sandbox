// The figure editor's rules: which charts a result table can draw, how each declared setting reads for a
// scientist, which columns start in which role, and how the form becomes a request. Every word here is fixed text
// chosen by code; the settings themselves come from the render service's catalog.
import type { FigureCatalog, FigureControls, FigureFeature, FigureParam, FigureReceipt } from "./contracts.ts";
import { taskStatus, type Tone } from "./language.ts";

export const MAX_ROWS = 50_000;
export const MAX_COLUMNS = 64;

/** A table can be drawn when it has at least one row and fits the service's limits. */
export function canDraw(rows: number, columns: number): boolean {
  return rows >= 1 && rows <= MAX_ROWS && columns >= 1 && columns <= MAX_COLUMNS;
}

const columnParams = (feature: FigureFeature) =>
  Object.entries(feature.params).filter(([, p]) => p.type === "column" || p.type === "columns");

/** The charts whose required columns the table has enough columns for, in the catalog's order. */
export function drawable(catalog: FigureCatalog | null, columns: number): FigureFeature[] {
  return (catalog?.features ?? []).filter((f) => columnParams(f).filter(([, p]) => p.required).length <= columns);
}

const LABELS: Record<string, string> = {
  x: "Across", y: "Up", color: "Colour by", colour: "Colour by", group: "Groups", category: "Categories",
  value: "Values", label: "Label points with", size: "Point size by", shape: "Point shape by", facet: "Panels by",
  id: "Row names", feature: "Features", sample: "Samples", chr: "Chromosome", pos: "Position", effect: "Effect size",
  significance: "Significance (p-value)", taxon: "Taxon", abundance: "Abundance", fill: "Fill by",
  x_label: "Horizontal axis title", y_label: "Vertical axis title", legend_position: "Legend", width: "Width",
  height: "Height", dpi: "Resolution", format: "Also save as", font: "Typeface", base_size: "Text size",
  log_x: "Logarithmic horizontal axis", log_y: "Logarithmic vertical axis", x_limits: "Horizontal axis range",
  y_limits: "Vertical axis range", hline: "Horizontal reference lines", vline: "Vertical reference lines",
  palette: "Colours", bins: "Number of bins", orientation: "Direction", order_by: "Order by",
  show_points: "Show every point", accent: "Accent colour", sig_threshold: "Significance line",
  effect_threshold: "Effect size lines", threshold: "Threshold line", top_n: "Features shown", scale: "Scaling",
  method: "Correlation", alpha: "Cells shown up to", loadings: "Arrows shown", test: "Test between groups",
  order_by_median: "Order groups by median", metric: "Diversity measure", distance: "Distance measure",
  ci_low: "Interval start", ci_high: "Interval end", subtitle: "Subtitle", legend_title: "Legend title",
  group_colors: "Colours per group", z: "Height", theta: "Turn", phi: "Tilt", expand: "Vertical scale", z_label: "Height axis title",
  // A surface's second column runs front to back, not up.
  "surface:y": "Deep",
};

const HINTS: Record<string, string> = {
  x_label: "Leave empty to use the column's name.",
  y_label: "Leave empty to use the column's name.",
  subtitle: "A line under the title, such as the data and its units.",
  legend_title: "Leave empty to use the column's name.",
  width: "When printed. One journal column is about 85 mm, a full page about 180 mm.",
  height: "When printed.",
  theta: "Degrees around the surface; 0 looks from the front.",
  phi: "Degrees above the floor; 90 looks straight down.",
  expand: "How tall the heights are drawn against the width of the floor.",
  dpi: "Dots per inch for the PNG and TIFF. 300 suits most journals.",
  format: "The PNG and SVG are always made.",
  x_limits: "The lowest and highest value to show, such as 0, 50. It zooms the view and leaves out no data.",
  y_limits: "The lowest and highest value to show, such as 0, 50. It zooms the view and leaves out no data.",
  hline: "Values where a horizontal line is drawn, separated by commas.",
  vline: "Values where a vertical line is drawn, separated by commas.",
  log_x: "Values at or below zero cannot be drawn on a logarithmic axis and are left out; the figure says how many.",
  log_y: "Values at or below zero cannot be drawn on a logarithmic axis and are left out; the figure says how many.",
  base_size: "The size of the figure's text, in points.",
  sig_threshold: "A horizontal line at this significance value.",
  effect_threshold: "Vertical lines at this effect size, on both sides of zero.",
  threshold: "A horizontal line at this score, such as 6 for a LOD scan.",
  bins: "More bins show more detail. 30 suits most data.",
  accent: "A colour written as #rrggbb, such as #022851. The boxes are filled in shades of it.",
  top_n: "How many of the most variable features to show.",
  alpha: "Cells whose adjusted p-value is above this are left blank.",
  loadings: "How many of the columns that shape the two axes are drawn as arrows.",
  show_points: "Draws every value over its box.",
  test: "Chosen for the data picks a test from the number of groups and how the values are spread.",
  "pca_biplot:scale": "Gives every column the same spread before the axes are found.",
  "de_heatmap:scale": "Scaling each row shows each feature's pattern; as they are shows the values themselves.",
};

// Plain descriptions of the render service's charts; a chart not named here keeps the service's own description.
const CHARTS: Record<string, string> = {
  scatter: "One point per row, with one column across and one up. Points can be coloured by a third column.",
  line: "Points joined in order along the horizontal axis, one line per group.",
  bar: "One bar per category, as long as its value.",
  box: "How the values spread within each group: the middle half, the median and the extremes.",
  histogram: "How the values of one column are spread, counted in bins.",
  manhattan: "A genome scan: one score for each position, chromosome by chromosome.",
  volcano: "Effect size across and significance up, one point per feature.",
  qtl_lod_plot: "LOD scores along each chromosome, with an optional threshold and interval.",
  correlation_heatmap: "How closely each pair of numeric columns rises and falls together.",
  pca_biplot: "The rows placed on their two main axes of variation, with the columns that shape those axes.",
  de_heatmap: "The value of each feature in each sample, as a grid of colours.",
  surface: "A surface over a grid: one column across, one deep and one as the height. A cell without a row stays empty.",
  grouped_boxplot: "Box plots for each group with a significance test between the groups.",
  microbiome_alpha_diversity: "The diversity within each sample, compared across groups.",
  microbiome_beta_pcoa: "How different the samples' communities are, drawn as a map.",
};

const CHOICES: Record<string, Record<string, string>> = {
  legend_position: { right: "Right", left: "Left", top: "Above", bottom: "Below", none: "No legend" },
  format: { pdf: "PDF", tiff: "TIFF" },
  font: { sans: "Sans serif", serif: "Serif", mono: "Monospace" },
  palette: { clinical: "Clinical", CCcolors: "Collaborative Cross strains", sexpair: "Female and male",
    viridis: "Viridis, readable with colour blindness", grey: "Grey" },
  orientation: { vertical: "Upright bars", horizontal: "Sideways bars" },
  method: { pearson: "Pearson", spearman: "Spearman" },
  scale: { row: "Each row scaled", none: "As they are" },
  test: { auto: "Chosen for the data", t: "t-test", wilcoxon: "Wilcoxon rank-sum", anova: "ANOVA", kruskal: "Kruskal-Wallis" },
  metric: { shannon: "Shannon", simpson: "Simpson", invsimpson: "Inverse Simpson", observed: "Observed taxa", chao1: "Chao1" },
  distance: { bray: "Bray-Curtis", jaccard: "Jaccard" },
};

const words = (name: string) => {
  const text = name.replace(/_/g, " ").trim();
  return text.charAt(0).toUpperCase() + text.slice(1);
};

/** The plain name of a setting, in a chart's own words where it has them. */
export function settingLabel(name: string, chart = ""): string {
  return LABELS[`${chart}:${name}`] ?? LABELS[name] ?? words(name);
}

/** A short hint under a setting, or the service's own description of a chart-specific one. */
export function settingHint(name: string, param: FigureParam, chart = ""): string {
  return HINTS[`${chart}:${name}`] ?? HINTS[name]
    ?? (param.description && !LABELS[name] ? param.description.charAt(0).toUpperCase() + param.description.slice(1) : "");
}

/** What a chart shows, in plain words. */
export function chartHint(feature: FigureFeature): string {
  return CHARTS[feature.id] ?? (feature.description ? feature.description.charAt(0).toUpperCase() + feature.description.slice(1) : "");
}

/** The words for one choice of an enum setting. */
export function choiceLabel(name: string, choice: string | number): string {
  return CHOICES[name]?.[String(choice)] ?? words(String(choice));
}

/** Choices worth offering: the PNG and SVG are always made, so the extra export offers only the others. */
export function offeredChoices(name: string, param: FigureParam): (string | number)[] {
  const all = param.choices ?? [];
  return name === "format" ? all.filter((c) => c !== "png" && c !== "svg") : all;
}

/** A yes-or-no setting is an enum whose choices are exactly true and false. */
export function isSwitch(param: FigureParam): boolean {
  const c = (param.choices ?? []).map(String).sort();
  return param.type === "enum" && c.length === 2 && c[0] === "false" && c[1] === "true";
}

export type Section = "columns" | "angle" | "titles" | "legend" | "size" | "more";
const SECTION: Record<string, Section> = {
  x_label: "titles", y_label: "titles", z_label: "titles", subtitle: "titles", font: "titles", base_size: "titles", legend_position: "legend",
  legend_title: "legend", group_colors: "legend", width: "size", height: "size", dpi: "size", format: "size",
  theta: "angle", phi: "angle", expand: "angle",
};

/** Data colours a person can give the groups: Okabe and Ito's set, which reads with colour blindness, and a grey. They
 *  colour marks in R's drawing only; the interface keeps UC Davis colours. */
export const OKABE_ITO: { hex: string; name: string }[] = [
  { hex: "#E69F00", name: "Orange" }, { hex: "#56B4E9", name: "Sky blue" }, { hex: "#009E73", name: "Bluish green" },
  { hex: "#F0E442", name: "Yellow" }, { hex: "#0072B2", name: "Blue" }, { hex: "#D55E00", name: "Vermillion" },
  { hex: "#CC79A7", name: "Reddish purple" }, { hex: "#000000", name: "Black" }, { hex: "#999999", name: "Grey" },
];
export const colourName = (hex: string) => OKABE_ITO.find((c) => c.hex.toLowerCase() === hex.toLowerCase())?.name ?? hex;

// The render service takes sizes in inches; journals give them in millimetres, so the editor shows
// millimetres and sends inches to four decimals.
const MM_PER_INCH = 25.4;
const inchSize = (name: string) => name === "width" || name === "height";
/** A size in inches as millimetres to one decimal, without a trailing zero. */
export const toMm = (inches: number) => String(Math.round(inches * MM_PER_INCH * 10) / 10);
export const toInches = (mm: number) => Math.round((mm / MM_PER_INCH) * 10000) / 10000;
/** Journal widths: a column, one and a half and a full page. */
export const WIDTHS = [{ mm: 85, label: "One column" }, { mm: 120, label: "1.5 columns" }, { mm: 180, label: "Full width" }];

/** The settings of a chart in the editor's sections, each in the catalog's order. */
export function sections(feature: FigureFeature): Record<Section, [string, FigureParam][]> {
  const out: Record<Section, [string, FigureParam][]> = { columns: [], angle: [], titles: [], legend: [], size: [], more: [] };
  for (const [name, param] of Object.entries(feature.params)) {
    if (param.type === "column" || param.type === "columns") out.columns.push([name, param]);
    else out[SECTION[name] ?? "more"].push([name, param]);
  }
  out.columns.sort((a, b) => Number(!!b[1].required) - Number(!!a[1].required));
  return out;
}

const NUMERIC_ROLES = new Set(["y", "z", "value", "abundance", "effect", "significance", "lod", "pos", "size"]);
const NUMERIC_X = new Set(["scatter", "line", "histogram", "volcano", "surface"]);
const measured = (feature: FigureFeature, role: string) => NUMERIC_ROLES.has(role) || (role === "x" && NUMERIC_X.has(feature.id));

/** A column whose name marks it as a record's identifier or code (`fdc_id`, `Patient ID`, `sampleId`, `grid_no`): it
 *  may hold numbers, but it is not a measure to plot. */
export function identifierName(name: string): boolean {
  const n = name.trim();
  return /(^|[\s_.-])(id|ids|identifier|code|key|no)$/i.test(n) || /[a-z](Id|ID)$/.test(n);
}

/** A column a lab preparation writes beside a measure reported against a limit (`glucose_censored`, `glucose_limit`):
 *  it describes the measure's cells, so it is not the first thing to plot. */
export function companionName(name: string, columns: string[]): boolean {
  const base = name.match(/^(.+)_(censored|limit)$/)?.[1];
  return !!base && columns.includes(base);
}

/** The starting columns for a chart's required column settings: measures (numbers that are not identifiers) for the
 *  measured roles, other columns for categories, each column used once while there are enough. */
export function startingColumns(feature: FigureFeature, columns: string[], numeric: boolean[]): FigureControls {
  const out: FigureControls = {};
  const used = new Set<string>();
  const pick = (wantNumber: boolean) => {
    const order = columns.map((c, i) => ({ c, n: numeric[i] ?? false, id: identifierName(c) || companionName(c, columns) }))
      .sort((a, b) => Number(b.n === wantNumber) - Number(a.n === wantNumber) || Number(a.id) - Number(b.id));
    const hit = order.find((o) => !used.has(o.c)) ?? order[0];
    if (hit) used.add(hit.c);
    return hit?.c;
  };
  for (const [name, param] of columnParams(feature)) {
    if (!param.required || param.type !== "column") continue;
    const chosen = pick(measured(feature, name));
    if (chosen !== undefined) out[name] = chosen;
  }
  return out;
}

/** Whether a table can fill a chart's required columns with the kind each role needs, each column used once: a
 *  measure for a measured role and a column that is not a number for a category. A new figure starts on the first
 *  chart that suits its table. */
export function suits(feature: FigureFeature, columns: string[], numeric: boolean[]): boolean {
  let measures = columns.filter((c, i) => numeric[i] && !identifierName(c)).length;
  let others = columns.filter((_, i) => !numeric[i]).length;
  for (const [name, param] of columnParams(feature)) {
    if (!param.required || param.type !== "column") continue;
    if (measured(feature, name)) measures -= 1; else others -= 1;
  }
  return measures >= 0 && others >= 0;
}

/** The form's values: every setting as the text or choices a person entered. */
export type Draft = Record<string, string | string[]>;

/** The form for a chart filled from a request's settings. */
export function draftFrom(feature: FigureFeature, controls: FigureControls): Draft {
  const draft: Draft = {};
  for (const [name, param] of Object.entries(feature.params)) {
    const value = controls[name];
    if (value === undefined) continue;
    if (param.type === "columns") draft[name] = (Array.isArray(value) ? value : [value]).map(String);
    else if (param.type === "numbers") draft[name] = (Array.isArray(value) ? value : [value]).join(", ");
    else if (param.type === "colors") draft[name] = typeof value === "object" && !Array.isArray(value) ? JSON.stringify(value) : "";
    else if (param.type === "number" && inchSize(name) && typeof value === "number") draft[name] = toMm(value);
    else draft[name] = String(value);
  }
  return draft;
}

/** The request's settings from the form, and a plain sentence for each value that cannot be sent. */
export function controlsFrom(feature: FigureFeature, draft: Draft, columns: string[]): { controls: FigureControls; problems: string[] } {
  const controls: FigureControls = {};
  const problems: string[] = [];
  for (const [name, param] of Object.entries(feature.params)) {
    const raw = draft[name];
    const label = settingLabel(name, feature.id);
    if (param.type === "columns") {
      const list = (Array.isArray(raw) ? raw : []).filter((c) => columns.includes(c));
      if (list.length) controls[name] = list;
      else if (param.required) problems.push(`Choose at least one column for ${label.toLowerCase()}.`);
      continue;
    }
    const text = (Array.isArray(raw) ? raw.join(",") : raw ?? "").trim();
    if (!text) {
      if (param.required && param.type === "column") problems.push(`Choose a column for ${label.toLowerCase()}.`);
      continue;
    }
    if (param.type === "colors") {
      let map: Record<string, string> = {};
      try { map = JSON.parse(text); } catch { /* an unreadable draft sends nothing */ }
      const offered = (param.choices ?? []).map((c) => String(c).toLowerCase());
      const entries = Object.entries(map ?? {}).filter(([group, hex]) => group && typeof hex === "string" && hex);
      if (entries.some(([, hex]) => offered.length > 0 && !offered.includes(hex.toLowerCase())))
        problems.push(`${label}: choose colours from the offered set.`);
      else if (param.maxlen && entries.length > param.maxlen) problems.push(`${label} takes at most ${param.maxlen} groups.`);
      else if (entries.length) controls[name] = Object.fromEntries(entries);
      continue;
    }
    if (param.type === "number" && inchSize(name)) {
      const n = Number(text);
      const low = param.min !== undefined ? Number(toMm(param.min)) : undefined, high = param.max !== undefined ? Number(toMm(param.max)) : undefined;
      if (!Number.isFinite(n)) problems.push(`${label} needs a number of millimetres.`);
      else if ((low !== undefined && n < low) || (high !== undefined && n > high))
        problems.push(low !== undefined && high !== undefined ? `${label} must be between ${low} and ${high} mm.`
          : low !== undefined ? `${label} must be at least ${low} mm.` : `${label} must be at most ${high} mm.`);
      else controls[name] = toInches(n);
      continue;
    }
    if (param.type === "column") {
      if (columns.includes(text)) controls[name] = text;
      else problems.push(`${label}: the table has no column named ${text}.`);
    } else if (param.type === "number") {
      const n = Number(text);
      if (!Number.isFinite(n)) problems.push(`${label} needs a number.`);
      else if ((param.min !== undefined && n < param.min) || (param.max !== undefined && n > param.max))
        problems.push(param.min !== undefined && param.max !== undefined ? `${label} must be between ${param.min} and ${param.max}.`
          : param.min !== undefined ? `${label} must be at least ${param.min}.` : `${label} must be at most ${param.max}.`);
      else controls[name] = n;
    } else if (param.type === "numbers") {
      const parts = text.split(/[\s,;]+/).filter(Boolean).map(Number);
      if (!parts.length || parts.some((n) => !Number.isFinite(n))) problems.push(`${label} needs numbers separated by commas.`);
      else if (param.maxlen && parts.length > param.maxlen) problems.push(`${label} takes at most ${param.maxlen} numbers.`);
      else if (/_limits$/.test(name) && parts.length !== 2) problems.push(`${label} needs two numbers: the lowest and the highest.`);
      else controls[name] = parts;
    } else if (param.type === "enum") {
      const match = (param.choices ?? []).find((c) => String(c).toLowerCase() === text.toLowerCase());
      if (match !== undefined) controls[name] = match;
      else problems.push(`${label}: choose one of the offered values.`);
    } else if (text.length > (param.maxlen ?? 200)) problems.push(`${label} is longer than ${param.maxlen ?? 200} characters.`);
    else controls[name] = text;
  }
  return { controls, problems };
}

/** A figure version's settings as short lines a scientist reads, in the editor's order. */
export function settingsLines(feature: FigureFeature | null, controls: FigureControls): string[] {
  const s = feature ? sections(feature) : null;
  const names = s ? [...s.columns, ...s.angle, ...s.titles, ...s.legend, ...s.size, ...s.more].map(([name]) => name) : Object.keys(controls);
  const lines: string[] = [];
  const size = [controls.width, controls.height].every((v) => typeof v === "number")
    ? `${toMm(controls.width as number)} by ${toMm(controls.height as number)} mm` : "";
  for (const name of names) {
    const value = controls[name];
    if (value === undefined || name === "height" || (name === "width" && size)) {
      if (name === "width" && size) lines.push(`Size: ${size}`);
      continue;
    }
    const shown = Array.isArray(value) ? value.join(", ")
      : typeof value === "object" ? Object.entries(value).map(([group, hex]) => `${group} ${colourName(hex).toLowerCase()}`).join(", ")
      : feature?.params[name]?.type === "enum" ? (isSwitch(feature.params[name]) ? (String(value) === "true" ? "yes" : "no") : choiceLabel(name, String(value)))
      : name === "dpi" ? `${value} dots per inch` : inchSize(name) && typeof value === "number" ? `${toMm(value)} mm` : String(value);
    lines.push(`${settingLabel(name, feature?.id)}: ${shown}`);
  }
  return lines;
}

/** The render service's note or refusal with each quoted setting name in the editor's words: "'width' 30 is outside 3 to
 *  20; 20 was used" reads "Width 30 is outside 3 to 20; 20 was used". Column names and values stay as written. */
export function plainNote(text: string, chart = ""): string {
  const out = text.replace(/'([a-z][a-z0-9_]*)'/g, (whole, name: string) => (LABELS[name] ? settingLabel(name, chart) : whole));
  return out.charAt(0).toUpperCase() + out.slice(1);
}

const sentence = (text: string) => (/[.!?]$/.test(text.trim()) ? text.trim() : `${text.trim()}.`);

/** One sentence for a version's outcome. */
export function outcomeSentence(receipt: FigureReceipt | undefined, status: string): string {
  if (!receipt) return status === "failed" ? "This figure was not drawn." : "The figure is being drawn.";
  if (receipt.status === "completed") return receipt.notes.length ? "Drawn. The render service changed or left out:" : "Drawn exactly as asked.";
  // The receipt's message is the broker's own words.
  if ((receipt.error_code === "figure_refused" || receipt.error_code === "figure_render_failed") && receipt.message)
    return `Not drawn. ${sentence(plainNote(receipt.message))}`;
  if (receipt.error_code === "figure_source_changed") return "Not drawn: the table no longer matches the one it was asked from.";
  if (receipt.error_code === "render_service_unavailable") return "Not drawn: the render service did not answer. Try again in a moment.";
  return "Not drawn because of a service problem. Nothing about the analysis changed.";
}

/** A figure version's status in the editor's words: one that failed was not drawn, and its receipt says why. */
export function versionStatus(status: string): { label: string; tone: Tone } {
  return status === "failed" ? { label: "Not drawn", tone: "failed" } : taskStatus(status);
}

/** Why a request was refused before any version existed, in plain words. */
export function requestProblem(code: string): string {
  switch (code) {
    case "figure_settings_invalid": return "One of the settings is not one this chart takes. Check the columns and values and try again.";
    case "figure_table_unknown": return "This table is not in the analysis's recorded summary, so it cannot be drawn here.";
    case "figure_table_unsuitable": return "This table cannot be drawn here: it has no rows, more than 50,000 rows or 64 columns, or columns its record leaves out.";
    case "figure_source_changed": return "The analysis's files no longer match their record, so R will not draw from this table.";
    case "figure_source_unreadable": return "The analysis's files could not be read, so R could not draw from this table.";
    case "figure_table_missing": return "This table is not in the analysis's files.";
    case "figure_table_too_large": return "This table is larger than R reads for a figure: 50,000 rows and 64 columns at most.";
    case "figure_table_unreadable": return "R could not read this table as it was written.";
    case "figure_source_not_ready": return "The analysis has not finished, so its tables cannot be drawn yet.";
    case "figure_method_unavailable": return "Figures are not set up for this lab yet.";
    case "figure_catalog_unavailable": return "The render service has not been reached yet. Try again in a moment.";
    case "figure_feature_unknown": return "This chart is no longer offered. Choose another.";
    case "project_not_found": case "access_required": return "Your account cannot add to this project.";
    default: return "The figure could not be requested. Nothing was changed.";
  }
}

/** Why a preview did not come, in words, and whether asking again can help. A table the
 *  worker cannot read as its record names it stays that way, and so does lost access, so only a wait, a busy moment or
 *  a failed service is worth another try. */
export function previewProblem(status: number, code: string): { words: string; again: boolean } {
  if (status === 429) return { words: "Previews are paused for a moment: one is still being drawn, or many were asked for in the last minute.", again: true };
  if (status === 413) return { words: "This drawing is larger than a preview can be (2 MB). Keep a version to see it.", again: false };
  if (status === 503) return { words: "No preview came back in time, or the render service failed.", again: true };
  if (status === 404 && code === "project_not_found")
    return { words: "Your account can no longer add to this project, or the analysis has left it. Nothing was kept.", again: false };
  if (status === 404 && code === "task_not_found") return { words: "The analysis this table comes from is no longer available. Nothing was kept.", again: false };
  if (status === 409 || status === 400) {
    const words = requestProblem(code);
    return { words: words === requestProblem("") ? "R could not draw a preview from this table with these settings." : words, again: false };
  }
  return { words: "The preview could not be drawn just now.", again: true };
}

/** A chart's name from the catalog, or its identifier in words when the catalog no longer lists it. */
export function chartName(catalog: FigureCatalog | null, id: string): string {
  return catalog?.features.find((f) => f.id === id)?.label ?? words(id);
}

interface Versioned { task_id: string; previous: string | null; created_at: string; status: string; manuscript?: { version: string } | null;
  /** The figure's first version as the broker names it. */
  figure?: string }
const newestFirst = (a: Versioned, b: Versioned) => b.created_at.localeCompare(a.created_at) || b.task_id.localeCompare(a.task_id);

/** Figure versions joined into figures by the edits between them. Each figure's versions run newest first, and the
 *  figures are ordered by their newest version. */
export function figureGroups<T extends Versioned>(versions: T[]): T[][] {
  const root = new Map(versions.map((v) => [v.task_id, v.task_id]));
  const find = (id: string) => { while (root.get(id) !== id) id = root.get(id)!; return id; };
  for (const v of versions) if (v.previous && root.has(v.previous)) root.set(find(v.task_id), find(v.previous));
  const groups = new Map<string, T[]>();
  for (const v of versions) groups.set(find(v.task_id), [...(groups.get(find(v.task_id)) ?? []), v]);
  return [...groups.values()].map((g) => g.sort(newestFirst)).sort((a, b) => newestFirst(a[0], b[0]));
}

/** The version that stands for a figure: the one marked for the manuscript, else its newest drawn version, else its
 *  newest. */
export function shownVersion<T extends Versioned>(group: T[]): T {
  return group.find((v) => v.manuscript?.version === v.task_id) ?? group.find((v) => v.status === "completed") ?? group[0];
}

/** A figure's manuscript mark as its versions carry it: `undefined` when the broker records no marks, else the mark
 *  or null. */
export function manuscriptOf<T extends Versioned>(group: T[]): T["manuscript"] | undefined {
  if (group.every((v) => v.manuscript === undefined)) return undefined;
  return group.find((v) => v.manuscript)?.manuscript ?? null;
}

/** Two versions' settings side by side in the editor's order, chart and title first, each row marked when the two
 *  differ. A setting one version leaves to R reads "Default". */
export function settingsDiff(catalog: FigureCatalog | null, a: { feature: string; title: string; controls: FigureControls },
  b: { feature: string; title: string; controls: FigureControls }): { label: string; a: string; b: string; differs: boolean }[] {
  const feature = (id: string) => catalog?.features.find((f) => f.id === id) ?? null;
  const split = (lines: string[]) => new Map(lines.map((line) => { const at = line.indexOf(": "); return [line.slice(0, at), line.slice(at + 2)] as const; }));
  const left = split(settingsLines(feature(a.feature), a.controls)), right = split(settingsLines(feature(b.feature), b.controls));
  // The merged settings give one order for both; a label only one side uses goes at the end.
  const order = [...split(settingsLines(feature(a.feature), { ...b.controls, ...a.controls })).keys()];
  const labels = [...new Set([...order, ...left.keys(), ...right.keys()])].filter((label) => left.has(label) || right.has(label));
  const row = (label: string, x: string, y: string) => ({ label, a: x, b: y, differs: x !== y });
  return [row("Chart", chartName(catalog, a.feature), chartName(catalog, b.feature)), row("Title", a.title.trim() || "Default", b.title.trim() || "Default"),
    ...labels.map((label) => row(label, left.get(label) ?? "Default", right.get(label) ?? "Default"))];
}

/** The broker's name for a figure: its first version, as the broker names it, else the oldest version in its group. */
export function figureRoot<T extends Versioned>(group: T[]): string {
  return group.find((v) => v.figure)?.figure ?? group[group.length - 1].task_id;
}

/** A project's analyses with each figure listed once, by the version that stands for it, in the input order. */
export function withFiguresOnce<T extends { task_id: string; status: string; submitted_at: string;
  figure?: { previous: string | null } | null }>(tasks: T[]): T[] {
  const versions = tasks.filter((t) => t.figure).map((t) => ({ task_id: t.task_id, previous: t.figure!.previous,
    created_at: t.submitted_at, status: t.status }));
  const shown = new Set(figureGroups(versions).map((g) => shownVersion(g).task_id));
  return tasks.filter((t) => !t.figure || shown.has(t.task_id));
}
