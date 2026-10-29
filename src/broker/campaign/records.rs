use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum StepPurpose {
    Explore,
    Test,
    Replicate,
    Verify,
    Literature,
    Synthesize,
}

impl StepPurpose {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Explore => "explore",
            Self::Test => "test",
            Self::Replicate => "replicate",
            Self::Verify => "verify",
            Self::Literature => "literature",
            Self::Synthesize => "synthesize",
        }
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum OutcomeClass {
    Evidence,
    NoAnswer,
    NeedsPerson,
    Stalled,
    Failed,
    Cancelled,
}

impl OutcomeClass {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Evidence => "evidence",
            Self::NoAnswer => "no_answer",
            Self::NeedsPerson => "needs_person",
            Self::Stalled => "stalled",
            Self::Failed => "failed",
            Self::Cancelled => "cancelled",
        }
    }
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct CampaignBudgets {
    pub wall_seconds: u32,
    pub max_runs: u16,
    pub max_concurrent_runs: u8,
    pub max_turns_per_run: u8,
}

impl CampaignBudgets {
    pub fn validate(&self) -> Result<(), &'static str> {
        if !(1..=14 * 24 * 60 * 60).contains(&self.wall_seconds)
            || !(1..=500).contains(&self.max_runs)
            || !(1..=4).contains(&self.max_concurrent_runs)
            || !(1..=64).contains(&self.max_turns_per_run)
        {
            return Err("campaign_budget_invalid");
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct StopRules {
    #[serde(default = "default_change_approach")]
    pub change_approach_repeats: u8,
    #[serde(default = "default_branch_window")]
    pub branch_window: u8,
    #[serde(default = "default_campaign_window")]
    pub campaign_window: u8,
    #[serde(default = "default_planner_refused")]
    pub planner_refused_cycles: u8,
}

fn default_change_approach() -> u8 {
    2
}
fn default_branch_window() -> u8 {
    4
}
fn default_campaign_window() -> u8 {
    12
}
fn default_planner_refused() -> u8 {
    3
}

impl Default for StopRules {
    fn default() -> Self {
        Self {
            change_approach_repeats: default_change_approach(),
            branch_window: default_branch_window(),
            campaign_window: default_campaign_window(),
            planner_refused_cycles: default_planner_refused(),
        }
    }
}

impl StopRules {
    pub fn validate(&self) -> Result<(), &'static str> {
        if !(2..=8).contains(&self.change_approach_repeats)
            || !(2..=64).contains(&self.branch_window)
            || !(2..=128).contains(&self.campaign_window)
            || !(1..=8).contains(&self.planner_refused_cycles)
        {
            return Err("campaign_stop_rules_invalid");
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct CampaignScopeInput {
    pub starting_points: Vec<String>,
    pub profiles: Vec<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct CreateCampaign {
    pub campaign_id: Uuid,
    pub goal: String,
    #[serde(default)]
    pub constraints: String,
    pub scope: CampaignScopeInput,
    pub budgets: CampaignBudgets,
    #[serde(default)]
    pub stop_rules: StopRules,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct RunOutcome {
    pub run_state: String,
    #[serde(default)]
    pub answer: Option<String>,
    #[serde(default)]
    pub action: Option<String>,
    #[serde(default)]
    pub findings: u32,
    #[serde(default)]
    pub refused_replies: u32,
    #[serde(default)]
    pub turns: u32,
    #[serde(default)]
    pub max_turns: u32,
    #[serde(default)]
    pub cancelled: bool,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct FinishedStep {
    pub step_id: Uuid,
    pub branch_id: Uuid,
    pub project_id: Uuid,
    pub run_id: Uuid,
    pub outcome: RunOutcome,
}
