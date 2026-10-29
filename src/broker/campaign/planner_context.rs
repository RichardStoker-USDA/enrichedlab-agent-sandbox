use serde_json::{Value, json};

use super::{CampaignView, PlannerError, RecordView};

#[derive(Clone, Debug)]
pub struct CompiledPlannerContext {
    pub value: Value,
    pub bytes: usize,
    pub attempts_omitted: usize,
    pub records_omitted: usize,
    pub requests_omitted: usize,
    pub rule_notices_omitted: usize,
}

pub fn compile_planner_context(
    view: &CampaignView,
    max_bytes: usize,
) -> Result<CompiledPlannerContext, PlannerError> {
    let mut attempts: Vec<Value> = view
        .steps
        .iter()
        .rev()
        .filter_map(|step| {
            let class = step.outcome_class?;
            let outcome = step.outcome.as_ref();
            Some(json!({
                "step": step.step_id,
                "branch": step.branch_id,
                "question": step.question,
                "profiles": step.profiles,
                "outcome_class": class,
                "counts": {
                    "findings": outcome.map_or(0, |value| value.findings),
                    "refused_replies": outcome.map_or(0, |value| value.refused_replies),
                    "turns": outcome.map_or(0, |value| value.turns)
                }
            }))
        })
        .collect();
    let mut records: Vec<&RecordView> = view
        .records
        .iter()
        .filter(|record| record.status == "open" && record.kind != "request")
        .collect();
    records.sort_by_key(|record| std::cmp::Reverse(record.sequence));
    let mut request_records: Vec<&RecordView> = if view.open_requests.is_empty() {
        view.records
            .iter()
            .filter(|record| record.kind == "request" && record.status == "open")
            .collect()
    } else {
        view.open_requests.iter().collect()
    };
    request_records.sort_by_key(|record| std::cmp::Reverse(record.sequence));
    let original_attempts = attempts.len();
    let original_records = records.len();
    let original_requests = request_records.len();
    let mut remove_attempt = true;

    loop {
        let value = context_value(
            view,
            &attempts,
            &records,
            &request_records,
            original_attempts - attempts.len(),
            view.records_omitted
                .saturating_add(original_records - records.len()),
            original_requests - request_records.len(),
        );
        let bytes = serde_json::to_vec(&value)
            .map_err(|_| PlannerError("planner_context_invalid"))?
            .len();
        if bytes <= max_bytes {
            return Ok(CompiledPlannerContext {
                value,
                bytes,
                attempts_omitted: original_attempts - attempts.len(),
                records_omitted: view
                    .records_omitted
                    .saturating_add(original_records - records.len()),
                requests_omitted: original_requests - request_records.len(),
                rule_notices_omitted: view.rule_notices_omitted,
            });
        }
        let removed = if remove_attempt {
            attempts.pop().is_some() || remove_oldest_record(&mut records, &mut request_records)
        } else {
            remove_oldest_record(&mut records, &mut request_records) || attempts.pop().is_some()
        };
        if !removed {
            return Err(PlannerError("planner_context_too_large"));
        }
        remove_attempt = !remove_attempt;
    }
}

fn remove_oldest_record(records: &mut Vec<&RecordView>, requests: &mut Vec<&RecordView>) -> bool {
    match (records.last(), requests.last()) {
        (Some(record), Some(request)) if record.sequence <= request.sequence => {
            records.pop();
            true
        }
        (Some(_), Some(_)) | (None, Some(_)) => {
            requests.pop();
            true
        }
        (Some(_), None) => {
            records.pop();
            true
        }
        (None, None) => false,
    }
}

fn context_value(
    view: &CampaignView,
    attempts: &[Value],
    records: &[&RecordView],
    requests: &[&RecordView],
    attempts_omitted: usize,
    records_omitted: usize,
    requests_omitted: usize,
) -> Value {
    let profiles = view.profiles();
    let starting_points: Vec<String> = view
        .scope
        .get("starting_points")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .map(str::to_owned)
        .collect();
    let methods: Vec<Value> = profiles
        .iter()
        .map(|key| {
            view.methods
                .iter()
                .find(|method| &method.key == key)
                .map_or_else(
                    || {
                        json!({"key":key,"name":key,"description":"Method metadata unavailable",
                            "data_sources":starting_points})
                    },
                    |method| {
                        json!({"key":method.key,"name":method.name,"description":method.description,
                            "data_sources":method.data_sources})
                    },
                )
        })
        .collect();
    let branches: Vec<Value> = view
        .branches
        .iter()
        .map(|branch| {
            json!({
                "id":branch.branch_id,
                "parent":branch.parent_branch_id,
                "direction":branch.direction,
                "state":branch.state,
                "reason_class":branch.reason_class,
                "reason":branch.reason,
                "outcome_counts":{
                    "evidence":branch.evidence_count,
                    "no_answer":branch.no_answer_count,
                    "needs_person":branch.needs_person_count,
                    "stalled":branch.stalled_count,
                    "failed":branch.failed_count,
                    "cancelled":branch.cancelled_count,
                    "consecutive_without_evidence":branch.consecutive_no_evidence
                }
            })
        })
        .collect();
    let grouped_records = [
        "question",
        "hypothesis",
        "attempt",
        "finding",
        "prospect",
        "lesson",
    ]
    .into_iter()
    .map(|kind| {
        let items: Vec<Value> = records
            .iter()
            .filter(|record| record.kind == kind)
            .map(|record| json!({"id":record.record_id,"text":record.text,"refs":record.refs}))
            .collect();
        json!({"kind":kind,"items":items})
    })
    .collect::<Vec<_>>();
    let requests = requests
        .iter()
        .map(|record| json!({"id":record.record_id,"text":record.text,"refs":record.refs}))
        .collect::<Vec<_>>();

    json!({"sections":[
        {"section":"goal_and_constraints","goal":view.goal,"constraints":view.constraints},
        {"section":"scope","starting_points":starting_points,"methods":methods},
        {"section":"budgets_left","value":view.budgets_left},
        {"section":"rule_notices","omitted":view.rule_notices_omitted,"items":view.rule_notices},
        {"section":"branches","items":branches},
        {"section":"recent_attempts","omitted":attempts_omitted,"items":attempts},
        {"section":"open_records","omitted":records_omitted,"by_kind":grouped_records},
        {"section":"requests_waiting_on_a_person","omitted":requests_omitted,"items":requests}
    ]})
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::broker::campaign::{MethodView, OutcomeClass, RunOutcome};
    use serde_json::json;
    use uuid::Uuid;

    fn view() -> CampaignView {
        let mut view: CampaignView = serde_json::from_value(json!({
            "campaign_id":Uuid::new_v4(),"group_key":"lab","goal":"Goal 2",
            "constraints":"Constraint 2","scope":{"starting_points":["source"],"profiles":["method"]},
            "budgets":{"wall_seconds":3600,"max_runs":12,"max_concurrent_runs":2,"max_turns_per_run":8},
            "budgets_left":{"runs":10,"concurrent_runs":2},
            "stop_rules":{"change_approach_repeats":2,"branch_window":4,"campaign_window":12,"planner_refused_cycles":3},
            "state":"running","generation":1
        })).unwrap();
        view.methods.push(MethodView {
            key: "method".into(),
            name: "Method".into(),
            description: "A bounded method".into(),
            data_sources: vec!["source".into()],
        });
        for index in 0..8 {
            let mut step: crate::broker::campaign::StepView = serde_json::from_value(json!({
                "step_id":Uuid::new_v4(),"branch_id":Uuid::new_v4(),"purpose":"test",
                "question":format!("Long prior question {index} {}", "x".repeat(180)),
                "normalized_question":format!("q {index}"),"state":"finished",
                "outcome_class":"failed"
            }))
            .unwrap();
            step.outcome = Some(RunOutcome {
                run_state: "failed".into(),
                answer: None,
                action: None,
                findings: 0,
                refused_replies: 0,
                turns: 1,
                max_turns: 8,
                cancelled: false,
            });
            step.outcome_class = Some(OutcomeClass::Failed);
            view.steps.push(step);
            view.records.push(serde_json::from_value(json!({"record_id":Uuid::new_v4(),
                "sequence":index + 1,"kind":"lesson","text":format!("Old record {index} {}", "y".repeat(180)),
                "origin":"planner","status":"open","refs":[]})).unwrap());
        }
        view.rule_notices
            .push(json!({"rule":"branch_exhausted","count":4,"threshold":4}));
        view
    }

    #[test]
    fn sections_stay_ordered_and_only_old_history_is_cut() {
        let full = compile_planner_context(&view(), 64 * 1024).unwrap();
        let names: Vec<&str> = full.value["sections"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|section| section["section"].as_str())
            .collect();
        assert_eq!(
            names,
            [
                "goal_and_constraints",
                "scope",
                "budgets_left",
                "rule_notices",
                "branches",
                "recent_attempts",
                "open_records",
                "requests_waiting_on_a_person"
            ]
        );
        assert_eq!(full.attempts_omitted, 0);
        assert_eq!(full.records_omitted, 0);

        let cut = compile_planner_context(&view(), 3_500).unwrap();
        assert!(cut.attempts_omitted > 0);
        assert!(cut.records_omitted > 0);
        assert_eq!(cut.value["sections"][0]["goal"], "Goal 2");
        assert_eq!(
            cut.value["sections"][1]["methods"][0]["description"],
            "A bounded method"
        );
        assert_eq!(
            cut.value["sections"][3]["items"][0]["rule"],
            "branch_exhausted"
        );
        assert!(cut.bytes <= 3_500);
    }

    #[test]
    fn oldest_open_requests_are_cut_and_counted_as_record_history() {
        let mut campaign = view();
        campaign.steps.clear();
        campaign.records.clear();
        for index in 0..12 {
            campaign.open_requests.push(
                serde_json::from_value(json!({
                    "record_id":Uuid::new_v4(),"sequence":index + 1,"kind":"request",
                    "text":format!("Request {index} {}", "z".repeat(300)),
                    "origin":"director","status":"open","refs":[]
                }))
                .unwrap(),
            );
        }
        let cut = compile_planner_context(&campaign, 3_000).unwrap();
        assert!(cut.requests_omitted > 0);
        assert_eq!(cut.value["sections"][7]["omitted"], cut.requests_omitted);
        let kept = cut.value["sections"][7]["items"].as_array().unwrap();
        assert!(kept[0]["text"].as_str().unwrap().starts_with("Request 11 "));
        assert!(
            kept.iter()
                .all(|request| !request["text"].as_str().unwrap().starts_with("Request 0 "))
        );
    }

    #[test]
    fn immutable_sections_are_never_truncated() {
        let mut campaign = view();
        campaign.goal = "g".repeat(4_000);
        assert_eq!(
            compile_planner_context(&campaign, 1_000).unwrap_err().0,
            "planner_context_too_large"
        );
    }
}
