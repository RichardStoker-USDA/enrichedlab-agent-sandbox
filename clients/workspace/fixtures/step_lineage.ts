import { createHash } from "node:crypto";
import { readFileSync } from "node:fs";
import type { IncomingMessage, ServerResponse } from "node:http";
import type { Artifact, Lineage, StepLineage, Task, TaskEvent } from "../src/contracts.ts";
import { listing, syntheticPackage } from "./package.ts";
import type { ProjectTask } from "./projects.ts";

// One fixed, synthetic record for inspecting the version 3 drill-down. The upstream task ids
// exist only in the lineage response, just as tasks in another retained project need not be in
// the current project's listing.
export const STEP_LINEAGE_PROJECT = "50000000-0000-4000-8000-000000000001";
export const STEP_LINEAGE_TASK = "10000000-0000-4000-8000-000000000005";
const ANALYSIS_TASK = "11000000-0000-4000-8000-000000000001";
const QUERY_TASK = "11000000-0000-4000-8000-000000000002";
const ANALYSIS_ARTIFACT = "32000000-0000-4000-8000-000000000001";
const QUERY_ARTIFACT = "32000000-0000-4000-8000-000000000002";
const PACKAGE_ARTIFACT = "31000000-0000-4000-8000-000000000005";
const CHECKSUM_ARTIFACT = "31000000-0000-4000-8000-000000000006";

type Json = (value: unknown, status?: number) => void;
const sha = (bytes: Buffer | string) => createHash("sha256").update(bytes).digest("hex");
const digest = (seed: string) => sha(`synthetic-step-lineage:${seed}`);
const image = (seed: string) => `sha256:${digest(`image:${seed}`)}`;

export function stepLineageFixture(options: { me: string; at: (seconds: number) => string }) {
  const { me, at } = options;
  const files = [
    {
      path: "README.md",
      bytes: Buffer.from("# Recorded-source figure\n\nSynthetic retained output for the step-lineage preview.\n"),
    },
    {
      path: "results/recorded_source_figure.svg",
      bytes: readFileSync(new URL("./r/results/response_by_diet.svg", import.meta.url)),
    },
    {
      path: "results/recorded_source_figure.pdf",
      bytes: readFileSync(new URL("./r/results/response_by_diet.pdf", import.meta.url)),
    },
    {
      path: "results/method.txt",
      bytes: Buffer.from("Synthetic preview output retained from the reviewed figure method.\n"),
    },
  ];
  // Dated two hours before the reader's last look, so the demo never counts as new work.
  const pkg = syntheticPackage(files, Math.floor(Date.parse(at(-7080)) / 1000));
  const artifacts: Artifact[] = [
    {
      id: PACKAGE_ARTIFACT,
      role: "result",
      label: "Figure and reproducibility package",
      relative_path: `tasks/${STEP_LINEAGE_TASK}/payload`,
      media_type: "application/gzip",
      size_bytes: pkg.archive.length,
      sha256: pkg.sha256,
      created_at: at(-7080),
    },
    {
      id: CHECKSUM_ARTIFACT,
      role: "result",
      label: "Package checksum",
      relative_path: `tasks/${STEP_LINEAGE_TASK}/payload.sha256`,
      media_type: "text/plain",
      size_bytes: pkg.checksum.length,
      sha256: sha(pkg.checksum),
      created_at: at(-7080),
    },
  ];
  const outputs: Lineage["outputs"] = artifacts.map((artifact) => ({
    artifact_id: artifact.id,
    role: artifact.role,
    label: artifact.label,
    relative_path: artifact.relative_path,
    media_type: artifact.media_type,
    size_bytes: artifact.size_bytes,
    sha256: artifact.sha256,
    created_at: artifact.created_at,
    provenance_class: "vetted_tool_output",
  }));
  const task: Task = {
    id: STEP_LINEAGE_TASK,
    group: "open_methods",
    profile: "figure_render",
    objective: "Draw the publication figure from the reviewed nutrient summary",
    status: "completed",
    attempt: 1,
    max_task_attempts: 2,
    max_provider_attempts: 2,
    logical_agent: "render-worker",
    result_class: "success",
    error_code: null,
    submitted_at: at(-7200),
    started_at: at(-7195),
    updated_at: at(-7080),
    finished_at: at(-7080),
  };
  const events: TaskEvent[] = [
    { sequence: 1, kind: "task", state: "queued", code: "task_accepted", occurred_at: at(-7200) },
    { sequence: 2, kind: "attempt", state: "running", code: "worker_lease_acquired", occurred_at: at(-7195) },
    { sequence: 3, kind: "task", state: "completed", code: "task_completed", occurred_at: at(-7080) },
  ];
  const entry: ProjectTask = {
    task_id: STEP_LINEAGE_TASK,
    action_key: "draw-recorded-source-figure",
    parent_task_id: null,
    profile: "figure_render",
    status: "completed",
    attempt: 1,
    submitted_at: at(-7200),
    updated_at: at(-7080),
    figure: {
      title: "Publication figure from recorded sources",
      feature: "point_range",
      source_task: ANALYSIS_TASK,
      table_path: "results/nutrient_summary.csv",
      previous: null,
    },
  };

  const render = {
    source_task: ANALYSIS_TASK,
    table_path: "results/nutrient_summary.csv",
    table_sha256: digest("analysis-table"),
    source_package_sha256: digest("analysis-package"),
    feature: "point_range",
    controls: {},
    title: "Publication figure from recorded sources",
    previous: null,
    request_sha256: digest("figure-request"),
    created_at: at(-7200),
    service_version: "1.4.0",
    image_digest: image("render"),
  };
  const rootMethod: Lineage["method"] = {
    profile: "figure_render",
    profile_name: "Figure from a result table",
    tool_class: "vetted_service",
    template_ref: null,
    template_sha256: null,
    recorded: "submission",
    harness: null,
    role: null,
    tool_bundle: "render_service",
  };
  const lineage: StepLineage = {
    schema: "enrichedlab.task-lineage.v3",
    task: {
      task_id: task.id,
      project_id: STEP_LINEAGE_PROJECT,
      action_key: entry.action_key,
      objective: task.objective!,
      status: task.status,
      result_class: task.result_class,
      error_code: task.error_code,
      attempt: task.attempt,
      submitted_at: task.submitted_at,
      started_at: task.started_at,
      finished_at: task.finished_at,
    },
    origin: {
      origin: "person",
      basis: "submission",
      principal_id: me,
      principal_name: "Example researcher",
      principal_kind: "person",
      recorded_at: task.submitted_at,
    },
    plan_step: {
      action_key: entry.action_key,
      title: "Draw the publication figure",
      status: "completed",
      rationale: "Use the reviewed renderer on the retained analysis table.",
    },
    inputs: {
      workspace: null,
      checkpoint_revision: null,
      platform_tool: null,
      classes: ["prior_output"],
      classes_recorded_by: "render_request",
    },
    method: rootMethod,
    code: null,
    query: null,
    render,
    execution: {
      image_ref: image("render"),
      cpu_limit: null,
      memory_limit: null,
      max_run_seconds: 120,
      attempts: 1,
      scientific_history: 0,
    },
    outputs,
    review: { state: "not_reviewed" },
    gaps: [],
    attempts: [{
      attempt: 1,
      inputs: [{
        input_ref: `artifact://${ANALYSIS_ARTIFACT}`,
        class: "prior_output",
        sha256: digest("analysis-table"),
        size_bytes: 640,
        custody_transaction: "synthetic-custody-figure",
        source: { artifact_id: ANALYSIS_ARTIFACT },
        lease_generation: 1,
        recorded_at: at(-7196),
      }],
      execution: {
        image_digest: image("render"),
        template_ref: null,
        template_sha256: null,
        tool_ref: "render-service/point-range",
        parameters_sha256: digest("figure-parameters"),
        lease_generation: 1,
        recorded_at: at(-7195),
      },
      outputs: outputs.map((output) => ({
        artifact_id: output.artifact_id,
        relative_path: output.relative_path,
        role: output.role,
        sha256: output.sha256,
        size_bytes: output.size_bytes,
        lease_generation: 1,
        recorded_at: output.created_at,
        current: true,
      })),
      validation: { state: "not_reviewed" },
      gaps: [],
    }],
    upstream: {
      nodes: [
        {
          task_id: STEP_LINEAGE_TASK,
          title: "Publication figure from recorded sources",
          method_class: "vetted_service",
          method: { ...rootMethod, render },
          attempts: [],
          outputs,
          review: { state: "not_reviewed" },
          gaps: [],
        },
        {
          task_id: ANALYSIS_TASK,
          title: "Summarize nutrients for the figure",
          method_class: "vetted_service",
          method: {
            profile: "template_analysis",
            profile_name: "Reviewed nutrient summary",
            tool_class: "vetted_service",
            template_ref: "analysis/nutrient-summary.R",
            template_sha256: digest("analysis-template"),
            recorded: "submission",
            harness: null,
            role: null,
            tool_bundle: "render_service",
            analysis: {
              source_task: QUERY_TASK,
              table_path: "results/fdc_query.csv",
              table_sha256: digest("query-table"),
              source_package_sha256: digest("query-package"),
              feature: "nutrient_summary",
              settings: {},
              title: "Summarize nutrients for the figure",
              previous: null,
              request_sha256: digest("analysis-request"),
              created_at: at(-7280),
              service_version: "1.4.0",
              image_digest: image("analysis"),
              method: "Reviewed nutrient summary",
            },
          },
          attempts: [{
            attempt: 1,
            execution: {
              image_digest: image("analysis"),
              template_ref: "analysis/nutrient-summary.R",
              template_sha256: digest("analysis-template"),
              tool_ref: null,
              parameters_sha256: digest("analysis-parameters"),
              lease_generation: 1,
              recorded_at: at(-7275),
            },
            outputs: [{
              artifact_id: ANALYSIS_ARTIFACT,
              relative_path: "results/nutrient_summary.csv",
              role: "result",
              sha256: digest("analysis-table"),
              size_bytes: 640,
              lease_generation: 1,
              recorded_at: at(-7230),
              current: true,
            }],
            gaps: [],
          }],
          outputs: [{
            artifact_id: ANALYSIS_ARTIFACT,
            role: "result",
            label: "Reviewed nutrient summary",
            relative_path: "results/nutrient_summary.csv",
            media_type: "text/csv",
            size_bytes: 640,
            sha256: digest("analysis-table"),
            created_at: at(-7230),
            provenance_class: "vetted_tool_output",
          }],
          review: { state: "not_reviewed" },
          gaps: [],
        },
        {
          task_id: QUERY_TASK,
          title: "Read the Foundation Foods snapshot",
          method_class: "guarded_query",
          method: {
            profile: "reference-sql-fdc",
            profile_name: "Query FoodData Central",
            tool_class: "guarded_query",
            template_ref: null,
            template_sha256: null,
            recorded: "submission",
            harness: null,
            role: null,
            tool_bundle: "guarded_sql",
            query: {
              source: "fooddata-central",
              sha256: digest("query"),
              guard: "guarded-sql/v1",
              relations: ["food", "food_nutrient"],
              row_limit: 500,
              run_id: null,
              turn: null,
              created_at: at(-7380),
              title: "FoodData Central",
              release: "April 2026",
              snapshot_sha256: digest("fdc-snapshot"),
              data_class: "public_reference",
              snapshot_kind: null,
              source_version: null,
            },
          },
          attempts: [{
            attempt: 1,
            execution: {
              image_digest: image("query"),
              template_ref: null,
              template_sha256: null,
              tool_ref: "guarded-sql/v1",
              parameters_sha256: digest("query-parameters"),
              lease_generation: 1,
              recorded_at: at(-7375),
            },
            outputs: [{
              artifact_id: QUERY_ARTIFACT,
              relative_path: "results/fdc_query.csv",
              role: "result",
              sha256: digest("query-table"),
              size_bytes: 480,
              lease_generation: 1,
              recorded_at: at(-7320),
              current: true,
            }],
            gaps: [],
          }],
          outputs: [{
            artifact_id: QUERY_ARTIFACT,
            role: "result",
            label: "Guarded query result",
            relative_path: "results/fdc_query.csv",
            media_type: "text/csv",
            size_bytes: 480,
            sha256: digest("query-table"),
            created_at: at(-7320),
            provenance_class: "vetted_tool_output",
          }],
          review: { state: "not_reviewed" },
          gaps: [],
        },
      ],
      edges: [
        {
          from: ANALYSIS_TASK,
          to: STEP_LINEAGE_TASK,
          output: "results/nutrient_summary.csv",
          digest: digest("analysis-table"),
          recorded_by: "render_request",
        },
        {
          from: QUERY_TASK,
          to: ANALYSIS_TASK,
          output: "results/fdc_query.csv",
          digest: digest("query-table"),
          recorded_by: "analysis_request",
        },
      ],
      sources: [{
        kind: "guarded_query",
        node: QUERY_TASK,
        source: "fooddata-central",
        profile: "reference-sql-fdc",
        schema: "fdc",
        snapshot: {
          dataset: "FoodData Central Foundation Foods",
          release: "April 2026",
          files: [{
            file_name: "FoodData_Central_foundation_food_csv_2026-04-30.zip",
            sha256: digest("fdc-file"),
          }],
          manifest_sha256: digest("fdc-manifest"),
        },
      }],
      // The broker deliberately exposes no identifier or edge for a task this reader cannot read.
      withheld: 1,
      limits: { max_depth: 16, max_nodes: 200, truncated: false, reason: null },
    },
  };

  const versionOne = (): Lineage => {
    const { attempts: _attempts, upstream: _upstream, schema: _schema, ...base } = lineage;
    return { ...base, schema: "enrichedlab.task-lineage.v1" };
  };

  async function handle(
    req: IncomingMessage,
    res: ServerResponse,
    url: URL,
    json: Json,
    denied: () => void,
  ): Promise<boolean> {
    const match = url.pathname.match(/^\/api\/v1\/tasks\/([0-9a-f-]{36})(\/.*)?$/);
    if (!match || match[1] !== STEP_LINEAGE_TASK) return false;
    const suffix = match[2] ?? "";
    if (!suffix && req.method === "GET") {
      json(task);
      return true;
    }
    if (suffix === "/events" && req.method === "GET") {
      const after = Number(url.searchParams.get("after") ?? 0);
      json({ events: events.filter((event) => event.sequence > after).slice(0, 200) });
      return true;
    }
    if (suffix === "/artifacts" && req.method === "GET") {
      json({ artifacts });
      return true;
    }
    if (suffix === "/lineage" && req.method === "GET") {
      json(url.searchParams.get("version") === "3" ? lineage : versionOne());
      return true;
    }
    // A recorded task without these records answers as the broker does: present and empty, not missing.
    const empty: Record<string, unknown> = {
      "/reviews": { reviews: [] },
      "/scientific-history": { coverage: "pending", attempt: null, lease_generation: null, events: [], next_cursor: null },
      "/generated-code": { generated_code: null },
      "/query": { query: null },
      "/figure": { figure: null },
      "/figures": { versions: [] },
      "/template-analysis": { template_analysis: null },
      "/template-analyses": { template_analyses: [] },
    };
    if (req.method === "GET" && suffix in empty) {
      json(empty[suffix]);
      return true;
    }
    const digestRoute = suffix.match(/^\/artifacts\/([0-9a-f-]{36})\/digest$/);
    if (digestRoute && req.method === "GET") {
      const artifact = artifacts.find((candidate) => candidate.id === digestRoute[1]);
      if (!artifact) denied();
      else json({ artifact_id: artifact.id, artifact_sha256: artifact.sha256, status: "not_recorded", digest_sha256: null, recorded_at: null, digest: null });
      return true;
    }
    const content = suffix.match(/^\/artifacts\/([0-9a-f-]{36})\/(contents(?:\/file)?|download)$/);
    if (!content || req.method !== "GET") return false;
    const artifact = artifacts.find((candidate) => candidate.id === content[1]);
    if (!artifact) {
      denied();
      return true;
    }
    const packaged = artifact.id === PACKAGE_ARTIFACT;
    if (content[2] === "contents") {
      if (!packaged) json({ error: { code: "artifact_not_a_package" } }, 415);
      else json({ artifact_id: artifact.id, sha256: artifact.sha256, size_bytes: artifact.size_bytes, listing: listing(pkg.files) });
      return true;
    }
    if (content[2] === "contents/file") {
      if (!packaged) {
        json({ error: { code: "artifact_not_a_package" } }, 415);
        return true;
      }
      const file = pkg.files.find((candidate) => candidate.path === url.searchParams.get("path"));
      if (!file) {
        denied();
        return true;
      }
      res.setHeader("Content-Type", listing([file]).members[0]?.media_type ?? "application/octet-stream");
      res.setHeader("Content-Length", file.bytes.length);
      res.setHeader("X-Artifact-Sha256", sha(file.bytes));
      res.end(file.bytes);
      return true;
    }
    const bytes = packaged ? pkg.archive : pkg.checksum;
    res.setHeader("Content-Type", artifact.media_type);
    res.setHeader("Content-Length", bytes.length);
    res.setHeader("X-Artifact-Sha256", artifact.sha256);
    res.end(bytes);
    return true;
  }

  return { task, events, entry, lineage, handle };
}
