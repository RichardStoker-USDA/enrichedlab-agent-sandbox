<script lang="ts">
  import Self from "./WbTree.svelte";
  import { openable, type WbNode } from "./workbench.ts";

  // One level of the explorer. Each row is a tree item that takes focus itself, so a screen reader hears the item,
  // its level and whether it is open. Plain folders open and close; records open in the editor. Keyboard movement
  // between rows is handled once by the workbench for the whole explorer. A row shows its icon, its name, which wraps
  // rather than being cut short, and at most a New mark, a count, the formats of a figure or a status mark; the longer
  // detail is in its label and tooltip. A row that arrived while the page was open is marked for a moment.
  let { nodes, expanded, active, focusable = "", highlighted = "", loading, arrived = new Set(), depth = 0, label = "", onToggle, onOpen, onPin }: {
    nodes: WbNode[]; expanded: Set<string>; active: string; focusable?: string; highlighted?: string; loading: Set<string>; arrived?: Set<string>;
    depth?: number; label?: string; onToggle: (node: WbNode) => void; onOpen: (node: WbNode) => void; onPin: (node: WbNode) => void;
  } = $props();

  const expandable = (node: WbNode) => node.kind === "project" || !!node.children?.length;
  // Screen readers activate a tree item by clicking the item element itself, so the item owns the click. A click
  // that bubbles up from a nested item belongs to that item, not to this one.
  const own = (event: MouseEvent) => (event.target as Element | null)?.closest('[role="treeitem"]') === event.currentTarget;
  const count = (node: WbNode) => (node.detail && /^\d+$/.test(node.detail) ? node.detail : "");
  // One monoline icon per kind of record, on a 20 pixel grid; no per-file-type icons.
  const ICONS: Record<string, string> = {
    question: "M10 17.2a7.2 7.2 0 1 0 0-14.4 7.2 7.2 0 0 0 0 14.4ZM7.9 7.7a2.2 2.2 0 1 1 3 2.1c-.6.2-.9.7-.9 1.3v.6M10 14.1v.1",
    runs: "M2.8 10h3l2-4.6 3.4 9.2 2-4.6h4",
    run: "M2.8 10h3l2-4.6 3.4 9.2 2-4.6h4",
    turn: "M10 2.9 17.1 10 10 17.1 2.9 10Z",
    analyses: "M4 16.5V9.5M8 16.5V4.5M12 16.5v-5M16 16.5v-8M2.8 16.5h14.4",
    analysis: "M4 16.5V9.5M8 16.5V4.5M12 16.5v-5M16 16.5v-8M2.8 16.5h14.4",
    folder: "M2.8 5.6c0-.8.6-1.4 1.4-1.4h3.4l1.6 1.8h6.6c.8 0 1.4.6 1.4 1.4v7.4c0 .8-.6 1.4-1.4 1.4H4.2c-.8 0-1.4-.6-1.4-1.4Z",
    file: "M5.4 2.8h6.2l3.4 3.4v10.2c0 .4-.4.8-.8.8H5.4c-.4 0-.8-.4-.8-.8V3.6c0-.4.4-.8.8-.8ZM11.4 2.8v3.6H15",
    steps: "M7.6 5.2h9M7.6 10h9M7.6 14.8h9M3.2 5.2l1 1 1.6-2M3.2 10l1 1 1.6-2M3.2 14.8l1 1 1.6-2",
    code: "M7.2 6 3.2 10l4 4M12.8 6l4 4-4 4",
    record: "M10 17.2a7.2 7.2 0 1 0-7.2-7.2M2.8 10 1.4 8.6M2.8 10l1.6-1.4M10 6.4V10l2.6 1.6",
    people: "M7.4 9.2a2.6 2.6 0 1 0 0-5.2 2.6 2.6 0 0 0 0 5.2ZM2.8 16.2c.4-2.6 2.3-4.2 4.6-4.2s4.2 1.6 4.6 4.2M13.4 4.2a2.4 2.4 0 0 1 0 4.8M14.8 12.2c1.4.6 2.2 1.9 2.4 4",
    package: "M3.2 6.4 10 3l6.8 3.4v7.2L10 17l-6.8-3.4ZM3.2 6.4 10 9.8l6.8-3.4M10 9.8V17",
    project: "M2.8 5.6c0-.8.6-1.4 1.4-1.4h3.4l1.6 1.8h6.6c.8 0 1.4.6 1.4 1.4v7.4c0 .8-.6 1.4-1.4 1.4H4.2c-.8 0-1.4-.6-1.4-1.4Z",
    lab: "M3 16.6h14M4.6 16.6V8.2L10 4l5.4 4.2v8.4M8.2 16.6v-4.2h3.6v4.2",
    planned: "M10 17.2a7.2 7.2 0 1 0 0-14.4 7.2 7.2 0 0 0 0 14.4Z",
    data: "M10 3c3.6 0 6.4 1 6.4 2.4S13.6 7.8 10 7.8 3.6 6.8 3.6 5.4 6.4 3 10 3ZM3.6 5.4v9.2C3.6 16 6.4 17 10 17s6.4-1 6.4-2.4V5.4M3.6 10c0 1.4 2.8 2.4 6.4 2.4s6.4-1 6.4-2.4",
    files: "M5.4 2.8h6.2l3.4 3.4v10.2c0 .4-.4.8-.8.8H5.4c-.4 0-.8-.4-.8-.8V3.6c0-.4.4-.8.8-.8ZM7.4 10h5.2M7.4 13h5.2",
  };
</script>

<ul role={depth === 0 ? "tree" : "group"} aria-label={depth === 0 ? label || "Records" : undefined}>
  {#each nodes as node (node.id)}
    {@const open = expanded.has(node.id)}
    {@const canExpand = expandable(node)}
    {@const n = count(node)}
    <!-- svelte-ignore a11y_click_events_have_key_events -->
    <li role="treeitem" data-node={node.id} tabindex={(focusable || active) === node.id ? 0 : -1}
      aria-label={[node.label, node.detail, node.formats, node.fresh ? "new since your last look" : ""].filter(Boolean).join(", ")}
      aria-expanded={canExpand ? open : undefined} aria-selected={active === node.id} aria-level={depth + 1} aria-busy={loading.has(node.id) || undefined}
      onclick={(e) => { if (own(e)) (openable(node) ? onOpen(node) : onToggle(node)); }}
      ondblclick={(e) => { if (own(e) && openable(node)) onPin(node); }}>
      <div class="row" class:active={active === node.id} class:lit={highlighted === node.id} class:arrived={arrived.has(node.id)} style={`--depth:${depth}`} title={node.detail && !n ? `${node.label}: ${node.detail}` : undefined}>
        {#if canExpand}
          <!-- svelte-ignore a11y_click_events_have_key_events, a11y_no_static_element_interactions -->
          <span class="twisty" aria-hidden="true" onclick={(e) => { e.stopPropagation(); onToggle(node); }}>
            <svg viewBox="0 0 10 10" class:open><path d="M3.5 2l3 3-3 3" /></svg>
          </span>
        {:else}<span class="twisty" aria-hidden="true"></span>{/if}
        <svg class="icon" viewBox="0 0 20 20" aria-hidden="true"><path d={ICONS[node.kind] ?? ICONS.file} /></svg>
        <span class="t">{node.label}</span>
        {#if loading.has(node.id)}<span class="end waveform" aria-hidden="true"><i></i><i></i><i></i></span>
        {:else if node.tone === "failed"}<span class="end mark failed" aria-hidden="true">×</span>
        {:else if node.tone === "active"}<span class="end waveform" aria-hidden="true"><i></i><i></i><i></i></span>
        {:else if node.tone === "attention"}<span class="end mark attention" aria-hidden="true"></span>
        {:else if node.fresh}<span class="end new" aria-hidden="true">New</span>
        {:else if node.formats}<span class="end formats" aria-hidden="true">{node.formats}</span>
        {:else if n}<span class="end n" aria-hidden="true">{n}</span>{/if}
      </div>
      {#if canExpand && open && node.children?.length}
        <Self nodes={node.children} {expanded} {active} {focusable} {highlighted} {loading} {arrived} depth={depth + 1} {onToggle} {onOpen} {onPin} />
      {/if}
    </li>
  {/each}
</ul>

<style>
  ul { list-style: none; margin: 0; padding: 0; }
  li:focus { outline: none; }
  li:focus-visible > .row { outline: 2px solid var(--focus); outline-offset: -2px; }
  .row {
    position: relative; display: grid; grid-template-columns: 16px 18px minmax(0, 1fr) auto; column-gap: 7px; align-items: start;
    min-height: calc(32px * var(--type-scale)); margin: 1px 8px; padding: 6px 8px 6px calc(4px + var(--depth) * 14px); border-radius: var(--r-2);
    cursor: default; user-select: none; transition: background-color var(--t-fast) var(--ease), box-shadow var(--t-fast) var(--ease);
  }
  .row:hover, .row.lit { background: var(--row-hover); }
  /* The open row is raised, with the editor's mark at its left edge. */
  .row.active { background: var(--raised); box-shadow: var(--raised-edge); }
  .row::before { content: ""; position: absolute; left: 0; top: 7px; bottom: 7px; width: 3px; border-radius: 2px; background: var(--mark); opacity: 0; transform: scaleY(0.4); transition: opacity 160ms var(--ease), transform 160ms var(--ease); }
  .row.active::before { opacity: 1; transform: none; }
  .twisty { display: grid; place-items: center; width: 16px; height: 19px; color: var(--text-3); cursor: pointer; }
  .twisty svg { width: 10px; height: 10px; fill: none; stroke: currentColor; stroke-width: 1.5; stroke-linecap: round; stroke-linejoin: round; transition: transform 160ms var(--ease); }
  .twisty svg.open { transform: rotate(90deg); }
  .icon { width: 18px; height: 18px; margin-top: 0.5px; fill: none; stroke: var(--text-2); stroke-width: 1.5; stroke-linecap: round; stroke-linejoin: round; }
  .row.active .icon { stroke: var(--heading); }
  .t { min-width: 0; overflow-wrap: anywhere; line-height: 1.35; font-size: calc(13.5px * var(--type-scale)); color: var(--text); padding-top: 1px; }
  .row.active .t { color: var(--heading); font-weight: 600; }
  .end { justify-self: end; margin-top: 1px; }
  .new { font-size: calc(10.5px * var(--type-scale)); font-weight: 700; color: var(--heading); line-height: 18px; }
  .formats { font-size: calc(11px * var(--type-scale)); color: var(--text-3); white-space: nowrap; line-height: 18px; }
  /* A row that arrived while the page was open: a brief mark at its edge, then the ordinary row. */
  .row.arrived { animation: arrive 1.6s var(--ease); }
  @keyframes arrive { 0% { box-shadow: inset 3px 0 0 var(--mark); background: var(--row-hover); } 100% { box-shadow: inset 3px 0 0 transparent; } }
  .n { min-width: 20px; padding: 0 6px; border-radius: var(--r-pill); background: var(--track); font-size: calc(11px * var(--type-scale)); font-weight: 650; font-variant-numeric: tabular-nums; color: var(--text-2); text-align: center; line-height: 18px; }
  .mark.failed { font-weight: 700; color: var(--failed); }
  .mark.attention { width: 8px; height: 8px; background: var(--mark); }
  @media (hover: none) { .row { min-height: 42px; } }
  @media (prefers-reduced-motion: reduce) { .row::before, .twisty svg { transition: none; } .row.arrived { animation: none; box-shadow: inset 3px 0 0 var(--mark); } }
</style>
