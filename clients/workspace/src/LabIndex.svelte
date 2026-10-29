<script lang="ts">
  import { onMount, tick, untrack } from "svelte";
  import { flip } from "svelte/animate";
  import { askDraft } from "./ask_state.svelte.ts";
  import { labOrder, operatorLab } from "./context.ts";
  import type { Identity } from "./contracts.ts";
  import { itemAction, itemHeading, itemKey, itemLink, itemSentence, ordered, unseen, type InboxItem } from "./inbox.ts";
  import { lab } from "./lab.svelte.ts";
  import { combines, knownStarts, readStarts, startsSentence } from "./lab_starts.ts";
  import { when } from "./language.ts";
  import { live, mayBeOld } from "./live.ts";
  import { href } from "./route.ts";
  import { arrive, closeGap, leave } from "./motion.ts";
  import { factsFor, indexFilter, projectIndex, resetFilter, standingFor } from "./project_index.svelte.ts";
  import ProjectRow from "./ProjectRow.svelte";
  import WorkingRow from "./WorkingRow.svelte";
  import type { ProjectSummary, WorkspaceChoice } from "./projects.ts";
  import { standingNames, standingOrder } from "./standing.ts";
  import StatusMark from "./StatusMark.svelte";
  import { waiting } from "./waiting.svelte.ts";

  // Home (Glass Box): the question field leads, then what needs the reader, what is working and
  // what finished lately, then every project with its filters. `waitingItems` hold what waits in all the reader's labs
  // (inbox.ts); home lists those of the labs in view, so for the chosen lab home and the sidebar give one number.
  let { identity, waitingItems = [], openProject, openLibrary, openWaiting, notify }: {
    identity: Identity; waitingItems?: InboxItem[]; openProject: (id: string) => void;
    openLibrary?: () => void; openWaiting?: (labs?: string[]) => void; notify?: (text: string, error?: boolean) => void;
  } = $props();

  const SHOWN = 5;
  const RECENT_DAYS = 7;
  const me = $derived(identity.principal.id);
  const labName = (key: string) => identity.groups.find((g) => g.key === key)?.name ?? key;
  const mine = (p: ProjectSummary) => (p.owner?.principal_id ?? p.owner_id) === me;
  const text = (p: ProjectSummary) => `${p.title} ${factsFor(p).question ?? ""}`.toLowerCase();

  // The labs in view: the chosen lab unless the reader picks others in the filter (in one lab,
  // another lab's work never shows unasked). One lab in view: the page is that lab's, titled with its name.
  const labsInView = $derived(indexFilter.labs.length ? indexFilter.labs : lab.key ? [lab.key] : []);
  const single = $derived(labsInView.length === 1 ? identity.groups.find((g) => g.key === labsInView[0]) ?? null : null);
  // The filter offers every research lab, and an operator lab (qualification runs) only while it is chosen or picked.
  const shownLab = (key: string) => !operatorLab(key) || key === lab.key || indexFilter.labs.includes(key);
  const all = $derived(projectIndex.list.filter((p) => shownLab(p.group_key)));
  const inView = $derived(all.filter((p) => labsInView.includes(p.group_key)));

  // The ask field asks in the lab in view; its chip can choose another of the reader's labs for this question only.
  const askLabs = $derived(labOrder(identity.groups).filter((g) => !operatorLab(g.key) || g.key === lab.key));
  const askLab = $derived(askDraft.lab && askLabs.some((g) => g.key === askDraft.lab) ? askDraft.lab : single?.key ?? lab.key);
  let starts = $state<{ lab: string; items: WorkspaceChoice[] | null; failed: boolean }>({ lab: "", items: null, failed: false });
  $effect(() => {
    const key = askLab;
    if (!key) return;
    let current = true;
    starts = { lab: key, items: knownStarts(key), failed: false };
    readStarts(key).then((items) => { if (current) starts = { lab: key, items, failed: false }; })
      .catch(() => { if (current) starts = { lab: key, items: null, failed: true }; });
    return () => { current = false; };
  });
  const startsHere = $derived(starts.lab === askLab ? starts.items : null);
  const canAsk = $derived(!(startsHere && !startsHere.length && !starts.failed));
  let askField = $state<HTMLTextAreaElement>();
  let askCard = $state<HTMLElement>();
  function cont() {
    if (!askDraft.question.trim() || !canAsk) { askField?.focus(); return; }
    askDraft.lab = askLab; askDraft.open = true;
  }
  function askKeys(event: KeyboardEvent) {
    if (event.key === "Enter" && (event.metaKey || event.ctrlKey)) { event.preventDefault(); cont(); }
  }

  // What waits for the reader in the labs in view.
  const waitingHere = $derived(waitingItems.filter((i) => labsInView.includes(i.lab)));
  // Waiting for you opens on the same labs: the lab in view alone, or the labs the reader widened the index to.
  const waitingRoute = $derived(labsInView.length === 1 && labsInView[0] === lab.key ? { name: "waiting" as const } : { name: "waiting" as const, labs: labsInView });
  const needs = $derived(ordered(waitingHere).slice(0, SHOWN));
  const newHere = $derived(unseen(waitingHere));
  const severalLabs = $derived(new Set(waitingHere.map((i) => i.lab)).size > 1);
  const working = $derived(inView.filter((p) => standingFor(p) === "working").sort((a, b) => b.updated_at.localeCompare(a.updated_at)));
  const recent = (p: ProjectSummary) => Date.now() - Date.parse(p.updated_at) < RECENT_DAYS * 86_400_000;
  const finished = $derived(inView.filter((p) => standingFor(p) === "finished" && recent(p))
    .sort((a, b) => b.updated_at.localeCompare(a.updated_at)).slice(0, SHOWN));

  // A lab with nothing to list yet: the ask field still leads, and the page says what the lab can work from.
  const labEmpty = $derived(!!single && !projectIndex.list.some((p) => p.group_key === single.key));
  const welcoming = $derived(projectIndex.phase === "ready" && (!all.length || labEmpty));
  const welcomeLab = $derived(single ?? identity.groups.find((g) => g.key === lab.key) ?? null);

  // All projects: who started them is a segmented control; the lab and standing filters sit behind Filter.
  let filterButton = $state<HTMLButtonElement>();
  let filterPanel = $state<HTMLElement>();
  // Placed before it shows (beforetoggle), so it never paints at an old position, and again once its height is known.
  function placeFilter(event: ToggleEvent) {
    if (event.newState !== "open" || !filterButton || !filterPanel || matchMedia("(max-width: 640px)").matches) return;
    const r = filterButton.getBoundingClientRect();
    // The panel takes the side with more room and never more than that room, however short the window; it scrolls
    // inside when its filters do not fit.
    const below = innerHeight - r.bottom - 24, above = r.top - 24, height = filterPanel.scrollHeight || Math.max(below, above);
    const up = height > below && above > below;
    const room = Math.max(0, up ? above : below);
    filterPanel.style.maxHeight = `${Math.floor(room)}px`;
    filterPanel.style.top = `${Math.round(up ? r.top - 8 - Math.min(height, room) : r.bottom + 8)}px`;
    filterPanel.style.left = `${Math.round(Math.max(16, Math.min(r.right - 300, innerWidth - 316)))}px`;
  }
  type Facet = "labs" | "who" | "where" | "search";
  function passes(p: ProjectSummary, skip?: Facet) {
    if (skip !== "labs" && !labsInView.includes(p.group_key)) return false;
    if (skip !== "who" && indexFilter.who !== "anyone" && (indexFilter.who === "me") !== mine(p)) return false;
    if (skip !== "where" && indexFilter.where !== "any" && standingFor(p) !== indexFilter.where) return false;
    const q = indexFilter.search.trim().toLowerCase();
    return skip === "search" || !q || text(p).includes(q);
  }
  // The chosen lab leads; the others follow in their usual order.
  const groups = $derived(labOrder(identity.groups).filter((g) => shownLab(g.key))
    .sort((a, b) => Number(b.key === lab.key) - Number(a.key === lab.key)));
  const shown = $derived(all.filter((p) => passes(p)));
  // Each facet counts what the other filters leave, the way a library catalog does.
  const labCounts = $derived(new Map(groups.map((g) => [g.key, all.filter((p) => passes(p, "labs") && p.group_key === g.key).length])));
  const whoCounts = $derived({ anyone: all.filter((p) => passes(p, "who")).length, me: all.filter((p) => passes(p, "who") && mine(p)).length, colleagues: all.filter((p) => passes(p, "who") && !mine(p)).length });
  const whereCounts = $derived(new Map(standingOrder.map((s) => [s, all.filter((p) => passes(p, "where") && standingFor(p) === s).length])));
  const filtered = $derived(indexFilter.labs.length > 0 || indexFilter.who !== "anyone" || indexFilter.where !== "any" || !!indexFilter.search.trim());
  // While the reader narrows the list, the groups above it step aside so the list is what they see.
  const narrowed = $derived(indexFilter.who !== "anyone" || indexFilter.where !== "any" || !!indexFilter.search.trim());
  // The chosen lab alone is where the reader is, not a filter: the page names it in its title instead.
  const advanced = $derived((single?.key === lab.key ? 0 : indexFilter.labs.length) + (indexFilter.where !== "any" ? 1 : 0));
  const sections = $derived(groups
    .map((g) => ({ key: g.key, name: g.name, rows: shown.filter((p) => p.group_key === g.key) }))
    .filter((s) => s.rows.length));
  const whoNames = { anyone: "All", me: "Started by you", colleagues: "By colleagues" } as const;
  function toggleLab(key: string) {
    // Picking another lab widens the view from the labs in it; leaving none returns to the chosen lab.
    indexFilter.labs = labsInView.includes(key) ? labsInView.filter((k) => k !== key) : [...labsInView, key];
  }
  let allBlock = $state<HTMLElement>();
  function showAll(event: MouseEvent) {
    event.preventDefault();
    allBlock?.scrollIntoView({ behavior: matchMedia("(prefers-reduced-motion: reduce)").matches ? "auto" : "smooth", block: "start" });
  }

  // On a phone the ask field docks at the bottom once it scrolls away, so asking is reachable from anywhere on home.
  let docked = $state(false);
  onMount(() => {
    if (!askCard || typeof IntersectionObserver !== "function") return;
    const watch = new IntersectionObserver(([entry]) => { docked = !entry.isIntersecting && entry.boundingClientRect.top < 0; }, { threshold: 0 });
    watch.observe(askCard);
    return () => watch.disconnect();
  });
  function dockAsk() {
    if (askDraft.question.trim() && canAsk) { cont(); return; }
    askCard?.scrollIntoView({ behavior: matchMedia("(prefers-reduced-motion: reduce)").matches ? "auto" : "smooth", block: "center" });
    askField?.focus({ preventScroll: true });
  }
  // Home shows the workspace's copies of the lists, which every change reads again (live.ts). Without the change stream
  // a copy older than a few seconds is read again on arrival, so work finished elsewhere shows up.
  onMount(() => {
    const link = live().link, now = Date.now();
    if (projectIndex.phase === "ready" && mayBeOld(projectIndex.readAt, link, now)) void projectIndex.load();
    if (waiting.phase === "ready" && mayBeOld(waiting.readAt, link, now)) void waiting.load();
  });
  // Rows that arrive or leave while home is open move (motion.ts); the first answer arrives still, and a change of view
  // the reader makes (another lab, a filter) replaces the list at once: a new keyed block runs no row's transition.
  let settled = $state(false);
  $effect(() => { if (projectIndex.phase === "ready" && !settled) untrack(() => { void tick().then(() => { settled = true; }); }); });
  const viewKey = $derived(`${single?.key ?? ""}|${narrowed}`);
</script>

{#snippet chevron()}<svg class="chevron" viewBox="0 0 16 16" aria-hidden="true"><path d="M6 3.5 10.5 8 6 12.5" /></svg>{/snippet}

<section class="index" class:docked aria-labelledby="index-title">
  <header class="index-head">
    <h1 id="index-title">{single ? single.name : "Your research"}</h1>
    <p class="lede">{single && operatorLab(single.key) ? "The platform's own qualification runs. Research labs are in the lab menu."
      : single ? "Projects in this lab: the ones you started and the ones colleagues share with you."
      : `Projects in your labs: the ones you started and the ones colleagues share with you.`}</p>
  </header>

  <form class="ask" bind:this={askCard} onsubmit={(e) => { e.preventDefault(); cont(); }} aria-label="Ask a research question">
    <label class="ask-label" for="ask-question">Ask a research question</label>
    <textarea id="ask-question" bind:this={askField} bind:value={askDraft.question} onkeydown={askKeys} rows="2" maxlength="8192"
      placeholder="What do you want to find out? For example: which test diet raised plasma response the most, and by how much?"></textarea>
    <div class="ask-foot">
      {#if askLabs.length > 1}
        <label class="chip"><span class="visually-hidden">Lab for this question</span>
          <select value={askLab} onchange={(e) => (askDraft.lab = (e.currentTarget as HTMLSelectElement).value)}>
            {#each askLabs as g (g.key)}<option value={g.key}>In {g.name}</option>{/each}
          </select>
          <svg viewBox="0 0 12 12" aria-hidden="true"><path d="M3 4.5l3 3 3-3" /></svg></label>
      {:else}<span class="chip static">In {labName(askLab)}</span>{/if}
      <span class="can" aria-live="polite">{startsSentence(startsHere, labName(askLab), starts.failed, !!startsHere && combines(askLab))}</span>
      <button class="button" type="submit" disabled={!askDraft.question.trim() || !canAsk}>Continue</button>
    </div>
  </form>

  {#if projectIndex.phase === "loading" || projectIndex.phase === "idle"}<p class="soft" role="status"><span class="waveform" aria-hidden="true"><i></i><i></i><i></i></span> Opening your projects…</p>
  {:else if projectIndex.phase === "failed"}<p class="notice error" role="alert">{projectIndex.failure}</p>
  {:else}
    {#if welcoming && welcomeLab}
      <section class="welcome" aria-labelledby="welcome-title">
        <h2 id="welcome-title">{welcomeLab.name} is ready for its first project</h2>
        <p>A project holds one research question, the data and methods it may use, and everything the agent does on it.
          Nothing runs until you start it.</p>
        {#if startsHere?.length && askLab === welcomeLab.key}
          <h3>What this lab can work from</h3>
          <ul class="grouped starts">
            {#each startsHere as item (item.key)}
              <li class="grouped-row start"><span class="row-body"><span class="row-title">{item.name}</span><span class="start-text">{item.description}</span></span></li>
            {/each}
          </ul>
        {/if}
        {#if openLibrary}<button class="link" onclick={() => { if (welcomeLab && welcomeLab.key !== lab.key) lab.set(welcomeLab.key); openLibrary?.(); }}>Open the lab's library</button>{/if}
      </section>
    {/if}

    {#if needs.length && !narrowed}
      <section class="block" aria-labelledby="needs-title">
        <div class="block-head">
          <h2 id="needs-title">Needs you {#if newHere}<span class="count">{newHere}</span>{/if}</h2>
          {#if openWaiting}<a class="link small" href={href(waitingRoute)} onclick={(e) => { e.preventDefault(); openWaiting?.(waitingRoute.labs); }}>{waitingHere.length > SHOWN ? `All ${waitingHere.length} in Waiting for you` : "Waiting for you"}</a>{/if}
        </div>
        <!-- A change of view replaces the list at once; within one view, rows move as the data changes. -->
        {#key viewKey}<ul class="grouped">
          {#each needs as item (itemKey(item))}
            {@const to = itemLink(item)}
            {@const act = itemAction(item)}
            {@const heading = itemHeading(item, labName)}
            <li class="act-row" animate:flip={closeGap()} in:arrive={{ skip: !settled }} out:leave>
              <span class="row-body">
                <span class="row-title">{heading}</span>
                <span class="row-status"><StatusMark tone="attention" label="Waiting for you" /><span class="row-sentence">{itemSentence(item, labName, false)}</span></span>
                <span class="row-meta">{#if !item.seen_at}<span class="new">New</span>{/if}{#if severalLabs}<span>{labName(item.lab)}</span>{/if}<span>{when(item.at)}</span></span>
              </span>
              <a class={act.primary ? "button stretch" : "button secondary stretch"} href={to?.href ?? "#/waiting"} aria-label={`${act.label}: ${heading}`}>{act.label}</a>
            </li>
          {/each}
        </ul>{/key}
      </section>
    {/if}

    {#if working.length && !narrowed}
      <section class="block" aria-labelledby="working-title">
        <div class="block-head"><h2 id="working-title">Working now <span class="count">{working.length}</span></h2></div>
        <ul class="grouped">
          {#each working.slice(0, SHOWN) as p (p.project_id)}<WorkingRow project={p} {identity} {openProject} showLab={!single} {notify} />{/each}
        </ul>
      </section>
    {/if}

    {#if finished.length && !narrowed}
      <section class="block" aria-labelledby="finished-title">
        <div class="block-head">
          <h2 id="finished-title">Recently finished</h2>
          <a class="link small" href="#all-projects" onclick={showAll}>{single ? `All projects in ${single.name}` : "All projects"}</a>
        </div>
        <ul class="grouped">{#each finished as p (p.project_id)}<ProjectRow project={p} {identity} {openProject} showLab={!single} />{/each}</ul>
      </section>
    {/if}

    {#if all.length}
      <section class="block" id="all-projects" bind:this={allBlock} aria-labelledby="projects-title">
        <div class="block-head toolbar">
          <h2 id="projects-title">All projects</h2>
          <div class="segmented" role="radiogroup" aria-label="Started by">
            {#each ["anyone", "me", "colleagues"] as const as who (who)}
              <label><input type="radio" name="who" checked={indexFilter.who === who} onchange={() => (indexFilter.who = who)} />{whoNames[who]}<span class="count">{whoCounts[who]}</span></label>
            {/each}
          </div>
          <div class="tools-end">
            <label class="search-field"><span class="visually-hidden">Search projects</span><svg viewBox="0 0 16 16" aria-hidden="true"><circle cx="7" cy="7" r="4.6" /><path d="m10.4 10.4 3.4 3.4" /></svg>
              <input type="search" placeholder="Search projects" bind:value={indexFilter.search} /></label>
            <button class="button secondary filter-button" bind:this={filterButton} popovertarget="index-filter">Filter{#if advanced}<span class="filter-count">{advanced}</span>{/if}</button>
          </div>
        </div>

        <div id="index-filter" class="panel-popover filter-panel" popover bind:this={filterPanel} onbeforetoggle={placeFilter} ontoggle={placeFilter} role="dialog" aria-label="Filter projects">
          <fieldset>
            <legend>Lab</legend>
            {#each groups as g (g.key)}
              <label class="facet"><input type="checkbox" checked={labsInView.includes(g.key)} onchange={() => toggleLab(g.key)} /><span class="f-name">{g.name}</span><span class="count">{labCounts.get(g.key) ?? 0}</span></label>
            {/each}
          </fieldset>
          <fieldset>
            <legend>Where it stands</legend>
            <label class="facet"><input type="radio" name="where" checked={indexFilter.where === "any"} onchange={() => (indexFilter.where = "any")} /><span class="f-name">Any</span><span class="count">{all.filter((p) => passes(p, "where")).length}</span></label>
            {#each standingOrder as s (s)}
              <label class="facet"><input type="radio" name="where" checked={indexFilter.where === s} onchange={() => (indexFilter.where = s)} /><span class="f-name">{standingNames[s]}</span><span class="count">{whereCounts.get(s) ?? 0}</span></label>
            {/each}
          </fieldset>
          <div class="filter-foot">
            {#if filtered}<button class="link small" onclick={() => resetFilter()}>Clear the filters</button>{:else}<span></span>{/if}
            <button class="button secondary" popovertarget="index-filter" popovertargetaction="hide">Done</button>
          </div>
        </div>

        {#each sections as s (s.key)}
          <section class="lab" aria-labelledby={`lab-${s.key}`}>
            <h3 id={`lab-${s.key}`} class="grouped-title">{s.name} <span class="count">{s.rows.length}</span></h3>
            <ul class="grouped">{#each s.rows as p (p.project_id)}<ProjectRow project={p} {identity} {openProject} />{/each}</ul>
          </section>
        {/each}
        {#if !shown.length && !labEmpty}
          <div class="none">
            <p>No project matches these filters.</p>
            <button class="link" onclick={() => resetFilter()}>Clear the filters</button>
          </div>
        {/if}
      </section>
    {/if}
  {/if}
</section>

{#if docked}
  <div class="ask-dock">
    <button class="dock-field" onclick={dockAsk}>{askDraft.question.trim() || "Ask a question"}</button>
    <button class="button" onclick={dockAsk}>Ask</button>
  </div>
{/if}

<style>
  .index { padding: 36px var(--gutter) 24px; display: grid; gap: 30px; width: 100%; max-width: 900px; margin: 0 auto; align-content: start; }
  .index-head { display: grid; gap: 8px; }
  h1 { font-size: clamp(30px, calc(3vw * var(--type-scale)), calc(38px * var(--type-scale))); letter-spacing: -0.025em; line-height: 1.1; }
  .lede { max-width: 62ch; color: var(--text-2); font-size: calc(15px * var(--type-scale)); }

  /* The ask field: one raised sheet with the question in the paper's serif, the lab chip, what the agent can use and
     Continue. It is the page's one primary action. */
  .ask { display: grid; gap: 10px; margin-top: -8px; padding: 18px 18px 14px 20px; border-radius: var(--r-4); background: var(--raised); box-shadow: var(--group-edge), 0 14px 34px rgb(2 40 81 / 0.06); }
  :global([data-theme="dark"]) .ask { box-shadow: var(--group-edge); }
  .ask-label { font-size: var(--font-small); font-weight: 600; color: var(--text-2); }
  .ask textarea { min-height: 62px; max-height: 260px; field-sizing: content; padding: 0; border: 0; border-radius: 0; background: transparent; resize: none;
    font-family: var(--serif); font-size: calc(20px * var(--type-scale)); line-height: 1.42; color: var(--text); }
  .ask textarea:focus-visible { box-shadow: none; outline: none; }
  .ask:focus-within { box-shadow: var(--group-edge), 0 0 0 3px var(--focus-halo); }
  .ask-foot { display: flex; flex-wrap: wrap; align-items: center; gap: 10px 12px; }
  .chip { position: relative; display: inline-flex; align-items: center; font-weight: 600; color: var(--text); }
  .chip select { min-height: 32px; width: auto; padding: 0 28px 0 12px; border: 0; border-radius: var(--r-pill); background: var(--track); box-shadow: inset 0 0 0 1px var(--track-line); font-size: var(--font-small); font-weight: 600; appearance: none; cursor: pointer; }
  .chip svg { position: absolute; right: 10px; width: 12px; height: 12px; fill: none; stroke: var(--text-2); stroke-width: 1.6; stroke-linecap: round; stroke-linejoin: round; pointer-events: none; }
  .chip.static { min-height: 32px; padding: 0 12px; border-radius: var(--r-pill); background: var(--track); font-size: var(--font-small); }
  .can { flex: 1 1 260px; min-width: 0; font-size: var(--font-small); color: var(--text-2); }
  .ask-foot .button { margin-left: auto; }

  .block { display: grid; gap: 10px; min-width: 0; }
  .block-head { display: flex; align-items: baseline; justify-content: space-between; gap: 12px 16px; flex-wrap: wrap; margin: 0 4px; }
  .block-head h2 { font-size: calc(17px * var(--type-scale)); display: flex; align-items: baseline; gap: 10px; }
  .block-head .count { font-size: var(--font-small); }
  .toolbar { justify-content: flex-start; align-items: center; margin: 0; }
  .toolbar h2 { margin: 0 6px 0 4px; }
  .tools-end { display: flex; gap: 10px; align-items: center; margin-left: auto; min-width: 0; }
  .tools-end .search-field { width: min(280px, 100%); }
  .segmented .count { font-size: calc(11px * var(--type-scale)); }

  .filter-button { position: relative; min-height: 38px; padding: 0 14px; }
  .filter-count { display: inline-grid; place-items: center; min-width: 18px; height: 18px; padding: 0 5px; border-radius: var(--r-pill); background: var(--fill); color: var(--on-fill); font-size: 11px; font-weight: 700; font-variant-numeric: tabular-nums; }
  .filter-panel { position: fixed; inset: auto; width: 300px; padding: 14px 16px 12px; overflow: auto; overscroll-behavior: contain; }
  .filter-panel:popover-open { display: grid; gap: 16px; }
  .filter-panel fieldset { border: 0; margin: 0; padding: 0; display: grid; gap: 1px; }
  .filter-panel legend { font-size: var(--font-small); font-weight: 700; color: var(--heading); margin-bottom: 6px; }
  .facet { display: grid; grid-template-columns: 18px minmax(0, 1fr) auto; align-items: center; gap: 10px; padding: 6px 6px; margin: 0 -6px; border-radius: var(--r-2); font-weight: 400; color: var(--text); cursor: pointer; font-size: var(--font-small); }
  .facet:hover { background: var(--row-hover); }
  .facet input { margin: 0; accent-color: var(--fill); }
  .f-name { min-width: 0; overflow-wrap: anywhere; }
  .filter-foot { display: flex; justify-content: space-between; align-items: center; gap: 12px; padding-top: 10px; border-top: 1px solid var(--line); }
  .filter-foot .button { min-height: 34px; }
  .lab { display: grid; gap: 0; margin-top: 8px; }
  .none { display: grid; gap: 8px; justify-items: start; padding: 20px; border-radius: var(--r-3); box-shadow: var(--group-edge); }

  /* A new lab's welcome is the page itself, not a box around it: a heading, a sentence, what the lab holds. */
  .welcome { display: grid; gap: 12px; justify-items: start; }
  .welcome h2 { font-size: calc(19px * var(--type-scale)); letter-spacing: -0.012em; }
  .welcome h3 { margin-top: 4px; font-size: var(--font-small); font-weight: 650; color: var(--text-2); }
  .welcome > p { max-width: 62ch; color: var(--text-2); }
  .starts { width: 100%; }
  .start { padding: 14px 18px; cursor: default; }
  .start-text { font-size: var(--font-small); line-height: 1.5; color: var(--text-2); }

  .ask-dock { display: none; }
  @media (max-width: 860px) {
    .tools-end { margin-left: 0; width: 100%; }
    .tools-end .search-field { flex: 1; width: auto; }
  }
  @media (max-width: 640px) {
    .index { gap: 26px; padding-top: 24px; }
    .ask { padding: 16px; }
    .ask textarea { font-size: calc(18px * var(--type-scale)); min-height: 76px; }
    .can { flex-basis: 100%; order: 3; }
    .chip { flex: 1 1 0; min-width: 0; }
    .chip select { width: 100%; text-overflow: ellipsis; }
    .filter-panel { inset: auto 0 0 0; width: 100%; max-height: 80vh; overflow: auto; border-radius: var(--r-4) var(--r-4) 0 0; padding: 18px 20px calc(16px + env(safe-area-inset-bottom)); }
    .filter-panel:popover-open { animation: sheet-in 280ms var(--ease); }
    .index.docked { padding-bottom: 88px; }
    /* The dock: a compact ask field on glass above the home indicator. */
    .ask-dock { position: fixed; z-index: 15; left: 0; right: 0; bottom: 0; display: flex; gap: 10px; align-items: center;
      padding: 10px 16px calc(10px + env(safe-area-inset-bottom, 0px)); background: var(--glass); -webkit-backdrop-filter: saturate(1.6) blur(14px); backdrop-filter: saturate(1.6) blur(14px);
      border-top: 1px solid var(--line); animation: dock-in 220ms var(--ease); }
    .dock-field { flex: 1; min-width: 0; min-height: 44px; padding: 0 14px; border-radius: var(--r-pill); text-align: left; background: var(--track); box-shadow: inset 0 0 0 1px var(--track-line);
      font-family: var(--serif); font-size: calc(16px * var(--type-scale)); color: var(--text-3); white-space: nowrap; overflow: hidden; text-overflow: ellipsis; }
    .ask-dock .button { min-height: 44px; }
  }
  @keyframes sheet-in { from { transform: translateY(100%); } }
  @keyframes dock-in { from { transform: translateY(100%); } }
</style>
