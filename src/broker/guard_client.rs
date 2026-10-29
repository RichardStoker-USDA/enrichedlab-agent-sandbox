//! The upload guard's client (the guard's `README.md` is the contract). A caller sends a file for one
//! use case and reads the guard's decision, then reads the rebuilt copy of an allowed file; a fetcher, named in the
//! guard's configuration for that caller and use case, reads the rebuilt copy without being able to send anything. Every
//! copy is checked against the digest and size the decision named. A token is read from its owner-only file at each use
//! and never logged.
use anyhow::{Context, Result, ensure};
use futures_util::StreamExt;
use reqwest::{Client, StatusCode, Url, redirect::Policy};
use serde::Deserialize;
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::{
    path::{Path, PathBuf},
    time::Duration,
};
use uuid::Uuid;

use super::library::{guard_word, origin, private_bytes};

const TOKEN_LIMIT: u64 = 4096;
const CONFIG_LIMIT: u64 = 64 * 1024;
const DECISION_RESPONSE: usize = 256 * 1024;
pub const CALLER_SCHEMA: &str = "enrichedlab.guard-caller.v1";
pub const FETCHER_SCHEMA: &str = "enrichedlab.guard-fetcher.v1";

/// Why the guard gave no decision or copy. A file it holds or refuses is a decision, not an error.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GuardError {
    /// Unreachable, refused the token, without a fresh scanner, or answered out of contract.
    Unavailable,
    /// No permit was free.
    Busy,
    /// The person's daily quota is spent.
    Quota,
    /// Past the use case's size limit.
    TooLarge,
}

/// The guard's decision on one file, bounded for the API's answer and the broker's record.
#[derive(Debug, Clone, PartialEq)]
pub struct GuardVerdict {
    pub intake: Uuid,
    pub outcome: GuardOutcome,
    /// The finding codes that did not allow, at most 32, each a short word.
    pub findings: Vec<String>,
    pub policy: String,
    /// The rebuilt copy's digest and size, for an allowed file.
    pub rebuilt: Option<(String, u64)>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GuardOutcome {
    Allowed,
    Quarantined,
    Blocked,
}

impl GuardOutcome {
    pub fn as_str(self) -> &'static str {
        match self {
            GuardOutcome::Allowed => "allowed",
            GuardOutcome::Quarantined => "quarantined",
            GuardOutcome::Blocked => "blocked",
        }
    }
}

/// Whose file it is and what it claims to be. `person` is an opaque identifier the guard stores as a digest.
#[derive(Debug, Clone, Copy)]
pub struct Upload<'a> {
    pub media_type: &'a str,
    pub lab: &'a str,
    pub person: &'a str,
    pub project: Option<&'a str>,
    pub file_name: Option<&'a str>,
}

/// A caller: the guard's origin, this caller's token file, the use case and the tenant name the guard records.
pub struct GuardClient {
    client: Client,
    base: Url,
    token_file: PathBuf,
    use_case: String,
    tenant: String,
}

/// A fetcher: the guard's origin and its token file.
pub struct GuardFetcher {
    client: Client,
    base: Url,
    token_file: PathBuf,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CallerFile {
    schema: String,
    service_url: String,
    token_file: PathBuf,
    use_case: String,
    tenant: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct FetcherFile {
    schema: String,
    service_url: String,
    token_file: PathBuf,
}

fn short_name(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 63
        && value
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
}

fn http_client() -> Result<Client> {
    let _ = rustls::crypto::ring::default_provider().install_default();
    Client::builder()
        .redirect(Policy::none())
        .connect_timeout(Duration::from_secs(5))
        .build()
        .context("guard HTTP client")
}

fn token(path: &Path) -> Result<String, GuardError> {
    private_bytes(path, TOKEN_LIMIT)
        .ok()
        .and_then(|bytes| String::from_utf8(bytes).ok())
        .map(|text| text.trim().to_owned())
        .filter(|text| !text.is_empty() && text.bytes().all(|b| b.is_ascii_graphic()))
        .ok_or(GuardError::Unavailable)
}

/// A file name as the guard's `x-guard-file-name` takes it: percent-encoded UTF-8.
fn percent_encoded(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for b in text.bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'.' | b'_' | b'~') {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

impl GuardClient {
    pub fn new(
        client: Client,
        service_url: &str,
        token_file: PathBuf,
        use_case: String,
        tenant: String,
        what: &str,
    ) -> Result<Self> {
        ensure!(
            token_file.is_absolute(),
            "{what} token_file must be absolute"
        );
        ensure!(
            short_name(&use_case) && short_name(&tenant),
            "{what} use_case and tenant must be short lowercase names"
        );
        Ok(GuardClient {
            client,
            base: origin(service_url, &format!("{what} service_url"))?,
            token_file,
            use_case,
            tenant,
        })
    }

    /// A caller from its configuration file: `{"schema": "enrichedlab.guard-caller.v1", "service_url", "token_file",
    /// "use_case", "tenant"}`, owner-only.
    pub fn load(path: &Path) -> Result<Self> {
        let bytes = private_bytes(path, CONFIG_LIMIT).context("guard caller configuration")?;
        let file: CallerFile =
            serde_json::from_slice(&bytes).context("guard caller configuration invalid")?;
        ensure!(
            file.schema == CALLER_SCHEMA,
            "guard caller configuration schema"
        );
        GuardClient::new(
            http_client()?,
            &file.service_url,
            file.token_file,
            file.use_case,
            file.tenant,
            "guard caller",
        )
    }

    pub fn use_case(&self) -> &str {
        &self.use_case
    }

    fn refused(status: StatusCode, code: Option<&str>) -> GuardError {
        match (status, code) {
            (StatusCode::TOO_MANY_REQUESTS, Some("quota_exceeded")) => GuardError::Quota,
            (StatusCode::TOO_MANY_REQUESTS, _) => GuardError::Busy,
            (StatusCode::PAYLOAD_TOO_LARGE, _) => GuardError::TooLarge,
            _ => GuardError::Unavailable,
        }
    }

    /// Send a file and read the guard's decision.
    pub async fn check(
        &self,
        upload: &Upload<'_>,
        bytes: Vec<u8>,
    ) -> Result<GuardVerdict, GuardError> {
        let token = token(&self.token_file)?;
        let url = self
            .base
            .join("v1/intake")
            .map_err(|_| GuardError::Unavailable)?;
        let mut request = self
            .client
            .post(url)
            .bearer_auth(&token)
            .header("content-type", upload.media_type)
            .header("x-guard-use-case", &self.use_case)
            .header("x-guard-tenant", &self.tenant)
            .header("x-guard-lab", upload.lab)
            .header("x-guard-person", upload.person)
            .timeout(Duration::from_secs(300));
        if let Some(project) = upload.project {
            request = request.header("x-guard-project", project);
        }
        if let Some(name) = upload.file_name {
            request = request.header("x-guard-file-name", percent_encoded(name));
        }
        let response = request
            .body(bytes)
            .send()
            .await
            .map_err(|_| GuardError::Unavailable)?;
        let (status, body) = read_json(response, DECISION_RESPONSE).await?;
        if status != StatusCode::OK {
            return Err(Self::refused(status, body["error"].as_str()));
        }
        verdict(&body)
    }

    /// The rebuilt copy of an allowed file, at most `max` bytes.
    pub async fn rebuilt(&self, verdict: &GuardVerdict, max: u64) -> Result<Vec<u8>, GuardError> {
        let (sha, size) = verdict.rebuilt.clone().ok_or(GuardError::Unavailable)?;
        fetch(
            &self.client,
            &self.base,
            &self.token_file,
            verdict.intake,
            &sha,
            size,
            max,
        )
        .await
    }
}

impl GuardFetcher {
    /// A fetcher from its configuration file: `{"schema": "enrichedlab.guard-fetcher.v1", "service_url",
    /// "token_file"}`, owner-only.
    pub fn load(path: &Path) -> Result<Self> {
        let bytes = private_bytes(path, CONFIG_LIMIT).context("guard fetcher configuration")?;
        let file: FetcherFile =
            serde_json::from_slice(&bytes).context("guard fetcher configuration invalid")?;
        ensure!(
            file.schema == FETCHER_SCHEMA,
            "guard fetcher configuration schema"
        );
        ensure!(
            file.token_file.is_absolute(),
            "guard fetcher token_file must be absolute"
        );
        Ok(GuardFetcher {
            client: http_client()?,
            base: origin(&file.service_url, "guard fetcher service_url")?,
            token_file: file.token_file,
        })
    }

    /// The rebuilt copy of the file the guard recorded as `intake`, checked against `sha256` and `size`.
    pub async fn rebuilt(
        &self,
        intake: Uuid,
        sha256: &str,
        size: u64,
        max: u64,
    ) -> Result<Vec<u8>, GuardError> {
        fetch(
            &self.client,
            &self.base,
            &self.token_file,
            intake,
            sha256,
            size,
            max,
        )
        .await
    }
}

fn verdict(body: &Value) -> Result<GuardVerdict, GuardError> {
    let intake = body["intake_id"]
        .as_str()
        .and_then(|id| Uuid::parse_str(id).ok())
        .ok_or(GuardError::Unavailable)?;
    let outcome = match body["outcome"].as_str() {
        Some("allowed") => GuardOutcome::Allowed,
        Some("quarantined") => GuardOutcome::Quarantined,
        Some("blocked") => GuardOutcome::Blocked,
        _ => return Err(GuardError::Unavailable),
    };
    let mut findings: Vec<String> = body["findings"]
        .as_array()
        .map(|all| {
            all.iter()
                .filter(|f| f["action"].as_str() != Some("allow"))
                .filter_map(|f| guard_word(&f["code"], 64))
                .collect()
        })
        .unwrap_or_default();
    findings.dedup();
    findings.truncate(32);
    let rebuilt = match (outcome, &body["disarmed"]) {
        (GuardOutcome::Allowed, disarmed) => {
            let sha = disarmed["sha256"]
                .as_str()
                .filter(|s| s.len() == 64 && s.bytes().all(|b| b.is_ascii_hexdigit()))
                .ok_or(GuardError::Unavailable)?;
            let size = disarmed["bytes"].as_u64().ok_or(GuardError::Unavailable)?;
            Some((sha.to_ascii_lowercase(), size))
        }
        _ => None,
    };
    Ok(GuardVerdict {
        intake,
        outcome,
        findings,
        policy: guard_word(&body["policy_version"], 128).unwrap_or_default(),
        rebuilt,
    })
}

async fn read_json(
    response: reqwest::Response,
    limit: usize,
) -> Result<(StatusCode, Value), GuardError> {
    let status = response.status();
    if response.content_length().is_some_and(|n| n > limit as u64) {
        return Err(GuardError::Unavailable);
    }
    let mut bytes = Vec::new();
    let mut stream = response.bytes_stream();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|_| GuardError::Unavailable)?;
        if bytes.len() + chunk.len() > limit {
            return Err(GuardError::Unavailable);
        }
        bytes.extend_from_slice(&chunk);
    }
    let body = if bytes.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&bytes).map_err(|_| GuardError::Unavailable)?
    };
    Ok((status, body))
}

async fn fetch(
    client: &Client,
    base: &Url,
    token_file: &Path,
    intake: Uuid,
    sha256: &str,
    size: u64,
    max: u64,
) -> Result<Vec<u8>, GuardError> {
    if size == 0 || size > max {
        return Err(GuardError::TooLarge);
    }
    let token = token(token_file)?;
    let url = base
        .join(&format!("v1/intake/{intake}/disarmed"))
        .map_err(|_| GuardError::Unavailable)?;
    let response = client
        .get(url)
        .bearer_auth(&token)
        .timeout(Duration::from_secs(120))
        .send()
        .await
        .map_err(|_| GuardError::Unavailable)?;
    if response.status() != StatusCode::OK || response.content_length().is_some_and(|n| n != size) {
        return Err(GuardError::Unavailable);
    }
    let mut bytes = Vec::new();
    let mut stream = response.bytes_stream();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|_| GuardError::Unavailable)?;
        if (bytes.len() + chunk.len()) as u64 > size {
            return Err(GuardError::Unavailable);
        }
        bytes.extend_from_slice(&chunk);
    }
    if bytes.len() as u64 != size || hex::encode(Sha256::digest(&bytes)) != sha256 {
        return Err(GuardError::Unavailable);
    }
    Ok(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn file_names_go_percent_encoded() {
        assert_eq!(
            percent_encoded("GTT cohort 3 (final).xlsx"),
            "GTT%20cohort%203%20%28final%29.xlsx"
        );
        assert_eq!(percent_encoded("µg.csv"), "%C2%B5g.csv");
    }

    #[test]
    fn a_decision_is_read_within_its_contract() {
        let sha = "a".repeat(64);
        let allowed = serde_json::json!({"intake_id": Uuid::nil(), "outcome": "allowed", "policy_version": "p-1",
            "findings": [{"code": "macros_removed", "action": "disarm"}, {"code": "fine", "action": "allow"}],
            "disarmed": {"sha256": sha, "bytes": 10}});
        let read = verdict(&allowed).unwrap();
        assert_eq!(read.outcome, GuardOutcome::Allowed);
        assert_eq!(read.findings, ["macros_removed"]);
        assert_eq!(read.rebuilt, Some((sha.clone(), 10)));
        let mut held = allowed.clone();
        held["outcome"] = "quarantined".into();
        assert_eq!(verdict(&held).unwrap().rebuilt, None);
        let mut broken = allowed;
        broken["disarmed"]["sha256"] = "zz".into();
        assert_eq!(verdict(&broken).unwrap_err(), GuardError::Unavailable);
    }
}
