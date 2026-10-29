// Projections of src/broker/model.rs. The broker owns permission and lifecycle decisions.
export interface Identity {
  principal: { id: string; email: string; display_name: string | null };
  groups: {
    key: string;
    name: string;
    role: string;
    max_active_agents: number;
    max_active_agents_per_principal: number;
    max_queued_tasks: number;
    retention_days: number;
    /** The enabled methods granted to this lab (broker migration 052); absent from older brokers. */
    profiles?: string[];
    /** Whether the lab holds the dataset method, so its projects take lab data (cluster's per-lab signal after shapes
     *  v12); absent reads as false. */
    lab_data?: boolean;
  }[];
  profiles: {
    key: string;
    name: string;
    description: string;
    tool_bundle: string;
    max_run_seconds: number;
    /** Whether a person may choose it to run or give to the agent; the figure method is listed for its name only.
     *  Absent from older brokers, where every method is offered. */
    offered?: boolean;
    /** Whether a research run may list it: the methods a person may run, and the reviewed R
     *  analyses the research lead chooses but a person asks for only from a result table. Absent from older brokers,
     *  where it follows `offered`. */
    in_runs?: boolean;
  }[];
  // Separately enrolled human capabilities; absent from older brokers.
  administration?: { project_inspection: boolean };
  // Names the site shows beside its labs, such as the research center; absent from older brokers.
  site?: { organization?: string };
  /** Whether the broker serves the reader's change stream (`GET /api/v1/changes/stream`). Absent
   *  from older brokers, which the workspace reads on a timer and never asks for the stream. */
  change_stream?: boolean;
}
export interface Task {
  id: string;
  group: string;
  profile: string;
  status: string;
  objective: string | null;
  attempt: number;
  max_task_attempts: number;
  max_provider_attempts: number;
  logical_agent: string | null;
  result_class: string | null;
  error_code: string | null;
  submitted_at: string;
  updated_at: string;
  started_at: string | null;
  finished_at: string | null;
}
export interface TaskEvent {
  sequence: number;
  kind: string;
  state: string;
  code: string;
  occurred_at: string;
}
export interface Artifact {
  id: string;
  role: string;
  label: string;
  relative_path: string;
  media_type: string;
  size_bytes: number;
  sha256: string;
  created_at: string;
  /** The registry kind once its validator passed, the check with its receipt, the
   *  same-values group and the views the method declared. Absent from older brokers. */
  kind?: { id: string; version: number } | null;
  validation?: (KindCheck & { receipt: string | null }) | null;
  same_values_group?: string | null;
  views?: { view: string; encoding?: Record<string, string> }[] | null;
}
export interface ScientificHistoryEvent {
  sequence: number;
  event_id: string;
  segment: number;
  harness: string;
  provider: string | null;
  model: string | null;
  engine: string | null;
  parser: string | null;
  kind: string;
  evidence_role: string;
  visibility: string;
  occurred_at: string;
  hash: string;
  evidence_count: number;
}
export interface ScientificHistoryPage {
  coverage: "available" | "pending" | "unavailable";
  attempt: number | null;
  lease_generation: number | null;
  events: ScientificHistoryEvent[];
  next_cursor: string | null;
}
export interface ScientificHistoryEvidence {
  artifact_id: string;
  sha256: string;
  size_bytes: number;
  media_type: string;
  role: string;
  relation: "observed_after";
}
export interface ScientificHistoryEvidencePage {
  attempt: number;
  lease_generation: number;
  evidence: ScientificHistoryEvidence[];
  next_cursor: string | null;
}
export interface Submission {
  project_id?: string;
  action_key?: string;
  parent_task_id?: string;
  group: string;
  profile: string;
  objective: string;
}
/** The methods a person may choose to run or to give the research agent; with a lab, only that lab's methods when the
 *  broker names them (older brokers name none, and every method is offered as before). */
export const offeredProfiles = (identity: Identity | null | undefined, lab?: string) => {
  const offered = (identity?.profiles ?? []).filter((p) => p.offered !== false);
  const granted = lab ? identity?.groups.find((g) => g.key === lab)?.profiles : undefined;
  return granted ? offered.filter((p) => granted.includes(p.key)) : offered;
};
/** The methods a person may give the research agent: those a run may list, in the lab's grants when the broker names
 *  them. */
export const runProfiles = (identity: Identity | null | undefined, lab?: string) => {
  const listed = (identity?.profiles ?? []).filter((p) => p.in_runs ?? p.offered !== false);
  const granted = lab ? identity?.groups.find((g) => g.key === lab)?.profiles : undefined;
  return granted ? listed.filter((p) => granted.includes(p.key)) : listed;
};
export const terminal = (status: string) =>
  ["completed", "failed", "cancelled", "review_required"].includes(status);
export const titleCase = (value: string) =>
  value.replaceAll("_", " ").replace(/^./, (c) => c.toUpperCase());
export function mergeEvents(
  previous: TaskEvent[],
  incoming: TaskEvent[],
): TaskEvent[] {
  const events = new Map(previous.map((event) => [event.sequence, event]));
  for (const event of incoming) events.set(event.sequence, event);
  return [...events.values()].sort((a, b) => a.sequence - b.sequence);
}

// Members of a retained tar+gzip package, each hashed by the broker from verified bytes.
export type PackageKind = "figure" | "table" | "code" | "log" | "record" | "text" | "checksum" | "data";
/** A file's check against its registry kind. A file that fails is kept as `file.opaque`
 *  and the check names the kind it was checked as and why it did not pass. */
export interface KindCheck {
  state: "passed" | "failed" | "not_checked";
  checked_as?: { id: string; version: number } | null;
  reason?: string | null;
}
export interface PackageMember {
  path: string;
  size_bytes: number;
  sha256: string;
  media_type: string;
  kind: PackageKind;
  manifest: "matched" | "mismatched" | "unlisted" | "not_applicable";
  /** What the method declared for this file (its member fields):
   *  its registry kind, the files that hold the same numbers, and the views that draw it. Absent from older brokers. */
  result_kind?: { id: string; version: number } | null;
  same_values_group?: string | null;
  views?: { view: string; encoding?: Record<string, string> }[] | null;
  validation?: KindCheck | null;
}
export interface PackageListing {
  format: string;
  checksum_file: string | null;
  members: PackageMember[];
  missing_from_archive: string[];
  skipped: { path: string; reason: string }[];
  expanded_bytes: number;
}
/** Answer documents: a workbook, a Word document or slides of a run's answer, written by code
 *  from the stored decision and the result tables it cites. */
export type DocumentKind = "xlsx" | "docx" | "pptx";
export interface DocumentFile { kind: DocumentKind; path: string; media_type: string; sha256: string; size_bytes: number }
export interface RunDocument {
  task_id: string;
  kind: DocumentKind;
  turn: number;
  status: string;
  created_at: string;
  finished_at: string | null;
  receipt: { status: "completed" | "refused" | "failed"; error_code: string | null; message: string | null;
    file: DocumentFile | null; package_sha256: string | null } | null;
}
export interface PackageContents {
  artifact_id: string;
  sha256: string;
  size_bytes: number;
  listing: PackageListing;
}
export const packageMediaTypes = ["application/gzip", "application/x-gzip", "application/x-tar+gzip", "application/x-compressed-tar"];
export const isPackage = (artifact: Artifact) => packageMediaTypes.includes(artifact.media_type.split(";")[0].trim().toLowerCase());

// Model-written R for a reviewed template, as the broker admitted and stored it.
export interface GeneratedCodeAdmission {
  schema: string;
  result: "accepted" | "rejected";
  template_ref: string;
  block_sha256: string;
  size_bytes: number;
  lines: number;
  reasons: string[];
  functions_used: string[];
  namespaces: string[];
  numeric_literals: number;
  string_literals: number;
  uses_random_numbers: boolean;
  /** Digest of the allowlist that decided admission; absent before positive admission. */
  allowlist?: string;
  scientific_validation: string;
}
/** A configured method's settings as the broker stored them at commit. The words come
 *  from the method's template row, never from the agent's reply. */
export interface CodeConfiguration {
  schema: string;
  block_sha256: string;
  settings: Record<string, string>;
  described: { setting: string; label: string; choice: string; choice_label: string }[];
}
export interface GeneratedCode {
  task_id: string;
  profile: string;
  template_ref: string;
  template_sha256: string;
  path: string;
  content: string;
  size_bytes: number;
  sha256: string;
  admission: GeneratedCodeAdmission;
  /** Present when a configured method rendered the block from settings; the agent wrote no code. */
  configuration?: CodeConfiguration | null;
  run_id: string;
  turn: number;
  created_at: string;
  validation: "unvalidated";
}

/** A database query the research agent wrote for a source's method, with its source as a scientist
 *  reads it and each attempt's receipt. `proposed_sql` is the agent's text, with the first
 *  argument of a two-argument round or trunc cast to numeric (the agent's reply itself stays kept
 *  by its hash); `admitted_sql` is what ran, the same text with the row cap added when the agent gave none. */
export interface QueryReceipt {
  attempt: number;
  status: "completed" | "refused" | "failed";
  error_code: string | null;
  row_count: number | null;
  columns: { name: string; type: string }[] | null;
  package_sha256: string | null;
  server_version: string | null;
  started_at: string;
  finished_at: string;
  /** The row a live lab database stated about its version in the query's own transaction (broker migration 053). */
  source_version?: Record<string, unknown> | null;
}
export interface TaskQuery {
  task_id: string;
  profile: string;
  source: string;
  title: string;
  description: string;
  release: string;
  snapshot: { manifest_sha256?: string; files?: { table: string; rows: number; sha256: string }[] } & Record<string, unknown>;
  /** Public reference data, or a live lab database (broker migration 053); absent from older brokers. */
  data_class?: "public_reference" | "lab_database";
  proposed_sql: string;
  admitted_sql: string;
  sql_sha256: string;
  guard: string;
  relations: string[];
  row_limit: number;
  run_id: string | null;
  turn: number | null;
  created_at: string;
  validation: "unvalidated";
  receipts: QueryReceipt[];
}

/** Where an analysis came from, from the broker's own records (`enrichedlab.task-lineage.v1`). */
export type LineageGap =
  | "origin_not_recorded" | "origin_before_records" | "not_in_project" | "plan_step_not_recorded"
  | "inputs_not_recorded" | "input_class_not_recorded" | "image_not_digest_pinned" | "no_outputs_recorded"
  | "code_not_recorded" | "method_recorded_later" | "method_not_recorded";
export type ToolClass = "agent_built" | "vetted_service" | "configured" | "agent_session" | "guarded_query" | "reviewed_query";
/** What kind of data a task read. */
export type InputClass = "lab_database" | "object_data" | "paper" | "public_reference" | "prior_output";
export interface Lineage {
  schema: "enrichedlab.task-lineage.v1";
  task: { task_id: string; project_id: string | null; action_key: string | null; objective: string; status: string;
    result_class: string | null; error_code: string | null; attempt: number; submitted_at: string;
    started_at: string | null; finished_at: string | null };
  /** Who asked, as recorded when the task was submitted. `unrecorded` is never filled in by guessing. */
  origin: { origin: "person" | "research_run" | "unrecorded";
    basis: "submission" | "turn_record" | "standalone_task" | "before_origin_records" | null;
    principal_id: string; principal_name: string | null; principal_kind: "person" | "service_account" | null;
    run_id?: string | null; turn?: number | null; recorded_at?: string };
  /** The step as first recorded for this task; `status` is its current status. A later revision that
   * changed its title or rationale is in `current`, never in place of the original. */
  plan_step: { action_key: string; title: string; status: string; rationale: string | null; revision?: number;
    added_after_submission?: boolean; current?: { title: string; rationale: string | null; revision: number } | null } | null;
  inputs: { workspace: { input_refs?: string[]; tool_refs?: string[]; source_digest?: string; image_digest?: string;
      profile?: string; preparation_receipt?: unknown } | null;
    checkpoint_revision: number | null;
    platform_tool: { tool_id: string; catalog_digest: string; input_sha256: string } | null;
    /** What kind of data the task read (migration 039): a query's source as registered, or the classes recorded when
     * the platform sealed the latest attempt's copies. Absent from brokers before 039. */
    classes?: InputClass[];
    classes_recorded_by?: "query_source" | "input_custodian" | "render_request" | "analysis_request" | "dataset_request" | null };
  /** The method the task was submitted under: `submission` when recorded with the task, `recorded_later`
   * for an earlier task bound from the catalog's dated records. */
  method: { profile: string; profile_name: string | null; tool_class: ToolClass; template_ref: string | null;
    template_sha256: string | null; recorded?: "submission" | "recorded_later" | null; harness: string | null;
    role: string | null; tool_bundle: string | null };
  code: { path: string; sha256: string; size_bytes: number; admission: GeneratedCodeAdmission;
    configuration?: CodeConfiguration | null; run_id: string; turn: number; created_at: string } | null;
  /** A database query's record; absent from brokers before migration 038. */
  query?: { source: string; sha256: string; guard: string; relations: string[]; row_limit: number; run_id: string | null;
    turn: number | null; created_at: string;
    /** The source as registered (migration 039); absent from brokers before 039. */
    title?: string; release?: string; snapshot_sha256?: string | null; data_class?: InputClass;
    /** A live source's recorded version, or a pinned snapshot's kind; absent from older brokers. */
    snapshot_kind?: string | null; source_version?: Record<string, unknown> | null } | null;
  /** A figure version's request; absent from brokers before migration 040. */
  render?: { source_task: string; table_path: string; table_sha256: string; source_package_sha256: string;
    feature: string; controls: FigureControls; title: string; previous: string | null; request_sha256: string;
    created_at: string; service_version: string | null; image_digest: string | null } | null;
  /** A template analysis's request: a reviewed R analysis template run on an earlier result's table.
   *  `method` is the template's own statement of what ran. Absent from older brokers. */
  analysis?: { source_task: string; table_path: string; table_sha256: string; source_package_sha256: string;
    feature: string; settings: Record<string, unknown>; title: string; previous: string | null; request_sha256: string;
    created_at: string; service_version: string | null; image_digest: string | null; method: string | null } | null;
  /** A lab dataset's intake or preparation: the person's file as the upload guard checked it and
   *  the broker's own code read it. Absent from older brokers. */
  dataset?: { dataset: string; operation: "intake" | "preparation"; file_name: string; file_sha256: string;
    guard_intake: string | null; guard_policy: string | null; grid_sha256: string | null; spec_sha256: string | null;
    previous: string | null; title: string | null; request_sha256: string; created_at: string; executor: string | null;
    executor_version: number | string | null; table_sha256: string | null } | null;
  execution: { image_ref: string | null; cpu_limit: string | null; memory_limit: string | null;
    max_run_seconds: number | null; attempts: number; scientific_history: number };
  outputs: { artifact_id: string; role: string; label: string; relative_path: string; media_type: string;
    size_bytes: number; sha256: string; created_at: string; provenance_class: string }[];
  review: { state: "unvalidated" | "not_reviewed" };
  /** Known codes are placed on their rows; a newer broker may add codes this page does not know. */
  gaps: (LineageGap | string)[];
}

/** One attempt in lineage version 2 or 3. Its fields are custody records, not a claim of scientific validation. */
export interface LineageAttempt {
  attempt: number;
  inputs: { input_ref: string; class: InputClass | string; sha256: string; size_bytes: number;
    custody_transaction: string; source: Record<string, unknown>; lease_generation: number; recorded_at: string }[];
  execution: { image_digest: string; template_ref: string | null; template_sha256: string | null;
    tool_ref: string | null; parameters_sha256: string; lease_generation: number; recorded_at: string } | null;
  outputs: { artifact_id: string; relative_path: string; role: string; sha256: string; size_bytes: number;
    lease_generation: number; recorded_at: string; current: boolean }[];
  validation: { state: string };
  gaps: string[];
}

export interface StepLineageEdge {
  from: string;
  to: string;
  output: string | null;
  digest: string | null;
  recorded_by: "analysis_request" | "render_request" | "dataset_request" | "input_custodian";
}

export interface StepLineageNode {
  task_id: string;
  title: string | null;
  method_class: ToolClass | string | null;
  method: Lineage["method"] & {
    tool?: Lineage["inputs"]["platform_tool"];
    query?: NonNullable<Lineage["query"]>;
    analysis?: NonNullable<Lineage["analysis"]>;
    render?: NonNullable<Lineage["render"]>;
    dataset?: NonNullable<Lineage["dataset"]>;
  };
  /** Version 3 deliberately keeps only execution, output registrations and gaps for an upstream attempt. */
  attempts: { attempt: number; execution: LineageAttempt["execution"]; outputs: LineageAttempt["outputs"]; gaps: string[] }[];
  outputs: Lineage["outputs"];
  review: Lineage["review"];
  gaps: string[];
}

export type StepLineageSource =
  | { kind: "guarded_query"; node: string; source: string; profile: string; schema: string;
      snapshot: Record<string, unknown> }
  | { kind: "lab_dataset"; node: string; dataset: string; file_name: string; sha256: string; size_bytes: number;
      guard: { decision: string | null; policy: string | null } }
  | { kind: "paper"; node: string; reference: string; paper?: string; lab?: string; document?: string;
      source_sha256?: string; sealed_sha256: string }
  | { kind: "sealed_input"; node: string; reference: string; class: InputClass | string; sealed_sha256: string;
      source_snapshot: Record<string, unknown> };

/** The broker's one-read drill-down (`GET /tasks/{task}/lineage?version=3`). */
export type StepLineage = Omit<Lineage, "schema"> & {
  schema: "enrichedlab.task-lineage.v3";
  attempts: LineageAttempt[];
  upstream: {
    nodes: StepLineageNode[];
    edges: StepLineageEdge[];
    sources: StepLineageSource[];
    /** Producing tasks this reader cannot read. Their identifiers and links are intentionally absent. */
    withheld: number;
    limits: { max_depth: number; max_nodes: number; truncated: boolean; reason: "depth" | "nodes" | null };
  };
};

/** A person's decision on an analysis (broker migration 023). The broker names the reviewer from the
 *  signed-in account and binds the checksum of every result file the analysis had then; `covers_current_outputs` is
 *  false once those files change. A review is a person's reading, not statistical validation. */
export type ReviewDecision = "accepted" | "needs_changes" | "rejected";
export interface TaskReview {
  review_id: string; task_id: string; attempt: number; decision: ReviewDecision; note: string | null;
  reviewer_id: string; reviewer_name: string | null;
  /** Migration 024 refuses service accounts; a row from before it may still name one. */
  reviewer_kind: "person" | "service_account";
  outputs: { artifact_id: string; relative_path: string; sha256: string }[];
  created_at: string; covers_current_outputs: boolean;
}

/** A note a lab lead or reviewer shared with the lab (broker migration 027). It binds the result files
 *  and the acceptances of the analyses it rests on; the broker decides its state by a fixed rule each time it is read. */
export type LabNoteState = "active" | "paused" | "expired" | "retired";
export interface LabNote {
  note_id: string; lab: string; project_id: string; project_title: string | null; revision: number;
  kind: "observation"; statement: string; statement_sha256: string;
  evidence: { task_id: string; refs: string[]; title: string | null; method: string;
    outputs: { artifact_id: string; relative_path: string; sha256: string }[]; review_id: string }[];
  methods: string[]; data: string[];
  shared_by: { principal_id: string; name: string | null; kind: "person" | "service_account" };
  reason: string | null; shared_at: string;
  /** `cause` names why a note is paused; `reason` is the retiring person's words. */
  status: { state: LabNoteState; review_by: string; cause?: "evidence_review_changed" | "evidence_files_changed";
    task_id?: string; retired_at?: string; reason?: string };
  events: { kind: "retired" | "renewed"; actor_name: string | null; reason: string | null; review_by: string | null; recorded_at: string }[];
}
export interface LabNotes { lab: string; can_curate: boolean; notes: LabNote[] }

// A lab's tools (broker migration 080). Every enabled method the lab may run, how it is granted (directly
// or through tool groups, named and versioned sets of methods), and its catalog fields: kind, template digest,
// configured setting and choice labels, the reviewed queries it offers and up to three accepted result examples the
// reader can open. Never SQL, inserted R text, rows or values.
export type ToolKind = "query_method" | "document_method" | "render_method" | "server_analysis" | "configured_method"
  | "template_analysis" | "sandbox_method";
export interface ToolParameter {
  name: string; description?: string; type?: "text" | "integer" | "number" | "enum"; required?: boolean;
  max_length?: number; minimum?: number; maximum?: number; choices?: string[];
}
export interface ToolColumn { name: string; type?: string; description?: string }
export interface ReviewedQuery {
  key: string; version: number; source: string; title?: string; purpose?: string; parameters: ToolParameter[];
  output_columns: ToolColumn[]; row_cap?: number; template_sha256?: string;
}
export interface ToolSetting { name: string; label?: string; choices: { key: string; label?: string }[] }
export interface LabTool {
  profile: string; name: string; description: string; route: string; kind: ToolKind; via_groups: string[];
  source?: string; template?: { ref: string; sha256: string };
  configuration?: { block_sha256?: string; settings: ToolSetting[] };
  reviewed_queries?: ReviewedQuery[];
  examples: { task: string; project: string; finished_at: string }[];
}
export interface ToolGroup { key: string; version: number; title: string; description: string; granted_at: string; tools: string[] }
export interface LabTools { lab: string; tool_groups: ToolGroup[]; tools: LabTool[] }

// Lab library. A paper's bytes and parse live in the lab's corpus of the document service; the broker
// keeps who added it, its catalog fields and counts from the parse, removals, typed labels and whether a lab curator
// accepted it for the research agent.
export type LibraryLabelNamespace = "topic" | "organism" | "food" | "compound" | "method" | "design" | "population";
export interface LibraryLabel { namespace: LibraryLabelNamespace; value: string; state: "set" | "proposed"; since: string }
export interface LibraryPaper {
  paper_id: string;
  lab: string;
  title: string | null;
  authors: string[];
  year: number | null;
  doi: string | null;
  page_count: number | null;
  byte_size: number;
  source_sha256: string;
  parse: {
    state?: string | null; severity?: string | null; chunks?: number | null; figures?: number | null; index?: string | null;
    // The upload check's record for a paper added since it began: its intake, policy, the rebuilt
    // copy the library keeps, and the finding codes it noted.
    guard?: { intake?: string; policy?: string; rebuilt_sha256?: string; findings?: string[] } | null;
  };
  added_by: { name: string | null; kind: "person" | "service_account"; you: boolean };
  added_at: string;
  labels: LibraryLabel[];
  removed: { removed_at: string; reason: string; by: string | null } | null;
  acceptance: { accepted_at: string; by: string | null } | null;
  can_remove: boolean;
  can_accept: boolean;
}
export interface Library { lab: string; role: "researcher" | "lead" | "reviewer"; papers: LibraryPaper[]; removed: number; configured: boolean }
export interface LibraryOutline {
  abstract: string | null;
  journal: string | null;
  figures: { label: string | null; caption: string | null; page: number | null }[];
  figures_total: number | null;
}
export interface LibraryPaperView { paper: LibraryPaper; outline: LibraryOutline | null; outline_error: string | null }
export interface LibraryPassage {
  paper_id: string;
  title: string | null;
  year: number | null;
  authors: string[] | null;
  section: string | null;
  heading_path: string[];
  pages: string | null;
  kind: string | null;
  text: string;
}
export interface LibrarySearch { query: string; ranking: "words"; results: LibraryPassage[]; left_out: number }
export interface LibraryAdded { paper: LibraryPaper; created: boolean; restored: boolean }
/** One library passage the research lead read, as its decision recorded it. */
// A passage is recorded by its chunk, or, when the lab's knowledge search found it, by its section's record and key.
export interface PassageRead {
  paper_id: string; chunk_id?: string; section_id?: string; section_key?: string; pages?: string | null; text_sha256: string;
  /** A passage of the commons literature: its paper is a Commons record, named by `title`. */
  origin?: "commons"; title?: string;
}
export interface LiteratureRead {
  status: "shown" | "none_fit" | "no_match" | "not_configured" | "not_member" | "unavailable";
  query_sha256?: string; passages?: PassageRead[]; left_out?: number; code?: string; via?: "knowledge"; other_records?: number;
}

/** A setting a plot feature declares in the render service's catalog. The service checks values and
 *  clamps numbers; the broker checks names and kinds. */
export interface FigureParam {
  type: "column" | "columns" | "enum" | "number" | "numbers" | "string" | "colors";
  required?: boolean; description?: string; default?: unknown; choices?: (string | number)[];
  min?: number; max?: number; maxlen?: number;
  /** For `colors`: the column setting whose values are the groups that take a colour. */
  of?: string;
}
export interface FigureFeature { id: string; label: string; description: string; params: Record<string, FigureParam> }
/** The plot features the running render service declares, as the worker last read them; null before its first read. */
export interface FigureCatalog { catalog_sha256: string; service_version: string; image_digest: string | null;
  features: FigureFeature[]; seen_at: string;
  /** The reader's labs whose figure method is enabled; figures are offered only in their projects. */
  groups: string[] }
/** A `colors` setting maps each group, a value of the grouping column, to a colour written as #RRGGBB. */
export type FigureControls = Record<string, string | number | boolean | (string | number)[] | Record<string, string>>;
/** One attempt's outcome: a completed one names the figure files; any other a fixed code and, when the service
 *  refused a setting, its own words. `notes` say what the service changed or left out. */
export interface FigureReceipt {
  attempt: number; status: "completed" | "refused" | "failed"; error_code: string | null; message: string | null;
  service_version: string | null; image_digest: string | null;
  files: { path: string; sha256: string; size_bytes: number; media_type: string }[] | null;
  notes: string[]; package_sha256: string | null; started_at: string; finished_at: string;
}
/** A figure version (broker migration 040): the request, its source analysis and every receipt. */
export interface FigureVersion {
  task_id: string; project_id: string; source_task: string; source_title: string | null; source_package_sha256: string;
  table_path: string; table_sha256: string; table_rows: number; table_columns: string[]; feature: string;
  controls: FigureControls; title: string; previous: string | null; catalog_sha256: string; request_sha256: string;
  created_at: string; receipts: FigureReceipt[];
  /** The figure's version marked for the manuscript; absent from older brokers. */
  manuscript?: ManuscriptMark | null;
  /** The figure's first version, which names the figure; absent from older brokers. */
  figure?: string;
}
export interface FigureVersionSummary {
  task_id: string; table_path: string; feature: string; title: string; previous: string | null; status: string;
  created_at: string; finished_at: string | null;
  manuscript?: ManuscriptMark | null;
  figure?: string;
}
/** One version per figure is marked as the one in the manuscript; a figure is named by its first version. */
export interface ManuscriptMark { figure?: string; version: string; marked_by: string; marked_by_name?: string | null; marked_at: string }
/** An R drawing asked for without making a version: no version, task, package
 *  or history entry. A compute worker draws it and the broker holds the drawing only until it is handed over. When the
 *  render service refuses or cannot draw the settings, `refused` holds the broker's words and there is no drawing;
 *  `refusal` names a known refusal's code, columns and counts, for pointing at a setting only. */
export interface FigurePreview {
  preview: { media_type: string; sha256: string; size_bytes: number; svg_base64: string } | null;
  refused: string | null; refusal?: { code: string; columns: string[]; counts: number[] } | null;
  notes: string[]; settings_used: Record<string, unknown>;
  service_version: string | null; image_digest: string | null; catalog_sha256: string; table_sha256: string;
}
/** A commons record as the workspace shows it: the knowledge service's fields the broker passes on. */
export type CommonsKind = "dataset" | "paper" | "tool" | "object_collection";
export interface CommonsRecord {
  resource_id: string; kind: CommonsKind; scope: string; baseline: boolean; title: string; summary: string | null;
  data_class: string; rights: string; lifecycle: string; source_uri: string | null; created_at: string;
  /** A paper's active sections, figures and tables that wait for a mark while the paper is marked. */
  parts_waiting?: number;
}
/** One page of the commons records of one kind, the reader's role in the commons, and whether to offer the mark.
 *  `next` continues the listing; `complete` is true on the last page. */
export interface CommonsView {
  scope: string; kind: CommonsKind; records: CommonsRecord[]; complete: boolean; next: string | null;
  role: "member" | "curator" | "lead" | null; can_mark: boolean;
}
/** A record's mark as the service stored it, with how many of a paper's parts changed with it. */
export type CommonsMarked = CommonsRecord & { parts_changed: number };
/** A commons collection: the records whose source addresses share a prefix, such as `literature://papers/`. */
export interface CommonsCollection {
  prefix: string; kinds: Record<string, { active: number; marked: number }>; parts_waiting: number;
}
export interface CommonsCollections {
  scope: string; collections: CommonsCollection[]; role: "member" | "curator" | "lead" | null; can_mark: boolean;
}
export interface CollectionMarked { prefix: string; baseline: boolean; records_changed: number; parts_changed: number }
/** A link the knowledge service proposed from a lab's record, such as a paper that names one of the lab's tools. */
export interface LabLink {
  relation_id: string; kind: string; created_at: string;
  source: { id: string; kind: string; title: string };
  target: { id: string; kind: string; title: string };
  terms: string[]; sections: number | null;
}
/** A lab's proposed links and whether the reader may review them. */
export interface LabLinkQueue {
  lab: string; proposals: LabLink[]; complete: boolean;
  role: "member" | "curator" | "lead" | null; can_review: boolean;
}
