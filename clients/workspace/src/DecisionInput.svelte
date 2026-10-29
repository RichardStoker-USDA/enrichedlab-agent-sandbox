<script lang="ts">
  import { BrokerError } from "./api.ts";
  import { contextSummary } from "./context_summary.ts";
  import type { DecisionInputs } from "./history.ts";
  import { projects, type KeptText } from "./projects.ts";

  // What a decision's models were given, kept byte for byte. It can hold passages from the lab's
  // papers and its shared notes, so the broker serves it only to project readers who are lab members; the page shows
  // it and offers no download. The text is checked here against the SHA-256 the decision recorded. A plain summary,
  // read from the same text by fixed code, comes first; the full text opens on request.
  let { input }: { input: DecisionInputs } = $props();
  type Which = "lead" | "review";
  let open = $state(false);
  let which = $state<Which>("lead");
  let exact = $state(false);
  let full = $state(false);
  let loading = $state(false);
  let failure = $state("");
  let texts = $state<Partial<Record<Which, { kept: KeptText; matches: boolean }>>>({});
  const current = $derived(texts[which]);
  const summary = $derived(current ? contextSummary(current.kept.text) : null);
  const formatted = $derived.by(() => {
    if (!current) return "";
    try { return JSON.stringify(JSON.parse(current.kept.text), null, 2); } catch { return current.kept.text; }
  });

  async function sha256(text: string): Promise<string> {
    const digest = await crypto.subtle.digest("SHA-256", new TextEncoder().encode(text));
    return "sha256:" + [...new Uint8Array(digest)].map((b) => b.toString(16).padStart(2, "0")).join("");
  }

  async function show(next: Which) {
    which = next;
    const address = next === "lead" ? input.lead : input.review;
    if (!address || texts[next]) return;
    loading = true; failure = "";
    try {
      const kept = await projects.context(input.run, address);
      texts = { ...texts, [next]: { kept, matches: (await sha256(kept.text)) === address && kept.sha256 === address } };
    } catch (cause) {
      const reason = cause instanceof BrokerError ? cause.reason : "";
      failure = reason === "context_lab_members_only" ? "Only members of this lab can read what the agent was given."
        : reason === "context_not_kept" ? "This decision was made before its exact input was kept."
        : "The input could not be loaded. Try again.";
    } finally {
      loading = false;
    }
  }

  function toggle() {
    open = !open;
    if (open) void show(which);
  }
</script>

<div class="decision-input">
  <button class="link small" aria-expanded={open} onclick={toggle}>{open ? "Hide what the agent was given" : "What the agent was given"}</button>
  {#if open}
    <div class="panel">
      {#if input.review}
        <div class="which" role="group" aria-label="Whose input">
          <button aria-pressed={which === "lead"} onclick={() => show("lead")}>Research agent</button>
          <button aria-pressed={which === "review"} onclick={() => show("review")}>Methods check</button>
        </div>
      {/if}
      {#if loading}
        <p class="small soft">Loading…</p>
      {:else if failure}
        <p class="small">{failure}</p>
      {:else if current}
        <p class="small soft">
          {current.matches ? "Checked in your browser: this is the exact text the decision recorded" : "This text does not match the record of the decision"}
          ({current.kept.bytes.toLocaleString()} bytes). It is shown here only and cannot be downloaded.
        </p>
        {#if summary}
          <dl class="summary" aria-label={which === "lead" ? "Summary of what the research agent was given" : "Summary of what the methods check was given"}>
            {#each summary as item (item.label)}
              <div><dt>{item.label}</dt><dd>{item.text}{#if item.list?.length}<ul>{#each item.list as line, i (i)}<li>{line}</li>{/each}</ul>{/if}</dd></div>
            {/each}
          </dl>
          <button class="link small" aria-expanded={full} onclick={() => (full = !full)}>{full ? "Hide the full text" : "Show the full text"}</button>
        {/if}
        {#if full || !summary}
          <label class="small exact"><input type="checkbox" bind:checked={exact} /> Show the exact text as sent</label>
          <pre class:mismatch={!current.matches} aria-label={which === "lead" ? "What the research agent was given" : "What the methods check was given"}>{exact ? current.kept.text : formatted}</pre>
        {/if}
      {/if}
    </div>
  {/if}
</div>

<style>
  .decision-input { display: grid; gap: 6px; justify-items: start; width: 100%; }
  .panel { display: grid; gap: 8px; width: 100%; padding: 10px 0 2px 12px; border-left: 2px solid var(--line-2); }
  .which { display: flex; gap: 4px 18px; }
  .which button { padding: 0; min-height: 30px; font-size: var(--font-small); font-weight: 600; color: var(--text-2); border-bottom: 2px solid transparent; }
  .which button[aria-pressed="true"] { color: var(--heading); border-bottom-color: var(--mark); }
  .exact { display: inline-flex; align-items: center; gap: 6px; color: var(--text-2); }
  .summary { display: grid; gap: 8px; margin: 0; width: 100%; max-width: 68ch; }
  .panel > .link { justify-self: start; }
  .summary div { display: grid; grid-template-columns: minmax(9rem, 13rem) 1fr; gap: 2px 14px; }
  .summary dt { font-size: var(--font-small); font-weight: 600; color: var(--text-2); }
  .summary dd { margin: 0; font-size: var(--font-small); color: var(--text); overflow-wrap: anywhere; }
  .summary ul { margin: 4px 0 0; padding-left: 1.1em; display: grid; gap: 2px; color: var(--text-2); }
  @media (max-width: 560px) { .summary div { grid-template-columns: 1fr; } }
  pre { margin: 0; max-height: 26rem; overflow: auto; padding: 10px 12px; background: var(--surface-2, transparent);
    border: 1px solid var(--line); font: var(--font-xs) / 1.55 var(--mono); color: var(--text); white-space: pre-wrap; overflow-wrap: anywhere; }
  pre.mismatch { border-color: var(--failed); }
</style>
