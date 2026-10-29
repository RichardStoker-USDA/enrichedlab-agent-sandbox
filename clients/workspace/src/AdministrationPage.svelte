<script lang="ts">
  import { onDestroy } from "svelte";
  import { BrokerError } from "./api.ts";
  import Dialog from "./Dialog.svelte";
  import { person, when } from "./language.ts";
  import { projects, type DirectoryEntry } from "./projects.ts";

  // Administrator inspection across every project. Listing the directory is itself
  // recorded. Opening a project the administrator cannot otherwise read needs a written
  // reason and gives read-only access for 30 minutes, visible to the owner.
  let { openProject, onInspected }: { openProject: (id: string) => void; onInspected: (id: string, expires: string) => void } = $props();
  let entries = $state<DirectoryEntry[]>([]);
  let loading = $state(true);
  let failure = $state("");
  let filter = $state("");
  let target = $state<DirectoryEntry | null>(null);
  let reason = $state("");
  let busy = $state(false);
  let inspectFailure = $state("");
  const controller = new AbortController();
  const visible = $derived(entries.filter((e) => `${e.title} ${e.group_name} ${person(e.owner)}`.toLowerCase().includes(filter.trim().toLowerCase())));

  projects.directory(controller.signal).then((r) => { entries = r.projects; })
    .catch((cause) => { if (!controller.signal.aborted) failure = cause instanceof BrokerError && cause.code === "access_required" ? "This account is not enrolled for project inspection." : "The project directory could not be loaded."; })
    .finally(() => { loading = false; });
  onDestroy(() => controller.abort());

  async function inspect() {
    if (!target || busy || reason.trim().length < 8) return;
    busy = true; inspectFailure = "";
    try {
      const result = await projects.inspect(target.project_id, reason.trim());
      onInspected(target.project_id, result.expires_at);
      const id = target.project_id;
      target = null; reason = "";
      openProject(id);
    } catch { inspectFailure = "The inspection was not started. Check that the reason has at least 8 characters and try again."; }
    finally { busy = false; }
  }
</script>

<section class="admin">
  <header>
    <h1>Project inspection</h1>
    <p>You can open any project here to review its quality or respond to an incident. Opening a project you do not otherwise
      have access to needs a reason, gives read-only access for 30 minutes, and is recorded where the project's owner can see it.
      This list view is also recorded.</p>
  </header>
  {#if entries.length > 8}<input type="search" placeholder="Find by title, group or owner" aria-label="Find a project" bind:value={filter} />{/if}
  {#if loading}<p class="soft" role="status">Opening the directory…</p>
  {:else if failure}<p class="notice error" role="alert">{failure}</p>
  {:else}
    <!-- svelte-ignore a11y_no_noninteractive_tabindex -->
    <div class="table" role="region" aria-label="Projects" tabindex="0">
      <table>
        <thead><tr><th scope="col">Project</th><th scope="col">Owner</th><th scope="col">Group</th><th scope="col">Analyses</th><th scope="col">Updated</th><th scope="col"><span class="visually-hidden">Action</span></th></tr></thead>
        <tbody>
          {#each visible as entry (entry.project_id)}
            <tr>
              <td><strong>{entry.title}</strong><span class="soft small">{entry.visibility === "lab" ? "Shared with the group" : "Private"}</span></td>
              <td>{person(entry.owner)}{#if entry.owner.kind === "service_account"}<span class="soft small">service account</span>{/if}</td>
              <td>{entry.group_name}</td>
              <td class="num">{entry.task_count}</td>
              <td>{when(entry.updated_at)}</td>
              <td class="action">
                {#if entry.inspection_expires_at}<button class="link" onclick={() => { onInspected(entry.project_id, entry.inspection_expires_at!); openProject(entry.project_id); }}>Open · inspection until {when(entry.inspection_expires_at)}</button>
                {:else if entry.readable}<button class="link" onclick={() => openProject(entry.project_id)}>Open</button>
                {:else}<button class="button secondary" onclick={() => { target = entry; reason = ""; inspectFailure = ""; }}>Inspect</button>{/if}
              </td>
            </tr>
          {/each}
        </tbody>
      </table>
    </div>
  {/if}
</section>

<Dialog open={target !== null} title="Start an inspection" onclose={() => (target = null)}>
  <p><strong>{target?.title}</strong>, owned by {target ? person(target.owner) : ""}.</p>
  <label>Reason <small>At least 8 characters. The owner will see it.</small>
    <textarea bind:value={reason} maxlength="500" rows="3" placeholder="For example: quality review of retained evidence before the lab meeting"></textarea></label>
  <p class="small soft">Read-only access for 30 minutes. You can end it sooner from the project page.</p>
  {#if inspectFailure}<p class="notice error" role="alert">{inspectFailure}</p>{/if}
  {#snippet footer()}
    <button class="link" onclick={() => (target = null)}>Cancel</button>
    <button class="button" onclick={inspect} disabled={busy || reason.trim().length < 8}>{busy ? "Starting…" : "Start the inspection"}</button>
  {/snippet}
</Dialog>

<style>
  .admin { padding: 40px var(--gutter) 24px; display: grid; gap: 22px; width: 100%; max-width: 1080px; margin: 0 auto; }
  header { display: grid; gap: 10px; }
  h1 { font-size: clamp(30px, calc(3.4vw * var(--type-scale)), calc(42px * var(--type-scale))); letter-spacing: -0.022em; }
  header p { max-width: 76ch; }
  input { max-width: 440px; }
  /* A publication-style table: heavy rules above and below, light rules between rows. */
  .table { overflow: auto; }
  table { width: 100%; border-collapse: collapse; font-size: var(--font-small); }
  th, td { text-align: left; padding: 11px 16px 11px 0; border-bottom: 1px solid var(--line); vertical-align: top; }
  thead th { font-weight: 650; color: var(--heading); border-top: 2px solid var(--heading); border-bottom: 1px solid var(--heading); }
  tbody tr:last-child td { border-bottom: 2px solid var(--heading); }
  td strong { display: block; color: var(--heading); font-size: calc(15px * var(--type-scale)); }
  td .soft { display: block; }
  .num { text-align: right; font-variant-numeric: tabular-nums; }
  .action { text-align: right; white-space: nowrap; padding-right: 0; }
</style>
