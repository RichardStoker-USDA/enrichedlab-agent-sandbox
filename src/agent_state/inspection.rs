//! Reading runs and collections back: summaries, metadata search and traces.
use super::*;

pub fn load_run(config: &RuntimeConfig, run_id: &str) -> Result<StateRun> {
    validate_identifier(run_id, "run id")?;
    let connection = open_database(config)?;
    load_run_connection(&connection, run_id)
}

pub fn load_collection(
    config: &RuntimeConfig,
    run_id: &str,
    subject: &str,
    name: &str,
) -> Result<StateCollection> {
    let connection = open_database(config)?;
    load_collection_connection(&connection, run_id, subject, name)
}

pub fn list_summaries(config: &RuntimeConfig) -> Result<Vec<StateRunSummary>> {
    let connection = open_database(config)?;
    let mut statement = connection.prepare(
        "SELECT r.run_id, r.domain, r.profile, r.subject, r.team, r.status, r.epoch, r.updated_at,
                (SELECT COUNT(*) FROM state_collections c WHERE c.run_id = r.run_id),
                (SELECT COUNT(*) FROM state_events e WHERE e.run_id = r.run_id),
                (SELECT COUNT(DISTINCT l.sha256) FROM artifact_links l JOIN state_events e ON e.event_id = l.event_id WHERE e.run_id = r.run_id)
         FROM state_runs r ORDER BY r.run_id",
    )?;
    let rows = statement.query_map([], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, String>(2)?,
            row.get::<_, String>(3)?,
            row.get::<_, Option<String>>(4)?,
            row.get::<_, String>(5)?,
            row.get::<_, u64>(6)?,
            row.get::<_, String>(7)?,
            row.get::<_, u64>(8)?,
            row.get::<_, u64>(9)?,
            row.get::<_, u64>(10)?,
        ))
    })?;
    let mut summaries = Vec::new();
    for row in rows {
        let (
            run_id,
            domain,
            profile,
            subject,
            team,
            status,
            epoch,
            updated_at,
            collections,
            events,
            artifacts,
        ) = row?;
        summaries.push(StateRunSummary {
            run_id,
            domain: parse_domain(&domain)?,
            profile,
            subject,
            team,
            status,
            epoch,
            collections,
            events,
            artifacts,
            updated_at,
        });
    }
    Ok(summaries)
}

pub fn search_metadata(
    config: &RuntimeConfig,
    query: &str,
    category: Option<&str>,
    fetch_limit: usize,
) -> Result<StateMetadataSearchPage> {
    let trimmed = query.trim();
    if trimmed.len() > 128
        || trimmed
            .chars()
            .any(|character| character == '\0' || character == '\r' || character.is_control())
    {
        bail!("state metadata search query is invalid");
    }
    if !(1..=MAX_METADATA_SEARCH_WINDOW).contains(&fetch_limit) {
        bail!("state metadata search limit must be between one and {MAX_METADATA_SEARCH_WINDOW}");
    }
    if category.is_some_and(|value| !matches!(value, "state" | "artifact")) {
        bail!("state metadata search category is invalid");
    }
    let include_state = category != Some("artifact");
    let include_artifacts = category != Some("state");
    let pattern = state_like_pattern(&trimmed.to_lowercase());
    let connection = open_database(config)?;
    let mut totals = BTreeMap::new();
    let mut hits = Vec::new();

    if include_state {
        let run_predicate = "(lower(run_id) LIKE ?1 ESCAPE '\\' OR lower(domain) LIKE ?1 ESCAPE '\\' OR lower(profile) LIKE ?1 ESCAPE '\\' OR lower(subject) LIKE ?1 ESCAPE '\\' OR lower(COALESCE(team, '')) LIKE ?1 ESCAPE '\\' OR lower(status) LIKE ?1 ESCAPE '\\')";
        let run_total: u64 = connection.query_row(
            &format!("SELECT COUNT(*) FROM state_runs WHERE {run_predicate}"),
            params![pattern],
            |row| row.get(0),
        )?;
        totals.insert("run".into(), run_total);
        let mut statement = connection.prepare(&format!(
            "SELECT run_id, subject, domain, profile, status, updated_at FROM state_runs WHERE {run_predicate} ORDER BY updated_at DESC, run_id LIMIT ?2"
        ))?;
        hits.extend(
            statement
                .query_map(params![pattern, fetch_limit as u64], |row| {
                    let run_id = row.get::<_, String>(0)?;
                    let subject = row.get::<_, String>(1)?;
                    let domain = row.get::<_, String>(2)?;
                    let profile = row.get::<_, String>(3)?;
                    let status = row.get::<_, String>(4)?;
                    Ok(StateMetadataSearchHit {
                        id: format!("run:{run_id}"),
                        target_id: format!("run:{run_id}"),
                        category: "run".into(),
                        run_id,
                        title: subject,
                        detail: format!("{domain} · {profile} · {status}"),
                        occurred_at: row.get(5)?,
                        digest: None,
                    })
                })?
                .collect::<std::result::Result<Vec<_>, _>>()?,
        );

        let collection_predicate = "(lower(c.run_id) LIKE ?1 ESCAPE '\\' OR lower(r.subject) LIKE ?1 ESCAPE '\\' OR lower(c.name) LIKE ?1 ESCAPE '\\' OR lower(c.document_schema) LIKE ?1 ESCAPE '\\')";
        let collection_total: u64 = connection.query_row(
            &format!("SELECT COUNT(*) FROM state_collections c JOIN state_runs r ON r.run_id = c.run_id WHERE {collection_predicate}"),
            params![pattern],
            |row| row.get(0),
        )?;
        totals.insert("collection".into(), collection_total);
        let mut statement = connection.prepare(&format!(
            "SELECT c.run_id, c.name, c.document_schema, c.updated_at, c.content_sha256, r.subject FROM state_collections c JOIN state_runs r ON r.run_id = c.run_id WHERE {collection_predicate} ORDER BY c.updated_at DESC, c.run_id, c.name LIMIT ?2"
        ))?;
        hits.extend(
            statement
                .query_map(params![pattern, fetch_limit as u64], |row| {
                    let run_id = row.get::<_, String>(0)?;
                    let name = row.get::<_, String>(1)?;
                    Ok(StateMetadataSearchHit {
                        id: format!("collection:{run_id}:{name}"),
                        target_id: format!("collection:{run_id}:{name}"),
                        category: "collection".into(),
                        run_id,
                        title: name,
                        detail: format!(
                            "{} · {}",
                            row.get::<_, String>(5)?,
                            row.get::<_, String>(2)?
                        ),
                        occurred_at: row.get(3)?,
                        digest: Some(row.get(4)?),
                    })
                })?
                .collect::<std::result::Result<Vec<_>, _>>()?,
        );

        let event_predicate = "(lower(e.run_id) LIKE ?1 ESCAPE '\\' OR lower(e.subject) LIKE ?1 ESCAPE '\\' OR lower(e.kind) LIKE ?1 ESCAPE '\\' OR lower(e.actor) LIKE ?1 ESCAPE '\\' OR lower(e.authority) LIKE ?1 ESCAPE '\\')";
        let event_total: u64 = connection.query_row(
            &format!("SELECT COUNT(*) FROM state_events e WHERE {event_predicate}"),
            params![pattern],
            |row| row.get(0),
        )?;
        totals.insert("event".into(), event_total);
        let mut statement = connection.prepare(&format!(
            "SELECT e.event_id, e.run_id, e.kind, e.actor, e.authority, e.occurred_at, e.payload_sha256, e.subject FROM state_events e WHERE {event_predicate} ORDER BY e.occurred_at DESC, e.sequence DESC LIMIT ?2"
        ))?;
        hits.extend(
            statement
                .query_map(params![pattern, fetch_limit as u64], |row| {
                    let event_id = row.get::<_, String>(0)?;
                    Ok(StateMetadataSearchHit {
                        id: format!("state:{event_id}"),
                        target_id: format!("state:{event_id}"),
                        category: "event".into(),
                        run_id: row.get(1)?,
                        title: row.get(2)?,
                        detail: format!(
                            "{} · {} · {}",
                            row.get::<_, String>(7)?,
                            row.get::<_, String>(3)?,
                            row.get::<_, String>(4)?
                        ),
                        occurred_at: row.get(5)?,
                        digest: Some(row.get(6)?),
                    })
                })?
                .collect::<std::result::Result<Vec<_>, _>>()?,
        );
    }

    if include_artifacts {
        let artifact_predicate = "(lower(e.run_id) LIKE ?1 ESCAPE '\\' OR lower(e.subject) LIKE ?1 ESCAPE '\\' OR lower(a.sha256) LIKE ?1 ESCAPE '\\' OR lower(l.media_type) LIKE ?1 ESCAPE '\\' OR lower(l.role) LIKE ?1 ESCAPE '\\' OR lower(l.label) LIKE ?1 ESCAPE '\\' OR lower(l.sensitivity) LIKE ?1 ESCAPE '\\')";
        let artifact_total: u64 = connection.query_row(
            &format!("SELECT COUNT(*) FROM artifact_links l JOIN artifacts a ON a.sha256 = l.sha256 JOIN state_events e ON e.event_id = l.event_id WHERE {artifact_predicate}"),
            params![pattern],
            |row| row.get(0),
        )?;
        totals.insert("artifact".into(), artifact_total);
        let mut statement = connection.prepare(&format!(
            "SELECT l.event_id, e.run_id, a.sha256, l.label, l.role, l.media_type, l.sensitivity, a.created_at, e.subject FROM artifact_links l JOIN artifacts a ON a.sha256 = l.sha256 JOIN state_events e ON e.event_id = l.event_id WHERE {artifact_predicate} ORDER BY a.created_at DESC, e.run_id, a.sha256 LIMIT ?2"
        ))?;
        hits.extend(
            statement
                .query_map(params![pattern, fetch_limit as u64], |row| {
                    let event_id = row.get::<_, String>(0)?;
                    let run_id = row.get::<_, String>(1)?;
                    let sha256 = row.get::<_, String>(2)?;
                    Ok(StateMetadataSearchHit {
                        id: format!("artifact-link:{run_id}:{event_id}:{sha256}"),
                        target_id: format!("artifact:{sha256}"),
                        category: "artifact".into(),
                        run_id,
                        title: row.get(3)?,
                        detail: format!(
                            "{} · {} · {} · {}",
                            row.get::<_, String>(8)?,
                            row.get::<_, String>(4)?,
                            row.get::<_, String>(5)?,
                            row.get::<_, String>(6)?
                        ),
                        occurred_at: row.get(7)?,
                        digest: Some(sha256),
                    })
                })?
                .collect::<std::result::Result<Vec<_>, _>>()?,
        );
    }

    hits.sort_by(|left, right| {
        right
            .occurred_at
            .cmp(&left.occurred_at)
            .then_with(|| left.id.cmp(&right.id))
    });
    hits.truncate(fetch_limit);
    Ok(StateMetadataSearchPage { totals, hits })
}

pub fn trace(config: &RuntimeConfig, run_id: &str) -> Result<Vec<StateEvent>> {
    validate_identifier(run_id, "run id")?;
    let connection = open_database(config)?;
    let mut statement = connection.prepare(
        "SELECT sequence, event_id, run_id, subject, epoch, kind, actor, authority, occurred_at, payload_json, payload_sha256, previous_hash, hash FROM state_events WHERE run_id = ?1 ORDER BY sequence",
    )?;
    let rows = statement.query_map(params![run_id], state_event_from_row)?;
    rows.collect::<std::result::Result<Vec<_>, _>>()
        .map_err(Into::into)
}

pub fn trace_summary(
    config: &RuntimeConfig,
    run_id: &str,
    event_limit: usize,
    artifact_limit: usize,
) -> Result<StateTraceSummary> {
    validate_identifier(run_id, "run id")?;
    if !(1..=1_000).contains(&event_limit) || !(1..=1_000).contains(&artifact_limit) {
        bail!("state trace summary limits must be between one and 1000");
    }
    let run = load_run(config, run_id)?;
    let connection = open_database(config)?;
    let mut collection_statement = connection.prepare(
        "SELECT name, document_schema, version, authorities_json, access_json, prompt_eligible, content_sha256, updated_at
         FROM state_collections WHERE run_id = ?1 AND subject = ?2 ORDER BY name",
    )?;
    let collection_rows = collection_statement.query_map(params![run_id, run.subject], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, u64>(2)?,
            row.get::<_, String>(3)?,
            row.get::<_, String>(4)?,
            row.get::<_, bool>(5)?,
            row.get::<_, String>(6)?,
            row.get::<_, String>(7)?,
        ))
    })?;
    let mut collections = Vec::new();
    for row in collection_rows {
        let (
            name,
            document_schema,
            version,
            authorities_json,
            access_json,
            prompt_eligible,
            content_sha256,
            updated_at,
        ) = row?;
        let authorities: Vec<StateAuthority> = serde_json::from_str(&authorities_json)?;
        let access: Vec<CollectionAccess> = serde_json::from_str(&access_json)?;
        let audiences = access
            .iter()
            .map(|rule| rule.audience)
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect();
        collections.push(StateTraceCollectionSummary {
            name,
            document_schema,
            version,
            authorities,
            audiences,
            principal_scoped: access.iter().any(|rule| !rule.principals.is_empty()),
            prompt_eligible,
            content_sha256,
            updated_at,
        });
    }
    let events_total: u64 = connection.query_row(
        "SELECT COUNT(*) FROM state_events WHERE run_id = ?1",
        params![run_id],
        |row| row.get(0),
    )?;
    let mut event_statement = connection.prepare(
        "SELECT sequence, event_id, epoch, kind, actor, authority, occurred_at, payload_sha256, previous_hash, hash
         FROM state_events WHERE run_id = ?1 ORDER BY sequence DESC LIMIT ?2",
    )?;
    let event_rows = event_statement.query_map(params![run_id, event_limit as u64], |row| {
        Ok((
            row.get::<_, u64>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, u64>(2)?,
            row.get::<_, String>(3)?,
            row.get::<_, String>(4)?,
            row.get::<_, String>(5)?,
            row.get::<_, String>(6)?,
            row.get::<_, String>(7)?,
            row.get::<_, String>(8)?,
            row.get::<_, String>(9)?,
        ))
    })?;
    let mut events = Vec::new();
    for row in event_rows {
        let (
            sequence,
            event_id,
            epoch,
            kind,
            actor,
            authority,
            occurred_at,
            payload_sha256,
            previous_hash,
            hash,
        ) = row?;
        events.push(StateTraceEventSummary {
            sequence,
            event_id,
            epoch,
            kind,
            actor,
            authority: parse_authority(&authority)?,
            occurred_at,
            payload_sha256,
            previous_hash,
            hash,
        });
    }
    events.reverse();
    let artifact_links = artifacts_for_run(config, run_id)?;
    let artifacts_total = artifact_links.len() as u64;
    let artifacts = artifact_links
        .into_iter()
        .rev()
        .take(artifact_limit)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .map(|link| StateTraceArtifactSummary {
            event_id: link.event_id,
            sha256: link.artifact.sha256,
            size: link.artifact.size,
            media_type: link.artifact.media_type,
            role: link.artifact.role,
            label: link.artifact.label,
            sensitivity: link.artifact.sensitivity,
            created_at: link.artifact.created_at,
        })
        .collect::<Vec<_>>();
    Ok(StateTraceSummary {
        run_id: run.run_id,
        subject: run.subject,
        status: run.status,
        epoch: run.epoch,
        collections,
        events_truncated: events_total > events.len() as u64,
        events_total,
        events,
        artifacts_truncated: artifacts_total > artifacts.len() as u64,
        artifacts_total,
        artifacts,
        payloads_included: false,
        state_values_included: false,
    })
}
