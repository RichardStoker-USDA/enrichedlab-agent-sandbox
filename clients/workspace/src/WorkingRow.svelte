<script lang="ts">
  import type { Identity } from "./contracts.ts";
  import { when } from "./language.ts";
  import { factsFor, projectIndex } from "./project_index.svelte.ts";
  import { projects, type ProjectSummary } from "./projects.ts";
  import { standingSentence } from "./standing.ts";
  import StatusMark from "./StatusMark.svelte";

  // A project the agent is working on, as home's Working now and its own page list it: where the work stands in one
  // recorded sentence, and Pause for a reader who may steer it. The row opens the project; Pause sits above that link.
  let { project: p, identity, openProject, showLab = false, notify }: {
    project: ProjectSummary; identity: Identity; openProject: (id: string) => void; showLab?: boolean;
    notify?: (text: string, error?: boolean) => void;
  } = $props();
  let pausing = $state(false);
  const labName = (key: string) => identity.groups.find((g) => g.key === key)?.name ?? key;
  const starter = $derived((p.owner?.principal_id ?? p.owner_id) === identity.principal.id ? "Started by you"
    : p.owner?.kind === "service_account" ? "Started automatically" : `Started by ${p.owner?.display_name?.trim() || "a colleague"}`);
  const mayPause = $derived(!!p.latest_run && ["queued", "running", "waiting"].includes(p.latest_run.status)
    && (p.capabilities ?? []).includes("steer") && (p.capabilities ?? []).includes("contribute"));
  async function pause() {
    if (!p.latest_run || pausing) return;
    pausing = true;
    try { await projects.pause(p.latest_run.run_id); notify?.("The agent is paused. Analyses already running keep saving their files."); void projectIndex.load(); }
    catch { notify?.("The agent could not be paused. Open the project and try again.", true); }
    finally { pausing = false; }
  }
</script>

<li class="act-row">
  <span class="row-body">
    <span class="row-title">{p.title}</span>
    <span class="row-status"><StatusMark tone="active" label={standingSentence(factsFor(p))} wrap /></span>
    <span class="row-meta">{#if showLab}<span>{labName(p.group_key)}</span>{/if}<span>{starter}</span><span>Changed {when(p.updated_at)}</span></span>
  </span>
  {#if mayPause}<button class="quiet-action" onclick={() => void pause()} disabled={pausing}>{pausing ? "Pausing…" : "Pause"}</button>{/if}
  <a class="stretch row-open" href={`#/projects/${p.project_id}`} aria-label={`Open ${p.title}`} onclick={(e) => { e.preventDefault(); openProject(p.project_id); }}>
    <svg class="chevron" viewBox="0 0 16 16" aria-hidden="true"><path d="M6 3.5 10.5 8 6 12.5" /></svg></a>
</li>
