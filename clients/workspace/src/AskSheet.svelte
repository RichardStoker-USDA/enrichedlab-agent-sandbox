<script lang="ts">
  import { untrack } from "svelte";
  import { BrokerError } from "./api.ts";
  import { titleFrom } from "./ask.ts";
  import { labOrder, operatorLab } from "./context.ts";
  import { runProfiles, type Identity } from "./contracts.ts";
  import Dialog from "./Dialog.svelte";
  import { combines, methodsOf, readStarts } from "./lab_starts.ts";
  import { projects, type WorkspaceChoice } from "./projects.ts";
  import StartingPoints from "./StartingPoints.svelte";

  // Ask first (Glass Box rule 1): the question leads, then the setup a project needs. Start creates the project, saves
  // the question with its data and methods, and starts the agent; Save without starting stops after the question. Each
  // request carries its own idempotency key, so a retry after a lost answer never makes a second project or run, and a
  // retry after a failed step continues from that step.
  let { open = $bindable(false), question = $bindable(""), identity, labKey, onDone }: {
    open?: boolean; question?: string; identity: Identity; labKey: string;
    onDone: (project: string, started: boolean) => void;
  } = $props();

  let title = $state(""), titleEdited = $state(false);
  let group = $state(""), visibility = $state<"private" | "lab">("private");
  let chosen = $state<string[]>([]), methods = $state<string[]>([]);
  let maxTurns = $state(6), parallel = $state(2), constraints = $state("");
  let busy = $state(false), failure = $state("");
  // The lab's starting points (lab_starts.ts).
  let starts = $state<{ lab: string; items: WorkspaceChoice[] | null; failed: boolean; combine: boolean }>({ lab: "", items: null, failed: false, combine: false });
  // Progress through the three requests, kept while the sheet stays open.
  let created = $state(""), createKey = "", createBody = "";
  let prepared = $state(0), allowedAfter: string[] = [], prepareKey = "", prepareBody = "";
  let runKey = "", runBody = "";
  let stage = $state<"create" | "prepare" | "start">("create");

  const labs = $derived(labOrder(identity.groups).filter((g) => !operatorLab(g.key) || g.key === labKey));
  const labName = $derived(identity.groups.find((g) => g.key === group)?.name ?? "the lab");
  const choices = $derived(starts.lab === group ? starts.items : null);
  const picked = $derived(choices?.filter((c) => chosen.includes(c.key)) ?? []);
  const offered = $derived(runProfiles(identity, group).filter((p) => !picked.length || methodsOf(picked).includes(p.key)));
  const lines = (text: string) => text.split("\n").map((s) => s.trim()).filter(Boolean);
  const ready = $derived(!!question.trim() && !!title.trim() && !!group && picked.length > 0);

  // A fresh sheet each time it opens: the lab in view, the title drawn from the question, nothing sent yet.
  let wasOpen = false;
  $effect(() => {
    const now = open;
    untrack(() => {
      if (now && !wasOpen) {
        group = labs.some((g) => g.key === labKey) ? labKey : labs[0]?.key ?? "";
        visibility = "private"; titleEdited = false; title = titleFrom(question);
        maxTurns = 6; parallel = 2; constraints = ""; failure = ""; stage = "create";
        created = ""; createKey = ""; createBody = ""; prepared = 0; allowedAfter = []; prepareKey = ""; prepareBody = ""; runKey = ""; runBody = "";
      }
      wasOpen = now;
    });
  });
  $effect(() => { const q = question; if (!titleEdited) untrack(() => { title = titleFrom(q); }); });
  $effect(() => {
    const key = open ? group : "";
    if (!key) return;
    let current = true;
    starts = { lab: key, items: null, failed: false, combine: false };
    readStarts(key).then((items) => { if (current) starts = { lab: key, items, failed: false, combine: combines(key) }; })
      .catch(() => { if (current) starts = { lab: key, items: [], failed: true, combine: false }; });
    return () => { current = false; };
  });
  // The one starting point is chosen for the reader; with several, nothing is chosen until they pick.
  $effect(() => {
    const list = choices;
    untrack(() => {
      if (!list) return;
      const kept = chosen.filter((k) => list.some((c) => c.key === k));
      chosen = kept.length ? kept : list.length === 1 ? [list[0].key] : [];
    });
  });
  // Every method the chosen sets allow is ticked until the reader changes it.
  $effect(() => { const keys = offered.map((p) => p.key); untrack(() => { methods = keys; }); });

  function explain(cause: unknown): string {
    if (cause instanceof BrokerError && cause.code === "access_required") return "Your session or permission changed. Sign in again, then try once more.";
    if (cause instanceof BrokerError && (cause.code === "request_conflict" || cause.code === "revision_conflict"))
      return "The project changed while this was being set up. Open it from Your research to see the latest record.";
    if (stage === "start" && cause instanceof BrokerError && cause.code === "capacity_reached")
      return "The project and its question are saved, but the shared analysis queue is full. Try Start again shortly.";
    return stage === "create" ? "The project was not created. Your question is still here; try again."
      : stage === "prepare" ? "The project was created, but its question was not saved. Try again; nothing is made twice."
      : "The project and its question are saved, but the agent did not start. Try Start again, or start it from the project's page.";
  }

  async function finish(start: boolean) {
    if (busy || !ready || !picked.length || (start && !methods.length)) return;
    busy = true; failure = "";
    try {
      if (!created) {
        stage = "create";
        const body = JSON.stringify({ group, title: title.trim(), visibility });
        if (body !== createBody) { createBody = body; createKey = crypto.randomUUID(); }
        await projects.create({ ...JSON.parse(body), project_id: createKey });
        created = createKey;
      }
      if (!prepared) {
        stage = "prepare";
        // Several sets go as one request where the broker combines them; one set keeps the older field.
        const sets = picked.length > 1 && starts.combine ? { workspace_keys: picked.map((c) => c.key) } : { workspace_key: picked[0].key };
        const body = JSON.stringify({ ...sets, goal: question.trim(), constraints: lines(constraints), expected_revision: 0 });
        if (body !== prepareBody) { prepareBody = body; prepareKey = crypto.randomUUID(); }
        const result = await projects.prepare(created, { ...JSON.parse(body), request_id: prepareKey });
        prepared = result.revision; allowedAfter = result.allowed_profiles;
      }
      if (start) {
        stage = "start";
        const allowed = methods.filter((k) => allowedAfter.includes(k)).sort();
        const body = JSON.stringify({ expected_revision: prepared, allowed_profiles: allowed, max_turns: maxTurns, max_parallel: parallel });
        if (body !== runBody) { runBody = body; runKey = crypto.randomUUID(); }
        await projects.start(created, { ...JSON.parse(body), run_id: runKey });
      }
      const project = created;
      open = false; question = "";
      onDone(project, start);
    } catch (cause) { failure = explain(cause); }
    finally { busy = false; }
  }
  const locked = $derived(!!prepared);
</script>

<Dialog bind:open title="Set up the question" wide sheet>
  <label class="question">Your question
    <textarea bind:value={question} rows="3" maxlength="8192" disabled={locked}></textarea></label>
  <!-- svelte-ignore a11y_autofocus -->
  <!-- Each hint describes its field; the field's name stays one word, so "Your question" names one field. -->
  <label><span id="ask-title-name">Title</span> <small id="ask-title-hint">Drawn from your question. Change it if you like.</small>
    <input bind:value={title} oninput={() => (titleEdited = true)} maxlength="160" disabled={!!created} autofocus
      aria-labelledby="ask-title-name" aria-describedby="ask-title-hint" /></label>
  {#if labs.length > 1}
    <label>Lab<select bind:value={group} disabled={!!created}>{#each labs as g (g.key)}<option value={g.key}>{g.name}</option>{/each}</select></label>
  {/if}

  {#if choices && !starts.failed && choices.length}
    <StartingPoints {choices} combine={starts.combine} bind:chosen disabled={locked} {labName} />
  {:else}
    <fieldset class="starts">
      <legend>Data and methods</legend>
      {#if !choices}<p class="soft small" role="status">Reading what {labName} can work from…</p>
      {:else if starts.failed}<p class="notice error" role="alert">What {labName} can work from could not be read. Close this and try again.</p>
      {:else}<p class="small">No data and method set is registered for {labName} yet, so the agent can't start research here. Papers can already go in its library.</p>{/if}
    </fieldset>
  {/if}

  <fieldset>
    <legend>Who can open it</legend>
    <div class="segmented who" role="radiogroup" aria-label="Who can open it">
      <label><input type="radio" value="private" bind:group={visibility} disabled={!!created} />Only me and people I invite</label>
      <label><input type="radio" value="lab" bind:group={visibility} disabled={!!created} />Everyone in {labName}</label>
    </div>
  </fieldset>

  <details class="more">
    <summary>More options</summary>
    <div class="more-body">
      {#if offered.length}
        <fieldset>
          <legend>Methods the agent may use</legend>
          {#each offered as p (p.key)}
            <label class="method"><input type="checkbox" value={p.key} bind:group={methods} /><span><strong>{p.name}</strong><small>{p.description}</small></span></label>
          {/each}
        </fieldset>
      {/if}
      <div class="limits">
        <label>Decisions before it stops for you<input type="number" min="1" max="128" bind:value={maxTurns} /></label>
        <label>Analyses at the same time<input type="number" min="1" max="8" bind:value={parallel} /></label>
      </div>
      <label><span id="ask-constraints-name">Study constraints</span> <small id="ask-constraints-hint">Optional, one per line, for example "Treat each animal as the experimental unit."</small>
        <textarea bind:value={constraints} rows="3" maxlength="32768" disabled={locked} aria-labelledby="ask-constraints-name" aria-describedby="ask-constraints-hint"></textarea></label>
    </div>
  </details>

  {#if failure}<p class="notice error" role="alert">{failure}</p>{/if}
  {#snippet footer()}
    <p class="hint">Only Start runs anything. You can leave once it starts.</p>
    <button class="link" onclick={() => void finish(false)} disabled={busy || !ready}>Save without starting</button>
    <button class="button" onclick={() => void finish(true)} disabled={busy || !ready || !methods.length}>{busy ? (stage === "start" ? "Starting…" : "Saving…") : "Start"}</button>
  {/snippet}
</Dialog>

<style>
  .question textarea { font-family: var(--serif); font-size: calc(17px * var(--type-scale)); line-height: 1.5; min-height: 88px; }
  fieldset { border: 0; margin: 0; padding: 0; display: grid; gap: 8px; min-width: 0; }
  legend { font-size: var(--font-small); font-weight: 650; color: var(--heading); margin-bottom: 6px; padding: 0; }
  .segmented { justify-self: start; }
  .more > summary { cursor: pointer; min-height: 36px; display: flex; align-items: center; font-size: var(--font-small); font-weight: 650; color: var(--heading); }
  .more-body { display: grid; gap: 16px; padding-top: 8px; }
  .method { display: flex; align-items: flex-start; gap: 10px; font-weight: 400; color: var(--text); }
  .method input { margin-top: 3px; flex: none; }
  .method span { display: grid; gap: 2px; }
  .method small { color: var(--text-2); font-size: var(--font-xs); }
  .limits { display: grid; grid-template-columns: repeat(2, minmax(0, 1fr)); gap: 12px; }
  .hint { margin-right: auto; font-size: var(--font-xs); color: var(--text-3); max-width: 34ch; }
  @media (max-width: 640px) {
    .limits { grid-template-columns: minmax(0, 1fr); }
    /* Two long choices do not fit side by side on a phone, so they stack on the same track. */
    .who { display: grid; width: 100%; border-radius: var(--r-3); }
    .who > label { min-height: 40px; border-radius: var(--r-2); white-space: normal; }
    .who > label:has(input:checked)::after { left: 0; right: auto; top: 10px; bottom: 10px; width: 3px; height: auto; }
    .hint { flex-basis: 100%; max-width: none; }
  }
</style>
