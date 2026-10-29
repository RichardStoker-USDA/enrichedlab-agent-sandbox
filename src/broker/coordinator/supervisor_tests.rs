use super::*;

fn refused(reasons: &[&str]) -> Value {
    json!({"events": reasons.iter().enumerate().map(|(n, r)| json!({"sequence": n + 1, "code": "proposal_invalid",
        "reason": r, "turn": 1})).collect::<Vec<_>>(), "turns": []})
}

#[test]
fn a_reason_that_keeps_returning_narrows_then_pauses() {
    // Not in a row: successes between them reset the failure count, which the yield's own pause reads.
    let three = counts(
        &refused(&["proposal_action_invalid"; 3]),
        &json!([]),
        &json!([]),
    );
    assert_eq!(three.repeated, Some(("proposal_action_invalid".into(), 3)));
    assert_eq!(
        decide(&three),
        Some(Action::Narrow(Reason::RepeatedRefusal, Redirect::OneStep))
    );
    let four = counts(
        &refused(&["proposal_action_invalid"; 4]),
        &json!([]),
        &json!([]),
    );
    assert_eq!(decide(&four), Some(Action::Pause(Reason::RepeatedRefusal)));
}

#[test]
fn a_reason_no_narrowing_repairs_waits_for_the_pause() {
    let three = counts(
        &refused(&["proposal_prose_number"; 3]),
        &json!([]),
        &json!([]),
    );
    assert_eq!(decide(&three), None);
    assert_eq!(
        decide(&counts(
            &refused(&["proposal_prose_number"; 4]),
            &json!([]),
            &json!([])
        )),
        Some(Action::Pause(Reason::RepeatedRefusal))
    );
}

#[test]
fn refusals_in_all_pause_and_service_faults_do_not_count() {
    let mixed = refused(&[
        "proposal_prose_number",
        "proposal_schema_invalid",
        "proposal_table_unbound",
        "proposal_prose_number",
        "proposal_schema_invalid",
        "proposal_table_unbound",
    ]);
    assert_eq!(
        decide(&counts(&mixed, &json!([]), &json!([]))),
        Some(Action::Pause(Reason::RefusalsTotal))
    );
    let outages = json!({"events": (0..9).map(|n| json!({"sequence": n, "code": "model_unavailable",
        "reason": "model_unavailable", "turn": 1})).collect::<Vec<_>>(), "turns": []});
    assert_eq!(counts(&outages, &json!([]), &json!([])), Counts::default());
}

fn failed(task: &str, n: usize) -> Value {
    json!({"task_id": task, "ref": format!("artifact://tasks/{task}/{n}"), "state": "failed"})
}

#[test]
fn failed_checks_narrow_to_one_step_then_pause() {
    let checks = |n: usize| {
        json!(
            (0..n)
                .map(|i| failed(&format!("t{i}"), 0))
                .collect::<Vec<_>>()
        )
    };
    // The run's own turns submitted the tasks t0 to t4.
    let own = json!({"events": [], "turns": (0..5).map(|i| json!({"turn": i + 1, "outcome": "delegate",
        "tasks": [format!("t{i}")]})).collect::<Vec<_>>()});
    assert_eq!(
        decide(&counts(&own, &checks(3), &json!([]))),
        Some(Action::Narrow(Reason::FailedChecks, Redirect::OneStep))
    );
    assert_eq!(
        decide(&counts(&own, &checks(5), &json!([]))),
        Some(Action::Pause(Reason::FailedChecks))
    );
    assert_eq!(decide(&counts(&own, &checks(2), &json!([]))), None);
}

#[test]
fn each_failed_result_counts_even_from_one_task() {
    // The bounds count results, so three tables of one task that fail count three.
    let own = json!({"events": [], "turns": [{"turn": 1, "outcome": "delegate", "tasks": ["t0"]}]});
    let checks = json!([failed("t0", 1), failed("t0", 2), failed("t0", 3)]);
    assert_eq!(counts(&own, &checks, &json!([])).failed_checks, 3);
}

#[test]
fn an_earlier_runs_failures_never_weigh_on_a_new_run() {
    // The project's plan holds an earlier run's five failed results; this run submitted none.
    let checks = json!(
        (0..5)
            .map(|i| failed(&format!("old{i}"), 0))
            .collect::<Vec<_>>()
    );
    let fresh = json!({"events": [], "turns": []});
    assert_eq!(counts(&fresh, &checks, &json!([])).failed_checks, 0);
    assert_eq!(decide(&counts(&fresh, &checks, &json!([]))), None);
}

#[test]
fn a_failure_stays_counted_after_its_step_is_archived() {
    // Four failures were recorded by earlier turns and their steps archived; a fifth now fails: the bound is reached.
    let turns: Vec<Value> = (0..5)
        .map(|i| {
            let task = format!("t{i}");
            let recorded = if i < 4 {
                json!([failed(&task, 0)])
            } else {
                json!([])
            };
            json!({"turn": i + 1, "outcome": "delegate", "tasks": [task], "failed": recorded})
        })
        .collect();
    let faults = json!({"events": [], "turns": turns});
    let now = json!([failed("t4", 0), failed("t3", 0)]);
    let c = counts(&faults, &now, &json!([]));
    assert_eq!(c.failed_checks, 5, "t3's result read twice counts once");
    assert_eq!(decide(&c), Some(Action::Pause(Reason::FailedChecks)));
}

#[test]
fn only_the_leads_own_refusals_count() {
    // Service, catalog, specialist and context faults share the event code but are not
    // the lead's reply; code and query admission refusals are.
    let faults = refused(&[
        "model_envelope_invalid",
        "model_content_missing",
        "configured_method_invalid",
        "configured_block_rejected",
        "proposal_analysis_unavailable",
        "method_review_invalid",
        "context_budget_too_small",
        "coordinator_internal",
        "sql_refused_unordered",
        "code_embedded_data",
        "proposal_settings_invalid",
        "read_rounds_spent",
    ]);
    let c = counts(&faults, &json!([]), &json!([]));
    assert_eq!(c.refusals, 4);
    for reason in [
        "code_literal_data",
        "sql_refused_unordered",
        "proposal_shape_invalid",
        "read_rounds_spent",
    ] {
        assert!(reply_refusal(reason), "{reason}");
    }
    for reason in [
        "model_unavailable",
        "configured_method_invalid",
        "proposal_analysis_unavailable",
        "method_review_invalid",
    ] {
        assert!(!reply_refusal(reason), "{reason}");
    }
}

#[test]
fn turns_that_add_nothing_usable_narrow_then_pause() {
    let turn = |n: i64, task: &str| json!({"turn": n, "outcome": "delegate", "tasks": [task]});
    let faults = |turns: Vec<Value>| json!({"events": [], "turns": turns});
    let facts = json!([{"task_id": "a", "status": "failed"}, {"task_id": "b", "status": "completed"},
        {"task_id": "c", "status": "completed"}, {"task_id": "d", "status": "completed"},
        {"task_id": "e", "status": "running"}]);
    // b and c completed with failed checks; d with a check its summary could not decide, which counts as usable.
    let checks = json!([{"task_id": "b", "state": "failed"}, {"task_id": "c", "state": "failed"},
        {"task_id": "d", "state": "not_checkable"}]);
    let two = counts(
        &faults(vec![turn(1, "d"), turn(2, "a"), turn(3, "b")]),
        &checks,
        &facts,
    );
    assert_eq!(two.idle_turns, 2);
    assert_eq!(
        decide(&Counts {
            failed_checks: 0,
            ..two
        }),
        Some(Action::Narrow(Reason::NoProgress, Redirect::OneStep))
    );
    let three = counts(
        &faults(vec![turn(1, "a"), turn(2, "b"), turn(3, "c")]),
        &checks,
        &facts,
    );
    assert_eq!(three.idle_turns, 3);
    assert_eq!(
        decide(&Counts {
            failed_checks: 0,
            ..three
        }),
        Some(Action::Pause(Reason::NoProgress))
    );
    // A turn whose task is still running ends the count.
    let running = counts(
        &faults(vec![turn(1, "a"), turn(2, "b"), turn(3, "e")]),
        &checks,
        &facts,
    );
    assert_eq!(running.idle_turns, 0);
}

#[test]
fn a_refusals_own_redirect_is_kept() {
    let mut context = json!({"redirect": {"kind": "shape", "after": "proposal_schema_invalid"}});
    assert!(!narrow(
        &mut context,
        Reason::FailedChecks,
        Redirect::OneStep
    ));
    assert_eq!(context["redirect"]["kind"], "shape");
    let mut fresh = json!({});
    assert!(narrow(&mut fresh, Reason::FailedChecks, Redirect::OneStep));
    assert_eq!(
        fresh["redirect"],
        json!({"kind": "one_step", "after": "supervisor_failed_checks"})
    );
}
