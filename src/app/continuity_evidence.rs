//! What a continuity agent's workspace holds: observed artifacts, their policy and hashes.
use super::*;

pub(super) const CONTINUITY_EVIDENCE_DIRECTORY: &str = "evidence";

pub(super) const MAX_CONTINUITY_SNAPSHOT_FILES: u64 = 100_000;

pub(super) const MAX_CONTINUITY_SNAPSHOT_BYTES: u64 = 4 * 1024 * 1024 * 1024;

#[derive(Clone, Debug, Serialize)]
pub(super) struct ObservedAfterReference {
    pub(super) run_id: String,
    pub(super) event_id: String,
    pub(super) sequence: u64,
    pub(super) hash: String,
}

impl From<provenance::ProvenanceAppendReceipt> for ObservedAfterReference {
    fn from(receipt: provenance::ProvenanceAppendReceipt) -> Self {
        Self {
            run_id: receipt.run_id,
            event_id: receipt.event_id,
            sequence: receipt.sequence,
            hash: receipt.hash,
        }
    }
}

#[derive(Clone, Debug, Serialize)]
pub(super) struct ContinuityArtifactVersion {
    pub(super) sha256: String,
    pub(super) size: i64,
    pub(super) role: String,
    pub(super) sensitivity: String,
}

#[derive(Clone, Debug, Serialize)]
pub(super) struct ContinuityWorkspaceDelta {
    pub(super) schema_version: u32,
    pub(super) trigger: String,
    pub(super) artifact_versions: Vec<ContinuityArtifactVersion>,
    pub(super) observed_after: Vec<ObservedAfterReference>,
}

pub(super) struct ContinuityWorkspaceObserver {
    pub(super) config: RuntimeConfig,
    pub(super) run_id: String,
    pub(super) subject: String,
    pub(super) workspace: PathBuf,
    pub(super) segment: u32,
    pub(super) sequence: u64,
    pub(super) head: SnapshotManifest,
}

impl ContinuityWorkspaceObserver {
    pub(super) fn new(
        config: RuntimeConfig,
        run_id: String,
        subject: String,
        workspace: PathBuf,
        segment: u32,
    ) -> Self {
        Self {
            config,
            run_id,
            subject,
            workspace,
            segment,
            sequence: 0,
            head: SnapshotManifest {
                schema_version: 1,
                files: Vec::new(),
                total_bytes: 0,
            },
        }
    }

    pub(super) fn capture_after_terminal_events(
        &mut self,
        events: &[CanonicalHarnessEvent],
        receipts: &[provenance::ProvenanceAppendReceipt],
    ) -> Result<()> {
        if events.len() != receipts.len() {
            bail!("canonical provenance receipt count does not match observed events");
        }
        let observed_after = events
            .iter()
            .zip(receipts)
            .filter(|(event, _)| is_terminal_tool_event(event))
            .map(|(_, receipt)| ObservedAfterReference::from(receipt.clone()))
            .collect::<Vec<_>>();
        if observed_after.is_empty() {
            return Ok(());
        }
        self.capture_workspace_delta(&observed_after)
    }

    pub(super) fn capture_workspace_delta(
        &mut self,
        observed_after: &[ObservedAfterReference],
    ) -> Result<()> {
        let evidence_root = self.workspace.join(CONTINUITY_EVIDENCE_DIRECTORY);
        if !evidence_root.exists() {
            return Ok(());
        }
        let manifest = inventory_continuity_evidence(&evidence_root)?;
        let previous = self
            .head
            .files
            .iter()
            .map(|entry| (entry.path.clone(), entry))
            .collect::<BTreeMap<_, _>>();
        let added_or_changed = manifest
            .files
            .iter()
            .filter(|entry| {
                previous
                    .get(&entry.path)
                    .is_none_or(|prior| prior.sha256 != entry.sha256 || prior.size != entry.size)
            })
            .collect::<Vec<_>>();
        if added_or_changed.is_empty() {
            return Ok(());
        }
        self.sequence = self.sequence.saturating_add(1);
        let mut versions = Vec::with_capacity(added_or_changed.len());
        let mut added_files = 0_u64;
        let mut changed_files = 0_u64;
        for entry in &added_or_changed {
            let relative = Path::new(&entry.path);
            let (role, sensitivity) = continuity_artifact_policy(relative);
            let artifact = agent_state::add_artifact(
                &self.config,
                &self.run_id,
                &self.subject,
                &evidence_root.join(relative),
                continuity_media_type(relative),
                role,
                &continuity_artifact_label(self.segment, self.sequence, &entry.path),
                sensitivity,
                "host-observer",
            )?;
            if artifact.sha256 != entry.sha256 || artifact.size != entry.size {
                bail!("continuity evidence changed while its immutable version was recorded");
            }
            if sensitivity == agent_state::ArtifactSensitivity::Standard {
                if previous.contains_key(&entry.path) {
                    changed_files = changed_files.saturating_add(1);
                } else {
                    added_files = added_files.saturating_add(1);
                }
                versions.push(ContinuityArtifactVersion {
                    sha256: artifact.sha256,
                    size: i64::try_from(artifact.size)?,
                    role: role.into(),
                    sensitivity: "standard".into(),
                });
            }
        }
        if versions.is_empty() {
            self.head = manifest;
            return Ok(());
        }
        let receipt = ContinuityWorkspaceDelta {
            schema_version: 2,
            trigger: "tool-completed".into(),
            artifact_versions: versions,
            observed_after: observed_after.to_vec(),
        };
        let receipt_dir = self
            .config
            .logs_dir()
            .join("logical-agents")
            .join(&self.subject)
            .join("segments")
            .join(format!("{:06}", self.segment))
            .join("snapshots");
        create_private_dir(&receipt_dir)?;
        let receipt_path = receipt_dir.join(format!("{:06}.json", self.sequence));
        write_private(&receipt_path, &serde_json::to_vec_pretty(&receipt)?)?;
        let receipt_artifact = agent_state::add_artifact(
            &self.config,
            &self.run_id,
            &self.subject,
            &receipt_path,
            "application/json",
            "workspace-delta",
            &format!(
                "continuity/segment-{}/snapshot-{}",
                self.segment, self.sequence
            ),
            agent_state::ArtifactSensitivity::Standard,
            "host-observer",
        )?;
        let manifest_sha256 = hash_json(&manifest)?;
        agent_state::append_event(
            &self.config,
            &self.run_id,
            &self.subject,
            "workspace.snapshot.captured",
            "host-observer",
            agent_state::StateAuthority::Machine,
            json!({
                "segment": self.segment,
                "sequence": self.sequence,
                "trigger": "tool-completed",
                "manifest_sha256": manifest_sha256,
                "receipt_sha256": receipt_artifact.sha256,
                "added_files": added_files,
                "changed_files": changed_files,
                "removed_files": 0,
                "artifact_versions": receipt.artifact_versions.len(),
                "observed_after": observed_after
            }),
        )?;
        self.head = manifest;
        Ok(())
    }
}

pub(super) fn is_terminal_tool_event(event: &CanonicalHarnessEvent) -> bool {
    event.kind == "agent.tool.finished"
        || (event.kind == "agent.tool.observed"
            && matches!(
                event.metadata.get("status").and_then(Value::as_str),
                Some("completed" | "error" | "failed")
            ))
}

pub(super) fn inventory_continuity_evidence(root: &Path) -> Result<SnapshotManifest> {
    let metadata = fs::symlink_metadata(root)?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        bail!("continuity evidence root must be a regular directory");
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
            bail!("continuity evidence contains a symbolic link");
        }
        if metadata.is_file() {
            total_bytes = total_bytes
                .checked_add(metadata.len())
                .context("continuity evidence byte count overflow")?;
            if files.len() as u64 >= MAX_CONTINUITY_SNAPSHOT_FILES
                || total_bytes > MAX_CONTINUITY_SNAPSHOT_BYTES
            {
                bail!("continuity evidence exceeds its file or byte limit");
            }
            let path = relative
                .to_str()
                .context("continuity evidence path is not UTF-8")?;
            if path.is_empty() || path.contains(['\n', '\r', '\0']) {
                bail!("continuity evidence path is invalid");
            }
            files.push(SnapshotEntry {
                path: path.into(),
                size: metadata.len(),
                sha256: hash_continuity_file(entry.path())?,
            });
        } else if !metadata.is_dir() {
            bail!("continuity evidence contains a non-regular entry");
        }
    }
    Ok(SnapshotManifest {
        schema_version: 1,
        files,
        total_bytes,
    })
}

pub(super) fn continuity_artifact_policy(
    relative: &Path,
) -> (&'static str, agent_state::ArtifactSensitivity) {
    let first = relative
        .components()
        .next()
        .and_then(|component| match component {
            std::path::Component::Normal(value) => value.to_str(),
            _ => None,
        });
    let lower = relative.to_string_lossy().to_ascii_lowercase();
    if matches!(first, Some("logs")) || lower.contains("reasoning") || lower.contains("trace") {
        (
            "execution-log",
            agent_state::ArtifactSensitivity::Restricted,
        )
    } else {
        match first {
            Some("discarded") => ("failed-attempt", agent_state::ArtifactSensitivity::Standard),
            Some("results") => ("result", agent_state::ArtifactSensitivity::Standard),
            Some("figures") => ("figure", agent_state::ArtifactSensitivity::Standard),
            _ => ("method", agent_state::ArtifactSensitivity::Standard),
        }
    }
}

pub(super) fn continuity_media_type(path: &Path) -> &'static str {
    match path
        .extension()
        .and_then(|value| value.to_str())
        .map(str::to_ascii_lowercase)
        .as_deref()
    {
        Some("json") | Some("jsonl") => "application/json",
        Some("csv") => "text/csv",
        Some("tsv") => "text/tab-separated-values",
        Some("md") => "text/markdown",
        Some("txt") | Some("log") | Some("r") | Some("py") | Some("sh") => "text/plain",
        Some("png") => "image/png",
        Some("jpg") | Some("jpeg") => "image/jpeg",
        Some("svg") => "image/svg+xml",
        Some("pdf") => "application/pdf",
        Some("parquet") => "application/vnd.apache.parquet",
        Some("arrow") | Some("feather") => "application/vnd.apache.arrow.file",
        _ => "application/octet-stream",
    }
}

pub(super) fn continuity_artifact_label(segment: u32, sequence: u64, path: &str) -> String {
    let prefix = format!("versions/segment-{segment}/snapshot-{sequence}/");
    if prefix.len().saturating_add(path.len()) <= 1024 {
        format!("{prefix}{path}")
    } else {
        format!("{prefix}sha256-{}", hash_bytes(path.as_bytes()))
    }
}

pub(super) fn hash_continuity_file(path: &Path) -> Result<String> {
    let metadata = fs::symlink_metadata(path)?;
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        bail!("continuity evidence file must be a regular file");
    }
    let bytes = fs::read(path)?;
    if bytes.len() as u64 != metadata.len() {
        bail!("continuity evidence changed while it was hashed");
    }
    Ok(hash_bytes(&bytes))
}
