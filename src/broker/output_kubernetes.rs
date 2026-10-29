//! Namespace-bound create/read Kubernetes adapter for output inventory Jobs.

use super::output_workload::{
    OutputInventoryObject, OutputInventoryObjectKind, OutputInventoryObjectReceipt,
    OutputInventoryTransport,
};
use anyhow::{Result, anyhow, ensure};
use async_trait::async_trait;
use futures_util::StreamExt;
use reqwest::{Client, StatusCode, Url};
use serde::Deserialize;
use serde_json::Value;
use std::{
    io::Read,
    path::{Path, PathBuf},
    time::Duration,
};
use tokio_util::sync::CancellationToken;

const OBJECT_LIMIT: usize = 2 * 1024 * 1024;
const LOG_LIMIT: usize = 1024 * 1024 + 1;
const TOKEN_LIMIT: usize = 16 * 1024;
const PAGE_LIMIT: usize = 100;
const CONTAINER: &str = "output-inventory";

pub struct OutputInventoryKubernetesClient {
    client: Client,
    api: Url,
    namespace: String,
    token: PathBuf,
}
impl OutputInventoryKubernetesClient {
    pub fn from_service_account(
        api: Url,
        namespace: String,
        token: PathBuf,
        ca: &Path,
    ) -> Result<Self> {
        ensure!(dns(&namespace), "output_kubernetes_namespace_invalid");
        ensure!(
            api.scheme() == "https"
                && api.host_str().is_some()
                && api.username().is_empty()
                && api.password().is_none()
                && api.path() == "/"
                && api.query().is_none()
                && api.fragment().is_none(),
            "output_kubernetes_origin_invalid"
        );
        ensure!(
            token.is_absolute() && ca.is_absolute(),
            "output_kubernetes_file_reference_invalid"
        );
        let _ = rustls::crypto::ring::default_provider().install_default();
        let ca =
            read_bounded(ca, 64 * 1024).map_err(|_| anyhow!("output_kubernetes_ca_unavailable"))?;
        let certs = reqwest::Certificate::from_pem_bundle(&ca)
            .map_err(|_| anyhow!("output_kubernetes_ca_invalid"))?;
        ensure!(!certs.is_empty(), "output_kubernetes_ca_invalid");
        Ok(Self {
            client: Client::builder()
                .tls_certs_only(certs)
                .https_only(true)
                .no_proxy()
                .redirect(reqwest::redirect::Policy::none())
                .connect_timeout(Duration::from_secs(5))
                .timeout(Duration::from_secs(15))
                .build()
                .map_err(|_| anyhow!("output_kubernetes_client_invalid"))?,
            api,
            namespace,
            token,
        })
    }
    fn ns(&self, ns: &str) -> Result<()> {
        ensure!(
            ns == self.namespace && dns(ns),
            "output_kubernetes_namespace_mismatch"
        );
        Ok(())
    }
    fn url(&self, path: &str) -> Result<Url> {
        self.api
            .join(path)
            .map_err(|_| anyhow!("output_kubernetes_path_invalid"))
    }
    async fn token(&self) -> Result<String> {
        let path = self.token.clone();
        let bytes = tokio::task::spawn_blocking(move || read_bounded(&path, TOKEN_LIMIT))
            .await
            .map_err(|_| anyhow!("output_kubernetes_token_unavailable"))?
            .map_err(|_| anyhow!("output_kubernetes_token_unavailable"))?;
        let text = std::str::from_utf8(&bytes)
            .map_err(|_| anyhow!("output_kubernetes_token_invalid"))?
            .trim();
        ensure!(!text.is_empty(), "output_kubernetes_token_invalid");
        Ok(text.into())
    }
    async fn get(&self, url: Url, limit: usize, error: &'static str) -> Result<Vec<u8>> {
        let token = self.token().await.map_err(|_| anyhow!(error))?;
        let r = self
            .client
            .get(url)
            .bearer_auth(token)
            .send()
            .await
            .map_err(|_| anyhow!(error))?;
        ensure!(r.status() == StatusCode::OK, error);
        body(r, limit, error).await
    }
    async fn pods(&self, ns: &str, selector: Option<&str>) -> Result<PodList> {
        self.ns(ns)?;
        let mut url = self.url(&format!("api/v1/namespaces/{ns}/pods"))?;
        {
            let mut q = url.query_pairs_mut();
            q.append_pair("limit", &PAGE_LIMIT.to_string());
            if let Some(s) = selector {
                q.append_pair("labelSelector", s);
            }
        }
        let bytes = self
            .get(url, OBJECT_LIMIT, "output_kubernetes_pod_unavailable")
            .await?;
        serde_json::from_slice(&bytes).map_err(|_| anyhow!("output_kubernetes_pod_unavailable"))
    }
}
#[async_trait]
impl OutputInventoryTransport for OutputInventoryKubernetesClient {
    fn namespace(&self) -> &str {
        &self.namespace
    }
    async fn create(
        &self,
        object: &OutputInventoryObject,
        cancellation: &CancellationToken,
    ) -> Result<OutputInventoryObjectReceipt> {
        self.ns(object.namespace())?;
        let (prefix, resource) = match object.kind() {
            OutputInventoryObjectKind::NetworkPolicy => {
                ("apis/networking.k8s.io/v1", "networkpolicies")
            }
            OutputInventoryObjectKind::Secret => ("api/v1", "secrets"),
            OutputInventoryObjectKind::Job => ("apis/batch/v1", "jobs"),
        };
        let url = self.url(&format!(
            "{prefix}/namespaces/{}/{}",
            self.namespace, resource
        ))?;
        let token = self
            .token()
            .await
            .map_err(|_| anyhow!("output_kubernetes_create_uncertain"))?;
        let r = tokio::select! {biased;_=cancellation.cancelled()=>return Err(anyhow!("output_inventory_cancelled")), r=self.client.post(url).bearer_auth(token).json(object.body()).send()=>r.map_err(|_|anyhow!("output_kubernetes_create_uncertain"))?};
        ensure!(
            r.status() == StatusCode::CREATED,
            "output_kubernetes_create_rejected"
        );
        let bytes = body(r, OBJECT_LIMIT, "output_kubernetes_create_uncertain").await?;
        let created: Created = serde_json::from_slice(&bytes)
            .map_err(|_| anyhow!("output_kubernetes_create_uncertain"))?;
        ensure!(
            created.metadata.namespace == self.namespace
                && created.metadata.name == object.name()
                && uid(&created.metadata.uid),
            "output_kubernetes_create_identity_invalid"
        );
        Ok(OutputInventoryObjectReceipt {
            kind: object.kind().clone(),
            namespace: created.metadata.namespace,
            name: created.metadata.name,
            uid: created.metadata.uid,
        })
    }
    async fn get_pvc(&self, ns: &str, name: &str) -> Result<Value> {
        self.ns(ns)?;
        ensure!(dns(name), "output_kubernetes_pvc_reference_invalid");
        let bytes = self
            .get(
                self.url(&format!(
                    "api/v1/namespaces/{ns}/persistentvolumeclaims/{name}"
                ))?,
                OBJECT_LIMIT,
                "output_kubernetes_pvc_unavailable",
            )
            .await?;
        serde_json::from_slice(&bytes).map_err(|_| anyhow!("output_kubernetes_pvc_unavailable"))
    }
    async fn has_active_writer(&self, ns: &str, pvc: &str) -> Result<bool> {
        self.ns(ns)?;
        ensure!(dns(pvc), "output_kubernetes_pvc_reference_invalid");
        let pods = self.pods(ns, None).await?;
        ensure!(
            pods.metadata.continuation.is_empty() && pods.items.len() <= PAGE_LIMIT,
            "output_kubernetes_writer_pagination"
        );
        for pod in pods.items {
            if pod
                .pointer("/status/phase")
                .and_then(Value::as_str)
                .is_none_or(|x| matches!(x, "Succeeded" | "Failed"))
            {
                continue;
            }
            if writer(&pod, pvc)? {
                return Ok(true);
            }
        }
        Ok(false)
    }
    async fn get_job(&self, ns: &str, name: &str) -> Result<Value> {
        self.ns(ns)?;
        ensure!(dns(name), "output_kubernetes_job_reference_invalid");
        let b = self
            .get(
                self.url(&format!("apis/batch/v1/namespaces/{ns}/jobs/{name}"))?,
                OBJECT_LIMIT,
                "output_kubernetes_job_unavailable",
            )
            .await?;
        serde_json::from_slice(&b).map_err(|_| anyhow!("output_kubernetes_job_unavailable"))
    }
    async fn get_controlled_pod(&self, ns: &str, job: &str) -> Result<Option<Value>> {
        self.ns(ns)?;
        ensure!(dns(job), "output_kubernetes_job_reference_invalid");
        let pods = self
            .pods(ns, Some(&format!("batch.kubernetes.io/job-name={job}")))
            .await?;
        ensure!(
            pods.metadata.continuation.is_empty(),
            "output_kubernetes_pod_pagination"
        );
        match pods.items.len() {
            0 => Ok(None),
            1 => {
                let p = pods.items.into_iter().next().unwrap();
                ensure!(
                    p.pointer("/metadata/labels/batch.kubernetes.io~1job-name")
                        .and_then(Value::as_str)
                        == Some(job),
                    "output_kubernetes_pod_selector_mismatch"
                );
                Ok(Some(p))
            }
            _ => Err(anyhow!("output_kubernetes_pod_ambiguous")),
        }
    }
    async fn helper_stdout(&self, ns: &str, pod: &str, container: &str) -> Result<Vec<u8>> {
        self.ns(ns)?;
        ensure!(
            dns(pod) && container == CONTAINER,
            "output_kubernetes_log_reference_invalid"
        );
        let mut url = self.url(&format!("api/v1/namespaces/{ns}/pods/{pod}/log"))?;
        url.query_pairs_mut()
            .append_pair("container", CONTAINER)
            .append_pair("limitBytes", &LOG_LIMIT.to_string())
            .append_pair("timestamps", "false")
            .append_pair("previous", "false");
        self.get(url, LOG_LIMIT, "output_kubernetes_log_unavailable")
            .await
    }
}
#[derive(Deserialize)]
struct Created {
    metadata: CreatedMetadata,
}
#[derive(Deserialize)]
struct CreatedMetadata {
    name: String,
    namespace: String,
    uid: String,
}
#[derive(Deserialize)]
struct PodList {
    metadata: Page,
    items: Vec<Value>,
}
#[derive(Default, Deserialize)]
struct Page {
    #[serde(default, rename = "continue")]
    continuation: String,
}
async fn body(response: reqwest::Response, limit: usize, error: &'static str) -> Result<Vec<u8>> {
    let mut stream = response.bytes_stream();
    let mut out = Vec::new();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|_| anyhow!(error))?;
        ensure!(out.len().saturating_add(chunk.len()) <= limit, error);
        out.extend_from_slice(&chunk)
    }
    Ok(out)
}
fn read_bounded(path: &Path, limit: usize) -> Result<Vec<u8>> {
    let mut out = Vec::new();
    std::fs::File::open(path)?
        .take((limit + 1) as u64)
        .read_to_end(&mut out)?;
    ensure!(out.len() <= limit, "output_kubernetes_file_limit");
    Ok(out)
}
fn writer(pod: &Value, pvc: &str) -> Result<bool> {
    let volumes = pod
        .pointer("/spec/volumes")
        .and_then(Value::as_array)
        .ok_or_else(|| anyhow!("output_kubernetes_writer_unavailable"))?;
    let names: std::collections::HashSet<_> = volumes
        .iter()
        .filter(|v| {
            v.pointer("/persistentVolumeClaim/claimName")
                .and_then(Value::as_str)
                == Some(pvc)
                && !v
                    .pointer("/persistentVolumeClaim/readOnly")
                    .and_then(Value::as_bool)
                    .unwrap_or(false)
        })
        .filter_map(|v| v.get("name").and_then(Value::as_str))
        .collect();
    if names.is_empty() {
        return Ok(false);
    };
    for path in [
        "/spec/containers",
        "/spec/initContainers",
        "/spec/ephemeralContainers",
    ] {
        let Some(containers) = pod.pointer(path).and_then(Value::as_array) else {
            continue;
        };
        for c in containers {
            let mounts = c
                .get("volumeMounts")
                .and_then(Value::as_array)
                .ok_or_else(|| anyhow!("output_kubernetes_writer_unavailable"))?;
            if mounts.iter().any(|m| {
                m.get("name")
                    .and_then(Value::as_str)
                    .is_some_and(|n| names.contains(n))
                    && !m.get("readOnly").and_then(Value::as_bool).unwrap_or(false)
            }) {
                return Ok(true);
            }
        }
    }
    Ok(false)
}
fn dns(v: &str) -> bool {
    !v.is_empty()
        && v.len() <= 63
        && !v.starts_with('-')
        && !v.ends_with('-')
        && v.bytes()
            .all(|x| x.is_ascii_lowercase() || x.is_ascii_digit() || x == b'-')
}
fn uid(v: &str) -> bool {
    v.len() >= 8 && v.len() <= 128 && v.bytes().all(|x| x.is_ascii_alphanumeric() || x == b'-')
}
