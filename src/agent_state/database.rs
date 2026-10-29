//! The state database: opening it, event rows and digests.
use super::*;

pub(super) fn open_database(config: &RuntimeConfig) -> Result<Connection> {
    create_private_dir(&config.agent_state_dir())?;
    create_private_dir(&config.agent_state_artifacts_dir())?;
    create_private_dir(&config.state_profiles_dir())?;
    create_private_dir(&config.state_schemas_dir())?;
    let path = config.agent_state_database_path();
    if !path.exists() {
        let mut options = fs::OpenOptions::new();
        options.create_new(true).write(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        options.open(&path)?;
    }
    set_mode(&path, 0o600)?;
    let connection = Connection::open(&path)?;
    connection.busy_timeout(std::time::Duration::from_secs(10))?;
    connection.execute_batch(
        "PRAGMA journal_mode = WAL;
         PRAGMA synchronous = FULL;
         PRAGMA foreign_keys = ON;
         PRAGMA trusted_schema = OFF;
         CREATE TABLE IF NOT EXISTS state_runs (
           run_id TEXT PRIMARY KEY,
           domain TEXT NOT NULL,
           profile TEXT NOT NULL,
           subject TEXT NOT NULL,
           team TEXT,
           status TEXT NOT NULL,
           epoch INTEGER NOT NULL,
           created_at TEXT NOT NULL,
           updated_at TEXT NOT NULL
         );
         CREATE TABLE IF NOT EXISTS state_collections (
           run_id TEXT NOT NULL REFERENCES state_runs(run_id),
           subject TEXT NOT NULL,
           name TEXT NOT NULL,
           document_schema TEXT NOT NULL,
           version INTEGER NOT NULL,
           authorities_json TEXT NOT NULL,
           access_json TEXT NOT NULL,
           prompt_eligible INTEGER NOT NULL,
           content_sha256 TEXT NOT NULL,
           value_json TEXT NOT NULL,
           updated_at TEXT NOT NULL,
           PRIMARY KEY (run_id, subject, name)
         );
         CREATE TABLE IF NOT EXISTS state_events (
           sequence INTEGER PRIMARY KEY AUTOINCREMENT,
           event_id TEXT NOT NULL UNIQUE,
           run_id TEXT NOT NULL REFERENCES state_runs(run_id),
           subject TEXT NOT NULL,
           epoch INTEGER NOT NULL,
           kind TEXT NOT NULL,
           actor TEXT NOT NULL,
           authority TEXT NOT NULL,
           occurred_at TEXT NOT NULL,
           payload_json TEXT NOT NULL,
           payload_sha256 TEXT NOT NULL,
           previous_hash TEXT NOT NULL,
           hash TEXT NOT NULL UNIQUE
         );
         CREATE INDEX IF NOT EXISTS state_events_run_sequence ON state_events(run_id, sequence);
         CREATE TABLE IF NOT EXISTS artifacts (
           sha256 TEXT PRIMARY KEY,
           size INTEGER NOT NULL,
           media_type TEXT NOT NULL,
           sensitivity TEXT NOT NULL,
           relative_path TEXT NOT NULL UNIQUE,
           created_at TEXT NOT NULL
         );
         CREATE TABLE IF NOT EXISTS artifact_links (
           event_id TEXT NOT NULL REFERENCES state_events(event_id),
           sha256 TEXT NOT NULL REFERENCES artifacts(sha256),
           media_type TEXT NOT NULL,
           role TEXT NOT NULL,
           label TEXT NOT NULL,
           sensitivity TEXT NOT NULL,
           PRIMARY KEY (event_id, sha256, role)
         );
         CREATE TABLE IF NOT EXISTS state_checkpoints (
           run_id TEXT NOT NULL REFERENCES state_runs(run_id),
           subject TEXT NOT NULL,
           epoch INTEGER NOT NULL,
           state_sha256 TEXT NOT NULL,
           event_head TEXT NOT NULL,
           reason TEXT NOT NULL,
           created_at TEXT NOT NULL,
           PRIMARY KEY (run_id, subject, epoch)
         );
         CREATE TRIGGER IF NOT EXISTS state_events_no_update BEFORE UPDATE ON state_events
           BEGIN SELECT RAISE(ABORT, 'state events are append-only'); END;
         CREATE TRIGGER IF NOT EXISTS state_events_no_delete BEFORE DELETE ON state_events
           BEGIN SELECT RAISE(ABORT, 'state events are append-only'); END;
         CREATE TRIGGER IF NOT EXISTS artifacts_no_update BEFORE UPDATE ON artifacts
           BEGIN SELECT RAISE(ABORT, 'artifacts are immutable'); END;
         CREATE TRIGGER IF NOT EXISTS artifacts_no_delete BEFORE DELETE ON artifacts
           BEGIN SELECT RAISE(ABORT, 'artifacts are immutable'); END;",
    )?;
    for suffix in ["-wal", "-shm"] {
        let companion = PathBuf::from(format!("{}{}", path.display(), suffix));
        if companion.exists() {
            set_mode(&companion, 0o600)?;
        }
    }
    Ok(connection)
}

pub(super) fn append_event_tx(
    transaction: &Transaction<'_>,
    draft: EventDraft<'_>,
) -> Result<StateEvent> {
    validate_event_kind(draft.kind)?;
    let payload_json = serde_json::to_string(&draft.payload)?;
    if payload_json.len() > MAX_EVENT_BYTES {
        bail!("state event payload exceeds the maximum encoded size");
    }
    let payload_sha256 = hash_bytes(payload_json.as_bytes());
    let previous_hash = event_head_tx(transaction, draft.run_id)?;
    let event_id = Uuid::new_v4().to_string();
    let occurred_at = now()?;
    let mut event = StateEvent {
        sequence: 0,
        event_id,
        run_id: draft.run_id.into(),
        subject: draft.subject.into(),
        epoch: draft.epoch,
        kind: draft.kind.into(),
        actor: draft.actor.into(),
        authority: draft.authority,
        occurred_at,
        payload: draft.payload,
        payload_sha256,
        previous_hash,
        hash: String::new(),
    };
    event.hash = event_digest(&event);
    transaction.execute(
        "INSERT INTO state_events (event_id, run_id, subject, epoch, kind, actor, authority, occurred_at, payload_json, payload_sha256, previous_hash, hash) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)",
        params![event.event_id, event.run_id, event.subject, event.epoch, event.kind, event.actor, event.authority.as_str(), event.occurred_at, payload_json, event.payload_sha256, event.previous_hash, event.hash],
    )?;
    event.sequence = transaction.last_insert_rowid() as u64;
    Ok(event)
}

pub(super) fn load_run_connection(connection: &Connection, run_id: &str) -> Result<StateRun> {
    connection
        .query_row(
            "SELECT run_id, domain, profile, subject, team, status, epoch, created_at, updated_at FROM state_runs WHERE run_id = ?1",
            params![run_id],
            |row| {
                Ok((
                    row.get::<_, String>(0)?, row.get::<_, String>(1)?, row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?, row.get::<_, Option<String>>(4)?, row.get::<_, String>(5)?,
                    row.get::<_, u64>(6)?, row.get::<_, String>(7)?, row.get::<_, String>(8)?,
                ))
            },
        )
        .optional()?
        .map(|(run_id, domain, profile, subject, team, status, epoch, created_at, updated_at)| -> Result<StateRun> {
            Ok(StateRun { schema_version: 1, run_id, domain: parse_domain(&domain)?, profile, subject, team, status, epoch, created_at, updated_at })
        })
        .transpose()?
        .with_context(|| format!("unknown state run {run_id}"))
}

pub(super) fn load_run_tx(transaction: &Transaction<'_>, run_id: &str) -> Result<StateRun> {
    load_run_connection(transaction, run_id)
}

pub(super) fn ensure_run_active(run: &StateRun) -> Result<()> {
    if run.status != "active" {
        bail!("state run {} is {}", run.run_id, run.status);
    }
    Ok(())
}

pub(super) fn load_collection_connection(
    connection: &Connection,
    run_id: &str,
    subject: &str,
    name: &str,
) -> Result<StateCollection> {
    connection
        .query_row(
            "SELECT document_schema, version, authorities_json, access_json, prompt_eligible, content_sha256, value_json, updated_at FROM state_collections WHERE run_id = ?1 AND subject = ?2 AND name = ?3",
            params![run_id, subject, name],
            |row| {
                Ok((
                    row.get::<_, String>(0)?, row.get::<_, u64>(1)?, row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?, row.get::<_, bool>(4)?, row.get::<_, String>(5)?,
                    row.get::<_, String>(6)?, row.get::<_, String>(7)?,
                ))
            },
        )
        .optional()?
        .map(|(document_schema, version, authorities_json, access_json, prompt_eligible, content_sha256, value_json, updated_at)| -> Result<StateCollection> {
            Ok(StateCollection {
                schema_version: 1,
                run_id: run_id.into(),
                subject: subject.into(),
                name: name.into(),
                document_schema,
                version,
                authorities: serde_json::from_str(&authorities_json)?,
                access: serde_json::from_str(&access_json)?,
                prompt_eligible,
                content_sha256,
                value: serde_json::from_str(&value_json)?,
                updated_at,
            })
        })
        .transpose()?
        .with_context(|| format!("unknown state collection {run_id}/{subject}/{name}"))
}

pub(super) fn load_collection_tx(
    transaction: &Transaction<'_>,
    run_id: &str,
    subject: &str,
    name: &str,
) -> Result<StateCollection> {
    load_collection_connection(transaction, run_id, subject, name)
}

pub(super) fn latest_event_metadata(
    connection: &Connection,
    run_id: &str,
    subject: &str,
    count: usize,
) -> Result<Vec<StateViewEvent>> {
    let mut statement = connection.prepare(
        "SELECT sequence, event_id, epoch, kind, occurred_at, hash FROM state_events WHERE run_id = ?1 AND subject = ?2 ORDER BY sequence DESC LIMIT ?3",
    )?;
    let rows = statement.query_map(params![run_id, subject, count as u64], |row| {
        Ok(StateViewEvent {
            sequence: row.get(0)?,
            event_id: row.get(1)?,
            epoch: row.get(2)?,
            kind: row.get(3)?,
            occurred_at: row.get(4)?,
            hash: row.get(5)?,
        })
    })?;
    let mut events = rows.collect::<std::result::Result<Vec<_>, _>>()?;
    events.reverse();
    Ok(events)
}

pub(super) fn state_event_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<StateEvent> {
    let authority: String = row.get(7)?;
    let payload: String = row.get(9)?;
    Ok(StateEvent {
        sequence: row.get(0)?,
        event_id: row.get(1)?,
        run_id: row.get(2)?,
        subject: row.get(3)?,
        epoch: row.get(4)?,
        kind: row.get(5)?,
        actor: row.get(6)?,
        authority: parse_authority(&authority).map_err(|_| rusqlite::Error::InvalidQuery)?,
        occurred_at: row.get(8)?,
        payload: serde_json::from_str(&payload).map_err(|error| {
            rusqlite::Error::FromSqlConversionFailure(
                payload.len(),
                rusqlite::types::Type::Text,
                Box::new(error),
            )
        })?,
        payload_sha256: row.get(10)?,
        previous_hash: row.get(11)?,
        hash: row.get(12)?,
    })
}

pub(super) fn event_head_tx(transaction: &Transaction<'_>, run_id: &str) -> Result<String> {
    Ok(transaction
        .query_row(
            "SELECT hash FROM state_events WHERE run_id = ?1 ORDER BY sequence DESC LIMIT 1",
            params![run_id],
            |row| row.get(0),
        )
        .optional()?
        .unwrap_or_default())
}

pub(super) fn collection_set_digest(
    transaction: &Transaction<'_>,
    run_id: &str,
    subject: &str,
) -> Result<String> {
    let mut statement = transaction.prepare(
        "SELECT name, version, content_sha256 FROM state_collections WHERE run_id = ?1 AND subject = ?2 ORDER BY name",
    )?;
    let rows = statement.query_map(params![run_id, subject], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, u64>(1)?,
            row.get::<_, String>(2)?,
        ))
    })?;
    let mut hasher = Sha256::new();
    for row in rows {
        let (name, version, digest) = row?;
        hasher.update(name.as_bytes());
        hasher.update([0]);
        hasher.update(version.to_be_bytes());
        hasher.update(digest.as_bytes());
        hasher.update([0xff]);
    }
    Ok(hex::encode(hasher.finalize()))
}

pub(super) fn event_digest(event: &StateEvent) -> String {
    let mut hasher = Sha256::new();
    for value in [
        event.event_id.as_str(),
        event.run_id.as_str(),
        event.subject.as_str(),
        event.kind.as_str(),
        event.actor.as_str(),
        event.authority.as_str(),
        event.occurred_at.as_str(),
        event.payload_sha256.as_str(),
        event.previous_hash.as_str(),
    ] {
        hasher.update(value.as_bytes());
        hasher.update([0]);
    }
    hasher.update(event.epoch.to_be_bytes());
    hex::encode(hasher.finalize())
}
