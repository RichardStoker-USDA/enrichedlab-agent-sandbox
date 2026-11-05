<script lang="ts">
  import { onDestroy, untrack } from "svelte";
  import type { Identity, LibraryLabel, LibraryLabelNamespace, LibraryPaper, LibraryPaperView, LibraryReading, LibraryTableGrid } from "./contracts.ts";
  import { currentLab } from "./lab.svelte.ts";
  import { day } from "./lab_notes.ts";
  import { acceptanceWords, addedWords, byline, checkedWords, labelProblem, library, libraryFailure, MAX_LABEL, MAX_REMOVE_REASON, namespaceName, namespaces, parseWords, roleWords } from "./library.ts";
  import { covers, live } from "./live.ts";
  import { href } from "./route.ts";
  import { pageTitle } from "./project_index.svelte.ts";

  // One paper of the lab's library: its catalog fields and what the parse found, its labels, whether a
  // lab curator accepted it for the research agent, and removing it. A label a service account
  // proposed reads as proposed until a person sets it.
  let { paperId, identity, notify, openLibrary }: {
    paperId: string;
    identity: Identity;
    notify: (message: string, error?: boolean) => void;
    openLibrary: () => void;
  } = $props();

  const group = $derived(currentLab(identity));
  let view = $state<LibraryPaperView | null>(null);
  let loading = $state(true);
  let failure = $state("");
  let controller: AbortController | null = null;
  async function load(quiet = false) {
    const key = group?.key;
    if (!key) { loading = false; return; }
    controller?.abort();
    const c = new AbortController();
    controller = c;
    loading = true; if (!quiet) failure = "";
    try {
      const next = await library.paper(key, paperId, c.signal);
      if (!c.signal.aborted) { view = next; failure = ""; }
    } catch (cause) {
      if (!c.signal.aborted && !(quiet && view)) failure = libraryFailure(cause, "load");
    } finally {
      if (!c.signal.aborted) loading = false;
    }
  }
  $effect(() => { void group?.key; void paperId; untrack(() => { view = null; void load(); }); });
  // A change to the lab's records reads the page again in place (live.ts); a failed quiet read keeps what it shows.
  const unwatch = live().watch(["lab:*"], (changed) => { if (group && covers(changed, `lab:${group.key}`)) return load(true); }, { gapMs: 2_000 });
  onDestroy(() => { controller?.abort(); unwatch(); });

  const paper = $derived(view?.paper ?? null);
  $effect(() => { pageTitle.value = paper?.title ?? (paper ? "Untitled paper" : ""); });
  function replace(next: LibraryPaper) { if (view) view = { ...view, paper: next }; }

  // The whole paper from its parse, read once per paper after the catalog arrives so the header shows first.
  let reading = $state<LibraryReading | null>(null);
  let readingPhase = $state<"idle" | "loading" | "ready" | "failed">("idle");
  let readingFailure = $state("");
  let readController: AbortController | null = null;
  let readFor = "";
  async function loadReading() {
    const key = group?.key;
    if (!key) return;
    readController?.abort();
    const c = new AbortController();
    readController = c;
    readingPhase = "loading"; readingFailure = "";
    try {
      const next = await library.read(key, paperId, c.signal);
      if (!c.signal.aborted) { reading = next; readingPhase = "ready"; }
    } catch (cause) {
      if (!c.signal.aborted) { readingPhase = "failed"; readingFailure = libraryFailure(cause, "load"); }
    }
  }
  $effect(() => {
    const p = paper;
    // A removed paper shows none of its text: drop what was read, stop a read in flight, and read again if restored.
    if (p?.removed) {
      untrack(() => { readController?.abort(); readController = null; reading = null; grids = {}; readingPhase = "idle"; readFor = ""; });
      return;
    }
    if (p && readFor !== paperId) { readFor = paperId; untrack(() => { reading = null; grids = {}; void loadReading(); }); }
  });
  onDestroy(() => readController?.abort());
  // Sections with text, without a second abstract when the catalog already shows one.
  const sections = $derived((reading?.sections ?? []).filter((s) => s.paragraphs.length && !(s.role === "abstract" && view?.outline?.abstract)));
  const contents = $derived([
    ...(view?.outline?.abstract ? [{ id: "paper-abstract", label: "Abstract", depth: 1 }] : []),
    ...sections.map((s) => ({ id: `section-${s.key}`, label: s.path.at(-1) ?? roleWords(s.role) ?? "Opening", depth: Math.min(Math.max(s.path.length, 1), 3) })),
    ...(reading?.figures.length ? [{ id: "paper-figures", label: `Figures (${reading.figures_total})`, depth: 1 }] : []),
    ...(reading?.tables.length ? [{ id: "paper-tables", label: `Tables (${reading.tables_total})`, depth: 1 }] : []),
  ]);
  const reduced = typeof matchMedia === "function" && matchMedia("(prefers-reduced-motion: reduce)").matches;
  function goTo(id: string) {
    const target = document.getElementById(id);
    if (!target) return;
    target.scrollIntoView({ block: "start", behavior: reduced ? "auto" : "smooth" });
    target.focus({ preventScroll: true });
  }

  // Tables open one at a time on request; a failed read can be tried again.
  let grids = $state<Record<number, LibraryTableGrid | "loading" | "failed">>({});
  async function showTable(ordinal: number) {
    const key = group?.key;
    if (!key || grids[ordinal] === "loading") return;
    grids[ordinal] = "loading";
    const forPaper = readFor;
    try {
      const grid = await library.table(key, paperId, ordinal);
      if (readFor === forPaper && !paper?.removed) grids[ordinal] = grid;
    } catch { if (readFor === forPaper) grids[ordinal] = "failed"; }
  }

  // Labels: one request key per distinct change, so a repeated click saves once.
  let namespace = $state<LibraryLabelNamespace>("topic");
  let value = $state("");
  let labelBusy = $state(false);
  let labelFailure = $state("");
  const keys = new Map<string, string>();
  const keyFor = (change: string) => { let k = keys.get(change); if (!k) { k = crypto.randomUUID(); keys.set(change, k); } return k; };
  async function changeLabel(kind: "added" | "removed", ns: LibraryLabelNamespace, text: string) {
    const key = group?.key;
    if (!key || !paper || labelBusy) return;
    const problem = labelProblem(text);
    if (problem) { labelFailure = problem; return; }
    labelBusy = true; labelFailure = "";
    try {
      const change = `${kind}:${ns}:${text.trim()}`;
      replace(await library.label(key, paper.paper_id, { kind, namespace: ns, value: text.trim(), request_id: keyFor(change) }));
      keys.delete(change);
      if (kind === "added") value = "";
    } catch (cause) {
      labelFailure = libraryFailure(cause, "label");
    } finally { labelBusy = false; }
  }
  function addLabel(event: SubmitEvent) { event.preventDefault(); void changeLabel("added", namespace, value); }
  const setBy = (label: LibraryLabel) => label.state === "proposed" ? "Proposed by a service account." : `Set on ${day(label.since)}.`;

  // Acceptance for the research agent: a lead or reviewer accepts it, or withdraws that with a reason.
  let withdrawing = $state(false);
  let withdrawReason = $state("");
  let acceptBusy = $state(false);
  let acceptFailure = $state("");
  let acceptKey = "", acceptBody = "";
  async function changeAcceptance(accept: boolean, event?: SubmitEvent) {
    event?.preventDefault();
    const key = group?.key;
    if (!key || !paper || acceptBusy) return;
    const clean = accept ? "" : withdrawReason.trim();
    if (!accept && !clean) { acceptFailure = "Say why the acceptance is withdrawn."; return; }
    if (clean.length > MAX_REMOVE_REASON) { acceptFailure = `Keep it to ${MAX_REMOVE_REASON} characters or fewer.`; return; }
    const body = `${accept}:${clean}`;
    if (body !== acceptBody) { acceptBody = body; acceptKey = crypto.randomUUID(); }
    acceptBusy = true; acceptFailure = "";
    try {
      replace(await library.accept(key, paper.paper_id, accept ? { accept, request_id: acceptKey } : { accept, reason: clean, request_id: acceptKey }));
      withdrawing = false; withdrawReason = ""; acceptBody = "";
      notify(accept ? "Accepted for the research agent." : "The acceptance is withdrawn.");
    } catch (cause) {
      acceptFailure = libraryFailure(cause, "accept");
    } finally { acceptBusy = false; }
  }

  // Removal, with a reason.
  let removing = $state(false);
  let reason = $state("");
  let removeBusy = $state(false);
  let removeFailure = $state("");
  let removeKey = "", removeBody = "";
  async function remove(event: SubmitEvent) {
    event.preventDefault();
    const key = group?.key;
    if (!key || !paper || removeBusy) return;
    const clean = reason.trim();
    if (!clean) { removeFailure = "Say why it should be removed."; return; }
    if (clean.length > MAX_REMOVE_REASON) { removeFailure = `Keep it to ${MAX_REMOVE_REASON} characters or fewer.`; return; }
    if (clean !== removeBody) { removeBody = clean; removeKey = crypto.randomUUID(); }
    removeBusy = true; removeFailure = "";
    try {
      replace(await library.remove(key, paper.paper_id, { reason: clean, request_id: removeKey }));
      removing = false; removeBody = "";
      notify("The paper is removed from the library. Adding the same file again restores it.");
    } catch (cause) {
      removeFailure = libraryFailure(cause, "remove");
    } finally { removeBusy = false; }
  }
</script>

<article class="paper-page" aria-labelledby="paper-title">
  <p class="back"><a class="link small" href={href({ name: "library" })} onclick={(e) => { e.preventDefault(); openLibrary(); }}>Library</a></p>
  {#if !group}<p class="soft">Your account is not in a lab yet.</p>
  {:else if loading && !view}<p class="soft" role="status">Opening the paper…</p>
  {:else if failure}<p class="notice error" role="alert">{failure}</p>
  {:else if paper && view}
    {@const facts = parseWords(paper)}
    {@const checked = checkedWords(paper)}
    <header>
      <h1 id="paper-title">{paper.title ?? "Untitled paper"}</h1>
      {#if byline(paper)}<p class="byline">{byline(paper)}</p>{/if}
      <p class="small soft">
        {#if view.outline?.journal}{`${view.outline.journal}. `}{/if}{#if paper.doi}<a class="link" href={`https://doi.org/${encodeURI(paper.doi)}`} rel="noopener noreferrer" target="_blank">doi:{paper.doi}</a>{". "}{/if}{addedWords(paper, day)}{facts ? ` ${facts}.` : ""}
      </p>
      {#if checked}<p class="small soft">{checked}</p>{/if}
      {#if paper.removed}
        <p class="notice" role="status">Removed from the library on {day(paper.removed.removed_at)}{paper.removed.by ? ` by ${paper.removed.by}` : ""}: {paper.removed.reason} Adding the same file again restores it.</p>
      {/if}
    </header>

    {#if view.outline_error === "library_unavailable" && readingPhase !== "ready"}
      <p class="notice error" role="alert">The paper's text could not be read right now. The paper itself is unchanged.</p>
    {/if}
    <div class="reader" class:with-contents={contents.length > 2}>
      {#if contents.length > 2}
        <nav class="contents" aria-label="Contents of the paper">
          <p class="contents-title">Contents</p>
          <ol>
            {#each contents as entry (entry.id)}
              <li class={`depth-${entry.depth}`}><button class="contents-link" onclick={() => goTo(entry.id)}>{entry.label}</button></li>
            {/each}
          </ol>
        </nav>
      {/if}
      <div class="reading">
        {#if view.outline?.abstract}
          <section id="paper-abstract" tabindex="-1" aria-labelledby="abstract-title">
            <h2 id="abstract-title" class="section-title">Abstract</h2>
            <p class="abstract">{view.outline.abstract}</p>
          </section>
        {/if}
        {#if readingPhase === "loading"}
          <p class="soft" role="status">Reading the paper's sections, figures and tables…</p>
        {:else if readingPhase === "failed"}
          <p class="notice error" role="alert">{readingFailure} <button class="link small" onclick={() => loadReading()}>Try again</button></p>
        {/if}
        {#each sections as section (section.key)}
          <section id={`section-${section.key}`} class="paper-section" tabindex="-1" aria-label={section.heading}>
            {#if section.path.length > 1}<p class="section-trail">{section.path.slice(0, -1).join(" › ")}</p>{/if}
            {#if section.path.length <= 1}
              <h2 class="section-title">{section.path.at(-1) ?? roleWords(section.role) ?? "Opening"}</h2>
            {:else}
              <h3 class="section-title minor">{section.path.at(-1)}</h3>
            {/if}
            {#each section.paragraphs as paragraph, i (i)}<p class="body-text">{paragraph}</p>{/each}
            <p class="section-meta">{#if section.pages}{section.pages.includes("-") ? `Pages ${section.pages}` : `Page ${section.pages}`}{/if}{#if section.cut_paragraphs}{section.pages ? ". " : ""}{section.cut_paragraphs === 1 ? "One long paragraph is shortened here" : `${section.cut_paragraphs} long paragraphs are shortened here`}; the PDF holds the full text.{/if}</p>
          </section>
        {/each}
        {#if reading && !reading.complete}
          <p class="small soft">This paper is long: the first {reading.blocks_read} of its {reading.blocks_total} text blocks are shown{reading.figures.length < reading.figures_total ? `, and ${reading.figures.length} of ${reading.figures_total} figures` : ""}.</p>
        {/if}

        {#if reading?.figures.length}
          <section id="paper-figures" tabindex="-1" aria-labelledby="figures-title">
            <h2 id="figures-title" class="group-title">Figures ({reading.figures_total})</h2>
            <div class="figure-list">
              {#each reading.figures as figure, i (figure.id ?? i)}
                <div class="figure-item"><figure class="paper-figure">
                  {#if figure.image && figure.id && group}
                    <a class="plate" href={library.figureImage(group.key, paperId, figure.id)} target="_blank" rel="noopener" style:aspect-ratio={figure.width && figure.height ? `${figure.width} / ${figure.height}` : null} style:max-width={figure.width ? `${figure.width}px` : null}>
                      <img src={library.figureImage(group.key, paperId, figure.id)} alt={`${figure.label ?? `Figure ${i + 1}`}${figure.caption ? `: ${figure.caption.slice(0, 160)}` : ""}`} loading="lazy" decoding="async" width={figure.width ?? undefined} height={figure.height ?? undefined} />
                    </a>
                  {:else}
                    <p class="plate missing">No image was saved for this figure when the paper was read. The PDF has it{figure.page ? ` on page ${figure.page}` : ""}.</p>
                  {/if}
                  <figcaption>
                    <span class="figure-name">{figure.label ?? `Figure ${i + 1}`}{figure.page ? `, page ${figure.page}` : ""}</span>
                    {#if figure.caption}<span class="caption">{figure.caption}</span>{/if}
                  </figcaption>
                </figure>
                  {#if figure.description}
                    <details class="derived">
                      <summary>Description written by a model</summary>
                      <p>{figure.description}</p>
                      <p class="small soft">Made when the paper was read, not by the authors. Check it against the figure before relying on it.</p>
                    </details>
                  {/if}
                </div>
              {/each}
            </div>
          </section>
        {:else if readingPhase !== "ready" && view.outline?.figures.length}
          <section aria-labelledby="figures-title">
            <h2 id="figures-title" class="group-title">{view.outline.figures_total ? `Figures (${view.outline.figures_total})` : "Figures"}</h2>
            <ol class="figures">
              {#each view.outline.figures as figure, i (i)}
                <li><span class="figure-name">{figure.label ?? `Figure ${i + 1}`}{figure.page ? `, page ${figure.page}` : ""}</span>{#if figure.caption}<span class="caption">{figure.caption}</span>{/if}</li>
              {/each}
            </ol>
          </section>
        {/if}

        {#if reading?.tables.length}
          <section id="paper-tables" tabindex="-1" aria-labelledby="tables-title">
            <h2 id="tables-title" class="group-title">Tables ({reading.tables_total})</h2>
            <ol class="table-list">
              {#each reading.tables as table, i (table.ordinal)}
                {@const grid = grids[table.ordinal]}
                <li>
                  <p class="table-head"><span class="figure-name">{table.label ?? `Table ${i + 1}`}{table.page ? `, page ${table.page}` : ""}</span>{#if table.caption}<span class="caption">{table.caption}</span>{/if}</p>
                  {#if grid && typeof grid === "object"}
                    <!-- A scrollable table must take keyboard focus so it can be scrolled without a mouse. -->
                    <!-- svelte-ignore a11y_no_noninteractive_tabindex -->
                    <div class="grid-scroll" role="region" aria-label={table.label ?? `Table ${i + 1}`} tabindex="0">
                      <table>
                        {#if grid.header.length}<thead><tr>{#each grid.header as cell, c (c)}<th scope="col">{cell ?? ""}</th>{/each}</tr></thead>{/if}
                        <tbody>{#each grid.rows as row, r (r)}<tr>{#each row as cell, c (c)}<td>{cell ?? ""}</td>{/each}</tr>{/each}</tbody>
                      </table>
                    </div>
                    {#if grid.rows_total > grid.rows.length}<p class="small soft">The first {grid.rows.length} of {grid.rows_total} rows are shown.</p>{/if}
                    <p class="small soft">Read from the PDF by the parser; check numbers against the paper before using them.</p>
                  {:else}
                    <button class="link small" disabled={grid === "loading"} onclick={() => showTable(table.ordinal)}>{grid === "loading" ? "Opening the table…" : grid === "failed" ? "The table could not be read. Try again" : "Show the table"}</button>
                  {/if}
                </li>
              {/each}
            </ol>
          </section>
        {/if}
      </div>
    </div>

    <section aria-labelledby="labels-title">
      <h2 id="labels-title" class="group-title">Labels</h2>
      {#if !paper.labels.length}<p class="soft small">No label yet. Labels say what the paper is about so the lab can find it.</p>
      {:else}
        <ul class="labels">
          {#each paper.labels as label (`${label.namespace}:${label.value}`)}
            <li>
              <span class="ns">{namespaceName(label.namespace)}</span> <span class="value" class:proposed={label.state === "proposed"}>{label.value}</span>
              <span class="small soft">{setBy(label)}</span>
              {#if !paper.removed}
                {#if label.state === "proposed"}<button class="link small" disabled={labelBusy} onclick={() => changeLabel("added", label.namespace, label.value)}>Set it</button>{/if}
                <button class="link small" disabled={labelBusy} onclick={() => changeLabel("removed", label.namespace, label.value)}>Remove</button>
              {/if}
            </li>
          {/each}
        </ul>
      {/if}
      {#if !paper.removed}
        <form class="label-form" onsubmit={addLabel} aria-label="Add a label">
          <label>Kind
            <select bind:value={namespace}>{#each namespaces as ns (ns.key)}<option value={ns.key}>{ns.name}</option>{/each}</select>
          </label>
          <label>Label
            <input bind:value={value} maxlength={MAX_LABEL} placeholder="For example: tooth length" />
          </label>
          <button class="button secondary" type="submit" disabled={labelBusy}>{labelBusy ? "Saving…" : "Add the label"}</button>
        </form>
      {/if}
      {#if labelFailure}<p class="notice error" role="alert">{labelFailure}</p>{/if}
    </section>

    {#if !paper.removed}
      <section aria-labelledby="acceptance-title">
        <h2 id="acceptance-title" class="group-title">For the research agent</h2>
        <div class="acceptance">
          <p>{acceptanceWords(paper, day)}</p>
          <p class="small soft">A lead or reviewer of the lab accepts a paper after checking it. In a lab where the research agent reads papers, it reads accepted ones as background and cites the passages that shaped a step.</p>
          {#if paper.can_accept}
            {#if !paper.acceptance}
              <button class="button secondary" disabled={acceptBusy} onclick={() => changeAcceptance(true)}>{acceptBusy ? "Saving…" : "Accept for the research agent"}</button>
            {:else if !withdrawing}
              <button class="link small" onclick={() => { withdrawing = true; withdrawReason = ""; acceptFailure = ""; }}>Withdraw the acceptance…</button>
            {:else}
              <form class="inline" onsubmit={(e) => changeAcceptance(false, e)} aria-label="Withdraw the acceptance">
                <label>Why withdraw it
                  <textarea bind:value={withdrawReason} rows="2" maxlength={MAX_REMOVE_REASON} placeholder="For example: A corrected version replaces it."></textarea>
                </label>
                <div class="row">
                  <button class="button" type="submit" disabled={acceptBusy}>{acceptBusy ? "Saving…" : "Withdraw the acceptance"}</button>
                  <button class="link" type="button" onclick={() => (withdrawing = false)}>Cancel</button>
                </div>
              </form>
            {/if}
          {/if}
          {#if acceptFailure}<p class="notice error" role="alert">{acceptFailure}</p>{/if}
        </div>
      </section>
    {/if}

    {#if paper.can_remove && !paper.removed}
      <section class="remove" aria-labelledby="remove-title">
        <h2 id="remove-title" class="group-title">Remove from the library</h2>
        {#if !removing}
          <p class="small soft">The paper leaves the list and search; the record of who added and removed it stays.</p>
          <button class="link small" onclick={() => { removing = true; reason = ""; removeFailure = ""; }}>Remove this paper…</button>
        {:else}
          <form class="inline" onsubmit={remove} aria-label="Remove this paper">
            <label>Why remove it
              <textarea bind:value={reason} rows="2" maxlength={MAX_REMOVE_REASON} placeholder="For example: Added to the wrong lab."></textarea>
            </label>
            <div class="row">
              <button class="button" type="submit" disabled={removeBusy}>{removeBusy ? "Removing…" : "Remove the paper"}</button>
              <button class="link" type="button" onclick={() => (removing = false)}>Cancel</button>
            </div>
          </form>
        {/if}
        {#if removeFailure}<p class="notice error" role="alert">{removeFailure}</p>{/if}
      </section>
    {/if}
  {/if}
</article>

<style>
  .paper-page { padding: 32px var(--gutter) 24px; display: grid; gap: 24px; width: 100%; max-width: 1080px; margin: 0 auto; align-content: start; }
  header { display: grid; gap: 8px; }
  h1 { font-family: var(--serif); font-size: clamp(26px, calc(3vw * var(--type-scale)), calc(36px * var(--type-scale))); line-height: 1.2; letter-spacing: -0.012em; max-width: 30em; overflow-wrap: anywhere; }
  .byline { color: var(--text-2); max-width: 80ch; }
  .group-title { font-size: calc(17px * var(--type-scale)); font-weight: 700; color: var(--heading); padding-bottom: 6px; border-bottom: 1px solid var(--line); max-width: 820px; margin-bottom: 10px; }
  .abstract { font-family: var(--serif); font-size: calc(16px * var(--type-scale)); line-height: 1.65; max-width: 72ch; }
  .figures { margin: 0; padding: 0; list-style: none; display: grid; gap: 12px; max-width: 820px; }
  .figures li { display: grid; gap: 2px; }
  .figure-name { font-weight: 600; font-size: var(--font-small); color: var(--heading); }
  .caption { font-size: var(--font-small); line-height: 1.55; color: var(--text); max-width: 80ch; }
  .labels { list-style: none; margin: 0 0 12px; padding: 0; display: grid; gap: 8px; max-width: 820px; }
  .labels li { display: flex; flex-wrap: wrap; align-items: baseline; gap: 4px 12px; }
  .ns { font-size: var(--font-small); color: var(--text-2); min-width: 7.5em; }
  .value { font-weight: 600; }
  .value.proposed { font-style: italic; font-weight: 500; color: var(--text-2); }
  .label-form { display: flex; flex-wrap: wrap; align-items: end; gap: 10px 14px; max-width: 820px; }
  .label-form label { display: grid; gap: 4px; font-size: var(--font-small); color: var(--text-2); }
  .label-form input { min-width: min(320px, 100%); min-height: 38px; }
  .label-form select { min-height: 38px; }
  .inline { display: grid; gap: 8px; width: 100%; max-width: 640px; }
  .inline label { display: grid; gap: 4px; font-size: var(--font-small); color: var(--text-2); }
  .inline textarea { width: 100%; }
  .row { display: flex; flex-wrap: wrap; align-items: center; gap: 10px 20px; }
  .acceptance { display: grid; gap: 8px; justify-items: start; max-width: 820px; }
  .acceptance p { max-width: 72ch; }
  .reader { display: grid; gap: 28px; align-items: start; }
  .reader.with-contents { grid-template-columns: minmax(170px, 210px) minmax(0, 1fr); gap: 40px; }
  .reading { display: grid; gap: 28px; min-width: 0; }
  .contents { position: sticky; top: calc(var(--bar-h) + 12px); max-height: calc(100vh - var(--bar-h) - 24px); overflow: auto; padding-right: 4px; }
  .contents-title { font-size: var(--font-xs); font-weight: 700; letter-spacing: 0.06em; text-transform: uppercase; color: var(--text-3); margin-bottom: 6px; }
  .contents ol { list-style: none; margin: 0; padding: 0; display: grid; gap: 2px; border-left: 1px solid var(--line); }
  .contents li.depth-2 { padding-left: 12px; }
  .contents li.depth-3 { padding-left: 24px; }
  .contents-link { all: unset; display: block; cursor: pointer; padding: 5px 10px; margin-left: -1px; border-left: 2px solid transparent; font-size: var(--font-small); line-height: 1.35; color: var(--text-2); overflow-wrap: anywhere; }
  .contents-link:hover { color: var(--heading); border-left-color: var(--line-2); background: var(--hover-wash); }
  .contents-link:focus-visible { outline: 2px solid var(--focus); outline-offset: 1px; border-radius: 3px; }
  .paper-section, #paper-abstract, #paper-figures, #paper-tables { scroll-margin-top: calc(var(--bar-h) + 16px); display: grid; gap: 10px; }
  .paper-section:focus, #paper-abstract:focus, #paper-figures:focus, #paper-tables:focus { outline: none; }
  .section-trail { font-size: var(--font-xs); color: var(--text-3); }
  .section-title { font-family: var(--serif); font-size: calc(22px * var(--type-scale)); line-height: 1.25; color: var(--heading); font-weight: 600; max-width: 34em; }
  .section-title.minor { font-size: calc(18px * var(--type-scale)); }
  .body-text { font-family: var(--serif); font-size: calc(16px * var(--type-scale)); line-height: 1.68; max-width: 72ch; overflow-wrap: anywhere; }
  .section-meta { font-size: var(--font-xs); color: var(--text-3); }
  .section-meta:empty { display: none; }
  .figure-list { display: grid; gap: 28px; max-width: 860px; }
  .figure-item { display: grid; gap: 8px; }
  .paper-figure { margin: 0; display: grid; gap: 8px; }
  .plate { display: block; background: #FFFFFF; border-radius: 6px; box-shadow: var(--group-edge); overflow: hidden; max-height: 640px; }
  .plate img { display: block; width: 100%; height: 100%; object-fit: contain; }
  .plate:focus-visible { outline: 2px solid var(--focus); outline-offset: 3px; }
  .plate.missing { background: var(--stage); color: var(--text-2); font-size: var(--font-small); padding: 18px; box-shadow: none; }
  figcaption { display: grid; gap: 2px; }
  .derived { font-size: var(--font-small); max-width: 80ch; }
  .derived summary { cursor: pointer; color: var(--text-2); }
  .derived p { margin-top: 6px; line-height: 1.55; }
  .table-list { list-style: none; margin: 0; padding: 0; display: grid; gap: 20px; max-width: 980px; }
  .table-list li { display: grid; gap: 8px; justify-items: start; }
  .table-head { display: grid; gap: 2px; }
  .grid-scroll { max-width: 100%; overflow: auto; max-height: 520px; border: 1px solid var(--line); border-radius: 6px; }
  .grid-scroll:focus-visible { outline: 2px solid var(--focus); outline-offset: 2px; }
  .grid-scroll table { border-collapse: collapse; font-size: var(--font-small); font-variant-numeric: tabular-nums; }
  .grid-scroll th, .grid-scroll td { padding: 6px 10px; border-bottom: 1px solid var(--line); text-align: left; vertical-align: top; max-width: 28ch; }
  .grid-scroll th { position: sticky; top: 0; background: var(--surface-2); color: var(--heading); font-weight: 600; }
  @media (max-width: 860px) {
    .reader.with-contents { grid-template-columns: minmax(0, 1fr); gap: 20px; }
    .contents { position: static; max-height: 40vh; border-bottom: 1px solid var(--line); padding-bottom: 12px; }
  }
</style>
