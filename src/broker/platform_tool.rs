//! Adapter for the platform-owned reviewed compute job broker.
//!
//! It deliberately does not execute a binary or duplicate the platform catalog.
//! A trusted caller supplies an already-issued operation context; this module
//! submits the exact platform contract and returns only an accepted-job receipt.
#![allow(dead_code)] // Deliberately unwired until the server-side proof issuer exists.

use super::{model::ClaimedTask, store::Store};
use anyhow::{Result, anyhow, ensure};
use reqwest::{Client, StatusCode, Url, redirect::Policy};
use serde::Deserialize;
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
    time::Duration,
};
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

const API_VERSION: &str = "compute.enrichedlab.org/v1alpha1";
const TOKEN_LIMIT: u64 = 16 * 1024;
const RESPONSE_LIMIT: usize = 512 * 1024;

#[derive(Clone)]
pub(crate) struct ServerIssuedJobContext {
    catalog_jws: String,
    submit_jws: String,
}

impl std::fmt::Debug for ServerIssuedJobContext {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("ServerIssuedJobContext(REDACTED)")
    }
}

impl ServerIssuedJobContext {
    /// Only a server-side identity issuer may construct this. It is intentionally
    /// crate-private so agent requests cannot provide an assertion.
    pub(crate) fn issued(catalog_jws: String, submit_jws: String) -> Result<Self> {
        ensure!(
            valid_compact_jws(&catalog_jws) && valid_compact_jws(&submit_jws),
            "platform_tool_identity_proof_invalid"
        );
        Ok(Self {
            catalog_jws,
            submit_jws,
        })
    }
}

#[derive(Clone, Debug)]
pub(crate) struct PlatformArtifactInput {
    pub artifact_id: String,
    pub artifact_version: String,
    pub sha256: String,
    pub byte_length: u64,
    pub media_type: String,
}

#[derive(Clone, Debug)]
pub(crate) struct ApprovedPlatformTool {
    pub request_id: Uuid,
    pub conversation_id: Uuid,
    pub tool_id: String,
    pub catalog_digest: String,
    pub size: String,
    pub parameters: Value,
    pub input: PlatformArtifactInput,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AcceptedPlatformJob {
    pub job_id: String,
    pub request_id: Uuid,
    pub conversation_id: Uuid,
    pub tool_id: String,
    pub catalog_digest: String,
    pub input_sha256: String,
    pub accepted_at: String,
}

pub struct PlatformJobClient {
    client: Client,
    origin: Url,
    token_path: PathBuf,
}

impl PlatformJobClient {
    /// Operator-owned endpoint and projected ServiceAccount token references.
    pub(crate) fn from_service_account(
        origin: Url,
        token_path: PathBuf,
        ca_path: &Path,
        timeout: Duration,
    ) -> Result<Self> {
        ensure!(
            origin.scheme() == "https"
                && origin.host_str().is_some()
                && origin.username().is_empty()
                && origin.password().is_none()
                && origin.path() == "/"
                && origin.query().is_none()
                && origin.fragment().is_none(),
            "platform_tool_endpoint_invalid"
        );
        ensure!(
            token_path.is_absolute() && ca_path.is_absolute(),
            "platform_tool_credential_reference_invalid"
        );
        ensure!(
            (1..=30).contains(&timeout.as_secs()),
            "platform_tool_timeout_invalid"
        );
        let _ = rustls::crypto::ring::default_provider().install_default();
        let ca = read_bounded(ca_path, 64 * 1024)
            .map_err(|_| anyhow!("platform_tool_ca_unavailable"))?;
        let certificates = reqwest::Certificate::from_pem_bundle(&ca)
            .map_err(|_| anyhow!("platform_tool_ca_invalid"))?;
        ensure!(!certificates.is_empty(), "platform_tool_ca_invalid");
        let client = Client::builder()
            .tls_certs_only(certificates)
            .https_only(true)
            .no_proxy()
            .redirect(Policy::none())
            .connect_timeout(Duration::from_secs(5))
            .timeout(timeout)
            .pool_max_idle_per_host(1)
            .build()
            .map_err(|_| anyhow!("platform_tool_client_invalid"))?;
        Ok(Self {
            client,
            origin,
            token_path,
        })
    }

    pub(crate) async fn submit(
        &self,
        task: &ClaimedTask,
        approved: &ApprovedPlatformTool,
        identity: &ServerIssuedJobContext,
        cancellation: &CancellationToken,
    ) -> Result<AcceptedPlatformJob> {
        validate_approved(task, approved)?;
        let catalog = self.fetch_catalog(identity, cancellation).await?;
        validate_release_catalog(&catalog, approved)?;
        let body = serialized_request_bytes(approved)?;
        let token = self.token().await?;
        let url = self
            .origin
            .join("v1/jobs")
            .map_err(|_| anyhow!("platform_tool_endpoint_invalid"))?;
        let request = self
            .client
            .post(url)
            .bearer_auth(token)
            .header("Idempotency-Key", approved.request_id.to_string())
            .header("X-EnrichedLab-Job-Context", &identity.submit_jws)
            .header("Content-Type", "application/json")
            .body(body);
        let response = tokio::select! {
            biased;
            _ = cancellation.cancelled() => return Err(anyhow!("platform_tool_submit_cancelled")),
            response = request.send() => response,
        };
        let response = response.map_err(|_| anyhow!("platform_tool_submit_uncertain"))?;
        if response.status() != StatusCode::ACCEPTED {
            return Err(anyhow!("platform_tool_submit_refused"));
        }
        let value = bounded_json(response, "platform_tool_submit_invalid").await?;
        let accepted: AcceptedResponse =
            serde_json::from_value(value).map_err(|_| anyhow!("platform_tool_submit_invalid"))?;
        ensure!(
            accepted.api_version == API_VERSION
                && accepted.state == "accepted"
                && valid_job_id(&accepted.job_id)
                && accepted.catalog_digest == approved.catalog_digest
                && valid_timestamp(&accepted.submitted_at),
            "platform_tool_submit_invalid"
        );
        Ok(AcceptedPlatformJob {
            job_id: accepted.job_id,
            request_id: approved.request_id,
            conversation_id: approved.conversation_id,
            tool_id: approved.tool_id.clone(),
            catalog_digest: approved.catalog_digest.clone(),
            input_sha256: approved.input.sha256.clone(),
            accepted_at: accepted.submitted_at,
        })
    }

    async fn fetch_catalog(
        &self,
        identity: &ServerIssuedJobContext,
        cancellation: &CancellationToken,
    ) -> Result<CatalogResponse> {
        let token = self.token().await?;
        let url = self
            .origin
            .join("v1/catalog")
            .map_err(|_| anyhow!("platform_tool_endpoint_invalid"))?;
        let request = self
            .client
            .get(url)
            .bearer_auth(token)
            .header("X-EnrichedLab-Job-Context", &identity.catalog_jws);
        let response = tokio::select! {
            biased;
            _ = cancellation.cancelled() => return Err(anyhow!("platform_tool_catalog_cancelled")),
            response = request.send() => response,
        };
        let response = response.map_err(|_| anyhow!("platform_tool_catalog_unavailable"))?;
        ensure!(
            response.status() == StatusCode::OK,
            "platform_tool_catalog_unavailable"
        );
        let value = bounded_json(response, "platform_tool_catalog_invalid").await?;
        serde_json::from_value(value).map_err(|_| anyhow!("platform_tool_catalog_invalid"))
    }

    async fn token(&self) -> Result<String> {
        let path = self.token_path.clone();
        let bytes = tokio::task::spawn_blocking(move || read_bounded(&path, TOKEN_LIMIT))
            .await
            .map_err(|_| anyhow!("platform_tool_token_unavailable"))?
            .map_err(|_| anyhow!("platform_tool_token_unavailable"))?;
        let token = std::str::from_utf8(&bytes)
            .map_err(|_| anyhow!("platform_tool_token_invalid"))?
            .trim();
        ensure!(
            !token.is_empty() && token.len() <= TOKEN_LIMIT as usize,
            "platform_tool_token_invalid"
        );
        Ok(token.to_owned())
    }
}

/// Submits only through the authoritative platform broker and writes the core
/// receipt under the same lease. There is intentionally no automatic retry:
/// an ambiguous submit needs a fresh server-issued context for the same request.
pub(crate) async fn submit_and_bind(
    client: &PlatformJobClient,
    store: &Store,
    task: &ClaimedTask,
    worker: &str,
    approved: &ApprovedPlatformTool,
    identity: &ServerIssuedJobContext,
    cancellation: &CancellationToken,
) -> Result<AcceptedPlatformJob> {
    let accepted = client
        .submit(task, approved, identity, cancellation)
        .await?;
    ensure!(
        store
            .bind_platform_tool_job(task, worker, &accepted)
            .await?,
        "platform_tool_binding_lease_lost"
    );
    Ok(accepted)
}

#[derive(Deserialize)]
struct CatalogResponse {
    api_version: String,
    deployable: bool,
    release_blockers: Vec<String>,
    image_families: BTreeMap<String, ImageFamily>,
    tools: Vec<CatalogTool>,
}

#[derive(Deserialize)]
struct ImageFamily {
    release_ref: Option<String>,
    qualification_state: String,
}

#[derive(Deserialize)]
struct CatalogTool {
    id: String,
    trust_tier: String,
    image_family: String,
    runner_sha256: String,
    release_command: Vec<String>,
    release_input_format: String,
    result_kind: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct AcceptedResponse {
    api_version: String,
    job_id: String,
    state: String,
    submitted_at: String,
    catalog_digest: String,
}

fn validate_release_catalog(
    catalog: &CatalogResponse,
    approved: &ApprovedPlatformTool,
) -> Result<()> {
    ensure!(
        catalog.api_version == API_VERSION
            && catalog.deployable
            && catalog.release_blockers.is_empty(),
        "platform_tool_catalog_unavailable"
    );
    let tool = catalog
        .tools
        .iter()
        .filter(|tool| tool.id == approved.tool_id)
        .collect::<Vec<_>>();
    ensure!(tool.len() == 1, "platform_tool_catalog_unavailable");
    let tool = tool[0];
    let image = catalog
        .image_families
        .get(&tool.image_family)
        .ok_or_else(|| anyhow!("platform_tool_catalog_unavailable"))?;
    ensure!(
        tool.trust_tier == "reviewed"
            && valid_sha256(&tool.runner_sha256)
            && tool.release_command == ["/opt/enrichedlab/bin/run-reviewed-tool"]
            && tool.release_input_format == "compute-job-files-v1"
            && tool.result_kind == "table-v1"
            && image.qualification_state == "qualified"
            && image.release_ref.as_deref().is_some_and(valid_image_ref),
        "platform_tool_catalog_unavailable"
    );
    Ok(())
}

fn validate_approved(task: &ClaimedTask, approved: &ApprovedPlatformTool) -> Result<()> {
    ensure!(
        task.attempt > 0 && task.lease_generation > 0,
        "platform_tool_request_invalid"
    );
    validate_approved_shape(approved)
}

fn valid_artifact(input: &PlatformArtifactInput) -> bool {
    valid_prefixed(&input.artifact_id, "art_")
        && valid_prefixed(&input.artifact_version, "av_")
        && valid_sha256(&input.sha256)
        && (2..=10_737_418_240).contains(&input.byte_length)
        && input.media_type == "application/vnd.enrichedlab.table+json;version=1"
}

/// Exact request bytes the trusted issuer must bind into the submit JWS.
/// This remains crate-private so an agent cannot choose the wire representation.
pub(crate) fn serialized_request_bytes(approved: &ApprovedPlatformTool) -> Result<Vec<u8>> {
    validate_approved_shape(approved)?;
    serde_json::to_vec(&request_body(approved))
        .map_err(|_| anyhow!("platform_tool_request_invalid"))
}

fn validate_approved_shape(approved: &ApprovedPlatformTool) -> Result<()> {
    ensure!(
        valid_tool_id(&approved.tool_id)
            && valid_catalog_digest(&approved.catalog_digest)
            && matches!(approved.size.as_str(), "small" | "medium" | "large")
            && approved.parameters.is_object()
            && valid_artifact(&approved.input),
        "platform_tool_request_invalid"
    );
    Ok(())
}

fn request_body(approved: &ApprovedPlatformTool) -> Value {
    json!({
        "api_version": API_VERSION,
        "request_id": approved.request_id,
        "conversation_id": approved.conversation_id,
        "tool": {
            "id": approved.tool_id,
            "size": approved.size,
            "catalog_digest": approved.catalog_digest,
        },
        "parameters": approved.parameters,
        "input": {
            "transport": "artifact-ref-v1",
            "artifact_id": approved.input.artifact_id,
            "artifact_version": approved.input.artifact_version,
            "sha256": approved.input.sha256,
            "byte_length": approved.input.byte_length,
            "media_type": approved.input.media_type,
        }
    })
}

async fn bounded_json(response: reqwest::Response, error: &'static str) -> Result<Value> {
    ensure!(
        response
            .content_length()
            .is_none_or(|value| value <= RESPONSE_LIMIT as u64),
        error
    );
    let bytes = response.bytes().await.map_err(|_| anyhow!(error))?;
    ensure!(bytes.len() <= RESPONSE_LIMIT, error);
    serde_json::from_slice(&bytes).map_err(|_| anyhow!(error))
}

fn read_bounded(path: &Path, limit: u64) -> std::io::Result<Vec<u8>> {
    let metadata = fs::metadata(path)?;
    if !metadata.is_file() || metadata.len() > limit {
        return Err(std::io::Error::other("unsafe file"));
    }
    let bytes = fs::read(path)?;
    if bytes.len() > limit as usize {
        return Err(std::io::Error::other("file too large"));
    }
    Ok(bytes)
}

fn valid_compact_jws(value: &str) -> bool {
    (32..=4096).contains(&value.len())
        && value.split('.').count() == 3
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
}

fn valid_tool_id(value: &str) -> bool {
    (3..=64).contains(&value.len())
        && value.as_bytes().first().is_some_and(u8::is_ascii_lowercase)
        && value
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'_')
}

fn valid_sha256(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn valid_catalog_digest(value: &str) -> bool {
    value.strip_prefix("sha256:").is_some_and(valid_sha256)
}

fn valid_image_ref(value: &str) -> bool {
    value
        .rsplit_once("@sha256:")
        .is_some_and(|(repository, digest)| !repository.is_empty() && valid_sha256(digest))
}

fn valid_prefixed(value: &str, prefix: &str) -> bool {
    value.strip_prefix(prefix).is_some_and(|rest| {
        (26..=64).contains(&rest.len())
            && rest
                .bytes()
                .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit())
    })
}

fn valid_job_id(value: &str) -> bool {
    valid_prefixed(value, "job_")
}

fn valid_timestamp(value: &str) -> bool {
    time::OffsetDateTime::parse(value, &time::format_description::well_known::Rfc3339).is_ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn task() -> ClaimedTask {
        ClaimedTask {
            id: Uuid::new_v4(), principal_id: Uuid::new_v4(), group_key: "research".into(), profile_key: "reference".into(),
            objective: String::new(), logical_agent: format!("task-{}", Uuid::new_v4().simple()), lease_generation: 1, attempt: 1,
            max_task_attempts: 2, max_provider_attempts: 2, harness: "pi".into(), thinking: None, route: "local".into(),
            controller_profile: "controller".into(), gateway_profile: "gateway".into(),
            image_ref: "registry.test/controller@sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa".into(),
            cpu_limit: "500m".into(), memory_limit: "512Mi".into(), state_profile: "reference".into(), role: "research".into(), max_run_seconds: 60,
        }
    }

    fn approved() -> ApprovedPlatformTool {
        ApprovedPlatformTool {
            request_id: Uuid::new_v4(),
            conversation_id: Uuid::new_v4(),
            tool_id: "profile_columns".into(),
            catalog_digest: format!("sha256:{}", "a".repeat(64)),
            size: "small".into(),
            parameters: json!({}),
            input: PlatformArtifactInput {
                artifact_id: format!("art_{}", "a".repeat(26)),
                artifact_version: format!("av_{}", "b".repeat(26)),
                sha256: "c".repeat(64),
                byte_length: 2,
                media_type: "application/vnd.enrichedlab.table+json;version=1".into(),
            },
        }
    }

    #[test]
    fn release_catalog_is_a_hard_gate_and_request_is_platform_shaped() {
        let approved = approved();
        let unavailable: CatalogResponse = serde_json::from_value(json!({
            "api_version": API_VERSION, "deployable": false, "release_blockers": ["broker_acceptance_gate"],
            "image_families": {}, "tools": []
        })).unwrap();
        assert!(validate_release_catalog(&unavailable, &approved).is_err());
        let release_ready: CatalogResponse = serde_json::from_value(json!({
            "api_version": API_VERSION,
            "catalog_version": "2026-09-10.1",
            "release_registry": "registry.enrichedlab.internal",
            "deployable": true,
            "release_blockers": [],
            "runtime": {"unconsumed": true},
            "resource_profiles": {"small": {}},
            "image_families": {
              "python": {
                "transition_ref": "registry.test/python:transition",
                "release_ref": format!("registry.test/python@sha256:{}", "a".repeat(64)),
                "release_repository": "python",
                "qualification_state": "qualified"
              }
            },
            "tools": [{
              "id": "profile_columns", "trust_tier": "reviewed", "image_family": "python",
              "runner_source": "backend/app/jobs/runner_profile.py", "runner_sha256": "a".repeat(64),
              "runner_image_path": "/opt/enrichedlab/jobs/runner_profile.py",
              "transition_command": ["python", "/job/runner.py"],
              "release_command": ["/opt/enrichedlab/bin/run-reviewed-tool"],
              "transition_input_format": "json", "release_input_format": "compute-job-files-v1",
              "deadline_seconds": 90, "default_size": "small", "allowed_sizes": ["small"],
              "parameter_schema": {}, "result_kind": "table-v1"
            }]
        })).unwrap();
        assert!(validate_release_catalog(&release_ready, &approved).is_ok());
        let body = request_body(&approved);
        assert_eq!(body["api_version"], API_VERSION);
        assert_eq!(body["tool"]["catalog_digest"], approved.catalog_digest);
        assert_eq!(body["input"]["transport"], "artifact-ref-v1");
        assert!(validate_approved(&task(), &approved).is_ok());
    }

    #[test]
    fn server_context_is_not_debuggable_and_request_identity_is_stable_for_retry() {
        let approved = approved();
        let one = request_body(&approved);
        let two = request_body(&approved);
        assert_eq!(one, two);
        assert_eq!(
            serialized_request_bytes(&approved).unwrap(),
            serialized_request_bytes(&approved).unwrap()
        );
        assert_eq!(one["request_id"], approved.request_id.to_string());
        assert_eq!(
            format!(
                "{:?}",
                ServerIssuedJobContext::issued(
                    format!("{}.{}.{}", "a".repeat(12), "b".repeat(12), "c".repeat(12)),
                    format!("{}.{}.{}", "d".repeat(12), "e".repeat(12), "f".repeat(12)),
                )
                .unwrap()
            ),
            "ServerIssuedJobContext(REDACTED)"
        );
    }
}
