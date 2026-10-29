<script lang="ts">
  import type { Tone } from "./language.ts";
  // `wrap` lets a long label (a reviewer's name, a finding's mark) break across lines at phone width.
  let { tone, label, wrap = false }: { tone: Tone; label: string; wrap?: boolean } = $props();
</script>

<span class={`status ${tone}`} class:wrap>
  {#if tone === "active"}<span class="waveform" aria-hidden="true"><i></i><i></i><i></i></span>
  {:else}
    <svg class="glyph" viewBox="0 0 12 12" aria-hidden="true">
      {#if tone === "done"}<path d="M2 6.4l2.6 2.6L10 3.2" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linecap="round" stroke-linejoin="round" />
      {:else if tone === "failed"}<path d="M3 3l6 6M9 3L3 9" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linecap="round" />
      {:else if tone === "attention"}<rect x="2.5" y="2.5" width="7" height="7" fill="currentColor" />
      {:else}<circle cx="6" cy="6" r="3.6" fill="none" stroke="currentColor" stroke-width="1.5" />{/if}
    </svg>
  {/if}
  {label}
</span>
