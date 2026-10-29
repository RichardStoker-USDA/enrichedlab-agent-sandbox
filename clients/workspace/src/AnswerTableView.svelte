<script lang="ts">
  import { answerTableRows, display, type AnswerTable, type ArtifactDigest } from "./findings.ts";
  import type { Tone } from "./language.ts";
  import StatusMark from "./StatusMark.svelte";

  // A whole results table the agent gave as part of its answer. The agent chose the table and its columns
  // and wrote only the opening words; every row comes from the stored digest of the retained file. A long decimal is
  // shown to four significant digits, as in a finding, with the file's exact text in its value and title. A table the
  // digest does not hold (more rows than a digest keeps, or a lab's own data) is read from the
  // verified file through the link: the project page opens its analysis, the detailed view the table itself.
  let { table, digests, source, flag = null }: {
    table: AnswerTable;
    digests: Record<string, ArtifactDigest | undefined>;
    source: { from: string; label: string; open: () => void; href?: string } | null;
    // A person's review of the analysis the table reads, marked as a finding's is.
    flag?: { tone: Tone; label: string } | null;
  } = $props();
  const shown = $derived(answerTableRows(table, digests));
  const numeric = /^-?(\d+\.?\d*|\.\d+)([eE][-+]?\d+)?$/;
  const numericColumns = $derived("columns" in shown
    ? shown.columns.map((_, c) => shown.rows.every((r) => r[c] === "" || numeric.test(r[c])))
    : []);
  const lead = $derived(table.lead_in.trim().replace(/[.:;,]+$/, ""));
  const count = $derived(`${table.rows.toLocaleString()} ${table.rows === 1 ? "row" : "rows"}`);
  const why: Record<string, string> = {
    too_large: "It has more rows than this page keeps.",
    lab_data: "Its values are the lab's own data.",
    lab_derived: "Its values were computed from the lab's own data.",
  };
</script>

<figure class="answer-table">
  <figcaption>
    <span class="lead">{lead}.</span>
    <span class="facts">{count}{source ? `, from ${source.from}` : ""}.</span>
    {#if source?.href}<a class="link small" href={source.href} onclick={(e) => { e.preventDefault(); source.open(); }}>{source.label}</a>
    {:else if source}<button class="link small" onclick={source.open}>{source.label}</button>{/if}
    {#if flag}<span class="small flag"><StatusMark tone={flag.tone} label={flag.label} wrap /></span>{/if}
  </figcaption>
  {#if "columns" in shown}
    <!-- svelte-ignore a11y_no_noninteractive_tabindex -->
    <div class="scroll" role="region" aria-label={`${lead}, scroll for more`} tabindex="0">
      <table>
        <thead><tr>{#each shown.columns as column, c}<th scope="col" class:num={numericColumns[c]}>{column}</th>{/each}</tr></thead>
        <tbody>
          {#each shown.rows as row, r (r)}
            <tr>{#each row as cell, c}<td class:num={numericColumns[c]}>{#if display(cell) !== cell}<data value={cell} title={cell}>{display(cell)}</data>{:else}{cell}{/if}</td>{/each}</tr>
          {/each}
        </tbody>
      </table>
    </div>
  {:else if shown.omitted !== "not_loaded"}
    <p class="small soft">{why[shown.omitted] ?? "This page cannot show its values."} The retained file has every row.</p>
  {/if}
</figure>

<style>
  .answer-table { display: grid; gap: 8px; min-width: 0; margin: 0; }
  figcaption { font-size: var(--font-small); color: var(--text); max-width: 72ch; }
  .lead { font-family: var(--serif); font-size: calc(15px * var(--type-scale)); color: var(--heading); margin-right: 4px; }
  .facts { color: var(--text-3); margin-right: 8px; }
  .flag { display: block; margin-top: 4px; }
  /* The publication table of the table view: a heavy rule above and below, a light rule under the header, no boxes. */
  .scroll { max-height: min(460px, 62vh); overflow: auto; }
  table { border-collapse: separate; border-spacing: 0; width: max-content; min-width: 100%; font-size: var(--font-small); font-variant-numeric: tabular-nums; }
  th, td { padding: 6px 16px 6px 0; text-align: left; white-space: nowrap; }
  th:last-child, td:last-child { padding-right: 2px; }
  thead th { position: sticky; top: 0; z-index: 1; background: var(--canvas); color: var(--heading); font-weight: 650; border-top: 2px solid var(--heading); border-bottom: 1px solid var(--heading); }
  tbody tr:last-child > * { border-bottom: 2px solid var(--heading); }
  td.num, th.num { text-align: right; }
  tbody tr:hover > * { background: var(--hover-wash); }
</style>
