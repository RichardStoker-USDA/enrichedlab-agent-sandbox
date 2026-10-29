<script lang="ts">
  import { outputsOf, rowRole, type ProfileSheet, type RowRole, type Spec } from "./lab_data.ts";

  // The sheet as the lab wrote it (rule 1): the spreadsheet's own row numbers and column letters, the first rows
  // and columns the profile's detail kept, and the broker's reading drawn on them. A row number leaves its row out or
  // takes it back; a column letter stops or starts reading its column. Cells are the person's own text, shown as text.
  let { preview, sheet, spec, onRow, onColumn }: {
    preview: string[][]; sheet: ProfileSheet; spec: Spec;
    onRow: (row: number) => void; onColumn: (letter: string) => void;
  } = $props();

  const LIMITED = /^[<>≤≥]\s*=?\s*[-+]?(?:\d+(?:[.,]\d*)?|[.,]\d+)/;
  const width = $derived(Math.min(30, Math.max(sheet.column_count, ...preview.map((r) => r.length))));
  const letters = $derived(Array.from({ length: width }, (_, i) => sheet.columns[i]?.letter ?? letterOf(i)));
  function letterOf(i: number): string { return (i >= 26 ? letterOf(Math.floor(i / 26) - 1) : "") + String.fromCharCode(65 + (i % 26)); }
  const longAt = $derived(new Map((spec.long?.columns ?? []).map((l, i) => [l, i])));
  const outputs = $derived(outputsOf(spec));
  // What each column becomes: its name in the prepared table, a time point of the long layout, or nothing.
  function headOf(letter: string): { read: boolean; name: string; long: boolean } {
    const i = longAt.get(letter);
    if (i !== undefined && spec.long) return { read: true, name: `${spec.long.value.name} · ${spec.long.values[i]}`, long: true };
    const c = spec.columns.find((x) => x.from === letter);
    return c ? { read: true, name: c.name, long: false } : { read: false, name: "not read", long: false };
  }
  const heads = $derived(letters.map((l) => ({ letter: l, ...headOf(l) })));
  const missingWords = $derived(new Set(spec.missing));
  const roles = $derived(preview.map((cells, i) => rowRole(spec, sheet.table, i + 1, !cells.some((c) => c.trim()))));
  const TAG: Record<RowRole, string> = { title: "Title", names: "Names", units: "Units", data: "", left_out: "Left out", summary: "Summary, left out",
    blank: "Blank", note: "Note", outside: "" };
  const inRange = (row: number) => row >= spec.rows.first && row <= spec.rows.last;
  const recodeOf = (letter: string) => spec.columns.find((c) => c.from === letter)?.recode ?? {};
  // A data cell's mark: a value against a limit, a missing word, or a spelling the preparation rewrites.
  function cellMark(role: RowRole, letter: string, text: string): { kind: "" | "limit" | "missing" | "recoded"; note: string } {
    if (role !== "data" || !headOf(letter).read) return { kind: "", note: "" };
    const t = text.trim();
    if (!t) return { kind: "", note: "" };
    if (missingWords.has(t)) return { kind: "missing", note: "Read as missing" };
    if (LIMITED.test(t)) return { kind: "limit", note: "Reported against a limit" };
    const to = recodeOf(letter)[t];
    if (to !== undefined && to !== text) return { kind: "recoded", note: `Written as ${to}` };
    return { kind: "", note: "" };
  }
  const shownRows = $derived(preview.length);
  const more = $derived([sheet.row_count > shownRows ? `rows past ${shownRows}` : "", sheet.column_count > width ? `columns past ${letters[width - 1]}` : ""].filter(Boolean));
</script>

<div class="sheet-wrap">
  <div class="sheet" role="region" aria-label="The sheet's cells" tabindex="-1">
    <table>
      <thead>
        <tr>
          <th class="corner" scope="col"><span class="visually-hidden">Row</span></th>
          {#each heads as h, i (h.letter)}
            <th scope="col" class="col" class:unread={!h.read} class:long={h.long} class:long-first={h.long && (i === 0 || !heads[i - 1].long)}
              class:long-last={h.long && (i === heads.length - 1 || !heads[i + 1].long)}>
              <button class="letter" aria-pressed={h.read} disabled={h.long} onclick={() => onColumn(h.letter)}
                title={h.long ? "Part of the time points; change the layout below" : h.read ? `Stop reading column ${h.letter}` : `Read column ${h.letter}`}>
                {h.letter}
              </button>
              <span class="becomes">{h.name}</span>
            </th>
          {/each}
        </tr>
      </thead>
      <tbody>
        {#each preview as cells, r (r)}
          {@const row = r + 1}
          {@const role = roles[r]}
          <tr class={role}>
            <th scope="row" class="num">
              {#if inRange(row)}
                <button aria-pressed={role === "data" || role === "blank"} onclick={() => onRow(row)}
                  title={role === "data" || role === "blank" ? `Leave row ${row} out` : `Read row ${row} again`}>{row}</button>
              {:else}<span class="plain">{row}</span>{/if}
              {#if TAG[role]}<span class="tag">{TAG[role]}</span>{/if}
            </th>
            {#each heads as h, c (h.letter)}
              {@const text = cells[c] ?? ""}
              {@const mark = cellMark(role, h.letter, text)}
              <td class:unread={!h.read} class={mark.kind} title={text ? (mark.note ? `${text} · ${mark.note}` : text) : undefined}>{text}</td>
            {/each}
          </tr>
        {/each}
      </tbody>
    </table>
  </div>
  <p class="small faint legend">
    Row numbers and column letters are the spreadsheet's. Select a row number to leave that row out or read it again, or a column letter to stop or start
    reading that column.{#if more.length} The {more.join(" and ")} are not shown here; the same choices apply to them.{/if}
    {#if outputs.length}<span class="visually-hidden">The prepared table will have {outputs.length} columns.</span>{/if}
  </p>
</div>

<style>
  .sheet-wrap { display: grid; gap: 8px; min-width: 0; }
  .sheet { overflow: auto; max-height: min(66vh, 620px); border-radius: var(--r-2); background: var(--surface); box-shadow: var(--group-edge); overscroll-behavior: contain; }
  .sheet:focus { outline: none; }
  table { border-collapse: separate; border-spacing: 0; font-size: var(--font-small); font-variant-numeric: tabular-nums; min-width: max-content; }
  th, td { border-right: 1px solid var(--track-line); border-bottom: 1px solid var(--track-line); }
  thead th { position: sticky; top: 0; z-index: 2; background: var(--surface-2); vertical-align: bottom; text-align: left; padding: 4px 8px 5px; }
  .corner { left: 0; z-index: 3; min-width: 74px; }
  .col { min-width: 9ch; }
  .letter { display: block; font: inherit; font-weight: 650; color: var(--heading); background: none; border: 0; padding: 2px 4px; margin: -2px -4px 1px;
    border-radius: var(--r-1); cursor: pointer; letter-spacing: 0.02em; }
  .letter:hover:not(:disabled) { background: var(--hover-wash); }
  .letter:disabled { cursor: default; opacity: 1; }
  .becomes { display: block; font-family: var(--mono); font-size: var(--font-xs); font-weight: 500; color: var(--text-2); white-space: nowrap; max-width: 24ch;
    overflow: hidden; text-overflow: ellipsis; }
  .unread .becomes, .col.unread .letter { color: var(--text-3); font-weight: 500; }
  /* The time points read as one measure: one rule under their letters, from the first to the last. */
  .col.long { box-shadow: inset 0 -2px 0 var(--mark); }
  tbody th { position: sticky; left: 0; z-index: 1; background: var(--surface-2); text-align: left; padding: 3px 8px; white-space: nowrap; font-weight: 500; }
  .num button, .num .plain { font: inherit; color: var(--text-2); min-width: 2.6ch; display: inline-block; text-align: right; }
  .num button { background: none; border: 0; padding: 1px 4px; margin: -1px -4px; border-radius: var(--r-1); cursor: pointer; color: var(--heading); }
  .num button:hover { background: var(--hover-wash); }
  .tag { margin-left: 8px; font-size: var(--font-xs); color: var(--text-3); }
  td { padding: 3px 8px; white-space: nowrap; max-width: 26ch; overflow: hidden; text-overflow: ellipsis; color: var(--text); }
  td.unread { color: var(--text-3); }
  tr.names td { font-weight: 650; color: var(--heading); }
  tr.units td { color: var(--text-2); font-style: italic; }
  tr.title td, tr.note td { color: var(--text-3); font-style: italic; }
  tr.outside td { color: var(--text-3); }
  tr.summary td, tr.left_out td { color: var(--text-3); text-decoration: line-through; text-decoration-color: var(--line-2); }
  tr.summary .num button, tr.left_out .num button { color: var(--text-3); }
  td.limit { text-decoration: underline dotted var(--mark); text-decoration-thickness: 2px; text-underline-offset: 3px; }
  td.missing { color: var(--text-3); }
  td.recoded { text-decoration: underline dotted var(--line-2); text-underline-offset: 3px; }
  .legend { max-width: 78ch; }
  @media (prefers-reduced-motion: no-preference) { .letter, .num button { transition: background-color var(--t-fast) var(--ease); } }
  /* On a phone the row numbers keep their tags on a second line, so the sheet keeps its width for cells. */
  @media (max-width: 640px) {
    .corner { min-width: 0; }
    tbody th { padding: 3px 6px; }
    .tag { display: block; margin-left: 0; font-size: 10.5px; line-height: 1.2; }
  }
</style>
