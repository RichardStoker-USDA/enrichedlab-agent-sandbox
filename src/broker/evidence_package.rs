//! Retained package reading and the project evidence package.
//!
//! A retained reviewed-tool result is one checksum-bound tar+gzip file. The API reads
//! its members so a scientist can see the original figures, tables, code and logs
//! without downloading the whole bundle. Every member is hashed from the verified
//! archive bytes; the package's own `SHA256SUMS`, when present, is compared rather
//! than trusted.
//!
//! The project evidence package is a deterministic tar+gzip built from one database
//! snapshot and the original artifact bytes. The first pass measures its size and
//! SHA-256; the second pass streams identical bytes to the caller. An artifact whose
//! bytes no longer match its record is excluded and named in the manifest instead of
//! being copied.
use std::{
    cell::Cell,
    collections::{BTreeMap, BTreeSet, HashMap, VecDeque},
    fs::{File, OpenOptions},
    io::{self, Read, Seek, SeekFrom, Write},
    os::unix::fs::OpenOptionsExt,
    path::PathBuf,
    rc::Rc,
    sync::Arc,
};

use flate2::{Compression, GzBuilder, read::GzDecoder};
use serde::Serialize;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

use super::package_registry::{
    PackageRevision, changes_since, manifest_contents, package_name, package_snapshot,
};

/// Reading bounds for a retained package. A larger package stays downloadable.
pub const MAX_PACKAGE_MEMBERS: usize = 5_000;
pub const MAX_PACKAGE_EXPANDED_BYTES: u64 = 2 * 1024 * 1024 * 1024;
const MAX_MEMBER_PATH: usize = 512;
const MAX_CHECKSUM_FILE: u64 = 1024 * 1024;
/// Bounds on what an archive can make a reader hold or do beyond its members' bytes: entries of every kind, a GNU long
/// name or link (a path plus its closing NUL), a PAX extended header, and the decompressed stream with its headers,
/// padding and skipped bodies.
pub const MAX_PACKAGE_ENTRIES: usize = 4 * MAX_PACKAGE_MEMBERS;
const MAX_LONG_NAME: u64 = 4096 + 1;
const MAX_PAX_HEADER: u64 = 64 * 1024;
const MAX_PACKAGE_STREAM_BYTES: u64 = MAX_PACKAGE_EXPANDED_BYTES + 256 * 1024 * 1024;

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ManifestCheck {
    /// The package's own checksum list names this file with the same SHA-256.
    Matched,
    /// The checksum list names this file with a different SHA-256.
    Mismatched,
    /// The package has a checksum list that does not name this file.
    Unlisted,
    /// The package has no checksum list, or this is the list itself.
    NotApplicable,
}

#[derive(Clone, Debug, Serialize)]
pub struct PackageMember {
    pub path: String,
    pub size_bytes: u64,
    pub sha256: String,
    pub media_type: &'static str,
    pub kind: &'static str,
    pub manifest: ManifestCheck,
    /// The result kind the member's bytes hold, when the listing was asked to check kinds.
    #[serde(flatten)]
    pub result: Option<super::kinds::MemberKind>,
}

#[derive(Clone, Debug, Serialize)]
pub struct SkippedMember {
    pub path: String,
    pub reason: &'static str,
}

#[derive(Clone, Debug, Serialize)]
pub struct PackageListing {
    pub format: &'static str,
    pub checksum_file: Option<String>,
    pub members: Vec<PackageMember>,
    /// Paths the checksum list names that are absent from the archive.
    pub missing_from_archive: Vec<String>,
    /// Links, devices and unsafe paths are never offered for reading.
    pub skipped: Vec<SkippedMember>,
    pub expanded_bytes: u64,
    /// The routine and registry that checked the members' kinds, when the listing checked them.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub kind_check: Option<serde_json::Value>,
}

impl PackageListing {
    pub fn member(&self, path: &str) -> Option<&PackageMember> {
        self.members.iter().find(|m| m.path == path)
    }

    /// Gives each checked member that a declaration names, by path and SHA-256, the declared kind when the registry
    /// allows it. Returns how many members took a declared kind.
    pub fn declare(
        &mut self,
        registry: &super::kinds::Registry,
        declarations: &[super::kinds::Declaration],
    ) -> usize {
        let mut taken = 0;
        for member in &mut self.members {
            let Some(result) = member.result.as_mut() else {
                continue;
            };
            if declarations.iter().any(|declared| {
                declared.path == member.path
                    && declared.sha256 == member.sha256
                    && result.declare(registry, declared.kind, member.media_type)
            }) {
                taken += 1;
            }
        }
        taken
    }
}

#[derive(Debug, PartialEq, Eq)]
pub enum PackageError {
    NotAPackage,
    TooLarge,
    Integrity,
    Io,
}

impl PackageError {
    pub fn code(&self) -> &'static str {
        match self {
            Self::NotAPackage => "artifact_not_a_package",
            Self::TooLarge => "package_too_large_to_inspect",
            Self::Integrity => "artifact_integrity_failed",
            Self::Io => "artifact_unavailable",
        }
    }
}

/// Accept only a clean relative path. `./` prefixes from common tar tools are removed.
pub fn normalize_member_path(raw: &[u8]) -> Option<String> {
    let text = std::str::from_utf8(raw).ok()?;
    let mut text = text;
    while let Some(rest) = text.strip_prefix("./") {
        text = rest;
    }
    let text = text.trim_end_matches('/');
    if text.is_empty() || text.len() > MAX_MEMBER_PATH || text.starts_with('/') {
        return None;
    }
    let mut parts = Vec::new();
    for part in text.split('/') {
        if part.is_empty()
            || part == "."
            || part == ".."
            || part.contains('\\')
            || part.chars().any(char::is_control)
        {
            return None;
        }
        parts.push(part);
    }
    Some(parts.join("/"))
}

pub fn media_type_for(path: &str) -> &'static str {
    let name = path.rsplit('/').next().unwrap_or(path).to_ascii_lowercase();
    let extension = name.rsplit_once('.').map(|(_, e)| e).unwrap_or("");
    match extension {
        "csv" => "text/csv",
        "tsv" | "tab" => "text/tab-separated-values",
        "pdf" => "application/pdf",
        "svg" => "image/svg+xml",
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "webp" => "image/webp",
        "gif" => "image/gif",
        "json" => "application/json",
        "jsonl" | "ndjson" => "application/x-ndjson",
        "md" => "text/markdown",
        "r" | "py" | "sql" | "sh" | "txt" | "log" | "yaml" | "yml" | "toml" => "text/plain",
        "zip" => "application/zip",
        "gz" | "tgz" => "application/gzip",
        _ if name == "sha256sums" || name.ends_with(".sha256") => "text/plain",
        _ => "application/octet-stream",
    }
}

/// A scientist-facing grouping. It chooses a reader; it never changes the bytes.
pub fn content_kind(path: &str) -> &'static str {
    let lower = path.to_ascii_lowercase();
    let name = lower.rsplit('/').next().unwrap_or(&lower);
    let extension = name.rsplit_once('.').map(|(_, e)| e).unwrap_or("");
    match extension {
        "pdf" | "svg" | "png" | "jpg" | "jpeg" | "webp" | "gif" => "figure",
        "csv" | "tsv" | "tab" => "table",
        "r" | "py" | "sql" | "sh" => "code",
        "log" => "log",
        "json" | "jsonl" | "ndjson" | "yaml" | "yml" | "toml" => "record",
        "md" | "txt" => "text",
        _ if name == "sha256sums" || name.ends_with(".sha256") => "checksum",
        _ => "data",
    }
}

fn open_regular(path: &PathBuf, expected_size: u64) -> Result<File, PackageError> {
    let file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW)
        .open(path)
        .map_err(|_| PackageError::Io)?;
    let metadata = file.metadata().map_err(|_| PackageError::Io)?;
    if !metadata.is_file() || metadata.len() != expected_size {
        return Err(PackageError::Integrity);
    }
    Ok(file)
}

/// Hash a whole file on one descriptor and hand that descriptor back, rewound. A file replaced after the check is never
/// the one read, since the descriptor still names the bytes that were checked.
fn verified_file(path: &PathBuf, size: u64, sha256: &str) -> Result<File, PackageError> {
    verified_file_then(path, size, sha256, || {})
}

/// `verified_file`, running `between` after the hash and before the descriptor is handed back.
fn verified_file_then(
    path: &PathBuf,
    size: u64,
    sha256: &str,
    between: impl FnOnce(),
) -> Result<File, PackageError> {
    let mut file = open_regular(path, size)?;
    let mut digest = Sha256::new();
    let mut buffer = vec![0_u8; 64 * 1024];
    let mut total = 0_u64;
    loop {
        let read = file.read(&mut buffer).map_err(|_| PackageError::Io)?;
        if read == 0 {
            break;
        }
        total += read as u64;
        digest.update(&buffer[..read]);
    }
    if total != size || hex::encode(digest.finalize()) != sha256 {
        return Err(PackageError::Integrity);
    }
    between();
    file.seek(SeekFrom::Start(0))
        .map_err(|_| PackageError::Io)?;
    Ok(file)
}

/// A gzip stream that gives at most `limit` decompressed bytes in all.
pub struct Expanded<R> {
    inner: GzDecoder<R>,
    read: Rc<Cell<u64>>,
    limit: u64,
}

impl<R: Read> Read for Expanded<R> {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        let read = self.inner.read(buffer)?;
        self.read.set(self.read.get() + read as u64);
        if self.read.get() > self.limit {
            return Err(io::Error::other("package_too_large"));
        }
        Ok(read)
    }
}

/// A tar+gzip package whose entries `members` reads.
pub struct Package<R: Read> {
    archive: tar::Archive<Expanded<R>>,
    read: Rc<Cell<u64>>,
    limit: u64,
}

impl<R: Read> Package<R> {
    pub fn new(reader: R) -> Self {
        Self::with_limit(reader, MAX_PACKAGE_STREAM_BYTES)
    }

    /// A package whose decompressed stream, headers, extension bodies, padding and skipped bodies included, may run to
    /// at most `budget` bytes (and never past the usual bound); reading past it fails as too large.
    pub(crate) fn within(reader: R, budget: u64) -> Self {
        Self::with_limit(reader, budget.min(MAX_PACKAGE_STREAM_BYTES))
    }

    fn with_limit(reader: R, limit: u64) -> Self {
        let read = Rc::new(Cell::new(0));
        let inner = GzDecoder::new(reader);
        Package {
            archive: tar::Archive::new(Expanded {
                inner,
                read: read.clone(),
                limit,
            }),
            read,
            limit,
        }
    }

    pub fn members(&mut self) -> Result<Members<'_, R>, PackageError> {
        let entries = self
            .archive
            .entries()
            .map_err(|_| PackageError::NotAPackage)?;
        Ok(Members {
            entries: entries.raw(true),
            read: self.read.clone(),
            limit: self.limit,
            seen: 0,
        })
    }
}

/// An entry of a package and its path as stored.
pub type Member<'a, R> = (Vec<u8>, tar::Entry<'a, Expanded<R>>);

/// The entries of a package with their paths. The tar crate reads a GNU long name or a PAX extended header whole into
/// memory, whatever its size, so entries are read raw and those are read here within fixed bounds. A GNU long name wins
/// over a PAX path, as in the tar crate. A PAX header that sets an entry's size, or a sparse entry with extension
/// headers, refuses the package: the packages the broker reads hold neither, and a raw reader cannot frame the entries
/// after them.
pub struct Members<'a, R: Read> {
    entries: tar::Entries<'a, Expanded<R>>,
    read: Rc<Cell<u64>>,
    limit: u64,
    seen: usize,
}

impl<'a, R: Read> Members<'a, R> {
    /// Why a read of the package failed: too large past the stream bound, otherwise not a readable package.
    pub fn failure(&self) -> PackageError {
        if self.read.get() > self.limit {
            PackageError::TooLarge
        } else {
            PackageError::NotAPackage
        }
    }

    /// How many bytes of the decompressed stream have been read so far.
    pub(crate) fn expanded(&self) -> u64 {
        self.read.get()
    }

    /// The next entry with its path as stored, before any normalization.
    pub fn next_member(&mut self) -> Result<Option<Member<'a, R>>, PackageError> {
        let (mut long_name, mut long_link, mut pax) = (None, false, None::<Option<Vec<u8>>>);
        loop {
            let Some(entry) = self.entries.next() else {
                // A long name or PAX header with no entry after it describes nothing.
                return if long_name.is_some() || long_link || pax.is_some() {
                    Err(PackageError::NotAPackage)
                } else {
                    Ok(None)
                };
            };
            let mut entry = entry.map_err(|_| self.failure())?;
            self.seen += 1;
            if self.seen > MAX_PACKAGE_ENTRIES {
                return Err(PackageError::TooLarge);
            }
            let header = entry.header();
            let kind = header.entry_type();
            let recognized = header.as_gnu().is_some() || header.as_ustar().is_some();
            let limit = if !recognized {
                None
            } else if kind.is_gnu_longname() || kind.is_gnu_longlink() {
                Some(MAX_LONG_NAME)
            } else if kind.is_pax_local_extensions() {
                Some(MAX_PAX_HEADER)
            } else {
                None
            };
            if let Some(limit) = limit {
                if entry.size() > limit {
                    return Err(PackageError::NotAPackage);
                }
                let mut body = Vec::with_capacity(entry.size() as usize);
                entry.read_to_end(&mut body).map_err(|_| self.failure())?;
                // Two of one kind for the same entry are refused, as the tar crate refuses them.
                if kind.is_gnu_longname() {
                    if body.last() == Some(&0) {
                        body.pop();
                    }
                    if long_name.replace(body).is_some() {
                        return Err(PackageError::NotAPackage);
                    }
                } else if kind.is_gnu_longlink() {
                    if std::mem::replace(&mut long_link, true) {
                        return Err(PackageError::NotAPackage);
                    }
                } else {
                    let mut path = None;
                    for extension in tar::PaxExtensions::new(&body).filter_map(Result::ok) {
                        match extension.key_bytes() {
                            b"path" => path = Some(extension.value_bytes().to_vec()),
                            b"size" => return Err(PackageError::NotAPackage),
                            _ => {}
                        }
                    }
                    if pax.replace(path).is_some() {
                        return Err(PackageError::NotAPackage);
                    }
                }
                continue;
            }
            if kind.is_gnu_sparse() && header.as_gnu().is_some_and(|gnu| gnu.is_extended()) {
                return Err(PackageError::NotAPackage);
            }
            let path = match long_name.or(pax.flatten()) {
                Some(path) => path,
                None => entry.path_bytes().into_owned(),
            };
            return Ok(Some((path, entry)));
        }
    }
}

fn parse_checksums(text: &str) -> BTreeMap<String, String> {
    let mut out = BTreeMap::new();
    for line in text.lines() {
        let line = line.trim_end();
        // A digest is 64 ASCII hex digits, so checking those bytes first keeps the split between characters.
        if line.len() < 66 || !line.as_bytes()[..64].iter().all(u8::is_ascii_hexdigit) {
            continue;
        }
        let (hash, rest) = line.split_at(64);
        let name = rest
            .strip_prefix("  ")
            .or_else(|| rest.strip_prefix(" *"))
            .or_else(|| rest.strip_prefix(' '));
        if let Some(path) = name.and_then(|n| normalize_member_path(n.as_bytes())) {
            out.insert(path, hash.to_ascii_lowercase());
        }
    }
    out
}

/// Read every regular member of a verified tar+gzip artifact.
/// Lists a verified package. With a kind registry, each member's bytes are also checked against the kind its media type
/// names, in the same pass that hashes them.
pub fn list_package(
    path: &PathBuf,
    size: u64,
    sha256: &str,
    kinds: Option<&super::kinds::Registry>,
) -> Result<PackageListing, PackageError> {
    let mut file = verified_file(path, size, sha256)?;
    let mut magic = [0_u8; 2];
    if file.read_exact(&mut magic).is_err() || magic != [0x1f, 0x8b] {
        return Err(PackageError::NotAPackage);
    }
    file.seek(SeekFrom::Start(0))
        .map_err(|_| PackageError::Io)?;
    let mut package = Package::new(file);
    let mut entries = package.members()?;
    let mut members = Vec::new();
    let mut skipped = Vec::new();
    let mut checksum_text: Option<String> = None;
    let mut expanded = 0_u64;
    let mut buffer = vec![0_u8; 64 * 1024];
    while let Some((raw_path, mut entry)) = entries.next_member()? {
        let kind = entry.header().entry_type();
        if kind.is_dir() {
            continue;
        }
        // At most four bytes a character, so no more than this is converted to show the path.
        let shown = &raw_path[..raw_path.len().min(4 * MAX_MEMBER_PATH)];
        let display = String::from_utf8_lossy(shown)
            .chars()
            .take(MAX_MEMBER_PATH)
            .collect::<String>();
        let Some(clean) = normalize_member_path(&raw_path) else {
            skipped.push(SkippedMember {
                path: display,
                reason: "unsafe_path",
            });
            continue;
        };
        if !kind.is_file() {
            skipped.push(SkippedMember {
                path: clean,
                reason: "not_a_regular_file",
            });
            continue;
        }
        if members.len() >= MAX_PACKAGE_MEMBERS {
            return Err(PackageError::TooLarge);
        }
        let mut digest = Sha256::new();
        let mut member_size = 0_u64;
        let keep_text = clean == "SHA256SUMS";
        let media_type = media_type_for(&clean);
        let mut check = kinds.map(|registry| {
            super::kinds::Check::new(registry.recognize(media_type), media_type, true)
        });
        let mut text = Vec::new();
        loop {
            let read = entry.read(&mut buffer).map_err(|_| entries.failure())?;
            if read == 0 {
                break;
            }
            member_size += read as u64;
            expanded += read as u64;
            if expanded > MAX_PACKAGE_EXPANDED_BYTES {
                return Err(PackageError::TooLarge);
            }
            digest.update(&buffer[..read]);
            if let Some(check) = check.as_mut() {
                check.update(&buffer[..read]);
            }
            if keep_text && member_size <= MAX_CHECKSUM_FILE {
                text.extend_from_slice(&buffer[..read]);
            }
        }
        if members.iter().any(|m: &PackageMember| m.path == clean) {
            skipped.push(SkippedMember {
                path: clean,
                reason: "duplicate_path",
            });
            continue;
        }
        if keep_text && member_size <= MAX_CHECKSUM_FILE {
            checksum_text = String::from_utf8(text).ok();
        }
        let result = kinds.zip(check).map(|(registry, check)| {
            let checked = check.kind();
            super::kinds::member_kind(registry, checked, check.finish())
        });
        members.push(PackageMember {
            media_type,
            kind: content_kind(&clean),
            path: clean,
            size_bytes: member_size,
            sha256: hex::encode(digest.finalize()),
            manifest: ManifestCheck::NotApplicable,
            result,
        });
    }
    let listed = checksum_text.as_deref().map(parse_checksums);
    let mut missing_from_archive = Vec::new();
    if let Some(listed) = &listed {
        for member in &mut members {
            if member.path == "SHA256SUMS" {
                continue;
            }
            member.manifest = match listed.get(&member.path) {
                Some(hash) if *hash == member.sha256 => ManifestCheck::Matched,
                Some(_) => ManifestCheck::Mismatched,
                None => ManifestCheck::Unlisted,
            };
        }
        for path in listed.keys() {
            if !members.iter().any(|m| &m.path == path) {
                missing_from_archive.push(path.clone());
            }
        }
    }
    members.sort_by(|a, b| a.path.cmp(&b.path));
    Ok(PackageListing {
        format: "tar+gzip",
        checksum_file: listed.map(|_| "SHA256SUMS".to_owned()),
        members,
        missing_from_archive,
        skipped,
        expanded_bytes: expanded,
        kind_check: kinds.map(super::kinds::listing_check),
    })
}

/// Check a whole package file against its recorded size and SHA-256 and return the descriptor that was checked, to
/// read from. A range read calls this first: the client cannot check part of a member against the member's digest, so
/// the server checks the package before it reads.
pub fn verify_archive(path: &PathBuf, size: u64, sha256: &str) -> Result<File, PackageError> {
    verified_file(path, size, sha256)
}

/// Open a package file of the recorded size without checking its digest, for a whole member that `stream_member`
/// checks as it reads.
pub fn open_package(path: &PathBuf, size: u64) -> Result<File, PackageError> {
    open_regular(path, size)
}

/// One member's bytes in memory, after the package file and then the member match their digests. For callers that
/// parse a member (a table page or summary) and so must not start from unchecked bytes.
pub fn read_member(
    path: &PathBuf,
    size: u64,
    sha256: &str,
    member: &PackageMember,
    max_bytes: u64,
) -> Result<Vec<u8>, PackageError> {
    if member.size_bytes > max_bytes {
        return Err(PackageError::TooLarge);
    }
    let file = verify_archive(path, size, sha256)?;
    let mut bytes = Vec::with_capacity(member.size_bytes as usize);
    stream_member(file, member, None, |chunk| {
        bytes.extend_from_slice(&chunk);
        true
    })?;
    Ok(bytes)
}

/// Stream one member's bytes, or the inclusive byte range `range` of them. The whole member is read and hashed
/// either way; a mismatch ends the stream with an error so a client never receives a silent substitute.
pub fn stream_member(
    file: File,
    member: &PackageMember,
    range: Option<(u64, u64)>,
    mut send: impl FnMut(Vec<u8>) -> bool,
) -> Result<(), PackageError> {
    let mut package = Package::new(file);
    let mut entries = package.members()?;
    while let Some((raw_path, mut entry)) = entries.next_member()? {
        if !entry.header().entry_type().is_file() {
            continue;
        }
        if normalize_member_path(&raw_path).as_deref() != Some(member.path.as_str()) {
            continue;
        }
        let mut digest = Sha256::new();
        let mut sent = 0_u64;
        loop {
            let mut buffer = vec![0_u8; 64 * 1024];
            let read = entry
                .read(&mut buffer)
                .map_err(|_| PackageError::Integrity)?;
            if read == 0 {
                break;
            }
            buffer.truncate(read);
            let chunk_start = sent;
            sent += read as u64;
            if sent > member.size_bytes {
                return Err(PackageError::Integrity);
            }
            digest.update(&buffer);
            let part = match range {
                None => buffer,
                Some((first, last)) => {
                    let from = first.max(chunk_start);
                    let to = (last + 1).min(sent);
                    if from >= to {
                        continue;
                    }
                    buffer[(from - chunk_start) as usize..(to - chunk_start) as usize].to_vec()
                }
            };
            if !send(part) {
                return Err(PackageError::Io);
            }
        }
        if sent != member.size_bytes || hex::encode(digest.finalize()) != member.sha256 {
            return Err(PackageError::Integrity);
        }
        return Ok(());
    }
    Err(PackageError::Integrity)
}

/// Small bounded cache of listings keyed by the verified archive checksum.
#[derive(Default)]
pub struct ListingCache {
    order: VecDeque<String>,
    entries: HashMap<String, Arc<PackageListing>>,
}

impl ListingCache {
    const CAPACITY: usize = 48;
    pub fn get(&mut self, sha256: &str) -> Option<Arc<PackageListing>> {
        self.entries.get(sha256).cloned()
    }
    pub fn insert(&mut self, sha256: String, listing: Arc<PackageListing>) {
        if self.entries.insert(sha256.clone(), listing).is_none() {
            self.order.push_back(sha256);
            while self.order.len() > Self::CAPACITY {
                if let Some(old) = self.order.pop_front() {
                    self.entries.remove(&old);
                }
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Project evidence package
// ---------------------------------------------------------------------------

/// The rerun kit: a standard-library Python verifier and a Slurm script, the same bytes in every package.
const RERUN_VERIFY: &str = include_str!("repro/verify.py");
const RERUN_SBATCH: &str = include_str!("repro/rerun.sbatch");
/// The R image a rerun was tested with: the same R version (4.6.1) the analyses ran with.
const RERUN_R_IMAGE: &str = "docker.io/library/r-base@sha256:4e6fb1eebb29725c9c524851fb9a4bd2bd152254bcc3778cb44fd1c3d7fe9d1b";
/// SCINet's Atlas and Ceres as SCINet's own guides describe them, with the date they were checked: the
/// facts the hand-off steps need, in one file, so the package README and the workspace show the same steps.
const HPC_SITES: &str = include_str!("repro/hpc_sites.json");

fn hpc_step(text: String, command: Option<String>) -> Value {
    match command {
        Some(command) => json!({"text": text, "command": command}),
        None => json!({"text": text}),
    }
}

/// How to run this package's rerun kit on each SCINet site, step by step: sign in through Open OnDemand, upload,
/// unpack, check, submit, follow the job and bring the report back. Code writes every step from the site table and the
/// package's folder name, so the steps never come from a model and the same package always gets the same words.
pub fn hpc_handoff(root: &str) -> Value {
    let table: Value = serde_json::from_str(HPC_SITES).unwrap_or(Value::Null);
    let text = |value: &Value| value.as_str().unwrap_or_default().to_owned();
    let number = |key: &str| table[key].as_u64().unwrap_or_default();
    let sites: Vec<Value> = table["sites"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|site| {
            let name = text(&site["name"]);
            let account = if site["account_required"].as_bool() == Some(true) {
                format!(" On {name} the account is required.")
            } else {
                String::new()
            };
            let steps = vec![
                hpc_step(
                    format!(
                        "Sign in to {name} Open OnDemand at {} with your SCINet account.",
                        text(&site["open_ondemand"])
                    ),
                    None,
                ),
                hpc_step(
                    format!(
                        "Open the Files menu, go to your project's folder, /project/YOUR_PROJECT, and upload \
{root}.tar.gz there. Your home folder holds only {} GB, and files in /90daydata are deleted after {} days. Open \
OnDemand uploads files under {} GB; for a larger package use Globus ({}, organization {}, collection {}).",
                        number("home_quota_gb"),
                        number("short_term_days"),
                        number("upload_limit_gb"),
                        text(&table["globus"]),
                        text(&table["globus_organization"]),
                        text(&site["globus_collection"])
                    ),
                    None,
                ),
                hpc_step(
                    format!(
                        "Open a shell on {name} from the Clusters menu and unpack the package in that folder. tar \
stops rather than replace a file that is already there, so move an earlier copy of this folder away first; the folder \
must stay writable, since the check and the rerun write beside the retained files."
                    ),
                    Some(format!(
                        "cd /project/YOUR_PROJECT && tar --keep-old-files -xzf {root}.tar.gz && cd {root}"
                    )),
                ),
                hpc_step(
                    "Check every file against the package's checksums. This needs Python 3.8 or later: if python3 \
--version shows an older one, load a newer Python first (module spider python lists them)."
                        .into(),
                    Some("python3 rerun/verify.py".into()),
                ),
                hpc_step(
                    format!(
                        "Submit the rerun with your Slurm account, which is your project folder's name, or sandbox \
if you have no project.{account} The first time, the job pulls the R image these analyses ran with, by its digest, \
into the package folder. It reruns an analysis whose code the research agent wrote only when a person on the project \
has accepted it; the report names any it skipped."
                    ),
                    Some("sbatch -A YOUR_PROJECT rerun/slurm/rerun.sbatch".into()),
                ),
                hpc_step(
                    "Follow the job. When it has finished, seff JOB_ID shows what it used; scancel JOB_ID stops \
it early."
                        .into(),
                    Some("squeue -u $USER".into()),
                ),
                hpc_step(
                    "When it ends, download extracted-JOB_ID/rerun_report.json and rerun-JOB_ID.log from the Files \
menu; each job writes to a folder of its own. The report says for each analysis and table whether the rerun matched, \
in counts and never cell values. Keep it with the package or give it to your reviewer."
                        .into(),
                    None,
                ),
            ];
            json!({"key": site["key"], "name": name, "open_ondemand": site["open_ondemand"], "steps": steps})
        })
        .collect();
    json!({"checked": table["checked"], "sites": sites, "sources": table["sources"]})
}

/// The original bytes of one registered artifact, resolved inside the artifact root.
#[derive(Clone, Debug)]
pub struct ArtifactSource {
    pub task_id: String,
    pub artifact_id: String,
    pub relative_path: String,
    pub media_type: String,
    pub label: String,
    pub size_bytes: u64,
    pub sha256: String,
    /// `None` when the path was unsafe or absent when the plan was made.
    pub path: Option<PathBuf>,
    /// Set when the bytes were found but no longer match the record.
    pub mismatch: bool,
}

/// Resolve artifact availability before a content revision is allocated. This
/// makes a missing or changed retained file part of the package identity.
pub fn verify_artifact_sources(artifacts: &mut [ArtifactSource]) {
    for artifact in artifacts {
        let Some(path) = artifact.path.as_ref() else {
            continue;
        };
        match verified_file(path, artifact.size_bytes, &artifact.sha256) {
            Ok(_) => {}
            Err(PackageError::Integrity) => {
                artifact.path = None;
                artifact.mismatch = true;
            }
            Err(_) => {
                artifact.path = None;
                artifact.mismatch = false;
            }
        }
    }
}

#[derive(Clone, Debug)]
enum EntrySource {
    Bytes(Arc<Vec<u8>>),
    Artifact(ArtifactSource),
}

#[derive(Clone, Debug)]
struct Entry {
    path: String,
    size: u64,
    sha256: String,
    source: EntrySource,
}

#[derive(Clone, Debug, Serialize)]
pub struct MissingArtifact {
    pub task_id: String,
    pub artifact_id: String,
    pub relative_path: String,
    pub label: String,
    pub reason: &'static str,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, PartialOrd, Ord)]
struct DataSourceFact {
    title: String,
    release: String,
    download: Option<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, PartialOrd, Ord)]
struct RenderServiceFact {
    version: String,
    image_digest: Option<String>,
}

#[derive(Clone, Debug)]
struct ResultFileFact {
    artifact_id: String,
    path: String,
    sha256: String,
}

#[derive(Clone, Debug, Default)]
struct AnalysisFact {
    method: Option<String>,
    source_task: Option<String>,
    source: Option<DataSourceFact>,
    rows_in: Option<u64>,
    result_files: Vec<ResultFileFact>,
}

#[derive(Clone, Debug, Default)]
struct PackageFacts {
    analyses: BTreeMap<String, AnalysisFact>,
    sources: BTreeSet<DataSourceFact>,
    render_services: BTreeSet<RenderServiceFact>,
}

fn first_download(snapshot: &Value) -> Option<String> {
    fn direct(value: &Value) -> Option<String> {
        for key in ["download", "download_url", "url"] {
            if let Some(download) = value[key]
                .as_str()
                .filter(|text| !text.is_empty())
                .map(str::to_owned)
            {
                return Some(download);
            }
        }
        ["download", "downloads"]
            .into_iter()
            .filter_map(|key| value[key].as_object().map(|_| &value[key]))
            .find_map(|nested| {
                ["url", "download_url"].into_iter().find_map(|key| {
                    nested[key]
                        .as_str()
                        .filter(|text| !text.is_empty())
                        .map(str::to_owned)
                })
            })
    }
    if let Some(download) = direct(snapshot) {
        return Some(download);
    }
    for key in ["downloads", "files"] {
        for item in snapshot[key].as_array().into_iter().flatten() {
            if let Some(download) = item
                .as_str()
                .filter(|text| !text.is_empty())
                .map(str::to_owned)
                .or_else(|| direct(item))
            {
                return Some(download);
            }
        }
    }
    None
}

fn registered_result_files(task: &Value) -> Vec<ResultFileFact> {
    task["artifacts"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|artifact| artifact["role"].as_str() == Some("result"))
        .filter_map(|artifact| {
            Some(ResultFileFact {
                artifact_id: artifact["artifact_id"].as_str()?.to_owned(),
                path: artifact["relative_path"].as_str()?.to_owned(),
                sha256: artifact["sha256"].as_str()?.to_owned(),
            })
        })
        .collect()
}

fn current_receipt<'a>(
    task: &Value,
    detail: &'a Value,
    result_files: &[ResultFileFact],
) -> Option<&'a Value> {
    let attempt = task["attempt"].as_i64()?;
    detail["receipts"].as_array()?.iter().rev().find(|receipt| {
        receipt["attempt"].as_i64() == Some(attempt)
            && receipt["status"].as_str() == Some("completed")
            && receipt["package_sha256"]
                .as_str()
                .is_some_and(|sha256| result_files.iter().any(|file| file.sha256 == sha256))
    })
}

/// Facts shown to a scientist come only from broker-owned task records. Analysis archives remain package payloads;
/// their self-described receipts, specifications and run files never supply citation or summary claims.
fn package_facts(snapshot: &Value) -> PackageFacts {
    let mut facts = PackageFacts::default();
    for task in snapshot["tasks"].as_array().into_iter().flatten() {
        let Some(task_id) = task["task_id"].as_str() else {
            continue;
        };
        let result_files = registered_result_files(task);
        let analysis = facts.analyses.entry(task_id.to_owned()).or_default();
        analysis.result_files = result_files.clone();

        let query = &task["query_detail"];
        if query["task_id"].as_str() == Some(task_id)
            && let Some(receipt) = current_receipt(task, query, &result_files)
        {
            analysis.method = query["profile"]
                .as_str()
                .filter(|text| !text.is_empty())
                .map(str::to_owned);
            analysis.rows_in = receipt["row_count"].as_u64();
            let source =
                query["title"]
                    .as_str()
                    .zip(query["release"].as_str())
                    .map(|(title, release)| DataSourceFact {
                        title: title.to_owned(),
                        release: release.to_owned(),
                        download: first_download(&query["snapshot"]),
                    });
            if let Some(source) = source {
                facts.sources.insert(source.clone());
                analysis.source = Some(source);
            }
        }

        let detail = &task["analysis_detail"];
        if detail["task_id"].as_str() == Some(task_id)
            && let Some(receipt) = current_receipt(task, detail, &result_files)
        {
            analysis.method = receipt["method"]
                .as_str()
                .or_else(|| detail["feature"].as_str())
                .filter(|text| !text.is_empty())
                .map(str::to_owned);
            analysis.source_task = detail["source_task"].as_str().map(str::to_owned);
            analysis.rows_in = detail["table_rows"].as_u64();
            if let Some(version) = receipt["service_version"]
                .as_str()
                .filter(|text| !text.is_empty())
            {
                facts.render_services.insert(RenderServiceFact {
                    version: version.to_owned(),
                    image_digest: receipt["image_digest"].as_str().map(str::to_owned),
                });
            }
        }
    }
    for generated in snapshot["generated_code"].as_array().into_iter().flatten() {
        let Some((task_id, template)) = generated["task_id"]
            .as_str()
            .zip(generated["template_ref"].as_str())
        else {
            continue;
        };
        facts
            .analyses
            .entry(task_id.to_owned())
            .or_default()
            .method
            .get_or_insert_with(|| template.to_owned());
    }
    facts
}

/// Everything needed to write the package twice with identical bytes.
#[derive(Clone, Debug)]
pub struct PackagePlan {
    pub root: String,
    pub mtime: u64,
    pub snapshot_at: String,
    pub manifest: Value,
    pub manifest_sha256: String,
    entries: Vec<Entry>,
    pub missing: Vec<MissingArtifact>,
}

#[derive(Clone, Debug, Serialize)]
pub struct PackageSummary {
    pub bytes: u64,
    pub sha256: String,
    pub files: usize,
}

fn json_bytes(value: &Value) -> Vec<u8> {
    let mut bytes = serde_json::to_vec_pretty(value).expect("serializable package record");
    bytes.push(b'\n');
    bytes
}

fn sha_hex(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}

fn human_bytes(bytes: u64) -> String {
    match bytes {
        b if b < 1024 => format!("{b} bytes"),
        b if b < 1024 * 1024 => format!("{:.1} KB", b as f64 / 1024.0),
        b if b < 1024 * 1024 * 1024 => format!("{:.1} MB", b as f64 / 1024.0 / 1024.0),
        b => format!("{:.2} GB", b as f64 / 1024.0 / 1024.0 / 1024.0),
    }
}

fn yaml_string(text: &str) -> String {
    serde_json::to_string(text).expect("a string is serializable")
}

/// A display name may leave the broker only when it reads like a person's name rather than an account or locator.
fn package_person_name(name: Option<&str>) -> Option<String> {
    let name = name?;
    if name.chars().any(char::is_control) {
        return None;
    }
    let name = name.trim();
    if name.is_empty() || name.contains('@') || uuid::Uuid::parse_str(name).is_ok() {
        return None;
    }
    let lower = name.to_ascii_lowercase();
    if ["principal:", "service:", "system:"]
        .iter()
        .any(|prefix| lower.contains(prefix))
        || lower.contains("://")
        || lower.contains("www.")
    {
        return None;
    }
    let mut digits = 0;
    let mut hex = 0;
    for byte in name.bytes() {
        digits = if byte.is_ascii_digit() { digits + 1 } else { 0 };
        hex = if byte.is_ascii_hexdigit() { hex + 1 } else { 0 };
        if digits >= 16 || hex >= 16 {
            return None;
        }
    }
    Some(name.to_owned())
}

fn citation_name(value: &Value) -> Option<String> {
    let name = value
        .as_str()
        .or_else(|| value["display_name"].as_str())
        .or_else(|| value["name"].as_str());
    package_person_name(name)
}

fn project_authors(project: &Value) -> Vec<String> {
    let mut authors = Vec::new();
    if let Some(owner) = citation_name(&project["owner"]).or_else(|| {
        project["owner_name"]
            .as_str()
            .and_then(|name| package_person_name(Some(name)))
    }) {
        authors.push(owner);
    }
    for member in project["members"].as_array().into_iter().flatten() {
        if member["kind"].as_str() == Some("service_account") {
            continue;
        }
        if let Some(name) = citation_name(member) {
            authors.push(name);
        }
    }
    if authors.is_empty() {
        let group = project["group_name"]
            .as_str()
            .filter(|name| !name.trim().is_empty())
            .or_else(|| project["group_key"].as_str())
            .expect("package name checked the research group");
        authors.push(group.to_owned());
    }
    authors
}

fn citation_text(
    project: &Value,
    question: &str,
    released: &str,
    revision: u64,
    facts: &PackageFacts,
) -> String {
    let title = format!(
        "{} results package, revision {revision}",
        project["title"]
            .as_str()
            .expect("package name checked the title")
    );
    let mut out = String::new();
    out.push_str(&format!("cff-version: {}\n", yaml_string("1.2.0")));
    out.push_str(&format!(
        "message: {}\n",
        yaml_string("If you use these results, cite this package and the data sources it names.")
    ));
    out.push_str(&format!("type: {}\n", yaml_string("dataset")));
    out.push_str(&format!("title: {}\n", yaml_string(&title)));
    out.push_str(&format!(
        "version: {}\n",
        yaml_string(&revision.to_string())
    ));
    out.push_str(&format!("date-released: {}\n", yaml_string(released)));
    out.push_str("authors:\n");
    for author in project_authors(project) {
        out.push_str(&format!("  - name: {}\n", yaml_string(&author)));
    }
    if !question.is_empty() {
        out.push_str(&format!("abstract: {}\n", yaml_string(question)));
    }
    out.push_str("references:\n");
    for source in &facts.sources {
        out.push_str(&format!(
            "  - type: {}\n    title: {}\n    version: {}\n",
            yaml_string("dataset"),
            yaml_string(&source.title),
            yaml_string(&source.release)
        ));
        if let Some(download) = &source.download {
            out.push_str(&format!("    url: {}\n", yaml_string(download)));
        }
    }
    for service in &facts.render_services {
        out.push_str(&format!(
            "  - type: {}\n    title: {}\n    version: {}\n",
            yaml_string("software"),
            yaml_string("render-service"),
            yaml_string(&service.version)
        ));
        if let Some(digest) = &service.image_digest {
            out.push_str(&format!(
                "    notes: {}\n",
                yaml_string(&format!("Image digest: {digest}"))
            ));
        }
    }
    out.push_str(&format!(
        "  - type: {}\n    title: {}\n    version: {}\n",
        yaml_string("software"),
        yaml_string("enrichedlab-agent-broker"),
        yaml_string(env!("CARGO_PKG_VERSION"))
    ));
    out
}

fn analysis_title(task: &Value, plan: &Value) -> String {
    let task_id = task["task_id"].as_str().unwrap_or_default();
    plan.as_array()
        .into_iter()
        .flatten()
        .find(|step| step["task_id"].as_str() == Some(task_id))
        .and_then(|step| step["title"].as_str())
        .or_else(|| task["objective"].as_str())
        .unwrap_or(task_id)
        .to_owned()
}

fn task_source(task_id: &str, facts: &PackageFacts) -> Option<DataSourceFact> {
    let mut current = task_id;
    let mut seen = BTreeSet::new();
    while seen.insert(current) {
        if let Some(source) = facts
            .analyses
            .get(current)
            .and_then(|fact| fact.source.clone())
        {
            return Some(source);
        }
        current = facts
            .analyses
            .get(current)
            .and_then(|fact| fact.source_task.as_deref())?;
    }
    None
}

fn markdown_cell(text: &str) -> String {
    text.replace(['\r', '\n'], " ").replace('|', "\\|")
}

fn review_summary(task_id: &str, reviews: &[Value]) -> String {
    let newest = reviews
        .iter()
        .filter(|review| review["task_id"].as_str() == Some(task_id))
        .filter(|review| review["reviewer_kind"].as_str() == Some("person"))
        .max_by_key(|review| review["created_at"].as_str().unwrap_or_default());
    let Some(review) = newest else {
        return "No person review".into();
    };
    let decision = review["decision"].as_str().unwrap_or("reviewed");
    let reviewer =
        package_person_name(review["reviewer_name"].as_str()).unwrap_or_else(|| "a person".into());
    if review["covers_current_outputs"].as_bool() == Some(true) {
        format!("Yes: {reviewer} ({decision})")
    } else {
        format!("reviewed an earlier version; not the current results: {reviewer} ({decision})")
    }
}

fn replace_unsafe_name(value: &mut Value) {
    let Some(name) = value.as_str() else {
        return;
    };
    *value =
        Value::String(package_person_name(Some(name)).unwrap_or_else(|| "a person".to_owned()));
}

fn sanitize_package_names(snapshot: &mut Value) {
    if let Some(project) = snapshot.get_mut("project") {
        if let Some(owner) = project.get_mut("owner") {
            if owner.is_string() {
                replace_unsafe_name(owner);
            } else if let Some(object) = owner.as_object_mut() {
                if let Some(name) = object.get_mut("display_name") {
                    replace_unsafe_name(name);
                }
                if let Some(name) = object.get_mut("name") {
                    replace_unsafe_name(name);
                }
            }
        }
        if let Some(name) = project.get_mut("owner_name") {
            replace_unsafe_name(name);
        }
        for member in project
            .get_mut("members")
            .and_then(Value::as_array_mut)
            .into_iter()
            .flatten()
        {
            if member.is_string() {
                replace_unsafe_name(member);
            } else if let Some(object) = member.as_object_mut() {
                if let Some(name) = object.get_mut("display_name") {
                    replace_unsafe_name(name);
                }
                if let Some(name) = object.get_mut("name") {
                    replace_unsafe_name(name);
                }
            }
        }
    }
    for review in snapshot
        .get_mut("reviews")
        .and_then(Value::as_array_mut)
        .into_iter()
        .flatten()
    {
        if let Some(name) = review.get_mut("reviewer_name") {
            replace_unsafe_name(name);
        }
    }
}

struct SummaryContext<'a> {
    title: &'a str,
    question: &'a str,
    tasks: &'a [&'a Value],
    latest_plan: &'a Value,
    reviews: &'a [Value],
    facts: &'a PackageFacts,
    missing: &'a [MissingArtifact],
    reasoning_included: bool,
}

fn summary_text(context: SummaryContext<'_>) -> String {
    let mut out = format!("# {}: results summary\n\n", context.title);
    out.push_str("## Question\n\n");
    out.push_str(if context.question.is_empty() {
        "No research question was recorded.\n\n"
    } else {
        context.question
    });
    if !context.question.is_empty() {
        out.push_str("\n\n");
    }
    out.push_str("## Analyses\n\n");
    out.push_str("| Analysis | Method | Data source and release | Rows in | Status | Person review | Result files |\n");
    out.push_str("|---|---|---|---:|---|---|---|\n");
    for task in context.tasks {
        let task_id = task["task_id"].as_str().unwrap_or_default();
        let fact = context.facts.analyses.get(task_id);
        let method = fact
            .and_then(|fact| fact.method.as_deref())
            .unwrap_or("Not recorded");
        let source = task_source(task_id, context.facts)
            .map(|source| format!("{} ({})", source.title, source.release))
            .unwrap_or_else(|| "Not recorded".into());
        let rows = fact
            .and_then(|fact| fact.rows_in)
            .map(|rows| rows.to_string())
            .unwrap_or_else(|| "Not recorded".into());
        let status = task["status"].as_str().unwrap_or("Not recorded");
        let result_files = fact
            .filter(|fact| !fact.result_files.is_empty())
            .map(|fact| {
                fact.result_files
                    .iter()
                    .map(|file| format!("`{}` (artifact `{}`)", file.path, file.artifact_id))
                    .collect::<Vec<_>>()
                    .join("<br>")
            })
            .unwrap_or_else(|| "None included".into());
        out.push_str(&format!(
            "| {} | {} | {} | {} | {} | {} | {} |\n",
            markdown_cell(&analysis_title(task, context.latest_plan)),
            markdown_cell(method),
            markdown_cell(&source),
            rows,
            markdown_cell(status),
            markdown_cell(&review_summary(task_id, context.reviews)),
            markdown_cell(&result_files),
        ));
    }
    out.push_str("\n## What was not included\n\n");
    if context.missing.is_empty() {
        out.push_str("Every registered retained result file was included.\n");
    } else {
        for item in context.missing {
            let reason = match item.reason {
                "retained_bytes_do_not_match_record" => {
                    "the retained bytes did not match the recorded checksum"
                }
                _ => "the retained bytes were unavailable",
            };
            out.push_str(&format!(
                "- `{}` ({}) from analysis `{}` was not included: {}.\n",
                item.relative_path, item.label, item.task_id, reason
            ));
        }
    }
    if !context.reasoning_included {
        out.push_str("Exact agent reasoning was not included for this package.\n");
    }
    out.push_str(
        "\n## Rerun and check\n\nFollow [`rerun/README.md`](rerun/README.md) to check and rerun the analyses. Check every retained file against [`SHA256SUMS`](SHA256SUMS).\n\n",
    );
    out.push_str("## Cite\n\nUse [`CITATION.cff`](CITATION.cff).\n");
    out
}

/// The package-relative location of an artifact. A gzip file without an extension
/// gains `.tar.gz` so it opens on a scientist's computer; the manifest keeps the
/// original path.
fn artifact_entry_path(task_id: &str, relative_path: &str, media_type: &str) -> Option<String> {
    let prefix = format!("tasks/{task_id}/");
    let rest = relative_path.strip_prefix(&prefix)?;
    let rest = normalize_member_path(rest.as_bytes())?;
    let name = rest.rsplit('/').next().unwrap_or(&rest);
    let extended = if media_type.starts_with("application/gzip") && !name.contains('.') {
        format!("{rest}.tar.gz")
    } else {
        rest
    };
    Some(format!("tasks/{task_id}/files/{extended}"))
}

/// Build the deterministic entry list and manifest from one snapshot.
pub fn plan_package(
    snapshot: &Value,
    artifacts: Vec<ArtifactSource>,
    package_revision: &PackageRevision,
) -> Result<PackagePlan, &'static str> {
    let mut snapshot = package_snapshot(snapshot, package_revision)?;
    let citation_project = snapshot["project"].clone();
    sanitize_package_names(&mut snapshot);
    let project = &snapshot["project"];
    let title = project["title"].as_str().ok_or("snapshot_invalid")?;
    let snapshot_at = snapshot["snapshot_at"]
        .as_str()
        .ok_or("snapshot_invalid")?
        .to_owned();
    let parsed =
        time::OffsetDateTime::parse(&snapshot_at, &time::format_description::well_known::Rfc3339)
            .map_err(|_| "snapshot_invalid")?;
    let mtime = parsed.unix_timestamp().max(0) as u64;
    let root = package_name(project, package_revision)?;
    let facts = package_facts(&snapshot);

    let mut entries: Vec<Entry> = Vec::new();
    let add_json = |entries: &mut Vec<Entry>, path: String, value: &Value| {
        let bytes = json_bytes(value);
        entries.push(Entry {
            path,
            size: bytes.len() as u64,
            sha256: sha_hex(&bytes),
            source: EntrySource::Bytes(Arc::new(bytes)),
        });
    };

    add_json(
        &mut entries,
        "project/project.json".into(),
        &json!({
            "project": project, "snapshot_at": snapshot_at,
        }),
    );
    let checkpoints = snapshot["checkpoints"]
        .as_array()
        .ok_or("snapshot_invalid")?;
    for checkpoint in checkpoints {
        let revision = checkpoint["revision"].as_i64().ok_or("snapshot_invalid")?;
        add_json(
            &mut entries,
            format!("project/plan/revision-{revision:04}.json"),
            checkpoint,
        );
    }
    let access = &snapshot["access_history"];
    add_json(&mut entries, "project/access_history.json".into(), access);
    let runs = snapshot["research_runs"]
        .as_array()
        .ok_or("snapshot_invalid")?;
    let reasoning = snapshot["reasoning"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    let digests = snapshot["digests"].as_array().cloned().unwrap_or_default();
    let task_origins = snapshot["task_origins"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    let mut turn_total = 0;
    let mut deferral_total = 0;
    for run in runs {
        let run_id = run["run"]["run_id"].as_str().ok_or("snapshot_invalid")?;
        add_json(&mut entries, format!("runs/{run_id}/run.json"), &run["run"]);
        add_json(
            &mut entries,
            format!("runs/{run_id}/deferrals.json"),
            &run["events"],
        );
        let turns = run["turns"].as_array().ok_or("snapshot_invalid")?;
        for turn_record in turns {
            let turn = turn_record["turn"].as_i64().ok_or("snapshot_invalid")?;
            add_json(
                &mut entries,
                format!("runs/{run_id}/turns/{turn:04}/record.json"),
                turn_record,
            );
            add_json(
                &mut entries,
                format!("runs/{run_id}/turns/{turn:04}/literature_ledger.json"),
                &json!({
                    "literature": turn_record["record"]["context_inputs"]["literature"],
                    "claims": turn_record["record"]["literature_claims"],
                }),
            );
        }
        for review in snapshot["literature_reviews"]
            .as_array()
            .into_iter()
            .flatten()
            .filter(|v| v["run_id"].as_str() == Some(run_id))
        {
            let digest = review["inputs_sha256"]
                .as_str()
                .and_then(|s| s.strip_prefix("sha256:"))
                .filter(|s| s.len() == 64 && s.bytes().all(|b| b.is_ascii_hexdigit()))
                .ok_or("snapshot_invalid")?;
            add_json(
                &mut entries,
                format!("runs/{run_id}/literature_reviews/{digest}.json"),
                &review["record"],
            );
            if review["record"]["status"] == "refused" {
                continue;
            }
            let markdown =
                super::literature_review::markdown(&review["record"]["review"]).into_bytes();
            entries.push(Entry {
                path: format!("runs/{run_id}/literature_reviews/{digest}.md"),
                size: markdown.len() as u64,
                sha256: sha_hex(&markdown),
                source: EntrySource::Bytes(Arc::new(markdown)),
            });
        }
        let run_reasoning: Vec<Value> = reasoning
            .iter()
            .filter(|item| item["run_id"].as_str() == Some(run_id))
            .cloned()
            .collect();
        for item in &run_reasoning {
            let turn = item["turn"].as_i64().ok_or("snapshot_invalid")?;
            let role = item["role"].as_str().ok_or("snapshot_invalid")?;
            if role.is_empty()
                || role.len() > 48
                || !role
                    .bytes()
                    .all(|byte| byte.is_ascii_lowercase() || byte == b'_')
            {
                return Err("snapshot_invalid");
            }
            let digest = item["sha256"]
                .as_str()
                .and_then(|value| value.strip_prefix("sha256:"))
                .filter(|value| value.len() == 64)
                .ok_or("snapshot_invalid")?;
            let text = item["text"].as_str().ok_or("snapshot_invalid")?;
            if sha_hex(text.as_bytes()) != digest {
                return Err("snapshot_invalid");
            }
            entries.push(Entry {
                path: format!("runs/{run_id}/reasoning/turn-{turn:04}-{role}-{digest}.txt"),
                size: text.len() as u64,
                sha256: digest.to_owned(),
                source: EntrySource::Bytes(Arc::new(text.as_bytes().to_vec())),
            });
        }
        let run_digests: Vec<Value> = digests
            .iter()
            .filter(|item| item["run_id"].as_str() == Some(run_id))
            .cloned()
            .collect();
        add_json(
            &mut entries,
            format!("runs/{run_id}/digests.json"),
            &Value::Array(run_digests),
        );
        let run_origins: Vec<Value> = task_origins
            .iter()
            .filter(|item| item["run_id"].as_str() == Some(run_id))
            .cloned()
            .collect();
        add_json(
            &mut entries,
            format!("runs/{run_id}/tasks.json"),
            &Value::Array(run_origins),
        );
        add_json(
            &mut entries,
            format!("runs/{run_id}/index.json"),
            &json!({
                "run_id": run_id,
                "turns": turns.iter().filter_map(|turn| turn["turn"].as_i64()).collect::<Vec<_>>(),
                "reasoning_records": run_reasoning.len(),
            }),
        );
        turn_total += run["turns"].as_array().map_or(0, Vec::len);
        deferral_total += run["events"].as_array().map_or(0, Vec::len);
    }
    let papers = snapshot["papers"].as_array().cloned().unwrap_or_default();
    for paper in &papers {
        let paper_id = paper["paper_id"].as_str().ok_or("snapshot_invalid")?;
        if normalize_member_path(paper_id.as_bytes()).as_deref() != Some(paper_id) {
            return Err("snapshot_invalid");
        }
        add_json(
            &mut entries,
            format!("project/papers/{paper_id}.json"),
            paper,
        );
    }
    let tasks = snapshot["tasks"].as_array().ok_or("snapshot_invalid")?;
    for task in tasks {
        let task_id = task["task_id"].as_str().ok_or("snapshot_invalid")?;
        let mut record = task.clone();
        if let Some(object) = record.as_object_mut() {
            object.remove("events");
            object.remove("artifacts");
            object.remove("recoveries");
        }
        add_json(&mut entries, format!("tasks/{task_id}/task.json"), &record);
        add_json(
            &mut entries,
            format!("tasks/{task_id}/events.json"),
            &task["events"],
        );
        add_json(
            &mut entries,
            format!("tasks/{task_id}/artifacts.json"),
            &task["artifacts"],
        );
        if task["recoveries"].as_array().is_some_and(|r| !r.is_empty()) {
            add_json(
                &mut entries,
                format!("tasks/{task_id}/recoveries.json"),
                &task["recoveries"],
            );
        }
    }

    // Model-written blocks: the exact bytes that were sealed as the run's input, and
    // their admission record. The snapshot's digest must match the bytes.
    let generated = snapshot["generated_code"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    for code in &generated {
        let task_id = code["task_id"].as_str().ok_or("snapshot_invalid")?;
        let bytes = code["content"]
            .as_str()
            .ok_or("snapshot_invalid")?
            .as_bytes()
            .to_vec();
        let sha256 = sha_hex(&bytes);
        if code["sha256"].as_str() != Some(format!("sha256:{sha256}").as_str()) {
            return Err("snapshot_invalid");
        }
        let mut record = code.clone();
        if let Some(object) = record.as_object_mut() {
            object.remove("content");
        }
        add_json(
            &mut entries,
            format!("tasks/{task_id}/generated_code/record.json"),
            &record,
        );
        entries.push(Entry {
            path: format!("tasks/{task_id}/generated_code/block.R"),
            size: bytes.len() as u64,
            sha256,
            source: EntrySource::Bytes(Arc::new(bytes)),
        });
    }

    // Reviews, oldest first for each analysis. A service-account row remains labelled as such; every display name is
    // already safe for the package view.
    let reviews = snapshot["reviews"].as_array().cloned().unwrap_or_default();
    let person_review_count = reviews
        .iter()
        .filter(|review| review["reviewer_kind"].as_str() == Some("person"))
        .count();
    let mut reviewed: BTreeMap<&str, Vec<Value>> = BTreeMap::new();
    for review in &reviews {
        let task_id = review["task_id"].as_str().ok_or("snapshot_invalid")?;
        reviewed.entry(task_id).or_default().push(review.clone());
    }
    for (task_id, list) in reviewed {
        add_json(
            &mut entries,
            format!("tasks/{task_id}/reviews.json"),
            &Value::Array(list),
        );
    }

    let mut missing = Vec::new();
    let mut included_bytes = 0_u64;
    let mut file_records = Vec::new();
    let mut packages: Vec<(String, String, String)> = Vec::new();
    for artifact in artifacts {
        let target = artifact_entry_path(
            &artifact.task_id,
            &artifact.relative_path,
            &artifact.media_type,
        );
        match (&artifact.path, target) {
            (Some(_), Some(target)) => {
                included_bytes += artifact.size_bytes;
                if artifact.media_type.starts_with("application/gzip") {
                    packages.push((
                        artifact.task_id.clone(),
                        target.clone(),
                        artifact.sha256.clone(),
                    ));
                }
                file_records.push(json!({"path": target, "task_id": artifact.task_id, "artifact_id": artifact.artifact_id,
                    "original_relative_path": artifact.relative_path, "label": artifact.label, "media_type": artifact.media_type}));
                entries.push(Entry {
                    path: target,
                    size: artifact.size_bytes,
                    sha256: artifact.sha256.clone(),
                    source: EntrySource::Artifact(artifact),
                });
            }
            (None, _) | (_, None) => missing.push(MissingArtifact {
                reason: if artifact.mismatch {
                    "retained_bytes_do_not_match_record"
                } else {
                    "retained_bytes_unavailable"
                },
                task_id: artifact.task_id,
                artifact_id: artifact.artifact_id,
                relative_path: artifact.relative_path,
                label: artifact.label,
            }),
        }
    }
    // The rerun kit: every retained analysis package, oldest analysis first, with what the snapshot
    // says about it; the verifier unpacks, checks and reruns them.
    let latest_plan = checkpoints
        .last()
        .map(|c| c["state"]["plan"].clone())
        .unwrap_or(Value::Null);
    let mut ordered: Vec<&Value> = tasks.iter().collect();
    ordered.sort_by(|a, b| {
        a["submitted_at"]
            .as_str()
            .cmp(&b["submitted_at"].as_str())
            .then(a["task_id"].as_str().cmp(&b["task_id"].as_str()))
    });
    let mut analyses = Vec::new();
    for task in &ordered {
        let task_id = task["task_id"].as_str().unwrap_or_default();
        let mut own: Vec<&(String, String, String)> =
            packages.iter().filter(|p| p.0 == task_id).collect();
        own.sort_by(|a, b| a.1.cmp(&b.1));
        let title = latest_plan
            .as_array()
            .into_iter()
            .flatten()
            .find(|step| step["task_id"].as_str() == Some(task_id))
            .and_then(|step| step["title"].as_str())
            .map(str::to_owned);
        let settings = generated
            .iter()
            .find(|c| c["task_id"].as_str() == Some(task_id))
            .and_then(|c| c.get("configuration").cloned());
        let task_reviews: Vec<Value> = reviews
            .iter()
            .filter(|r| r["task_id"].as_str() == Some(task_id))
            .map(|r| json!({"decision": r["decision"], "reviewer": r["reviewer_name"], "reviewer_kind": r["reviewer_kind"],
                "recorded_at": r["created_at"], "note": r["note"]}))
            .collect();
        for (_, path, sha256) in own {
            analyses.push(
                json!({"task_id": task_id, "title": title, "method": task["profile"],
                "objective": task["objective"], "status": task["status"], "settings": settings,
                "package": path, "package_sha256": sha256, "reviews": task_reviews}),
            );
        }
    }
    add_json(
        &mut entries,
        "rerun/analyses.json".into(),
        &json!({"schema": "enrichedlab.rerun-analyses.v1", "r_image": RERUN_R_IMAGE, "analyses": analyses}),
    );
    for (path, text) in [
        ("rerun/verify.py", RERUN_VERIFY.to_owned()),
        ("rerun/slurm/rerun.sbatch", RERUN_SBATCH.to_owned()),
        ("rerun/README.md", rerun_readme(&analyses)),
    ] {
        let bytes = text.into_bytes();
        entries.push(Entry {
            path: path.into(),
            size: bytes.len() as u64,
            sha256: sha_hex(&bytes),
            source: EntrySource::Bytes(Arc::new(bytes)),
        });
    }
    let latest_goal = checkpoints
        .last()
        .and_then(|c| c["state"]["goal"].as_str())
        .unwrap_or("");
    let citation = citation_text(
        &citation_project,
        latest_goal,
        &parsed.date().to_string(),
        package_revision.revision,
        &facts,
    )
    .into_bytes();
    entries.push(Entry {
        path: "CITATION.cff".into(),
        size: citation.len() as u64,
        sha256: sha_hex(&citation),
        source: EntrySource::Bytes(Arc::new(citation)),
    });
    let summary = summary_text(SummaryContext {
        title,
        question: latest_goal,
        tasks: &ordered,
        latest_plan: &latest_plan,
        reviews: &reviews,
        facts: &facts,
        missing: &missing,
        reasoning_included: snapshot["reasoning_access"].as_bool() == Some(true),
    })
    .into_bytes();
    entries.push(Entry {
        path: "SUMMARY.md".into(),
        size: summary.len() as u64,
        sha256: sha_hex(&summary),
        source: EntrySource::Bytes(Arc::new(summary)),
    });
    entries.sort_by(|a, b| a.path.cmp(&b.path));
    if entries.windows(2).any(|pair| pair[0].path == pair[1].path) {
        return Err("package_path_collision");
    }

    let counts = json!({
        "plan_revisions": checkpoints.len(), "research_runs": runs.len(), "agent_decisions": turn_total,
        "agent_deferrals": deferral_total, "access_events": access.as_array().map_or(0, Vec::len),
        "tasks": tasks.len(), "artifacts_included": file_records.len(), "artifacts_missing": missing.len(),
        "artifact_bytes_included": included_bytes, "generated_code_blocks": generated.len(),
        "reviews": reviews.len(), "person_reviews": person_review_count,
        "rerun_analyses": analyses.len(), "papers": papers.len(),
        "reasoning_records": reasoning.len(), "result_digests": digests.len(),
    });
    let files: Vec<Value> = entries
        .iter()
        .map(|e| json!({"path": e.path, "size_bytes": e.size, "sha256": e.sha256}))
        .collect();
    let manifest = json!({
        "schema": "enrichedlab.project-evidence-package.v2",
        "root": root,
        "snapshot_at": snapshot_at,
        "disclosure_scope": package_revision.disclosure_scope.as_str(),
        "revision": package_revision.revision,
        "content_sha256": package_revision.content_sha256,
        "generator": {"service": "enrichedlab-agent-broker", "version": env!("CARGO_PKG_VERSION")},
        "project": project,
        "research_question": latest_goal,
        "contents": manifest_contents(&package_revision.inventory)?,
        "changes_since": changes_since(package_revision)?,
        "counts": counts,
        "coverage": {
            "database_records": "complete",
            "artifact_bytes": if missing.is_empty() { "complete" } else { "partial" },
            "reasoning": if snapshot["reasoning_access"].as_bool() == Some(true) { "complete" } else { "lab_members_only" },
            "scientific_validation": "not_performed",
        },
        "missing": missing,
        "artifact_sources": file_records,
        "files": files,
        "verification": {"algorithm": "sha256", "checksums_file": "SHA256SUMS",
            "statement": "Checksums prove the files match the retained records. They do not prove scientific validity."},
    });
    let manifest_bytes = json_bytes(&manifest);
    let manifest_sha256 = sha_hex(&manifest_bytes);
    let readme = readme_text(title, latest_goal, &snapshot_at, &counts, &missing);
    let readme_bytes = readme.into_bytes();
    let mut all = entries;
    all.push(Entry {
        path: "manifest.json".into(),
        size: manifest_bytes.len() as u64,
        sha256: manifest_sha256.clone(),
        source: EntrySource::Bytes(Arc::new(manifest_bytes)),
    });
    all.push(Entry {
        path: "README.md".into(),
        size: readme_bytes.len() as u64,
        sha256: sha_hex(&readme_bytes),
        source: EntrySource::Bytes(Arc::new(readme_bytes)),
    });
    all.sort_by(|a, b| a.path.cmp(&b.path));
    let mut sums = String::new();
    for entry in &all {
        sums.push_str(&format!("{}  {}\n", entry.sha256, entry.path));
    }
    let sums = sums.into_bytes();
    all.push(Entry {
        path: "SHA256SUMS".into(),
        size: sums.len() as u64,
        sha256: sha_hex(&sums),
        source: EntrySource::Bytes(Arc::new(sums)),
    });
    // README first so an archive browser shows it at the top; checksums last.
    all.sort_by_key(|e| match e.path.as_str() {
        "README.md" => (0, String::new()),
        "manifest.json" => (1, String::new()),
        "SHA256SUMS" => (3, String::new()),
        other => (2, other.to_owned()),
    });
    Ok(PackagePlan {
        root,
        mtime,
        snapshot_at,
        manifest,
        manifest_sha256,
        entries: all,
        missing,
    })
}

fn readme_text(
    title: &str,
    goal: &str,
    snapshot_at: &str,
    counts: &Value,
    missing: &[MissingArtifact],
) -> String {
    let n = |key: &str| counts[key].as_u64().unwrap_or(0);
    let mut out = String::new();
    out.push_str(&format!("# {title}\n\n"));
    out.push_str("Start with [`SUMMARY.md`](SUMMARY.md). Citation details are in [`CITATION.cff`](CITATION.cff).\n\n");
    if !goal.is_empty() {
        out.push_str(&format!("Research question: {goal}\n\n"));
    }
    out.push_str(&format!(
        "This package holds the complete retained record of the project as of {snapshot_at} (UTC), taken from one \
consistent database snapshot, with the original result files.\n\n"
    ));
    out.push_str("## What is inside\n\n");
    out.push_str(&format!(
        "- `project/plan/`: {} saved versions of the question and plan, oldest first.\n",
        n("plan_revisions")
    ));
    out.push_str(&format!(
        "- `runs/`: {} self-contained research runs with {} recorded agent decisions and {} deferrals. A deferral is a \
pause, a retry after the planning model's reply was rejected, or a wait for tool results. Agent statements are \
saved interpretations; they have not been verified.\n",
        n("research_runs"), n("agent_decisions"), n("agent_deferrals")
    ));
    out.push_str(&format!(
        "- `project/access_history.json`: {} access and administration events.\n",
        n("access_events")
    ));
    out.push_str(&format!(
        "- `tasks/`: {} analyses. Each has its request, lifecycle and runtime record, and under `files/` the original \
retained outputs ({} files, {}).\n",
        n("tasks"), n("artifacts_included"), human_bytes(n("artifact_bytes_included"))
    ));
    if n("generated_code_blocks") > 0 {
        out.push_str(&format!(
            "- `tasks/<analysis>/generated_code/`: {} analyses ran R code written by the research model inside a reviewed \
template. `block.R` is the exact code that ran and `record.json` its admission check. That code is unvalidated until a \
scientist reviews it.\n",
            n("generated_code_blocks")
        ));
    }
    if n("reviews") > 0 {
        out.push_str(&format!(
            "- `tasks/<analysis>/reviews.json`: {} recorded reviews, oldest first. Each records whether its reviewer was \
a person or a service account, the safe display name, the decision, the note and the SHA-256 of every result file it \
covered. A person's review is a reading of the results and code, not statistical validation.\n",
            n("reviews")
        ));
    }
    out.push_str("- `manifest.json`: counts, coverage, and the SHA-256 of every file.\n\n");
    out.push_str("## Check the files\n\n");
    out.push_str("macOS: `shasum -a 256 -c SHA256SUMS`  \nLinux: `sha256sum -c SHA256SUMS`  \n");
    out.push_str("Windows PowerShell: compare `Get-FileHash <file> -Algorithm SHA256` with the value listed in `SHA256SUMS`.\n\n");
    out.push_str("## Rerun the analyses\n\n");
    out.push_str(
        "`rerun/README.md` explains the rerun kit. From this folder, `python3 rerun/verify.py` checks every checksum, \
unpacks each analysis into `extracted/` with a method sheet, and writes import scripts for SAS and JMP; `python3 \
rerun/verify.py --rerun` also runs each analysis again and compares its tables with the retained ones. \
`rerun/slurm/rerun.sbatch` does the same on an HPC. Each analysis package (`files/payload.tar.gz`) also carries its own \
README, exact R code, input data, parameters, seed, environment record and checksums.\n\n",
    );
    out.push_str("## What this does not show\n\n");
    if missing.is_empty() {
        out.push_str("Every retained file registered for these analyses is included.\n\n");
    } else {
        out.push_str("These registered files were not available and are not included:\n\n");
        for item in missing {
            out.push_str(&format!(
                "- `{}` ({}), analysis {}\n",
                item.relative_path, item.label, item.task_id
            ));
        }
        out.push('\n');
    }
    out.push_str(
        "Completed runs and matching checksums show what ran and that the files are intact. They are not scientific \
validation. Scientific review and acceptance remain with the research team.\n",
    );
    out
}

/// How to use the rerun kit, with the analyses in the order the verifier numbers them.
fn rerun_readme(analyses: &[Value]) -> String {
    let mut out = String::from("# Rerun and check these analyses\n\n");
    out.push_str(
        "This folder checks that every file in the package is intact, unpacks each analysis, and can run each one again \
and compare its results with the retained ones. It needs Python 3.8 or later. Reviewed template and recipe packages use \
R 4.6.1; each render-service template analysis records its R version and package versions in `spec.json`.\n\n",
    );
    out.push_str("## 1. Check and unpack\n\nFrom the package folder (the one with `SHA256SUMS`):\n\n    python3 rerun/verify.py\n\n");
    out.push_str(
        "This checks every checksum and unpacks each analysis into `extracted/`, each with a `METHOD.md`: what ran, with \
which settings, seed, code and data checksums, the input-dependence check and people's reviews. It also writes:\n\n\
- `extracted/tables.csv`: every result table with its checksum.\n\
- `extracted/import_all.sas`: one `PROC IMPORT` per table, into SAS's `WORK` library.\n\
- `extracted/open_all.jsl`: opens every table in JMP.\n\n",
    );
    out.push_str(
        "## 2. Rerun\n\nWith the matching R environment installed:\n\n    python3 rerun/verify.py --rerun\n\n",
    );
    out.push_str(&format!(
        "With Docker or Podman and the R 4.6.1 image tested for template and recipe packages:\n\n    python3 rerun/verify.py --rerun --formats template,recipe --rscript \
\"docker run --rm --network none -e ENRICHEDLAB_TEMPLATE_RERUN=local -v {{dir}}:{{dir}} -w {{dir}} {RERUN_R_IMAGE} Rscript\"\n\n"
    ));
    out.push_str(
        "In RStudio, open an analysis folder under `extracted/` and run the command in its `METHOD.md` in the Terminal \
tab. Each rerun writes a new `rerun` folder beside the retained files and never overwrites them. \
`extracted/rerun_report.json` says whether each rerun is identical, within the relative tolerance (1e-9 by default; \
`--tolerance` changes it) or different, with comparison counts and never result values. The supported formats are:\n\n\
- `template`: an analysis whose `run.json` names its reviewed template and seed.\n\
- `recipe`: the reviewed recipe whose `run_parameters.json` names its statistic and seed.\n\
- `render-template`: reviewed render-service code whose `spec.json` has schema \
  `enrichedlab.template-analysis-spec.v1`; it reuses the recorded table from another analysis in this package and \
  compares `results_exact.json` with retained `results/record.json`.\n\n\
`--formats` takes a comma-separated subset of those names and defaults to all three. An excluded or unsupported package \
is reported as not rerun. The verifier's exit status: 0 when every checksum matches and every analysis was rerun with \
matching results; 1 when a checksum or file is wrong; 2 when a rerun failed or a result is missing or differs; 3 when \
some analysis could not be rerun or had nothing to compare.\n\n",
    );
    out.push_str(
        "## 3. On an HPC with Slurm\n\n`rerun/slurm/rerun.sbatch` runs the template and recipe formats as a Slurm job with Apptainer. \
From the package folder:\n\n    sbatch rerun/slurm/rerun.sbatch\n\nAdd `-A <account>` where your cluster asks for one. \
The first time, the job pulls the R image by its digest when the compute node can reach the internet; the top of the \
script says how to pull it elsewhere. The job runs the check with `--reviewed-only`: an analysis whose code the \
research agent wrote is rerun only when a person on the project has accepted it, and the report names the others as \
not rerun. It also passes `--formats template,recipe` because its pinned base-R image does not include the packages \
render-service templates load; those analyses are reported as not rerun until a pinned image carries that environment. \
On your own computer, without `--reviewed-only`, every selected analysis reruns, so a reviewer can rerun one before \
accepting it. Each job writes its results to `extracted-<job id>`, so two jobs from one package never share files; \
the package folder must be writable.\n\n",
    );
    let handoff = hpc_handoff("PACKAGE");
    out.push_str(&format!(
        "### SCINet Atlas and Ceres\n\nThese steps are written by code from SCINet's own guides, as checked on {}; \
the sources are listed below. If a page has changed since, the guide wins.\n\n",
        handoff["checked"].as_str().unwrap_or_default()
    ));
    out.push_str(
        "`PACKAGE` is the archive's file name without `.tar.gz`; it is also the folder the archive unpacks to.\n\n",
    );
    for site in handoff["sites"].as_array().into_iter().flatten() {
        out.push_str(&format!(
            "#### {}\n\n",
            site["name"].as_str().unwrap_or_default()
        ));
        for (i, step) in site["steps"].as_array().into_iter().flatten().enumerate() {
            out.push_str(&format!(
                "{}. {}\n",
                i + 1,
                step["text"].as_str().unwrap_or_default()
            ));
            if let Some(command) = step["command"].as_str() {
                out.push_str(&format!("\n        {command}\n"));
            }
            out.push('\n');
        }
    }
    out.push_str("Sources:\n\n");
    for source in handoff["sources"].as_array().into_iter().flatten() {
        out.push_str(&format!(
            "- {}, updated {}: <{}>\n",
            source["title"].as_str().unwrap_or_default(),
            source["updated"].as_str().unwrap_or_default(),
            source["url"].as_str().unwrap_or_default()
        ));
    }
    out.push('\n');
    out.push_str("## The analyses\n\n");
    if analyses.is_empty() {
        out.push_str("This package has no retained analysis package to rerun.\n\n");
    } else {
        out.push_str("| # | Analysis | Method | Package |\n|---|---|---|---|\n");
        for (i, a) in analyses.iter().enumerate() {
            let title = a["title"]
                .as_str()
                .or_else(|| a["task_id"].as_str())
                .unwrap_or_default()
                .replace('|', "/");
            out.push_str(&format!(
                "| {} | {} | `{}` | `{}` |\n",
                i + 1,
                title,
                a["method"].as_str().unwrap_or("unknown"),
                a["package"].as_str().unwrap_or_default()
            ));
        }
        out.push('\n');
    }
    out.push_str(
        "An analysis in an unsupported format says so; follow its own README. A matching rerun shows that the code \
reproduces the retained results from the retained input. It is not statistical validation, which stays with the \
research team.\n",
    );
    out
}

struct HashingWriter<W: Write> {
    inner: W,
    digest: Sha256,
    bytes: u64,
}

impl<W: Write> Write for HashingWriter<W> {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        let written = self.inner.write(buf)?;
        self.digest.update(&buf[..written]);
        self.bytes += written as u64;
        Ok(written)
    }
    fn flush(&mut self) -> io::Result<()> {
        self.inner.flush()
    }
}

/// Reads exactly `remaining` bytes and fails on a short file, extra bytes or a hash
/// mismatch. The tar header already committed to the recorded size.
struct VerifiedArtifactReader {
    file: File,
    remaining: u64,
    digest: Sha256,
    expected: String,
    finished: bool,
}

impl Read for VerifiedArtifactReader {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if self.finished {
            return Ok(0);
        }
        if self.remaining == 0 {
            let mut probe = [0_u8; 1];
            if self.file.read(&mut probe)? != 0 {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "artifact_integrity_failed",
                ));
            }
            let actual = hex::encode(std::mem::take(&mut self.digest).finalize());
            self.finished = true;
            if actual != self.expected {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "artifact_integrity_failed",
                ));
            }
            return Ok(0);
        }
        let limit = buf.len().min(self.remaining as usize);
        let read = self.file.read(&mut buf[..limit])?;
        if read == 0 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "artifact_integrity_failed",
            ));
        }
        self.remaining -= read as u64;
        self.digest.update(&buf[..read]);
        Ok(read)
    }
}

#[derive(Debug)]
pub enum WriteError {
    /// This artifact's bytes no longer match its record.
    Artifact(String),
    Io,
}

/// Write the whole package. Identical plans produce identical bytes.
pub fn write_package<W: Write>(plan: &PackagePlan, out: W) -> Result<PackageSummary, WriteError> {
    let hashing = HashingWriter {
        inner: out,
        digest: Sha256::new(),
        bytes: 0,
    };
    let gzip = GzBuilder::new()
        .mtime(0)
        .write(hashing, Compression::default());
    let mut builder = tar::Builder::new(gzip);
    builder.mode(tar::HeaderMode::Deterministic);
    for entry in &plan.entries {
        let mut header = tar::Header::new_gnu();
        header.set_entry_type(tar::EntryType::Regular);
        header.set_size(entry.size);
        header.set_mode(0o644);
        header.set_uid(0);
        header.set_gid(0);
        header.set_mtime(plan.mtime);
        let path = format!("{}/{}", plan.root, entry.path);
        match &entry.source {
            EntrySource::Bytes(bytes) => builder
                .append_data(&mut header, &path, bytes.as_slice())
                .map_err(|_| WriteError::Io)?,
            EntrySource::Artifact(artifact) => {
                let Some(file_path) = &artifact.path else {
                    return Err(WriteError::Artifact(artifact.artifact_id.clone()));
                };
                let file = open_regular(file_path, artifact.size_bytes)
                    .map_err(|_| WriteError::Artifact(artifact.artifact_id.clone()))?;
                let reader = VerifiedArtifactReader {
                    file,
                    remaining: artifact.size_bytes,
                    digest: Sha256::new(),
                    expected: artifact.sha256.clone(),
                    finished: false,
                };
                builder
                    .append_data(&mut header, &path, reader)
                    .map_err(|error| {
                        if error.kind() == io::ErrorKind::InvalidData {
                            WriteError::Artifact(artifact.artifact_id.clone())
                        } else {
                            WriteError::Io
                        }
                    })?;
            }
        }
    }
    let gzip = builder.into_inner().map_err(|_| WriteError::Io)?;
    let mut hashing = gzip.finish().map_err(|_| WriteError::Io)?;
    hashing.flush().map_err(|_| WriteError::Io)?;
    Ok(PackageSummary {
        bytes: hashing.bytes,
        sha256: hex::encode(hashing.digest.finalize()),
        files: plan.entries.len(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    fn tar_gz(files: &[(&str, &[u8])]) -> Vec<u8> {
        let mut builder =
            tar::Builder::new(GzBuilder::new().write(Vec::new(), Compression::default()));
        for (path, bytes) in files {
            let mut header = tar::Header::new_gnu();
            header.set_size(bytes.len() as u64);
            header.set_mode(0o600);
            header.set_cksum();
            builder.append_data(&mut header, path, *bytes).unwrap();
        }
        builder.into_inner().unwrap().finish().unwrap()
    }

    fn retained_package(mut files: Vec<(String, Vec<u8>)>) -> Vec<u8> {
        files.sort_by(|a, b| a.0.cmp(&b.0));
        let sums = files
            .iter()
            .map(|(path, bytes)| format!("{}  {path}\n", sha_hex(bytes)))
            .collect::<String>();
        files.push(("SHA256SUMS".into(), sums.into_bytes()));
        let refs: Vec<(&str, &[u8])> = files
            .iter()
            .map(|(path, bytes)| (path.as_str(), bytes.as_slice()))
            .collect();
        tar_gz(&refs)
    }

    fn write_temp(bytes: &[u8]) -> (tempfile::TempDir, PathBuf, String) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("payload");
        std::fs::write(&path, bytes).unwrap();
        (dir, path, sha_hex(bytes))
    }

    #[test]
    fn member_paths_reject_escape_and_normalize_tool_prefixes() {
        assert_eq!(
            normalize_member_path(b"./results/summary.csv").as_deref(),
            Some("results/summary.csv")
        );
        for bad in [
            &b"../secret"[..],
            b"/etc/passwd",
            b"a/../b",
            b"a//b",
            b"a\\b",
            b"",
        ] {
            assert!(
                normalize_member_path(bad).is_none(),
                "{:?}",
                String::from_utf8_lossy(bad)
            );
        }
    }

    #[test]
    fn listing_hashes_members_and_compares_the_packages_own_checksums() {
        let figure = b"%PDF-1.4 figure";
        let table = b"supp,dose,mean\nVC,0.5,7.98\n";
        let sums = format!(
            "{}  results/figure.pdf\n{}  results/summary.csv\n{}  results/absent.csv\n",
            sha_hex(figure),
            "0".repeat(64),
            sha_hex(b"x")
        );
        let archive = tar_gz(&[
            ("./results/figure.pdf", figure),
            ("./results/summary.csv", table),
            ("SHA256SUMS", sums.as_bytes()),
        ]);
        let (_dir, path, sha) = write_temp(&archive);
        let listing = list_package(&path, archive.len() as u64, &sha, None).unwrap();
        let figure_member = listing.member("results/figure.pdf").unwrap();
        assert_eq!(figure_member.manifest, ManifestCheck::Matched);
        assert_eq!(figure_member.kind, "figure");
        assert_eq!(
            listing.member("results/summary.csv").unwrap().manifest,
            ManifestCheck::Mismatched
        );
        assert_eq!(
            listing.missing_from_archive,
            vec!["results/absent.csv".to_owned()]
        );
        let mut streamed = Vec::new();
        let open = || open_package(&path, archive.len() as u64).unwrap();
        stream_member(open(), figure_member, None, |chunk| {
            streamed.extend(chunk);
            true
        })
        .unwrap();
        assert_eq!(streamed, figure);
        for (range, want) in [
            ((0, 3), &figure[..4]),
            ((5, 99), &figure[5..]),
            ((14, 14), &figure[14..]),
        ] {
            let mut part = Vec::new();
            stream_member(open(), figure_member, Some(range), |chunk| {
                part.extend(chunk);
                true
            })
            .unwrap();
            assert_eq!(part, want, "{range:?}");
        }
        let table_member = listing.member("results/summary.csv").unwrap();
        assert_eq!(
            read_member(&path, archive.len() as u64, &sha, table_member, 1024).unwrap(),
            table
        );
        assert_eq!(
            read_member(&path, archive.len() as u64, &sha, table_member, 4).unwrap_err(),
            PackageError::TooLarge
        );
        assert_eq!(
            read_member(
                &path,
                archive.len() as u64,
                &"0".repeat(64),
                table_member,
                1024
            )
            .unwrap_err(),
            PackageError::Integrity
        );
    }

    #[test]
    fn listing_refuses_changed_bytes_and_non_packages() {
        let archive = tar_gz(&[("a.txt", b"a")]);
        let (_dir, path, _) = write_temp(&archive);
        assert_eq!(
            list_package(&path, archive.len() as u64, &"0".repeat(64), None).unwrap_err(),
            PackageError::Integrity
        );
        let (_dir2, plain, sha) = write_temp(b"not an archive");
        assert_eq!(
            list_package(&plain, 14, &sha, None).unwrap_err(),
            PackageError::NotAPackage
        );
    }

    #[test]
    fn a_file_replaced_after_its_check_is_not_the_one_read() {
        let (dir, path, sha) = write_temp(b"checked bytes");
        let mut file = verified_file_then(&path, 13, &sha, || {
            let other = dir.path().join("other");
            std::fs::write(&other, b"changed bytes").unwrap();
            std::fs::rename(&other, &path).unwrap();
        })
        .unwrap();
        let mut read = String::new();
        file.read_to_string(&mut read).unwrap();
        assert_eq!(read, "checked bytes");
        assert_eq!(std::fs::read(&path).unwrap(), b"changed bytes");
    }

    /// A raw entry the way another writer, or a hostile one, would put it.
    fn raw_entry(
        builder: &mut tar::Builder<impl Write>,
        kind: tar::EntryType,
        name: &str,
        body: &[u8],
    ) {
        let mut header = if kind.is_pax_local_extensions() || kind.is_pax_global_extensions() {
            tar::Header::new_ustar()
        } else {
            tar::Header::new_gnu()
        };
        header.set_entry_type(kind);
        header.set_path(name).unwrap();
        header.set_size(body.len() as u64);
        header.set_mode(0o600);
        header.set_cksum();
        builder.append(&header, body).unwrap();
    }

    fn pax_record(key: &str, value: &str) -> Vec<u8> {
        let rest = format!(" {key}={value}\n");
        let mut length = rest.len() + 1;
        while format!("{length}{rest}").len() != length {
            length += 1;
        }
        format!("{length}{rest}").into_bytes()
    }

    fn package_of(
        entries: impl FnOnce(&mut tar::Builder<flate2::write::GzEncoder<Vec<u8>>>),
    ) -> Vec<u8> {
        let mut builder =
            tar::Builder::new(GzBuilder::new().write(Vec::new(), Compression::default()));
        entries(&mut builder);
        builder.into_inner().unwrap().finish().unwrap()
    }

    fn listed(archive: &[u8]) -> Result<PackageListing, PackageError> {
        let (_dir, path, sha) = write_temp(archive);
        list_package(&path, archive.len() as u64, &sha, None)
    }

    #[test]
    fn long_names_and_pax_paths_name_members_within_bounds() {
        let long = format!("results/{}.csv", "n".repeat(200));
        let archive = package_of(|b| {
            let mut header = tar::Header::new_gnu();
            header.set_size(3);
            header.set_mode(0o600);
            header.set_cksum();
            // The tar crate writes a GNU long name entry ahead of a path this long.
            b.append_data(&mut header, &long, &b"1,2"[..]).unwrap();
            raw_entry(
                b,
                tar::EntryType::XHeader,
                "PaxHeader",
                &pax_record("path", "results/pax.csv"),
            );
            raw_entry(b, tar::EntryType::Regular, "short", b"pax");
            raw_entry(
                b,
                tar::EntryType::XHeader,
                "PaxHeader",
                &pax_record("mtime", "1.5"),
            );
            raw_entry(b, tar::EntryType::Regular, "results/plain.csv", b"plain");
        });
        let (_dir, path, sha) = write_temp(&archive);
        let listing = list_package(&path, archive.len() as u64, &sha, None).unwrap();
        let paths: Vec<&str> = listing.members.iter().map(|m| m.path.as_str()).collect();
        assert_eq!(
            paths,
            [long.as_str(), "results/pax.csv", "results/plain.csv"]
        );
        for (name, want) in [(long.as_str(), &b"1,2"[..]), ("results/pax.csv", b"pax")] {
            let member = listing.member(name).unwrap();
            assert_eq!(
                read_member(&path, archive.len() as u64, &sha, member, 64).unwrap(),
                want
            );
        }
    }

    #[test]
    fn metadata_past_its_bounds_refuses_the_package() {
        let refused = [
            (
                "a long name past its bound",
                package_of(|b| {
                    raw_entry(
                        b,
                        tar::EntryType::GNULongName,
                        "././@LongLink",
                        &vec![b'a'; 4098],
                    );
                    raw_entry(b, tar::EntryType::Regular, "x", b"x");
                }),
            ),
            (
                "a PAX header past its bound",
                package_of(|b| {
                    let body = pax_record("comment", &"c".repeat(70 * 1024));
                    raw_entry(b, tar::EntryType::XHeader, "PaxHeader", &body);
                    raw_entry(b, tar::EntryType::Regular, "x", b"x");
                }),
            ),
            (
                "a PAX size",
                package_of(|b| {
                    raw_entry(
                        b,
                        tar::EntryType::XHeader,
                        "PaxHeader",
                        &pax_record("size", "1"),
                    );
                    raw_entry(b, tar::EntryType::Regular, "x", b"x");
                }),
            ),
            (
                "two long names for one entry",
                package_of(|b| {
                    raw_entry(b, tar::EntryType::GNULongName, "././@LongLink", b"a\0");
                    raw_entry(b, tar::EntryType::GNULongName, "././@LongLink", b"b\0");
                    raw_entry(b, tar::EntryType::Regular, "x", b"x");
                }),
            ),
            (
                "two PAX headers for one entry",
                package_of(|b| {
                    raw_entry(
                        b,
                        tar::EntryType::XHeader,
                        "PaxHeader",
                        &pax_record("path", "a"),
                    );
                    raw_entry(
                        b,
                        tar::EntryType::XHeader,
                        "PaxHeader",
                        &pax_record("path", "b"),
                    );
                    raw_entry(b, tar::EntryType::Regular, "x", b"x");
                }),
            ),
            (
                "a long name that names no entry",
                package_of(|b| {
                    raw_entry(b, tar::EntryType::GNULongName, "././@LongLink", b"a\0");
                }),
            ),
            (
                "a sparse entry with extension headers",
                package_of(|b| {
                    let mut header = tar::Header::new_gnu();
                    header.set_entry_type(tar::EntryType::GNUSparse);
                    header.set_path("results/sparse.csv").unwrap();
                    header.set_size(512);
                    header.set_mode(0o600);
                    header.as_gnu_mut().unwrap().isextended[0] = 1;
                    header.set_cksum();
                    b.append(&header, &[0_u8; 512][..]).unwrap();
                }),
            ),
        ];
        for (why, archive) in refused {
            assert_eq!(
                listed(&archive).unwrap_err(),
                PackageError::NotAPackage,
                "{why}"
            );
        }
        let at_bound = package_of(|b| {
            let name = format!("results/{}", "a".repeat(4088));
            raw_entry(
                b,
                tar::EntryType::GNULongName,
                "././@LongLink",
                &[name.as_bytes(), b"\0"].concat(),
            );
            raw_entry(b, tar::EntryType::Regular, "x", b"x");
        });
        let listing = listed(&at_bound).unwrap();
        assert_eq!(
            listing.skipped.len(),
            1,
            "a long name at its bound is read; past 512 bytes it is unsafe"
        );
        assert_eq!(listing.skipped[0].reason, "unsafe_path");
    }

    #[test]
    fn entries_past_the_bound_refuse_the_package() {
        let archive = |count: usize| {
            package_of(|b| {
                for at in 0..count {
                    raw_entry(b, tar::EntryType::Symlink, &format!("l{at}"), b"");
                }
            })
        };
        let listing = listed(&archive(MAX_PACKAGE_ENTRIES)).unwrap();
        assert_eq!(listing.skipped.len(), MAX_PACKAGE_ENTRIES);
        assert_eq!(
            listed(&archive(MAX_PACKAGE_ENTRIES + 1)).unwrap_err(),
            PackageError::TooLarge
        );
    }

    #[test]
    fn the_decompressed_stream_is_bounded_across_skipped_entries() {
        let archive = package_of(|b| {
            raw_entry(
                b,
                tar::EntryType::XGlobalHeader,
                "pax_global_header",
                &vec![b'0'; 2 << 20],
            );
            raw_entry(b, tar::EntryType::Regular, "x", b"x");
        });
        let mut package = Package::with_limit(&archive[..], 1 << 20);
        let mut members = package.members().unwrap();
        let mut seen = Vec::new();
        let failure = loop {
            match members.next_member() {
                Ok(Some((path, _))) => seen.push(path),
                Ok(None) => break None,
                Err(error) => break Some(error),
            }
        };
        assert_eq!(failure, Some(PackageError::TooLarge));
        let mut whole = Package::new(&archive[..]);
        let mut members = whole.members().unwrap();
        let mut count = 0;
        while members.next_member().unwrap().is_some() {
            count += 1;
        }
        assert_eq!(
            count, 2,
            "the same package within the production bound reads whole"
        );
    }

    #[test]
    fn a_checksum_line_with_text_across_its_digest_is_ignored() {
        let line = format!(
            "{}\u{e9}{}  results/a.csv\n",
            "a".repeat(63),
            "b".repeat(10)
        );
        assert!(parse_checksums(&line).is_empty());
        let good = format!("{}  results/a.csv\n", "c".repeat(64));
        assert_eq!(parse_checksums(&good)["results/a.csv"], "c".repeat(64));
    }

    #[test]
    fn unsafe_members_are_skipped_not_served() {
        let mut builder =
            tar::Builder::new(GzBuilder::new().write(Vec::new(), Compression::default()));
        let mut header = tar::Header::new_gnu();
        header.set_size(1);
        header.set_mode(0o600);
        // Write a raw traversal name the way a hostile archive would.
        header.as_gnu_mut().unwrap().name[..9].copy_from_slice(b"../escape");
        header.set_cksum();
        builder.append(&header, &b"x"[..]).unwrap();
        let mut link = tar::Header::new_gnu();
        link.set_entry_type(tar::EntryType::Symlink);
        link.set_size(0);
        link.set_link_name("/etc/passwd").unwrap();
        link.set_cksum();
        builder.append_data(&mut link, "link", io::empty()).unwrap();
        let archive = builder.into_inner().unwrap().finish().unwrap();
        let (_dir, path, sha) = write_temp(&archive);
        let listing = list_package(&path, archive.len() as u64, &sha, None).unwrap();
        assert!(listing.members.is_empty());
        assert_eq!(listing.skipped.len(), 2);
    }

    fn snapshot() -> Value {
        json!({
            "snapshot_at": "2026-09-23T06:00:00.123456+00:00",
            "requester": {"principal_id": "00000000-0000-0000-0000-000000000001", "capabilities": ["read"]},
            "reasoning_access": true,
            "reasoning": [],
            "project": {"project_id": "11111111-2222-3333-4444-555555555555", "title": "Tooth growth: vitamin C", "group_key": "example-lab"},
            "checkpoints": [{"revision": 1, "state": {"goal": "Describe mean tooth length."}}],
            "research_runs": [{"run": {"run_id": "r1"}, "turns": [{"turn": 1}], "events": [{"sequence": 1, "code": "proposal_invalid"}]}],
            "access_history": [],
            "tasks": [{"task_id": "t1", "events": [], "artifacts": [], "recoveries": []}],
        })
    }

    fn generated_text(plan: &PackagePlan, path: &str) -> String {
        match &plan
            .entries
            .iter()
            .find(|entry| entry.path == path)
            .unwrap()
            .source
        {
            EntrySource::Bytes(bytes) => String::from_utf8(bytes.to_vec()).unwrap(),
            EntrySource::Artifact(_) => panic!("{path} is an artifact"),
        }
    }

    fn revision(snapshot: &Value, artifacts: &[ArtifactSource]) -> PackageRevision {
        let mut revision = PackageRevision::fixture(
            1,
            snapshot["snapshot_at"].as_str().unwrap(),
            super::super::package_registry::build_inventory(snapshot, artifacts).unwrap(),
        );
        revision.disclosure_scope =
            super::super::package_registry::disclosure_scope(snapshot).unwrap();
        revision
    }

    #[test]
    fn literature_review_artifacts_are_packaged_only_for_member_readers() {
        let mut snap = snapshot();
        let run = snap["research_runs"][0]["run"]["run_id"]
            .as_str()
            .unwrap()
            .to_owned();
        let digest = "a".repeat(64);
        let review = json!({"approaches": [{"text": "Synthetic approach", "refs": ["library://synthetic/section/a"]}],
            "methods_and_data": [], "expected": [], "pitfalls": [], "gaps": [], "papers": []});
        snap["literature_reviews"] = json!([{"run_id": run, "inputs_sha256": format!("sha256:{digest}"),
            "record": {"review": review}}]);
        let plan = plan_package(&snap, vec![], &revision(&snap, &[])).unwrap();
        let path = format!("runs/{run}/literature_reviews/{digest}");
        assert!(generated_text(&plan, &format!("{path}.json")).contains("Synthetic approach"));
        assert!(generated_text(&plan, &format!("{path}.md")).contains("Synthetic approach"));
        snap["literature_reviews"][0]["record"] = json!({"status": "refused", "faults": ["shape"]});
        let refused = plan_package(&snap, vec![], &revision(&snap, &[])).unwrap();
        assert!(generated_text(&refused, &format!("{path}.json")).contains("refused"));
        assert!(
            !refused
                .entries
                .iter()
                .any(|e| e.path == format!("{path}.md"))
        );
        for scope in ["ordinary_without_reasoning", "inspection"] {
            snap["requester"]["inspection_expires_at"] = if scope == "inspection" {
                json!("2099-01-01T00:00:00Z")
            } else {
                Value::Null
            };
            snap["reasoning_access"] = json!(false);
            let rev = revision(&snap, &[]);
            assert!(!rev.inventory.to_string().contains("literature_review"));
            let plan = plan_package(&snap, vec![], &rev).unwrap();
            assert!(
                !plan
                    .entries
                    .iter()
                    .any(|e| e.path.contains("literature_reviews"))
            );
        }
    }

    #[test]
    fn project_package_is_deterministic_verified_and_names_missing_files() {
        let dir = tempfile::tempdir().unwrap();
        let bytes = b"retained package bytes";
        let file = dir.path().join("payload");
        std::fs::write(&file, bytes).unwrap();
        let artifact = ArtifactSource {
            task_id: "t1".into(),
            artifact_id: "a1".into(),
            relative_path: "tasks/t1/payload".into(),
            media_type: "application/gzip".into(),
            label: "R analysis package".into(),
            size_bytes: bytes.len() as u64,
            sha256: sha_hex(bytes),
            path: Some(file.clone()),
            mismatch: false,
        };
        let absent = ArtifactSource {
            artifact_id: "a2".into(),
            relative_path: "tasks/t1/payload.sha256".into(),
            path: None,
            ..artifact.clone()
        };
        let snap = snapshot();
        let artifacts = vec![artifact.clone(), absent];
        let plan = plan_package(&snap, artifacts.clone(), &revision(&snap, &artifacts)).unwrap();
        assert_eq!(
            plan.root,
            "example-lab_tooth_growth_vitamin_c_11111111_r1_20260923T060000Z"
        );
        assert_eq!(plan.manifest["disclosure_scope"], "ordinary_with_reasoning");
        assert_eq!(plan.missing.len(), 1);
        let mut first = Vec::new();
        let summary = write_package(&plan, &mut first).unwrap();
        let mut second = Vec::new();
        assert_eq!(
            write_package(&plan, &mut second).unwrap().sha256,
            summary.sha256
        );
        assert_eq!(first, second);
        assert_eq!(summary.bytes, first.len() as u64);
        let mut archive = tar::Archive::new(GzDecoder::new(Cursor::new(first)));
        let names: Vec<String> = archive
            .entries()
            .unwrap()
            .map(|e| e.unwrap().path().unwrap().display().to_string())
            .collect();
        let first_path = format!("{}/README.md", plan.root);
        assert_eq!(names.first().map(String::as_str), Some(first_path.as_str()));
        assert!(
            names
                .iter()
                .any(|n| n.ends_with("tasks/t1/files/payload.tar.gz"))
        );
        assert!(names.last().unwrap().ends_with("SHA256SUMS"));

        // Changed bytes are refused during the write, then excluded and named.
        std::fs::write(&file, b"altered package bytes!").unwrap();
        let mut artifacts = vec![artifact];
        match write_package(
            &plan_package(&snap, artifacts.clone(), &revision(&snap, &artifacts)).unwrap(),
            io::sink(),
        ) {
            Err(WriteError::Artifact(id)) => assert_eq!(id, "a1"),
            other => panic!("expected an integrity refusal, got {other:?}"),
        }
        verify_artifact_sources(&mut artifacts);
        let corrected =
            plan_package(&snap, artifacts.clone(), &revision(&snap, &artifacts)).unwrap();
        assert_eq!(
            corrected.manifest["missing"][0]["reason"],
            "retained_bytes_do_not_match_record"
        );
        assert_eq!(corrected.manifest["coverage"]["artifact_bytes"], "partial");
        write_package(&corrected, io::sink()).unwrap();
    }

    #[test]
    fn citation_and_summary_use_only_recorded_people_sources_software_and_analyses() {
        let dir = tempfile::tempdir().unwrap();
        let query_receipt = json_bytes(&json!({
            "schema": "enrichedlab.query-receipt.v1",
            "title": "Forged archive source",
            "release": "Forged archive release",
            "snapshot": {"download": "https://forged.invalid/source.zip"},
            "row_count": 999
        }));
        let query = retained_package(vec![
            ("receipt.json".into(), query_receipt),
            (
                "run.json".into(),
                json_bytes(&json!({"method": "Forged query method", "dataset": {"rows": 999}})),
            ),
            ("results/unregistered.csv".into(), b"food,amount\n".to_vec()),
        ]);
        let analysis_spec = json_bytes(&json!({
            "schema": "enrichedlab.template-analysis-spec.v1",
            "method": "Forged analysis method",
            "source": {"task_id": "forged-task", "rows": 999},
            "service": {"version": "99.0.0", "image_digest": format!("sha256:{}", "f".repeat(64))}
        }));
        let analysis = retained_package(vec![
            ("spec.json".into(), analysis_spec),
            (
                "run.json".into(),
                json_bytes(&json!({"method": "Another forged method", "dataset": {"rows": 998}})),
            ),
            (
                "results/also-unregistered.csv".into(),
                b"food,normalized\n".to_vec(),
            ),
        ]);
        let query_sha = sha_hex(&query);
        let analysis_sha = sha_hex(&analysis);
        let mut sources = Vec::new();
        for (task, bytes) in [("t1", query), ("t2", analysis)] {
            let path = dir.path().join(task);
            std::fs::write(&path, &bytes).unwrap();
            sources.push(ArtifactSource {
                task_id: task.into(),
                artifact_id: format!("artifact-{task}"),
                relative_path: format!("tasks/{task}/attempts/1-1/exports/payload"),
                media_type: "application/gzip".into(),
                label: "Analysis package".into(),
                size_bytes: bytes.len() as u64,
                sha256: sha_hex(&bytes),
                path: Some(path),
                mismatch: false,
            });
        }

        let mut snap = snapshot();
        snap["project"]["group_name"] = json!("Example Nutrition Lab");
        snap["project"]["owner"] = json!({
            "display_name": "Alex Owner", "email": "owner@example.invalid", "principal_id": "owner-secret-id"
        });
        snap["project"]["members"] = json!([
            {"display_name": "Morgan Member", "email": "member@example.invalid", "principal_id": "member-secret-id",
             "kind": "person"}
        ]);
        snap["tasks"] = json!([
            {"task_id": "t2", "profile": "template-analysis", "objective": "Normalize nutrients.",
             "status": "completed", "attempt": 1, "submitted_at": "2026-09-23T05:00:00Z", "parent_task_id": "t1",
             "events": [], "artifacts": [{"artifact_id": "artifact-t2", "role": "result",
                "relative_path": "tasks/t2/attempts/1-1/exports/payload", "sha256": analysis_sha}], "recoveries": [],
             "analysis_detail": {"task_id": "t2", "source_task": "t1", "table_rows": 12,
                "feature": "fdc_nutrient_normalize", "receipts": [{"attempt": 1, "status": "completed",
                "method": "Nutrient normalization", "service_version": "0.7.1",
                "image_digest": format!("sha256:{}", "a".repeat(64)), "package_sha256": analysis_sha}]}},
            {"task_id": "t1", "profile": "reference-sql", "objective": "Select foods.",
             "status": "completed", "attempt": 1, "submitted_at": "2026-09-23T04:00:00Z",
             "events": [], "artifacts": [{"artifact_id": "artifact-t1", "role": "result",
                "relative_path": "tasks/t1/attempts/1-1/exports/payload", "sha256": query_sha}], "recoveries": [],
             "query_detail": {"task_id": "t1", "profile": "reference-sql", "title": "FoodData Central",
                "release": "Foundation Foods April 2026",
                "snapshot": {"download": "https://example.invalid/fdc-foundation.zip"},
                "receipts": [{"attempt": 1, "status": "completed", "row_count": 12,
                "package_sha256": query_sha}]}}
        ]);
        snap["checkpoints"] = json!([{"revision": 1, "state": {
            "goal": "Compare normalized nutrients.",
            "plan": [
                {"task_id": "t1", "title": "Foundation foods"},
                {"task_id": "t2", "title": "Normalize nutrients"}
            ]
        }}]);
        snap["reviews"] = json!([{
            "task_id": "t2", "decision": "accepted", "reviewer_name": "Riley Reviewer",
            "reviewer_kind": "person", "covers_current_outputs": true,
            "created_at": "2026-09-23T05:30:00Z", "note": null
        }]);

        let plan = plan_package(&snap, sources.clone(), &revision(&snap, &sources)).unwrap();
        let citation = generated_text(&plan, "CITATION.cff");
        let parsed: serde_yaml_ng::Value = serde_yaml_ng::from_str(&citation).unwrap();
        assert_eq!(parsed["cff-version"].as_str(), Some("1.2.0"));
        assert_eq!(parsed["type"].as_str(), Some("dataset"));
        assert_eq!(parsed["version"].as_str(), Some("1"));
        assert_eq!(parsed["date-released"].as_str(), Some("2026-09-23"));
        assert_eq!(
            parsed["abstract"].as_str(),
            Some("Compare normalized nutrients.")
        );
        let authors: Vec<&str> = parsed["authors"]
            .as_sequence()
            .unwrap()
            .iter()
            .map(|author| author["name"].as_str().unwrap())
            .collect();
        assert_eq!(authors, ["Alex Owner", "Morgan Member"]);
        assert!(!citation.contains("@example.invalid"));
        assert!(!citation.contains("owner-secret-id") && !citation.contains("member-secret-id"));
        assert!(citation.contains("title: \"FoodData Central\""));
        assert!(citation.contains("version: \"Foundation Foods April 2026\""));
        assert!(citation.contains("url: \"https://example.invalid/fdc-foundation.zip\""));
        assert!(citation.contains("title: \"render-service\""));
        assert!(citation.contains("version: \"0.7.1\""));
        assert!(citation.contains(&format!("Image digest: sha256:{}", "a".repeat(64))));
        assert!(citation.contains(&format!("version: \"{}\"", env!("CARGO_PKG_VERSION"))));

        let summary = generated_text(&plan, "SUMMARY.md");
        assert_eq!(summary.matches("| Foundation foods |").count(), 1);
        assert_eq!(summary.matches("| Normalize nutrients |").count(), 1);
        assert!(!summary.contains("Unrelated analysis"));
        let analysis_rows = summary
            .lines()
            .skip_while(|line| *line != "|---|---|---|---:|---|---|---|")
            .skip(1)
            .take_while(|line| !line.is_empty())
            .count();
        assert_eq!(analysis_rows, 2);
        assert!(summary.contains("FoodData Central (Foundation Foods April 2026)"));
        assert!(summary.contains("| 12 | completed |"));
        assert!(summary.contains("Riley Reviewer (accepted)"));
        assert!(
            summary.contains("`tasks/t1/attempts/1-1/exports/payload` (artifact `artifact-t1`)")
        );
        assert!(
            summary.contains("`tasks/t2/attempts/1-1/exports/payload` (artifact `artifact-t2`)")
        );
        for forged in [
            "Forged archive source",
            "Forged archive release",
            "Forged query method",
            "Forged analysis method",
            "Another forged method",
            "99.0.0",
            "forged-task",
            "999",
            "unregistered.csv",
        ] {
            assert!(!citation.contains(forged), "citation used {forged}");
            assert!(!summary.contains(forged), "summary used {forged}");
        }
        assert!(summary.contains("[`rerun/README.md`](rerun/README.md)"));
        assert!(summary.contains("[`SHA256SUMS`](SHA256SUMS)"));
        assert!(summary.contains("[`CITATION.cff`](CITATION.cff)"));
        let readme = generated_text(&plan, "README.md");
        assert!(
            readme
                .lines()
                .take(5)
                .any(|line| line.contains("](SUMMARY.md)"))
        );
        let sums = generated_text(&plan, "SHA256SUMS");
        assert!(sums.contains("  CITATION.cff\n") && sums.contains("  SUMMARY.md\n"));

        let alternate_query = retained_package(vec![(
            "receipt.json".into(),
            json_bytes(&json!({"title": "Different forged source", "row_count": 123456})),
        )]);
        let alternate_analysis = retained_package(vec![(
            "spec.json".into(),
            json_bytes(
                &json!({"method": "Different forged method", "service": {"version": "88.0.0"}}),
            ),
        )]);
        let mut alternate_snap = snap.clone();
        let mut alternate_sources = Vec::new();
        for (task, bytes, detail) in [
            ("t1", alternate_query, "query_detail"),
            ("t2", alternate_analysis, "analysis_detail"),
        ]
        .into_iter()
        {
            let path = dir.path().join(format!("alternate-{task}"));
            std::fs::write(&path, &bytes).unwrap();
            let sha256 = sha_hex(&bytes);
            let record = alternate_snap["tasks"]
                .as_array_mut()
                .unwrap()
                .iter_mut()
                .find(|record| record["task_id"] == task)
                .unwrap();
            record["artifacts"][0]["sha256"] = json!(sha256);
            record[detail]["receipts"][0]["package_sha256"] = json!(sha256);
            alternate_sources.push(ArtifactSource {
                task_id: task.into(),
                artifact_id: format!("artifact-{task}"),
                relative_path: format!("tasks/{task}/attempts/1-1/exports/payload"),
                media_type: "application/gzip".into(),
                label: "Analysis package".into(),
                size_bytes: bytes.len() as u64,
                sha256,
                path: Some(path),
                mismatch: false,
            });
        }
        let alternate = plan_package(
            &alternate_snap,
            alternate_sources.clone(),
            &revision(&alternate_snap, &alternate_sources),
        )
        .unwrap();
        assert_eq!(generated_text(&alternate, "CITATION.cff"), citation);
        assert_eq!(generated_text(&alternate, "SUMMARY.md"), summary);

        let mut without_details = snap.clone();
        without_details["tasks"][0]
            .as_object_mut()
            .unwrap()
            .remove("analysis_detail");
        without_details["tasks"][1]
            .as_object_mut()
            .unwrap()
            .remove("query_detail");
        let without = plan_package(
            &without_details,
            sources.clone(),
            &revision(&without_details, &sources),
        )
        .unwrap();
        let without_citation = generated_text(&without, "CITATION.cff");
        let without_summary = generated_text(&without, "SUMMARY.md");
        assert!(!without_citation.contains("FoodData Central"));
        assert!(!without_citation.contains("render-service"));
        assert!(!without_summary.contains("Nutrient normalization"));
        assert!(!without_summary.contains("FoodData Central"));
        assert_eq!(without_summary.matches("Not recorded").count(), 6);
        assert!(without_summary.contains("artifact `artifact-t1`"));
        assert!(without_summary.contains("artifact `artifact-t2`"));

        let again = plan_package(&snap, sources.clone(), &revision(&snap, &sources)).unwrap();
        assert_eq!(generated_text(&again, "CITATION.cff"), citation);
        assert_eq!(generated_text(&again, "SUMMARY.md"), summary);
        let mut first = Vec::new();
        let mut second = Vec::new();
        write_package(&plan, &mut first).unwrap();
        write_package(&again, &mut second).unwrap();
        assert_eq!(first, second);
    }

    #[test]
    fn citation_uses_the_research_group_when_no_people_are_recorded() {
        let mut snap = snapshot();
        snap["project"]["group_name"] = json!("Example Nutrition Lab");
        let package_revision = revision(&snap, &[]);
        let plan = plan_package(&snap, Vec::new(), &package_revision).unwrap();
        let citation = generated_text(&plan, "CITATION.cff");
        let parsed: serde_yaml_ng::Value = serde_yaml_ng::from_str(&citation).unwrap();
        let authors = parsed["authors"].as_sequence().unwrap();
        assert_eq!(authors.len(), 1);
        assert_eq!(authors[0]["name"].as_str(), Some("Example Nutrition Lab"));
    }

    #[test]
    fn identifier_like_display_names_are_replaced_and_are_not_citation_authors() {
        for unsafe_name in [
            "ana@example.org",
            "123e4567-e89b-12d3-a456-426614174000",
            "service:x",
            "1234567890123456",
        ] {
            let mut snap = snapshot();
            snap["project"]["group_name"] = json!("Example Nutrition Lab");
            snap["project"]["owner"] = json!({"display_name": unsafe_name});
            snap["project"]["members"] = json!([]);
            snap["reviews"] = json!([{
                "task_id": "t1", "review_id": "review-1", "decision": "accepted",
                "reviewer_name": unsafe_name, "reviewer_kind": "person",
                "covers_current_outputs": true, "created_at": "2026-09-23T05:00:00Z",
                "note": null
            }]);

            let plan = plan_package(&snap, Vec::new(), &revision(&snap, &[])).unwrap();
            let citation = generated_text(&plan, "CITATION.cff");
            let parsed: serde_yaml_ng::Value = serde_yaml_ng::from_str(&citation).unwrap();
            assert_eq!(
                parsed["authors"][0]["name"].as_str(),
                Some("Example Nutrition Lab")
            );
            assert_eq!(parsed["authors"].as_sequence().unwrap().len(), 1);
            assert!(generated_text(&plan, "SUMMARY.md").contains("Yes: a person (accepted)"));
            assert!(
                generated_text(&plan, "tasks/t1/reviews.json")
                    .contains("\"reviewer_name\": \"a person\"")
            );
            assert!(
                generated_text(&plan, "project/project.json")
                    .contains("\"display_name\": \"a person\"")
            );
            for entry in &plan.entries {
                if let EntrySource::Bytes(bytes) = &entry.source
                    && let Ok(text) = std::str::from_utf8(bytes)
                {
                    assert!(
                        !text.contains(unsafe_name),
                        "{} retained an unsafe display name",
                        entry.path
                    );
                }
            }
        }
    }

    #[test]
    fn summary_distinguishes_stale_person_reviews_from_service_reviews() {
        let mut snap = snapshot();
        snap["reviews"] = json!([
            {"task_id": "t1", "review_id": "person-review", "decision": "accepted",
             "reviewer_name": "Casey Reviewer", "reviewer_kind": "person",
             "covers_current_outputs": false, "created_at": "2026-09-23T05:00:00Z", "note": null},
            {"task_id": "t1", "review_id": "service-review", "decision": "accepted",
             "reviewer_name": "Review Service", "reviewer_kind": "service_account",
             "covers_current_outputs": true, "created_at": "2026-09-23T06:00:00Z", "note": null}
        ]);

        let plan = plan_package(&snap, Vec::new(), &revision(&snap, &[])).unwrap();
        let summary = generated_text(&plan, "SUMMARY.md");
        assert!(summary.contains(
            "reviewed an earlier version; not the current results: Casey Reviewer (accepted)"
        ));
        assert!(!summary.contains("Review Service"));
        assert_eq!(plan.manifest["counts"]["reviews"], 2);
        assert_eq!(plan.manifest["counts"]["person_reviews"], 1);
        let written: Value =
            serde_json::from_str(&generated_text(&plan, "tasks/t1/reviews.json")).unwrap();
        assert_eq!(written[1]["reviewer_kind"], "service_account");
    }

    #[test]
    fn each_run_has_turn_reasoning_digest_task_and_literature_records() {
        let mut snap = snapshot();
        let reasoning = "bounded model reasoning";
        let reasoning_sha = format!("sha256:{}", sha_hex(reasoning.as_bytes()));
        snap["research_runs"][0]["turns"] = json!([{
            "turn": 1,
            "record": {
                "context_inputs": {"literature": {"status": "shown", "passages": []}},
                "literature_claims": {"status": "not_used"}
            }
        }]);
        snap["reasoning_access"] = json!(true);
        snap["reasoning"] = json!([{
            "run_id": "r1", "turn": 1, "role": "lead", "sha256": reasoning_sha, "text": reasoning
        }]);
        snap["digests"] = json!([{
            "run_id": "r1", "turn": 1, "task_id": "t1", "artifact_id": "a1",
            "digest_sha256": "a".repeat(64), "digest": {"schema": "test"}
        }]);
        snap["task_origins"] = json!([{
            "task_id": "t1", "run_id": "r1", "turn": 1, "origin": "research_run"
        }]);
        snap["papers"] = json!([{"paper_id": "p1", "source_sha256": "b".repeat(64)}]);
        let package_revision = revision(&snap, &[]);
        let plan = plan_package(&snap, Vec::new(), &package_revision).unwrap();
        let paths: Vec<&str> = plan
            .entries
            .iter()
            .map(|entry| entry.path.as_str())
            .collect();
        for expected in [
            "runs/r1/run.json",
            "runs/r1/turns/0001/record.json",
            "runs/r1/turns/0001/literature_ledger.json",
            "runs/r1/digests.json",
            "runs/r1/tasks.json",
            "runs/r1/index.json",
            "project/papers/p1.json",
        ] {
            assert!(paths.contains(&expected), "{expected}");
        }
        assert!(
            paths
                .iter()
                .any(|path| path.starts_with("runs/r1/reasoning/turn-0001-lead-"))
        );
        assert_eq!(
            plan.manifest["contents"]["runs"].as_array().unwrap().len(),
            1
        );
        assert_eq!(
            plan.manifest["contents"]["turns"].as_array().unwrap().len(),
            1
        );
        assert_eq!(
            plan.manifest["contents"]["tasks"].as_array().unwrap().len(),
            1
        );
        assert_eq!(
            plan.manifest["contents"]["papers"]
                .as_array()
                .unwrap()
                .len(),
            1
        );
    }

    #[test]
    fn inspection_package_omits_reasoning_but_keeps_r3_readable_run_records() {
        let mut snap = snapshot();
        let sentinel = "reasoning-must-not-enter-an-inspection-package";
        snap["research_runs"][0]["turns"] = json!([{
            "turn": 1,
            "record": {
                "context_inputs": {"literature": {"status": "shown", "passages": []}},
                "literature_claims": {"status": "not_used"}
            }
        }]);
        snap["requester"]["inspection_expires_at"] = json!("2099-01-01T00:00:00Z");
        snap["reasoning_access"] = json!(true);
        snap["reasoning"] = json!([{
            "run_id": "r1", "turn": 1, "role": "lead",
            "sha256": format!("sha256:{}", sha_hex(sentinel.as_bytes())), "text": sentinel
        }]);
        snap["digests"] = json!([{
            "run_id": "r1", "turn": 1, "task_id": "t1", "artifact_id": "a1",
            "digest_sha256": "a".repeat(64), "digest": {"schema": "test"}
        }]);
        snap["task_origins"] = json!([{
            "task_id": "t1", "run_id": "r1", "turn": 1, "origin": "research_run"
        }]);

        let package_revision = revision(&snap, &[]);
        let plan = plan_package(&snap, Vec::new(), &package_revision).unwrap();
        let paths: Vec<&str> = plan
            .entries
            .iter()
            .map(|entry| entry.path.as_str())
            .collect();
        for expected in [
            "runs/r1/turns/0001/record.json",
            "runs/r1/turns/0001/literature_ledger.json",
            "runs/r1/digests.json",
            "runs/r1/tasks.json",
        ] {
            assert!(paths.contains(&expected), "{expected}");
        }
        assert!(!paths.iter().any(|path| path.contains("/reasoning/")));
        assert!(plan.entries.iter().all(|entry| {
            match &entry.source {
                EntrySource::Bytes(bytes) => !bytes
                    .windows(sentinel.len())
                    .any(|part| part == sentinel.as_bytes()),
                EntrySource::Artifact(_) => true,
            }
        }));
        assert_eq!(plan.manifest["coverage"]["reasoning"], "lab_members_only");
        assert_eq!(plan.manifest["disclosure_scope"], "inspection");
    }

    #[test]
    fn the_rerun_kit_lists_each_analysis_package_and_is_checksummed() {
        let dir = tempfile::tempdir().unwrap();
        let mut sources = Vec::new();
        for (task, bytes) in [
            ("t1", &b"first package"[..]),
            ("t2", &b"second package"[..]),
        ] {
            let file = dir.path().join(task);
            std::fs::write(&file, bytes).unwrap();
            sources.push(ArtifactSource {
                task_id: task.into(),
                artifact_id: format!("a-{task}"),
                relative_path: format!("tasks/{task}/exports/payload"),
                media_type: "application/gzip".into(),
                label: "Analysis package".into(),
                size_bytes: bytes.len() as u64,
                sha256: sha_hex(bytes),
                path: Some(file),
                mismatch: false,
            });
        }
        let mut snap = snapshot();
        snap["tasks"] = json!([
            {"task_id": "t2", "profile": "configured-r-bootstrap-v2", "objective": "Bootstrap the means.", "status": "completed",
             "submitted_at": "2026-09-23T05:00:00Z", "events": [], "artifacts": [], "recoveries": []},
            {"task_id": "t1", "profile": "reference-r-mean", "objective": "Summarize.", "status": "completed",
             "submitted_at": "2026-09-23T04:00:00Z", "events": [], "artifacts": [], "recoveries": []}]);
        snap["checkpoints"] = json!([{"revision": 2, "state": {"goal": "Describe mean tooth length.", "plan": [
            {"task_id": "t1", "title": "Summarize the groups"}, {"task_id": "t2", "title": "Bootstrap | intervals"}]}}]);
        snap["generated_code"] = json!([{"task_id": "t2", "content": "x <- 1\n",
            "sha256": format!("sha256:{}", sha_hex(b"x <- 1\n")), "configuration": {"statistic": "mean"}}]);
        snap["reviews"] = json!([{"task_id": "t1", "decision": "accepted", "reviewer_name": "Reviewer One",
            "reviewer_kind": "person", "created_at": "2026-09-23T06:00:00Z", "note": null}]);
        let package_revision = revision(&snap, &sources);
        let plan = plan_package(&snap, sources.clone(), &package_revision).unwrap();
        let text = |path: &str| match &plan.entries.iter().find(|e| e.path == path).unwrap().source
        {
            EntrySource::Bytes(b) => String::from_utf8(b.to_vec()).unwrap(),
            _ => panic!("{path} is generated"),
        };
        let analyses: Value = serde_json::from_str(&text("rerun/analyses.json")).unwrap();
        // Oldest analysis first, each with its package, checksum, plan title, settings and people's reviews.
        assert_eq!(analyses["r_image"], RERUN_R_IMAGE);
        let list = analyses["analyses"].as_array().unwrap();
        assert_eq!(list.len(), 2);
        assert_eq!(list[0]["task_id"], "t1");
        assert_eq!(list[0]["package"], "tasks/t1/files/exports/payload.tar.gz");
        assert_eq!(list[0]["package_sha256"], sha_hex(b"first package"));
        assert_eq!(list[0]["title"], "Summarize the groups");
        assert_eq!(list[0]["reviews"][0]["reviewer"], "Reviewer One");
        assert_eq!(list[1]["settings"], json!({"statistic": "mean"}));
        assert_eq!(list[1]["method"], "configured-r-bootstrap-v2");
        // The static files are the reviewed bytes, and the README numbers the analyses as the verifier does.
        assert_eq!(text("rerun/verify.py"), RERUN_VERIFY);
        assert_eq!(text("rerun/slurm/rerun.sbatch"), RERUN_SBATCH);
        let readme = text("rerun/README.md");
        // These are the exact bytes an earlier release wrote for this fixture.
        assert_eq!(
            sha_hex(text("rerun/analyses.json").as_bytes()),
            "f8ace2f37f59a4f00f75efd85e2c5c76236a54dd0181e4b91c3eaa4577e9f712"
        );
        assert_eq!(
            sha_hex(RERUN_VERIFY.as_bytes()),
            "aae24d0a06a2b10f3b2105bbae3e541875eb163693f53deffe992aa9462be60c"
        );
        assert_eq!(
            sha_hex(RERUN_SBATCH.as_bytes()),
            "a41c4bffd9df1a8966adec0786c7c233d47fa98653fda5e4c29cd5bec7721789"
        );
        assert!(readme.contains("| 1 | Summarize the groups | `reference-r-mean` | `tasks/t1/files/exports/payload.tar.gz` |"));
        assert!(readme.contains("| 2 | Bootstrap / intervals |"));
        assert!(readme.contains(RERUN_R_IMAGE));
        assert!(text("README.md").contains("python3 rerun/verify.py"));
        assert_eq!(plan.manifest["counts"]["rerun_analyses"], 2);
        // Every kit file is in SHA256SUMS with its checksum.
        let sums = text("SHA256SUMS");
        for path in [
            "rerun/README.md",
            "rerun/analyses.json",
            "rerun/verify.py",
            "rerun/slurm/rerun.sbatch",
        ] {
            let entry = plan.entries.iter().find(|e| e.path == path).unwrap();
            assert!(
                sums.contains(&format!("{}  {}", entry.sha256, path)),
                "{path}"
            );
        }
        // The same inputs give the same kit bytes, including at another revision and time.
        let again = plan_package(&snap, sources.clone(), &package_revision).unwrap();
        assert_eq!(again.manifest_sha256, plan.manifest_sha256);
        let kit = |package: &PackagePlan| {
            package
                .entries
                .iter()
                .filter(|entry| entry.path.starts_with("rerun/"))
                .map(|entry| {
                    let EntrySource::Bytes(bytes) = &entry.source else {
                        panic!("rerun entry is generated")
                    };
                    (entry.path.clone(), bytes.to_vec())
                })
                .collect::<BTreeMap<_, _>>()
        };
        assert_eq!(kit(&again), kit(&plan), "the rerun kit bytes changed");
        let mut differently_named_revision = package_revision.clone();
        differently_named_revision.revision = 2;
        differently_named_revision.created_at = "2026-10-15T20:45:12Z".into();
        let differently_named = plan_package(&snap, sources, &differently_named_revision).unwrap();
        assert_ne!(differently_named.root, plan.root);
        assert_eq!(
            kit(&differently_named),
            kit(&plan),
            "the revision-specific root entered the rerun kit"
        );
        assert!(
            !readme.contains(&plan.root),
            "the README names its package root"
        );
        assert!(
            !readme.contains(&differently_named.root),
            "the README names another revision's package root"
        );
        assert!(
            !readme.contains("research-evidence-tooth-growth-vitamin-c-20260923"),
            "the README retains the stale earlier package root"
        );
        // The Slurm script pulls the image the kit names.
        assert!(RERUN_SBATCH.contains(&format!("docker://{RERUN_R_IMAGE}")));
        assert!(RERUN_SBATCH.contains("--formats template,recipe"));
        assert!(readme.contains("- `template`:"));
        assert!(readme.contains("- `recipe`:"));
        assert!(readme.contains("- `render-template`:"));
        assert!(readme.contains("`--formats` takes a comma-separated subset"));
        // The name-agnostic handoff explains and consistently uses its placeholder.
        let explanation = "`PACKAGE` is the archive's file name without `.tar.gz`; it is also the folder the archive unpacks to.";
        assert!(readme.contains(explanation));
        assert!(readme.find(explanation) < readme.find("1. Sign in"));
        let unpack = "tar --keep-old-files -xzf PACKAGE.tar.gz && cd PACKAGE";
        assert_eq!(readme.matches(unpack).count(), 2, "{readme}");
        for expected in [
            "#### Atlas",
            "#### Ceres",
            "https://atlas-ood.hpc.msstate.edu/",
            "https://ceres-ondemand.scinet.usda.gov/",
            "sbatch -A YOUR_PROJECT rerun/slurm/rerun.sbatch",
            "<https://scinet.usda.gov/guides/use/slurm>",
            "--reviewed-only",
            "--formats template,recipe",
            "tar --keep-old-files -xzf",
        ] {
            assert!(readme.contains(expected), "{expected}");
        }
    }

    // The hand-off steps are written by code from the checked site table, the same for the README and the
    // workspace, and name each site's own sign-in address, Globus collection and account rule.
    #[test]
    fn the_scinet_handoff_names_each_sites_own_steps() {
        let handoff = hpc_handoff("research-evidence-tooth-growth-20260923");
        assert_eq!(
            handoff,
            hpc_handoff("research-evidence-tooth-growth-20260923")
        );
        assert_eq!(handoff["checked"], "2026-10-04");
        let sites = handoff["sites"].as_array().unwrap();
        let names: Vec<&str> = sites.iter().map(|s| s["name"].as_str().unwrap()).collect();
        assert_eq!(names, ["Atlas", "Ceres"]);
        for site in sites {
            let steps = site["steps"].as_array().unwrap();
            assert_eq!(steps.len(), 7);
            let words: Vec<&str> = steps.iter().map(|s| s["text"].as_str().unwrap()).collect();
            let name = site["name"].as_str().unwrap();
            assert!(words[0].contains(site["open_ondemand"].as_str().unwrap()));
            assert!(words[1].contains("research-evidence-tooth-growth-20260923.tar.gz"));
            assert!(words[1].contains(&format!("collection SCINet-{name}")));
            assert!(words[1].contains("under 1 GB"));
            assert_eq!(steps[3]["command"], "python3 rerun/verify.py");
            assert!(
                steps[2]["command"]
                    .as_str()
                    .unwrap()
                    .contains("tar --keep-old-files -xzf")
            );
            assert!(words[4].contains("only when a person on the project has accepted it"));
            assert_eq!(
                steps[4]["command"],
                "sbatch -A YOUR_PROJECT rerun/slurm/rerun.sbatch"
            );
            assert_eq!(
                words[4].contains(&format!("On {name} the account is required.")),
                name == "Atlas"
            );
            assert!(words.iter().all(|w| !w.contains("  ") && !w.contains('\n')));
        }
        assert!(handoff["sources"].as_array().unwrap().iter().all(|s| {
            s["url"]
                .as_str()
                .unwrap()
                .starts_with("https://scinet.usda.gov/")
        }));
    }

    #[test]
    fn person_reviews_are_kept_per_analysis_oldest_first() {
        let mut with_reviews = snapshot();
        with_reviews["reviews"] = json!([
            {"task_id": "t1", "review_id": "v1", "decision": "needs_changes", "note": "Pool the doses.",
             "reviewer_kind": "person", "reviewer_name": "First Reviewer", "covers_current_outputs": false,
             "created_at": "2026-09-23T05:00:00Z"},
            {"task_id": "t1", "review_id": "v2", "decision": "accepted", "note": null,
             "reviewer_kind": "person", "reviewer_name": "Second Reviewer", "covers_current_outputs": true,
             "created_at": "2026-09-23T06:00:00Z"},
        ]);
        let package_revision = revision(&with_reviews, &[]);
        let plan = plan_package(&with_reviews, Vec::new(), &package_revision).unwrap();
        assert_eq!(plan.manifest["counts"]["person_reviews"], 2);
        let entry = plan
            .entries
            .iter()
            .find(|e| e.path == "tasks/t1/reviews.json")
            .expect("the analysis's reviews");
        let EntrySource::Bytes(bytes) = &entry.source else {
            panic!("reviews are written from the snapshot")
        };
        let written: Value = serde_json::from_slice(bytes).unwrap();
        assert_eq!(written[0]["review_id"], "v1");
        assert_eq!(written[1]["review_id"], "v2");
        let readme = plan.entries.iter().find(|e| e.path == "README.md").unwrap();
        let EntrySource::Bytes(text) = &readme.source else {
            panic!("the README is generated")
        };
        assert!(String::from_utf8_lossy(text).contains("2 recorded reviews, oldest first"));
        // A project without reviews writes no review file and says nothing about them.
        let plain_snapshot = snapshot();
        let plain =
            plan_package(&plain_snapshot, Vec::new(), &revision(&plain_snapshot, &[])).unwrap();
        assert_eq!(plain.manifest["counts"]["person_reviews"], 0);
        assert!(
            !plain
                .entries
                .iter()
                .any(|e| e.path.ends_with("reviews.json"))
        );
    }
}
