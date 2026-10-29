//! Durable, private publication intent journal shared by trusted publishers.
//!
//! The caller supplies only typed metadata. This module reserves one safe file
//! name before a side effect, fsyncs every JSONL event, and leaves incomplete
//! journals for recovery rather than allowing a retry to replace a scope.

use anyhow::{Result, anyhow, ensure};
use serde_json::{Value, json};
use std::path::Path;
use time::OffsetDateTime;
use tokio::io::AsyncWriteExt;

const EVENT_LIMIT: usize = 4096;

/// A private, exclusively reserved JSONL journal. It has no public serializer
/// so callers cannot accidentally expose its metadata as a response payload.
pub(crate) struct PublicationJournal {
    file: tokio::fs::File,
    sequence: u32,
}

impl PublicationJournal {
    /// Reserve one server-selected journal path and sync its parent before any
    /// publication intent. Existing scopes always require recovery.
    pub(crate) async fn create(root: &Path, name: &str, writer_uid: u32) -> Result<Self> {
        let root = root.to_path_buf();
        let name = name.to_owned();
        let file = tokio::task::spawn_blocking(move || create_file(&root, &name, writer_uid))
            .await
            .map_err(|_| anyhow!("publication_journal_unavailable"))??;
        Ok(Self {
            file: tokio::fs::File::from_std(file),
            sequence: 0,
        })
    }

    /// Append one bounded typed event and make it durable before the next
    /// external action. Incomplete journals remain recoverable evidence.
    pub(crate) async fn append(&mut self, mut event: Value) -> Result<()> {
        self.sequence = self
            .sequence
            .checked_add(1)
            .ok_or_else(|| anyhow!("publication_journal_sequence_exhausted"))?;
        event["sequence"] = json!(self.sequence);
        event["observedAt"] = json!(OffsetDateTime::now_utc().unix_timestamp());
        let mut bytes =
            serde_json::to_vec(&event).map_err(|_| anyhow!("publication_journal_invalid"))?;
        ensure!(
            bytes.len() <= EVENT_LIMIT,
            "publication_journal_event_limit"
        );
        bytes.push(b'\n');
        self.file
            .write_all(&bytes)
            .await
            .map_err(|_| anyhow!("publication_journal_write_failed"))?;
        self.file
            .sync_all()
            .await
            .map_err(|_| anyhow!("publication_journal_sync_failed"))
    }
}

pub(crate) fn valid_private_root(path: &Path) -> bool {
    use std::path::Component;
    path.is_absolute()
        && path != Path::new("/")
        && path
            .components()
            .all(|part| matches!(part, Component::RootDir | Component::Normal(_)))
}

fn safe_name(name: &str) -> bool {
    name.len() <= 128
        && name
            .bytes()
            .next()
            .is_some_and(|byte| byte.is_ascii_alphanumeric())
        && name
            .bytes()
            .last()
            .is_some_and(|byte| byte.is_ascii_alphanumeric())
        && name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'-' | b'_'))
}

#[cfg(unix)]
fn create_file(root: &Path, name: &str, uid: u32) -> Result<std::fs::File> {
    use std::{
        ffi::CString,
        fs::{File, OpenOptions},
        os::{
            fd::{AsRawFd, FromRawFd},
            unix::{
                ffi::OsStrExt,
                fs::{MetadataExt, OpenOptionsExt},
            },
        },
        path::Component,
    };

    ensure!(valid_private_root(root), "publication_journal_root_invalid");
    ensure!(safe_name(name), "publication_journal_name_invalid");
    let mut directory = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open("/")
        .map_err(|_| anyhow!("publication_journal_unavailable"))?;
    for part in root.components() {
        let Component::Normal(part) = part else {
            continue;
        };
        let part = CString::new(part.as_bytes())
            .map_err(|_| anyhow!("publication_journal_root_invalid"))?;
        let fd = unsafe {
            libc::openat(
                directory.as_raw_fd(),
                part.as_ptr(),
                libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC,
            )
        };
        ensure!(fd >= 0, "publication_journal_unavailable");
        directory = unsafe { File::from_raw_fd(fd) };
    }
    let metadata = directory
        .metadata()
        .map_err(|_| anyhow!("publication_journal_unavailable"))?;
    ensure!(
        metadata.uid() == uid && metadata.mode() & 0o077 == 0 && unsafe { libc::geteuid() } == uid,
        "publication_journal_not_private"
    );
    let name = CString::new(name).map_err(|_| anyhow!("publication_journal_name_invalid"))?;
    let fd = unsafe {
        libc::openat(
            directory.as_raw_fd(),
            name.as_ptr(),
            libc::O_WRONLY | libc::O_CREAT | libc::O_EXCL | libc::O_NOFOLLOW | libc::O_CLOEXEC,
            0o600,
        )
    };
    if fd < 0 {
        return Err(anyhow!(if std::io::Error::last_os_error().kind()
            == std::io::ErrorKind::AlreadyExists
        {
            "publication_journal_exists_requires_recovery"
        } else {
            "publication_journal_unavailable"
        }));
    }
    let file = unsafe { File::from_raw_fd(fd) };
    // Retain even an empty journal if this sync fails: its scope is reserved and
    // recovery, not a second publisher, determines the next action.
    directory
        .sync_all()
        .map_err(|_| anyhow!("publication_journal_sync_failed"))?;
    Ok(file)
}

#[cfg(not(unix))]
fn create_file(_: &Path, _: &str, _: u32) -> Result<std::fs::File> {
    Err(anyhow!("publication_journal_requires_unix"))
}
