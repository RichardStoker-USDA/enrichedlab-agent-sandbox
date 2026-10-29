<script lang="ts">
  import { onDestroy, untrack } from "svelte";
  import type { CommonsCollection, CommonsCollections, CommonsKind, CommonsRecord, CommonsView } from "./contracts.ts";
  import { BrokerError } from "./api.ts";
  import { collectionMarkedWords, collectionName, collectionWords, commonsClaim, commonsFailure, commonsKinds, countWords, knowledge, markedWords,
    outcomeUnknown, partsWaitingWords, reachWords, recordFacts, roleWords, waitingWords } from "./knowledge.ts";
  import { live } from "./live.ts";
  import StatusMark from "./StatusMark.svelte";

  // The commons: what every lab's research agent reads. A person who leads the commons marks a record
  // or a whole collection baseline, or withdraws the mark, as themselves; the knowledge service decides and records
  // each mark in their name. A paper's mark covers its sections, figures and tables as they stand.
  let { notify }: { notify: (message: string, error?: boolean) => void } = $props();

  let kind = $state<CommonsKind>("dataset");
  let view = $state<CommonsView | null>(null);
  let collections = $state<CommonsCollections | null>(null);
  let loading = $state(true);
  let loadingMore = $state(false);
  let failure = $state("");
  let collectionsFailure = $state("");
  let busy = $state("");
  let rowFailure = $state<{ id: string; message: string; stale: boolean } | null>(null);
  let confirming = $state("");
  // Which change the open confirmation makes: set by the button that opened it, never inferred from the marks.
  let confirmAction = $state<"mark" | "withdraw">("mark");
  let controller: AbortController | null = null;

  async function load(quiet = false) {
    controller?.abort();
    const c = new AbortController();
    controller = c;
    loading = true;
    if (!quiet) { failure = ""; collectionsFailure = ""; }
    const [records, held] = await Promise.allSettled([knowledge.commons(kind, c.signal), knowledge.collections(c.signal)]);
    if (c.signal.aborted) return;
    if (records.status === "fulfilled") { view = records.value; failure = ""; }
    else if (!(quiet && view)) { view = null; failure = commonsFailure(records.reason, "load"); }
    if (held.status === "fulfilled") { collections = held.value; collectionsFailure = ""; }
    else if (!(quiet && collections)) { collections = null; collectionsFailure = commonsFailure(held.reason, "load"); }
    loading = false;
  }
  $effect(() => { void kind; untrack(() => { view = null; rowFailure = null; confirming = ""; void load(); }); });
  // A change to the commons reads the page again in place (live.ts); a failed quiet read keeps what it shows.
  const unwatch = live().watch(["commons"], () => load(true), { gapMs: 2_000 });
  onDestroy(() => { controller?.abort(); unwatch(); });

  async function more() {
    if (!view?.next || loadingMore) return;
    loadingMore = true;
    const signal = controller?.signal;
    try {
      const page = await knowledge.commons(kind, signal, view.next);
      if (!signal?.aborted && view) view = { ...page, records: [...view.records, ...page.records] };
    } catch (cause) {
      if (!signal?.aborted) notify(commonsFailure(cause, "load"), true);
    } finally {
      loadingMore = false;
    }
  }

  const waits = (record: CommonsRecord) => !!view?.can_mark && !record.baseline && record.lifecycle === "active";
  const waiting = $derived(view ? view.records.filter(waits).length : 0);
  const stale = (cause: unknown) => cause instanceof BrokerError && (cause.reason || cause.code) === "person_sign_in_stale";

  // A change the service did not confirm may have been made, so the page reloads what the service holds before the
  // person decides again.
  async function failed(id: string, cause: unknown, action: "mark" | "collection") {
    rowFailure = { id, message: commonsFailure(cause, action), stale: stale(cause) };
    if (outcomeUnknown(cause)) await load();
  }

  async function change(record: CommonsRecord, baseline = !record.baseline) {
    if (busy) return;
    busy = record.resource_id; rowFailure = null;
    try {
      const marked = await knowledge.mark(record.resource_id, baseline);
      const { parts_changed: _, ...next } = marked;
      const shown: CommonsRecord = next.kind === "paper" ? { ...next, parts_waiting: 0 } : next;
      if (view) view = { ...view, records: view.records.map((r) => (r.resource_id === shown.resource_id ? shown : r)) };
      notify(markedWords(marked));
    } catch (cause) {
      await failed(record.resource_id, cause, "mark");
    } finally {
      busy = "";
    }
  }

  async function markCollection(collection: CommonsCollection, baseline: boolean) {
    if (busy) return;
    busy = collection.prefix; rowFailure = null; confirming = "";
    try {
      notify(collectionMarkedWords(collection.prefix, await knowledge.markCollection(collection.prefix, baseline)));
      await load();
    } catch (cause) {
      await failed(collection.prefix, cause, "collection");
    } finally {
      busy = "";
    }
  }
  const counts = (c: CommonsCollection) => Object.values(c.kinds).reduce((t, k) => ({ active: t.active + k.active, marked: t.marked + k.marked }), { active: 0, marked: 0 });
</script>

<section class="commons-page" aria-labelledby="commons-title">
  <header>
    <h1 id="commons-title">Commons</h1>
    <p>{commonsClaim}</p>
  </header>
  {#if collections?.collections.length}
    <section class="collections" aria-labelledby="collections-title">
      <h2 id="collections-title" class="section-title">Collections</h2>
      <ol class="records collection-list" class:marking={collections.can_mark}>
        {#each collections.collections as collection (collection.prefix)}
          {@const words = collectionWords(collection)}
          {@const total = counts(collection)}
          {@const all = total.active > 0 && total.marked === total.active}
          <li class:waits={collections.can_mark && (!all || collection.parts_waiting > 0)}>
            <div class="head">
              <h3 class="title" id={`collection-${collection.prefix}`}>{collectionName(collection.prefix)}</h3>
              <StatusMark tone={all ? "done" : total.marked ? "attention" : "quiet"} label={all ? "Every lab reads it" : total.marked ? "Partly marked" : "Commons only"} />
            </div>
            <p class="summary">{words.holds}</p>
            {#if words.reach}<p class="small soft">{words.reach}</p>{/if}
            {#if words.waiting}<p class="small">{words.waiting} Mark the collection again to cover them.</p>{/if}
            {#if collections.can_mark}
              {#if confirming === collection.prefix}
                <p class="small" id={`confirm-${collection.prefix}`}>
                  {confirmAction === "withdraw" ? "Withdrawing takes every record of this collection, and each paper's parts, out of every lab's research agent's reading." : `Marking lets every lab's research agent read all ${total.active.toLocaleString("en-US")} records of this collection and each paper's sections, figures and tables. You can withdraw the mark later.`}
                </p>
                <div class="actions">
                  <button class="button" disabled={!!busy} aria-describedby={`confirm-${collection.prefix}`} onclick={() => markCollection(collection, confirmAction === "mark")}>
                    {busy === collection.prefix ? (confirmAction === "withdraw" ? "Withdrawing…" : "Marking…") : confirmAction === "withdraw" ? "Withdraw the collection's mark" : all ? "Mark the newer parts" : "Mark the collection baseline"}
                  </button>
                  <button class="link" disabled={!!busy} onclick={() => (confirming = "")}>Keep it as it is</button>
                </div>
              {:else}
                <div class="actions">
                  {#if !all || collection.parts_waiting > 0}
                    <button class="button secondary" disabled={!!busy} aria-describedby={`collection-${collection.prefix}`} onclick={() => { confirmAction = "mark"; confirming = collection.prefix; }}>
                      {all ? "Mark the newer parts" : total.marked ? "Mark the rest of the collection" : "Mark the collection baseline"}
                    </button>
                  {/if}
                  {#if total.marked > 0}
                    <button class="link small" disabled={!!busy} aria-describedby={`collection-${collection.prefix}`}
                      onclick={() => (all ? ((confirmAction = "withdraw"), (confirming = collection.prefix)) : markCollection(collection, false))}>Withdraw the collection's mark</button>
                  {/if}
                </div>
              {/if}
            {/if}
            {#if rowFailure?.id === collection.prefix}
              <p class="notice error" role="alert">{rowFailure.message}{#if rowFailure.stale} <a href="/cdn-cgi/access/logout">Sign out</a> (this signs you out of every campus app behind the same sign-in).{/if}</p>
            {/if}
            <details class="technical">
              <summary>Technical details</summary>
              <dl><dt>Source prefix</dt><dd>{collection.prefix}</dd><dt>Scope</dt><dd>{collections.scope}</dd></dl>
            </details>
          </li>
        {/each}
      </ol>
    </section>
  {:else if collectionsFailure && !failure}<p class="notice error" role="alert">{collectionsFailure}</p>{/if}
  <div class="kinds" role="group" aria-label="Kind of record">
    {#each commonsKinds as k (k.key)}
      <button class="kind" aria-pressed={kind === k.key} onclick={() => (kind = k.key)}>{k.heading}</button>
    {/each}
  </div>
  {#if loading && !view}<p class="soft" role="status">Opening the commons…</p>
  {:else if failure}<p class="notice error" role="alert">{failure}</p>
  {:else if view}
    <p class="lede">{countWords(view)} {view.can_mark ? waitingWords(waiting) : roleWords(view)}</p>
    {#if view.records.length}
      <ol class="records" class:marking={view.can_mark}>
        {#each view.records as record (record.resource_id)}
          {@const newer = partsWaitingWords(record)}
          <li class:waits={waits(record) || (!!view.can_mark && !!newer)}>
            <div class="head">
              <h2 class="title" id={`record-${record.resource_id}`}>{record.title}</h2>
              <StatusMark tone={record.baseline ? "done" : "quiet"} label={reachWords(record)} />
            </div>
            {#if record.summary}<p class="summary">{record.summary}</p>{/if}
            <p class="small soft">{recordFacts(record)}</p>
            {#if newer}<p class="small">{newer}</p>{/if}
            {#if view.can_mark && (record.lifecycle === "active" || record.baseline)}
              <div class="actions">
                {#if newer && record.lifecycle === "active"}
                  <button class="button secondary" disabled={!!busy} aria-describedby={`record-${record.resource_id}`} onclick={() => change(record, true)}>
                    {busy === record.resource_id ? "Marking…" : "Mark the newer parts"}
                  </button>
                {/if}
                <button class={newer ? "link small" : "button secondary"} disabled={!!busy} aria-describedby={`record-${record.resource_id}`} onclick={() => change(record)}>
                  {busy === record.resource_id && !newer ? (record.baseline ? "Withdrawing…" : "Marking…") : record.baseline ? "Withdraw the mark" : "Mark baseline"}
                </button>
              </div>
            {/if}
            {#if rowFailure?.id === record.resource_id}
              <p class="notice error" role="alert">{rowFailure.message}{#if rowFailure.stale} <a href="/cdn-cgi/access/logout">Sign out</a> (this signs you out of every campus app behind the same sign-in).{/if}</p>
            {/if}
            <details class="technical">
              <summary>Technical details</summary>
              <dl>
                <dt>Record</dt><dd>{record.resource_id}</dd>
                <dt>Scope</dt><dd>{record.scope}</dd>
                {#if record.source_uri}<dt>Source</dt><dd>{record.source_uri}</dd>{/if}
              </dl>
            </details>
          </li>
        {/each}
      </ol>
      {#if view.next}
        <button class="button secondary more" disabled={loadingMore} onclick={more}>{loadingMore ? "Loading more…" : `Show more ${commonsKinds.find((k) => k.key === kind)?.many ?? "records"}`}</button>
      {/if}
    {/if}
  {/if}
</section>

<style>
  .commons-page { padding: 40px var(--gutter) 24px; display: grid; gap: 22px; width: 100%; max-width: 1080px; margin: 0 auto; align-content: start; }
  header { display: grid; gap: 10px; }
  h1 { font-size: clamp(30px, calc(3.4vw * var(--type-scale)), calc(42px * var(--type-scale))); letter-spacing: -0.022em; }
  header p, .lede { max-width: 76ch; }
  .kinds { display: flex; flex-wrap: wrap; gap: 4px 22px; border-bottom: 1px solid var(--line); max-width: 900px; }
  .kind { position: relative; background: none; border: 0; padding: 8px 0 10px; min-height: 40px; cursor: pointer;
    font: inherit; font-size: var(--font-small); font-weight: 600; color: var(--text-2); transition: color 120ms var(--ease); }
  .kind:hover { color: var(--heading); }
  .kind[aria-pressed="true"] { color: var(--heading); }
  .kind[aria-pressed="true"]::after { content: ""; position: absolute; left: 0; right: 0; bottom: -1px; height: 3px; background: var(--mark); }
  .kind:focus-visible { outline: 2px solid var(--focus); outline-offset: 2px; }
  .lede { color: var(--text-2); }
  .records { list-style: none; margin: 0; padding: 0; display: grid; gap: 26px; max-width: 900px; }
  .records li { position: relative; display: grid; gap: 6px; justify-items: start; min-width: 0; padding-left: 14px; border-left: 1px dashed var(--line-2); }
  /* A lead's unmarked records carry the editor's change bar; it draws back once the mark is made. */
  .records.marking li::before { content: ""; position: absolute; left: -16px; top: 3px; bottom: 3px; width: 3px; background: var(--mark);
    transform: scaleY(0); transform-origin: top; transition: transform 200ms var(--ease); }
  .records.marking li.waits::before { transform: scaleY(1); }
  .head { display: flex; flex-wrap: wrap; align-items: baseline; gap: 6px 16px; }
  .title { font-size: calc(16px * var(--type-scale)); font-weight: 600; color: var(--heading); overflow-wrap: anywhere; }
  .summary { font-family: var(--serif); font-size: calc(15.5px * var(--type-scale)); line-height: 1.6; max-width: 72ch; overflow-wrap: anywhere; }
  .records .actions { display: flex; flex-wrap: wrap; align-items: center; gap: 8px 18px; margin-top: 4px; }
  .records .actions .link { min-height: 40px; }
  .collections { display: grid; gap: 14px; max-width: 900px; }
  .section-title { font-size: calc(20px * var(--type-scale)); font-weight: 700; color: var(--heading); letter-spacing: -0.01em; }
  .collections .title { font-size: calc(18px * var(--type-scale)); }
  .more { justify-self: start; }
  .records details.technical { justify-self: stretch; max-width: 72ch; }
  @media (prefers-reduced-motion: reduce) { .records.marking li::before { transition: none; } }
</style>
