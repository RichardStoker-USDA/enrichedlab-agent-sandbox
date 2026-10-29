//! Descriptor-safe local staging for one issued export client identity.
//!
//! The caller is a trusted broker-side publisher. This module never accepts a
//! task-selected path and never exposes credential material through its result.

use crate::broker::{
    export_identity::IssuedExportCredentials,
    export_transport::{ExportScope, RemoteExportConfig},
};
use anyhow::{Context, Result, bail, ensure};
use std::{
    ffi::{CStr, CString},
    fs::{File, OpenOptions},
    io::{Read, Write},
    path::{Component, Path},
};
use time::OffsetDateTime;
use uuid::Uuid;

const MAX_CREDENTIAL_BYTES: usize = 64 * 1024;

/// Public evidence that the complete client bundle is available at the trusted
/// scope-derived location. It has no private key or certificate bytes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ClientCredentialReceipt {
    pub scope: ExportScope,
    pub client_fingerprint_sha256: String,
    pub expires_at: OffsetDateTime,
}

/// Whether the atomically published per-run directory exists. Only ENOENT for
/// that exact child of a verified private root is pending. The existing client
/// constructor still validates every credential file and certificate afterward.
// Isolated source-inclusion tests exercise staging without the terminal-export
// caller that consumes this production helper.
#[cfg_attr(test, allow(dead_code))]
pub fn export_client_scope_exists(
    remote: &RemoteExportConfig,
    scope: &ExportScope,
) -> Result<bool> {
    remote.validate()?;
    ExportScope::new(scope.task_id, scope.attempt, scope.lease_generation)?;
    #[cfg(unix)]
    {
        let root = open_private_root(&remote.credential_root, remote.writer_uid)?;
        let name =
            CString::new(scope.credential_name()).context("export_client_staging_name_invalid")?;
        match open_directory_at(&root, &name) {
            Ok(directory) => {
                verify_directory(&directory, remote.writer_uid)?;
                Ok(true)
            }
            Err(error)
                if error
                    .downcast_ref::<std::io::Error>()
                    .is_some_and(|e| e.kind() == std::io::ErrorKind::NotFound) =>
            {
                Ok(false)
            }
            Err(_) => bail!("export_client_staging_scope_invalid"),
        }
    }
    #[cfg(not(unix))]
    {
        bail!("export_client_staging_requires_unix")
    }
}

/// Stages exactly `ca.pem` and `client.pem` beneath the operator-owned root.
/// Existing bundles are accepted only after exact descriptor-held verification.
pub fn stage_export_client(
    remote: &RemoteExportConfig,
    credentials: &IssuedExportCredentials,
) -> Result<ClientCredentialReceipt> {
    remote.validate()?;
    #[cfg(unix)]
    {
        let writer_uid = remote.writer_uid;
        ensure!(
            unsafe { libc::geteuid() } == writer_uid,
            "export_client_staging_writer_uid_mismatch"
        );
        let metadata = credentials.metadata();
        let scope = ExportScope::new(
            metadata.scope.task_id,
            metadata.scope.attempt,
            metadata.scope.lease_generation,
        )?;
        let name =
            CString::new(scope.credential_name()).context("export_client_staging_name_invalid")?;
        let root = open_private_root(&remote.credential_root, writer_uid)?;
        let client = credentials.client_files();
        validate_bundle_bytes(client.ca_pem, client.client_pem)?;

        let temporary = CString::new(format!(
            ".{}.partial.{}",
            scope.credential_name(),
            Uuid::new_v4()
        ))
        .context("export_client_staging_name_invalid")?;
        let partial = PartialDirectory::new(
            root.try_clone()
                .context("export_client_staging_root_clone_failed")?,
            temporary,
        );
        mkdir_private_at(&root, &partial.name)?;
        let directory = open_directory_at(&root, &partial.name)?;
        verify_directory(&directory, writer_uid)?;
        write_private_file(&directory, "ca.pem", client.ca_pem, writer_uid)?;
        write_private_file(&directory, "client.pem", client.client_pem, writer_uid)?;
        directory
            .sync_all()
            .context("export_client_staging_directory_sync_failed")?;
        drop(directory);

        match publish_no_replace(&root, &partial.name, &name)? {
            Publish::Created => {
                partial.publish();
                root.sync_all()
                    .context("export_client_staging_root_sync_failed")?;
            }
            Publish::Exists => {
                let existing = open_directory_at(&root, &name)
                    .map_err(|_| anyhow::anyhow!("export_client_staging_existing_invalid"))?;
                verify_bundle(&existing, writer_uid, client.ca_pem, client.client_pem)?;
                root.sync_all()
                    .context("export_client_staging_root_sync_failed")?;
            }
        }
        Ok(receipt(metadata))
    }
    #[cfg(not(unix))]
    {
        let _ = (remote, credentials);
        bail!("export_client_staging_requires_unix")
    }
}

fn receipt(
    metadata: &crate::broker::export_identity::IssuedExportMetadata,
) -> ClientCredentialReceipt {
    ClientCredentialReceipt {
        scope: metadata.scope.clone(),
        client_fingerprint_sha256: metadata.client_fingerprint_sha256.clone(),
        expires_at: metadata.expires_at,
    }
}

fn validate_bundle_bytes(ca_pem: &[u8], client_pem: &[u8]) -> Result<()> {
    ensure!(
        !ca_pem.is_empty()
            && !client_pem.is_empty()
            && ca_pem.len() <= MAX_CREDENTIAL_BYTES
            && client_pem.len() <= MAX_CREDENTIAL_BYTES,
        "export_client_staging_bundle_invalid"
    );
    Ok(())
}

#[cfg(unix)]
fn open_private_root(path: &Path, writer_uid: u32) -> Result<File> {
    ensure!(
        path.is_absolute()
            && path != Path::new("/")
            && path
                .components()
                .all(|part| matches!(part, Component::RootDir | Component::Normal(_))),
        "export_client_staging_root_invalid"
    );
    let mut directory = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open("/")
        .context("export_client_staging_root_unavailable")?;
    for part in path.components() {
        let Component::Normal(part) = part else {
            if matches!(part, Component::RootDir) {
                continue;
            }
            bail!("export_client_staging_root_invalid")
        };
        let part = CString::new(part.as_bytes()).context("export_client_staging_root_invalid")?;
        directory = open_directory_at(&directory, &part)
            .map_err(|_| anyhow::anyhow!("export_client_staging_root_unavailable"))?;
    }
    verify_directory(&directory, writer_uid)?;
    Ok(directory)
}

#[cfg(unix)]
fn open_directory_at(parent: &File, name: &CString) -> Result<File> {
    let descriptor = unsafe {
        libc::openat(
            parent.as_raw_fd(),
            name.as_ptr(),
            libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC,
        )
    };
    if descriptor < 0 {
        return Err(std::io::Error::last_os_error())
            .context("export_client_staging_directory_open_failed");
    }
    Ok(unsafe { File::from_raw_fd(descriptor) })
}

#[cfg(unix)]
fn verify_directory(directory: &File, writer_uid: u32) -> Result<()> {
    let metadata = directory
        .metadata()
        .context("export_client_staging_directory_metadata_failed")?;
    ensure!(
        metadata.is_dir() && metadata.uid() == writer_uid && metadata.mode() & 0o777 == 0o700,
        "export_client_staging_directory_not_private"
    );
    Ok(())
}

#[cfg(unix)]
fn mkdir_private_at(parent: &File, name: &CString) -> Result<()> {
    let result = unsafe { libc::mkdirat(parent.as_raw_fd(), name.as_ptr(), 0o700) };
    if result < 0 {
        return Err(std::io::Error::last_os_error())
            .context("export_client_staging_directory_create_failed");
    }
    let directory = open_directory_at(parent, name)?;
    if unsafe { libc::fchmod(directory.as_raw_fd(), 0o700) } < 0 {
        bail!("export_client_staging_directory_mode_failed")
    }
    directory
        .sync_all()
        .context("export_client_staging_directory_sync_failed")?;
    parent
        .sync_all()
        .context("export_client_staging_root_sync_failed")?;
    Ok(())
}

#[cfg(unix)]
fn write_private_file(parent: &File, name: &str, bytes: &[u8], writer_uid: u32) -> Result<()> {
    let name = CString::new(name).context("export_client_staging_name_invalid")?;
    let descriptor = unsafe {
        libc::openat(
            parent.as_raw_fd(),
            name.as_ptr(),
            libc::O_WRONLY | libc::O_CREAT | libc::O_EXCL | libc::O_NOFOLLOW | libc::O_CLOEXEC,
            0o600,
        )
    };
    if descriptor < 0 {
        return Err(std::io::Error::last_os_error())
            .context("export_client_staging_file_create_failed");
    }
    let mut file = unsafe { File::from_raw_fd(descriptor) };
    if unsafe { libc::fchmod(file.as_raw_fd(), 0o600) } < 0 {
        bail!("export_client_staging_file_mode_failed")
    }
    file.write_all(bytes)
        .context("export_client_staging_file_write_failed")?;
    file.sync_all()
        .context("export_client_staging_file_sync_failed")?;
    verify_file(&file, writer_uid, bytes)?;
    Ok(())
}

#[cfg(unix)]
fn verify_bundle(
    directory: &File,
    writer_uid: u32,
    ca_pem: &[u8],
    client_pem: &[u8],
) -> Result<()> {
    verify_directory(directory, writer_uid)?;
    verify_directory_entries(directory)?;
    verify_existing_file(directory, "ca.pem", writer_uid, ca_pem)?;
    verify_existing_file(directory, "client.pem", writer_uid, client_pem)
}

#[cfg(unix)]
fn verify_directory_entries(directory: &File) -> Result<()> {
    let copy = directory
        .try_clone()
        .context("export_client_staging_directory_clone_failed")?;
    let descriptor = copy.into_raw_fd();
    let stream = unsafe { libc::fdopendir(descriptor) };
    if stream.is_null() {
        drop(unsafe { File::from_raw_fd(descriptor) });
        bail!("export_client_staging_directory_read_failed")
    }
    let stream = DirectoryStream(stream);
    let mut entries = std::collections::BTreeSet::new();
    loop {
        let entry = unsafe { libc::readdir(stream.0) };
        if entry.is_null() {
            break;
        }
        let name = unsafe { CStr::from_ptr((*entry).d_name.as_ptr()) }.to_bytes();
        if !matches!(name, b"." | b"..") {
            entries.insert(name.to_vec());
        }
    }
    ensure!(
        entries == std::collections::BTreeSet::from([b"ca.pem".to_vec(), b"client.pem".to_vec()]),
        "export_client_staging_existing_invalid"
    );
    Ok(())
}

#[cfg(unix)]
fn verify_existing_file(parent: &File, name: &str, writer_uid: u32, expected: &[u8]) -> Result<()> {
    let name = CString::new(name).context("export_client_staging_name_invalid")?;
    let descriptor = unsafe {
        libc::openat(
            parent.as_raw_fd(),
            name.as_ptr(),
            libc::O_RDONLY | libc::O_NOFOLLOW | libc::O_CLOEXEC | libc::O_NONBLOCK,
        )
    };
    if descriptor < 0 {
        bail!("export_client_staging_existing_invalid")
    }
    let file = unsafe { File::from_raw_fd(descriptor) };
    verify_file(&file, writer_uid, expected)?;
    let mut bytes = Vec::with_capacity(expected.len());
    file.take((expected.len() + 1) as u64)
        .read_to_end(&mut bytes)
        .context("export_client_staging_existing_read_failed")?;
    ensure!(bytes == expected, "export_client_staging_existing_conflict");
    Ok(())
}

#[cfg(unix)]
fn verify_file(file: &File, writer_uid: u32, expected: &[u8]) -> Result<()> {
    let metadata = file
        .metadata()
        .context("export_client_staging_file_metadata_failed")?;
    ensure!(
        metadata.is_file()
            && metadata.uid() == writer_uid
            && metadata.mode() & 0o777 == 0o600
            && metadata.nlink() == 1
            && metadata.len() == expected.len() as u64,
        "export_client_staging_file_not_private_regular"
    );
    Ok(())
}

#[cfg(unix)]
enum Publish {
    Created,
    Exists,
}

#[cfg(target_os = "macos")]
fn publish_no_replace(parent: &File, temporary: &CString, target: &CString) -> Result<Publish> {
    let result = unsafe {
        libc::renameatx_np(
            parent.as_raw_fd(),
            temporary.as_ptr(),
            parent.as_raw_fd(),
            target.as_ptr(),
            libc::RENAME_EXCL,
        )
    };
    publish_result(result)
}

#[cfg(target_os = "linux")]
fn publish_no_replace(parent: &File, temporary: &CString, target: &CString) -> Result<Publish> {
    let result = unsafe {
        libc::renameat2(
            parent.as_raw_fd(),
            temporary.as_ptr(),
            parent.as_raw_fd(),
            target.as_ptr(),
            libc::RENAME_NOREPLACE,
        )
    };
    publish_result(result)
}

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
fn publish_no_replace(_: &File, _: &CString, _: &CString) -> Result<Publish> {
    bail!("export_client_staging_atomic_publish_unsupported")
}

#[cfg(unix)]
fn publish_result(result: i32) -> Result<Publish> {
    if result == 0 {
        return Ok(Publish::Created);
    }
    if std::io::Error::last_os_error().kind() == std::io::ErrorKind::AlreadyExists {
        return Ok(Publish::Exists);
    }
    bail!("export_client_staging_publish_failed")
}

#[cfg(unix)]
struct DirectoryStream(*mut libc::DIR);

#[cfg(unix)]
impl Drop for DirectoryStream {
    fn drop(&mut self) {
        unsafe {
            libc::closedir(self.0);
        }
    }
}

#[cfg(unix)]
struct PartialDirectory {
    parent: File,
    name: CString,
    published: std::cell::Cell<bool>,
}

#[cfg(unix)]
impl PartialDirectory {
    fn new(parent: File, name: CString) -> Self {
        Self {
            parent,
            name,
            published: std::cell::Cell::new(false),
        }
    }

    fn publish(&self) {
        self.published.set(true);
    }
}

#[cfg(unix)]
impl Drop for PartialDirectory {
    fn drop(&mut self) {
        if self.published.get() {
            return;
        }
        if let Ok(directory) = open_directory_at(&self.parent, &self.name) {
            for name in [b"ca.pem".as_slice(), b"client.pem".as_slice()] {
                if let Ok(name) = CString::new(name) {
                    unsafe {
                        libc::unlinkat(directory.as_raw_fd(), name.as_ptr(), 0);
                    }
                }
            }
        }
        unsafe {
            libc::unlinkat(
                self.parent.as_raw_fd(),
                self.name.as_ptr(),
                libc::AT_REMOVEDIR,
            );
        }
    }
}

#[cfg(unix)]
use std::os::{
    fd::{AsRawFd, FromRawFd, IntoRawFd},
    unix::{
        ffi::OsStrExt,
        fs::{MetadataExt, OpenOptionsExt},
    },
};
