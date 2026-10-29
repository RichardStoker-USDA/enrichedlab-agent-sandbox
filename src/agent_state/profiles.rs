//! State profiles and schemas.
use super::*;

pub fn list_profiles(config: &RuntimeConfig) -> Result<Vec<StateViewProfile>> {
    let mut profiles = BTreeMap::new();
    for name in [
        "scientific-worker",
        "research-lead",
        "cluster-operator",
        "software-engineer",
    ] {
        let profile = built_in_profile(name)?;
        profiles.insert(profile.name.clone(), profile);
    }
    if config.state_profiles_dir().is_dir() {
        for entry in fs::read_dir(config.state_profiles_dir())? {
            let path = entry?.path();
            if path.extension().and_then(|value| value.to_str()) == Some("json") {
                let profile: StateViewProfile =
                    serde_json::from_slice(&read_bounded_regular(&path, 1024 * 1024)?)?;
                validate_profile(&profile)?;
                profiles.insert(profile.name.clone(), profile);
            }
        }
    }
    Ok(profiles.into_values().collect())
}

pub fn install_profile(
    config: &RuntimeConfig,
    source: &Path,
    replace: bool,
) -> Result<StateViewProfile> {
    let profile: StateViewProfile =
        serde_json::from_slice(&read_bounded_regular(source, 1024 * 1024)?)?;
    validate_profile(&profile)?;
    let destination = config
        .state_profiles_dir()
        .join(format!("{}.json", profile.name));
    let shadows_builtin = matches!(
        profile.name.as_str(),
        "scientific-worker" | "research-lead" | "cluster-operator" | "software-engineer"
    );
    if (destination.exists() || shadows_builtin) && !replace {
        bail!("state profile already exists; use --replace after review");
    }
    write_private(&destination, &serde_json::to_vec_pretty(&profile)?)?;
    Ok(profile)
}

pub fn list_schema_names(config: &RuntimeConfig) -> Result<Vec<String>> {
    let mut names = BTreeSet::from([
        "cluster-ops".to_string(),
        "coordination".to_string(),
        "execution".to_string(),
        "knowledge".to_string(),
        "scientific".to_string(),
    ]);
    if config.state_schemas_dir().is_dir() {
        for entry in fs::read_dir(config.state_schemas_dir())? {
            let path = entry?.path();
            if path.extension().and_then(|value| value.to_str()) == Some("json")
                && let Some(stem) = path.file_stem().and_then(|value| value.to_str())
            {
                names.insert(stem.to_string());
            }
        }
    }
    Ok(names.into_iter().collect())
}

pub fn install_schema(
    config: &RuntimeConfig,
    name: &str,
    source: &Path,
    replace: bool,
) -> Result<String> {
    validate_identifier(name, "schema")?;
    let bytes = read_bounded_regular(source, 1024 * 1024)?;
    let schema: Value = serde_json::from_slice(&bytes)?;
    jsonschema::validator_for(&schema).context("invalid JSON Schema")?;
    let destination = config.state_schemas_dir().join(format!("{name}.json"));
    let shadows_builtin = matches!(
        name,
        "execution" | "knowledge" | "scientific" | "cluster-ops" | "coordination"
    );
    if (destination.exists() || shadows_builtin) && !replace {
        bail!("state schema already exists; use --replace after review");
    }
    write_private(&destination, &serde_json::to_vec_pretty(&schema)?)?;
    Ok(name.into())
}

pub(super) fn load_profile(config: &RuntimeConfig, name: &str) -> Result<StateViewProfile> {
    validate_identifier(name, "profile")?;
    let custom = config.state_profiles_dir().join(format!("{name}.json"));
    let profile = if custom.is_file() {
        serde_json::from_slice(&read_bounded_regular(&custom, 1024 * 1024)?)?
    } else {
        built_in_profile(name)?
    };
    validate_profile(&profile)?;
    Ok(profile)
}

pub(super) fn built_in_profile(name: &str) -> Result<StateViewProfile> {
    let body = match name {
        "scientific-worker" => SCIENTIFIC_WORKER_PROFILE,
        "research-lead" => RESEARCH_LEAD_PROFILE,
        "cluster-operator" => CLUSTER_OPERATOR_PROFILE,
        "software-engineer" => SOFTWARE_ENGINEER_PROFILE,
        _ => bail!("unknown state profile {name}"),
    };
    Ok(serde_json::from_str(body)?)
}

pub(super) fn validate_profile(profile: &StateViewProfile) -> Result<()> {
    if profile.schema_version != 1 {
        bail!("unsupported state profile schema");
    }
    validate_identifier(&profile.name, "profile")?;
    if profile.max_state_tokens == 0 || profile.max_state_tokens > 262_144 {
        bail!("state profile token cap is invalid");
    }
    if profile.latest_event_count > 256
        || !(0.1..0.95).contains(&profile.soft_reset_ratio)
        || !(0.1..=0.95).contains(&profile.hard_reset_ratio)
        || profile.soft_reset_ratio >= profile.hard_reset_ratio
    {
        bail!("state profile context thresholds are invalid");
    }
    let mut names = BTreeSet::new();
    for rule in &profile.collections {
        validate_identifier(&rule.name, "profile collection")?;
        if !names.insert(rule.name.clone())
            || rule.max_bytes == 0
            || rule.max_bytes > MAX_STATE_BYTES
        {
            bail!("state profile collection rule is invalid");
        }
        if rule.pointers.is_empty() || rule.pointers.len() > MAX_POINTERS {
            bail!("state profile collection needs bounded JSON pointers");
        }
        for pointer in &rule.pointers {
            validate_pointer(pointer)?;
        }
    }
    Ok(())
}

pub(super) fn load_schema(config: &RuntimeConfig, name: &str) -> Result<Value> {
    let custom = config.state_schemas_dir().join(format!("{name}.json"));
    let body = if custom.is_file() {
        String::from_utf8(read_bounded_regular(&custom, 1024 * 1024)?)?
    } else {
        match name {
            "execution" => EXECUTION_SCHEMA.into(),
            "knowledge" => KNOWLEDGE_SCHEMA.into(),
            "scientific" => SCIENTIFIC_SCHEMA.into(),
            "cluster-ops" => CLUSTER_OPS_SCHEMA.into(),
            "coordination" => COORDINATION_SCHEMA.into(),
            _ => bail!("unknown state schema {name}"),
        }
    };
    Ok(serde_json::from_str(&body)?)
}

pub(super) fn validate_document(schema: &Value, document: &Value) -> Result<()> {
    let validator = jsonschema::validator_for(schema).context("invalid state JSON Schema")?;
    if let Err(error) = validator.validate(document) {
        bail!("state document does not match its schema: {error}");
    }
    Ok(())
}
