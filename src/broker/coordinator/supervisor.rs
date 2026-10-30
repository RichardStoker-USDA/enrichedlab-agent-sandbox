//! The run's supervisor. Before each lead call, code counts the run's faults across the whole run,
//! not only the last calls in a row: a refusal reason that keeps returning after successes, refused replies in all,
//! results that failed their step's check, and delegating turns that added nothing usable. Between a bound's narrowing
//! and pausing counts every call is narrowed through the context's `redirect`, which the reply check already holds the
//! reply to; at the pausing count the run stops for a person with a fixed reason. The supervisor never edits a reply.
use super::methods_review::Redirect;
use super::*;

/// A refusal reason seen this many times in the run narrows the next call, where a narrowing fits the reason.
pub const REPEATS_TO_NARROW: usize = 3;
/// The same reason this many times pauses the run.
pub const REPEATS_TO_PAUSE: usize = 4;
/// Refused replies in all, of any reason, that pause the run.
pub const REFUSALS_TO_PAUSE: usize = 6;
/// Results whose step check failed that narrow the next call to one step.
pub const FAILED_CHECKS_TO_NARROW: usize = 3;
/// Failed results that pause the run.
pub const FAILED_CHECKS_TO_PAUSE: usize = 5;
/// Delegating turns in a row, newest first, that added no usable result and narrow the next call.
pub const IDLE_TURNS_TO_NARROW: usize = 2;
/// Idle delegating turns in a row that pause the run.
pub const IDLE_TURNS_TO_PAUSE: usize = 3;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Reason {
    RepeatedRefusal,
    RefusalsTotal,
    FailedChecks,
    NoProgress,
}

impl Reason {
    /// The fixed reason the pause records (migration 074 accepts exactly these).
    pub fn as_str(self) -> &'static str {
        match self {
            Reason::RepeatedRefusal => "repeated_refusal",
            Reason::RefusalsTotal => "refusals_total",
            Reason::FailedChecks => "failed_checks",
            Reason::NoProgress => "no_progress",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Action {
    /// Narrow the next call with this redirect.
    Narrow(Reason, Redirect),
    /// Pause the run for a person.
    Pause(Reason),
}

/// What the supervisor counted for one call; the turn record keeps it when the supervisor acts.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
pub struct Counts {
    /// The refusal reason seen most often in the run, and how often.
    pub repeated: Option<(String, usize)>,
    pub refusals: usize,
    pub failed_checks: usize,
    pub idle_turns: usize,
}

/// Whether a deferral's reason is the reply check refusing the lead's own reply: its plan, findings, tables, expectations,
/// queries (`sql_refused*`) or code (`code_*`), and reading rounds it spent. Not counted, though some share the event
/// code `proposal_invalid`: a service or envelope fault (`model_*`), a catalog defect (`configured_*`, an analysis no
/// worker read), the methods specialist's own reply (`method_review_*`), a context the coordinator could not build
/// (`context_*`) and an internal fault.
pub fn reply_refusal(reason: &str) -> bool {
    (reason.starts_with("proposal_") && reason != "proposal_analysis_unavailable")
        || reason.starts_with("sql_refused")
        || reason.starts_with("code_")
        || reason == "read_rounds_spent"
}

/// The tasks this run's own committed turns submitted, from what the database returned (`broker_research_run_faults`).
pub fn own_tasks(faults: &Value) -> BTreeSet<String> {
    faults["turns"]
        .as_array()
        .into_iter()
        .flatten()
        .flat_map(|t| t["tasks"].as_array().into_iter().flatten())
        .filter_map(|t| t.as_str().map(str::to_owned))
        .collect()
}

/// Counts the run's faults from what the database returned (`broker_research_run_faults`), the current step checks of
/// its results and the task facts of the claim. Only this run counts: its own refused replies, and checks of the tasks
/// its own turns submitted, so a project's earlier runs never weigh on a new one. A failure stays counted after its
/// step leaves the working plan, from the turn record that read it.
pub fn counts(faults: &Value, checks: &Value, facts: &Value) -> Counts {
    let mut reasons: BTreeMap<&str, usize> = BTreeMap::new();
    let mut refusals = 0;
    for event in faults["events"].as_array().into_iter().flatten() {
        let Some(reason) = event["reason"].as_str() else {
            continue;
        };
        if event["code"] != "proposal_invalid" || !reply_refusal(reason) {
            continue;
        }
        refusals += 1;
        *reasons.entry(reason).or_default() += 1;
    }
    let turns_all: Vec<&Value> = faults["turns"].as_array().into_iter().flatten().collect();
    let own = own_tasks(faults);
    let own_tasks: BTreeSet<&str> = own.iter().map(String::as_str).collect();
    // The most frequent reason; of equal counts the first by name, so the choice is deterministic.
    let repeated = reasons
        .iter()
        .max_by(|a, b| a.1.cmp(b.1).then(b.0.cmp(a.0)))
        .map(|(reason, n)| ((*reason).to_owned(), *n));
    let checks: Vec<&Value> = checks
        .as_array()
        .into_iter()
        .flatten()
        .filter(|c| c["task_id"].as_str().is_some_and(|t| own_tasks.contains(t)))
        .collect();
    // Each failed result counts once, by its reference, whether read now or by an earlier turn whose step was archived.
    let mut failed: BTreeSet<&str> = turns_all
        .iter()
        .flat_map(|t| t["failed"].as_array().into_iter().flatten())
        .filter(|f| f["task_id"].as_str().is_some_and(|t| own_tasks.contains(t)))
        .filter_map(|f| f["ref"].as_str())
        .collect();
    failed.extend(
        checks
            .iter()
            .filter(|c| c["state"] == "failed")
            .filter_map(|c| c["ref"].as_str()),
    );
    let failed_checks = failed.len();
    let task_status = |task: &str| {
        facts
            .as_array()
            .into_iter()
            .flatten()
            .find(|f| f["task_id"].as_str() == Some(task))
            .and_then(|f| f["status"].as_str())
    };
    // A task added something usable when it completed with a result whose check did not fail: a passed check, or one
    // its summary could not decide, which is not held against the lead.
    let usable = |task: &str| {
        matches!(task_status(task), Some("completed" | "review_required"))
            && checks
                .iter()
                .filter(|c| c["task_id"].as_str() == Some(task))
                .any(|c| c["state"] != "failed")
    };
    let finished = |task: &str| {
        matches!(
            task_status(task),
            Some("completed" | "review_required" | "failed" | "cancelled")
        )
    };
    let mut idle_turns = 0;
    let mut turns = turns_all;
    turns.sort_by_key(|t| std::cmp::Reverse(t["turn"].as_i64().unwrap_or(0)));
    for turn in turns.into_iter().filter(|t| t["outcome"] == "delegate") {
        let tasks: Vec<&str> = turn["tasks"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
            .collect();
        // A turn still running, or one without tasks, ends the count: only finished work can be idle.
        if tasks.is_empty() || !tasks.iter().all(|t| finished(t)) || tasks.iter().any(|t| usable(t))
        {
            break;
        }
        idle_turns += 1;
    }
    Counts {
        repeated,
        refusals,
        failed_checks,
        idle_turns,
    }
}

/// The supervisor's action for these counts, pausing before narrowing, or none.
pub fn decide(counts: &Counts) -> Option<Action> {
    let repeats = counts.repeated.as_ref().map_or(0, |(_, n)| *n);
    if repeats >= REPEATS_TO_PAUSE {
        return Some(Action::Pause(Reason::RepeatedRefusal));
    }
    if counts.refusals >= REFUSALS_TO_PAUSE {
        return Some(Action::Pause(Reason::RefusalsTotal));
    }
    if counts.failed_checks >= FAILED_CHECKS_TO_PAUSE {
        return Some(Action::Pause(Reason::FailedChecks));
    }
    if counts.idle_turns >= IDLE_TURNS_TO_PAUSE {
        return Some(Action::Pause(Reason::NoProgress));
    }
    if repeats >= REPEATS_TO_NARROW {
        // The narrowing the reason's own redirect would choose after two refusals in a row; a reason no narrowing
        // can repair (a number in prose, a service fault) waits for the pause instead.
        let (reason, _) = counts.repeated.as_ref()?;
        let refusal = Refusal {
            reason: interned(reason),
            names: Vec::new(),
            focus: Vec::new(),
        };
        if let Some(redirect) = Redirect::after(&refusal, 2) {
            return Some(Action::Narrow(Reason::RepeatedRefusal, redirect));
        }
    }
    if counts.failed_checks >= FAILED_CHECKS_TO_NARROW {
        return Some(Action::Narrow(Reason::FailedChecks, Redirect::OneStep));
    }
    if counts.idle_turns >= IDLE_TURNS_TO_NARROW {
        return Some(Action::Narrow(Reason::NoProgress, Redirect::OneStep));
    }
    None
}

/// Narrows this call: the context's `redirect` names the supervisor's reason, unless a refusal's own redirect
/// already narrows it, which is more specific.
pub fn narrow(context: &mut Value, reason: Reason, redirect: Redirect) -> bool {
    if context.get("redirect").is_some() {
        return false;
    }
    context["redirect"] =
        json!({"kind": redirect.as_str(), "after": format!("supervisor_{}", reason.as_str())});
    true
}

/// The turn record's view of what the supervisor counted and did.
pub fn record(counts: &Counts, action: &Action) -> Value {
    let (kind, reason) = match action {
        Action::Narrow(reason, _) => ("narrow", reason),
        Action::Pause(reason) => ("pause", reason),
    };
    json!({"action": kind, "reason": reason.as_str(), "counts": counts})
}

/// A stored refusal reason as the `&'static str` a `Refusal` carries: the reply check's own fixed code for it.
fn interned(reason: &str) -> &'static str {
    lead_check::rejection_reason(reason)
}

#[cfg(test)]
#[path = "supervisor_tests.rs"]
mod tests;
