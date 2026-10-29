<script lang="ts">
  import { verifiedMember } from "./api.ts";
  import FigureView from "./FigureView.svelte";
  import TableView from "./TableView.svelte";
  import type { RetainedPackage } from "./evidence.ts";
  import { fileTitle } from "./language.ts";
  import { integritySentence } from "./package_view.ts";
  import { fromMember, saveBlob, saveName } from "./viewer.ts";
  import type { PackageMember } from "./contracts.ts";
  import type { FigureDataSource } from "./figure_data.ts";

  // The actual figures and tables from one analysis's retained package, numbered through
  // the page like a paper, with the plain statement of how the files were checked.
  // Nothing here is drawn from model text.
  // `writer` says who wrote a template package's code, from the broker's admitted record; until it
  // is read, figures carry no note rather than a guess.
  // `onFigure`, when given, offers a figure drawn by R from each table. A figure version's package holds
  // one figure, named by `figureTitle`; `writer` "render" says the render service drew it.
  // `onAnalyze` offers a reviewed R analysis of each table. `leaveOut` names files the page shows another way, such
  // as a template analysis's headline statistics, and `tableTitles` names tables whose file name says little.
  let { taskId, pkg, onError, figureStart = 1, tableStart = 1, onShowMethod, writer = null, onFigure, onAnalyze, figureTitle = "",
    leaveOut = [], tableTitles = {}, companion = null, exploreOpen, onExploreChange }: {
    taskId: string; pkg: RetainedPackage; onError: (message: string) => void;
    figureStart?: number; tableStart?: number; onShowMethod?: (paths: string[]) => void;
    writer?: "model" | "configured" | "render" | "template" | null; figureTitle?: string;
    onFigure?: (table: PackageMember, read: { columns: string[]; numeric: boolean[] }) => void;
    onAnalyze?: (table: PackageMember, read: { columns: string[]; numeric: boolean[] }) => void;
    leaveOut?: string[]; tableTitles?: Record<string, string>;
    companion?: FigureDataSource | null; exploreOpen?: boolean; onExploreChange?: (open: boolean) => void;
  } = $props();
  const view = $derived(pkg.view);
  const tables = $derived(view.tables.filter((t) => !leaveOut.includes(t.path)));
  const made = $derived({ code: view.code.map((m) => m.path), inputs: view.inputs.map((m) => m.path),
    writer: view.template || writer === "render" || writer === "template" ? writer : null });
  const load = (member: PackageMember) => (signal: AbortSignal) => verifiedMember(taskId, pkg.artifact, member, signal);
  // A reviewed method can bind its retained R figure and input table as the same values, with the
  // columns it drew. That recorded binding is the figure record for an ordinary analysis; a figure
  // version's render request, supplied by the page, takes precedence.
  function declaredCompanion(figure: PackageMember): FigureDataSource | null {
    if (!figure.same_values_group) return null;
    const table = pkg.contents.listing.members.find((member) => member.same_values_group === figure.same_values_group
      && member !== figure && /\.(csv|tsv|tab|json)$/i.test(member.path));
    const declared = table?.views?.find((entry) => entry.view === "surface" || entry.view === "companion");
    if (!table || !declared?.encoding?.x || !declared.encoding.y) return null;
    const record = { feature: declared.encoding.z ? "surface" : declared.encoding.chart === "line" ? "line" : "scatter",
      controls: declared.encoding };
    return { record, load: async (signal) => ({
      file: { path: table.path, media_type: table.media_type, size_bytes: table.size_bytes, sha256: table.sha256 },
      blob: await verifiedMember(taskId, pkg.artifact, table, signal),
    }) };
  }
  async function save(member: PackageMember) {
    try { saveBlob(await verifiedMember(taskId, pkg.artifact, member), saveName(member.path)); }
    catch { onError("The file did not match its recorded checksum or is unavailable. Nothing was saved."); }
  }
</script>

<div class="results">
  {#each view.figures as figure, i (figure.display.path)}
    {@const title = figureTitle && view.figures.length === 1 ? figureTitle : figure.title}
    {@const packageBound = !companion?.figure_package_sha256
      || companion.figure_package_sha256.replace(/^sha256:/, "") === pkg.artifact.sha256.replace(/^sha256:/, "")}
    {@const explicitlyBound = companion && packageBound && (!companion.figure_paths?.length || companion.figure_paths.includes(figure.display.path)
      || (!!figure.original && companion.figure_paths.includes(figure.original.path))) ? companion : null}
    {@const dataSource = explicitlyBound ?? declaredCompanion(figure.display)}
    <FigureView number={figureStart + i} file={fromMember(figure.display, title)} load={load(figure.display)} {title}
      onDownload={() => save(figure.display)} original={figure.original ? fromMember(figure.original, title) : null}
      onOriginal={figure.original ? () => save(figure.original!) : undefined}
      {made} onShowMethod={onShowMethod ? () => onShowMethod([...made.code, ...made.inputs]) : undefined}
      companion={dataSource} exploreOpen={explicitlyBound ? exploreOpen : undefined}
      onExploreChange={explicitlyBound ? onExploreChange : undefined} />
  {/each}
  {#each tables as table, i (table.path)}
    {@const title = tableTitles[table.path] ?? fileTitle(table.path)}
    <TableView number={tableStart + i} file={fromMember(table, title)} load={load(table)} {title} onDownload={() => save(table)}
      onFigure={onFigure ? (read) => onFigure(table, read) : undefined} onAnalyze={onAnalyze ? (read) => onAnalyze(table, read) : undefined} />
  {/each}
  {#if !view.figures.length && !tables.length}
    <p class="soft">This package has no figures or tables. Its files are listed under How it was done.</p>
  {/if}
  <p class="integrity small">
    <svg viewBox="0 0 16 16" aria-hidden="true"><path d="M3 8.4l3.2 3.2L13 4.6" /></svg>
    {integritySentence(view.integrity)}
  </p>
</div>

<style>
  .results { display: grid; gap: 34px; }
  .integrity { display: flex; align-items: center; gap: 8px; color: var(--text-2); }
  .integrity svg { width: 16px; height: 16px; flex: none; fill: none; stroke: var(--heading); stroke-width: 2; stroke-linecap: round; stroke-linejoin: round; }
</style>
