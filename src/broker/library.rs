//! The document service behind the lab library. Each lab's papers live in its own
//! ParsEnriched corpus, reached with a token scoped to that corpus alone. The lab-to-corpus map and
//! the token files are server configuration; a token is read from its file each time it is used and
//! never logged. The broker keeps no paper bytes: it sends them to the corpus, asks for the chunk
//! index, and reads back the catalog fields and counts it records.
use anyhow::{Context, Result, anyhow, bail, ensure};
use futures_util::StreamExt;
use reqwest::{Client, StatusCode, Url, redirect::Policy};
use serde::Deserialize;
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    fs::File,
    io::Read,
    os::unix::fs::{MetadataExt, OpenOptionsExt},
    path::{Path, PathBuf},
    time::Duration,
};
use uuid::Uuid;

use super::guard_client::GuardError;
pub use super::guard_client::{GuardClient, GuardOutcome, GuardVerdict};

pub const MAX_PAPER_BYTES: usize = 100 * 1024 * 1024;
const SCHEMA: &str = "enrichedlab.lab-library.v1";
const TOKEN_LIMIT: u64 = 4096;
const CONFIG_LIMIT: u64 = 64 * 1024;
const SMALL_RESPONSE: usize = 256 * 1024;
const DOCUMENT_RESPONSE: usize = 2 * 1024 * 1024;
const SEARCH_RESPONSE: usize = 1024 * 1024;
pub const SEARCH_LIMIT: usize = 20;

/// Why a library call to the document service did not complete. Each maps to one fixed code.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LibraryError {
    /// The lab has no corpus in the configuration, or the corpus does not exist.
    NotConfigured,
    /// The service could not be reached, refused the token or answered out of contract.
    Unavailable,
    /// The service read the bytes and did not accept them as a paper.
    ParseFailed,
    /// The bytes exceed the service's upload limit.
    TooLarge,
    /// The library is configured but no upload guard is, so uploads stay closed.
    GuardNotConfigured,
    /// The upload guard could not be reached, refused the caller, or its malware scanner is unavailable.
    GuardUnavailable,
    /// The upload guard is checking other files at its limit.
    GuardBusy,
    /// The upload guard refused the file because this person's daily upload quota is used.
    GuardQuota,
    /// The same bytes match a paper added before the upload guard existed, which is never returned or restored.
    Unchecked,
    /// The library is adding other papers at its limit.
    Busy,
    /// The upload stopped before the whole file arrived, or took too long.
    Incomplete,
    /// The upload did not state its size, which its memory share is reserved by.
    LengthRequired,
}

impl LibraryError {
    pub fn code(self) -> &'static str {
        match self {
            Self::NotConfigured => "library_not_configured",
            Self::Unavailable => "library_unavailable",
            Self::ParseFailed => "library_parse_failed",
            Self::TooLarge => "library_paper_too_large",
            Self::GuardNotConfigured => "library_guard_not_configured",
            Self::GuardUnavailable => "library_guard_unavailable",
            Self::GuardBusy => "library_guard_busy",
            Self::GuardQuota => "library_upload_quota",
            Self::Unchecked => "library_paper_unchecked",
            Self::Busy => "library_busy",
            Self::Incomplete => "library_upload_incomplete",
            Self::LengthRequired => "library_upload_length_required",
        }
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct LibraryFile {
    schema: String,
    service_url: String,
    labs: Vec<LabEntry>,
    /// The upload guard every added paper passes through first. Without it no paper is added.
    #[serde(default)]
    guard: Option<GuardEntry>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct GuardEntry {
    service_url: String,
    token_file: PathBuf,
    use_case: String,
    tenant: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct LabEntry {
    lab: String,
    corpus: String,
    token_file: PathBuf,
}

struct LabCorpus {
    corpus: String,
    token_file: PathBuf,
}

/// What the service reported for a paper it accepted, bounded for the library record.
#[derive(Debug, Clone, PartialEq)]
pub struct Ingested {
    pub document_id: Uuid,
    pub title: Option<String>,
    pub authors: Vec<String>,
    pub year: Option<i32>,
    pub doi: Option<String>,
    pub page_count: Option<i32>,
    pub parse: Value,
}

/// One retrieved passage: the paper's own text as the service returned it, with where it is.
#[derive(Debug, Clone, PartialEq)]
pub struct Passage {
    pub document_id: Uuid,
    /// The service's identifier for this chunk; a passage the lead reads is cited by it.
    pub chunk_id: Option<Uuid>,
    pub section: Option<String>,
    pub heading_path: Vec<String>,
    pub pages: Option<String>,
    pub kind: Option<String>,
    pub text: String,
}

pub struct LibraryService {
    client: Client,
    base: Url,
    labs: BTreeMap<String, LabCorpus>,
    guard: Option<GuardClient>,
}

impl std::fmt::Debug for LibraryService {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("LibraryService")
            .field("labs", &self.labs.keys().collect::<Vec<_>>())
            .finish()
    }
}

fn lab_key(value: &str) -> bool {
    let mut bytes = value.bytes();
    (2..=64).contains(&value.len())
        && bytes.next().is_some_and(|b| b.is_ascii_lowercase())
        && bytes.all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || matches!(b, b'_' | b'-'))
}

impl From<GuardError> for LibraryError {
    fn from(error: GuardError) -> Self {
        match error {
            GuardError::Unavailable => LibraryError::GuardUnavailable,
            GuardError::Busy => LibraryError::GuardBusy,
            GuardError::Quota => LibraryError::GuardQuota,
            GuardError::TooLarge => LibraryError::TooLarge,
        }
    }
}

fn corpus_key(value: &str) -> bool {
    lab_key(value) && value.len() <= 63
}

/// A bare http(s) origin: a host, no credentials, query, fragment or path.
pub(super) fn origin(value: &str, what: &str) -> Result<Url> {
    let base = Url::parse(value).with_context(|| format!("{what} invalid"))?;
    ensure!(
        matches!(base.scheme(), "http" | "https")
            && base.host_str().is_some()
            && base.username().is_empty()
            && base.password().is_none()
            && base.query().is_none()
            && base.fragment().is_none()
            && base.path() == "/",
        "{what} must be a bare http(s) origin"
    );
    Ok(base)
}

/// A finding code or policy version as the guard writes them, bounded.
pub(super) fn guard_word(value: &Value, limit: usize) -> Option<String> {
    let text = value.as_str()?;
    (!text.is_empty()
        && text.len() <= limit
        && text
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-' | b'.')))
    .then(|| text.to_owned())
}

/// Reads a private file: absolute, not a link, a regular file owned by this process's user with no
/// group or other access, one link, within `limit` bytes.
pub(super) fn private_bytes(path: &Path, limit: u64) -> Result<Vec<u8>> {
    ensure!(path.is_absolute(), "private_file_invalid");
    let file = File::options()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(path)
        .map_err(|_| anyhow!("private_file_unavailable"))?;
    let metadata = file
        .metadata()
        .map_err(|_| anyhow!("private_file_invalid"))?;
    ensure!(
        metadata.is_file()
            && metadata.uid() == unsafe { libc::geteuid() }
            && metadata.mode() & 0o077 == 0
            && metadata.nlink() == 1
            && metadata.len() <= limit,
        "private_file_invalid"
    );
    let mut bytes = Vec::new();
    file.take(limit + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| anyhow!("private_file_unavailable"))?;
    ensure!(bytes.len() as u64 <= limit, "private_file_invalid");
    Ok(bytes)
}

/// Standard base64 with padding, for the service's JSON upload field.
pub fn base64(bytes: &[u8]) -> String {
    let mut out = Vec::with_capacity(bytes.len().div_ceil(3) * 4);
    base64_into(bytes, &mut out);
    String::from_utf8(out).expect("base64 is ASCII")
}

/// Appends the standard base64 encoding of `bytes` to `out`.
fn base64_into(bytes: &[u8], out: &mut Vec<u8>) {
    const TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    for chunk in bytes.chunks(3) {
        let b = [
            chunk[0],
            *chunk.get(1).unwrap_or(&0),
            *chunk.get(2).unwrap_or(&0),
        ];
        let n = (u32::from(b[0]) << 16) | (u32::from(b[1]) << 8) | u32::from(b[2]);
        for (i, shift) in [18u32, 12, 6, 0].into_iter().enumerate() {
            if i <= chunk.len() {
                out.push(TABLE[((n >> shift) & 63) as usize]);
            } else {
                out.push(b'=');
            }
        }
    }
}

pub(crate) fn bounded_text(value: &Value, limit: usize) -> Option<String> {
    let text = value.as_str()?.trim();
    (!text.is_empty()).then(|| text.chars().take(limit).collect())
}

pub(crate) fn author_name(value: &Value) -> Option<String> {
    if let Some(name) = bounded_text(&value["name"], 256) {
        return Some(name);
    }
    let parts: Vec<String> = ["given", "family"]
        .iter()
        .filter_map(|key| bounded_text(&value[*key], 128))
        .collect();
    (!parts.is_empty()).then(|| parts.join(" "))
}

pub(crate) fn year(value: &Value) -> Option<i32> {
    let text = value.as_str()?;
    let year: i32 = text.get(0..4)?.parse().ok()?;
    (1500..=2200).contains(&year).then_some(year)
}

impl LibraryService {
    /// Loads the private library configuration. Every lab key, corpus key and token path is
    /// checked here; token contents are read only when used.
    pub fn load(path: &Path) -> Result<Self> {
        let bytes =
            private_bytes(path, CONFIG_LIMIT).context("library configuration unreadable")?;
        let file: LibraryFile =
            serde_json::from_slice(&bytes).context("library configuration invalid")?;
        ensure!(
            file.schema == SCHEMA,
            "library configuration schema must be {SCHEMA}"
        );
        let base = origin(&file.service_url, "library service_url")?;
        ensure!(
            (1..=64).contains(&file.labs.len()),
            "library configuration needs 1 to 64 labs"
        );
        let mut labs = BTreeMap::new();
        for entry in file.labs {
            ensure!(lab_key(&entry.lab), "library lab key invalid");
            ensure!(corpus_key(&entry.corpus), "library corpus key invalid");
            ensure!(
                entry.token_file.is_absolute(),
                "library token_file must be absolute"
            );
            let lab = entry.lab.clone();
            ensure!(
                labs.insert(
                    entry.lab,
                    LabCorpus {
                        corpus: entry.corpus,
                        token_file: entry.token_file,
                    },
                )
                .is_none(),
                "library lab {lab} is listed twice"
            );
        }
        let _ = rustls::crypto::ring::default_provider().install_default();
        let client = Client::builder()
            .redirect(Policy::none())
            .connect_timeout(Duration::from_secs(5))
            .build()
            .context("library HTTP client")?;
        let guard = match file.guard {
            None => None,
            Some(entry) => {
                ensure!(
                    entry.token_file.is_absolute(),
                    "library guard token_file must be absolute"
                );
                ensure!(
                    corpus_key(&entry.use_case) && corpus_key(&entry.tenant),
                    "library guard use_case and tenant must be short lowercase names"
                );
                Some(GuardClient::new(
                    client.clone(),
                    &entry.service_url,
                    entry.token_file,
                    entry.use_case,
                    entry.tenant,
                    "library guard",
                )?)
            }
        };
        Ok(Self {
            client,
            base,
            labs,
            guard,
        })
    }

    /// The upload guard, when one is configured.
    pub fn guard(&self) -> Option<&GuardClient> {
        self.guard.as_ref()
    }

    pub fn configured(&self, lab: &str) -> bool {
        self.labs.contains_key(lab)
    }

    /// The corpus that holds `lab`'s library, when the lab has one.
    pub fn corpus(&self, lab: &str) -> Option<&str> {
        self.labs.get(lab).map(|entry| entry.corpus.as_str())
    }

    fn lab(&self, lab: &str) -> Result<(&LabCorpus, String), LibraryError> {
        let entry = self.labs.get(lab).ok_or(LibraryError::NotConfigured)?;
        let token = private_bytes(&entry.token_file, TOKEN_LIMIT)
            .ok()
            .and_then(|bytes| String::from_utf8(bytes).ok())
            .map(|text| text.trim().to_owned())
            .filter(|text| !text.is_empty() && text.bytes().all(|b| b.is_ascii_graphic()))
            .ok_or(LibraryError::Unavailable)?;
        Ok((entry, token))
    }

    fn url(&self, corpus: &str, tail: &str) -> Result<Url, LibraryError> {
        self.base
            .join(&format!("v1/corpora/{corpus}/{tail}"))
            .map_err(|_| LibraryError::Unavailable)
    }

    async fn read(
        response: reqwest::Response,
        limit: usize,
    ) -> Result<(StatusCode, Value), LibraryError> {
        let status = response.status();
        if response.content_length().is_some_and(|n| n > limit as u64) {
            return Err(LibraryError::Unavailable);
        }
        let mut bytes = Vec::new();
        let mut stream = response.bytes_stream();
        while let Some(chunk) = stream.next().await {
            let chunk = chunk.map_err(|_| LibraryError::Unavailable)?;
            if bytes.len() + chunk.len() > limit {
                return Err(LibraryError::Unavailable);
            }
            bytes.extend_from_slice(&chunk);
        }
        let body = if bytes.is_empty() {
            Value::Null
        } else {
            serde_json::from_slice(&bytes).map_err(|_| LibraryError::Unavailable)?
        };
        Ok((status, body))
    }

    fn refused(status: StatusCode) -> LibraryError {
        match status {
            StatusCode::NOT_FOUND => LibraryError::NotConfigured,
            StatusCode::PAYLOAD_TOO_LARGE => LibraryError::TooLarge,
            _ => LibraryError::Unavailable,
        }
    }

    /// Places a PDF in the lab's corpus, indexes its chunks and reads back its catalog fields.
    /// The same bytes again return the same document.
    /// Takes the paper's bytes and writes them straight into the request as base64, dropping the bytes before it
    /// is sent, so the paper is held once plus its encoding, never as a JSON value and a second serialized copy.
    pub async fn ingest(
        &self,
        lab: &str,
        bytes: Vec<u8>,
        sha256: &str,
    ) -> Result<Ingested, LibraryError> {
        let (entry, token) = self.lab(lab)?;
        let mut body = Vec::with_capacity(bytes.len().div_ceil(3) * 4 + 160);
        body.extend_from_slice(br#"{"content_base64":""#);
        base64_into(&bytes, &mut body);
        drop(bytes);
        let rest = json!({
            "external_id": format!("sha256:{sha256}"),
            "media_type": "application/pdf",
            "profile": "paper",
        })
        .to_string();
        body.extend_from_slice(b"\",");
        body.extend_from_slice(&rest.as_bytes()[1..]);
        let response = self
            .client
            .post(self.url(&entry.corpus, "documents")?)
            .bearer_auth(&token)
            .timeout(Duration::from_secs(180))
            .header(reqwest::header::CONTENT_TYPE, "application/json")
            .body(body)
            .send()
            .await
            .map_err(|_| LibraryError::Unavailable)?;
        let (status, ingested) = Self::read(response, SMALL_RESPONSE).await?;
        match status {
            StatusCode::OK => {}
            StatusCode::BAD_REQUEST | StatusCode::UNPROCESSABLE_ENTITY => {
                return Err(LibraryError::ParseFailed);
            }
            other => return Err(Self::refused(other)),
        }
        let document_id = ingested["document_id"]
            .as_str()
            .and_then(|id| Uuid::parse_str(id).ok())
            .ok_or(LibraryError::Unavailable)?;
        if ingested["ok"] != true {
            return Err(LibraryError::ParseFailed);
        }
        if ingested["source_checksum"]
            .as_str()
            .is_some_and(|checksum| checksum != sha256)
        {
            return Err(LibraryError::Unavailable);
        }
        let response = self
            .client
            .post(self.url(&entry.corpus, &format!("documents/{document_id}/index"))?)
            .bearer_auth(&token)
            .timeout(Duration::from_secs(180))
            .json(&json!({"embed": false}))
            .send()
            .await
            .map_err(|_| LibraryError::Unavailable)?;
        let (status, index) = Self::read(response, SMALL_RESPONSE).await?;
        if status != StatusCode::OK {
            return Err(Self::refused(status));
        }
        let document = self.document(entry, &token, document_id).await?;
        let figures = &document["page"]["collections"]["figures"]["total"];
        Ok(Ingested {
            document_id,
            title: bounded_text(&document["title"], 2048),
            authors: document["authors"]
                .as_array()
                .map(|all| all.iter().filter_map(author_name).take(200).collect())
                .unwrap_or_default(),
            year: year(&document["published_on"]),
            doi: bounded_text(&document["identifiers"]["doi"], 256)
                .filter(|doi| doi.starts_with("10.") && doi.len() >= 4),
            page_count: document["page_count"]
                .as_i64()
                .and_then(|n| i32::try_from(n).ok())
                .filter(|n| (1..=100_000).contains(n)),
            parse: json!({
                "state": bounded_text(&document["state"], 32),
                "severity": bounded_text(&ingested["severity"], 32),
                "chunks": index["chunk_count"].as_u64().or_else(|| index["chunks"].as_u64()),
                "figures": figures.as_u64(),
                "index": bounded_text(&index["index_state"], 32),
            }),
        })
    }

    async fn document(
        &self,
        entry: &LabCorpus,
        token: &str,
        document_id: Uuid,
    ) -> Result<Value, LibraryError> {
        let mut url = self.url(&entry.corpus, &format!("documents/{document_id}"))?;
        url.query_pairs_mut()
            .append_pair("include", "figures")
            .append_pair("limit", "25")
            .append_pair("include_spans", "false");
        self.get_object(url, token).await
    }

    async fn get_object(&self, url: Url, token: &str) -> Result<Value, LibraryError> {
        let response = self
            .client
            .get(url)
            .bearer_auth(token)
            .timeout(Duration::from_secs(30))
            .send()
            .await
            .map_err(|_| LibraryError::Unavailable)?;
        let (status, document) = Self::read(response, DOCUMENT_RESPONSE).await?;
        if status != StatusCode::OK || !document.is_object() {
            return Err(Self::refused(status));
        }
        Ok(document)
    }

    /// One page of a document's `collections` (a comma list of `blocks`, `figures` and `tables`) from `offset`, with
    /// the document's catalog fields and each collection's total, for reading a paper whole. The
    /// service returns at most 25 items of each and keeps 2,048 characters of a block's text.
    pub async fn document_page(
        &self,
        lab: &str,
        document_id: Uuid,
        collections: &str,
        offset: usize,
    ) -> Result<Value, LibraryError> {
        let (entry, token) = self.lab(lab)?;
        let mut url = self.url(&entry.corpus, &format!("documents/{document_id}"))?;
        url.query_pairs_mut()
            .append_pair("include", collections)
            .append_pair("offset", &offset.to_string())
            .append_pair("limit", "25")
            .append_pair("include_spans", "false");
        self.get_object(url, &token).await
    }

    /// One table's complete grid from the lab's corpus; a grid past the service's response limit is `TooLarge`.
    pub async fn table(
        &self,
        lab: &str,
        document_id: Uuid,
        ordinal: i64,
    ) -> Result<Value, LibraryError> {
        let (entry, token) = self.lab(lab)?;
        let url = self.url(
            &entry.corpus,
            &format!("documents/{document_id}/tables/{ordinal}"),
        )?;
        self.get_object(url, &token).await
    }

    /// The paper's figure captions and counts from its parse, for the paper page.
    pub async fn outline(&self, lab: &str, document_id: Uuid) -> Result<Value, LibraryError> {
        let (entry, token) = self.lab(lab)?;
        let document = self.document(entry, &token, document_id).await?;
        let figures: Vec<Value> = document["figures"]
            .as_array()
            .map(|all| {
                all.iter()
                    .take(25)
                    .map(|figure| {
                        json!({
                            "label": bounded_text(&figure["label"], 64),
                            "caption": bounded_text(&figure["caption"], 1200),
                            "page": figure["page"].as_u64(),
                        })
                    })
                    .collect()
            })
            .unwrap_or_default();
        Ok(json!({
            "abstract": bounded_text(&document["abstract"], 6000),
            "journal": bounded_text(&document["container_title"], 512),
            "figures": figures,
            "figures_total": document["page"]["collections"]["figures"]["total"].as_u64(),
        }))
    }

    /// Word search over the lab's corpus: at most 20 passages, 2 per paper.
    pub async fn search(&self, lab: &str, query: &str) -> Result<Vec<Passage>, LibraryError> {
        let (entry, token) = self.lab(lab)?;
        let mut url = self.url(&entry.corpus, "search")?;
        url.query_pairs_mut()
            .append_pair("q", query)
            .append_pair("mode", "lexical")
            .append_pair("limit", &SEARCH_LIMIT.to_string())
            .append_pair("max_per_document", "2")
            .append_pair("rerank", "false");
        let response = self
            .client
            .get(url)
            .bearer_auth(&token)
            .timeout(Duration::from_secs(30))
            .send()
            .await
            .map_err(|_| LibraryError::Unavailable)?;
        let (status, found) = Self::read(response, SEARCH_RESPONSE).await?;
        if status != StatusCode::OK {
            return Err(Self::refused(status));
        }
        let results = found["results"]
            .as_array()
            .ok_or(LibraryError::Unavailable)?;
        Ok(results
            .iter()
            .take(SEARCH_LIMIT)
            .filter_map(|hit| {
                Some(Passage {
                    document_id: Uuid::parse_str(hit["document_id"].as_str()?).ok()?,
                    chunk_id: hit["chunk_id"]
                        .as_str()
                        .and_then(|id| Uuid::parse_str(id).ok()),
                    section: bounded_text(&hit["section"], 512),
                    heading_path: hit["heading_path"]
                        .as_array()
                        .map(|all| {
                            all.iter()
                                .take(8)
                                .filter_map(|h| bounded_text(h, 256))
                                .collect()
                        })
                        .unwrap_or_default(),
                    pages: bounded_text(&hit["where"], 64),
                    kind: bounded_text(&hit["kind"], 32),
                    text: bounded_text(&hit["text"], 1500)?,
                })
            })
            .collect())
    }
}

/// The optional library configuration named by `AGENT_SANDBOX_LIBRARY_CONFIG_FILE`.
pub fn load_optional(path: Option<&Path>) -> Result<Option<LibraryService>> {
    match path {
        None => Ok(None),
        Some(path) => LibraryService::load(path).map(Some),
    }
}

/// Checks a PDF body before it is sent anywhere.
pub fn check_pdf(bytes: &[u8]) -> Result<()> {
    if bytes.len() > MAX_PAPER_BYTES {
        bail!("library_paper_too_large")
    }
    if bytes.len() < 5 || &bytes[..5] != b"%PDF-" {
        bail!("library_not_pdf")
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{
        Json, Router,
        extract::{Path as AxumPath, Query},
        http::{HeaderMap, StatusCode as AxumStatus},
        routing::{get, post},
    };
    use std::{collections::HashMap, fs, os::unix::fs::PermissionsExt};

    async fn check_with(
        client: &GuardClient,
        person: &str,
        bytes: Vec<u8>,
    ) -> Result<GuardVerdict, LibraryError> {
        let upload = crate::broker::guard_client::Upload {
            media_type: "application/pdf",
            lab: "pilot-lab",
            person,
            project: None,
            file_name: None,
        };
        client
            .check(&upload, bytes)
            .await
            .map_err(LibraryError::from)
    }

    async fn rebuilt_with(
        client: &GuardClient,
        verdict: &GuardVerdict,
    ) -> Result<Vec<u8>, LibraryError> {
        client
            .rebuilt(verdict, MAX_PAPER_BYTES as u64)
            .await
            .map_err(LibraryError::from)
    }

    #[test]
    fn base64_matches_the_standard_vectors() {
        for (input, output) in [
            ("", ""),
            ("f", "Zg=="),
            ("fo", "Zm8="),
            ("foo", "Zm9v"),
            ("foob", "Zm9vYg=="),
            ("fooba", "Zm9vYmE="),
            ("foobar", "Zm9vYmFy"),
        ] {
            assert_eq!(base64(input.as_bytes()), output);
        }
        assert_eq!(base64(&[0xff, 0xfe, 0xfd]), "//79");
    }

    #[test]
    fn a_pdf_needs_its_signature_and_a_bounded_size() {
        assert!(check_pdf(b"%PDF-1.7\n").is_ok());
        assert_eq!(
            check_pdf(b"<html>").unwrap_err().to_string(),
            "library_not_pdf"
        );
        assert_eq!(
            check_pdf(b"%PD").unwrap_err().to_string(),
            "library_not_pdf"
        );
    }

    fn private(dir: &Path, name: &str, contents: &str) -> PathBuf {
        let path = dir.join(name);
        fs::write(&path, contents).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
        path
    }

    fn config(dir: &Path, url: &str, extra: &str) -> PathBuf {
        let token = private(dir, "lab.token", "test-token-value\n");
        private(
            dir,
            "library.json",
            &format!(
                r#"{{"schema":"enrichedlab.lab-library.v1","service_url":"{url}","labs":[{{"lab":"pilot-lab","corpus":"lab-pilot-lab","token_file":"{}"}}{extra}]}}"#,
                token.display()
            ),
        )
    }

    #[test]
    fn the_configuration_is_checked_before_use() {
        let dir = tempfile::tempdir().unwrap();
        let good = config(dir.path(), "http://parsenriched.parsenriched.svc:8500", "");
        let service = LibraryService::load(&good).unwrap();
        assert!(service.configured("pilot-lab"));
        assert!(!service.configured("other-lab"));
        for (url, extra) in [
            ("http://user:pass@parsenriched:8500", ""),
            ("http://parsenriched:8500/v1", ""),
            ("ftp://parsenriched", ""),
            (
                "http://parsenriched:8500",
                r#",{"lab":"pilot-lab","corpus":"lab-x","token_file":"/x"}"#,
            ),
            (
                "http://parsenriched:8500",
                r#",{"lab":"b-lab","corpus":"Lab-B","token_file":"/x"}"#,
            ),
            (
                "http://parsenriched:8500",
                r#",{"lab":"c-lab","corpus":"lab-c","token_file":"relative.token"}"#,
            ),
        ] {
            let bad_dir = tempfile::tempdir().unwrap();
            assert!(
                LibraryService::load(&config(bad_dir.path(), url, extra)).is_err(),
                "{url} {extra}"
            );
        }
        // A configuration others can read is refused.
        fs::set_permissions(&good, fs::Permissions::from_mode(0o644)).unwrap();
        assert!(LibraryService::load(&good).is_err());
    }

    #[derive(Clone, Default)]
    struct Seen(std::sync::Arc<std::sync::Mutex<Vec<(String, Value)>>>);

    async fn fake(seen: Seen, parse_ok: bool) -> String {
        let authorized = |headers: &HeaderMap| {
            headers.get("authorization").and_then(|v| v.to_str().ok())
                == Some("Bearer test-token-value")
        };
        let s1 = seen.clone();
        let s2 = seen.clone();
        let s3 = seen.clone();
        let app = Router::new()
            .route(
                "/v1/corpora/{corpus}/documents",
                post(
                    move |AxumPath(corpus): AxumPath<String>,
                          headers: HeaderMap,
                          Json(body): Json<Value>| async move {
                        if !authorized(&headers) {
                            return (AxumStatus::UNAUTHORIZED, Json(json!({})));
                        }
                        if corpus != "lab-pilot-lab" {
                            return (AxumStatus::NOT_FOUND, Json(json!({"detail": "no corpus"})));
                        }
                        s1.0.lock().unwrap().push(("ingest".into(), body.clone()));
                        let checksum = body["external_id"].as_str().unwrap()[7..].to_owned();
                        (
                            AxumStatus::OK,
                            Json(json!({"document_id": "7a4c1b1e-0000-4000-8000-000000000001",
                                "ok": parse_ok, "severity": "ok", "source_checksum": checksum})),
                        )
                    },
                ),
            )
            .route(
                "/v1/corpora/{corpus}/documents/{id}/index",
                post(move |Json(body): Json<Value>| async move {
                    s2.0.lock().unwrap().push(("index".into(), body));
                    Json(json!({"chunks": 12, "chunk_count": 12, "active_embedding_count": 0,
                        "index_state": "chunks_only", "document_state": "indexed"}))
                }),
            )
            .route(
                "/v1/corpora/{corpus}/documents/{id}",
                get(move |Query(q): Query<HashMap<String, String>>| async move {
                    s3.0.lock().unwrap().push(("document".into(), json!(q)));
                    Json(json!({"id": "7a4c1b1e-0000-4000-8000-000000000001",
                        "title": "  A synthetic paper about tooth length  ",
                        "authors": [{"name": "Ada Example"}, {"given": "Ben", "family": "Sample"}, {}],
                        "identifiers": {"doi": "10.5555/example.1"}, "published_on": "2024-03-01",
                        "page_count": 3, "state": "indexed", "abstract": "Abstract text.",
                        "figures": [{"label": "Figure 1", "caption": "Tooth length by dose.", "page": 2}],
                        "page": {"collections": {"figures": {"total": 1, "returned": 1}}}}))
                }),
            )
            .route(
                "/v1/corpora/{corpus}/search",
                get(|Query(q): Query<HashMap<String, String>>| async move {
                    assert_eq!(q["mode"], "lexical");
                    assert_eq!(q["max_per_document"], "2");
                    Json(json!({"results": [
                        {"document_id": "7a4c1b1e-0000-4000-8000-000000000001", "section": "Results",
                         "chunk_id": "5c0ffee0-0000-4000-8000-000000000002",
                         "heading_path": ["Results"], "where": "page 2", "kind": "text",
                         "text": "Tooth length rose with dose."},
                        {"document_id": "not-a-uuid", "text": "dropped"}]}))
                }),
            );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        format!("http://{address}")
    }

    #[tokio::test]
    async fn a_paper_is_placed_indexed_and_read_back_with_the_lab_token() {
        let seen = Seen::default();
        let url = fake(seen.clone(), true).await;
        let dir = tempfile::tempdir().unwrap();
        let service = LibraryService::load(&config(dir.path(), &url, "")).unwrap();
        let pdf = b"%PDF-1.4 synthetic";
        let sha = "ab".repeat(32);
        let got = service
            .ingest("pilot-lab", pdf.to_vec(), &sha)
            .await
            .unwrap();
        assert_eq!(
            got.document_id,
            Uuid::parse_str("7a4c1b1e-0000-4000-8000-000000000001").unwrap()
        );
        assert_eq!(
            got.title.as_deref(),
            Some("A synthetic paper about tooth length")
        );
        assert_eq!(got.authors, vec!["Ada Example", "Ben Sample"]);
        assert_eq!((got.year, got.page_count), (Some(2024), Some(3)));
        assert_eq!(got.doi.as_deref(), Some("10.5555/example.1"));
        assert_eq!(got.parse["chunks"], 12);
        assert_eq!(got.parse["figures"], 1);
        let calls = seen.0.lock().unwrap().clone();
        assert_eq!(calls[0].0, "ingest");
        assert_eq!(calls[0].1["external_id"], format!("sha256:{sha}"));
        assert_eq!(calls[0].1["content_base64"], base64(pdf));
        assert_eq!(calls[0].1["profile"], "paper");
        assert_eq!(calls[1], ("index".into(), json!({"embed": false})));
        assert_eq!(calls[2].1["include"], "figures");
        // A lab without a corpus is not configured; the service's own refusals map to fixed codes.
        assert_eq!(
            service
                .ingest("other-lab", pdf.to_vec(), &sha)
                .await
                .unwrap_err(),
            LibraryError::NotConfigured
        );
        let passages = service.search("pilot-lab", "tooth length").await.unwrap();
        assert_eq!(passages.len(), 1);
        assert_eq!(passages[0].pages.as_deref(), Some("page 2"));
        assert_eq!(passages[0].text, "Tooth length rose with dose.");
        assert_eq!(
            passages[0].chunk_id,
            Some(Uuid::parse_str("5c0ffee0-0000-4000-8000-000000000002").unwrap())
        );
        let outline = service.outline("pilot-lab", got.document_id).await.unwrap();
        assert_eq!(outline["figures"][0]["caption"], "Tooth length by dose.");
        assert_eq!(outline["abstract"], "Abstract text.");
    }

    #[tokio::test]
    async fn a_refused_parse_or_an_unreachable_service_records_nothing() {
        let url = fake(Seen::default(), false).await;
        let dir = tempfile::tempdir().unwrap();
        let service = LibraryService::load(&config(dir.path(), &url, "")).unwrap();
        assert_eq!(
            service
                .ingest("pilot-lab", b"%PDF-1.4".to_vec(), &"cd".repeat(32))
                .await
                .unwrap_err(),
            LibraryError::ParseFailed
        );
        // Nothing listens on this port.
        let closed = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = closed.local_addr().unwrap();
        drop(closed);
        let dir = tempfile::tempdir().unwrap();
        let service =
            LibraryService::load(&config(dir.path(), &format!("http://{address}"), "")).unwrap();
        assert_eq!(
            service.search("pilot-lab", "tooth").await.unwrap_err(),
            LibraryError::Unavailable
        );
    }

    fn guarded(
        dir: &Path,
        url: &str,
        guard: &str,
        use_case: &str,
        token_path: Option<&str>,
    ) -> PathBuf {
        let token = private(dir, "lab.token", "test-token-value\n");
        let guard_token = private(dir, "guard.token", "guard-token-value\n");
        let guard_token = token_path
            .map(str::to_owned)
            .unwrap_or_else(|| guard_token.display().to_string());
        private(
            dir,
            "library.json",
            &format!(
                r#"{{"schema":"enrichedlab.lab-library.v1","service_url":"{url}","labs":[{{"lab":"pilot-lab","corpus":"lab-pilot-lab","token_file":"{}"}}],"guard":{{"service_url":"{guard}","token_file":"{guard_token}","use_case":"{use_case}","tenant":"qualification"}}}}"#,
                token.display()
            ),
        )
    }

    /// A stand-in guard. It allows a file holding `clean` and serves its rebuilt copy (the bytes reversed), holds one
    /// holding `held`, refuses one holding `virus`, answers 429 for `busy` and `quota`, and for `tamper` names a digest its copy
    /// does not have.
    async fn fake_guard(seen: Seen) -> String {
        use axum::extract::State;
        use axum::response::IntoResponse;
        #[derive(Clone)]
        struct Guard {
            seen: Seen,
            rebuilt: std::sync::Arc<std::sync::Mutex<Vec<u8>>>,
        }
        let state = Guard {
            seen,
            rebuilt: Default::default(),
        };
        let intake = |State(g): State<Guard>, headers: HeaderMap, body: axum::body::Bytes| async move {
            if headers.get("authorization").and_then(|v| v.to_str().ok())
                != Some("Bearer guard-token-value")
            {
                return (AxumStatus::UNAUTHORIZED, Json(json!({}))).into_response();
            }
            let header = |name: &str| {
                headers
                    .get(name)
                    .and_then(|v| v.to_str().ok())
                    .unwrap_or("")
                    .to_owned()
            };
            g.seen.0.lock().unwrap().push((
                "intake".into(),
                json!({"use_case": header("x-guard-use-case"), "tenant": header("x-guard-tenant"),
                    "lab": header("x-guard-lab"), "person": header("x-guard-person"),
                    "type": header("content-type"), "bytes": body.len()}),
            ));
            let text = String::from_utf8_lossy(&body).into_owned();
            for (word, error) in [("busy", "busy"), ("quota", "quota_exceeded")] {
                if text.contains(word) {
                    return (AxumStatus::TOO_MANY_REQUESTS, Json(json!({"error": error})))
                        .into_response();
                }
            }
            let rebuilt: Vec<u8> = body.iter().rev().copied().collect();
            let outcome = if text.contains("virus") {
                "blocked"
            } else if text.contains("held") {
                "quarantined"
            } else {
                "allowed"
            };
            let mut digest = hex::encode(<sha2::Sha256 as sha2::Digest>::digest(&rebuilt));
            if text.contains("tamper") {
                digest = "00".repeat(32);
            }
            *g.rebuilt.lock().unwrap() = rebuilt.clone();
            let disarmed =
                (outcome == "allowed").then(|| json!({"sha256": digest, "bytes": rebuilt.len()}));
            Json(json!({"intake_id": "0199b0f5-0000-7000-8000-000000000001", "outcome": outcome,
                "findings": [{"code": "active_javascript", "action": "disarm"}, {"code": "metadata_text", "action": "allow"}],
                "policy_version": "library-paper-test", "disarmed": disarmed}))
            .into_response()
        };
        let disarmed = |State(g): State<Guard>, headers: HeaderMap| async move {
            if headers.get("authorization").and_then(|v| v.to_str().ok())
                != Some("Bearer guard-token-value")
            {
                return (AxumStatus::UNAUTHORIZED, Vec::new()).into_response();
            }
            (AxumStatus::OK, g.rebuilt.lock().unwrap().clone()).into_response()
        };
        let app = Router::new()
            .route("/v1/intake", post(intake))
            .route("/v1/intake/{id}/disarmed", get(disarmed))
            .with_state(state);
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        format!("http://{address}")
    }

    #[test]
    fn the_guard_section_is_checked_before_use() {
        let dir = tempfile::tempdir().unwrap();
        let service = LibraryService::load(&guarded(
            dir.path(),
            "http://parsenriched:8500",
            "http://guard.upload-guard.svc:8080",
            "library-paper",
            None,
        ))
        .unwrap();
        assert!(service.guard().is_some());
        let plain = tempfile::tempdir().unwrap();
        assert!(
            LibraryService::load(&config(plain.path(), "http://parsenriched:8500", ""))
                .unwrap()
                .guard()
                .is_none()
        );
        for (guard, use_case, token) in [
            ("http://guard:8080/v1", "library-paper", None),
            ("http://guard:8080", "Library Paper", None),
            ("http://guard:8080", "library-paper", Some("relative.token")),
        ] {
            let bad = tempfile::tempdir().unwrap();
            assert!(
                LibraryService::load(&guarded(
                    bad.path(),
                    "http://parsenriched:8500",
                    guard,
                    use_case,
                    token
                ))
                .is_err(),
                "{guard} {use_case}"
            );
        }
    }

    #[tokio::test]
    async fn the_guard_decides_and_its_rebuilt_copy_is_checked() {
        let seen = Seen::default();
        let guard = fake_guard(seen.clone()).await;
        let dir = tempfile::tempdir().unwrap();
        let service = LibraryService::load(&guarded(
            dir.path(),
            "http://parsenriched:8500",
            &guard,
            "library-paper",
            None,
        ))
        .unwrap();
        let client = service.guard().unwrap();
        let person = "0199b0f5-0000-7000-8000-0000000000aa";
        let clean = b"%PDF-1.7 clean paper";
        let verdict = check_with(client, person, clean.to_vec()).await.unwrap();
        assert_eq!(verdict.outcome, GuardOutcome::Allowed);
        assert_eq!(
            verdict.findings,
            vec!["active_javascript"],
            "allow-level findings are left out"
        );
        assert_eq!(verdict.policy, "library-paper-test");
        let rebuilt = rebuilt_with(client, &verdict).await.unwrap();
        assert_eq!(rebuilt, clean.iter().rev().copied().collect::<Vec<u8>>());
        let calls = seen.0.lock().unwrap().clone();
        assert_eq!(
            calls[0].1,
            json!({"use_case": "library-paper", "tenant": "qualification", "lab": "pilot-lab",
            "person": person, "type": "application/pdf", "bytes": clean.len()})
        );
        for (bytes, outcome) in [
            (&b"%PDF-1.7 held"[..], GuardOutcome::Quarantined),
            (b"%PDF-1.7 virus", GuardOutcome::Blocked),
        ] {
            let verdict = check_with(client, person, bytes.to_vec()).await.unwrap();
            assert_eq!(
                (verdict.outcome, verdict.rebuilt.is_none()),
                (outcome, true)
            );
            assert_eq!(
                rebuilt_with(client, &verdict).await.unwrap_err(),
                LibraryError::GuardUnavailable
            );
        }
        assert_eq!(
            check_with(client, person, b"%PDF-1.7 busy".to_vec())
                .await
                .unwrap_err(),
            LibraryError::GuardBusy
        );
        assert_eq!(
            check_with(client, person, b"%PDF-1.7 quota".to_vec())
                .await
                .unwrap_err(),
            LibraryError::GuardQuota,
            "a used quota is not reported as a busy guard"
        );
        let tampered = check_with(client, person, b"%PDF-1.7 tamper".to_vec())
            .await
            .unwrap();
        assert_eq!(
            rebuilt_with(client, &tampered).await.unwrap_err(),
            LibraryError::GuardUnavailable,
            "a copy that does not match the named digest is not used"
        );
        // A token the guard does not know, and a guard that is not there, both close uploads.
        let wrong = tempfile::tempdir().unwrap();
        let path = guarded(
            wrong.path(),
            "http://parsenriched:8500",
            &guard,
            "library-paper",
            None,
        );
        fs::write(wrong.path().join("guard.token"), "another-token\n").unwrap();
        let service = LibraryService::load(&path).unwrap();
        assert_eq!(
            check_with(service.guard().unwrap(), person, clean.to_vec())
                .await
                .unwrap_err(),
            LibraryError::GuardUnavailable
        );
        let closed = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = closed.local_addr().unwrap();
        drop(closed);
        let gone = tempfile::tempdir().unwrap();
        let service = LibraryService::load(&guarded(
            gone.path(),
            "http://parsenriched:8500",
            &format!("http://{address}"),
            "library-paper",
            None,
        ))
        .unwrap();
        assert_eq!(
            check_with(service.guard().unwrap(), person, clean.to_vec())
                .await
                .unwrap_err(),
            LibraryError::GuardUnavailable
        );
    }
}
