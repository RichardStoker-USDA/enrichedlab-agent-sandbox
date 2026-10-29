import assert from "node:assert/strict";
import test from "node:test";
import {
  buildFigureData,
  MAX_FIGURE_DATA_BYTES,
  MAX_FIGURE_DATA_ROWS,
  type FigureDataRecord,
  type FigureInputFile,
} from "./figure_data.ts";

const file = (path = "input/points.csv", size = 200): FigureInputFile => ({
  path,
  media_type: path.endsWith(".json") ? "application/json" : path.endsWith(".tsv") ? "text/tab-separated-values" : "text/csv",
  size_bytes: size,
  sha256: "a".repeat(64),
});

const mapped: FigureDataRecord = {
  controls: { x: "unused", y: "unused" },
  render: {
    feature: "surface",
    controls: {
      x: "dose", y: "response", z: "depth", colour: "treatment", facet: "site",
      x_label: "Dose", y_label: "Measured response", z_label: "Depth",
      palette: "grey",
      group_colors: { control: "#0072B2", treated: "#D55E00" },
    },
  },
  units: { x: "mg", y: "mm", z: "cm" },
};

test("encoding comes only from the render request mapping", () => {
  const source = "dose,response,depth,treatment,site,unused\n1,2,3,control,north,99\n2,4,6,treated,south,98\n3,8,9,control,north,97\n";
  const result = buildFigureData(mapped, file("input/points.csv", Buffer.byteLength(source)), source);
  assert.equal(result.ok, true);
  if (!result.ok) return;
  assert.deepEqual({
    dimensions: result.table.encoding.dimensions,
    x: result.table.encoding.x.column,
    y: result.table.encoding.y.column,
    z: result.table.encoding.z?.column,
    group: result.table.encoding.group?.column,
    facet: result.table.encoding.facet?.column,
    geometry: result.table.encoding.geometry,
    palette: result.table.encoding.palette,
  }, { dimensions: 3, x: "dose", y: "response", z: "depth", group: "treatment", facet: "site", geometry: "surface", palette: "grey" });
  assert.deepEqual(result.table.encoding.group_order, ["control", "treated"]);
  assert.deepEqual(result.table.encoding.facet_order, ["north", "south"]);
  assert.deepEqual([result.table.encoding.x.label, result.table.encoding.x.unit], ["Dose", "mg"]);
  assert.deepEqual([result.table.encoding.z?.label, result.table.encoding.z?.unit], ["Depth", "cm"]);
});

test("byte and row bounds refuse the whole table", () => {
  const small = "x,y\n1,2\n";
  const bytes = buildFigureData({ feature: "scatter", controls: { x: "x", y: "y" } }, file("input/points.csv", MAX_FIGURE_DATA_BYTES + 1), small);
  assert.deepEqual([bytes.ok, bytes.ok ? "" : bytes.code], [false, "bytes"]);

  const body = Array.from({ length: MAX_FIGURE_DATA_ROWS + 1 }, (_, index) => `${index},${index}`).join("\n");
  const source = `x,y\n${body}\n`;
  const rows = buildFigureData({ feature: "scatter", controls: { x: "x", y: "y" } }, file("input/points.csv", Buffer.byteLength(source)), source);
  assert.deepEqual([rows.ok, rows.ok ? "" : rows.code], [false, "rows"]);
});

test("unsupported files are not interpreted as tables", () => {
  const result = buildFigureData({ feature: "scatter", controls: { x: "x", y: "y" } },
    { path: "input/points.xlsx", media_type: "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet", size_bytes: 10 }, "not a sheet");
  assert.deepEqual([result.ok, result.ok ? "" : result.code], [false, "unsupported_file"]);
});

test("stored values stay exact in CSV, TSV and JSON", () => {
  const cases = [
    [file("input/points.csv", 80), "x,y,group\n0.0100,1e-09,A\n2.000,9007199254740991,B\n"],
    [file("input/points.tsv", 80), "x\ty\tgroup\n0.0100\t1e-09\tA\n2.000\t9007199254740991\tB\n"],
    [file("input/points.json", 160), '[{"x":0.0100,"y":1e-09,"group":"A"},{"x":2.000,"y":9007199254740991,"group":"B"}]'],
  ] as const;
  for (const [input, source] of cases) {
    input.size_bytes = Buffer.byteLength(source);
    const result = buildFigureData({ feature: "scatter", controls: { x: "x", y: "y", group: "group" } }, input, source);
    assert.equal(result.ok, true);
    if (!result.ok) continue;
    assert.deepEqual(result.table.rows.map((row) => row.cells.slice(0, 2)), [["0.0100", "1e-09"], ["2.000", "9007199254740991"]]);
  }
});

test("mapped coordinates are refused when doubles collide or underflow", () => {
  const collision = "x,y\n9007199254740992,1\n9007199254740993,2\n";
  const collided = buildFigureData({ feature: "scatter", controls: { x: "x", y: "y" } },
    file("input/points.csv", Buffer.byteLength(collision)), collision);
  assert.equal(collided.ok, false);
  if (!collided.ok) {
    assert.equal(collided.code, "inexact_numeric");
    assert.match(collided.message, /Exact placement not possible/);
  }

  const underflow = "x,y\n1e-400,1\n2e-400,2\n";
  const underflowed = buildFigureData({ feature: "scatter", controls: { x: "x", y: "y" } },
    file("input/points.csv", Buffer.byteLength(underflow)), underflow);
  assert.deepEqual([underflowed.ok, underflowed.ok ? "" : underflowed.code], [false, "inexact_numeric"]);

  const signedZero = "x,y\n-0,1\n1,2\n";
  const signLost = buildFigureData({ feature: "scatter", controls: { x: "x", y: "y" } },
    file("input/points.csv", Buffer.byteLength(signedZero)), signedZero);
  assert.deepEqual([signLost.ok, signLost.ok ? "" : signLost.code], [false, "inexact_numeric"]);
});

test("mapped coordinates are refused when an exact double range or layout intermediate is non-finite", () => {
  for (const axis of ["x", "y", "z"] as const) {
    const rows = [
      { x: "0", y: "1", z: "2" },
      { x: "1", y: "2", z: "3" },
    ];
    rows[0][axis] = "-1e308";
    rows[1][axis] = "1e308";
    const source = `x,y,z\n${rows.map((row) => `${row.x},${row.y},${row.z}`).join("\n")}\n`;
    const result = buildFigureData({ feature: "scatter", controls: { x: "x", y: "y", z: "z" } },
      file("input/points.csv", Buffer.byteLength(source)), source);
    assert.equal(result.ok, false, `${axis} span must be refused`);
    if (!result.ok) {
      assert.equal(result.code, "nonfinite_layout");
      assert.match(result.message, /Exact placement not possible/);
    }
  }

  const rows: string[] = [];
  for (const y of ["0", "1"]) for (const x of ["0", "8e307", "1.6e308"]) rows.push(`${x},${y},1`);
  const source = `x,y,z\n${rows.join("\n")}\n`;
  const result = buildFigureData({ feature: "surface", controls: { x: "x", y: "y", z: "z" } },
    file("input/points.csv", Buffer.byteLength(source)), source);
  assert.deepEqual([result.ok, result.ok ? "" : result.code], [false, "nonfinite_layout"]);

  const faceted = `x,y,z,facet\n${rows.map((row) => `${row},A`).join("\n")}\n1,2,1,B\n`;
  const facetedResult = buildFigureData({ feature: "surface", controls: { x: "x", y: "y", z: "z", facet: "facet" } },
    file("input/points.csv", Buffer.byteLength(faceted)), faceted);
  assert.deepEqual([facetedResult.ok, facetedResult.ok ? "" : facetedResult.code], [false, "nonfinite_layout"]);
});

test("the companion adds no columns and omits only rows without mapped coordinates", () => {
  const source = "x,y,group,note\n1,2,b,first\n3,4,,empty group\n,3,a,missing x\n4,5,b,last\n";
  const result = buildFigureData({ feature: "line", controls: { x: "x", y: "y", color: "group" } },
    file("input/points.csv", Buffer.byteLength(source)), source);
  assert.equal(result.ok, true);
  if (!result.ok) return;
  assert.deepEqual(result.table.columns.map((column) => column.name), ["x", "y", "group", "note"]);
  assert.deepEqual(result.table.rows.map((row) => row.cells), [["1", "2", "b", "first"], ["3", "4", "", "empty group"], ["", "3", "a", "missing x"], ["4", "5", "b", "last"]]);
  assert.deepEqual(result.table.points.map((point) => point.row), [0, 1, 3]);
  assert.equal(result.table.omitted_coordinate_rows, 1);
  assert.equal(result.table.encoding.chart, "line");
  assert.deepEqual(result.table.encoding.group_order, ["b", "", "a"]);
  assert.ok(result.table.columns.every((column) => !["normalized", "bin", "fit"].includes(column.name)));
});
