import { createHash } from "node:crypto";
import { gzipSync } from "node:zlib";
import { kindOf } from "../src/file_role.ts";

// Archive helpers for the synthetic retained packages. The files themselves come from
// fixtures/r: simulated inputs, the R code that ran, and what it wrote.

const sha = (bytes: Buffer) => createHash("sha256").update(bytes).digest("hex");

// Minimal ustar archive; paths here are short, so no long-name records are needed.
export function tar(files: { path: string; bytes: Buffer }[], mtime: number): Buffer {
  const blocks: Buffer[] = [];
  for (const file of files) {
    const header = Buffer.alloc(512, 0);
    header.write(file.path, 0, 100, "utf8");
    header.write("0000644\0", 100, 8, "ascii");
    header.write("0000000\0", 108, 8, "ascii");
    header.write("0000000\0", 116, 8, "ascii");
    header.write(file.bytes.length.toString(8).padStart(11, "0") + "\0", 124, 12, "ascii");
    header.write(mtime.toString(8).padStart(11, "0") + "\0", 136, 12, "ascii");
    header.fill(" ", 148, 156);
    header.write("0", 156, 1, "ascii");
    header.write("ustar\0", 257, 6, "ascii");
    header.write("00", 263, 2, "ascii");
    let sum = 0;
    for (const byte of header) sum += byte;
    header.write(sum.toString(8).padStart(6, "0") + "\0 ", 148, 8, "ascii");
    blocks.push(header, file.bytes, Buffer.alloc((512 - (file.bytes.length % 512)) % 512, 0));
  }
  blocks.push(Buffer.alloc(1024, 0));
  return Buffer.concat(blocks);
}

// Wrap retained files as a package with a SHA256SUMS list, like a reviewed R result.
export function syntheticPackage(files: { path: string; bytes: Buffer }[], mtime: number) {
  const members = [...files];
  const sums = members.map((f) => `${sha(f.bytes)}  ${f.path}`).join("\n") + "\n";
  members.push({ path: "SHA256SUMS", bytes: Buffer.from(sums) });
  const archive = gzipSync(tar(members, mtime));
  return { archive, sha256: sha(archive), files: members, checksum: Buffer.from(`${sha(archive)}\n`) };
}

/** What a method declared for one of its files (the member fields): its
 *  registry kind, the files that hold the same numbers, and the views that draw it. The listing adds the broker's check,
 *  which every declared file in the preview passes. */
export interface MemberResult { result_kind: { id: string; version: number }; same_values_group: string | null; views: { view: string; encoding?: Record<string, string> }[] }

// Members are grouped by the broker's rules (kindOf), so the preview sorts files as a site does.
export function listing(files: { path: string; bytes: Buffer }[], results: Record<string, MemberResult> = {}) {
  const media = (path: string) => path.endsWith(".pdf") ? "application/pdf" : path.endsWith(".svg") ? "image/svg+xml" : path.endsWith(".csv") ? "text/csv"
    : path.endsWith(".json") ? "application/json" : path.endsWith(".md") ? "text/markdown" : "text/plain";
  return {
    format: "tar+gzip", checksum_file: "SHA256SUMS", missing_from_archive: [], skipped: [],
    expanded_bytes: files.reduce((n, f) => n + f.bytes.length, 0),
    members: files.map((f) => ({ path: f.path, size_bytes: f.bytes.length, sha256: sha(f.bytes), media_type: media(f.path), kind: kindOf(f.path),
      manifest: f.path === "SHA256SUMS" ? "not_applicable" : "matched", ...(results[f.path] ?? {}),
      ...(results[f.path] ? { validation: { state: "passed", checked_as: results[f.path].result_kind, reason: null } } : {}) }))
      .sort((a, b) => a.path.localeCompare(b.path)),
  };
}

// RFC 4180 fields: quoted fields may hold commas, doubled quotes and line breaks.
export function csvRows(text: string): string[][] {
  const rows: string[][] = [];
  let row: string[] = [], field = "", quoted = false;
  for (let i = 0; i < text.length; i++) {
    const c = text[i];
    if (quoted) {
      if (c === '"' && text[i + 1] === '"') { field += '"'; i++; }
      else if (c === '"') quoted = false;
      else field += c;
    } else if (c === '"') quoted = true;
    else if (c === ",") { row.push(field); field = ""; }
    else if (c === "\n") { row.push(field); rows.push(row); row = []; field = ""; }
    else if (c !== "\r") field += c;
  }
  if (field || row.length) { row.push(field); rows.push(row); }
  return rows;
}

// The digest the broker's worker stores for a result package, from the package's own
// result tables: cells as written when a table is small, otherwise its shape.
export function fixtureDigest(retained: { files: { path: string; bytes: Buffer }[] }, artifactId: string, artifactSha: string, recordedAt: string) {
  const tables = retained.files.filter((f) => f.path.startsWith("results/") && f.path.endsWith(".csv")).map((f) => {
    const [header = [], ...body] = csvRows(f.bytes.toString("utf8"));
    const columns = header.map((c, i) => c.trim() || `column_${i + 1}`);
    const small = f.bytes.length <= 8192 && columns.length <= 16 && body.length <= 50;
    return { path: f.path, sha256: sha(f.bytes), bytes: f.bytes.length, rows: body.length, columns, ...(small ? { cells: body } : { cells_omitted: "too_large" }) };
  });
  const figures = retained.files.filter((f) => f.path.startsWith("results/") && /\.(pdf|svg|png)$/.test(f.path)).map((f) => ({ path: f.path, sha256: sha(f.bytes) }));
  const digest = { schema: "enrichedlab.result-digest.v1", status: tables.length ? "summarized" : "no_result_tables",
    package: { members: retained.files.length, checksums_listed: true, all_matched: true }, tables, tables_omitted: 0, figures };
  return { artifact_id: artifactId, artifact_sha256: artifactSha, status: digest.status, digest_sha256: sha(Buffer.from(JSON.stringify(digest))), recorded_at: recordedAt, digest };
}
