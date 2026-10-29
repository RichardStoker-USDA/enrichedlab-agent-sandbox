<script lang="ts">
  import { verifiedMember } from "./api.ts";
  import type { GeneratedCode, PackageMember } from "./contracts.ts";
  import type { RetainedPackage } from "./evidence.ts";
  import { when } from "./language.ts";
  import StatusMark from "./StatusMark.svelte";
  import TextView from "./TextView.svelte";

  // Model-written R, or a configured recipe rendered from the agent's chosen settings, shown
  // with what was checked before it ran. The block comes from the broker's admitted record and
  // is shown only after its digest matches; the template's own check and isolation probe come
  // from the retained package.
  let { taskId, code, pkg, onDownload }: {
    taskId: string; code: GeneratedCode; pkg: RetainedPackage | null; onDownload: () => void;
  } = $props();

  type Probe = { enforced: boolean; isolated: boolean; checks: Record<string, string | boolean> };
  type Recheck = { result: string; reasons: string[] };
  type Dependence = {
    checked: boolean; outcome: string; rows_removed?: number; reason?: string;
    tables?: { name: string; numeric_cells: number; changed_cells: number; outcome: string }[];
  };
  type Outcome = { status: string; error: string | null; r_version: string; seed: number; evaluation?: string; input_dependence?: Dependence };
  let verified = $state<boolean | null>(null);
  let probe = $state<Probe | null>(null);
  let recheck = $state<Recheck | null>(null);
  let outcome = $state<Outcome | null>(null);

  const short = (digest: string) => digest.replace(/^sha256:/, "").slice(0, 12);
  const configured = $derived(code.configuration ?? null);
  const codeTitle = $derived(configured ? "Configured recipe with its settings" : "Model-written block");
  const functions = $derived(code.admission.functions_used);
  const isolation: { key: string; expected: string | boolean; text: string }[] = [
    { key: "external_tcp_1_1_1_1_443", expected: "refused", text: "No internet: a test connection to an outside address was refused" },
    { key: "cluster_api_10_43_0_1_443", expected: "refused", text: "No cluster control: the cluster API could not be reached" },
    { key: "dns_example_org", expected: "unresolved", text: "No name lookups: an internet name could not be resolved" },
    { key: "service_account_token", expected: "absent", text: "No cluster credential inside the sandbox" },
    { key: "input_writable", expected: false, text: "The staged input could not be changed" },
    { key: "root_writable", expected: false, text: "System files could not be changed" },
  ];
  const outcomeText: Record<string, string> = {
    succeeded: "The block ran and returned its tables and figures.",
    block_failed: "The block stopped with an error. Its code, logs and any files it wrote are kept.",
    result_invalid: "The block returned something other than named tables and figures.",
    boundary_violation: "The isolation probe found access it should not have; the block did not run.",
    admission_rejected: "The template's own check refused the block; it did not run.",
    input_independent: "A table's numbers did not change when the input changed, so they did not come from the input. The results were not kept.",
  };
  // Template v2 reruns the block on a changed copy of the input (rows removed, small
  // noise on measured values) and compares the tables' numbers.
  const dependence = $derived(outcome?.input_dependence && outcome.input_dependence.outcome !== "not_run" ? outcome.input_dependence : null);
  const changed = $derived((dependence?.tables ?? []).reduce((sum, t) => sum + t.changed_cells, 0));
  const cells = $derived((dependence?.tables ?? []).reduce((sum, t) => sum + t.numeric_cells, 0));
  const unchanged = $derived((dependence?.tables ?? []).filter((t) => t.outcome === "unchanged").map((t) => t.name));

  async function digest(text: string) {
    const bytes = new TextEncoder().encode(text);
    return [...new Uint8Array(await crypto.subtle.digest("SHA-256", bytes))].map((b) => b.toString(16).padStart(2, "0")).join("");
  }
  async function member<T>(path: string, signal: AbortSignal): Promise<T | null> {
    const found: PackageMember | undefined = pkg?.contents.listing.members.find((m) => m.path === path);
    if (!pkg || !found) return null;
    return JSON.parse(await (await verifiedMember(taskId, pkg.artifact, found, signal)).text()) as T;
  }
  $effect(() => {
    const c = new AbortController();
    verified = null; probe = null; recheck = null; outcome = null;
    void digest(code.content).then((hex) => { if (!c.signal.aborted) verified = `sha256:${hex}` === code.sha256 && code.content.length > 0; });
    void member<Probe>("boundary.json", c.signal).then((v) => { if (!c.signal.aborted) probe = v; }).catch(() => {});
    void member<Recheck>("admission.json", c.signal).then((v) => { if (!c.signal.aborted) recheck = v; }).catch(() => {});
    void member<Outcome>("run.json", c.signal).then((v) => { if (!c.signal.aborted) outcome = v; }).catch(() => {});
    return () => c.abort();
  });
</script>

<div class="generated">
  {#if configured}
    <p class="abstract">
      This is a configured recipe. The research agent chose the settings below and wrote none of the code. It ran inside
      the analysis template in an isolated sandbox. The checks below are the template's own records; an independent
      review of the recipe and of those checks is still open.
      <strong>Treat the results as unvalidated until you have read the code.</strong>
    </p>
    <dl class="settings">
      {#each configured.described as s (s.setting)}<dt>{s.label}</dt><dd>{s.choice_label}</dd>{/each}
    </dl>
  {:else}
    <p class="abstract">
      The research model wrote this R code. It ran inside the analysis template in an isolated sandbox. The checks below
      are the template's own records; an independent review of them is still open.
      <strong>Treat the results as unvalidated until you have read the code.</strong>
    </p>
  {/if}

  <div class="checks">
    <h3>Checked before it ran</h3>
    <ul>
      <li><StatusMark tone={code.admission.result === "accepted" ? "done" : "failed"} label={code.admission.result === "accepted" ? "Accepted by the broker's check" : "Refused by the broker's check"} />
        <span class="detail">Calls {functions.length} functions{#if code.admission.namespaces.length}{" from "}{code.admission.namespaces.join(", ")}{/if}; {code.admission.numeric_literals} numbers and {code.admission.string_literals} text values written in the code; {code.admission.uses_random_numbers ? "uses random numbers from the recorded seed" : "no random numbers"}.</span></li>
      {#if recheck}
        <li><StatusMark tone={recheck.result === "accepted" ? "done" : "failed"} label={recheck.result === "accepted" ? "Accepted again by the template" : "Refused by the template"} />
          {#if recheck.reasons.length}<span class="detail">{recheck.reasons.join(", ")}</span>{/if}</li>
      {/if}
      {#if probe}
        {#each isolation as item (item.key)}
          <li><StatusMark tone={probe.checks[item.key] === item.expected ? "done" : "failed"} label={item.text} /></li>
        {/each}
        {#if !probe.enforced}<li class="detail">This record comes from a local rerun, where isolation is recorded but not enforced.</li>{/if}
      {:else if pkg}
        <li class="detail">Reading the isolation record from the analysis package…</li>
      {/if}
    </ul>
    {#if outcome?.evaluation || dependence}
      <h3>Checked while it ran</h3>
      <ul>
        {#if outcome?.evaluation}<li><StatusMark tone="done" label="Ran in a separate R process; every function its code names is on the template's allowlist" /></li>{/if}
        {#if dependence?.outcome === "passed"}
          <li><StatusMark tone="done" label="Its tables changed when the input changed, by the template's count" />
            <span class="detail">Rerun on a copy with {dependence.rows_removed ?? 0} {dependence.rows_removed === 1 ? "row" : "rows"} removed and small noise on measured values: the template counted {changed} of {cells} numbers as changed.</span></li>
        {:else if dependence?.outcome === "failed"}
          <li><StatusMark tone="failed" label="A table did not change when the input changed" />
            <span class="detail">{unchanged.join(", ")}</span></li>
        {:else if dependence}
          <li><StatusMark tone="attention" label="Whether its tables depend on the input was not checked" />
            {#if dependence.reason}<span class="detail">{dependence.reason}</span>{/if}</li>
        {/if}
      </ul>
    {/if}
    {#if outcome}<p class="outcome"><span class="lead">Outcome.</span> {outcomeText[outcome.status] ?? outcome.status}{#if outcome.error && outcome.status !== "succeeded"} <span class="soft">{outcome.error}</span>{/if}</p>{/if}
  </div>

  {#if verified === false}
    <p class="refused">The stored code does not match its recorded checksum, so it is not shown.</p>
  {:else if verified}
    <TextView file={{ label: codeTitle, path: code.path, media_type: "text/x-r", size_bytes: code.size_bytes, sha256: code.sha256 }}
      load={async () => new Blob([code.content], { type: "text/plain" })} {onDownload} title={codeTitle} code showTitle={false} />
  {/if}

  <dl class="record small">
    <dt>Template</dt><dd>{code.template_ref} <span class="soft">· {short(code.template_sha256)}</span></dd>
    {#if configured}<dt>Recipe</dt><dd title={configured.block_sha256}>{short(configured.block_sha256)}</dd>{/if}
    <dt>Code checksum</dt><dd title={code.sha256}>{short(code.sha256)} · {code.admission.lines} lines</dd>
    <dt>Written</dt><dd>{when(code.created_at)}, research decision {code.turn}</dd>
    {#if outcome}<dt>Environment</dt><dd>{outcome.r_version}, seed {outcome.seed}</dd>{/if}
  </dl>
</div>

<style>
  .generated { display: grid; gap: 24px; max-width: 920px; }
  .abstract { font-family: var(--serif); font-size: var(--font-lede); line-height: 1.55; max-width: 66ch; }
  .abstract strong { font-weight: 600; color: var(--heading); }
  .settings { display: grid; grid-template-columns: max-content minmax(0, 1fr); gap: 6px 18px; margin: 0; padding: 12px 0 0;
    border-top: 1px solid var(--line); }
  .settings dt { color: var(--text-3); font-size: var(--font-small); }
  .settings dd { margin: 0; color: var(--heading); font-weight: 600; }
  @media (max-width: 640px) { .settings { grid-template-columns: 1fr; } .settings dd { padding-bottom: 6px; } }
  .checks { display: grid; gap: 10px; padding-top: 14px; border-top: 2px solid var(--heading); }
  h3 { font-size: calc(15px * var(--type-scale)); color: var(--heading); }
  ul { list-style: none; margin: 0; padding: 0; display: grid; gap: 8px; }
  li { display: grid; gap: 2px; min-width: 0; }
  /* Check labels are sentences here; let them wrap on narrow screens. */
  .checks :global(.status) { white-space: normal; align-items: flex-start; }
  .checks :global(.status .glyph) { margin-top: 0.3em; }
  .detail { font-size: var(--font-small); color: var(--text-2); padding-left: 22px; }
  li.detail { padding-left: 0; }
  .outcome { font-size: var(--font-small); }
  .lead { font-weight: 700; color: var(--heading); }
  .refused { padding-left: 10px; border-left: 3px solid var(--failed); }
  .record { display: grid; grid-template-columns: max-content minmax(0, 1fr); gap: 4px 16px; padding-top: 10px; border-top: 1px solid var(--line); }
  .record dt { color: var(--text-3); }
  .record dd { margin: 0; font-family: var(--mono); font-size: 0.95em; overflow-wrap: anywhere; }
  @media (max-width: 640px) { .record { grid-template-columns: 1fr; } .record dd { padding-bottom: 6px; } }
</style>
