//! Scoped lessons. A lesson may say what it holds for (`scope`: a method, object collection or reviewed query key the
//! call showed), the state it held under (`condition`: refs the call showed), what it came from (`outcome_class`:
//! `accepted`, a fixed check outcome or a refusal code the call showed) and the earlier numbered lesson it disputes
//! (`contradicts`). Code, never the model, writes two more fields: `validated_snapshot`, the digest of each scoped
//! key's definition when the lesson is committed, and `counter_refs` on a disputed lesson, the disputing lesson's
//! evidence.
//!
//! A scoped lesson is shown only while every key it names is shown, and is stale once a named definition changes, is
//! gone or was never recorded; the checkpoint view places stale lessons after the fresh ones before it is cut to its
//! budget (`ResearchState::context_placed`). A checkpoint with no lesson using these fields compiles to the same bytes
//! as before them.
use std::collections::{BTreeMap, BTreeSet, HashSet};

use serde_json::{Value, json};
use sha2::{Digest, Sha256};

use super::methods::methods_view;
use super::plan::ref_task;
use super::replies::CodeRefused;
use crate::broker::canonical::canonical_json;
use crate::broker::lead_objects;
use crate::broker::project::{
    FIXED_OUTCOMES, LESSON_CONDITION_MAX, LESSON_COUNTER_REFS_MAX, Lesson, LessonPlacement,
};

/// The kinds a scope may name, each a key the methods section shows. A query source is named by its method: the
/// section shows a source by its title, and each query method reads one source.
pub(crate) const KINDS: [&str; 3] = ["method", "collection", "tool"];

fn digest(value: &Value) -> String {
    format!(
        "sha256:{}",
        hex::encode(Sha256::digest(canonical_json(value)))
    )
}

/// Each key in a methods list, as `kind:key`, with its method and its own entry. `methods` is either the run's method
/// profiles or a methods section: both carry the method key and the query's reviewed queries, and a profile's object
/// collections are the ones its view shows.
fn keys(methods: &Value, mut each: impl FnMut(String, &Value, &Value)) {
    for method in methods.as_array().into_iter().flatten() {
        let Some(key) = method["key"].as_str() else {
            continue;
        };
        each(format!("method:{key}"), method, &Value::Null);
        for tool in method["query"]["reviewed_queries"]
            .as_array()
            .into_iter()
            .flatten()
        {
            if let Some(k) = tool["key"].as_str() {
                each(format!("tool:{k}"), method, tool);
            }
        }
        let collections = match method.get(lead_objects::FOUND_FIELD) {
            Some(found) => lead_objects::view(found, key).0["collections"].take(),
            None => method["objects"]["collections"].clone(),
        };
        for c in collections.as_array().into_iter().flatten() {
            if let Some(r) = c["ref"].as_str() {
                each(format!("collection:{r}"), method, c);
            }
        }
    }
}

/// The scope keys a methods section shows.
pub(crate) fn shown(context: &Value) -> BTreeSet<String> {
    let mut out = BTreeSet::new();
    keys(&context["methods"], |k, _, _| {
        out.insert(k);
    });
    out
}

/// The digest of what defines each key in the run's method profiles: a method by its name, description, template,
/// refs, query source title and the stored card's digest; a reviewed query by its entry; a collection by its reference,
/// title and summary. File counts, sizes and samples are left out, so ordinary growth does not make a lesson stale.
pub(crate) fn definitions(profiles: &Value) -> BTreeMap<String, String> {
    let mut out = BTreeMap::new();
    keys(profiles, |k, method, item| {
        let value = match k.split_once(':').map(|(kind, _)| kind) {
            Some("method") => digest(
                &json!({"name": method["name"], "description": method["description"],
                "template": method["template"], "input_refs": method["input_refs"], "tool_refs": method["tool_refs"],
                "query_source": method["query"]["source"], "card_sha256": method["query_card"]["sha256"]}),
            ),
            Some("tool") => digest(item),
            Some("collection") => digest(
                &json!({"ref": item["ref"], "title": item["title"], "summary": item["summary"]}),
            ),
            _ => return,
        };
        out.insert(k, value);
    });
    out
}

/// Where a run's checkpoint view places each lesson, by the methods the run shows and their current definitions: an
/// unscoped lesson is fresh; a scoped one is out of scope unless every key it names is shown, and stale unless its
/// snapshot holds every key's current digest.
pub(crate) fn placement(profiles: &Value) -> impl Fn(&Lesson) -> LessonPlacement + use<> {
    let shown = shown(&json!({"methods": methods_view(profiles, 0)}));
    let definitions = definitions(profiles);
    move |lesson: &Lesson| {
        let Some(scope) = &lesson.scope else {
            return LessonPlacement::Fresh;
        };
        let ids: Vec<String> = scope.iter().map(|(k, v)| format!("{k}:{v}")).collect();
        if !ids.iter().all(|id| shown.contains(id)) {
            return LessonPlacement::OutOfScope;
        }
        let current = lesson.validated_snapshot.as_ref().is_some_and(|snapshot| {
            ids.iter().all(|id| {
                snapshot
                    .get(id)
                    .is_some_and(|d| definitions.get(id) == Some(d))
            })
        });
        if current {
            LessonPlacement::Fresh
        } else {
            LessonPlacement::Stale
        }
    }
}

/// Every reply-level fault in the lessons' scoped fields, named by lesson and field, never by the reply's text: a
/// scope kind outside the three or a key the call did not show; a condition ref the call did not show or one from a
/// rejected analysis; an outcome class that is neither fixed nor a refusal code the call's attempts showed; a dispute
/// of a lesson the call did not number; or a field only code writes.
pub(crate) fn faults(
    lessons: &[Lesson],
    context: &Value,
    profiles: &Value,
    evidence: &HashSet<String>,
    rejected: &HashSet<String>,
) -> Result<(), CodeRefused> {
    let shown = shown(context);
    let defined = definitions(profiles);
    let numbered: BTreeSet<u64> = context["earlier_readings"]["lessons"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|l| l["n"].as_u64())
        .collect();
    let refusals: BTreeSet<&str> = context["attempts"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|a| a["refused"].as_str())
        .collect();
    let total = lessons.len();
    let mut names = Vec::new();
    for (i, lesson) in lessons.iter().enumerate() {
        let name = format!("lesson {} of {total}", i + 1);
        if let Some(scope) = &lesson.scope {
            if scope.is_empty() {
                names.push(format!("{name} scope: names no key"));
            }
            for (kind, key) in scope {
                let id = format!("{kind}:{key}");
                if !KINDS.contains(&kind.as_str()) {
                    names.push(format!(
                        "{name} scope.{}: not one of method, collection, tool",
                        super::reply_schema::field_name(kind)
                    ));
                } else if !shown.contains(&id) || !defined.contains_key(&id) {
                    names.push(format!(
                        "{name} scope.{kind}: not a {kind} key shown in methods"
                    ));
                }
            }
        }
        if lesson.condition.len() > LESSON_CONDITION_MAX {
            names.push(format!(
                "{name} condition: {} of {LESSON_CONDITION_MAX} refs",
                lesson.condition.len()
            ));
        }
        let positions = |keep: &dyn Fn(&String) -> bool| -> Vec<String> {
            lesson
                .condition
                .iter()
                .enumerate()
                .filter(|(_, r)| keep(r))
                .map(|(n, _)| (n + 1).to_string())
                .collect()
        };
        let unshown = positions(&|r| !evidence.contains(r));
        if !unshown.is_empty() {
            names.push(format!(
                "{name} condition: ref {} was not shown",
                unshown.join(", ")
            ));
        }
        let withheld = positions(&|r| ref_task(r).is_some_and(|t| rejected.contains(t)));
        if !withheld.is_empty() {
            names.push(format!(
                "{name} condition: ref {} is a rejected analysis's",
                withheld.join(", ")
            ));
        }
        if let Some(class) = lesson.outcome_class.as_deref()
            && !FIXED_OUTCOMES.contains(&class)
            && !refusals.contains(class)
        {
            names.push(format!(
                "{name} outcome_class: not accepted, failed_checks, no_progress, query_failed or a refusal \
                 in attempts"
            ));
        }
        if let Some(n) = lesson.contradicts
            && !numbered.contains(&(n as u64))
        {
            names.push(format!("{name} contradicts: not the n of a lesson shown"));
        }
        if lesson.validated_snapshot.is_some() {
            names.push(format!("{name} validated_snapshot: written by code"));
        }
        if !lesson.counter_refs.is_empty() {
            names.push(format!("{name} counter_refs: written by code"));
        }
    }
    if names.is_empty() {
        Ok(())
    } else {
        Err(CodeRefused {
            code: "proposal_lesson_invalid",
            names,
        })
    }
}

/// Adds a turn's admitted lessons to the checkpoint: each scoped lesson gets one digest per key it names, and a
/// disputing lesson's evidence becomes the disputed lesson's counter refs, at most `LESSON_COUNTER_REFS_MAX`, first
/// kept. A key without a definition gets no digest, which the checkpoint refuses (`checkpoint_invalid`); the lead check
/// has already refused a key the run does not define.
pub(crate) fn commit(lessons: &mut Vec<Lesson>, added: &[Lesson], profiles: &Value) {
    let definitions = definitions(profiles);
    for lesson in added {
        let mut lesson = lesson.clone();
        lesson.validated_snapshot = lesson.scope.as_ref().map(|scope| {
            scope
                .iter()
                .filter_map(|(kind, key)| {
                    let id = format!("{kind}:{key}");
                    definitions.get(&id).map(|d| (id, d.clone()))
                })
                .collect()
        });
        if let Some(target) = lesson
            .contradicts
            .and_then(|n| n.checked_sub(1))
            .and_then(|i| lessons.get_mut(i))
        {
            for r in &lesson.evidence_refs {
                if target.counter_refs.len() < LESSON_COUNTER_REFS_MAX
                    && !target.counter_refs.contains(r)
                {
                    target.counter_refs.push(r.clone());
                }
            }
        }
        lessons.push(lesson);
    }
}

#[cfg(test)]
#[path = "lesson_scope_tests.rs"]
mod tests;
