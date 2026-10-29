<script lang="ts">
  import DecisionInput from "./DecisionInput.svelte";
  import type { AgentNote, HistoryEntry, Source } from "./history.ts";
  import { when } from "./language.ts";

  // One timeline for the whole project. Each entry says who produced it, and agent
  // entries say plainly that they are the agent's reading, not verified facts.
  // The project's notebook reads oldest first, like a lab notebook; other records read newest first.
  let { entries, notes, openTask, oldestFirst = false }: { entries: HistoryEntry[]; notes: AgentNote[]; openTask: (id: string) => void; oldestFirst?: boolean } = $props();
  let filter = $state<"all" | Source>("all");
  let limit = $state(40);
  const sourceNames: Record<Source, string> = { you: "You", colleague: "A colleague", agent: "Research agent", system: "System record", administrator: "Administrator" };
  const shown = $derived((oldestFirst ? [...entries].reverse() : entries).filter((e) => filter === "all" || e.source === filter));
  const counts = $derived(entries.reduce((m, e) => m.set(e.source, (m.get(e.source) ?? 0) + 1), new Map<Source, number>()));
  const filters = $derived((["all", "system", "agent", "you", "colleague", "administrator"] as const).filter((f) => f === "all" || counts.get(f)));
</script>

<div class="history">
  <div class="filters" role="group" aria-label="Show entries from">
    {#each filters as f}
      <button aria-pressed={filter === f} onclick={() => { filter = f; limit = 40; }}>
        {f === "all" ? "Everything" : sourceNames[f]}<span class="count">{f === "all" ? entries.length : counts.get(f)}</span>
      </button>
    {/each}
  </div>
  <ol class="timeline">
    {#each shown.slice(0, limit) as entry (entry.key)}
      <li class={`entry ${entry.source} ${entry.tone}`}>
        <time datetime={entry.at}>{when(entry.at)}</time>
        <div class="body">
          <p class="who">{sourceNames[entry.source]}{#if entry.unverified}<span class="unverified">its reading, not verified</span>{/if}</p>
          <p class="title">{entry.title}</p>
          {#if entry.detail}<p class="detail" class:quote={entry.source === "agent"}>{entry.detail}</p>{/if}
          {#if entry.secondary}<p class="secondary">{entry.secondary}</p>{/if}
          {#if entry.taskId}<button class="link small" onclick={() => openTask(entry.taskId!)}>Open the analysis</button>{/if}
          {#if entry.input}<DecisionInput input={entry.input} />{/if}
        </div>
      </li>
    {/each}
  </ol>
  {#if shown.length > limit}<button class="button secondary" onclick={() => (limit += 60)}>Show {Math.min(60, shown.length - limit)} {oldestFirst ? "later" : "earlier"} entries</button>{/if}
  {#if !shown.length}<p class="soft">Nothing recorded yet.</p>{/if}

  {#if notes.length}
    <section class="notes">
      <h4>Notes the agent saved</h4>
      <p class="soft small">These are the research agent's own interpretations, kept with the plan so later decisions can see them. They are not verified. Where one conflicts with a system record above, the system record is correct.</p>
      <ul>
        {#each notes as note (note.statement)}
          <li class:withdrawn={!note.current}>
            <p class="label small">{note.label} · first saved in version {note.firstRevision}{#if !note.current} · no longer in the current version{/if}</p>
            <p>{note.statement}</p>
          </li>
        {/each}
      </ul>
    </section>
  {/if}
</div>

<style>
  .history { display: grid; gap: 16px; max-width: 920px; }
  /* Filters are words: the one in force is underlined with the editor's mark. */
  .filters { display: flex; flex-wrap: wrap; gap: 4px 22px; padding-bottom: 6px; border-bottom: 1px solid var(--line); }
  .filters button { min-height: 34px; padding: 0; font-size: var(--font-small); font-weight: 600; color: var(--text-2); border-bottom: 2px solid transparent; }
  .filters button:hover { color: var(--heading); }
  .filters .count { margin-left: 6px; font-weight: 400; font-variant-numeric: tabular-nums; color: var(--text-3); }
  .filters button[aria-pressed="true"] { color: var(--heading); border-bottom-color: var(--mark); }
  .timeline { list-style: none; margin: 0; padding: 0; }
  .entry { display: grid; grid-template-columns: 128px minmax(0, 1fr); gap: 18px; padding: 14px 0; border-bottom: 1px solid var(--line); position: relative; }
  time { font-size: var(--font-xs); color: var(--text-3); font-variant-numeric: tabular-nums; padding-top: 3px; }
  .body { position: relative; display: grid; gap: 3px; justify-items: start; padding-left: 16px; border-left: 1px solid var(--line-2); }
  .entry.agent .body { border-left-style: dashed; }
  .entry.attention .body::before { content: ""; position: absolute; left: -2px; top: 0; bottom: 0; width: 3px; background: var(--mark); }
  .entry.failed .title { color: var(--failed); }
  .who { font-size: var(--font-xs); font-weight: 650; color: var(--text-2); display: flex; flex-wrap: wrap; gap: 8px; }
  .unverified { font-weight: 500; font-style: italic; color: var(--text-3); }
  .title { font-weight: 650; color: var(--heading); }
  .detail { max-width: 72ch; line-height: 1.6; }
  .detail.quote { font-family: var(--serif); font-size: calc(16px * var(--type-scale)); color: var(--text); }
  .secondary { font-size: var(--font-small); color: var(--text-2); max-width: 72ch; }
  .notes { display: grid; gap: 8px; padding-top: 14px; }
  .notes h4 { font-size: calc(15px * var(--type-scale)); }
  .notes ul { list-style: none; margin: 0; padding: 0; display: grid; gap: 12px; }
  .notes li { padding-left: 14px; border-left: 1px dashed var(--line-2); }
  .notes li p:last-child { font-family: var(--serif); font-size: calc(16px * var(--type-scale)); line-height: 1.55; }
  .notes .label { color: var(--text-3); }
  .notes li.withdrawn p:last-child { color: var(--text-3); }
  @media (max-width: 640px) {
    .entry { grid-template-columns: 1fr; gap: 4px; }
    .body { padding-left: 12px; }
  }
</style>
