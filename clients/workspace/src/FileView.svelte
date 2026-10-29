<script lang="ts">
  import ResultFrame from "./ResultFrame.svelte";
  import { plainResult, type ResultFile } from "./results.ts";
  import type { ViewedFile } from "./viewer.ts";
  import type { FigureHandle } from "./views/types.ts";

  // A retained file in its result frame. Callers that know the file's package pass `result` (and the files holding the
  // same numbers as `group`), which gives the frame the broker's pages and the method's declared views; otherwise the
  // frame draws the file by its kind from the caller's checked loader. A figure's PDF original, when the file shown is
  // its SVG copy, is saved from the figure's own Download PDF.
  let { file, load, onDownload, title, showTitle = true, original = null, onOriginal, result = null, group = [], onExpand, figure = null }: {
    file: ViewedFile; load: (signal: AbortSignal) => Promise<Blob>; onDownload: () => void; title: string; showTitle?: boolean;
    original?: ViewedFile | null; onOriginal?: () => void; result?: ResultFile | null; group?: ResultFile[]; onExpand?: () => void;
    figure?: ((file: ResultFile) => FigureHandle | null) | null;
  } = $props();
  const subject = $derived(result ?? plainResult(file, (signal) => load(signal ?? new AbortController().signal), onDownload,
    original ? { file: original, download: onOriginal ?? (() => {}) } : null));
</script>

{#key `${subject.file.path}|${subject.file.sha256}`}
  <ResultFrame {subject} {group} {title} {showTitle} {onExpand} {figure} />
{/key}
