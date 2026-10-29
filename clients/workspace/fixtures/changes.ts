import { createHash } from "node:crypto";
import type { IncomingMessage, ServerResponse } from "node:http";
import { Readable } from "node:stream";

// The reader's change stream in the preview (proposed broker shape): `GET /api/v1/changes/stream` sends
// `ready`, then `changed` with the keys whose records changed, `reset` when it cannot say, and `stream_complete` at its
// lifetime. The broker writes a change row in the same transaction as each change; the preview instead reads every
// watched record through its own routes after each change and on a short timer, and names the keys whose answers
// differ, which covers the reader's requests, the browser suite's preview triggers and the preview's own timers alike.
type Handle = (req: IncomingMessage, res: ServerResponse, next: () => void) => unknown;
type Json = (value: unknown, status?: number) => void;

const LIFETIME_MS = 15 * 60_000;
const TICK_MS = 600;
const KEPT = 2_000;

export function changeFeed(options: { handle: Handle; projects: () => string[]; tasks: () => string[]; labs: string[];
  busy: (project: string) => boolean | undefined }) {
  let on = true;
  let seq = 0;
  const log: { seq: number; keys: string[] }[] = [];
  const known = new Map<string, string>();
  const streams = new Set<ServerResponse>();
  let ticking: Promise<void> | null = null, again = false;
  let timer: ReturnType<typeof setInterval> | null = null;

  // One answer from the preview's own routes, as the reader would get it.
  const read = (path: string) => new Promise<string>((resolve) => {
    const req = Object.assign(Readable.from([]), { method: "GET", url: path, headers: { "x-fixture-internal": "1" } }) as unknown as IncomingMessage;
    const chunks: Buffer[] = [];
    const res = {
      statusCode: 200, headersSent: false,
      setHeader() { return res; }, getHeader() { return undefined; }, removeHeader() {}, flushHeaders() {}, on() { return res; },
      write(chunk: unknown) { chunks.push(Buffer.from(chunk as string)); return true; },
      end(chunk?: unknown) { if (chunk) chunks.push(Buffer.from(chunk as string)); resolve(`${res.statusCode} ${Buffer.concat(chunks).toString("utf8")}`); return res; },
    };
    Promise.resolve(options.handle(req, res as unknown as ServerResponse, () => resolve("none"))).catch(() => resolve("error"));
  });
  const fingerprint = async (paths: string[]) =>
    createHash("sha256").update((await Promise.all(paths.map(read))).join("\n")).digest("hex");
  const watched = (): [string, string[]][] => [
    ["inbox", ["/api/v1/inbox"]],
    ["projects", ["/api/v1/projects?limit=200"]],
    ["tasks", ["/api/v1/tasks?limit=200"]],
    ["commons", ["/api/v1/knowledge/commons?kind=dataset", "/api/v1/knowledge/commons/collections"]],
    ...options.labs.map((lab): [string, string[]] => [`lab:${lab}`, [`/api/v1/labs/${lab}/papers`, `/api/v1/labs/${lab}/notes`]]),
    ...options.projects().map((id): [string, string[]] => [`project:${id}`,
      [`/api/v1/projects/${id}`, `/api/v1/projects/${id}/runs`, `/api/v1/projects/${id}/reviews`, `/api/v1/projects/${id}/acknowledgements`]]),
    ...options.tasks().map((id): [string, string[]] => [`task:${id}`,
      [`/api/v1/tasks/${id}`, `/api/v1/tasks/${id}/reviews`, `/api/v1/tasks/${id}/figures`, `/api/v1/tasks/${id}/template-analyses`]]),
  ];
  const send = (res: ServerResponse, event: string, id: number | null, data: unknown) =>
    res.write(`${id === null ? "" : `id: ${id}\n`}event: ${event}\ndata: ${JSON.stringify(data)}\n\n`);
  // A change names, for each project among its keys, whether work still runs there, so a page that reads nothing while
  // hidden still knows when its project stops working.
  const changedEvent = (keys: string[]) => {
    const busy = Object.fromEntries(keys.filter((k) => k.startsWith("project:")).map((k) => [k.slice(8), options.busy(k.slice(8))])
      .filter(([, b]) => typeof b === "boolean"));
    return Object.keys(busy).length ? { keys, busy } : { keys };
  };

  async function detect() {
    const changed: string[] = [];
    for (const [key, paths] of watched()) {
      const print = await fingerprint(paths);
      if (known.has(key) && known.get(key) !== print) changed.push(key);
      known.set(key, print);
    }
    if (!changed.length) return;
    seq += 1;
    log.push({ seq, keys: changed });
    if (log.length > KEPT) log.splice(0, log.length - KEPT);
    const event = changedEvent(changed);
    for (const res of streams) send(res, "changed", seq, event);
  }
  /** Looks for changes now; a call during a look waits for it and looks once more. */
  function tick(): Promise<void> {
    if (ticking) { again = true; return ticking; }
    ticking = (async () => { do { again = false; try { await detect(); } catch { /* the next look tries again */ } } while (again); })()
      .finally(() => { ticking = null; });
    return ticking;
  }

  return {
    tick,
    /** Preview-only: a broker without the stream answers not found, and open streams drop. */
    setOn(next: boolean) {
      on = next;
      if (!on) for (const res of [...streams]) { streams.delete(res); res.destroy(); }
    },
    /** Preview-only: every open stream reaches its lifetime now. */
    renewAll() { for (const res of [...streams]) { streams.delete(res); send(res, "stream_complete", null, { reason: "renew" }); res.end(); } },
    /** Preview-only: every open stream drops, as a lost connection does. */
    dropAll() { for (const res of [...streams]) { streams.delete(res); res.destroy(); } },
    get open() { return streams.size; },
    /** Whether the preview serves the stream, which `/api/v1/me` says as the broker would. */
    get on() { return on; },
    async serve(req: IncomingMessage, res: ServerResponse, url: URL, json: Json) {
      if (!on) return json({ error: { code: "record_unavailable" } }, 404);
      res.setHeader("Content-Type", "text/event-stream");
      res.setHeader("Cache-Control", "no-store");
      res.setHeader("Connection", "keep-alive");
      res.flushHeaders();
      if (!known.size) await tick();
      const after = url.searchParams.get("after") ?? req.headers["last-event-id"];
      const from = typeof after === "string" && /^\d{1,12}$/.test(after) ? Number(after) : null;
      if (from !== null && from < seq) {
        const missed = log.filter((c) => c.seq > from);
        if (from < (log[0]?.seq ?? 1) - 1 || !missed.length) send(res, "reset", seq, { reason: "behind" });
        else send(res, "changed", seq, changedEvent([...new Set(missed.flatMap((c) => c.keys))]));
      }
      send(res, "ready", seq, {});
      streams.add(res);
      timer ??= setInterval(() => { if (streams.size) void tick(); }, TICK_MS);
      const keepalive = setInterval(() => res.write(": keepalive\n\n"), 15_000);
      const lifetime = setTimeout(() => { if (streams.delete(res)) { send(res, "stream_complete", null, { reason: "renew" }); res.end(); } }, LIFETIME_MS);
      req.on("close", () => {
        streams.delete(res); clearInterval(keepalive); clearTimeout(lifetime);
        if (!streams.size && timer) { clearInterval(timer); timer = null; }
      });
    },
    close() { if (timer) clearInterval(timer); timer = null; for (const res of streams) res.destroy(); streams.clear(); },
  };
}
