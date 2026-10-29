<script lang="ts">
  import { onDestroy, untrack } from "svelte";
  import { BrokerError } from "../api.ts";
  import { strictText, tableRead } from "../artifact_content.ts";
  import { canonicalJson, sha256Hex } from "../canonical.ts";
  import { MAX_TABLE_BYTES } from "../viewer.ts";
  import type { Location, Page, Summary, ViewContext } from "./types.ts";

  // Values: a table as its stored strings, a page at a time from the broker, sorted by the broker by each
  // column's type, each row with its place in the file. A page whose checksum is not the file's is refused. Without
  // server pages the verified file is read whole (up to 8 MB) and its first 1,000 rows are shown and can be searched.
  // Nothing here turns a value into a number for display or computes one.
  // A view's context is fixed for its mount; the frame mounts a new view for a new file.
  const props: { ctx: ViewContext } = $props();
  const ctx = untrack(() => props.ctx);
  // A page holds 100 rows, or 10 when 100 of this table's rows pass the broker's 8 MiB for one answer.
  let limit = $state(100);
  type Row = { index: number; line: number | null; values: string[] };
  type Sort = { column: string; order: "asc" | "desc" } | null;
  let columns = $state<string[]>([]), types = $state<string[]>([]), page = $state.raw<Row[]>([]), total = $state(0);
  let offset = $state(Number(ctx.state?.offset ?? 0)), sort = $state<Sort>((ctx.state?.sort as Sort) ?? null);
  let mode = $state<"server" | "whole">(ctx.page ? "server" : "whole");
  let failure = $state(""), busy = $state(true), shortened = $state(false);
  let whole = $state.raw<Row[] | null>(null), filter = $state("");
  let picked = $state<number | null>(null);
  let summary = $state<Summary | null>(null), summaryState = $state<"closed" | "reading" | "open" | "failed" | "too_large">("closed");
  const numeric = (c: number) => types[c] === "integer" || types[c] === "number";
  const matching = $derived(whole && filter ? whole.filter((r) => r.values.some((v) => v.toLowerCase().includes(filter.toLowerCase()))) : whole);
  const rows = $derived(mode === "whole" ? (matching ?? []).slice(offset, offset + limit) : page);
  const count = $derived(mode === "whole" ? (matching?.length ?? 0) : total);

  const refusals: Record<string, string> = {
    not_a_table: "This file is not a table the server can page.",
    table_unreadable: "The server could not read this table without changing its structure. The download is the complete original.",
    table_too_large: "This table is larger than the server pages. The download is the complete original.",
    package_member_not_found: "The server no longer finds this file in its package.",
    artifact_integrity_failed: "The stored file did not match its recorded checksum, so its rows are not shown.",
    page_too_large: "Even a few of this table's rows are too large to show a page at a time. The download is the complete original.",
  };

  let controller: AbortController | null = null;
  async function load() {
    if (mode === "whole" && whole) return;
    controller?.abort();
    const c = new AbortController();
    controller = c;
    busy = true;
    try {
      if (mode === "server" && ctx.page) {
        try {
          const read: Page = await ctx.page({ offset, limit, ...(sort ? { sort: sort.column, order: sort.order } : {}) }, c.signal);
          if (c.signal.aborted) return;
          if (read.sha256 !== ctx.file.sha256) { failure = "The server's pages came from other bytes than this file's checksum names, so they are not shown."; return; }
          if (!Array.isArray(read.rows) || read.rows.some((r) => !Number.isInteger(r.index) || r.values.length !== read.columns.length)) throw new BrokerError("invalid_response");
          columns = read.columns; types = read.types; page = read.rows; total = read.total; failure = "";
          ctx.keep({ offset, sort: $state.snapshot(sort) });
          return;
        } catch (cause) {
          if (c.signal.aborted) return;
          // Rows too large for 100 to a page are asked for again ten at a time, from the page that holds the same rows.
          if (cause instanceof BrokerError && cause.reason === "page_too_large" && limit > 10) {
            limit = 10;
            return;
          }
          // A broker without pages answers 404; the verified file is then read whole, as before.
          if (!(cause instanceof BrokerError && cause.code === "record_unavailable" && cause.reason !== "package_member_not_found")) {
            failure = cause instanceof BrokerError ? refusals[cause.reason] ?? "The server could not page this table just now. The download is the complete original." : "The table could not be read.";
            return;
          }
          mode = "whole";
          sort = null;
        }
      }
      if (ctx.file.size_bytes > MAX_TABLE_BYTES) { failure = "This table is too large to show here; the download is the complete original."; return; }
      const read = tableRead(await strictText(await ctx.read(c.signal), MAX_TABLE_BYTES),
        ctx.file.path.toLowerCase().endsWith(".tsv") || ctx.file.media_type.startsWith("text/tab-separated") ? "\t" : ",");
      if (c.signal.aborted) return;
      columns = read.columns;
      types = [];
      shortened = read.totalRows > read.rows.length;
      total = read.totalRows;
      whole = read.rows.map((values, index) => ({ index, line: null, values }));
      failure = "";
    } catch {
      if (!c.signal.aborted) failure = "This table could not be read without changing its structure; download the original.";
    } finally {
      if (controller === c) busy = false;
    }
  }
  $effect(() => { void offset; void sort; void limit; void load(); });
  $effect(() => { if (mode === "whole") ctx.keep({ offset, sort: null }); });
  onDestroy(() => controller?.abort());

  function sortBy(column: string) {
    sort = sort?.column === column ? (sort.order === "asc" ? { column, order: "desc" } : null) : { column, order: "asc" };
    offset = 0;
  }
  function pick(row: Row) {
    picked = row.index;
    const location: Location = { group: null, file: ctx.file.path, row: row.index, line: row.line, values: columns.map((name, i) => [name, row.values[i]]) };
    ctx.pick(location);
  }
  // A pick from another view of the same numbers: mark the row and turn to its page (in file order).
  const stop = ctx.onPick((location) => {
    if (!location || location.file !== ctx.file.path) { picked = null; return; }
    picked = location.row;
    if (untrack(() => rows).some((r) => r.index === location.row)) return;
    sort = null;
    filter = "";
    offset = Math.floor(location.row / limit) * limit;
  });
  onDestroy(stop);

  async function openSummary() {
    if (summaryState === "open") { summaryState = "closed"; return; }
    if (summary) { summaryState = "open"; return; }
    if (!ctx.summary) return;
    summaryState = "reading";
    try {
      // Shown only when the receipt names this file's bytes and its output digest is the summaries' own.
      const read = await ctx.summary();
      const kept = read.sha256 === ctx.file.sha256 && read.receipt?.input_sha256 === ctx.file.sha256
        && (await sha256Hex(canonicalJson(read.columns))) === read.receipt.output_sha256;
      summary = kept ? read : null;
      summaryState = kept ? "open" : "failed";
    } catch (cause) {
      summaryState = cause instanceof BrokerError && cause.reason === "summary_too_large" ? "too_large" : "failed";
    }
  }
  const sortWord = (column: string) => sort?.column !== column ? "none" : sort.order === "asc" ? "ascending" : "descending";
</script>

<section class="values" aria-label={`${ctx.title}, values`}>
  <div class="bar">
    <p class="facts">
      {#if failure}{failure}
      {:else if !columns.length}Reading the table…
      {:else}{total.toLocaleString()} {total === 1 ? "row" : "rows"}, {columns.length} {columns.length === 1 ? "column" : "columns"}{shortened ? `; the first ${(whole?.length ?? 0).toLocaleString()} are shown and the download has every row` : ""}.
        {mode === "server" ? "Pages come from the stored file; its checksum matches." : "Read whole; its checksum matches."}{/if}
    </p>
    <div class="tools">
      {#if mode === "whole" && whole && whole.length > 12}<input type="search" aria-label={`Find rows in ${ctx.title}`} placeholder="Find rows" bind:value={filter} oninput={() => (offset = 0)} />{/if}
      {#if mode === "server" && ctx.summary && columns.length}<button class="link small" aria-expanded={summaryState === "open"} onclick={openSummary}>Columns</button>{/if}
      <button class="link small" onclick={ctx.download}>Download</button>
    </div>
  </div>
  {#if summaryState === "reading"}<p class="small soft">Reading the column summaries…</p>
  {:else if summaryState === "failed"}<p class="small soft">The column summaries could not be read for this file, or did not match their receipt, so they are not shown.</p>
  {:else if summaryState === "too_large"}<p class="small soft">This table has too many columns to summarize here. The download is the complete original.</p>
  {:else if summaryState === "open" && summary}
    <div class="scroll summary">
      <table>
        <caption class="small soft">Column summaries by {summary.receipt.routine} version {summary.receipt.version}, read from the stored file{summary.columns.some((c) => c.type_source === "inferred") ? "; types are inferred from every stored value" : ""}.</caption>
        <thead><tr><th scope="col">Column</th><th scope="col">Type</th><th scope="col" class="n">Values</th><th scope="col" class="n">Missing</th><th scope="col">Lowest</th><th scope="col">Highest</th><th scope="col" class="n">Distinct</th></tr></thead>
        <tbody>{#each summary.columns as c (c.name)}<tr><th scope="row">{c.name}</th><td>{c.type}</td><td class="n">{c.count.toLocaleString()}</td><td class="n">{c.missing.toLocaleString()}</td><td class="v">{c.min ?? "None"}</td><td class="v">{c.max ?? "None"}</td><td class="n">{c.distinct.toLocaleString()}</td></tr>{/each}</tbody>
      </table>
    </div>
  {/if}
  {#if columns.length && !failure}
    <!-- svelte-ignore a11y_no_noninteractive_tabindex -->
    <div class="scroll" role="region" aria-label={`${ctx.title}, scroll for more`} tabindex="0">
      <table class:busy={busy && mode === "server"}>
        <thead>
          <tr>
            <th scope="col" class="row-head">Row</th>
            {#each columns as column, c (column)}
              <th scope="col" class:n={numeric(c)} aria-sort={mode === "server" ? sortWord(column) : undefined}>
                {#if mode === "server"}<button class="sort" onclick={() => sortBy(column)}>{column}<span class="arrow" aria-hidden="true">{sort?.column === column ? (sort.order === "asc" ? "↑" : "↓") : ""}</span></button>
                {:else}{column}{/if}
              </th>
            {/each}
          </tr>
        </thead>
        <tbody>
          {#each rows as row (row.index)}
            <!-- A pointer picks anywhere on the row; the keyboard picks with the row number. -->
            <!-- svelte-ignore a11y_click_events_have_key_events, a11y_no_noninteractive_element_interactions -->
            <tr class:picked={picked === row.index} onclick={(event) => { if (!(event.target as HTMLElement).closest("button")) pick(row); }}>
              <th scope="row" class="row-head"><button class="row-pick" aria-pressed={picked === row.index} aria-label={`Pick row ${(row.index + 1).toLocaleString()}${row.line ? `, line ${row.line}` : ""}`} onclick={() => pick(row)}>{(row.index + 1).toLocaleString()}</button></th>
              {#each row.values as value, c (c)}<td class:n={numeric(c)}>{value}</td>{/each}
            </tr>
          {/each}
        </tbody>
      </table>
      {#if !rows.length}<p class="soft small empty">No rows match.</p>{/if}
    </div>
    {#if count > limit}
      <nav class="pager" aria-label="Pages of rows">
        <button class="button secondary compact" disabled={offset === 0 || (busy && mode === "server")} onclick={() => (offset = Math.max(0, offset - limit))}>Previous</button>
        <span class="where">Rows {(offset + 1).toLocaleString()} to {Math.min(count, offset + limit).toLocaleString()} of {count.toLocaleString()}</span>
        <button class="button secondary compact" disabled={offset + limit >= count || (busy && mode === "server")} onclick={() => (offset += limit)}>Next</button>
      </nav>
    {/if}
  {/if}
</section>

<style>
  .values { display: grid; gap: 10px; min-width: 0; }
  .bar { display: flex; flex-wrap: wrap; align-items: center; gap: 6px 16px; }
  .facts { flex: 1 1 32ch; font-size: var(--font-small); color: var(--text-2); }
  .tools { display: flex; flex-wrap: wrap; align-items: center; gap: 8px 16px; }
  .tools .link { min-height: 36px; }
  .tools input { width: min(220px, 100%); min-height: 36px; }
  /* Publication table, as the results tables are drawn: a heavy rule above and below, a light rule under the header,
     no box. The row number and its column stay in place while the values scroll. */
  .scroll { overflow: auto; max-height: min(62vh, 640px); }
  .scroll:focus-visible { outline: 2px solid var(--focus); outline-offset: 2px; }
  .summary { max-height: none; }
  table { border-collapse: separate; border-spacing: 0; width: max-content; min-width: 100%; font-size: var(--font-small); font-variant-numeric: tabular-nums; }
  table.busy tbody { opacity: 0.6; }
  caption { text-align: left; padding: 0 0 8px; caption-side: top; }
  th, td { padding: 4px 16px 4px 0; text-align: left; white-space: nowrap; }
  th:last-child, td:last-child { padding-right: 2px; }
  thead th { position: sticky; top: 0; z-index: 1; background: var(--canvas); color: var(--heading); font-weight: 650; border-top: 2px solid var(--heading); border-bottom: 1px solid var(--heading); padding-top: 6px; }
  tbody tr:last-child > * { border-bottom: 2px solid var(--heading); }
  tbody tr { cursor: pointer; }
  tbody tr:hover > * { background: var(--hover-wash); }
  .n { text-align: right; }
  .v { font-family: var(--mono); font-size: calc(12.5px * var(--type-scale)); }
  .sort { display: inline-flex; align-items: center; gap: 4px; min-height: 30px; font: inherit; color: inherit; font-weight: 650; }
  .n .sort { flex-direction: row-reverse; }
  .sort:hover { text-decoration: underline; text-decoration-color: var(--mark); text-underline-offset: 3px; }
  .arrow { width: 1ch; color: var(--text-2); }
  .row-head { position: sticky; left: 0; background: var(--canvas); color: var(--text-3); font-weight: 400; text-align: right; width: 1%; padding-right: 14px; }
  thead .row-head { z-index: 2; }
  .row-pick { min-width: 2.5ch; min-height: 26px; font: inherit; color: inherit; text-align: right; }
  .row-pick:hover { color: var(--heading); text-decoration: underline; text-underline-offset: 3px; }
  tr.picked > * { background: var(--surface-2); }
  tr.picked .row-head { box-shadow: inset 3px 0 0 var(--mark); }
  tr.picked .row-pick { color: var(--heading); font-weight: 650; }
  .empty { padding: 12px; }
  .pager { display: flex; flex-wrap: wrap; align-items: center; gap: 8px 12px; }
  .where { font-size: var(--font-small); color: var(--text-2); font-variant-numeric: tabular-nums; }
</style>
