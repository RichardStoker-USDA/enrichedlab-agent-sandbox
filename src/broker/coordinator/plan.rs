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

/// The steps no archive may move, by task and by plan key: the analyses an earlier answer reads (`bound_tasks`), each
/// analysis whose newest person review is not `accepted` (its request for changes or rejection stays in front of the
/// lead until a person resolves it), every analysis the current methods revision request cites, the orientation's
/// profiling steps (the pinned orientation shows their status from the plan), and keys already archived (archiving one
/// again would violate the archive's uniqueness and never progress).
#[derive(Debug, Default)]
pub struct HeldSteps {
    pub tasks: HashSet<String>,
    pub keys: HashSet<String>,
}

/// Every analysis an artifact reference anywhere in `value` names, and every `task_id` it carries.
fn cited_tasks(value: &Value, out: &mut HashSet<String>) {
    match value {
        Value::String(s) => {
            if let Some(task) = ref_task(s) {
                out.insert(task.to_owned());
            }
        }
        Value::Array(items) => items.iter().for_each(|v| cited_tasks(v, out)),
        Value::Object(map) => {
            for (key, v) in map {
                if key == "task_id"
                    && let Some(task) = v.as_str()
                {
                    out.insert(task.to_owned());
                }
                cited_tasks(v, out);
            }
        }
        _ => {}
    }
}

/// The held steps from the run's results read (`earlier_findings`, `reviews`, `revision_request`), the claim's archive
/// summary (`plan_pages.keys`) and the kept orientation record (`step_keys`), when the lab has one.
pub fn held_steps(results: &Value, pages: &Value, orientation: Option<&Value>) -> HeldSteps {
    let mut tasks = bound_tasks(results);
    let mut newest: HashMap<&str, (&str, &str)> = HashMap::new();
    for review in results["reviews"].as_array().into_iter().flatten() {
        let (Some(task), Some(decision)) =
            (review["task_id"].as_str(), review["decision"].as_str())
        else {
            continue;
        };
        let at = review["recorded_at"].as_str().unwrap_or_default();
        if newest.get(task).is_none_or(|(seen, _)| at >= *seen) {
            newest.insert(task, (at, decision));
        }
    }
    tasks.extend(
        newest
            .into_iter()
            .filter(|(_, (_, decision))| *decision != "accepted")
            .map(|(task, _)| task.to_owned()),
    );
    cited_tasks(&results["revision_request"], &mut tasks);
    let keys = pages["keys"]
        .as_array()
        .into_iter()
        .flatten()
        .chain(
            orientation
                .and_then(|o| o["step_keys"].as_array())
                .into_iter()
                .flatten(),
        )
        .filter_map(|k| k.as_str().map(str::to_owned))
        .collect();
    HeldSteps { tasks, keys }
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

/// The steps that may leave the working plan, in plan order: completed, failed or skipped steps whose analysis, if
/// any, finished (completed, failed or cancelled), and that are not held (`HeldSteps`).
fn movable_steps<'a>(
    state: &'a ResearchState,
    facts: &Value,
    held: &HeldSteps,
) -> Vec<&'a PlanStep> {
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
        .filter(|s| !held.keys.contains(&s.action_key))
        .filter(|s| {
            s.task_id.is_none_or(|t| {
                let t = t.to_string();
                finished.contains(t.as_str()) && !held.tasks.contains(&t)
            })
        })
        .collect()
}

/// The steps to move out of a working plan that could not take another delegation: movable steps in plan order until
/// the plan holds `PLAN_STEPS_AFTER_ARCHIVE` steps. Empty while the plan has room for `parallel` more steps, or when no
/// step can move.
pub fn plan_archive_keys(
    state: &ResearchState,
    facts: &Value,
    parallel: usize,
    held: &HeldSteps,
) -> Vec<String> {
    if state.plan.len() + parallel <= PLAN_STEPS_MAX {
        return Vec::new();
    }
    movable_steps(state, facts, held)
        .into_iter()
        .take(state.plan.len().saturating_sub(PLAN_STEPS_AFTER_ARCHIVE))
        .map(|s| s.action_key.clone())
        .collect()
}

/// The steps to move out at a segment boundary, when a lead context no longer fits its budget: every movable failed or
/// skipped step that holds no evidence (its analysis left no researcher result), in plan order, at most
/// `PLAN_STEPS_MAX` (one archive call takes 1 to 128 keys). A completed step never moves at a boundary, so no result
/// leaves the lead's context before it has been answered with; held steps stay too. Empty when no step can move.
pub fn plan_segment_keys(state: &ResearchState, facts: &Value, held: &HeldSteps) -> Vec<String> {
    let with_evidence: HashSet<&str> = facts
        .as_array()
        .into_iter()
        .flatten()
        .filter(|f| f["evidence_refs"].as_array().is_some_and(|r| !r.is_empty()))
        .filter_map(|f| f["task_id"].as_str())
        .collect();
    movable_steps(state, facts, held)
        .into_iter()
        .filter(|s| matches!(s.status, StepStatus::Failed | StepStatus::Skipped))
        .filter(|s| {
            s.task_id
                .is_none_or(|t| !with_evidence.contains(t.to_string().as_str()))
        })
        .take(PLAN_STEPS_MAX)
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
pub(super) fn plan_pages_view(pages: &Value, room: usize) -> Option<Value> {
    let steps = pages["steps"].as_u64().filter(|n| *n > 0)?;
    let size = |v: &Value| serde_json::to_vec(v).map_or(usize::MAX, |b| b.len());
    let mut shown = json!({"pages": pages["pages"], "steps": steps, "by_status": pages["by_status"],
        "newest": [], "steps_not_listed": steps});
    let mut listed: Vec<Value> = Vec::new();
    for s in pages["newest"].as_array().into_iter().flatten() {
        listed.push(json!({"key": s["action_key"], "title": s["title"], "status": s["status"]}));
        shown["newest"] = Value::Array(listed.clone());
        shown["steps_not_listed"] = json!(steps.saturating_sub(listed.len() as u64));
        if size(&shown) > room {
            listed.pop();
            break;
        }
    }
    shown["newest"] = Value::Array(listed.clone());
    shown["steps_not_listed"] = json!(steps.saturating_sub(listed.len() as u64));
    if size(&shown) > room {
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
