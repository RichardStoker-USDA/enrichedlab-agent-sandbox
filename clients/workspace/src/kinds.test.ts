import assert from "node:assert/strict";
import test from "node:test";
import { BUILT_IN, kindOf, registryFrom, viewsOf } from "./kinds.ts";
import { checkWords, planViews, type ResultFile } from "./results.ts";

const file = (path: string, media_type: string, extra: Partial<ResultFile> = {}): ResultFile => ({
  file: { label: path, path, media_type, size_bytes: 1, sha256: "0".repeat(64) }, group: null,
  read: async () => new Blob(), download: () => {}, page: null, summary: null, ...extra,
});
const DRAWABLE = new Set(["surface", "figure", "values", "text", "file"]);

test("a file's kind is the one its method recorded, else its media type's, else an opaque file", () => {
  assert.equal(kindOf(BUILT_IN, { path: "a.csv", media_type: "text/csv" }).id, "table.delimited");
  assert.equal(kindOf(BUILT_IN, { path: "a.csv", media_type: "application/octet-stream" }).id, "table.delimited");
  assert.equal(kindOf(BUILT_IN, { path: "fig.svg", media_type: "image/svg+xml" }).id, "figure.r");
  assert.equal(kindOf(BUILT_IN, { path: "model.rds", media_type: "application/octet-stream" }).id, "file.opaque");
  assert.equal(kindOf(BUILT_IN, { path: "x.bin", media_type: "text/csv", result_kind: { id: "file.opaque", version: 1 } }).id, "file.opaque");
  // A recorded kind the registry does not hold falls back on the media type.
  assert.equal(kindOf(BUILT_IN, { path: "a.csv", media_type: "text/csv", result_kind: { id: "mesh.gltf", version: 2 } }).id, "table.delimited");
});

test("a 3D view appears only when the method declared its columns", () => {
  const table = kindOf(BUILT_IN, { path: "a.csv", media_type: "text/csv" });
  assert.deepEqual(viewsOf(table, null), [{ view: "values" }]);
  assert.deepEqual(viewsOf(table, [{ view: "surface" }]), [{ view: "values" }]);
  const surface = { view: "surface", encoding: { x: "hour", y: "block", z: "difference_c" } };
  assert.deepEqual(viewsOf(table, [{ view: "values" }, surface]), [{ view: "values" }, surface]);
  // A view the kind does not have is not offered.
  assert.deepEqual(viewsOf(table, [{ view: "map" }]), [{ view: "values" }]);
});

test("a registry answer that is not a registry leaves the built-in entries", () => {
  assert.equal(registryFrom(null), BUILT_IN);
  assert.equal(registryFrom({ registry: { digest: "x" }, kinds: [] }), BUILT_IN);
  assert.equal(registryFrom({ registry: { digest: "x" }, kinds: [{ id: "k", version: 1, media_types: ["a/b"], access: "sideways", views: [] }] }), BUILT_IN);
  const served = { registry: { digest: "sha256:1", version: 1 }, kinds: BUILT_IN.kinds };
  assert.deepEqual(registryFrom(served), served);
});

test("files holding the same numbers are one result: 3D, R figure, values, with R's PDF as the figure's original", () => {
  const csv = file("results/season.csv", "text/csv", { group: "g", result_kind: { id: "table.delimited", version: 1 },
    views: [{ view: "values" }, { view: "surface", encoding: { x: "hour", y: "block", z: "d" } }] });
  const svg = file("results/season.svg", "image/svg+xml", { group: "g", views: [{ view: "figure" }] });
  const pdf = file("results/season.pdf", "application/pdf", { group: "g", views: [{ view: "figure" }] });
  // The PDF comes first in the group, as a package lists it; the lighter SVG is still the one drawn.
  const fromTable = planViews(BUILT_IN, csv, [pdf, svg], DRAWABLE);
  assert.deepEqual(fromTable.views.map((v) => v.label), ["3D", "R figure", "Values"]);
  assert.equal(fromTable.first, "values");
  const figure = fromTable.views.find((v) => v.view === "figure")!;
  assert.equal(figure.file, svg);
  assert.equal(figure.original?.file.path, "results/season.pdf");
  assert.deepEqual(fromTable.views.find((v) => v.view === "surface")!.encoding, { x: "hour", y: "block", z: "d" });
  // Opened from the PDF, the figure view is the PDF itself and it opens first.
  const fromPdf = planViews(BUILT_IN, pdf, [csv, svg], DRAWABLE);
  assert.equal(fromPdf.first, "figure");
  assert.equal(fromPdf.views.find((v) => v.view === "figure")!.file, pdf);
  // A file alone, of no kind the workspace draws, is still a file.
  const lone = planViews(BUILT_IN, file("model.rds", "application/octet-stream"), [], DRAWABLE);
  assert.deepEqual(lone.views.map((v) => v.view), ["file"]);
  // A view without a module is left out rather than drawn wrong.
  const noSurface = planViews(BUILT_IN, csv, [svg], new Set(["figure", "values", "file"]));
  assert.deepEqual(noSurface.views.map((v) => v.label), ["R figure", "Values"]);
});

test("a file's kind check reads in the broker's terms", () => {
  assert.equal(checkWords(null), null);
  assert.equal(checkWords({ state: "passed", checked_as: { id: "table.delimited", version: 1 }, reason: null }), "Passed as table.delimited version 1.");
  assert.equal(checkWords({ state: "not_checked" }), "Not checked yet.");
  // A file that fails keeps its bytes and opens as a plain file; the reason is the broker's, with one full stop.
  assert.equal(checkWords({ state: "failed", checked_as: { id: "table.delimited", version: 1 }, reason: "line 3 has 1 fields; the header has 2." }),
    "Did not pass as table.delimited version 1: line 3 has 1 fields; the header has 2. It opens as a plain file.");
});

// The broker's registry as the broker serves it: views named by what they show.
const brokerV3 = {
  registry: { digest: "sha256:" + "c".repeat(64), version: 1 },
  kinds: [
    { id: "table.delimited", version: 1, status: "accepted", profile_uri: "urn:enrichedlab:kind:table.delimited:1", media_types: ["text/csv", "text/tab-separated-values"],
      access: "paged", limits: {}, views: [{ view: "table", gpu: ["still"], fallback: "download" }] },
    { id: "image.svg", version: 1, status: "accepted", profile_uri: "urn:enrichedlab:kind:image.svg:1", media_types: ["image/svg+xml"], access: "whole", limits: {},
      views: [{ view: "svg-sandboxed", gpu: ["still"], fallback: "download" }] },
    { id: "document.pdf", version: 1, status: "accepted", profile_uri: "urn:enrichedlab:kind:document.pdf:1", media_types: ["application/pdf"], access: "whole", limits: {},
      views: [{ view: "pdf", gpu: ["still"], fallback: "download" }] },
    { id: "record.json", version: 1, status: "accepted", profile_uri: "urn:enrichedlab:kind:record.json:1", media_types: ["application/json"], access: "whole", limits: {},
      views: [{ view: "record", gpu: ["still"], fallback: "download" }] },
    { id: "text.plain", version: 1, status: "accepted", profile_uri: "urn:enrichedlab:kind:text.plain:1", media_types: ["text/plain"], access: "whole", limits: {},
      views: [{ view: "text", gpu: ["still"], fallback: "download" }] },
    { id: "file.opaque", version: 1, status: "accepted", profile_uri: "urn:enrichedlab:kind:file.opaque:1", media_types: [], access: "whole", limits: {},
      views: [{ view: "download", gpu: ["still"], fallback: "download" }] },
  ],
};

// Kinds say how a file gets them, the R figure kind is declared, and views may name the media types
// they draw and the channels a method maps to columns.
const brokerV5 = {
  registry: { digest: "sha256:" + "d".repeat(64), version: 2 },
  kinds: [
    { id: "figure.r", version: 1, status: "accepted", profile_uri: "urn:enrichedlab:kind:figure.r:1", media_types: ["image/svg+xml", "application/pdf", "image/png"],
      access: "whole", limits: {}, recognition: "declared", checked_as: ["image.svg", "document.pdf", "image.raster"], validator: "delegated",
      views: [{ view: "svg-sandboxed", gpu: ["still"], fallback: "download", media_types: ["image/svg+xml"] },
        { view: "pdf", gpu: ["still"], fallback: "download", media_types: ["application/pdf"] },
        { view: "image", gpu: ["still"], fallback: "download", media_types: ["image/png"] }] },
    { ...brokerV3.kinds[0], recognition: "media-type", views: [{ view: "table", gpu: ["still"], fallback: "download" },
      { view: "surface", gpu: ["webgpu", "webgl2", "still"], fallback: "image", encoding: ["x", "y", "z"] }] },
    ...brokerV3.kinds.slice(1).map((k) => ({ ...k, recognition: "media-type" })),
    { id: "image.raster", version: 1, status: "accepted", profile_uri: "urn:enrichedlab:kind:image.raster:1", media_types: ["image/png"], access: "whole",
      limits: {}, recognition: "media-type", views: [{ view: "image", gpu: ["still"], fallback: "download" }] },
  ],
};

test("a declared kind comes only from the code that wrote the file, and each of its views draws its own media types", () => {
  const registry = registryFrom(brokerV5);
  assert.notEqual(registry, BUILT_IN);
  // An SVG or PNG nobody declared reads as its media type's kind, though the R figure kind lists the same types first.
  assert.equal(kindOf(registry, { path: "figures/a.svg", media_type: "image/svg+xml" }).id, "image.svg");
  assert.equal(kindOf(registry, { path: "figures/a.png", media_type: "image/png" }).id, "image.raster");
  const declared = { id: "figure.r", version: 1 };
  for (const [path, type] of [["figures/a.svg", "image/svg+xml"], ["figures/a.pdf", "application/pdf"], ["figures/a.png", "image/png"]]) {
    const kind = kindOf(registry, { path, media_type: type, result_kind: declared });
    assert.equal(kind.id, "figure.r");
    assert.deepEqual(viewsOf(kind, null, type).map((v) => v.view), ["figure"], type);
  }
  // A type none of the kind's views draws is still a file.
  assert.deepEqual(viewsOf(kindOf(registry, { path: "figures/a.tiff", media_type: "image/tiff", result_kind: declared }), null, "image/tiff"), [{ view: "file" }]);
  // The switch names a declared R figure as one, and an undeclared drawing as an image.
  assert.deepEqual(planViews(registry, file("figures/a.svg", "image/svg+xml", { result_kind: declared }), [], DRAWABLE).views.map((v) => v.label), ["R figure"]);
  assert.deepEqual(planViews(registry, file("figures/a.svg", "image/svg+xml"), [], DRAWABLE).views.map((v) => v.label), ["Image"]);
});

test("the 3D view needs every channel the registry names, mapped to a column by the method", () => {
  const registry = registryFrom(brokerV5);
  const table = kindOf(registry, { path: "results/a.csv", media_type: "text/csv" });
  assert.deepEqual(viewsOf(table, null, "text/csv"), [{ view: "values" }]);
  assert.deepEqual(viewsOf(table, [{ view: "surface", encoding: { x: "hour", y: "block" } }], "text/csv"), [{ view: "values" }]);
  const surface = { view: "surface", encoding: { x: "hour", y: "block", z: "difference_c" } };
  assert.deepEqual(viewsOf(table, [{ view: "table" }, surface], "text/csv"), [{ view: "values" }, surface]);
  // Its fallback is the R figure of the same numbers.
  assert.equal(table.views.find((v) => v.view === "surface")?.fallback, "figure");
});

test("a registry whose view fields are not lists of names is not a registry", () => {
  const bad = { ...brokerV5, kinds: [{ ...brokerV5.kinds[0], views: [{ view: "pdf", gpu: ["still"], fallback: "download", media_types: "application/pdf" }] }] };
  assert.equal(registryFrom(bad), BUILT_IN);
  assert.equal(registryFrom({ ...brokerV5, kinds: [{ ...brokerV5.kinds[0], recognition: "guessed" }] }), BUILT_IN);
});

test("the broker's view names open the workspace's modules", () => {
  const registry = registryFrom(brokerV3);
  assert.notEqual(registry, BUILT_IN);
  const views = (path: string, media_type: string) => viewsOf(kindOf(registry, { path, media_type }), null).map((v) => v.view);
  assert.deepEqual(views("results/a.csv", "text/csv"), ["values"]);
  assert.deepEqual(views("figures/a.svg", "image/svg+xml"), ["figure"]);
  assert.deepEqual(views("figures/a.pdf", "application/pdf"), ["figure"]);
  assert.deepEqual(views("receipt.json", "application/json"), ["text"]);
  assert.deepEqual(views("archive.bin", "application/octet-stream"), ["file"]);
  // A file with a generic type falls back on its name, to whichever kind draws tables or text.
  assert.equal(kindOf(registry, { path: "results/b.tsv", media_type: "application/octet-stream" }).id, "table.delimited");
  assert.equal(kindOf(registry, { path: "code/run.R", media_type: "application/octet-stream" }).id, "text.plain");
  // A method's declared view in the broker's words opens the same module; the switch names a PDF as a PDF.
  assert.deepEqual(viewsOf(kindOf(registry, { path: "results/a.csv", media_type: "text/csv" }), [{ view: "table" }]).map((v) => v.view), ["values"]);
  const plan = planViews(registry, file("figures/a.pdf", "application/pdf"), [], new Set(["values", "figure", "text", "file", "surface"]));
  assert.deepEqual(plan.views.map((v) => v.label), ["PDF"]);
});
