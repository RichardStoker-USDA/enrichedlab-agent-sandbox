import { afterEach, beforeEach, mock, test } from "node:test";
import assert from "node:assert/strict";
import { CHECK_BUSY_MS, CHECK_IDLE_MS, followProject, POLL_MS, RENEW_MS, RETRIES, viaChanges, type Link, type StreamHandlers } from "./follow.ts";

const A = "a".repeat(32), B = "b".repeat(32), C = "c".repeat(32), D = "d".repeat(32);
const flush = () => new Promise<void>((resolve) => setImmediate(resolve));

beforeEach(() => mock.timers.enable({ apis: ["setTimeout"] }));
afterEach(() => mock.timers.reset());

// A follower over a stand-in stream: each connection is recorded, and each read waits until the test finishes it.
function harness(hidden = false) {
  const streams: { on: StreamHandlers; closed: boolean }[] = [];
  const pending: (() => void)[] = [];
  const links: Link[] = [];
  const busy: boolean[] = [];
  let reads = 0;
  let isHidden = hidden;
  const follower = followProject({
    open: (on) => { const stream = { on, closed: false }; streams.push(stream); return () => { stream.closed = true; }; },
    read: () => { reads++; return new Promise<void>((resolve) => pending.push(resolve)); },
    hidden: () => isHidden,
    link: (l) => links.push(l),
    busy: (b) => busy.push(b),
  });
  return {
    follower, streams, links, busy,
    get reads() { return reads; },
    get last() { return streams.at(-1)!.on; },
    async finish() { pending.shift()?.(); await flush(); },
    hide(value: boolean) { isHidden = value; },
  };
}

test("a change reads the project again; the first state and a repeated fingerprint do not", () => {
  const h = harness();
  h.last.state({ pulse: A, busy: true });
  assert.equal(h.reads, 0);
  assert.equal(h.follower.link, "live");
  assert.deepEqual(h.busy, [true]);
  h.last.changed({ pulse: B, busy: true });
  assert.equal(h.reads, 1);
  h.last.changed({ pulse: B, busy: true });
  assert.equal(h.reads, 1);
});

test("changes during a read wait for it and become one more read", async () => {
  const h = harness();
  h.last.state({ pulse: A, busy: true });
  h.last.changed({ pulse: B, busy: true });
  h.last.changed({ pulse: C, busy: true });
  h.last.changed({ pulse: D, busy: false });
  assert.equal(h.reads, 1);
  await h.finish();
  assert.equal(h.reads, 2);
  await h.finish();
  assert.equal(h.reads, 2);
  assert.deepEqual(h.busy, [true, false]);
});

test("a hidden page reads once when it is shown, and its title still learns that work stopped", () => {
  const h = harness(true);
  h.last.state({ pulse: A, busy: true });
  h.last.changed({ pulse: B, busy: false });
  h.last.changed({ pulse: C, busy: false });
  assert.equal(h.reads, 0);
  assert.deepEqual(h.busy, [true, false]);
  h.hide(false);
  h.follower.shown();
  h.follower.shown();
  assert.equal(h.reads, 1);
});

test("a live page still reads once a minute while work runs and every five minutes otherwise", async () => {
  const h = harness();
  h.last.state({ pulse: A, busy: true });
  mock.timers.tick(CHECK_BUSY_MS - 1);
  assert.equal(h.reads, 0);
  mock.timers.tick(1);
  assert.equal(h.reads, 1);
  await h.finish();
  h.follower.loaded(false);
  mock.timers.tick(CHECK_IDLE_MS - 1);
  assert.equal(h.reads, 1);
  mock.timers.tick(1);
  assert.equal(h.reads, 2);
});

test("after repeated failures the page reads every five seconds while work runs, as it did before the stream", async () => {
  const h = harness();
  for (let i = 0; i < RETRIES; i++) {
    h.last.error();
    assert.equal(h.follower.link, "reconnecting");
    mock.timers.tick(15_000);
  }
  assert.equal(h.streams.length, RETRIES + 1);
  h.last.error();
  assert.equal(h.follower.link, "polling");
  assert.deepEqual(h.links, ["reconnecting", "polling"]);
  h.follower.loaded(true);
  mock.timers.tick(POLL_MS);
  assert.equal(h.reads, 1);
  await h.finish();
  // A hidden page without the stream waits one more interval and then reads, so its title stays true.
  h.hide(true);
  h.follower.loaded(true);
  mock.timers.tick(POLL_MS);
  assert.equal(h.reads, 1);
  mock.timers.tick(POLL_MS);
  assert.equal(h.reads, 2);
  await h.finish();
  h.follower.loaded(false);
  mock.timers.tick(10 * CHECK_IDLE_MS);
  assert.equal(h.reads, 2);
  assert.equal(h.streams.length, RETRIES + 1);
});

test("while it reconnects a busy page reads on the old timer, and a successful connection resets the count", () => {
  const h = harness();
  h.follower.loaded(true);
  h.last.error();
  mock.timers.tick(POLL_MS);
  assert.equal(h.reads, 1);
  assert.equal(h.streams.length, 2, "reconnected after one second");
  h.last.state({ pulse: A, busy: true });
  assert.equal(h.follower.link, "live");
  for (let i = 0; i < RETRIES; i++) { h.last.error(); mock.timers.tick(15_000); }
  assert.equal(h.follower.link, "reconnecting");
});

test("a renewed connection stays live and reads only a change made in between", () => {
  const h = harness();
  h.last.state({ pulse: A, busy: false });
  h.last.renew();
  assert.equal(h.follower.link, "live");
  assert.equal(h.streams[0].closed, true);
  mock.timers.tick(RENEW_MS);
  h.last.state({ pulse: A, busy: false });
  assert.equal(h.reads, 0);
  h.last.renew();
  mock.timers.tick(RENEW_MS);
  h.last.state({ pulse: B, busy: true });
  assert.equal(h.reads, 1);
  assert.deepEqual(h.links, ["live"]);
});

test("a closed stream reads the page, which then shows whether the project is still open, and tries again", () => {
  const h = harness();
  h.last.state({ pulse: A, busy: false });
  h.last.closed();
  assert.equal(h.reads, 1);
  assert.equal(h.follower.link, "reconnecting");
  mock.timers.tick(1000);
  assert.equal(h.streams.length, 2);
});

test("a stream that cannot open counts as a failed connection", () => {
  let opened = 0;
  const follower = followProject({ open: () => { opened++; throw new Error("refused"); }, read: () => undefined, hidden: () => false });
  assert.equal(follower.link, "reconnecting");
  for (let i = 0; i < RETRIES; i++) mock.timers.tick(15_000);
  assert.equal(opened, RETRIES + 1);
  assert.equal(follower.link, "polling");
});

test("stop closes the stream and cancels every timer", () => {
  const h = harness();
  h.last.state({ pulse: A, busy: true });
  h.follower.stop();
  assert.equal(h.streams[0].closed, true);
  h.last.changed({ pulse: B, busy: true });
  mock.timers.tick(10 * CHECK_IDLE_MS);
  assert.equal(h.reads, 0);
  assert.equal(h.streams.length, 1);
});

test("on the reader's change stream a follower opens no project stream and connects again when that stream drops", async () => {
  const listeners = new Set<(link: string) => void>();
  const busyListeners = new Set<(busy: Record<string, boolean>) => void>();
  const hub = { link: "live", onLink: (l: (link: string) => void) => { listeners.add(l); return () => listeners.delete(l); },
    onBusy: (l: (busy: Record<string, boolean>) => void) => { busyListeners.add(l); return () => busyListeners.delete(l); } };
  const project = "50000000-0000-4000-8000-000000000006";
  const opened: string[] = [];
  let reads = 0;
  const links: Link[] = [];
  const follower = followProject({
    open: (on) => { if (hub.link === "live") { opened.push("changes"); return viaChanges(hub, project, on); } opened.push("project"); return () => {}; },
    read: () => { reads++; },
    hidden: () => false,
    link: (l) => links.push(l),
    busy: (b) => busy.push(b),
  });
  const busy: boolean[] = [];
  await flush();
  assert.equal(follower.link, "live");
  assert.deepEqual(opened, ["changes"]);
  // Work stopping on this project reaches the page without a read; another project's does not.
  follower.loaded(true);
  for (const l of busyListeners) l({ "50000000-0000-4000-8000-000000000001": false });
  for (const l of busyListeners) l({ [project]: false });
  assert.deepEqual(busy, [true, false]);
  assert.equal(reads, 0);
  hub.link = "polling";
  for (const l of [...listeners]) l("polling");
  assert.equal(follower.link, "reconnecting");
  mock.timers.tick(1_000);
  assert.deepEqual(opened, ["changes", "project"]);
  assert.equal(reads, 0);
  follower.stop();
});
