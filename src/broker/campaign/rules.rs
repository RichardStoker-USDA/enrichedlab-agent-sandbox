use std::collections::HashMap;

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use super::{CampaignView, FinishedStep, OutcomeClass};

pub const REFUSED_REPLY_LIMIT: u32 = 3;

pub fn classify_outcome(outcome: &super::RunOutcome) -> OutcomeClass {
    if outcome.cancelled || outcome.run_state == "cancelled" {
        return OutcomeClass::Cancelled;
    }
    if matches!(
        outcome.answer.as_deref(),
        Some("answered" | "partly_answered")
    ) && outcome.findings > 0
    {
        return OutcomeClass::Evidence;
    }
    if outcome.answer.as_deref() == Some("not_answered") {
        return OutcomeClass::NoAnswer;
    }
    if outcome.action.as_deref() == Some("review_required") {
        return OutcomeClass::NeedsPerson;
    }
    if outcome.run_state == "failed" || outcome.refused_replies >= REFUSED_REPLY_LIMIT {
        return OutcomeClass::Failed;
    }
    if outcome.run_state == "paused"
        || (outcome.max_turns > 0
            && outcome.turns >= outcome.max_turns
            && !matches!(
                outcome.answer.as_deref(),
                Some("answered" | "partly_answered")
            ))
    {
        return OutcomeClass::Stalled;
    }
    OutcomeClass::Failed
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct RuleNotice {
    pub rule: String,
    pub count: u32,
    pub threshold: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub branch_id: Option<Uuid>,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
pub struct RuleDecision {
    pub notices: Vec<RuleNotice>,
    pub wait_branches: Vec<Uuid>,
    pub conclude_branches: Vec<Uuid>,
    pub stop_reason: Option<String>,
}

/// Evaluate the counters after `finished` is reconciled. The database repeats the
/// state checks under the generation fence; these decisions carry no model text.
pub fn evaluate_rules(view: &CampaignView, finished: &[FinishedStep]) -> RuleDecision {
    let mut decision = RuleDecision::default();
    let mut last_by_branch: HashMap<Uuid, (Option<OutcomeClass>, u32)> = HashMap::new();
    let mut no_evidence_by_branch: HashMap<Uuid, u32> = view
        .branches
        .iter()
        .map(|branch| (branch.branch_id, branch.consecutive_no_evidence))
        .collect();
    for branch in &view.branches {
        let mut last = None;
        let mut streak = 0;
        for class in view
            .steps
            .iter()
            .filter(|step| step.branch_id == branch.branch_id)
            .filter_map(|step| step.outcome_class)
        {
            if class != OutcomeClass::Evidence && last == Some(class) {
                streak += 1;
            } else if class != OutcomeClass::Evidence {
                streak = 1;
            } else {
                streak = 0;
            }
            last = Some(class);
        }
        last_by_branch.insert(branch.branch_id, (last, streak));
    }
    let mut campaign_no_evidence = view
        .steps
        .iter()
        .rev()
        .take_while(|step| step.outcome_class != Some(OutcomeClass::Evidence))
        .filter(|step| step.outcome_class.is_some())
        .count() as u32;
    for step in finished {
        let class = classify_outcome(&step.outcome);
        campaign_no_evidence = if class == OutcomeClass::Evidence {
            0
        } else {
            campaign_no_evidence + 1
        };
        let Some(branch) = view
            .branches
            .iter()
            .find(|branch| branch.branch_id == step.branch_id)
        else {
            continue;
        };
        let branch_accepts_rules = matches!(branch.state.as_str(), "open" | "waiting");
        let previous_no_evidence = no_evidence_by_branch.entry(branch.branch_id).or_default();
        let branch_no_evidence = if class == OutcomeClass::Evidence {
            0
        } else {
            *previous_no_evidence + 1
        };
        *previous_no_evidence = branch_no_evidence;
        let (last, streak) = last_by_branch.entry(branch.branch_id).or_default();
        let same = if class != OutcomeClass::Evidence && *last == Some(class) {
            *streak + 1
        } else if class != OutcomeClass::Evidence {
            1
        } else {
            0
        };
        *last = Some(class);
        *streak = same;
        if branch_accepts_rules && same >= u32::from(view.stop_rules.change_approach_repeats) {
            decision.wait_branches.push(branch.branch_id);
            decision.notices.push(RuleNotice {
                rule: "change_approach".into(),
                count: same,
                threshold: u32::from(view.stop_rules.change_approach_repeats),
                branch_id: Some(branch.branch_id),
            });
        }
        if branch_accepts_rules && branch_no_evidence == u32::from(view.stop_rules.branch_window) {
            decision.conclude_branches.push(branch.branch_id);
            decision.notices.push(RuleNotice {
                rule: "branch_exhausted".into(),
                count: branch_no_evidence,
                threshold: u32::from(view.stop_rules.branch_window),
                branch_id: Some(branch.branch_id),
            });
        }
        if campaign_no_evidence == u32::from(view.stop_rules.campaign_window) {
            decision.stop_reason = Some("campaign_exhausted".into());
            decision.notices.push(RuleNotice {
                rule: "campaign_exhausted".into(),
                count: campaign_no_evidence,
                threshold: u32::from(view.stop_rules.campaign_window),
                branch_id: None,
            });
        }
    }
    if view.run_count >= u32::from(view.budgets.max_runs) {
        decision.stop_reason = Some("budget_exhausted".into());
        decision.notices.push(RuleNotice {
            rule: "budget".into(),
            count: view.run_count,
            threshold: u32::from(view.budgets.max_runs),
            branch_id: None,
        });
    }
    decision
}

#[cfg(test)]
mod tests {
    use super::*;

    fn outcome(
        state: &str,
        answer: Option<&str>,
        action: Option<&str>,
        findings: u32,
    ) -> super::super::RunOutcome {
        super::super::RunOutcome {
            run_state: state.into(),
            answer: answer.map(str::to_owned),
            action: action.map(str::to_owned),
            findings,
            refused_replies: 0,
            turns: 1,
            max_turns: 8,
            cancelled: false,
        }
    }

    #[test]
    fn outcome_table_is_exhaustive() {
        assert_eq!(
            classify_outcome(&outcome("finished", Some("answered"), None, 1)),
            OutcomeClass::Evidence
        );
        assert_eq!(
            classify_outcome(&outcome("finished", Some("partly_answered"), None, 1)),
            OutcomeClass::Evidence
        );
        assert_eq!(
            classify_outcome(&outcome("finished", Some("not_answered"), None, 0)),
            OutcomeClass::NoAnswer
        );
        assert_eq!(
            classify_outcome(&outcome("finished", None, Some("review_required"), 0)),
            OutcomeClass::NeedsPerson
        );
        assert_eq!(
            classify_outcome(&outcome("paused", None, None, 0)),
            OutcomeClass::Stalled
        );
        assert_eq!(
            classify_outcome(&outcome("failed", None, None, 0)),
            OutcomeClass::Failed
        );
        assert_eq!(
            classify_outcome(&outcome("cancelled", None, None, 0)),
            OutcomeClass::Cancelled
        );
    }
}
