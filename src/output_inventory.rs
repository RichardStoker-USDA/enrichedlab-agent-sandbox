//! Measure only an already-approved output selection through held descriptors.
//!
//! This module neither creates a terminal export record nor binds task, Pod, or
//! lease identity. A later trusted observer owns those bindings.

use crate::{
    broker::{admission::open_bounded_record, artifact_retainer::open_regular_file_beneath},
    custody::canonical_document_digest,
};
use anyhow::{Context, Result, anyhow, bail, ensure};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    collections::HashSet,
    fs::File,
    io::{Read, Seek, SeekFrom},
    path::{Component, Path, PathBuf},
    time::{Duration, Instant},
};
use tokio_util::sync::CancellationToken;

pub const REQUEST_SCHEMA: &str = "sandbox.enrichedlab.org/output-inventory-request/v1";
pub const INVENTORY_SCHEMA: &str = "sandbox.enrichedlab.org/output-inventory/v1";
const REQUEST_LIMIT: u64 = 1024 * 1024;
const OUTPUT_LIMIT: usize = 1024 * 1024;
const MAX_FILES: usize = 100_000;
const MAX_BYTES: u64 = 64 * 1024 * 1024 * 1024;
const MAX_DURATION: Duration = Duration::from_secs(60);

#[derive(Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct OutputInventoryRequest {
    pub schema_version: String,
    pub mounted_root: PathBuf,
    pub files: Vec<SelectedOutput>,
    pub max_files: usize,
    pub max_bytes: u64,
}

#[derive(Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SelectedOutput {
    pub relative_path: String,
    pub visibility: String,
    pub role: String,
    pub label: String,
    pub media_type: String,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct MeasuredOutput {
    pub relative_path: String,
    pub visibility: String,
    pub role: String,
    pub label: String,
    pub media_type: String,
    pub size_bytes: u64,
    /// Bare lowercase SHA-256 for the existing terminal export selection shape.
    pub sha256: String,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct OutputInventory {
    pub schema_version: String,
    pub selection_digest: String,
    pub files: Vec<MeasuredOutput>,
}

impl OutputInventoryRequest {
    /// Reads an operator-owned closed request from a private descriptor root.
    /// The path is never selected by a task or model.
    pub fn load(root: &Path, name: &str, writer_uid: u32) -> Result<Self> {
        if name.is_empty() || name == "." || name == ".." || name.contains(['/', '\\', '\0']) {
            bail!("output_inventory_request_name_invalid")
        }
        let file = open_bounded_record(root, name, writer_uid, REQUEST_LIMIT)
            .map_err(|_| anyhow!("output_inventory_request_unavailable"))?;
        let mut bytes = Vec::new();
        file.take(REQUEST_LIMIT + 1)
            .read_to_end(&mut bytes)
            .map_err(|_| anyhow!("output_inventory_request_read_failed"))?;
        if bytes.len() as u64 > REQUEST_LIMIT {
            bail!("output_inventory_request_too_large")
        }
        let request = serde_json::from_slice(&bytes)
            .map_err(|_| anyhow!("output_inventory_request_invalid"))?;
        validate_request(&request)?;
        Ok(request)
    }

    /// Verifies that a measured inventory is still the exact ordered selection
    /// from this closed request. It carries no task, runtime, or export authority.
    pub fn validate_inventory(&self, inventory: &OutputInventory) -> Result<()> {
        validate_request(self)?;
        ensure_result_capacity(self)?;
        ensure!(
            inventory.schema_version == INVENTORY_SCHEMA
                && inventory.selection_digest == selection_digest(self)?,
            "output_inventory_selection_mismatch"
        );
        ensure!(
            inventory.files.len() == self.files.len() && inventory.files.len() <= self.max_files,
            "output_inventory_selection_mismatch"
        );
        let mut total = 0_u64;
        for (selected, measured) in self.files.iter().zip(&inventory.files) {
            ensure!(
                selected.relative_path == measured.relative_path
                    && selected.visibility == measured.visibility
                    && selected.role == measured.role
                    && selected.label == measured.label
                    && selected.media_type == measured.media_type
                    && measured.sha256.len() == 64
                    && measured
                        .sha256
                        .bytes()
                        .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte)),
                "output_inventory_selection_mismatch"
            );
            total = total
                .checked_add(measured.size_bytes)
                .ok_or_else(|| anyhow!("output_inventory_size_overflow"))?;
            ensure!(
                total <= self.max_bytes && measured.size_bytes <= i64::MAX as u64,
                "output_inventory_limits_exceeded"
            );
        }
        ensure!(
            serde_json::to_vec(inventory)
                .map_err(|_| anyhow!("output_inventory_encode_failed"))?
                .len()
                <= OUTPUT_LIMIT,
            "output_inventory_result_too_large"
        );
        Ok(())
    }
}

pub fn inventory(
    request: &OutputInventoryRequest,
    cancellation: &CancellationToken,
) -> Result<OutputInventory> {
    validate_request(request)?;
    ensure_result_capacity(request)?;
    active(cancellation)?;
    let started = Instant::now();
    let selection_digest = selection_digest(request)?;
    let mut total = 0_u64;
    let mut measured = Vec::with_capacity(request.files.len());
    for selected in &request.files {
        active(cancellation)?;
        if started.elapsed() > MAX_DURATION {
            bail!("output_inventory_deadline_exceeded")
        }
        let mut file = open_regular_file_beneath(&request.mounted_root, &selected.relative_path)
            .map_err(|_| anyhow!("output_inventory_file_unavailable"))?;
        let before = fingerprint(&file)?;
        let remaining = request
            .max_bytes
            .checked_sub(total)
            .ok_or_else(|| anyhow!("output_inventory_limits_exceeded"))?;
        let (size_bytes, sha256) =
            measure(&mut file, before.size, remaining, started, cancellation)?;
        let after = fingerprint(&file)?;
        if before != after || size_bytes != before.size {
            bail!("output_inventory_file_changed")
        }
        total = total
            .checked_add(size_bytes)
            .ok_or_else(|| anyhow!("output_inventory_size_overflow"))?;
        if total > request.max_bytes {
            bail!("output_inventory_limits_exceeded")
        }
        measured.push(MeasuredOutput {
            relative_path: selected.relative_path.clone(),
            visibility: selected.visibility.clone(),
            role: selected.role.clone(),
            label: selected.label.clone(),
            media_type: selected.media_type.clone(),
            size_bytes,
            sha256,
        });
    }
    let output = OutputInventory {
        schema_version: INVENTORY_SCHEMA.into(),
        selection_digest,
        files: measured,
    };
    if serde_json::to_vec(&output)
        .map_err(|_| anyhow!("output_inventory_encode_failed"))?
        .len()
        > OUTPUT_LIMIT
    {
        bail!("output_inventory_result_too_large")
    }
    Ok(output)
}

fn validate_request(request: &OutputInventoryRequest) -> Result<()> {
    ensure!(
        request.schema_version == REQUEST_SCHEMA
            && request.mounted_root.is_absolute()
            && request.mounted_root != Path::new("/")
            && request
                .mounted_root
                .components()
                .all(|part| matches!(part, Component::RootDir | Component::Normal(_)))
            && !request.files.is_empty()
            && request.files.len() <= request.max_files
            && request.max_files <= MAX_FILES
            && request.max_bytes > 0
            && request.max_bytes <= MAX_BYTES,
        "output_inventory_request_invalid"
    );
    let mut paths = HashSet::new();
    for file in &request.files {
        ensure!(
            paths.insert(&file.relative_path),
            "output_inventory_duplicate_path"
        );
        validate_selected(file)?;
    }
    Ok(())
}

fn validate_selected(file: &SelectedOutput) -> Result<()> {
    let path = Path::new(&file.relative_path);
    ensure!(
        !file.relative_path.is_empty()
            && !file.relative_path.contains(['\\', '\0'])
            && path
                .components()
                .all(|part| matches!(part, Component::Normal(_)))
            && matches!(file.visibility.as_str(), "researcher" | "operator")
            && matches!(
                file.role.as_str(),
                "request" | "stdout" | "stderr" | "result" | "workspace" | "state" | "evidence"
            )
            && (1..=256).contains(&file.label.chars().count())
            && !file.label.contains('\0')
            && (1..=256).contains(&file.media_type.chars().count())
            && !file.media_type.contains('\0'),
        "output_inventory_selected_metadata_invalid"
    );
    Ok(())
}

fn ensure_result_capacity(request: &OutputInventoryRequest) -> Result<()> {
    let preview = OutputInventory {
        schema_version: INVENTORY_SCHEMA.into(),
        selection_digest: "sha256:".to_owned() + &"0".repeat(64),
        files: request
            .files
            .iter()
            .map(|file| MeasuredOutput {
                relative_path: file.relative_path.clone(),
                visibility: file.visibility.clone(),
                role: file.role.clone(),
                label: file.label.clone(),
                media_type: file.media_type.clone(),
                size_bytes: 0,
                sha256: "0".repeat(64),
            })
            .collect(),
    };
    ensure!(
        serde_json::to_vec(&preview)
            .map_err(|_| anyhow!("output_inventory_encode_failed"))?
            .len()
            <= OUTPUT_LIMIT,
        "output_inventory_result_too_large"
    );
    Ok(())
}

fn selection_digest(request: &OutputInventoryRequest) -> Result<String> {
    #[derive(Serialize)]
    #[serde(rename_all = "camelCase")]
    struct Selection<'a> {
        schema_version: &'a str,
        files: &'a [SelectedOutput],
        max_files: usize,
        max_bytes: u64,
    }
    canonical_document_digest(&Selection {
        schema_version: &request.schema_version,
        files: &request.files,
        max_files: request.max_files,
        max_bytes: request.max_bytes,
    })
    .map_err(|_| anyhow!("output_inventory_selection_digest_failed"))
}

fn measure(
    file: &mut File,
    expected_size: u64,
    remaining: u64,
    started: Instant,
    cancellation: &CancellationToken,
) -> Result<(u64, String)> {
    file.seek(SeekFrom::Start(0))
        .context("output_inventory_seek_failed")?;
    let mut total = 0_u64;
    let mut hash = Sha256::new();
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        active(cancellation)?;
        if started.elapsed() > MAX_DURATION {
            bail!("output_inventory_deadline_exceeded")
        }
        let count = file
            .read(&mut buffer)
            .context("output_inventory_read_failed")?;
        if count == 0 {
            break;
        }
        total = total
            .checked_add(count as u64)
            .ok_or_else(|| anyhow!("output_inventory_size_overflow"))?;
        if total > expected_size || total > remaining {
            bail!("output_inventory_limits_exceeded")
        }
        hash.update(&buffer[..count]);
    }
    if total != expected_size {
        bail!("output_inventory_file_changed")
    }
    Ok((total, hex::encode(hash.finalize())))
}

#[cfg(unix)]
#[derive(Debug, PartialEq, Eq)]
struct Fingerprint {
    dev: u64,
    ino: u64,
    mode: u32,
    nlink: u64,
    size: u64,
    mtime: i64,
    mtime_nsec: i64,
    ctime: i64,
    ctime_nsec: i64,
}

#[cfg(unix)]
fn fingerprint(file: &File) -> Result<Fingerprint> {
    use std::os::unix::fs::MetadataExt;
    let m = file
        .metadata()
        .context("output_inventory_file_metadata_failed")?;
    if !m.is_file() || m.nlink() != 1 {
        bail!("output_inventory_file_shape_invalid")
    }
    Ok(Fingerprint {
        dev: m.dev(),
        ino: m.ino(),
        mode: m.mode(),
        nlink: m.nlink(),
        size: m.len(),
        mtime: m.mtime(),
        mtime_nsec: m.mtime_nsec(),
        ctime: m.ctime(),
        ctime_nsec: m.ctime_nsec(),
    })
}

#[cfg(not(unix))]
fn fingerprint(_: &File) -> Result<()> {
    bail!("output_inventory_requires_unix")
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn detects_in_place_file_tampering_after_open() {
        let temp = TempDir::new().unwrap();
        let path = temp.path().join("output.txt");
        std::fs::write(&path, b"before").unwrap();
        let file = File::open(&path).unwrap();
        let before = fingerprint(&file).unwrap();
        std::fs::write(&path, b"after!").unwrap();
        assert_ne!(before, fingerprint(&file).unwrap());
    }
}

fn active(cancellation: &CancellationToken) -> Result<()> {
    if cancellation.is_cancelled() {
        bail!("output_inventory_cancelled")
    }
    Ok(())
}
