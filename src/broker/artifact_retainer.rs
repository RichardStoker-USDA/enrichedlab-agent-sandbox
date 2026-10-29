//! Copy only server-declared, digest-bound files into durable broker storage.
//! The site exporter owns the PVC mount and the frozen per-attempt selection.

use super::{
    model::ClaimedTask,
    sandbox::{ArtifactRetainer, RetainedArtifact, SandboxBinding, SandboxHandle, valid_artifact},
};
use anyhow::{Context, Result, bail};
use async_trait::async_trait;
use sha2::{Digest, Sha256};
use std::{
    collections::HashSet,
    fs::File,
    io::{Read, Write},
    path::{Component, Path, PathBuf},
};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWriteExt};
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

#[derive(Clone, Debug)]
pub struct ExportFile {
    pub relative_path: String,
    pub visibility: String,
    pub role: String,
    pub label: String,
    pub media_type: String,
    pub size_bytes: u64,
    /// Bare lowercase SHA-256 from the trusted file observer, never model text.
    pub sha256: String,
}

#[derive(Clone, Debug)]
pub struct ExportRoots {
    pub mounted_artifacts: PathBuf,
    pub durable_artifacts: PathBuf,
}

#[derive(Clone, Copy, Debug)]
pub struct ExportLimits {
    pub max_files: usize,
    pub max_bytes: u64,
}

/// Copies one server-declared artifact stream into the durable broker store.
/// The caller selects only an index from the frozen server-owned plan.
#[derive(Clone, Debug)]
pub struct StreamArtifactRetainer {
    expected: SandboxHandle,
    durable_artifacts: PathBuf,
    prefix: String,
    files: Vec<ExportFile>,
}

#[derive(Clone, Debug)]
pub struct FilesystemArtifactRetainer {
    expected: SandboxHandle,
    roots: ExportRoots,
    prefix: String,
    files: Vec<ExportFile>,
}

impl FilesystemArtifactRetainer {
    /// Trusted dispatcher entry point. Roots and file selections are server-owned;
    /// this constructor is not exposed through the researcher or model tool API.
    pub fn for_task(
        task: &ClaimedTask,
        binding: &SandboxBinding,
        handle: &SandboxHandle,
        roots: ExportRoots,
        files: Vec<ExportFile>,
        limits: ExportLimits,
    ) -> Result<Self> {
        validate_task_binding(task, binding, handle)?;
        validate_export_files(&files, limits)?;
        if !roots.mounted_artifacts.is_absolute()
            || !roots.durable_artifacts.is_absolute()
            || roots.mounted_artifacts == roots.durable_artifacts
            || roots
                .mounted_artifacts
                .starts_with(&roots.durable_artifacts)
            || roots
                .durable_artifacts
                .starts_with(&roots.mounted_artifacts)
        {
            bail!("export_roots_invalid")
        }
        let prefix = export_prefix(task);
        Ok(Self {
            expected: handle.clone(),
            roots,
            prefix,
            files,
        })
    }

    fn copy(&self, cancellation: &CancellationToken) -> Result<Vec<RetainedArtifact>> {
        active(cancellation)?;
        let source = Directory::open(&self.roots.mounted_artifacts, false)?;
        let destination = Directory::open(&self.roots.durable_artifacts, true)?;
        let mut artifacts = Vec::with_capacity(self.files.len());
        for selected in &self.files {
            active(cancellation)?;
            let artifact = metadata(&self.prefix, selected);
            copy_one(
                &source,
                &destination,
                selected,
                &artifact.relative_path,
                cancellation,
            )?;
            artifacts.push(artifact);
        }
        active(cancellation)?;
        Ok(artifacts)
    }
}

impl StreamArtifactRetainer {
    /// Trusted transport entry point. It has no source root because source bytes
    /// arrive through an authenticated caller-owned stream.
    pub fn for_task(
        task: &ClaimedTask,
        binding: &SandboxBinding,
        handle: &SandboxHandle,
        durable_artifacts: PathBuf,
        files: Vec<ExportFile>,
        limits: ExportLimits,
    ) -> Result<Self> {
        validate_task_binding(task, binding, handle)?;
        validate_export_files(&files, limits)?;
        if !durable_artifacts.is_absolute() || durable_artifacts == Path::new("/") {
            bail!("export_roots_invalid")
        }
        Ok(Self {
            expected: handle.clone(),
            durable_artifacts,
            prefix: export_prefix(task),
            files,
        })
    }

    /// The durable root the retained paths are relative to.
    pub fn durable_root(&self) -> &Path {
        &self.durable_artifacts
    }

    pub fn files(&self) -> &[ExportFile] {
        &self.files
    }

    pub fn check_handle(&self, handle: &SandboxHandle) -> Result<()> {
        if handle != &self.expected {
            bail!("export_handle_mismatch")
        }
        Ok(())
    }

    pub async fn retain_stream<R>(
        &self,
        index: usize,
        input: R,
        cancellation: &CancellationToken,
    ) -> Result<RetainedArtifact>
    where
        R: AsyncRead + Unpin + Send,
    {
        let selected = self
            .files
            .get(index)
            .ok_or_else(|| anyhow::anyhow!("export_file_index_invalid"))?;
        retain_stream_to_durable(
            &self.durable_artifacts,
            &self.prefix,
            selected,
            input,
            cancellation,
        )
        .await
    }
}

pub(crate) fn validate_task_binding(
    task: &ClaimedTask,
    binding: &SandboxBinding,
    handle: &SandboxHandle,
) -> Result<()> {
    if task.id != handle.task_id
        || task.lease_generation != handle.lease_generation
        || task.principal_id != binding.principal_id
        || task.group_key != binding.group_key
        || task.profile_key != binding.profile_key
        || handle.namespace != binding.namespace
        || handle.lab_ref != binding.lab_ref
        || handle.worker_id.is_empty()
        || handle.sandbox_uid.is_empty()
        || handle.sandbox_generation < 1
        || task.attempt < 1
        || task.lease_generation < 1
    {
        bail!("export_task_binding_mismatch")
    }
    Ok(())
}

pub(crate) fn validate_export_files(files: &[ExportFile], limits: ExportLimits) -> Result<()> {
    if files.is_empty()
        || files.len() > limits.max_files
        || limits.max_files > 100_000
        || limits.max_bytes == 0
        || limits.max_bytes > i64::MAX as u64
    {
        bail!("export_limits_invalid")
    }
    let mut paths = HashSet::new();
    let mut total = 0_u64;
    for file in files {
        if !paths.insert(&file.relative_path) {
            bail!("export_duplicate_path")
        }
        total = total
            .checked_add(file.size_bytes)
            .context("export_size_overflow")?;
        if total > limits.max_bytes || file.size_bytes > i64::MAX as u64 {
            bail!("export_limits_exceeded")
        }
        valid_artifact(&metadata("selected", file))?;
    }
    Ok(())
}

fn export_prefix(task: &ClaimedTask) -> String {
    format!(
        "tasks/{}/attempts/{}-{}/exports",
        task.id, task.attempt, task.lease_generation
    )
}

/// Opens a server-selected source file beneath a held trusted root. Callers
/// receive only a checked regular descriptor; they never pass arbitrary paths
/// to the transport.
pub fn open_selected_file(root: &Path, file: &ExportFile) -> Result<File> {
    valid_artifact(&metadata("selected", file))?;
    let opened = open_regular_file_beneath(root, &file.relative_path)?;
    #[cfg(unix)]
    regular(&opened, file.size_bytes, false)?;
    Ok(opened)
}

/// Opens a regular file selected by a trusted caller beneath a held root.
/// This establishes path and file shape only. Callers that already have a
/// measured size or digest must verify those bytes separately.
pub(crate) fn open_regular_file_beneath(root: &Path, relative_path: &str) -> Result<File> {
    #[cfg(unix)]
    {
        let source = Directory::open(root, false)?;
        let (parent, name) = source.parent(relative_path, false)?;
        let opened = open_at(
            &parent,
            OsStr::from_bytes(name.as_bytes()),
            libc::O_RDONLY,
            0,
        )?;
        let m = opened.metadata().context("export_file_metadata_failed")?;
        if !m.is_file() || m.nlink() != 1 {
            bail!("export_file_shape_mismatch")
        }
        Ok(opened)
    }
    #[cfg(not(unix))]
    {
        let _ = root;
        let _ = relative_path;
        bail!("export_requires_unix")
    }
}

#[async_trait]
impl ArtifactRetainer for FilesystemArtifactRetainer {
    async fn retain(
        &self,
        handle: &SandboxHandle,
        cancellation: &CancellationToken,
    ) -> Result<Vec<RetainedArtifact>> {
        if handle != &self.expected {
            bail!("export_handle_mismatch")
        }
        let exporter = self.clone();
        let cancellation = cancellation.clone();
        tokio::task::spawn_blocking(move || exporter.copy(&cancellation))
            .await
            .context("export_worker_failed")?
    }
    fn retained_root(&self) -> Option<&Path> {
        Some(&self.roots.durable_artifacts)
    }
}

fn metadata(prefix: &str, f: &ExportFile) -> RetainedArtifact {
    RetainedArtifact {
        visibility: f.visibility.clone(),
        role: f.role.clone(),
        label: f.label.clone(),
        relative_path: format!("{prefix}/{}", f.relative_path),
        media_type: f.media_type.clone(),
        size_bytes: f.size_bytes as i64,
        sha256: f.sha256.clone(),
    }
}

#[cfg(unix)]
async fn retain_stream_to_durable<R>(
    durable_artifacts: &Path,
    prefix: &str,
    selected: &ExportFile,
    input: R,
    cancellation: &CancellationToken,
) -> Result<RetainedArtifact>
where
    R: AsyncRead + Unpin + Send,
{
    active(cancellation)?;
    let destination = Directory::open(durable_artifacts, true)?;
    let artifact = metadata(prefix, selected);
    let (parent, name) = destination.parent(&artifact.relative_path, true)?;
    match open_at(
        &parent,
        OsStr::from_bytes(name.as_bytes()),
        libc::O_RDONLY,
        0,
    ) {
        Ok(existing) => {
            regular(&existing, selected.size_bytes, true)?;
            let mut existing = tokio::fs::File::from_std(existing);
            verify_async_stream(&mut existing, None, selected, cancellation).await?;
            existing
                .sync_all()
                .await
                .context("export_file_sync_failed")?;
            parent.sync_all().context("export_directory_sync_failed")?;
            return Ok(artifact);
        }
        Err(e)
            if e.downcast_ref::<std::io::Error>()
                .is_some_and(|e| e.kind() == std::io::ErrorKind::NotFound) => {}
        Err(e) => return Err(e),
    }

    let temporary = format!(".{}.partial", Uuid::new_v4());
    let output = open_at(
        &parent,
        OsStr::new(&temporary),
        libc::O_WRONLY | libc::O_CREAT | libc::O_EXCL,
        0o600,
    )?;
    let partial = PartialFile {
        parent: parent
            .try_clone()
            .context("export_directory_clone_failed")?,
        name: CString::new(temporary).context("export_path_invalid")?,
    };
    let mut output = tokio::fs::File::from_std(output);
    let mut input = input;
    verify_async_stream(&mut input, Some(&mut output), selected, cancellation).await?;
    output.sync_all().await.context("export_file_sync_failed")?;
    let output = output.into_std().await;
    regular(&output, selected.size_bytes, true)?;
    drop(output);
    active(cancellation)?;

    // linkat publishes without replacing a result retained by another worker.
    if unsafe {
        libc::linkat(
            parent.as_raw_fd(),
            partial.name.as_ptr(),
            parent.as_raw_fd(),
            name.as_ptr(),
            0,
        )
    } < 0
    {
        if std::io::Error::last_os_error().kind() != std::io::ErrorKind::AlreadyExists {
            bail!("export_publish_failed")
        }
        let existing = open_at(
            &parent,
            OsStr::from_bytes(name.as_bytes()),
            libc::O_RDONLY,
            0,
        )?;
        regular(&existing, selected.size_bytes, true)?;
        let mut existing = tokio::fs::File::from_std(existing);
        verify_async_stream(&mut existing, None, selected, cancellation).await?;
    }
    drop(partial);
    parent.sync_all().context("export_directory_sync_failed")?;
    Ok(artifact)
}

#[cfg(unix)]
async fn verify_async_stream<R>(
    input: &mut R,
    mut output: Option<&mut tokio::fs::File>,
    expected: &ExportFile,
    cancellation: &CancellationToken,
) -> Result<()>
where
    R: AsyncRead + Unpin,
{
    let mut bytes = 0_u64;
    let mut hash = Sha256::new();
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        active(cancellation)?;
        let count = tokio::select! {
            _ = cancellation.cancelled() => bail!("export_cancelled"),
            result = input.read(&mut buffer) => result.context("export_read_failed")?,
        };
        if count == 0 {
            break;
        }
        bytes = bytes
            .checked_add(count as u64)
            .context("export_size_overflow")?;
        if bytes > expected.size_bytes {
            bail!("export_size_mismatch")
        }
        hash.update(&buffer[..count]);
        if let Some(writer) = output.as_deref_mut() {
            tokio::select! {
                _ = cancellation.cancelled() => bail!("export_cancelled"),
                result = writer.write_all(&buffer[..count]) => result.context("export_write_failed")?,
            }
        }
    }
    if bytes != expected.size_bytes || hex::encode(hash.finalize()) != expected.sha256 {
        bail!("export_digest_mismatch")
    }
    active(cancellation)
}

#[cfg(not(unix))]
async fn retain_stream_to_durable<R>(
    _: &Path,
    _: &str,
    _: &ExportFile,
    _: R,
    _: &CancellationToken,
) -> Result<RetainedArtifact>
where
    R: AsyncRead + Unpin + Send,
{
    bail!("export_requires_unix")
}
fn active(cancellation: &CancellationToken) -> Result<()> {
    if cancellation.is_cancelled() {
        bail!("export_cancelled")
    }
    Ok(())
}

#[cfg(unix)]
use std::{
    ffi::{CString, OsStr},
    os::{
        fd::{AsRawFd, FromRawFd},
        unix::{
            ffi::OsStrExt,
            fs::{MetadataExt, OpenOptionsExt},
        },
    },
};

struct Directory(File);
impl Directory {
    #[cfg(unix)]
    fn open(path: &Path, private: bool) -> Result<Self> {
        if !path.is_absolute() || path == Path::new("/") {
            bail!("export_root_invalid")
        }
        let mut dir = std::fs::OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC)
            .open("/")
            .context("export_root_unavailable")?;
        for part in path.components() {
            match part {
                Component::RootDir => {}
                Component::Normal(name) => {
                    dir = open_at(&dir, name, libc::O_RDONLY | libc::O_DIRECTORY, 0)?
                }
                _ => bail!("export_root_invalid"),
            }
        }
        if private {
            let m = dir.metadata().context("export_root_metadata_failed")?;
            if m.uid() != unsafe { libc::geteuid() } || m.mode() & 0o077 != 0 {
                bail!("export_destination_not_private")
            }
        }
        Ok(Self(dir))
    }

    #[cfg(unix)]
    fn parent(&self, relative: &str, create: bool) -> Result<(File, CString)> {
        let path = Path::new(relative);
        let mut dir = self
            .0
            .try_clone()
            .context("export_directory_clone_failed")?;
        for part in path.parent().unwrap_or(Path::new("")).components() {
            let Component::Normal(name) = part else {
                bail!("export_path_invalid")
            };
            match open_at(&dir, name, libc::O_RDONLY | libc::O_DIRECTORY, 0) {
                Ok(next) => dir = next,
                Err(e)
                    if create
                        && e.downcast_ref::<std::io::Error>()
                            .is_some_and(|e| e.kind() == std::io::ErrorKind::NotFound) =>
                {
                    let name_c = CString::new(name.as_bytes()).context("export_path_invalid")?;
                    if unsafe { libc::mkdirat(dir.as_raw_fd(), name_c.as_ptr(), 0o700) } < 0
                        && std::io::Error::last_os_error().kind()
                            != std::io::ErrorKind::AlreadyExists
                    {
                        bail!("export_directory_create_failed")
                    }
                    dir.sync_all().context("export_directory_sync_failed")?;
                    dir = open_at(&dir, name, libc::O_RDONLY | libc::O_DIRECTORY, 0)?;
                }
                Err(e) => return Err(e),
            }
        }
        let name = CString::new(path.file_name().context("export_path_invalid")?.as_bytes())
            .context("export_path_invalid")?;
        Ok((dir, name))
    }

    #[cfg(not(unix))]
    fn open(_: &Path, _: bool) -> Result<Self> {
        bail!("export_requires_unix")
    }
}

#[cfg(unix)]
fn open_at(parent: &File, name: &OsStr, flags: i32, mode: u32) -> Result<File> {
    let name = CString::new(name.as_bytes()).context("export_path_invalid")?;
    let fd = unsafe {
        libc::openat(
            parent.as_raw_fd(),
            name.as_ptr(),
            flags | libc::O_NOFOLLOW | libc::O_CLOEXEC | libc::O_NONBLOCK,
            mode,
        )
    };
    if fd < 0 {
        return Err(std::io::Error::last_os_error()).context("export_file_open_failed");
    }
    Ok(unsafe { File::from_raw_fd(fd) })
}

#[cfg(unix)]
fn regular(file: &File, expected_size: u64, private: bool) -> Result<()> {
    let m = file.metadata().context("export_file_metadata_failed")?;
    if !m.is_file()
        || m.nlink() != 1
        || m.len() != expected_size
        || (private && (m.uid() != unsafe { libc::geteuid() } || m.mode() & 0o077 != 0))
    {
        bail!("export_file_shape_mismatch")
    }
    Ok(())
}

#[cfg(unix)]
struct PartialFile {
    parent: File,
    name: CString,
}
#[cfg(unix)]
impl Drop for PartialFile {
    fn drop(&mut self) {
        unsafe {
            libc::unlinkat(self.parent.as_raw_fd(), self.name.as_ptr(), 0);
        }
    }
}

#[cfg(unix)]
fn copy_one(
    source: &Directory,
    destination: &Directory,
    selected: &ExportFile,
    target: &str,
    cancellation: &CancellationToken,
) -> Result<()> {
    let (source_parent, source_name) = source.parent(&selected.relative_path, false)?;
    let mut input = open_at(
        &source_parent,
        OsStr::from_bytes(source_name.as_bytes()),
        libc::O_RDONLY,
        0,
    )?;
    regular(&input, selected.size_bytes, false)?;
    let (parent, name) = destination.parent(target, true)?;
    match open_at(
        &parent,
        OsStr::from_bytes(name.as_bytes()),
        libc::O_RDONLY,
        0,
    ) {
        Ok(mut existing) => {
            regular(&existing, selected.size_bytes, true)?;
            verify_stream(&mut existing, None, selected, cancellation)?;
            existing.sync_all().context("export_file_sync_failed")?;
            parent.sync_all().context("export_directory_sync_failed")?;
            return Ok(());
        }
        Err(e)
            if e.downcast_ref::<std::io::Error>()
                .is_some_and(|e| e.kind() == std::io::ErrorKind::NotFound) => {}
        Err(e) => return Err(e),
    }
    let temporary = format!(".{}.partial", Uuid::new_v4());
    let mut output = open_at(
        &parent,
        OsStr::new(&temporary),
        libc::O_WRONLY | libc::O_CREAT | libc::O_EXCL,
        0o600,
    )?;
    let partial = PartialFile {
        parent: parent
            .try_clone()
            .context("export_directory_clone_failed")?,
        name: CString::new(temporary).unwrap(),
    };
    verify_stream(&mut input, Some(&mut output), selected, cancellation)?;
    output.sync_all().context("export_file_sync_failed")?;
    active(cancellation)?;
    // linkat is an atomic no-replace publication on the destination filesystem.
    if unsafe {
        libc::linkat(
            parent.as_raw_fd(),
            partial.name.as_ptr(),
            parent.as_raw_fd(),
            name.as_ptr(),
            0,
        )
    } < 0
    {
        if std::io::Error::last_os_error().kind() != std::io::ErrorKind::AlreadyExists {
            bail!("export_publish_failed")
        }
        let mut existing = open_at(
            &parent,
            OsStr::from_bytes(name.as_bytes()),
            libc::O_RDONLY,
            0,
        )?;
        regular(&existing, selected.size_bytes, true)?;
        verify_stream(&mut existing, None, selected, cancellation)?;
    }
    drop(partial);
    parent.sync_all().context("export_directory_sync_failed")?;
    Ok(())
}

#[cfg(unix)]
fn verify_stream(
    input: &mut File,
    mut output: Option<&mut File>,
    expected: &ExportFile,
    cancellation: &CancellationToken,
) -> Result<()> {
    let mut bytes = 0_u64;
    let mut hash = Sha256::new();
    let mut buffer = [0u8; 64 * 1024];
    loop {
        active(cancellation)?;
        let count = input.read(&mut buffer).context("export_read_failed")?;
        if count == 0 {
            break;
        }
        bytes = bytes
            .checked_add(count as u64)
            .context("export_size_overflow")?;
        if bytes > expected.size_bytes {
            bail!("export_size_mismatch")
        }
        hash.update(&buffer[..count]);
        if let Some(writer) = output.as_deref_mut() {
            writer
                .write_all(&buffer[..count])
                .context("export_write_failed")?;
        }
    }
    if bytes != expected.size_bytes || hex::encode(hash.finalize()) != expected.sha256 {
        bail!("export_digest_mismatch")
    }
    regular(input, expected.size_bytes, false)?;
    Ok(())
}
#[cfg(not(unix))]
fn copy_one(
    _: &Directory,
    _: &Directory,
    _: &ExportFile,
    _: &str,
    _: &CancellationToken,
) -> Result<()> {
    bail!("export_requires_unix")
}
