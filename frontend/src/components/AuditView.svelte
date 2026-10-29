<script>
  import { onDestroy } from 'svelte';
  import { searchDashboard } from '../lib/api.js';
  import { time, title } from '../lib/format.js';

  let { chain = null } = $props();

  const limit = 50;
  let query = $state('');
  let page = $state(null);
  let cursor = $state(0);
  let loading = $state(false);
  let error = $state('');
  let request;
  let debounce;

  $effect(() => {
    const current = query.trim();
    cursor = 0;
    window.clearTimeout(debounce);
    debounce = window.setTimeout(() => void load(current, 0), 180);
    return () => window.clearTimeout(debounce);
  });

  onDestroy(() => {
    request?.abort();
    window.clearTimeout(debounce);
  });

  async function load(currentQuery, nextCursor) {
    request?.abort();
    request = new AbortController();
    loading = true;
    try {
      page = await searchDashboard(currentQuery, 'audit', limit, nextCursor, request.signal);
      cursor = nextCursor;
      error = '';
    } catch (caught) {
      if (caught.name !== 'AbortError') error = caught.message;
    } finally {
      loading = false;
    }
  }
</script>

<header class="page-heading audit-heading">
  <div>
    <p class="eyebrow">Host record</p>
    <h2>Audit ledger</h2>
    <p class="secondary">Paged, metadata-only access to the verified controller chain.</p>
  </div>
  <div class="integrity-readout" data-valid={chain?.valid ? 'true' : 'false'}>
    <strong>{chain?.valid ? 'Chain verified' : 'Verification failed'}</strong>
    <span>{chain?.valid ? `${chain.records} records` : chain?.error || 'No verification receipt'}</span>
  </div>
</header>

<section class="surface-section ledger-surface">
  <div class="ledger-tools">
    <label class="ledger-search">
      <span>Search audit metadata</span>
      <input bind:value={query} type="search" placeholder="actor, action, outcome, resource">
    </label>
    <div class="ledger-page" aria-live="polite">
      {#if page}
        <strong>{page.total}</strong><span>matching records</span>
      {:else}
        <span>Loading ledger</span>
      {/if}
    </div>
  </div>

  {#if error}
    <p class="inline-error" role="status">Audit search unavailable: {error}</p>
  {/if}

  <div class="table-wrap ledger-table" aria-busy={loading}>
    <table>
      <thead><tr><th>Sequence</th><th>Time</th><th>Actor</th><th>Action</th><th>Outcome</th><th>Resource</th></tr></thead>
      <tbody>
        {#each page?.results || [] as event}
          <tr>
            <td><code>{event.id.replace('audit:', '')}</code></td>
            <td><time>{time(event.occurred_at)}</time></td>
            <td><span class="bounded-cell" title={event.subtitle || ''}>{event.subtitle || '-'}</span></td>
            <td><strong class="bounded-cell" title={event.title}>{title(event.title)}</strong></td>
            <td><span class="ledger-outcome" data-outcome={event.status}>{event.status || '-'}</span></td>
            <td><code class="bounded-cell" title={event.detail || ''}>{event.detail || '-'}</code></td>
          </tr>
        {:else}
          {#if !loading}<tr><td colspan="6" class="empty-cell">No audit records matched.</td></tr>{/if}
        {/each}
      </tbody>
    </table>
  </div>

  <footer class="pagination">
    <button class="quiet-button" type="button" disabled={cursor === 0 || loading} onclick={() => load(query.trim(), Math.max(0, cursor - limit))}>Previous</button>
    <span>{page?.total ? `${cursor + 1}–${Math.min(cursor + limit, page.total)} of ${page.total}` : '0 records'}</span>
    <button class="quiet-button" type="button" disabled={!page?.next_cursor || loading} onclick={() => load(query.trim(), page.next_cursor)}>Next</button>
  </footer>
</section>
