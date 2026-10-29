//! Starting and sealing a run.
use super::*;

pub fn init_run(
    config: &RuntimeConfig,
    run_id: &str,
    domain: StateDomain,
    profile_name: &str,
    subject: &str,
    team: Option<&str>,
) -> Result<StateRun> {
    validate_identifier(run_id, "run id")?;
    validate_identifier(subject, "subject")?;
    if let Some(team) = team {
        validate_identifier(team, "team")?;
    }
    let profile = load_profile(config, profile_name)?;
    let created_at = now()?;
    let mut connection = open_database(config)?;
    let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let run = StateRun {
        schema_version: 1,
        run_id: run_id.into(),
        domain,
        profile: profile.name,
        subject: subject.into(),
        team: team.map(str::to_owned),
        status: "active".into(),
        epoch: 0,
        created_at: created_at.clone(),
        updated_at: created_at.clone(),
    };
    transaction
        .execute(
            "INSERT INTO state_runs (run_id, domain, profile, subject, team, status, epoch, created_at, updated_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6, 0, ?7, ?7)",
            params![run.run_id, run.domain.as_str(), run.profile, run.subject, run.team, run.status, created_at],
        )
        .with_context(|| format!("state run already exists: {run_id}"))?;
    let event = append_event_tx(
        &transaction,
        EventDraft {
            run_id,
            subject,
            epoch: 0,
            kind: "state.run.created",
            actor: "host-controller",
            authority: StateAuthority::Operator,
            payload: json!({"domain": domain, "profile": run.profile, "team": run.team}),
        },
    )?;
    transaction.commit()?;
    audit::append(
        config,
        "host-controller",
        "agent-state.run.create",
        &format!("agent-state/{run_id}"),
        "passed",
        json!({"domain": domain, "profile": run.profile, "subject": subject, "event_id": event.event_id}),
    )?;
    Ok(run)
}

pub fn seal_run(
    config: &RuntimeConfig,
    run_id: &str,
    subject: &str,
    reason: &str,
    actor: &str,
) -> Result<StateRun> {
    validate_identifier(run_id, "run id")?;
    validate_identifier(subject, "subject")?;
    validate_text(reason, "seal reason", 4096)?;
    validate_text(actor, "actor", 256)?;
    let mut connection = open_database(config)?;
    let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let run = load_run_tx(&transaction, run_id)?;
    if run.subject != subject {
        bail!("state subject does not match the run owner");
    }
    if run.status == "sealed" {
        transaction.commit()?;
        return load_run(config, run_id);
    }
    if run.status != "active" {
        bail!("state run cannot be sealed from status {}", run.status);
    }
    let updated_at = now()?;
    let changed = transaction.execute(
        "UPDATE state_runs SET status = 'sealed', updated_at = ?2 WHERE run_id = ?1 AND status = 'active'",
        params![run_id, updated_at],
    )?;
    if changed != 1 {
        bail!("state run status changed while sealing");
    }
    let event = append_event_tx(
        &transaction,
        EventDraft {
            run_id,
            subject,
            epoch: run.epoch,
            kind: "state.run.sealed",
            actor,
            authority: StateAuthority::Operator,
            payload: json!({"reason": reason}),
        },
    )?;
    transaction.commit()?;
    audit::append(
        config,
        actor,
        "agent-state.run.seal",
        &format!("agent-state/{run_id}"),
        "passed",
        json!({"subject": subject, "reason": reason, "event_id": event.event_id}),
    )?;
    load_run(config, run_id)
}
