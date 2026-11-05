use std::collections::{BTreeMap, HashSet};

use serde_json::json;

use super::*;
use crate::broker::project::LessonKind;

const CARD: &str = "sha256:1111111111111111111111111111111111111111111111111111111111111111";

fn profiles(card: &str, files: &[&str]) -> Value {
    json!([
        {"key": "lab-sql", "name": "Query the lab", "description": "One SELECT.", "input_refs": ["input://lab"],
         "tool_refs": ["tool://reviewed/query"],
         "query": {"source": "lab", "reviewed_queries": [{"key": "weights", "sql_sha256": "sha256:aa"}]},
         "query_card": {"sha256": card, "card": {}}},
        {"key": "lab-objects", "name": "Read the lab files", "description": "Files.",
         lead_objects::FOUND_FIELD: [{"ref": "objects://scans", "title": "Scans", "summary": "Scan files.",
            "files": files, "bytes": 10, "sample": []}]}
    ])
}

fn context(numbered: &[u64]) -> Value {
    json!({
        "methods": [
            {"key": "lab-sql", "query": {"source": "lab", "reviewed_queries": [{"key": "weights"}]}},
            {"key": "lab-objects", "objects": {"collections": [{"ref": "objects://scans"}]}}
        ],
        "earlier_readings": {"lessons": numbered.iter().map(|n| json!({"n": n})).collect::<Vec<_>>()}
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
    let shown = shown(&context(&[]));
    let defined = definitions(&profiles(CARD, &["a.csv"]));
    let expected: Vec<&str> = vec![
        "collection:objects://scans",
        "method:lab-objects",
        "method:lab-sql",
        "source:lab",
        "tool:weights",
    ];
    assert_eq!(
        shown.iter().map(String::as_str).collect::<Vec<_>>(),
        expected
    );
    assert_eq!(
        defined.keys().map(String::as_str).collect::<Vec<_>>(),
        expected
    );
    assert_eq!(defined["source:lab"], CARD);
}

#[test]
fn a_scoped_lesson_on_shown_keys_passes_and_every_other_fault_is_named() {
    let evidence: HashSet<String> = ["artifact://tasks/t/a".to_owned()].into();
    let mut good = scoped(&[("source", "lab"), ("tool", "weights")]);
    good.condition = vec!["artifact://tasks/t/a".into()];
    good.outcome_class = Some("sql_refused_relation".into());
    assert!(faults(&[good], &context(&[]), &evidence).is_ok());

    let unknown_kind = scoped(&[("lab", "lab-a")]);
    let unshown = scoped(&[("method", "other")]);
    let mut written_by_code = scoped(&[("source", "lab")]);
    written_by_code.validated_snapshot = Some(BTreeMap::new());
    written_by_code.counter_refs = vec!["input://x".into()];
    let mut dispute = Lesson::new("s", LessonKind::Hypothesis, vec![]);
    dispute.contradicts = Some(3);
    dispute.condition = vec!["input://not-shown".into()];
    dispute.outcome_class = Some("made_up".into());
    let refused = faults(
        &[unknown_kind, unshown, written_by_code, dispute],
        &context(&[1, 2]),
        &evidence,
    )
    .unwrap_err();
    assert_eq!(refused.code, "proposal_lesson_invalid");
    assert_eq!(
        refused.names,
        vec![
            "lesson 1 of 4 scope.lab: not one of method, source, collection, tool",
            "lesson 2 of 4 scope.method: not a method key shown in methods",
            "lesson 3 of 4 validated_snapshot: written by code",
            "lesson 3 of 4 counter_refs: written by code",
            "lesson 4 of 4 condition: ref 1 was not shown",
            "lesson 4 of 4 outcome_class: not accepted or a refusal or check code",
            "lesson 4 of 4 contradicts: not the n of a lesson shown",
        ]
    );
}

#[test]
fn a_committed_scoped_lesson_holds_its_snapshot_and_a_dispute_links_back() {
    let mut lessons = vec![Lesson::new("First.", LessonKind::Hypothesis, vec![])];
    let mut dispute = scoped(&[("source", "lab")]);
    dispute.contradicts = Some(1);
    commit(&mut lessons, &[dispute], &profiles(CARD, &["a.csv"]));
    assert_eq!(lessons[0].counter_refs, vec!["artifact://tasks/t/a"]);
    assert_eq!(
        lessons[1].validated_snapshot,
        Some(BTreeMap::from([("source:lab".to_owned(), CARD.to_owned())]))
    );
    let state_faults = |lessons: &[Lesson]| {
        lessons
            .iter()
            .enumerate()
            .flat_map(|(i, l)| {
                crate::broker::project::lesson_field_faults(&format!("lesson {}", i + 1), l, i)
            })
            .collect::<Vec<_>>()
    };
    assert!(state_faults(&lessons).is_empty());
}

#[test]
fn placement_hides_lessons_out_of_scope_and_puts_stale_ones_last() {
    let first = profiles(CARD, &["a.csv"]);
    let mut lessons = Vec::new();
    commit(
        &mut lessons,
        &[
            Lesson::new("Unscoped.", LessonKind::Hypothesis, vec![]),
            scoped(&[("collection", "objects://scans")]),
            scoped(&[("source", "lab")]),
            scoped(&[("method", "gone")]),
        ],
        &first,
    );
    let view: Vec<Value> = lessons
        .iter()
        .map(|l| serde_json::to_value(l).unwrap())
        .collect();
    // A new file in the collection changes its definition; the source card is unchanged.
    let later = profiles(CARD, &["a.csv", "b.csv"]);
    let (placed, out_of_scope) = place(view, &shown(&context(&[])), &definitions(&later));
    assert_eq!(out_of_scope, 1);
    let order: Vec<(&str, bool)> = placed
        .iter()
        .map(|l| {
            (
                l["scope"]
                    .as_object()
                    .map_or("none", |s| s.keys().next().unwrap().as_str()),
                l["stale"] == true,
            )
        })
        .collect();
    assert_eq!(
        order,
        vec![("none", false), ("source", false), ("collection", true)]
    );
    assert!(placed.iter().all(|l| l.get("validated_snapshot").is_none()));
}

#[test]
fn a_checkpoint_numbers_its_lessons_only_once_one_uses_a_scoped_field() {
    let mut lessons = vec![Lesson::new("Plain.", LessonKind::Hypothesis, vec![])];
    let numbered = |lessons: &[Lesson]| lessons.iter().any(Lesson::uses_scoped_fields);
    assert!(!numbered(&lessons));
    lessons.push(scoped(&[("source", "lab")]));
    assert!(numbered(&lessons));
}

#[test]
fn the_compiled_context_shows_scoped_lessons_by_the_methods_it_shows() {
    use crate::broker::project::ResearchState;
    let state = |lessons: Value| -> ResearchState {
        serde_json::from_value(json!({"schema_version": 1, "goal": "Which samples were weighed?", "constraints": [],
            "plan": [], "lessons": lessons, "next_action": "Plan.",
            "workspace": {"schema_version": 1, "profile": "lab-sql", "image_digest": format!("sha256:{}", "a".repeat(64)),
                "source_digest": format!("sha256:{}", "b".repeat(64)), "input_refs": ["input://lab/example"],
                "tool_refs": ["tool://reviewed/query"], "preparation_receipt": null}}))
        .unwrap()
    };
    let methods = json!([{"key": "lab-sql", "name": "Query the lab", "description": "One SELECT.",
        "query": {"source": "lab"}}]);
    let compile = |s: &ResearchState| {
        super::super::context::compile_context(
            &s.context(1, 65536).unwrap(),
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
    let plain = json!([{"statement": "Plain.", "kind": "hypothesis", "evidence_refs": []}]);
    let unscoped = compile(&state(plain.clone()));
    assert_eq!(unscoped["lessons"], plain);
    assert!(unscoped.get("out_of_scope_lessons").is_none());

    let defined = definitions(&methods);
    let mut lessons = plain.as_array().unwrap().clone();
    lessons.push(
        json!({"statement": "On the source.", "kind": "hypothesis", "evidence_refs": [],
        "scope": {"source": "lab"}, "validated_snapshot": {"source:lab": defined["source:lab"]}}),
    );
    lessons.push(json!({"statement": "On a method not shown.", "kind": "hypothesis", "evidence_refs": [],
        "scope": {"method": "other"}, "validated_snapshot": {"method:other": defined["method:lab-sql"]}}));
    let scoped = compile(&state(Value::Array(lessons)));
    assert_eq!(scoped["out_of_scope_lessons"], 1);
    let shown: Vec<(&str, u64, bool)> = scoped["lessons"]
        .as_array()
        .unwrap()
        .iter()
        .map(|l| {
            (
                l["statement"].as_str().unwrap(),
                l["n"].as_u64().unwrap(),
                l.get("validated_snapshot").is_some(),
            )
        })
        .collect();
    assert_eq!(
        shown,
        vec![("Plain.", 1, false), ("On the source.", 2, false)]
    );
}
