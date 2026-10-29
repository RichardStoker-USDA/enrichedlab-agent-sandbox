//! The team commands.
use super::*;

pub(super) fn team_command(
    config: &RuntimeConfig,
    output: OutputFormat,
    command: TeamCommand,
) -> Result<()> {
    match command {
        TeamCommand::Create(args) => print_value(
            output,
            &team::create(config, &args.name, &args.repo, args.shared_output)?,
            "Team created",
        ),
        TeamCommand::Add(mut args) => {
            let worker =
                team::prepare_worker(config, &args.team, &args.worker, &args.owns, &args.task)?;
            if let Some(shared) = team::load(config, &args.team)?.shared_output {
                args.runtime.read_write_mounts.push(shared);
            }
            let record = match create_one(
                config,
                Some(worker.sandbox.clone()),
                Some(worker.workspace.clone()),
                &args.runtime,
            ) {
                Ok(record) => record,
                Err(error) => {
                    let recovered = team::rollback_worker(
                        config,
                        &args.team,
                        &args.worker,
                        "sandbox creation failed",
                    );
                    return match recovered {
                        Ok(path) => Err(error).with_context(|| {
                            format!(
                                "team worker setup was rolled back; its files were kept at {}",
                                path.display()
                            )
                        }),
                        Err(rollback_error) => bail!(
                            "sandbox creation failed ({error}); worker rollback also failed ({rollback_error})"
                        ),
                    };
                }
            };
            team::attach_sandbox(config, &args.team, &args.worker, &record.name)?;
            print_value(
                output,
                &json!({"worker": worker, "sandbox": record}),
                "Team worker created",
            )
        }
        TeamCommand::Status { team: name } => {
            print_value(output, &team::load(config, &name)?, "Team status")
        }
        TeamCommand::Collect { team: name, worker } => {
            let team_record = team::load(config, &name)?;
            let worker_record = team_record
                .workers
                .get(&worker)
                .with_context(|| format!("unknown team worker {worker}"))?;
            let sandbox = state::load(config, &worker_record.sandbox)?;
            let sync = if sandbox.runtime.workspace_transport == platform::WorkspaceTransport::Sync
            {
                Some(json!({
                    "workspace": workspace_sync::pull(config, &sandbox, true)?,
                    "writable_mounts": workspace_sync::pull_writable_mounts(config, &sandbox)?
                }))
            } else {
                None
            };
            print_value(
                output,
                &json!({"team": team::collect(config, &name, &worker)?, "workspace_sync": sync}),
                "Worker collected",
            )
        }
        TeamCommand::Merge { team: name, worker } => print_value(
            output,
            &team::merge(config, &name, &worker)?,
            "Worker merged into the team integration clone",
        ),
        TeamCommand::Stop { team: name } => {
            let record = team::load(config, &name)?;
            for worker in record.workers.values() {
                if let Ok(sandbox) = state::load(config, &worker.sandbox) {
                    lease::close_for_sandbox(config, &sandbox.name, "team stopped")?;
                    openshell::lifecycle(&config.openshell_bin, &sandbox, "stop")?;
                }
            }
            audit::append(
                config,
                "host-controller",
                "team.stop",
                &format!("team/{name}"),
                "passed",
                json!({}),
            )?;
            print_value(output, &record, "Team sandboxes stopped")
        }
        TeamCommand::Delete {
            team: name,
            skip_evidence,
        } => {
            let record = team::load(config, &name)?;
            let mut deleted = Vec::new();
            for worker in record.workers.values() {
                let path = state::record_path(config, &worker.sandbox)?;
                if path.exists() {
                    let sandbox = state::load(config, &worker.sandbox)?;
                    deleted.push(
                        delete_record(config, &sandbox, false, skip_evidence).with_context(
                            || format!("failed to delete team worker {}", worker.name),
                        )?,
                    );
                }
            }
            audit::append(
                config,
                "host-controller",
                "team.delete",
                &format!("team/{name}"),
                "passed",
                json!({"deleted_sandboxes": deleted.len(), "evidence_skipped": skip_evidence, "project_retained": true}),
            )?;
            print_value(
                output,
                &json!({"team": record.name, "deleted": deleted, "project": record.root, "project_retained": true}),
                "Team sandboxes deleted; project retained",
            )
        }
    }
}
