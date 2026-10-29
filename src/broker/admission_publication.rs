//! Publish an observed input seal and its validated direct admission by reference.
use super::{
    admission::encode_staged,
    dispatch::admission_record_name,
    export_kubernetes::valid_uid,
    input_completion::ObservedInputSeal,
    model::ClaimedTask,
    sandbox::{
        BrokerLeaseFence, CanonicalSandbox, SandboxAuthorization, SandboxBinding,
        validate_custody_pvc,
    },
};
use anyhow::{Result, anyhow, ensure};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    path::{Path, PathBuf},
    time::Duration,
};
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AdmissionPublicationConfig {
    pub admission_root: PathBuf,
    pub custody_root: PathBuf,
    pub writer_uid: u32,
    pub lease_seconds: i32,
}

/// UIDs received by the site custodian when it created these two run volumes.
/// Fresh observed objects must still have these same identities.
#[derive(Clone, Debug)]
pub struct CreatedVolumeIdentities {
    pub input_uid: String,
    pub artifact_uid: String,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AdmissionPublicationReceipt {
    pub task_id: Uuid,
    pub attempt: i32,
    pub lease_generation: i64,
    pub input_pvc_uid: String,
    pub artifact_pvc_uid: String,
    pub copy_job_uid: String,
    pub copy_pod_uid: String,
    pub original_seal_sha256: String,
    pub admission_sha256: String,
}

pub struct PreparedAdmissionPublication {
    config: AdmissionPublicationConfig,
    receipt: AdmissionPublicationReceipt,
    record_name: String,
    admission: Vec<u8>,
    seal: Vec<u8>,
}

impl PreparedAdmissionPublication {
    /// `seal` must come from the observed helper-completion validator. The site
    /// custodian owns the create acknowledgements and fresh PVC observations.
    #[allow(clippy::too_many_arguments)]
    pub fn prepare(
        config: AdmissionPublicationConfig,
        task: &ClaimedTask,
        binding: &SandboxBinding,
        authorization: &SandboxAuthorization,
        canonical: &CanonicalSandbox,
        seal: &ObservedInputSeal,
        created: &CreatedVolumeIdentities,
        observed_input: &Value,
        observed_artifacts: &Value,
    ) -> Result<Self> {
        ensure!(
            private_root(&config.admission_root)
                && private_root(&config.custody_root)
                && !config.admission_root.starts_with(&config.custody_root)
                && !config.custody_root.starts_with(&config.admission_root)
                && (30..=3600).contains(&config.lease_seconds),
            "admission_publication_config_invalid"
        );
        ensure!(
            valid_uid(&created.input_uid)
                && valid_uid(&created.artifact_uid)
                && created.input_uid != created.artifact_uid,
            "admission_volume_identity_invalid"
        );
        let mut sandbox = canonical.manifest().clone();
        let annotations = sandbox
            .pointer_mut("/metadata/annotations")
            .and_then(Value::as_object_mut)
            .ok_or_else(|| anyhow!("admission_annotations_missing"))?;
        for (key, uid) in [
            ("sandbox.enrichedlab.org/input-pvc-uid", &created.input_uid),
            (
                "sandbox.enrichedlab.org/artifact-pvc-uid",
                &created.artifact_uid,
            ),
        ] {
            ensure!(
                annotations
                    .get(key)
                    .is_none_or(|value| value.as_str() == Some(uid.as_str())),
                "admission_volume_identity_changed"
            );
            annotations.insert(key.into(), json!(uid));
        }
        validate_custody_pvc(
            observed_input,
            &authorization.run_id,
            "immutable-input",
            true,
            Some(&sandbox),
        )
        .map_err(|_| anyhow!("admission_sealed_volume_mismatch"))?;
        validate_custody_pvc(
            observed_artifacts,
            &authorization.run_id,
            "durable-artifacts",
            false,
            Some(&sandbox),
        )
        .map_err(|_| anyhow!("admission_artifact_volume_mismatch"))?;
        let admission = encode_staged(task, binding, authorization, seal.attestation(), sandbox)
            .map_err(|_| anyhow!("admission_publication_contract_mismatch"))?;
        let original = seal.original_receipt();
        ensure!(
            !original.is_empty() && original.len() <= 1024 * 1024,
            "admission_seal_size_invalid"
        );
        let receipt = AdmissionPublicationReceipt {
            task_id: task.id,
            attempt: task.attempt,
            lease_generation: task.lease_generation,
            input_pvc_uid: created.input_uid.clone(),
            artifact_pvc_uid: created.artifact_uid.clone(),
            copy_job_uid: seal.job_uid().into(),
            copy_pod_uid: seal.pod_uid().into(),
            original_seal_sha256: hex::encode(Sha256::digest(original)),
            admission_sha256: hex::encode(Sha256::digest(&admission)),
        };
        Ok(Self {
            config,
            receipt,
            record_name: admission_record_name(task)?,
            admission,
            seal: original.to_vec(),
        })
    }

    pub fn receipt(&self) -> &AdmissionPublicationReceipt {
        &self.receipt
    }

    /// Original evidence is durable before the admission appears. A prior exact
    /// publication is reusable; a changed record is never replaced.
    pub async fn publish<L: BrokerLeaseFence>(
        &self,
        lease: &L,
        worker_id: &str,
        cancellation: &CancellationToken,
    ) -> Result<AdmissionPublicationReceipt> {
        self.fence(lease, worker_id, cancellation).await?;
        let root = self.config.custody_root.clone();
        let name = format!("{}.seal.json", self.record_name);
        let bytes = self.seal.clone();
        let uid = self.config.writer_uid;
        tokio::task::spawn_blocking(move || publish_private(&root, &name, uid, &bytes))
            .await
            .map_err(|_| anyhow!("admission_evidence_staging_failed"))??;
        let root = self.config.custody_root.clone();
        let name = format!("{}.observation.json", self.record_name);
        let bytes =
            serde_json::to_vec(&self.receipt).map_err(|_| anyhow!("admission_receipt_invalid"))?;
        tokio::task::spawn_blocking(move || publish_private(&root, &name, uid, &bytes))
            .await
            .map_err(|_| anyhow!("admission_evidence_staging_failed"))??;
        self.fence(lease, worker_id, cancellation).await?;
        let root = self.config.admission_root.clone();
        let name = self.record_name.clone();
        let bytes = self.admission.clone();
        tokio::task::spawn_blocking(move || publish_private(&root, &name, uid, &bytes))
            .await
            .map_err(|_| anyhow!("admission_publication_failed"))??;
        self.fence(lease, worker_id, cancellation).await?;
        tracing::info!(task_id=%self.receipt.task_id, attempt=self.receipt.attempt, lease_generation=self.receipt.lease_generation,
            admission_sha256=%self.receipt.admission_sha256, "sealed_admission_published");
        Ok(self.receipt.clone())
    }

    async fn fence<L: BrokerLeaseFence>(
        &self,
        lease: &L,
        worker: &str,
        cancellation: &CancellationToken,
    ) -> Result<()> {
        ensure!(!worker.is_empty(), "admission_worker_invalid");
        let r = &self.receipt;
        let check = async {
            ensure!(
                !lease
                    .cancel_requested(r.task_id, worker, r.lease_generation)
                    .await?,
                "admission_cancelled"
            );
            ensure!(
                lease
                    .renew_lease(
                        r.task_id,
                        worker,
                        r.lease_generation,
                        self.config.lease_seconds
                    )
                    .await?,
                "admission_lease_lost"
            );
            Ok(())
        };
        tokio::select! {
            biased;
            _=cancellation.cancelled()=>Err(anyhow!("admission_publication_cancelled")),
            result=tokio::time::timeout(Duration::from_secs(20),check)=>result
                .map_err(|_|anyhow!("admission_authority_deadline"))?
                .map_err(|_:anyhow::Error|anyhow!("admission_authority_lost")),
        }
    }
}

fn private_root(path: &Path) -> bool {
    use std::path::Component;
    path.is_absolute()
        && path != Path::new("/")
        && path
            .components()
            .all(|p| matches!(p, Component::RootDir | Component::Normal(_)))
}

#[cfg(unix)]
pub(crate) fn publish_private(root: &Path, name: &str, uid: u32, bytes: &[u8]) -> Result<()> {
    use std::{
        ffi::CString,
        fs::{File, OpenOptions},
        io::{Read, Write},
        os::{
            fd::{AsRawFd, FromRawFd},
            unix::{
                ffi::OsStrExt,
                fs::{MetadataExt, OpenOptionsExt},
            },
        },
        path::Component,
    };
    ensure!(
        private_root(root) && unsafe { libc::geteuid() } == uid,
        "admission_private_root_invalid"
    );
    ensure!(
        !bytes.is_empty() && bytes.len() <= 1024 * 1024,
        "admission_private_record_limit"
    );
    let mut directory = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open("/")
        .map_err(|_| anyhow!("admission_private_root_unavailable"))?;
    for part in root.components() {
        let Component::Normal(part) = part else {
            continue;
        };
        let name =
            CString::new(part.as_bytes()).map_err(|_| anyhow!("admission_private_root_invalid"))?;
        let fd = unsafe {
            libc::openat(
                directory.as_raw_fd(),
                name.as_ptr(),
                libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC,
            )
        };
        ensure!(fd >= 0, "admission_private_root_unavailable");
        directory = unsafe { File::from_raw_fd(fd) };
    }
    let metadata = directory
        .metadata()
        .map_err(|_| anyhow!("admission_private_root_unavailable"))?;
    ensure!(
        metadata.uid() == uid && metadata.mode() & 0o777 == 0o700,
        "admission_private_root_mode"
    );
    ensure!(
        !name.contains(['/', '\\', '\0']) && !matches!(name, "" | "." | ".."),
        "admission_private_record_name"
    );
    let destination = CString::new(name).map_err(|_| anyhow!("admission_private_record_name"))?;
    let temporary = CString::new(format!(".{}.partial", Uuid::new_v4()))
        .map_err(|_| anyhow!("admission_private_record_name"))?;
    let fd = unsafe {
        libc::openat(
            directory.as_raw_fd(),
            temporary.as_ptr(),
            libc::O_WRONLY | libc::O_CREAT | libc::O_EXCL | libc::O_NOFOLLOW | libc::O_CLOEXEC,
            0o600,
        )
    };
    ensure!(fd >= 0, "admission_private_record_unavailable");
    let mut file = unsafe { File::from_raw_fd(fd) };
    let result = (|| {
        let metadata = file
            .metadata()
            .map_err(|_| anyhow!("admission_private_record_unavailable"))?;
        ensure!(
            metadata.uid() == uid && metadata.mode() & 0o777 == 0o600 && metadata.nlink() == 1,
            "admission_private_record_mode"
        );
        file.write_all(bytes)
            .map_err(|_| anyhow!("admission_private_write_failed"))?;
        file.sync_all()
            .map_err(|_| anyhow!("admission_private_sync_failed"))?;
        #[cfg(target_os = "linux")]
        let renamed = unsafe {
            libc::renameat2(
                directory.as_raw_fd(),
                temporary.as_ptr(),
                directory.as_raw_fd(),
                destination.as_ptr(),
                libc::RENAME_NOREPLACE,
            )
        };
        #[cfg(target_os = "macos")]
        let renamed = unsafe {
            libc::renameatx_np(
                directory.as_raw_fd(),
                temporary.as_ptr(),
                directory.as_raw_fd(),
                destination.as_ptr(),
                libc::RENAME_EXCL,
            )
        };
        #[cfg(not(any(target_os = "linux", target_os = "macos")))]
        let renamed = -1;
        if renamed != 0 {
            ensure!(
                std::io::Error::last_os_error().kind() == std::io::ErrorKind::AlreadyExists,
                "admission_private_publish_failed"
            );
            let existing = unsafe {
                libc::openat(
                    directory.as_raw_fd(),
                    destination.as_ptr(),
                    libc::O_RDONLY | libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC,
                )
            };
            ensure!(existing >= 0, "admission_existing_record_invalid");
            let existing = unsafe { File::from_raw_fd(existing) };
            let metadata = existing
                .metadata()
                .map_err(|_| anyhow!("admission_existing_record_invalid"))?;
            ensure!(
                metadata.is_file()
                    && metadata.uid() == uid
                    && metadata.mode() & 0o777 == 0o600
                    && metadata.nlink() == 1
                    && metadata.len() == bytes.len() as u64,
                "admission_existing_record_invalid"
            );
            let mut prior = Vec::new();
            existing
                .take(bytes.len() as u64 + 1)
                .read_to_end(&mut prior)
                .map_err(|_| anyhow!("admission_existing_record_invalid"))?;
            ensure!(prior == bytes, "admission_existing_record_conflict");
        }
        Ok(())
    })();
    let removed = unsafe { libc::unlinkat(directory.as_raw_fd(), temporary.as_ptr(), 0) };
    ensure!(
        removed == 0 || std::io::Error::last_os_error().kind() == std::io::ErrorKind::NotFound,
        "admission_private_cleanup_failed"
    );
    directory
        .sync_all()
        .map_err(|_| anyhow!("admission_private_sync_failed"))?;
    result
}
#[cfg(not(unix))]
pub(crate) fn publish_private(_: &Path, _: &str, _: u32, _: &[u8]) -> Result<()> {
    Err(anyhow!("admission_publication_requires_unix"))
}
