use serde::{Deserialize, Serialize};
use serde_json::Value;
use uuid::Uuid;

use super::{CampaignBudgets, OutcomeClass, RunOutcome, StopRules};

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct CampaignView {
    pub campaign_id: Uuid,
    pub group_key: String,
    pub goal: String,
    #[serde(default)]
    pub constraints: String,
    #[serde(default)]
    pub text_origin: String,
    pub scope: Value,
    #[serde(default)]
    pub methods: Vec<MethodView>,
    pub budgets: CampaignBudgets,
    #[serde(default)]
    pub budgets_left: Value,
    pub stop_rules: StopRules,
    pub state: String,
    #[serde(default)]
    pub state_reason: Option<String>,
    pub generation: i64,
    #[serde(default)]
    pub run_count: u32,
    #[serde(default)]
    pub active_runs: u32,
    #[serde(default)]
    pub finished_steps: u32,
    #[serde(default)]
    pub evidence_steps: u32,
    #[serde(default)]
    pub planner_refused_cycles: u32,
    #[serde(default)]
    pub branches: Vec<BranchView>,
    #[serde(default)]
    pub steps: Vec<StepView>,
    #[serde(default)]
    pub records: Vec<RecordView>,
    #[serde(default)]
    pub records_omitted: usize,
    #[serde(default)]
    pub relations: Vec<RelationView>,
    #[serde(default)]
    pub open_requests: Vec<RecordView>,
    #[serde(default)]
    pub rule_notices: Vec<Value>,
    #[serde(default)]
    pub rule_notices_omitted: usize,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct MethodView {
    pub key: String,
    pub name: String,
    pub description: String,
    #[serde(default)]
    pub data_sources: Vec<String>,
}

impl CampaignView {
    pub fn profiles(&self) -> Vec<String> {
        self.scope
            .get("profiles")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
            .map(str::to_owned)
            .collect()
    }

    pub fn free_concurrency(&self) -> usize {
        usize::from(self.budgets.max_concurrent_runs).saturating_sub(self.active_runs as usize)
    }

    pub fn runs_left(&self) -> usize {
        usize::from(self.budgets.max_runs).saturating_sub(self.run_count as usize)
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct BranchView {
    pub branch_id: Uuid,
    #[serde(default, rename = "parent_id")]
    pub parent_branch_id: Option<Uuid>,
    pub direction: String,
    pub rationale: String,
    #[serde(default)]
    pub text_origin: String,
    pub state: String,
    #[serde(default)]
    pub reason_class: Option<String>,
    #[serde(default)]
    pub reason: Option<String>,
    #[serde(default)]
    pub step_count: u32,
    #[serde(default)]
    pub evidence_count: u32,
    #[serde(default)]
    pub no_answer_count: u32,
    #[serde(default)]
    pub needs_person_count: u32,
    #[serde(default)]
    pub stalled_count: u32,
    #[serde(default)]
    pub failed_count: u32,
    #[serde(default)]
    pub cancelled_count: u32,
    #[serde(default, rename = "consecutive_without_evidence")]
    pub consecutive_no_evidence: u32,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct StepView {
    pub step_id: Uuid,
    pub branch_id: Uuid,
    pub purpose: String,
    pub question: String,
    #[serde(default)]
    pub text_origin: String,
    pub normalized_question: String,
    #[serde(default)]
    pub profiles: Vec<String>,
    #[serde(default)]
    pub project_id: Option<Uuid>,
    #[serde(default)]
    pub run_id: Option<Uuid>,
    #[serde(default)]
    pub task_ids: Vec<Uuid>,
    #[serde(default)]
    pub artifact_refs: Vec<String>,
    pub state: String,
    #[serde(default)]
    pub outcome_class: Option<OutcomeClass>,
    #[serde(default)]
    pub outcome: Option<RunOutcome>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct RecordView {
    pub record_id: Uuid,
    pub sequence: i64,
    pub kind: String,
    pub text: String,
    pub origin: String,
    pub status: String,
    #[serde(default)]
    pub refs: Vec<String>,
    #[serde(default)]
    pub superseded_by: Option<Uuid>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct RelationView {
    pub relation_id: Uuid,
    pub kind: String,
    pub source_record: Uuid,
    pub target_record: Uuid,
}
