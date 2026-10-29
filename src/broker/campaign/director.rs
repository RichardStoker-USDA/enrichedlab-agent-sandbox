use std::{path::PathBuf, time::Duration};

use anyhow::{Context, Result, bail};
use clap::Args;
use serde_json::{Value, json};
use tokio::{
    task::JoinSet,
    time::{MissedTickBehavior, interval, sleep},
};
use uuid::Uuid;

use crate::broker::{
    config::{migration_dsn, validate_tenant},
    project::action_key,
    store::{CampaignClaim, Store},
};

use super::{
    CampaignPlanner, CampaignView, FinishedStep, ModelPlanner, OutcomeClass, Proposal,
    classify_outcome, evaluate_rules, load_shared_model, validate_planner_input_bytes,
    validate_proposal,
};

#[derive(Clone, Debug, Args)]
pub struct CampaignDirectorArgs {
    #[arg(long)]
    pub database_dsn_file: PathBuf,
    /// Private operator-owned model route and budget configuration.
    #[arg(long)]
    pub model_config_file: PathBuf,
    #[arg(long)]
    pub tenant: String,
    #[arg(long)]
    pub worker_id: Option<String>,
    /// Claim and advance at most one campaign, then exit.
    #[arg(long)]
    pub once: bool,
    #[arg(long, default_value_t = 4)]
    pub max_parallel: usize,
    #[arg(long, default_value_t = 2_000)]
    pub poll_millis: u64,
    #[arg(long, default_value_t = 90)]
    pub lease_seconds: i32,
    /// Maximum UTF-8 bytes for the planner's fixed system text and compiled context.
    #[arg(long, default_value_t = 65_536)]
    pub planner_max_input_bytes: usize,
}

pub async fn run(args: CampaignDirectorArgs) -> Result<()> {
    validate_tenant(&args.tenant)?;
    if !(1..=16).contains(&args.max_parallel)
        || !(100..=60_000).contains(&args.poll_millis)
        || !(30..=600).contains(&args.lease_seconds)
        || validate_planner_input_bytes(args.planner_max_input_bytes).is_err()
    {
        bail!("campaign_director_config_invalid");
    }
    let worker = args
        .worker_id
        .unwrap_or_else(|| format!("campaign-{}", Uuid::new_v4()));
    if !action_key(&worker) {
        bail!("campaign_director_config_invalid");
    }
    let dsn = migration_dsn(
        &Some(args.database_dsn_file),
        "AGENT_SANDBOX_CAMPAIGN_DIRECTOR_DSN",
    )?;
    let (model, configured_model) = load_shared_model(&args.model_config_file)?;
    let store = Store::connect(&dsn, args.tenant, args.max_parallel + 2)?;
    store.ready_for_campaign_director().await?;
    let mut active = JoinSet::new();
    loop {
        while active.len() < args.max_parallel {
            let Some(claim) = store.claim_campaign(&worker, args.lease_seconds).await? else {
                break;
            };
            let mut planner = ModelPlanner::new(
                model.clone(),
                configured_model.clone(),
                store.clone(),
                claim.campaign_id,
                worker.clone(),
                claim.generation,
                args.planner_max_input_bytes,
            );
            if args.once {
                advance_with_renewal(&store, &worker, claim, &mut planner, args.lease_seconds)
                    .await?;
                return Ok(());
            }
            let claimed_store = store.clone();
            let claimed_worker = worker.clone();
            active.spawn(async move {
                advance_with_renewal(
                    &claimed_store,
                    &claimed_worker,
                    claim,
                    &mut planner,
                    args.lease_seconds,
                )
                .await
            });
        }
        if args.once {
            return Ok(());
        }
        tokio::select! {
            _ = tokio::signal::ctrl_c() => {
                active.abort_all();
                while active.join_next().await.is_some() {}
                return Ok(());
            }
            _ = sleep(Duration::from_millis(args.poll_millis)) => {}
            Some(result) = active.join_next(), if !active.is_empty() => {
                match result {
                    Ok(Ok(())) => {}
                    Ok(Err(error)) => tracing::error!(code = error.to_string(), "campaign cycle failed; lease will recover"),
                    Err(error) => crate::broker::log_join_failure(&error, "campaign-director", None),
                }
            }
        }
    }
}

async fn advance_with_renewal(
    store: &Store,
    worker: &str,
    claim: CampaignClaim,
    planner: &mut dyn CampaignPlanner,
    lease_seconds: i32,
) -> Result<()> {
    let campaign = claim.campaign_id;
    let generation = claim.generation;
    let mut feature_telemetry = claim.view["campaign_principal"]
        .as_str()
        .and_then(|value| value.parse::<Uuid>().ok())
        .zip(claim.view["group_key"].as_str())
        .map(|(principal, lab)| {
            crate::broker::feature_events::AttemptGuard::new(
                crate::broker::feature_events::Feature::Campaign,
                claim.view["planner_refused_cycles"]
                    .as_u64()
                    .unwrap_or(0)
                    .min(u64::from(u32::MAX)) as u32,
                principal,
                crate::broker::feature_events::PrincipalKind::Agent,
                lab,
                crate::broker::feature_events::TraceContext::new(),
                Some(campaign),
            )
        });
    let advance = advance_claim(store, worker, claim, planner);
    tokio::pin!(advance);
    let mut renewal = interval(Duration::from_secs((lease_seconds as u64 / 3).max(1)));
    renewal.set_missed_tick_behavior(MissedTickBehavior::Delay);
    renewal.tick().await;
    let mut lease_fenced = false;
    loop {
        tokio::select! {
            result = &mut advance => {
                if let Some(telemetry) = feature_telemetry.take() {
                    telemetry.finish(
                        if result.is_ok() {
                            crate::broker::feature_events::Outcome::Success
                        } else {
                            crate::broker::feature_events::Outcome::Failure
                        },
                        result.as_ref().err().map(|_| crate::broker::feature_events::ErrorCode::CampaignFailed),
                        u64::from(result.is_ok()),
                        u64::from(result.is_err()),
                    );
                }
                return result
            },
            _ = renewal.tick(), if !lease_fenced => {
                if !store.renew_campaign(campaign, worker, generation, lease_seconds).await? {
                    // Pause, stop, or a new claim fences every campaign mutation.
                    // Keep only the already-started model call alive so it can append
                    // its terminal custody row; its later apply will still be refused.
                    lease_fenced = true;
                }
            }
        }
    }
}

pub async fn advance_claim(
    store: &Store,
    worker: &str,
    claim: CampaignClaim,
    planner: &mut dyn CampaignPlanner,
) -> Result<()> {
    let campaign = claim.campaign_id;
    let generation = claim.generation;
    let mut view: CampaignView = serde_json::from_value(
        store
            .campaign_director_view(campaign, worker, generation)
            .await?,
    )
    .context("campaign_view_invalid")?;
    let finished: Vec<FinishedStep> = store
        .campaign_finished_steps(campaign, worker, generation)
        .await?
        .into_iter()
        .map(serde_json::from_value)
        .collect::<std::result::Result<_, _>>()
        .context("campaign_outcome_invalid")?;
    if !finished.is_empty() {
        let change = reconciliation_change(&view, &finished);
        let updated = store
            .apply_campaign_change(campaign, worker, generation, &change)
            .await?;
        view = if updated.get("generation").and_then(Value::as_i64) == Some(generation)
            && updated.get("state").and_then(Value::as_str) == Some("running")
        {
            serde_json::from_value(
                store
                    .campaign_director_view(campaign, worker, generation)
                    .await?,
            )
        } else {
            serde_json::from_value(updated)
        }
        .context("campaign_view_invalid")?;
    }
    if view.state != "running" {
        // Pause and stop drain work already running; reconciliation above lets the
        // database settle the terminal state when the last run finishes.
        if view.generation == generation {
            store.yield_campaign(campaign, worker, generation).await?;
        }
        return Ok(());
    }

    let proposal = planner.propose(&view).await;
    let proposal = match proposal.and_then(|proposal| {
        validate_proposal(&view, &proposal).map_err(super::PlannerError)?;
        Ok(proposal)
    }) {
        Ok(proposal) => proposal,
        Err(error) => {
            let change = proposal_refusal_change(&view, error.0);
            let updated = store
                .apply_campaign_change(campaign, worker, generation, &change)
                .await?;
            if updated.get("generation").and_then(Value::as_i64) == Some(generation)
                && matches!(
                    updated.get("state").and_then(Value::as_str),
                    Some("running" | "pausing" | "stopping")
                )
            {
                store.yield_campaign(campaign, worker, generation).await?;
            }
            return Ok(());
        }
    };
    let change = proposal_change(&proposal);
    let updated = store
        .apply_campaign_change(campaign, worker, generation, &change)
        .await?;
    view = serde_json::from_value(updated).context("campaign_view_invalid")?;
    if proposal.steps.is_empty()
        && view.active_runs == 0
        && !view.branches.is_empty()
        && view
            .branches
            .iter()
            .all(|branch| matches!(branch.state.as_str(), "abandoned" | "concluded"))
    {
        store
            .apply_campaign_change(
                campaign,
                worker,
                generation,
                &json!({"campaign_state":{"state":"finished","reason":"campaign_complete"}}),
            )
            .await?;
        return Ok(());
    }
    if view.state == "running" {
        for step in proposal
            .steps
            .into_iter()
            .take(view.free_concurrency().min(view.runs_left()))
        {
            let started = store
                .start_campaign_step(
                    campaign,
                    worker,
                    generation,
                    Uuid::new_v4(),
                    step.branch_id,
                    step.purpose.as_str(),
                    &step.question,
                    &step.profiles,
                    Uuid::new_v4(),
                    Uuid::new_v4(),
                )
                .await?;
            if started.get("started").and_then(Value::as_bool) != Some(true) {
                if started.get("state").and_then(Value::as_str) == Some("running") {
                    store.yield_campaign(campaign, worker, generation).await?;
                }
                return Ok(());
            }
        }
        store.yield_campaign(campaign, worker, generation).await?;
    }
    Ok(())
}

pub fn reconciliation_change(view: &CampaignView, finished: &[FinishedStep]) -> Value {
    let decision = evaluate_rules(view, finished);
    let mut records = Vec::new();
    let mut finished_changes = Vec::new();
    for step in finished {
        let class = classify_outcome(&step.outcome);
        let attempt_text = format!(
            "Campaign step {} completed run {} with outcome {}; findings {}; refused replies {}; turns {}",
            step.step_id,
            step.run_id,
            class.as_str(),
            step.outcome.findings,
            step.outcome.refused_replies,
            step.outcome.turns
        );
        records.push(json!({
            "record_id":Uuid::new_v4(), "kind":"attempt",
            "text":attempt_text, "origin":"director", "status":"open",
            "refs":[format!("step:{}",step.step_id),format!("run:{}",step.run_id),format!("project:{}",step.project_id)]
        }));
        if class == OutcomeClass::Evidence {
            for finding in 0..step.outcome.findings {
                records.push(json!({
                    "record_id":Uuid::new_v4(), "kind":"finding",
                    "text":format!("Finding reference {} from the completed campaign step", finding + 1),
                    "origin":"director", "status":"open", "refs":[format!("run:{}",step.run_id)]
                }));
            }
        }
        if class == OutcomeClass::NeedsPerson {
            records.push(json!({
                "record_id":Uuid::new_v4(), "kind":"request",
                "text":"This step needs a person's review", "origin":"director", "status":"open",
                "refs":[format!("step:{}",step.step_id),format!("run:{}",step.run_id)]
            }));
        }
        finished_changes.push(json!({
            "step_id":step.step_id, "outcome_class":class.as_str(), "outcome":step.outcome
        }));
    }
    let mut branch_updates: Vec<Value> = decision.wait_branches.iter().map(|branch| json!({
        "branch_id":branch, "state":"waiting", "reason_class":"change_approach",
        "reason":"The same non-evidence outcome repeated; the next step must change question and profiles"
    })).collect();
    branch_updates.extend(decision.conclude_branches.iter().map(|branch| {
        json!({
            "branch_id":branch, "state":"concluded", "reason_class":"no_new_evidence",
            "reason":"The branch reached its configured window without evidence"
        })
    }));
    let events: Vec<Value> = decision
        .notices
        .iter()
        .map(|notice| {
            let mut detail =
                json!({"rule":notice.rule,"count":notice.count,"threshold":notice.threshold});
            if let Some(branch) = notice.branch_id {
                detail["branch_id"] = json!(branch);
            }
            json!({"kind":"rule_fired","detail":detail})
        })
        .collect();
    let mut change = json!({
        "finished_steps":finished_changes,
        "records":records,
        "branch_updates":branch_updates,
        "events":events
    });
    if let Some(reason) = decision.stop_reason {
        change["campaign_state"] = json!({"state":"stopping","reason":reason});
    }
    change
}

pub fn proposal_refusal_change(view: &CampaignView, reason: &str) -> Value {
    let next = view.planner_refused_cycles + 1;
    let mut change = json!({
        "planner_cycle": "refused",
        "events": [{"kind":"proposal_refused", "detail":{"reason_code":reason}}]
    });
    if next >= u32::from(view.stop_rules.planner_refused_cycles) {
        change["campaign_state"] = json!({"state":"pausing", "reason":"planner_refused"});
        change["events"].as_array_mut().expect("array").push(json!({
            "kind":"rule_fired",
            "detail":{"rule":"planner_refused", "count":next,
                      "threshold":view.stop_rules.planner_refused_cycles}
        }));
    }
    change
}

pub fn proposal_change(proposal: &Proposal) -> Value {
    json!({
        "branches":proposal.open_branches.iter().map(|branch| json!({
            "branch_id":branch.branch_id, "parent_id":branch.parent_branch_id,
            "direction":branch.direction, "rationale":branch.rationale
        })).collect::<Vec<_>>(),
        "branch_updates":proposal.close_branches.iter().map(|branch| json!({
            "branch_id":branch.branch_id, "state":"concluded", "reason_class":branch.reason_class,
            "reason":branch.reason
        })).collect::<Vec<_>>(),
        "records":proposal.records.iter().map(|record| json!({
            "record_id":record.record_id, "kind":record.kind, "text":record.text,
            "origin":"planner", "status":record.status, "refs":record.refs
        })).collect::<Vec<_>>(),
        "relations":proposal.relations.iter().map(|relation| json!({
            "relation_id":relation.relation_id, "kind":relation.kind,
            "source_record":relation.from_record, "target_record":relation.to_record
        })).collect::<Vec<_>>(),
        "planner_cycle":"accepted"
    })
}
