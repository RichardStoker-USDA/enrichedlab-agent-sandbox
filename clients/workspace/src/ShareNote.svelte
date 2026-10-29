<script lang="ts">
  import type { LabNote } from "./contracts.ts";
  import { DEFAULT_KEEP_DAYS, keepChoices, labNotes, MAX_SHARE_REASON, noteFailure, reasonProblem } from "./lab_notes.ts";

  // Share one of the agent's notes with the lab. The request key is kept for an unchanged form, so a
  // repeated click shares once; the broker decides whether the note may be shared.
  let { project, revision, lesson, labName, onShared }: {
    project: string; revision: number; labName: string;
    lesson: { statement: string; evidence_refs: string[] };
    onShared: (note: LabNote) => void;
  } = $props();

  let open = $state(false);
  let keep = $state<number>(DEFAULT_KEEP_DAYS);
  let reason = $state("");
  let sending = $state(false);
  let failure = $state("");
  let key = "", body = "";

  async function share(event: SubmitEvent) {
    event.preventDefault();
    const problem = reasonProblem(reason, false);
    if (problem) { failure = problem; return; }
    const input = { revision, statement: lesson.statement, evidence_refs: lesson.evidence_refs, reason: reason.trim() || null, review_days: keep };
    const next = JSON.stringify(input);
    if (next !== body) { body = next; key = crypto.randomUUID(); }
    sending = true; failure = "";
    try {
      const note = await labNotes.share(project, { ...input, request_id: key });
      open = false; reason = ""; body = "";
      onShared(note);
    } catch (cause) {
      failure = noteFailure(cause, "share");
    } finally { sending = false; }
  }
</script>

{#if !open}
  <button class="link small" onclick={() => { open = true; failure = ""; }}>Share with the lab</button>
{:else}
  <form class="share" onsubmit={share} aria-label="Share this note with the lab">
    <p class="small">Everyone in {labName} can read a shared note. The agent reads it in their projects that start from the same data or use the same methods, as background, never as their results.</p>
    <div class="fields">
      <label>Keep it in use for
        <select bind:value={keep}>
          {#each keepChoices as choice (choice.days)}<option value={choice.days}>{choice.label}</option>{/each}
        </select>
      </label>
      <label class="why">Why share it <span class="soft">(optional)</span>
        <textarea bind:value={reason} rows="2" maxlength={MAX_SHARE_REASON} placeholder="For example: Checked against the raw measurements."></textarea>
      </label>
    </div>
    <div class="row">
      <button class="button" type="submit" disabled={sending}>{sending ? "Sharing…" : "Share with the lab"}</button>
      <button class="link" type="button" onclick={() => { open = false; failure = ""; }}>Cancel</button>
    </div>
    {#if failure}<p class="notice error" role="alert">{failure}</p>{/if}
  </form>
{/if}

<style>
  .share { display: grid; gap: 10px; width: 100%; max-width: 640px; padding: 12px 0 4px; }
  .share p { max-width: 64ch; }
  .fields { display: grid; gap: 10px; }
  label { display: grid; gap: 4px; font-size: var(--font-small); color: var(--text-2); }
  select { justify-self: start; min-height: 36px; }
  textarea { width: 100%; }
  .row { display: flex; flex-wrap: wrap; align-items: center; gap: 10px 20px; }
</style>
