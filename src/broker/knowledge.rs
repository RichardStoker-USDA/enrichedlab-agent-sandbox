//! The knowledge foundation's search for the research lead. Each turn of a run in a
//! configured lab first delegates the run to that lab's scope alone, renewing the delegation, and then searches with
//! the question under it, so the service ranks only the lab's records and the commons baseline. The coordinator reaches
//! the service with its own projected ServiceAccount token for the service's audience, read from its file at each call
//! (the kubelet rotates it) and never logged. A failure becomes a recorded status, never a failed turn.
use anyhow::{Result, anyhow, ensure};
use futures_util::StreamExt;
use reqwest::{Client, StatusCode, Url, redirect::Policy};
use serde::Deserialize;
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs::File,
    io::Read,
    path::{Path, PathBuf},
    time::Duration,
};
use time::{OffsetDateTime, format_description::well_known::Rfc3339};
use uuid::Uuid;

#[path = "knowledge_study.rs"]
mod study;
pub use study::{STUDY_FIELDS, StudyRecord, parse_study};

const SCHEMA: &str = "enrichedlab.knowledge-search.v1";
const CONFIG_LIMIT: u64 = 16 * 1024;
const TOKEN_LIMIT: u64 = 16 * 1024;
const RESPONSE_LIMIT: usize = 512 * 1024;
/// The service reads at most this many characters of a query.
pub const QUERY_CHARS: usize = 500;
const SEARCH_LIMIT: u32 = 12;
/// A delegation lasts less than the service's one-day limit; every turn renews it.
const DELEGATION: time::Duration = time::Duration::hours(20);
/// What the lead may learn from: earlier analyses, figures and the lab's methods.
const KINDS: [&str; 3] = ["analysis", "figure", "tool"];
/// Research-facing records only. Operational estate kinds are deliberately absent.
const CATALOG_KINDS: [&str; 4] = ["lab", "tool", "starting_point", "dataset"];
/// The lab's object collections (migration 083), for a run with a lab objects method.
const OBJECT_KINDS: [&str; 1] = ["object_collection"];
const OBJECTS_LIMIT: u32 = 8;
/// What the lead reads from its lab library: sections of the lab's papers.
const LIBRARY_KINDS: [&str; 1] = ["section"];
/// More sections than the lead is shown, since papers the lab no longer holds or has not accepted are left out after.
const LIBRARY_LIMIT: u32 = 12;
/// The longest section key the library ingest writes: a heading digest and an occurrence.
const SECTION_KEY_CHARS: usize = 40;

/// Why a search did not complete. Each maps to one fixed code.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KnowledgeError {
    /// The service refused the coordinator's token or the run's delegation.
    Refused,
    /// The service could not be reached or did not answer in time.
    Unavailable,
    /// The service answered out of contract.
    Invalid,
}

impl KnowledgeError {
    pub fn code(self) -> &'static str {
        match self {
            Self::Refused => "knowledge_refused",
            Self::Unavailable => "knowledge_unavailable",
            Self::Invalid => "knowledge_invalid",
        }
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct KnowledgeFile {
    schema: String,
    service_url: String,
    token_file: PathBuf,
    labs: Vec<String>,
    #[serde(default = "default_timeout")]
    timeout_seconds: u64,
    /// The knowledge service's retrieval generation the lead's library sections are ranked with: their
    /// whole text as passages, by BM25, the GPU embedder and the relations. Absent keeps search v1.
    #[serde(default)]
    retrieval_generation: Option<String>,
    /// A lab's own retrieval generation, in place of `retrieval_generation` for that lab: a generation's name, or `null`
    /// for search v1. One lab can then measure a generation on its own questions while the others keep theirs (the
    /// autonomy suite refused a coordinator-wide switch on October 29). Each key is one of `labs`.
    #[serde(default)]
    lab_retrieval_generations: BTreeMap<String, Option<String>>,
}

fn generation_name(name: &str) -> bool {
    (1..=63).contains(&name.len())
        && name
            .bytes()
            .next()
            .is_some_and(|b| b.is_ascii_lowercase() || b.is_ascii_digit())
        && name.bytes().all(|b| {
            b.is_ascii_lowercase() || b.is_ascii_digit() || matches!(b, b'.' | b'_' | b'-')
        })
}

fn default_timeout() -> u64 {
    8
}

/// One record the search returned, as the service described it.
#[derive(Debug, Clone, PartialEq)]
pub struct Hit {
    pub id: Uuid,
    pub kind: String,
    pub scope: String,
    /// The record's data class: `prior_output` and `method` are the lab's own work, `paper` its library.
    pub data_class: String,
    pub title: String,
    pub summary: String,
    pub source_uri: Option<String>,
    /// The digest of the record's content, as the ingest that wrote it computed it: for an object collection, its
    /// manifest's SHA-256.
    pub content_sha256: Option<String>,
    pub lexical_rank: Option<u64>,
    pub dense_rank: Option<u64>,
    /// Where the record sits in its source, such as a section's heading path and pages.
    pub locator: Option<Value>,
    /// For a section, figure or table: its paper, as the service named it. None for other kinds, for
    /// a part without a recorded edge to a paper the run may read, and from an older service.
    pub paper: Option<HitPaper>,
}

/// The paper a part belongs to: its knowledge record and title.
#[derive(Debug, Clone, PartialEq)]
pub struct HitPaper {
    pub id: Uuid,
    pub title: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Found {
    pub query_sha256: String,
    pub dense_profile: Option<String>,
    pub hits: Vec<Hit>,
}

pub struct KnowledgeService {
    client: Client,
    base: Url,
    token_file: PathBuf,
    labs: BTreeSet<String>,
    generation: Option<String>,
    lab_generations: BTreeMap<String, Option<String>>,
}

impl std::fmt::Debug for KnowledgeService {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("KnowledgeService")
            .field("labs", &self.labs)
            .finish()
    }
}

fn lab_key(value: &str) -> bool {
    let mut bytes = value.bytes();
    (2..=64).contains(&value.len())
        && bytes.next().is_some_and(|b| b.is_ascii_lowercase())
        && bytes.all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || matches!(b, b'_' | b'-'))
}

/// A bounded read of a mounted file. The configuration and the projected token are Kubernetes volume files, which
/// are links into a timestamped folder, so this read follows links; neither holds a reusable credential beyond the
/// token's short life.
pub(crate) fn mounted_bytes(path: &Path, limit: u64) -> Result<Vec<u8>> {
    ensure!(path.is_absolute(), "knowledge_file_invalid");
    let file = File::open(path).map_err(|_| anyhow!("knowledge_file_unavailable"))?;
    let mut bytes = Vec::new();
    file.take(limit + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| anyhow!("knowledge_file_unavailable"))?;
    ensure!(bytes.len() as u64 <= limit, "knowledge_file_invalid");
    Ok(bytes)
}

/// The service configuration, when one is named. Without it, no lab's lead searches.
pub fn load_optional(path: Option<&Path>) -> Result<Option<KnowledgeService>> {
    let Some(path) = path else {
        return Ok(None);
    };
    let file: KnowledgeFile = serde_json::from_slice(&mounted_bytes(path, CONFIG_LIMIT)?)
        .map_err(|_| anyhow!("knowledge_config_invalid"))?;
    let base = Url::parse(&file.service_url).map_err(|_| anyhow!("knowledge_config_invalid"))?;
    // Plain HTTP stays inside the cluster network or the Pod itself (loopback, as in the isolated qualification).
    let internal = base
        .host_str()
        .is_some_and(|h| h.ends_with(".svc.cluster.local") || h == "127.0.0.1" || h == "[::1]");
    ensure!(
        file.schema == SCHEMA
            && (base.scheme() == "https" || (base.scheme() == "http" && internal))
            && base.username().is_empty()
            && base.password().is_none()
            && base.query().is_none()
            && base.fragment().is_none()
            && file.token_file.is_absolute()
            && !file.labs.is_empty()
            && file.labs.iter().all(|lab| lab_key(lab))
            && (1..=30).contains(&file.timeout_seconds)
            && file
                .retrieval_generation
                .as_deref()
                .is_none_or(generation_name)
            && file
                .lab_retrieval_generations
                .iter()
                .all(|(lab, generation)| {
                    file.labs.contains(lab) && generation.as_deref().is_none_or(generation_name)
                }),
        "knowledge_config_invalid"
    );
    let client = Client::builder()
        .redirect(Policy::none())
        .connect_timeout(Duration::from_secs(3))
        .timeout(Duration::from_secs(file.timeout_seconds))
        .build()
        .map_err(|_| anyhow!("knowledge_client_invalid"))?;
    Ok(Some(KnowledgeService {
        client,
        base,
        token_file: file.token_file,
        labs: file.labs.into_iter().collect(),
        generation: file.retrieval_generation,
        lab_generations: file.lab_retrieval_generations,
    }))
}

/// The text a search sends: the question's first characters, trimmed as the service trims it.
pub fn search_text(question: &str) -> String {
    question
        .chars()
        .take(QUERY_CHARS)
        .collect::<String>()
        .trim()
        .to_owned()
}

fn hit(value: &Value) -> Option<Hit> {
    let r = &value["resource"];
    let rank = |v: &Value| match v {
        Value::Null => Some(None),
        other => other.as_u64().map(Some),
    };
    Some(Hit {
        id: r["resource_id"].as_str()?.parse().ok()?,
        kind: r["kind"].as_str()?.to_owned(),
        scope: r["scope"].as_str()?.to_owned(),
        data_class: r["data_class"].as_str()?.to_owned(),
        title: r["title"].as_str()?.to_owned(),
        summary: r["summary"].as_str().unwrap_or_default().to_owned(),
        source_uri: r["source_uri"].as_str().map(str::to_owned),
        content_sha256: r["content_sha256"].as_str().map(str::to_owned),
        lexical_rank: rank(&value["lexical_rank"])?,
        dense_rank: rank(&value["dense_rank"])?,
        locator: match &r["locator"] {
            Value::Null => None,
            Value::Object(_) => Some(r["locator"].clone()),
            _ => return None,
        },
        paper: match &value["paper"] {
            Value::Null => None,
            p => Some(HitPaper {
                id: p["resource_id"].as_str()?.parse().ok()?,
                title: p["title"].as_str()?.to_owned(),
            }),
        },
    })
}

/// A library section's address in the lab's corpus, `parsenriched://<corpus>/document/<id>/section/<key>`: the
/// document's identity and the section key. Any other address, or another corpus's, is none.
pub fn section_address(uri: &str, corpus: &str) -> Option<(Uuid, String)> {
    let rest = uri
        .strip_prefix("parsenriched://")?
        .strip_prefix(corpus)?
        .strip_prefix("/document/")?;
    document_section(rest)
}

/// A section's address in any corpus: the corpus, the document's identity and the section key. The commons
/// literature's sections sit in their own corpora, such as `literature-papers`.
pub fn section_address_any(uri: &str) -> Option<(String, Uuid, String)> {
    let rest = uri.strip_prefix("parsenriched://")?;
    let (corpus, rest) = rest.split_once("/document/")?;
    let corpus_ok = (1..=64).contains(&corpus.len())
        && corpus
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || matches!(b, b'-' | b'_'));
    let (document, key) = document_section(rest)?;
    Some((corpus.to_owned(), document, key)).filter(|_| corpus_ok)
}

fn document_section(rest: &str) -> Option<(Uuid, String)> {
    let (document, key) = rest.split_once("/section/")?;
    let key_ok = !key.is_empty()
        && key.len() <= SECTION_KEY_CHARS
        && key
            .bytes()
            .all(|b| b.is_ascii_hexdigit() || b == b'-' || b.is_ascii_digit());
    Some((document.parse().ok()?, key.to_owned())).filter(|_| key_ok)
}

/// A section's pages from its locator, in words: `page 3` or `pages 3 to 5`.
pub fn locator_pages(locator: Option<&Value>) -> Option<String> {
    let pages = locator?.get("pages")?;
    let from = pages.get("from")?.as_u64()?;
    let to = pages.get("to").and_then(Value::as_u64).unwrap_or(from);
    Some(if to > from {
        format!("pages {from} to {to}")
    } else {
        format!("page {from}")
    })
}

/// A section's headings from its locator, outermost first, joined for reading.
pub fn locator_headings(locator: Option<&Value>) -> Option<String> {
    let path: Vec<&str> = locator?
        .get("heading_path")?
        .as_array()?
        .iter()
        .filter_map(Value::as_str)
        .collect();
    (!path.is_empty()).then(|| path.join(" › "))
}

/// Reads a retrieval answer as a search answer: each hit is its record, ranked by its best passage, whose
/// words (its paper title and heading path, then the passage) stand for the record's summary, so the lead reads the
/// part of a section that matched rather than the section's opening. A section with two matching passages keeps its
/// first. Every hit must parse, or the whole answer is out of contract.
pub fn parse_retrieved(value: &Value) -> Result<Found, KnowledgeError> {
    let hits = value["hits"].as_array().ok_or(KnowledgeError::Invalid)?;
    let query_sha256 = value["query_sha256"]
        .as_str()
        .filter(|s| s.len() == 64)
        .ok_or(KnowledgeError::Invalid)?;
    let mut seen = BTreeSet::new();
    let mut out = Vec::new();
    for h in hits {
        let mut resource = h["resource"].clone();
        match &h["passage"] {
            Value::Null => {}
            Value::Object(p) => {
                let (Some(context), Some(body)) = (
                    p.get("context").and_then(Value::as_str),
                    p.get("body").and_then(Value::as_str),
                ) else {
                    return Err(KnowledgeError::Invalid);
                };
                resource["summary"] = json!(format!("{context}\n\n{body}"));
            }
            _ => return Err(KnowledgeError::Invalid),
        }
        let shaped = json!({"resource": resource, "lexical_rank": h["ranks"]["lexical"], "dense_rank": h["ranks"]["dense"],
                            "paper": h["paper"]});
        let parsed = hit(&shaped).ok_or(KnowledgeError::Invalid)?;
        if seen.insert(parsed.id) {
            out.push(parsed);
        }
    }
    Ok(Found {
        query_sha256: query_sha256.to_owned(),
        dense_profile: value["dense_profile"].as_str().map(str::to_owned),
        hits: out,
    })
}

/// Reads a search answer: every hit must parse, or the whole answer is out of contract.
pub fn parse_found(value: &Value) -> Result<Found, KnowledgeError> {
    let hits = value["hits"].as_array().ok_or(KnowledgeError::Invalid)?;
    let query_sha256 = value["query_sha256"]
        .as_str()
        .filter(|s| s.len() == 64)
        .ok_or(KnowledgeError::Invalid)?;
    Ok(Found {
        query_sha256: query_sha256.to_owned(),
        dense_profile: value["dense_profile"].as_str().map(str::to_owned),
        hits: hits
            .iter()
            .map(hit)
            .collect::<Option<Vec<_>>>()
            .ok_or(KnowledgeError::Invalid)?,
    })
}

impl KnowledgeService {
    pub fn configured(&self, lab: &str) -> bool {
        self.labs.contains(lab)
    }

    fn token(&self) -> Result<String, KnowledgeError> {
        let bytes = mounted_bytes(&self.token_file, TOKEN_LIMIT)
            .map_err(|_| KnowledgeError::Unavailable)?;
        let token = String::from_utf8(bytes)
            .map_err(|_| KnowledgeError::Invalid)?
            .trim()
            .to_owned();
        if token.is_empty() {
            return Err(KnowledgeError::Unavailable);
        }
        Ok(token)
    }

    async fn post(
        &self,
        path: &str,
        run: Option<Uuid>,
        body: &Value,
        change: bool,
    ) -> Result<(StatusCode, Vec<u8>), KnowledgeError> {
        let url = self.base.join(path).map_err(|_| KnowledgeError::Invalid)?;
        let mut request = self.client.post(url).bearer_auth(self.token()?).json(body);
        if let Some(run) = run {
            request = request.header("x-knowledge-run", run.to_string());
        }
        if change {
            request = request.header("x-knowledge-intent", "mutate");
        }
        let response = request
            .send()
            .await
            .map_err(|_| KnowledgeError::Unavailable)?;
        Self::response(response).await
    }

    async fn response(
        response: reqwest::Response,
    ) -> Result<(StatusCode, Vec<u8>), KnowledgeError> {
        let status = response.status();
        let mut body = Vec::new();
        let mut stream = response.bytes_stream();
        while let Some(chunk) = stream.next().await {
            let chunk = chunk.map_err(|_| KnowledgeError::Unavailable)?;
            if body.len() + chunk.len() > RESPONSE_LIMIT {
                return Err(KnowledgeError::Invalid);
            }
            body.extend_from_slice(&chunk);
        }
        Ok((status, body))
    }

    /// Reads the current study by library source under this run's lab-only delegation.
    pub async fn study(
        &self,
        run: Uuid,
        lab: &str,
        corpus: &str,
        document: Uuid,
    ) -> Result<Option<StudyRecord>, KnowledgeError> {
        if !self.configured(lab)
            || corpus.is_empty()
            || corpus.len() > 64
            || !corpus
                .bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || matches!(b, b'-' | b'_'))
        {
            return Err(KnowledgeError::Invalid);
        }
        self.delegate(run, lab).await?;
        let url = self
            .base
            .join(&format!("v1/library/{corpus}/documents/{document}/study"))
            .map_err(|_| KnowledgeError::Invalid)?;
        let response = self
            .client
            .get(url)
            .bearer_auth(self.token()?)
            .header("x-knowledge-run", run.to_string())
            .send()
            .await
            .map_err(|_| KnowledgeError::Unavailable)?;
        let (status, body) = Self::response(response).await?;
        study::answer(status, &body)
    }

    /// Delegates `run` to `lab` alone, renewing it, then searches its earlier work under that delegation.
    pub async fn search(&self, run: Uuid, lab: &str, text: &str) -> Result<Found, KnowledgeError> {
        self.delegate(run, lab).await?;
        self.find(run, text, &KINDS, SEARCH_LIMIT).await
    }

    /// Searches the lab's catalog under the same scoped delegation as its earlier research.
    /// The coordinator binds matches to current grants before ordering its existing method cards.
    pub async fn search_catalog(
        &self,
        run: Uuid,
        lab: &str,
        text: &str,
    ) -> Result<Found, KnowledgeError> {
        self.delegate(run, lab).await?;
        self.find(run, text, &CATALOG_KINDS, SEARCH_LIMIT).await
    }

    /// Delegates `run` to `lab` alone, renewing it, then searches the lab's object collections.
    pub async fn search_objects(
        &self,
        run: Uuid,
        lab: &str,
        text: &str,
    ) -> Result<Found, KnowledgeError> {
        self.delegate(run, lab).await?;
        self.find(run, text, &OBJECT_KINDS, OBJECTS_LIMIT).await
    }

    /// Delegates `run` to `lab` alone, renewing it, then searches the sections of the lab's library papers.
    pub async fn search_sections(
        &self,
        run: Uuid,
        lab: &str,
        text: &str,
    ) -> Result<Found, KnowledgeError> {
        self.delegate(run, lab).await?;
        match self.generation_for(lab) {
            Some(generation) => self.retrieve(run, text, generation).await,
            None => self.find(run, text, &LIBRARY_KINDS, LIBRARY_LIMIT).await,
        }
    }

    /// The retrieval generation `lab`'s library sections are ranked with: the lab's own entry when it has one, otherwise
    /// the coordinator's; `None` is search v1.
    pub fn generation_for(&self, lab: &str) -> Option<&str> {
        match self.lab_generations.get(lab) {
            Some(own) => own.as_deref(),
            None => self.generation.as_deref(),
        }
    }

    async fn retrieve(
        &self,
        run: Uuid,
        text: &str,
        generation: &str,
    ) -> Result<Found, KnowledgeError> {
        let query = json!({"text": text, "kinds": LIBRARY_KINDS, "limit": LIBRARY_LIMIT, "generation": generation});
        let (status, body) = self.post("v1/retrieve", Some(run), &query, false).await?;
        match status {
            StatusCode::OK => {}
            StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN => {
                return Err(KnowledgeError::Refused);
            }
            _ => return Err(KnowledgeError::Unavailable),
        }
        let value: Value = serde_json::from_slice(&body).map_err(|_| KnowledgeError::Invalid)?;
        parse_retrieved(&value)
    }

    async fn delegate(&self, run: Uuid, lab: &str) -> Result<(), KnowledgeError> {
        let expires = (OffsetDateTime::now_utc() + DELEGATION)
            .format(&Rfc3339)
            .map_err(|_| KnowledgeError::Invalid)?;
        let delegation = json!({"scopes": [lab], "purpose": "research run", "expires_at": expires});
        let (status, _) = self
            .post(
                &format!("v1/runs/{run}/delegation"),
                None,
                &delegation,
                true,
            )
            .await?;
        match status {
            StatusCode::NO_CONTENT => Ok(()),
            StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN | StatusCode::BAD_REQUEST => {
                Err(KnowledgeError::Refused)
            }
            _ => Err(KnowledgeError::Unavailable),
        }
    }

    async fn find(
        &self,
        run: Uuid,
        text: &str,
        kinds: &[&str],
        limit: u32,
    ) -> Result<Found, KnowledgeError> {
        let query = json!({"text": text, "kinds": kinds, "limit": limit});
        let (status, body) = self.post("v1/search", Some(run), &query, false).await?;
        match status {
            StatusCode::OK => {}
            StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN => {
                return Err(KnowledgeError::Refused);
            }
            _ => return Err(KnowledgeError::Unavailable),
        }
        let value: Value = serde_json::from_slice(&body).map_err(|_| KnowledgeError::Invalid)?;
        parse_found(&value)
    }
}

#[cfg(test)]
#[path = "knowledge_tests.rs"]
mod tests;
