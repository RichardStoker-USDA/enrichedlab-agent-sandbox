//! The research lead's template analyses. A run that lists the lab's template-analysis method
//! shows the lead the newest analysis catalog, compactly; a step for that method names a finished step of the plan, one
//! of its result tables as the context showed it, an analysis the method listed and the settings. The lead chooses
//! names and settings only: no code and no values move through it. This module checks a step against what the context
//! showed before the methods review spends a call on it; the turn commit runs the database's own checks again
//! (migration 060, `broker_check_analysis_request`).
use super::{analysis_view, coordinator::CodeRefused, figure_run};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};
use sha2::{Digest, Sha256};
use std::collections::{HashMap, HashSet};

/// The limits `broker_check_analysis_request` (migration 058) holds a request to.
const SETTINGS_MAX: usize = 40;
const SETTINGS_BYTES: usize = 8192;
const TABLE_ROWS_MAX: u64 = 50_000;
const TABLE_COLUMNS_MAX: usize = 64;
const LIST_MAX: usize = 16;
const TITLE_CHARS: usize = 200;

/// The analysis a step of the template-analysis method runs: `source` is the key of a finished step, `table` one of its
/// result tables, `feature` an analysis the method lists and `settings` its values by the catalog's types.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AnalysisChoice {
    pub source: String,
    pub table: String,
    pub feature: String,
    #[serde(default)]
    pub settings: Map<String, Value>,
}

/// A step's analysis as the checks admitted it, in the terms the turn commit stores (`template_analysis`). `artifact` is
/// the result the lead was shown the table in and `catalog_sha256` the catalog it chose from: the commit checks the same
/// ones, so the analysis reads the table the lead saw, or the commit refuses it.
#[derive(Clone, Debug, PartialEq)]
pub struct AdmittedAnalysis {
    pub source_task: String,
    pub artifact: String,
    pub table_path: String,
    pub feature: String,
    pub settings: Value,
    pub title: String,
    pub catalog_sha256: String,
}

impl AdmittedAnalysis {
    /// The object the turn commit checks and records.
    pub fn record(&self) -> Value {
        json!({"source_task": self.source_task, "artifact": self.artifact, "table_path": self.table_path,
            "feature": self.feature, "settings": self.settings, "title": self.title,
            "catalog_sha256": self.catalog_sha256})
    }

    /// The record's digest, part of the task request so a replay with another analysis under the same step cannot pass
    /// as the same request.
    pub fn sha256(&self) -> String {
        let bytes = serde_json::to_vec(&self.record()).unwrap_or_default();
        format!("sha256:{}", hex::encode(Sha256::digest(bytes)))
    }
}

/// The run's methods with the template-analysis method marked: its entry carries `analysis_catalog`, the newest catalog
/// or null before a worker read one. `read` is `broker_research_run_analysis_catalog`'s answer.
pub fn with_analysis_catalog(methods: &Value, read: &Value) -> Value {
    let mut methods = methods.clone();
    if let Some(key) = read["profile"].as_str() {
        for method in methods.as_array_mut().into_iter().flatten() {
            if method["key"] == key {
                method["analysis_catalog"] = read["catalog"].clone();
            }
        }
    }
    methods
}

/// The template-analysis method's key and its catalog (null when none was read), when the run lists it.
pub fn analysis_method(methods: &Value) -> Option<(&str, &Value)> {
    methods
        .as_array()?
        .iter()
        .find_map(|m| Some((m["key"].as_str()?, m.get("analysis_catalog")?)))
}

/// A JSON scalar as PostgreSQL's `#>> '{}'` writes it: a string's own text, a number or boolean in JSON form.
fn text_of(value: &Value) -> String {
    match value {
        Value::String(s) => s.clone(),
        other => other.to_string(),
    }
}

/// A column name in either the result view's historical string form or the typed input table's object form.
fn column_name(column: &Value) -> Option<&str> {
    column
        .as_str()
        .or_else(|| column.get("name").and_then(Value::as_str))
}

fn table_columns(table: &Value) -> Vec<&str> {
    table["columns"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(column_name)
        .collect()
}

/// The input table contract shown for one source and path. When the field is present it is authoritative: a table left
/// out for its bounded share cannot be chosen from an output-file name elsewhere in the context.
fn analysis_source_table<'a>(context: &'a Value, source: &str, table: &str) -> Option<&'a Value> {
    context["analysis_source_tables"]
        .as_array()
        .into_iter()
        .flatten()
        .find(|shown| shown["source"] == source && shown["table"] == table)
}

/// The result artifact and table in the older output view, retained as a fallback for replaying contexts recorded
/// before `analysis_source_tables` and for resolving a typed input whose result reference is absent.
fn result_table<'a>(step: &'a Value, task: &str, path: &str) -> Option<(String, &'a Value)> {
    step["results"]
        .as_array()
        .into_iter()
        .flatten()
        .find_map(|result| {
            let table = result["tables"]
                .as_array()?
                .iter()
                .find(|table| table["table"] == path)?;
            Some((result_artifact(result["ref"].as_str()?, task)?, table))
        })
}

/// The table and column names the bounded input list offered for `source`, in the same order. Legacy contexts derive
/// them from the source step's result view. These are names only; no result cell enters refusal feedback.
fn available_table_faults(context: &Value, step: &Value, source: &str) -> Vec<String> {
    let listed = context.get("analysis_source_tables").is_some();
    let tables: Vec<&Value> = if listed {
        context["analysis_source_tables"]
            .as_array()
            .into_iter()
            .flatten()
            .filter(|table| table["source"] == source)
            .collect()
    } else {
        step["results"]
            .as_array()
            .into_iter()
            .flatten()
            .flat_map(|result| result["tables"].as_array().into_iter().flatten())
            .collect()
    };
    let mut paths = Vec::new();
    let mut columns = Vec::new();
    for table in tables {
        let path = table["table"].as_str().unwrap_or_default();
        if !path.is_empty() && !paths.contains(&path) {
            paths.push(path);
        }
        for column in table_columns(table) {
            if !columns.contains(&column) {
                columns.push(column);
            }
        }
    }
    let mut faults = Vec::new();
    if !paths.is_empty() {
        faults.push(format!("available tables: {}", paths.join(", ")));
    }
    if !columns.is_empty() {
        faults.push(format!("available columns: {}", columns.join(", ")));
    }
    faults
}

/// Every step of the proposal for the template-analysis method, checked against the method's catalog and the results
/// `context` showed; `rejected` holds the analyses a person rejected. A step for another method must not carry an
/// analysis. A refusal names the step and what was wrong, for the next lead call.
pub fn admit(
    actions: &[super::coordinator::ProposedAction],
    methods: &Value,
    context: &Value,
    rejected: &HashSet<String>,
) -> Result<HashMap<String, AdmittedAnalysis>, CodeRefused> {
    let method = analysis_method(methods);
    let shown_method = method.and_then(|(key, _)| {
        context["methods"]
            .as_array()?
            .iter()
            .find(|m| m["key"] == key)
    });
    let ids = |field: &str| -> HashSet<&str> {
        shown_method
            .and_then(|m| m[field].as_array())
            .into_iter()
            .flatten()
            .filter_map(|a| a["id"].as_str())
            .collect()
    };
    // Analyses shown with their contract can run; those listed by id alone must be shown first.
    let shown_ids = ids("analyses");
    let listed_ids = ids("other_analyses");
    let mut admitted = HashMap::new();
    for a in actions {
        let refused = |code: &'static str, extra: &[&str]| CodeRefused {
            code,
            names: std::iter::once(a.action_key.clone())
                .chain(extra.iter().map(|s| (*s).to_owned()))
                .collect(),
        };
        let choice = match (method.filter(|(key, _)| *key == a.profile), &a.analysis) {
            (None, None) => continue,
            (None, Some(_)) => return Err(refused("proposal_analysis_unexpected", &[])),
            (Some(_), None) => return Err(refused("proposal_analysis_missing", &[])),
            (Some(_), Some(choice)) => choice,
        };
        if a.code.is_some() || a.settings.is_some() || a.sql.is_some() {
            return Err(refused("proposal_analysis_unexpected", &[]));
        }
        let catalog = method.map_or(&Value::Null, |(_, c)| c);
        let catalog_sha256 = catalog["catalog_sha256"]
            .as_str()
            .filter(|s| s.len() == 64 && s.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f')))
            .ok_or_else(|| refused("proposal_analysis_unavailable", &[]))?;
        let feature = catalog["features"]
            .as_array()
            .into_iter()
            .flatten()
            .find(|f| f["id"] == choice.feature.as_str())
            .ok_or_else(|| refused("proposal_analysis_feature_unknown", &[&choice.feature]))?;
        if !shown_ids.contains(choice.feature.as_str()) {
            return Err(if listed_ids.contains(choice.feature.as_str()) {
                refused(analysis_view::CONTRACT_NOT_SHOWN, &[&choice.feature])
            } else {
                refused("proposal_analysis_feature_unknown", &[&choice.feature])
            });
        }
        let step = context["analyses"]
            .as_array()
            .into_iter()
            .flatten()
            .find(|s| s["key"] == choice.source.as_str())
            .ok_or_else(|| refused("proposal_analysis_source_unknown", &[&choice.source]))?;
        let task = step["analysis_id"]
            .as_str()
            .ok_or_else(|| refused("proposal_analysis_source_unknown", &[&choice.source]))?;
        // The plan shows a step awaiting review as completed; the commit needs the analysis itself completed.
        if step["status"] != "completed" || step["record"]["status"] != "completed" {
            return Err(refused(
                "proposal_analysis_source_not_finished",
                &[&choice.source],
            ));
        }
        if rejected.contains(task) {
            return Err(refused(
                "proposal_analysis_source_rejected",
                &[&choice.source],
            ));
        }
        // The dedicated input list is authoritative when present. Its result reference pins the artifact without
        // requiring the output excerpt to fit too; older recorded contexts still resolve the result they showed.
        let input = analysis_source_table(context, &choice.source, &choice.table);
        let result = result_table(step, task, &choice.table);
        if context.get("analysis_source_tables").is_some() && input.is_none() {
            let mut names = vec![
                a.action_key.clone(),
                choice.source.clone(),
                choice.table.clone(),
            ];
            names.extend(available_table_faults(context, step, &choice.source));
            return Err(CodeRefused {
                code: "proposal_analysis_table_unknown",
                names,
            });
        }
        let table = input
            .or_else(|| result.as_ref().map(|(_, table)| *table))
            .ok_or_else(|| {
                let mut names = vec![
                    a.action_key.clone(),
                    choice.source.clone(),
                    choice.table.clone(),
                ];
                names.extend(available_table_faults(context, step, &choice.source));
                CodeRefused {
                    code: "proposal_analysis_table_unknown",
                    names,
                }
            })?;
        let artifact = input
            .and_then(|shown| shown["result"].as_str())
            .and_then(|reference| result_artifact(reference, task))
            .or_else(|| result.as_ref().map(|(artifact, _)| artifact.clone()))
            .ok_or_else(|| {
                refused(
                    "proposal_analysis_table_unknown",
                    &[&choice.source, &choice.table],
                )
            })?;
        let columns = table_columns(table);
        let result_shape = result.as_ref().map(|(_, table)| *table);
        let rows = table["rows"]
            .as_u64()
            .or_else(|| result_shape.and_then(|shown| shown["rows"].as_u64()));
        let columns_omitted = table
            .get("columns_omitted")
            .or_else(|| result_shape.and_then(|shown| shown.get("columns_omitted")));
        if !rows.is_some_and(|n| (1..=TABLE_ROWS_MAX).contains(&n))
            || !(1..=TABLE_COLUMNS_MAX).contains(&columns.len())
            || columns_omitted.is_some()
        {
            return Err(refused(
                "proposal_analysis_table_unsuitable",
                &[&choice.table],
            ));
        }
        // Measured as the commit measures it, in PostgreSQL's text for jsonb, which holds no NUL character.
        let settings = Value::Object(choice.settings.clone());
        if choice.settings.len() > SETTINGS_MAX
            || jsonb_text_bytes(&settings).is_none_or(|n| n > SETTINGS_BYTES)
        {
            return Err(refused("proposal_analysis_settings_invalid", &[]));
        }
        let params = &feature["params"];
        for (name, value) in &choice.settings {
            let Some(p) = params.get(name) else {
                // The analysis's own setting names, as `available tables` does for a table, so the next call can correct it.
                let available = params
                    .as_object()
                    .into_iter()
                    .flatten()
                    .map(|(n, _)| n.as_str())
                    .collect::<Vec<_>>()
                    .join(", ");
                return Err(refused(
                    "proposal_analysis_setting_unknown",
                    &[name, &format!("available settings: {available}")],
                ));
            };
            if let Some(column) = unknown_column(p, value, &columns) {
                let mut names = vec![a.action_key.clone(), name.clone(), column.to_owned()];
                names.extend(available_table_faults(context, step, &choice.source));
                return Err(CodeRefused {
                    code: "proposal_analysis_column_unknown",
                    names,
                });
            }
            if !setting_fits(p, value, &columns) {
                return Err(refused("proposal_analysis_setting_invalid", &[name]));
            }
        }
        for (name, p) in params.as_object().into_iter().flatten() {
            let column = matches!(p["type"].as_str(), Some("column" | "columns"));
            if p["required"] == true && column && !choice.settings.contains_key(name) {
                return Err(refused("proposal_analysis_setting_missing", &[name]));
            }
        }
        admitted.insert(
            a.action_key.clone(),
            AdmittedAnalysis {
                source_task: task.to_owned(),
                artifact,
                table_path: choice.table.clone(),
                feature: choice.feature.clone(),
                settings,
                title: figure_run::cut(a.title.trim(), TITLE_CHARS),
                catalog_sha256: catalog_sha256.to_owned(),
            },
        );
    }
    Ok(admitted)
}

/// The artifact of an `artifact://tasks/<task>/<artifact>` reference of `task`'s results, when it names one.
fn result_artifact(reference: &str, task: &str) -> Option<String> {
    let (of, artifact) = reference
        .strip_prefix("artifact://tasks/")?
        .split_once('/')?;
    let uuid = |s: &str| uuid::Uuid::parse_str(s).is_ok_and(|u| u.hyphenated().to_string() == s);
    (of == task && uuid(artifact)).then(|| artifact.to_owned())
}

/// The bytes of a value as PostgreSQL writes a jsonb value as text: `", "` and `": "` between members and items, its
/// own string escapes, and numbers as `numeric` prints them, without an exponent (`1e3` is `1000`, `1e-7` is
/// `0.0000001`). None for a string holding NUL, which jsonb refuses.
pub fn jsonb_text_bytes(value: &Value) -> Option<usize> {
    Some(match value {
        Value::Null => 4,
        Value::Bool(b) => {
            if *b {
                4
            } else {
                5
            }
        }
        Value::Number(n) => numeric_text_bytes(&n.to_string()),
        Value::String(s) => {
            if s.contains('\0') {
                return None;
            }
            serde_json::to_string(s).ok()?.len()
        }
        Value::Array(items) => {
            let mut n = 2 + 2 * items.len().saturating_sub(1);
            for item in items {
                n += jsonb_text_bytes(item)?;
            }
            n
        }
        Value::Object(members) => {
            let mut n = 2 + 2 * members.len().saturating_sub(1);
            for (key, item) in members {
                if key.contains('\0') {
                    return None;
                }
                n += serde_json::to_string(key).ok()?.len() + 2 + jsonb_text_bytes(item)?;
            }
            n
        }
    })
}

/// The bytes PostgreSQL's `numeric` output takes for a JSON number's text: a sign unless the value is zero, the integer
/// digits without leading zeros (at least one), and as many decimals as the text's own after its exponent is applied.
fn numeric_text_bytes(text: &str) -> usize {
    let (negative, unsigned) = match text.strip_prefix('-') {
        Some(rest) => (true, rest),
        None => (false, text),
    };
    let (mantissa, exponent) = match unsigned.split_once(['e', 'E']) {
        Some((m, e)) => (m, e.parse::<i64>().unwrap_or(0)),
        None => (unsigned, 0),
    };
    let (whole, fraction) = mantissa.split_once('.').unwrap_or((mantissa, ""));
    let digits: Vec<u8> = whole.bytes().chain(fraction.bytes()).collect();
    let zero = digits.iter().all(|d| *d == b'0');
    // Where the decimal point falls in `digits` once the exponent moves it.
    let point = whole.len() as i64 + exponent;
    let scale = (fraction.len() as i64 - exponent).max(0) as usize;
    // The integer digits are the first `point` of `digits`, with zeros past their end, without leading zeros.
    let integer = if point <= 0 {
        1
    } else {
        let point = point as usize;
        let shown = &digits[..point.min(digits.len())];
        match shown.iter().position(|d| *d != b'0') {
            Some(first) => shown.len() - first + (point - shown.len()),
            None => 1,
        }
    };
    usize::from(negative && !zero) + integer + if scale > 0 { 1 + scale } else { 0 }
}

/// The first name a column setting gives that the table does not have.
fn unknown_column<'a>(p: &Value, value: &'a Value, columns: &[&str]) -> Option<&'a str> {
    let names: Vec<&Value> = match p["type"].as_str() {
        Some("column") => vec![value],
        Some("columns") => value.as_array().into_iter().flatten().collect(),
        _ => return None,
    };
    names
        .into_iter()
        .filter_map(Value::as_str)
        .find(|n| !columns.contains(n))
}

/// Whether a value fits its setting as `broker_check_analysis_request` checks it, and within a number's declared range.
fn setting_fits(p: &Value, value: &Value, columns: &[&str]) -> bool {
    let choice_of = |v: &Value| {
        p["choices"]
            .as_array()
            .into_iter()
            .flatten()
            .any(|c| text_of(c).to_lowercase() == text_of(v).to_lowercase())
    };
    let column = |v: &Value| v.as_str().is_some_and(|n| columns.contains(&n));
    let list = |v: &Value, each: &dyn Fn(&Value) -> bool| {
        v.as_array()
            .is_some_and(|l| (1..=LIST_MAX).contains(&l.len()) && l.iter().all(each))
    };
    match p["type"].as_str().unwrap_or_default() {
        "column" => column(value),
        "columns" => list(value, &column),
        "enum" => {
            matches!(value, Value::String(_) | Value::Number(_) | Value::Bool(_))
                && choice_of(value)
        }
        "number" => value.as_f64().is_some_and(|x| {
            p["min"].as_f64().is_none_or(|min| x >= min)
                && p["max"].as_f64().is_none_or(|max| x <= max)
        }),
        "numbers" => list(value, &|v: &Value| v.is_number()),
        "string" => {
            let max = p["maxlen"]
                .as_f64()
                .filter(|m| (1.0..=9999.0).contains(m))
                .map_or(200, |m| (m.floor() as usize).min(200));
            value.as_str().is_some_and(|s| s.chars().count() <= max)
        }
        "colors" => value.as_object().is_some_and(|o| {
            (1..=12).contains(&o.len())
                && o.iter().all(|(k, v)| {
                    (1..=200).contains(&k.chars().count()) && v.is_string() && choice_of(v)
                })
        }),
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::broker::coordinator::ProposedAction;

    fn catalog() -> Value {
        json!({"catalog_sha256": "a".repeat(64), "service_version": "0.6.0", "features": [
            {"id": "group_compare", "label": "Compare two groups", "description": "Compares a value between two groups and writes results/results.csv. Welch or Wilcoxon.",
             "params": {"value": {"type": "column", "required": true}, "group": {"type": "column", "required": true},
                        "test": {"type": "enum", "choices": ["t", "wilcoxon"]}}},
            {"id": "animal_window_summary", "label": "Per-animal summary over a time window", "description": "One row per animal from repeated rows.",
             "params": {"id": {"type": "column", "required": true}, "values": {"type": "columns", "required": true},
                        "from": {"type": "number"}, "phase_value": {"type": "string", "maxlen": 40},
                        "upper": {"type": "number", "min": 1, "max": 600}}}]})
    }

    fn methods(catalog: Value) -> Value {
        with_analysis_catalog(
            &json!([{"key": "reference-sql", "name": "Query"}, {"key": "template-analysis", "name": "Analyses"}]),
            &json!({"profile": "template-analysis", "catalog": catalog}),
        )
    }

    fn context(methods: &Value) -> Value {
        let mut shown = methods.clone();
        for m in shown.as_array_mut().unwrap() {
            if let Some(c) = m.as_object_mut().unwrap().remove("analysis_catalog") {
                let view = analysis_view::analyses_view(
                    &c,
                    analysis_view::ANALYSES_VIEW_BYTES,
                    &analysis_view::Ranking {
                        question: "",
                        around: "",
                        focus: &[],
                    },
                );
                m["analyses"] = json!(view.analyses);
                if !view.other.is_empty() {
                    m["other_analyses"] = json!(view.other);
                }
            }
        }
        json!({"methods": shown, "analysis_source_tables": [
            {"source": "hours", "table": "results/query_result.csv",
             "result": format!("artifact://tasks/{HOURS}/{SHOWN}"), "rows": 192,
             "columns": [
                {"name": "mouse", "type": "text"}, {"name": "diet", "type": "text"},
                {"name": "hour", "type": "integer"}, {"name": "ee", "type": "double precision"}]},
            {"source": "hours", "table": "results/wide.csv",
             "result": format!("artifact://tasks/{HOURS}/{SHOWN}"), "rows": 8,
             "columns": [{"name": "mouse", "type": "text"}], "columns_omitted": 70}],
            "analyses": [
            {"key": "hours", "status": "completed", "analysis_id": HOURS, "record": {"status": "completed"},
             "results": [
                {"ref": format!("artifact://tasks/{HOURS}/{SHOWN}"), "tables": [
                    {"table": "results/query_result.csv", "rows": 192, "columns": ["mouse", "diet", "hour", "ee"]},
                    {"table": "results/wide.csv", "rows": 8, "columns": ["mouse"], "columns_omitted": 70}]},
                {"ref": format!("artifact://tasks/{OTHER_TASK}/{SHOWN}"), "tables": [
                    {"table": "results/elsewhere.csv", "rows": 2, "columns": ["mouse"]}]}]},
            {"key": "running", "status": "active", "analysis_id": "22222222-2222-4222-8222-222222222222"},
            {"key": "rejected", "status": "completed", "analysis_id": "33333333-3333-4333-8333-333333333333",
             "record": {"status": "completed"}, "results_withheld": "a person rejected this analysis"},
            {"key": "in-review", "status": "completed", "analysis_id": "55555555-5555-4555-8555-555555555555",
             "record": {"status": "review_required"}, "results": [
                {"ref": "artifact://tasks/55555555-5555-4555-8555-555555555555/66666666-6666-4666-8666-666666666666",
                 "tables": [{"table": "results/query_result.csv", "rows": 2, "columns": ["mouse"]}]}]}]})
    }

    const HOURS: &str = "11111111-1111-4111-8111-111111111111";
    const SHOWN: &str = "44444444-4444-4444-8444-444444444444";
    const OTHER_TASK: &str = "77777777-7777-4777-8777-777777777777";

    fn action(profile: &str, analysis: Option<Value>) -> ProposedAction {
        let mut a = json!({"action_key": "per-animal", "title": "Energy per mouse in the dark", "profile": profile,
            "objective": "o", "rationale": "r"});
        if let Some(analysis) = analysis {
            a["analysis"] = analysis;
        }
        serde_json::from_value(a).unwrap()
    }

    fn choice(settings: Value) -> Value {
        json!({"source": "hours", "table": "results/query_result.csv", "feature": "animal_window_summary",
            "settings": settings})
    }

    fn code(
        actions: &[ProposedAction],
        methods: &Value,
    ) -> Result<HashMap<String, AdmittedAnalysis>, String> {
        let rejected = HashSet::from(["33333333-3333-4333-8333-333333333333".to_owned()]);
        admit(actions, methods, &context(methods), &rejected)
            .map_err(|e| format!("{} {}", e.code, e.names.join(",")))
    }

    #[test]
    fn a_step_names_a_finished_table_and_settings_by_type() {
        let m = methods(catalog());
        let ok = code(
            &[action(
                "template-analysis",
                Some(choice(json!({"id": "mouse", "values": ["ee"], "from": 12,
                "phase_value": "dark", "upper": 600}))),
            )],
            &m,
        )
        .unwrap();
        let a = &ok["per-animal"];
        assert_eq!(a.source_task, HOURS);
        // The result the table was shown in and the catalog it was chosen from go to the commit.
        assert_eq!(
            (a.artifact.as_str(), a.catalog_sha256.as_str()),
            (SHOWN, "a".repeat(64).as_str())
        );
        assert_eq!(
            (&a.record()["artifact"], &a.record()["catalog_sha256"]),
            (&json!(SHOWN), &json!("a".repeat(64)))
        );
        assert_eq!(a.table_path, "results/query_result.csv");
        assert_eq!(a.title, "Energy per mouse in the dark");
        assert_eq!(a.record()["settings"]["values"], json!(["ee"]));
        assert!(a.sha256().starts_with("sha256:"));
        // Another method's step carries no analysis and passes untouched.
        assert!(
            code(&[action("reference-sql", None)], &m)
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn each_mistake_is_refused_with_the_step_and_what_was_wrong() {
        let m = methods(catalog());
        let s = |settings: Value| Some(choice(settings));
        let ok = json!({"id": "mouse", "values": ["ee"]});
        let with = |key: &str, value: Value| {
            let mut c = choice(ok.clone());
            c[key] = value;
            Some(c)
        };
        for (step, expected) in [
            (
                action("reference-sql", s(ok.clone())),
                "proposal_analysis_unexpected per-animal",
            ),
            (
                action("template-analysis", None),
                "proposal_analysis_missing per-animal",
            ),
            (
                action("template-analysis", with("feature", json!("pca"))),
                "proposal_analysis_feature_unknown per-animal,pca",
            ),
            (
                action("template-analysis", with("source", json!("absent"))),
                "proposal_analysis_source_unknown per-animal,absent",
            ),
            (
                action("template-analysis", with("source", json!("running"))),
                "proposal_analysis_source_not_finished per-animal,running",
            ),
            (
                action("template-analysis", with("source", json!("rejected"))),
                "proposal_analysis_source_rejected per-animal,rejected",
            ),
            (
                action("template-analysis", with("source", json!("in-review"))),
                "proposal_analysis_source_not_finished per-animal,in-review",
            ),
            (
                action(
                    "template-analysis",
                    with("table", json!("results/elsewhere.csv")),
                ),
                "proposal_analysis_table_unknown per-animal,hours,results/elsewhere.csv,available tables: results/query_result.csv, results/wide.csv,available columns: mouse, diet, hour, ee",
            ),
            (
                action(
                    "template-analysis",
                    with("table", json!("results/other.csv")),
                ),
                "proposal_analysis_table_unknown per-animal,hours,results/other.csv,available tables: results/query_result.csv, results/wide.csv,available columns: mouse, diet, hour, ee",
            ),
            (
                action(
                    "template-analysis",
                    with("table", json!("results/wide.csv")),
                ),
                "proposal_analysis_table_unsuitable per-animal,results/wide.csv",
            ),
            (
                action(
                    "template-analysis",
                    s(json!({"id": "mouse", "values": ["ee"], "bins": 3})),
                ),
                "proposal_analysis_setting_unknown per-animal,bins,available settings: from, id, phase_value, upper, values",
            ),
            (
                action(
                    "template-analysis",
                    s(json!({"id": "animal", "values": ["ee"]})),
                ),
                "proposal_analysis_column_unknown per-animal,id,animal,available tables: results/query_result.csv, results/wide.csv,available columns: mouse, diet, hour, ee",
            ),
            (
                action(
                    "template-analysis",
                    s(json!({"id": "mouse", "values": ["ee", "vo2"]})),
                ),
                "proposal_analysis_column_unknown per-animal,values,vo2,available tables: results/query_result.csv, results/wide.csv,available columns: mouse, diet, hour, ee",
            ),
            (
                action("template-analysis", s(json!({"id": "mouse", "values": []}))),
                "proposal_analysis_setting_invalid per-animal,values",
            ),
            (
                action(
                    "template-analysis",
                    s(json!({"id": "mouse", "values": ["ee"], "from": "12"})),
                ),
                "proposal_analysis_setting_invalid per-animal,from",
            ),
            (
                action(
                    "template-analysis",
                    s(json!({"id": "mouse", "values": ["ee"], "upper": 601})),
                ),
                "proposal_analysis_setting_invalid per-animal,upper",
            ),
            (
                action(
                    "template-analysis",
                    s(json!({"id": "mouse", "values": ["ee"], "phase_value": "d".repeat(41)})),
                ),
                "proposal_analysis_setting_invalid per-animal,phase_value",
            ),
            (
                action("template-analysis", s(json!({"values": ["ee"]}))),
                "proposal_analysis_setting_missing per-animal,id",
            ),
        ] {
            assert_eq!(code(&[step], &m).unwrap_err(), expected);
        }
        // A step for the method with code, settings or a query is refused.
        let mut both = action("template-analysis", s(ok.clone()));
        both.sql = Some("SELECT 1".into());
        assert_eq!(
            code(&[both], &m).unwrap_err(),
            "proposal_analysis_unexpected per-animal"
        );
        // Before a worker read a catalog the method can run nothing, nor from a catalog without its digest.
        let mut unnamed = catalog();
        unnamed["catalog_sha256"] = json!("not-a-digest");
        for m in [methods(Value::Null), methods(unnamed)] {
            assert_eq!(
                code(&[action("template-analysis", s(ok.clone()))], &m).unwrap_err(),
                "proposal_analysis_unavailable per-animal"
            );
        }
    }

    #[test]
    fn an_output_path_refusal_tells_the_next_call_which_inputs_were_available() {
        use crate::broker::coordinator::{Refusal, with_prior_attempt};

        let methods = methods(catalog());
        let mut proposed = choice(json!({"id": "mouse", "values": ["ee"]}));
        proposed["table"] = json!("results/results.csv");
        let rejected = HashSet::new();
        let error = admit(
            &[action("template-analysis", Some(proposed))],
            &methods,
            &context(&methods),
            &rejected,
        )
        .unwrap_err();
        let refused = Refusal {
            reason: error.code,
            names: error.names,
            focus: Vec::new(),
        };
        let mut next = json!({});
        with_prior_attempt(&mut next, Some(&refused), 1);
        assert_eq!(
            next["prior_attempt"]["refused"],
            "proposal_analysis_table_unknown"
        );
        assert_eq!(
            next["prior_attempt"]["names"],
            json!([
                "per-animal",
                "hours",
                "results/results.csv",
                "available tables: results/query_result.csv, results/wide.csv",
                "available columns: mouse, diet, hour, ee",
            ])
        );
    }

    #[test]
    fn an_enum_matches_its_choices_without_case() {
        let m = methods(catalog());
        let mut c = choice(json!({"value": "ee", "group": "diet", "test": "Wilcoxon"}));
        c["feature"] = json!("group_compare");
        assert!(code(&[action("template-analysis", Some(c.clone()))], &m).is_ok());
        c["settings"]["test"] = json!("anova");
        assert_eq!(
            code(&[action("template-analysis", Some(c))], &m).unwrap_err(),
            "proposal_analysis_setting_invalid per-animal,test"
        );
    }

    #[test]
    fn settings_are_measured_as_postgresql_writes_jsonb() {
        // Forty settings of 195 characters: 8,161 bytes compact, 8,240 as jsonb text, past the 8,192 the commit allows.
        let long: Map<String, Value> = (0..40)
            .map(|n| (format!("s{n:02}"), json!("x".repeat(195))))
            .collect();
        let long = Value::Object(long);
        assert_eq!(serde_json::to_vec(&long).unwrap().len(), 8161);
        assert_eq!(jsonb_text_bytes(&long), Some(8240));
        // Numbers print without an exponent, a negative zero without its sign; NUL cannot be stored.
        for (value, text) in [
            (json!(1e3), "1000.0"),
            (json!(1.5e-7), "0.00000015"),
            (json!(-0.0), "0.0"),
            (json!(1e300), &format!("1{}", "0".repeat(300))[..]),
            (json!(-12), "-12"),
            (json!(0.001), "0.001"),
            (
                json!({"a": [1, true, null], "b": "q\"\n\u{1}é"}),
                r#"{"a": [1, true, null], "b": "q\"\n\u0001é"}"#,
            ),
        ] {
            assert_eq!(jsonb_text_bytes(&value), Some(text.len()), "{value}");
        }
        assert_eq!(jsonb_text_bytes(&json!({"a": "x\u{0}"})), None);
        let m = methods(catalog());
        let mut c = choice(json!({"value": "ee", "group": "diet", "test": "t"}));
        c["feature"] = json!("group_compare");
        c["settings"]["test"] = json!("t\u{0}");
        assert_eq!(
            code(&[action("template-analysis", Some(c))], &m).unwrap_err(),
            "proposal_analysis_settings_invalid per-animal"
        );
    }

    #[test]
    fn only_an_analysis_shown_with_its_contract_can_run() {
        // 200 analyses: the first ones are shown with their contracts, more are listed by id, the rest left out.
        let many: Vec<Value> = (0..200)
            .map(|n| json!({"id": format!("analysis_{n:03}"), "label": "x".repeat(100),
                "description": "y".repeat(400), "params": {"v": {"type": "column", "required": true}}}))
            .collect();
        let m = methods(json!({"catalog_sha256": "b".repeat(64), "features": many}));
        let shown = context(&m);
        let method = &shown["methods"][1];
        let first_listed = method["other_analyses"][0]["id"]
            .as_str()
            .unwrap()
            .to_owned();
        let shown_n = method["analyses"].as_array().unwrap().len();
        let listed_n = method["other_analyses"].as_array().unwrap().len();
        assert!(shown_n > 0 && listed_n > 0 && shown_n + listed_n < 200);
        let run = |feature: &str| {
            let mut c = choice(json!({"v": "ee"}));
            c["feature"] = json!(feature);
            code(&[action("template-analysis", Some(c))], &m)
        };
        assert!(run("analysis_000").is_ok());
        // Listed by id alone: refused so the next call shows its contract (analysis_view::focus_of).
        assert_eq!(
            run(&first_listed).unwrap_err(),
            format!("proposal_analysis_contract_not_shown per-animal,{first_listed}")
        );
        // Neither shown nor listed, or not in the catalog: unknown.
        assert_eq!(
            run("analysis_199").unwrap_err(),
            "proposal_analysis_feature_unknown per-animal,analysis_199"
        );
        assert_eq!(
            run("pca").unwrap_err(),
            "proposal_analysis_feature_unknown per-animal,pca"
        );
        // The refusal's names put the analysis first in the next call's view, with its contract.
        let focus = analysis_view::focus_of(
            analysis_view::CONTRACT_NOT_SHOWN,
            &["per-animal".to_owned(), first_listed.clone()],
            &[],
        );
        let next = analysis_view::analyses_view(
            &json!({"features": (0..200).map(|n| json!({"id": format!("analysis_{n:03}"), "label": "x".repeat(100),
                "description": "y".repeat(400), "params": {"v": {"type": "column", "required": true}}})).collect::<Vec<_>>()}),
            analysis_view::ANALYSES_VIEW_BYTES,
            &analysis_view::Ranking {
                question: "",
                around: "",
                focus: &focus,
            },
        );
        assert_eq!(next.analyses[0]["id"], first_listed.as_str());
    }
}
