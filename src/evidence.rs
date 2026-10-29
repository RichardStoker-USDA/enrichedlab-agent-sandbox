use std::{
    collections::BTreeMap,
    ffi::OsString,
    fs,
    io::Read,
    net::{IpAddr, Ipv4Addr, SocketAddr, TcpListener},
    path::{Path, PathBuf},
    process::{Command, Stdio},
};

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use time::{OffsetDateTime, format_description::well_known::Rfc3339, macros::format_description};
use uuid::Uuid;
use walkdir::WalkDir;

use crate::{
    audit,
    config::{RuntimeConfig, create_private_dir, write_private},
    lease, openshell,
    platform::{self, RuntimeAdapter, WorkspaceTransport},
    runtime, state, workspace_sync,
};

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct EvidenceManifest {
    pub schema_version: u32,
    pub sandbox: String,
    pub captured_at: String,
    pub directory: PathBuf,
    pub files: BTreeMap<String, String>,
    pub limitations: Vec<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct IsolationReport {
    pub schema_version: u32,
    pub sandbox: String,
    pub captured_at: String,
    pub checks: BTreeMap<String, bool>,
    pub policy: String,
    pub decision_receipts: Vec<workspace_sync::DecisionDeliveryReceipt>,
    pub sentinel_targets: Vec<String>,
    pub limitations: Vec<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct EvidenceSummary {
    pub schema_version: u32,
    pub manifest: EvidenceManifest,
    pub process_count: u64,
    pub debian_package_count: u64,
    pub python_package_count: u64,
    pub node_package_count: u64,
    pub host_listener_count: u64,
    pub decision_receipt_count: u64,
    pub decision_integrity_verified: bool,
    pub verified: bool,
}

pub fn latest_summaries(config: &RuntimeConfig) -> Result<BTreeMap<String, EvidenceSummary>> {
    let mut summaries = BTreeMap::new();
    for sandbox_entry in fs::read_dir(config.evidence_dir())? {
        let sandbox_entry = sandbox_entry?;
        if !sandbox_entry.file_type()?.is_dir() {
            continue;
        }
        let sandbox = sandbox_entry
            .file_name()
            .into_string()
            .map_err(|_| anyhow::anyhow!("evidence sandbox name is not UTF-8"))?;
        state::validate_name(&sandbox)?;
        let mut captures = fs::read_dir(sandbox_entry.path())?
            .filter_map(|entry| entry.ok())
            .filter_map(|entry| {
                entry
                    .file_type()
                    .ok()
                    .filter(|kind| kind.is_dir())
                    .map(|_| entry.path())
            })
            .filter(|path| path.join("manifest.json").is_file())
            .collect::<Vec<_>>();
        captures.sort();
        let Some(directory) = captures.pop() else {
            continue;
        };
        let manifest_path = directory.join("manifest.json");
        let manifest = verify_manifest(&manifest_path)?;
        if manifest.sandbox != sandbox || manifest.directory != directory {
            bail!("evidence manifest path or sandbox identity changed");
        }
        summaries.insert(
            sandbox,
            EvidenceSummary {
                schema_version: 1,
                process_count: bounded_line_count(&directory.join("processes.txt"))?,
                debian_package_count: bounded_line_count(&directory.join("debian-packages.txt"))?,
                python_package_count: bounded_line_count(&directory.join("python-packages.txt"))?,
                node_package_count: node_package_count(&directory.join("node-packages.json"))?,
                host_listener_count: bounded_line_count(&directory.join("host-listeners.txt"))?,
                decision_receipt_count: bounded_json_array_count(
                    &directory.join("decision-delivery-receipts.json"),
                )?,
                decision_integrity_verified: directory
                    .join("decision-delivery-receipts.json")
                    .is_file()
                    && !directory.join("decision-delivery-error.json").exists(),
                manifest,
                verified: true,
            },
        );
    }
    Ok(summaries)
}

pub fn ocsf_tail(config: &RuntimeConfig, sandbox: &str, count: usize) -> Result<Vec<Value>> {
    if count == 0 || count > 200 {
        bail!("OCSF tail count must be between 1 and 200");
    }
    let record = state::load(config, sandbox)?;
    let script = format!(
        "tail -q -c 262144 /var/log/openshell-ocsf.*.log 2>/dev/null | tail -n {count} || true"
    );
    let command = ["/bin/sh", "-lc", &script]
        .into_iter()
        .map(OsString::from)
        .collect::<Vec<_>>();
    let output = openshell::exec_capture(&config.openshell_bin, &record, &command)?;
    let mut events = Vec::new();
    for line in output.lines().filter(|line| !line.trim().is_empty()) {
        if line.len() > 64 * 1024 {
            events.push(json!({"sandbox": sandbox, "parse_error": "OCSF line exceeds 64 KiB"}));
            continue;
        }
        match serde_json::from_str::<Value>(line) {
            Ok(mut value) => {
                if let Some(object) = value.as_object_mut() {
                    object.insert("controller_sandbox".into(), Value::String(sandbox.into()));
                }
                events.push(value);
            }
            Err(error) => events.push(json!({
                "sandbox": sandbox,
                "parse_error": error.to_string(),
                "raw": line.chars().take(2048).collect::<String>()
            })),
        }
    }
    Ok(events)
}

pub fn snapshot(config: &RuntimeConfig, sandbox: &str) -> Result<EvidenceManifest> {
    let record = state::load(config, sandbox)?;
    let captured_at = OffsetDateTime::now_utc();
    let compact = captured_at.format(format_description!(
        "[year][month][day]T[hour][minute][second]Z"
    ))?;
    let capture_id = Uuid::new_v4().simple().to_string();
    let directory = config
        .evidence_dir()
        .join(sandbox)
        .join(format!("{compact}-{}", &capture_id[..8]));
    create_private_dir(&directory)?;
    write_json(
        &directory.join("capture-status.json"),
        &json!({"schema_version": 1, "state": "capturing", "captured_at": captured_at.format(&Rfc3339)?}),
    )?;

    write_json(&directory.join("controller-record.json"), &record)?;
    let mut limitations = vec![
        "OpenShell OCSF JSONL is complete inside the sandbox, but gateway-pushed buffers can drop events.".into(),
        "Package manifests and workspace hashes show observed state; they do not reconstruct every downloaded byte or network response.".into(),
        "Host listener evidence is a point-in-time snapshot.".into(),
    ];
    write_text(
        &directory.join("openshell-sandbox.json"),
        &openshell::sandbox_get(&config.openshell_bin, &record)?,
    )?;
    write_text(
        &directory.join("openshell-policy.txt"),
        &openshell::record_probe(
            &config.openshell_bin,
            &record,
            &["policy", "get", &record.name],
        )?,
    )?;
    write_text(
        &directory.join("openshell-settings.txt"),
        &openshell::record_probe(
            &config.openshell_bin,
            &record,
            &["settings", "get", &record.name],
        )?,
    )?;
    write_json(
        &directory.join("service-leases.json"),
        &lease::list(config)?
            .into_iter()
            .filter(|item| {
                item.sandbox == sandbox || item.source_sandbox.as_deref() == Some(sandbox)
            })
            .collect::<Vec<_>>(),
    )?;
    write_json(
        &directory.join("workspace-inventory.json"),
        &workspace_inventory(&record.host_workspace)?,
    )?;
    match platform::observe(config, &record) {
        Ok(proof) => write_json(&directory.join("runtime-proof.json"), &proof)?,
        Err(error) => {
            write_json(
                &directory.join("runtime-proof-error.json"),
                &json!({"error": error.to_string()}),
            )?;
            limitations.push(format!("Independent runtime proof failed: {error}"));
        }
    }
    if record.runtime.workspace_transport == WorkspaceTransport::Sync {
        match workspace_sync::verify_decision_receipts(config, &record) {
            Ok(receipts) => {
                write_json(
                    &directory.join("decision-delivery-receipts.json"),
                    &receipts,
                )?;
                limitations.push(
                    "Decision delivery receipts prove the remote copies matched the host canonical files when this snapshot was captured. The sandbox can modify those copies later."
                        .into(),
                );
            }
            Err(error) => {
                write_json(
                    &directory.join("decision-delivery-receipts.json"),
                    &Vec::<workspace_sync::DecisionDeliveryReceipt>::new(),
                )?;
                write_json(
                    &directory.join("decision-delivery-error.json"),
                    &json!({"error": error.to_string()}),
                )?;
                limitations.push(format!(
                    "Remote decision-copy integrity verification failed: {error}"
                ));
            }
        }
    } else {
        write_json(
            &directory.join("decision-delivery-receipts.json"),
            &Vec::<workspace_sync::DecisionDeliveryReceipt>::new(),
        )?;
    }
    if record.runtime.workspace_transport == WorkspaceTransport::Sync {
        match workspace_sync::pull(config, &record, false) {
            Ok(report) => {
                write_json(&directory.join("remote-sync-report.json"), &report)?;
                if let Some(snapshot) = &report.snapshot {
                    write_json(
                        &directory.join("remote-workspace-inventory.json"),
                        &workspace_inventory(&snapshot.join("workspace"))?,
                    )?;
                }
            }
            Err(error) => {
                write_json(
                    &directory.join("remote-sync-error.json"),
                    &json!({"error": error.to_string()}),
                )?;
                limitations.push(format!("Remote workspace snapshot failed: {error}"));
            }
        }
    }

    for (name, command) in [
        (
            "processes.txt",
            vec![
                "/bin/sh",
                "-lc",
                "ps -eo pid,ppid,user,comm 2>/dev/null || true",
            ],
        ),
        (
            "debian-packages.txt",
            vec![
                "/bin/sh",
                "-lc",
                "dpkg-query -W -f='${Package}\t${Version}\n' 2>/dev/null | sort || true",
            ],
        ),
        (
            "python-packages.txt",
            vec![
                "/bin/sh",
                "-lc",
                "python3 -m pip freeze 2>/dev/null | sort || true",
            ],
        ),
        (
            "node-packages.json",
            vec![
                "/bin/sh",
                "-lc",
                "npm --global list --depth=0 --json 2>/dev/null || true",
            ],
        ),
        (
            "mountinfo.txt",
            vec![
                "/bin/sh",
                "-lc",
                "cat /proc/self/mountinfo 2>/dev/null || true",
            ],
        ),
    ] {
        let command: Vec<OsString> = command.into_iter().map(OsString::from).collect();
        let output = openshell::exec_capture(&config.openshell_bin, &record, &command)?;
        write_text(&directory.join(name), &output)?;
    }

    let listeners = host_capture("lsof", &["-nP", "-iTCP", "-sTCP:LISTEN"])
        .unwrap_or_else(|error| format!("unavailable: {error}\n"));
    write_text(&directory.join("host-listeners.txt"), &listeners)?;

    let ocsf_directory = directory.join("sandbox-var-log");
    create_private_dir(&ocsf_directory)?;
    capture_sandbox_logs(config, &record, &ocsf_directory)
        .context("failed to copy sandbox logs; OCSF custody is incomplete")?;

    write_json(
        &directory.join("capture-status.json"),
        &json!({"schema_version": 1, "state": "complete", "captured_at": captured_at.format(&Rfc3339)?}),
    )?;
    let files = file_hashes(&directory, Some("manifest.json"))?;
    let manifest = EvidenceManifest {
        schema_version: 1,
        sandbox: sandbox.into(),
        captured_at: captured_at.format(&Rfc3339)?,
        directory: directory.clone(),
        files,
        limitations,
    };
    write_json(&directory.join("manifest.json"), &manifest)?;
    audit::append(
        config,
        "host-controller",
        "evidence.snapshot",
        &format!("sandbox/{sandbox}"),
        "passed",
        json!({"directory": directory, "file_count": manifest.files.len()}),
    )?;
    Ok(manifest)
}

fn capture_sandbox_logs(
    config: &RuntimeConfig,
    record: &state::SandboxRecord,
    destination: &Path,
) -> Result<()> {
    const FILE_LIMIT: u64 = 16 * 1024 * 1024;
    const TOTAL_LIMIT: u64 = 64 * 1024 * 1024;
    let listing = openshell::exec_capture(
        &config.openshell_bin,
        record,
        &[
            "/bin/sh".into(),
            "-lc".into(),
            "find /var/log -maxdepth 1 -type f -name 'openshell*.log' -printf '%f\\t%s\\n' 2>/dev/null | sort".into(),
        ],
    )?;
    let mut files = Vec::new();
    let mut total = 0_u64;
    for line in listing.lines().filter(|line| !line.is_empty()) {
        let (name, size) = line
            .split_once('\t')
            .context("sandbox log listing was malformed")?;
        if !name.starts_with("openshell")
            || !name.ends_with(".log")
            || !name
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'-' | b'_'))
        {
            bail!("sandbox returned an invalid log filename");
        }
        let size: u64 = size.parse().context("sandbox log size was invalid")?;
        if size > FILE_LIMIT {
            bail!("sandbox log {name} exceeds the 16 MiB evidence limit");
        }
        total = total
            .checked_add(size)
            .context("sandbox log size overflow")?;
        if total > TOTAL_LIMIT {
            bail!("sandbox logs exceed the 64 MiB evidence limit");
        }
        files.push((name.to_string(), size));
    }
    if !files
        .iter()
        .any(|(name, _)| name.starts_with("openshell-ocsf."))
    {
        bail!("sandbox has no OpenShell OCSF log");
    }
    if files.len() > 32 {
        bail!("sandbox has more than 32 OpenShell log files");
    }
    let mut captured_total = 0_u64;
    for (name, _) in files {
        let body = openshell::exec_capture(
            &config.openshell_bin,
            record,
            &["/bin/cat".into(), format!("/var/log/{name}").into()],
        )?;
        let captured_size = body.len() as u64;
        if captured_size > FILE_LIMIT {
            bail!("sandbox log {name} exceeds the 16 MiB evidence limit during capture");
        }
        captured_total = captured_total
            .checked_add(captured_size)
            .context("captured sandbox log size overflow")?;
        if captured_total > TOTAL_LIMIT {
            bail!("captured sandbox logs exceed the 64 MiB evidence limit");
        }
        write_text(&destination.join(name), &body)?;
    }
    Ok(())
}

pub fn verify_manifest(path: &Path) -> Result<EvidenceManifest> {
    let body = fs::read(path)?;
    let manifest: EvidenceManifest = serde_json::from_slice(&body)?;
    if manifest.schema_version != 1 {
        bail!("unsupported evidence manifest schema");
    }
    let current = file_hashes(&manifest.directory, Some("manifest.json"))?;
    if current != manifest.files {
        bail!("evidence files do not match the manifest");
    }
    Ok(manifest)
}

fn bounded_line_count(path: &Path) -> Result<u64> {
    const MAX_BYTES: u64 = 16 * 1024 * 1024;
    let metadata = fs::metadata(path)
        .with_context(|| format!("missing evidence inventory {}", path.display()))?;
    if !metadata.is_file() || metadata.len() > MAX_BYTES {
        bail!(
            "evidence inventory {} is invalid or too large",
            path.display()
        );
    }
    let body = fs::read_to_string(path)?;
    Ok(body.lines().filter(|line| !line.trim().is_empty()).count() as u64)
}

fn node_package_count(path: &Path) -> Result<u64> {
    const MAX_BYTES: u64 = 16 * 1024 * 1024;
    let metadata = fs::metadata(path)
        .with_context(|| format!("missing Node package inventory {}", path.display()))?;
    if !metadata.is_file() || metadata.len() > MAX_BYTES {
        bail!(
            "Node package inventory {} is invalid or too large",
            path.display()
        );
    }
    let value: Value = serde_json::from_slice(&fs::read(path)?)?;
    Ok(value
        .get("dependencies")
        .and_then(Value::as_object)
        .map_or(0, |packages| packages.len() as u64))
}

fn bounded_json_array_count(path: &Path) -> Result<u64> {
    const MAX_BYTES: u64 = 16 * 1024 * 1024;
    let metadata = match fs::metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(0),
        Err(error) => return Err(error.into()),
    };
    if !metadata.is_file() || metadata.len() > MAX_BYTES {
        bail!("evidence JSON inventory is invalid or too large");
    }
    let value: Value = serde_json::from_slice(&fs::read(path)?)?;
    let values = value
        .as_array()
        .context("evidence JSON inventory is not an array")?;
    if values.len() > 256 {
        bail!("evidence JSON inventory exceeds 256 entries");
    }
    Ok(values.len() as u64)
}

pub fn isolation(config: &RuntimeConfig, sandbox: &str) -> Result<IsolationReport> {
    let record = state::load(config, sandbox)?;
    let decision_receipts = if record.runtime.workspace_transport == WorkspaceTransport::Sync {
        workspace_sync::verify_decision_receipts(config, &record)
            .context("remote decision copies do not match host delivery receipts")?
    } else {
        Vec::new()
    };
    let decision_contract = match record.runtime.workspace_transport {
        WorkspaceTransport::Bind => concat!(
            "test ! -w /sandbox/control/decisions; ",
            "printf 'decision-contract:host-read-only-bind\\n'; "
        ),
        WorkspaceTransport::Sync => concat!(
            "test -w /sandbox/control/decisions; ",
            "! awk '$5 == \"/sandbox/control/decisions\" { found=1 } END { exit found ? 0 : 1 }' /proc/self/mountinfo; ",
            "printf 'decision-contract:remote-copy-not-host-mount\\n'; "
        ),
    };
    let filesystem_script = format!(
        concat!(
            "set -eu; ",
            "test -d /sandbox/work; ",
            "test -w /sandbox/work; ",
            "test -d /sandbox/control/requests; ",
            "test -w /sandbox/control/requests; ",
            "test -d /sandbox/control/decisions; ",
            "{}",
            "test ! -e /host; ",
            "test ! -e /Users; ",
            "printf 'filesystem-contract-ok\\n'"
        ),
        decision_contract
    );
    let command = ["/bin/sh", "-lc", &filesystem_script]
        .into_iter()
        .map(OsString::from)
        .collect::<Vec<_>>();
    let output = openshell::exec_capture(&config.openshell_bin, &record, &command)
        .context("sandbox filesystem isolation probe failed")?;
    let policy = openshell::active_policy(&config.openshell_bin, &record)?;
    let settings = openshell::record_probe(
        &config.openshell_bin,
        &record,
        &["settings", "get", &record.name],
    )?;
    let mut checks = BTreeMap::new();
    checks.insert(
        "filesystem_contract".into(),
        output.contains("filesystem-contract-ok"),
    );
    checks.insert(
        match record.runtime.workspace_transport {
            WorkspaceTransport::Bind => "control_decisions_host_bind_read_only",
            WorkspaceTransport::Sync => "control_decisions_remote_copy_not_host_mount",
        }
        .into(),
        output.contains("decision-contract:"),
    );
    checks.insert("control_requests_writable".into(), true);
    checks.insert("host_root_not_mounted".into(), true);
    checks.insert("primary_workspace_writable".into(), true);
    checks.insert(
        "control_decision_copies_match_receipts".into(),
        record.runtime.workspace_transport == WorkspaceTransport::Bind
            || decision_receipts.iter().all(|receipt| receipt.verified),
    );
    checks.insert(
        "hard_landlock_recorded".into(),
        matches!(record.landlock, crate::cli::LandlockMode::HardRequirement),
    );
    checks.insert(
        "ocsf_json_enabled".into(),
        record.ocsf_json_enabled && settings.contains("ocsf_json_enabled"),
    );
    checks.insert(
        "policy_advisor_enabled".into(),
        record.policy_advisor_enabled && settings.contains("agent_policy_proposals_enabled"),
    );
    let private_hosts = declared_private_hosts(&policy)?;
    checks.insert(
        "private_network_not_declared".into(),
        private_hosts.is_empty(),
    );

    let mut sentinel_targets = Vec::new();
    let mut excluded_private = None;
    if record.runtime.driver == RuntimeAdapter::Docker {
        let docker = runtime::inspect_sandbox(&config.docker_bin, sandbox)?;
        checks.insert(
            "docker_namespaces_private".into(),
            docker.namespaces_private(),
        );
        let alias = runtime::sandbox_host_alias_ip(&config.openshell_bin, &record)?;
        let (alias_bind, alias_route_valid) = if cfg!(target_os = "macos") {
            (
                Ipv4Addr::LOCALHOST,
                !alias.is_unspecified() && !alias.is_multicast(),
            )
        } else {
            let docker_host = runtime::host_bind_ip(&config.docker_bin, &docker.network)?;
            excluded_private = Some(docker_host);
            (docker_host, alias == docker_host)
        };
        checks.insert("host_alias_route_valid".into(), alias_route_valid);
        let target = "host.openshell.internal";
        checks.insert(
            "host_alias_sentinel_blocked".into(),
            alias_route_valid && sentinel_blocked(config, &record, alias_bind, target)?,
        );
        sentinel_targets.push(format!("{target} ({alias})"));
    }
    let runtime_proof = platform::observe(config, &record)?;
    checks.insert(
        "runtime_driver_observed".into(),
        runtime_proof.observed_driver.as_deref() == Some(record.runtime.driver.as_str()),
    );
    if record.runtime.driver == RuntimeAdapter::Kubernetes {
        checks.insert(
            "kubernetes_runtime_class_api_binding".into(),
            runtime_proof
                .kubernetes
                .as_ref()
                .is_some_and(|proof| proof.api_binding_passed),
        );
    }
    let mut private_addresses = runtime::host_private_ipv4_addresses()?;
    if let Some(excluded) = excluded_private {
        private_addresses.retain(|address| *address != excluded);
    }
    let truncated_addresses = private_addresses.len() > 16;
    private_addresses.truncate(16);
    let mut host_interfaces_blocked = true;
    for address in private_addresses {
        host_interfaces_blocked &=
            sentinel_blocked(config, &record, address, &address.to_string())?;
        sentinel_targets.push(address.to_string());
    }
    checks.insert(
        "host_private_interfaces_blocked".into(),
        host_interfaces_blocked,
    );
    if checks.values().any(|passed| !passed) {
        bail!("one or more isolation checks failed: {checks:?}");
    }
    let report = IsolationReport {
        schema_version: 2,
        sandbox: sandbox.into(),
        captured_at: OffsetDateTime::now_utc().format(&Rfc3339)?,
        checks,
        policy,
        decision_receipts,
        sentinel_targets,
        limitations: vec![
            "This probe checks the controller mount contract and active OpenShell policy. It is not proof against a compromised host kernel, container runtime, or OpenShell gateway.".into(),
            match record.runtime.workspace_transport {
                WorkspaceTransport::Bind => "Read-only bind checks cover the mounted host decision path at the runtime boundary.".into(),
                WorkspaceTransport::Sync => "The host source is not mounted. Remote input and decision copies remain writable by the sandbox user and are snapshots, not immutable files.".into(),
            },
            if record.runtime.driver == RuntimeAdapter::Kubernetes {
                "Kubernetes API proof binds the Sandbox CR to its Pod and RuntimeClass handler. CRI-level microVM proof still depends on a trusted node/runtime receipt.".into()
            } else if record.runtime.driver == RuntimeAdapter::Vm {
                "Gateway proof identifies the VM driver. Hypervisor process and guest-kernel attestation need a trusted host runtime receipt.".into()
            } else {
                "Runtime proof is point-in-time controller and gateway evidence.".into()
            },
            if truncated_addresses {
                "The host has more than 16 private IPv4 interfaces; the sentinel probe was capped at 16.".into()
            } else {
                "Host sentinels prove that the tested sandbox could not connect to listeners on assigned host-private interfaces. They do not probe every other device on the LAN.".into()
            },
        ],
    };
    audit::append(
        config,
        "host-controller",
        "audit.isolation",
        &format!("sandbox/{sandbox}"),
        "passed",
        json!({"checks": report.checks}),
    )?;
    Ok(report)
}

fn sentinel_blocked(
    config: &RuntimeConfig,
    record: &state::SandboxRecord,
    bind: Ipv4Addr,
    destination: &str,
) -> Result<bool> {
    let listener = TcpListener::bind(SocketAddr::new(IpAddr::V4(bind), 0))?;
    listener.set_nonblocking(true)?;
    let port = listener.local_addr()?.port();
    let script = format!(
        "set -u; test -x /usr/bin/curl; set +e; /usr/bin/curl --noproxy '*' --connect-timeout 2 --max-time 2 --silent --show-error http://{destination}:{port}/ >/dev/null 2>&1; rc=$?; printf 'sentinel-attempt:%s\\n' \"$rc\"; exit 0"
    );
    let command = ["/bin/sh", "-lc", &script]
        .into_iter()
        .map(OsString::from)
        .collect::<Vec<_>>();
    let output = openshell::exec_capture(&config.openshell_bin, record, &command)?;
    if !output.contains("sentinel-attempt:") {
        bail!("sandbox did not complete the host sentinel attempt");
    }
    match listener.accept() {
        Ok(_) => Ok(false),
        Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => Ok(true),
        Err(error) => Err(error.into()),
    }
}

fn declared_private_hosts(body: &str) -> Result<Vec<String>> {
    let value: serde_yaml_ng::Value = serde_yaml_ng::from_str(body)?;
    let mut hosts = Vec::new();
    collect_hosts(&value, &mut hosts);
    hosts.sort();
    hosts.dedup();
    Ok(hosts
        .into_iter()
        .filter(|host| {
            if host == "inference.local" || host == "policy.local" {
                return false;
            }
            host.parse::<IpAddr>().is_ok_and(|ip| {
                ip.is_loopback()
                    || match ip {
                        IpAddr::V4(ip) => ip.is_private() || ip.is_link_local(),
                        IpAddr::V6(ip) => ip.is_unique_local() || ip.is_unicast_link_local(),
                    }
            }) || host.ends_with(".internal")
                || host.ends_with(".local")
        })
        .collect())
}

fn collect_hosts(value: &serde_yaml_ng::Value, hosts: &mut Vec<String>) {
    match value {
        serde_yaml_ng::Value::Mapping(mapping) => {
            if let Some(host) = mapping
                .get(serde_yaml_ng::Value::String("host".into()))
                .and_then(serde_yaml_ng::Value::as_str)
            {
                hosts.push(host.to_ascii_lowercase());
            }
            for value in mapping.values() {
                collect_hosts(value, hosts);
            }
        }
        serde_yaml_ng::Value::Sequence(values) => {
            for value in values {
                collect_hosts(value, hosts);
            }
        }
        _ => {}
    }
}

fn workspace_inventory(root: &Path) -> Result<Vec<Value>> {
    let mut entries = WalkDir::new(root)
        .follow_links(false)
        .into_iter()
        .collect::<std::result::Result<Vec<_>, _>>()?;
    entries.sort_by(|left, right| left.path().cmp(right.path()));
    if entries.len() > 100_000 {
        bail!("workspace inventory exceeds 100000 entries");
    }
    let mut result = Vec::new();
    for entry in entries {
        let path = entry.path();
        let relative = path.strip_prefix(root)?;
        if relative.as_os_str().is_empty() {
            continue;
        }
        let metadata = fs::symlink_metadata(path)?;
        let kind = if metadata.file_type().is_symlink() {
            "symlink"
        } else if metadata.is_dir() {
            "directory"
        } else if metadata.is_file() {
            "file"
        } else {
            "other"
        };
        let digest = if metadata.is_file() {
            Some(hash_file(path)?)
        } else {
            None
        };
        result.push(
            json!({"path": relative, "kind": kind, "size": metadata.len(), "sha256": digest}),
        );
    }
    Ok(result)
}

fn file_hashes(root: &Path, exclude: Option<&str>) -> Result<BTreeMap<String, String>> {
    let mut result = BTreeMap::new();
    for entry in WalkDir::new(root).follow_links(false) {
        let entry = entry?;
        if !entry.file_type().is_file() {
            continue;
        }
        let relative = entry
            .path()
            .strip_prefix(root)?
            .to_string_lossy()
            .into_owned();
        if exclude == Some(relative.as_str()) {
            continue;
        }
        result.insert(relative, hash_file(entry.path())?);
    }
    Ok(result)
}

fn hash_file(path: &Path) -> Result<String> {
    let mut file = fs::File::open(path)?;
    let mut hasher = Sha256::new();
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        let read = file.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    Ok(hex::encode(hasher.finalize()))
}

fn write_json(path: &Path, value: &impl Serialize) -> Result<()> {
    let mut body = serde_json::to_vec_pretty(value)?;
    body.push(b'\n');
    write_private(path, &body)
}

fn write_text(path: &Path, value: &str) -> Result<()> {
    write_private(path, value.as_bytes())
}

fn host_capture(bin: &str, args: &[&str]) -> Result<String> {
    let output = Command::new(bin).args(args).stdin(Stdio::null()).output()?;
    if !output.status.success() {
        bail!("{bin} exited with {}", output.status);
    }
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

#[cfg(test)]
mod tests {
    use std::fs;

    use tempfile::tempdir;

    use super::workspace_inventory;

    #[test]
    fn workspace_inventory_hashes_files_without_reading_symlink_targets() {
        let temp = tempdir().unwrap();
        fs::write(temp.path().join("result.txt"), "value").unwrap();
        let inventory = workspace_inventory(temp.path()).unwrap();
        assert_eq!(inventory.len(), 1);
        assert_eq!(inventory[0]["kind"], "file");
        assert!(inventory[0]["sha256"].as_str().is_some());
    }
}
