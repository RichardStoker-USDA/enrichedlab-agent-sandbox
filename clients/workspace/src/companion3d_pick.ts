export interface CompanionInstanceIntersection {
  distance: number;
  instanceId?: number;
}

export interface CompanionPointIntersection {
  distance: number;
  index?: number;
}

/** Three.js returns intersections nearest first; instance order never decides the picked row. */
export function nearestVisibleInstanceRow(
  intersections: readonly CompanionInstanceIntersection[],
  rows: readonly number[],
  groups: readonly string[],
  visibleGroups: ReadonlySet<string>,
): number | null {
  for (const intersection of intersections) {
    const index = intersection.instanceId;
    if (index === undefined || index < 0 || index >= rows.length || !visibleGroups.has(groups[index] ?? "")) continue;
    return rows[index];
  }
  return null;
}

/** Keep Three.js depth ordering, then apply the point mark's fixed CSS-pixel hit radius. */
export function nearestVisiblePointRow(
  intersections: readonly CompanionPointIntersection[],
  rows: readonly number[],
  groups: readonly string[],
  visibleGroups: ReadonlySet<string>,
  insidePixelRadius: (index: number) => boolean,
): number | null {
  for (const intersection of intersections) {
    const index = intersection.index;
    if (index === undefined || index < 0 || index >= rows.length || !visibleGroups.has(groups[index] ?? "")) continue;
    if (insidePixelRadius(index)) return rows[index];
  }
  return null;
}
