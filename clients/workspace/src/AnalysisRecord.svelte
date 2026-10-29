<script lang="ts">
  import { fileTitle, when } from "./language.ts";
  import { href } from "./route.ts";
  import { analysisName, analysisStatus, outcomeWords, settingLines, type AnalysisCatalog, type AnalysisVersionSummary,
    type TemplateAnalysis } from "./template_analysis.ts";

  // A reviewed R analysis's own record: which template ran on which table, with which settings,
  // who chose it, every attempt's receipt, and the other runs of the same analysis. Every word is fixed text or the
  // person's and the service's own.
  // `sourceTitle` is the source analysis's title as its own page shows it, when the page knows it.
  let { analysis, catalog, status, versions, current, project, sourceTitle, canChange, onChange }: {
    analysis: TemplateAnalysis; catalog: AnalysisCatalog | null; status: string; versions: AnalysisVersionSummary[];
    current: string; project?: string; sourceTitle?: string; canChange: boolean; onChange: () => void;
  } = $props();
  const feature = $derived(catalog?.features.find((f) => f.id === analysis.feature) ?? null);
  const lines = $derived(settingLines(feature, analysis.settings));
  const link = (task: string) => href(project ? { name: "analysis", project, task } : { name: "analysis", task });
  const source = $derived((sourceTitle?.trim() || analysis.source_title?.trim() || "an earlier analysis").replace(/[.!?]+$/, ""));
  const shortImage = (digest: string | null) => digest ? digest.replace(/^sha256:/, "").slice(0, 12) : "not named";
  // An earlier analysis's results file is its results table; a lab table carries its own name, often the source's too.
  const tableName = $derived(/^results\/(lab_derived\/)?results\.csv$/.test(analysis.table_path) ? "the results table" : fileTitle(analysis.table_path));
  const plain = (text: string) => text.toLowerCase().replace(/[_-]+/g, " ").replace(/\s+/g, " ").trim();
  const sameName = $derived(plain(tableName) === plain(source));
  const size = $derived(`${analysis.table_rows.toLocaleString()} ${analysis.table_rows === 1 ? "row" : "rows"} and ${analysis.table_columns.length} ${analysis.table_columns.length === 1 ? "column" : "columns"}`);
</script>

<div class="record">
  <p class="summary">{analysisName(catalog, analysis.feature)} of
    {#if sameName}<a class="link" href={link(analysis.source_task)}>{source}</a>, {size}.
    {:else}{tableName} from <a class="link" href={link(analysis.source_task)}>{source}</a>, {size}.{/if}</p>
  <dl>
    <div class="row"><dt>Table</dt><dd><span class="mono">{analysis.table_path}</span></dd></div>
    {#each lines as line (line.name)}<div class="row"><dt>{line.label}</dt><dd>{line.value}</dd></div>{/each}
    <div class="row"><dt>{lines.length ? "Other settings" : "Settings"}</dt><dd>The template's own defaults.</dd></div>
    <div class="row"><dt>Asked for by</dt><dd>{analysis.origin ? `The research lead, in turn ${analysis.origin.turn} of its research run` : "A person, from this workspace"}</dd></div>
  </dl>
  {#if canChange}
    <div class="edit">
      <button class="button secondary" onclick={onChange}>Change these settings</button>
      <p class="small soft">Opens these settings to run the analysis again. The new run is its own analysis; this one stays as it is.</p>
    </div>
  {/if}
  {#if analysis.receipts.length}
    <div class="attempts">
      <h3>{analysis.receipts.length === 1 ? "Its receipt" : "Its receipts"}</h3>
      <ol>
        {#each analysis.receipts as r (r.attempt)}
          <li>
            <span class="t">Attempt {r.attempt}: {outcomeWords(r, status)}</span>
            <span class="d">Analysis service {r.service_version ?? "version not named"} · image {shortImage(r.image_digest)} · {when(r.started_at)} to {when(r.finished_at)}</span>
            {#if r.package_sha256}<span class="d mono">Package SHA-256 {r.package_sha256}</span>{/if}
          </li>
        {/each}
      </ol>
    </div>
  {/if}
  {#if versions.length > 1}
    <div class="versions">
      <h3>Runs of this analysis</h3>
      <ol>
        {#each versions as v, i (v.task_id)}
          {@const n = versions.length - i}
          <li class:current={v.task_id === current}>
            {#if v.task_id === current}<span class="t" aria-current="page">Run {n}, this page</span>
            {:else}<a class="t" href={link(v.task_id)}>Run {n}</a>{/if}
            <span class="d">{when(v.created_at)} · {analysisStatus(v.status).label}{v.title ? ` · ${v.title}` : ""}</span>
          </li>
        {/each}
      </ol>
    </div>
  {/if}
</div>

<style>
  .record { display: grid; gap: 16px; max-width: 920px; }
  .summary { font-family: var(--serif); font-size: var(--font-lede); line-height: 1.5; max-width: 66ch; color: var(--text); }
  dl { margin: 0; border-top: 2px solid var(--heading); }
  .row { display: grid; grid-template-columns: 200px minmax(0, 1fr); gap: 4px 20px; padding: 9px 0; border-bottom: 1px solid var(--line); }
  .row dt { font-weight: 700; font-size: var(--font-small); color: var(--heading); padding-top: 1px; }
  .row dd { margin: 0; max-width: 70ch; overflow-wrap: anywhere; }
  .mono { font-family: var(--mono); font-size: 0.92em; }
  .edit { display: flex; flex-wrap: wrap; align-items: center; gap: 8px 18px; }
  .edit p { max-width: 56ch; }
  .attempts, .versions { display: grid; gap: 8px; }
  h3 { font-size: calc(15px * var(--type-scale)); color: var(--heading); }
  ol { list-style: none; margin: 0; padding: 0; border-top: 1px solid var(--line); }
  li { position: relative; display: flex; flex-wrap: wrap; align-items: baseline; gap: 2px 14px; padding: 8px 0 8px 12px; border-bottom: 1px solid var(--line); }
  .attempts li { display: grid; gap: 2px; }
  li.current::before { content: ""; position: absolute; left: 0; top: 6px; bottom: 6px; width: 3px; background: var(--mark); }
  .t { font-weight: 600; color: var(--heading); }
  .d { font-size: var(--font-small); color: var(--text-3); overflow-wrap: anywhere; }
  @media (max-width: 640px) {
    .row { grid-template-columns: 1fr; gap: 2px; }
  }
</style>
