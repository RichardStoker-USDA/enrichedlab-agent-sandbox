<script lang="ts">
  import Papa from "papaparse";
  import { onDestroy, onMount, untrack } from "svelte";
  import { BrokerError } from "../../api.ts";
  import { strictText } from "../../artifact_content.ts";
  import FigureView from "../../FigureView.svelte";
  import { MAX_TABLE_BYTES } from "../../viewer.ts";
  import type { Location, ViewContext } from "../types.ts";
  import { perspAngle } from "./angle.ts";
  import { buildGrid, MAX_POINTS, VIRIDIS, type Grid } from "./grid.ts";
  import type { CameraState, SurfaceScene } from "./scene.ts";

  // 3D (Glass Box, results rules 3 and 5): the table's stored points as a surface that turns, in the columns the method
  // declared. It reads the same pages the values view reads, draws each row once at its stored values, leaves gaps
  // where the table has no rows, colours by viridis and prints its vertical scale. The drawing code loads only now;
  // when no GPU path works, R's figure of the same numbers stands in.
  // A view's context is fixed for its mount; the frame mounts a new view for a new file.
  const props: { ctx: ViewContext } = $props();
  const ctx = untrack(() => props.ctx);
  const x = ctx.encoding.x, y = ctx.encoding.y, z = ctx.encoding.z;
  let host = $state<HTMLElement>();
  let phase = $state<"reading" | "drawing" | "ready" | "still" | "refused">("reading");
  let reason = $state("");
  let backend = $state("");
  let height = $state(Number(ctx.state?.height ?? 1));
  let facts = $state<{ points: number; low: string; high: string; across: [string, string]; deep: [string, string]; gaps: string; missing: number } | null>(null);
  let grid: Grid | null = null;
  let scene = $state.raw<SurfaceScene | null>(null);
  let pending: number | null = null;
  // Each row's stored strings and line, and the vertex it became.
  const stored = new Map<number, { x: string; y: string; z: string; line: number | null }>();
  const vertexOf = new Map<number, number>();
  // The table's columns and which hold numbers, for the figure editor.
  let table: { columns: string[]; numeric: boolean[] } = { columns: [], numeric: [] };
  const SCALES = [1, 2, 4];
  const radios = `surface-scale-${Math.random().toString(36).slice(2, 9)}`;
  const controller = new AbortController();

  async function readRows(): Promise<{ columns: string[]; types: string[] | null; rows: { index: number; line: number | null; values: string[] }[] }> {
    if (ctx.page) {
      const rows: { index: number; line: number | null; values: string[] }[] = [];
      let columns: string[] = [], types: string[] = [], total = Infinity;
      try {
        for (let offset = 0; offset < total; offset += 1000) {
          const page = await ctx.page({ offset, limit: 1000 }, controller.signal);
          if (page.sha256 !== ctx.file.sha256) throw new Error("other_bytes");
          columns = page.columns; types = page.types; total = page.total;
          if (total > MAX_POINTS) throw new Error("too_many");
          rows.push(...page.rows);
        }
        return { columns, types, rows };
      } catch (cause) {
        // A broker without pages answers 404; the verified file is then read whole, as the values view does.
        if (!(cause instanceof BrokerError && cause.code === "record_unavailable" && cause.reason !== "package_member_not_found")) throw cause;
      }
    }
    if (ctx.file.size_bytes > MAX_TABLE_BYTES) throw new Error("too_large");
    const text = await strictText(await ctx.read(controller.signal), MAX_TABLE_BYTES);
    const parsed = Papa.parse<string[]>(text, { delimiter: ctx.file.path.toLowerCase().endsWith(".tsv") ? "\t" : ",", header: false, dynamicTyping: false, skipEmptyLines: true });
    if (parsed.errors.length || !parsed.data.length) throw new Error("unreadable");
    const [columns, ...body] = parsed.data;
    return { columns, types: null, rows: body.map((values, index) => ({ index, line: null, values })) };
  }

  const refusals: Record<string, string> = {
    too_few: "The table holds fewer than two values on an axis, so it is not a surface.",
    too_many: `The table holds more points than the 3D view draws (${MAX_POINTS.toLocaleString()}). The values view pages through all of them.`,
    not_numeric: "An axis holds values that are not numbers, so the surface cannot place them.",
    repeated: "Some rows share the same place on both axes, so the table is not one surface.",
    other_bytes: "The server's pages came from other bytes than this file's checksum names, so they are not drawn.",
    too_large: "This table is larger than the browser reads whole, and the server does not page it. The download is the complete original.",
  };

  onMount(() => {
    void (async () => {
      try {
        const { columns, types, rows } = await readRows();
        const number = /^-?(\d+\.?\d*|\.\d+)([eE][-+]?\d+)?$/;
        table = { columns, numeric: columns.map((_, c) => types ? ["integer", "number"].includes(types[c])
          : rows.slice(0, 200).every((r) => r.values[c] === "" || r.values[c] === "NA" || number.test(r.values[c]))) };
        const at = [x, y, z].map((name) => columns.indexOf(name));
        if (at.some((i) => i < 0)) { reason = `The method declared ${[x, y, z].filter((_, i) => at[i] < 0).join(" and ")}, which the table does not have.`; phase = "refused"; return; }
        for (const r of rows) stored.set(r.index, { x: r.values[at[0]], y: r.values[at[1]], z: r.values[at[2]], line: r.line });
        const built = buildGrid({ x: rows.map((r) => r.values[at[0]]), y: rows.map((r) => r.values[at[1]]), z: rows.map((r) => r.values[at[2]]), rows: rows.map((r) => r.index) });
        if (typeof built === "string") { reason = refusals[built]; phase = "refused"; return; }
        grid = built;
        built.rows.forEach((row, vertex) => vertexOf.set(row, vertex));
        const first = (values: number[], axis: "x" | "y") => [values[0], values.at(-1)!].map((v) => [...stored.values()].find((s) => Number(s[axis]) === v)?.[axis] ?? String(v)) as [string, string];
        facts = { points: built.rows.length, low: built.low, high: built.high, across: first(built.xs, "x"), deep: first(built.ys, "y"),
          gaps: [built.gaps.y.length ? `${y} ${built.gaps.y.join(", ")}` : "", built.gaps.x.length ? `${x} ${built.gaps.x.join(", ")}` : ""].filter(Boolean).join("; "),
          missing: built.missing };
        phase = "drawing";
        const { createScene } = await import("./scene.ts");
        if (controller.signal.aborted || !host) return;
        try {
          scene = await createScene(host, built, {
            ink: getComputedStyle(document.documentElement).getPropertyValue("--heading").trim() || "#022851",
            height, state: (ctx.state?.camera as CameraState | undefined) ?? null,
            onPick: (vertex) => {
              const row = built.rows[vertex], s = stored.get(row)!;
              const location: Location = { group: null, file: ctx.file.path, row, line: s.line, values: [[x, s.x], [y, s.y], [z, s.z]] };
              ctx.pick(location);
            },
            onMove: (camera) => ctx.keep({ camera, height }),
          });
          if (controller.signal.aborted) { scene.dispose(); scene = null; return; }
          backend = scene.backend;
          if (pending !== null) scene.mark(pending);
          phase = "ready";
        } catch (cause) {
          const busy = cause instanceof Error && cause.message === "gpu_budget";
          reason = ctx.still
            ? (busy ? "Two 3D views are already open in this window, so R's figure of the same numbers is shown here." : "This browser cannot draw the 3D view, so R's figure of the same numbers is shown.")
            : (busy ? "Two 3D views are already open in this window. Close one to draw this one; the values view shows the same numbers." : "This browser cannot draw the 3D view. The values view shows the same numbers.");
          phase = "still";
        }
      } catch (cause) {
        if (controller.signal.aborted) return;
        reason = refusals[cause instanceof Error ? cause.message : ""] ?? "The table could not be read for the 3D view. The values view shows it as stored.";
        phase = "refused";
      }
    })();
  });
  const stop = ctx.onPick((location) => {
    const vertex = location && location.file === ctx.file.path ? vertexOf.get(location.row) ?? null : null;
    pending = vertex;
    scene?.mark(vertex);
  });
  onDestroy(() => { stop(); controller.abort(); scene?.dispose(); scene = null; });

  function setHeight(next: number) {
    height = next;
    scene?.scale(next);
    if (scene) ctx.keep({ camera: scene.camera(), height });
  }
  function keys(event: KeyboardEvent) {
    if (scene?.key(event)) event.preventDefault();
  }
  // R draws the same surface from the view's angle and vertical scale (results rule 4) as a preview in the editor.
  function drawInR() {
    if (!scene || !ctx.figure) return;
    const angle = perspAngle(scene.camera(), height);
    ctx.figure.draw({ feature: "surface", controls: { x, y, z, theta: angle.theta, phi: angle.phi, expand: angle.expand }, ...table });
  }
  const ramp = `linear-gradient(to right, ${VIRIDIS.join(", ")})`;
</script>

<section class="surface" aria-label={`${ctx.title}, 3D view`}>
  {#if phase === "refused"}
    <p class="note">{reason}</p>
  {:else if phase === "still"}
    <p class="note">{reason}</p>
    {#if ctx.still}<FigureView file={ctx.still.file} load={(signal) => ctx.still!.read(signal)} title={ctx.title} onDownload={ctx.still.download} />{/if}
  {:else}
    <div class="stage">
      <!-- svelte-ignore a11y_no_noninteractive_tabindex, a11y_no_noninteractive_element_interactions -->
      <div class="canvas" bind:this={host} tabindex="0" role="application" aria-roledescription="3D view" onkeydown={keys}
        aria-label={`3D view of ${z} by ${x} and ${y}. Arrow keys turn it, plus and minus zoom, Shift with an arrow moves it, 0 resets.`}
        aria-describedby="surface-facts"></div>
      {#if phase !== "ready"}<p class="placeholder" role="status">{phase === "reading" ? "Reading the table…" : "Drawing…"}</p>{/if}
    </div>
    {#if facts}
      <div class="legend" id="surface-facts">
        <div class="scale-line">
          <span class="value">{facts.low}</span>
          <span class="ramp" style:background={ramp} aria-hidden="true"></span>
          <span class="value">{facts.high}</span>
          <span class="what">Height and colour: {z}</span>
        </div>
        <p>Across: {x}, {facts.across[0]} to {facts.across[1]}. Deep: {y}, {facts.deep[0]} to {facts.deep[1]}. {facts.points.toLocaleString()} stored points, each drawn once{facts.missing ? `; ${facts.missing.toLocaleString()} rows have no ${z}` : ""}.{facts.gaps ? ` No rows for ${facts.gaps}, so the surface has a gap there.` : ""}</p>
        <p class="printed">Vertical scale ×{height}{backend ? ` · drawn with ${backend}` : ""}</p>
      </div>
      <div class="controls">
        <div class="segmented" role="radiogroup" aria-label="Vertical scale">
          {#each SCALES as s (s)}<label><input type="radio" name={radios} checked={height === s} onchange={() => setHeight(s)} />×{s}</label>{/each}
        </div>
        <button class="link small" onclick={() => scene?.reset()} disabled={!scene}>Reset the view</button>
        {#if ctx.figure?.charts.includes("surface")}<button class="link small" onclick={drawInR} disabled={!scene}>Draw this angle in R</button>{/if}
        <p class="hint">Drag to turn, scroll or pinch to zoom, right-drag or two fingers to move; click a point to pick it.</p>
      </div>
    {/if}
  {/if}
</section>

<style>
  .surface { display: grid; gap: 10px; min-width: 0; }
  .stage { position: relative; }
  .canvas { width: 100%; aspect-ratio: 4 / 3; max-height: min(64vh, 620px); border-radius: var(--r-3); background: var(--plate); box-shadow: inset 0 0 0 1px var(--plate-edge); overflow: hidden; touch-action: none; }
  .canvas:focus-visible { outline: 2px solid var(--focus); outline-offset: 2px; }
  .placeholder { position: absolute; inset: 0; display: grid; place-items: center; font-size: var(--font-small); color: var(--text-3); pointer-events: none; }
  .legend { display: grid; gap: 4px; font-size: var(--font-small); color: var(--text-2); }
  .scale-line { display: flex; flex-wrap: wrap; align-items: center; gap: 6px 10px; font-variant-numeric: tabular-nums; }
  .ramp { width: 140px; height: 10px; border-radius: 5px; }
  .value { font-family: var(--mono); font-size: calc(12.5px * var(--type-scale)); color: var(--text); }
  .what { color: var(--text-2); }
  .printed { font-weight: 600; color: var(--heading); }
  .controls { display: flex; flex-wrap: wrap; align-items: center; gap: 8px 16px; }
  .controls .link { min-height: 36px; }
  .hint { flex: 1 1 30ch; font-size: var(--font-xs); color: var(--text-3); }
  .note { font-size: var(--font-small); color: var(--text-2); }
  @media (max-width: 640px) { .canvas { aspect-ratio: 1; } }
</style>
