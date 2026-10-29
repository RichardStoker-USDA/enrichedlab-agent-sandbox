<script lang="ts">
  import { onMount } from "svelte";
  import type { Identity } from "./contracts.ts";
  import { lab } from "./lab.svelte.ts";
  import { live, mayBeOld } from "./live.ts";
  import { projectIndex, standingFor } from "./project_index.svelte.ts";
  import WorkingRow from "./WorkingRow.svelte";

  // Working now (Glass Box): every project an agent is working on, across the reader's labs, the way an HPC portal
  // lists running sessions. The shell reads the list again whenever a project changes (live.ts); without the change
  // stream this page also reads it every 15 seconds while the tab is shown.
  let { identity, openProject, openWaiting, notify }: {
    identity: Identity; openProject: (id: string) => void; openWaiting: () => void; notify?: (text: string, error?: boolean) => void;
  } = $props();
  // The lab in view only, as the sidebar counts it.
  const shownLab = (key: string) => key === lab.key;
  const working = $derived(projectIndex.list.filter((p) => shownLab(p.group_key) && standingFor(p) === "working")
    .sort((a, b) => b.updated_at.localeCompare(a.updated_at)));
  const severalLabs = $derived(new Set(working.map((p) => p.group_key)).size > 1);
  onMount(() => {
    if (mayBeOld(projectIndex.readAt, live().link, Date.now())) void projectIndex.load();
    const timer = setInterval(() => { if (!document.hidden && live().link !== "live") void projectIndex.load(); }, 15_000);
    return () => clearInterval(timer);
  });
</script>

<section class="page" aria-labelledby="working-title">
  <header class="head">
    <h1 id="working-title">Working now</h1>
    <p class="lede">Projects an agent is working on. You can leave; when one needs you it moves to
      <a class="link" href="#/waiting" onclick={(e) => { e.preventDefault(); openWaiting(); }}>Waiting for you</a>.</p>
  </header>
  {#if projectIndex.phase === "loading" || projectIndex.phase === "idle"}<p class="soft" role="status"><span class="waveform" aria-hidden="true"><i></i><i></i><i></i></span> Reading your projects…</p>
  {:else if projectIndex.phase === "failed"}<p class="notice error" role="alert">{projectIndex.failure}</p>
  {:else if working.length}
    <ul class="grouped">{#each working as p (p.project_id)}<WorkingRow project={p} {identity} {openProject} showLab={severalLabs} {notify} />{/each}</ul>
  {:else}
    <p class="none">No agent is working on your projects right now. Ask a question from Your research to start one.</p>
  {/if}
</section>

<style>
  .page { padding: 36px var(--gutter) 24px; display: grid; gap: 24px; width: 100%; max-width: 900px; margin: 0 auto; align-content: start; }
  .head { display: grid; gap: 8px; }
  h1 { font-size: clamp(28px, calc(2.8vw * var(--type-scale)), calc(34px * var(--type-scale))); letter-spacing: -0.02em; line-height: 1.12; }
  .lede { max-width: 62ch; color: var(--text-2); }
  .none { color: var(--text-2); }
  @media (max-width: 640px) { .page { padding-top: 24px; } }
</style>
