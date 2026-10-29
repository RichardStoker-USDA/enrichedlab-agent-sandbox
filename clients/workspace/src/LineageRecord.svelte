<script lang="ts">
  import type { Lineage, StepLineage, TaskReview } from "./contracts.ts";
  import { paperReadWords, type PaperRead } from "./library.ts";
  import { describeLineage, lineageSummary } from "./lineage.ts";
  import { href } from "./route.ts";
  import StepLineageRecord from "./StepLineage.svelte";

  // Where one analysis came from, read from the broker's records: who asked, the plan
  // step, the data, the method, the code, where it ran, what it made and its review.
  // A part the records do not hold is said in words and marked in the margin. Identifiers,
  // digests and resource figures stay under Technical details.
  // `reviewLink` names the link to the page's review section, when the page has one. `papers` are the library papers
  // the agent cited when it planned the analysis; they follow the plan step.
  let { lineage, reader, reviews = [], reviewLink = "", papers = [], onShow }: {
    lineage: Lineage | StepLineage; reader: string; reviews?: TaskReview[]; reviewLink?: string; papers?: PaperRead[];
    onShow: (section: "a-code" | "a-query" | "a-files" | "a-review") => void;
  } = $props();
  const rows = $derived(describeLineage(lineage, reader, reviews));
  const papersAfter = $derived(rows.some((row) => row.key === "step") ? "step" : "asked");
  const technical = $derived(rows.filter((row) => row.details.length));
  const links = $derived<Partial<Record<string, { section: "a-code" | "a-query" | "a-files" | "a-review"; label: string }>>>({
    code: { section: "a-code", label: "Read the code" },
    query: { section: "a-query", label: "Read the query" },
    results: { section: "a-files", label: "See the files" },
    ...(reviewLink ? { review: { section: "a-review", label: reviewLink } } : {}),
  });
</script>

<div class="lineage">
  <p class="summary">{lineageSummary(lineage)}</p>
  <dl>
    {#each rows as row (row.key)}
      <div class="row" class:change-bar={row.gaps.length > 0}>
        <dt>{row.label}</dt>
        <dd>
          <p>{row.text}</p>
          {#each row.notes as line (line)}<p class="note">{line}</p>{/each}
          {#each row.gaps as gap (gap)}<p class="gap">{gap}</p>{/each}
          {#if row.key === "data" && lineage.schema === "enrichedlab.task-lineage.v3"}
            <StepLineageRecord {lineage} />
          {/if}
          {#if links[row.key] && (row.key !== "results" || lineage.outputs.length)}
            {@const link = links[row.key]!}
            <button class="link small" onclick={() => onShow(link.section)}>{link.label}</button>
          {/if}
        </dd>
      </div>
      {#if row.key === papersAfter && papers.length}
        <div class="row papers">
          <dt>Papers</dt>
          <dd>
            <p>{`When the agent planned this analysis it cited ${papers.length === 1 ? "a paper" : `${papers.length} papers`} from the lab's library${papers.some((item) => item.state === "commons") ? " and the Commons" : ""}.`}</p>
            <ul>
              {#each papers as item (item.paper_id)}
                <li>{#if item.paper}<a class="link" href={href({ name: "paper", paper: item.paper_id })}>{item.paper.title?.trim() || "An untitled paper"}</a>{item.pages.length ? `, ${item.pages.join(", ")}` : ""}
                  {:else if item.state === "commons"}<a class="link" href={href({ name: "commons" })}>{item.title?.trim() || "A paper"}</a>{`, from the Commons${item.pages.length ? `, ${item.pages.join(", ")}` : ""}`}
                  {:else}{`${paperReadWords(item).charAt(0).toUpperCase()}${paperReadWords(item).slice(1)}.`}{/if}</li>
              {/each}
            </ul>
            <p class="note">Papers are background the agent read. The results come from this analysis's own data.</p>
          </dd>
        </div>
      {/if}
    {/each}
  </dl>
  {#if technical.length}
    <details class="technical">
      <summary>Technical details</summary>
      <dl>
        {#each technical as row (row.key)}<dt>{row.label}</dt><dd>{row.details.join("; ")}</dd>{/each}
      </dl>
    </details>
  {/if}
</div>

<style>
  .lineage { display: grid; gap: 14px; max-width: 920px; }
  .summary { font-family: var(--serif); font-size: var(--font-lede); line-height: 1.5; max-width: 66ch; color: var(--text); }
  .lineage > dl { margin: 0; border-top: 2px solid var(--heading); }
  .row { display: grid; grid-template-columns: 150px minmax(0, 1fr); gap: 4px 20px; padding: 11px 0; border-bottom: 1px solid var(--line); }
  .row dt { font-weight: 700; font-size: var(--font-small); color: var(--heading); padding-top: 1px; }
  .row dd { margin: 0; display: grid; gap: 3px; justify-items: start; max-width: 70ch; }
  .note { font-size: var(--font-small); color: var(--text-2); }
  .gap { font-size: var(--font-small); color: var(--text-2); }
  .papers ul { margin: 0; padding-left: 18px; display: grid; gap: 3px; }
  @media (max-width: 640px) {
    .row { grid-template-columns: 1fr; gap: 2px; }
  }
</style>
