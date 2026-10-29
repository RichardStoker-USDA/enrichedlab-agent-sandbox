//! Continuity agents: create, start, release and run them, and each attempt they make.
use super::*;

#[derive(Serialize)]
pub(super) struct ContinuityStartResult {
    pub(super) agent: agent_continuity::LogicalAgentRecord,
    pub(super) sandbox: SandboxRecord,
    pub(super) reused_runtime: bool,
    pub(super) runtime_restarted: bool,
}

#[derive(Serialize)]
pub(super) struct ContinuityReleaseResult {
    pub(super) agent: agent_continuity::LogicalAgentRecord,
    pub(super) released_sandbox: Option<String>,
    pub(super) checkpoint: Option<agent_state::StateCheckpoint>,
    pub(super) evidence: Option<evidence::EvidenceManifest>,
}

#[derive(Serialize)]
pub(super) struct ContinuityRunResult {
    pub(super) agent: agent_continuity::LogicalAgentRecord,
    pub(super) sandbox: String,
    pub(super) provenance_run: String,
    pub(super) segment: u32,
    pub(super) attempts: u32,
    pub(super) passed: bool,
    pub(super) result_class: String,
    pub(super) assistant_output: String,
    pub(super) runtime_released: bool,
}

pub(super) struct ContinuityAttemptResult {
    pub(super) passed: bool,
    pub(super) result_class: String,
    pub(super) assistant_output: String,
    pub(super) observation_ref: String,
    pub(super) lines: u64,
    pub(super) events: usize,
}

pub(super) fn continuity_agent_command(
    config: &RuntimeConfig,
    output: OutputFormat,
    command: ContinuityAgentCommand,
) -> Result<()> {
    match command {
        ContinuityAgentCommand::Create(args) => {
            let record = create_continuity_agent(config, *args)?;
            print_value(output, &record, "Logical agent registered")
        }
        ContinuityAgentCommand::Status { name } => match name {
            Some(name) => print_value(
                output,
                &agent_continuity::load(config, &name)?,
                "Logical agent",
            ),
            None => print_value(output, &agent_continuity::list(config)?, "Logical agents"),
        },
        ContinuityAgentCommand::Start { name } => {
            let _guard = agent_continuity::lock(config, &name)?;
            let mut agent = agent_continuity::load(config, &name)?;
            agent_continuity::ensure_registered_state(config, &mut agent)?;
            let result = start_continuity_agent_locked(config, &mut agent)?;
            print_value(output, &result, "Logical-agent runtime ready")
        }
        ContinuityAgentCommand::Run(args) => {
            let result = run_continuity_agent(config, *args)?;
            let passed = result.passed;
            match output {
                OutputFormat::Json => print_json(&result)?,
                OutputFormat::Human => {
                    if !result.assistant_output.is_empty() {
                        println!("{}", result.assistant_output);
                    }
                    println!(
                        "Agent {} session {}: {} ({})",
                        result.agent.name,
                        result.segment,
                        if result.passed { "passed" } else { "failed" },
                        result.result_class
                    );
                    println!("Provenance run: {}", result.provenance_run);
                }
            }
            if !passed {
                bail!("logical-agent task failed after bounded retries");
            }
            Ok(())
        }
        ContinuityAgentCommand::Release {
            name,
            skip_evidence,
        } => {
            let _guard = agent_continuity::lock(config, &name)?;
            let mut agent = agent_continuity::load(config, &name)?;
            let result = release_continuity_agent_locked(config, &mut agent, skip_evidence)?;
            print_value(output, &result, "Logical-agent runtime released")
        }
        ContinuityAgentCommand::Checkpoint { name, reason } => {
            let _guard = agent_continuity::lock(config, &name)?;
            let agent = agent_continuity::load(config, &name)?;
            let checkpoint = agent_state::checkpoint(
                config,
                &agent.state_run_id,
                &agent.agent_id,
                &reason,
                "host-controller",
            )?;
            agent_state::materialize_view(
                config,
                &agent.state_run_id,
                &agent.agent_id,
                &agent.workspace,
            )?;
            print_value(output, &checkpoint, "Logical-agent checkpoint written")
        }
        ContinuityAgentCommand::Retire {
            name,
            reason,
            skip_evidence,
        } => {
            let _guard = agent_continuity::lock(config, &name)?;
            let mut agent = agent_continuity::load(config, &name)?;
            let release = release_continuity_agent_locked(config, &mut agent, skip_evidence)?;
            agent_state::seal_run(
                config,
                &agent.state_run_id,
                &agent.agent_id,
                &reason,
                "host-controller",
            )?;
            agent_continuity::retire(config, &mut agent)?;
            audit::append(
                config,
                "host-controller",
                "logical-agent.retire",
                &format!("logical-agent/{}", agent.name),
                "passed",
                json!({"reason": reason, "state_retained": true}),
            )?;
            print_value(
                output,
                &json!({"agent": agent, "release": release}),
                "Logical agent retired; state retained",
            )
        }
    }
}

pub(super) fn create_continuity_agent(
    config: &RuntimeConfig,
    args: ContinuityAgentCreateArgs,
) -> Result<agent_continuity::LogicalAgentRecord> {
    let objective = match (args.objective, args.objective_file) {
        (Some(value), None) => value,
        (None, Some(path)) => read_bounded_objective(&path)?,
        (None, None) => bail!("logical-agent creation requires --objective or --objective-file"),
        (Some(_), Some(_)) => bail!("objective inputs are mutually exclusive"),
    };
    let runtime = resolve_continuity_runtime(config, args.runtime)?;
    agent_continuity::create(
        config,
        agent_continuity::CreateLogicalAgent {
            name: &args.name,
            owner: &args.owner,
            domain: args.domain,
            state_profile: &args.state_profile,
            objective: &objective,
            role: &args.role,
            harness: args.harness.into(),
            thinking: args.thinking,
            runtime,
        },
    )
}

pub(super) fn resolve_continuity_runtime(
    config: &RuntimeConfig,
    mut runtime: RuntimeArgs,
) -> Result<RuntimeArgs> {
    let settings = config.load_settings()?;
    runtime.route.get_or_insert(settings.default_route.clone());
    runtime
        .profile
        .get_or_insert(settings.default_profile.clone());
    runtime
        .gateway_profile
        .get_or_insert(settings.default_gateway_profile.clone());
    let route_name = runtime
        .route
        .as_deref()
        .context("runtime route is missing")?;
    let route = RouteCatalog::load(config)?.get(route_name)?.clone();
    if route.model.is_none() {
        bail!("logical-agent route {route_name} has no inference model");
    }
    settings.gateway_profile(
        runtime
            .gateway_profile
            .as_deref()
            .context("runtime gateway profile is missing")?,
    )?;
    Ok(runtime)
}

pub(super) fn read_bounded_objective(path: &std::path::Path) -> Result<String> {
    let metadata = fs::symlink_metadata(path)
        .with_context(|| format!("failed to inspect objective file {}", path.display()))?;
    if metadata.file_type().is_symlink() || !metadata.is_file() || metadata.len() > 8_192 {
        bail!("objective file must be a regular non-symlink file no larger than 8192 bytes");
    }
    let value = fs::read_to_string(path)
        .with_context(|| format!("failed to read objective file {}", path.display()))?;
    if value.trim().is_empty() {
        bail!("objective file is empty");
    }
    Ok(value)
}

pub(super) fn start_continuity_agent_locked(
    config: &RuntimeConfig,
    agent: &mut agent_continuity::LogicalAgentRecord,
) -> Result<ContinuityStartResult> {
    if agent.lifecycle == agent_continuity::LogicalAgentLifecycle::Retired {
        bail!("logical agent is retired: {}", agent.name);
    }
    if let Some(active_name) = agent.active_sandbox.clone() {
        match state::load(config, &active_name) {
            Ok(sandbox) => {
                let live = openshell::live_sandboxes(
                    &config.openshell_bin,
                    &sandbox.runtime.openshell_gateway,
                )?;
                if let Some(status) = find_live_status(&live, &active_name) {
                    let mut restarted = false;
                    if !is_runnable_status(status) {
                        openshell::lifecycle(&config.openshell_bin, &sandbox, "start")?;
                        let refreshed = openshell::live_sandboxes(
                            &config.openshell_bin,
                            &sandbox.runtime.openshell_gateway,
                        )?;
                        let refreshed_status = find_live_status(&refreshed, &active_name)
                            .context("logical-agent runtime disappeared while it was restarting")?;
                        if !is_runnable_status(refreshed_status) {
                            bail!(
                                "logical-agent runtime {active_name} is not runnable after restart"
                            );
                        }
                        restarted = true;
                        audit::append(
                            config,
                            "host-controller",
                            "logical-agent.runtime.restart",
                            &format!("logical-agent/{}", agent.name),
                            "passed",
                            json!({"sandbox": active_name, "previous_status": status}),
                        )?;
                    }
                    return Ok(ContinuityStartResult {
                        agent: agent.clone(),
                        sandbox,
                        reused_runtime: true,
                        runtime_restarted: restarted,
                    });
                }
                delete_record(config, &sandbox, false, true)?;
                agent_continuity::detach_runtime(config, agent)?;
                audit::append(
                    config,
                    "host-controller",
                    "logical-agent.runtime.reconcile",
                    &format!("logical-agent/{}", agent.name),
                    "passed",
                    json!({"missing_sandbox": active_name, "controller_record_removed": true}),
                )?;
            }
            Err(record_error) => {
                let settings = config.load_settings()?;
                let gateway_name = agent
                    .runtime
                    .gateway_profile
                    .as_deref()
                    .context("logical-agent runtime has no gateway profile")?;
                let gateway = &settings.gateway_profile(gateway_name)?.openshell_gateway;
                let live = openshell::live_sandboxes(&config.openshell_bin, gateway)?;
                if find_live_status(&live, &active_name).is_some() {
                    return Err(record_error).context(format!(
                        "logical-agent sandbox {active_name} is live but its controller record is missing"
                    ));
                }
                agent_continuity::detach_runtime(config, agent)?;
            }
        }
    }
    agent_state::materialize_view(
        config,
        &agent.state_run_id,
        &agent.agent_id,
        &agent.workspace,
    )?;
    let (generation, sandbox_name) = agent_continuity::next_runtime(agent)?;
    let sandbox = create_one(
        config,
        Some(sandbox_name.clone()),
        Some(agent.workspace.clone()),
        &agent.runtime,
    )?;
    if let Err(error) = agent_continuity::attach_runtime(config, agent, generation, &sandbox.name) {
        return match delete_record(config, &sandbox, false, true) {
            Ok(_) => {
                Err(error).context("new runtime was removed after logical-agent attach failed")
            }
            Err(cleanup) => Err(error).context(format!(
                "logical-agent attach failed and runtime cleanup also failed: {cleanup:#}"
            )),
        };
    }
    audit::append(
        config,
        "host-controller",
        "logical-agent.runtime.allocate",
        &format!("logical-agent/{}", agent.name),
        "passed",
        json!({
            "agent_id": agent.agent_id,
            "sandbox": sandbox.name,
            "generation": generation,
            "gateway_profile": sandbox.runtime.gateway_profile,
            "workspace_transport": sandbox.runtime.workspace_transport
        }),
    )?;
    Ok(ContinuityStartResult {
        agent: agent.clone(),
        sandbox,
        reused_runtime: false,
        runtime_restarted: false,
    })
}

pub(super) fn is_runnable_status(status: &str) -> bool {
    matches!(status.to_ascii_lowercase().as_str(), "ready" | "running")
}

pub(super) fn release_continuity_agent_locked(
    config: &RuntimeConfig,
    agent: &mut agent_continuity::LogicalAgentRecord,
    skip_evidence: bool,
) -> Result<ContinuityReleaseResult> {
    let Some(sandbox_name) = agent.active_sandbox.clone() else {
        return Ok(ContinuityReleaseResult {
            agent: agent.clone(),
            released_sandbox: None,
            checkpoint: None,
            evidence: None,
        });
    };
    let sandbox = match state::load(config, &sandbox_name) {
        Ok(sandbox) => Some(sandbox),
        Err(error) => {
            let settings = config.load_settings()?;
            let gateway_name = agent
                .runtime
                .gateway_profile
                .as_deref()
                .context("logical-agent runtime has no gateway profile")?;
            let gateway = &settings.gateway_profile(gateway_name)?.openshell_gateway;
            let live = openshell::live_sandboxes(&config.openshell_bin, gateway)?;
            if find_live_status(&live, &sandbox_name).is_some() {
                return Err(error).context(format!(
                    "cannot release live sandbox {sandbox_name} without its controller record"
                ));
            }
            None
        }
    };
    if let Some(sandbox) = &sandbox {
        sync_logical_agent_workspace(config, agent, sandbox)?;
    } else {
        agent_state::reconcile_outbox(
            config,
            &agent.state_run_id,
            &agent.agent_id,
            &agent.workspace,
        )?;
    }
    let checkpoint = agent_state::checkpoint(
        config,
        &agent.state_run_id,
        &agent.agent_id,
        "runtime released after durable state reconciliation",
        "host-controller",
    )?;
    agent_state::materialize_view(
        config,
        &agent.state_run_id,
        &agent.agent_id,
        &agent.workspace,
    )?;
    let evidence = if let Some(sandbox) = &sandbox {
        delete_record(config, sandbox, false, skip_evidence)?.evidence
    } else {
        None
    };
    agent_continuity::detach_runtime(config, agent)?;
    audit::append(
        config,
        "host-controller",
        "logical-agent.runtime.release",
        &format!("logical-agent/{}", agent.name),
        "passed",
        json!({
            "sandbox": sandbox_name,
            "runtime_generation": agent.runtime_generation,
            "checkpoint_epoch": checkpoint.epoch,
            "evidence_skipped": skip_evidence,
            "state_retained": true,
            "workspace_retained": true
        }),
    )?;
    Ok(ContinuityReleaseResult {
        agent: agent.clone(),
        released_sandbox: Some(sandbox_name),
        checkpoint: Some(checkpoint),
        evidence,
    })
}

pub(super) fn sync_logical_agent_workspace(
    config: &RuntimeConfig,
    agent: &agent_continuity::LogicalAgentRecord,
    sandbox: &SandboxRecord,
) -> Result<()> {
    if sandbox.runtime.workspace_transport == platform::WorkspaceTransport::Sync {
        workspace_sync::pull(config, sandbox, true)?;
        workspace_sync::pull_writable_mounts(config, sandbox)?;
    }
    agent_state::reconcile_outbox(
        config,
        &agent.state_run_id,
        &agent.agent_id,
        &agent.workspace,
    )?;
    Ok(())
}

pub(super) fn run_continuity_agent(
    config: &RuntimeConfig,
    args: ContinuityAgentRunArgs,
) -> Result<ContinuityRunResult> {
    if !(1..=8).contains(&args.max_attempts) {
        bail!("max attempts must be between 1 and 8");
    }
    if args.retry_delay_ms > 30_000 {
        bail!("retry delay cannot exceed 30000 milliseconds");
    }
    if !(30..=86_400).contains(&args.attempt_timeout_seconds) {
        bail!("attempt timeout must be between 30 and 86400 seconds");
    }
    let task = args
        .message
        .iter()
        .map(|part| part.to_string_lossy())
        .collect::<Vec<_>>()
        .join(" ");
    if task.trim().is_empty() || task.len() > 256 * 1024 {
        bail!("logical-agent task must contain 1-262144 bytes");
    }
    let _guard = agent_continuity::lock(config, &args.name)?;
    let mut agent = agent_continuity::load(config, &args.name)?;
    agent_continuity::ensure_registered_state(config, &mut agent)?;
    let start = start_continuity_agent_locked(config, &mut agent)?;
    let sandbox = start.sandbox;
    let harness = args.harness.map(Into::into).unwrap_or(agent.harness);
    let thinking = args.thinking.or(agent.thinking);
    let prompt = continuity_prompt(&agent, &task);
    let execution = execute_continuity_sessions(
        config,
        &mut agent,
        &sandbox,
        harness,
        thinking,
        args.attempt_timeout_seconds,
        args.max_attempts,
        args.retry_delay_ms,
        &prompt,
    );
    let release = if args.keep_runtime {
        Ok(false)
    } else {
        release_continuity_agent_locked(config, &mut agent, false).map(|_| true)
    };
    let (execution, runtime_released) = match (execution, release) {
        (Ok(execution), Ok(released)) => (execution, released),
        (Err(error), Ok(_)) => return Err(error),
        (Ok(_), Err(release_error)) => {
            return Err(release_error)
                .context("logical-agent task finished, but automatic runtime release failed");
        }
        (Err(error), Err(release_error)) => {
            return Err(error).context(format!(
                "logical-agent task failed and automatic runtime release also failed: {release_error:#}"
            ));
        }
    };
    let final_attempt = execution.final_attempt;
    let result = ContinuityRunResult {
        agent: agent.clone(),
        sandbox: sandbox.name,
        provenance_run: agent.state_run_id.clone(),
        segment: execution.last_segment,
        attempts: execution.attempts,
        passed: final_attempt.passed,
        result_class: final_attempt.result_class,
        assistant_output: final_attempt.assistant_output,
        runtime_released,
    };
    audit::append(
        config,
        "host-controller",
        "logical-agent.task",
        &format!("logical-agent/{}", agent.name),
        if result.passed { "passed" } else { "failed" },
        json!({
            "first_segment": execution.first_segment,
            "last_segment": execution.last_segment,
            "attempts": execution.attempts,
            "result_class": result.result_class,
            "runtime_released": runtime_released,
            "lines": final_attempt.lines,
            "events": final_attempt.events
        }),
    )?;
    Ok(result)
}

pub(super) struct ContinuityExecutionResult {
    pub(super) first_segment: u32,
    pub(super) last_segment: u32,
    pub(super) attempts: u32,
    pub(super) final_attempt: ContinuityAttemptResult,
}

#[allow(clippy::too_many_arguments)]
pub(super) fn execute_continuity_sessions(
    config: &RuntimeConfig,
    agent: &mut agent_continuity::LogicalAgentRecord,
    sandbox: &SandboxRecord,
    harness: HarnessKind,
    thinking: Option<crate::cli::Thinking>,
    attempt_timeout_seconds: u64,
    max_attempts: u32,
    retry_delay_ms: u64,
    prompt: &str,
) -> Result<ContinuityExecutionResult> {
    ensure_harness(config, sandbox, harness)?;
    let mut first_segment = 0_u32;
    let mut last_segment = 0_u32;
    let mut attempts = 0_u32;
    let mut final_attempt = ContinuityAttemptResult {
        passed: false,
        result_class: "not-started".into(),
        assistant_output: String::new(),
        observation_ref: String::new(),
        lines: 0,
        events: 0,
    };
    for attempt in 1..=max_attempts {
        attempts = attempt;
        agent_state::materialize_view(
            config,
            &agent.state_run_id,
            &agent.agent_id,
            &agent.workspace,
        )?;
        if sandbox.runtime.workspace_transport == platform::WorkspaceTransport::Sync {
            workspace_sync::push(config, sandbox)?;
        }
        let segment = agent_continuity::begin_session(config, agent)?;
        if first_segment == 0 {
            first_segment = segment;
        }
        last_segment = segment;
        let attempt_result = run_continuity_attempt(
            config,
            agent,
            sandbox,
            harness,
            thinking,
            attempt_timeout_seconds,
            segment,
            attempt,
            prompt,
        );
        let mut result = match attempt_result {
            Ok(result) => result,
            Err(_error) => {
                let event = agent_state::append_event(
                    config,
                    &agent.state_run_id,
                    &agent.agent_id,
                    "logical-agent.session.controller-error",
                    "host-controller",
                    agent_state::StateAuthority::Machine,
                    json!({
                        "segment": segment,
                        "attempt": attempt,
                        "sandbox": sandbox.name,
                        "runtime_generation": agent.runtime_generation,
                        "error_class": "controller-error"
                    }),
                )?;
                audit::append(
                    config,
                    "host-controller",
                    "logical-agent.session",
                    &format!("logical-agent/{}", agent.name),
                    "failed",
                    json!({"segment": segment, "attempt": attempt, "error_class": "controller-error"}),
                )?;
                ContinuityAttemptResult {
                    passed: false,
                    result_class: "controller-error".into(),
                    assistant_output: String::new(),
                    observation_ref: format!("state-event:{}", event.event_id),
                    lines: 0,
                    events: 0,
                }
            }
        };
        if let Err(_error) = sync_logical_agent_workspace(config, agent, sandbox) {
            result.passed = false;
            result.result_class = "state-reconciliation-error".into();
            let event = agent_state::append_event(
                config,
                &agent.state_run_id,
                &agent.agent_id,
                "logical-agent.state.reconciliation-failed",
                "host-controller",
                agent_state::StateAuthority::Machine,
                json!({
                    "segment": segment,
                    "attempt": attempt,
                    "sandbox": sandbox.name,
                    "runtime_generation": agent.runtime_generation,
                    "error_class": "state-reconciliation-error"
                }),
            )?;
            result.observation_ref = format!("state-event:{}", event.event_id);
            audit::append(
                config,
                "host-controller",
                "logical-agent.state.reconcile",
                &format!("logical-agent/{}", agent.name),
                "failed",
                json!({"segment": segment, "attempt": attempt, "error_class": "state-reconciliation-error"}),
            )?;
        }
        let retry_pending = !result.passed && attempt < max_attempts;
        agent_continuity::record_execution_attempt(
            config,
            agent,
            result.passed,
            retry_pending,
            attempt,
            &result.observation_ref,
        )?;
        agent_continuity::record_session(config, agent, result.passed, &result.result_class)?;
        agent_state::checkpoint(
            config,
            &agent.state_run_id,
            &agent.agent_id,
            &format!("provider session {segment} attempt {attempt} finished"),
            "host-controller",
        )?;
        final_attempt = result;
        if final_attempt.passed || !retry_pending {
            break;
        }
        let multiplier = 1_u64 << attempt.saturating_sub(1).min(6);
        let delay = retry_delay_ms.saturating_mul(multiplier).min(30_000);
        thread::sleep(Duration::from_millis(delay));
    }
    Ok(ContinuityExecutionResult {
        first_segment,
        last_segment,
        attempts,
        final_attempt,
    })
}

#[allow(clippy::too_many_arguments)]
pub(super) fn run_continuity_attempt(
    config: &RuntimeConfig,
    agent: &agent_continuity::LogicalAgentRecord,
    sandbox: &SandboxRecord,
    harness: HarnessKind,
    thinking: Option<crate::cli::Thinking>,
    attempt_timeout_seconds: u64,
    segment: u32,
    attempt: u32,
    prompt: &str,
) -> Result<ContinuityAttemptResult> {
    let command = agent_command(
        config,
        sandbox,
        harness,
        thinking,
        true,
        &[OsString::from(prompt)],
    )?;
    let routes = RouteCatalog::load(config)?;
    let route = routes.get(&sandbox.route)?;
    let writer = Arc::new(IncrementalProvenanceWriter {
        config: config.clone(),
        run_id: agent.state_run_id.clone(),
        subject: agent.agent_id.clone(),
        segment,
        harness,
        model: route.model.clone(),
        engine: route.inference_engine.clone(),
        parser: route.reasoning_parser.clone(),
    });
    let observer = Arc::new(Mutex::new(DirectRunObserver::new(harness)));
    let callback_observer = Arc::clone(&observer);
    let callback_writer = Arc::clone(&writer);
    let continuity_observer = Arc::new(Mutex::new(ContinuityWorkspaceObserver::new(
        config.clone(),
        agent.state_run_id.clone(),
        agent.agent_id.clone(),
        agent.workspace.clone(),
        segment,
    )));
    let callback_continuity_observer = Arc::clone(&continuity_observer);
    let deferred_sync_observations = Arc::new(Mutex::new(Vec::<(
        CanonicalHarnessEvent,
        provenance::ProvenanceAppendReceipt,
    )>::new()));
    let callback_deferred_sync_observations = Arc::clone(&deferred_sync_observations);
    let defer_snapshot_until_workspace_pull =
        sandbox.runtime.workspace_transport == platform::WorkspaceTransport::Sync;
    let persistence_failed = Arc::new(AtomicBool::new(false));
    let callback_persistence_failed = Arc::clone(&persistence_failed);
    let watcher = if sandbox.runtime.workspace_transport == platform::WorkspaceTransport::Bind {
        Some(start_state_watcher(
            config.clone(),
            agent.state_run_id.clone(),
            agent.agent_id.clone(),
            agent.workspace.clone(),
        ))
    } else {
        None
    };
    let capture = openshell::exec_capture_bounded_observed_timeout(
        &config.openshell_bin,
        sandbox,
        &command,
        MAX_DIRECT_RUN_CAPTURE_BYTES,
        attempt_timeout_seconds,
        move |line| {
            let events = callback_observer
                .lock()
                .map_err(|_| anyhow::anyhow!("logical-agent observer lock was poisoned"))?
                .observe_line(line)?;
            let receipts = match callback_writer.append_all(&events) {
                Ok(receipts) => receipts,
                Err(error) => {
                    callback_persistence_failed.store(true, Ordering::Release);
                    return Err(error)
                        .context("canonical provenance persistence failed during harness capture");
                }
            };
            if defer_snapshot_until_workspace_pull {
                let mut deferred = callback_deferred_sync_observations.lock().map_err(|_| {
                    anyhow::anyhow!("continuity workspace observer lock was poisoned")
                })?;
                deferred.extend(events.into_iter().zip(receipts));
            } else if let Err(error) = callback_continuity_observer
                .lock()
                .map_err(|_| anyhow::anyhow!("continuity workspace observer lock was poisoned"))?
                .capture_after_terminal_events(&events, &receipts)
            {
                callback_persistence_failed.store(true, Ordering::Release);
                return Err(error)
                    .context("continuity evidence capture failed during harness capture");
            }
            Ok(())
        },
    );
    if let Some((stop, handle)) = watcher {
        stop.store(true, Ordering::Release);
        handle
            .join()
            .map_err(|_| anyhow::anyhow!("logical-agent state watcher panicked"))??;
    }
    let captured = capture?;
    let mut observer = observer
        .lock()
        .map_err(|_| anyhow::anyhow!("logical-agent observer lock was poisoned"))?;
    let lines = observer.lines;
    let (final_events, adapter_finalized) = match observer.finish() {
        Ok(events) => (events, true),
        Err(_) if captured.timed_out => (Vec::new(), false),
        Err(error) => return Err(error),
    };
    let events = observer.take_observed();
    drop(observer);
    if !persistence_failed.load(Ordering::Acquire) {
        let receipts = match writer.append_all(&final_events) {
            Ok(receipts) => receipts,
            Err(error) => {
                persistence_failed.store(true, Ordering::Release);
                return Err(error).context(
                    "canonical provenance persistence failed during harness finalization",
                );
            }
        };
        if defer_snapshot_until_workspace_pull {
            deferred_sync_observations
                .lock()
                .map_err(|_| anyhow::anyhow!("continuity workspace observer lock was poisoned"))?
                .extend(final_events.iter().cloned().zip(receipts));
        } else if let Err(error) = continuity_observer
            .lock()
            .map_err(|_| anyhow::anyhow!("continuity workspace observer lock was poisoned"))?
            .capture_after_terminal_events(&final_events, &receipts)
        {
            persistence_failed.store(true, Ordering::Release);
            return Err(error)
                .context("continuity evidence capture failed during harness finalization");
        }
    }
    if defer_snapshot_until_workspace_pull && !persistence_failed.load(Ordering::Acquire) {
        workspace_sync::pull(config, sandbox, true)?;
        workspace_sync::pull_writable_mounts(config, sandbox)?;
        let deferred = std::mem::take(
            &mut *deferred_sync_observations
                .lock()
                .map_err(|_| anyhow::anyhow!("continuity workspace observer lock was poisoned"))?,
        );
        let (events, receipts): (Vec<_>, Vec<_>) = deferred.into_iter().unzip();
        if let Err(error) = continuity_observer
            .lock()
            .map_err(|_| anyhow::anyhow!("continuity workspace observer lock was poisoned"))?
            .capture_after_terminal_events(&events, &receipts)
        {
            persistence_failed.store(true, Ordering::Release);
            return Err(error).context("continuity evidence capture failed after workspace pull");
        }
    }
    let provenance_persistence_failed = persistence_failed.load(Ordering::Acquire);
    let error_events = events
        .iter()
        .filter(|event| event.evidence_role == "error")
        .count();
    let terminal = CanonicalHarnessEvent {
        kind: "logical-agent.session.finished".into(),
        evidence_role: "lifecycle".into(),
        visibility: EvidenceVisibility::Metadata,
        source_event_id: None,
        parent_source_event_id: None,
        metadata: json!({
            "segment": segment,
            "attempt": attempt,
            "sandbox": sandbox.name,
            "runtime_generation": agent.runtime_generation,
            "success": captured.success && !provenance_persistence_failed,
            "exit_code": captured.code,
            "timed_out": captured.timed_out,
            "attempt_timeout_seconds": attempt_timeout_seconds,
            "adapter_finalized": adapter_finalized,
            "stdout_bytes": captured.stdout.len(),
            "stderr_bytes": captured.stderr.len(),
            "stdout_truncated": captured.stdout_truncated,
            "stderr_truncated": captured.stderr_truncated,
            "observer_failed": captured.observer_error.is_some(),
            "provenance_persistence_failed": provenance_persistence_failed,
            "harness_error_events": error_events
        }),
        restricted_content: None,
    };
    writer.append(&terminal)?;
    let assistant_output = if provenance_persistence_failed {
        String::new()
    } else {
        events
            .iter()
            .filter(|event| event.evidence_role == "output")
            .filter_map(|event| event.restricted_content.as_deref())
            .collect::<Vec<_>>()
            .join("\n")
    };
    let passed = captured.success
        && !captured.timed_out
        && !captured.stdout_truncated
        && !captured.stderr_truncated
        && captured.observer_error.is_none()
        && !provenance_persistence_failed
        && error_events == 0;
    let result_class = if provenance_persistence_failed {
        "provenance-persistence-failed"
    } else if captured.timed_out {
        "attempt-timeout"
    } else if captured.stdout_truncated || captured.stderr_truncated {
        "capture-limit"
    } else if captured.observer_error.is_some() {
        "malformed-event-stream"
    } else if error_events > 0 {
        "harness-error-event"
    } else if !captured.success {
        "harness-exit"
    } else {
        "passed"
    };
    let event = agent_state::append_event(
        config,
        &agent.state_run_id,
        &agent.agent_id,
        "logical-agent.session.finished",
        "host-controller",
        agent_state::StateAuthority::Machine,
        json!({
            "segment": segment,
            "attempt": attempt,
            "sandbox": sandbox.name,
            "runtime_generation": agent.runtime_generation,
            "passed": passed,
            "result_class": result_class,
            "provenance_events": events.len() + 1
        }),
    )?;
    Ok(ContinuityAttemptResult {
        passed,
        result_class: result_class.into(),
        assistant_output,
        observation_ref: format!("state-event:{}", event.event_id),
        lines,
        events: events.len() + 1,
    })
}

pub(super) fn continuity_prompt(
    agent: &agent_continuity::LogicalAgentRecord,
    task: &str,
) -> String {
    format!(
        "You are logical agent {name} ({agent_id}), continuing the durable role: {role}.\n\
Your current objective is: {objective}\n\
Read /sandbox/work/.agent-state/view.json before acting. It is the bounded, scoped authority for prior work, decisions, lessons, pending work, and the current epoch. The sandbox and provider session are disposable. Do not infer memory from files that are absent from the state view.\n\
Use tools for deterministic analysis and retain input, method, output, validation, and failed-attempt references under /sandbox/work/evidence. That directory is the only continuity workspace path captured as immutable scientific file versions after terminal tool observations. The language model coordinates the analysis; it is not a substitute for the scientific method or deterministic analysis tool.\n\
To update durable state, write one complete StateMutation JSON file to /sandbox/work/.agent-state/outbox/.<unique>.pending and atomically rename it to a .json filename. Use the run_id, subject, collection, and base_version from the current view. Store short evidence-backed decisions and lessons, not a hidden chain-of-thought transcript.\n\
Current bounded task:\n{task}",
        name = agent.name,
        agent_id = agent.agent_id,
        role = agent.role,
        objective = agent.objective,
    )
}
