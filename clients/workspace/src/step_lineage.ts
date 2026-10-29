import type {
  InputClass,
  StepLineage,
  StepLineageEdge,
  StepLineageNode,
  StepLineageSource,
} from "./contracts.ts";

// The version 3 response is a bounded graph. This module turns it into a disclosure tree without
// filling gaps: a repeated node is named once, a cycle stops where it returns, and an unreadable
// producer remains only the aggregate refusal the broker returned.

export interface StepLineageMethodView {
  name: string;
  version: string | null;
  receipt: string;
}

export interface StepLineageDetail {
  label: string;
  value: string;
}

export interface StepLineageItem {
  key: string;
  type: "step" | "source" | "cycle" | "missing" | "limit";
  title: string;
  kind: string;
  path: string[];
  method: StepLineageMethodView | null;
  notes: string[];
  technical: StepLineageDetail[];
  inputs: StepLineageItem[];
}

export interface StepLineageView {
  root: string;
  inputs: StepLineageItem[];
  empty: string | null;
  incomplete: string | null;
  rootGaps: string[];
  rootTechnical: StepLineageDetail[];
}

const MAX_SHAPED_DEPTH = 16;
const MAX_SHAPED_ITEMS = 400;

const clean = (value: unknown): string => typeof value === "string" ? value.trim() : "";
const text = (record: Record<string, unknown>, ...keys: string[]): string => {
  for (const key of keys) {
    const value = record[key];
    if (typeof value === "string" && value.trim()) return value.trim();
    if (typeof value === "number" && Number.isFinite(value)) return String(value);
  }
  return "";
};
const unique = (items: string[]) => [...new Set(items.filter(Boolean))];
const detail = (items: StepLineageDetail[], label: string, value: unknown) => {
  const shown = typeof value === "number" ? String(value) : clean(value);
  if (shown) items.push({ label, value: shown });
};

const GAP_WORDS: Record<string, string> = {
  origin_not_recorded: "Who asked for this step was not recorded.",
  origin_before_records: "This step ran before the record kept who asked for it.",
  not_in_project: "No project is recorded for this step.",
  plan_step_not_recorded: "No project-plan step is recorded for this step.",
  inputs_not_recorded: "The selected inputs were not recorded.",
  input_class_not_recorded: "The kind of input was not recorded.",
  image_not_digest_pinned: "The exact build that ran this step was not recorded.",
  no_outputs_recorded: "No output files were recorded.",
  code_not_recorded: "No admitted code was recorded.",
  method_recorded_later: "The method name was recorded later from the dated catalog.",
  method_not_recorded: "The method was not recorded.",
  attempt_inputs_not_recorded: "The copies sealed for this attempt were not recorded.",
  attempt_execution_not_recorded: "What the worker bound for this attempt was not recorded.",
  attempt_outputs_not_linked: "The attempt's output registrations are not linked here.",
};

function gapWords(gaps: readonly string[], attempt?: number): string[] {
  const prefix = attempt === undefined ? "" : `Attempt ${attempt}: `;
  return unique(gaps.map((gap) => `${prefix}${GAP_WORDS[gap]
    ?? "The record names another missing part that this page does not describe yet."}`));
}

function attemptGapWords(attempts: readonly { attempt: number; gaps: string[] }[]): string[] {
  return attempts.flatMap((attempt) => gapWords(attempt.gaps, attempt.attempt));
}

function nodeGapWords(node: StepLineageNode): string[] {
  return unique([...gapWords(node.gaps), ...attemptGapWords(node.attempts)]);
}

function nodeTitle(node: StepLineageNode): string {
  return clean(node.title) || clean(node.method?.profile_name) || "Unnamed recorded step";
}

function nodeKind(node: StepLineageNode): string {
  if (node.method?.render) return "Figure";
  if (node.method?.analysis) return "Reviewed analysis";
  if (node.method?.dataset?.operation === "preparation") return "Data preparation";
  if (node.method?.dataset?.operation === "intake") return "Uploaded file reading";
  if (node.method?.query || node.method_class === "guarded_query") return "Database query";
  return {
    vetted_service: "Reviewed method",
    configured: "Configured method",
    agent_built: "Model-written analysis",
    agent_session: "Research step",
  }[node.method_class ?? ""] ?? "Recorded step";
}

function methodView(node: StepLineageNode): StepLineageMethodView {
  const method = node.method ?? ({} as StepLineageNode["method"]);
  const service = method.render ?? method.analysis;
  const dataset = method.dataset;
  const query = method.query;
  const name = clean(dataset?.executor)
    || clean(method.analysis?.method)
    || clean(method.profile_name)
    || (dataset?.operation === "preparation" ? "Reviewed data preparation" : "Method name not recorded");
  const version = clean(service?.service_version)
    || (dataset?.executor_version === null || dataset?.executor_version === undefined ? "" : String(dataset.executor_version))
    || clean(query?.guard)
    || null;
  const receiptFields = !!(
    clean(service?.service_version) || clean(service?.image_digest) || clean(method.analysis?.method)
    || clean(dataset?.executor) || dataset?.executor_version !== null && dataset?.executor_version !== undefined
    || clean(dataset?.table_sha256) || !!query?.source_version
  );
  const registeredOutputs = node.outputs.length > 0 || node.attempts.some((attempt) => attempt.outputs.length > 0);
  const receipt = receiptFields
    ? `Receipt details are recorded${registeredOutputs ? " with registered output files" : ""}; its outcome is not included in this lineage response.`
    : registeredOutputs
      ? "Output registrations are recorded, but the method receipt state is not included in this lineage response."
      : "The method receipt state is not recorded in this lineage response.";
  return { name, version, receipt };
}

function nodeTechnical(node: StepLineageNode, edge: StepLineageEdge): StepLineageDetail[] {
  const out: StepLineageDetail[] = [];
  detail(out, "Step", node.task_id);
  detail(out, "Input file", edge.output);
  detail(out, "Input SHA-256", edge.digest);
  detail(out, "Recorded link", edge.recorded_by);
  detail(out, "Method", node.method?.profile);
  detail(out, "Template", node.method?.template_ref);
  detail(out, "Template SHA-256", node.method?.template_sha256);
  detail(out, "Tool", node.method?.tool?.tool_id);
  detail(out, "Tool catalog", node.method?.tool?.catalog_digest);
  for (const attempt of node.attempts) {
    const prefix = `Attempt ${attempt.attempt}`;
    detail(out, `${prefix} image`, attempt.execution?.image_digest);
    detail(out, `${prefix} template`, attempt.execution?.template_ref);
    detail(out, `${prefix} template SHA-256`, attempt.execution?.template_sha256);
    detail(out, `${prefix} tool`, attempt.execution?.tool_ref);
    detail(out, `${prefix} parameters SHA-256`, attempt.execution?.parameters_sha256);
    for (const output of attempt.outputs) {
      detail(out, `${prefix} output`, output.relative_path);
      detail(out, `${prefix} output SHA-256`, output.sha256);
    }
    if (attempt.gaps.length)
      out.push({ label: `${prefix} missing-part codes`, value: attempt.gaps.join(", ") });
  }
  if (node.gaps.length) out.push({ label: "Missing-part codes", value: node.gaps.join(", ") });
  return out;
}

export function lineageCoverageNotice(lineage: StepLineage): string | null {
  const parts: string[] = [];
  const withheld = lineage.upstream.withheld;
  if (withheld > 0) {
    // `upstream.withheld` counts recorded input rows whose producing task this account cannot read. Several rows can
    // share one producer, so the words count inputs and never steps.
    parts.push(withheld === 1
      ? "1 recorded input comes from a step this account cannot read. Where it attaches is not shown."
      : `${withheld} recorded inputs come from one or more steps this account cannot read. Where they attach is not shown.`);
  }
  const limits = lineage.upstream.limits;
  if (limits.truncated) {
    parts.push(limits.reason === "depth"
      ? `The response reached its depth limit of ${limits.max_depth}.`
      : limits.reason === "nodes"
        ? `The response reached its node limit of ${limits.max_nodes} steps.`
        : "The response reached an unnamed lineage limit.");
  }
  return parts.length ? `Earlier history is not fully shown. ${parts.join(" ")}` : null;
}

function snapshotFiles(snapshot: Record<string, unknown>): string[] {
  const direct = text(snapshot, "file_name", "source_file", "filename");
  const listed = Array.isArray(snapshot.files) ? snapshot.files.flatMap((entry) => {
    if (typeof entry === "string") return entry.trim() ? [entry.trim()] : [];
    if (!entry || typeof entry !== "object" || Array.isArray(entry)) return [];
    const value = text(entry as Record<string, unknown>, "file_name", "source_file", "filename", "name", "path");
    return value ? [value] : [];
  }) : [];
  return unique([direct, ...listed]);
}

function snapshotTitle(snapshot: Record<string, unknown>): string {
  return text(snapshot, "title", "dataset_name", "dataset", "source_name", "name");
}

function snapshotNotes(snapshot: Record<string, unknown>): string[] {
  const release = text(snapshot, "release", "release_date");
  const version = text(snapshot, "source_version", "snapshot", "version");
  const files = snapshotFiles(snapshot);
  return unique([
    release ? `Release ${release}.` : "",
    ...files.map((file) => `Source file ${file}.`),
    !release && version ? `Snapshot ${version}.` : "",
  ]);
}

function sourceVersionNote(version: Record<string, unknown> | null | undefined): string {
  return version ? "The query receipt records the live source version used." : "";
}

function sourceItem(source: StepLineageSource, key: string, path: string[], node?: StepLineageNode): StepLineageItem {
  const technical: StepLineageDetail[] = [];
  let title = "Recorded source";
  let kind = "Terminal source";
  let notes: string[] = [];
  detail(technical, "Source kind", source.kind);
  detail(technical, "Used by step", source.node);

  if (source.kind === "guarded_query") {
    const query = node?.method.query;
    const live = source.snapshot.kind === "live" || query?.snapshot_kind === "live" || query?.data_class === "lab_database";
    title = snapshotTitle(source.snapshot) || clean(query?.title) || (live ? "Lab source snapshot" : "Reference source snapshot");
    kind = live ? "Live lab source" : "Reference source";
    notes = unique([
      ...snapshotNotes(source.snapshot),
      !text(source.snapshot, "release", "release_date") && clean(query?.release) ? `Release ${clean(query?.release)}.` : "",
      live ? sourceVersionNote(query?.source_version) : "",
    ]);
    if (!notes.length) notes.push("The source snapshot is recorded, but its release and source file are not named here.");
    detail(technical, "Source", source.source);
    detail(technical, "Method", source.profile);
    detail(technical, "Database schema", source.schema);
    if (query?.source_version) technical.push({ label: "Source version", value: JSON.stringify(query.source_version) });
    technical.push({ label: "Snapshot record", value: JSON.stringify(source.snapshot) });
  } else if (source.kind === "lab_dataset") {
    title = clean(source.file_name) || "Uploaded dataset";
    kind = "Uploaded dataset";
    notes = source.guard.decision
      ? [`The upload guard recorded its decision as ${source.guard.decision.replaceAll("_", " ")}.`]
      : ["The upload guard's decision is not recorded here."];
    detail(technical, "Dataset", source.dataset);
    detail(technical, "File SHA-256", source.sha256);
    detail(technical, "Size in bytes", source.size_bytes);
    detail(technical, "Upload policy", source.guard.policy);
  } else if (source.kind === "paper") {
    title = "Paper from the lab's library";
    kind = "Paper source";
    notes = ["The lineage response records the paper's identity, not its text."];
    detail(technical, "Reference", source.reference);
    detail(technical, "Paper", source.paper);
    detail(technical, "Lab", source.lab);
    detail(technical, "Document", source.document);
    detail(technical, "Source SHA-256", source.source_sha256);
    detail(technical, "Sealed SHA-256", source.sealed_sha256);
  } else {
    const snapshot = source.source_snapshot ?? {};
    const className = source.class as InputClass | string;
    title = snapshotTitle(snapshot) || ({
      lab_database: "Lab source snapshot",
      public_reference: "Public reference snapshot",
      object_data: "File snapshot",
      prior_output: "Earlier recorded output",
      paper: "Paper source",
    }[className] ?? "Sealed source snapshot");
    kind = ({
      lab_database: "Live lab source",
      public_reference: "Reference source",
      object_data: "Stored file source",
      prior_output: "Earlier output",
      paper: "Paper source",
    }[className] ?? "Sealed source");
    notes = snapshotNotes(snapshot);
    if (!notes.length) notes.push("The source snapshot is recorded; no further plain-language description is held here.");
    detail(technical, "Reference", source.reference);
    detail(technical, "Input class", source.class);
    detail(technical, "Sealed SHA-256", source.sealed_sha256);
    technical.push({ label: "Snapshot record", value: JSON.stringify(snapshot) });
  }

  return { key, type: "source", title, kind, path: [...path, title], method: null, notes, technical, inputs: [] };
}

export function shapeStepLineage(lineage: StepLineage): StepLineageView {
  const nodes = new Map(lineage.upstream.nodes.map((node) => [node.task_id, node]));
  const incoming = new Map<string, { edge: StepLineageEdge; index: number }[]>();
  lineage.upstream.edges.forEach((edge, index) => incoming.set(edge.to, [...(incoming.get(edge.to) ?? []), { edge, index }]));
  const terminals = new Map<string, { source: StepLineageSource; index: number }[]>();
  lineage.upstream.sources.forEach((source, index) => terminals.set(source.node, [...(terminals.get(source.node) ?? []), { source, index }]));

  const rootId = lineage.task.task_id;
  const root = nodes.get(rootId);
  const rootTitle = root ? nodeTitle(root) : clean(lineage.task.objective) || "This result";
  const expanded = new Set<string>([rootId]);
  const depthLimit = Math.min(MAX_SHAPED_DEPTH, Math.max(0, Math.floor(lineage.upstream.limits.max_depth)));
  let itemCount = 0;
  let clientLimited = false;
  const leafNotes: string[][] = [];

  const makeLimit = (path: string[], key: string, words: string): StepLineageItem => ({
    key, type: "limit", title: "More recorded inputs", kind: "Display limit", path,
    method: null, notes: [words], technical: [], inputs: [],
  });

  const buildInputs = (consumer: string, ancestors: readonly string[], path: string[], depth: number): StepLineageItem[] => {
    const items: StepLineageItem[] = [];
    for (const { edge, index } of incoming.get(consumer) ?? []) {
      if (itemCount >= MAX_SHAPED_ITEMS) {
        clientLimited = true;
        items.push(makeLimit(path, `item-limit-${consumer}-${index}`, "More recorded inputs are present, but this page stopped at its display bound."));
        break;
      }
      itemCount++;
      const producer = nodes.get(edge.from);
      if (!producer) {
        const title = "Recorded step not included";
        const technical: StepLineageDetail[] = [];
        detail(technical, "Producing step", edge.from);
        detail(technical, "Input file", edge.output);
        detail(technical, "Input SHA-256", edge.digest);
        items.push({ key: `missing-${index}`, type: "missing", title, kind: "Missing record", path: [...path, title], method: null,
          notes: ["This input points to a producing step that is not included in the lineage response."], technical, inputs: [] });
        continue;
      }

      const title = nodeTitle(producer);
      const nextPath = [...path, title];
      if (ancestors.includes(producer.task_id)) {
        items.push({ key: `cycle-${index}`, type: "cycle", title, kind: "Recorded cycle", path: nextPath, method: methodView(producer),
          notes: [...nodeGapWords(producer), "This recorded link returns to a step already on this path, so the path stops here."],
          technical: nodeTechnical(producer, edge), inputs: [] });
        continue;
      }
      if (expanded.has(producer.task_id)) {
        items.push({ key: `repeat-${index}`, type: "step", title, kind: nodeKind(producer), path: nextPath, method: methodView(producer),
          notes: [...nodeGapWords(producer), "This step is already shown on an earlier path, so its inputs are not repeated here."],
          technical: nodeTechnical(producer, edge), inputs: [] });
        continue;
      }
      if (depth + 1 > depthLimit) {
        clientLimited = true;
        items.push(makeLimit(nextPath, `depth-limit-${index}`, `This page stops at ${depthLimit} recorded steps from the result.`));
        continue;
      }

      expanded.add(producer.task_id);
      const children = buildInputs(producer.task_id, [...ancestors, producer.task_id], nextPath, depth + 1);
      const notes = nodeGapWords(producer);
      if (!children.length) leafNotes.push(notes);
      items.push({ key: `step-${index}`, type: "step", title, kind: nodeKind(producer), path: nextPath, method: methodView(producer),
        notes, technical: nodeTechnical(producer, edge), inputs: children });
    }

    for (const { source, index } of terminals.get(consumer) ?? []) {
      if (itemCount >= MAX_SHAPED_ITEMS) {
        clientLimited = true;
        if (!items.some((item) => item.type === "limit"))
          items.push(makeLimit(path, `source-limit-${consumer}-${index}`, "More recorded sources are present, but this page stopped at its display bound."));
        break;
      }
      itemCount++;
      items.push(sourceItem(source, `source-${consumer}-${index}`, path, nodes.get(consumer)));
    }
    return items;
  };

  const inputs = buildInputs(rootId, [rootId], [rootTitle], 0);
  const incomplete = unique([
    lineageCoverageNotice(lineage) ?? "",
    clientLimited ? "Some recorded inputs are not shown because this page reached its own display limit." : "",
  ]).join(" ") || null;
  const leafWords = incomplete
    ? "No earlier input or terminal source is shown for this step."
    : "No earlier input or terminal source is recorded for this step.";
  leafNotes.forEach((notes) => notes.push(leafWords));
  const rootGaps = attemptGapWords(lineage.attempts);
  const rootTechnical = lineage.attempts.flatMap((attempt) => attempt.gaps.length
    ? [{ label: `Attempt ${attempt.attempt} missing-part codes`, value: attempt.gaps.join(", ") }]
    : []);
  return {
    root: rootTitle,
    inputs,
    empty: inputs.length ? null : leafWords,
    incomplete,
    rootGaps,
    rootTechnical,
  };
}
