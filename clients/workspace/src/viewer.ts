import type { Artifact, PackageMember } from "./contracts.ts";

// The fields a reader needs, whether the file is a registered artifact or a member of a
// retained package. Both carry a recorded size and SHA-256 that the browser checks.
export interface ViewedFile {
  label: string;
  path: string;
  media_type: string;
  size_bytes: number;
  sha256: string;
}

export type Reader = "image" | "pdf" | "table" | "text" | "none";

export const MAX_TABLE_BYTES = 8 * 1024 * 1024;
export const MAX_TEXT_BYTES = 2 * 1024 * 1024;

export function readerFor(file: ViewedFile): Reader {
  const type = file.media_type.split(";")[0].trim().toLowerCase();
  const name = file.path.toLowerCase();
  if (["image/png", "image/jpeg", "image/webp", "image/gif", "image/svg+xml"].includes(type)) return "image";
  if (type === "application/pdf") return "pdf";
  if (["text/csv", "text/tab-separated-values"].includes(type) || /\.(csv|tsv)$/.test(name)) return "table";
  if (type.startsWith("text/") || ["application/json", "application/x-ndjson"].includes(type) || /\.(r|py|sql|json|jsonl|txt|log|md|yaml|yml)$/.test(name)) return "text";
  return "none";
}

export const fromArtifact = (artifact: Artifact): ViewedFile => ({
  label: artifact.label, path: artifact.relative_path.split("/").slice(2).join("/") || artifact.relative_path,
  media_type: artifact.media_type, size_bytes: artifact.size_bytes, sha256: artifact.sha256,
});

export const fromMember = (member: PackageMember, label: string): ViewedFile => ({
  label, path: member.path, media_type: member.media_type, size_bytes: member.size_bytes, sha256: member.sha256,
});

// A safe local file name: the last path segment, restricted to plain characters.
export const saveName = (path: string) => (path.split("/").at(-1) ?? "file").replace(/[^a-zA-Z0-9._-]/g, "_") || "file";

export function saveBlob(blob: Blob, name: string) {
  const url = URL.createObjectURL(blob);
  const link = document.createElement("a");
  link.href = url;
  link.download = name;
  link.click();
  setTimeout(() => URL.revokeObjectURL(url), 1000);
}
