//! Namespace-bound, read-only Kubernetes transport for input-copy completion.
//!
//! This adapter reads only the exact trusted Job, at most one
//! `batch.kubernetes.io/job-name` labelled Pod, and the fixed custodian's
//! current stdout. It never mutates Kubernetes
//! resources or exposes API response bodies in errors.

use super::{
    export_kubernetes::{valid_label, valid_uid},
    input_completion::InputCopyObservationTransport,
    input_finalization::{InputFinalizationTransport, InputSealMutation},
    input_publication::{
        InputObject, InputObjectKind, InputObjectReceipt, InputPublicationTransport,
        InputSourceObservationTransport,
    },
};
use anyhow::{Result, anyhow, ensure};
use async_trait::async_trait;
use futures_util::StreamExt;
use reqwest::{Client, StatusCode, Url};
use serde::Deserialize;
use serde_json::{Value, json};
use std::{
    io::Read,
    path::{Path, PathBuf},
    time::Duration,
};

const OBJECT_LIMIT: usize = 2 * 1024 * 1024;
const LOG_LIMIT: usize = 1024 * 1024 + 1;
const TOKEN_LIMIT: usize = 16 * 1024;
const CUSTODIAN_CONTAINER: &str = "custodian";
const POD_PAGE_SIZE: usize = 100;
const POD_PAGE_LIMIT: usize = 10;
const POD_ITEM_LIMIT: usize = POD_PAGE_SIZE * POD_PAGE_LIMIT;

/// Operator-owned Kubernetes API endpoint and projected service-account files.
/// The token reference is deliberately read for each request because projected
/// service-account token files rotate through symlinks.
pub struct InputKubernetesClient {
    client: Client,
    api: Url,
    namespace: String,
    token: PathBuf,
}

impl InputKubernetesClient {
    pub fn from_service_account(
        api: Url,
        namespace: String,
        token: PathBuf,
        ca: &Path,
    ) -> Result<Self> {
        ensure!(
            valid_label(&namespace),
            "input_kubernetes_namespace_invalid"
        );
        ensure!(
            api.scheme() == "https"
                && api.host_str().is_some()
                && api.username().is_empty()
                && api.password().is_none()
                && api.path() == "/"
                && api.query().is_none()
                && api.fragment().is_none(),
            "input_kubernetes_origin_invalid"
        );
        ensure!(
            token.is_absolute(),
            "input_kubernetes_token_reference_invalid"
        );
        ensure!(ca.is_absolute(), "input_kubernetes_ca_reference_invalid");
        let _ = rustls::crypto::ring::default_provider().install_default();
        let ca =
            read_bounded(ca, 64 * 1024).map_err(|_| anyhow!("input_kubernetes_ca_unavailable"))?;
        let certificates = reqwest::Certificate::from_pem_bundle(&ca)
            .map_err(|_| anyhow!("input_kubernetes_ca_invalid"))?;
        ensure!(!certificates.is_empty(), "input_kubernetes_ca_invalid");
        let client = Client::builder()
            .tls_certs_only(certificates)
            .https_only(true)
            .no_proxy()
            .redirect(reqwest::redirect::Policy::none())
            .connect_timeout(Duration::from_secs(5))
            .timeout(Duration::from_secs(15))
            .build()
            .map_err(|_| anyhow!("input_kubernetes_client_invalid"))?;
        Ok(Self {
            client,
            api,
            namespace,
            token,
        })
    }

    fn require_namespace(&self, namespace: &str) -> Result<()> {
        ensure!(
            namespace == self.namespace && valid_label(namespace),
            "input_kubernetes_namespace_mismatch"
        );
        Ok(())
    }

    fn resource_url(&self, path: &str) -> Result<Url> {
        self.api
            .join(path)
            .map_err(|_| anyhow!("input_kubernetes_path_invalid"))
    }

    async fn token(&self) -> Result<String> {
        let path = self.token.clone();
        let bytes = tokio::task::spawn_blocking(move || read_bounded(&path, TOKEN_LIMIT))
            .await
            .map_err(|_| anyhow!("input_kubernetes_token_unavailable"))?
            .map_err(|_| anyhow!("input_kubernetes_token_unavailable"))?;
        let token = std::str::from_utf8(&bytes)
            .map_err(|_| anyhow!("input_kubernetes_token_invalid"))?
            .trim();
        ensure!(!token.is_empty(), "input_kubernetes_token_invalid");
        Ok(token.to_owned())
    }

    async fn get_bytes(&self, url: Url, limit: usize, error: &'static str) -> Result<Vec<u8>> {
        let token = self.token().await.map_err(|_| anyhow!(error))?;
        let response = self
            .client
            .get(url)
            .bearer_auth(token)
            .send()
            .await
            .map_err(|_| anyhow!(error))?;
        ensure!(response.status() == StatusCode::OK, error);
        bounded_response(response, limit, error).await
    }

    async fn request(
        &self,
        request: reqwest::RequestBuilder,
        cancellation: &tokio_util::sync::CancellationToken,
        error: &'static str,
    ) -> Result<reqwest::Response> {
        tokio::select! {
            biased;
            _ = cancellation.cancelled() => Err(anyhow!("input_finalization_cancelled")),
            result = request.send() => result.map_err(|_| anyhow!(error)),
        }
    }

    async fn list_pod_page(&self, namespace: &str, continuation: Option<&str>) -> Result<PodList> {
        let mut url = self.resource_url(&format!("api/v1/namespaces/{namespace}/pods"))?;
        {
            let mut query = url.query_pairs_mut();
            query.append_pair("limit", &POD_PAGE_SIZE.to_string());
            if let Some(continuation) = continuation {
                ensure!(
                    !continuation.is_empty() && continuation.len() <= 4096,
                    "input_source_writer_pagination_invalid"
                );
                query.append_pair("continue", continuation);
            }
        }
        let body = self
            .get_bytes(url, OBJECT_LIMIT, "input_source_writer_unavailable")
            .await?;
        serde_json::from_slice(&body).map_err(|_| anyhow!("input_source_writer_unavailable"))
    }
}

#[derive(Deserialize)]
struct PodList {
    metadata: PodListMetadata,
    items: Vec<Value>,
}

#[derive(Default, Deserialize)]
struct PodListMetadata {
    #[serde(default, rename = "continue")]
    continuation: String,
}

#[async_trait]
impl InputCopyObservationTransport for InputKubernetesClient {
    async fn get_job(&self, namespace: &str, name: &str) -> Result<Value> {
        self.require_namespace(namespace)?;
        ensure!(valid_label(name), "input_kubernetes_job_reference_invalid");
        let url =
            self.resource_url(&format!("apis/batch/v1/namespaces/{namespace}/jobs/{name}"))?;
        let body = self
            .get_bytes(url, OBJECT_LIMIT, "input_kubernetes_job_unavailable")
            .await?;
        serde_json::from_slice(&body).map_err(|_| anyhow!("input_kubernetes_job_unavailable"))
    }

    async fn get_controlled_pod(&self, namespace: &str, job_name: &str) -> Result<Option<Value>> {
        self.require_namespace(namespace)?;
        ensure!(
            valid_label(job_name),
            "input_kubernetes_pod_reference_invalid"
        );
        let mut url = self.resource_url(&format!("api/v1/namespaces/{namespace}/pods"))?;
        url.query_pairs_mut()
            .append_pair(
                "labelSelector",
                &format!("batch.kubernetes.io/job-name={job_name}"),
            )
            .append_pair("limit", "2");
        let body = self
            .get_bytes(url, OBJECT_LIMIT, "input_kubernetes_pod_unavailable")
            .await?;
        let pods: PodList = serde_json::from_slice(&body)
            .map_err(|_| anyhow!("input_kubernetes_pod_unavailable"))?;
        ensure!(
            pods.metadata.continuation.is_empty(),
            "input_kubernetes_pod_pagination"
        );
        match pods.items.len() {
            0 => Ok(None),
            1 => {
                let pod = pods.items.into_iter().next().expect("one pod");
                ensure!(
                    pod.pointer("/metadata/labels/batch.kubernetes.io~1job-name")
                        .and_then(Value::as_str)
                        == Some(job_name),
                    "input_kubernetes_pod_selector_mismatch"
                );
                Ok(Some(pod))
            }
            _ => Err(anyhow!("input_kubernetes_pod_ambiguous")),
        }
    }

    async fn helper_stdout(
        &self,
        namespace: &str,
        pod_name: &str,
        container: &str,
    ) -> Result<Vec<u8>> {
        self.require_namespace(namespace)?;
        ensure!(
            valid_label(pod_name) && container == CUSTODIAN_CONTAINER,
            "input_kubernetes_log_reference_invalid"
        );
        let mut url = self.resource_url(&format!(
            "api/v1/namespaces/{namespace}/pods/{pod_name}/log"
        ))?;
        url.query_pairs_mut()
            .append_pair("container", CUSTODIAN_CONTAINER)
            .append_pair("limitBytes", &LOG_LIMIT.to_string())
            .append_pair("timestamps", "false")
            .append_pair("previous", "false");
        self.get_bytes(url, LOG_LIMIT, "input_kubernetes_log_unavailable")
            .await
    }
}

fn read_bounded(path: &Path, limit: usize) -> Result<Vec<u8>> {
    let mut bytes = Vec::new();
    std::fs::File::open(path)?
        .take((limit + 1) as u64)
        .read_to_end(&mut bytes)?;
    ensure!(bytes.len() <= limit, "input_kubernetes_file_limit");
    Ok(bytes)
}

#[derive(Deserialize)]
struct CreatedObject {
    metadata: CreatedMetadata,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct CreatedMetadata {
    name: String,
    namespace: String,
    uid: String,
}

#[async_trait]
impl InputPublicationTransport for InputKubernetesClient {
    fn namespace(&self) -> &str {
        &self.namespace
    }

    async fn create(
        &self,
        object: &InputObject,
        cancellation: &tokio_util::sync::CancellationToken,
    ) -> Result<InputObjectReceipt> {
        ensure!(
            object.namespace() == self.namespace,
            "input_publication_namespace_mismatch"
        );
        let (prefix, resource) = match object.kind() {
            InputObjectKind::NetworkPolicy => ("apis/networking.k8s.io/v1", "networkpolicies"),
            InputObjectKind::PersistentVolumeClaim => ("api/v1", "persistentvolumeclaims"),
            InputObjectKind::Secret => ("api/v1", "secrets"),
            InputObjectKind::Job => ("apis/batch/v1", "jobs"),
        };
        let url = self.resource_url(&format!(
            "{prefix}/namespaces/{}/{}",
            self.namespace, resource
        ))?;
        let operation = async {
            let token = self
                .token()
                .await
                .map_err(|_| anyhow!("input_publication_create_uncertain"))?;
            let response = self
                .client
                .post(url)
                .bearer_auth(token)
                .json(object.body())
                .send()
                .await
                .map_err(|_| anyhow!("input_publication_create_uncertain"))?;
            match response.status() {
                StatusCode::CREATED => {}
                StatusCode::CONFLICT => {
                    return Err(anyhow!(
                        "input_publication_existing_object_requires_recovery"
                    ));
                }
                // Quota admission refuses a volume the tenant has no room for; the refusal is named
                // so the run's record says why. Nothing was created.
                StatusCode::FORBIDDEN => {
                    let body = bounded_response(
                        response,
                        OBJECT_LIMIT,
                        "input_publication_create_rejected",
                    )
                    .await
                    .unwrap_or_default();
                    return Err(anyhow!(if quota_refused(&body) {
                        "input_capacity_exhausted"
                    } else {
                        "input_publication_create_rejected"
                    }));
                }
                _ => return Err(anyhow!("input_publication_create_rejected")),
            }
            let body =
                bounded_response(response, OBJECT_LIMIT, "input_publication_create_uncertain")
                    .await?;
            let created: CreatedObject = serde_json::from_slice(&body)
                .map_err(|_| anyhow!("input_publication_response_invalid"))?;
            ensure!(
                created.metadata.name == object.name()
                    && created.metadata.namespace == self.namespace
                    && valid_uid(&created.metadata.uid),
                "input_publication_created_identity_mismatch"
            );
            Ok(InputObjectReceipt {
                kind: object.kind(),
                namespace: created.metadata.namespace,
                name: created.metadata.name,
                uid: created.metadata.uid,
                request_sha256: object.digest().to_owned(),
            })
        };
        tokio::select! {
            biased;
            _ = cancellation.cancelled() => Err(anyhow!("input_publication_cancelled")),
            result = operation => result,
        }
    }
}

#[async_trait]
impl InputSourceObservationTransport for InputKubernetesClient {
    async fn get_source_pvc(&self, namespace: &str, name: &str) -> Result<Value> {
        self.require_namespace(namespace)?;
        ensure!(valid_label(name), "input_source_pvc_reference_invalid");
        let url = self.resource_url(&format!(
            "api/v1/namespaces/{namespace}/persistentvolumeclaims/{name}"
        ))?;
        let body = self
            .get_bytes(url, OBJECT_LIMIT, "input_source_pvc_unavailable")
            .await?;
        serde_json::from_slice(&body).map_err(|_| anyhow!("input_source_pvc_unavailable"))
    }

    async fn source_has_active_writer(&self, namespace: &str, pvc_name: &str) -> Result<bool> {
        self.require_namespace(namespace)?;
        ensure!(valid_label(pvc_name), "input_source_pvc_reference_invalid");
        let mut continuation = None;
        let mut observed = 0_usize;
        for _ in 0..POD_PAGE_LIMIT {
            let pods = self
                .list_pod_page(namespace, continuation.as_deref())
                .await?;
            ensure!(
                pods.items.len() <= POD_PAGE_SIZE
                    && observed.saturating_add(pods.items.len()) <= POD_ITEM_LIMIT,
                "input_source_writer_pagination_limit"
            );
            observed += pods.items.len();
            for pod in &pods.items {
                ensure!(
                    pod.pointer("/metadata/namespace").and_then(Value::as_str) == Some(namespace),
                    "input_source_writer_observation_incomplete"
                );
                if pod_has_active_writer(pod, pvc_name)? {
                    return Ok(true);
                }
            }
            if pods.metadata.continuation.is_empty() {
                return Ok(false);
            }
            continuation = Some(pods.metadata.continuation);
        }
        Err(anyhow!("input_source_writer_pagination_limit"))
    }
}

#[async_trait]
impl InputFinalizationTransport for InputKubernetesClient {
    fn namespace(&self) -> &str {
        &self.namespace
    }

    async fn get_pvc(&self, namespace: &str, name: &str) -> Result<Value> {
        self.require_namespace(namespace)?;
        ensure!(
            valid_label(name),
            "input_finalization_pvc_reference_invalid"
        );
        let url = self.resource_url(&format!(
            "api/v1/namespaces/{namespace}/persistentvolumeclaims/{name}"
        ))?;
        let body = self
            .get_bytes(url, OBJECT_LIMIT, "input_finalization_pvc_unavailable")
            .await?;
        serde_json::from_slice(&body).map_err(|_| anyhow!("input_finalization_pvc_unavailable"))
    }

    async fn delete_completed_job(
        &self,
        namespace: &str,
        name: &str,
        uid: &str,
        cancellation: &tokio_util::sync::CancellationToken,
    ) -> Result<()> {
        self.require_namespace(namespace)?;
        ensure!(
            valid_label(name) && valid_uid(uid),
            "input_finalization_job_reference_invalid"
        );
        let url =
            self.resource_url(&format!("apis/batch/v1/namespaces/{namespace}/jobs/{name}"))?;
        let token = self
            .token()
            .await
            .map_err(|_| anyhow!("input_finalization_delete_uncertain"))?;
        let response = self
            .request(
                self.client.delete(url).bearer_auth(token).json(&json!({
                    "apiVersion":"v1",
                    "kind":"DeleteOptions",
                    "propagationPolicy":"Foreground",
                    "preconditions":{"uid":uid}
                })),
                cancellation,
                "input_finalization_delete_uncertain",
            )
            .await?;
        ensure!(
            matches!(response.status(), StatusCode::OK | StatusCode::ACCEPTED),
            "input_finalization_delete_rejected"
        );
        let _ = bounded_response(
            response,
            OBJECT_LIMIT,
            "input_finalization_delete_uncertain",
        )
        .await?;
        Ok(())
    }

    async fn get_job_after_cleanup(&self, namespace: &str, name: &str) -> Result<Option<Value>> {
        self.require_namespace(namespace)?;
        ensure!(
            valid_label(name),
            "input_finalization_job_reference_invalid"
        );
        let url =
            self.resource_url(&format!("apis/batch/v1/namespaces/{namespace}/jobs/{name}"))?;
        let token = self
            .token()
            .await
            .map_err(|_| anyhow!("input_finalization_cleanup_observation_failed"))?;
        let response = self
            .client
            .get(url)
            .bearer_auth(token)
            .send()
            .await
            .map_err(|_| anyhow!("input_finalization_cleanup_observation_failed"))?;
        if response.status() == StatusCode::NOT_FOUND {
            return Ok(None);
        }
        ensure!(
            response.status() == StatusCode::OK,
            "input_finalization_cleanup_observation_failed"
        );
        let body = bounded_response(
            response,
            OBJECT_LIMIT,
            "input_finalization_cleanup_observation_failed",
        )
        .await?;
        let job: Value = serde_json::from_slice(&body)
            .map_err(|_| anyhow!("input_finalization_cleanup_observation_failed"))?;
        ensure!(
            job.pointer("/metadata/name").and_then(Value::as_str) == Some(name)
                && job.pointer("/metadata/namespace").and_then(Value::as_str) == Some(namespace)
                && job
                    .pointer("/metadata/uid")
                    .and_then(Value::as_str)
                    .is_some_and(valid_uid),
            "input_finalization_cleanup_observation_failed"
        );
        Ok(Some(job))
    }

    async fn get_pod_after_cleanup(
        &self,
        namespace: &str,
        name: &str,
        uid: &str,
    ) -> Result<Option<Value>> {
        self.require_namespace(namespace)?;
        ensure!(
            valid_label(name) && valid_uid(uid),
            "input_finalization_pod_reference_invalid"
        );
        // Kubernetes does not support a metadata.uid field selector for Pods.
        // Use the observed name for one GET, then fence the returned incarnation.
        let url = self.resource_url(&format!("api/v1/namespaces/{namespace}/pods/{name}"))?;
        let token = self
            .token()
            .await
            .map_err(|_| anyhow!("input_finalization_cleanup_observation_failed"))?;
        let response = self
            .client
            .get(url)
            .bearer_auth(token)
            .send()
            .await
            .map_err(|_| anyhow!("input_finalization_cleanup_observation_failed"))?;
        if response.status() == StatusCode::NOT_FOUND {
            return Ok(None);
        }
        ensure!(
            response.status() == StatusCode::OK,
            "input_finalization_cleanup_observation_failed"
        );
        let body = bounded_response(
            response,
            OBJECT_LIMIT,
            "input_finalization_cleanup_observation_failed",
        )
        .await?;
        let pod: Value = serde_json::from_slice(&body)
            .map_err(|_| anyhow!("input_finalization_cleanup_observation_failed"))?;
        ensure!(
            pod.pointer("/metadata/namespace").and_then(Value::as_str) == Some(namespace)
                && pod.pointer("/metadata/name").and_then(Value::as_str) == Some(name)
                && pod.pointer("/metadata/uid").and_then(Value::as_str) == Some(uid),
            "input_finalization_cleanup_observation_failed"
        );
        Ok(Some(pod))
    }

    async fn apply_input_seal(
        &self,
        mutation: &InputSealMutation,
        cancellation: &tokio_util::sync::CancellationToken,
    ) -> Result<Value> {
        self.require_namespace(mutation.namespace())?;
        ensure!(
            valid_label(mutation.name())
                && valid_uid(mutation.uid())
                && !mutation.resource_version().is_empty(),
            "input_finalization_seal_reference_invalid"
        );
        let body = mutation.body();
        let label = body
            .pointer("/metadata/labels/sandbox.enrichedlab.org~1seal-state")
            .and_then(Value::as_str)
            .filter(|value| *value == "sealed")
            .ok_or_else(|| anyhow!("input_finalization_seal_mutation_invalid"))?;
        let annotation = |name| {
            body.pointer(&format!(
                "/metadata/annotations/sandbox.enrichedlab.org~1{name}"
            ))
            .and_then(Value::as_str)
            .filter(|value| !value.is_empty())
            .ok_or_else(|| anyhow!("input_finalization_seal_mutation_invalid"))
        };
        let input_digest = annotation("input-digest")?;
        let staging_digest = annotation("input-staging-digest")?;
        let manifest_digest = annotation("input-manifest-digest")?;
        let attestation_digest = annotation("input-attestation-digest")?;
        let patch = json!([
            {"op":"test","path":"/metadata/uid","value":mutation.uid()},
            {"op":"test","path":"/metadata/resourceVersion","value":mutation.resource_version()},
            {"op":"replace","path":"/metadata/labels/sandbox.enrichedlab.org~1seal-state","value":label},
            {"op":"add","path":"/metadata/annotations/sandbox.enrichedlab.org~1input-digest","value":input_digest},
            {"op":"add","path":"/metadata/annotations/sandbox.enrichedlab.org~1input-staging-digest","value":staging_digest},
            {"op":"add","path":"/metadata/annotations/sandbox.enrichedlab.org~1input-manifest-digest","value":manifest_digest},
            {"op":"add","path":"/metadata/annotations/sandbox.enrichedlab.org~1input-attestation-digest","value":attestation_digest}
        ]);
        let url = self.resource_url(&format!(
            "api/v1/namespaces/{}/persistentvolumeclaims/{}",
            mutation.namespace(),
            mutation.name()
        ))?;
        let token = self
            .token()
            .await
            .map_err(|_| anyhow!("input_finalization_seal_uncertain"))?;
        let response = self
            .request(
                self.client
                    .patch(url)
                    .bearer_auth(token)
                    .header(reqwest::header::CONTENT_TYPE, "application/json-patch+json")
                    .body(
                        serde_json::to_vec(&patch)
                            .map_err(|_| anyhow!("input_finalization_seal_mutation_invalid"))?,
                    ),
                cancellation,
                "input_finalization_seal_uncertain",
            )
            .await?;
        ensure!(
            response.status() == StatusCode::OK,
            "input_finalization_seal_rejected"
        );
        let value: Value = serde_json::from_slice(
            &bounded_response(response, OBJECT_LIMIT, "input_finalization_seal_uncertain").await?,
        )
        .map_err(|_| anyhow!("input_finalization_seal_response_invalid"))?;
        ensure!(
            value.pointer("/metadata/name").and_then(Value::as_str) == Some(mutation.name())
                && value.pointer("/metadata/namespace").and_then(Value::as_str)
                    == Some(mutation.namespace())
                && value.pointer("/metadata/uid").and_then(Value::as_str) == Some(mutation.uid()),
            "input_finalization_seal_response_invalid"
        );
        Ok(value)
    }
}

/// A Kubernetes Status that refuses a create because a ResourceQuota has no room left.
fn quota_refused(body: &[u8]) -> bool {
    serde_json::from_slice::<serde_json::Value>(body).is_ok_and(|status| {
        status["kind"] == "Status"
            && status["reason"] == "Forbidden"
            && status["message"]
                .as_str()
                .is_some_and(|message| message.contains("exceeded quota"))
    })
}

async fn bounded_response(
    response: reqwest::Response,
    limit: usize,
    error: &'static str,
) -> Result<Vec<u8>> {
    ensure!(
        response
            .content_length()
            .is_none_or(|length| length <= limit as u64),
        error
    );
    let mut bytes = Vec::new();
    let mut stream = response.bytes_stream();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|_| anyhow!(error))?;
        ensure!(bytes.len().saturating_add(chunk.len()) <= limit, error);
        bytes.extend_from_slice(&chunk);
    }
    Ok(bytes)
}

fn pod_has_active_writer(pod: &Value, pvc_name: &str) -> Result<bool> {
    let phase = pod
        .pointer("/status/phase")
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow!("input_source_writer_observation_incomplete"))?;
    if matches!(phase, "Succeeded" | "Failed") {
        return Ok(false);
    }
    ensure!(
        matches!(phase, "Pending" | "Running"),
        "input_source_writer_observation_incomplete"
    );
    let volumes = pod
        .pointer("/spec/volumes")
        .and_then(Value::as_array)
        .ok_or_else(|| anyhow!("input_source_writer_observation_incomplete"))?;
    let mut source_volumes = std::collections::BTreeMap::new();
    for volume in volumes {
        let object = volume
            .as_object()
            .ok_or_else(|| anyhow!("input_source_writer_observation_incomplete"))?;
        let Some(source) = object.get("persistentVolumeClaim") else {
            continue;
        };
        let source = source
            .as_object()
            .ok_or_else(|| anyhow!("input_source_writer_observation_incomplete"))?;
        let claim_name = source
            .get("claimName")
            .and_then(Value::as_str)
            .ok_or_else(|| anyhow!("input_source_writer_observation_incomplete"))?;
        let source_read_only = match source.get("readOnly") {
            Some(value) => value
                .as_bool()
                .ok_or_else(|| anyhow!("input_source_writer_observation_incomplete"))?,
            None => false,
        };
        if claim_name == pvc_name {
            let volume_name = object
                .get("name")
                .and_then(Value::as_str)
                .ok_or_else(|| anyhow!("input_source_writer_observation_incomplete"))?;
            ensure!(
                source_volumes
                    .insert(volume_name, source_read_only)
                    .is_none(),
                "input_source_writer_observation_incomplete"
            );
        }
    }
    for container_class in ["containers", "initContainers", "ephemeralContainers"] {
        let Some(containers) = pod.pointer(&format!("/spec/{container_class}")) else {
            continue;
        };
        let containers = containers
            .as_array()
            .ok_or_else(|| anyhow!("input_source_writer_observation_incomplete"))?;
        for container in containers {
            let container = container
                .as_object()
                .ok_or_else(|| anyhow!("input_source_writer_observation_incomplete"))?;
            if container_uses_writable_pvc(container.get("volumeMounts"), &source_volumes)?
                || container_uses_writable_pvc(container.get("volumeDevices"), &source_volumes)?
            {
                return Ok(true);
            }
        }
    }
    Ok(false)
}

fn container_uses_writable_pvc(
    field: Option<&Value>,
    source_volumes: &std::collections::BTreeMap<&str, bool>,
) -> Result<bool> {
    let Some(values) = field else {
        return Ok(false);
    };
    let values = values
        .as_array()
        .ok_or_else(|| anyhow!("input_source_writer_observation_incomplete"))?;
    for value in values {
        let value = value
            .as_object()
            .ok_or_else(|| anyhow!("input_source_writer_observation_incomplete"))?;
        let name = value
            .get("name")
            .and_then(Value::as_str)
            .ok_or_else(|| anyhow!("input_source_writer_observation_incomplete"))?;
        let Some(source_read_only) = source_volumes.get(name) else {
            continue;
        };
        let mount_read_only = match value.get("readOnly") {
            Some(value) => value
                .as_bool()
                .ok_or_else(|| anyhow!("input_source_writer_observation_incomplete"))?,
            None => false,
        };
        if !source_read_only && !mount_read_only {
            return Ok(true);
        }
    }
    Ok(false)
}
