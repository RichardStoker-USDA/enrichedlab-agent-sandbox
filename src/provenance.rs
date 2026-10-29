use std::{
    collections::{BTreeMap, BTreeSet},
    env,
    fs::{self, OpenOptions},
    io::{BufWriter, Write},
    net::IpAddr,
    path::{Path, PathBuf},
    str::FromStr,
    time::{SystemTime, UNIX_EPOCH},
};

use anyhow::{Context, Result, bail};
use native_tls::TlsConnector;
use postgres::{Client, Config as PostgresConfig, NoTls, Transaction as PostgresTransaction};
use postgres_native_tls::MakeTlsConnector;
use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};
use serde::Serialize;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use time::{Duration, OffsetDateTime, format_description::well_known::Rfc3339};
use uuid::Uuid;

use crate::{
    agent_state,
    config::{ProvenanceBackend, RuntimeConfig, create_private_dir, set_mode, write_private},
    harness::{CanonicalHarnessEvent, EvidenceVisibility, HarnessKind},
};

const POSTGRES_MIGRATION_1: &str = include_str!("../migrations/postgres/001_provenance.sql");
const MAX_METADATA_BYTES: usize = 256 * 1024;
const MAX_RESTRICTED_CONTENT_BYTES: usize = 2 * 1024 * 1024;
const MAX_GRAPH_EVENTS: usize = 10_000;
const MAX_SEARCH_WINDOW: usize = 10_000;
const MAX_EXPORT_EVENTS: usize = 100_000;

#[derive(Clone, Debug)]
pub struct ProvenanceContext<'a> {
    pub run_id: &'a str,
    pub subject: &'a str,
    pub segment: u32,
    pub harness: HarnessKind,
    pub provider: Option<&'a str>,
    pub model: Option<&'a str>,
    pub engine: Option<&'a str>,
    pub parser: Option<&'a str>,
    pub session_id: Option<&'a str>,
    pub actor: &'a str,
}

#[derive(Clone, Debug, Serialize)]
pub struct ProvenanceAppendReceipt {
    pub tenant_id: String,
    pub run_id: String,
    pub sequence: u64,
    pub event_id: String,
    pub hash: String,
    pub backend: String,
}

#[derive(Clone, Debug, Serialize)]
pub struct ProvenanceNode {
    pub id: String,
    pub source: String,
    pub sequence: u64,
    pub segment: u32,
    pub kind: String,
    pub evidence_role: String,
    pub visibility: String,
    pub actor: String,
    pub harness: String,
    pub provider: Option<String>,
    pub model: Option<String>,
    pub engine: Option<String>,
    pub parser: Option<String>,
    pub session_id: Option<String>,
    pub occurred_at: String,
    pub hash: String,
    pub content_sha256: Option<String>,
    pub content_bytes: Option<usize>,
    pub summary: Value,
}

#[derive(Clone, Debug, Serialize)]
pub struct ProvenanceEdge {
    pub id: String,
    pub source: String,
    pub target: String,
    pub kind: String,
}

#[derive(Clone, Debug, Serialize)]
pub struct ProvenanceArtifactNode {
    pub id: String,
    pub event_id: String,
    pub sha256: String,
    pub size: u64,
    pub media_type: String,
    pub role: String,
    pub label: String,
    pub sensitivity: String,
    pub created_at: String,
}

#[derive(Clone, Debug, Serialize)]
pub struct ProvenanceGraph {
    pub schema_version: u32,
    pub run_id: String,
    pub subject: String,
    pub backend: String,
    pub generated_at: String,
    pub nodes: Vec<ProvenanceNode>,
    pub edges: Vec<ProvenanceEdge>,
    pub artifacts: Vec<ProvenanceArtifactNode>,
    pub metadata_only: bool,
    pub truncated: bool,
}

#[derive(Clone, Debug, Serialize)]
pub struct EvidenceLeaseReceipt {
    pub lease_id: String,
    pub run_id: String,
    pub token: String,
    pub expires_at: String,
    pub purpose: String,
}

#[derive(Clone, Debug, Serialize)]
pub struct EvidenceEventDetail {
    pub schema_version: u32,
    pub run_id: String,
    pub event_id: String,
    pub source: String,
    pub kind: String,
    pub occurred_at: String,
    pub payload: Value,
    pub restricted_content: Option<String>,
    pub content_truncated: bool,
    pub content_sha256: Option<String>,
}

#[derive(Clone, Debug, Serialize)]
pub struct ProvenanceRunSummary {
    pub run_id: String,
    pub subject: String,
    pub status: String,
    pub created_at: String,
    pub updated_at: String,
    pub events: u64,
}

#[derive(Clone, Debug, Serialize)]
pub struct ProvenanceStorageStatus {
    pub backend: String,
    pub tenant_id: String,
    pub schema_version: u32,
    pub events: u64,
    pub leases: u64,
    pub runs: Vec<ProvenanceRunSummary>,
    pub append_only_controls: bool,
    pub tenant_policy_present: bool,
    pub chain_valid: bool,
    pub chain_runs_checked: u64,
}

#[derive(Clone, Debug, Serialize)]
pub struct ProvenanceExportReceipt {
    pub run_id: String,
    pub output_dir: PathBuf,
    pub events: usize,
    pub restricted_content_included: bool,
    pub events_sha256: String,
    pub manifest_sha256: String,
}

#[derive(Clone, Debug, Serialize)]
pub struct ProvenanceSearchHit {
    pub id: String,
    pub run_id: String,
    pub subject: String,
    pub kind: String,
    pub evidence_role: String,
    pub actor: String,
    pub harness: String,
    pub model: Option<String>,
    pub engine: Option<String>,
    pub occurred_at: String,
    pub summary: Value,
}

#[derive(Clone, Debug, Serialize)]
pub struct ProvenanceSearchPage {
    pub total: u64,
    pub hits: Vec<ProvenanceSearchHit>,
}

#[derive(Clone, Debug, Serialize)]
pub struct EvidenceSearchHit {
    pub event_id: String,
    pub source: String,
    pub kind: String,
    pub occurred_at: String,
    pub content_sha256: Option<String>,
    pub excerpt: String,
}

#[derive(Clone, Debug, Serialize)]
pub struct EvidenceSearchPage {
    pub query: String,
    pub total: u64,
    pub cursor: usize,
    pub next_cursor: Option<usize>,
    pub hits: Vec<EvidenceSearchHit>,
}

#[derive(Clone, Debug, Serialize)]
struct StoredEvent {
    sequence: u64,
    event_id: String,
    run_id: String,
    subject: String,
    segment: u32,
    harness: String,
    provider: Option<String>,
    model: Option<String>,
    engine: Option<String>,
    parser: Option<String>,
    session_id: Option<String>,
    kind: String,
    evidence_role: String,
    visibility: String,
    actor: String,
    occurred_at: String,
    source_event_id: Option<String>,
    parent_source_event_id: Option<String>,
    metadata: Value,
    restricted_content: Option<String>,
    content_sha256: Option<String>,
    previous_hash: String,
    hash: String,
}

#[derive(Clone, Debug)]
struct EvidenceCandidate {
    event_id: String,
    source: String,
    kind: String,
    occurred_at: String,
    content_sha256: Option<String>,
    content: String,
}

pub fn initialize(config: &RuntimeConfig) -> Result<ProvenanceStorageStatus> {
    let settings = config.load_settings()?;
    match settings.provenance.backend {
        ProvenanceBackend::Embedded => {
            let connection = open_embedded(config)?;
            embedded_status(&connection, &settings.provenance.tenant_id)
        }
        ProvenanceBackend::Postgres => {
            migrate_postgres(config)?;
            let mut client = open_postgres(config)?;
            postgres_status(&mut client, &settings.provenance.tenant_id)
        }
    }
}

pub fn status(config: &RuntimeConfig) -> Result<ProvenanceStorageStatus> {
    let settings = config.load_settings()?;
    match settings.provenance.backend {
        ProvenanceBackend::Embedded => {
            let connection = open_embedded(config)?;
            embedded_status(&connection, &settings.provenance.tenant_id)
        }
        ProvenanceBackend::Postgres => {
            let mut client = open_postgres(config)?;
            postgres_status(&mut client, &settings.provenance.tenant_id)
        }
    }
}

pub fn append_harness_event(
    config: &RuntimeConfig,
    context: &ProvenanceContext<'_>,
    event: &CanonicalHarnessEvent,
) -> Result<ProvenanceAppendReceipt> {
    validate_identifier(context.run_id, "run id")?;
    validate_identifier(context.subject, "subject")?;
    validate_text(&event.kind, "event kind", 128)?;
    validate_text(&event.evidence_role, "evidence role", 128)?;
    validate_text(context.actor, "actor", 128)?;
    let metadata_json = serde_json::to_string(&event.metadata)?;
    if metadata_json.len() > MAX_METADATA_BYTES {
        bail!("provenance event metadata exceeds the limit");
    }
    if event
        .restricted_content
        .as_ref()
        .is_some_and(|content| content.len() > MAX_RESTRICTED_CONTENT_BYTES)
    {
        bail!("provenance event content exceeds the limit");
    }
    let settings = config.load_settings()?;
    match settings.provenance.backend {
        ProvenanceBackend::Embedded => append_embedded(
            config,
            &settings.provenance.tenant_id,
            context,
            event,
            &metadata_json,
        ),
        ProvenanceBackend::Postgres => {
            append_postgres(config, &settings.provenance.tenant_id, context, event)
        }
    }
}

pub fn graph(config: &RuntimeConfig, run_id: &str, limit: usize) -> Result<ProvenanceGraph> {
    validate_identifier(run_id, "run id")?;
    if !(1..=MAX_GRAPH_EVENTS).contains(&limit) {
        bail!("provenance graph limit must be between one and {MAX_GRAPH_EVENTS}");
    }
    let settings = config.load_settings()?;
    let (stored, total) = match settings.provenance.backend {
        ProvenanceBackend::Embedded => {
            list_embedded(config, &settings.provenance.tenant_id, run_id, limit)?
        }
        ProvenanceBackend::Postgres => {
            list_postgres(config, &settings.provenance.tenant_id, run_id, limit)?
        }
    };
    let mut nodes = Vec::new();
    let mut edges = Vec::new();
    let mut artifacts = Vec::new();
    let mut rendered_node_ids = BTreeSet::new();
    let mut canonical_tools = BTreeMap::new();
    let mut subject = stored
        .first()
        .map(|event| event.subject.clone())
        .unwrap_or_else(|| "unknown".into());

    let mut prior: Option<String> = None;
    let mut source_ids = BTreeMap::new();
    for event in stored {
        let node_id = format!("provenance:{}", event.event_id);
        if is_canonical_terminal_tool_event(&event, run_id) {
            canonical_tools.insert(
                event.event_id.clone(),
                canonical_tool_reference(&event, &node_id),
            );
        }
        if let Some(previous) = prior.replace(node_id.clone()) {
            edges.push(ProvenanceEdge {
                id: format!("sequence:{previous}:{node_id}"),
                source: previous,
                target: node_id.clone(),
                kind: "sequence".into(),
            });
        }
        if let Some(source_id) = &event.source_event_id {
            source_ids.insert(source_id.clone(), node_id.clone());
        }
        if let Some(parent) = &event.parent_source_event_id
            && let Some(parent_node) = source_ids.get(parent)
        {
            edges.push(ProvenanceEdge {
                id: format!("parent:{parent_node}:{node_id}"),
                source: parent_node.clone(),
                target: node_id.clone(),
                kind: "parent".into(),
            });
        }
        let content_bytes = event.restricted_content.as_ref().map(String::len);
        rendered_node_ids.insert(node_id.clone());
        nodes.push(ProvenanceNode {
            id: node_id,
            source: "harness".into(),
            sequence: event.sequence,
            segment: event.segment,
            kind: event.kind,
            evidence_role: event.evidence_role,
            visibility: event.visibility,
            actor: event.actor,
            harness: event.harness,
            provider: event.provider,
            model: event.model,
            engine: event.engine,
            parser: event.parser,
            session_id: event.session_id,
            occurred_at: event.occurred_at,
            hash: event.hash,
            content_sha256: event.content_sha256,
            content_bytes,
            summary: safe_summary(&event.metadata),
        });
    }

    let mut truncated = total > nodes.len() as u64;
    if let Ok(run) = agent_state::load_run(config, run_id) {
        subject = run.subject;
        let state_events = agent_state::trace(config, run_id)?;
        truncated |= state_events.len().saturating_add(nodes.len()) > limit;
        let available = limit.saturating_sub(nodes.len());
        let selected = state_events
            .iter()
            .rev()
            .take(available)
            .cloned()
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
            .collect::<Vec<_>>();
        let mut snapshots = Vec::new();
        let mut prior_state: Option<String> = None;
        for event in selected {
            let node_id = format!("state:{}", event.event_id);
            if let Some(snapshot) = workspace_snapshot_reference(&event, run_id, &node_id) {
                snapshots.push(snapshot);
            }
            if let Some(previous) = prior_state.replace(node_id.clone()) {
                edges.push(ProvenanceEdge {
                    id: format!("state-sequence:{previous}:{node_id}"),
                    source: previous,
                    target: node_id.clone(),
                    kind: "state-sequence".into(),
                });
            }
            rendered_node_ids.insert(node_id.clone());
            nodes.push(ProvenanceNode {
                id: node_id,
                source: "state".into(),
                sequence: event.sequence,
                segment: event
                    .payload
                    .get("segment")
                    .and_then(Value::as_u64)
                    .and_then(|value| u32::try_from(value).ok())
                    .unwrap_or(0),
                kind: event.kind,
                evidence_role: state_event_role(&event.payload),
                visibility: "restricted".into(),
                actor: event.actor,
                harness: "controller".into(),
                provider: None,
                model: None,
                engine: None,
                parser: None,
                session_id: None,
                occurred_at: event.occurred_at,
                hash: event.hash,
                content_sha256: Some(event.payload_sha256),
                content_bytes: None,
                summary: safe_summary(&event.payload),
            });
        }
        let trace = agent_state::trace_summary(config, run_id, 1, 1_000)?;
        truncated |= trace.artifacts_truncated;
        let mut artifact_ids = BTreeSet::new();
        for artifact in &trace.artifacts {
            let artifact_id = format!("artifact:{}", artifact.sha256);
            if artifact_ids.insert(artifact_id.clone()) {
                artifacts.push(ProvenanceArtifactNode {
                    id: artifact_id.clone(),
                    event_id: artifact.event_id.clone(),
                    sha256: artifact.sha256.clone(),
                    size: artifact.size,
                    media_type: artifact.media_type.clone(),
                    role: artifact.role.clone(),
                    label: artifact.label.clone(),
                    sensitivity: format!("{:?}", artifact.sensitivity).to_ascii_lowercase(),
                    created_at: artifact.created_at.clone(),
                });
            }
            let source_event = format!("state:{}", artifact.event_id);
            if rendered_node_ids.contains(&source_event) {
                edges.push(ProvenanceEdge {
                    id: format!("artifact-link:{source_event}:{artifact_id}"),
                    source: source_event,
                    target: artifact_id,
                    kind: "artifact".into(),
                });
            }
        }
        for snapshot in snapshots {
            for observed in &snapshot.observed_after {
                let Some(tool) = canonical_tools.get(&observed.event_id) else {
                    continue;
                };
                if tool.run_id != observed.run_id
                    || tool.sequence != observed.sequence
                    || tool.hash != observed.hash
                {
                    continue;
                }
                edges.push(ProvenanceEdge {
                    id: format!("observed-after:{}:{}", tool.node_id, snapshot.node_id),
                    source: tool.node_id.clone(),
                    target: snapshot.node_id.clone(),
                    kind: "observed-after".into(),
                });
            }
            let matching = trace
                .artifacts
                .iter()
                .filter(|artifact| {
                    artifact.sha256 == snapshot.receipt_sha256
                        && artifact.media_type == "application/json"
                        && artifact.role == "workspace-delta"
                        && artifact.sensitivity == agent_state::ArtifactSensitivity::Standard
                })
                .collect::<Vec<_>>();
            if matching.len() == 1 {
                let artifact = matching[0];
                let artifact_event = state_events
                    .iter()
                    .find(|event| event.event_id == artifact.event_id);
                if artifact_event
                    .is_some_and(|event| is_workspace_delta_artifact_event(event, artifact))
                {
                    let artifact_id = format!("artifact:{}", artifact.sha256);
                    edges.push(ProvenanceEdge {
                        id: format!("snapshot-receipt:{}:{artifact_id}", snapshot.node_id),
                        source: snapshot.node_id,
                        target: artifact_id,
                        kind: "snapshot-receipt".into(),
                    });
                }
            }
        }
    }
    nodes.sort_by(|left, right| {
        left.occurred_at
            .cmp(&right.occurred_at)
            .then_with(|| left.id.cmp(&right.id))
    });
    Ok(ProvenanceGraph {
        schema_version: 1,
        run_id: run_id.into(),
        subject,
        backend: settings.provenance.backend.as_str().into(),
        generated_at: now()?,
        nodes,
        edges,
        artifacts,
        metadata_only: true,
        truncated,
    })
}

#[derive(Clone, Debug)]
struct CanonicalToolReference {
    node_id: String,
    run_id: String,
    sequence: u64,
    hash: String,
}

#[derive(Clone, Debug)]
struct ObservedAfterReference {
    run_id: String,
    event_id: String,
    sequence: u64,
    hash: String,
}

#[derive(Clone, Debug)]
struct WorkspaceSnapshotReference {
    node_id: String,
    receipt_sha256: String,
    observed_after: Vec<ObservedAfterReference>,
}

fn canonical_tool_reference(event: &StoredEvent, node_id: &str) -> CanonicalToolReference {
    CanonicalToolReference {
        node_id: node_id.into(),
        run_id: event.run_id.clone(),
        sequence: event.sequence,
        hash: event.hash.clone(),
    }
}

fn is_canonical_terminal_tool_event(event: &StoredEvent, run_id: &str) -> bool {
    if event.run_id != run_id
        || event.actor != "host-observer"
        || event.evidence_role != "tool-io"
        || event.visibility != "restricted"
        || !matches!(event.harness.as_str(), "pi" | "opencode")
    {
        return false;
    }
    match event.kind.as_str() {
        "agent.tool.finished" => true,
        "agent.tool.observed" => matches!(
            event.metadata.get("status").and_then(Value::as_str),
            Some("completed" | "error" | "failed")
        ),
        _ => false,
    }
}

fn workspace_snapshot_reference(
    event: &agent_state::StateEvent,
    run_id: &str,
    node_id: &str,
) -> Option<WorkspaceSnapshotReference> {
    if event.kind != "workspace.snapshot.captured"
        || event.run_id != run_id
        || event.actor != "host-observer"
        || event.authority != agent_state::StateAuthority::Machine
    {
        return None;
    }
    let payload = event.payload.as_object()?;
    const SNAPSHOT_KEYS: &[&str] = &[
        "added_files",
        "artifact_versions",
        "changed_files",
        "manifest_sha256",
        "observed_after",
        "receipt_sha256",
        "removed_files",
        "segment",
        "sequence",
        "trigger",
    ];
    if payload.len() != SNAPSHOT_KEYS.len()
        || SNAPSHOT_KEYS.iter().any(|key| !payload.contains_key(*key))
        || payload
            .keys()
            .any(|key| !SNAPSHOT_KEYS.contains(&key.as_str()))
        || payload.get("segment").and_then(Value::as_u64).is_none()
        || payload.get("sequence").and_then(Value::as_u64).is_none()
        || payload.get("trigger").and_then(Value::as_str).is_none()
        || [
            "added_files",
            "changed_files",
            "removed_files",
            "artifact_versions",
        ]
        .iter()
        .any(|key| payload.get(*key).and_then(Value::as_u64).is_none())
    {
        return None;
    }
    let receipt_sha256 = payload.get("receipt_sha256")?.as_str()?;
    if !is_sha256(receipt_sha256) || !is_sha256(payload.get("manifest_sha256")?.as_str()?) {
        return None;
    }
    let observed = payload.get("observed_after")?.as_array()?;
    let mut observed_after = Vec::with_capacity(observed.len());
    for reference in observed {
        let object = reference.as_object()?;
        const OBSERVED_KEYS: &[&str] = &["event_id", "hash", "run_id", "sequence"];
        if object.len() != OBSERVED_KEYS.len()
            || OBSERVED_KEYS.iter().any(|key| !object.contains_key(*key))
            || object
                .keys()
                .any(|key| !OBSERVED_KEYS.contains(&key.as_str()))
        {
            return None;
        }
        let reference_run_id = object.get("run_id")?.as_str()?;
        let event_id = object.get("event_id")?.as_str()?;
        let sequence = object.get("sequence")?.as_u64()?;
        let hash = object.get("hash")?.as_str()?;
        if reference_run_id != run_id || Uuid::parse_str(event_id).is_err() || !is_sha256(hash) {
            return None;
        }
        observed_after.push(ObservedAfterReference {
            run_id: reference_run_id.into(),
            event_id: event_id.into(),
            sequence,
            hash: hash.into(),
        });
    }
    Some(WorkspaceSnapshotReference {
        node_id: node_id.into(),
        receipt_sha256: receipt_sha256.into(),
        observed_after,
    })
}

fn is_workspace_delta_artifact_event(
    event: &agent_state::StateEvent,
    artifact: &agent_state::StateTraceArtifactSummary,
) -> bool {
    if event.kind != "artifact.recorded"
        || event.actor != "host-observer"
        || event.authority != agent_state::StateAuthority::Machine
    {
        return false;
    }
    let Some(payload) = event.payload.as_object() else {
        return false;
    };
    const ARTIFACT_KEYS: &[&str] = &[
        "label",
        "media_type",
        "role",
        "sensitivity",
        "sha256",
        "size",
    ];
    payload.len() == ARTIFACT_KEYS.len()
        && ARTIFACT_KEYS.iter().all(|key| payload.contains_key(*key))
        && payload
            .keys()
            .all(|key| ARTIFACT_KEYS.contains(&key.as_str()))
        && payload.get("sha256").and_then(Value::as_str) == Some(artifact.sha256.as_str())
        && payload.get("size").and_then(Value::as_u64) == Some(artifact.size)
        && payload.get("media_type").and_then(Value::as_str) == Some(artifact.media_type.as_str())
        && payload.get("role").and_then(Value::as_str) == Some(artifact.role.as_str())
        && payload.get("label").and_then(Value::as_str) == Some(artifact.label.as_str())
        && payload.get("sensitivity").and_then(Value::as_str) == Some("standard")
}

fn is_sha256(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
}

pub fn export_run(
    config: &RuntimeConfig,
    run_id: &str,
    output_dir: &Path,
    include_restricted: bool,
) -> Result<ProvenanceExportReceipt> {
    validate_identifier(run_id, "run id")?;
    if !output_dir.is_absolute() || output_dir.exists() {
        bail!("provenance export output must be a new absolute path");
    }
    let parent = output_dir
        .parent()
        .context("provenance export output has no parent")?;
    let parent_metadata = fs::symlink_metadata(parent).with_context(|| {
        format!(
            "provenance export parent does not exist: {}",
            parent.display()
        )
    })?;
    if parent_metadata.file_type().is_symlink() || !parent_metadata.is_dir() {
        bail!("provenance export parent must be a non-symlink directory");
    }
    let output_name = output_dir
        .file_name()
        .and_then(|value| value.to_str())
        .context("provenance export output needs a UTF-8 directory name")?;
    let settings = config.load_settings()?;
    let (mut events, total) = match settings.provenance.backend {
        ProvenanceBackend::Embedded => list_embedded(
            config,
            &settings.provenance.tenant_id,
            run_id,
            MAX_EXPORT_EVENTS,
        )?,
        ProvenanceBackend::Postgres => list_postgres(
            config,
            &settings.provenance.tenant_id,
            run_id,
            MAX_EXPORT_EVENTS,
        )?,
    };
    if total > MAX_EXPORT_EVENTS as u64 {
        bail!("provenance run exceeds the {MAX_EXPORT_EVENTS}-event export limit");
    }
    if events.is_empty() {
        bail!("provenance run has no events");
    }
    let pending = parent.join(format!(".{output_name}.{}.pending", Uuid::new_v4()));
    create_private_dir(&pending)?;
    let result = (|| -> Result<(String, String)> {
        let events_path = pending.join("events.jsonl");
        let mut options = OpenOptions::new();
        options.create_new(true).write(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let file = options.open(&events_path)?;
        let mut writer = BufWriter::new(file);
        let mut digest = Sha256::new();
        for event in &mut events {
            if !include_restricted {
                event.restricted_content = None;
            }
            let mut line = serde_json::to_vec(event)?;
            line.push(b'\n');
            digest.update(&line);
            writer.write_all(&line)?;
        }
        writer.flush()?;
        writer.get_ref().sync_all()?;
        set_mode(&events_path, 0o600)?;
        let events_sha256 = hex::encode(digest.finalize());
        let manifest = format!("{events_sha256}  events.jsonl\n");
        let manifest_sha256 = hash_text(&manifest);
        write_private(&pending.join("manifest-sha256.txt"), manifest.as_bytes())?;
        Ok((events_sha256, manifest_sha256))
    })();
    let (events_sha256, manifest_sha256) = match result {
        Ok(value) => value,
        Err(error) => {
            let _ = fs::remove_dir_all(&pending);
            return Err(error);
        }
    };
    fs::rename(&pending, output_dir)?;
    Ok(ProvenanceExportReceipt {
        run_id: run_id.into(),
        output_dir: output_dir.into(),
        events: events.len(),
        restricted_content_included: include_restricted,
        events_sha256,
        manifest_sha256,
    })
}

pub fn search_metadata(
    config: &RuntimeConfig,
    query: &str,
    fetch_limit: usize,
) -> Result<ProvenanceSearchPage> {
    validate_search_query(query, true)?;
    if !(1..=MAX_SEARCH_WINDOW).contains(&fetch_limit) {
        bail!("provenance search limit must be between one and {MAX_SEARCH_WINDOW}");
    }
    let settings = config.load_settings()?;
    let normalized = query.trim().to_lowercase();
    let (total, stored) = match settings.provenance.backend {
        ProvenanceBackend::Embedded => search_metadata_embedded(
            config,
            &settings.provenance.tenant_id,
            &normalized,
            fetch_limit,
        )?,
        ProvenanceBackend::Postgres => search_metadata_postgres(
            config,
            &settings.provenance.tenant_id,
            &normalized,
            fetch_limit,
        )?,
    };
    Ok(ProvenanceSearchPage {
        total,
        hits: stored
            .into_iter()
            .map(|event| ProvenanceSearchHit {
                id: format!("provenance:{}", event.event_id),
                run_id: event.run_id,
                subject: event.subject,
                kind: event.kind,
                evidence_role: event.evidence_role,
                actor: event.actor,
                harness: event.harness,
                model: event.model,
                engine: event.engine,
                occurred_at: event.occurred_at,
                summary: safe_summary(&event.metadata),
            })
            .collect(),
    })
}

pub fn search_evidence(
    config: &RuntimeConfig,
    run_id: &str,
    token: &str,
    query: &str,
    limit: usize,
    cursor: usize,
) -> Result<EvidenceSearchPage> {
    validate_identifier(run_id, "run id")?;
    validate_search_query(query, false)?;
    if !(1..=100).contains(&limit) {
        bail!("evidence search limit must be between one and 100");
    }
    let end = cursor
        .checked_add(limit)
        .context("evidence search cursor overflow")?;
    if end > MAX_SEARCH_WINDOW {
        bail!("evidence search window exceeds {MAX_SEARCH_WINDOW} records");
    }
    validate_evidence_lease(config, run_id, token)?;
    let settings = config.load_settings()?;
    let normalized = query.trim().to_lowercase();
    let (harness_total, mut candidates) = match settings.provenance.backend {
        ProvenanceBackend::Embedded => search_evidence_embedded(
            config,
            &settings.provenance.tenant_id,
            run_id,
            &normalized,
            end,
        )?,
        ProvenanceBackend::Postgres => search_evidence_postgres(
            config,
            &settings.provenance.tenant_id,
            run_id,
            &normalized,
            end,
        )?,
    };
    let mut state_total = 0_u64;
    for event in agent_state::trace(config, run_id)? {
        let content = serde_json::to_string(&event.payload)?;
        if !content.to_lowercase().contains(&normalized) {
            continue;
        }
        state_total = state_total.saturating_add(1);
        candidates.push(EvidenceCandidate {
            event_id: format!("state:{}", event.event_id),
            source: "state".into(),
            kind: event.kind,
            occurred_at: event.occurred_at,
            content_sha256: Some(event.payload_sha256),
            content,
        });
    }
    candidates.sort_by(|left, right| {
        right
            .occurred_at
            .cmp(&left.occurred_at)
            .then_with(|| left.event_id.cmp(&right.event_id))
    });
    let total = harness_total.saturating_add(state_total);
    let hits = candidates
        .into_iter()
        .skip(cursor)
        .take(limit)
        .map(|candidate| EvidenceSearchHit {
            event_id: candidate.event_id,
            source: candidate.source,
            kind: candidate.kind,
            occurred_at: candidate.occurred_at,
            content_sha256: candidate.content_sha256,
            excerpt: search_excerpt(&candidate.content, &normalized, 360),
        })
        .collect::<Vec<_>>();
    let next_cursor = (cursor + hits.len() < total as usize).then_some(cursor + hits.len());
    Ok(EvidenceSearchPage {
        query: query.trim().into(),
        total,
        cursor,
        next_cursor,
        hits,
    })
}

pub fn issue_evidence_lease(
    config: &RuntimeConfig,
    run_id: &str,
    actor: &str,
    purpose: &str,
) -> Result<EvidenceLeaseReceipt> {
    validate_identifier(run_id, "run id")?;
    validate_text(actor, "actor", 128)?;
    validate_text(purpose, "evidence lease purpose", 512)?;
    let settings = config.load_settings()?;
    let mut token = [0_u8; 32];
    getrandom::fill(&mut token).context("operating system randomness unavailable")?;
    let token = hex::encode(token);
    let token_sha256 = hash_text(&token);
    let lease_uuid = Uuid::new_v4();
    let lease_id = lease_uuid.to_string();
    let issued_at = OffsetDateTime::now_utc();
    let expires_at =
        issued_at + Duration::seconds(settings.provenance.evidence_lease_seconds as i64);
    match settings.provenance.backend {
        ProvenanceBackend::Embedded => {
            let mut connection = open_embedded(config)?;
            let transaction =
                connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
            ensure_embedded_run(
                &transaction,
                &settings.provenance.tenant_id,
                run_id,
                "unknown",
            )?;
            transaction.execute(
                "INSERT INTO evidence_leases (tenant_id, lease_id, run_id, token_sha256, actor, purpose, issued_at_unix, expires_at_unix, use_count) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, 0)",
                params![settings.provenance.tenant_id, lease_id, run_id, token_sha256, actor, purpose, issued_at.unix_timestamp(), expires_at.unix_timestamp()],
            )?;
            transaction.commit()?;
        }
        ProvenanceBackend::Postgres => {
            let mut client = open_postgres(config)?;
            let mut transaction = client.transaction()?;
            set_postgres_tenant_tx(&mut transaction, &settings.provenance.tenant_id)?;
            ensure_postgres_run(
                &mut transaction,
                &settings.provenance.tenant_id,
                run_id,
                "unknown",
            )?;
            transaction.execute(
                "INSERT INTO enrichedlab_agent.evidence_leases (tenant_id, lease_id, run_id, token_sha256, actor, purpose, issued_at, expires_at) VALUES ($1, $2, $3, $4, $5, $6, $7, $8)",
                &[&settings.provenance.tenant_id, &lease_uuid, &run_id, &token_sha256, &actor, &purpose, &issued_at, &expires_at],
            )?;
            transaction.commit()?;
        }
    }
    Ok(EvidenceLeaseReceipt {
        lease_id,
        run_id: run_id.into(),
        token,
        expires_at: expires_at.format(&Rfc3339)?,
        purpose: purpose.into(),
    })
}

pub fn event_evidence(
    config: &RuntimeConfig,
    run_id: &str,
    event_id: &str,
    token: &str,
) -> Result<EvidenceEventDetail> {
    validate_identifier(run_id, "run id")?;
    validate_evidence_lease(config, run_id, token)?;
    let settings = config.load_settings()?;
    let source_id = event_id.strip_prefix("provenance:").unwrap_or(event_id);
    let stored = match settings.provenance.backend {
        ProvenanceBackend::Embedded => {
            get_embedded_event(config, &settings.provenance.tenant_id, run_id, source_id)?
        }
        ProvenanceBackend::Postgres => {
            get_postgres_event(config, &settings.provenance.tenant_id, run_id, source_id)?
        }
    };
    if let Some(event) = stored {
        let (restricted_content, content_truncated) = bounded_preview(
            event.restricted_content,
            settings.provenance.evidence_preview_bytes,
        );
        return Ok(EvidenceEventDetail {
            schema_version: 1,
            run_id: run_id.into(),
            event_id: format!("provenance:{}", event.event_id),
            source: "harness".into(),
            kind: event.kind,
            occurred_at: event.occurred_at,
            payload: event.metadata,
            restricted_content,
            content_truncated,
            content_sha256: event.content_sha256,
        });
    }
    let state_id = event_id.strip_prefix("state:").unwrap_or(event_id);
    let event = agent_state::trace(config, run_id)?
        .into_iter()
        .find(|event| event.event_id == state_id)
        .context("provenance event not found")?;
    Ok(EvidenceEventDetail {
        schema_version: 1,
        run_id: run_id.into(),
        event_id: format!("state:{}", event.event_id),
        source: "state".into(),
        kind: event.kind,
        occurred_at: event.occurred_at,
        payload: event.payload,
        restricted_content: None,
        content_truncated: false,
        content_sha256: Some(event.payload_sha256),
    })
}

pub fn validate_evidence_lease(config: &RuntimeConfig, run_id: &str, token: &str) -> Result<()> {
    if token.len() != 64 || !token.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        bail!("evidence lease is invalid");
    }
    let settings = config.load_settings()?;
    let token_sha256 = hash_text(token);
    let valid = match settings.provenance.backend {
        ProvenanceBackend::Embedded => {
            let connection = open_embedded(config)?;
            let changed = connection.execute(
                "UPDATE evidence_leases SET use_count = use_count + 1, last_used_at_unix = ?1 WHERE tenant_id = ?2 AND run_id = ?3 AND token_sha256 = ?4 AND expires_at_unix > ?1",
                params![unix_now()?, settings.provenance.tenant_id, run_id, token_sha256],
            )?;
            changed == 1
        }
        ProvenanceBackend::Postgres => {
            let mut client = open_postgres(config)?;
            let changed = client.execute(
                "UPDATE enrichedlab_agent.evidence_leases SET use_count = use_count + 1, last_used_at = clock_timestamp() WHERE tenant_id = $1 AND run_id = $2 AND token_sha256 = $3 AND expires_at > clock_timestamp()",
                &[&settings.provenance.tenant_id, &run_id, &token_sha256],
            )?;
            changed == 1
        }
    };
    if !valid {
        bail!("evidence lease is expired or does not authorize this run");
    }
    Ok(())
}

fn append_embedded(
    config: &RuntimeConfig,
    tenant_id: &str,
    context: &ProvenanceContext<'_>,
    event: &CanonicalHarnessEvent,
    metadata_json: &str,
) -> Result<ProvenanceAppendReceipt> {
    let mut connection = open_embedded(config)?;
    let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
    ensure_embedded_run(&transaction, tenant_id, context.run_id, context.subject)?;
    let (sequence, previous_hash) = transaction.query_row(
        "SELECT COALESCE(MAX(sequence), 0) + 1, COALESCE((SELECT hash FROM provenance_events WHERE tenant_id = ?1 AND run_id = ?2 ORDER BY sequence DESC LIMIT 1), '') FROM provenance_events WHERE tenant_id = ?1 AND run_id = ?2",
        params![tenant_id, context.run_id],
        |row| Ok((row.get::<_, u64>(0)?, row.get::<_, String>(1)?)),
    )?;
    let event_id = Uuid::new_v4().to_string();
    let occurred_at = now()?;
    let content_sha256 = event.restricted_content.as_deref().map(hash_text);
    let hash = event_hash(EventHashMaterial {
        tenant_id,
        context,
        event,
        sequence,
        event_id: &event_id,
        occurred_at: &occurred_at,
        content_sha256: content_sha256.as_deref(),
        previous_hash: &previous_hash,
    })?;
    transaction.execute(
        "INSERT INTO provenance_events (tenant_id, run_id, sequence, event_id, subject, segment, harness, provider, model, engine, parser, session_id, kind, evidence_role, visibility, actor, occurred_at, source_event_id, parent_source_event_id, metadata_json, restricted_content, content_sha256, previous_hash, hash) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18, ?19, ?20, ?21, ?22, ?23, ?24)",
        params![
            tenant_id,
            context.run_id,
            sequence,
            event_id,
            context.subject,
            context.segment,
            context.harness.as_str(),
            context.provider,
            context.model,
            context.engine,
            context.parser,
            context.session_id,
            event.kind,
            event.evidence_role,
            visibility_name(event.visibility),
            context.actor,
            occurred_at,
            event.source_event_id,
            event.parent_source_event_id,
            metadata_json,
            event.restricted_content,
            content_sha256,
            previous_hash,
            hash,
        ],
    )?;
    transaction.execute(
        "UPDATE provenance_runs SET subject = ?1, updated_at = ?2 WHERE tenant_id = ?3 AND run_id = ?4",
        params![context.subject, occurred_at, tenant_id, context.run_id],
    )?;
    transaction.commit()?;
    Ok(ProvenanceAppendReceipt {
        tenant_id: tenant_id.into(),
        run_id: context.run_id.into(),
        sequence,
        event_id,
        hash,
        backend: "embedded".into(),
    })
}

fn append_postgres(
    config: &RuntimeConfig,
    tenant_id: &str,
    context: &ProvenanceContext<'_>,
    event: &CanonicalHarnessEvent,
) -> Result<ProvenanceAppendReceipt> {
    let mut client = open_postgres(config)?;
    let mut transaction = client.transaction()?;
    set_postgres_tenant_tx(&mut transaction, tenant_id)?;
    ensure_postgres_run(&mut transaction, tenant_id, context.run_id, context.subject)?;
    transaction.query_one(
        "SELECT pg_advisory_xact_lock(hashtextextended($1, 0))",
        &[&format!("{tenant_id}:{}", context.run_id)],
    )?;
    let row = transaction.query_one(
        "SELECT COALESCE(MAX(sequence), 0), COALESCE((SELECT hash FROM enrichedlab_agent.provenance_events WHERE tenant_id = $1 AND run_id = $2 ORDER BY sequence DESC LIMIT 1), '') FROM enrichedlab_agent.provenance_events WHERE tenant_id = $1 AND run_id = $2",
        &[&tenant_id, &context.run_id],
    )?;
    let sequence = u64::try_from(row.get::<_, i64>(0))?.saturating_add(1);
    let previous_hash = row.get::<_, String>(1);
    let event_uuid = Uuid::new_v4();
    let event_id = event_uuid.to_string();
    let occurred_at = now()?;
    let content_sha256 = event.restricted_content.as_deref().map(hash_text);
    let hash = event_hash(EventHashMaterial {
        tenant_id,
        context,
        event,
        sequence,
        event_id: &event_id,
        occurred_at: &occurred_at,
        content_sha256: content_sha256.as_deref(),
        previous_hash: &previous_hash,
    })?;
    transaction.execute(
        "INSERT INTO enrichedlab_agent.provenance_events (tenant_id, run_id, sequence, event_id, subject, segment, harness, provider, model, engine, parser, session_id, kind, evidence_role, visibility, actor, occurred_at, source_event_id, parent_source_event_id, metadata, restricted_content, content_sha256, previous_hash, hash) VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, $15, $16, $17, $18, $19, $20, $21, $22, $23, $24)",
        &[
            &tenant_id,
            &context.run_id,
            &i64::try_from(sequence)?,
            &event_uuid,
            &context.subject,
            &i64::from(context.segment),
            &context.harness.as_str(),
            &context.provider,
            &context.model,
            &context.engine,
            &context.parser,
            &context.session_id,
            &event.kind,
            &event.evidence_role,
            &visibility_name(event.visibility),
            &context.actor,
            &occurred_at,
            &event.source_event_id,
            &event.parent_source_event_id,
            &event.metadata,
            &event.restricted_content,
            &content_sha256,
            &previous_hash,
            &hash,
        ],
    )?;
    transaction.execute(
        "UPDATE enrichedlab_agent.provenance_runs SET subject = $1, updated_at = clock_timestamp() WHERE tenant_id = $2 AND run_id = $3",
        &[&context.subject, &tenant_id, &context.run_id],
    )?;
    transaction.commit()?;
    Ok(ProvenanceAppendReceipt {
        tenant_id: tenant_id.into(),
        run_id: context.run_id.into(),
        sequence,
        event_id,
        hash,
        backend: "postgres".into(),
    })
}

fn list_embedded(
    config: &RuntimeConfig,
    tenant_id: &str,
    run_id: &str,
    limit: usize,
) -> Result<(Vec<StoredEvent>, u64)> {
    let connection = open_embedded(config)?;
    let total = connection.query_row(
        "SELECT COUNT(*) FROM provenance_events WHERE tenant_id = ?1 AND run_id = ?2",
        params![tenant_id, run_id],
        |row| row.get(0),
    )?;
    let mut statement = connection.prepare(
        "SELECT sequence, event_id, run_id, subject, segment, harness, provider, model, engine, parser, session_id, kind, evidence_role, visibility, actor, occurred_at, source_event_id, parent_source_event_id, metadata_json, restricted_content, content_sha256, previous_hash, hash FROM provenance_events WHERE tenant_id = ?1 AND run_id = ?2 ORDER BY sequence DESC LIMIT ?3",
    )?;
    let rows = statement
        .query_map(
            params![tenant_id, run_id, limit as u64],
            stored_event_from_sqlite_row,
        )?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    Ok((rows.into_iter().rev().collect(), total))
}

fn get_embedded_event(
    config: &RuntimeConfig,
    tenant_id: &str,
    run_id: &str,
    event_id: &str,
) -> Result<Option<StoredEvent>> {
    let connection = open_embedded(config)?;
    connection
        .query_row(
            "SELECT sequence, event_id, run_id, subject, segment, harness, provider, model, engine, parser, session_id, kind, evidence_role, visibility, actor, occurred_at, source_event_id, parent_source_event_id, metadata_json, restricted_content, content_sha256, previous_hash, hash FROM provenance_events WHERE tenant_id = ?1 AND run_id = ?2 AND event_id = ?3",
            params![tenant_id, run_id, event_id],
            stored_event_from_sqlite_row,
        )
        .optional()
        .map_err(Into::into)
}

fn stored_event_from_sqlite_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<StoredEvent> {
    let metadata: String = row.get(18)?;
    Ok(StoredEvent {
        sequence: row.get(0)?,
        event_id: row.get(1)?,
        run_id: row.get(2)?,
        subject: row.get(3)?,
        segment: row.get(4)?,
        harness: row.get(5)?,
        provider: row.get(6)?,
        model: row.get(7)?,
        engine: row.get(8)?,
        parser: row.get(9)?,
        session_id: row.get(10)?,
        kind: row.get(11)?,
        evidence_role: row.get(12)?,
        visibility: row.get(13)?,
        actor: row.get(14)?,
        occurred_at: row.get(15)?,
        source_event_id: row.get(16)?,
        parent_source_event_id: row.get(17)?,
        metadata: serde_json::from_str(&metadata).unwrap_or_else(|_| json!({"invalid": true})),
        restricted_content: row.get(19)?,
        content_sha256: row.get(20)?,
        previous_hash: row.get(21)?,
        hash: row.get(22)?,
    })
}

fn list_postgres(
    config: &RuntimeConfig,
    tenant_id: &str,
    run_id: &str,
    limit: usize,
) -> Result<(Vec<StoredEvent>, u64)> {
    let mut client = open_postgres(config)?;
    let total = u64::try_from(
        client
            .query_one(
                "SELECT COUNT(*) FROM enrichedlab_agent.provenance_events WHERE tenant_id = $1 AND run_id = $2",
                &[&tenant_id, &run_id],
            )?
            .get::<_, i64>(0),
    )?;
    let rows = client.query(
        "SELECT sequence, event_id::text, run_id, subject, segment, harness, provider, model, engine, parser, session_id, kind, evidence_role, visibility, actor, occurred_at, source_event_id, parent_source_event_id, metadata, restricted_content, content_sha256, previous_hash, hash FROM enrichedlab_agent.provenance_events WHERE tenant_id = $1 AND run_id = $2 ORDER BY sequence DESC LIMIT $3",
        &[&tenant_id, &run_id, &i64::try_from(limit)?],
    )?;
    let mut events = rows
        .iter()
        .map(stored_event_from_postgres_row)
        .collect::<Result<Vec<_>>>()?;
    events.reverse();
    Ok((events, total))
}

fn get_postgres_event(
    config: &RuntimeConfig,
    tenant_id: &str,
    run_id: &str,
    event_id: &str,
) -> Result<Option<StoredEvent>> {
    let Ok(event_uuid) = Uuid::parse_str(event_id) else {
        return Ok(None);
    };
    let mut client = open_postgres(config)?;
    client
        .query_opt(
            "SELECT sequence, event_id::text, run_id, subject, segment, harness, provider, model, engine, parser, session_id, kind, evidence_role, visibility, actor, occurred_at, source_event_id, parent_source_event_id, metadata, restricted_content, content_sha256, previous_hash, hash FROM enrichedlab_agent.provenance_events WHERE tenant_id = $1 AND run_id = $2 AND event_id = $3",
            &[&tenant_id, &run_id, &event_uuid],
        )?
        .as_ref()
        .map(stored_event_from_postgres_row)
        .transpose()
}

fn stored_event_from_postgres_row(row: &postgres::Row) -> Result<StoredEvent> {
    Ok(StoredEvent {
        sequence: u64::try_from(row.get::<_, i64>(0))?,
        event_id: row.get(1),
        run_id: row.get(2),
        subject: row.get(3),
        segment: u32::try_from(row.get::<_, i64>(4))?,
        harness: row.get(5),
        provider: row.get(6),
        model: row.get(7),
        engine: row.get(8),
        parser: row.get(9),
        session_id: row.get(10),
        kind: row.get(11),
        evidence_role: row.get(12),
        visibility: row.get(13),
        actor: row.get(14),
        occurred_at: row.get(15),
        source_event_id: row.get(16),
        parent_source_event_id: row.get(17),
        metadata: row.get(18),
        restricted_content: row.get(19),
        content_sha256: row.get(20),
        previous_hash: row.get(21),
        hash: row.get(22),
    })
}

fn search_metadata_embedded(
    config: &RuntimeConfig,
    tenant_id: &str,
    query: &str,
    fetch_limit: usize,
) -> Result<(u64, Vec<StoredEvent>)> {
    let connection = open_embedded(config)?;
    if query.is_empty() {
        let total = connection.query_row(
            "SELECT COUNT(*) FROM provenance_events WHERE tenant_id = ?1",
            params![tenant_id],
            |row| row.get(0),
        )?;
        let mut statement = connection.prepare(
            "SELECT sequence, event_id, run_id, subject, segment, harness, provider, model, engine, parser, session_id, kind, evidence_role, visibility, actor, occurred_at, source_event_id, parent_source_event_id, metadata_json, restricted_content, content_sha256, previous_hash, hash FROM provenance_events WHERE tenant_id = ?1 ORDER BY occurred_at DESC, run_id, sequence DESC LIMIT ?2",
        )?;
        let events = statement
            .query_map(
                params![tenant_id, fetch_limit as u64],
                stored_event_from_sqlite_row,
            )?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        return Ok((total, events));
    }
    let pattern = like_pattern(query);
    let predicate = "(lower(run_id) LIKE ?2 ESCAPE '\\' OR lower(subject) LIKE ?2 ESCAPE '\\' OR lower(harness) LIKE ?2 ESCAPE '\\' OR lower(COALESCE(provider, '')) LIKE ?2 ESCAPE '\\' OR lower(COALESCE(model, '')) LIKE ?2 ESCAPE '\\' OR lower(COALESCE(engine, '')) LIKE ?2 ESCAPE '\\' OR lower(COALESCE(parser, '')) LIKE ?2 ESCAPE '\\' OR lower(kind) LIKE ?2 ESCAPE '\\' OR lower(evidence_role) LIKE ?2 ESCAPE '\\' OR lower(actor) LIKE ?2 ESCAPE '\\' OR lower(metadata_json) LIKE ?2 ESCAPE '\\')";
    let total = connection.query_row(
        &format!("SELECT COUNT(*) FROM provenance_events WHERE tenant_id = ?1 AND {predicate}"),
        params![tenant_id, pattern],
        |row| row.get(0),
    )?;
    let mut statement = connection.prepare(&format!(
        "SELECT sequence, event_id, run_id, subject, segment, harness, provider, model, engine, parser, session_id, kind, evidence_role, visibility, actor, occurred_at, source_event_id, parent_source_event_id, metadata_json, restricted_content, content_sha256, previous_hash, hash FROM provenance_events WHERE tenant_id = ?1 AND {predicate} ORDER BY occurred_at DESC, run_id, sequence DESC LIMIT ?3"
    ))?;
    let events = statement
        .query_map(
            params![tenant_id, pattern, fetch_limit as u64],
            stored_event_from_sqlite_row,
        )?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    Ok((total, events))
}

fn search_metadata_postgres(
    config: &RuntimeConfig,
    tenant_id: &str,
    query: &str,
    fetch_limit: usize,
) -> Result<(u64, Vec<StoredEvent>)> {
    let mut client = open_postgres(config)?;
    if query.is_empty() {
        let total = u64::try_from(
            client
                .query_one(
                    "SELECT COUNT(*) FROM enrichedlab_agent.provenance_events WHERE tenant_id = $1",
                    &[&tenant_id],
                )?
                .get::<_, i64>(0),
        )?;
        let rows = client.query(
            "SELECT sequence, event_id::text, run_id, subject, segment, harness, provider, model, engine, parser, session_id, kind, evidence_role, visibility, actor, occurred_at, source_event_id, parent_source_event_id, metadata, restricted_content, content_sha256, previous_hash, hash FROM enrichedlab_agent.provenance_events WHERE tenant_id = $1 ORDER BY occurred_at DESC, run_id, sequence DESC LIMIT $2",
            &[&tenant_id, &i64::try_from(fetch_limit)?],
        )?;
        let events = rows
            .iter()
            .map(stored_event_from_postgres_row)
            .collect::<Result<Vec<_>>>()?;
        return Ok((total, events));
    }
    let pattern = like_pattern(query);
    let predicate = "(run_id ILIKE $2 ESCAPE '\\' OR subject ILIKE $2 ESCAPE '\\' OR harness ILIKE $2 ESCAPE '\\' OR COALESCE(provider, '') ILIKE $2 ESCAPE '\\' OR COALESCE(model, '') ILIKE $2 ESCAPE '\\' OR COALESCE(engine, '') ILIKE $2 ESCAPE '\\' OR COALESCE(parser, '') ILIKE $2 ESCAPE '\\' OR kind ILIKE $2 ESCAPE '\\' OR evidence_role ILIKE $2 ESCAPE '\\' OR actor ILIKE $2 ESCAPE '\\' OR metadata::text ILIKE $2 ESCAPE '\\')";
    let total = u64::try_from(
        client
            .query_one(
                &format!("SELECT COUNT(*) FROM enrichedlab_agent.provenance_events WHERE tenant_id = $1 AND {predicate}"),
                &[&tenant_id, &pattern],
            )?
            .get::<_, i64>(0),
    )?;
    let rows = client.query(
        &format!("SELECT sequence, event_id::text, run_id, subject, segment, harness, provider, model, engine, parser, session_id, kind, evidence_role, visibility, actor, occurred_at, source_event_id, parent_source_event_id, metadata, restricted_content, content_sha256, previous_hash, hash FROM enrichedlab_agent.provenance_events WHERE tenant_id = $1 AND {predicate} ORDER BY occurred_at DESC, run_id, sequence DESC LIMIT $3"),
        &[&tenant_id, &pattern, &i64::try_from(fetch_limit)?],
    )?;
    let events = rows
        .iter()
        .map(stored_event_from_postgres_row)
        .collect::<Result<Vec<_>>>()?;
    Ok((total, events))
}

fn search_evidence_embedded(
    config: &RuntimeConfig,
    tenant_id: &str,
    run_id: &str,
    query: &str,
    fetch_limit: usize,
) -> Result<(u64, Vec<EvidenceCandidate>)> {
    let connection = open_embedded(config)?;
    let pattern = like_pattern(query);
    let total = connection.query_row(
        "SELECT COUNT(*) FROM provenance_events WHERE tenant_id = ?1 AND run_id = ?2 AND restricted_content IS NOT NULL AND lower(restricted_content) LIKE ?3 ESCAPE '\\'",
        params![tenant_id, run_id, pattern],
        |row| row.get(0),
    )?;
    let mut statement = connection.prepare(
        "SELECT event_id, kind, occurred_at, content_sha256, restricted_content FROM provenance_events WHERE tenant_id = ?1 AND run_id = ?2 AND restricted_content IS NOT NULL AND lower(restricted_content) LIKE ?3 ESCAPE '\\' ORDER BY occurred_at DESC, sequence DESC LIMIT ?4",
    )?;
    let candidates = statement
        .query_map(
            params![tenant_id, run_id, pattern, fetch_limit as u64],
            |row| {
                Ok(EvidenceCandidate {
                    event_id: format!("provenance:{}", row.get::<_, String>(0)?),
                    source: "harness".into(),
                    kind: row.get(1)?,
                    occurred_at: row.get(2)?,
                    content_sha256: row.get(3)?,
                    content: row.get(4)?,
                })
            },
        )?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    Ok((total, candidates))
}

fn search_evidence_postgres(
    config: &RuntimeConfig,
    tenant_id: &str,
    run_id: &str,
    query: &str,
    fetch_limit: usize,
) -> Result<(u64, Vec<EvidenceCandidate>)> {
    let mut client = open_postgres(config)?;
    let pattern = like_pattern(query);
    let total = u64::try_from(
        client
            .query_one(
                "SELECT COUNT(*) FROM enrichedlab_agent.provenance_events WHERE tenant_id = $1 AND run_id = $2 AND restricted_content IS NOT NULL AND restricted_content ILIKE $3 ESCAPE '\\'",
                &[&tenant_id, &run_id, &pattern],
            )?
            .get::<_, i64>(0),
    )?;
    let candidates = client
        .query(
            "SELECT event_id::text, kind, occurred_at, content_sha256, restricted_content FROM enrichedlab_agent.provenance_events WHERE tenant_id = $1 AND run_id = $2 AND restricted_content IS NOT NULL AND restricted_content ILIKE $3 ESCAPE '\\' ORDER BY occurred_at DESC, sequence DESC LIMIT $4",
            &[&tenant_id, &run_id, &pattern, &i64::try_from(fetch_limit)?],
        )?
        .into_iter()
        .map(|row| EvidenceCandidate {
            event_id: format!("provenance:{}", row.get::<_, String>(0)),
            source: "harness".into(),
            kind: row.get(1),
            occurred_at: row.get(2),
            content_sha256: row.get(3),
            content: row.get(4),
        })
        .collect();
    Ok((total, candidates))
}

fn open_embedded(config: &RuntimeConfig) -> Result<Connection> {
    create_private_dir(&config.agent_state_dir())?;
    create_private_dir(&config.provenance_artifacts_dir())?;
    let path = config.provenance_database_path();
    if !path.exists() {
        let mut options = std::fs::OpenOptions::new();
        options.create_new(true).write(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        options.open(&path)?;
    }
    set_mode(&path, 0o600)?;
    let connection = Connection::open(&path)?;
    connection.busy_timeout(std::time::Duration::from_secs(10))?;
    connection.execute_batch(
        "PRAGMA journal_mode = WAL;
         PRAGMA synchronous = FULL;
         PRAGMA foreign_keys = ON;
         PRAGMA trusted_schema = OFF;
         CREATE TABLE IF NOT EXISTS provenance_runs (
           tenant_id TEXT NOT NULL,
           run_id TEXT NOT NULL,
           subject TEXT NOT NULL,
           status TEXT NOT NULL,
           created_at TEXT NOT NULL,
           updated_at TEXT NOT NULL,
           PRIMARY KEY (tenant_id, run_id)
         );
         CREATE TABLE IF NOT EXISTS provenance_events (
           tenant_id TEXT NOT NULL,
           run_id TEXT NOT NULL,
           sequence INTEGER NOT NULL,
           event_id TEXT NOT NULL,
           subject TEXT NOT NULL,
           segment INTEGER NOT NULL,
           harness TEXT NOT NULL,
           provider TEXT,
           model TEXT,
           engine TEXT,
           parser TEXT,
           session_id TEXT,
           kind TEXT NOT NULL,
           evidence_role TEXT NOT NULL,
           visibility TEXT NOT NULL,
           actor TEXT NOT NULL,
           occurred_at TEXT NOT NULL,
           source_event_id TEXT,
           parent_source_event_id TEXT,
           metadata_json TEXT NOT NULL,
           restricted_content TEXT,
           content_sha256 TEXT,
           previous_hash TEXT NOT NULL,
           hash TEXT NOT NULL,
           PRIMARY KEY (tenant_id, run_id, sequence),
           UNIQUE (tenant_id, event_id),
           UNIQUE (tenant_id, run_id, hash),
           FOREIGN KEY (tenant_id, run_id) REFERENCES provenance_runs(tenant_id, run_id)
         );
         CREATE INDEX IF NOT EXISTS provenance_events_run_time ON provenance_events(tenant_id, run_id, occurred_at, sequence);
         CREATE TABLE IF NOT EXISTS evidence_leases (
           tenant_id TEXT NOT NULL,
           lease_id TEXT NOT NULL,
           run_id TEXT NOT NULL,
           token_sha256 TEXT NOT NULL,
           actor TEXT NOT NULL,
           purpose TEXT NOT NULL,
           issued_at_unix INTEGER NOT NULL,
           expires_at_unix INTEGER NOT NULL,
           use_count INTEGER NOT NULL,
           last_used_at_unix INTEGER,
           PRIMARY KEY (tenant_id, lease_id),
           UNIQUE (tenant_id, token_sha256),
           FOREIGN KEY (tenant_id, run_id) REFERENCES provenance_runs(tenant_id, run_id)
         );
         CREATE TRIGGER IF NOT EXISTS provenance_events_no_update BEFORE UPDATE ON provenance_events
           BEGIN SELECT RAISE(ABORT, 'provenance events are append-only'); END;
         CREATE TRIGGER IF NOT EXISTS provenance_events_no_delete BEFORE DELETE ON provenance_events
           BEGIN SELECT RAISE(ABORT, 'provenance events are append-only'); END;",
    )?;
    for suffix in ["-wal", "-shm"] {
        let companion = std::path::PathBuf::from(format!("{}{}", path.display(), suffix));
        if companion.exists() {
            set_mode(&companion, 0o600)?;
        }
    }
    Ok(connection)
}

fn open_postgres(config: &RuntimeConfig) -> Result<Client> {
    let settings = config.load_settings()?;
    let dsn = env::var(&settings.provenance.postgres_dsn_env).with_context(|| {
        format!(
            "PostgreSQL provenance is selected but {} is not set",
            settings.provenance.postgres_dsn_env
        )
    })?;
    let mut client = connect_postgres(&dsn)?;
    client.query_one(
        "SELECT set_config('enrichedlab.tenant_id', $1, false)",
        &[&settings.provenance.tenant_id],
    )?;
    Ok(client)
}

fn migrate_postgres(config: &RuntimeConfig) -> Result<()> {
    let settings = config.load_settings()?;
    let dsn = env::var(&settings.provenance.postgres_migration_dsn_env).with_context(|| {
        format!(
            "PostgreSQL provenance migration needs {} and never falls back to the runtime identity",
            settings.provenance.postgres_migration_dsn_env
        )
    })?;
    let mut client = connect_postgres(&dsn)?;
    client
        .batch_execute(POSTGRES_MIGRATION_1)
        .context("failed to apply PostgreSQL provenance migrations")?;
    Ok(())
}

fn connect_postgres(dsn: &str) -> Result<Client> {
    let pg_config = PostgresConfig::from_str(dsn).context("invalid PostgreSQL provenance DSN")?;
    let insecure = format!("{:?}", pg_config.get_ssl_mode()).eq_ignore_ascii_case("disable");
    if insecure && !postgres_hosts_are_loopback(&pg_config) {
        bail!("PostgreSQL sslmode=disable is allowed only for a loopback qualification database");
    }
    let client = if insecure {
        pg_config.connect(NoTls)
    } else {
        let connector = TlsConnector::builder()
            .build()
            .context("failed to build PostgreSQL TLS connector")?;
        pg_config.connect(MakeTlsConnector::new(connector))
    }
    .context("failed to connect to PostgreSQL provenance storage")?;
    Ok(client)
}

fn postgres_hosts_are_loopback(config: &PostgresConfig) -> bool {
    use postgres::config::Host;
    config.get_hosts().iter().all(|host| match host {
        Host::Tcp(host) => {
            host == "localhost"
                || host
                    .parse::<IpAddr>()
                    .is_ok_and(|address| address.is_loopback())
        }
        #[cfg(unix)]
        Host::Unix(_) => true,
    })
}

fn ensure_embedded_run(
    transaction: &rusqlite::Transaction<'_>,
    tenant_id: &str,
    run_id: &str,
    subject: &str,
) -> Result<()> {
    let timestamp = now()?;
    transaction.execute(
        "INSERT INTO provenance_runs (tenant_id, run_id, subject, status, created_at, updated_at) VALUES (?1, ?2, ?3, 'active', ?4, ?4) ON CONFLICT (tenant_id, run_id) DO NOTHING",
        params![tenant_id, run_id, subject, timestamp],
    )?;
    Ok(())
}

fn ensure_postgres_run(
    transaction: &mut PostgresTransaction<'_>,
    tenant_id: &str,
    run_id: &str,
    subject: &str,
) -> Result<()> {
    transaction.execute(
        "INSERT INTO enrichedlab_agent.provenance_runs (tenant_id, run_id, subject, status, created_at, updated_at) VALUES ($1, $2, $3, 'active', clock_timestamp(), clock_timestamp()) ON CONFLICT (tenant_id, run_id) DO NOTHING",
        &[&tenant_id, &run_id, &subject],
    )?;
    Ok(())
}

fn set_postgres_tenant_tx(
    transaction: &mut PostgresTransaction<'_>,
    tenant_id: &str,
) -> Result<()> {
    transaction.query_one(
        "SELECT set_config('enrichedlab.tenant_id', $1, true)",
        &[&tenant_id],
    )?;
    Ok(())
}

fn embedded_status(connection: &Connection, tenant_id: &str) -> Result<ProvenanceStorageStatus> {
    let events = connection.query_row(
        "SELECT COUNT(*) FROM provenance_events WHERE tenant_id = ?1",
        params![tenant_id],
        |row| row.get(0),
    )?;
    let leases = connection.query_row(
        "SELECT COUNT(*) FROM evidence_leases WHERE tenant_id = ?1",
        params![tenant_id],
        |row| row.get(0),
    )?;
    let triggers: u64 = connection.query_row(
        "SELECT COUNT(*) FROM sqlite_master WHERE type = 'trigger' AND name IN ('provenance_events_no_update', 'provenance_events_no_delete')",
        [],
        |row| row.get(0),
    )?;
    let stored = all_embedded_events(connection, tenant_id)?;
    let mut statement = connection.prepare(
        "SELECT runs.run_id, runs.subject, runs.status, runs.created_at, runs.updated_at, COUNT(events.sequence) FROM provenance_runs AS runs LEFT JOIN provenance_events AS events ON events.tenant_id = runs.tenant_id AND events.run_id = runs.run_id WHERE runs.tenant_id = ?1 GROUP BY runs.run_id, runs.subject, runs.status, runs.created_at, runs.updated_at ORDER BY runs.updated_at DESC, runs.run_id",
    )?;
    let runs = statement
        .query_map(params![tenant_id], |row| {
            Ok(ProvenanceRunSummary {
                run_id: row.get(0)?,
                subject: row.get(1)?,
                status: row.get(2)?,
                created_at: row.get(3)?,
                updated_at: row.get(4)?,
                events: row.get(5)?,
            })
        })?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    let chain_runs_checked = count_chain_runs(&stored);
    let chain_valid = verify_event_chains(tenant_id, &stored).is_ok();
    Ok(ProvenanceStorageStatus {
        backend: "embedded".into(),
        tenant_id: tenant_id.into(),
        schema_version: 1,
        events,
        leases,
        runs,
        append_only_controls: triggers == 2,
        tenant_policy_present: true,
        chain_valid,
        chain_runs_checked,
    })
}

fn postgres_status(client: &mut Client, tenant_id: &str) -> Result<ProvenanceStorageStatus> {
    let events = u64::try_from(
        client
            .query_one(
                "SELECT COUNT(*) FROM enrichedlab_agent.provenance_events WHERE tenant_id = $1",
                &[&tenant_id],
            )?
            .get::<_, i64>(0),
    )?;
    let leases = u64::try_from(
        client
            .query_one(
                "SELECT COUNT(*) FROM enrichedlab_agent.evidence_leases WHERE tenant_id = $1",
                &[&tenant_id],
            )?
            .get::<_, i64>(0),
    )?;
    let policy_count = client
        .query_one(
            "SELECT COUNT(*) FROM pg_policies WHERE schemaname = 'enrichedlab_agent' AND policyname IN ('provenance_runs_tenant', 'provenance_events_tenant', 'evidence_leases_tenant')",
            &[],
        )?
        .get::<_, i64>(0);
    let trigger_count = client
        .query_one(
            "SELECT COUNT(*) FROM information_schema.triggers WHERE event_object_schema = 'enrichedlab_agent' AND trigger_name IN ('provenance_events_no_update', 'provenance_events_no_delete')",
            &[],
        )?
        .get::<_, i64>(0);
    let runs = client
        .query(
            "SELECT runs.run_id, runs.subject, runs.status, runs.created_at, runs.updated_at, COUNT(events.sequence) FROM enrichedlab_agent.provenance_runs AS runs LEFT JOIN enrichedlab_agent.provenance_events AS events ON events.tenant_id = runs.tenant_id AND events.run_id = runs.run_id WHERE runs.tenant_id = $1 GROUP BY runs.run_id, runs.subject, runs.status, runs.created_at, runs.updated_at ORDER BY runs.updated_at DESC, runs.run_id",
            &[&tenant_id],
        )?
        .iter()
        .map(|row| {
            Ok(ProvenanceRunSummary {
                run_id: row.get(0),
                subject: row.get(1),
                status: row.get(2),
                created_at: row.get::<_, OffsetDateTime>(3).format(&Rfc3339)?,
                updated_at: row.get::<_, OffsetDateTime>(4).format(&Rfc3339)?,
                events: u64::try_from(row.get::<_, i64>(5))?,
            })
        })
        .collect::<Result<Vec<_>>>()?;
    let stored = all_postgres_events(client, tenant_id)?;
    let chain_runs_checked = count_chain_runs(&stored);
    let chain_valid = verify_event_chains(tenant_id, &stored).is_ok();
    Ok(ProvenanceStorageStatus {
        backend: "postgres".into(),
        tenant_id: tenant_id.into(),
        schema_version: 1,
        events,
        leases,
        runs,
        append_only_controls: trigger_count == 2,
        tenant_policy_present: policy_count == 3,
        chain_valid,
        chain_runs_checked,
    })
}

fn all_embedded_events(connection: &Connection, tenant_id: &str) -> Result<Vec<StoredEvent>> {
    let mut statement = connection.prepare(
        "SELECT sequence, event_id, run_id, subject, segment, harness, provider, model, engine, parser, session_id, kind, evidence_role, visibility, actor, occurred_at, source_event_id, parent_source_event_id, metadata_json, restricted_content, content_sha256, previous_hash, hash FROM provenance_events WHERE tenant_id = ?1 ORDER BY run_id, sequence",
    )?;
    Ok(statement
        .query_map(params![tenant_id], stored_event_from_sqlite_row)?
        .collect::<std::result::Result<Vec<_>, _>>()?)
}

fn all_postgres_events(client: &mut Client, tenant_id: &str) -> Result<Vec<StoredEvent>> {
    client
        .query(
            "SELECT sequence, event_id::text, run_id, subject, segment, harness, provider, model, engine, parser, session_id, kind, evidence_role, visibility, actor, occurred_at, source_event_id, parent_source_event_id, metadata, restricted_content, content_sha256, previous_hash, hash FROM enrichedlab_agent.provenance_events WHERE tenant_id = $1 ORDER BY run_id, sequence",
            &[&tenant_id],
        )?
        .iter()
        .map(stored_event_from_postgres_row)
        .collect()
}

fn count_chain_runs(events: &[StoredEvent]) -> u64 {
    let mut previous: Option<&str> = None;
    let mut runs = 0_u64;
    for event in events {
        if previous != Some(event.run_id.as_str()) {
            runs = runs.saturating_add(1);
            previous = Some(event.run_id.as_str());
        }
    }
    runs
}

fn verify_event_chains(tenant_id: &str, events: &[StoredEvent]) -> Result<()> {
    let mut run_id = "";
    let mut expected_sequence = 1_u64;
    let mut expected_previous = String::new();
    for event in events {
        if event.run_id != run_id {
            run_id = &event.run_id;
            expected_sequence = 1;
            expected_previous.clear();
        }
        if event.sequence != expected_sequence || event.previous_hash != expected_previous {
            bail!("provenance hash-chain sequence is invalid");
        }
        let content_sha256 = event.restricted_content.as_deref().map(hash_text);
        if content_sha256 != event.content_sha256 {
            bail!("provenance restricted-content digest is invalid");
        }
        let harness = parse_harness(&event.harness)?;
        let visibility = parse_visibility(&event.visibility)?;
        let context = ProvenanceContext {
            run_id: &event.run_id,
            subject: &event.subject,
            segment: event.segment,
            harness,
            provider: event.provider.as_deref(),
            model: event.model.as_deref(),
            engine: event.engine.as_deref(),
            parser: event.parser.as_deref(),
            session_id: event.session_id.as_deref(),
            actor: &event.actor,
        };
        let canonical = CanonicalHarnessEvent {
            kind: event.kind.clone(),
            evidence_role: event.evidence_role.clone(),
            visibility,
            source_event_id: event.source_event_id.clone(),
            parent_source_event_id: event.parent_source_event_id.clone(),
            metadata: event.metadata.clone(),
            restricted_content: event.restricted_content.clone(),
        };
        let observed = event_hash(EventHashMaterial {
            tenant_id,
            context: &context,
            event: &canonical,
            sequence: event.sequence,
            event_id: &event.event_id,
            occurred_at: &event.occurred_at,
            content_sha256: content_sha256.as_deref(),
            previous_hash: &event.previous_hash,
        })?;
        if observed != event.hash {
            bail!("provenance event digest is invalid");
        }
        expected_previous.clone_from(&event.hash);
        expected_sequence = expected_sequence.saturating_add(1);
    }
    Ok(())
}

fn parse_harness(value: &str) -> Result<HarnessKind> {
    match value {
        "opencode" => Ok(HarnessKind::OpenCode),
        "pi" => Ok(HarnessKind::Pi),
        "openai-compatible" => Ok(HarnessKind::OpenAiCompatible),
        "native" => Ok(HarnessKind::Native),
        _ => bail!("provenance harness is invalid"),
    }
}

fn parse_visibility(value: &str) -> Result<EvidenceVisibility> {
    match value {
        "metadata" => Ok(EvidenceVisibility::Metadata),
        "restricted" => Ok(EvidenceVisibility::Restricted),
        _ => bail!("provenance visibility is invalid"),
    }
}

struct EventHashMaterial<'a> {
    tenant_id: &'a str,
    context: &'a ProvenanceContext<'a>,
    event: &'a CanonicalHarnessEvent,
    sequence: u64,
    event_id: &'a str,
    occurred_at: &'a str,
    content_sha256: Option<&'a str>,
    previous_hash: &'a str,
}

fn event_hash(material: EventHashMaterial<'_>) -> Result<String> {
    let EventHashMaterial {
        tenant_id,
        context,
        event,
        sequence,
        event_id,
        occurred_at,
        content_sha256,
        previous_hash,
    } = material;
    let canonical = json!({
        "tenant_id": tenant_id,
        "run_id": context.run_id,
        "subject": context.subject,
        "segment": context.segment,
        "harness": context.harness.as_str(),
        "provider": context.provider,
        "model": context.model,
        "engine": context.engine,
        "parser": context.parser,
        "session_id": context.session_id,
        "sequence": sequence,
        "event_id": event_id,
        "kind": event.kind,
        "evidence_role": event.evidence_role,
        "visibility": visibility_name(event.visibility),
        "actor": context.actor,
        "occurred_at": occurred_at,
        "source_event_id": event.source_event_id,
        "parent_source_event_id": event.parent_source_event_id,
        "metadata": event.metadata,
        "content_sha256": content_sha256,
        "previous_hash": previous_hash
    });
    Ok(hash_bytes(&serde_json::to_vec(&canonical)?))
}

fn safe_summary(metadata: &Value) -> Value {
    const ALLOWED: &[&str] = &[
        "segment",
        "sequence",
        "trigger",
        "tool",
        "status",
        "source",
        "source_type",
        "bytes",
        "sha256",
        "input_bytes",
        "output_bytes",
        "input_sha256",
        "output_sha256",
        "duration_ms",
        "input_tokens",
        "output_tokens",
        "reasoning_tokens",
        "total_tokens",
        "cache_read_tokens",
        "cache_write_tokens",
        "manifest_sha256",
        "added_files",
        "changed_files",
        "removed_files",
        "artifact_versions",
        "provider_emitted",
        "internal_activations_captured",
        "content_in_prompt_state",
        "fresh_provider_session",
        "context_action",
        "result_class",
        "result_complete",
    ];
    let mut summary = serde_json::Map::new();
    if let Some(object) = metadata.as_object() {
        for key in ALLOWED {
            if let Some(value) = object.get(*key) {
                summary.insert((*key).into(), value.clone());
            }
        }
        summary.insert(
            "metadata_keys".into(),
            json!(object.keys().take(64).collect::<Vec<_>>()),
        );
    }
    Value::Object(summary)
}

fn state_event_role(payload: &Value) -> String {
    payload
        .get("role")
        .and_then(Value::as_str)
        .unwrap_or("state-transition")
        .to_string()
}

fn bounded_preview(content: Option<String>, max_bytes: usize) -> (Option<String>, bool) {
    match content {
        Some(content) if content.len() > max_bytes => {
            let mut boundary = max_bytes;
            while !content.is_char_boundary(boundary) {
                boundary = boundary.saturating_sub(1);
            }
            (Some(content[..boundary].to_string()), true)
        }
        Some(content) => (Some(content), false),
        None => (None, false),
    }
}

fn visibility_name(visibility: EvidenceVisibility) -> &'static str {
    match visibility {
        EvidenceVisibility::Metadata => "metadata",
        EvidenceVisibility::Restricted => "restricted",
    }
}

fn validate_identifier(value: &str, field: &str) -> Result<()> {
    if value.is_empty()
        || value.len() > 128
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b':'))
    {
        bail!("{field} is invalid");
    }
    Ok(())
}

fn validate_text(value: &str, field: &str, max_bytes: usize) -> Result<()> {
    if value.trim().is_empty()
        || value.len() > max_bytes
        || value
            .chars()
            .any(|character| character == '\0' || character == '\r' || character.is_control())
    {
        bail!("{field} is invalid");
    }
    Ok(())
}

fn validate_search_query(value: &str, allow_empty: bool) -> Result<()> {
    let trimmed = value.trim();
    if (!allow_empty && trimmed.is_empty())
        || trimmed.len() > 128
        || trimmed
            .chars()
            .any(|character| character == '\0' || character == '\r' || character.is_control())
    {
        bail!("search query is invalid");
    }
    Ok(())
}

fn like_pattern(value: &str) -> String {
    let escaped = value
        .replace('\\', "\\\\")
        .replace('%', "\\%")
        .replace('_', "\\_");
    format!("%{escaped}%")
}

fn search_excerpt(content: &str, normalized_query: &str, max_chars: usize) -> String {
    let chars = content.chars().collect::<Vec<_>>();
    if chars.len() <= max_chars {
        return content.into();
    }
    let normalized = content.to_lowercase();
    let match_char = normalized
        .find(normalized_query)
        .map(|index| normalized[..index].chars().count())
        .unwrap_or(0)
        .min(chars.len());
    let half = max_chars / 2;
    let start = match_char.saturating_sub(half);
    let end = start.saturating_add(max_chars).min(chars.len());
    let mut excerpt = chars[start..end].iter().collect::<String>();
    if start > 0 {
        excerpt.insert(0, '…');
    }
    if end < chars.len() {
        excerpt.push('…');
    }
    excerpt
}

fn now() -> Result<String> {
    Ok(OffsetDateTime::now_utc().format(&Rfc3339)?)
}

fn unix_now() -> Result<i64> {
    Ok(i64::try_from(
        SystemTime::now().duration_since(UNIX_EPOCH)?.as_secs(),
    )?)
}

fn hash_text(value: &str) -> String {
    hash_bytes(value.as_bytes())
}

fn hash_bytes(value: &[u8]) -> String {
    hex::encode(Sha256::digest(value))
}

#[cfg(test)]
mod tests {
    use std::fs;

    use serde_json::{Value, json};
    use tempfile::tempdir;

    use super::{
        ProvenanceContext, append_harness_event, event_evidence, export_run, graph, initialize,
        issue_evidence_lease, search_evidence, search_metadata,
    };
    use crate::{
        config::RuntimeConfig,
        harness::{CanonicalHarnessEvent, EvidenceVisibility, HarnessKind},
    };

    fn config() -> (tempfile::TempDir, RuntimeConfig) {
        let temp = tempdir().unwrap();
        let root = temp.path().join("root");
        let state = temp.path().join("state");
        let config = temp.path().join("config");
        for path in [&root, &state, &config] {
            fs::create_dir_all(path).unwrap();
        }
        let runtime = RuntimeConfig {
            root,
            state_dir: state,
            config_dir: config,
            openshell_bin: "openshell".into(),
            docker_bin: "docker".into(),
        };
        runtime
            .save_settings(&crate::config::ControllerSettings::default())
            .unwrap();
        fs::create_dir_all(runtime.audit_dir()).unwrap();
        (temp, runtime)
    }

    #[test]
    fn embedded_events_are_hash_chained_and_evidence_is_leased() {
        let (_temp, config) = config();
        let status = initialize(&config).unwrap();
        assert!(status.append_only_controls);
        let event = CanonicalHarnessEvent {
            kind: "agent.reasoning.returned".into(),
            evidence_role: "reasoning".into(),
            visibility: EvidenceVisibility::Restricted,
            source_event_id: None,
            parent_source_event_id: None,
            metadata: json!({"bytes": 12, "provider_emitted": true}),
            restricted_content: Some("review schema".into()),
        };
        let context = ProvenanceContext {
            run_id: "run-1",
            subject: "worker-1",
            segment: 1,
            harness: HarnessKind::Pi,
            provider: Some("local"),
            model: Some("model"),
            engine: Some("vllm"),
            parser: Some("reasoning-parser-v1"),
            session_id: Some("session"),
            actor: "host-observer",
        };
        let receipt = append_harness_event(&config, &context, &event).unwrap();
        let graph = graph(&config, "run-1", 100).unwrap();
        assert_eq!(graph.nodes.len(), 1);
        assert!(graph.metadata_only);
        let lease = issue_evidence_lease(&config, "run-1", "operator", "review").unwrap();
        let detail = event_evidence(
            &config,
            "run-1",
            &format!("provenance:{}", receipt.event_id),
            &lease.token,
        )
        .unwrap();
        assert_eq!(detail.restricted_content.as_deref(), Some("review schema"));
        assert_eq!(detail.source, "harness");

        let export = config.state_dir.join("provenance-export");
        let receipt = export_run(&config, "run-1", &export, true).unwrap();
        assert_eq!(receipt.events, 1);
        let events = fs::read_to_string(export.join("events.jsonl")).unwrap();
        assert!(events.contains("review schema"));
        assert!(export.join("manifest-sha256.txt").is_file());
    }

    #[test]
    fn evidence_token_is_scoped_to_one_run() {
        let (_temp, config) = config();
        initialize(&config).unwrap();
        let lease = issue_evidence_lease(&config, "run-1", "operator", "review").unwrap();
        assert!(event_evidence(&config, "run-2", "missing", &lease.token).is_err());
    }

    fn tool_context(run_id: &str) -> ProvenanceContext<'_> {
        ProvenanceContext {
            run_id,
            subject: "worker-1",
            segment: 1,
            harness: HarnessKind::Pi,
            provider: Some("local"),
            model: Some("model"),
            engine: Some("vllm"),
            parser: Some("parser"),
            session_id: Some("session"),
            actor: "host-observer",
        }
    }

    fn append_terminal_tool(
        config: &RuntimeConfig,
        run_id: &str,
    ) -> super::ProvenanceAppendReceipt {
        append_harness_event(
            config,
            &tool_context(run_id),
            &CanonicalHarnessEvent {
                kind: "agent.tool.finished".into(),
                evidence_role: "tool-io".into(),
                visibility: EvidenceVisibility::Restricted,
                source_event_id: Some("tool-1".into()),
                parent_source_event_id: None,
                metadata: json!({"tool": "analysis"}),
                restricted_content: Some("restricted tool output".into()),
            },
        )
        .unwrap()
    }

    fn add_snapshot_receipt(
        config: &RuntimeConfig,
        temp: &tempfile::TempDir,
        run_id: &str,
        label: &str,
    ) -> crate::agent_state::ArtifactRecord {
        let source = temp.path().join(format!("{label}.json"));
        fs::write(&source, format!("{{\"snapshot\":\"{label}\"}}\n")).unwrap();
        crate::agent_state::add_artifact(
            config,
            run_id,
            "worker-1",
            &source,
            "application/json",
            "workspace-delta",
            label,
            crate::agent_state::ArtifactSensitivity::Standard,
            "host-observer",
        )
        .unwrap()
    }

    fn append_snapshot(
        config: &RuntimeConfig,
        run_id: &str,
        receipt_sha256: &str,
        observed_after: Value,
        authority: crate::agent_state::StateAuthority,
    ) -> crate::agent_state::StateEvent {
        crate::agent_state::append_event(
            config,
            run_id,
            "worker-1",
            "workspace.snapshot.captured",
            "host-observer",
            authority,
            json!({
                "segment": 1,
                "sequence": 1,
                "trigger": "tool-completed",
                "manifest_sha256": "a".repeat(64),
                "receipt_sha256": receipt_sha256,
                "added_files": 0,
                "changed_files": 0,
                "removed_files": 0,
                "artifact_versions": 0,
                "observed_after": observed_after,
            }),
        )
        .unwrap()
    }

    fn observed_after(receipt: &super::ProvenanceAppendReceipt) -> Value {
        json!([{
            "run_id": receipt.run_id,
            "event_id": receipt.event_id,
            "sequence": receipt.sequence,
            "hash": receipt.hash,
        }])
    }

    fn initialize_state_run(config: &RuntimeConfig, run_id: &str) {
        crate::agent_state::init_run(
            config,
            run_id,
            crate::agent_state::StateDomain::Scientific,
            "scientific-worker",
            "worker-1",
            None,
        )
        .unwrap();
    }

    #[test]
    fn graph_links_machine_snapshot_to_exact_observed_tool_and_receipt() {
        let (temp, config) = config();
        initialize(&config).unwrap();
        let run_id = "snapshot-run";
        initialize_state_run(&config, run_id);
        let tool = append_terminal_tool(&config, run_id);
        let artifact = add_snapshot_receipt(&config, &temp, run_id, "snapshot-1");
        let snapshot = append_snapshot(
            &config,
            run_id,
            &artifact.sha256,
            observed_after(&tool),
            crate::agent_state::StateAuthority::Machine,
        );

        let graph = graph(&config, run_id, 100).unwrap();
        assert!(graph.edges.iter().any(|edge| {
            edge.kind == "observed-after"
                && edge.source == format!("provenance:{}", tool.event_id)
                && edge.target == format!("state:{}", snapshot.event_id)
        }));
        assert!(graph.edges.iter().any(|edge| {
            edge.kind == "snapshot-receipt"
                && edge.source == format!("state:{}", snapshot.event_id)
                && edge.target == format!("artifact:{}", artifact.sha256)
        }));
        assert!(graph.metadata_only);
        assert!(
            graph
                .nodes
                .iter()
                .all(|node| node.summary != json!("restricted tool output"))
        );
    }

    #[test]
    fn graph_omits_mismatched_or_model_owned_snapshot_references() {
        let (temp, config) = config();
        initialize(&config).unwrap();
        let run_id = "mismatch-run";
        initialize_state_run(&config, run_id);
        let tool = append_terminal_tool(&config, run_id);
        for (label, reference, authority) in [
            (
                "wrong-hash",
                json!([{
                    "run_id": tool.run_id,
                    "event_id": tool.event_id,
                    "sequence": tool.sequence,
                    "hash": "b".repeat(64),
                }]),
                crate::agent_state::StateAuthority::Machine,
            ),
            (
                "wrong-run",
                json!([{
                    "run_id": "other-run",
                    "event_id": tool.event_id,
                    "sequence": tool.sequence,
                    "hash": tool.hash,
                }]),
                crate::agent_state::StateAuthority::Machine,
            ),
            (
                "wrong-sequence",
                json!([{
                    "run_id": tool.run_id,
                    "event_id": tool.event_id,
                    "sequence": tool.sequence + 1,
                    "hash": tool.hash,
                }]),
                crate::agent_state::StateAuthority::Machine,
            ),
            (
                "model-owned",
                observed_after(&tool),
                crate::agent_state::StateAuthority::Model,
            ),
        ] {
            let artifact = add_snapshot_receipt(&config, &temp, run_id, label);
            append_snapshot(&config, run_id, &artifact.sha256, reference, authority);
        }

        let graph = graph(&config, run_id, 100).unwrap();
        assert!(!graph.edges.iter().any(|edge| edge.kind == "observed-after"));
        assert_eq!(
            graph
                .edges
                .iter()
                .filter(|edge| edge.kind == "snapshot-receipt")
                .count(),
            2
        );
    }

    #[test]
    fn graph_truncation_keeps_only_resolved_edges() {
        let (temp, config) = config();
        initialize(&config).unwrap();
        let run_id = "truncated-run";
        initialize_state_run(&config, run_id);
        let tool = append_terminal_tool(&config, run_id);
        let artifact = add_snapshot_receipt(&config, &temp, run_id, "snapshot-1");
        let snapshot = append_snapshot(
            &config,
            run_id,
            &artifact.sha256,
            observed_after(&tool),
            crate::agent_state::StateAuthority::Machine,
        );

        let graph = graph(&config, run_id, 2).unwrap();
        assert!(graph.truncated);
        assert!(graph.edges.iter().any(|edge| {
            edge.kind == "observed-after" && edge.target == format!("state:{}", snapshot.event_id)
        }));
        let node_ids = graph
            .nodes
            .iter()
            .map(|node| node.id.as_str())
            .collect::<std::collections::BTreeSet<_>>();
        let artifact_ids = graph
            .artifacts
            .iter()
            .map(|artifact| artifact.id.as_str())
            .collect::<std::collections::BTreeSet<_>>();
        assert!(graph.edges.iter().all(|edge| {
            node_ids.contains(edge.source.as_str())
                && (node_ids.contains(edge.target.as_str())
                    || artifact_ids.contains(edge.target.as_str()))
        }));
    }

    #[test]
    fn metadata_and_restricted_search_keep_the_lease_boundary() {
        let (_temp, config) = config();
        initialize(&config).unwrap();
        append_harness_event(
            &config,
            &ProvenanceContext {
                run_id: "search-run",
                subject: "search-worker",
                segment: 1,
                harness: HarnessKind::OpenCode,
                provider: Some("local"),
                model: Some("search-model"),
                engine: Some("vllm"),
                parser: Some("reviewed-parser"),
                session_id: Some("search-session"),
                actor: "host-observer",
            },
            &CanonicalHarnessEvent {
                kind: "agent.tool.finished".into(),
                evidence_role: "tool-io".into(),
                visibility: EvidenceVisibility::Restricted,
                source_event_id: Some("tool-1".into()),
                parent_source_event_id: None,
                metadata: json!({"tool": "quality-control"}),
                restricted_content: Some("review schema at 50% with _token".into()),
            },
        )
        .unwrap();

        let metadata = search_metadata(&config, "quality-control", 20).unwrap();
        assert_eq!(metadata.total, 1);
        assert_eq!(metadata.hits[0].run_id, "search-run");

        let lease = issue_evidence_lease(&config, "search-run", "operator", "search").unwrap();
        let evidence = search_evidence(&config, "search-run", &lease.token, "50%", 20, 0).unwrap();
        assert_eq!(evidence.total, 1);
        assert!(evidence.hits[0].excerpt.contains("50%"));
        assert!(search_evidence(&config, "search-run", "invalid", "schema", 20, 0).is_err());
    }
}
