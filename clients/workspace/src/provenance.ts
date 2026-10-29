// Where a value or a figure on a project page came from (Glass Box rule 4), assembled from records the page has read:
// the finding's binding, the stored digest of the result package, the package listing with its checksum states, and
// the analysis and step that made it. Nothing is inferred from file names alone.
import type { RetainedPackage } from "./evidence.ts";
import { artifactOf, display, type ArtifactDigest, type Binding } from "./findings.ts";
import type { Figure } from "./package_view.ts";

export interface Made { task: string; step: number | null; title: string; method: string }
export interface Source {
  kind: "value" | "figure";
  /** The value as the sentence shows it and as the file holds it; empty for a figure. */
  shown: string; exact: string;
  /** What it is, in words. */
  what: string;
  file: string;
  /** For a value, the row and column; for a figure, its formats. */
  place: string;
  made: Made;
  check: { state: "matched" | "mismatched" | "unknown"; words: string };
}

const checkOf = (manifest: string | undefined): Source["check"] =>
  manifest === "matched" ? { state: "matched", words: "The file matches the package's checksum list." }
  : manifest === "mismatched" ? { state: "mismatched", words: "The file does not match the package's checksum list." }
  : { state: "unknown", words: "The package's checksum list does not cover this file." };

/** A finding's value: its cell in a result table, the analysis that wrote it, and the file's checksum state. */
export function valueSource(binding: Binding, value: string | null, digests: Record<string, ArtifactDigest | undefined>,
                            packages: RetainedPackage[] | undefined, made: (task: string) => Made): Source | null {
  const ref = artifactOf(binding.ref);
  if (!ref) return null;
  const member = packages?.find((p) => p.artifact.id === ref.artifact)?.contents.listing.members.find((m) => m.path === binding.table);
  const digest = digests[binding.ref];
  const recorded = digest?.digest?.tables?.some((t) => t.path === binding.table);
  return {
    kind: "value", shown: value === null ? "" : display(value), exact: value ?? "",
    what: `“${binding.column}” in row ${binding.row}`,
    file: binding.table, place: `Row ${binding.row}, column “${binding.column}”`,
    made: made(ref.task),
    check: member ? checkOf(member.manifest)
      : recorded ? { state: "unknown", words: "Read from the package's stored digest; open the analysis to check the file itself." }
      : { state: "unknown", words: "The stored digest of this package does not hold this table." },
  };
}

/** A figure: the file the page draws, the R original beside it, the analysis that drew it, and its checksum state. */
export function figureSource(task: string, figure: Figure, made: (task: string) => Made): Source {
  const formats = [figure.display, figure.original].filter((m): m is NonNullable<typeof m> => !!m)
    .map((m) => m.media_type === "application/pdf" ? "PDF" : m.media_type === "image/svg+xml" ? "SVG" : m.media_type.replace(/^image\//, "").toUpperCase());
  return {
    kind: "figure", shown: "", exact: "", what: figure.title,
    file: (figure.original ?? figure.display).path, place: [...new Set(formats)].join(" and "),
    made: made(task),
    check: checkOf(figure.original?.manifest === "mismatched" ? "mismatched" : figure.display.manifest),
  };
}
