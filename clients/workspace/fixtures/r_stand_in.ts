// The loopback preview's stand-in for three of the render service's R analysis templates (render 0.6.0): summary
// statistics, the two-group comparison and the tolerance test's areas per animal. Each follows its template's rules and
// refusals (the render service's `templates/*.R`) so the workspace meets the shapes it will meet on the cluster: one
// row per animal, groups in the template's order, every digit of each number, and refusals that name columns and
// counts. It is not R: the two-group comparison does not run R's Shapiro-Wilk check, and its Wilcoxon p-value uses the
// normal approximation, which the notes say. Every value the preview holds is simulated.

export type Cell = number | string | boolean | null;
export interface Outcome {
  refusal?: { code: string; columns: string[]; counts: number[] };
  results?: { columns: string[]; rows: Cell[][] };
  statistics?: [string, Cell][];
  method?: string;
  notes?: string[];
  /** Each group's mean reading at each time with its standard error, for the tolerance test's figure. */
  curves?: { group: string; points: { t: number; mean: number; se: number | null }[] }[];
}
type Table = { columns: string[]; rows: string[][] };
type Settings = Record<string, unknown>;

const column = (table: Table, name: unknown) => typeof name === "string" ? table.columns.indexOf(name) : -1;
// R's as.numeric on the text the file holds: NA, empty and words are missing.
const num = (text: string | undefined): number => {
  const t = (text ?? "").trim();
  return /^[-+]?(?:\d+\.?\d*|\.\d+)(?:[eE][-+]?\d+)?$/.test(t) ? Number(t) : NaN;
};
// ids_of: the text without surrounding spaces; a blank one or NA is missing.
const idOf = (text: string | undefined): string | null => { const t = (text ?? "").trim(); return t && t !== "NA" ? t : null; };
const textOf = (text: string | undefined): string | null => text === undefined || text === "NA" ? null : text;

export const mean = (x: number[]) => x.reduce((a, b) => a + b, 0) / x.length;
export const sd = (x: number[]) => {
  if (x.length < 2) return NaN;
  const m = mean(x);
  return Math.sqrt(x.reduce((a, b) => a + (b - m) ** 2, 0) / (x.length - 1));
};
/** R's quantile type 7. */
export function quantile(x: number[], p: number): number {
  const s = [...x].sort((a, b) => a - b);
  const h = (s.length - 1) * p, lo = Math.floor(h);
  return lo + 1 < s.length ? s[lo] + (h - lo) * (s[lo + 1] - s[lo]) : s[lo];
}
export const median = (x: number[]) => quantile(x, 0.5);

// The log of the gamma function (Lanczos, g = 7), and the regularized incomplete beta by its continued fraction.
const LANCZOS = [0.99999999999980993, 676.5203681218851, -1259.1392167224028, 771.32342877765313, -176.61502916214059,
  12.507343278686905, -0.13857109526572012, 9.9843695780195716e-6, 1.5056327351493116e-7];
function logGamma(z: number): number {
  if (z < 0.5) return Math.log(Math.PI / Math.sin(Math.PI * z)) - logGamma(1 - z);
  z -= 1;
  let x = LANCZOS[0];
  for (let i = 1; i < 9; i++) x += LANCZOS[i] / (z + i);
  const t = z + 7.5;
  return 0.5 * Math.log(2 * Math.PI) + (z + 0.5) * Math.log(t) - t + Math.log(x);
}
function betaFraction(a: number, b: number, x: number): number {
  const tiny = 1e-300;
  let c = 1, d = 1 - ((a + b) * x) / (a + 1);
  if (Math.abs(d) < tiny) d = tiny;
  d = 1 / d;
  let h = d;
  for (let m = 1; m <= 300; m++) {
    const m2 = 2 * m;
    let aa = (m * (b - m) * x) / ((a + m2 - 1) * (a + m2));
    d = 1 + aa * d; if (Math.abs(d) < tiny) d = tiny;
    c = 1 + aa / c; if (Math.abs(c) < tiny) c = tiny;
    d = 1 / d; h *= d * c;
    aa = (-(a + m) * (a + b + m) * x) / ((a + m2) * (a + m2 + 1));
    d = 1 + aa * d; if (Math.abs(d) < tiny) d = tiny;
    c = 1 + aa / c; if (Math.abs(c) < tiny) c = tiny;
    d = 1 / d;
    const step = d * c;
    h *= step;
    if (Math.abs(step - 1) < 1e-15) break;
  }
  return h;
}
function incompleteBeta(a: number, b: number, x: number): number {
  if (x <= 0) return 0;
  if (x >= 1) return 1;
  const front = Math.exp(logGamma(a + b) - logGamma(a) - logGamma(b) + a * Math.log(x) + b * Math.log(1 - x));
  return x < (a + 1) / (a + b + 2) ? (front * betaFraction(a, b, x)) / a : 1 - (front * betaFraction(b, a, 1 - x)) / b;
}
/** The t distribution's two-sided tail beyond |t|. */
export const tTail2 = (t: number, df: number) => incompleteBeta(df / 2, 0.5, df / (df + t * t));
/** The t distribution's quantile for an upper tail of `p`, by bisection on the tail. */
export function tQuantile(p: number, df: number): number {
  let lo = 0, hi = 1e3;
  for (let i = 0; i < 200; i++) { const mid = (lo + hi) / 2; if (tTail2(mid, df) / 2 > p) lo = mid; else hi = mid; }
  return (lo + hi) / 2;
}
// The standard normal's upper tail, from erfc (Numerical Recipes' Chebyshev fit, relative error under 1.2e-7).
function erfc(x: number): number {
  const z = Math.abs(x), t = 1 / (1 + 0.5 * z);
  const r = t * Math.exp(-z * z - 1.26551223 + t * (1.00002368 + t * (0.37409196 + t * (0.09678418 + t * (-0.18628806 + t * (0.27886807
    + t * (-1.13520398 + t * (1.48851587 + t * (-0.82215223 + t * 0.17087277)))))))));
  return x >= 0 ? r : 2 - r;
}
const normalTail2 = (z: number) => erfc(Math.abs(z) / Math.SQRT2);

/** summary_stats.R: n, mean, SD, median, IQR, minimum and maximum of one column, each group on its own row in R's
 *  sorted order of the groups. */
export function summaryStats(table: Table, s: Settings): Outcome {
  const vi = column(table, s.value), gi = column(table, s.group);
  if (vi < 0) return { refusal: { code: "column_missing", columns: [], counts: [] } };
  const row = (x: number[]): Cell[] => [x.length, mean(x), sd(x), median(x), quantile(x, 0.75) - quantile(x, 0.25), Math.min(...x), Math.max(...x)];
  const stats = ["n", "mean", "sd", "median", "IQR", "min", "max"];
  const nanToNull = (cells: Cell[]) => cells.map((c) => typeof c === "number" && !Number.isFinite(c) ? null : c);
  if (gi >= 0) {
    const parts = new Map<string, number[]>();
    for (const r of table.rows) {
      const v = num(r[vi]), g = textOf(r[gi]);
      if (Number.isFinite(v) && g !== null && g !== "") parts.set(g, [...(parts.get(g) ?? []), v]);
    }
    if (!parts.size) return { refusal: { code: "no_numeric_values", columns: [String(s.value)], counts: [] } };
    const groups = [...parts.keys()].sort((a, b) => (a < b ? -1 : a > b ? 1 : 0));
    return { results: { columns: ["group", ...stats], rows: groups.map((g) => nanToNull([g, ...row(parts.get(g)!)])) },
      method: "Descriptive statistics", notes: [`value = ${s.value}, grouped by ${s.group}`] };
  }
  const v = table.rows.map((r) => num(r[vi])).filter(Number.isFinite);
  if (!v.length) return { refusal: { code: "no_numeric_values", columns: [String(s.value)], counts: [] } };
  return { results: { columns: stats, rows: [nanToNull(row(v))] }, method: "Descriptive statistics", notes: [`value = ${s.value}`] };
}

/** group_compare.R: one value between exactly two groups, in the order they first appear. With an experimental unit,
 *  each unit must have one row. */
export function groupCompare(table: Table, s: Settings): Outcome {
  const vi = column(table, s.value), gi = column(table, s.group);
  if (vi < 0 || gi < 0) return { refusal: { code: "column_missing", columns: [], counts: [] } };
  const notes: string[] = [];
  let rows = table.rows.filter((r) => Number.isFinite(num(r[vi])) && textOf(r[gi]) !== null && r[gi] !== "");
  if (s.experimental_unit !== undefined) {
    const ui = column(table, s.experimental_unit);
    if (ui < 0) return { refusal: { code: "column_missing", columns: [], counts: [] } };
    const unnamed = rows.filter((r) => idOf(r[ui]) === null).length;
    if (unnamed) notes.push(`${unnamed} rows name no unit in '${s.experimental_unit}' and were left out`);
    rows = rows.filter((r) => idOf(r[ui]) !== null);
    const reps = new Map<string, number>();
    for (const r of rows) reps.set(idOf(r[ui])!, (reps.get(idOf(r[ui])!) ?? 0) + 1);
    const repeated = [...reps.values()].filter((n) => n > 1).length;
    if (repeated) return { refusal: { code: "repeated_rows_per_animal", columns: [String(s.experimental_unit)], counts: [repeated] } };
  }
  const levels = [...new Set(rows.map((r) => r[gi]))];
  if (levels.length !== 2) return { refusal: { code: "group_count_not_two", columns: [String(s.group)], counts: [levels.length] } };
  const a = rows.filter((r) => r[gi] === levels[0]).map((r) => num(r[vi])), b = rows.filter((r) => r[gi] === levels[1]).map((r) => num(r[vi]));
  if (a.length < 2 || b.length < 2) return { refusal: { code: "too_few_values_per_group", columns: [String(s.group)], counts: [2] } };
  const test = typeof s.test === "string" ? s.test : "auto";
  if (test === "wilcoxon") {
    const all = [...a.map((v) => ({ v, inA: true })), ...b.map((v) => ({ v, inA: false }))].sort((x, y) => x.v - y.v);
    const ranks: number[] = [];
    let ties = 0;
    for (let i = 0; i < all.length;) {
      let j = i; while (j + 1 < all.length && all[j + 1].v === all[i].v) j++;
      const t = j - i + 1; ties += t ** 3 - t;
      for (let k = i; k <= j; k++) ranks[k] = (i + j) / 2 + 1;
      i = j + 1;
    }
    const n = all.length, w = all.reduce((sum, x, i) => sum + (x.inA ? ranks[i] : 0), 0) - (a.length * (a.length + 1)) / 2;
    const mu = (a.length * b.length) / 2, variance = ((a.length * b.length) / 12) * (n + 1 - ties / (n * (n - 1)));
    const z = (w - mu - 0.5 * Math.sign(w - mu)) / Math.sqrt(variance);
    const shifts = a.flatMap((x) => b.map((y) => x - y));
    notes.push("The preview's stand-in gives the Wilcoxon p-value by the normal approximation; R gives the exact one for small groups without ties.");
    return { results: { columns: ["group", "n", "median", "IQR"], rows: [[levels[0], a.length, median(a), quantile(a, 0.75) - quantile(a, 0.25)],
      [levels[1], b.length, median(b), quantile(b, 0.75) - quantile(b, 0.25)]] },
      statistics: [["test", "Wilcoxon rank-sum"], ["statistic", w], ["p_value", normalTail2(z)], ["location_shift", median(shifts)], ["shapiro_min_p", null]],
      method: "Wilcoxon rank-sum", notes: [`${levels[0]} vs ${levels[1]} (value = ${s.value})`, ...notes] };
  }
  if (test === "auto") notes.push("The preview's stand-in does not run R's Shapiro-Wilk check, so it used the Welch t-test.");
  const ma = mean(a), mb = mean(b), va = sd(a) ** 2, vb = sd(b) ** 2;
  const se2 = va / a.length + vb / b.length;
  const t = (ma - mb) / Math.sqrt(se2);
  const df = se2 ** 2 / ((va / a.length) ** 2 / (a.length - 1) + (vb / b.length) ** 2 / (b.length - 1));
  const half = tQuantile(0.025, df) * Math.sqrt(se2);
  const pooled = Math.sqrt(((a.length - 1) * va + (b.length - 1) * vb) / (a.length + b.length - 2));
  return { results: { columns: ["group", "n", "mean", "sd"], rows: [[levels[0], a.length, ma, Math.sqrt(va)], [levels[1], b.length, mb, Math.sqrt(vb)]] },
    statistics: [["test", "Welch t-test"], ["statistic", t], ["df", df], ["p_value", tTail2(t, df)], ["mean_diff", ma - mb],
      ["cohens_d", (ma - mb) / pooled], ["ci_low", ma - mb - half], ["ci_high", ma - mb + half], ["shapiro_min_p", null]],
    method: "Welch t-test", notes: [`${levels[0]} vs ${levels[1]} (value = ${s.value})`, ...notes] };
}

/** tolerance_test_auc.R: each animal's areas and landmarks from one row per animal and time; one row per animal. */
export function toleranceTest(table: Table, s: Settings): Outcome {
  const ii = column(table, s.id), ti = column(table, s.time), vi = column(table, s.value);
  if (ii < 0 || ti < 0 || vi < 0) return { refusal: { code: "column_missing", columns: [], counts: [] } };
  const gi = column(table, s.group);
  const kind = typeof s.test === "string" ? s.test : "glucose_tolerance";
  if (kind !== "glucose_tolerance" && kind !== "insulin_tolerance") return { refusal: { code: "setting_invalid", columns: [], counts: [] } };
  const upper = typeof s.upper_limit === "number" && Number.isFinite(s.upper_limit) ? s.upper_limit : null;
  const all = table.rows.map((r) => ({ id: idOf(r[ii]), t: num(r[ti]), v: num(r[vi]), g: gi < 0 ? "all" : textOf(r[gi]) ?? "" }));
  const usable = all.filter((r) => r.id !== null && Number.isFinite(r.t));
  if (!usable.length) return { refusal: { code: "time_not_numeric", columns: [String(s.time)], counts: [] } };
  const unusable = all.length - usable.length;
  if (gi >= 0) {
    const per = new Map<string, Set<string>>();
    for (const r of usable) per.set(r.id!, (per.get(r.id!) ?? new Set()).add(r.g));
    const varying = [...per.values()].filter((g) => g.size > 1).length;
    if (varying) return { refusal: { code: "factor_varies_within_animal", columns: [String(s.id), String(s.group)], counts: [varying] } };
  }
  const pairs = usable.map((r) => `${r.id}\r${r.t}`);
  const repeated = pairs.length - new Set(pairs).size;
  if (repeated) return { refusal: { code: "repeated_time_rows", columns: [String(s.id), String(s.time)], counts: [repeated] } };
  const design = [...new Set(usable.map((r) => r.t))].sort((x, y) => x - y);
  if (design.length < 3) return { refusal: { code: "too_few_time_points", columns: [String(s.time)], counts: [design.length, 3] } };
  const first = design[0], last = design[design.length - 1];
  // The area of y above 0 over one segment, split where it crosses 0.
  const above = (t0: number, t1: number, a: number, b: number) => {
    const w = t1 - t0;
    if (a >= 0 && b >= 0) return (w * (a + b)) / 2;
    if (a <= 0 && b <= 0) return 0;
    return a > 0 ? (w * a * a) / (2 * (a - b)) : (w * b * b) / (2 * (b - a));
  };
  type Rec = { id: string; g: string; included: boolean; excluded: string | null; baseline: number | null; peak: number | null; tPeak: number | null;
    nadir: number | null; tNadir: number | null; nadirPct: number | null; total: number | null; incremental: number | null; below: number | null;
    readings: number; bridged: number | null; atLimit: number | null };
  const ids = [...new Set(usable.map((r) => r.id!))];
  const recs: Rec[] = ids.map((id) => {
    const rows = usable.filter((r) => r.id === id).sort((x, y) => x.t - y.t);
    const seen = rows.filter((r) => Number.isFinite(r.v));
    const rec: Rec = { id, g: rows[0].g, included: false, excluded: null, baseline: null, peak: null, tPeak: null, nadir: null, tNadir: null,
      nadirPct: null, total: null, incremental: null, below: null, readings: seen.length, bridged: null,
      atLimit: upper === null ? null : seen.filter((r) => r.v >= upper).length };
    if (!seen.some((r) => r.t === first)) return { ...rec, excluded: "no reading at the first time" };
    if (!seen.some((r) => r.t === last)) return { ...rec, excluded: "no reading at the last time" };
    const t = seen.map((r) => r.t), y = seen.map((r) => r.v), base = y[0];
    let total = 0, incremental = 0, below = 0;
    for (let i = 0; i + 1 < t.length; i++) {
      total += ((t[i + 1] - t[i]) * (y[i + 1] + y[i])) / 2;
      incremental += above(t[i], t[i + 1], y[i] - base, y[i + 1] - base);
      below += above(t[i], t[i + 1], base - y[i], base - y[i + 1]);
    }
    const peak = Math.max(...y), nadir = Math.min(...y);
    return { ...rec, included: true, baseline: base, peak, tPeak: t[y.indexOf(peak)], nadir, tNadir: t[y.indexOf(nadir)],
      nadirPct: base !== 0 ? (100 * nadir) / base : null, total, incremental, below,
      bridged: design.filter((d) => d > first && d < last && !t.includes(d)).length };
  });
  const byText = (a: string, b: string) => (a < b ? -1 : a > b ? 1 : 0);
  recs.sort((a, b) => byText(a.g, b.g) || byText(a.id, b.id));
  const used = recs.filter((r) => r.included);
  if (used.length < 2) return { refusal: { code: "too_few_animals", columns: [String(s.id)], counts: [used.length, 2] } };
  const columns = [String(s.id), ...(gi >= 0 ? [String(s.group)] : []), "included", "excluded_reason", "baseline", "peak", "time_of_peak", "nadir",
    "time_of_nadir", "nadir_percent_of_baseline", "total_auc", "incremental_auc", "area_below_baseline", "readings", "readings_bridged", "readings_at_limit"];
  const rows: Cell[][] = recs.map((r) => [r.id, ...(gi >= 0 ? [r.g] : []), r.included, r.excluded, r.baseline, r.peak, r.tPeak, r.nadir, r.tNadir,
    r.nadirPct, r.total, r.incremental, r.below, r.readings, r.bridged, r.atLimit]);
  const primary = kind === "glucose_tolerance" ? "incremental_auc" : "area_below_baseline";
  const groups = [...new Set(used.map((r) => r.g))];
  const noFirst = recs.filter((r) => r.excluded === "no reading at the first time").length, noLast = recs.filter((r) => r.excluded === "no reading at the last time").length;
  const bridged = used.reduce((n, r) => n + (r.bridged ?? 0), 0);
  const atLimit = upper === null ? null : recs.reduce((n, r) => n + (r.atLimit ?? 0), 0);
  const label = kind === "glucose_tolerance" ? "glucose tolerance test" : "insulin tolerance test";
  const excluded = [noFirst ? `${noFirst} without a reading at the first time` : "", noLast ? `${noLast} without one at the last time` : ""].filter(Boolean);
  const endpoint = primary === "incremental_auc" ? "incremental areas (incremental_auc)" : "areas below the first reading (area_below_baseline)";
  const notes = [
    `${used.length} of ${recs.length} animals used.${excluded.length ? ` Listed and not used: ${excluded.join(" and ")}.` : ""}`,
    bridged ? `${bridged} missed reading(s) between the first and last times were bridged by a straight line.` : "",
    unusable ? `${unusable} row(s) without an animal or a numeric time were left out.` : "",
    atLimit ? `${atLimit} reading(s) are at or above the upper limit ${upper}; they may stand for higher values, so those animals' areas may be too small.` : "",
    `Areas are in the value's unit times the time's unit. Compare the groups' ${endpoint} on this table, one row per animal, with the group comparison (2 groups) or ANOVA analysis.`,
  ].filter(Boolean).join(" ");
  const statistics: [string, Cell][] = [["test", label], ["primary_endpoint", primary], ["animals", recs.length], ["animals_used", used.length],
    ["times", design.length], ["first_time", first], ["last_time", last], ["readings_bridged", bridged], ["groups", groups.length]];
  if (atLimit !== null) statistics.push(["readings_at_limit", atLimit]);
  const seenUsed = usable.filter((r) => Number.isFinite(r.v) && used.some((u) => u.id === r.id));
  const curves = [...new Set(seenUsed.map((r) => r.g))].sort(byText).map((g) => ({ group: g, points: design.flatMap((t) => {
    const v = seenUsed.filter((r) => r.g === g && r.t === t).map((r) => r.v);
    return v.length ? [{ t, mean: mean(v), se: v.length > 1 ? sd(v) / Math.sqrt(v.length) : null }] : [];
  }) }));
  return { results: { columns, rows }, statistics, notes: [notes], curves,
    method: `${label} per animal: trapezoid areas over each animal's readings from ${first} to ${last} (${design.length} times); incremental area above and area below the animal's first reading, split where a segment crosses it; peak, nadir and nadir as % of the first reading. One row per animal; readings of one animal are not independent animals.` };
}

/** The templates the stand-in follows; any other gives the chosen numeric columns' counts and means, and says so. */
export function runStandIn(feature: string, table: Table, s: Settings, label: string): Outcome {
  if (feature === "summary_stats") return summaryStats(table, s);
  if (feature === "group_compare") return groupCompare(table, s);
  if (feature === "tolerance_test_auc") return toleranceTest(table, s);
  const chosen = Object.values(s).flatMap((v) => (Array.isArray(v) ? v : [v])).filter((v): v is string => typeof v === "string" && table.columns.includes(v));
  const rows: Cell[][] = [...new Set(chosen)].flatMap((name) => {
    const x = table.rows.map((r) => num(r[table.columns.indexOf(name)])).filter(Number.isFinite);
    return x.length ? [[name, x.length, mean(x), x.length > 1 ? sd(x) : null]] : [];
  });
  return { results: { columns: ["column", "n", "mean", "sd"], rows },
    method: `Preview stand-in for ${label}: the counts, means and standard deviations of the chosen numeric columns. On the cluster the render service runs the reviewed R template.`,
    notes: ["The workspace preview does not run this template; its numbers describe the chosen columns only."] };
}
