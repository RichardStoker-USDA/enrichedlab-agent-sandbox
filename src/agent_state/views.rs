//! Views, the outbox, context status and checkpoints.
use super::*;

pub fn generate_view(config: &RuntimeConfig, run_id: &str, subject: &str) -> Result<StateView> {
    let run = load_run(config, run_id)?;
    if run.subject != subject {
        bail!("state subject does not match the run owner");
    }
    let profile = load_profile(config, &run.profile)?;
    let connection = open_database(config)?;
    let mut rules = profile.collections.clone();
    rules.sort_by_key(|rule| rule.priority);
    let mut collections = BTreeMap::new();
    let mut omitted = Vec::new();
    let mut encoded_bytes = 0usize;
    let mut accepted_order = Vec::new();
    for rule in rules {
        let collection = match load_collection_connection(&connection, run_id, subject, &rule.name)
        {
            Ok(collection) => collection,
            Err(_) => {
                omitted.push(StateViewOmission {
                    collection: rule.name,
                    reason: "not-present".into(),
                });
                continue;
            }
        };
        if !collection.prompt_eligible || !allows(&collection, profile.audience, subject) {
            omitted.push(StateViewOmission {
                collection: rule.name,
                reason: "not-authorized-for-profile".into(),
            });
            continue;
        }
        let (selected, truncated_pointers) =
            select_pointers_bounded(&collection.value, &rule.pointers, rule.max_bytes)?;
        let selected_bytes = serde_json::to_vec(&selected)?.len();
        if selected_bytes > rule.max_bytes
            || (selected == json!({}) && !truncated_pointers.is_empty())
        {
            omitted.push(StateViewOmission {
                collection: rule.name,
                reason: "all selected pointers exceeded the profile byte cap".into(),
            });
            continue;
        }
        for pointer in truncated_pointers {
            omitted.push(StateViewOmission {
                collection: rule.name.clone(),
                reason: format!("pointer was bounded by the profile byte cap: {pointer}"),
            });
        }
        let candidate_total = encoded_bytes.saturating_add(selected_bytes);
        if estimate_tokens(candidate_total) > profile.max_state_tokens {
            omitted.push(StateViewOmission {
                collection: rule.name,
                reason: "profile token cap reached".into(),
            });
            continue;
        }
        encoded_bytes = candidate_total;
        accepted_order.push(rule.name.clone());
        collections.insert(
            rule.name,
            StateViewCollection {
                version: collection.version,
                content_sha256: collection.content_sha256,
                value: selected,
            },
        );
    }
    let latest_events =
        latest_event_metadata(&connection, run_id, subject, profile.latest_event_count)?;
    let mut view = StateView {
        schema_version: 1,
        run_id: run_id.into(),
        subject: subject.into(),
        domain: run.domain,
        profile: profile.name,
        audience: profile.audience,
        epoch: run.epoch,
        reasoning_retention: profile.reasoning_retention,
        estimated_tokens: 0,
        max_state_tokens: profile.max_state_tokens,
        collections,
        latest_events,
        omitted,
        generated_at: now()?,
    };
    view.estimated_tokens = estimate_tokens(serde_json::to_vec(&view)?.len());
    while view.estimated_tokens > view.max_state_tokens {
        let Some(name) = accepted_order.pop() else {
            break;
        };
        view.collections.remove(&name);
        view.omitted.push(StateViewOmission {
            collection: name,
            reason: "removed to keep the complete view under its token cap".into(),
        });
        view.estimated_tokens = estimate_tokens(serde_json::to_vec(&view)?.len());
    }
    while view.estimated_tokens > view.max_state_tokens && !view.latest_events.is_empty() {
        view.latest_events.remove(0);
        view.estimated_tokens = estimate_tokens(serde_json::to_vec(&view)?.len());
    }
    if view.estimated_tokens > view.max_state_tokens {
        bail!("state view metadata alone exceeds the profile token cap");
    }
    Ok(view)
}

pub fn materialize_view(
    config: &RuntimeConfig,
    run_id: &str,
    subject: &str,
    workspace: &Path,
) -> Result<PathBuf> {
    let state_root = workspace.join(".agent-state");
    create_private_dir(&state_root)?;
    for name in ["outbox", "accepted", "rejected"] {
        create_private_dir(&state_root.join(name))?;
    }
    let view = generate_view(config, run_id, subject)?;
    let path = state_root.join("view.json");
    write_private(&path, &serde_json::to_vec_pretty(&view)?)?;
    Ok(path)
}

pub fn reconcile_outbox(
    config: &RuntimeConfig,
    run_id: &str,
    subject: &str,
    workspace: &Path,
) -> Result<ReconcileOutcome> {
    let state_root = workspace.join(".agent-state");
    let outbox = state_root.join("outbox");
    let accepted_root = state_root.join("accepted");
    let rejected_root = state_root.join("rejected");
    for directory in [&outbox, &accepted_root, &rejected_root] {
        create_private_dir(directory)?;
    }
    let mut paths = fs::read_dir(&outbox)?
        .filter_map(|entry| entry.ok().map(|entry| entry.path()))
        .filter(|path| path.extension().and_then(|value| value.to_str()) == Some("json"))
        .collect::<Vec<_>>();
    paths.sort();
    let mut accepted = 0usize;
    let mut rejected = 0usize;
    for path in paths {
        let file_name = path
            .file_name()
            .and_then(|name| name.to_str())
            .context("invalid state mutation filename")?;
        validate_outbox_name(file_name)?;
        let result = apply_mutation_file(config, &path, subject, StateAuthority::Model);
        match result {
            Ok(receipt) => {
                let destination = accepted_root.join(file_name);
                fs::rename(&path, &destination)?;
                write_private(
                    &accepted_root.join(format!("{file_name}.receipt.json")),
                    &serde_json::to_vec_pretty(&receipt)?,
                )?;
                accepted += 1;
            }
            Err(error) => {
                let destination = rejected_root.join(file_name);
                fs::rename(&path, &destination)?;
                write_private(
                    &rejected_root.join(format!("{file_name}.error.json")),
                    &serde_json::to_vec_pretty(&json!({
                        "schema_version": 1,
                        "accepted": false,
                        "error": bounded_workspace_error(config, workspace, &error)
                    }))?,
                )?;
                rejected += 1;
            }
        }
    }
    let view_path = materialize_view(config, run_id, subject, workspace)?;
    Ok(ReconcileOutcome {
        run_id: run_id.into(),
        subject: subject.into(),
        accepted,
        rejected,
        view_path,
    })
}

pub fn context_status(
    config: &RuntimeConfig,
    profile_name: &str,
    window_tokens: u64,
    used_tokens: u64,
    output_reserve_tokens: u64,
) -> Result<ContextStatus> {
    if window_tokens == 0
        || used_tokens > window_tokens
        || output_reserve_tokens > window_tokens
        || used_tokens.saturating_add(output_reserve_tokens) > window_tokens
    {
        bail!("context token values are outside the declared window");
    }
    let profile = load_profile(config, profile_name)?;
    let pressure = used_tokens.saturating_add(output_reserve_tokens) as f64 / window_tokens as f64;
    let action = if pressure >= profile.hard_reset_ratio {
        ContextAction::Reset
    } else if pressure >= profile.soft_reset_ratio {
        ContextAction::Checkpoint
    } else {
        ContextAction::Continue
    };
    Ok(ContextStatus {
        window_tokens,
        used_tokens,
        output_reserve_tokens,
        pressure_ratio: pressure,
        soft_reset_ratio: profile.soft_reset_ratio,
        hard_reset_ratio: profile.hard_reset_ratio,
        action,
    })
}

pub fn checkpoint(
    config: &RuntimeConfig,
    run_id: &str,
    subject: &str,
    reason: &str,
    actor: &str,
) -> Result<StateCheckpoint> {
    validate_text(reason, "checkpoint reason", 4096)?;
    validate_text(actor, "actor", 256)?;
    let mut connection = open_database(config)?;
    let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let run = load_run_tx(&transaction, run_id)?;
    if run.subject != subject {
        bail!("state subject does not match the run owner");
    }
    ensure_run_active(&run)?;
    let state_sha256 = collection_set_digest(&transaction, run_id, subject)?;
    let event_head = event_head_tx(&transaction, run_id)?;
    let previous_epoch = run.epoch;
    let epoch = previous_epoch + 1;
    let created_at = now()?;
    transaction.execute(
        "UPDATE state_runs SET epoch = ?2, updated_at = ?3 WHERE run_id = ?1 AND epoch = ?4",
        params![run_id, epoch, created_at, previous_epoch],
    )?;
    let event = append_event_tx(
        &transaction,
        EventDraft {
            run_id,
            subject,
            epoch,
            kind: "state.epoch.started",
            actor,
            authority: StateAuthority::Operator,
            payload: json!({
                "previous_epoch": previous_epoch,
                "epoch": epoch,
                "reason": reason,
                "state_sha256": state_sha256,
                "previous_event_head": event_head
            }),
        },
    )?;
    transaction.execute(
        "INSERT INTO state_checkpoints (run_id, subject, epoch, state_sha256, event_head, reason, created_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        params![run_id, subject, epoch, state_sha256, event.hash, reason, created_at],
    )?;
    transaction.commit()?;
    Ok(StateCheckpoint {
        run_id: run_id.into(),
        subject: subject.into(),
        previous_epoch,
        epoch,
        state_sha256,
        event_head: event.hash,
        event_id: event.event_id,
        reason: reason.into(),
        created_at,
    })
}
