use std::{
    fs::{self, OpenOptions},
    net::{IpAddr, Ipv4Addr, SocketAddr, TcpListener as StdTcpListener},
    process::{Command, Stdio},
    sync::Arc,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use anyhow::{Context, Result, bail};
use hmac::{Hmac, KeyInit, Mac};
use ipnet::IpNet;
use serde::{Deserialize, Serialize};
use serde_json::json;
use sha2::Sha256;
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt, copy_bidirectional},
    net::{TcpListener, TcpStream},
    process::Command as TokioCommand,
    sync::Semaphore,
    task::JoinSet,
    time::{Instant, sleep, sleep_until, timeout},
};
use tokio_rustls::TlsAcceptor;
use uuid::Uuid;

use crate::{
    audit,
    config::{RuntimeConfig, create_private_dir, set_mode, write_private},
    openshell, pki,
    platform::{self, KubernetesPeerIdentity, RuntimeAdapter, WorkspaceTransport},
    runtime, state, workspace,
};

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum LeaseKind {
    Host,
    Lan,
    Peer,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum LeaseState {
    Starting,
    Active,
    Stopped,
    Expired,
    Failed,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum PeerTransport {
    #[default]
    HostAlias,
    /// Legacy transport retained so old stopped or failed leases can be cleaned.
    KubernetesDirectTls,
    KubernetesHostGatewayTls,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ServiceLease {
    pub schema_version: u32,
    pub id: String,
    pub kind: LeaseKind,
    pub state: LeaseState,
    pub label: String,
    pub sandbox: String,
    pub source_sandbox: Option<String>,
    pub parent_lease_id: Option<String>,
    pub target_port: u16,
    pub bind: SocketAddr,
    pub upstream: Option<SocketAddr>,
    #[serde(default)]
    pub source_endpoint: Option<String>,
    pub sources: Vec<IpNet>,
    pub client_profiles: Vec<String>,
    pub client_fingerprints: Vec<String>,
    pub expected_source_ip: Option<IpAddr>,
    pub expected_source_container: Option<String>,
    #[serde(default)]
    pub expected_source_runtime_id: Option<String>,
    #[serde(default)]
    pub source_runtime_driver: Option<RuntimeAdapter>,
    pub peer_policy_host: Option<String>,
    #[serde(default)]
    pub peer_transport: PeerTransport,
    #[serde(default)]
    pub expected_source_pod_uid: Option<String>,
    #[serde(default)]
    pub expected_target_runtime_id: Option<String>,
    #[serde(default)]
    pub expected_target_ip: Option<IpAddr>,
    #[serde(default)]
    pub expected_target_pod_uid: Option<String>,
    #[serde(default)]
    pub kubernetes_policy_names: Vec<String>,
    pub pid: Option<u32>,
    #[serde(default)]
    pub child_pid: Option<u32>,
    #[serde(default)]
    pub target_child_pid: Option<u32>,
    pub created_at_unix: u64,
    pub expires_at_unix: u64,
    pub stopped_at_unix: Option<u64>,
    pub failure: Option<String>,
}

pub fn open_host(
    config: &RuntimeConfig,
    sandbox: &str,
    target_port: u16,
    local_port: u16,
    ttl_minutes: u64,
    label: &str,
) -> Result<ServiceLease> {
    let record = state::load(config, sandbox)?;
    validate_ttl(ttl_minutes)?;
    validate_label(label)?;
    if target_port == 0 {
        bail!("target port must be nonzero");
    }
    let bind = reserve_address(IpAddr::V4(Ipv4Addr::LOCALHOST), local_port)?;
    let now = unix_now()?;
    let lease = ServiceLease {
        schema_version: 1,
        id: Uuid::new_v4().to_string(),
        kind: LeaseKind::Host,
        state: LeaseState::Starting,
        label: label.into(),
        sandbox: record.name,
        source_sandbox: None,
        parent_lease_id: None,
        target_port,
        bind,
        upstream: None,
        source_endpoint: None,
        sources: Vec::new(),
        client_profiles: Vec::new(),
        client_fingerprints: Vec::new(),
        expected_source_ip: None,
        expected_source_container: None,
        expected_source_runtime_id: None,
        source_runtime_driver: None,
        peer_policy_host: None,
        peer_transport: PeerTransport::HostAlias,
        expected_source_pod_uid: None,
        expected_target_runtime_id: None,
        expected_target_ip: None,
        expected_target_pod_uid: None,
        kubernetes_policy_names: Vec::new(),
        pid: None,
        child_pid: None,
        target_child_pid: None,
        created_at_unix: now,
        expires_at_unix: expiry(now, ttl_minutes)?,
        stopped_at_unix: None,
        failure: None,
    };
    save(config, &lease)?;
    spawn_and_wait(config, &lease)
}

pub fn open_lan(
    config: &RuntimeConfig,
    parent_id: &str,
    bind_ip: IpAddr,
    local_port: u16,
    sources: &[IpNet],
    clients: &[String],
    ttl_minutes: u64,
) -> Result<ServiceLease> {
    validate_ttl(ttl_minutes)?;
    runtime::validate_bind_ip(bind_ip)?;
    if bind_ip.is_loopback() {
        bail!("LAN leases need an assigned non-loopback address");
    }
    validate_sources(sources)?;
    let parent = load(config, parent_id)?;
    require_active(&parent, LeaseKind::Host)?;
    let now = unix_now()?;
    let expires = expiry(now, ttl_minutes)?;
    if expires > parent.expires_at_unix {
        bail!("LAN lease cannot outlive its host lease");
    }
    let fingerprints = pki::allowed_fingerprints(config, clients)?;
    pki::ensure_server(config, bind_ip)?;
    let bind = reserve_address(bind_ip, local_port)?;
    let lease = ServiceLease {
        schema_version: 1,
        id: Uuid::new_v4().to_string(),
        kind: LeaseKind::Lan,
        state: LeaseState::Starting,
        label: format!("lan-{}", parent.label),
        sandbox: parent.sandbox.clone(),
        source_sandbox: None,
        parent_lease_id: Some(parent.id.clone()),
        target_port: parent.target_port,
        bind,
        upstream: Some(parent.bind),
        source_endpoint: None,
        sources: sources.to_vec(),
        client_profiles: clients.to_vec(),
        client_fingerprints: fingerprints,
        expected_source_ip: None,
        expected_source_container: None,
        expected_source_runtime_id: None,
        source_runtime_driver: None,
        peer_policy_host: None,
        peer_transport: PeerTransport::HostAlias,
        expected_source_pod_uid: None,
        expected_target_runtime_id: None,
        expected_target_ip: None,
        expected_target_pod_uid: None,
        kubernetes_policy_names: Vec::new(),
        pid: None,
        child_pid: None,
        target_child_pid: None,
        created_at_unix: now,
        expires_at_unix: expires,
        stopped_at_unix: None,
        failure: None,
    };
    save(config, &lease)?;
    spawn_and_wait(config, &lease)
}

pub fn open_peer(
    config: &RuntimeConfig,
    source: &str,
    target: &str,
    target_port: u16,
    source_port: u16,
    ttl_minutes: u64,
) -> Result<ServiceLease> {
    validate_ttl(ttl_minutes)?;
    if source == target {
        bail!("peer source and target must differ");
    }
    if target_port == 0 {
        bail!("peer target port must be nonzero");
    }
    let source_record = state::load(config, source)?;
    let target_record = state::load(config, target)?;
    if source_record.runtime.driver != target_record.runtime.driver
        || source_record.runtime.openshell_gateway != target_record.runtime.openshell_gateway
    {
        bail!("peer sandboxes must use the same runtime driver and OpenShell gateway");
    }
    match source_record.runtime.driver {
        RuntimeAdapter::Kubernetes => open_kubernetes_peer(
            config,
            &source_record,
            &target_record,
            target_port,
            source_port,
            ttl_minutes,
        ),
        RuntimeAdapter::Docker | RuntimeAdapter::Vm => open_host_alias_peer(
            config,
            &source_record,
            &target_record,
            target_port,
            source_port,
            ttl_minutes,
        ),
        RuntimeAdapter::Podman => bail!("Podman peer service identity is not qualified"),
    }
}

fn open_host_alias_peer(
    config: &RuntimeConfig,
    source_record: &state::SandboxRecord,
    target_record: &state::SandboxRecord,
    target_port: u16,
    source_port: u16,
    ttl_minutes: u64,
) -> Result<ServiceLease> {
    let (expected_source_ip, expected_source_container, expected_source_runtime_id) =
        match source_record.runtime.driver {
            RuntimeAdapter::Docker => {
                if source_record.runtime.workspace_transport != WorkspaceTransport::Bind
                    || target_record.runtime.workspace_transport != WorkspaceTransport::Bind
                {
                    bail!("Docker peer sandboxes require bind workspace transport");
                }
                let source_runtime =
                    runtime::inspect_sandbox(&config.docker_bin, &source_record.name)?;
                let target_runtime =
                    runtime::inspect_sandbox(&config.docker_bin, &target_record.name)?;
                if source_runtime.network != target_runtime.network {
                    bail!("peer sandboxes must use the same isolated Docker network");
                }
                (
                    Some(IpAddr::V4(source_runtime.ip)),
                    Some(source_runtime.container_id),
                    None,
                )
            }
            RuntimeAdapter::Vm => {
                if source_record.runtime.workspace_transport != WorkspaceTransport::Sync
                    || target_record.runtime.workspace_transport != WorkspaceTransport::Sync
                {
                    bail!("VM peer sandboxes require sync workspace transport");
                }
                let source_id =
                    openshell::sandbox_runtime_id(&config.openshell_bin, source_record)?;
                let _target_id =
                    openshell::sandbox_runtime_id(&config.openshell_bin, target_record)?;
                (None, None, Some(source_id))
            }
            _ => bail!("host-alias peer transport is not valid for this runtime"),
        };
    let parent = open_host(
        config,
        &target_record.name,
        target_port,
        0,
        ttl_minutes,
        &format!("peer-target-{}", target_record.name),
    )?;
    let bind = reserve_address(IpAddr::V4(Ipv4Addr::LOCALHOST), source_port)?;
    let now = unix_now()?;
    let rule_name = format!("agent-sandbox-peer-{}", short_id(&parent.id));
    let endpoint = format!("host.openshell.internal:{}", parent.bind.port());
    let binaries = [
        "/usr/bin/curl",
        "/usr/bin/node",
        "/usr/bin/python3",
        "/sandbox/.local/opencode/node_modules/.bin/opencode",
        "/sandbox/.local/pi/node_modules/.bin/pi",
        "/sandbox/.local/bin/**",
    ];
    if let Err(error) = openshell::add_endpoint(
        &config.openshell_bin,
        source_record,
        &endpoint,
        &binaries,
        Some(&rule_name),
    ) {
        let _ = close(config, &parent.id, "peer setup rollback");
        return Err(error).context("failed to add the source-only peer endpoint");
    }
    if let Err(error) = probe_peer_target(config, source_record, parent.bind.port()) {
        let _ = openshell::remove_endpoint(
            &config.openshell_bin,
            source_record,
            "host.openshell.internal",
            parent.bind.port(),
        );
        let _ = close(config, &parent.id, "peer target probe rollback");
        return Err(error).context("source sandbox could not reach the target through OpenShell");
    }
    let lease = ServiceLease {
        schema_version: 1,
        id: Uuid::new_v4().to_string(),
        kind: LeaseKind::Peer,
        state: LeaseState::Starting,
        label: format!("{}-to-{}", source_record.name, target_record.name),
        sandbox: target_record.name.clone(),
        source_sandbox: Some(source_record.name.clone()),
        parent_lease_id: Some(parent.id.clone()),
        target_port,
        bind,
        upstream: Some(parent.bind),
        source_endpoint: Some(format!("host.openshell.internal:{}", parent.bind.port())),
        sources: Vec::new(),
        client_profiles: Vec::new(),
        client_fingerprints: Vec::new(),
        expected_source_ip,
        expected_source_container,
        expected_source_runtime_id,
        source_runtime_driver: Some(source_record.runtime.driver),
        peer_policy_host: Some("host.openshell.internal".into()),
        peer_transport: PeerTransport::HostAlias,
        expected_source_pod_uid: None,
        expected_target_runtime_id: None,
        expected_target_ip: None,
        expected_target_pod_uid: None,
        kubernetes_policy_names: Vec::new(),
        pid: None,
        child_pid: None,
        target_child_pid: None,
        created_at_unix: now,
        expires_at_unix: parent.expires_at_unix,
        stopped_at_unix: None,
        failure: None,
    };
    if let Err(error) = save(config, &lease) {
        let _ = openshell::remove_endpoint(
            &config.openshell_bin,
            source_record,
            "host.openshell.internal",
            parent.bind.port(),
        );
        let _ = close(config, &parent.id, "peer record rollback");
        return Err(error).context("failed to save peer lease; live setup was rolled back");
    }
    if let Err(error) = spawn_and_wait(config, &lease) {
        let _ = openshell::remove_endpoint(
            &config.openshell_bin,
            source_record,
            "host.openshell.internal",
            parent.bind.port(),
        );
        let _ = close(config, &parent.id, "peer setup rollback");
        return Err(error);
    }
    load(config, &lease.id)
}

fn open_kubernetes_peer(
    config: &RuntimeConfig,
    source_record: &state::SandboxRecord,
    target_record: &state::SandboxRecord,
    target_port: u16,
    source_port: u16,
    ttl_minutes: u64,
) -> Result<ServiceLease> {
    if source_record.runtime.workspace_transport != WorkspaceTransport::Sync
        || target_record.runtime.workspace_transport != WorkspaceTransport::Sync
    {
        bail!("Kubernetes peer sandboxes require sync workspace transport");
    }
    if source_record.runtime.gateway_profile != target_record.runtime.gateway_profile {
        bail!("Kubernetes peer sandboxes must use the same gateway profile");
    }
    let source_runtime_id = openshell::sandbox_runtime_id(&config.openshell_bin, source_record)?;
    let target_runtime_id = openshell::sandbox_runtime_id(&config.openshell_bin, target_record)?;
    let (source_observer, source_identity) =
        platform::kubernetes_peer_identity(config, source_record)?;
    let (target_observer, target_identity) =
        platform::kubernetes_peer_identity(config, target_record)?;
    if source_observer.namespace != target_observer.namespace
        || source_observer.command != target_observer.command
        || source_observer.transport != target_observer.transport
        || source_observer.ssh_host != target_observer.ssh_host
    {
        bail!("Kubernetes peer sandboxes must use the same cluster observer");
    }
    let gateway_ip = source_observer
        .peer_gateway_ip
        .context("Kubernetes peer gateway IP is not configured")?;
    runtime::validate_bind_ip(gateway_ip)
        .context("Kubernetes peer gateway IP is not assigned to this controller host")?;
    let source_relay_port = select_sandbox_port(config, source_record, source_port, true)?;
    let gateway_bind = reserve_address(gateway_ip, 0)?;
    let parent = open_host(
        config,
        &target_record.name,
        target_port,
        0,
        ttl_minutes,
        &format!("peer-target-{}", target_record.name),
    )?;
    let id = Uuid::new_v4().to_string();
    if let Err(error) = prepare_kubernetes_peer_material(config, &id, source_record, gateway_ip) {
        let _ = close(config, &parent.id, "Kubernetes peer material rollback");
        return Err(error);
    }
    let now = unix_now()?;
    let endpoint = format_endpoint(&gateway_ip.to_string(), gateway_bind.port());
    let lease = ServiceLease {
        schema_version: 1,
        id,
        kind: LeaseKind::Peer,
        state: LeaseState::Starting,
        label: format!("{}-to-{}", source_record.name, target_record.name),
        sandbox: target_record.name.clone(),
        source_sandbox: Some(source_record.name.clone()),
        parent_lease_id: Some(parent.id.clone()),
        target_port,
        bind: SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), source_relay_port),
        upstream: Some(gateway_bind),
        source_endpoint: Some(endpoint),
        sources: Vec::new(),
        client_profiles: Vec::new(),
        client_fingerprints: Vec::new(),
        expected_source_ip: Some(source_identity.pod_ip),
        expected_source_container: None,
        expected_source_runtime_id: Some(source_runtime_id),
        source_runtime_driver: Some(RuntimeAdapter::Kubernetes),
        peer_policy_host: Some(gateway_ip.to_string()),
        peer_transport: PeerTransport::KubernetesHostGatewayTls,
        expected_source_pod_uid: Some(source_identity.pod_uid),
        expected_target_runtime_id: Some(target_runtime_id),
        expected_target_ip: Some(target_identity.pod_ip),
        expected_target_pod_uid: Some(target_identity.pod_uid),
        kubernetes_policy_names: Vec::new(),
        pid: None,
        child_pid: None,
        target_child_pid: None,
        created_at_unix: now,
        expires_at_unix: parent.expires_at_unix,
        stopped_at_unix: None,
        failure: None,
    };
    if let Err(error) = save(config, &lease) {
        let _ = cleanup_kubernetes_peer_material(config, &lease);
        let _ = close(config, &parent.id, "Kubernetes peer record rollback");
        return Err(error);
    }
    match spawn_and_wait(config, &lease) {
        Ok(active) => Ok(active),
        Err(error) => {
            let current = load(config, &lease.id).unwrap_or(lease);
            let _ = cleanup_peer_policy(config, &current);
            let _ = cleanup_kubernetes_peer_material(config, &current);
            let _ = close(config, &parent.id, "Kubernetes peer setup rollback");
            Err(error)
        }
    }
}

fn probe_peer_target(
    config: &RuntimeConfig,
    source: &state::SandboxRecord,
    target_port: u16,
) -> Result<()> {
    openshell::exec_capture(
        &config.openshell_bin,
        source,
        &[
            "/usr/bin/python3".into(),
            workspace::PEER_PROXY_TARGET.into(),
            "--target-host".into(),
            "host.openshell.internal".into(),
            "--target-port".into(),
            target_port.to_string().into(),
            "--probe".into(),
        ],
    )?;
    Ok(())
}

fn select_sandbox_port(
    config: &RuntimeConfig,
    record: &state::SandboxRecord,
    requested: u16,
    loopback_only: bool,
) -> Result<u16> {
    let bind_host = if loopback_only {
        "127.0.0.1"
    } else {
        "0.0.0.0"
    };
    if requested != 0 {
        probe_sandbox_bind(config, record, bind_host, requested)?;
        return Ok(requested);
    }
    for _ in 0..32 {
        let mut bytes = [0_u8; 2];
        getrandom::fill(&mut bytes).context("operating system randomness unavailable")?;
        let candidate = 30_000 + (u16::from_be_bytes(bytes) % 25_000);
        if probe_sandbox_bind(config, record, bind_host, candidate).is_ok() {
            return Ok(candidate);
        }
    }
    bail!("could not select a free sandbox peer port after 32 attempts")
}

fn probe_sandbox_bind(
    config: &RuntimeConfig,
    record: &state::SandboxRecord,
    host: &str,
    port: u16,
) -> Result<()> {
    let script =
        "import socket,sys; s=socket.socket(); s.bind((sys.argv[1], int(sys.argv[2]))); s.close()";
    openshell::exec_capture(
        &config.openshell_bin,
        record,
        &[
            "/usr/bin/python3".into(),
            "-c".into(),
            script.into(),
            host.into(),
            port.to_string().into(),
        ],
    )
    .map(|_| ())
}

fn format_endpoint(host: &str, port: u16) -> String {
    match host.parse::<IpAddr>() {
        Ok(IpAddr::V6(_)) => format!("[{host}]:{port}"),
        _ => format!("{host}:{port}"),
    }
}

fn prepare_kubernetes_peer_material(
    config: &RuntimeConfig,
    lease_id: &str,
    source: &state::SandboxRecord,
    gateway_ip: IpAddr,
) -> Result<()> {
    validate_id(lease_id)?;
    let root = peer_material_dir(config, lease_id)?;
    let source_dir = root.join("source");
    let host_dir = root.join("host");
    create_private_dir(&source_dir)?;
    create_private_dir(&host_dir)?;
    let mut capability = [0_u8; 32];
    getrandom::fill(&mut capability).context("operating system randomness unavailable")?;
    let capability = format!("{}\n", hex::encode(capability));
    pki::ensure_server(config, gateway_ip)?;
    copy_private(&pki::ca_cert_path(config), &source_dir.join("ca.crt"))?;
    write_private(&source_dir.join("token"), capability.as_bytes())?;
    write_private(&host_dir.join("token"), capability.as_bytes())?;

    let result = upload_peer_material(config, source, lease_id, "source", &source_dir);
    if let Err(error) = result {
        let _ = remove_remote_peer_material(config, source, lease_id, "source");
        let _ = remove_remote_peer_parent(config, source, lease_id);
        let _ = remove_local_peer_material(config, lease_id);
        return Err(error).context("failed to deliver Kubernetes peer material");
    }
    Ok(())
}

fn copy_private(source: &std::path::Path, target: &std::path::Path) -> Result<()> {
    fs::copy(source, target).with_context(|| {
        format!(
            "failed to copy private peer material to {}",
            target.display()
        )
    })?;
    set_mode(target, 0o600)
}

fn upload_peer_material(
    config: &RuntimeConfig,
    record: &state::SandboxRecord,
    lease_id: &str,
    role: &str,
    local_dir: &std::path::Path,
) -> Result<()> {
    let remote = remote_peer_material_dir(lease_id, role)?;
    openshell::exec_capture(
        &config.openshell_bin,
        record,
        &["/bin/mkdir".into(), "-p".into(), remote.clone().into()],
    )?;
    openshell::exec_capture(
        &config.openshell_bin,
        record,
        &["/bin/chmod".into(), "700".into(), remote.clone().into()],
    )?;
    for entry in fs::read_dir(local_dir)? {
        let path = entry?.path();
        let name = path
            .file_name()
            .and_then(|value| value.to_str())
            .context("peer material filename is not UTF-8")?;
        let destination = format!("{remote}/{name}");
        openshell::upload(&config.openshell_bin, record, &path, &destination, true)?;
        openshell::exec_capture(
            &config.openshell_bin,
            record,
            &["/bin/chmod".into(), "600".into(), destination.into()],
        )?;
    }
    Ok(())
}

fn peer_material_dir(config: &RuntimeConfig, lease_id: &str) -> Result<std::path::PathBuf> {
    validate_id(lease_id)?;
    Ok(config.peer_runtime_dir().join(lease_id))
}

fn remote_peer_material_dir(lease_id: &str, role: &str) -> Result<String> {
    validate_id(lease_id)?;
    if !matches!(role, "source" | "target") {
        bail!("invalid Kubernetes peer material role");
    }
    Ok(format!(
        "{}/peers/{lease_id}/{role}",
        workspace::RUNTIME_TARGET
    ))
}

fn remove_remote_peer_material(
    config: &RuntimeConfig,
    record: &state::SandboxRecord,
    lease_id: &str,
    role: &str,
) -> Result<()> {
    let directory = remote_peer_material_dir(lease_id, role)?;
    let names: &[&str] = if role == "source" {
        &["ca.crt", "token"]
    } else {
        &["server.crt", "server.key", "token"]
    };
    let files = names
        .iter()
        .map(|name| format!("{directory}/{name}"))
        .collect::<Vec<_>>();
    let refs = files.iter().map(String::as_str).collect::<Vec<_>>();
    openshell::remove_files(&config.openshell_bin, record, &refs)?;
    let _ = openshell::exec_capture(
        &config.openshell_bin,
        record,
        &["/bin/rmdir".into(), "--".into(), directory.into()],
    );
    Ok(())
}

fn remove_remote_peer_parent(
    config: &RuntimeConfig,
    record: &state::SandboxRecord,
    lease_id: &str,
) -> Result<()> {
    validate_id(lease_id)?;
    let directory = format!("{}/peers/{lease_id}", workspace::RUNTIME_TARGET);
    openshell::exec_capture(
        &config.openshell_bin,
        record,
        &[
            "/bin/sh".into(),
            "-c".into(),
            "if [ -d \"$1\" ]; then /bin/rmdir -- \"$1\"; fi".into(),
            "agent-sandbox-peer-cleanup".into(),
            directory.into(),
        ],
    )
    .map(|_| ())
}

fn remove_local_peer_material(config: &RuntimeConfig, lease_id: &str) -> Result<()> {
    let path = peer_material_dir(config, lease_id)?;
    if path.exists() {
        fs::remove_dir_all(&path)
            .with_context(|| format!("failed to remove peer material {}", path.display()))?;
    }
    Ok(())
}

fn cleanup_kubernetes_peer_material(config: &RuntimeConfig, lease: &ServiceLease) -> Result<()> {
    if !matches!(
        lease.peer_transport,
        PeerTransport::KubernetesDirectTls | PeerTransport::KubernetesHostGatewayTls
    ) {
        return Ok(());
    }
    let source_name = lease
        .source_sandbox
        .as_deref()
        .context("peer source missing")?;
    let source = state::load(config, source_name)?;
    let mut failures = Vec::new();
    if let Err(error) = remove_remote_peer_material(config, &source, &lease.id, "source") {
        failures.push(format!("source credential removal: {error:#}"));
    }
    if lease.peer_transport == PeerTransport::KubernetesDirectTls {
        let target = state::load(config, &lease.sandbox)?;
        if let Err(error) = remove_remote_peer_material(config, &target, &lease.id, "target") {
            failures.push(format!("legacy target credential removal: {error:#}"));
        }
        if let Err(error) = remove_remote_peer_parent(config, &target, &lease.id) {
            failures.push(format!("legacy target directory removal: {error:#}"));
        }
    }
    if let Err(error) = remove_remote_peer_parent(config, &source, &lease.id) {
        failures.push(format!("source directory removal: {error:#}"));
    }
    if let Err(error) = remove_local_peer_material(config, &lease.id) {
        failures.push(format!("host credential removal: {error:#}"));
    }
    if failures.is_empty() {
        Ok(())
    } else {
        bail!(
            "Kubernetes peer credential cleanup failed: {}",
            failures.join("; ")
        )
    }
}

pub fn list(config: &RuntimeConfig) -> Result<Vec<ServiceLease>> {
    let mut leases = Vec::new();
    for entry in fs::read_dir(config.leases_dir())? {
        let path = entry?.path();
        if path.extension().and_then(|value| value.to_str()) != Some("json") {
            continue;
        }
        let body = fs::read(&path)?;
        let lease: ServiceLease = serde_json::from_slice(&body)
            .with_context(|| format!("invalid lease {}", path.display()))?;
        let mut lease = normalize(lease);
        validate(&lease)?;
        if matches!(lease.state, LeaseState::Active | LeaseState::Starting)
            && lease.pid.is_some_and(|pid| !pid_alive(pid))
        {
            if lease.kind == LeaseKind::Peer {
                let _ = signal_peer_stop_for_lease(config, &lease);
                let _ = cleanup_peer_control_for_lease(config, &lease);
            }
            let cleanup = terminate_orphan_child(config, &lease);
            let failure = match &cleanup {
                Ok(()) => "supervisor process is not running; orphan child cleanup passed".into(),
                Err(error) => format!(
                    "supervisor process is not running; orphan child cleanup failed: {error}"
                ),
            };
            if cleanup.is_ok() {
                lease.child_pid = None;
                lease.target_child_pid = None;
            }
            mark_failed(config, &mut lease, &failure)?;
            if lease.kind == LeaseKind::Peer
                && let Err(error) = cleanup_peer_policy(config, &lease)
            {
                lease.failure = Some(truncate_failure(&format!(
                    "{}; peer policy cleanup failed: {error}",
                    lease.failure.as_deref().unwrap_or(&failure)
                )));
                save(config, &lease)?;
            }
            if lease.kind == LeaseKind::Peer {
                let _ = cleanup_kubernetes_peer_material(config, &lease);
            }
        }
        leases.push(lease);
    }
    let active_parents: std::collections::BTreeSet<String> = leases
        .iter()
        .filter(|lease| matches!(lease.state, LeaseState::Active | LeaseState::Starting))
        .map(|lease| lease.id.clone())
        .collect();
    let stale_children: Vec<String> = leases
        .iter()
        .filter(|lease| {
            matches!(lease.state, LeaseState::Active | LeaseState::Starting)
                && lease
                    .parent_lease_id
                    .as_ref()
                    .is_some_and(|parent| !active_parents.contains(parent))
        })
        .map(|lease| lease.id.clone())
        .collect();
    for id in stale_children {
        close_one(config, &id, "parent lease is unavailable")?;
        if let Some(index) = leases.iter().position(|lease| lease.id == id) {
            leases[index] = load(config, &id)?;
        }
    }
    let failed_peer_parents: std::collections::BTreeSet<String> = leases
        .iter()
        .filter(|lease| lease.kind == LeaseKind::Peer && lease.state == LeaseState::Failed)
        .filter_map(|lease| lease.parent_lease_id.clone())
        .collect();
    for parent_id in failed_peer_parents {
        if leases.iter().any(|lease| {
            lease.id == parent_id
                && matches!(lease.state, LeaseState::Active | LeaseState::Starting)
        }) {
            close_one(config, &parent_id, "peer supervisor is unavailable")?;
            if let Some(index) = leases.iter().position(|lease| lease.id == parent_id) {
                leases[index] = load(config, &parent_id)?;
            }
        }
    }
    leases.sort_by(|left, right| left.created_at_unix.cmp(&right.created_at_unix));
    Ok(leases)
}

pub fn load(config: &RuntimeConfig, id: &str) -> Result<ServiceLease> {
    validate_id(id)?;
    let path = lease_path(config, id);
    let body = fs::read(&path).with_context(|| format!("unknown service lease {id}"))?;
    let lease: ServiceLease = normalize(serde_json::from_slice(&body)?);
    validate(&lease)?;
    Ok(lease)
}

pub fn close(config: &RuntimeConfig, id: &str, reason: &str) -> Result<ServiceLease> {
    validate_reason(reason)?;
    let dependents: Vec<String> = list(config)?
        .into_iter()
        .filter(|lease| {
            lease.parent_lease_id.as_deref() == Some(id)
                && matches!(lease.state, LeaseState::Active | LeaseState::Starting)
        })
        .map(|lease| lease.id)
        .collect();
    for dependent in dependents {
        close(config, &dependent, "parent lease closed")?;
    }
    close_one(config, id, reason)
}

fn close_one(config: &RuntimeConfig, id: &str, reason: &str) -> Result<ServiceLease> {
    let mut lease = load(config, id)?;
    if !matches!(lease.state, LeaseState::Active | LeaseState::Starting) {
        if lease.kind == LeaseKind::Peer {
            let mut failures = Vec::new();
            for (label, result) in [
                (
                    "control cleanup",
                    cleanup_peer_control_for_lease(config, &lease),
                ),
                ("policy cleanup", cleanup_peer_policy(config, &lease)),
                (
                    "credential cleanup",
                    cleanup_kubernetes_peer_material(config, &lease),
                ),
            ] {
                if let Err(error) = result {
                    failures.push(format!("{label}: {error:#}"));
                }
            }
            if reason != "parent lease closed"
                && let Err(error) = close_peer_parent(config, &lease, "peer lease closed")
            {
                failures.push(format!("target forward cleanup: {error:#}"));
            }
            if !failures.is_empty() {
                bail!("peer lease cleanup failed: {}", failures.join("; "));
            }
        }
        return Ok(lease);
    }
    if let Some(pid) = lease.pid {
        terminate_supervisor(pid, &lease.id)?;
        for _ in 0..50 {
            if !pid_alive(pid) {
                break;
            }
            std::thread::sleep(Duration::from_millis(100));
        }
    }
    if lease.kind == LeaseKind::Peer {
        signal_peer_stop_for_lease(config, &lease)?;
        cleanup_peer_control_for_lease(config, &lease)?;
    }
    cleanup_peer_policy(config, &lease)?;
    cleanup_kubernetes_peer_material(config, &lease)?;
    lease.state = LeaseState::Stopped;
    lease.stopped_at_unix = Some(unix_now()?);
    lease.failure = None;
    lease.child_pid = None;
    lease.target_child_pid = None;
    save(config, &lease)?;
    audit::append(
        config,
        "host-controller",
        "service.close",
        &format!("lease/{}", lease.id),
        "passed",
        json!({"reason": reason, "kind": lease.kind}),
    )?;
    if lease.kind == LeaseKind::Peer && reason != "parent lease closed" {
        close_peer_parent(config, &lease, "peer lease closed")?;
    }
    Ok(lease)
}

pub fn close_for_sandbox(config: &RuntimeConfig, sandbox: &str, reason: &str) -> Result<()> {
    state::validate_name(sandbox)?;
    let ids: Vec<String> = list(config)?
        .into_iter()
        .filter(|lease| {
            (lease.sandbox == sandbox || lease.source_sandbox.as_deref() == Some(sandbox))
                && matches!(lease.state, LeaseState::Active | LeaseState::Starting)
        })
        .map(|lease| lease.id)
        .collect();
    for id in ids {
        close(config, &id, reason)?;
    }
    Ok(())
}

pub async fn run_internal(config: RuntimeConfig, id: &str) -> Result<()> {
    let lease = load(&config, id)?;
    let result = match lease.kind {
        LeaseKind::Host => run_forward(config.clone(), lease).await,
        LeaseKind::Lan => run_ingress(config.clone(), lease).await,
        LeaseKind::Peer => run_peer(config.clone(), lease).await,
    };
    if let Err(error) = result {
        if let Ok(mut current) = load(&config, id) {
            if matches!(current.state, LeaseState::Active | LeaseState::Starting) {
                let _ = mark_failed(&config, &mut current, &error.to_string());
            }
            let _ = cleanup_peer_policy(&config, &current);
            let _ = cleanup_kubernetes_peer_material(&config, &current);
            if current.kind == LeaseKind::Host {
                let _ = close(&config, id, "host forward failed");
            } else if current.kind == LeaseKind::Peer
                && let Some(parent) = current.parent_lease_id
            {
                let _ = close(&config, &parent, "peer relay failed");
            }
        }
        return Err(error);
    }
    Ok(())
}

fn spawn_worker(config: &RuntimeConfig, lease: &ServiceLease) -> Result<()> {
    let executable = std::env::current_exe()?;
    let log_path = config.logs_dir().join(format!("lease-{}.log", lease.id));
    let mut options = OpenOptions::new();
    options.create(true).append(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let log = options.open(&log_path)?;
    set_mode(&log_path, 0o600)?;
    let mut command = Command::new(executable);
    command
        .arg("--root")
        .arg(&config.root)
        .arg("--state-dir")
        .arg(&config.state_dir)
        .arg("--config-dir")
        .arg(&config.config_dir)
        .arg("--openshell-bin")
        .arg(&config.openshell_bin)
        .arg("--docker-bin")
        .arg(&config.docker_bin)
        .arg("internal")
        .arg(match lease.kind {
            LeaseKind::Host => "forward",
            LeaseKind::Lan => "ingress",
            LeaseKind::Peer => "peer",
        })
        .arg(&lease.id)
        .stdin(Stdio::null())
        .stdout(Stdio::from(log.try_clone()?))
        .stderr(Stdio::from(log));
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        command.process_group(0);
    }
    let child = command
        .spawn()
        .context("failed to start lease supervisor")?;
    if child.id() == 0 {
        bail!("lease supervisor returned an invalid PID");
    }
    Ok(())
}

fn spawn_and_wait(config: &RuntimeConfig, lease: &ServiceLease) -> Result<ServiceLease> {
    if let Err(error) = spawn_worker(config, lease) {
        let mut failed = lease.clone();
        mark_failed(config, &mut failed, &error.to_string())?;
        return Err(error);
    }
    wait_until_started(config, &lease.id)
}

fn wait_until_started(config: &RuntimeConfig, id: &str) -> Result<ServiceLease> {
    for _ in 0..300 {
        let lease = load(config, id)?;
        match lease.state {
            LeaseState::Active => return Ok(lease),
            LeaseState::Failed => bail!(
                "lease failed: {}",
                lease.failure.unwrap_or_else(|| "unknown failure".into())
            ),
            _ => std::thread::sleep(Duration::from_millis(100)),
        }
    }
    let _ = close(config, id, "startup timeout");
    bail!("lease {id} did not become active within 30 seconds")
}

async fn run_forward(config: RuntimeConfig, mut lease: ServiceLease) -> Result<()> {
    let record = state::load(&config, &lease.sandbox)?;
    lease.pid = Some(std::process::id());
    save(&config, &lease)?;
    let log_path = config
        .logs_dir()
        .join(format!("openshell-forward-{}.log", lease.id));
    let log = OpenOptions::new()
        .create(true)
        .append(true)
        .open(&log_path)?;
    set_mode(&log_path, 0o600)?;
    let args = openshell::service_forward_args(
        &record,
        lease.target_port,
        &lease.bind.ip().to_string(),
        lease.bind.port(),
    );
    let mut child = TokioCommand::new(&config.openshell_bin)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::from(log.try_clone()?))
        .stderr(Stdio::from(log))
        .kill_on_drop(true)
        .spawn()?;
    lease.child_pid = child.id();
    save(&config, &lease)?;
    let ready = wait_for_port(lease.bind, Duration::from_secs(8)).await;
    if let Err(error) = ready {
        let _ = child.start_kill();
        let _ = child.wait().await;
        lease.child_pid = None;
        mark_failed(&config, &mut lease, &error.to_string())?;
        return Err(error);
    }
    mark_active(&config, &mut lease)?;
    let reason = match wait_for_exit_or_signal(&mut child, lease.expires_at_unix).await {
        Ok(reason) => reason,
        Err(error) => {
            let _ = child.start_kill();
            let _ = child.wait().await;
            lease.child_pid = None;
            mark_failed(&config, &mut lease, &error.to_string())?;
            return Err(error);
        }
    };
    let _ = child.start_kill();
    let _ = child.wait().await;
    lease.child_pid = None;
    mark_finished(&config, &mut lease, reason)?;
    Ok(())
}

async fn run_ingress(config: RuntimeConfig, mut lease: ServiceLease) -> Result<()> {
    lease.pid = Some(std::process::id());
    let listener = TcpListener::bind(lease.bind).await?;
    let acceptor = TlsAcceptor::from(Arc::new(pki::server_config(&config, lease.bind.ip())?));
    mark_active(&config, &mut lease)?;
    let permits = Arc::new(Semaphore::new(64));
    let mut tasks = JoinSet::new();
    let deadline =
        Instant::now() + Duration::from_secs(lease.expires_at_unix.saturating_sub(unix_now()?));
    let mut terminate = termination_signal()?;
    loop {
        tokio::select! {
            _ = sleep_until(deadline) => { mark_finished(&config, &mut lease, FinishReason::Expired)?; break; }
            _ = terminate.recv() => { mark_finished(&config, &mut lease, FinishReason::Stopped)?; break; }
            accepted = listener.accept() => {
                let (socket, peer) = accepted?;
                let route = lease.clone();
                let acceptor = acceptor.clone();
                let permit = Arc::clone(&permits).acquire_owned().await?;
                let task_config = config.clone();
                tasks.spawn(async move {
                    let _permit = permit;
                    handle_ingress(task_config, route, acceptor, socket, peer).await
                });
            }
        }
    }
    tasks.abort_all();
    while tasks.join_next().await.is_some() {}
    Ok(())
}

async fn handle_ingress(
    config: RuntimeConfig,
    lease: ServiceLease,
    acceptor: TlsAcceptor,
    socket: TcpStream,
    peer: SocketAddr,
) -> Result<()> {
    if !lease
        .sources
        .iter()
        .any(|network| network.contains(&peer.ip()))
    {
        audit::append(
            &config,
            "lan-ingress",
            "connection",
            &format!("lease/{}", lease.id),
            "denied-source",
            json!({"source_ip": peer.ip().to_string()}),
        )?;
        bail!("source address is outside the lease networks");
    }
    let mut tls = timeout(Duration::from_secs(10), acceptor.accept(socket)).await??;
    let fingerprint = pki::peer_fingerprint(&tls)?;
    if !lease.client_fingerprints.contains(&fingerprint)
        || !pki::fingerprint_is_active(&config, &fingerprint)?
    {
        audit::append(
            &config,
            "lan-ingress",
            "connection",
            &format!("lease/{}", lease.id),
            "denied-client",
            json!({"source_ip": peer.ip().to_string(), "client_fingerprint": fingerprint}),
        )?;
        bail!("client certificate is not allowed by this lease");
    }
    let upstream = lease.upstream.context("LAN lease has no upstream")?;
    let mut target = timeout(Duration::from_secs(10), TcpStream::connect(upstream)).await??;
    audit::append(
        &config,
        "lan-ingress",
        "connection",
        &format!("lease/{}", lease.id),
        "allowed",
        json!({"source_ip": peer.ip().to_string(), "client_fingerprint": fingerprint}),
    )?;
    copy_bidirectional(&mut tls, &mut target).await?;
    Ok(())
}

async fn run_peer(config: RuntimeConfig, lease: ServiceLease) -> Result<()> {
    match lease.peer_transport {
        PeerTransport::HostAlias => run_host_alias_peer(config, lease).await,
        PeerTransport::KubernetesDirectTls => {
            bail!("legacy Kubernetes direct peer leases must be closed and recreated")
        }
        PeerTransport::KubernetesHostGatewayTls => {
            run_kubernetes_host_gateway_peer(config, lease).await
        }
    }
}

async fn run_host_alias_peer(config: RuntimeConfig, mut lease: ServiceLease) -> Result<()> {
    let source_name = lease
        .source_sandbox
        .clone()
        .context("peer source missing")?;
    let source = state::load(&config, &source_name)?;
    if lease
        .source_runtime_driver
        .is_some_and(|driver| driver != source.runtime.driver)
    {
        bail!("source sandbox runtime driver changed; recreate the peer lease");
    }
    match source.runtime.driver {
        RuntimeAdapter::Docker => {
            let live = runtime::inspect_sandbox(&config.docker_bin, &source_name)?;
            if Some(IpAddr::V4(live.ip)) != lease.expected_source_ip
                || Some(live.container_id) != lease.expected_source_container
            {
                bail!("source sandbox runtime identity changed; recreate the peer lease");
            }
        }
        RuntimeAdapter::Vm | RuntimeAdapter::Kubernetes => {
            let live = openshell::sandbox_runtime_id(&config.openshell_bin, &source)?;
            if lease.expected_source_runtime_id.as_deref() != Some(live.as_str()) {
                bail!("source sandbox runtime identity changed; recreate the peer lease");
            }
        }
        RuntimeAdapter::Podman => {
            bail!("source runtime does not support source-local peer relay custody")
        }
    }
    let upstream = lease.upstream.context("peer upstream missing")?;
    let (ready_path, stop_path, ready_container, stop_container) =
        peer_control_paths(&config, &lease)?;
    clear_peer_control(
        &config,
        &source,
        &ready_path,
        &stop_path,
        &ready_container,
        &stop_container,
    )?;
    let command = vec![
        "/usr/bin/python3".into(),
        workspace::PEER_PROXY_TARGET.into(),
        "--listen-port".into(),
        lease.bind.port().to_string().into(),
        "--target-host".into(),
        "host.openshell.internal".into(),
        "--target-port".into(),
        upstream.port().to_string().into(),
        "--ready-file".into(),
        ready_container.clone().into(),
        "--stop-file".into(),
        stop_container.clone().into(),
    ];
    let log_path = config.logs_dir().join(format!("peer-{}.log", lease.id));
    let log = OpenOptions::new()
        .create(true)
        .append(true)
        .open(&log_path)?;
    set_mode(&log_path, 0o600)?;
    lease.pid = Some(std::process::id());
    save(&config, &lease)?;
    let mut child = TokioCommand::new(&config.openshell_bin)
        .args(openshell::exec_args(&source, &command, false))
        .stdin(Stdio::null())
        .stdout(Stdio::from(log.try_clone()?))
        .stderr(Stdio::from(log))
        .kill_on_drop(true)
        .spawn()?;
    lease.child_pid = child.id();
    save(&config, &lease)?;

    let outcome = async {
        let deadline = Instant::now() + Duration::from_secs(8);
        loop {
            if peer_control_exists(&config, &source, &ready_path, &ready_container)? {
                break;
            }
            if let Some(status) = child.try_wait()? {
                bail!("source-local peer relay exited before readiness with {status}");
            }
            if Instant::now() >= deadline {
                bail!("source-local peer relay did not become ready within 8 seconds");
            }
            sleep(Duration::from_millis(250)).await;
        }
        mark_active(&config, &mut lease)?;
        audit::append(
            &config,
            "peer-relay",
            "service.start",
            &format!("lease/{}", lease.id),
            "passed",
            json!({"source_sandbox": source_name, "source_loopback": lease.bind, "target_port": lease.target_port}),
        )?;
        wait_for_exit_or_signal(&mut child, lease.expires_at_unix).await
    }
    .await;
    let stop_result = signal_peer_stop(&config, &source, &lease);
    if tokio::time::timeout(Duration::from_secs(5), child.wait())
        .await
        .is_err()
    {
        let _ = child.start_kill();
        let _ = child.wait().await;
    }
    let _ = clear_peer_control(
        &config,
        &source,
        &ready_path,
        &stop_path,
        &ready_container,
        &stop_container,
    );
    stop_result?;
    lease.child_pid = None;
    save(&config, &lease)?;
    let reason = outcome?;
    mark_finished(&config, &mut lease, reason)?;
    cleanup_peer_policy(&config, &lease)?;
    close_peer_parent(&config, &lease, "peer lease ended")?;
    Ok(())
}

async fn run_kubernetes_host_gateway_peer(
    config: RuntimeConfig,
    mut lease: ServiceLease,
) -> Result<()> {
    let source_name = lease
        .source_sandbox
        .clone()
        .context("peer source missing")?;
    let source = state::load(&config, &source_name)?;
    let target = state::load(&config, &lease.sandbox)?;
    if source.runtime.driver != RuntimeAdapter::Kubernetes
        || target.runtime.driver != RuntimeAdapter::Kubernetes
    {
        bail!("Kubernetes peer lease runtime changed");
    }
    let source_runtime_id = openshell::sandbox_runtime_id(&config.openshell_bin, &source)?;
    let target_runtime_id = openshell::sandbox_runtime_id(&config.openshell_bin, &target)?;
    let (observer, source_identity) = platform::kubernetes_peer_identity(&config, &source)?;
    let (_, target_identity) = platform::kubernetes_peer_identity(&config, &target)?;
    verify_kubernetes_peer_identity(
        &lease,
        &source_runtime_id,
        &target_runtime_id,
        &source_identity,
        &target_identity,
    )?;
    let gateway = lease
        .upstream
        .context("Kubernetes peer gateway endpoint missing")?;
    if observer.peer_gateway_ip != Some(gateway.ip()) {
        bail!("Kubernetes peer gateway no longer matches the observer configuration");
    }
    let parent_id = lease
        .parent_lease_id
        .as_deref()
        .context("Kubernetes peer target forward is missing")?;
    let parent = load(&config, parent_id)?;
    require_active(&parent, LeaseKind::Host)?;
    if parent.sandbox != target.name
        || parent.target_port != lease.target_port
        || !parent.bind.ip().is_loopback()
        || parent.expires_at_unix < lease.expires_at_unix
    {
        bail!("Kubernetes peer target forward custody changed");
    }
    let (source_ready_path, source_stop_path, source_ready_container, source_stop_container) =
        peer_control_paths(&config, &lease)?;
    clear_peer_control(
        &config,
        &source,
        &source_ready_path,
        &source_stop_path,
        &source_ready_container,
        &source_stop_container,
    )?;
    let listener = TcpListener::bind(gateway)
        .await
        .with_context(|| format!("failed to bind Kubernetes peer gateway {gateway}"))?;
    let acceptor = TlsAcceptor::from(Arc::new(pki::capability_server_config(
        &config,
        gateway.ip(),
    )?));
    let capability = Arc::new(load_peer_capability(&config, &lease.id)?);
    let expected_source_ip = lease
        .expected_source_ip
        .context("Kubernetes peer source IP is missing")?;
    let (listener_stop, listener_stop_rx) = tokio::sync::oneshot::channel();
    let listener_config = config.clone();
    let listener_lease = lease.clone();
    let mut listener_task = tokio::spawn(run_kubernetes_gateway_listener(
        listener_config,
        listener_lease,
        listener,
        acceptor,
        capability,
        expected_source_ip,
        parent.bind,
        listener_stop_rx,
    ));
    let mut listener_finished = false;
    lease.pid = Some(std::process::id());
    save(&config, &lease)?;

    let mut source_child: Option<tokio::process::Child> = None;
    let outcome: Result<FinishReason> = async {
        lease.kubernetes_policy_names = platform::create_kubernetes_peer_policies(
            &observer,
            &lease.id,
            &source_identity,
            gateway.ip(),
            gateway.port(),
        )?;
        save(&config, &lease)?;
        audit::append(
            &config,
            "kubernetes-peer-policy",
            "network-policy.create",
            &format!("lease/{}", lease.id),
            "passed",
            json!({
                "namespace": source_identity.namespace,
                "source_pod_uid": source_identity.pod_uid,
                "target_pod_uid": target_identity.pod_uid,
                "gateway_ip": gateway.ip(),
                "gateway_port": gateway.port(),
                "policies": lease.kubernetes_policy_names
            }),
        )?;
        let endpoint = lease
            .source_endpoint
            .as_deref()
            .context("Kubernetes peer source endpoint missing")?;
        let rule_name = format!("agent-sandbox-peer-{}", short_id(&lease.id));
        let binaries = [
            "/usr/bin/curl",
            "/usr/bin/node",
            "/usr/bin/python3",
            "/sandbox/.local/opencode/node_modules/.bin/opencode",
            "/sandbox/.local/pi/node_modules/.bin/pi",
            "/sandbox/.local/bin/**",
        ];
        openshell::add_endpoint(
            &config.openshell_bin,
            &source,
            endpoint,
            &binaries,
            Some(&rule_name),
        )?;
        probe_kubernetes_peer_target(&config, &source, &lease)?;

        let source_material = remote_peer_material_dir(&lease.id, "source")?;
        let source_command = vec![
            "/usr/bin/python3".into(),
            workspace::PEER_PROXY_TARGET.into(),
            "--listen-port".into(),
            lease.bind.port().to_string().into(),
            "--target-host".into(),
            gateway.ip().to_string().into(),
            "--target-port".into(),
            gateway.port().to_string().into(),
            "--tls-ca".into(),
            format!("{source_material}/ca.crt").into(),
            "--tls-token".into(),
            format!("{source_material}/token").into(),
            "--ready-file".into(),
            source_ready_container.clone().into(),
            "--stop-file".into(),
            source_stop_container.clone().into(),
        ];
        let source_log_path = config.logs_dir().join(format!("peer-{}.log", lease.id));
        let source_log = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&source_log_path)?;
        set_mode(&source_log_path, 0o600)?;
        let child = TokioCommand::new(&config.openshell_bin)
            .args(openshell::exec_args(&source, &source_command, false))
            .stdin(Stdio::null())
            .stdout(Stdio::from(source_log.try_clone()?))
            .stderr(Stdio::from(source_log))
            .kill_on_drop(true)
            .spawn()?;
        lease.child_pid = child.id();
        source_child = Some(child);
        save(&config, &lease)?;
        wait_for_peer_ready(
            &config,
            &source,
            &source_ready_path,
            &source_ready_container,
            source_child
                .as_mut()
                .context("source peer relay child missing")?,
            "source-local relay",
        )
        .await?;
        mark_active(&config, &mut lease)?;
        audit::append(
            &config,
            "peer-relay",
            "service.start",
            &format!("lease/{}", lease.id),
            "passed",
            json!({
                "source_sandbox": source_name,
                "target_sandbox": target.name,
                "source_loopback": lease.bind,
                "gateway": gateway,
                "target_forward": parent.id,
                "transport": lease.peer_transport,
                "network_policies": lease.kubernetes_policy_names
            }),
        )?;
        let wait = Duration::from_secs(lease.expires_at_unix.saturating_sub(unix_now()?));
        let mut terminate = termination_signal()?;
        tokio::select! {
            status = source_child.as_mut().context("source peer relay child missing")?.wait() => {
                bail!("source peer relay exited early with {}", status?)
            }
            result = &mut listener_task => {
                listener_finished = true;
                result.context("Kubernetes peer gateway task failed")??;
                bail!("Kubernetes peer gateway stopped early")
            }
            _ = sleep(wait) => Ok(FinishReason::Expired),
            _ = terminate.recv() => Ok(FinishReason::Stopped),
        }
    }
    .await;

    let source_stop = signal_peer_stop(&config, &source, &lease);
    if let Some(child) = source_child.as_mut() {
        wait_or_kill_child(child).await;
    }
    let _ = listener_stop.send(());
    let listener_cleanup = if listener_finished {
        Ok(())
    } else {
        match timeout(Duration::from_secs(5), &mut listener_task).await {
            Ok(Ok(result)) => result,
            Ok(Err(error)) => Err(error.into()),
            Err(_) => {
                listener_task.abort();
                let _ = listener_task.await;
                Err(anyhow::anyhow!(
                    "Kubernetes peer gateway did not stop within 5 seconds"
                ))
            }
        }
    };
    let _ = clear_peer_control(
        &config,
        &source,
        &source_ready_path,
        &source_stop_path,
        &source_ready_container,
        &source_stop_container,
    );
    let policy_cleanup = cleanup_peer_policy(&config, &lease);
    let material_cleanup = cleanup_kubernetes_peer_material(&config, &lease);
    let parent_cleanup = close_peer_parent(&config, &lease, "Kubernetes peer lease ended");
    lease.child_pid = None;
    lease.target_child_pid = None;
    let cleanup_failures = [
        ("source stop", source_stop),
        ("gateway stop", listener_cleanup),
        ("NetworkPolicy removal", policy_cleanup),
        ("credential removal", material_cleanup),
        ("target forward close", parent_cleanup),
    ]
    .into_iter()
    .filter_map(|(label, result)| result.err().map(|error| format!("{label}: {error:#}")))
    .collect::<Vec<_>>();
    match (outcome, cleanup_failures.is_empty()) {
        (Ok(reason), true) => mark_finished(&config, &mut lease, reason),
        (Ok(_), false) => bail!(
            "Kubernetes peer cleanup failed: {}",
            cleanup_failures.join("; ")
        ),
        (Err(error), true) => Err(error),
        (Err(error), false) => bail!(
            "Kubernetes peer failed: {error:#}; cleanup also failed: {}",
            cleanup_failures.join("; ")
        ),
    }
}

#[allow(clippy::too_many_arguments)]
async fn run_kubernetes_gateway_listener(
    config: RuntimeConfig,
    lease: ServiceLease,
    listener: TcpListener,
    acceptor: TlsAcceptor,
    capability: Arc<[u8; 32]>,
    expected_source_ip: IpAddr,
    target: SocketAddr,
    mut stop: tokio::sync::oneshot::Receiver<()>,
) -> Result<()> {
    let permits = Arc::new(Semaphore::new(64));
    let mut tasks = JoinSet::new();
    loop {
        tokio::select! {
            _ = &mut stop => break,
            accepted = listener.accept() => {
                let (socket, peer) = accepted?;
                let permit = Arc::clone(&permits).acquire_owned().await?;
                let task_config = config.clone();
                let task_lease = lease.clone();
                let task_acceptor = acceptor.clone();
                let task_capability = Arc::clone(&capability);
                tasks.spawn(async move {
                    let _permit = permit;
                    let _ = handle_kubernetes_gateway_connection(
                        task_config,
                        task_lease,
                        task_acceptor,
                        socket,
                        peer,
                        expected_source_ip,
                        task_capability,
                        target,
                    )
                    .await;
                });
            }
        }
    }
    tasks.abort_all();
    while tasks.join_next().await.is_some() {}
    Ok(())
}

#[allow(clippy::too_many_arguments)]
async fn handle_kubernetes_gateway_connection(
    config: RuntimeConfig,
    lease: ServiceLease,
    acceptor: TlsAcceptor,
    mut socket: TcpStream,
    peer: SocketAddr,
    expected_source_ip: IpAddr,
    capability: Arc<[u8; 32]>,
    target: SocketAddr,
) -> Result<()> {
    if peer.ip() != expected_source_ip {
        audit::append(
            &config,
            "peer-gateway",
            "connection",
            &format!("lease/{}", lease.id),
            "denied-source",
            json!({"source_ip": peer.ip().to_string()}),
        )?;
        return Ok(());
    }
    let mut preface = [0_u8; 7];
    if !matches!(
        timeout(Duration::from_secs(5), socket.read_exact(&mut preface)).await,
        Ok(Ok(_))
    ) || &preface != b"PEER/1\n"
    {
        audit::append(
            &config,
            "peer-gateway",
            "connection",
            &format!("lease/{}", lease.id),
            "denied-protocol",
            json!({"source_ip": peer.ip().to_string()}),
        )?;
        return Ok(());
    }
    let mut challenge = [0_u8; 32];
    getrandom::fill(&mut challenge).context("operating system randomness unavailable")?;
    socket
        .write_all(format!("CHALLENGE {}\n", hex::encode(challenge)).as_bytes())
        .await?;
    socket.flush().await?;
    let mut response = [0_u8; 74];
    let response_valid = matches!(
        timeout(Duration::from_secs(5), socket.read_exact(&mut response)).await,
        Ok(Ok(_))
    ) && &response[..9] == b"RESPONSE "
        && response[73] == b'\n';
    let mut response_digest = [0_u8; 32];
    let response_decoded =
        response_valid && hex::decode_to_slice(&response[9..73], &mut response_digest).is_ok();
    if !response_decoded
        || !verify_peer_challenge(capability.as_ref(), &challenge, &response_digest)
    {
        audit::append(
            &config,
            "peer-gateway",
            "connection",
            &format!("lease/{}", lease.id),
            "denied-capability",
            json!({"source_ip": peer.ip().to_string()}),
        )?;
        return Ok(());
    }
    socket.write_all(b"STARTTLS\n").await?;
    socket.flush().await?;
    let mut tls = match timeout(Duration::from_secs(10), acceptor.accept(socket)).await {
        Ok(Ok(stream)) => stream,
        _ => {
            audit::append(
                &config,
                "peer-gateway",
                "connection",
                &format!("lease/{}", lease.id),
                "denied-tls",
                json!({"source_ip": peer.ip().to_string()}),
            )?;
            return Ok(());
        }
    };
    let mut upstream = timeout(Duration::from_secs(10), TcpStream::connect(target)).await??;
    tls.write_all(b"OK\n").await?;
    tls.flush().await?;
    audit::append(
        &config,
        "peer-gateway",
        "connection",
        &format!("lease/{}", lease.id),
        "allowed",
        json!({"source_ip": peer.ip().to_string()}),
    )?;
    copy_bidirectional(&mut tls, &mut upstream).await?;
    Ok(())
}

fn load_peer_capability(config: &RuntimeConfig, lease_id: &str) -> Result<[u8; 32]> {
    let path = peer_material_dir(config, lease_id)?.join("host/token");
    let capability = fs::read_to_string(&path)
        .with_context(|| format!("failed to read peer capability {}", path.display()))?;
    let capability = capability.trim();
    if capability.len() != 64 || !capability.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        bail!("peer capability file is invalid");
    }
    let mut decoded = [0_u8; 32];
    hex::decode_to_slice(capability, &mut decoded).context("peer capability is not valid hex")?;
    Ok(decoded)
}

fn verify_peer_challenge(capability: &[u8], challenge: &[u8], response: &[u8]) -> bool {
    let Ok(mut mac) = Hmac::<Sha256>::new_from_slice(capability) else {
        return false;
    };
    mac.update(challenge);
    mac.verify_slice(response).is_ok()
}

fn verify_kubernetes_peer_identity(
    lease: &ServiceLease,
    source_runtime_id: &str,
    target_runtime_id: &str,
    source: &KubernetesPeerIdentity,
    target: &KubernetesPeerIdentity,
) -> Result<()> {
    if lease.expected_source_runtime_id.as_deref() != Some(source_runtime_id)
        || lease.expected_target_runtime_id.as_deref() != Some(target_runtime_id)
        || lease.expected_source_ip != Some(source.pod_ip)
        || lease.expected_target_ip != Some(target.pod_ip)
        || lease.expected_source_pod_uid.as_deref() != Some(source.pod_uid.as_str())
        || lease.expected_target_pod_uid.as_deref() != Some(target.pod_uid.as_str())
    {
        bail!("Kubernetes peer runtime identity changed; recreate the lease");
    }
    Ok(())
}

fn probe_kubernetes_peer_target(
    config: &RuntimeConfig,
    source: &state::SandboxRecord,
    lease: &ServiceLease,
) -> Result<()> {
    let upstream = lease.upstream.context("Kubernetes peer upstream missing")?;
    let source_material = remote_peer_material_dir(&lease.id, "source")?;
    openshell::exec_capture(
        &config.openshell_bin,
        source,
        &[
            "/usr/bin/python3".into(),
            workspace::PEER_PROXY_TARGET.into(),
            "--target-host".into(),
            upstream.ip().to_string().into(),
            "--target-port".into(),
            upstream.port().to_string().into(),
            "--tls-ca".into(),
            format!("{source_material}/ca.crt").into(),
            "--tls-token".into(),
            format!("{source_material}/token").into(),
            "--probe".into(),
        ],
    )?;
    Ok(())
}

async fn wait_for_peer_ready(
    config: &RuntimeConfig,
    record: &state::SandboxRecord,
    host_path: &std::path::Path,
    container_path: &str,
    child: &mut tokio::process::Child,
    label: &str,
) -> Result<()> {
    let deadline = Instant::now() + Duration::from_secs(12);
    loop {
        if peer_control_exists(config, record, host_path, container_path)? {
            return Ok(());
        }
        if let Some(status) = child.try_wait()? {
            bail!("{label} exited before readiness with {status}");
        }
        if Instant::now() >= deadline {
            bail!("{label} did not become ready within 12 seconds");
        }
        sleep(Duration::from_millis(250)).await;
    }
}

async fn wait_or_kill_child(child: &mut tokio::process::Child) {
    if tokio::time::timeout(Duration::from_secs(5), child.wait())
        .await
        .is_err()
    {
        let _ = child.start_kill();
        let _ = child.wait().await;
    }
}

fn peer_control_paths(
    config: &RuntimeConfig,
    lease: &ServiceLease,
) -> Result<(std::path::PathBuf, std::path::PathBuf, String, String)> {
    let source = lease
        .source_sandbox
        .as_deref()
        .context("peer source missing")?;
    state::validate_name(source)?;
    validate_id(&lease.id)?;
    let directory = config
        .controls_dir()
        .join(source)
        .join("requests")
        .join(".controller-runtime");
    crate::config::create_private_dir(&directory)?;
    let stem = format!("peer-{}", lease.id);
    Ok((
        directory.join(format!("{stem}.ready")),
        directory.join(format!("{stem}.stop")),
        format!(
            "{}/.controller-runtime/{stem}.ready",
            workspace::REQUESTS_TARGET
        ),
        format!(
            "{}/.controller-runtime/{stem}.stop",
            workspace::REQUESTS_TARGET
        ),
    ))
}

fn target_peer_control_paths(
    config: &RuntimeConfig,
    lease: &ServiceLease,
) -> Result<(std::path::PathBuf, std::path::PathBuf, String, String)> {
    state::validate_name(&lease.sandbox)?;
    validate_id(&lease.id)?;
    let directory = config
        .controls_dir()
        .join(&lease.sandbox)
        .join("requests")
        .join(".controller-runtime");
    create_private_dir(&directory)?;
    let stem = format!("peer-target-{}", lease.id);
    Ok((
        directory.join(format!("{stem}.ready")),
        directory.join(format!("{stem}.stop")),
        format!(
            "{}/.controller-runtime/{stem}.ready",
            workspace::REQUESTS_TARGET
        ),
        format!(
            "{}/.controller-runtime/{stem}.stop",
            workspace::REQUESTS_TARGET
        ),
    ))
}

fn signal_peer_stop(
    config: &RuntimeConfig,
    source: &state::SandboxRecord,
    lease: &ServiceLease,
) -> Result<()> {
    let (_, stop_path, _, stop_container) = peer_control_paths(config, lease)?;
    write_private(&stop_path, b"stop\n")?;
    if source.runtime.workspace_transport == WorkspaceTransport::Sync {
        openshell::upload(
            &config.openshell_bin,
            source,
            &stop_path,
            &stop_container,
            true,
        )?;
    }
    Ok(())
}

fn signal_target_peer_stop(
    config: &RuntimeConfig,
    target: &state::SandboxRecord,
    lease: &ServiceLease,
) -> Result<()> {
    if lease.peer_transport != PeerTransport::KubernetesDirectTls {
        return Ok(());
    }
    let (_, stop_path, _, stop_container) = target_peer_control_paths(config, lease)?;
    write_private(&stop_path, b"stop\n")?;
    if target.runtime.workspace_transport == WorkspaceTransport::Sync {
        openshell::upload(
            &config.openshell_bin,
            target,
            &stop_path,
            &stop_container,
            true,
        )?;
    }
    Ok(())
}

fn signal_peer_stop_for_lease(config: &RuntimeConfig, lease: &ServiceLease) -> Result<()> {
    let source_name = lease
        .source_sandbox
        .as_deref()
        .context("peer source missing")?;
    let source = state::load(config, source_name)?;
    signal_peer_stop(config, &source, lease)?;
    if lease.peer_transport == PeerTransport::KubernetesDirectTls {
        let target = state::load(config, &lease.sandbox)?;
        signal_target_peer_stop(config, &target, lease)?;
    }
    Ok(())
}

fn cleanup_peer_control_for_lease(config: &RuntimeConfig, lease: &ServiceLease) -> Result<()> {
    let source_name = lease
        .source_sandbox
        .as_deref()
        .context("peer source missing")?;
    let source = state::load(config, source_name)?;
    let (ready_path, stop_path, ready_container, stop_container) =
        peer_control_paths(config, lease)?;
    clear_peer_control(
        config,
        &source,
        &ready_path,
        &stop_path,
        &ready_container,
        &stop_container,
    )?;
    if lease.peer_transport == PeerTransport::KubernetesDirectTls {
        let target = state::load(config, &lease.sandbox)?;
        let (ready_path, stop_path, ready_container, stop_container) =
            target_peer_control_paths(config, lease)?;
        clear_peer_control(
            config,
            &target,
            &ready_path,
            &stop_path,
            &ready_container,
            &stop_container,
        )?;
    }
    Ok(())
}

fn peer_control_exists(
    config: &RuntimeConfig,
    source: &state::SandboxRecord,
    host_path: &std::path::Path,
    container_path: &str,
) -> Result<bool> {
    if source.runtime.workspace_transport == WorkspaceTransport::Bind {
        return Ok(host_path.is_file());
    }
    openshell::file_exists(&config.openshell_bin, source, container_path)
}

fn clear_peer_control(
    config: &RuntimeConfig,
    source: &state::SandboxRecord,
    ready_path: &std::path::Path,
    stop_path: &std::path::Path,
    ready_container: &str,
    stop_container: &str,
) -> Result<()> {
    for path in [ready_path, stop_path] {
        if path.exists() {
            fs::remove_file(path)?;
        }
    }
    if source.runtime.workspace_transport == WorkspaceTransport::Sync {
        openshell::remove_files(
            &config.openshell_bin,
            source,
            &[ready_container, stop_container],
        )?;
    }
    Ok(())
}

fn close_peer_parent(config: &RuntimeConfig, lease: &ServiceLease, reason: &str) -> Result<()> {
    let Some(parent_id) = lease.parent_lease_id.as_deref() else {
        return Ok(());
    };
    let parent = load(config, parent_id)?;
    if matches!(parent.state, LeaseState::Active | LeaseState::Starting) {
        close(config, parent_id, reason)?;
    }
    Ok(())
}

#[derive(Clone, Copy)]
enum FinishReason {
    Stopped,
    Expired,
}

async fn wait_for_exit_or_signal(
    child: &mut tokio::process::Child,
    expires_at: u64,
) -> Result<FinishReason> {
    let wait = Duration::from_secs(expires_at.saturating_sub(unix_now()?));
    let mut terminate = termination_signal()?;
    tokio::select! {
        status = child.wait() => {
            let status = status?;
            bail!("OpenShell forward exited early with {status}")
        }
        _ = sleep(wait) => Ok(FinishReason::Expired),
        _ = terminate.recv() => Ok(FinishReason::Stopped),
    }
}

#[cfg(unix)]
fn termination_signal() -> Result<tokio::signal::unix::Signal> {
    Ok(tokio::signal::unix::signal(
        tokio::signal::unix::SignalKind::terminate(),
    )?)
}

async fn wait_for_port(address: SocketAddr, duration: Duration) -> Result<()> {
    let started = tokio::time::Instant::now();
    while started.elapsed() < duration {
        if TcpStream::connect(address).await.is_ok() {
            return Ok(());
        }
        sleep(Duration::from_millis(100)).await;
    }
    bail!("forward did not bind {address}")
}

fn mark_active(config: &RuntimeConfig, lease: &mut ServiceLease) -> Result<()> {
    lease.state = LeaseState::Active;
    lease.failure = None;
    save(config, lease)?;
    audit::append(
        config,
        "lease-supervisor",
        "service.start",
        &format!("lease/{}", lease.id),
        "passed",
        json!({"kind": lease.kind, "bind": lease.bind.to_string(), "sandbox": lease.sandbox, "target_port": lease.target_port, "expires_at_unix": lease.expires_at_unix}),
    )?;
    Ok(())
}

fn mark_failed(config: &RuntimeConfig, lease: &mut ServiceLease, failure: &str) -> Result<()> {
    lease.state = LeaseState::Failed;
    lease.failure = Some(truncate_failure(failure));
    lease.stopped_at_unix = Some(unix_now()?);
    save(config, lease)?;
    audit::append(
        config,
        "lease-supervisor",
        "service.start",
        &format!("lease/{}", lease.id),
        "failed",
        json!({"kind": lease.kind, "failure": lease.failure}),
    )?;
    Ok(())
}

fn mark_finished(
    config: &RuntimeConfig,
    lease: &mut ServiceLease,
    reason: FinishReason,
) -> Result<()> {
    lease.state = match reason {
        FinishReason::Stopped => LeaseState::Stopped,
        FinishReason::Expired => LeaseState::Expired,
    };
    lease.stopped_at_unix = Some(unix_now()?);
    save(config, lease)?;
    audit::append(
        config,
        "lease-supervisor",
        "service.stop",
        &format!("lease/{}", lease.id),
        "passed",
        json!({"state": lease.state}),
    )?;
    Ok(())
}

fn cleanup_peer_policy(config: &RuntimeConfig, lease: &ServiceLease) -> Result<()> {
    if lease.kind == LeaseKind::Peer
        && let (Some(source), Some(host)) = (&lease.source_sandbox, &lease.peer_policy_host)
    {
        let record = state::load(config, source)?;
        let port = lease.upstream.context("peer upstream missing")?.port();
        let endpoint_result: Result<()> = (|| {
            let active = openshell::active_policy(&config.openshell_bin, &record)?;
            if active.contains(host) && active.contains(&port.to_string()) {
                openshell::remove_endpoint(&config.openshell_bin, &record, host, port)?;
            }
            Ok(())
        })();
        let network_result = if matches!(
            lease.peer_transport,
            PeerTransport::KubernetesDirectTls | PeerTransport::KubernetesHostGatewayTls
        ) && !lease.kubernetes_policy_names.is_empty()
        {
            let profile = platform::profile_for_record(config, &record)?;
            let observer = profile
                .observer
                .context("Kubernetes peer cleanup needs its configured observer")?;
            let result = platform::remove_kubernetes_peer_policies(
                &observer,
                &lease.id,
                &lease.kubernetes_policy_names,
            );
            if result.is_ok() {
                let _ = audit::append(
                    config,
                    "kubernetes-peer-policy",
                    "network-policy.remove",
                    &format!("lease/{}", lease.id),
                    "passed",
                    json!({"policies": lease.kubernetes_policy_names}),
                );
            }
            result
        } else {
            Ok(())
        };
        endpoint_result?;
        network_result?;
    }
    Ok(())
}

fn save(config: &RuntimeConfig, lease: &ServiceLease) -> Result<()> {
    validate(lease)?;
    let mut body = serde_json::to_vec_pretty(lease)?;
    body.push(b'\n');
    write_private(&lease_path(config, &lease.id), &body)
}

fn normalize(mut lease: ServiceLease) -> ServiceLease {
    if lease.kind == LeaseKind::Peer
        && lease.peer_transport == PeerTransport::HostAlias
        && lease.source_endpoint.is_none()
        && let Some(upstream) = lease.upstream
    {
        lease.source_endpoint = Some(format!("host.openshell.internal:{}", upstream.port()));
    }
    lease
}

fn lease_path(config: &RuntimeConfig, id: &str) -> std::path::PathBuf {
    config.leases_dir().join(format!("{id}.json"))
}

fn validate(lease: &ServiceLease) -> Result<()> {
    if lease.schema_version != 1 {
        bail!("unsupported lease schema");
    }
    validate_id(&lease.id)?;
    state::validate_name(&lease.sandbox)?;
    validate_label(&lease.label)?;
    if lease.bind.port() == 0
        || lease.target_port == 0
        || lease.expires_at_unix <= lease.created_at_unix
    {
        bail!("lease ports or time bounds are invalid");
    }
    if matches!(lease.kind, LeaseKind::Host | LeaseKind::Peer) && !lease.bind.ip().is_loopback() {
        bail!("host and peer leases must bind loopback");
    }
    if lease.kind == LeaseKind::Peer {
        let source = lease
            .source_sandbox
            .as_deref()
            .context("peer lease source is missing")?;
        state::validate_name(source)?;
        let upstream = lease.upstream.context("peer upstream is missing")?;
        match lease.peer_transport {
            PeerTransport::HostAlias => {
                if lease.source_endpoint.as_deref()
                    != Some(format!("host.openshell.internal:{}", upstream.port()).as_str())
                    || lease.peer_policy_host.as_deref() != Some("host.openshell.internal")
                    || lease.parent_lease_id.is_none()
                {
                    bail!("host-alias peer lease custody fields are incomplete");
                }
                match lease
                    .source_runtime_driver
                    .unwrap_or(RuntimeAdapter::Docker)
                {
                    RuntimeAdapter::Docker => {
                        if lease.expected_source_ip.is_none()
                            || lease.expected_source_container.is_none()
                        {
                            bail!("Docker peer runtime identity is incomplete");
                        }
                    }
                    RuntimeAdapter::Vm => {
                        if lease.expected_source_runtime_id.is_none() {
                            bail!("VM peer runtime identity is incomplete");
                        }
                    }
                    RuntimeAdapter::Kubernetes | RuntimeAdapter::Podman => {
                        bail!("host-alias peer lease declares an unsupported source runtime")
                    }
                }
            }
            PeerTransport::KubernetesDirectTls => {
                let expected_endpoint =
                    format_endpoint(&upstream.ip().to_string(), upstream.port());
                if lease.source_runtime_driver != Some(RuntimeAdapter::Kubernetes)
                    || lease.parent_lease_id.is_some()
                    || lease.source_endpoint.as_deref() != Some(expected_endpoint.as_str())
                    || lease.peer_policy_host.as_deref() != Some(upstream.ip().to_string().as_str())
                    || lease.expected_source_runtime_id.is_none()
                    || lease.expected_target_runtime_id.is_none()
                    || lease.expected_source_ip.is_none()
                    || lease.expected_source_pod_uid.is_none()
                    || lease.expected_target_ip != Some(upstream.ip())
                    || lease.expected_target_pod_uid.is_none()
                    || lease.expected_source_container.is_some()
                {
                    bail!("Kubernetes peer runtime identity is incomplete");
                }
                if lease.kubernetes_policy_names.len() > 2
                    || lease
                        .kubernetes_policy_names
                        .iter()
                        .any(|name| !name.starts_with("agent-sandbox-peer-"))
                {
                    bail!("Kubernetes peer NetworkPolicy custody is invalid");
                }
                if lease.state == LeaseState::Active
                    && (lease.kubernetes_policy_names.len() != 2
                        || lease.child_pid.is_none()
                        || lease.target_child_pid.is_none())
                {
                    bail!("active Kubernetes peer lease has incomplete process custody");
                }
            }
            PeerTransport::KubernetesHostGatewayTls => {
                let expected_endpoint =
                    format_endpoint(&upstream.ip().to_string(), upstream.port());
                if lease.source_runtime_driver != Some(RuntimeAdapter::Kubernetes)
                    || lease.parent_lease_id.is_none()
                    || lease.source_endpoint.as_deref() != Some(expected_endpoint.as_str())
                    || lease.peer_policy_host.as_deref() != Some(upstream.ip().to_string().as_str())
                    || lease.expected_source_runtime_id.is_none()
                    || lease.expected_target_runtime_id.is_none()
                    || lease.expected_source_ip.is_none()
                    || lease.expected_source_pod_uid.is_none()
                    || lease.expected_target_ip.is_none()
                    || lease.expected_target_pod_uid.is_none()
                    || lease.expected_source_container.is_some()
                    || lease.target_child_pid.is_some()
                {
                    bail!("Kubernetes host-gateway peer custody is incomplete");
                }
                if lease.kubernetes_policy_names.len() > 1
                    || lease
                        .kubernetes_policy_names
                        .iter()
                        .any(|name| !name.starts_with("agent-sandbox-peer-"))
                {
                    bail!("Kubernetes peer NetworkPolicy custody is invalid");
                }
                if lease.state == LeaseState::Active
                    && (lease.kubernetes_policy_names.len() != 1 || lease.child_pid.is_none())
                {
                    bail!("active Kubernetes host-gateway peer has incomplete process custody");
                }
            }
        }
    }
    Ok(())
}

fn require_active(lease: &ServiceLease, kind: LeaseKind) -> Result<()> {
    if lease.kind != kind
        || lease.state != LeaseState::Active
        || lease.pid.is_some_and(|pid| !pid_alive(pid))
    {
        bail!("parent lease must be an active {kind:?} lease");
    }
    Ok(())
}

fn validate_sources(sources: &[IpNet]) -> Result<()> {
    if sources.is_empty() {
        bail!("at least one source network is required");
    }
    for source in sources {
        let narrow = match source {
            IpNet::V4(network) => network.prefix_len() >= 24,
            IpNet::V6(network) => network.prefix_len() >= 64,
        };
        if !narrow {
            bail!("source network {source} is broader than /24 IPv4 or /64 IPv6");
        }
    }
    Ok(())
}

fn validate_ttl(minutes: u64) -> Result<()> {
    if minutes == 0 || minutes > 7 * 24 * 60 {
        bail!("lease TTL must be between 1 minute and 7 days");
    }
    Ok(())
}

fn validate_label(label: &str) -> Result<()> {
    if label.is_empty() || label.len() > 96 || label.contains(['\n', '\r']) {
        bail!("invalid service label");
    }
    Ok(())
}

fn validate_reason(reason: &str) -> Result<()> {
    if reason.trim().is_empty() || reason.len() > 512 || reason.contains(['\n', '\r']) {
        bail!("invalid close reason");
    }
    Ok(())
}

fn validate_id(id: &str) -> Result<()> {
    if Uuid::parse_str(id).is_err() {
        bail!("lease ID must be a UUID");
    }
    Ok(())
}

fn reserve_address(ip: IpAddr, requested_port: u16) -> Result<SocketAddr> {
    let listener = StdTcpListener::bind(SocketAddr::new(ip, requested_port))?;
    let address = listener.local_addr()?;
    drop(listener);
    Ok(address)
}

fn unix_now() -> Result<u64> {
    Ok(SystemTime::now().duration_since(UNIX_EPOCH)?.as_secs())
}

fn expiry(now: u64, minutes: u64) -> Result<u64> {
    now.checked_add(minutes.checked_mul(60).context("TTL overflow")?)
        .context("expiry overflow")
}

fn short_id(id: &str) -> &str {
    id.get(..8).unwrap_or(id)
}
fn truncate_failure(value: &str) -> String {
    value.chars().take(512).collect()
}

fn pid_alive(pid: u32) -> bool {
    unsafe { libc::kill(pid as i32, 0) == 0 }
}

fn terminate_orphan_child(config: &RuntimeConfig, lease: &ServiceLease) -> Result<()> {
    if lease.kind == LeaseKind::Lan {
        return Ok(());
    }
    let mut child_pids = lease.child_pid.into_iter().collect::<Vec<_>>();
    if lease.peer_transport == PeerTransport::KubernetesDirectTls
        && let Some(pid) = lease.target_child_pid
    {
        child_pids.push(pid);
    }
    if child_pids.is_empty() {
        bail!("active host or peer lease has no recorded child PID");
    }
    let live_pids = child_pids
        .into_iter()
        .filter(|pid| pid_alive(*pid))
        .collect::<Vec<_>>();
    if live_pids.is_empty() {
        return Ok(());
    }
    for child_pid in &live_pids {
        if !child_process_matches(config, lease, *child_pid)? {
            bail!(
                "refusing to signal child PID {child_pid}; its command does not match lease {}",
                lease.id
            );
        }
    }
    let supervisor_pid = lease.pid.context("lease supervisor PID is missing")?;
    for child_pid in &live_pids {
        let process_group = unsafe { libc::getpgid(*child_pid as i32) };
        if process_group != supervisor_pid as i32 {
            bail!(
                "refusing to signal process group {process_group}; expected {supervisor_pid} for lease {}",
                lease.id
            );
        }
    }
    if unsafe { libc::kill(-(supervisor_pid as i32), libc::SIGTERM) } != 0
        && live_pids.iter().any(|pid| pid_alive(*pid))
    {
        bail!("failed to stop orphan process group {supervisor_pid}");
    }
    for _ in 0..50 {
        if live_pids.iter().all(|pid| !pid_alive(*pid)) {
            return Ok(());
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    if unsafe { libc::kill(-(supervisor_pid as i32), libc::SIGKILL) } != 0
        && live_pids.iter().any(|pid| pid_alive(*pid))
    {
        bail!("failed to kill orphan process group {supervisor_pid}");
    }
    for _ in 0..20 {
        if live_pids.iter().all(|pid| !pid_alive(*pid)) {
            return Ok(());
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    bail!("one or more orphan child processes are still present")
}

fn child_process_matches(config: &RuntimeConfig, lease: &ServiceLease, pid: u32) -> Result<bool> {
    let command = process_command(pid)?;
    let binary = config
        .openshell_bin
        .file_name()
        .and_then(|value| value.to_str())
        .unwrap_or("openshell");
    if !command.contains(binary) {
        return Ok(false);
    }
    Ok(match lease.kind {
        LeaseKind::Host => {
            command.contains("forward")
                && command.contains("service")
                && command.contains(&lease.sandbox)
                && command.contains(&lease.bind.to_string())
                && command.contains(&lease.target_port.to_string())
        }
        LeaseKind::Peer => {
            command.contains("sandbox")
                && command.contains("exec")
                && (lease
                    .source_sandbox
                    .as_ref()
                    .is_some_and(|source| command.contains(source))
                    || (lease.peer_transport == PeerTransport::KubernetesDirectTls
                        && command.contains(&lease.sandbox)))
                && command.contains(&lease.id)
        }
        LeaseKind::Lan => false,
    })
}

fn terminate_supervisor(pid: u32, lease_id: &str) -> Result<()> {
    if !process_matches(pid, lease_id)? {
        bail!("refusing to signal PID {pid}; it does not match lease {lease_id}");
    }
    let result = unsafe { libc::kill(-(pid as i32), libc::SIGTERM) };
    if result != 0 {
        let fallback = unsafe { libc::kill(pid as i32, libc::SIGTERM) };
        if fallback != 0 {
            bail!("failed to stop lease supervisor PID {pid}");
        }
    }
    Ok(())
}

fn process_matches(pid: u32, lease_id: &str) -> Result<bool> {
    let command = process_command(pid)?;
    Ok(command.contains("agent-sandbox")
        && command.contains("internal")
        && command.contains(lease_id))
}

fn process_command(pid: u32) -> Result<String> {
    let proc_cmdline = std::path::PathBuf::from(format!("/proc/{pid}/cmdline"));
    let command = if proc_cmdline.exists() {
        String::from_utf8_lossy(&fs::read(proc_cmdline)?).replace('\0', " ")
    } else {
        let output = Command::new("ps")
            .args(["-p", &pid.to_string(), "-o", "command="])
            .output()?;
        String::from_utf8_lossy(&output.stdout).into_owned()
    };
    Ok(command)
}

#[cfg(test)]
mod tests {
    use std::net::IpAddr;

    use ipnet::IpNet;

    use super::{reserve_address, validate_sources, verify_peer_challenge};

    #[test]
    fn dynamic_loopback_port_is_nonzero() {
        let address = reserve_address("127.0.0.1".parse::<IpAddr>().unwrap(), 0).unwrap();
        assert!(address.ip().is_loopback());
        assert_ne!(address.port(), 0);
    }

    #[test]
    fn broad_lan_sources_are_rejected() {
        assert!(validate_sources(&["0.0.0.0/0".parse::<IpNet>().unwrap()]).is_err());
        assert!(validate_sources(&["192.168.1.10/32".parse::<IpNet>().unwrap()]).is_ok());
    }

    #[test]
    fn peer_challenge_rejects_replay_or_tampering() {
        let key = [0x4b_u8; 32];
        let challenge = [0x19_u8; 32];
        let expected =
            hex::decode("0d61ad60c65b9b8a57b01af544c8d842d52a0f76fe4c6dd0721983669323e873")
                .unwrap();
        assert!(verify_peer_challenge(&key, &challenge, &expected));

        let mut changed = expected.clone();
        changed[0] ^= 1;
        assert!(!verify_peer_challenge(&key, &challenge, &changed));
        assert!(!verify_peer_challenge(&key, &[0x18_u8; 32], &expected));
    }
}
