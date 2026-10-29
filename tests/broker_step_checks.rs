//! Step checks: every step the lead proposes states what its result must look like, code checks
//! each stored result against it before the lead's next call, and a result that fails its check supports nothing.
use enrichedlab_agent_sandbox::broker::{
    coordinator::{
        LeadCheck, Proposal, check_lead_reply, compile_context, proposal_schema_names, reconcile,
        refusal, rejection_reason, repeated_queries,
    },
    project::ResearchState,
    step_checks::{self, CheckState},
};
use serde_json::{Value, json};
use std::collections::{HashMap, HashSet};
use uuid::Uuid;

fn research() -> ResearchState {
    serde_json::from_value(json!({"schema_version":1,"goal":"Which foods have the most protein per 100 grams?",
        "constraints":[],"plan":[],"lessons":[],"next_action":"Rank the foods.",
        "workspace":{"schema_version":1,"profile":"reference-sql","image_digest":format!("sha256:{}","a".repeat(64)),
            "source_digest":format!("sha256:{}","b".repeat(64)),"input_refs":["input://public/foods"],
            "tool_refs":["tool://reviewed/query"],"preparation_receipt":null}}))
    .unwrap()
}

/// One finished step that states `expect`, with a stored result of three foods whose protein reaches 131.5 grams per
/// 100 grams, more than any food can hold.
fn finished(expect: Value) -> (ResearchState, Value, Value, String) {
    let mut state = research();
    let task = Uuid::new_v4();
    state.plan.push(
        serde_json::from_value(json!({"action_key":"rank-protein","title":"Rank foods by protein","status":"pending",
            "task_id":task,"evidence_refs":[],"rationale":"The question asks for the foods with the most protein.",
            "expect":expect}))
        .unwrap(),
    );
    let reference = format!("artifact://tasks/{task}/{}", Uuid::new_v4());
    let facts = json!([{"task_id":task,"status":"completed","result_class":"success","error_code":null,
        "evidence_refs":[reference]}]);
    reconcile(&mut state, &facts).unwrap();
    let digest = json!({"schema":"enrichedlab.result-digest.v1","status":"summarized",
        "package":{"members":5,"checksums_listed":true,"all_matched":true},
        "tables":[{"path":"results/query_result.csv","rows":3,"columns":["fdc_id","description","protein_g"],
            "summary":{"fdc_id":{"numeric":true,"min":"1001","max":"1003","distinct":3},
                "description":{"numeric":false,"distinct":3},
                "protein_g":{"numeric":true,"min":"10.5","max":"131.5","distinct":3}},
            "cells":[["1001","Lentils","10.5"],["1002","Tofu","17.3"],["1003","Seitan","131.5"]]}],
        "tables_omitted":0,"figures":[]});
    let mut results = json!({"results":[{"task_id":task,"ref":reference,"label":"Query result","status":"summarized",
        "digest_sha256":"d".repeat(64),"digest":digest}],"earlier_findings":null});
    results["step_checks"] = json!(step_checks::checks_for(&state.plan, &results["results"]));
    (state, facts, results, reference)
}

fn protein_range(max: f64) -> Value {
    json!({"columns":["description","protein_g"],"rows":{"min":1,"max":12},"unique":["fdc_id"],
        "ranges":[{"column":"protein_g","min":0,"max":max,"basis":"grams in 100 grams of food"}]})
}

fn compile(
    state: &ResearchState,
    facts: &Value,
    results: &Value,
) -> enrichedlab_agent_sandbox::broker::coordinator::CompiledContext {
    compile_context(
        &state.context(3, 8192).unwrap(),
        &HashMap::new(),
        &json!([]),
        facts,
        results,
        1,
        32768,
    )
}

#[test]
fn a_result_that_breaks_its_expectation_reaches_the_lead_without_cells() {
    let (state, facts, results, reference) = finished(protein_range(100.0));
    assert_eq!(results["step_checks"][0]["state"], "failed");
    assert_eq!(
        results["step_checks"][0]["reasons"],
        json!([
            "Column protein_g goes up to 131.5, above the 100 expected (grams in 100 grams of food)."
        ])
    );
    let compiled = compile(&state, &facts, &results);
    let analysis = &compiled.value["analyses"][0];
    assert_eq!(analysis["check"]["state"], "failed");
    assert_eq!(
        analysis["check"]["reasons"],
        results["step_checks"][0]["reasons"]
    );
    let table = &analysis["results"][0]["tables"][0];
    assert!(table.get("cells").is_none());
    assert_eq!(table["cells_omitted"], "check_failed");
    assert_eq!(table["rows"], 3);
    assert_eq!(
        compiled.inputs["step_checks"],
        json!({"passed":0,"failed":1,"not_checkable":0})
    );
    // No finding binds to it, and no answer table gives it.
    let p: Proposal = serde_json::from_value(json!({"action":"review_required","answer":"answered","summary":"Ready.",
        "findings":[{"statement":"Seitan has {p}.","bindings":{"p":{"ref":reference,"table":"results/query_result.csv",
            "row":3,"column":"protein_g"}}}],"actions":[],"lessons":[]}))
    .unwrap();
    let error = p
        .validate_findings(&compiled.index, &compiled.rejected)
        .unwrap_err();
    assert_eq!(error.to_string(), "proposal_finding_unbound");
    assert!(
        refusal(&error)
            .names
            .iter()
            .any(|n| n.contains("failed its step's check")),
        "{:?}",
        refusal(&error).names
    );
    let mut p: Proposal = serde_json::from_value(json!({"action":"review_required","answer":"answered","summary":"Ready.",
        "tables":[{"ref":reference,"table":"results/query_result.csv","columns":["description","protein_g"],
            "lead_in":"Foods by protein"}],"actions":[],"lessons":[]}))
    .unwrap();
    let error = p
        .check_tables(&compiled.index, &compiled.rejected)
        .unwrap_err();
    assert_eq!(error.to_string(), "proposal_table_unbound");
    assert_eq!(refusal(&error).names, ["table 1: check failed"]);
    // The turn's record keeps the check as the lead read it.
    let record = step_checks::record_view(&results["step_checks"]);
    assert_eq!(record["checks"][0]["key"], "rank-protein");
    assert_eq!(record["checks"][0]["ref"], json!(reference));
    assert_eq!(record["omitted"], 0);
}

#[test]
fn a_result_that_meets_its_expectation_keeps_its_cells() {
    let (state, facts, results, _) = finished(protein_range(200.0));
    assert_eq!(results["step_checks"][0]["state"], "passed");
    let compiled = compile(&state, &facts, &results);
    let analysis = &compiled.value["analyses"][0];
    assert_eq!(analysis["check"], json!({"state":"passed"}));
    assert_eq!(
        analysis["results"][0]["tables"][0]["cells"]
            .as_array()
            .unwrap()
            .len(),
        3
    );
    // A step planned before step checks states nothing, has no check and reads as before.
    let (mut state, facts, mut results, _) = finished(protein_range(200.0));
    state.plan[0].expect = None;
    results["step_checks"] = json!(step_checks::checks_for(&state.plan, &results["results"]));
    assert_eq!(results["step_checks"], json!([]));
    let compiled = compile(&state, &facts, &results);
    assert!(compiled.value["analyses"][0].get("check").is_none());
    assert!(compiled.inputs.get("step_checks").is_none());
}

#[test]
fn a_query_whose_result_failed_its_check_may_run_again() {
    let (state, _, results, _) = finished(protein_range(100.0));
    let task = state.plan[0].task_id.unwrap();
    let sql = "SELECT f.fdc_id, f.description, n.amount AS protein_g FROM food f JOIN food_nutrient n ON n.fdc_id = f.fdc_id ORDER BY 3 DESC";
    let sha = {
        use sha2::{Digest, Sha256};
        format!("sha256:{}", hex::encode(Sha256::digest(sql.as_bytes())))
    };
    let own =
        json!([{"task_id": task, "admitted_sql": sql, "sql_sha256": sha, "status": "completed"}]);
    assert_eq!(
        repeated_queries([("again", sql)], &own),
        vec!["again".to_string()]
    );
    let repeatable = step_checks::without_failed(&own, &results["step_checks"]);
    assert!(repeated_queries([("again", sql)], &repeatable).is_empty());
}

#[test]
fn every_step_states_what_its_result_must_look_like() {
    let state = research();
    let methods = json!([{"key": "reference-r-mean", "name": "Mean bootstrap", "description": "Bootstrap intervals."}]);
    let results = json!({"results": [], "earlier_findings": null});
    let compiled = compile_context(
        &state.context(1, 8192).unwrap(),
        &HashMap::new(),
        &methods,
        &json!([]),
        &results,
        2,
        16384,
    );
    let names = vec!["reference-r-mean".to_owned()];
    let (sources, own, archived) = (HashMap::new(), json!([]), HashSet::new());
    let check = |reply: &Value| {
        check_lead_reply(
            &LeadCheck {
                state: &state,
                profiles: &names,
                parallel: 2,
                methods: &methods,
                sources: &sources,
                own_queries: &own,
                context: &compiled.value,
                index: &compiled.index,
                rejected: &compiled.rejected,
                archived: &archived,
                input_ceiling: usize::MAX,
            },
            reply,
        )
    };
    let reply = |expect: Option<Value>| {
        let mut step = json!({"action_key": "bootstrap", "title": "Bootstrap the means", "profile": "reference-r-mean",
            "objective": "Estimate the interval of each mean.", "rationale": "The question asks for intervals."});
        if let Some(e) = expect {
            step["expect"] = e;
        }
        json!({"action": "delegate", "answer": "not_yet", "summary": "Bootstrap the means next.", "actions": [step],
            "lessons": []})
    };
    let missing = check(&reply(None)).err().unwrap();
    assert_eq!(refusal(&missing).reason, "proposal_step_expect_missing");
    assert_eq!(refusal(&missing).names, ["bootstrap"]);
    for (expect, part) in [
        (json!({}), "nothing to check"),
        (json!({"rows": {"min": 5, "max": 2}}), "rows"),
        (json!({"unique": ["mean", "mean"]}), "unique"),
        (
            json!({"ranges": [{"column": "mean", "min": 0, "basis": " "}]}),
            "ranges",
        ),
    ] {
        let invalid = check(&reply(Some(expect))).err().unwrap();
        assert_eq!(refusal(&invalid).reason, "proposal_step_expect_invalid");
        assert_eq!(refusal(&invalid).names, [format!("bootstrap: {part}")]);
    }
    let accepted = check(&reply(Some(json!({"rows": {"min": 1}})))).unwrap();
    assert_eq!(
        accepted.proposal.actions[0]
            .expect
            .as_ref()
            .unwrap()
            .rows
            .as_ref()
            .unwrap()
            .min,
        Some(1)
    );
    assert_eq!(
        rejection_reason("proposal_step_expect_missing"),
        "proposal_step_expect_missing"
    );
    assert_eq!(
        rejection_reason("proposal_step_expect_invalid"),
        "proposal_step_expect_invalid"
    );
    // A reply that does not parse names the expectation's fields at fault.
    let mut odd = reply(Some(
        json!({"rows": {"least": 1}, "ranges": [{"column": "mean", "max": "high"}], "extra": 1}),
    ));
    assert!(serde_json::from_value::<Proposal>(odd.clone()).is_err());
    let names = proposal_schema_names(&odd);
    for name in [
        "unexpected:actions.expect.extra",
        "unexpected:actions.expect.rows.least",
        "invalid:actions.expect.ranges.max",
        "missing:actions.expect.ranges.basis",
    ] {
        assert!(names.contains(&name.to_owned()), "{name} in {names:?}");
    }
    odd["actions"][0].as_object_mut().unwrap().remove("expect");
    assert!(proposal_schema_names(&odd).contains(&"missing:actions.expect".to_owned()));
}

#[test]
fn the_record_keeps_checks_within_its_bound() {
    let long = "r".repeat(250);
    let checks: Vec<Value> = (0..40)
        .map(|i| json!({"key": format!("s{i}"), "task_id": format!("t{i}"), "ref": format!("artifact://tasks/t{i}/a"),
            "state": "failed", "reasons": vec![long.clone(); 8]}))
        .collect();
    let record = step_checks::record_view(&json!(checks));
    let kept = record["checks"].as_array().unwrap();
    assert!(!kept.is_empty() && kept.len() < 40);
    assert_eq!(
        kept.len() + record["omitted"].as_u64().unwrap() as usize,
        40
    );
    assert!(serde_json::to_vec(&record["checks"]).unwrap().len() <= step_checks::RECORD_BYTES);
    assert_eq!(
        kept[0]["reasons"].as_array().unwrap().len(),
        step_checks::LEAD_REASONS_MAX
    );
    assert_eq!(
        kept[0]["reasons_omitted"],
        8 - step_checks::LEAD_REASONS_MAX
    );
    assert_eq!(
        CheckState::Failed > CheckState::NotCheckable,
        CheckState::NotCheckable > CheckState::Passed
    );
}

// A step's title and rationale may state the numbers its own SQL compares or lists, as a whole token only;
// checked through the lead reply check as a run takes it.
#[test]
fn a_step_may_state_its_own_sql_literals_through_the_lead_check() {
    let state = research();
    let source = json!({"profile": "reference-sql-fdc", "source": "fooddata-central", "title": "FoodData Central",
        "description": "Foods and their nutrients per 100 g.", "release": "April 2026", "schema": "fdc",
        "relations": [{"name": "food", "columns": []}, {"name": "food_nutrient", "columns": []}],
        "functions": ["count"], "row_cap": 500});
    let sources =
        enrichedlab_agent_sandbox::broker::coordinator::query_sources(&json!([source])).unwrap();
    let methods = json!([{"key": "reference-sql-fdc", "name": "Query FoodData Central", "description": "One SELECT."}]);
    let results = json!({"results": [], "earlier_findings": null});
    let compiled = compile_context(
        &state.context(1, 8192).unwrap(),
        &HashMap::new(),
        &methods,
        &json!([]),
        &results,
        2,
        16384,
    );
    let names = vec!["reference-sql-fdc".to_owned()];
    let (own, archived) = (json!([]), HashSet::new());
    let check = |rationale: &str| {
        let reply = json!({"action": "delegate", "answer": "not_yet", "summary": "Read the foods next.",
            "lessons": [], "actions": [{"action_key": "foods", "title": "Read foods", "profile": "reference-sql-fdc",
                "objective": "Find the foods with the most protein.", "rationale": rationale,
                "sql": "SELECT n.fdc_id FROM food_nutrient n WHERE n.nutrient_id IN (1003, 1004) AND n.amount > 25 ORDER BY 1",
                "expect": {"rows": {"min": 1}}}]});
        check_lead_reply(
            &LeadCheck {
                state: &state,
                profiles: &names,
                parallel: 2,
                methods: &methods,
                sources: &sources,
                own_queries: &own,
                context: &compiled.value,
                index: &compiled.index,
                rejected: &compiled.rejected,
                archived: &archived,
                input_ceiling: usize::MAX,
            },
            &reply,
        )
    };
    let kept = check("Keep nutrients 1003 and 1004 above 25 grams.").unwrap();
    assert_eq!(kept.step_numbers_masked, 0);
    // Anything else is no literal of the step's own SQL as it writes it: masked in the stored rationale,
    // so the composed and wrapped forms the r33 reviews found never reach the record either.
    for (rationale, stored) in [
        (
            "Keep nutrients 1003 and 1004 above 30 grams.",
            "Keep nutrients 1003 and 1004 above (a number) grams.",
        ),
        (
            // A direction mark anywhere refuses every number in the text, the step's own literals included, and
            // masking drops the mark.
            "Keep nutrient 1003\u{200e}1004 above 25.",
            "Keep nutrient (a number)(a number) above (a number).",
        ),
        (
            "Keep nutrients 1003 and 1004 above .25 grams.",
            "Keep nutrients 1003 and 1004 above .(a number) grams.",
        ),
        (
            "Keep nutrients 1003 and 1004 above -.25 grams.",
            "Keep nutrients 1003 and 1004 above -.(a number) grams.",
        ),
        ("Keep 25^1003 rows.", "Keep (a number)^(a number) rows."),
    ] {
        let checked = check(rationale).unwrap();
        assert_eq!(
            checked.proposal.actions[0].rationale, stored,
            "{rationale:?}"
        );
        assert!(checked.step_numbers_masked > 0, "{rationale:?}");
    }
    // A number inside a name nothing shows is still refused.
    let error = check("Keep nutrient Gene1003 above 25.").err().unwrap();
    assert_eq!(refusal(&error).reason, "proposal_prose_number");
    assert_eq!(refusal(&error).names, ["actions.rationale: 1003"]);
}
