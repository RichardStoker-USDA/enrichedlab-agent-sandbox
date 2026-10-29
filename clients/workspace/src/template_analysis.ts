// Reviewed R analyses: a person runs one of the render service's analysis templates on
// one result table of a finished analysis, or on a prepared lab table. No model writes code for it: each run is its own
// analysis, with the request, the service's receipt and a package that reruns it, and it waits for a person's review
// like any analysis. This module holds the shapes, the requests, the words for each template and setting, the starting
// settings for a table, and the outcome words. Every word here is fixed text chosen by code; the settings come from the
// render service's catalog, and the results from the checked files.
import { BrokerError, request } from "./api.ts";
import type { FigureFeature, FigureParam } from "./contracts.ts";
import { companionName, identifierName } from "./figure.ts";
import { taskStatus, type Tone } from "./language.ts";

export type AnalysisFeature = FigureFeature;
export type AnalysisParam = FigureParam;
/** The analysis templates the render service declares (`kind: analysis`), kept apart from the figure catalog. */
export interface AnalysisCatalog {
  catalog_sha256: string; service_version: string; image_digest: string | null; features: AnalysisFeature[]; seen_at: string;
  /** The reader's labs whose template-analysis method is enabled; analyses are offered only in their projects. */
  groups: string[];
}
export type AnalysisSettings = Record<string, string | number | boolean | (string | number)[]>;
/** A refusal the broker knows, naming only columns of the table and whole counts. It points at
 *  the setting to change; the words are the broker's `message`. */
export interface AnalysisRefusal { code: string; columns: string[]; counts: number[] }
export interface AnalysisFile { path: string; sha256: string; size_bytes: number; media_type: string; role: string }
export interface AnalysisReceipt {
  attempt: number; status: "completed" | "refused" | "failed"; error_code: string | null; message: string | null;
  refusal: AnalysisRefusal | null; service_version: string | null; image_digest: string | null;
  /** The template's own statement of what ran, plain text. */
  method: string | null;
  files: AnalysisFile[] | null; notes: string[]; package_sha256: string | null; started_at: string; finished_at: string;
}
export interface TemplateAnalysis {
  task_id: string; project_id: string; source_task: string; source_title: string | null; source_package_sha256: string;
  table_path: string; table_sha256: string; table_rows: number; table_columns: string[]; feature: string;
  settings: AnalysisSettings; title: string; previous: string | null; catalog_sha256: string; request_sha256: string;
  created_at: string; receipts: AnalysisReceipt[];
  /** The research lead's choice: its run, turn and plan step. A person's request has none. */
  origin?: { run: string; turn: number; action_key: string } | null;
}
export interface AnalysisVersionSummary {
  task_id: string; table_path: string; feature: string; title: string; previous: string | null; status: string;
  created_at: string; finished_at: string | null;
}
export interface AnalysisRequest {
  source_task: string; table_path: string; feature: string; settings: AnalysisSettings; title: string; previous: string | null;
  request_id: string;
}

const UUID = /^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/i;
const id = (value: string) => { if (!UUID.test(value)) throw new BrokerError("invalid_reference"); return value; };

export const analyses = {
  /** The newest catalog the worker read, or null until it has read one. */
  catalog: async (signal?: AbortSignal) =>
    (await request<{ catalog: AnalysisCatalog | null }>("/api/v1/template-analyses/catalog", { signal })).catalog,
  /** A template analysis's request, its source and every receipt; null for any other task. */
  detail: async (task: string, signal?: AbortSignal) =>
    (await request<{ template_analysis: TemplateAnalysis | null }>(`/api/v1/tasks/${id(task)}/template-analysis`, { signal })).template_analysis,
  /** The template analyses made from one analysis that the reader may open, newest first. */
  versions: async (task: string, signal?: AbortSignal) =>
    (await request<{ template_analyses: AnalysisVersionSummary[] }>(`/api/v1/tasks/${id(task)}/template-analyses`, { signal })).template_analyses,
  /** Asks for a new analysis; the request id makes a repeated send one analysis. */
  request: async (project: string, body: AnalysisRequest) =>
    (await request<{ task_id: string }>(`/api/v1/projects/${id(project)}/template-analyses`, {
      method: "POST", headers: { "Content-Type": "application/json", "X-Agent-Sandbox-Intent": "mutate" }, body: JSON.stringify(body) })).task_id,
};

/** The questions the analyses answer, in the order a bench scientist meets them. An analysis a newer catalog adds goes
 *  under "More analyses" with the service's own description. */
export const QUESTIONS: { title: string; ids: string[] }[] = [
  { title: "Describe a column", ids: ["summary_stats", "transform_selector"] },
  { title: "Compare groups", ids: ["group_compare", "anova_emmeans", "ancova_body_size", "batch_screen_tests", "permanova"] },
  { title: "Animals over time", ids: ["tolerance_test_auc", "repeated_measures_mixed", "animal_window_summary", "body_composition_delta"] },
  { title: "Relate columns", ids: ["correlation", "linear_model", "correlation_fdr_matrix", "ml_model"] },
  { title: "Genetic scans", ids: ["qtl_peaks_table", "eqtl_hotspot_scan"] },
  { title: "Plan a study", ids: ["power_calc"] },
];

const ABOUT: Record<string, string> = {
  summary_stats: "Count, mean, spread, median and range of one column, for each group if you choose one.",
  transform_selector: "Whether a column reads best raw, as log2 or as a square root, from its normality and skew.",
  group_compare: "Whether a value differs between two groups: a Welch t-test or a Wilcoxon test, with each group's numbers and the difference.",
  anova_emmeans: "Differences across one to three factors, with each group's estimated mean and which groups differ.",
  ancova_body_size: "Groups of animals compared on a measure that grows with body size, such as energy expenditure, adjusted for it.",
  batch_screen_tests: "The same test on every feature against the groups, with one false discovery rate across all of them.",
  permanova: "Whether whole communities, taxa and their abundances, differ between groups.",
  tolerance_test_auc: "A glucose or insulin tolerance test, one animal at a time: the areas under each curve, its peak and its nadir.",
  repeated_measures_mixed: "Repeated readings of the same animals over time, by a mixed model with the animal as the unit.",
  animal_window_summary: "One row per animal from repeated rows (hours, days or visits), over a window of time you choose.",
  body_composition_delta: "Each subject's change from baseline to follow-up, tested as pairs and compared between groups.",
  correlation: "How closely two columns rise and fall together, with the estimate's interval.",
  linear_model: "A straight-line model of one column from another, adjusted for other columns if you choose them.",
  correlation_fdr_matrix: "Every pair of numeric columns correlated, with one false discovery rate over the unique pairs.",
  ml_model: "Which features best predict an outcome, and how well, by random forest, elastic net, a support vector machine or a neural network.",
  qtl_peaks_table: "One peak for each trait and chromosome from a marker-level QTL scan, above a LOD threshold.",
  eqtl_hotspot_scan: "Stretches of the genome where more eQTL markers gather than chance would put there.",
  power_calc: "How many animals a planned comparison needs, or what effect it can find. It reads no values from the table.",
};

const capital = (text: string) => text.charAt(0).toUpperCase() + text.slice(1);
const words = (name: string) => capital(name.replace(/_/g, " ").trim());

/** What an analysis does, in plain words, or the service's own description of one this page does not know. */
export function analysisAbout(feature: AnalysisFeature): string {
  return ABOUT[feature.id] ?? (feature.description ? capital(feature.description.trim()) : "");
}
/** An analysis's name from the catalog, or its identifier in words when the catalog no longer lists it. */
export function analysisName(catalog: AnalysisCatalog | null, id: string): string {
  return catalog?.features.find((f) => f.id === id)?.label ?? words(id);
}

/** The catalog's analyses under their questions, each question's in this page's order; analyses this page does not
 *  know follow under "More analyses". */
export function byQuestion(catalog: AnalysisCatalog | null): { title: string; features: AnalysisFeature[] }[] {
  const all = catalog?.features ?? [];
  const known = new Set(QUESTIONS.flatMap((q) => q.ids));
  const out = QUESTIONS.map((q) => ({ title: q.title, features: q.ids.flatMap((id) => all.filter((f) => f.id === id)) }));
  out.push({ title: "More analyses", features: all.filter((f) => !known.has(f.id)) });
  return out.filter((q) => q.features.length);
}

// Setting names as a scientist reads them: an analysis's own words first, then the shared ones.
const LABELS: Record<string, string> = {
  value: "Values", values: "Values to summarize", group: "Groups", test: "Test", experimental_unit: "One row for each",
  outcome: "Outcome", factors: "Factors", interaction: "Include the interaction", covariates: "Adjust for", method: "Method",
  sample: "Samples", taxon: "Taxa", abundance: "Abundance", distance: "Distance", feature: "Features", task: "Task",
  top_n: "How many to show", id: "Animal", time: "Time", baseline: "Baseline", followup: "Follow-up", measure: "Measure",
  trait: "Traits", chromosome: "Chromosome", chrom: "Chromosome", lod: "LOD score", position: "Position", gene: "Gene",
  analysis_type: "Scan type", min_lod: "LOD threshold", alpha: "Significance level", x_label: "Horizontal axis title",
  base_size: "Text size", keep: "Carry along", from: "Window starts at", to: "Window ends before", phase: "Light phase",
  phase_value: "Keep the phase", statistic: "Summarize each animal by", body_size: "Body size", estimand: "Compare",
  factor2: "Second factor", body_size2: "Second covariate", covariance: "Correlation between visits", time_model: "Time as",
  comparisons: "Report", show_animals: "Draw each animal", upper_limit: "Meter's highest reading", design: "Design",
  n: "Animals per group", effect_size: "Effect size", power: "Power", allocation_ratio: "Group size ratio", tails: "Sides",
  pve: "Variance the locus explains", lod_threshold: "LOD threshold", df: "Degrees of freedom", window_mb: "Window (Mb)",
  step_mb: "Step (Mb)", n_trees: "Trees", kernel: "Kernel", size: "Hidden units", min_n: "Values needed in each group",
  "correlation:x": "First column", "correlation:y": "Second column", "correlation:method": "Correlation",
  "linear_model:y": "Outcome", "linear_model:x": "Predictor", "summary_stats:value": "Column", "transform_selector:value": "Column",
  "ml_model:id": "Subjects", "ml_model:alpha": "Elastic net mixing", "ml_model:top_n": "Features shown",
  "batch_screen_tests:top_n": "Features shown", "body_composition_delta:id": "Subjects", "permanova:group": "Groups",
  "correlation_fdr_matrix:method": "Correlation", "correlation_fdr_matrix:alpha": "Adjusted p-value cutoff",
  "correlation_fdr_matrix:id": "Leave out", "eqtl_hotspot_scan:alpha": "Poisson cutoff", "eqtl_hotspot_scan:top_n": "Hotspots shown",
  "tolerance_test_auc:time": "Time after the dose", "tolerance_test_auc:value": "Reading",
  "repeated_measures_mixed:time": "Time or visit", "repeated_measures_mixed:value": "Reading", "repeated_measures_mixed:baseline": "First reading",
  "ancova_body_size:value": "Measure", "anova_emmeans:interaction": "Include interactions",
};

const HINTS: Record<string, string> = {
  experimental_unit: "The animal or subject column. Each must have one row: readings of one animal are not separate animals.",
  upper_limit: "For example 600 for a meter that reads up to 600 mg/dL. Readings at or above it are counted, since they may stand for higher values.",
  "tolerance_test_auc:group": "Optional. The group each animal belongs to, such as its diet.",
  "tolerance_test_auc:test": "A glucose test's main area is above the first reading; an insulin test's is below it.",
  "group_compare:test": "Chosen for the data uses a Welch t-test when both groups look normal, else the Wilcoxon test.",
  "group_compare:group": "A column with exactly two groups.",
  "anova_emmeans:factors": "One to three columns, such as diet, sex and strain.",
  "permanova:group": "One or two columns, such as diet and sex.",
  "repeated_measures_mixed:factors": "None, one or two columns that belong to the animal, such as diet and sex.",
  "animal_window_summary:values": "One to eight columns, such as energy expenditure and activity.",
  "animal_window_summary:keep": "Columns with one value per animal, such as its group or body mass.",
  "ancova_body_size:body_size": "Body mass or lean mass. Needed to adjust for body size or to divide by it.",
  "power_calc:design": "Give two of animals per group, effect size and power; the third is solved.",
  "summary_stats:group": "Optional. Each group's numbers are listed on their own row.",
  "correlation_fdr_matrix:id": "An ID or label column to leave out of the matrix.",
  "batch_screen_tests:min_n": "A feature with fewer values in a group is skipped, never filled in.",
  covariates: "Optional numeric columns to adjust for.",
};

const CHOICES: Record<string, Record<string, string>> = {
  test: { auto: "Chosen for the data", t: "Welch t-test", wilcoxon: "Wilcoxon", anova: "ANOVA", kruskal: "Kruskal-Wallis",
    glucose_tolerance: "Glucose tolerance", insulin_tolerance: "Insulin tolerance" },
  method: { pearson: "Pearson", spearman: "Spearman", both: "Both", random_forest: "Random forest", elastic_net: "Elastic net",
    svm: "Support vector machine", neural_net: "Neural network" },
  distance: { bray: "Bray-Curtis", jaccard: "Jaccard" },
  task: { auto: "Chosen from the outcome", regression: "Regression", classification: "Classification" },
  kernel: { radial: "Radial", linear: "Linear" },
  design: { two_group: "Two groups", qtl: "QTL detection" },
  tails: { "1": "One-sided", "2": "Two-sided" },
  statistic: { mean: "Mean", median: "Median", sum: "Sum" },
  estimand: { adjusted: "Animals of the same body size", whole_animal: "The animals as they are", per_unit_size: "Per unit of body size" },
  covariance: { compound_symmetry: "Any two visits equally", ar1: "Nearer visits more closely" },
  time_model: { levels: "Each time its own mean", linear: "A straight line for each group" },
  baseline: { none: "Modeled like the others", adjust: "Each animal's covariate" },
  comparisons: { groups_at_each_time: "Groups at each time", times_within_group: "Times within each group", none: "No comparisons" },
};

/** The plain name of a setting, in an analysis's own words where it has them. */
export function settingName(name: string, analysis = ""): string {
  return LABELS[`${analysis}:${name}`] ?? LABELS[name] ?? words(name);
}
/** A hint under a setting: this page's words, else the service's description of the setting. */
export function settingHelp(name: string, param: AnalysisParam, analysis = ""): string {
  return HINTS[`${analysis}:${name}`] ?? HINTS[name] ?? (param.description ? capital(param.description.trim()) : "");
}
export function choiceName(name: string, choice: string | number): string {
  return CHOICES[name]?.[String(choice)] ?? words(String(choice));
}

// What each column setting takes: a measure (numbers that are not identifiers), a category (text), the animal or
// subject, a time, or any column.
type Kind = "measure" | "category" | "subject" | "time" | "any";
const KIND: Record<string, Kind> = {
  value: "measure", values: "measure", x: "measure", y: "measure", abundance: "measure", lod: "measure", baseline: "measure",
  followup: "measure", body_size: "measure", body_size2: "measure", covariates: "measure", position: "measure",
  group: "category", factors: "category", factor2: "category", feature: "category", taxon: "category", trait: "category",
  chromosome: "category", chrom: "category", gene: "category", phase: "category", analysis_type: "category", measure: "category",
  id: "subject", sample: "subject", experimental_unit: "subject", time: "time", outcome: "any", keep: "any",
};
const kindOf = (name: string): Kind => KIND[name] ?? "any";
// A column named as a time ("minutes", "day", "visit") is the first choice for time, one that names a time among other
// words ("time_after_dose") the next; "insulin_0_min" is a measure taken at a time, not a time.
const TIME_NAME = /^(time|minutes?|mins?|hours?|hrs?|days?|weeks?|months?|visits?|t|timepoint|time_point|zt)$/i;
const TIME_PART = /(^|_)(time|minutes|hours|days|weeks|visit)($|_)/i;
const SUBJECT_NAME = /(^|_)(animal|mouse|mice|rat|subject|participant|sample|id)($|_)/i;

interface Column { name: string; numeric: boolean; measure: boolean; subject: boolean; time: 0 | 1 | 2 }
function described(columns: string[], numeric: boolean[]): Column[] {
  return columns.map((name, i) => {
    const n = numeric[i] ?? false;
    const ident = identifierName(name) || SUBJECT_NAME.test(name);
    return { name, numeric: n, measure: n && !ident && !companionName(name, columns), subject: ident,
      time: TIME_NAME.test(name) ? 0 : TIME_PART.test(name) ? 1 : 2 };
  });
}

const required = (feature: AnalysisFeature) =>
  Object.entries(feature.params).filter(([, p]) => p.required && (p.type === "column" || p.type === "columns"));

// One column for each required setting, each used once, the best fit first.
function assign(feature: AnalysisFeature, columns: Column[]): { picks: Map<string, string>; missing: Kind[] } {
  const used = new Set<string>(), picks = new Map<string, string>(), missing: Kind[] = [];
  // A time is a measure too, so a measure prefers columns not named as times.
  const rank = (kind: Kind, c: Column) => kind === "measure" ? (c.measure ? (c.time === 0 ? 1 : 0) : 9)
    : kind === "category" ? (!c.numeric && !c.subject ? 0 : !c.numeric ? 1 : 9)
    : kind === "subject" ? (c.subject ? 0 : !c.numeric ? 1 : 9)
    : kind === "time" ? (c.time === 0 ? 0 : c.time === 1 ? 1 : c.numeric && !c.subject ? 2 : 5)
    : c.measure ? 0 : !c.subject ? 1 : 2;
  // Settings that only one kind of column fits choose first, so a looser one cannot take its column.
  const order = required(feature).map(([name]) => name).sort((a, b) => Number(kindOf(a) === "any") - Number(kindOf(b) === "any"));
  for (const name of order) {
    const kind = kindOf(name);
    const best = columns.filter((c) => !used.has(c.name) && rank(kind, c) < 9).sort((a, b) => rank(kind, a) - rank(kind, b))[0];
    if (best) { used.add(best.name); picks.set(name, best.name); } else missing.push(kind);
  }
  return { picks, missing };
}

const MIN_MEASURES: Record<string, number> = { correlation_fdr_matrix: 2 };

/** Whether a table has the columns an analysis needs, and if not, what it lacks in words. */
export function fit(feature: AnalysisFeature, columns: string[], numeric: boolean[]): { fits: boolean; why: string } {
  const cols = described(columns, numeric);
  const { missing } = assign(feature, cols);
  const measures = cols.filter((c) => c.measure).length;
  if (MIN_MEASURES[feature.id] && measures < MIN_MEASURES[feature.id])
    return { fits: false, why: `Needs at least ${MIN_MEASURES[feature.id]} numeric columns; this table has ${measures}.` };
  if (!missing.length) return { fits: true, why: "" };
  const need = (kind: Kind) => required(feature).filter(([name]) => kindOf(name) === kind).length;
  if (missing.includes("measure")) return { fits: false, why: `Needs ${need("measure") === 1 ? "a numeric column" : `${need("measure")} numeric columns`} this table does not have.` };
  if (missing.includes("category")) return { fits: false, why: `Needs ${need("category") === 1 ? "a column of groups" : `${need("category")} columns of groups`} this table does not have.` };
  return { fits: false, why: `Needs ${required(feature).length} columns; this table has ${columns.length}.` };
}

/** The required columns an analysis starts with for a table: measures for values, text columns for groups, an ID column
 *  for the animal, a time-like column for time, each used once while there are enough. */
export function startingSettings(feature: AnalysisFeature, columns: string[], numeric: boolean[]): AnalysisSettings {
  const { picks } = assign(feature, described(columns, numeric));
  const out: AnalysisSettings = {};
  for (const [name, param] of required(feature)) {
    const pick = picks.get(name);
    if (pick) out[name] = param.type === "columns" ? [pick] : pick;
  }
  return out;
}

/** The analyses offered for a table: those it fits first, in the questions' order; the rest after, with what they need. */
export function offered(catalog: AnalysisCatalog | null, columns: string[], numeric: boolean[]) {
  return byQuestion(catalog).map((q) => ({ title: q.title, features: q.features.map((f) => ({ feature: f, ...fit(f, columns, numeric) })) }));
}

/** The first analysis for a prepared lab table in the long layout: a tolerance test when its points
 *  are numbers, read per animal with its first category as the groups; else repeated measures. Other tables start as the
 *  dialog chooses. */
export function labTableStart(spec: { long?: { name: string; values: (string | number)[]; value: { name: string } } | null;
  key?: string[] | null; columns: { name: string; type: string }[] }, catalog: AnalysisCatalog | null, title: string)
  : { feature: string; settings: AnalysisSettings; title: string; previous: null } | null {
  if (!spec.long || !catalog) return null;
  const has = (id: string) => catalog.features.some((f) => f.id === id);
  const subject = (spec.key ?? []).find((k) => k !== spec.long!.name) ?? spec.columns.find((c) => c.type === "text" && SUBJECT_NAME.test(c.name))?.name;
  if (!subject) return null;
  const groups = spec.columns.filter((c) => c.type === "category" && c.name !== subject).map((c) => c.name);
  const numericPoints = spec.long.values.every((v) => typeof v === "number");
  if (numericPoints && has("tolerance_test_auc"))
    return { feature: "tolerance_test_auc", settings: { id: subject, time: spec.long.name, value: spec.long.value.name, ...(groups[0] ? { group: groups[0] } : {}) },
      title: title ? `Tolerance test areas of ${title}` : "", previous: null };
  if (has("repeated_measures_mixed"))
    return { feature: "repeated_measures_mixed", settings: { id: subject, time: spec.long.name, value: spec.long.value.name, ...(groups.length ? { factors: groups.slice(0, 2) } : {}) },
      title: "", previous: null };
  return null;
}

/** A form's values: each setting as the text or the columns a person chose. */
export type Draft = Record<string, string | string[]>;

/** The form for an analysis filled from a request's settings. */
export function draftOf(feature: AnalysisFeature, settings: AnalysisSettings): Draft {
  const draft: Draft = {};
  for (const [name, param] of Object.entries(feature.params)) {
    const value = settings[name];
    if (value === undefined || value === null) continue;
    draft[name] = param.type === "columns" ? (Array.isArray(value) ? value : [value]).map(String)
      : param.type === "numbers" ? (Array.isArray(value) ? value : [value]).join(", ") : String(value);
  }
  return draft;
}

/** The request's settings from the form, with a plain sentence for each value that cannot be sent. A setting left
 *  empty is left to the template's default; the broker checks the rest again. */
export function settingsFrom(feature: AnalysisFeature, draft: Draft, columns: string[]): { settings: AnalysisSettings; problems: string[] } {
  const settings: AnalysisSettings = {};
  const problems: string[] = [];
  for (const [name, param] of Object.entries(feature.params)) {
    const raw = draft[name];
    const label = settingName(name, feature.id);
    if (param.type === "columns") {
      const list = (Array.isArray(raw) ? raw : []).filter((c) => columns.includes(c));
      if (list.length) settings[name] = list;
      else if (param.required) problems.push(`Choose at least one column for ${label.toLowerCase()}.`);
      continue;
    }
    const text = (Array.isArray(raw) ? raw.join(",") : raw ?? "").trim();
    if (!text) {
      if (param.required) problems.push(param.type === "column" ? `Choose a column for ${label.toLowerCase()}.` : `${label} is needed.`);
      continue;
    }
    if (param.type === "column") {
      if (columns.includes(text)) settings[name] = text;
      else problems.push(`${label}: the table has no column named ${text}.`);
    } else if (param.type === "number") {
      const n = Number(text);
      if (!Number.isFinite(n)) problems.push(`${label} needs a number.`);
      else if ((param.min !== undefined && n < param.min) || (param.max !== undefined && n > param.max))
        problems.push(param.min !== undefined && param.max !== undefined ? `${label} must be between ${param.min} and ${param.max}.`
          : param.min !== undefined ? `${label} must be at least ${param.min}.` : `${label} must be at most ${param.max}.`);
      else settings[name] = n;
    } else if (param.type === "numbers") {
      const parts = text.split(/[\s,;]+/).filter(Boolean).map(Number);
      if (!parts.length || parts.some((n) => !Number.isFinite(n))) problems.push(`${label} needs numbers separated by commas.`);
      else if (param.maxlen && parts.length > param.maxlen) problems.push(`${label} takes at most ${param.maxlen} numbers.`);
      else settings[name] = parts;
    } else if (param.type === "enum") {
      const match = (param.choices ?? []).find((c) => String(c).toLowerCase() === text.toLowerCase());
      if (match !== undefined) settings[name] = match;
      else problems.push(`${label}: choose one of the offered values.`);
    } else if (text.length > (param.maxlen ?? 200)) problems.push(`${label} is longer than ${param.maxlen ?? 200} characters.`);
    else settings[name] = text;
  }
  return { settings, problems };
}

/** The comparison a tolerance test's table is made for: the groups' main areas, one row per animal, by a group
 *  comparison for two groups or an ANOVA for more. The template names its main area and counts the groups in its
 *  statistics; this reads them from the checked file. Other analyses have no next step here. */
export function nextStep(analysis: TemplateAnalysis, statistics: Map<string, string> | null, catalog: AnalysisCatalog | null,
  resultsPath: string): { label: string; table: string; feature: string; settings: AnalysisSettings; title: string } | null {
  if (analysis.feature !== "tolerance_test_auc" || !catalog || !statistics) return null;
  const group = typeof analysis.settings.group === "string" ? analysis.settings.group : "";
  const animal = typeof analysis.settings.id === "string" ? analysis.settings.id : "";
  const area = statistics.get("primary_endpoint") ?? "";
  const groups = Number(statistics.get("groups"));
  if (!group || !animal || !/^[a-z_]+$/.test(area) || !Number.isInteger(groups) || groups < 2) return null;
  const feature = groups === 2 ? "group_compare" : "anova_emmeans";
  if (!catalog.features.some((f) => f.id === feature)) return null;
  const settings: AnalysisSettings = feature === "group_compare" ? { value: area, group, experimental_unit: animal }
    : { outcome: area, factors: [group], experimental_unit: animal };
  return { label: `Compare the ${groups} groups on ${area.replaceAll("_", " ")}`, table: resultsPath, feature, settings,
    title: `${capital(area.replaceAll("_", " "))} by ${group.replaceAll("_", " ")}` };
}

/** The step a refusal of repeated rows asks for: one row per animal from the same table, by the per-animal summary,
 *  carrying the groups along, before groups are compared. The person can pick the tolerance test from the same list
 *  when the rows are its readings. Other refusals point at their settings only. */
export function refusalStep(analysis: TemplateAnalysis, receipt: AnalysisReceipt | undefined, catalog: AnalysisCatalog | null):
  { label: string; table: string; feature: string; settings: AnalysisSettings; title: string } | null {
  if (receipt?.status !== "refused" || receipt.refusal?.code !== "repeated_rows_per_animal") return null;
  if (!catalog?.features.some((f) => f.id === "animal_window_summary")) return null;
  const s = analysis.settings, text = (v: unknown) => typeof v === "string" && v ? v : null;
  const unit = text(s.experimental_unit), value = text(s.value) ?? text(s.outcome);
  if (!unit || !value) return null;
  const groups = [text(s.group), ...(Array.isArray(s.factors) ? s.factors.map(text) : [])];
  const keep = [...new Set(groups.filter((c): c is string => !!c && c !== unit && c !== value))];
  return { label: "Summarize each animal first", table: analysis.table_path, feature: "animal_window_summary",
    settings: { id: unit, values: [value], ...(keep.length ? { keep } : {}) }, title: `${capital(value.replaceAll("_", " "))} per animal` };
}

/** The settings a refusal points at: each column setting whose column, or one of whose columns, the refusal names. */
export function refusedSettings(feature: AnalysisFeature | null, settings: AnalysisSettings, refusal: AnalysisRefusal | null | undefined): string[] {
  if (!refusal?.columns.length) return [];
  const named = new Set(refusal.columns);
  return Object.entries(settings).filter(([name, value]) => {
    const type = feature?.params[name]?.type;
    if (type && type !== "column" && type !== "columns") return false;
    return (Array.isArray(value) ? value : [value]).some((v) => named.has(String(v)));
  }).map(([name]) => name);
}

/** An analysis's settings as short lines, in the catalog's order with the columns first. A setting left to the
 *  template's default is not listed. */
export function settingLines(feature: AnalysisFeature | null, settings: AnalysisSettings): { name: string; label: string; value: string }[] {
  const names = feature ? Object.entries(feature.params).sort(([, a], [, b]) =>
    Number(b.type === "column" || b.type === "columns") - Number(a.type === "column" || a.type === "columns")).map(([name]) => name)
    : Object.keys(settings);
  const lines: { name: string; label: string; value: string }[] = [];
  for (const name of [...names, ...Object.keys(settings).filter((n) => !names.includes(n))]) {
    const value = settings[name];
    if (value === undefined || value === null || value === "") continue;
    const param = feature?.params[name];
    const switchLike = param?.type === "enum" && (param.choices ?? []).map(String).sort().join() === "false,true";
    const shown = Array.isArray(value) ? value.join(", ")
      : switchLike ? (String(value) === "true" ? "Yes" : "No")
      : param?.type === "enum" ? choiceName(name, String(value)) : String(value);
    lines.push({ name, label: settingName(name, feature?.id ?? ""), value: shown });
  }
  return lines;
}

/** A template analysis's status in plain words: one the service refused was not run, and its receipt says why. */
export function analysisStatus(status: string, receipt?: AnalysisReceipt): { label: string; tone: Tone } {
  if (status === "failed") return { label: receipt?.status === "refused" ? "Not run" : "Did not finish", tone: "failed" };
  return taskStatus(status);
}

const sentence = (text: string) => (/[.!?]$/.test(text.trim()) ? text.trim() : `${text.trim()}.`);

/** One sentence for an analysis's outcome. A refusal's words are the broker's own, shown as they are. */
export function outcomeWords(receipt: AnalysisReceipt | undefined, status: string): string {
  if (!receipt) return status === "failed" ? "This analysis did not run." : status === "cancelled" ? "Stopped before it ran."
    : "Waiting for a compute worker, or running. The page updates when it finishes.";
  if (receipt.status === "completed") return "Ran with the settings as asked.";
  if ((receipt.error_code === "analysis_refused" || receipt.error_code === "analysis_failed") && receipt.message) return `Not run. ${sentence(receipt.message)}`;
  switch (receipt.error_code) {
    case "analysis_source_changed": return "Not run: the table no longer matches the one it was asked from.";
    case "analysis_source_unreadable": return "Not run: the source analysis's files could not be read.";
    case "analysis_table_missing": return "Not run: the table is not in the source analysis's files.";
    case "analysis_table_too_large": return "Not run: the table is larger than the analysis service reads (64 MB).";
    case "analysis_table_unreadable": return "Not run: the table is not one header and rows of equal width.";
    case "analysis_table_unsuitable": return "Not run: the table has no rows, or more than 50,000.";
    case "analysis_result_too_large": return "Not kept: the results were larger than the broker keeps.";
    case "analysis_result_unreadable": return "Not kept: the results could not be read as a table.";
    case "cancelled": return "Stopped while the analysis service worked. Nothing was kept.";
    case "render_service_outdated": return "Not run: the analysis service is older than the release that keeps every digit of the results.";
    case "render_service_unavailable": return "Not run: the analysis service did not answer. Ask again in a moment.";
    case "render_service_unconfigured": return "Not run: the analysis service is not set up yet.";
    default: return "Not run because of a service problem. The source analysis is unchanged.";
  }
}

/** Why a request was refused before any analysis existed, in plain words. */
export function requestProblem(status: number, code: string): string {
  switch (code) {
    case "analysis_settings_invalid": return "One of the settings is not one this analysis takes, or a column it needs is missing. Check them and try again.";
    case "analysis_table_unknown": return "This table is not in the analysis's recorded summary, so it cannot be analyzed here.";
    case "analysis_table_unsuitable": return "This table cannot be analyzed here: it has no rows, more than 50,000 rows or 64 columns, or columns its record leaves out.";
    case "analysis_source_not_ready": return "The analysis has not finished, so its tables cannot be analyzed yet.";
    case "analysis_method_unavailable": return "Reviewed R analyses are not set up for this lab yet.";
    case "analysis_catalog_unavailable": return "The analysis service has not been reached yet. Try again in a moment.";
    case "analysis_feature_unknown": return "This analysis is no longer offered. Choose another.";
    case "analysis_previous_invalid": return "The analysis this would change is not one of this table's. Start a new one instead.";
    case "analysis_request_invalid": return "The title is longer than 200 characters, or the table's name is not valid.";
    case "idempotency_conflict": return "This request was sent before with other settings. Send it again to make a new one.";
    case "project_not_found": case "access_required": return "Your account cannot add to this project.";
    case "task_not_found": return "The analysis this table comes from is no longer available.";
    default: return status === 429 ? "The lab's queue is full. Try again in a few minutes." : "The analysis could not be requested. Nothing was changed.";
  }
}

const NUMBER = /^[-+]?(?:\d+\.?\d*|\.\d+)(?:[eE][-+]?\d+)?$/;
const SUPER: Record<string, string> = { "-": "⁻", "0": "⁰", "1": "¹", "2": "²", "3": "³", "4": "⁴", "5": "⁵", "6": "⁶", "7": "⁷", "8": "⁸", "9": "⁹" };

/** A value from a results file as the page shows it. Text stays as written. A number keeps up to four significant
 *  digits (one decimal from 1,000 up); a nonzero number that would show as 0 is written in scientific form, never as 0.
 *  The file keeps every digit, and the page shows them on request. */
export function shownValue(text: string): string {
  const t = text.trim();
  if (!NUMBER.test(t)) return text;
  const n = Number(t);
  if (!Number.isFinite(n)) return text;
  if (n === 0) return "0";
  if (Number.isInteger(n) && Math.abs(n) < 1e15) return String(n);
  const a = Math.abs(n);
  if (a >= 1e-3 && a < 1e3) return String(Number(n.toPrecision(4)));
  if (a >= 1e3 && a < 1e9) return String(Number(n.toFixed(1)));
  const [mantissa, exponent] = n.toExponential(2).split("e");
  return `${Number(mantissa)} × 10${[...String(Number(exponent))].map((c) => SUPER[c] ?? c).join("")}`;
}

/** Whether the shown value leaves out digits the file holds. */
export function rounded(text: string): boolean {
  return NUMBER.test(text.trim()) && shownValue(text) !== text.trim();
}
