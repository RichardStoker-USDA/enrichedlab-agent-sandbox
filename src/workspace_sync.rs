use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    io::ErrorKind,
    path::{Component, Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use serde_json::json;
use sha2::{Digest, Sha256};
use walkdir::WalkDir;

use crate::{
    audit,
    config::{RuntimeConfig, create_private_dir, set_mode, write_private},
    openshell,
    platform::{GatewayProfile, WorkspaceTransport, profile_for_record},
    state::{self, SandboxRecord},
    workspace::{DECISIONS_TARGET, MountRecord, PRIMARY_TARGET, REQUESTS_TARGET, RUNTIME_TARGET},
};

const MAX_CONTROL_FILES: usize = 256;
const MAX_CONTROL_FILE_BYTES: u64 = 64 * 1024;

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct TreeSummary {
    pub files: u64,
    pub directories: u64,
    pub symlinks: u64,
    pub bytes: u64,
    pub sha256: String,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct SyncReport {
    pub schema_version: u32,
    pub sandbox: String,
    pub direction: String,
    pub target: String,
    pub workspace_transport: WorkspaceTransport,
    pub snapshot: Option<PathBuf>,
    pub backup: Option<PathBuf>,
    pub applied: bool,
    pub summary: TreeSummary,
    pub observed_at_unix: u64,
    pub limitations: Vec<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct DecisionDeliveryReceipt {
    pub schema_version: u32,
    pub sandbox: String,
    pub file: String,
    pub remote_path: String,
    pub canonical_sha256: String,
    pub observed_remote_sha256: String,
    pub bytes: u64,
    pub delivered_at_unix: u64,
    pub verified_at_unix: u64,
    pub verified: bool,
    pub limitations: Vec<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct LegacyDecisionDeliveryReceipt {
    schema_version: u32,
    sandbox: String,
    file: String,
    sha256: String,
    delivered_at_unix: u64,
}

pub fn initialize(config: &RuntimeConfig, record: &SandboxRecord) -> Result<SyncReport> {
    let profile = profile_for_record(config, record)?;
    require_sync(record, &profile)?;
    prepare_remote_layout(config, record)?;
    let report = push(config, record)?;
    push_control_snapshots(config, record)?;
    push_all_decisions(config, record)?;
    Ok(report)
}

pub fn push(config: &RuntimeConfig, record: &SandboxRecord) -> Result<SyncReport> {
    let profile = profile_for_record(config, record)?;
    require_sync(record, &profile)?;
    let summary = validate_tree(
        &record.host_workspace,
        profile.sync_max_files,
        profile.sync_max_bytes,
    )?;
    replace_remote_tree(
        config,
        record,
        &record.host_workspace,
        PRIMARY_TARGET,
        "work",
    )?;
    for mount in user_mounts(record) {
        let mount_summary = validate_tree(
            &mount.host_path,
            profile.sync_max_files,
            profile.sync_max_bytes,
        )?;
        replace_remote_tree(
            config,
            record,
            &mount.host_path,
            &mount.container_path,
            "mount",
        )?;
        audit::append(
            config,
            "workspace-sync",
            "sync.mount.push",
            &format!("sandbox/{}/mount", record.name),
            "passed",
            json!({
                "target": mount.container_path,
                "read_only_source": mount.read_only,
                "summary": mount_summary,
                "copy_semantics": true
            }),
        )?;
    }
    let report = SyncReport {
        schema_version: 1,
        sandbox: record.name.clone(),
        direction: "host-to-sandbox".into(),
        target: PRIMARY_TARGET.into(),
        workspace_transport: record.runtime.workspace_transport,
        snapshot: None,
        backup: None,
        applied: true,
        summary,
        observed_at_unix: unix_now()?,
        limitations: vec![
            "Sync copies protect the host source from direct sandbox writes. A sandbox can modify its remote copy.".into(),
        ],
    };
    save_latest_report(config, &report)?;
    audit::append(
        config,
        "workspace-sync",
        "sync.push",
        &format!("sandbox/{}/workspace", record.name),
        "passed",
        serde_json::to_value(&report)?,
    )?;
    Ok(report)
}

pub fn pull(config: &RuntimeConfig, record: &SandboxRecord, apply: bool) -> Result<SyncReport> {
    let profile = profile_for_record(config, record)?;
    require_sync(record, &profile)?;
    let snapshot = fresh_snapshot_directory(config, &record.name)?;
    let workspace = snapshot.join("workspace");
    create_private_dir(&workspace)?;
    openshell::download(&config.openshell_bin, record, PRIMARY_TARGET, &workspace)?;
    let summary = validate_tree(&workspace, profile.sync_max_files, profile.sync_max_bytes)?;
    let backup = if apply {
        Some(apply_snapshot(config, record, &workspace)?)
    } else {
        None
    };
    let report = SyncReport {
        schema_version: 1,
        sandbox: record.name.clone(),
        direction: "sandbox-to-host".into(),
        target: PRIMARY_TARGET.into(),
        workspace_transport: record.runtime.workspace_transport,
        snapshot: Some(snapshot.clone()),
        backup,
        applied: apply,
        summary,
        observed_at_unix: unix_now()?,
        limitations: vec![
            "The snapshot records transferred filesystem state. It does not prove which process produced each byte.".into(),
        ],
    };
    write_json(&snapshot.join("sync-report.json"), &report)?;
    save_latest_report(config, &report)?;
    audit::append(
        config,
        "workspace-sync",
        if apply {
            "sync.pull.apply"
        } else {
            "sync.pull"
        },
        &format!("sandbox/{}/workspace", record.name),
        "passed",
        serde_json::to_value(&report)?,
    )?;
    Ok(report)
}

pub fn pull_writable_mounts(
    config: &RuntimeConfig,
    record: &SandboxRecord,
) -> Result<Vec<SyncReport>> {
    let profile = profile_for_record(config, record)?;
    require_sync(record, &profile)?;
    let mut reports = Vec::new();
    for mount in user_mounts(record).filter(|mount| !mount.read_only) {
        let snapshot_root = fresh_snapshot_directory(config, &record.name)?;
        let snapshot = snapshot_root.join("mount");
        create_private_dir(&snapshot)?;
        openshell::download(
            &config.openshell_bin,
            record,
            &mount.container_path,
            &snapshot,
        )?;
        let summary = validate_tree(&snapshot, profile.sync_max_files, profile.sync_max_bytes)?;
        let backup = apply_overlay(
            config,
            record,
            &snapshot,
            &mount.host_path,
            &mount.container_path,
        )?;
        let report = SyncReport {
            schema_version: 1,
            sandbox: record.name.clone(),
            direction: "sandbox-to-host-overlay".into(),
            target: mount.container_path.clone(),
            workspace_transport: record.runtime.workspace_transport,
            snapshot: Some(snapshot_root.clone()),
            backup: Some(backup),
            applied: true,
            summary,
            observed_at_unix: unix_now()?,
            limitations: vec![
                "Writable copied inputs use a recoverable overlay on pull. Deletions in the sandbox copy do not delete host files.".into(),
            ],
        };
        write_json(&snapshot_root.join("sync-report.json"), &report)?;
        audit::append(
            config,
            "workspace-sync",
            "sync.mount.pull-overlay",
            &format!("sandbox/{}/mount", record.name),
            "passed",
            serde_json::to_value(&report)?,
        )?;
        reports.push(report);
    }
    Ok(reports)
}

pub fn pull_requests(config: &RuntimeConfig, record: &SandboxRecord) -> Result<usize> {
    if record.runtime.workspace_transport != WorkspaceTransport::Sync {
        return Ok(0);
    }
    let listing = openshell::exec_capture(
        &config.openshell_bin,
        record,
        &[
            "/bin/sh".into(),
            "-lc".into(),
            "find /sandbox/control/requests -mindepth 1 -maxdepth 1 -type f -name '*.json' -printf '%f\\t%s\\n' 2>/dev/null | sort".into(),
        ],
    )?;
    let mut candidates = Vec::new();
    for line in listing.lines().filter(|line| !line.is_empty()) {
        let (name, size) = line
            .split_once('\t')
            .context("remote request listing was malformed")?;
        validate_request_filename(name)?;
        let size: u64 = size.parse().context("remote request size was invalid")?;
        if size > MAX_CONTROL_FILE_BYTES {
            bail!("remote request {name} exceeds 64 KiB");
        }
        candidates.push(name.to_string());
        if candidates.len() > MAX_CONTROL_FILES {
            bail!("remote request inbox exceeds {MAX_CONTROL_FILES} files");
        }
    }
    create_private_dir(&record.control_requests)?;
    let staging = fresh_staging_directory(config, &record.name, "requests")?;
    let mut imported = 0;
    for name in candidates {
        let destination = staging.join(&name);
        openshell::download(
            &config.openshell_bin,
            record,
            &format!("{REQUESTS_TARGET}/{name}"),
            &destination,
        )?;
        let metadata = fs::symlink_metadata(&destination)?;
        if !metadata.is_file() || metadata.len() > MAX_CONTROL_FILE_BYTES {
            bail!("downloaded request {name} failed the regular-file check");
        }
        let target = record.control_requests.join(&name);
        if !target.exists() {
            fs::copy(&destination, &target)?;
            set_mode(&target, 0o600)?;
            imported += 1;
        }
    }
    remove_owned_directory(&staging, &config.sync_dir())?;
    if imported > 0 {
        audit::append(
            config,
            "workspace-sync",
            "sync.requests.pull",
            &format!("sandbox/{}/control/requests", record.name),
            "passed",
            json!({"imported": imported}),
        )?;
    }
    Ok(imported)
}

pub fn push_decision(
    config: &RuntimeConfig,
    record: &SandboxRecord,
    local_path: &Path,
) -> Result<()> {
    if record.runtime.workspace_transport != WorkspaceTransport::Sync {
        return Ok(());
    }
    let name = local_path
        .file_name()
        .and_then(|value| value.to_str())
        .context("decision path has no UTF-8 filename")?;
    validate_request_filename(name)?;
    let metadata = fs::symlink_metadata(local_path)?;
    if !metadata.is_file() || metadata.len() > MAX_CONTROL_FILE_BYTES {
        bail!("decision must be a regular file no larger than 64 KiB");
    }
    let body = fs::read(local_path)?;
    let canonical_sha256 = file_digest(&body);
    let remote_path = format!("{DECISIONS_TARGET}/{name}");
    openshell::upload(
        &config.openshell_bin,
        record,
        local_path,
        &remote_path,
        true,
    )?;
    let observed_remote_sha256 = match verify_remote_decision_copy(
        config,
        record,
        name,
        &canonical_sha256,
        metadata.len(),
    ) {
        Ok(digest) => digest,
        Err(error) => {
            let cleanup = openshell::remove_files(&config.openshell_bin, record, &[&remote_path]);
            audit::append(
                config,
                "workspace-sync",
                "sync.decision.verify",
                &format!("sandbox/{}/control/decisions/{name}", record.name),
                "failed",
                json!({"error_type": "remote_copy_verification_failed"}),
            )?;
            if cleanup.is_err() {
                bail!(
                    "decision delivery verification failed and the unverified remote copy could not be removed"
                );
            }
            return Err(error).context("decision delivery verification failed");
        }
    };
    let now = unix_now()?;
    let receipt = DecisionDeliveryReceipt {
        schema_version: 2,
        sandbox: record.name.clone(),
        file: name.into(),
        remote_path,
        canonical_sha256: canonical_sha256.clone(),
        observed_remote_sha256,
        bytes: metadata.len(),
        delivered_at_unix: now,
        verified_at_unix: now,
        verified: true,
        limitations: vec![
            "The receipt proves a point-in-time download matched the host canonical decision. The sandbox can later modify its remote copy.".into(),
        ],
    };
    save_decision_receipt(config, &receipt)?;
    audit::append(
        config,
        "workspace-sync",
        "sync.decision.push",
        &format!("sandbox/{}/control/decisions/{name}", record.name),
        "passed",
        json!({
            "bytes": metadata.len(),
            "canonical_sha256": canonical_sha256,
            "observed_remote_sha256": receipt.observed_remote_sha256,
            "receipt_schema_version": receipt.schema_version
        }),
    )?;
    Ok(())
}

pub fn push_pending_decisions(config: &RuntimeConfig, record: &SandboxRecord) -> Result<usize> {
    if record.runtime.workspace_transport != WorkspaceTransport::Sync {
        return Ok(0);
    }
    create_private_dir(&record.control_decisions)?;
    let mut paths = fs::read_dir(&record.control_decisions)?
        .filter_map(|entry| entry.ok().map(|entry| entry.path()))
        .filter(|path| path.extension().and_then(|value| value.to_str()) == Some("json"))
        .collect::<Vec<_>>();
    paths.sort();
    if paths.len() > MAX_CONTROL_FILES {
        bail!("decision outbox exceeds {MAX_CONTROL_FILES} files");
    }
    reconcile_current_receipts(
        config,
        record,
        &paths
            .iter()
            .map(|path| {
                path.file_name()
                    .and_then(|value| value.to_str())
                    .map(str::to_string)
                    .context("decision filename is not UTF-8")
            })
            .collect::<Result<BTreeSet<_>>>()?,
    )?;
    for path in &paths {
        let name = path
            .file_name()
            .and_then(|value| value.to_str())
            .context("decision filename is not UTF-8")?;
        let body = fs::read(path)?;
        let digest = file_digest(&body);
        let delivered =
            load_decision_receipt_path(&decision_receipt_path(config, &record.name, name))?
                .is_some_and(|receipt| {
                    receipt.verified
                        && receipt.sandbox == record.name
                        && receipt.file == name
                        && receipt.canonical_sha256 == digest
                        && receipt.observed_remote_sha256 == digest
                        && receipt.bytes == body.len() as u64
                });
        if !delivered {
            push_decision(config, record, path)?;
        }
    }
    Ok(paths.len())
}

pub fn load_decision_receipts(config: &RuntimeConfig) -> Result<Vec<DecisionDeliveryReceipt>> {
    let root = config.sync_dir().join("delivery");
    create_private_dir(&root)?;
    let mut receipts = Vec::new();
    for sandbox_entry in fs::read_dir(&root)? {
        let sandbox_entry = sandbox_entry?;
        if !sandbox_entry.file_type()?.is_dir() {
            bail!("decision receipt root contains a non-directory entry");
        }
        let sandbox = sandbox_entry
            .file_name()
            .into_string()
            .map_err(|_| anyhow::anyhow!("decision receipt sandbox name is not UTF-8"))?;
        state::validate_name(&sandbox)?;
        for entry in fs::read_dir(sandbox_entry.path())? {
            let entry = entry?;
            if !entry.file_type()?.is_file() {
                bail!("decision receipt directory contains a non-file entry");
            }
            let name = entry
                .file_name()
                .into_string()
                .map_err(|_| anyhow::anyhow!("decision receipt filename is not UTF-8"))?;
            validate_request_filename(&name)?;
            let receipt = load_decision_receipt_path(&entry.path())?
                .context("decision receipt disappeared during load")?;
            if receipt.sandbox != sandbox || receipt.file != name {
                bail!("decision receipt path identity does not match its contents");
            }
            receipts.push(receipt);
            if receipts.len() > MAX_CONTROL_FILES * 64 {
                bail!("decision receipt inventory exceeds the controller limit");
            }
        }
    }
    receipts.sort_by(|left, right| {
        left.sandbox
            .cmp(&right.sandbox)
            .then_with(|| left.file.cmp(&right.file))
    });
    Ok(receipts)
}

pub fn verify_decision_receipts(
    config: &RuntimeConfig,
    record: &SandboxRecord,
) -> Result<Vec<DecisionDeliveryReceipt>> {
    if record.runtime.workspace_transport != WorkspaceTransport::Sync {
        return Ok(Vec::new());
    }
    let result = verify_decision_receipts_inner(config, record);
    let (outcome, details) = match &result {
        Ok(receipts) => ("passed", json!({"verified": receipts.len()})),
        Err(_) => (
            "failed",
            json!({"error_type": "decision_copy_integrity_failed"}),
        ),
    };
    audit::append(
        config,
        "workspace-sync",
        "sync.decisions.verify",
        &format!("sandbox/{}/control/decisions", record.name),
        outcome,
        details,
    )?;
    result
}

fn verify_decision_receipts_inner(
    config: &RuntimeConfig,
    record: &SandboxRecord,
) -> Result<Vec<DecisionDeliveryReceipt>> {
    create_private_dir(&record.control_decisions)?;
    let mut paths = fs::read_dir(&record.control_decisions)?
        .filter_map(|entry| entry.ok().map(|entry| entry.path()))
        .filter(|path| path.extension().and_then(|value| value.to_str()) == Some("json"))
        .collect::<Vec<_>>();
    paths.sort();
    if paths.len() > MAX_CONTROL_FILES {
        bail!("decision outbox exceeds {MAX_CONTROL_FILES} files");
    }
    let local_names = paths
        .iter()
        .map(|path| {
            path.file_name()
                .and_then(|value| value.to_str())
                .map(str::to_string)
                .context("decision filename is not UTF-8")
        })
        .collect::<Result<Vec<_>>>()?;
    let remote_files = remote_decision_files(config, record)?;
    let remote_names = remote_files
        .iter()
        .map(|file| file.name.clone())
        .collect::<Vec<_>>();
    if local_names != remote_names {
        bail!("remote decision inventory does not match the host canonical outbox");
    }
    let mut verified = Vec::new();
    for ((path, name), remote_file) in paths.iter().zip(local_names).zip(remote_files) {
        validate_request_filename(&name)?;
        let metadata = fs::symlink_metadata(path)?;
        if !metadata.is_file() || metadata.len() > MAX_CONTROL_FILE_BYTES {
            bail!("canonical decision is not a bounded regular file");
        }
        if remote_file.bytes != metadata.len() {
            bail!("remote decision size does not match the host canonical decision");
        }
        let canonical_sha256 = file_digest(&fs::read(path)?);
        let mut receipt =
            load_decision_receipt_path(&decision_receipt_path(config, &record.name, &name))?
                .context("canonical decision has no delivery receipt")?;
        if !receipt.verified
            || receipt.sandbox != record.name
            || receipt.file != name
            || receipt.remote_path != format!("{DECISIONS_TARGET}/{name}")
            || receipt.canonical_sha256 != canonical_sha256
            || receipt.observed_remote_sha256 != canonical_sha256
            || receipt.bytes != metadata.len()
        {
            bail!("decision delivery receipt does not match the host canonical decision");
        }
        let observed =
            verify_remote_decision_copy(config, record, &name, &canonical_sha256, metadata.len())?;
        receipt.observed_remote_sha256 = observed;
        receipt.verified_at_unix = unix_now()?;
        save_decision_receipt(config, &receipt)?;
        verified.push(receipt);
    }
    Ok(verified)
}

pub fn push_mount(
    config: &RuntimeConfig,
    record: &SandboxRecord,
    mount: &MountRecord,
) -> Result<TreeSummary> {
    let profile = profile_for_record(config, record)?;
    require_sync(record, &profile)?;
    if is_control_mount(mount) {
        bail!("control mounts use their dedicated sync path");
    }
    let summary = validate_tree(
        &mount.host_path,
        profile.sync_max_files,
        profile.sync_max_bytes,
    )?;
    replace_remote_tree(
        config,
        record,
        &mount.host_path,
        &mount.container_path,
        "mount",
    )?;
    Ok(summary)
}

pub fn load_latest_reports(config: &RuntimeConfig) -> Result<BTreeMap<String, SyncReport>> {
    let mut reports = BTreeMap::new();
    let directory = config.sync_dir().join("reports");
    create_private_dir(&directory)?;
    for entry in fs::read_dir(directory)? {
        let path = entry?.path();
        if path.extension().and_then(|value| value.to_str()) != Some("json") {
            continue;
        }
        let report: SyncReport = serde_json::from_slice(&fs::read(&path)?)
            .with_context(|| format!("invalid sync report {}", path.display()))?;
        reports.insert(report.sandbox.clone(), report);
    }
    Ok(reports)
}

fn prepare_remote_layout(config: &RuntimeConfig, record: &SandboxRecord) -> Result<()> {
    openshell::exec_capture_at(
        &config.openshell_bin,
        record,
        "/sandbox",
        &[
            "/bin/sh".into(),
            "-lc".into(),
            "set -eu; mkdir -p /sandbox/work /sandbox/mounts /sandbox/control/requests /sandbox/control/decisions /sandbox/control/runtime /sandbox/control/.sync".into(),
        ],
    )?;
    Ok(())
}

fn push_control_snapshots(config: &RuntimeConfig, record: &SandboxRecord) -> Result<()> {
    for (source, target, label) in [
        (&record.control_decisions, DECISIONS_TARGET, "decisions"),
        (
            &state::control_paths(config, &record.name)?.2,
            RUNTIME_TARGET,
            "runtime",
        ),
    ] {
        validate_tree(source, MAX_CONTROL_FILES as u64, 16 * 1024 * 1024)?;
        replace_remote_tree(config, record, source, target, label)?;
    }
    Ok(())
}

fn push_all_decisions(config: &RuntimeConfig, record: &SandboxRecord) -> Result<()> {
    create_private_dir(&record.control_decisions)?;
    let mut paths = fs::read_dir(&record.control_decisions)?
        .filter_map(|entry| entry.ok().map(|entry| entry.path()))
        .filter(|path| path.extension().and_then(|value| value.to_str()) == Some("json"))
        .collect::<Vec<_>>();
    paths.sort();
    if paths.len() > MAX_CONTROL_FILES {
        bail!("decision outbox exceeds {MAX_CONTROL_FILES} files");
    }
    let canonical_names = paths
        .iter()
        .map(|path| {
            path.file_name()
                .and_then(|value| value.to_str())
                .map(str::to_string)
                .context("decision filename is not UTF-8")
        })
        .collect::<Result<BTreeSet<_>>>()?;
    reconcile_current_receipts(config, record, &canonical_names)?;
    for path in paths {
        push_decision(config, record, &path)?;
    }
    Ok(())
}

#[derive(Clone, Debug)]
struct RemoteDecisionFile {
    name: String,
    bytes: u64,
}

fn remote_decision_files(
    config: &RuntimeConfig,
    record: &SandboxRecord,
) -> Result<Vec<RemoteDecisionFile>> {
    let listing = openshell::exec_capture(
        &config.openshell_bin,
        record,
        &[
            "/bin/sh".into(),
            "-lc".into(),
            "find /sandbox/control/decisions -mindepth 1 -maxdepth 1 -printf '%y\\t%f\\t%s\\n' 2>/dev/null | sort".into(),
        ],
    )?;
    let mut files = Vec::new();
    for line in listing.lines().filter(|line| !line.is_empty()) {
        let mut fields = line.split('\t');
        let kind = fields
            .next()
            .context("remote decision listing was malformed")?;
        let name = fields
            .next()
            .context("remote decision listing was malformed")?;
        let size = fields
            .next()
            .context("remote decision listing was malformed")?;
        if fields.next().is_some() || kind != "f" {
            bail!("remote decision inventory contains an unsupported entry");
        }
        validate_request_filename(name)?;
        let bytes: u64 = size
            .parse()
            .context("remote decision size was not a valid integer")?;
        if bytes > MAX_CONTROL_FILE_BYTES {
            bail!("remote decision exceeds 64 KiB");
        }
        files.push(RemoteDecisionFile {
            name: name.into(),
            bytes,
        });
        if files.len() > MAX_CONTROL_FILES {
            bail!("remote decision inventory exceeds {MAX_CONTROL_FILES} files");
        }
    }
    for pair in files.windows(2) {
        if pair[0].name == pair[1].name {
            bail!("remote decision inventory contains a duplicate filename");
        }
    }
    Ok(files)
}

fn verify_remote_decision_copy(
    config: &RuntimeConfig,
    record: &SandboxRecord,
    name: &str,
    expected_sha256: &str,
    expected_bytes: u64,
) -> Result<String> {
    validate_request_filename(name)?;
    validate_sha256(expected_sha256)?;
    if expected_bytes > MAX_CONTROL_FILE_BYTES {
        bail!("expected decision size exceeds the controller limit");
    }
    let staging = fresh_staging_directory(config, &record.name, "decision-verify")?;
    let destination = staging.join(name);
    let verification = (|| -> Result<String> {
        openshell::download(
            &config.openshell_bin,
            record,
            &format!("{DECISIONS_TARGET}/{name}"),
            &destination,
        )?;
        let metadata = fs::symlink_metadata(&destination)?;
        if !metadata.is_file()
            || metadata.file_type().is_symlink()
            || metadata.len() > MAX_CONTROL_FILE_BYTES
            || metadata.len() != expected_bytes
        {
            bail!("downloaded decision failed its bounded regular-file check");
        }
        let observed_sha256 = file_digest(&fs::read(&destination)?);
        if observed_sha256 != expected_sha256 {
            bail!("downloaded decision digest does not match the host canonical decision");
        }
        Ok(observed_sha256)
    })();
    let cleanup = remove_owned_directory(&staging, &config.sync_dir());
    match (verification, cleanup) {
        (Ok(digest), Ok(())) => Ok(digest),
        (Err(error), Ok(())) => Err(error),
        (Ok(_), Err(_)) => bail!("decision verification staging cleanup failed"),
        (Err(error), Err(_)) => {
            Err(error).context("decision verification failed and staging cleanup also failed")
        }
    }
}

fn file_digest(body: &[u8]) -> String {
    format!("sha256:{}", hex::encode(Sha256::digest(body)))
}

fn validate_sha256(value: &str) -> Result<()> {
    let digest = value
        .strip_prefix("sha256:")
        .context("digest must use the sha256 prefix")?;
    if digest.len() != 64 || !digest.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        bail!("digest is not a SHA-256 value");
    }
    Ok(())
}

fn decision_receipt_path(config: &RuntimeConfig, sandbox: &str, name: &str) -> PathBuf {
    config.sync_dir().join("delivery").join(sandbox).join(name)
}

fn load_decision_receipt_path(path: &Path) -> Result<Option<DecisionDeliveryReceipt>> {
    let metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error.into()),
    };
    if !metadata.is_file()
        || metadata.file_type().is_symlink()
        || metadata.len() > MAX_CONTROL_FILE_BYTES
    {
        bail!("decision receipt is not a bounded regular file");
    }
    let body = fs::read(path)?;
    let value: serde_json::Value = serde_json::from_slice(&body)
        .with_context(|| format!("invalid decision receipt {}", path.display()))?;
    let schema_version = value
        .get("schema_version")
        .and_then(serde_json::Value::as_u64)
        .context("decision receipt has no numeric schema version")?;
    let receipt = match schema_version {
        1 => {
            let legacy: LegacyDecisionDeliveryReceipt = serde_json::from_value(value)?;
            if legacy.schema_version != 1 {
                bail!("legacy decision receipt schema is invalid");
            }
            DecisionDeliveryReceipt {
                schema_version: 1,
                sandbox: legacy.sandbox,
                file: legacy.file.clone(),
                remote_path: format!("{DECISIONS_TARGET}/{}", legacy.file),
                canonical_sha256: legacy.sha256,
                observed_remote_sha256: String::new(),
                bytes: 0,
                delivered_at_unix: legacy.delivered_at_unix,
                verified_at_unix: 0,
                verified: false,
                limitations: vec![
                    "Legacy receipt: the remote sandbox copy was not downloaded and verified."
                        .into(),
                ],
            }
        }
        2 => serde_json::from_value(value)?,
        _ => bail!("unsupported decision receipt schema {schema_version}"),
    };
    validate_decision_receipt(&receipt)?;
    Ok(Some(receipt))
}

fn validate_decision_receipt(receipt: &DecisionDeliveryReceipt) -> Result<()> {
    if !(1..=2).contains(&receipt.schema_version) {
        bail!("unsupported decision receipt schema");
    }
    state::validate_name(&receipt.sandbox)?;
    validate_request_filename(&receipt.file)?;
    validate_sha256(&receipt.canonical_sha256)?;
    if receipt.remote_path != format!("{DECISIONS_TARGET}/{}", receipt.file) {
        bail!("decision receipt remote path does not match its identity");
    }
    if receipt.bytes > MAX_CONTROL_FILE_BYTES
        || receipt.delivered_at_unix == 0
        || receipt.verified_at_unix > unix_now()?.saturating_add(300)
        || receipt.limitations.len() > 8
        || receipt.limitations.iter().any(|value| {
            value.is_empty()
                || value.len() > 512
                || value
                    .chars()
                    .any(|character| character == '\0' || character.is_control())
        })
    {
        bail!("decision receipt fields are invalid");
    }
    if receipt.schema_version == 1 {
        if receipt.verified
            || receipt.verified_at_unix != 0
            || !receipt.observed_remote_sha256.is_empty()
            || receipt.bytes != 0
        {
            bail!("legacy decision receipt contains unsupported verification claims");
        }
    } else {
        validate_sha256(&receipt.observed_remote_sha256)?;
        if !receipt.verified
            || receipt.bytes == 0
            || receipt.verified_at_unix < receipt.delivered_at_unix
            || receipt.canonical_sha256 != receipt.observed_remote_sha256
        {
            bail!("verified decision receipt does not bind matching copies");
        }
    }
    Ok(())
}

fn save_decision_receipt(config: &RuntimeConfig, receipt: &DecisionDeliveryReceipt) -> Result<()> {
    if receipt.schema_version != 2 {
        bail!("only verified schema 2 decision receipts can be saved");
    }
    validate_decision_receipt(receipt)?;
    let request_id = receipt
        .file
        .strip_suffix(".json")
        .context("decision receipt filename has no JSON suffix")?;
    let history = config
        .sync_dir()
        .join("delivery-history")
        .join(&receipt.sandbox)
        .join(request_id)
        .join(format!(
            "{}-{}.json",
            receipt.verified_at_unix,
            hex::encode(random_bytes()?)
        ));
    if history.exists() {
        bail!("decision receipt history path collision");
    }
    write_json(&history, receipt)?;
    write_json(
        &decision_receipt_path(config, &receipt.sandbox, &receipt.file),
        receipt,
    )
}

fn reconcile_current_receipts(
    config: &RuntimeConfig,
    record: &SandboxRecord,
    canonical_names: &BTreeSet<String>,
) -> Result<()> {
    let directory = config.sync_dir().join("delivery").join(&record.name);
    create_private_dir(&directory)?;
    let mut removed = 0_u64;
    for entry in fs::read_dir(&directory)? {
        let entry = entry?;
        if !entry.file_type()?.is_file() {
            bail!("decision receipt directory contains a non-file entry");
        }
        let name = entry
            .file_name()
            .into_string()
            .map_err(|_| anyhow::anyhow!("decision receipt filename is not UTF-8"))?;
        validate_request_filename(&name)?;
        let receipt = load_decision_receipt_path(&entry.path())?
            .context("decision receipt disappeared during reconciliation")?;
        if receipt.sandbox != record.name || receipt.file != name {
            bail!("decision receipt path identity does not match its contents");
        }
        if !canonical_names.contains(&name) {
            fs::remove_file(entry.path())?;
            removed += 1;
        }
    }
    if removed > 0 {
        audit::append(
            config,
            "workspace-sync",
            "sync.decision.receipt-retire",
            &format!("sandbox/{}/control/decisions", record.name),
            "passed",
            json!({"retired_current_receipts": removed, "history_retained": true}),
        )?;
    }
    Ok(())
}

fn replace_remote_tree(
    config: &RuntimeConfig,
    record: &SandboxRecord,
    source: &Path,
    target: &str,
    label: &str,
) -> Result<()> {
    validate_remote_target(target)?;
    let nonce = hex::encode(random_bytes()?);
    let incoming = format!("/sandbox/control/.sync/{label}-{nonce}");
    let create = format!("set -eu; rm -rf -- {incoming}; mkdir -p -- {incoming}");
    openshell::exec_capture(
        &config.openshell_bin,
        record,
        &["/bin/sh".into(), "-lc".into(), create.into()],
    )?;
    if let Err(error) = openshell::upload_contents(
        &config.openshell_bin,
        record,
        source,
        &format!("{incoming}/"),
    ) {
        let _ = openshell::exec_capture(
            &config.openshell_bin,
            record,
            &[
                "/bin/sh".into(),
                "-lc".into(),
                format!("rm -rf -- {incoming}").into(),
            ],
        );
        return Err(error).context("remote sync upload failed before activation");
    }
    let activate = format!(
        "set -eu; mkdir -p -- {target}; find {target} -mindepth 1 -maxdepth 1 -exec rm -rf -- {{}} +; cp -a -- {incoming}/. {target}/; rm -rf -- {incoming}"
    );
    openshell::exec_capture(
        &config.openshell_bin,
        record,
        &["/bin/sh".into(), "-lc".into(), activate.into()],
    )?;
    Ok(())
}

fn apply_snapshot(
    config: &RuntimeConfig,
    record: &SandboxRecord,
    snapshot: &Path,
) -> Result<PathBuf> {
    let profile = profile_for_record(config, record)?;
    validate_tree(snapshot, profile.sync_max_files, profile.sync_max_bytes)?;
    let parent = record
        .host_workspace
        .parent()
        .context("host workspace has no parent")?;
    let nonce = hex::encode(random_bytes()?);
    let replacement = parent.join(format!(".agent-sandbox-replacement-{nonce}"));
    let retired = parent.join(format!(".agent-sandbox-retired-{nonce}"));
    if replacement.exists() || retired.exists() {
        bail!("sync replacement path collision");
    }
    copy_tree(snapshot, &replacement)?;
    let backup = config
        .sync_backups_dir()
        .join(&record.name)
        .join(format!("{}-{nonce}", unix_now()?))
        .join("workspace");
    copy_tree(&record.host_workspace, &backup)
        .context("failed to create the recoverable host workspace backup")?;
    fs::rename(&record.host_workspace, &retired)
        .context("failed to stage the current host workspace")?;
    if let Err(error) = fs::rename(&replacement, &record.host_workspace) {
        let _ = fs::rename(&retired, &record.host_workspace);
        return Err(error).context("failed to activate the pulled workspace; original restored");
    }
    remove_owned_directory(&retired, parent)?;
    Ok(backup)
}

fn apply_overlay(
    config: &RuntimeConfig,
    record: &SandboxRecord,
    snapshot: &Path,
    destination: &Path,
    target_label: &str,
) -> Result<PathBuf> {
    let profile = profile_for_record(config, record)?;
    validate_tree(snapshot, profile.sync_max_files, profile.sync_max_bytes)?;
    validate_tree(destination, profile.sync_max_files, profile.sync_max_bytes)?;
    let parent = destination.parent().context("mount source has no parent")?;
    let nonce = hex::encode(random_bytes()?);
    let replacement = parent.join(format!(".agent-sandbox-overlay-{nonce}"));
    let retired = parent.join(format!(".agent-sandbox-retired-{nonce}"));
    copy_tree(destination, &replacement)?;
    overlay_tree(snapshot, &replacement)?;
    let label_digest = hex::encode(Sha256::digest(target_label.as_bytes()));
    let backup = config
        .sync_backups_dir()
        .join(&record.name)
        .join(format!("{}-{nonce}", unix_now()?))
        .join(format!("mount-{}", &label_digest[..12]));
    copy_tree(destination, &backup)
        .context("failed to create the recoverable copied-input backup")?;
    fs::rename(destination, &retired).context("failed to stage the copied input")?;
    if let Err(error) = fs::rename(&replacement, destination) {
        let _ = fs::rename(&retired, destination);
        return Err(error).context("failed to activate copied input overlay; original restored");
    }
    remove_owned_directory(&retired, parent)?;
    Ok(backup)
}

fn user_mounts(record: &SandboxRecord) -> impl Iterator<Item = &MountRecord> {
    record
        .mounts
        .iter()
        .filter(|mount| !is_control_mount(mount))
}

fn is_control_mount(mount: &MountRecord) -> bool {
    matches!(
        mount.container_path.as_str(),
        REQUESTS_TARGET | DECISIONS_TARGET | RUNTIME_TARGET
    )
}

fn require_sync(record: &SandboxRecord, profile: &GatewayProfile) -> Result<()> {
    if record.runtime.workspace_transport != WorkspaceTransport::Sync
        || profile.workspace_transport != WorkspaceTransport::Sync
    {
        bail!(
            "sandbox {} does not use workspace sync transport",
            record.name
        );
    }
    Ok(())
}

fn validate_remote_target(target: &str) -> Result<()> {
    if target != PRIMARY_TARGET
        && target != REQUESTS_TARGET
        && target != DECISIONS_TARGET
        && target != RUNTIME_TARGET
        && !target.starts_with("/sandbox/mounts/")
    {
        bail!("remote sync target is outside the controller namespace");
    }
    if target.contains([
        '\0', '\n', '\r', ' ', '\'', '"', '`', '$', ';', '&', '|', '<', '>', '(', ')',
    ]) || target.contains("..")
    {
        bail!("remote sync target contains unsafe characters");
    }
    Ok(())
}

fn validate_request_filename(name: &str) -> Result<()> {
    let id = name
        .strip_suffix(".json")
        .context("control filename must end in .json")?;
    uuid::Uuid::parse_str(id).context("control filename must be a UUID")?;
    Ok(())
}

fn validate_tree(root: &Path, max_files: u64, max_bytes: u64) -> Result<TreeSummary> {
    let root = fs::canonicalize(root)
        .with_context(|| format!("failed to resolve sync source {}", root.display()))?;
    if !root.is_dir() {
        bail!("sync source is not a directory: {}", root.display());
    }
    let mut files = 0_u64;
    let mut directories = 0_u64;
    let mut symlinks = 0_u64;
    let mut bytes = 0_u64;
    let mut digest = Sha256::new();
    for entry in WalkDir::new(&root).follow_links(false).sort_by_file_name() {
        let entry = entry?;
        let path = entry.path();
        if path == root {
            continue;
        }
        let relative = path.strip_prefix(&root)?;
        validate_relative(relative)?;
        let metadata = fs::symlink_metadata(path)?;
        digest.update(relative.to_string_lossy().as_bytes());
        digest.update([0]);
        if metadata.is_dir() {
            directories += 1;
            digest.update(b"d");
        } else if metadata.is_file() {
            files += 1;
            bytes = bytes
                .checked_add(metadata.len())
                .context("sync byte count overflow")?;
            digest.update(b"f");
            digest.update(metadata.len().to_le_bytes());
            let mut file = fs::File::open(path)?;
            std::io::copy(&mut file, &mut DigestWriter(&mut digest))?;
        } else if metadata.file_type().is_symlink() {
            symlinks += 1;
            let target = fs::read_link(path)?;
            validate_symlink_target(relative, &target)?;
            digest.update(b"l");
            digest.update(target.to_string_lossy().as_bytes());
        } else {
            bail!("sync source contains a special file: {}", path.display());
        }
        if files + symlinks > max_files {
            bail!("sync source exceeds the {max_files} file limit");
        }
        if bytes > max_bytes {
            bail!("sync source exceeds the {max_bytes} byte limit");
        }
    }
    Ok(TreeSummary {
        files,
        directories,
        symlinks,
        bytes,
        sha256: format!("sha256:{}", hex::encode(digest.finalize())),
    })
}

struct DigestWriter<'a>(&'a mut Sha256);

impl std::io::Write for DigestWriter<'_> {
    fn write(&mut self, buffer: &[u8]) -> std::io::Result<usize> {
        self.0.update(buffer);
        Ok(buffer.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

fn validate_relative(path: &Path) -> Result<()> {
    if path.as_os_str().is_empty()
        || path.is_absolute()
        || path.components().any(|component| {
            matches!(
                component,
                Component::ParentDir | Component::RootDir | Component::Prefix(_)
            )
        })
    {
        bail!("sync path is not a safe relative path: {}", path.display());
    }
    Ok(())
}

fn validate_symlink_target(link_relative: &Path, target: &Path) -> Result<()> {
    if target.is_absolute() {
        bail!("sync source contains an absolute symlink");
    }
    let mut depth = link_relative.parent().map_or(0_i64, |path| {
        path.components()
            .filter(|component| matches!(component, Component::Normal(_)))
            .count() as i64
    });
    for component in target.components() {
        match component {
            Component::CurDir => {}
            Component::Normal(_) => depth += 1,
            Component::ParentDir => {
                depth -= 1;
                if depth < 0 {
                    bail!("sync source contains a symlink that escapes its root");
                }
            }
            Component::RootDir | Component::Prefix(_) => {
                bail!("sync source contains an invalid symlink")
            }
        }
    }
    Ok(())
}

fn copy_tree(source: &Path, destination: &Path) -> Result<()> {
    if destination.exists() {
        bail!("copy destination already exists: {}", destination.display());
    }
    create_private_dir(destination)?;
    for entry in WalkDir::new(source).follow_links(false).sort_by_file_name() {
        let entry = entry?;
        let relative = entry.path().strip_prefix(source)?;
        if relative.as_os_str().is_empty() {
            continue;
        }
        validate_relative(relative)?;
        let target = destination.join(relative);
        let metadata = fs::symlink_metadata(entry.path())?;
        if metadata.is_dir() {
            fs::create_dir(&target)?;
            copy_permissions(entry.path(), &target)?;
        } else if metadata.is_file() {
            if let Some(parent) = target.parent() {
                fs::create_dir_all(parent)?;
            }
            fs::copy(entry.path(), &target)?;
            copy_permissions(entry.path(), &target)?;
        } else if metadata.file_type().is_symlink() {
            let link_target = fs::read_link(entry.path())?;
            validate_symlink_target(relative, &link_target)?;
            create_symlink(&link_target, &target, entry.path())?;
        } else {
            bail!("copy source contains a special file");
        }
    }
    copy_permissions(source, destination)?;
    Ok(())
}

fn overlay_tree(source: &Path, destination: &Path) -> Result<()> {
    for entry in WalkDir::new(source).follow_links(false).sort_by_file_name() {
        let entry = entry?;
        let relative = entry.path().strip_prefix(source)?;
        if relative.as_os_str().is_empty() {
            continue;
        }
        validate_relative(relative)?;
        let target = destination.join(relative);
        let metadata = fs::symlink_metadata(entry.path())?;
        if metadata.is_dir() {
            if target.exists() && !target.is_dir() {
                remove_overlay_target(&target, destination)?;
            }
            fs::create_dir_all(&target)?;
            copy_permissions(entry.path(), &target)?;
        } else if metadata.is_file() {
            if target.exists() && !target.is_file() {
                remove_overlay_target(&target, destination)?;
            }
            if let Some(parent) = target.parent() {
                fs::create_dir_all(parent)?;
            }
            fs::copy(entry.path(), &target)?;
            copy_permissions(entry.path(), &target)?;
        } else if metadata.file_type().is_symlink() {
            let link_target = fs::read_link(entry.path())?;
            validate_symlink_target(relative, &link_target)?;
            if fs::symlink_metadata(&target).is_ok() {
                remove_overlay_target(&target, destination)?;
            }
            create_symlink(&link_target, &target, entry.path())?;
        } else {
            bail!("overlay source contains a special file");
        }
    }
    Ok(())
}

fn remove_overlay_target(path: &Path, owned_root: &Path) -> Result<()> {
    if !path.starts_with(owned_root) || path == owned_root {
        bail!("refusing to replace a path outside the owned overlay tree");
    }
    let metadata = fs::symlink_metadata(path)?;
    if metadata.is_dir() && !metadata.file_type().is_symlink() {
        fs::remove_dir_all(path)?;
    } else {
        fs::remove_file(path)?;
    }
    Ok(())
}

fn copy_permissions(source: &Path, destination: &Path) -> Result<()> {
    fs::set_permissions(destination, fs::metadata(source)?.permissions())?;
    Ok(())
}

#[cfg(unix)]
fn create_symlink(target: &Path, destination: &Path, _source: &Path) -> Result<()> {
    std::os::unix::fs::symlink(target, destination)?;
    Ok(())
}

#[cfg(windows)]
fn create_symlink(target: &Path, destination: &Path, source: &Path) -> Result<()> {
    if fs::metadata(source)?.is_dir() {
        std::os::windows::fs::symlink_dir(target, destination)?;
    } else {
        std::os::windows::fs::symlink_file(target, destination)?;
    }
    Ok(())
}

fn fresh_snapshot_directory(config: &RuntimeConfig, sandbox: &str) -> Result<PathBuf> {
    state::validate_name(sandbox)?;
    let path = config
        .sync_dir()
        .join("snapshots")
        .join(sandbox)
        .join(format!("{}-{}", unix_now()?, hex::encode(random_bytes()?)));
    create_private_dir(&path)?;
    Ok(path)
}

fn fresh_staging_directory(config: &RuntimeConfig, sandbox: &str, label: &str) -> Result<PathBuf> {
    state::validate_name(sandbox)?;
    let path = config.sync_dir().join("staging").join(format!(
        "{sandbox}-{label}-{}",
        hex::encode(random_bytes()?)
    ));
    create_private_dir(&path)?;
    Ok(path)
}

fn save_latest_report(config: &RuntimeConfig, report: &SyncReport) -> Result<()> {
    state::validate_name(&report.sandbox)?;
    write_json(
        &config
            .sync_dir()
            .join("reports")
            .join(format!("{}.json", report.sandbox)),
        report,
    )
}

fn write_json(path: &Path, value: &impl Serialize) -> Result<()> {
    let mut body = serde_json::to_vec_pretty(value)?;
    body.push(b'\n');
    write_private(path, &body)
}

fn remove_owned_directory(path: &Path, allowed_parent: &Path) -> Result<()> {
    let parent = path.parent().context("owned directory has no parent")?;
    let allowed_parent = fs::canonicalize(allowed_parent)
        .with_context(|| format!("failed to resolve {}", allowed_parent.display()))?;
    let parent = fs::canonicalize(parent)
        .with_context(|| format!("failed to resolve {}", parent.display()))?;
    if !parent.starts_with(&allowed_parent) || path == allowed_parent {
        bail!("refusing to remove a directory outside controller custody");
    }
    fs::remove_dir_all(path)?;
    Ok(())
}

fn random_bytes() -> Result<[u8; 6]> {
    let mut bytes = [0_u8; 6];
    getrandom::fill(&mut bytes).context("operating system randomness unavailable")?;
    Ok(bytes)
}

fn unix_now() -> Result<u64> {
    Ok(SystemTime::now().duration_since(UNIX_EPOCH)?.as_secs())
}

#[cfg(test)]
mod tests {
    use std::{fs, path::Path};

    use tempfile::tempdir;

    use crate::config::RuntimeConfig;

    use super::{
        DecisionDeliveryReceipt, copy_tree, file_digest, load_decision_receipt_path,
        save_decision_receipt, validate_remote_target, validate_symlink_target, validate_tree,
    };

    fn test_config(root: &Path) -> RuntimeConfig {
        RuntimeConfig {
            root: root.join("workspaces"),
            state_dir: root.join("state"),
            config_dir: root.join("config"),
            openshell_bin: root.join("openshell"),
            docker_bin: root.join("docker"),
        }
    }

    #[test]
    fn tree_summary_is_stable_and_copy_preserves_content() {
        let temp = tempdir().unwrap();
        let source = temp.path().join("source");
        fs::create_dir(&source).unwrap();
        fs::create_dir(source.join("nested")).unwrap();
        fs::write(source.join("nested/file.txt"), b"hello").unwrap();
        let first = validate_tree(&source, 10, 1024).unwrap();
        let destination = temp.path().join("destination");
        copy_tree(&source, &destination).unwrap();
        let second = validate_tree(&destination, 10, 1024).unwrap();
        assert_eq!(first.sha256, second.sha256);
        assert_eq!(
            fs::read(destination.join("nested/file.txt")).unwrap(),
            b"hello"
        );
    }

    #[test]
    fn unsafe_targets_and_escaping_symlinks_are_rejected() {
        assert!(validate_remote_target("/sandbox/work").is_ok());
        assert!(validate_remote_target("/etc").is_err());
        assert!(validate_remote_target("/sandbox/mounts/a;id").is_err());
        assert!(
            validate_symlink_target(Path::new("nested/link"), Path::new("../../outside")).is_err()
        );
    }

    #[test]
    fn legacy_receipt_is_loaded_without_a_remote_verification_claim() {
        let temp = tempdir().unwrap();
        let path = temp.path().join("legacy.json");
        let request_id = "8209d4a2-6acd-45ef-b491-f033548de100";
        fs::write(
            &path,
            format!(
                "{{\"schema_version\":1,\"sandbox\":\"example\",\"file\":\"{request_id}.json\",\"sha256\":\"{}\",\"delivered_at_unix\":1}}",
                file_digest(b"decision")
            ),
        )
        .unwrap();
        let receipt = load_decision_receipt_path(&path).unwrap().unwrap();
        assert_eq!(receipt.schema_version, 1);
        assert!(!receipt.verified);
        assert_eq!(receipt.verified_at_unix, 0);
        assert!(receipt.observed_remote_sha256.is_empty());
    }

    #[test]
    fn verified_receipt_keeps_current_and_append_only_history_copies() {
        let temp = tempdir().unwrap();
        let config = test_config(temp.path());
        let digest = file_digest(b"decision");
        let receipt = DecisionDeliveryReceipt {
            schema_version: 2,
            sandbox: "example".into(),
            file: "8209d4a2-6acd-45ef-b491-f033548de100.json".into(),
            remote_path: "/sandbox/control/decisions/8209d4a2-6acd-45ef-b491-f033548de100.json"
                .into(),
            canonical_sha256: digest.clone(),
            observed_remote_sha256: digest,
            bytes: 8,
            delivered_at_unix: 1,
            verified_at_unix: 2,
            verified: true,
            limitations: vec!["point-in-time verification".into()],
        };
        save_decision_receipt(&config, &receipt).unwrap();
        save_decision_receipt(&config, &receipt).unwrap();
        let current = config
            .sync_dir()
            .join("delivery/example/8209d4a2-6acd-45ef-b491-f033548de100.json");
        assert!(
            load_decision_receipt_path(&current)
                .unwrap()
                .unwrap()
                .verified
        );
        let history = config
            .sync_dir()
            .join("delivery-history/example/8209d4a2-6acd-45ef-b491-f033548de100");
        assert_eq!(fs::read_dir(history).unwrap().count(), 2);
    }

    #[test]
    fn verified_receipt_rejects_different_canonical_and_remote_digests() {
        let temp = tempdir().unwrap();
        let config = test_config(temp.path());
        let receipt = DecisionDeliveryReceipt {
            schema_version: 2,
            sandbox: "example".into(),
            file: "8209d4a2-6acd-45ef-b491-f033548de100.json".into(),
            remote_path: "/sandbox/control/decisions/8209d4a2-6acd-45ef-b491-f033548de100.json"
                .into(),
            canonical_sha256: file_digest(b"host"),
            observed_remote_sha256: file_digest(b"sandbox"),
            bytes: 4,
            delivered_at_unix: 1,
            verified_at_unix: 2,
            verified: true,
            limitations: Vec::new(),
        };
        assert!(save_decision_receipt(&config, &receipt).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn safe_relative_symlink_is_copied() {
        let temp = tempdir().unwrap();
        let source = temp.path().join("source");
        fs::create_dir(&source).unwrap();
        fs::write(source.join("target"), b"ok").unwrap();
        std::os::unix::fs::symlink("target", source.join("link")).unwrap();
        let destination = temp.path().join("destination");
        copy_tree(&source, &destination).unwrap();
        assert_eq!(
            fs::read_link(destination.join("link")).unwrap(),
            Path::new("target")
        );
    }
}
