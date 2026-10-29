<script>
  import { onDestroy, onMount } from 'svelte';
  import { searchDashboard } from '../lib/api.js';
  import { time, title } from '../lib/format.js';

  let { onOpen = () => {} } = $props();

  let root;
  let input;
  let query = $state('');
  let page = $state(null);
  let loading = $state(false);
  let error = $state('');
  let expanded = $state(false);
  let activeIndex = $state(-1);
  let request;
  let debounce;
  let shortcut = $state('Ctrl K');

  $effect(() => {
    const current = query.trim();
    window.clearTimeout(debounce);
    request?.abort();
    if (current.length < 2) {
      page = null;
      error = '';
      loading = false;
      activeIndex = -1;
      return;
    }
    loading = true;
    expanded = true;
    debounce = window.setTimeout(() => void runSearch(current), 180);
    return () => window.clearTimeout(debounce);
  });

  onMount(() => {
    shortcut = /Mac|iPhone|iPad/u.test(navigator.platform) ? '⌘K' : 'Ctrl K';
    const keydown = (event) => {
      if ((event.metaKey || event.ctrlKey) && event.key.toLowerCase() === 'k') {
        event.preventDefault();
        input?.focus();
        expanded = true;
      } else if (event.key === 'Escape' && document.activeElement === input) {
        query = '';
        expanded = false;
        input.blur();
      }
    };
    const pointerdown = (event) => {
      if (root && !root.contains(event.target)) expanded = false;
    };
    document.addEventListener('keydown', keydown);
    document.addEventListener('pointerdown', pointerdown);
    return () => {
      document.removeEventListener('keydown', keydown);
      document.removeEventListener('pointerdown', pointerdown);
    };
  });

  onDestroy(() => {
    request?.abort();
    window.clearTimeout(debounce);
  });

  async function runSearch(current) {
    request = new AbortController();
    try {
      const result = await searchDashboard(current, 'all', 40, 0, request.signal);
      if (query.trim() !== current) return;
      page = result;
      error = '';
      activeIndex = result.results.length ? 0 : -1;
    } catch (caught) {
      if (caught.name === 'AbortError') return;
      error = caught.message;
      page = null;
      activeIndex = -1;
    } finally {
      if (query.trim() === current) loading = false;
    }
  }

  function choose(result) {
    onOpen(result);
    expanded = false;
    query = '';
  }

  function handleInputKey(event) {
    const results = page?.results || [];
    if (!expanded || !results.length) return;
    if (event.key === 'ArrowDown') {
      event.preventDefault();
      activeIndex = Math.min(results.length - 1, activeIndex + 1);
    } else if (event.key === 'ArrowUp') {
      event.preventDefault();
      activeIndex = Math.max(0, activeIndex - 1);
    } else if (event.key === 'Enter' && activeIndex >= 0) {
      event.preventDefault();
      choose(results[activeIndex]);
    }
  }
</script>

<div class="global-search" bind:this={root}>
  <label class="search-input">
    <span class="visually-hidden">Search controller records</span>
    <span class="search-glyph" aria-hidden="true"></span>
    <input
      bind:this={input}
      bind:value={query}
      type="search"
      role="combobox"
      aria-autocomplete="list"
      placeholder="Search agents, runs, evidence, audit"
      autocomplete="off"
      aria-expanded={expanded && query.trim().length >= 2}
      aria-controls="controller-search-results"
      aria-activedescendant={activeIndex >= 0 ? `controller-search-${activeIndex}` : undefined}
      onfocus={() => (expanded = query.trim().length >= 2)}
      onkeydown={handleInputKey}
    >
    <kbd>{shortcut}</kbd>
  </label>

  {#if expanded && query.trim().length >= 2}
    <section class="search-results" id="controller-search-results" aria-label="Controller search results">
      <div class="search-summary" aria-live="polite">
        {#if loading}
          <span>Searching metadata</span>
        {:else if error}
          <span>Search unavailable: {error}</span>
        {:else if page}
          <span>{page.total} {page.total === 1 ? 'record' : 'records'}</span>
          {#if page.total > page.results.length}<small>Showing the newest {page.results.length}. Open a section to page further.</small>{/if}
        {/if}
      </div>
      <div class="search-result-list" role="listbox">
        {#each page?.results || [] as result, index}
          <button
            id={`controller-search-${index}`}
            class:active={index === activeIndex}
            type="button"
            role="option"
            aria-selected={index === activeIndex}
            onclick={() => choose(result)}
            onpointerenter={() => (activeIndex = index)}
          >
            <span class="result-scope">{title(result.scope)}</span>
            <strong>{result.title}</strong>
            {#if result.subtitle}<span class="result-subtitle">{result.subtitle}</span>{/if}
            <span class="result-meta">
              <span>{title(result.kind)}</span>
              {#if result.status}<span>{title(result.status)}</span>{/if}
              {#if result.occurred_at}<time>{time(result.occurred_at)}</time>{/if}
            </span>
          </button>
        {:else}
          {#if page && !loading}<p class="search-empty">No metadata matched this search.</p>{/if}
        {/each}
      </div>
    </section>
  {/if}
</div>
