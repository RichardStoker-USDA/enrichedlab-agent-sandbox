<script lang="ts">
  import type { FigureCatalog, FigureVersion, FigureVersionSummary } from "./contracts.ts";
  import FigureCompare from "./FigureCompare.svelte";
  import { chartName, manuscriptOf, outcomeSentence, plainNote, settingsLines, versionStatus } from "./figure.ts";
  import { fileTitle, when } from "./language.ts";
  import { href } from "./route.ts";

  // A figure version's own record: what was drawn from which table, what the render service said, the
  // settings it was drawn with, and the other versions of the same figure. Every word is fixed text or the person's and
  // the service's own; nothing here is inferred from the figure.
  // `sourceTitle` is the source analysis's title as its own page shows it (its plan step's), when the page knows it.
  // One drawn version of a figure can be marked as the one in the manuscript; `onMark` sets the mark on a version or
  // clears it (null), and the page reads the versions again.
  let { figure, catalog, status, versions, current, project, sourceTitle, canEdit, onEdit, reader = "", canMark = false, onMark,
    companionAvailable = false, companionOpen = false, onCompanionOpen }: {
    figure: FigureVersion; catalog: FigureCatalog | null; status: string; versions: FigureVersionSummary[];
    current: string; project?: string; sourceTitle?: string; canEdit: boolean; onEdit: () => void;
    reader?: string; canMark?: boolean; onMark?: (version: string | null) => Promise<void>;
    companionAvailable?: boolean; companionOpen?: boolean; onCompanionOpen?: (open: boolean) => void;
  } = $props();
  const feature = $derived(catalog?.features.find((f) => f.id === figure.feature) ?? null);
  const receipt = $derived(figure.receipts.at(-1));
  const lines = $derived(settingsLines(feature, figure.controls).map((line) => {
    const at = line.indexOf(": ");
    return { label: line.slice(0, at), value: line.slice(at + 2) };
  }));
  const link = (task: string) => href(project ? { name: "analysis", project, task } : { name: "analysis", task });
  // The sentence ends with its own full stop, so the title's is dropped.
  const source = $derived((sourceTitle?.trim() || figure.source_title?.trim() || "an earlier analysis").replace(/[.!?]+$/, ""));
  // The versions list is newest first, so version 1 is the last.
  const numberOf = (task: string) => versions.length - versions.findIndex((v) => v.task_id === task);
  // The mark as the versions list carries it, else as this version's record does; undefined when the broker keeps none.
  const mark = $derived(versions.length ? manuscriptOf(versions) : figure.manuscript);
  const who = (m: { marked_by: string; marked_by_name?: string | null }) => m.marked_by_name?.trim() || (m.marked_by === reader ? "you" : "another member of the project");
  const markWords = $derived(!mark ? "No version of this figure is marked as the one in the manuscript."
    : mark.version === current ? `This version is the one in the manuscript, marked by ${who(mark)} on ${when(mark.marked_at)}.`
    : `Version ${numberOf(mark.version)} is the one in the manuscript, marked by ${who(mark)} on ${when(mark.marked_at)}.`);
  let marking = $state<"" | "mark" | "clear">("");
  async function setMark(version: string | null) {
    if (!onMark || marking) return;
    marking = version ? "mark" : "clear";
    try { await onMark(version); } finally { marking = ""; }
  }
  const drawn = $derived(versions.filter((v) => v.status === "completed").length);
  let comparing = $state(false);
</script>

<div class="record">
  <p class="summary">{chartName(catalog, figure.feature)} of <span class="table">{fileTitle(figure.table_path)}</span>,
    {figure.table_rows.toLocaleString()} {figure.table_rows === 1 ? "row" : "rows"}, from
    <a class="link" href={link(figure.source_task)}>{source}</a>.</p>
  {#if receipt || status === "failed"}
    <div class="outcome" class:change-bar={!!receipt && receipt.status !== "completed"}>
      <p>{outcomeSentence(receipt, status)}</p>
      {#if receipt?.notes.length}<ul>{#each receipt.notes as note (note)}<li>{plainNote(note, figure.feature)}</li>{/each}</ul>{/if}
    </div>
  {/if}
  {#if mark !== undefined}
    <div class="manuscript">
      <p>{markWords}</p>
      {#if canMark && onMark && status === "completed"}
        {#if mark?.version === current}
          <button class="button secondary" disabled={!!marking} onclick={() => setMark(null)}>{marking === "clear" ? "Clearing…" : "Clear the mark"}</button>
        {:else}
          <button class="button secondary" disabled={!!marking} onclick={() => setMark(current)}>{marking === "mark" ? "Marking…" : mark ? "Mark this version instead" : "Mark this version for the manuscript"}</button>
        {/if}
      {/if}
    </div>
  {/if}
  <dl>
    {#each lines as line (line.label)}<div class="row"><dt>{line.label}</dt><dd>{line.value}</dd></div>{/each}
    {#if !lines.length}<div class="row"><dt>Settings</dt><dd>Drawn with the chart's own defaults.</dd></div>{/if}
  </dl>
  {#if canEdit}
    <div class="edit">
      <button class="button" onclick={onEdit}>Edit this figure</button>
      <p class="small soft">Opens these settings in the figure editor, where R draws previews. Keeping one makes a new version; this one stays as it is.</p>
    </div>
  {/if}
  {#if companionAvailable && onCompanionOpen}
    <div class="explore-record">
      <button class="button secondary" aria-expanded={companionOpen} onclick={() => onCompanionOpen?.(!companionOpen)}>{companionOpen ? "Close the interactive view" : "Explore this figure's data"}</button>
      <p class="small soft">Uses the mapped columns and exact rows from <span class="mono">{figure.table_path}</span>. It does not change this R figure.</p>
    </div>
  {/if}
  {#if versions.length > 1}
    <div class="versions">
      <h3>Versions of this figure</h3>
      <ol>
        {#each versions as v, i (v.task_id)}
          {@const n = versions.length - i}
          <li class:current={v.task_id === current}>
            {#if v.task_id === current}<span class="t" aria-current="page">Version {n}, this page</span>
            {:else}<a class="t" href={link(v.task_id)}>Version {n}</a>{/if}
            <span class="d">{when(v.created_at)} · {versionStatus(v.status).label}{v.title ? ` · ${v.title}` : ""}</span>
            {#if mark?.version === v.task_id}<span class="in">In the manuscript</span>{/if}
          </li>
        {/each}
      </ol>
      {#if drawn > 1 && status === "completed"}
        <button class="button secondary compare-toggle" aria-expanded={comparing} onclick={() => (comparing = !comparing)}>{comparing ? "Hide the comparison" : "Compare two versions"}</button>
        {#if comparing}<FigureCompare current={figure} {versions} {catalog} {numberOf} />{/if}
      {/if}
    </div>
  {/if}
</div>

<style>
  .record { display: grid; gap: 16px; max-width: 920px; }
  .summary { font-family: var(--serif); font-size: var(--font-lede); line-height: 1.5; max-width: 66ch; color: var(--text); }
  .summary .table { color: var(--heading); }
  .outcome { display: grid; gap: 6px; max-width: 70ch; }
  .outcome.change-bar { padding-left: 14px; }
  .outcome ul { margin: 0; padding-left: 18px; display: grid; gap: 3px; font-size: var(--font-small); color: var(--text-2); }
  dl { margin: 0; border-top: 2px solid var(--heading); }
  .row { display: grid; grid-template-columns: 200px minmax(0, 1fr); gap: 4px 20px; padding: 9px 0; border-bottom: 1px solid var(--line); }
  .row dt { font-weight: 700; font-size: var(--font-small); color: var(--heading); padding-top: 1px; }
  .row dd { margin: 0; max-width: 70ch; overflow-wrap: anywhere; }
  .manuscript { display: flex; flex-wrap: wrap; align-items: center; gap: 8px 18px; }
  .manuscript p { max-width: 64ch; }
  .edit { display: flex; flex-wrap: wrap; align-items: center; gap: 8px 18px; }
  .edit p { max-width: 56ch; }
  .explore-record { display: flex; flex-wrap: wrap; align-items: center; gap: 8px 18px; }
  .explore-record p { max-width: 62ch; }
  .versions { display: grid; gap: 8px; }
  h3 { font-size: calc(15px * var(--type-scale)); color: var(--heading); }
  .versions ol { list-style: none; margin: 0; padding: 0; border-top: 1px solid var(--line); }
  .versions li { position: relative; display: flex; flex-wrap: wrap; align-items: baseline; gap: 2px 14px; padding: 8px 0 8px 12px; border-bottom: 1px solid var(--line); }
  .versions li.current::before { content: ""; position: absolute; left: 0; top: 6px; bottom: 6px; width: 3px; background: var(--mark); }
  .t { font-weight: 600; color: var(--heading); }
  .d { font-size: var(--font-small); color: var(--text-3); }
  .in { font-size: var(--font-small); font-weight: 700; color: var(--heading); }
  .compare-toggle { justify-self: start; margin-top: 6px; }
  @media (max-width: 640px) {
    .row { grid-template-columns: 1fr; gap: 2px; }
  }
</style>
