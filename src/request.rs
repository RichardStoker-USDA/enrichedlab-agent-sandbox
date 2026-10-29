use std::{
    fs,
    net::IpAddr,
    path::{Component, Path, PathBuf},
};

use anyhow::{Context, Result, bail};
use ipnet::IpNet;
use serde::{Deserialize, Serialize};
use serde_json::json;
use sha2::{Digest, Sha256};
use time::{OffsetDateTime, format_description::well_known::Rfc3339};
use uuid::Uuid;

use crate::{
    audit, catalog,
    config::{RuntimeConfig, create_private_dir, write_private},
    evidence, lease, openshell, platform, policy,
    state::{self, SandboxRecord},
    workspace, workspace_sync,
};

pub const SANDBOX_REQUEST_PATH: &str = "/sandbox/control/requests";
pub const SANDBOX_DECISION_PATH: &str = "/sandbox/control/decisions";
const MAX_REQUEST_BYTES: u64 = 64 * 1024;

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct SandboxRequest {
    pub schema_version: u32,
    pub request_id: String,
    pub reason: String,
    pub created_at: String,
    #[serde(flatten)]
    pub action: RequestAction,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(tag = "kind", content = "parameters", rename_all = "kebab-case")]
pub enum RequestAction {
    HostService(HostServiceRequest),
    LanService(LanServiceRequest),
    PeerService(PeerServiceRequest),
    DomainRead(DomainRequest),
    DomainWrite(DomainRequest),
    Mount(MountRequest),
    CatalogOutput(CatalogOutputRequest),
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct HostServiceRequest {
    pub target_port: u16,
    #[serde(default)]
    pub local_port: u16,
    #[serde(default = "default_ttl")]
    pub ttl_minutes: u64,
    #[serde(default = "default_label")]
    pub label: String,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct LanServiceRequest {
    pub host_lease_id: String,
    pub bind: IpAddr,
    #[serde(default)]
    pub local_port: u16,
    pub sources: Vec<IpNet>,
    pub clients: Vec<String>,
    #[serde(default = "default_ttl")]
    pub ttl_minutes: u64,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct PeerServiceRequest {
    pub target_sandbox: String,
    pub target_port: u16,
    #[serde(default, alias = "host_port")]
    pub source_port: u16,
    #[serde(default = "default_ttl")]
    pub ttl_minutes: u64,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct DomainRequest {
    pub host: String,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct MountRequest {
    pub host_path: PathBuf,
    pub read_only: bool,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct CatalogOutputRequest {
    pub name: String,
    pub artifact_kind: String,
    pub workspace_path: PathBuf,
    pub entrypoint: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct NormalizedRequest {
    pub schema_version: u32,
    pub sandbox: String,
    pub observed_at: String,
    pub source_sha256: String,
    pub request: SandboxRequest,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct RequestDecision {
    pub schema_version: u32,
    pub request_id: String,
    pub sandbox: String,
    pub decision: DecisionKind,
    pub decided_at: String,
    pub decided_by: String,
    pub reason: String,
    pub applied_resource: Option<String>,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum DecisionKind {
    Approved,
    Rejected,
}

fn default_ttl() -> u64 {
    60
}
fn default_label() -> String {
    "requested-service".into()
}

pub fn list_pending(
    config: &RuntimeConfig,
    sandbox: Option<&str>,
) -> Result<Vec<NormalizedRequest>> {
    let records = match sandbox {
        Some(name) => vec![state::load(config, name)?],
        None => state::list(config)?,
    };
    let mut result = Vec::new();
    for record in records {
        ingest(config, &record)?;
        for entry in fs::read_dir(normalized_dir(config, &record.name))? {
            let path = entry?.path();
            if path.extension().and_then(|value| value.to_str()) != Some("json") {
                continue;
            }
            let request: NormalizedRequest = serde_json::from_slice(&fs::read(&path)?)?;
            if !decision_path(config, &record.name, &request.request.request_id).exists() {
                result.push(request);
            }
        }
    }
    result.sort_by(|left, right| left.observed_at.cmp(&right.observed_at));
    Ok(result)
}

pub fn get(config: &RuntimeConfig, sandbox: &str, request_id: &str) -> Result<NormalizedRequest> {
    validate_request_id(request_id)?;
    let record = state::load(config, sandbox)?;
    ingest(config, &record)?;
    let path = normalized_path(config, sandbox, request_id);
    serde_json::from_slice(
        &fs::read(&path).with_context(|| format!("unknown request {request_id}"))?,
    )
    .map_err(Into::into)
}

pub fn approve(
    config: &RuntimeConfig,
    sandbox: &str,
    request_id: &str,
    actor: &str,
    decision_reason: Option<&str>,
) -> Result<RequestDecision> {
    validate_actor(actor)?;
    let normalized = get(config, sandbox, request_id)?;
    if decision_path(config, sandbox, request_id).exists() {
        bail!("request {request_id} already has a decision");
    }
    let resource = apply(config, sandbox, &normalized.request.action)?;
    let reason = decision_reason.unwrap_or(&normalized.request.reason);
    validate_decision_reason(reason, "approval")?;
    let decision = RequestDecision {
        schema_version: 1,
        request_id: request_id.into(),
        sandbox: sandbox.into(),
        decision: DecisionKind::Approved,
        decided_at: OffsetDateTime::now_utc().format(&Rfc3339)?,
        decided_by: actor.into(),
        reason: reason.into(),
        applied_resource: Some(resource),
    };
    save_decision(config, &decision)?;
    audit::append(
        config,
        actor,
        "request.approve",
        &format!("sandbox/{sandbox}/request/{request_id}"),
        "passed",
        json!({"kind": action_name(&normalized.request.action), "source_sha256": normalized.source_sha256, "applied_resource": decision.applied_resource, "reason": decision.reason}),
    )?;
    Ok(decision)
}

pub fn reject(
    config: &RuntimeConfig,
    sandbox: &str,
    request_id: &str,
    actor: &str,
    reason: &str,
) -> Result<RequestDecision> {
    validate_actor(actor)?;
    validate_decision_reason(reason, "rejection")?;
    let normalized = get(config, sandbox, request_id)?;
    if decision_path(config, sandbox, request_id).exists() {
        bail!("request {request_id} already has a decision");
    }
    let decision = RequestDecision {
        schema_version: 1,
        request_id: request_id.into(),
        sandbox: sandbox.into(),
        decision: DecisionKind::Rejected,
        decided_at: OffsetDateTime::now_utc().format(&Rfc3339)?,
        decided_by: actor.into(),
        reason: reason.into(),
        applied_resource: None,
    };
    save_decision(config, &decision)?;
    audit::append(
        config,
        actor,
        "request.reject",
        &format!("sandbox/{sandbox}/request/{request_id}"),
        "passed",
        json!({"kind": action_name(&normalized.request.action), "source_sha256": normalized.source_sha256, "reason": reason}),
    )?;
    Ok(decision)
}

pub fn process_auto(config: &RuntimeConfig) -> Result<Vec<RequestDecision>> {
    let settings = config.load_settings()?;
    let mut decisions = Vec::new();
    for pending in list_pending(config, None)? {
        let record = state::load(config, &pending.sandbox)?;
        if record.approval_mode != crate::cli::ApprovalMode::AutoSafe {
            continue;
        }
        let allowed = match pending.request.action {
            RequestAction::HostService(_) => settings.custom_auto_approve_loopback_services,
            RequestAction::CatalogOutput(_) => settings.custom_auto_approve_catalog_entries,
            _ => false,
        };
        if allowed {
            decisions.push(approve(
                config,
                &pending.sandbox,
                &pending.request.request_id,
                "host-auto-safe",
                Some("Matched the configured bounded auto-safe policy"),
            )?);
        }
    }
    Ok(decisions)
}

fn validate_actor(actor: &str) -> Result<()> {
    if actor.trim().is_empty()
        || actor.len() > 128
        || actor.contains(['\n', '\r'])
        || actor.chars().any(char::is_control)
    {
        bail!("decision actor is invalid");
    }
    Ok(())
}

fn validate_decision_reason(reason: &str, decision: &str) -> Result<()> {
    if reason.trim().is_empty()
        || reason.len() > 512
        || reason.contains(['\n', '\r'])
        || reason.chars().any(char::is_control)
    {
        bail!("{decision} reason is invalid");
    }
    Ok(())
}

pub fn schema() -> serde_json::Value {
    json!({
        "$schema": "https://json-schema.org/draft/2020-12/schema",
        "title": "EnrichedLab Agent Sandbox Request",
        "type": "object",
        "required": ["schema_version", "request_id", "reason", "created_at", "kind", "parameters"],
        "properties": {
            "schema_version": {"const": 1},
            "request_id": {"type": "string", "format": "uuid"},
            "reason": {"type": "string", "minLength": 1, "maxLength": 512},
            "created_at": {"type": "string", "format": "date-time"},
            "kind": {"enum": ["host-service", "lan-service", "peer-service", "domain-read", "domain-write", "mount", "catalog-output"]},
            "parameters": {"type": "object"}
        },
        "additionalProperties": false,
        "sandbox_request_path": SANDBOX_REQUEST_PATH,
        "sandbox_decision_path": SANDBOX_DECISION_PATH,
        "notes": [
            "Use policy.local for normal domain proposals; domain request kinds are a manual fallback.",
            "LAN, peer, domain, and mount requests are never auto-approved.",
            "Catalog output requests may only name an existing literal path below /sandbox/work.",
            "Mount approval recreates the sandbox because filesystem policy and mounts are static."
        ]
    })
}

fn ingest(config: &RuntimeConfig, record: &SandboxRecord) -> Result<()> {
    create_private_dir(&normalized_dir(config, &record.name))?;
    workspace_sync::push_pending_decisions(config, record)?;
    workspace_sync::pull_requests(config, record)?;
    if !record.control_requests.exists() {
        return Ok(());
    }
    for entry in fs::read_dir(&record.control_requests)? {
        let path = entry?.path();
        let metadata = fs::symlink_metadata(&path)?;
        if !metadata.file_type().is_file() {
            record_ingest_error(config, record, &path, None, "request is not a regular file")?;
            continue;
        }
        if metadata.len() > MAX_REQUEST_BYTES {
            record_ingest_error(
                config,
                record,
                &path,
                None,
                "request exceeds the 64 KiB limit",
            )?;
            continue;
        }
        if path.extension().and_then(|value| value.to_str()) != Some("json") {
            continue;
        }
        let body = fs::read(&path)?;
        let request: SandboxRequest = match serde_json::from_slice(&body) {
            Ok(request) => request,
            Err(error) => {
                record_ingest_error(
                    config,
                    record,
                    &path,
                    Some(&body),
                    &format!("invalid request JSON: {error}"),
                )?;
                continue;
            }
        };
        if let Err(error) = validate_request(&request) {
            record_ingest_error(
                config,
                record,
                &path,
                Some(&body),
                &format!("invalid request fields: {error}"),
            )?;
            continue;
        }
        let expected = format!("{}.json", request.request_id);
        if path.file_name().and_then(|value| value.to_str()) != Some(&expected) {
            record_ingest_error(
                config,
                record,
                &path,
                Some(&body),
                "request filename does not match request_id",
            )?;
            continue;
        }
        let destination = normalized_path(config, &record.name, &request.request_id);
        if destination.exists() {
            continue;
        }
        let normalized = NormalizedRequest {
            schema_version: 1,
            sandbox: record.name.clone(),
            observed_at: OffsetDateTime::now_utc().format(&Rfc3339)?,
            source_sha256: hex::encode(Sha256::digest(&body)),
            request,
        };
        let mut output = serde_json::to_vec_pretty(&normalized)?;
        output.push(b'\n');
        write_private(&destination, &output)?;
        audit::append(
            config,
            "request-ingest",
            "request.observe",
            &format!(
                "sandbox/{}/request/{}",
                record.name, normalized.request.request_id
            ),
            "pending",
            json!({"kind": action_name(&normalized.request.action), "source_sha256": normalized.source_sha256}),
        )?;
    }
    Ok(())
}

fn record_ingest_error(
    config: &RuntimeConfig,
    record: &SandboxRecord,
    source: &Path,
    body: Option<&[u8]>,
    error: &str,
) -> Result<()> {
    let identity = body.map_or_else(
        || {
            format!(
                "{}:{}",
                source.file_name().unwrap_or_default().to_string_lossy(),
                fs::symlink_metadata(source).map_or(0, |metadata| metadata.len())
            )
            .into_bytes()
        },
        <[u8]>::to_vec,
    );
    let digest = hex::encode(Sha256::digest(&identity));
    let directory = config
        .controls_dir()
        .join(&record.name)
        .join("ingest-errors");
    create_private_dir(&directory)?;
    let destination = directory.join(format!("{digest}.json"));
    if destination.exists() {
        return Ok(());
    }
    let value = json!({
        "schema_version": 1,
        "sandbox": record.name,
        "observed_at": OffsetDateTime::now_utc().format(&Rfc3339)?,
        "source_file": source.file_name().unwrap_or_default().to_string_lossy(),
        "source_sha256": body.map(|value| hex::encode(Sha256::digest(value))),
        "error": error.chars().take(1024).collect::<String>()
    });
    let mut output = serde_json::to_vec_pretty(&value)?;
    output.push(b'\n');
    write_private(&destination, &output)?;
    audit::append(
        config,
        "request-ingest",
        "request.reject-input",
        &format!("sandbox/{}/request-input/{digest}", record.name),
        "rejected",
        json!({"source_file": value["source_file"], "error": value["error"]}),
    )?;
    Ok(())
}

fn apply(config: &RuntimeConfig, sandbox: &str, action: &RequestAction) -> Result<String> {
    match action {
        RequestAction::HostService(request) => {
            let lease = lease::open_host(
                config,
                sandbox,
                request.target_port,
                request.local_port,
                request.ttl_minutes,
                &request.label,
            )?;
            Ok(format!("lease/{}", lease.id))
        }
        RequestAction::LanService(request) => {
            let lease = lease::open_lan(
                config,
                &request.host_lease_id,
                request.bind,
                request.local_port,
                &request.sources,
                &request.clients,
                request.ttl_minutes,
            )?;
            Ok(format!("lease/{}", lease.id))
        }
        RequestAction::PeerService(request) => {
            let lease = lease::open_peer(
                config,
                sandbox,
                &request.target_sandbox,
                request.target_port,
                request.source_port,
                request.ttl_minutes,
            )?;
            Ok(format!("lease/{}", lease.id))
        }
        RequestAction::DomainRead(request) => apply_domain(config, sandbox, &request.host, false),
        RequestAction::DomainWrite(request) => apply_domain(config, sandbox, &request.host, true),
        RequestAction::Mount(request) => apply_mount(config, sandbox, request),
        RequestAction::CatalogOutput(request) => apply_catalog(config, sandbox, request),
    }
}

fn apply_catalog(
    config: &RuntimeConfig,
    sandbox: &str,
    request: &CatalogOutputRequest,
) -> Result<String> {
    let record = state::load(config, sandbox)?;
    if record.runtime.workspace_transport == platform::WorkspaceTransport::Sync {
        workspace_sync::pull(config, &record, true)
            .context("failed to pull the requested catalog output from the sandbox")?;
    }
    validate_relative_path(&request.workspace_path)?;
    let candidate = record.host_workspace.join(&request.workspace_path);
    let path = fs::canonicalize(&candidate)
        .with_context(|| format!("catalog output does not exist: {}", candidate.display()))?;
    if !path.starts_with(&record.host_workspace) {
        bail!("catalog output escapes the sandbox workspace");
    }
    let entry = catalog::add(
        config,
        &request.name,
        &request.artifact_kind,
        &path,
        request.entrypoint.as_deref(),
        Some(sandbox),
        None,
    )?;
    Ok(format!("catalog/{}", entry.name))
}

fn validate_relative_path(path: &Path) -> Result<()> {
    if path.as_os_str().is_empty()
        || path.is_absolute()
        || path.components().any(|component| {
            matches!(
                component,
                Component::ParentDir | Component::RootDir | Component::Prefix(_)
            )
        })
    {
        bail!("workspace_path must be a non-empty literal relative path");
    }
    Ok(())
}

fn apply_domain(config: &RuntimeConfig, sandbox: &str, host: &str, write: bool) -> Result<String> {
    let mut record = state::load(config, sandbox)?;
    let previous = record.clone();
    let host = policy::validate_public_host(host)?;
    let previous_write = record.write_hosts.contains(&host);
    let previous_read = record.read_hosts.contains(&host);
    if previous_write || (!write && previous_read) {
        return Ok(format!("network/{host}:443"));
    }
    let rule_name = policy::host_rule_name(&host);
    if previous_read || previous_write {
        openshell::remove_rule(&config.openshell_bin, &record, &rule_name)?;
    }
    let endpoint = format!(
        "{host}:443:{}:rest:enforce",
        if write { "full" } else { "read-only" }
    );
    let binaries = [
        "/usr/bin/curl",
        "/usr/bin/wget",
        "/usr/bin/git",
        "/usr/bin/node",
        "/usr/bin/python3",
        "/sandbox/.local/opencode/node_modules/.bin/opencode",
        "/sandbox/.local/pi/node_modules/.bin/pi",
        "/sandbox/.local/bin/**",
    ];
    if let Err(error) = openshell::add_endpoint(
        &config.openshell_bin,
        &record,
        &endpoint,
        &binaries,
        Some(&rule_name),
    ) {
        if previous_read || previous_write {
            let _ = restore_domain(config, &previous, &host, previous_write, &rule_name);
        }
        return Err(error);
    }
    record.read_hosts.retain(|candidate| candidate != &host);
    record.write_hosts.retain(|candidate| candidate != &host);
    if write {
        record.write_hosts.push(host.clone());
    } else {
        record.read_hosts.push(host.clone());
    }
    if let Err(error) = policy::sync_active(config, &record) {
        let _ = openshell::remove_rule(&config.openshell_bin, &record, &rule_name);
        if previous_read || previous_write {
            let _ = restore_domain(config, &previous, &host, previous_write, &rule_name);
        }
        let _ = policy::sync_active(config, &previous);
        return Err(error)
            .context("active domain policy could not be persisted; live change was rolled back");
    }
    Ok(format!("network/{host}:443"))
}

fn restore_domain(
    config: &RuntimeConfig,
    record: &SandboxRecord,
    host: &str,
    write: bool,
    rule_name: &str,
) -> Result<()> {
    let endpoint = format!(
        "{host}:443:{}:rest:enforce",
        if write { "full" } else { "read-only" }
    );
    let binaries = [
        "/usr/bin/curl",
        "/usr/bin/wget",
        "/usr/bin/git",
        "/usr/bin/node",
        "/usr/bin/python3",
        "/sandbox/.local/opencode/node_modules/.bin/opencode",
        "/sandbox/.local/pi/node_modules/.bin/pi",
        "/sandbox/.local/bin/**",
    ];
    openshell::add_endpoint(
        &config.openshell_bin,
        record,
        &endpoint,
        &binaries,
        Some(rule_name),
    )
}

fn apply_mount(config: &RuntimeConfig, sandbox: &str, request: &MountRequest) -> Result<String> {
    let mut record = state::load(config, sandbox)?;
    let (read_only, read_write) = if request.read_only {
        (vec![request.host_path.clone()], Vec::new())
    } else {
        (Vec::new(), vec![request.host_path.clone()])
    };
    let additions =
        workspace::materialize_extra_mounts(&read_only, &read_write, &record.host_workspace)?;
    let addition = additions
        .into_iter()
        .next()
        .context("mount request produced no mount")?;
    if record.mounts.iter().any(|mount| {
        mount.host_path == addition.host_path || mount.container_path == addition.container_path
    }) {
        bail!("requested mount duplicates an existing mount");
    }
    let previous = record.clone();
    record.mounts.push(addition.clone());
    if record.runtime.workspace_transport == platform::WorkspaceTransport::Sync {
        let evidence = evidence::snapshot(config, sandbox)
            .context("failed to capture evidence before adding a synced input")?;
        state::save(config, &record)?;
        if let Err(error) = workspace_sync::push_mount(config, &record, &addition) {
            state::save(config, &previous).context(
                "synced input failed and the prior controller record could not be restored",
            )?;
            return Err(error).context("synced input failed; controller record was restored");
        }
        audit::append(
            config,
            "host-controller",
            "sandbox.sync.mount",
            &format!("sandbox/{sandbox}"),
            "passed",
            json!({"mount_target": addition.container_path, "read_only_source": addition.read_only, "copy_semantics": true, "evidence": evidence.directory}),
        )?;
        return Ok(format!("mount:{}", addition.container_path));
    }
    policy::sync_active(config, &previous)
        .context("failed to persist the current active policy before mount recreation")?;
    let evidence = evidence::snapshot(config, sandbox)
        .context("failed to capture evidence before mount recreation")?;
    let gateway_profile = platform::profile_for_record(config, &record)?;
    let driver = platform::driver_config(&gateway_profile, &record.host_workspace, &record.mounts)?;
    record.runtime = platform::RuntimeContract::from_profile(
        &record.runtime.gateway_profile,
        &gateway_profile,
        &driver,
    );
    lease::close_for_sandbox(config, sandbox, "sandbox mount recreation")?;
    openshell::delete_and_verify(&config.openshell_bin, &previous)?;
    let recreated = openshell::create(
        &config.openshell_bin,
        &record.runtime.openshell_gateway,
        &record.name,
        &record.openshell_workspace,
        &record.route,
        &record.image,
        &record.cpu,
        &record.memory,
        &driver,
        &record.policy_path,
        record.policy_advisor_enabled,
        record.ocsf_json_enabled,
    );
    if let Err(error) = recreated {
        let rollback_profile = platform::profile_for_record(config, &previous)?;
        let rollback_driver = platform::driver_config(
            &rollback_profile,
            &previous.host_workspace,
            &previous.mounts,
        )?;
        let rollback = openshell::create(
            &config.openshell_bin,
            &previous.runtime.openshell_gateway,
            &previous.name,
            &previous.openshell_workspace,
            &previous.route,
            &previous.image,
            &previous.cpu,
            &previous.memory,
            &rollback_driver,
            &previous.policy_path,
            previous.policy_advisor_enabled,
            previous.ocsf_json_enabled,
        );
        if let Err(rollback_error) = rollback {
            bail!("mount recreation failed ({error}); rollback also failed ({rollback_error})");
        }
        return Err(error).context("mount recreation failed; original sandbox was restored");
    }
    if let Err(error) = state::save(config, &record) {
        if let Err(cleanup_error) = openshell::delete_and_verify(&config.openshell_bin, &record) {
            bail!(
                "new sandbox was created but its record could not be saved ({error}); verified cleanup also failed ({cleanup_error})"
            );
        }
        let rollback_profile = platform::profile_for_record(config, &previous)?;
        let rollback_driver = platform::driver_config(
            &rollback_profile,
            &previous.host_workspace,
            &previous.mounts,
        )?;
        let rollback = openshell::create(
            &config.openshell_bin,
            &previous.runtime.openshell_gateway,
            &previous.name,
            &previous.openshell_workspace,
            &previous.route,
            &previous.image,
            &previous.cpu,
            &previous.memory,
            &rollback_driver,
            &previous.policy_path,
            previous.policy_advisor_enabled,
            previous.ocsf_json_enabled,
        );
        if let Err(rollback_error) = rollback {
            bail!(
                "new sandbox was created but its record could not be saved ({error}); rollback also failed ({rollback_error})"
            );
        }
        return Err(error)
            .context("new sandbox record could not be saved; original sandbox was restored");
    }
    audit::append(
        config,
        "host-controller",
        "sandbox.recreate.mount",
        &format!("sandbox/{sandbox}"),
        "passed",
        json!({"mount_target": addition.container_path, "read_only": addition.read_only, "evidence": evidence.directory}),
    )?;
    Ok(format!("mount:{}", addition.container_path))
}

fn save_decision(config: &RuntimeConfig, decision: &RequestDecision) -> Result<()> {
    let mut body = serde_json::to_vec_pretty(decision)?;
    body.push(b'\n');
    write_private(
        &decision_path(config, &decision.sandbox, &decision.request_id),
        &body,
    )?;
    let record = state::load(config, &decision.sandbox)?;
    let sandbox_decision = record
        .control_decisions
        .join(format!("{}.json", decision.request_id));
    write_private(&sandbox_decision, &body)?;
    if let Err(error) = workspace_sync::push_decision(config, &record, &sandbox_decision) {
        audit::append(
            config,
            "workspace-sync",
            "sync.decision.push",
            &format!(
                "sandbox/{}/control/decisions/{}",
                record.name, decision.request_id
            ),
            "retry-pending",
            json!({"error": error.to_string()}),
        )?;
    }
    Ok(())
}

fn validate_request(request: &SandboxRequest) -> Result<()> {
    if request.schema_version != 1 {
        bail!("unsupported request schema");
    }
    validate_request_id(&request.request_id)?;
    if request.reason.trim().is_empty()
        || request.reason.len() > 512
        || request.reason.contains(['\n', '\r'])
    {
        bail!("request reason is invalid");
    }
    OffsetDateTime::parse(&request.created_at, &Rfc3339)
        .context("request created_at must be RFC3339")?;
    Ok(())
}

fn validate_request_id(id: &str) -> Result<()> {
    Uuid::parse_str(id)
        .map(|_| ())
        .context("request ID must be a UUID")
}

fn action_name(action: &RequestAction) -> &'static str {
    match action {
        RequestAction::HostService(_) => "host-service",
        RequestAction::LanService(_) => "lan-service",
        RequestAction::PeerService(_) => "peer-service",
        RequestAction::DomainRead(_) => "domain-read",
        RequestAction::DomainWrite(_) => "domain-write",
        RequestAction::Mount(_) => "mount",
        RequestAction::CatalogOutput(_) => "catalog-output",
    }
}

fn normalized_dir(config: &RuntimeConfig, sandbox: &str) -> PathBuf {
    config.controls_dir().join(sandbox).join("normalized")
}
fn normalized_path(config: &RuntimeConfig, sandbox: &str, id: &str) -> PathBuf {
    normalized_dir(config, sandbox).join(format!("{id}.json"))
}
fn decision_path(config: &RuntimeConfig, sandbox: &str, id: &str) -> PathBuf {
    config
        .controls_dir()
        .join(sandbox)
        .join("host-decisions")
        .join(format!("{id}.json"))
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::{RequestAction, SandboxRequest, validate_relative_path, validate_request};

    #[test]
    fn strict_request_deserialization_rejects_unknown_fields() {
        let value = json!({
            "schema_version": 1,
            "request_id": "d667af88-1f89-4308-880c-7c1719a63452",
            "reason": "serve a report",
            "created_at": "2026-08-29T00:00:00Z",
            "kind": "host-service",
            "parameters": {"target_port": 8000, "unexpected": true}
        });
        assert!(serde_json::from_value::<SandboxRequest>(value).is_err());
    }

    #[test]
    fn valid_request_passes_shape_checks() {
        let request = SandboxRequest {
            schema_version: 1,
            request_id: "d667af88-1f89-4308-880c-7c1719a63452".into(),
            reason: "serve a report".into(),
            created_at: "2026-08-29T00:00:00Z".into(),
            action: RequestAction::HostService(super::HostServiceRequest {
                target_port: 8000,
                local_port: 0,
                ttl_minutes: 60,
                label: "report".into(),
            }),
        };
        assert!(validate_request(&request).is_ok());
    }

    #[test]
    fn shipped_request_examples_match_the_strict_schema() {
        for body in [
            include_str!("../examples/requests/host_service.json"),
            include_str!("../examples/requests/domain_read.json"),
            include_str!("../examples/requests/catalog_output.json"),
        ] {
            let request: SandboxRequest = serde_json::from_str(body).unwrap();
            validate_request(&request).unwrap();
        }
    }

    #[test]
    fn catalog_workspace_paths_cannot_escape() {
        assert!(validate_relative_path(std::path::Path::new("output/tool")).is_ok());
        assert!(validate_relative_path(std::path::Path::new("../tool")).is_err());
        assert!(validate_relative_path(std::path::Path::new("/tmp/tool")).is_err());
    }
}
