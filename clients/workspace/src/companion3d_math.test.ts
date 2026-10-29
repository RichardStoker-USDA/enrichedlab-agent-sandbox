import assert from "node:assert/strict";
import test from "node:test";
import {
  COLUMN_FILL,
  COLUMN_SIDE_LIGHT,
  COMPANION_GREY,
  COMPANION_VIRIDIS,
  columnFaceLight,
  companionLayout,
  companionPalette,
  fitCompanionCamera,
  paletteColour,
  storedCategoryTicks,
  storedNumericTicks,
  type CompanionMathPoint,
} from "./companion3d_math.ts";

const point = (row: number, x: number, y: number, z: number): CompanionMathPoint => ({
  row, x, y, z, xText: `${x}.0`, yText: `${y}`, zText: z.toFixed(3), group: "",
});

test("a regular grid becomes one floor-based column per stored row and leaves gaps empty", () => {
  const points: CompanionMathPoint[] = [];
  let row = 0;
  for (const y of [1, 2, 4]) for (const x of [0, 1, 2]) {
    if (x === 1 && y === 2) continue;
    points.push(point(row++, x, y, x * x + y * y / 3 + (x === 2 && y === 4 ? 0.125 : 0)));
  }
  const layout = companionLayout(points, true);
  const low = Math.min(...points.map((source) => source.z));
  const high = Math.max(...points.map((source) => source.z));
  assert.equal(layout.mode, "surface");
  assert.equal(layout.fallback, null);
  assert.equal(layout.marks.length, points.length);
  assert.equal(layout.missingCells, 4); // one absent cell plus the three cells of the absent y = 3 row
  assert.ok(layout.marks.every((mark) => mark.column && mark.value === points.find((source) => source.row === mark.row)?.z));
  assert.ok(layout.marks.every((mark) => mark.column!.base === layout.floor
    && Math.abs(mark.column!.base + mark.column!.height - mark.position[1]) < 1e-12));
  assert.ok(layout.marks.every((mark) => Math.abs(mark.column!.width - COLUMN_FILL / 3) < 1e-12));
  assert.ok(layout.marks.every((mark) => Math.abs(mark.column!.depth - COLUMN_FILL / 4) < 1e-12));
  assert.ok(layout.marks.every((mark) => {
    const source = points.find((candidate) => candidate.row === mark.row)!;
    const exactStoredHeight = ((source.z - low) / (high - low)) * layout.bounds.height;
    return Math.abs(mark.column!.height - exactStoredHeight) < 1e-12;
  }), "every column height is the normalized stored cell value, with no fitted or interpolated value");
  assert.equal(layout.marks.some((mark) => mark.sourceX === 1 && mark.sourceY === 2), false);
  assert.equal(layout.marks.filter((mark) => mark.column!.height === 0).length, 1, "the minimum ends at the floor");
  assert.equal(columnFaceLight(1), 1);
  assert.equal(columnFaceLight(0), COLUMN_SIDE_LIGHT);
  assert.equal(columnFaceLight(-1), COLUMN_SIDE_LIGHT);
});

test("ticks are only exact retained labels spread across the stored range", () => {
  const retained = [
    { value: -1.25, text: "-1.2500" }, { value: -1, text: "-1.00" },
    { value: 0.125, text: ".125" }, { value: 0.5, text: "0.500" },
    { value: 1, text: "1e0" }, { value: 3.75, text: "3.750" },
    { value: 9, text: "09.0" }, { value: 20, text: "2.0e1" },
  ];
  const ticks = storedNumericTicks(retained);
  assert.ok(ticks.length <= 6);
  assert.deepEqual(ticks.at(0), retained[0]);
  assert.deepEqual(ticks.at(-1), retained.at(-1));
  assert.ok(ticks.every((tick) => retained.some((stored) => stored.value === tick.value && stored.text === tick.text)));
  assert.deepEqual(storedCategoryTicks(["first", "second", "first", "third"], 2).map((tick) => tick.text), ["first", "third"]);

  const grid: CompanionMathPoint[] = [];
  let row = 0;
  for (let y = 1; y <= 2; y++) for (let x = 0; x < 24; x++) grid.push(point(row++, x, y, x - y));
  const xTicks = companionLayout(grid, true).ticks.x;
  assert.ok(xTicks.length <= 6);
  assert.deepEqual([xTicks[0].text, xTicks.at(-1)?.text], [grid[0].xText, grid.at(-1)?.xText]);
  assert.ok(xTicks.every((tick) => grid.some((stored) => stored.x === tick.value && stored.xText === tick.text)));
});

test("an irregular or repeated grid falls back to stored points", () => {
  const irregular = [point(0, 0, 1, 1), point(1, 1, 1, 2), point(2, 2.5, 1, 3), point(3, 0, 2, 4), point(4, 1, 2, 5), point(5, 2.5, 2, 6)];
  assert.deepEqual([companionLayout(irregular, true).mode, companionLayout(irregular, true).fallback], ["points", "irregular"]);
  const repeated = [point(0, 0, 1, 1), point(1, 1, 1, 2), point(2, 0, 2, 3), point(3, 1, 2, 4), point(4, 1, 2, 5)];
  assert.deepEqual([companionLayout(repeated, true).mode, companionLayout(repeated, true).fallback], ["points", "repeated"]);
});

test("the request's known palette wins and viridis is the perceptual default", () => {
  const recorded = ["#102030", "#F0E0D0"];
  assert.deepEqual(companionPalette("unknown", recorded), { name: "recorded", stops: recorded, fromRequest: true });
  assert.deepEqual(companionPalette("grey"), { name: "grey", stops: COMPANION_GREY, fromRequest: true });
  assert.deepEqual(companionPalette("viridis"), { name: "viridis", stops: COMPANION_VIRIDIS, fromRequest: true });
  assert.deepEqual(companionPalette(null), { name: "viridis", stops: COMPANION_VIRIDIS, fromRequest: false });
  assert.deepEqual(companionPalette("unknown").stops, COMPANION_VIRIDIS);
  assert.deepEqual(paletteColour(COMPANION_VIRIDIS, 0).map((value) => Math.round(value * 255)), [0x44, 0x01, 0x54]);
  assert.deepEqual(paletteColour(COMPANION_VIRIDIS, 1).map((value) => Math.round(value * 255)), [0xfd, 0xe7, 0x25]);
});

test("camera fit frames the projected box to 85 percent at narrow and desktop aspects", () => {
  const bounds = { width: 1, height: 0.62, depth: 1 };
  const phone = fitCompanionCamera(bounds, 0.55);
  const desktop = fitCompanionCamera(bounds, 4 / 3);
  assert.ok(phone.distance > desktop.distance);
  assert.deepEqual([phone.azimuth, phone.elevation, phone.target], [35, 25, [0, 0, 0]]);
  const radius = Math.hypot(...phone.position);
  assert.ok(Math.abs(radius - phone.distance) < 1e-12);

  const azimuth = phone.azimuth * Math.PI / 180, elevation = phone.elevation * Math.PI / 180;
  const direction = [Math.sin(azimuth) * Math.cos(elevation), Math.sin(elevation), Math.cos(azimuth) * Math.cos(elevation)];
  const right = [Math.cos(azimuth), 0, -Math.sin(azimuth)];
  const up = [
    right[1] * -direction[2] - right[2] * -direction[1],
    right[2] * -direction[0] - right[0] * -direction[2],
    right[0] * -direction[1] - right[1] * -direction[0],
  ];
  const occupancy = (fit: ReturnType<typeof fitCompanionCamera>, aspect: number) => {
    const tanV = Math.tan(40 * Math.PI / 360), tanH = tanV * aspect;
    let horizontalMax = 0, verticalMax = 0;
    for (const x of [-bounds.width / 2, bounds.width / 2]) for (const y of [-bounds.height / 2, bounds.height / 2]) {
      for (const z of [-bounds.depth / 2, bounds.depth / 2]) {
        const depth = fit.distance - (x * direction[0] + y * direction[1] + z * direction[2]);
        const horizontal = Math.abs(x * right[0] + y * right[1] + z * right[2]) / (depth * tanH) * fit.projectionScale[0];
        const vertical = Math.abs(x * up[0] + y * up[1] + z * up[2]) / (depth * tanV) * fit.projectionScale[1];
        horizontalMax = Math.max(horizontalMax, horizontal);
        verticalMax = Math.max(verticalMax, vertical);
      }
    }
    return [horizontalMax, verticalMax];
  };
  for (const [fit, aspect] of [[phone, 0.55], [desktop, 4 / 3]] as const)
    for (const used of occupancy(fit, aspect)) assert.ok(Math.abs(used - 0.85) < 1e-12);
});
