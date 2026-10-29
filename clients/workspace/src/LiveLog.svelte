<script lang="ts">
  import { onDestroy, onMount, tick } from "svelte";
  import { watchLog } from "./api.ts";
  import { bytes } from "./language.ts";
  import { checkRecord, clock, findIn, LogBuilder, marked, plainText, quietWords, withSystem, type LogClosed, type LogRow, type RecordCheck,
    type Span } from "./live_log.ts";

  // A running step's output as it is written (Glass Box results rule 6). Text, not a terminal: it follows new lines until
  // the reader scrolls up, then offers a jump back with the count of new lines; messages, errors and the step's own
  // lifecycle (System) filter apart; find, times, wrap and copy. The text is marked as not yet checked until the step
  // ends and the broker's record of it matches what arrived here; the saved output files keep the exact bytes.
  let { task, running, system = [], onSave, label = "Output of this step", fill = false }: {
    task: string; running: boolean; system?: { at: string; text: string }[];
    onSave?: (artifact: string) => void; label?: string;
    /** Take the height of the panel it sits in, so the newest lines stay in view however tall the panel is. */
    fill?: boolean;
  } = $props();
  const uid = $props.id();

  let builder = new LogBuilder();
  let version = $state(0);
  let connection = $state<"connecting" | "live" | "lost" | "gone" | "ended">("connecting");
  let ended = $state<LogClosed | null>(null);
  let check = $state<RecordCheck | null>(null);
  let lastPiece = $state(Date.now());
  let now = $state(Date.now());
  let show = $state<"all" | "stdout" | "stderr" | "combined" | "system">("all");
  let query = $state("");
  let at = $state(0);
  let times = $state(false);
  let wrap = $state(true);
  let follow = $state(true);
  let unseen = $state(0);
  let copied = $state("");
  let body = $state<HTMLElement>();
  let stop: (() => void) | null = null;
  let frame = 0;

  // Pieces are drawn once per frame however fast they arrive.
  function redraw() {
    if (frame) return;
    frame = requestAnimationFrame(() => { frame = 0; version++; });
  }
  function connect() {
    stop?.();
    connection = "connecting";
    stop = watchLog(task, builder.last, {
      open: () => { connection = "live"; },
      piece: (piece) => { if (builder.add(piece)) { lastPiece = Date.now(); redraw(); } },
      closed: (closed) => {
        ended = closed; connection = "ended";
        void checkRecord(builder.sent, closed).then((result) => { check = result; });
        redraw();
      },
      lost: () => { connection = "lost"; },
      gone: () => { connection = "gone"; },
    });
  }
  onMount(() => {
    connect();
    const ticker = setInterval(() => { now = Date.now(); }, 5000);
    return () => clearInterval(ticker);
  });
  onDestroy(() => { stop?.(); if (frame) cancelAnimationFrame(frame); });

  const all = $derived.by(() => { void version; return withSystem(builder.view(), system); });
  const kinds = $derived(new Set(all.flatMap((r) => (r.kind === "line" ? [r.stream] : r.kind === "system" || r.kind === "attempt" ? ["system"] : []))));
  // The cluster merges a step's messages and errors into one stream; a source that keeps them apart
  // gets one filter each.
  const FILTERS = [["all", "All"], ["stdout", "Messages"], ["stderr", "Errors"], ["combined", "Messages and errors"], ["system", "System"]] as const;
  const filters = $derived(FILTERS.filter(([key]) => key === "all" || kinds.has(key)));
  const rows = $derived(show === "all" ? all : all.filter((r) => show === "system" ? r.kind === "system" || r.kind === "attempt"
    : r.kind === "line" && r.stream === show));
  const found = $derived(findIn(rows, query));
  const current = $derived(found.length ? Math.min(at, found.length - 1) : -1);
  const byRow = $derived.by(() => {
    const map = new Map<number, { start: number; end: number; current: boolean }[]>();
    found.forEach((m, i) => map.set(m.row, [...(map.get(m.row) ?? []), { start: m.start, end: m.end, current: i === current }]));
    return map;
  });
  // The newest rows are drawn, at most `limit` of them; earlier ones on request, so a long log stays quick to scroll.
  let limit = $state(5000);
  const from = $derived(Math.max(0, rows.length - limit));
  const drawn = $derived(rows.slice(from));
  const lineCount = $derived(all.filter((r) => r.kind === "line").length);
  // Quiet since the last piece, or since the log was opened when nothing has come yet.
  const quiet = $derived(connection === "live" && running && now - lastPiece >= 20_000 ? now - lastPiece : 0);
  const removed = $derived.by(() => { void version; return builder.removed; });

  // New lines keep the newest in view while the reader follows; otherwise they are counted for the jump back. Only
  // arrivals count: a filter that shows rows again adds none.
  let seen = 0;
  $effect(() => {
    void rows;
    const count = all.length;
    const added = count - seen;
    seen = count;
    if (follow) void tick().then(() => { if (body) body.scrollTop = body.scrollHeight; });
    else if (added > 0) unseen += added;
  });
  function scrolled() {
    if (!body) return;
    const bottom = body.scrollHeight - body.scrollTop - body.clientHeight < 24;
    follow = bottom;
    if (bottom) unseen = 0;
  }
  function jump() {
    follow = true; unseen = 0;
    if (body) body.scrollTop = body.scrollHeight;
  }
  // Moving to a match stops following, so the match stays where the reader can read it.
  async function move(by: number) {
    if (!found.length) return;
    at = (current + by + found.length) % found.length;
    follow = false;
    await tick();
    // Only the log scrolls, so the page stays where the reader left it.
    const mark = body?.querySelector(`[data-row="${found[at]?.row}"] mark.current`);
    if (body && mark) body.scrollTop += mark.getBoundingClientRect().top - body.getBoundingClientRect().top - body.clientHeight / 2;
  }
  function findKeys(event: KeyboardEvent) {
    if (event.key === "Enter") { event.preventDefault(); void move(event.shiftKey ? -1 : 1); }
    else if (event.key === "Escape" && query) { event.preventDefault(); event.stopPropagation(); query = ""; }
  }
  async function copy() {
    const text = plainText(rows);
    try {
      await navigator.clipboard.writeText(text);
      copied = `Copied ${rows.length.toLocaleString()} ${rows.length === 1 ? "line" : "lines"}.`;
    } catch { copied = "Copying is not allowed here. Select the text to copy it."; }
  }
  const styleClass = (s: Span) => [s.style.bold && "b", s.style.dim && "d", s.style.italic && "i", s.style.underline && "u", s.style.tone && `tone-${s.style.tone}`]
    .filter(Boolean).join(" ");
  const digits = $derived(String(Math.max(lineCount, 9)).length);
  // A step that ended without a closure record may have no saved output to point to.
  const saved = $derived(ended?.artifacts.length ? " The saved output below is the record." : "");
  const checkWords = $derived(!ended ? "" : !check ? "Checking the text against the broker's record…"
    : check.state === "checked" ? `Ended. The text shown is the text the broker recorded: ${check.pieces.toLocaleString()} ${check.pieces === 1 ? "piece" : "pieces"}, ${bytes(check.size)}, SHA-256 ${check.sha256.slice(0, 12)}….`
    : check.state === "differs" ? `Ended, but the text shown here does not match the broker's record.${saved}`
    : check.state === "incomplete" ? `Ended, but ${check.missing.toLocaleString()} ${check.missing === 1 ? "piece" : "pieces"} of it did not arrive here, so the text cannot be checked.${saved}`
    : lineCount ? `Ended. The broker kept no record of this text, so it cannot be checked.${saved}`
    : "Ended. No live output was kept for this step.");
</script>

<section class="live-log" class:fill aria-labelledby={`${uid}-title`}>
  <header class="bar">
    <div class="head">
      <h3 id={`${uid}-title`}>Output</h3>
      <p class="state" role="status">
        {#if connection === "connecting"}Connecting to the step's output…
        {:else if connection === "live"}Live. Not yet checked; the text is checked against the broker's record when the step ends.
        {:else if connection === "lost"}The connection dropped. Reconnecting from the last line received…
        {:else if connection === "gone"}The live output could not be reached.
        {:else}{checkWords}{/if}
      </p>
    </div>
    <div class="tools">
      {#if filters.length > 2}
        <div class="segmented" role="radiogroup" aria-label="Show">
          {#each filters as [key, name] (key)}<label><input type="radio" name={`${uid}-show`} checked={show === key} onchange={() => (show = key)} />{name}</label>{/each}
        </div>
      {/if}
      <div class="find">
        <input type="search" placeholder="Find" aria-label="Find in the output" bind:value={query} oninput={() => { at = 0; void tick().then(() => move(0)); }} onkeydown={findKeys} />
        {#if query}<span class="count" aria-live="polite">{found.length ? `${current + 1} of ${found.length}` : "No match"}</span>{/if}
        <button class="icon" aria-label="Previous match" disabled={!found.length} onclick={() => move(-1)}><svg viewBox="0 0 16 16" aria-hidden="true"><path d="M4 10l4-4 4 4" /></svg></button>
        <button class="icon" aria-label="Next match" disabled={!found.length} onclick={() => move(1)}><svg viewBox="0 0 16 16" aria-hidden="true"><path d="M4 6l4 4 4-4" /></svg></button>
      </div>
      <label class="switch"><input type="checkbox" bind:checked={times} />Times</label>
      <label class="switch"><input type="checkbox" bind:checked={wrap} />Wrap lines</label>
      <button class="button secondary compact" onclick={copy} disabled={!rows.length}>Copy</button>
    </div>
  </header>
  {#if copied}<p class="copied small" role="status">{copied}</p>{/if}

  <div class="frame">
    <!-- svelte-ignore a11y_no_noninteractive_tabindex -->
    <div class="body" class:wrap role="log" aria-live="off" aria-label={label} tabindex="0" bind:this={body} onscroll={scrolled} style:--digits={digits}>
      {#if !rows.length}
        <p class="empty">{connection === "gone" ? "Nothing arrived." : show === "all" ? "Nothing written yet." : "Nothing of this kind yet."}</p>
      {/if}
      {#if from > 0}
        <button class="link small earlier" onclick={() => (limit += 5000)}>Show {Math.min(from, 5000).toLocaleString()} earlier {from === 1 ? "line" : "lines"}</button>
      {/if}
      {#each drawn as row, k (from + k)}
            {@const i = from + k}
            <div class="row {row.kind}" class:err={row.kind === "line" && row.stream === "stderr"} data-row={i}>
              {#if row.kind === "line"}
                <span class="n" aria-hidden="true">{row.open ? "" : row.n}</span>
                {#if times}<time datetime={row.at}>{clock(row.at)}</time>{/if}
                <span class="t">{#each marked(row.spans, byRow.get(i) ?? []) as s, j (j)}{#if s.hit}<mark class={`${styleClass(s)} ${s.hit}`}>{s.text}</mark>{:else}<span class={styleClass(s)}>{s.text}</span>{/if}{/each}</span>
              {:else if row.kind === "attempt"}
                <span class="n" aria-hidden="true"></span>{#if times}<time datetime={row.at}>{clock(row.at)}</time>{/if}
                <span class="t marker">Attempt {row.attempt} started</span>
              {:else}
                <span class="n" aria-hidden="true"></span>{#if times}<time datetime={row.at}>{clock(row.at)}</time>{/if}
                <span class="t marker">{#each marked([{ text: row.text, style: {} }], byRow.get(i) ?? []) as s, j (j)}{#if s.hit}<mark class={s.hit}>{s.text}</mark>{:else}{s.text}{/if}{/each}</span>
              {/if}
            </div>
      {/each}
    </div>
    {#if !follow && unseen > 0}
      <button class="jump" onclick={jump}>{unseen.toLocaleString()} new {unseen === 1 ? "line" : "lines"} · Jump to the newest</button>
    {/if}
  </div>

  <footer class="foot small">
    {#if quiet}<p>{lineCount ? `Quiet for ${quietWords(quiet)}.` : `Nothing written in ${quietWords(quiet)}.`} The step is still running.</p>{/if}
    {#if connection === "gone"}
      <p>The step's saved output appears with its files when it ends. <button class="link" onclick={connect}>Try again</button></p>
    {/if}
    {#if ended?.artifacts.length}
      <p class="saved">The saved output keeps the exact bytes:
        {#each ended.artifacts as a (a.artifact)}
          {#if onSave}<button class="link" onclick={() => onSave?.(a.artifact)}>{a.role === "stderr" ? "errors" : "messages"} ({bytes(a.size)})</button>
          {:else}<span>{a.role === "stderr" ? "errors" : "messages"} ({bytes(a.size)})</span>{/if}
        {/each}
      </p>
    {/if}
    {#if ended && ended.dropped}<p>{ended.dropped.toLocaleString()} bytes of output are not in the live text (past the limit for one attempt, or not stored); the saved output has them.</p>{/if}
    {#if removed}<p>{removed.toLocaleString()} bytes of terminal control codes were removed before the text reached the page.</p>{/if}
  </footer>
</section>

<style>
  .live-log { display: grid; gap: 10px; min-width: 0; }
  .live-log.fill { display: flex; flex-direction: column; height: 100%; min-height: 0; }
  .bar { display: grid; gap: 10px; }
  .head { display: flex; flex-wrap: wrap; align-items: baseline; gap: 4px 14px; }
  h3 { font-size: calc(15px * var(--type-scale)); color: var(--heading); }
  .state { font-size: var(--font-small); color: var(--text-2); max-width: 72ch; }
  .tools { display: flex; flex-wrap: wrap; align-items: center; gap: 8px 14px; }
  .find { display: inline-flex; align-items: center; gap: 4px; }
  .find input { width: 13rem; max-width: 46vw; min-height: 36px; }
  .count { font-size: var(--font-xs); color: var(--text-2); font-variant-numeric: tabular-nums; min-width: 5.5em; }
  .icon { width: 36px; height: 36px; border-radius: var(--r-2); display: grid; place-items: center; color: var(--heading); }
  .icon:hover:not(:disabled) { background: var(--hover-wash); }
  .icon:disabled { color: var(--text-3); }
  .icon svg { width: 14px; height: 14px; fill: none; stroke: currentColor; stroke-width: 1.8; stroke-linecap: round; stroke-linejoin: round; }
  .switch { display: inline-flex; align-items: center; gap: 6px; min-height: 36px; font-size: var(--font-small); cursor: pointer; }
  .copied { color: var(--text-2); }
  .frame { position: relative; min-width: 0; }
  /* Logs are text on the page's own surface, not a terminal: one hairline, the page's ink, the system monospace. */
  .body { max-height: min(60vh, 560px); min-height: 160px; overflow: auto; padding: 8px 0; border-radius: var(--r-3); background: var(--surface);
    box-shadow: inset 0 0 0 1px var(--line); font-family: var(--mono); font-size: calc(12.5px * var(--type-scale)); line-height: 1.55; tab-size: 4; overscroll-behavior: contain; }
  .body:focus-visible { outline: 2px solid var(--focus); outline-offset: 2px; }
  .earlier { margin: 2px 0 8px calc(var(--digits) * 1ch + 30px); }
  .row { display: grid; grid-template-columns: calc(var(--digits) * 1ch + 20px) auto minmax(0, 1fr); column-gap: 10px; padding: 0 14px 0 0; position: relative; }
  .row:not(:has(time)) { grid-template-columns: calc(var(--digits) * 1ch + 20px) minmax(0, 1fr); }
  .row.err::before { content: ""; position: absolute; left: 0; top: 2px; bottom: 2px; width: 2px; background: var(--failed); }
  .n { text-align: right; color: var(--text-3); user-select: none; font-variant-numeric: tabular-nums; }
  time { color: var(--text-3); font-variant-numeric: tabular-nums; }
  .t { white-space: pre; color: var(--text); min-width: 0; }
  .body.wrap .t { white-space: pre-wrap; overflow-wrap: anywhere; }
  .t.marker { font-family: var(--ui); font-style: italic; color: var(--text-2); white-space: normal; }
  .b { font-weight: 700; }
  .d { color: var(--text-2); }
  .i { font-style: italic; }
  .u { text-decoration: underline; }
  .tone-alarm { color: var(--failed); }
  .tone-note { color: var(--active); }
  .tone-strong { color: var(--heading); font-weight: 600; }
  mark { background: var(--focus-halo); color: inherit; border-radius: 2px; }
  mark.current { outline: 2px solid var(--focus); outline-offset: 0; }
  .empty { padding: 10px 16px; color: var(--text-3); font-family: var(--ui); font-size: var(--font-small); }
  .jump { position: absolute; left: 50%; bottom: 12px; transform: translateX(-50%); min-height: 36px; padding: 0 14px; border-radius: var(--r-pill);
    background: var(--raised); color: var(--heading); font-weight: 600; font-size: var(--font-small); box-shadow: var(--overlay-shadow); white-space: nowrap; }
  .fill .frame { flex: 1; min-height: 0; display: flex; flex-direction: column; }
  .fill .body { flex: 1; max-height: none; min-height: 96px; }
  .foot { display: grid; gap: 4px; color: var(--text-2); }
  .foot:empty { display: none; }
  .saved { display: flex; flex-wrap: wrap; gap: 4px 12px; align-items: baseline; }
  @media (max-width: 640px) {
    .find input { width: 9rem; }
    .body { max-height: 56vh; }
  }
</style>
