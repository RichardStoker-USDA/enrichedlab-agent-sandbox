<script lang="ts">
  import { onDestroy, untrack } from "svelte";
  import { broker, BrokerError } from "./api.ts";
  import { sha256OfBytes } from "./canonical.ts";
  import { colorMatrixValues, VISIONS } from "./colour_vision.ts";
  import { terminal, type FigureCatalog, type FigureControls, type FigureFeature, type FigureParam, type FigureReceipt } from "./contracts.ts";
  import { figureDrawing } from "./evidence.ts";
  import { chartHint, choiceLabel, controlsFrom, draftFrom, drawable, isSwitch, OKABE_ITO, offeredChoices, outcomeSentence, plainNote, previewProblem, requestProblem,
    sections, settingHint, settingLabel, startingColumns, suits, toMm, WIDTHS, type Draft } from "./figure.ts";

  // The figure editor (Glass Box results rule 4). A person chooses a chart the table can draw, maps its
  // columns and sets what journals ask for: titles, legend, colours per group, size in millimetres and typeface. R
  // draws a preview as the settings change and no version is made until the person keeps one;
  // each kept version is its own record, and keeping again from here makes the next version of the same figure. A
  // broker without previews keeps the earlier flow, where each drawing is kept. The colour vision and print-size views
  // change how the drawing is shown, never the figure.
  let { open = $bindable(false), project, sourceTask, table, initial = null, groupsOf, onOpenVersion, onDrawn, notify }: {
    open?: boolean;
    project: string;
    sourceTask: string;
    table: { path: string; title: string; columns: string[]; numeric: boolean[] };
    initial?: { feature: string; controls: FigureControls; title: string; previous: string | null } | null;
    /** The groups in one column of the table, in the order they first appear, for colours per group. */
    groupsOf?: (column: string, signal: AbortSignal) => Promise<{ values: string[]; more: boolean }>;
    onOpenVersion: (task: string) => void;
    // Called when a version finishes, drawn or not, so the page can list it.
    onDrawn?: (task: string) => void;
    notify: (message: string, error?: boolean) => void;
  } = $props();
  const uid = $props.id();

  let dialog = $state<HTMLDialogElement | undefined>(undefined);
  let stage = $state<HTMLElement | undefined>(undefined);
  let catalog = $state<FigureCatalog | null>(null);
  let catalogPhase = $state<"loading" | "ready" | "failed">("loading");
  let featureId = $state("");
  let draft = $state<Draft>({});
  let title = $state("");
  let problems = $state<string[]>([]);
  let phase = $state<"editing" | "sending" | "drawing" | "done">("editing");
  // The newest version kept here, which the next one edits.
  let version = $state<string | null>(null);
  let receipt = $state<FigureReceipt | undefined>(undefined);
  let finalStatus = $state("");
  // What the stage shows: R's preview of the settings, or the version kept last, with its width in inches.
  let shown = $state<{ kind: "preview" | "version"; url: string; widthIn: number } | null>(null);
  // Whether the broker draws previews: unknown until its first answer, then offered or absent for this page.
  let previews = $state<"unknown" | "offered" | "absent">("unknown");
  let previewPhase = $state<"idle" | "drawing" | "refused" | "failed">("idle");
  // The columns a refused preview names: their settings are marked, never worded from the code.
  let refusedColumns = $state<Set<string>>(new Set());
  // Why the last preview did not come, and whether asking again can help.
  let previewTrouble = $state<{ words: string; again: boolean } | null>(null);
  // The chart catalog the shown preview was checked against, and the one the preview of the kept settings used.
  let previewCatalog = "", keptFromPreview = "";
  let keptNote = $state("");
  let previewWords = $state("");
  let previewNotes = $state<string[]>([]);
  let previewFace = $state("");
  // The settings the last preview answered, as the request it was asked with.
  let previewKey = $state("");
  let vision = $state("typical");
  let printSize = $state(false);
  let groups = $state<Record<string, { values: string[]; more: boolean } | "loading" | "failed">>({});
  let requestId = "", lastBody = "";
  let timer: ReturnType<typeof setTimeout> | undefined;
  let controller: AbortController | null = null;
  let previewController: AbortController | null = null;
  // A compute worker draws each preview and the broker waits up to 40 seconds for it; the
  // page waits a little longer before it says the preview did not come, and says so plainly when one takes a while.
  const PREVIEW_LIMIT_MS = 45_000;
  let previewSlow = $state(false);
  // The broker keeps one waiting preview per person, so the page sends one at a time: settings that
  // change while R draws wait, and only the newest of them goes when the drawing comes back.
  let queued: { asked: NonNullable<typeof request>; key: string } | null = null;

  const features = $derived(drawable(catalog, table.columns.length));
  const feature = $derived(features.find((f) => f.id === featureId) ?? null);
  const parts = $derived(feature ? sections(feature) : null);
  const busy = $derived(phase === "sending" || phase === "drawing");
  const editing = $derived(!!initial?.previous || !!version);
  const built = $derived(feature ? controlsFrom(feature, draft, table.columns) : null);
  const request = $derived(feature && built && !built.problems.length
    ? { source_task: sourceTask, table_path: table.path, feature: feature.id, controls: built.controls, title: title.trim() } : null);
  const requestKey = $derived(request ? JSON.stringify(request) : "");
  const fresh = $derived(shown?.kind === "preview" && previewPhase === "idle" && previewKey === requestKey);
  const keeps = $derived(previews === "offered");
  const visionLabel = $derived(VISIONS.find((v) => v.id === vision)?.label ?? "");
  const defaultWidth = () => Number(feature?.params.width?.default) || 7;

  $effect(() => {
    if (!dialog) return;
    if (open && !dialog.open) dialog.showModal();
    if (!open && dialog.open) dialog.close();
  });
  $effect(() => { if (open) untrack(() => void start()); });

  async function start() {
    stop();
    phase = "editing"; problems = []; receipt = undefined; finalStatus = ""; setShown(null);
    previewKey = ""; previewPhase = "idle"; previewWords = ""; previewNotes = []; previewFace = ""; groups = {};
    vision = "typical"; printSize = false;
    version = initial?.previous ?? null;
    title = initial?.title ?? table.title;
    catalogPhase = "loading";
    controller = new AbortController();
    try {
      catalog = await broker.figureCatalog(controller.signal);
      catalogPhase = "ready";
    } catch {
      if (!controller.signal.aborted) catalogPhase = "failed";
      return;
    }
    const first = features.find((f) => f.id === initial?.feature)
      ?? features.find((f) => suits(f, table.columns, table.numeric)) ?? features[0];
    if (first) choose(first, initial?.controls ?? null);
  }

  // Switching charts keeps every setting the new chart also takes, and fills its required columns.
  function choose(next: FigureFeature, controls: FigureControls | null = null) {
    const kept: FigureControls = {};
    if (!controls && feature) {
      const current = controlsFrom(feature, draft, table.columns).controls;
      for (const [name, value] of Object.entries(current)) if (name in next.params && next.params[name].type !== "column") kept[name] = value;
    }
    featureId = next.id;
    draft = draftFrom(next, controls ?? { ...startingColumns(next, table.columns, table.numeric), ...kept });
    problems = [];
  }

  function setShown(next: typeof shown) {
    if (shown) URL.revokeObjectURL(shown.url);
    shown = next;
  }
  function stop() { clearTimeout(timer); controller?.abort(); controller = null; previewController?.abort(); previewController = null; queued = null; }

  // R draws a preview a moment after the settings stop changing; a value that cannot be sent is named instead.
  $effect(() => {
    if (!open || catalogPhase !== "ready" || previews === "absent" || !feature || !built) return;
    const key = requestKey, asked = request, found = built.problems;
    if (key && key === previewKey) return;
    const wait = setTimeout(() => {
      problems = found;
      if (asked) void preview(asked, key);
    }, 650);
    return () => clearTimeout(wait);
  });

  async function preview(asked: NonNullable<typeof request>, key: string) {
    if (previewController) { queued = { asked, key }; return; }
    const c = new AbortController();
    previewController = c;
    previewPhase = "drawing";
    previewSlow = false;
    refusedColumns = new Set();
    const slow = setTimeout(() => { if (previewController === c) previewSlow = true; }, 8000);
    try {
      const answer = await broker.previewFigure(project, asked, AbortSignal.any([c.signal, AbortSignal.timeout(PREVIEW_LIMIT_MS)]));
      if (c.signal.aborted) return;
      // A preview of changed settings replaces the kept version's outcome.
      if (phase === "done") phase = "editing";
      previews = "offered";
      previewKey = key;
      previewNotes = answer.notes ?? [];
      previewTrouble = null;
      previewFace = typeof answer.settings_used?.font_face === "string" ? answer.settings_used.font_face : "";
      if (!answer.preview) {
        previewWords = answer.refused ?? "";
        refusedColumns = new Set(Array.isArray(answer.refusal?.columns) ? answer.refusal.columns : []);
        previewPhase = "refused";
        return;
      }
      // The drawing is shown only when its bytes are the ones the answer names.
      const bytes = Uint8Array.from(atob(answer.preview.svg_base64), (ch) => ch.charCodeAt(0));
      const same = answer.preview.media_type === "image/svg+xml" && bytes.length === answer.preview.size_bytes
        && (await sha256OfBytes(bytes)) === answer.preview.sha256;
      if (c.signal.aborted) return;
      if (!same) { previewPhase = "failed"; return; }
      const width = Number(answer.settings_used?.width);
      setShown({ kind: "preview", url: URL.createObjectURL(new Blob([bytes], { type: "image/svg+xml" })), widthIn: width > 0 ? width : defaultWidth() });
      previewCatalog = answer.catalog_sha256 ?? "";
      previewPhase = "idle";
    } catch (cause) {
      if (c.signal.aborted) return;
      // A broker without previews answers that the route does not exist; the editor then keeps every drawing.
      const missing = cause instanceof BrokerError && (cause.status === 404 || cause.status === 405)
        && !["task_not_found", "project_not_found", "figure_table_unknown"].includes(cause.reason);
      if (missing && previews === "unknown") { previews = "absent"; previewPhase = "idle"; return; }
      if (phase === "done") phase = "editing";
      previewKey = key;
      previewTrouble = cause instanceof BrokerError ? previewProblem(cause.status, cause.reason || cause.code) : previewProblem(0, "");
      previewPhase = "failed";
    } finally {
      clearTimeout(slow);
      if (previewController === c) { previewController = null; previewSlow = false; }
      const next = queued;
      queued = null;
      if (next && open && !c.signal.aborted && next.key !== previewKey) void preview(next.asked, next.key);
    }
  }
  function previewAgain() { previewKey = ""; previewPhase = "idle"; }

  async function draw() {
    if (!feature || busy) return;
    const sent = controlsFrom(feature, draft, table.columns);
    problems = sent.problems;
    if (sent.problems.length) return;
    const body = { source_task: sourceTask, table_path: table.path, feature: feature.id, controls: sent.controls,
      title: title.trim(), previous: version };
    const serialized = JSON.stringify(body);
    // The same request sent again keeps its id, so a repeated press makes one version.
    if (serialized !== lastBody) { lastBody = serialized; requestId = crypto.randomUUID(); }
    phase = "sending";
    keptFromPreview = fresh ? previewCatalog : "";
    keptNote = "";
    // On a phone the settings sit below the figure, so the figure comes back into view while R draws it.
    if (stage && matchMedia("(max-width: 860px)").matches)
      stage.scrollIntoView({ block: "start", behavior: matchMedia("(prefers-reduced-motion: reduce)").matches ? "auto" : "smooth" });
    try {
      const task = await broker.requestFigure(project, { ...body, request_id: requestId });
      phase = "drawing";
      watch(task, Date.now(), typeof sent.controls.width === "number" ? sent.controls.width : defaultWidth());
    } catch (cause) {
      phase = version ? "done" : "editing";
      problems = [requestProblem(cause instanceof BrokerError ? cause.reason || cause.code : "")];
    }
  }

  // Figures are drawn in seconds, so the version is checked every second until it finishes.
  function watch(task: string, started: number, widthIn: number) {
    timer = setTimeout(async () => {
      try {
        const t = await broker.task(task);
        if (!terminal(t.status)) {
          if (Date.now() - started < 5 * 60_000) watch(task, started, widthIn);
          else { phase = "done"; finalStatus = "running"; version = task; }
          return;
        }
        await show(task, t.status, widthIn);
      } catch {
        if (Date.now() - started < 5 * 60_000) watch(task, started, widthIn);
      }
    }, 1000);
  }

  async function show(task: string, status: string, widthIn: number) {
    const c = new AbortController();
    controller = c;
    const figure = await broker.taskFigure(task, c.signal).catch(() => null);
    receipt = figure?.receipts.at(-1);
    // A version is checked against the newest catalog when it is asked for; one that moved since the preview can draw
    // differently from what the person saw.
    keptNote = keptFromPreview && figure?.catalog_sha256 && figure.catalog_sha256 !== keptFromPreview
      ? "R's chart settings changed between the preview and this version, so the version can differ from the preview." : "";
    finalStatus = status;
    version = task;
    if (receipt?.status === "completed") {
      try {
        const drawn = await figureDrawing(task, c.signal);
        if (drawn) setShown({ kind: "version", url: URL.createObjectURL(drawn), widthIn });
      } catch { notify("The figure was drawn, but it could not be shown here. Open the version to see it.", true); }
    } else setShown(null);
    phase = "done";
    onDrawn?.(task);
  }

  // A grouping column's groups are read once per column, for its colours.
  $effect(() => {
    if (!open || !feature || !groupsOf) return;
    for (const param of Object.values(feature.params)) {
      if (param.type !== "colors" || !param.of) continue;
      const column = value(param.of);
      if (column && !untrack(() => groups[column])) untrack(() => void loadGroups(column));
    }
  });
  async function loadGroups(column: string) {
    groups = { ...groups, [column]: "loading" };
    try {
      const found = await groupsOf!(column, controller?.signal ?? new AbortController().signal);
      groups = { ...groups, [column]: found };
    } catch { groups = { ...groups, [column]: "failed" }; }
  }

  // Changing a grouping column clears the colours given to the old column's groups.
  function setValue(name: string, next: string) {
    const out: Draft = { ...draft, [name]: next };
    for (const [other, param] of Object.entries(feature?.params ?? {}))
      if (param.type === "colors" && param.of === name && value(name) !== next) out[other] = "";
    draft = out;
  }
  function toggle(name: string, on: boolean) { draft = { ...draft, [name]: on ? "true" : "" }; }
  const value = (name: string) => { const v = draft[name]; return Array.isArray(v) ? v.join(", ") : v ?? ""; };
  const colours = (name: string): Record<string, string> => { try { return JSON.parse(value(name) || "{}") ?? {}; } catch { return {}; } };
  function setColour(name: string, group: string, hex: string) {
    const map = { ...colours(name) };
    if (hex) map[group] = hex; else delete map[group];
    setValue(name, Object.keys(map).length ? JSON.stringify(map) : "");
  }
  const offered = (param: FigureParam) => OKABE_ITO.filter((c) => !param.choices?.length || param.choices.some((x) => String(x).toLowerCase() === c.hex.toLowerCase()));
  const presets = [{ dpi: "150", label: "Screen" }, { dpi: "300", label: "Print" }, { dpi: "600", label: "Poster" }];
  const kept = $derived(receipt?.status === "completed" && version);
  const alt = $derived(`${title.trim() || feature?.label || "Figure"}, drawn by R from ${table.title}`);

  onDestroy(() => { stop(); setShown(null); });
</script>

{#snippet control(name: string, param: FigureParam, quiet = false)}
  {@const hint = settingHint(name, param, featureId)}
  {@const id = `figure-${name}`}
  {@const described = hint ? `${id}-hint` : undefined}
  {@const label = settingLabel(name, featureId)}
  {#if param.type === "column"}
    {@const named = previewPhase === "refused" && refusedColumns.has(String(value(name)))}
    <div class="field" class:named>
      <label class="name" for={id}>{label}{#if param.required}<span class="need" aria-hidden="true"> needed</span>{/if}</label>
      <select {id} aria-describedby={described} aria-required={param.required ? "true" : undefined} aria-invalid={named ? "true" : undefined} value={value(name)} onchange={(e) => setValue(name, e.currentTarget.value)}>
        {#if !param.required}<option value="">Not used</option>{/if}
        {#each table.columns as column (column)}<option value={column}>{column}</option>{/each}
      </select>
      {#if hint}<span class="hint" id={described}>{hint}</span>{/if}
    </div>
  {:else if param.type === "colors"}
    {@const of = param.of ? value(param.of) : ""}
    {@const read = of ? groups[of] : undefined}
    <fieldset class="colours" aria-describedby={`${id}-hint`}><legend class="name">{label}</legend>
      {#if !param.of || !groupsOf}
        <p class="hint">The groups of this table cannot be read here.</p>
      {:else if !of}
        <p class="hint">Choose a column for {settingLabel(param.of, featureId).toLowerCase()} to give its groups colours.</p>
      {:else if read === "loading" || read === undefined}
        <p class="hint" role="status">Reading the groups in {of}…</p>
      {:else if read === "failed"}
        <p class="hint">The groups in {of} could not be read. R gives them its own colours.</p>
      {:else}
        {@const chosen = colours(name)}
        <div class="colour-rows">
          {#each read.values as group, i (group)}
            <div class="colour-row">
              <label for={`${id}-${i}`}>{group}</label>
              <!-- A data colour from Okabe and Ito's set, drawn as a mark like the figure's own. -->
              <svg class="swatch" viewBox="0 0 18 18" aria-hidden="true"><rect x="1" y="1" width="16" height="16" rx="4" fill={chosen[group] ?? "none"} /></svg>
              <select id={`${id}-${i}`} value={chosen[group] ?? ""} onchange={(e) => setColour(name, group, e.currentTarget.value)}>
                <option value="">R's default</option>
                {#each offered(param) as c (c.hex)}<option value={c.hex}>{c.name}</option>{/each}
              </select>
            </div>
          {/each}
        </div>
        {#if read.more}<p class="hint">The first {read.values.length} groups are listed; the others keep R's colours.</p>{/if}
      {/if}
      <span class="hint" id={`${id}-hint`}>Okabe and Ito's colours, which read with colour blindness.</span>
    </fieldset>
  {:else if param.type === "enum" && isSwitch(param)}
    <div class="switch">
      <input {id} type="checkbox" aria-describedby={described} checked={value(name) === "true"} onchange={(e) => toggle(name, e.currentTarget.checked)} />
      <label class="name" for={id}>{label}</label>
      {#if hint}<span class="hint" id={described}>{hint}</span>{/if}
    </div>
  {:else if param.type === "enum"}
    {@const choices = offeredChoices(name, param)}
    <fieldset class="choices" aria-describedby={described}><legend class={quiet ? "visually-hidden" : "name"}>{label}</legend>
      <div class="options">
        <label class:on={value(name) === ""}><input type="radio" name={id} checked={value(name) === ""} onchange={() => setValue(name, "")} />
          {name === "format" ? "Nothing else" : "Default"}</label>
        {#each choices as choice (choice)}
          <label class:on={value(name).toLowerCase() === String(choice).toLowerCase()}>
            <input type="radio" name={id} checked={value(name).toLowerCase() === String(choice).toLowerCase()} onchange={() => setValue(name, String(choice))} />
            {choiceLabel(name, choice)}</label>
        {/each}
      </div>
      {#if hint}<span class="hint" id={described}>{hint}</span>{/if}
      {#if name === "font" && previewFace}<span class="hint">R drew the last preview in {previewFace}.</span>{/if}
    </fieldset>
  {:else if param.type === "number"}
    <div class="field short">
      <label class="name" for={id}>{label}</label>
      <input {id} type="number" step="any" min={param.min} max={param.max} aria-describedby={described} value={value(name)} placeholder="Default" oninput={(e) => setValue(name, e.currentTarget.value)} />
      {#if hint}<span class="hint" id={described}>{hint}</span>{/if}
    </div>
  {:else if param.type === "numbers"}
    <div class="field">
      <label class="name" for={id}>{label}</label>
      <input {id} type="text" inputmode="decimal" aria-describedby={described} value={value(name)} placeholder={/_limits$/.test(name) ? "For example: 0, 50" : "For example: 1, 2.5"} oninput={(e) => setValue(name, e.currentTarget.value)} />
      {#if hint}<span class="hint" id={described}>{hint}</span>{/if}
    </div>
  {:else if param.type === "string"}
    <div class="field">
      <label class="name" for={id}>{label}</label>
      <input {id} type="text" maxlength={param.maxlen ?? 200} aria-describedby={described} value={value(name)} oninput={(e) => setValue(name, e.currentTarget.value)} />
      {#if hint}<span class="hint" id={described}>{hint}</span>{/if}
    </div>
  {/if}
{/snippet}

<dialog bind:this={dialog} class="studio" onclose={() => { open = false; stop(); }} aria-labelledby="studio-title">
  <header class="bar">
    <div>
      <p class="kicker">{editing ? "A new version of this figure" : "A new figure"} from <span class="table-name">{table.title}</span></p>
      <h2 id="studio-title">{title.trim() || feature?.label || "Figure"}</h2>
    </div>
    <button class="close" onclick={() => (open = false)} aria-label="Close the figure editor">
      <svg viewBox="0 0 16 16" aria-hidden="true"><path d="M3.5 3.5l9 9m0-9l-9 9" /></svg>
    </button>
  </header>

  <!-- The colour vision simulations, applied to the drawing on the stage only. -->
  <svg class="filters" width="0" height="0" aria-hidden="true" focusable="false">
    <defs>
      {#each VISIONS as v (v.id)}
        {#if v.matrix}<filter id={`${uid}-${v.id}`} color-interpolation-filters="linearRGB"><feColorMatrix type="matrix" values={colorMatrixValues(v.matrix)} /></filter>{/if}
      {/each}
    </defs>
  </svg>

  <div class="body">
    <section class="stage" aria-label="The figure" aria-busy={busy || previewPhase === "drawing"} bind:this={stage}>
      {#if shown}
        <div class="stage-bar">
          <p class="state" role="status">
            {#if busy}Keeping this version…
            {:else if shown.kind === "version"}<strong>Kept version</strong>
            {:else if fresh}<strong>Preview, not saved</strong>
            {:else if previewPhase === "drawing"}{previewSlow ? "Drawing a preview of the new settings on a compute worker; this can take up to a minute…" : "Drawing a preview of the new settings…"}
            {:else}Preview of earlier settings, not saved{/if}
          </p>
          <div class="views">
            <label class="vision"><span>Colour vision</span>
              <select bind:value={vision}>{#each VISIONS as v (v.id)}<option value={v.id}>{v.label}</option>{/each}</select></label>
            <label class="print"><input type="checkbox" bind:checked={printSize} />Print size</label>
          </div>
        </div>
      {/if}
      <div class="paper" class:blank={!shown} class:printed={printSize && !!shown}>
        {#if shown}
          <img src={shown.url} {alt} style:filter={vision === "typical" ? null : `url(#${uid}-${vision})`}
            style:width={printSize ? `${toMm(shown.widthIn)}mm` : null} class:stale={shown.kind === "preview" && !fresh} />
        {:else if busy}
          <p class="waiting" role="status"><span class="waveform" aria-hidden="true"><i></i><i></i><i></i></span> R is drawing the figure from the table…</p>
        {:else if previewPhase === "drawing"}
          <p class="waiting" role="status"><span class="waveform" aria-hidden="true"><i></i><i></i><i></i></span> {previewSlow ? "R is drawing a preview on a compute worker; this can take up to a minute…" : "R is drawing a preview…"}</p>
        {:else if phase === "done" && receipt?.status !== "completed"}
          <p class="empty">Nothing was drawn. The reason is below.</p>
        {:else if previews === "absent"}
          <p class="empty">Choose what to draw and press <strong>Draw the figure</strong>. R draws it from the table's own values, and it appears here.</p>
        {:else}
          <p class="empty">R draws a preview from the table's own values as you choose the settings. No version is made until you keep one.</p>
        {/if}
      </div>
      {#if shown && (printSize || vision !== "typical")}
        <div class="view-notes small">
          {#if printSize}<p class="ruler"><span class="tick" aria-hidden="true"></span><span>10 mm at print size. The drawing is {toMm(shown.widthIn)} mm wide on paper; screens differ, so hold a ruler to this bar to check yours.</span></p>{/if}
          {#if vision !== "typical"}<p>A simulation of {visionLabel.toLowerCase()} for checking the colours. The figure does not change.</p>{/if}
        </div>
      {/if}
      {#if phase === "done"}
        <div class="outcome" class:change-bar={!!receipt && receipt.status !== "completed"} role="status">
          <p>{finalStatus === "running" ? "The figure is still being drawn. Open the version to see it when it finishes." : outcomeSentence(receipt, finalStatus)}</p>
          {#if receipt?.notes.length}<ul class="notes">{#each receipt.notes as note (note)}<li>{plainNote(note, featureId)}</li>{/each}</ul>{/if}
          {#if keptNote}<p>{keptNote}</p>{/if}
          {#if version}
            <p class="kept small">{kept ? "Kept as its own version, with where it came from. Change any setting for a new preview, and keep it for another version." : "The attempt is kept on its own page."}
              <button class="link" onclick={() => { open = false; onOpenVersion(version!); }}>Open this version</button></p>
          {/if}
        </div>
      {:else if keeps && previewPhase !== "drawing"}
        <div class="outcome" class:change-bar={previewPhase !== "idle"} role="status">
          {#if previewPhase === "refused"}
            <p>{previewWords || "R would not draw these settings."}</p>
          {:else if previewPhase === "failed"}
            <p>{previewTrouble?.words ?? "The preview could not be drawn just now."}
              {#if previewTrouble?.again ?? true}<button class="link" onclick={previewAgain}>Preview again</button>{/if}</p>
          {:else if fresh && previewNotes.length}
            <p>R changed or left out:</p>
          {/if}
          {#if fresh && previewNotes.length}<ul class="notes">{#each previewNotes as note (note)}<li>{plainNote(note, featureId)}</li>{/each}</ul>{/if}
        </div>
      {/if}
    </section>

    <!-- The editor names each value it cannot send in plain words, so the browser's own checks stay out of the way. -->
    <form class="settings" novalidate onsubmit={(e) => { e.preventDefault(); void draw(); }}>
      {#if catalogPhase === "loading"}
        <p class="soft" role="status">Reading which charts the render service offers…</p>
      {:else if catalogPhase === "failed" || !catalog}
        <p class="soft" role="alert">{catalogPhase === "failed" ? "The charts could not be read just now. Close this and try again." : "The render service has not been reached yet. Try again in a moment."}</p>
      {:else if !features.length}
        <p class="soft">No chart the render service offers fits this table's columns.</p>
      {:else}
        <fieldset class="group"><legend>Chart</legend>
          <div class="charts">
            {#each features as f (f.id)}
              <label class="chart" class:on={f.id === featureId}>
                <input type="radio" name="chart" checked={f.id === featureId} onchange={() => choose(f)} />
                <span class="name">{f.label}</span>{#if chartHint(f)}<span class="hint">{chartHint(f)}</span>{/if}
              </label>
            {/each}
          </div>
        </fieldset>
        {#if feature && parts}
          {#if parts.columns.length}
            <fieldset class="group"><legend>Columns</legend>{#each parts.columns as [name, param] (name)}{@render control(name, param)}{/each}</fieldset>
          {/if}
          {#if parts.angle.length}
            <fieldset class="group"><legend>Angle</legend><div class="trio">{#each parts.angle as [name, param] (name)}{@render control(name, param)}{/each}</div></fieldset>
          {/if}
          <fieldset class="group"><legend>Titles and text</legend>
            <div class="field">
              <label class="name" for="figure-title">Title</label>
              <input id="figure-title" type="text" maxlength="200" aria-describedby="figure-title-hint" bind:value={title} placeholder={feature.label} />
              <span class="hint" id="figure-title-hint">Leave empty for the chart's name.</span>
            </div>
            {#each parts.titles as [name, param] (name)}{@render control(name, param)}{/each}
          </fieldset>
          {#if parts.legend.length}
            <fieldset class="group"><legend>Legend and colours</legend>{#each parts.legend as [name, param] (name)}{@render control(name, param, parts.legend.length === 1)}{/each}</fieldset>
          {/if}
          {#if parts.size.length}
            <fieldset class="group"><legend>Size and resolution</legend>
              {#if feature.params.width}
                <fieldset class="choices"><legend class="name">Width</legend>
                  <div class="options">
                    {#each WIDTHS as w (w.mm)}
                      <label class:on={value("width") === String(w.mm)}><input type="radio" name={`${uid}-width`} checked={value("width") === String(w.mm)} onchange={() => setValue("width", String(w.mm))} />{w.label}, {w.mm} mm</label>
                    {/each}
                    <label class="other"><span class="visually-hidden">Width in mm</span>
                      <input type="number" step="any" placeholder="Other, mm" value={WIDTHS.some((w) => String(w.mm) === value("width")) ? "" : value("width")} oninput={(e) => setValue("width", e.currentTarget.value)} /></label>
                  </div>
                  <span class="hint">{settingHint("width", feature.params.width)}</span>
                </fieldset>
              {/if}
              {#if feature.params.height}
                <div class="field short">
                  <label class="name" for={`${uid}-height`}>Height in mm</label>
                  <input id={`${uid}-height`} type="number" step="any" placeholder="Default" aria-describedby={`${uid}-height-hint`} value={value("height")} oninput={(e) => setValue("height", e.currentTarget.value)} />
                  <span class="hint" id={`${uid}-height-hint`}>{settingHint("height", feature.params.height)}</span>
                </div>
              {/if}
              {#if feature.params.dpi}
                <fieldset class="choices"><legend class="name">{settingLabel("dpi")}</legend>
                  <div class="options">
                    {#each presets as p (p.dpi)}
                      <label class:on={value("dpi") === p.dpi}><input type="radio" name="figure-dpi" checked={value("dpi") === p.dpi} onchange={() => setValue("dpi", p.dpi)} />{p.label}, {p.dpi}</label>
                    {/each}
                    <label class="other"><span class="visually-hidden">Other resolution</span>
                      <input type="number" min={feature.params.dpi.min} max={feature.params.dpi.max} step="1" placeholder="Other" value={presets.some((p) => p.dpi === value("dpi")) ? "" : value("dpi")} oninput={(e) => setValue("dpi", e.currentTarget.value)} /></label>
                  </div>
                  <span class="hint">{settingHint("dpi", feature.params.dpi)}</span>
                </fieldset>
              {/if}
              {#each parts.size.filter(([n]) => n === "format") as [name, param] (name)}{@render control(name, param)}{/each}
            </fieldset>
          {/if}
          {#if parts.more.length}
            <details class="more"><summary>More settings</summary>
              <div class="more-body">{#each parts.more as [name, param] (name)}{@render control(name, param)}{/each}</div>
            </details>
          {/if}
        {/if}
      {/if}
      {#if problems.length}
        <ul class="problems" role="alert">{#each problems as problem (problem)}<li>{problem}</li>{/each}</ul>
      {/if}
      <div class="actions">
        <button class="button" type="submit" disabled={busy || !feature}>{busy ? (keeps ? "Keeping…" : "Drawing…") : keeps ? "Keep this version" : version ? "Draw this version" : "Draw the figure"}</button>
        <p class="small soft">{keeps ? "Previews are not saved. Keeping one makes a version, with the table and settings R drew it from."
          : "R draws it on the cluster with the reviewed plot code. Every drawing is kept as its own version."}</p>
      </div>
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
  .filters { position: absolute; width: 0; height: 0; overflow: hidden; }
  .bar { display: flex; align-items: flex-start; justify-content: space-between; gap: 16px; padding: 16px var(--gutter) 14px;
    border-bottom: 1px solid var(--line); background: var(--surface); }
  .kicker { font-size: var(--font-small); color: var(--text-2); }
  .table-name { font-family: var(--serif); color: var(--heading); }
  h2 { font-size: calc(22px * var(--type-scale)); letter-spacing: -0.01em; line-height: 1.2; margin-top: 2px; }
  .close { width: 40px; height: 40px; border-radius: var(--r-1); display: grid; place-items: center; color: var(--text-2); flex: none; }
  .close:hover { background: var(--hover-wash); color: var(--heading); }
  .close svg { width: 16px; height: 16px; stroke: currentColor; stroke-width: 1.6; fill: none; }
  .body { display: grid; grid-template-columns: minmax(0, 1fr) minmax(320px, 420px); grid-template-rows: minmax(0, 1fr); min-height: 0; }
  .stage { min-height: 0; overflow: auto; padding: 22px var(--gutter) 28px; background: var(--stage); display: grid; gap: 12px; align-content: start; }
  .stage-bar { display: flex; flex-wrap: wrap; align-items: center; justify-content: space-between; gap: 8px 18px; }
  .state { font-size: var(--font-small); color: var(--text-2); }
  .state strong { color: var(--heading); }
  .views { display: flex; flex-wrap: wrap; align-items: center; gap: 8px 16px; font-size: var(--font-small); }
  .vision { display: inline-flex; align-items: center; gap: 8px; color: var(--text-2); white-space: nowrap; }
  .vision select { min-height: 36px; }
  .print { display: inline-flex; align-items: center; gap: 6px; min-height: 36px; color: var(--text); cursor: pointer; }
  /* R draws for white paper, so the figure always sits on white, whatever the theme. */
  .paper { background: var(--paper); box-shadow: 0 0 0 1px var(--plate-edge); min-height: 320px; display: grid; place-items: center; padding: 18px; }
  .paper img { display: block; max-width: 100%; max-height: calc(100dvh - 300px); width: auto; height: auto; animation: develop 240ms var(--ease); transition: opacity 160ms var(--ease); }
  .paper img.stale { opacity: 0.55; }
  .paper.printed { place-items: start; overflow: auto; }
  .paper.printed img { max-width: none; max-height: none; height: auto; }
  @keyframes develop { from { opacity: 0; } }
  @media (prefers-reduced-motion: reduce) { .paper img { animation: none; transition: none; } }
  .paper.blank p { max-width: 42ch; text-align: center; color: #355B85; font-size: var(--font-small); line-height: 1.55; }
  .view-notes { display: grid; gap: 4px; color: var(--text-2); max-width: 72ch; }
  .ruler { display: grid; grid-template-columns: 10mm minmax(0, 1fr); align-items: baseline; gap: 10px; }
  .ruler .tick { display: inline-block; width: 10mm; height: 6px; border: solid var(--heading); border-width: 0 1px 1px; flex: none; }
  .waiting { display: flex; align-items: center; gap: 10px; }
  .outcome { display: grid; gap: 8px; max-width: 72ch; }
  .outcome.change-bar { padding-left: 14px; }
  .notes { margin: 0; padding-left: 18px; font-size: var(--font-small); color: var(--text-2); display: grid; gap: 4px; }
  .kept { color: var(--text-2); display: flex; flex-wrap: wrap; gap: 4px 10px; align-items: baseline; }
  /* The settings scroll on their own; they also hold the hidden radio inputs, so nothing escapes to scroll the sheet. */
  .settings { position: relative; min-height: 0; overflow: auto; padding: 22px var(--gutter) 28px 26px; border-left: 1px solid var(--line); display: grid; gap: 22px; align-content: start; }
  .group { border: 0; margin: 0; padding: 0; display: grid; gap: 12px; min-width: 0; }
  .group > legend { padding: 0 0 8px; margin-bottom: 4px; width: 100%; border-bottom: 2px solid var(--heading); font-weight: 700;
    font-size: calc(15px * var(--type-scale)); color: var(--heading); }
  .charts { display: grid; gap: 2px; }
  .chart { position: relative; display: grid; grid-template-columns: 18px minmax(0, 1fr); column-gap: 10px; padding: 8px 10px 8px 12px; cursor: pointer; }
  .chart input { grid-row: span 2; margin-top: 3px; }
  .chart:hover { background: var(--hover-wash); }
  .chart.on { background: var(--surface-2); }
  .chart.on::before { content: ""; position: absolute; left: 0; top: 6px; bottom: 6px; width: 3px; background: var(--mark); }
  .name { font-weight: 600; color: var(--heading); font-size: calc(14px * var(--type-scale)); }
  .need { font-weight: 400; color: var(--text-3); font-size: var(--font-xs); }
  .hint { display: block; font-size: var(--font-xs); line-height: 1.45; color: var(--text-3); }
  .field { display: grid; gap: 5px; min-width: 0; align-content: start; }
  .field input, .field select { width: 100%; }
  .field.short input { max-width: 140px; }
  /* A column a refused preview names: the setting to change. */
  .field.named select { border-color: var(--failed); box-shadow: inset 3px 0 0 var(--failed); }
  .trio { display: grid; grid-template-columns: repeat(auto-fit, minmax(110px, 1fr)); gap: 12px; }
  .trio .field.short input { max-width: none; }
  .choices, .colours { border: 0; margin: 0; padding: 0; display: grid; gap: 6px; min-width: 0; }
  .choices > legend, .colours > legend { padding: 0; margin-bottom: 6px; }
  .options { display: flex; flex-wrap: wrap; gap: 6px; }
  .options > label { position: relative; display: inline-flex; align-items: center; gap: 6px; padding: 6px 11px; border-radius: var(--r-1);
    box-shadow: inset 0 0 0 1px var(--line-2); font-size: var(--font-small); cursor: pointer; color: var(--text); }
  .options > label:hover { box-shadow: inset 0 0 0 1px var(--heading); }
  .options > label.on { box-shadow: inset 0 0 0 2px var(--heading); color: var(--heading); font-weight: 600; }
  .options > label input[type="radio"] { position: absolute; opacity: 0; pointer-events: none; }
  .options > label:has(input:focus-visible) { outline: 2px solid var(--focus); outline-offset: 2px; }
  .options .other { padding: 0; box-shadow: none; }
  .options .other input { width: 110px; }
  .colour-rows { display: grid; gap: 6px; }
  .colour-row { display: grid; grid-template-columns: minmax(0, 1fr) 18px minmax(0, 11rem); align-items: center; gap: 10px; }
  .colour-row label { font-size: var(--font-small); color: var(--text); overflow-wrap: anywhere; }
  .swatch { width: 18px; height: 18px; }
  .swatch rect { stroke: var(--line-2); stroke-width: 1; }
  .switch { display: grid; grid-template-columns: 18px minmax(0, 1fr); column-gap: 10px; row-gap: 2px; align-items: start; }
  .switch input { margin-top: 3px; }
  .switch label { cursor: pointer; }
  .switch .hint { grid-column: 2; }
  .more { border-top: 1px solid var(--line); padding-top: 12px; }
  .more summary { cursor: pointer; font-weight: 600; color: var(--heading); }
  .more-body { display: grid; gap: 14px; padding-top: 14px; }
  .problems { margin: 0; padding: 0 0 0 14px; display: grid; gap: 4px; font-size: var(--font-small); color: var(--failed); border-left: 3px solid var(--failed); list-style: none; }
  .actions { display: grid; gap: 8px; justify-items: start; padding-top: 6px; position: sticky; bottom: -28px; background: var(--page); padding-bottom: 6px; }
  @media (max-width: 860px) {
    /* One scrolling column on phones: the figure first, then its settings, each as tall as its content. */
    .body { display: block; overflow: auto; }
    .stage, .settings { overflow: visible; }
    .stage { padding: 18px var(--gutter); }
    .paper { min-height: 220px; }
    .paper img { max-height: 55vh; }
    .settings { border-left: 0; border-top: 1px solid var(--line); padding: 20px var(--gutter) 28px; }
    .actions .button { width: 100%; }
  }
</style>
