//! Resetting, stopping and deleting workers and studies.
use super::*;

pub fn reset_worker(
    config: &RuntimeConfig,
    study_name: &str,
    worker_name: &str,
    reason: &str,
) -> Result<ResearchWorker> {
    validate_text(reason, "reason")?;
    let mut study = load(config, study_name)?;
    let worker = study
        .workers
        .get_mut(worker_name)
        .context("unknown research worker")?;
    if matches!(
        worker.state,
        ResearchWorkerState::Running | ResearchWorkerState::Sealed | ResearchWorkerState::Deleted
    ) {
        bail!("running, sealed, or deleted workers cannot be reset");
    }
    let recovery = study.root.join("recovery").join(format!(
        "{}-{}-output",
        worker_name,
        OffsetDateTime::now_utc().unix_timestamp()
    ));
    fs::rename(&worker.output, &recovery)?;
    create_output_tree(&worker.output)?;
    if let Some(run_id) = &worker.state_run_id {
        agent_state::checkpoint(
            config,
            run_id,
            worker_name,
            &format!("research worker reset: {reason}"),
            "host-controller",
        )?;
        agent_state::materialize_view(config, run_id, worker_name, &worker.workspace)?;
    }
    worker.state = ResearchWorkerState::Prepared;
    worker.started_at = None;
    worker.finished_at = None;
    worker.segments_completed = 0;
    worker.last_segment_at = None;
    worker.last_result_class = None;
    worker.result_sha256 = None;
    worker.output_sha256 = None;
    let result = worker.clone();
    save(config, &study)?;
    audit::append(
        config,
        "host-controller",
        "research.worker.reset",
        &format!("research/{study_name}/worker/{worker_name}"),
        "passed",
        json!({"reason": reason, "recovery_path": recovery}),
    )?;
    Ok(result)
}

pub fn mark_stopped(config: &RuntimeConfig, study_name: &str) -> Result<ResearchStudy> {
    let mut study = load(config, study_name)?;
    for worker in study.workers.values_mut() {
        if matches!(
            worker.state,
            ResearchWorkerState::Prepared | ResearchWorkerState::Running
        ) {
            worker.state = ResearchWorkerState::Stopped;
        }
    }
    save(config, &study)?;
    Ok(study)
}

pub fn mark_worker_stopped(
    config: &RuntimeConfig,
    study_name: &str,
    worker_name: &str,
) -> Result<ResearchWorker> {
    update_worker(config, study_name, worker_name, |worker| {
        if matches!(
            worker.state,
            ResearchWorkerState::Prepared | ResearchWorkerState::Running
        ) {
            worker.state = ResearchWorkerState::Stopped;
        }
        Ok(())
    })
}

pub fn mark_deleted(config: &RuntimeConfig, study_name: &str) -> Result<ResearchStudy> {
    let mut study = load(config, study_name)?;
    for worker in study.workers.values() {
        if let Some(run_id) = &worker.state_run_id {
            agent_state::seal_run(
                config,
                run_id,
                &worker.name,
                "research worker sandbox deleted; state closed independently of result status",
                "host-controller",
            )?;
        }
    }
    for worker in study.workers.values_mut() {
        worker.sandbox_deleted = true;
        if worker.state != ResearchWorkerState::Sealed {
            worker.state = ResearchWorkerState::Deleted;
        }
    }
    save(config, &study)?;
    Ok(study)
}

pub fn mark_worker_deleted(
    config: &RuntimeConfig,
    study_name: &str,
    worker_name: &str,
) -> Result<ResearchWorker> {
    let study = load(config, study_name)?;
    let worker = study
        .workers
        .get(worker_name)
        .context("unknown research worker")?;
    if let Some(run_id) = &worker.state_run_id {
        agent_state::seal_run(
            config,
            run_id,
            &worker.name,
            "research worker sandbox deleted; state closed independently of result status",
            "host-controller",
        )?;
    }
    update_worker(config, study_name, worker_name, |worker| {
        worker.sandbox_deleted = true;
        if worker.state != ResearchWorkerState::Sealed {
            worker.state = ResearchWorkerState::Deleted;
        }
        Ok(())
    })
}

impl ResearchStudy {
    pub fn summary(self) -> ResearchStudySummary {
        ResearchStudySummary {
            name: self.name,
            mode: self.mode,
            input_file_count: self.input_file_count,
            input_bytes: self.input_bytes,
            max_output_files: self.max_output_files,
            max_output_bytes: self.max_output_bytes,
            workers: self
                .workers
                .into_values()
                .map(|worker| ResearchWorkerSummary {
                    name: worker.name,
                    sandbox: worker.sandbox,
                    route: worker.route,
                    route_context_limit: worker.route_context_limit,
                    route_output_limit: worker.route_output_limit,
                    route_pool_cap: worker.route_pool_cap,
                    gateway_profile: worker.gateway_profile,
                    harness: worker.harness,
                    segments_completed: worker.segments_completed,
                    state: worker.state,
                    tool_count: worker.tools.len(),
                    sealed: worker.sealed_at.is_some(),
                    sandbox_deleted: worker.sandbox_deleted,
                })
                .collect(),
            comparison: self.comparison,
        }
    }
}

pub(super) fn update_worker(
    config: &RuntimeConfig,
    study_name: &str,
    worker_name: &str,
    update: impl FnOnce(&mut ResearchWorker) -> Result<()>,
) -> Result<ResearchWorker> {
    let mut study = load(config, study_name)?;
    let worker = study
        .workers
        .get_mut(worker_name)
        .context("unknown research worker")?;
    update(worker)?;
    let result = worker.clone();
    save(config, &study)?;
    Ok(result)
}
