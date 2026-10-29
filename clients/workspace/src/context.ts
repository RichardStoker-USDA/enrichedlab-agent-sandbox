import type { Project } from "./projects.ts";

// Where a record sits: its lab and its project. Only recorded associations are used; a
// search that finds nothing is reported as that, never turned into a claim.

// Operator labs hold the platform's own qualification runs, not a lab's research. Their keys end in "-qualification"
// (research onboarding never gives a research lab that ending), and the workspace keeps them out of the default view:
// they sit apart in the lab menu and their projects show only when the operator lab is chosen.
export const operatorLab = (key: string) => key.endsWith("-qualification");

// Research labs first, then operator labs, each in the broker's order.
export function labOrder<T extends { key: string }>(groups: T[]): T[] {
  return [...groups.filter((g) => !operatorLab(g.key)), ...groups.filter((g) => operatorLab(g.key))];
}

// The lab to show: the one already chosen, else the one saved in this browser, else the first research lab.
export function chooseLab(keys: string[], current: string, stored: string): string {
  if (keys.includes(current)) return current;
  if (keys.includes(stored)) return stored;
  return keys.find((key) => !operatorLab(key)) ?? keys[0] ?? "";
}

// The project whose recorded task list contains this analysis.
export function projectForTask(details: Record<string, Project>, taskId: string): Project | null {
  for (const project of Object.values(details)) if (project.tasks.some((t) => t.task_id === taskId)) return project;
  return null;
}

// Who can open a project, in the words a scientist uses.
export function sharing(visibility: "private" | "lab", labName: string): string {
  return visibility === "lab" ? `Shared with ${labName}` : "Private: you and people you invite";
}
