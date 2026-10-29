//! Private, server-owned direct Sandbox dispatch configuration.
//!
//! This module selects and validates a staged admission record. It does not create
//! a Sandbox, mount a PVC, contact Kubernetes, or choose a model/tool route.
use super::{
    admission::{self, AdmittedSandbox},
    input_request::{self, InputPreparationConfig},
    model::ClaimedTask,
    sandbox::{BrokerLeaseFence, SandboxBinding, SandboxBindingCatalog},
    terminal_exports::TerminalExportConfig,
};
use anyhow::{Context, Result, anyhow, bail};
use reqwest::Url;
use serde::Deserialize;
use std::{
    fs::File,
    io::Read,
    path::{Component, Path, PathBuf},
    time::Duration,
};
use tokio::time::{Instant, sleep, timeout};
use tokio_util::sync::CancellationToken;

const SCHEMA: &str = "sandbox.enrichedlab.org/direct-dispatch/v1";
const CATALOG_LIMIT: usize = 256 * 1024;

#[derive(Clone, Debug)]
pub struct KubernetesConnection {
    pub origin: Url,
    pub ca_path: PathBuf,
    pub token_path: PathBuf,
}

#[derive(Debug)]
pub struct PreparedDirectDispatch {
    pub kubernetes: KubernetesConnection,
    pub binding: SandboxBinding,
    pub admission: AdmittedSandbox,
    pub terminal_export: Option<TerminalExportConfig>,
}

#[derive(Debug)]
pub struct DirectDispatchCatalog {
    kubernetes: KubernetesConnection,
    admission_root: PathBuf,
    admission_writer_uid: u32,
    admission_wait: Duration,
    bindings: SandboxBindingCatalog,
    terminal_export: Option<TerminalExportConfig>,
    preparation: Option<InputPreparationConfig>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct CatalogDocument {
    schema_version: String,
    kubernetes: KubernetesDocument,
    admission: AdmissionDocument,
    bindings: Vec<SandboxBinding>,
    terminal_export: Option<TerminalExportConfig>,
    preparation: Option<InputPreparationConfig>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct KubernetesDocument {
    origin: String,
    ca_path: PathBuf,
    token_path: PathBuf,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct AdmissionDocument {
    root: PathBuf,
    writer_uid: u32,
    #[serde(default)]
    wait_seconds: u32,
}

/// Worker-owned supervision, independent of the eventual compute deadline.
pub struct AdmissionWait<'a> {
    pub worker_id: &'a str,
    pub lease_seconds: i32,
    pub poll_interval: Duration,
    pub stop: &'a CancellationToken,
}

impl DirectDispatchCatalog {
    /// Reads a bounded, private server configuration. The caller supplies this
    /// path from its deployment, never from a researcher or model request.
    pub fn load(path: &Path, catalog_writer_uid: u32) -> Result<Self> {
        let bytes = read_private_catalog(path, catalog_writer_uid)?;
        let document: CatalogDocument = serde_json::from_slice(&bytes)
            .map_err(|_| anyhow!("direct_dispatch_catalog_decode_failed"))?;
        if document.schema_version != SCHEMA {
            bail!("direct_dispatch_catalog_schema_invalid")
        }
        if document.admission.wait_seconds > 3600 {
            bail!("direct_dispatch_admission_wait_invalid")
        }
        if let Some(config) = &document.preparation {
            config.validate(&document.admission.root, document.admission.wait_seconds)?;
        }
        let origin = parse_origin(&document.kubernetes.origin)?;
        if let Some(config) = &document.terminal_export {
            config.validate()?;
        }
        validate_absolute_path(
            &document.kubernetes.ca_path,
            "direct_dispatch_ca_path_invalid",
        )?;
        validate_absolute_path(
            &document.kubernetes.token_path,
            "direct_dispatch_token_path_invalid",
        )?;
        validate_absolute_path(
            &document.admission.root,
            "direct_dispatch_admission_root_invalid",
        )?;
        if document.bindings.is_empty() {
            bail!("direct_dispatch_bindings_empty")
        }
        let bindings = SandboxBindingCatalog::new(document.bindings)
            .map_err(|_| anyhow!("direct_dispatch_bindings_invalid"))?;
        Ok(Self {
            kubernetes: KubernetesConnection {
                origin,
                ca_path: document.kubernetes.ca_path,
                token_path: document.kubernetes.token_path,
            },
            admission_root: document.admission.root,
            admission_writer_uid: document.admission.writer_uid,
            admission_wait: Duration::from_secs(document.admission.wait_seconds.into()),
            bindings,
            terminal_export: document.terminal_export,
            preparation: document.preparation,
        })
    }

    /// Returns `None` when this claimed task is not in the private direct
    /// catalog, preserving the existing runtime caller's fallback decision.
    /// A matched task must have a valid admission record and never falls back.
    pub fn prepare(&self, task: &ClaimedTask) -> Result<Option<PreparedDirectDispatch>> {
        let Some(binding) = self.bindings.find(task) else {
            return Ok(None);
        };
        let admission = admission::load(
            &self.admission_root,
            &admission_record_name(task)?,
            self.admission_writer_uid,
            task,
            binding,
        )
        .context("direct_dispatch_admission_invalid")?;
        Ok(Some(self.prepared(binding, admission)))
    }

    /// Accept only absence of the exact admission file as pending. The claimed
    /// lease stays current while the trusted custodian stages input. This does
    /// not request or start Kubernetes resources.
    pub async fn prepare_for_execution<L: BrokerLeaseFence>(
        &self,
        lease: &L,
        task: &ClaimedTask,
        control: AdmissionWait<'_>,
    ) -> Result<Option<PreparedDirectDispatch>> {
        let Some(binding) = self.bindings.find(task) else {
            return Ok(None);
        };
        if self.admission_wait.is_zero() {
            return self.prepare(task);
        }
        if control.worker_id.is_empty()
            || !(30..=3600).contains(&control.lease_seconds)
            || control.poll_interval.is_zero()
        {
            bail!("direct_dispatch_admission_supervision_invalid")
        }
        let name = admission_record_name(task)?;
        let renew_interval = Duration::from_secs((control.lease_seconds / 3) as u64);
        let poll_interval = control.poll_interval.min(renew_interval);
        let operation = async {
            admission_authority(lease, task, &control, true).await?;
            let mut next_renewal = Instant::now() + renew_interval;
            let mut waiting_reported = false;
            let mut preparation_requested = false;
            loop {
                let root = self.admission_root.clone();
                let name = name.clone();
                let writer_uid = self.admission_writer_uid;
                let claimed = task.clone();
                let selected = binding.clone();
                let record = tokio::task::spawn_blocking(move || {
                    admission::load(&root, &name, writer_uid, &claimed, &selected)
                })
                .await
                .map_err(|_| anyhow!("direct_dispatch_admission_read_failed"))?;
                match record {
                    Ok(admission) => {
                        admission_authority(lease, task, &control, true).await?;
                        tracing::info!(task_id=%task.id, attempt=task.attempt,
                            lease_generation=task.lease_generation, "direct_admission_ready");
                        return Ok(Some(self.prepared(binding, admission)));
                    }
                    Err(error) if absent_admission(&error) => {
                        if let Some(config) = &self.preparation
                            && !preparation_requested
                        {
                            admission_authority(lease, task, &control, false).await?;
                            // A digest-only reference; model-written bytes never
                            // enter the notice, plan or any command line.
                            let admitted = lease
                                .admitted_code(
                                    task.id,
                                    control.worker_id,
                                    task.attempt,
                                    task.lease_generation,
                                )
                                .await
                                .map_err(|_| {
                                    anyhow!("direct_dispatch_admitted_code_unavailable")
                                })?;
                            input_request::publish(
                                config,
                                task,
                                control.worker_id,
                                binding,
                                admitted.as_ref(),
                            )
                            .await?;
                            admission_authority(lease, task, &control, false).await?;
                            preparation_requested = true;
                            tracing::info!(task_id=%task.id, attempt=task.attempt,
                                lease_generation=task.lease_generation, "direct_input_preparation_requested");
                        }
                        if !waiting_reported {
                            tracing::info!(task_id=%task.id, attempt=task.attempt,
                                lease_generation=task.lease_generation, "direct_admission_pending");
                            waiting_reported = true;
                        }
                    }
                    Err(_) => return Err(anyhow!("direct_dispatch_admission_invalid")),
                }
                sleep(poll_interval).await;
                let renew = Instant::now() >= next_renewal;
                admission_authority(lease, task, &control, renew).await?;
                if renew {
                    next_renewal = Instant::now() + renew_interval;
                }
            }
        };
        tokio::select! {
            biased;
            _ = control.stop.cancelled() => Err(anyhow!("direct_dispatch_admission_stopped")),
            result = timeout(self.admission_wait, operation) => {
                result.map_err(|_| anyhow!("direct_dispatch_admission_timeout"))?
            }
        }
    }

    fn prepared(
        &self,
        binding: &SandboxBinding,
        admission: AdmittedSandbox,
    ) -> PreparedDirectDispatch {
        PreparedDirectDispatch {
            kubernetes: self.kubernetes.clone(),
            binding: binding.clone(),
            admission,
            terminal_export: self.terminal_export.clone(),
        }
    }
}

fn absent_admission(error: &anyhow::Error) -> bool {
    // Only the final descriptor-relative open preserves this context and errno.
    // Missing/unsafe parents and dangling symlinks cannot become a pending task.
    error.to_string() == "admission_record_unavailable"
        && error.chain().any(|cause| {
            cause
                .downcast_ref::<std::io::Error>()
                .is_some_and(|error| error.kind() == std::io::ErrorKind::NotFound)
        })
}

async fn admission_authority<L: BrokerLeaseFence>(
    lease: &L,
    task: &ClaimedTask,
    control: &AdmissionWait<'_>,
    renew: bool,
) -> Result<()> {
    let check = async {
        if lease
            .cancel_requested(task.id, control.worker_id, task.lease_generation)
            .await
            .map_err(|_| anyhow!("direct_dispatch_admission_authority_unavailable"))?
        {
            bail!("direct_dispatch_admission_cancelled")
        }
        if renew
            && !lease
                .renew_lease(
                    task.id,
                    control.worker_id,
                    task.lease_generation,
                    control.lease_seconds,
                )
                .await
                .map_err(|_| anyhow!("direct_dispatch_admission_authority_unavailable"))?
        {
            bail!("direct_dispatch_admission_lease_lost")
        }
        Ok(())
    };
    timeout(
        Duration::from_secs((control.lease_seconds / 3) as u64),
        check,
    )
    .await
    .map_err(|_| anyhow!("direct_dispatch_admission_authority_timeout"))?
}

pub fn admission_record_name(task: &ClaimedTask) -> Result<String> {
    if task.attempt < 1 || task.lease_generation < 1 {
        bail!("direct_dispatch_task_attempt_invalid")
    }
    Ok(format!(
        "{}-{}-{}.json",
        task.id, task.attempt, task.lease_generation
    ))
}

fn parse_origin(value: &str) -> Result<Url> {
    let origin = Url::parse(value).map_err(|_| anyhow!("direct_dispatch_origin_invalid"))?;
    if origin.scheme() != "https"
        || origin.host_str().is_none()
        || !origin.username().is_empty()
        || origin.password().is_some()
        || origin.path() != "/"
        || origin.query().is_some()
        || origin.fragment().is_some()
    {
        bail!("direct_dispatch_origin_invalid")
    }
    Ok(origin)
}

fn validate_absolute_path(path: &Path, code: &str) -> Result<()> {
    if !path.is_absolute()
        || path == Path::new("/")
        || path
            .components()
            .any(|component| !matches!(component, Component::RootDir | Component::Normal(_)))
    {
        bail!("{code}")
    }
    Ok(())
}

fn read_private_catalog(path: &Path, catalog_writer_uid: u32) -> Result<Vec<u8>> {
    let (root, name) = split_catalog_path(path)?;
    let file =
        admission::open_record(&root, name, catalog_writer_uid).map_err(map_catalog_open_error)?;
    read_bounded(file)
}

fn split_catalog_path(path: &Path) -> Result<(PathBuf, &str)> {
    validate_absolute_path(path, "direct_dispatch_catalog_path_invalid")?;
    let root = path
        .parent()
        .filter(|parent| *parent != Path::new("/"))
        .ok_or_else(|| anyhow!("direct_dispatch_catalog_path_invalid"))?;
    let name = path
        .file_name()
        .and_then(|name| name.to_str())
        .filter(|name| !name.is_empty())
        .ok_or_else(|| anyhow!("direct_dispatch_catalog_path_invalid"))?;
    Ok((root.to_path_buf(), name))
}

fn map_catalog_open_error(error: anyhow::Error) -> anyhow::Error {
    let private = error.chain().any(|cause| {
        matches!(
            cause.to_string().as_str(),
            "admission_root_not_private" | "admission_record_not_private_regular"
        )
    });
    if private {
        anyhow!("direct_dispatch_catalog_not_private_regular")
    } else if error
        .chain()
        .any(|cause| cause.to_string() == "admission_size_exceeded")
    {
        anyhow!("direct_dispatch_catalog_size_exceeded")
    } else {
        anyhow!("direct_dispatch_catalog_unavailable")
    }
}

fn read_bounded(file: File) -> Result<Vec<u8>> {
    let mut bytes = Vec::new();
    file.take((CATALOG_LIMIT + 1) as u64)
        .read_to_end(&mut bytes)
        .map_err(|_| anyhow!("direct_dispatch_catalog_unavailable"))?;
    if bytes.len() > CATALOG_LIMIT {
        bail!("direct_dispatch_catalog_size_exceeded")
    }
    Ok(bytes)
}
