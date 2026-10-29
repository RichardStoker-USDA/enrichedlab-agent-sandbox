<script module lang="ts">
  let companionSequence = 0;
</script>

<script lang="ts">
  import { onDestroy, tick } from "svelte";
  import { OKABE_ITO } from "./figure.ts";
  import { loadFigureData, type FigureDataChannel, type FigureDataSource, type FigureDataTable, type FigureDataPoint } from "./figure_data.ts";
  import { companionPalette, storedNumericTicks } from "./companion3d_math.ts";
  import { groupVisible, ScreenGridIndex, virtualWindow } from "./figure_companion_model.ts";
  import type { Companion3DScene } from "./companion3d.ts";

  let { source }: { source: FigureDataSource } = $props();
  const companionInstance = ++companionSequence;
  const facetOptionId = (index: number) => `figure-companion-${companionInstance}-facet-${index}`;
  const groupOptionId = (index: number) => `figure-companion-${companionInstance}-group-${index}`;

  let phase = $state<"loading" | "ready" | "refused">("loading");
  let reason = $state("");
  let table = $state<FigureDataTable | null>(null);
  let hiddenGroups = $state<Set<string>>(new Set());
  let facet = $state("");
  let active = $state<number | null>(null);
  let selected = $state<Set<number>>(new Set());
  let brushMode = $state(false);
  let brush = $state<{ x0: number; y0: number; x1: number; y1: number; add: boolean } | null>(null);
  let controller: AbortController | null = null;
  const CONTROL_ROW = 36, SELECTED_ROW = 34, OVERSCAN = 3;
  let facetOpen = $state(false), facetCursor = $state(0), facetTop = $state(0), facetHeight = $state(180);
  let groupCursor = $state(0), groupTop = $state(0), groupHeight = $state(144);
  let selectedTop = $state(0), selectedHeight = $state(220);
  let facetButton = $state<HTMLButtonElement>(), facetScroller = $state<HTMLElement>();
  let groupScroller = $state<HTMLElement>(), selectedScroller = $state<HTMLElement>();

  $effect(() => {
    const current = source;
    controller?.abort();
    const c = new AbortController();
    controller = c;
    phase = "loading"; reason = ""; table = null; hiddenGroups = new Set(); facet = ""; active = null; selected = new Set();
    facetOpen = false; facetCursor = 0; facetTop = 0; groupCursor = 0; groupTop = 0; selectedTop = 0;
    void (async () => {
      try {
        const { file, blob } = await current.load(c.signal);
        const result = await loadFigureData(current.record, file, blob);
        if (c.signal.aborted) return;
        if (!result.ok) { reason = result.message; phase = "refused"; return; }
        table = result.table;
        facet = result.table.encoding.facet_order[0] ?? "";
        phase = "ready";
      } catch {
        if (!c.signal.aborted) {
          reason = "The input file could not be checksum-verified and read, so nothing is drawn.";
          phase = "refused";
        }
      }
    })();
    return () => c.abort();
  });
  onDestroy(() => controller?.abort());

  const inFacet = $derived(table?.points.filter((point) => !table?.encoding.facet || point.facet === facet) ?? []);
  const visible = $derived(inFacet.filter((point) => groupVisible(point.group, hiddenGroups)));
  const hidden = $derived(inFacet.length - visible.length);
  const selectedRows = $derived(table ? [...selected].sort((a, b) => a - b).map((row) => table!.rows[row]).filter(Boolean) : []);
  const facetOrder = $derived(table?.encoding.facet_order ?? []);
  const groupOrder = $derived(table?.encoding.group_order ?? []);
  const facetWindow = $derived(virtualWindow(facetOrder.length, facetTop, facetHeight, CONTROL_ROW, OVERSCAN));
  const groupWindow = $derived(virtualWindow(groupOrder.length, groupTop, groupHeight, CONTROL_ROW, OVERSCAN));
  const selectedWindow = $derived(virtualWindow(selectedRows.length,
    Math.min(selectedTop, Math.max(0, selectedRows.length * SELECTED_ROW - selectedHeight)), selectedHeight, SELECTED_ROW, OVERSCAN));
  const shownFacets = $derived(facetOrder.slice(facetWindow.first, facetWindow.last)
    .map((value, offset) => ({ value, index: facetWindow.first + offset })));
  const shownGroups = $derived(groupOrder.slice(groupWindow.first, groupWindow.last)
    .map((value, offset) => ({ value, index: groupWindow.first + offset })));
  const shownSelectedRows = $derived(selectedRows.slice(selectedWindow.first, selectedWindow.last)
    .map((row, offset) => ({ row, index: selectedWindow.first + offset })));
  const currentRow = $derived(active === null || !table ? null : table.rows[active] ?? null);
  const is3d = $derived(table?.encoding.dimensions === 3);
  const summary = $derived(`${visible.length.toLocaleString()} ${visible.length === 1 ? "row" : "rows"} drawn, ${hidden.toLocaleString()} hidden by legend toggles, ${selected.size.toLocaleString()} selected.`);

  const groupPalette = OKABE_ITO.map((colour) => colour.hex);
  const safeColour = (value: string | undefined) => value && /^#[0-9a-f]{6}$/i.test(value) ? value : null;
  function coloursFor(value: FigureDataTable): Map<string, string> {
    const colours = new Map<string, string>();
    value.encoding.group_order.forEach((group, index) => colours.set(group,
      safeColour(value.encoding.group_colours[group]) ?? groupPalette[index % groupPalette.length]));
    if (!value.encoding.group) colours.set("", "#022851");
    return colours;
  }
  const colours = $derived(table ? coloursFor(table) : new Map<string, string>());
  const colourOf = (point: FigureDataPoint) => colours.get(point.group ?? "") ?? "#022851";
  const categoryLabel = (value: string) => value === "" ? "Empty value" : value;
  const titleOf = (channel: FigureDataChannel) => `${channel.label}${channel.unit ? ` (${channel.unit})` : ""}`;
  const shownChannels = $derived.by(() => {
    if (!table) return [];
    const channels = [table.encoding.x, table.encoding.y, table.encoding.z, table.encoding.group, table.encoding.facet]
      .filter((channel): channel is FigureDataChannel => !!channel);
    return channels.filter((channel, index) => channels.findIndex((other) => other.index === channel.index) === index);
  });
  const verticalScale = $derived.by(() => {
    if (!table?.encoding.z || !inFacet.length) return null;
    const choice = companionPalette(table.encoding.palette, table.encoding.palette_colours);
    let low = inFacet[0], high = inFacet[0];
    for (const point of inFacet) {
      if (point.z! < low.z!) low = point;
      if (point.z! > high.z!) high = point;
    }
    const index = table.encoding.z.index;
    const span = high.z! - low.z! || 1;
    return {
      ...choice,
      low: table.rows[low.row].cells[index],
      high: table.rows[high.row].cells[index],
      ticks: storedNumericTicks(inFacet.map((point) => ({
        value: point.z!, text: table!.rows[point.row].cells[index],
      }))).map((tick) => ({ ...tick, at: (tick.value - low.z!) / span })),
      gradient: `linear-gradient(to right, ${choice.stops.join(", ")})`,
    };
  });

  function toggleGroup(group: string) {
    const next = new Set(hiddenGroups);
    if (next.has(group)) next.delete(group); else next.add(group);
    hiddenGroups = next;
    if (active !== null && !inFacet.some((point) => point.row === active && groupVisible(point.group, next)))
      active = inFacet.find((point) => groupVisible(point.group, next))?.row ?? null;
  }
  function toggleSelected(row: number) {
    const next = new Set(selected);
    if (next.has(row)) next.delete(row); else next.add(row);
    selected = next;
  }
  function setRows(rows: number[], add: boolean) {
    selected = add ? new Set([...selected, ...rows]) : new Set(rows);
  }

  function reveal(scroller: HTMLElement | undefined, index: number, rowHeight: number, setTop: (top: number) => void) {
    if (!scroller) return;
    const rowTop = index * rowHeight, rowBottom = rowTop + rowHeight;
    if (rowTop < scroller.scrollTop) scroller.scrollTop = rowTop;
    else if (rowBottom > scroller.scrollTop + scroller.clientHeight) scroller.scrollTop = rowBottom - scroller.clientHeight;
    setTop(scroller.scrollTop);
  }
  async function openFacets() {
    facetOpen = !facetOpen;
    if (!facetOpen) return;
    facetCursor = Math.max(0, facetOrder.indexOf(facet));
    await tick();
    reveal(facetScroller, facetCursor, CONTROL_ROW, (top) => (facetTop = top));
    facetScroller?.focus();
  }
  function chooseFacet(index: number) {
    const value = facetOrder[index];
    if (value === undefined) return;
    facetCursor = index;
    facet = value;
    facetOpen = false;
    active = table?.points.find((point) => point.facet === value && groupVisible(point.group, hiddenGroups))?.row ?? null;
    void tick().then(() => facetButton?.focus());
  }
  function facetKeys(event: KeyboardEvent) {
    let next = facetCursor;
    if (event.key === "ArrowDown") next++;
    else if (event.key === "ArrowUp") next--;
    else if (event.key === "Home") next = 0;
    else if (event.key === "End") next = facetOrder.length - 1;
    else if (event.key === "Enter" || event.key === " ") { event.preventDefault(); chooseFacet(facetCursor); return; }
    else if (event.key === "Escape") {
      event.preventDefault(); facetOpen = false; void tick().then(() => facetButton?.focus()); return;
    }
    else return;
    event.preventDefault();
    facetCursor = Math.max(0, Math.min(facetOrder.length - 1, next));
    reveal(facetScroller, facetCursor, CONTROL_ROW, (top) => (facetTop = top));
  }
  function groupKeys(event: KeyboardEvent) {
    let next = groupCursor;
    if (event.key === "ArrowDown") next++;
    else if (event.key === "ArrowUp") next--;
    else if (event.key === "Home") next = 0;
    else if (event.key === "End") next = groupOrder.length - 1;
    else if (event.key === "Enter" || event.key === " ") {
      event.preventDefault();
      const group = groupOrder[groupCursor];
      if (group !== undefined) toggleGroup(group);
      return;
    } else return;
    event.preventDefault();
    groupCursor = Math.max(0, Math.min(groupOrder.length - 1, next));
    reveal(groupScroller, groupCursor, CONTROL_ROW, (top) => (groupTop = top));
  }
  function facetScrolled() {
    facetTop = facetScroller?.scrollTop ?? 0;
    const first = Math.floor(facetTop / CONTROL_ROW), last = Math.max(first, Math.ceil((facetTop + facetHeight) / CONTROL_ROW) - 1);
    if (facetCursor < first || facetCursor > last) facetCursor = Math.min(facetOrder.length - 1, first);
  }
  function groupScrolled() {
    groupTop = groupScroller?.scrollTop ?? 0;
    const first = Math.floor(groupTop / CONTROL_ROW), last = Math.max(first, Math.ceil((groupTop + groupHeight) / CONTROL_ROW) - 1);
    if (groupCursor < first || groupCursor > last) groupCursor = Math.min(groupOrder.length - 1, first);
  }

  // The 2D view is a bounded canvas so every accepted row can be drawn without thousands of
  // DOM nodes. Exact values live in the focus readout, retained-value ticks and selection table.
  let canvas = $state<HTMLCanvasElement>();
  let canvasWrap = $state<HTMLElement>();
  let resize2d: ResizeObserver | null = null;
  let theme2d: MutationObserver | null = null;
  let drawRequest = 0;
  const screen = new Map<number, { x: number; y: number }>();
  const hit2d = new ScreenGridIndex();
  let hit2dDirty = true;
  let base2d: HTMLCanvasElement | null = null;
  let base2dDirty = true;
  const plot = { left: 34, top: 18, right: 18, bottom: 28 };

  function draw2d() {
    drawRequest = 0;
    if (!canvas || !canvasWrap || !table || is3d) return;
    const box = canvasWrap.getBoundingClientRect();
    const ratio = Math.min(window.devicePixelRatio || 1, 2);
    if (Math.floor(box.width * ratio) < 1 || Math.floor(box.height * ratio) < 1) return;
    const width = box.width, height = box.height;
    const pixelWidth = Math.max(1, Math.floor(width * ratio)), pixelHeight = Math.max(1, Math.floor(height * ratio));
    if (canvas.width !== pixelWidth || canvas.height !== pixelHeight) {
      canvas.width = pixelWidth; canvas.height = pixelHeight;
      hit2dDirty = true; base2dDirty = true;
    }
    canvas.style.width = `${width}px`; canvas.style.height = `${height}px`;
    const ctx = canvas.getContext("2d");
    if (!ctx) return;
    const style = getComputedStyle(document.documentElement);
    const ink = style.getPropertyValue("--heading").trim() || "#022851";
    const line = style.getPropertyValue("--line-2").trim() || "#999999";
    const pw = Math.max(1, width - plot.left - plot.right), ph = Math.max(1, height - plot.top - plot.bottom);
    if (!base2d) { base2d = document.createElement("canvas"); base2dDirty = true; }
    if (base2d.width !== pixelWidth || base2d.height !== pixelHeight) {
      base2d.width = pixelWidth; base2d.height = pixelHeight; base2dDirty = true;
    }
    if (base2dDirty) {
      const base = base2d.getContext("2d");
      if (!base) return;
      base.setTransform(ratio, 0, 0, ratio, 0, 0);
      base.clearRect(0, 0, width, height);
      base.strokeStyle = line; base.lineWidth = 1;
      base.beginPath(); base.moveTo(plot.left, plot.top); base.lineTo(plot.left, plot.top + ph); base.lineTo(plot.left + pw, plot.top + ph); base.stroke();
      screen.clear();
      if (hit2dDirty) hit2d.clear();
      if (visible.length) {
        const xs = table.points.map((point) => point.x), ys = table.points.map((point) => point.y);
        const x0 = Math.min(...xs), x1 = Math.max(...xs), y0 = Math.min(...ys), y1 = Math.max(...ys);
        const sx = (value: number) => plot.left + ((value - x0) / (x1 - x0 || 1)) * pw;
        const sy = (value: number) => plot.top + ph - ((value - y0) / (y1 - y0 || 1)) * ph;
        for (const point of visible) {
          const x = sx(point.x), y = sy(point.y);
          screen.set(point.row, { x, y });
          if (hit2dDirty) hit2d.addPoint(point.row, x, y, 0, 11);
        }
        if (table.encoding.chart === "line") {
          const groups = table.encoding.group ? table.encoding.group_order : [""];
          const grouped = new Map<string, FigureDataPoint[]>();
          for (const point of visible) {
            const group = point.group ?? "", points = grouped.get(group) ?? [];
            points.push(point); grouped.set(group, points);
          }
          for (const group of groups) {
            const points = grouped.get(group) ?? [];
            if (!points.length) continue;
            base.beginPath();
            points.forEach((point, index) => { const at = screen.get(point.row)!; if (index) base.lineTo(at.x, at.y); else base.moveTo(at.x, at.y); });
            base.strokeStyle = colours.get(group) ?? ink; base.lineWidth = 1.5; base.stroke();
          }
        }
        for (const point of visible) {
          const at = screen.get(point.row)!;
          base.beginPath(); base.arc(at.x, at.y, selected.has(point.row) ? 4.5 : 3.2, 0, Math.PI * 2);
          base.fillStyle = colourOf(point); base.globalAlpha = hiddenGroups.size ? 0.92 : 0.84; base.fill(); base.globalAlpha = 1;
          if (selected.has(point.row)) { base.strokeStyle = ink; base.lineWidth = 1.5; base.stroke(); }
        }
      }
      hit2dDirty = false;
      base2dDirty = false;
    }
    ctx.setTransform(ratio, 0, 0, ratio, 0, 0);
    ctx.clearRect(0, 0, width, height);
    ctx.drawImage(base2d, 0, 0, width, height);
    if (active !== null && screen.has(active)) {
      const at = screen.get(active)!;
      ctx.beginPath(); ctx.arc(at.x, at.y, 7.5, 0, Math.PI * 2); ctx.strokeStyle = ink; ctx.lineWidth = 2; ctx.stroke();
    }
    if (brush) {
      const left = Math.min(brush.x0, brush.x1), top = Math.min(brush.y0, brush.y1);
      ctx.fillStyle = style.getPropertyValue("--selection").trim() || "rgb(2 40 81 / .08)";
      ctx.strokeStyle = ink; ctx.lineWidth = 1; ctx.setLineDash([4, 3]);
      ctx.fillRect(left, top, Math.abs(brush.x1 - brush.x0), Math.abs(brush.y1 - brush.y0));
      ctx.strokeRect(left, top, Math.abs(brush.x1 - brush.x0), Math.abs(brush.y1 - brush.y0)); ctx.setLineDash([]);
    }
  }
  const queue2d = () => { if (!drawRequest) drawRequest = requestAnimationFrame(draw2d); };
  $effect(() => {
    const wrap = canvasWrap;
    if (!wrap || is3d) return;
    resize2d?.disconnect(); resize2d = new ResizeObserver(() => { hit2dDirty = true; base2dDirty = true; queue2d(); }); resize2d.observe(wrap); queue2d();
    theme2d?.disconnect(); theme2d = new MutationObserver(() => { base2dDirty = true; queue2d(); });
    theme2d.observe(document.documentElement, { attributes: true, attributeFilter: ["class", "data-theme", "style"] });
    return () => { resize2d?.disconnect(); resize2d = null; theme2d?.disconnect(); theme2d = null; };
  });
  $effect(() => { void visible; hit2dDirty = true; base2dDirty = true; queue2d(); });
  $effect(() => { void selected; void colours; base2dDirty = true; queue2d(); });
  $effect(() => { void active; void brush; queue2d(); });
  onDestroy(() => { resize2d?.disconnect(); theme2d?.disconnect(); if (drawRequest) cancelAnimationFrame(drawRequest); base2d = null; });

  function local(event: PointerEvent, element: HTMLElement) {
    const box = element.getBoundingClientRect();
    return { x: event.clientX - box.left, y: event.clientY - box.top };
  }
  function nearest(event: PointerEvent): number | null {
    if (!canvas) return null;
    const point = local(event, canvas);
    return hit2d.hit(point.x, point.y);
  }
  function pointerDown2d(event: PointerEvent) {
    if (!canvas) return;
    canvasWrap?.focus();
    if (!brushMode) { const row = nearest(event); if (row !== null) { active = row; toggleSelected(row); } return; }
    const point = local(event, canvas);
    brush = { x0: point.x, y0: point.y, x1: point.x, y1: point.y, add: event.shiftKey };
    canvas.setPointerCapture(event.pointerId);
  }
  function pointerMove2d(event: PointerEvent) {
    if (!canvas) return;
    if (brush) { const point = local(event, canvas); brush = { ...brush, x1: point.x, y1: point.y }; }
    else if (!brushMode) active = nearest(event);
  }
  function pointerUp2d(event: PointerEvent) {
    if (!brush || !canvas) return;
    const done = brush;
    canvas.releasePointerCapture(event.pointerId);
    const left = Math.min(done.x0, done.x1), right = Math.max(done.x0, done.x1), top = Math.min(done.y0, done.y1), bottom = Math.max(done.y0, done.y1);
    const rows = hit2d.rowsIn({ left, right, top, bottom });
    setRows(rows, done.add); brush = null;
  }
  function focusPlot() { if (active === null || !visible.some((point) => point.row === active)) active = visible[0]?.row ?? null; }
  function keys2d(event: KeyboardEvent) {
    const rows = visible.map((point) => point.row), index = active === null ? -1 : rows.indexOf(active);
    const next = event.key === "ArrowRight" || event.key === "ArrowDown" ? rows[(index + 1 + rows.length) % rows.length]
      : event.key === "ArrowLeft" || event.key === "ArrowUp" ? rows[(index - 1 + rows.length) % rows.length]
      : event.key === "Home" ? rows[0] : event.key === "End" ? rows.at(-1) : undefined;
    if (next !== undefined) { event.preventDefault(); active = next; }
    else if ((event.key === "Enter" || event.key === " ") && active !== null) { event.preventDefault(); toggleSelected(active); }
  }

  // The 3D module and three.js enter the bundle only for a three-column mapping.
  let host3d = $state<HTMLElement>();
  let scene = $state.raw<Companion3DScene | null>(null);
  let backend = $state("");
  let gpuReason = $state("");
  let sceneMode = $state<"surface" | "points">("points");
  let surfaceFallback = $state<string | null>(null);
  $effect(() => {
    const host = host3d, value = table, panel = facet;
    if (!host || !value || value.encoding.dimensions !== 3) return;
    let live = true;
    const points = value.points.filter((point) => !value.encoding.facet || point.facet === panel)
      .map((point) => ({
        row: point.row, x: point.x, y: point.y, z: point.z!, group: point.group ?? "",
        xText: value.rows[point.row].cells[value.encoding.x.index],
        yText: value.rows[point.row].cells[value.encoding.y.index],
        zText: value.rows[point.row].cells[value.encoding.z!.index],
      }));
    const scale = companionPalette(value.encoding.palette, value.encoding.palette_colours);
    backend = ""; gpuReason = ""; surfaceFallback = null;
    sceneMode = value.encoding.geometry === "surface" ? "surface" : "points";
    void import("./companion3d.ts").then(async ({ createCompanion3D }) => {
      try {
        const made = await createCompanion3D(host, points, {
          surface: value.encoding.geometry === "surface",
          palette: scale.stops,
          theme: () => {
            const style = getComputedStyle(document.documentElement);
            return {
              ink: style.getPropertyValue("--heading").trim() || "#022851",
              grid: style.getPropertyValue("--line-2").trim() || "#8198B2",
            };
          },
          reducedMotion: matchMedia("(prefers-reduced-motion: reduce)").matches,
          onHover: (row) => { if (live) active = row; },
          onPick: (row) => { if (live) { active = row; toggleSelected(row); } },
        });
        if (!live) { made.dispose(); return; }
        scene = made; backend = made.backend; sceneMode = made.mode; surfaceFallback = made.fallback;
        made.setVisible(new Set(value.encoding.group ? value.encoding.group_order.filter((group) => !hiddenGroups.has(group)) : [""]));
        made.setSelected(selected); made.setActive(active); made.setBrushMode(brushMode);
      } catch (cause) {
        if (live) gpuReason = cause instanceof Error && cause.message === "gpu_budget"
          ? "Two interactive 3D views are already open. Close one to open this companion."
          : "This browser could not open the WebGPU or WebGL 2 view. The R figure remains available.";
      }
    });
    return () => { live = false; scene?.dispose(); scene = null; };
  });
  $effect(() => {
    const value = table, current = scene; void hiddenGroups;
    if (value && current) current.setVisible(new Set(value.encoding.group ? value.encoding.group_order.filter((group) => !hiddenGroups.has(group)) : [""]));
  });
  $effect(() => { scene?.setSelected(selected); });
  $effect(() => { scene?.setActive(active); });
  $effect(() => { scene?.setBrushMode(brushMode); });
  onDestroy(() => scene?.dispose());

  function keys3d(event: KeyboardEvent) {
    if (scene?.key(event)) { event.preventDefault(); return; }
    const rows = visible.map((point) => point.row), index = active === null ? -1 : rows.indexOf(active);
    const next = event.key === "PageDown" ? rows[(index + 1 + rows.length) % rows.length]
      : event.key === "PageUp" ? rows[(index - 1 + rows.length) % rows.length]
      : event.key === "Home" ? rows[0] : event.key === "End" ? rows.at(-1) : undefined;
    if (next !== undefined) { event.preventDefault(); active = next; }
    else if ((event.key === "Enter" || event.key === " ") && active !== null) { event.preventDefault(); toggleSelected(active); }
  }
  function pointerDown3d(event: PointerEvent) {
    if (!brushMode || !host3d) return;
    const point = local(event, host3d);
    brush = { x0: point.x, y0: point.y, x1: point.x, y1: point.y, add: event.shiftKey };
    (event.currentTarget as HTMLElement).setPointerCapture(event.pointerId);
  }
  function pointerMove3d(event: PointerEvent) {
    if (!brush || !host3d) return;
    const point = local(event, host3d); brush = { ...brush, x1: point.x, y1: point.y };
  }
  function pointerUp3d(event: PointerEvent) {
    if (!brush || !host3d || !scene) return;
    const done = brush;
    (event.currentTarget as HTMLElement).releasePointerCapture(event.pointerId);
    setRows(scene.brush({ left: Math.min(done.x0, done.x1), right: Math.max(done.x0, done.x1), top: Math.min(done.y0, done.y1), bottom: Math.max(done.y0, done.y1) }), done.add);
    brush = null;
  }
</script>

<section class="companion" aria-label="Interactive view of this figure's input data">
  <p class="master">Interactive view of this figure's input data. The R figure is the publication master.</p>
  {#if phase === "loading"}
    <p class="state" role="status">Checking and opening the figure's retained input…</p>
  {:else if phase === "refused"}
    <p class="state" role="status">{reason}</p>
  {:else if table}
    <p class="sr" role="status" aria-live="polite">{summary}</p>
    <div class="plot-controls">
      {#if table.encoding.facet && table.encoding.facet_order.length}
        <div class="facet">
          <span>{table.encoding.facet.label}</span>
          <div class="facet-choice">
            <button class="facet-button" bind:this={facetButton} aria-haspopup="listbox" aria-expanded={facetOpen} onclick={openFacets}>
              <span>{categoryLabel(facet)}</span><span class="control-count">{facetOrder.length.toLocaleString()}</span>
            </button>
            {#if facetOpen}
              <!-- svelte-ignore a11y_no_noninteractive_tabindex -->
              <div class="facet-popover">
                <p>{facetOrder.length.toLocaleString()} {facetOrder.length === 1 ? "facet" : "facets"}</p>
                <div class="choice-scroll" bind:this={facetScroller} bind:clientHeight={facetHeight} tabindex="0" role="listbox"
                  aria-label={`${table.encoding.facet.label}, ${facetOrder.length.toLocaleString()} stored values. Use arrow keys, then Enter.`}
                  aria-activedescendant={facetOptionId(facetCursor)} style:height={`${Math.min(facetWindow.totalHeight, 180)}px`}
                  onscroll={facetScrolled} onkeydown={facetKeys}>
                  <div class="choice-space" style:height={`${facetWindow.totalHeight}px`}>
                    {#each shownFacets as choice (choice.index)}
                      <button id={facetOptionId(choice.index)} class:current={choice.index === facetCursor} role="option" aria-selected={choice.value === facet} tabindex="-1"
                        style:transform={`translateY(${choice.index * CONTROL_ROW}px)`} onclick={() => chooseFacet(choice.index)}>{categoryLabel(choice.value)}</button>
                    {/each}
                  </div>
                </div>
              </div>
            {/if}
          </div>
        </div>
      {/if}
      <button class="button secondary compact" aria-pressed={brushMode} onclick={() => { brushMode = !brushMode; brush = null; }}>
        {brushMode ? "Brushing rows" : "Select with a brush"}
      </button>
      {#if selected.size}<button class="link small" onclick={() => { selected = new Set(); selectedTop = 0; }}>Clear selection</button>{/if}
      {#if is3d}
        <div class="camera" role="group" aria-label="3D view controls">
          <button onclick={() => scene?.zoom("out")} disabled={!scene} aria-label="Zoom out">−</button>
          <button onclick={() => scene?.zoom("in")} disabled={!scene} aria-label="Zoom in">+</button>
          <button onclick={() => scene?.reset()} disabled={!scene}>Reset</button>
        </div>
      {/if}
    </div>

    {#if table.encoding.group && table.encoding.group_order.length}
      <div class="legend">
        <p><strong>{table.encoding.group.label}</strong><span>{groupOrder.length.toLocaleString()} {groupOrder.length === 1 ? "group" : "groups"}, {hiddenGroups.size.toLocaleString()} hidden</span></p>
        <!-- svelte-ignore a11y_no_noninteractive_tabindex -->
        <!-- svelte-ignore a11y_no_noninteractive_element_interactions -->
        <div class="legend-scroll" bind:this={groupScroller} bind:clientHeight={groupHeight} tabindex="0" role="group"
          aria-label={`${table.encoding.group.label} legend, ${groupOrder.length.toLocaleString()} groups. Use arrow keys and Space to toggle.`}
          aria-activedescendant={groupOptionId(groupCursor)}
          style:height={`${Math.min(groupWindow.totalHeight, 144)}px`} onscroll={groupScrolled} onkeydown={groupKeys}
          onfocus={() => reveal(groupScroller, groupCursor, CONTROL_ROW, (top) => (groupTop = top))}>
          <div class="choice-space" style:height={`${groupWindow.totalHeight}px`}>
            {#each shownGroups as choice (choice.index)}
              <button id={groupOptionId(choice.index)} class="legend-row" class:current={choice.index === groupCursor} aria-pressed={!hiddenGroups.has(choice.value)} tabindex="-1"
                style:transform={`translateY(${choice.index * CONTROL_ROW}px)`} onclick={() => { groupCursor = choice.index; toggleGroup(choice.value); }}>
                <span class="swatch" style:background={is3d && verticalScale ? verticalScale.gradient : colours.get(choice.value)} aria-hidden="true"></span><span>{categoryLabel(choice.value)}</span>
              </button>
            {/each}
          </div>
        </div>
      </div>
    {/if}

    <div class="stage" class:brushing={brushMode}>
      {#if is3d}
        <!-- svelte-ignore a11y_no_noninteractive_tabindex, a11y_no_noninteractive_element_interactions -->
        <div class="plot three" bind:this={host3d} tabindex="0" role="application" aria-roledescription={sceneMode === "surface" ? "3D column surface" : "3D scatter plot"}
          aria-label={`${titleOf(table.encoding.z!)} by ${titleOf(table.encoding.x)} and ${titleOf(table.encoding.y)}. Drag or use arrow keys to turn; wheel, pinch, plus and minus zoom; Page Up and Page Down inspect rows; Space selects; 0 resets.`}
          onkeydown={keys3d} onfocus={focusPlot}></div>
        <!-- svelte-ignore a11y_no_static_element_interactions -->
        {#if brushMode}<div class="brush-layer" aria-hidden="true" onpointerdown={pointerDown3d} onpointermove={pointerMove3d} onpointerup={pointerUp3d}></div>{/if}
        {#if brush}<span class="brush-box" style:left={`${Math.min(brush.x0, brush.x1)}px`} style:top={`${Math.min(brush.y0, brush.y1)}px`}
          style:width={`${Math.abs(brush.x1 - brush.x0)}px`} style:height={`${Math.abs(brush.y1 - brush.y0)}px`}></span>{/if}
        {#if !scene && !gpuReason}<p class="plot-state" role="status">Opening the 3D view…</p>{/if}
        {#if gpuReason}<p class="plot-state" role="status">{gpuReason}</p>{/if}
      {:else}
        <!-- svelte-ignore a11y_no_noninteractive_tabindex, a11y_no_noninteractive_element_interactions -->
        <div class="plot two" bind:this={canvasWrap} tabindex="0" role="application" aria-roledescription={`${table.encoding.chart} plot`}
          aria-label={`${titleOf(table.encoding.y)} by ${titleOf(table.encoding.x)}. Focus and use arrow keys to inspect rows; Space selects a row.`}
          onfocus={focusPlot} onkeydown={keys2d}>
          <canvas bind:this={canvas} aria-hidden="true"
            onpointerdown={pointerDown2d} onpointermove={pointerMove2d} onpointerup={pointerUp2d}
            onpointerleave={() => { if (!canvasWrap?.matches(":focus")) active = null; }}></canvas>
        </div>
      {/if}
    </div>
    {#if is3d && verticalScale && table.encoding.z}
      <div class="colour-scale" role="img" aria-label={`${titleOf(table.encoding.z)} colour scale. Exact range ${verticalScale.low} to ${verticalScale.high}. Stored-value ticks ${verticalScale.ticks.map((tick) => tick.text).join(", ")}.`}>
        <span class="colour-scale-title" aria-hidden="true">{titleOf(table.encoding.z)}</span>
        <span class="colour-ramp-wrap" aria-hidden="true">
          <span class="colour-ramp" style:background={verticalScale.gradient}></span>
          <span class="colour-ticks">{#each verticalScale.ticks as tick (tick.value)}<span style:left={`${tick.at * 100}%`}>{tick.text}</span>{/each}</span>
        </span>
      </div>
    {/if}
    <div class="axis-names" aria-label="Axes">
      <span><strong>Across</strong> {titleOf(table.encoding.x)}</span>
      <span><strong>{is3d ? "Deep" : "Up"}</strong> {titleOf(table.encoding.y)}</span>
      {#if table.encoding.z}<span><strong>Up</strong> {titleOf(table.encoding.z)}</span>{/if}
      {#if backend}<span class="backend">{backend}</span>{/if}
    </div>
    {#if is3d && table.encoding.geometry === "surface" && scene}
      <p class="geometry-note">{surfaceFallback
        ? "The requested surface is not one regular x-by-y grid, so its stored rows are shown as points."
        : "Each column is one stored value above the scale minimum. Missing grid cells stay at the empty floor; no value is interpolated."}</p>
    {/if}
    <p class="counts">{summary}{#if table.omitted_coordinate_rows} {table.omitted_coordinate_rows.toLocaleString()} {table.omitted_coordinate_rows === 1 ? "row has" : "rows have"} a missing mapped coordinate and cannot be placed.{/if}</p>
    <p class="hint">{brushMode ? "Drag across the plot to select rows; hold Shift to add them." : is3d ? "Drag or use arrow keys to turn. Wheel, pinch or +/− zooms. Page Up and Page Down inspect rows; Space or a click selects one." : "Hover or focus the plot to inspect rows. Click a point to select it."}</p>

    {#if currentRow}
      <div class="readout" id="companion-row" aria-live="polite">
        <strong>Row {currentRow.number.toLocaleString()}</strong>
        <dl>{#each shownChannels as channel (channel.index)}<div><dt>{channel.label}</dt><dd>{currentRow.cells[channel.index]}</dd></div>{/each}</dl>
        <button class="link small" onclick={() => toggleSelected(currentRow.index)}>{selected.has(currentRow.index) ? "Remove row from selection" : "Add row to selection"}</button>
      </div>
    {/if}

    {#if selectedRows.length}
      <section class="selection" aria-labelledby="selected-rows-title">
        <div class="selection-head"><h3 id="selected-rows-title">Selected rows</h3><span>{selectedRows.length.toLocaleString()}</span></div>
        <div class="table-wrap" bind:this={selectedScroller} bind:clientHeight={selectedHeight} onscroll={() => (selectedTop = selectedScroller?.scrollTop ?? 0)}>
          <table aria-rowcount={selectedRows.length + 1}>
            <thead><tr><th scope="col">Row</th>{#each shownChannels as channel (channel.index)}<th scope="col">{channel.label}</th>{/each}</tr></thead>
            <tbody>
              {#if selectedWindow.offset}<tr class="table-space" aria-hidden="true" style:height={`${selectedWindow.offset}px`}><td colspan={shownChannels.length + 1}></td></tr>{/if}
              {#each shownSelectedRows as item (item.row.index)}<tr aria-rowindex={item.index + 2}><th scope="row">{item.row.number}</th>{#each shownChannels as channel (channel.index)}<td>{item.row.cells[channel.index]}</td>{/each}</tr>{/each}
              {#if selectedWindow.last < selectedRows.length}<tr class="table-space" aria-hidden="true" style:height={`${(selectedRows.length - selectedWindow.last) * SELECTED_ROW}px`}><td colspan={shownChannels.length + 1}></td></tr>{/if}
            </tbody>
          </table>
        </div>
      </section>
    {/if}
  {/if}
</section>

<style>
  .companion { display: grid; align-content: start; gap: 10px; min-width: 0; }
  .master { padding-bottom: 8px; border-bottom: 1px solid var(--line); color: var(--heading); font-family: var(--serif); font-size: var(--font-small); line-height: 1.45; }
  .state { min-height: 180px; display: grid; place-items: center; padding: 24px; color: var(--text-2); font-size: var(--font-small); text-align: center; }
  .plot-controls { display: flex; flex-wrap: wrap; align-items: center; gap: 7px 12px; }
  .facet { display: flex; align-items: center; gap: 7px; font-size: var(--font-small); font-weight: 650; color: var(--heading); }
  .facet-choice { position: relative; min-width: 10rem; max-width: min(18rem, 70vw); }
  .facet-button { display: flex; align-items: center; justify-content: space-between; gap: 12px; width: 100%; min-height: 36px; padding: 0 9px; border: 1px solid var(--line-2); border-radius: var(--r-2); background: var(--surface); color: var(--text); font-weight: 500; }
  .facet-button > span:first-child { min-width: 0; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
  .control-count { color: var(--text-3); font-size: var(--font-xs); font-variant-numeric: tabular-nums; }
  .facet-popover { position: absolute; z-index: 5; top: calc(100% + 5px); left: 0; width: min(18rem, 78vw); padding: 6px; border: 1px solid var(--line-2); border-radius: var(--r-2); background: var(--surface); box-shadow: var(--overlay-shadow); }
  .facet-popover > p { padding: 2px 5px 5px; color: var(--text-3); font-size: var(--font-xs); font-weight: 500; }
  .choice-scroll, .legend-scroll { position: relative; overflow: auto; overscroll-behavior: contain; }
  .choice-scroll { height: 180px; }
  .choice-scroll:focus-visible, .legend-scroll:focus-visible { outline: 2px solid var(--focus); outline-offset: -2px; }
  .choice-space { position: relative; }
  .choice-space > button { position: absolute; top: 0; left: 0; width: 100%; height: 36px; min-width: 0; padding: 0 7px; overflow: hidden; color: var(--text); font-size: var(--font-small); text-align: left; text-overflow: ellipsis; white-space: nowrap; }
  .choice-space > button:hover, .choice-space > button.current { background: var(--row-hover); }
  .camera { display: flex; margin-left: auto; border: 1px solid var(--line); border-radius: var(--r-2); overflow: hidden; }
  .camera button { min-width: 38px; min-height: 36px; padding: 0 10px; color: var(--heading); font-weight: 650; border-left: 1px solid var(--line); }
  .camera button:first-child { border-left: 0; }
  .camera button:hover:not(:disabled) { background: var(--hover-wash); }
  .legend { display: grid; gap: 4px; max-width: 24rem; }
  .legend > p { display: flex; justify-content: space-between; gap: 12px; color: var(--heading); font-size: var(--font-small); }
  .legend > p span { color: var(--text-3); font-size: var(--font-xs); font-weight: 400; font-variant-numeric: tabular-nums; }
  .legend-scroll { min-height: 36px; max-height: 144px; border-block: 1px solid var(--line); }
  .choice-space > .legend-row { display: flex; align-items: center; gap: 6px; }
  .legend-row[aria-pressed="false"] { color: var(--text-3); text-decoration: line-through; }
  .swatch { width: 10px; height: 10px; flex: none; border: 1px solid rgb(0 0 0 / .28); }
  .legend-row[aria-pressed="false"] .swatch { background: transparent !important; border-color: var(--line-2); }
  .stage { position: relative; min-width: 0; }
  .plot { position: relative; width: 100%; aspect-ratio: 4 / 3; min-height: 260px; max-height: min(64vh, 620px); overflow: hidden; background: var(--plate); box-shadow: inset 0 0 0 1px var(--plate-edge); border-radius: var(--r-1); }
  .plot canvas { display: block; width: 100%; height: 100%; touch-action: none; cursor: crosshair; }
  .plot.three { touch-action: none; }
  :global(.companion-axis-ticks) { position: absolute; inset: 0; z-index: 2; overflow: hidden; pointer-events: none; }
  :global(.companion-axis-tick) { position: absolute; max-width: 7rem; color: var(--text-2); font-family: var(--mono); font-size: 10px; line-height: 1.25; white-space: nowrap; transform: translate(-50%, -50%); }
  .plot:focus-visible, .plot canvas:focus-visible { outline: 2px solid var(--focus); outline-offset: 2px; }
  .brushing .plot, .brushing .plot canvas, .brush-layer { cursor: crosshair; }
  .brush-layer { position: absolute; inset: 0; touch-action: none; }
  .brush-box { position: absolute; border: 1px dashed var(--mark); background: var(--track); pointer-events: none; }
  .plot-state { position: absolute; inset: 0; display: grid; place-items: center; padding: 24px; color: var(--text-2); font-size: var(--font-small); text-align: center; pointer-events: none; }
  .axis-names { display: flex; flex-wrap: wrap; gap: 4px 18px; font-size: var(--font-xs); color: var(--text-2); }
  .axis-names strong { color: var(--heading); margin-right: 3px; }
  .backend { margin-left: auto; font-family: var(--mono); color: var(--text-3); }
  .colour-scale { display: grid; grid-template-columns: minmax(8rem, 18rem); gap: 4px; width: min(100%, 18rem); color: var(--text-2); font-size: var(--font-xs); }
  .colour-scale-title { color: var(--heading); font-weight: 650; }
  .colour-ramp-wrap { position: relative; display: grid; padding-bottom: 17px; }
  .colour-ramp { height: 11px; border: 1px solid var(--line-2); }
  .colour-ticks { position: absolute; inset: 11px 0 auto; height: 16px; font-family: var(--mono); color: var(--text-2); font-variant-numeric: tabular-nums; }
  .colour-ticks > span { position: absolute; padding-top: 4px; font-size: 10px; line-height: 1; transform: translateX(-50%); }
  .colour-ticks > span::before { content: ""; position: absolute; top: 0; left: 50%; height: 3px; border-left: 1px solid var(--line-2); }
  .counts, .hint, .geometry-note { font-size: var(--font-xs); line-height: 1.45; color: var(--text-2); }
  .geometry-note { color: var(--text-3); }
  .hint { color: var(--text-3); }
  .readout { display: grid; gap: 6px; padding-top: 9px; border-top: 1px solid var(--line); }
  .readout > strong { color: var(--heading); font-size: var(--font-small); }
  .readout dl { display: flex; flex-wrap: wrap; gap: 6px 18px; margin: 0; }
  .readout dl div { display: flex; gap: 5px; min-width: 0; }
  .readout dt { color: var(--text-2); font-size: var(--font-xs); }
  .readout dd { margin: 0; color: var(--text); font-family: var(--mono); font-size: var(--font-xs); overflow-wrap: anywhere; }
  .readout .link { justify-self: start; }
  .selection { display: grid; gap: 6px; margin-top: 3px; }
  .selection-head { display: flex; align-items: baseline; justify-content: space-between; gap: 12px; }
  .selection h3 { color: var(--heading); font-size: calc(15px * var(--type-scale)); }
  .selection-head span { color: var(--text-3); font-size: var(--font-xs); font-variant-numeric: tabular-nums; }
  .table-wrap { max-height: 220px; overflow: auto; border-top: 2px solid var(--heading); border-bottom: 1px solid var(--heading); }
  table { width: 100%; border-collapse: collapse; font-size: var(--font-xs); font-variant-numeric: tabular-nums; }
  th, td { padding: 6px 9px; border-bottom: 1px solid var(--line); text-align: left; white-space: nowrap; }
  thead th { position: sticky; top: 0; background: var(--surface); color: var(--heading); font-weight: 700; }
  tbody tr:not(.table-space) { height: 34px; }
  .table-space td { height: inherit; padding: 0; border: 0; }
  tbody th { color: var(--text-2); font-weight: 600; }
  td { font-family: var(--mono); color: var(--text); }
  .sr { position: absolute; width: 1px; height: 1px; padding: 0; margin: -1px; overflow: hidden; clip: rect(0, 0, 0, 0); white-space: nowrap; border: 0; }
  button:focus-visible { outline: 2px solid var(--focus); outline-offset: 2px; }
  @media (max-width: 640px) {
    .plot { aspect-ratio: 1; min-height: 250px; }
    .camera { margin-left: 0; }
    .axis-names { display: grid; }
    .backend { margin-left: 0; }
  }
  @media (prefers-reduced-motion: reduce) { * { scroll-behavior: auto; transition: none !important; } }
</style>
