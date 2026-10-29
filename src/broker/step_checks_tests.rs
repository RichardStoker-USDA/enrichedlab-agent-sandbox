use serde_json::json;

use super::*;

fn expect(value: serde_json::Value) -> Expectation {
    serde_json::from_value(value).unwrap()
}

/// A digest of one query result: 10 foods, protein from 0.1 to 31.5, one food without a category.
fn digest() -> Value {
    json!({"schema": "result-digest", "status": "summarized", "tables": [{
        "path": "results/query_result.csv", "rows": 10,
        "columns": ["fdc_id", "description", "protein_g", "category"],
        "missing": {"category": 1},
        "summary": {
            "fdc_id": {"numeric": true, "min": "167512", "max": "2710823", "distinct": 10},
            "description": {"numeric": false, "distinct": 9},
            "protein_g": {"numeric": true, "min": "0.1", "max": "31.5", "distinct": 10},
            "category": {"numeric": false, "distinct": 4}
        }
    }]})
}

#[test]
fn a_result_that_meets_every_stated_property_passes() {
    let e = expect(
        json!({"columns": ["fdc_id", "protein_g"], "rows": {"min": 1, "max": 12},
        "not_null": ["fdc_id", "protein_g"], "unique": ["fdc_id"],
        "ranges": [{"column": "protein_g", "min": 0, "max": 100, "basis": "grams per 100 g cannot exceed 100"}]}),
    );
    assert_eq!(invalid(&e), None);
    let c = check(&e, &digest());
    assert_eq!(c.state, CheckState::Passed, "{:?}", c.reasons);
    assert!(c.reasons.is_empty());
    assert_eq!(c.table.as_deref(), Some("results/query_result.csv"));
}

#[test]
fn each_failed_property_is_named() {
    let e = expect(json!({"columns": ["fdc_id", "kcal"], "rows": {"min": 20},
        "not_null": ["category"], "unique": ["description"],
        "ranges": [{"column": "protein_g", "max": 30, "basis": "the highest value the paper reports"},
                   {"column": "description", "min": 0, "basis": "a count"}]}));
    let c = check(&e, &digest());
    assert_eq!(c.state, CheckState::Failed);
    assert_eq!(
        c.reasons,
        vec![
            "The table has no column kcal.",
            "The table has 10 rows, fewer than the 20 expected.",
            "Column category has no value in 1 rows.",
            "Column description holds 9 distinct values in 10 rows with a value, so some repeat.",
            "Column protein_g goes up to 31.5, above the 30 expected (the highest value the paper reports).",
            "Column description holds values that are not numbers, so its range cannot hold.",
        ]
    );
}

#[test]
fn what_the_digest_cannot_tell_is_not_checkable_and_never_passes() {
    let mut d = digest();
    d["tables"][0]["columns_omitted"] = json!(3);
    d["tables"][0]["summary"]["fdc_id"] =
        json!({"numeric": true, "min": "167512", "max": "2710823", "distinct_capped": true});
    let e = expect(json!({"columns": ["water_g"], "unique": ["fdc_id"]}));
    let c = check(&e, &d);
    assert_eq!(c.state, CheckState::NotCheckable);
    assert_eq!(c.reasons.len(), 2);
    // A failure outranks what could not be read.
    let e = expect(json!({"columns": ["water_g"], "rows": {"max": 5}}));
    assert_eq!(check(&e, &d).state, CheckState::Failed);
}

#[test]
fn the_table_is_named_when_a_result_has_several() {
    let mut d = digest();
    let second = json!({"path": "results/summary.csv", "rows": 2, "columns": ["n"], "summary": {}});
    d["tables"].as_array_mut().unwrap().push(second);
    let any = expect(json!({"rows": {"min": 1}}));
    assert_eq!(check(&any, &d).state, CheckState::NotCheckable);
    let named = expect(json!({"table": "results/summary.csv", "rows": {"min": 1, "max": 2}}));
    assert_eq!(check(&named, &d).state, CheckState::Passed);
    let wrong = expect(json!({"table": "results/other.csv", "rows": {"min": 1}}));
    assert_eq!(check(&wrong, &d).state, CheckState::Failed);
    assert_eq!(
        check(&any, &json!({"status": "no_result_tables", "tables": []})).state,
        CheckState::Failed
    );
}

#[test]
fn a_column_with_no_value_in_any_row_meets_any_range() {
    let mut d = digest();
    d["tables"][0]["missing"]["category"] = json!(10);
    d["tables"][0]["summary"]["category"] = json!({"numeric": false, "distinct": 0});
    let e = expect(json!({"ranges": [{"column": "category", "min": 0, "basis": "a count"}]}));
    assert_eq!(check(&e, &d).state, CheckState::Passed);
}

#[test]
fn an_expectation_must_say_something_checkable() {
    for (value, code) in [
        (json!({}), "expect_empty"),
        (json!({"table": " ", "rows": {"min": 1}}), "expect_table"),
        (json!({"columns": ["a", "a"]}), "expect_columns"),
        (json!({"not_null": [""]}), "expect_not_null"),
        (
            json!({"unique": (0..17).map(|i| format!("c{i}")).collect::<Vec<_>>()}),
            "expect_unique",
        ),
        (json!({"rows": {}}), "expect_rows"),
        (json!({"rows": {"min": 5, "max": 2}}), "expect_rows"),
        (
            json!({"ranges": [{"column": "x", "basis": "b"}]}),
            "expect_ranges",
        ),
        (
            json!({"ranges": [{"column": "x", "min": 2, "max": 1, "basis": "b"}]}),
            "expect_ranges",
        ),
        (
            json!({"ranges": [{"column": "x", "min": 0, "basis": " "}]}),
            "expect_ranges",
        ),
        (
            json!({"ranges": [{"column": "x", "min": 0, "basis": "b".repeat(301)}]}),
            "expect_ranges",
        ),
    ] {
        assert_eq!(invalid(&expect(value.clone())), Some(code), "{value}");
    }
    // Unknown fields are refused when the reply is read.
    assert!(
        serde_json::from_value::<Expectation>(json!({"rows": {"min": 1}, "extra": 1})).is_err()
    );
    assert!(
        serde_json::from_value::<Expectation>(json!({"ranges": [{"column": "x", "min": 0}]}))
            .is_err()
    );
}

#[test]
fn a_result_whose_digest_could_not_be_read_is_not_checkable() {
    let e = expect(json!({"rows": {"min": 1}}));
    for (status, reason) in [
        (
            "too_large",
            "The result could not be checked: it is too large to read.",
        ),
        (
            "integrity_failed",
            "The result could not be checked: its checksums did not match.",
        ),
        (
            "unreadable",
            "The result could not be checked: it could not be read.",
        ),
    ] {
        let c = check(&e, &json!({"status": status}));
        assert_eq!(
            (c.state, c.reasons),
            (CheckState::NotCheckable, vec![reason.to_owned()])
        );
    }
    // A named table the digest left out for room cannot be read either.
    let mut d = digest();
    d["tables_omitted"] = json!(2);
    let named = expect(json!({"table": "results/other.csv", "rows": {"min": 1}}));
    assert_eq!(check(&named, &d).state, CheckState::NotCheckable);
}

#[test]
fn what_the_digest_left_out_for_room_is_not_read() {
    let mut d = digest();
    let t = d["tables"][0].as_object_mut().unwrap();
    t.remove("summary");
    t.insert("summary_omitted".into(), json!("digest_budget"));
    t.remove("missing");
    t.insert("missing_omitted".into(), json!("digest_budget"));
    let e = expect(json!({"not_null": ["fdc_id"], "unique": ["fdc_id"],
        "ranges": [{"column": "protein_g", "max": 30, "basis": "b"}]}));
    let c = check(&e, &d);
    assert_eq!(c.state, CheckState::NotCheckable);
    assert_eq!(
        c.reasons,
        vec![
            "The result's summary does not count the rows without a value in column fdc_id.",
            "The result's summary does not count the distinct values of column fdc_id.",
            "The result's summary does not cover column protein_g.",
        ]
    );
    // Without missing counts, as many distinct values as rows still shows every value differs.
    let mut d = digest();
    d["tables"][0].as_object_mut().unwrap().remove("missing");
    d["tables"][0]["missing_omitted"] = json!("digest_budget");
    assert_eq!(
        check(&expect(json!({"unique": ["fdc_id"]})), &d).state,
        CheckState::Passed
    );
}

#[test]
fn a_lab_table_range_is_not_read_and_its_reasons_hold_no_value() {
    let mut d = digest();
    d["tables"][0]["summary"]["protein_g"] = json!({"numeric": true, "distinct": 10});
    let c = check(
        &expect(json!({"ranges": [{"column": "protein_g", "max": 30, "basis": "b"}]})),
        &d,
    );
    assert_eq!(c.state, CheckState::NotCheckable);
    assert_eq!(
        c.reasons,
        vec!["The result's summary does not give the range of column protein_g."]
    );
}

#[test]
fn each_step_is_checked_through_its_results_and_the_lead_reads_the_worst() {
    use crate::broker::project::{PlanStep, StepStatus};
    let (a, b) = (
        "00000000-0000-4000-8000-00000000000a",
        "00000000-0000-4000-8000-00000000000b",
    );
    let step = |key: &str, task: &str, e: Option<Expectation>| PlanStep {
        action_key: key.into(),
        title: key.into(),
        status: StepStatus::Completed,
        task_id: Some(task.parse().unwrap()),
        evidence_refs: vec![format!("artifact://tasks/{task}/r")],
        rationale: "r".into(),
        expect: e,
    };
    let plan = vec![
        step("first", a, Some(expect(json!({"rows": {"max": 5}})))),
        step("second", b, None),
    ];
    let results = json!([
        {"task_id": a, "ref": "artifact://tasks/a/1", "digest": digest()},
        {"task_id": a, "ref": "artifact://tasks/a/2", "digest": {"status": "summarized", "tables": [{"path": "x.csv", "rows": 2, "columns": ["n"]}]}},
        {"task_id": b, "ref": "artifact://tasks/b/1", "digest": digest()},
    ]);
    let checks = json!(checks_for(&plan, &results));
    assert_eq!(checks.as_array().unwrap().len(), 2);
    assert_eq!(
        (checks[0]["key"].as_str(), checks[0]["state"].as_str()),
        (Some("first"), Some("failed"))
    );
    assert_eq!(checks[1]["state"], "passed");
    let shown = ShownChecks::of(&checks);
    assert!(shown.failed("artifact://tasks/a/1") && !shown.failed("artifact://tasks/a/2"));
    assert_eq!(shown.analysis(a).unwrap()["state"], "failed");
    assert!(shown.analysis(b).is_none());
    assert_eq!(
        counts(&checks),
        json!({"passed": 1, "failed": 1, "not_checkable": 0})
    );
    let mut view = json!({"tables": [{"table": "x.csv", "cells": [{"#": 1}], "rows_shown": 50}]});
    withhold_failed(&mut view);
    assert_eq!(
        view["tables"][0],
        json!({"table": "x.csv", "cells_omitted": CHECK_FAILED})
    );
    let own = json!([{"task_id": a, "sql_sha256": "x"}, {"task_id": b, "sql_sha256": "y"}]);
    assert_eq!(
        without_failed(&own, &checks),
        json!([{"task_id": b, "sql_sha256": "y"}])
    );
}

#[test]
fn a_table_the_worker_did_not_count_proves_no_value_is_missing() {
    // Repeated column names, or a row the worker could not read, leave a table without counts or summary and without
    // a marker saying so (`digest::table`); `not_null` once passed on such a table.
    let duplicate = json!({"status": "summarized", "tables": [{"path": "results/t.csv", "rows": 3,
        "columns": ["a", "a"], "cells": [["1", ""], ["2", "3"], ["", "4"]]}]});
    let e = expect(json!({"not_null": ["a"], "unique": ["a"]}));
    let c = check(&e, &duplicate);
    assert_eq!(c.state, CheckState::NotCheckable);
    assert_eq!(
        c.reasons,
        vec![
            "The result's summary does not count the rows without a value in column a.",
            "The result's summary does not count the distinct values of column a.",
        ]
    );
    let unreadable = json!({"status": "summarized", "tables": [{"path": "results/t.csv", "columns": ["a"],
        "cells_omitted": "unreadable"}]});
    let c = check(
        &expect(json!({"rows": {"min": 1}, "not_null": ["a"]})),
        &unreadable,
    );
    assert_eq!(c.state, CheckState::NotCheckable);
    assert_eq!(c.reasons.len(), 2);
    // A summary left out for room still means the worker counted.
    let mut room = digest();
    room["tables"][0].as_object_mut().unwrap().remove("summary");
    room["tables"][0]["summary_omitted"] = json!("digest_budget");
    assert_eq!(
        check(&expect(json!({"not_null": ["category"]})), &room).state,
        CheckState::Failed
    );
}

#[test]
fn a_column_name_longer_than_the_digest_keeps_cannot_be_matched() {
    let long = "n".repeat(70);
    let shortened = format!("{}…", "n".repeat(64));
    let d = json!({"status": "summarized", "tables": [{"path": "results/t.csv", "rows": 2,
        "columns": [shortened.clone()], "summary": {shortened: {"numeric": true, "min": "1", "max": "2", "distinct": 2}}}]});
    let c = check(
        &expect(json!({"columns": [long.clone()], "unique": [long.clone()],
            "ranges": [{"column": long.clone(), "max": 1, "basis": "b"}]})),
        &d,
    );
    assert_eq!(c.state, CheckState::NotCheckable);
    assert_eq!(c.reasons.len(), 3);
    assert!(
        c.reasons[0].ends_with(
            "has a longer name than the result's summary keeps, so it cannot be matched."
        )
    );
    // A name of exactly the kept length still matches.
    let exact = "m".repeat(64);
    let d = json!({"status": "summarized", "tables": [{"path": "results/t.csv", "rows": 2, "columns": [exact.clone()],
        "summary": {exact.clone(): {"numeric": true, "min": "1", "max": "2", "distinct": 2}}}]});
    assert_eq!(
        check(&expect(json!({"unique": [exact]})), &d).state,
        CheckState::Passed
    );
}

#[test]
fn a_turn_record_keeps_within_its_stored_bound() {
    // The checks give way before a record outgrows what PostgreSQL stores, and a record still over
    // says by how much, which the coordinator refuses as too large instead of failing the write.
    let reason = "r".repeat(250);
    let checks: Vec<Value> = (0..40)
        .map(|i| json!({"key": format!("s{i}"), "task_id": format!("t{i}"), "ref": format!("artifact://tasks/t{i}/a"),
            "state": "failed", "reasons": vec![reason.clone(); 6]}))
        .collect();
    let mut record = json!({"proposal": {"summary": "s".repeat(120 * 1024)},
        "step_checks": {"checks": checks, "omitted": 0}});
    // What the commit adds before it stores the record counts too.
    let added = json!({"submitted_tasks": (0..16).map(|i| json!({"task_id": uuid::Uuid::nil(),
        "action_key": format!("{i}").repeat(100)})).collect::<Vec<_>>(), "actor_id": uuid::Uuid::nil(),
        "outcome": "review_required"});
    assert!(crate::broker::digest::stored_bytes(&record) > TURN_RECORD_BYTES);
    assert_eq!(fit_record(&mut record, &added), None);
    let mut stored = record.clone();
    stored
        .as_object_mut()
        .unwrap()
        .extend(added.as_object().unwrap().clone());
    assert!(crate::broker::digest::stored_bytes(&stored) <= TURN_RECORD_BYTES);
    let kept = record["step_checks"]["checks"].as_array().unwrap().len();
    assert!(kept < 40);
    assert_eq!(
        record["step_checks"]["omitted"].as_u64().unwrap() as usize,
        40 - kept
    );
    let mut over = json!({"proposal": {"summary": "s".repeat(130 * 1024)}});
    assert!(fit_record(&mut over, &json!({})).is_some_and(|n| n > 0));
    // A record that fits alone but not with what the commit adds is over.
    let mut edge = json!({"proposal": {"summary": "s".repeat(128 * 1024 - 64)}});
    assert_eq!(fit_record(&mut edge, &json!({})), None);
    assert!(fit_record(&mut edge, &added).is_some());
}

#[test]
fn a_plan_past_its_bound_says_by_how_much() {
    use crate::broker::project::{ResearchState, STATE_BYTES_MAX};
    let mut state: ResearchState = serde_json::from_value(json!({"schema_version":1,"goal":"g","constraints":[],"plan":[],
        "lessons":[],"next_action":"n","workspace":{"schema_version":1,"profile":"p",
        "image_digest":format!("sha256:{}","a".repeat(64)),"source_digest":format!("sha256:{}","b".repeat(64)),
        "input_refs":[],"tool_refs":[],"preparation_receipt":null}}))
    .unwrap();
    assert_eq!(state.bytes_over(), None);
    let big = expect(
        json!({"ranges": (0..12).map(|i| json!({"column": format!("c{i}"), "min": 0,
        "basis": "b".repeat(100)})).collect::<Vec<_>>()}),
    );
    for i in 0..40 {
        state.plan.push(serde_json::from_value(json!({"action_key": format!("k{i}"), "title": "t".repeat(500),
            "status": "pending", "task_id": null, "evidence_refs": [], "rationale": "r".repeat(2000)})).unwrap());
        state.plan[i].expect = Some(big.clone());
    }
    let over = state.bytes_over().unwrap();
    assert_eq!(
        serde_json::to_vec(&state).unwrap().len(),
        STATE_BYTES_MAX + over
    );
    assert_eq!(state.validate(), Err("checkpoint_invalid"));
}

#[test]
fn a_plan_near_its_bound_keeps_room_for_commit_and_reconciliation() {
    // The commit gives a delegated step its task and makes it active, and reconciliation later adds
    // its results' references; a plan admitted with room for both still reconciles within the bound.
    use crate::broker::coordinator::reconcile;
    use crate::broker::project::{
        ResearchState, STATE_BYTES_MAX, StepStatus, UNFINISHED_STEP_GROWTH,
    };
    let mut state: ResearchState = serde_json::from_value(json!({"schema_version":1,"goal":"g","constraints":[],
        "plan":[],"lessons":[],"next_action":"n","workspace":{"schema_version":1,"profile":"p",
        "image_digest":format!("sha256:{}","a".repeat(64)),"source_digest":format!("sha256:{}","b".repeat(64)),
        "input_refs":[],"tool_refs":[],"preparation_receipt":null}}))
    .unwrap();
    let step = |key: &str, status: &str, task: Option<uuid::Uuid>, refs: Vec<String>| {
        serde_json::from_value(
            json!({"action_key": key, "title": "t".repeat(400), "status": status, "task_id": task,
            "rationale": "r".repeat(400), "evidence_refs": refs, "expect": {"rows": {"min": 1}}}),
        )
        .unwrap()
    };
    let new_ref = |task: uuid::Uuid| format!("artifact://tasks/{task}/{}", uuid::Uuid::new_v4());
    let mut i = 0;
    while serde_json::to_vec(&state).unwrap().len() < STATE_BYTES_MAX - 3 * 1024 {
        let task = uuid::Uuid::new_v4();
        state.plan.push(step(
            &format!("done-{i}"),
            "completed",
            Some(task),
            vec![new_ref(task)],
        ));
        i += 1;
    }
    state.plan.push(step("next", "pending", None, vec![]));
    // Measured as the commit writes it: the delegated step with its task and active.
    let mut committed = state.clone();
    let last = committed.plan.last_mut().unwrap();
    last.task_id = Some(uuid::Uuid::nil());
    last.status = StepStatus::Active;
    assert_eq!(committed.bytes_over_after_growth(), None);
    // Pushed to the admitted edge, each rationale within its 2,048 characters: one more byte would be refused.
    let mut at = 0;
    loop {
        while committed.plan[at].rationale.len() >= 2048 {
            at += 1;
        }
        let mut next = committed.clone();
        next.plan[at].rationale.push('r');
        if next.bytes_over_after_growth().is_some() {
            break;
        }
        committed = next;
        state.plan[at].rationale.push('r');
    }
    // Then committed with a real task and reconciled with the claim's most results: still within the bound.
    let task = uuid::Uuid::new_v4();
    let last = state.plan.last_mut().unwrap();
    last.task_id = Some(task);
    last.status = StepStatus::Active;
    let mut facts: Vec<Value> = state.plan[..state.plan.len() - 1]
        .iter()
        .map(|s| json!({"task_id": s.task_id, "status": "completed", "result_class": "success", "error_code": null,
            "evidence_refs": s.evidence_refs}))
        .collect();
    facts.push(json!({"task_id": task, "status": "completed", "result_class": "success", "error_code": null,
        "evidence_refs": (0..64).map(|_| new_ref(task)).collect::<Vec<_>>()}));
    let facts = json!(facts);
    reconcile(&mut state, &facts).unwrap();
    // The claim's 64 references become at most `STEP_REFS_MAX` on the step, and the checkpoint stays within its bound.
    assert_eq!(
        state.plan.last().unwrap().evidence_refs.len(),
        crate::broker::project::STEP_REFS_MAX
    );
    assert!(state.bytes_over().is_none());
    // A plan within `UNFINISHED_STEP_GROWTH` of the bound with an unfinished step is not admitted.
    let mut tight = committed.clone();
    while tight.bytes_over_after_growth().is_none() {
        tight.plan[0].rationale.push_str(&"r".repeat(16));
    }
    let n = serde_json::to_vec(&tight).unwrap().len();
    assert!(n + UNFINISHED_STEP_GROWTH > STATE_BYTES_MAX && n <= STATE_BYTES_MAX);
}

#[test]
fn an_unfinished_step_grows_by_at_most_its_reserve() {
    // The reserve is the most a step can gain from its task's facts, measured on the real types: active with no
    // references, then finished with `STEP_REFS_MAX` references and the longest status.
    use crate::broker::project::{
        PlanStep, STEP_REF_BYTES, STEP_REFS_MAX, StepStatus, UNFINISHED_STEP_GROWTH,
    };
    let mut step: PlanStep =
        serde_json::from_value(json!({"action_key": "k", "title": "t", "status": "active",
        "task_id": uuid::Uuid::new_v4(), "evidence_refs": [], "rationale": "r"}))
        .unwrap();
    let before = serde_json::to_vec(&step).unwrap().len();
    step.evidence_refs = (0..STEP_REFS_MAX)
        .map(|_| {
            format!(
                "artifact://tasks/{}/{}",
                uuid::Uuid::new_v4(),
                uuid::Uuid::new_v4()
            )
        })
        .collect();
    for status in [StepStatus::Completed, StepStatus::Uncertain] {
        step.status = status;
        let grown = serde_json::to_vec(&step).unwrap().len() - before;
        assert!(grown <= UNFINISHED_STEP_GROWTH, "{grown}");
        assert!(
            grown + STEP_REF_BYTES > UNFINISHED_STEP_GROWTH,
            "the reserve is the worst case, not a guess"
        );
    }
}

#[test]
fn a_bound_written_as_a_number_in_text_reads_as_that_number_and_stays_a_number() {
    // A test run: a lead wrote a range's bounds as text and the whole reply was refused.
    let e = expect(json!({"rows": {"min": "1", "max": 50}, "ranges": [
        {"column": "value", "min": "0", "max": " 1e3 ", "basis": "a physical limit"}]}));
    assert_eq!(
        (e.ranges[0].min, e.ranges[0].max),
        (Some(0.0), Some(1000.0))
    );
    assert_eq!(
        (e.rows.as_ref().unwrap().min, e.rows.as_ref().unwrap().max),
        (Some(1), Some(50))
    );
    let stored = serde_json::to_value(&e).unwrap();
    assert_eq!(
        stored["ranges"][0]["max"],
        json!(1000.0),
        "stored as a number, so the checkpoint format is unchanged"
    );
    assert_eq!(stored["rows"]["min"], json!(1));
    for bad in [
        json!("about 5"),
        json!(""),
        json!("NaN"),
        json!("inf"),
        json!([1]),
        json!(true),
    ] {
        let value = json!({"ranges": [{"column": "value", "max": bad, "basis": "b"}]});
        assert!(
            serde_json::from_value::<Expectation>(value.clone()).is_err(),
            "{value}"
        );
    }
    assert!(serde_json::from_value::<Expectation>(json!({"rows": {"min": "1.5"}})).is_err());
    assert!(serde_json::from_value::<Expectation>(json!({"rows": {"min": null}})).is_ok());
}

#[test]
fn failed_results_survive_a_view_trimmed_for_room() {
    // The supervisor's failures must not depend on which checks the record's view kept.
    let reasons: Vec<String> = (0..6).map(|n| format!("{n} {}", "x".repeat(400))).collect();
    let checks = json!((0..40)
        .map(|i| json!({"key": format!("s{i}"), "task_id": format!("t{i}"), "ref": format!("artifact://tasks/t{i}/r"),
            "state": if i % 2 == 0 { "failed" } else { "passed" }, "reasons": reasons}))
        .collect::<Vec<_>>());
    let view = record_view(&checks);
    assert!(
        view["omitted"].as_u64().unwrap() > 0,
        "the view leaves checks out"
    );
    let own: std::collections::BTreeSet<String> = (0..40).map(|i| format!("t{i}")).collect();
    let failed = failed_results(&checks, &own);
    assert_eq!(failed.as_array().unwrap().len(), FAILED_RESULTS_KEPT);
    assert_eq!(
        failed[0],
        json!({"task_id": "t0", "ref": "artifact://tasks/t0/r"})
    );
    let mut record = json!({"step_checks": view, "failed_results": failed.clone()});
    assert!(fit_record(&mut record, &json!({"filler": "y".repeat(120 * 1024)})).is_none());
    assert_eq!(
        record["failed_results"], failed,
        "trimming for room drops checks, never the failures"
    );
}

#[test]
fn an_earlier_runs_failures_never_take_this_runs_places() {
    // Sixteen failures of earlier runs' tasks come first in the plan; this run's three still
    // fit, and nothing of the earlier runs is kept.
    let mut checks: Vec<Value> = (0..16)
        .map(|i| json!({"task_id": format!("old{i}"), "ref": format!("artifact://tasks/old{i}/r"), "state": "failed"}))
        .collect();
    checks.extend((0..3).map(|i| json!({"task_id": format!("t{i}"), "ref": format!("artifact://tasks/t{i}/r"), "state": "failed"})));
    let own: std::collections::BTreeSet<String> = (0..3).map(|i| format!("t{i}")).collect();
    let failed = failed_results(&json!(checks), &own);
    assert_eq!(
        failed,
        json!((0..3).map(|i| json!({"task_id": format!("t{i}"), "ref": format!("artifact://tasks/t{i}/r")})).collect::<Vec<_>>())
    );
}
