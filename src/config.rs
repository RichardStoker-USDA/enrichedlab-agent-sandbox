use std::{
    collections::BTreeMap,
    env, fs,
    path::{Path, PathBuf},
};

use anyhow::{Context, Result, bail};
use clap::ValueEnum;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::{
    cli::Cli,
    platform::{GatewayProfile, default_gateway_profiles},
};

pub const APP_NAME: &str = "enrichedlab-agent-sandbox";

#[derive(Clone, Debug)]
pub struct RuntimeConfig {
    pub root: PathBuf,
    pub state_dir: PathBuf,
    pub config_dir: PathBuf,
    pub openshell_bin: PathBuf,
    pub docker_bin: PathBuf,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ControllerSettings {
    pub schema_version: u32,
    pub dashboard_bind: String,
    pub default_profile: String,
    pub default_route: String,
    #[serde(default = "default_git_committer_name")]
    pub git_committer_name: String,
    #[serde(default = "default_git_committer_email")]
    pub git_committer_email: String,
    pub custom_auto_approve_loopback_services: bool,
    pub custom_auto_approve_catalog_entries: bool,
    pub require_hard_landlock: bool,
    pub ocsf_json_enabled: bool,
    pub policy_advisor_enabled: bool,
    #[serde(default = "default_gateway_profile")]
    pub default_gateway_profile: String,
    #[serde(default = "default_gateway_profiles")]
    pub gateway_profiles: BTreeMap<String, GatewayProfile>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub runtime_adapter: Option<String>,
    #[serde(default)]
    pub provenance: ProvenanceSettings,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize, PartialEq, Eq, ValueEnum)]
#[serde(rename_all = "kebab-case")]
pub enum ProvenanceBackend {
    #[default]
    Embedded,
    Postgres,
}

impl ProvenanceBackend {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Embedded => "embedded",
            Self::Postgres => "postgres",
        }
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ProvenanceSettings {
    #[serde(default)]
    pub backend: ProvenanceBackend,
    #[serde(default = "default_tenant_id")]
    pub tenant_id: String,
    #[serde(default = "default_postgres_dsn_env")]
    pub postgres_dsn_env: String,
    #[serde(default = "default_postgres_migration_dsn_env")]
    pub postgres_migration_dsn_env: String,
    #[serde(default = "default_evidence_lease_seconds")]
    pub evidence_lease_seconds: u64,
    #[serde(default = "default_evidence_preview_bytes")]
    pub evidence_preview_bytes: usize,
}

impl Default for ProvenanceSettings {
    fn default() -> Self {
        Self {
            backend: ProvenanceBackend::Embedded,
            tenant_id: default_tenant_id(),
            postgres_dsn_env: default_postgres_dsn_env(),
            postgres_migration_dsn_env: default_postgres_migration_dsn_env(),
            evidence_lease_seconds: default_evidence_lease_seconds(),
            evidence_preview_bytes: default_evidence_preview_bytes(),
        }
    }
}

impl ProvenanceSettings {
    fn validate(&self) -> Result<()> {
        validate_identifier(&self.tenant_id, "provenance tenant_id", 64)?;
        validate_identifier(&self.postgres_dsn_env, "postgres_dsn_env", 128)?;
        validate_identifier(
            &self.postgres_migration_dsn_env,
            "postgres_migration_dsn_env",
            128,
        )?;
        if !(60..=3_600).contains(&self.evidence_lease_seconds) {
            bail!("evidence_lease_seconds must be between 60 and 3600");
        }
        if !(1_024..=1_048_576).contains(&self.evidence_preview_bytes) {
            bail!("evidence_preview_bytes must be between 1024 and 1048576");
        }
        Ok(())
    }
}

impl Default for ControllerSettings {
    fn default() -> Self {
        Self {
            schema_version: 3,
            dashboard_bind: "127.0.0.1:32100".into(),
            default_profile: "development".into(),
            default_route: "none".into(),
            git_committer_name: default_git_committer_name(),
            git_committer_email: default_git_committer_email(),
            custom_auto_approve_loopback_services: false,
            custom_auto_approve_catalog_entries: false,
            require_hard_landlock: true,
            ocsf_json_enabled: true,
            policy_advisor_enabled: true,
            default_gateway_profile: default_gateway_profile(),
            gateway_profiles: default_gateway_profiles(),
            runtime_adapter: None,
            provenance: ProvenanceSettings::default(),
        }
    }
}

impl ControllerSettings {
    pub fn validate(&self) -> Result<()> {
        if !(1..=3).contains(&self.schema_version) {
            bail!(
                "unsupported controller settings schema {}",
                self.schema_version
            );
        }
        let bind: std::net::SocketAddr = self
            .dashboard_bind
            .parse()
            .context("dashboard_bind must be an IP socket address")?;
        if !bind.ip().is_loopback() {
            bail!("dashboard_bind must remain on loopback");
        }
        if self.default_profile.trim().is_empty() || self.default_route.trim().is_empty() {
            bail!("default profile and route cannot be empty");
        }
        validate_git_identity(&self.git_committer_name, "git_committer_name")?;
        validate_git_identity(&self.git_committer_email, "git_committer_email")?;
        if !self.git_committer_email.contains('@') {
            bail!("git_committer_email must contain @");
        }
        if self.gateway_profiles.is_empty() {
            bail!("at least one gateway profile is required");
        }
        for (name, profile) in &self.gateway_profiles {
            profile.validate(name)?;
        }
        let default = self
            .gateway_profiles
            .get(&self.default_gateway_profile)
            .with_context(|| {
                format!(
                    "default gateway profile {} is not configured",
                    self.default_gateway_profile
                )
            })?;
        if !default.enabled {
            bail!("default gateway profile must be enabled");
        }
        if self.schema_version == 1 && self.runtime_adapter.as_deref() != Some("docker") {
            bail!("legacy controller settings must declare runtime_adapter = docker");
        }
        if self.schema_version >= 2 && self.runtime_adapter.is_some() {
            bail!("runtime_adapter is replaced by named gateway_profiles in schema 2");
        }
        self.provenance.validate()?;
        Ok(())
    }

    pub fn gateway_profile(&self, name: &str) -> Result<&GatewayProfile> {
        let profile = self
            .gateway_profiles
            .get(name)
            .with_context(|| format!("unknown gateway profile {name}"))?;
        if !profile.enabled {
            bail!("gateway profile {name} is disabled");
        }
        Ok(profile)
    }
}

fn default_gateway_profile() -> String {
    "docker-local".into()
}

fn default_tenant_id() -> String {
    "local".into()
}

fn default_postgres_dsn_env() -> String {
    "AGENT_SANDBOX_POSTGRES_DSN".into()
}

fn default_postgres_migration_dsn_env() -> String {
    "AGENT_SANDBOX_POSTGRES_MIGRATION_DSN".into()
}

fn default_evidence_lease_seconds() -> u64 {
    300
}

fn default_evidence_preview_bytes() -> usize {
    64 * 1024
}

fn default_git_committer_name() -> String {
    "agent-sandbox host integrator".into()
}

fn default_git_committer_email() -> String {
    "agent-sandbox@localhost.invalid".into()
}

fn validate_git_identity(value: &str, field: &str) -> Result<()> {
    if value.trim().is_empty()
        || value.len() > 128
        || value.chars().any(|character| {
            character == '\0' || character == '\n' || character == '\r' || character.is_control()
        })
    {
        bail!("{field} is invalid");
    }
    Ok(())
}

fn validate_identifier(value: &str, field: &str, max_bytes: usize) -> Result<()> {
    if value.is_empty()
        || value.len() > max_bytes
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-' | b'.'))
    {
        bail!("{field} must be a bounded alphanumeric identifier");
    }
    Ok(())
}

impl RuntimeConfig {
    pub fn resolve(cli: &Cli) -> Result<Self> {
        let home = env::var_os("HOME")
            .map(PathBuf::from)
            .context("HOME is not set")?;
        let root = cli
            .root
            .clone()
            .or_else(|| env::var_os("AGENT_SANDBOX_ROOT").map(PathBuf::from))
            .unwrap_or_else(|| home.join("agent-sandboxes"));
        let state_dir = cli
            .state_dir
            .clone()
            .or_else(|| env::var_os("AGENT_SANDBOX_STATE_DIR").map(PathBuf::from))
            .or_else(|| {
                env::var_os("XDG_STATE_HOME").map(|path| PathBuf::from(path).join(APP_NAME))
            })
            .unwrap_or_else(|| home.join(".local/state").join(APP_NAME));
        let config_dir = cli
            .config_dir
            .clone()
            .or_else(|| env::var_os("AGENT_SANDBOX_CONFIG_DIR").map(PathBuf::from))
            .or_else(|| {
                env::var_os("XDG_CONFIG_HOME").map(|path| PathBuf::from(path).join(APP_NAME))
            })
            .unwrap_or_else(|| home.join(".config").join(APP_NAME));

        for path in [&root, &state_dir, &config_dir] {
            if !path.is_absolute() {
                bail!("controller paths must be absolute: {}", path.display());
            }
            create_private_dir(path)?;
        }

        let config = Self {
            root,
            state_dir,
            config_dir,
            openshell_bin: cli.openshell_bin.clone(),
            docker_bin: cli.docker_bin.clone(),
        };
        for directory in [
            config.records_dir(),
            config.policies_dir(),
            config.controls_dir(),
            config.leases_dir(),
            config.teams_dir(),
            config.research_dir(),
            config.allocation_reservations_dir(),
            config.continuity_agents_dir(),
            config.continuity_agent_locks_dir(),
            config.catalog_dir(),
            config.audit_dir(),
            config.evidence_dir(),
            config.pki_dir(),
            config.logs_dir(),
            config.runtime_dir(),
            config.runtime_proofs_dir(),
            config.peer_runtime_dir(),
            config.sync_dir(),
            config.sync_backups_dir(),
            config.agent_state_dir(),
            config.agent_state_artifacts_dir(),
            config.provenance_artifacts_dir(),
            config.state_profiles_dir(),
            config.state_schemas_dir(),
        ] {
            create_private_dir(&directory)?;
        }
        if !config.settings_path().exists() {
            config.save_settings(&ControllerSettings::default())?;
        }
        let mut settings = config.load_settings()?;
        if settings.schema_version < 3 {
            settings.schema_version = 3;
            settings.runtime_adapter = None;
            config.save_settings(&settings)?;
        }
        Ok(config)
    }

    pub fn records_dir(&self) -> PathBuf {
        self.state_dir.join("records")
    }
    pub fn policies_dir(&self) -> PathBuf {
        self.state_dir.join("policies")
    }
    pub fn controls_dir(&self) -> PathBuf {
        self.state_dir.join("controls")
    }
    pub fn leases_dir(&self) -> PathBuf {
        self.state_dir.join("leases")
    }
    pub fn teams_dir(&self) -> PathBuf {
        self.state_dir.join("teams")
    }
    pub fn research_dir(&self) -> PathBuf {
        self.state_dir.join("research")
    }
    pub fn allocation_reservations_dir(&self) -> PathBuf {
        self.state_dir.join("allocation-reservations")
    }
    pub fn continuity_agents_dir(&self) -> PathBuf {
        self.state_dir.join("logical-agents")
    }
    pub fn continuity_agent_locks_dir(&self) -> PathBuf {
        self.continuity_agents_dir().join("locks")
    }
    pub fn catalog_dir(&self) -> PathBuf {
        self.state_dir.join("catalog")
    }
    pub fn audit_dir(&self) -> PathBuf {
        self.state_dir.join("audit")
    }
    pub fn evidence_dir(&self) -> PathBuf {
        self.state_dir.join("evidence")
    }
    pub fn pki_dir(&self) -> PathBuf {
        self.state_dir.join("pki")
    }
    pub fn logs_dir(&self) -> PathBuf {
        self.state_dir.join("logs")
    }
    pub fn runtime_dir(&self) -> PathBuf {
        self.state_dir.join("runtime")
    }
    pub fn runtime_proofs_dir(&self) -> PathBuf {
        self.runtime_dir().join("proofs")
    }
    pub fn peer_runtime_dir(&self) -> PathBuf {
        self.runtime_dir().join("peers")
    }
    pub fn sync_dir(&self) -> PathBuf {
        self.state_dir.join("sync")
    }
    pub fn sync_backups_dir(&self) -> PathBuf {
        self.state_dir.join("sync-backups")
    }
    pub fn agent_state_dir(&self) -> PathBuf {
        self.state_dir.join("agent-state")
    }
    pub fn agent_state_database_path(&self) -> PathBuf {
        self.agent_state_dir().join("state.sqlite3")
    }
    pub fn agent_state_artifacts_dir(&self) -> PathBuf {
        self.agent_state_dir().join("artifacts")
    }
    pub fn provenance_database_path(&self) -> PathBuf {
        self.agent_state_dir().join("provenance.sqlite3")
    }
    pub fn provenance_artifacts_dir(&self) -> PathBuf {
        self.agent_state_dir().join("provenance-artifacts")
    }
    pub fn state_profiles_dir(&self) -> PathBuf {
        self.config_dir.join("state-profiles")
    }
    pub fn state_schemas_dir(&self) -> PathBuf {
        self.config_dir.join("state-schemas")
    }
    pub fn settings_path(&self) -> PathBuf {
        self.config_dir.join("controller.toml")
    }
    pub fn routes_path(&self) -> PathBuf {
        self.config_dir.join("routes.json")
    }

    pub fn load_settings(&self) -> Result<ControllerSettings> {
        let path = self.settings_path();
        let body = fs::read_to_string(&path)
            .with_context(|| format!("failed to read {}", path.display()))?;
        let settings: ControllerSettings = toml::from_str(&body)
            .with_context(|| format!("invalid settings {}", path.display()))?;
        settings.validate()?;
        Ok(settings)
    }

    pub fn save_settings(&self, settings: &ControllerSettings) -> Result<()> {
        settings.validate()?;
        let body = toml::to_string_pretty(settings)?;
        write_private(&self.settings_path(), body.as_bytes())
    }
}

pub fn create_private_dir(path: &Path) -> Result<()> {
    fs::create_dir_all(path).with_context(|| format!("failed to create {}", path.display()))?;
    set_mode(path, 0o700)
}

pub fn write_private(path: &Path, body: &[u8]) -> Result<()> {
    if let Some(parent) = path.parent() {
        create_private_dir(parent)?;
    }
    let file_name = path
        .file_name()
        .and_then(|name| name.to_str())
        .context("invalid file name")?;
    let pending = path.with_file_name(format!(".{file_name}.{}.pending", Uuid::new_v4()));
    let mut options = fs::OpenOptions::new();
    options.create(true).truncate(true).write(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options
        .open(&pending)
        .with_context(|| format!("failed to write {}", pending.display()))?;
    use std::io::Write;
    file.write_all(body)?;
    file.sync_all()?;
    set_mode(&pending, 0o600)?;
    fs::rename(&pending, path)?;
    Ok(())
}

pub fn set_mode(path: &Path, mode: u32) -> Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(mode))
            .with_context(|| format!("failed to set permissions on {}", path.display()))?;
    }
    #[cfg(not(unix))]
    let _ = (path, mode);
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::{fs, sync::Arc, thread};

    use tempfile::tempdir;

    use super::{ControllerSettings, write_private};

    #[test]
    fn default_git_identity_is_valid_and_private() {
        let settings = ControllerSettings::default();
        settings.validate().unwrap();
        assert!(settings.git_committer_email.ends_with(".invalid"));
    }

    #[test]
    fn concurrent_private_writes_use_distinct_atomic_paths() {
        let temp = tempdir().unwrap();
        let path = Arc::new(temp.path().join("view.json"));
        let bodies = (0..8)
            .map(|index| {
                format!(
                    "{{\"writer\":{index},\"padding\":\"{}\"}}\n",
                    "x".repeat(512)
                )
            })
            .collect::<Vec<_>>();
        thread::scope(|scope| {
            let handles = bodies
                .iter()
                .map(|body| {
                    let path = Arc::clone(&path);
                    scope.spawn(move || {
                        for _ in 0..25 {
                            write_private(&path, body.as_bytes()).unwrap();
                        }
                    })
                })
                .collect::<Vec<_>>();
            for handle in handles {
                handle.join().unwrap();
            }
        });
        let final_body = fs::read_to_string(path.as_ref()).unwrap();
        assert!(bodies.contains(&final_body));
        let pending = fs::read_dir(temp.path())
            .unwrap()
            .filter_map(|entry| entry.ok())
            .filter(|entry| entry.file_name().to_string_lossy().contains(".pending"))
            .count();
        assert_eq!(pending, 0);
    }

    #[test]
    fn git_identity_rejects_line_controls() {
        let settings = ControllerSettings {
            git_committer_name: "host\nattacker".into(),
            ..ControllerSettings::default()
        };
        assert!(settings.validate().is_err());
    }

    #[test]
    fn shipped_controller_example_matches_current_schema() {
        let settings: ControllerSettings =
            toml::from_str(include_str!("../examples/controller.toml")).unwrap();
        settings.validate().unwrap();
        assert!(settings.gateway_profiles.contains_key("docker-local"));
        assert!(settings.gateway_profiles.contains_key("vm-local"));
        assert!(settings.gateway_profiles.contains_key("kubernetes-kata"));
    }
}
