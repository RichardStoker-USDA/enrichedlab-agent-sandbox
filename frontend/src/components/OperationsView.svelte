<script>
  import { mutate } from '../lib/api.js';
  import { bytes, short, time, title } from '../lib/format.js';

  let { data, view = 'overview', onRefresh = async () => {}, onOpenRun = () => {}, onStatus = () => {} } = $props();

  let rejection = $state({});
  let proposalChunk = $state({});
  let proposalReason = $state({});
  let autoServices = $state(false);
  let autoCatalog = $state(false);

  $effect(() => {
    autoServices = Boolean(data?.settings?.custom_auto_approve_loopback_services);
    autoCatalog = Boolean(data?.settings?.custom_auto_approve_catalog_entries);
  });

  let activeLeases = $derived((data?.leases || []).filter((lease) => lease.state === 'active').length);
  let researchWorkers = $derived(
    (data?.research || []).reduce((count, study) => count + study.workers.length, 0)
  );
  let stateEvents = $derived(
    (data?.agent_state || []).reduce((count, run) => count + run.events, 0)
  );
  let activeLogicalAgents = $derived(
    (data?.logical_agents || []).filter((agent) => agent.lifecycle === 'active').length
  );

  async function act(path, intent, body = {}) {
    onStatus('Applying the requested control change');
    try {
      await mutate(path, intent, body);
      await onRefresh();
      onStatus('Control change recorded');
    } catch (error) {
      onStatus(`Request failed: ${error.message}`);
    }
  }

  function encoded(value) {
    return encodeURIComponent(value);
  }
</script>

{#if view === 'overview'}
  <header class="page-heading">
    <div><p class="eyebrow">Current state</p><h2>Control plane overview</h2></div>
    <p class="secondary">Local operator data, runtime observations, and approval controls.</p>
  </header>
  <section class="metric-grid">
    <article><strong>{data.logical_agents?.length || 0}</strong><span>Logical agents</span></article>
    <article><strong>{activeLogicalAgents}</strong><span>Allocated runtimes</span></article>
    <article><strong>{data.requests.length}</strong><span>Pending requests</span></article>
    <article><strong>{activeLeases}</strong><span>Active service leases</span></article>
    <article><strong>{researchWorkers}</strong><span>Research workers</span></article>
    <article><strong>{stateEvents}</strong><span>State events</span></article>
    <article><strong>{data.catalog.length}</strong><span>Catalog entries</span></article>
  </section>

  <section class="surface-section">
    <div class="section-title"><div><p class="eyebrow">Independent observations</p><h3>Platforms and isolation</h3></div></div>
    <div class="card-grid">
      {#each Object.entries(data.gateway_health || {}) as [name, gateway]}
        <article class="detail-card">
          <div class="card-heading"><h4>{name}</h4><span class="pill">{gateway.disabled ? 'disabled' : gateway.passed ? 'observed' : 'unavailable'}</span></div>
          <dl class="compact-facts">
            <div><dt>Gateway</dt><dd><code>{gateway.gateway || '-'}</code></dd></div>
            <div><dt>Expected driver</dt><dd>{gateway.expected_driver || '-'}</dd></div>
            <div><dt>Observed driver</dt><dd>{gateway.observed_driver || '-'}</dd></div>
            <div><dt>Version</dt><dd><code>{gateway.gateway_version || '-'}</code></dd></div>
          </dl>
          {#if gateway.error}<p class="secondary">{gateway.error}</p>{/if}
        </article>
      {/each}
    </div>
    <details class="disclosure"><summary>Evidence boundary</summary><p>RuntimeClass binding shows the handler Kubernetes assigned. CRI-level microVM attestation still needs a trusted node receipt. Host or gateway compromise remains outside this boundary.</p></details>
  </section>

  <section class="surface-section settings-surface">
    <div><p class="eyebrow">Operator defaults</p><h3>Approval settings</h3><p class="secondary">LAN, peer, domain, and mount requests stay manual.</p></div>
    <form onsubmit={(event) => { event.preventDefault(); act('/api/v1/settings', 'update-settings', { custom_auto_approve_loopback_services: autoServices, custom_auto_approve_catalog_entries: autoCatalog }); }}>
      <label class="check-row"><input type="checkbox" bind:checked={autoServices}><span>Auto-approve bounded loopback services for auto-safe sandboxes</span></label>
      <label class="check-row"><input type="checkbox" bind:checked={autoCatalog}><span>Auto-register bounded catalog outputs</span></label>
      <button class="primary-button" type="submit">Save settings</button>
    </form>
  </section>
{:else if view === 'runtime'}
  <header class="page-heading"><div><p class="eyebrow">Runtime custody</p><h2>Sandboxes and services</h2></div></header>
  <section class="surface-section">
    <div class="section-title"><div><p class="eyebrow">Durable identity</p><h3>Logical agents</h3></div><p class="secondary">State and workspaces persist when the runtime is released.</p></div>
    <div class="table-wrap"><table><thead><tr><th>Name</th><th>Lifecycle</th><th>Domain</th><th>State profile</th><th>Runtime</th><th>Generation</th><th>Sessions</th><th>Last result</th></tr></thead><tbody>
      {#each data.logical_agents || [] as agent}
        <tr><td>{agent.name}</td><td>{agent.lifecycle}</td><td>{agent.domain}</td><td>{agent.state_profile}</td><td>{agent.active_sandbox || 'released'}</td><td>{agent.runtime_generation}</td><td>{agent.sessions_completed} / {agent.sessions_started}</td><td>{agent.last_result_class || '-'}</td></tr>
      {:else}<tr><td colspan="8" class="empty-cell">No logical agents.</td></tr>{/each}
    </tbody></table></div>
  </section>
  {#if data.allocation_reservations?.length}
    <section class="surface-section">
      <div class="section-title"><div><p class="eyebrow">Allocation custody</p><h3>Route reservations</h3></div><p class="secondary">Reservations make concurrent creates capacity-safe and expose interrupted allocation state.</p></div>
      <div class="table-wrap"><table><thead><tr><th>Sandbox</th><th>Route</th><th>Status</th><th>Created</th></tr></thead><tbody>
        {#each data.allocation_reservations as reservation}
          <tr><td>{reservation.sandbox}</td><td>{reservation.route}</td><td>{reservation.status}</td><td>{time(Number(reservation.created_at_unix) * 1000)}</td></tr>
        {/each}
      </tbody></table></div>
    </section>
  {/if}
  <section class="surface-section">
    <div class="section-title"><h3>Managed sandboxes</h3></div>
    <div class="table-wrap"><table><thead><tr><th>Name</th><th>Route</th><th>Gateway</th><th>Driver</th><th>Transport</th><th>Proof</th><th>Workspace</th><th></th></tr></thead><tbody>
      {#each data.sandboxes as sandbox}
        <tr><td>{sandbox.name}</td><td>{sandbox.route}</td><td>{sandbox.runtime.gateway_profile}</td><td>{sandbox.runtime.driver}</td><td>{sandbox.runtime.workspace_transport}</td><td>{data.runtime_proofs?.[sandbox.name]?.result || 'not observed'}</td><td><code>{sandbox.host_workspace}</code></td><td><button class="quiet-button" type="button" onclick={() => act(`/api/v1/evidence/${encoded(sandbox.name)}/capture`, 'capture-evidence')}>Capture evidence</button></td></tr>
      {:else}<tr><td colspan="8" class="empty-cell">No managed sandboxes.</td></tr>{/each}
    </tbody></table></div>
  </section>
  <section class="surface-section">
    <div class="section-title"><div><p class="eyebrow">Time-limited</p><h3>Service access</h3></div></div>
    <div class="table-wrap"><table><thead><tr><th>Kind</th><th>Sandbox</th><th>Bind</th><th>State</th><th>Expires</th><th></th></tr></thead><tbody>
      {#each data.leases as lease}
        <tr><td>{lease.kind}</td><td>{lease.source_sandbox ? `${lease.source_sandbox} → ${lease.sandbox}` : lease.sandbox}</td><td><code>{lease.bind}</code></td><td>{lease.state}</td><td>{time(Number(lease.expires_at_unix) * 1000)}</td><td>{#if ['active', 'starting'].includes(lease.state)}<button class="danger-button" type="button" onclick={() => act(`/api/v1/lease/${encoded(lease.id)}/close`, 'close-lease')}>Close</button>{/if}</td></tr>
      {:else}<tr><td colspan="6" class="empty-cell">No service leases.</td></tr>{/each}
    </tbody></table></div>
  </section>
  <section class="surface-section">
    <div class="section-title"><div><p class="eyebrow">Copy custody</p><h3>Workspace synchronization</h3></div></div>
    <div class="card-grid">
      {#each Object.values(data.sync_reports || {}) as report}
        <article class="detail-card"><div class="card-heading"><h4>{report.sandbox}</h4><span class="pill">{report.direction}</span></div><dl class="compact-facts"><div><dt>Applied</dt><dd>{report.applied ? 'yes' : 'no'}</dd></div><div><dt>Files</dt><dd>{report.summary?.files || 0}</dd></div><div><dt>Bytes</dt><dd>{bytes(report.summary?.bytes)}</dd></div><div><dt>Observed</dt><dd>{time(Number(report.observed_at_unix) * 1000)}</dd></div></dl></article>
      {:else}<p class="empty-copy">No sync reports.</p>{/each}
    </div>
  </section>
{:else if view === 'access'}
  <header class="page-heading"><div><p class="eyebrow">Least privilege</p><h2>Access decisions</h2></div><p class="secondary">Requests and proposals remain scoped to one sandbox and one declared use.</p></header>
  <section class="two-column">
    <div class="surface-section"><div class="section-title"><h3>Pending requests</h3><span class="section-count">{data.requests.length}</span></div><div class="stack-list queue-scroll">
      {#each data.requests as item}
        <article class="request-card"><div class="card-heading"><h4>{item.sandbox} · {item.request.kind}</h4><span class="pill">manual</span></div><p>{item.request.reason}</p><pre>{JSON.stringify(item.request.parameters, null, 2)}</pre><div class="inline-actions"><button class="primary-button" type="button" onclick={() => act(`/api/v1/request/${encoded(item.sandbox)}/${encoded(item.request.request_id)}/approve`, 'approve-request')}>Approve</button><input aria-label="Rejection reason" placeholder="Rejection reason" bind:value={rejection[item.request.request_id]}><button class="danger-button" type="button" onclick={() => act(`/api/v1/request/${encoded(item.sandbox)}/${encoded(item.request.request_id)}/reject`, 'reject-request', { reason: rejection[item.request.request_id] || 'Not approved' })}>Reject</button></div></article>
      {:else}<p class="empty-copy">No pending requests.</p>{/each}
    </div></div>
    <div class="surface-section"><div class="section-title"><h3>Network proposals</h3><span class="section-count">{Object.keys(data.proposals || {}).length}</span></div><div class="stack-list queue-scroll">
      {#each Object.entries(data.proposals || {}) as [sandbox, output]}
        <article class="request-card"><div class="card-heading"><h4>{sandbox}</h4><span class="pill">OpenShell</span></div><pre>{output.trim() || 'No pending proposals.'}</pre><div class="inline-actions proposal-actions"><input aria-label="Proposal chunk ID" placeholder="Chunk ID" bind:value={proposalChunk[sandbox]}><button class="primary-button" type="button" onclick={() => act(`/api/v1/proposal/${encoded(sandbox)}/${encoded(proposalChunk[sandbox] || '')}/approve`, 'approve-proposal')}>Approve ID</button><input aria-label="Proposal rejection reason" placeholder="Reason" bind:value={proposalReason[sandbox]}><button class="danger-button" type="button" onclick={() => act(`/api/v1/proposal/${encoded(sandbox)}/${encoded(proposalChunk[sandbox] || '')}/reject`, 'reject-proposal', { reason: proposalReason[sandbox] || 'Not approved' })}>Reject ID</button></div></article>
      {:else}<p class="empty-copy">No proposal sources.</p>{/each}
    </div></div>
  </section>
  <section class="two-column">
    <div class="surface-section"><div class="section-title"><h3>Active policies</h3></div>{#each Object.entries(data.active_policies || {}) as [sandbox, policy]}<details class="disclosure"><summary>{sandbox}</summary><pre>{JSON.stringify(policy, null, 2)}</pre></details>{:else}<p class="empty-copy">No active policies.</p>{/each}</div>
    <div class="surface-section"><div class="section-title"><h3>Recent OCSF events</h3></div><div class="event-list">{#each [...(data.ocsf_events || [])].reverse().slice(0, 40) as event}<details class="disclosure"><summary>{event.class_name || event.activity_name || event.type_name || 'OCSF event'} · {event.controller_sandbox || event.sandbox || 'unknown'}</summary><pre>{JSON.stringify(event, null, 2)}</pre></details>{:else}<p class="empty-copy">No OCSF observations.</p>{/each}</div></div>
  </section>
{:else if view === 'research'}
  <header class="page-heading"><div><p class="eyebrow">Reproducible work</p><h2>Research and agent state</h2></div><p class="secondary">Worker agreement is review evidence, not scientific validation.</p></header>
  <section class="surface-section"><div class="section-title"><h3>Research studies</h3></div><div class="card-grid">
    {#each data.research || [] as study}
      <article class="detail-card wide-card"><div class="card-heading"><h4>{study.name}</h4><span class="pill">{study.workers.filter((worker) => worker.sealed).length}/{study.workers.length} sealed</span></div><p>{title(study.mode)} · {study.input_file_count} inputs · {bytes(study.input_bytes)} · output cap {study.max_output_files} files / {bytes(study.max_output_bytes)}</p>{#each study.workers as worker}<div class="worker-row"><div><strong>{worker.name}</strong><span>{worker.harness || 'opencode'} · {worker.route} · {worker.state}</span></div><span>{worker.segments_completed || 0} segments · context {worker.route_context_limit} · output {worker.route_output_limit} · pool {worker.route_pool_cap}</span></div>{/each}</article>
    {:else}<p class="empty-copy">No research studies.</p>{/each}
  </div></section>
  <section class="surface-section"><div class="section-title"><div><p class="eyebrow">Scoped working memory</p><h3>Agent state runs</h3></div></div><div class="card-grid">
    {#each data.agent_state || [] as run}
      <article class="detail-card"><div class="card-heading"><h4>{run.run_id}</h4><span class="pill">{run.status}</span></div><p>{run.domain} · {run.profile}</p><dl class="compact-facts"><div><dt>Subject</dt><dd>{run.subject}</dd></div><div><dt>Epoch</dt><dd>{run.epoch}</dd></div><div><dt>Events</dt><dd>{run.events}</dd></div><div><dt>Artifacts</dt><dd>{run.artifacts}</dd></div></dl><button class="quiet-button" type="button" onclick={() => onOpenRun(run.run_id)}>Open provenance replay</button></article>
    {:else}<p class="empty-copy">No agent state runs.</p>{/each}
  </div></section>
  <section class="surface-section"><div class="section-title"><h3>Verified tool catalog</h3></div><div class="table-wrap"><table><thead><tr><th>Name</th><th>Kind</th><th>Files</th><th>Digest</th><th>Path</th></tr></thead><tbody>{#each data.catalog as entry}<tr><td>{entry.name}</td><td>{entry.kind}</td><td>{entry.file_count}</td><td><code>{short(entry.sha256, 18)}</code></td><td><code>{entry.path}</code></td></tr>{:else}<tr><td colspan="5" class="empty-cell">No catalog entries.</td></tr>{/each}</tbody></table></div></section>
{/if}
