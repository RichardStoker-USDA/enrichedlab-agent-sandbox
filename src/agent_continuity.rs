use std::{
    collections::BTreeMap,
    fs::{self, OpenOptions},
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};

use anyhow::{Context, Result, bail};
use fs2::FileExt;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use uuid::Uuid;

use crate::{
    agent_state::{self, StateDomain},
    audit,
    cli::{RuntimeArgs, Thinking},
    config::{RuntimeConfig, create_private_dir, set_mode, write_private},
    harness::HarnessKind,
};

const MAX_AGENT_NAME_BYTES: usize = 40;
const MAX_OWNER_BYTES: usize = 128;
const MAX_OBJECTIVE_BYTES: usize = 8_192;
const MAX_ROLE_BYTES: usize = 1_024;
const MAX_RESULT_CLASS_BYTES: usize = 256;

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum LogicalAgentLifecycle {
    Registering,
    Idle,
    Active,
    Retired,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct LogicalAgentRecord {
    pub schema_version: u32,
    pub agent_id: String,
    pub name: String,
    pub owner: String,
    pub domain: StateDomain,
    pub state_profile: String,
    pub state_run_id: String,
    pub objective: String,
    pub role: String,
    pub workspace: PathBuf,
    pub harness: HarnessKind,
    pub thinking: Option<Thinking>,
    pub runtime: RuntimeArgs,
    pub runtime_generation: u64,
    pub active_sandbox: Option<String>,
    pub sessions_started: u64,
    pub sessions_completed: u64,
    pub failed_sessions: u64,
    pub last_result_class: Option<String>,
    pub lifecycle: LogicalAgentLifecycle,
    pub revision: u64,
    pub created_at_unix: u64,
    pub updated_at_unix: u64,
}

pub struct CreateLogicalAgent<'a> {
    pub name: &'a str,
    pub owner: &'a str,
    pub domain: StateDomain,
    pub state_profile: &'a str,
    pub objective: &'a str,
    pub role: &'a str,
    pub harness: HarnessKind,
    pub thinking: Option<Thinking>,
    pub runtime: RuntimeArgs,
}

pub struct AgentLock {
    file: fs::File,
}

impl Drop for AgentLock {
    fn drop(&mut self) {
        let _ = self.file.unlock();
    }
}

pub fn lock(config: &RuntimeConfig, name: &str) -> Result<AgentLock> {
    validate_name(name)?;
    create_private_dir(&config.continuity_agent_locks_dir())?;
    let path = config
        .continuity_agent_locks_dir()
        .join(format!("{name}.lock"));
    let mut options = OpenOptions::new();
    options.create(true).read(true).write(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let file = options
        .open(&path)
        .with_context(|| format!("failed to open logical-agent lock {}", path.display()))?;
    file.lock_exclusive()?;
    set_mode(&path, 0o600)?;
    Ok(AgentLock { file })
}

pub fn create(config: &RuntimeConfig, spec: CreateLogicalAgent<'_>) -> Result<LogicalAgentRecord> {
    validate_name(spec.name)?;
    validate_identifier(spec.owner, "owner", MAX_OWNER_BYTES)?;
    validate_identifier(spec.state_profile, "state profile", 128)?;
    validate_text(spec.objective, "objective", MAX_OBJECTIVE_BYTES)?;
    validate_text(spec.role, "role", MAX_ROLE_BYTES)?;
    agent_state::validate_continuity_profile(config, spec.domain, spec.state_profile)?;
    if !matches!(spec.harness, HarnessKind::OpenCode | HarnessKind::Pi) {
        bail!("logical agents can be launched only with opencode or pi");
    }
    let _guard = lock(config, spec.name)?;
    let path = record_path(config, spec.name)?;
    if path.exists() {
        bail!("logical agent already exists: {}", spec.name);
    }
    let now = unix_now()?;
    let agent_id = format!("agent-{}", Uuid::new_v4().simple());
    let agents_root = config.root.join("agents");
    create_private_dir(&agents_root)?;
    let agents_root = fs::canonicalize(&agents_root)
        .with_context(|| format!("failed to resolve {}", agents_root.display()))?;
    let workspace = agents_root.join(spec.name);
    create_private_dir(&workspace)?;
    let workspace = fs::canonicalize(&workspace)
        .with_context(|| format!("failed to resolve {}", workspace.display()))?;
    let mut record = LogicalAgentRecord {
        schema_version: 1,
        state_run_id: agent_id.clone(),
        agent_id,
        name: spec.name.into(),
        owner: spec.owner.into(),
        domain: spec.domain,
        state_profile: spec.state_profile.into(),
        objective: spec.objective.into(),
        role: spec.role.into(),
        workspace,
        harness: spec.harness,
        thinking: spec.thinking,
        runtime: spec.runtime,
        runtime_generation: 0,
        active_sandbox: None,
        sessions_started: 0,
        sessions_completed: 0,
        failed_sessions: 0,
        last_result_class: None,
        lifecycle: LogicalAgentLifecycle::Registering,
        revision: 1,
        created_at_unix: now,
        updated_at_unix: now,
    };
    save(config, &record)?;
    agent_state::seed_continuity_agent(
        config,
        &record.state_run_id,
        &record.agent_id,
        &record.owner,
        record.domain,
        &record.state_profile,
        &record.objective,
        &record.role,
    )?;
    agent_state::materialize_view(
        config,
        &record.state_run_id,
        &record.agent_id,
        &record.workspace,
    )?;
    record.lifecycle = LogicalAgentLifecycle::Idle;
    record.revision += 1;
    record.updated_at_unix = unix_now()?;
    save(config, &record)?;
    audit::append(
        config,
        "host-controller",
        "logical-agent.create",
        &format!("logical-agent/{}", record.name),
        "passed",
        serde_json::json!({
            "agent_id": record.agent_id,
            "owner": record.owner,
            "domain": record.domain,
            "state_profile": record.state_profile,
            "harness": record.harness,
            "runtime_route": record.runtime.route,
            "runtime_gateway_profile": record.runtime.gateway_profile
        }),
    )?;
    Ok(record)
}

pub fn load(config: &RuntimeConfig, name: &str) -> Result<LogicalAgentRecord> {
    let path = record_path(config, name)?;
    let body =
        fs::read(&path).with_context(|| format!("logical agent is not registered: {name}"))?;
    let record: LogicalAgentRecord = serde_json::from_slice(&body)
        .with_context(|| format!("invalid logical-agent record {}", path.display()))?;
    validate_record(config, &record)?;
    Ok(record)
}

pub fn list(config: &RuntimeConfig) -> Result<Vec<LogicalAgentRecord>> {
    let mut records = Vec::new();
    for entry in fs::read_dir(config.continuity_agents_dir())? {
        let path = entry?.path();
        if path.extension().and_then(|value| value.to_str()) != Some("json") {
            continue;
        }
        let record: LogicalAgentRecord = serde_json::from_slice(&fs::read(&path)?)
            .with_context(|| format!("invalid logical-agent record {}", path.display()))?;
        validate_record(config, &record)?;
        records.push(record);
    }
    records.sort_by(|left, right| left.name.cmp(&right.name));
    Ok(records)
}

pub fn save(config: &RuntimeConfig, record: &LogicalAgentRecord) -> Result<()> {
    validate_record(config, record)?;
    let mut body = serde_json::to_vec_pretty(record)?;
    body.push(b'\n');
    write_private(&record_path(config, &record.name)?, &body)
}

pub fn ensure_registered_state(
    config: &RuntimeConfig,
    record: &mut LogicalAgentRecord,
) -> Result<()> {
    if record.lifecycle != LogicalAgentLifecycle::Registering {
        return Ok(());
    }
    agent_state::seed_continuity_agent(
        config,
        &record.state_run_id,
        &record.agent_id,
        &record.owner,
        record.domain,
        &record.state_profile,
        &record.objective,
        &record.role,
    )?;
    agent_state::materialize_view(
        config,
        &record.state_run_id,
        &record.agent_id,
        &record.workspace,
    )?;
    record.lifecycle = LogicalAgentLifecycle::Idle;
    touch(record)?;
    save(config, record)
}

pub fn next_runtime(record: &LogicalAgentRecord) -> Result<(u64, String)> {
    if record.lifecycle == LogicalAgentLifecycle::Retired {
        bail!("logical agent is retired: {}", record.name);
    }
    if let Some(active) = &record.active_sandbox {
        bail!("logical agent already has active sandbox {active}");
    }
    let generation = record.runtime_generation.saturating_add(1);
    let mut hasher = Sha256::new();
    hasher.update(record.agent_id.as_bytes());
    hasher.update(generation.to_be_bytes());
    let digest = hex::encode(hasher.finalize());
    let name = format!("na-{}-{generation:04x}", &digest[..8]);
    crate::state::validate_name(&name)?;
    Ok((generation, name))
}

pub fn attach_runtime(
    config: &RuntimeConfig,
    record: &mut LogicalAgentRecord,
    generation: u64,
    sandbox: &str,
) -> Result<()> {
    crate::state::validate_name(sandbox)?;
    if record.lifecycle == LogicalAgentLifecycle::Retired || record.active_sandbox.is_some() {
        bail!("logical agent cannot attach another runtime");
    }
    if generation != record.runtime_generation.saturating_add(1) {
        bail!("logical-agent runtime generation changed during allocation");
    }
    record.runtime_generation = generation;
    record.active_sandbox = Some(sandbox.into());
    record.lifecycle = LogicalAgentLifecycle::Active;
    touch(record)?;
    save(config, record)
}

pub fn detach_runtime(config: &RuntimeConfig, record: &mut LogicalAgentRecord) -> Result<()> {
    if record.lifecycle == LogicalAgentLifecycle::Retired {
        bail!("retired logical agent cannot detach a runtime");
    }
    record.active_sandbox = None;
    record.lifecycle = LogicalAgentLifecycle::Idle;
    touch(record)?;
    save(config, record)
}

pub fn record_session(
    config: &RuntimeConfig,
    record: &mut LogicalAgentRecord,
    passed: bool,
    result_class: &str,
) -> Result<()> {
    validate_text(result_class, "result class", MAX_RESULT_CLASS_BYTES)?;
    if record.sessions_completed >= record.sessions_started {
        bail!("logical agent has no started provider session to complete");
    }
    record.sessions_completed = record.sessions_completed.saturating_add(1);
    if !passed {
        record.failed_sessions = record.failed_sessions.saturating_add(1);
    }
    record.last_result_class = Some(result_class.into());
    touch(record)?;
    save(config, record)
}

pub fn begin_session(config: &RuntimeConfig, record: &mut LogicalAgentRecord) -> Result<u32> {
    let next = record
        .sessions_started
        .checked_add(1)
        .context("logical-agent session counter overflowed")?;
    let segment = u32::try_from(next)
        .context("logical-agent session count exceeds the provenance segment range")?;
    record.sessions_started = next;
    touch(record)?;
    save(config, record)?;
    Ok(segment)
}

pub fn record_execution_attempt(
    config: &RuntimeConfig,
    record: &LogicalAgentRecord,
    passed: bool,
    retry_pending: bool,
    attempt: u32,
    observation_ref: &str,
) -> Result<()> {
    let collection =
        agent_state::load_collection(config, &record.state_run_id, &record.agent_id, "execution")?;
    let phase = if passed {
        "completed"
    } else if retry_pending {
        "retry-pending"
    } else {
        "failed"
    };
    let next_action = if passed {
        Value::Null
    } else if retry_pending {
        json!("start a fresh provider session using the updated state view")
    } else {
        json!("wait for the Team Lead to inspect evidence and assign the next task")
    };
    agent_state::apply_mutation(
        config,
        &agent_state::StateMutation {
            schema_version: 1,
            run_id: record.state_run_id.clone(),
            subject: record.agent_id.clone(),
            collection: "execution".into(),
            base_version: collection.version,
            set: BTreeMap::from([
                ("/phase".into(), json!(phase)),
                ("/retry_count".into(), json!(attempt.saturating_sub(1))),
                ("/last_observation_ref".into(), json!(observation_ref)),
                ("/next_action".into(), next_action),
            ]),
            remove: Vec::new(),
            rationale: Some(agent_state::DecisionRationale {
                summary: "machine-recorded result of a bounded provider session".into(),
                alternatives: Vec::new(),
                evidence: vec![observation_ref.into()],
                confidence: Some(1.0),
            }),
        },
        "host-controller",
        agent_state::StateAuthority::Machine,
    )?;
    Ok(())
}

pub fn retire(config: &RuntimeConfig, record: &mut LogicalAgentRecord) -> Result<()> {
    if record.active_sandbox.is_some() {
        bail!("release the logical agent runtime before retiring it");
    }
    record.lifecycle = LogicalAgentLifecycle::Retired;
    touch(record)?;
    save(config, record)
}

pub fn record_path(config: &RuntimeConfig, name: &str) -> Result<PathBuf> {
    validate_name(name)?;
    Ok(config.continuity_agents_dir().join(format!("{name}.json")))
}

pub fn validate_name(name: &str) -> Result<()> {
    if name.is_empty()
        || name.len() > MAX_AGENT_NAME_BYTES
        || name.starts_with('-')
        || name.ends_with('-')
        || name.contains("--")
        || !name
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
    {
        bail!(
            "logical-agent name must be a lowercase DNS label of at most {MAX_AGENT_NAME_BYTES} characters"
        );
    }
    Ok(())
}

fn validate_record(config: &RuntimeConfig, record: &LogicalAgentRecord) -> Result<()> {
    if record.schema_version != 1 {
        bail!(
            "unsupported logical-agent record schema {}",
            record.schema_version
        );
    }
    validate_name(&record.name)?;
    validate_identifier(&record.agent_id, "agent id", 128)?;
    validate_identifier(&record.state_run_id, "state run id", 128)?;
    validate_identifier(&record.owner, "owner", MAX_OWNER_BYTES)?;
    validate_identifier(&record.state_profile, "state profile", 128)?;
    validate_text(&record.objective, "objective", MAX_OBJECTIVE_BYTES)?;
    validate_text(&record.role, "role", MAX_ROLE_BYTES)?;
    let agents_root = fs::canonicalize(config.root.join("agents"))
        .context("failed to resolve the managed agents root")?;
    let workspace = fs::canonicalize(&record.workspace)
        .with_context(|| format!("failed to resolve {}", record.workspace.display()))?;
    if !record.workspace.is_absolute()
        || workspace != record.workspace
        || !workspace.starts_with(&agents_root)
    {
        bail!("logical-agent workspace is outside the managed agents root");
    }
    if record.lifecycle == LogicalAgentLifecycle::Active && record.active_sandbox.is_none() {
        bail!("active logical-agent record has no sandbox");
    }
    if record.lifecycle != LogicalAgentLifecycle::Active && record.active_sandbox.is_some() {
        bail!("inactive logical-agent record still references a sandbox");
    }
    if let Some(sandbox) = &record.active_sandbox {
        crate::state::validate_name(sandbox)?;
    }
    if let Some(result) = &record.last_result_class {
        validate_text(result, "result class", MAX_RESULT_CLASS_BYTES)?;
    }
    if record.sessions_completed > record.sessions_started
        || record.failed_sessions > record.sessions_completed
    {
        bail!("logical-agent session counters are inconsistent");
    }
    if !matches!(record.harness, HarnessKind::OpenCode | HarnessKind::Pi) {
        bail!("logical-agent record uses an ingest-only harness");
    }
    Ok(())
}

fn touch(record: &mut LogicalAgentRecord) -> Result<()> {
    record.revision = record.revision.saturating_add(1);
    record.updated_at_unix = unix_now()?;
    Ok(())
}

fn validate_identifier(value: &str, label: &str, max: usize) -> Result<()> {
    if value.is_empty()
        || value.len() > max
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
    {
        bail!("{label} must use bounded ASCII letters, digits, hyphens, or underscores");
    }
    Ok(())
}

fn validate_text(value: &str, label: &str, max: usize) -> Result<()> {
    if value.trim().is_empty()
        || value.len() > max
        || value.contains(['\0', '\r'])
        || value
            .chars()
            .any(|character| character.is_control() && character != '\n' && character != '\t')
    {
        bail!("{label} is empty, too large, or contains control characters");
    }
    Ok(())
}

fn unix_now() -> Result<u64> {
    Ok(SystemTime::now().duration_since(UNIX_EPOCH)?.as_secs())
}

pub fn workspace_state_view(record: &LogicalAgentRecord) -> PathBuf {
    record.workspace.join(Path::new(".agent-state/view.json"))
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use serde_json::json;
    use tempfile::tempdir;

    use super::{
        CreateLogicalAgent, LogicalAgentLifecycle, LogicalAgentRecord, attach_runtime,
        begin_session, create, detach_runtime, load, next_runtime, record_path, record_session,
        validate_name,
    };
    use crate::{agent_state::StateDomain, cli::RuntimeArgs, harness::HarnessKind};
    use crate::{
        agent_state::{self, StateAuthority, StateMutation},
        config::{ControllerSettings, RuntimeConfig, create_private_dir},
    };

    fn fixture() -> LogicalAgentRecord {
        LogicalAgentRecord {
            schema_version: 1,
            agent_id: "agent-1234".into(),
            name: "cluster-helper".into(),
            owner: "local-operator".into(),
            domain: StateDomain::ClusterOps,
            state_profile: "cluster-operator".into(),
            state_run_id: "agent-1234".into(),
            objective: "inspect one cluster".into(),
            role: "operator".into(),
            workspace: tempdir().unwrap().keep().join("agents/cluster-helper"),
            harness: HarnessKind::OpenCode,
            thinking: None,
            runtime: RuntimeArgs::default(),
            runtime_generation: 0,
            active_sandbox: None,
            sessions_started: 0,
            sessions_completed: 0,
            failed_sessions: 0,
            last_result_class: None,
            lifecycle: LogicalAgentLifecycle::Idle,
            revision: 1,
            created_at_unix: 1,
            updated_at_unix: 1,
        }
    }

    #[test]
    fn logical_names_are_bounded_and_path_safe() {
        assert!(validate_name("cluster-helper").is_ok());
        for value in ["Cluster", "cluster_helper", "-cluster", "cluster--helper"] {
            assert!(validate_name(value).is_err());
        }
    }

    #[test]
    fn runtime_names_change_by_generation_and_fit_openshell() {
        let mut record = fixture();
        let (first_generation, first) = next_runtime(&record).unwrap();
        record.runtime_generation = first_generation;
        let (_, second) = next_runtime(&record).unwrap();
        assert_ne!(first, second);
        assert!(first.len() <= 19);
        crate::state::validate_name(&first).unwrap();
    }

    #[test]
    fn state_survives_disposable_runtime_generations() {
        let temp = tempdir().unwrap();
        let config = test_config(temp.path());
        let runtime = RuntimeArgs {
            route: Some("local-model".into()),
            profile: Some("development".into()),
            gateway_profile: Some("docker-local".into()),
            ..RuntimeArgs::default()
        };
        let mut record = create(
            &config,
            CreateLogicalAgent {
                name: "cluster-helper",
                owner: "owner-one",
                domain: StateDomain::ClusterOps,
                state_profile: "cluster-operator",
                objective: "inspect one declared target",
                role: "cluster operations subagent",
                harness: HarnessKind::OpenCode,
                thinking: None,
                runtime,
            },
        )
        .unwrap();
        let (first_generation, first_runtime) = next_runtime(&record).unwrap();
        attach_runtime(&config, &mut record, first_generation, &first_runtime).unwrap();
        let knowledge = agent_state::load_collection(
            &config,
            &record.state_run_id,
            &record.agent_id,
            "knowledge",
        )
        .unwrap();
        agent_state::apply_mutation(
            &config,
            &StateMutation {
                schema_version: 1,
                run_id: record.state_run_id.clone(),
                subject: record.agent_id.clone(),
                collection: "knowledge".into(),
                base_version: knowledge.version,
                set: BTreeMap::from([(
                    "/lessons/runtime".into(),
                    json!({
                        "lesson": "logical state outlives compute",
                        "trigger": "runtime release",
                        "evidence": ["state:event"]
                    }),
                )]),
                remove: Vec::new(),
                rationale: None,
            },
            &record.agent_id,
            StateAuthority::Model,
        )
        .unwrap();
        detach_runtime(&config, &mut record).unwrap();
        let (second_generation, second_runtime) = next_runtime(&record).unwrap();
        assert_ne!(first_runtime, second_runtime);
        attach_runtime(&config, &mut record, second_generation, &second_runtime).unwrap();
        let restored =
            agent_state::generate_view(&config, &record.state_run_id, &record.agent_id).unwrap();
        assert_eq!(
            restored.collections["knowledge"].value["lessons"]["runtime"]["lesson"],
            "logical state outlives compute"
        );
        assert_eq!(
            load(&config, "cluster-helper").unwrap().active_sandbox,
            Some(second_runtime)
        );
    }

    #[test]
    fn invalid_profile_contract_leaves_no_agent_record_or_workspace() {
        let temp = tempdir().unwrap();
        let config = test_config(temp.path());
        let result = create(
            &config,
            CreateLogicalAgent {
                name: "invalid-profile",
                owner: "owner-one",
                domain: StateDomain::ClusterOps,
                state_profile: "scientific-worker",
                objective: "inspect one declared target",
                role: "cluster operations subagent",
                harness: HarnessKind::OpenCode,
                thinking: None,
                runtime: RuntimeArgs::default(),
            },
        );
        assert!(result.is_err());
        assert!(!record_path(&config, "invalid-profile").unwrap().exists());
        assert!(!config.root.join("agents/invalid-profile").exists());
    }

    #[test]
    fn provider_segments_are_reserved_before_completion() {
        let temp = tempdir().unwrap();
        let config = test_config(temp.path());
        let mut record = create(
            &config,
            CreateLogicalAgent {
                name: "session-owner",
                owner: "owner-one",
                domain: StateDomain::ClusterOps,
                state_profile: "cluster-operator",
                objective: "inspect one declared target",
                role: "cluster operations subagent",
                harness: HarnessKind::OpenCode,
                thinking: None,
                runtime: RuntimeArgs::default(),
            },
        )
        .unwrap();
        assert_eq!(begin_session(&config, &mut record).unwrap(), 1);
        record_session(&config, &mut record, false, "controller-error").unwrap();
        assert_eq!(begin_session(&config, &mut record).unwrap(), 2);
        let stored = load(&config, "session-owner").unwrap();
        assert_eq!(stored.sessions_started, 2);
        assert_eq!(stored.sessions_completed, 1);
        assert_eq!(stored.failed_sessions, 1);
    }

    fn test_config(root: &std::path::Path) -> RuntimeConfig {
        let config = RuntimeConfig {
            root: root.join("workspaces"),
            state_dir: root.join("state"),
            config_dir: root.join("config"),
            openshell_bin: "/bin/true".into(),
            docker_bin: "/bin/true".into(),
        };
        for directory in [
            &config.root,
            &config.state_dir,
            &config.config_dir,
            &config.audit_dir(),
            &config.continuity_agents_dir(),
            &config.continuity_agent_locks_dir(),
        ] {
            create_private_dir(directory).unwrap();
        }
        config
            .save_settings(&ControllerSettings::default())
            .unwrap();
        config
    }
}
