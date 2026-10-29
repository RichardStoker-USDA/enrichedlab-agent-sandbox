//! Step checks. Each step the research lead proposes states what its result must look
//! like: the columns it has, how many rows, which columns hold a value in every row or a different value in each, and
//! the range each numeric column must stay within, with where the bound comes from. When the result is stored, code
//! checks the result's digest against that statement before the lead's next turn reads it, so a wrong result at one
//! step is caught there and never becomes the ground of later steps. The check reads only the digest the worker wrote
//! over every row of the stored table (`digest::table`); no model judges it.
use std::collections::{BTreeSet, HashMap};

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use super::digest::{self, MAX_NAME_CHARS};
use super::project::PlanStep;

/// The most names one list of an expectation may hold.
pub const EXPECT_NAMES_MAX: usize = 16;
/// The most ranges one expectation may hold.
pub const EXPECT_RANGES_MAX: usize = 12;
/// The longest a range's basis may be: the plain words saying where its bounds come from.
pub const BASIS_CHARS: usize = 300;
/// The longest a column or table name in an expectation may be; the digest keeps column names to 64 characters.
const NAME_CHARS: usize = 200;
/// The most bytes one expectation may take in the checkpoint, which holds at most 128 steps within 96 KiB.
pub const EXPECT_BYTES: usize = 2048;

/// What a step's result must look like, as the lead states it with the step.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Expectation {
    /// The result table it is about, by its path in the result; absent means the result's only table.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub table: Option<String>,
    /// Columns the table must have, by exact name.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub columns: Vec<String>,
    /// Inclusive bounds on how many rows the table holds.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rows: Option<RowBounds>,
    /// Columns that must hold a value in every row.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub not_null: Vec<String>,
    /// Columns whose values must all differ.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub unique: Vec<String>,
    /// Numeric columns whose every value must fall within bounds.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub ranges: Vec<ValueRange>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RowBounds {
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "count_bound"
    )]
    pub min: Option<u64>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "count_bound"
    )]
    pub max: Option<u64>,
}

/// A bound as the lead wrote it: a number, or the same number written as text (a test run saw a lead
/// write `"max": "100"`, which refused the whole reply). Text that is not exactly one finite number is still refused,
/// and the bound is stored as a number either way, so the checkpoint's format does not change.
fn value_bound<'de, D: serde::Deserializer<'de>>(d: D) -> Result<Option<f64>, D::Error> {
    match Option::<Value>::deserialize(d)? {
        None | Some(Value::Null) => Ok(None),
        Some(Value::Number(n)) => n
            .as_f64()
            .filter(|v| v.is_finite())
            .map(Some)
            .ok_or_else(|| serde::de::Error::custom("bound_not_finite")),
        Some(Value::String(t)) => t
            .trim()
            .parse::<f64>()
            .ok()
            .filter(|v| v.is_finite() && !t.trim().is_empty())
            .map(Some)
            .ok_or_else(|| serde::de::Error::custom("bound_not_a_number")),
        Some(_) => Err(serde::de::Error::custom("bound_not_a_number")),
    }
}

/// A row-count bound: a whole number, or one written as text.
fn count_bound<'de, D: serde::Deserializer<'de>>(d: D) -> Result<Option<u64>, D::Error> {
    match Option::<Value>::deserialize(d)? {
        None | Some(Value::Null) => Ok(None),
        Some(Value::Number(n)) => n
            .as_u64()
            .map(Some)
            .ok_or_else(|| serde::de::Error::custom("count_not_whole")),
        Some(Value::String(t)) => t
            .trim()
            .parse::<u64>()
            .map(Some)
            .map_err(|_| serde::de::Error::custom("count_not_whole")),
        Some(_) => Err(serde::de::Error::custom("count_not_whole")),
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ValueRange {
    pub column: String,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "value_bound"
    )]
    pub min: Option<f64>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "value_bound"
    )]
    pub max: Option<f64>,
    /// Where the bounds come from, in plain words: a physical limit, a value a paper reports or an earlier result.
    pub basis: String,
}

/// The outcome of one result's check, written by code.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CheckState {
    /// Every stated property holds for every row of the stored table.
    Passed,
    /// No property failed, but at least one could not be read from the digest. It is not a pass.
    NotCheckable,
    /// At least one stated property does not hold.
    Failed,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StepCheck {
    pub state: CheckState,
    /// One plain sentence for each property that failed, then each that could not be read, in the expectation's
    /// order.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub reasons: Vec<String>,
    /// The table the check read, by its path in the result.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub table: Option<String>,
}

/// One result's check, with its step's key and the result's reference, as the lead's context and the workspace show it.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ResultCheck {
    pub key: String,
    pub task_id: String,
    #[serde(rename = "ref")]
    pub reference: String,
    #[serde(flatten)]
    pub check: StepCheck,
}

fn name_ok(name: &str) -> bool {
    !name.trim().is_empty()
        && name.chars().count() <= NAME_CHARS
        && !name.chars().any(char::is_control)
}

/// Why an expectation cannot be checked as written, or `None` when it can: each code names the part at fault.
pub fn invalid(expect: &Expectation) -> Option<&'static str> {
    if expect.columns.is_empty()
        && expect.rows.is_none()
        && expect.not_null.is_empty()
        && expect.unique.is_empty()
        && expect.ranges.is_empty()
    {
        return Some("expect_empty");
    }
    if expect.table.as_deref().is_some_and(|t| !name_ok(t)) {
        return Some("expect_table");
    }
    for (list, code) in [
        (&expect.columns, "expect_columns"),
        (&expect.not_null, "expect_not_null"),
        (&expect.unique, "expect_unique"),
    ] {
        let distinct: BTreeSet<&String> = list.iter().collect();
        if list.len() > EXPECT_NAMES_MAX
            || distinct.len() != list.len()
            || !list.iter().all(|n| name_ok(n))
        {
            return Some(code);
        }
    }
    if let Some(rows) = &expect.rows
        && (rows.min.is_none() && rows.max.is_none()
            || rows.min.zip(rows.max).is_some_and(|(a, b)| a > b))
    {
        return Some("expect_rows");
    }
    if expect.ranges.len() > EXPECT_RANGES_MAX {
        return Some("expect_ranges");
    }
    let finite = |v: Option<f64>| v.is_none_or(f64::is_finite);
    for r in &expect.ranges {
        if !name_ok(&r.column)
            || r.min.is_none() && r.max.is_none()
            || !finite(r.min)
            || !finite(r.max)
            || r.min.zip(r.max).is_some_and(|(a, b)| a > b)
            || r.basis.trim().is_empty()
            || r.basis.chars().count() > BASIS_CHARS
            || r.basis.chars().any(char::is_control)
        {
            return Some("expect_ranges");
        }
    }
    if serde_json::to_vec(expect).map_or(usize::MAX, |b| b.len()) > EXPECT_BYTES {
        return Some("expect_too_long");
    }
    None
}

fn unreadable(status: Option<&str>) -> &'static str {
    match status {
        Some("not_a_package") => "the result is not a package",
        Some("integrity_failed") => "its checksums did not match",
        Some("too_large") => "it is too large to read",
        _ => "it could not be read",
    }
}

/// Check one stored result against its step's expectation, from the result's digest (`ResultDigest::value`).
pub fn check(expect: &Expectation, digest: &Value) -> StepCheck {
    let not_checkable = |reason: String| StepCheck {
        state: CheckState::NotCheckable,
        reasons: vec![reason],
        table: None,
    };
    let status = digest["status"].as_str();
    if !matches!(status, Some("summarized" | "no_result_tables")) {
        return not_checkable(format!(
            "The result could not be checked: {}.",
            unreadable(status)
        ));
    }
    let tables: Vec<&Value> = digest["tables"].as_array().into_iter().flatten().collect();
    let tables_omitted = digest["tables_omitted"].as_u64().unwrap_or(0) > 0;
    let chosen = match &expect.table {
        Some(path) => tables
            .iter()
            .find(|t| t["path"].as_str() == Some(path.as_str()))
            .copied(),
        None if tables.len() == 1 && !tables_omitted => Some(tables[0]),
        None => None,
    };
    let Some(table) = chosen else {
        return match &expect.table {
            Some(path) if tables_omitted => not_checkable(format!(
                "The result's summary does not list table {path} among the tables it kept."
            )),
            Some(path) => StepCheck {
                state: CheckState::Failed,
                reasons: vec![format!("The result has no table {path}.")],
                table: None,
            },
            None if tables.is_empty() && !tables_omitted => StepCheck {
                state: CheckState::Failed,
                reasons: vec!["The result has no table to check.".to_owned()],
                table: None,
            },
            None => not_checkable(
                "The result has several tables and the expectation names none of them.".to_owned(),
            ),
        };
    };
    let path = table["path"].as_str().unwrap_or_default().to_owned();
    let mut failed = Vec::new();
    let mut unread = Vec::new();
    let listed: Vec<&str> = table["columns"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .collect();
    let partial = table["columns_omitted"].as_u64().unwrap_or(0) > 0;
    let rows = table["rows"].as_u64();
    // The worker counts missing values and summarizes the columns together, and only when every row could be read and
    // the column names differ (`digest::table`); a table without them carries no marker, so the summary is the sign.
    let counted = table.get("summary").is_some() || table.get("summary_omitted").is_some();
    let missing_counted = counted && table.get("missing_omitted").is_none();
    // Whether a named column can be checked: a name the digest does not list fails, unless the digest left columns out
    // or shortened the name past `digest::MAX_NAME_CHARS`, when it cannot be read.
    let present = |name: &str, failed: &mut Vec<String>, unread: &mut Vec<String>| {
        if name.chars().count() > MAX_NAME_CHARS {
            unread.push(format!(
                "Column {name} has a longer name than the result's summary keeps, so it cannot be matched."
            ));
            false
        } else if listed.contains(&name) {
            true
        } else if partial {
            unread.push(format!(
                "Column {name} is not among the columns the result's summary lists."
            ));
            false
        } else {
            failed.push(format!("The table has no column {name}."));
            false
        }
    };
    for name in &expect.columns {
        present(name, &mut failed, &mut unread);
    }
    if let Some(bounds) = &expect.rows {
        match rows {
            None => unread.push("The result's summary does not count the table's rows.".to_owned()),
            Some(n) => {
                if let Some(min) = bounds.min.filter(|min| n < *min) {
                    failed.push(format!(
                        "The table has {n} rows, fewer than the {min} expected."
                    ));
                }
                if let Some(max) = bounds.max.filter(|max| n > *max) {
                    failed.push(format!(
                        "The table has {n} rows, more than the {max} expected."
                    ));
                }
            }
        }
    }
    // Only columns with a row lacking a value are listed under `missing`.
    let missing = |name: &str| table["missing"][name].as_u64().unwrap_or(0);
    for name in &expect.not_null {
        if !present(name, &mut failed, &mut unread) {
            continue;
        }
        if !missing_counted {
            unread.push(format!(
                "The result's summary does not count the rows without a value in column {name}."
            ));
        } else if missing(name) > 0 {
            failed.push(format!(
                "Column {name} has no value in {} rows.",
                missing(name)
            ));
        }
    }
    let summary = |name: &str| table["summary"].get(name).filter(|s| s.is_object());
    for name in &expect.unique {
        if !present(name, &mut failed, &mut unread) {
            continue;
        }
        let distinct = summary(name).and_then(|s| s["distinct"].as_u64());
        match (distinct, rows) {
            (Some(distinct), Some(n)) if missing_counted => {
                let present = n.saturating_sub(missing(name));
                if distinct < present {
                    failed.push(format!(
                        "Column {name} holds {distinct} distinct values in {present} rows with a value, so some repeat."
                    ));
                }
            }
            // Without the missing counts, as many distinct values as rows still shows every value differs.
            (Some(distinct), Some(n)) if distinct == n => {}
            _ => unread.push(format!(
                "The result's summary does not count the distinct values of column {name}."
            )),
        }
    }
    for r in &expect.ranges {
        let name = r.column.as_str();
        if !present(name, &mut failed, &mut unread) {
            continue;
        }
        let Some(s) = summary(name) else {
            unread.push(format!(
                "The result's summary does not cover column {name}."
            ));
            continue;
        };
        // No row holds a value, so no value is out of range.
        if s["distinct"].as_u64() == Some(0) {
            continue;
        }
        if s["numeric"].as_bool() != Some(true) {
            failed.push(format!(
                "Column {name} holds values that are not numbers, so its range cannot hold."
            ));
            continue;
        }
        let bound = |key: &str| {
            s[key]
                .as_str()
                .and_then(|v| v.parse::<f64>().ok().map(|n| (n, v)))
        };
        let (Some((low, low_text)), Some((high, high_text))) = (bound("min"), bound("max")) else {
            // A lab's own table keeps no values in its digest, so its range is not read here.
            unread.push(format!(
                "The result's summary does not give the range of column {name}."
            ));
            continue;
        };
        if let Some(min) = r.min.filter(|min| low < *min) {
            failed.push(format!(
                "Column {name} goes down to {low_text}, below the {} expected ({}).",
                number(min),
                r.basis
            ));
        }
        if let Some(max) = r.max.filter(|max| high > *max) {
            failed.push(format!(
                "Column {name} goes up to {high_text}, above the {} expected ({}).",
                number(max),
                r.basis
            ));
        }
    }
    let state = if !failed.is_empty() {
        CheckState::Failed
    } else if !unread.is_empty() {
        CheckState::NotCheckable
    } else {
        CheckState::Passed
    };
    failed.extend(unread);
    StepCheck {
        state,
        reasons: failed,
        table: Some(path),
    }
}

/// The check of every result of each step that states an expectation, in the order of `results`: the coordinator's
/// result list, one item per stored result with its `task_id`, `ref` and `digest`.
pub fn checks_for(plan: &[PlanStep], results: &Value) -> Vec<ResultCheck> {
    let expected: HashMap<String, (&str, &Expectation)> = plan
        .iter()
        .filter_map(|s| {
            Some((
                s.task_id?.to_string(),
                (s.action_key.as_str(), s.expect.as_ref()?),
            ))
        })
        .collect();
    results
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|item| {
            let task = item["task_id"].as_str()?;
            let (key, expect) = expected.get(task)?;
            Some(ResultCheck {
                key: (*key).to_owned(),
                task_id: task.to_owned(),
                reference: item["ref"].as_str()?.to_owned(),
                check: check(expect, &item["digest"]),
            })
        })
        .collect()
}

/// The most reasons one analysis shows the lead; `reasons_omitted` counts the rest. The workspace shows them all.
pub const LEAD_REASONS_MAX: usize = 6;

/// The cells-withheld reason of a table whose result failed its check.
pub const CHECK_FAILED: &str = "check_failed";

/// One lead call's result checks (`checks_for`, as JSON), by result and by step: a step with several results shows its
/// worst, the first of equals in the results' order.
pub struct ShownChecks<'a> {
    by_ref: HashMap<&'a str, &'a Value>,
    by_task: HashMap<&'a str, &'a Value>,
}

impl<'a> ShownChecks<'a> {
    pub fn of(checks: &'a Value) -> Self {
        let rank = |c: &Value| match c["state"].as_str() {
            Some("failed") => 2,
            Some("not_checkable") => 1,
            _ => 0,
        };
        let mut by_ref = HashMap::new();
        let mut by_task: HashMap<&str, &Value> = HashMap::new();
        for c in checks.as_array().into_iter().flatten() {
            let (Some(reference), Some(task)) = (c["ref"].as_str(), c["task_id"].as_str()) else {
                continue;
            };
            by_ref.insert(reference, c);
            if by_task.get(task).is_none_or(|old| rank(c) > rank(old)) {
                by_task.insert(task, c);
            }
        }
        Self { by_ref, by_task }
    }

    /// Whether the result with this reference failed its check.
    pub fn failed(&self, reference: &str) -> bool {
        self.by_ref
            .get(reference)
            .is_some_and(|c| c["state"] == "failed")
    }

    /// A step's check as the lead reads it on its analysis: the state and at most `LEAD_REASONS_MAX` reasons.
    pub fn analysis(&self, task: &str) -> Option<Value> {
        let c = self.by_task.get(task)?;
        let mut shown = json!({"state": c["state"]});
        if let Some(reasons) = c["reasons"].as_array() {
            shown["reasons"] = json!(reasons.iter().take(LEAD_REASONS_MAX).collect::<Vec<_>>());
            if reasons.len() > LEAD_REASONS_MAX {
                shown["reasons_omitted"] = json!(reasons.len() - LEAD_REASONS_MAX);
            }
        }
        Some(shown)
    }
}

/// The bytes of a turn record the step checks may take; the whole record holds at most 128 KiB (migration 011).
pub const RECORD_BYTES: usize = 12 * 1024;

/// The checks a turn's record keeps for the workspace, as the lead read them: each with its step key, task, result, state,
/// table and at most `LEAD_REASONS_MAX` reasons, in the results' order, while they fit `RECORD_BYTES`; `omitted` counts
/// the rest.
pub fn record_view(checks: &Value) -> Value {
    let (mut kept, mut bytes, mut omitted) = (Vec::new(), 2_usize, 0_usize);
    for c in checks.as_array().into_iter().flatten() {
        let mut v = c.clone();
        if let Some(reasons) = c["reasons"].as_array()
            && reasons.len() > LEAD_REASONS_MAX
        {
            v["reasons"] = json!(reasons[..LEAD_REASONS_MAX]);
            v["reasons_omitted"] = json!(reasons.len() - LEAD_REASONS_MAX);
        }
        let n = serde_json::to_vec(&v)
            .map_or(usize::MAX, |b| b.len())
            .saturating_add(1);
        if bytes.saturating_add(n) > RECORD_BYTES {
            omitted += 1;
            continue;
        }
        bytes += n;
        kept.push(v);
    }
    json!({"checks": kept, "omitted": omitted})
}

/// The most failed results one turn record names for the supervisor, past its pause bound.
pub const FAILED_RESULTS_KEPT: usize = 16;

/// The results of `own` tasks whose check failed, as the supervisor counts them across a run: each
/// one's task and reference, at most `FAILED_RESULTS_KEPT`, kept apart from `record_view`, which may leave checks out for
/// room. The run's own tasks are chosen before the cap, so an earlier run's failures never take this run's places.
pub fn failed_results(checks: &Value, own: &std::collections::BTreeSet<String>) -> Value {
    json!(
        checks
            .as_array()
            .into_iter()
            .flatten()
            .filter(|c| c["state"] == "failed")
            .filter(|c| c["task_id"].as_str().is_some_and(|t| own.contains(t)))
            .filter_map(|c| Some(
                json!({"task_id": c["task_id"].as_str()?, "ref": c["ref"].as_str()?})
            ))
            .take(FAILED_RESULTS_KEPT)
            .collect::<Vec<_>>()
    )
}

/// The most bytes a turn record holds as PostgreSQL stores it (`record::text`, migration 011).
pub const TURN_RECORD_BYTES: usize = 128 * 1024;

/// Keep a turn record within `TURN_RECORD_BYTES` as PostgreSQL stores it once the commit merges `added` into it (each
/// submitted task, the actor and the outcome): its step checks give way first, the last first, each counted in
/// `omitted`. Returns how many bytes the stored record would still be over, if any.
pub fn fit_record(record: &mut Value, added: &Value) -> Option<usize> {
    let stored = |record: &Value| {
        let mut whole = record.clone();
        if let (Some(whole), Some(added)) = (whole.as_object_mut(), added.as_object()) {
            whole.extend(added.clone());
        }
        digest::stored_bytes(&whole)
    };
    while stored(record) > TURN_RECORD_BYTES {
        let Some(checks) = record["step_checks"]["checks"].as_array_mut() else {
            break;
        };
        if checks.pop().is_none() {
            break;
        }
        let omitted = record["step_checks"]["omitted"].as_u64().unwrap_or(0) + 1;
        record["step_checks"]["omitted"] = json!(omitted);
    }
    let over = stored(record).saturating_sub(TURN_RECORD_BYTES);
    (over > 0).then_some(over)
}

/// How many of a call's results passed, failed or could not be checked, for the turn's record of its context.
pub fn counts(checks: &Value) -> Value {
    let mut out = json!({"passed": 0, "failed": 0, "not_checkable": 0});
    for c in checks.as_array().into_iter().flatten() {
        if let Some(state) = c["state"].as_str()
            && let Some(n) = out[state].as_u64()
        {
            out[state] = json!(n + 1);
        }
    }
    out
}

/// This project's own queries (migration 043) without those whose result failed its check, for the rule that refuses a
/// query an analysis already ran: such a query may run again under a corrected expectation.
pub fn without_failed(own: &Value, checks: &Value) -> Value {
    let failed: BTreeSet<&str> = checks
        .as_array()
        .into_iter()
        .flatten()
        .filter(|c| c["state"] == "failed")
        .filter_map(|c| c["task_id"].as_str())
        .collect();
    Value::Array(
        own.as_array()
            .into_iter()
            .flatten()
            .filter(|q| !q["task_id"].as_str().is_some_and(|t| failed.contains(t)))
            .cloned()
            .collect(),
    )
}

/// A result as the lead reads it after its check failed: each table keeps its shape and counts but not its cells, so no
/// finding, table statement, answer table or later analysis can rest on it.
pub fn withhold_failed(view: &mut Value) {
    for t in view["tables"].as_array_mut().into_iter().flatten() {
        if let Some(t) = t.as_object_mut() {
            t.remove("cells");
            t.remove("rows_shown");
            t.insert("cells_omitted".into(), json!(CHECK_FAILED));
        }
    }
}

/// A bound as the lead wrote it, without a trailing `.0`.
fn number(v: f64) -> String {
    if v.fract() == 0.0 && v.abs() < 1e15 {
        format!("{}", v as i64)
    } else {
        format!("{v}")
    }
}

#[cfg(test)]
#[path = "step_checks_tests.rs"]
mod tests;
