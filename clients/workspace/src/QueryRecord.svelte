<script lang="ts">
  import type { TaskQuery } from "./contracts.ts";
  import { when } from "./language.ts";
  import { capAdded, newestReceipt, resultSentence, versionWords } from "./query.ts";
  import StatusMark from "./StatusMark.svelte";
  import TextView from "./TextView.svelte";

  // A database query the research agent wrote, shown with what was checked before it ran. The text is
  // the broker's stored record and is shown only after its digest matches; the receipt is the worker's record of the
  // attempt. No code ran: the database answered the query in a read-only transaction.
  let { query, onDownload }: { query: TaskQuery; onDownload: () => void } = $props();

  let verified = $state<boolean | null>(null);
  const short = (digest: string) => digest.replace(/^sha256:/, "").slice(0, 12);
  const receipt = $derived(newestReceipt(query));
  const manifest = $derived(typeof query.snapshot.manifest_sha256 === "string" ? query.snapshot.manifest_sha256 : null);
  const tables = $derived(query.relations.join(", "));

  async function digest(text: string) {
    const bytes = new TextEncoder().encode(text);
    return [...new Uint8Array(await crypto.subtle.digest("SHA-256", bytes))].map((b) => b.toString(16).padStart(2, "0")).join("");
  }
  $effect(() => {
    let live = true;
    verified = null;
    void digest(query.admitted_sql).then((hex) => { if (live) verified = `sha256:${hex}` === query.sql_sha256 && query.admitted_sql.length > 0; });
    return () => { live = false; };
  });
</script>

<div class="query">
  <p class="abstract">
    The research agent wrote this query. It read {query.title} ({query.release}) without changing it, and the table
    under Results is what the database returned. No code ran.
    <strong>Treat the results as unvalidated until you have read the query.</strong>
  </p>
  <p class="source small">{query.description}</p>

  <div class="checks">
    <h3>Checked before it ran</h3>
    <ul>
      <li><StatusMark tone="done" label="Admitted by the platform's query check" />
        <span class="detail">One read-only query of {query.relations.length === 1 ? "one table" : `${query.relations.length} tables`} ({tables}), at most {query.row_limit} rows, with no values typed in as results.</span></li>
      {#if capAdded(query)}<li class="detail">The row limit was added to the agent's query. The only other change the platform makes is to write a value rounded to a number of decimal places as an exact decimal, which PostgreSQL needs for that rounding.</li>{/if}
    </ul>
    <h3>How it ran</h3>
    <ul>
      <li><StatusMark tone="done" label="Read only, in one consistent view of the database" /></li>
      {#if manifest}<li><StatusMark tone="done" label="The database held exactly the recorded release of this source" />
        <span class="detail">Snapshot {short(manifest)}</span></li>{/if}
      {#if receipt?.source_version}<li><StatusMark tone="done" label="Read the live lab database at the version it stated" />
        <span class="detail">{versionWords(receipt.source_version)}</span></li>{/if}
    </ul>
    <p class="outcome"><span class="lead">Outcome.</span> {resultSentence(query)}</p>
  </div>

  {#if verified === false}
    <p class="refused">The stored query does not match its recorded checksum, so it is not shown.</p>
  {:else if verified}
    <TextView file={{ label: "The query", path: "query.sql", media_type: "text/plain", size_bytes: new TextEncoder().encode(query.admitted_sql).length, sha256: query.sql_sha256 }}
      load={async () => new Blob([query.admitted_sql], { type: "text/plain" })} {onDownload} title="The query" code showTitle={false} />
  {/if}

  <dl class="record small">
    <dt>Query checksum</dt><dd title={query.sql_sha256}>{short(query.sql_sha256)}</dd>
    <dt>Written</dt><dd>{when(query.created_at)}{#if query.turn}, research decision {query.turn}{/if}</dd>
    {#if receipt?.server_version}<dt>Database</dt><dd>PostgreSQL {receipt.server_version}</dd>{/if}
    <dt>Check</dt><dd>{query.guard}</dd>
  </dl>
</div>

<style>
  .query { display: grid; gap: 20px; max-width: 920px; }
  .abstract { font-family: var(--serif); font-size: var(--font-lede); line-height: 1.55; max-width: 66ch; }
  .abstract strong { font-weight: 600; color: var(--heading); }
  .source { color: var(--text-2); max-width: 72ch; }
  .checks { display: grid; gap: 10px; padding-top: 14px; border-top: 2px solid var(--heading); }
  h3 { font-size: calc(15px * var(--type-scale)); color: var(--heading); }
  ul { list-style: none; margin: 0; padding: 0; display: grid; gap: 8px; }
  li { display: grid; gap: 2px; min-width: 0; }
  .checks :global(.status) { white-space: normal; align-items: flex-start; }
  .checks :global(.status .glyph) { margin-top: 0.3em; }
  .detail { font-size: var(--font-small); color: var(--text-2); padding-left: 22px; }
  li.detail { padding-left: 0; }
  .outcome { font-size: var(--font-small); }
  .lead { font-weight: 700; color: var(--heading); }
  .refused { padding-left: 10px; border-left: 3px solid var(--failed); }
  .record { display: grid; grid-template-columns: max-content minmax(0, 1fr); gap: 4px 16px; padding-top: 10px; border-top: 1px solid var(--line); }
  .record dt { color: var(--text-3); }
  .record dd { margin: 0; font-family: var(--mono); font-size: 0.95em; overflow-wrap: anywhere; }
  @media (max-width: 640px) { .record { grid-template-columns: 1fr; } .record dd { padding-bottom: 6px; } }
</style>
