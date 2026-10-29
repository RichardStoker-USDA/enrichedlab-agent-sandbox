<script lang="ts">
  import { packageHolds, queryOnly } from "./package_view.ts";
  import { verifiedMember } from "./api.ts";
  import type { PackageMember } from "./contracts.ts";
  import type { RetainedPackage } from "./evidence.ts";
  import FileView from "./FileView.svelte";
  import { bytes, fileTitle } from "./language.ts";
  import { fromMember, saveBlob, saveName } from "./viewer.ts";

  // How one analysis was made, read from its retained package: method notes, exact
  // parameters, code, input data, software environment and logs.
  // `onRecordPage` is true on the analysis page itself, where the Files section is below.
  // `highlight` marks the files a figure's note pointed to ("Show how this was made").
  let { taskId, pkg, onError, onRecordPage = false, highlight = [] }: {
    taskId: string; pkg: RetainedPackage; onError: (message: string) => void; onRecordPage?: boolean; highlight?: string[];
  } = $props();
  let open = $state<string>("");
  const view = $derived(pkg.view);
  const groups = $derived([
    { name: "Method notes", help: "What the reviewed method does and how it was configured.", items: view.notes },
    { name: "Parameters", help: "The exact values the run used, including any random seed.", items: view.parameters },
    queryOnly(view) ? { name: "Query", help: "The database query that ran, byte for byte.", items: view.code }
      : { name: "Code", help: "The R or Python code that ran, byte for byte.", items: view.code },
    { name: "Input data", help: "The retained copy of the data the analysis read.", items: view.inputs },
    { name: "Software environment", help: "Versions of R, packages and system libraries at run time.", items: view.environment },
    { name: "Logs", help: "What the program printed while it ran, and its exit status.", items: view.logs },
    { name: "Other files", help: "Everything else in the package.", items: view.other },
  ].filter((g) => g.items.length));
  // Open the first method note so the page answers "how" without a click.
  $effect(() => { if (!open && view.notes[0]) open = view.notes[0].path; });
  // Opening the code first answers "how was this figure made" directly.
  $effect(() => { const first = view.code.find((m) => highlight.includes(m.path)); if (first) open = first.path; });
  const load = (member: PackageMember) => (signal: AbortSignal) => verifiedMember(taskId, pkg.artifact, member, signal);
  async function save(member: PackageMember) {
    try { saveBlob(await verifiedMember(taskId, pkg.artifact, member), saveName(member.path)); }
    catch { onError("The file did not match its recorded checksum or is unavailable. Nothing was saved."); }
  }
</script>

<div class="method">
  {#each groups as group (group.name)}
    <section>
      <h4>{group.name}</h4>
      <p class="soft small">{group.help}</p>
      <ul>
        {#each group.items as item (item.path)}
          <li class:open={open === item.path} class:change-bar={highlight.includes(item.path)}>
            <button class="row" aria-expanded={open === item.path} onclick={() => (open = open === item.path ? "" : item.path)}>
              <span class="name">{fileTitle(item.path)}</span>
              <span class="path">{item.path}</span>
              <span class="size">{bytes(item.size_bytes)}</span>
              {#if item.manifest === "mismatched"}<span class="warn">Does not match its recorded checksum</span>{/if}
            </button>
            {#if open === item.path}
              <div class="viewer"><FileView file={fromMember(item, fileTitle(item.path))} load={load(item)} title={fileTitle(item.path)} onDownload={() => save(item)} showTitle={false} /></div>
            {/if}
          </li>
        {/each}
      </ul>
    </section>
  {/each}
  <p class="soft small">To rerun this analysis on your computer, {onRecordPage ? "download the analysis package from the Files tab" : "open its full record, download the analysis package from its Files tab"} and follow its README. The package holds {packageHolds(pkg.view)}.</p>
</div>

<style>
  .method { display: grid; gap: 22px; }
  section { display: grid; gap: 4px; }
  h4 { font-size: calc(15px * var(--type-scale)); }
  ul { list-style: none; margin: 6px 0 0; padding: 0; border-top: 1px solid var(--line); }
  li { border-bottom: 1px solid var(--line); scroll-margin-top: calc(var(--bar-h) + 24px); }
  .row { width: 100%; display: grid; grid-template-columns: minmax(0, 1fr) auto; gap: 2px 16px; padding: 10px 2px; text-align: left; align-items: baseline; }
  .row:hover { background: var(--hover-wash); }
  .name { font-weight: 600; color: var(--heading); }
  .path { grid-column: 1; grid-row: 2; font: var(--font-xs) var(--mono); color: var(--text-3); overflow-wrap: anywhere; }
  .size { grid-row: 1 / span 2; grid-column: 2; font-size: var(--font-xs); color: var(--text-3); align-self: center; font-variant-numeric: tabular-nums; }
  .warn { grid-column: 1 / -1; color: var(--failed); font-size: var(--font-small); font-weight: 600; }
  .viewer { padding: 6px 2px 18px; display: grid; gap: 10px; }
</style>
