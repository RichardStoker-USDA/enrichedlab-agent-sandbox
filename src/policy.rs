use std::{
    net::IpAddr,
    path::{Path, PathBuf},
};

use anyhow::{Result, bail};
use serde_yaml_ng::Value;
use sha2::{Digest, Sha256};

use crate::{
    cli::LandlockMode,
    config::{RuntimeConfig, write_private},
    openshell,
    state::{self, SandboxRecord},
};

const DEVELOPMENT: &str = include_str!("../policies/development.yaml");
const DATA_SCIENCE: &str = include_str!("../policies/data-science.yaml");
const OFFLINE: &str = include_str!("../policies/offline.yaml");
const MARKER: &str = "  # __EXTRA_POLICIES__";
const LANDLOCK_MARKER: &str = "__LANDLOCK_MODE__";

pub fn validate_public_host(value: &str) -> Result<String> {
    let host = value.trim().to_ascii_lowercase();
    if host != value || host.len() > 253 || host.is_empty() {
        bail!("host must be a normalized lowercase DNS name: {value}");
    }
    if host.parse::<IpAddr>().is_ok()
        || host == "localhost"
        || host.ends_with(".local")
        || host.ends_with(".internal")
        || host.contains('/')
        || host.contains(':')
        || host.contains('*')
    {
        bail!("only exact public DNS hosts are accepted: {value}");
    }
    let labels: Vec<&str> = host.split('.').collect();
    if labels.len() < 2
        || labels.iter().any(|label| {
            label.is_empty()
                || label.len() > 63
                || label.starts_with('-')
                || label.ends_with('-')
                || !label
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
        })
    {
        bail!("invalid public DNS host: {value}");
    }
    Ok(host)
}

pub fn render(
    profile: &str,
    landlock: LandlockMode,
    read_hosts: &[String],
    write_hosts: &[String],
) -> Result<String> {
    let template = match profile {
        "development" => DEVELOPMENT,
        "data-science" => DATA_SCIENCE,
        "offline" => OFFLINE,
        _ => bail!("unknown policy profile {profile}"),
    };
    let mut read = normalized_hosts(read_hosts)?;
    let mut write = normalized_hosts(write_hosts)?;
    read.retain(|host| !write.contains(host));
    read.sort();
    write.sort();

    let mut extra = String::new();
    append_policy(&mut extra, "extra_web_read", &read, "read-only");
    append_policy(&mut extra, "extra_web_write", &write, "full");
    let compatibility = match landlock {
        LandlockMode::HardRequirement => "hard_requirement",
        LandlockMode::BestEffort => "best_effort",
    };
    Ok(template
        .replace(LANDLOCK_MARKER, compatibility)
        .replace(MARKER, extra.trim_end()))
}

pub fn write(
    path: &Path,
    profile: &str,
    landlock: LandlockMode,
    read_hosts: &[String],
    write_hosts: &[String],
) -> Result<PathBuf> {
    let body = render(profile, landlock, read_hosts, write_hosts)?;
    write_private(path, body.as_bytes())?;
    Ok(path.to_path_buf())
}

pub fn sync_active(config: &RuntimeConfig, record: &SandboxRecord) -> Result<()> {
    let body = openshell::active_policy(&config.openshell_bin, record)?;
    let parsed: Value = serde_yaml_ng::from_str(&body)?;
    let mapping = parsed
        .as_mapping()
        .ok_or_else(|| anyhow::anyhow!("active OpenShell policy is not a YAML mapping"))?;
    if !mapping.contains_key(Value::String("version".into()))
        || !mapping.contains_key(Value::String("filesystem_policy".into()))
        || !mapping.contains_key(Value::String("network_policies".into()))
    {
        bail!("active OpenShell policy is missing required sections");
    }
    write_private(&record.policy_path, body.as_bytes())?;
    state::save(config, record)
}

pub fn host_rule_name(host: &str) -> String {
    let digest = hex::encode(Sha256::digest(host));
    format!("agent-sandbox-extra-{}", &digest[..12])
}

fn normalized_hosts(hosts: &[String]) -> Result<Vec<String>> {
    let mut result = Vec::new();
    for host in hosts {
        let host = validate_public_host(host)?;
        if !result.contains(&host) {
            result.push(host);
        }
    }
    Ok(result)
}

fn append_policy(output: &mut String, name: &str, hosts: &[String], access: &str) {
    if hosts.is_empty() {
        return;
    }
    output.push_str(&format!("  {name}:\n    name: {name}\n    endpoints:\n"));
    for host in hosts {
        output.push_str(&format!(
            "      - {{ host: {host}, port: 443, protocol: rest, enforcement: enforce, access: {access} }}\n"
        ));
    }
    output.push_str(
        "    binaries:\n      - { path: /usr/bin/curl }\n      - { path: /usr/bin/wget }\n      - { path: /usr/bin/git }\n      - { path: /usr/bin/node }\n      - { path: /usr/bin/npm }\n      - { path: /usr/bin/python3 }\n      - { path: /sandbox/.local/opencode/node_modules/.bin/opencode }\n      - { path: /sandbox/.local/pi/node_modules/.bin/pi }\n      - { path: /sandbox/.local/bin/** }\n",
    );
}

#[cfg(test)]
mod tests {
    use crate::cli::LandlockMode;

    use super::{host_rule_name, render, validate_public_host};

    #[test]
    fn public_hosts_are_strict() {
        assert_eq!(
            validate_public_host("api.example.com").unwrap(),
            "api.example.com"
        );
        for invalid in [
            "localhost",
            "10.0.0.1",
            "*.example.com",
            "EXAMPLE.com",
            "a/b.com",
        ] {
            assert!(validate_public_host(invalid).is_err(), "accepted {invalid}");
        }
    }

    #[test]
    fn write_access_wins_and_landlock_is_hard() {
        let policy = render(
            "offline",
            LandlockMode::HardRequirement,
            &["docs.example.com".to_string()],
            &["docs.example.com".to_string()],
        )
        .unwrap();
        assert!(!policy.contains("extra_web_read:"));
        assert!(policy.contains("extra_web_write:"));
        assert!(policy.contains("compatibility: hard_requirement"));
        assert!(!policy.contains("/app"));
    }

    #[test]
    fn every_profile_lets_runtimes_read_limits_and_share_memory() {
        // DuckDB, the JVM, Go and Node size themselves from the cgroup files, and Python multiprocessing needs
        // POSIX semaphores in /dev/shm; without them DuckDB aborts and process pools fall back or fail. The controller
        // grants both only in a sandbox's own cgroup and IPC namespaces (runtime::require_private_namespaces).
        for profile in ["development", "data-science", "offline"] {
            let policy = render(profile, LandlockMode::BestEffort, &[], &[]).unwrap();
            let parsed: serde_yaml_ng::Value = serde_yaml_ng::from_str(&policy).unwrap();
            let entries = |key: &str| -> Vec<String> {
                parsed["filesystem_policy"][key]
                    .as_sequence()
                    .unwrap()
                    .iter()
                    .map(|entry| entry.as_str().unwrap().to_owned())
                    .collect()
            };
            let (read_only, read_write) = (entries("read_only"), entries("read_write"));
            assert_eq!(
                read_only
                    .iter()
                    .filter(|e| e.as_str() == "/sys/fs/cgroup")
                    .count(),
                1,
                "{profile}"
            );
            assert_eq!(
                read_write
                    .iter()
                    .filter(|e| e.as_str() == "/dev/shm")
                    .count(),
                1,
                "{profile}"
            );
            for entry in read_only.iter().chain(&read_write) {
                assert!(
                    !entry.starts_with("/sys/") || entry == "/sys/fs/cgroup",
                    "{profile}: {entry}"
                );
                assert!(
                    !entry.starts_with("/dev/")
                        || ["/dev/urandom", "/dev/null", "/dev/shm"].contains(&entry.as_str()),
                    "{profile}: {entry}"
                );
            }
            for entry in &read_write {
                assert!(
                    entry != "/sys" && !entry.starts_with("/sys/"),
                    "{profile}: {entry}"
                );
            }
        }
    }

    #[test]
    fn host_rule_names_are_stable_and_bounded() {
        let name = host_rule_name("api.example.com");
        assert_eq!(name, host_rule_name("api.example.com"));
        assert!(name.len() < 63);
    }
}
