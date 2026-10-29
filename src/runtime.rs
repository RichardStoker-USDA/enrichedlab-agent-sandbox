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

use crate::{openshell, state::SandboxRecord};

#[derive(Clone, Debug)]
pub struct DockerSandbox {
    pub container_id: String,
    pub ip: Ipv4Addr,
    pub gateway: Ipv4Addr,
    pub network: String,
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
    let networks = run_capture(
        docker_bin,
        &[
            "inspect",
            "--format",
            "{{json .NetworkSettings.Networks}}",
            ids[0],
        ],
    )?;
    let attachments: BTreeMap<String, DockerNetworkAttachment> =
        serde_json::from_str(networks.trim())?;
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
    })
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

    use super::validate_bind_ip;

    #[test]
    fn wildcard_bind_is_rejected() {
        assert!(validate_bind_ip(IpAddr::V4(Ipv4Addr::UNSPECIFIED)).is_err());
    }
}
