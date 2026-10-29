/** Pure interaction policies for the bounded figure companion. */

export interface VirtualWindow {
  first: number;
  last: number;
  offset: number;
  totalHeight: number;
}

/** Return only the fixed-height rows near a scroll viewport. */
export function virtualWindow(
  total: number,
  scrollTop: number,
  viewportHeight: number,
  rowHeight: number,
  overscan = 3,
): VirtualWindow {
  const count = Math.max(0, Math.floor(total));
  const height = Math.max(1, rowHeight);
  const top = Math.max(0, scrollTop);
  const first = Math.min(count, Math.max(0, Math.floor(top / height) - overscan));
  const last = Math.min(count, Math.ceil((top + Math.max(0, viewportHeight)) / height) + overscan);
  return { first, last, offset: first * height, totalHeight: count * height };
}

/** Null means there is no grouping channel. The empty string remains a real group value. */
export const groupVisible = (group: string | null, hidden: ReadonlySet<string>) =>
  group === null || !hidden.has(group);

interface HitArea {
  row: number;
  centreX: number;
  centreY: number;
  left: number;
  top: number;
  right: number;
  bottom: number;
  depth: number;
  radius: number | null;
  polygon: { x: number; y: number }[] | null;
}

const cross = (origin: { x: number; y: number }, a: { x: number; y: number }, b: { x: number; y: number }) =>
  (a.x - origin.x) * (b.y - origin.y) - (a.y - origin.y) * (b.x - origin.x);

function convexHull(points: { x: number; y: number }[]) {
  const sorted = [...points].sort((a, b) => a.x - b.x || a.y - b.y);
  if (sorted.length <= 2) return sorted;
  const lower: { x: number; y: number }[] = [], upper: { x: number; y: number }[] = [];
  for (const point of sorted) {
    while (lower.length >= 2 && cross(lower.at(-2)!, lower.at(-1)!, point) <= 0) lower.pop();
    lower.push(point);
  }
  for (let index = sorted.length - 1; index >= 0; index--) {
    const point = sorted[index];
    while (upper.length >= 2 && cross(upper.at(-2)!, upper.at(-1)!, point) <= 0) upper.pop();
    upper.push(point);
  }
  lower.pop(); upper.pop();
  return [...lower, ...upper];
}

function insidePolygon(x: number, y: number, polygon: { x: number; y: number }[]) {
  let inside = false;
  for (let left = polygon.length - 1, right = 0; right < polygon.length; left = right++) {
    const a = polygon[left], b = polygon[right];
    if ((a.y > y) !== (b.y > y) && x < ((b.x - a.x) * (y - a.y)) / (b.y - a.y) + a.x) inside = !inside;
  }
  return inside;
}

/**
 * A screen-space grid rebuilt when a layout or camera changes. Pointer lookup visits one
 * small bucket rather than every drawn mark; range lookup visits only intersected buckets.
 */
export class ScreenGridIndex {
  readonly cellSize: number;
  lastCandidateCount = 0;
  private readonly cells = new Map<string, HitArea[]>();

  constructor(cellSize = 32) {
    this.cellSize = Math.max(8, cellSize);
  }

  clear() {
    this.cells.clear();
    this.lastCandidateCount = 0;
  }

  private key(x: number, y: number) {
    return `${Math.floor(x / this.cellSize)}:${Math.floor(y / this.cellSize)}`;
  }

  private add(area: HitArea) {
    const x0 = Math.floor(area.left / this.cellSize), x1 = Math.floor(area.right / this.cellSize);
    const y0 = Math.floor(area.top / this.cellSize), y1 = Math.floor(area.bottom / this.cellSize);
    for (let y = y0; y <= y1; y++) for (let x = x0; x <= x1; x++) {
      const key = `${x}:${y}`;
      const bucket = this.cells.get(key) ?? [];
      bucket.push(area);
      this.cells.set(key, bucket);
    }
  }

  addPoint(row: number, x: number, y: number, depth: number, radius = 12) {
    this.add({ row, centreX: x, centreY: y, left: x - radius, top: y - radius,
      right: x + radius, bottom: y + radius, depth, radius, polygon: null });
  }

  addRectangle(
    row: number,
    centreX: number,
    centreY: number,
    left: number,
    top: number,
    right: number,
    bottom: number,
    depth: number,
  ) {
    this.add({ row, centreX, centreY, left: Math.min(left, right), top: Math.min(top, bottom),
      right: Math.max(left, right), bottom: Math.max(top, bottom), depth, radius: null, polygon: null });
  }

  addPolygon(row: number, centreX: number, centreY: number, points: { x: number; y: number }[], depth: number) {
    const polygon = convexHull(points);
    if (polygon.length < 3) return;
    this.add({ row, centreX, centreY,
      left: Math.min(...polygon.map((point) => point.x)), top: Math.min(...polygon.map((point) => point.y)),
      right: Math.max(...polygon.map((point) => point.x)), bottom: Math.max(...polygon.map((point) => point.y)),
      depth, radius: null, polygon });
  }

  hit(x: number, y: number): number | null {
    const candidates = this.cells.get(this.key(x, y)) ?? [];
    this.lastCandidateCount = candidates.length;
    let found: HitArea | null = null;
    let bestDistance = Infinity;
    for (const area of candidates) {
      if (area.polygon && !insidePolygon(x, y, area.polygon)) continue;
      const distance = area.radius === null
        ? Math.hypot(Math.max(area.left - x, 0, x - area.right), Math.max(area.top - y, 0, y - area.bottom))
        : Math.hypot(area.centreX - x, area.centreY - y);
      if (area.radius !== null && distance > area.radius) continue;
      if (distance < bestDistance || (distance === bestDistance && (!found || area.depth < found.depth))) {
        found = area;
        bestDistance = distance;
      }
    }
    return found?.row ?? null;
  }

  rowsIn(rect: { left: number; top: number; right: number; bottom: number }): number[] {
    const left = Math.min(rect.left, rect.right), right = Math.max(rect.left, rect.right);
    const top = Math.min(rect.top, rect.bottom), bottom = Math.max(rect.top, rect.bottom);
    const x0 = Math.floor(left / this.cellSize), x1 = Math.floor(right / this.cellSize);
    const y0 = Math.floor(top / this.cellSize), y1 = Math.floor(bottom / this.cellSize);
    const rows = new Set<number>();
    let candidates = 0;
    for (let y = y0; y <= y1; y++) for (let x = x0; x <= x1; x++) {
      for (const area of this.cells.get(`${x}:${y}`) ?? []) {
        candidates++;
        if (area.centreX >= left && area.centreX <= right && area.centreY >= top && area.centreY <= bottom)
          rows.add(area.row);
      }
    }
    this.lastCandidateCount = candidates;
    return [...rows].sort((a, b) => a - b);
  }
}
