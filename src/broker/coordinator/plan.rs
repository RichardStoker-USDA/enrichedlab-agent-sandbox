//! The working plan: task references, archival into plan pages, and reconciliation with task facts.
use super::*;

/// The working plan holds at most this many steps (`ResearchState::validate`). When it could not take another
/// delegation, finished steps move out into archive pages until it holds `PLAN_STEPS_AFTER_ARCHIVE`.
pub const PLAN_STEPS_MAX: usize = 128;

pub(super) const PLAN_STEPS_AFTER_ARCHIVE: usize = 96;

/// The archived steps the lead sees (`plan_pages`): their counts, then the newest with their titles, within this many
/// bytes.
pub(super) const PLAN_PAGES_BYTES: usize = 2048;

/// The analysis an `artifact://tasks/<analysis>/<artifact>` reference belongs to.
pub fn ref_task(reference: &str) -> Option<&str> {
    reference
        .strip_prefix("artifact://tasks/")?
        .split('/')
        .next()
        .filter(|t| !t.is_empty())
}

/// The analyses an earlier answer reads: the steps its findings bind to and its answer tables come
/// from stay in the working plan, so neither is left without its result or the person's review of it.
pub fn bound_tasks(results: &Value) -> HashSet<String> {
    let earlier = &results["earlier_findings"];
    let bindings = earlier["findings"]
        .as_array()
        .into_iter()
        .flatten()
        .flat_map(|f| f["bindings"].as_object().into_iter().flatten())
        .filter_map(|(_, b)| b["ref"].as_str());
    let tables = earlier["tables"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|t| t["ref"].as_str());
    bindings
        .chain(tables)
        .filter_map(ref_task)
        .map(str::to_owned)
        .collect()
}

/// This project's analyses, archived and in the working plan: an archived analysis is still this
/// project's own work, so the lab's earlier research leaves it out. The order is not age; `newest_tasks` gives that.
pub fn project_tasks(state: &ResearchState, pages: &Value) -> Vec<Uuid> {
    let mut tasks: Vec<Uuid> = pages["tasks"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|t| t.as_str()?.parse().ok())
        .collect();
    tasks.extend(state.plan.iter().filter_map(|s| s.task_id));
    tasks
}

/// The project's newest analyses by submission, oldest first, from the claim's `newest_tasks` (migration 048): the plan
/// and its archive pages hold them in plan order, and archival keeps older steps an earlier finding reads, so neither
/// order is age. At most `OWN_QUERY_TASKS_MAX`, the newest kept.
pub fn newest_tasks(claim: &Value) -> Vec<Uuid> {
    let mut tasks: Vec<Uuid> = claim["newest_tasks"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|t| t.as_str()?.parse().ok())
        .collect();
    let skip = tasks.len().saturating_sub(OWN_QUERY_TASKS_MAX);
    tasks.drain(..skip);
    tasks
}

/// The steps to move out of a working plan that could not take another delegation: in plan order,
/// completed, failed or skipped steps whose analysis, if any, finished (completed, failed or cancelled) and is read by no
/// earlier finding, until the plan holds `PLAN_STEPS_AFTER_ARCHIVE` steps. Empty while the plan has room for `parallel`
/// more steps, or when no step can move.
pub fn plan_archive_keys(
    state: &ResearchState,
    facts: &Value,
    parallel: usize,
    bound: &HashSet<String>,
) -> Vec<String> {
    if state.plan.len() + parallel <= PLAN_STEPS_MAX {
        return Vec::new();
    }
    let finished: HashSet<&str> = facts
        .as_array()
        .into_iter()
        .flatten()
        .filter(|f| {
            matches!(
                f["status"].as_str(),
                Some("completed" | "failed" | "cancelled")
            )
        })
        .filter_map(|f| f["task_id"].as_str())
        .collect();
    state
        .plan
        .iter()
        .filter(|s| {
            matches!(
                s.status,
                StepStatus::Completed | StepStatus::Failed | StepStatus::Skipped
            )
        })
        .filter(|s| {
            s.task_id.is_none_or(|t| {
                let t = t.to_string();
                finished.contains(t.as_str()) && !bound.contains(&t)
            })
        })
        .take(state.plan.len().saturating_sub(PLAN_STEPS_AFTER_ARCHIVE))
        .map(|s| s.action_key.clone())
        .collect()
}

/// The claim's archive summary (migration 048) with a page this call just archived: the counts, the keys and the newest
/// sixteen steps, as the database would return them on the next claim.
pub fn with_archived_page(pages: &Value, moved: &[PlanStep], facts: &Value) -> Value {
    let methods: HashMap<&str, &str> = facts
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|f| Some((f["task_id"].as_str()?, f["profile"].as_str()?)))
        .collect();
    let mut out = pages.clone();
    if !out.is_object() {
        out = json!({});
    }
    out["pages"] = json!(pages["pages"].as_u64().unwrap_or(0) + 1);
    out["steps"] = json!(pages["steps"].as_u64().unwrap_or(0) + moved.len() as u64);
    let mut keys: Vec<Value> = pages["keys"].as_array().cloned().unwrap_or_default();
    let mut tasks: Vec<Value> = pages["tasks"].as_array().cloned().unwrap_or_default();
    let mut by_status: BTreeMap<String, u64> =
        serde_json::from_value(pages["by_status"].clone()).unwrap_or_default();
    let mut by_method: BTreeMap<String, BTreeMap<String, u64>> =
        serde_json::from_value(pages["by_method"].clone()).unwrap_or_default();
    let mut newest: Vec<Value> = Vec::new();
    for step in moved {
        let status = serde_json::to_value(&step.status)
            .ok()
            .and_then(|s| s.as_str().map(str::to_owned))
            .unwrap_or_default();
        keys.push(json!(step.action_key));
        if let Some(task) = step.task_id {
            tasks.push(json!(task));
        }
        *by_status.entry(status.clone()).or_default() += 1;
        if let Some(method) = step
            .task_id
            .and_then(|t| methods.get(t.to_string().as_str()))
        {
            *by_method
                .entry((*method).to_owned())
                .or_default()
                .entry(status.clone())
                .or_default() += 1;
        }
        newest.push(json!({"action_key": step.action_key, "title": step.title, "status": status}));
    }
    newest.reverse();
    newest.extend(pages["newest"].as_array().cloned().unwrap_or_default());
    newest.truncate(16);
    out["keys"] = Value::Array(keys);
    out["tasks"] = Value::Array(tasks);
    out["by_status"] = json!(by_status);
    out["by_method"] = json!(by_method);
    out["newest"] = Value::Array(newest);
    out
}

/// The archived steps as the lead sees them: the pages, how many steps they hold and by status, then
/// the newest with their titles while `PLAN_PAGES_BYTES` holds them; `steps_not_listed` counts the rest.
pub(super) fn plan_pages_view(pages: &Value) -> Option<Value> {
    let steps = pages["steps"].as_u64().filter(|n| *n > 0)?;
    let size = |v: &Value| serde_json::to_vec(v).map_or(usize::MAX, |b| b.len());
    let mut shown = json!({"pages": pages["pages"], "steps": steps, "by_status": pages["by_status"],
        "newest": [], "steps_not_listed": steps});
    let mut listed: Vec<Value> = Vec::new();
    for s in pages["newest"].as_array().into_iter().flatten() {
        listed.push(json!({"key": s["action_key"], "title": s["title"], "status": s["status"]}));
        shown["newest"] = Value::Array(listed.clone());
        shown["steps_not_listed"] = json!(steps.saturating_sub(listed.len() as u64));
        if size(&shown) > PLAN_PAGES_BYTES {
            listed.pop();
            break;
        }
    }
    shown["newest"] = Value::Array(listed.clone());
    shown["steps_not_listed"] = json!(steps.saturating_sub(listed.len() as u64));
    if size(&shown) > PLAN_PAGES_BYTES {
        shown = json!({"pages": pages["pages"], "steps": steps, "steps_not_listed": steps});
    }
    Some(shown)
}

/// Reconcile status from durable broker facts, never the model's self-report.
pub fn reconcile(state: &mut ResearchState, facts: &Value) -> Result<bool> {
    let facts = facts
        .as_array()
        .ok_or_else(|| anyhow!("task_facts_invalid"))?;
    let mut active = false;
    for step in &mut state.plan {
        let Some(task) = step.task_id else { continue };
        let fact = facts
            .iter()
            .find(|f| f["task_id"].as_str() == Some(task.to_string().as_str()))
            .ok_or_else(|| anyhow!("task_facts_incomplete"))?;
        match fact["status"].as_str() {
            Some("queued" | "leased" | "starting" | "running" | "cancelling") => {
                step.status = StepStatus::Active;
                active = true;
            }
            Some("completed" | "review_required") => {
                let refs = fact["evidence_refs"]
                    .as_array()
                    .ok_or_else(|| anyhow!("task_facts_invalid"))?;
                // A step lists at most `STEP_REFS_MAX` of its results, in the claim's order, and the rest stay on the task, so
                // the checkpoint can keep room for each unfinished step's references. The lead reads
                // results by task, not from this list.
                step.evidence_refs = refs
                    .iter()
                    .filter_map(|v| v.as_str().map(str::to_owned))
                    .take(crate::broker::project::STEP_REFS_MAX)
                    .collect();
                step.status = if step.evidence_refs.is_empty() {
                    StepStatus::Uncertain
                } else {
                    StepStatus::Completed
                };
            }
            Some("failed")
                if fact["error_code"].as_str() == Some("worker_lease_expired")
                    && step.status != StepStatus::Failed =>
            {
                // Compute may have completed without retained outputs. Persist
                // uncertainty before yielding so a restart cannot retry it.
                step.status = StepStatus::Uncertain;
            }
            Some("failed" | "cancelled") => step.status = StepStatus::Failed,
            _ => return Err(anyhow!("task_facts_invalid")),
        }
    }
    state
        .validate()
        .map_err(|_| anyhow!("checkpoint_invalid"))?;
    Ok(active)
}

pub fn requires_execution_review(state: &ResearchState) -> bool {
    state
        .plan
        .iter()
        .any(|step| step.task_id.is_some() && step.status == StepStatus::Uncertain)
}
