<script lang="ts">
  import { onDestroy, untrack } from "svelte";
  import type { Identity, LabTool, LabTools, ReviewedQuery } from "./contracts.ts";
  import { currentLab } from "./lab.svelte.ts";
  import { day } from "./lab_notes.ts";
  import { covers, live } from "./live.ts";
  import { href } from "./route.ts";
  import { grantWords, kindName, kindSentence, labTools, parameterWords, sectionKey, shortDigest, toolSections, toolsClaim, toolsFailure } from "./tools.ts";

  // The lab's tools: what the lab may run, grouped by the tool groups it holds, and for each method what
  // it reads, its settings, the reviewed queries it offers and analyses a person accepted. The broker decides each grant.
  let { identity }: { identity: Identity } = $props();

  const group = $derived(currentLab(identity));
  let view = $state<LabTools | null>(null);
  let loading = $state(true);
  let failure = $state("");
  let controller: AbortController | null = null;

  async function load(quiet = false) {
    const key = group?.key;
    if (!key) { loading = false; return; }
    controller?.abort();
    const c = new AbortController();
    controller = c;
    loading = true; if (!quiet) failure = "";
    try {
      const next = await labTools.list(key, c.signal);
      if (!c.signal.aborted) { view = next; failure = ""; }
    } catch (cause) {
      if (!c.signal.aborted && !(quiet && view)) failure = toolsFailure(cause);
    } finally {
      if (!c.signal.aborted) loading = false;
    }
  }
  $effect(() => { void group?.key; untrack(() => { view = null; void load(); }); });
  // A change to the lab's methods or grants reads the page again in place; a failed quiet read keeps what it shows.
  const unwatch = live().watch(["lab:*"], (changed) => { if (group && covers(changed, `lab:${group.key}`)) return load(true); }, { gapMs: 2_000 });
  onDestroy(() => { controller?.abort(); unwatch(); });

  const sections = $derived(view ? toolSections(view) : []);
  const queryCount = (tool: LabTool) => tool.reviewed_queries?.length ?? 0;
</script>

<section class="tools-page" aria-labelledby="tools-title">
  <header>
    <h1 id="tools-title">Tools</h1>
    {#if group}<p>The methods people and agents in {group.name} may run, under the tool groups the lab holds. {toolsClaim}</p>{/if}
  </header>
  {#if !group}<p class="soft">Your account is not in a lab yet.</p>
  {:else if loading && !view}<p class="soft" role="status">Opening the lab's tools…</p>
  {:else if failure}<p class="notice error" role="alert">{failure}</p>
  {:else if view && !view.tools.length}
    <p class="soft empty">No method is granted to {group.name} yet. A site administrator grants methods and tool groups to a lab.</p>
  {:else if view}
    {#each sections as section (sectionKey(section))}
      <section class="section" aria-labelledby={`tools-${sectionKey(section)}`}>
        <div class="section-head">
          <h2 id={`tools-${sectionKey(section)}`}>{section.group ? section.group.title : "Granted directly"}</h2>
          {#if section.group}
            <p class="small soft">Tool group, version {section.group.version}, granted on {day(section.group.granted_at)}. {section.group.description}</p>
          {:else}
            <p class="small soft">Methods granted to the lab one by one.</p>
          {/if}
        </div>
        <ul class="tools">
          {#each section.tools as tool (tool.profile)}
            <li>
              <details>
                <summary>
                  <span class="name">{tool.name}</span>
                  <span class="kind small">{kindName(tool.kind)}{#if queryCount(tool)}{` · ${queryCount(tool)}`} reviewed {queryCount(tool) === 1 ? "query" : "queries"}{/if}</span>
                </summary>
                <div class="body">
                  <p class="description">{tool.description}</p>
                  <p class="small soft">{kindSentence(tool.kind)} {grantWords(tool, view.tool_groups)}</p>
                  <dl class="facts small">
                    {#if tool.source}<dt>Reads</dt><dd>The lab data source <code>{tool.source}</code></dd>{/if}
                    {#if tool.template}<dt>Template</dt><dd><code>{tool.template.ref}</code> at <code title={tool.template.sha256}>{shortDigest(tool.template.sha256)}</code></dd>{/if}
                    <dt>Method key</dt><dd><code>{tool.profile}</code></dd>
                  </dl>
                  {#if tool.configuration?.settings.length}
                    <h3>Settings</h3>
                    <ul class="settings small">
                      {#each tool.configuration.settings as setting (setting.name)}
                        <li><span class="label">{setting.label ?? setting.name}</span>{#if setting.choices.length}: {setting.choices.map((c) => c.label ?? c.key).join(", ")}{/if}</li>
                      {/each}
                    </ul>
                  {/if}
                  {#if tool.reviewed_queries?.length}
                    <h3>Reviewed queries</h3>
                    <ul class="queries">
                      {#each tool.reviewed_queries as query (query.key)}{@render reviewed(query)}{/each}
                    </ul>
                  {/if}
                  <h3>Examples</h3>
                  {#if tool.examples.length}
                    <ul class="examples small">
                      {#each tool.examples as example (example.task)}
                        <li><a class="link" href={href({ name: "analysis", project: example.project, task: example.task })}>An accepted analysis from {day(example.finished_at)}</a></li>
                      {/each}
                    </ul>
                  {:else}
                    <p class="small soft">No analysis of this method in a project you can open has been accepted yet.</p>
                  {/if}
                </div>
              </details>
            </li>
          {/each}
        </ul>
      </section>
    {/each}
  {/if}
</section>

{#snippet reviewed(query: ReviewedQuery)}
  <li>
    <p class="query-title">{query.title ?? query.key} <span class="small soft">version {query.version}</span></p>
    {#if query.purpose}<p class="small">{query.purpose}</p>{/if}
    {#if query.parameters.length}
      <table class="small">
        <caption class="soft">What it asks for</caption>
        <thead><tr><th scope="col">Setting</th><th scope="col">Accepts</th><th scope="col">Meaning</th></tr></thead>
        <tbody>
          {#each query.parameters as p (p.name)}<tr><th scope="row"><code>{p.name}</code></th><td>{parameterWords(p)}</td><td>{p.description ?? ""}</td></tr>{/each}
        </tbody>
      </table>
    {/if}
    {#if query.output_columns.length}
      <table class="small">
        <caption class="soft">What it returns{#if query.row_cap}, at most {query.row_cap.toLocaleString()} rows{/if}</caption>
        <thead><tr><th scope="col">Column</th><th scope="col">Type</th><th scope="col">Meaning</th></tr></thead>
        <tbody>
          {#each query.output_columns as column (column.name)}<tr><th scope="row"><code>{column.name}</code></th><td>{column.type ?? ""}</td><td>{column.description ?? ""}</td></tr>{/each}
        </tbody>
      </table>
    {/if}
  </li>
{/snippet}

<style>
  .tools-page { padding: 40px var(--gutter) 24px; display: grid; gap: 26px; width: 100%; max-width: 1080px; margin: 0 auto; align-content: start; }
  header { display: grid; gap: 10px; }
  h1 { font-size: clamp(30px, calc(3.4vw * var(--type-scale)), calc(42px * var(--type-scale))); letter-spacing: -0.022em; }
  header p, .empty { max-width: 76ch; }
  .section { display: grid; gap: 10px; max-width: 900px; }
  .section-head { display: grid; gap: 4px; padding-bottom: 8px; border-bottom: 1px solid var(--line); }
  h2 { font-size: calc(19px * var(--type-scale)); font-weight: 700; color: var(--heading); }
  .section-head p { max-width: 80ch; }
  .tools { list-style: none; margin: 0; padding: 0; display: grid; }
  .tools > li { border-bottom: 1px solid var(--line); }
  summary { cursor: pointer; list-style: none; display: flex; flex-wrap: wrap; align-items: baseline; gap: 4px 14px; padding: 12px 0; min-height: 44px; }
  summary::-webkit-details-marker { display: none; }
  /* The disclosure mark: a chevron that turns down when the row is open. */
  summary::before { content: ""; width: 6px; height: 6px; margin-right: 2px; border-right: 1.5px solid var(--text-2); border-bottom: 1.5px solid var(--text-2);
    transform: translateY(-2px) rotate(-45deg); transition: transform 160ms ease-out; flex: none; align-self: center; }
  details[open] > summary::before { transform: translateY(-3px) rotate(45deg); }
  @media (prefers-reduced-motion: reduce) { summary::before { transition: none; } }
  summary:focus-visible { outline: 2px solid var(--focus); outline-offset: 2px; border-radius: 4px; }
  .name { font-weight: 600; color: var(--text); }
  .kind { color: var(--text-2); }
  .body { display: grid; gap: 10px; padding: 0 0 18px 2px; justify-items: start; min-width: 0; }
  .description { max-width: 72ch; }
  h3 { font-size: var(--font-small); font-weight: 700; color: var(--heading); margin-top: 6px; }
  .facts { display: grid; grid-template-columns: max-content minmax(0, 1fr); gap: 4px 14px; }
  .facts dt { color: var(--text-2); }
  .facts dd { margin: 0; min-width: 0; overflow-wrap: anywhere; }
  .settings, .examples, .queries { margin: 0; padding-left: 18px; display: grid; gap: 4px; }
  .queries { list-style: none; padding-left: 0; gap: 18px; width: 100%; }
  .queries > li { display: grid; gap: 6px; min-width: 0; }
  .query-title { font-weight: 600; }
  .label { color: var(--text); }
  table { border-collapse: collapse; width: 100%; max-width: 820px; display: block; overflow-x: auto; }
  caption { text-align: left; padding-bottom: 4px; }
  th, td { text-align: left; vertical-align: top; padding: 5px 12px 5px 0; border-top: 1px solid var(--line); }
  thead th { color: var(--text-2); font-weight: 600; border-top: none; }
  code { overflow-wrap: anywhere; }
  th[scope="row"] code { overflow-wrap: normal; white-space: nowrap; }
</style>
