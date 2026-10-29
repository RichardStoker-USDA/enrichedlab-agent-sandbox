<script lang="ts">
  import { onDestroy } from "svelte";
  import type { PDFDocumentProxy } from "pdfjs-dist";
  import { drawPage, openPdf } from "./pdf.ts";
  import type { ViewedFile } from "./viewer.ts";

  // A small plate of one retained figure for a project's contents page. It reads the same
  // checksum-verified file the analysis shows, only when the plate scrolls near, and draws
  // it on white paper. Opening it hands the drawn plate to the analysis page, so the figure
  // the reader chose is the one they arrive at.
  // `lead` draws the project's lead figure at reading size, `width` CSS pixels wide.
  let { file, load, title, number, onOpen, width = 220, lead = false }: {
    file: ViewedFile;
    load: (signal: AbortSignal) => Promise<Blob>;
    title: string;
    number: number;
    onOpen: (preview: string, element: HTMLElement) => void;
    width?: number;
    lead?: boolean;
  } = $props();

  const isPdf = $derived(file.media_type === "application/pdf");
  let phase = $state<"waiting" | "loading" | "ready" | "failed">("waiting");
  let imageUrl = $state("");
  let host = $state<HTMLElement | undefined>(undefined);
  let canvas = $state<HTMLCanvasElement | undefined>(undefined);
  let document_: PDFDocumentProxy | null = null;
  let closePdf: (() => Promise<void>) | null = null;
  const controller = new AbortController();

  async function start() {
    if (phase !== "waiting") return;
    phase = "loading";
    try {
      const blob = await load(controller.signal);
      if (isPdf) {
        const opened = await openPdf(await blob.arrayBuffer());
        document_ = opened.document; closePdf = opened.close;
      } else {
        imageUrl = URL.createObjectURL(new Blob([blob], { type: file.media_type }));
      }
      phase = "ready";
    } catch {
      if (!controller.signal.aborted) phase = "failed";
    }
  }
  $effect(() => {
    if (!host) return;
    const observer = new IntersectionObserver((entries) => { if (entries.some((e) => e.isIntersecting)) { void start(); observer.disconnect(); } }, { rootMargin: "300px" });
    observer.observe(host);
    return () => observer.disconnect();
  });
  $effect(() => { if (phase === "ready" && isPdf && canvas && document_) void drawPage(document_, 1, canvas, width); });

  function open() {
    if (!host) return;
    let preview = imageUrl;
    if (isPdf && canvas) { try { preview = canvas.toDataURL("image/png"); } catch { preview = ""; } }
    onOpen(phase === "ready" ? preview : "", host);
  }
  onDestroy(() => { controller.abort(); if (imageUrl) setTimeout(() => URL.revokeObjectURL(imageUrl), 2000); void closePdf?.(); });
</script>

<button class="plate" class:lead bind:this={host} onclick={open} aria-label={`Open figure ${number}, ${title}, in its analysis`}>
  <span class="paper">
    {#if phase === "ready" && isPdf}<canvas bind:this={canvas} aria-hidden="true"></canvas>
    {:else if phase === "ready"}<img src={imageUrl} alt="" />
    {:else if phase === "failed"}<span class="note">Not verified</span>
    {:else}<span class="note" aria-hidden="true"></span>{/if}
  </span>
  <span class="cap"><strong>Figure {number}.</strong> {title}</span>
</button>

<style>
  .plate { display: grid; gap: 8px; width: 100%; text-align: left; align-content: start; }
  .paper {
    display: grid; place-items: center; aspect-ratio: 4 / 3; overflow: hidden; background: #ffffff;
    box-shadow: inset 0 0 0 1px var(--line); border-radius: 2px; transition: box-shadow 160ms var(--ease), transform 160ms var(--ease);
  }
  .plate:hover .paper, .plate:focus-visible .paper { box-shadow: inset 0 0 0 1px var(--line-2), 0 1px 0 0 var(--mark); transform: translateY(-1px); }
  .paper img, .paper canvas { display: block; max-width: 100%; max-height: 100%; object-fit: contain; }
  .note { font-size: var(--font-xs); color: #4c4c4c; }
  .cap { font-size: var(--font-xs); line-height: 1.4; color: var(--text-2); display: -webkit-box; -webkit-line-clamp: 2; line-clamp: 2; -webkit-box-orient: vertical; overflow: hidden; }
  .cap strong { color: var(--heading); font-weight: 650; }
  .lead .paper { aspect-ratio: 7 / 5; border-radius: 3px; }
  .lead .cap { font-size: var(--font-small); -webkit-line-clamp: 3; line-clamp: 3; }
  @media (prefers-reduced-motion: reduce) { .paper { transition: none; } .plate:hover .paper { transform: none; } }
</style>
