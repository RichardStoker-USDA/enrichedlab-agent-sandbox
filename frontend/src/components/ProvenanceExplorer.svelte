<script>
  import { onDestroy, onMount } from 'svelte';
  import { Background, Controls, MiniMap, Panel, SvelteFlow } from '@xyflow/svelte';
  import '@xyflow/svelte/dist/style.css';

  import ProvenanceLane from './ProvenanceLane.svelte';
  import ProvenanceNode from './ProvenanceNode.svelte';
  import {
    downloadArtifact,
    graphStreamUrl,
    issueEvidenceLease,
    loadEventEvidence,
    loadGraph,
    loadProvenanceStatus,
    searchEvidence
  } from '../lib/api.js';
  import { bytes, short, time, title } from '../lib/format.js';

  let { runs = [], initialRun = '', initialTarget = '', theme = 'light' } = $props();

  const nodeTypes = { lane: ProvenanceLane, provenance: ProvenanceNode };
  const lanes = [
    { id: 'lifecycle', label: 'Lifecycle', description: 'Sessions and checkpoints' },
    { id: 'reasoning', label: 'Reasoning', description: 'Provider-returned review evidence' },
    { id: 'output', label: 'Output', description: 'Assistant results' },
    { id: 'tool-io', label: 'Tool I/O', description: 'Calls and returned data' },
    { id: 'usage', label: 'Usage', description: 'Tokens and context pressure' },
    { id: 'state-transition', label: 'State', description: 'Durable memory changes' },
    { id: 'artifact', label: 'Artifacts', description: 'Content-addressed outputs' }
  ];
  const laneOrder = lanes.map((lane) => lane.id);

  let selectedRun = $state('');
  let lastInitialRun = $state('');
  let lastInitialTarget = $state('');
  let graph = $state(null);
  let storage = $state(null);
  let cursor = $state(0);
  let baselineCursor = $state(null);
  let selectedNode = $state(null);
  let selectedArtifact = $state(null);
  let evidence = $state(null);
  let evidenceQuery = $state('');
  let evidenceMatches = $state(null);
  let evidenceSearchLoading = $state(false);
  let lease = $state(null);
  let leasePurpose = $state('Review scientific provenance for validation');
  let filterRole = $state('all');
  let filterHarness = $state('all');
  let filterActor = $state('all');
  let search = $state('');
  let liveState = $state('idle');
  let message = $state('Select a run to load its provenance.');
  let source = null;

  let availableRuns = $derived.by(() => {
    const merged = new Map();
    for (const run of storage?.runs || []) merged.set(run.run_id, run);
    for (const run of runs) {
      merged.set(run.run_id, { ...(merged.get(run.run_id) || {}), ...run });
    }
    return [...merged.values()].sort((left, right) =>
      String(right.updated_at || '').localeCompare(String(left.updated_at || '')) ||
      left.run_id.localeCompare(right.run_id)
    );
  });
  let chronological = $derived.by(() =>
    [...(graph?.nodes || [])].sort((left, right) =>
      left.occurred_at.localeCompare(right.occurred_at) || left.id.localeCompare(right.id)
    )
  );
  let roles = $derived([...new Set(chronological.map((node) => node.evidence_role))].sort());
  let harnesses = $derived([...new Set(chronological.map((node) => node.harness))].sort());
  let actors = $derived([...new Set(chronological.map((node) => node.actor))].sort());
  let cursorNode = $derived(cursor > 0 ? chronological[Math.min(cursor, chronological.length) - 1] : null);
  let visible = $derived.by(() => {
    const term = search.trim().toLowerCase();
    return chronological.slice(0, cursor).filter((node) => {
      if (filterRole !== 'all' && node.evidence_role !== filterRole) return false;
      if (filterHarness !== 'all' && node.harness !== filterHarness) return false;
      if (filterActor !== 'all' && node.actor !== filterActor) return false;
      if (!term) return true;
      return [node.kind, node.evidence_role, node.actor, node.harness, node.model, node.engine]
        .filter(Boolean)
        .some((value) => String(value).toLowerCase().includes(term));
    });
  });
  let comparison = $derived.by(() => {
    if (baselineCursor === null) return null;
    const from = Math.min(baselineCursor, cursor);
    const to = Math.max(baselineCursor, cursor);
    const nodes = chronological.slice(from, to);
    return {
      events: nodes.length,
      tools: nodes.filter((node) => node.evidence_role === 'tool-io').length,
      reasoning: nodes.filter((node) => node.evidence_role === 'reasoning').length,
      artifacts: (graph?.artifacts || []).filter((artifact) => {
        const eventIndex = chronological.findIndex((node) => node.id === `state:${artifact.event_id}`);
        return eventIndex >= from && eventIndex < to;
      }).length
    };
  });
  let flow = $derived.by(() => buildFlow(visible, graph, selectedNode?.id));

  $effect(() => {
    const ids = availableRuns.map((run) => run.run_id);
    if (initialRun && initialRun !== lastInitialRun && ids.includes(initialRun)) {
      lastInitialRun = initialRun;
      void selectRun(initialRun);
    } else if ((!selectedRun || !ids.includes(selectedRun)) && ids.length) {
      void selectRun(ids[0]);
    }
  });

  $effect(() => {
    if (!graph || !initialTarget || !selectedRun) return;
    const request = `${selectedRun}:${initialTarget}`;
    if (request === lastInitialTarget) return;
    lastInitialTarget = request;
    selectTarget(initialTarget, true);
  });

  onMount(async () => {
    try {
      storage = await loadProvenanceStatus();
    } catch (error) {
      message = `Storage status unavailable: ${error.message}`;
    }
  });

  onDestroy(() => source?.close());

  async function selectRun(run) {
    if (!run) return;
    source?.close();
    selectedRun = run;
    graph = null;
    cursor = 0;
    baselineCursor = null;
    selectedNode = null;
    selectedArtifact = null;
    evidence = null;
    evidenceQuery = '';
    evidenceMatches = null;
    lease = null;
    liveState = 'loading';
    message = 'Loading provenance graph';
    try {
      const loaded = await loadGraph(run);
      acceptGraph(loaded, true);
      connectStream(run);
    } catch (error) {
      liveState = 'unavailable';
      message = error.message;
    }
  }

  function acceptGraph(next, moveToLatest = false) {
    const wasLatest = graph === null || cursor >= (graph?.nodes?.length || 0);
    graph = next;
    if (moveToLatest || wasLatest) cursor = next.nodes.length;
    liveState = 'live';
    message = next.truncated
      ? 'The API limit was reached. Narrow the run or export its full package.'
      : `Live metadata through ${time(next.generated_at)}`;
  }

  function connectStream(run) {
    source?.close();
    source = new EventSource(graphStreamUrl(run));
    source.addEventListener('graph', (event) => {
      try {
        const payload = JSON.parse(event.data);
        if (payload.ok) acceptGraph(payload.data);
      } catch {
        liveState = 'unavailable';
      }
    });
    source.addEventListener('error', () => {
      liveState = 'reconnecting';
    });
  }

  function buildFlow(nodes, graphValue, selectedId) {
    const limited = nodes.length > 700 ? nodes.slice(nodes.length - 700) : nodes;
    const positions = new Map();
    const laneSpan = Math.max(720, limited.length * 238 + 220);
    const flowNodes = lanes.map((lane, index) => ({
      id: `lane:${lane.id}`,
      type: 'lane',
      position: { x: -210, y: index * 148 + 6 },
      draggable: false,
      selectable: false,
      focusable: false,
      zIndex: 0,
      data: { ...lane, span: laneSpan }
    }));
    flowNodes.push(...limited.map((node, index) => {
      const lane = laneIndex(node.evidence_role, node.source);
      const position = { x: index * 238 + 20, y: lane * 148 };
      positions.set(node.id, position);
      return {
        id: node.id,
        type: 'provenance',
        position,
        draggable: false,
        selected: selectedId === node.id,
        zIndex: 1,
        data: {
          label: title(node.kind),
          clock: time(node.occurred_at),
          raw: node
        }
      };
    }));
    for (const artifact of graphValue?.artifacts || []) {
      const sourceId = `state:${artifact.event_id}`;
      const sourcePosition = positions.get(sourceId);
      if (!sourcePosition) continue;
      flowNodes.push({
        id: artifact.id,
        type: 'provenance',
        position: { x: sourcePosition.x + 118, y: laneIndex('artifact', 'artifact') * 148 },
        draggable: false,
        selected: selectedId === artifact.id,
        zIndex: 1,
        data: {
          label: artifact.label,
          clock: time(artifact.created_at),
          raw: {
            ...artifact,
            source: 'artifact',
            kind: 'artifact.version',
            evidence_role: 'artifact',
            actor: 'host-observer',
            harness: 'controller',
            occurred_at: artifact.created_at
          }
        }
      });
    }
    const ids = new Set(flowNodes.map((node) => node.id));
    const flowEdges = (graphValue?.edges || [])
      .filter((edge) => ids.has(edge.source) && ids.has(edge.target))
      .map((edge) => ({
        ...edge,
        type: edge.kind.includes('sequence') ? 'smoothstep' : 'bezier',
        animated: false,
        label: edge.kind === 'artifact' ? 'generated' : undefined
      }));
    return { nodes: flowNodes, edges: flowEdges, clipped: nodes.length > limited.length };
  }

  function laneIndex(role, sourceType) {
    if (sourceType === 'artifact') return laneOrder.indexOf('artifact');
    const normalized = role === 'context' || role === 'session' ? 'lifecycle' : role;
    const index = laneOrder.indexOf(normalized);
    return index >= 0 ? index : laneOrder.indexOf('state-transition');
  }

  function selectFlowNode({ node }) {
    if (node.type === 'lane') return;
    selectedNode = node.data.raw;
    selectedArtifact = node.data.raw.source === 'artifact' ? node.data.raw : null;
    evidence = null;
  }

  function selectTarget(target, fromSearch = false) {
    const targetNode = (graph?.nodes || []).find((node) => node.id === target);
    if (targetNode) {
      const index = chronological.findIndex((node) => node.id === targetNode.id);
      if (index >= 0) cursor = Math.max(cursor, index + 1);
      selectedNode = targetNode;
      selectedArtifact = null;
      evidence = null;
      message = `Selected ${title(targetNode.kind)} at ${time(targetNode.occurred_at)}`;
      return true;
    }
    const artifact = (graph?.artifacts || []).find((item) => item.id === target);
    if (artifact) {
      const sourceIndex = chronological.findIndex((node) => node.id === `state:${artifact.event_id}`);
      if (sourceIndex >= 0) cursor = Math.max(cursor, sourceIndex + 1);
      selectedNode = {
        ...artifact,
        source: 'artifact',
        kind: 'artifact.version',
        evidence_role: 'artifact',
        actor: 'host-observer',
        harness: 'controller',
        occurred_at: artifact.created_at
      };
      selectedArtifact = selectedNode;
      evidence = null;
      message = `Selected artifact ${artifact.label}`;
      return true;
    }
    if (fromSearch) message = 'The search result is outside the loaded graph window.';
    return false;
  }

  async function openLease() {
    if (leasePurpose.trim().length < 3) {
      message = 'Enter a review purpose before opening restricted evidence.';
      return;
    }
    try {
      lease = await issueEvidenceLease(selectedRun, leasePurpose.trim());
      evidenceMatches = null;
      message = `Evidence lease expires ${time(lease.expires_at)}`;
    } catch (error) {
      message = error.message;
    }
  }

  async function runEvidenceSearch(cursorValue = 0) {
    if (!lease || evidenceQuery.trim().length < 2 || evidenceSearchLoading) return;
    evidenceSearchLoading = true;
    try {
      evidenceMatches = await searchEvidence(
        selectedRun,
        evidenceQuery.trim(),
        lease.token,
        20,
        cursorValue
      );
      message = `${evidenceMatches.total} restricted evidence match${evidenceMatches.total === 1 ? '' : 'es'}`;
    } catch (error) {
      message = error.message;
      evidenceMatches = null;
    } finally {
      evidenceSearchLoading = false;
    }
  }

  async function openEvidence() {
    if (!lease || !selectedNode || selectedNode.source === 'artifact') return;
    try {
      evidence = await loadEventEvidence(selectedRun, selectedNode.id, lease.token);
    } catch (error) {
      message = error.message;
      evidence = null;
    }
  }

  async function saveArtifact() {
    if (!lease || !selectedArtifact) return;
    try {
      await downloadArtifact(selectedRun, selectedArtifact, lease.token);
      message = `Verified artifact ${short(selectedArtifact.sha256)} downloaded.`;
    } catch (error) {
      message = error.message;
    }
  }

  function step(amount) {
    cursor = Math.max(0, Math.min(chronological.length, cursor + amount));
    selectedNode = null;
    selectedArtifact = null;
    evidence = null;
  }
</script>

<section class="provenance-head">
  <div>
    <p class="eyebrow">Scientific provenance</p>
    <h2>Provenance replay</h2>
    <p class="secondary">Inspect a run step by step without adding evidence payloads to agent context.</p>
  </div>
  <div class="run-controls">
    <label>
      <span>Recorded run</span>
      <select value={selectedRun} onchange={(event) => selectRun(event.currentTarget.value)}>
        {#each availableRuns as run}
          <option value={run.run_id}>{run.run_id} · {run.subject}</option>
        {/each}
      </select>
    </label>
    <span class="live-state" data-state={liveState}>{liveState}</span>
  </div>
</section>

<div class="status-line" role="status">
  <span>{message}</span>
  {#if storage}
    <span>{storage.backend} · {storage.events} events · chain {storage.chain_valid ? 'verified' : 'failed'}</span>
  {/if}
</div>

{#if graph}
  <section class="provenance-toolbar" aria-label="Provenance filters">
    <label><span>Search metadata</span><input bind:value={search} type="search" placeholder="tool, actor, engine"></label>
    <label><span>Evidence role</span><select bind:value={filterRole}><option value="all">All roles</option>{#each roles as role}<option value={role}>{role}</option>{/each}</select></label>
    <label><span>Harness</span><select bind:value={filterHarness}><option value="all">All harnesses</option>{#each harnesses as harness}<option value={harness}>{harness}</option>{/each}</select></label>
    <label><span>Actor</span><select bind:value={filterActor}><option value="all">All actors</option>{#each actors as actor}<option value={actor}>{actor}</option>{/each}</select></label>
  </section>

  <section class="replay-controls">
    <div class="time-actions">
      <button class="quiet-button" type="button" onclick={() => (cursor = 0)}>Start</button>
      <button class="quiet-button" type="button" onclick={() => step(-1)} aria-label="Previous event">Previous</button>
      <button class="quiet-button" type="button" onclick={() => step(1)} aria-label="Next event">Next</button>
      <button class="quiet-button" type="button" onclick={() => (cursor = chronological.length)}>Latest</button>
      <button class="quiet-button" type="button" onclick={() => (baselineCursor = cursor)}>Pin comparison</button>
      {#if baselineCursor !== null}<button class="text-button" type="button" onclick={() => (baselineCursor = null)}>Clear comparison</button>{/if}
    </div>
    <label class="timeline">
      <span>Step {cursor} of {chronological.length}</span>
      <input type="range" min="0" max={chronological.length} bind:value={cursor}>
      <strong>{cursorNode ? time(cursorNode.occurred_at) : 'Before first event'}</strong>
    </label>
    {#if comparison}
      <div class="comparison" aria-label="Comparison summary">
        <span><strong>{comparison.events}</strong> events</span>
        <span><strong>{comparison.tools}</strong> tool records</span>
        <span><strong>{comparison.reasoning}</strong> reasoning records</span>
        <span><strong>{comparison.artifacts}</strong> artifacts</span>
      </div>
    {/if}
  </section>

  <div class="explorer-layout">
    <section class="flow-panel" aria-label="Interactive provenance graph">
      <div class="flow-canvas">
        <SvelteFlow
          nodes={flow.nodes}
          edges={flow.edges}
          {nodeTypes}
          fitView
          fitViewOptions={{
            padding: 0.08,
            maxZoom: 0.72,
            nodes: flow.nodes.filter((node, index) => node.type === 'lane' || index < lanes.length + 6)
          }}
          colorMode={theme}
          minZoom={0.12}
          maxZoom={1.8}
          onlyRenderVisibleElements
          nodesDraggable={false}
          nodesConnectable={false}
          onnodeclick={selectFlowNode}
        >
          <Background gap={24} size={1} />
          <Controls />
          <Panel position="top-right"><span class="canvas-guide">Drag canvas to pan · scroll to zoom · drag minimap to jump</span></Panel>
          <MiniMap
            pannable
            zoomable
            ariaLabel="Provenance graph overview and viewport control"
            nodeColor={(node) => node.type === 'lane' ? 'transparent' : 'var(--ucd-blue)'}
            nodeStrokeColor={(node) => node.type === 'lane' ? 'transparent' : 'var(--line-strong)'}
            nodeStrokeWidth={2}
          />
        </SvelteFlow>
      </div>
      {#if flow.clipped}<p class="canvas-note">The canvas shows the latest 700 matching nodes. The timeline and export keep the larger record.</p>{/if}
    </section>

    <aside class="evidence-drawer" aria-label="Selected provenance record">
      {#if selectedNode}
        <div class="drawer-heading">
          <div><p class="eyebrow">Selected record</p><h3>{title(selectedNode.kind)}</h3></div>
          <button class="icon-button" type="button" onclick={() => { selectedNode = null; selectedArtifact = null; evidence = null; }} aria-label="Close record">×</button>
        </div>
        <dl class="record-facts">
          <div><dt>Time</dt><dd>{time(selectedNode.occurred_at)}</dd></div>
          <div><dt>Role</dt><dd>{selectedNode.evidence_role}</dd></div>
          <div><dt>Actor</dt><dd>{selectedNode.actor}</dd></div>
          <div><dt>Harness</dt><dd>{selectedNode.harness}</dd></div>
          {#if selectedNode.model}<div><dt>Model</dt><dd>{selectedNode.model}</dd></div>{/if}
          {#if selectedNode.engine}<div><dt>Engine</dt><dd>{selectedNode.engine}</dd></div>{/if}
          {#if selectedNode.parser}<div><dt>Parser</dt><dd>{selectedNode.parser}</dd></div>{/if}
          {#if selectedNode.hash}<div><dt>Digest</dt><dd><code>{short(selectedNode.hash, 24)}</code></dd></div>{/if}
          {#if selectedArtifact}<div><dt>Size</dt><dd>{bytes(selectedArtifact.size)}</dd></div>{/if}
        </dl>
        {#if selectedNode.summary && Object.keys(selectedNode.summary).length}
          <details open><summary>Metadata</summary><pre>{JSON.stringify(selectedNode.summary, null, 2)}</pre></details>
        {/if}

        <section class="restricted-review">
          <h4>Restricted evidence</h4>
          <p class="secondary">Opening content needs a short-lived, run-scoped lease. The token stays in this tab and each read is audited.</p>
          {#if !lease}
            <label><span>Review purpose</span><textarea bind:value={leasePurpose} rows="3"></textarea></label>
            <button class="primary-button" type="button" onclick={openLease}>Open evidence lease</button>
          {:else}
            <p class="lease-expiry">Lease expires {time(lease.expires_at)}</p>
            {#if selectedArtifact}
              <button class="primary-button" type="button" onclick={saveArtifact}>Download verified artifact</button>
            {:else}
              <button class="primary-button" type="button" onclick={openEvidence}>Read this event</button>
            {/if}
            <form class="evidence-search" onsubmit={(event) => { event.preventDefault(); void runEvidenceSearch(); }}>
              <label><span>Search restricted evidence</span><input bind:value={evidenceQuery} type="search" placeholder="Exact term or phrase"></label>
              <button class="quiet-button" type="submit" disabled={evidenceQuery.trim().length < 2 || evidenceSearchLoading}>{evidenceSearchLoading ? 'Searching' : 'Search'}</button>
            </form>
            {#if evidenceMatches}
              <div class="evidence-matches" aria-live="polite">
                <p class="secondary">{evidenceMatches.total} matches. Excerpts remain restricted to this lease.</p>
                {#each evidenceMatches.hits as hit}
                  <button type="button" onclick={() => selectTarget(hit.event_id, true)}>
                    <span><strong>{title(hit.kind)}</strong><small>{time(hit.occurred_at)} · {hit.source}</small></span>
                    <span class="evidence-excerpt">{hit.excerpt}</span>
                  </button>
                {/each}
                {#if evidenceMatches.cursor > 0 || evidenceMatches.next_cursor !== null}
                  <div class="pager">
                    <button class="text-button" type="button" disabled={evidenceMatches.cursor === 0} onclick={() => runEvidenceSearch(Math.max(0, evidenceMatches.cursor - 20))}>Previous</button>
                    <button class="text-button" type="button" disabled={evidenceMatches.next_cursor === null} onclick={() => runEvidenceSearch(evidenceMatches.next_cursor)}>Next</button>
                  </div>
                {/if}
              </div>
            {/if}
          {/if}
        </section>

        {#if evidence}
          <section class="evidence-content">
            <div class="evidence-title"><h4>Evidence preview</h4>{#if evidence.content_truncated}<span>bounded preview</span>{/if}</div>
            {#if evidence.kind === 'agent.reasoning.returned'}
              <p class="secondary">Provider-returned reasoning. This is not hidden activation state and is not scientific authority.</p>
            {/if}
            {#if evidence.restricted_content}<pre>{evidence.restricted_content}</pre>{/if}
            <details><summary>Structured payload</summary><pre>{JSON.stringify(evidence.payload, null, 2)}</pre></details>
          </section>
        {/if}
      {:else}
        <div class="drawer-empty">
          <p class="eyebrow">Evidence inspector</p>
          <h3>Select a node</h3>
          <p class="secondary">Metadata is visible by default. Content and artifact bytes use an explicit evidence lease.</p>
        </div>
      {/if}
    </aside>
  </div>
{:else}
  <div class="empty-surface">No provenance graph is loaded.</div>
{/if}
