//! Server-owned staging handoff. This validates admission; it does not start compute.
use super::{
    model::ClaimedTask,
    sandbox::{CanonicalSandbox, SandboxAuthorization, SandboxBinding, build_sandbox},
};
use crate::custody::{SealAttestation, canonical_document_digest};
use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{fs::File, io::Read, path::Path};
use uuid::Uuid;

const SCHEMA: &str = "sandbox.enrichedlab.org/broker-staged-admission/v1";
const LIMIT: usize = 256 * 1024;

#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct StagedAdmission {
    schema_version: String,
    task_id: Uuid,
    attempt: i32,
    lease_generation: i64,
    principal_id: Uuid,
    group_key: String,
    profile_key: String,
    authorization: SandboxAuthorization,
    sealed_input: SealAttestation,
    sandbox: Value,
}

#[derive(Debug)]
pub struct AdmittedSandbox {
    pub canonical: CanonicalSandbox,
    pub authorization: SandboxAuthorization,
    pub sealed_input: SealAttestation,
}

/// `root`, `writer_uid` and `binding` come from trusted server configuration.
/// The caller must resolve its binding from the authenticated task and fence the
/// lease again before a subsequent runtime mutation. No model path is accepted.
pub fn load(
    root: &Path,
    name: &str,
    writer_uid: u32,
    task: &ClaimedTask,
    binding: &SandboxBinding,
) -> Result<AdmittedSandbox> {
    if name.is_empty() || name == "." || name == ".." || name.contains(['/', '\\', '\0']) {
        bail!("admission_name_invalid")
    }
    let file = open_record(root, name, writer_uid)?;
    let mut bytes = Vec::new();
    file.take((LIMIT + 1) as u64)
        .read_to_end(&mut bytes)
        .context("admission_read_failed")?;
    if bytes.len() > LIMIT {
        bail!("admission_size_exceeded")
    }
    let record: StagedAdmission =
        serde_json::from_slice(&bytes).map_err(|_| anyhow::anyhow!("admission_decode_failed"))?;
    validate(record, task, binding)
}

fn validate(r: StagedAdmission, t: &ClaimedTask, b: &SandboxBinding) -> Result<AdmittedSandbox> {
    if r.schema_version != SCHEMA
        || r.task_id != t.id
        || r.attempt != t.attempt
        || r.lease_generation != t.lease_generation
        || r.principal_id != t.principal_id
        || r.group_key != t.group_key
        || r.profile_key != t.profile_key
    {
        bail!("admission_task_lease_mismatch")
    }
    let s = &r.sealed_input;
    let a = &r.authorization;
    if s.schema_version != "sandbox.enrichedlab.org/input-staging/v1"
        || s.materialization_mode != "checksummed-copy"
        || s.run_id != a.run_id
        || s.namespace != a.namespace
        || s.principal_hash != a.principal_hash
        || s.input_digest != a.input_digest
        || s.input_digest != s.manifest_digest
        || s.manifest_digest != a.manifest_digest
        || s.attestation_digest != a.attestation_digest
        || s.pvc_name != format!("input-{}", a.run_id)
        || r.sandbox
            .pointer("/metadata/labels/sandbox.enrichedlab.org~1tenant")
            .and_then(Value::as_str)
            != Some(s.tenant_id.as_str())
    {
        bail!("admission_sealed_input_mismatch")
    }
    if canonical_document_digest(s)? != a.staging_digest {
        bail!("admission_staging_digest_mismatch")
    }
    let mut unsigned = s.clone();
    unsigned.attestation_digest.clear();
    if canonical_document_digest(&unsigned)? != s.attestation_digest {
        bail!("admission_attestation_digest_mismatch")
    }
    let canonical = CanonicalSandbox::from_renderer(r.sandbox, a.input_digest.clone())?;
    // Reuse the adapter's task, identity, profile and workload binding checks.
    build_sandbox(t, b, &canonical, a).context("admission_renderer_binding_mismatch")?;
    Ok(AdmittedSandbox {
        canonical,
        authorization: r.authorization,
        sealed_input: r.sealed_input,
    })
}

/// Encode only after applying the same closed admission contract used by readers.
pub(crate) fn encode_staged(
    task: &ClaimedTask,
    binding: &SandboxBinding,
    authorization: &SandboxAuthorization,
    sealed_input: &SealAttestation,
    sandbox: Value,
) -> Result<Vec<u8>> {
    let record = StagedAdmission {
        schema_version: SCHEMA.into(),
        task_id: task.id,
        attempt: task.attempt,
        lease_generation: task.lease_generation,
        principal_id: task.principal_id,
        group_key: task.group_key.clone(),
        profile_key: task.profile_key.clone(),
        authorization: authorization.clone(),
        sealed_input: sealed_input.clone(),
        sandbox,
    };
    let bytes =
        serde_json::to_vec(&record).map_err(|_| anyhow::anyhow!("admission_encode_failed"))?;
    if bytes.len() > LIMIT {
        bail!("admission_size_exceeded")
    }
    validate(record, task, binding)?;
    Ok(bytes)
}

pub(crate) fn open_record(root: &Path, name: &str, writer_uid: u32) -> Result<File> {
    open_bounded_record(root, name, writer_uid, LIMIT as u64)
}

#[cfg(unix)]
pub(crate) fn open_bounded_record(
    root: &Path,
    name: &str,
    writer_uid: u32,
    limit: u64,
) -> Result<File> {
    use std::{
        ffi::CString,
        fs::OpenOptions,
        os::{
            fd::{AsRawFd, FromRawFd},
            unix::{
                ffi::OsStrExt,
                fs::{MetadataExt, OpenOptionsExt},
            },
        },
        path::Component,
    };
    if !root.is_absolute() || root == Path::new("/") {
        bail!("admission_root_invalid")
    }
    let mut directory = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open("/")
        .context("admission_root_unavailable")?;
    // Walk from the filesystem root through held descriptors. O_NOFOLLOW applies
    // to every component, so an ancestor symlink cannot redirect the trusted root.
    for part in root.components() {
        let Component::Normal(part) = part else {
            if matches!(part, Component::RootDir) {
                continue;
            }
            bail!("admission_root_invalid")
        };
        let part = CString::new(part.as_bytes()).context("admission_root_invalid")?;
        let fd = unsafe {
            libc::openat(
                directory.as_raw_fd(),
                part.as_ptr(),
                libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC,
            )
        };
        if fd < 0 {
            bail!("admission_root_unavailable")
        }
        directory = unsafe { File::from_raw_fd(fd) };
    }
    let metadata = directory
        .metadata()
        .context("admission_root_metadata_failed")?;
    if metadata.uid() != writer_uid || metadata.mode() & 0o077 != 0 {
        bail!("admission_root_not_private")
    }
    let name = CString::new(name).context("admission_name_invalid")?;
    // NONBLOCK prevents a malicious FIFO from hanging before fstat rejects it.
    let fd = unsafe {
        libc::openat(
            directory.as_raw_fd(),
            name.as_ptr(),
            libc::O_RDONLY | libc::O_NOFOLLOW | libc::O_CLOEXEC | libc::O_NONBLOCK,
        )
    };
    if fd < 0 {
        return Err(std::io::Error::last_os_error()).context("admission_record_unavailable");
    }
    let file = unsafe { File::from_raw_fd(fd) };
    let metadata = file
        .metadata()
        .context("admission_record_metadata_failed")?;
    if !metadata.is_file()
        || metadata.uid() != writer_uid
        || metadata.mode() & 0o077 != 0
        || metadata.nlink() != 1
    {
        bail!("admission_record_not_private_regular")
    }
    if metadata.len() > limit {
        bail!("admission_size_exceeded")
    }
    Ok(file)
}

#[cfg(not(unix))]
pub(crate) fn open_bounded_record(_: &Path, _: &str, _: u32, _: u64) -> Result<File> {
    bail!("admission_requires_unix_descriptors")
}
