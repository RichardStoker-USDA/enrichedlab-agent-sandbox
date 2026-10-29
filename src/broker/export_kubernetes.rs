//! Namespace-bound, create-only transport for the trusted export publisher.
//! Secret bodies stay inside this module; only typed metadata leaves it.
use super::export_publication::{
    ExportObject, ExportObjectKind, ExportObjectReceipt, ExportPublicationTransport,
};
use anyhow::{Result, anyhow, ensure};
use async_trait::async_trait;
use futures_util::StreamExt;
use reqwest::{Client, StatusCode, Url};
use serde::Deserialize;
use std::{
    io::Read,
    path::{Path, PathBuf},
    time::Duration,
};
use tokio_util::sync::CancellationToken;

const RESPONSE_LIMIT: usize = 2 * 1024 * 1024;

pub struct ExportKubernetesClient {
    client: Client,
    api: Url,
    namespace: String,
    token: PathBuf,
}

impl ExportKubernetesClient {
    /// Operator-owned endpoint and projected service-account files. This is
    /// deliberately separate from the worker's read/runtime grants.
    pub fn from_service_account(
        api: Url,
        namespace: String,
        token: PathBuf,
        ca: &Path,
    ) -> Result<Self> {
        ensure!(valid_label(&namespace), "export_api_namespace_invalid");
        ensure!(
            api.scheme() == "https"
                && api.host_str().is_some()
                && api.username().is_empty()
                && api.password().is_none()
                && api.path() == "/"
                && api.query().is_none()
                && api.fragment().is_none(),
            "export_api_origin_invalid"
        );
        ensure!(token.is_absolute(), "export_api_token_reference_invalid");
        let _ = rustls::crypto::ring::default_provider().install_default();
        let bytes =
            read_bounded(ca, 64 * 1024).map_err(|_| anyhow!("export_api_ca_unavailable"))?;
        let certificates = reqwest::Certificate::from_pem_bundle(&bytes)
            .map_err(|_| anyhow!("export_api_ca_invalid"))?;
        ensure!(!certificates.is_empty(), "export_api_ca_invalid");
        let client = Client::builder()
            .tls_certs_only(certificates)
            .https_only(true)
            .no_proxy()
            .redirect(reqwest::redirect::Policy::none())
            .connect_timeout(Duration::from_secs(5))
            .timeout(Duration::from_secs(15))
            .build()
            .map_err(|_| anyhow!("export_api_client_invalid"))?;
        Ok(Self {
            client,
            api,
            namespace,
            token,
        })
    }
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
impl ExportPublicationTransport for ExportKubernetesClient {
    fn namespace(&self) -> &str {
        &self.namespace
    }

    async fn create(
        &self,
        object: &ExportObject,
        cancellation: &CancellationToken,
    ) -> Result<ExportObjectReceipt> {
        ensure!(
            object.namespace() == self.namespace,
            "export_api_namespace_mismatch"
        );
        let prefix = match object.kind() {
            ExportObjectKind::NetworkPolicy => "apis/networking.k8s.io/v1",
            _ => "api/v1",
        };
        let path = format!(
            "{prefix}/namespaces/{}/{}",
            self.namespace,
            object.kind().resource()
        );
        let url = self
            .api
            .join(&path)
            .map_err(|_| anyhow!("export_api_path_invalid"))?;
        let token_path = self.token.clone();
        let operation = async {
            // Projected service-account files rotate through symlinks. The
            // reference is trusted configuration; the value is bounded and never logged.
            let token = tokio::task::spawn_blocking(move || read_bounded(&token_path, 16 * 1024))
                .await
                .map_err(|_| anyhow!("export_api_token_unavailable"))?
                .map_err(|_| anyhow!("export_api_token_unavailable"))?;
            let token = std::str::from_utf8(&token)
                .map_err(|_| anyhow!("export_api_token_invalid"))?
                .trim();
            ensure!(!token.is_empty(), "export_api_token_invalid");
            let response = self
                .client
                .post(url)
                .bearer_auth(token)
                .json(object.body())
                .send()
                .await
                .map_err(|_| anyhow!("export_api_create_uncertain"))?;
            match response.status() {
                StatusCode::CREATED => {}
                StatusCode::CONFLICT => {
                    return Err(anyhow!("export_api_existing_object_requires_recovery"));
                }
                _ => return Err(anyhow!("export_api_create_rejected")),
            }
            ensure!(
                response
                    .content_length()
                    .is_none_or(|n| n <= RESPONSE_LIMIT as u64),
                "export_api_response_limit"
            );
            let mut bytes = Vec::new();
            let mut stream = response.bytes_stream();
            while let Some(chunk) = stream.next().await {
                let chunk = chunk.map_err(|_| anyhow!("export_api_create_uncertain"))?;
                ensure!(
                    bytes.len().saturating_add(chunk.len()) <= RESPONSE_LIMIT,
                    "export_api_response_limit"
                );
                bytes.extend_from_slice(&chunk);
            }
            // Unknown fields, including Secret.data, are discarded by this
            // deserializer. No raw API error or response is exposed.
            let created: CreatedObject = serde_json::from_slice(&bytes)
                .map_err(|_| anyhow!("export_api_response_invalid"))?;
            ensure!(
                created.metadata.name == object.name()
                    && created.metadata.namespace == self.namespace
                    && valid_uid(&created.metadata.uid),
                "export_api_created_identity_mismatch"
            );
            Ok(ExportObjectReceipt {
                kind: object.kind(),
                name: created.metadata.name,
                uid: created.metadata.uid,
                request_sha256: object.digest().to_owned(),
            })
        };
        tokio::select! {
            biased;
            _ = cancellation.cancelled() => Err(anyhow!("export_publication_cancelled")),
            result = operation => result,
        }
    }
}

pub(crate) fn valid_label(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 63
        && value.starts_with(|c: char| c.is_ascii_lowercase() || c.is_ascii_digit())
        && value.ends_with(|c: char| c.is_ascii_lowercase() || c.is_ascii_digit())
        && value
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
}
pub(crate) fn valid_uid(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 128
        && value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-')
}
fn read_bounded(path: &Path, limit: usize) -> Result<Vec<u8>> {
    let mut bytes = Vec::new();
    std::fs::File::open(path)?
        .take((limit + 1) as u64)
        .read_to_end(&mut bytes)?;
    ensure!(bytes.len() <= limit, "export_api_file_limit");
    Ok(bytes)
}
