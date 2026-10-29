//! The agent-state records: domains, runs, collections, events, artifacts, views and summaries.
use super::*;

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq, ValueEnum)]
#[serde(rename_all = "kebab-case")]
pub enum StateDomain {
    Generic,
    Scientific,
    ClusterOps,
    SoftwareEngineering,
}

impl StateDomain {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Generic => "generic",
            Self::Scientific => "scientific",
            Self::ClusterOps => "cluster-ops",
            Self::SoftwareEngineering => "software-engineering",
        }
    }
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize, PartialEq, Eq, ValueEnum)]
#[serde(rename_all = "kebab-case")]
pub enum StateAuthority {
    Machine,
    Model,
    #[default]
    Operator,
}

impl StateAuthority {
    pub(super) fn as_str(self) -> &'static str {
        match self {
            Self::Machine => "machine",
            Self::Model => "model",
            Self::Operator => "operator",
        }
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq, PartialOrd, Ord, ValueEnum)]
#[serde(rename_all = "kebab-case")]
pub enum StateAudience {
    Agent,
    Team,
    Lead,
    Oversight,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum ReasoningRetention {
    None,
    Structured,
    ReturnedTrace,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize, PartialEq, Eq, ValueEnum)]
#[serde(rename_all = "kebab-case")]
pub enum ArtifactSensitivity {
    #[default]
    Standard,
    Restricted,
}

impl ArtifactSensitivity {
    pub(super) fn as_str(self) -> &'static str {
        match self {
            Self::Standard => "standard",
            Self::Restricted => "restricted",
        }
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct StateCollectionRule {
    pub name: String,
    pub priority: u32,
    pub max_bytes: usize,
    pub pointers: Vec<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct StateViewProfile {
    pub schema_version: u32,
    pub name: String,
    pub audience: StateAudience,
    pub reasoning_retention: ReasoningRetention,
    pub max_state_tokens: u64,
    pub latest_event_count: usize,
    pub soft_reset_ratio: f64,
    pub hard_reset_ratio: f64,
    pub collections: Vec<StateCollectionRule>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct CollectionAccess {
    pub audience: StateAudience,
    #[serde(default)]
    pub principals: Vec<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct StateRun {
    pub schema_version: u32,
    pub run_id: String,
    pub domain: StateDomain,
    pub profile: String,
    pub subject: String,
    pub team: Option<String>,
    pub status: String,
    pub epoch: u64,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct StateCollection {
    pub schema_version: u32,
    pub run_id: String,
    pub subject: String,
    pub name: String,
    pub document_schema: String,
    pub version: u64,
    pub authorities: Vec<StateAuthority>,
    pub access: Vec<CollectionAccess>,
    pub prompt_eligible: bool,
    pub content_sha256: String,
    pub value: Value,
    pub updated_at: String,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct DecisionRationale {
    pub summary: String,
    #[serde(default)]
    pub alternatives: Vec<String>,
    #[serde(default)]
    pub evidence: Vec<String>,
    pub confidence: Option<f64>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct StateMutation {
    pub schema_version: u32,
    pub run_id: String,
    pub subject: String,
    pub collection: String,
    pub base_version: u64,
    #[serde(default)]
    pub set: BTreeMap<String, Value>,
    #[serde(default)]
    pub remove: Vec<String>,
    pub rationale: Option<DecisionRationale>,
}

#[derive(Clone, Debug, Serialize)]
pub struct StateMutationReceipt {
    pub run_id: String,
    pub subject: String,
    pub collection: String,
    pub previous_version: u64,
    pub version: u64,
    pub event_id: String,
    pub content_sha256: String,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct StateEvent {
    pub sequence: u64,
    pub event_id: String,
    pub run_id: String,
    pub subject: String,
    pub epoch: u64,
    pub kind: String,
    pub actor: String,
    pub authority: StateAuthority,
    pub occurred_at: String,
    pub payload: Value,
    pub payload_sha256: String,
    pub previous_hash: String,
    pub hash: String,
}

pub(super) struct EventDraft<'a> {
    pub(super) run_id: &'a str,
    pub(super) subject: &'a str,
    pub(super) epoch: u64,
    pub(super) kind: &'a str,
    pub(super) actor: &'a str,
    pub(super) authority: StateAuthority,
    pub(super) payload: Value,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ArtifactRecord {
    pub schema_version: u32,
    pub sha256: String,
    pub size: u64,
    pub media_type: String,
    pub role: String,
    pub label: String,
    pub sensitivity: ArtifactSensitivity,
    pub relative_path: String,
    pub created_at: String,
}

pub(super) struct ExportArtifactAggregate {
    pub(super) artifact: ArtifactRecord,
    pub(super) has_standard_link: bool,
    pub(super) has_restricted_link: bool,
    pub(super) labels: BTreeSet<String>,
    pub(super) roles: BTreeSet<String>,
    pub(super) event_ids: BTreeSet<String>,
}

#[derive(Clone, Debug, Serialize)]
pub struct StateViewCollection {
    pub version: u64,
    pub content_sha256: String,
    pub value: Value,
}

#[derive(Clone, Debug, Serialize)]
pub struct StateViewEvent {
    pub sequence: u64,
    pub event_id: String,
    pub epoch: u64,
    pub kind: String,
    pub occurred_at: String,
    pub hash: String,
}

#[derive(Clone, Debug, Serialize)]
pub struct StateViewOmission {
    pub collection: String,
    pub reason: String,
}

#[derive(Clone, Debug, Serialize)]
pub struct StateView {
    pub schema_version: u32,
    pub run_id: String,
    pub subject: String,
    pub domain: StateDomain,
    pub profile: String,
    pub audience: StateAudience,
    pub epoch: u64,
    pub reasoning_retention: ReasoningRetention,
    pub estimated_tokens: u64,
    pub max_state_tokens: u64,
    pub collections: BTreeMap<String, StateViewCollection>,
    pub latest_events: Vec<StateViewEvent>,
    pub omitted: Vec<StateViewOmission>,
    pub generated_at: String,
}

#[derive(Clone, Copy, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum ContextAction {
    Continue,
    Checkpoint,
    Reset,
}

#[derive(Clone, Debug, Serialize)]
pub struct ContextStatus {
    pub window_tokens: u64,
    pub used_tokens: u64,
    pub output_reserve_tokens: u64,
    pub pressure_ratio: f64,
    pub soft_reset_ratio: f64,
    pub hard_reset_ratio: f64,
    pub action: ContextAction,
}

#[derive(Clone, Debug, Serialize)]
pub struct StateCheckpoint {
    pub run_id: String,
    pub subject: String,
    pub previous_epoch: u64,
    pub epoch: u64,
    pub state_sha256: String,
    pub event_head: String,
    pub event_id: String,
    pub reason: String,
    pub created_at: String,
}

#[derive(Clone, Debug, Serialize)]
pub struct StateRunSummary {
    pub run_id: String,
    pub domain: StateDomain,
    pub profile: String,
    pub subject: String,
    pub team: Option<String>,
    pub status: String,
    pub epoch: u64,
    pub collections: u64,
    pub events: u64,
    pub artifacts: u64,
    pub updated_at: String,
}

#[derive(Clone, Debug, Serialize)]
pub struct StateTraceCollectionSummary {
    pub name: String,
    pub document_schema: String,
    pub version: u64,
    pub authorities: Vec<StateAuthority>,
    pub audiences: Vec<StateAudience>,
    pub principal_scoped: bool,
    pub prompt_eligible: bool,
    pub content_sha256: String,
    pub updated_at: String,
}

#[derive(Clone, Debug, Serialize)]
pub struct StateTraceEventSummary {
    pub sequence: u64,
    pub event_id: String,
    pub epoch: u64,
    pub kind: String,
    pub actor: String,
    pub authority: StateAuthority,
    pub occurred_at: String,
    pub payload_sha256: String,
    pub previous_hash: String,
    pub hash: String,
}

#[derive(Clone, Debug, Serialize)]
pub struct StateTraceArtifactSummary {
    pub event_id: String,
    pub sha256: String,
    pub size: u64,
    pub media_type: String,
    pub role: String,
    pub label: String,
    pub sensitivity: ArtifactSensitivity,
    pub created_at: String,
}

#[derive(Clone, Debug, Serialize)]
pub struct StateTraceSummary {
    pub run_id: String,
    pub subject: String,
    pub status: String,
    pub epoch: u64,
    pub collections: Vec<StateTraceCollectionSummary>,
    pub events: Vec<StateTraceEventSummary>,
    pub events_total: u64,
    pub events_truncated: bool,
    pub artifacts: Vec<StateTraceArtifactSummary>,
    pub artifacts_total: u64,
    pub artifacts_truncated: bool,
    pub payloads_included: bool,
    pub state_values_included: bool,
}

#[derive(Clone, Debug, Serialize)]
pub struct StateMetadataSearchHit {
    pub id: String,
    pub target_id: String,
    pub category: String,
    pub run_id: String,
    pub title: String,
    pub detail: String,
    pub occurred_at: String,
    pub digest: Option<String>,
}

#[derive(Clone, Debug, Serialize)]
pub struct StateMetadataSearchPage {
    pub totals: BTreeMap<String, u64>,
    pub hits: Vec<StateMetadataSearchHit>,
}

#[derive(Clone, Debug, Serialize)]
pub struct StateVerification {
    pub database: PathBuf,
    pub runs: u64,
    pub collections: u64,
    pub events: u64,
    pub artifacts: u64,
    pub event_chains_valid: bool,
    pub schemas_valid: bool,
    pub artifacts_valid: bool,
}

#[derive(Clone, Debug, Serialize)]
pub struct ReconcileOutcome {
    pub run_id: String,
    pub subject: String,
    pub accepted: usize,
    pub rejected: usize,
    pub view_path: PathBuf,
}

#[derive(Clone, Debug, Serialize)]
pub struct RunArtifactLink {
    pub event_id: String,
    pub artifact: ArtifactRecord,
}

#[derive(Clone, Debug, Serialize)]
pub struct StateExportReceipt {
    pub run_id: String,
    pub output_dir: PathBuf,
    pub events: usize,
    pub artifact_links: usize,
    pub copied_artifacts: usize,
    pub restricted_artifacts_omitted: usize,
    pub event_payloads_included: bool,
    pub manifest_sha256: String,
}
