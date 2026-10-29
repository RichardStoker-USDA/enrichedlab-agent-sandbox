<script lang="ts">
  import { sharing } from "./context.ts";
  import type { Identity } from "./contracts.ts";
  import { when } from "./language.ts";
  import { factsFor, standingFor } from "./project_index.svelte.ts";
  import type { ProjectSummary } from "./projects.ts";
  import { reviewSentence, standingSentence, standingTone } from "./standing.ts";
  import StatusMark from "./StatusMark.svelte";

  // One project in a grouped list: its title, the question in one serif line, where it stands in one recorded
  // sentence, and who started it, who can open it and when it changed. Home's groups and Working now share it.
  let { project: p, identity, openProject, showLab = false }: {
    project: ProjectSummary; identity: Identity; openProject: (id: string) => void; showLab?: boolean;
  } = $props();
  const me = $derived(identity.principal.id);
  const labName = (key: string) => identity.groups.find((g) => g.key === key)?.name ?? key;
  const starter = $derived((p.owner?.principal_id ?? p.owner_id) === me ? "Started by you"
    : p.owner?.kind === "service_account" ? "Started automatically"
    : `Started by ${p.owner?.display_name?.trim() || "a colleague"}`);
  const facts = $derived(factsFor(p));
  const standing = $derived(standingFor(p));
</script>

<li>
  <a class="grouped-row project-row" href={`#/projects/${p.project_id}`} onclick={(e) => { e.preventDefault(); openProject(p.project_id); }}>
    <span class="row-body">
      <span class="row-title">{p.title}</span>
      {#if facts.question}<span class="row-question">{facts.question}</span>{/if}
      <span class="row-status"><StatusMark tone={standingTone[standing]} label={standingSentence(facts)} wrap />{#if reviewSentence(facts.task_counts)}<span class="row-review">{reviewSentence(facts.task_counts)}</span>{/if}</span>
      <span class="row-meta">{#if showLab}<span>{labName(p.group_key)}</span>{/if}<span>{starter}</span><span class="who-can">{sharing(p.visibility, labName(p.group_key))}</span><span>Changed {when(p.updated_at)}</span></span>
    </span>
    <svg class="chevron" viewBox="0 0 16 16" aria-hidden="true"><path d="M6 3.5 10.5 8 6 12.5" /></svg>
  </a>
</li>

<style>
  .project-row { padding: 16px 18px 16px 20px; }
  @media (max-width: 640px) { .project-row { padding: 14px 14px 14px 16px; } }
</style>
