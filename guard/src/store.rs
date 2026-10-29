//! The guard's volume: each person's quarantine scope with the originals and an index of intakes, the intake records,
//! the disarmed copies waiting for their callers, and recorded exceptions. Every file is owner-only. An hourly sweep
//! deletes what is past its retention date, drops index lines older than the quota and retry windows need, and
//! removes expired exceptions.
//!
//! The store counts every byte it writes (originals, copies, records, index lines and exceptions) against its cap.
//! Before an intake is evaluated it takes a reservation under one lock: the person's quota is checked against their
//! index and their other intakes in flight, and the store's room against its bytes and every other reservation, so
//! concurrent intakes cannot all pass the same check. The room covers the intake's work directory too: each helper's
//! output file is limited (`work_file_limit`), and the directory removes itself when the intake ends, however it
//! ends. A refusal at that point writes nothing. A write outside a reservation (a retry's record, an exception) is
//! checked against the cap under the same lock.
//!
//! Layout under the root:
//! `scopes/<tenant>/<person digest>/objects/<intake id>`, `scopes/<tenant>/<person digest>/index.jsonl`,
//! `records/<date>/<intake id>.json`, `disarmed/<intake id>`, `exceptions/<use case>/<sha256>.json`, `work/<intake id>/`.
use crate::detect::FileType;
use crate::finding::Finding;
use crate::pipeline::{Exception, Outcome, Versions};
use crate::policy::Quota;
use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::io::Write;
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};
use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;
use uuid::Uuid;

pub const RECORD_SCHEMA: &str = "enrichedlab.guard-intake.v1";
/// Retry times a record keeps; `retry_count` counts every retry.
const RETRIES_KEPT: usize = 16;
/// Room an intake's record and index line may take.
const RECORD_ROOM: u64 = 64 * 1024;
/// Index lines older than this serve neither the daily quota nor the retry window.
const INDEX_DAYS: i64 = 2;

/// The largest rebuilt copy the guard keeps for an original of this size; its reservation covers that much.
pub fn rebuilt_limit(original: u64) -> u64 {
    original
        .saturating_mul(2)
        .max(original.saturating_add(64 * 1024))
}

/// The largest file any helper may write while checking an original of this size.
pub fn work_file_limit(original: u64) -> u64 {
    original.saturating_mul(4).max(64 * 1024 * 1024)
}

/// The work directory's room for one intake: the original's copy and up to three helper outputs.
pub fn work_room(original: u64) -> u64 {
    original.saturating_add(work_file_limit(original).saturating_mul(3))
}

/// The store has no room for a write outside a reservation.
#[derive(Debug)]
pub struct StoreFull;

impl std::fmt::Display for StoreFull {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "the guard's store is full")
    }
}

impl std::error::Error for StoreFull {}

/// One intake's private work directory, removed when dropped, so a cancelled request leaves nothing behind.
pub struct Work {
    path: PathBuf,
}

impl Work {
    pub fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for Work {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.path);
    }
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct Disarmed {
    pub sha256: String,
    pub bytes: u64,
    pub media_type: String,
    pub expires_at: String,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct Retention {
    pub original_until: String,
    pub record_until: String,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct Usage {
    pub files_24h: u64,
    pub bytes_24h: u64,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct Record {
    pub schema: String,
    pub intake_id: String,
    pub received_at: String,
    pub completed_at: String,
    pub caller: String,
    pub tenant: String,
    pub lab: String,
    pub project: Option<String>,
    /// SHA-256 of the tenant and the caller's person identifier; the identifier itself is not stored.
    pub person: String,
    pub use_case: String,
    pub file_name: Option<String>,
    pub bytes: u64,
    pub sha256: String,
    pub declared_type: FileType,
    pub detected_type: FileType,
    pub policy_version: String,
    pub versions: Versions,
    pub findings: Vec<Finding>,
    pub outcome: Outcome,
    pub refusal: Option<String>,
    pub disarmed: Option<Disarmed>,
    pub exception: Option<Exception>,
    pub retention: Retention,
    pub usage: Usage,
    /// The most recent retries; `retry_count` has them all.
    pub retries: Vec<String>,
    #[serde(default)]
    pub retry_count: u64,
}

impl Record {
    /// Notes the same bytes sent again, keeping the record's size bounded.
    pub fn note_retry(&mut self, at: OffsetDateTime) {
        self.retry_count = self.retry_count.max(self.retries.len() as u64) + 1;
        self.retries.push(stamp(at));
        if self.retries.len() > RETRIES_KEPT {
            let extra = self.retries.len() - RETRIES_KEPT;
            self.retries.drain(..extra);
        }
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
struct IndexEntry {
    id: String,
    at: String,
    use_case: String,
    sha256: String,
    bytes: u64,
    outcome: Outcome,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize)]
pub struct Swept {
    pub originals: u64,
    pub disarmed: u64,
    pub records: u64,
    pub exceptions: u64,
    pub index_entries: u64,
    pub work: u64,
}

/// Why an intake was refused before it was evaluated.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Refusal {
    Quota,
    Full,
}

/// Room and quota held for one intake in flight, released when dropped.
pub struct Reservation<'a> {
    store: &'a Store,
    person: String,
    bytes: u64,
    room: u64,
    pub usage: Usage,
}

impl Drop for Reservation<'_> {
    fn drop(&mut self) {
        let mut ledger = self.store.ledger.lock().unwrap_or_else(|e| e.into_inner());
        ledger.reserved = ledger.reserved.saturating_sub(self.room);
        if let Some(held) = ledger.people.get_mut(&self.person) {
            held.0 = held.0.saturating_sub(1);
            held.1 = held.1.saturating_sub(self.bytes);
            if held.0 == 0 {
                ledger.people.remove(&self.person);
            }
        }
    }
}

#[derive(Default)]
struct Ledger {
    /// Store bytes held for intakes in flight.
    reserved: u64,
    /// Per person (tenant and digest): intakes in flight and their bytes.
    people: HashMap<String, (u64, u64)>,
}

pub struct Store {
    root: PathBuf,
    used: AtomicU64,
    pub max_bytes: u64,
    /// Held for each reservation, index append and index compaction.
    ledger: Mutex<Ledger>,
}

pub fn now() -> OffsetDateTime {
    OffsetDateTime::now_utc()
        .replace_nanosecond(0)
        .unwrap_or_else(|_| OffsetDateTime::now_utc())
}

pub fn stamp(at: OffsetDateTime) -> String {
    at.format(&Rfc3339).unwrap_or_default()
}

pub fn parse(at: &str) -> Option<OffsetDateTime> {
    OffsetDateTime::parse(at, &Rfc3339).ok()
}

pub fn sha256(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}

pub fn person_digest(tenant: &str, person: &str) -> String {
    let mut h = Sha256::new();
    h.update(tenant.as_bytes());
    h.update([0]);
    h.update(person.as_bytes());
    hex::encode(&h.finalize()[..16])
}

fn private_dir(path: &Path) -> Result<()> {
    std::fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(path)
        .with_context(|| format!("{} not created", path.display()))
}

/// Writes a file atomically with mode 0600: a temporary name in the same directory, synced, then renamed.
pub fn write_private(path: &Path, bytes: &[u8]) -> Result<()> {
    let dir = path.parent().context("no parent directory")?;
    private_dir(dir)?;
    let temporary = dir.join(format!(".{}.tmp", Uuid::now_v7()));
    {
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&temporary)
            .with_context(|| format!("{} not created", temporary.display()))?;
        file.write_all(bytes)?;
        file.sync_all()?;
    }
    std::fs::rename(&temporary, path).with_context(|| format!("{} not written", path.display()))
}

fn size_of(path: &Path) -> u64 {
    std::fs::metadata(path).map(|m| m.len()).unwrap_or(0)
}

fn intake_date(id: &str) -> Result<String> {
    let uuid = Uuid::parse_str(id).context("intake id is not a UUID")?;
    let (seconds, _) = uuid
        .get_timestamp()
        .context("intake id carries no time")?
        .to_unix();
    let at = OffsetDateTime::from_unix_timestamp(seconds as i64)?;
    Ok(format!(
        "{:04}-{:02}-{:02}",
        at.year(),
        at.month() as u8,
        at.day()
    ))
}

fn dir_bytes(path: &Path) -> u64 {
    let Ok(entries) = std::fs::read_dir(path) else {
        return 0;
    };
    entries
        .filter_map(|e| e.ok())
        .map(|e| {
            let p = e.path();
            match e.file_type() {
                Ok(t) if t.is_dir() => dir_bytes(&p),
                Ok(t) if t.is_file() => e.metadata().map(|m| m.len()).unwrap_or(0),
                _ => 0,
            }
        })
        .sum()
}

fn subdirs(path: &Path) -> Vec<PathBuf> {
    std::fs::read_dir(path)
        .map(|entries| {
            entries
                .filter_map(|e| e.ok())
                .filter(|e| e.file_type().is_ok_and(|t| t.is_dir()))
                .map(|e| e.path())
                .collect()
        })
        .unwrap_or_default()
}

const COUNTED: [&str; 4] = ["scopes", "records", "disarmed", "exceptions"];

impl Store {
    pub fn open(root: &Path, max_bytes: u64) -> Result<Self> {
        for sub in COUNTED.iter().chain(&["work"]) {
            private_dir(&root.join(sub))?;
        }
        let used = COUNTED.iter().map(|sub| dir_bytes(&root.join(sub))).sum();
        Ok(Store {
            root: root.to_path_buf(),
            used: AtomicU64::new(used),
            max_bytes,
            ledger: Mutex::new(Ledger::default()),
        })
    }

    pub fn used(&self) -> u64 {
        self.used.load(Ordering::Relaxed)
    }

    fn grow(&self, bytes: u64) {
        self.used.fetch_add(bytes, Ordering::Relaxed);
    }

    fn shrink(&self, bytes: u64) {
        let _ = self
            .used
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |u| {
                Some(u.saturating_sub(bytes))
            });
    }

    fn ledger(&self) -> std::sync::MutexGuard<'_, Ledger> {
        self.ledger.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Writes or replaces a counted file whose room a reservation holds, keeping the byte count exact.
    fn put_reserved(&self, path: &Path, bytes: &[u8]) -> Result<()> {
        let before = size_of(path);
        write_private(path, bytes)?;
        self.grow(bytes.len() as u64);
        self.shrink(before);
        Ok(())
    }

    /// Writes or replaces a counted file outside a reservation: refused with `StoreFull` when its growth would pass
    /// the cap with every reservation held. The check and the write happen under the ledger lock.
    fn put(&self, path: &Path, bytes: &[u8]) -> Result<()> {
        let ledger = self.ledger();
        let growth = (bytes.len() as u64).saturating_sub(size_of(path));
        if self.used() + ledger.reserved + growth > self.max_bytes {
            return Err(StoreFull.into());
        }
        self.put_reserved(path, bytes)
    }

    /// Removes a counted file; true when it was there.
    fn remove(&self, path: &Path) -> bool {
        let size = size_of(path);
        let removed = std::fs::remove_file(path).is_ok();
        if removed {
            self.shrink(size);
        }
        removed
    }

    pub fn writable(&self) -> bool {
        write_private(&self.root.join("work").join(".probe"), b"ok").is_ok()
    }

    fn scope(&self, tenant: &str, person: &str) -> PathBuf {
        self.root.join("scopes").join(tenant).join(person)
    }

    pub fn work_dir(&self, id: &str) -> Result<Work> {
        let path = self.root.join("work").join(id);
        private_dir(&path)?;
        Ok(Work { path })
    }

    fn record_path(&self, id: &str) -> Result<PathBuf> {
        Ok(self
            .root
            .join("records")
            .join(intake_date(id)?)
            .join(format!("{id}.json")))
    }

    fn index(&self, tenant: &str, person: &str) -> Vec<IndexEntry> {
        let Ok(text) = std::fs::read_to_string(self.scope(tenant, person).join("index.jsonl"))
        else {
            return Vec::new();
        };
        text.lines()
            .filter_map(|l| serde_json::from_str(l).ok())
            .collect()
    }

    fn append_index(&self, tenant: &str, person: &str, entry: &IndexEntry) -> Result<()> {
        let dir = self.scope(tenant, person);
        private_dir(&dir)?;
        let line = format!("{}\n", serde_json::to_string(entry)?);
        let _ledger = self.ledger();
        let mut file = std::fs::OpenOptions::new()
            .append(true)
            .create(true)
            .mode(0o600)
            .open(dir.join("index.jsonl"))?;
        file.write_all(line.as_bytes())?;
        file.sync_all()?;
        self.grow(line.len() as u64);
        Ok(())
    }

    /// The person's intakes of the last day that were evaluated or started, for the quota.
    pub fn usage(&self, tenant: &str, person: &str, at: OffsetDateTime) -> Usage {
        let since = at - time::Duration::days(1);
        let recent: Vec<_> = self
            .index(tenant, person)
            .into_iter()
            .filter(|e| parse(&e.at).is_some_and(|t| t > since))
            .collect();
        Usage {
            files_24h: recent.len() as u64,
            bytes_24h: recent.iter().map(|e| e.bytes).sum(),
        }
    }

    /// Holds the person's quota and the store's room for one intake of `bytes`, or says why not. The index read and
    /// the check happen under the ledger lock, so two intakes cannot both pass on the same room.
    pub fn reserve(
        &self,
        tenant: &str,
        person: &str,
        at: OffsetDateTime,
        bytes: u64,
        quota: &Quota,
    ) -> Result<Reservation<'_>, Refusal> {
        let key = format!("{tenant}/{person}");
        let mut ledger = self.ledger();
        let usage = self.usage(tenant, person, at);
        let (files, held) = ledger.people.get(&key).copied().unwrap_or((0, 0));
        if usage.files_24h + files + 1 > quota.files_per_day
            || usage.bytes_24h + held + bytes > quota.bytes_per_day
        {
            return Err(Refusal::Quota);
        }
        let room = bytes + rebuilt_limit(bytes) + RECORD_ROOM + work_room(bytes);
        if self.used() + ledger.reserved + room > self.max_bytes {
            return Err(Refusal::Full);
        }
        ledger.reserved += room;
        let entry = ledger.people.entry(key.clone()).or_insert((0, 0));
        entry.0 += 1;
        entry.1 += bytes;
        Ok(Reservation {
            store: self,
            person: key,
            bytes,
            room,
            usage,
        })
    }

    /// The same bytes from the same person for the same use case within the window, evaluated (not refused).
    pub fn recent(
        &self,
        tenant: &str,
        person: &str,
        use_case: &str,
        digest: &str,
        within: time::Duration,
        at: OffsetDateTime,
    ) -> Option<Record> {
        let since = at - within;
        let entry = self.index(tenant, person).into_iter().rev().find(|e| {
            e.use_case == use_case
                && e.sha256 == digest
                && e.outcome != Outcome::Refused
                && parse(&e.at).is_some_and(|t| t > since)
        })?;
        self.read_record(&entry.id).ok().flatten()
    }

    pub fn read_record(&self, id: &str) -> Result<Option<Record>> {
        let path = self.record_path(id)?;
        match std::fs::read(&path) {
            Ok(bytes) => Ok(Some(
                serde_json::from_slice(&bytes).context("record unreadable")?,
            )),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(e.into()),
        }
    }

    pub fn write_record(&self, record: &Record) -> Result<()> {
        self.put(
            &self.record_path(&record.intake_id)?,
            &serde_json::to_vec_pretty(record)?,
        )
    }

    /// Stores an intake under its reservation: the original in the person's scope, the disarmed copy, the record and
    /// its index line. A refused intake stores no original.
    pub fn commit(
        &self,
        _reservation: &Reservation,
        record: &Record,
        original: Option<&[u8]>,
        disarmed: Option<&[u8]>,
    ) -> Result<()> {
        if let Some(bytes) = original {
            self.put_reserved(
                &self
                    .scope(&record.tenant, &record.person)
                    .join("objects")
                    .join(&record.intake_id),
                bytes,
            )?;
        }
        if let Some(bytes) = disarmed {
            self.put_reserved(&self.root.join("disarmed").join(&record.intake_id), bytes)?;
        }
        self.put_reserved(
            &self.record_path(&record.intake_id)?,
            &serde_json::to_vec_pretty(record)?,
        )?;
        self.append_index(
            &record.tenant,
            &record.person,
            &IndexEntry {
                id: record.intake_id.clone(),
                at: record.received_at.clone(),
                use_case: record.use_case.clone(),
                sha256: record.sha256.clone(),
                bytes: record.bytes,
                outcome: record.outcome,
            },
        )
    }

    pub fn read_disarmed(&self, record: &Record, at: OffsetDateTime) -> Result<Option<Vec<u8>>> {
        let Some(disarmed) = &record.disarmed else {
            return Ok(None);
        };
        if parse(&disarmed.expires_at).is_none_or(|t| t <= at) {
            return Ok(None);
        }
        match std::fs::read(self.root.join("disarmed").join(&record.intake_id)) {
            Ok(bytes) if sha256(&bytes) == disarmed.sha256 => Ok(Some(bytes)),
            Ok(_) => bail!("disarmed copy does not match its record"),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(e.into()),
        }
    }

    pub fn exception(&self, use_case: &str, digest: &str, at: OffsetDateTime) -> Option<Exception> {
        let bytes = std::fs::read(
            self.root
                .join("exceptions")
                .join(use_case)
                .join(format!("{digest}.json")),
        )
        .ok()?;
        let exception: Exception = serde_json::from_slice(&bytes).ok()?;
        parse(&exception.expires_at)
            .is_some_and(|t| t > at)
            .then_some(exception)
    }

    pub fn put_exception(&self, exception: &Exception) -> Result<()> {
        self.put(
            &self
                .root
                .join("exceptions")
                .join(&exception.use_case)
                .join(format!("{}.json", exception.sha256)),
            &serde_json::to_vec_pretty(exception)?,
        )
    }

    /// Rewrites one person's index without lines older than `since`; removes the scope once it holds nothing.
    fn compact(&self, scope: &Path, since: OffsetDateTime) -> Result<u64> {
        let _ledger = self.ledger();
        let path = scope.join("index.jsonl");
        let text = std::fs::read_to_string(&path).unwrap_or_default();
        let total = text.lines().count();
        let kept: Vec<&str> = text
            .lines()
            .filter(|line| {
                serde_json::from_str::<IndexEntry>(line)
                    .ok()
                    .and_then(|e| parse(&e.at))
                    .is_some_and(|t| t > since)
            })
            .collect();
        let dropped = (total - kept.len()) as u64;
        if kept.is_empty() {
            self.remove(&path);
        } else if dropped > 0 {
            let mut body = kept.join("\n");
            body.push('\n');
            // A compaction only shrinks the file, and the ledger lock is held already.
            self.put_reserved(&path, body.as_bytes())?;
        }
        let objects = scope.join("objects");
        let _ = std::fs::remove_dir(&objects);
        let _ = std::fs::remove_dir(scope);
        Ok(dropped)
    }

    /// Deletes originals, disarmed copies, records and exceptions past their dates, index lines past their use, and
    /// work directories left by a crash.
    pub fn sweep(&self, at: OffsetDateTime) -> Result<Swept> {
        let mut swept = Swept::default();
        for day in subdirs(&self.root.join("records")) {
            let Ok(files) = std::fs::read_dir(&day) else {
                continue;
            };
            for file in files.filter_map(|e| e.ok()) {
                let path = file.path();
                let Ok(bytes) = std::fs::read(&path) else {
                    continue;
                };
                let Ok(record) = serde_json::from_slice::<Record>(&bytes) else {
                    continue;
                };
                let original = self
                    .scope(&record.tenant, &record.person)
                    .join("objects")
                    .join(&record.intake_id);
                if parse(&record.retention.original_until).is_some_and(|t| t <= at)
                    && self.remove(&original)
                {
                    swept.originals += 1;
                }
                let copy = self.root.join("disarmed").join(&record.intake_id);
                let expired = record
                    .disarmed
                    .as_ref()
                    .is_none_or(|d| parse(&d.expires_at).is_none_or(|t| t <= at));
                if expired && self.remove(&copy) {
                    swept.disarmed += 1;
                }
                if parse(&record.retention.record_until).is_some_and(|t| t <= at)
                    && self.remove(&path)
                {
                    swept.records += 1;
                }
            }
            let _ = std::fs::remove_dir(&day);
        }
        for use_case in subdirs(&self.root.join("exceptions")) {
            let Ok(files) = std::fs::read_dir(&use_case) else {
                continue;
            };
            for file in files.filter_map(|e| e.ok()) {
                let path = file.path();
                let live = std::fs::read(&path)
                    .ok()
                    .and_then(|b| serde_json::from_slice::<Exception>(&b).ok())
                    .and_then(|e| parse(&e.expires_at))
                    .is_some_and(|t| t > at);
                if !live && self.remove(&path) {
                    swept.exceptions += 1;
                }
            }
            let _ = std::fs::remove_dir(&use_case);
        }
        let since = at - time::Duration::days(INDEX_DAYS);
        for tenant in subdirs(&self.root.join("scopes")) {
            for scope in subdirs(&tenant) {
                swept.index_entries += self.compact(&scope, since)?;
            }
            let _ = std::fs::remove_dir(&tenant);
        }
        for entry in std::fs::read_dir(self.root.join("work"))?.filter_map(|e| e.ok()) {
            let old = entry
                .metadata()
                .ok()
                .and_then(|m| m.modified().ok())
                .is_some_and(|m| m.elapsed().is_ok_and(|e| e.as_secs() > 3600));
            if entry.file_type().is_ok_and(|t| t.is_dir())
                && old
                && std::fs::remove_dir_all(entry.path()).is_ok()
            {
                swept.work += 1;
            }
        }
        Ok(swept)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn quota(files: u64, bytes: u64) -> Quota {
        Quota {
            files_per_day: files,
            bytes_per_day: bytes,
        }
    }

    fn record(id: &str, at: OffsetDateTime, outcome: Outcome) -> Record {
        Record {
            schema: RECORD_SCHEMA.into(),
            intake_id: id.into(),
            received_at: stamp(at),
            completed_at: stamp(at),
            caller: "workspace".into(),
            tenant: "lab-tenant".into(),
            lab: "lab".into(),
            project: None,
            person: person_digest("lab-tenant", "person-1"),
            use_case: "library-paper".into(),
            file_name: None,
            bytes: 10,
            sha256: sha256(b"0123456789"),
            declared_type: FileType::Pdf,
            detected_type: FileType::Pdf,
            policy_version: "p".into(),
            versions: Versions::default(),
            findings: vec![],
            outcome,
            refusal: None,
            disarmed: Some(Disarmed {
                sha256: sha256(b"rebuilt"),
                bytes: 7,
                media_type: "application/pdf".into(),
                expires_at: stamp(at + time::Duration::hours(1)),
            }),
            exception: None,
            retention: Retention {
                original_until: stamp(at + time::Duration::days(30)),
                record_until: stamp(at + time::Duration::days(400)),
            },
            usage: Usage {
                files_24h: 0,
                bytes_24h: 0,
            },
            retries: vec![],
            retry_count: 0,
        }
    }

    #[test]
    fn stores_finds_counts_and_sweeps_every_class() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path(), 1 << 30).unwrap();
        let at = now();
        let id = Uuid::now_v7().to_string();
        let r = record(&id, at, Outcome::Allowed);
        let held = store
            .reserve("lab-tenant", &r.person, at, 10, &quota(5, 1000))
            .unwrap();
        store
            .commit(&held, &r, Some(b"0123456789"), Some(b"rebuilt"))
            .unwrap();
        drop(held);
        assert_eq!(
            store.used(),
            dir_bytes(&dir.path().join("scopes"))
                + dir_bytes(&dir.path().join("records"))
                + dir_bytes(&dir.path().join("disarmed")),
            "every written byte is counted"
        );
        assert_eq!(store.read_record(&id).unwrap().unwrap(), r);
        assert_eq!(store.read_disarmed(&r, at).unwrap().unwrap(), b"rebuilt");
        assert!(
            store
                .read_disarmed(&r, at + time::Duration::hours(2))
                .unwrap()
                .is_none()
        );
        let found = store.recent(
            "lab-tenant",
            &r.person,
            "library-paper",
            &r.sha256,
            time::Duration::hours(1),
            at,
        );
        assert_eq!(found.unwrap().intake_id, id);
        assert_eq!(
            store.usage("lab-tenant", &r.person, at),
            Usage {
                files_24h: 1,
                bytes_24h: 10
            }
        );
        let mode = std::fs::metadata(dir.path().join("disarmed").join(&id))
            .unwrap()
            .permissions();
        assert_eq!(
            std::os::unix::fs::PermissionsExt::mode(&mode) & 0o777,
            0o600
        );
        assert_eq!(
            store.sweep(at + time::Duration::hours(2)).unwrap(),
            Swept {
                disarmed: 1,
                ..Default::default()
            }
        );
        assert_eq!(
            store.sweep(at + time::Duration::days(31)).unwrap(),
            Swept {
                originals: 1,
                index_entries: 1,
                ..Default::default()
            }
        );
        assert!(
            !dir.path().join("scopes").join("lab-tenant").exists(),
            "an empty scope is removed"
        );
        assert_eq!(
            store.sweep(at + time::Duration::days(401)).unwrap(),
            Swept {
                records: 1,
                ..Default::default()
            }
        );
        assert!(store.read_record(&id).unwrap().is_none());
        assert_eq!(store.used(), 0);
    }

    #[test]
    fn reservations_hold_quota_and_room_until_released() {
        let dir = tempfile::tempdir().unwrap();
        let at = now();
        let store = Store::open(dir.path(), 1 << 30).unwrap();
        let person = person_digest("t", "p");
        let first = store
            .reserve("t", &person, at, 100, &quota(2, 1000))
            .unwrap();
        let second = store
            .reserve("t", &person, at, 100, &quota(2, 1000))
            .unwrap();
        assert_eq!(
            store.reserve("t", &person, at, 100, &quota(2, 1000)).err(),
            Some(Refusal::Quota),
            "two in flight fill a quota of two"
        );
        drop(second);
        assert!(
            store
                .reserve("t", &person, at, 100, &quota(2, 1000))
                .is_ok()
        );
        assert_eq!(
            store.reserve("t", &person, at, 2000, &quota(9, 1000)).err(),
            Some(Refusal::Quota),
            "bytes count too"
        );
        let other = person_digest("t", "q");
        assert_eq!(
            store
                .reserve("t", &other, at, 100_000_000, &quota(9, 1 << 30))
                .err(),
            Some(Refusal::Full),
            "the room for the original, its largest copy, the record and the work directory must fit"
        );
        drop(first);
        assert_eq!(store.ledger().reserved, 0);
        assert!(store.ledger().people.is_empty());
    }

    #[test]
    fn exceptions_expire_and_are_swept() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path(), 1 << 30).unwrap();
        let at = now();
        let e = Exception {
            sha256: sha256(b"x"),
            use_case: "library-paper".into(),
            approved_by: "curator".into(),
            reason: "a paper about attacks".into(),
            expires_at: stamp(at + time::Duration::days(7)),
            recorded_at: stamp(at),
        };
        store.put_exception(&e).unwrap();
        assert!(store.used() > 0);
        assert_eq!(
            store.exception("library-paper", &e.sha256, at),
            Some(e.clone())
        );
        assert!(
            store
                .exception("library-paper", &e.sha256, at + time::Duration::days(8))
                .is_none()
        );
        assert!(store.exception("other", &e.sha256, at).is_none());
        assert_eq!(store.sweep(at).unwrap().exceptions, 0);
        assert_eq!(
            store
                .sweep(at + time::Duration::days(8))
                .unwrap()
                .exceptions,
            1
        );
        assert_eq!(store.used(), 0);
    }

    #[test]
    fn writes_outside_a_reservation_respect_the_cap_and_work_cleans_up() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path(), 4096).unwrap();
        let at = now();
        let exception = |n: u8| Exception {
            sha256: sha256(&[n]),
            use_case: "library-paper".into(),
            approved_by: "curator".into(),
            reason: "x".repeat(400),
            expires_at: stamp(at + time::Duration::days(1)),
            recorded_at: stamp(at),
        };
        let mut written = 0;
        let mut refused = None;
        for n in 0..20 {
            match store.put_exception(&exception(n)) {
                Ok(()) => written += 1,
                Err(e) => {
                    refused = Some(e);
                    break;
                }
            }
        }
        assert!(written > 0 && store.used() <= 4096, "{written} written");
        assert!(refused.unwrap().downcast_ref::<StoreFull>().is_some());
        let work = store.work_dir(&Uuid::now_v7().to_string()).unwrap();
        let path = work.path().to_path_buf();
        std::fs::write(path.join("normalized.pdf"), b"x").unwrap();
        drop(work);
        assert!(!path.exists(), "a dropped work directory is removed");
    }

    #[test]
    fn retries_keep_a_bounded_list_and_a_count() {
        let at = now();
        let mut r = record(&Uuid::now_v7().to_string(), at, Outcome::Allowed);
        for i in 0..40 {
            r.note_retry(at + time::Duration::seconds(i));
        }
        assert_eq!((r.retries.len(), r.retry_count), (RETRIES_KEPT, 40));
        assert_eq!(
            r.retries.last().unwrap(),
            &stamp(at + time::Duration::seconds(39))
        );
    }

    #[test]
    fn person_digests_separate_tenants() {
        assert_ne!(person_digest("a", "p"), person_digest("b", "p"));
        assert_eq!(person_digest("a", "p").len(), 32);
    }

    #[test]
    fn the_rebuilt_limit_leaves_room_for_small_files() {
        assert_eq!(rebuilt_limit(10), 10 + 64 * 1024);
        assert_eq!(rebuilt_limit(1 << 20), 2 << 20);
    }
}
