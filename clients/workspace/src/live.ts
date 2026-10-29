// Live data (the workspace should know "when to invalidate cache after a change for a user or
// agent"). Every view names what it shows by key: `inbox`, `projects`, `tasks`, `commons`, `project:<id>`, `task:<id>`,
// `dataset:<id>` and `lab:<key>`. A change to a key reads again only the views that show it: at once while the page is
// visible, and when it is next shown otherwise. Changes come from three places: the reader's own requests (each names
// what it changes, `keysFor`), the reader's other tabs (a broadcast channel), and the broker's change stream for what
// the agent and colleagues do (`GET /api/v1/changes/stream`). Without the stream (an older broker, or after repeated
// failures) lists read again on a timer and when the page is shown, as they did before. Keys name records and carry no
// research data; views read the records through the ordinary endpoints, which decide every access.

export type Key = string;
export type LiveLink = "connecting" | "live" | "polling";
/** Where a change came from: the reader's own request (sent on to their other tabs), another tab of theirs, something
 *  a page learned on its own (work on its project stopped), or the broker's stream and other news of the agent's
 *  progress, which reach a view at most once per its gap. */
export type Origin = "self" | "tab" | "derived" | "stream";

/** Without the stream, lists read again this often while the page is visible. */
export const POLL_MS = 60_000;
/** Connection attempts in a row before lists fall back to the timer. */
export const RETRIES = 5;
/** A renewed connection waits this long, so a stream that ends at once cannot loop. */
export const RENEW_MS = 1_000;
/** A broker that does not serve the stream is asked again this often. */
export const ABSENT_MS = 600_000;
/** Changes from the stream reach one view at most this often; the last one always arrives. */
export const GAP_MS = 1_000;
/** How long the workspace waits for the stream before reading its first lists without it. */
export const READY_WAIT_MS = 1_500;
export const CHANNEL = "enrichedlab-workspace-changes";

const KEY = /^(?:inbox|projects|tasks|commons|(?:project|task|dataset):[0-9a-f-]{36}|lab:[a-z][a-z0-9_-]{1,63})$/;
const CURSOR = /^[A-Za-z0-9._:-]{1,128}$/;
const UUID = "[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}";

/** Whether a change to `changed` reaches a view that shows `shown`. A key ending in ":*" names every key of its kind:
 *  a change whose exact record the client cannot name, or a view that shows several records of one kind. "*" names
 *  everything. */
export function touches(changed: Key, shown: Key): boolean {
  if (changed === "*" || shown === "*") return true;
  if (changed.endsWith(":*")) return shown.startsWith(changed.slice(0, -1));
  if (shown.endsWith(":*")) return changed.startsWith(shown.slice(0, -1));
  return changed === shown;
}

/** Whether what a view was told changed covers `key`. */
export const covers = (changed: Key[] | null, key: Key) => changed === null || changed.some((k) => touches(k, key));

const LIST_CHANGE: Key[] = ["projects", "tasks", "inbox"];
// What each request the workspace sends changes, by method and address. The broker's stream names the same keys for
// the same changes; this table lets the reader's own change show at once. A request it does not name changes
// everything the reader may be looking at, so a new route is never left stale.
const RULES: [RegExp, (m: RegExpMatchArray, body: Record<string, unknown>) => Key[]][] = [
  [/^\/api\/v1\/projects$/, () => ["projects"]],
  [new RegExp(`^/api/v1/projects/(${UUID})/look$`), () => ["projects"]],
  [new RegExp(`^/api/v1/projects/(${UUID})/figures/previews$`), () => []],
  [new RegExp(`^/api/v1/projects/(${UUID})/(?:prepare|checkpoints|grants|inspect|members)$`), (m) => [`project:${m[1]}`, "projects"]],
  [new RegExp(`^/api/v1/projects/(${UUID})/runs$`), (m) => [`project:${m[1]}`, "projects", "inbox"]],
  [new RegExp(`^/api/v1/projects/(${UUID})/(?:figures|template-analyses)$`),
    (m, b) => [`project:${m[1]}`, ...source(b), ...LIST_CHANGE]],
  [new RegExp(`^/api/v1/projects/(${UUID})/figures/(${UUID})/manuscript$`), (m) => [`project:${m[1]}`, `task:${m[2]}`, "task:*"]],
  [new RegExp(`^/api/v1/projects/(${UUID})/notes/share$`), (m) => [`project:${m[1]}`, "lab:*", "inbox"]],
  [new RegExp(`^/api/v1/projects/(${UUID})/datasets$`), (m) => [`project:${m[1]}`, "projects"]],
  [new RegExp(`^/api/v1/projects/(${UUID})/datasets/(${UUID})/(?:intake|preparations)$`),
    (m) => [`project:${m[1]}`, `dataset:${m[2]}`, ...LIST_CHANGE]],
  [new RegExp(`^/api/v1/research-runs/(${UUID})/pause$`), () => ["project:*", "projects", "inbox"]],
  [/^\/api\/v1\/tasks$/, (_, b) => [...LIST_CHANGE, ...(typeof b.project_id === "string" ? [`project:${b.project_id}`] : [])]],
  [new RegExp(`^/api/v1/tasks/(${UUID})/(?:reviews|cancel)$`), (m) => [`task:${m[1]}`, "project:*", ...LIST_CHANGE]],
  [/^\/api\/v1\/inbox\/acknowledgements$/, () => ["inbox", "project:*"]],
  [/^\/api\/v1\/notes\/[0-9a-f-]{36}\/(?:retire|renew)$/, () => ["lab:*", "project:*", "inbox"]],
  [/^\/api\/v1\/labs\/([a-z][a-z0-9_-]{1,63})\/papers(?:\/[0-9a-f-]{36}\/(?:acceptance|labels|remove))?$/, (m) => [`lab:${m[1]}`, "inbox"]],
  [/^\/api\/v1\/knowledge\//, () => ["commons", "lab:*"]],
];
function source(body: Record<string, unknown>): Key[] {
  return typeof body.source_task === "string" && new RegExp(`^${UUID}$`).test(body.source_task) ? [`task:${body.source_task}`] : [];
}

/** The keys a request changes when it succeeds; none for a read. */
export function keysFor(method: string, address: string, body?: unknown): Key[] {
  if (method === "GET" || method === "HEAD") return [];
  const path = address.split("?")[0];
  let parsed: Record<string, unknown> = {};
  if (typeof body === "string") { try { const v = JSON.parse(body); if (v && typeof v === "object") parsed = v; } catch { /* not JSON */ } }
  for (const [pattern, keys] of RULES) {
    const m = path.match(pattern);
    if (m) return keys(m, parsed);
  }
  return ["*"];
}

/** One read at a time: a call while a read runs waits for it and one more read after it, so the last answer always
 *  reflects the newest request. A failure ends the call quietly; the caller keeps what it had. */
export function serial(read: () => Promise<unknown>): () => Promise<void> {
  let current: Promise<void> | null = null, again = false;
  return () => {
    if (current) { again = true; return current; }
    current = (async () => {
      do { again = false; try { await read(); } catch { /* the view keeps its last answer */ } } while (again);
    })().finally(() => { current = null; });
    return current;
  };
}

export interface StreamOn {
  /** Connected; `cursor` is where a later connection resumes. */
  ready: (cursor: string | null) => void;
  /** `busy` says, for projects among the keys, whether work still runs on them. */
  changed: (keys: Key[], cursor: string | null, busy?: Record<string, boolean>) => void;
  /** The broker could not say what changed (the reader fell too far behind, or their access changed). */
  reset: (cursor: string | null) => void;
  /** The connection reached its lifetime; connect again. */
  renew: () => void;
  /** `fatal` when the broker refused the stream or does not serve it, as against a dropped connection. */
  error: (fatal: boolean) => void;
}

export interface LiveEnv {
  hidden: () => boolean;
  now: () => number;
  later: (run: () => void, ms: number) => () => void;
  /** Opens the change stream, resuming after `after` when given; returns its closer. Null when the browser has none. */
  open: ((after: string | null, on: StreamOn) => () => void) | null;
  /** The reader's other tabs; null when the browser has no broadcast channel. */
  channel: (receive: (message: unknown) => void) => { post: (message: unknown) => void; close: () => void } | null;
  /** Page shown, page hidden, browser back online. */
  listen: (on: (event: "shown" | "hidden" | "online") => void) => () => void;
}

/** A view's read. `changed` names the keys that changed since its last read, or is null when anything may have. */
export type Read = (changed: Key[] | null) => unknown;
interface Watch {
  keys: Key[];
  run: Read;
  /** What changed since the last read; null when anything may have. */
  pending: Set<Key> | null;
  whileHidden: boolean;
  poll: boolean;
  gapMs: number;
  stale: boolean;
  running: boolean;
  again: "" | "soon" | "now";
  lastStart: number;
  cancel?: () => void;
}
export interface WatchOptions {
  /** Read even while the page is hidden (the waiting list, whose count is in the tab title and the notices). */
  whileHidden?: boolean;
  /** Read on the timer and when the page is shown while there is no stream (the lists every page shows). */
  poll?: boolean;
  /** At most one read this often for changes from the stream. */
  gapMs?: number;
}

export class Live {
  link: LiveLink = "connecting";
  #env: LiveEnv;
  #watches = new Set<Watch>();
  #links = new Set<(link: LiveLink) => void>();
  #busy = new Set<(busy: Record<string, boolean>) => void>();
  #cursor: string | null = null;
  #retries = 0;
  #started = false;
  #stream = true;
  #ran = false;
  #close?: () => void;
  #retry?: () => void;
  #poll?: () => void;
  #channel: { post: (message: unknown) => void; close: () => void } | null = null;
  #unlisten?: () => void;
  #settle?: () => void;
  #ready: Promise<void>;

  constructor(env: LiveEnv) {
    this.#env = env;
    this.#ready = new Promise((settle) => { this.#settle = settle; });
  }

  /** Reads `run` again whenever a key it shows changes. Returns the way to stop. */
  watch(keys: Key[], run: Read, options: WatchOptions = {}): () => void {
    const w: Watch = { keys, run, pending: new Set(), whileHidden: !!options.whileHidden, poll: !!options.poll, gapMs: Math.max(options.gapMs ?? GAP_MS, 0),
      stale: false, running: false, again: "", lastStart: -Infinity };
    this.#watches.add(w);
    return () => { this.#watches.delete(w); w.cancel?.(); w.cancel = undefined; };
  }

  /** Something changed. The reader's own changes reach every view at once and their other tabs; changes from the
   *  stream reach a view at most once per its gap. */
  changed(keys: Key[], from: Origin = "self") {
    if (!keys.length) return;
    if (from === "self") this.#channel?.post({ type: "changed", keys });
    const urgent = from !== "stream";
    for (const w of this.#watches) {
      const hit = keys.filter((k) => w.keys.some((s) => touches(k, s)));
      if (hit.length) this.#kick(w, urgent, hit);
    }
  }

  /** Every view reads again (visible) or when next shown (hidden). */
  reset() { for (const w of this.#watches) this.#kick(w, true, null); }

  /** The page was shown: views changed while it was hidden read now, and without the stream so do the lists. */
  shown() {
    for (const w of this.#watches) {
      if (this.link !== "live" && w.poll) this.#kick(w, true, null);
      else if (w.stale) this.#kick(w, true, []);
    }
  }

  onLink(listener: (link: LiveLink) => void): () => void {
    this.#links.add(listener);
    return () => this.#links.delete(listener);
  }

  /** Whether work runs on projects the stream names, as each change arrives, so a hidden page that reads nothing still
   *  knows when its project stops working (the tab title says so). */
  onBusy(listener: (busy: Record<string, boolean>) => void): () => void {
    this.#busy.add(listener);
    return () => this.#busy.delete(listener);
  }

  /** Settles once the stream is connected, known to be unavailable, or after `ms`, whichever is first. */
  whenReady(ms = READY_WAIT_MS): Promise<void> {
    return Promise.race([this.#ready, new Promise<void>((settle) => { this.#env.later(settle, ms); })]);
  }

  /** `stream: false` when the broker does not serve the change stream: lists read on the timer and when shown, and the
   *  reader's own changes still reach every view and tab at once. */
  start({ stream = true }: { stream?: boolean } = {}) {
    if (this.#started) return;
    this.#started = true;
    this.#stream = stream;
    this.#channel = this.#env.channel((message) => this.#fromTab(message));
    this.#unlisten = this.#env.listen((event) => {
      if (event === "shown") this.shown();
      else if (event === "online") this.#online();
    });
    this.#schedulePoll();
    this.#connect();
  }

  stop() {
    this.#started = false;
    this.#close?.(); this.#close = undefined;
    this.#retry?.(); this.#retry = undefined;
    this.#poll?.(); this.#poll = undefined;
    this.#channel?.close(); this.#channel = null;
    this.#unlisten?.(); this.#unlisten = undefined;
  }

  #setLink(next: LiveLink) {
    if (this.link === next) return;
    this.link = next;
    for (const listener of this.#links) listener(next);
  }

  // `keys` adds to what the view's next read is told changed; null means anything may have.
  #kick(w: Watch, urgent: boolean, keys: Key[] | null) {
    if (!this.#watches.has(w)) return;
    if (keys === null) w.pending = null; else if (w.pending) for (const k of keys) w.pending.add(k);
    if (this.#env.hidden() && !w.whileHidden) { w.stale = true; return; }
    if (w.running) { if (urgent || !w.again) w.again = urgent ? "now" : "soon"; return; }
    const wait = urgent ? 0 : w.lastStart + w.gapMs - this.#env.now();
    if (wait > 0) {
      w.cancel ??= this.#env.later(() => { w.cancel = undefined; this.#kick(w, true, []); }, wait);
      return;
    }
    w.cancel?.(); w.cancel = undefined;
    const changed = w.pending === null ? null : [...w.pending];
    w.pending = new Set();
    w.stale = false; w.running = true; w.lastStart = this.#env.now(); this.#ran = true;
    void Promise.resolve().then(() => w.run(changed)).catch(() => undefined).finally(() => {
      w.running = false;
      const again = w.again;
      w.again = "";
      if (again) this.#kick(w, again === "now", []);
    });
  }

  #fromTab(message: unknown) {
    const m = message as { type?: unknown; keys?: unknown } | null;
    if (m?.type !== "changed" || !Array.isArray(m.keys)) return;
    const keys = m.keys.filter((k): k is string => typeof k === "string" && (KEY.test(k) || k === "*" || /^[a-z]+:\*$/.test(k))).slice(0, 64);
    this.changed(keys, "tab");
  }

  #schedulePoll() {
    this.#poll = this.#env.later(() => {
      if (!this.#started) return;
      if (this.link !== "live" && !this.#env.hidden()) for (const w of this.#watches) if (w.poll) this.#kick(w, true, null);
      this.#schedulePoll();
    }, POLL_MS);
  }

  #connect() {
    this.#retry?.(); this.#retry = undefined;
    this.#close?.(); this.#close = undefined;
    if (!this.#started) return;
    const open = this.#stream ? this.#env.open : null;
    if (!open) { this.#setLink("polling"); this.#settle?.(); return; }
    let closed = false;
    const finish = () => { if (closed) return false; closed = true; this.#close?.(); this.#close = undefined; return true; };
    this.#close = open(this.#cursor, {
      ready: (cursor) => {
        if (closed) return;
        // A first connection after lists were read without it cannot say what changed in between.
        const unknown = this.#cursor === null && this.#ran;
        this.#retries = 0;
        if (cursor) this.#cursor = cursor;
        this.#setLink("live");
        this.#settle?.();
        if (unknown) this.reset();
      },
      changed: (keys, cursor, busy) => {
        if (closed) return;
        if (cursor) this.#cursor = cursor;
        if (busy) for (const listener of this.#busy) listener(busy);
        this.changed(keys, "stream");
      },
      reset: (cursor) => { if (closed) return; if (cursor) this.#cursor = cursor; this.reset(); },
      renew: () => { if (!finish()) return; this.#retry = this.#env.later(() => this.#connect(), RENEW_MS); },
      error: (fatal) => { if (finish()) this.#lost(fatal); },
    });
  }

  #lost(fatal: boolean) {
    if (!this.#started) return;
    if (fatal && this.link !== "live" && this.#cursor === null) {
      // The broker does not serve the stream or refused it: lists read on the timer, and the stream is tried later.
      this.#setLink("polling");
      this.#settle?.();
      this.#retry = this.#env.later(() => this.#connect(), ABSENT_MS);
      return;
    }
    this.#retries++;
    this.#setLink(this.#retries > RETRIES ? "polling" : "connecting");
    if (this.#retries > RETRIES) this.#settle?.();
    this.#retry = this.#env.later(() => this.#connect(), Math.min(1_000 * 2 ** Math.min(this.#retries - 1, 5), 30_000));
  }

  #online() {
    if (this.link === "live") return;
    if (!this.#env.hidden()) for (const w of this.#watches) if (w.poll) this.#kick(w, true, null);
    this.#retries = 0;
    this.#connect();
  }
}

/** Whether a shared list a page opens on should be read first: never read yet, or read more than `ms` ago while no
 *  stream says when it changes. */
export function mayBeOld(readAt: number, link: LiveLink, now: number, ms = 5_000): boolean {
  return readAt === 0 || (link !== "live" && now - readAt > ms);
}

const PROJECT_ID = new RegExp(`^${UUID}$`);
/** One change-stream event: its keys, and whether work runs on the projects it names; null when the event is not one
 *  the workspace reads. */
export function changeEvent(data: string): { keys: Key[]; busy?: Record<string, boolean> } | null {
  try {
    const v = JSON.parse(data) as { keys?: unknown; busy?: unknown };
    if (!Array.isArray(v?.keys) || v.keys.length > 500) return null;
    const keys = v.keys.filter((k): k is string => typeof k === "string" && KEY.test(k));
    if (v.busy === undefined) return { keys };
    if (!v.busy || typeof v.busy !== "object" || Array.isArray(v.busy)) return null;
    const busy = Object.fromEntries(Object.entries(v.busy as Record<string, unknown>)
      .filter(([id, value]) => PROJECT_ID.test(id) && typeof value === "boolean").slice(0, 500)) as Record<string, boolean>;
    return { keys, busy };
  } catch { return null; }
}
export const cursorOf = (id: string): string | null => (CURSOR.test(id) ? id : null);

// The broker's change stream over EventSource. Each connection resumes after the last cursor; the stream sends `ready`
// once, `changed` for each change the reader may see, `reset` when it cannot say what changed, and `stream_complete`
// at its lifetime. An event the workspace cannot read ends the connection, as a dropped one does.
export function openChangeStream(after: string | null, on: StreamOn): () => void {
  const source = new EventSource(`/api/v1/changes/stream${after ? `?after=${encodeURIComponent(after)}` : ""}`);
  let done = false;
  const finish = (then: () => void) => { if (done) return; done = true; source.close(); then(); };
  const idOf = (m: Event) => cursorOf((m as MessageEvent).lastEventId ?? "");
  source.addEventListener("ready", (m) => { if (!done) on.ready(idOf(m)); });
  source.addEventListener("changed", (m) => {
    if (done) return;
    const event = changeEvent((m as MessageEvent).data);
    if (event === null) finish(() => on.error(false)); else on.changed(event.keys, idOf(m), event.busy);
  });
  source.addEventListener("reset", (m) => { if (!done) on.reset(idOf(m)); });
  source.addEventListener("stream_complete", () => finish(on.renew));
  source.onerror = () => { const fatal = source.readyState === EventSource.CLOSED; finish(() => on.error(fatal)); };
  return () => { done = true; source.close(); };
}

export function browserEnv(): LiveEnv {
  return {
    hidden: () => typeof document !== "undefined" && document.hidden,
    now: () => Date.now(),
    later: (run, ms) => { const timer = setTimeout(run, ms); return () => clearTimeout(timer); },
    open: typeof EventSource === "undefined" ? null : openChangeStream,
    channel: (receive) => {
      if (typeof BroadcastChannel === "undefined") return null;
      const channel = new BroadcastChannel(CHANNEL);
      channel.onmessage = (event) => receive(event.data);
      return { post: (message) => channel.postMessage(message), close: () => channel.close() };
    },
    listen: (on) => {
      const shown = () => on(document.hidden ? "hidden" : "shown");
      const online = () => on("online");
      document.addEventListener("visibilitychange", shown);
      addEventListener("online", online);
      return () => { document.removeEventListener("visibilitychange", shown); removeEventListener("online", online); };
    },
  };
}

// The workspace's one hub. Nothing runs until the shell starts it after sign-in, so modules that only name changes
// (the request helper) can load anywhere, tests included.
let shared: Live | null = null;
export function live(): Live {
  shared ??= new Live(browserEnv());
  return shared;
}
/** For the request helper: a change the reader made. Silent before the hub exists. */
export function changedBySelf(keys: Key[]) { if (keys.length) shared?.changed(keys, "self"); }
