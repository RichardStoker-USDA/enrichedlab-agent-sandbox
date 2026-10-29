import assert from "node:assert/strict";
import { createHash } from "node:crypto";
import { test } from "node:test";
import { applySgr, checkRecord, clean, closedFrom, findIn, LogBuilder, marked, pieceFrom, plainText, quietWords, withSystem, type LogPiece } from "./live_log.ts";

const piece = (seq: number, text: string, extra: Partial<LogPiece> = {}): LogPiece =>
  ({ seq, attempt: 1, stream: "combined", text, bytes: Buffer.byteLength(text), at: `2026-10-12T08:00:${String(seq).padStart(2, "0")}Z`, dropped: 0, ...extra });
const lines = (b: LogBuilder) => b.view().flatMap((r) => (r.kind === "line" ? [r.plain] : [`[${r.kind}]`]));

test("only colour and weight codes, line feeds, carriage returns and tabs pass", () => {
  // A title, a hyperlink, a clipboard request, cursor moves, a bell and a direction override are all removed.
  const raw = "a\x1b]0;title\x07b\x1b]8;;https://example.invalid\x1b\\link\x1b]8;;\x1b\\ \x1b]52;c;ZXZpbA==\x07c\x1b[2J\x1b[10;1Hd\x07e‮f\t\x1b[1;31mred\x1b[0m\r\n";
  assert.equal(clean(raw).text, "ablink cdef\t\x1b[1;31mred\x1b[0m\r\n");
  // An escape cut off by the end of a piece waits for the next one.
  assert.deepEqual(clean("text\x1b[3"), { text: "text", rest: "\x1b[3" });
  assert.deepEqual(clean("x\x1b"), { text: "x", rest: "\x1b" });
  // C1 controls and DEL go too.
  assert.equal(clean("a\x9b31mb\x7fc").text, "a31mbc");
});

test("colour and weight codes become styles by meaning, and other colours use the page's ink", () => {
  assert.deepEqual(applySgr({}, "1;31"), { bold: true, tone: "alarm" });
  assert.deepEqual(applySgr({ bold: true, tone: "alarm" }, "22;39"), {});
  assert.deepEqual(applySgr({}, "33"), { tone: "strong" });
  assert.deepEqual(applySgr({}, "94"), { tone: "note" });
  assert.deepEqual(applySgr({ tone: "note" }, "38;5;196"), {});
  assert.deepEqual(applySgr({}, "38;5;1;4"), { tone: "alarm", underline: true });
  assert.deepEqual(applySgr({ tone: "alarm" }, "38;2;10;20;30;1"), { bold: true });
  // Colon subparameters: an indexed colour, a direct colour and underline styles.
  assert.deepEqual(applySgr({}, "38:5:4"), { tone: "note" });
  assert.deepEqual(applySgr({ tone: "note" }, "38:2::10:20:30;1"), { bold: true });
  assert.deepEqual(applySgr({}, "4:3"), { underline: true });
  assert.deepEqual(applySgr({ underline: true }, "4:0"), {});
  assert.equal(clean("\x1b[38:5:4mx\x1b[0m").text, "\x1b[38:5:4mx\x1b[0m");
  assert.deepEqual(applySgr({ italic: true }, ""), {});
});

test("pieces become lines; a carriage return starts its line again; a line can span pieces", () => {
  const b = new LogBuilder();
  b.add(piece(1, "Reading 23 blocks\nBootstrap  10%\rBootstrap  60%"));
  b.add(piece(2, "\rBootstrap 100%\r"));
  b.add(piece(3, "\nDone\n"));
  assert.deepEqual(lines(b), ["Reading 23 blocks", "Bootstrap 100%", "Done"]);
  // A repeated or older piece changes nothing.
  assert.equal(b.add(piece(2, "again\n")), false);
  assert.deepEqual(lines(b), ["Reading 23 blocks", "Bootstrap 100%", "Done"]);
});

test("each view is a new array, so a page that redraws on a changed value sees lines that end a piece", () => {
  // The Detailed view froze on its first, empty view when every piece ended its line and no System rows were mixed in.
  const b = new LogBuilder();
  const empty = b.view();
  b.add(piece(1, "Read 69 block-days\n"));
  const after = b.view();
  assert.notEqual(after, empty);
  assert.deepEqual(empty, []);
  assert.notEqual(withSystem(after, []), withSystem(b.view(), []));
});

test("styles carry across pieces and an escape split between pieces is read whole", () => {
  const b = new LogBuilder();
  b.add(piece(1, "\x1b[3"));
  b.add(piece(2, "3mWarning:\x1b[0m block 19 is left out\n"));
  const [row] = b.view();
  assert.equal(row.kind, "line");
  if (row.kind !== "line") return;
  assert.deepEqual(row.spans, [{ text: "Warning:", style: { tone: "strong" } }, { text: " block 19 is left out", style: {} }]);
});

test("each stream keeps its own unfinished line, a new attempt is marked, and the bytes the broker removed are counted", () => {
  const b = new LogBuilder();
  b.add(piece(1, "out part", { stream: "stdout" }));
  b.add(piece(2, "err line\n", { stream: "stderr" }));
  b.add(piece(3, " two\n", { stream: "stdout", dropped: 12 }));
  b.add(piece(4, "next\n", { stream: "stdout", dropped: 30 }));
  b.add(piece(5, "again\n", { attempt: 2 }));
  assert.deepEqual(lines(b), ["err line", "out part two", "next", "[attempt]", "again"]);
  assert.deepEqual(b.view().flatMap((r) => (r.kind === "line" ? [r.n] : [])), [1, 2, 3, 4]);
  assert.equal(b.removed, 42);
});

test("the text sent is checked against the broker's record of the last attempt", async () => {
  const b = new LogBuilder();
  b.add(piece(1, "old\n"));
  b.add(piece(2, "first\n", { attempt: 2 }));
  b.add(piece(3, "second\n", { attempt: 2 }));
  const text = "first\nsecond\n";
  const sha = createHash("sha256").update(text).digest("hex");
  const closed = { attempt: 2, recorded: true, reason: null, pieces: 2, size: Buffer.byteLength(text), sha256: sha, dropped: 0, artifacts: [] };
  assert.deepEqual(await checkRecord(b.sent, closed), { state: "checked", pieces: 2, size: 13, sha256: sha });
  assert.deepEqual(await checkRecord(b.sent, { ...closed, sha256: `sha256:${sha.toUpperCase()}` }), { state: "checked", pieces: 2, size: 13, sha256: sha });
  assert.deepEqual(await checkRecord(b.sent, { ...closed, sha256: "0".repeat(64) }), { state: "differs", missing: 0 });
  assert.deepEqual(await checkRecord(b.sent, { ...closed, pieces: 3 }), { state: "incomplete", missing: 1 });
  assert.deepEqual(await checkRecord(b.sent, { ...closed, recorded: false }), { state: "not_recorded" });
  const none = { ...closed, recorded: false, reason: "no_closure" as const, pieces: null, size: null, sha256: null, dropped: null };
  assert.deepEqual(await checkRecord(b.sent, none), { state: "not_recorded" });
});

test("the shapes are checked before use", () => {
  assert.deepEqual(pieceFrom(piece(1, "x")), piece(1, "x"));
  assert.equal(pieceFrom({ ...piece(1, "x"), stream: "tty" }), null);
  assert.equal(pieceFrom({ ...piece(0, "x") }), null);
  assert.equal(pieceFrom({ ...piece(1, "x"), at: "yesterday" }), null);
  const closed = { attempt: 1, recorded: true, pieces: 1, size: 1, sha256: "a", dropped: 0, artifacts: [{ artifact: "f", role: "stdout", sha256: "b", size: 1 }] };
  assert.deepEqual(closedFrom(closed), { ...closed, reason: null });
  assert.equal(closedFrom({ ...closed, artifacts: [{ artifact: "f", role: "tty", sha256: "b", size: 1 }] }), null);
  // A step that ended without a closure record names no counts and is never recorded.
  const none = { attempt: 1, recorded: false, reason: "no_closure", pieces: null, size: null, sha256: null, dropped: null, artifacts: [] };
  assert.deepEqual(closedFrom(none), none);
  assert.equal(closedFrom({ ...none, recorded: true }), null);
  assert.equal(closedFrom({ ...none, pieces: 0 }), null);
  assert.equal(closedFrom({ ...closed, reason: "other" }), null);
  assert.equal(closedFrom({ ...closed, pieces: null }), null);
});

test("find marks every match, the current one apart, across styled spans", () => {
  const b = new LogBuilder();
  b.add(piece(1, "Block \x1b[1m19\x1b[0m has no rows; block 7 has rows\n"));
  const rows = b.view();
  const found = findIn(rows, "ROWS");
  assert.deepEqual(found.map((m) => [m.start, m.end]), [[16, 20], [34, 38]]);
  const row = rows[0];
  if (row.kind !== "line") return;
  const parts = marked(row.spans, found.map((m, i) => ({ start: m.start, end: m.end, current: i === 1 })));
  assert.deepEqual(parts.filter((p) => p.hit).map((p) => [p.text, p.hit]), [["rows", "match"], ["rows", "current"]]);
  assert.equal(parts.map((p) => p.text).join(""), row.plain);
});

test("quiet times and copied text read in words", () => {
  assert.equal(quietWords(45_000), "45 seconds");
  assert.equal(quietWords(61_000), "1 minute");
  assert.equal(quietWords(3_900_000), "1 hour 5 minutes");
  const b = new LogBuilder();
  b.add(piece(1, "a\n"));
  b.add(piece(2, "b\n", { attempt: 2 }));
  assert.equal(plainText(b.view()), "a\n[Attempt 2 started]\nb");
});

test("the step's own lifecycle lines sit among the log's rows by time", () => {
  const b = new LogBuilder();
  b.add(piece(5, "one\n"));
  b.add(piece(9, "two\n"));
  const rows = withSystem(b.view(), [{ at: "2026-10-12T08:00:07Z", text: "Assigned to a compute worker" }, { at: "2026-10-12T08:00:01Z", text: "Request received" }]);
  assert.deepEqual(rows.map((r) => (r.kind === "line" ? r.plain : r.kind === "system" ? r.text : r.kind)), ["Request received", "one", "Assigned to a compute worker", "two"]);
});
