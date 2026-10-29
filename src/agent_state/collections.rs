//! Collections, mutations, events and artifacts.
use super::*;

#[allow(clippy::too_many_arguments)]
pub fn seed_collection(
    config: &RuntimeConfig,
    run_id: &str,
    subject: &str,
    name: &str,
    schema_name: &str,
    value: Value,
    authorities: &[StateAuthority],
    access: &[CollectionAccess],
    prompt_eligible: bool,
) -> Result<StateCollection> {
    validate_identifier(run_id, "run id")?;
    validate_identifier(subject, "subject")?;
    validate_identifier(name, "collection")?;
    validate_identifier(schema_name, "schema")?;
    if authorities.is_empty() || access.is_empty() {
        bail!("state collection needs at least one authority and access rule");
    }
    let schema = load_schema(config, schema_name)?;
    validate_document(&schema, &value)?;
    let encoded = serde_json::to_vec(&value)?;
    if encoded.len() > MAX_STATE_BYTES {
        bail!("state collection exceeds the maximum encoded size");
    }
    let digest = hash_bytes(&encoded);
    let updated_at = now()?;
    let mut connection = open_database(config)?;
    let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let run = load_run_tx(&transaction, run_id)?;
    if run.subject != subject {
        bail!("state subject does not match the run owner");
    }
    ensure_run_active(&run)?;
    transaction.execute(
        "INSERT INTO state_collections (run_id, subject, name, document_schema, version, authorities_json, access_json, prompt_eligible, content_sha256, value_json, updated_at) VALUES (?1, ?2, ?3, ?4, 0, ?5, ?6, ?7, ?8, ?9, ?10)",
        params![run_id, subject, name, schema_name, serde_json::to_string(authorities)?, serde_json::to_string(access)?, prompt_eligible, digest, String::from_utf8(encoded)?, updated_at],
    ).with_context(|| format!("state collection already exists: {name}"))?;
    let event = append_event_tx(
        &transaction,
        EventDraft {
            run_id,
            subject,
            epoch: run.epoch,
            kind: "state.collection.seeded",
            actor: "host-controller",
            authority: StateAuthority::Operator,
            payload: json!({"collection": name, "schema": schema_name, "content_sha256": digest}),
        },
    )?;
    transaction.execute(
        "UPDATE state_runs SET updated_at = ?2 WHERE run_id = ?1",
        params![run_id, updated_at],
    )?;
    transaction.commit()?;
    audit::append(
        config,
        "host-controller",
        "agent-state.collection.seed",
        &format!("agent-state/{run_id}/{subject}/{name}"),
        "passed",
        json!({"schema": schema_name, "content_sha256": digest, "event_id": event.event_id}),
    )?;
    load_collection(config, run_id, subject, name)
}

#[allow(clippy::too_many_arguments)]
pub fn seed_collection_file(
    config: &RuntimeConfig,
    run_id: &str,
    subject: &str,
    name: &str,
    schema_name: &str,
    value_file: &Path,
    authorities: &[StateAuthority],
    access: &[CollectionAccess],
    prompt_eligible: bool,
) -> Result<StateCollection> {
    let value: Value =
        serde_json::from_slice(&read_bounded_regular(value_file, MAX_STATE_BYTES as u64)?)?;
    seed_collection(
        config,
        run_id,
        subject,
        name,
        schema_name,
        value,
        authorities,
        access,
        prompt_eligible,
    )
}

pub fn apply_mutation(
    config: &RuntimeConfig,
    mutation: &StateMutation,
    actor: &str,
    authority: StateAuthority,
) -> Result<StateMutationReceipt> {
    validate_mutation(mutation)?;
    validate_text(actor, "actor", 256)?;
    let encoded = serde_json::to_vec(mutation)?;
    if encoded.len() > MAX_MUTATION_BYTES {
        bail!("state mutation exceeds the maximum encoded size");
    }
    let mut connection = open_database(config)?;
    let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let run = load_run_tx(&transaction, &mutation.run_id)?;
    if run.subject != mutation.subject {
        bail!("state subject does not match the run owner");
    }
    ensure_run_active(&run)?;
    let mut collection = load_collection_tx(
        &transaction,
        &mutation.run_id,
        &mutation.subject,
        &mutation.collection,
    )?;
    if collection.version != mutation.base_version {
        bail!(
            "state version conflict: collection is {}, mutation is based on {}",
            collection.version,
            mutation.base_version
        );
    }
    if !collection.authorities.contains(&authority) {
        bail!("{authority:?} is not allowed to update this collection");
    }
    if authority == StateAuthority::Model
        && (mutation.set.contains_key("") || mutation.remove.iter().any(String::is_empty))
    {
        bail!("model state mutations cannot replace or remove a collection root");
    }
    if authority == StateAuthority::Model {
        validate_model_pointers(
            &mutation.collection,
            mutation.set.keys().chain(&mutation.remove),
        )?;
    }
    for (pointer, value) in &mutation.set {
        set_pointer(&mut collection.value, pointer, value.clone())?;
    }
    for pointer in &mutation.remove {
        remove_pointer(&mut collection.value, pointer)?;
    }
    let schema = load_schema(config, &collection.document_schema)?;
    validate_document(&schema, &collection.value)?;
    let value_json = serde_json::to_string(&collection.value)?;
    if value_json.len() > MAX_STATE_BYTES {
        bail!("state collection exceeds the maximum encoded size");
    }
    let content_sha256 = hash_bytes(value_json.as_bytes());
    let updated_at = now()?;
    let next_version = collection.version + 1;
    let changed = transaction.execute(
        "UPDATE state_collections SET version = ?5, content_sha256 = ?6, value_json = ?7, updated_at = ?8 WHERE run_id = ?1 AND subject = ?2 AND name = ?3 AND version = ?4",
        params![mutation.run_id, mutation.subject, mutation.collection, mutation.base_version, next_version, content_sha256, value_json, updated_at],
    )?;
    if changed != 1 {
        bail!("state version changed during mutation");
    }
    let event = append_event_tx(
        &transaction,
        EventDraft {
            run_id: &mutation.run_id,
            subject: &mutation.subject,
            epoch: run.epoch,
            kind: "state.collection.mutated",
            actor,
            authority,
            payload: json!({
                "collection": mutation.collection,
                "previous_version": mutation.base_version,
                "version": next_version,
                "set": mutation.set,
                "remove": mutation.remove,
                "rationale": mutation.rationale,
                "content_sha256": content_sha256
            }),
        },
    )?;
    transaction.execute(
        "UPDATE state_runs SET updated_at = ?2 WHERE run_id = ?1",
        params![mutation.run_id, updated_at],
    )?;
    transaction.commit()?;
    audit::append(
        config,
        actor,
        "agent-state.collection.mutate",
        &format!(
            "agent-state/{}/{}/{}",
            mutation.run_id, mutation.subject, mutation.collection
        ),
        "passed",
        json!({
            "authority": authority,
            "previous_version": mutation.base_version,
            "version": next_version,
            "event_id": event.event_id,
            "content_sha256": content_sha256
        }),
    )?;
    Ok(StateMutationReceipt {
        run_id: mutation.run_id.clone(),
        subject: mutation.subject.clone(),
        collection: mutation.collection.clone(),
        previous_version: mutation.base_version,
        version: next_version,
        event_id: event.event_id,
        content_sha256,
    })
}

pub fn apply_mutation_file(
    config: &RuntimeConfig,
    path: &Path,
    actor: &str,
    authority: StateAuthority,
) -> Result<StateMutationReceipt> {
    let bytes = read_bounded_regular(path, MAX_MUTATION_BYTES as u64)?;
    let mutation: StateMutation = serde_json::from_slice(&bytes)
        .with_context(|| format!("invalid state mutation {}", path.display()))?;
    apply_mutation(config, &mutation, actor, authority)
}

pub fn append_event(
    config: &RuntimeConfig,
    run_id: &str,
    subject: &str,
    kind: &str,
    actor: &str,
    authority: StateAuthority,
    payload: Value,
) -> Result<StateEvent> {
    validate_identifier(run_id, "run id")?;
    validate_identifier(subject, "subject")?;
    validate_event_kind(kind)?;
    validate_text(actor, "actor", 256)?;
    let mut connection = open_database(config)?;
    let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let run = load_run_tx(&transaction, run_id)?;
    if run.subject != subject {
        bail!("state subject does not match the run owner");
    }
    ensure_run_active(&run)?;
    let event = append_event_tx(
        &transaction,
        EventDraft {
            run_id,
            subject,
            epoch: run.epoch,
            kind,
            actor,
            authority,
            payload,
        },
    )?;
    transaction.execute(
        "UPDATE state_runs SET updated_at = ?2 WHERE run_id = ?1",
        params![run_id, event.occurred_at],
    )?;
    transaction.commit()?;
    Ok(event)
}

pub fn append_event_file(
    config: &RuntimeConfig,
    run_id: &str,
    subject: &str,
    kind: &str,
    actor: &str,
    authority: StateAuthority,
    payload_file: &Path,
) -> Result<StateEvent> {
    let payload: Value =
        serde_json::from_slice(&read_bounded_regular(payload_file, MAX_EVENT_BYTES as u64)?)?;
    append_event(config, run_id, subject, kind, actor, authority, payload)
}

#[allow(clippy::too_many_arguments)]
pub fn add_artifact(
    config: &RuntimeConfig,
    run_id: &str,
    subject: &str,
    source: &Path,
    media_type: &str,
    role: &str,
    label: &str,
    sensitivity: ArtifactSensitivity,
    actor: &str,
) -> Result<ArtifactRecord> {
    validate_identifier(run_id, "run id")?;
    validate_identifier(subject, "subject")?;
    validate_text(media_type, "media type", 256)?;
    validate_text(role, "artifact role", 128)?;
    validate_text(label, "artifact label", 1024)?;
    validate_text(actor, "actor", 256)?;
    if !source.is_absolute() {
        bail!("artifact source must be an absolute path");
    }
    let metadata = fs::symlink_metadata(source)
        .with_context(|| format!("failed to inspect artifact {}", source.display()))?;
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        bail!("artifact source must be a regular file, not a symlink");
    }
    if metadata.len() > MAX_ARTIFACT_BYTES {
        bail!("artifact exceeds the local content-store limit");
    }
    let run_check = load_run(config, run_id)?;
    if run_check.subject != subject {
        bail!("state subject does not match the run owner");
    }
    ensure_run_active(&run_check)?;
    let (sha256, size) = hash_file(source)?;
    let relative_path = format!("sha256/{}/{}", &sha256[..2], sha256);
    let destination = config.agent_state_artifacts_dir().join(&relative_path);
    if let Some(parent) = destination.parent() {
        create_private_dir(parent)?;
    }
    if destination.exists() {
        let (existing, existing_size) = hash_file(&destination)?;
        if existing != sha256 || existing_size != size {
            bail!("content-addressed artifact path does not match its digest");
        }
    } else {
        copy_private_atomic(source, &destination, &sha256)?;
    }
    let created_at = now()?;
    let artifact = ArtifactRecord {
        schema_version: 1,
        sha256: sha256.clone(),
        size,
        media_type: media_type.into(),
        role: role.into(),
        label: label.into(),
        sensitivity,
        relative_path: relative_path.clone(),
        created_at: created_at.clone(),
    };
    let mut connection = open_database(config)?;
    let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let run = load_run_tx(&transaction, run_id)?;
    if run.subject != subject {
        bail!("state subject does not match the run owner");
    }
    ensure_run_active(&run)?;
    let existing: Option<(u64, String)> = transaction
        .query_row(
            "SELECT size, relative_path FROM artifacts WHERE sha256 = ?1",
            params![sha256],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()?;
    if let Some((existing_size, existing_path)) = existing {
        if existing_size != size || existing_path != relative_path {
            bail!("artifact database record conflicts with content-addressed bytes");
        }
    } else {
        transaction.execute(
            "INSERT INTO artifacts (sha256, size, media_type, sensitivity, relative_path, created_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![sha256, size, media_type, sensitivity.as_str(), relative_path, created_at],
        )?;
    }
    let event = append_event_tx(
        &transaction,
        EventDraft {
            run_id,
            subject,
            epoch: run.epoch,
            kind: "artifact.recorded",
            actor,
            authority: StateAuthority::Machine,
            payload: json!({
                "sha256": sha256,
                "size": size,
                "media_type": media_type,
                "role": role,
                "label": label,
                "sensitivity": sensitivity
            }),
        },
    )?;
    transaction.execute(
        "INSERT INTO artifact_links (event_id, sha256, media_type, role, label, sensitivity) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        params![event.event_id, sha256, media_type, role, label, sensitivity.as_str()],
    )?;
    transaction.execute(
        "UPDATE state_runs SET updated_at = ?2 WHERE run_id = ?1",
        params![run_id, created_at],
    )?;
    transaction.commit()?;
    audit::append(
        config,
        actor,
        "agent-state.artifact.add",
        &format!("agent-state/{run_id}/artifact/{sha256}"),
        "passed",
        json!({"size": size, "role": role, "sensitivity": sensitivity, "event_id": event.event_id}),
    )?;
    Ok(artifact)
}
