// A page that shows one project follows it through the broker's project stream
// (`GET /api/v1/projects/{project}/events/stream`). The stream says only that something the page shows has changed and
// whether work is still running; the page then reads the project again through the ordinary endpoints, so the stream
// carries no research data and changes no access rule. A hidden page reads again when it is next shown. Without the
// stream (an older broker, or after repeated failures) the page reads again every few seconds while work runs, as it
// did before the stream existed.

export type Link = "connecting" | "live" | "reconnecting" | "polling";
/** `busy` is left out by a source that does not know it (the reader's change stream); the page's own reads say it. */
export interface Pulse { pulse: string; busy?: boolean }
export interface StreamHandlers {
  /** The first event of a connection: the project's fingerprint now. */
  state: (pulse: Pulse) => void;
  /** Something the page shows has changed. */
  changed: (pulse: Pulse) => void;
  /** The connection reached its lifetime; connect again. */
  renew: () => void;
  /** The project can no longer be read with this connection. */
  closed: () => void;
  error: () => void;
  /** Whether work still runs, from a source that reports it apart from changes (the reader's change stream). */
  busy?: (busy: boolean) => void;
}
export interface Follower {
  /** The page finished reading the project; `busy` says whether work is still running. */
  loaded: (busy: boolean) => void;
  /** The page became visible. */
  shown: () => void;
  stop: () => void;
  readonly link: Link;
}

/** Without the stream, a page reads again this often while work runs. */
export const POLL_MS = 5_000;
/** With the stream, a page still reads again this often while work runs: reviews, notes and acknowledgements are not
 *  part of the fingerprint. */
export const CHECK_BUSY_MS = 60_000;
/** And this often while nothing runs. */
export const CHECK_IDLE_MS = 300_000;
/** Connection attempts in a row before the page falls back to reading on a timer. */
export const RETRIES = 5;
/** A renewed connection waits this long, so a stream that ends at once cannot loop. */
export const RENEW_MS = 1_000;

export function followProject(options: {
  open: (handlers: StreamHandlers) => () => void;
  /** Read the project again. A promise lets changes that arrive during a read wait for it instead of cancelling it. */
  read: () => Promise<unknown> | void;
  hidden: () => boolean;
  link?: (link: Link) => void;
  busy?: (busy: boolean) => void;
  /** The stream said the project changed (not a timer or the page being shown). */
  changed?: () => void;
}): Follower {
  let link: Link = "connecting";
  let pulse = "";
  let busy = false;
  let stale = false;
  let again = false;
  let reading: Promise<unknown> | null = null;
  let retries = 0;
  let stopped = false;
  let close: (() => void) | undefined;
  let retryTimer: ReturnType<typeof setTimeout> | undefined;
  let checkTimer: ReturnType<typeof setTimeout> | undefined;

  const setLink = (next: Link) => { if (link !== next) { link = next; options.link?.(next); } };
  const setBusy = (next: boolean | undefined) => { if (next !== undefined && busy !== next) { busy = next; options.busy?.(next); } };
  const read = () => {
    if (stopped) return;
    if (reading) { again = true; return; }
    const pending = options.read();
    if (!pending) return;
    reading = Promise.resolve(pending).catch(() => undefined).finally(() => {
      reading = null;
      if (again) { again = false; want(); }
    });
  };
  // Read now, or when the page is next shown.
  const want = () => {
    if (stopped) return;
    if (options.hidden()) { stale = true; return; }
    stale = false;
    read();
  };
  const schedule = () => {
    clearTimeout(checkTimer);
    if (stopped) return;
    const wait = link === "live" ? (busy ? CHECK_BUSY_MS : CHECK_IDLE_MS) : busy ? POLL_MS : 0;
    if (!wait) return;
    checkTimer = setTimeout(() => {
      if (link === "live") want();
      // Without the stream a hidden page still reads, one interval later, so its tab title stays true.
      else if (options.hidden()) checkTimer = setTimeout(read, POLL_MS);
      else read();
    }, wait);
  };
  const lost = () => {
    close?.();
    close = undefined;
    if (stopped) return;
    if (++retries > RETRIES) { setLink("polling"); schedule(); return; }
    setLink("reconnecting");
    schedule();
    retryTimer = setTimeout(connect, Math.min(1000 * 2 ** (retries - 1), 15_000));
  };
  const connect = () => {
    if (stopped) return;
    try { open(); } catch { lost(); }
  };
  const open = () => {
    close = options.open({
      state: (next) => {
        retries = 0;
        // A change made while the page was not connected is read now; the first connection only sets the baseline.
        const missed = pulse !== "" && next.pulse !== pulse;
        pulse = next.pulse;
        setBusy(next.busy);
        const was = link;
        setLink("live");
        if (missed) { options.changed?.(); want(); }
        if (was !== "live") schedule();
      },
      changed: (next) => {
        if (next.pulse === pulse) return;
        pulse = next.pulse;
        setBusy(next.busy);
        options.changed?.();
        want();
      },
      renew: () => {
        close?.();
        close = undefined;
        retryTimer = setTimeout(connect, RENEW_MS);
      },
      closed: () => { want(); lost(); },
      error: lost,
      busy: (next) => { setBusy(next); schedule(); },
    });
  };
  connect();
  return {
    loaded(next) { setBusy(next); schedule(); },
    shown() { if (stale) want(); },
    stop() {
      stopped = true;
      clearTimeout(retryTimer);
      clearTimeout(checkTimer);
      close?.();
      close = undefined;
    },
    get link() { return link; },
  };
}

/** While the reader's change stream is connected, a project page needs no stream of its own: changes to the project
 *  reach the page through the stream's `project:<id>` key (live.ts), and whether work still runs arrives with them.
 *  The follower stays connected until that stream drops, then connects again, which opens the project's own stream. */
export function viaChanges(hub: { link: string; onLink: (listener: (link: string) => void) => () => void;
  onBusy: (listener: (busy: Record<string, boolean>) => void) => () => void }, project: string, on: StreamHandlers): () => void {
  let closed = false;
  const offBusy = hub.onBusy((busy) => { if (!closed && typeof busy[project] === "boolean") on.busy?.(busy[project]); });
  const off = hub.onLink((link) => { if (link !== "live" && !closed) { closed = true; off(); offBusy(); on.error(); } });
  queueMicrotask(() => { if (!closed) on.state({ pulse: "changes" }); });
  return () => { closed = true; off(); offBusy(); };
}
