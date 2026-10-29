<script lang="ts">
  import type { StepLineage } from "./contracts.ts";
  import { shapeStepLineage, type StepLineageItem } from "./step_lineage.ts";

  let { lineage }: { lineage: StepLineage } = $props();
  const view = $derived(shapeStepLineage(lineage));
</script>

{#snippet branch(item: StepLineageItem)}
  <li>
    <details class="input" class:gap={item.type === "missing" || item.type === "cycle" || item.type === "limit"}>
      <summary>
        <svg class="chevron" viewBox="0 0 16 16" aria-hidden="true"><path d="M6 3.5 10.5 8 6 12.5" /></svg>
        <span class="summary-words">
          <span class="input-title">{item.title}</span>
          <span class="input-kind">{item.kind}</span>
        </span>
      </summary>
      <div class="branch-body">
        <p class="path" aria-label={`Recorded path: ${item.path.join(" to ")}`}><span>Path</span>{item.path.join(" → ")}</p>
        {#if item.method}
          <dl class="method">
            <div><dt>Method</dt><dd>{item.method.name}</dd></div>
            <div><dt>Version</dt><dd>{item.method.version ?? "Not recorded in this response."}</dd></div>
            <div><dt>Receipt</dt><dd>{item.method.receipt}</dd></div>
          </dl>
        {/if}
        {#each item.notes as note (note)}<p class="note">{note}</p>{/each}
        {#if item.inputs.length}
          <p class="inputs-title">Inputs to this step</p>
          <ol class="children">
            {#each item.inputs as child (child.key)}{@render branch(child)}{/each}
          </ol>
        {/if}
        {#if item.technical.length}
          <details class="technical">
            <summary>Technical details</summary>
            <dl>
              {#each item.technical as fact, i (`${fact.label}:${i}`)}<dt>{fact.label}</dt><dd>{fact.value}</dd>{/each}
            </dl>
          </details>
        {/if}
      </div>
    </details>
  </li>
{/snippet}

<div class="step-lineage" aria-label="Recorded inputs and their sources">
  {#if view.incomplete}<p class="gap-note">{view.incomplete}</p>{/if}
  <p class="intro">Open an input to follow the recorded steps back to its source. Each level stays closed until you open it.</p>
  {#if view.rootGaps.length}
    <div class="root-gaps">
      <p class="inputs-title">Missing records for this step</p>
      {#each view.rootGaps as gap (gap)}<p class="note">{gap}</p>{/each}
      {#if view.rootTechnical.length}
        <details class="technical">
          <summary>Technical details</summary>
          <dl>
            {#each view.rootTechnical as fact, i (`${fact.label}:${i}`)}<dt>{fact.label}</dt><dd>{fact.value}</dd>{/each}
          </dl>
        </details>
      {/if}
    </div>
  {/if}
  {#if view.inputs.length}
    <ol class="roots">
      {#each view.inputs as item (item.key)}{@render branch(item)}{/each}
    </ol>
  {:else if view.empty}
    <p class="gap-note">{view.empty}</p>
  {/if}
</div>

<style>
  .step-lineage { display: grid; gap: 10px; width: 100%; margin-top: 8px; }
  .intro { max-width: 66ch; color: var(--text-2); font-size: var(--font-small); }
  .roots, .children { list-style: none; margin: 0; padding: 0; }
  .roots { border-top: 1px solid var(--line); }
  .roots > li { border-bottom: 1px solid var(--line); }
  .input { background: var(--surface); }
  .input.gap { box-shadow: inset 2px 0 0 var(--mark); }
  .input > summary { display: grid; grid-template-columns: 18px minmax(0, 1fr); gap: 8px; align-items: start;
    min-height: 44px; padding: 7px 8px 7px 4px; cursor: pointer; list-style: none; color: var(--text); }
  .input > summary::-webkit-details-marker { display: none; }
  .input > summary:focus-visible { outline: 2px solid var(--focus); outline-offset: 2px; border-radius: var(--r-1); }
  .chevron { width: 14px; height: 14px; margin-top: 5px; fill: none; stroke: currentColor; stroke-width: 1.6; color: var(--text-2);
    transition: transform var(--t-base) var(--ease); }
  .input[open] > summary .chevron { transform: rotate(90deg); }
  .summary-words { display: flex; flex-wrap: wrap; gap: 1px 10px; align-items: baseline; min-width: 0; }
  .input-title { color: var(--heading); font-weight: 650; overflow-wrap: anywhere; }
  .input-kind { color: var(--text-2); font-size: var(--font-small); overflow-wrap: anywhere; }
  .branch-body { display: grid; gap: 8px; padding: 0 8px 14px 30px; }
  .input[open] > .branch-body { animation: lineage-open var(--t-fast) var(--ease); }
  .path { color: var(--text-2); font-size: var(--font-small); overflow-wrap: anywhere; }
  .path span { color: var(--text-3); margin-right: 8px; }
  .method { display: grid; gap: 5px; margin: 2px 0; padding: 9px 0; border-block: 1px solid var(--line); }
  .method div { display: grid; grid-template-columns: 66px minmax(0, 1fr); gap: 10px; }
  .method dt { color: var(--text-2); font-size: var(--font-small); }
  .method dd { margin: 0; overflow-wrap: anywhere; }
  .note, .gap-note { color: var(--text-2); font-size: var(--font-small); max-width: 70ch; overflow-wrap: anywhere; }
  .gap-note { padding-left: 10px; border-left: 2px solid var(--mark); }
  .root-gaps { display: grid; gap: 6px; padding-bottom: 8px; border-bottom: 1px solid var(--line); }
  .inputs-title { margin-top: 3px; color: var(--text); font-size: var(--font-small); font-weight: 650; overflow-wrap: anywhere; }
  .children { margin-left: 3px; border-left: 1px solid var(--line); }
  .children > li { padding-left: 12px; }
  .children > li + li { border-top: 1px solid var(--line); }
  .technical { color: var(--text-2); font-size: var(--font-small); }
  .technical > summary { display: inline-flex; min-height: 32px; align-items: center; cursor: pointer; }
  .technical > summary:focus-visible { outline: 2px solid var(--focus); outline-offset: 2px; border-radius: var(--r-1); }
  .technical dl { display: grid; grid-template-columns: max-content minmax(0, 1fr); gap: 4px 12px; margin: 5px 0 0; }
  .technical dt { color: var(--text-3); }
  .technical dd { margin: 0; font-family: var(--mono); font-size: .92em; overflow-wrap: anywhere; }
  @keyframes lineage-open { from { opacity: 0; transform: translateY(-2px); } to { opacity: 1; transform: translateY(0); } }
  @media (prefers-reduced-motion: reduce) {
    .chevron { transition: none; }
    .branch-body { animation: none; }
  }
  @media (max-width: 640px) {
    .branch-body { padding-left: 26px; padding-right: 2px; }
    .method div { grid-template-columns: 58px minmax(0, 1fr); }
    .technical dl { grid-template-columns: 1fr; gap: 1px; }
    .technical dd + dt { margin-top: 5px; }
  }
</style>
