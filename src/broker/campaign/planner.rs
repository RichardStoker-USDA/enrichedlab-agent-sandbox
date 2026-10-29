use std::collections::{HashSet, VecDeque};

use async_trait::async_trait;
use regex::Regex;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use super::{CampaignView, StepPurpose};

const MAX_PROPOSAL_BYTES: usize = 192 * 1024;

#[async_trait]
pub trait CampaignPlanner: Send {
    async fn propose(&mut self, view: &CampaignView) -> Result<Proposal, PlannerError>;
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PlannerError(pub &'static str);

#[derive(Default)]
pub struct NoPlanner;

#[async_trait]
impl CampaignPlanner for NoPlanner {
    async fn propose(&mut self, _view: &CampaignView) -> Result<Proposal, PlannerError> {
        Ok(Proposal::default())
    }
}

pub struct ScriptedPlanner {
    proposals: VecDeque<Proposal>,
}

impl ScriptedPlanner {
    pub fn new(proposals: impl IntoIterator<Item = Proposal>) -> Self {
        Self {
            proposals: proposals.into_iter().collect(),
        }
    }
}

#[async_trait]
impl CampaignPlanner for ScriptedPlanner {
    async fn propose(&mut self, _view: &CampaignView) -> Result<Proposal, PlannerError> {
        self.proposals
            .pop_front()
            .ok_or(PlannerError("planner_script_exhausted"))
    }
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Proposal {
    #[serde(default)]
    pub open_branches: Vec<BranchProposal>,
    #[serde(default)]
    pub steps: Vec<StepProposal>,
    #[serde(default)]
    pub close_branches: Vec<CloseBranchProposal>,
    #[serde(default)]
    pub records: Vec<RecordProposal>,
    #[serde(default)]
    pub relations: Vec<RelationProposal>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct BranchProposal {
    pub branch_id: Uuid,
    #[serde(default)]
    pub parent_branch_id: Option<Uuid>,
    pub direction: String,
    pub rationale: String,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct StepProposal {
    pub branch_id: Uuid,
    pub purpose: StepPurpose,
    pub question: String,
    pub profiles: Vec<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct CloseBranchProposal {
    pub branch_id: Uuid,
    #[serde(default = "default_close_reason_class")]
    pub reason_class: String,
    pub reason: String,
}

fn default_close_reason_class() -> String {
    "planner_concluded".into()
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct RecordProposal {
    pub record_id: Uuid,
    pub kind: String,
    pub text: String,
    #[serde(default = "default_record_status")]
    pub status: String,
    #[serde(default)]
    pub refs: Vec<String>,
}

fn default_record_status() -> String {
    "open".into()
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct RelationProposal {
    pub relation_id: Uuid,
    pub kind: String,
    pub from_record: Uuid,
    pub to_record: Uuid,
}

pub fn normalize_question(value: &str) -> String {
    Regex::new(r"[^\p{L}\p{N}]+")
        .expect("fixed regex")
        .replace_all(&value.to_lowercase(), " ")
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

pub fn validate_proposal(view: &CampaignView, proposal: &Proposal) -> Result<(), &'static str> {
    if serde_json::to_vec(proposal).map_or(true, |encoded| encoded.len() > MAX_PROPOSAL_BYTES) {
        return Err("proposal_too_large");
    }
    if proposal.open_branches.is_empty()
        && proposal.steps.is_empty()
        && proposal.close_branches.is_empty()
        && proposal.records.is_empty()
    {
        return Err("planner_no_progress");
    }
    if proposal.open_branches.len() > 32
        || view.branches.len() + proposal.open_branches.len() > 500
        || proposal.close_branches.len() > 32
        || proposal.records.len() > 100
        || proposal.relations.len() > 200
    {
        return Err("proposal_too_large");
    }
    if proposal.steps.len() > view.free_concurrency().min(view.runs_left()) {
        return Err("proposal_concurrency_exceeded");
    }
    let allowed_profiles: HashSet<String> = view.profiles().into_iter().collect();
    let mut branch_ids: HashSet<Uuid> = view
        .branches
        .iter()
        .map(|branch| branch.branch_id)
        .collect();
    for branch in &proposal.open_branches {
        if !branch_ids.insert(branch.branch_id)
            || branch
                .parent_branch_id
                .is_some_and(|parent| !branch_ids.contains(&parent))
            || !bounded_text(&branch.direction, 1, 2_000)
            || !bounded_text(&branch.rationale, 1, 2_000)
        {
            return Err("proposal_branch_invalid");
        }
    }
    let mut questions: HashSet<String> = view
        .steps
        .iter()
        .map(|step| step.normalized_question.clone())
        .collect();
    for step in &proposal.steps {
        let normalized = normalize_question(&step.question);
        let branch_accepts_step = proposal
            .open_branches
            .iter()
            .any(|branch| branch.branch_id == step.branch_id)
            || view.branches.iter().any(|branch| {
                branch.branch_id == step.branch_id
                    && matches!(branch.state.as_str(), "open" | "waiting")
            });
        if !branch_accepts_step
            || proposal
                .close_branches
                .iter()
                .any(|close| close.branch_id == step.branch_id)
            || !bounded_text(&step.question, 1, 2_000)
            || normalized.is_empty()
            || !questions.insert(normalized.clone())
            || step.profiles.is_empty()
            || step.profiles.len() > 16
            || step.profiles.iter().collect::<HashSet<_>>().len() != step.profiles.len()
            || step
                .profiles
                .iter()
                .any(|profile| !allowed_profiles.contains(profile))
        {
            return Err("proposal_step_invalid");
        }
        if let Some(branch) = view
            .branches
            .iter()
            .find(|branch| branch.branch_id == step.branch_id)
            && branch.state == "waiting"
            && view
                .steps
                .iter()
                .rev()
                .find(|previous| {
                    previous.branch_id == step.branch_id
                        && previous
                            .outcome_class
                            .is_some_and(|class| class != super::OutcomeClass::Evidence)
                })
                .is_some_and(|previous| {
                    previous.normalized_question == normalized
                        || same_profiles(&previous.profiles, &step.profiles)
                })
        {
            return Err("proposal_approach_repeated");
        }
    }
    let existing_records: HashSet<Uuid> = view
        .records
        .iter()
        .chain(&view.open_requests)
        .map(|record| record.record_id)
        .collect();
    let mut record_ids = existing_records.clone();
    for record in &proposal.records {
        if !record_ids.insert(record.record_id)
            || !matches!(
                record.kind.as_str(),
                "question"
                    | "hypothesis"
                    | "attempt"
                    | "finding"
                    | "prospect"
                    | "lesson"
                    | "request"
            )
            || !matches!(
                record.status.as_str(),
                "open" | "supported" | "contradicted" | "abandoned" | "withdrawn"
            )
            || !bounded_text(&record.text, 1, 2_000)
            || record.refs.len() > 32
            || record
                .refs
                .iter()
                .any(|reference| !reference_resolves(view, reference, &record_ids))
        {
            return Err("proposal_record_invalid");
        }
    }
    let mut relation_ids: HashSet<Uuid> = view
        .relations
        .iter()
        .map(|relation| relation.relation_id)
        .collect();
    let mut relation_edges: HashSet<(String, Uuid, Uuid)> = view
        .relations
        .iter()
        .map(|relation| {
            (
                relation.kind.clone(),
                relation.source_record,
                relation.target_record,
            )
        })
        .collect();
    for relation in &proposal.relations {
        if !relation_ids.insert(relation.relation_id)
            || !relation_edges.insert((
                relation.kind.clone(),
                relation.from_record,
                relation.to_record,
            ))
            || !matches!(
                relation.kind.as_str(),
                "supports"
                    | "contradicts"
                    | "tested_by"
                    | "retry_of"
                    | "derived_from"
                    | "cites_evidence"
                    | "answers"
            )
            || relation.from_record == relation.to_record
            || !record_ids.contains(&relation.from_record)
            || !record_ids.contains(&relation.to_record)
        {
            return Err("proposal_relation_invalid");
        }
    }
    let mut closed_branches = HashSet::new();
    for close in &proposal.close_branches {
        let branch_can_close = proposal
            .open_branches
            .iter()
            .any(|branch| branch.branch_id == close.branch_id)
            || view.branches.iter().any(|branch| {
                branch.branch_id == close.branch_id
                    && matches!(branch.state.as_str(), "open" | "waiting")
            });
        if !branch_ids.contains(&close.branch_id)
            || !branch_can_close
            || !closed_branches.insert(close.branch_id)
            || !fixed_code(&close.reason_class)
            || !bounded_text(&close.reason, 1, 500)
        {
            return Err("proposal_close_invalid");
        }
    }
    if proposal_texts(proposal).any(|text| has_unowned_number(text, &view.goal, &view.constraints))
    {
        return Err("proposal_number_unowned");
    }
    Ok(())
}

fn proposal_texts(proposal: &Proposal) -> impl Iterator<Item = &str> {
    proposal
        .open_branches
        .iter()
        .flat_map(|branch| [branch.direction.as_str(), branch.rationale.as_str()])
        .chain(proposal.steps.iter().map(|step| step.question.as_str()))
        .chain(
            proposal
                .close_branches
                .iter()
                .map(|close| close.reason.as_str()),
        )
        .chain(proposal.records.iter().map(|record| record.text.as_str()))
}

fn bounded_text(value: &str, min: usize, max: usize) -> bool {
    let count = value.chars().count();
    (min..=max).contains(&count) && !value.chars().any(char::is_control)
}

fn fixed_code(value: &str) -> bool {
    (2..=64).contains(&value.len())
        && value.as_bytes()[0].is_ascii_lowercase()
        && value
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'_')
}

fn same_profiles(left: &[String], right: &[String]) -> bool {
    let mut left = left.to_vec();
    let mut right = right.to_vec();
    left.sort();
    right.sort();
    left == right
}

pub(super) fn has_unowned_number(text: &str, goal: &str, constraints: &str) -> bool {
    let number = Regex::new(r"[-+]?\d+(?:[.,]\d+)*").expect("fixed regex");
    let owned: HashSet<&str> = number
        .captures_iter(goal)
        .chain(number.captures_iter(constraints))
        .filter_map(|capture| capture.get(0).map(|found| found.as_str()))
        .collect();
    number
        .captures_iter(text)
        .filter_map(|capture| capture.get(0).map(|found| found.as_str()))
        .any(|found| !owned.contains(found))
}

fn reference_resolves(view: &CampaignView, reference: &str, records: &HashSet<Uuid>) -> bool {
    if let Some(raw) = reference.strip_prefix("record:") {
        return raw.parse::<Uuid>().is_ok_and(|id| records.contains(&id));
    }
    if let Some(raw) = reference.strip_prefix("step:") {
        return raw
            .parse::<Uuid>()
            .is_ok_and(|id| view.steps.iter().any(|step| step.step_id == id));
    }
    if let Some(raw) = reference.strip_prefix("run:") {
        return raw
            .parse::<Uuid>()
            .is_ok_and(|id| view.steps.iter().any(|step| step.run_id == Some(id)));
    }
    if let Some(raw) = reference.strip_prefix("project:") {
        return raw
            .parse::<Uuid>()
            .is_ok_and(|id| view.steps.iter().any(|step| step.project_id == Some(id)));
    }
    if let Some(raw) = reference.strip_prefix("task:") {
        return raw
            .parse::<Uuid>()
            .is_ok_and(|id| view.steps.iter().any(|step| step.task_ids.contains(&id)));
    }
    if reference.starts_with("artifact://") {
        return view
            .steps
            .iter()
            .any(|step| step.artifact_refs.iter().any(|known| known == reference));
    }
    false
}
