//! Legacy migration and the doctor's checks.
use super::*;

pub(super) fn migrate_legacy(config: &RuntimeConfig, output: OutputFormat) -> Result<()> {
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .context("HOME is not set")?;
    let legacy = std::env::var_os("AGENT_SANDBOX_LEGACY_STATE_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| home.join(".local/state/agent-sandbox"));
    if !legacy.is_absolute() {
        bail!("legacy state directory must be absolute");
    }
    if !legacy.exists() {
        return print_value(
            output,
            &json!({"migrated": 0, "legacy": legacy}),
            "No legacy state found",
        );
    }
    if !state::list(config)?.is_empty() {
        bail!("new controller state is not empty; refusing an ambiguous legacy migration");
    }
    let legacy_records = legacy.join("records");
    let mut migrated = 0_u64;
    if legacy_records.exists() {
        for entry in fs::read_dir(legacy_records)? {
            let path = entry?.path();
            if path.extension().and_then(|value| value.to_str()) != Some("json") {
                continue;
            }
            let mut record: SandboxRecord = serde_json::from_slice(&fs::read(&path)?)?;
            record.schema_version = 2;
            let (requests, decisions, runtime) = state::control_paths(config, &record.name)?;
            create_private_dir(&requests)?;
            create_private_dir(&decisions)?;
            let control_mounts =
                workspace::materialize_control_mounts(&requests, &decisions, &runtime)?;
            record.mounts.retain(|mount| {
                ![
                    workspace::REQUESTS_TARGET,
                    workspace::DECISIONS_TARGET,
                    workspace::RUNTIME_TARGET,
                ]
                .contains(&mount.container_path.as_str())
            });
            record.mounts.extend(control_mounts);
            record.control_requests = requests;
            record.control_decisions = decisions;
            record.policy_path = state::policy_path(config, &record.name)?;
            let legacy_policy = legacy
                .join("policies")
                .join(format!("{}.yaml", record.name));
            if legacy_policy.exists() {
                fs::copy(&legacy_policy, &record.policy_path)?;
            }
            state::save(config, &record)?;
            migrated += 1;
        }
    }
    audit::append(
        config,
        "host-controller",
        "state.migrate-legacy",
        "controller/state",
        "passed",
        json!({"records": migrated, "legacy_source_kept": true}),
    )?;
    print_value(
        output,
        &json!({"migrated": migrated, "legacy": legacy, "source_kept": true}),
        "Legacy records copied; source state kept",
    )
}

pub(super) fn doctor(config: &RuntimeConfig, output: OutputFormat) -> Result<()> {
    let openshell_version = openshell::probe(&config.openshell_bin, &["--version"])?;
    let settings = config.load_settings()?;
    let routes = RouteCatalog::load(config)?;
    let needs_local_docker = settings
        .gateway_profiles
        .values()
        .any(|profile| profile.enabled && profile.driver == platform::RuntimeAdapter::Docker);
    let docker_context = if needs_local_docker {
        let result = ProcessCommand::new(&config.docker_bin)
            .args(["context", "show"])
            .output()
            .context("failed to inspect Docker context")?;
        if !result.status.success() {
            None
        } else {
            Some(String::from_utf8_lossy(&result.stdout).trim().to_string())
        }
    } else {
        None
    };
    let local_bind_mounts = bind_mounts_enabled();
    let mut gateway_checks = Vec::new();
    let mut required_gateways_passed = true;
    let mut required_bind_mounts_passed = true;
    for (name, profile) in &settings.gateway_profiles {
        if !profile.enabled {
            gateway_checks
                .push(json!({"name": name, "enabled": false, "required": profile.required}));
            continue;
        }
        let health = platform::observe_gateway(config, name, profile);
        let passed = health.as_ref().is_ok_and(|value| value.passed);
        if profile.required && !passed {
            required_gateways_passed = false;
        }
        let bind_observed = if profile.workspace_transport == platform::WorkspaceTransport::Bind
            && profile.openshell_gateway == "openshell"
        {
            Some(local_bind_mounts)
        } else {
            None
        };
        if profile.required && bind_observed == Some(false) {
            required_bind_mounts_passed = false;
        }
        gateway_checks.push(match health {
            Ok(value) => json!({
                "name": name,
                "enabled": true,
                "required": profile.required,
                "workspace_transport": profile.workspace_transport,
                "bind_mounts_observed": bind_observed,
                "health": value
            }),
            Err(error) => json!({
                "name": name,
                "enabled": true,
                "required": profile.required,
                "workspace_transport": profile.workspace_transport,
                "bind_mounts_observed": bind_observed,
                "error": error.to_string(),
                "passed": false
            }),
        });
    }
    let mut route_checks = Vec::new();
    let mut required_routes_passed = true;
    for (gateway_name, gateway) in &settings.gateway_profiles {
        if !gateway.enabled {
            continue;
        }
        for (name, route) in &routes.routes {
            if route.model.is_none() {
                continue;
            }
            let configured = openshell::probe(
                &config.openshell_bin,
                &[
                    "--gateway",
                    &gateway.openshell_gateway,
                    "--workspace",
                    &route.workspace,
                    "inference",
                    "get",
                ],
            )
            .is_ok();
            if gateway.required && !configured {
                required_routes_passed = false;
            }
            route_checks.push(json!({
                "name": name,
                "gateway_profile": gateway_name,
                "gateway": gateway.openshell_gateway,
                "workspace": route.workspace,
                "configured": configured,
                "required": gateway.required
            }));
        }
    }
    let audit_records = audit::verify(config)?;
    let allocation_reservations = allocation::summaries(config)?;
    let allocation_custody_passed = allocation_reservations.iter().all(|reservation| {
        !matches!(
            reservation.status.as_str(),
            "unowned-live" | "custody-unknown"
        )
    });
    let report = json!({
        "openshell_version": openshell_version.trim(),
        "docker_context": docker_context,
        "local_bind_mounts_enabled": local_bind_mounts,
        "root": config.root,
        "state_dir": config.state_dir,
        "config_dir": config.config_dir,
        "dashboard_loopback": settings.dashboard_bind.parse::<std::net::SocketAddr>().is_ok_and(|bind| bind.ip().is_loopback()),
        "policy_advisor_enabled": settings.policy_advisor_enabled,
        "ocsf_json_enabled": settings.ocsf_json_enabled,
        "hard_landlock_default": settings.require_hard_landlock,
        "audit_chain_valid": true,
        "audit_records": audit_records.len(),
        "gateways": gateway_checks,
        "routes": route_checks,
        "platform": std::env::consts::OS,
        "default_gateway_profile": settings.default_gateway_profile,
        "runtime_proofs": platform::load_proofs(config)?,
        "allocation_reservations": allocation_reservations,
        "allocation_custody_passed": allocation_custody_passed,
    });
    let healthy = required_gateways_passed
        && required_bind_mounts_passed
        && required_routes_passed
        && (!needs_local_docker || docker_context.is_some())
        && report["dashboard_loopback"] == true
        && report["policy_advisor_enabled"] == true
        && report["ocsf_json_enabled"] == true
        && report["audit_chain_valid"] == true
        && report["allocation_custody_passed"] == true;
    print_value(
        output,
        &report,
        if healthy {
            "Controller checks passed"
        } else {
            "Controller checks failed"
        },
    )?;
    if !healthy {
        bail!("agent-sandbox doctor found a failed check");
    }
    Ok(())
}

pub(super) fn bind_mounts_enabled() -> bool {
    let mut paths = vec![
        PathBuf::from("/opt/homebrew/var/openshell/gateway.toml"),
        PathBuf::from("/etc/openshell/gateway.toml"),
    ];
    if let Some(home) = std::env::var_os("HOME") {
        paths.push(PathBuf::from(home).join(".config/openshell/gateway.toml"));
    }
    paths
        .into_iter()
        .filter_map(|path| fs::read_to_string(path).ok())
        .any(|body| body.contains("enable_bind_mounts = true"))
}
