// A surface drawn from a table's stored values (numbers stay exact). Each row is one vertex at its stored
// x, y and height. Cells join only neighbours one regular step apart on both axes, so a block or hour the table does
// not hold leaves a gap instead of a surface drawn across it. Nothing is fitted, smoothed or interpolated; the renderer's
// shading between vertices is how it draws the stored points. Values are parsed as numbers only to place them.

export interface GridInput { x: string[]; y: string[]; z: string[]; rows: number[] }
export interface Grid {
  xs: number[]; ys: number[];
  /** Vertex positions in a unit box, in three.js axes: the table's x along x and its y along -z (each from -0.5 to 0.5),
   *  height along y from 0 to 1. Keeping y along -z keeps the space right-handed, as R's persp() draws it, so the
   *  3D view is not a mirror image of the table. */
  positions: Float32Array;
  /** Each vertex's height between 0 (lowest stored value) and 1 (highest), for its colour. */
  levels: Float32Array;
  /** The table row each vertex was drawn from. */
  rows: Int32Array;
  indices: Uint32Array;
  /** The stored strings of the lowest and highest heights, as written in the file. */
  low: string; high: string;
  /** Rows whose height is missing (empty or NA): no vertex. */
  missing: number;
  /** Values of each axis that a regular step expects and the table does not hold. */
  gaps: { x: number[]; y: number[] };
}
export type GridRefusal = "too_few" | "too_many" | "not_numeric" | "repeated";
export const MAX_POINTS = 250_000;

const MISSING = (value: string) => value.trim() === "" || value.trim() === "NA";

/** The regular step of an axis: the smallest difference between its sorted values. */
function stepOf(values: number[]): number {
  let step = Infinity;
  for (let i = 1; i < values.length; i++) step = Math.min(step, values[i] - values[i - 1]);
  return Number.isFinite(step) ? step : 1;
}
const near = (a: number, b: number) => Math.abs(a - b) <= 1e-9 * Math.max(1, Math.abs(a), Math.abs(b));
/** The values a regular step expects between the first and last, missing from the axis (at most 50 named). */
function gapsOf(values: number[], step: number): number[] {
  const out: number[] = [];
  for (let i = 1; i < values.length && out.length < 50; i++) {
    for (let v = values[i - 1] + step; v < values[i] - step / 2 && out.length < 50; v += step) out.push(Number(v.toPrecision(12)));
  }
  return out;
}

export function buildGrid(input: GridInput): Grid | GridRefusal {
  const n = Math.min(input.x.length, input.y.length, input.z.length, input.rows.length);
  if (n > MAX_POINTS) return "too_many";
  const points: { x: number; y: number; z: number; zText: string; row: number }[] = [];
  let missing = 0;
  for (let i = 0; i < n; i++) {
    const x = Number(input.x[i]), y = Number(input.y[i]);
    if (MISSING(input.x[i]) || MISSING(input.y[i]) || !Number.isFinite(x) || !Number.isFinite(y)) return "not_numeric";
    if (MISSING(input.z[i])) { missing++; continue; }
    const z = Number(input.z[i]);
    if (!Number.isFinite(z)) return "not_numeric";
    points.push({ x, y, z, zText: input.z[i].trim(), row: input.rows[i] });
  }
  const xs = [...new Set(points.map((p) => p.x))].sort((a, b) => a - b);
  const ys = [...new Set(points.map((p) => p.y))].sort((a, b) => a - b);
  if (xs.length < 2 || ys.length < 2) return "too_few";
  const xAt = new Map(xs.map((v, i) => [v, i])), yAt = new Map(ys.map((v, i) => [v, i]));
  const cell = new Int32Array(xs.length * ys.length).fill(-1);
  for (let i = 0; i < points.length; i++) {
    const at = yAt.get(points[i].y)! * xs.length + xAt.get(points[i].x)!;
    if (cell[at] !== -1) return "repeated";
    cell[at] = i;
  }
  let lowAt = 0, highAt = 0;
  points.forEach((p, i) => { if (p.z < points[lowAt].z) lowAt = i; if (p.z > points[highAt].z) highAt = i; });
  const zMin = points[lowAt].z, zSpan = points[highAt].z - zMin || 1;
  const xMin = xs[0], xSpan = xs.at(-1)! - xMin, yMin = ys[0], ySpan = ys.at(-1)! - yMin;
  const positions = new Float32Array(points.length * 3), levels = new Float32Array(points.length), rows = new Int32Array(points.length);
  points.forEach((p, i) => {
    const level = (p.z - zMin) / zSpan;
    positions.set([(p.x - xMin) / xSpan - 0.5, level, 0.5 - (p.y - yMin) / ySpan], i * 3);
    levels[i] = level;
    rows[i] = p.row;
  });
  const stepX = stepOf(xs), stepY = stepOf(ys);
  const triangles: number[] = [];
  for (let j = 0; j + 1 < ys.length; j++) {
    if (!near(ys[j + 1] - ys[j], stepY)) continue;
    for (let i = 0; i + 1 < xs.length; i++) {
      if (!near(xs[i + 1] - xs[i], stepX)) continue;
      const a = cell[j * xs.length + i], b = cell[j * xs.length + i + 1], c = cell[(j + 1) * xs.length + i], d = cell[(j + 1) * xs.length + i + 1];
      if (a < 0 || b < 0 || c < 0 || d < 0) continue;
      // Wound so each face's normal points up out of the surface, for the lighting.
      triangles.push(a, b, c, b, d, c);
    }
  }
  return { xs, ys, positions, levels, rows, indices: Uint32Array.from(triangles), low: points[lowAt].zText, high: points[highAt].zText, missing,
    gaps: { x: gapsOf(xs, stepX), y: gapsOf(ys, stepY) } };
}

// Viridis (eleven stops, as R's viridisLite writes them): even in lightness and readable with colour blindness.
export const VIRIDIS = ["#440154", "#482576", "#414487", "#35608D", "#2A788E", "#21908C", "#22A884", "#43BF71", "#7AD151", "#BBDF27", "#FDE725"];
/** The colour of a level between 0 and 1, between the two nearest stops. */
export function viridis(level: number): [number, number, number] {
  const t = Math.min(1, Math.max(0, level)) * (VIRIDIS.length - 1), i = Math.min(VIRIDIS.length - 2, Math.floor(t)), f = t - i;
  const rgb = (hex: string) => [1, 3, 5].map((k) => parseInt(hex.slice(k, k + 2), 16) / 255);
  const [a, b] = [rgb(VIRIDIS[i]), rgb(VIRIDIS[i + 1])];
  return [a[0] + (b[0] - a[0]) * f, a[1] + (b[1] - a[1]) * f, a[2] + (b[2] - a[2]) * f];
}
