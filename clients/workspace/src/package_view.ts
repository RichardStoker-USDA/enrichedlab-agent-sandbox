import type { PackageListing, PackageMember } from "./contracts.ts";
import { fileTitle } from "./language.ts";

export interface Figure {
  title: string;
  // What the page draws: an SVG or raster when the package has one, otherwise the PDF.
  display: PackageMember;
  // The R PDF when a lighter display version exists; always the publication original.
  original: PackageMember | null;
}

export interface Integrity {
  hasList: boolean;
  checked: number;
  matched: number;
  mismatched: string[];
  unlisted: string[];
  missing: string[];
}

export interface PackageView {
  figures: Figure[];
  tables: PackageMember[];
  inputs: PackageMember[];
  code: PackageMember[];
  notes: PackageMember[];
  parameters: PackageMember[];
  environment: PackageMember[];
  logs: PackageMember[];
  other: PackageMember[];
  checksums: PackageMember | null;
  integrity: Integrity;
  // Written by the reviewed template around one model-written block.
  template: boolean;
}

const displayOrder = ["image/svg+xml", "image/png", "image/webp", "image/jpeg", "image/gif", "application/pdf"];
const stemOf = (path: string) => path.replace(/\.[^./]+$/, "");
const nameOf = (path: string) => (path.split("/").at(-1) ?? path).toLowerCase();

// Results come first; inside a group, shorter and alphabetically earlier paths lead.
const byPath = (a: PackageMember, b: PackageMember) => {
  const rank = (m: PackageMember) => (m.path.startsWith("results/") ? 0 : 1);
  return rank(a) - rank(b) || a.path.split("/").length - b.path.split("/").length || a.path.localeCompare(b.path);
};

export function organize(listing: PackageListing): PackageView {
  const view: PackageView = {
    figures: [], tables: [], inputs: [], code: [], notes: [], parameters: [], environment: [], logs: [], other: [],
    checksums: null,
    template: listing.members.some((m) => m.path === "code/block.R") && listing.members.some((m) => m.path === "run.json"),
    integrity: { hasList: listing.checksum_file !== null, checked: 0, matched: 0, mismatched: [], unlisted: [], missing: [...listing.missing_from_archive] },
  };
  const figureGroups = new Map<string, PackageMember[]>();
  for (const member of listing.members) {
    const name = nameOf(member.path);
    if (member.path === "SHA256SUMS") { view.checksums = member; continue; }
    if (view.integrity.hasList) {
      view.integrity.checked += 1;
      if (member.manifest === "matched") view.integrity.matched += 1;
      else if (member.manifest === "mismatched") view.integrity.mismatched.push(member.path);
      else if (member.manifest === "unlisted") view.integrity.unlisted.push(member.path);
    }
    if (member.kind === "figure" && !member.path.startsWith("input/")) {
      const group = figureGroups.get(stemOf(member.path)) ?? [];
      group.push(member);
      figureGroups.set(stemOf(member.path), group);
    } else if (member.path.startsWith("input/")) view.inputs.push(member);
    else if (member.kind === "table") view.tables.push(member);
    else if (member.kind === "code") view.code.push(member);
    else if (name === "readme.md" || name === "method.txt" || name.endsWith("_method.txt")) view.notes.push(member);
    else if (/param/.test(name) && member.kind === "record") view.parameters.push(member);
    else if (name === "environment.txt" || name === "graphics.txt" || name.startsWith("sessioninfo") || name === "session_info.txt") view.environment.push(member);
    else if (member.kind === "log" || name === "exit_code.txt" || member.path.startsWith("logs/")) view.logs.push(member);
    else view.other.push(member);
  }
  for (const group of figureGroups.values()) {
    group.sort((a, b) => displayOrder.indexOf(a.media_type) - displayOrder.indexOf(b.media_type));
    const display = group[0];
    const original = group.find((m) => m.media_type === "application/pdf" && m !== display) ?? null;
    view.figures.push({ title: fileTitle(display.path), display, original });
  }
  view.figures.sort((a, b) => byPath(a.display, b.display));
  for (const list of [view.tables, view.inputs, view.code, view.notes, view.parameters, view.environment, view.logs, view.other]) list.sort(byPath);
  return view;
}

// A single sentence a scientist can read about the package's own checksum list.
export function integritySentence(integrity: Integrity): string {
  if (!integrity.hasList) return "This package has no checksum list of its own; each file was still hashed when it was read.";
  const problems = integrity.mismatched.length + integrity.missing.length;
  if (problems === 0 && integrity.unlisted.length === 0)
    return `All ${integrity.checked} files match the checksums recorded when the analysis ran.`;
  const parts = [`${integrity.matched} of ${integrity.checked} files match the checksums recorded when the analysis ran`];
  if (integrity.mismatched.length) parts.push(`${integrity.mismatched.length} do not match`);
  if (integrity.missing.length) parts.push(`${integrity.missing.length} listed files are missing`);
  if (integrity.unlisted.length) parts.push(`${integrity.unlisted.length} are not on the list`);
  return `${parts.join("; ")}.`;
}

/** True when the package's only code is a database query. */
export function queryOnly(view: PackageView): boolean {
  return view.code.length > 0 && view.code.every((m) => m.path.toLowerCase().endsWith(".sql"));
}

/** What a retained package holds, in words, from its own listing: a database query's package names its query and no
 *  input copy, an R analysis's its code, input copy and logs. An unread package keeps the general description. */
export function packageHolds(view: PackageView | null | undefined): string {
  if (!view) return "its code, inputs, parameters, results and checksums";
  const parts = [
    view.code.length ? (queryOnly(view) ? "the query" : "the code") : "",
    view.inputs.length ? "the input copy" : "",
    view.parameters.length ? "the parameters" : "",
    view.tables.length || view.figures.length ? "the results" : "",
    view.logs.length ? "the logs" : "",
    view.checksums ? "the checksums" : "",
  ].filter(Boolean);
  if (!parts.length) return "its files";
  return parts.length === 1 ? parts[0] : `${parts.slice(0, -1).join(", ")} and ${parts.at(-1)}`;
}
