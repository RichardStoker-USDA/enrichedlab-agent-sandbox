<script lang="ts">
  import { onMount, untrack } from "svelte";
  import AdministrationPage from "./AdministrationPage.svelte";
  import AnalysesPage from "./AnalysesPage.svelte";
  import AnalysisPage from "./AnalysisPage.svelte";
  import { broker, BrokerError } from "./api.ts";
  import AskSheet from "./AskSheet.svelte";
  import { askDraft } from "./ask_state.svelte.ts";
  import { offeredProfiles, type Identity, type Submission } from "./contracts.ts";
  import { labOrder, operatorLab } from "./context.ts";
  import Dialog from "./Dialog.svelte";
  import { currentLab, lab } from "./lab.svelte.ts";
  import LabIndex from "./LabIndex.svelte";
  import { landing, landingWords } from "./landing.ts";
  import { initials as initialsOf, person } from "./language.ts";
  import { markOpened, openedRecord } from "./opened.ts";
  import ProjectPage from "./ProjectPage.svelte";
  import SearchPalette from "./SearchPalette.svelte";
  import type { SearchEntry } from "./search.ts";
  import SharedNotesPage from "./SharedNotesPage.svelte";
  import WaitingPage from "./WaitingPage.svelte";
  import WorkingPage from "./WorkingPage.svelte";
  import { acrossLabs, forLab, itemKey, newlyWaiting, noticeWords, tabTitle, unseen, unseenByLab, type InboxItem } from "./inbox.ts";
  import { live, type LiveLink } from "./live.ts";
  import { waiting } from "./waiting.svelte.ts";
  import LibraryPage from "./LibraryPage.svelte";
  import ToolsPage from "./ToolsPage.svelte";
  import CommonsPage from "./CommonsPage.svelte";
  import PaperPage from "./PaperPage.svelte";
  import { factsFor, indexFilter, openWork, pageTitle, projectIndex, resetFilter, standingFor } from "./project_index.svelte.ts";
  import { projects, type Project } from "./projects.ts";
  import DatasetPage from "./DatasetPage.svelte";
  import { href, navigate, parseRoute, type Route } from "./route.ts";
  import { standingNames } from "./standing.ts";
  import WorkbenchPage from "./WorkbenchPage.svelte";

  let identity = $state<Identity | null>(null);
  // What waits for the reader, as the broker last gave it (waiting.svelte.ts); the count is the
  // unseen items the chosen lab shows, so it follows every change of lab wherever it happens. Zero when unknown.
  const inboxItems = $derived(waiting.items);
  const inboxRead = $derived(waiting.phase !== "idle");
  const waitingCount = $derived(unseen(forLab(inboxItems, lab.key)));
  // Another lab's items show only as counts: beside each lab in the lab menu, and as a mark on the lab button.
  const labWaiting = $derived(unseenByLab(acrossLabs(inboxItems, lab.key)));
  const waitingElsewhere = $derived([...labWaiting].reduce((n, [key, count]) => (key === lab.key ? n : n + count), 0));
  // Whether the broker's change stream is connected (live.ts); without it, lists read again every minute. Only a stream
  // that was live and stays lost for a few seconds is mentioned: a broker without one is the ordinary state, and a
  // reconnect that succeeds at once is not news.
  const LOST_NOTE_MS = 5_000;
  let liveLost = $state(false);
  let failure = $state("");
  let route = $state<Route>(parseRoute(location.hash));
  // The theme follows the device until a person picks Light or Dark, and System returns it to the device.
  const systemDark = matchMedia("(prefers-color-scheme: dark)");
  let theme = $state(stored("workspace_theme", "system"));
  let textSize = $state(stored("workspace_text_size", "standard"));
  // The sidebar opens as a drawer below the wide layout and over the Detailed view.
  let menuOpen = $state(false);
  let searchOpen = $state(false);
  let notices = $state<{ id: number; text: string; error: boolean }[]>([]);
  // Inspection expiry is client knowledge only; the broker enforces it on every request.
  let inspections = $state<Record<string, string>>({});
  let composerOpen = $state(false);
  let draft = $state<Submission>({ group: "", profile: "", objective: "" });
  let draftContext = $state<Project | null>(null);
  let draftKey = "", draftBody = "";
  let busy = $state(false);
  let dialogFailure = $state("");
  let noticeId = 0;
  // Each change of page counts, so the landing rule never moves a reader who has already gone somewhere.
  let moves = 0;

  const admin = $derived(identity?.administration?.project_inspection ?? false);
  // The lab is the letterhead: its name leads the bar, and its menu changes the lab in view.
  const here = $derived(currentLab(identity));
  const labName = $derived(here?.name ?? "Research workspace");
  const organization = $derived(identity?.site?.organization?.trim() ?? "");
  const labCount = (key: string) => projectIndex.list.filter((p) => p.group_key === key).length;
  const onIndex = $derived(route.name === "home");
  // A project and its analyses sit under Your research, so the sidebar keeps that place marked while one is open.
  const inProject = $derived(route.name === "project" || route.name === "dataset" || (route.name === "analysis" && !!route.project));
  // The lab in view is where the reader is: counts, search and landing read its projects only.
  const shownLab = (key: string) => key === lab.key;
  const workingCount = $derived(projectIndex.list.filter((p) => shownLab(p.group_key) && standingFor(p) === "working").length);
  const detailed = $derived(route.name === "workbench");
  // The project in view, for the Simple and Detailed switch and the tab title.
  const projectInView = $derived(route.name === "project" || route.name === "workbench" ? route.project
    : route.name === "analysis" || route.name === "dataset" ? route.project : undefined);
  const isMac = /Mac|iPhone|iPad/.test(navigator.userAgent);
  // One lab's page has its own address, so a reload or the back button returns to the page the reader left (once,
  // after a reload, a lab's page became "Your research"). Other filters stay with the page.
  // The filter changes with the page, so a move the reader overtakes (another link before the transition ends) leaves
  // both as they were.
  function place(next: Partial<typeof indexFilter>) { go(next.labs?.length === 1 ? { name: "home", lab: next.labs[0] } : { name: "home" }, () => resetFilter(next)); }
  // The address decides the lab in view when the page opens and when the reader goes back or forward. A lab the
  // reader does not belong to opens Your research instead. The home address without a lab clears a lab the last home
  // address chose, also when the reader reaches it from another page; labs chosen in the filter stay with the page.
  let lastHome: Route | null = null;
  $effect(() => { if (route.name === "home") lastHome = route; });
  function followAddress() {
    if (route.name !== "home") return;
    if (route.lab) {
      if (identity && !identity.groups.some((g) => g.key === (route as { lab?: string }).lab)) {
        history.replaceState(null, "", "#/"); route = { name: "home" }; indexFilter.labs = []; return;
      }
      if (identity) lab.set(route.lab);
      indexFilter.labs = [route.lab];
    } else if (lastHome?.name === "home" && lastHome.lab) indexFilter.labs = [];
  }
  const roleWord = (role: string) => role ? role.charAt(0).toUpperCase() + role.slice(1).replaceAll("_", " ") : "";
  // Research labs lead every list; operator labs sit apart (see context.ts).
  const researchLabs = $derived(labOrder(identity?.groups ?? []).filter((g) => !operatorLab(g.key)));
  const operatorLabs = $derived((identity?.groups ?? []).filter((g) => operatorLab(g.key)));
  function chooseLab(key: string) {
    lab.set(key);
    document.getElementById("lab-menu")?.hidePopover();
    // The Detailed view stays open, on the chosen lab's start page; every other page goes to the lab's projects.
    if (route.name === "workbench") go({ name: "workbench" });
    else place({ labs: [key] });
  }
  // The list is read again whenever what waits for the reader changes (live.ts): their own reviews and marks at once,
  // the agent's and colleagues' changes as the broker's stream reports them, and without the stream every minute and
  // when the tab is shown again. Items new since the last answer may raise a notice while the tab is in the background.
  let waitingKeys: Set<string> | null = null;
  $effect(() => {
    const items = waiting.items, read = waiting.phase === "ready";
    untrack(() => {
      if (!read) return;
      const fresh = newlyWaiting(waitingKeys, acrossLabs(items, lab.key));
      waitingKeys = new Set(items.map(itemKey));
      if (notifyOn && document.hidden) announce(fresh);
    });
  });
  // A notice is opt-in and per browser ("start it and walk away"). It appears only while the tab is in
  // the background and names no project (see noticeWords). Touch devices are left out: their browsers show page
  // notices only from an installed app's service worker, which the workspace does not have.
  const notifySupported = typeof Notification !== "undefined" && !matchMedia("(pointer: coarse)").matches;
  let notifyOn = $state(notifySupported && stored("workspace_notify", "") === "on" && Notification.permission === "granted");
  let notifyBlocked = $state(notifySupported && Notification.permission === "denied");
  async function setNotify(on: boolean) {
    if (!notifySupported) return;
    if (on && Notification.permission !== "granted") {
      const answer = await Notification.requestPermission().catch(() => "denied" as NotificationPermission);
      notifyBlocked = answer === "denied";
      on = answer === "granted";
    }
    notifyOn = on;
    // A refused or dismissed prompt leaves the setting off, so the choice shown returns to Off.
    for (const input of document.querySelectorAll<HTMLInputElement>('input[name="notify"]')) input.checked = input.value === (on ? "on" : "off");
    try { if (on) localStorage.setItem("workspace_notify", "on"); else localStorage.removeItem("workspace_notify"); } catch { /* a convenience only */ }
  }
  function announce(fresh: InboxItem[]) {
    const words = noticeWords(fresh, lab.key);
    if (!words) return;
    try {
      const notice = new Notification(words.title, { body: words.body, tag: "research-workspace-waiting" });
      notice.onclick = () => { window.focus(); if (words.lab !== lab.key) lab.set(words.lab); go({ name: "waiting" }); notice.close(); };
    } catch { /* a browser that allows notices only from a service worker */ }
  }
  const inspection = $derived(route.name === "project" ? inspections[route.project] ?? null : null);
  const preview = $derived((identity as (Identity & { preview?: string }) | null)?.preview === "synthetic");

  function stored(key: string, fallback: string) {
    try { return localStorage.getItem(key) ?? fallback; } catch { return fallback; }
  }
  // Theme and text size are per-browser conveniences, not account settings.
  function applyTheme() {
    document.documentElement.dataset.theme = theme === "light" || theme === "dark" ? theme : systemDark.matches ? "dark" : "light";
  }
  function setDisplay(nextTheme: string, nextSize: string) {
    theme = nextTheme; textSize = nextSize;
    applyTheme();
    if (nextSize === "standard") delete document.documentElement.dataset.typeSize;
    else document.documentElement.dataset.typeSize = nextSize;
    try { localStorage.setItem("workspace_theme", nextTheme); localStorage.setItem("workspace_text_size", nextSize); } catch { /* a convenience only */ }
  }
  function go(next: Route, alongside?: () => void) {
    menuOpen = false; moves++;
    navigate(next, () => { alongside?.(); route = next; pageTitle.value = ""; window.scrollTo({ top: 0 }); });
  }
  // The crumbs at the top of the page column name where a page sits; every step but the last is a link.
  type Step = { label: string; route?: Route };
  const titleOf = (id: string | undefined) => (id ? projectIndex.list.find((p) => p.project_id === id)?.title ?? "" : "");
  const trail = $derived.by((): Step[] => {
    const home: Step = { label: "Your research", route: { name: "home" } };
    switch (route.name) {
      case "home": case "working": case "waiting": case "workbench": return [];
      case "project": return [home, { label: pageTitle.value || titleOf(route.project) || "Project" }];
      case "analysis": return route.project
        ? [home, { label: titleOf(route.project) || "Project", route: { name: "project", project: route.project } }, { label: pageTitle.value || "Analysis" }]
        : [{ label: "Results", route: { name: "analyses" } }, { label: pageTitle.value || "Analysis" }];
      case "dataset": return [home, { label: titleOf(route.project) || "Project", route: { name: "project", project: route.project } }, { label: pageTitle.value || "Lab data" }];
      case "paper": return [{ label: "Library", route: { name: "library" } }, { label: pageTitle.value || "Paper" }];
      case "commons": case "notes": return [{ label: "Library", route: { name: "library" } }, { label: crumb }];
      default: return [];
    }
  });
  function closeMenus() { for (const id of ["lab-menu", "account-menu", "display-settings"]) document.getElementById(id)?.hidePopover?.(); }
  const crumb = $derived(route.name === "analyses" ? "Results" : route.name === "administration" ? "Inspection" : route.name === "workbench" ? "Detailed view"
    : route.name === "notes" ? "Shared notes" : route.name === "waiting" ? "Waiting for you" : route.name === "working" ? "Working now"
    : route.name === "library" ? "Library" : route.name === "commons" ? "Commons" : route.name === "tools" ? "Tools" : route.name === "home" ? "" : pageTitle.value);
  $effect(() => {
    const base = route.name === "workbench" && pageTitle.value ? `${pageTitle.value} · Detailed view · ${labName}` : crumb ? `${crumb} · ${labName}` : labName;
    document.title = tabTitle(base, waitingCount, openWork.busy && (route.name === "project" || route.name === "workbench"));
  });
  // When work on the open project stops, what waits for the reader may have changed.
  let wasWorking = false;
  $effect(() => {
    const working = openWork.busy;
    untrack(() => { if (wasWorking && !working && identity) live().changed(["inbox", "projects"], "derived"); wasWorking = working; });
  });
  // An opened drawer takes the focus to its first place, so the keyboard starts where the eye does.
  $effect(() => {
    if (!menuOpen) return;
    untrack(() => requestAnimationFrame(() => document.querySelector<HTMLElement>("#sidebar.open .places a")?.focus({ preventScroll: true })));
  });
  // The browser remembers when it last showed each project, on arrival and on leaving, for the landing rule (opened.ts).
  let shownProject = "";
  $effect(() => {
    const id = route.name === "project" ? route.project : "";
    untrack(() => {
      if (shownProject && shownProject !== id) markOpened(shownProject);
      if (id) markOpened(id);
      shownProject = id;
    });
  });
  const initials = $derived(initialsOf(identity?.principal));
  function notify(text: string, error = false) {
    const id = ++noticeId;
    notices = [...notices, { id, text, error }];
    setTimeout(() => { notices = notices.filter((n) => n.id !== id); }, error ? 9000 : 6000);
  }
  async function loadIdentity() {
    failure = "";
    try {
      // The change stream connects once the broker says it serves one, and the first lists are read once it is ready
      // (or known to be missing), so no change falls between a list and the stream (live.ts). An older broker is never
      // asked for it.
      const me = await broker.me();
      const hub = live();
      hub.start({ stream: me.change_stream === true });
      await hub.whenReady();
      identity = me;
      lab.settle(identity);
      followAddress();
      void waiting.load();
      void projectIndex.load();
    } catch (cause) {
      failure = cause instanceof BrokerError && cause.code === "access_required"
        ? "This account does not have access to the research workspace, or the session needs to sign in again."
        : "The research service is unavailable right now. Work already started continues on the cluster.";
    }
  }

  // Where the workspace opens (landing.ts). Only the first load of the bare address in this tab lands, only once the
  // inbox and the project list are both known, and never after the reader has moved or started typing a question.
  const LANDED = "workspace_landed";
  let landingPending = false;
  function claimLanding(): boolean {
    if (location.hash !== "" && location.hash !== "#" && location.hash !== "#/") return false;
    try { if (sessionStorage.getItem(LANDED)) return false; sessionStorage.setItem(LANDED, "1"); } catch { return false; }
    return true;
  }
  $effect(() => {
    if (!identity || !inboxRead || projectIndex.phase === "idle" || projectIndex.phase === "loading") return;
    untrack(() => {
      if (!landingPending) return;
      landingPending = false;
      if (projectIndex.phase !== "ready" || moves || route.name !== "home" || route.lab || askDraft.question.trim() || !identity) return;
      const me = identity.principal.id;
      const result = landing({
        waiting: forLab(inboxItems, lab.key),
        projects: projectIndex.list.filter((p) => shownLab(p.group_key)).map((p) => ({ project_id: p.project_id, standing: standingFor(p),
          updated_at: p.updated_at, mine: (p.owner?.principal_id ?? p.owner_id) === me, last_look_at: p.last_look?.at ?? null })),
        opened: openedRecord(), now: Date.now(),
      });
      if (!result) return;
      history.replaceState(null, "", href(result.route));
      route = result.route;
      notify(landingWords(result.reason, titleOf(result.project_id) || "your project"));
    });
  });

  function setInspection(id: string, expires: string | null) {
    if (!id || (inspections[id] ?? null) === expires) return;
    const next = { ...inspections };
    if (expires) next[id] = expires; else delete next[id];
    inspections = next;
  }
  async function endInspection() {
    if (route.name !== "project") return;
    const id = route.project;
    try { await projects.endInspection(id); } catch { /* the expiry still applies */ }
    const next = { ...inspections };
    delete next[id];
    inspections = next;
    notify("Inspection ended. It is recorded in the project's access record.");
    go({ name: "administration" });
  }

  // New question: on home the ask field takes it; anywhere else the setup sheet opens with the question first.
  function newQuestion() {
    menuOpen = false; closeMenus();
    const field = document.getElementById("ask-question") as HTMLTextAreaElement | null;
    if (route.name === "home" && field) {
      field.scrollIntoView({ behavior: matchMedia("(prefers-reduced-motion: reduce)").matches ? "auto" : "smooth", block: "center" });
      field.focus({ preventScroll: true });
      return;
    }
    askDraft.lab = indexFilter.labs.length === 1 ? indexFilter.labs[0] : lab.key;
    askDraft.open = true;
  }
  function asked(project: string, started: boolean) {
    void projectIndex.load();
    go({ name: "project", project });
    notify(started ? "Started. You can leave this page; the agent keeps working on the cluster, and Waiting for you shows it when it needs you."
      : "Saved. Nothing runs until you start it from the project.");
  }
  // Simple and Detailed show the same project; with none in view, Detailed opens on the lab and Simple on home.
  function toDetailed() { if (!detailed) go(projectInView ? { name: "workbench", project: projectInView } : { name: "workbench" }); }
  function toSimple() { if (detailed) go(projectInView ? { name: "project", project: projectInView } : { name: "home" }); }

  function openComposer(context: Project | null) {
    draftContext = context;
    draft = context
      ? { group: context.group_key, profile: offeredProfiles(identity, context.group_key)[0]?.key ?? "", objective: "", project_id: context.project_id, action_key: `individual-${crypto.randomUUID().slice(0, 8)}` }
      : { group: lab.key || (identity?.groups[0]?.key ?? ""), profile: offeredProfiles(identity, lab.key || identity?.groups[0]?.key)[0]?.key ?? "", objective: "" };
    draftBody = ""; dialogFailure = ""; composerOpen = true;
  }
  async function submit() {
    if (busy || !draft.objective.trim()) return;
    if (new TextEncoder().encode(draft.objective).length > 8192) { dialogFailure = "Please shorten the description to under 8,000 characters."; return; }
    const body = JSON.stringify(draft);
    if (body !== draftBody) { draftBody = body; draftKey = crypto.randomUUID(); }
    busy = true; dialogFailure = "";
    try {
      const result = await broker.submit(draft, draftKey);
      composerOpen = false;
      notify("Analysis requested. You can leave this page; it runs on the cluster.");
      go(draftContext ? { name: "analysis", project: draftContext.project_id, task: result.task_id } : { name: "analysis", task: result.task_id });
    } catch (cause) {
      dialogFailure = cause instanceof BrokerError && cause.code === "capacity_reached" ? "Your group's queue is full. Existing work continues; try again shortly." : "The analysis was not requested. Your text is still here.";
    } finally { busy = false; }
  }

  // The method stays one the draft's lab may run, after a change of lab by any path.
  $effect(() => {
    if (!composerOpen) return;
    const methods = offeredProfiles(identity, draft.group);
    if (!methods.some((m) => m.key === draft.profile)) draft.profile = methods[0]?.key ?? "";
  });

  // Search your research: the places, the actions and the reader's projects by title and question.
  const searchEntries = $derived.by((): SearchEntry[] => {
    if (!identity) return [];
    const placeEntry = (id: string, label: string, detail: string): SearchEntry => ({ id, kind: "place", label, detail, words: `${label} ${detail}`.toLowerCase() });
    const entries: SearchEntry[] = [
      placeEntry("home", "Your research", "Ask a question and see your projects"),
      placeEntry("waiting", "Waiting for you", waitingCount ? `${waitingCount} new` : "What needs you"),
      placeEntry("working", "Working now", workingCount ? `${workingCount} working` : "Projects an agent is working on"),
      placeEntry("analyses", "Results", "Every analysis and what it produced"),
      placeEntry("library", "Library", "The lab's papers"),
      placeEntry("tools", "Tools", "The methods the lab may run"),
      placeEntry("commons", "Commons", "What every lab receives"),
      placeEntry("notes", "Shared notes", "What the lab's agents read"),
      placeEntry("workbench", "Detailed view", "Every file, step and record"),
      { id: "new-question", kind: "command", label: "New question", detail: "Ask a research question", words: "new question ask start project" },
    ];
    if (offeredProfiles(identity, lab.key).length) entries.push({ id: "run-method", kind: "command", label: "Run one method", detail: "One analysis without a project", words: "run one method analysis" });
    if (admin) entries.push(placeEntry("administration", "Inspection", "Open a project for a stated reason"));
    for (const p of projectIndex.list.filter((x) => shownLab(x.group_key))) {
      const facts = factsFor(p);
      const labOf = identity.groups.find((g) => g.key === p.group_key)?.name ?? p.group_key;
      entries.push({ id: p.project_id, kind: "project", label: p.title, detail: `${labOf} · ${standingNames[standingFor(p)]}`,
        words: `${p.title} ${facts.question ?? ""} ${labOf}`.toLowerCase(), at: p.updated_at });
    }
    return entries;
  });
  function searched(entry: SearchEntry) {
    if (entry.kind === "project") { go({ name: "project", project: entry.id }); return; }
    if (entry.id === "new-question") { newQuestion(); return; }
    if (entry.id === "run-method") { openComposer(null); return; }
    if (entry.id === "home") { place({}); return; }
    go({ name: entry.id } as Route);
  }
  function globalKeys(event: KeyboardEvent) {
    // Escape closes the drawer and returns to the button that opened it.
    if (event.key === "Escape" && menuOpen) { menuOpen = false; document.querySelector<HTMLElement>("header.bar button.menu")?.focus(); return; }
    // The Detailed view keeps its own Command K for the records of its project.
    if (route.name === "workbench" || !identity) return;
    if ((event.metaKey || event.ctrlKey) && !event.altKey && event.key.toLowerCase() === "k") { event.preventDefault(); closeMenus(); searchOpen = true; }
  }

  onMount(() => {
    landingPending = claimLanding();
    setDisplay(theme, textSize);
    systemDark.addEventListener("change", applyTheme);
    void loadIdentity();
    const changed = () => { route = parseRoute(location.hash); moves++; followAddress(); pageTitle.value = ""; menuOpen = false; };
    addEventListener("popstate", changed);
    addEventListener("hashchange", changed);
    addEventListener("keydown", globalKeys);
    // Links saved before this layout used a bare task identifier; keep them working.
    if (/^#[a-f0-9-]{36}$/i.test(location.hash)) history.replaceState(null, "", `#/analyses/${location.hash.slice(1)}`);
    // The lists every page shows. What waits for the reader also reads while the tab is hidden, for its title and notices.
    const hub = live();
    const unwatch = [
      hub.watch(["inbox"], () => (identity ? waiting.load() : undefined), { whileHidden: true, poll: true, gapMs: 2_000 }),
      hub.watch(["projects"], () => (identity ? projectIndex.load() : undefined), { poll: true, gapMs: 2_000 }),
    ];
    let wasLive = false, noting: ReturnType<typeof setTimeout> | undefined;
    const offLink = hub.onLink((next: LiveLink) => {
      if (next === "live") { wasLive = true; clearTimeout(noting); noting = undefined; liveLost = false; }
      else if (wasLive && !liveLost && noting === undefined) noting = setTimeout(() => { noting = undefined; liveLost = hub.link !== "live"; }, LOST_NOTE_MS);
    });
    const hidden = () => { if (document.hidden && shownProject) markOpened(shownProject); };
    document.addEventListener("visibilitychange", hidden);
    return () => {
      removeEventListener("popstate", changed); removeEventListener("hashchange", changed); removeEventListener("keydown", globalKeys);
      systemDark.removeEventListener("change", applyTheme); document.removeEventListener("visibilitychange", hidden);
      for (const stop of unwatch) stop();
      offLink(); clearTimeout(noting); hub.stop();
    };
  });
</script>

<svelte:head><title>{labName}</title></svelte:head>

{#snippet icon(name: "home" | "tray" | "wave" | "table" | "book" | "tools" | "plus" | "search" | "side" | "down" | "right")}
  <svg class="i" viewBox="0 0 20 20" aria-hidden="true">
    {#if name === "home"}<path d="M3.5 9.2 10 3.8l6.5 5.4V16a1 1 0 0 1-1 1H12v-4.5H8V17H4.5a1 1 0 0 1-1-1z" />
    {:else if name === "tray"}<path d="M3 11.5h4l1 2h4l1-2h4M4.6 4.5h10.8L17 11.5v4a1 1 0 0 1-1 1H4a1 1 0 0 1-1-1v-4z" />
    {:else if name === "wave"}<path d="M6 7.5v5M10 4.5v11M14 7.5v5" />
    {:else if name === "table"}<rect x="3.5" y="4.5" width="13" height="11" rx="1.5" /><path d="M3.5 8.5h13M8.5 8.5v7" />
    {:else if name === "book"}<path d="M3.5 4.5h4.6A1.9 1.9 0 0 1 10 6.4V16a1.6 1.6 0 0 0-1.6-1.6H3.5zM16.5 4.5h-4.6A1.9 1.9 0 0 0 10 6.4V16a1.6 1.6 0 0 1 1.6-1.6h4.9z" />
    {:else if name === "tools"}<path d="M4 6h8M14.5 6H16M4 14h1.5M8 14h8" /><circle cx="13" cy="6" r="1.6" /><circle cx="6.8" cy="14" r="1.6" />
    {:else if name === "plus"}<path d="M10 4.5v11M4.5 10h11" />
    {:else if name === "search"}<circle cx="9" cy="9" r="5" /><path d="m13 13 3.5 3.5" />
    {:else if name === "side"}<rect x="2.6" y="3.6" width="14.8" height="12.8" rx="2.4" /><path d="M7.6 3.6v12.8M4.6 7h1.4M4.6 9.6h1.4" />
    {:else if name === "down"}<path d="m6 8 4 4 4-4" />
    {:else}<path d="m8 5.5 4.5 4.5L8 14.5" />{/if}
  </svg>
{/snippet}

<a class="skip" href="#main">Skip to content</a>
<header class="bar" class:bench={detailed}>
  <button class="icon-btn menu" onclick={() => (menuOpen = !menuOpen)} aria-expanded={menuOpen} aria-controls="sidebar"
    aria-label={menuOpen ? "Close the sidebar" : "Open the sidebar"} title={menuOpen ? "Close the sidebar" : "Open the sidebar"}>{@render icon("side")}</button>
  <div class="letterhead">
    {#if (identity?.groups.length ?? 0) > 1}
      <button class="lab" popovertarget="lab-menu" aria-label={`Change lab. You are in ${labName}.${waitingElsewhere ? ` ${waitingElsewhere === 1 ? "One item waits" : `${waitingElsewhere} items wait`} for you in your other labs.` : ""}`}>{labName}{#if waitingElsewhere}<span class="elsewhere" aria-hidden="true"></span>{/if}{@render icon("down")}</button>
    {:else}
      <a class="lab" href="#/" onclick={(e) => { e.preventDefault(); place({}); }}>{labName}</a>
    {/if}
    {#if organization}<span class="org">{organization}</span>{/if}
  </div>
  <div class="bar-end">
    {#if preview}<span class="preview" title="The files and records in this preview are simulated examples, not research data.">Preview with simulated data</span>{/if}
    {#if identity}
      <button class="new-question" onclick={newQuestion} aria-label="New question" title="New question">{@render icon("plus")}<span class="long">New question</span></button>
      <button class="search" onclick={() => { closeMenus(); searchOpen = true; }} aria-label="Search your research" aria-keyshortcuts={isMac ? "Meta+K" : "Control+K"}>
        {@render icon("search")}<span class="long">Search your research</span><span class="mid">Search</span><kbd>{isMac ? "⌘K" : "Ctrl K"}</kbd></button>
      <div class="views" role="group" aria-label="How much to show">
        <button aria-pressed={!detailed} onclick={toSimple}>Simple</button>
        <button aria-pressed={detailed} onclick={toDetailed}>Detailed</button>
      </div>
    {/if}
    <button class="round display" popovertarget="display-settings" aria-label="Display" title="Display">Aa</button>
    {#if identity}
      <button class="me" popovertarget="account-menu" aria-label={`Account: signed in as ${person({ display_name: identity.principal.display_name, email: identity.principal.email })}`}>{#if initials}{initials}{:else}<svg viewBox="0 0 20 20" aria-hidden="true"><circle cx="10" cy="7.5" r="3" /><path d="M4.5 16.5c.7-3 2.9-4.6 5.5-4.6s4.8 1.6 5.5 4.6" /></svg>{/if}</button>
      <div id="account-menu" class="bar-menu account-menu" popover>
        {#if identity.principal.display_name?.trim()}<p class="who">{identity.principal.display_name.trim()}</p>{/if}
        <p class="address">{identity.principal.email}</p>
        {#if here}<p class="role">{roleWord(here.role)} in {here.name}</p>{/if}
        {#if admin}<a class="admin-link" href="#/administration" aria-current={route.name === "administration" || inspection ? "page" : undefined}
          onclick={(e) => { e.preventDefault(); closeMenus(); go({ name: "administration" }); }}><span class="t">Inspection</span><span class="s">Open a project for a stated reason</span></a>{/if}
        <p class="runs">runs on enrichedlab</p>
      </div>
    {/if}
  </div>
</header>

{#snippet labChoice(group: Identity["groups"][number])}
  <li><button aria-current={group.key === lab.key ? "true" : undefined} onclick={() => chooseLab(group.key)}>
    <span class="t">{group.name}</span><span class="r">{roleWord(group.role)}{#if projectIndex.phase === "ready"}{" · "}{labCount(group.key)} {labCount(group.key) === 1 ? "project" : "projects"}{/if}{#if labWaiting.get(group.key)}{" · "}{labWaiting.get(group.key)} waiting for you{/if}</span>
  </button></li>
{/snippet}
<div id="lab-menu" class="lab-menu" popover>
  <p class="menu-label">Your labs</p>
  <ul>{#each researchLabs as group (group.key)}{@render labChoice(group)}{/each}</ul>
  {#if operatorLabs.length}
    <p class="menu-label operator">Operator</p>
    <ul>{#each operatorLabs as group (group.key)}{@render labChoice(group)}{/each}</ul>
  {/if}
</div>

<div id="display-settings" class="display-settings" popover>
  <fieldset>
    <legend>Theme</legend>
    <div class="segments">
      {#each [["system", "System"], ["light", "Light"], ["dark", "Dark"]] as [value, label] (value)}
        <label><input type="radio" name="theme" {value} checked={theme === value} onchange={() => setDisplay(value, textSize)} /><span>{label}</span></label>
      {/each}
    </div>
  </fieldset>
  <fieldset>
    <legend>Text size</legend>
    <div class="segments">
      {#each [["small", "Smaller"], ["standard", "Standard"], ["large", "Larger"], ["largest", "Largest"]] as [value, label] (value)}
        <label><input type="radio" name="text-size" {value} checked={textSize === value} onchange={() => setDisplay(theme, value)} /><span>{label}</span></label>
      {/each}
    </div>
  </fieldset>
  {#if notifySupported}
    <fieldset>
      <legend>Notify me when an agent waits for me</legend>
      <div class="segments">
        {#each [["off", "Off"], ["on", "On"]] as [value, label] (value)}
          <label><input type="radio" name="notify" {value} checked={notifyOn === (value === "on")} disabled={value === "on" && notifyBlocked} onchange={() => void setNotify(value === "on")} /><span>{label}</span></label>
        {/each}
      </div>
      <p class="faint small notify-hint">{notifyBlocked ? "This browser blocks notifications from the workspace. Allow them in the browser's site settings to turn this on."
        : "Only while this tab is in the background. A notice names no project."}</p>
    </fieldset>
  {/if}
  <p class="faint small">Saved in this browser only.</p>
</div>

<div class="frame" class:bench={detailed}>
  {#if menuOpen}<button class="scrim" aria-label="Close the sidebar" onclick={() => (menuOpen = false)}></button>{/if}
  <nav id="sidebar" class="side" class:open={menuOpen} aria-label="Places">
    {#if identity}
      <button class="button drawer-only side-new" onclick={newQuestion}>{@render icon("plus")}New question</button>
      <ul class="places">
        <li><a href="#/" aria-current={onIndex ? "page" : inProject ? "true" : undefined} onclick={(e) => { e.preventDefault(); place({}); }}>{@render icon("home")}<span class="t">Your research</span></a></li>
        <li><a href="#/waiting" aria-current={route.name === "waiting" ? "page" : undefined} onclick={(e) => { e.preventDefault(); go({ name: "waiting" }); }}
          aria-label={waitingCount ? `Waiting for you, ${waitingCount} new` : undefined}>{@render icon("tray")}<span class="t">Waiting for you</span>{#if waitingCount}<span class="n" aria-hidden="true">{waitingCount}</span>{/if}</a></li>
        <li><a href="#/working" aria-current={route.name === "working" ? "page" : undefined} onclick={(e) => { e.preventDefault(); go({ name: "working" }); }}
          aria-label={workingCount ? `Working now, ${workingCount}` : undefined}>{@render icon("wave")}<span class="t">Working now</span>{#if workingCount}<span class="n" aria-hidden="true">{workingCount}</span>{/if}</a></li>
        <li><a href="#/analyses" aria-current={route.name === "analyses" || (route.name === "analysis" && !route.project) ? "page" : undefined} onclick={(e) => { e.preventDefault(); go({ name: "analyses" }); }}>{@render icon("table")}<span class="t">Results</span></a></li>
        <li><a href="#/library" aria-current={route.name === "library" || route.name === "paper" ? "page" : undefined} onclick={(e) => { e.preventDefault(); go({ name: "library" }); }}>{@render icon("book")}<span class="t">Library</span></a>
          <ul class="sub">
            <li><a href="#/commons" aria-current={route.name === "commons" ? "page" : undefined} onclick={(e) => { e.preventDefault(); go({ name: "commons" }); }}><span class="t">Commons</span></a></li>
            <li><a href="#/notes" aria-current={route.name === "notes" ? "page" : undefined} onclick={(e) => { e.preventDefault(); go({ name: "notes" }); }}><span class="t">Shared notes</span></a></li>
          </ul>
        </li>
        <li><a href="#/tools" aria-current={route.name === "tools" ? "page" : undefined} onclick={(e) => { e.preventDefault(); go({ name: "tools" }); }}>{@render icon("tools")}<span class="t">Tools</span></a></li>
      </ul>
      {#if projectIndex.phase === "failed"}<p class="side-note">{projectIndex.failure}</p>{/if}
      <div class="drawer-only side-views" role="group" aria-label="How much to show">
        <button aria-pressed={!detailed} onclick={toSimple}>Simple</button>
        <button aria-pressed={detailed} onclick={toDetailed}>Detailed</button>
      </div>
      <p class="side-foot">{#if liveLost}<span class="live-lost" role="status">Reconnecting for live updates. Lists update every minute meanwhile.</span>{/if}runs on enrichedlab</p>
    {/if}
  </nav>

  <main id="main" tabindex="-1">
    {#if trail.length}
      <nav class="crumbs" aria-label="Breadcrumb">
        <ol>
          {#each trail as step, i (i)}
            <li>{#if step.route && i < trail.length - 1}<a href={href(step.route)} onclick={(e) => { e.preventDefault(); if (step.route?.name === "home") place({}); else if (step.route) go(step.route); }}>{step.label}</a>{@render icon("right")}{:else}<span aria-current="page">{step.label}</span>{/if}</li>
          {/each}
        </ol>
      </nav>
    {/if}
    {#if failure}
      <section class="state"><h1>The workspace is unavailable</h1><p>{failure}</p><button class="button" onclick={loadIdentity}>Try again</button></section>
    {:else if !identity}
      <section class="state" role="status"><span class="waveform" aria-hidden="true"><i></i><i></i><i></i></span> Signing you in…</section>
    {:else if route.name === "project"}
      {#key route.project}
        <ProjectPage projectId={route.project} {identity} section={route.section} {inspection}
          openTask={(task) => go({ name: "analysis", project: route.name === "project" ? route.project : undefined, task })}
          openDataset={(dataset) => { if (route.name === "project") go({ name: "dataset", project: route.project, dataset }); }}
          onEndInspection={endInspection} onInspection={(expires) => setInspection(route.name === "project" ? route.project : "", expires)}
          onRunIndividual={(p) => openComposer(p)} {notify} />
      {/key}
    {:else if route.name === "analysis"}
      {#key route.task}<AnalysisPage taskId={route.task} projectId={route.project} {identity} openProject={(id) => go({ name: "project", project: id })}
        openTask={(task, project) => go(project ? { name: "analysis", project, task } : { name: "analysis", task })} {notify} />{/key}
    {:else if route.name === "dataset"}
      {#key route.dataset}<DatasetPage projectId={route.project} datasetId={route.dataset} {identity}
        openProject={(id) => go({ name: "project", project: id })} openTask={(task, project) => go({ name: "analysis", project, task })} {notify} />{/key}
    {:else if route.name === "analyses"}
      <AnalysesPage {identity} openTask={(task, project) => go(project ? { name: "analysis", task, project } : { name: "analysis", task })} newAnalysis={() => openComposer(null)} />
    {:else if route.name === "workbench"}
      <WorkbenchPage {identity} projectId={route.project} {notify} openClassic={(hash) => go(parseRoute(hash))} />
    {:else if route.name === "notes"}
      <SharedNotesPage {identity} {notify} />
    {:else if route.name === "waiting"}
      {#key route.labs?.join(",") ?? ""}<WaitingPage {identity} {notify} labs={route.labs} />{/key}
    {:else if route.name === "working"}
      <WorkingPage {identity} openProject={(id) => go({ name: "project", project: id })} openWaiting={() => go({ name: "waiting" })} {notify} />
    {:else if route.name === "library"}
      <LibraryPage {identity} {notify} openPaper={(paper) => go({ name: "paper", paper })} />
    {:else if route.name === "commons"}
      <CommonsPage {notify} />
    {:else if route.name === "tools"}
      <ToolsPage {identity} />
    {:else if route.name === "paper"}
      {#key route.paper}<PaperPage paperId={route.paper} {identity} {notify} openLibrary={() => go({ name: "library" })} />{/key}
    {:else if route.name === "administration" && admin}
      <AdministrationPage openProject={(id) => go({ name: "project", project: id })} onInspected={(id, expires) => { inspections = { ...inspections, [id]: expires }; }} />
    {:else}
      <LabIndex {identity} waitingItems={acrossLabs(inboxItems, lab.key)} openProject={(id) => go({ name: "project", project: id })}
        openLibrary={() => go({ name: "library" })} openWaiting={(labs) => go(labs ? { name: "waiting", labs } : { name: "waiting" })} {notify} />
    {/if}
    {#if !detailed}<footer class="foot">Results show what ran and that its files are intact. Scientific review stays with your team.</footer>{/if}
  </main>
</div>

<div class="notices" aria-live="polite">
  {#each notices as notice (notice.id)}<p class:error={notice.error} role={notice.error ? "alert" : "status"}>{notice.text}</p>{/each}
</div>

<!-- The sheet and the search exist only while open, so their fields never sit hidden beside a page's own form. -->
{#if identity && askDraft.open}
  <AskSheet bind:open={askDraft.open} bind:question={askDraft.question} {identity} labKey={askDraft.lab || lab.key} onDone={asked} />
{/if}
{#if identity && searchOpen}
  <SearchPalette bind:open={searchOpen} entries={searchEntries} choose={searched} />
{/if}

<Dialog bind:open={composerOpen} title={draftContext ? `Run one method in “${draftContext.title}”` : "Run one method"} wide>
  {#if !draftContext}<label>Lab<select bind:value={draft.group}>{#each labOrder(identity?.groups ?? []) as group (group.key)}<option value={group.key}>{group.name}</option>{/each}</select></label>{/if}
  {#if offeredProfiles(identity, draft.group).length}
    <label>Method<select bind:value={draft.profile}>{#each offeredProfiles(identity, draft.group) as profile (profile.key)}<option value={profile.key}>{profile.name}</option>{/each}</select></label>
  {:else}
    <p class="notice">This lab has no method to run yet. A lab lead can ask the platform administrator to add one.</p>
  {/if}
  {#if identity?.profiles.find((p) => p.key === draft.profile)}<p class="small soft">{identity.profiles.find((p) => p.key === draft.profile)?.description}</p>{/if}
  <label>What should it do?<textarea bind:value={draft.objective} maxlength="8192" rows="5" placeholder="Describe the question or check you need"></textarea></label>
  <p class="small soft">Sending the same request twice cannot start it twice.</p>
  {#if dialogFailure}<p class="notice error" role="alert">{dialogFailure}</p>{/if}
  {#snippet footer()}
    <button class="link" onclick={() => (composerOpen = false)}>Cancel</button>
    <button class="button" onclick={submit} disabled={busy || !draft.objective.trim() || !draft.profile}>{busy ? "Requesting…" : "Run it"}</button>
  {/snippet}
</Dialog>

<style>
  .skip { position: fixed; z-index: 40; left: 16px; top: -60px; padding: 10px 14px; background: var(--fill); color: var(--on-fill); border-radius: var(--r-2); }
  .skip:focus { top: 12px; }
  .i { width: 20px; height: 20px; flex: none; fill: none; stroke: currentColor; stroke-width: 1.6; stroke-linecap: round; stroke-linejoin: round; }

  /* The bar: the lab's letterhead on the dark plane, closed by the inset gold rule (the reference application's bar). */
  .bar {
    position: sticky; top: 0; z-index: 20; display: flex; align-items: center; gap: 14px;
    height: var(--bar-h); padding: 0 16px 0 20px; background: var(--brand); color: var(--on-brand);
    box-shadow: inset 0 -2px 0 var(--aggie-gold);
  }
  .letterhead { display: grid; min-width: 0; line-height: 1.22; }
  .lab { display: inline-flex; align-items: center; gap: 4px; min-width: 0; max-width: 100%; padding: 0; font-size: calc(15.5px * var(--type-scale)); font-weight: 650;
    color: var(--on-brand); text-decoration: none; white-space: nowrap; overflow: hidden; text-overflow: ellipsis; border-radius: var(--r-1); }
  .lab .i { width: 16px; height: 16px; opacity: 0.8; }
  .lab .elsewhere { flex: none; width: 7px; height: 7px; margin-left: 3px; border-radius: 50%; background: var(--aggie-gold); }
  .lab:hover { text-decoration: underline; text-underline-offset: 3px; }
  .org { font-size: calc(11.5px * var(--type-scale)); color: var(--on-brand-2); white-space: nowrap; overflow: hidden; text-overflow: ellipsis; }
  .bar-end { margin-left: auto; display: flex; align-items: center; gap: 10px; min-width: 0; }
  .preview { font-size: var(--font-xs); color: var(--on-brand-2); white-space: nowrap; }
  .bar :focus-visible { outline-color: var(--aggie-gold); }
  .icon-btn, .round { display: grid; place-items: center; width: 34px; height: 34px; border-radius: var(--r-2); color: var(--on-brand); }
  .icon-btn:hover, .new-question:hover, .search:hover { background: var(--bar-track); }
  .menu { display: none; margin-left: -6px; }
  .new-question { display: inline-flex; align-items: center; gap: 6px; height: 32px; padding: 0 12px 0 9px; border-radius: var(--r-pill); color: var(--on-brand);
    font-size: var(--font-small); font-weight: 600; box-shadow: inset 0 0 0 1px var(--bar-line); white-space: nowrap; }
  .new-question .i, .search .i { width: 16px; height: 16px; }
  .search { display: inline-flex; align-items: center; gap: 8px; height: 32px; width: 236px; padding: 0 8px 0 10px; border-radius: var(--r-pill);
    background: var(--bar-track); color: var(--bar-ink-2); font-size: var(--font-small); white-space: nowrap; transition: background-color var(--t-fast) var(--ease); }
  .search .long { overflow: hidden; text-overflow: ellipsis; }
  .search .mid { display: none; }
  .search kbd { margin-left: auto; padding: 0 5px; border-radius: 5px; box-shadow: inset 0 0 0 1px var(--bar-line); font: 500 11px var(--ui); color: var(--bar-ink-2); }
  .views { display: inline-flex; padding: 2px; border-radius: var(--r-pill); background: var(--bar-track); }
  .views button { height: 28px; padding: 0 13px; border-radius: var(--r-pill); font-size: var(--font-small); font-weight: 600; color: var(--bar-ink-2); transition: background-color var(--t-fast) var(--ease), color var(--t-fast) var(--ease); }
  .views button:hover { color: var(--on-brand); }
  .views button[aria-pressed="true"] { background: var(--bar-seg-on); color: var(--bar-seg-on-ink); box-shadow: 0 1px 2px rgb(0 0 0 / 0.22); }
  .round { width: 32px; height: 32px; border-radius: 50%; box-shadow: inset 0 0 0 1px var(--bar-line); font-family: var(--serif); font-size: calc(14px * var(--type-scale)); font-weight: 600; }
  .round:hover { background: var(--bar-track); }
  .me { display: grid; place-items: center; width: 32px; height: 32px; border-radius: 50%; font-size: 12px; font-weight: 700; color: var(--aggie-blue); background: var(--aggie-gold); letter-spacing: 0.02em; }
  .me svg { width: 18px; height: 18px; fill: none; stroke: currentColor; stroke-width: 1.6; stroke-linecap: round; }

  /* Menus are raised panels above the page: 12 pixel corners, a hairline and a soft shadow, opening from 98 percent. */
  .lab-menu, .bar-menu, .display-settings { margin: 0; border: 0; border-radius: var(--r-3); background: var(--surface); color: var(--text); box-shadow: 0 0 0 1px var(--track-line), var(--overlay-shadow); }
  .lab-menu:popover-open, .bar-menu:popover-open, .display-settings:popover-open { animation: menu-in 140ms var(--ease); }
  @keyframes menu-in { from { opacity: 0; transform: translateY(-4px) scale(0.98); } }
  .lab-menu { position: fixed; inset: auto; top: calc(var(--bar-h) + 8px); left: 16px; width: min(340px, calc(100vw - 32px)); padding: 10px 6px 6px; }
  .lab-menu .menu-label { padding: 0 12px 6px; font-size: var(--font-xs); font-weight: 650; color: var(--text-3); }
  .lab-menu .menu-label.operator { margin-top: 8px; padding-top: 10px; border-top: 1px solid var(--line); }
  .lab-menu ul { list-style: none; margin: 0; padding: 0; display: grid; gap: 1px; }
  .lab-menu button { position: relative; display: grid; gap: 1px; width: 100%; padding: 9px 12px 9px 26px; border-radius: var(--r-2); text-align: left; transition: background-color var(--t-fast) var(--ease); }
  .lab-menu button:hover { background: var(--row-hover); }
  .lab-menu button[aria-current]::before { content: ""; position: absolute; left: 11px; top: 15px; width: 7px; height: 7px; border-radius: 50%; background: var(--mark); }
  .lab-menu .t { font-weight: 650; color: var(--heading); }
  .lab-menu .r { font-size: var(--font-xs); color: var(--text-3); }
  .bar-menu { position: fixed; inset: auto; top: calc(var(--bar-h) + 8px); min-width: 270px; max-width: calc(100vw - 32px); padding: 6px; }
  .account-menu { right: 16px; left: auto; min-width: 260px; padding: 14px 16px 12px; }
  .account-menu:popover-open { display: grid; gap: 2px; }
  .account-menu .who { font-weight: 700; color: var(--heading); }
  .account-menu .address { font-size: var(--font-small); color: var(--text-2); overflow-wrap: anywhere; }
  .account-menu .role { margin-top: 6px; font-size: var(--font-small); color: var(--text); }
  .account-menu .runs { margin-top: 10px; font-size: var(--font-xs); color: var(--text-3); letter-spacing: 0.01em; }
  .account-menu .admin-link { position: relative; display: grid; gap: 1px; margin: 10px -8px 0; padding: 9px 10px 9px 24px; border-radius: var(--r-2); border-top: 1px solid var(--line); text-decoration: none; color: var(--text); }
  .account-menu .admin-link:hover { background: var(--row-hover); }
  .account-menu .admin-link[aria-current]::before { content: ""; position: absolute; left: 9px; top: 16px; width: 7px; height: 7px; border-radius: 50%; background: var(--mark); }
  .account-menu .admin-link .t { font-weight: 650; color: var(--heading); }
  .account-menu .admin-link .s { font-size: var(--font-xs); color: var(--text-3); }

  .display-settings { position: fixed; inset: auto; top: calc(var(--bar-h) + 8px); right: 16px; width: min(340px, calc(100vw - 32px)); padding: 16px 18px; }
  .display-settings fieldset { border: 0; margin: 0 0 14px; padding: 0; }
  .display-settings legend { font-size: var(--font-small); font-weight: 650; color: var(--heading); margin-bottom: 8px; }
  .notify-hint { margin: 6px 0 0; }
  .segments { display: flex; gap: 2px; padding: 3px; border-radius: var(--r-pill); background: var(--track); box-shadow: inset 0 0 0 1px var(--track-line); }
  .segments label { flex: 1; display: block; font-weight: 600; color: var(--text-2); }
  .segments input { position: absolute; opacity: 0; width: 1px; height: 1px; }
  .segments span { display: grid; place-items: center; min-height: 32px; padding: 0 6px; border-radius: var(--r-pill); font-size: var(--font-xs); cursor: pointer; transition: color var(--t-fast) var(--ease), background-color var(--t-fast) var(--ease), box-shadow var(--t-fast) var(--ease); }
  .segments input:checked + span { position: relative; background: var(--raised); color: var(--heading); box-shadow: var(--raised-edge); }
  .segments input:checked + span::after { content: ""; position: absolute; left: 10px; right: 10px; bottom: 3px; height: 2px; border-radius: 2px; background: var(--mark); }
  .segments input:focus-visible + span { outline: 2px solid var(--focus); outline-offset: 1px; }

  /* The sidebar: an inset glass panel beside the page with the five places. The place in view is raised with the
     editor's mark at its edge. */
  .frame { display: grid; grid-template-columns: calc(var(--side-w) + 24px) minmax(0, 1fr); min-height: calc(100vh - var(--bar-h)); }
  .side {
    position: sticky; top: calc(var(--bar-h) + 12px); align-self: start; display: flex; flex-direction: column; gap: 2px;
    height: calc(100vh - var(--bar-h) - 24px); height: calc(100dvh - var(--bar-h) - 24px); margin: 12px; padding: 10px 8px;
    border-radius: var(--r-3); background: var(--side); box-shadow: var(--group-edge); overflow: auto; overscroll-behavior: contain;
  }
  .places, .sub { list-style: none; margin: 0; padding: 0; display: grid; gap: 2px; }
  .places a { position: relative; display: flex; align-items: center; gap: 10px; min-height: 38px; padding: 0 10px 0 12px; border-radius: 9px;
    font-size: calc(14.5px * var(--type-scale)); font-weight: 500; color: var(--text); text-decoration: none; transition: background-color var(--t-fast) var(--ease); }
  .places a .i { color: var(--text-2); }
  .places a:hover { background: var(--row-hover); }
  .places a[aria-current] { background: var(--raised); box-shadow: var(--raised-edge); font-weight: 650; color: var(--heading); }
  .places a[aria-current]::before { content: ""; position: absolute; left: 0; top: 10px; bottom: 10px; width: 3px; border-radius: 2px; background: var(--mark); }
  .places .t { min-width: 0; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
  .places .n { margin-left: auto; font-size: calc(12.5px * var(--type-scale)); font-weight: 600; color: var(--text-3); font-variant-numeric: tabular-nums; }
  .sub { margin: 2px 0 4px; }
  .sub a { min-height: 32px; padding-left: 42px; font-size: calc(13.5px * var(--type-scale)); color: var(--text-2); }
  .side-note { margin: 8px 12px; font-size: var(--font-xs); color: var(--text-3); }
  .side-foot { margin-top: auto; padding: 10px 12px 4px; font-size: var(--font-xs); color: var(--text-3); letter-spacing: 0.01em; display: grid; gap: 6px; }
  .live-lost { color: var(--text-2); line-height: 1.45; }
  .drawer-only { display: none; }
  .side-new { margin: 2px 4px 10px; }
  .side-new .i { width: 18px; height: 18px; }
  .side-views { margin: 14px 4px 0; padding: 3px; gap: 2px; border-radius: var(--r-pill); background: var(--track); box-shadow: inset 0 0 0 1px var(--track-line); }
  .side-views button { flex: 1; min-height: 34px; border-radius: var(--r-pill); font-size: var(--font-small); font-weight: 600; color: var(--text-2); }
  .side-views button[aria-pressed="true"] { position: relative; background: var(--raised); color: var(--heading); box-shadow: var(--raised-edge); }
  .side-views button[aria-pressed="true"]::after { content: ""; position: absolute; left: 14px; right: 14px; bottom: 3px; height: 2px; border-radius: 2px; background: var(--mark); }
  .scrim { display: none; }
  /* The Detailed view takes the full width; the sidebar opens over it from the sidebar button. */
  .bar.bench .menu { display: grid; }
  .frame.bench { grid-template-columns: minmax(0, 1fr); }
  .frame.bench .side { position: fixed; z-index: 25; top: var(--bar-h); left: 0; bottom: 0; height: auto; width: min(300px, 86vw); margin: 0; align-self: stretch;
    border-radius: 0 var(--r-3) var(--r-3) 0; background: var(--surface); box-shadow: var(--overlay-shadow);
    transform: translateX(-102%); visibility: hidden; transition: transform 220ms var(--ease), visibility 0s 220ms; }
  .frame.bench .side.open { transform: none; visibility: visible; transition: transform 220ms var(--ease); }
  .frame.bench .scrim { display: block; position: fixed; z-index: 24; inset: var(--bar-h) 0 0 0; background: var(--scrim); }

  main { min-width: 0; display: flex; flex-direction: column; }
  main:focus { outline: none; }
  main > :global(:not(.crumbs):not(.foot)) { flex: 1 0 auto; }
  .crumbs ol { list-style: none; margin: 0 auto; padding: 18px var(--gutter) 0; max-width: 1080px; display: flex; flex-wrap: wrap; align-items: center; gap: 4px 6px; font-size: var(--font-small); color: var(--text-3); }
  .crumbs li { display: flex; align-items: center; gap: 6px; min-width: 0; }
  .crumbs .i { width: 12px; height: 12px; }
  .crumbs a { color: var(--text-2); text-decoration: none; }
  .crumbs a:hover { color: var(--heading); text-decoration: underline; text-underline-offset: 3px; }
  .crumbs [aria-current] { color: var(--text-2); max-width: 60ch; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
  .state { padding: 72px var(--gutter); display: grid; gap: 14px; justify-items: start; max-width: 720px; }
  .state[role="status"] { display: flex; align-items: center; gap: 10px; color: var(--text-2); }
  .foot { width: calc(100% - 2 * var(--gutter)); max-width: calc(1080px - 2 * var(--gutter)); margin: 48px auto 0; padding: 16px 0 24px; border-top: 1px solid var(--line); font-size: var(--font-xs); color: var(--text-3); }

  .notices { position: fixed; z-index: 35; right: 16px; bottom: 16px; display: grid; gap: 8px; width: min(420px, calc(100vw - 32px)); }
  .notices p { padding: 12px 16px; border-radius: var(--r-2); background: var(--surface); color: var(--text); border: 1px solid var(--line); box-shadow: var(--overlay-shadow); font-size: var(--font-small); animation: in 180ms var(--ease); }
  .notices p.error { color: var(--failed); }
  @keyframes in { from { opacity: 0; transform: translateY(6px); } }

  /* Between the wide layout and a roomy desktop the bar keeps its controls and drops their words. */
  @media (max-width: 1279px) {
    .new-question { width: 34px; padding: 0; justify-content: center; }
    .new-question .long, .preview { display: none; }
    .search { width: 150px; }
    .search .long { display: none; }
    .search .mid { display: inline; }
  }
  /* Below the wide layout the sidebar becomes a drawer from the sidebar button, with New question at its top. */
  @media (max-width: 992px) {
    .bar { gap: 10px; padding-left: 14px; }
    .menu { display: grid; }
    .frame { grid-template-columns: minmax(0, 1fr); }
    .side { position: fixed; z-index: 25; top: var(--bar-h); left: 0; bottom: 0; height: auto; width: min(300px, 86vw); margin: 0; align-self: stretch;
      border-radius: 0 var(--r-3) var(--r-3) 0; background: var(--surface); box-shadow: var(--overlay-shadow);
      transform: translateX(-102%); visibility: hidden; transition: transform 220ms var(--ease), visibility 0s 220ms; }
    .side.open { transform: none; visibility: visible; transition: transform 220ms var(--ease); }
    .scrim { display: block; position: fixed; z-index: 24; inset: var(--bar-h) 0 0 0; background: var(--scrim); }
    .drawer-only { display: flex; }
    .new-question { display: none; }
    .search { width: 34px; padding: 0; justify-content: center; background: transparent; }
    .search .long, .search .mid, .search kbd { display: none; }
    .search .i { width: 19px; height: 19px; }
  }
  @media (max-width: 760px) { .views { display: none; } }
  @media (max-width: 560px) {
    .bar { gap: 6px; padding-right: 10px; }
    .bar-end { gap: 6px; }
    .lab { font-size: calc(15px * var(--type-scale)); }
  }
</style>
