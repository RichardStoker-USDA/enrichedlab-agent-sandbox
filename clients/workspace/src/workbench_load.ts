import { loadMany, loadTaskEvidence } from "./evidence.ts";
import type { AuditRecord } from "./history.ts";
import { activeTask } from "./language.ts";
import { projects } from "./projects.ts";
import type { ProjectModel } from "./workbench.ts";

// Everything the workbench shows for one project, read the way the project page reads it. Retained evidence is
// read once per finished analysis and kept across reloads; the broker checks access on every request.
export async function loadProjectModel(projectId: string, previous: ProjectModel | null, signal?: AbortSignal): Promise<ProjectModel> {
  const project = await projects.get(projectId, signal);
  const [runList, checkpoints, auditPage, reviewList] = await Promise.all([
    projects.runs(projectId, signal), projects.allHistory(projectId, signal), projects.audit(projectId, signal),
    projects.reviews(projectId, signal).catch(() => null),
  ]);
  const runs = await loadMany(runList.runs, 3, async (run) => ({
    run, turns: (await projects.turns(run.run_id, signal)).turns, events: await projects.runEvents(run.run_id, signal),
  }));
  const members = project.capabilities.includes("manage") ? await projects.members(projectId, signal) : null;
  const evidence = { ...(previous?.evidence ?? {}) };
  const needs = [...project.tasks, ...(project.lab_data ?? [])].filter((t) => !activeTask(t.status) && evidence[t.task_id] === undefined);
  const loaded = await loadMany(needs, 3, (t) => loadTaskEvidence(t.task_id, signal).catch(() => null));
  const evidenceFailed: string[] = [];
  loaded.forEach((item, i) => { if (item) evidence[needs[i].task_id] = item; else evidenceFailed.push(needs[i].task_id); });
  const reviews = reviewList ? Object.fromEntries(reviewList.map((r) => [r.task_id, r])) : undefined;
  return { project, runs, checkpoints, audit: auditPage.events as AuditRecord[], members, evidence, evidenceFailed, reviews };
}

// Something is still running, so the model should be read again shortly.
export const busy = (model: ProjectModel) =>
  model.project.tasks.some((t) => activeTask(t.status)) || model.runs.some(({ run }) => ["queued", "running", "waiting"].includes(run.status));
