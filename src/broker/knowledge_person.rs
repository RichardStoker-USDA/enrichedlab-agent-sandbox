//! A signed-in person's knowledge actions through the workspace. The API forwards the person's
//! Cloudflare Access assertion exactly as Access sent it, beside a projected token of its own ServiceAccount for the
//! knowledge service's audience; the knowledge service verifies the assertion itself and acts as that person, directly.
//! The API never mints, keeps or logs an assertion, and forwards nothing for a service account.
use super::knowledge::mounted_bytes;
use anyhow::{Result, anyhow, ensure};
use futures_util::StreamExt;
use reqwest::{Client, Method, StatusCode, Url, redirect::Policy};
use serde::Deserialize;
use serde_json::Value;
use std::path::{Path, PathBuf};
use std::time::Duration;

const SCHEMA: &str = "enrichedlab.knowledge-person.v1";
const CONFIG_LIMIT: u64 = 16 * 1024;
const TOKEN_LIMIT: u64 = 16 * 1024;
const RESPONSE_LIMIT: usize = 512 * 1024;
/// The header Access puts the signed-in person's assertion in, and the one the knowledge service reads.
pub const ASSERTION_HEADER: &str = "cf-access-jwt-assertion";

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PersonFile {
    schema: String,
    service_url: String,
    token_file: PathBuf,
    /// The site's commons scope, whose records a person who leads it can mark baseline for every lab.
    commons_scope: String,
    #[serde(default = "default_timeout")]
    timeout_seconds: u64,
}

fn default_timeout() -> u64 {
    8
}

/// Why a forwarded call did not complete. Each maps to one fixed code.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PersonCallError {
    /// The service could not be reached, or the API's own token could not be read; nothing was sent.
    Unavailable,
    /// The service answered out of contract.
    Invalid,
    /// A change was sent but its answer was lost or unreadable, so whether it was made is not known.
    OutcomeUnknown,
    /// A read's answer passed the response limit; a smaller page may fit.
    TooLarge,
}

impl PersonCallError {
    pub fn code(self) -> &'static str {
        match self {
            Self::Unavailable => "knowledge_unavailable",
            Self::Invalid => "knowledge_invalid",
            Self::OutcomeUnknown => "knowledge_outcome_unknown",
            Self::TooLarge => "knowledge_response_too_large",
        }
    }
}

pub struct KnowledgePerson {
    client: Client,
    base: Url,
    token_file: PathBuf,
    commons_scope: String,
}

impl std::fmt::Debug for KnowledgePerson {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("KnowledgePerson")
            .field("base", &self.base.as_str())
            .finish()
    }
}

/// The forwarding configuration, when one is named. Without it the workspace offers no knowledge actions.
pub fn load_optional(path: Option<&Path>) -> Result<Option<KnowledgePerson>> {
    let Some(path) = path else {
        return Ok(None);
    };
    let file: PersonFile = serde_json::from_slice(&mounted_bytes(path, CONFIG_LIMIT)?)
        .map_err(|_| anyhow!("knowledge_person_config_invalid"))?;
    let base =
        Url::parse(&file.service_url).map_err(|_| anyhow!("knowledge_person_config_invalid"))?;
    // Plain HTTP stays inside the cluster network or the Pod itself, as for the coordinator's client.
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
            && scope_key(&file.commons_scope)
            && (1..=30).contains(&file.timeout_seconds),
        "knowledge_person_config_invalid"
    );
    let _ = rustls::crypto::ring::default_provider().install_default();
    let client = Client::builder()
        .redirect(Policy::none())
        .connect_timeout(Duration::from_secs(3))
        .timeout(Duration::from_secs(file.timeout_seconds))
        .build()
        .map_err(|_| anyhow!("knowledge_person_client_invalid"))?;
    Ok(Some(KnowledgePerson {
        client,
        base,
        token_file: file.token_file,
        commons_scope: file.commons_scope,
    }))
}

/// The knowledge schema's rule for a scope key: `^[a-z0-9][a-z0-9._-]{0,62}$`.
fn scope_key(value: &str) -> bool {
    (1..=63).contains(&value.len())
        && value
            .bytes()
            .next()
            .is_some_and(|b| b.is_ascii_lowercase() || b.is_ascii_digit())
        && value.bytes().all(|b| {
            b.is_ascii_lowercase() || b.is_ascii_digit() || matches!(b, b'.' | b'_' | b'-')
        })
}

impl KnowledgePerson {
    pub fn commons_scope(&self) -> &str {
        &self.commons_scope
    }

    fn token(&self) -> Result<String, PersonCallError> {
        let bytes = mounted_bytes(&self.token_file, TOKEN_LIMIT)
            .map_err(|_| PersonCallError::Unavailable)?;
        let token = String::from_utf8(bytes)
            .map_err(|_| PersonCallError::Invalid)?
            .trim()
            .to_owned();
        if token.is_empty() {
            return Err(PersonCallError::Unavailable);
        }
        Ok(token)
    }

    /// One call as the person whose `assertion` this is: GET, or POST with the change header and `body`. The answer's
    /// status and JSON come back as the service gave them, within the response limit. Once a change is sent, a lost
    /// or unreadable answer is `OutcomeUnknown`, never `Unavailable`: the change may have been made.
    pub async fn call(
        &self,
        method: Method,
        path: &str,
        assertion: &str,
        body: Option<&Value>,
    ) -> Result<(StatusCode, Value), PersonCallError> {
        if assertion.is_empty() || assertion.len() > 16 * 1024 {
            return Err(PersonCallError::Invalid);
        }
        let url = self.base.join(path).map_err(|_| PersonCallError::Invalid)?;
        let change = method == Method::POST;
        let lost = |fallback| {
            if change {
                PersonCallError::OutcomeUnknown
            } else {
                fallback
            }
        };
        let mut request = self
            .client
            .request(method.clone(), url)
            .bearer_auth(self.token()?)
            .header(ASSERTION_HEADER, assertion);
        if method == Method::POST {
            request = request.header("x-knowledge-intent", "mutate");
            if let Some(body) = body {
                request = request.json(body);
            }
        }
        // A connection that never opened sent nothing; any later failure may follow a change the service made.
        let response = request.send().await.map_err(|error| {
            if error.is_connect() {
                PersonCallError::Unavailable
            } else {
                lost(PersonCallError::Unavailable)
            }
        })?;
        let status = response.status();
        let mut bytes = Vec::new();
        let mut stream = response.bytes_stream();
        while let Some(chunk) = stream.next().await {
            let chunk = chunk.map_err(|_| lost(PersonCallError::Unavailable))?;
            if bytes.len() + chunk.len() > RESPONSE_LIMIT {
                return Err(lost(PersonCallError::TooLarge));
            }
            bytes.extend_from_slice(&chunk);
        }
        let value = if bytes.is_empty() {
            Value::Null
        } else {
            serde_json::from_slice(&bytes).map_err(|_| lost(PersonCallError::Invalid))?
        };
        Ok((status, value))
    }
}

/// The knowledge service's refusal codes the workspace passes on; any other code reads as `knowledge_refused`, so an
/// answer can never place its own text in the workspace's response.
pub fn refusal_code(value: &Value) -> &'static str {
    match value["error"]["code"].as_str().unwrap_or_default() {
        "baseline_refused" => "baseline_refused",
        "baseline_not_active" => "baseline_not_active",
        "resource_not_found" => "resource_not_found",
        "relation_not_found" => "relation_not_found",
        "relation_review_refused" => "relation_review_refused",
        "relation_not_proposed" => "relation_not_proposed",
        "person_not_enrolled" => "person_not_enrolled",
        "person_assertion_invalid" => "person_assertion_invalid",
        "person_assertion_not_accepted" => "person_assertion_not_accepted",
        "person_keys_unavailable" => "person_keys_unavailable",
        "person_sign_in_stale" => "person_sign_in_stale",
        "forwarder_not_allowed" => "forwarder_not_allowed",
        "forwarder_not_active" => "forwarder_not_active",
        "collection_invalid" => "collection_invalid",
        "cursor_invalid" => "cursor_invalid",
        "limit_invalid" => "limit_invalid",
        "scope_invalid" => "scope_invalid",
        "unauthenticated" => "knowledge_unauthenticated",
        _ => "knowledge_refused",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{Json, Router, http::HeaderMap, routing::any};
    use serde_json::json;
    use std::sync::{Arc, Mutex};

    type Seen = Arc<
        Mutex<
            Vec<(
                String,
                String,
                Option<String>,
                Option<String>,
                Option<String>,
            )>,
        >,
    >;

    async fn fake_service(answer: (u16, Value)) -> (String, Seen) {
        let seen: Seen = Arc::default();
        let log = seen.clone();
        let app = Router::new().fallback(any(
            move |method: axum::http::Method, uri: axum::http::Uri, headers: HeaderMap| {
                let log = log.clone();
                let answer = answer.clone();
                async move {
                    let get = |name: &str| {
                        headers
                            .get(name)
                            .and_then(|v| v.to_str().ok())
                            .map(str::to_owned)
                    };
                    log.lock().unwrap().push((
                        method.to_string(),
                        uri.to_string(),
                        get("authorization"),
                        get(ASSERTION_HEADER),
                        get("x-knowledge-intent"),
                    ));
                    (StatusCode::from_u16(answer.0).unwrap(), Json(answer.1))
                }
            },
        ));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let origin = format!("http://{}/", listener.local_addr().unwrap());
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        (origin, seen)
    }

    fn configured(dir: &Path, origin: &str) -> KnowledgePerson {
        std::fs::write(dir.join("token"), "workspace-token\n").unwrap();
        let config = json!({"schema": SCHEMA, "service_url": origin, "token_file": dir.join("token"), "commons_scope": "commons"});
        std::fs::write(dir.join("config.json"), config.to_string()).unwrap();
        load_optional(Some(&dir.join("config.json")))
            .unwrap()
            .unwrap()
    }

    #[tokio::test]
    async fn a_person_call_carries_the_assertion_and_the_workspace_token() {
        let (origin, seen) =
            fake_service((200, json!({"resource_id": "r", "baseline": true}))).await;
        let dir = tempfile::tempdir().unwrap();
        let person = configured(dir.path(), &origin);
        let (status, body) = person
            .call(
                Method::POST,
                "v1/resources/r/baseline",
                "signed.assertion.value",
                Some(&json!({"baseline": true})),
            )
            .await
            .unwrap();
        assert_eq!(
            (status, body["baseline"].clone()),
            (StatusCode::OK, json!(true))
        );
        let (_, _) = person
            .call(
                Method::GET,
                "v1/resources?kind=dataset&limit=500",
                "signed.assertion.value",
                None,
            )
            .await
            .unwrap();
        let seen = seen.lock().unwrap().clone();
        assert_eq!(
            seen[0],
            (
                "POST".into(),
                "/v1/resources/r/baseline".into(),
                Some("Bearer workspace-token".into()),
                Some("signed.assertion.value".into()),
                Some("mutate".into())
            )
        );
        assert_eq!(seen[1].0, "GET");
        assert_eq!(seen[1].4, None, "a read carries no change header");
        assert_eq!(
            person.call(Method::GET, "v1/x", "", None).await,
            Err(PersonCallError::Invalid)
        );
    }

    /// A stand-in that accepts each connection, reads the request and closes without answering.
    async fn silent_service() -> String {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let origin = format!("http://{}/", listener.local_addr().unwrap());
        tokio::spawn(async move {
            loop {
                let Ok((mut socket, _)) = listener.accept().await else {
                    return;
                };
                tokio::spawn(async move {
                    use tokio::io::AsyncReadExt;
                    let mut buffer = [0_u8; 4096];
                    let _ = socket.read(&mut buffer).await;
                });
            }
        });
        origin
    }

    #[tokio::test]
    async fn a_change_whose_answer_is_lost_has_an_unknown_outcome() {
        let dir = tempfile::tempdir().unwrap();
        let person = configured(dir.path(), &silent_service().await);
        assert_eq!(
            person
                .call(
                    Method::POST,
                    "v1/resources/r/baseline",
                    "signed.assertion.value",
                    Some(&json!({"baseline": true})),
                )
                .await,
            Err(PersonCallError::OutcomeUnknown),
            "the change was sent, so it may have been made"
        );
        assert_eq!(
            person
                .call(Method::GET, "v1/resources", "signed.assertion.value", None)
                .await,
            Err(PersonCallError::Unavailable),
            "a lost read changed nothing"
        );
        // Nothing listens on this port: the connection never opens, so the change was never sent.
        let closed = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let origin = format!("http://{}/", closed.local_addr().unwrap());
        drop(closed);
        let unreachable = configured(dir.path(), &origin);
        assert_eq!(
            unreachable
                .call(
                    Method::POST,
                    "v1/resources/r/baseline",
                    "signed.assertion.value",
                    Some(&json!({"baseline": true})),
                )
                .await,
            Err(PersonCallError::Unavailable)
        );
    }

    #[tokio::test]
    async fn a_refusal_passes_on_only_known_codes() {
        assert_eq!(
            refusal_code(&json!({"error": {"code": "baseline_refused"}})),
            "baseline_refused"
        );
        assert_eq!(
            refusal_code(&json!({"error": {"code": "<script>"}})),
            "knowledge_refused"
        );
        assert_eq!(refusal_code(&Value::Null), "knowledge_refused");
        let dir = tempfile::tempdir().unwrap();
        let url = "http://knowledge.research-knowledge.svc.cluster.local:8080/";
        let good = json!({"schema": SCHEMA, "service_url": url, "token_file": "/run/t", "commons_scope": "commons"});
        std::fs::write(dir.path().join("good.json"), good.to_string()).unwrap();
        let loaded = load_optional(Some(&dir.path().join("good.json")))
            .unwrap()
            .unwrap();
        assert_eq!(loaded.commons_scope(), "commons");
        for bad in [
            json!({"schema": "other", "service_url": url, "token_file": "/run/t", "commons_scope": "commons"}),
            json!({"schema": SCHEMA, "service_url": "http://knowledge.example.com/", "token_file": "/run/t", "commons_scope": "commons"}),
            json!({"schema": SCHEMA, "service_url": url, "token_file": "relative", "commons_scope": "commons"}),
            json!({"schema": SCHEMA, "service_url": url, "token_file": "/run/t", "commons_scope": "Commons scope"}),
            json!({"schema": SCHEMA, "service_url": url, "token_file": "/run/t"}),
            json!({"schema": SCHEMA, "service_url": url, "token_file": "/run/t", "commons_scope": "commons", "labs": []}),
        ] {
            std::fs::write(dir.path().join("bad.json"), bad.to_string()).unwrap();
            assert!(
                load_optional(Some(&dir.path().join("bad.json"))).is_err(),
                "{bad}"
            );
        }
        assert!(load_optional(None).unwrap().is_none());
    }
}
