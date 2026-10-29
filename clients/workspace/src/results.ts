// What the result frame shows for a file (Glass Box, results rules 1 and 2): the file with its checked reads, and the
// views it and the files holding the same numbers can be drawn in, one switch apart. Which views a file has comes from
// its kind and what its method declared; the frame never guesses columns or kinds.
import { broker, verifiedArtifact, verifiedMember } from "./api.ts";
import type { Artifact, KindCheck, PackageMember } from "./contracts.ts";
import { kindOf, viewsOf, type DeclaredView, type Gpu, type Kind, type Registry } from "./kinds.ts";
import { fromArtifact, fromMember, type ViewedFile } from "./viewer.ts";
import type { Page, PageQuery, Summary } from "./views/types.ts";

export interface ResultFile {
  file: ViewedFile;
  result_kind?: { id: string; version: number } | null;
  views?: DeclaredView[] | null;
  /** The same-values group the method recorded: files that hold the same numbers in different forms. */
  group: string | null;
  read(signal?: AbortSignal): Promise<Blob>;
  download(): void;
  /** Server pages and summaries; a broker without them answers 404 and the views read the file whole. */
  page: ((query: PageQuery, signal?: AbortSignal) => Promise<Page>) | null;
  summary: ((signal?: AbortSignal) => Promise<Summary>) | null;
  /** A figure's PDF original when the file is its SVG copy, as the caller knows it. */
  original?: { file: ViewedFile; download(): void } | null;
  /** How the file's checksum stands against its package's list, in words. */
  checksum?: string;
  /** The broker's check of the file against its kind, when it sent one. */
  check?: KindCheck | null;
}

const CHECKSUM: Record<PackageMember["manifest"], string> = {
  matched: "matches the package checksum list", mismatched: "does not match the package checksum list",
  unlisted: "not on the package checksum list", not_applicable: "checked when read",
};

export function memberResult(task: string, artifact: Artifact, member: PackageMember, label: string, save: () => void): ResultFile {
  return {
    file: fromMember(member, label), result_kind: member.result_kind ?? null, views: member.views ?? null, group: member.same_values_group ?? null,
    checksum: CHECKSUM[member.manifest], check: member.validation ?? null,
    read: (signal) => verifiedMember(task, artifact, member, signal),
    download: save,
    page: (query, signal) => broker.pages(task, artifact.id, member.path, query, signal),
    summary: (signal) => broker.summary(task, artifact.id, member.path, signal),
  };
}

export function artifactResult(task: string, artifact: Artifact, save: () => void): ResultFile {
  return {
    file: fromArtifact(artifact), result_kind: artifact.kind ?? null, views: artifact.views ?? null, group: artifact.same_values_group ?? null,
    checksum: "checked when read", check: artifact.validation ?? null,
    read: (signal) => verifiedArtifact(task, artifact, signal),
    download: save,
    page: (query, signal) => broker.pages(task, artifact.id, null, query, signal),
    summary: (signal) => broker.summary(task, artifact.id, null, signal),
  };
}

/** A file read by a caller's own loader, with no server pages: the frame still draws it by its kind. */
export function plainResult(file: ViewedFile, read: (signal?: AbortSignal) => Promise<Blob>, download: () => void,
  original: { file: ViewedFile; download(): void } | null = null): ResultFile {
  return { file, group: null, read, download, page: null, summary: null, original };
}

/** The broker's check of a file against its kind, in words, or null when it sent none. */
export function checkWords(check: KindCheck | null | undefined): string | null {
  if (!check) return null;
  const as = check.checked_as ? ` as ${check.checked_as.id} version ${check.checked_as.version}` : "";
  if (check.state === "passed") return `Passed${as}.`;
  if (check.state === "not_checked") return "Not checked yet.";
  const why = check.reason?.trim().replace(/[.!?]+$/, "");
  return `Did not pass${as}${why ? `: ${why}` : ""}. It opens as a plain file.`;
}

/** The other files of a package that hold the same numbers as `subject`, as the method recorded them. */
export function sameValues(subject: ResultFile, all: ResultFile[]): ResultFile[] {
  return subject.group ? all.filter((f) => f.group === subject.group && f.file.path !== subject.file.path) : [];
}

/** One file in Quick Look: its result frame, or for a file the browser does not open (an analysis package) its words
 *  and action. */
export interface QuickItem {
  key: string; title: string; made?: string | null;
  result: ResultFile | null; group?: ResultFile[];
  note?: { text: string; action: string; run(): void };
}

export interface PlannedView {
  view: string; label: string; file: ResultFile; kind: Kind; encoding: Record<string, string>; gpu: Gpu[];
  original: { file: ViewedFile; download(): void } | null;
}
const ORDER = ["surface", "figure", "values", "text", "file"];
const LABEL: Record<string, string> = { surface: "3D", values: "Values", text: "Text", file: "File" };
const pdf = (f: ResultFile) => f.file.media_type.split(";")[0].trim().toLowerCase() === "application/pdf";
const stem = (path: string) => path.replace(/\.[^./]+$/, "");

/** The views of a result, in the switch's order (3D, R figure, values, then text and file): the opened file's own
 *  views and those of the files holding the same numbers, one per view. A view goes to the opened file when it has
 *  it; a figure goes to the lighter SVG or image with R's PDF as its original. `drawable` names the views the workspace
 *  has modules for; a kind's view without one is left out, and a file with none left is still a file. */
export function planViews(registry: Registry, subject: ResultFile, group: ResultFile[], drawable: Set<string>): { views: PlannedView[]; first: string } {
  const members = [subject, ...group.filter((g) => g.file.path !== subject.file.path)];
  const offers = members.flatMap((file) => {
    const kind = kindOf(registry, { path: file.file.path, media_type: file.file.media_type, result_kind: file.result_kind, views: file.views });
    return viewsOf(kind, file.views, file.file.media_type).filter((v) => drawable.has(v.view)).map((v) => ({ view: v.view, file, kind, encoding: v.encoding ?? {},
      gpu: kind.views.find((k) => k.view === v.view)?.gpu ?? (["still"] as Gpu[]) }));
  });
  const views: PlannedView[] = [];
  for (const id of [...ORDER, ...new Set(offers.map((o) => o.view).filter((v) => !ORDER.includes(v)))]) {
    const all = offers.filter((o) => o.view === id);
    if (!all.length) continue;
    // A group's own "file" view adds nothing when the opened file already has views.
    if (id === "file" && all.every((o) => o.file !== subject)) continue;
    const chosen = all.find((o) => o.file === subject) ?? all.find((o) => !pdf(o.file)) ?? all[0];
    const pdfOriginal = id === "figure" && !pdf(chosen.file) ? all.find((o) => pdf(o.file) && stem(o.file.file.path) === stem(chosen.file.file.path)) : undefined;
    const figureLabel = chosen.kind.id === "figure.r" ? "R figure" : pdf(chosen.file) ? "PDF" : "Image";
    views.push({ ...chosen, label: id === "figure" ? figureLabel : LABEL[id] ?? id,
      original: chosen.file.original ?? (pdfOriginal ? { file: pdfOriginal.file.file, download: pdfOriginal.file.download } : null) });
  }
  if (!views.length) {
    const kind = kindOf(registry, { path: subject.file.path, media_type: subject.file.media_type });
    views.push({ view: "file", label: "File", file: subject, kind, encoding: {}, gpu: ["still"], original: null });
  }
  const own = offers.filter((o) => o.file === subject).map((o) => o.view);
  return { views, first: own.find((v) => views.some((p) => p.view === v)) ?? views[0].view };
}
