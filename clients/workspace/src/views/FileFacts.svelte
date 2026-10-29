<script lang="ts">
  import { bytes } from "../language.ts";
  import type { ViewContext } from "./types.ts";

  // A file no registered kind draws is still a file: its name, size and checksum, and the original to download.
  let { ctx }: { ctx: ViewContext } = $props();
</script>

<section class="download-only">
  <p class="soft small">{ctx.file.path} · {bytes(ctx.file.size_bytes)}. This format has no reader in the browser. The download is checked against its recorded checksum before it is saved.</p>
  <button class="button secondary" onclick={ctx.download}>Download the original</button>
</section>

<style>
  .download-only { display: grid; gap: 10px; justify-items: start; padding: 18px; border-radius: 8px; background: var(--plate); box-shadow: inset 0 0 0 1px var(--plate-edge); }
</style>
