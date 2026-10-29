<script lang="ts">
  import type { Snippet } from "svelte";

  // A native modal dialog: the browser supplies the focus trap, Escape and the inert page. A `sheet` rises from the
  // bottom edge on a phone, the way a setup sheet does on the platform it imitates.
  let { open = $bindable(false), title, wide = false, sheet = false, onclose, children, footer }: {
    open?: boolean; title: string; wide?: boolean; sheet?: boolean; onclose?: () => void; children: Snippet; footer?: Snippet;
  } = $props();
  let dialog = $state<HTMLDialogElement | undefined>(undefined);
  // Several dialogs share a page; each needs its own title id for its accessible name.
  const uid = $props.id();

  $effect(() => {
    if (!dialog) return;
    if (open && !dialog.open) dialog.showModal();
    if (!open && dialog.open) dialog.close();
  });
  function closed() {
    open = false;
    onclose?.();
  }
</script>

<dialog bind:this={dialog} class:wide class:sheet onclose={closed} aria-labelledby={`dialog-${uid}`}>
  <header>
    <h2 id={`dialog-${uid}`}>{title}</h2>
    <button class="close" onclick={() => (open = false)} aria-label="Close">
      <svg viewBox="0 0 16 16" aria-hidden="true"><path d="M3.5 3.5l9 9m0-9l-9 9" /></svg>
    </button>
  </header>
  <div class="body">{@render children()}</div>
  {#if footer}<footer>{@render footer()}</footer>{/if}
</dialog>

<style>
  dialog {
    width: min(560px, calc(100vw - 24px)); max-height: min(86vh, 900px); padding: 0;
    border: 1px solid var(--rule); border-radius: 10px; background: var(--page); color: var(--ink);
    box-shadow: 0 24px 60px rgb(2 40 81 / 0.28);
  }
  dialog.wide { width: min(880px, calc(100vw - 24px)); }
  dialog::backdrop { background: var(--scrim); }
  dialog[open] { display: flex; flex-direction: column; animation: rise 180ms var(--ease); }
  @keyframes rise { from { opacity: 0; transform: translateY(8px); } }
  header { display: flex; align-items: center; justify-content: space-between; gap: 16px; padding: 18px 22px 12px; }
  h2 { font-size: 18px; }
  .close { width: 36px; height: 36px; border-radius: 6px; display: grid; place-items: center; color: var(--ink-soft); }
  .close:hover { background: var(--hover-wash); color: var(--heading); }
  .close svg { width: 16px; height: 16px; stroke: currentColor; stroke-width: 1.6; fill: none; }
  .body { padding: 4px 22px 18px; overflow: auto; display: grid; gap: 16px; }
  footer { display: flex; justify-content: flex-end; align-items: center; gap: 12px; flex-wrap: wrap; padding: 14px 22px 18px; border-top: 1px solid var(--rule); }
  @media (max-width: 640px) {
    dialog.sheet { width: 100vw; max-width: 100vw; max-height: 92dvh; margin: auto 0 0; border-radius: var(--r-4) var(--r-4) 0 0; border-width: 1px 0 0; }
    dialog.sheet[open] { animation: sheet-up 280ms var(--ease); }
    dialog.sheet footer { padding-bottom: calc(16px + env(safe-area-inset-bottom, 0px)); }
    @keyframes sheet-up { from { transform: translateY(100%); } }
  }
</style>
