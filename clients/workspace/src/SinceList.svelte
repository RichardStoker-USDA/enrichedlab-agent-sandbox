<script lang="ts">
  import { when } from "./language.ts";
  import type { SinceKey, SinceRow } from "./since.ts";
  import StatusMark from "./StatusMark.svelte";

  // Since you last looked (long runs rule 1): rows counted from the records since the reader's own last look. Each
  // row takes the reader to the part of the page that holds it.
  let { at, rows, go }: { at: string; rows: SinceRow[]; go: (key: SinceKey) => void } = $props();
</script>

<section id="section-since" class="since" aria-labelledby="since-title">
  <h2 id="since-title" class="section-title">Since you last looked <small><time datetime={at}>{when(at)}</time></small></h2>
  {#if rows.length}
    <ul class="grouped since-rows">
      {#each rows as row (row.key)}
        <li><button class="since-row" onclick={() => go(row.key)}>
          <StatusMark tone={row.tone} label="" />
          <span class="since-body"><span class="since-title">{row.title}</span><span class="since-detail">{row.detail}</span></span>
          <svg class="chevron" viewBox="0 0 16 16" aria-hidden="true"><path d="M6 3.5 10.5 8 6 12.5" /></svg>
        </button></li>
      {/each}
    </ul>
  {:else}
    <p class="soft">Nothing new in its records since then.</p>
  {/if}
</section>

<style>
  .since { display: grid; grid-template-columns: minmax(0, 1fr); gap: 12px; }
  .since-row { width: 100%; display: flex; align-items: center; gap: 14px; min-height: 52px; padding: 12px 16px; border: 0; background: none;
    text-align: start; color: var(--text); font: inherit; cursor: pointer; border-radius: inherit; }
  .since-row:hover { background: var(--hover); }
  .since-row:focus-visible { outline: 2px solid var(--focus); outline-offset: -2px; }
  .since-row > :global(.status) { flex: none; width: 16px; justify-content: center; }
  .since-body { flex: 1; min-width: 0; display: grid; gap: 2px; }
  .since-title { font-weight: 600; }
  .since-detail { font-size: var(--font-small); color: var(--text-2); overflow-wrap: anywhere; }
  .chevron { flex: none; width: 16px; height: 16px; fill: none; stroke: var(--text-3); stroke-width: 1.6; stroke-linecap: round; stroke-linejoin: round; }
</style>
