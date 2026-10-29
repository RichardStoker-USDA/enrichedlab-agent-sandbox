<script>
  import { onMount } from 'svelte';
  import AuditView from './components/AuditView.svelte';
  import GlobalSearch from './components/GlobalSearch.svelte';
  import OperationsView from './components/OperationsView.svelte';
  import ProvenanceExplorer from './components/ProvenanceExplorer.svelte';
  import { loadDashboard, stateStreamUrl } from './lib/api.js';

  const navigation = [
    ['overview', 'Overview'],
    ['provenance', 'Provenance'],
    ['research', 'Research'],
    ['runtime', 'Runtime'],
    ['access', 'Access'],
    ['audit', 'Audit']
  ];

  let view = $state('overview');
  let data = $state(null);
  let loading = $state(false);
  let loadMessage = $state('Connecting to the local controller');
  let actionStatus = $state('');
  let updated = $state(null);
  let observed = $state(null);
  let streamState = $state('connecting');
  let streamRevision = $state(0);
  let provenanceRun = $state('');
  let provenanceTarget = $state('');
  let theme = $state('light');
  let source;
  let staleTimer;
  let statusTimer;
  let refreshQueued = false;

  onMount(() => {
    const stored = localStorage.getItem('enrichedlab-agent-sandbox-theme');
    theme = stored === 'dark' || stored === 'light'
      ? stored
      : matchMedia('(prefers-color-scheme: dark)').matches ? 'dark' : 'light';
    document.documentElement.dataset.theme = theme;
    void refresh();
    connectStateStream();
    staleTimer = window.setInterval(() => {
      if (
        observed &&
        Date.now() - observed.getTime() > 9000 &&
        ['live', 'connecting', 'reconnecting'].includes(streamState)
      ) {
        streamState = 'stale';
      }
    }, 2000);
    return () => {
      source?.close();
      window.clearInterval(staleTimer);
      window.clearTimeout(statusTimer);
    };
  });

  async function refresh() {
    if (loading) {
      refreshQueued = true;
      return;
    }
    loading = true;
    try {
      const next = await loadDashboard();
      data = next;
      updated = new Date(Number(next.generated_at_unix) * 1000);
      streamRevision = Number(next.live_revision || streamRevision);
      loadMessage = 'Controller state loaded';
    } catch (error) {
      loadMessage = `Controller unavailable: ${error.message}`;
      if (!data) streamState = 'unavailable';
    } finally {
      loading = false;
      if (refreshQueued) {
        refreshQueued = false;
        void refresh();
      }
    }
  }

  function connectStateStream() {
    source?.close();
    streamState = 'connecting';
    source = new EventSource(stateStreamUrl());
    source.addEventListener('state', (event) => {
      try {
        const payload = JSON.parse(event.data);
        if (!payload.ok) throw new Error('state feed rejected');
        const state = payload.data;
        observed = new Date(Number(state.observed_at_unix) * 1000);
        streamState = state.healthy ? 'live' : 'stale';
        if (Number(state.revision) !== streamRevision) {
          streamRevision = Number(state.revision);
          void refresh();
        }
      } catch {
        streamState = 'unavailable';
      }
    });
    source.addEventListener('error', () => {
      if (streamState !== 'stale') streamState = 'reconnecting';
    });
  }

  function selectView(next) {
    view = next;
    window.scrollTo({ top: 0, behavior: 'smooth' });
  }

  function openRun(run, target = '') {
    provenanceRun = run;
    provenanceTarget = target;
    selectView('provenance');
  }

  function openSearchResult(result) {
    if (result.view === 'provenance' && result.run_id) {
      openRun(result.run_id, result.target_id || '');
      return;
    }
    selectView(result.view || 'overview');
  }

  function showStatus(value) {
    actionStatus = value;
    window.clearTimeout(statusTimer);
    statusTimer = window.setTimeout(() => (actionStatus = ''), 6000);
  }

  function toggleTheme() {
    theme = theme === 'dark' ? 'light' : 'dark';
    document.documentElement.dataset.theme = theme;
    localStorage.setItem('enrichedlab-agent-sandbox-theme', theme);
  }

  function navigationCount(id) {
    if (id === 'access') return data?.requests?.length || 0;
    return 0;
  }
</script>

<svelte:head><meta name="theme-color" content={theme === 'dark' ? '#07111e' : '#022851'}></svelte:head>

<a class="skip-link" href="#main">Skip to dashboard</a>
<div class="app-shell">
  <aside class="sidebar">
    <div class="brand-lockup">
      <span class="brand-mark" aria-hidden="true"><i></i></span>
      <div><p>EnrichedLab</p><strong>Agent Sandbox</strong></div>
    </div>
    <nav aria-label="Dashboard">
      {#each navigation as [id, label]}
        <button class:active={view === id} type="button" onclick={() => selectView(id)}>
          <span class="nav-marker" aria-hidden="true"></span>
          <span>{label}</span>
          {#if navigationCount(id)}<strong class="nav-count" aria-label={`${navigationCount(id)} pending`}>{navigationCount(id)}</strong>{/if}
        </button>
      {/each}
    </nav>
    <div class="sidebar-foot">
      <span class="controller-dot" data-state={streamState}></span>
      <div><strong>{streamState === 'live' ? 'Controller connected' : 'Controller reconnecting'}</strong><small>Evidence stays local by default</small></div>
    </div>
  </aside>

  <main id="main" tabindex="-1">
    <header class="topbar">
      <div class="view-title"><p class="eyebrow">Operator workspace</p><strong>{navigation.find(([id]) => id === view)?.[1]}</strong></div>
      <GlobalSearch onOpen={openSearchResult} />
      <div class="top-actions">
        <div class="connection-state" data-state={streamState} role="status">
          <span></span>
          <div><strong>{streamState}</strong><small>{observed ? `Observed ${observed.toLocaleTimeString()}` : updated ? `Loaded ${updated.toLocaleTimeString()}` : 'Waiting for state'}</small></div>
        </div>
        {#if ['stale', 'unavailable'].includes(streamState)}
          <button class="quiet-button" type="button" onclick={() => { connectStateStream(); void refresh(); }}>Reconnect</button>
        {/if}
        <button class="quiet-button theme-toggle" type="button" onclick={toggleTheme}>{theme === 'dark' ? 'Light' : 'Dark'}</button>
      </div>
    </header>
    <div class="main-content">
      {#if actionStatus}<div class="operation-status" role="status">{actionStatus}</div>{/if}
      {#if data}
        {#if view === 'provenance'}
          <ProvenanceExplorer runs={data.agent_state || []} initialRun={provenanceRun} initialTarget={provenanceTarget} {theme} />
        {:else if view === 'audit'}
          <AuditView chain={data.audit_chain} />
        {:else}
          <OperationsView {data} {view} onRefresh={refresh} onOpenRun={openRun} onStatus={showStatus} />
        {/if}
      {:else}
        <section class="loading-surface"><span class="loading-mark"></span><h2>Waiting for the controller</h2><p>{loadMessage}</p></section>
      {/if}
    </div>
  </main>
</div>
