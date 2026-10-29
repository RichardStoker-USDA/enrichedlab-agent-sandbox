// Result kinds: a kind is a registry entry, not code. The broker serves the registry at
// `GET /api/v1/kinds`; until a broker offers it, the workspace uses the built-in entries
// below, which describe the files methods write today. A file's kind picks the views that can draw it; a view that
// needs an encoding (which columns a 3D surface draws) appears only when the method declared it, so no view guesses
// columns. A file that matches no kind is still kept and shown as a file: name, size, checksum and download.

export type Gpu = "webgpu" | "webgl2" | "still";
/** A view of a kind. It may draw only some of the kind's media types, and it may need channels (`encoding`) that a
 *  method maps to columns before it can draw. */
export interface KindView { view: string; gpu: Gpu[]; fallback: string; media_types?: string[]; encoding?: string[] }
export interface Kind {
  id: string; version: number; status: "candidate" | "accepted" | "retired"; profile_uri: string; media_types: string[];
  access: "whole" | "range" | "paged" | "live"; limits: Record<string, number>; views: KindView[];
  /** How a file gets the kind: from its media type, or only when the code that wrote it declares it. */
  recognition?: "media-type" | "declared";
  /** For a declared kind, the kinds whose checks its files pass by media type. */
  checked_as?: string[];
}
export interface Registry { registry: { digest: string; version: number }; kinds: Kind[] }
/** A view a method declared for one of its files, with the columns it draws (output binding). */
export interface DeclaredView { view: string; encoding?: Record<string, string> }
/** What a file records about its kind: the media type always; the registry kind and declared views when the broker
 *  records them (and its member fields). */
export interface KindedFile {
  path: string; media_type: string;
  result_kind?: { id: string; version: number } | null;
  views?: DeclaredView[] | null;
}

const still: Gpu[] = ["still"];
const profile = (id: string) => `urn:enrichedlab:kind:${id}`;
export const BUILT_IN: Registry = {
  registry: { digest: "built-in", version: 1 },
  kinds: [
    { id: "table.delimited", version: 1, status: "accepted", profile_uri: profile("table.delimited"), media_types: ["text/csv", "text/tab-separated-values"],
      access: "paged", limits: { bytes: 16 * 1024 * 1024, rows: 1_000_000, columns: 1000, cells: 2_000_000 },
      views: [{ view: "values", gpu: still, fallback: "file" }, { view: "surface", gpu: ["webgpu", "webgl2", "still"], fallback: "figure" }] },
    { id: "figure.r", version: 1, status: "accepted", profile_uri: profile("figure.r"), media_types: ["image/svg+xml", "application/pdf"],
      access: "whole", limits: { bytes: 64 * 1024 * 1024 }, views: [{ view: "figure", gpu: still, fallback: "file" }] },
    { id: "image.raster", version: 1, status: "accepted", profile_uri: profile("image.raster"), media_types: ["image/png", "image/jpeg", "image/webp", "image/gif"],
      access: "whole", limits: { bytes: 64 * 1024 * 1024 }, views: [{ view: "figure", gpu: still, fallback: "file" }] },
    { id: "text.record", version: 1, status: "accepted", profile_uri: profile("text.record"),
      media_types: ["text/plain", "text/markdown", "text/x-r", "text/x-python", "text/x-sql", "application/sql", "application/json", "application/x-ndjson",
        "application/yaml", "text/yaml", "application/x-yaml"],
      access: "whole", limits: { bytes: 2 * 1024 * 1024 }, views: [{ view: "text", gpu: still, fallback: "file" }] },
    { id: "file.opaque", version: 1, status: "accepted", profile_uri: profile("file.opaque"), media_types: [], access: "whole", limits: {},
      views: [{ view: "file", gpu: still, fallback: "file" }] },
  ],
};

/** Views that draw nothing without an encoding the method declared, for a registry that does not name the channels. */
export const NEEDS_ENCODING = new Set(["surface"]);
const needsEncoding = (view: KindView) => !!view.encoding?.length || NEEDS_ENCODING.has(view.view);
/** A declared view can draw when the method mapped every channel the registry names to a column, or for a registry
 *  that names none, when it mapped any. */
const supplied = (view: KindView, declared: DeclaredView) => view.encoding?.length
  ? view.encoding.every((channel) => typeof declared.encoding?.[channel] === "string" && declared.encoding[channel] !== "")
  : !NEEDS_ENCODING.has(view.view) || !!(declared.encoding && Object.keys(declared.encoding).length);
/** A view with media types draws a file of one of them. */
const draws = (view: KindView, mediaType: string | undefined) => !view.media_types?.length || mediaType === undefined
  || view.media_types.includes(base(mediaType));

// The broker's registry names its views by what they show; the workspace draws them with
// these modules. A view name the workspace has no module for stays as it is and is left out of the switch.
const MODULE_OF: Record<string, string> = {
  table: "values", image: "figure", "svg-sandboxed": "figure", pdf: "figure", record: "text", package: "file", download: "file",
};
/** The workspace module that draws a view the registry or a method names. */
export const moduleOf = (view: string) => MODULE_OF[view] ?? view;

const base = (type: string) => type.split(";")[0].trim().toLowerCase();
const GENERIC = new Set(["", "application/octet-stream", "binary/octet-stream"]);

/** The kind of a file: the kind the broker recorded when the registry holds it, else the accepted kind registered for
 *  its media type. A file with a generic type falls back on its name for tables and text, as the readers before the
 *  registry did; anything else is an opaque file. */
export function kindOf(registry: Registry, file: KindedFile): Kind {
  // A declared kind never comes from a media type: only the code that wrote a file can say the file is one.
  const accepted = registry.kinds.filter((k) => k.status !== "retired" && k.recognition !== "declared");
  const opaque = registry.kinds.find((k) => k.id === "file.opaque") ?? BUILT_IN.kinds.at(-1)!;
  if (file.result_kind) {
    const same = registry.kinds.filter((k) => k.id === file.result_kind!.id);
    const recorded = same.find((k) => k.version === file.result_kind!.version) ?? same.sort((a, b) => b.version - a.version)[0];
    if (recorded) return recorded;
  }
  const type = base(file.media_type);
  const byType = accepted.find((k) => k.media_types.includes(type));
  if (byType) return byType;
  const name = file.path.toLowerCase();
  const textual = GENERIC.has(type) || type.startsWith("text/");
  // The kind registered for the plain type of a table or text, else any kind its view draws.
  const fallback = (type: string, view: string) => accepted.find((k) => k.media_types.includes(type)) ?? accepted.find((k) => k.views.some((v) => v.view === view));
  if (textual && /\.(csv|tsv)$/.test(name)) return fallback("text/csv", "values") ?? opaque;
  if (textual && (type.startsWith("text/") || /\.(r|py|sql|json|jsonl|txt|log|md|yaml|yml)$/.test(name))) return fallback("text/plain", "text") ?? opaque;
  return opaque;
}

/** The views a file appears in: the ones its method declared that its kind can draw, or else the kind's views that need
 *  no encoding; with the file's media type, only the views that draw it. Never empty: an opaque file is still a file. */
export function viewsOf(kind: Kind, declared: DeclaredView[] | null | undefined, mediaType?: string): DeclaredView[] {
  const known = new Map(kind.views.map((v) => [v.view, v]));
  const chosen = (declared ?? []).map((d) => ({ ...d, view: moduleOf(d.view) }))
    .filter((d) => { const view = known.get(d.view); return !!view && draws(view, mediaType) && supplied(view, d); });
  if (chosen.length) return chosen;
  const plain = kind.views.filter((v) => !needsEncoding(v) && draws(v, mediaType)).map((v) => ({ view: v.view }));
  return plain.length ? plain : [{ view: "file" }];
}

const strings = (x: unknown) => Array.isArray(x) && x.every((t) => typeof t === "string");
const isKind = (k: unknown): k is Kind => {
  const v = k as Kind;
  return !!v && typeof v.id === "string" && Number.isInteger(v.version) && strings(v.media_types)
    && ["whole", "range", "paged", "live"].includes(v.access) && (v.recognition === undefined || ["media-type", "declared"].includes(v.recognition))
    && Array.isArray(v.views) && v.views.every((x) => typeof x?.view === "string"
      && (x.media_types === undefined || strings(x.media_types)) && (x.encoding === undefined || strings(x.encoding)));
};

/** A kind's views as the workspace's modules. Views one module draws become one view that draws every media type any of
 *  them draws (an R figure's SVG, PDF and PNG views are one figure view). */
function asModules(views: KindView[]): KindView[] {
  const out: KindView[] = [];
  for (const view of views) {
    const named: KindView = { ...view, view: moduleOf(view.view), fallback: moduleOf(view.fallback ?? "file") };
    const same = out.find((o) => o.view === named.view);
    if (!same) { out.push(named); continue; }
    // A view without media types draws every type of its kind.
    if (same.media_types && named.media_types) same.media_types = [...new Set([...same.media_types, ...named.media_types])];
    else delete same.media_types;
    if (!same.encoding?.length && named.encoding?.length) same.encoding = named.encoding;
  }
  return out;
}

/** The registry when the answer is one, with its view names as the workspace's modules, else the built-in entries. */
export const registryFrom = (r: unknown): Registry => {
  const v = r as Registry;
  if (!(v && typeof v.registry?.digest === "string" && Array.isArray(v.kinds) && v.kinds.length && v.kinds.every(isKind))) return BUILT_IN;
  return { registry: v.registry, kinds: v.kinds.map((k) => ({ ...k, views: asModules(k.views) })) };
};
let loaded: Promise<Registry> | null = null;
/** The broker's registry, read once per page; a broker without one, or an answer that is not a registry, leaves the
 *  built-in entries in force. */
export function kinds(): Promise<Registry> {
  // Same-origin only, no redirects, revalidated against the registry's ETag.
  loaded ??= fetch("/api/v1/kinds", { credentials: "same-origin", cache: "no-cache", redirect: "error", signal: AbortSignal.timeout(15000),
    headers: { Accept: "application/json" } })
    .then((r) => (r.ok && r.headers.get("content-type")?.includes("application/json") ? r.json() : null))
    .then(registryFrom, () => BUILT_IN);
  return loaded;
}
