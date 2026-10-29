import assert from "node:assert/strict";
import test from "node:test";
import { buildGrid, viridis, type Grid } from "./views/surface/grid.ts";

// A small block-by-hour table: blocks 1, 2 and 4 (block 3 has no rows), hours 0 to 2.
const table = (skip: (block: number, hour: number) => boolean = () => false) => {
  const x: string[] = [], y: string[] = [], z: string[] = [], rows: number[] = [];
  let row = 0;
  for (const block of [1, 2, 4]) for (const hour of [0, 1, 2]) {
    if (!skip(block, hour)) { x.push(String(hour)); y.push(String(block)); z.push((block + hour / 10).toFixed(3)); rows.push(row); }
    row++;
  }
  return { x, y, z, rows };
};

test("each row is one vertex at its stored values, and a block the table does not hold leaves a gap", () => {
  const grid = buildGrid(table()) as Grid;
  assert.deepEqual(grid.xs, [0, 1, 2]);
  assert.deepEqual(grid.ys, [1, 2, 4]);
  assert.deepEqual(grid.gaps, { x: [], y: [3] });
  // Two cells between blocks 1 and 2 (four triangles); none across the missing block 3.
  assert.equal(grid.indices.length, 4 * 3);
  // The lowest and highest heights keep the strings the file holds.
  assert.equal(grid.low, "1.000");
  assert.equal(grid.high, "4.200");
  // Every vertex names its row in the file, and its height is its stored value placed between the extremes.
  assert.deepEqual([...grid.rows], [0, 1, 2, 3, 4, 5, 6, 7, 8]);
  assert.ok(Math.abs(grid.levels[4] - (2.1 - 1) / 3.2) < 1e-6);
  // The table's x runs along x and its y along -z, so block 1 is at the front (+z) and block 4 at the back.
  const vertex = (i: number) => [...grid.positions.slice(i * 3, i * 3 + 3)].map((v) => Math.round(v * 1000) / 1000);
  assert.deepEqual(vertex(0), [-0.5, 0, 0.5]);
  assert.deepEqual(vertex(8), [0.5, 1, -0.5]);
});

test("a missing height leaves its cells out instead of drawing across them", () => {
  const grid = buildGrid(table((block, hour) => block === 1 && hour === 1)) as Grid;
  // Of the two cells between blocks 1 and 2, both touch the missing vertex.
  assert.equal(grid.indices.length, 0);
  const na = table();
  na.z[4] = "NA";
  const withNa = buildGrid(na) as Grid;
  assert.equal(withNa.missing, 1);
  assert.equal(withNa.rows.length, 8);
});

test("a table that is not one surface is refused rather than guessed", () => {
  const repeated = table();
  repeated.x.push("0"); repeated.y.push("1"); repeated.z.push("9.000"); repeated.rows.push(99);
  assert.equal(buildGrid(repeated), "repeated");
  const words = table();
  words.x[0] = "noon";
  assert.equal(buildGrid(words), "not_numeric");
  assert.equal(buildGrid({ x: ["0", "1"], y: ["1", "1"], z: ["1", "2"], rows: [0, 1] }), "too_few");
});

test("viridis runs from its first stop to its last", () => {
  assert.deepEqual(viridis(0).map((v) => Math.round(v * 255)), [0x44, 0x01, 0x54]);
  assert.deepEqual(viridis(1).map((v) => Math.round(v * 255)), [0xfd, 0xe7, 0x25]);
  assert.deepEqual(viridis(-1), viridis(0));
});
