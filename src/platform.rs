use std::{
    collections::BTreeMap,
    ffi::OsString,
    fs,
    io::Write,
    net::IpAddr,
    path::{Path, PathBuf},
    process::{Command, Stdio},
    time::{SystemTime, UNIX_EPOCH},
};

use anyhow::{Context, Result, bail};
use clap::ValueEnum;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};
use sha2::{Digest, Sha256};
use uuid::Uuid;

use crate::{
    config::{RuntimeConfig, write_private},
    openshell, runtime,
    state::{self, SandboxRecord},
    workspace::MountRecord,
};

const MAX_OBSERVER_OUTPUT: usize = 4 * 1024 * 1024;
const PEER_POLICY_MANAGER: &str = "enrichedlab-agent-sandbox";
const LEGACY_PEER_POLICY_MANAGER: &str = "openshell-agent-sandbox";

#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize, PartialEq, Eq, ValueEnum)]
#[serde(rename_all = "lowercase")]
pub enum RuntimeAdapter {
    #[default]
    Docker,
    Podman,
    Kubernetes,
    Vm,
}

impl RuntimeAdapter {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Docker => "docker",
            Self::Podman => "podman",
            Self::Kubernetes => "kubernetes",
            Self::Vm => "vm",
        }
    }
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum WorkspaceTransport {
    #[default]
    Bind,
    Sync,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct KubernetesProfile {
    pub runtime_class_name: Option<String>,
    pub node_selector: BTreeMap<String, String>,
    pub tolerations: Vec<Value>,
    pub priority_class_name: Option<String>,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum ObserverTransport {
    #[default]
    Local,
    Ssh,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct KubernetesObserver {
    pub transport: ObserverTransport,
    pub command: Vec<String>,
    pub namespace: String,
    pub kubeconfig: Option<PathBuf>,
    pub context: Option<String>,
    pub ssh_bin: PathBuf,
    pub ssh_host: Option<String>,
    pub manage_peer_network_policies: bool,
    pub peer_gateway_ip: Option<IpAddr>,
}

impl Default for KubernetesObserver {
    fn default() -> Self {
        Self {
            transport: ObserverTransport::Local,
            command: vec!["kubectl".into()],
            namespace: "default".into(),
            kubeconfig: None,
            context: None,
            ssh_bin: PathBuf::from("ssh"),
            ssh_host: None,
            manage_peer_network_policies: false,
            peer_gateway_ip: None,
        }
    }
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct KubernetesPeerIdentity {
    pub namespace: String,
    pub pod_name: String,
    pub pod_uid: String,
    pub pod_ip: IpAddr,
    pub sandbox_label_hash: String,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct GatewayProfile {
    pub enabled: bool,
    pub required: bool,
    pub openshell_gateway: String,
    pub driver: RuntimeAdapter,
    pub workspace_transport: WorkspaceTransport,
    pub bind_mounts_enabled: bool,
    pub sync_max_files: u64,
    pub sync_max_bytes: u64,
    pub kubernetes: KubernetesProfile,
    pub observer: Option<KubernetesObserver>,
}

impl Default for GatewayProfile {
    fn default() -> Self {
        Self {
            enabled: true,
            required: true,
            openshell_gateway: "openshell".into(),
            driver: RuntimeAdapter::Docker,
            workspace_transport: WorkspaceTransport::Bind,
            bind_mounts_enabled: true,
            sync_max_files: 100_000,
            sync_max_bytes: 20 * 1024 * 1024 * 1024,
            kubernetes: KubernetesProfile::default(),
            observer: None,
        }
    }
}

impl GatewayProfile {
    pub fn validate(&self, name: &str) -> Result<()> {
        validate_identifier(name, "gateway profile", 64)?;
        validate_identifier(&self.openshell_gateway, "OpenShell gateway", 128)?;
        if !self.enabled && self.required {
            bail!("gateway profile {name} cannot be required while disabled");
        }
        if self.sync_max_files == 0 || self.sync_max_files > 1_000_000 {
            bail!("gateway profile {name} has an invalid sync_max_files limit");
        }
        if self.sync_max_bytes < 1024 * 1024 || self.sync_max_bytes > 1024_u64.pow(4) {
            bail!("gateway profile {name} has an invalid sync_max_bytes limit");
        }
        match self.workspace_transport {
            WorkspaceTransport::Bind => {
                if !matches!(self.driver, RuntimeAdapter::Docker | RuntimeAdapter::Podman) {
                    bail!("gateway profile {name} cannot use bind transport with its driver");
                }
                if !self.bind_mounts_enabled {
                    bail!("gateway profile {name} uses bind transport without bind mounts enabled");
                }
            }
            WorkspaceTransport::Sync => {
                if self.bind_mounts_enabled {
                    bail!("gateway profile {name} uses sync transport but declares bind mounts");
                }
            }
        }
        if self.driver != RuntimeAdapter::Kubernetes
            && (self.kubernetes.runtime_class_name.is_some()
                || !self.kubernetes.node_selector.is_empty()
                || !self.kubernetes.tolerations.is_empty()
                || self.kubernetes.priority_class_name.is_some()
                || self.observer.is_some())
        {
            bail!("gateway profile {name} has Kubernetes settings for a non-Kubernetes driver");
        }
        if self.driver == RuntimeAdapter::Kubernetes {
            if self.workspace_transport != WorkspaceTransport::Sync {
                bail!("Kubernetes gateway profile {name} must use sync transport");
            }
            validate_kubernetes_profile(name, &self.kubernetes)?;
            if let Some(observer) = &self.observer {
                observer.validate(name)?;
            }
        }
        if self.driver == RuntimeAdapter::Vm && self.workspace_transport != WorkspaceTransport::Sync
        {
            bail!("VM gateway profile {name} must use sync transport");
        }
        Ok(())
    }
}

impl KubernetesObserver {
    fn validate(&self, profile: &str) -> Result<()> {
        if self.command.is_empty() || self.command.len() > 8 {
            bail!("gateway profile {profile} observer command must contain 1 to 8 tokens");
        }
        for token in &self.command {
            validate_command_token(token, "observer command")?;
        }
        if self.command.first().is_some_and(|token| token == "sudo")
            && !self.command.iter().any(|token| token == "-n")
        {
            bail!("gateway profile {profile} observer sudo command must include -n");
        }
        validate_dns_label(&self.namespace, "observer namespace")?;
        if let Some(context) = &self.context {
            validate_command_token(context, "Kubernetes context")?;
        }
        if let Some(kubeconfig) = &self.kubeconfig
            && !kubeconfig.is_absolute()
        {
            bail!("gateway profile {profile} observer kubeconfig must be absolute");
        }
        match self.transport {
            ObserverTransport::Local => {
                if self.ssh_host.is_some() {
                    bail!("gateway profile {profile} local observer cannot set ssh_host");
                }
            }
            ObserverTransport::Ssh => {
                let host = self
                    .ssh_host
                    .as_deref()
                    .context("SSH Kubernetes observer needs ssh_host")?;
                if host.len() > 253
                    || !host.bytes().all(|byte| {
                        byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'-' | b'_' | b'@')
                    })
                {
                    bail!("gateway profile {profile} has an invalid observer SSH host");
                }
            }
        }
        match (self.manage_peer_network_policies, self.peer_gateway_ip) {
            (true, Some(IpAddr::V4(address)))
                if !address.is_loopback()
                    && !address.is_unspecified()
                    && !address.is_multicast()
                    && (address.is_private() || address.is_link_local()) => {}
            (true, Some(_)) => bail!(
                "gateway profile {profile} peer_gateway_ip must be a private or link-local IPv4 address assigned to this controller host"
            ),
            (true, None) => bail!(
                "gateway profile {profile} needs peer_gateway_ip when peer NetworkPolicy management is enabled"
            ),
            (false, Some(_)) => bail!(
                "gateway profile {profile} cannot set peer_gateway_ip while peer NetworkPolicy management is disabled"
            ),
            (false, None) => {}
        }
        if self.peer_gateway_ip.is_some() && self.transport != ObserverTransport::Local {
            bail!(
                "gateway profile {profile} peer gateway transport needs a local Kubernetes observer"
            );
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct RuntimeContract {
    pub gateway_profile: String,
    pub openshell_gateway: String,
    pub driver: RuntimeAdapter,
    pub workspace_transport: WorkspaceTransport,
    pub runtime_class_name: Option<String>,
    pub driver_config_sha256: String,
}

impl Default for RuntimeContract {
    fn default() -> Self {
        Self {
            gateway_profile: "docker-local".into(),
            openshell_gateway: "openshell".into(),
            driver: RuntimeAdapter::Docker,
            workspace_transport: WorkspaceTransport::Bind,
            runtime_class_name: None,
            driver_config_sha256: String::new(),
        }
    }
}

impl RuntimeContract {
    pub fn from_profile(name: &str, profile: &GatewayProfile, driver_config: &str) -> Self {
        Self {
            gateway_profile: name.into(),
            openshell_gateway: profile.openshell_gateway.clone(),
            driver: profile.driver,
            workspace_transport: profile.workspace_transport,
            runtime_class_name: profile.kubernetes.runtime_class_name.clone(),
            driver_config_sha256: format!(
                "sha256:{}",
                hex::encode(Sha256::digest(driver_config.as_bytes()))
            ),
        }
    }

    pub fn validate(&self, schema_version: u32) -> Result<()> {
        validate_identifier(&self.gateway_profile, "record gateway profile", 64)?;
        validate_identifier(&self.openshell_gateway, "record OpenShell gateway", 128)?;
        if schema_version >= 3
            && (self.driver_config_sha256.len() != 71
                || !self.driver_config_sha256.starts_with("sha256:")
                || !self.driver_config_sha256[7..]
                    .bytes()
                    .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase()))
        {
            bail!("sandbox record driver config digest is invalid");
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct GatewayHealth {
    pub schema_version: u32,
    pub profile: String,
    pub gateway: String,
    pub expected_driver: RuntimeAdapter,
    pub observed_driver: Option<String>,
    pub advertised_driver: Option<String>,
    pub driver_version: Option<String>,
    pub gateway_version: Option<String>,
    pub server: Option<String>,
    pub status: String,
    pub passed: bool,
    pub observed_at_unix: u64,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct KubernetesRuntimeProof {
    pub namespace: String,
    pub sandbox_resource: Value,
    pub pod: Value,
    pub runtime_class: Value,
    pub network_policies: Vec<Value>,
    pub requested_runtime_class: Option<String>,
    pub observed_runtime_class: Option<String>,
    pub runtime_handler: Option<String>,
    pub api_binding_passed: bool,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct RuntimeProof {
    pub schema_version: u32,
    pub sandbox: String,
    pub gateway_profile: String,
    pub gateway: String,
    pub expected_driver: RuntimeAdapter,
    pub observed_driver: Option<String>,
    pub advertised_driver: Option<String>,
    pub driver_version: Option<String>,
    pub gateway_version: Option<String>,
    pub workspace_transport: WorkspaceTransport,
    pub sandbox_phase: Option<String>,
    pub docker_container_id: Option<String>,
    pub docker_network: Option<String>,
    pub kubernetes: Option<KubernetesRuntimeProof>,
    pub result: String,
    pub observed_at_unix: u64,
    pub limitations: Vec<String>,
}

pub fn default_gateway_profiles() -> BTreeMap<String, GatewayProfile> {
    BTreeMap::from([("docker-local".into(), GatewayProfile::default())])
}

pub fn driver_config(
    profile: &GatewayProfile,
    primary: &Path,
    mounts: &[MountRecord],
) -> Result<String> {
    profile.validate("selected")?;
    let inner = match profile.driver {
        RuntimeAdapter::Docker | RuntimeAdapter::Podman
            if profile.workspace_transport == WorkspaceTransport::Bind =>
        {
            let mut values = vec![json!({
                "type": "bind",
                "source": primary,
                "target": "/sandbox/work",
                "read_only": false
            })];
            for mount in mounts {
                values.push(json!({
                    "type": "bind",
                    "source": mount.host_path,
                    "target": mount.container_path,
                    "read_only": mount.read_only
                }));
            }
            json!({"mounts": values})
        }
        RuntimeAdapter::Kubernetes => {
            let mut pod = Map::new();
            if let Some(runtime_class) = &profile.kubernetes.runtime_class_name {
                pod.insert("runtime_class_name".into(), json!(runtime_class));
            }
            if !profile.kubernetes.node_selector.is_empty() {
                pod.insert(
                    "node_selector".into(),
                    json!(profile.kubernetes.node_selector),
                );
            }
            if !profile.kubernetes.tolerations.is_empty() {
                pod.insert("tolerations".into(), json!(profile.kubernetes.tolerations));
            }
            if let Some(priority) = &profile.kubernetes.priority_class_name {
                pod.insert("priority_class_name".into(), json!(priority));
            }
            Value::Object(Map::from_iter([("pod".into(), Value::Object(pod))]))
        }
        RuntimeAdapter::Docker | RuntimeAdapter::Podman | RuntimeAdapter::Vm => json!({}),
    };
    let mut envelope = Map::new();
    envelope.insert(profile.driver.as_str().into(), inner);
    Ok(serde_json::to_string(&Value::Object(envelope))?)
}

pub fn profile_for_record(
    config: &RuntimeConfig,
    record: &SandboxRecord,
) -> Result<GatewayProfile> {
    let settings = config.load_settings()?;
    let profile = settings
        .gateway_profile(&record.runtime.gateway_profile)?
        .clone();
    if profile.openshell_gateway != record.runtime.openshell_gateway
        || profile.driver != record.runtime.driver
        || profile.workspace_transport != record.runtime.workspace_transport
    {
        bail!(
            "gateway profile {} drifted from sandbox {} runtime contract",
            record.runtime.gateway_profile,
            record.name
        );
    }
    Ok(profile)
}

pub fn observe_gateway(
    config: &RuntimeConfig,
    profile_name: &str,
    profile: &GatewayProfile,
) -> Result<GatewayHealth> {
    let value = openshell::gateway_info(&config.openshell_bin, &profile.openshell_gateway)?;
    let driver = value
        .get("compute_drivers")
        .and_then(Value::as_array)
        .and_then(|drivers| {
            drivers.iter().find(|item| {
                item.get("name").and_then(Value::as_str) == Some(profile.driver.as_str())
                    || item
                        .pointer("/capabilities/driver_name")
                        .and_then(Value::as_str)
                        .is_some_and(|name| {
                            name == profile.driver.as_str()
                                || name == format!("openshell-driver-{}", profile.driver.as_str())
                        })
            })
        });
    let observed_driver = driver
        .and_then(|item| item.get("name"))
        .and_then(Value::as_str)
        .map(str::to_string);
    Ok(GatewayHealth {
        schema_version: 1,
        profile: profile_name.into(),
        gateway: profile.openshell_gateway.clone(),
        expected_driver: profile.driver,
        observed_driver: observed_driver.clone(),
        advertised_driver: driver
            .and_then(|item| item.pointer("/capabilities/driver_name"))
            .and_then(Value::as_str)
            .map(str::to_string),
        driver_version: driver
            .and_then(|item| item.pointer("/capabilities/driver_version"))
            .and_then(Value::as_str)
            .map(str::to_string),
        gateway_version: value
            .get("version")
            .and_then(Value::as_str)
            .map(str::to_string),
        server: value
            .get("server")
            .and_then(Value::as_str)
            .map(str::to_string),
        status: value
            .get("status")
            .and_then(Value::as_str)
            .unwrap_or("unknown")
            .into(),
        passed: observed_driver.as_deref() == Some(profile.driver.as_str())
            && value.get("status").and_then(Value::as_str) == Some("healthy"),
        observed_at_unix: unix_now()?,
    })
}

pub fn observe(config: &RuntimeConfig, record: &SandboxRecord) -> Result<RuntimeProof> {
    let profile = profile_for_record(config, record)?;
    let gateway = observe_gateway(config, &record.runtime.gateway_profile, &profile)?;
    let sandbox_body = openshell::sandbox_get(&config.openshell_bin, record)?;
    let sandbox_value: Value =
        serde_json::from_str(&sandbox_body).context("OpenShell sandbox record was not JSON")?;
    let sandbox_phase = sandbox_value
        .get("phase")
        .or_else(|| sandbox_value.get("status"))
        .or_else(|| sandbox_value.get("state"))
        .and_then(Value::as_str)
        .map(str::to_string);
    let mut docker_container_id = None;
    let mut docker_network = None;
    let mut limitations = Vec::new();
    if profile.driver == RuntimeAdapter::Docker {
        match runtime::inspect_sandbox(&config.docker_bin, &record.name) {
            Ok(observed) => {
                docker_container_id = Some(observed.container_id);
                docker_network = Some(observed.network);
            }
            Err(error) => limitations.push(format!("Docker runtime detail unavailable: {error}")),
        }
    }
    let kubernetes = if profile.driver == RuntimeAdapter::Kubernetes {
        match &profile.observer {
            Some(observer) => Some(observe_kubernetes(observer, record)?),
            None => {
                limitations.push(
                    "No bounded Kubernetes observer is configured; Pod and RuntimeClass binding are not independently observed."
                        .into(),
                );
                None
            }
        }
    } else {
        None
    };
    if profile.driver == RuntimeAdapter::Vm {
        limitations.push(
            "The gateway driver identity is observed. Guest hypervisor process details need a host runtime receipt."
                .into(),
        );
    }
    let driver_passed = gateway.observed_driver.as_deref() == Some(profile.driver.as_str());
    let result = if !driver_passed
        || kubernetes
            .as_ref()
            .is_some_and(|proof| !proof.api_binding_passed)
    {
        "failed"
    } else if profile.driver == RuntimeAdapter::Kubernetes && kubernetes.is_none() {
        "partial"
    } else {
        "passed"
    };
    let proof = RuntimeProof {
        schema_version: 1,
        sandbox: record.name.clone(),
        gateway_profile: record.runtime.gateway_profile.clone(),
        gateway: record.runtime.openshell_gateway.clone(),
        expected_driver: record.runtime.driver,
        observed_driver: gateway.observed_driver,
        advertised_driver: gateway.advertised_driver,
        driver_version: gateway.driver_version,
        gateway_version: gateway.gateway_version,
        workspace_transport: record.runtime.workspace_transport,
        sandbox_phase,
        docker_container_id,
        docker_network,
        kubernetes,
        result: result.into(),
        observed_at_unix: unix_now()?,
        limitations,
    };
    save_proof(config, &proof)?;
    Ok(proof)
}

pub fn refresh_all(config: &RuntimeConfig) -> BTreeMap<String, String> {
    let mut failures = BTreeMap::new();
    match state::list(config) {
        Ok(records) => {
            for record in records {
                if let Err(error) = observe(config, &record) {
                    failures.insert(record.name, error.to_string());
                }
            }
        }
        Err(error) => {
            failures.insert("controller".into(), error.to_string());
        }
    }
    failures
}

pub fn load_proofs(config: &RuntimeConfig) -> Result<BTreeMap<String, RuntimeProof>> {
    let mut proofs = BTreeMap::new();
    for entry in fs::read_dir(config.runtime_proofs_dir())? {
        let path = entry?.path();
        if path.extension().and_then(|value| value.to_str()) != Some("json") {
            continue;
        }
        let proof: RuntimeProof = serde_json::from_slice(&fs::read(&path)?)
            .with_context(|| format!("invalid runtime proof {}", path.display()))?;
        proofs.insert(proof.sandbox.clone(), proof);
    }
    Ok(proofs)
}

fn save_proof(config: &RuntimeConfig, proof: &RuntimeProof) -> Result<()> {
    state::validate_name(&proof.sandbox)?;
    let mut body = serde_json::to_vec_pretty(proof)?;
    body.push(b'\n');
    write_private(
        &config
            .runtime_proofs_dir()
            .join(format!("{}.json", proof.sandbox)),
        &body,
    )
}

pub fn kubernetes_peer_identity(
    config: &RuntimeConfig,
    record: &SandboxRecord,
) -> Result<(KubernetesObserver, KubernetesPeerIdentity)> {
    let profile = profile_for_record(config, record)?;
    if profile.driver != RuntimeAdapter::Kubernetes {
        bail!(
            "sandbox {} is not using the Kubernetes adapter",
            record.name
        );
    }
    let observer = profile
        .observer
        .context("Kubernetes peer links need a configured Kubernetes observer")?;
    if !observer.manage_peer_network_policies {
        bail!(
            "Kubernetes peer links need manage_peer_network_policies = true for their gateway profile"
        );
    }
    let (_, pod) = find_kubernetes_sandbox_and_pod(&observer, record)?;
    let pod_name = pod
        .pointer("/metadata/name")
        .and_then(Value::as_str)
        .context("Kubernetes peer Pod has no name")?
        .to_string();
    let pod_uid = pod
        .pointer("/metadata/uid")
        .and_then(Value::as_str)
        .context("Kubernetes peer Pod has no UID")?
        .to_string();
    let pod_ip = pod
        .pointer("/status/podIP")
        .and_then(Value::as_str)
        .context("Kubernetes peer Pod has no IP")?
        .parse()
        .context("Kubernetes peer Pod has an invalid IP")?;
    let sandbox_label_hash = pod
        .pointer("/metadata/labels/agents.x-k8s.io~1sandbox-name-hash")
        .and_then(Value::as_str)
        .context("Kubernetes peer Pod has no sandbox identity label")?
        .to_string();
    Ok((
        observer.clone(),
        KubernetesPeerIdentity {
            namespace: observer.namespace.clone(),
            pod_name,
            pod_uid,
            pod_ip,
            sandbox_label_hash,
        },
    ))
}

pub fn create_kubernetes_peer_policies(
    observer: &KubernetesObserver,
    lease_id: &str,
    source: &KubernetesPeerIdentity,
    gateway_ip: IpAddr,
    port: u16,
) -> Result<Vec<String>> {
    validate_peer_policy_inputs(observer, lease_id, source, gateway_ip, port)?;
    let (names, objects) = build_kubernetes_peer_policy_objects(
        &observer.namespace,
        lease_id,
        source,
        gateway_ip,
        port,
    );
    let expected_objects = objects.clone();
    let document = json!({
        "apiVersion": "v1",
        "kind": "List",
        "items": objects
    });
    kubectl_input(
        observer,
        &["create", "-f", "-"],
        &serde_json::to_vec(&document)?,
    )?;
    if let Err(error) = verify_kubernetes_peer_policies(observer, lease_id, &expected_objects) {
        let _ = remove_kubernetes_peer_policies(observer, lease_id, &names);
        return Err(error).context("created Kubernetes peer policies failed verification");
    }
    Ok(names)
}

fn build_kubernetes_peer_policy_objects(
    namespace: &str,
    lease_id: &str,
    source: &KubernetesPeerIdentity,
    gateway_ip: IpAddr,
    port: u16,
) -> (Vec<String>, Vec<Value>) {
    let stem = format!("agent-sandbox-peer-{}", &lease_id[..8]);
    let egress_name = format!("{stem}-out");
    let labels = json!({
        "app.kubernetes.io/managed-by": PEER_POLICY_MANAGER,
        "enrichedlab-agent-sandbox/lease-id": lease_id
    });
    let cidr = match gateway_ip {
        IpAddr::V4(address) => format!("{address}/32"),
        IpAddr::V6(address) => format!("{address}/128"),
    };
    let objects = vec![json!({
        "apiVersion": "networking.k8s.io/v1",
        "kind": "NetworkPolicy",
        "metadata": {
            "name": egress_name,
            "namespace": namespace,
            "labels": labels
        },
        "spec": {
            "podSelector": {"matchLabels": {"agents.x-k8s.io/sandbox-name-hash": source.sandbox_label_hash}},
            "policyTypes": ["Egress"],
            "egress": [{
                "to": [{"ipBlock": {"cidr": cidr}}],
                "ports": [{"protocol": "TCP", "port": port}]
            }]
        }
    })];
    let names = vec![egress_name];
    (names, objects)
}

pub fn remove_kubernetes_peer_policies(
    observer: &KubernetesObserver,
    lease_id: &str,
    names: &[String],
) -> Result<()> {
    if !observer.manage_peer_network_policies {
        bail!("Kubernetes peer policy mutation is disabled");
    }
    if Uuid::parse_str(lease_id).is_err() || names.len() > 2 {
        bail!("invalid Kubernetes peer policy custody record");
    }
    for name in names {
        validate_dns_label(name, "Kubernetes peer NetworkPolicy")?;
        let Some(value) = kubectl_json_optional(
            observer,
            &[
                "get",
                "networkpolicy",
                "-n",
                &observer.namespace,
                name,
                "-o",
                "json",
                "--ignore-not-found",
            ],
        )?
        else {
            continue;
        };
        if !policy_has_lease_custody(&value, lease_id) {
            bail!("refusing to remove Kubernetes NetworkPolicy without exact lease custody");
        }
        kubectl_no_input(
            observer,
            &[
                "delete",
                "networkpolicy",
                "-n",
                &observer.namespace,
                name,
                "--wait=true",
                "--timeout=30s",
            ],
        )?;
    }
    Ok(())
}

fn verify_kubernetes_peer_policies(
    observer: &KubernetesObserver,
    lease_id: &str,
    expected_objects: &[Value],
) -> Result<()> {
    for expected in expected_objects {
        let name = expected
            .pointer("/metadata/name")
            .and_then(Value::as_str)
            .context("expected Kubernetes peer policy has no name")?;
        let value = kubectl_json(
            observer,
            &[
                "get",
                "networkpolicy",
                "-n",
                &observer.namespace,
                name,
                "-o",
                "json",
            ],
        )?;
        if !policy_has_lease_custody(&value, lease_id) {
            bail!("Kubernetes peer NetworkPolicy custody labels do not match");
        }
        if value.get("spec") != expected.get("spec") {
            bail!("Kubernetes peer NetworkPolicy spec does not match the lease");
        }
    }
    Ok(())
}

fn policy_has_lease_custody(value: &Value, lease_id: &str) -> bool {
    let manager = value
        .pointer("/metadata/labels/app.kubernetes.io~1managed-by")
        .and_then(Value::as_str);
    match manager {
        Some(PEER_POLICY_MANAGER) => {
            value
                .pointer("/metadata/labels/enrichedlab-agent-sandbox~1lease-id")
                .and_then(Value::as_str)
                == Some(lease_id)
        }
        Some(LEGACY_PEER_POLICY_MANAGER) => {
            value
                .pointer("/metadata/labels/openshell-agent-sandbox~1lease-id")
                .and_then(Value::as_str)
                == Some(lease_id)
        }
        _ => false,
    }
}

fn validate_peer_policy_inputs(
    observer: &KubernetesObserver,
    lease_id: &str,
    source: &KubernetesPeerIdentity,
    gateway_ip: IpAddr,
    port: u16,
) -> Result<()> {
    if !observer.manage_peer_network_policies {
        bail!("Kubernetes peer policy mutation is disabled");
    }
    if Uuid::parse_str(lease_id).is_err() || port == 0 {
        bail!("invalid Kubernetes peer policy lease or port");
    }
    if source.namespace != observer.namespace {
        bail!("Kubernetes peer source is outside the managed namespace");
    }
    if observer.peer_gateway_ip != Some(gateway_ip) {
        bail!("Kubernetes peer gateway does not match the configured observer gateway");
    }
    for value in [&source.sandbox_label_hash] {
        if value.is_empty()
            || value.len() > 63
            || !value
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
        {
            bail!("Kubernetes peer Pod identity label is invalid");
        }
    }
    Ok(())
}

fn find_kubernetes_sandbox_and_pod(
    observer: &KubernetesObserver,
    record: &SandboxRecord,
) -> Result<(Value, Value)> {
    let selector = format!("openshell.ai/sandbox-name={}", record.name);
    let sandboxes = kubectl_json(
        observer,
        &[
            "get",
            "sandboxes.agents.x-k8s.io",
            "-n",
            &observer.namespace,
            "-l",
            &selector,
            "-o",
            "json",
        ],
    )?;
    let items = sandboxes
        .get("items")
        .and_then(Value::as_array)
        .context("Kubernetes observer returned no Sandbox item array")?;
    let matching = items
        .iter()
        .filter(|item| {
            metadata_value(item, "openshell.ai/sandbox-workspace")
                .is_none_or(|workspace| workspace == record.openshell_workspace)
        })
        .collect::<Vec<_>>();
    if matching.len() != 1 {
        bail!(
            "expected one Kubernetes Sandbox for {}, found {}",
            record.name,
            matching.len()
        );
    }
    let sandbox = matching[0].clone();
    let sandbox_uid = sandbox
        .pointer("/metadata/uid")
        .and_then(Value::as_str)
        .context("Kubernetes Sandbox has no UID")?;
    let pods = kubectl_json(
        observer,
        &[
            "get",
            "pods",
            "-n",
            &observer.namespace,
            "-l",
            "agents.x-k8s.io/sandbox-name-hash",
            "-o",
            "json",
        ],
    )?;
    let pods = pods
        .get("items")
        .and_then(Value::as_array)
        .context("Kubernetes observer returned no Pod item array")?;
    let matching = pods
        .iter()
        .filter(|pod| pod_owned_by_sandbox(pod, sandbox_uid))
        .collect::<Vec<_>>();
    if matching.len() != 1 {
        bail!(
            "expected one Kubernetes Pod for {}, found {}",
            record.name,
            matching.len()
        );
    }
    Ok((sandbox, matching[0].clone()))
}

fn observe_kubernetes(
    observer: &KubernetesObserver,
    record: &SandboxRecord,
) -> Result<KubernetesRuntimeProof> {
    let selector = format!("openshell.ai/sandbox-name={}", record.name);
    let sandboxes = kubectl_json(
        observer,
        &[
            "get",
            "sandboxes.agents.x-k8s.io",
            "-n",
            &observer.namespace,
            "-l",
            &selector,
            "-o",
            "json",
        ],
    )?;
    let items = sandboxes
        .get("items")
        .and_then(Value::as_array)
        .context("Kubernetes observer returned no Sandbox item array")?;
    let matching = items
        .iter()
        .filter(|item| {
            metadata_value(item, "openshell.ai/sandbox-workspace")
                .is_none_or(|workspace| workspace == record.openshell_workspace)
        })
        .collect::<Vec<_>>();
    if matching.len() != 1 {
        bail!(
            "expected one Kubernetes Sandbox for {}, found {}",
            record.name,
            matching.len()
        );
    }
    let sandbox = matching[0];
    let sandbox_id = metadata_value(sandbox, "openshell.ai/sandbox-id")
        .context("Kubernetes Sandbox has no OpenShell sandbox ID")?;
    let sandbox_uid = sandbox
        .pointer("/metadata/uid")
        .and_then(Value::as_str)
        .context("Kubernetes Sandbox has no UID")?;
    let pods = kubectl_json(
        observer,
        &[
            "get",
            "pods",
            "-n",
            &observer.namespace,
            "-l",
            "agents.x-k8s.io/sandbox-name-hash",
            "-o",
            "json",
        ],
    )?;
    let pods = pods
        .get("items")
        .and_then(Value::as_array)
        .context("Kubernetes observer returned no Pod item array")?;
    let matching_pods = pods
        .iter()
        .filter(|pod| pod_owned_by_sandbox(pod, sandbox_uid))
        .collect::<Vec<_>>();
    if matching_pods.len() != 1 {
        bail!(
            "expected one Kubernetes Pod for {}, found {}",
            record.name,
            matching_pods.len()
        );
    }
    let pod = matching_pods[0];
    let observed_runtime_class = pod
        .pointer("/spec/runtimeClassName")
        .and_then(Value::as_str)
        .map(str::to_string);
    let runtime_class = if let Some(name) = &observed_runtime_class {
        kubectl_json(observer, &["get", "runtimeclass", name, "-o", "json"])?
    } else {
        json!({})
    };
    let runtime_handler = runtime_class
        .get("handler")
        .and_then(Value::as_str)
        .map(str::to_string);
    let policies = kubectl_json(
        observer,
        &[
            "get",
            "networkpolicy",
            "-n",
            &observer.namespace,
            "-o",
            "json",
        ],
    )?;
    let network_policies = policies
        .get("items")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .map(|policy| {
            json!({
                "name": policy.pointer("/metadata/name"),
                "pod_selector": policy.pointer("/spec/podSelector"),
                "policy_types": policy.pointer("/spec/policyTypes"),
                "ingress_rule_count": policy.pointer("/spec/ingress").and_then(Value::as_array).map_or(0, Vec::len),
                "egress_rule_count": policy.pointer("/spec/egress").and_then(Value::as_array).map_or(0, Vec::len)
            })
        })
        .collect::<Vec<_>>();
    let requested_runtime_class = record.runtime.runtime_class_name.clone();
    let api_binding_passed = requested_runtime_class == observed_runtime_class
        && observed_runtime_class.is_some()
        && runtime_handler.is_some();
    Ok(KubernetesRuntimeProof {
        namespace: observer.namespace.clone(),
        sandbox_resource: json!({
            "api_version": sandbox.get("apiVersion"),
            "name": sandbox.pointer("/metadata/name"),
            "uid": sandbox.pointer("/metadata/uid"),
            "openshell_sandbox_id": sandbox_id,
            "resource_version": sandbox.pointer("/metadata/resourceVersion"),
            "phase": sandbox.pointer("/status/phase"),
            "conditions": sandbox.pointer("/status/conditions")
        }),
        pod: json!({
            "name": pod.pointer("/metadata/name"),
            "uid": pod.pointer("/metadata/uid"),
            "node": pod.pointer("/spec/nodeName"),
            "runtime_class_name": pod.pointer("/spec/runtimeClassName"),
            "service_account_name": pod.pointer("/spec/serviceAccountName"),
            "host_network": pod.pointer("/spec/hostNetwork"),
            "host_pid": pod.pointer("/spec/hostPID"),
            "host_ipc": pod.pointer("/spec/hostIPC"),
            "security_context": pod.pointer("/spec/securityContext"),
            "containers": pod.pointer("/spec/containers").and_then(Value::as_array).map(|items| items.iter().map(|item| json!({
                "name": item.get("name"),
                "image": item.get("image"),
                "security_context": item.get("securityContext"),
                "resources": item.get("resources")
            })).collect::<Vec<_>>()),
            "container_statuses": pod.pointer("/status/containerStatuses").and_then(Value::as_array).map(|items| items.iter().map(|item| json!({
                "name": item.get("name"),
                "ready": item.get("ready"),
                "restart_count": item.get("restartCount"),
                "image_id": item.get("imageID")
            })).collect::<Vec<_>>()),
            "persistent_volume_claims": pod.pointer("/spec/volumes").and_then(Value::as_array).map(|items| items.iter().filter_map(|item| item.pointer("/persistentVolumeClaim/claimName").and_then(Value::as_str)).collect::<Vec<_>>())
        }),
        runtime_class: json!({
            "name": runtime_class.pointer("/metadata/name"),
            "uid": runtime_class.pointer("/metadata/uid"),
            "handler": runtime_class.get("handler"),
            "overhead": runtime_class.get("overhead"),
            "scheduling": runtime_class.get("scheduling")
        }),
        network_policies,
        requested_runtime_class,
        observed_runtime_class,
        runtime_handler,
        api_binding_passed,
    })
}

fn pod_owned_by_sandbox(pod: &Value, sandbox_uid: &str) -> bool {
    pod.pointer("/metadata/ownerReferences")
        .and_then(Value::as_array)
        .is_some_and(|owners| {
            owners.iter().any(|owner| {
                owner.get("uid").and_then(Value::as_str) == Some(sandbox_uid)
                    && owner.get("kind").and_then(Value::as_str) == Some("Sandbox")
                    && owner.get("controller").and_then(Value::as_bool) == Some(true)
            })
        })
}

fn kubectl_json(observer: &KubernetesObserver, query: &[&str]) -> Result<Value> {
    let output = kubectl_output(observer, query, None)?;
    serde_json::from_slice(&output).context("Kubernetes observer returned invalid JSON")
}

fn kubectl_json_optional(observer: &KubernetesObserver, query: &[&str]) -> Result<Option<Value>> {
    let output = kubectl_output(observer, query, None)?;
    if output.iter().all(u8::is_ascii_whitespace) {
        return Ok(None);
    }
    Ok(Some(
        serde_json::from_slice(&output).context("Kubernetes observer returned invalid JSON")?,
    ))
}

fn kubectl_no_input(observer: &KubernetesObserver, query: &[&str]) -> Result<()> {
    kubectl_output(observer, query, None).map(|_| ())
}

fn kubectl_input(observer: &KubernetesObserver, query: &[&str], input: &[u8]) -> Result<()> {
    if input.is_empty() || input.len() > 256 * 1024 {
        bail!("Kubernetes mutation input must be between 1 byte and 256 KiB");
    }
    kubectl_output(observer, query, Some(input)).map(|_| ())
}

fn kubectl_output(
    observer: &KubernetesObserver,
    query: &[&str],
    input: Option<&[u8]>,
) -> Result<Vec<u8>> {
    let mut kubectl = observer.command.clone();
    if let Some(kubeconfig) = &observer.kubeconfig {
        kubectl.push("--kubeconfig".into());
        kubectl.push(kubeconfig.to_string_lossy().into_owned());
    }
    if let Some(context) = &observer.context {
        kubectl.push("--context".into());
        kubectl.push(context.clone());
    }
    kubectl.extend(query.iter().map(|value| (*value).to_string()));
    let mut command = match observer.transport {
        ObserverTransport::Local => {
            let mut command = Command::new(&kubectl[0]);
            command.args(&kubectl[1..]);
            command
        }
        ObserverTransport::Ssh => {
            let host = observer
                .ssh_host
                .as_deref()
                .context("SSH Kubernetes observer needs ssh_host")?;
            let remote = kubectl
                .iter()
                .map(|value| shell_quote(value))
                .collect::<Vec<_>>()
                .join(" ");
            let mut command = Command::new(&observer.ssh_bin);
            command.args([
                OsString::from("-o"),
                OsString::from("BatchMode=yes"),
                OsString::from("-o"),
                OsString::from("ConnectTimeout=5"),
                OsString::from("--"),
                OsString::from(host),
                OsString::from(remote),
            ]);
            command
        }
    };
    command
        .stdin(if input.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = command
        .spawn()
        .context("failed to run bounded Kubernetes command")?;
    if let Some(body) = input {
        child
            .stdin
            .take()
            .context("Kubernetes command stdin pipe was not available")?
            .write_all(body)?;
    }
    let output = child.wait_with_output()?;
    if !output.status.success() {
        let error = String::from_utf8_lossy(&output.stderr)
            .chars()
            .take(512)
            .collect::<String>();
        bail!(
            "bounded Kubernetes command failed with {}: {error}",
            output.status
        );
    }
    if output.stdout.len() > MAX_OBSERVER_OUTPUT {
        bail!("bounded Kubernetes observer output exceeded 4 MiB");
    }
    Ok(output.stdout)
}

fn metadata_value<'a>(value: &'a Value, key: &str) -> Option<&'a str> {
    value
        .pointer("/metadata/labels")
        .and_then(|labels| labels.get(key))
        .or_else(|| {
            value
                .pointer("/metadata/annotations")
                .and_then(|annotations| annotations.get(key))
        })
        .and_then(Value::as_str)
}

fn validate_kubernetes_profile(name: &str, profile: &KubernetesProfile) -> Result<()> {
    if let Some(runtime_class) = &profile.runtime_class_name {
        validate_dns_label(runtime_class, "runtime class")?;
    }
    if let Some(priority) = &profile.priority_class_name {
        validate_dns_label(priority, "priority class")?;
    }
    for (key, value) in &profile.node_selector {
        if key.is_empty()
            || key.len() > 253
            || !key.bytes().all(|byte| {
                byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'-' | b'_' | b'/')
            })
        {
            bail!("gateway profile {name} has an invalid node selector key");
        }
        if value.is_empty()
            || value.len() > 63
            || !value
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'-' | b'_'))
        {
            bail!("gateway profile {name} has an invalid node selector value");
        }
    }
    if profile.tolerations.len() > 32 || profile.tolerations.iter().any(|value| !value.is_object())
    {
        bail!("gateway profile {name} tolerations must be at most 32 JSON objects");
    }
    Ok(())
}

fn validate_identifier(value: &str, field: &str, max: usize) -> Result<()> {
    if value.is_empty()
        || value.len() > max
        || value.starts_with('-')
        || value.ends_with('-')
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
    {
        bail!("{field} is invalid: {value}");
    }
    Ok(())
}

fn validate_dns_label(value: &str, field: &str) -> Result<()> {
    if value.is_empty()
        || value.len() > 63
        || value.starts_with('-')
        || value.ends_with('-')
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
    {
        bail!("{field} must be a lowercase DNS label");
    }
    Ok(())
}

fn validate_command_token(value: &str, field: &str) -> Result<()> {
    if value.is_empty()
        || value.len() > 1024
        || value.contains(['\0', '\n', '\r'])
        || value.chars().any(char::is_control)
    {
        bail!("{field} contains an invalid token");
    }
    Ok(())
}

fn shell_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}

fn unix_now() -> Result<u64> {
    Ok(SystemTime::now().duration_since(UNIX_EPOCH)?.as_secs())
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::{
        GatewayProfile, KubernetesObserver, KubernetesPeerIdentity, ObserverTransport,
        RuntimeAdapter, WorkspaceTransport, build_kubernetes_peer_policy_objects, driver_config,
        pod_owned_by_sandbox,
    };
    use crate::workspace::MountRecord;

    #[test]
    fn docker_bind_config_contains_only_declared_mounts() {
        let profile = GatewayProfile::default();
        let mounts = vec![MountRecord {
            host_path: "/tmp/input".into(),
            container_path: "/sandbox/mounts/input".into(),
            read_only: true,
        }];
        let value: serde_json::Value = serde_json::from_str(
            &driver_config(&profile, Path::new("/tmp/work"), &mounts).unwrap(),
        )
        .unwrap();
        assert_eq!(value["docker"]["mounts"].as_array().unwrap().len(), 2);
    }

    #[test]
    fn kubernetes_config_requests_kata_and_sync() {
        let profile = GatewayProfile {
            openshell_gateway: "home-kata".into(),
            driver: RuntimeAdapter::Kubernetes,
            workspace_transport: WorkspaceTransport::Sync,
            bind_mounts_enabled: false,
            kubernetes: super::KubernetesProfile {
                runtime_class_name: Some("kata-clh".into()),
                node_selector: [("runtime.example/qualified".into(), "true".into())]
                    .into_iter()
                    .collect(),
                ..Default::default()
            },
            ..GatewayProfile::default()
        };
        profile.validate("home-kata").unwrap();
        let value: serde_json::Value =
            serde_json::from_str(&driver_config(&profile, Path::new("/unused"), &[]).unwrap())
                .unwrap();
        assert_eq!(value["kubernetes"]["pod"]["runtime_class_name"], "kata-clh");
        assert!(value["kubernetes"].get("mounts").is_none());
    }

    #[test]
    fn vm_and_kubernetes_reject_bind_transport() {
        for driver in [RuntimeAdapter::Vm, RuntimeAdapter::Kubernetes] {
            let profile = GatewayProfile {
                driver,
                ..GatewayProfile::default()
            };
            assert!(profile.validate("invalid").is_err());
        }
    }

    #[test]
    fn ssh_observer_needs_a_host() {
        let profile = GatewayProfile {
            driver: RuntimeAdapter::Kubernetes,
            workspace_transport: WorkspaceTransport::Sync,
            bind_mounts_enabled: false,
            observer: Some(KubernetesObserver {
                transport: ObserverTransport::Ssh,
                ..Default::default()
            }),
            ..GatewayProfile::default()
        };
        assert!(profile.validate("home-kata").is_err());
    }

    #[test]
    fn kubernetes_pod_proof_uses_the_sandbox_controller_owner() {
        let pod = serde_json::json!({
            "metadata": {
                "labels": {"agents.x-k8s.io/sandbox-name-hash": "b8e1bd88"},
                "ownerReferences": [{
                    "apiVersion": "agents.x-k8s.io/v1beta1",
                    "kind": "Sandbox",
                    "name": "default--worker",
                    "uid": "sandbox-uid",
                    "controller": true
                }]
            }
        });
        assert!(pod_owned_by_sandbox(&pod, "sandbox-uid"));
        assert!(!pod_owned_by_sandbox(&pod, "another-uid"));

        let not_controller = serde_json::json!({
            "metadata": {
                "ownerReferences": [{
                    "kind": "Sandbox",
                    "uid": "sandbox-uid",
                    "controller": false
                }]
            }
        });
        assert!(!pod_owned_by_sandbox(&not_controller, "sandbox-uid"));
    }

    #[test]
    fn kubernetes_peer_policy_selects_source_and_cluster_gateway_only() {
        let source = KubernetesPeerIdentity {
            namespace: "agent-sandboxes".into(),
            pod_name: "source-pod".into(),
            pod_uid: "source-uid".into(),
            pod_ip: "172.30.255.10".parse().unwrap(),
            sandbox_label_hash: "sourcehash".into(),
        };
        let (names, policies) = build_kubernetes_peer_policy_objects(
            "agent-sandboxes",
            "9e16dc68-7e68-46bb-8843-c8ad1a68532d",
            &source,
            "172.30.255.1".parse().unwrap(),
            43123,
        );
        assert_eq!(names, ["agent-sandbox-peer-9e16dc68-out"]);
        assert_eq!(
            policies[0].pointer("/spec/podSelector/matchLabels/agents.x-k8s.io~1sandbox-name-hash"),
            Some(&serde_json::json!("sourcehash"))
        );
        assert_eq!(
            policies[0].pointer("/spec/egress/0/to/0/ipBlock/cidr"),
            Some(&serde_json::json!("172.30.255.1/32"))
        );
        assert_eq!(
            policies[0].pointer("/spec/egress/0/ports/0/port"),
            Some(&serde_json::json!(43123))
        );
    }

    #[test]
    fn kubernetes_peer_gateway_requires_local_private_address() {
        let mut observer = KubernetesObserver {
            manage_peer_network_policies: true,
            peer_gateway_ip: Some("172.30.255.1".parse().unwrap()),
            ..KubernetesObserver::default()
        };
        assert!(observer.validate("cluster-kata").is_ok());
        observer.peer_gateway_ip = Some("203.0.113.10".parse().unwrap());
        assert!(observer.validate("cluster-kata").is_err());
        observer.peer_gateway_ip = Some("172.30.255.1".parse().unwrap());
        observer.transport = ObserverTransport::Ssh;
        observer.ssh_host = Some("cluster-control".into());
        assert!(observer.validate("cluster-kata").is_err());
    }
}
