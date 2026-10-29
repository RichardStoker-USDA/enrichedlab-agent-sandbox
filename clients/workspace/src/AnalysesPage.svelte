<script lang="ts">
  import { onDestroy, untrack } from "svelte";
  import { broker } from "./api.ts";
  import { projectForTask } from "./context.ts";
  import { offeredProfiles, type Identity, type Task } from "./contracts.ts";
  import { loadMany } from "./evidence.ts";
  import { currentLab, lab } from "./lab.svelte.ts";
  import { activeTask, duration, taskStatus, when } from "./language.ts";
  import { live } from "./live.ts";
  import { projectIndex } from "./project_index.svelte.ts";
  import { projects, taskTitle } from "./projects.ts";
  import StatusMark from "./StatusMark.svelte";

  // Every analysis in this lab the account can read, set out like the jobs table of an HPC portal: one row per
  // analysis, sortable by its column heads, filtered by where it stands. An analysis that belongs to a project names
  // the project whose record lists it; one run on its own says so.
  let { identity, openTask, newAnalysis }: { identity: Identity; openTask: (id: string, project?: string) => void; newAnalysis: () => void } = $props();
  let tasks = $state<Task[]>([]);
  let filter = $state("");
  let standing = $state<"all" | "active" | "finished" | "attention" | "failed">("all");
  let sortKey = $state<"requested" | "status" | "took">("requested");
  let descending = $state(true);
  let loading = $state(true);
  let failure = $state("");
  const controller = new AbortController();
  // A lab dataset's tasks are not analyses; they leave the list once their project is read.
  const labData = $derived(new Set(Object.values(projectIndex.details).flatMap((p) => (p.lab_data ?? []).map((t) => t.task_id))));
  const inLab = $derived(tasks.filter((t) => t.group === lab.key && !labData.has(t.id)));
  const labName = $derived(currentLab(identity)?.name ?? "this lab");
  const projectOf = (id: string) => projectForTask(projectIndex.details, id);
  const methodName = (t: Task) => identity.profiles.find((p) => p.key === t.profile)?.name ?? t.profile;
  // An analysis the agent planned has no objective of its own: its plan step's title names it, so rows of one method
  // read differently (every row read as the method's name).
  const stepTitle = (t: Task) => {
    const project = projectOf(t.id);
    return project?.checkpoint?.state.plan.find((s) => s.task_id === t.id)?.title ?? taskTitle(project, t.id);
  };
  const question = (t: Task) => t.objective ?? stepTitle(t) ?? methodName(t);
  // The broker's list of analyses names no project, so the page reads the lab's projects, newest first and at most
  // 60, four at a time, and the Project column and step titles fill in as each arrives. Read projects are shared with
  // the rest of the workspace, so a project already read is not read again.
  const DETAIL_LIMIT = 60;
  const asked = new Set<string>();
  $effect(() => {
    if (projectIndex.phase !== "ready") return;
    const key = lab.key;
    const c = new AbortController();
    untrack(() => {
      const wanted = projectIndex.list
        .filter((p) => p.group_key === key && !projectIndex.details[p.project_id] && !asked.has(p.project_id))
        .toSorted((a, b) => b.updated_at.localeCompare(a.updated_at)).slice(0, DETAIL_LIMIT);
      for (const p of wanted) asked.add(p.project_id);
      void loadMany(wanted, 4, async (p) => {
        if (c.signal.aborted) { asked.delete(p.project_id); return; }
        try { projectIndex.remember(await projects.get(p.project_id, c.signal)); }
        catch { asked.delete(p.project_id); }
      });
    });
    return () => c.abort();
  });
  // How long a finished analysis took, from its own start and finish times; nothing is estimated for work still going.
  function took(t: Task): number | null {
    if (!t.started_at || !t.finished_at) return null;
    const ms = new Date(t.finished_at).getTime() - new Date(t.started_at).getTime();
    return Number.isFinite(ms) && ms >= 0 ? ms : null;
  }
  const group = (t: Task) => activeTask(t.status) ? "active" : t.status === "completed" ? "finished" : t.status === "review_required" ? "attention" : t.status === "failed" ? "failed" : "other";
  const counts = $derived({
    all: inLab.length,
    active: inLab.filter((t) => group(t) === "active").length,
    attention: inLab.filter((t) => group(t) === "attention").length,
    finished: inLab.filter((t) => group(t) === "finished").length,
    failed: inLab.filter((t) => group(t) === "failed").length,
  });
  const standingNames = { all: "All", active: "Running or waiting to start", attention: "Stopped for review", finished: "Finished", failed: "Did not finish" } as const;
  const statusRank: Record<string, number> = { attention: 0, active: 1, failed: 2, finished: 3, other: 4 };
  const visible = $derived(inLab
    .filter((t) => standing === "all" || group(t) === standing)
    .filter((t) => `${t.objective ?? ""} ${methodName(t)} ${when(t.submitted_at)} ${projectOf(t.id)?.title ?? ""}`.toLowerCase().includes(filter.trim().toLowerCase()))
    .toSorted((a, b) => {
      const order = sortKey === "status" ? statusRank[group(a)] - statusRank[group(b)]
        : sortKey === "took" ? (took(a) ?? -1) - (took(b) ?? -1)
        : new Date(a.submitted_at).getTime() - new Date(b.submitted_at).getTime();
      return descending ? -order : order;
    }));
  function sortBy(key: typeof sortKey) {
    if (sortKey === key) descending = !descending;
    else { sortKey = key; descending = key !== "status"; }
  }
  const sortState = (key: typeof sortKey) => (sortKey === key ? (descending ? "descending" : "ascending") : "none");
  // The list reads again when any analysis changes (live.ts), and without the change stream every minute while shown. A
  // failed read again keeps the rows the page already shows.
  async function read() {
    try { tasks = (await broker.tasks(controller.signal)).tasks; failure = ""; }
    catch { if (!controller.signal.aborted && loading) failure = "Your analyses could not be listed. Reload to try again."; }
    finally { loading = false; }
  }
  void read();
  const unwatch = live().watch(["tasks"], () => read(), { poll: true, gapMs: 2_000 });
  onDestroy(() => { controller.abort(); unwatch(); });
</script>

<section class="analyses">
  <header>
    <div><h1>Results</h1><p class="soft">Every analysis in {labName} that you can open. An analysis that belongs to a project also appears on that project.</p></div>
    {#if offeredProfiles(identity, lab.key).length}<button class="button secondary" onclick={newAnalysis}>Run one method</button>{/if}
  </header>
  {#if loading}<p class="soft" role="status">Opening your analyses…</p>
  {:else if failure}<p class="notice error" role="alert">{failure}</p>
  {:else}
    <div class="controls">
      <div class="standings" role="group" aria-label="Show analyses">
        {#each Object.keys(standingNames) as key (key)}
          {@const k = key as keyof typeof standingNames}
          <button aria-pressed={standing === k} onclick={() => (standing = k)}>{standingNames[k]} <span class="count">{counts[k]}</span></button>
        {/each}
      </div>
      {#if inLab.length > 8}<input type="search" placeholder="Find by question, project, method or date" aria-label="Find an analysis" bind:value={filter} />{/if}
    </div>
    <table class="jobs">
      <caption class="visually-hidden">Analyses in {labName}, sorted by {sortKey === "requested" ? "when they were requested" : sortKey === "status" ? "status" : "how long they took"}</caption>
      <thead>
        <tr>
          <th scope="col" aria-sort={sortState("status")}><button onclick={() => sortBy("status")}>Status</button></th>
          <th scope="col">Analysis</th>
          <th scope="col">Project</th>
          <th scope="col">Method</th>
          <th scope="col" aria-sort={sortState("requested")}><button onclick={() => sortBy("requested")}>Requested</button></th>
          <th scope="col" aria-sort={sortState("took")}><button onclick={() => sortBy("took")}>Took</button></th>
        </tr>
      </thead>
      <tbody>
        {#each visible as task (task.id)}
          {@const s = taskStatus(task.status)}
          {@const project = projectOf(task.id)}
          <tr>
            <td class="status-cell"><StatusMark tone={s.tone} label={s.label} /></td>
            <td class="question"><a href={project ? `#/projects/${project.project_id}/analyses/${task.id}` : `#/analyses/${task.id}`} onclick={(e) => { e.preventDefault(); openTask(task.id, project?.project_id); }}>{question(task)}</a></td>
            <td class="project">{project?.title ?? ""}</td>
            <td class="method">{methodName(task)}</td>
            <td class="when">{when(task.submitted_at)}</td>
            <td class="took">{duration(took(task))}</td>
          </tr>
        {/each}
      </tbody>
    </table>
    {#if !visible.length}<p class="soft">{filter || standing !== "all" ? "Nothing matches that choice." : `No analyses in ${labName} yet.`}</p>{/if}
  {/if}
</section>

<style>
  .analyses { padding: 36px var(--gutter) 24px; display: grid; gap: 20px; width: 100%; max-width: 1080px; margin: 0 auto; align-content: start; }
  header { display: flex; justify-content: space-between; align-items: flex-end; gap: 16px; flex-wrap: wrap; }
  h1 { font-size: clamp(30px, calc(3.4vw * var(--type-scale)), calc(42px * var(--type-scale))); letter-spacing: -0.022em; }
  header p { margin-top: 8px; max-width: 60ch; }
  .controls { display: flex; flex-wrap: wrap; align-items: center; justify-content: space-between; gap: 12px 20px; }
  .standings { display: flex; flex-wrap: wrap; gap: 4px 0; border-bottom: 1px solid var(--line); }
  .standings button { position: relative; padding: 8px 12px 10px; font-size: var(--font-small); font-weight: 600; color: var(--text-2); }
  .standings button:hover { color: var(--heading); }
  .standings button[aria-pressed="true"] { color: var(--heading); }
  .standings button[aria-pressed="true"]::after { content: ""; position: absolute; left: 12px; right: 12px; bottom: -1px; height: 3px; background: var(--mark); }
  .count { font-variant-numeric: tabular-nums; color: var(--text-3); font-size: var(--font-xs); font-weight: 600; }
  input { max-width: 380px; }
  .jobs { width: 100%; border-collapse: collapse; border-top: 2px solid var(--heading); }
  .jobs th { padding: 10px 14px 10px 0; border-bottom: 1px solid var(--line-2); font-size: var(--font-xs); font-weight: 700; color: var(--text-2); text-align: left; white-space: nowrap; }
  .jobs th button { font: inherit; color: inherit; }
  .jobs th button::after { content: " ↕"; color: var(--text-3); }
  .jobs th[aria-sort="ascending"] button::after { content: " ↑"; color: var(--heading); }
  .jobs th[aria-sort="descending"] button::after { content: " ↓"; color: var(--heading); }
  .jobs th button:hover { color: var(--heading); }
  .jobs td { padding: 12px 14px 12px 0; border-bottom: 1px solid var(--line); vertical-align: top; font-size: var(--font-small); color: var(--text-2); }
  .jobs tbody tr:hover { background: var(--hover-wash); }
  .question { min-width: 16rem; }
  .question a { color: var(--heading); font-weight: 650; font-size: var(--font-body); line-height: 1.4; text-decoration: none; display: -webkit-box; -webkit-line-clamp: 2; line-clamp: 2; -webkit-box-orient: vertical; overflow: hidden; }
  .question a:hover { text-decoration: underline; text-underline-offset: 3px; }
  .status-cell { white-space: nowrap; }
  .when, .took { white-space: nowrap; font-variant-numeric: tabular-nums; }
  .visually-hidden { position: absolute; width: 1px; height: 1px; overflow: hidden; clip: rect(0 0 0 0); white-space: nowrap; }
  /* On phones each row folds to the question over its status and facts. */
  @media (max-width: 760px) {
    .jobs, .jobs tbody, .jobs tr, .jobs td { display: block; }
    .jobs thead { display: none; }
    .jobs tr { display: grid; grid-template-columns: minmax(0, 1fr) auto; gap: 4px 12px; padding: 12px 0; border-bottom: 1px solid var(--line); }
    .jobs td { padding: 0; border: 0; }
    .question { grid-column: 1 / -1; min-width: 0; }
    .status-cell { grid-column: 1; }
    .took { grid-column: 2; grid-row: 2; text-align: right; }
    .project, .method, .when { grid-column: 1 / -1; font-size: var(--font-xs); }
  }
</style>
