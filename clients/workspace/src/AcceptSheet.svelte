<script module lang="ts">
  export interface Acceptable { task_id: string; title: string; method: string; written: boolean; checks: string; review: string; rests: boolean }
</script>

<script lang="ts">
  import { broker, BrokerError } from "./api.ts";
  import Dialog from "./Dialog.svelte";
  import { MAX_REVIEW_NOTE, reviewClaim } from "./person_review.ts";

  // Accept the result (Glass Box, "Your next step"): records the reader's own review, accepted, on each finished
  // analysis no person has accepted yet. The sheet names each analysis, its checks and any earlier review
  // first and offers to read it, so accepting is a decision about named records, never a single click past them. Each
  // review keeps its own request key while the note is unchanged, so sending again after a lost answer saves each once.
  let { open = $bindable(false), analyses, openTask, onDone }: {
    open?: boolean; analyses: Acceptable[]; openTask: (task: string) => void; onDone: (accepted: number) => void;
  } = $props();

  let note = $state(""), busy = $state(false), failure = $state("");
  let keys: Record<string, string> = {}, keyedNote = "";
  let saved = $state<string[]>([]);
  const left = $derived(analyses.filter((a) => !saved.includes(a.task_id)));
  const tooLong = $derived(note.trim().length > MAX_REVIEW_NOTE);

  let wasOpen = false;
  $effect(() => {
    const now = open;
    if (now && !wasOpen) { note = ""; failure = ""; saved = []; keys = {}; keyedNote = ""; }
    wasOpen = now;
  });

  async function accept() {
    if (busy || !left.length || tooLong) return;
    const clean = note.trim();
    if (clean !== keyedNote) { keyedNote = clean; keys = {}; }
    busy = true; failure = "";
    try {
      for (const item of left) {
        keys[item.task_id] ??= crypto.randomUUID();
        await broker.review(item.task_id, { decision: "accepted", note: clean || null, request_id: keys[item.task_id] });
        saved = [...saved, item.task_id];
      }
      const count = saved.length;
      open = false;
      onDone(count);
    } catch (cause) {
      const some = saved.length ? `${saved.length} of ${analyses.length} accepted. ` : "";
      failure = cause instanceof BrokerError && cause.code === "access_required" ? `${some}Your account can no longer review this project.`
        : cause instanceof BrokerError && cause.code === "request_conflict" ? `${some}An analysis changed while you read it. Close this, read it again and accept it from its page.`
        : `${some}The rest were not saved. Try again; nothing is saved twice.`;
    } finally { busy = false; }
  }
</script>

<Dialog bind:open title="Accept the result" wide sheet>
  <p>{analyses.length === 1 ? "Accepting records your review on this analysis: you read its inputs, code and checks, and you accept its results for this project."
    : `Accepting records your review on each of these ${analyses.length} analyses: you read their inputs, code and checks, and you accept their results for this project.`}
    {analyses.length === 1 ? "The review shows your name." : "Each review shows your name."}</p>
  <p class="small soft">{reviewClaim}</p>
  <ul class="grouped accept-list" aria-label="The analyses you accept">
    {#each analyses as item (item.task_id)}
      <li class="accept-row">
        <span class="accept-body">
          <span class="accept-title">{item.title}</span>
          <span class="accept-meta">{[item.rests ? "The answer's findings come from it" : "", item.method, item.checks].filter(Boolean).join(" · ")}</span>
          {#if item.review}<span class="accept-meta">{item.review}. Your review becomes its current one.</span>{/if}
          {#if item.written}<span class="accept-meta">The agent wrote its code; it stays unvalidated until a person reads it.</span>{/if}
        </span>
        {#if saved.includes(item.task_id)}<span class="accept-done">Accepted</span>
        {:else}<button class="link small" onclick={() => { open = false; openTask(item.task_id); }}>Read it first</button>{/if}
      </li>
    {/each}
  </ul>
  <label><span id="accept-note-name">A note with your review</span> <small id="accept-note-hint">Optional. It is saved with each review.</small>
    <textarea bind:value={note} rows="3" aria-labelledby="accept-note-name" aria-describedby="accept-note-hint"></textarea></label>
  {#if tooLong}<p class="notice error" role="alert">Keep the note to {MAX_REVIEW_NOTE.toLocaleString("en-US")} characters or fewer.</p>{/if}
  {#if failure}<p class="notice error" role="alert">{failure}</p>{/if}
  {#snippet footer()}
    <button class="link" onclick={() => (open = false)} disabled={busy}>Cancel</button>
    <button class="button" onclick={() => void accept()} disabled={busy || !left.length || tooLong}>
      {busy ? "Saving…" : left.length === 1 ? "Accept the analysis" : `Accept ${left.length} analyses`}</button>
  {/snippet}
</Dialog>

<style>
  .accept-list { box-shadow: var(--group-edge); }
  .accept-row { display: flex; align-items: center; gap: 14px; padding: 12px 14px; }
  .accept-body { flex: 1; min-width: 0; display: grid; gap: 2px; }
  .accept-title { font-weight: 650; color: var(--heading); }
  .accept-meta { font-size: var(--font-small); color: var(--text-2); }
  .accept-done { font-size: var(--font-small); font-weight: 650; color: var(--text-2); }
</style>
