<script lang="ts">
  import { onDestroy, onMount, tick, untrack } from "svelte";
  import { broker, watchProject } from "./api.ts";
  import type { Identity, Lineage, TaskEvent } from "./contracts.ts";
  import { agentNotes, buildHistory } from "./history.ts";
  import HistoryTimeline from "./HistoryTimeline.svelte";
  import LiveLog from "./LiveLog.svelte";
  import { accessSummary, activeTask, eventLabel, runSentence, when } from "./language.ts";
  import { nowLine } from "./live_words.ts";
  import { followProject, viaChanges, type Follower } from "./follow.ts";
  import { covers, live } from "./live.ts";
  import { openWork, pageTitle, projectIndex, standingFor } from "./project_index.svelte.ts";
  import { projects as projectApi, type ProjectSummary } from "./projects.ts";
  import { standingNames, standingTone } from "./standing.ts";
  import StatusMark from "./StatusMark.svelte";
  import { href } from "./route.ts";
  import WbTree from "./WbTree.svelte";
  import WbView from "./WbView.svelte";
  import { attentionFor, detailsFor, flatten, ids, labTree, openable, pathTo, pickerLabs, projectChildren, recentProjects, search, taskOf, traceFor, type ProjectModel, type WbNode } from "./workbench.ts";
  import { currentLab as chosenLab, lab } from "./lab.svelte.ts";
  import { busy, loadProjectModel } from "./workbench_load.ts";

  // The Detailed view (Glass Box): every record a project holds, laid out like a code editor in this product's words.
  // An activity bar switches the side pane between the explorer, search and what needs attention, and opens the
  // timeline and the evidence package; the editor opens on the project's Overview; the inspector at the right shows the
  // open record's details and its thread back to the question; the panel below holds the timeline, a step's output and
  // the service activity. The broker decides what the reader may see; this view only arranges it.
  let { identity, projectId = "", notify, openClassic }: {
    identity: Identity; projectId?: string; notify: (text: string, error?: boolean) => void; openClassic: (hash: string) => void;
  } = $props();

  type Pane = "explorer" | "editor" | "inspector" | "panel";
  type PanelTab = "timeline" | "output" | "activity";
  type SideView = "explorer" | "search" | "attention";
  let models = $state<Record<string, ProjectModel>>({});
  let failures = $state<Record<string, string>>({});
  let loading = $state<Set<string>>(new Set());
  let expanded = $state<Set<string>>(new Set());
  let tabs = $state<{ id: string; pinned: boolean }[]>([]);
  let activeId = $state("");
  let panel = $state<PanelTab>("timeline");
  // The timeline, output and activity start as a slim bar and open on request.
  let panelOpen = $state(false);
  let sideView = $state<SideView>("explorer");
  let sideOpen = $state(true);
  // The inspector starts open on a wide window and closed on a narrower one; the reader's choice is kept.
  let inspectorOpen = $state(storedFlag("wb_inspector", innerWidth >= 1280));
  // The explorer shows one project at a time, chosen from the picker at its top; the filter narrows its rows.
  let filter = $state("");
  let sideQuery = $state("");
  let pickerButton = $state<HTMLButtonElement>();
  let pickerPanel = $state<HTMLElement>();
  let paletteOpen = $state(false);
  let query = $state("");
  let pick = $state(0);
  let hoverId = $state("");
  let pane = $state<Pane>("explorer");
  let sideWidth = $state(stored("wb_side", 300));
  let panelHeight = $state(stored("wb_panel", 230));
  let activity = $state<{ taskId: string; status: string; events: TaskEvent[] } | null>(null);
  let treeElement = $state<HTMLElement | null>(null);
  let tabStrip = $state<HTMLElement | null>(null);
  let paletteInput = $state<HTMLInputElement | null>(null);
  let sideSearch = $state<HTMLInputElement | null>(null);
  let panelBody = $state<HTMLElement | null>(null);
  // The reader's last look at each project as it stood when this page first read it: New marks hold for the visit.
  let sinces = $state<Record<string, string | null>>({});
  // Records that appeared while the page was open, marked for a moment so the eye finds them, and announced politely.
  let arrived = $state<Set<string>>(new Set());
  let announcement = $state("");
  const known = new Map<string, Set<string>>();
  const controllers = new Map<string, AbortController>();
  let timer: ReturnType<typeof setInterval> | undefined;
  // Once read, the open project follows the project stream (follow.ts); other projects open in the explorer keep the
  // timer below.
  let follower: Follower | null = null;
  let followed = "";
  // Changes to the open project read it again (live.ts), as on the project page.
  let unwatch: (() => void) | null = null;
  function unfollow() { follower?.stop(); follower = null; unwatch?.(); unwatch = null; followed = ""; openWork.busy = false; }
  function follow(id: string, model: ProjectModel) {
    if (followed !== id) {
      unfollow();
      followed = id;
      unwatch = live().watch([`project:${id}`], () => loadModel(id), { gapMs: 2_000 });
      follower = followProject({ open: (on) => (live().link === "live" ? viaChanges(live(), id, on) : watchProject(id, on)),
        read: () => loadModel(id), hidden: () => document.hidden,
        busy: (next) => { openWork.busy = next; },
        changed: () => { if (live().link !== "live") live().changed(["inbox", "projects"], "stream"); } });
    }
    follower?.loaded(busy(model));
  }
  let typed = "", typedAt = 0;

  function stored(key: string, fallback: number) {
    try { const value = Number(localStorage.getItem(key)); return Number.isFinite(value) && value > 0 ? value : fallback; } catch { return fallback; }
  }
  function storedFlag(key: string, fallback: boolean) {
    try { const value = localStorage.getItem(key); return value === null ? fallback : value === "1"; } catch { return fallback; }
  }
  function keep(key: string, value: number | boolean) {
    try { localStorage.setItem(key, typeof value === "boolean" ? (value ? "1" : "0") : String(Math.round(value))); } catch { /* a convenience only */ }
  }

  // The reader's projects, plus any project opened by link that the index does not list, such as an inspection.
  const listed = $derived.by((): ProjectSummary[] => {
    const seen = new Set(projectIndex.list.map((p) => p.project_id));
    return [...projectIndex.list, ...Object.values(models).map((m) => m.project).filter((p) => !seen.has(p.project_id))];
  });
  const tree = $derived(labTree(listed, identity.groups).map((lab) => ({
    ...lab,
    children: lab.children?.map((p) => (models[p.projectId!] ? { ...p, children: projectChildren(models[p.projectId!], identity.profiles, sinces[p.projectId!] ?? null) } : p)),
  })));
  const nodes = $derived(flatten(tree));
  const byId = $derived(new Map(nodes.map((n) => [n.id, n])));
  const projectOf = (id: string) => /^p:([0-9a-f-]{36})/.exec(id)?.[1] ?? "";
  const active = $derived(byId.get(activeId));
  const activeProject = $derived(activeId ? projectOf(activeId) : "");
  const activeModel = $derived(activeProject ? models[activeProject] : undefined);
  // Who asked for the selected analysis, as the broker recorded it. Read once per analysis;
  // until it arrives, or if it cannot be read, the thread says the loaded record does not show it.
  let origins = $state<Record<string, Lineage["origin"]>>({});
  const asked = new Set<string>();
  $effect(() => {
    const task = active?.taskId;
    if (!task || asked.has(task)) return;
    asked.add(task);
    void broker.lineage(task).then((l) => { origins = { ...origins, [task]: l.origin }; }).catch(() => { asked.delete(task); });
  });
  const trace = $derived(active && activeModel ? traceFor(activeModel, active, identity.profiles, origins, identity.principal.id) : []);
  const details = $derived(active && activeModel ? detailsFor(activeModel, active, identity.profiles, identity.principal.id) : []);
  // The project the explorer shows: the open record's, or the one the address names.
  const current = $derived(activeProject || projectId);
  const currentNode = $derived(current ? byId.get(ids.project(current)) : undefined);
  const currentSummary = $derived(listed.find((p) => p.project_id === current));
  const currentModel = $derived(current ? models[current] : undefined);
  const currentLab = $derived(currentSummary ? identity.groups.find((g) => g.key === currentSummary.group_key)?.name ?? "" : "");
  const sections = $derived.by(() => {
    const kids = currentNode?.children ?? [];
    return [
      { key: "question", label: "The question and the answer", heading: false, nodes: kids.filter((n) => n.group === "question") },
      { key: "steps", label: "Steps", heading: true, nodes: kids.filter((n) => n.group === "steps") },
      { key: "also", label: "Also in this project", heading: true, nodes: kids.filter((n) => n.group === "also") },
      { key: "lab", label: "Lab data", heading: true, nodes: kids.filter((n) => n.group === "lab") },
      { key: "record", label: "The record", heading: true, nodes: kids.filter((n) => n.group === "record") },
    ].filter((g) => g.nodes.length);
  });
  const inProject = (n: WbNode) => n.kind !== "project" && n.kind !== "lab" && projectOf(n.id) === current;
  const filtered = $derived(filter.trim() ? search(nodes.filter(inProject), filter) : []);
  const found = $derived(sideQuery.trim() ? search(nodes.filter(inProject), sideQuery, 80) : []);
  // The lab chosen at the top, as on every page: the start page, the picker's order and the project count follow it.
  const here = $derived(lab.key ? chosenLab(identity) : null);
  const hereName = $derived(here?.name ?? "this lab");
  // The start page's projects: the chosen lab's most recently changed, each with where it stands.
  const recent = $derived(recentProjects(listed, here?.key ?? ""));
  const inLab = $derived(listed.filter((p) => p.group_key === here?.key).length);
  // The chosen lab's projects first, then the reader's other research labs, for the picker.
  const labsForPicker = $derived(pickerLabs(identity.groups, listed, here?.key ?? ""));
  // Choosing another lab at the top closes the records left open, so the page starts on that lab's projects; an open
  // still loading from before the change then leaves the tabs alone.
  let shownLab = "";
  let openEpoch = 0;
  $effect(() => {
    const key = here?.key ?? "";
    untrack(() => { if (shownLab && key !== shownLab) { openEpoch += 1; tabs = []; activeId = ""; filter = ""; sideQuery = ""; } shownLab = key; });
  });
  function placePicker(event: ToggleEvent) {
    if (event.newState !== "open" || !pickerButton || !pickerPanel) return;
    const r = pickerButton.getBoundingClientRect();
    pickerPanel.style.top = `${Math.round(r.bottom + 6)}px`;
    // Never taller than the room below the picker, however short the window.
    pickerPanel.style.maxHeight = `${Math.floor(Math.max(0, Math.min(560, innerHeight - r.bottom - 22)))}px`;
    pickerPanel.style.left = `${Math.round(Math.max(8, r.left))}px`;
    pickerPanel.style.width = `${Math.round(Math.max(280, Math.min(380, r.width)))}px`;
  }
  async function pickProject(id: string) {
    pickerPanel?.hidePopover();
    filter = "";
    await open(ids.question(id), true);
  }
  // A project opened from the start page replaces the button that opened it, so focus moves to its open tab.
  async function openRecent(id: string) {
    await pickProject(id);
    await tick();
    tabStrip?.querySelector<HTMLElement>('[role="tab"][aria-selected="true"]')?.focus();
  }
  const attention = $derived(currentModel ? attentionFor(currentModel, identity.profiles) : []);
  const attentionCount = $derived(attention.reduce((n, g) => n + g.items.length, 0));
  const results = $derived(search(nodes, query));
  // Search results name the step or project they belong to, since two analyses can hold files of the same name.
  const places = $derived(new Map(nodes.filter((n) => n.kind === "analysis" || n.kind === "project").map((n) => [n.kind === "analysis" ? n.taskId! : n.projectId!, n.label])));
  const placeOf = (node: WbNode) => node.kind === "project" ? "" : node.taskId && node.kind !== "analysis" ? places.get(node.taskId) ?? "" : node.projectId ? places.get(node.projectId) ?? "" : "";
  const kindWords = (node: WbNode) => node.kind === "file" ? "File" : node.kind === "turn" ? "Agent decision" : node.kind === "analysis" ? (node.step ? `Step ${node.step}` : "Analysis")
    : node.kind === "planned" ? `Step ${node.step}, planned` : node.kind === "run" ? "Work session" : node.kind === "project" ? "Project" : node.kind === "code" ? "Analysis code"
    : node.kind === "steps" ? "Recorded steps" : node.kind === "question" ? "Overview" : "Record";
  // The row that takes Tab: the open record when its row is showing, otherwise the first row.
  const showing = (id: string) => (pathTo(tree, id) ?? []).every((p) => expanded.has(p) || p.startsWith("lab:") || /^p:[0-9a-f-]{36}$/.test(p));
  // A tree's Tab stop is the open record when it sits in that tree (by its recorded ancestry) and is showing.
  const focusableIn = (list: WbNode[]) => {
    if (active && active.kind !== "project" && showing(active.id)) {
      const path = pathTo(tree, active.id) ?? [];
      if (list.some((n) => n.id === active.id || path.includes(n.id))) return active.id;
    }
    return list[0]?.id ?? "";
  };
  const groupName = $derived(activeModel ? identity.groups.find((g) => g.key === activeModel.project.group_key)?.name ?? "" : "");
  const run = $derived(activeModel?.runs[0]?.run);
  const working = $derived(!!activeModel && busy(activeModel));
  // What the system does now and for how long, in the project page's words; the clock moves with the refresh timer.
  let clock = $state(Date.now());
  const doing = $derived(working && activeModel ? nowLine(run ? { status: run.status, code: run.code, updated_at: run.updated_at } : undefined,
    activeModel.project.tasks.filter((t) => activeTask(t.status)).map((t) => ({ status: t.status, submitted_at: t.submitted_at, queue: t.queue })), clock) : null);
  const fileState = $derived.by(() => {
    if (!active || active.kind !== "file" || !activeModel || !active.taskId) return "";
    const pkg = activeModel.evidence[active.taskId]?.packages.find((p) => p.artifact.id === active.artifactId);
    const member = pkg?.contents.listing.members.find((m) => m.path === active.memberPath);
    if (!member) return "Checked against its recorded checksum when read";
    return member.manifest === "matched" ? "Matches the package checksum list" : member.manifest === "mismatched" ? "Does not match the package checksum list" : "Checked when read";
  });
  // Where the open record sits: the project, its step, its folder, then the record.
  const crumbs = $derived.by(() => {
    if (!active) return [];
    const up = (pathTo(tree, active.id) ?? []).filter((id) => !id.startsWith("lab:")).map((id) => byId.get(id)).filter((n): n is WbNode => !!n);
    const label = (n: WbNode) => n.kind === "project" ? n.label : n.step ? `Step ${n.step}` : n.label;
    return [...up.map((n) => ({ id: n.id, label: label(n), node: n })),
      { id: active.id, label: active.kind === "question" ? "Overview" : active.kind === "file" ? active.memberPath ?? active.label : label(active), node: null }];
  });
  // The timeline: every saved version, decision, analysis and access change, newest first, each naming its source.
  const timeline = $derived(currentModel ? buildHistory({
    me: identity.principal.id, checkpoints: currentModel.checkpoints, runs: currentModel.runs, audit: currentModel.audit,
    tasks: currentModel.project.tasks.map((t) => ({ task_id: t.task_id, action_key: t.action_key, status: t.status, submitted_at: t.submitted_at, updated_at: t.updated_at, profile: t.profile })),
    stepTitle: (id) => currentModel.project.checkpoint?.state.plan.find((s) => s.task_id === id)?.title,
    personName: (id) => {
      if (!id) return "A person";
      if (id === identity.principal.id) return "you";
      const m = currentModel.members;
      const person = m ? [m.owner, ...m.grants, ...m.eligible].find((x) => x.principal_id === id) : undefined;
      return person ? (person.display_name?.trim() || person.email) : "A person";
    },
  }) : []);
  // A step's output: its saved logs once it ends. A running step's output is saved when it ends; it shows here live once
  // the broker streams it.
  const outputTask = $derived(active?.taskId && activeModel ? taskOf(activeModel, active.taskId) : undefined);
  // The step whose live log the Output panel shows. It stays shown after the step ends, so the reader keeps the text and
  // can read its check; opening another step lets it go.
  let watched = $state<string | null>(null);
  $effect(() => {
    const task = outputTask;
    if (task?.status === "running") watched = task.task_id;
    else if (task?.task_id !== watched) watched = null;
  });
  const outputLogs = $derived(outputTask && activeModel ? (activeModel.evidence[outputTask.task_id]?.packages ?? [])
    .flatMap((pkg) => pkg.view.logs.map((m) => ({ id: ids.file(activeModel.project.project_id, outputTask.task_id, pkg.artifact.id, m.path), path: m.path }))) : []);

  async function loadModel(id: string) {
    if (!id) return;
    controllers.get(id)?.abort();
    const c = new AbortController();
    controllers.set(id, c);
    const first = !models[id];
    if (first) loading = new Set([...loading, ids.project(id)]);
    try {
      const model = await loadProjectModel(id, models[id] ?? null, c.signal);
      if (c.signal.aborted) return;
      if (!(id in sinces)) {
        sinces = { ...sinces, [id]: model.project.last_look?.at ?? null };
        // Opening the project here is a look at it; a refusal (a service account) changes nothing on the page.
        void projectApi.look(id, model.project.revision).catch(() => undefined);
      }
      models = { ...models, [id]: model };
      noteArrivals(id, model);
      projectIndex.remember(model.project);
      if (id === activeProject) follow(id, model);
      if (failures[id]) { const { [id]: _cleared, ...rest } = failures; failures = rest; }
    } catch {
      if (!c.signal.aborted) failures = { ...failures, [id]: "This project could not be read. It may have been closed to your account." };
    } finally {
      if (controllers.get(id) === c) {
        controllers.delete(id);
        if (first) { const next = new Set(loading); next.delete(ids.project(id)); loading = next; }
      }
    }
  }
  // The explorer grows while the agent works: a record that arrives after the first read is marked for a moment and
  // named in a polite announcement.
  function noteArrivals(id: string, model: ProjectModel) {
    const list = flatten(projectChildren(model, identity.profiles, sinces[id] ?? null));
    const before = known.get(id);
    known.set(id, new Set(list.map((n) => n.id)));
    if (!before) return;
    const added = list.filter((n) => !before.has(n.id) && n.kind !== "folder");
    if (!added.length) return;
    arrived = new Set([...arrived, ...added.map((n) => n.id)]);
    announcement = `New in the explorer: ${added.slice(0, 3).map((n) => n.label).join(", ")}${added.length > 3 ? ` and ${added.length - 3} more` : ""}.`;
    setTimeout(() => { const next = new Set(arrived); for (const n of added) next.delete(n.id); arrived = next; }, 2400);
  }

  function toggle(node: WbNode) {
    const next = new Set(expanded);
    if (next.has(node.id)) next.delete(node.id);
    else {
      next.add(node.id);
      if (node.kind === "project" && node.projectId && !models[node.projectId] && !controllers.has(node.projectId)) void loadModel(node.projectId);
    }
    expanded = next;
  }

  async function open(id: string, pin = false) {
    const epoch = openEpoch;
    const p = projectOf(id);
    // A project opens on its Overview.
    if (p && id === ids.project(p)) id = ids.question(p);
    if (p && !models[p]) await loadModel(p);
    if (epoch !== openEpoch) return;
    // The letterhead follows the lab of the project being read, as on the project page. The view shows one lab at a
    // time, so the records left open from the other lab close, as they do when a lab is chosen at the top.
    const group = p ? models[p]?.project.group_key : undefined;
    if (group && group !== lab.key && identity.groups.some((g) => g.key === group)) {
      shownLab = group; lab.set(group);
      tabs = tabs.filter((t) => models[projectOf(t.id)]?.project.group_key === group);
    }
    const path = pathTo(tree, id);
    if (path) expanded = new Set([...expanded, ...path]);
    if (!byId.get(id)) return;
    // The Overview is kept open, first among the project's tabs.
    if (p && !tabs.some((t) => t.id === ids.question(p))) tabs = [...tabs, { id: ids.question(p), pinned: true }];
    const existing = tabs.find((t) => t.id === id);
    if (existing) { if (pin && !existing.pinned) tabs = tabs.map((t) => (t.id === id ? { ...t, pinned: true } : t)); }
    else {
      // One preview tab at a time, as in a code editor: a new preview replaces the old one until it is kept.
      const previewAt = tabs.findIndex((t) => !t.pinned);
      const tab = { id, pinned: pin };
      tabs = previewAt >= 0 ? tabs.map((t, i) => (i === previewAt ? tab : t)) : [...tabs, tab];
    }
    activeId = id;
    pane = "editor";
    await tick();
    treeElement?.querySelector<HTMLElement>(`[data-node="${CSS.escape(id)}"]`)?.scrollIntoView({ block: "nearest" });
  }

  function close(id: string) {
    const index = tabs.findIndex((t) => t.id === id);
    tabs = tabs.filter((t) => t.id !== id);
    if (activeId === id) activeId = tabs[Math.min(index, tabs.length - 1)]?.id ?? "";
  }

  // Shows a record's row in the explorer without opening it, as a breadcrumb for a folder does.
  async function reveal(id: string) {
    const path = pathTo(tree, id) ?? [];
    expanded = new Set([...expanded, ...path, id]);
    sideView = "explorer"; sideOpen = true; pane = "explorer";
    await tick();
    const row = treeElement?.querySelector<HTMLElement>(`[data-node="${CSS.escape(id)}"]`);
    row?.scrollIntoView({ block: "nearest" });
    row?.focus();
  }

  // Service activity for the open analysis, read when the panel shows it and again when the analysis changes state.
  $effect(() => {
    const taskId = active?.taskId, show = panel === "activity" && panelOpen;
    const status = (activeModel && taskId ? taskOf(activeModel, taskId)?.status : undefined) ?? "";
    if (!taskId || !show) return;
    const c = new AbortController();
    untrack(() => {
      if (activity?.taskId === taskId && activity.status === status) return;
      broker.events(taskId, 0, c.signal).then((page) => { activity = { taskId, status, events: page.events }; }).catch(() => {});
    });
    return () => c.abort();
  });

  // Keyboard movement through the visible rows of one explorer tree, as in a code editor's file tree. Each group is its
  // own tree with its own Tab stop; keys pressed elsewhere, such as on a filter result, are left alone.
  function treeKeys(event: KeyboardEvent) {
    const item = (event.target as HTMLElement | null)?.closest<HTMLElement>('[role="treeitem"]');
    const widget = item?.closest<HTMLElement>('[role="tree"]');
    if (!item || !widget || item !== event.target) return;
    const rows = [...widget.querySelectorAll<HTMLElement>('[role="treeitem"]')];
    const at = rows.findIndex((r) => r === document.activeElement);
    const node = at >= 0 ? byId.get(rows[at].dataset.node ?? "") : undefined;
    const focusAt = (i: number) => rows[Math.max(0, Math.min(rows.length - 1, i))]?.focus();
    const canExpand = node && (node.kind === "project" || !!node.children?.length);
    switch (event.key) {
      case "ArrowDown": focusAt(at + 1); break;
      case "ArrowUp": focusAt(at - 1); break;
      case "Home": focusAt(0); break;
      case "End": focusAt(rows.length - 1); break;
      case "ArrowRight":
        if (node && canExpand && !expanded.has(node.id)) toggle(node); else if (canExpand) focusAt(at + 1);
        break;
      case "ArrowLeft":
        if (node && canExpand && expanded.has(node.id)) toggle(node);
        else if (node) { const parent = pathTo(tree, node.id)?.at(-1); if (parent) treeElement?.querySelector<HTMLElement>(`[data-node="${CSS.escape(parent)}"]`)?.focus(); }
        break;
      case "Enter": case " ":
        if (node) void (openable(node) ? open(node.id, true) : toggle(node));
        break;
      default: {
        // Type the start of a name to move to it.
        if (event.key.length !== 1 || event.metaKey || event.ctrlKey || event.altKey || event.key === "/") return;
        const now = Date.now();
        typed = now - typedAt < 700 ? typed + event.key.toLowerCase() : event.key.toLowerCase();
        typedAt = now;
        const from = at + (typed.length === 1 ? 1 : 0);
        [...rows.slice(from), ...rows.slice(0, from)].find((r) => byId.get(r.dataset.node ?? "")?.label.toLowerCase().replace(/^\d+ · /, "").startsWith(typed))?.focus();
      }
    }
    event.preventDefault();
  }

  function choose(node: WbNode) { paletteOpen = false; query = ""; void open(node.id, true); }
  function paletteKeys(event: KeyboardEvent) {
    if (event.key === "ArrowDown") { pick = Math.min(results.length - 1, pick + 1); event.preventDefault(); }
    else if (event.key === "ArrowUp") { pick = Math.max(0, pick - 1); event.preventDefault(); }
    else if (event.key === "Enter" && results[pick]) choose(results[pick]);
    else if (event.key === "Escape") paletteOpen = false;
  }
  async function showPalette() { paletteOpen = true; pick = 0; await tick(); paletteInput?.focus(); }

  function globalKeys(event: KeyboardEvent) {
    const target = event.target as HTMLElement | null;
    const typing = /^(INPUT|TEXTAREA|SELECT)$/.test(target?.tagName ?? "") || !!target?.isContentEditable;
    if ((event.metaKey || event.ctrlKey) && event.key.toLowerCase() === "k") { event.preventDefault(); void showPalette(); }
    else if (!typing && event.key === "/" && !target?.closest('[role="tree"]')) { event.preventDefault(); void showPalette(); }
  }

  // The activity bar: a side view toggles the side pane when it is already showing; the timeline opens the panel.
  async function tool(key: SideView | "timeline" | "package") {
    if (key === "timeline") {
      const show = !(panelOpen && panel === "timeline");
      panel = "timeline"; panelOpen = show; if (show) pane = "panel";
      if (show) { await tick(); panelBody?.focus(); }
      return;
    }
    if (key === "package") { if (current) void open(ids.package(current), true); return; }
    if (sideView === key && sideOpen) { sideOpen = false; return; }
    sideView = key; sideOpen = true; pane = "explorer";
    if (key === "search") { await tick(); sideSearch?.focus(); }
  }
  function showInspector(on: boolean) { inspectorOpen = on; keep("wb_inspector", on); }

  // Pane edges move with the pointer or the arrow keys; sizes are kept in this browser only.
  const clampSide = (v: number) => Math.max(200, Math.min(560, v));
  const clampPanel = (v: number) => Math.max(120, Math.min(innerHeight * 0.7, v));
  function drag(axis: "x" | "y", event: PointerEvent) {
    const start = axis === "x" ? event.clientX : event.clientY;
    const from = axis === "x" ? sideWidth : panelHeight;
    const move = (e: PointerEvent) => {
      if (axis === "x") sideWidth = clampSide(from + e.clientX - start);
      else panelHeight = clampPanel(from - (e.clientY - start));
    };
    const up = () => { removeEventListener("pointermove", move); removeEventListener("pointerup", up); keep("wb_side", sideWidth); keep("wb_panel", panelHeight); };
    addEventListener("pointermove", move); addEventListener("pointerup", up);
  }
  function nudge(axis: "x" | "y", event: KeyboardEvent) {
    const step = event.shiftKey ? 40 : 12;
    if (axis === "x" && (event.key === "ArrowLeft" || event.key === "ArrowRight")) { sideWidth = clampSide(sideWidth + (event.key === "ArrowRight" ? step : -step)); keep("wb_side", sideWidth); event.preventDefault(); }
    if (axis === "y" && (event.key === "ArrowUp" || event.key === "ArrowDown")) { panelHeight = clampPanel(panelHeight + (event.key === "ArrowUp" ? step : -step)); keep("wb_panel", panelHeight); event.preventDefault(); }
  }
  function showPane(next: Pane) { pane = next; if (next === "panel") panelOpen = true; }

  onMount(() => {
    if (projectIndex.phase === "idle") void projectIndex.load();
    addEventListener("keydown", globalKeys);
    // Other projects open in the explorer read again when the change stream names them; without it, only projects with
    // work still running read again, only while the page is visible, and never over a read in flight.
    const others = live().watch(["project:*"], (changed) => {
      for (const id of Object.keys(models)) if (id !== followed && covers(changed, `project:${id}`)) void loadModel(id);
    }, { gapMs: 2_000 });
    timer = setInterval(() => {
      if (document.hidden) return;
      clock = Date.now();
      if (live().link === "live") return;
      for (const [id, m] of Object.entries(models)) if (id !== followed && busy(m) && !controllers.has(id)) void loadModel(id);
    }, 5000);
    return () => { removeEventListener("keydown", globalKeys); others(); };
  });
  $effect(() => { const id = projectId; if (id) untrack(() => void open(ids.question(id), true)); });
  // The address names the open project only, so a reload or a shared link returns to it.
  $effect(() => {
    if (!activeProject) return;
    const target = href({ name: "workbench", project: activeProject });
    if (location.hash !== target) history.replaceState(null, "", target);
  });
  // The open record's tab stays in view in a strip of many tabs, however the record was chosen.
  $effect(() => {
    const id = activeId;
    if (!id) return;
    void tick().then(() => tabStrip?.querySelector<HTMLElement>('[role="tab"][aria-selected="true"]')?.parentElement?.scrollIntoView({ block: "nearest", inline: "nearest" }));
  });
  $effect(() => { pageTitle.value = active ? (active.kind === "question" ? `Overview · ${currentSummary?.title ?? ""}` : active.label) : ""; });
  // Another open project changes what is followed; one already read is followed from what the page knows.
  $effect(() => {
    const id = activeProject;
    untrack(() => { if (id !== followed) { unfollow(); if (id && models[id]) follow(id, models[id]); } });
  });
  $effect(() => {
    const shown = () => { if (!document.hidden) follower?.shown(); };
    document.addEventListener("visibilitychange", shown);
    return () => document.removeEventListener("visibilitychange", shown);
  });
  onDestroy(() => { clearInterval(timer); for (const c of controllers.values()) c.abort(); pageTitle.value = ""; unfollow(); });
  // A hop of the thread that arrives while the record is open fades in from just above its place; reduced motion places it.
  function arrive(_node: Element) {
    const still = matchMedia("(prefers-reduced-motion: reduce)").matches;
    return { duration: still ? 0 : 240, easing: (t: number) => 1 - (1 - t) ** 3, css: (t: number) => `opacity: ${t}; transform: translateY(${(1 - t) * -4}px)` };
  }
  const TOOLS: { key: SideView | "timeline" | "package"; label: string; icon: string }[] = [
    { key: "explorer", label: "Explorer", icon: "M2.8 5.6c0-.8.6-1.4 1.4-1.4h3.4l1.6 1.8h6.6c.8 0 1.4.6 1.4 1.4v7.4c0 .8-.6 1.4-1.4 1.4H4.2c-.8 0-1.4-.6-1.4-1.4Z" },
    { key: "search", label: "Search", icon: "M8.8 14.6a5.8 5.8 0 1 0 0-11.6 5.8 5.8 0 0 0 0 11.6ZM13 13l4.2 4.2" },
    { key: "timeline", label: "Timeline", icon: "M10 17.2a7.2 7.2 0 1 0 0-14.4 7.2 7.2 0 0 0 0 14.4ZM10 6.2V10l2.8 1.8" },
    { key: "attention", label: "Needs attention", icon: "M4.4 17.4V3.2M4.4 3.8h9.8l-2.2 3.6 2.2 3.6H4.4" },
    { key: "package", label: "Evidence package", icon: "M3.2 6.4 10 3l6.8 3.4v7.2L10 17l-6.8-3.4ZM3.2 6.4 10 9.8l6.8-3.4M10 9.8V17" },
  ];
</script>

<div class="bench" class:no-side={!sideOpen} class:no-insp={!inspectorOpen || !activeModel} style={`--side:${sideWidth}px; --panel:${panelHeight}px`} data-pane={pane}>
  <nav class="act" aria-label="Detailed view tools">
    {#each TOOLS as t (t.key)}
      {@const on = t.key === "timeline" ? panelOpen && panel === "timeline" : t.key === "package" ? active?.kind === "package" : sideOpen && sideView === t.key}
      <button class:on aria-label={t.key === "attention" && attentionCount ? `${t.label}, ${attentionCount}` : t.label}
        aria-pressed={t.key === "package" ? undefined : on} disabled={t.key === "package" && !current} onclick={() => void tool(t.key)}>
        <svg viewBox="0 0 20 20" aria-hidden="true"><path d={t.icon} /></svg>
        {#if t.key === "attention" && attentionCount}<span class="act-count" aria-hidden="true">{attentionCount}</span>{/if}
        <span class="tip" aria-hidden="true">{t.label}</span>
      </button>
    {/each}
  </nav>

  <aside class="side" aria-label={sideView === "explorer" ? "Explorer" : sideView === "search" ? "Search" : "Needs attention"}>
    <div class="side-tabs" role="group" aria-label="Side pane">
      {#each [["explorer", "Explorer"], ["search", "Search"], ["attention", "Needs attention"]] as [key, label] (key)}
        <button aria-pressed={sideView === key} aria-label={key === "attention" && attentionCount ? `${label}, ${attentionCount}` : undefined}
          onclick={() => (sideView = key as SideView)}>{label}{#if key === "attention" && attentionCount}<span class="n" aria-hidden="true">{attentionCount}</span>{/if}</button>
      {/each}
    </div>
    <div class="side-head">
      <button class="picker" bind:this={pickerButton} popovertarget="wb-picker" aria-label={currentSummary ? `Project: ${currentSummary.title}. Choose another project` : "Choose a project"}>
        <span class="p-lab">{currentLab || "Detailed view"}</span>
        <span class="p-title">{currentSummary?.title ?? "Choose a project"}</span>
        <svg viewBox="0 0 12 12" aria-hidden="true"><path d="M3 4.5l3 3 3-3" /></svg>
      </button>
      {#if sideView === "explorer"}
        <label class="search-field side-filter"><span class="visually-hidden">Filter this project</span><svg viewBox="0 0 16 16" aria-hidden="true"><circle cx="7" cy="7" r="4.6" /><path d="m10.4 10.4 3.4 3.4" /></svg>
          <input type="search" placeholder="Filter this project" bind:value={filter} disabled={!current} /></label>
      {:else if sideView === "search"}
        <label class="search-field side-filter"><span class="visually-hidden">Search this project</span><svg viewBox="0 0 16 16" aria-hidden="true"><circle cx="7" cy="7" r="4.6" /><path d="m10.4 10.4 3.4 3.4" /></svg>
          <input type="search" bind:this={sideSearch} placeholder="Search files, steps and decisions" bind:value={sideQuery} disabled={!current} /></label>
      {/if}
    </div>
    <div id="wb-picker" class="panel-popover picker-panel" popover bind:this={pickerPanel} onbeforetoggle={placePicker} ontoggle={placePicker} role="dialog" aria-label="Choose a project">
      {#each labsForPicker as group (group.key)}
        <p class="pk-lab">{group.name}</p>
        <ul class="pk-list">
          {#each group.projects as pr (pr.project_id)}
            <li><button aria-current={pr.project_id === current ? "true" : undefined} onclick={() => void pickProject(pr.project_id)}>
              <span class="t">{pr.title}</span><span class="s">{pr.visibility === "lab" ? "Shared with the lab" : "Private"}</span></button></li>
          {/each}
        </ul>
      {:else}<p class="note">No projects yet. Start one from Your research.</p>{/each}
    </div>
    <!-- svelte-ignore a11y_no_static_element_interactions -->
    <div class="tree" bind:this={treeElement} onkeydown={treeKeys}>
      {#if projectIndex.phase === "loading" && !listed.length}<p class="note"><span class="waveform" aria-hidden="true"><i></i><i></i><i></i></span> Opening your projects…</p>
      {:else if projectIndex.phase === "failed" && !listed.length}<p class="note">{projectIndex.failure}</p>
      {:else if !listed.length}<p class="note">No projects yet. Start one from Your research.</p>
      {:else if !current}<p class="note">Choose a project above to see its question, its steps and its record.</p>
      {:else if loading.has(ids.project(current))}<p class="note"><span class="waveform" aria-hidden="true"><i></i><i></i><i></i></span> Reading the project…</p>
      {:else if sideView === "search"}
        {#if sideQuery.trim()}
          <ul class="matches" aria-label="Search results">
            {#each found as node (node.id)}
              <li><button class:current={node.id === activeId} onclick={() => void open(node.id, true)}>
                <span class="k">{kindWords(node)}</span><span class="t">{node.label}</span>{#if placeOf(node)}<span class="s">{placeOf(node)}</span>{/if}</button></li>
            {:else}<li class="note">Nothing in this project by that name.</li>{/each}
          </ul>
        {:else}<p class="note">Type part of a name: a file, a step, a table or a decision.</p>{/if}
      {:else if sideView === "attention"}
        {#if attention.length}
          {#each attention as group (group.kind)}
            <h3 class="side-group">{group.title} <span class="n">{group.items.length}</span></h3>
            <ul class="problems" aria-label={group.title}>{#each group.items as problem, i (i)}<li class={problem.tone}>{#if problem.target}<button class="link" onclick={() => void open(problem.target!, true)}>{problem.text}</button>{:else}{problem.text}{/if}</li>{/each}</ul>
          {/each}
        {:else}<p class="note">Nothing in this project needs attention. A normal wait is not listed here.</p>{/if}
      {:else if filter.trim()}
        <ul class="matches" aria-label="Records that match">
          {#each filtered as node (node.id)}
            <li><button class:current={node.id === activeId} onclick={() => { filter = ""; void open(node.id, true); }}>
              <span class="t">{node.label}</span>{#if placeOf(node)}<span class="s">{placeOf(node)}</span>{/if}</button></li>
          {:else}<li class="note">No record in this project by that name.</li>{/each}
        </ul>
      {:else}
        {#each sections as section (section.key)}
          {#if section.heading}<h3 class="side-group">{section.label}</h3>{/if}
          <WbTree nodes={section.nodes} label={section.label} {expanded} active={activeId} focusable={focusableIn(section.nodes)} highlighted={hoverId} {loading} {arrived} onToggle={toggle} onOpen={(n) => open(n.id)} onPin={(n) => open(n.id, true)} />
        {/each}
      {/if}
      {#each Object.entries(failures) as [id, text] (id)}<p class="note error">{text}</p>{/each}
    </div>
  </aside>
  <!-- A focusable separator is the ARIA window splitter; svelte-check does not count it as interactive. -->
  <!-- svelte-ignore a11y_no_noninteractive_tabindex, a11y_no_noninteractive_element_interactions -->
  <div class="edge x" role="separator" aria-orientation="vertical" aria-label="Resize the side pane" tabindex="0"
    aria-valuenow={Math.round(sideWidth)} aria-valuemin={200} aria-valuemax={560}
    onpointerdown={(e) => drag("x", e)} onkeydown={(e) => nudge("x", e)}></div>

  <section class="editor" aria-label="Open records">
    {#if tabs.length}
      <div class="tabs-row">
        <div class="tabs" role="tablist" aria-label="Open records" bind:this={tabStrip}>
          {#each tabs as tab (tab.id)}
            {@const node = byId.get(tab.id)}
            {@const overview = node?.kind === "question"}
            {@const name = overview ? "Overview" : node?.label ?? "Closed record"}
            <div class="tab" class:active={tab.id === activeId} class:preview={!tab.pinned}>
              <button role="tab" aria-selected={tab.id === activeId} onclick={() => (activeId = tab.id)} ondblclick={() => void open(tab.id, true)}
                title={overview ? `Overview of ${byId.get(ids.project(projectOf(tab.id)))?.label ?? "the project"}` : tab.pinned ? node?.label : `${node?.label ?? ""}. Preview: the next record you open replaces it. Double-click to keep it open.`}>{name}</button>
              {#if !overview}<button class="x" aria-label={`Close ${node?.label ?? "record"}`} onclick={() => close(tab.id)}>×</button>{/if}
            </div>
          {/each}
        </div>
        {#if activeModel}<button class="insp-toggle" aria-pressed={inspectorOpen} onclick={() => showInspector(!inspectorOpen)}>{inspectorOpen ? "Hide the thread" : "Show the thread"}</button>{/if}
      </div>
      {#if crumbs.length}
        <nav class="crumbs" aria-label="Where this record sits">
          <ol>{#each crumbs as crumb, i (crumb.id)}<li>{#if crumb.node}<button class="crumb" onclick={() => (crumb.node!.kind === "folder" ? void reveal(crumb.id) : void open(crumb.id, true))}>{crumb.label}</button>{:else}<span aria-current="page">{crumb.label}</span>{/if}{#if i < crumbs.length - 1}<svg viewBox="0 0 10 10" aria-hidden="true"><path d="M3.5 2l3 3-3 3" /></svg>{/if}</li>{/each}</ol>
        </nav>
      {/if}
    {/if}
    <div class="content" role={tabs.length ? "tabpanel" : undefined}>
      {#if active && activeModel}
        {#key active.id}<WbView node={active} model={activeModel} {identity} open={(id) => void open(id, true)} preview={(id) => void open(id)} {openClassic} {notify}
          since={sinces[activeModel.project.project_id] ?? null} reload={() => void loadModel(activeModel!.project.project_id)} />{/key}
      {:else if activeProject && failures[activeProject]}
        <p class="note error">{failures[activeProject]}</p>
      {:else if activeId}
        <p class="note" role="status"><span class="waveform" aria-hidden="true"><i></i><i></i><i></i></span> Reading the project…</p>
      {:else}
        <div class="welcome">
          <h1>Detailed view</h1>
          <p class="lede">Every record of a project, laid out like an editor: the question and the answer, each step with its figures, tables, code and data, and the record of who did what. The thread at the right follows any result back to the question.</p>
          {#if recent.length}
            <section class="recent" aria-labelledby="wb-recent">
              <h2 id="wb-recent">Recent projects in {hereName}</h2>
              <ul>
                {#each recent as project (project.project_id)}
                  {@const standing = standingFor(project)}
                  <li><button class="recent-row" onclick={() => void openRecent(project.project_id)}>
                    <span class="r-title">{project.title}</span>
                    <span class="r-state"><StatusMark tone={standingTone[standing]} label={standingNames[standing]} /></span>
                    <span class="r-where">{project.visibility === "lab" ? "Shared with the lab" : "Private"}</span>
                    <time class="r-when" datetime={project.updated_at}>{when(project.updated_at)}</time>
                  </button></li>
                {/each}
              </ul>
            </section>
          {:else if projectIndex.phase === "ready" && here}
            <p class="lede">No project in {hereName} yet. Start one from Your research and its records appear here.</p>
          {/if}
          <section class="legend" aria-labelledby="wb-keys">
            <h2 id="wb-keys">Keys</h2>
            <dl class="keys">
              <dt><kbd>Ctrl</kbd> <kbd>K</kbd> or <kbd>⌘</kbd> <kbd>K</kbd></dt><dd>Find a record by name in the projects you have opened</dd>
              <dt><kbd>↑</kbd> <kbd>↓</kbd> <kbd>Enter</kbd></dt><dd>Move through the explorer and open a record; typing a name jumps to it</dd>
              <dt>Click, double-click</dt><dd>Open a record in a preview tab; keep it open</dd>
              <dt>The thread</dt><dd>Follow any result back through its code, data, step and the agent's decision to the question</dd>
            </dl>
          </section>
        </div>
      {/if}
    </div>
  </section>

  {#if activeModel && active}
    <aside class="inspector" aria-label="Inspector">
      <div class="insp-head"><p class="insp-kind">{kindWords(active)}</p><h2 class="insp-title">{active.kind === "question" ? activeModel.project.title : active.label}</h2></div>
      {#if details.length}
        <section aria-labelledby="insp-details"><h3 id="insp-details">Details</h3>
          <dl class="facts">{#each details as [label, value] (label)}<div><dt>{label}</dt><dd>{value}</dd></div>{/each}</dl></section>
      {/if}
      <section aria-labelledby="insp-thread"><h3 id="insp-thread">The thread</h3>
        {#if trace.length}
          <ol class="thread" aria-label="How this record connects to the question">
            {#each trace as hop, i (`${hop.role}:${hop.label}`)}
              <li class:current={hop.target === activeId || (i === 0 && !hop.target)} class:failed={hop.tone === "failed"} in:arrive>
                <span class="dot" aria-hidden="true"></span>
                <span class="hop-body"><span class="role">{hop.role}</span>
                  {#if hop.target && hop.target !== activeId}
                    <button class="hop" onclick={() => void open(hop.target!, true)} onpointerenter={() => (hoverId = hop.target!)} onpointerleave={() => (hoverId = "")}
                      onfocus={() => (hoverId = hop.target!)} onblur={() => (hoverId = "")}>{hop.label}</button>
                  {:else}<span class="hop">{hop.label}</span>{/if}
                  {#if hop.detail}<span class="detail">{hop.detail}</span>{/if}</span>
              </li>
            {/each}
          </ol>
        {:else}<p class="note">Open a result, a step or a decision to follow it back to the question.</p>{/if}
      </section>
    </aside>
  {/if}

  <!-- svelte-ignore a11y_no_noninteractive_tabindex, a11y_no_noninteractive_element_interactions -->
  {#if panelOpen}<div class="edge y" role="separator" aria-orientation="horizontal" aria-label="Resize the panel" tabindex="0"
    aria-valuenow={Math.round(panelHeight)} aria-valuemin={120} aria-valuemax={800}
    onpointerdown={(e) => drag("y", e)} onkeydown={(e) => nudge("y", e)}></div>{/if}
  <section class="panel" aria-label="Panel">
    <div class="panel-tabs">
      <div class="panel-tablist" role="tablist" aria-label="Panel">
        {#each [["timeline", "Timeline"], ["output", "Output"], ["activity", "Activity"]] as [key, label] (key)}
          <button role="tab" aria-selected={panelOpen && panel === key} onclick={() => { if (panelOpen && panel === key) panelOpen = false; else { panel = key as PanelTab; panelOpen = true; } }}>{label}</button>
        {/each}
      </div>
      <button class="fold" onclick={() => (panelOpen = !panelOpen)} aria-expanded={panelOpen}>{panelOpen ? "Hide the panel" : "Show the panel"}</button>
    </div>
    {#if panelOpen}
      <!-- svelte-ignore a11y_no_noninteractive_tabindex -->
      <div class="panel-body" role="tabpanel" tabindex="-1" bind:this={panelBody}>
        {#if panel === "timeline"}
          {#if currentModel}<HistoryTimeline entries={timeline} notes={agentNotes(currentModel.checkpoints)} openTask={(id) => void open(ids.analysis(currentModel!.project.project_id, id), true)} />
          {:else}<p class="note">Open a project to see everything that happened in it, newest first.</p>{/if}
        {:else if panel === "output"}
          {#if outputTask}
            {#if outputTask.status === "running" || watched === outputTask.task_id}
              {#key outputTask.task_id}
                <LiveLog task={outputTask.task_id} running={outputTask.status === "running"} fill
                  system={activity?.taskId === outputTask.task_id ? activity.events.map((e) => ({ at: e.occurred_at, text: eventLabel(e.code) })) : []} />
              {/key}
              {#if outputTask.status !== "running" && outputLogs.length}
                <ul class="logs">{#each outputLogs as log (log.id)}<li><button class="link" onclick={() => void open(log.id, true)}>{log.path}</button></li>{/each}</ul>
              {/if}
            {:else if activeTask(outputTask.status)}
              <p class="note">This step is waiting for a compute slot. Its output shows here as it is written once it runs.</p>
            {:else if outputLogs.length}
              <ul class="logs">{#each outputLogs as log (log.id)}<li><button class="link" onclick={() => void open(log.id, true)}>{log.path}</button></li>{/each}</ul>
              <p class="note">The saved output of this step. Each file matches the checksum recorded when the step ended.</p>
            {:else}<p class="note">No output was saved for this step.</p>{/if}
          {:else}<p class="note">Open a step or one of its files to see its output.</p>{/if}
        {:else if active?.taskId && activity?.taskId === active.taskId}
          <ol class="activity">{#each activity.events as event (event.sequence)}<li><time datetime={event.occurred_at}>{when(event.occurred_at)}</time><span>{eventLabel(event.code)}</span></li>{/each}</ol>
          <p class="note">Service activity shows how the work moved through the cluster. It is not scientific evidence.</p>
        {:else if active?.taskId}<p class="note"><span class="waveform" aria-hidden="true"><i></i><i></i><i></i></span> Reading the activity…</p>
        {:else}<p class="note">Open a step or one of its files to see its service activity.</p>{/if}
      </div>
    {/if}
  </section>

  <nav class="panes" aria-label="Detailed view panes">
    {#each [["explorer", "Explorer"], ["editor", "Open record"], ["inspector", "Thread"], ["panel", "Timeline"]] as [key, label] (key)}
      <button aria-pressed={pane === key} onclick={() => showPane(key as Pane)}>{label}</button>
    {/each}
  </nav>

  <footer class="status" aria-label="Status">
    {#if activeModel}
      {#if groupName}<span>{groupName}</span>{/if}
      <span>{activeModel.project.visibility === "lab" ? "Shared with the lab" : "Private"}</span>
      <span>{activeModel.project.inspection_expires_at ? `Inspection until ${when(activeModel.project.inspection_expires_at)}` : accessSummary(activeModel.project.capabilities)}</span>
      <!-- While work runs, what the system does now and for how long says more than the session's sentence. -->
      {#if doing}<span class="run"><span class="waveform" aria-hidden="true"><i></i><i></i><i></i></span>{doing.text} · {doing.time}</span>
      {:else if run}<span class="run">{#if working}<span class="waveform" aria-hidden="true"><i></i><i></i><i></i></span>{/if}{runSentence(run, activeModel.project.tasks.some((t) => activeTask(t.status)), activeModel.project.capabilities.includes("steer") || activeModel.project.capabilities.includes("review")).text}</span>{/if}
    {:else if here}<span>{inLab} project{inLab === 1 ? "" : "s"} in {hereName}</span>{/if}
    <span class="right">{#if fileState}<span class:bad={fileState.startsWith("Does not")}>{fileState}</span>{/if}<span class="hint">Ctrl or ⌘ K to find</span></span>
    <span class="visually-hidden" aria-live="polite">{announcement}</span>
  </footer>
</div>

{#if paletteOpen}
  <!-- svelte-ignore a11y_click_events_have_key_events, a11y_no_static_element_interactions -->
  <div class="scrim" onclick={() => (paletteOpen = false)}></div>
  <div class="palette" role="dialog" aria-modal="true" aria-label="Find a record">
    <input bind:this={paletteInput} bind:value={query} oninput={() => (pick = 0)} onkeydown={paletteKeys}
      placeholder="Find a figure, table, step or agent decision" role="combobox" aria-expanded="true" aria-controls="palette-results" aria-autocomplete="list"
      aria-activedescendant={results[pick] ? `palette-${pick}` : undefined} />
    <ul id="palette-results" role="listbox" aria-label="Matching records">
      {#each results as node, i (node.id)}
        <li id={`palette-${i}`} role="option" aria-selected={i === pick}>
          <button tabindex="-1" onclick={() => choose(node)} onpointerenter={() => (pick = i)}>
            <span class="kind">{kindWords(node)}</span>
            <span class="label">{node.label}</span>{#if placeOf(node) || node.detail}<span class="detail">{[placeOf(node), node.detail].filter(Boolean).join(" · ")}</span>{/if}</button>
        </li>
      {:else}
        <li class="empty">{query ? "No record by that name. Projects that are not open yet are matched by title only." : "Type part of a name."}</li>
      {/each}
    </ul>
  </div>
{/if}

<style>
  .bench {
    display: grid; height: calc(100vh - var(--bar-h) - 3px); height: calc(100dvh - var(--bar-h) - 3px); overflow: hidden; background: var(--canvas);
    grid-template-columns: 48px var(--side) 5px minmax(0, 1fr) var(--insp, 300px);
    grid-template-rows: minmax(0, 1fr) auto auto auto;
    grid-template-areas: "act side ex editor insp" "act side ex ey insp" "act side ex panel insp" "status status status status status";
  }
  .bench.no-side { grid-template-columns: 48px 0 0 minmax(0, 1fr) var(--insp, 300px); }
  .bench.no-side .side, .bench.no-side .edge.x { display: none; }
  .bench.no-insp { --insp: 0px; }
  .panes, .side-tabs { display: none; }

  /* The activity bar: one icon per tool, its name shown on hover and focus. */
  .act { grid-area: act; display: flex; flex-direction: column; align-items: center; gap: 4px; padding: 8px 0; border-right: 1px solid var(--line); background: var(--surface); }
  .act button { position: relative; display: grid; place-items: center; width: 40px; height: 40px; border-radius: var(--r-2); color: var(--text-3); transition: color var(--t-fast) var(--ease), background-color var(--t-fast) var(--ease); }
  .act button:hover { color: var(--heading); background: var(--row-hover); }
  .act button.on { color: var(--heading); }
  .act button.on::before { content: ""; position: absolute; left: -4px; top: 8px; bottom: 8px; width: 3px; border-radius: 2px; background: var(--mark); }
  .act button:disabled { opacity: 0.4; }
  .act svg { width: 20px; height: 20px; fill: none; stroke: currentColor; stroke-width: 1.5; stroke-linecap: round; stroke-linejoin: round; }
  .act-count { position: absolute; right: 2px; bottom: 2px; min-width: 15px; padding: 0 3px; border-radius: var(--r-pill); background: var(--surface); box-shadow: 0 0 0 1px var(--line-2);
    font-size: 10px; line-height: 15px; font-weight: 700; color: var(--text-2); font-variant-numeric: tabular-nums; }
  .tip { position: absolute; left: calc(100% + 8px); top: 50%; z-index: 20; padding: 4px 8px; border-radius: var(--r-2); background: var(--brand); color: var(--on-brand);
    font-size: var(--font-xs); font-weight: 600; white-space: nowrap; pointer-events: none; opacity: 0; transform: translate(-4px, -50%); transition: opacity 120ms var(--ease), transform 120ms var(--ease); }
  .act button:hover .tip, .act button:focus-visible .tip { opacity: 1; transform: translate(0, -50%); }

  /* The side pane: the project picker and its filter at the top, then the project's rows in plain groups. */
  .side { grid-area: side; display: flex; flex-direction: column; min-height: 0; min-width: 0; border-right: 1px solid var(--line); background: var(--track); }
  .side-head { display: grid; gap: 10px; padding: 12px 10px 10px; }
  .picker { position: relative; display: grid; gap: 1px; width: 100%; padding: 8px 32px 9px 12px; border-radius: var(--r-3); text-align: left; background: var(--surface); box-shadow: var(--group-edge); transition: box-shadow var(--t-fast) var(--ease); }
  .picker:hover { box-shadow: var(--raised-edge); }
  .picker .p-lab { font-size: var(--font-xs); color: var(--text-3); white-space: nowrap; overflow: hidden; text-overflow: ellipsis; }
  .picker .p-title { font-size: calc(14px * var(--type-scale)); font-weight: 650; color: var(--heading); line-height: 1.3; display: -webkit-box; -webkit-line-clamp: 2; line-clamp: 2; -webkit-box-orient: vertical; overflow: hidden; }
  .picker svg { position: absolute; right: 12px; top: 50%; width: 12px; height: 12px; margin-top: -6px; fill: none; stroke: var(--text-2); stroke-width: 1.6; stroke-linecap: round; stroke-linejoin: round; }
  .side-filter input { min-height: 34px; background: var(--surface); }
  .picker-panel { position: fixed; inset: auto; max-height: min(70vh, 560px); overflow: auto; padding: 8px 6px; }
  .picker-panel:popover-open { display: grid; gap: 2px; }
  .pk-lab { margin: 6px 10px 4px; font-size: var(--font-xs); font-weight: 700; color: var(--text-3); }
  .pk-list { list-style: none; margin: 0 0 4px; padding: 0; display: grid; gap: 1px; }
  .pk-list button, .matches button { position: relative; display: grid; gap: 1px; width: 100%; padding: 8px 10px 8px 24px; border-radius: var(--r-2); text-align: left; transition: background-color var(--t-fast) var(--ease); }
  .pk-list button:hover, .matches button:hover { background: var(--row-hover); }
  .pk-list button[aria-current]::before, .matches button.current::before { content: ""; position: absolute; left: 10px; top: 14px; width: 7px; height: 7px; border-radius: 50%; background: var(--mark); }
  .pk-list .t, .matches .t { font-size: calc(13.5px * var(--type-scale)); font-weight: 600; color: var(--heading); overflow-wrap: anywhere; }
  .pk-list .s, .matches .s, .matches .k { font-size: var(--font-xs); color: var(--text-3); }
  .matches { list-style: none; margin: 0; padding: 4px 8px; display: grid; gap: 1px; }
  .side-group { margin: 14px 18px 4px; font-size: calc(11.5px * var(--type-scale)); font-weight: 700; color: var(--text-3); letter-spacing: 0.01em; }
  .side-group .n { margin-left: 4px; font-weight: 400; font-variant-numeric: tabular-nums; }
  .tree { flex: 1; min-height: 0; overflow: auto; padding: 2px 0 16px; }
  .note { padding: 10px 14px; font-size: var(--font-small); color: var(--text-3); display: flex; gap: 8px; align-items: center; }
  .note.error { color: var(--failed); }
  .problems { list-style: none; margin: 0 0 6px; padding: 0 14px; display: grid; gap: 8px; }
  .problems li { position: relative; padding-left: 16px; font-size: var(--font-small); }
  .problems li::before { content: ""; position: absolute; left: 0; top: 7px; width: 6px; height: 6px; background: var(--text-3); }
  .problems li.failed::before { background: var(--failed); transform: rotate(45deg); }
  .problems li.attention::before { background: var(--mark); }
  .problems .link { text-align: left; }
  .edge { position: relative; touch-action: none; }
  .edge:focus-visible { outline: 2px solid var(--focus); outline-offset: -2px; }
  .edge.x { grid-area: ex; cursor: col-resize; }
  .edge.y { grid-area: ey; height: 5px; cursor: row-resize; }
  .edge::after { content: ""; position: absolute; inset: 0; background: var(--mark); opacity: 0; transition: opacity 120ms var(--ease); }
  .edge:hover::after, .edge:active::after { opacity: 0.4; }

  /* The editor: tabs with a preview tab in italics, the record's place as breadcrumbs, then the record. */
  .editor { grid-area: editor; display: flex; flex-direction: column; min-width: 0; min-height: 0; }
  .tabs-row { display: flex; flex: none; align-items: stretch; border-bottom: 1px solid var(--line); background: var(--surface); }
  .tabs { display: flex; flex: 1; min-width: 0; overflow-x: auto; scrollbar-width: thin; }
  .tab { position: relative; flex: none; display: flex; align-items: center; max-width: 280px; border-right: 1px solid var(--line); }
  .tab button[role="tab"] { min-width: 0; min-height: 38px; padding: 8px 14px; font-size: var(--font-small); color: var(--text-2); white-space: nowrap; overflow: hidden; text-overflow: ellipsis; }
  .tab:has(.x) button[role="tab"] { padding-right: 4px; }
  .tab.preview button[role="tab"] { font-style: italic; }
  .tab.active { background: var(--canvas); }
  .tab.active button[role="tab"] { color: var(--heading); font-weight: 600; }
  .tab.active::before { content: ""; position: absolute; left: 0; right: 0; top: 0; height: 2px; background: var(--mark); }
  .tab .x { width: 26px; height: 26px; margin-right: 4px; border-radius: var(--r-2); color: var(--text-3); font-size: 15px; line-height: 1; }
  .tab .x:hover { background: var(--hover-wash); color: var(--heading); }
  .insp-toggle { flex: none; padding: 0 14px; font-size: var(--font-xs); font-weight: 600; color: var(--text-3); border-left: 1px solid var(--line); }
  .insp-toggle:hover { color: var(--heading); }
  .crumbs { flex: none; padding: 6px 18px; border-bottom: 1px solid var(--line); background: var(--canvas); overflow-x: auto; scrollbar-width: none; }
  .crumbs ol { list-style: none; margin: 0; padding: 0; display: flex; align-items: center; gap: 4px; white-space: nowrap; font-size: var(--font-xs); color: var(--text-3); }
  .crumbs li { display: flex; align-items: center; gap: 4px; }
  .crumbs svg { width: 9px; height: 9px; fill: none; stroke: currentColor; stroke-width: 1.4; stroke-linecap: round; stroke-linejoin: round; }
  .crumb { min-height: 24px; padding: 0 2px; font-size: inherit; color: var(--text-2); }
  .crumb:hover { color: var(--heading); text-decoration: underline; text-underline-offset: 3px; }
  .crumbs [aria-current] { color: var(--text); font-weight: 600; }
  .content { flex: 1; min-height: 0; overflow: auto; }
  .welcome { max-width: 720px; padding: 40px 32px; display: grid; gap: 14px; }
  .welcome h1 { font-size: calc(26px * var(--type-scale)); }
  .welcome .lede { font-size: calc(15px * var(--type-scale)); line-height: 1.55; color: var(--text-2); }
  .welcome h2 { margin: 12px 0 6px; font-size: calc(12px * var(--type-scale)); font-weight: 700; color: var(--text-3); letter-spacing: 0.01em; }
  .recent ul { list-style: none; margin: 0; padding: 0; border-radius: var(--r-3); background: var(--surface); box-shadow: var(--group-edge); overflow: hidden; }
  .recent li + li { border-top: 1px solid var(--line); }
  .recent-row { display: grid; grid-template-columns: minmax(0, 1fr) auto; grid-template-areas: "title state" "where when"; column-gap: 16px; row-gap: 3px;
    width: 100%; padding: 11px 14px; text-align: left; transition: background-color var(--t-fast) var(--ease); }
  .recent-row:hover { background: var(--row-hover); }
  .recent-row:focus-visible { outline: 2px solid var(--focus); outline-offset: -2px; }
  .r-title { grid-area: title; min-width: 0; font-size: calc(14px * var(--type-scale)); font-weight: 600; color: var(--heading); white-space: nowrap; overflow: hidden; text-overflow: ellipsis; }
  .r-state { grid-area: state; justify-self: end; font-size: var(--font-xs); }
  .r-where { grid-area: where; min-width: 0; font-size: var(--font-xs); color: var(--text-3); white-space: nowrap; overflow: hidden; text-overflow: ellipsis; }
  .r-when { grid-area: when; justify-self: end; font-size: var(--font-xs); color: var(--text-3); font-variant-numeric: tabular-nums; white-space: nowrap; }
  .keys { display: grid; grid-template-columns: max-content minmax(0, 1fr); gap: 9px 20px; margin: 0; font-size: var(--font-small); color: var(--text-2); }
  .keys dt { color: var(--text); white-space: nowrap; }
  .keys dd { margin: 0; }
  kbd { font: var(--font-xs) var(--mono); padding: 1px 5px; border-radius: 3px; box-shadow: inset 0 0 0 1px var(--line-2); color: var(--text); }

  /* The inspector: the open record's facts, then its thread back to the question. */
  .inspector { grid-area: insp; min-width: 0; min-height: 0; overflow: auto; display: grid; gap: 18px; align-content: start; padding: 14px 16px 20px;
    border-left: 1px solid var(--line); background: var(--track); }
  .insp-head { display: grid; gap: 2px; }
  .insp-kind { font-size: var(--font-xs); color: var(--text-3); }
  .insp-title { font-size: calc(15px * var(--type-scale)); line-height: 1.3; color: var(--heading); overflow-wrap: anywhere; }
  .inspector h3 { font-size: calc(11.5px * var(--type-scale)); font-weight: 700; color: var(--text-3); letter-spacing: 0.01em; margin-bottom: 8px; }
  .facts { margin: 0; display: grid; gap: 7px; }
  .facts div { display: grid; gap: 1px; }
  .facts dt { font-size: var(--font-xs); color: var(--text-3); }
  .facts dd { margin: 0; font-size: var(--font-small); color: var(--text); overflow-wrap: anywhere; }
  .thread { list-style: none; margin: 0; padding: 0; }
  .thread li { position: relative; display: grid; grid-template-columns: 16px minmax(0, 1fr); gap: 10px; padding-bottom: 13px; }
  .thread li:not(:last-child)::after { content: ""; position: absolute; left: 7px; top: 15px; bottom: -1px; width: 1.5px; background: var(--line-2); }
  .thread .dot { position: relative; z-index: 1; width: 10px; height: 10px; margin: 4px 0 0 2.5px; border-radius: 50%; border: 1.5px solid var(--text-3); background: var(--canvas); }
  .thread li.current .dot { border-color: var(--mark); background: var(--mark); }
  .thread li.failed .hop { color: var(--failed); }
  .hop-body { display: grid; gap: 1px; justify-items: start; min-width: 0; }
  .role { font-size: var(--font-xs); color: var(--text-3); }
  .hop { text-align: left; font-size: var(--font-small); color: var(--text); line-height: 1.4; overflow-wrap: anywhere; }
  .thread li.current .hop { font-weight: 650; color: var(--heading); }
  .thread button.hop { color: var(--heading); text-decoration: underline; text-decoration-color: var(--line-2); text-underline-offset: 3px; }
  .thread button.hop:hover, .thread button.hop:focus-visible { text-decoration-color: var(--mark); }
  .detail { font-size: var(--font-xs); color: var(--text-3); overflow-wrap: anywhere; }

  /* The panel: the timeline, a step's output and service activity. */
  .panel { grid-area: panel; display: flex; flex-direction: column; min-height: 0; min-width: 0; border-top: 1px solid var(--line); }
  .panel-tabs { flex: none; display: flex; align-items: center; height: 34px; padding: 0 10px; background: var(--surface); }
  .panel-tablist { display: flex; align-items: center; gap: 2px; height: 100%; }
  .panel-tabs button[role="tab"] { padding: 8px 10px 6px; font-size: var(--font-xs); font-weight: 650; color: var(--text-3); border-bottom: 2px solid transparent; }
  .panel-tabs button[aria-selected="true"] { color: var(--heading); border-bottom-color: var(--mark); }
  .fold { margin-left: auto; font-size: var(--font-xs); color: var(--text-3); padding: 4px 8px; }
  .fold:hover { color: var(--heading); }
  .panel-body { height: var(--panel); overflow: auto; padding: 10px 18px 12px; }
  .panel-body:focus { outline: none; }
  .logs { list-style: none; margin: 0; padding: 0; display: grid; gap: 6px; }
  .logs .link { font-family: var(--mono); font-size: var(--font-small); }
  .activity { list-style: none; margin: 0; padding: 0; }
  .activity li { display: grid; grid-template-columns: 170px minmax(0, 1fr); gap: 12px; padding: 4px 0; border-bottom: 1px solid var(--line); font-size: var(--font-small); }
  .activity time { color: var(--text-3); font-variant-numeric: tabular-nums; }

  .status { grid-area: status; display: flex; align-items: center; min-height: 26px; padding: 0 8px; background: var(--brand); color: var(--on-brand); font-size: var(--font-xs); overflow: hidden; white-space: nowrap; }
  .status > span:not(.visually-hidden), .status .right > span { display: inline-flex; align-items: center; gap: 6px; padding: 0 10px; border-right: 1px solid rgb(255 255 255 / 0.2); }
  .status .run { overflow: hidden; text-overflow: ellipsis; min-width: 0; }
  .status .waveform i { background: var(--on-brand); }
  .status .right { margin-left: auto; display: flex; border-right: 0; padding: 0; }
  .status .right > span:last-child { border-right: 0; }
  .status .bad { color: var(--uc-rose); font-weight: 650; }
  .status .hint { color: var(--on-brand-2); }

  .scrim { position: fixed; inset: 0; z-index: 30; background: var(--scrim); }
  .palette { position: fixed; z-index: 31; top: calc(var(--bar-h) + 40px); left: 50%; transform: translateX(-50%); width: min(640px, calc(100vw - 32px));
    background: var(--surface); border: 1px solid var(--line-2); border-radius: var(--r-2); box-shadow: var(--overlay-shadow); overflow: hidden; }
  .palette input { width: 100%; padding: 12px 16px; border: 0; border-bottom: 1px solid var(--line); border-radius: 0; font-size: calc(15px * var(--type-scale)); background: transparent; color: var(--text); }
  .palette input:focus { outline: none; box-shadow: inset 0 -2px 0 var(--mark); }
  .palette ul { list-style: none; margin: 0; padding: 4px 0; max-height: min(420px, 60vh); overflow: auto; }
  .palette li button { width: 100%; display: grid; grid-template-columns: 124px minmax(0, 1fr); column-gap: 12px; padding: 7px 16px; text-align: left; }
  .palette li[aria-selected="true"] button { background: var(--surface-2); box-shadow: inset 2px 0 0 var(--mark); }
  .palette .kind { font-size: var(--font-xs); color: var(--text-3); padding-top: 2px; }
  .palette .label { font-size: var(--font-small); color: var(--heading); font-weight: 600; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
  .palette .detail { grid-column: 2; font: var(--font-xs) var(--mono); color: var(--text-3); overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
  .palette .empty { padding: 12px 16px; font-size: var(--font-small); color: var(--text-3); }

  /* A closed inspector leaves the layout and the tab order; on a phone it is the Thread pane. Narrower windows give it
     less room. */
  @media (min-width: 901px) { .bench.no-insp .inspector { display: none; } }
  @media (max-width: 1200px) and (min-width: 901px) { .bench:not(.no-insp) { --insp: 280px; } }
  /* Phones and narrow windows show one pane at a time, switched from a bar within thumb reach; the side pane's views are
     a segmented control at its top instead of the activity bar. */
  @media (max-width: 900px) {
    .bench, .bench.no-side { grid-template-columns: minmax(0, 1fr); grid-template-rows: minmax(0, 1fr) auto auto; grid-template-areas: "main" "panes" "status"; }
    .act, .edge, .insp-toggle { display: none; }
    .panes { grid-area: panes; display: flex; border-top: 1px solid var(--line); background: var(--surface); }
    .panes button { flex: 1; min-height: 44px; font-size: var(--font-small); font-weight: 600; color: var(--text-2); box-shadow: inset 0 2px 0 transparent; }
    .panes button[aria-pressed="true"] { color: var(--heading); box-shadow: inset 0 2px 0 var(--mark); }
    .side-tabs { display: flex; gap: 2px; margin: 10px 10px 0; padding: 3px; border-radius: var(--r-pill); background: var(--surface); box-shadow: var(--group-edge); }
    .side-tabs button { flex: 1; min-height: 36px; border-radius: var(--r-pill); font-size: var(--font-xs); font-weight: 650; color: var(--text-2); }
    .side-tabs button[aria-pressed="true"] { background: var(--raised); box-shadow: var(--raised-edge); color: var(--heading); }
    .side-tabs .n { margin-left: 4px; font-variant-numeric: tabular-nums; }
    .side, .editor, .panel, .inspector { grid-area: main; border: 0; }
    .bench.no-side .side { display: flex; }
    .bench:not([data-pane="explorer"]) .side, .bench:not([data-pane="editor"]) .editor, .bench:not([data-pane="panel"]) .panel,
    .bench:not([data-pane="inspector"]) .inspector { display: none; }
    .bench[data-pane="inspector"] .inspector { display: grid; }
    .panel-body { flex: 1; height: auto; }
    .fold, .status .hint, .status .run { display: none; }
    /* Every status fact stays reachable on a phone by swiping the bar. */
    .status { overflow-x: auto; scrollbar-width: none; }
    .status .right { margin-left: 0; }
    .welcome { padding: 28px 16px; }
    .crumbs { padding: 6px 16px; }
    .keys { grid-template-columns: minmax(0, 1fr); gap: 2px; }
    .keys dd { margin-bottom: 8px; }
    .activity li { grid-template-columns: 1fr; gap: 0; }
    .palette { top: 16px; }
  }
  @media (prefers-reduced-motion: reduce) { .edge::after, .act button, .tip { transition: none; } }
</style>
