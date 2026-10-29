//! Durable, lab-scoped research campaigns and their generation-fenced director.

mod director;
mod planner;
mod planner_context;
mod planner_model;
mod planner_reply;
mod records;
mod rules;
mod view;

pub use director::{
    CampaignDirectorArgs, advance_claim, proposal_change, proposal_refusal_change,
    reconciliation_change, run,
};
pub use planner::{
    BranchProposal, CampaignPlanner, CloseBranchProposal, NoPlanner, PlannerError, Proposal,
    RecordProposal, RelationProposal, ScriptedPlanner, StepProposal, normalize_question,
    validate_proposal,
};
pub use planner_context::{CompiledPlannerContext, compile_planner_context};
pub(crate) use planner_model::load_shared_model;
pub use planner_model::{ModelPlanner, PLANNER_SYSTEM, validate_planner_input_bytes};
pub use planner_reply::parse_planner_reply;
pub use records::{
    CampaignBudgets, CampaignScopeInput, CreateCampaign, FinishedStep, OutcomeClass, RunOutcome,
    StepPurpose, StopRules,
};
pub use rules::{RuleDecision, RuleNotice, classify_outcome, evaluate_rules};
pub use view::{BranchView, CampaignView, MethodView, RecordView, RelationView, StepView};
