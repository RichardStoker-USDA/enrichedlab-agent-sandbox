<script lang="ts">
  import { onDestroy, untrack } from "svelte";
  import type { Identity, LibraryLabel, LibraryLabelNamespace, LibraryPaper, LibraryPaperView } from "./contracts.ts";
  import { currentLab } from "./lab.svelte.ts";
  import { day } from "./lab_notes.ts";
  import { acceptanceWords, addedWords, byline, checkedWords, labelProblem, library, libraryFailure, MAX_LABEL, MAX_REMOVE_REASON, namespaceName, namespaces, parseWords } from "./library.ts";
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

    {#if view.outline_error === "library_unavailable"}
      <p class="notice error" role="alert">The abstract and figure captions could not be read right now. The paper itself is unchanged.</p>
    {/if}
    {#if view.outline?.abstract}
      <section aria-labelledby="abstract-title">
        <h2 id="abstract-title" class="group-title">Abstract</h2>
        <p class="abstract">{view.outline.abstract}</p>
      </section>
    {/if}
    {#if view.outline?.figures.length}
      <section aria-labelledby="figures-title">
        <h2 id="figures-title" class="group-title">{view.outline.figures_total ? `Figures (${view.outline.figures_total})` : "Figures"}</h2>
        <ol class="figures">
          {#each view.outline.figures as figure, i (i)}
            <li><span class="figure-name">{figure.label ?? `Figure ${i + 1}`}{figure.page ? `, page ${figure.page}` : ""}</span>{#if figure.caption}<span class="caption">{figure.caption}</span>{/if}</li>
          {/each}
        </ol>
        {#if (view.outline.figures_total ?? 0) > view.outline.figures.length}<p class="small soft">The first {view.outline.figures.length} are listed.</p>{/if}
      </section>
    {/if}

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
</style>
