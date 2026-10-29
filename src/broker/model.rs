use serde::{Deserialize, Serialize};
use time::OffsetDateTime;
use uuid::Uuid;

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum TaskStatus {
    Queued,
    Leased,
    Starting,
    Running,
    Cancelling,
    Completed,
    Failed,
    Cancelled,
    ReviewRequired,
}

impl TaskStatus {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Queued => "queued",
            Self::Leased => "leased",
            Self::Starting => "starting",
            Self::Running => "running",
            Self::Cancelling => "cancelling",
            Self::Completed => "completed",
            Self::Failed => "failed",
            Self::Cancelled => "cancelled",
            Self::ReviewRequired => "review_required",
        }
    }
}

#[derive(Clone, Debug, Serialize)]
pub struct PrincipalView {
    pub id: Uuid,
    pub email: String,
    pub display_name: Option<String>,
}

#[derive(Clone, Debug, Serialize)]
pub struct GroupView {
    pub key: String,
    pub name: String,
    pub role: String,
    pub max_active_agents: i32,
    pub max_active_agents_per_principal: i32,
    pub max_queued_tasks: i32,
    pub retention_days: i32,
    /// The enabled methods granted to this lab (migration 052), so the workspace offers a lab only its own methods.
    pub profiles: Vec<String>,
    /// Whether people may add data to this lab's projects: exactly one enabled lab-data method is granted to it
    /// (migration 061, the rule of migration 059).
    pub lab_data: bool,
}

#[derive(Clone, Debug, Serialize)]
pub struct ProfileView {
    pub key: String,
    pub name: String,
    pub description: String,
    pub tool_bundle: String,
    pub max_run_seconds: i32,
    /// Whether a person may choose the method to run or give to the research agent. The figure method
    /// runs only from a figure request, so it is listed for its name but not offered.
    pub offered: bool,
    /// Whether a research run may list the method (migration 060): the template-analysis method is not offered to run
    /// on its own but a run may hold it; the figure and dataset methods may not.
    pub in_runs: bool,
}

#[derive(Clone, Debug, Serialize)]
pub struct MeView {
    pub principal: PrincipalView,
    pub groups: Vec<GroupView>,
    pub profiles: Vec<ProfileView>,
    pub administration: AdministrationView,
}

/// The identity view with the site's presentation settings. Presentation grants nothing.
#[derive(Clone, Debug, Serialize)]
pub struct MeResponse {
    #[serde(flatten)]
    pub me: MeView,
    pub site: SiteView,
    /// Whether `GET /api/v1/changes/stream` is served; a client opens it only then.
    pub change_stream: bool,
}

/// Names a site shows beside its labs, for example the research center that hosts them.
#[derive(Clone, Debug, Default, Serialize)]
pub struct SiteView {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub organization: Option<String>,
}

/// Separately enrolled human capabilities. Group roles never set these.
#[derive(Clone, Debug, Serialize)]
pub struct AdministrationView {
    pub project_inspection: bool,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct SubmitTaskRequest {
    pub group: String,
    pub profile: String,
    pub objective: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub project_id: Option<Uuid>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub action_key: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_task_id: Option<Uuid>,
}

#[derive(Clone, Debug, Serialize)]
pub struct TaskView {
    pub id: Uuid,
    pub group: String,
    pub profile: String,
    pub status: String,
    pub objective: Option<String>,
    pub attempt: i32,
    pub max_task_attempts: i32,
    pub max_provider_attempts: i32,
    pub logical_agent: Option<String>,
    pub result_class: Option<String>,
    pub error_code: Option<String>,
    #[serde(with = "time::serde::rfc3339")]
    pub submitted_at: OffsetDateTime,
    #[serde(with = "time::serde::rfc3339")]
    pub updated_at: OffsetDateTime,
    #[serde(with = "time::serde::rfc3339::option")]
    pub started_at: Option<OffsetDateTime>,
    #[serde(with = "time::serde::rfc3339::option")]
    pub finished_at: Option<OffsetDateTime>,
}

#[derive(Clone, Debug, Serialize)]
pub struct TaskEventView {
    pub sequence: i64,
    pub kind: String,
    pub state: String,
    pub code: String,
    #[serde(with = "time::serde::rfc3339")]
    pub occurred_at: OffsetDateTime,
}

#[derive(Clone, Debug, Serialize)]
pub struct ScientificHistoryEventView {
    pub sequence: i64,
    pub event_id: Uuid,
    pub segment: i64,
    pub harness: String,
    pub provider: Option<String>,
    pub model: Option<String>,
    pub engine: Option<String>,
    pub parser: Option<String>,
    pub kind: String,
    pub evidence_role: String,
    pub visibility: String,
    pub occurred_at: String,
    pub hash: String,
    pub evidence_count: i64,
}

#[derive(Clone, Debug, Serialize)]
pub struct ScientificHistoryPage {
    pub coverage: String,
    pub attempt: Option<i32>,
    pub lease_generation: Option<i64>,
    pub events: Vec<ScientificHistoryEventView>,
}

#[derive(Clone, Debug, Serialize)]
pub struct ScientificHistoryEvidenceView {
    pub artifact_id: Uuid,
    pub sha256: String,
    pub size_bytes: i64,
    pub media_type: String,
    pub role: String,
    pub relation: String,
}

#[derive(Clone, Debug, Serialize)]
pub struct ScientificHistoryEvidencePage {
    pub attempt: i32,
    pub lease_generation: i64,
    pub evidence: Vec<ScientificHistoryEvidenceView>,
}

#[derive(Clone, Debug, Serialize)]
pub struct ArtifactView {
    pub id: Uuid,
    pub role: String,
    pub label: String,
    pub relative_path: String,
    pub media_type: String,
    pub size_bytes: i64,
    pub sha256: String,
    #[serde(with = "time::serde::rfc3339")]
    pub created_at: OffsetDateTime,
}

/// Which kind of principal an authenticator verified. The authenticator names it;
/// the database refuses a new identity without one and never lets an identity change class.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PrincipalClass {
    Person,
    ServiceAccount,
    /// An internal campaign owner. No authenticator may produce this class.
    Campaign,
}

impl PrincipalClass {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Person => "person",
            Self::ServiceAccount => "service_account",
            Self::Campaign => "campaign",
        }
    }
}

#[derive(Clone, Debug)]
pub struct VerifiedIdentity {
    pub issuer: String,
    pub subject: String,
    pub email: String,
    pub display_name: Option<String>,
    pub class: PrincipalClass,
}

#[derive(Clone, Debug)]
pub struct ClaimedTask {
    pub id: Uuid,
    pub principal_id: Uuid,
    pub group_key: String,
    pub profile_key: String,
    pub objective: String,
    pub logical_agent: String,
    pub lease_generation: i64,
    pub attempt: i32,
    pub max_task_attempts: i32,
    pub max_provider_attempts: i32,
    pub harness: String,
    pub thinking: Option<String>,
    pub route: String,
    pub controller_profile: String,
    pub gateway_profile: String,
    pub image_ref: String,
    pub cpu_limit: String,
    pub memory_limit: String,
    pub state_profile: String,
    pub role: String,
    pub max_run_seconds: i32,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BrokerCatalog {
    pub schema_version: u32,
    pub tenant: TenantSeed,
    #[serde(default)]
    pub groups: Vec<GroupSeed>,
    #[serde(default)]
    pub profiles: Vec<ProfileSeed>,
    #[serde(default)]
    pub grants: Vec<ProfileGrantSeed>,
    #[serde(default)]
    pub invitations: Vec<InvitationSeed>,
    #[serde(default)]
    pub capabilities: Vec<CapabilityGrantSeed>,
    /// Named, versioned sets of methods. A stored version never changes.
    #[serde(default)]
    pub tool_groups: Vec<ToolGroupSeed>,
    /// Tool group versions granted to labs; each adds its methods to the lab's profile grants.
    #[serde(default)]
    pub tool_group_grants: Vec<ToolGroupGrantSeed>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TenantSeed {
    pub key: String,
    pub name: String,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GroupSeed {
    pub key: String,
    pub name: String,
    pub max_active_agents: i32,
    pub max_active_agents_per_principal: i32,
    pub max_queued_tasks: i32,
    pub retention_days: i32,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProfileSeed {
    pub key: String,
    pub name: String,
    pub description: String,
    pub harness: String,
    pub thinking: Option<String>,
    pub route: String,
    pub controller_profile: String,
    pub gateway_profile: String,
    pub image_ref: String,
    pub cpu_limit: String,
    pub memory_limit: String,
    pub state_profile: String,
    pub role: String,
    pub tool_bundle: String,
    pub max_run_seconds: i32,
    pub max_task_attempts: i32,
    pub max_provider_attempts: i32,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProfileGrantSeed {
    pub group: String,
    pub profile: String,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ToolGroupSeed {
    pub key: String,
    pub version: i32,
    pub title: String,
    pub description: String,
    /// The profile keys of the methods in this version.
    pub tools: Vec<String>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ToolGroupGrantSeed {
    /// The lab.
    pub group: String,
    pub tool_group: String,
    pub version: i32,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InvitationSeed {
    pub email: String,
    pub group: String,
    pub role: String,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CapabilityGrantSeed {
    pub email: String,
    pub group: String,
    pub capability: String,
}

#[cfg(test)]
mod timestamp_tests {
    use super::*;

    #[test]
    fn public_timestamps_are_rfc3339_strings_with_nullable_lifecycle_fields() {
        let at = time::macros::datetime!(2026-09-22 23:00:00 UTC);
        let task = TaskView {
            id: Uuid::nil(),
            group: "example".into(),
            profile: "reference".into(),
            status: "completed".into(),
            objective: None,
            attempt: 1,
            max_task_attempts: 1,
            max_provider_attempts: 1,
            logical_agent: None,
            result_class: None,
            error_code: None,
            submitted_at: at,
            updated_at: at,
            started_at: Some(at),
            finished_at: None,
        };
        let json = serde_json::to_value(task).unwrap();
        for field in ["submitted_at", "updated_at", "started_at"] {
            assert_eq!(json[field], "2026-09-22T23:00:00Z");
        }
        assert!(json["finished_at"].is_null());
        let event = TaskEventView {
            sequence: 1,
            kind: "task".into(),
            state: "completed".into(),
            code: "completed".into(),
            occurred_at: at,
        };
        assert_eq!(
            serde_json::to_value(event).unwrap()["occurred_at"],
            "2026-09-22T23:00:00Z"
        );
        let artifact = ArtifactView {
            id: Uuid::nil(),
            role: "output".into(),
            label: "Result".into(),
            relative_path: "result.txt".into(),
            media_type: "text/plain".into(),
            size_bytes: 0,
            sha256: "a".repeat(64),
            created_at: at,
        };
        assert_eq!(
            serde_json::to_value(artifact).unwrap()["created_at"],
            "2026-09-22T23:00:00Z"
        );
    }
}
