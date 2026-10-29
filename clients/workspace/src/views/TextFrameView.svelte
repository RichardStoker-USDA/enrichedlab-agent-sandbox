<script lang="ts">
  import TextView from "../TextView.svelte";
  import type { ViewContext } from "./types.ts";

  // Code, logs, notes and records as their escaped original text.
  let { ctx }: { ctx: ViewContext } = $props();
</script>

<TextView file={ctx.file} load={(signal) => ctx.read(signal)} title={ctx.title} onDownload={ctx.download} showTitle={false}
  code={/\.(r|py|sql|sh|mjs|js)$/i.test(ctx.file.path)} />
