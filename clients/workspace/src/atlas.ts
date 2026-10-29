import type { ResearchStep, Trajectory } from "./trajectory.ts";

export interface AtlasFilter {
  query: string;
  actor: string;
  segment: string;
  state: string;
}
export interface AtlasGroup {
  actor: string;
  segment: number;
  steps: ResearchStep[];
}
export interface AtlasPage {
  steps: ResearchStep[];
  groups: AtlasGroup[];
  page: number;
  pageSize: number;
  pageCount: number;
  total: number;
}

export const ATLAS_PAGE_SIZE = 50;
export const initialAtlasFilter = (): AtlasFilter => ({ query: "", actor: "", segment: "", state: "" });

export function atlasActors(steps: ResearchStep[]): string[] {
  return [...new Set(steps.map((step) => step.actor))].sort((a, b) => a.localeCompare(b));
}
export function atlasSegments(steps: ResearchStep[]): number[] {
  return [...new Set(steps.map((step) => step.segment))].sort((a, b) => a - b);
}
export function atlasOverview(trajectory: Trajectory): { capturedEvents: number; toolSteps: number; actors: number; segments: number } {
  return { capturedEvents: trajectory.events, toolSteps: trajectory.steps.length, actors: atlasActors(trajectory.steps).length, segments: atlasSegments(trajectory.steps).length };
}
export function atlasPage(steps: ResearchStep[], filter: AtlasFilter, requestedPage: number, pageSize = ATLAS_PAGE_SIZE): AtlasPage {
  const query = filter.query.trim().toLocaleLowerCase();
  const matching = steps.filter((step) =>
    (!filter.actor || step.actor === filter.actor) &&
    (!filter.segment || String(step.segment) === filter.segment) &&
    (!filter.state || step.state === filter.state) &&
    (!query || `${step.title} ${step.kind} ${step.actor} ${step.segment} ${step.sequence}`.toLocaleLowerCase().includes(query)),
  );
  const pageCount = Math.max(1, Math.ceil(matching.length / pageSize));
  const page = Math.min(Math.max(1, requestedPage), pageCount);
  const visible = matching.slice((page - 1) * pageSize, page * pageSize);
  const groups = new Map<string, AtlasGroup>();
  for (const step of visible) {
    const key = `${step.actor}\u0000${step.segment}`;
    const group = groups.get(key) ?? { actor: step.actor, segment: step.segment, steps: [] };
    group.steps.push(step); groups.set(key, group);
  }
  return { steps: visible, groups: [...groups.values()], page, pageSize, pageCount, total: matching.length };
}
