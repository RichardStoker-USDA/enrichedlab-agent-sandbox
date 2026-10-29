<script lang="ts">
  import StatusMark from "./StatusMark.svelte";
  import { checkLabel, checkSentence, checkTone, type StepCheck } from "./step_checks.ts";
  // Each of an analysis's results checked against what its step said the result must show, as
  // the research agent read it. The reasons are code's own words; a failed result supports nothing later.
  let { checks }: { checks: StepCheck[] } = $props();
</script>

{#if checks.length}
  <section class="block" aria-labelledby="a-check">
    <h2 id="a-check" class="panel-title">Result check</h2>
    {#each checks as c (c.ref)}
      <div class="check" class:change-bar={c.state === "failed"}>
        <p><StatusMark tone={checkTone(c.state)} label={checkLabel(c.state)} wrap /> {checkSentence(c.state)}</p>
        {#if c.reasons?.length}<ul>{#each c.reasons as reason, i (i)}<li>{reason}</li>{/each}</ul>{/if}
        {#if c.reasons_omitted}<p class="more">{c.reasons_omitted} more {c.reasons_omitted === 1 ? "reason" : "reasons"} in the decision's record.</p>{/if}
      </div>
    {/each}
  </section>
{/if}

<style>
  .check + .check { margin-top: 12px; }
  .check p { margin: 0; }
  ul { margin: 6px 0 0; padding-left: 18px; }
  li { margin: 2px 0; overflow-wrap: anywhere; }
  .more { margin-top: 4px; font-size: var(--font-xs); color: var(--text-3); }
</style>
