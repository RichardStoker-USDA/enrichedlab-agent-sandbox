<script lang="ts">
  import { tick } from "svelte";
  import { bytes } from "./language.ts";
  import type { FileGroup, FileKind, FileRow } from "./workbench.ts";

  // All files (long runs rule 6): every retained file of the project, grouped by step, filtered by kind, found by name,
  // with New marks since the reader's last look. Only the lines in view are drawn, so a run with thousands of files
  // scrolls as one with ten; each file still says its place in the whole list. The list is one listbox: the arrow keys
  // move through files, Enter keeps one open, Space opens it as a preview.
  let { groups, open, active = "" }: { groups: FileGroup[]; open: (row: FileRow, keep: boolean) => void; active?: string } = $props();

  const KINDS: [FileKind | "all", string][] = [["all", "All"], ["figure", "Figures"], ["table", "Tables"], ["code", "Code"], ["data", "Data used"],
    ["log", "Logs"], ["other", "Other"]];
  let kind = $state<FileKind | "all">("all");
  let query = $state("");
  let onlyNew = $state(false);
  const all = $derived(groups.flatMap((g) => g.rows));
  const counts = $derived(new Map(KINDS.map(([k]) => [k, k === "all" ? all.length : all.filter((r) => r.kind === k).length])));
  const freshCount = $derived(all.filter((r) => r.fresh).length);
  const words = $derived(query.toLowerCase().split(/\s+/).filter(Boolean));
  const keep = (r: FileRow) => (kind === "all" || r.kind === kind) && (!onlyNew || r.fresh)
    && words.every((w) => r.title.toLowerCase().includes(w) || r.path.toLowerCase().includes(w));

  type Line = { type: "group"; key: string; title: string; count: number }
    | { type: "file"; key: string; row: FileRow; group: string; index: number };
  const lines = $derived.by(() => {
    const out: Line[] = [];
    let index = 0;
    for (const g of groups) {
      const rows = g.rows.filter(keep);
      if (!rows.length) continue;
      out.push({ type: "group", key: `g:${g.key}`, title: g.title, count: rows.length });
      for (const row of rows) out.push({ type: "file", key: row.id, row, group: g.title, index: index++ });
    }
    return out;
  });
  const files = $derived(lines.filter((l): l is Extract<Line, { type: "file" }> => l.type === "file"));

  // One height for every line keeps the window arithmetic exact; a few lines above and below the view are drawn too.
  const ROW = 48, EXTRA = 8;
  let scroller = $state<HTMLElement>();
  let top = $state(0), height = $state(480);
  const first = $derived(Math.max(0, Math.floor(top / ROW) - EXTRA));
  const last = $derived(Math.min(lines.length, Math.ceil((top + height) / ROW) + EXTRA));
  const shown = $derived(lines.slice(first, last).map((line, i) => ({ line, at: first + i })));

  // The file the keyboard is on, kept by identity so filtering does not move it to another file.
  let current = $state("");
  const at = $derived(Math.max(0, files.findIndex((f) => f.key === current)));
  const optionId = (key: string) => `wbf-${key.replace(/[^a-zA-Z0-9_-]/g, "_")}`;
  const ICONS: Record<FileKind, string> = {
    figure: "M3.2 15.8V4.2M3.2 15.8h13.6M6.4 12.6l3-3.6 2.4 2.2 4-4.8",
    table: "M3 4h14v12H3ZM3 8h14M3 12h14M8 4v12",
    code: "M7.2 6 3.2 10l4 4M12.8 6l4 4-4 4",
    data: "M10 3c3.6 0 6.4 1 6.4 2.4S13.6 7.8 10 7.8 3.6 6.8 3.6 5.4 6.4 3 10 3ZM3.6 5.4v9.2C3.6 16 6.4 17 10 17s6.4-1 6.4-2.4V5.4M3.6 10c0 1.4 2.8 2.4 6.4 2.4s6.4-1 6.4-2.4",
    log: "M4 4h12M4 8h12M4 12h8M4 16h10",
    other: "M5.4 2.8h6.2l3.4 3.4v10.2c0 .4-.4.8-.8.8H5.4c-.4 0-.8-.4-.8-.8V3.6c0-.4.4-.8.8-.8ZM11.4 2.8v3.6H15",
  };

  function restart() { current = ""; top = 0; if (scroller) scroller.scrollTop = 0; }
  async function moveTo(i: number) {
    const target = files[Math.max(0, Math.min(files.length - 1, i))];
    if (!target || !scroller) return;
    current = target.key;
    const line = lines.indexOf(target);
    // Keep the file in view, with its group's line above the first file.
    if (line * ROW < scroller.scrollTop + ROW) scroller.scrollTop = Math.max(0, (line - 1) * ROW);
    else if ((line + 1) * ROW > scroller.scrollTop + scroller.clientHeight) scroller.scrollTop = (line + 1) * ROW - scroller.clientHeight;
    top = scroller.scrollTop;
    await tick();
  }
  function keys(event: KeyboardEvent) {
    const page = Math.max(1, Math.floor(height / ROW) - 1);
    switch (event.key) {
      case "ArrowDown": void moveTo(current ? at + 1 : 0); break;
      case "ArrowUp": void moveTo(at - 1); break;
      case "PageDown": void moveTo(at + page); break;
      case "PageUp": void moveTo(at - page); break;
      case "Home": void moveTo(0); break;
      case "End": void moveTo(files.length - 1); break;
      case "Enter": if (files[at]) open(files[at].row, true); break;
      case " ": if (files[at]) open(files[at].row, false); break;
      default: return;
    }
    event.preventDefault();
  }
</script>

<div class="files">
  <div class="tools">
    <label class="search-field"><span class="visually-hidden">Find a file by name</span>
      <svg viewBox="0 0 16 16" aria-hidden="true"><circle cx="7" cy="7" r="4.6" /><path d="m10.4 10.4 3.4 3.4" /></svg>
      <input type="search" placeholder="Find a file by name" bind:value={query} oninput={restart} /></label>
    <div class="kinds" role="group" aria-label="Show files of one kind">
      {#each KINDS as [k, label] (k)}
        {#if k === "all" || counts.get(k)}<button aria-pressed={kind === k} onclick={() => { kind = k; restart(); }}>{label}<span class="count">{counts.get(k)}</span></button>{/if}
      {/each}
      {#if freshCount}<button aria-pressed={onlyNew} onclick={() => { onlyNew = !onlyNew; restart(); }}>New since your last look<span class="count">{freshCount}</span></button>{/if}
    </div>
    <p class="tally" aria-live="polite">{files.length === all.length ? `${all.length} ${all.length === 1 ? "file" : "files"}` : `${files.length} of ${all.length} files`}</p>
  </div>
  {#if files.length}
    <!-- svelte-ignore a11y_no_noninteractive_tabindex -->
    <div class="scroller" bind:this={scroller} bind:clientHeight={height} onscroll={() => (top = scroller?.scrollTop ?? 0)}
      role="listbox" tabindex="0" aria-label="Files, by step" aria-activedescendant={current ? optionId(current) : undefined} onkeydown={keys}
      onfocus={() => { if (!current && files[0]) current = files[0].key; }}>
      <div class="space" style:height={`${lines.length * ROW}px`}>
        {#each shown as { line, at: place } (line.key)}
          {#if line.type === "group"}
            <div class="group" style:transform={`translateY(${place * ROW}px)`} aria-hidden="true"><span class="g-title">{line.title}</span><span class="count">{line.count}</span></div>
          {:else}
            <!-- svelte-ignore a11y_click_events_have_key_events -->
            <div id={optionId(line.key)} class="file" class:current={line.key === current} class:open={line.key === active} class:bad={line.row.manifest === "mismatched"}
              style:transform={`translateY(${place * ROW}px)`} role="option" tabindex="-1" aria-selected={line.key === current} aria-setsize={files.length} aria-posinset={line.index + 1}
              aria-label={`${line.row.title}, ${line.group}${line.row.fresh ? ", new since your last look" : ""}${line.row.manifest === "mismatched" ? ", does not match its checksum" : ""}`}
              onclick={() => { current = line.key; open(line.row, false); }} ondblclick={() => open(line.row, true)}>
              <svg class="icon" viewBox="0 0 20 20" aria-hidden="true"><path d={ICONS[line.row.kind]} /></svg>
              <span class="name"><span class="t">{line.row.title}</span><span class="p">{line.row.path}{line.row.originalPath ? ` · ${line.row.originalPath.split("/").at(-1)}` : ""}</span></span>
              {#if line.row.fresh}<span class="new">New</span>{/if}
              {#if line.row.manifest === "mismatched"}<span class="mismatch">Does not match</span>{/if}
              <span class="size">{bytes(line.row.size)}</span>
            </div>
          {/if}
        {/each}
      </div>
    </div>
  {:else}
    <p class="empty">{all.length ? "No file matches. Clear the search or choose All." : "No retained file yet. Files appear here as steps finish."}</p>
  {/if}
</div>

<style>
  .files { display: grid; grid-template-rows: auto minmax(0, 1fr); gap: 12px; height: 100%; min-height: 360px; }
  .tools { display: grid; gap: 10px; }
  .search-field { max-width: 420px; }
  .kinds { display: flex; flex-wrap: wrap; gap: 4px 18px; border-bottom: 1px solid var(--line); }
  .kinds button { min-height: 36px; padding: 0; font-size: var(--font-small); font-weight: 600; color: var(--text-2); border-bottom: 2px solid transparent; }
  .kinds button:hover { color: var(--heading); }
  .kinds button[aria-pressed="true"] { color: var(--heading); border-bottom-color: var(--mark); }
  .count { margin-left: 6px; font-weight: 400; font-variant-numeric: tabular-nums; color: var(--text-3); }
  .tally { font-size: var(--font-xs); color: var(--text-3); font-variant-numeric: tabular-nums; }
  .scroller { position: relative; min-height: 0; overflow: auto; border-radius: var(--r-3); background: var(--surface); box-shadow: var(--group-edge); overscroll-behavior: contain; }
  .scroller:focus-visible { outline: 2px solid var(--focus); outline-offset: 2px; }
  .space { position: relative; }
  .group, .file { position: absolute; left: 0; right: 0; top: 0; height: 48px; display: flex; align-items: center; gap: 12px; padding: 0 16px; }
  .group { font-size: var(--font-xs); font-weight: 700; color: var(--text-2); background: var(--track); border-bottom: 1px solid var(--line); }
  .g-title { min-width: 0; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
  .file { border-bottom: 1px solid var(--line); cursor: default; transition: background-color var(--t-fast) var(--ease); }
  .file:hover { background: var(--row-hover); }
  .file.current { box-shadow: inset 3px 0 0 var(--mark); background: var(--row-hover); }
  .file.open .t { color: var(--heading); font-weight: 650; }
  .icon { flex: none; width: 18px; height: 18px; fill: none; stroke: var(--text-2); stroke-width: 1.5; stroke-linecap: round; stroke-linejoin: round; }
  .name { flex: 1; min-width: 0; display: grid; gap: 1px; }
  .t { font-size: calc(13.5px * var(--type-scale)); color: var(--text); white-space: nowrap; overflow: hidden; text-overflow: ellipsis; }
  .p { font: calc(11.5px * var(--type-scale)) / 1.3 var(--mono); color: var(--text-3); white-space: nowrap; overflow: hidden; text-overflow: ellipsis; }
  .new { flex: none; font-size: var(--font-xs); font-weight: 700; color: var(--heading); }
  .mismatch { flex: none; font-size: var(--font-xs); font-weight: 650; color: var(--failed); }
  .size { flex: none; min-width: 64px; text-align: end; font-size: var(--font-xs); color: var(--text-3); font-variant-numeric: tabular-nums; }
  .empty { padding: 14px 2px; font-size: var(--font-small); color: var(--text-3); }
  @media (max-width: 640px) { .size { display: none; } .file, .group { padding: 0 12px; } }
  @media (prefers-reduced-motion: reduce) { .file { transition: none; } }
</style>
