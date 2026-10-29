import type { IncomingMessage, ServerResponse } from "node:http";

// A running analysis's live output for the loopback preview: numbered pieces over Server-Sent
// Events, each sent once its time has come, resumed after the Last-Event-ID the browser sends when it reconnects (or
// ?after=, which the header overrides). The preview keeps no live output for a finished analysis, so its stream ends at
// once with recorded false, as the broker's does while live output is off. Every line is simulated.

// The SHA-256 of nothing, which a stream with no record names.
const NOTHING = "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855";

export interface ScriptPiece { after: number; text: string; stream?: "stdout" | "stderr" | "combined"; dropped?: number }

export function serveLog(req: IncomingMessage, res: ServerResponse, options: { started: number | null; attempt: number; ended: boolean; script: ScriptPiece[] }) {
  res.setHeader("Content-Type", "text/event-stream");
  res.setHeader("Cache-Control", "no-store");
  if (options.ended) {
    res.end(`event: closed\ndata: ${JSON.stringify({ attempt: null, recorded: false, pieces: 0, size: 0, sha256: NOTHING, dropped: 0, artifacts: [] })}\n\n`);
    return;
  }
  const resume = Number(req.headers["last-event-id"] ?? new URL(req.url ?? "/", "http://preview").searchParams.get("after") ?? 0);
  let sent = Number.isSafeInteger(resume) && resume > 0 ? resume : 0;
  const send = () => {
    if (options.started === null) return;
    const now = Date.now();
    while (sent < options.script.length && options.started + options.script[sent].after <= now) {
      const piece = options.script[sent];
      sent += 1;
      const data = { seq: sent, attempt: options.attempt, stream: piece.stream ?? "combined", text: piece.text, bytes: Buffer.byteLength(piece.text),
        at: new Date(options.started + piece.after).toISOString(), dropped: piece.dropped ?? 0 };
      res.write(`id: ${sent}\nevent: piece\ndata: ${JSON.stringify(data)}\n\n`);
    }
  };
  res.write("retry: 1000\n\n");
  send();
  let beats = 0;
  const timer = setInterval(() => {
    send();
    if (++beats % 30 === 0) res.write(": still running\n\n");
  }, 500);
  req.on("close", () => clearInterval(timer));
}

const SGR = (code: string) => `\x1b[${code}m`;

/** The residual check's output in the long run: counts and file names from the preview's own tables, a progress count
 *  that rewrites its line, one warning, a table of the blocks it read and the files it wrote. */
export function residualScript(blocks: number[], daysPerBlock: number, leftOut: number): ScriptPiece[] {
  const blockDays = blocks.length * daysPerBlock;
  const lines: [number, string][] = [
    [0, "Rscript code/check_residuals.R\n"],
    [600, `${SGR("1")}Residual check${SGR("22")}: afternoon model by block\n`],
    [1400, `Read ${blockDays} block-days from input/block_days.csv\n`],
    [2200, `Read ${blocks.length} block estimates from results/model_coefficients.csv\n`],
    [3200, "Residuals by block "],
    [3600, "  0%\r"], [4400, "Residuals by block  25%\r"], [5200, "Residuals by block  50%\r"], [6000, "Residuals by block  75%\r"],
    [6800, "Residuals by block 100%\n"],
    [7600, `${SGR("33")}Warning:${SGR("39")} block ${leftOut} has no summary and is left out of the residuals\n`],
    [8600, "Residuals by day "],
    [9200, "  0%\r"], [10000, "Residuals by day  50%\r"], [10800, "Residuals by day 100%\n"],
    [11800, `block\tblock_days\n${blocks.map((b) => `${b}\t${daysPerBlock}`).join("\n")}\n`],
    [13400, `${SGR("32")}Wrote${SGR("39")} results/residuals_by_block.csv\n`],
    [14200, `${SGR("32")}Wrote${SGR("39")} results/residuals_by_day.csv\n`],
    [15200, "Drawing the residual plots with the reviewed plot code; each plot is saved as an SVG copy beside the PDF R draws, so the figure that is published stays R's own\n"],
  ];
  return lines.map(([after, text]) => ({ after, text }));
}

/** Any other running analysis in the preview: the reviewed method's opening lines. */
export const GENERIC_SCRIPT: ScriptPiece[] = [
  { after: 0, text: "Rscript code/analysis.R\n" },
  { after: 800, text: "Checking each input against its recorded checksum\n" },
  { after: 1800, text: `${SGR("32")}All inputs match${SGR("39")}\n` },
  { after: 2800, text: "Running the reviewed method with the parameters in run_parameters.json\n" },
];
