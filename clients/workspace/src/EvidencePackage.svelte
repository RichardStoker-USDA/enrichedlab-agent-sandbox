<script lang="ts">
  import { BrokerError, downloadProjectPackage } from "./api.ts";
  import Dialog from "./Dialog.svelte";
  import { bytes, when } from "./language.ts";
  import { projects, type PackagePreview } from "./projects.ts";
  import { saveBlob } from "./viewer.ts";

  // The whole project as one file a scientist can keep, share with a reviewer or rerun.
  // The broker builds it from one database snapshot and the original files; the browser
  // checks its SHA-256 before saving it.
  let { projectId, open = $bindable(false) }: { projectId: string; open?: boolean } = $props();
  let preview = $state<PackagePreview | null>(null);
  let failure = $state("");
  let phase = $state<"idle" | "preparing" | "downloading" | "done">("idle");
  let received = $state(0);
  let total = $state(0);
  let result = $state<{ sha256: string; filename: string; bytes: number } | null>(null);
  let site = $state("");
  const handoff = $derived(preview?.hpc_handoff?.sites.length ? preview.hpc_handoff : null);
  const shown = $derived(handoff ? (handoff.sites.find((s) => s.key === site) ?? handoff.sites[0]) : null);
  let controller: AbortController | null = null;

  $effect(() => {
    if (!open) return;
    preview = null; failure = ""; phase = "idle"; result = null;
    const c = new AbortController();
    controller = c;
    void projects.packagePreview(projectId, c.signal).then((p) => { if (!c.signal.aborted) preview = p; })
      .catch((cause) => { if (!c.signal.aborted) failure = cause instanceof BrokerError && cause.code === "record_unavailable" ? "This project is no longer available to your account." : "The package contents could not be listed. Try again shortly."; });
    return () => c.abort();
  });

  async function download() {
    phase = "preparing"; failure = ""; received = 0; total = 0;
    const c = new AbortController();
    controller = c;
    try {
      const pkg = await downloadProjectPackage(projectId, (r, t) => { phase = "downloading"; received = r; total = t; }, c.signal);
      saveBlob(pkg.blob, pkg.filename);
      result = { sha256: pkg.sha256, filename: pkg.filename, bytes: pkg.bytes };
      phase = "done";
    } catch (cause) {
      phase = "idle";
      if (c.signal.aborted) return;
      const code = cause instanceof BrokerError ? cause.code : "";
      failure = code === "use_direct_download" ? "This package is larger than the browser can check in memory. Ask the site operator for the verified command-line download."
        : code === "capacity_reached" ? "Other evidence packages are being prepared right now. Try again in a minute."
        : code === "package_too_large" ? "This project's record is larger than the package limit. Ask the site operator for a verified export."
        : code === "artifact_integrity_failed" ? "The downloaded package did not match the checksum the server computed, so it was not saved. Try again."
        : "The package could not be prepared. Nothing was saved.";
    }
  }
</script>

<Dialog bind:open title="Evidence package" wide onclose={() => controller?.abort()}>
  <p>One file with this project's complete record: every version of the question and steps, every agent decision and
    rejected reply, the analysis records and runtime details, each person's review, the access record, and the original
    result files with their checksums. Its rerun kit checks every file, unpacks each analysis with a method sheet, writes
    import scripts for SAS and JMP, and can run each analysis again in R and compare its tables with the retained ones,
    on your computer or on an HPC with Slurm.</p>
  {#if preview}
    <dl class="contents">
      <div><dt>Saved versions</dt><dd>{preview.counts.plan_revisions}</dd></div>
      <div><dt>Agent decisions</dt><dd>{preview.counts.agent_decisions}</dd></div>
      <div><dt>Rejected agent replies and pauses</dt><dd>{preview.counts.agent_deferrals}</dd></div>
      <div><dt>Analyses</dt><dd>{preview.counts.tasks}</dd></div>
      {#if preview.counts.generated_code_blocks}<div><dt>Model-written or configured code, unvalidated</dt><dd>{preview.counts.generated_code_blocks}</dd></div>{/if}
      {#if preview.counts.person_reviews}<div><dt>Reviews by named people</dt><dd>{preview.counts.person_reviews}</dd></div>{/if}
      {#if preview.counts.rerun_analyses}<div><dt>Analyses the rerun kit can check</dt><dd>{preview.counts.rerun_analyses}</dd></div>{/if}
      <div><dt>Original files</dt><dd>{preview.counts.artifacts_included} · {bytes(preview.counts.artifact_bytes_included)}</dd></div>
      <div><dt>Access events</dt><dd>{preview.counts.access_events}</dd></div>
    </dl>
    <p class="small soft">Record as of {when(preview.snapshot_at)} · {preview.files} files before compression.
      {preview.coverage.artifact_bytes === "complete" ? "Every registered result file is included." : `${preview.missing.length} registered files are unavailable and are listed in the package.`}</p>
    {#if preview.missing.length}
      <ul class="missing small">{#each preview.missing as item (item.artifact_id)}<li>{item.label} ({item.relative_path})</li>{/each}</ul>
    {/if}
    {#if handoff && shown}
      <details class="hpc">
        <summary>Rerun it on SCINet Atlas or Ceres</summary>
        <div class="sites" role="group" aria-label="SCINet system">
          {#each handoff.sites as s (s.key)}<button type="button" aria-pressed={shown.key === s.key} onclick={() => (site = s.key)}>{s.name}</button>{/each}
        </div>
        <ol class="steps">
          {#each shown.steps as step, i (i)}<li>{step.text}{#if step.command}<code class="command">{step.command}</code>{/if}</li>{/each}
        </ol>
        <p class="small soft">These steps come from SCINet's guides as checked on {handoff.checked}, and the package's
          <code>rerun/README.md</code> holds them too, with the sources. If a guide has changed since, the guide wins.
          <a href={shown.open_ondemand} target="_blank" rel="noreferrer">Go to {shown.name} Open OnDemand</a></p>
      </details>
    {/if}
  {:else if !failure}<p class="soft">Listing the contents…</p>{/if}
  {#if phase === "preparing"}<p class="status active" role="status"><span class="waveform" aria-hidden="true"><i></i><i></i><i></i></span>Preparing the package on the server…</p>{/if}
  {#if phase === "downloading"}
    <div class="progress" role="status"><span>Downloading {bytes(received)} of {bytes(total)}</span><progress max={total} value={received}></progress></div>
  {/if}
  {#if result}
    <p class="notice" role="status">Saved <strong>{result.filename}</strong> ({bytes(result.bytes)}). The browser checked it against the server's SHA-256
      <code>{result.sha256}</code>. The download is recorded in the project's access record.</p>
  {/if}
  {#if failure}<p class="notice error" role="alert">{failure}</p>{/if}
  <p class="small soft">A matching checksum shows the files are intact. It is not scientific validation.</p>
  {#snippet footer()}
    <button class="link" onclick={() => (open = false)}>{result ? "Done" : "Cancel"}</button>
    <button class="button" onclick={download} disabled={!preview || phase === "preparing" || phase === "downloading"}>{result ? "Download again" : "Download the package"}</button>
  {/snippet}
</Dialog>

<style>
  /* Six counts fill three or two whole columns, so no empty cell shows the rule color. */
  /* clip, not hidden: a hidden overflow makes the grid a scroll container, which the dialog's scrolling body then
     shrinks below its content once that content is taller than the dialog. */
  .contents { display: grid; grid-template-columns: repeat(3, minmax(0, 1fr)); gap: 1px; margin: 0; background: var(--rule); border-radius: 8px; overflow: clip; box-shadow: 0 0 0 1px var(--rule); }
  @media (max-width: 520px) { .contents { grid-template-columns: repeat(2, minmax(0, 1fr)); } }
  .contents div { background: var(--page); padding: 12px 14px; }
  .contents dt { font-size: 12px; color: var(--ink-soft); }
  .contents dd { margin: 4px 0 0; font-size: 20px; font-weight: 650; color: var(--heading); font-variant-numeric: tabular-nums; }
  .missing { margin: 0; padding-left: 18px; color: var(--failed); }
  .progress { display: grid; gap: 6px; font-size: 13px; }
  progress { width: 100%; height: 6px; accent-color: var(--control); }
  code { overflow-wrap: anywhere; }
  .hpc { border-top: 1px solid var(--rule); padding-top: 12px; }
  .hpc summary { cursor: pointer; min-height: 32px; display: flex; align-items: center; font-weight: 650; color: var(--heading); }
  .sites { display: flex; gap: 4px 22px; margin: 6px 0 4px; border-bottom: 1px solid var(--rule); }
  .sites button { min-height: 34px; padding: 0; font-size: var(--font-small); font-weight: 600; color: var(--ink-soft); border-bottom: 2px solid transparent; }
  .sites button:hover { color: var(--heading); }
  .sites button[aria-pressed="true"] { color: var(--heading); border-bottom-color: var(--mark); }
  .steps { display: grid; gap: 10px; margin: 12px 0; padding-left: 22px; line-height: 1.5; }
  .steps li { padding-left: 2px; }
  .command { display: block; margin-top: 6px; padding: 8px 10px; border: 1px solid var(--rule); border-radius: 6px; background: var(--bench); color: var(--ink); user-select: all; }
</style>
