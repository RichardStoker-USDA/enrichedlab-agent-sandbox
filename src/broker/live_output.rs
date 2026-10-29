//! Live output. While a direct Sandbox runs, the worker that holds its lease follows the runner's
//! output through the Kubernetes Pod log, keeps only sanitized text, and records it in numbered pieces under that lease.
//! When the runner stops, the worker closes the attempt with the count, size and SHA-256 of what it stored, which the
//! database recomputes. The saved output files stay the full record; the pieces are a bounded copy for watching.
//!
//! Kubernetes merges a Pod's stdout and stderr unless an alpha feature gate is on, so pieces say `combined`.
//!
//! It is off unless the worker is started with live output enabled, and it needs `get` on `pods/log` in the lab's
//! namespace. Reading a running sandbox's output is a containment change: it waits for an independent containment review
//! before it is enabled anywhere.
use std::{pin::Pin, sync::Arc, time::Duration};

use anyhow::Result;
use async_trait::async_trait;
use futures_util::{Stream, StreamExt};
use sha2::{Digest, Sha256};
use time::{OffsetDateTime, format_description::well_known::Rfc3339};
use tokio::{
    sync::Mutex,
    task::JoinHandle,
    time::{Instant, sleep, timeout},
};
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use super::{
    model::ClaimedTask,
    runtime_events::{RuntimeEvent, RuntimeEventRecorder},
    store::{OutputPiece, OutputTotals, Store},
};

/// Text kept per attempt; the database refuses more.
pub const ATTEMPT_LIMIT: u64 = 4 * 1024 * 1024;
/// A piece is cut at this much text, or when its first text is `PIECE_AGE` old.
const PIECE_TEXT: usize = 4096;
const PIECE_AGE: Duration = Duration::from_millis(250);
/// Pieces are sent at most `SEND_BATCH` at a time, or when the oldest unsent one is `SEND_AGE` old.
const SEND_BATCH: usize = 64;
const SEND_AGE: Duration = Duration::from_millis(500);
/// A line longer than this is cut, so one line cannot hold the whole stream in memory.
const MAX_LINE: usize = 1024 * 1024;
/// Escape strings (OSC, DCS and the like) longer than this end, so an unterminated one cannot hide later output.
const STRING_LIMIT: usize = 4096;
const SGR_LIMIT: usize = 64;
/// After the runner stops, the follower has this long to read the rest and send it.
const DRAIN: Duration = Duration::from_secs(20);
const RETRY: Duration = Duration::from_secs(1);

pub type LogStream = Pin<Box<dyn Stream<Item = Result<Vec<u8>>> + Send>>;

/// Unicode's `Bidi_Control` characters. Each changes the order in which the text around it is shown, so a line or a
/// name holding one can display differently from what it says.
pub fn is_bidi_control(c: char) -> bool {
    matches!(
        c,
        '\u{61c}' | '\u{200e}' | '\u{200f}' | '\u{202a}'..='\u{202e}' | '\u{2066}'..='\u{2069}'
    )
}

/// Keeps printable text, LF, CR, TAB and SGR sequences (`ESC [ <digits ; :> m`) from one line of raw output and
/// removes every other control character, every `Bidi_Control` character and every other escape sequence. Invalid
/// UTF-8 becomes U+FFFD. Returns how many raw bytes were not kept.
pub fn sanitize_line(raw: &[u8], out: &mut String) -> u64 {
    #[derive(Clone, Copy, PartialEq, Eq)]
    enum Escape {
        Text,
        Esc,
        /// After ESC and an intermediate byte (0x20 to 0x2F), until a final byte, as in `ESC ( B`.
        EscMore,
        Csi,
        CsiDrop,
        Str(usize),
        StrEsc,
    }
    let mut state = Escape::Text;
    let mut sgr = String::new();
    let mut kept = 0_u64;
    let mut feed = |c: char, raw_len: usize, out: &mut String| loop {
        state = match state {
            Escape::Text => {
                if c == '\u{1b}' {
                    Escape::Esc
                } else {
                    if matches!(c, '\n' | '\r' | '\t') || !(c.is_control() || is_bidi_control(c)) {
                        out.push(c);
                        kept += raw_len as u64;
                    }
                    Escape::Text
                }
            }
            Escape::Esc => match c {
                '[' => {
                    sgr.clear();
                    Escape::Csi
                }
                ']' | 'P' | 'X' | '^' | '_' => Escape::Str(0),
                '\u{20}'..='\u{2f}' => Escape::EscMore,
                // A control character ends the sequence and is handled as text.
                c if c.is_control() => {
                    state = Escape::Text;
                    continue;
                }
                _ => Escape::Text,
            },
            Escape::EscMore => match c {
                '\u{20}'..='\u{2f}' => Escape::EscMore,
                '\u{30}'..='\u{7e}' => Escape::Text,
                _ => {
                    state = Escape::Text;
                    continue;
                }
            },
            Escape::Csi => match c {
                '0'..='9' | ';' | ':' if sgr.len() < SGR_LIMIT => {
                    sgr.push(c);
                    Escape::Csi
                }
                'm' => {
                    out.push_str("\u{1b}[");
                    out.push_str(&sgr);
                    out.push('m');
                    kept += 3 + sgr.len() as u64;
                    Escape::Text
                }
                '\u{40}'..='\u{7e}' => Escape::Text,
                '\u{20}'..='\u{3f}' => Escape::CsiDrop,
                _ => {
                    state = Escape::Text;
                    continue;
                }
            },
            Escape::CsiDrop => match c {
                '\u{40}'..='\u{7e}' => Escape::Text,
                '\u{20}'..='\u{3f}' => Escape::CsiDrop,
                _ => {
                    state = Escape::Text;
                    continue;
                }
            },
            Escape::Str(n) => match c {
                '\u{7}' => Escape::Text,
                '\u{1b}' => Escape::StrEsc,
                _ if n + 1 >= STRING_LIMIT => Escape::Text,
                _ => Escape::Str(n + 1),
            },
            Escape::StrEsc => {
                if c == '\\' {
                    Escape::Text
                } else {
                    // ESC ends the string and begins another sequence.
                    state = Escape::Esc;
                    continue;
                }
            }
        };
        break;
    };
    let mut rest = raw;
    loop {
        match std::str::from_utf8(rest) {
            Ok(text) => {
                for c in text.chars() {
                    feed(c, c.len_utf8(), out);
                }
                break;
            }
            Err(error) => {
                let (valid, after) = rest.split_at(error.valid_up_to());
                for c in std::str::from_utf8(valid).unwrap_or_default().chars() {
                    feed(c, c.len_utf8(), out);
                }
                feed('\u{fffd}', 0, out);
                let skip = error.error_len().unwrap_or(after.len());
                rest = &after[skip..];
            }
        }
    }
    raw.len() as u64 - kept
}

/// One line of the Pod log, or one part of a line longer than `MAX_LINE`: the kubelet's timestamp, when it parsed, and
/// the program's bytes. Every part of a line carries the timestamp its first part (`part` 0) began with.
#[derive(Debug, PartialEq, Eq)]
pub struct Line {
    pub at: Option<OffsetDateTime>,
    pub content: Vec<u8>,
    pub newline: bool,
    pub part: u32,
}

/// Splits a timestamped Pod log (`<RFC 3339> <content>\n`) into lines across chunk boundaries. A line longer than
/// `MAX_LINE` is cut into parts at the same places however its bytes arrive, so a reread cuts it the same way.
#[derive(Default)]
pub struct LineReader {
    partial: Vec<u8>,
    /// The line being read: the timestamp its first part carried and how many parts were taken.
    at: Option<OffsetDateTime>,
    parts: u32,
}

impl LineReader {
    pub fn push(&mut self, chunk: &[u8]) -> Vec<Line> {
        let mut lines = Vec::new();
        for part in chunk.split_inclusive(|&b| b == b'\n') {
            let ended = part.ends_with(b"\n");
            self.partial
                .extend_from_slice(&part[..part.len() - usize::from(ended)]);
            while self.partial.len() > MAX_LINE {
                // Cut at a character boundary so the next part starts with a whole character.
                let mut cut = MAX_LINE;
                while cut > MAX_LINE - 4 && self.partial[cut] & 0xc0 == 0x80 {
                    cut -= 1;
                }
                let rest = self.partial.split_off(cut);
                let raw = std::mem::replace(&mut self.partial, rest);
                lines.push(self.line(raw, false));
            }
            if ended {
                let raw = std::mem::take(&mut self.partial);
                lines.push(self.line(raw, true));
                (self.at, self.parts) = (None, 0);
            }
        }
        lines
    }

    /// The rest of the last line, once the log has been read to its end.
    pub fn finish(&mut self) -> Option<Line> {
        let rest = std::mem::take(&mut self.partial);
        let line = (!rest.is_empty()).then(|| self.line(rest, false));
        self.discard();
        line
    }

    /// Forgets a line not read to its end, so that a reread, which starts on a whole line, gives it whole.
    pub fn discard(&mut self) {
        self.partial.clear();
        (self.at, self.parts) = (None, 0);
    }

    /// One part of the line being read. Its first part takes the timestamp; a later part's bytes are the program's,
    /// even when they look like a timestamp.
    fn line(&mut self, raw: Vec<u8>, newline: bool) -> Line {
        let part = self.parts;
        self.parts += 1;
        if part > 0 {
            return Line {
                at: self.at,
                content: raw,
                newline,
                part,
            };
        }
        let (at, content) = match raw.iter().position(|&b| b == b' ') {
            Some(space) => match std::str::from_utf8(&raw[..space])
                .map_err(drop)
                .and_then(|text| OffsetDateTime::parse(text, &Rfc3339).map_err(drop))
            {
                Ok(at) => (Some(at), raw[space + 1..].to_vec()),
                Err(()) => (None, raw),
            },
            None => (None, raw),
        };
        self.at = at;
        Line {
            at,
            content,
            newline,
            part,
        }
    }
}

/// Builds pieces from sanitized lines and enforces the attempt's text limit. A piece's `bytes` are the raw output bytes
/// it first represents and `dropped` how many of them sanitizing removed; a long line's later pieces carry zero.
#[derive(Default)]
pub struct Pieces {
    text: String,
    bytes: u64,
    dropped: u64,
    at: Option<OffsetDateTime>,
    since: Option<Instant>,
    used: u64,
    full: bool,
    /// Raw bytes left out because the attempt reached its limit.
    pub over_limit: u64,
}

impl Pieces {
    pub fn push(&mut self, line: &Line, now: Instant, out: &mut Vec<OutputPiece>) {
        let raw = line.content.len() as u64 + u64::from(line.newline);
        if self.full {
            self.over_limit += raw;
            return;
        }
        let mut text = String::new();
        let removed = sanitize_line(&line.content, &mut text);
        if line.newline {
            text.push('\n');
        }
        if self.text.is_empty() {
            self.at = line.at.or(self.at);
            self.since = Some(now);
        }
        self.text.push_str(&text);
        self.bytes += raw;
        self.dropped += removed;
        while self.text.len() >= PIECE_TEXT && !self.full {
            let rest = self.text.split_off(piece_cut(&self.text));
            self.cut(out);
            self.text = rest;
            self.since = Some(now);
        }
    }

    /// Cuts the pending text into a piece when its first text is old enough.
    pub fn age(&mut self, now: Instant, out: &mut Vec<OutputPiece>) {
        if !self.text.is_empty() && self.since.is_some_and(|since| now - since >= PIECE_AGE) {
            self.cut(out);
        }
    }

    pub fn finish(&mut self, out: &mut Vec<OutputPiece>) {
        if !self.text.is_empty() {
            self.cut(out);
        }
    }

    fn cut(&mut self, out: &mut Vec<OutputPiece>) {
        let text = std::mem::take(&mut self.text);
        if self.full || self.used + text.len() as u64 > ATTEMPT_LIMIT {
            self.full = true;
            self.over_limit += self.bytes;
        } else {
            self.used += text.len() as u64;
            let at = self.at.unwrap_or_else(OffsetDateTime::now_utc);
            out.push(OutputPiece {
                stream: "combined",
                text,
                bytes: self.bytes.min(u64::from(u32::MAX)) as u32,
                dropped: self.dropped.min(self.bytes).min(u64::from(u32::MAX)) as u32,
                at: at.format(&Rfc3339).unwrap_or_default(),
            });
        }
        self.bytes = 0;
        self.dropped = 0;
        self.since = None;
    }
}

/// Where to cut text of at least `PIECE_TEXT` bytes into a piece: at a character boundary, and before a color code the
/// cut would split, since the database takes a piece only with whole codes. Sanitized text holds an ESC only at the
/// start of a whole code, `ESC [ <digits ; :> m`, at most 3 + `SGR_LIMIT` bytes, so a code that starts the text ends
/// before the cut.
fn piece_cut(text: &str) -> usize {
    let mut cut = PIECE_TEXT;
    while !text.is_char_boundary(cut) {
        cut -= 1;
    }
    if let Some(esc) = text[..cut].rfind('\u{1b}')
        && esc > 0
        && !text[esc..cut].contains('m')
    {
        cut = esc;
    }
    cut
}

/// Where the runner's output comes from: a timestamped log, optionally following, from just before `since`.
#[async_trait]
pub trait LogSource: Send + Sync {
    async fn open(&self, since: Option<OffsetDateTime>, follow: bool) -> Result<LogStream>;
}

/// The lease a follower records under.
#[derive(Clone, Debug)]
pub struct Lease {
    pub task: Uuid,
    pub worker: String,
    pub generation: i64,
}

/// Where pieces go. A `false` from `record` means the lease is gone and nothing more can be recorded.
#[async_trait]
pub trait OutputSink: Send + Sync {
    /// `first` is how many of the attempt's pieces were acknowledged before these, so a batch whose reply was lost
    /// is recognized when it is sent again.
    async fn record(&self, lease: &Lease, first: i32, pieces: &[OutputPiece]) -> Result<bool>;
    async fn close(&self, lease: &Lease, totals: &OutputTotals) -> Result<()>;
}

#[async_trait]
impl OutputSink for Store {
    async fn record(&self, lease: &Lease, first: i32, pieces: &[OutputPiece]) -> Result<bool> {
        match self
            .record_task_output(lease.task, &lease.worker, lease.generation, first, pieces)
            .await
        {
            Ok(_) => Ok(true),
            Err(error) if error.code == "lease_fence_lost" => Ok(false),
            Err(error) => Err(anyhow::anyhow!(error.code)),
        }
    }

    async fn close(&self, lease: &Lease, totals: &OutputTotals) -> Result<()> {
        self.close_task_output(lease.task, &lease.worker, lease.generation, totals)
            .await
            .map(drop)
            .map_err(|error| anyhow::anyhow!(error.code))
    }
}

/// What a follower stored. `lease_lost` means it must not close the attempt.
#[derive(Debug, Default)]
pub struct Followed {
    pub totals: OutputTotals,
    pub lease_lost: bool,
}

struct Sent {
    pieces: i32,
    size: i64,
    digest: Sha256,
    lost: u64,
    lease_lost: bool,
}

impl Sent {
    /// Sends pieces in batches the database accepts; failed pieces stay queued unless `last` is set, when their raw
    /// bytes count as left out.
    async fn send(
        &mut self,
        sink: &dyn OutputSink,
        lease: &Lease,
        outbox: &mut Vec<OutputPiece>,
        last: bool,
    ) {
        while !outbox.is_empty() && !self.lease_lost {
            let take = outbox.len().min(256);
            match sink.record(lease, self.pieces, &outbox[..take]).await {
                Ok(true) => {
                    for piece in outbox.drain(..take) {
                        self.pieces += 1;
                        self.size += piece.text.len() as i64;
                        self.digest.update(piece.text.as_bytes());
                    }
                }
                Ok(false) => self.lease_lost = true,
                Err(error) => {
                    tracing::warn!(event = "task_output_not_recorded", task = %lease.task, %error);
                    if !last {
                        return;
                    }
                    self.lost += outbox
                        .drain(..)
                        .map(|piece| u64::from(piece.bytes))
                        .sum::<u64>();
                }
            }
        }
    }
}

/// Where a line part stands in the Pod log: its timestamp, its place among the lines with that timestamp in one read,
/// and its part. A reread from a timestamp's second gives every line of that second again in order and cuts each the
/// same way, so a part has the same place in every read, and distinct lines that share a timestamp have distinct places.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
struct Place {
    at: OffsetDateTime,
    nth: u32,
    part: u32,
}

/// Follows the runner's output until `stop`, or until the runner has ended (`ended`) and its output has been read to
/// the end. After a reconnect, the parts already taken are skipped by their place; a line a read did not finish is
/// read again whole.
pub async fn follow(
    source: Arc<dyn LogSource>,
    sink: Arc<dyn OutputSink>,
    lease: Lease,
    ended: CancellationToken,
    stop: CancellationToken,
) -> Followed {
    let mut run = Run {
        sink: sink.as_ref(),
        lease: &lease,
        lines: LineReader::default(),
        pieces: Pieces::default(),
        outbox: Vec::new(),
        sent: Sent {
            pieces: 0,
            size: 0,
            digest: Sha256::new(),
            lost: 0,
            lease_lost: false,
        },
        seen: None,
        read_at: None,
        oldest_unsent: None,
    };
    // Reads after the runner ended that failed to open or broke off; the follower gives up after three.
    let mut failures = 0;
    'outer: loop {
        if stop.is_cancelled() || run.sent.lease_lost {
            break;
        }
        let following = !ended.is_cancelled();
        // While the runner works, a wait ends early once it is known to have ended.
        let until = following.then_some(&ended);
        match source.open(run.seen.map(|place| place.at), following).await {
            Ok(mut stream) => {
                run.read_at = None;
                let mut read_to_end = false;
                loop {
                    tokio::select! {
                        _ = stop.cancelled() => break 'outer,
                        next = stream.next() => match next {
                            Some(Ok(chunk)) => run.take(&chunk),
                            Some(Err(_)) => break,
                            None => {
                                read_to_end = true;
                                break;
                            }
                        },
                        _ = sleep(PIECE_AGE) => {}
                    }
                    run.tick().await;
                    if run.sent.lease_lost {
                        break 'outer;
                    }
                }
                if read_to_end && !following {
                    // The runner has ended and its log was read to the end: its last line is whole.
                    run.end_of_stream();
                    break;
                }
                // A broken read, or a followed one that ended (the runner exited or the connection dropped): the next
                // read gives the unfinished line again whole. Once the runner is known to have ended, read once more
                // without following; until then, follow again.
                run.lines.discard();
                if !following {
                    failures += 1;
                    if failures >= 3 {
                        break;
                    }
                }
                if run.idle(RETRY, &stop, until).await {
                    break;
                }
            }
            Err(_) => {
                if !following {
                    failures += 1;
                    if failures >= 3 {
                        break;
                    }
                }
                if run.idle(RETRY, &stop, until).await {
                    break;
                }
            }
        }
    }
    run.pieces.finish(&mut run.outbox);
    run.sent
        .send(run.sink, run.lease, &mut run.outbox, true)
        .await;
    Followed {
        totals: OutputTotals {
            pieces: run.sent.pieces,
            size: run.sent.size,
            sha256: hex::encode(run.sent.digest.finalize()),
            dropped: (run.pieces.over_limit + run.sent.lost) as i64,
        },
        lease_lost: run.sent.lease_lost,
    }
}

/// A follower's working state.
struct Run<'a> {
    sink: &'a dyn OutputSink,
    lease: &'a Lease,
    lines: LineReader,
    pieces: Pieces,
    outbox: Vec<OutputPiece>,
    sent: Sent,
    /// The place of the last part taken.
    seen: Option<Place>,
    /// In the current read: the timestamp of the last line begun and its place among the lines with that timestamp.
    read_at: Option<(OffsetDateTime, u32)>,
    oldest_unsent: Option<Instant>,
}

impl Run<'_> {
    fn take(&mut self, chunk: &[u8]) {
        for line in self.lines.push(chunk) {
            self.accept(line);
        }
    }

    fn end_of_stream(&mut self) {
        if let Some(line) = self.lines.finish() {
            self.accept(line);
        }
    }

    /// Skips a part taken before a reconnect, judged by its place, and builds pieces from the rest. A line without a
    /// timestamp cannot be placed and is always taken.
    fn accept(&mut self, line: Line) {
        if let Some(at) = line.at {
            let nth = match (self.read_at, line.part) {
                (Some((last, nth)), 0) if last == at => nth + 1,
                (_, 0) | (None, _) => 0,
                (Some((_, nth)), _) => nth,
            };
            self.read_at = Some((at, nth));
            let place = Place {
                at,
                nth,
                part: line.part,
            };
            if self.seen.is_some_and(|seen| place <= seen) {
                return;
            }
            self.seen = Some(place);
        }
        self.pieces.push(&line, Instant::now(), &mut self.outbox);
    }

    /// Cuts pieces that are old enough and sends the queue when it is full or its oldest piece is due.
    async fn tick(&mut self) {
        self.pieces.age(Instant::now(), &mut self.outbox);
        if self.outbox.is_empty() {
            return;
        }
        let since = *self.oldest_unsent.get_or_insert_with(Instant::now);
        if self.outbox.len() >= SEND_BATCH || since.elapsed() >= SEND_AGE {
            self.sent
                .send(self.sink, self.lease, &mut self.outbox, false)
                .await;
            self.oldest_unsent = None;
        }
    }

    /// Waits up to `wait`, or until `until` is cancelled, still cutting and sending pieces. True when the follower
    /// must stop: `stop` was cancelled or the lease is gone.
    async fn idle(
        &mut self,
        wait: Duration,
        stop: &CancellationToken,
        until: Option<&CancellationToken>,
    ) -> bool {
        let deadline = Instant::now() + wait;
        loop {
            self.tick().await;
            if self.sent.lease_lost || stop.is_cancelled() {
                return true;
            }
            let now = Instant::now();
            if now >= deadline || until.is_some_and(CancellationToken::is_cancelled) {
                return false;
            }
            let ended = async {
                match until {
                    Some(until) => until.cancelled().await,
                    None => std::future::pending().await,
                }
            };
            tokio::select! {
                _ = stop.cancelled() => return true,
                _ = ended => return false,
                _ = sleep(PIECE_AGE.min(deadline - now)) => {}
            }
        }
    }
}

/// What following a Pod's runner log needs from Kubernetes.
#[async_trait]
pub trait PodLogs: Send + Sync {
    /// The UID of the Pod with this name, or none when there is no such Pod.
    async fn pod_uid(&self, namespace: &str, pod: &str) -> Result<Option<String>>;
    /// The runner's log of the Pod with this name.
    async fn log(
        &self,
        namespace: &str,
        pod: &str,
        since: Option<OffsetDateTime>,
        follow: bool,
    ) -> Result<LogStream>;
}

#[async_trait]
impl PodLogs for super::sandbox::KubernetesHttpTransport {
    async fn pod_uid(&self, namespace: &str, pod: &str) -> Result<Option<String>> {
        use super::sandbox::SandboxTransport;
        Ok(self.get_pod(namespace, pod).await?.and_then(|pod| {
            pod.pointer("/metadata/uid")
                .and_then(serde_json::Value::as_str)
                .map(str::to_owned)
        }))
    }

    async fn log(
        &self,
        namespace: &str,
        pod: &str,
        since: Option<OffsetDateTime>,
        follow: bool,
    ) -> Result<LogStream> {
        self.pod_log(namespace, pod, "runner", since, follow).await
    }
}

/// The runner log of the Pod the driver observed, named and identified by its UID.
struct PodLog {
    logs: Arc<dyn PodLogs>,
    namespace: String,
    pod: String,
    uid: String,
}

impl PodLog {
    async fn same_pod(&self) -> Result<()> {
        let uid = self.logs.pod_uid(&self.namespace, &self.pod).await?;
        anyhow::ensure!(
            uid.as_deref() == Some(self.uid.as_str()),
            "the runner's Pod is not the one observed"
        );
        Ok(())
    }
}

#[async_trait]
impl LogSource for PodLog {
    /// A log is opened by its Pod's name, so the Pod's UID is checked before and after: a UID is never reused, so a Pod
    /// with the observed UID both times is the Pod whose log was opened. A replacement Pod's output is never read.
    async fn open(&self, since: Option<OffsetDateTime>, follow: bool) -> Result<LogStream> {
        self.same_pod().await?;
        let stream = self
            .logs
            .log(&self.namespace, &self.pod, since, follow)
            .await?;
        self.same_pod().await?;
        Ok(stream)
    }
}

struct Following {
    lease: Lease,
    ended: CancellationToken,
    stop: CancellationToken,
    handle: JoinHandle<Followed>,
}

/// Wraps the driver's runtime event recorder: a recorded Pod observation starts following that Pod's runner, and a
/// terminal observation or recovery closes the attempt before the event itself is recorded, so a closure always
/// precedes the task's end.
pub struct LiveOutputRecorder<'a, E> {
    inner: &'a E,
    sink: Arc<dyn OutputSink>,
    /// None when live output is off: every event passes straight through.
    logs: Option<Arc<dyn PodLogs>>,
    following: Mutex<Option<Following>>,
}

impl<'a, E> LiveOutputRecorder<'a, E> {
    pub fn new(inner: &'a E, sink: Arc<dyn OutputSink>, logs: Option<Arc<dyn PodLogs>>) -> Self {
        Self {
            inner,
            sink,
            logs,
            following: Mutex::new(None),
        }
    }

    async fn finish(&self, gently: bool) {
        let Some(following) = self.following.lock().await.take() else {
            return;
        };
        following.ended.cancel();
        if !gently {
            following.stop.cancel();
        }
        let mut handle = following.handle;
        let followed = match timeout(DRAIN, &mut handle).await {
            Ok(result) => result.ok(),
            Err(_) => {
                following.stop.cancel();
                handle.await.ok()
            }
        };
        let Some(followed) = followed else {
            return;
        };
        if followed.lease_lost {
            return;
        }
        if let Err(error) = self.sink.close(&following.lease, &followed.totals).await {
            tracing::warn!(event = "task_output_not_closed", task = %following.lease.task, %error);
        }
    }
}

impl<E> Drop for LiveOutputRecorder<'_, E> {
    fn drop(&mut self) {
        if let Some(following) = self.following.get_mut().take() {
            following.stop.cancel();
        }
    }
}

#[async_trait]
impl<E: RuntimeEventRecorder> RuntimeEventRecorder for LiveOutputRecorder<'_, E> {
    async fn record_runtime_event(
        &self,
        task: &ClaimedTask,
        worker_id: &str,
        event: &RuntimeEvent,
    ) -> Result<bool> {
        match event {
            RuntimeEvent::TerminalObserved { .. } => self.finish(true).await,
            RuntimeEvent::RecoveryNeeded { .. } => self.finish(false).await,
            _ => {}
        }
        let recorded = self
            .inner
            .record_runtime_event(task, worker_id, event)
            .await?;
        if let (
            true,
            RuntimeEvent::PodObserved {
                handle,
                observation,
            },
            Some(logs),
        ) = (recorded, event, &self.logs)
        {
            let mut following = self.following.lock().await;
            if following.is_none() {
                let lease = Lease {
                    task: task.id,
                    worker: worker_id.to_owned(),
                    generation: task.lease_generation,
                };
                let source: Arc<dyn LogSource> = Arc::new(PodLog {
                    logs: Arc::clone(logs),
                    namespace: handle.namespace.clone(),
                    pod: observation.pod_name.clone(),
                    uid: observation.pod_uid.clone(),
                });
                let (ended, stop) = (CancellationToken::new(), CancellationToken::new());
                let handle = tokio::spawn(follow(
                    source,
                    Arc::clone(&self.sink),
                    lease.clone(),
                    ended.clone(),
                    stop.clone(),
                ));
                *following = Some(Following {
                    lease,
                    ended,
                    stop,
                    handle,
                });
            }
        }
        Ok(recorded)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::broker::sandbox::{RuntimeObservation, SandboxHandle};
    use std::sync::Mutex as StdMutex;

    fn clean(raw: &[u8]) -> (String, u64) {
        let mut out = String::new();
        let removed = sanitize_line(raw, &mut out);
        (out, removed)
    }

    /// The database's two checks on stored text, restated.
    fn database_accepts(text: &str) -> bool {
        let mut rest = text;
        let mut plain = String::new();
        while let Some(at) = rest.find('\u{1b}') {
            plain.push_str(&rest[..at]);
            let after = &rest[at + 1..];
            let Some(params) = after.strip_prefix('[') else {
                return false;
            };
            let end = params
                .find(|c: char| !(c.is_ascii_digit() || c == ';' || c == ':'))
                .unwrap_or(params.len());
            if !params[end..].starts_with('m') {
                return false;
            }
            rest = &params[end + 1..];
        }
        plain.push_str(rest);
        !plain.chars().any(|c| {
            matches!(c, '\u{1}'..='\u{8}' | '\u{b}' | '\u{c}' | '\u{e}'..='\u{1a}' | '\u{1c}'..='\u{1f}' | '\u{7f}'..='\u{9f}')
        }) && !plain.contains('\u{1b}')
            && !text.chars().any(is_bidi_control)
    }

    #[test]
    fn only_text_tabs_returns_and_color_codes_pass() {
        for (raw, kept, removed) in [
            (&b"plain text"[..], "plain text", 0),
            (
                b"\x1b[1;31mred\x1b[0m \x1b[m",
                "\x1b[1;31mred\x1b[0m \x1b[m",
                0,
            ),
            (b"\x1b[38:2::255:0:0mtrue", "\x1b[38:2::255:0:0mtrue", 0),
            (b"a\x1b[2Kb\x1b[?25lc\x1b[10;5Hd", "abcd", 17),
            (b"\x1b]0;title\x07x", "x", 10),
            (b"\x1b]8;;http://x\x1b\\link\x1b]8;;\x1b\\", "link", 22),
            (b"\x1bPdata\x1b\\y\x1b_apc\x1b\\z", "yz", 15),
            (b"a\x00b\x07c\td\re\x7f\xc2\x85f", "abc\td\ref", 5),
            (b"ok\xffno\xe2\x82", "ok\u{fffd}no\u{fffd}", 3),
            (b"\x1b\tafter", "\tafter", 1),
            (b"\x1b(Bx", "x", 3),
            (b"\x1b(\nkept", "\nkept", 2),
            (b"\x1b[1\nnext", "\nnext", 3),
            // Direction overrides, embeddings, isolates and marks: U+202E then U+202C, U+2066 and U+2069, U+200F and
            // U+061C.
            (
                b"a\xe2\x80\xaeb\xe2\x80\xacc\xe2\x81\xa6d\xe2\x81\xa9e\xe2\x80\x8ff\xd8\x9cg",
                "abcdefg",
                17,
            ),
        ] {
            let (text, dropped) = clean(raw);
            assert_eq!((text.as_str(), dropped), (kept, removed), "{raw:?}");
        }
        let bidi: String = ('\u{0}'..='\u{ffff}')
            .filter(|&c| is_bidi_control(c))
            .collect();
        assert_eq!(
            bidi,
            "\u{61c}\u{200e}\u{200f}\u{202a}\u{202b}\u{202c}\u{202d}\u{202e}\u{2066}\u{2067}\u{2068}\u{2069}",
            "exactly Unicode's Bidi_Control set"
        );
        assert_eq!(clean(bidi.as_bytes()), (String::new(), bidi.len() as u64));
        let long = format!("\x1b[{}mx", "1;".repeat(40));
        assert_eq!(
            clean(long.as_bytes()).0,
            "x",
            "parameters past the SGR limit"
        );
        let mut open = b"\x1b]".to_vec();
        open.extend(std::iter::repeat_n(b'a', STRING_LIMIT + 10));
        assert_eq!(
            clean(&open).0,
            "a".repeat(10),
            "an unterminated string ends after its limit"
        );
    }

    #[test]
    fn any_bytes_sanitize_to_text_the_database_accepts() {
        let mut state = 0x9e37_79b9_7f4a_7c15_u64;
        let alphabet: Vec<u8> =
            b"\x1b[]0123456789;:m?Pa \t\r\x07\\\x00\x7f\xc2\x85\xe2\x82\xac\xff\x80\xae\x81\xa6\xd8\x9c".to_vec();
        for _ in 0..20_000 {
            let mut raw = Vec::new();
            for _ in 0..(state % 40) {
                state ^= state << 13;
                state ^= state >> 7;
                state ^= state << 17;
                raw.push(alphabet[(state % alphabet.len() as u64) as usize]);
            }
            let (text, removed) = clean(&raw);
            assert!(database_accepts(&text), "{raw:?} gave {text:?}");
            assert!(removed <= raw.len() as u64);
        }
    }

    #[test]
    fn lines_split_across_chunks_keep_their_timestamps() {
        let log = b"2026-10-12T08:00:00.123456789Z first\n2026-10-12T08:00:01Z \xe2\x82\xac second\nno stamp\n2026-10-12T08:00:02Z tail";
        for size in [1, 2, 7, log.len()] {
            let mut reader = LineReader::default();
            let mut lines = Vec::new();
            for chunk in log.chunks(size) {
                lines.extend(reader.push(chunk));
            }
            lines.extend(reader.finish());
            let shown: Vec<_> = lines
                .iter()
                .map(|line| {
                    (
                        line.at.map(|at| at.unix_timestamp()),
                        String::from_utf8(line.content.clone()).unwrap(),
                        line.newline,
                    )
                })
                .collect();
            assert_eq!(
                shown,
                [
                    (Some(1_791_792_000), "first".to_owned(), true),
                    (Some(1_791_792_001), "\u{20ac} second".to_owned(), true),
                    (None, "no stamp".to_owned(), true),
                    (Some(1_791_792_002), "tail".to_owned(), false),
                ],
                "chunks of {size}"
            );
        }
        let mut reader = LineReader::default();
        let mut long = vec![b'x'; MAX_LINE - 1];
        long.extend_from_slice("\u{20ac}\u{20ac}".as_bytes());
        let lines = reader.push(&long);
        assert_eq!(lines.len(), 1, "a line past the limit is cut");
        assert!(
            std::str::from_utf8(&lines[0].content).is_ok(),
            "at a character boundary"
        );
        assert!(std::str::from_utf8(&reader.finish().unwrap().content).is_ok());
    }

    #[test]
    fn pieces_hold_at_most_4_kib_and_stop_at_the_attempt_limit() {
        let now = Instant::now();
        let mut pieces = Pieces::default();
        let mut out = Vec::new();
        let line = |text: &str| Line {
            at: None,
            part: 0,
            content: text.as_bytes().to_vec(),
            newline: true,
        };
        pieces.push(&line(&format!("\x07{}", "y".repeat(5000))), now, &mut out);
        assert_eq!(out.len(), 1);
        assert_eq!(
            (out[0].text.len(), out[0].bytes, out[0].dropped),
            (4096, 5002, 1)
        );
        pieces.finish(&mut out);
        assert_eq!(
            (out[1].text.len(), out[1].bytes, out[1].dropped),
            (905, 0, 0)
        );
        assert!(out[1].text.ends_with('\n'));
        let mut pieces = Pieces::default();
        let mut out = Vec::new();
        let block = "z".repeat(4095);
        for _ in 0..1030 {
            pieces.push(&line(&block), now, &mut out);
        }
        pieces.finish(&mut out);
        let kept: u64 = out.iter().map(|piece| piece.text.len() as u64).sum();
        assert_eq!(out.len(), 1024);
        assert!(kept <= ATTEMPT_LIMIT);
        assert_eq!(
            pieces.over_limit,
            6 * 4096,
            "every line past the limit is counted"
        );
        let mut pieces = Pieces::default();
        let mut out = Vec::new();
        pieces.push(&line("short"), now, &mut out);
        pieces.age(now + PIECE_AGE / 2, &mut out);
        assert!(out.is_empty());
        pieces.age(now + PIECE_AGE, &mut out);
        assert_eq!(out.len(), 1, "a piece is cut when its text is old enough");
    }

    /// In a script, a read that breaks off here.
    const BROKEN: &[u8] = b"<the connection breaks>";

    /// Plays scripted log reads: each open returns the next script; `since` and `follow` are kept for checks.
    struct Script {
        reads: StdMutex<Vec<Result<Vec<&'static [u8]>>>>,
        opened: StdMutex<Vec<(Option<i64>, bool)>>,
    }

    #[async_trait]
    impl LogSource for Script {
        async fn open(&self, since: Option<OffsetDateTime>, follow: bool) -> Result<LogStream> {
            self.opened
                .lock()
                .unwrap()
                .push((since.map(OffsetDateTime::unix_timestamp), follow));
            let mut reads = self.reads.lock().unwrap();
            if reads.is_empty() {
                anyhow::bail!("no more reads");
            }
            let chunks = reads.remove(0)?;
            Ok(Box::pin(futures_util::stream::iter(
                chunks.into_iter().map(|chunk| {
                    if chunk == BROKEN {
                        Err(anyhow::anyhow!("connection reset"))
                    } else {
                        Ok(chunk.to_vec())
                    }
                }),
            )))
        }
    }

    #[derive(Default)]
    struct Sink {
        stored: StdMutex<Vec<OutputPiece>>,
        fail_next: StdMutex<u32>,
        lose_lease: bool,
        closes: StdMutex<Vec<OutputTotals>>,
        log: Arc<StdMutex<Vec<String>>>,
    }

    #[async_trait]
    impl OutputSink for Sink {
        async fn record(&self, _: &Lease, first: i32, pieces: &[OutputPiece]) -> Result<bool> {
            if self.lose_lease {
                return Ok(false);
            }
            let mut fail = self.fail_next.lock().unwrap();
            if *fail > 0 {
                *fail -= 1;
                anyhow::bail!("database unavailable");
            }
            let mut stored = self.stored.lock().unwrap();
            assert_eq!(first as usize, stored.len(), "first names what is stored");
            stored.extend_from_slice(pieces);
            Ok(true)
        }

        async fn close(&self, _: &Lease, totals: &OutputTotals) -> Result<()> {
            self.log.lock().unwrap().push("close".into());
            self.closes.lock().unwrap().push(totals.clone());
            Ok(())
        }
    }

    fn claimed() -> ClaimedTask {
        let text = |value: &str| value.to_owned();
        ClaimedTask {
            id: Uuid::nil(),
            principal_id: Uuid::nil(),
            group_key: text("g"),
            profile_key: text("p"),
            objective: text("o"),
            logical_agent: text("a"),
            lease_generation: 3,
            attempt: 1,
            max_task_attempts: 2,
            max_provider_attempts: 2,
            harness: text("h"),
            thinking: None,
            route: text("direct"),
            controller_profile: text("c"),
            gateway_profile: text("g"),
            image_ref: text("i"),
            cpu_limit: text("1"),
            memory_limit: text("1Gi"),
            state_profile: text("s"),
            role: text("r"),
            max_run_seconds: 60,
        }
    }

    fn lease() -> Lease {
        Lease {
            task: Uuid::nil(),
            worker: "worker-1".into(),
            generation: 1,
        }
    }

    #[tokio::test(start_paused = true)]
    async fn the_follower_reconnects_without_repeating_lines_and_closes_on_what_it_stored() {
        let source = Arc::new(Script {
            reads: StdMutex::new(vec![
                Err(anyhow::anyhow!("container is waiting to start")),
                Ok(vec![b"2026-10-12T08:00:00.1Z one\n2026-10-12T08:00:00.5Z tw", b"o\n"]),
                // The connection dropped; the reread starts at the second and repeats what was seen.
                Ok(vec![b"2026-10-12T08:00:00.1Z one\n2026-10-12T08:00:00.5Z two\n2026-10-12T08:00:01Z three\n"]),
                // After the runner ended: one read without following.
                Ok(vec![b"2026-10-12T08:00:01Z three\n2026-10-12T08:00:02Z \x1b[31mfour\x1b[0m\n"]),
            ]),
            opened: StdMutex::new(Vec::new()),
        });
        let sink = Arc::new(Sink::default());
        *sink.fail_next.lock().unwrap() = 1;
        let (ended, stop) = (CancellationToken::new(), CancellationToken::new());
        let run = tokio::spawn(follow(
            source.clone(),
            sink.clone(),
            lease(),
            ended.clone(),
            stop.clone(),
        ));
        while source.opened.lock().unwrap().len() < 3 {
            sleep(Duration::from_millis(100)).await;
        }
        ended.cancel();
        let followed = run.await.unwrap();
        let stored = sink.stored.lock().unwrap().clone();
        let text: String = stored.iter().map(|piece| piece.text.as_str()).collect();
        assert_eq!(text, "one\ntwo\nthree\n\x1b[31mfour\x1b[0m\n");
        assert!(!followed.lease_lost);
        assert_eq!(followed.totals.pieces as usize, stored.len());
        assert_eq!(followed.totals.size, text.len() as i64);
        assert_eq!(
            followed.totals.sha256,
            hex::encode(Sha256::digest(text.as_bytes()))
        );
        assert_eq!(followed.totals.dropped, 0);
        let opened = source.opened.lock().unwrap().clone();
        assert_eq!(opened[0], (None, true));
        assert_eq!(
            opened[2],
            (Some(1_791_792_000), true),
            "reopened from the last line's second"
        );
        assert!(!opened.last().unwrap().1, "the last read does not follow");
        assert!(stored.iter().all(|piece| piece.stream == "combined"));
    }

    /// What a follower stores from scripted reads, the first while the runner works and the rest after it ended.
    async fn followed_text(reads: Vec<Result<Vec<&'static [u8]>>>, working: usize) -> String {
        let source = Arc::new(Script {
            reads: StdMutex::new(reads),
            opened: StdMutex::new(Vec::new()),
        });
        let sink = Arc::new(Sink::default());
        let (ended, stop) = (CancellationToken::new(), CancellationToken::new());
        let run = tokio::spawn(follow(
            source.clone(),
            sink.clone(),
            lease(),
            ended.clone(),
            stop.clone(),
        ));
        while source.opened.lock().unwrap().len() < working {
            sleep(Duration::from_millis(100)).await;
        }
        ended.cancel();
        let followed = run.await.unwrap();
        let stored = sink.stored.lock().unwrap().clone();
        let text: String = stored.iter().map(|piece| piece.text.as_str()).collect();
        assert_eq!(
            followed.totals.sha256,
            hex::encode(Sha256::digest(text.as_bytes()))
        );
        text
    }

    #[tokio::test(start_paused = true)]
    async fn a_broken_read_gives_its_unfinished_line_again_whole() {
        let text = followed_text(
            vec![
                Ok(vec![b"2026-10-12T08:00:00.1Z one\n2026-10-12T08:00:00.5Z tw", BROKEN]),
                Ok(vec![b"2026-10-12T08:00:00.1Z one\n2026-10-12T08:00:00.5Z two\n2026-10-12T08:00:01Z three\n"]),
                Ok(vec![b"2026-10-12T08:00:01Z three\n"]),
            ],
            2,
        )
        .await;
        assert_eq!(text, "one\ntwo\nthree\n");
    }

    #[tokio::test(start_paused = true)]
    async fn lines_that_share_a_timestamp_are_each_kept_once() {
        let text = followed_text(
            vec![
                Ok(vec![b"2026-10-12T08:00:00Z a\n2026-10-12T08:00:00Z b\n", BROKEN]),
                Ok(vec![b"2026-10-12T08:00:00Z a\n2026-10-12T08:00:00Z b\n2026-10-12T08:00:00Z b\n"]),
                Ok(vec![b"2026-10-12T08:00:00Z a\n2026-10-12T08:00:00Z b\n2026-10-12T08:00:00Z b\n2026-10-12T08:00:01Z c\n"]),
            ],
            2,
        )
        .await;
        assert_eq!(text, "a\nb\nb\nc\n");
    }

    #[tokio::test(start_paused = true)]
    async fn a_long_line_broken_off_is_finished_without_repeating_its_parts() {
        let stamp = "2026-10-12T08:00:00Z ";
        let content = "x".repeat(MAX_LINE + 1000);
        let first: &'static [u8] =
            Box::leak(format!("{stamp}{content}").into_bytes().into_boxed_slice());
        let whole: &'static [u8] = Box::leak(
            format!("{stamp}{content}\n")
                .into_bytes()
                .into_boxed_slice(),
        );
        let text = followed_text(
            vec![Ok(vec![first, BROKEN]), Ok(vec![whole]), Ok(vec![whole])],
            2,
        )
        .await;
        assert_eq!(text, format!("{content}\n"));
    }

    #[test]
    fn a_long_line_is_cut_at_the_same_places_however_it_arrives() {
        let stamp = "2026-10-12T08:00:00Z ";
        // Text past the first cut that looks like a timestamp stays the program's text.
        let mut raw = format!("{stamp}{}", "a".repeat(MAX_LINE - stamp.len()));
        raw.push_str("2026-10-12T09:00:00Z fake ");
        raw.push_str(&"\u{e9}b".repeat(MAX_LINE / 2));
        raw.push('\n');
        let parts = |size: usize| {
            let mut reader = LineReader::default();
            let mut lines = Vec::new();
            for chunk in raw.as_bytes().chunks(size) {
                lines.extend(reader.push(chunk));
            }
            assert!(reader.finish().is_none());
            lines
                .into_iter()
                .map(|line| (line.at, line.part, line.newline, line.content))
                .collect::<Vec<_>>()
        };
        let whole = parts(raw.len());
        assert_eq!(whole, parts(65_536));
        assert_eq!(whole, parts(999));
        let at = OffsetDateTime::parse("2026-10-12T08:00:00Z", &Rfc3339).ok();
        assert_eq!(
            whole
                .iter()
                .map(|(when, part, newline, content)| (
                    *when,
                    *part,
                    *newline,
                    content.len() <= MAX_LINE
                ))
                .collect::<Vec<_>>(),
            [
                (at, 0, false, true),
                (at, 1, false, true),
                (at, 2, true, true)
            ]
        );
        assert!(whole[1].3.starts_with(b"2026-10-12T09:00:00Z fake "));
        let joined: Vec<u8> = whole
            .iter()
            .flat_map(|(_, _, _, content)| content.clone())
            .collect();
        assert_eq!(joined, raw.as_bytes()[stamp.len()..raw.len() - 1]);
        assert!(
            whole
                .iter()
                .all(|(_, _, _, content)| std::str::from_utf8(content).is_ok())
        );
    }

    #[test]
    fn a_piece_never_splits_a_color_code() {
        for at in PIECE_TEXT - 70..PIECE_TEXT + 5 {
            let text = format!("{}\x1b[38;2;255;0;0mred\x1b[0m tail", "a".repeat(at));
            let mut pieces = Pieces::default();
            let mut out = Vec::new();
            let now = Instant::now();
            pieces.push(
                &Line {
                    at: None,
                    part: 0,
                    content: text.clone().into_bytes(),
                    newline: true,
                },
                now,
                &mut out,
            );
            pieces.finish(&mut out);
            assert!(
                out.iter().all(|piece| database_accepts(&piece.text)),
                "{at}"
            );
            assert!(
                out.iter().all(|piece| piece.text.len() <= PIECE_TEXT),
                "{at}"
            );
            let joined: String = out.iter().map(|piece| piece.text.as_str()).collect();
            assert_eq!(joined, format!("{text}\n"), "{at}");
        }
    }

    #[tokio::test(start_paused = true)]
    async fn a_lost_lease_stops_the_follower_without_a_closure() {
        let source = Arc::new(Script {
            reads: StdMutex::new(vec![Ok(vec![b"2026-10-12T08:00:00Z one\n"])]),
            opened: StdMutex::new(Vec::new()),
        });
        let sink = Arc::new(Sink {
            lose_lease: true,
            ..Sink::default()
        });
        let followed = follow(
            source,
            sink.clone(),
            lease(),
            CancellationToken::new(),
            CancellationToken::new(),
        )
        .await;
        assert!(followed.lease_lost);
        assert!(sink.stored.lock().unwrap().is_empty());
    }

    struct Inner(Arc<StdMutex<Vec<String>>>);

    #[async_trait]
    impl RuntimeEventRecorder for Inner {
        async fn record_runtime_event(
            &self,
            _: &ClaimedTask,
            _: &str,
            event: &RuntimeEvent,
        ) -> Result<bool> {
            self.0.lock().unwrap().push(event.code().into());
            Ok(true)
        }
    }

    /// Pod logs from a script, for a Pod whose UID reads in turn from `uids` (the observed "pod" once they run out).
    struct Logs(Arc<Script>, StdMutex<Vec<Option<&'static str>>>);

    #[async_trait]
    impl PodLogs for Logs {
        async fn pod_uid(&self, namespace: &str, pod: &str) -> Result<Option<String>> {
            assert_eq!((namespace, pod), ("sandbox-lab", "run-1"));
            let mut uids = self.1.lock().unwrap();
            let uid = if uids.is_empty() {
                Some("pod")
            } else {
                uids.remove(0)
            };
            Ok(uid.map(str::to_owned))
        }

        async fn log(
            &self,
            namespace: &str,
            pod: &str,
            since: Option<OffsetDateTime>,
            follow: bool,
        ) -> Result<LogStream> {
            assert_eq!((namespace, pod), ("sandbox-lab", "run-1"));
            self.0.open(since, follow).await
        }
    }

    #[tokio::test]
    async fn a_log_is_read_only_from_the_pod_observed() {
        for (uids, opened, read) in [
            (vec![Some("pod"), Some("pod")], 1, true),
            (vec![Some("other")], 0, false),
            (vec![None], 0, false),
            // Replaced while its log was opened: the stream is refused.
            (vec![Some("pod"), Some("other")], 1, false),
        ] {
            let script = Arc::new(Script {
                reads: StdMutex::new(vec![Ok(vec![b"2026-10-12T08:00:00Z one\n"])]),
                opened: StdMutex::new(Vec::new()),
            });
            let source = PodLog {
                logs: Arc::new(Logs(script.clone(), StdMutex::new(uids.clone()))),
                namespace: "sandbox-lab".into(),
                pod: "run-1".into(),
                uid: "pod".into(),
            };
            assert_eq!(source.open(None, true).await.is_ok(), read, "{uids:?}");
            assert_eq!(script.opened.lock().unwrap().len(), opened, "{uids:?}");
        }
    }

    #[tokio::test(start_paused = true)]
    async fn the_attempt_is_closed_before_the_terminal_event_is_recorded() {
        let log = Arc::new(StdMutex::new(Vec::new()));
        let inner = Inner(log.clone());
        let sink = Arc::new(Sink {
            log: log.clone(),
            ..Sink::default()
        });
        let script = Arc::new(Script {
            reads: StdMutex::new(vec![
                Ok(vec![b"2026-10-12T08:00:00Z working\n"]),
                Ok(vec![b"2026-10-12T08:00:01Z done\n"]),
            ]),
            opened: StdMutex::new(Vec::new()),
        });
        let recorder = LiveOutputRecorder::new(
            &inner,
            sink.clone(),
            Some(Arc::new(Logs(script, StdMutex::new(Vec::new())))),
        );
        let task = claimed();
        let handle = SandboxHandle {
            task_id: Uuid::nil(),
            worker_id: "worker-1".into(),
            lease_generation: 3,
            namespace: "sandbox-lab".into(),
            lab_ref: "lab".into(),
            sandbox_name: "run-1".into(),
            sandbox_uid: "uid".into(),
            sandbox_generation: 1,
        };
        let observation = RuntimeObservation {
            sandbox_uid: "uid".into(),
            sandbox_generation: 1,
            pod_name: "run-1".into(),
            pod_uid: "pod".into(),
            runtime_class_name: "kata-clh".into(),
        };
        recorder
            .record_runtime_event(
                &task,
                "worker-1",
                &RuntimeEvent::PodObserved {
                    handle: handle.clone(),
                    observation: observation.clone(),
                },
            )
            .await
            .unwrap();
        sleep(Duration::from_secs(2)).await;
        recorder
            .record_runtime_event(
                &task,
                "worker-1",
                &RuntimeEvent::TerminalObserved {
                    handle,
                    observation,
                    exit_code: 0,
                },
            )
            .await
            .unwrap();
        assert_eq!(
            *log.lock().unwrap(),
            ["sandbox_pod_observed", "close", "sandbox_terminal_observed"]
        );
        let closes = sink.closes.lock().unwrap().clone();
        let text: String = sink
            .stored
            .lock()
            .unwrap()
            .iter()
            .map(|piece| piece.text.clone())
            .collect();
        assert_eq!(text, "working\ndone\n");
        assert_eq!(
            closes[0].sha256,
            hex::encode(Sha256::digest(text.as_bytes()))
        );
    }

    #[tokio::test]
    async fn without_pod_logs_events_pass_straight_through() {
        let log = Arc::new(StdMutex::new(Vec::new()));
        let inner = Inner(log.clone());
        let sink = Arc::new(Sink::default());
        let recorder = LiveOutputRecorder::new(&inner, sink.clone(), None);
        let task = claimed();
        recorder
            .record_runtime_event(
                &task,
                "worker-1",
                &RuntimeEvent::RecoveryNeeded {
                    handle: None,
                    code: "stopped".into(),
                },
            )
            .await
            .unwrap();
        assert_eq!(*log.lock().unwrap(), ["sandbox_recovery_needed"]);
        assert!(sink.closes.lock().unwrap().is_empty());
    }
}
