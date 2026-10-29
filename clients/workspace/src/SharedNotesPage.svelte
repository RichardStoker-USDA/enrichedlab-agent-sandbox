<script lang="ts">
  import { onDestroy, untrack } from "svelte";
  import type { Identity, LabNote, LabNotes } from "./contracts.ts";
  import { currentLab } from "./lab.svelte.ts";
  import { day, DEFAULT_KEEP_DAYS, keepChoices, labNotes, MAX_SHARE_REASON, noteActions, noteClaim, noteFailure, noteStatus, reasonProblem } from "./lab_notes.ts";
  import { covers, live } from "./live.ts";
  import { href } from "./route.ts";
  import StatusMark from "./StatusMark.svelte";

  // The lab's shared notes: what people shared from their projects' reviewed analyses, whether the
  // agent reads each one now, and, for the people allowed to, renewing and retiring. The broker decides every state.
  let { identity, notify }: { identity: Identity; notify: (message: string, error?: boolean) => void } = $props();

  const group = $derived(currentLab(identity));
  let view = $state<LabNotes | null>(null);
  let loading = $state(true);
  let failure = $state("");
  let controller: AbortController | null = null;

  async function load(quiet = false) {
    const key = group?.key;
    if (!key) { loading = false; return; }
    controller?.abort();
    const c = new AbortController();
    controller = c;
    loading = true; if (!quiet) failure = "";
    try {
      const next = await labNotes.list(key, c.signal);
      if (!c.signal.aborted) { view = next; failure = ""; }
    } catch {
      if (!c.signal.aborted && !(quiet && view)) failure = "The lab's shared notes could not be loaded. Nothing has changed; reload to try again.";
    } finally {
      if (!c.signal.aborted) loading = false;
    }
  }
  $effect(() => { void group?.key; untrack(() => { view = null; editing = null; void load(); }); });
  // A change to the lab's records reads the page again in place (live.ts); a failed quiet read keeps what it shows.
  const unwatch = live().watch(["lab:*"], (changed) => { if (group && covers(changed, `lab:${group.key}`)) return load(true); }, { gapMs: 2_000 });
  onDestroy(() => { controller?.abort(); unwatch(); });

  const profileName = (key: string) => identity.profiles.find((p) => p.key === key)?.name ?? "a method";
  const person = (id: string, name: string | null) => (id === identity.principal.id ? "you" : name?.trim() || "a colleague");
  const inUse = $derived(view?.notes.filter((n) => n.status.state === "active") ?? []);
  const held = $derived(view?.notes.filter((n) => n.status.state === "paused" || n.status.state === "expired") ?? []);
  const retired = $derived(view?.notes.filter((n) => n.status.state === "retired") ?? []);
  function lastChange(note: LabNote): string {
    const e = note.events.filter((x) => x.kind === "renewed").at(-1);
    return e ? `Renewed by ${e.actor_name?.trim() || "a colleague"} on ${day(e.recorded_at)}.` : "";
  }

  // One inline form at a time. The request key is kept while the form is unchanged, so a repeated click saves once.
  let editing = $state<{ note: string; action: "retire" | "renew" } | null>(null);
  let keep = $state<number>(DEFAULT_KEEP_DAYS);
  let reason = $state("");
  let busy = $state(false);
  let formFailure = $state("");
  let key = "", body = "";
  function openForm(note: LabNote, action: "retire" | "renew") {
    editing = { note: note.note_id, action }; keep = DEFAULT_KEEP_DAYS; reason = ""; formFailure = "";
  }
  async function submit(note: LabNote, event: SubmitEvent) {
    event.preventDefault();
    if (!editing || busy) return;
    const action = editing.action;
    if (action === "retire") {
      const problem = reasonProblem(reason, true);
      if (problem) { formFailure = problem; return; }
    }
    const next = JSON.stringify({ note: note.note_id, action, keep, reason: reason.trim() });
    if (next !== body) { body = next; key = crypto.randomUUID(); }
    busy = true; formFailure = "";
    try {
      const updated = action === "renew"
        ? await labNotes.renew(note.note_id, { review_days: keep, request_id: key })
        : await labNotes.retire(note.note_id, { reason: reason.trim(), request_id: key });
      if (view) view = { ...view, notes: view.notes.map((n) => (n.note_id === updated.note_id ? updated : n)) };
      editing = null; body = "";
      notify(action === "renew" ? "The note is renewed." : "The note is retired. The agent no longer reads it.");
    } catch (cause) {
      formFailure = noteFailure(cause, action);
    } finally { busy = false; }
  }
</script>

<section class="notes-page" aria-labelledby="notes-title">
  <header>
    <h1 id="notes-title">Shared notes</h1>
    {#if group}<p>Notes people in {group.name} shared from their projects. {noteClaim}</p>{/if}
  </header>
  {#if !group}<p class="soft">Your account is not in a lab yet.</p>
  {:else if loading && !view}<p class="soft" role="status">Opening the lab's shared notes…</p>
  {:else if failure}<p class="notice error" role="alert">{failure}</p>
  {:else if view && !view.notes.length}
    <p class="soft empty">No note has been shared in {group.name} yet. A lab lead or reviewer can share one from a project's notes once a person accepts the analyses it rests on.</p>
  {:else if view}
    {#if inUse.length}<h2 class="group-title">In use</h2>{@render list(inUse)}{/if}
    {#if held.length}<h2 class="group-title">Not in use</h2>{@render list(held)}{/if}
    {#if retired.length}
      <details class="retired">
        <summary>Retired notes ({retired.length})</summary>
        {@render list(retired)}
      </details>
    {/if}
  {/if}
</section>

{#snippet list(items: LabNote[])}
  <ol class="notes">
    {#each items as note (note.note_id)}
      {@const status = noteStatus(note)}
      {@const can = noteActions(note, identity.principal.id, view?.can_curate ?? false)}
      {@const change = lastChange(note)}
      <li>
        <p class="statement">{note.statement}</p>
        <p class="state"><StatusMark tone={status.tone} label={status.label} wrap /> <span class="small">{status.sentence}</span></p>
        <p class="small meta">Shared by {person(note.shared_by.principal_id, note.shared_by.name)} on {day(note.shared_at)} from
          <a class="link" href={href({ name: "project", project: note.project_id })}>{note.project_title ?? "a project"}</a>.{#if note.reason}{` “${note.reason}”`}{/if}</p>
        <p class="small meta">It rests on {#each note.evidence as e, i (e.task_id)}{i ? (i === note.evidence.length - 1 ? " and " : ", ") : ""}<a class="link" href={href({ name: "analysis", project: note.project_id, task: e.task_id })}>{e.title ?? "an analysis"}</a> ({profileName(e.method)}){/each}, which a person had accepted when it was shared.</p>
        {#if change}<p class="small soft">{change}</p>{/if}
        {#if can.renew || can.retire}
          <div class="actions">
            {#if can.renew}<button class="link small" onclick={() => openForm(note, "renew")}>Renew</button>{/if}
            {#if can.retire}<button class="link small" onclick={() => openForm(note, "retire")}>Retire</button>{/if}
          </div>
        {/if}
        {#if editing?.note === note.note_id}
          <form class="inline" onsubmit={(e) => submit(note, e)} aria-label={editing.action === "renew" ? "Renew this note" : "Retire this note"}>
            {#if editing.action === "renew"}
              <label>Keep it in use for
                <select bind:value={keep}>{#each keepChoices as choice (choice.days)}<option value={choice.days}>{choice.label}</option>{/each}</select>
              </label>
              <p class="small soft">Counted from today. The agent reads it only while the analyses it rests on stay accepted.</p>
            {:else}
              <label>Why retire it
                <textarea bind:value={reason} rows="2" maxlength={MAX_SHARE_REASON} placeholder="For example: A later analysis split the effect by dose."></textarea>
              </label>
              <p class="small soft">A retired note stays listed here with your reason, and the agent stops reading it.</p>
            {/if}
            <div class="row">
              <button class="button" type="submit" disabled={busy}>{busy ? "Saving…" : editing.action === "renew" ? "Renew the note" : "Retire the note"}</button>
              <button class="link" type="button" onclick={() => (editing = null)}>Cancel</button>
            </div>
            {#if formFailure}<p class="notice error" role="alert">{formFailure}</p>{/if}
          </form>
        {/if}
      </li>
    {/each}
  </ol>
{/snippet}

<style>
  .notes-page { padding: 40px var(--gutter) 24px; display: grid; gap: 22px; width: 100%; max-width: 1080px; margin: 0 auto; align-content: start; }
  header { display: grid; gap: 10px; }
  h1 { font-size: clamp(30px, calc(3.4vw * var(--type-scale)), calc(42px * var(--type-scale))); letter-spacing: -0.022em; }
  header p, .empty { max-width: 76ch; }
  .group-title { font-size: calc(19px * var(--type-scale)); font-weight: 700; color: var(--heading); padding-bottom: 8px; border-bottom: 1px solid var(--line); max-width: 900px; }
  .notes { list-style: none; margin: 0; padding: 0; display: grid; gap: 22px; max-width: 900px; }
  .notes li { display: grid; gap: 6px; padding-left: 14px; border-left: 1px dashed var(--line-2); justify-items: start; min-width: 0; }
  .statement { font-family: var(--serif); font-size: calc(17px * var(--type-scale)); line-height: 1.55; max-width: 72ch; color: var(--text); }
  .state { display: flex; flex-wrap: wrap; align-items: baseline; gap: 4px 12px; }
  .meta { max-width: 80ch; color: var(--text-2); }
  .actions { display: flex; gap: 6px 22px; flex-wrap: wrap; }
  .inline { display: grid; gap: 8px; width: 100%; max-width: 640px; padding-top: 6px; }
  .inline label { display: grid; gap: 4px; font-size: var(--font-small); color: var(--text-2); }
  .inline select { justify-self: start; min-height: 36px; }
  .inline textarea { width: 100%; }
  .row { display: flex; flex-wrap: wrap; align-items: center; gap: 10px 20px; }
  .retired summary { cursor: pointer; color: var(--heading); font-weight: 600; padding: 6px 0; }
  .retired[open] summary { margin-bottom: 14px; }
</style>
