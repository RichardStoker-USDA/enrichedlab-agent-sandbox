<script lang="ts">
  // Take a run's answer away as a Word document, an Excel workbook or PowerPoint slides. The
  // broker's worker writes each one from the stored decision and the result tables it cites; the browser checks the
  // file's SHA-256 before saving it. A document restates the answer, so it carries the answer's standing with it.
  import { onDestroy } from "svelte";
  import { broker, BrokerError, packageContents, verifiedMember } from "./api.ts";
  import type { DocumentKind, RunDocument } from "./contracts.ts";
  import { anyWriting, documentKinds, documentName, documentRefusal, documentState, kindLabel } from "./documents.ts";
  import { saveBlob } from "./viewer.ts";

  let { project, run, title, canAsk }: { project: string; run: string; title: string; canAsk: boolean } = $props();

  let documents = $state<RunDocument[]>([]);
  let asking = $state<DocumentKind | null>(null);
  let saving = $state<string | null>(null);
  let problem = $state("");
  let timer: ReturnType<typeof setTimeout> | undefined;
  let controller: AbortController | undefined;
  // While a document is being written the list is read again: every 3 seconds for the first minute, then every 15.
  let looks = 0;
  const READ_PROBLEM = "The documents for this answer could not be read. Try again later.";

  async function refresh() {
    clearTimeout(timer);
    controller?.abort();
    const current = (controller = new AbortController());
    try {
      documents = await broker.runDocuments(run, current.signal);
      if (problem === READ_PROBLEM) problem = "";
      if (anyWriting(documents)) timer = setTimeout(() => void refresh(), ++looks <= 20 ? 3000 : 15000);
      else looks = 0;
    } catch {
      if (!current.signal.aborted) problem = READ_PROBLEM;
    }
  }

  $effect(() => {
    void run;
    void refresh();
  });
  onDestroy(() => { clearTimeout(timer); controller?.abort(); });

  async function ask(kind: DocumentKind) {
    asking = kind;
    problem = "";
    looks = 0;
    try {
      await broker.requestDocument(project, { run_id: run, kind, request_id: crypto.randomUUID() });
      await refresh();
    } catch (error) {
      problem = (error instanceof BrokerError && documentRefusal(error.code)) || `The ${kindLabel(kind)} could not be asked for. Try again later.`;
    } finally {
      asking = null;
    }
  }

  async function save(document: RunDocument) {
    const state = documentState(document);
    if (state.state !== "ready") return;
    saving = document.task_id;
    problem = "";
    try {
      const { artifacts } = await broker.artifacts(document.task_id);
      const pkg = artifacts.find((a) => a.role === "result");
      if (!pkg) throw new BrokerError("record_unavailable");
      const contents = await packageContents(document.task_id, pkg);
      const member = contents.listing.members.find((m) => m.path === state.file.path && m.sha256 === state.file.sha256);
      if (!member) throw new BrokerError("artifact_integrity_failed");
      saveBlob(await verifiedMember(document.task_id, pkg, member), documentName(title, document));
    } catch {
      problem = "The file could not be checked and saved. Try again later.";
    } finally {
      saving = null;
    }
  }
</script>

<div class="export" aria-labelledby="export-title">
  <h3 id="export-title" class="export-title">Take this answer with you</h3>
  <p class="small soft">Code writes the file from the stored answer and the result tables it cites, with every value's
    source. No person has accepted this answer yet.</p>
  {#if canAsk}
    <div class="kinds">
      {#each documentKinds as option (option.kind)}
        <button class="button secondary" disabled={asking !== null} onclick={() => void ask(option.kind)}>
          {asking === option.kind ? `Asking for the ${option.label}…` : option.label}</button>
      {/each}
    </div>
  {/if}
  {#if documents.length}
    <ul class="made" aria-label="Documents of this answer">
      {#each documents as document (document.task_id)}
        {@const state = documentState(document)}
        <li>
          <span>{kindLabel(document.kind)}</span>
          {#if state.state === "ready"}
            <button class="link small" disabled={saving === document.task_id} onclick={() => void save(document)}>
              {saving === document.task_id ? "Checking…" : "Save"}</button>
          {:else if state.state === "writing"}
            <span class="small soft">Being written</span>
          {:else}
            <span class="small soft">{state.words}</span>
          {/if}
        </li>
      {/each}
    </ul>
  {/if}
  {#if problem}<p class="small" role="status">{problem}</p>{/if}
</div>

<style>
  .export { margin-top: 1.5rem; padding-top: 1rem; border-top: 1px solid var(--line); }
  .export-title { font-size: var(--font-body); color: var(--heading); margin: 0 0 0.25rem; }
  .kinds { display: flex; flex-wrap: wrap; gap: 0.5rem; margin-top: 0.75rem; }
  .made { list-style: none; margin: 0.75rem 0 0; padding: 0; display: grid; gap: 0.35rem; }
  .made li { display: flex; gap: 0.75rem; align-items: baseline; flex-wrap: wrap; }
</style>
