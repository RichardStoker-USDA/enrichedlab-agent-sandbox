<script lang="ts">
  import { onDestroy, untrack } from "svelte";
  import { broker } from "./api.ts";
  import type { FigureCatalog, FigureVersion, FigureVersionSummary } from "./contracts.ts";
  import { figureDrawing } from "./evidence.ts";
  import { settingsDiff } from "./figure.ts";
  import { when } from "./language.ts";

  // Two versions of one figure side by side (Glass Box results rule 4): both drawings as R made them, checked against
  // their recorded checksums, and the settings in one table with each difference marked. The page's own version is
  // on the left; the reader chooses the other from the figure's drawn versions.
  let { current, versions, catalog, numberOf }: {
    current: FigureVersion; versions: FigureVersionSummary[]; catalog: FigureCatalog | null; numberOf: (task: string) => number;
  } = $props();
  const uid = $props.id();
  const others = $derived(versions.filter((v) => v.task_id !== current.task_id && v.status === "completed"));
  // The version made just before this one, else the newest other.
  let chosen = $state("");
  $effect(() => {
    if (others.some((v) => v.task_id === chosen)) return;
    const older = others.filter((v) => v.created_at < current.created_at);
    chosen = (older[0] ?? others[0])?.task_id ?? "";
  });
  let other = $state<FigureVersion | null>(null);
  let drawings = $state<Record<string, { url: string } | "loading" | "failed" | "none">>({});
  let onlyDiffers = $state(false);
  let controller: AbortController | null = null;

  async function draw(task: string, signal: AbortSignal) {
    if (drawings[task] && drawings[task] !== "failed") return;
    drawings = { ...drawings, [task]: "loading" };
    try {
      const blob = await figureDrawing(task, signal);
      if (!signal.aborted) drawings = { ...drawings, [task]: blob ? { url: URL.createObjectURL(blob) } : "none" };
    } catch { if (!signal.aborted) drawings = { ...drawings, [task]: "failed" }; }
  }
  // Only the chosen version starts a read; the drawings it records do not.
  $effect(() => {
    const task = chosen, own = current.task_id;
    if (!task) return;
    untrack(() => {
      controller?.abort();
      const c = new AbortController();
      controller = c;
      other = null;
      void draw(own, c.signal);
      void draw(task, c.signal);
      void broker.taskFigure(task, c.signal).then((v) => { if (!c.signal.aborted) other = v; }).catch(() => {});
    });
  });
  const rows = $derived(other ? settingsDiff(catalog, current, other) : []);
  const differ = $derived(rows.filter((r) => r.differs));
  const service = $derived.by(() => {
    const a = current.receipts.at(-1)?.service_version, b = other?.receipts.at(-1)?.service_version;
    return a && b && a !== b ? { a, b } : null;
  });
  const listed = (labels: string[]) => labels.length < 3 ? labels.join(" and ") : `${labels.slice(0, -1).join(", ")} and ${labels.at(-1)}`;

  onDestroy(() => {
    controller?.abort();
    for (const d of Object.values(drawings)) if (typeof d === "object") URL.revokeObjectURL(d.url);
  });
</script>

{#snippet panel(task: string, label: string)}
  {@const d = drawings[task]}
  <figure class="side">
    <figcaption><strong>{label}</strong></figcaption>
    <div class="paper">
      {#if typeof d === "object"}<img src={d.url} alt={`${label}, drawn by R`} />
      {:else if d === "failed"}<p>This drawing could not be checked and shown just now.</p>
      {:else if d === "none"}<p>This version holds no drawing.</p>
      {:else}<p role="status">Opening the drawing…</p>{/if}
    </div>
  </figure>
{/snippet}

<section class="compare" aria-labelledby={`${uid}-title`}>
  <div class="head">
    <h3 id={`${uid}-title`}>Two versions side by side</h3>
    {#if others.length > 1}
      <label class="pick"><span>Compare with</span>
        <select bind:value={chosen}>
          {#each others as v (v.task_id)}<option value={v.task_id}>Version {numberOf(v.task_id)}, {when(v.created_at)}</option>{/each}
        </select></label>
    {/if}
  </div>
  {#if !chosen}
    <p class="soft small">No other version of this figure was drawn.</p>
  {:else}
    {@const n = numberOf(current.task_id)}
    {@const m = numberOf(chosen)}
    <div class="sides">
      {@render panel(current.task_id, `Version ${n}, this page`)}
      {@render panel(chosen, `Version ${m}`)}
    </div>
    {#if other}
      <p class="sum" role="status">{differ.length ? `${differ.length === 1 ? "One setting differs" : `${differ.length} settings differ`}: ${listed(differ.map((r) => r.label))}.`
        : "The two versions were drawn with the same settings."}{service ? ` They were drawn by different releases of the render service (${service.a} and ${service.b}).` : ""}</p>
      <label class="only"><input type="checkbox" bind:checked={onlyDiffers} disabled={!differ.length} />Only the settings that differ</label>
      <div class="table-wrap">
        <table>
          <thead><tr><th scope="col">Setting</th><th scope="col">Version {n}</th><th scope="col">Version {m}</th></tr></thead>
          <tbody>
            {#each rows.filter((r) => r.differs || !onlyDiffers) as row (row.label)}
              <tr class:differs={row.differs}><th scope="row">{#if row.differs}<span class="visually-hidden">Differs: </span>{/if}{row.label}</th><td>{row.a}</td><td>{row.b}</td></tr>
            {/each}
          </tbody>
        </table>
      </div>
    {:else}
      <p class="soft small" role="status">Reading version {m}'s settings…</p>
    {/if}
  {/if}
</section>

<style>
  .compare { display: grid; gap: 14px; }
  .head { display: flex; flex-wrap: wrap; align-items: center; justify-content: space-between; gap: 8px 18px; }
  h3 { font-size: calc(15px * var(--type-scale)); color: var(--heading); }
  .pick { display: inline-flex; align-items: center; gap: 8px; font-size: var(--font-small); color: var(--text-2); white-space: nowrap; }
  .pick select { min-height: 40px; max-width: 100%; }
  .sides { display: grid; grid-template-columns: repeat(auto-fit, minmax(min(100%, 280px), 1fr)); gap: 16px; }
  .side { margin: 0; display: grid; gap: 6px; align-content: start; min-width: 0; }
  .side figcaption { font-size: var(--font-small); color: var(--heading); }
  /* R draws for white paper, so both drawings sit on white, whatever the theme. */
  .paper { background: var(--paper); box-shadow: 0 0 0 1px var(--plate-edge); min-height: 200px; display: grid; place-items: center; padding: 12px; }
  .paper img { display: block; max-width: 100%; height: auto; }
  .paper p { color: #355B85; font-size: var(--font-small); text-align: center; max-width: 36ch; }
  .sum { max-width: 72ch; }
  .only { display: inline-flex; align-items: center; gap: 8px; min-height: 40px; font-size: var(--font-small); cursor: pointer; justify-self: start; }
  .table-wrap { overflow-x: auto; }
  table { width: 100%; border-collapse: collapse; font-size: var(--font-small); border-top: 2px solid var(--heading); }
  th, td { text-align: left; vertical-align: top; padding: 8px 12px 8px 0; border-bottom: 1px solid var(--line); overflow-wrap: anywhere; }
  thead th { color: var(--text-2); font-weight: 600; }
  tbody th { font-weight: 700; color: var(--heading); padding-left: 12px; position: relative; min-width: 9em; }
  tr.differs th::before { content: ""; position: absolute; left: 0; top: 6px; bottom: 6px; width: 3px; background: var(--mark); }
  tr.differs td { color: var(--heading); font-weight: 600; }
</style>
