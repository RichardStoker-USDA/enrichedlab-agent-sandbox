//! Scoped lessons. A lesson may say what it holds for (`scope`: a method, query source, object collection or reviewed
//! query key the call showed), the state it held under (`condition`: refs the call showed), the refusal or check it
//! came from (`outcome_class`) and the earlier numbered lesson it disputes (`contradicts`). Code, never the model,
//! writes two more fields: `validated_snapshot`, the digest of each scoped key's definition when the lesson is
//! committed, and `counter_refs` on a disputed lesson, the disputing lesson's evidence.
//!
//! A scoped lesson is shown only while every key it names is shown, and is marked `stale` once a named definition
//! changes or is gone; stale lessons give way first. A checkpoint with no lesson using these fields compiles to the same
//! bytes as before them.
use std::collections::{BTreeMap, BTreeSet};

use serde_json::{Value, json};
use sha2::{Digest, Sha256};

use super::replies::CodeRefused;
use crate::broker::canonical::canonical_json;
use crate::broker::lead_objects;
use crate::broker::project::{
    LESSON_CONDITION_MAX, LESSON_COUNTER_REFS_MAX, Lesson, outcome_class,
};

/// The kinds a scope may name, each a key the methods section shows.
pub(crate) const KINDS: [&str; 4] = ["method", "source", "collection", "tool"];

fn digest(value: &Value) -> String {
    format!(
        "sha256:{}",
        hex::encode(Sha256::digest(canonical_json(value)))
    )
}

/// The keys of each kind in a methods list, as `kind:key`. `methods` is either the run's method profiles or the
/// methods section a call showed: both carry the key, the query source and its reviewed queries, and the profiles'
/// object collections are the section's after its view.
fn keys(methods: &Value, mut each: impl FnMut(String, &Value, Value)) {
    for method in methods.as_array().into_iter().flatten() {
        let Some(key) = method["key"].as_str() else {
            continue;
        };
        each(format!("method:{key}"), method, Value::Null);
        if let Some(source) = method["query"]["source"].as_str() {
            each(format!("source:{source}"), method, Value::Null);
        }
        for tool in method["query"]["reviewed_queries"]
            .as_array()
            .into_iter()
            .flatten()
        {
            if let Some(k) = tool["key"].as_str() {
                each(format!("tool:{k}"), method, tool.clone());
            }
        }
        let collections = match method.get(lead_objects::FOUND_FIELD) {
            Some(found) => lead_objects::view(found, key).0["collections"].clone(),
            None => method["objects"]["collections"].clone(),
        };
        for c in collections.as_array().into_iter().flatten() {
            if let Some(r) = c["ref"].as_str() {
                each(format!("collection:{r}"), method, c.clone());
            }
        }
    }
}

/// The scope keys a call showed: those its methods section lists.
pub(crate) fn shown(context: &Value) -> BTreeSet<String> {
    let mut out = BTreeSet::new();
    keys(&context["methods"], |k, _, _| {
        out.insert(k);
    });
    out
}

/// The digest of what defines each key in the run's method profiles: a method by its name, description, template,
/// query source and refs; a source by its stored card's digest (its name when it has no card); a reviewed query by its
/// entry; a collection by its reference, title, summary and file names. Sizes, samples and counts are left out, so
/// ordinary growth does not mark a lesson stale.
pub(crate) fn definitions(profiles: &Value) -> BTreeMap<String, String> {
    let mut out = BTreeMap::new();
    keys(profiles, |k, method, item| {
        let value = match k.split_once(':').map(|(kind, _)| kind) {
            Some("method") => digest(
                &json!({"name": method["name"], "description": method["description"],
                "template": method["template"], "query_source": method["query"]["source"],
                "input_refs": method["input_refs"], "tool_refs": method["tool_refs"]}),
            ),
            Some("source") => method["query_card"]["sha256"]
                .as_str()
                .map_or_else(|| digest(&method["query"]["source"]), str::to_owned),
            Some("tool") => digest(&item),
            Some("collection") => digest(&json!({"ref": item["ref"], "title": item["title"],
                "summary": item["summary"], "files": item["files"]})),
            _ => return,
        };
        out.insert(k, value);
    });
    out
}

/// What code records for a scoped lesson when it is committed: the current digest of each key it names.
pub(crate) fn snapshot(
    scope: &BTreeMap<String, String>,
    definitions: &BTreeMap<String, String>,
) -> BTreeMap<String, String> {
    scope
        .iter()
        .filter_map(|(kind, key)| {
            let id = format!("{kind}:{key}");
            definitions.get(&id).map(|d| (id, d.clone()))
        })
        .collect()
}

/// Every reply-level fault in the lessons' scoped fields, named by lesson and field, never by the reply's text:
/// a scope key or kind the call did not show, a condition ref it did not show, an outcome class outside the codes, a
/// dispute of a lesson the call did not number, or a field only code writes.
pub(crate) fn faults(
    lessons: &[Lesson],
    context: &Value,
    evidence: &std::collections::HashSet<String>,
) -> Result<(), CodeRefused> {
    let shown = shown(context);
    let numbered: BTreeSet<u64> = context["earlier_readings"]["lessons"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|l| l["n"].as_u64())
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
                if !KINDS.contains(&kind.as_str()) {
                    names.push(format!(
                        "{name} scope.{}: not one of method, source, collection, tool",
                        super::reply_schema::field_name(kind)
                    ));
                } else if !shown.contains(&format!("{kind}:{key}")) {
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
        let unshown: Vec<String> = lesson
            .condition
            .iter()
            .enumerate()
            .filter(|(_, r)| !evidence.contains(*r))
            .map(|(n, _)| (n + 1).to_string())
            .collect();
        if !unshown.is_empty() {
            names.push(format!(
                "{name} condition: ref {} was not shown",
                unshown.join(", ")
            ));
        }
        if lesson
            .outcome_class
            .as_deref()
            .is_some_and(|c| !outcome_class(c))
        {
            names.push(format!(
                "{name} outcome_class: not accepted or a refusal or check code"
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

/// Adds a turn's admitted lessons to the checkpoint: each scoped lesson gets its snapshot, and a disputing lesson's
/// evidence becomes the disputed lesson's counter refs, at most `LESSON_COUNTER_REFS_MAX`, first kept.
pub(crate) fn commit(lessons: &mut Vec<Lesson>, added: &[Lesson], profiles: &Value) {
    let definitions = definitions(profiles);
    for lesson in added {
        let mut lesson = lesson.clone();
        lesson.validated_snapshot = lesson.scope.as_ref().map(|s| snapshot(s, &definitions));
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

/// How the compiled context shows the checkpoint view's lessons: a scoped lesson whose keys are not all shown is left
/// out and counted; the snapshot is replaced by `stale: true` when a key's definition changed or is gone. Returns the
/// lessons to show, stale ones after the rest in their order, and how many were out of scope.
pub(crate) fn place(
    lessons: Vec<Value>,
    shown: &BTreeSet<String>,
    definitions: &BTreeMap<String, String>,
) -> (Vec<Value>, u64) {
    let (mut fresh, mut stale, mut out_of_scope) = (Vec::new(), Vec::new(), 0_u64);
    for mut lesson in lessons {
        let in_scope = lesson["scope"].as_object().is_none_or(|scope| {
            scope.iter().all(|(kind, key)| {
                key.as_str()
                    .is_some_and(|k| shown.contains(&format!("{kind}:{k}")))
            })
        });
        if !in_scope {
            out_of_scope += 1;
            continue;
        }
        let Some(object) = lesson.as_object_mut() else {
            continue;
        };
        let changed = object.remove("validated_snapshot").is_some_and(|snapshot| {
            snapshot.as_object().is_none_or(|entries| {
                entries
                    .iter()
                    .any(|(id, d)| definitions.get(id).map(String::as_str) != d.as_str())
            })
        });
        if changed {
            object.insert("stale".into(), json!(true));
            stale.push(lesson);
        } else {
            fresh.push(lesson);
        }
    }
    fresh.extend(stale);
    (fresh, out_of_scope)
}

#[cfg(test)]
#[path = "lesson_scope_tests.rs"]
mod tests;
