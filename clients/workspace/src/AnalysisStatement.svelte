<script lang="ts">
  import { onDestroy } from "svelte";
  import { verifiedMember } from "./api.ts";
  import { strictText, tableRead } from "./artifact_content.ts";
  import type { TaskEvidence } from "./evidence.ts";
  import { analysisStatus, nextStep, outcomeWords, refusalStep, refusedSettings, rounded, settingName, shownValue,
    type AnalysisCatalog, type TemplateAnalysis } from "./template_analysis.ts";
  import { MAX_TABLE_BYTES } from "./viewer.ts";

  // What a reviewed R analysis did (rule 2): its outcome, the template's own statement of the method, the headline
  // statistics read from the checked `statistics.csv`, and the template's notes. A refusal shows the broker's words and
  // names each setting it points at (rule 3). A tolerance test offers the comparison its table is made for (rule 4).
  // Every value shown is read from the checked file by this code; the page rounds for reading and shows every digit on
  // request.
  let { taskId, analysis, catalog, status, evidence, canChange, onChange, onNext }: {
    taskId: string; analysis: TemplateAnalysis; catalog: AnalysisCatalog | null; status: string; evidence: TaskEvidence | null;
    canChange: boolean;
    /** Opens the settings with these marked. */
    onChange: (marked: string[]) => void;
    /** Opens the analysis a step names: on this analysis's results for a next step, on its own table for a refusal's. */
    onNext: (step: NonNullable<ReturnType<typeof nextStep>>, from: "results" | "source") => void;
  } = $props();

  const receipt = $derived(analysis.receipts.at(-1));
  const outcome = $derived(analysisStatus(status, receipt));
  const feature = $derived(catalog?.features.find((f) => f.id === analysis.feature) ?? null);
  const pointed = $derived(receipt?.status === "refused" ? refusedSettings(feature, analysis.settings, receipt.refusal) : []);
  const first = $derived(refusalStep(analysis, receipt, catalog));
  const statisticsPath = $derived(receipt?.files?.find((f) => f.role === "statistics")?.path ?? null);
  const resultsPath = $derived(receipt?.files?.find((f) => f.role === "table")?.path ?? null);
  let statistics = $state<[string, string][] | null>(null);
  let statisticsPhase = $state<"idle" | "loading" | "failed" | "ready">("idle");
  let every = $state(false);
  let controller: AbortController | null = null;

  $effect(() => {
    const path = statisticsPath, pkg = evidence?.packages.find((p) => p.contents.listing.members.some((m) => m.path === path));
    statistics = null; every = false;
    if (!path || !pkg) { statisticsPhase = "idle"; return; }
    const member = pkg.contents.listing.members.find((m) => m.path === path)!;
    controller?.abort();
    const c = new AbortController();
    controller = c;
    statisticsPhase = "loading";
    void verifiedMember(taskId, pkg.artifact, member, c.signal)
      .then((blob) => strictText(blob, MAX_TABLE_BYTES))
      .then((text) => {
        if (c.signal.aborted) return;
        const read = tableRead(text, ",");
        if (read.columns.length !== 2 || read.columns[0] !== "statistic" || read.columns[1] !== "value") { statisticsPhase = "failed"; return; }
        statistics = read.rows.map((r) => [r[0] ?? "", r[1] ?? ""] as [string, string]);
        statisticsPhase = "ready";
      })
      .catch(() => { if (!c.signal.aborted) statisticsPhase = "failed"; });
    return () => c.abort();
  });
  onDestroy(() => controller?.abort());
  const anyRounded = $derived(!!statistics?.some(([, v]) => rounded(v)));
  // As many columns as fill the last row, or leave it fullest: nine statistics sit three by three, seven four and three.
  const columns = $derived.by(() => {
    const n = statistics?.length ?? 0;
    if (n <= 4) return Math.max(n, 1);
    return [4, 3, 5].find((c) => n % c === 0) ?? (n % 4 >= n % 3 ? 4 : 3);
  });
  const next = $derived(status === "completed" && statistics && resultsPath ? nextStep(analysis, new Map(statistics), catalog, resultsPath) : null);
</script>

<section class="statement" aria-labelledby="a-statement">
  <h2 id="a-statement" class="visually-hidden">What ran</h2>
  <div class="outcome" class:change-bar={outcome.tone === "failed"}>
    <p>{outcomeWords(receipt, status)}</p>
    {#if receipt && receipt.status !== "completed" && canChange}
      <div class="fixes">
        {#if first}<button class="button" onclick={() => onNext(first!, "source")}>{first.label}</button>{/if}
        {#each pointed as name (name)}
          <button class="button secondary" onclick={() => onChange([name])}>Change “{settingName(name, analysis.feature)}”</button>
        {/each}
        <button class={pointed.length || first ? "link" : "button secondary"} onclick={() => onChange([])}>{pointed.length ? "Change other settings" : "Change the settings"}</button>
      </div>
      {#if first}<p class="small soft">The per-animal summary makes one row for each {String(analysis.settings.experimental_unit)} from the same table; compare the groups on its results. For a glucose or insulin tolerance test, pick the tolerance test in the same list.</p>{/if}
    {/if}
  </div>
  {#if receipt?.method}
    <div class="method">
      <p class="label">Method, as the template states it</p>
      <p class="text">{receipt.method}</p>
    </div>
  {/if}
  {#if statistics?.length}
    <div class="ledger">
      <p class="label">Headline statistics <span class="from">from <span class="mono">{statisticsPath}</span></span></p>
      <dl style="--columns: {columns}">
        {#each statistics as [name, value], i (i)}
          <div class="stat"><dt class="mono">{name}</dt><dd title={rounded(value) ? value : undefined}>{every ? value : shownValue(value)}</dd></div>
        {/each}
      </dl>
      {#if anyRounded}<button class="link small" aria-pressed={every} onclick={() => (every = !every)}>{every ? "Show rounded values" : "Show every digit"}</button>{/if}
    </div>
  {:else if statisticsPhase === "failed"}
    <p class="soft small">The headline statistics could not be read from their file just now; the file is listed under Files.</p>
  {/if}
  {#if receipt?.notes.length}
    <div class="notes">
      <p class="label">Notes from the template</p>
      <ul>{#each receipt.notes as note, i (i)}<li>{note}</li>{/each}</ul>
    </div>
  {/if}
  {#if next && canChange}
    <div class="next">
      <button class="button" onclick={() => onNext(next!, "results")}>{next.label}</button>
      <p class="small soft">This table has one row per animal, so the comparison counts animals, not readings; it also checks that each {String(analysis.settings.id)} has one row.</p>
    </div>
  {/if}
</section>

<style>
  .statement { display: grid; gap: 20px; max-width: 920px; }
  .outcome { display: grid; gap: 10px; }
  .outcome > p { max-width: 76ch; }
  .outcome.change-bar { padding-left: 14px; }
  .fixes { display: flex; flex-wrap: wrap; align-items: center; gap: 8px 14px; }
  .label { font-size: var(--font-xs); font-weight: 700; letter-spacing: 0.04em; text-transform: uppercase; color: var(--text-2); }
  .from { font-weight: 400; letter-spacing: 0; text-transform: none; color: var(--text-3); margin-left: 6px; }
  .mono { font-family: var(--mono); }
  /* The method as a paper's methods section would print it. */
  .method { display: grid; gap: 6px; }
  .method .text { font-family: var(--serif); font-size: var(--font-lede); line-height: 1.55; max-width: 68ch; color: var(--text); }
  /* The headline statistics as a ledger: the template's own names, the values set in tabular figures. */
  .ledger { display: grid; gap: 8px; justify-items: start; }
  .ledger dl { margin: 0; display: grid; grid-template-columns: repeat(var(--columns), minmax(0, 1fr)); width: 100%; border-top: 2px solid var(--heading); }
  @media (max-width: 640px) { .ledger dl { grid-template-columns: repeat(2, minmax(0, 1fr)); } }
  .stat { display: grid; gap: 2px; padding: 9px 14px 10px 0; border-bottom: 1px solid var(--line); min-width: 0; }
  .stat dt { font-size: var(--font-xs); color: var(--text-2); overflow-wrap: anywhere; }
  .stat dd { margin: 0; font-size: calc(17px * var(--type-scale)); font-weight: 600; color: var(--heading); font-variant-numeric: tabular-nums; overflow-wrap: anywhere; }
  .notes { display: grid; gap: 6px; }
  .notes ul { margin: 0; padding-left: 18px; display: grid; gap: 4px; max-width: 76ch; color: var(--text); }
  .next { display: flex; flex-wrap: wrap; align-items: center; gap: 8px 18px; padding-top: 4px; }
  .next p { max-width: 58ch; }
</style>
