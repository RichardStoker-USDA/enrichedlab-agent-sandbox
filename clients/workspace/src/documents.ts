// Answer documents: what a person can take away from a run's answer, and how each one stands.
// Code writes them from the stored decision and the result tables it cites; a person still has to accept the answer.
import type { DocumentKind, RunDocument } from "./contracts.ts";

export const documentKinds: { kind: DocumentKind; label: string }[] = [
  { kind: "docx", label: "Word document" },
  { kind: "xlsx", label: "Excel workbook" },
  { kind: "pptx", label: "PowerPoint slides" },
];

export const kindLabel = (kind: DocumentKind) => documentKinds.find((k) => k.kind === kind)?.label ?? kind;

// The broker's refusals of a request that the person can act on, in plain words; any other refusal is a fault.
const REFUSALS: Record<string, string> = {
  document_method_unavailable: "This lab has no document method yet.",
  document_run_not_ended: "The agent's newest step did not end on this answer, so it cannot be written down.",
  document_source_unknown: "A result this answer cites is no longer available, so it cannot be written down.",
  document_sources_too_many: "This answer cites more result tables than one document holds.",
};
export const documentRefusal = (code: string): string | null => REFUSALS[code] ?? null;

/** A run's answer can be written down once the run stopped on it: for a person's review, or paused. */
export const canWriteDown = (status: string | undefined) => status === "review_required" || status === "paused";

export type DocumentState =
  | { state: "writing" }
  | { state: "ready"; file: NonNullable<NonNullable<RunDocument["receipt"]>["file"]> }
  | { state: "stopped"; words: string };

/** How a document stands, from its task and newest receipt. */
export function documentState(document: RunDocument): DocumentState {
  const receipt = document.receipt;
  if (receipt?.status === "completed" && receipt.file) return { state: "ready", file: receipt.file };
  if (receipt) return { state: "stopped", words: receipt.message ?? "It could not be written." };
  if (["failed", "cancelled"].includes(document.status)) return { state: "stopped", words: "It could not be written." };
  return { state: "writing" };
}

/** True while any document is still being written, so the list looks again soon. */
export const anyWriting = (documents: RunDocument[]) => documents.some((d) => documentState(d).state === "writing");

/** A file name from the project's title: lower case words joined by hyphens, then the turn and the kind. */
export function documentName(title: string, document: Pick<RunDocument, "kind" | "turn">): string {
  const words = title.normalize("NFKD").replace(/[\u0300-\u036f]/g, "").toLowerCase().replace(/[^a-z0-9]+/g, "-").replace(/^-+|-+$/g, "").slice(0, 60)
    .replace(/-+$/, "");
  return `${words || "answer"}-turn-${document.turn}.${document.kind}`;
}
