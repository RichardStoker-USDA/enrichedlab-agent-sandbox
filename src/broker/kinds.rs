//! Result kinds. The registry is `kinds/*.yaml`, embedded when the broker is built and checked when it
//! loads; its digest names exactly which registry checked a file. A kind is recognized from a file's media type and
//! accepted only after its validator reads the stored bytes: recognition never trusts a file name alone, and a file
//! that fails its check, or matches no accepted kind, is kept as `file.opaque` with the reason.
//!
//! A declared kind, such as `figure.r`, is never recognized from a media type. The broker path or reviewed method that
//! wrote a file declares it, bound to the file's path and SHA-256, and the file is still checked by the kind that owns
//! its media type; the declaration only names what the checked bytes are.
//!
//! Checks are incremental, so a stored file and a package member are read once through the same code while their
//! SHA-256 is computed. Every check is deterministic; its receipt names this routine, its version, the registry digest,
//! the kind checked and the input digest, so anyone can repeat it.
use std::{
    collections::{BTreeMap, BTreeSet},
    fs::OpenOptions,
    io::{self, Read, Write},
    os::unix::fs::OpenOptionsExt,
    path::PathBuf,
    sync::LazyLock,
};

use serde::{Deserialize, Serialize, de::IgnoredAny};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

use super::{canonical::canonical_json, evidence_package, tables};

include!(concat!(env!("OUT_DIR"), "/kind_files.rs"));

pub const REGISTRY_FORMAT: u32 = 1;
pub const OPAQUE: &str = "file.opaque";
pub const CHECK_ROUTINE: &str = "enrichedlab.kind-check";
pub const CHECK_VERSION: u32 = 1;

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum Status {
    Candidate,
    Accepted,
    Retired,
}

/// How a file comes to hold a kind.
#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum Recognition {
    /// From the file's media type, then the kind's validator.
    #[default]
    MediaType,
    /// From a declaration by the broker path or reviewed method that wrote the file; the kind in `checked_as` that
    /// owns the file's media type checks it.
    Declared,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum Access {
    Whole,
    Range,
    Paged,
    Live,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum Validator {
    DelimitedTable,
    RasterImage,
    SvgDocument,
    PdfDocument,
    JsonDocument,
    NdjsonDocument,
    Utf8Text,
    ResultPackage,
    None,
    /// A declared kind's files are checked by the kind in `checked_as` that owns their media type.
    Delegated,
}

impl Validator {
    /// The media types this validator can check, and the limits it reads besides `bytes`.
    fn supports(self) -> (&'static [&'static str], &'static [&'static str]) {
        match self {
            Self::DelimitedTable => (
                &["text/csv", "text/tab-separated-values"],
                &["rows", "columns", "cells"],
            ),
            Self::RasterImage => (&["image/png", "image/jpeg", "image/gif", "image/webp"], &[]),
            Self::SvgDocument => (&["image/svg+xml"], &[]),
            Self::PdfDocument => (&["application/pdf"], &[]),
            Self::JsonDocument => (&["application/json"], &[]),
            Self::NdjsonDocument => (&["application/x-ndjson"], &["line_bytes"]),
            Self::Utf8Text => (&["text/plain", "text/markdown"], &[]),
            Self::ResultPackage => (&["application/gzip"], &[]),
            Self::None | Self::Delegated => (&[], &[]),
        }
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum Gpu {
    Webgpu,
    Webgl2,
    Still,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct View {
    pub view: String,
    pub gpu: Vec<Gpu>,
    pub fallback: String,
    /// The media types this view draws, when it draws only some of the kind's.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub media_types: Vec<String>,
    /// The channels a declaration maps to the file's columns before this view can draw it, such as x, y and z.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub encoding: Vec<String>,
}

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum Expect {
    Passed,
    Failed,
}

/// A small synthetic file that the kind's tests check; never research data.
#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Fixture {
    pub path: String,
    pub media_type: String,
    pub expect: Expect,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Kind {
    pub id: String,
    pub version: u32,
    pub status: Status,
    pub owner: String,
    pub summary: String,
    #[serde(default)]
    pub recognition: Recognition,
    /// For a declared kind: the kinds that check its files, one for each of its media types.
    #[serde(default)]
    pub checked_as: Vec<String>,
    pub media_types: Vec<String>,
    pub validator: Validator,
    pub access: Access,
    pub limits: BTreeMap<String, u64>,
    pub views: Vec<View>,
    #[serde(default)]
    pub fixtures: Vec<Fixture>,
}

impl Kind {
    pub fn profile_uri(&self) -> String {
        format!("urn:enrichedlab:kind:{}:{}", self.id, self.version)
    }

    pub fn reference(&self) -> Value {
        json!({"id": self.id, "version": self.version})
    }

    fn public(&self) -> Value {
        let mut public = json!({
            "id": self.id, "version": self.version, "status": self.status, "profile_uri": self.profile_uri(),
            "summary": self.summary, "recognition": self.recognition, "media_types": self.media_types,
            "validator": self.validator, "access": self.access, "limits": self.limits, "views": self.views,
        });
        if self.recognition == Recognition::Declared {
            public["checked_as"] = json!(self.checked_as);
        }
        public
    }
}

pub struct Registry {
    pub kinds: Vec<Kind>,
    /// `sha256:` and the SHA-256 of the kinds' public form in RFC 8785 canonical JSON.
    pub digest: String,
    by_media: BTreeMap<String, usize>,
    opaque: usize,
    public: Value,
}

/// The media type without parameters, in lower case.
fn essence(media_type: &str) -> String {
    media_type
        .split(';')
        .next()
        .unwrap_or_default()
        .trim()
        .to_ascii_lowercase()
}

fn valid_id(id: &str) -> bool {
    let mut parts = id.split('.');
    let word = |part: &str| {
        part.starts_with(|c: char| c.is_ascii_lowercase())
            && part
                .bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
    };
    id.contains('.') && parts.all(word)
}

fn valid_media_type(media_type: &str) -> bool {
    let token = |part: &str| {
        !part.is_empty()
            && part
                .bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b"!#$&^_.+-".contains(&b))
    };
    matches!(media_type.split_once('/'), Some((kind, sub)) if token(kind) && token(sub))
}

impl Registry {
    /// Loads and checks the registry files (file name, kind fields and the rules between kinds).
    pub fn parse(files: &[(&str, &str)]) -> Result<Self, String> {
        let mut kinds: Vec<Kind> = Vec::new();
        for (name, text) in files {
            let kind: Kind =
                serde_yaml_ng::from_str(text).map_err(|error| format!("{name}: {error}"))?;
            check_kind(name, &kind)?;
            if kinds.iter().any(|other| other.id == kind.id) {
                return Err(format!("{name}: kind {} appears twice", kind.id));
            }
            kinds.push(kind);
        }
        kinds.sort_by(|a, b| a.id.cmp(&b.id));
        let mut by_media = BTreeMap::new();
        for (index, kind) in kinds.iter().enumerate() {
            if kind.status != Status::Accepted || kind.recognition == Recognition::Declared {
                continue;
            }
            for media_type in &kind.media_types {
                if let Some(other) = by_media.insert(media_type.clone(), index) {
                    return Err(format!(
                        "{media_type} belongs to both {} and {}",
                        kinds[other].id, kind.id
                    ));
                }
            }
        }
        for kind in kinds
            .iter()
            .filter(|kind| kind.recognition == Recognition::Declared)
        {
            let checks: Vec<&Kind> = kind
                .checked_as
                .iter()
                .map(|id| {
                    kinds
                        .iter()
                        .find(|other| other.id == *id)
                        .filter(|other| {
                            other.status == Status::Accepted
                                && other.recognition == Recognition::MediaType
                                && !matches!(other.validator, Validator::None)
                        })
                        .ok_or(format!(
                            "{}: {id} is not an accepted kind with a validator",
                            kind.id
                        ))
                })
                .collect::<Result<_, _>>()?;
            for media_type in &kind.media_types {
                let owners = checks
                    .iter()
                    .filter(|check| check.media_types.contains(media_type))
                    .count();
                if owners != 1 {
                    return Err(format!(
                        "{}: {media_type} must belong to exactly one kind it is checked as",
                        kind.id
                    ));
                }
            }
        }
        let opaque = kinds
            .iter()
            .position(|kind| kind.id == OPAQUE)
            .ok_or("the registry has no file.opaque kind")?;
        let fallback = &kinds[opaque];
        if fallback.status != Status::Accepted
            || fallback.validator != Validator::None
            || !fallback.media_types.is_empty()
        {
            return Err("file.opaque must be accepted, unvalidated and claim no media type".into());
        }
        let listed: Vec<Value> = kinds.iter().map(Kind::public).collect();
        let digest = format!(
            "sha256:{}",
            hex::encode(Sha256::digest(canonical_json(&Value::Array(
                listed.clone()
            ))))
        );
        let public =
            json!({"registry": {"digest": digest, "version": REGISTRY_FORMAT}, "kinds": listed});
        Ok(Self {
            kinds,
            digest,
            by_media,
            opaque,
            public,
        })
    }

    pub fn opaque(&self) -> &Kind {
        &self.kinds[self.opaque]
    }

    pub fn kind(&self, id: &str) -> Option<&Kind> {
        self.kinds.iter().find(|kind| kind.id == id)
    }

    /// The accepted kind that claims a media type, or `file.opaque`.
    pub fn recognize(&self, media_type: &str) -> &Kind {
        self.by_media
            .get(&essence(media_type))
            .map_or(self.opaque(), |&index| &self.kinds[index])
    }

    /// `{"registry": {"digest", "version"}, "kinds": [...]}` for `GET /api/v1/kinds`.
    pub fn public(&self) -> &Value {
        &self.public
    }

    /// The declared kind a checked file takes when a trusted declarer names it: the file's media type is one the
    /// declared kind takes, and the file passed its check under the kind that owns that media type. Loading the
    /// registry made that owner one of the kinds the declared kind is checked as. Otherwise none, and the file keeps the
    /// kind its check gave it.
    pub fn declared(
        &self,
        kind_id: &str,
        media_type: &str,
        validation: &MemberValidation,
    ) -> Option<&Kind> {
        let kind = self.kind(kind_id)?;
        let media_type = essence(media_type);
        (kind.status == Status::Accepted
            && kind.recognition == Recognition::Declared
            && validation.state == "passed"
            && kind.media_types.contains(&media_type)
            && self.recognize(&media_type).id == validation.checked_as.id)
            .then_some(kind)
    }
}

/// A trusted declaration that a file holds a declared kind, bound to the file's path and SHA-256. Only broker code that
/// wrote the file, or a reviewed method's output binding, makes one; a file's own contents never do.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Declaration {
    pub kind: &'static str,
    pub path: String,
    pub sha256: String,
}

fn check_kind(name: &str, kind: &Kind) -> Result<(), String> {
    let fail = |message: String| Err(format!("{name}: {message}"));
    if *name != format!("{}.yaml", kind.id) {
        return fail(format!("the file must be named {}.yaml", kind.id));
    }
    if !valid_id(&kind.id) || kind.id.len() > 64 {
        return fail("the id must be dotted lower-case words".into());
    }
    if kind.version == 0 {
        return fail("the version starts at 1".into());
    }
    if kind.owner.trim().is_empty() || kind.summary.trim().is_empty() || kind.summary.len() > 200 {
        return fail("owner and a summary of at most 200 characters are required".into());
    }
    let declared = kind.recognition == Recognition::Declared;
    if declared != (kind.validator == Validator::Delegated)
        || declared == kind.checked_as.is_empty()
    {
        return fail(
            "a declared kind, and only a declared kind, is delegated and names the kinds it is checked as".into(),
        );
    }
    if declared
        && (!kind.limits.is_empty() || !kind.fixtures.is_empty() || kind.media_types.is_empty())
    {
        return fail(
            "a declared kind takes media types but no limits or fixtures; the kinds it is checked as hold those".into(),
        );
    }
    let mut named = BTreeSet::new();
    if !kind
        .checked_as
        .iter()
        .all(|id| valid_id(id) && *id != kind.id && named.insert(id))
    {
        return fail("checked_as names other kinds, each once".into());
    }
    let (media_types, extra_limits) = kind.validator.supports();
    let mut seen = BTreeSet::new();
    for media_type in &kind.media_types {
        if !valid_media_type(media_type) || !seen.insert(media_type) {
            return fail(format!("media type {media_type} is malformed or repeated"));
        }
        if !declared && !media_types.contains(&media_type.as_str()) {
            return fail(format!("{:?} cannot check {media_type}", kind.validator));
        }
    }
    if !matches!(kind.validator, Validator::None | Validator::Delegated)
        && kind.media_types.is_empty()
    {
        return fail("a validated kind claims at least one media type".into());
    }
    for limit in kind.limits.keys() {
        if limit != "bytes" && !extra_limits.contains(&limit.as_str()) {
            return fail(format!("{:?} reads no limit named {limit}", kind.validator));
        }
    }
    if !matches!(kind.validator, Validator::None | Validator::Delegated)
        && !kind.limits.contains_key("bytes")
    {
        return fail("a validated kind sets a bytes limit".into());
    }
    if kind.views.is_empty() {
        return fail("a kind names at least one view".into());
    }
    for view in &kind.views {
        let mut gpus = BTreeSet::new();
        if !valid_id(&format!("view.{}", view.view))
            || view.gpu.last() != Some(&Gpu::Still)
            || !view.gpu.iter().all(|gpu| gpus.insert(format!("{gpu:?}")))
            || view.fallback.trim().is_empty()
        {
            return fail(format!(
                "view {} needs a lower-case name, a GPU preference ending in still and a fallback",
                view.view
            ));
        }
        let mut drawn = BTreeSet::new();
        if !view
            .media_types
            .iter()
            .all(|media_type| kind.media_types.contains(media_type) && drawn.insert(media_type))
        {
            return fail(format!(
                "view {} draws only media types of this kind, each once",
                view.view
            ));
        }
        let mut channels = BTreeSet::new();
        let channel = |name: &str| {
            name.len() <= 32
                && name.starts_with(|c: char| c.is_ascii_lowercase())
                && name
                    .bytes()
                    .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_')
        };
        if view.encoding.len() > 8
            || !view
                .encoding
                .iter()
                .all(|name| channel(name) && channels.insert(name))
        {
            return fail(format!(
                "view {} names at most eight lower-case channels, each once",
                view.view
            ));
        }
    }
    for fixture in &kind.fixtures {
        let prefix = format!("fixtures/{}/", kind.id);
        if !fixture.path.starts_with(&prefix)
            || fixture.path.contains("..")
            || fixture.path.contains("//")
        {
            return fail(format!("fixture {} must sit under {prefix}", fixture.path));
        }
    }
    Ok(())
}

static REGISTRY: LazyLock<Result<Registry, String>> = LazyLock::new(|| Registry::parse(KIND_FILES));

/// The line `check-kinds` prints for a registry that loads: its digest and how many kinds it holds.
pub fn check_line(registry: &Registry) -> Value {
    json!({"registry": registry.digest, "kinds": registry.kinds.len()})
}

/// The registry this binary was built with. The API and worker refuse to start when it does not load.
pub fn registry() -> Result<&'static Registry, String> {
    REGISTRY.as_ref().map_err(Clone::clone)
}

/// What a check found. `checks` names each check that ran, in order.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Outcome {
    pub passed: bool,
    pub checks: Vec<&'static str>,
    pub reason: Option<String>,
}

/// Streaming UTF-8 and NUL check: a sequence split between chunks is carried over.
#[derive(Default)]
struct Utf8Stream {
    carry: Vec<u8>,
    offset: u64,
    error: Option<String>,
}

impl Utf8Stream {
    fn update(&mut self, chunk: &[u8]) {
        if self.error.is_some() {
            return;
        }
        let joined;
        let bytes = if self.carry.is_empty() {
            chunk
        } else {
            joined = [self.carry.as_slice(), chunk].concat();
            joined.as_slice()
        };
        let start = self.offset - self.carry.len() as u64;
        let valid = match std::str::from_utf8(bytes) {
            Ok(_) => bytes.len(),
            Err(error) if error.error_len().is_none() => error.valid_up_to(),
            Err(error) => {
                self.error = Some(format!(
                    "not UTF-8 at byte {}",
                    start + error.valid_up_to() as u64
                ));
                return;
            }
        };
        if let Some(at) = bytes[..valid].iter().position(|&b| b == 0) {
            self.error = Some(format!("a NUL byte at byte {}", start + at as u64));
            return;
        }
        self.carry = bytes[valid..].to_vec();
        self.offset += chunk.len() as u64;
    }

    fn finish(&mut self) -> Option<String> {
        if self.error.is_none() && !self.carry.is_empty() {
            self.error = Some(format!(
                "not UTF-8 at byte {}",
                self.offset - self.carry.len() as u64
            ));
        }
        self.error.take()
    }
}

enum State {
    Buffer(Vec<u8>),
    Text(Utf8Stream),
    Svg {
        text: Utf8Stream,
        head: Vec<u8>,
    },
    Ends {
        head: Vec<u8>,
        tail: Vec<u8>,
    },
    Lines {
        line: Vec<u8>,
        number: u64,
        error: Option<String>,
    },
    Gzip {
        decoder: Box<flate2::write::GzDecoder<Expanded>>,
        error: Option<String>,
    },
    Nothing,
}

/// Counts decompressed bytes and refuses past a limit, so a small package cannot expand without bound.
struct Expanded {
    bytes: u64,
    limit: u64,
}

impl Write for Expanded {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.bytes += buf.len() as u64;
        if self.bytes > self.limit {
            return Err(io::Error::other("expanded past the limit"));
        }
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

const HEAD: usize = 4096;
const TAIL: usize = 2048;

/// One file's check under one kind, fed in chunks.
pub struct Check<'a> {
    kind: &'a Kind,
    media_type: String,
    inside_package: bool,
    size: u64,
    over_limit: bool,
    state: State,
}

impl<'a> Check<'a> {
    pub fn new(kind: &'a Kind, media_type: &str, inside_package: bool) -> Self {
        let state = match kind.validator {
            Validator::DelimitedTable | Validator::JsonDocument => State::Buffer(Vec::new()),
            Validator::Utf8Text => State::Text(Utf8Stream::default()),
            Validator::SvgDocument => State::Svg {
                text: Utf8Stream::default(),
                head: Vec::new(),
            },
            Validator::RasterImage | Validator::PdfDocument => State::Ends {
                head: Vec::new(),
                tail: Vec::new(),
            },
            // A package inside a package is refused without reading it.
            Validator::ResultPackage if inside_package => State::Nothing,
            Validator::ResultPackage => State::Gzip {
                decoder: Box::new(flate2::write::GzDecoder::new(Expanded {
                    bytes: 0,
                    limit: evidence_package::MAX_PACKAGE_EXPANDED_BYTES,
                })),
                error: None,
            },
            Validator::NdjsonDocument => State::Lines {
                line: Vec::new(),
                number: 0,
                error: None,
            },
            Validator::None | Validator::Delegated => State::Nothing,
        };
        Self {
            kind,
            media_type: essence(media_type),
            inside_package,
            size: 0,
            over_limit: false,
            state,
        }
    }

    pub fn kind(&self) -> &'a Kind {
        self.kind
    }

    pub fn update(&mut self, chunk: &[u8]) {
        self.size += chunk.len() as u64;
        if self.over_limit {
            return;
        }
        if self
            .kind
            .limits
            .get("bytes")
            .is_some_and(|&limit| self.size > limit)
        {
            self.over_limit = true;
            self.state = State::Nothing;
            return;
        }
        match &mut self.state {
            State::Buffer(buffer) => buffer.extend_from_slice(chunk),
            State::Text(text) => text.update(chunk),
            State::Svg { text, head } => {
                text.update(chunk);
                let room = HEAD.saturating_sub(head.len());
                head.extend_from_slice(&chunk[..room.min(chunk.len())]);
            }
            State::Ends { head, tail } => {
                let room = HEAD.saturating_sub(head.len());
                head.extend_from_slice(&chunk[..room.min(chunk.len())]);
                tail.extend_from_slice(chunk);
                if tail.len() > TAIL {
                    tail.drain(..tail.len() - TAIL);
                }
            }
            State::Lines {
                line,
                number,
                error,
            } => {
                let limit = self
                    .kind
                    .limits
                    .get("line_bytes")
                    .copied()
                    .unwrap_or(u64::MAX);
                for piece in chunk.split_inclusive(|&b| b == b'\n') {
                    if error.is_some() {
                        return;
                    }
                    line.extend_from_slice(piece);
                    if line.len() as u64 > limit {
                        *error = Some(format!("line {} is longer than {limit} bytes", *number + 1));
                        return;
                    }
                    if piece.ends_with(b"\n") {
                        *number += 1;
                        *error = json_line(line, *number);
                        line.clear();
                    }
                }
            }
            State::Gzip { decoder, error } => {
                let mut rest = chunk;
                while error.is_none() && !rest.is_empty() {
                    match decoder.write(rest) {
                        Ok(0) => *error = Some("bytes follow the first gzip member".into()),
                        Ok(read) => rest = &rest[read..],
                        Err(_) if decoder.header().is_none() => *error = Some("not gzip".into()),
                        Err(_) if decoder.get_ref().bytes > decoder.get_ref().limit => {
                            *error = Some(format!("expands past {} bytes", decoder.get_ref().limit))
                        }
                        Err(_) => *error = Some("the gzip stream is corrupt".into()),
                    }
                }
            }
            State::Nothing => {}
        }
    }

    pub fn finish(self) -> Outcome {
        let fail = |checks: Vec<&'static str>, reason: String| Outcome {
            passed: false,
            checks,
            reason: Some(reason),
        };
        if self.over_limit {
            let limit = self.kind.limits.get("bytes").copied().unwrap_or_default();
            return fail(
                vec!["bytes"],
                format!("larger than this kind's limit of {limit} bytes"),
            );
        }
        let pass = |checks: Vec<&'static str>| Outcome {
            passed: true,
            checks,
            reason: None,
        };
        match (self.kind.validator, self.state) {
            (Validator::None, _) => pass(vec![]),
            (Validator::Delegated, _) => fail(
                vec![],
                "a declared kind is checked as the kind that owns the media type".into(),
            ),
            (Validator::DelimitedTable, State::Buffer(bytes)) => {
                let checks = vec!["bytes", "utf8", "header", "field-count", "limits"];
                let delimiter = tables::delimiter_for(&self.media_type).unwrap_or(b',');
                match tables::parse(&bytes, delimiter, String::new()) {
                    Err(tables::TableError::Unreadable(reason)) => fail(checks, reason),
                    Err(tables::TableError::TooLarge) => {
                        fail(checks, "past the table limits".into())
                    }
                    Err(tables::TableError::NotATable) => fail(checks, "not a table".into()),
                    Ok(table) => {
                        let rows = table.rows() as u64;
                        let columns = table.columns.len() as u64;
                        let past = |name: &str, value: u64| {
                            self.kind
                                .limits
                                .get(name)
                                .is_some_and(|&limit| value > limit)
                        };
                        if past("rows", rows)
                            || past("columns", columns)
                            || past("cells", rows * columns)
                        {
                            fail(
                                checks,
                                format!(
                                    "{rows} rows and {columns} columns pass this kind's limits"
                                ),
                            )
                        } else {
                            pass(checks)
                        }
                    }
                }
            }
            (Validator::JsonDocument, State::Buffer(bytes)) => {
                // Skipping a value does not check that its strings are UTF-8, so the whole document is checked first.
                if let Err(error) = std::str::from_utf8(&bytes) {
                    return fail(
                        vec!["bytes", "json"],
                        format!("not UTF-8 (byte {})", error.valid_up_to()),
                    );
                }
                match serde_json::from_slice::<IgnoredAny>(&bytes) {
                    Ok(_) => pass(vec!["bytes", "json"]),
                    Err(error) => fail(
                        vec!["bytes", "json"],
                        format!(
                            "not one JSON document (line {}, column {})",
                            error.line(),
                            error.column()
                        ),
                    ),
                }
            }
            (Validator::Utf8Text, State::Text(mut text)) => match text.finish() {
                None => pass(vec!["bytes", "utf8"]),
                Some(reason) => fail(vec!["bytes", "utf8"], reason),
            },
            (Validator::SvgDocument, State::Svg { mut text, head }) => {
                let checks = vec!["bytes", "utf8", "root-element"];
                if let Some(reason) = text.finish() {
                    return fail(checks, reason);
                }
                if svg_root(&head) {
                    pass(checks)
                } else {
                    fail(checks, "the first element is not svg".into())
                }
            }
            (
                Validator::NdjsonDocument,
                State::Lines {
                    line,
                    number,
                    error,
                },
            ) => {
                let checks = vec!["bytes", "json-lines"];
                let error = error.or_else(|| json_line(&line, number + 1));
                match error {
                    None => pass(checks),
                    Some(reason) => fail(checks, reason),
                }
            }
            (Validator::RasterImage, State::Ends { head, tail }) => {
                match raster(&self.media_type, &head, &tail, self.size) {
                    Ok(()) => pass(vec!["bytes", "signature", "ending"]),
                    Err(reason) => fail(vec!["bytes", "signature", "ending"], reason),
                }
            }
            (Validator::PdfDocument, State::Ends { head, tail }) => {
                let checks = vec!["bytes", "pdf-header", "pdf-end"];
                let header = head.len() >= 8
                    && head.starts_with(b"%PDF-")
                    && head[5].is_ascii_digit()
                    && head[6] == b'.'
                    && head[7].is_ascii_digit();
                if !header {
                    fail(checks, "no PDF header".into())
                } else if !tail.windows(5).any(|window| window == b"%%EOF") {
                    fail(
                        checks,
                        "no end-of-file marker in the last 2048 bytes".into(),
                    )
                } else {
                    pass(checks)
                }
            }
            (Validator::ResultPackage, _) if self.inside_package => fail(
                vec!["bytes", "gzip"],
                "a package inside a package is not read; download it".into(),
            ),
            (Validator::ResultPackage, State::Gzip { mut decoder, error }) => {
                let checks = vec!["bytes", "gzip"];
                match error {
                    Some(reason) => fail(checks, reason),
                    None if decoder.header().is_none() => fail(checks, "not gzip".into()),
                    None => match decoder.try_finish() {
                        Ok(()) => pass(checks),
                        Err(_) => fail(
                            checks,
                            "the gzip stream ends early or its checksum differs".into(),
                        ),
                    },
                }
            }
            _ => fail(vec![], "the check lost its state".into()),
        }
    }
}

/// `None` when a line is blank or one JSON document.
fn json_line(line: &[u8], number: u64) -> Option<String> {
    let text = line.strip_suffix(b"\n").unwrap_or(line);
    let text = text.strip_suffix(b"\r").unwrap_or(text);
    if text.iter().all(u8::is_ascii_whitespace) {
        return None;
    }
    // Skipping a value does not check that its strings are UTF-8.
    if std::str::from_utf8(text).is_err() {
        return Some(format!("line {number} is not UTF-8"));
    }
    serde_json::from_slice::<IgnoredAny>(text)
        .err()
        .map(|_| format!("line {number} is not one JSON document"))
}

/// After an optional byte-order mark, white space, XML declaration, comments and a document type, the first element
/// is `svg`.
fn svg_root(head: &[u8]) -> bool {
    let text = String::from_utf8_lossy(head);
    let mut rest = text.trim_start_matches('\u{feff}');
    loop {
        rest = rest.trim_start();
        if let Some(after) = rest.strip_prefix("<?") {
            let Some(end) = after.find("?>") else {
                return false;
            };
            rest = &after[end + 2..];
        } else if let Some(after) = rest.strip_prefix("<!--") {
            let Some(end) = after.find("-->") else {
                return false;
            };
            rest = &after[end + 3..];
        } else if rest.starts_with("<!DOCTYPE") || rest.starts_with("<!doctype") {
            let (mut depth, mut end) = (0_i32, None);
            for (at, c) in rest.char_indices() {
                match c {
                    '[' => depth += 1,
                    ']' => depth -= 1,
                    '>' if depth <= 0 => {
                        end = Some(at);
                        break;
                    }
                    _ => {}
                }
            }
            let Some(end) = end else { return false };
            rest = &rest[end + 1..];
        } else {
            return rest.strip_prefix("<svg").is_some_and(|after| {
                after.starts_with(|c: char| c.is_whitespace() || c == '>' || c == '/')
            });
        }
    }
}

fn raster(media_type: &str, head: &[u8], tail: &[u8], size: u64) -> Result<(), String> {
    match media_type {
        "image/png" => {
            const IEND: [u8; 12] = [0, 0, 0, 0, b'I', b'E', b'N', b'D', 0xae, 0x42, 0x60, 0x82];
            if !head.starts_with(b"\x89PNG\r\n\x1a\n")
                || head.get(8..16) != Some(b"\x00\x00\x00\x0dIHDR")
            {
                return Err("the bytes are not a PNG".into());
            }
            let dimension = |at: usize| {
                u32::from_be_bytes([head[at], head[at + 1], head[at + 2], head[at + 3]])
            };
            if head.len() < 24 || dimension(16) == 0 || dimension(20) == 0 {
                return Err("the PNG has no width or height".into());
            }
            if !tail.ends_with(&IEND) {
                return Err("the PNG does not end with its IEND chunk".into());
            }
        }
        "image/jpeg" => {
            if !head.starts_with(&[0xff, 0xd8, 0xff]) {
                return Err("the bytes are not a JPEG".into());
            }
            if !tail.ends_with(&[0xff, 0xd9]) {
                return Err("the JPEG does not end with its end-of-image marker".into());
            }
        }
        "image/gif" => {
            if !(head.starts_with(b"GIF87a") || head.starts_with(b"GIF89a")) {
                return Err("the bytes are not a GIF".into());
            }
            if tail.last() != Some(&0x3b) {
                return Err("the GIF does not end with its trailer".into());
            }
        }
        "image/webp" => {
            if head.len() < 12 || &head[..4] != b"RIFF" || &head[8..12] != b"WEBP" {
                return Err("the bytes are not a WebP image".into());
            }
            let declared = u32::from_le_bytes([head[4], head[5], head[6], head[7]]) as u64;
            if declared + 8 != size {
                return Err("the WebP length does not match the file".into());
            }
        }
        other => return Err(format!("{other} is not a raster image this kind checks")),
    }
    Ok(())
}

/// The result of checking one stored artifact: the kind to record, the kind checked, how the check ended, and its
/// receipt.
pub struct StoredCheck {
    pub recorded: (String, u32),
    pub checked: (String, u32),
    pub outcome: Outcome,
    pub receipt: Value,
}

/// Checks a stored file whose record names `size` and `sha256`. The bytes are hashed while they are checked. A file that
/// cannot be read, or whose bytes differ from the record, gives no result (`Err`): that is a custody fault to retry and
/// report, not a property of the kind. A result package is also listed, which checks every member path and digest.
pub fn check_stored(
    registry: &Registry,
    path: &PathBuf,
    media_type: &str,
    size: u64,
    sha256: &str,
) -> Result<StoredCheck, &'static str> {
    let checked = registry.recognize(media_type);
    let mut outcome = read_and_check(checked, path, media_type, size, sha256)?;
    if outcome.passed && checked.validator == Validator::ResultPackage {
        outcome.checks.push("listing");
        match evidence_package::list_package(path, size, sha256, None) {
            Ok(_) => {}
            Err(evidence_package::PackageError::Integrity | evidence_package::PackageError::Io) => {
                return Err("the stored bytes changed while they were checked");
            }
            Err(evidence_package::PackageError::TooLarge) => {
                outcome.passed = false;
                outcome.reason = Some("the package holds more than the broker lists".into());
            }
            Err(evidence_package::PackageError::NotAPackage) => {
                outcome.passed = false;
                outcome.reason = Some("the package cannot be listed".into());
            }
        }
    }
    outcome.reason = outcome.reason.map(|reason| bounded(&reason));
    let recorded = if outcome.passed {
        checked
    } else {
        registry.opaque()
    };
    let receipt = receipt(registry, checked, media_type, sha256, size, &outcome);
    Ok(StoredCheck {
        recorded: (recorded.id.clone(), recorded.version),
        checked: (checked.id.clone(), checked.version),
        outcome,
        receipt,
    })
}

/// A reason as recorded: at most 300 characters.
fn bounded(reason: &str) -> String {
    reason.chars().take(300).collect()
}

fn read_and_check(
    kind: &Kind,
    path: &PathBuf,
    media_type: &str,
    size: u64,
    sha256: &str,
) -> Result<Outcome, &'static str> {
    let mut file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW)
        .open(path)
        .map_err(|_| "the stored file cannot be opened")?;
    if !file
        .metadata()
        .is_ok_and(|meta| meta.is_file() && meta.len() == size)
    {
        return Err("the stored file's size differs from the record");
    }
    let mut check = Check::new(kind, media_type, false);
    let mut digest = Sha256::new();
    let mut buffer = vec![0_u8; 64 * 1024];
    let mut read_total = 0_u64;
    loop {
        match file.read(&mut buffer) {
            Ok(0) => break,
            Ok(read) => {
                read_total += read as u64;
                digest.update(&buffer[..read]);
                check.update(&buffer[..read]);
            }
            Err(_) => return Err("the stored file cannot be read"),
        }
    }
    if read_total != size || hex::encode(digest.finalize()) != sha256 {
        return Err("the stored bytes differ from the record");
    }
    let mut outcome = check.finish();
    outcome.checks.insert(0, "sha256");
    Ok(outcome)
}

/// The receipt of one check: enough to repeat it with the same registry. The media type is the one recorded for the
/// file, which chose the kind and how its bytes are read.
pub fn receipt(
    registry: &Registry,
    checked: &Kind,
    media_type: &str,
    input_sha256: &str,
    size: u64,
    outcome: &Outcome,
) -> Value {
    json!({
        "routine": CHECK_ROUTINE, "version": CHECK_VERSION, "registry": registry.digest,
        "checked_as": checked.reference(), "media_type": media_type, "input_sha256": input_sha256, "size": size,
        "checks": outcome.checks, "result": if outcome.passed { "passed" } else { "failed" },
        "reason": outcome.reason,
    })
}

/// A kind named by id and version.
#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
pub struct KindRef {
    pub id: String,
    pub version: u32,
}

impl From<&Kind> for KindRef {
    fn from(kind: &Kind) -> Self {
        Self {
            id: kind.id.clone(),
            version: kind.version,
        }
    }
}

#[derive(Clone, Debug, Serialize)]
pub struct MemberValidation {
    pub state: &'static str,
    pub checked_as: KindRef,
    pub reason: Option<String>,
}

/// A package member's kind fields. `same_values_group` and `views` come from a method's output binding, which no
/// method declares yet, so they are null and empty; a reader then offers the kind's own views.
#[derive(Clone, Debug, Serialize)]
pub struct MemberKind {
    pub result_kind: KindRef,
    pub same_values_group: Option<String>,
    pub views: Vec<Value>,
    pub validation: MemberValidation,
}

/// A member's fields from its check. The listing names the routine and registry once for all members.
pub fn member_kind(registry: &Registry, checked: &Kind, outcome: Outcome) -> MemberKind {
    let recorded = if outcome.passed {
        checked
    } else {
        registry.opaque()
    };
    MemberKind {
        result_kind: recorded.into(),
        same_values_group: None,
        views: Vec::new(),
        validation: MemberValidation {
            state: if outcome.passed { "passed" } else { "failed" },
            checked_as: checked.into(),
            reason: outcome.reason.map(|reason| bounded(&reason)),
        },
    }
}

impl MemberKind {
    /// Takes a declared kind when the registry allows it for this member's check and media type. True when it did.
    pub fn declare(&mut self, registry: &Registry, kind_id: &str, media_type: &str) -> bool {
        match registry.declared(kind_id, media_type, &self.validation) {
            Some(kind) => {
                self.result_kind = kind.into();
                true
            }
            None => false,
        }
    }
}

/// The routine and registry that checked a listing's members.
pub fn listing_check(registry: &Registry) -> Value {
    json!({"routine": CHECK_ROUTINE, "version": CHECK_VERSION, "registry": registry.digest})
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture(path: &str) -> Vec<u8> {
        std::fs::read(
            PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .join("kinds")
                .join(path),
        )
        .unwrap()
    }

    fn check_bytes(kind: &Kind, media_type: &str, bytes: &[u8], chunk: usize) -> Outcome {
        let mut check = Check::new(kind, media_type, false);
        for piece in bytes.chunks(chunk.max(1)) {
            check.update(piece);
        }
        check.finish()
    }

    #[test]
    fn the_registry_loads_and_every_fixture_meets_its_expectation_in_any_chunking() {
        let registry = registry().unwrap();
        assert!(registry.digest.starts_with("sha256:") && registry.digest.len() == 71);
        assert_eq!(registry.opaque().id, OPAQUE);
        let mut fixtures = 0;
        for kind in &registry.kinds {
            // A declared kind's files are checked by the kinds it is checked as, whose fixtures cover them.
            assert_eq!(
                kind.fixtures.is_empty(),
                kind.recognition == Recognition::Declared,
                "{} has fixtures unless it is declared",
                kind.id
            );
            for case in &kind.fixtures {
                fixtures += 1;
                let bytes = fixture(&case.path);
                if kind.id != OPAQUE {
                    assert_eq!(
                        registry.recognize(&case.media_type).id,
                        kind.id,
                        "{}",
                        case.path
                    );
                }
                for chunk in [1, 3, 7, 64 * 1024] {
                    let outcome = check_bytes(kind, &case.media_type, &bytes, chunk);
                    assert_eq!(
                        outcome.passed,
                        case.expect == Expect::Passed,
                        "{} in chunks of {chunk}: {:?}",
                        case.path,
                        outcome.reason
                    );
                }
            }
        }
        assert!(fixtures >= 18);
    }

    #[test]
    fn recognition_reads_the_media_type_only_and_unknown_types_are_opaque() {
        let registry = registry().unwrap();
        assert_eq!(
            registry.recognize("Text/CSV; charset=utf-8").id,
            "table.delimited"
        );
        assert_eq!(registry.recognize("application/octet-stream").id, OPAQUE);
        assert_eq!(registry.recognize("").id, OPAQUE);
        let public = registry.public();
        assert_eq!(public["registry"]["digest"], registry.digest);
        assert_eq!(public["registry"]["version"], REGISTRY_FORMAT);
        let table = public["kinds"]
            .as_array()
            .unwrap()
            .iter()
            .find(|k| k["id"] == "table.delimited")
            .unwrap();
        assert_eq!(table["access"], "paged");
        assert_eq!(
            table["profile_uri"],
            "urn:enrichedlab:kind:table.delimited:1"
        );
        assert_eq!(
            table["views"][0],
            json!({"view": "table", "gpu": ["still"], "fallback": "download"})
        );
        assert_eq!(
            table["views"][1],
            json!({"view": "surface", "gpu": ["webgpu", "webgl2", "still"], "fallback": "image",
                "encoding": ["x", "y", "z"]})
        );
        assert_eq!(table["recognition"], "media-type");
        assert!(table.get("checked_as").is_none());
        // A declared kind is never recognized from a media type.
        let figure = public["kinds"]
            .as_array()
            .unwrap()
            .iter()
            .find(|k| k["id"] == "figure.r")
            .unwrap();
        assert_eq!(figure["recognition"], "declared");
        assert_eq!(figure["validator"], "delegated");
        assert_eq!(
            figure["checked_as"],
            json!(["image.svg", "document.pdf", "image.raster"])
        );
        assert_eq!(figure["views"][0]["media_types"], json!(["image/svg+xml"]));
        for (media_type, kind) in [
            ("image/svg+xml", "image.svg"),
            ("application/pdf", "document.pdf"),
            ("image/png", "image.raster"),
        ] {
            assert_eq!(registry.recognize(media_type).id, kind);
        }
    }

    #[test]
    fn a_declared_kind_names_only_files_its_checked_kinds_passed() {
        let registry = registry().unwrap();
        let member = |checked: &str, passed: bool| {
            let kind = registry.kind(checked).unwrap();
            member_kind(
                registry,
                kind,
                Outcome {
                    passed,
                    checks: vec![],
                    reason: (!passed).then(|| "broken".into()),
                },
            )
        };
        let mut svg = member("image.svg", true);
        assert!(svg.declare(registry, "figure.r", "image/svg+xml"));
        assert_eq!(
            (
                svg.result_kind.id.as_str(),
                svg.validation.checked_as.id.as_str()
            ),
            ("figure.r", "image.svg")
        );
        let mut png = member("image.raster", true);
        assert!(png.declare(registry, "figure.r", "image/png; charset=binary"));
        for (checked, passed, kind, media_type, why) in [
            (
                "image.svg",
                false,
                "figure.r",
                "image/svg+xml",
                "failed its check",
            ),
            (
                "image.raster",
                true,
                "figure.r",
                "image/jpeg",
                "a media type figure.r does not take",
            ),
            (
                "image.raster",
                true,
                "figure.r",
                "image/svg+xml",
                "checked by a kind that does not own the media type",
            ),
            (
                "text.plain",
                true,
                "figure.r",
                "text/plain",
                "not a kind figure.r is checked as",
            ),
            (
                "image.svg",
                true,
                "image.svg",
                "image/svg+xml",
                "not a declared kind",
            ),
            (
                "image.svg",
                true,
                "figure.unknown",
                "image/svg+xml",
                "no such kind",
            ),
        ] {
            let mut file = member(checked, passed);
            let before = file.result_kind.clone();
            assert!(!file.declare(registry, kind, media_type), "{why}");
            assert_eq!(file.result_kind, before, "{why}");
        }
    }

    #[test]
    fn a_registry_that_breaks_a_rule_does_not_load() {
        let good = "id: x.y\nversion: 1\nstatus: accepted\nowner: o\nsummary: s\nmedia_types: [text/plain]\n\
                    validator: utf8-text\naccess: whole\nlimits: {bytes: 10}\n\
                    views: [{view: text, gpu: [still], fallback: download}]\n";
        let opaque = (
            "file.opaque.yaml",
            "id: file.opaque\nversion: 1\nstatus: accepted\nowner: o\nsummary: s\nmedia_types: []\nvalidator: none\n\
             access: whole\nlimits: {}\nviews: [{view: download, gpu: [still], fallback: download}]\n",
        );
        assert!(Registry::parse(&[("x.y.yaml", good), opaque]).is_ok());
        for (name, text, why) in [
            ("x.z.yaml", good.to_string(), "file name"),
            (
                "x.y.yaml",
                good.replace("version: 1", "version: 0"),
                "version",
            ),
            (
                "x.y.yaml",
                good.replace("text/plain", "image/png"),
                "validator cannot check",
            ),
            (
                "x.y.yaml",
                good.replace("gpu: [still]", "gpu: [still, webgpu]"),
                "still last",
            ),
            (
                "x.y.yaml",
                good.replace("limits: {bytes: 10}", "limits: {bytes: 10, rows: 5}"),
                "unknown limit",
            ),
            (
                "x.y.yaml",
                good.replace("limits: {bytes: 10}", "limits: {}"),
                "bytes limit",
            ),
            (
                "x.y.yaml",
                good.replace("owner: o", "owner: o\ncolour: red"),
                "unknown field",
            ),
            ("x.y.yaml", good.replace("id: x.y", "id: X.y"), "id form"),
        ] {
            assert!(Registry::parse(&[(name, &text), opaque]).is_err(), "{why}");
        }
        let declared = (
            "x.d.yaml",
            "id: x.d\nversion: 1\nstatus: accepted\nowner: o\nsummary: s\nrecognition: declared\n\
             checked_as: [x.y]\nmedia_types: [text/plain]\nvalidator: delegated\naccess: whole\nlimits: {}\n\
             views: [{view: text, media_types: [text/plain], gpu: [still], fallback: download}]\n",
        );
        assert!(Registry::parse(&[("x.y.yaml", good), declared, opaque]).is_ok());
        for (text, why) in [
            (
                declared
                    .1
                    .replace("validator: delegated", "validator: utf8-text"),
                "declared but validated",
            ),
            (
                declared.1.replace("recognition: declared\n", ""),
                "delegated but recognized",
            ),
            (
                declared.1.replace("checked_as: [x.y]\n", ""),
                "declared without checked kinds",
            ),
            (
                declared.1.replace("checked_as: [x.y]", "checked_as: [x.q]"),
                "checked as a missing kind",
            ),
            (
                declared
                    .1
                    .replace("checked_as: [x.y]", "checked_as: [file.opaque]"),
                "checked as an unvalidated kind",
            ),
            (
                declared.1.replace("checked_as: [x.y]", "checked_as: [x.d]"),
                "checked as itself",
            ),
            (
                declared.1.replace("limits: {}", "limits: {bytes: 5}"),
                "declared with limits",
            ),
            (
                declared.1.replace(
                    "media_types: [text/plain]\nvalidator",
                    "media_types: [text/plain, text/csv]\nvalidator",
                ),
                "a media type no checked kind owns",
            ),
            (
                declared.1.replace(
                    "view: text, media_types: [text/plain]",
                    "view: text, media_types: [text/csv]",
                ),
                "a view of a foreign media type",
            ),
        ] {
            assert!(
                Registry::parse(&[("x.y.yaml", good), ("x.d.yaml", &text), opaque]).is_err(),
                "{why}"
            );
        }
        for (text, why) in [
            (
                good.replace("access: whole", "checked_as: [x.y]\naccess: whole"),
                "recognized but names checked kinds",
            ),
            (
                good.replace(
                    "fallback: download}",
                    "fallback: download, encoding: [x, x]}",
                ),
                "a repeated channel",
            ),
            (
                good.replace("fallback: download}", "fallback: download, encoding: [X]}"),
                "an upper-case channel",
            ),
            (
                good.replace(
                    "fallback: download}",
                    "fallback: download, media_types: [image/png]}",
                ),
                "a view of a foreign media type",
            ),
        ] {
            assert!(
                Registry::parse(&[("x.y.yaml", &text), opaque]).is_err(),
                "{why}"
            );
        }
        assert!(
            Registry::parse(&[
                (
                    "x.y.yaml",
                    &good.replace(
                        "fallback: download}",
                        "fallback: download, encoding: [x, y, z]}"
                    )
                ),
                opaque
            ])
            .is_ok(),
            "a view with an encoding"
        );
        let twin = good.replace("id: x.y", "id: x.z");
        assert!(
            Registry::parse(&[("x.y.yaml", good), ("x.z.yaml", &twin), opaque]).is_err(),
            "two accepted kinds claim one media type"
        );
        assert!(
            Registry::parse(&[("x.y.yaml", good)]).is_err(),
            "no opaque kind"
        );
    }

    #[test]
    fn checks_name_the_place_a_file_goes_wrong() {
        let registry = registry().unwrap();
        let text = registry.kind("text.plain").unwrap();
        let split = "caf\u{e9} au lait".as_bytes();
        for chunk in 1..split.len() {
            assert!(
                check_bytes(text, "text/plain", split, chunk).passed,
                "a character split at {chunk}"
            );
        }
        assert_eq!(
            check_bytes(text, "text/plain", b"ab\xffcd", 2)
                .reason
                .as_deref(),
            Some("not UTF-8 at byte 2")
        );
        assert_eq!(
            check_bytes(text, "text/plain", b"abc\xc3", 2)
                .reason
                .as_deref(),
            Some("not UTF-8 at byte 3"),
            "a sequence cut off at the end"
        );
        assert_eq!(
            check_bytes(text, "text/plain", b"ab\0", 1)
                .reason
                .as_deref(),
            Some("a NUL byte at byte 2")
        );
        let png = registry.kind("image.raster").unwrap();
        let dot = fixture("fixtures/image.raster/dot.png");
        assert_eq!(
            check_bytes(png, "image/jpeg", &dot, 64).reason.as_deref(),
            Some("the bytes are not a JPEG"),
            "a PNG stored as JPEG fails"
        );
        let json = registry.kind("record.json").unwrap();
        assert_eq!(
            check_bytes(json, "application/json", b"{\"a\":1}\n{\"b\":2}", 4)
                .reason
                .as_deref(),
            Some("not one JSON document (line 2, column 1)")
        );
        let ndjson = registry.kind("record.ndjson").unwrap();
        assert!(
            check_bytes(
                ndjson,
                "application/x-ndjson",
                b"{\"a\":1}\r\n\n{\"b\":2}",
                3
            )
            .passed
        );
        assert_eq!(
            check_bytes(ndjson, "application/x-ndjson", b"{\"a\":1}\n{\"b\":", 3)
                .reason
                .as_deref(),
            Some("line 2 is not one JSON document")
        );
        // A raw byte that is not UTF-8 inside a string: skipping a value would not see it.
        assert_eq!(
            check_bytes(json, "application/json", b"{\"a\":\"x\xff\"}", 4)
                .reason
                .as_deref(),
            Some("not UTF-8 (byte 7)")
        );
        assert!(check_bytes(json, "application/json", "{\"a\":\"\u{e9}\"}".as_bytes(), 3).passed);
        assert_eq!(
            check_bytes(
                ndjson,
                "application/x-ndjson",
                b"{\"a\":1}\n{\"b\":\"\xc3\"}\n",
                3
            )
            .reason
            .as_deref(),
            Some("line 2 is not UTF-8")
        );
        let svg = registry.kind("image.svg").unwrap();
        for good in [
            "<svg/>",
            "\u{feff}<?xml version=\"1.0\"?><!DOCTYPE svg [<!ENTITY a \"b\">]><svg width=\"1\">",
        ] {
            assert!(
                check_bytes(svg, "image/svg+xml", good.as_bytes(), 5).passed,
                "{good}"
            );
        }
        assert!(!check_bytes(svg, "image/svg+xml", b"<svgx/>", 5).passed);
        let package = registry.kind("package.result").unwrap();
        let mut inside = Check::new(package, "application/gzip", true);
        inside.update(&fixture("fixtures/package.result/result.tar.gz"));
        assert!(
            !inside.finish().passed,
            "a package inside a package is not read"
        );
        let table = registry.kind("table.delimited").unwrap();
        let mut big = Check::new(table, "text/csv", false);
        big.update(&vec![b'a'; 16 * 1024 * 1024 + 1]);
        assert_eq!(
            big.finish().reason.as_deref(),
            Some("larger than this kind's limit of 16777216 bytes")
        );
    }

    #[test]
    fn decompression_stops_at_its_limit() {
        let mut sink = Expanded { bytes: 0, limit: 4 };
        assert_eq!(sink.write(b"abcd").unwrap(), 4);
        assert!(sink.write(b"e").is_err());
    }

    #[test]
    fn a_stored_check_hashes_the_bytes_it_checks_and_lists_a_package() {
        let registry = registry().unwrap();
        let root = tempfile::tempdir().unwrap();
        let write = |name: &str, bytes: &[u8]| {
            let path = root.path().join(name);
            std::fs::write(&path, bytes).unwrap();
            (path, bytes.len() as u64, hex::encode(Sha256::digest(bytes)))
        };
        let (path, size, sha) = write("dose.csv", &fixture("fixtures/table.delimited/dose.csv"));
        let good = check_stored(registry, &path, "text/csv", size, &sha).unwrap();
        assert_eq!(good.recorded, ("table.delimited".into(), 1));
        assert_eq!(good.receipt["checks"][0], "sha256");
        assert_eq!(good.receipt["registry"], registry.digest);
        assert_eq!(
            check_stored(registry, &path, "text/csv", size, &"0".repeat(64)).err(),
            Some("the stored bytes differ from the record"),
            "a custody fault gives no result to record"
        );
        assert_eq!(
            check_stored(registry, &path, "text/csv", size + 1, &sha).err(),
            Some("the stored file's size differs from the record")
        );
        let package = fixture("fixtures/package.result/result.tar.gz");
        let (path, size, sha) = write("result.tar.gz", &package);
        let listed = check_stored(registry, &path, "application/gzip", size, &sha).unwrap();
        assert_eq!(listed.recorded.0, "package.result");
        assert_eq!(
            listed.outcome.checks,
            ["sha256", "bytes", "gzip", "listing"]
        );
        let mut broken = package.clone();
        let at = broken.len() - 9;
        broken[at] ^= 0xff;
        let (path, size, sha) = write("broken.tar.gz", &broken);
        let refused = check_stored(registry, &path, "application/gzip", size, &sha).unwrap();
        assert_eq!(
            refused.recorded.0, OPAQUE,
            "a corrupt tail the listing never reads"
        );
        let mut twice = package.clone();
        twice.extend_from_slice(&package);
        let (path, size, sha) = write("twice.tar.gz", &twice);
        let refused = check_stored(registry, &path, "application/gzip", size, &sha).unwrap();
        assert_eq!(
            refused.outcome.reason.as_deref(),
            Some("bytes follow the first gzip member")
        );
        let (path, size, sha) = write("cut.tar.gz", &package[..package.len() - 4]);
        let refused = check_stored(registry, &path, "application/gzip", size, &sha).unwrap();
        assert_eq!(
            refused.outcome.reason.as_deref(),
            Some("the gzip stream ends early or its checksum differs")
        );
        let (path, size, sha) = write("blob.bin", b"\x00\x01");
        let opaque = check_stored(registry, &path, "application/octet-stream", size, &sha).unwrap();
        assert_eq!(
            (opaque.recorded.0.as_str(), opaque.outcome.passed),
            (OPAQUE, true)
        );
    }
}
