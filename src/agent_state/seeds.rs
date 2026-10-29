//! Seeding the scientific worker and continuity agent collections.
use super::*;

pub fn seed_scientific_worker(
    config: &RuntimeConfig,
    run_id: &str,
    subject: &str,
    objective: &str,
    team: Option<&str>,
) -> Result<Vec<StateCollection>> {
    match load_run(config, run_id) {
        Ok(run) => {
            if run.domain != StateDomain::Scientific
                || run.profile != "scientific-worker"
                || run.subject != subject
                || run.team.as_deref() != team
            {
                bail!("existing scientific state run has a different owner or contract");
            }
        }
        Err(error) => {
            if state_run_exists(config, run_id)? {
                return Err(error);
            }
            init_run(
                config,
                run_id,
                StateDomain::Scientific,
                "scientific-worker",
                subject,
                team,
            )?;
        }
    }
    let access = initial_access(subject);
    let mut seeded = Vec::new();
    seeded.push(seed_or_load_collection(
        config,
        run_id,
        subject,
        "execution",
        "execution",
        json!({
            "objective": objective,
            "phase": "prepared",
            "completed": {},
            "pending": {},
            "blockers": {},
            "next_action": "read the fixed assignment and protocol",
            "retry_count": 0,
            "last_observation_ref": null
        }),
        &[
            StateAuthority::Model,
            StateAuthority::Operator,
            StateAuthority::Machine,
        ],
        &access,
        true,
    )?);
    seeded.push(seed_or_load_collection(
        config,
        run_id,
        subject,
        "knowledge",
        "knowledge",
        json!({"facts": {}, "hypotheses": {}, "decisions": {}, "lessons": {}, "rejected": {}}),
        &[StateAuthority::Model, StateAuthority::Operator],
        &access,
        true,
    )?);
    seeded.push(seed_or_load_collection(
        config,
        run_id,
        subject,
        "scientific",
        "scientific",
        json!({"input_refs": {}, "method_refs": {}, "intermediate_refs": {}, "failed_attempts": {}, "claim_refs": {}, "validation_refs": {}}),
        &[StateAuthority::Model, StateAuthority::Operator, StateAuthority::Machine],
        &access,
        true,
    )?);
    seeded.push(seed_or_load_collection(
        config,
        run_id,
        subject,
        "coordination",
        "coordination",
        json!({"role": "independent research worker", "owned_paths": ["output"], "dependencies": {}, "shared_artifact_refs": {}, "messages": {}}),
        &[StateAuthority::Operator, StateAuthority::Machine],
        &[
            CollectionAccess { audience: StateAudience::Lead, principals: Vec::new() },
            CollectionAccess { audience: StateAudience::Oversight, principals: Vec::new() },
        ],
        false,
    )?);
    Ok(seeded)
}

#[allow(clippy::too_many_arguments)]
pub fn seed_continuity_agent(
    config: &RuntimeConfig,
    run_id: &str,
    subject: &str,
    owner: &str,
    domain: StateDomain,
    profile_name: &str,
    objective: &str,
    role: &str,
) -> Result<Vec<StateCollection>> {
    validate_identifier(owner, "owner")?;
    validate_text(objective, "objective", 8_192)?;
    validate_text(role, "role", 1_024)?;
    validate_continuity_profile(config, domain, profile_name)?;
    let profile = load_profile(config, profile_name)?;
    let requested = profile
        .collections
        .iter()
        .map(|rule| rule.name.as_str())
        .collect::<BTreeSet<_>>();
    match load_run(config, run_id) {
        Ok(run) => {
            if run.domain != domain
                || run.profile != profile.name
                || run.subject != subject
                || run.team.is_some()
            {
                bail!("existing logical-agent state run has a different owner or contract");
            }
        }
        Err(error) => {
            if state_run_exists(config, run_id)? {
                return Err(error);
            }
            init_run(config, run_id, domain, &profile.name, subject, None)?;
        }
    }

    let principals = if owner == subject {
        vec![subject.into()]
    } else {
        vec![subject.into(), owner.into()]
    };
    let access = [
        StateAudience::Agent,
        StateAudience::Team,
        StateAudience::Lead,
        StateAudience::Oversight,
    ]
    .into_iter()
    .map(|audience| CollectionAccess {
        audience,
        principals: principals.clone(),
    })
    .collect::<Vec<_>>();

    let mut seeded = Vec::new();
    if requested.contains("execution") {
        seeded.push(seed_or_load_collection(
            config,
            run_id,
            subject,
            "execution",
            "execution",
            json!({
                "objective": objective,
                "phase": "prepared",
                "completed": {},
                "pending": {},
                "blockers": {},
                "next_action": "read the current state view and bounded task",
                "retry_count": 0,
                "last_observation_ref": null
            }),
            &[
                StateAuthority::Model,
                StateAuthority::Operator,
                StateAuthority::Machine,
            ],
            &access,
            true,
        )?);
    }
    if requested.contains("knowledge") {
        seeded.push(seed_or_load_collection(
            config,
            run_id,
            subject,
            "knowledge",
            "knowledge",
            json!({"facts": {}, "hypotheses": {}, "decisions": {}, "lessons": {}, "rejected": {}}),
            &[StateAuthority::Model, StateAuthority::Operator],
            &access,
            true,
        )?);
    }
    if requested.contains("scientific") {
        seeded.push(seed_or_load_collection(
            config,
            run_id,
            subject,
            "scientific",
            "scientific",
            json!({
                "input_refs": {},
                "method_refs": {},
                "intermediate_refs": {},
                "failed_attempts": {},
                "claim_refs": {},
                "validation_refs": {}
            }),
            &[
                StateAuthority::Model,
                StateAuthority::Operator,
                StateAuthority::Machine,
            ],
            &access,
            true,
        )?);
    }
    if requested.contains("cluster-ops") {
        seeded.push(seed_or_load_collection(
            config,
            run_id,
            subject,
            "cluster-ops",
            "cluster-ops",
            json!({
                "desired_state": {},
                "observed_refs": {},
                "proposals": {},
                "custody": {},
                "rollback": {},
                "verification": {}
            }),
            &[
                StateAuthority::Model,
                StateAuthority::Operator,
                StateAuthority::Machine,
            ],
            &access,
            true,
        )?);
    }
    if requested.contains("coordination") {
        seeded.push(seed_or_load_collection(
            config,
            run_id,
            subject,
            "coordination",
            "coordination",
            json!({
                "role": role,
                "owned_paths": ["."],
                "dependencies": {},
                "shared_artifact_refs": {},
                "messages": {}
            }),
            &[StateAuthority::Operator, StateAuthority::Machine],
            &access,
            true,
        )?);
    }
    Ok(seeded)
}

pub fn validate_continuity_profile(
    config: &RuntimeConfig,
    domain: StateDomain,
    profile_name: &str,
) -> Result<()> {
    match profile_name {
        "scientific-worker" | "research-lead" if domain != StateDomain::Scientific => {
            bail!("state profile {profile_name} requires the scientific domain")
        }
        "cluster-operator" if domain != StateDomain::ClusterOps => {
            bail!("the cluster-operator profile requires the cluster-ops domain")
        }
        "software-engineer" if domain != StateDomain::SoftwareEngineering => {
            bail!("the software-engineer profile requires the software-engineering domain")
        }
        _ => {}
    }
    let profile = load_profile(config, profile_name)?;
    let requested = profile
        .collections
        .iter()
        .map(|rule| rule.name.as_str())
        .collect::<BTreeSet<_>>();
    if !requested.contains("execution") {
        bail!("a continuity state profile must include the execution collection");
    }
    let known = [
        "execution",
        "knowledge",
        "scientific",
        "cluster-ops",
        "coordination",
    ]
    .into_iter()
    .collect::<BTreeSet<_>>();
    if let Some(name) = requested.difference(&known).next() {
        bail!("state profile requests an unsupported collection: {name}");
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
pub(super) fn seed_or_load_collection(
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
    match load_collection(config, run_id, subject, name) {
        Ok(collection) => {
            if collection.document_schema != schema_name
                || collection.authorities != authorities
                || serde_json::to_value(&collection.access)? != serde_json::to_value(access)?
                || collection.prompt_eligible != prompt_eligible
            {
                bail!(
                    "existing state collection has a different access or schema contract: {name}"
                );
            }
            if name == "execution" && collection.value["objective"] != value["objective"] {
                bail!("existing execution state has a different immutable objective");
            }
            Ok(collection)
        }
        Err(error) => {
            if state_collection_exists(config, run_id, subject, name)? {
                return Err(error);
            }
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
    }
}

pub(super) fn state_run_exists(config: &RuntimeConfig, run_id: &str) -> Result<bool> {
    let connection = open_database(config)?;
    Ok(connection.query_row(
        "SELECT EXISTS(SELECT 1 FROM state_runs WHERE run_id = ?1)",
        params![run_id],
        |row| row.get(0),
    )?)
}

pub(super) fn state_collection_exists(
    config: &RuntimeConfig,
    run_id: &str,
    subject: &str,
    name: &str,
) -> Result<bool> {
    let connection = open_database(config)?;
    Ok(connection.query_row(
        "SELECT EXISTS(SELECT 1 FROM state_collections WHERE run_id = ?1 AND subject = ?2 AND name = ?3)",
        params![run_id, subject, name],
        |row| row.get(0),
    )?)
}
