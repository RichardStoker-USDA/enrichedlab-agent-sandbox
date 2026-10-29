use enrichedlab_agent_sandbox::broker::{
    campaign::{
        BranchView, CampaignPlanner, CampaignView, FinishedStep, MethodView, ModelPlanner,
        OutcomeClass, PLANNER_SYSTEM, Proposal, RunOutcome, ScriptedPlanner, StepView,
        advance_claim, classify_outcome, compile_planner_context, evaluate_rules,
        normalize_question, parse_planner_reply, proposal_change, proposal_refusal_change,
        reconciliation_change, validate_proposal,
    },
    model::{PrincipalClass, VerifiedIdentity},
    project::{CheckpointRequest, ResearchState},
    store::Store,
};
use serde_json::{Value, json};
use std::{
    env, fs,
    os::unix::fs::PermissionsExt,
    sync::{Arc, Mutex},
};
use tokio_postgres::{Config, NoTls, config::Host};
use uuid::Uuid;

const BRANCH: Uuid = Uuid::from_u128(0x11111111111141118111111111111111);
const STEP: Uuid = Uuid::from_u128(0x22222222222242228222222222222222);
const PROJECT: Uuid = Uuid::from_u128(0x33333333333343338333333333333333);
const RUN: Uuid = Uuid::from_u128(0x44444444444444448444444444444444);

fn view() -> CampaignView {
    serde_json::from_value(json!({
        "campaign_id":"aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa",
        "group_key":"pilot-lab",
        "goal":"Compare the two supported approaches",
        "constraints":"Use two independent methods",
        "text_origin":"person",
        "scope":{"starting_points":["reference"],"profiles":["general-research"],"workspace":{}},
        "budgets":{"wall_seconds":3600,"max_runs":12,"max_concurrent_runs":2,"max_turns_per_run":8},
        "stop_rules":{"change_approach_repeats":2,"branch_window":4,"campaign_window":12,"planner_refused_cycles":3},
        "state":"running","generation":7,"run_count":0,"active_runs":0,"finished_steps":0,
        "evidence_steps":0,"planner_refused_cycles":0,"branches":[],"steps":[],"records":[]
    })).unwrap()
}

fn outcome(
    run_state: &str,
    answer: Option<&str>,
    action: Option<&str>,
    findings: u32,
) -> RunOutcome {
    RunOutcome {
        run_state: run_state.into(),
        answer: answer.map(str::to_owned),
        action: action.map(str::to_owned),
        findings,
        refused_replies: 0,
        turns: 1,
        max_turns: 8,
        cancelled: false,
    }
}

fn branch(no_evidence: u32) -> BranchView {
    BranchView {
        branch_id: BRANCH,
        parent_branch_id: None,
        direction: "Independent approach".into(),
        rationale: "Test the scoped question".into(),
        text_origin: "planner".into(),
        state: "open".into(),
        reason_class: None,
        reason: None,
        step_count: no_evidence,
        evidence_count: 0,
        no_answer_count: 0,
        needs_person_count: 0,
        stalled_count: 0,
        failed_count: 0,
        cancelled_count: 0,
        consecutive_no_evidence: no_evidence,
    }
}

fn step(class: Option<OutcomeClass>, question: &str) -> StepView {
    StepView {
        step_id: STEP,
        branch_id: BRANCH,
        purpose: "explore".into(),
        question: question.into(),
        text_origin: "planner".into(),
        normalized_question: normalize_question(question),
        profiles: vec!["general-research".into()],
        project_id: Some(PROJECT),
        run_id: Some(RUN),
        task_ids: vec![],
        artifact_refs: vec![],
        state: if class.is_some() {
            "finished"
        } else {
            "running"
        }
        .into(),
        outcome_class: class,
        outcome: None,
    }
}

fn finished(run_outcome: RunOutcome) -> FinishedStep {
    FinishedStep {
        step_id: STEP,
        branch_id: BRANCH,
        project_id: PROJECT,
        run_id: RUN,
        outcome: run_outcome,
    }
}

#[test]
fn migration_sql_and_plpgsql_parse() {
    for migration in [
        include_str!("../migrations/postgres/066_campaigns.sql"),
        include_str!("../migrations/postgres/067_campaign_planner.sql"),
    ] {
        pg_query::parse(migration).unwrap();
        pg_query::parse_plpgsql(migration).unwrap();
    }
}

#[test]
fn planner_migration_is_append_only_scoped_and_function_only() {
    let migration = include_str!("../migrations/postgres/067_campaign_planner.sql");
    assert!(
        migration
            .contains("CREATE TABLE IF NOT EXISTS enrichedlab_agent.broker_campaign_planner_calls")
    );
    assert!(
        migration
            .contains("CREATE TABLE IF NOT EXISTS enrichedlab_agent.broker_campaign_planner_texts")
    );
    assert!(migration.contains(
        "CREATE TABLE IF NOT EXISTS enrichedlab_agent.broker_campaign_planner_call_starts"
    ));
    assert_eq!(migration.matches("FORCE ROW LEVEL SECURITY").count(), 1);
    assert_eq!(
        migration.matches("CREATE TRIGGER immutable_record").count(),
        3
    );
    assert!(migration.contains("broker_campaign_has_capability"));
    assert!(migration.contains("cardinality(allowed_functions) <> 11"));
    assert!(migration.contains("'rule_notices_omitted', 0"));
    assert!(
        migration.contains("INSERT INTO enrichedlab_agent.broker_campaign_planner_call_starts")
    );
    let grants = include_str!("../migrations/postgres/grant_broker_runtime.psql");
    assert!(grants.contains("broker_start_campaign_planner_call(text,uuid,text,bigint,uuid,bytea,text) TO :\"director_role\""));
    assert!(grants.contains("broker_finish_campaign_planner_call(text,uuid,text,bigint,uuid,bytea,text,text) TO :\"director_role\""));
    assert!(grants.contains(
        "broker_campaign_planner_calls_page(text,uuid,uuid,bigint,integer) TO :\"api_role\""
    ));
    assert!(grants.contains("broker_campaign_planner_text(text,uuid,uuid,text) TO :\"api_role\""));
}

#[test]
fn planner_context_and_reply_keep_the_bounded_contract() {
    let mut campaign = view();
    campaign.records_omitted = 7;
    campaign.rule_notices = vec![
        json!({"rule":"change_approach","count":2,"threshold":2}),
        json!({"rule":"branch_exhausted","count":4,"threshold":4}),
    ];
    campaign.methods.push(MethodView {
        key: "general-research".into(),
        name: "General research".into(),
        description: "Answers one bounded question from the saved source".into(),
        data_sources: vec!["reference".into()],
    });
    for index in 0..10 {
        let mut prior = step(
            Some(OutcomeClass::Failed),
            &format!("Prior attempt {index}"),
        );
        prior.step_id = Uuid::new_v4();
        prior.outcome = Some(outcome("failed", None, None, 0));
        campaign.steps.push(prior);
        campaign.records.push(
            serde_json::from_value(json!({"record_id":Uuid::new_v4(),"sequence":index + 1,
                "kind":"lesson","text":format!("Prior planning lesson {index}"),
                "origin":"planner","status":"open","refs":[]}))
            .unwrap(),
        );
    }
    let full = compile_planner_context(&campaign, 64 * 1024).unwrap();
    let section_names: Vec<&str> = full.value["sections"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|section| section["section"].as_str())
        .collect();
    assert_eq!(section_names[0], "goal_and_constraints");
    assert_eq!(section_names[1], "scope");
    assert_eq!(section_names[3], "rule_notices");
    assert_eq!(full.records_omitted, 7);
    assert_eq!(full.rule_notices_omitted, 0);
    assert_eq!(full.value["sections"][3]["omitted"], 0);
    assert_eq!(
        full.value["sections"][3]["items"].as_array().unwrap().len(),
        2
    );
    assert_eq!(full.value["sections"][6]["omitted"], 7);
    assert!(PLANNER_SYSTEM.len() < 6 * 1024);
    let cut = compile_planner_context(&campaign, 2_000).unwrap();
    assert!(cut.attempts_omitted > 0);
    assert!(cut.records_omitted > 7);
    assert_eq!(cut.value["sections"][0]["goal"], campaign.goal);
    assert_eq!(
        cut.value["sections"][1]["methods"][0]["key"],
        "general-research"
    );

    let reply = json!({
        "branches_open":[{"parent":null,"direction":"Independent control","rationale":"Test an expected direction"}],
        "steps":[{"branch":0,"purpose":"test","question":"Does the control support the expected direction?"}],
        "branches_close":[],"records":[],"summary":"Start with a controlled comparison"
    });
    let proposal = parse_planner_reply(&reply.to_string(), &campaign).unwrap();
    assert_eq!(
        proposal.steps[0].branch_id,
        proposal.open_branches[0].branch_id
    );
    assert_eq!(proposal.steps[0].profiles, ["general-research"]);
    assert_eq!(
        parse_planner_reply(&format!("answer: {reply}"), &campaign)
            .unwrap_err()
            .0,
        "planner_reply_invalid"
    );
    let mut missing = reply.clone();
    missing.as_object_mut().unwrap().remove("summary");
    assert_eq!(
        parse_planner_reply(&missing.to_string(), &campaign)
            .unwrap_err()
            .0,
        "planner_reply_invalid"
    );
    let mut numbered = reply.clone();
    numbered["summary"] = json!("Try 3 unsupported directions");
    assert_eq!(
        parse_planner_reply(&numbered.to_string(), &campaign)
            .unwrap_err()
            .0,
        "proposal_number_unowned"
    );
    let mut unknown = reply;
    unknown["unexpected"] = json!(true);
    assert_eq!(
        parse_planner_reply(&unknown.to_string(), &campaign)
            .unwrap_err()
            .0,
        "planner_reply_invalid"
    );
}

#[test]
fn migration_upgrades_the_committed_campaign_draft_shape() {
    let migration = include_str!("../migrations/postgres/066_campaigns.sql");
    let add_column = migration
        .find("ADD COLUMN IF NOT EXISTS checkpoint_revision bigint")
        .unwrap();
    let checkpoint_guard = migration
        .find("CREATE OR REPLACE FUNCTION enrichedlab_agent.broker_campaign_checkpoint_guard")
        .unwrap();
    assert!(add_column < checkpoint_guard);
    assert!(migration.contains("CREATE TEMPORARY TABLE broker_campaign_step_pin_upgrade"));
    assert!(migration.contains("broker_research_turns turn_row"));
    assert!(migration.contains("broker_plan_revisions plan_row"));
    assert!(migration.contains("campaign_checkpoint_upgrade_unsafe"));
    assert!(migration.contains("active_safe IS NOT TRUE"));
    assert!(migration.contains("code = 'campaign_checkpoint_unpinned'"));
    assert!(migration.contains("state_reason = 'checkpoint_unpinned'"));
    assert!(migration.contains("broker_campaign_steps_state_shape"));
    assert!(migration.contains("broker_campaign_steps_checkpoint_revision_fkey"));
    assert!(migration.contains("ON CONFLICT (tenant_id, project_id, principal_id) DO UPDATE"));
    assert!(migration.contains("SET capabilities = EXCLUDED.capabilities"));
    assert!(migration.contains("ARRAY['read']::text[]"));
    assert!(migration.contains("DROP POLICY IF EXISTS campaign_tenant"));
}

#[test]
fn step_start_capacity_contract_is_ordered_and_structured() {
    let migration = include_str!("../migrations/postgres/066_campaigns.sql");
    let (_, step_start) = migration
        .split_once("CREATE OR REPLACE FUNCTION enrichedlab_agent.broker_start_campaign_step(")
        .unwrap();
    let (step_start, _) = step_start
        .split_once("REVOKE ALL ON FUNCTION enrichedlab_agent.broker_start_campaign_step(")
        .unwrap();
    let total = step_start
        .find("IF campaign_row.run_count >= max_runs THEN")
        .unwrap();
    let concurrent = step_start
        .find("IF campaign_row.active_runs >= max_concurrent THEN")
        .unwrap();
    assert!(total < concurrent);
    assert!(step_start[total..concurrent].contains(
        "RETURN jsonb_build_object('started', false, 'code', 'campaign_budget_exhausted', 'state', 'stopping')"
    ));
    let concurrent_clause = &step_start[concurrent..];
    let concurrent_clause = &concurrent_clause[..concurrent_clause.find("END IF;").unwrap()];
    assert!(concurrent_clause.contains(
        "RETURN jsonb_build_object('started', false, 'code', 'campaign_concurrency_exhausted', 'state', 'running')"
    ));
    assert!(!concurrent_clause.contains("RAISE EXCEPTION"));
}

#[test]
fn director_role_contract_is_checked_before_provisioning_cleanup() {
    let provisioning = include_str!("../migrations/postgres/grant_broker_runtime.psql");
    let safety_check = provisioning
        .find("broker_campaign_director_role_safe(:'director_role'::name)")
        .unwrap();
    let first_role_alter = provisioning.find("ALTER ROLE :\"api_role\"").unwrap();
    assert!(safety_check < first_role_alter);
    assert!(
        provisioning.contains(
            "GRANT EXECUTE ON FUNCTION enrichedlab_agent.broker_campaign_director_ready()"
        )
    );

    let migration = include_str!("../migrations/postgres/066_campaigns.sql");
    let (_, safety_function) = migration
        .split_once(
            "CREATE OR REPLACE FUNCTION enrichedlab_agent.broker_campaign_director_role_safe",
        )
        .unwrap();
    assert!(safety_function.contains("membership.roleid = role_oid"));
    assert!(safety_function.contains("membership.member = role_oid"));
    assert!(safety_function.contains("role.rolsuper"));
    assert!(safety_function.contains("dependency.deptype = 'o'"));
    assert!(safety_function.contains("aclexplode(relation.relacl)"));
    assert!(safety_function.contains("aclexplode(attribute.attacl)"));
    assert!(safety_function.contains("has_table_privilege"));
    assert!(safety_function.contains("has_any_column_privilege"));
    assert!(safety_function.contains("has_sequence_privilege"));
    assert!(safety_function.contains("has_function_privilege"));
}

#[test]
fn outcome_classification_matches_every_director_case() {
    assert_eq!(
        classify_outcome(&outcome(
            "review_required",
            Some("answered"),
            Some("review_required"),
            1
        )),
        OutcomeClass::Evidence
    );
    assert_eq!(
        classify_outcome(&outcome(
            "review_required",
            Some("partly_answered"),
            Some("review_required"),
            1
        )),
        OutcomeClass::Evidence
    );
    assert_eq!(
        classify_outcome(&outcome(
            "review_required",
            Some("not_answered"),
            Some("review_required"),
            0
        )),
        OutcomeClass::NoAnswer
    );
    assert_eq!(
        classify_outcome(&outcome(
            "review_required",
            None,
            Some("review_required"),
            0
        )),
        OutcomeClass::NeedsPerson
    );
    assert_eq!(
        classify_outcome(&outcome("paused", None, None, 0)),
        OutcomeClass::Stalled
    );
    let mut turn_limit = outcome("review_required", None, None, 0);
    turn_limit.turns = turn_limit.max_turns;
    assert_eq!(classify_outcome(&turn_limit), OutcomeClass::Stalled);
    assert_eq!(
        classify_outcome(&outcome("failed", None, None, 0)),
        OutcomeClass::Failed
    );
    let mut refused = outcome("paused", None, None, 0);
    refused.refused_replies = 3;
    assert_eq!(classify_outcome(&refused), OutcomeClass::Failed);
    assert_eq!(
        classify_outcome(&outcome("cancelled", None, None, 0)),
        OutcomeClass::Cancelled
    );
}

#[test]
fn rules_fire_at_the_threshold_and_not_before() {
    let mut campaign = view();
    campaign.branches.push(branch(0));
    campaign
        .steps
        .push(step(Some(OutcomeClass::Failed), "Earlier attempt"));
    let decision = evaluate_rules(&campaign, &[finished(outcome("failed", None, None, 0))]);
    assert_eq!(decision.wait_branches, vec![BRANCH]);
    assert!(!decision.conclude_branches.contains(&BRANCH));

    campaign.branches[0].consecutive_no_evidence = 2;
    let before = evaluate_rules(&campaign, &[finished(outcome("paused", None, None, 0))]);
    assert!(!before.conclude_branches.contains(&BRANCH));
    campaign.branches[0].consecutive_no_evidence = 3;
    let at = evaluate_rules(&campaign, &[finished(outcome("paused", None, None, 0))]);
    assert!(at.conclude_branches.contains(&BRANCH));

    campaign.steps = (0..10)
        .map(|index| step(Some(OutcomeClass::Failed), &format!("Attempt {index}")))
        .collect();
    let campaign_before = evaluate_rules(&campaign, &[finished(outcome("failed", None, None, 0))]);
    assert_eq!(campaign_before.stop_reason, None);
    campaign
        .steps
        .push(step(Some(OutcomeClass::Failed), "Attempt eleven"));
    let campaign_at = evaluate_rules(&campaign, &[finished(outcome("failed", None, None, 0))]);
    assert_eq!(
        campaign_at.stop_reason.as_deref(),
        Some("campaign_exhausted")
    );

    campaign.run_count = 11;
    assert_ne!(
        evaluate_rules(&campaign, &[]).stop_reason.as_deref(),
        Some("budget_exhausted")
    );
    campaign.run_count = 12;
    assert_eq!(
        evaluate_rules(&campaign, &[]).stop_reason.as_deref(),
        Some("budget_exhausted")
    );

    campaign.run_count = 0;
    campaign.planner_refused_cycles = 1;
    assert!(
        proposal_refusal_change(&campaign, "proposal_step_invalid")["campaign_state"].is_null()
    );
    campaign.planner_refused_cycles = 2;
    assert_eq!(
        proposal_refusal_change(&campaign, "proposal_step_invalid")["campaign_state"]["state"],
        "pausing"
    );
}

#[test]
fn director_records_never_copy_a_planner_question() {
    let claim = "The treatment definitively causes the claimed result";
    let mut campaign = view();
    campaign.branches.push(branch(0));
    campaign.steps.push(step(None, claim));
    let change = reconciliation_change(
        &campaign,
        &[finished(outcome(
            "review_required",
            Some("answered"),
            None,
            1,
        ))],
    );
    let records = change["records"].as_array().unwrap();
    assert!(records.iter().all(|record| {
        record["origin"] != "director" || !record["text"].as_str().unwrap().contains(claim)
    }));
    assert_eq!(
        records[0]["text"],
        format!(
            "Campaign step {STEP} completed run {RUN} with outcome evidence; findings 1; refused replies 0; turns 1"
        )
    );
}

#[test]
fn empty_proposal_counts_as_no_progress_and_pauses_at_the_threshold() {
    let proposal = Proposal::default();
    let mut campaign = view();
    assert_eq!(
        validate_proposal(&campaign, &proposal),
        Err("planner_no_progress")
    );
    campaign.planner_refused_cycles = u32::from(campaign.stop_rules.planner_refused_cycles) - 1;
    let change = proposal_refusal_change(&campaign, "planner_no_progress");
    assert_eq!(change["campaign_state"]["state"], "pausing");
    assert_eq!(change["campaign_state"]["reason"], "planner_refused");
    assert_eq!(
        change["events"][0]["detail"]["reason_code"],
        "planner_no_progress"
    );
}

#[test]
fn terminal_branches_accept_late_results_but_no_more_rule_or_planner_work() {
    let mut campaign = view();
    let mut concluded = branch(3);
    concluded.state = "concluded".into();
    concluded.reason_class = Some("no_new_evidence".into());
    concluded.reason = Some("The configured branch window was reached".into());
    campaign.branches.push(concluded);
    campaign
        .steps
        .push(step(Some(OutcomeClass::Failed), "Earlier failed question"));

    let late = finished(outcome("failed", None, None, 0));
    let decision = evaluate_rules(&campaign, std::slice::from_ref(&late));
    assert!(decision.wait_branches.is_empty());
    assert!(decision.conclude_branches.is_empty());
    let reconciliation = reconciliation_change(&campaign, &[late]);
    assert!(
        reconciliation["branch_updates"]
            .as_array()
            .unwrap()
            .is_empty()
    );

    let mut proposal: Proposal = serde_json::from_str(include_str!(
        "fixtures/broker_campaigns/scripted_planner.json"
    ))
    .unwrap();
    proposal.open_branches.clear();
    assert_eq!(
        validate_proposal(&campaign, &proposal),
        Err("proposal_step_invalid")
    );
}

#[test]
fn planner_validation_refuses_each_owned_boundary() {
    let mut campaign = view();
    let valid: Proposal = serde_json::from_str(include_str!(
        "fixtures/broker_campaigns/scripted_planner.json"
    ))
    .unwrap();
    assert!(validate_proposal(&campaign, &valid).is_ok());

    let mut oversized = valid.clone();
    oversized.records = (0..100)
        .map(|_| {
            serde_json::from_value(json!({
                "record_id":Uuid::new_v4(), "kind":"lesson", "text":"x".repeat(2_000),
                "status":"open", "refs":[]
            }))
            .unwrap()
        })
        .collect();
    assert_eq!(
        validate_proposal(&campaign, &oversized),
        Err("proposal_too_large")
    );

    let mut too_many = valid.clone();
    campaign.active_runs = campaign.budgets.max_concurrent_runs.into();
    assert_eq!(
        validate_proposal(&campaign, &too_many),
        Err("proposal_concurrency_exceeded")
    );
    campaign.active_runs = 0;
    too_many.steps[0].profiles = vec!["outside-scope".into()];
    assert_eq!(
        validate_proposal(&campaign, &too_many),
        Err("proposal_step_invalid")
    );

    let mut no_runs_left = view();
    no_runs_left.run_count = u32::from(no_runs_left.budgets.max_runs);
    assert_eq!(
        validate_proposal(&no_runs_left, &valid),
        Err("proposal_concurrency_exceeded")
    );

    let mut bad_branch = valid.clone();
    bad_branch.open_branches[0].parent_branch_id = Some(Uuid::new_v4());
    assert_eq!(
        validate_proposal(&view(), &bad_branch),
        Err("proposal_branch_invalid")
    );

    let mut bad_text = valid.clone();
    bad_text.steps[0].question = "x".repeat(2_001);
    assert_eq!(
        validate_proposal(&view(), &bad_text),
        Err("proposal_step_invalid")
    );

    let mut empty_normalized_question = valid.clone();
    empty_normalized_question.steps[0].question = "???".into();
    assert_eq!(
        validate_proposal(&view(), &empty_normalized_question),
        Err("proposal_step_invalid")
    );

    let mut duplicate_profiles = valid.clone();
    duplicate_profiles.steps[0]
        .profiles
        .push("general-research".into());
    assert_eq!(
        validate_proposal(&view(), &duplicate_profiles),
        Err("proposal_step_invalid")
    );

    let mut duplicate = valid.clone();
    campaign.steps.push(step(
        Some(OutcomeClass::Failed),
        duplicate.steps[0].question.trim_end_matches('?'),
    ));
    assert_eq!(
        validate_proposal(&campaign, &duplicate),
        Err("proposal_step_invalid")
    );
    campaign.steps.clear();
    duplicate.steps[0].question = "Try method 9".into();
    assert_eq!(
        validate_proposal(&campaign, &duplicate),
        Err("proposal_number_unowned")
    );

    let mut bad_ref = valid.clone();
    bad_ref.records.push(
        serde_json::from_value(json!({
            "record_id":Uuid::new_v4(),"kind":"lesson","text":"Keep the bounded method",
            "status":"open","refs":[format!("step:{}",Uuid::new_v4())]
        }))
        .unwrap(),
    );
    assert_eq!(
        validate_proposal(&campaign, &bad_ref),
        Err("proposal_record_invalid")
    );

    let mut promoted = valid.clone();
    promoted.records.push(
        serde_json::from_value(json!({
            "record_id":Uuid::new_v4(),"kind":"prospect","text":"Keep for person review",
            "status":"promoted","refs":[]
        }))
        .unwrap(),
    );
    assert_eq!(
        validate_proposal(&campaign, &promoted),
        Err("proposal_record_invalid")
    );

    let mut bad_relation = valid.clone();
    bad_relation.relations.push(
        serde_json::from_value(json!({
            "relation_id":Uuid::new_v4(),"kind":"supports",
            "from_record":Uuid::new_v4(),"to_record":Uuid::new_v4()
        }))
        .unwrap(),
    );
    assert_eq!(
        validate_proposal(&campaign, &bad_relation),
        Err("proposal_relation_invalid")
    );

    let mut bad_close = valid.clone();
    bad_close.steps.clear();
    bad_close
        .close_branches
        .push(serde_json::from_value(json!({"branch_id":BRANCH,"reason":""})).unwrap());
    assert_eq!(
        validate_proposal(&campaign, &bad_close),
        Err("proposal_close_invalid")
    );

    let mut embedded_number = valid.clone();
    embedded_number.steps[0].question = "Test marker B9 with the supported method".into();
    assert_eq!(
        validate_proposal(&campaign, &embedded_number),
        Err("proposal_number_unowned")
    );

    campaign.branches.push(branch(1));
    campaign
        .steps
        .push(step(Some(OutcomeClass::Failed), &valid.steps[0].question));
    campaign.branches[0].state = "waiting".into();
    let mut repeated = valid.clone();
    repeated.open_branches.clear();
    repeated.steps[0].question =
        "Which other supported pattern should the campaign test next?".into();
    assert_eq!(
        validate_proposal(&campaign, &repeated),
        Err("proposal_approach_repeated")
    );
}

#[tokio::test]
async fn scripted_campaign_starts_records_results_and_stops_on_window() {
    let mut campaign = view();
    let proposal: Proposal = serde_json::from_str(include_str!(
        "fixtures/broker_campaigns/scripted_planner.json"
    ))
    .unwrap();
    let mut planner = ScriptedPlanner::new([proposal.clone()]);
    let proposed = planner.propose(&campaign).await.unwrap();
    validate_proposal(&campaign, &proposed).unwrap();
    let change = proposal_change(&proposed);
    assert_eq!(change["branches"].as_array().unwrap().len(), 1);
    assert_eq!(proposed.steps.len(), 1);

    campaign.branches.push(branch(11));
    campaign.steps = (0..11)
        .map(|index| {
            step(
                Some(OutcomeClass::Failed),
                &format!("Prior question {index}"),
            )
        })
        .collect();
    campaign.steps.push(step(None, &proposed.steps[0].question));
    let change = reconciliation_change(
        &campaign,
        &[finished(outcome(
            "review_required",
            Some("answered"),
            None,
            2,
        ))],
    );
    assert_eq!(change["finished_steps"][0]["outcome_class"], "evidence");
    assert_eq!(change["records"].as_array().unwrap().len(), 3);
    assert_eq!(change["records"][1]["kind"], "finding");
    assert_eq!(change["records"][1]["refs"], json!([format!("run:{RUN}")]));

    let mut needs_view = view();
    needs_view.branches.push(branch(0));
    needs_view.steps.push(step(None, "Question needing review"));
    let needs_person = reconciliation_change(
        &needs_view,
        &[finished(outcome(
            "review_required",
            None,
            Some("review_required"),
            0,
        ))],
    );
    assert!(
        needs_person["records"]
            .as_array()
            .unwrap()
            .iter()
            .any(|record| record["kind"] == "request")
    );
    assert!(needs_person["campaign_state"].is_null());

    let stop = reconciliation_change(&campaign, &[finished(outcome("failed", None, None, 0))]);
    assert_eq!(stop["campaign_state"]["state"], "stopping");
    assert_eq!(stop["campaign_state"]["reason"], "campaign_exhausted");
}

fn person(subject: &str, email: &str) -> VerifiedIdentity {
    VerifiedIdentity {
        issuer: "https://identity.example.invalid".into(),
        subject: subject.into(),
        email: email.into(),
        display_name: None,
        class: PrincipalClass::Person,
    }
}

fn scope(profiles: &[&str]) -> Value {
    json!({
        "starting_points":["qualification-reference"],
        "profiles":profiles,
        "workspace":{"schema_version":1,"profile":"qualification","image_digest":format!("sha256:{}","a".repeat(64)),
            "source_digest":format!("sha256:{}","b".repeat(64)),"input_refs":["input://qualification/reference"],
            "tool_refs":["tool://qualification/reference"],"preparation_receipt":null}
    })
}

fn assert_constraint_refusal(
    result: Result<u64, tokio_postgres::Error>,
    sqlstate: &str,
    constraint: &str,
) {
    let error = result.expect_err("database statement unexpectedly succeeded");
    let database = error
        .as_db_error()
        .expect("database statement did not return a PostgreSQL error");
    assert_eq!(database.code().code(), sqlstate);
    assert_eq!(database.constraint(), Some(constraint));
}

fn assert_message_refusal(
    result: Result<u64, tokio_postgres::Error>,
    sqlstate: &str,
    message: &str,
) {
    let error = result.expect_err("database statement unexpectedly succeeded");
    let database = error
        .as_db_error()
        .expect("database statement did not return a PostgreSQL error");
    assert_eq!(database.code().code(), sqlstate);
    assert_eq!(database.message(), message);
}

#[test]
fn continued_sql_literals_keep_token_separators() {
    let source = include_str!("broker_campaigns.rs");
    let unseparated: Vec<usize> = source
        .lines()
        .enumerate()
        .filter_map(|(index, line)| {
            (line.ends_with('\\') && !line.ends_with(" \\")).then_some(index + 1)
        })
        .collect();
    assert!(unseparated.is_empty(), "lines: {unseparated:?}");
}

#[test]
fn database_refusals_do_not_accept_arbitrary_errors() {
    let source = include_str!("broker_campaigns.rs");
    let generic_error_check = [".is_", "err()"].concat();
    assert!(!source.contains(&generic_error_check));
}

#[tokio::test]
#[ignore = "uses isolated PostgreSQL in qualify_remote_broker.sh campaigns"]
async fn campaign_database_contract_is_capability_scoped_and_generation_fenced() {
    // The planner's model client is a rustls reqwest client; the binaries install ring, so the test does too.
    let _ = rustls::crypto::ring::default_provider().install_default();
    let dsn = fs::read_to_string(
        env::var_os("BROKER_QUALIFICATION_MIGRATION_DSN_FILE").expect("private test DSN required"),
    )
    .expect("private test DSN unreadable");
    let config: Config = dsn.parse().unwrap();
    assert_eq!(config.get_dbname(), Some("broker_qual"));
    assert!(matches!(config.get_hosts(),[Host::Tcp(host)] if host=="127.0.0.1"));
    let (owner, connection) = config.connect(NoTls).await.unwrap();
    let owner_connection = tokio::spawn(connection);
    owner.batch_execute(
        "SELECT set_config('enrichedlab.tenant_id','research-site',false); \
         INSERT INTO enrichedlab_agent.broker_principal_invitations(tenant_id,email,group_key,role) VALUES \
         ('research-site','campaign-nonholder@example.invalid','pilot-lab','researcher'), \
         ('research-site','campaign-service@example.invalid','pilot-lab','researcher') ON CONFLICT DO NOTHING;"
    ).await.unwrap();
    let api_dsn = dsn.replace("user=postgres", "user=broker_api_qual");
    let director_dsn = dsn.replace("user=postgres", "user=broker_director_qual");
    let api = Store::connect(&api_dsn, "research-site".into(), 4).unwrap();
    let director = Store::connect(&director_dsn, "research-site".into(), 4).unwrap();
    api.ready_for_campaign_api().await.unwrap();
    director.ready_for_campaign_director().await.unwrap();
    owner
        .batch_execute(
            "CREATE ROLE broker_director_parent_qual NOLOGIN; \
             GRANT broker_director_parent_qual TO broker_director_qual;",
        )
        .await
        .unwrap();
    assert_eq!(
        director
            .ready_for_campaign_director()
            .await
            .unwrap_err()
            .code,
        "campaign_director_role_unsafe"
    );
    owner
        .batch_execute(
            "REVOKE broker_director_parent_qual FROM broker_director_qual; \
             DROP ROLE broker_director_parent_qual;",
        )
        .await
        .unwrap();
    director.ready_for_campaign_director().await.unwrap();
    owner
        .batch_execute(
            "GRANT SELECT ON enrichedlab_agent.broker_campaigns TO broker_director_qual;",
        )
        .await
        .unwrap();
    assert_eq!(
        director
            .ready_for_campaign_director()
            .await
            .unwrap_err()
            .code,
        "campaign_director_role_unsafe"
    );
    owner
        .batch_execute(
            "REVOKE SELECT ON enrichedlab_agent.broker_campaigns FROM broker_director_qual;",
        )
        .await
        .unwrap();
    director.ready_for_campaign_director().await.unwrap();
    let holder = api
        .resolve_principal(&person("campaign-holder", "researcher@example.invalid"))
        .await
        .unwrap();
    let nonholder = api
        .resolve_principal(&person(
            "campaign-nonholder",
            "campaign-nonholder@example.invalid",
        ))
        .await
        .unwrap();
    let service = api
        .resolve_principal(&VerifiedIdentity {
            issuer: "kubernetes://serviceaccount".into(),
            subject: "system:serviceaccount:qualification:campaign".into(),
            email: "campaign-service@example.invalid".into(),
            display_name: None,
            class: PrincipalClass::ServiceAccount,
        })
        .await
        .unwrap();
    owner.execute(
        "INSERT INTO enrichedlab_agent.broker_capability_grants(tenant_id,email,group_key,capability) \
         VALUES('research-site','researcher@example.invalid','pilot-lab','research.discovery.autonomous'), \
               ('research-site','campaign-service@example.invalid','pilot-lab','research.discovery.autonomous')",
        &[],
    ).await.unwrap();
    let budgets =
        json!({"wall_seconds":3600,"max_runs":2,"max_concurrent_runs":1,"max_turns_per_run":8});
    let rules = json!({"change_approach_repeats":2,"branch_window":4,"campaign_window":12,"planner_refused_cycles":3});
    let denied = Uuid::new_v4();
    assert_eq!(
        api.create_campaign(
            nonholder,
            denied,
            "pilot-lab",
            "Bounded qualification",
            "",
            &scope(&["general-research"]),
            &budgets,
            &rules
        )
        .await
        .unwrap_err()
        .code,
        "campaign_capability_required"
    );
    assert_eq!(
        api.create_campaign(
            service,
            Uuid::new_v4(),
            "pilot-lab",
            "Bounded qualification",
            "",
            &scope(&["general-research"]),
            &budgets,
            &rules
        )
        .await
        .unwrap_err()
        .code,
        "campaign_capability_required"
    );
    assert_eq!(
        api.create_campaign(
            holder,
            Uuid::new_v4(),
            "pilot-lab",
            "Bounded qualification",
            "",
            &scope(&["not-granted"]),
            &budgets,
            &rules
        )
        .await
        .unwrap_err()
        .code,
        "campaign_invalid"
    );

    let campaign = Uuid::new_v4();
    api.create_campaign(
        holder,
        campaign,
        "pilot-lab",
        "Bounded qualification",
        "",
        &scope(&["general-research"]),
        &budgets,
        &rules,
    )
    .await
    .unwrap();
    api.campaign_transition("start", holder, campaign, None)
        .await
        .unwrap();
    let claim = director
        .claim_campaign("campaign-qualification", 90)
        .await
        .unwrap()
        .unwrap();
    let root = Uuid::new_v4();
    director.apply_campaign_change(campaign, "campaign-qualification", claim.generation, &json!({
        "branches":[{"branch_id":root,"parent_id":null,"direction":"Initial qualification","rationale":"Exercise one bounded run"}]
    })).await.unwrap();
    let first_records: Vec<Value> = (0..100)
        .map(|index| {
            json!({"record_id":Uuid::new_v4(),"kind":"lesson",
                "text":format!("Qualification record {index}"),"origin":"director",
                "status":"open","refs":[]})
        })
        .collect();
    let rule_events: Vec<Value> = (0..51)
        .map(|count| {
            json!({"kind":"rule_fired","detail":{"rule":"qualification_notice",
                "count":count,"threshold":51}})
        })
        .collect();
    director
        .apply_campaign_change(
            campaign,
            "campaign-qualification",
            claim.generation,
            &json!({"records":first_records,"events":rule_events}),
        )
        .await
        .unwrap();
    director
        .apply_campaign_change(
            campaign,
            "campaign-qualification",
            claim.generation,
            &json!({"records":[{"record_id":Uuid::new_v4(),"kind":"lesson",
                "text":"Last qualification record","origin":"director","status":"open","refs":[]}]}),
        )
        .await
        .unwrap();
    let bounded_snapshot = director
        .campaign_director_view(campaign, "campaign-qualification", claim.generation)
        .await
        .unwrap();
    assert_eq!(bounded_snapshot["records"].as_array().unwrap().len(), 100);
    assert_eq!(bounded_snapshot["records_omitted"], 1);
    assert_eq!(
        bounded_snapshot["rule_notices"].as_array().unwrap().len(),
        51
    );
    assert_eq!(bounded_snapshot["rule_notices_omitted"], 0);
    let planner_call = Uuid::new_v4();
    let planner_context = br#"{"sections":[]}"#;
    let planner_reply = br#"{"branches_open":[],"steps":[],"branches_close":[],"records":[],"summary":"No proposal"}"#;
    let started_call = director
        .start_campaign_planner_call(
            campaign,
            "campaign-qualification",
            claim.generation,
            planner_call,
            planner_context,
            "qualification-model",
        )
        .await
        .unwrap();
    let kept_call = director
        .finish_campaign_planner_call(
            campaign,
            "campaign-qualification",
            claim.generation,
            planner_call,
            Some(planner_reply),
            Some("qualification-model"),
            "accepted",
        )
        .await
        .unwrap();
    let context_digest = started_call["context_sha256"].as_str().unwrap();
    let reply_digest = kept_call["reply_sha256"].as_str().unwrap();
    let inflight_call = Uuid::new_v4();
    let inflight_context = br#"{"sections":[{"section":"inflight"}]}"#;
    let inflight_started = director
        .start_campaign_planner_call(
            campaign,
            "campaign-qualification",
            claim.generation,
            inflight_call,
            inflight_context,
            "qualification-model",
        )
        .await
        .unwrap();
    let inflight_digest = inflight_started["context_sha256"].as_str().unwrap();
    let calls = api
        .campaign_planner_calls(holder, campaign, 0, 100)
        .await
        .unwrap();
    assert_eq!(calls.items.len(), 2);
    assert_eq!(calls.items[0]["call_id"], planner_call.to_string());
    assert_eq!(calls.items[0]["outcome_code"], "accepted");
    assert_eq!(calls.items[1]["call_id"], inflight_call.to_string());
    assert!(calls.items[1]["outcome_code"].is_null());
    assert_eq!(calls.items[1]["configured_model"], "qualification-model");
    assert!(calls.items[1]["started_at"].is_string());
    assert_eq!(calls.items[1]["lease_generation"], claim.generation);
    assert_eq!(
        calls.items[1]["input_size_bytes"].as_u64(),
        Some(inflight_context.len() as u64)
    );
    assert_eq!(
        api.campaign_planner_text(holder, campaign, inflight_digest)
            .await
            .unwrap()["text"],
        std::str::from_utf8(inflight_context).unwrap()
    );
    assert_eq!(
        api.campaign_planner_text(holder, campaign, context_digest)
            .await
            .unwrap()["text"],
        std::str::from_utf8(planner_context).unwrap()
    );
    assert_eq!(
        api.campaign_planner_text(holder, campaign, reply_digest)
            .await
            .unwrap()["text"],
        std::str::from_utf8(planner_reply).unwrap()
    );
    assert_eq!(
        api.campaign_planner_calls(nonholder, campaign, 0, 100)
            .await
            .unwrap_err()
            .code,
        "campaign_not_found"
    );
    assert_eq!(
        api.campaign_planner_text(nonholder, campaign, context_digest)
            .await
            .unwrap_err()
            .code,
        "campaign_not_found"
    );
    assert_message_refusal(
        owner
            .execute(
                "UPDATE enrichedlab_agent.broker_campaign_planner_calls SET outcome_code='changed' \
                 WHERE tenant_id='research-site' AND campaign_id=$1 AND call_id=$2",
                &[&campaign, &planner_call],
            )
            .await,
        "P0001",
        "broker task events are append-only",
    );
    assert_message_refusal(
        owner
            .execute(
                "UPDATE enrichedlab_agent.broker_campaign_planner_call_starts SET model='changed' \
                 WHERE tenant_id='research-site' AND campaign_id=$1 AND call_id=$2",
                &[&campaign, &planner_call],
            )
            .await,
        "P0001",
        "broker task events are append-only",
    );
    assert_eq!(
        director
            .apply_campaign_change(
                campaign,
                "campaign-qualification",
                claim.generation - 1,
                &json!({})
            )
            .await
            .unwrap_err()
            .code,
        "campaign_lease_fence_lost"
    );
    assert_eq!(
        director
            .start_campaign_planner_call(
                campaign,
                "campaign-qualification",
                claim.generation - 1,
                Uuid::new_v4(),
                planner_context,
                "qualification-model",
            )
            .await
            .unwrap_err()
            .code,
        "campaign_lease_fence_lost"
    );
    assert_eq!(
        director
            .finish_campaign_planner_call(
                campaign,
                "campaign-qualification",
                claim.generation - 1,
                inflight_call,
                None,
                None,
                "planner_model_unavailable",
            )
            .await
            .unwrap_err()
            .code,
        "campaign_lease_fence_lost"
    );
    assert_eq!(
        director
            .start_campaign_step(
                campaign,
                "campaign-qualification",
                claim.generation - 1,
                Uuid::new_v4(),
                root,
                "explore",
                "Stale generation question",
                &["general-research".into()],
                Uuid::new_v4(),
                Uuid::new_v4()
            )
            .await
            .unwrap_err()
            .code,
        "campaign_lease_fence_lost"
    );
    assert_eq!(
        director
            .start_campaign_step(
                campaign,
                "campaign-qualification",
                claim.generation,
                Uuid::new_v4(),
                root,
                "explore",
                "Outside profile",
                &["not-granted".into()],
                Uuid::new_v4(),
                Uuid::new_v4()
            )
            .await
            .unwrap_err()
            .code,
        "campaign_scope_forbidden"
    );
    let project = Uuid::new_v4();
    let campaign_step = Uuid::new_v4();
    let run = Uuid::new_v4();
    director
        .start_campaign_step(
            campaign,
            "campaign-qualification",
            claim.generation,
            campaign_step,
            root,
            "explore",
            "Run the bounded qualification question",
            &["general-research".into()],
            project,
            run,
        )
        .await
        .unwrap();
    let project_contract = owner.query_one(
        "SELECT p.visibility,p.owner_id=c.campaign_principal, \
         (SELECT capabilities FROM enrichedlab_agent.broker_project_grants g WHERE g.tenant_id=p.tenant_id AND g.project_id=p.project_id AND g.principal_id=c.started_by) \
         ,(SELECT role FROM enrichedlab_agent.broker_group_memberships m WHERE m.tenant_id=c.tenant_id AND m.group_key=c.group_key AND m.principal_id=c.campaign_principal) \
         ,(SELECT max_turns FROM enrichedlab_agent.broker_research_runs r WHERE r.tenant_id=p.tenant_id AND r.run_id=$3) \
         ,(SELECT max_parallel FROM enrichedlab_agent.broker_research_runs r WHERE r.tenant_id=p.tenant_id AND r.run_id=$3) \
         FROM enrichedlab_agent.broker_projects p JOIN enrichedlab_agent.broker_campaigns c USING(tenant_id) \
         WHERE p.tenant_id='research-site' AND p.project_id=$1 AND c.campaign_id=$2", &[&project,&campaign,&run]
    ).await.unwrap();
    assert_eq!(project_contract.get::<_, String>(0), "private");
    assert!(project_contract.get::<_, bool>(1));
    assert_eq!(project_contract.get::<_, Vec<String>>(2), vec!["read"]);
    assert_eq!(project_contract.get::<_, String>(3), "researcher");
    assert_eq!(project_contract.get::<_, i32>(4), 8);
    assert_eq!(project_contract.get::<_, i32>(5), 1);
    owner
        .batch_execute(
            "ALTER TABLE enrichedlab_agent.broker_campaign_steps \
               DROP CONSTRAINT broker_campaign_steps_state_shape; \
             ALTER TABLE enrichedlab_agent.broker_campaign_steps \
               DROP CONSTRAINT broker_campaign_steps_checkpoint_revision_fkey; \
             ALTER TABLE enrichedlab_agent.broker_campaign_steps \
               DROP COLUMN checkpoint_revision; \
             ALTER TABLE enrichedlab_agent.broker_campaign_steps \
               ADD CONSTRAINT broker_campaign_steps_old_state_shape CHECK ( \
                 (state='planned' AND project_id IS NULL AND run_id IS NULL AND started_at IS NULL \
                   AND outcome_class IS NULL AND outcome IS NULL AND finished_at IS NULL) \
                 OR (state='running' AND project_id IS NOT NULL AND run_id IS NOT NULL \
                   AND started_at IS NOT NULL AND outcome_class IS NULL AND outcome IS NULL \
                   AND finished_at IS NULL) \
                 OR (state='finished' AND project_id IS NOT NULL AND run_id IS NOT NULL \
                   AND started_at IS NOT NULL AND outcome_class IS NOT NULL AND outcome IS NOT NULL \
                   AND finished_at IS NOT NULL));",
        )
        .await
        .unwrap();
    let broadened_grant = owner
        .execute(
            "UPDATE enrichedlab_agent.broker_project_grants \
             SET capabilities=ARRAY['read','review','steer','cancel']::text[] \
             WHERE tenant_id='research-site' AND project_id=$1 AND principal_id=$2",
            &[&project, &holder],
        )
        .await
        .unwrap();
    assert_eq!(broadened_grant, 1);
    owner
        .batch_execute(include_str!("../migrations/postgres/066_campaigns.sql"))
        .await
        .unwrap();
    owner
        .batch_execute(include_str!(
            "../migrations/postgres/067_campaign_planner.sql"
        ))
        .await
        .unwrap();
    director.ready_for_campaign_director().await.unwrap();
    let upgraded_contract = owner
        .query_one(
            "SELECT s.checkpoint_revision,c.state,r.status,g.capabilities, \
               (SELECT count(*) FROM pg_catalog.pg_constraint constraint_row \
                WHERE constraint_row.conrelid='enrichedlab_agent.broker_campaign_steps'::regclass \
                  AND constraint_row.conname IN ('broker_campaign_steps_state_shape', \
                    'broker_campaign_steps_checkpoint_revision_fkey')) \
             FROM enrichedlab_agent.broker_campaign_steps s \
             JOIN enrichedlab_agent.broker_campaigns c \
               ON c.tenant_id=s.tenant_id AND c.campaign_id=s.campaign_id \
             JOIN enrichedlab_agent.broker_research_runs r \
               ON r.tenant_id=s.tenant_id AND r.run_id=s.run_id \
             JOIN enrichedlab_agent.broker_project_grants g \
               ON g.tenant_id=s.tenant_id AND g.project_id=s.project_id AND g.principal_id=$3 \
             WHERE s.tenant_id='research-site' AND s.campaign_id=$1 AND s.step_id=$2",
            &[&campaign, &campaign_step, &holder],
        )
        .await
        .unwrap();
    assert_eq!(upgraded_contract.get::<_, i64>(0), 1);
    assert_eq!(upgraded_contract.get::<_, String>(1), "running");
    assert_eq!(upgraded_contract.get::<_, String>(2), "queued");
    assert_eq!(upgraded_contract.get::<_, Vec<String>>(3), vec!["read"]);
    assert_eq!(upgraded_contract.get::<_, i64>(4), 2);
    let initial_state_value: Value = owner
        .query_one(
            "SELECT state FROM enrichedlab_agent.broker_project_checkpoints \
             WHERE tenant_id='research-site' AND project_id=$1 AND revision=1",
            &[&project],
        )
        .await
        .unwrap()
        .get(0);
    let initial_state: ResearchState = serde_json::from_value(initial_state_value.clone()).unwrap();
    let mut replacement_state = initial_state.clone();
    replacement_state.goal = "Replacement outside the campaign snapshot".into();
    assert_eq!(
        api.checkpoint_project(
            holder,
            project,
            &CheckpointRequest {
                expected_revision: 1,
                request_id: Uuid::new_v4(),
                state: replacement_state.clone(),
            },
        )
        .await
        .unwrap_err()
        .code,
        "campaign_checkpoint_forbidden"
    );
    let revoked_rows = owner
        .execute(
            "UPDATE enrichedlab_agent.broker_capability_grants \
             SET enabled=false,revoked_at=clock_timestamp() \
             WHERE tenant_id='research-site' AND email='researcher@example.invalid' \
               AND group_key='pilot-lab' AND capability='research.discovery.autonomous'",
            &[],
        )
        .await
        .unwrap();
    assert_eq!(revoked_rows, 1);
    let holder_still_capable: bool = owner
        .query_one(
            "SELECT enrichedlab_agent.broker_campaign_has_capability( \
               'research-site',$1,'pilot-lab')",
            &[&holder],
        )
        .await
        .unwrap()
        .get(0);
    assert!(!holder_still_capable);
    assert_eq!(
        api.campaign_planner_calls(holder, campaign, 0, 100)
            .await
            .unwrap_err()
            .code,
        "campaign_not_found"
    );
    assert_eq!(
        api.campaign_planner_text(holder, campaign, context_digest)
            .await
            .unwrap_err()
            .code,
        "campaign_not_found"
    );
    assert_eq!(
        api.checkpoint_project(
            holder,
            project,
            &CheckpointRequest {
                expected_revision: 1,
                request_id: Uuid::new_v4(),
                state: replacement_state,
            },
        )
        .await
        .unwrap_err()
        .code,
        "campaign_checkpoint_forbidden"
    );
    let restored_rows = owner
        .execute(
            "UPDATE enrichedlab_agent.broker_capability_grants \
             SET enabled=true,revoked_at=NULL \
             WHERE tenant_id='research-site' AND email='researcher@example.invalid' \
               AND group_key='pilot-lab' AND capability='research.discovery.autonomous'",
            &[],
        )
        .await
        .unwrap();
    assert_eq!(restored_rows, 1);
    let campaign_principal: Uuid = owner
        .query_one(
            "SELECT campaign_principal FROM enrichedlab_agent.broker_campaigns \
             WHERE tenant_id='research-site' AND campaign_id=$1",
            &[&campaign],
        )
        .await
        .unwrap()
        .get(0);
    let mut scope_replacements = Vec::new();
    let mut changed_goal = initial_state_value.clone();
    changed_goal["goal"] = json!("Changed campaign goal");
    scope_replacements.push(changed_goal);
    let mut changed_constraints = initial_state_value.clone();
    changed_constraints["constraints"] = json!(["Changed campaign constraint"]);
    scope_replacements.push(changed_constraints);
    let mut changed_workspace = initial_state_value.clone();
    changed_workspace["workspace"]["input_refs"] = json!(["input://outside/snapshot"]);
    scope_replacements.push(changed_workspace);
    for replacement in scope_replacements {
        assert_message_refusal(
            owner
                .execute(
                    "SELECT enrichedlab_agent.broker_write_checkpoint( \
                       'research-site',$1,$2,1,$3,$4,'lead_turn',$5,1,NULL)",
                    &[
                        &campaign_principal,
                        &project,
                        &Uuid::new_v4(),
                        &replacement,
                        &run,
                    ],
                )
                .await,
            "22023",
            "broker:campaign_checkpoint_scope_changed",
        );
    }
    let mut trusted_descendant = initial_state_value.clone();
    trusted_descendant["next_action"] = json!("Continue with the accepted campaign scope.");
    let trusted_revision: i64 = owner
        .query_one(
            "SELECT enrichedlab_agent.broker_write_checkpoint( \
               'research-site',$1,$2,1,$3,$4,'lead_turn',$5,1,NULL)",
            &[
                &campaign_principal,
                &project,
                &Uuid::new_v4(),
                &trusted_descendant,
                &run,
            ],
        )
        .await
        .unwrap()
        .get(0);
    assert_eq!(trusted_revision, 2);
    let automatically_pinned: i64 = owner
        .query_one(
            "SELECT checkpoint_revision FROM enrichedlab_agent.broker_campaign_steps \
             WHERE tenant_id='research-site' AND campaign_id=$1 AND step_id=$2",
            &[&campaign, &campaign_step],
        )
        .await
        .unwrap()
        .get(0);
    assert_eq!(automatically_pinned, trusted_revision);
    // Make latest and pinned differ to prove the coordinator read follows the step,
    // not the project's independently maintained latest-revision pointer.
    owner
        .execute(
            "UPDATE enrichedlab_agent.broker_campaign_steps SET checkpoint_revision=1 \
             WHERE tenant_id='research-site' AND campaign_id=$1 AND step_id=$2",
            &[&campaign, &campaign_step],
        )
        .await
        .unwrap();
    let coordinator_claim: Value = owner
        .query_one(
            "SELECT enrichedlab_agent.broker_claim_research_run( \
               'research-site','campaign-coordinator-qualification',90)",
            &[],
        )
        .await
        .unwrap()
        .get(0);
    let pinned_revision: i64 = owner
        .query_one(
            "SELECT checkpoint_revision FROM enrichedlab_agent.broker_campaign_steps \
             WHERE tenant_id='research-site' AND campaign_id=$1 AND step_id=$2",
            &[&campaign, &campaign_step],
        )
        .await
        .unwrap()
        .get(0);
    assert_eq!(
        coordinator_claim["run"]["initial_revision"],
        pinned_revision
    );
    assert_eq!(
        coordinator_claim["project"]["checkpoint"]["revision"],
        pinned_revision
    );
    assert_eq!(coordinator_claim["project"]["revision"], trusted_revision);
    owner
        .execute(
            "UPDATE enrichedlab_agent.broker_campaign_steps SET checkpoint_revision=$3 \
             WHERE tenant_id='research-site' AND campaign_id=$1 AND step_id=$2",
            &[&campaign, &campaign_step, &trusted_revision],
        )
        .await
        .unwrap();
    // Both SELECTs resolve one campaign principal and otherwise valid target keys; only the
    // reserved campaign email rules refuse these inserts.
    let capability_campaign_constraint: String = owner
        .query_one(
            "SELECT conname FROM pg_constraint \
             WHERE conrelid='enrichedlab_agent.broker_capability_grants'::regclass \
               AND contype='c' AND pg_get_constraintdef(oid) LIKE '%campaign.invalid%'",
            &[],
        )
        .await
        .unwrap()
        .get(0);
    assert_constraint_refusal(
        owner
            .execute(
                "INSERT INTO enrichedlab_agent.broker_capability_grants(tenant_id,email,group_key,capability) \
                 SELECT c.tenant_id,p.email,c.group_key,'research.discovery.autonomous' \
                 FROM enrichedlab_agent.broker_campaigns c JOIN enrichedlab_agent.broker_principals p \
                   ON p.tenant_id=c.tenant_id AND p.principal_id=c.campaign_principal \
                 WHERE c.tenant_id='research-site' AND c.campaign_id=$1",
                &[&campaign],
            )
            .await,
        "23514",
        &capability_campaign_constraint,
    );
    assert_constraint_refusal(
        owner
            .execute(
                "INSERT INTO enrichedlab_agent.broker_principal_invitations(tenant_id,email,group_key,role) \
                 SELECT c.tenant_id,p.email,c.group_key,'researcher' \
                 FROM enrichedlab_agent.broker_campaigns c JOIN enrichedlab_agent.broker_principals p \
                   ON p.tenant_id=c.tenant_id AND p.principal_id=c.campaign_principal \
                 WHERE c.tenant_id='research-site' AND c.campaign_id=$1",
                &[&campaign],
            )
            .await,
        "23514",
        "broker_invitations_no_campaign",
    );
    let concurrency_refusal = director
        .start_campaign_step(
            campaign,
            "campaign-qualification",
            claim.generation,
            Uuid::new_v4(),
            root,
            "explore",
            "Another question",
            &["general-research".into()],
            Uuid::new_v4(),
            Uuid::new_v4(),
        )
        .await
        .unwrap();
    assert_eq!(concurrency_refusal["started"], false);
    assert_eq!(
        concurrency_refusal["code"],
        "campaign_concurrency_exhausted"
    );
    assert_eq!(concurrency_refusal["state"], "running");
    let capacity_state = owner
        .query_one(
            "SELECT state,run_count,active_runs FROM enrichedlab_agent.broker_campaigns \
             WHERE tenant_id='research-site' AND campaign_id=$1",
            &[&campaign],
        )
        .await
        .unwrap();
    assert_eq!(capacity_state.get::<_, String>(0), "running");
    assert_eq!(capacity_state.get::<_, i32>(1), 1);
    assert_eq!(capacity_state.get::<_, i32>(2), 1);
    assert_eq!(
        api.campaign(nonholder, campaign).await.unwrap_err().code,
        "campaign_not_found"
    );
    let other_tenant_api = Store::connect(&api_dsn, "other-tenant".into(), 1).unwrap();
    assert_eq!(
        other_tenant_api
            .campaign(holder, campaign)
            .await
            .unwrap_err()
            .code,
        "campaign_not_found"
    );
    assert_eq!(
        other_tenant_api
            .campaign_planner_calls(holder, campaign, 0, 100)
            .await
            .unwrap_err()
            .code,
        "campaign_not_found"
    );
    assert_eq!(
        other_tenant_api
            .campaign_planner_text(holder, campaign, context_digest)
            .await
            .unwrap_err()
            .code,
        "campaign_not_found"
    );
    // Matching events exist and both mutations are otherwise valid; the append-only trigger
    // is the only expected refusal.
    let update = owner.execute("UPDATE enrichedlab_agent.broker_campaign_events SET detail='{}' WHERE tenant_id='research-site' AND campaign_id=$1", &[&campaign]).await;
    assert_message_refusal(update, "P0001", "broker task events are append-only");
    let delete = owner.execute("DELETE FROM enrichedlab_agent.broker_campaign_events WHERE tenant_id='research-site' AND campaign_id=$1", &[&campaign]).await;
    assert_message_refusal(delete, "P0001", "broker task events are append-only");

    let queued_task = Uuid::new_v4();
    let leased_task = Uuid::new_v4();
    owner
        .execute(
            "INSERT INTO enrichedlab_agent.broker_tasks( \
               tenant_id,task_id,principal_id,group_key,profile_key,objective,status,desired_status, \
               attempt,max_task_attempts,logical_agent,profile_snapshot,idempotency_key,request_sha256, \
               lease_owner,lease_generation,lease_expires_at) VALUES \
             ('research-site',$1::uuid,$3,'pilot-lab','general-research','Queued campaign task','queued','run', \
               0,1,'task-'||replace($1::uuid::text,'-',''),'{}'::jsonb,'campaign-queued-'||$1::uuid::text,repeat('a',64),NULL,0,NULL), \
             ('research-site',$2::uuid,$3,'pilot-lab','general-research','Leased campaign task','leased','run', \
               0,1,'task-'||replace($2::uuid::text,'-',''),'{}'::jsonb,'campaign-leased-'||$2::uuid::text,repeat('b',64), \
               'campaign-task-worker',1,clock_timestamp()+interval '5 minutes')",
            &[&queued_task, &leased_task, &campaign_principal],
        )
        .await
        .unwrap();
    owner
        .execute(
            "INSERT INTO enrichedlab_agent.broker_project_tasks \
               (tenant_id,project_id,task_id,action_key,parent_task_id) VALUES \
             ('research-site',$1,$2,'queued-drain',NULL), \
             ('research-site',$1,$3,'leased-drain',NULL)",
            &[&project, &queued_task, &leased_task],
        )
        .await
        .unwrap();
    owner
        .execute(
            "UPDATE enrichedlab_agent.broker_research_runs \
             SET status='paused',code='qualification_paused',generation=generation+1, \
                 lease_owner=NULL,lease_until=NULL \
             WHERE tenant_id='research-site' AND project_id=$1",
            &[&project],
        )
        .await
        .unwrap();
    assert_eq!(
        api.campaign_transition("stop", holder, campaign, None)
            .await
            .unwrap()["state"],
        "stopping"
    );
    let finished_after_fence = director
        .finish_campaign_planner_call(
            campaign,
            "campaign-qualification",
            claim.generation,
            inflight_call,
            None,
            None,
            "planner_model_unavailable",
        )
        .await
        .unwrap();
    assert_eq!(finished_after_fence["state"], "finished");
    assert_eq!(
        director
            .apply_campaign_change(
                campaign,
                "campaign-qualification",
                claim.generation,
                &json!({}),
            )
            .await
            .unwrap_err()
            .code,
        "campaign_lease_fence_lost"
    );
    assert_eq!(
        owner
            .query_one(
                "SELECT outcome_code FROM enrichedlab_agent.broker_campaign_planner_calls \
                 WHERE tenant_id='research-site' AND campaign_id=$1 AND call_id=$2",
                &[&campaign, &inflight_call],
            )
            .await
            .unwrap()
            .get::<_, String>(0),
        "planner_model_unavailable"
    );
    let finishing = director
        .claim_campaign("campaign-qualification", 90)
        .await
        .unwrap()
        .unwrap();
    let blocked_finished = director
        .campaign_finished_steps(campaign, "campaign-qualification", finishing.generation)
        .await
        .unwrap();
    assert!(blocked_finished.is_empty());
    // Even a stale director that had already marked the paused step finished cannot
    // finalize stop while either submitted task is still active.
    owner
        .execute(
            "UPDATE enrichedlab_agent.broker_campaign_steps \
             SET state='finished',outcome_class='stalled', \
                 outcome='{\"run_state\":\"paused\",\"findings\":0}'::jsonb, \
                 finished_at=clock_timestamp() \
             WHERE tenant_id='research-site' AND campaign_id=$1 AND step_id=$2",
            &[&campaign, &campaign_step],
        )
        .await
        .unwrap();
    let draining_state: String = owner
        .query_one(
            "SELECT enrichedlab_agent.broker_campaign_finalize_lifecycle('research-site',$1)",
            &[&campaign],
        )
        .await
        .unwrap()
        .get(0);
    assert_eq!(draining_state, "stopping");
    let draining_principal = owner.query_one(
        "SELECT c.state,p.enabled,(SELECT count(*) FROM enrichedlab_agent.broker_group_memberships m \
           WHERE m.tenant_id=p.tenant_id AND m.principal_id=p.principal_id) \
         FROM enrichedlab_agent.broker_campaigns c JOIN enrichedlab_agent.broker_principals p \
           ON p.tenant_id=c.tenant_id AND p.principal_id=c.campaign_principal \
         WHERE c.tenant_id='research-site' AND c.campaign_id=$1", &[&campaign]
    ).await.unwrap();
    assert_eq!(draining_principal.get::<_, String>(0), "stopping");
    assert!(draining_principal.get::<_, bool>(1));
    assert_eq!(draining_principal.get::<_, i64>(2), 1);
    owner
        .execute(
            "UPDATE enrichedlab_agent.broker_tasks \
             SET status='cancelled',desired_status='cancel',lease_owner=NULL,lease_expires_at=NULL, \
                 finished_at=clock_timestamp(),updated_at=clock_timestamp() \
             WHERE tenant_id='research-site' AND task_id=ANY($1)",
            &[&vec![queued_task, leased_task]],
        )
        .await
        .unwrap();
    let stopped_state: String = owner
        .query_one(
            "SELECT enrichedlab_agent.broker_campaign_finalize_lifecycle('research-site',$1)",
            &[&campaign],
        )
        .await
        .unwrap()
        .get(0);
    assert_eq!(stopped_state, "stopped");
    assert!(
        director
            .claim_campaign("campaign-qualification", 90)
            .await
            .unwrap()
            .is_none()
    );
    let disabled = owner.query_one(
        "SELECT p.enabled,(SELECT count(*) FROM enrichedlab_agent.broker_group_memberships m WHERE m.tenant_id=p.tenant_id AND m.principal_id=p.principal_id) \
         FROM enrichedlab_agent.broker_principals p JOIN enrichedlab_agent.broker_campaigns c ON c.tenant_id=p.tenant_id AND c.campaign_principal=p.principal_id \
         WHERE c.tenant_id='research-site' AND c.campaign_id=$1", &[&campaign]
    ).await.unwrap();
    assert!(!disabled.get::<_, bool>(0));
    assert_eq!(disabled.get::<_, i64>(1), 0);

    let terminal_campaign = Uuid::new_v4();
    let terminal_budgets =
        json!({"wall_seconds":3600,"max_runs":3,"max_concurrent_runs":2,"max_turns_per_run":4});
    api.create_campaign(
        holder,
        terminal_campaign,
        "pilot-lab",
        "Terminal branch qualification",
        "",
        &scope(&["general-research"]),
        &terminal_budgets,
        &rules,
    )
    .await
    .unwrap();
    api.campaign_transition("start", holder, terminal_campaign, None)
        .await
        .unwrap();
    let terminal_claim = director
        .claim_campaign("campaign-terminal", 90)
        .await
        .unwrap()
        .unwrap();
    let terminal_branch = Uuid::new_v4();
    director
        .apply_campaign_change(
            terminal_campaign,
            "campaign-terminal",
            terminal_claim.generation,
            &json!({"branches":[{"branch_id":terminal_branch,"parent_id":null,
                "direction":"Terminal path","rationale":"Reconcile work that finishes after closure"}]}),
        )
        .await
        .unwrap();
    let terminal_step = Uuid::new_v4();
    let terminal_project = Uuid::new_v4();
    let terminal_run = Uuid::new_v4();
    director
        .start_campaign_step(
            terminal_campaign,
            "campaign-terminal",
            terminal_claim.generation,
            terminal_step,
            terminal_branch,
            "explore",
            "Finish after this branch closes",
            &["general-research".into()],
            terminal_project,
            terminal_run,
        )
        .await
        .unwrap();
    let terminal_update = json!({"branch_updates":[{"branch_id":terminal_branch,
        "state":"concluded","reason_class":"planner_concluded",
        "reason":"No more work should start on this path"}]});
    director
        .apply_campaign_change(
            terminal_campaign,
            "campaign-terminal",
            terminal_claim.generation,
            &terminal_update,
        )
        .await
        .unwrap();
    director
        .apply_campaign_change(
            terminal_campaign,
            "campaign-terminal",
            terminal_claim.generation,
            &terminal_update,
        )
        .await
        .unwrap();
    let terminal_contract = owner.query_one(
        "SELECT b.state,b.reason_class,(SELECT count(*) FROM enrichedlab_agent.broker_campaign_events e \
           WHERE e.tenant_id=b.tenant_id AND e.campaign_id=b.campaign_id AND e.kind='branch_closed' \
             AND e.detail->>'branch_id'=$2::uuid::text) \
         FROM enrichedlab_agent.broker_campaign_branches b \
         WHERE b.tenant_id='research-site' AND b.campaign_id=$1 AND b.branch_id=$2::uuid",
        &[&terminal_campaign, &terminal_branch]
    ).await.unwrap();
    assert_eq!(terminal_contract.get::<_, String>(0), "concluded");
    assert_eq!(terminal_contract.get::<_, String>(1), "planner_concluded");
    assert_eq!(terminal_contract.get::<_, i64>(2), 2);
    assert_eq!(
        director
            .start_campaign_step(
                terminal_campaign,
                "campaign-terminal",
                terminal_claim.generation,
                Uuid::new_v4(),
                terminal_branch,
                "explore",
                "Work refused after branch closure",
                &["general-research".into()],
                Uuid::new_v4(),
                Uuid::new_v4(),
            )
            .await
            .unwrap_err()
            .code,
        "campaign_ref_invalid"
    );
    owner
        .execute(
            "UPDATE enrichedlab_agent.broker_research_runs SET status='failed',code='qualification' \
             WHERE tenant_id='research-site' AND run_id=$1",
            &[&terminal_run],
        )
        .await
        .unwrap();
    assert_eq!(
        api.campaign_transition("stop", holder, terminal_campaign, None)
            .await
            .unwrap()["state"],
        "stopping"
    );
    let late_claim = director
        .claim_campaign("campaign-terminal-late", 90)
        .await
        .unwrap()
        .unwrap();
    let late_steps = director
        .campaign_finished_steps(
            terminal_campaign,
            "campaign-terminal-late",
            late_claim.generation,
        )
        .await
        .unwrap();
    assert_eq!(late_steps.len(), 1);
    let late_step: FinishedStep = serde_json::from_value(late_steps[0].clone()).unwrap();
    let late_change = reconciliation_change(
        &serde_json::from_value(late_claim.view).unwrap(),
        &[late_step],
    );
    assert!(late_change["branch_updates"].as_array().unwrap().is_empty());
    director
        .apply_campaign_change(
            terminal_campaign,
            "campaign-terminal-late",
            late_claim.generation,
            &late_change,
        )
        .await
        .unwrap();
    assert!(
        director
            .claim_campaign("campaign-terminal-late", 90)
            .await
            .unwrap()
            .is_none()
    );
    let terminal_state: String = owner
        .query_one(
            "SELECT state FROM enrichedlab_agent.broker_campaigns \
             WHERE tenant_id='research-site' AND campaign_id=$1",
            &[&terminal_campaign],
        )
        .await
        .unwrap()
        .get(0);
    assert_eq!(terminal_state, "stopped");

    owner.execute(
        "INSERT INTO enrichedlab_agent.broker_principal_invitations(tenant_id,email,group_key,role) \
         VALUES('research-site','campaign-other-holder@example.invalid','pilot-lab','researcher')",
        &[],
    ).await.unwrap();
    let other_holder = api
        .resolve_principal(&person(
            "campaign-other-holder",
            "campaign-other-holder@example.invalid",
        ))
        .await
        .unwrap();
    owner.execute(
        "INSERT INTO enrichedlab_agent.broker_capability_grants(tenant_id,email,group_key,capability) \
         VALUES('research-site','campaign-other-holder@example.invalid','pilot-lab','research.discovery.autonomous')",
        &[],
    ).await.unwrap();
    let lifecycle = Uuid::new_v4();
    let lifecycle_budgets =
        json!({"wall_seconds":3600,"max_runs":4,"max_concurrent_runs":2,"max_turns_per_run":4});
    api.create_campaign(
        holder,
        lifecycle,
        "pilot-lab",
        "Lifecycle qualification",
        "",
        &scope(&["general-research"]),
        &lifecycle_budgets,
        &rules,
    )
    .await
    .unwrap();
    assert_eq!(
        api.campaign_transition("start", nonholder, lifecycle, None)
            .await
            .unwrap_err()
            .code,
        "campaign_not_found"
    );
    assert_eq!(
        api.campaign_transition("start", other_holder, lifecycle, None)
            .await
            .unwrap()["state"],
        "running"
    );
    assert_eq!(
        api.campaign_transition("pause", nonholder, lifecycle, None)
            .await
            .unwrap_err()
            .code,
        "campaign_not_found"
    );
    assert_eq!(
        api.campaign_transition("pause", other_holder, lifecycle, None)
            .await
            .unwrap()["state"],
        "paused"
    );
    assert_eq!(
        api.campaign_transition("resume", nonholder, lifecycle, None)
            .await
            .unwrap_err()
            .code,
        "campaign_not_found"
    );
    assert_eq!(
        api.campaign_transition("resume", holder, lifecycle, None)
            .await
            .unwrap()["state"],
        "running"
    );
    assert_eq!(
        api.campaign_transition("stop", nonholder, lifecycle, None)
            .await
            .unwrap_err()
            .code,
        "campaign_not_found"
    );
    assert_eq!(
        api.campaign_transition("stop", other_holder, lifecycle, None)
            .await
            .unwrap()["state"],
        "stopped"
    );

    let revoked_scope = Uuid::new_v4();
    api.create_campaign(
        holder,
        revoked_scope,
        "pilot-lab",
        "Current method grant qualification",
        "",
        &scope(&["general-research"]),
        &lifecycle_budgets,
        &rules,
    )
    .await
    .unwrap();
    api.campaign_transition("start", holder, revoked_scope, None)
        .await
        .unwrap();
    let revoked_claim = director
        .claim_campaign("campaign-scope", 90)
        .await
        .unwrap()
        .unwrap();
    let revoked_branch = Uuid::new_v4();
    director.apply_campaign_change(revoked_scope, "campaign-scope", revoked_claim.generation, &json!({
        "branches":[{"branch_id":revoked_branch,"parent_id":null,"direction":"Current grant","rationale":"Recheck the lab method at start"}]
    })).await.unwrap();
    owner.execute(
        "DELETE FROM enrichedlab_agent.broker_group_profile_grants \
         WHERE tenant_id='research-site' AND group_key='pilot-lab' AND profile_key='general-research'",
        &[],
    ).await.unwrap();
    assert_eq!(
        director
            .start_campaign_step(
                revoked_scope,
                "campaign-scope",
                revoked_claim.generation,
                Uuid::new_v4(),
                revoked_branch,
                "explore",
                "Recheck the current method grant",
                &["general-research".into()],
                Uuid::new_v4(),
                Uuid::new_v4(),
            )
            .await
            .unwrap_err()
            .code,
        "campaign_scope_forbidden"
    );
    owner.execute(
        "INSERT INTO enrichedlab_agent.broker_group_profile_grants(tenant_id,group_key,profile_key) \
         VALUES('research-site','pilot-lab','general-research')",
        &[],
    ).await.unwrap();
    api.campaign_transition("stop", holder, revoked_scope, None)
        .await
        .unwrap();

    owner.batch_execute(
        "INSERT INTO enrichedlab_agent.broker_research_groups \
           (tenant_id,group_key,name,max_active_agents,max_active_agents_per_principal,max_queued_tasks,retention_days,enabled) \
         VALUES('research-site','other-lab','Other lab',2,1,10,30,true); \
         INSERT INTO enrichedlab_agent.broker_group_profile_grants(tenant_id,group_key,profile_key) \
         VALUES('research-site','other-lab','general-research'); \
         INSERT INTO enrichedlab_agent.broker_principal_invitations(tenant_id,email,group_key,role) \
         VALUES('research-site','campaign-other-lab@example.invalid','other-lab','researcher');"
    ).await.unwrap();
    let other_lab_holder = api
        .resolve_principal(&person(
            "campaign-other-lab",
            "campaign-other-lab@example.invalid",
        ))
        .await
        .unwrap();
    owner.execute(
        "INSERT INTO enrichedlab_agent.broker_capability_grants(tenant_id,email,group_key,capability) \
         VALUES('research-site','campaign-other-lab@example.invalid','other-lab','research.discovery.autonomous')",
        &[],
    ).await.unwrap();
    let other_lab_campaign = Uuid::new_v4();
    api.create_campaign(
        other_lab_holder,
        other_lab_campaign,
        "other-lab",
        "Cross lab qualification",
        "",
        &scope(&["general-research"]),
        &lifecycle_budgets,
        &rules,
    )
    .await
    .unwrap();
    assert_eq!(
        api.campaign(holder, other_lab_campaign)
            .await
            .unwrap_err()
            .code,
        "campaign_not_found"
    );
    assert_eq!(
        api.campaign_planner_calls(holder, other_lab_campaign, 0, 100)
            .await
            .unwrap_err()
            .code,
        "campaign_not_found"
    );
    // The lab, principal, role, and new membership key are valid; only the campaign membership
    // invariant prevents the principal from joining another lab.
    assert_message_refusal(
        owner
            .execute(
                "INSERT INTO enrichedlab_agent.broker_group_memberships(tenant_id,group_key,principal_id,role) \
                 SELECT tenant_id,'other-lab',campaign_principal,'researcher' \
                 FROM enrichedlab_agent.broker_campaigns \
                 WHERE tenant_id='research-site' AND campaign_id=$1",
                &[&campaign],
            )
            .await,
        "42501",
        "broker:campaign_scope_forbidden",
    );
    assert_eq!(
        api.campaign_transition("stop", other_lab_holder, other_lab_campaign, None)
            .await
            .unwrap()["state"],
        "stopped"
    );

    let stop_all_running = Uuid::new_v4();
    let stop_all_paused = Uuid::new_v4();
    for campaign_id in [stop_all_running, stop_all_paused] {
        api.create_campaign(
            holder,
            campaign_id,
            "pilot-lab",
            "Stop all qualification",
            "",
            &scope(&["general-research"]),
            &lifecycle_budgets,
            &rules,
        )
        .await
        .unwrap();
        api.campaign_transition("start", holder, campaign_id, None)
            .await
            .unwrap();
    }
    api.campaign_transition("pause", holder, stop_all_paused, None)
        .await
        .unwrap();
    let stopped: i32 = owner
        .query_one(
            "SELECT enrichedlab_agent.broker_stop_all_campaigns('research-site','operator_stop')",
            &[],
        )
        .await
        .unwrap()
        .get(0);
    assert_eq!(stopped, 2);
    let stop_all_state = owner.query_one(
        "SELECT count(*) FILTER (WHERE state='stopped'), \
                (SELECT count(*) FROM enrichedlab_agent.broker_campaign_events e \
                 WHERE e.tenant_id='research-site' AND e.campaign_id IN ($1,$2) AND e.kind='stop_all') \
         FROM enrichedlab_agent.broker_campaigns c \
         WHERE c.tenant_id='research-site' AND c.campaign_id IN ($1,$2)",
        &[&stop_all_running, &stop_all_paused],
    ).await.unwrap();
    assert_eq!(stop_all_state.get::<_, i64>(0), 2);
    assert_eq!(stop_all_state.get::<_, i64>(1), 2);
    let stop_all_privilege = owner
        .query_one(
            "SELECT has_function_privilege('broker_api_qual', \
          'enrichedlab_agent.broker_stop_all_campaigns(text,text)','EXECUTE'), \
          has_function_privilege('broker_director_qual', \
          'enrichedlab_agent.broker_stop_all_campaigns(text,text)','EXECUTE')",
            &[],
        )
        .await
        .unwrap();
    assert!(!stop_all_privilege.get::<_, bool>(0));
    assert!(!stop_all_privilege.get::<_, bool>(1));

    let run_budget_campaign = Uuid::new_v4();
    let run_budget_budgets =
        json!({"wall_seconds":3600,"max_runs":1,"max_concurrent_runs":1,"max_turns_per_run":8});
    api.create_campaign(
        holder,
        run_budget_campaign,
        "pilot-lab",
        "Run budget qualification",
        "",
        &scope(&["general-research"]),
        &run_budget_budgets,
        &rules,
    )
    .await
    .unwrap();
    api.campaign_transition("start", holder, run_budget_campaign, None)
        .await
        .unwrap();
    let run_budget_claim = director
        .claim_campaign("campaign-run-budget", 90)
        .await
        .unwrap()
        .unwrap();
    let run_budget_branch = Uuid::new_v4();
    director
        .apply_campaign_change(
            run_budget_campaign,
            "campaign-run-budget",
            run_budget_claim.generation,
            &json!({"branches":[{"branch_id":run_budget_branch,"parent_id":null,
            "direction":"Run budget","rationale":"Refuse work after the total run budget"}]}),
        )
        .await
        .unwrap();
    // Both counters are at their limits; the permanent total-run budget must win.
    owner
        .execute(
            "UPDATE enrichedlab_agent.broker_campaigns SET run_count=1,active_runs=1 \
         WHERE tenant_id='research-site' AND campaign_id=$1",
            &[&run_budget_campaign],
        )
        .await
        .unwrap();
    let refused_project = Uuid::new_v4();
    let run_budget_refusal = director
        .start_campaign_step(
            run_budget_campaign,
            "campaign-run-budget",
            run_budget_claim.generation,
            Uuid::new_v4(),
            run_budget_branch,
            "explore",
            "This work exceeds the total run budget",
            &["general-research".into()],
            refused_project,
            Uuid::new_v4(),
        )
        .await
        .unwrap();
    assert_eq!(run_budget_refusal["started"], false);
    assert_eq!(run_budget_refusal["code"], "campaign_budget_exhausted");
    assert_eq!(run_budget_refusal["state"], "stopping");
    let refused_project_count: i64 = owner
        .query_one(
            "SELECT count(*) FROM enrichedlab_agent.broker_projects \
         WHERE tenant_id='research-site' AND project_id=$1",
            &[&refused_project],
        )
        .await
        .unwrap()
        .get(0);
    assert_eq!(refused_project_count, 0);

    let wall_campaign = Uuid::new_v4();
    let wall_budgets =
        json!({"wall_seconds":60,"max_runs":2,"max_concurrent_runs":1,"max_turns_per_run":2});
    api.create_campaign(
        holder,
        wall_campaign,
        "pilot-lab",
        "Wall budget qualification",
        "",
        &scope(&["general-research"]),
        &wall_budgets,
        &rules,
    )
    .await
    .unwrap();
    api.campaign_transition("start", holder, wall_campaign, None)
        .await
        .unwrap();
    let wall_claim = director
        .claim_campaign("campaign-wall", 90)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(wall_claim.campaign_id, wall_campaign);
    let wall_branch = Uuid::new_v4();
    director
        .apply_campaign_change(
            wall_campaign,
            "campaign-wall",
            wall_claim.generation,
            &json!({"branches":[{"branch_id":wall_branch,"parent_id":null,
            "direction":"Wall budget","rationale":"Fence work after elapsed wall time"}]}),
        )
        .await
        .unwrap();
    let wall_project = Uuid::new_v4();
    director
        .start_campaign_step(
            wall_campaign,
            "campaign-wall",
            wall_claim.generation,
            Uuid::new_v4(),
            wall_branch,
            "explore",
            "Run while the wall budget remains",
            &["general-research".into()],
            wall_project,
            Uuid::new_v4(),
        )
        .await
        .unwrap();
    owner
        .execute(
            "UPDATE enrichedlab_agent.broker_campaigns \
         SET started_at=clock_timestamp()-interval '61 seconds' \
         WHERE tenant_id='research-site' AND campaign_id=$1",
            &[&wall_campaign],
        )
        .await
        .unwrap();
    let wall_expired = director
        .apply_campaign_change(
            wall_campaign,
            "campaign-wall",
            wall_claim.generation,
            &json!({}),
        )
        .await
        .unwrap();
    assert_eq!(wall_expired["state"], "stopping");
    assert!(wall_expired["generation"].as_i64().unwrap() > wall_claim.generation);
    assert_eq!(
        director
            .start_campaign_step(
                wall_campaign,
                "campaign-wall",
                wall_claim.generation,
                Uuid::new_v4(),
                wall_branch,
                "explore",
                "Stale work after the wall budget",
                &["general-research".into()],
                Uuid::new_v4(),
                Uuid::new_v4(),
            )
            .await
            .unwrap_err()
            .code,
        "campaign_lease_fence_lost"
    );
    let wall_event_count: i64 = owner
        .query_one(
            "SELECT count(*) FROM enrichedlab_agent.broker_campaign_events \
         WHERE tenant_id='research-site' AND campaign_id=$1 AND kind='budget_exhausted' \
           AND detail->>'reason_code'='wall_seconds'",
            &[&wall_campaign],
        )
        .await
        .unwrap()
        .get(0);
    assert_eq!(wall_event_count, 1);

    let wall_upgrade = owner
        .query_one(
            "SELECT s.step_id,s.run_id,c.campaign_principal,checkpoint.state \
             FROM enrichedlab_agent.broker_campaign_steps s \
             JOIN enrichedlab_agent.broker_campaigns c \
               ON c.tenant_id=s.tenant_id AND c.campaign_id=s.campaign_id \
             JOIN enrichedlab_agent.broker_project_checkpoints checkpoint \
               ON checkpoint.tenant_id=s.tenant_id AND checkpoint.project_id=s.project_id \
              AND checkpoint.revision=1 \
             WHERE s.tenant_id='research-site' AND s.project_id=$1",
            &[&wall_project],
        )
        .await
        .unwrap();
    let wall_step: Uuid = wall_upgrade.get(0);
    let wall_run: Uuid = wall_upgrade.get(1);
    let wall_principal: Uuid = wall_upgrade.get(2);
    let mut untrusted_state: Value = wall_upgrade.get(3);
    untrusted_state["goal"] = json!("Untrusted replacement from the retired draft");
    owner
        .execute(
            "UPDATE enrichedlab_agent.broker_project_grants \
             SET capabilities=ARRAY['read','review','steer','cancel']::text[] \
             WHERE tenant_id='research-site' AND project_id=$1 AND principal_id=$2",
            &[&wall_project, &holder],
        )
        .await
        .unwrap();
    owner
        .batch_execute(
            "DROP TRIGGER campaign_checkpoint_guard \
               ON enrichedlab_agent.broker_project_checkpoints;",
        )
        .await
        .unwrap();
    let untrusted_revision: i64 = owner
        .query_one(
            "SELECT enrichedlab_agent.broker_write_checkpoint( \
               'research-site',$1,$2,1,$3,$4,'person',NULL,NULL,$1)",
            &[&holder, &wall_project, &Uuid::new_v4(), &untrusted_state],
        )
        .await
        .unwrap()
        .get(0);
    assert_eq!(untrusted_revision, 2);
    let reactivated_campaign = owner
        .execute(
            "UPDATE enrichedlab_agent.broker_campaigns \
             SET state='running',state_reason=NULL,started_at=clock_timestamp(), \
                 lease_owner=NULL,lease_until=NULL \
             WHERE tenant_id='research-site' AND campaign_id=$1",
            &[&wall_campaign],
        )
        .await
        .unwrap();
    assert_eq!(reactivated_campaign, 1);
    owner
        .batch_execute(
            "DROP TRIGGER campaign_pin_checkpoint \
               ON enrichedlab_agent.broker_project_checkpoints; \
             ALTER TABLE enrichedlab_agent.broker_campaign_steps \
               DROP CONSTRAINT broker_campaign_steps_state_shape; \
             ALTER TABLE enrichedlab_agent.broker_campaign_steps \
               DROP CONSTRAINT broker_campaign_steps_checkpoint_revision_fkey; \
             ALTER TABLE enrichedlab_agent.broker_campaign_steps \
               DROP COLUMN checkpoint_revision; \
             ALTER TABLE enrichedlab_agent.broker_campaign_steps \
               ADD CONSTRAINT broker_campaign_steps_old_state_shape CHECK ( \
                 (state='planned' AND project_id IS NULL AND run_id IS NULL AND started_at IS NULL \
                   AND outcome_class IS NULL AND outcome IS NULL AND finished_at IS NULL) \
                 OR (state='running' AND project_id IS NOT NULL AND run_id IS NOT NULL \
                   AND started_at IS NOT NULL AND outcome_class IS NULL AND outcome IS NULL \
                   AND finished_at IS NULL) \
                 OR (state='finished' AND project_id IS NOT NULL AND run_id IS NOT NULL \
                   AND started_at IS NOT NULL AND outcome_class IS NOT NULL AND outcome IS NOT NULL \
                   AND finished_at IS NOT NULL));",
        )
        .await
        .unwrap();
    owner
        .batch_execute(include_str!("../migrations/postgres/066_campaigns.sql"))
        .await
        .unwrap();
    owner
        .batch_execute(include_str!(
            "../migrations/postgres/067_campaign_planner.sql"
        ))
        .await
        .unwrap();
    director.ready_for_campaign_director().await.unwrap();
    let unsafe_upgrade = owner
        .query_one(
            "SELECT s.checkpoint_revision,p.revision,r.status,r.code,c.state,c.state_reason,g.capabilities, \
               c.campaign_principal=$5 \
             FROM enrichedlab_agent.broker_campaign_steps s \
             JOIN enrichedlab_agent.broker_projects p \
               ON p.tenant_id=s.tenant_id AND p.project_id=s.project_id \
             JOIN enrichedlab_agent.broker_research_runs r \
               ON r.tenant_id=s.tenant_id AND r.run_id=s.run_id \
             JOIN enrichedlab_agent.broker_campaigns c \
               ON c.tenant_id=s.tenant_id AND c.campaign_id=s.campaign_id \
             JOIN enrichedlab_agent.broker_project_grants g \
               ON g.tenant_id=s.tenant_id AND g.project_id=s.project_id AND g.principal_id=$4 \
             WHERE s.tenant_id='research-site' AND s.campaign_id=$1 \
               AND s.step_id=$2 AND s.run_id=$3",
            &[
                &wall_campaign,
                &wall_step,
                &wall_run,
                &holder,
                &wall_principal,
            ],
        )
        .await
        .unwrap();
    assert_eq!(unsafe_upgrade.get::<_, i64>(0), 1);
    assert_eq!(unsafe_upgrade.get::<_, i64>(1), 2);
    assert_eq!(unsafe_upgrade.get::<_, String>(2), "paused");
    assert_eq!(
        unsafe_upgrade.get::<_, String>(3),
        "campaign_checkpoint_unpinned"
    );
    assert_eq!(unsafe_upgrade.get::<_, String>(4), "stopping");
    assert_eq!(unsafe_upgrade.get::<_, String>(5), "checkpoint_unpinned");
    assert_eq!(unsafe_upgrade.get::<_, Vec<String>>(6), vec!["read"]);
    assert!(unsafe_upgrade.get::<_, bool>(7));

    // A real ModelPlanner call crosses the shared HTTP model client, retries one
    // strict-schema refusal, records both calls, applies the accepted proposal,
    // and starts its bounded step through the ordinary director cycle.
    let sent_bodies: Arc<Mutex<Vec<Value>>> = Arc::default();
    let captured = sent_bodies.clone();
    let model_app = axum::Router::new().route(
        "/v1/chat/completions",
        axum::routing::post(move |axum::Json(body): axum::Json<Value>| {
            let captured = captured.clone();
            async move {
                let mut bodies = captured.lock().unwrap();
                bodies.push(body);
                let wants_empty = bodies
                    .last()
                    .and_then(|request| request["messages"][1]["content"].as_str())
                    .is_some_and(|context| context.contains("Empty planner qualification"));
                let content = if wants_empty {
                    json!({"branches_open":[],"steps":[],"branches_close":[],"records":[],
                        "summary":"No bounded work proposed"})
                    .to_string()
                } else if bodies.len() == 2 {
                    json!({
                        "branches_open":[{"parent":null,"direction":"Independent control",
                            "rationale":"Test the expected direction with a control"}],
                        "steps":[{"branch":0,"purpose":"test",
                            "question":"Does the control support the expected direction?"}],
                        "branches_close":[],
                        "records":[{"kind":"hypothesis","text":"The control may support the direction",
                            "refs":[],"relations":[]}],
                        "summary":"Start with a controlled comparison"
                    })
                    .to_string()
                } else {
                    json!({"branches_open":[],"steps":[],"branches_close":[],"records":[],
                        "summary":"Retry the malformed reply","unexpected":true}).to_string()
                };
                axum::Json(json!({"model":"qualification-model","choices":[{"message":{
                    "content":content},"finish_reason":"stop"}],"usage":{
                    "prompt_tokens":10,"completion_tokens":5}}))
            }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let model_port = listener.local_addr().unwrap().port();
    let model_server = tokio::spawn(async move { axum::serve(listener, model_app).await.unwrap() });
    let private_dir = tempfile::tempdir().unwrap();
    let model_config = private_dir.path().join("model.json");
    fs::write(
        &model_config,
        json!({"schema_version":1,
            "endpoint":format!("http://127.0.0.1:{model_port}/v1/chat/completions"),
            "model":"qualification-model","allow_internal_http":true,
            "max_input_bytes":65536,"max_output_tokens":1024,
            "context_window_tokens":131072,"timeout_seconds":30})
        .to_string(),
    )
    .unwrap();
    fs::set_permissions(&model_config, fs::Permissions::from_mode(0o600)).unwrap();

    let model_campaign = Uuid::new_v4();
    api.create_campaign(
        holder,
        model_campaign,
        "pilot-lab",
        "Model planner qualification",
        "Use the supported method",
        &scope(&["general-research"]),
        &budgets,
        &rules,
    )
    .await
    .unwrap();
    api.campaign_transition("start", holder, model_campaign, None)
        .await
        .unwrap();
    owner
        .execute(
            "UPDATE enrichedlab_agent.broker_campaigns \
             SET available_at=CASE WHEN campaign_id=$1 THEN '2000-01-01'::timestamptz \
               ELSE clock_timestamp()+interval '1 hour' END \
             WHERE tenant_id='research-site' AND state IN ('running','pausing','stopping') \
               AND (lease_until IS NULL OR lease_until <= clock_timestamp())",
            &[&model_campaign],
        )
        .await
        .unwrap();
    let model_claim = director
        .claim_campaign("campaign-model", 90)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(model_claim.campaign_id, model_campaign);
    let mut model_planner = ModelPlanner::load(
        &model_config,
        director.clone(),
        model_campaign,
        "campaign-model".into(),
        model_claim.generation,
        65_536,
    )
    .unwrap();
    advance_claim(&director, "campaign-model", model_claim, &mut model_planner)
        .await
        .unwrap();
    let model_cycle = owner
        .query_one(
            "SELECT (SELECT array_agg(outcome_code ORDER BY sequence) \
                       FROM enrichedlab_agent.broker_campaign_planner_calls \
                       WHERE tenant_id='research-site' AND campaign_id=$1), \
                    (SELECT count(*) FROM enrichedlab_agent.broker_campaign_steps \
                       WHERE tenant_id='research-site' AND campaign_id=$1), \
                    (SELECT count(*) FROM enrichedlab_agent.broker_campaign_records \
                       WHERE tenant_id='research-site' AND campaign_id=$1 AND origin='planner')",
            &[&model_campaign],
        )
        .await
        .unwrap();
    assert_eq!(
        model_cycle.get::<_, Vec<String>>(0),
        vec!["planner_reply_invalid", "accepted"]
    );
    assert_eq!(model_cycle.get::<_, i64>(1), 1);
    assert_eq!(model_cycle.get::<_, i64>(2), 1);

    let refused_campaign = Uuid::new_v4();
    api.create_campaign(
        holder,
        refused_campaign,
        "pilot-lab",
        "Planner refusal qualification",
        "Use the supported method",
        &scope(&["general-research"]),
        &budgets,
        &rules,
    )
    .await
    .unwrap();
    api.campaign_transition("start", holder, refused_campaign, None)
        .await
        .unwrap();
    owner
        .execute(
            "UPDATE enrichedlab_agent.broker_campaigns \
             SET available_at=CASE WHEN campaign_id=$1 THEN '2000-01-01'::timestamptz \
               ELSE clock_timestamp()+interval '1 hour' END \
             WHERE tenant_id='research-site' AND state IN ('running','pausing','stopping') \
               AND (lease_until IS NULL OR lease_until <= clock_timestamp())",
            &[&refused_campaign],
        )
        .await
        .unwrap();
    let refused_claim = director
        .claim_campaign("campaign-model-refused", 90)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(refused_claim.campaign_id, refused_campaign);
    let mut refused_planner = ModelPlanner::load(
        &model_config,
        director.clone(),
        refused_campaign,
        "campaign-model-refused".into(),
        refused_claim.generation,
        65_536,
    )
    .unwrap();
    advance_claim(
        &director,
        "campaign-model-refused",
        refused_claim,
        &mut refused_planner,
    )
    .await
    .unwrap();
    let refused_cycle = owner
        .query_one(
            "SELECT (SELECT array_agg(outcome_code ORDER BY sequence) \
                       FROM enrichedlab_agent.broker_campaign_planner_calls \
                       WHERE tenant_id='research-site' AND campaign_id=$1), \
                    planner_refused_cycles, \
                    (SELECT count(*) FROM enrichedlab_agent.broker_campaign_events \
                       WHERE tenant_id='research-site' AND campaign_id=$1 \
                         AND kind='proposal_refused' \
                         AND detail->>'reason_code'='planner_reply_invalid') \
             FROM enrichedlab_agent.broker_campaigns \
             WHERE tenant_id='research-site' AND campaign_id=$1",
            &[&refused_campaign],
        )
        .await
        .unwrap();
    assert_eq!(
        refused_cycle.get::<_, Vec<String>>(0),
        vec!["planner_reply_invalid", "planner_reply_invalid"]
    );
    assert_eq!(refused_cycle.get::<_, i32>(1), 1);
    assert_eq!(refused_cycle.get::<_, i64>(2), 1);

    let empty_campaign = Uuid::new_v4();
    let empty_rules = json!({"change_approach_repeats":2,"branch_window":4,
        "campaign_window":12,"planner_refused_cycles":1});
    api.create_campaign(
        holder,
        empty_campaign,
        "pilot-lab",
        "Empty planner qualification",
        "Use the supported method",
        &scope(&["general-research"]),
        &budgets,
        &empty_rules,
    )
    .await
    .unwrap();
    api.campaign_transition("start", holder, empty_campaign, None)
        .await
        .unwrap();
    owner
        .execute(
            "UPDATE enrichedlab_agent.broker_campaigns \
             SET available_at=CASE WHEN campaign_id=$1 THEN '2000-01-01'::timestamptz \
               ELSE clock_timestamp()+interval '1 hour' END \
             WHERE tenant_id='research-site' AND state IN ('running','pausing','stopping') \
               AND (lease_until IS NULL OR lease_until <= clock_timestamp())",
            &[&empty_campaign],
        )
        .await
        .unwrap();
    let empty_claim = director
        .claim_campaign("campaign-model-empty", 90)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(empty_claim.campaign_id, empty_campaign);
    let mut empty_planner = ModelPlanner::load(
        &model_config,
        director.clone(),
        empty_campaign,
        "campaign-model-empty".into(),
        empty_claim.generation,
        65_536,
    )
    .unwrap();
    advance_claim(
        &director,
        "campaign-model-empty",
        empty_claim,
        &mut empty_planner,
    )
    .await
    .unwrap();
    let empty_cycle = owner
        .query_one(
            "SELECT state,state_reason,planner_refused_cycles, \
                    (SELECT array_agg(outcome_code ORDER BY sequence) \
                     FROM enrichedlab_agent.broker_campaign_planner_calls \
                     WHERE tenant_id='research-site' AND campaign_id=$1), \
                    (SELECT count(*) FROM enrichedlab_agent.broker_campaign_planner_call_starts \
                     WHERE tenant_id='research-site' AND campaign_id=$1) \
             FROM enrichedlab_agent.broker_campaigns \
             WHERE tenant_id='research-site' AND campaign_id=$1",
            &[&empty_campaign],
        )
        .await
        .unwrap();
    // Nothing is in flight, so the same cycle settles the pause: pausing becomes paused and keeps its reason.
    assert_eq!(empty_cycle.get::<_, String>(0), "paused");
    assert_eq!(empty_cycle.get::<_, String>(1), "planner_refused");
    assert_eq!(empty_cycle.get::<_, i32>(2), 1);
    assert_eq!(
        empty_cycle.get::<_, Vec<String>>(3),
        vec!["planner_no_progress"]
    );
    assert_eq!(empty_cycle.get::<_, i64>(4), 1);
    let bodies = sent_bodies.lock().unwrap();
    assert_eq!(bodies.len(), 5);
    let compiled_context: Value =
        serde_json::from_str(bodies[0]["messages"][1]["content"].as_str().unwrap()).unwrap();
    assert_eq!(
        compiled_context["sections"][0]["section"],
        "goal_and_constraints"
    );
    assert_eq!(compiled_context["sections"][1]["section"], "scope");
    assert_eq!(
        compiled_context["sections"][1]["methods"][0]["key"],
        "general-research"
    );
    assert!(
        compiled_context["sections"][1]["methods"][0]["description"]
            .as_str()
            .is_some_and(|description| !description.is_empty())
    );
    assert_eq!(
        compiled_context["sections"][1]["methods"][0]["data_sources"][0],
        "qualification-reference"
    );
    for index in [0, 2] {
        assert!(
            !bodies[index]["messages"][1]["content"]
                .as_str()
                .unwrap()
                .contains("reply_retry")
        );
        assert!(
            bodies[index + 1]["messages"][1]["content"]
                .as_str()
                .unwrap()
                .contains("previous_reply_unreadable")
        );
    }
    drop(bodies);
    model_server.abort();
    owner_connection.abort();
}
