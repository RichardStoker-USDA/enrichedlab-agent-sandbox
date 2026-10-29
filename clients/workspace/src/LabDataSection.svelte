<script lang="ts" module>
  // Labs found without the dataset method this session (a refused upload says so); the section stops offering to send.
  const unavailable = new Set<string>();
</script>

<script lang="ts">
  import { onDestroy, onMount } from "svelte";
  import { BrokerError } from "./api.ts";
  import { ACCEPT, datasetState, fileProblem, guardWords, labData, uploadProblem, type ListedDataset } from "./lab_data.ts";
  import { bytes, when } from "./language.ts";
  import type { Project } from "./projects.ts";
  import { href } from "./route.ts";
  import StatusMark from "./StatusMark.svelte";

  // A project's lab data: the tables the lab sent, each with where it stands, and a way to send another by
  // choosing or dropping a file. The guard and the broker's code read the file; the page sends it and shows the answer.
  let { project, labData: offered, openDataset, notify }: {
    project: Project;
    /** Whether the reader's identity says the project's lab holds the dataset method; a missing signal reads false. */
    labData: boolean;
    openDataset: (dataset: string) => void; notify: (text: string, error?: boolean) => void;
  } = $props();

  let datasets = $state<ListedDataset[]>([]);
  let phase = $state<"loading" | "ready" | "failed">("loading");
  let sending = $state<string | null>(null);
  let problem = $state("");
  let dragging = $state(false);
  let input = $state<HTMLInputElement | null>(null);
  let alive = true;
  const controller = new AbortController();
  const canSend = $derived(offered && !project.inspection_expires_at && project.capabilities.includes("contribute") && !unavailable.has(project.group_key));
  const shown = $derived(datasets.length > 0 || canSend);

  // One request id per chosen file until it is taken, so sending it again after a lost answer is one dataset.
  const requestKeys = new Map<string, string>();
  const fileKey = (file: File) => `${file.name}/${file.size}/${file.lastModified}`;
  function requestFor(file: File) {
    const key = fileKey(file);
    if (!requestKeys.has(key)) requestKeys.set(key, crypto.randomUUID());
    return requestKeys.get(key)!;
  }

  let timer: ReturnType<typeof setTimeout> | null = null;
  async function load() {
    try {
      datasets = await labData.list(project.project_id, controller.signal);
      phase = "ready";
    } catch {
      if (!alive || controller.signal.aborted) return;
      if (phase === "loading") phase = "failed";
    }
    if (timer) clearTimeout(timer);
    const busy = datasets.some((d) => datasetState(d).tone === "active");
    if (alive && busy) timer = setTimeout(() => { if (!document.hidden) void load(); }, 2500);
  }
  onMount(() => { void load(); });
  onDestroy(() => { alive = false; controller.abort(); if (timer) clearTimeout(timer); });

  async function send(files: File[]) {
    if (!files.length || sending || !canSend) return;
    problem = "";
    let opened = false;
    for (const file of files) {
      const refused = fileProblem(file);
      if (refused) { problem = `${file.name}: ${refused}`; continue; }
      sending = file.name;
      try {
        const answer = await labData.upload(project.project_id, file, requestFor(file));
        requestKeys.delete(fileKey(file));
        const words = guardWords(answer.dataset.guard);
        if (!words.taken) { problem = `${file.name}: ${words.text}`; continue; }
        notify(answer.created ? `${file.name} was checked and taken. The broker is reading it.` : `${file.name} was already sent; this is the same file.`);
        if (files.length === 1 && alive) { opened = true; openDataset(answer.dataset.dataset_id); }
      } catch (cause) {
        const code = cause instanceof BrokerError ? cause.reason : "";
        const said = uploadProblem(code, cause instanceof BrokerError ? cause.status : 0);
        if (said.unavailable) unavailable.add(project.group_key);
        if (code === "idempotency_conflict") requestKeys.delete(fileKey(file));
        problem = `${file.name}: ${said.words}`;
      } finally { sending = null; }
    }
    if (!opened && alive) await load();
  }
  function chosen() {
    const files = Array.from(input?.files ?? []);
    if (input) input.value = "";
    void send(files);
  }
  const hasFiles = (e: DragEvent) => Array.from(e.dataTransfer?.types ?? []).includes("Files");
  function over(e: DragEvent) {
    if (!canSend || !hasFiles(e)) return;
    e.preventDefault();
    if (e.dataTransfer) e.dataTransfer.dropEffect = "copy";
    dragging = true;
  }
  function dropped(e: DragEvent) {
    if (!canSend || !hasFiles(e)) return;
    e.preventDefault();
    dragging = false;
    void send(Array.from(e.dataTransfer?.files ?? []));
  }
</script>

{#if shown}
  <section id="section-lab-data" class="block lab-data" class:dragging aria-labelledby="lab-data-title" ondragover={over} ondragleave={(e) => { if (e.currentTarget === e.target) dragging = false; }} ondrop={dropped}>
    <div class="title-row">
      <h2 id="lab-data-title" class="section-title" tabindex="-1">Lab data {#if datasets.length}<small>{datasets.length === 1 ? "1 file" : `${datasets.length} files`}</small>{/if}</h2>
      {#if canSend}
        <input bind:this={input} id="lab-data-file" class="visually-hidden" type="file" accept={ACCEPT} multiple onchange={chosen} disabled={!!sending} />
        <label class="button secondary compact" for="lab-data-file" aria-disabled={!!sending}>{sending ? "Sending…" : "Add lab data"}</label>
      {/if}
    </div>
    <p class="soft small lede">Tables the lab keeps, from CSV, tab-separated text or Excel (.xlsx). The upload guard checks each file; the broker's own code reads it and prepares the table you approve. The research lead sees a prepared table's columns and counts, never its rows.</p>
    {#if sending}<p class="soft small" role="status"><span class="waveform" aria-hidden="true"><i></i><i></i><i></i></span> Sending {sending} to the upload guard…</p>{/if}
    {#if problem}<p class="notice error" role="alert">{problem}</p>{/if}
    {#if phase === "failed"}<p class="soft small">The project's lab data could not be listed just now.</p>
    {:else if datasets.length}
      <ul class="files">
        {#each datasets as d (d.dataset_id)}
          {@const state = datasetState(d)}
          <li>
            <a class="name" href={href({ name: "dataset", project: project.project_id, dataset: d.dataset_id })}
              onclick={(e) => { e.preventDefault(); openDataset(d.dataset_id); }}>{d.file_name}</a>
            <span class="meta small"><StatusMark tone={state.tone} label={state.words} /> <span class="faint">{bytes(d.size_bytes)} · {when(d.created_at)}</span></span>
          </li>
        {/each}
      </ul>
    {:else if phase === "ready" && canSend}
      <p class="faint small">No lab data yet. Choose a file, or drop one here.</p>
    {/if}
    {#if dragging}<p class="drop-hint" aria-hidden="true">Drop to send it to the upload guard</p>{/if}
  </section>
{/if}

<style>
  .block { display: grid; gap: 12px; min-width: 0; position: relative; scroll-margin-top: calc(var(--bar-h) + 64px); border-radius: var(--r-3); }
  .title-row { display: flex; flex-wrap: wrap; gap: 10px 18px; align-items: center; justify-content: space-between; }
  .compact { min-height: 34px; padding-inline: 14px; font-size: var(--font-small); }
  .lede { max-width: 78ch; }
  .files { list-style: none; padding: 0; margin: 0; display: grid; }
  .files li { display: flex; flex-wrap: wrap; gap: 4px 16px; align-items: baseline; justify-content: space-between; padding: 10px 0; border-top: 1px solid var(--line); }
  .files li:last-child { border-bottom: 1px solid var(--line); }
  .name { font-weight: 600; color: var(--heading); text-decoration: none; overflow-wrap: anywhere; }
  .name:hover { text-decoration: underline; text-decoration-color: var(--mark); }
  .meta { display: inline-flex; flex-wrap: wrap; gap: 4px 10px; align-items: baseline; }
  /* A file held over the section: the section shows where it lands, and nothing else moves. */
  .dragging { box-shadow: 0 0 0 2px var(--mark); background: var(--track); }
  .drop-hint { position: absolute; inset: auto 0 8px 0; text-align: center; font-weight: 600; color: var(--heading); pointer-events: none; }
  @media (prefers-reduced-motion: no-preference) { .block { transition: box-shadow var(--t-fast) var(--ease), background-color var(--t-fast) var(--ease); } }
</style>
