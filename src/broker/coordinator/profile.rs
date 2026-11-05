//! The model profile: what one served model is, the route and request settings each model call role uses with it,
//! and what is known about where it goes wrong. It is read once at start (`--model-profile-file`), and every model
//! call's receipt names it (`profile`, `profile_sha256`), so a turn record names the profile that shaped each call.
//! Without a profile, or for a role it does not set, a call uses the model configuration's route and settings, and its
//! request body is byte-identical to one sent without a profile.
use super::*;

/// The largest profile file read.
const PROFILE_BYTES: u64 = 16384;

/// The model call roles a profile may set.
pub(super) const PROFILE_ROLES: [&str; 6] = [
    "lead",
    "methods_specialist",
    "literature_specialist",
    "literature_review",
    "orientation",
    "working_memory",
];

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ProfileFile {
    schema_version: u32,
    name: String,
    model_identity: ModelIdentity,
    #[serde(default)]
    roles: BTreeMap<String, RoleSettings>,
    #[serde(default)]
    known_weaknesses: Vec<Weakness>,
    #[serde(default)]
    evidence: Evidence,
}

/// The served model the profile describes. Recorded through the profile's digest; the context window must equal the
/// model configuration's, so a profile written for another model is refused.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ModelIdentity {
    name: String,
    #[serde(default)]
    quant: Option<String>,
    #[serde(default)]
    engine: Option<String>,
    #[serde(default)]
    engine_version: Option<String>,
    context_window_tokens: usize,
}

/// One role's route (a model name the gateway serves) and request settings. A role's settings replace the model
/// configuration's settings for that role; they are not merged.
#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct RoleSettings {
    #[serde(default)]
    pub(super) route: Option<String>,
    #[serde(default)]
    pub(super) settings: Option<serde_json::Map<String, Value>>,
}

/// A suspected or measured failure mechanism of this model, kept as a record for the profile's trials.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Weakness {
    mechanism: String,
    source: String,
    status: String,
}

#[derive(Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct Evidence {
    #[serde(default)]
    rounds: Vec<String>,
    #[serde(default)]
    lineage_parent: Option<String>,
}

pub(crate) struct Profile {
    pub(super) name: String,
    pub(super) sha256: String,
    roles: BTreeMap<String, RoleSettings>,
}

/// A profile name: lowercase letters, digits, dots and hyphens, starting with a letter, at most 64 bytes.
fn profile_name(value: &str) -> bool {
    (2..=64).contains(&value.len())
        && value.starts_with(|c: char| c.is_ascii_lowercase())
        && value
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'.' || b == b'-')
}

/// A route: a model name the gateway serves, as the model configuration's `model` is checked, without spaces.
fn route_name(value: &str) -> bool {
    clean_text(value, 128)
        && !value.contains(char::is_whitespace)
        && !value.chars().any(char::is_control)
}

impl Profile {
    /// Reads and checks a profile for this model configuration. Any fault refuses start with `model_profile_invalid`.
    pub(super) fn load(path: &PathBuf, config: &ModelConfig) -> Result<Self> {
        let bytes = private_bytes(path, PROFILE_BYTES)?;
        let file: ProfileFile =
            serde_json::from_slice(&bytes).map_err(|_| anyhow!("model_profile_invalid"))?;
        let identity = &file.model_identity;
        let short = |v: &Option<String>| v.as_deref().is_none_or(|s| clean_text(s, 64));
        let valid = file.schema_version == 1
            && profile_name(&file.name)
            && clean_text(&identity.name, 128)
            && short(&identity.quant)
            && short(&identity.engine)
            && short(&identity.engine_version)
            && identity.context_window_tokens == config.context_window_tokens
            && file.roles.iter().all(|(role, set)| {
                PROFILE_ROLES.contains(&role.as_str())
                    && (set.route.is_some() || set.settings.is_some())
                    && set.route.as_deref().is_none_or(route_name)
                    && set.settings.as_ref().is_none_or(settings_valid)
            })
            && file.known_weaknesses.len() <= 16
            && file.known_weaknesses.iter().all(|w| {
                fixed_code(&w.mechanism)
                    && clean_text(&w.source, 200)
                    && matches!(w.status.as_str(), "hypothesis" | "measured" | "retired")
            })
            && file.evidence.rounds.len() <= 32
            && file.evidence.rounds.iter().all(|r| clean_text(r, 200))
            && file
                .evidence
                .lineage_parent
                .as_deref()
                .is_none_or(profile_name);
        if !valid {
            bail!("model_profile_invalid")
        }
        Ok(Self {
            name: file.name,
            sha256: sha(&bytes),
            roles: file.roles,
        })
    }

    /// The route and settings a role's calls use: the profile's where it sets them, else the model configuration's.
    pub(super) fn route_and_settings<'a>(
        profile: Option<&'a Self>,
        config: &'a ModelConfig,
        role: &str,
    ) -> (&'a str, &'a serde_json::Map<String, Value>) {
        let set = profile.and_then(|p| p.roles.get(role));
        (
            set.and_then(|s| s.route.as_deref())
                .unwrap_or(&config.model),
            set.and_then(|s| s.settings.as_ref())
                .unwrap_or(&config.settings),
        )
    }
}
