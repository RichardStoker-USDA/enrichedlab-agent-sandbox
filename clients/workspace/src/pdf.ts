// PDF.js loads only when a PDF figure is shown. It draws the original R PDF; it never
// replaces the downloadable publication file.
import type { PDFDocumentProxy } from "pdfjs-dist";

let library: Promise<typeof import("pdfjs-dist")> | null = null;

function pdfjs() {
  library ??= (async () => {
    const [module, worker] = await Promise.all([
      import("pdfjs-dist"),
      import("pdfjs-dist/build/pdf.worker.min.mjs?url"),
    ]);
    module.GlobalWorkerOptions.workerSrc = worker.default;
    return module;
  })();
  return library;
}

// The caller keeps `close` and calls it when the figure leaves the page; it stops the
// worker's work for this document.
export async function openPdf(bytes: ArrayBuffer): Promise<{ document: PDFDocumentProxy; close: () => Promise<void> }> {
  const module = await pdfjs();
  const task = module.getDocument({
    data: new Uint8Array(bytes),
    useWasm: false,
    enableXfa: false,
    standardFontDataUrl: "/assets/pdfjs-fonts/",
  });
  return { document: await task.promise, close: () => task.destroy() };
}

// Draws in flight, per canvas. PDF.js refuses two renders on one canvas, and resize,
// page and zoom changes can each start one.
const active = new WeakMap<HTMLCanvasElement, { cancel: () => void; done: Promise<unknown> }>();
const latest = new WeakMap<HTMLCanvasElement, number>();

// Draw one page to fit `width` CSS pixels, sharp on high-density screens. Canvas area
// is bounded so deep zoom cannot exhaust browser memory. A newer call for the same
// canvas cancels an older one; the superseded call returns null.
// With `natural`, the page is never drawn wider than its printed size at 96 CSS px per
// inch, so an R PDF and its SVG copy appear at the same size in the page.
export async function drawPage(document: PDFDocumentProxy, pageNumber: number, canvas: HTMLCanvasElement, width: number, natural = false) {
  const mine = (latest.get(canvas) ?? 0) + 1;
  latest.set(canvas, mine);
  const previous = active.get(canvas);
  if (previous) { previous.cancel(); await previous.done.catch(() => undefined); }
  const page = await document.getPage(pageNumber);
  if (latest.get(canvas) !== mine) return null;
  const base = page.getViewport({ scale: 1 });
  const ratio = Math.min(window.devicePixelRatio || 1, 3);
  let scale = (natural ? Math.min(width, (base.width * 96) / 72) : width) / base.width;
  const maxPixels = 40_000_000;
  if (base.width * base.height * (scale * ratio) ** 2 > maxPixels) scale = Math.sqrt(maxPixels / (base.width * base.height)) / ratio;
  const viewport = page.getViewport({ scale: scale * ratio });
  canvas.width = Math.floor(viewport.width);
  canvas.height = Math.floor(viewport.height);
  canvas.style.width = `${Math.floor(viewport.width / ratio)}px`;
  canvas.style.height = `${Math.floor(viewport.height / ratio)}px`;
  const task = page.render({ canvas, viewport });
  const done = task.promise;
  active.set(canvas, { cancel: () => task.cancel(), done });
  try {
    await done;
  } catch (error) {
    if ((error as { name?: string }).name === "RenderingCancelledException") return null;
    throw error;
  } finally {
    if (active.get(canvas)?.done === done) active.delete(canvas);
  }
  return { width: base.width, height: base.height };
}
