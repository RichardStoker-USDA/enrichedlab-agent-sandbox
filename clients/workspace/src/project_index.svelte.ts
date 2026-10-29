import { BrokerError } from "./api.ts";
import { projectForTask } from "./context.ts";
import { loadMany } from "./evidence.ts";
import { serial } from "./live.ts";
import { activeTask, type Tone } from "./language.ts";
import { projects, type Project, type ProjectSummary } from "./projects.ts";
import { standingOf, type IndexFacts, type Standing } from "./standing.ts";

// The reader's projects, shared by the rail and the lab index. The broker's list says where
// each project stands; only a list from an older broker loads each project's record for it.
// The shell reads it again when a project changes (live.ts). A read while one runs waits for it and reads once more,
// so an older list never replaces a newer one; a failed read keeps the list the broker last gave.
class ProjectIndex {
  list = $state<ProjectSummary[]>([]);
  details = $state<Record<string, Project>>({});
  phase = $state<"idle" | "loading" | "ready" | "failed">("idle");
  failure = $state("");
  /** When the list was last read, for a page that opens on a copy that may be old (no change stream). */
  readAt = 0;

  readonly load = serial(async () => {
    if (this.phase !== "ready") this.phase = "loading";
    try {
      const result = await projects.list();
      this.list = result.projects;
      this.phase = "ready";
      this.failure = "";
      this.readAt = Date.now();
      if (result.projects.every((p) => p.task_counts)) return;
      await loadMany(result.projects.slice(0, 60), 4, async (p) => {
        try { this.remember(await projects.get(p.project_id)); } catch { /* the entry still opens */ }
      });
    } catch (cause) {
      if (this.phase === "ready") return;
      this.phase = "failed";
      this.failure = cause instanceof BrokerError && cause.code === "access_required"
        ? "Your session needs to sign in again." : "Your projects could not be listed. Reload to try again.";
    }
  });

  // A page that loads a project shares it, so the rail never shows an older state.
  remember(project: Project) {
    this.details = { ...this.details, [project.project_id]: project };
    this.list = this.list.map((p) => (p.project_id === project.project_id ? { ...p, title: project.title, updated_at: project.updated_at } : p));
  }
}

export const projectIndex = new ProjectIndex();

// The facts for one index row: the list's summary, or the project's own record from an older broker.
export function factsFor(p: ProjectSummary, detail: Project | undefined = projectIndex.details[p.project_id]): IndexFacts {
  if (p.task_counts) return { question: p.question ?? null, capabilities: p.capabilities ?? [], latest_run: p.latest_run ?? null, task_counts: p.task_counts };
  const tasks = detail?.tasks ?? [];
  return {
    question: detail?.checkpoint?.state.goal ?? null,
    capabilities: detail?.capabilities ?? [],
    latest_run: null,
    task_counts: { total: tasks.length, active: tasks.filter((t) => activeTask(t.status)).length,
      finished: tasks.filter((t) => t.status === "completed" || t.status === "review_required").length,
      failed: tasks.filter((t) => t.status === "failed").length },
  };
}

// The lab index's filters. The rail's places set them; they live for this visit only.
export const indexFilter = $state<{ labs: string[]; who: "anyone" | "me" | "colleagues"; where: "any" | Standing; search: string }>(
  { labs: [], who: "anyone", where: "any", search: "" });
export function resetFilter(next: Partial<typeof indexFilter> = {}) {
  indexFilter.labs = next.labs ?? []; indexFilter.who = next.who ?? "anyone"; indexFilter.where = next.where ?? "any"; indexFilter.search = next.search ?? "";
}
export const standingFor = (p: ProjectSummary) => standingOf(factsFor(p));

// Where a project's work stands, in words.
export function standing(project: Project | undefined): { label: string; tone: Tone } {
  if (!project) return { label: "", tone: "quiet" };
  if (!project.checkpoint) return { label: "No question yet", tone: "quiet" };
  const running = project.tasks.filter((t) => activeTask(t.status)).length;
  if (running) return { label: running === 1 ? "An analysis is running" : `${running} analyses running`, tone: "active" };
  const finished = project.tasks.filter((t) => t.status === "completed" || t.status === "review_required").length;
  const failed = project.tasks.filter((t) => t.status === "failed").length;
  if (finished) return { label: `${finished} ${finished === 1 ? "result" : "results"}${failed ? `, ${failed} did not finish` : ""}`, tone: "done" };
  if (failed) return { label: `${failed} did not finish`, tone: "failed" };
  return { label: "Not started", tone: "quiet" };
}

// The figure a reader opened from a project's contents page: the analysis page shows the
// same drawn plate until its own verified figures are ready, then scrolls to that figure.
// It holds only an in-memory picture already checked by the plate, never a URL route.
export const chosenFigure = $state({ task: "", path: "", preview: "" });

// The title of the page being read, for the masthead and the browser tab.
export const pageTitle = $state({ value: "" });
// Whether the agent is working on the project the page shows; the project page and the Detailed view set it, and the
// tab title says so, so a person who walked away can see it from the tab strip.
export const openWork = $state({ busy: false });

// The open project's sections, shown under it in the rail. The page sets the list and
// marks the section being read; the rail only reads it.
// A page may take the rail's clicks itself (`go`), for example to open a tab.
export const pageOutline = $state<{ project: string; sections: { id: string; label: string }[]; active: string; go?: (id: string) => void }>({ project: "", sections: [], active: "" });
