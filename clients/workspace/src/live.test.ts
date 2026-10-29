import { test } from "node:test";
import assert from "node:assert/strict";
import { ABSENT_MS, changeEvent, GAP_MS, keysFor, Live, mayBeOld, POLL_MS, RETRIES, serial, touches, type LiveEnv, type LiveLink, type StreamOn } from "./live.ts";

const P = "50000000-0000-4000-8000-000000000001", T = "10000000-0000-4000-8000-000000000001", D = "60000000-0000-4000-8000-000000000001";
const flush = () => new Promise<void>((resolve) => setImmediate(resolve));

// A hub over a stand-in browser: a manual clock, recorded streams and tabs, and a visibility switch.
function harness() {
  let now = 0, hidden = false, id = 0;
  const timers: { at: number; id: number; run: () => void }[] = [];
  const streams: { after: string | null; on: StreamOn; closed: boolean }[] = [];
  const posted: unknown[] = [];
  let receive: ((message: unknown) => void) | null = null;
  let listener: ((event: "shown" | "hidden" | "online") => void) | null = null;
  const env: LiveEnv = {
    hidden: () => hidden,
    now: () => now,
    later: (run, ms) => { const t = { at: now + ms, id: ++id, run }; timers.push(t); return () => { const i = timers.indexOf(t); if (i >= 0) timers.splice(i, 1); }; },
    open: (after, on) => { const s = { after, on, closed: false }; streams.push(s); return () => { s.closed = true; }; },
    channel: (r) => { receive = r; return { post: (m) => posted.push(m), close: () => { receive = null; } }; },
    listen: (on) => { listener = on; return () => { listener = null; }; },
  };
  const hub = new Live(env);
  const links: LiveLink[] = [];
  hub.onLink((l) => links.push(l));
  return {
    hub, streams, posted, links,
    get last() { return streams.at(-1)!; },
    async advance(ms: number) {
      const end = now + ms;
      for (;;) {
        const due = timers.filter((t) => t.at <= end).sort((a, b) => a.at - b.at || a.id - b.id)[0];
        if (!due) break;
        timers.splice(timers.indexOf(due), 1);
        now = Math.max(now, due.at);
        due.run();
        await flush();
      }
      now = end;
      await flush();
    },
    async hide(value: boolean) { hidden = value; listener?.(value ? "hidden" : "shown"); await flush(); },
    async fromTab(message: unknown) { receive?.(message); await flush(); },
    async online() { listener?.("online"); await flush(); },
  };
}
// A view that counts its reads; each read ends when the test says so, or at once.
function view(h: ReturnType<typeof harness>, keys: string[], options: Parameters<Live["watch"]>[2] = {}, hold = false) {
  const pending: (() => void)[] = [];
  let reads = 0;
  const stop = h.hub.watch(keys, () => { reads++; return hold ? new Promise<void>((done) => pending.push(done)) : undefined; }, options);
  return { stop, get reads() { return reads; }, async finish() { pending.shift()?.(); await flush(); } };
}

test("a key change reaches the views that show it; a kind's star reaches all of that kind", () => {
  assert.equal(touches(`project:${P}`, `project:${P}`), true);
  assert.equal(touches("project:*", `project:${P}`), true);
  assert.equal(touches("project:*", "projects"), false);
  assert.equal(touches("*", "inbox"), true);
  assert.equal(touches("inbox", "projects"), false);
});

test("each request names what it changes; reads and previews change nothing; an unknown route changes everything", () => {
  assert.deepEqual(keysFor("GET", "/api/v1/inbox"), []);
  assert.deepEqual(keysFor("POST", `/api/v1/tasks/${T}/reviews`), [`task:${T}`, "project:*", "projects", "tasks", "inbox"]);
  assert.deepEqual(keysFor("POST", `/api/v1/projects/${P}/look`), ["projects"]);
  assert.deepEqual(keysFor("POST", `/api/v1/projects/${P}/figures/previews`), []);
  assert.deepEqual(keysFor("POST", `/api/v1/projects/${P}/figures`, JSON.stringify({ source_task: T })), [`project:${P}`, `task:${T}`, "projects", "tasks", "inbox"]);
  assert.deepEqual(keysFor("POST", `/api/v1/projects/${P}/datasets/${D}/preparations`), [`project:${P}`, `dataset:${D}`, "projects", "tasks", "inbox"]);
  assert.deepEqual(keysFor("POST", `/api/v1/projects/${P}/datasets?request_id=${T}`), [`project:${P}`, "projects"]);
  assert.deepEqual(keysFor("POST", `/api/v1/labs/open_methods/papers/${D}/acceptance`), ["lab:open_methods", "inbox"]);
  assert.deepEqual(keysFor("POST", "/api/v1/tasks", JSON.stringify({ project_id: P })), ["projects", "tasks", "inbox", `project:${P}`]);
  assert.deepEqual(keysFor("PUT", "/api/v1/something-new"), ["*"]);
});

test("one read at a time: a call during a read waits for it and one more read", async () => {
  let reads = 0;
  const pending: (() => void)[] = [];
  const read = serial(() => { reads++; return new Promise<void>((done) => pending.push(done)); });
  const first = read();
  const second = read(), third = read();
  assert.equal(reads, 1);
  pending.shift()!();
  await flush();
  assert.equal(reads, 2, "the calls made during the read share one more read");
  pending.shift()!();
  await Promise.all([first, second, third]);
  assert.equal(reads, 2);
});

test("the reader's own change reads every showing view at once and tells their other tabs", async () => {
  const h = harness();
  h.hub.start();
  const inbox = view(h, ["inbox"]), project = view(h, [`project:${P}`]), other = view(h, ["lab:open_methods"]);
  h.hub.changed([`task:${T}`, "project:*", "inbox"]);
  await flush();
  assert.equal(inbox.reads, 1);
  assert.equal(project.reads, 1);
  assert.equal(other.reads, 0);
  assert.deepEqual(h.posted, [{ type: "changed", keys: [`task:${T}`, "project:*", "inbox"] }]);
});

test("another tab's change reads at once and is not sent on; unreadable tab messages are ignored", async () => {
  const h = harness();
  h.hub.start();
  const inbox = view(h, ["inbox"]);
  await h.fromTab({ type: "changed", keys: ["inbox"] });
  await h.fromTab({ type: "changed", keys: ["<script>"] });
  await h.fromTab("inbox");
  assert.equal(inbox.reads, 1);
  assert.deepEqual(h.posted, []);
});

test("stream changes reach a view at most once per gap, and the last one always arrives", async () => {
  const h = harness();
  h.hub.start();
  h.last.on.ready("1");
  const list = view(h, ["tasks"], { gapMs: GAP_MS });
  h.last.on.changed(["tasks"], "2");
  await flush();
  assert.equal(list.reads, 1);
  h.last.on.changed(["tasks"], "3");
  h.last.on.changed(["tasks"], "4");
  await flush();
  assert.equal(list.reads, 1, "changes inside the gap wait");
  await h.advance(GAP_MS);
  assert.equal(list.reads, 2, "one read after the gap covers both");
  await h.advance(GAP_MS * 3);
  assert.equal(list.reads, 2);
});

test("a change during a read reads once more after it; the reader's own change does not wait for the gap", async () => {
  const h = harness();
  h.hub.start();
  h.last.on.ready("1");
  const page = view(h, [`project:${P}`], { gapMs: 5_000 }, true);
  h.hub.changed([`project:${P}`]);
  await flush();
  h.last.on.changed([`project:${P}`], "2");
  h.hub.changed([`project:${P}`]);
  assert.equal(page.reads, 1);
  await page.finish();
  assert.equal(page.reads, 2, "the reader's change during the read reads again at once");
  await page.finish();
  assert.equal(page.reads, 2);
});

test("a hidden page marks its views stale and reads them when shown; a view that reads while hidden still does", async () => {
  const h = harness();
  h.hub.start();
  h.last.on.ready("1");
  const page = view(h, [`project:${P}`]), inbox = view(h, ["inbox"], { whileHidden: true });
  await h.hide(true);
  h.last.on.changed([`project:${P}`, "inbox"], "2");
  await flush();
  assert.equal(page.reads, 0);
  assert.equal(inbox.reads, 1);
  await h.hide(false);
  assert.equal(page.reads, 1);
  assert.equal(inbox.reads, 1, "a live page reads only what changed while hidden");
});

test("the first connection resumes nothing; later ones resume after the last cursor; renew waits briefly", async () => {
  const h = harness();
  h.hub.start();
  assert.equal(h.last.after, null);
  h.last.on.ready("10");
  h.last.on.changed(["inbox"], "12");
  h.last.on.renew();
  assert.equal(h.streams.length, 1);
  await h.advance(1_000);
  assert.equal(h.streams.length, 2);
  assert.equal(h.last.after, "12");
  assert.equal(h.streams[0].closed, true);
});

test("a broker without the stream leaves lists on the timer, reads them when shown, and is asked again later", async () => {
  const h = harness();
  h.hub.start();
  const ready = h.hub.whenReady(60_000);
  h.last.on.error(true);
  await ready;
  assert.equal(h.hub.link, "polling");
  const inbox = view(h, ["inbox"], { poll: true }), page = view(h, [`project:${P}`]);
  await h.advance(POLL_MS);
  assert.equal(inbox.reads, 1);
  assert.equal(page.reads, 0, "only lists read on the timer");
  await h.hide(true);
  await h.advance(POLL_MS);
  assert.equal(inbox.reads, 1, "a hidden page does not read on the timer");
  await h.hide(false);
  assert.equal(inbox.reads, 2, "it reads when shown");
  assert.equal(h.streams.length, 1);
  await h.advance(ABSENT_MS);
  assert.equal(h.streams.length, 2);
});

test("a broker that says it serves no stream is never asked for one; the reader's own changes still reach every view", async () => {
  const h = harness();
  h.hub.start({ stream: false });
  await h.hub.whenReady(60_000);
  assert.equal(h.hub.link, "polling");
  const inbox = view(h, ["inbox"], { poll: true });
  h.hub.changed(["inbox"]);
  await h.advance(0);
  assert.equal(inbox.reads, 1);
  await h.advance(ABSENT_MS * 2);
  assert.equal(h.streams.length, 0);
});

test("a dropped stream reconnects with backoff, falls back to the timer after its retries, and a later ready resets views read without it", async () => {
  const h = harness();
  h.hub.start();
  h.last.on.ready(null);
  const inbox = view(h, ["inbox"], { poll: true });
  h.hub.changed(["inbox"]);
  await flush();
  for (let i = 1; i <= RETRIES + 1; i++) {
    h.last.on.error(false);
    await h.advance(30_000);
  }
  assert.equal(h.hub.link, "polling");
  assert.ok(h.links.includes("connecting"));
  const before = inbox.reads;
  h.last.on.ready("40");
  await flush();
  assert.equal(h.hub.link, "live");
  assert.equal(inbox.reads, before + 1, "a stream that gave no cursor before cannot say what changed, so views read again");
});

test("coming back online reconnects at once and reads the lists", async () => {
  const h = harness();
  h.hub.start();
  h.last.on.ready("5");
  const inbox = view(h, ["inbox"], { poll: true });
  h.last.on.error(false);
  const count = h.streams.length;
  await h.online();
  assert.equal(h.streams.length, count + 1);
  assert.equal(h.last.after, "5");
  assert.equal(inbox.reads, 1);
});

test("the stream's own events are read strictly", () => {
  assert.deepEqual(changeEvent(JSON.stringify({ keys: ["inbox", `project:${P}`, "lab:open_methods", "drop table"] })), { keys: ["inbox", `project:${P}`, "lab:open_methods"] });
  assert.deepEqual(changeEvent(JSON.stringify({ keys: [`project:${P}`], busy: { [P]: false, "not-a-project": true, [D]: "yes" } })), { keys: [`project:${P}`], busy: { [P]: false } });
  assert.equal(changeEvent("not json"), null);
  assert.equal(changeEvent(JSON.stringify({ keys: "inbox" })), null);
  assert.equal(changeEvent(JSON.stringify({ keys: [], busy: [true] })), null);
});

test("whether work runs reaches its listeners with each change, before the views read", async () => {
  const h = harness();
  h.hub.start();
  h.last.on.ready("1");
  const heard: Record<string, boolean>[] = [];
  h.hub.onBusy((busy) => heard.push(busy));
  h.last.on.changed([`project:${P}`], "2", { [P]: false });
  h.last.on.changed(["inbox"], "3");
  assert.deepEqual(heard, [{ [P]: false }]);
});

test("a stopped view and a stopped hub read nothing more", async () => {
  const h = harness();
  h.hub.start();
  h.last.on.ready("1");
  const inbox = view(h, ["inbox"]);
  inbox.stop();
  h.hub.changed(["inbox"]);
  await flush();
  assert.equal(inbox.reads, 0);
  h.hub.stop();
  assert.equal(h.last.closed, true);
});

test("a page reads a shared list first only when it was never read, or may be old because no stream says so", () => {
  assert.equal(mayBeOld(0, "live", 10_000), true);
  assert.equal(mayBeOld(1_000, "live", 600_000), false);
  assert.equal(mayBeOld(1_000, "polling", 4_000), false);
  assert.equal(mayBeOld(1_000, "polling", 7_000), true);
  assert.equal(mayBeOld(1_000, "connecting", 7_000), true);
});

test("news of the agent's progress waits for the gap like the stream; a page's own finding reads at once", async () => {
  const h = harness();
  h.hub.start();
  h.last.on.ready("1");
  const inbox = view(h, ["inbox"], { gapMs: 5_000 });
  h.hub.changed(["inbox"], "stream");
  await flush();
  h.hub.changed(["inbox"], "stream");
  await flush();
  assert.equal(inbox.reads, 1);
  h.hub.changed(["inbox"], "derived");
  await flush();
  assert.equal(inbox.reads, 2);
  assert.deepEqual(h.posted, [], "neither is sent to other tabs");
});

test("a view of several records of one kind hears which changed; a reset says anything may have", async () => {
  const h = harness();
  h.hub.start();
  h.last.on.ready("1");
  const told: (string[] | null)[] = [];
  h.hub.watch(["project:*"], (changed) => { told.push(changed); }, { gapMs: 0 });
  h.last.on.changed([`project:${P}`, "inbox"], "2");
  await flush();
  h.last.on.reset("3");
  await flush();
  assert.deepEqual(told, [[`project:${P}`], null]);
  assert.equal(touches(`project:${P}`, "project:*"), true);
  assert.equal(touches("projects", "project:*"), false);
});
