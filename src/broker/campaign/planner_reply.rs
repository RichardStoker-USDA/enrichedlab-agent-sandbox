use serde::Deserialize;
use uuid::Uuid;

use super::{
    BranchProposal, CampaignView, CloseBranchProposal, PlannerError, Proposal, RecordProposal,
    RelationProposal, StepProposal, StepPurpose,
    planner::{has_unowned_number, normalize_question},
};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PlannerReply {
    branches_open: Vec<OpenBranch>,
    steps: Vec<ProposedStep>,
    branches_close: Vec<CloseBranch>,
    records: Vec<ProposedRecord>,
    summary: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct OpenBranch {
    parent: Option<Uuid>,
    direction: String,
    rationale: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ProposedStep {
    branch: BranchReference,
    purpose: StepPurpose,
    question: String,
    profiles: Option<Vec<String>>,
}

#[derive(Deserialize)]
#[serde(untagged)]
enum BranchReference {
    Existing(Uuid),
    New(usize),
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CloseBranch {
    branch: Uuid,
    reason_class: String,
    note: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ProposedRecord {
    kind: String,
    text: String,
    refs: Vec<String>,
    relations: Vec<ProposedRelation>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ProposedRelation {
    kind: String,
    record: Uuid,
}

pub fn parse_planner_reply(text: &str, view: &CampaignView) -> Result<Proposal, PlannerError> {
    let reply: PlannerReply =
        serde_json::from_str(text).map_err(|_| PlannerError("planner_reply_invalid"))?;
    if !bounded(&reply.summary, 1, 2_000) {
        return Err(PlannerError("planner_reply_invalid"));
    }
    if has_unowned_number(&reply.summary, &view.goal, &view.constraints) {
        return Err(PlannerError("proposal_number_unowned"));
    }
    let new_branch_ids: Vec<Uuid> = reply.branches_open.iter().map(|_| Uuid::new_v4()).collect();
    let open_branches = reply
        .branches_open
        .into_iter()
        .zip(&new_branch_ids)
        .map(|(branch, branch_id)| BranchProposal {
            branch_id: *branch_id,
            parent_branch_id: branch.parent,
            direction: branch.direction,
            rationale: branch.rationale,
        })
        .collect();
    let default_profiles = view.profiles();
    let steps = reply
        .steps
        .into_iter()
        .map(|step| {
            let branch_id = match step.branch {
                BranchReference::Existing(branch) => branch,
                BranchReference::New(index) => *new_branch_ids
                    .get(index)
                    .ok_or(PlannerError("planner_reply_invalid"))?,
            };
            Ok(StepProposal {
                branch_id,
                purpose: step.purpose,
                question: step.question,
                profiles: step.profiles.unwrap_or_else(|| default_profiles.clone()),
            })
        })
        .collect::<Result<Vec<_>, PlannerError>>()?;
    let close_branches = reply
        .branches_close
        .into_iter()
        .map(|branch| CloseBranchProposal {
            branch_id: branch.branch,
            reason_class: branch.reason_class,
            reason: branch.note,
        })
        .collect();

    let existing_records = view
        .records
        .iter()
        .chain(&view.open_requests)
        .map(|record| record.record_id)
        .collect::<std::collections::HashSet<_>>();
    let mut records = Vec::with_capacity(reply.records.len());
    let mut relations = Vec::new();
    for record in reply.records {
        if !matches!(
            record.kind.as_str(),
            "question" | "hypothesis" | "prospect" | "lesson" | "request"
        ) || record
            .relations
            .iter()
            .any(|relation| !existing_records.contains(&relation.record))
        {
            return Err(PlannerError("planner_reply_invalid"));
        }
        let record_id = Uuid::new_v4();
        for relation in record.relations {
            relations.push(RelationProposal {
                relation_id: Uuid::new_v4(),
                kind: relation.kind,
                from_record: record_id,
                to_record: relation.record,
            });
        }
        records.push(RecordProposal {
            record_id,
            kind: record.kind,
            text: record.text,
            status: "open".into(),
            refs: record.refs,
        });
    }
    let proposal = Proposal {
        open_branches,
        steps,
        close_branches,
        records,
        relations,
    };
    if proposal
        .steps
        .iter()
        .any(|step| normalize_question(&step.question).is_empty())
    {
        return Err(PlannerError("planner_reply_invalid"));
    }
    Ok(proposal)
}

fn bounded(value: &str, min: usize, max: usize) -> bool {
    let count = value.chars().count();
    (min..=max).contains(&count) && !value.chars().any(char::is_control)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn view() -> CampaignView {
        serde_json::from_value(json!({
            "campaign_id":Uuid::new_v4(),"group_key":"lab","goal":"Compare 2 approaches",
            "constraints":"Use 2 methods","scope":{"profiles":["method"],"starting_points":["source"]},
            "budgets":{"wall_seconds":3600,"max_runs":12,"max_concurrent_runs":2,"max_turns_per_run":8},
            "stop_rules":{"change_approach_repeats":2,"branch_window":4,"campaign_window":12,"planner_refused_cycles":3},
            "state":"running","generation":1
        })).unwrap()
    }

    fn valid() -> String {
        json!({
            "branches_open":[{"parent":null,"direction":"Independent approach","rationale":"Use a control"}],
            "steps":[{"branch":0,"purpose":"test","question":"Does the supported control change the direction?"}],
            "branches_close":[],"records":[{"kind":"hypothesis","text":"The direction may differ",
                "refs":[],"relations":[]}],"summary":"Test a controlled direction"
        }).to_string()
    }

    #[test]
    fn strict_reply_maps_new_branch_and_default_profiles() {
        let parsed = parse_planner_reply(&valid(), &view()).unwrap();
        assert_eq!(parsed.steps[0].branch_id, parsed.open_branches[0].branch_id);
        assert_eq!(parsed.steps[0].profiles, ["method"]);
    }

    #[test]
    fn strict_reply_refuses_wrappers_unknown_missing_and_unowned_numbers() {
        assert_eq!(
            parse_planner_reply(&format!("reply: {}", valid()), &view())
                .unwrap_err()
                .0,
            "planner_reply_invalid"
        );
        let mut unknown: serde_json::Value = serde_json::from_str(&valid()).unwrap();
        unknown["extra"] = json!(true);
        assert_eq!(
            parse_planner_reply(&unknown.to_string(), &view())
                .unwrap_err()
                .0,
            "planner_reply_invalid"
        );
        let mut missing: serde_json::Value = serde_json::from_str(&valid()).unwrap();
        missing.as_object_mut().unwrap().remove("summary");
        assert_eq!(
            parse_planner_reply(&missing.to_string(), &view())
                .unwrap_err()
                .0,
            "planner_reply_invalid"
        );
        let mut numbered: serde_json::Value = serde_json::from_str(&valid()).unwrap();
        numbered["summary"] = json!("Try 3 new directions");
        assert_eq!(
            parse_planner_reply(&numbered.to_string(), &view())
                .unwrap_err()
                .0,
            "proposal_number_unowned"
        );
    }
}
