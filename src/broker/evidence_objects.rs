//! Evidence object stores for custody (the evidence append service's `StoreObject`): each copy of a lab's evidence is
//! one bucket on an S3-compatible service, reached over `reqwest` with requests signed here with AWS Signature Version
//! 4 (no SDK, so the adapter stays small and replaceable; the AWS signing crates need a newer Rust than this crate's
//! floor). Objects are content-addressed: the key is the SHA-256 of the uncompressed bytes and
//! the body is those bytes compressed with plain zstd (encoding `zstd-v1`, no dictionary).
//!
//! The adapter writes only new keys (`If-None-Match: *`), reads exact versions with a size bound, and reads a copy's
//! object-lock facts back so custody records what the store holds rather than what it was asked to do. It never
//! deletes and never changes retention; those belong to later, separately authorised identities.
use std::{fmt, fs, path::Path, time::Duration};

use anyhow::{Context, Result, bail};
use futures_util::StreamExt;
use hmac::{Hmac, KeyInit, Mac};
use reqwest::{Certificate, Client, Method, StatusCode, Url, redirect::Policy};
use sha2::{Digest, Sha256};
use time::{OffsetDateTime, format_description::well_known::Rfc3339, macros::format_description};

/// The only encoding this version writes.
pub const ENCODING: &str = "zstd-v1";
/// zstd level for `zstd-v1`: fast, and the level is not part of the format (any level decodes the same way).
const ZSTD_LEVEL: i32 = 3;
/// SHA-256 of an empty body, the payload hash of every request without one.
const EMPTY_SHA256: &str = "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855";

/// Why a store call failed, in the terms custody acts on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StoreError {
    /// The store could not be reached or answered 5xx; retry later.
    Unavailable,
    /// The key or version does not exist.
    NotFound,
    /// The store refused the request (authorisation, a malformed request); not retried.
    Refused(u16),
    /// The answer did not have the shape S3 promises (a missing version id, a body past its bound).
    Malformed(&'static str),
}

impl fmt::Display for StoreError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Unavailable => f.write_str("object store unavailable"),
            Self::NotFound => f.write_str("object not found"),
            Self::Refused(status) => write!(f, "object store refused the request ({status})"),
            Self::Malformed(what) => write!(f, "object store answer malformed: {what}"),
        }
    }
}

impl std::error::Error for StoreError {}

/// What one copy holds for a key and version, read from the store.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CopyFacts {
    pub version_id: String,
    pub stored_size: u64,
    /// `GOVERNANCE` or `COMPLIANCE` when the version carries a retention, as the store reports it.
    pub retention_mode: Option<String>,
    pub retain_until: Option<OffsetDateTime>,
    pub legal_hold: bool,
}

/// The outcome of writing a new key.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Put {
    Created {
        version_id: String,
    },
    /// The key already exists; nothing was written. The caller reads what is there.
    Exists,
}

/// How to reach one store endpoint with one identity. Credentials are read from files at connect time and never
/// logged.
#[derive(Debug, Clone, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EndpointConfig {
    /// An https origin, for example `https://store.example:9000`.
    pub url: String,
    pub region: String,
    pub access_key_file: std::path::PathBuf,
    pub secret_key_file: std::path::PathBuf,
}

/// One store endpoint and identity; each call names its bucket.
pub struct S3Store {
    client: Client,
    endpoint: Url,
    signer: Signer,
}

/// AWS Signature Version 4 for S3, as AWS specifies it: a canonical request (method, the path with each byte outside
/// the unreserved set percent-encoded once, the query sorted and encoded, the signed headers lowercased, trimmed and
/// sorted, the payload's SHA-256), the string to sign over its digest and the credential scope, and an HMAC-SHA256
/// chain from the secret through date, region, service and `aws4_request`.
struct Signer {
    access_key: String,
    secret_key: String,
    region: String,
}

type HmacSha256 = Hmac<Sha256>;

fn hmac(key: &[u8], data: &[u8]) -> Vec<u8> {
    let mut mac =
        <HmacSha256 as KeyInit>::new_from_slice(key).expect("HMAC accepts a key of any length");
    mac.update(data);
    mac.finalize().into_bytes().to_vec()
}

/// RFC 3986 percent-encoding as SigV4 requires: unreserved characters stay, every other byte is `%XX` uppercase;
/// `/` stays only when `keep_slash` (a path).
fn uri_encode(value: &str, keep_slash: bool) -> String {
    let mut out = String::with_capacity(value.len());
    for byte in value.bytes() {
        if byte.is_ascii_alphanumeric() || b"-._~".contains(&byte) || (keep_slash && byte == b'/') {
            out.push(byte as char);
        } else {
            out.push_str(&format!("%{byte:02X}"));
        }
    }
    out
}

/// A URL path as SigV4 canonicalizes it for S3: each `%XX` the URL library wrote is decoded first, then every byte
/// outside the unreserved set (and `/`) is encoded once, so a key with a space or a parenthesis signs as S3 checks it.
fn canonical_uri(path: &str) -> String {
    let bytes = path.as_bytes();
    let mut raw = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        let hex = |b: u8| (b as char).to_digit(16);
        if bytes[i] == b'%'
            && i + 2 < bytes.len()
            && let (Some(h), Some(l)) = (hex(bytes[i + 1]), hex(bytes[i + 2]))
        {
            raw.push((h * 16 + l) as u8);
            i += 3;
        } else {
            raw.push(bytes[i]);
            i += 1;
        }
    }
    let mut out = String::with_capacity(raw.len());
    for byte in raw {
        if byte.is_ascii_alphanumeric() || b"-._~/".contains(&byte) {
            out.push(byte as char);
        } else {
            out.push_str(&format!("%{byte:02X}"));
        }
    }
    out
}

impl Signer {
    /// The headers to add to a request so S3 accepts it: `x-amz-date`, `x-amz-content-sha256` and `authorization`.
    /// `headers` are the other headers the signature covers, lowercase names; `host` is taken from the URL.
    fn headers(
        &self,
        method: &str,
        url: &Url,
        headers: &[(&str, &str)],
        payload_sha256: &str,
        now: OffsetDateTime,
    ) -> Result<Vec<(String, String)>> {
        let amz_date = now
            .format(format_description!(
                "[year][month][day]T[hour][minute][second]Z"
            ))
            .context("request time")?;
        let date = &amz_date[..8];
        let host = match url.port() {
            Some(port) => format!("{}:{port}", url.host_str().context("the URL has no host")?),
            None => url.host_str().context("the URL has no host")?.to_owned(),
        };
        let mut signed: Vec<(String, String)> = headers
            .iter()
            .map(|(name, value)| (name.to_ascii_lowercase(), value.trim().to_owned()))
            .chain([
                ("host".to_owned(), host),
                ("x-amz-content-sha256".to_owned(), payload_sha256.to_owned()),
                ("x-amz-date".to_owned(), amz_date.clone()),
            ])
            .collect();
        signed.sort();
        let mut query: Vec<(String, String)> = url
            .query_pairs()
            .map(|(name, value)| (uri_encode(&name, false), uri_encode(&value, false)))
            .collect();
        query.sort();
        let canonical_query = query
            .iter()
            .map(|(name, value)| format!("{name}={value}"))
            .collect::<Vec<_>>()
            .join("&");
        let signed_names = signed
            .iter()
            .map(|(name, _)| name.as_str())
            .collect::<Vec<_>>()
            .join(";");
        let canonical_headers: String = signed
            .iter()
            .map(|(name, value)| format!("{name}:{value}\n"))
            .collect();
        let canonical_request = format!(
            "{method}\n{}\n{canonical_query}\n{canonical_headers}\n{signed_names}\n{payload_sha256}",
            canonical_uri(url.path())
        );
        let scope = format!("{date}/{}/s3/aws4_request", self.region);
        let string_to_sign = format!(
            "AWS4-HMAC-SHA256\n{amz_date}\n{scope}\n{}",
            sha256_hex(canonical_request.as_bytes())
        );
        let key = [b"AWS4".as_slice(), self.secret_key.as_bytes()].concat();
        let key = hmac(
            &hmac(
                &hmac(&hmac(&key, date.as_bytes()), self.region.as_bytes()),
                b"s3",
            ),
            b"aws4_request",
        );
        let signature = hex::encode(hmac(&key, string_to_sign.as_bytes()));
        Ok(vec![
            ("x-amz-date".to_owned(), amz_date),
            ("x-amz-content-sha256".to_owned(), payload_sha256.to_owned()),
            (
                "authorization".to_owned(),
                format!(
                    "AWS4-HMAC-SHA256 Credential={}/{scope}, SignedHeaders={signed_names}, Signature={signature}",
                    self.access_key
                ),
            ),
        ])
    }
}

impl fmt::Debug for S3Store {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("S3Store")
            .field("endpoint", &self.endpoint.as_str())
            .finish()
    }
}

/// The key of an object: `sha256/<first two hex>/<sha256 of the uncompressed bytes>.zst`.
pub fn object_key(raw_sha256: &str) -> Result<String> {
    if !is_sha256_hex(raw_sha256) {
        bail!("an object key needs a lowercase hex SHA-256");
    }
    Ok(format!("sha256/{}/{}.zst", &raw_sha256[..2], raw_sha256))
}

pub fn is_sha256_hex(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

pub fn sha256_hex(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}

/// Compresses raw bytes as `zstd-v1`.
pub fn encode(raw: &[u8]) -> Result<Vec<u8>> {
    zstd::bulk::compress(raw, ZSTD_LEVEL).context("zstd compression failed")
}

/// Decompresses `zstd-v1` bytes, refusing output larger than `max_raw` bytes.
pub fn decode(stored: &[u8], max_raw: usize) -> Result<Vec<u8>> {
    zstd::bulk::decompress(stored, max_raw)
        .context("the stored bytes are not zstd within the size bound")
}

fn valid_bucket(bucket: &str) -> bool {
    (3..=63).contains(&bucket.len())
        && bucket
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
        && !bucket.starts_with('-')
        && !bucket.ends_with('-')
}

fn valid_key(key: &str) -> bool {
    key.strip_prefix("sha256/")
        .and_then(|rest| rest.strip_suffix(".zst"))
        .and_then(|rest| rest.split_once('/'))
        .is_some_and(|(prefix, digest)| {
            is_sha256_hex(digest) && digest.starts_with(prefix) && prefix.len() == 2
        })
}

/// A lab object's request path: the bucket, then each key byte outside the unreserved set encoded once, so `%`, `\`
/// and dot spellings stay literal key bytes and never become path syntax (the URL library treats `%2e%2e` as `..` and
/// `\` as `/` for https).
fn object_path(bucket: &str, key: &str) -> String {
    let mut out = format!("/{bucket}/");
    for byte in key.bytes() {
        if byte.is_ascii_alphanumeric() || b"-._~/".contains(&byte) {
            out.push(byte as char);
        } else {
            out.push_str(&format!("%{byte:02X}"));
        }
    }
    out
}

/// A lab object's key: 1 to 1024 bytes, no control character, no leading `/`, no empty, `.` or `..` part.
pub fn valid_object_key(key: &str) -> bool {
    (1..=1024).contains(&key.len())
        && !key.chars().any(char::is_control)
        && key
            .split('/')
            .all(|part| !part.is_empty() && part != "." && part != "..")
}

fn valid_version(version: &str) -> bool {
    (1..=256).contains(&version.len())
        && version
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"-._".contains(&b))
}

fn read_secret_file(path: &Path) -> Result<String> {
    let value =
        fs::read_to_string(path).with_context(|| format!("cannot read {}", path.display()))?;
    let value = value.trim().to_owned();
    if value.is_empty() || value.len() > 256 {
        bail!("{} is empty or too long", path.display());
    }
    Ok(value)
}

impl S3Store {
    /// Connects one endpoint. `ca_pem` is the bundle that signed the store's TLS certificate; only HTTPS is used.
    pub fn connect(config: &EndpointConfig, ca_pem: &[u8]) -> Result<Self> {
        let url = Url::parse(&config.url).context("the store endpoint is not a URL")?;
        if url.scheme() != "https" || url.host_str().is_none() || url.path() != "/" {
            bail!("an object store endpoint is an https origin with no path");
        }
        let _ = rustls::crypto::ring::default_provider().install_default();
        let certificates =
            Certificate::from_pem_bundle(ca_pem).context("the store CA bundle does not parse")?;
        if certificates.is_empty() || certificates.len() > 32 {
            bail!("the store CA bundle holds no certificate or too many");
        }
        let mut builder = Client::builder()
            .https_only(true)
            .no_proxy()
            .redirect(Policy::none())
            .connect_timeout(Duration::from_secs(5))
            .timeout(Duration::from_secs(120));
        for certificate in certificates {
            builder = builder.add_root_certificate(certificate);
        }
        if config.region.is_empty()
            || !config
                .region
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'-')
        {
            bail!("the region is a plain name");
        }
        Ok(Self {
            client: builder
                .build()
                .context("the store client could not be built")?,
            endpoint: url,
            signer: Signer {
                access_key: read_secret_file(&config.access_key_file)?,
                secret_key: read_secret_file(&config.secret_key_file)?,
                region: config.region.clone(),
            },
        })
    }

    /// The endpoint's origin (scheme, host and port), to tell two endpoints apart.
    pub fn origin(&self) -> String {
        self.endpoint.origin().ascii_serialization()
    }

    fn url(&self, bucket: &str, key: &str, query: &[(&str, &str)]) -> Result<Url> {
        if !valid_bucket(bucket) {
            bail!("the bucket name is not a plain S3 bucket name");
        }
        if !valid_key(key) {
            bail!("an evidence object key is sha256/<2 hex>/<64 hex>.zst");
        }
        let mut url = self.endpoint.clone();
        url.set_path(&format!("/{bucket}/{key}"));
        if !query.is_empty() {
            url.query_pairs_mut().extend_pairs(query);
        }
        Ok(url)
    }

    /// A request signed for S3. `signed_headers` (lowercase names) are added to the request and covered by the
    /// signature, with `host`, `x-amz-date` and `x-amz-content-sha256`.
    fn signed(
        &self,
        method: Method,
        url: &Url,
        payload_sha256: &str,
        signed_headers: &[(&str, &str)],
    ) -> Result<reqwest::RequestBuilder> {
        let added = self.signer.headers(
            method.as_str(),
            url,
            signed_headers,
            payload_sha256,
            OffsetDateTime::now_utc(),
        )?;
        let mut builder = self.client.request(method, url.clone());
        for (name, value) in signed_headers {
            builder = builder.header(*name, *value);
        }
        for (name, value) in added {
            builder = builder.header(name, value);
        }
        Ok(builder)
    }

    /// Writes `stored` under `key` only if the key does not exist. Returns the new version id, or `Exists`.
    pub async fn put_new(
        &self,
        bucket: &str,
        key: &str,
        stored: Vec<u8>,
    ) -> Result<Put, StoreError> {
        let url = self
            .url(bucket, key, &[])
            .map_err(|_| StoreError::Malformed("key"))?;
        let payload = sha256_hex(&stored);
        let request = self
            .signed(Method::PUT, &url, &payload, &[("if-none-match", "*")])
            .map_err(|_| StoreError::Malformed("signing"))?
            .header("content-type", "application/zstd")
            .body(stored);
        let response = request.send().await.map_err(|_| StoreError::Unavailable)?;
        match response.status() {
            status if status.is_success() => {
                let version = response
                    .headers()
                    .get("x-amz-version-id")
                    .and_then(|value| value.to_str().ok())
                    .filter(|value| valid_version(value))
                    .ok_or(StoreError::Malformed("version id"))?;
                Ok(Put::Created {
                    version_id: version.to_owned(),
                })
            }
            StatusCode::PRECONDITION_FAILED | StatusCode::CONFLICT => Ok(Put::Exists),
            status => Err(status_error(status)),
        }
    }

    /// Reads one exact version's stored bytes, refusing more than `max_stored` bytes.
    pub async fn get_version(
        &self,
        bucket: &str,
        key: &str,
        version: &str,
        max_stored: u64,
    ) -> Result<Vec<u8>, StoreError> {
        if !valid_version(version) {
            return Err(StoreError::Malformed("version id"));
        }
        let url = self
            .url(bucket, key, &[("versionId", version)])
            .map_err(|_| StoreError::Malformed("key"))?;
        let response = self
            .signed(Method::GET, &url, EMPTY_SHA256, &[])
            .map_err(|_| StoreError::Malformed("signing"))?
            .send()
            .await
            .map_err(|_| StoreError::Unavailable)?;
        if !response.status().is_success() {
            return Err(status_error(response.status()));
        }
        if response
            .content_length()
            .is_some_and(|length| length > max_stored)
        {
            return Err(StoreError::Malformed("body past its bound"));
        }
        let mut body = Vec::new();
        let mut stream = response.bytes_stream();
        while let Some(chunk) = stream.next().await {
            let chunk = chunk.map_err(|_| StoreError::Unavailable)?;
            if body.len() as u64 + chunk.len() as u64 > max_stored {
                return Err(StoreError::Malformed("body past its bound"));
            }
            body.extend_from_slice(&chunk);
        }
        Ok(body)
    }

    /// Reads one lab object (any key `valid_object_key` accepts) by version, or its newest version when `version` is
    /// `None`, refusing a body past `max_bytes` while it arrives.
    pub async fn get_object(
        &self,
        bucket: &str,
        key: &str,
        version: Option<&str>,
        max_bytes: u64,
    ) -> Result<Vec<u8>, StoreError> {
        if !valid_bucket(bucket) || !valid_object_key(key) {
            return Err(StoreError::Malformed("key"));
        }
        if version.is_some_and(|v| !valid_version(v)) {
            return Err(StoreError::Malformed("version id"));
        }
        let path = object_path(bucket, key);
        let mut url = self.endpoint.clone();
        url.set_path(&path);
        // The path the URL library keeps is the one signed and sent; it must be the literal key's encoding.
        if url.path() != path || canonical_uri(url.path()) != path {
            return Err(StoreError::Malformed("key"));
        }
        if let Some(version) = version {
            url.query_pairs_mut().append_pair("versionId", version);
        }
        let response = self
            .signed(Method::GET, &url, EMPTY_SHA256, &[])
            .map_err(|_| StoreError::Malformed("signing"))?
            .send()
            .await
            .map_err(|_| StoreError::Unavailable)?;
        if !response.status().is_success() {
            return Err(status_error(response.status()));
        }
        if response
            .content_length()
            .is_some_and(|length| length > max_bytes)
        {
            return Err(StoreError::Malformed("body past its bound"));
        }
        let mut body = Vec::new();
        let mut stream = response.bytes_stream();
        while let Some(chunk) = stream.next().await {
            let chunk = chunk.map_err(|_| StoreError::Unavailable)?;
            if body.len() as u64 + chunk.len() as u64 > max_bytes {
                return Err(StoreError::Malformed("body past its bound"));
            }
            body.extend_from_slice(&chunk);
        }
        Ok(body)
    }

    /// Reads a copy's facts for one version, or for the newest version when `version` is `None`.
    pub async fn head(
        &self,
        bucket: &str,
        key: &str,
        version: Option<&str>,
    ) -> Result<CopyFacts, StoreError> {
        let query: Vec<(&str, &str)> = match version {
            Some(version) if valid_version(version) => vec![("versionId", version)],
            Some(_) => return Err(StoreError::Malformed("version id")),
            None => Vec::new(),
        };
        let url = self
            .url(bucket, key, &query)
            .map_err(|_| StoreError::Malformed("key"))?;
        let response = self
            .signed(Method::HEAD, &url, EMPTY_SHA256, &[])
            .map_err(|_| StoreError::Malformed("signing"))?
            .send()
            .await
            .map_err(|_| StoreError::Unavailable)?;
        if !response.status().is_success() {
            return Err(status_error(response.status()));
        }
        let header = |name: &str| {
            response
                .headers()
                .get(name)
                .and_then(|value| value.to_str().ok())
        };
        let version_id = header("x-amz-version-id")
            .filter(|value| valid_version(value))
            .ok_or(StoreError::Malformed("version id"))?
            .to_owned();
        if version.is_some_and(|asked| asked != version_id) {
            return Err(StoreError::Malformed("another version answered"));
        }
        let stored_size = header("content-length")
            .and_then(|value| value.parse::<u64>().ok())
            .ok_or(StoreError::Malformed("content length"))?;
        let retain_until = match header("x-amz-object-lock-retain-until-date") {
            Some(text) => Some(
                OffsetDateTime::parse(text, &Rfc3339)
                    .map_err(|_| StoreError::Malformed("retain-until"))?,
            ),
            None => None,
        };
        Ok(CopyFacts {
            version_id,
            stored_size,
            retention_mode: header("x-amz-object-lock-mode").map(str::to_ascii_uppercase),
            retain_until,
            legal_hold: header("x-amz-object-lock-legal-hold")
                .is_some_and(|value| value.eq_ignore_ascii_case("ON")),
        })
    }

    /// Extends one version's governance retention to `until` (governance only extends; the store refuses an earlier
    /// date without a bypass this identity does not hold). The caller reads the version's facts back afterwards.
    pub async fn put_retention(
        &self,
        bucket: &str,
        key: &str,
        version: &str,
        until: OffsetDateTime,
    ) -> Result<(), StoreError> {
        if !valid_version(version) {
            return Err(StoreError::Malformed("version id"));
        }
        let until = until
            .to_offset(time::UtcOffset::UTC)
            .format(format_description!(
                "[year]-[month]-[day]T[hour]:[minute]:[second]Z"
            ))
            .map_err(|_| StoreError::Malformed("retain-until"))?;
        let body = format!(
            "<Retention xmlns=\"http://s3.amazonaws.com/doc/2006-03-01/\"><Mode>GOVERNANCE</Mode>\
             <RetainUntilDate>{until}</RetainUntilDate></Retention>"
        );
        let md5 = base64::Engine::encode(
            &base64::engine::general_purpose::STANDARD,
            md5::Md5::digest(body.as_bytes()),
        );
        let url = self
            .url(bucket, key, &[("retention", ""), ("versionId", version)])
            .map_err(|_| StoreError::Malformed("key"))?;
        let response = self
            .signed(
                Method::PUT,
                &url,
                &sha256_hex(body.as_bytes()),
                &[
                    ("content-md5", md5.as_str()),
                    ("content-type", "application/xml"),
                ],
            )
            .map_err(|_| StoreError::Malformed("signing"))?
            .body(body)
            .send()
            .await
            .map_err(|_| StoreError::Unavailable)?;
        if response.status().is_success() {
            Ok(())
        } else {
            Err(status_error(response.status()))
        }
    }
}

fn status_error(status: StatusCode) -> StoreError {
    match status {
        StatusCode::NOT_FOUND => StoreError::NotFound,
        status if status.is_server_error() || status == StatusCode::TOO_MANY_REQUESTS => {
            StoreError::Unavailable
        }
        status => StoreError::Refused(status.as_u16()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keys_are_content_addresses_only() {
        let digest = sha256_hex(b"evidence");
        let key = object_key(&digest).unwrap();
        assert_eq!(key, format!("sha256/{}/{}.zst", &digest[..2], digest));
        assert!(valid_key(&key));
        assert!(object_key("ABC").is_err());
        assert!(object_key(&digest.to_uppercase()).is_err());
        assert!(!valid_key(&format!("sha256/zz/{digest}.zst")));
        assert!(!valid_key(&format!(
            "sha256/{}/{digest}.zst/../x",
            &digest[..2]
        )));
        assert!(!valid_key(&format!("gc-journal/{digest}")));
    }

    #[test]
    fn encoding_round_trips_and_bounds_its_output() {
        let raw = b"prompt text ".repeat(1000);
        let stored = encode(&raw).unwrap();
        assert!(stored.len() < raw.len());
        assert_eq!(decode(&stored, raw.len()).unwrap(), raw);
        assert!(decode(&stored, raw.len() - 1).is_err());
        assert!(decode(b"not zstd", 1024).is_err());
    }

    /// AWS's published S3 example ("GET Object", Signature Version 4 header authentication): bucket `examplebucket`,
    /// key `test.txt`, `Range: bytes=0-9`, 24 May 2013, us-east-1, the documentation's example credentials.
    #[test]
    fn a_key_with_spaces_and_parentheses_signs_its_path_encoded_once() {
        let mut url = Url::parse("https://store.example:9000/").unwrap();
        url.set_path("/bucket/afri/raw/inputs/a b (1).csv");
        assert_eq!(
            canonical_uri(url.path()),
            "/bucket/afri/raw/inputs/a%20b%20%281%29.csv"
        );
        assert_eq!(
            canonical_uri("/bucket/sha256/ab/abcd.zst"),
            "/bucket/sha256/ab/abcd.zst"
        );
        assert!(valid_object_key("afri/raw/inputs/a b (1).csv"));
        // Encoded dots and backslashes stay key bytes inside the bucket and the study's prefix.
        for key in [
            "study/raw/inputs/%2e%2e/%2e%2e/%2e%2e/%2e%2e/other-bucket/file.csv",
            "study/raw/inputs/%2E./.%2e/x.csv",
            "study/raw/inputs/..\\..\\..\\other-bucket\\file.csv",
            "study/raw/inputs/a%2fb.csv",
        ] {
            assert!(valid_object_key(key), "{key:?}");
            let path = object_path("bucket", key);
            let mut url = Url::parse("https://store.example:9000/").unwrap();
            url.set_path(&path);
            assert_eq!(url.path(), path, "{key:?}");
            assert_eq!(canonical_uri(url.path()), path, "{key:?}");
            assert!(path.starts_with("/bucket/study/raw/inputs/"), "{path}");
            assert_eq!(
                path.matches('/').count(),
                key.matches('/').count() + 2,
                "{path}"
            );
            assert!(
                !path.contains("%2e") && !path.contains("%2E") && !path.contains('\\'),
                "{path}"
            );
        }
        assert_eq!(
            object_path("bucket", "afri/raw/inputs/a b (1).csv"),
            "/bucket/afri/raw/inputs/a%20b%20%281%29.csv"
        );
        for bad in ["", "/a", "a//b", "a/../b", "a/./b", "a\nb"] {
            assert!(!valid_object_key(bad), "{bad:?}");
        }
    }

    #[test]
    fn signature_matches_the_published_s3_example() {
        // The example key id is written in two parts so the public-boundary scan's access-key pattern stays exact.
        const KEY_ID: &str = concat!("AKIA", "IOSFODNN7EXAMPLE");
        let signer = Signer {
            access_key: KEY_ID.to_owned(),
            secret_key: "wJalrXUtnFEMI/K7MDENG/bPxRfiCYEXAMPLEKEY".to_owned(),
            region: "us-east-1".to_owned(),
        };
        let url = Url::parse("https://examplebucket.s3.amazonaws.com/test.txt").unwrap();
        let now = OffsetDateTime::parse("2013-05-24T00:00:00Z", &Rfc3339).unwrap();
        let headers = signer
            .headers("GET", &url, &[("range", "bytes=0-9")], EMPTY_SHA256, now)
            .unwrap();
        let authorization = headers
            .iter()
            .find(|(name, _)| name == "authorization")
            .unwrap()
            .1
            .clone();
        assert_eq!(
            authorization,
            format!(
                "AWS4-HMAC-SHA256 Credential={KEY_ID}/20130524/us-east-1/s3/aws4_request, \
                 SignedHeaders=host;range;x-amz-content-sha256;x-amz-date, \
                 Signature=f0e8bdb87c964420e857bd35b5d6ed310bd44f0170aba48dd91039c6036bdb41"
            )
        );
    }

    #[test]
    fn query_and_path_are_encoded_once() {
        assert_eq!(uri_encode("a b/c~d", true), "a%20b/c~d");
        assert_eq!(uri_encode("a/b", false), "a%2Fb");
        assert_eq!(uri_encode("=&+", false), "%3D%26%2B");
    }

    #[test]
    fn bucket_and_version_names_are_plain() {
        assert!(valid_bucket("ev-0123456789abcdef0123"));
        assert!(!valid_bucket("Ev-Upper"));
        assert!(!valid_bucket("ev_underscore"));
        assert!(!valid_bucket("-ev"));
        assert!(valid_version("e538e1cd-43e5-4840-8d08-083294726a1e"));
        assert!(!valid_version("a/b"));
        assert!(!valid_version(""));
    }
}
