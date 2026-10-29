// The interface every result view implements: a view mounts into an element with a context and gives
// back its state when it unmounts. The context is the only way a view reaches bytes, and every read goes through the
// broker's checked path; a view never fetches a URL of its own. A new kind of result is a registry entry and a view
// module that implements this, not a change to the frame.
import type { FigureControls } from "../contracts.ts";
import type { Gpu, Kind } from "../kinds.ts";
import type { ViewedFile } from "../viewer.ts";

/** A place in a result: the same-values group, the file, the row's index in the file (0 = first data row) and, for a
 *  text table, the line where the row starts. `values` names the row's stored values for the frame's caption. */
export interface Location { group: string | null; file: string; row: number; line?: number | null; values?: [string, string][] }

export interface PageQuery { offset: number; limit: number; sort?: string; order?: "asc" | "desc" }
/** One server page of a table: stored strings, each row with its index and line. */
export interface Page {
  path: string | null; sha256: string; columns: string[]; types: string[];
  rows: { index: number; line: number; values: string[] }[];
  total: number; offset: number; limit: number; sort: { column: string; order: "asc" | "desc" } | null;
}
/** Column summaries made by the broker's reviewed routine, with its receipt. */
export interface Summary {
  path: string | null; sha256: string;
  columns: { name: string; type: string; type_source: string; count: number; missing: number; min: string | null; max: string | null; distinct: number }[];
  receipt: { routine: string; version: number; input_sha256: string; output_sha256: string };
}

export type ViewState = Record<string, unknown>;

/** The figure editor on a table a figure can be drawn from: `charts` names the charts the render
 *  service offers, and `draw` opens the editor on this file with a chart and its settings filled in. R draws a preview
 *  first; nothing is saved until the person keeps a version. */
export interface FigureHandle {
  charts: string[];
  draw(spec: { feature: string; controls: FigureControls; columns: string[]; numeric: boolean[] }): void;
}

export interface ViewContext {
  file: ViewedFile;
  title: string;
  kind: Kind;
  /** The columns a view draws, as the method declared them; empty for views that need none. */
  encoding: Record<string, string>;
  /** The state the view gave back when it last unmounted, kept by the frame. */
  state: ViewState | null;
  /** Lets the frame keep the view's state (camera, scale, page) while it is mounted. */
  keep(state: ViewState): void;
  /** The whole file, checked against its recorded SHA-256. */
  read(signal?: AbortSignal): Promise<Blob>;
  /** Server pages and summaries of a table, when the broker offers them; null otherwise. */
  page: ((query: PageQuery, signal?: AbortSignal) => Promise<Page>) | null;
  summary: ((signal?: AbortSignal) => Promise<Summary>) | null;
  /** Picks go both ways between the views of one result. A handler hears the current pick when it registers. */
  pick(location: Location | null): void;
  onPick(handler: (location: Location | null) => void): () => void;
  download(): void;
  /** A figure's PDF original when the file shown is its SVG copy. */
  original: { file: ViewedFile; download(): void } | null;
  /** The group's R figure, which a GPU view shows when neither GPU path works. */
  still: { file: ViewedFile; read(signal?: AbortSignal): Promise<Blob>; download(): void } | null;
  /** The figure editor for this file, when the page offers one; null otherwise. */
  figure: FigureHandle | null;
  theme(): "light" | "dark";
  reducedMotion(): boolean;
}

export interface ViewModule { mount(el: HTMLElement, ctx: ViewContext): void; unmount(): ViewState | void }
/** A registered view: what it is called, what it needs from the GPU in order of preference, and how to load it. Each
 *  `load()` gives a fresh module, so two frames never share a view's state. */
export interface ResultView { id: string; version: number; gpu: Gpu[]; load(): Promise<ViewModule> }
