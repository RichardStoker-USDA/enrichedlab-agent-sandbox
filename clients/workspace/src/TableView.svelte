<script lang="ts">
  import { onDestroy } from "svelte";
  import { strictText, tableRead, type TableRead } from "./artifact_content.ts";
  import { canDraw } from "./figure.ts";
  import { MAX_TABLE_BYTES, type ViewedFile } from "./viewer.ts";

  // A retained table exactly as written. Numbers align for reading; they are not parsed
  // or rounded, and the download is always the complete original. `onFigure`, when given,
  // offers a figure drawn by R from this table, and `onAnalyze` a reviewed R analysis of it.
  let { file, load, onDownload, title, showTitle = true, number, onFigure, onAnalyze }: {
    file: ViewedFile; load: (signal: AbortSignal) => Promise<Blob>; onDownload: () => void; title: string; showTitle?: boolean; number?: number;
    onFigure?: (read: { columns: string[]; numeric: boolean[] }) => void;
    onAnalyze?: (read: { columns: string[]; numeric: boolean[] }) => void;
  } = $props();
  let table = $state<TableRead | null>(null);
  let failure = $state("");
  let filter = $state("");
  let controller: AbortController | null = null;
  const numeric = /^-?(\d+\.?\d*|\.\d+)([eE][-+]?\d+)?$/;
  const rows = $derived(table?.rows.map((cells, index) => ({ cells, index })).filter(({ cells }) => !filter || cells.some((cell) => cell.toLowerCase().includes(filter.toLowerCase()))) ?? []);
  const numericColumns = $derived(table ? table.columns.map((_, c) => table!.rows.slice(0, 200).every((r) => r[c] === "" || numeric.test(r[c]))) : []);

  $effect(() => {
    void file.sha256;
    table = null; failure = ""; filter = "";
    if (file.size_bytes > MAX_TABLE_BYTES) { failure = "large"; return; }
    controller?.abort();
    const c = new AbortController();
    controller = c;
    void load(c.signal)
      .then((blob) => strictText(blob, MAX_TABLE_BYTES))
      .then((text) => { if (!c.signal.aborted) table = tableRead(text, file.path.toLowerCase().endsWith(".tsv") || file.media_type.startsWith("text/tab-separated") ? "\t" : ","); })
      .catch(() => { if (!c.signal.aborted) failure = "unreadable"; });
    return () => c.abort();
  });
  onDestroy(() => controller?.abort());
</script>

<section class="table-view" aria-label={number ? `Table ${number}: ${title}` : title}>
  <div class="caption">
    <p>
      {#if number}<strong>Table {number}.</strong>{/if}
      {#if showTitle}<span class="name">{title}.</span>{/if}
      <span class="facts">
        {#if table}{table.totalRows.toLocaleString()} {table.totalRows === 1 ? "row" : "rows"}, {table.columns.length} columns. Checksum matches.
        {:else if failure === "large"}Too large to show here; the download is the complete original.
        {:else if failure}This table could not be read without changing its structure; download the original.
        {:else}Opening…{/if}
      </span>
    </p>
    <div class="tools">
      {#if table && table.totalRows > 12}<input type="search" aria-label={`Find rows in ${title}`} placeholder="Find rows" bind:value={filter} />{/if}
      {#if table && onFigure && canDraw(table.totalRows, table.columns.length)}
        <button class="link" onclick={() => onFigure({ columns: table!.columns, numeric: numericColumns })}>Make a figure</button>
      {/if}
      {#if table && onAnalyze && canDraw(table.totalRows, table.columns.length)}
        <button class="link" onclick={() => onAnalyze({ columns: table!.columns, numeric: numericColumns })}>Analyze</button>
      {/if}
      <button class="link" onclick={onDownload}>Download</button>
    </div>
  </div>
  {#if table}
    <!-- svelte-ignore a11y_no_noninteractive_tabindex -->
    <div class="scroll" role="region" aria-label={`${title}, scroll for more`} tabindex="0">
      <table>
        <thead><tr><th scope="col" class="row"><span class="visually-hidden">Row</span></th>{#each table.columns as column, c}<th scope="col" class:num={numericColumns[c]}>{column}</th>{/each}</tr></thead>
        <tbody>
          {#each rows as row (row.index)}
            <tr><th scope="row" class="row">{row.index + 1}</th>{#each row.cells as cell, c}<td class:num={numericColumns[c]}>{cell}</td>{/each}</tr>
          {/each}
        </tbody>
      </table>
      {#if !rows.length}<p class="soft small empty">No rows match.</p>{/if}
    </div>
    {#if table.truncated}<p class="faint small">Showing the first {table.rows.length.toLocaleString()} of {table.totalRows.toLocaleString()} rows. The download has every row.</p>{/if}
  {/if}
</section>

<style>
  .table-view { display: grid; gap: 8px; min-width: 0; }
  .caption { display: flex; justify-content: space-between; align-items: baseline; gap: 6px 20px; flex-wrap: wrap; }
  .caption p { max-width: 72ch; font-size: var(--font-small); color: var(--text); }
  .caption strong { color: var(--heading); margin-right: 4px; }
  .caption .name { font-family: var(--serif); font-size: calc(15px * var(--type-scale)); color: var(--heading); margin-right: 6px; }
  .facts { color: var(--text-3); }
  /* The search and the links wrap on a phone rather than push the page sideways. */
  .tools { display: flex; flex-wrap: wrap; align-items: center; gap: 8px 14px; font-size: var(--font-small); min-width: 0; }
  .tools input { width: 200px; max-width: 100%; min-height: 34px; padding: 5px 10px; font-size: var(--font-small); }
  /* Publication table: a heavy rule above and below, a light rule under the header, no boxes. */
  .scroll { max-height: min(460px, 62vh); overflow: auto; }
  table { border-collapse: separate; border-spacing: 0; width: max-content; min-width: 100%; font-size: var(--font-small); font-variant-numeric: tabular-nums; }
  th, td { padding: 6px 16px 6px 0; text-align: left; white-space: nowrap; }
  th:last-child, td:last-child { padding-right: 2px; }
  thead th { position: sticky; top: 0; z-index: 1; background: var(--canvas); color: var(--heading); font-weight: 650; border-top: 2px solid var(--heading); border-bottom: 1px solid var(--heading); padding-top: 8px; }
  tbody tr:last-child > * { border-bottom: 2px solid var(--heading); }
  .row { color: var(--text-3); font-weight: 400; text-align: right; width: 1%; padding-right: 14px; }
  td.num, th.num { text-align: right; }
  tbody tr:hover > * { background: var(--hover-wash); }
  .empty { padding: 12px 0; }
</style>
