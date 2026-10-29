<script lang="ts">
  import FigureView from "../FigureView.svelte";
  import type { ViewContext } from "./types.ts";

  // An R figure or an image as its plate, drawn from its own checked bytes. A figure cannot point at one row, so a pick
  // made in another view shows in the frame's caption instead.
  let { ctx }: { ctx: ViewContext } = $props();
</script>

<FigureView file={ctx.file} load={(signal) => ctx.read(signal)} title={ctx.title} onDownload={ctx.download}
  original={ctx.original?.file ?? null} onOriginal={ctx.original?.download} />
