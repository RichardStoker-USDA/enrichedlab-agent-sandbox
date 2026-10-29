// The bar's search (Command K): places, commands and the reader's projects by title and question. It matches the
// words the reader types against names the workspace already shows; it searches no file contents.

export interface SearchEntry {
  id: string;
  kind: "place" | "command" | "project";
  label: string;
  detail?: string;
  /** Lowercase text the query is matched against: the label and anything else the entry is known by. */
  words: string;
  /** Newer projects first when nothing is typed. */
  at?: string;
}

export const kindWord: Record<SearchEntry["kind"], string> = { place: "Go to", command: "Do", project: "Project" };

const tokens = (query: string) => query.toLowerCase().split(/\s+/).filter(Boolean);

/** Entries whose words hold every typed word, best first: a label that starts with the query, then one that holds it,
 *  then the rest; places and commands before projects at equal rank. With nothing typed, places and commands, then the
 *  most recently changed projects. */
export function rank(entries: SearchEntry[], query: string, limit = 12): SearchEntry[] {
  const q = query.trim().toLowerCase();
  const order = (e: SearchEntry) => (e.kind === "project" ? 1 : 0);
  if (!q) {
    const fixed = entries.filter((e) => e.kind !== "project");
    const recent = entries.filter((e) => e.kind === "project").sort((a, b) => (b.at ?? "").localeCompare(a.at ?? ""));
    return [...fixed, ...recent].slice(0, limit);
  }
  const words = tokens(q);
  const score = (e: SearchEntry) => {
    const label = e.label.toLowerCase();
    return label.startsWith(q) ? 0 : label.includes(q) ? 1 : 2;
  };
  return entries
    .filter((e) => words.every((w) => e.words.includes(w)))
    .map((e, i) => ({ e, i, s: score(e) }))
    .sort((a, b) => a.s - b.s || order(a.e) - order(b.e) || a.i - b.i)
    .slice(0, limit)
    .map((x) => x.e);
}
