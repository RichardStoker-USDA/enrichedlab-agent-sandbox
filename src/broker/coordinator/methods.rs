//! Configured methods: templates, views, query relations and request digests.
use super::*;

/// Each code profile's reviewed template, from the claim's allowed profile descriptions. A
/// configured method's template is not a code template: its block comes from its settings.
pub fn profile_templates(profiles: &Value) -> HashMap<String, String> {
    profiles
        .as_array()
        .map(|profiles| {
            profiles
                .iter()
                .filter(|p| p["template"]["contract"].get("configuration").is_none())
                .filter_map(|p| {
                    Some((
                        p["key"].as_str()?.to_owned(),
                        p["template"]["template_ref"].as_str()?.to_owned(),
                    ))
                })
                .collect()
        })
        .unwrap_or_default()
}

/// A configured method: its template and its configuration, or why the
/// declared configuration cannot be used.
#[derive(Debug, Clone)]
pub struct ConfiguredMethod {
    pub template_ref: String,
    pub configuration: std::result::Result<Configuration, &'static str>,
}

/// Every allowed profile whose template contract declares a configuration.
pub fn profile_configurations(profiles: &Value) -> HashMap<String, ConfiguredMethod> {
    profiles
        .as_array()
        .map(|profiles| {
            profiles
                .iter()
                .filter_map(|p| {
                    let configuration = Configuration::from_contract(&p["template"]["contract"])?;
                    Some((
                        p["key"].as_str()?.to_owned(),
                        ConfiguredMethod {
                            template_ref: p["template"]["template_ref"].as_str()?.to_owned(),
                            configuration,
                        },
                    ))
                })
                .collect()
        })
        .unwrap_or_default()
}

/// The methods as the lead and the methods check see them: a configured method shows its settings and choices by label
/// in place of its template, and the template-analysis method its catalog within `analyses_room` bytes, ranked by
/// `ranking` (`analysis_view`): whole contracts first, the other analyses by id and label.
pub fn methods_view(profiles: &Value, analyses_room: usize) -> Value {
    let unranked = crate::broker::analysis_view::Ranking {
        question: "",
        around: "",
        focus: &[],
    };
    methods_view_ranked(profiles, analyses_room, &unranked)
}

/// `methods_view` with the template-analysis catalog ranked for one call.
pub fn methods_view_ranked(
    profiles: &Value,
    analyses_room: usize,
    ranking: &crate::broker::analysis_view::Ranking,
) -> Value {
    Value::Array(
        profiles
            .as_array()
            .into_iter()
            .flatten()
            .map(|p| {
                let mut method = p.clone();
                // A source's schema card reaches the lead as its own section, trimmed to its share.
                if let Some(object) = method.as_object_mut() {
                    object.remove("query_card");
                }
                if let Some(configured) = Configuration::from_contract(&p["template"]["contract"]) {
                    if let Some(object) = method.as_object_mut() {
                        object.remove("template");
                    }
                    method["settings"] = configured.map(|c| c.view()).unwrap_or_else(|_| json!({}));
                }
                // A lab objects method shows its collections without the store details the commit uses.
                if let Some(found) = method
                    .as_object_mut()
                    .and_then(|o| o.remove(crate::broker::lead_objects::FOUND_FIELD))
                {
                    let key = p["key"].as_str().unwrap_or_default();
                    let (mut view, held) = crate::broker::lead_objects::view(&found, key);
                    if !held.is_empty() {
                        view["withheld"] = json!(held.len());
                    }
                    method["objects"] = view;
                }
                // The template-analysis method shows its catalog compactly; a step may choose only these.
                if let Some(catalog) = method
                    .as_object_mut()
                    .and_then(|o| o.remove("analysis_catalog"))
                {
                    let view = crate::broker::analysis_view::analyses_view(
                        &catalog,
                        analyses_room,
                        ranking,
                    );
                    method["analyses"] = json!(view.analyses);
                    if !view.analyses.is_empty() {
                        method["analyses_note"] =
                            json!(crate::broker::analysis_view::SETTINGS_NOTE);
                    }
                    if !view.other.is_empty() {
                        method["other_analyses"] = json!(view.other);
                        method["other_analyses_note"] =
                            json!(crate::broker::analysis_view::OTHER_ANALYSES_NOTE);
                    }
                    if view.omitted > 0 {
                        method["analyses_omitted"] = json!(view.omitted);
                    }
                }
                method
            })
            .collect(),
    )
}

/// Each query source too large for `room` shown in part, so a lab's own database fits a lead call. Before
/// deciding whether it fits, screen each relation and column description that the lead could read; a held field becomes
/// a content-free marker, and its source, location, rule and hash enter the turn record. A source whose screened
/// relations fit keeps them whole. A larger one keeps under `relations`, in the order of `relation_order`, each relation
/// with its columns and description while they fit, lists the rest by name under `other_relations` in the source's own
/// order, and says so in `relations_note`; the guard admits a query of any of them either way. Returns the relations each
/// cut method shows, by method key, the record of each cut and the screened fields' records.
pub(super) fn query_relations_view(
    methods: &mut Value,
    question: &str,
    context: &str,
    own_queries: &Value,
    room: usize,
) -> (HashMap<String, HashSet<String>>, Vec<Value>, Vec<Value>) {
    let size = |v: &Value| serde_json::to_vec(v).map_or(usize::MAX, |b| b.len());
    // The project's admitted queries, newest first (`newest_tasks` lists the newest last).
    let own_sql: Vec<&str> = own_queries
        .as_array()
        .into_iter()
        .flatten()
        .rev()
        .filter_map(|q| q["admitted_sql"].as_str())
        .collect();
    let (mut shown_by_method, mut records, mut withheld) = (HashMap::new(), Vec::new(), Vec::new());
    for method in methods.as_array_mut().into_iter().flatten() {
        let key = method.get("key").and_then(Value::as_str).map(str::to_owned);
        let Some(query) = method.get_mut("query").filter(|q| q.is_object()) else {
            continue;
        };
        let source = query
            .get("source")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned();
        let mut fields_withheld = Vec::new();
        if let Some(relations) = query.get_mut("relations").and_then(Value::as_array_mut) {
            screen_relation_fields(screen::source_key(&source), relations, &mut fields_withheld);
        }
        // Query-source boundary (QuerySource::view): every string value and key, including source metadata and
        // custody records, passes screen::screen before ranking, sizing or sending. The methods specialist and
        // retries inherit this view; revision feedback never rebuilds it from the original registration.
        let (mut screened, held) = screen::screen_view(
            &json!({"query": query, "withheld": fields_withheld}),
            screen::source_key(key.as_deref().unwrap_or_default()),
        );
        *query = screened["query"].take();
        withheld.extend(
            screened["withheld"]
                .as_array()
                .into_iter()
                .flatten()
                .cloned(),
        );
        withheld.extend(held);
        let source = screen::source_key(&source);
        let query = query.as_object_mut().expect("query view object");
        if query.get("relations").map_or(usize::MAX, &size) <= room {
            continue;
        }
        let relations: Vec<Value> = query
            .get("relations")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        let total = relations.len();
        let note = |shown: usize| {
            format!(
                "This source declares {total} relations, more than one call can show. The {shown} under relations, \
                 those this project's queries read and those the question names first, show their columns; \
                 other_relations names the rest. A query may read any of them: to see a listed relation's columns, \
                 query it (SELECT * FROM <relation> ORDER BY 1 LIMIT 5), and later calls show the relations this \
                 project's queries read first."
            )
        };
        let names: Vec<&str> = relations
            .iter()
            .map(|r| r["name"].as_str().unwrap_or_default())
            .collect();
        // The frame with every relation listed by name and the widest count in the note: the most it takes.
        let frame = size(
            &json!({"relations": [], "other_relations": names, "relations_note": note(total)}),
        );
        let mut free = room.saturating_sub(frame);
        let mut chosen = vec![false; total];
        let order = relation_order(&relations, question, context, &own_sql);
        for i in &order {
            let bytes = size(&relations[*i]).saturating_add(1);
            if bytes <= free {
                chosen[*i] = true;
                free -= bytes;
            }
        }
        let shown: Vec<Value> = order
            .iter()
            .filter(|i| chosen[**i])
            .map(|i| relations[*i].clone())
            .collect();
        let shown_names: Vec<&str> = order
            .iter()
            .filter(|i| chosen[**i])
            .map(|i| names[*i])
            .collect();
        let others: Vec<&str> = (0..total)
            .filter(|i| !chosen[*i])
            .map(|i| names[i])
            .collect();
        records.push(json!({"source": source, "relations": total,
            "shown": shown_names, "listed": others.len()}));
        if let Some(key) = key {
            shown_by_method.insert(key, shown_names.iter().map(|n| (*n).to_owned()).collect());
        }
        let count = shown.len();
        query.insert("relations".to_owned(), Value::Array(shown));
        query.insert("other_relations".to_owned(), json!(others));
        query.insert("relations_note".to_owned(), json!(note(count)));
    }
    (shown_by_method, records, withheld)
}

/// Project registered JSON onto the model's relation schema before screening, measuring or ordering. The lead's
/// compile_context uses this view; method_review_context clones it for the methods specialist and review retries.
/// Revision feedback and lead retries reuse that compiled view, never QuerySource::relations. The raw relations stay
/// with the SQL guard and query_facts. No extra column keys are needed: query_facts::column_types and
/// source_card::CardColumn read only name and type; relation_order also reads description. Older registrations may
/// put a column description under `about`; it becomes `description` only when that field is absent.
pub(super) fn screen_relation_fields(
    source: &str,
    relations: &mut [Value],
    withheld: &mut Vec<Value>,
) {
    // Registration restricts relation identifiers but not column names or types. Screen those strings too, and use
    // an ordinal to locate any withheld name so its text cannot leak through the custody record.
    let location = |value: &Value, index: usize| {
        value
            .as_str()
            .filter(|s| screen::screen(s).is_none())
            .map(str::to_owned)
            .unwrap_or_else(|| format!("[{index}]"))
    };
    for (r, relation) in relations.iter_mut().enumerate() {
        let name = location(&relation["name"], r);
        let mut shown =
            relation_text_fields(source, &name, None, relation, &["name", "about"], withheld);
        let mut dropped = serde_json::Map::new();
        let mut dropped_names = BTreeMap::new();
        for (key, value) in relation.as_object().into_iter().flatten() {
            if !matches!(key.as_str(), "name" | "about" | "columns") {
                dropped.insert(key.clone(), value.clone());
                dropped_names.insert(key.clone(), key.clone());
            }
        }
        let mut columns = Vec::new();
        for (c, column) in relation["columns"]
            .as_array()
            .into_iter()
            .flatten()
            .enumerate()
        {
            let column_name = location(&column["name"], c);
            let mut column_shown = relation_text_fields(
                source,
                &name,
                Some(&column_name),
                column,
                &["name", "type"],
                withheld,
            );
            let description_field = if column.get("description").is_some() {
                Some("description")
            } else if column.get("about").is_some() {
                Some("about")
            } else {
                None
            };
            if let Some(field) = description_field
                && let Some(value) =
                    relation_text_value(source, &name, Some(&column_name), column, field, withheld)
            {
                column_shown["description"] = value;
            }
            columns.push(column_shown);
            for (key, value) in column.as_object().into_iter().flatten() {
                let used_about = description_field == Some("about") && key == "about";
                if !matches!(key.as_str(), "name" | "type" | "description") && !used_about {
                    dropped.insert(format!("columns[{c}].{key}"), value.clone());
                    dropped_names.insert(format!("columns[{c}].{key}"), key.clone());
                }
            }
        }
        shown["columns"] = json!(columns);
        if !dropped.is_empty() {
            // Audit boundary: dropped names themselves pass the same screen. A failed name is recorded only by
            // ordinal and hash, so a key repeating withheld description text cannot put it back into the record.
            let fields: Vec<Value> = dropped_names
                .iter()
                .enumerate()
                .map(|(i, (location, key))| {
                    if screen::screen(key).is_some() {
                        json!({"ordinal": i + 1, "field_sha256": sha(key.as_bytes())})
                    } else {
                        json!(location)
                    }
                })
                .collect();
            let mut record = screen::withheld_field(
                source,
                &name,
                None,
                "fields_dropped",
                "fields_dropped",
                &json!(dropped),
            );
            record["fields"] = json!(fields);
            withheld.push(record);
        }
        *relation = shown;
    }
}

pub(super) fn relation_text_fields(
    source: &str,
    relation: &str,
    column: Option<&str>,
    object: &Value,
    fields: &[&str],
    withheld: &mut Vec<Value>,
) -> Value {
    let mut shown = json!({});
    for field in fields {
        if let Some(value) = relation_text_value(source, relation, column, object, field, withheld)
        {
            shown[*field] = value;
        }
    }
    shown
}

fn relation_text_value(
    source: &str,
    relation: &str,
    column: Option<&str>,
    object: &Value,
    field: &str,
    withheld: &mut Vec<Value>,
) -> Option<Value> {
    let value = object.get(field)?;
    let reason = value.as_str().map_or(Some("non_text"), screen::screen);
    if let Some(reason) = reason {
        withheld.push(screen::withheld_field(
            source, relation, column, field, reason, value,
        ));
        // Structured descriptions are omitted. A screened string keeps the existing content-free marker.
        value.is_string().then(|| json!({"withheld": true}))
    } else {
        Some(value.clone())
    }
}

/// The order in which a source shown in part offers its relations' columns: first those this project's queries read,
/// the newest query first (a relation counts as read when its name is a word of the admitted text); then by how strongly
/// the question names them, then its constraints and instructions; then the source's own order. A word of the question
/// counts when it has two characters or more, is not only digits and is not a stop word, and weighs 3 in a relation's
/// name, else 2 in a column's name, else 1 in its description or a column's, times the number of relations over the
/// number that hold the word anywhere, so a word few relations hold outranks one most of them hold.
pub(super) fn relation_order(
    relations: &[Value],
    question: &str,
    context: &str,
    own_sql: &[&str],
) -> Vec<usize> {
    let words_of =
        |text: &str| -> HashSet<String> { source_card::words(text).into_iter().collect() };
    let text = |v: &Value| v.as_str().unwrap_or_default().to_owned();
    // Each relation's words by field: its name, its columns' names, its and its columns' descriptions.
    let fields: Vec<[HashSet<String>; 3]> = relations
        .iter()
        .map(|r| {
            let columns: Vec<&Value> = r["columns"].as_array().into_iter().flatten().collect();
            let column_names = columns
                .iter()
                .map(|c| text(&c["name"]))
                .collect::<Vec<_>>()
                .join(" ");
            let descriptions = std::iter::once(text(&r["about"]))
                .chain(columns.iter().map(|c| text(&c["description"])))
                .collect::<Vec<_>>()
                .join(" ");
            [
                words_of(&text(&r["name"])),
                words_of(&column_names),
                words_of(&descriptions),
            ]
        })
        .collect();
    let mut holders: HashMap<&String, u64> = HashMap::new();
    for f in &fields {
        for w in f.iter().flatten().collect::<HashSet<_>>() {
            *holders.entry(w).or_default() += 1;
        }
    }
    let terms = |text: &str| -> HashSet<String> {
        words_of(text)
            .into_iter()
            .filter(|w| {
                w.chars().count() >= 2
                    && !w.chars().all(|c| c.is_ascii_digit())
                    && !source_card::STOP_WORDS.contains(&w.as_str())
            })
            .collect()
    };
    let asked = terms(question);
    let around: HashSet<String> = terms(context).difference(&asked).cloned().collect();
    let n = relations.len() as u64;
    let score = |terms: &HashSet<String>, f: &[HashSet<String>; 3]| -> u64 {
        terms
            .iter()
            .filter_map(|w| {
                let field = [3_u64, 2, 1]
                    .into_iter()
                    .zip(f)
                    .find(|(_, words)| words.contains(w))?
                    .0;
                Some(field.saturating_mul(n.saturating_mul(64) / holders.get(w).copied()?.max(1)))
            })
            .fold(0_u64, u64::saturating_add)
    };
    // The newest query that reads each relation, by its place in `own_sql`.
    let tokens: Vec<HashSet<String>> = own_sql
        .iter()
        .map(|sql| {
            sql.to_lowercase()
                .split(|c: char| !(c.is_ascii_alphanumeric() || c == '_'))
                .filter(|t| !t.is_empty())
                .map(str::to_owned)
                .collect()
        })
        .collect();
    let read_by: Vec<Option<usize>> = relations
        .iter()
        .map(|r| {
            let name = r["name"].as_str().unwrap_or_default();
            tokens.iter().position(|t| t.contains(name))
        })
        .collect();
    let asked_scores: Vec<u64> = fields.iter().map(|f| score(&asked, f)).collect();
    let around_scores: Vec<u64> = fields.iter().map(|f| score(&around, f)).collect();
    let mut order: Vec<usize> = (0..relations.len()).collect();
    order.sort_by_key(|i| {
        (
            read_by[*i].is_none(),
            read_by[*i].unwrap_or(0),
            std::cmp::Reverse(asked_scores[*i]),
            std::cmp::Reverse(around_scores[*i]),
            *i,
        )
    });
    order
}

/// The staged dataset's column names for each template profile, from its contract.
/// Admission lets a block refer to them.
pub fn profile_columns(profiles: &Value) -> HashMap<String, Vec<String>> {
    profiles
        .as_array()
        .map(|profiles| {
            profiles
                .iter()
                .filter_map(|p| {
                    let columns = p["template"]["contract"]["dataset"]["columns"].as_object()?;
                    Some((
                        p["key"].as_str()?.to_owned(),
                        columns.keys().cloned().collect(),
                    ))
                })
                .collect()
        })
        .unwrap_or_default()
}

/// The admitted query's digest is part of the task request, like a block's, so a replay with a different query under
/// the same action cannot pass as the same request.
pub fn query_request_digest(request: &SubmitTaskRequest, query_sha256: &str) -> String {
    let mut digest = Sha256::new();
    digest.update(request_digest(request).as_bytes());
    digest.update([0]);
    digest.update(b"query");
    digest.update([0]);
    digest.update(query_sha256.as_bytes());
    hex::encode(digest.finalize())
}

/// A delegated step as the turn commit receives it: its request digest and, for a step that carries one, its admitted
/// code, template analysis or query. The digest covers what the step carries, so a replay with other code,
/// another analysis or another query under the same action is another request.
pub fn delegated_action(
    req: &SubmitTaskRequest,
    a: &ProposedAction,
    admitted: &HashMap<String, (String, String, codegen::AdmissionReport, Option<Value>)>,
    analyses: &HashMap<String, AdmittedAnalysis>,
    queries: &HashMap<String, AdmittedQuery>,
    objects: &HashMap<String, crate::broker::lead_objects::AdmittedObjects>,
) -> Value {
    let mut action = json!({"action_key":a.action_key,"profile":a.profile,"objective":a.objective,"request_sha256":request_digest(req)});
    if let Some((template, content, report, configuration)) = admitted.get(&a.action_key) {
        action["request_sha256"] = json!(code_request_digest(req, &report.block_sha256));
        action["generated_code"] = json!({"template_ref":template,
            "path":super::input_request::ADMITTED_CODE_PATH,"content":content,
            "sha256":report.block_sha256,"admission":report});
        if let Some(configuration) = configuration {
            action["generated_code"]["configuration"] = configuration.clone();
        }
    }
    if let Some(analysis) = analyses.get(&a.action_key) {
        action["request_sha256"] = json!(analysis_request_digest(req, &analysis.sha256()));
        action["template_analysis"] = analysis.record();
    }
    if let Some(files) = objects.get(&a.action_key) {
        action["request_sha256"] = json!(objects_request_digest(req, &files.sha256()));
        action["objects"] = files.record();
    }
    if let Some(query) = queries.get(&a.action_key) {
        let sha = format!(
            "sha256:{}",
            hex::encode(Sha256::digest(query.admitted.sql.as_bytes()))
        );
        action["request_sha256"] = json!(query_request_digest(req, &sha));
        action["query"] = json!({"source": query.source, "proposed_sql": query.proposed,
            "admitted_sql": query.admitted.sql, "sha256": sha, "guard": sql_guard::GUARD,
            "relations": query.admitted.relations, "row_limit": query.admitted.limit});
        if let Some(record) = &query.reviewed {
            action["query"]["reviewed_query"] = json!(record);
            action["request_sha256"] = json!(query_request_digest(
                req,
                &format!(
                    "{sha}:{}",
                    super::super::reviewed_query::digest(
                        &serde_json::to_vec(record).expect("reviewed record serializes")
                    )
                )
            ));
        }
    }
    action
}

/// The chosen files' digest is part of the task request, so a replay with other files under the same action cannot
/// pass as the same request.
pub fn objects_request_digest(request: &SubmitTaskRequest, objects_sha256: &str) -> String {
    let mut digest = Sha256::new();
    digest.update(request_digest(request).as_bytes());
    digest.update([0]);
    digest.update(b"objects");
    digest.update([0]);
    digest.update(objects_sha256.as_bytes());
    hex::encode(digest.finalize())
}

/// The chosen analysis's digest is part of the task request, so a replay with another analysis under the
/// same action cannot pass as the same request.
pub fn analysis_request_digest(request: &SubmitTaskRequest, analysis_sha256: &str) -> String {
    let mut digest = Sha256::new();
    digest.update(request_digest(request).as_bytes());
    digest.update([0]);
    digest.update(b"template_analysis");
    digest.update([0]);
    digest.update(analysis_sha256.as_bytes());
    hex::encode(digest.finalize())
}

/// The admitted block's digest is part of the task request, so a replay with a
/// different block under the same action cannot pass as the same request.
pub fn code_request_digest(request: &SubmitTaskRequest, block_sha256: &str) -> String {
    let mut digest = Sha256::new();
    digest.update(request_digest(request).as_bytes());
    digest.update([0]);
    digest.update(b"generated_code");
    digest.update([0]);
    digest.update(block_sha256.as_bytes());
    hex::encode(digest.finalize())
}
