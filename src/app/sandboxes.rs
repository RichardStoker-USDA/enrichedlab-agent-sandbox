//! Sandbox pools and single sandboxes: create, list, run agents in them, and delete them.
use super::*;

pub(super) fn run_async(future: impl std::future::Future<Output = Result<()>>) -> Result<()> {
    tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?
        .block_on(future)
}

pub(super) fn create_pool(
    config: &RuntimeConfig,
    output: OutputFormat,
    args: PoolArgs,
) -> Result<()> {
    if args.count == 0 {
        bail!("pool count must be at least 1");
    }
    let settings = config.load_settings()?;
    let route_name = args
        .runtime
        .route
        .as_deref()
        .unwrap_or(&settings.default_route);
    let routes = RouteCatalog::load(config)?;
    let cap = routes.get(route_name)?.pool_cap;
    if args.count > cap {
        bail!(
            "route {route_name} is capped at {cap} sandboxes; requested {}",
            args.count
        );
    }
    let mut created = Vec::new();
    for _ in 0..args.count {
        match create_one(config, None, args.shared_workspace.clone(), &args.runtime) {
            Ok(record) => created.push(record),
            Err(error) => {
                let mut cleanup_failures = Vec::new();
                for record in &created {
                    match openshell::delete_and_verify(&config.openshell_bin, record) {
                        Ok(()) => {
                            let _ = state::remove(config, record);
                        }
                        Err(cleanup_error) => {
                            cleanup_failures.push(format!("{}: {cleanup_error:#}", record.name))
                        }
                    }
                }
                if !cleanup_failures.is_empty() {
                    return Err(error).context(format!(
                        "pool creation failed and cleanup was incomplete: {}",
                        cleanup_failures.join("; ")
                    ));
                }
                return Err(error)
                    .context("pool creation rolled back; host work folders were kept");
            }
        }
    }
    print_records(output, &created);
    Ok(())
}

pub(crate) fn create_one(
    config: &RuntimeConfig,
    requested_name: Option<String>,
    requested_workspace: Option<PathBuf>,
    runtime_args: &RuntimeArgs,
) -> Result<SandboxRecord> {
    let settings = config.load_settings()?;
    let routes = RouteCatalog::load(config)?;
    let route_name = runtime_args
        .route
        .as_deref()
        .unwrap_or(&settings.default_route);
    let route = routes.get(route_name)?;
    let profile = runtime_args
        .profile
        .as_deref()
        .unwrap_or(&settings.default_profile);
    let gateway_profile_name = runtime_args
        .gateway_profile
        .as_deref()
        .unwrap_or(&settings.default_gateway_profile);
    let gateway_profile = settings.gateway_profile(gateway_profile_name)?.clone();
    let name = requested_name.unwrap_or_else(random_name);
    state::validate_name(&name)?;
    if state::record_path(config, &name)?.exists() {
        bail!("managed sandbox already exists: {name}");
    }
    let _reservation = allocation::reserve(
        config,
        route_name,
        &name,
        route.pool_cap,
        &gateway_profile.openshell_gateway,
    )?;
    let primary =
        workspace::materialize_primary(&config.root, requested_workspace.as_deref(), &name)?;
    let mut mounts = workspace::materialize_extra_mounts(
        &runtime_args.read_only_mounts,
        &runtime_args.read_write_mounts,
        &primary.path,
    )?;
    let (control_requests, control_decisions, control_runtime) =
        state::control_paths(config, &name)?;
    mounts.extend(workspace::materialize_control_mounts(
        &control_requests,
        &control_decisions,
        &control_runtime,
    )?);
    let policy_path = state::policy_path(config, &name)?;
    policy::write(
        &policy_path,
        profile,
        runtime_args.landlock,
        &runtime_args.read_hosts,
        &runtime_args.write_hosts,
    )?;
    let driver = platform::driver_config(&gateway_profile, &primary.path, &mounts)?;
    if route.model.is_some() {
        openshell::probe(
            &config.openshell_bin,
            &[
                "--gateway",
                &gateway_profile.openshell_gateway,
                "--workspace",
                &route.workspace,
                "inference",
                "get",
            ],
        )
        .with_context(|| {
            format!(
                "inference route {route_name} is not configured on gateway {}",
                gateway_profile.openshell_gateway
            )
        })?;
    }
    let runtime =
        platform::RuntimeContract::from_profile(gateway_profile_name, &gateway_profile, &driver);
    if let Err(error) = openshell::create(
        &config.openshell_bin,
        &gateway_profile.openshell_gateway,
        &name,
        &route.workspace,
        route_name,
        &runtime_args.image,
        &runtime_args.cpu,
        &runtime_args.memory,
        &driver,
        &policy_path,
        settings.policy_advisor_enabled,
        settings.ocsf_json_enabled,
    ) {
        let _ = fs::remove_file(&policy_path);
        if primary.managed {
            let _ = fs::remove_dir(&primary.path);
        }
        return Err(error);
    }
    let record = SandboxRecord {
        schema_version: 3,
        name,
        route: route_name.into(),
        profile: profile.into(),
        runtime,
        openshell_workspace: route.workspace.clone(),
        host_workspace: primary.path,
        managed_workspace: primary.managed,
        mounts,
        image: runtime_args.image.clone(),
        cpu: runtime_args.cpu.clone(),
        memory: runtime_args.memory.clone(),
        read_hosts: runtime_args.read_hosts.clone(),
        write_hosts: runtime_args.write_hosts.clone(),
        policy_path,
        approval_mode: runtime_args.approval_mode,
        landlock: runtime_args.landlock,
        policy_advisor_enabled: settings.policy_advisor_enabled,
        ocsf_json_enabled: settings.ocsf_json_enabled,
        control_requests,
        control_decisions,
        created_at_unix: SystemTime::now().duration_since(UNIX_EPOCH)?.as_secs(),
    };
    if record.runtime.driver == platform::RuntimeAdapter::Docker
        && let Err(error) =
            crate::runtime::require_private_namespaces(&config.docker_bin, &record.name)
    {
        let _ = fs::remove_file(&record.policy_path);
        return match openshell::delete_and_verify(&config.openshell_bin, &record) {
            Ok(()) => {
                Err(error).context("sandbox was deleted because its namespaces are not private")
            }
            Err(cleanup_error) => Err(error).context(format!(
                "sandbox namespaces are not private and verified cleanup failed: {cleanup_error:#}"
            )),
        };
    }
    if record.runtime.workspace_transport == platform::WorkspaceTransport::Sync
        && let Err(error) = workspace_sync::initialize(config, &record)
    {
        return match openshell::delete_and_verify(&config.openshell_bin, &record) {
            Ok(()) => {
                Err(error).context("sandbox was deleted because its initial workspace sync failed")
            }
            Err(cleanup_error) => Err(error).context(format!(
                "initial workspace sync failed and verified cleanup failed: {cleanup_error:#}"
            )),
        };
    }
    if let Err(error) = state::save(config, &record) {
        return match openshell::delete_and_verify(&config.openshell_bin, &record) {
            Ok(()) => Err(error)
                .context("sandbox was deleted because its controller record could not be saved"),
            Err(cleanup_error) => Err(error).context(format!(
                "sandbox record could not be saved and verified cleanup failed: {cleanup_error:#}"
            )),
        };
    }
    audit::append(
        config,
        "host-controller",
        "sandbox.create",
        &format!("sandbox/{}", record.name),
        "passed",
        json!({"route": record.route, "profile": record.profile, "runtime": record.runtime, "approval_mode": record.approval_mode, "landlock": record.landlock, "mount_targets": record.mounts.iter().map(|mount| &mount.container_path).collect::<Vec<_>>(), "workspace": record.host_workspace}),
    )?;
    Ok(record)
}

pub(super) fn list(config: &RuntimeConfig, output: OutputFormat) -> Result<()> {
    let records = state::list(config)?;
    let mut live = BTreeMap::new();
    for record in &records {
        live.entry(record.runtime.openshell_gateway.clone())
            .or_insert_with(|| {
                openshell::live_sandboxes(&config.openshell_bin, &record.runtime.openshell_gateway)
                    .unwrap_or(Value::Array(Vec::new()))
            });
    }
    if matches!(output, OutputFormat::Json) {
        return print_json(&json!({"managed": records, "live": live}));
    }
    if records.is_empty() {
        println!("No managed sandboxes.");
        return Ok(());
    }
    for record in records {
        let status = live
            .get(&record.runtime.openshell_gateway)
            .and_then(|items| find_live_status(items, &record.name))
            .unwrap_or("missing");
        println!(
            "{}  {:12}  {:10}  {:12}  {}",
            record.name,
            record.route,
            status,
            record.runtime.driver.as_str(),
            record.host_workspace.display()
        );
    }
    Ok(())
}

pub(super) fn exec(config: &RuntimeConfig, args: ExecArgs) -> Result<()> {
    let record = state::load(config, &args.name)?;
    openshell::exec(&config.openshell_bin, &record, &args.command, false)
}

pub(super) fn bootstrap(config: &RuntimeConfig, args: BootstrapArgs) -> Result<()> {
    let record = state::load(config, &args.name)?;
    ensure_harness(config, &record, args.harness)?;
    let harness_bin = match args.harness {
        HarnessKind::OpenCode => OPENCODE_BIN,
        HarnessKind::Pi => PI_BIN,
        HarnessKind::OpenAiCompatible | HarnessKind::Native => {
            bail!("this harness is ingest-only and has no sandbox runner to install")
        }
    };
    openshell::exec(
        &config.openshell_bin,
        &record,
        &[OsString::from(harness_bin), OsString::from("--version")],
        false,
    )
}

pub(super) fn ensure_opencode(config: &RuntimeConfig, record: &SandboxRecord) -> Result<()> {
    let install = format!(
        "set -eu\ncurrent=''\nif [ -x {OPENCODE_BIN} ]; then current=$({OPENCODE_BIN} --version 2>/dev/null || true); fi\nif [ \"$current\" != \"{OPENCODE_VERSION}\" ]; then npm install --silent --no-audit --no-fund --prefix /sandbox/.local/opencode opencode-ai@{OPENCODE_VERSION}; fi"
    );
    openshell::exec_capture(
        &config.openshell_bin,
        record,
        &[
            OsString::from("/bin/sh"),
            OsString::from("-lc"),
            OsString::from(install),
        ],
    )
    .map(|_| ())
    .context("failed to install the pinned OpenCode runner")
}

pub(super) fn ensure_pi(config: &RuntimeConfig, record: &SandboxRecord) -> Result<()> {
    let install = format!(
        "set -eu\ncurrent=''\nif [ -x {PI_BIN} ]; then current=$({PI_BIN} --version 2>/dev/null || true); fi\nif [ \"$current\" != \"{PI_VERSION}\" ]; then npm install --silent --no-audit --no-fund --prefix /sandbox/.local/pi @earendil-works/pi-coding-agent@{PI_VERSION}; fi"
    );
    openshell::exec_capture(
        &config.openshell_bin,
        record,
        &[
            OsString::from("/bin/sh"),
            OsString::from("-lc"),
            OsString::from(install),
        ],
    )
    .map(|_| ())
    .context("failed to install the pinned Pi runner")
}

pub(super) fn run_agent(config: &RuntimeConfig, output: OutputFormat, args: RunArgs) -> Result<()> {
    let record = state::load(config, &args.name)?;
    ensure_harness(config, &record, args.harness)?;
    let command = agent_command(
        config,
        &record,
        args.harness,
        args.thinking,
        true,
        &args.message,
    )?;
    let observer = Arc::new(Mutex::new(DirectRunObserver::new(args.harness)));
    let callback_observer = Arc::clone(&observer);
    let captured = openshell::exec_capture_bounded_observed(
        &config.openshell_bin,
        &record,
        &command,
        MAX_DIRECT_RUN_CAPTURE_BYTES,
        move |line| {
            callback_observer
                .lock()
                .map_err(|_| anyhow::anyhow!("direct-run observer lock was poisoned"))?
                .observe_line(line)
                .map(|_| ())
        },
    )?;
    let mut observer = observer
        .lock()
        .map_err(|_| anyhow::anyhow!("direct-run observer lock was poisoned"))?;
    let lines = observer.lines;
    let _final_events = observer.finish()?;
    let mut events = observer.take_observed();
    drop(observer);

    let terminal = CanonicalHarnessEvent {
        kind: "agent.run.finished".into(),
        evidence_role: "lifecycle".into(),
        visibility: EvidenceVisibility::Metadata,
        source_event_id: None,
        parent_source_event_id: None,
        metadata: json!({
            "success": captured.success,
            "exit_code": captured.code,
            "stdout_bytes": captured.stdout.len(),
            "stderr_bytes": captured.stderr.len(),
            "stdout_truncated": captured.stdout_truncated,
            "stderr_truncated": captured.stderr_truncated,
            "observer_failed": captured.observer_error.is_some()
        }),
        restricted_content: None,
    };
    events.push(terminal);

    let routes = RouteCatalog::load(config)?;
    let route = routes.get(&record.route)?;
    let run_id = format!("direct-{}-{}", record.name, Uuid::new_v4().simple());
    let context = provenance::ProvenanceContext {
        run_id: &run_id,
        subject: &record.name,
        segment: 1,
        harness: args.harness,
        provider: Some("openshell"),
        model: route.model.as_deref(),
        engine: route.inference_engine.as_deref(),
        parser: route.reasoning_parser.as_deref(),
        session_id: None,
        actor: "host-runner",
    };
    for event in &events {
        provenance::append_harness_event(config, &context, event)?;
    }
    audit::append(
        config,
        "host-controller",
        "agent.run",
        &format!("agent-state/{run_id}/provenance"),
        if captured.success { "passed" } else { "failed" },
        json!({
            "sandbox": record.name,
            "harness": args.harness,
            "lines": lines,
            "events": events.len(),
            "stdout_truncated": captured.stdout_truncated,
            "stderr_truncated": captured.stderr_truncated,
            "observer_failed": captured.observer_error.is_some()
        }),
    )?;

    let assistant_output = events
        .iter()
        .filter(|event| event.evidence_role == "output")
        .filter_map(|event| event.restricted_content.as_deref())
        .collect::<Vec<_>>()
        .join("\n");
    let capture_valid = !captured.stdout_truncated
        && !captured.stderr_truncated
        && captured.observer_error.is_none();
    if !capture_valid {
        bail!("agent run finished, but its provenance stream was incomplete; run id {run_id}");
    }
    if !captured.success {
        bail!(
            "agent harness exited unsuccessfully with code {:?}; run id {run_id}",
            captured.code
        );
    }
    match output {
        OutputFormat::Human => {
            if assistant_output.is_empty() {
                println!("Agent run completed");
            } else {
                println!("{assistant_output}");
            }
            println!("Provenance run: {run_id}");
            Ok(())
        }
        OutputFormat::Json => print_json(&json!({
            "run_id": run_id,
            "sandbox": record.name,
            "harness": args.harness,
            "events_recorded": events.len(),
            "assistant_output": assistant_output
        })),
    }
}

pub(super) fn agent_command(
    config: &RuntimeConfig,
    record: &SandboxRecord,
    harness: HarnessKind,
    thinking: Option<crate::cli::Thinking>,
    json_output: bool,
    message: &[OsString],
) -> Result<Vec<OsString>> {
    let routes = RouteCatalog::load(config)?;
    let route = routes.get(&record.route)?;
    let model = route
        .model
        .as_ref()
        .context("sandbox has no inference route")?;
    let variant = route.resolve_variant(thinking)?;
    let mut command = match harness {
        HarnessKind::OpenCode => {
            let mut command = vec![
                OsString::from("/usr/bin/env"),
                OsString::from("OPENCODE_ENABLE_EXA=1"),
                OsString::from("NODE_NO_WARNINGS=1"),
                OsString::from(format!(
                    "OPENCODE_CONFIG_CONTENT={}",
                    serde_json::to_string(&route.opencode_config()?)?
                )),
                OsString::from(OPENCODE_BIN),
                OsString::from("run"),
                OsString::from("--pure"),
                OsString::from("--auto"),
                OsString::from("--model"),
                OsString::from(format!("openshell/{model}")),
            ];
            if let Some(variant) = variant {
                command.extend([OsString::from("--variant"), OsString::from(variant)]);
            }
            if json_output {
                command.extend([OsString::from("--format"), OsString::from("json")]);
            }
            command
        }
        HarnessKind::Pi => {
            write_pi_route(record, route)?;
            let mut command = vec![
                OsString::from("/usr/bin/env"),
                OsString::from("NODE_NO_WARNINGS=1"),
                OsString::from("PI_TELEMETRY=0"),
                OsString::from("PI_CODING_AGENT_DIR=/sandbox/work/.agent-harness/pi"),
                OsString::from(PI_BIN),
                OsString::from("--print"),
                OsString::from("--no-session"),
                OsString::from("--no-extensions"),
                OsString::from("--no-skills"),
                OsString::from("--no-prompt-templates"),
                OsString::from("--no-context-files"),
                OsString::from("--approve"),
                OsString::from("--model"),
                OsString::from(format!("openshell/{model}")),
            ];
            if let Some(variant) = variant {
                command.extend([
                    OsString::from("--thinking"),
                    OsString::from(pi_thinking_level(&variant)?),
                ]);
            }
            if json_output {
                command.extend([OsString::from("--mode"), OsString::from("json")]);
            }
            command
        }
        HarnessKind::OpenAiCompatible | HarnessKind::Native => {
            bail!(
                "the direct run command supports opencode and pi; use native ingestion for other harnesses"
            )
        }
    };
    command.extend(message.iter().cloned());
    Ok(command)
}

pub(super) fn ensure_harness(
    config: &RuntimeConfig,
    record: &SandboxRecord,
    harness: HarnessKind,
) -> Result<()> {
    match harness {
        HarnessKind::OpenCode => ensure_opencode(config, record),
        HarnessKind::Pi => ensure_pi(config, record),
        HarnessKind::OpenAiCompatible | HarnessKind::Native => {
            bail!("this harness is ingest-only and cannot be launched as a sandbox runner")
        }
    }
}

pub(super) fn write_pi_route(
    record: &SandboxRecord,
    route: &crate::routes::RouteConfig,
) -> Result<()> {
    let directory = record.host_workspace.join(".agent-harness/pi");
    create_private_dir(&directory)?;
    let mut body = serde_json::to_vec_pretty(&route.pi_models_config()?)?;
    body.push(b'\n');
    write_private(&directory.join("models.json"), &body)
}

pub(super) fn pi_thinking_level(route_variant: &str) -> Result<&'static str> {
    match route_variant {
        "off" => Ok("off"),
        "low" => Ok("low"),
        "medium" => Ok("medium"),
        "on" | "high" => Ok("high"),
        "xhigh" => Ok("xhigh"),
        other => bail!("route thinking variant {other} has no Pi level mapping"),
    }
}

pub(super) fn shell(config: &RuntimeConfig, args: NameArgs) -> Result<()> {
    let record = state::load(config, &args.name)?;
    openshell::exec(
        &config.openshell_bin,
        &record,
        &[OsString::from("/bin/bash"), OsString::from("-l")],
        true,
    )
}

pub(super) fn lifecycle(
    config: &RuntimeConfig,
    output: OutputFormat,
    args: NameArgs,
    action: &str,
) -> Result<()> {
    let record = state::load(config, &args.name)?;
    if action == "stop" {
        lease::close_for_sandbox(config, &record.name, "sandbox stopped")?;
    }
    openshell::lifecycle(&config.openshell_bin, &record, action)?;
    audit::append(
        config,
        "host-controller",
        &format!("sandbox.{action}"),
        &format!("sandbox/{}", record.name),
        "passed",
        json!({}),
    )?;
    print_action(output, action, &record)
}

pub(super) fn delete(config: &RuntimeConfig, output: OutputFormat, args: DeleteArgs) -> Result<()> {
    let record = state::load(config, &args.name)?;
    let result = delete_record(config, &record, args.trash_workspace, args.skip_evidence)?;
    match output {
        OutputFormat::Human => println!(
            "Deleted {}. Work folder {}.",
            record.name,
            if result.trashed {
                "moved to Trash".into()
            } else {
                format!("kept at {}", record.host_workspace.display())
            }
        ),
        OutputFormat::Json => print_json(&result)?,
    }
    Ok(())
}

#[derive(Serialize)]
pub(super) struct DeleteResult {
    pub(super) action: &'static str,
    pub(super) name: String,
    pub(super) workspace: PathBuf,
    pub(super) trashed: bool,
    pub(super) evidence: Option<evidence::EvidenceManifest>,
}

pub(super) fn delete_record(
    config: &RuntimeConfig,
    record: &SandboxRecord,
    trash_workspace: bool,
    skip_evidence: bool,
) -> Result<DeleteResult> {
    if trash_workspace
        && (!record.managed_workspace
            || !state::is_under(&record.host_workspace, &config.root)
            || state::other_records_use_workspace(config, record)?)
    {
        bail!(
            "refusing to trash an explicit, shared, or unmanaged work folder; the sandbox was left running"
        );
    }
    lease::close_for_sandbox(config, &record.name, "sandbox deleted")?;
    let evidence = if skip_evidence {
        None
    } else {
        Some(evidence::snapshot(config, &record.name)?)
    };
    openshell::delete_and_verify(&config.openshell_bin, record)?;
    let mut trashed = false;
    if trash_workspace {
        let trash_bin = if cfg!(target_os = "macos") {
            "/usr/bin/trash"
        } else {
            "gio"
        };
        let mut command = ProcessCommand::new(trash_bin);
        if !cfg!(target_os = "macos") {
            command.arg("trash");
        }
        let status = command
            .arg(&record.host_workspace)
            .status()
            .context("failed to move the work folder to Trash")?;
        if !status.success() {
            bail!("sandbox was deleted, but moving the work folder to Trash failed");
        }
        trashed = true;
    }
    state::remove(config, record)?;
    audit::append(
        config,
        "host-controller",
        "sandbox.delete",
        &format!("sandbox/{}", record.name),
        "passed",
        json!({"workspace_trashed": trashed, "evidence": evidence.as_ref().map(|item| &item.directory), "evidence_skipped": skip_evidence}),
    )?;
    Ok(DeleteResult {
        action: "delete",
        name: record.name.clone(),
        workspace: record.host_workspace.clone(),
        trashed,
        evidence,
    })
}
