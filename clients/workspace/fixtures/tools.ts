import type { IncomingMessage, ServerResponse } from "node:http";
import type { LabTools } from "../src/contracts.ts";

// A synthetic tools list for the loopback preview, shaped like the broker's `GET /api/v1/labs/{lab}/tools` (migration
// 080). Names, queries and columns are invented; nothing here names a real method, dataset or table.

type Json = (value: unknown, status?: number) => void;
const digest = (c: string) => `sha256:${c.repeat(64)}`;

export function toolFixtures(options: { at: (seconds: number) => string; analysis: { task: string; project: string } | null }) {
  const { at, analysis } = options;
  const view: LabTools = {
    lab: "open_methods",
    tool_groups: [
      { key: "core-summaries", version: 2, title: "Core summaries", granted_at: at(-86400 * 9),
        description: "Group means, comparisons and their figures for the lab's result tables.",
        tools: ["figure_render", "template_analysis"] },
      { key: "reference-lookups", version: 1, title: "Reference lookups", granted_at: at(-86400 * 30),
        description: "Read-only lookups in the synthetic reference tables.", tools: ["reference_query"] },
    ],
    tools: [
      { profile: "template_analysis", name: "Template analysis", description: "Runs a reviewed R template on a result table.",
        route: "server-analysis", kind: "configured_method", via_groups: ["core-summaries"],
        template: { ref: "templates/group_compare.R", sha256: digest("a") },
        configuration: { block_sha256: digest("b"), settings: [
          { name: "test", label: "Comparison test", choices: [{ key: "welch", label: "Welch t-test" }, { key: "wilcoxon", label: "Wilcoxon rank-sum" }] },
          { name: "adjust", label: "Multiple testing", choices: [{ key: "bh", label: "Benjamini-Hochberg" }, { key: "none", label: "None" }] }] },
        examples: analysis ? [{ task: analysis.task, project: analysis.project, finished_at: at(-3600 * 5) }] : [] },
      { profile: "figure_render", name: "Draw a figure", description: "Draw a figure from a result table.",
        route: "server-render", kind: "render_method", via_groups: ["core-summaries"], examples: [] },
      { profile: "reference_query", name: "Reference lookup", description: "Reads the synthetic reference tables.",
        route: "server-query", kind: "query_method", via_groups: ["reference-lookups"], source: "synthetic-reference",
        reviewed_queries: [
          { key: "nutrient-by-food-group", version: 3, source: "synthetic-reference", title: "Nutrient amounts by food group",
            purpose: "Averages one invented nutrient across the foods of each group.",
            parameters: [
              { name: "nutrient", description: "The nutrient to average", type: "enum", required: true, choices: ["fibre", "protein", "vitamin_c"] },
              { name: "min_foods", description: "Leave out groups with fewer foods", type: "integer", required: false, minimum: 1, maximum: 500 }],
            output_columns: [
              { name: "food_group", type: "text", description: "Food group" },
              { name: "foods", type: "integer", description: "Foods averaged" },
              { name: "mean_amount", type: "numeric", description: "Mean amount per 100 g" }],
            row_cap: 5000, template_sha256: digest("c") }],
        examples: [] },
      { profile: "evidence_review", name: "Evidence review", description: "Inspect approved inputs and retain each tool's original files.",
        route: "local-agent", kind: "sandbox_method", via_groups: [], examples: [] },
    ],
  };
  return {
    async handle(req: IncomingMessage, _res: ServerResponse, url: URL, json: Json): Promise<boolean> {
      const m = url.pathname.match(/^\/api\/v1\/labs\/([a-z][a-z0-9_-]{1,63})\/tools$/);
      if (!m || req.method !== "GET") return false;
      if (m[1] === "open_methods") json(view);
      else if (m[1] === "field_trials") json({ lab: "field_trials", tool_groups: [], tools: [] });
      else json({ error: { code: "lab_not_found", message: "Refused" } }, 404);
      return true;
    },
  };
}
