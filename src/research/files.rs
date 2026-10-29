//! Copying, inventorying and hashing input and output trees.
use super::*;

pub(super) fn copy_named_source(source: &Path, destination_root: &Path) -> Result<()> {
    let source = fs::canonicalize(source)?;
    let name = source
        .file_name()
        .and_then(|value| value.to_str())
        .context("research input has no UTF-8 filename")?;
    validate_component(name, "research input")?;
    copy_tree(&source, &destination_root.join(name))
}

pub(super) fn copy_tree(source: &Path, destination: &Path) -> Result<()> {
    let metadata = fs::symlink_metadata(source)?;
    if metadata.file_type().is_symlink() {
        bail!("snapshot sources cannot be symbolic links");
    }
    if metadata.is_file() {
        if destination.exists() {
            bail!(
                "snapshot destination already exists: {}",
                destination.display()
            );
        }
        copy_regular(source, destination, &metadata)?;
        return Ok(());
    }
    if !metadata.is_dir() {
        bail!("snapshot sources must be regular files or directories");
    }
    if destination.exists() {
        bail!(
            "snapshot destination already exists: {}",
            destination.display()
        );
    }
    create_private_dir(destination)?;
    let mut entries = WalkDir::new(source)
        .follow_links(false)
        .into_iter()
        .collect::<std::result::Result<Vec<_>, _>>()?;
    entries.sort_by(|left, right| left.path().cmp(right.path()));
    if entries.len() as u64 > MAX_SNAPSHOT_FILES + 1 {
        bail!("snapshot exceeds the file-count limit");
    }
    let mut bytes = 0_u64;
    for entry in entries.into_iter().skip(1) {
        let relative = entry.path().strip_prefix(source)?;
        let target = destination.join(relative);
        let metadata = fs::symlink_metadata(entry.path())?;
        if metadata.file_type().is_symlink() {
            bail!(
                "snapshot cannot contain symbolic links: {}",
                relative.display()
            );
        }
        if metadata.is_dir() {
            create_private_dir(&target)?;
        } else if metadata.is_file() {
            bytes = bytes
                .checked_add(metadata.len())
                .context("snapshot byte count overflow")?;
            if bytes > MAX_SNAPSHOT_BYTES {
                bail!("snapshot exceeds the byte limit");
            }
            copy_regular(entry.path(), &target, &metadata)?;
        } else {
            bail!(
                "snapshot contains a non-regular entry: {}",
                relative.display()
            );
        }
    }
    Ok(())
}

pub(super) fn copy_regular(
    source: &Path,
    destination: &Path,
    metadata: &fs::Metadata,
) -> Result<()> {
    if let Some(parent) = destination.parent() {
        create_private_dir(parent)?;
    }
    fs::copy(source, destination)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let source_mode = metadata.permissions().mode();
        let mode = if source_mode & 0o100 != 0 {
            0o700
        } else {
            0o600
        };
        fs::set_permissions(destination, fs::Permissions::from_mode(mode))?;
    }
    if hash_file(source)? != hash_file(destination)? {
        bail!("snapshot copy digest mismatch: {}", destination.display());
    }
    Ok(())
}

pub(super) fn inventory_tree(root: &Path) -> Result<SnapshotManifest> {
    if !root.is_dir() {
        bail!("snapshot root is not a directory: {}", root.display());
    }
    let mut entries = WalkDir::new(root)
        .follow_links(false)
        .into_iter()
        .collect::<std::result::Result<Vec<_>, _>>()?;
    entries.sort_by(|left, right| left.path().cmp(right.path()));
    let mut files = Vec::new();
    let mut total_bytes = 0_u64;
    for entry in entries.into_iter().skip(1) {
        let metadata = fs::symlink_metadata(entry.path())?;
        let relative = entry.path().strip_prefix(root)?;
        if metadata.file_type().is_symlink() {
            bail!("snapshot contains a symbolic link: {}", relative.display());
        }
        if metadata.is_file() {
            total_bytes = total_bytes
                .checked_add(metadata.len())
                .context("snapshot byte count overflow")?;
            if files.len() as u64 >= MAX_SNAPSHOT_FILES || total_bytes > MAX_SNAPSHOT_BYTES {
                bail!("snapshot exceeds its file or byte limit");
            }
            let path = relative
                .to_str()
                .context("snapshot contains a non-UTF-8 path")?;
            if path.contains(['\n', '\r', '\0']) {
                bail!("snapshot contains a path with control characters");
            }
            files.push(SnapshotEntry {
                path: path.into(),
                size: metadata.len(),
                sha256: hash_file(entry.path())?,
            });
        } else if !metadata.is_dir() {
            bail!(
                "snapshot contains a non-regular entry: {}",
                relative.display()
            );
        }
    }
    Ok(SnapshotManifest {
        schema_version: 1,
        files,
        total_bytes,
    })
}

pub(super) fn hash_path(path: &Path) -> Result<(String, u64, u64)> {
    hash_path_with_limits(path, MAX_SNAPSHOT_FILES, MAX_SNAPSHOT_BYTES)
}

pub(super) fn hash_path_with_limits(
    path: &Path,
    max_files: u64,
    max_bytes: u64,
) -> Result<(String, u64, u64)> {
    let metadata = fs::symlink_metadata(path)?;
    if metadata.file_type().is_symlink() {
        bail!("hash roots cannot be symbolic links");
    }
    let mut hasher = Sha256::new();
    let mut count = 0_u64;
    let mut total = 0_u64;
    if metadata.is_file() {
        if max_files == 0 || metadata.len() > max_bytes {
            bail!("hashed file exceeds its file or byte limit");
        }
        hash_file_into(&mut hasher, path, Path::new("."))?;
        return Ok((hex::encode(hasher.finalize()), 1, metadata.len()));
    }
    if !metadata.is_dir() {
        bail!("hash root must be a regular file or directory");
    }
    let mut entries = WalkDir::new(path)
        .follow_links(false)
        .into_iter()
        .collect::<std::result::Result<Vec<_>, _>>()?;
    entries.sort_by(|left, right| left.path().cmp(right.path()));
    for entry in entries {
        let metadata = fs::symlink_metadata(entry.path())?;
        let relative = entry.path().strip_prefix(path)?;
        if metadata.file_type().is_symlink() {
            bail!("hash tree contains a symbolic link: {}", relative.display());
        }
        if metadata.is_dir() {
            hasher.update(b"dir\0");
            hasher.update(relative.as_os_str().as_encoded_bytes());
            hasher.update(b"\0");
        } else if metadata.is_file() {
            count = count.checked_add(1).context("file count overflow")?;
            total = total
                .checked_add(metadata.len())
                .context("byte count overflow")?;
            if count > max_files || total > max_bytes {
                bail!("hashed tree exceeds its file or byte limit");
            }
            hash_file_into(&mut hasher, entry.path(), relative)?;
        } else {
            bail!(
                "hash tree contains a non-regular entry: {}",
                relative.display()
            );
        }
    }
    Ok((hex::encode(hasher.finalize()), count, total))
}

pub(super) fn hash_file_into(hasher: &mut Sha256, path: &Path, relative: &Path) -> Result<()> {
    hasher.update(b"file\0");
    hasher.update(relative.as_os_str().as_encoded_bytes());
    hasher.update(b"\0");
    let mut file = fs::File::open(path)?;
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        let read = file.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    hasher.update(b"\0");
    Ok(())
}

pub(super) fn create_output_tree(output: &Path) -> Result<()> {
    create_private_dir(output)?;
    for name in [
        "scripts",
        "intermediate",
        "discarded",
        "results",
        "figures",
        "logs",
    ] {
        create_private_dir(&output.join(name))?;
    }
    Ok(())
}

pub(super) fn read_required_text(
    inline: Option<&str>,
    file: Option<&Path>,
    label: &str,
) -> Result<String> {
    match (inline, file) {
        (Some(value), None) => {
            validate_text(value, label)?;
            Ok(value.trim().to_string())
        }
        (None, Some(path)) => read_absolute_text(path, label),
        (None, None) => bail!("{label} is required"),
        (Some(_), Some(_)) => bail!("provide {label} inline or by file, not both"),
    }
}

pub(super) fn read_absolute_text(path: &Path, label: &str) -> Result<String> {
    if !path.is_absolute() {
        bail!("{label} file must use an absolute path");
    }
    let metadata = fs::symlink_metadata(path)?;
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        bail!("{label} path must be a regular file");
    }
    if metadata.len() as usize > MAX_TEXT_BYTES {
        bail!("{label} exceeds the 1 MiB text limit");
    }
    let body = fs::read_to_string(path)?;
    validate_text(&body, label)?;
    Ok(body.trim().to_string())
}
