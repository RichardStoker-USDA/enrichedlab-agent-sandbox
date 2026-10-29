<script lang="ts">
  import { onMount, tick } from "svelte";
  import ResultFrame from "./ResultFrame.svelte";
  import type { QuickItem, ResultFile } from "./results.ts";
  import type { FigureHandle } from "./views/types.ts";

  // Quick Look (Glass Box, results rule 1): Space on a file in a list opens it larger in its result frame; the left and
  // right arrow keys move to the previous and next file, Space or Escape closes it, and focus returns to the file the
  // reader ended on. On a phone the frame is a sheet from the bottom edge. Keys inside a view (a table, the 3D view, the
  // switch between views) stay with the view.
  let { items, index = $bindable(0), onclose, figure = null }: { items: QuickItem[]; index?: number; onclose: (key: string) => void;
    figure?: ((file: ResultFile) => FigureHandle | null) | null } = $props();
  const uid = $props.id();
  let dialog = $state<HTMLDialogElement>();
  const item = $derived(items[Math.min(Math.max(index, 0), items.length - 1)]);

  onMount(() => {
    dialog?.showModal();
    dialog?.focus();
  });
  async function move(by: number) {
    const next = index + by;
    if (next < 0 || next >= items.length) return;
    index = next;
    await tick();
  }
  const ownKeys = "[data-own-keys], [role=tablist], [role=application], [role=radiogroup], input, textarea, select, summary";
  function keys(event: KeyboardEvent) {
    const target = event.target as HTMLElement;
    if (target.closest(ownKeys)) return;
    if (event.key === "ArrowRight") { event.preventDefault(); void move(1); }
    else if (event.key === "ArrowLeft") { event.preventDefault(); void move(-1); }
    else if (event.key === " " && target === dialog) { event.preventDefault(); dialog?.close(); }
  }
</script>

<dialog class="quick-look" bind:this={dialog} tabindex="-1" aria-labelledby={`${uid}-name`} onkeydown={keys} onclose={() => onclose(item.key)}>
  <div class="bar">
    <p class="count" id={`${uid}-name`}><span class="visually-hidden">Quick Look: {item.title}, </span><span>{index + 1} of {items.length}</span></p>
    <div class="nav">
      <button class="icon" aria-label="Previous file" disabled={index === 0} onclick={() => move(-1)}>
        <svg viewBox="0 0 16 16" aria-hidden="true"><path d="M10 3.5 5.5 8l4.5 4.5" /></svg></button>
      <button class="icon" aria-label="Next file" disabled={index === items.length - 1} onclick={() => move(1)}>
        <svg viewBox="0 0 16 16" aria-hidden="true"><path d="M6 3.5 10.5 8 6 12.5" /></svg></button>
    </div>
    <p class="hint">Left and right arrows move between files</p>
    <button class="close" onclick={() => dialog?.close()}>Close</button>
  </div>
  <div class="body">
    {#key item.key}
      {#if item.result}
        <ResultFrame chrome="full" subject={item.result} group={item.group ?? []} title={item.title} made={item.made ?? null} {figure} />
      {:else if item.note}
        <section class="note-only" aria-label={item.title}>
          <h2>{item.title}</h2>
          <p class="small">{item.note.text}</p>
          <button class="button" onclick={item.note.run}>{item.note.action}</button>
        </section>
      {/if}
    {/key}
  </div>
</dialog>

<style>
  .quick-look {
    width: min(1120px, calc(100vw - 48px)); max-width: none; height: min(880px, calc(100dvh - 48px)); max-height: none; padding: 0;
    border: 0; border-radius: var(--r-4); background: var(--page); color: var(--ink); box-shadow: 0 30px 80px rgb(0 0 0 / 0.32), 0 0 0 1px var(--rule);
  }
  .quick-look[open] { display: grid; grid-template-rows: auto minmax(0, 1fr); animation: look 200ms var(--ease); }
  .quick-look::backdrop { background: var(--scrim); }
  /* The open sheet itself holds focus so the arrow keys move between files; the sheet is its own indicator. */
  .quick-look:focus { outline: none; }
  @keyframes look { from { opacity: 0; transform: scale(0.985); } }
  .bar { display: flex; align-items: center; gap: 8px 14px; padding: 10px 14px 10px 20px; border-bottom: 1px solid var(--rule); }
  .count { font-size: var(--font-small); font-weight: 600; color: var(--text-2); font-variant-numeric: tabular-nums; }
  .nav { display: flex; gap: 2px; }
  .icon { width: 44px; height: 44px; border-radius: var(--r-2); display: grid; place-items: center; color: var(--heading); }
  .icon:hover:not(:disabled) { background: var(--hover-wash); }
  .icon:disabled { color: var(--text-3); }
  .icon svg { width: 16px; height: 16px; fill: none; stroke: currentColor; stroke-width: 1.8; stroke-linecap: round; stroke-linejoin: round; }
  .hint { flex: 1; font-size: var(--font-xs); color: var(--text-3); }
  .close { min-height: 44px; padding: 0 14px; border-radius: var(--r-2); font-weight: 600; color: var(--heading); }
  .close:hover { background: var(--hover-wash); }
  .body { overflow: auto; padding: 20px 24px 28px; overscroll-behavior: contain; }
  .note-only { display: grid; gap: 10px; justify-items: start; max-width: 64ch; }
  .note-only h2 { font-size: calc(20px * var(--type-scale)); color: var(--heading); }
  @media (max-width: 640px) {
    .quick-look { width: 100vw; height: auto; max-height: 92dvh; margin: auto 0 0; border-radius: var(--r-4) var(--r-4) 0 0; }
    .quick-look[open] { animation: sheet-up 280ms var(--ease); }
    @keyframes sheet-up { from { transform: translateY(100%); } }
    .hint { display: none; }
    .count { flex: 1; }
    .bar { padding: 6px 8px 6px 16px; }
    .body { padding: 16px 16px calc(24px + env(safe-area-inset-bottom, 0px)); }
  }
</style>
