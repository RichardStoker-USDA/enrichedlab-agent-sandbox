// The workspace's view modules. Each loads when a frame first shows it, so the 3D view's three.js stays
// out of the page until a 3D result opens. A new kind of result adds an entry here and a module that implements
// `ViewModule`; the frame does not change.
import { svelteView } from "./svelte_view.ts";
import type { ResultView } from "./types.ts";

export const VIEWS: Record<string, ResultView> = {
  surface: { id: "surface", version: 1, gpu: ["webgpu", "webgl2", "still"], load: async () => svelteView((await import("./surface/SurfaceView.svelte")).default) },
  figure: { id: "figure", version: 1, gpu: ["still"], load: async () => svelteView((await import("./FigureFrameView.svelte")).default) },
  values: { id: "values", version: 1, gpu: ["still"], load: async () => svelteView((await import("./ValuesView.svelte")).default) },
  text: { id: "text", version: 1, gpu: ["still"], load: async () => svelteView((await import("./TextFrameView.svelte")).default) },
  file: { id: "file", version: 1, gpu: ["still"], load: async () => svelteView((await import("./FileFacts.svelte")).default) },
};
export const DRAWABLE = new Set(Object.keys(VIEWS));
