//! Research runs: launching them, their workers and segments, and state sync.
use super::*;

#[derive(Serialize)]
pub(super) struct ResearchLaunchOutcome {
    pub(super) worker: String,
    pub(super) sandbox: String,
    pub(super) passed: bool,
    pub(super) capture: Option<research::ResearchExecutionCapture>,
    pub(super) failure_class: Option<String>,
}

#[derive(Serialize)]
pub(super) struct ResearchLaunchReport {
    pub(super) study: String,
    pub(super) max_parallel: usize,
    pub(super) max_segments: u32,
    pub(super) outcomes: Vec<ResearchLaunchOutcome>,
    pub(super) all_passed: bool,
}

pub(super) fn research_command(
    config: &RuntimeConfig,
    output: OutputFormat,
    command: ResearchCommand,
) -> Result<()> {
    match command {
        ResearchCommand::Create(args) => print_value(
            output,
            &research::create(
                config,
                research::ResearchStudySpec {
                    name: &args.name,
                    mode: args.mode,
                    question: args.question.as_deref(),
                    question_file: args.question_file.as_deref(),
                    protocol: args.protocol.as_deref(),
                    inputs: &args.inputs,
                    max_output_files: args.max_output_files,
                    max_output_bytes: args.max_output_bytes,
                },
            )?,
            "Research study created",
        ),
        ResearchCommand::Add(args) => {
            let worker = research::prepare_worker(
                config,
                research::ResearchWorkerSpec {
                    study_name: &args.study,
                    worker_name: &args.worker,
                    perspective: args.perspective.as_deref(),
                    perspective_file: args.perspective_file.as_deref(),
                    tool_names: &args.tools,
                    thinking: args.thinking,
                    harness: args.harness,
                    runtime: &args.runtime,
                },
            )?;
            let record = match create_one(
                config,
                Some(worker.sandbox.clone()),
                Some(worker.workspace.clone()),
                &args.runtime,
            ) {
                Ok(record) => record,
                Err(error) => {
                    let recovered = research::rollback_worker(
                        config,
                        &args.study,
                        &args.worker,
                        "sandbox creation failed",
                    );
                    return match recovered {
                        Ok(path) => Err(error).with_context(|| {
                            format!(
                                "research worker setup was rolled back; files were kept at {}",
                                path.display()
                            )
                        }),
                        Err(rollback_error) => bail!(
                            "sandbox creation failed ({error}); worker rollback also failed ({rollback_error})"
                        ),
                    };
                }
            };
            research::attach_sandbox(config, &args.study, &args.worker, &record.name)?;
            print_value(
                output,
                &json!({"worker": worker, "sandbox": record}),
                "Research worker created",
            )
        }
        ResearchCommand::Status { study } => {
            print_value(output, &research::load(config, &study)?, "Research status")
        }
        ResearchCommand::Launch {
            study,
            max_parallel,
            max_segments,
        } => {
            let report = launch_research(config, &study, max_parallel, max_segments)?;
            let all_passed = report.all_passed;
            print_value(output, &report, "Research workers finished")?;
            if !all_passed {
                bail!(
                    "one or more research workers failed; inspect research status and private controller logs"
                );
            }
            Ok(())
        }
        ResearchCommand::Collect { study, worker } => {
            let study_record = research::load(config, &study)?;
            let worker_record = study_record
                .workers
                .get(&worker)
                .with_context(|| format!("unknown research worker {worker}"))?;
            let sync = if worker_record.state == ResearchWorkerState::Sealed {
                None
            } else {
                let sandbox = state::load(config, &worker_record.sandbox)?;
                if sandbox.runtime.workspace_transport == platform::WorkspaceTransport::Sync {
                    Some(json!({
                        "workspace": workspace_sync::pull(config, &sandbox, true)?,
                        "writable_mounts": workspace_sync::pull_writable_mounts(config, &sandbox)?
                    }))
                } else {
                    None
                }
            };
            print_value(
                output,
                &json!({"seal": research::collect(config, &study, &worker)?, "workspace_sync": sync}),
                "Research worker sealed",
            )
        }
        ResearchCommand::Compare { study } => print_value(
            output,
            &research::compare(config, &study)?,
            "Research outputs compared",
        ),
        ResearchCommand::Verify { study } => print_value(
            output,
            &research::verify(config, &study)?,
            "Research evidence verified",
        ),
        ResearchCommand::ResetWorker {
            study,
            worker,
            reason,
        } => print_value(
            output,
            &research::reset_worker(config, &study, &worker, &reason)?,
            "Research worker reset",
        ),
        ResearchCommand::Stop { study } => {
            let study_record = research::load(config, &study)?;
            let mut stopped = Vec::new();
            let mut failures = Vec::new();
            for worker in study_record.workers.values() {
                let path = state::record_path(config, &worker.sandbox)?;
                if !path.exists() || worker.sandbox_deleted {
                    continue;
                }
                let sandbox = state::load(config, &worker.sandbox)?;
                let result = lease::close_for_sandbox(config, &sandbox.name, "research stopped")
                    .and_then(|_| openshell::lifecycle(&config.openshell_bin, &sandbox, "stop"));
                match result {
                    Ok(()) => {
                        research::mark_worker_stopped(config, &study, &worker.name)?;
                        stopped.push(worker.name.clone());
                    }
                    Err(error) => failures.push(format!("{}: {error:#}", worker.name)),
                }
            }
            audit::append(
                config,
                "host-controller",
                "research.stop",
                &format!("research/{study}"),
                if failures.is_empty() {
                    "passed"
                } else {
                    "failed"
                },
                json!({"stopped_workers": stopped, "failure_count": failures.len()}),
            )?;
            if !failures.is_empty() {
                bail!("research stop was incomplete: {}", failures.join("; "));
            }
            print_value(
                output,
                &research::load(config, &study)?,
                "Research sandboxes stopped",
            )
        }
        ResearchCommand::Delete {
            study,
            skip_evidence,
        } => {
            let study_record = research::load(config, &study)?;
            let mut deleted = Vec::new();
            for worker in study_record.workers.values() {
                if worker.sandbox_deleted {
                    research::mark_worker_deleted(config, &study, &worker.name)?;
                    continue;
                }
                let path = state::record_path(config, &worker.sandbox)?;
                if !path.exists() {
                    bail!(
                        "research worker {} has no controller record; verify its runtime before marking it deleted",
                        worker.name
                    );
                }
                let sandbox = state::load(config, &worker.sandbox)?;
                deleted.push(delete_record(config, &sandbox, false, skip_evidence)?);
                research::mark_worker_deleted(config, &study, &worker.name)?;
            }
            audit::append(
                config,
                "host-controller",
                "research.delete",
                &format!("research/{study}"),
                "passed",
                json!({"deleted_sandboxes": deleted.len(), "study_retained": true, "evidence_skipped": skip_evidence}),
            )?;
            print_value(
                output,
                &json!({
                    "study": research::load(config, &study)?,
                    "deleted": deleted,
                    "study_retained": true
                }),
                "Research sandboxes deleted; study retained",
            )
        }
    }
}

pub(super) fn launch_research(
    config: &RuntimeConfig,
    study: &str,
    max_parallel: usize,
    max_segments: u32,
) -> Result<ResearchLaunchReport> {
    if !(1..=64).contains(&max_parallel) {
        bail!("max_parallel must be between 1 and 64");
    }
    if !(1..=64).contains(&max_segments) {
        bail!("max_segments must be between 1 and 64");
    }
    let candidates = research::launch_candidates(config, study)?;
    let mut outcomes = Vec::new();
    for chunk in candidates.chunks(max_parallel) {
        let mut running = Vec::new();
        for worker in chunk {
            if worker.state == ResearchWorkerState::Stopped {
                let sandbox = state::load(config, &worker.sandbox)?;
                openshell::lifecycle(&config.openshell_bin, &sandbox, "start")?;
            }
            running.push(research::mark_running(config, study, &worker.name)?);
        }
        let completed = thread::scope(|scope| {
            let handles = running
                .into_iter()
                .map(|worker| {
                    scope.spawn(move || {
                        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                            execute_research_worker(config, study, &worker, max_segments)
                        }))
                        .unwrap_or_else(|_| Err(anyhow::anyhow!("research worker panicked")));
                        (worker, result)
                    })
                })
                .collect::<Vec<_>>();
            handles
                .into_iter()
                .map(|handle| handle.join())
                .collect::<Vec<_>>()
        });
        for completed_worker in completed {
            match completed_worker {
                Ok((worker, Ok(capture))) => {
                    research::mark_finished(
                        config,
                        study,
                        &worker.name,
                        capture.passed,
                        &capture.result_class,
                    )?;
                    outcomes.push(ResearchLaunchOutcome {
                        worker: worker.name,
                        sandbox: worker.sandbox,
                        passed: capture.passed,
                        failure_class: (!capture.passed).then(|| capture.result_class.clone()),
                        capture: Some(capture),
                    });
                }
                Ok((worker, Err(_error))) => {
                    research::mark_finished(
                        config,
                        study,
                        &worker.name,
                        false,
                        "controller-capture",
                    )?;
                    outcomes.push(ResearchLaunchOutcome {
                        worker: worker.name,
                        sandbox: worker.sandbox,
                        passed: false,
                        capture: None,
                        failure_class: Some("controller-capture".into()),
                    });
                }
                Err(_) => {
                    bail!("research worker thread failed; inspect research status before retrying")
                }
            }
        }
    }
    let all_passed = outcomes.iter().all(|outcome| outcome.passed);
    Ok(ResearchLaunchReport {
        study: study.into(),
        max_parallel,
        max_segments,
        outcomes,
        all_passed,
    })
}

pub(super) fn execute_research_worker(
    config: &RuntimeConfig,
    study: &str,
    worker: &ResearchWorker,
    max_segments: u32,
) -> Result<research::ResearchExecutionCapture> {
    let sandbox = state::load(config, &worker.sandbox)?;
    let first_segment = worker
        .segments_completed
        .checked_add(1)
        .context("research segment counter overflowed")?;
    let last_segment = first_segment
        .checked_add(max_segments - 1)
        .context("research segment limit overflowed")?;
    if let Err(error) = ensure_harness(config, &sandbox, worker.harness) {
        let detail = format!(
            "pinned {} bootstrap failed: {error:#}",
            worker.harness.as_str()
        );
        let capture = research::store_run_capture(
            config,
            study,
            &worker.name,
            first_segment,
            false,
            "controller-bootstrap",
            false,
            None,
            &[],
            detail.as_bytes(),
            false,
            false,
            research::ResearchObservationSummary::default(),
            0.0,
            "reset",
        )?;
        return Ok(research::ResearchExecutionCapture {
            schema_version: 1,
            study: study.into(),
            worker: worker.name.clone(),
            passed: false,
            result_class: "controller-bootstrap".into(),
            result_complete: false,
            segments: vec![capture],
        });
    }
    let mut captures = Vec::new();
    for segment in first_segment..=last_segment {
        let capture =
            match execute_research_segment(config, study, &worker.name, segment, last_segment) {
                Ok(capture) => capture,
                Err(error) => {
                    let detail = format!("{error:#}");
                    research::store_run_capture(
                        config,
                        study,
                        &worker.name,
                        segment,
                        false,
                        "controller-execution",
                        false,
                        None,
                        &[],
                        detail.as_bytes(),
                        false,
                        false,
                        research::ResearchObservationSummary::default(),
                        0.0,
                        "reset",
                    )?
                }
            };
        let passed = capture.passed;
        let result_complete = capture.result_complete;
        let result_class = capture.result_class.clone();
        captures.push(capture);
        if !passed {
            return Ok(research::ResearchExecutionCapture {
                schema_version: 1,
                study: study.into(),
                worker: worker.name.clone(),
                passed: false,
                result_class,
                result_complete: false,
                segments: captures,
            });
        }
        if result_complete {
            return Ok(research::ResearchExecutionCapture {
                schema_version: 1,
                study: study.into(),
                worker: worker.name.clone(),
                passed: true,
                result_class: "passed".into(),
                result_complete: true,
                segments: captures,
            });
        }
    }
    Ok(research::ResearchExecutionCapture {
        schema_version: 1,
        study: study.into(),
        worker: worker.name.clone(),
        passed: false,
        result_class: "segment-limit".into(),
        result_complete: false,
        segments: captures,
    })
}

pub(super) fn execute_research_segment(
    config: &RuntimeConfig,
    study: &str,
    worker_name: &str,
    segment: u32,
    last_segment: u32,
) -> Result<research::ResearchRunCapture> {
    const MAX_CAPTURE_BYTES: usize = 8 * 1024 * 1024;
    let study_record = research::load(config, study)?;
    let worker = study_record
        .workers
        .get(worker_name)
        .context("unknown research worker")?
        .clone();
    let sandbox = state::load(config, &worker.sandbox)?;
    let run_id = worker
        .state_run_id
        .as_deref()
        .context("research worker has no state run")?;
    agent_state::materialize_view(config, run_id, worker_name, &worker.workspace)?;
    if sandbox.runtime.workspace_transport == platform::WorkspaceTransport::Sync {
        workspace_sync::push(config, &sandbox)?;
    }
    let prompt = research::worker_segment_prompt(&study_record, &worker, segment, last_segment);
    let command = agent_command(
        config,
        &sandbox,
        worker.harness,
        worker.thinking,
        true,
        &[OsString::from(prompt)],
    )?;
    let observer = Arc::new(Mutex::new(research::ResearchEventObserver::new(
        config, study, &worker, segment,
    )?));
    let callback_observer = Arc::clone(&observer);
    let watcher = if sandbox.runtime.workspace_transport == platform::WorkspaceTransport::Bind {
        Some(start_state_watcher(
            config.clone(),
            run_id.into(),
            worker.name.clone(),
            worker.workspace.clone(),
        ))
    } else {
        None
    };
    let captured = openshell::exec_capture_bounded_observed(
        &config.openshell_bin,
        &sandbox,
        &command,
        MAX_CAPTURE_BYTES,
        move |line| {
            callback_observer
                .lock()
                .map_err(|_| anyhow::anyhow!("research observer lock was poisoned"))?
                .observe_line(line)
        },
    );
    let mut post_errors = Vec::new();
    if let Some((stop, handle)) = watcher {
        stop.store(true, Ordering::Release);
        match handle.join() {
            Ok(Ok(())) => {}
            Ok(Err(error)) => post_errors.push(format!("state watcher: {error:#}")),
            Err(_) => post_errors.push("state watcher panicked".into()),
        }
    }
    let mut captured = captured?;
    if sandbox.runtime.workspace_transport == platform::WorkspaceTransport::Sync {
        if let Err(error) = workspace_sync::pull(config, &sandbox, true) {
            post_errors.push(format!("workspace pull: {error:#}"));
        }
        if let Err(error) = workspace_sync::pull_writable_mounts(config, &sandbox) {
            post_errors.push(format!("writable mount pull: {error:#}"));
        }
    }
    let observations = match observer.lock() {
        Ok(mut observer) => match observer.finish() {
            Ok(summary) => summary,
            Err(error) => {
                post_errors.push(format!("final workspace observation: {error:#}"));
                observer.summary()
            }
        },
        Err(_) => {
            post_errors.push("research observer lock was poisoned".into());
            research::ResearchObservationSummary::default()
        }
    };
    if let Some(error) = &captured.observer_error {
        post_errors.push(format!("stream observer: {error}"));
    }
    let result_complete = match research::result_is_complete(config, study, worker_name) {
        Ok(value) => value,
        Err(error) => {
            post_errors.push(format!("result contract: {error:#}"));
            false
        }
    };
    let context = match agent_state::context_status(
        config,
        worker
            .state_profile
            .as_deref()
            .unwrap_or("scientific-worker"),
        worker.route_context_limit,
        observations.peak_context_tokens,
        worker.route_output_limit,
    ) {
        Ok(status) => Some(status),
        Err(error) => {
            post_errors.push(format!("context accounting: {error:#}"));
            None
        }
    };
    let context_pressure_ratio = context.as_ref().map_or(0.0, |status| status.pressure_ratio);
    let context_action = match context.as_ref().map(|status| status.action) {
        Some(agent_state::ContextAction::Continue) => "continue",
        Some(agent_state::ContextAction::Checkpoint) => "checkpoint",
        Some(agent_state::ContextAction::Reset) | None => "reset",
    };
    let event_stream_complete = research::event_stream_complete(
        &observations,
        captured.stdout_truncated,
        captured.stderr_truncated,
        captured.observer_error.is_some(),
        !post_errors.is_empty(),
    );
    let passed = captured.success && event_stream_complete && observations.error_events == 0;
    let result_class = if !post_errors.is_empty() {
        "controller-postprocess"
    } else if captured.stdout_truncated || captured.stderr_truncated {
        "capture-truncated"
    } else if observations.malformed_lines != 0
        || observations.step_finishes == 0
        || captured.observer_error.is_some()
    {
        "event-stream-invalid"
    } else if observations.error_events != 0 {
        "agent-error"
    } else if !captured.success {
        "agent-exit"
    } else if result_complete {
        "passed"
    } else {
        "segment-incomplete"
    };
    if !post_errors.is_empty() {
        captured
            .stderr
            .extend_from_slice(b"\n[controller postprocess errors]\n");
        captured
            .stderr
            .extend_from_slice(post_errors.join("\n").as_bytes());
    }
    let mut capture = research::store_run_capture(
        config,
        study,
        worker_name,
        segment,
        passed,
        result_class,
        result_complete,
        captured.code,
        &captured.stdout,
        &captured.stderr,
        captured.stdout_truncated,
        captured.stderr_truncated,
        observations.clone(),
        context_pressure_ratio,
        context_action,
    )?;
    if event_stream_complete {
        let checkpoint = research::checkpoint_segment(
            config,
            study,
            worker_name,
            segment,
            result_complete,
            &observations,
            context_action,
        )
        .and_then(|checkpoint| {
            research::bind_capture_checkpoint(
                config,
                study,
                worker_name,
                &mut capture,
                checkpoint.epoch,
            )
        });
        if let Err(error) = checkpoint {
            research::mark_capture_checkpoint_failed(
                config,
                study,
                worker_name,
                &mut capture,
                &format!("{error:#}"),
            )?;
        }
    }
    Ok(capture)
}

pub(super) fn start_state_watcher(
    config: RuntimeConfig,
    run_id: String,
    subject: String,
    workspace: PathBuf,
) -> (Arc<AtomicBool>, thread::JoinHandle<Result<()>>) {
    let stop = Arc::new(AtomicBool::new(false));
    let watcher_stop = Arc::clone(&stop);
    let handle = thread::spawn(move || {
        while !watcher_stop.load(Ordering::Acquire) {
            reconcile_state_if_pending(&config, &run_id, &subject, &workspace)?;
            thread::sleep(Duration::from_millis(250));
        }
        reconcile_state_if_pending(&config, &run_id, &subject, &workspace)
    });
    (stop, handle)
}

pub(super) fn reconcile_state_if_pending(
    config: &RuntimeConfig,
    run_id: &str,
    subject: &str,
    workspace: &std::path::Path,
) -> Result<()> {
    let outbox = workspace.join(".agent-state/outbox");
    let pending = fs::read_dir(&outbox)?.any(|entry| {
        entry.ok().is_some_and(|entry| {
            entry.path().extension().and_then(|value| value.to_str()) == Some("json")
        })
    });
    if pending {
        agent_state::reconcile_outbox(config, run_id, subject, workspace)?;
    }
    Ok(())
}

pub(super) fn sync_command(
    config: &RuntimeConfig,
    output: OutputFormat,
    command: SyncCommand,
) -> Result<()> {
    match command {
        SyncCommand::Push { sandbox } => {
            let record = state::load(config, &sandbox)?;
            print_value(
                output,
                &workspace_sync::push(config, &record)?,
                "Workspace pushed",
            )
        }
        SyncCommand::Pull { sandbox, apply } => {
            let record = state::load(config, &sandbox)?;
            print_value(
                output,
                &workspace_sync::pull(config, &record, apply)?,
                if apply {
                    "Workspace pulled and applied after backup"
                } else {
                    "Workspace snapshot pulled"
                },
            )
        }
    }
}
