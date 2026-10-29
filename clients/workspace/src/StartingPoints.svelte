<script lang="ts">
  import { chosenSentence } from "./lab_starts.ts";
  import type { WorkspaceChoice } from "./projects.ts";

  // The data and method sets a project starts from, for the ask sheet and a project's setup. Where the broker combines
  // sets, a person may choose one, several or every set in the lab, for a question across all of them
  // ("even do all of them if there are many"); an older broker takes one set, as before. A lab with one
  // set has it chosen.
  let { choices, combine, chosen = $bindable([]), disabled = false, labName, name = "start" }: {
    choices: WorkspaceChoice[]; combine: boolean; chosen?: string[]; disabled?: boolean; labName: string; name?: string;
  } = $props();

  const several = $derived(combine && choices.length > 1);
  const picked = $derived(choices.filter((c) => chosen.includes(c.key)));
  const allChosen = $derived(choices.length > 0 && picked.length === choices.length);
  function toggleAll() { chosen = allChosen ? [] : choices.map((c) => c.key); }
</script>

<fieldset class="starts" {disabled}>
  <legend>Data and methods{#if several}<span class="count">{picked.length} of {choices.length} chosen</span>{/if}</legend>
  <ul class="grouped">
    {#if several}
      <li class="all-row"><label class="start">
        <input type="checkbox" checked={allChosen} indeterminate={picked.length > 0 && !allChosen} onchange={toggleAll} />
        <span class="start-body"><span class="start-name">Every set in {labName}</span>
          <span class="start-text">For a question across everything the lab holds.</span></span>
      </label></li>
    {/if}
    {#each choices as c (c.key)}
      <li><label class="start" class:only={choices.length === 1}>
        {#if several}<input type="checkbox" value={c.key} bind:group={chosen} />
        {:else if choices.length > 1}<input type="radio" {name} value={c.key} checked={chosen[0] === c.key} onchange={() => (chosen = [c.key])} />{/if}
        <span class="start-body"><span class="start-name">{c.name}</span><span class="start-text">{c.description}</span></span>
      </label></li>
    {/each}
  </ul>
  {#if several}<p class="chosen" aria-live="polite">{chosenSentence(picked, choices.length)}</p>{/if}
</fieldset>

<style>
  fieldset { border: 0; margin: 0; padding: 0; display: grid; gap: 8px; min-width: 0; }
  legend { font-size: var(--font-small); font-weight: 650; color: var(--heading); margin-bottom: 6px; padding: 0; display: flex; align-items: baseline; gap: 10px; }
  .start { display: flex; align-items: flex-start; gap: 12px; padding: 12px 14px; font-weight: 400; color: var(--text); cursor: pointer;
    transition: background-color var(--t-fast) var(--ease); }
  .start:hover { background: var(--row-hover); }
  .start.only { cursor: default; }
  .start.only:hover { background: none; }
  .start input { margin-top: 3px; flex: none; }
  .start-body { display: grid; gap: 3px; min-width: 0; }
  .start-name { font-weight: 650; color: var(--heading); }
  .start-text { font-size: var(--font-small); line-height: 1.5; color: var(--text-2); }
  /* Every set reads as the list's own head: the same row, set apart by a firmer rule below it. */
  .grouped > li.all-row + li { border-top-color: var(--line-2); }
  .chosen { font-size: var(--font-small); color: var(--text-2); min-height: 1.5em; margin: 2px 0 0 2px; }
  fieldset:disabled .start { cursor: default; }
  fieldset:disabled .start:hover { background: none; }
  @media (prefers-reduced-motion: reduce) { .start { transition: none; } }
</style>
