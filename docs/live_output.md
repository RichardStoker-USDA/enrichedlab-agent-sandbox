# Live output

A person can watch an analysis's output while it runs. The worker that holds a direct Sandbox task's lease follows the
runner's output, keeps only sanitized text, and records it in numbered pieces. The saved output files remain the full
record; live pieces are a bounded copy for watching.

Live output is off unless the worker is started with `AGENT_SANDBOX_BROKER_LIVE_OUTPUT=true`. It then needs `get` on
`pods/log` in each lab namespace, beside the `get` on `pods` the driver already has. Reading a running sandbox's output is a containment change, so it stays off until
an independent containment review accepts it.

## Following

When the driver records its first Pod observation, the worker opens the runner container's log through the Kubernetes
API with timestamps and follows it. The log is opened by the Pod's name, so the worker reads the Pod's UID before and
after each open and reads nothing unless both are the UID the driver observed: a Pod that replaced it under the same
name is never read.

If the connection drops while the runner works, the worker reopens from the last line's second. Kubernetes then sends
every line of that second again, in order, so the worker skips what it already took by its place: the line's
timestamp, its place among the lines with that timestamp, and its part (a line longer than 1 MiB is read in parts, cut
at the same places whatever the connection delivers). Lines that share a timestamp are each kept once. A line a read did
not finish is dropped and taken whole from the next read. After the driver observes the runner's end, the worker
reads the log once more without following, sends what remains, keeps the last line even without a line end, and
closes the attempt before the terminal event is recorded, so a closure always precedes the task's end. If that last
read fails three times, it closes with what it stored. A recovery stops the follower and closes with what was stored.
A lost lease stops it without a closure. A log Kubernetes rotated between two reads can lose or repeat lines across
that point; the saved output files remain the full record.

Kubernetes merges a Pod's stdout and stderr unless the alpha `PodLogsQuerySplitStreams` feature gate is on, which this
deployment does not use, so pieces say `combined`.

## Text

Each line keeps printable characters, LF, CR, TAB and SGR sequences (`ESC [` digits, `;` or `:`, then `m`), which
carry color and weight. Every other control character and escape sequence is removed: cursor and erase sequences,
OSC strings such as titles and links, DCS and APC strings, C1 controls, and Unicode's `Bidi_Control` characters
(U+061C, U+200E, U+200F, U+202A to U+202E, U+2066 to U+2069), which would change the order a reader sees text in. An escape string that never ends stops
after 4,096 characters. Invalid UTF-8 becomes U+FFFD. The database refuses any piece that breaks these rules.

A piece holds at most 4 KiB of text and is cut when its first text is 250 ms old. A cut never splits an SGR sequence:
the database takes a piece only with whole ones. Its `bytes` are the raw output bytes
it first represents and `dropped` how many of those sanitizing removed; later pieces of one long line carry zero. An
attempt keeps at most 4 MiB of text; once a piece would pass that, the worker stores no more and counts the raw bytes
it left out in the closure's `dropped`.

## Recording

The worker sends pieces in batches of up to 256 under its lease, naming how many of the attempt's pieces were already
acknowledged. The lease must be active: held by that worker and generation, in a running state, and not past its
expiry when the task's row is locked, as for runtime events. The database numbers each piece after the task's last
one. When a reply is lost, the worker sends those pieces again, often with newer ones: the database checks the ones it
already stored against the batch in every field (stream, text, bytes, dropped and time), stores only the rest, and
refuses any other gap. When the runner stops, the worker closes the attempt with the count, size and SHA-256 of the
UTF-8 concatenation of the pieces it stored, and the database recomputes all three before it accepts them. A closed
attempt takes no more pieces. Pieces and closures never change.

## Reading

`GET /api/v1/tasks/{task}/log/stream` sends Server-Sent Events to anyone who may read the task; anyone else gets 404.
A reader resumes with the standard `Last-Event-ID` header or `?after=<piece number>`.

- `piece`: `id` is the piece number; data `{"seq", "attempt", "stream": "combined", "text", "bytes", "dropped",
  "at"}`, where `at` is the kubelet's time for the piece's first line.
- `closed`: sent once the task has ended and every piece has been sent. Data `{"attempt", "recorded", "reason",
  "pieces", "size", "sha256", "dropped", "artifacts": [{"artifact", "role", "sha256", "size"}]}`. `sha256` and `size`
  cover the current attempt's piece texts in order, so a reader checks what it showed against what the broker stored.
  Without a closure, as when the worker stopped before closing its attempt or live output was off, `recorded` is
  false, `reason` is `no_closure`, `attempt` is the task's attempt, and `pieces`, `size`, `sha256` and `dropped` are
  null: the pieces shown were not checked, which differs from an attempt that stored nothing. `artifacts` names the
  task's saved stdout and stderr files, the full record.

The stream polls once a second and sends a keep-alive comment every 15 seconds. At most 64 streams are open at once,
and 4 per person; past either, the answer is 429 `output_streams_busy`. A stream gives its place back when it ends or
its reader goes away.

## Checks

`src/broker/live_output.rs` holds the sanitizer, the log reader, the follower and their unit tests, including random
input checked against the database's rules. `migrations/postgres/056_live_output.sql` holds the record.
`scripts/qualify_remote_broker.sh live-output` runs recording and reading against PostgreSQL, including a lost reply
sent again with newer pieces, an expired lease, direction controls, what the sanitizer and piece builder make, the
stream limit and a task that ended without a closure.
