//! Trusted, fixed filesystem staging for a direct Sandbox input PVC.
//!
//! This module copies only a server-owned typed request. It does not create a
//! Kubernetes object, alter PVC metadata, run a caller command, or export an
//! artifact. The caller must seal the PVC metadata only after this module
//! returns a matching [`SealAttestation`].

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
#[cfg(unix)]
use std::os::unix::{
    ffi::OsStrExt,
    fs::OpenOptionsExt,
    io::{AsRawFd, FromRawFd},
};
use std::{
    fs::{self, File, OpenOptions},
    io::{Read, Write},
    path::{Component, Path, PathBuf},
};
use time::{OffsetDateTime, format_description::well_known::Rfc3339};
use walkdir::WalkDir;

const SCHEMA: &str = "sandbox.enrichedlab.org/input-staging/v1";
const MODE: &str = "checksummed-copy";
const MAX_FILES: u64 = 100_000;
const MAX_BYTES: u64 = 64 * 1024 * 1024 * 1024;
const MAX_STATE_BYTES: u64 = 1024 * 1024;
const MAX_ADMITTED_FILES: usize = 4;
const MAX_ADMITTED_BYTES: usize = 64 * 1024;

struct TrustedRoots {
    source: TrustedRoot,
    destination: TrustedRoot,
    custody: TrustedRoot,
}

struct TrustedRoot {
    path: PathBuf,
    // Hold each approved root open for the complete staging operation. Child
    // opens use O_NOFOLLOW, so a replacement cannot redirect an individual
    // declared file while it is hashed or copied.
    _directory: File,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CopySealRequest {
    pub schema_version: String,
    pub staging_id: String,
    pub run_id: String,
    pub tenant_id: String,
    pub namespace: String,
    pub principal_hash: String,
    pub input_ref: String,
    pub input_digest: String,
    pub pvc_name: String,
    pub manifest_ref: String,
    pub attestation_ref: String,
    pub source_root: PathBuf,
    pub destination_root: PathBuf,
    pub custody_root: PathBuf,
    pub files: Vec<DeclaredFile>,
    #[serde(default)]
    pub max_files: Option<u64>,
    #[serde(default)]
    pub max_bytes: Option<u64>,
    /// Exact UTF-8 bytes for declared paths absent from the source tree. Only
    /// the trusted input custodian adds these, from the broker's admitted-code
    /// record; the declared size and SHA-256 still bind every byte.
    #[serde(default)]
    pub admitted_files: Vec<AdmittedFile>,
}

#[derive(Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AdmittedFile {
    pub path: String,
    pub content: String,
}
impl std::fmt::Debug for AdmittedFile {
    // The content is model-written code; keep it out of debug output.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AdmittedFile")
            .field("path", &self.path)
            .field("bytes", &self.content.len())
            .finish()
    }
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct DeclaredFile {
    pub path: String,
    pub size_bytes: u64,
    pub sha256: String,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct InputManifest {
    pub schema_version: String,
    pub staging_id: String,
    pub run_id: String,
    pub files: Vec<DeclaredFile>,
    pub total_bytes: u64,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SealAttestation {
    pub schema_version: String,
    pub staging_id: String,
    pub run_id: String,
    pub tenant_id: String,
    pub namespace: String,
    pub principal_hash: String,
    pub materialization_mode: String,
    pub input_ref: String,
    pub input_digest: String,
    pub pvc_name: String,
    pub manifest_ref: String,
    pub manifest_digest: String,
    pub attestation_ref: String,
    pub attestation_digest: String,
    pub sealed_at: String,
}

/// Copy a declared source tree into `destination_root/payload`, then emit a
/// private manifest and typed attestation beneath `custody_root`. Existing
/// incomplete state is rejected. An exact prior attestation is reusable.
pub fn copy_and_seal(request: &CopySealRequest) -> Result<SealAttestation> {
    validate_request(request)?;
    let roots = open_trusted_roots(request)?;
    let attestation_path = roots
        .custody
        .path
        .join("attestations")
        .join(format!("{}.json", request.staging_id));
    if path_exists(&attestation_path)? {
        let existing: SealAttestation = read_json(&attestation_path, MAX_STATE_BYTES)?;
        validate_existing(&existing, request, &roots.custody.path)?;
        let payload_root = open_trusted_root(&roots.destination.path.join("payload"), "payload")?;
        verify_destination(&payload_root, request)?;
        return Ok(existing);
    }

    let payload = roots.destination.path.join("payload");
    let staging_parent = roots.destination.path.join(".custody-staging");
    create_private_dir(&staging_parent)?;
    let staging = staging_parent.join(&request.staging_id);
    if path_exists(&payload)? || path_exists(&staging)? {
        bail!("staging_incomplete_or_unowned");
    }
    create_private_dir(&staging)?;

    let source_manifest = inventory(&roots.source, request)?;
    verify_source(&source_manifest, request)?;
    let sealed_manifest = with_admitted(&source_manifest, request)?;
    let manifest_digest = canonical_document_digest(&sealed_manifest)?;
    if manifest_digest != request.input_digest {
        bail!("source_manifest_digest_mismatch");
    }

    let staged_payload = staging.join("payload");
    create_private_dir(&staged_payload)?;
    let staged_root = open_trusted_root(&staged_payload, "staged_payload")?;
    copy_files(
        &roots.source,
        &staged_root,
        &source_manifest.files,
        request.max_bytes.unwrap_or(MAX_BYTES),
    )?;
    write_admitted(&staged_root, request)?;
    verify_destination(&staged_root, request)?;
    let source_after = inventory(&roots.source, request)?;
    if source_after != source_manifest {
        bail!("source_changed_during_copy");
    }
    sync_tree(&staged_payload)?;

    fs::rename(&staged_payload, &payload).context("destination_publish_failed")?;
    sync_dir(&roots.destination.path)?;
    let manifest_path = roots
        .custody
        .path
        .join("manifests")
        .join(format!("{}.json", request.staging_id));
    atomic_json(&manifest_path, &sealed_manifest)?;

    let sealed_at = OffsetDateTime::now_utc()
        .format(&Rfc3339)
        .context("seal_timestamp_failed")?;
    let mut attestation = SealAttestation {
        schema_version: SCHEMA.into(),
        staging_id: request.staging_id.clone(),
        run_id: request.run_id.clone(),
        tenant_id: request.tenant_id.clone(),
        namespace: request.namespace.clone(),
        principal_hash: request.principal_hash.clone(),
        materialization_mode: MODE.into(),
        input_ref: request.input_ref.clone(),
        input_digest: request.input_digest.clone(),
        pvc_name: request.pvc_name.clone(),
        manifest_ref: request.manifest_ref.clone(),
        manifest_digest,
        attestation_ref: request.attestation_ref.clone(),
        attestation_digest: String::new(),
        sealed_at,
    };
    attestation.attestation_digest = digest_attestation(&attestation)?;
    atomic_json(&attestation_path, &attestation)?;
    sync_dir(&roots.custody.path.join("attestations"))?;
    Ok(attestation)
}

fn validate_request(request: &CopySealRequest) -> Result<()> {
    if request.schema_version != SCHEMA
        || !dns(&request.staging_id)
        || !dns(&request.run_id)
        || !dns(&request.tenant_id)
        || !dns(&request.namespace)
        || request.run_id.len() > 53
        || request.pvc_name != format!("input-{}", request.run_id)
        || !sha(&request.input_digest)
        || !opaque(&request.input_ref, "input")
        || !opaque(&request.manifest_ref, "artifact")
        || !opaque(&request.attestation_ref, "receipt")
        || request.principal_hash.len() != 32
        || !request
            .principal_hash
            .bytes()
            .all(|b| b.is_ascii_hexdigit())
    {
        bail!("invalid_copy_seal_request");
    }
    let max_files = request.max_files.unwrap_or(MAX_FILES);
    let max_bytes = request.max_bytes.unwrap_or(MAX_BYTES);
    if max_files == 0
        || max_files > MAX_FILES
        || max_bytes == 0
        || max_bytes > MAX_BYTES
        || request.files.is_empty()
    {
        bail!("invalid_copy_bounds");
    }
    let mut declared_bytes = 0_u64;
    for file in &request.files {
        validate_relative(&file.path)?;
        if !sha(&file.sha256) {
            bail!("invalid_declared_digest");
        }
        declared_bytes = declared_bytes
            .checked_add(file.size_bytes)
            .context("declared_size_overflow")?;
    }
    if declared_bytes > max_bytes {
        bail!("declared_copy_bounds_exceeded");
    }
    let mut sorted = request
        .files
        .iter()
        .map(|f| f.path.as_str())
        .collect::<Vec<_>>();
    sorted.sort_unstable();
    sorted.dedup();
    if sorted.len() != request.files.len() {
        bail!("duplicate_declared_path");
    }
    validate_admitted_files(request)
}

/// Admitted bytes must match a declared entry exactly before any copy starts.
/// The input custodian runs the same check before it publishes a copy request.
pub fn validate_admitted_files(request: &CopySealRequest) -> Result<()> {
    if request.admitted_files.len() > MAX_ADMITTED_FILES {
        bail!("invalid_admitted_files");
    }
    let mut seen = std::collections::HashSet::new();
    for admitted in &request.admitted_files {
        validate_relative(&admitted.path)?;
        let declared = request
            .files
            .iter()
            .find(|file| file.path == admitted.path)
            .context("admitted_file_undeclared")?;
        if !seen.insert(admitted.path.as_str())
            || admitted.content.is_empty()
            || admitted.content.len() > MAX_ADMITTED_BYTES
            || admitted.content.contains('\0')
            || admitted.content.len() as u64 != declared.size_bytes
            || format!(
                "sha256:{}",
                hex::encode(Sha256::digest(admitted.content.as_bytes()))
            ) != declared.sha256
        {
            bail!("admitted_file_mismatch");
        }
    }
    Ok(())
}

/// The source tree holds every declared file except the admitted ones.
fn verify_source(manifest: &InputManifest, request: &CopySealRequest) -> Result<()> {
    let mut declared = request
        .files
        .iter()
        .filter(|file| {
            !request
                .admitted_files
                .iter()
                .any(|admitted| admitted.path == file.path)
        })
        .cloned()
        .collect::<Vec<_>>();
    declared.sort_by(|a, b| a.path.cmp(&b.path));
    if manifest.files != declared {
        bail!("declared_source_manifest_mismatch");
    }
    Ok(())
}

/// The sealed manifest is the source inventory plus the admitted declarations.
/// Without admitted files it is the source inventory unchanged.
fn with_admitted(source: &InputManifest, request: &CopySealRequest) -> Result<InputManifest> {
    let mut manifest = source.clone();
    for admitted in &request.admitted_files {
        let declared = request
            .files
            .iter()
            .find(|file| file.path == admitted.path)
            .context("admitted_file_undeclared")?;
        if manifest.files.iter().any(|file| file.path == declared.path) {
            bail!("admitted_file_shadows_source");
        }
        manifest.total_bytes = manifest
            .total_bytes
            .checked_add(declared.size_bytes)
            .context("declared_size_overflow")?;
        manifest.files.push(declared.clone());
    }
    manifest.files.sort_by(|a, b| a.path.cmp(&b.path));
    Ok(manifest)
}

fn write_admitted(destination: &TrustedRoot, request: &CopySealRequest) -> Result<()> {
    for admitted in &request.admitted_files {
        let relative = validated_relative(&admitted.path)?;
        let mut writer =
            create_relative_private_file(destination, &relative, "admitted_file_create_failed")?;
        writer
            .write_all(admitted.content.as_bytes())
            .context("admitted_file_write_failed")?;
        writer.sync_all().context("destination_sync_failed")?;
    }
    Ok(())
}

fn inventory(root: &TrustedRoot, request: &CopySealRequest) -> Result<InputManifest> {
    let max_files = request.max_files.unwrap_or(MAX_FILES);
    let max_bytes = request.max_bytes.unwrap_or(MAX_BYTES);
    let mut files = Vec::new();
    let mut total = 0_u64;
    for entry in WalkDir::new(&root.path)
        .follow_links(false)
        .sort_by_file_name()
    {
        let entry = entry.context("source_walk_failed")?;
        if entry.path() == root.path {
            continue;
        }
        let metadata = fs::symlink_metadata(entry.path()).context("source_metadata_failed")?;
        if metadata.file_type().is_symlink() || !(metadata.is_dir() || metadata.is_file()) {
            bail!("source_has_symlink_or_special_file");
        }
        if metadata.is_file() {
            let rel = entry
                .path()
                .strip_prefix(&root.path)
                .context("source_path_escape")?;
            let path = relative_string(rel)?;
            let remaining = max_bytes
                .checked_sub(total)
                .context("source_copy_bounds_exceeded")?;
            total = total
                .checked_add(metadata.len())
                .context("source_size_overflow")?;
            if files.len() as u64 >= max_files || total > max_bytes {
                bail!("source_copy_bounds_exceeded");
            }
            let relative = validated_relative(&path)?;
            let mut file = open_relative_read(root, &relative, "source_open_failed")?;
            files.push(DeclaredFile {
                path,
                size_bytes: metadata.len(),
                sha256: hash_file(&mut file, metadata.len(), remaining)?,
            });
        }
    }
    files.sort_by(|a, b| a.path.cmp(&b.path));
    Ok(InputManifest {
        schema_version: SCHEMA.into(),
        staging_id: request.staging_id.clone(),
        run_id: request.run_id.clone(),
        files,
        total_bytes: total,
    })
}

fn verify_declared(manifest: &InputManifest, request: &CopySealRequest) -> Result<()> {
    let mut declared = request.files.clone();
    declared.sort_by(|a, b| a.path.cmp(&b.path));
    if manifest.files != declared {
        bail!("declared_source_manifest_mismatch");
    }
    Ok(())
}

fn copy_files(
    source: &TrustedRoot,
    destination: &TrustedRoot,
    files: &[DeclaredFile],
    max_bytes: u64,
) -> Result<()> {
    let mut copied = 0_u64;
    for file in files {
        let relative = validated_relative(&file.path)?;
        let mut reader = open_relative_read(source, &relative, "source_open_failed")?;
        let source_meta = reader.metadata().context("source_metadata_failed")?;
        if !source_meta.is_file() || source_meta.len() != file.size_bytes {
            bail!("source_changed_before_copy");
        }
        let remaining = max_bytes
            .checked_sub(copied)
            .context("copy_read_bounds_exceeded")?;
        let mut writer =
            create_relative_private_file(destination, &relative, "destination_create_failed")?;
        let digest = copy_file(&mut reader, &mut writer, file.size_bytes, remaining)?;
        if digest != file.sha256 {
            bail!("source_changed_during_copy");
        }
        writer.sync_all().context("destination_sync_failed")?;
        copied = copied
            .checked_add(file.size_bytes)
            .context("copy_size_overflow")?;
    }
    Ok(())
}

fn verify_destination(root: &TrustedRoot, request: &CopySealRequest) -> Result<()> {
    let observed = inventory(root, request)?;
    verify_declared(&observed, request)?;
    let digest = canonical_document_digest(&observed)?;
    if digest != request.input_digest {
        bail!("destination_manifest_digest_mismatch");
    }
    Ok(())
}

fn validate_existing(
    attestation: &SealAttestation,
    request: &CopySealRequest,
    custody_root: &Path,
) -> Result<()> {
    if attestation.schema_version != SCHEMA
        || attestation.materialization_mode != MODE
        || attestation.staging_id != request.staging_id
        || attestation.run_id != request.run_id
        || attestation.tenant_id != request.tenant_id
        || attestation.namespace != request.namespace
        || attestation.principal_hash != request.principal_hash
        || attestation.input_ref != request.input_ref
        || attestation.input_digest != request.input_digest
        || attestation.pvc_name != request.pvc_name
        || attestation.manifest_ref != request.manifest_ref
        || attestation.attestation_ref != request.attestation_ref
        || digest_attestation(attestation)? != attestation.attestation_digest
    {
        bail!("existing_seal_mismatch");
    }
    let manifest_path = custody_root
        .join("manifests")
        .join(format!("{}.json", request.staging_id));
    let manifest: InputManifest = read_json(&manifest_path, MAX_STATE_BYTES)?;
    verify_declared(&manifest, request)?;
    if manifest.schema_version != SCHEMA
        || manifest.staging_id != request.staging_id
        || manifest.run_id != request.run_id
        || manifest.total_bytes
            != request
                .files
                .iter()
                .map(|file| file.size_bytes)
                .sum::<u64>()
        || canonical_document_digest(&manifest)? != attestation.manifest_digest
        || attestation.manifest_digest != request.input_digest
    {
        bail!("existing_manifest_mismatch");
    }
    Ok(())
}

fn digest_attestation(value: &SealAttestation) -> Result<String> {
    let mut copy = value.clone();
    copy.attestation_digest.clear();
    canonical_document_digest(&copy)
}
pub fn canonical_document_digest<T: Serialize>(value: &T) -> Result<String> {
    let value = canonicalize_json(serde_json::to_value(value).context("json_encode_failed")?);
    Ok(format!(
        "sha256:{}",
        hex::encode(Sha256::digest(
            serde_json::to_vec(&value).context("json_encode_failed")?
        ))
    ))
}

fn canonicalize_json(value: serde_json::Value) -> serde_json::Value {
    match value {
        serde_json::Value::Object(map) => {
            let mut keys = map.into_iter().collect::<Vec<_>>();
            keys.sort_unstable_by(|left, right| left.0.cmp(&right.0));
            serde_json::Value::Object(
                keys.into_iter()
                    .map(|(key, value)| (key, canonicalize_json(value)))
                    .collect(),
            )
        }
        serde_json::Value::Array(values) => {
            serde_json::Value::Array(values.into_iter().map(canonicalize_json).collect())
        }
        other => other,
    }
}

fn hash_file(file: &mut File, expected_size: u64, max_bytes: u64) -> Result<String> {
    let mut hasher = Sha256::new();
    let mut total = 0_u64;
    let mut buf = [0_u8; 64 * 1024];
    loop {
        let n = file.read(&mut buf).context("source_read_failed")?;
        if n == 0 {
            break;
        };
        total = total
            .checked_add(n as u64)
            .context("source_size_overflow")?;
        if total > expected_size || total > max_bytes {
            bail!("source_read_bounds_exceeded");
        }
        hasher.update(&buf[..n]);
    }
    if total != expected_size {
        bail!("source_size_changed_during_read");
    }
    Ok(format!("sha256:{}", hex::encode(hasher.finalize())))
}

fn copy_file(
    reader: &mut File,
    writer: &mut File,
    expected_size: u64,
    max_bytes: u64,
) -> Result<String> {
    let mut hasher = Sha256::new();
    let mut total = 0_u64;
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        let n = reader.read(&mut buffer).context("source_read_failed")?;
        if n == 0 {
            break;
        }
        total = total.checked_add(n as u64).context("copy_size_overflow")?;
        if total > expected_size || total > max_bytes {
            bail!("copy_read_bounds_exceeded");
        }
        writer
            .write_all(&buffer[..n])
            .context("destination_write_failed")?;
        hasher.update(&buffer[..n]);
    }
    if total != expected_size {
        bail!("source_size_changed_during_copy");
    }
    Ok(format!("sha256:{}", hex::encode(hasher.finalize())))
}

fn open_directory_no_follow(path: &Path, context: &str) -> Result<File> {
    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    options.custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC);
    let file = options.open(path).with_context(|| context.to_owned())?;
    if !file
        .metadata()
        .context("directory_metadata_failed")?
        .is_dir()
    {
        bail!("invalid_directory_root");
    }
    Ok(file)
}

fn open_regular_read(path: &Path, context: &str) -> Result<File> {
    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    options.custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC);
    let file = options.open(path).with_context(|| context.to_owned())?;
    if !file.metadata().context("source_metadata_failed")?.is_file() {
        bail!("source_has_symlink_or_special_file");
    }
    Ok(file)
}

fn open_relative_read(root: &TrustedRoot, relative: &Path, context: &str) -> Result<File> {
    let parent = open_relative_directory(root, relative.parent().unwrap_or(Path::new("")), false)?;
    let name = relative.file_name().context("relative_file_name_missing")?;
    let file = openat_file(&parent, name, libc::O_RDONLY, 0, context)?;
    if !file.metadata().context("source_metadata_failed")?.is_file() {
        bail!("source_has_symlink_or_special_file");
    }
    Ok(file)
}

fn create_relative_private_file(
    root: &TrustedRoot,
    relative: &Path,
    context: &str,
) -> Result<File> {
    let parent = open_relative_directory(root, relative.parent().unwrap_or(Path::new("")), true)?;
    let name = relative.file_name().context("relative_file_name_missing")?;
    openat_file(
        &parent,
        name,
        libc::O_WRONLY | libc::O_CREAT | libc::O_EXCL,
        0o600,
        context,
    )
}

fn open_relative_directory(root: &TrustedRoot, relative: &Path, create: bool) -> Result<File> {
    let mut directory = root
        ._directory
        .try_clone()
        .context("trusted_root_clone_failed")?;
    for component in relative.components() {
        let Component::Normal(name) = component else {
            bail!("invalid_relative_path");
        };
        match openat_file(
            &directory,
            name,
            libc::O_RDONLY | libc::O_DIRECTORY,
            0,
            "relative_directory_open_failed",
        ) {
            Ok(next) => directory = next,
            Err(error) if create && is_not_found(&error) => {
                mkdirat_private(&directory, name)?;
                directory = openat_file(
                    &directory,
                    name,
                    libc::O_RDONLY | libc::O_DIRECTORY,
                    0,
                    "relative_directory_open_failed",
                )?;
            }
            Err(error) => return Err(error),
        }
    }
    Ok(directory)
}

fn is_not_found(error: &anyhow::Error) -> bool {
    error
        .downcast_ref::<std::io::Error>()
        .is_some_and(|cause| cause.kind() == std::io::ErrorKind::NotFound)
}

fn openat_file(
    parent: &File,
    name: &std::ffi::OsStr,
    flags: i32,
    mode: u32,
    context: &str,
) -> Result<File> {
    #[cfg(unix)]
    {
        let name = std::ffi::CString::new(name.as_bytes()).context("relative_name_invalid")?;
        let descriptor = unsafe {
            libc::openat(
                parent.as_raw_fd(),
                name.as_ptr(),
                flags | libc::O_CLOEXEC | libc::O_NOFOLLOW,
                mode,
            )
        };
        if descriptor < 0 {
            return Err(std::io::Error::last_os_error()).with_context(|| context.to_owned());
        }
        Ok(unsafe { File::from_raw_fd(descriptor) })
    }
    #[cfg(not(unix))]
    {
        let _ = (parent, name, flags, mode, context);
        bail!("secure_copy_requires_unix");
    }
}

fn mkdirat_private(parent: &File, name: &std::ffi::OsStr) -> Result<()> {
    #[cfg(unix)]
    {
        let name = std::ffi::CString::new(name.as_bytes()).context("relative_name_invalid")?;
        let result = unsafe { libc::mkdirat(parent.as_raw_fd(), name.as_ptr(), 0o700) };
        if result != 0 {
            return Err(std::io::Error::last_os_error()).context("destination_directory_failed");
        }
        Ok(())
    }
    #[cfg(not(unix))]
    {
        let _ = (parent, name);
        bail!("secure_copy_requires_unix");
    }
}

fn read_bounded(file: &mut File, max_bytes: u64, error: &str) -> Result<Vec<u8>> {
    let mut bytes = Vec::new();
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        let count = file.read(&mut buffer).context("state_read_failed")?;
        if count == 0 {
            break;
        }
        let next = (bytes.len() as u64)
            .checked_add(count as u64)
            .context("state_size_overflow")?;
        if next > max_bytes {
            bail!("{}", error);
        }
        bytes.extend_from_slice(&buffer[..count]);
    }
    Ok(bytes)
}

fn path_exists(path: &Path) -> Result<bool> {
    match fs::symlink_metadata(path) {
        Ok(_) => Ok(true),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(error).context("path_metadata_failed"),
    }
}
fn open_trusted_roots(request: &CopySealRequest) -> Result<TrustedRoots> {
    let source = open_trusted_root(&request.source_root, "source_root")?;
    let destination = open_trusted_root(&request.destination_root, "destination_root")?;
    let custody = open_trusted_root(&request.custody_root, "custody_root")?;
    if source.path == destination.path
        || source.path == custody.path
        || destination.path == custody.path
    {
        bail!("copy_roots_must_be_distinct");
    }
    Ok(TrustedRoots {
        source,
        destination,
        custody,
    })
}

fn open_trusted_root(path: &Path, kind: &str) -> Result<TrustedRoot> {
    if !path.is_absolute() {
        bail!("trusted_root_must_be_absolute");
    }
    let canonical = fs::canonicalize(path).context("trusted_root_canonicalize_failed")?;
    if canonical != path {
        bail!("trusted_root_contains_symlink");
    }
    ensure_directory(&canonical, kind)?;
    let directory = open_directory_no_follow(&canonical, "trusted_root_open_failed")?;
    Ok(TrustedRoot {
        path: canonical,
        _directory: directory,
    })
}

fn ensure_directory(path: &Path, _kind: &str) -> Result<()> {
    let m = fs::symlink_metadata(path).context("directory_metadata_failed")?;
    if !m.is_dir() || m.file_type().is_symlink() {
        bail!("invalid_directory_root");
    }
    Ok(())
}
fn create_private_dir(path: &Path) -> Result<()> {
    match fs::symlink_metadata(path) {
        Ok(metadata) => {
            if !metadata.is_dir() || metadata.file_type().is_symlink() {
                bail!("staging_directory_not_private");
            }
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            let parent = path.parent().context("staging_parent_missing")?;
            ensure_directory(parent, "staging_parent")?;
            fs::create_dir(path).context("staging_directory_failed")?;
        }
        Err(error) => return Err(error).context("staging_directory_metadata_failed"),
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o700))
            .context("staging_permissions_failed")?;
    }
    Ok(())
}
fn sync_tree(root: &Path) -> Result<()> {
    for e in WalkDir::new(root).follow_links(false) {
        let e = e.context("staging_walk_failed")?;
        if e.file_type().is_file() {
            open_regular_read(e.path(), "staging_open_failed")?
                .sync_all()
                .context("staging_sync_failed")?;
        }
    }
    sync_dir(root)
}
fn sync_dir(path: &Path) -> Result<()> {
    File::open(path)
        .context("directory_open_failed")?
        .sync_all()
        .context("directory_sync_failed")
}
fn atomic_json<T: Serialize>(path: &Path, value: &T) -> Result<()> {
    let parent = path.parent().context("state_parent_missing")?;
    create_private_dir(parent)?;
    let tmp = parent.join(format!(
        ".{}.tmp",
        path.file_name()
            .and_then(|x| x.to_str())
            .context("state_name_invalid")?
    ));
    if tmp.exists() {
        bail!("state_temp_exists")
    };
    let mut f = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&tmp)
        .context("state_create_failed")?;
    f.write_all(
        &serde_json::to_vec(&canonicalize_json(
            serde_json::to_value(value).context("state_encode_failed")?,
        ))
        .context("state_encode_failed")?,
    )
    .context("state_write_failed")?;
    f.write_all(b"\n").context("state_write_failed")?;
    f.sync_all().context("state_sync_failed")?;
    fs::rename(tmp, path).context("state_publish_failed")?;
    sync_dir(parent)
}
fn read_json<T: for<'a> Deserialize<'a>>(path: &Path, max_bytes: u64) -> Result<T> {
    let mut file = open_regular_read(path, "state_open_failed")?;
    let bytes = read_bounded(&mut file, max_bytes, "state_read_bounds_exceeded")?;
    serde_json::from_slice(&bytes).context("state_decode_failed")
}
fn relative_string(path: &Path) -> Result<String> {
    let text = path.to_str().context("non_utf8_source_path")?;
    validate_relative(text)?;
    Ok(text.to_owned())
}
fn validated_relative(value: &str) -> Result<PathBuf> {
    validate_relative(value)?;
    Ok(PathBuf::from(value))
}
fn validate_relative(value: &str) -> Result<()> {
    let path = Path::new(value);
    if value.is_empty()
        || path.is_absolute()
        || path
            .components()
            .any(|c| !matches!(c, Component::Normal(_)))
    {
        bail!("invalid_relative_path")
    }
    Ok(())
}
fn dns(v: &str) -> bool {
    !v.is_empty()
        && v.len() <= 63
        && v.bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
        && !v.starts_with('-')
        && !v.ends_with('-')
}
fn sha(v: &str) -> bool {
    v.len() == 71
        && v.starts_with("sha256:")
        && v[7..]
            .bytes()
            .all(|b| b.is_ascii_hexdigit() && (!b.is_ascii_uppercase()))
}
fn opaque(v: &str, scheme: &str) -> bool {
    v.starts_with(&format!("{scheme}://"))
        && !v.contains('?')
        && !v.contains('#')
        && !v.contains('@')
        && v.len() <= 460
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;
    fn digest(bytes: &[u8]) -> String {
        format!("sha256:{}", hex::encode(Sha256::digest(bytes)))
    }
    fn request(temp: &TempDir, bytes: &[u8]) -> CopySealRequest {
        let source = temp.path().join("source");
        let destination = temp.path().join("destination");
        let custody = temp.path().join("custody");
        fs::create_dir_all(source.join("nested")).unwrap();
        fs::create_dir_all(&destination).unwrap();
        fs::create_dir_all(&custody).unwrap();
        fs::write(source.join("nested/input.bin"), bytes).unwrap();
        CopySealRequest {
            schema_version: SCHEMA.into(),
            staging_id: "stage-1".into(),
            run_id: "run-1".into(),
            tenant_id: "tenant-1".into(),
            namespace: "sandbox-tenant-1".into(),
            principal_hash: "a".repeat(32),
            input_ref: "input://synthetic/source".into(),
            input_digest: String::new(),
            pvc_name: "input-run-1".into(),
            manifest_ref: "artifact://synthetic/manifest".into(),
            attestation_ref: "receipt://synthetic/attestation".into(),
            source_root: fs::canonicalize(source).unwrap(),
            destination_root: fs::canonicalize(destination).unwrap(),
            custody_root: fs::canonicalize(custody).unwrap(),
            files: vec![DeclaredFile {
                path: "nested/input.bin".into(),
                size_bytes: bytes.len() as u64,
                sha256: digest(bytes),
            }],
            max_files: None,
            max_bytes: None,
            admitted_files: vec![],
        }
    }
    const BLOCK: &str = "analyze <- function(data) list()\n";
    fn with_block(temp: &TempDir) -> CopySealRequest {
        let mut request = request(temp, b"synthetic");
        request.files.push(DeclaredFile {
            path: "generated/block.R".into(),
            size_bytes: BLOCK.len() as u64,
            sha256: digest(BLOCK.as_bytes()),
        });
        request.admitted_files.push(AdmittedFile {
            path: "generated/block.R".into(),
            content: BLOCK.into(),
        });
        let source = open_trusted_root(&request.source_root, "source_root").unwrap();
        let manifest = with_admitted(&inventory(&source, &request).unwrap(), &request).unwrap();
        request.input_digest = canonical_document_digest(&manifest).unwrap();
        request
    }
    #[test]
    fn seals_admitted_code_with_the_source_and_reuses_it() {
        let temp = TempDir::new().unwrap();
        let request = with_block(&temp);
        let seal = copy_and_seal(&request).unwrap();
        let payload = request.destination_root.join("payload");
        assert_eq!(
            fs::read(payload.join("generated/block.R")).unwrap(),
            BLOCK.as_bytes()
        );
        assert_eq!(
            fs::read(payload.join("nested/input.bin")).unwrap(),
            b"synthetic"
        );
        let manifest: InputManifest = read_json(
            &request.custody_root.join("manifests/stage-1.json"),
            MAX_STATE_BYTES,
        )
        .unwrap();
        assert_eq!(manifest.files.len(), 2);
        assert_eq!(
            manifest.total_bytes,
            (BLOCK.len() + "synthetic".len()) as u64
        );
        assert_eq!(seal, copy_and_seal(&request).unwrap());
    }
    #[test]
    fn rejects_admitted_bytes_that_differ_from_the_declaration() {
        let temp = TempDir::new().unwrap();
        let mut request = with_block(&temp);
        request.admitted_files[0].content = "analyze <- function(data) list(1)\n".into();
        assert_eq!(
            copy_and_seal(&request).unwrap_err().to_string(),
            "admitted_file_mismatch"
        );
        assert!(!request.destination_root.join("payload").exists());
    }
    #[test]
    fn rejects_undeclared_admitted_paths() {
        let temp = TempDir::new().unwrap();
        let mut request = with_block(&temp);
        request.admitted_files[0].path = "generated/other.R".into();
        assert_eq!(
            copy_and_seal(&request).unwrap_err().to_string(),
            "admitted_file_undeclared"
        );
    }
    #[test]
    fn rejects_an_admitted_path_that_exists_in_the_source() {
        let temp = TempDir::new().unwrap();
        let request = with_block(&temp);
        fs::create_dir_all(request.source_root.join("generated")).unwrap();
        fs::write(request.source_root.join("generated/block.R"), BLOCK).unwrap();
        assert_eq!(
            copy_and_seal(&request).unwrap_err().to_string(),
            "declared_source_manifest_mismatch"
        );
    }
    fn ready(temp: &TempDir, bytes: &[u8]) -> CopySealRequest {
        let mut request = request(temp, bytes);
        let source = open_trusted_root(&request.source_root, "source_root").unwrap();
        let manifest = inventory(&source, &request).unwrap();
        request.input_digest = canonical_document_digest(&manifest).unwrap();
        request
    }
    #[test]
    fn copies_reverifies_and_reuses_exact_seal() {
        let temp = TempDir::new().unwrap();
        let request = ready(&temp, b"synthetic");
        let seal = copy_and_seal(&request).unwrap();
        assert_eq!(seal.input_digest, request.input_digest);
        assert_eq!(
            fs::read(request.destination_root.join("payload/nested/input.bin")).unwrap(),
            b"synthetic"
        );
        assert_eq!(seal, copy_and_seal(&request).unwrap());
    }
    #[test]
    fn rejects_tampered_declared_source() {
        let temp = TempDir::new().unwrap();
        let request = ready(&temp, b"synthetic");
        fs::write(request.source_root.join("nested/input.bin"), b"changed").unwrap();
        assert_eq!(
            copy_and_seal(&request).unwrap_err().to_string(),
            "declared_source_manifest_mismatch"
        );
    }
    #[test]
    fn rejects_partial_destination_without_attestation() {
        let temp = TempDir::new().unwrap();
        let request = ready(&temp, b"synthetic");
        fs::create_dir_all(request.destination_root.join("payload")).unwrap();
        assert_eq!(
            copy_and_seal(&request).unwrap_err().to_string(),
            "staging_incomplete_or_unowned"
        );
    }

    #[test]
    fn rejects_source_growth_while_the_declared_file_is_read() {
        let temp = TempDir::new().unwrap();
        let request = ready(&temp, b"synthetic");
        let path = request.source_root.join("nested/input.bin");
        fs::write(&path, b"synthetic-growth").unwrap();
        let mut file = open_regular_read(&path, "source_open_failed").unwrap();
        assert_eq!(
            hash_file(
                &mut file,
                b"synthetic".len() as u64,
                b"synthetic".len() as u64
            )
            .unwrap_err()
            .to_string(),
            "source_read_bounds_exceeded"
        );
    }

    #[test]
    fn rejects_reuse_when_the_retained_manifest_changes() {
        let temp = TempDir::new().unwrap();
        let request = ready(&temp, b"synthetic");
        copy_and_seal(&request).unwrap();
        let path = request.custody_root.join("manifests").join("stage-1.json");
        let mut manifest: serde_json::Value =
            serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        manifest["totalBytes"] = serde_json::json!(0);
        fs::write(path, serde_json::to_vec(&manifest).unwrap()).unwrap();
        assert_eq!(
            copy_and_seal(&request).unwrap_err().to_string(),
            "existing_manifest_mismatch"
        );
    }

    #[cfg(unix)]
    #[test]
    fn rejects_a_root_with_a_symlinked_parent() {
        let temp = TempDir::new().unwrap();
        let mut request = ready(&temp, b"synthetic");
        let alias = temp.path().join("alias");
        std::os::unix::fs::symlink(&request.custody_root, &alias).unwrap();
        request.custody_root = alias;
        assert_eq!(
            copy_and_seal(&request).unwrap_err().to_string(),
            "trusted_root_contains_symlink"
        );
    }
}
