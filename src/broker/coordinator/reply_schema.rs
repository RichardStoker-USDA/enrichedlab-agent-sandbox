//! The reply's JSON shape: the fields each object may carry and the check that names any other.
use super::*;

/// The kinds of value a proposal field may hold, as its serde shape accepts them.
#[derive(Clone, Copy)]
pub(super) enum Shape {
    Text,
    TextOrNull,
    List,
    Map,
    TextList,
    Row,
    StatedRows,
    Settings,
    AnswerWord,
    LessonWord,
    PartWord,
    NumberList,
    Count,
    Number,
}

pub(super) type Fields = &'static [(&'static str, bool, Shape)];

pub(super) const PROPOSAL_FIELDS: Fields = &[
    ("action", true, Shape::Text),
    ("answer", false, Shape::AnswerWord),
    ("summary", true, Shape::Text),
    ("findings", false, Shape::List),
    ("table_statements", false, Shape::List),
    ("tables", false, Shape::List),
    ("actions", false, Shape::List),
    ("lessons", false, Shape::List),
    ("parts", false, Shape::List),
];

pub(super) const PART_FIELDS: Fields = &[
    ("part", true, Shape::Text),
    ("state", true, Shape::PartWord),
    ("findings", false, Shape::NumberList),
    ("table_statements", false, Shape::NumberList),
    ("tables", false, Shape::NumberList),
    ("reason", false, Shape::TextOrNull),
];

pub(super) const ACTION_FIELDS: Fields = &[
    ("action_key", true, Shape::Text),
    ("title", true, Shape::Text),
    ("profile", true, Shape::Text),
    ("objective", true, Shape::Text),
    ("rationale", true, Shape::Text),
    ("code", false, Shape::TextOrNull),
    ("settings", false, Shape::Settings),
    ("literature_refs", false, Shape::TextList),
    ("sql", false, Shape::TextOrNull),
    ("reviewed_query", false, Shape::Map),
    ("analysis", false, Shape::Map),
    ("expect", true, Shape::Map),
];

/// A step's expectation and its parts.
pub(super) const EXPECT_FIELDS: Fields = &[
    ("table", false, Shape::Text),
    ("columns", false, Shape::TextList),
    ("rows", false, Shape::Map),
    ("not_null", false, Shape::TextList),
    ("unique", false, Shape::TextList),
    ("ranges", false, Shape::List),
];

pub(super) const EXPECT_ROWS_FIELDS: Fields =
    &[("min", false, Shape::Count), ("max", false, Shape::Count)];

pub(super) const EXPECT_RANGE_FIELDS: Fields = &[
    ("column", true, Shape::Text),
    ("min", false, Shape::Number),
    ("max", false, Shape::Number),
    ("basis", true, Shape::Text),
];

pub(super) const FINDING_FIELDS: Fields = &[
    ("statement", true, Shape::Text),
    ("bindings", true, Shape::Map),
];

pub(super) const BINDING_FIELDS: Fields = &[
    ("ref", true, Shape::Text),
    ("table", true, Shape::Text),
    ("row", true, Shape::Row),
    ("column", true, Shape::Text),
];

pub(super) const TABLE_STATEMENT_FIELDS: Fields = &[
    ("ref", true, Shape::Text),
    ("table", true, Shape::Text),
    ("rows", true, Shape::StatedRows),
    ("columns", true, Shape::TextList),
    ("lead_in", true, Shape::Text),
];

/// An answer table as the lead writes it: its row count is code's, so a reply that gives one is refused.
pub(super) const ANSWER_TABLE_FIELDS: Fields = &[
    ("ref", true, Shape::Text),
    ("table", true, Shape::Text),
    ("columns", true, Shape::TextList),
    ("lead_in", true, Shape::Text),
];

pub(super) const LESSON_FIELDS: Fields = &[
    ("statement", true, Shape::Text),
    ("kind", true, Shape::LessonWord),
    ("evidence_refs", true, Shape::TextList),
];

pub(super) const SCHEMA_NAMES_MAX: usize = 12;

pub(super) fn fits(shape: Shape, value: &Value) -> bool {
    match shape {
        Shape::Text => value.is_string(),
        Shape::TextOrNull => value.is_string() || value.is_null(),
        Shape::List => value.is_array(),
        Shape::Map => value.is_object(),
        Shape::TextList => value
            .as_array()
            .is_some_and(|a| a.iter().all(Value::is_string)),
        Shape::Row => value.as_u64().is_some(),
        Shape::StatedRows => {
            value.is_string()
                || value
                    .as_array()
                    .is_some_and(|rows| rows.iter().all(|r| r.as_u64().is_some()))
        }
        Shape::Settings => {
            value.is_null()
                || value
                    .as_object()
                    .is_some_and(|m| m.values().all(|v| v.is_string() || v.is_number()))
        }
        Shape::AnswerWord => matches!(
            value.as_str(),
            Some("answered" | "partly_answered" | "not_answered" | "not_yet")
        ),
        Shape::LessonWord => matches!(
            value.as_str(),
            Some("observation" | "hypothesis" | "constraint")
        ),
        Shape::PartWord => matches!(
            value.as_str(),
            Some("supported" | "partial" | "unsupported" | "open" | "awaiting")
        ),
        Shape::NumberList => value
            .as_array()
            .is_some_and(|a| a.iter().all(|n| n.as_u64().is_some())),
        Shape::Count => value.as_u64().is_some(),
        Shape::Number => value.as_f64().is_some(),
    }
}

/// A field name is structure, never content: it is kept when it is at most 40 lowercase letters, digits and
/// underscores, and is otherwise written `other`.
pub(super) fn field_name(key: &str) -> &str {
    let plain = (1..=40).contains(&key.len())
        && key
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_');
    if plain { key } else { "other" }
}

pub(super) fn check_fields(value: &Value, fields: Fields, at: &str, names: &mut BTreeSet<String>) {
    let path = |field: &str| {
        if at.is_empty() {
            field.to_owned()
        } else {
            format!("{at}.{field}")
        }
    };
    let Some(object) = value.as_object() else {
        names.insert(format!(
            "invalid:{}",
            if at.is_empty() { "reply" } else { at }
        ));
        return;
    };
    for (field, required, shape) in fields {
        match object.get(*field) {
            None if *required => {
                names.insert(format!("missing:{}", path(field)));
            }
            Some(v) if !fits(*shape, v) => {
                names.insert(format!("invalid:{}", path(field)));
            }
            _ => {}
        }
    }
    for key in object.keys() {
        if !fields.iter().any(|(field, _, _)| field == key) {
            names.insert(format!("unexpected:{}", path(field_name(key))));
        }
    }
}

/// Why a lead reply does not fit the proposal's shape, named by its fields: `missing:<field>`, `invalid:<field>` (a
/// value of the wrong kind) and `unexpected:<field>` (a field the shape does not have), nested fields written as
/// `actions.<field>`. The next lead call reads them; unlike refused code names they are only field names, so the
/// deferred turn's log line carries them too. Empty when every field fits, which leaves the fixed reason alone.
pub fn proposal_schema_names(value: &Value) -> Vec<String> {
    let mut names = BTreeSet::new();
    check_fields(value, PROPOSAL_FIELDS, "", &mut names);
    for (list, fields) in [
        ("actions", ACTION_FIELDS),
        ("findings", FINDING_FIELDS),
        ("table_statements", TABLE_STATEMENT_FIELDS),
        ("tables", ANSWER_TABLE_FIELDS),
        ("lessons", LESSON_FIELDS),
        ("parts", PART_FIELDS),
    ] {
        for item in value[list].as_array().into_iter().flatten() {
            check_fields(item, fields, list, &mut names);
            if list == "actions"
                && let Some(query) = item.get("reviewed_query")
            {
                check_fields(
                    query,
                    &[
                        ("tool", true, Shape::Text),
                        ("parameters", true, Shape::Map),
                    ],
                    "actions.reviewed_query",
                    &mut names,
                );
            }
            if list == "actions"
                && let Some(expect) = item.get("expect")
            {
                check_fields(expect, EXPECT_FIELDS, "actions.expect", &mut names);
                if let Some(rows) = expect.get("rows") {
                    check_fields(rows, EXPECT_ROWS_FIELDS, "actions.expect.rows", &mut names);
                }
                for range in expect["ranges"].as_array().into_iter().flatten() {
                    check_fields(
                        range,
                        EXPECT_RANGE_FIELDS,
                        "actions.expect.ranges",
                        &mut names,
                    );
                }
            }
            if list == "findings" {
                for binding in item["bindings"]
                    .as_object()
                    .into_iter()
                    .flat_map(|b| b.values())
                {
                    check_fields(binding, BINDING_FIELDS, "findings.bindings", &mut names);
                }
            }
        }
    }
    names.into_iter().take(SCHEMA_NAMES_MAX).collect()
}
