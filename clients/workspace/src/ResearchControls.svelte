<script lang="ts">
  import { BrokerError } from "./api.ts";
  import { runProfiles, type Identity } from "./contracts.ts";
  import Dialog from "./Dialog.svelte";
  import { projects, type Project, type ResearchState } from "./projects.ts";

  // Start or continue the research, and edit the question. Each request carries an
  // idempotency key so a repeated click cannot start a second run or save a second version.
  let { project, identity, defaults, startOpen = $bindable(false), editOpen = $bindable(false), onChanged }: {
    project: Project; identity: Identity; defaults: string[];
    startOpen?: boolean; editOpen?: boolean; onChanged: (notice: string) => void;
  } = $props();
  let chosen = $state<string[]>([]);
  let maxTurns = $state(6);
  let parallel = $state(2);
  let question = $state("");
  let constraints = $state("");
  let busy = $state(false);
  let failure = $state("");
  let startKey = "", startBody = "", editKey = "", editBody = "";
  let editState: ResearchState | null = null, editRevision = 0;

  const explain = (cause: unknown) => cause instanceof BrokerError && cause.code === "request_conflict"
    ? "The project changed while you were working. Close this, review the latest record, then try again."
    : cause instanceof BrokerError && cause.code === "access_required" ? "Your session or permission changed. Sign in again."
    : cause instanceof BrokerError && cause.code === "capacity_reached" ? "The shared analysis queue is full. Try again shortly."
    : "The request did not complete. Your saved work is unchanged.";

  $effect(() => {
    if (startOpen) { failure = ""; chosen = defaults.filter((key) => runProfiles(identity, project.group_key).some((p) => p.key === key)); }
  });
  $effect(() => {
    if (editOpen && project.checkpoint) {
      failure = "";
      editState = structuredClone($state.snapshot(project.checkpoint.state)) as ResearchState;
      editRevision = project.revision;
      question = editState.goal;
      constraints = editState.constraints.join("\n");
    }
  });

  async function start() {
    if (busy || !chosen.length) return;
    const body = { expected_revision: project.revision, allowed_profiles: [...chosen].sort(), max_turns: maxTurns, max_parallel: parallel };
    const serialized = JSON.stringify(body);
    if (serialized !== startBody) { startKey = crypto.randomUUID(); startBody = serialized; }
    busy = true; failure = "";
    try { await projects.start(project.project_id, { ...body, run_id: startKey }); startOpen = false; onChanged("Research started. You can close this page; the work continues on the cluster."); }
    catch (cause) { failure = explain(cause); }
    finally { busy = false; }
  }
  async function save() {
    if (!editState || busy || !question.trim()) return;
    const state = { ...editState, goal: question.trim(), constraints: constraints.split("\n").map((v) => v.trim()).filter(Boolean) };
    const serialized = JSON.stringify(state);
    if (serialized !== editBody) { editKey = crypto.randomUUID(); editBody = serialized; }
    busy = true; failure = "";
    try { await projects.checkpoint(project.project_id, { expected_revision: editRevision, request_id: editKey, state }); editOpen = false; onChanged("Saved as a new version. Earlier versions stay in the history."); }
    catch (cause) { failure = explain(cause); }
    finally { busy = false; }
  }
</script>

<Dialog bind:open={startOpen} title={project.task_count ? "Continue the research" : "Start the research"} wide>
  <p>The research agent works from your saved question and steps. It can only use the methods you tick here. A second
    check reviews each proposed step before anything runs; it is advisory, and you judge the findings.</p>
  <fieldset>
    <legend>Methods the agent may use</legend>
    {#each runProfiles(identity, project.group_key) as profile (profile.key)}
      <label class="method"><input type="checkbox" value={profile.key} bind:group={chosen} />
        <span><strong>{profile.name}</strong><small>{profile.description}</small></span></label>
    {/each}
  </fieldset>
  <div class="limits">
    <label>Decisions before it stops for you<input type="number" min="1" max="128" bind:value={maxTurns} /></label>
    <label>Analyses at the same time<input type="number" min="1" max="8" bind:value={parallel} /></label>
  </div>
  {#if failure}<p class="notice error" role="alert">{failure}</p>{/if}
  {#snippet footer()}
    <button class="link" onclick={() => (startOpen = false)}>Cancel</button>
    <button class="button" onclick={start} disabled={busy || !chosen.length}>{busy ? "Starting…" : "Start from the saved plan"}</button>
  {/snippet}
</Dialog>

<Dialog bind:open={editOpen} title="Edit the question" wide>
  <label>Research question<textarea bind:value={question} rows="4" maxlength="8192"></textarea></label>
  <label>Study constraints <small>One per line, for example "Treat each animal as the experimental unit."</small>
    <textarea bind:value={constraints} rows="4" maxlength="32768"></textarea></label>
  <p class="small soft">Saving creates a new version. The plan, methods, data references and every earlier version stay attached.</p>
  {#if failure}<p class="notice error" role="alert">{failure}</p>{/if}
  {#snippet footer()}
    <button class="link" onclick={() => (editOpen = false)}>Keep the current question</button>
    <button class="button" onclick={save} disabled={busy || !question.trim()}>{busy ? "Saving…" : "Save a new version"}</button>
  {/snippet}
</Dialog>

<style>
  fieldset { border: 0; margin: 0; padding: 0; display: grid; gap: 4px; }
  legend { font-size: 13px; font-weight: 600; color: var(--heading); margin-bottom: 8px; }
  .method { display: flex; align-items: flex-start; gap: 12px; padding: 12px 0; border-top: 1px solid var(--rule); font-weight: 400; cursor: pointer; }
  .method input { width: 18px; height: 18px; min-height: 0; margin-top: 2px; accent-color: var(--control); }
  .method strong { display: block; color: var(--heading); }
  .method small { display: block; margin-top: 4px; font-size: 13px; color: var(--ink-soft); line-height: 1.5; }
  .limits { display: grid; grid-template-columns: repeat(auto-fit, minmax(200px, 1fr)); gap: 14px; }
  .limits input { max-width: 120px; }
</style>
