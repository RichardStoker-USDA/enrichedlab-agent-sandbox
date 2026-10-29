// Pure layout work for the figure companion's 3D renderer. These functions never change a
// stored value: parsed numbers place marks, while display ticks are explicitly presentation.
// Keeping this module free of three.js makes the scientific mapping easy to test.
import { VIRIDIS } from "./views/surface/grid.ts";

export interface CompanionMathPoint {
  row: number;
  x: number;
  y: number;
  z: number;
  xText: string;
  yText: string;
  zText: string;
  group: string;
}

export interface CompanionTick {
  value: number;
  text: string;
  /** Position along the matching three.js axis. */
  at: number;
}

export interface CompanionMark {
  row: number;
  group: string;
  sourceX: number;
  sourceY: number;
  value: number;
  level: number;
  /** Top centre of the stored row's mark. */
  position: [number, number, number];
  /** One independent column from the z minimum to this row's stored z value. */
  column: { width: number; depth: number; base: number; height: number } | null;
}

export interface CompanionLayout {
  mode: "surface" | "points";
  fallback: "too_few" | "irregular" | "repeated" | "too_sparse" | null;
  marks: CompanionMark[];
  ticks: { x: CompanionTick[]; y: CompanionTick[]; z: CompanionTick[] };
  missingCells: number;
  floor: number;
  bounds: { width: number; height: number; depth: number };
}

export interface PaletteChoice {
  name: "viridis" | "grey" | "recorded";
  stops: string[];
  fromRequest: boolean;
}

// Use the accepted surface view's viridis-family stops so the two 3D readers cannot drift.
export const COMPANION_VIRIDIS = VIRIDIS;
export const COMPANION_GREY = [
  "#1A1A1A", "#2E2E2E", "#424242", "#575757", "#6B6B6B",
  "#808080", "#949494", "#A8A8A8", "#BDBDBD", "#D1D1D1",
];
export const COLUMN_FILL = 0.92;
export const COLUMN_SIDE_LIGHT = 0.62;

const HEX = /^#[0-9a-f]{6}$/i;
const near = (a: number, b: number) => Math.abs(a - b) <= 1e-9 * Math.max(1, Math.abs(a), Math.abs(b));
const clamp = (value: number, low = 0, high = 1) => Math.min(high, Math.max(low, value));
const NONFINITE_LAYOUT = "nonfinite_layout";
const finite = (value: number): number => {
  if (!Number.isFinite(value)) throw new RangeError(NONFINITE_LAYOUT);
  return value;
};

/** Top faces retain the palette colour; every other face gets one fixed separation factor. */
export const columnFaceLight = (normalY: number) => normalY > 0.5 ? 1 : COLUMN_SIDE_LIGHT;

export function companionPalette(requested: string | null, recorded: string[] = []): PaletteChoice {
  const kept = recorded.filter((colour) => HEX.test(colour));
  if (kept.length >= 2) return { name: "recorded", stops: kept, fromRequest: true };
  const name = requested?.trim().toLowerCase();
  if (name === "grey" || name === "gray") return { name: "grey", stops: COMPANION_GREY, fromRequest: true };
  return { name: "viridis", stops: COMPANION_VIRIDIS, fromRequest: name === "viridis" };
}

export function paletteColour(stops: string[], level: number): [number, number, number] {
  const colours = stops.length >= 2 ? stops : COMPANION_VIRIDIS;
  const at = clamp(level) * (colours.length - 1);
  const index = Math.min(colours.length - 2, Math.floor(at));
  const fraction = at - index;
  const rgb = (hex: string) => [1, 3, 5].map((place) => parseInt(hex.slice(place, place + 2), 16) / 255);
  const left = rgb(colours[index]), right = rgb(colours[index + 1]);
  return [0, 1, 2].map((channel) => left[channel] + (right[channel] - left[channel]) * fraction) as [number, number, number];
}

interface AxisGrid { values: number[]; first: number; last: number; step: number; expected: number }

function regularAxis(input: number[]): AxisGrid | null {
  const values = [...new Set(input)].sort((a, b) => a - b);
  if (values.length < 2) return null;
  let step = Infinity;
  for (let index = 1; index < values.length; index++) step = Math.min(step, finite(values[index] - values[index - 1]));
  if (!Number.isFinite(step) || step <= 0) return null;
  const first = values[0], last = values.at(-1)!;
  const intervals = Math.round(finite(finite(last - first) / step));
  if (intervals < 1 || intervals > 10_000 || !near(finite(first + finite(intervals * step)), last)) return null;
  if (values.some((value) => {
    const place = finite(finite(value - first) / step);
    return !near(place, Math.round(place));
  })) return null;
  return { values, first, last, step, expected: intervals + 1 };
}

export interface StoredNumericValue { value: number; text: string }

/** Choose retained values nearest evenly spaced positions, always keeping both stored ends. */
export function storedNumericTicks(values: StoredNumericValue[], limit = 6): StoredNumericValue[] {
  const firstText = new Map<number, string>();
  for (const item of values) if (Number.isFinite(item.value) && !firstText.has(item.value)) firstText.set(item.value, item.text);
  const sorted = [...firstText].map(([value, text]) => ({ value, text })).sort((a, b) => a.value - b.value);
  const count = Math.max(2, Math.floor(limit));
  if (sorted.length <= count) return sorted;
  const chosen = new Set<number>([0, sorted.length - 1]);
  const low = sorted[0].value, high = sorted.at(-1)!.value;
  for (let place = 1; place < count - 1; place++) {
    const target = low + ((high - low) * place) / (count - 1);
    let left = 0, right = sorted.length;
    while (left < right) {
      const middle = (left + right) >>> 1;
      if (sorted[middle].value < target) left = middle + 1; else right = middle;
    }
    const above = Math.min(sorted.length - 1, left), below = Math.max(0, above - 1);
    chosen.add(target - sorted[below].value <= sorted[above].value - target ? below : above);
  }
  return [...chosen].sort((a, b) => a - b).map((index) => sorted[index]);
}

/** Category labels remain retained strings in first-appearance order and are thinned by place. */
export function storedCategoryTicks(values: string[], limit = 6): { value: string; index: number; text: string }[] {
  const unique = [...new Set(values)];
  const count = Math.max(2, Math.floor(limit));
  if (unique.length <= count) return unique.map((value, index) => ({ value, index, text: value }));
  const chosen = new Set<number>();
  for (let place = 0; place < count; place++) chosen.add(Math.round((place * (unique.length - 1)) / (count - 1)));
  return [...chosen].sort((a, b) => a - b).map((index) => ({ value: unique[index], index, text: unique[index] }));
}

const numericTicks = (values: StoredNumericValue[], position: (value: number) => number): CompanionTick[] =>
  storedNumericTicks(values).map((tick) => ({ ...tick, at: position(tick.value) }));

const range = (values: number[]) => {
  if (!values.length) throw new RangeError(NONFINITE_LAYOUT);
  let low = Infinity, high = -Infinity;
  for (const value of values) {
    finite(value);
    low = Math.min(low, value);
    high = Math.max(high, value);
  }
  const storedSpan = finite(high - low);
  const span = storedSpan || 1;
  for (const value of values) finite(finite(value - low) / span);
  return { low, high, span };
};

/** A mapped numeric axis is placeable only when every layout subtraction and division stays finite. */
export function axisHasFiniteLayout(values: number[]): boolean {
  try { range(values); return true; }
  catch (error) {
    if (error instanceof RangeError && error.message === NONFINITE_LAYOUT) return false;
    throw error;
  }
}

function pointsLayout(points: CompanionMathPoint[], fallback: CompanionLayout["fallback"]): CompanionLayout {
  const xr = range(points.map((point) => point.x));
  const yr = range(points.map((point) => point.y));
  const zr = range(points.map((point) => point.z));
  const xAt = (value: number) => ((value - xr.low) / xr.span - 0.5) * 0.9;
  const yAt = (value: number) => (0.5 - (value - yr.low) / yr.span) * 0.9;
  const zAt = (value: number) => ((value - zr.low) / zr.span - 0.5) * 0.62;
  return {
    mode: "points",
    fallback,
    marks: points.map((point) => ({
      row: point.row,
      group: point.group,
      sourceX: point.x,
      sourceY: point.y,
      value: point.z,
      level: (point.z - zr.low) / zr.span,
      position: [xAt(point.x), zAt(point.z), yAt(point.y)],
      column: null,
    })),
    ticks: {
      x: numericTicks(points.map((point) => ({ value: point.x, text: point.xText })), xAt),
      y: numericTicks(points.map((point) => ({ value: point.y, text: point.yText })), yAt),
      z: numericTicks(points.map((point) => ({ value: point.z, text: point.zText })), zAt),
    },
    missingCells: 0,
    floor: -0.35,
    bounds: { width: 1, height: 0.66, depth: 1 },
  };
}

/** One floor-based column per stored row. A grid hole creates no geometry, and no rows are joined. */
export function columnLayout(points: CompanionMathPoint[]): CompanionLayout {
  const xCount = new Set(points.map((point) => point.x)).size;
  const yCount = new Set(points.map((point) => point.y)).size;
  if (xCount < 2 || yCount < 2) return pointsLayout(points, "too_few");
  const x = regularAxis(points.map((point) => point.x));
  const y = regularAxis(points.map((point) => point.y));
  if (!x || !y) return pointsLayout(points, "irregular");
  if (x.expected * y.expected > 1_000_000) return pointsLayout(points, "too_sparse");
  const occupied = new Set<string>();
  for (const point of points) {
    const xi = Math.round(finite(finite(point.x - x.first) / x.step));
    const yi = Math.round(finite(finite(point.y - y.first) / y.step));
    if (!near(point.x, finite(x.first + finite(xi * x.step))) || !near(point.y, finite(y.first + finite(yi * y.step))))
      return pointsLayout(points, "irregular");
    const key = `${xi}:${yi}`;
    if (occupied.has(key)) return pointsLayout(points, "repeated");
    occupied.add(key);
  }
  // A jump must be a whole number of the smallest observed step. This permits genuine gaps,
  // such as one absent block, without treating arbitrary spacing as a grid.
  const aligned = (axis: AxisGrid) => axis.values.every((value) => {
    const place = finite(finite(value - axis.first) / axis.step);
    return near(place, Math.round(place));
  });
  if (!aligned(x) || !aligned(y)) return pointsLayout(points, "irregular");

  const zr = range(points.map((point) => point.z));
  const xExtent = finite(finite(x.last - x.first) + x.step);
  const yExtent = finite(finite(y.last - y.first) + y.step);
  const xBase = finite(x.first - x.step / 2), yBase = finite(y.first - y.step / 2);
  const xAt = (value: number) => finite(finite(finite(value - xBase) / xExtent) - 0.5);
  const yAt = (value: number) => finite(0.5 - finite(finite(value - yBase) / yExtent));
  const floor = -0.31;
  const zAt = (value: number) => finite(floor + finite(finite(value - zr.low) / zr.span) * 0.62);
  const width = finite(finite(x.step / xExtent) * COLUMN_FILL);
  const depth = finite(finite(y.step / yExtent) * COLUMN_FILL);
  return {
    mode: "surface",
    fallback: null,
    marks: points.map((point) => ({
      row: point.row,
      group: point.group,
      sourceX: point.x,
      sourceY: point.y,
      value: point.z,
      level: (point.z - zr.low) / zr.span,
      position: [xAt(point.x), zAt(point.z), yAt(point.y)],
      column: { width, depth, base: floor, height: zAt(point.z) - floor },
    })),
    ticks: {
      x: numericTicks(points.map((point) => ({ value: point.x, text: point.xText })), xAt),
      y: numericTicks(points.map((point) => ({ value: point.y, text: point.yText })), yAt),
      z: numericTicks(points.map((point) => ({ value: point.z, text: point.zText })), zAt),
    },
    missingCells: x.expected * y.expected - points.length,
    floor,
    bounds: { width: 1, height: 0.62, depth: 1 },
  };
}

export function companionLayout(points: CompanionMathPoint[], surface: boolean): CompanionLayout {
  if (!surface) return pointsLayout(points, null);
  return columnLayout(points);
}

/** Run the same arithmetic as the renderer before an accepted retained table reaches the GPU. */
export function coordinatesHaveFiniteLayout(points: CompanionMathPoint[], surface: boolean): boolean {
  try { companionLayout(points, surface); return true; }
  catch (error) {
    if (error instanceof RangeError && error.message === NONFINITE_LAYOUT) return false;
    throw error;
  }
}

export interface CameraFit {
  position: [number, number, number];
  target: [number, number, number];
  distance: number;
  azimuth: number;
  elevation: number;
  /** Extra projection-axis scales that make the projected box occupy the target in both axes. */
  projectionScale: [number, number];
}

/** Fit all eight corners at a fixed readable angle and frame them to the target in both screen axes. */
export function fitCompanionCamera(
  bounds: { width: number; height: number; depth: number },
  aspect: number,
  verticalFov = 40,
  azimuth = 35,
  elevation = 25,
  target = 0.85,
): CameraFit {
  const a = (azimuth * Math.PI) / 180, e = (elevation * Math.PI) / 180;
  const direction: [number, number, number] = [Math.sin(a) * Math.cos(e), Math.sin(e), Math.cos(a) * Math.cos(e)];
  const right: [number, number, number] = [Math.cos(a), 0, -Math.sin(a)];
  const forward: [number, number, number] = direction.map((value) => -value) as [number, number, number];
  const up: [number, number, number] = [
    right[1] * forward[2] - right[2] * forward[1],
    right[2] * forward[0] - right[0] * forward[2],
    right[0] * forward[1] - right[1] * forward[0],
  ];
  const tanV = Math.tan((verticalFov * Math.PI) / 360);
  const tanH = tanV * Math.max(0.05, aspect);
  const padding = 1 / clamp(target, 0.1, 0.98);
  let distance = 0;
  for (const x of [-bounds.width / 2, bounds.width / 2]) for (const y of [-bounds.height / 2, bounds.height / 2]) {
    for (const z of [-bounds.depth / 2, bounds.depth / 2]) {
      const depth = x * direction[0] + y * direction[1] + z * direction[2];
      const horizontal = Math.abs(x * right[0] + y * right[1] + z * right[2]);
      const vertical = Math.abs(x * up[0] + y * up[1] + z * up[2]);
      distance = Math.max(distance, depth + (horizontal * padding) / tanH, depth + (vertical * padding) / tanV);
    }
  }
  let projectedX = 0, projectedY = 0;
  for (const x of [-bounds.width / 2, bounds.width / 2]) for (const y of [-bounds.height / 2, bounds.height / 2]) {
    for (const z of [-bounds.depth / 2, bounds.depth / 2]) {
      const viewDepth = distance - (x * direction[0] + y * direction[1] + z * direction[2]);
      projectedX = Math.max(projectedX, Math.abs(x * right[0] + y * right[1] + z * right[2]) / (viewDepth * tanH));
      projectedY = Math.max(projectedY, Math.abs(x * up[0] + y * up[1] + z * up[2]) / (viewDepth * tanV));
    }
  }
  return {
    position: direction.map((value) => value * distance) as [number, number, number],
    target: [0, 0, 0],
    distance,
    azimuth,
    elevation,
    projectionScale: [target / projectedX, target / projectedY],
  };
}
