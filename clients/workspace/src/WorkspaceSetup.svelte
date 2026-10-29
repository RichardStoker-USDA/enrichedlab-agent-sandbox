<script lang="ts">
  import { untrack } from "svelte";
  import { projects, type Project, type WorkspaceChoice } from "./projects.ts";
  import StartingPoints from "./StartingPoints.svelte";

  // A project saved without its question (one made before Ask first, or a setup that stopped after the project was
  // made): its owner writes the question and chooses the data and methods here, as the ask sheet does, with one,
  // several or every set where the broker combines them. Nothing runs until they start it.
  let { project, labName = "this lab", update }: { project: Project; labName?: string; update: (p: Project) => void } = $props();
  let choices = $state<WorkspaceChoice[]>([]), combine = $state(false), chosen = $state<string[]>([]);
  let goal = $state(""), constraints = $state(""), error = $state("");
  let loading = $state(true), saving = $state(false);
  let controller: AbortController | null = null, requestId = "", requestBody = "";
  const picked = $derived(choices.filter((c) => chosen.includes(c.key)));
  $effect(() => {
    const id = project.project_id;
    return untrack(() => {
      const c = new AbortController(); controller = c;
      choices = []; combine = false; chosen = []; goal = ""; constraints = ""; error = ""; loading = true;
      requestId = ""; requestBody = "";
      void projects.workspaces(id, c.signal).then((result) => {
        if (c.signal.aborted) return;
        choices = result.workspaces; combine = result.combine === true;
        if (choices.length === 1) chosen = [choices[0].key];
      }).catch(() => { if (!c.signal.aborted) error = "The available data and methods could not be loaded. Reopen this project to try again."; })
        .finally(() => { if (!c.signal.aborted) loading = false; });
      return () => c.abort();
    });
  });
  async function save() {
    if (saving || !picked.length || !controller || !goal.trim()) return;
    const c = controller, id = project.project_id;
    const sets = picked.length > 1 && combine ? { workspace_keys: picked.map((x) => x.key) } : { workspace_key: picked[0].key };
    const body = { ...sets, goal: goal.trim(), constraints: constraints.split("\n").map((s) => s.trim()).filter(Boolean), expected_revision: 0 };
    if (JSON.stringify(body) !== requestBody) { requestBody = JSON.stringify(body); requestId = crypto.randomUUID(); }
    saving = true; error = "";
    try {
      await projects.prepare(id, { ...body, request_id: requestId }, c.signal);
      const saved = await projects.get(id, c.signal);
      if (!c.signal.aborted) update(saved);
    } catch { if (!c.signal.aborted) error = "This question could not be saved. Your project may have changed or your access may have expired. Reopen the project before changing the request."; }
    finally { if (!c.signal.aborted) saving = false; }
  }
</script>

<section aria-labelledby="starting-question" class="starting-question">
  <h2 id="starting-question">What would you like to find out?</h2>
  <p class="lede">Write the question and choose what the agent works from. Nothing runs until you start it, and the lab's
    data stays as it is.</p>
  {#if loading}<p class="soft" role="status">Reading the data and methods this project can use…</p>
  {:else if choices.length}
    <form onsubmit={(e) => { e.preventDefault(); void save(); }}>
      <label class="question">Research question
        <textarea bind:value={goal} rows="3" maxlength="8192" required placeholder="The question, comparison or pattern you want to investigate."></textarea></label>
      <StartingPoints {choices} {combine} bind:chosen disabled={saving} {labName} name="setup-start" />
      <label><span id="setup-constraints-name">Study constraints</span> <small id="setup-constraints-hint">Optional, one per line, for example "Treat each animal as the experimental unit."</small>
        <textarea bind:value={constraints} rows="3" maxlength="32768" aria-labelledby="setup-constraints-name" aria-describedby="setup-constraints-hint"></textarea></label>
      <div class="save-row">
        <button class="button" type="submit" disabled={saving || !picked.length || !goal.trim()}>{saving ? "Saving the question…" : "Save the question"}</button>
      </div>
    </form>
  {:else if !error}<p>No data and method set is available to your account in this lab yet. A lab lead can register one.</p>{/if}
  {#if error}<p class="notice error" role="alert">{error}</p>{/if}
</section>

<style>
  .starting-question { display: grid; gap: 14px; max-width: var(--measure); margin: 8px 0 28px; }
  h2 { font-size: calc(22px * var(--type-scale)); letter-spacing: -0.01em; }
  .lede { color: var(--text-2); max-width: 64ch; }
  form { display: grid; gap: 18px; margin-top: 4px; }
  label { display: grid; gap: 6px; font-size: var(--font-small); font-weight: 650; color: var(--heading); }
  label small { font-weight: 400; color: var(--text-2); font-size: var(--font-xs); }
  .question textarea { font-family: var(--serif); font-size: calc(17px * var(--type-scale)); line-height: 1.5; min-height: 88px; }
  .save-row { display: flex; gap: 12px; align-items: center; flex-wrap: wrap; }
</style>
