//! The research lead's lab objects (migration 083). A run that lists a lab objects method (route `server-objects`) shows
//! the lead, in that method's view, the lab's object collections the knowledge service found for the goal: each
//! collection's reference, title, summary, file count, bytes and the sample of file names the index keeps. A step for
//! that method names one shown collection and 1 to 20 files from its shown sample; the lead chooses names only, and no
//! file byte moves through it. This module builds the view and checks a step against it before the methods review
//! spends a call on it; the turn commit checks the shape again and the worker checks every file against the pinned
//! manifest.
use super::{
    coordinator::CodeRefused,
    knowledge::{self, Found, KnowledgeService},
    screen,
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::collections::{HashMap, HashSet};
use uuid::Uuid;

pub const ROUTE: &str = super::objects_run::ROUTE;
/// The files one step may name (`broker_object_requests` holds 1 to 20).
pub const FILES_MAX: usize = 20;
/// The collections one call shows.
const COLLECTIONS_MAX: usize = 8;
/// The file names one collection shows: the index keeps a sample capped by its locator, at most this many here.
const SAMPLE_MAX: usize = 40;
/// The private field that carries a method's collections with what the commit needs; the lead sees `objects`.
pub const FOUND_FIELD: &str = "lab_objects";
pub const NOTE: &str = "A step for this method names one collection by its ref and 1 to 20 files from that \
     collection's sample, exactly as listed (\"objects\": {\"collection\": \"<ref>\", \"files\": [\"<name>\"]}). The \
     worker keeps each file, checked against the collection's manifest; a delimited text file of at most 50,000 rows \
     also becomes a table that a template analysis can read. You never see a file's contents.";

/// The files a step of a lab objects method reads: a collection the method showed and names from its sample.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ObjectsChoice {
    pub collection: String,
    pub files: Vec<String>,
}

/// A step's files as the checks admitted them, in the terms the turn commit stores (`objects`).
#[derive(Clone, Debug, PartialEq)]
pub struct AdmittedObjects {
    pub collection: Uuid,
    pub bucket: String,
    pub prefix: String,
    pub generation: String,
    pub manifest_sha256: String,
    pub files: Vec<String>,
}

impl AdmittedObjects {
    /// The object the turn commit checks and records.
    pub fn record(&self) -> Value {
        json!({"collection": self.collection, "bucket": self.bucket, "prefix": self.prefix,
            "generation": self.generation, "manifest_sha256": self.manifest_sha256, "files": self.files})
    }

    /// The record's digest, part of the task request so a replay with other files under the same step cannot pass as
    /// the same request.
    pub fn sha256(&self) -> String {
        let bytes = serde_json::to_vec(&self.record()).unwrap_or_default();
        format!("sha256:{}", hex::encode(Sha256::digest(bytes)))
    }
}

fn segment(value: &str) -> bool {
    (1..=64).contains(&value.len())
        && value
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_' || b == b'-')
}

fn lower_hex(value: &str, len: usize) -> bool {
    value.len() == len
        && value
            .bytes()
            .all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
}

/// The whole object collection records a search found, each with what the commit needs; a hit that is not one whole
/// record (another kind, a locator that disagrees with its source, no manifest digest) is left out.
pub fn collections(found: &Found) -> Value {
    let mut out = Vec::new();
    for hit in found.hits.iter().filter(|h| h.kind == "object_collection") {
        let Some(locator) = hit.locator.as_ref() else {
            continue;
        };
        let text = |field: &str| locator[field].as_str().unwrap_or_default();
        let (bucket, prefix, generation) = (text("bucket"), text("prefix"), text("generation"));
        let parts: Vec<&str> = prefix.strip_suffix('/').unwrap_or("").split('/').collect();
        let whole = (3..=63).contains(&bucket.len())
            && bucket
                .bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
            && parts.len() == 3
            && parts.iter().all(|p| segment(p))
            && segment(generation)
            && hit.source_uri.as_deref() == Some(&format!("s3://{bucket}/{prefix}"))
            && hit
                .content_sha256
                .as_deref()
                .is_some_and(|s| lower_hex(s, 64))
            && locator["manifest"].as_str() == Some(&format!("manifests/{generation}.jsonl"));
        if !whole {
            continue;
        }
        let sample: Vec<Value> = locator["sample"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|s| s.as_str())
            .filter(|s| !s.is_empty() && s.len() <= 1024)
            .take(SAMPLE_MAX)
            .map(|s| json!(s))
            .collect();
        out.push(json!({"ref": format!("knowledge://{}", hit.id), "title": hit.title, "summary": hit.summary,
            "files": locator["files"], "bytes": locator["bytes"], "sample": sample,
            "bucket": bucket, "prefix": prefix, "generation": generation,
            "manifest_sha256": hit.content_sha256}));
        if out.len() == COLLECTIONS_MAX {
            break;
        }
    }
    Value::Array(out)
}

/// The lab's object collections the knowledge service finds for the question, for a run with an objects method; none
/// when the lab has no knowledge scope configured or the search fails (the method then shows no collection this call).
pub async fn read(
    service: Option<&KnowledgeService>,
    run: Uuid,
    lab: &str,
    question: &str,
) -> Value {
    let text = knowledge::search_text(question);
    let Some(service) = service.filter(|s| s.configured(lab)) else {
        return json!([]);
    };
    if text.is_empty() {
        return json!([]);
    }
    match service.search_objects(run, lab, &text).await {
        Ok(found) => collections(&found),
        Err(error) => {
            tracing::warn!(%run, code = error.code(), event = "lab_objects_search");
            json!([])
        }
    }
}

/// The run's methods with each lab objects method marked by its route and carrying the collections found for it.
pub fn with_lab_objects(methods: &Value, object_methods: &[String], found: &Value) -> Value {
    let mut methods = methods.clone();
    for m in methods.as_array_mut().into_iter().flatten() {
        if m["key"]
            .as_str()
            .is_some_and(|k| object_methods.iter().any(|o| o == k))
        {
            m["route"] = json!(ROUTE);
            m[FOUND_FIELD] = found.clone();
        }
    }
    methods
}

/// The objects method's view for the lead: each collection's ref, title, summary, counts and sample, every string
/// screened as any source-derived text is, and a note on how a step names files. Returns the view and the screen's
/// records of what it withheld.
pub fn view(found: &Value, method_key: &str) -> (Value, Vec<Value>) {
    let shown: Vec<Value> = found
        .as_array()
        .into_iter()
        .flatten()
        .map(|c| {
            json!({"ref": c["ref"], "title": c["title"], "summary": c["summary"], "files": c["files"],
                "bytes": c["bytes"], "sample": c["sample"]})
        })
        .collect();
    let (screened, held) = screen::screen_view(
        &json!({"collections": shown}),
        screen::source_key(method_key),
    );
    let mut view = screened;
    view["note"] = json!(NOTE);
    (view, held)
}

/// The objects method among the run's methods, by key, with the collections found for it.
fn objects_methods(methods: &Value) -> HashMap<&str, &Value> {
    methods
        .as_array()
        .into_iter()
        .flatten()
        .filter(|m| m["route"] == ROUTE)
        .filter_map(|m| Some((m["key"].as_str()?, &m[FOUND_FIELD])))
        .collect()
}

/// Checks each step against what the context showed: a step of an objects method carries `objects` and nothing else
/// a method takes, names a collection its method showed, and 1 to 20 distinct files from that collection's shown
/// sample; any other step carries no `objects`.
pub fn admit(
    actions: &[super::coordinator::ProposedAction],
    methods: &Value,
    context: &Value,
) -> Result<HashMap<String, AdmittedObjects>, CodeRefused> {
    let found = objects_methods(methods);
    let mut admitted = HashMap::new();
    for a in actions {
        let refused = |code: &'static str, extra: &[&str]| CodeRefused {
            code,
            names: std::iter::once(a.action_key.clone())
                .chain(extra.iter().map(|s| (*s).to_owned()))
                .collect(),
        };
        let choice = match (found.get(a.profile.as_str()), &a.objects) {
            (None, None) => continue,
            (None, Some(_)) => return Err(refused("proposal_objects_unexpected", &[])),
            (Some(_), None) => return Err(refused("proposal_objects_missing", &[])),
            (Some(_), Some(choice)) => choice,
        };
        if a.code.is_some() || a.settings.is_some() || a.sql.is_some() || a.analysis.is_some() {
            return Err(refused("proposal_objects_unexpected", &[]));
        }
        // What this call showed for the method, after screening: a withheld name cannot be chosen.
        let shown = context["methods"]
            .as_array()
            .into_iter()
            .flatten()
            .find(|m| m["key"] == a.profile.as_str())
            .and_then(|m| m["objects"]["collections"].as_array())
            .and_then(|c| c.iter().find(|c| c["ref"] == choice.collection.as_str()))
            .ok_or_else(|| refused("proposal_objects_collection_unknown", &[&choice.collection]))?;
        let full = found[a.profile.as_str()]
            .as_array()
            .into_iter()
            .flatten()
            .find(|c| c["ref"] == choice.collection.as_str())
            .ok_or_else(|| refused("proposal_objects_collection_unknown", &[&choice.collection]))?;
        if choice.files.is_empty() || choice.files.len() > FILES_MAX {
            return Err(refused("proposal_objects_files_invalid", &[]));
        }
        let sample: HashSet<&str> = shown["sample"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
            .collect();
        let mut seen = HashSet::new();
        for file in &choice.files {
            if !seen.insert(file.as_str()) {
                return Err(refused("proposal_objects_files_invalid", &[]));
            }
            if !sample.contains(file.as_str()) {
                return Err(refused("proposal_objects_file_unknown", &[]));
            }
        }
        let text = |field: &str| full[field].as_str().unwrap_or_default().to_owned();
        let collection = choice
            .collection
            .strip_prefix("knowledge://")
            .and_then(|id| id.parse::<Uuid>().ok())
            .ok_or_else(|| refused("proposal_objects_collection_unknown", &[&choice.collection]))?;
        admitted.insert(
            a.action_key.clone(),
            AdmittedObjects {
                collection,
                bucket: text("bucket"),
                prefix: text("prefix"),
                generation: text("generation"),
                manifest_sha256: text("manifest_sha256"),
                files: choice.files.clone(),
            },
        );
    }
    Ok(admitted)
}

#[cfg(test)]
#[path = "lead_objects_tests.rs"]
mod tests;
