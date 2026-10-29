<script lang="ts">
  import { untrack } from "svelte";
  import { BUILT_IN, kinds, type Registry } from "./kinds.ts";
  import { bytes } from "./language.ts";
  import { checkWords, planViews, type PlannedView, type ResultFile } from "./results.ts";
  import { DRAWABLE, VIEWS } from "./views/registry.ts";
  import type { FigureHandle, Location, ViewContext, ViewModule, ViewState } from "./views/types.ts";

  // One frame for every result (Glass Box, results rules 1 and 2). It names the file and its checksum and, in full
  // chrome, the step and method that made it; then the result's views, one switch apart when files hold the same numbers
  // in different forms. Each view is a module from the view registry: the frame mounts the chosen one,
  // keeps each view's state while another is shown, and carries a pick from one view to the others.
  let { subject, group = [], title, chrome = "embedded", showTitle = true, made = null, onExpand, figure = null }: {
    subject: ResultFile; group?: ResultFile[]; title: string; chrome?: "embedded" | "full"; showTitle?: boolean;
    /** The step and method that made the file, in words (full chrome). */
    made?: string | null;
    /** Opens the file larger (Quick Look) from an embedded frame. */
    onExpand?: () => void;
    /** The page's figure editor for a file it can draw from. */
    figure?: ((file: ResultFile) => FigureHandle | null) | null;
  } = $props();
  const uid = $props.id();

  let registry = $state<Registry>(BUILT_IN);
  void kinds().then((r) => { registry = r; });
  const plan = $derived(planViews(registry, subject, group, DRAWABLE));
  let chosen = $state("");
  const current = $derived(plan.views.find((v) => v.view === chosen) ?? plan.views.find((v) => v.view === plan.first) ?? plan.views[0]);
  // A new plan with the same view on the same file keeps the mounted view.
  const signature = $derived(`${current.view}|${current.file.file.path}|${current.file.file.sha256}|${current.kind.id}@${current.kind.version}|${JSON.stringify(current.encoding)}`);

  const states = new Map<string, ViewState>();
  let pick = $state<Location | null>(null);
  const handlers = new Set<(location: Location | null) => void>();
  function setPick(location: Location | null) {
    pick = location;
    for (const handler of handlers) handler(location);
  }

  function contextFor(planned: PlannedView): ViewContext {
    const still = plan.views.find((v) => v.view === "figure");
    return {
      file: planned.file.file, title, kind: planned.kind, encoding: planned.encoding,
      state: states.get(planned.view) ?? null,
      keep: (state) => { states.set(planned.view, state); },
      read: (signal) => planned.file.read(signal),
      page: planned.file.page, summary: planned.file.summary,
      pick: (location) => setPick(location ? { ...location, group: subject.group } : null),
      onPick: (handler) => { handlers.add(handler); handler(pick); return () => { handlers.delete(handler); }; },
      download: () => planned.file.download(),
      original: planned.original,
      still: still ? { file: still.file.file, read: (signal) => still.file.read(signal), download: () => still.file.download() } : null,
      figure: figure?.(planned.file) ?? null,
      theme: () => (document.documentElement.dataset.theme === "dark" ? "dark" : "light"),
      reducedMotion: () => matchMedia("(prefers-reduced-motion: reduce)").matches,
    };
  }

  let host = $state<HTMLElement>();
  // A view that takes more than a moment to load says so; a quick one appears without a passing line.
  let slow = $state(false);
  let failed = $state(false);
  $effect(() => {
    void signature;
    const el = host;
    if (!el) return;
    const planned = untrack(() => current);
    let live = true;
    let mounted: ViewModule | null = null;
    failed = false;
    const timer = setTimeout(() => { if (live) slow = true; }, 300);
    VIEWS[planned.view].load().then((module) => {
      if (!live) return;
      module.mount(el, contextFor(planned));
      mounted = module;
      clearTimeout(timer); slow = false;
    }, () => { if (live) { failed = true; clearTimeout(timer); slow = false; } });
    return () => {
      live = false;
      clearTimeout(timer); slow = false;
      const kept = mounted?.unmount();
      if (kept) states.set(planned.view, kept);
    };
  });

  function switchKeys(event: KeyboardEvent) {
    const ids = plan.views.map((v) => v.view), i = ids.indexOf(current.view);
    const next = event.key === "ArrowRight" ? ids[(i + 1) % ids.length] : event.key === "ArrowLeft" ? ids[(i - 1 + ids.length) % ids.length]
      : event.key === "Home" ? ids[0] : event.key === "End" ? ids.at(-1) : undefined;
    if (!next) return;
    event.preventDefault();
    chosen = next;
    document.getElementById(`${uid}-tab-${next}`)?.focus();
  }
  const siblings = $derived(group.filter((g) => g.file.path !== subject.file.path));
  const check = $derived(checkWords(current.file.check));
  // A table of the lab's own data: the research lead's digest of it lists its columns and
  // counts, never its rows.
  const labOwn = $derived(/^results\/lab_derived\//.test(current.file.file.path)
    ? "Computed from the lab's own data; the research lead sees its columns and counts, not its rows."
    : /^results\/lab_data\//.test(current.file.file.path)
      ? "The lab's own data, prepared as a person approved it; the research lead sees its columns and counts, not its rows." : "");
  const pickWords = (p: Location) => `${p.values?.length ? `${p.values.map(([name, value]) => `${name} ${value}`).join(", ")} · ` : ""}row ${(p.row + 1).toLocaleString()}${p.line ? `, line ${p.line.toLocaleString()}` : ""} of ${p.file}`;
</script>

<section class="result-frame" class:full={chrome === "full"} aria-labelledby={chrome === "full" ? `${uid}-title` : undefined}>
  {#if chrome === "full"}
    <header class="frame-head">
      <div class="names">
        <h2 id={`${uid}-title`}>{title}</h2>
        <p class="facts">{subject.file.path} · {bytes(subject.file.size_bytes)} · SHA-256 {subject.file.sha256.slice(0, 12)}…{subject.checksum ? ` · ${subject.checksum}` : ""}</p>
        {#if made}<p class="made">{made}</p>{/if}
      </div>
      <button class="button secondary compact" onclick={() => subject.download()}>Download</button>
    </header>
  {:else if showTitle || onExpand}
    <div class="frame-bar">
      {#if showTitle}<p class="name">{title}</p>{/if}
      {#if onExpand}<button class="link small expand" onclick={onExpand}>Open larger</button>{/if}
    </div>
  {/if}

  {#if plan.views.length > 1}
    <div class="segmented switch" role="tablist" aria-label={`Views of ${title}`} tabindex="-1" onkeydown={switchKeys}>
      {#each plan.views as v (v.view)}
        <button role="tab" id={`${uid}-tab-${v.view}`} aria-selected={v.view === current.view} aria-controls={`${uid}-panel`}
          tabindex={v.view === current.view ? 0 : -1} onclick={() => (chosen = v.view)}>{v.label}</button>
      {/each}
    </div>
  {/if}

  {#if pick}
    <p class="pick-caption" role="status"><span>Picked: {pickWords(pick)}</span> <button class="link small" onclick={() => setPick(null)}>Clear</button></p>
  {/if}
  {#if slow}<p class="soft small" role="status">Opening the {current.label === "3D" ? "3D" : current.label.toLowerCase()} view…</p>{/if}
  {#if failed}<p class="soft small" role="alert">This view could not be opened. The download is the complete original.</p>{/if}
  <div class="view-host" id={`${uid}-panel`} data-own-keys bind:this={host}
    role={plan.views.length > 1 ? "tabpanel" : undefined} aria-labelledby={plan.views.length > 1 ? `${uid}-tab-${current.view}` : undefined}></div>

  <details class="technical">
    <summary>File record</summary>
    <dl>
      <dt>Path</dt><dd>{current.file.file.path}</dd>
      <dt>Type</dt><dd>{current.file.file.media_type}</dd>
      <dt>Size</dt><dd>{current.file.file.size_bytes.toLocaleString()} bytes</dd>
      <dt>SHA-256</dt><dd>{current.file.file.sha256}</dd>
      <dt>Kind</dt><dd>{current.kind.id} version {current.kind.version}{current.file.result_kind ? ", as the broker recorded it" : ", from its media type"}</dd>
      {#if check}<dt>Check</dt><dd>{check}</dd>{/if}
      {#if labOwn}<dt>Lab data</dt><dd>{labOwn}</dd>{/if}
      {#if siblings.length}<dt>Same numbers in</dt><dd>{siblings.map((s) => s.file.path).join(", ")}</dd>{/if}
    </dl>
  </details>
</section>

<style>
  .result-frame { display: grid; gap: 12px; min-width: 0; }
  .frame-head { display: flex; flex-wrap: wrap; align-items: flex-start; justify-content: space-between; gap: 10px 20px; }
  .names { display: grid; gap: 4px; min-width: 0; flex: 1 1 36ch; }
  h2 { font-size: calc(20px * var(--type-scale)); line-height: 1.25; color: var(--heading); text-wrap: balance; }
  .facts { font-family: var(--mono); font-size: var(--font-xs); color: var(--text-2); overflow-wrap: anywhere; }
  .made { font-size: var(--font-small); color: var(--text-2); }
  .frame-bar { display: flex; flex-wrap: wrap; align-items: baseline; justify-content: space-between; gap: 4px 16px; }
  .name { font-family: var(--serif); font-size: calc(15px * var(--type-scale)); color: var(--heading); }
  .expand { min-height: 36px; margin-left: auto; }
  .switch { justify-self: start; }
  .switch:focus { outline: none; }
  .switch > button:focus-visible { outline: 2px solid var(--focus); outline-offset: 1px; }
  .pick-caption { display: flex; flex-wrap: wrap; align-items: baseline; gap: 4px 12px; font-size: var(--font-small); color: var(--text); font-variant-numeric: tabular-nums; }
  .pick-caption span { overflow-wrap: anywhere; }
  .pick-caption .link { min-height: 32px; }
  .view-host { min-width: 0; }
  .view-host:empty { display: none; }
</style>
