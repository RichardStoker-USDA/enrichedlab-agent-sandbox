use std::{
    collections::BTreeMap,
    ffi::OsString,
    net::{IpAddr, Ipv4Addr, SocketAddr, TcpListener},
    path::Path,
    process::{Command, Stdio},
};

use anyhow::{Context, Result, bail};
use ipnet::Ipv4Net;
use serde::Deserialize;

use crate::{config::RuntimeConfig, openshell, platform::RuntimeAdapter, state::SandboxRecord};

#[derive(Clone, Debug)]
pub struct DockerSandbox {
    pub container_id: String,
    pub ip: Ipv4Addr,
    pub gateway: Ipv4Addr,
    pub network: String,
    /// The container's cgroup namespace mode as Docker reports it (`private` or `host`).
    pub cgroupns_mode: String,
    /// The container's IPC namespace mode as Docker reports it (`private`, `shareable`, `none`, `host` or
    /// `container:<id>`).
    pub ipc_mode: String,
}

impl DockerSandbox {
    /// Whether the container has its own cgroup and IPC namespaces. The sandbox policies let a workload read
    /// `/sys/fs/cgroup` and write `/dev/shm`; under a host cgroup namespace the first shows the daemon's whole hierarchy
    /// with its services and sibling containers, and under a host or borrowed IPC namespace the second is shared with
    /// them.
    pub fn namespaces_private(&self) -> bool {
        self.cgroupns_mode == "private"
            && matches!(self.ipc_mode.as_str(), "private" | "shareable" | "none")
    }
}

#[derive(Debug, Deserialize)]
struct DockerContainerInspect {
    networks: BTreeMap<String, DockerNetworkAttachment>,
    cgroupns: String,
    ipc: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "PascalCase")]
struct DockerNetworkAttachment {
    #[serde(rename = "IPAddress")]
    ip_address: String,
    gateway: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "PascalCase")]
struct DockerIpamConfig {
    subnet: Option<String>,
    gateway: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "PascalCase")]
struct DockerIpam {
    config: Option<Vec<DockerIpamConfig>>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "PascalCase")]
struct DockerNetworkInspect {
    #[serde(rename = "IPAM")]
    ipam: DockerIpam,
}

pub fn inspect_sandbox(docker_bin: &Path, sandbox: &str) -> Result<DockerSandbox> {
    let ids = run_capture(
        docker_bin,
        &[
            "ps",
            "--filter",
            &format!("label=openshell.ai/sandbox-name={sandbox}"),
            "--format",
            "{{.ID}}",
        ],
    )?;
    let ids: Vec<&str> = ids.lines().filter(|line| !line.trim().is_empty()).collect();
    if ids.len() != 1 {
        bail!(
            "expected one running Docker container for {sandbox}, found {}",
            ids.len()
        );
    }
    let inspected = run_capture(
        docker_bin,
        &[
            "inspect",
            "--format",
            r#"{"networks":{{json .NetworkSettings.Networks}},"cgroupns":{{json .HostConfig.CgroupnsMode}},"ipc":{{json .HostConfig.IpcMode}}}"#,
            ids[0],
        ],
    )?;
    let DockerContainerInspect {
        networks: attachments,
        cgroupns: cgroupns_mode,
        ipc: ipc_mode,
    } = serde_json::from_str(inspected.trim())?;
    if attachments.len() != 1 {
        bail!("sandbox {sandbox} must have exactly one Docker network");
    }
    let (network, attachment) = attachments
        .into_iter()
        .next()
        .expect("checked one attachment");
    Ok(DockerSandbox {
        container_id: ids[0].into(),
        ip: attachment
            .ip_address
            .parse()
            .context("invalid sandbox IPv4 address")?,
        gateway: attachment
            .gateway
            .parse()
            .context("invalid Docker gateway IPv4 address")?,
        network,
        cgroupns_mode,
        ipc_mode,
    })
}

/// The gate every path that creates a sandbox (create, recreation and each rollback) passes before the sandbox is
/// saved or used: a Docker sandbox whose namespaces are not private, or cannot be inspected, is deleted.
pub fn admit_created(config: &RuntimeConfig, record: &SandboxRecord) -> Result<()> {
    if record.runtime.driver != RuntimeAdapter::Docker {
        return Ok(());
    }
    let Err(error) = require_private_namespaces(&config.docker_bin, &record.name) else {
        return Ok(());
    };
    match openshell::delete_and_verify(&config.openshell_bin, record) {
        Ok(()) => Err(error).context("sandbox was deleted because its namespaces are not private"),
        Err(cleanup_error) => Err(error).context(format!(
            "sandbox namespaces are not private and verified cleanup failed: {cleanup_error:#}"
        )),
    }
}

/// Refuse a Docker sandbox whose cgroup or IPC namespace is not its own (`DockerSandbox::namespaces_private`).
pub fn require_private_namespaces(docker_bin: &Path, sandbox: &str) -> Result<()> {
    let observed = inspect_sandbox(docker_bin, sandbox)?;
    if !observed.namespaces_private() {
        bail!(
            "sandbox {sandbox} shares a namespace with its host (cgroup namespace {:?}, IPC {:?}); the policy lets it \
             read /sys/fs/cgroup and write /dev/shm only inside its own",
            observed.cgroupns_mode,
            observed.ipc_mode
        );
    }
    Ok(())
}

pub fn host_bind_ip(docker_bin: &Path, network: &str) -> Result<Ipv4Addr> {
    let body = run_capture(docker_bin, &["network", "inspect", network])?;
    let values: Vec<DockerNetworkInspect> = serde_json::from_str(&body)?;
    let config = values
        .first()
        .and_then(|value| value.ipam.config.as_ref())
        .and_then(|values| {
            values.iter().find(|value| {
                value
                    .subnet
                    .as_deref()
                    .is_some_and(|subnet| subnet.contains('.'))
            })
        })
        .context("Docker network has no IPv4 IPAM config")?;
    let subnet: Ipv4Net = config
        .subnet
        .as_deref()
        .context("Docker subnet missing")?
        .parse()?;
    let gateway: Option<Ipv4Addr> = config.gateway.as_deref().map(str::parse).transpose()?;
    let candidates = host_ipv4_addresses()?;
    let candidate = candidates
        .into_iter()
        .find(|ip| subnet.contains(ip) && Some(*ip) != gateway)
        .or(gateway)
        .context("no host interface address belongs to the Docker network")?;
    validate_bind_ip(IpAddr::V4(candidate))?;
    Ok(candidate)
}

pub fn sandbox_host_alias_ip(openshell_bin: &Path, record: &SandboxRecord) -> Result<Ipv4Addr> {
    let output = openshell::exec_capture(
        openshell_bin,
        record,
        &[
            OsString::from("/bin/sh"),
            OsString::from("-lc"),
            OsString::from("getent ahostsv4 host.openshell.internal | awk 'NR==1 {print $1}'"),
        ],
    )?;
    output
        .trim()
        .parse()
        .context("host.openshell.internal did not resolve to IPv4")
}

pub fn validate_bind_ip(ip: IpAddr) -> Result<()> {
    if ip.is_unspecified() || ip.is_multicast() {
        bail!("refusing unspecified or multicast bind address {ip}");
    }
    TcpListener::bind(SocketAddr::new(ip, 0))
        .with_context(|| format!("{ip} is not an assigned host bind address"))?;
    Ok(())
}

pub fn host_private_ipv4_addresses() -> Result<Vec<Ipv4Addr>> {
    let mut addresses = host_ipv4_addresses()?
        .into_iter()
        .filter(|ip| !ip.is_loopback() && ip.is_private())
        .collect::<Vec<_>>();
    addresses.sort();
    addresses.dedup();
    Ok(addresses)
}

fn host_ipv4_addresses() -> Result<Vec<Ipv4Addr>> {
    if cfg!(target_os = "macos") {
        let output = run_capture(Path::new("/sbin/ifconfig"), &[])?;
        let mut addresses = Vec::new();
        for line in output.lines() {
            let mut fields = line.split_whitespace();
            if fields.next() == Some("inet")
                && let Some(value) = fields.next()
                && let Ok(ip) = value.parse()
            {
                addresses.push(ip);
            }
        }
        Ok(addresses)
    } else {
        let output = run_capture(Path::new("ip"), &["-json", "address", "show"])?;
        let value: serde_json::Value = serde_json::from_str(&output)?;
        let mut addresses = Vec::new();
        for interface in value.as_array().into_iter().flatten() {
            for address in interface["addr_info"].as_array().into_iter().flatten() {
                if address["family"] == "inet"
                    && let Some(local) = address["local"].as_str()
                    && let Ok(ip) = local.parse()
                {
                    addresses.push(ip);
                }
            }
        }
        Ok(addresses)
    }
}

fn run_capture(bin: &Path, args: &[&str]) -> Result<String> {
    let output = Command::new(bin)
        .args(args)
        .stdin(Stdio::null())
        .output()
        .with_context(|| format!("failed to run {}", bin.display()))?;
    if !output.status.success() {
        bail!("{} failed with {}", bin.display(), output.status);
    }
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

#[cfg(test)]
mod tests {
    use std::net::{IpAddr, Ipv4Addr};

    use super::{DockerSandbox, validate_bind_ip};

    #[test]
    fn only_a_container_in_its_own_namespaces_is_private() {
        let sandbox = |cgroupns: &str, ipc: &str| DockerSandbox {
            container_id: "c".into(),
            ip: Ipv4Addr::new(172, 17, 0, 2),
            gateway: Ipv4Addr::new(172, 17, 0, 1),
            network: "n".into(),
            cgroupns_mode: cgroupns.into(),
            ipc_mode: ipc.into(),
        };
        for ipc in ["private", "shareable", "none"] {
            assert!(sandbox("private", ipc).namespaces_private(), "{ipc}");
        }
        for (cgroupns, ipc) in [
            ("host", "private"),
            ("", "private"),
            ("private", "host"),
            ("private", "container:abc"),
            ("private", ""),
        ] {
            assert!(
                !sandbox(cgroupns, ipc).namespaces_private(),
                "{cgroupns} {ipc}"
            );
        }
    }

    #[test]
    fn wildcard_bind_is_rejected() {
        assert!(validate_bind_ip(IpAddr::V4(Ipv4Addr::UNSPECIFIED)).is_err());
    }
}
