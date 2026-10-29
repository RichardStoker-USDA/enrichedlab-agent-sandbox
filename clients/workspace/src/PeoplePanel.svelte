<script lang="ts">
  import Dialog from "./Dialog.svelte";
  import type { AuditRecord } from "./history.ts";
  import { accessLevels, accessSummary, auditLabel, bytes, person, when } from "./language.ts";
  import { projects, type ProjectMembers } from "./projects.ts";

  // Who can use the project, and every access decision recorded for it. Only the owner
  // can invite or remove people; the broker enforces that on every request.
  let { projectId, manage, members, audit, visibility, groupName, onChanged }: {
    projectId: string; manage: boolean; members: ProjectMembers | null; audit: AuditRecord[];
    visibility: "private" | "lab"; groupName: string; onChanged: () => void;
  } = $props();
  let inviteOpen = $state(false);
  let invitee = $state("");
  let level = $state<string>("view");
  let removing = $state<ProjectMembers["grants"][number] | null>(null);
  let busy = $state(false);
  let failure = $state("");
  const accessEvents = $derived(audit.filter((e) => ["access_changed", "admin_inspection", "admin_inspection_ended", "evidence_package_exported"].includes(e.kind)));
  const nameOf = (id: string | undefined) => {
    if (!id || !members) return "A person";
    if (members.owner.principal_id === id) return person(members.owner);
    const found = [...members.grants, ...members.eligible].find((m) => m.principal_id === id);
    return found ? person(found) : "A person";
  };

  async function invite() {
    const chosen = accessLevels.find((l) => l.key === level);
    if (!invitee || !chosen || busy) return;
    busy = true; failure = "";
    try { await projects.grant(projectId, invitee, [...chosen.capabilities]); inviteOpen = false; invitee = ""; onChanged(); }
    catch { failure = "Access could not be changed. The person may have left the research group, or your session expired."; }
    finally { busy = false; }
  }
  async function remove() {
    if (!removing || busy) return;
    busy = true; failure = "";
    try { await projects.grant(projectId, removing.principal_id, []); removing = null; onChanged(); }
    catch { failure = "Access could not be removed. Reload the project and try again."; }
    finally { busy = false; }
  }
</script>

<div class="people">
  <p>
    {visibility === "lab" ? `Everyone in ${groupName} can view this project. Changing it needs the permissions below.` : "This project is private. Only the owner and the people listed here can open it."}
    Administrator inspections, when they happen, are recorded below with their reason.
  </p>
  {#if members}
    <ul class="list">
      <li><span class="name">{person(members.owner)}</span><span class="soft small">{members.owner.email}</span><span class="access">Owner</span></li>
      {#each members.grants as grant (grant.principal_id)}
        <li>
          <span class="name">{person(grant)}{#if grant.kind === "service_account"} <span class="soft small">(service account)</span>{/if}</span>
          <span class="soft small">{grant.email}</span>
          <span class="access">{accessSummary(grant.capabilities)}{#if manage}<button class="link small" onclick={() => (removing = grant)}>Remove</button>{/if}</span>
        </li>
      {/each}
    </ul>
    {#if manage}
      <div class="actions">
        <button class="button secondary" onclick={() => { inviteOpen = true; failure = ""; }} disabled={!members.eligible.length}>Invite a colleague</button>
        {#if !members.eligible.length}<span class="soft small">Everyone else in {groupName} already has access.</span>{/if}
      </div>
    {/if}
  {:else}
    <p class="soft small">The owner manages who can use this project.</p>
  {/if}
  {#if failure}<p class="notice error" role="alert">{failure}</p>{/if}

  <section>
    <h4>Access record</h4>
    {#if accessEvents.length}
      <ol class="record">
        {#each accessEvents as event (event.event_id)}
          <li>
            <time datetime={event.occurred_at}>{when(event.occurred_at)}</time>
            <div>
              <p class="title">{auditLabel(event.kind)}</p>
              {#if event.kind === "access_changed"}
                <p class="small">{nameOf(event.detail.member_id)}: {event.detail.capabilities?.length ? accessSummary(event.detail.capabilities) : "access removed"}</p>
              {:else if event.kind === "admin_inspection"}
                <p class="small">Reason given: “{event.detail.reason}”. Read-only access until {when(event.detail.expires_at)}.</p>
              {:else if event.kind === "evidence_package_exported"}
                <p class="small">By {nameOf(event.actor_id)} · {bytes(event.detail.bytes ?? 0)}</p>
              {/if}
            </div>
          </li>
        {/each}
      </ol>
    {:else}<p class="soft small">No access changes, inspections or evidence downloads are recorded.</p>{/if}
  </section>
</div>

<Dialog bind:open={inviteOpen} title="Invite a colleague">
  <p class="small soft">Only members of {groupName} can be invited. They can open the project the next time they sign in.</p>
  <label>Colleague
    <select bind:value={invitee}>
      <option value="" disabled>Choose a person</option>
      {#each members?.eligible ?? [] as candidate (candidate.principal_id)}<option value={candidate.principal_id}>{person(candidate)} · {candidate.email}</option>{/each}
    </select>
  </label>
  <fieldset>
    <legend>What they can do</legend>
    {#each accessLevels as option (option.key)}
      <label class="choice"><input type="radio" name="access-level" value={option.key} bind:group={level} /><span>{option.label}</span></label>
    {/each}
  </fieldset>
  {#snippet footer()}
    <button class="link" onclick={() => (inviteOpen = false)}>Cancel</button>
    <button class="button" onclick={invite} disabled={!invitee || busy}>{busy ? "Saving…" : "Give access"}</button>
  {/snippet}
</Dialog>

<Dialog open={removing !== null} title="Remove access" onclose={() => (removing = null)}>
  <p>{removing ? person(removing) : ""} will no longer be able to open this project, its analyses or its files. Their earlier contributions stay in the record.</p>
  {#snippet footer()}
    <button class="link" onclick={() => (removing = null)}>Keep access</button>
    <button class="button" onclick={remove} disabled={busy}>{busy ? "Removing…" : "Remove access"}</button>
  {/snippet}
</Dialog>

<style>
  .people { display: grid; gap: 18px; max-width: 820px; }
  .list { list-style: none; margin: 0; padding: 0; border-top: 1px solid var(--rule); }
  .list li { display: grid; grid-template-columns: minmax(0, 1fr) auto; gap: 2px 16px; padding: 12px 2px; border-bottom: 1px solid var(--rule); }
  .name { font-weight: 650; color: var(--heading); }
  .list .soft { grid-column: 1; }
  .access { grid-column: 2; grid-row: 1 / span 2; align-self: center; display: flex; gap: 14px; align-items: baseline; font-size: 14px; }
  .actions { display: flex; align-items: center; gap: 14px; flex-wrap: wrap; }
  section { display: grid; gap: 8px; }
  h4 { font-size: 15px; }
  .record { list-style: none; margin: 0; padding: 0; }
  .record li { display: grid; grid-template-columns: 130px minmax(0, 1fr); gap: 16px; padding: 10px 0; border-top: 1px solid var(--rule); }
  .record time { font-size: 13px; color: var(--ink-soft); }
  .record .title { font-weight: 600; color: var(--heading); }
  fieldset { border: 0; padding: 0; margin: 0; display: grid; gap: 8px; }
  legend { font-size: 13px; font-weight: 600; color: var(--heading); margin-bottom: 6px; }
  .choice { display: flex; align-items: center; gap: 10px; font-weight: 400; color: var(--ink); }
  .choice input { width: 18px; height: 18px; min-height: 0; accent-color: var(--control); }
  @media (max-width: 640px) { .record li { grid-template-columns: 1fr; gap: 2px; } }
</style>
