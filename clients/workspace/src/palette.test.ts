import assert from "node:assert/strict";
import { readdirSync, readFileSync } from "node:fs";
import { join } from "node:path";
import test from "node:test";

// UC Davis rules out mixing your own colours: "Custom mix. Straight tints of the blue will not work;
// please use only the color values below" (marketingtoolbox.ucdavis.edu/brand-guide/colors). The reference application and
// the shared platform frontend enforce the same published set.
const PUBLISHED = new Set([
  "#022851", "#FFBF00",
  "#033266", "#1D4776", "#355B85", "#4F7094", "#6884A3", "#8198B2", "#9AADC2", "#B3C1D1", "#CDD6E0",
  "#FFC519", "#FFCC33", "#FFD24C", "#FFD966", "#FFDF80", "#FFE599", "#FFECB2", "#FFF2CC", "#FFF9E5",
  "#000000", "#191919", "#333333", "#4C4C4C", "#666666", "#7F7F7F", "#999999", "#B2B2B2", "#CCCCCC", "#E5E5E5", "#FFFFFF",
  "#003A5D", "#0047BA", "#008EAA", "#00B2E3", "#6FCFEB", "#00C4B3", "#03F9E6", "#00524C", "#266041", "#3DAE2B",
  "#6CCA98", "#AADA91", "#FFFF3B", "#FFDC00", "#F18A00", "#8A532F", "#79242F", "#C10230", "#F93549", "#FF8189",
  "#C6007E", "#F095CD", "#76236C", "#481268",
]);
// Shadows, scrims and washes are a published colour at an opacity.
const PUBLISHED_RGB = new Set(["0 0 0", "255 255 255", "2 40 81", "25 25 25", "0,0,0", "255,255,255", "2,40,81", "25,25,25"]);

const here = import.meta.dirname;
const sources = readdirSync(here).filter((f) => /\.(svelte|css)$/.test(f)).map((f) => [f, readFileSync(join(here, f), "utf8")] as const);

test("every colour the workspace declares is one UC Davis publishes", () => {
  const offenders: string[] = [];
  for (const [file, text] of sources) {
    for (const m of text.matchAll(/#[0-9a-fA-F]{6}\b/g)) if (!PUBLISHED.has(m[0].toUpperCase())) offenders.push(`${file}: ${m[0]}`);
    for (const m of text.matchAll(/rgba?\(\s*(\d+)[\s,]+(\d+)[\s,]+(\d+)/g)) {
      const triple = `${m[1]} ${m[2]} ${m[3]}`;
      if (!PUBLISHED_RGB.has(triple)) offenders.push(`${file}: rgb(${triple})`);
    }
    if (/oklch\(|color-mix\(/.test(text)) offenders.push(`${file}: a derived colour`);
  }
  assert.deepEqual(offenders, []);
});
