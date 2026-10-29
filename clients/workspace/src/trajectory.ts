import type { Artifact } from "./contracts.ts";

export interface ResearchStep {
  id: string;
  sourceId: string | null;
  parentSourceId: string | null;
  runId: string;
  actor: string;
  segment: number;
  sequence: number;
  title: string;
  kind: string;
  state: "running" | "finished" | "failed";
  occurredAt: string;
  metadata: Record<string, unknown>;
}
export interface Trajectory {
  steps: ResearchStep[];
  events: number;
  truncated: false;
}
// Browser artifact verification bounds a provenance export at 32 MiB. Keep that
// finite contract and parse every retained event within it; never silently hide
// later steps behind a UI-only event count.
export const MAX_TRAJECTORY_BYTES = 32 * 1024 * 1024;
const boundedString = (value: unknown, limit = 512): string | null =>
  typeof value === "string" && value.length > 0 && value.length <= limit ? value : null;
const object = (value: unknown): value is Record<string, unknown> => !!value && typeof value === "object" && !Array.isArray(value);

// This reads the controller's exported StoredEvent contract. Broker lifecycle
// events are deliberately not presented as scientific tool steps. Restricted
// provider content is never copied into this projection.
export function parseTrajectory(text: string): Trajectory {
  if (new TextEncoder().encode(text).byteLength > MAX_TRAJECTORY_BYTES)
    throw new Error("trajectory_oversize");
  const lines = text.split("\n").filter((line) => line.trim());
  const steps = new Map<string, ResearchStep>();
  let lastSequence = 0, runId: string | null = null;
  for (const line of lines) {
    let event: unknown;
    try { event = JSON.parse(line); } catch { throw new Error("trajectory_invalid"); }
    if (!object(event) || !Number.isSafeInteger(event.sequence) || Number(event.sequence) <= lastSequence ||
        !boundedString(event.event_id) || !boundedString(event.run_id) || !boundedString(event.actor) ||
        !boundedString(event.kind) || !Number.isSafeInteger(event.segment) || Number(event.segment) < 0 ||
        !boundedString(event.occurred_at) || !Number.isFinite(Date.parse(String(event.occurred_at))) || !object(event.metadata))
      throw new Error("trajectory_invalid");
    if (runId !== null && runId !== event.run_id) throw new Error("trajectory_run_mismatch");
    runId = String(event.run_id); lastSequence = Number(event.sequence);
    const kind = String(event.kind);
    if (!/(^|[.])tool[.]/.test(kind)) continue;
    const sourceId = boundedString(event.source_event_id);
    const callHash = boundedString(event.metadata.call_id_sha256, 64);
    const key = JSON.stringify([runId, event.actor, event.segment, event.harness, event.session_id, sourceId ?? callHash ?? event.event_id]);
    const previous = steps.get(key);
    const metadata = { ...previous?.metadata, ...event.metadata };
    const failed = /(?:failed|error)$/.test(kind) || metadata.status === "failed" || metadata.is_error === true ||
      (typeof metadata.exit_code === "number" && metadata.exit_code !== 0);
    const state = failed ? "failed" : /(?:finished|completed|result)$/.test(kind) ? "finished" : "running";
    steps.set(key, {
      id: previous?.id ?? String(event.event_id), sourceId,
      parentSourceId: boundedString(event.parent_source_event_id), runId,
      actor: String(event.actor), segment: Number(event.segment), sequence: previous?.sequence ?? Number(event.sequence),
      title: boundedString(metadata.label, 240) ?? boundedString(metadata.tool, 240) ?? "Recorded tool step",
      kind, state, occurredAt: String(event.occurred_at), metadata,
    });
  }
  return { steps: [...steps.values()].sort((a, b) => a.sequence - b.sequence), events: lines.length, truncated: false };
}

export type StepMaterial = "input" | "output" | "method" | "logs";
const fields: Record<StepMaterial, string[]> = {
  input: ["input_sha256", "input_sha256s"],
  output: ["output_sha256", "output_sha256s", "artifact_sha256"],
  method: ["code_sha256", "parameters_sha256", "query_sha256", "source_diff_sha256"],
  logs: ["stdout_sha256", "stderr_sha256", "log_sha256"],
};
export function stepArtifacts(step: ResearchStep, kind: StepMaterial, authorized: Artifact[]): Artifact[] {
  const hashes = new Set<string>();
  for (const field of fields[kind]) {
    const value = step.metadata[field];
    for (const item of Array.isArray(value) ? value : [value]) {
      if (typeof item !== "string") continue;
      const hash = item.replace(/^sha256:/, "");
      if (/^[a-f0-9]{64}$/.test(hash)) hashes.add(hash);
    }
  }
  return authorized.filter((artifact) => hashes.has(artifact.sha256));
}

export function trajectoryArtifacts(artifacts: Artifact[]): Artifact[] {
  return artifacts.filter((artifact) => artifact.role === "evidence" && /\/provenance-export\/events\.jsonl$/.test(artifact.relative_path))
    .sort((a, b) => b.created_at.localeCompare(a.created_at));
}

export function relatedSteps(step: ResearchStep, material: StepMaterial, steps: ResearchStep[], authorized: Artifact[]): ResearchStep[] {
  if (material !== "input" && material !== "output") return [];
  const hashes = new Set(stepArtifacts(step, material, authorized).map((file) => file.sha256));
  return steps.filter((other) => other.id !== step.id && stepArtifacts(other, material === "input" ? "output" : "input", authorized)
    .some((file) => hashes.has(file.sha256)));
}
