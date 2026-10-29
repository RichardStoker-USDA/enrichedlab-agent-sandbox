<script lang="ts">
  import { tick, untrack } from "svelte";
  import { BrokerError } from "./api.ts";
  import { analyses, analysisAbout, choiceName, draftOf, offered, requestProblem, settingHelp, settingName, settingsFrom, startingSettings,
    type AnalysisCatalog, type AnalysisFeature, type AnalysisParam, type AnalysisSettings, type Draft } from "./template_analysis.ts";

  // A reviewed R analysis of one table. The person chooses one of the render service's analysis
  // templates, by the question it answers, and its columns and options; the broker checks the request and a compute
  // worker runs the template on the table as it was saved. No model writes code for it, nothing is previewed, and each
  // run is its own analysis that waits for a person's review. Changing a finished one's settings makes the next run of
  // the same analysis; the earlier one stays as it was.
  let { open = $bindable(false), project, sourceTask, table, initial = null, marked = [], onStarted, notify }: {
    open?: boolean;
    project: string;
    sourceTask: string;
    table: { path: string; title: string; columns: string[]; numeric: boolean[] };
    initial?: { feature: string; settings: AnalysisSettings; title: string; previous: string | null } | null;
    /** Settings a refusal named, marked until they change. */
    marked?: string[];
    onStarted: (task: string) => void;
    notify: (message: string, error?: boolean) => void;
  } = $props();

  let dialog = $state<HTMLDialogElement | undefined>(undefined);
  let catalog = $state<AnalysisCatalog | null>(null);
  let catalogPhase = $state<"loading" | "ready" | "failed">("loading");
  let featureId = $state("");
  let draft = $state<Draft>({});
  let title = $state("");
  let problems = $state<string[]>([]);
  let failure = $state("");
  let sending = $state(false);
  let named = $state<Set<string>>(new Set());
  let requestId = "", lastBody = "";
  let controller: AbortController | null = null;

  const questions = $derived(offered(catalog, table.columns, table.numeric));
  const feature = $derived(catalog?.features.find((f) => f.id === featureId) ?? null);
  const params = $derived(feature ? Object.entries(feature.params) : []);
  const columnSettings = $derived(params.filter(([, p]) => p.type === "column" || p.type === "columns")
    .sort(([, a], [, b]) => Number(!!b.required) - Number(!!a.required)));
  const options = $derived(params.filter(([, p]) => p.type !== "column" && p.type !== "columns"));
  const built = $derived(feature ? settingsFrom(feature, draft, table.columns) : null);
  const changing = $derived(!!initial?.previous);

  $effect(() => {
    if (!dialog) return;
    if (open && !dialog.open) dialog.showModal();
    if (!open && dialog.open) dialog.close();
  });
  $effect(() => { if (open) untrack(() => void start()); });

  async function start() {
    controller?.abort();
    controller = new AbortController();
    problems = []; failure = ""; sending = false; requestId = ""; lastBody = "";
    named = new Set(marked);
    title = initial?.title ?? "";
    catalogPhase = "loading";
    try {
      catalog = await analyses.catalog(controller.signal);
      catalogPhase = "ready";
    } catch {
      if (!controller.signal.aborted) catalogPhase = "failed";
      return;
    }
    const all = questions.flatMap((q) => q.features);
    const first = all.find((f) => f.feature.id === initial?.feature) ?? all.find((f) => f.fits) ?? all[0];
    if (first) choose(first.feature, first.feature.id === initial?.feature ? initial!.settings : null);
    if (named.size) {
      await tick();
      const target = dialog?.querySelector<HTMLElement>(`[data-setting="${CSS.escape([...named][0])}"] :is(select, input)`);
      target?.scrollIntoView({ block: "center" });
      target?.focus({ preventScroll: true });
    } else if (initial && narrow()) {
      // On a phone an analysis that opens filled in shows its settings first; the list stays above them.
      await tick();
      document.getElementById("analysis-settings")?.scrollIntoView({ block: "start" });
    }
  }
  const narrow = () => matchMedia("(max-width: 860px)").matches;

  // Switching analyses keeps every column the new one also takes, and fills its required columns.
  function choose(next: AnalysisFeature, settings: AnalysisSettings | null = null) {
    const kept: AnalysisSettings = {};
    if (!settings && feature) {
      const current = settingsFrom(feature, draft, table.columns).settings;
      for (const [name, value] of Object.entries(current))
        if (name in next.params && next.params[name].type === feature.params[name]?.type) kept[name] = value;
    }
    featureId = next.id;
    draft = draftOf(next, settings ?? { ...startingSettings(next, table.columns, table.numeric), ...kept });
    problems = []; failure = "";
  }
  async function picked(next: AnalysisFeature) {
    choose(next);
    // On a phone the settings sit below the list; the chosen analysis's settings come into view.
    if (narrow()) {
      await tick();
      document.getElementById("analysis-settings")?.scrollIntoView({ behavior: matchMedia("(prefers-reduced-motion: reduce)").matches ? "auto" : "smooth", block: "start" });
    }
  }

  const value = (name: string) => { const v = draft[name]; return Array.isArray(v) ? v.join(", ") : v ?? ""; };
  const list = (name: string) => { const v = draft[name]; return Array.isArray(v) ? v : v ? [v] : []; };
  function setValue(name: string, next: string | string[]) {
    draft = { ...draft, [name]: next };
    if (named.has(name)) { const left = new Set(named); left.delete(name); named = left; }
  }
  function toggleColumn(name: string, column: string, on: boolean) {
    const now = list(name).filter((c) => c !== column);
    setValue(name, on ? table.columns.filter((c) => c === column || now.includes(c)) : now);
  }
  const isSwitch = (param: AnalysisParam) => param.type === "enum" && (param.choices ?? []).map(String).sort().join() === "false,true";
  const chosenChoice = (name: string, param: AnalysisParam) => (value(name) || String(param.default ?? "")).toLowerCase();

  const placeholder = $derived(feature ? `${feature.label} of ${table.title}` : "");
  async function send() {
    if (!feature || !built || sending) return;
    problems = built.problems;
    if (problems.length) return;
    // An empty title takes the name the field shows, so every page names the analysis the same way.
    const named = [...(title.trim() || placeholder)].slice(0, 200).join("").trim();
    const body = { source_task: sourceTask, table_path: table.path, feature: feature.id, settings: built.settings, title: named,
      previous: initial?.previous ?? null };
    const text = JSON.stringify(body);
    // The same request keeps its request id, so a send whose answer was lost makes one analysis.
    if (text !== lastBody) { lastBody = text; requestId = crypto.randomUUID(); }
    sending = true; failure = "";
    try {
      const task = await analyses.request(project, { ...body, request_id: requestId });
      notify("The analysis is queued. A compute worker runs the reviewed R template; its page shows the results when it finishes.");
      open = false;
      onStarted(task);
    } catch (cause) {
      const code = cause instanceof BrokerError ? cause.reason : "";
      failure = requestProblem(cause instanceof BrokerError ? cause.status : 0, code);
      if (code === "idempotency_conflict") lastBody = "";
    } finally { sending = false; }
  }
</script>

{#snippet control(name: string, param: AnalysisParam)}
  {@const help = settingHelp(name, param, featureId)}
  {@const id = `analysis-${name}`}
  {@const described = help ? `${id}-hint` : undefined}
  {@const label = settingName(name, featureId)}
  {@const flagged = named.has(name)}
  {#if param.type === "column"}
    <div class="field" class:named={flagged} data-setting={name}>
      <label class="name" for={id}>{label}{#if param.required}<span class="need" aria-hidden="true"> needed</span>{/if}</label>
      <select {id} aria-describedby={described} aria-required={param.required ? "true" : undefined} aria-invalid={flagged ? "true" : undefined}
        value={value(name)} onchange={(e) => setValue(name, e.currentTarget.value)}>
        {#if !param.required || !value(name)}<option value="">{param.required ? "Choose a column" : "Not used"}</option>{/if}
        {#each table.columns as column (column)}<option value={column}>{column}</option>{/each}
      </select>
      {#if help}<span class="hint" id={described}>{help}</span>{/if}
    </div>
  {:else if param.type === "columns"}
    <fieldset class="field columns" class:named={flagged} data-setting={name} aria-describedby={described}>
      <legend class="name">{label}{#if param.required}<span class="need" aria-hidden="true"> needed</span>{/if}</legend>
      <div class="column-list">
        {#each table.columns as column (column)}
          <label><input type="checkbox" checked={list(name).includes(column)} onchange={(e) => toggleColumn(name, column, e.currentTarget.checked)} />
            <span>{column}</span></label>
        {/each}
      </div>
      {#if help}<span class="hint" id={described}>{help}</span>{/if}
    </fieldset>
  {:else if isSwitch(param)}
    <div class="switch" data-setting={name}>
      <input {id} type="checkbox" aria-describedby={described} checked={chosenChoice(name, param) === "true"}
        onchange={(e) => setValue(name, e.currentTarget.checked === (String(param.default ?? "") === "true") ? "" : String(e.currentTarget.checked))} />
      <label class="name" for={id}>{label}</label>
      {#if help}<span class="hint" id={described}>{help}</span>{/if}
    </div>
  {:else if param.type === "enum"}
    <fieldset class="choices" data-setting={name} aria-describedby={described}><legend class="name">{label}</legend>
      <div class="options">
        {#if param.default === undefined}
          <label class:on={!value(name)}><input type="radio" name={id} checked={!value(name)} onchange={() => setValue(name, "")} />Default</label>
        {/if}
        {#each param.choices ?? [] as choice (choice)}
          {@const on = chosenChoice(name, param) === String(choice).toLowerCase()}
          <label class:on><input type="radio" name={id} checked={on}
            onchange={() => setValue(name, String(choice) === String(param.default ?? "") ? "" : String(choice))} />{choiceName(name, choice)}</label>
        {/each}
      </div>
      {#if help}<span class="hint" id={described}>{help}</span>{/if}
    </fieldset>
  {:else if param.type === "number"}
    <div class="field short" data-setting={name}>
      <label class="name" for={id}>{label}</label>
      <input {id} type="number" step="any" min={param.min} max={param.max} aria-describedby={described} value={value(name)}
        placeholder={param.default !== undefined ? `Default ${param.default}` : "Not set"} oninput={(e) => setValue(name, e.currentTarget.value)} />
      {#if help}<span class="hint" id={described}>{help}</span>{/if}
    </div>
  {:else if param.type === "numbers"}
    <div class="field" data-setting={name}>
      <label class="name" for={id}>{label}</label>
      <input {id} type="text" inputmode="decimal" aria-describedby={described} value={value(name)} placeholder="For example: 1, 2.5" oninput={(e) => setValue(name, e.currentTarget.value)} />
      {#if help}<span class="hint" id={described}>{help}</span>{/if}
    </div>
  {:else}
    <div class="field" data-setting={name}>
      <label class="name" for={id}>{label}</label>
      <input {id} type="text" maxlength={param.maxlen ?? 200} aria-describedby={described} value={value(name)} oninput={(e) => setValue(name, e.currentTarget.value)} />
      {#if help}<span class="hint" id={described}>{help}</span>{/if}
    </div>
  {/if}
{/snippet}

<dialog bind:this={dialog} class="studio" onclose={() => { open = false; controller?.abort(); }} aria-labelledby="analysis-dialog-title">
  <header class="bar">
    <div>
      <p class="kicker">{changing ? "A new run of this analysis" : "A reviewed R analysis"} of <span class="table-name">{table.title}</span></p>
      <h2 id="analysis-dialog-title">{title.trim() || feature?.label || "Analysis"}</h2>
    </div>
    <button class="close" onclick={() => (open = false)} aria-label="Close without running">
      <svg viewBox="0 0 16 16" aria-hidden="true"><path d="M3.5 3.5l9 9m0-9l-9 9" /></svg>
    </button>
  </header>

  <div class="body">
    <section class="index" aria-labelledby="analysis-index-title">
      <h3 id="analysis-index-title" class="visually-hidden">Analyses</h3>
      {#if catalogPhase === "loading"}
        <p class="soft" role="status">Reading which analyses the analysis service offers…</p>
      {:else if catalogPhase === "failed" || !catalog}
        <p class="soft" role="alert">{catalogPhase === "failed" ? "The analyses could not be read just now. Close this and try again." : "The analysis service has not been reached yet. Try again in a moment."}</p>
      {:else}
        <p class="lede small">Each analysis is reviewed R code that no model wrote. It runs on the table as it was saved, {table.columns.length} columns, and keeps every digit of its results.</p>
        {#each questions as q (q.title)}
          <fieldset class="question"><legend>{q.title}</legend>
            {#each q.features as f (f.feature.id)}
              <label class="analysis" class:on={f.feature.id === featureId} class:off={!f.fits}>
                <input type="radio" name="analysis" checked={f.feature.id === featureId} onchange={() => picked(f.feature)} />
                <span class="name">{f.feature.label}</span>
                <span class="hint">{analysisAbout(f.feature)}{#if !f.fits}<span class="why"> {f.why}</span>{/if}</span>
              </label>
            {/each}
          </fieldset>
        {/each}
      {/if}
    </section>

    <!-- The dialog names each value it cannot send in plain words, so the browser's own checks stay out of the way. -->
    <form id="analysis-settings" class="settings" novalidate onsubmit={(e) => { e.preventDefault(); void send(); }}>
      {#if feature}
        <div class="chosen">
          <p class="eyebrow">{feature.label}</p>
          <p class="about">{analysisAbout(feature)}</p>
        </div>
        {#if columnSettings.length}
          <fieldset class="group"><legend>Columns</legend>{#each columnSettings as [name, param] (name)}{@render control(name, param)}{/each}</fieldset>
        {/if}
        {#if options.length}
          <fieldset class="group"><legend>Options</legend>{#each options as [name, param] (name)}{@render control(name, param)}{/each}</fieldset>
        {/if}
        <fieldset class="group"><legend>Title</legend>
          <div class="field">
            <label class="visually-hidden" for="analysis-title">Title</label>
            <input id="analysis-title" type="text" maxlength="200" aria-describedby="analysis-title-hint" bind:value={title} {placeholder} />
            <span class="hint" id="analysis-title-hint">Left empty, the title is the one shown.</span>
          </div>
        </fieldset>
        <div class="actions">
          {#if problems.length}<ul class="problems" role="alert">{#each problems as p (p)}<li>{p}</li>{/each}</ul>{/if}
          {#if failure}<p class="problems" role="alert">{failure}</p>{/if}
          <button class="button" type="submit" disabled={sending}>{sending ? "Sending…" : changing ? "Run it with these settings" : "Run this analysis"}</button>
          <p class="hint">It runs on a compute worker, can take a few minutes, and waits for a person's review like any analysis.{#if changing} The earlier run stays as it was.{/if}</p>
        </div>
      {/if}
    </form>
  </div>
</dialog>

<style>
  .studio {
    width: 100vw; max-width: 100vw; height: 100dvh; max-height: 100dvh; margin: 0; padding: 0; border: 0;
    background: var(--page); color: var(--ink); display: none; overflow: hidden;
  }
  .studio[open] { display: grid; grid-template-rows: auto minmax(0, 1fr); animation: arrive 200ms var(--ease); }
  @keyframes arrive { from { opacity: 0; } }
  @media (prefers-reduced-motion: reduce) { .studio[open] { animation: none; } }
  .studio::backdrop { background: var(--scrim); }
  .bar { display: flex; align-items: flex-start; justify-content: space-between; gap: 16px; padding: 16px var(--gutter) 14px;
    border-bottom: 1px solid var(--line); background: var(--surface); }
  .kicker { font-size: var(--font-small); color: var(--text-2); }
  .table-name { font-family: var(--serif); color: var(--heading); }
  h2 { font-size: calc(22px * var(--type-scale)); letter-spacing: -0.01em; line-height: 1.2; margin-top: 2px; overflow-wrap: anywhere; }
  .close { width: 40px; height: 40px; border-radius: var(--r-1); display: grid; place-items: center; color: var(--text-2); flex: none; }
  .close:hover { background: var(--hover-wash); color: var(--heading); }
  .close svg { width: 16px; height: 16px; stroke: currentColor; stroke-width: 1.6; fill: none; }
  .body { display: grid; grid-template-columns: minmax(0, 1fr) minmax(340px, 460px); grid-template-rows: minmax(0, 1fr); min-height: 0; }
  /* The analyses as an index by question, like a methods handbook's contents; the chosen one carries the mark. */
  .index { min-height: 0; overflow: auto; padding: 22px var(--gutter) 28px; display: grid; gap: 22px; align-content: start; }
  .lede { max-width: 68ch; color: var(--text-2); }
  .question { border: 0; margin: 0; padding: 0; display: grid; gap: 2px; min-width: 0; max-width: 760px; }
  .question > legend { padding: 0 0 6px; margin-bottom: 4px; width: 100%; border-bottom: 2px solid var(--heading); font-weight: 700;
    font-size: calc(15px * var(--type-scale)); color: var(--heading); }
  .analysis { position: relative; display: grid; grid-template-columns: 18px minmax(0, 1fr); column-gap: 10px; padding: 8px 10px 8px 12px; cursor: pointer; }
  .analysis input { grid-row: span 2; margin-top: 3px; }
  .analysis:hover { background: var(--hover-wash); }
  .analysis.on { background: var(--surface-2); }
  .analysis.on::before { content: ""; position: absolute; left: 0; top: 6px; bottom: 6px; width: 3px; background: var(--mark); }
  .analysis.off .name { color: var(--text-2); font-weight: 500; }
  .why { color: var(--text-2); }
  .name { font-weight: 600; color: var(--heading); font-size: calc(14px * var(--type-scale)); }
  .need { font-weight: 400; color: var(--text-3); font-size: var(--font-xs); }
  .hint { display: block; font-size: var(--font-xs); line-height: 1.45; color: var(--text-3); }
  .settings { position: relative; min-height: 0; overflow: auto; padding: 22px var(--gutter) 28px 26px; border-left: 1px solid var(--line);
    display: grid; gap: 22px; align-content: start; background: var(--surface); }
  .chosen { display: grid; gap: 4px; }
  .eyebrow { font-weight: 700; color: var(--heading); font-size: calc(17px * var(--type-scale)); letter-spacing: -0.01em; }
  .about { font-family: var(--serif); font-size: calc(15px * var(--type-scale)); line-height: 1.5; color: var(--text); max-width: 52ch; }
  .group { border: 0; margin: 0; padding: 0; display: grid; gap: 12px; min-width: 0; }
  .group > legend { padding: 0 0 8px; margin-bottom: 4px; width: 100%; border-bottom: 1px solid var(--heading); font-weight: 700;
    font-size: calc(14px * var(--type-scale)); color: var(--heading); }
  .field { display: grid; gap: 5px; min-width: 0; align-content: start; border: 0; margin: 0; padding: 0; }
  .field input, .field select { width: 100%; }
  .field.short input { max-width: 160px; }
  .field > legend { padding: 0; margin-bottom: 4px; }
  /* A setting a refusal named: the one to change. */
  .field.named select, .field.named .column-list { border-color: var(--failed); box-shadow: inset 3px 0 0 var(--failed); }
  .column-list { display: grid; gap: 2px; max-height: 196px; overflow: auto; padding: 6px 8px; border: 1px solid var(--line-2); border-radius: var(--r-1); background: var(--page); }
  .column-list label { display: grid; grid-template-columns: 18px minmax(0, 1fr); gap: 8px; align-items: center; font-size: var(--font-small); cursor: pointer; padding: 2px 0; }
  .column-list span { font-family: var(--mono); font-size: var(--font-xs); overflow-wrap: anywhere; }
  .choices { border: 0; margin: 0; padding: 0; display: grid; gap: 6px; min-width: 0; }
  .choices > legend { padding: 0; margin-bottom: 6px; }
  .options { display: flex; flex-wrap: wrap; gap: 6px; }
  .options > label { position: relative; display: inline-flex; align-items: center; gap: 6px; padding: 6px 11px; border-radius: var(--r-1);
    box-shadow: inset 0 0 0 1px var(--line-2); font-size: var(--font-small); cursor: pointer; color: var(--text); }
  .options > label:hover { box-shadow: inset 0 0 0 1px var(--heading); }
  .options > label.on { box-shadow: inset 0 0 0 2px var(--heading); color: var(--heading); font-weight: 600; }
  .options > label input[type="radio"] { position: absolute; opacity: 0; pointer-events: none; }
  .options > label:has(input:focus-visible) { outline: 2px solid var(--focus); outline-offset: 2px; }
  .switch { display: grid; grid-template-columns: 18px minmax(0, 1fr); column-gap: 10px; row-gap: 2px; align-items: start; }
  .switch input { margin-top: 3px; }
  .switch label { cursor: pointer; }
  .switch .hint { grid-column: 2; }
  .problems { margin: 0; padding: 0 0 0 14px; display: grid; gap: 4px; font-size: var(--font-small); color: var(--failed); border-left: 3px solid var(--failed); list-style: none; }
  .actions { display: grid; gap: 8px; justify-items: start; padding-top: 6px; position: sticky; bottom: -28px; background: var(--surface); padding-bottom: 6px; }
  @media (max-width: 860px) {
    /* One scrolling column on phones: the analyses first, then the chosen one's settings, each as tall as its content. */
    .body { display: block; overflow: auto; }
    .index, .settings { overflow: visible; }
    .index { padding: 18px var(--gutter); }
    .settings { border-left: 0; border-top: 1px solid var(--line); padding: 20px var(--gutter) 28px; scroll-margin-top: 0; }
    .actions .button { width: 100%; }
  }
</style>
