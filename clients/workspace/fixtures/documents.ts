// Preview fixtures for answer documents (broker migration 072): a run's documents, a person's request for one, and the
// file in its package. A request is written a moment later; the file is a few synthetic bytes under the document's
// name, served with its SHA-256 like any package member.
import { createHash, randomUUID } from "node:crypto";
import type { IncomingMessage, ServerResponse } from "node:http";
import type { DocumentKind, RunDocument } from "../src/contracts.ts";

type Json = (value: unknown, status?: number) => void;

const KINDS: DocumentKind[] = ["xlsx", "docx", "pptx"];
const MEDIA: Record<DocumentKind, string> = {
  xlsx: "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet",
  docx: "application/vnd.openxmlformats-officedocument.wordprocessingml.document",
  pptx: "application/vnd.openxmlformats-officedocument.presentationml.presentation",
};
const WRITE_MS = 1200;

export function documentFixtures(options: { runs: () => { run_id: string; project_id: string; turn: number }[]; at: (seconds: number) => string }) {
  const made = new Map<string, RunDocument[]>();
  const bytes = new Map<string, Uint8Array>();
  const sha = (data: Uint8Array) => createHash("sha256").update(data).digest("hex");
  const finish = (document: RunDocument) => {
    const data = new TextEncoder().encode(`PK\u0003\u0004 synthetic ${document.kind} of turn ${document.turn}`);
    bytes.set(document.task_id, data);
    Object.assign(document, { status: "completed", finished_at: new Date().toISOString(), receipt: { status: "completed",
      error_code: null, message: null, package_sha256: "f".repeat(64), file: { kind: document.kind, path: `document.${document.kind}`,
        media_type: MEDIA[document.kind], sha256: sha(data), size_bytes: data.length } } });
  };
  const owner = (task: string) => [...made.values()].flat().find((d) => d.task_id === task);
  const body = async (req: IncomingMessage) => { let text = ""; for await (const chunk of req) text += chunk; return text; };
  return {
    reset() { made.clear(); bytes.clear(); },
    async handle(req: IncomingMessage, res: ServerResponse, url: URL, json: Json, denied: () => void): Promise<boolean> {
      const list = url.pathname.match(/^\/api\/v1\/research-runs\/([0-9a-f-]{36})\/documents$/);
      if (list && req.method === "GET") {
        if (!options.runs().some((r) => r.run_id === list[1])) { denied(); return true; }
        json({ documents: made.get(list[1]) ?? [] });
        return true;
      }
      const ask = url.pathname.match(/^\/api\/v1\/projects\/([0-9a-f-]{36})\/documents$/);
      if (ask && req.method === "POST") {
        let input: { run_id?: string; kind?: DocumentKind; request_id?: string };
        try { input = JSON.parse(await body(req)); } catch { json({ error: { code: "request_invalid" } }, 400); return true; }
        const run = options.runs().find((r) => r.run_id === input.run_id && r.project_id === ask[1]);
        if (!run || !input.kind || !KINDS.includes(input.kind)) { json({ error: { code: "document_request_invalid" } }, 400); return true; }
        const document: RunDocument = { task_id: randomUUID(), kind: input.kind, turn: run.turn, status: "running",
          created_at: new Date().toISOString(), finished_at: null, receipt: null };
        made.set(run.run_id, [document, ...(made.get(run.run_id) ?? [])]);
        setTimeout(() => finish(document), WRITE_MS);
        json({ task_id: document.task_id }, 202);
        return true;
      }
      const artifacts = url.pathname.match(/^\/api\/v1\/tasks\/([0-9a-f-]{36})\/artifacts$/);
      const contents = url.pathname.match(/^\/api\/v1\/tasks\/([0-9a-f-]{36})\/artifacts\/([0-9a-f-]{36})\/contents(\/file)?$/);
      const task = (artifacts ?? contents)?.[1];
      const document = task ? owner(task) : undefined;
      if (!document || req.method !== "GET") return false;
      const file = document.receipt?.file;
      const data = bytes.get(document.task_id);
      if (!file || !data) { denied(); return true; }
      const artifact = { id: document.task_id, role: "result", label: "Answer document and its package",
        relative_path: `tasks/${document.task_id}/attempts/1-1/exports/payload`, media_type: "application/gzip",
        size_bytes: 512, sha256: "f".repeat(64), created_at: options.at(-1) };
      if (artifacts) { json({ artifacts: [artifact] }); return true; }
      if (contents && !contents[3]) {
        json({ artifact_id: artifact.id, sha256: artifact.sha256, size_bytes: artifact.size_bytes, listing: { format: "tar+gzip",
          checksum_file: "SHA256SUMS", members: [{ path: file.path, size_bytes: file.size_bytes, sha256: file.sha256,
            media_type: file.media_type, kind: "file", manifest: "matched" }], missing_from_archive: [], skipped: [], expanded_bytes: 512 } });
        return true;
      }
      if (url.searchParams.get("path") !== file.path) { denied(); return true; }
      res.statusCode = 200;
      res.setHeader("Content-Type", "application/octet-stream");
      res.setHeader("X-Artifact-SHA256", file.sha256);
      res.setHeader("Content-Length", String(data.length));
      res.end(Buffer.from(data));
      return true;
    },
  };
}
