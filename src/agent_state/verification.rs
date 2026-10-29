//! Verifying a run's records against its digests.
use super::*;

pub fn verify(config: &RuntimeConfig) -> Result<StateVerification> {
    let connection = open_database(config)?;
    let runs: u64 =
        connection.query_row("SELECT COUNT(*) FROM state_runs", [], |row| row.get(0))?;
    let collections: u64 =
        connection.query_row("SELECT COUNT(*) FROM state_collections", [], |row| {
            row.get(0)
        })?;
    let events: u64 =
        connection.query_row("SELECT COUNT(*) FROM state_events", [], |row| row.get(0))?;
    let artifacts: u64 =
        connection.query_row("SELECT COUNT(*) FROM artifacts", [], |row| row.get(0))?;
    for run in list_summaries(config)? {
        let mut previous = String::new();
        for event in trace(config, &run.run_id)? {
            if event.previous_hash != previous
                || event.payload_sha256 != hash_value(&event.payload)?
            {
                bail!("state event chain failed at {}", event.event_id);
            }
            let expected = event_digest(&event);
            if event.hash != expected {
                bail!("state event digest failed at {}", event.event_id);
            }
            previous = event.hash;
        }
    }
    let mut collection_statement = connection.prepare(
        "SELECT run_id, subject, name FROM state_collections ORDER BY run_id, subject, name",
    )?;
    let collection_rows = collection_statement.query_map([], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, String>(2)?,
        ))
    })?;
    for row in collection_rows {
        let (run_id, subject, name) = row?;
        let collection = load_collection_connection(&connection, &run_id, &subject, &name)?;
        let encoded = serde_json::to_vec(&collection.value)?;
        if collection.content_sha256 != hash_bytes(&encoded) {
            bail!("state collection digest failed: {run_id}/{subject}/{name}");
        }
        validate_document(
            &load_schema(config, &collection.document_schema)?,
            &collection.value,
        )?;
    }
    let mut artifact_statement =
        connection.prepare("SELECT sha256, size, relative_path FROM artifacts ORDER BY sha256")?;
    let artifact_rows = artifact_statement.query_map([], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, u64>(1)?,
            row.get::<_, String>(2)?,
        ))
    })?;
    for row in artifact_rows {
        let (sha256, size, relative_path) = row?;
        let path = config.agent_state_artifacts_dir().join(&relative_path);
        let (observed, observed_size) = hash_file(&path)?;
        if observed != sha256 || observed_size != size {
            bail!("state artifact digest failed: {sha256}");
        }
    }
    Ok(StateVerification {
        database: config.agent_state_database_path(),
        runs,
        collections,
        events,
        artifacts,
        event_chains_valid: true,
        schemas_valid: true,
        artifacts_valid: true,
    })
}
