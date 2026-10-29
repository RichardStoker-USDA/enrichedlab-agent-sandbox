<script lang="ts">
  import { onDestroy } from "svelte";
  import type { PDFDocumentProxy } from "pdfjs-dist";
  import FigureCompanion from "./FigureCompanion.svelte";
  import type { FigureDataSource } from "./figure_data.ts";
  import { drawPage, openPdf } from "./pdf.ts";
  import type { ViewedFile } from "./viewer.ts";
  import type { ViewTransitionStart } from "./route.ts";

  // One figure on its plate. SVG and raster originals are drawn by the browser; an R PDF
  // is drawn from its own bytes by PDF.js. The bytes are checksum-verified first.
  // `made` names the code and input files in the same retained package; the note says
  // what made the figure without inferring anything the package does not record.
  let { file, load, title, onDownload, original = null, onOriginal, number, made, onShowMethod,
    companion = null, exploreOpen, onExploreChange }: {
    file: ViewedFile;
    load: (signal: AbortSignal) => Promise<Blob>;
    title: string;
    onDownload: () => void;
    original?: ViewedFile | null;
    onOriginal?: () => void;
    number?: number;
    made?: { code: string[]; inputs: string[]; writer?: "model" | "configured" | "render" | "template" | null };
    onShowMethod?: () => void;
    companion?: FigureDataSource | null;
    /** Controlled by the page when the figure record offers the same action; local otherwise. */
    exploreOpen?: boolean;
    onExploreChange?: (open: boolean) => void;
  } = $props();
  const base = (path: string) => path.split("/").at(-1) ?? path;

  const isPdf = $derived(file.media_type === "application/pdf");
  let phase = $state<"waiting" | "loading" | "ready" | "failed">("waiting");
  let imageUrl = $state("");
  let pages = $state(1);
  let page = $state(1);
  // An analysis package's SVG is the platform's copy of R's PDF; the render service's R draws its SVG itself.
  const sourceNote = $derived(isPdf ? `R PDF${pages > 1 ? `, ${pages} pages` : ""}` : file.media_type === "image/svg+xml"
    ? (original && made?.writer !== "render" ? "SVG copy of the R PDF" : "SVG from R") : "Image from R");
  let plateOpen = $state(false);
  let zoom = $state(1);
  let host = $state<HTMLElement | undefined>(undefined);
  let canvas = $state<HTMLCanvasElement | undefined>(undefined);
  let plateCanvas = $state<HTMLCanvasElement | undefined>(undefined);
  let plateDialog = $state<HTMLDialogElement | undefined>(undefined);
  let plateStage = $state<HTMLElement | undefined>(undefined);
  let document_: PDFDocumentProxy | null = null;
  let closePdf: (() => Promise<void>) | null = null;
  let controller: AbortController | null = null;
  let observer: IntersectionObserver | null = null;
  let resize: ResizeObserver | null = null;
  let redraw: ReturnType<typeof setTimeout> | undefined;
  let localExplore = $state(false);
  const exploring = $derived(!!companion && (exploreOpen ?? localExplore));

  function setExplore(open: boolean) {
    localExplore = open;
    onExploreChange?.(open);
  }

  async function start() {
    if (phase !== "waiting") return;
    phase = "loading";
    controller = new AbortController();
    try {
      const blob = await load(controller.signal);
      if (isPdf) {
        const opened = await openPdf(await blob.arrayBuffer());
        document_ = opened.document;
        closePdf = opened.close;
        pages = document_.numPages;
        phase = "ready";
      } else {
        imageUrl = URL.createObjectURL(new Blob([blob], { type: file.media_type }));
        phase = "ready";
      }
    } catch {
      if (!controller.signal.aborted) phase = "failed";
    }
  }

  async function drawInline() {
    if (!document_ || !canvas || !host) return;
    const plate = host.querySelector<HTMLElement>(".plate");
    await drawPage(document_, page, canvas, Math.max(240, (plate?.clientWidth ?? host.clientWidth) - 30), true);
  }
  async function drawPlate() {
    if (!document_ || !plateCanvas || !plateStage) return;
    await drawPage(document_, page, plateCanvas, (plateStage.clientWidth - 48) * zoom);
  }

  $effect(() => {
    if (!host) return;
    observer = new IntersectionObserver((entries) => { if (entries.some((e) => e.isIntersecting)) { void start(); observer?.disconnect(); } }, { rootMargin: "400px" });
    observer.observe(host);
    resize = new ResizeObserver(() => { clearTimeout(redraw); redraw = setTimeout(() => void drawInline(), 120); });
    resize.observe(host);
    return () => { observer?.disconnect(); resize?.disconnect(); };
  });
  $effect(() => { void page; if (phase === "ready" && isPdf) { void drawInline(); if (plateOpen) void drawPlate(); } });
  $effect(() => {
    if (!plateDialog) return;
    if (plateOpen && !plateDialog.open) { plateDialog.showModal(); zoom = 1; queueMicrotask(() => void drawPlate()); }
    if (!plateOpen && plateDialog.open) plateDialog.close();
  });
  $effect(() => { void zoom; if (plateOpen && isPdf) { clearTimeout(redraw); redraw = setTimeout(() => void drawPlate(), 90); } });

  function openPlate() {
    const start = (document as Document & { startViewTransition?: ViewTransitionStart }).startViewTransition;
    const reduced = matchMedia("(prefers-reduced-motion: reduce)").matches;
    if (start && !reduced && document.visibilityState !== "hidden") start.call(document, () => { plateOpen = true; })?.ready?.catch(() => {});
    else plateOpen = true;
  }
  function keys(event: KeyboardEvent) {
    if (event.key === "+" || event.key === "=") { event.preventDefault(); zoom = Math.min(6, +(zoom * 1.25).toFixed(2)); }
    if (event.key === "-") { event.preventDefault(); zoom = Math.max(1, +(zoom / 1.25).toFixed(2)); }
    if (event.key === "0") { event.preventDefault(); zoom = 1; }
  }
  onDestroy(() => {
    controller?.abort();
    clearTimeout(redraw);
    if (imageUrl) URL.revokeObjectURL(imageUrl);
    void closePdf?.();
  });
</script>

<figure class="figure" class:with-companion={exploring} bind:this={host} data-path={file.path}>
  {#if companion}
    <div class="figure-toolbar">
      <button class="link explore" aria-expanded={exploring} onclick={() => setExplore(!exploring)}>{exploring ? "Close data view" : "Explore data"}</button>
    </div>
  {/if}
  <div class="view-pair" class:open={exploring}>
    <div class="figure-panel">
      <button class="plate" onclick={openPlate} disabled={phase !== "ready"} aria-label={`Enlarge ${number ? `figure ${number}, ` : ""}${title}`}>
        {#if phase === "ready" && isPdf}<span class="pdf" role="img" aria-label={title}><canvas bind:this={canvas} aria-hidden="true"></canvas></span>
        {:else if phase === "ready"}<img src={imageUrl} alt={title} />
        {:else if phase === "failed"}<span class="placeholder failed">This figure could not be verified or drawn. Download the original.</span>
        {:else}<span class="placeholder" aria-live="polite">Opening the original figure…</span>{/if}
      </button>
    </div>
    {#if exploring && companion}
      <div class="companion-panel"><FigureCompanion source={companion} /></div>
    {/if}
  </div>
  <aside class="note" aria-label={`About ${title}`}>
    <p class="file">{base(file.path)}</p>
    <p>{sourceNote}{#if phase === "ready"}. Checksum matches{/if}.</p>
    {#if made?.writer === "model"}<p class="generated">Model-written R in the reviewed template. Unvalidated until reviewed.</p>
    {:else if made?.writer === "configured"}<p class="generated">A configured recipe in the reviewed template, with settings the agent chose. Unvalidated until reviewed.</p>
    {:else if made?.writer === "render"}<p class="generated">Drawn by the reviewed R render service from the table's own values, with the settings chosen for it.</p>
    {:else if made?.writer === "template"}<p class="generated">Drawn by the reviewed R analysis template with its results, from the table's own values.</p>{/if}
    {#if made && (made.code.length || made.inputs.length)}
      <p>Made by {#each made.code as path, i (path)}{i ? ", " : ""}<span class="file">{base(path)}</span>{/each}{#if made.inputs.length}{" from "}{#each made.inputs as path, i (path)}{i ? ", " : ""}<span class="file">{base(path)}</span>{/each}{/if}.</p>
      {#if onShowMethod}<button class="link" onclick={onShowMethod}>Show how this was made</button>{/if}
    {/if}
    <p class="actions">
      <button class="link" onclick={openPlate} disabled={phase !== "ready"}>Enlarge</button>
      {#if original && onOriginal}<button class="link" onclick={onOriginal}>Download PDF</button><button class="link" onclick={onDownload}>SVG</button>
      {:else}<button class="link" onclick={onDownload}>Download</button>{/if}
    </p>
  </aside>
  <figcaption>
    {#if number}<strong>Figure {number}.</strong>{/if} <span class="name">{title}.</span>
    {#if pages > 1}<span class="pager">Page {page} of {pages}
      <button class="link" disabled={page === 1} onclick={() => page--}>Previous</button>
      <button class="link" disabled={page === pages} onclick={() => page++}>Next</button></span>{/if}
  </figcaption>
</figure>

<dialog class="plate-view" bind:this={plateDialog} onclose={() => (plateOpen = false)} onkeydown={keys} aria-label={`${title}, enlarged`}>
  <header>
    <strong>{title}</strong>
    <div class="zoom" role="group" aria-label="Zoom">
      <button onclick={() => (zoom = Math.max(1, +(zoom / 1.25).toFixed(2)))} disabled={zoom <= 1} aria-label="Zoom out">−</button>
      <span>{Math.round(zoom * 100)}%</span>
      <button onclick={() => (zoom = Math.min(6, +(zoom * 1.25).toFixed(2)))} disabled={zoom >= 6} aria-label="Zoom in">+</button>
      <button class="fit" onclick={() => (zoom = 1)}>Fit</button>
    </div>
    <button class="done" onclick={() => (plateOpen = false)}>Close</button>
  </header>
  <div class="stage" bind:this={plateStage}>
    {#if plateOpen}
      {#if isPdf}<span class="pdf" role="img" aria-label={title}><canvas bind:this={plateCanvas} aria-hidden="true"></canvas></span>
      {:else}<img src={imageUrl} alt={title} style={`width:${zoom * 100}%`} />{/if}
    {/if}
  </div>
  <footer class="soft small">The file shown is the retained original. Zoom changes only the view. Use + and − to zoom, 0 to fit, Esc to close.</footer>
</dialog>

<style>
  .figure { display: grid; grid-template-columns: minmax(0, 1fr); gap: 10px 32px; min-width: 0; }
  .figure-toolbar { display: flex; flex-wrap: wrap; justify-content: flex-end; align-items: center; gap: 8px 14px; min-height: 36px; border-bottom: 1px solid var(--line); }
  .figure-toolbar .explore { min-height: 36px; font-weight: 650; }
  .view-pair, .figure-panel, .companion-panel { min-width: 0; }
  .view-pair { display: grid; gap: 18px; }
  .figure-panel { display: grid; align-content: start; }
  /* R draws for white paper, so the figure sits on paper in both themes. */
  .plate {
    display: grid; place-items: center; width: 100%; min-height: 240px; padding: 14px;
    background: var(--stage); border-radius: var(--r-1); cursor: zoom-in;
    transition: box-shadow 160ms var(--ease);
  }
  .plate:hover:not(:disabled) { box-shadow: inset 0 0 0 1px var(--line-2); }
  .plate:disabled { cursor: default; opacity: 1; }
  .plate img, .plate canvas { max-width: 100%; height: auto; background: var(--paper); box-shadow: 0 0 0 1px rgb(0 0 0 / 0.06); }
  .pdf { display: block; max-width: 100%; line-height: 0; }
  .placeholder { color: var(--text-2); font-size: var(--font-small); }
  .placeholder.failed { color: var(--failed); }
  figcaption { font-size: var(--font-small); color: var(--text); max-width: 72ch; }
  figcaption strong { color: var(--heading); margin-right: 4px; }
  figcaption .name { font-family: var(--serif); font-size: calc(15px * var(--type-scale)); color: var(--heading); }
  .pager { margin-left: 10px; color: var(--text-2); display: inline-flex; gap: 10px; }
  .note { display: grid; gap: 5px; align-content: start; font-size: var(--font-xs); color: var(--text-2); }
  .note .file { font-family: var(--mono); font-size: 0.95em; color: var(--text); overflow-wrap: anywhere; }
  .note .link { justify-self: start; font-size: var(--font-xs); }
  .note .generated { color: var(--heading); font-weight: 600; padding-left: 8px; border-left: 3px solid var(--mark); }
  .actions { display: flex; flex-wrap: wrap; gap: 12px; margin-top: 2px; }
  figcaption { order: 1; }
  .note { order: 2; }
  .figure-toolbar, .view-pair { order: 0; }
  @media (min-width: 900px) {
    .view-pair.open { grid-template-columns: minmax(300px, 1fr) minmax(340px, 1fr); align-items: start; }
  }
  @media (min-width: 1180px) {
    .figure { grid-template-columns: minmax(0, 1fr) 200px; }
    .figure-toolbar { grid-column: 1; grid-row: 1; }
    .view-pair { grid-column: 1; grid-row: 2; }
    figcaption { grid-column: 1; grid-row: 3; }
    .note { grid-column: 2; grid-row: 1 / span 3; padding-top: 4px; border-top: 1px solid var(--line); }
  }
  /* The light table: a dark stage in both themes so the white figure reads as paper. */
  .plate-view { width: 100vw; height: 100vh; max-width: none; max-height: none; margin: 0; padding: 0; border: 0; background: var(--light-table); color: #fff; }
  .plate-view[open] { display: grid; grid-template-rows: auto minmax(0, 1fr) auto; }
  .plate-view::backdrop { background: var(--light-table); }
  .plate-view header { display: flex; align-items: center; gap: 18px; padding: 12px var(--gutter); border-bottom: 1px solid rgb(255 255 255 / 0.16); }
  .plate-view header strong { flex: 1; min-width: 0; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; font-weight: 650; }
  .zoom { display: flex; align-items: center; gap: 4px; font-variant-numeric: tabular-nums; font-size: 13px; }
  .zoom button { min-width: 36px; height: 36px; border-radius: var(--r-2); font-size: 18px; color: #fff; box-shadow: inset 0 0 0 1px rgb(255 255 255 / 0.28); }
  .zoom button:hover:not(:disabled) { background: rgb(255 255 255 / 0.12); }
  .zoom .fit { font-size: 13px; padding: 0 10px; margin-left: 6px; }
  .zoom span { min-width: 48px; text-align: center; color: rgb(255 255 255 / 0.82); }
  .done { min-height: 36px; padding: 0 14px; border-radius: var(--r-2); background: var(--aggie-gold); color: var(--aggie-blue); font-weight: 650; font-size: 13px; }
  .stage { overflow: auto; padding: 24px; display: grid; place-items: start center; }
  .stage img, .stage canvas { background: #fff; max-width: none; }
  .plate-view footer { padding: 8px var(--gutter) 12px; color: rgb(255 255 255 / 0.58); font-size: 12px; }
</style>
