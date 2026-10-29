<script lang="ts">
  import { tick } from "svelte";
  import { BrokerError, broker } from "./api.ts";
  import type { Artifact, ScientificHistoryEvent, ScientificHistoryEvidence, Task } from "./contracts.ts";
  import { verifiedArtifact } from "./api.ts";
  import FileView from "./FileView.svelte";
  import { fromArtifact } from "./viewer.ts";

  type StepState = "finished" | "failed";
  type Step = { event: ScientificHistoryEvent; state: StepState; title: string };
  let { task, artifacts, onDownload }: { task: Task; artifacts: Artifact[]; onDownload: (artifact: Artifact) => void } = $props();
  let events = $state<ScientificHistoryEvent[]>([]), coverage = $state<"available" | "pending" | "unavailable" | "">("");
  let attempt = $state<number | null>(null), generation = $state<number | null>(null), nextCursor = $state<string | null>(null);
  let loading = $state(false), loadingMore = $state(false), error = $state(""), retry = $state(0);
  let selectedId = $state(""), files = $state<ScientificHistoryEvidence[]>([]), nextFileCursor = $state<string | null>(null), fileError = $state("");
  let loadingFiles = $state(false), loadingMoreFiles = $state(false), selectedFileId = $state("");
  let query = $state(""), stateFilter = $state<"" | StepState>(""), pageNumber = $state(1), mobileDetail = $state(false);
  let jumpNumber = $state<number | undefined>(undefined);
  let indexPane = $state<HTMLElement | undefined>(undefined), detailPane = $state<HTMLElement | undefined>(undefined);
  let historyController: AbortController | undefined, fileController: AbortController | undefined;
  const uuid = /^[a-f0-9]{8}-[a-f0-9]{4}-[a-f0-9]{4}-[a-f0-9]{4}-[a-f0-9]{12}$/i, hash = /^[a-f0-9]{64}$/i, pageSize = 50;
  const isTool = (event: ScientificHistoryEvent) => event.visibility === "researcher" && event.evidence_role === "tool-io" && /^agent\.tool\.(finished|completed|failed|error|observed)$/.test(event.kind);
  const stateFor = (event: ScientificHistoryEvent): StepState => /(?:failed|error)$/.test(event.kind) ? "failed" : "finished";
  const steps = $derived(events.filter(isTool).map((event) => ({ event, state: stateFor(event), title: stateFor(event) === "failed" ? "Recorded tool step that did not finish" : "Recorded tool step" })));
  const filtered = $derived(steps.filter((step) => (!stateFilter || step.state === stateFilter) && (!query.trim() || `${step.title} ${step.event.kind} ${step.event.harness} ${step.event.segment} ${step.event.sequence} ${step.event.occurred_at} ${new Date(step.event.occurred_at).toLocaleString()}`.toLowerCase().includes(query.trim().toLowerCase()))));
  const pageCount = $derived(Math.max(1, Math.ceil(filtered.length / pageSize))), currentPage = $derived(Math.min(pageNumber, pageCount));
  const pageSteps = $derived(filtered.slice((currentPage - 1) * pageSize, currentPage * pageSize));
  const selected = $derived(pageSteps.find((step) => step.event.event_id === selectedId) ?? pageSteps[0]);
  const selectedIndex = $derived(selected ? steps.findIndex((step) => step.event.event_id === selected.event.event_id) : -1);
  const filteredIndex = $derived(selected ? filtered.findIndex((step) => step.event.event_id === selected.event.event_id) : -1);
  const previous = $derived(filteredIndex > 0 ? filtered[filteredIndex - 1] : undefined), next = $derived(filteredIndex >= 0 && filteredIndex < filtered.length - 1 ? filtered[filteredIndex + 1] : undefined);
  const retainedFiles = $derived(files.flatMap((reference) => { const artifact = artifacts.find((item) => item.id === reference.artifact_id); return artifact && artifact.sha256 === reference.sha256 && artifact.size_bytes === reference.size_bytes && artifact.media_type === reference.media_type ? [artifact] : []; }));
  const selectedFile = $derived(retainedFiles.find((file) => file.id === selectedFileId) ?? retainedFiles[0]);
  const unresolvedFiles = $derived(files.length - retainedFiles.length);
  const narrow = () => typeof window !== "undefined" && window.matchMedia("(max-width: 760px)").matches;
  const validEvent = (event: ScientificHistoryEvent) => Number.isSafeInteger(event.sequence) && event.sequence > 0 && uuid.test(event.event_id) && Number.isSafeInteger(event.segment) && event.segment >= 0 && typeof event.harness === "string" && typeof event.kind === "string" && typeof event.evidence_role === "string" && typeof event.visibility === "string" && typeof event.occurred_at === "string" && Number.isFinite(Date.parse(event.occurred_at)) && hash.test(event.hash) && Number.isSafeInteger(event.evidence_count) && event.evidence_count >= 0;
  function validHistory(page: Awaited<ReturnType<typeof broker.scientificHistory>>) {
    if (!['available','pending','unavailable'].includes(page.coverage) || !Array.isArray(page.events) || !page.events.every(validEvent) || (page.next_cursor !== null && typeof page.next_cursor !== "string")) throw new BrokerError("invalid_response");
    if (page.coverage === "available" && (typeof page.attempt !== "number" || !Number.isSafeInteger(page.attempt) || page.attempt < 1 || typeof page.lease_generation !== "number" || !Number.isSafeInteger(page.lease_generation) || page.lease_generation < 1)) throw new BrokerError("invalid_response");
    return page;
  }
  function validFiles(page: Awaited<ReturnType<typeof broker.scientificHistoryEvidence>>, expectedAttempt: number, expectedGeneration: number) {
    if (page.attempt !== expectedAttempt || page.lease_generation !== expectedGeneration || !Array.isArray(page.evidence) || (page.next_cursor !== null && typeof page.next_cursor !== "string") || !page.evidence.every((item) => uuid.test(item.artifact_id) && hash.test(item.sha256) && Number.isSafeInteger(item.size_bytes) && item.size_bytes >= 0 && typeof item.media_type === "string" && typeof item.role === "string" && item.relation === "observed_after")) throw new BrokerError("invalid_response");
    return page;
  }
  const message = (cause: unknown, file = false) => { const code = cause instanceof BrokerError ? cause.code : ""; if (code === "request_conflict") return file ? "This step belongs to an earlier attempt. Open the current recorded steps." : "A newer attempt replaced this step list. Open the current recorded steps."; if (code === "access_required" || code === "record_unavailable") return "These recorded steps are unavailable to your current account."; if (code === "connection_unavailable") return "Recorded steps are unavailable while the broker reconnects."; return file ? "The saved files for this step could not be opened." : "Recorded steps could not be opened."; };
  const merge = (current: ScientificHistoryEvent[], incoming: ScientificHistoryEvent[]) => { const result = new Map(current.map((event) => [event.event_id, event])); for (const event of incoming) result.set(event.event_id, event); return [...result.values()].sort((left, right) => left.sequence - right.sequence); };
  function reset() { events=[]; coverage=""; attempt=null; generation=null; nextCursor=null; selectedId=""; selectedFileId=""; files=[]; nextFileCursor=null; fileError=""; pageNumber=1; mobileDetail=false; error=""; }
  function retryHistory() { retry += 1; }
  function stopHistory() { historyController?.abort(); loading=false; loadingMore=false; error="Loading recorded steps was stopped."; }
  function setPage(page: number) {
    pageNumber = page;
    selectedId = "";
    selectedFileId = "";
    mobileDetail = false;
  }
  function updateFilter(next: Partial<{query:string; state:"" | StepState}>) {
    if (next.query !== undefined) query = next.query;
    if (next.state !== undefined) stateFilter = next.state;
    setPage(1);
  }
  async function choose(step: Step) {
    const position = filtered.findIndex((item) => item.event.event_id === step.event.event_id);
    if (position < 0) return;
    pageNumber = Math.floor(position / pageSize) + 1;
    selectedId = step.event.event_id;
    selectedFileId = "";
    if (narrow()) mobileDetail = true;
    await tick();
    detailPane?.scrollTo({top:0});
    if (narrow()) {
      detailPane?.scrollIntoView({block:"start"});
      detailPane?.focus({preventScroll:true});
    }
  }
  async function jumpToStep(event: SubmitEvent) {
    event.preventDefault();
    if (!jumpNumber || !Number.isInteger(jumpNumber) || jumpNumber < 1 || jumpNumber > steps.length) return;
    const step = steps[jumpNumber - 1];
    query = "";
    stateFilter = "";
    await choose(step);
    if (!narrow()) indexPane?.querySelector('[aria-current="step"]')?.scrollIntoView({block:"nearest"});
  }
  async function showResults() { mobileDetail=false; await tick(); indexPane?.focus({preventScroll:true}); }
  async function loadMoreSteps() { if (!nextCursor || loadingMore || coverage !== "available") return; const controller=new AbortController(); historyController?.abort(); historyController=controller; loadingMore=true; error=""; try { const page=validHistory(await broker.scientificHistory(task.id,nextCursor,controller.signal)); if (controller.signal.aborted) return; if (page.coverage !== "available" || page.attempt !== attempt || page.lease_generation !== generation) throw new BrokerError("request_conflict"); events=merge(events,page.events); nextCursor=page.next_cursor; } catch(cause) { if (!controller.signal.aborted) error=message(cause); } finally { if (!controller.signal.aborted) loadingMore=false; } }
  async function loadMoreFiles() { if (!selected || !nextFileCursor || loadingMoreFiles || attempt===null || generation===null) return; const controller=new AbortController(); fileController?.abort(); fileController=controller; loadingMoreFiles=true; fileError=""; try { const page=validFiles(await broker.scientificHistoryEvidence(task.id,selected.event.event_id,attempt,generation,nextFileCursor,controller.signal),attempt,generation); if (controller.signal.aborted) return; const merged=new Map(files.map((item)=>[item.artifact_id,item])); for (const item of page.evidence) merged.set(item.artifact_id,item); files=[...merged.values()]; nextFileCursor=page.next_cursor; } catch(cause) { if (!controller.signal.aborted) fileError=message(cause,true); } finally { if (!controller.signal.aborted) loadingMoreFiles=false; } }
  $effect(() => { const key=`${task.id}/${retry}`; void key; historyController?.abort(); reset(); loading=true; const controller=new AbortController(); historyController=controller; void broker.scientificHistory(task.id,null,controller.signal).then(validHistory).then((page)=>{ if (controller.signal.aborted) return; coverage=page.coverage; attempt=page.attempt; generation=page.lease_generation; events=merge([],page.events); nextCursor=page.next_cursor; }).catch((cause)=>{ if(!controller.signal.aborted) error=message(cause); }).finally(()=>{ if(!controller.signal.aborted) loading=false; }); return ()=>controller.abort(); });
  $effect(() => { const event=selected; fileController?.abort(); files=[]; nextFileCursor=null; fileError=""; selectedFileId=""; loadingFiles=false; loadingMoreFiles=false; if (!event || coverage!=="available" || attempt===null || generation===null) return; loadingFiles=true; const controller=new AbortController(); fileController=controller; void broker.scientificHistoryEvidence(task.id,event.event.event_id,attempt,generation,null,controller.signal).then((page)=>validFiles(page,attempt as number,generation as number)).then((page)=>{ if(!controller.signal.aborted) { files=page.evidence; nextFileCursor=page.next_cursor; } }).catch((cause)=>{ if(!controller.signal.aborted) fileError=message(cause,true); }).finally(()=>{ if(!controller.signal.aborted) loadingFiles=false; }); return ()=>controller.abort(); });
</script>

<section class:mobile-detail={mobileDetail} class="research-atlas" aria-label="Recorded steps">
  {#if loading && !coverage}<div class="content-message" role="status">Opening recorded steps… <button class="text-button" onclick={stopHistory}>Stop loading</button></div>
  {:else if error && !events.length}<div class="content-message" role="alert"><p>{error}</p><button class="text-button" onclick={retryHistory}>Try again</button></div>
  {:else if coverage === "pending"}<div class="empty-state"><h3>Recorded steps are still being saved</h3><p>This work has not produced an available step record yet.</p><button class="text-button" onclick={retryHistory}>Check again</button></div>
  {:else if coverage === "unavailable"}<div class="empty-state"><h3>Recorded steps are unavailable</h3><p>This work has no retained scientific step record for the current attempt. Files and service messages remain separate.</p></div>
  {:else if coverage === "available" && !steps.length}<div class="empty-state"><h3>No completed tool steps are available</h3><p>Service messages are kept separate from recorded tool steps. Retained files are listed under Files.</p>{#if nextCursor}<button class="text-button" onclick={loadMoreSteps}>Load more recorded events</button>{/if}</div>
  {:else if coverage === "available"}<div class:mobile-detail={mobileDetail} class="atlas-layout">
    <nav class="step-index" aria-label="Recorded steps" tabindex="-1" bind:this={indexPane}>
      <div class="step-index-tools"><label>Find a step <input value={query} oninput={(event)=>updateFilter({query:event.currentTarget.value})} placeholder="Search recorded steps" /></label><span>{steps.length} recorded tool {steps.length===1 ? "step" : "steps"} loaded</span></div>
      <div class="step-index-navigation">
        <label>Show <select value={stateFilter} onchange={(event)=>updateFilter({state:event.currentTarget.value as "" | StepState})}><option value="">All loaded steps</option><option value="finished">Completed steps</option><option value="failed">Failed steps</option></select></label>
        <form class="step-jump" onsubmit={jumpToStep}>
          <label for="recorded-step-number">Go to step</label>
          <div><input id="recorded-step-number" type="number" min="1" max={steps.length} step="1" required bind:value={jumpNumber} placeholder="Number" /><button type="submit">Go</button></div>
        </form>
      </div>
      <div class="step-results">{#each pageSteps as step}<button class:selected={step.event.event_id===selected?.event.event_id} class:failed={step.state==="failed"} onclick={()=>choose(step)} aria-current={step.event.event_id===selected?.event.event_id ? "step" : undefined} aria-label={`Step ${steps.findIndex((candidate)=>candidate.event.event_id===step.event.event_id)+1} of ${steps.length}: ${step.title}`}><b>{steps.findIndex((candidate)=>candidate.event.event_id===step.event.event_id)+1}</b><span><strong>{step.title}</strong><small>{new Date(step.event.occurred_at).toLocaleString()}</small></span></button>{/each}{#if !pageSteps.length}<p class="empty-state">No loaded steps match these filters.</p>{/if}</div>
      <footer class="pagination"><button disabled={currentPage===1} onclick={()=>setPage(Math.max(1,currentPage-1))}>Previous</button><span>Page {currentPage} of {pageCount}</span><button disabled={currentPage===pageCount} onclick={()=>setPage(Math.min(pageCount,currentPage+1))}>Next</button></footer>
      {#if nextCursor}<button class="text-button load-more-steps" disabled={loadingMore} onclick={loadMoreSteps}>{loadingMore ? "Loading more steps…" : "Load more steps"}</button>{/if}
      {#if error && events.length}<div class="content-message" role="alert"><p>{error}</p><button class="text-button" onclick={retryHistory}>Open current recorded steps</button></div>{/if}
    </nav>
    <article class="step-inspector" tabindex="-1" bind:this={detailPane}><button class="back-to-results" onclick={showResults}>Back to steps</button>{#if selected}
      <header class="step-reading-header"><div><p>Step {selectedIndex+1} of {steps.length} loaded</p><h3>{selected.title}</h3></div><nav class="step-neighbors" aria-label="Step navigation"><button disabled={!previous} onclick={()=>previous&&choose(previous)}>Previous</button><button disabled={!next} onclick={()=>next&&choose(next)}>Next</button></nav></header>
      {#if selected.state==="failed"}<p class="notice error">This tool step did not finish. Review only its saved files before relying on them.</p>{/if}
      <section class="artifact-links" aria-label="Files saved after this step"><h4>Files saved after this step</h4><p>These retained files were recorded after this tool step. They do not state what the tool used or produced.</p>{#if loadingFiles}<div class="content-message" role="status">Opening saved files…</div>{:else if fileError}<div class="content-message" role="alert"><p>{fileError}</p><button class="text-button" onclick={retryHistory}>Open current recorded steps</button></div>{:else if selectedFile}{#if retainedFiles.length>1}<div class="file-selector" aria-label="Choose a saved file">{#each retainedFiles as file}<button class:active={file.id===selectedFile.id} onclick={()=>selectedFileId=file.id}>{file.label}</button>{/each}</div>{/if}{#key selectedFile.id}<FileView file={fromArtifact(selectedFile)} load={(signal) => verifiedArtifact(task.id, selectedFile!, signal)} title={selectedFile.label} onDownload={() => onDownload(selectedFile!)} />{/key}{#if nextFileCursor}<button class="text-button" disabled={loadingMoreFiles} onclick={loadMoreFiles}>{loadingMoreFiles ? "Loading more files…" : "Load more saved files"}</button>{/if}{:else}<div class="empty-state"><h3>No saved files are linked to this step</h3><p>Other retained files are listed under Files.</p>{#if nextFileCursor}<button class="text-button" disabled={loadingMoreFiles} onclick={loadMoreFiles}>Load more saved files</button>{/if}</div>{/if}{#if unresolvedFiles}<p class="content-message" role="alert">A saved-file reference no longer matches the authorized task file record, so it was not opened.</p>{/if}</section>
      <details class="provenance"><summary>Step details</summary><dl><dt>Recorded environment</dt><dd>{selected.event.harness}</dd><dt>Recorded segment</dt><dd>{selected.event.segment}</dd><dt>Event sequence</dt><dd>{selected.event.sequence}</dd><dt>Event kind</dt><dd>{selected.event.kind}</dd><dt>Event hash</dt><dd>{selected.event.hash}</dd></dl></details>
    {:else}<div class="empty-state"><h3>No step selected</h3><p>Change the filters or choose a recorded step.</p></div>{/if}</article>
  </div>{/if}
</section>

<style>
  .research-atlas { display: grid; gap: 14px; }
  /* Two reading columns divided by a rule: the step list and the step being read. */
  .atlas-layout { display: grid; grid-template-columns: minmax(240px, 0.7fr) minmax(0, 1.6fr); gap: 0; min-height: 420px; border-top: 2px solid var(--heading); }
  .step-index, .step-inspector { min-width: 0; }
  .step-index { display: flex; flex-direction: column; padding: 16px 24px 0 0; gap: 12px; }
  .step-index-tools, .step-index-navigation { display: grid; gap: 10px; font-size: var(--font-small); }
  .step-index-tools span { color: var(--text-2); }
  .step-jump div { display: flex; gap: 8px; }
  .step-jump button, .pagination button { min-height: 36px; padding: 0 12px; border-radius: var(--r-2); box-shadow: inset 0 0 0 1px var(--line-2); font-weight: 600; color: var(--heading); }
  .step-results { display: grid; overflow: auto; max-height: 460px; border-top: 1px solid var(--line); }
  .step-results button { position: relative; display: grid; grid-template-columns: 42px minmax(0, 1fr); gap: 10px; padding: 10px 6px; text-align: left; border-bottom: 1px solid var(--line); }
  .step-results button:hover { background: var(--hover-wash); }
  .step-results button.selected { background: var(--surface-2); }
  .step-results button.selected::before { content: ""; position: absolute; left: 0; top: 6px; bottom: 6px; width: 3px; background: var(--mark); }
  .step-results button.failed b { color: var(--failed); }
  .step-results b { font: 600 var(--font-xs) var(--mono); color: var(--text-3); padding-top: 2px; }
  .step-results strong { display: block; font-size: calc(14px * var(--type-scale)); color: var(--heading); }
  .step-results small { display: block; font-size: var(--font-xs); color: var(--text-3); }
  .pagination { display: flex; justify-content: space-between; align-items: center; font-size: var(--font-small); }
  .step-inspector { padding: 16px 0 0 24px; border-left: 1px solid var(--line); display: grid; gap: 14px; align-content: start; }
  .back-to-results { display: none; }
  .step-reading-header { display: flex; justify-content: space-between; align-items: flex-start; gap: 16px; }
  .step-reading-header p { font-size: var(--font-xs); color: var(--text-3); }
  .step-reading-header h3 { font-size: calc(17px * var(--type-scale)); margin-top: 4px; }
  .step-neighbors { display: flex; gap: 14px; font-size: var(--font-small); }
  .step-neighbors button, .text-button, .back-to-results { color: var(--heading); font-weight: 600; text-decoration: underline; text-decoration-color: var(--line-2); text-underline-offset: 3px; justify-self: start; }
  .artifact-links { display: grid; gap: 10px; }
  .artifact-links h4 { font-size: calc(15px * var(--type-scale)); }
  .artifact-links > p { font-size: var(--font-small); color: var(--text-2); }
  .file-selector { display: flex; flex-wrap: wrap; gap: 4px 20px; border-bottom: 1px solid var(--line); }
  .file-selector button { min-height: 32px; font-size: var(--font-small); font-weight: 600; color: var(--text-2); border-bottom: 2px solid transparent; }
  .file-selector button.active { color: var(--heading); border-bottom-color: var(--mark); }
  .content-message, .empty-state { display: grid; gap: 8px; justify-items: start; padding: 14px 0; color: var(--text-2); font-size: var(--font-small); }
  .empty-state h3 { font-size: calc(16px * var(--type-scale)); color: var(--heading); }
  .provenance { border-top: 1px solid var(--line); padding-top: 10px; font-size: var(--font-small); }
  .provenance summary { cursor: pointer; color: var(--text-2); font-weight: 600; }
  .provenance dl { display: grid; grid-template-columns: max-content minmax(0, 1fr); gap: 6px 16px; margin: 10px 0 0; }
  .provenance dd { margin: 0; font-family: var(--mono); font-size: var(--font-xs); overflow-wrap: anywhere; }
  @media (max-width: 760px) {
    .atlas-layout { grid-template-columns: 1fr; }
    .step-index { padding-right: 0; }
    .step-inspector { padding-left: 0; border-left: 0; }
    .atlas-layout.mobile-detail .step-index { display: none; }
    .atlas-layout:not(.mobile-detail) .step-inspector { display: none; }
    .back-to-results { display: inline-block; }
  }
</style>
