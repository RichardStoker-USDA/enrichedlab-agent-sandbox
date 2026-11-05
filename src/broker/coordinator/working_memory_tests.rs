use super::*;
use serde_json::json;

fn context() -> Value {
    json!({"question": "Which 5 raw vegetables have the most vitamin C?", "constraints": [], "scientist_instructions": [],
        "analyses": [{"key": "rank-vitamin-c", "status": "pending", "result_refs": []},
            {"key": "profile-categories", "status": "completed", "result_refs": ["artifact://tasks/3/r3"]}],
        "results": [{"ref": "artifact://tasks/11111111-1111-1111-1111-111111111111/r1", "table": "top.csv"}],
        "literature": [{"ref": "library://p1/section/a", "text": "Vitamin C in raw foods."}],
        "earlier_findings": {"findings": [{"statement": "{a} leads.", "bindings": {"a": {"ref": "artifact://tasks/2/r2"}}}]},
        "lessons": [{"text": "Check units first.", "evidence_refs": ["artifact://tasks/3/r3"]}]})
}

fn valid() -> Value {
    json!({"open_threads": ["Whether cooked forms belong in the ranking."],
        "ruled_out": [{"what": "Ranking by dry weight.", "why_ref": "library://p1/section/a",
            "under": ["artifact://tasks/11111111-1111-1111-1111-111111111111/r1"]}],
        "depends_on": [{"next_step_key": "rank-vitamin-c", "needs": ["artifact://tasks/3/r3"]}],
        "unknowns": ["Which category names raw vegetables."]})
}

#[test]
fn a_memory_names_only_refs_and_steps_the_context_shows() {
    let memory = check(&valid(), &context(), WORKING_MEMORY_BYTES).expect("valid memory");
    assert_eq!(memory.ruled_out[0].why_ref, "library://p1/section/a");
    assert_eq!(memory.depends_on[0].next_step_key, "rank-vitamin-c");
    // Refs nested in bindings and `_refs` lists are shown refs too.
    let refs = shown_refs(&context());
    assert!(refs.contains("artifact://tasks/2/r2") && refs.contains("artifact://tasks/3/r3"));
}

#[test]
fn unknown_refs_and_steps_repeated_refs_and_counts_are_refused_by_position() {
    let mut m = valid();
    m["ruled_out"][0]["why_ref"] = json!("library://p9/invented");
    m["ruled_out"][0]["under"] = json!(["artifact://tasks/2/r2", "artifact://tasks/2/r2"]);
    m["depends_on"][0]["next_step_key"] = json!("a-step-nobody-planned");
    m["unknowns"] = json!(["a", "b", "c", "d", "e"]);
    let faults = check(&m, &context(), WORKING_MEMORY_BYTES).unwrap_err();
    assert_eq!(
        faults,
        vec![
            "unknowns: count",
            "ruled_out 1: why_ref ref_unknown",
            "ruled_out 1: under ref_repeated",
            "depends_on 1: next_step_key key_unknown"
        ]
    );
}

#[test]
fn a_number_the_question_does_not_state_and_an_overlong_text_are_refused() {
    let mut m = valid();
    m["open_threads"] = json!([
        "The top 5 is settled.",
        "Kale held 120 mg.",
        "x".repeat(301)
    ]);
    let faults = check(&m, &context(), WORKING_MEMORY_BYTES).unwrap_err();
    assert_eq!(
        faults,
        vec!["open_threads 2: number", "open_threads 3: text"]
    );
}

#[test]
fn a_reply_with_another_field_or_shape_is_one_fault_and_a_large_memory_is_too_large() {
    let mut extra = valid();
    extra["findings"] = json!([]);
    assert_eq!(
        check(&extra, &context(), WORKING_MEMORY_BYTES).unwrap_err(),
        vec!["shape"]
    );
    assert_eq!(
        check(&json!("text"), &context(), WORKING_MEMORY_BYTES).unwrap_err(),
        vec!["shape"]
    );
    assert_eq!(
        check(&valid(), &context(), 64).unwrap_err(),
        vec!["too_large"]
    );
    // Every list may be empty.
    let empty = json!({"open_threads": [], "ruled_out": [], "depends_on": [], "unknowns": []});
    assert!(check(&empty, &context(), WORKING_MEMORY_BYTES).is_ok());
}

#[test]
fn a_kept_memory_is_reserved_and_placed_whole_or_not_at_all() {
    let kept = json!({"turn": 3, "memory": valid(), "sha256": "sha256:x"});
    let reserved = reserve(Some(&kept));
    assert!(reserved > 0 && reserve(None) == 0);
    let mut context = context();
    let before = serde_json::to_vec(&context).unwrap().len();
    assert!(!place(&mut context, Some(&kept), before + reserved - 1));
    assert!(context.get("working_memory").is_none());
    assert!(place(&mut context, Some(&kept), before + reserved));
    assert_eq!(
        serde_json::to_vec(&context).unwrap().len(),
        before + reserved
    );
    assert_eq!(context["working_memory"]["label"], LABEL);
    assert_eq!(
        context["working_memory"]["open_threads"],
        valid()["open_threads"]
    );
}

#[test]
fn the_turn_record_keeps_the_accepted_memory_with_its_digest_or_only_the_refusal_codes() {
    let accepted = Refreshed {
        memory: Some(valid()),
        status: "accepted",
        faults: vec![],
        receipts: vec![json!({"r": 1})],
    };
    let mut record = json!({});
    accepted.record(&mut record, true);
    assert_eq!(record["working_memory"], valid());
    assert_eq!(
        record["working_memory_sha256"],
        json!(sha(&serde_json::to_vec(&valid()).unwrap()))
    );
    assert_eq!(record["working_memory_call"]["status"], "accepted");
    assert_eq!(record["working_memory_call"]["placed"], true);
    let refused = Refreshed {
        memory: None,
        status: "refused",
        faults: vec!["ruled_out 1: why_ref ref_unknown".into()],
        receipts: vec![],
    };
    let mut record = json!({});
    refused.record(&mut record, false);
    assert!(
        record.get("working_memory").is_none() && record.get("working_memory_sha256").is_none()
    );
    assert_eq!(
        record["working_memory_call"]["faults"],
        json!(["ruled_out 1: why_ref ref_unknown"])
    );
}
