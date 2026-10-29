//! Typed, bounded catalog ranking. Search order is advisory; the run's current methods remain authority.
//! The projection stays in the host. Only the order of existing compiled method cards may change.
use super::{knowledge, screen};
use reqwest::Url;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};
use uuid::Uuid;

pub const MAX_BYTES: usize = 1024;
const MAX_RECORDS: usize = 8;
const MAX_HITS: usize = 12;

/// Discovery is useful before work starts. An empty visible plan after archiving is not a new run.
/// Missing optional history is empty; malformed state never enables extra context or a search.
pub fn initial_plan(view: &Value, results: &Value) -> bool {
    fn empty_list(value: &Value) -> bool {
        value.is_null() || value.as_array().is_some_and(Vec::is_empty)
    }
    fn zero(value: &Value) -> bool {
        value.is_null() || value.as_u64() == Some(0)
    }
    if !view["plan"].as_array().is_some_and(Vec::is_empty)
        || !results.is_object()
        || !empty_list(&results["results"])
        || !results["earlier_findings"].is_null()
    {
        return false;
    }
    let pages = &results["plan_pages"];
    pages.is_null()
        || (pages.is_object()
            && zero(&pages["pages"])
            && zero(&pages["steps"])
            && empty_list(&pages["keys"])
            && empty_list(&pages["tasks"])
            && empty_list(&pages["newest"]))
}

/// The intermediate is held by the host until compilation against the current run's methods.
pub fn found(found: &knowledge::Found, lab: &str) -> Value {
    json!({"status": "found", "lab": lab, "query_sha256": found.query_sha256,
        "hits": found.hits.iter().take(MAX_HITS).map(|h| json!({
            "id": h.id, "kind": h.kind, "scope": h.scope, "data_class": h.data_class,
            "title": h.title, "source_uri": h.source_uri, "locator": h.locator
        })).collect::<Vec<_>>()})
}

fn profile_address(uri: &str) -> Option<String> {
    let url = Url::parse(uri).ok()?;
    let parts: Vec<_> = url.path_segments()?.collect();
    (url.scheme() == "broker"
        && url.host_str().is_some()
        && url.username().is_empty()
        && url.password().is_none()
        && url.port().is_none()
        && url.query().is_none()
        && url.fragment().is_none()
        && parts.len() == 2
        && parts[0] == "profile")
        .then(|| parts[1].to_owned())
}

fn text(value: &Value, max_chars: usize) -> Option<String> {
    let value = value.as_str()?;
    if value.trim().is_empty() || value.len() > 4096 || screen::screen(value).is_some() {
        return None;
    }
    Some(value.chars().take(max_chars).collect())
}

/// Compile whole typed records within a bounded host-side projection, including its wrapper.
/// This projection supplies method ranking and an audit receipt; it never enters model context.
/// Current method descriptions and every other compiled context field remain unchanged.
pub fn compile(read: &Value, profiles: &Value, room: usize) -> (Option<Value>, Value) {
    let status = read["status"].as_str().unwrap_or("not_configured");
    let status = match status {
        "found" | "no_match" | "unavailable" => status,
        _ => "not_configured",
    };
    let mut receipt = json!({"status": status, "query_sha256": read["query_sha256"], "records": [], "rejected": 0, "left_out": 0});
    if status != "found" {
        if let Some(code @ ("knowledge_refused" | "knowledge_unavailable" | "knowledge_invalid")) =
            read["code"].as_str()
        {
            receipt["code"] = json!(code);
        }
        return (None, receipt);
    }
    let Some(lab) = read["lab"]
        .as_str()
        .filter(|s| !s.is_empty() && *s != "commons" && *s != "clusterops")
    else {
        receipt["status"] = json!("invalid");
        return (None, receipt);
    };
    let methods: BTreeMap<_, _> = profiles
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|p| Some((p["key"].as_str()?, p)))
        .collect();
    let hits: Vec<_> = read["hits"]
        .as_array()
        .into_iter()
        .flatten()
        .take(MAX_HITS)
        .collect();
    let mut records = Vec::new();
    let mut recorded = Vec::new();
    let mut rejected = 0;
    let mut left_out = read["hits"]
        .as_array()
        .map_or(0, Vec::len)
        .saturating_sub(hits.len());
    let mut seen = BTreeSet::new();
    let make = |records: &Vec<Value>| json!({"records": records});
    for h in hits {
        let entry = (|| {
            let id = h["id"].as_str()?.parse::<Uuid>().ok()?;
            let own = h["scope"] == lab;
            let class = h["data_class"].as_str()?;
            let mut entry = match h["kind"].as_str()? {
                "lab" if own && class == "method" && h["locator"]["lab"] == lab => {
                    json!({"kind": "lab", "name": text(&h["title"], 160)?})
                }
                "tool" if own && class == "method" => {
                    let key = profile_address(h["source_uri"].as_str()?)?;
                    let method = methods.get(key.as_str())?;
                    json!({"kind": "method", "name": text(&method["name"], 160)?, "method_keys": [key]})
                }
                "starting_point" if own && class == "method" => {
                    let offered: BTreeSet<_> = h["locator"]["methods"]
                        .as_array()?
                        .iter()
                        .filter_map(Value::as_str)
                        .filter(|key| methods.contains_key(key))
                        .collect();
                    if offered.is_empty() {
                        return None;
                    }
                    json!({"kind": "starting_point", "name": text(&h["title"], 160)?, "method_keys": offered})
                }
                "dataset"
                    if (own && class == "lab_database")
                        || (h["scope"] == "commons" && class == "public_reference") =>
                {
                    let source = h["locator"]["source"].as_str()?;
                    let permitted: Vec<_> = methods
                        .iter()
                        .filter(|(_, p)| p["query"]["source"] == source)
                        .map(|(key, _)| *key)
                        .collect();
                    if permitted.is_empty() {
                        return None;
                    }
                    json!({"kind": "dataset", "name": text(&h["title"], 160)?, "method_keys": permitted,
                        "source_key": source})
                }
                _ => return None,
            };
            entry["ref"] = json!(format!("knowledge://{id}"));
            Some((id, entry))
        })();
        let Some((id, entry)) = entry else {
            rejected += 1;
            continue;
        };
        if !seen.insert(id) {
            rejected += 1;
            continue;
        }
        records.push(entry.clone());
        if records.len() > MAX_RECORDS
            || serde_json::to_vec(&make(&records)).map_or(usize::MAX, |v| v.len())
                > room.min(MAX_BYTES)
        {
            records.pop();
            left_out += 1;
            continue;
        }
        recorded.push(json!({"id": id, "kind": entry["kind"],
            "sha256": hex::encode(Sha256::digest(serde_json::to_vec(&entry).unwrap_or_default()))}));
    }
    receipt["rejected"] = json!(rejected);
    receipt["left_out"] = json!(left_out);
    receipt["records"] = json!(recorded);
    receipt["status"] = json!(if !records.is_empty() {
        "shown"
    } else if left_out > 0 {
        "none_fit"
    } else {
        "no_match"
    });
    ((!records.is_empty()).then(|| make(&records)), receipt)
}

/// Stable permutation only: never add, remove or edit a card in the completed model context.
/// Equal-rank and unmatched cards keep their existing order. The receipt stays outside the model request.
pub fn order_methods(methods: &mut Value, selected: &Value) -> Value {
    fn digest(value: &Value) -> String {
        hex::encode(Sha256::digest(
            serde_json::to_vec(value).unwrap_or_default(),
        ))
    }
    let before = digest(methods);
    let mut ranks = BTreeMap::new();
    for (rank, record) in selected["records"]
        .as_array()
        .into_iter()
        .flatten()
        .enumerate()
    {
        for key in record["method_keys"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
        {
            ranks.entry(key).or_insert(rank);
        }
    }
    let mut matched = 0;
    if let Some(cards) = methods.as_array_mut() {
        matched = cards
            .iter()
            .filter(|card| {
                card["key"]
                    .as_str()
                    .is_some_and(|key| ranks.contains_key(key))
            })
            .count();
        cards.sort_by_key(|card| {
            card["key"]
                .as_str()
                .and_then(|key| ranks.get(key))
                .copied()
                .unwrap_or(usize::MAX)
        });
    }
    let after = digest(methods);
    json!({"before_sha256": before, "after_sha256": after,
        "changed": before != after, "matched_methods": matched})
}
