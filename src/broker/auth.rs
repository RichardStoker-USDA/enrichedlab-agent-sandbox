use std::{
    collections::BTreeMap,
    ffi::{CString, OsString},
    fmt,
    fs::File,
    io::Read,
    path::{Component, Path, PathBuf},
    sync::Arc,
    time::Duration,
};

use async_trait::async_trait;
use axum::http::{HeaderMap, HeaderName, header};
use jsonwebtoken::{Algorithm, DecodingKey, Validation, decode, decode_header, jwk::JwkSet};
use reqwest::{Certificate, Client, Url, redirect::Policy};
use serde::{Deserialize, Serialize};
use tokio::sync::RwLock;
use tokio::time::Instant;

use super::model::{PrincipalClass, VerifiedIdentity};

const ACCESS_ASSERTION: HeaderName = HeaderName::from_static("cf-access-jwt-assertion");
const MAX_TOKEN_BYTES: usize = 16 * 1024;
const MAX_JWKS_BYTES: usize = 1024 * 1024;
const MAX_CONFIG_BYTES: usize = 64 * 1024;
const MAX_MAPPINGS: usize = 64;
const MAX_ID_BYTES: usize = 512;
const MAX_EMAIL_BYTES: usize = 320;
const MAX_DISPLAY_NAME_BYTES: usize = 512;
const SERVICE_ACCOUNT_ISSUER: &str = "kubernetes://serviceaccount";
pub(crate) const CAMPAIGN_ISSUER: &str = "urn:enrichedlab:campaign";
const CAMPAIGN_EMAIL_DOMAIN: &str = "@campaign.invalid";
const SERVICE_ACCOUNT_SCHEMA: &str = "enrichedlab.broker.service-account-auth/v1";
const EVIDENCE_PRODUCERS_SCHEMA: &str = "enrichedlab.broker.evidence-producers/v1";

#[derive(Clone, Copy, Debug)]
pub enum AuthError {
    MissingAssertion,
    InvalidAssertion,
    IdentityUnavailable,
    KeyServiceUnavailable,
    ServiceAccountUnavailable,
    ServiceAccountNotAllowed,
    AccessAssertionNotAllowed,
    CampaignIdentityNotAllowed,
}

impl AuthError {
    pub fn code(&self) -> &'static str {
        match self {
            Self::MissingAssertion => "access_assertion_missing",
            Self::InvalidAssertion => "access_assertion_invalid",
            Self::IdentityUnavailable => "access_identity_missing",
            Self::KeyServiceUnavailable => "access_keys_unavailable",
            Self::ServiceAccountUnavailable => "service_account_review_unavailable",
            Self::ServiceAccountNotAllowed => "service_account_authentication_denied",
            Self::AccessAssertionNotAllowed => "access_assertion_not_allowed",
            Self::CampaignIdentityNotAllowed => "campaign_authentication_denied",
        }
    }
}

impl fmt::Display for AuthError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::MissingAssertion => "Cloudflare Access assertion is missing",
            Self::InvalidAssertion => "Cloudflare Access assertion is invalid",
            Self::IdentityUnavailable => "verified Access identity is incomplete",
            Self::KeyServiceUnavailable => "Cloudflare Access signing keys are unavailable",
            Self::ServiceAccountUnavailable => "Kubernetes service-account review is unavailable",
            Self::ServiceAccountNotAllowed => "Kubernetes service-account identity is not allowed",
            Self::AccessAssertionNotAllowed => {
                "Cloudflare Access assertions are not allowed for this API"
            }
            Self::CampaignIdentityNotAllowed => "campaign principals cannot authenticate",
        })
    }
}

impl std::error::Error for AuthError {}

#[async_trait]
pub trait AuthVerifier: Send + Sync {
    async fn verify(&self, headers: &HeaderMap) -> Result<VerifiedIdentity, AuthError>;
}

#[derive(Clone)]
pub struct CloudflareAccessVerifier {
    issuer: String,
    audience: String,
    jwks_url: Url,
    client: Client,
    cache: Arc<RwLock<Option<CachedKeys>>>,
    cache_ttl: Duration,
}

struct CachedKeys {
    fetched_at: Instant,
    keys: JwkSet,
}

#[derive(Debug, Deserialize)]
struct AccessClaims {
    iss: String,
    sub: String,
    email: String,
    #[serde(default, alias = "name")]
    display_name: Option<String>,
    exp: u64,
    #[serde(default)]
    nbf: Option<u64>,
    aud: serde_json::Value,
}

impl CloudflareAccessVerifier {
    pub fn new(
        issuer: String,
        audience: String,
        jwks_url: Url,
        jwks_ca_pem: Option<&[u8]>,
    ) -> Result<Self, AuthError> {
        if issuer == CAMPAIGN_ISSUER {
            return Err(AuthError::CampaignIdentityNotAllowed);
        }
        let _ = rustls::crypto::ring::default_provider().install_default();
        let mut client = Client::builder()
            .https_only(true)
            .redirect(Policy::none())
            .connect_timeout(Duration::from_secs(5))
            .timeout(Duration::from_secs(10));
        if let Some(pem) = jwks_ca_pem {
            let certificates =
                Certificate::from_pem_bundle(pem).map_err(|_| AuthError::KeyServiceUnavailable)?;
            if certificates.is_empty() || certificates.len() > 32 {
                return Err(AuthError::KeyServiceUnavailable);
            }
            for certificate in certificates {
                client = client.add_root_certificate(certificate);
            }
        }
        let client = client
            .build()
            .map_err(|_| AuthError::KeyServiceUnavailable)?;
        Ok(Self {
            issuer,
            audience,
            jwks_url,
            client,
            cache: Arc::new(RwLock::new(None)),
            cache_ttl: Duration::from_secs(15 * 60),
        })
    }

    async fn decoding_key(&self, key_id: &str) -> Result<DecodingKey, AuthError> {
        {
            let cache = self.cache.read().await;
            if let Some(cache) = cache.as_ref()
                && cache.fetched_at.elapsed() < self.cache_ttl
                && let Some(key) = cache.keys.find(key_id)
            {
                return DecodingKey::from_jwk(key).map_err(|_| AuthError::InvalidAssertion);
            }
        }

        let mut cache = self.cache.write().await;
        if let Some(cached) = cache.as_ref()
            && cached.fetched_at.elapsed() < self.cache_ttl
            && let Some(key) = cached.keys.find(key_id)
        {
            return DecodingKey::from_jwk(key).map_err(|_| AuthError::InvalidAssertion);
        }

        let response = self
            .client
            .get(self.jwks_url.clone())
            .header(reqwest::header::ACCEPT, "application/json")
            .send()
            .await
            .map_err(|_| AuthError::KeyServiceUnavailable)?;
        if !response.status().is_success()
            || response
                .content_length()
                .is_some_and(|size| size > MAX_JWKS_BYTES as u64)
        {
            return Err(AuthError::KeyServiceUnavailable);
        }
        let bytes = response
            .bytes()
            .await
            .map_err(|_| AuthError::KeyServiceUnavailable)?;
        if bytes.len() > MAX_JWKS_BYTES {
            return Err(AuthError::KeyServiceUnavailable);
        }
        let keys: JwkSet =
            serde_json::from_slice(&bytes).map_err(|_| AuthError::KeyServiceUnavailable)?;
        if keys.keys.is_empty() || keys.keys.len() > 64 {
            return Err(AuthError::KeyServiceUnavailable);
        }
        let decoding_key = keys
            .find(key_id)
            .ok_or(AuthError::InvalidAssertion)
            .and_then(|key| DecodingKey::from_jwk(key).map_err(|_| AuthError::InvalidAssertion))?;
        *cache = Some(CachedKeys {
            fetched_at: Instant::now(),
            keys,
        });
        Ok(decoding_key)
    }
}

#[async_trait]
impl AuthVerifier for CloudflareAccessVerifier {
    async fn verify(&self, headers: &HeaderMap) -> Result<VerifiedIdentity, AuthError> {
        let token = headers
            .get(&ACCESS_ASSERTION)
            .ok_or(AuthError::MissingAssertion)?
            .to_str()
            .map_err(|_| AuthError::InvalidAssertion)?;
        if token.is_empty() || token.len() > MAX_TOKEN_BYTES {
            return Err(AuthError::InvalidAssertion);
        }
        let header = decode_header(token).map_err(|_| AuthError::InvalidAssertion)?;
        if header.alg != Algorithm::RS256 {
            return Err(AuthError::InvalidAssertion);
        }
        let key_id = header.kid.as_deref().ok_or(AuthError::InvalidAssertion)?;
        if key_id.is_empty() || key_id.len() > MAX_ID_BYTES {
            return Err(AuthError::InvalidAssertion);
        }
        let key = self.decoding_key(key_id).await?;
        let mut validation = Validation::new(Algorithm::RS256);
        validation.set_audience(&[&self.audience]);
        validation.set_issuer(&[&self.issuer]);
        validation.set_required_spec_claims(&["exp", "iss", "sub", "aud"]);
        validation.validate_nbf = true;
        validation.leeway = 30;
        validation.reject_tokens_expiring_in_less_than = 10;
        let claims = decode::<AccessClaims>(token, &key, &validation)
            .map_err(|_| AuthError::InvalidAssertion)?
            .claims;
        if claims.iss != self.issuer
            || claims.sub.is_empty()
            || claims.sub.len() > MAX_ID_BYTES
            || !valid_email(&claims.email)
            || claims.display_name.as_ref().is_some_and(|name| {
                name.len() > MAX_DISPLAY_NAME_BYTES || name.chars().any(char::is_control)
            })
        {
            return Err(AuthError::IdentityUnavailable);
        }
        let _validated_times = (claims.exp, claims.nbf);
        let _validated_audience = claims.aud;
        reject_campaign_identity(VerifiedIdentity {
            issuer: claims.iss,
            class: PrincipalClass::Person,
            subject: claims.sub,
            email: claims.email.to_ascii_lowercase(),
            display_name: claims.display_name,
        })
    }
}

/// Private, opt-in TokenReview configuration. Its mapping is the only source of
/// service-account to catalog-principal identity; requests contribute no tenant or
/// principal claims.
#[derive(Clone, Debug)]
pub struct ServiceAccountAuthConfig {
    api_server: Url,
    private_root: PathBuf,
    ca_file_name: OsString,
    reviewer_token_root: PathBuf,
    reviewer_token_file: String,
    audience: String,
    mappings: BTreeMap<String, VerifiedIdentity>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ServiceAccountAuthDocument {
    schema_version: String,
    api_server: String,
    reviewer_token_root: PathBuf,
    reviewer_token_file: String,
    ca_file: PathBuf,
    audience: String,
    mappings: Vec<ServiceAccountMappingDocument>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ServiceAccountMappingDocument {
    namespace: String,
    service_account: String,
    catalog_email: String,
    #[serde(default)]
    catalog_display_name: Option<String>,
}

impl ServiceAccountAuthConfig {
    pub fn load(path: &Path) -> Result<Self, AuthError> {
        let (private_root, config_file_name) = private_file_reference(path)?;
        let body = read_private_file(
            &private_root,
            &config_file_name,
            MAX_CONFIG_BYTES,
            AuthError::ServiceAccountUnavailable,
        )?;
        let document: ServiceAccountAuthDocument =
            serde_json::from_slice(&body).map_err(|_| AuthError::ServiceAccountUnavailable)?;
        if document.schema_version != SERVICE_ACCOUNT_SCHEMA
            || document.mappings.is_empty()
            || document.mappings.len() > MAX_MAPPINGS
            || !valid_id(&document.audience, 256)
        {
            return Err(AuthError::ServiceAccountUnavailable);
        }
        let api_server = parse_https_origin(&document.api_server)?;
        let (ca_root, ca_file_name) = private_file_reference(&document.ca_file)?;
        if ca_root != private_root
            || !valid_projected_token_root(&document.reviewer_token_root)
            || !valid_filename(&document.reviewer_token_file)
        {
            return Err(AuthError::ServiceAccountUnavailable);
        }
        let mut mappings = BTreeMap::new();
        for mapping in document.mappings {
            if !valid_dns_label(&mapping.namespace)
                || !valid_dns_label(&mapping.service_account)
                || !valid_email(&mapping.catalog_email)
                || mapping
                    .catalog_email
                    .to_ascii_lowercase()
                    .ends_with(CAMPAIGN_EMAIL_DOMAIN)
                || mapping.catalog_display_name.as_ref().is_some_and(|value| {
                    value.len() > MAX_DISPLAY_NAME_BYTES || value.chars().any(char::is_control)
                })
            {
                return Err(AuthError::ServiceAccountUnavailable);
            }
            let subject = service_account_subject(&mapping.namespace, &mapping.service_account);
            let identity = VerifiedIdentity {
                issuer: SERVICE_ACCOUNT_ISSUER.into(),
                class: PrincipalClass::ServiceAccount,
                subject: subject.clone(),
                email: mapping.catalog_email.to_ascii_lowercase(),
                display_name: mapping.catalog_display_name,
            };
            if mappings.insert(subject, identity).is_some() {
                return Err(AuthError::ServiceAccountUnavailable);
            }
        }
        Ok(Self {
            api_server,
            private_root,
            ca_file_name,
            reviewer_token_root: document.reviewer_token_root,
            reviewer_token_file: document.reviewer_token_file,
            audience: document.audience,
            mappings,
        })
    }
}

#[derive(Clone)]
pub struct ServiceAccountTokenVerifier {
    config: ServiceAccountAuthConfig,
    reviewer: Arc<dyn TokenReviewer>,
}

impl ServiceAccountTokenVerifier {
    pub fn new(config: ServiceAccountAuthConfig) -> Result<Self, AuthError> {
        let reviewer = KubernetesTokenReviewer::new(&config)?;
        Ok(Self {
            config,
            reviewer: Arc::new(reviewer),
        })
    }

    #[cfg(test)]
    fn with_reviewer(config: ServiceAccountAuthConfig, reviewer: Arc<dyn TokenReviewer>) -> Self {
        Self { config, reviewer }
    }
}

#[async_trait]
impl AuthVerifier for ServiceAccountTokenVerifier {
    async fn verify(&self, headers: &HeaderMap) -> Result<VerifiedIdentity, AuthError> {
        let token = bearer_token(headers)?;
        let review = self.reviewer.review(token, &self.config.audience).await?;
        if !review.authenticated
            || !review
                .audiences
                .iter()
                .any(|value| value == &self.config.audience)
        {
            return Err(AuthError::ServiceAccountNotAllowed);
        }
        let identity = self
            .config
            .mappings
            .get(&review.user.username)
            .cloned()
            .ok_or(AuthError::ServiceAccountNotAllowed)?;
        reject_campaign_identity(identity)
    }
}

fn reject_campaign_identity(identity: VerifiedIdentity) -> Result<VerifiedIdentity, AuthError> {
    if identity.issuer == CAMPAIGN_ISSUER
        || identity.class == PrincipalClass::Campaign
        || identity
            .email
            .to_ascii_lowercase()
            .ends_with(CAMPAIGN_EMAIL_DOMAIN)
    {
        Err(AuthError::CampaignIdentityNotAllowed)
    } else {
        Ok(identity)
    }
}

/// Cloudflare remains authoritative whenever its assertion header is present. The
/// ServiceAccount verifier is unreachable unless its private configuration exists
/// and the request does not carry an Access assertion.
#[derive(Clone)]
pub struct BrokerAuthVerifier {
    access: Arc<dyn AuthVerifier>,
    service_account: Option<Arc<dyn AuthVerifier>>,
}

impl BrokerAuthVerifier {
    pub fn new(
        access: Arc<dyn AuthVerifier>,
        service_account: Option<Arc<dyn AuthVerifier>>,
    ) -> Self {
        Self {
            access,
            service_account,
        }
    }
}

#[async_trait]
impl AuthVerifier for BrokerAuthVerifier {
    async fn verify(&self, headers: &HeaderMap) -> Result<VerifiedIdentity, AuthError> {
        if headers.contains_key(&ACCESS_ASSERTION) || self.service_account.is_none() {
            return self.access.verify(headers).await;
        }
        self.service_account
            .as_ref()
            .expect("checked service-account verifier")
            .verify(headers)
            .await
    }
}

/// Private ServiceAccount-only mode. It cannot silently accept an Access assertion
/// through the bearer-token path.
#[derive(Clone)]
pub struct ServiceAccountOnlyVerifier {
    service_account: Arc<dyn AuthVerifier>,
}

impl ServiceAccountOnlyVerifier {
    pub fn new(service_account: Arc<dyn AuthVerifier>) -> Self {
        Self { service_account }
    }
}

#[async_trait]
impl AuthVerifier for ServiceAccountOnlyVerifier {
    async fn verify(&self, headers: &HeaderMap) -> Result<VerifiedIdentity, AuthError> {
        if headers.contains_key(&ACCESS_ASSERTION) {
            return Err(AuthError::AccessAssertionNotAllowed);
        }
        self.service_account.verify(headers).await
    }
}

#[async_trait]
trait TokenReviewer: Send + Sync {
    async fn review(&self, token: &str, audience: &str) -> Result<TokenReviewStatus, AuthError>;
}

#[derive(Clone)]
struct KubernetesTokenReviewer {
    client: Client,
    endpoint: Url,
    reviewer_token_root: ProjectedTokenRoot,
}

#[derive(Debug, Clone, Default, Deserialize)]
struct TokenReviewStatus {
    #[serde(default)]
    authenticated: bool,
    #[serde(default)]
    user: TokenReviewUser,
    #[serde(default)]
    audiences: Vec<String>,
}

#[derive(Debug, Clone, Default, Deserialize)]
struct TokenReviewUser {
    #[serde(default)]
    username: String,
}

#[derive(Debug, Deserialize)]
struct TokenReviewResponse {
    #[serde(default)]
    status: TokenReviewStatus,
}

#[derive(Serialize)]
struct TokenReviewRequest<'a> {
    #[serde(rename = "apiVersion")]
    api_version: &'static str,
    kind: &'static str,
    spec: TokenReviewRequestSpec<'a>,
}

#[derive(Serialize)]
struct TokenReviewRequestSpec<'a> {
    token: &'a str,
    audiences: [&'a str; 1],
}

impl KubernetesTokenReviewer {
    fn new(config: &ServiceAccountAuthConfig) -> Result<Self, AuthError> {
        Self::from_parts(
            &config.api_server,
            &config.private_root,
            &config.ca_file_name,
            &config.reviewer_token_root,
            &config.reviewer_token_file,
        )
    }

    fn from_parts(
        api_server: &Url,
        private_root: &Path,
        ca_file_name: &OsString,
        reviewer_token_root: &Path,
        reviewer_token_file: &str,
    ) -> Result<Self, AuthError> {
        let _ = rustls::crypto::ring::default_provider().install_default();
        let pem = read_private_file(
            private_root,
            ca_file_name,
            MAX_JWKS_BYTES,
            AuthError::ServiceAccountUnavailable,
        )?;
        let certificates =
            Certificate::from_pem_bundle(&pem).map_err(|_| AuthError::ServiceAccountUnavailable)?;
        if certificates.is_empty() || certificates.len() > 32 {
            return Err(AuthError::ServiceAccountUnavailable);
        }
        let mut client = Client::builder()
            .https_only(true)
            .no_proxy()
            .redirect(Policy::none())
            .connect_timeout(Duration::from_secs(5))
            .timeout(Duration::from_secs(10));
        for certificate in certificates {
            client = client.add_root_certificate(certificate);
        }
        let client = client
            .build()
            .map_err(|_| AuthError::ServiceAccountUnavailable)?;
        let endpoint = api_server
            .join("/apis/authentication.k8s.io/v1/tokenreviews")
            .map_err(|_| AuthError::ServiceAccountUnavailable)?;
        Ok(Self {
            client,
            endpoint,
            reviewer_token_root: ProjectedTokenRoot::open(
                reviewer_token_root,
                reviewer_token_file,
            )?,
        })
    }
}

/// The evidence append service's producers: a Kubernetes ServiceAccount, proved by a bound token whose audience is
/// the service's, maps to exactly one evidence producer identity. The private configuration is the only source of that
/// mapping; a request names no producer.
#[derive(Clone)]
pub struct EvidenceProducerAuth {
    audience: String,
    producers: BTreeMap<String, String>,
    reviewer: Arc<dyn TokenReviewer>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct EvidenceProducersDocument {
    schema_version: String,
    api_server: String,
    reviewer_token_root: PathBuf,
    reviewer_token_file: String,
    ca_file: PathBuf,
    audience: String,
    producers: Vec<EvidenceProducerMappingDocument>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct EvidenceProducerMappingDocument {
    namespace: String,
    service_account: String,
    producer: String,
}

impl EvidenceProducerAuth {
    pub fn load(path: &Path) -> Result<Self, AuthError> {
        let (private_root, config_file_name) = private_file_reference(path)?;
        let body = read_private_file(
            &private_root,
            &config_file_name,
            MAX_CONFIG_BYTES,
            AuthError::ServiceAccountUnavailable,
        )?;
        let document: EvidenceProducersDocument =
            serde_json::from_slice(&body).map_err(|_| AuthError::ServiceAccountUnavailable)?;
        if document.schema_version != EVIDENCE_PRODUCERS_SCHEMA
            || document.producers.is_empty()
            || document.producers.len() > MAX_MAPPINGS
            || !valid_id(&document.audience, 256)
        {
            return Err(AuthError::ServiceAccountUnavailable);
        }
        let api_server = parse_https_origin(&document.api_server)?;
        let (ca_root, ca_file_name) = private_file_reference(&document.ca_file)?;
        if ca_root != private_root
            || !valid_projected_token_root(&document.reviewer_token_root)
            || !valid_filename(&document.reviewer_token_file)
        {
            return Err(AuthError::ServiceAccountUnavailable);
        }
        let mut producers = BTreeMap::new();
        for mapping in document.producers {
            if !valid_dns_label(&mapping.namespace)
                || !valid_dns_label(&mapping.service_account)
                || !valid_producer_identity(&mapping.producer)
            {
                return Err(AuthError::ServiceAccountUnavailable);
            }
            let subject = service_account_subject(&mapping.namespace, &mapping.service_account);
            if producers.insert(subject, mapping.producer).is_some() {
                return Err(AuthError::ServiceAccountUnavailable);
            }
        }
        let reviewer = KubernetesTokenReviewer::from_parts(
            &api_server,
            &private_root,
            &ca_file_name,
            &document.reviewer_token_root,
            &document.reviewer_token_file,
        )?;
        Ok(Self {
            audience: document.audience,
            producers,
            reviewer: Arc::new(reviewer),
        })
    }

    /// Every producer identity the configuration maps a ServiceAccount to.
    pub fn producers(&self) -> impl Iterator<Item = &str> {
        self.producers.values().map(String::as_str)
    }

    /// Whether the TokenReview API answers this service's reviewer (any answer to a token that is not one counts).
    pub async fn reviewer_available(&self) -> bool {
        self.reviewer
            .review("enrichedlab-evidence-readiness", &self.audience)
            .await
            .is_ok()
    }

    /// The producer identity a bound ServiceAccount token proves, or a refusal.
    pub async fn producer(&self, token: &str) -> Result<String, AuthError> {
        if token.is_empty()
            || token.len() > MAX_TOKEN_BYTES
            || token.chars().any(char::is_whitespace)
        {
            return Err(AuthError::ServiceAccountNotAllowed);
        }
        let review = self.reviewer.review(token, &self.audience).await?;
        if !review.authenticated || !review.audiences.iter().any(|value| value == &self.audience) {
            return Err(AuthError::ServiceAccountNotAllowed);
        }
        self.producers
            .get(&review.user.username)
            .cloned()
            .ok_or(AuthError::ServiceAccountNotAllowed)
    }

    /// For tests: tokens review as the paired ServiceAccount subjects for this audience; any other token does not.
    /// While `down` is set, every review fails as unavailable.
    #[cfg(test)]
    pub(crate) fn for_tests(
        audience: &str,
        tokens: &[(&str, &str)],
        producers: &[(&str, &str)],
        down: Arc<std::sync::atomic::AtomicBool>,
    ) -> Self {
        #[derive(Clone)]
        struct Tokens(
            String,
            BTreeMap<String, String>,
            Arc<std::sync::atomic::AtomicBool>,
        );
        #[async_trait]
        impl TokenReviewer for Tokens {
            async fn review(
                &self,
                token: &str,
                _audience: &str,
            ) -> Result<TokenReviewStatus, AuthError> {
                if self.2.load(std::sync::atomic::Ordering::Relaxed) {
                    return Err(AuthError::ServiceAccountUnavailable);
                }
                // A token starting "slow-" reviews like its suffix after two seconds, to hold a call in flight.
                let token = match token.strip_prefix("slow-") {
                    Some(rest) => {
                        tokio::time::sleep(std::time::Duration::from_secs(2)).await;
                        rest
                    }
                    None => token,
                };
                Ok(match self.1.get(token) {
                    Some(subject) => TokenReviewStatus {
                        authenticated: true,
                        user: TokenReviewUser {
                            username: subject.clone(),
                        },
                        audiences: vec![self.0.clone()],
                    },
                    None => TokenReviewStatus::default(),
                })
            }
        }
        let tokens = tokens
            .iter()
            .map(|(token, subject)| ((*token).into(), (*subject).into()))
            .collect();
        Self::with_reviewer(
            audience,
            producers,
            Arc::new(Tokens(audience.into(), tokens, down)),
        )
    }

    #[cfg(test)]
    fn with_reviewer(
        audience: &str,
        producers: &[(&str, &str)],
        reviewer: Arc<dyn TokenReviewer>,
    ) -> Self {
        Self {
            audience: audience.into(),
            producers: producers
                .iter()
                .map(|(subject, producer)| ((*subject).into(), (*producer).into()))
                .collect(),
            reviewer,
        }
    }
}

/// The evidence record's producer identity syntax (migration 075).
fn valid_producer_identity(value: &str) -> bool {
    (3..=256).contains(&value.len())
        && value
            .bytes()
            .next()
            .is_some_and(|byte| byte.is_ascii_lowercase())
        && value.bytes().all(|byte| {
            byte.is_ascii_lowercase()
                || byte.is_ascii_digit()
                || matches!(byte, b':' | b'/' | b'.' | b'_' | b'-')
        })
}

#[async_trait]
impl TokenReviewer for KubernetesTokenReviewer {
    async fn review(&self, token: &str, audience: &str) -> Result<TokenReviewStatus, AuthError> {
        let reviewer_token = self.reviewer_token_root.read(MAX_TOKEN_BYTES)?;
        let reviewer_token = std::str::from_utf8(&reviewer_token)
            .map_err(|_| AuthError::ServiceAccountUnavailable)?
            .trim();
        if reviewer_token.is_empty() || reviewer_token.len() > MAX_TOKEN_BYTES {
            return Err(AuthError::ServiceAccountUnavailable);
        }
        let response = self
            .client
            .post(self.endpoint.clone())
            .bearer_auth(reviewer_token)
            .json(&TokenReviewRequest {
                api_version: "authentication.k8s.io/v1",
                kind: "TokenReview",
                spec: TokenReviewRequestSpec {
                    token,
                    audiences: [audience],
                },
            })
            .send()
            .await
            .map_err(|_| AuthError::ServiceAccountUnavailable)?;
        if !response.status().is_success()
            || response
                .content_length()
                .is_some_and(|size| size > MAX_CONFIG_BYTES as u64)
        {
            return Err(AuthError::ServiceAccountUnavailable);
        }
        let bytes = response
            .bytes()
            .await
            .map_err(|_| AuthError::ServiceAccountUnavailable)?;
        if bytes.len() > MAX_CONFIG_BYTES {
            return Err(AuthError::ServiceAccountUnavailable);
        }
        serde_json::from_slice::<TokenReviewResponse>(&bytes)
            .map(|response| response.status)
            .map_err(|_| AuthError::ServiceAccountUnavailable)
    }
}

fn bearer_token(headers: &HeaderMap) -> Result<&str, AuthError> {
    let value = headers
        .get(header::AUTHORIZATION)
        .ok_or(AuthError::ServiceAccountNotAllowed)?
        .to_str()
        .map_err(|_| AuthError::ServiceAccountNotAllowed)?;
    let token = value
        .strip_prefix("Bearer ")
        .ok_or(AuthError::ServiceAccountNotAllowed)?;
    if token.is_empty() || token.len() > MAX_TOKEN_BYTES || token.chars().any(char::is_whitespace) {
        return Err(AuthError::ServiceAccountNotAllowed);
    }
    Ok(token)
}

fn parse_https_origin(value: &str) -> Result<Url, AuthError> {
    let url = Url::parse(value).map_err(|_| AuthError::ServiceAccountUnavailable)?;
    if url.scheme() != "https"
        || url.host_str().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
        || url.path() != "/"
        || url.query().is_some()
        || url.fragment().is_some()
    {
        return Err(AuthError::ServiceAccountUnavailable);
    }
    Ok(url)
}

fn private_file_reference(path: &Path) -> Result<(PathBuf, OsString), AuthError> {
    if !path.is_absolute() || path == Path::new("/") {
        return Err(AuthError::ServiceAccountUnavailable);
    }
    let name = path
        .file_name()
        .filter(|name| valid_filename(&name.to_string_lossy()))
        .ok_or(AuthError::ServiceAccountUnavailable)?
        .to_os_string();
    let root = path.parent().ok_or(AuthError::ServiceAccountUnavailable)?;
    if root == Path::new("/")
        || root
            .components()
            .any(|part| matches!(part, Component::ParentDir))
    {
        return Err(AuthError::ServiceAccountUnavailable);
    }
    Ok((root.to_path_buf(), name))
}

fn read_private_file(
    root: &Path,
    name: &OsString,
    maximum: usize,
    error: AuthError,
) -> Result<Vec<u8>, AuthError> {
    let name = name.to_str().ok_or(error)?;
    let file = super::admission::open_bounded_record(
        root,
        name,
        unsafe { libc::geteuid() },
        maximum as u64,
    )
    .map_err(|_| error)?;
    let mut bytes = Vec::with_capacity(maximum.min(4096));
    file.take(maximum as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| error)?;
    if bytes.is_empty() || bytes.len() > maximum {
        return Err(error);
    }
    Ok(bytes)
}

#[derive(Clone)]
struct ProjectedTokenRoot {
    directory: Arc<File>,
    name: CString,
}

impl ProjectedTokenRoot {
    fn open(root: &Path, name: &str) -> Result<Self, AuthError> {
        if !valid_projected_token_root(root) || !valid_filename(name) {
            return Err(AuthError::ServiceAccountUnavailable);
        }
        #[cfg(unix)]
        {
            use std::{
                fs::OpenOptions,
                os::{
                    fd::{AsRawFd, FromRawFd},
                    unix::{
                        ffi::OsStrExt,
                        fs::{MetadataExt, OpenOptionsExt},
                    },
                },
            };
            let mut directory = OpenOptions::new()
                .read(true)
                .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC)
                .open("/")
                .map_err(|_| AuthError::ServiceAccountUnavailable)?;
            for part in root.components() {
                let Component::Normal(part) = part else {
                    if matches!(part, Component::RootDir) {
                        continue;
                    }
                    return Err(AuthError::ServiceAccountUnavailable);
                };
                let part = CString::new(part.as_bytes())
                    .map_err(|_| AuthError::ServiceAccountUnavailable)?;
                let descriptor = unsafe {
                    libc::openat(
                        directory.as_raw_fd(),
                        part.as_ptr(),
                        libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC,
                    )
                };
                if descriptor < 0 {
                    return Err(AuthError::ServiceAccountUnavailable);
                }
                directory = unsafe { File::from_raw_fd(descriptor) };
            }
            let metadata = directory
                .metadata()
                .map_err(|_| AuthError::ServiceAccountUnavailable)?;
            if !metadata.is_dir() || metadata.uid() != 0 {
                return Err(AuthError::ServiceAccountUnavailable);
            }
            if metadata.mode() & 0o022 != 0 {
                // Kubelet's projected tmpfs root keeps group/other write bits,
                // but the container receives a read-only mount. Inspect the
                // held descriptor's mount instead of rejecting that valid layout.
                let mut filesystem = std::mem::MaybeUninit::<libc::statvfs>::uninit();
                if unsafe { libc::fstatvfs(directory.as_raw_fd(), filesystem.as_mut_ptr()) } != 0
                    || unsafe { filesystem.assume_init() }.f_flag & libc::ST_RDONLY == 0
                {
                    return Err(AuthError::ServiceAccountUnavailable);
                }
            }
            Ok(Self {
                directory: Arc::new(directory),
                name: CString::new(name).map_err(|_| AuthError::ServiceAccountUnavailable)?,
            })
        }
        #[cfg(not(unix))]
        {
            let _ = (root, name);
            Err(AuthError::ServiceAccountUnavailable)
        }
    }

    fn read(&self, maximum: usize) -> Result<Vec<u8>, AuthError> {
        #[cfg(unix)]
        {
            use std::os::fd::{AsRawFd, FromRawFd};
            let expected = format!("..data/{}", self.name.to_string_lossy());
            let mut target = [0u8; 256];
            let length = unsafe {
                libc::readlinkat(
                    self.directory.as_raw_fd(),
                    self.name.as_ptr(),
                    target.as_mut_ptr().cast(),
                    target.len(),
                )
            };
            if length <= 0 || target[..length as usize] != *expected.as_bytes() {
                return Err(AuthError::ServiceAccountUnavailable);
            }
            // Kubernetes rotates a projected token by replacing only ..data. The
            // held root is not writable by the API process; this one expected link
            // is the narrow exception to no-follow file reads.
            let data = CString::new("..data").unwrap();
            let data_fd = unsafe {
                libc::openat(
                    self.directory.as_raw_fd(),
                    data.as_ptr(),
                    libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC,
                )
            };
            if data_fd < 0 {
                return Err(AuthError::ServiceAccountUnavailable);
            }
            let data = unsafe { File::from_raw_fd(data_fd) };
            let descriptor = unsafe {
                libc::openat(
                    data.as_raw_fd(),
                    self.name.as_ptr(),
                    libc::O_RDONLY | libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC,
                )
            };
            if descriptor < 0 {
                return Err(AuthError::ServiceAccountUnavailable);
            }
            let token = unsafe { File::from_raw_fd(descriptor) };
            let metadata = token
                .metadata()
                .map_err(|_| AuthError::ServiceAccountUnavailable)?;
            if !metadata.is_file() || metadata.len() == 0 || metadata.len() > maximum as u64 {
                return Err(AuthError::ServiceAccountUnavailable);
            }
            let mut bytes = Vec::with_capacity(metadata.len() as usize);
            token
                .take(maximum as u64 + 1)
                .read_to_end(&mut bytes)
                .map_err(|_| AuthError::ServiceAccountUnavailable)?;
            if bytes.is_empty() || bytes.len() > maximum {
                return Err(AuthError::ServiceAccountUnavailable);
            }
            Ok(bytes)
        }
        #[cfg(not(unix))]
        {
            let _ = maximum;
            Err(AuthError::ServiceAccountUnavailable)
        }
    }
}

fn valid_projected_token_root(path: &Path) -> bool {
    path.is_absolute()
        && path != Path::new("/")
        && !path
            .components()
            .any(|part| matches!(part, Component::ParentDir))
}

fn valid_filename(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 255
        && value != "."
        && value != ".."
        && !value.contains('/')
        && !value.chars().any(char::is_control)
}

fn service_account_subject(namespace: &str, service_account: &str) -> String {
    format!("system:serviceaccount:{namespace}:{service_account}")
}

fn valid_dns_label(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 63
        && value
            .bytes()
            .next()
            .is_some_and(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit())
        && value
            .bytes()
            .last()
            .is_some_and(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit())
        && value
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
}

fn valid_id(value: &str, maximum: usize) -> bool {
    !value.is_empty()
        && value.len() <= maximum
        && !value
            .chars()
            .any(|character| character.is_control() || character.is_whitespace())
}

fn valid_email(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= MAX_EMAIL_BYTES
        && value.contains('@')
        && !value
            .chars()
            .any(|character| character.is_control() || character.is_whitespace())
}

#[cfg(test)]
mod tests {
    use std::{collections::BTreeMap, fs, os::unix::fs::PermissionsExt, sync::Arc};

    use async_trait::async_trait;
    use axum::http::{HeaderMap, HeaderValue};

    use super::{
        ACCESS_ASSERTION, AuthError, AuthVerifier, BrokerAuthVerifier, CloudflareAccessVerifier,
        EvidenceProducerAuth, PrincipalClass, SERVICE_ACCOUNT_ISSUER, SERVICE_ACCOUNT_SCHEMA,
        ServiceAccountAuthConfig, ServiceAccountOnlyVerifier, ServiceAccountTokenVerifier,
        TokenReviewRequest, TokenReviewRequestSpec, TokenReviewStatus, TokenReviewUser,
        TokenReviewer, VerifiedIdentity, valid_producer_identity,
    };

    fn review(authenticated: bool, username: &str, audiences: &[&str]) -> TokenReviewStatus {
        TokenReviewStatus {
            authenticated,
            user: TokenReviewUser {
                username: username.into(),
            },
            audiences: audiences.iter().map(|value| (*value).into()).collect(),
        }
    }

    #[tokio::test]
    async fn evidence_producer_comes_only_from_the_reviewed_service_account() {
        let subject = "system:serviceaccount:openshell-candidate:openshell-run-receipts";
        let producers = [(subject, "k8s:openshell-candidate/openshell-run-receipts")];
        let auth = |status| {
            EvidenceProducerAuth::with_reviewer(
                "enrichedlab-evidence",
                &producers,
                Arc::new(StaticReviewer(status)),
            )
        };
        assert_eq!(
            auth(review(true, subject, &["enrichedlab-evidence"]))
                .producer("token")
                .await
                .unwrap(),
            "k8s:openshell-candidate/openshell-run-receipts"
        );
        for status in [
            review(false, subject, &["enrichedlab-evidence"]),
            review(true, subject, &["enrichedlab-broker-submit"]),
            review(
                true,
                "system:serviceaccount:other:openshell-run-receipts",
                &["enrichedlab-evidence"],
            ),
        ] {
            assert!(matches!(
                auth(status).producer("token").await,
                Err(AuthError::ServiceAccountNotAllowed)
            ));
        }
        for token in ["", "two words", &"x".repeat(16 * 1024 + 1)] {
            assert!(matches!(
                auth(review(true, subject, &["enrichedlab-evidence"]))
                    .producer(token)
                    .await,
                Err(AuthError::ServiceAccountNotAllowed)
            ));
        }
    }

    #[test]
    fn evidence_producer_identities_follow_the_record_syntax() {
        assert!(valid_producer_identity(
            "k8s:openshell-candidate/openshell-run-receipts"
        ));
        assert!(valid_producer_identity("broker/api"));
        for value in [
            "",
            "ab",
            "K8s:x",
            "1abc",
            "k8s:ns/sa with space",
            &"a".repeat(257),
        ] {
            assert!(!valid_producer_identity(value), "{value}");
        }
    }

    #[derive(Clone)]
    struct StaticReviewer(TokenReviewStatus);

    #[async_trait]
    impl TokenReviewer for StaticReviewer {
        async fn review(
            &self,
            _token: &str,
            _audience: &str,
        ) -> Result<TokenReviewStatus, AuthError> {
            Ok(self.0.clone())
        }
    }

    struct RejectingVerifier;

    #[async_trait]
    impl AuthVerifier for RejectingVerifier {
        async fn verify(&self, _headers: &HeaderMap) -> Result<VerifiedIdentity, AuthError> {
            Err(AuthError::InvalidAssertion)
        }
    }

    struct NeverVerifier;

    #[async_trait]
    impl AuthVerifier for NeverVerifier {
        async fn verify(&self, _headers: &HeaderMap) -> Result<VerifiedIdentity, AuthError> {
            panic!("ServiceAccount verifier must not run when Cloudflare Access is present")
        }
    }

    fn config() -> ServiceAccountAuthConfig {
        let subject = "system:serviceaccount:qualification-retained-byte:task-submit".to_string();
        ServiceAccountAuthConfig {
            api_server: "https://kubernetes.example.invalid".parse().unwrap(),
            private_root: "/run/private-token-review".into(),
            ca_file_name: "ca.crt".into(),
            reviewer_token_root: "/run/reviewer".into(),
            reviewer_token_file: "reviewer-token".into(),
            audience: "enrichedlab-broker-submit".into(),
            mappings: BTreeMap::from([(
                subject.clone(),
                VerifiedIdentity {
                    issuer: "kubernetes://serviceaccount".into(),
                    class: PrincipalClass::ServiceAccount,
                    subject,
                    email: "task-submit@example.invalid".into(),
                    display_name: Some("Qualification task submitter".into()),
                },
            )]),
        }
    }

    fn headers() -> HeaderMap {
        let mut headers = HeaderMap::new();
        headers.insert(
            "authorization",
            HeaderValue::from_static("Bearer bounded-token"),
        );
        headers
    }

    #[test]
    fn service_account_config_keeps_ca_in_its_held_private_root() {
        let root = tempfile::tempdir().unwrap();
        let root_path = root.path().canonicalize().unwrap();
        fs::set_permissions(&root_path, fs::Permissions::from_mode(0o700)).unwrap();
        let config_file = root_path.join("service-account-auth.json");
        let ca_file = root_path.join("ca.crt");
        fs::write(&ca_file, "not-used-until-client-construction").unwrap();
        fs::set_permissions(&ca_file, fs::Permissions::from_mode(0o600)).unwrap();
        let mut document = serde_json::json!({
            "schemaVersion": SERVICE_ACCOUNT_SCHEMA,
            "apiServer": "https://kubernetes.example.invalid",
            "reviewerTokenRoot": "/var/run/enrichedlab/reviewer-source",
            "reviewerTokenFile": "reviewer-token",
            "caFile": ca_file,
            "audience": "enrichedlab-broker-submit",
            "mappings": [{
                "namespace": "qualification-retained-byte",
                "serviceAccount": "task-submit",
                "catalogEmail": "task-submit@example.invalid"
            }]
        });
        fs::write(&config_file, serde_json::to_vec(&document).unwrap()).unwrap();
        fs::set_permissions(&config_file, fs::Permissions::from_mode(0o600)).unwrap();
        assert!(
            ServiceAccountAuthConfig::load(&config_file).is_ok(),
            "private config should load"
        );

        let outside = tempfile::tempdir().unwrap();
        let outside_ca = outside.path().canonicalize().unwrap().join("ca.crt");
        fs::write(&outside_ca, "outside-private-root").unwrap();
        fs::set_permissions(&outside_ca, fs::Permissions::from_mode(0o600)).unwrap();
        document["caFile"] = serde_json::Value::String(outside_ca.display().to_string());
        fs::write(&config_file, serde_json::to_vec(&document).unwrap()).unwrap();
        fs::set_permissions(&config_file, fs::Permissions::from_mode(0o600)).unwrap();
        assert!(matches!(
            ServiceAccountAuthConfig::load(&config_file),
            Err(AuthError::ServiceAccountUnavailable)
        ));
    }

    #[cfg(target_os = "linux")]
    #[test]
    #[ignore = "requires read-only and writable mounts from scripts/qualify_projected_token.sh"]
    fn projected_token_mount_preserves_rotation_and_rejects_writable_roots() {
        use std::{os::unix::fs::symlink, path::PathBuf};

        let writer = PathBuf::from(std::env::var("PROJECTED_TOKEN_FIXTURE_WRITER").unwrap());
        let reader = PathBuf::from(std::env::var("PROJECTED_TOKEN_FIXTURE_READER").unwrap());
        let root_metadata = fs::metadata(&reader).unwrap();
        assert_eq!(std::os::unix::fs::MetadataExt::uid(&root_metadata), 0);
        assert_eq!(root_metadata.permissions().mode() & 0o3777, 0o3777);
        assert_eq!(unsafe { libc::geteuid() }, 65532);

        for (generation, token) in [
            ("..generation001", "fixture-one"),
            ("..generation002", "fixture-two"),
        ] {
            let directory = writer.join(generation);
            fs::create_dir(&directory).unwrap();
            fs::write(directory.join("reviewer-token"), token).unwrap();
        }
        symlink("..generation001", writer.join("..data")).unwrap();
        symlink("..data/reviewer-token", writer.join("reviewer-token")).unwrap();

        assert!(super::ProjectedTokenRoot::open(&writer, "reviewer-token").is_err());
        let held = super::ProjectedTokenRoot::open(&reader, "reviewer-token").unwrap();
        assert_eq!(held.read(1024).unwrap(), b"fixture-one");
        symlink("..generation002", writer.join("..data_next")).unwrap();
        fs::rename(writer.join("..data_next"), writer.join("..data")).unwrap();
        assert_eq!(held.read(1024).unwrap(), b"fixture-two");
        assert!(held.read(1).is_err());

        let owned = writer.join("process-owned");
        fs::create_dir(&owned).unwrap();
        assert!(
            super::ProjectedTokenRoot::open(&reader.join("process-owned"), "reviewer-token")
                .is_err()
        );
        symlink(&reader, writer.join("redirected-root")).unwrap();
        assert!(
            super::ProjectedTokenRoot::open(&reader.join("redirected-root"), "reviewer-token")
                .is_err()
        );
        fs::remove_file(writer.join("reviewer-token")).unwrap();
        symlink(
            "..generation002/reviewer-token",
            writer.join("reviewer-token"),
        )
        .unwrap();
        assert!(held.read(1024).is_err());
        fs::remove_file(writer.join("reviewer-token")).unwrap();
        symlink("..data/reviewer-token", writer.join("reviewer-token")).unwrap();

        // Construct the real reviewer too: private CA handling and the TLS
        // client must work with the projected mount, without a network request.
        let private = tempfile::tempdir().unwrap();
        let private_root = private.path().canonicalize().unwrap();
        fs::set_permissions(&private_root, fs::Permissions::from_mode(0o700)).unwrap();
        let certificate =
            rcgen::generate_simple_self_signed(vec!["kubernetes.example.invalid".into()]).unwrap();
        let ca = private_root.join("ca.crt");
        fs::write(&ca, certificate.cert.pem()).unwrap();
        fs::set_permissions(&ca, fs::Permissions::from_mode(0o600)).unwrap();
        let mut configuration = config();
        configuration.private_root = private_root;
        configuration.reviewer_token_root = reader;
        let reviewer = super::KubernetesTokenReviewer::new(&configuration).unwrap();
        assert_eq!(
            reviewer.reviewer_token_root.read(1024).unwrap(),
            b"fixture-two"
        );
    }

    #[test]
    fn production_verifier_requires_an_https_jwks_client() {
        let url = "https://example.cloudflareaccess.com/cdn-cgi/access/certs"
            .parse()
            .expect("valid URL");
        assert!(
            CloudflareAccessVerifier::new(
                "https://example.cloudflareaccess.com".into(),
                "audience".into(),
                url,
                None,
            )
            .is_ok()
        );
    }

    #[test]
    fn access_auth_refuses_the_reserved_campaign_issuer() {
        let url = "https://example.invalid/certs".parse().unwrap();
        assert!(matches!(
            CloudflareAccessVerifier::new(
                super::CAMPAIGN_ISSUER.into(),
                "audience".into(),
                url,
                None,
            ),
            Err(AuthError::CampaignIdentityNotAllowed)
        ));
    }

    #[tokio::test]
    async fn service_account_auth_refuses_a_campaign_mapping() {
        let mut configuration = config();
        let identity = configuration.mappings.values_mut().next().unwrap();
        identity.issuer = super::CAMPAIGN_ISSUER.into();
        identity.class = PrincipalClass::Campaign;
        identity.email = "campaign+synthetic@campaign.invalid".into();
        let verifier = ServiceAccountTokenVerifier::with_reviewer(
            configuration,
            Arc::new(StaticReviewer(TokenReviewStatus {
                authenticated: true,
                user: super::TokenReviewUser {
                    username: "system:serviceaccount:qualification-retained-byte:task-submit"
                        .into(),
                },
                audiences: vec!["enrichedlab-broker-submit".into()],
            })),
        );
        assert!(matches!(
            verifier.verify(&headers()).await,
            Err(AuthError::CampaignIdentityNotAllowed)
        ));
    }

    #[tokio::test]
    async fn verifies_real_access_signature_and_rejects_changed_scope_or_signature() {
        use jsonwebtoken::{Algorithm, EncodingKey, Header, encode, jwk::Jwk};
        use rsa::{RsaPrivateKey, pkcs1::EncodeRsaPrivateKey, rand_core::OsRng};

        // Generate synthetic signing material only in memory. A mocked verifier
        // cannot detect a build with no JWT cryptography provider enabled.
        let private = RsaPrivateKey::new(&mut OsRng, 2048).unwrap();
        let der = private.to_pkcs1_der().unwrap();
        let key = EncodingKey::from_rsa_der(der.as_bytes());
        let mut jwk = Jwk::from_encoding_key(&key, Algorithm::RS256).unwrap();
        jwk.common.key_id = Some("test-signing-key".into());
        let issuer = "https://example.cloudflareaccess.com";
        let verifier = CloudflareAccessVerifier::new(
            issuer.into(),
            "research-audience".into(),
            format!("{issuer}/cdn-cgi/access/certs").parse().unwrap(),
            None,
        )
        .unwrap();
        *verifier.cache.write().await = Some(super::CachedKeys {
            fetched_at: super::Instant::now(),
            keys: jsonwebtoken::jwk::JwkSet { keys: vec![jwk] },
        });
        let mut header = Header::new(Algorithm::RS256);
        header.kid = Some("test-signing-key".into());
        let mut claims = serde_json::json!({
            "iss": issuer, "sub": "synthetic-scientist", "email": "scientist@example.invalid",
            "aud": ["research-audience"], "exp": jsonwebtoken::get_current_timestamp() + 300
        });
        let token = encode(&header, &claims, &key).unwrap();
        let mut headers = HeaderMap::new();
        headers.insert(ACCESS_ASSERTION, HeaderValue::from_str(&token).unwrap());
        let person = verifier.verify(&headers).await.unwrap();
        assert_eq!(person.subject, "synthetic-scientist");
        assert_eq!(person.class, PrincipalClass::Person);

        claims["aud"] = serde_json::json!(["another-application"]);
        headers.insert(
            ACCESS_ASSERTION,
            HeaderValue::from_str(&encode(&header, &claims, &key).unwrap()).unwrap(),
        );
        assert!(matches!(
            verifier.verify(&headers).await,
            Err(AuthError::InvalidAssertion)
        ));

        let mut changed = token.into_bytes();
        let signature_start = changed.iter().rposition(|b| *b == b'.').unwrap() + 1;
        changed[signature_start] = if changed[signature_start] == b'A' {
            b'B'
        } else {
            b'A'
        };
        headers.insert(ACCESS_ASSERTION, HeaderValue::from_bytes(&changed).unwrap());
        assert!(matches!(
            verifier.verify(&headers).await,
            Err(AuthError::InvalidAssertion)
        ));
    }

    #[tokio::test]
    async fn accepts_only_audience_bound_mapped_service_account() {
        let verifier = ServiceAccountTokenVerifier::with_reviewer(
            config(),
            Arc::new(StaticReviewer(TokenReviewStatus {
                authenticated: true,
                user: super::TokenReviewUser {
                    username: "system:serviceaccount:qualification-retained-byte:task-submit"
                        .into(),
                },
                audiences: vec!["enrichedlab-broker-submit".into()],
            })),
        );
        let identity = verifier.verify(&headers()).await.unwrap();
        assert_eq!(identity.email, "task-submit@example.invalid");
        assert_eq!(identity.issuer, "kubernetes://serviceaccount");
        assert_eq!(identity.class, PrincipalClass::ServiceAccount);
    }

    #[tokio::test]
    async fn denies_wrong_audience_namespace_or_service_account() {
        for (username, audiences) in [
            (
                "system:serviceaccount:qualification-retained-byte:task-submit",
                vec!["wrong-audience".into()],
            ),
            (
                "system:serviceaccount:other-namespace:task-submit",
                vec!["enrichedlab-broker-submit".into()],
            ),
            (
                "system:serviceaccount:qualification-retained-byte:other-account",
                vec!["enrichedlab-broker-submit".into()],
            ),
        ] {
            let verifier = ServiceAccountTokenVerifier::with_reviewer(
                config(),
                Arc::new(StaticReviewer(TokenReviewStatus {
                    authenticated: true,
                    user: super::TokenReviewUser {
                        username: username.into(),
                    },
                    audiences,
                })),
            );
            assert!(matches!(
                verifier.verify(&headers()).await,
                Err(AuthError::ServiceAccountNotAllowed)
            ));
        }
    }

    #[tokio::test]
    async fn invalid_access_assertion_denies_even_with_a_valid_bearer() {
        let verifier =
            BrokerAuthVerifier::new(Arc::new(RejectingVerifier), Some(Arc::new(NeverVerifier)));
        let mut request_headers = headers();
        request_headers.insert(
            &ACCESS_ASSERTION,
            HeaderValue::from_static("invalid-access-token"),
        );
        assert!(matches!(
            verifier.verify(&request_headers).await,
            Err(AuthError::InvalidAssertion)
        ));
    }

    #[tokio::test]
    async fn service_account_only_rejects_access_assertions_and_accepts_a_mapped_bearer() {
        let verifier =
            ServiceAccountOnlyVerifier::new(Arc::new(ServiceAccountTokenVerifier::with_reviewer(
                config(),
                Arc::new(StaticReviewer(TokenReviewStatus {
                    authenticated: true,
                    user: super::TokenReviewUser {
                        username: "system:serviceaccount:qualification-retained-byte:task-submit"
                            .into(),
                    },
                    audiences: vec!["enrichedlab-broker-submit".into()],
                })),
            )));
        let identity = verifier.verify(&headers()).await.unwrap();
        assert_eq!(identity.issuer, SERVICE_ACCOUNT_ISSUER);

        let mut request_headers = headers();
        request_headers.insert(
            &ACCESS_ASSERTION,
            HeaderValue::from_static("unexpected-access-token"),
        );
        assert!(matches!(
            verifier.verify(&request_headers).await,
            Err(AuthError::AccessAssertionNotAllowed)
        ));
    }

    #[test]
    fn token_review_request_uses_v1_and_the_configured_audience() {
        let body = serde_json::to_value(TokenReviewRequest {
            api_version: "authentication.k8s.io/v1",
            kind: "TokenReview",
            spec: TokenReviewRequestSpec {
                token: "opaque-token",
                audiences: ["enrichedlab-broker-submit"],
            },
        })
        .unwrap();
        assert_eq!(body["apiVersion"], "authentication.k8s.io/v1");
        assert_eq!(
            body["spec"]["audiences"],
            serde_json::json!(["enrichedlab-broker-submit"])
        );
    }
}
