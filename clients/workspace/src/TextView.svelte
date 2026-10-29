<script lang="ts">
  import { onDestroy } from "svelte";
  import { strictText } from "./artifact_content.ts";
  import { MAX_TEXT_BYTES, type ViewedFile } from "./viewer.ts";

  // Code, logs and notes shown as escaped original text. Parameters that are a flat
  // JSON object also read as a two-column list; the original text stays one click away.
  let { file, load, onDownload, title, code = false, showTitle = true }: {
    file: ViewedFile; load: (signal: AbortSignal) => Promise<Blob>; onDownload: () => void; title: string; code?: boolean; showTitle?: boolean;
  } = $props();
  let text = $state<string | null>(null);
  let failure = $state("");
  let raw = $state(false);
  let wrap = $state(false);
  let controller: AbortController | null = null;
  const lines = $derived(text === null ? [] : text.replace(/\n$/, "").split("\n"));
  const flat = $derived.by(() => {
    if (text === null || !/json$/.test(file.media_type)) return null;
    try {
      const value = JSON.parse(text);
      if (!value || typeof value !== "object" || Array.isArray(value)) return null;
      const entries = Object.entries(value);
      return entries.every(([, v]) => v === null || ["string", "number", "boolean"].includes(typeof v)) ? entries.map(([k, v]) => [k, String(v)]) : null;
    } catch { return null; }
  });

  $effect(() => {
    void file.sha256;
    text = null; failure = ""; raw = false; wrap = !code;
    if (file.size_bytes > MAX_TEXT_BYTES) { failure = "large"; return; }
    controller?.abort();
    const c = new AbortController();
    controller = c;
    void load(c.signal).then((blob) => strictText(blob, MAX_TEXT_BYTES)).then((value) => { if (!c.signal.aborted) text = value; }).catch(() => { if (!c.signal.aborted) failure = "unreadable"; });
    return () => c.abort();
  });
  onDestroy(() => controller?.abort());
</script>

<section class="text-view" aria-label={title}>
  <header>
    <div>{#if showTitle}<strong>{title}</strong>{/if}<span class="soft small">{file.path}{#if text !== null}{" · "}{lines.length.toLocaleString()} {lines.length === 1 ? "line" : "lines"} · checksum verified{/if}</span></div>
    <div class="tools">
      {#if flat}<button class="link" onclick={() => (raw = !raw)}>{raw ? "Show as a list" : "Show original text"}</button>{/if}
      {#if text !== null && !flat}<button class="link" onclick={() => (wrap = !wrap)}>{wrap ? "Do not wrap lines" : "Wrap lines"}</button>{/if}
      <button class="link" onclick={onDownload}>Download</button>
    </div>
  </header>
  {#if failure === "large"}<p class="soft small">This file is too large to show here. The download is the complete original.</p>
  {:else if failure}<p class="soft small">This file is not plain text that can be shown without changes. Download the original.</p>
  {:else if text === null}<p class="soft small">Opening…</p>
  {:else if flat && !raw}
    <dl class="pairs">{#each flat as [key, value]}<dt>{key}</dt><dd>{value}</dd>{/each}</dl>
  {:else}
    <!-- svelte-ignore a11y_no_noninteractive_tabindex -->
    <div class="scroll" class:wrap tabindex="0" role="region" aria-label={`${title} text`}>
      <ol>{#each lines as line}<li><code>{line}</code></li>{/each}</ol>
    </div>
  {/if}
</section>

<style>
  .text-view { display: grid; gap: 10px; min-width: 0; }
  header { display: flex; justify-content: space-between; align-items: end; gap: 10px 20px; flex-wrap: wrap; }
  header strong { display: block; color: var(--heading); }
  header span { display: block; overflow-wrap: anywhere; }
  .tools { display: flex; gap: 14px; font-size: 13px; }
  .scroll { max-height: 460px; overflow: auto; border-radius: 8px; background: var(--plate); box-shadow: inset 0 0 0 1px var(--plate-edge); }
  ol { margin: 0; padding: 12px 0; counter-reset: line; list-style: none; }
  li { display: grid; grid-template-columns: 3.4em minmax(0, 1fr); counter-increment: line; }
  li::before { content: counter(line); padding-right: 12px; text-align: right; color: var(--ink-soft); font: 12px/1.7 var(--mono); user-select: none; }
  code { font: 12.5px/1.7 var(--mono); white-space: pre; color: var(--ink); padding-right: 16px; }
  .wrap code { white-space: pre-wrap; overflow-wrap: anywhere; }
  .pairs { display: grid; grid-template-columns: minmax(120px, max-content) minmax(0, 1fr); gap: 0; margin: 0; border-radius: 8px; box-shadow: inset 0 0 0 1px var(--rule); overflow: hidden; }
  .pairs dt, .pairs dd { margin: 0; padding: 8px 14px; border-bottom: 1px solid var(--rule); font-size: 14px; }
  .pairs dt { color: var(--ink-soft); background: var(--plate); }
  .pairs dd { font-family: var(--mono); font-size: 13px; overflow-wrap: anywhere; }
</style>
