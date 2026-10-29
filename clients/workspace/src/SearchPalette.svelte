<script lang="ts">
  import { tick } from "svelte";
  import { kindWord, rank, type SearchEntry } from "./search.ts";

  // Search your research (Command K): a native modal, so the browser keeps focus inside it and Escape closes it.
  let { open = $bindable(false), entries, choose }: { open?: boolean; entries: SearchEntry[]; choose: (entry: SearchEntry) => void } = $props();
  let dialog = $state<HTMLDialogElement>();
  let input = $state<HTMLInputElement>();
  let query = $state("");
  let pick = $state(0);
  const results = $derived(rank(entries, query));

  $effect(() => {
    if (!dialog) return;
    if (open && !dialog.open) { query = ""; pick = 0; dialog.showModal(); void tick().then(() => input?.focus()); }
    if (!open && dialog.open) dialog.close();
  });
  function take(entry: SearchEntry | undefined) {
    if (!entry) return;
    open = false;
    choose(entry);
  }
  function keys(event: KeyboardEvent) {
    if (event.key === "ArrowDown") { pick = Math.min(results.length - 1, pick + 1); event.preventDefault(); }
    else if (event.key === "ArrowUp") { pick = Math.max(0, pick - 1); event.preventDefault(); }
    else if (event.key === "Enter") { event.preventDefault(); take(results[pick]); }
  }
  // A click on the backdrop (the dialog element itself, outside its panel) closes it.
  function outside(event: MouseEvent) { if (event.target === dialog) open = false; }
</script>

<dialog bind:this={dialog} class="search" aria-label="Search your research" onclose={() => (open = false)} onclick={outside}>
  <div class="panel">
    <div class="field">
      <svg viewBox="0 0 20 20" aria-hidden="true"><circle cx="9" cy="9" r="5" /><path d="m13 13 3.5 3.5" /></svg>
      <input bind:this={input} bind:value={query} oninput={() => (pick = 0)} onkeydown={keys} placeholder="Search your research"
        role="combobox" aria-expanded="true" aria-controls="search-results" aria-autocomplete="list"
        aria-activedescendant={results[pick] ? `search-${pick}` : undefined} />
      <kbd>Esc</kbd>
    </div>
    <ul id="search-results" role="listbox" aria-label="Places, actions and projects">
      {#each results as entry, i (entry.kind + entry.id)}
        <li id={`search-${i}`} role="option" aria-selected={i === pick}>
          <button tabindex="-1" onclick={() => take(entry)} onpointerenter={() => (pick = i)}>
            <span class="kind">{kindWord[entry.kind]}</span>
            <span class="label">{entry.label}</span>
            {#if entry.detail}<span class="detail">{entry.detail}</span>{/if}
          </button>
        </li>
      {:else}
        <li class="empty">No place, action or project by that name. Projects match by title and question.</li>
      {/each}
    </ul>
  </div>
</dialog>

<style>
  .search { width: min(640px, calc(100vw - 32px)); max-height: min(560px, calc(100dvh - 120px)); margin: calc(var(--bar-h) + 32px) auto auto; padding: 0;
    border: 0; border-radius: var(--r-3); background: var(--surface); color: var(--text); box-shadow: 0 0 0 1px var(--track-line), var(--overlay-shadow); overflow: hidden; }
  .search::backdrop { background: var(--scrim); }
  .search[open] { animation: search-in 160ms var(--ease); }
  @keyframes search-in { from { opacity: 0; transform: translateY(-6px) scale(0.985); } }
  .panel { display: flex; flex-direction: column; max-height: inherit; }
  .field { display: flex; align-items: center; gap: 10px; padding: 0 14px; border-bottom: 1px solid var(--line); }
  .field svg { width: 18px; height: 18px; flex: none; fill: none; stroke: var(--text-3); stroke-width: 1.7; stroke-linecap: round; }
  .field input { min-height: 52px; padding: 0; border: 0; border-radius: 0; background: transparent; font-size: calc(16px * var(--type-scale)); }
  .field input:focus-visible { box-shadow: none; }
  kbd { flex: none; padding: 1px 6px; border-radius: 5px; box-shadow: inset 0 0 0 1px var(--line-2); font: 600 11px var(--ui); color: var(--text-3); }
  ul { list-style: none; margin: 0; padding: 6px; overflow: auto; }
  li button { width: 100%; display: grid; grid-template-columns: 72px minmax(0, 1fr); column-gap: 12px; align-items: baseline; padding: 9px 12px; border-radius: var(--r-2); text-align: left; }
  li[aria-selected="true"] button { background: var(--surface-2); box-shadow: inset 3px 0 0 var(--mark); }
  .kind { font-size: var(--font-xs); color: var(--text-3); }
  .label { font-size: var(--font-small); font-weight: 650; color: var(--heading); overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
  .detail { grid-column: 2; font-size: var(--font-xs); color: var(--text-3); overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
  .empty { padding: 12px; font-size: var(--font-small); color: var(--text-3); }
  @media (max-width: 640px) { .search { margin-top: 12px; max-height: calc(100dvh - 24px); } }
</style>
