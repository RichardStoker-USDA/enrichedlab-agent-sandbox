use std::collections::{BTreeMap, HashSet};

use serde_json::json;

use super::*;
use crate::broker::project::{LessonKind, ResearchState, code_fields_changed};

const CARD: &str = "sha256:1111111111111111111111111111111111111111111111111111111111111111";
const OTHER_CARD: &str = "sha256:2222222222222222222222222222222222222222222222222222222222222222";

/// The run's method profiles in production shape: a query method whose query view names its source by title and
/// lists reviewed queries, and a lab objects method whose found collections carry a file count and sampled names.
fn profiles(card: &str, files: u64, sample: &[&str]) -> Value {
    json!([
        {"key": "lab-sql", "name": "Query the lab", "description": "One SELECT.", "input_refs": ["input://lab"],
         "tool_refs": ["tool://reviewed/query"],
         "query": {"source": "Lab weights", "reviewed_queries": [{"key": "weights", "title": "Weights", "sql_sha256": "sha256:aa"}]},
         "query_card": {"sha256": card, "card": {}}},
        {"key": "lab-objects", "name": "Read the lab files", "description": "Files.",
         lead_objects::FOUND_FIELD: [{"ref": "objects://scans", "title": "Scans", "summary": "Scan files.",
            "files": files, "bytes": 10, "sample": sample}]}
    ])
}

fn context(numbered: &[u64], refusals: &[&str]) -> Value {
    json!({
        "methods": [
            {"key": "lab-sql", "query": {"source": "Lab weights", "reviewed_queries": [{"key": "weights"}]}},
            {"key": "lab-objects", "objects": {"collections": [{"ref": "objects://scans"}]}}
        ],
        "earlier_readings": {"lessons": numbered.iter().map(|n| json!({"n": n})).collect::<Vec<_>>()},
        "attempts": refusals.iter().map(|r| json!({"turn": 1, "refused": r})).collect::<Vec<_>>()
    })
}

fn scoped(pairs: &[(&str, &str)]) -> Lesson {
    let mut lesson = Lesson::new(
        "Weights are recorded per sample.",
        LessonKind::Observation,
        vec!["artifact://tasks/t/a".into()],
    );
    lesson.scope = Some(
        pairs
            .iter()
            .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
            .collect(),
    );
    lesson
}

fn state(lessons: Vec<Lesson>) -> ResearchState {
    let mut s: ResearchState = serde_json::from_value(json!({"schema_version": 1, "goal": "Which samples were weighed?",
        "constraints": [], "plan": [], "lessons": [], "next_action": "Plan.",
        "workspace": {"schema_version": 1, "profile": "lab-sql", "image_digest": format!("sha256:{}", "a".repeat(64)),
            "source_digest": format!("sha256:{}", "b".repeat(64)), "input_refs": ["input://lab/example"],
            "tool_refs": ["tool://reviewed/query"], "preparation_receipt": null}}))
    .unwrap();
    s.lessons = lessons;
    s
}

#[test]
fn a_lesson_without_scoped_fields_keeps_its_shape() {
    let lesson = Lesson::new("A hypothesis.", LessonKind::Hypothesis, vec![]);
    assert_eq!(
        serde_json::to_value(&lesson).unwrap(),
        json!({"statement": "A hypothesis.", "kind": "hypothesis", "evidence_refs": []})
    );
    let read: Lesson = serde_json::from_value(
        json!({"statement": "s", "kind": "observation", "evidence_refs": ["input://x"]}),
    )
    .unwrap();
    assert!(!read.uses_scoped_fields());
}

#[test]
fn the_shown_keys_and_the_definitions_name_the_same_keys() {
    let expected = vec![
        "collection:objects://scans",
        "method:lab-objects",
        "method:lab-sql",
        "tool:weights",
    ];
    assert_eq!(
        shown(&context(&[], &[]))
            .iter()
            .map(String::as_str)
            .collect::<Vec<_>>(),
        expected
    );
    assert_eq!(
        definitions(&profiles(CARD, 2, &["a.csv", "b.csv"]))
            .keys()
            .map(String::as_str)
            .collect::<Vec<_>>(),
        expected
    );
}

#[test]
fn growth_keeps_a_definition_and_a_new_card_or_summary_changes_one() {
    let first = definitions(&profiles(CARD, 2, &["a.csv", "b.csv"]));
    // More files and a different sample are growth.
    let grown = definitions(&profiles(CARD, 40, &["c.csv"]));
    assert_eq!(first, grown);
    // A new card changes the query method's definition, not the collection's.
    let recarded = definitions(&profiles(OTHER_CARD, 2, &["a.csv", "b.csv"]));
    assert_ne!(first["method:lab-sql"], recarded["method:lab-sql"]);
    assert_eq!(
        first["collection:objects://scans"],
        recarded["collection:objects://scans"]
    );
    let mut renamed = profiles(CARD, 2, &["a.csv"]);
    renamed[1][lead_objects::FOUND_FIELD][0]["summary"] = json!("Other scans.");
    assert_ne!(
        first["collection:objects://scans"],
        definitions(&renamed)["collection:objects://scans"]
    );
}

#[test]
fn a_scoped_lesson_on_shown_keys_passes_and_every_other_fault_is_named() {
    let evidence: HashSet<String> = [
        "artifact://tasks/t/a".to_owned(),
        "artifact://tasks/r/x".to_owned(),
    ]
    .into();
    let rejected: HashSet<String> = ["r".to_owned()].into();
    let methods = profiles(CARD, 2, &["a.csv"]);
    let mut good = scoped(&[("method", "lab-sql"), ("tool", "weights")]);
    good.condition = vec!["artifact://tasks/t/a".into()];
    good.outcome_class = Some("sql_refused_relation".into());
    assert!(
        faults(
            &[good],
            &context(&[], &["sql_refused_relation"]),
            &methods,
            &evidence,
            &rejected
        )
        .is_ok()
    );

    let source_kind = scoped(&[("source", "Lab weights")]);
    let unshown = scoped(&[("method", "other")]);
    let mut written_by_code = scoped(&[("collection", "objects://scans")]);
    written_by_code.validated_snapshot = Some(BTreeMap::new());
    written_by_code.counter_refs = vec!["input://x".into()];
    let mut dispute = Lesson::new("s", LessonKind::Hypothesis, vec![]);
    dispute.contradicts = Some(3);
    dispute.condition = vec!["input://not-shown".into(), "artifact://tasks/r/x".into()];
    dispute.outcome_class = Some("proposal_nonexistent".into());
    let refused = faults(
        &[source_kind, unshown, written_by_code, dispute],
        &context(&[1, 2], &["sql_refused_relation"]),
        &methods,
        &evidence,
        &rejected,
    )
    .unwrap_err();
    assert_eq!(refused.code, "proposal_lesson_invalid");
    assert_eq!(
        refused.names,
        vec![
            "lesson 1 of 4 scope.source: not one of method, collection, tool",
            "lesson 2 of 4 scope.method: not a method key shown in methods",
            "lesson 3 of 4 validated_snapshot: written by code",
            "lesson 3 of 4 counter_refs: written by code",
            "lesson 4 of 4 condition: ref 1 was not shown",
            "lesson 4 of 4 condition: ref 2 is a rejected analysis's",
            "lesson 4 of 4 outcome_class: not accepted, failed_checks, no_progress, query_failed or a refusal in attempts",
            "lesson 4 of 4 contradicts: not the n of a lesson shown",
        ]
    );
}

#[test]
fn a_committed_scoped_lesson_holds_a_complete_snapshot_and_a_dispute_links_back() {
    let mut lessons = vec![Lesson::new("First.", LessonKind::Hypothesis, vec![])];
    let mut dispute = scoped(&[("method", "lab-sql"), ("collection", "objects://scans")]);
    dispute.contradicts = Some(1);
    let methods = profiles(CARD, 2, &["a.csv"]);
    commit(&mut lessons, &[dispute], &methods);
    assert_eq!(lessons[0].counter_refs, vec!["artifact://tasks/t/a"]);
    let snapshot = lessons[1].validated_snapshot.as_ref().unwrap();
    assert_eq!(
        snapshot.keys().map(String::as_str).collect::<Vec<_>>(),
        vec!["collection:objects://scans", "method:lab-sql"]
    );
    assert!(state(lessons).checkpoint_faults().is_empty());
}

#[test]
fn the_placed_view_leaves_out_of_scope_lessons_and_cuts_stale_ones_first() {
    let methods = profiles(CARD, 2, &["a.csv"]);
    let mut lessons = Vec::new();
    commit(
        &mut lessons,
        &[
            scoped(&[("method", "lab-sql")]),
            Lesson::new(
                "Unscoped.",
                LessonKind::Observation,
                vec!["input://lab/example".into()],
            ),
            scoped(&[("collection", "objects://scans")]),
            scoped(&[("method", "lab-objects")]),
        ],
        &methods,
    );
    let mut research = state(lessons);
    // A long question keeps the tight budget below within the view's 4 KiB minimum.
    research.goal = "Which samples were weighed? ".repeat(160);
    research.validate().unwrap();
    // The card changed (the first lesson is stale) and the objects method is no longer offered.
    let later = json!([profiles(OTHER_CARD, 2, &["a.csv"])[0].clone()]);
    let view = research
        .context_placed(1, 65536, &placement(&later))
        .unwrap();
    assert_eq!(view["out_of_scope_lessons"], 2);
    let shown: Vec<(u64, bool)> = view["lessons"]
        .as_array()
        .unwrap()
        .iter()
        .map(|l| (l["n"].as_u64().unwrap(), l["stale"] == true))
        .collect();
    assert_eq!(shown, vec![(2, false), (1, true)]);
    assert!(
        view["lessons"]
            .as_array()
            .unwrap()
            .iter()
            .all(|l| l.get("validated_snapshot").is_none())
    );
    // At a budget that holds one lesson, the stale one gives way, not the fresh one after it in the checkpoint.
    let bytes = |v: &Value| serde_json::to_vec(v).unwrap().len();
    let one = bytes(&view) - bytes(&view["lessons"][1]) - 1;
    let tight = research.context_placed(1, one, &placement(&later)).unwrap();
    assert_eq!(tight["lessons"].as_array().unwrap().len(), 1);
    assert_eq!(tight["lessons"][0]["n"], 2);
    assert_eq!(tight["omitted_lessons"], 1);
}

#[test]
fn a_scoped_lesson_without_a_complete_snapshot_is_refused_when_stored_and_stale_when_shown() {
    let mut partial = scoped(&[("method", "lab-sql"), ("tool", "weights")]);
    partial.validated_snapshot = Some(BTreeMap::from([(
        "method:lab-sql".to_owned(),
        CARD.to_owned(),
    )]));
    assert_eq!(
        state(vec![partial.clone()]).checkpoint_faults(),
        vec!["lesson 1 of 1 validated_snapshot: not one digest per scope key"]
    );
    let bare = scoped(&[("method", "lab-sql")]);
    assert_eq!(
        state(vec![bare.clone()]).checkpoint_faults(),
        vec!["lesson 1 of 1 validated_snapshot: missing"]
    );
    let place = placement(&profiles(CARD, 2, &["a.csv"]));
    assert_eq!(place(&partial), LessonPlacement::Stale);
    assert_eq!(place(&bare), LessonPlacement::Stale);
}

#[test]
fn a_save_keeps_code_written_fields_but_cannot_add_or_change_them() {
    let methods = profiles(CARD, 2, &["a.csv"]);
    let mut prior = vec![Lesson::new("First.", LessonKind::Hypothesis, vec![])];
    let mut dispute = scoped(&[("method", "lab-sql")]);
    dispute.contradicts = Some(1);
    commit(&mut prior, &[dispute], &methods);
    assert!(code_fields_changed(&prior, &prior).is_empty());
    let mut forged = prior.clone();
    forged[1].validated_snapshot = Some(BTreeMap::from([(
        "method:lab-sql".to_owned(),
        OTHER_CARD.to_owned(),
    )]));
    forged[0].counter_refs.push("input://x".into());
    assert_eq!(
        code_fields_changed(&prior, &forged),
        vec![
            "lesson 1 of 2: code-written fields are not the saved ones",
            "lesson 2 of 2: code-written fields are not the saved ones",
        ]
    );
    let mut added = prior.clone();
    let mut copy = prior[1].clone();
    copy.statement = "Another.".into();
    added.push(copy);
    assert_eq!(code_fields_changed(&prior, &added).len(), 1);
}

#[test]
fn the_compiled_context_carries_the_placed_view() {
    let methods = json!([{"key": "lab-sql", "name": "Query the lab", "description": "One SELECT.",
        "query": {"source": "Lab weights"}}]);
    let compile = |view: Value| {
        super::super::context::compile_context(
            &view,
            &std::collections::HashMap::new(),
            &methods,
            &json!([]),
            &json!({"results": [], "earlier_findings": null}),
            1,
            60_000,
        )
        .value["earlier_readings"]
            .clone()
    };
    let plain = state(vec![Lesson::new("Plain.", LessonKind::Hypothesis, vec![])]);
    let unscoped = compile(plain.context(1, 65536).unwrap());
    assert_eq!(
        unscoped["lessons"],
        json!([{"statement": "Plain.", "kind": "hypothesis", "evidence_refs": []}])
    );
    assert!(unscoped.get("out_of_scope_lessons").is_none());
    let mut lessons = plain.lessons.clone();
    commit(&mut lessons, &[scoped(&[("method", "lab-sql")])], &methods);
    let mut gone = scoped(&[("tool", "gone")]);
    gone.validated_snapshot = Some(BTreeMap::from([("tool:gone".to_owned(), CARD.to_owned())]));
    lessons.push(gone);
    let scoped_state = state(lessons);
    let compiled = compile(
        scoped_state
            .context_placed(1, 65536, &placement(&methods))
            .unwrap(),
    );
    assert_eq!(compiled["out_of_scope_lessons"], 1);
    let shown: Vec<u64> = compiled["lessons"]
        .as_array()
        .unwrap()
        .iter()
        .map(|l| l["n"].as_u64().unwrap())
        .collect();
    assert_eq!(shown, vec![2, 1]);
}
