//! Stable project-package identities and content inventories.
//!
//! The database serializes revision allocation. This module owns the deterministic
//! parts: safe names, the content fingerprint, the manifest's bounded entity lists,
//! and the digest-only comparison with the preceding revision.

use std::collections::BTreeMap;

use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use time::{OffsetDateTime, format_description::FormatItem, macros::format_description};

use super::evidence_package::ArtifactSource;

const PROJECT_SLUG_BYTES: usize = 48;
const PACKAGE_TIME: &[FormatItem<'static>] =
    format_description!("[year][month][day]T[hour][minute][second]Z");

/// The three revision chains have different disclosure ceilings. Values are
/// persisted and passed back to PostgreSQL, which independently revalidates them.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DisclosureScope {
    OrdinaryWithReasoning,
    OrdinaryWithoutReasoning,
    Inspection,
}

impl DisclosureScope {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::OrdinaryWithReasoning => "ordinary_with_reasoning",
            Self::OrdinaryWithoutReasoning => "ordinary_without_reasoning",
            Self::Inspection => "inspection",
        }
    }

    fn parse(value: &str) -> Option<Self> {
        match value {
            "ordinary_with_reasoning" => Some(Self::OrdinaryWithReasoning),
            "ordinary_without_reasoning" => Some(Self::OrdinaryWithoutReasoning),
            "inspection" => Some(Self::Inspection),
            _ => None,
        }
    }
}

/// Derive the disclosure ceiling from the same snapshot whose inventory will be
/// registered. PostgreSQL rejects the registration if access changed meanwhile.
pub fn disclosure_scope(snapshot: &Value) -> Result<DisclosureScope, &'static str> {
    let object = snapshot.as_object().ok_or("snapshot_invalid")?;
    let inspecting = object
        .get("requester")
        .and_then(|requester| requester.get("inspection_expires_at"))
        .is_some_and(|expires| !expires.is_null());
    if inspecting {
        Ok(DisclosureScope::Inspection)
    } else if object.get("reasoning_access").and_then(Value::as_bool) == Some(true) {
        Ok(DisclosureScope::OrdinaryWithReasoning)
    } else {
        Ok(DisclosureScope::OrdinaryWithoutReasoning)
    }
}

/// The immutable registry row selected or inserted for this project content.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PackageRevision {
    pub disclosure_scope: DisclosureScope,
    pub revision: u64,
    pub created_at: String,
    pub content_sha256: String,
    pub inventory: Value,
    pub previous_revision: Option<u64>,
    pub previous_inventory: Value,
}

impl PackageRevision {
    /// Decode the fixed-shape value returned by `broker_register_project_package`.
    pub fn from_record(record: &Value, inventory: Value) -> Result<Self, &'static str> {
        let disclosure_scope = record["disclosure_scope"]
            .as_str()
            .and_then(DisclosureScope::parse)
            .ok_or("package_record_invalid")?;
        let revision = record["revision"]
            .as_u64()
            .ok_or("package_record_invalid")?;
        let created_at = record["created_at"]
            .as_str()
            .ok_or("package_record_invalid")?
            .to_owned();
        OffsetDateTime::parse(&created_at, &time::format_description::well_known::Rfc3339)
            .map_err(|_| "package_record_invalid")?;
        let content_sha256 = record["content_sha256"]
            .as_str()
            .filter(|digest| is_digest(digest))
            .ok_or("package_record_invalid")?
            .to_owned();
        if content_sha256 != inventory_sha256(&inventory)? {
            return Err("package_record_invalid");
        }
        let previous_revision = match record.get("previous_revision") {
            Some(Value::Number(value)) => value.as_u64(),
            Some(Value::Null) | None => None,
            _ => return Err("package_record_invalid"),
        };
        let previous_inventory = record
            .get("previous_inventory")
            .cloned()
            .unwrap_or_else(|| json!({}));
        if !previous_inventory.is_object() {
            return Err("package_record_invalid");
        }
        Ok(Self {
            disclosure_scope,
            revision,
            created_at,
            content_sha256,
            inventory,
            previous_revision,
            previous_inventory,
        })
    }

    #[cfg(test)]
    pub fn fixture(revision: u64, created_at: &str, inventory: Value) -> Self {
        Self {
            disclosure_scope: DisclosureScope::OrdinaryWithReasoning,
            revision,
            created_at: created_at.to_owned(),
            content_sha256: inventory_sha256(&inventory).unwrap(),
            inventory,
            previous_revision: revision.checked_sub(1).filter(|value| *value > 0),
            previous_inventory: json!({}),
        }
    }
}

fn is_digest(value: &str) -> bool {
    value.len() == 64 && value.bytes().all(|byte| byte.is_ascii_hexdigit())
}

fn sha256(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}

fn value_sha256(value: &Value) -> Result<String, &'static str> {
    serde_json::to_vec(value)
        .map(|bytes| sha256(&bytes))
        .map_err(|_| "snapshot_invalid")
}

/// The package view excludes the caller and earlier package-export audit rows.
/// Both are effects of downloading, not changes to the project being packaged.
pub fn normalized_snapshot(snapshot: &Value) -> Result<Value, &'static str> {
    let mut normalized = snapshot.clone();
    let disclosure_scope = disclosure_scope(&normalized)?;
    let object = normalized.as_object_mut().ok_or("snapshot_invalid")?;
    object.remove("requester");
    object.remove("snapshot_at");
    object.insert(
        "disclosure_scope".into(),
        Value::String(disclosure_scope.as_str().to_owned()),
    );
    // Default closed: an inconsistent database response must not let exact
    // reasoning enter either the revision inventory or the package planner.
    if disclosure_scope != DisclosureScope::OrdinaryWithReasoning {
        object.insert("reasoning_access".into(), Value::Bool(false));
        object.insert("reasoning".into(), json!([]));
        object.remove("literature_reviews");
        if let Some(papers) = object.get_mut("papers").and_then(Value::as_array_mut) {
            for paper in papers {
                let paper_id = paper["paper_id"]
                    .as_str()
                    .ok_or("snapshot_invalid")?
                    .to_owned();
                *paper = json!({"paper_id": paper_id});
            }
        }
    }
    if let Some(access) = object.get_mut("access_history") {
        let rows = access.as_array_mut().ok_or("snapshot_invalid")?;
        rows.retain(|row| row["kind"] != "evidence_package_exported");
    }
    Ok(normalized)
}

fn insert(
    inventory: &mut BTreeMap<String, String>,
    kind: &str,
    id: String,
    value: &Value,
) -> Result<(), &'static str> {
    if id.is_empty()
        || id.len() > 180
        || !id.bytes().all(|byte| {
            byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-' | b'.' | b'/' | b':')
        })
    {
        return Err("snapshot_invalid");
    }
    let key = format!("{kind}:{id}");
    if inventory.insert(key, value_sha256(value)?).is_some() {
        return Err("snapshot_invalid");
    }
    Ok(())
}

/// Digest-only inventory for revision allocation. It covers every package record
/// class and the availability of every retained artifact, without storing research
/// text or values in the package registry.
pub fn build_inventory(
    snapshot: &Value,
    artifacts: &[ArtifactSource],
) -> Result<Value, &'static str> {
    let snapshot = normalized_snapshot(snapshot)?;
    let mut inventory = BTreeMap::new();
    let project = snapshot.get("project").ok_or("snapshot_invalid")?;
    let project_id = project["project_id"].as_str().ok_or("snapshot_invalid")?;
    insert(&mut inventory, "project", project_id.to_owned(), project)?;
    for checkpoint in snapshot["checkpoints"]
        .as_array()
        .ok_or("snapshot_invalid")?
    {
        let revision = checkpoint["revision"].as_i64().ok_or("snapshot_invalid")?;
        insert(&mut inventory, "plan", format!("r{revision}"), checkpoint)?;
    }
    for run in snapshot["research_runs"]
        .as_array()
        .ok_or("snapshot_invalid")?
    {
        let run_id = run["run"]["run_id"].as_str().ok_or("snapshot_invalid")?;
        insert(&mut inventory, "run", run_id.to_owned(), &run["run"])?;
        for turn in run["turns"].as_array().ok_or("snapshot_invalid")? {
            let number = turn["turn"].as_i64().ok_or("snapshot_invalid")?;
            insert(&mut inventory, "turn", format!("{run_id}/{number}"), turn)?;
        }
        for event in run["events"].as_array().ok_or("snapshot_invalid")? {
            let sequence = event["sequence"].as_i64().ok_or("snapshot_invalid")?;
            insert(
                &mut inventory,
                "run_event",
                format!("{run_id}/{sequence}"),
                event,
            )?;
        }
    }
    for review in snapshot["literature_reviews"]
        .as_array()
        .into_iter()
        .flatten()
    {
        let run = review["run_id"].as_str().ok_or("snapshot_invalid")?;
        let digest = review["inputs_sha256"].as_str().ok_or("snapshot_invalid")?;
        insert(
            &mut inventory,
            "literature_review",
            format!("{run}/{digest}"),
            review,
        )?;
    }
    for task in snapshot["tasks"].as_array().ok_or("snapshot_invalid")? {
        let task_id = task["task_id"].as_str().ok_or("snapshot_invalid")?;
        insert(&mut inventory, "task", task_id.to_owned(), task)?;
    }
    for paper in snapshot["papers"].as_array().into_iter().flatten() {
        let paper_id = paper["paper_id"].as_str().ok_or("snapshot_invalid")?;
        insert(&mut inventory, "paper", paper_id.to_owned(), paper)?;
    }
    for (source_key, kind, id_key) in [
        ("reasoning", "reasoning", "sha256"),
        ("digests", "digest", "digest_sha256"),
        ("task_origins", "task_origin", "task_id"),
    ] {
        for (index, item) in snapshot[source_key]
            .as_array()
            .into_iter()
            .flatten()
            .enumerate()
        {
            let id = item[id_key]
                .as_str()
                .map(str::to_owned)
                .unwrap_or_else(|| index.to_string());
            let id = match kind {
                "reasoning" => format!(
                    "{}/{}/{}/{}",
                    item["run_id"].as_str().unwrap_or("run"),
                    item["turn"].as_i64().unwrap_or_default(),
                    item["role"].as_str().unwrap_or("model"),
                    id.trim_start_matches("sha256:")
                ),
                "digest" => format!(
                    "{}/{}",
                    item["task_id"].as_str().unwrap_or("task"),
                    item["artifact_id"].as_str().unwrap_or(id.as_str())
                ),
                _ => format!(
                    "{}/{}/{}",
                    id,
                    item["run_id"].as_str().unwrap_or("run"),
                    item["turn"].as_i64().unwrap_or_default()
                ),
            };
            insert(&mut inventory, kind, id, item)?;
        }
    }
    for (index, review) in snapshot["reviews"]
        .as_array()
        .into_iter()
        .flatten()
        .enumerate()
    {
        let id = review["review_id"]
            .as_str()
            .map(str::to_owned)
            .unwrap_or_else(|| {
                format!("{}/{}", review["task_id"].as_str().unwrap_or("task"), index)
            });
        insert(&mut inventory, "review", id, review)?;
    }
    for code in snapshot["generated_code"].as_array().into_iter().flatten() {
        let task_id = code["task_id"].as_str().ok_or("snapshot_invalid")?;
        insert(&mut inventory, "generated_code", task_id.to_owned(), code)?;
    }
    for access in snapshot["access_history"]
        .as_array()
        .ok_or("snapshot_invalid")?
    {
        let event_id = access["event_id"].as_str().ok_or("snapshot_invalid")?;
        insert(&mut inventory, "access", event_id.to_owned(), access)?;
    }
    for artifact in artifacts {
        let state = json!({
            "task_id": artifact.task_id,
            "artifact_id": artifact.artifact_id,
            "relative_path": artifact.relative_path,
            "media_type": artifact.media_type,
            "label": artifact.label,
            "size_bytes": artifact.size_bytes,
            "sha256": artifact.sha256,
            "available": artifact.path.is_some(),
            "mismatch": artifact.mismatch,
        });
        insert(
            &mut inventory,
            "artifact",
            artifact.artifact_id.clone(),
            &state,
        )?;
    }
    Ok(Value::Object(
        inventory
            .into_iter()
            .map(|(key, digest)| (key, Value::String(digest)))
            .collect(),
    ))
}

pub fn inventory_sha256(inventory: &Value) -> Result<String, &'static str> {
    if !inventory.is_object() {
        return Err("snapshot_invalid");
    }
    value_sha256(inventory)
}

fn split_item(key: &str, digest: &str) -> Value {
    let (kind, id) = key.split_once(':').unwrap_or(("unknown", key));
    json!({"kind": kind, "id": id, "sha256": digest})
}

/// The entity lists the package requires. IDs and digests only keep the manifest
/// useful without duplicating the records it already checksums.
pub fn manifest_contents(inventory: &Value) -> Result<Value, &'static str> {
    let object = inventory.as_object().ok_or("snapshot_invalid")?;
    let list = |kind: &str| {
        object
            .iter()
            .filter(|(key, _)| key.starts_with(&format!("{kind}:")))
            .map(|(key, digest)| split_item(key, digest.as_str().unwrap_or_default()))
            .collect::<Vec<_>>()
    };
    Ok(json!({
        "runs": list("run"),
        "turns": list("turn"),
        "tasks": list("task"),
        "papers": list("paper"),
        "literature_reviews": list("literature_review"),
    }))
}

/// Added, removed and digest-changed inventory entries since the preceding
/// package in this disclosure scope. Each list is stable by kind and id.
pub fn changes_since(revision: &PackageRevision) -> Result<Value, &'static str> {
    let current = revision.inventory.as_object().ok_or("snapshot_invalid")?;
    let previous = revision
        .previous_inventory
        .as_object()
        .ok_or("snapshot_invalid")?;
    let mut added = Vec::new();
    let mut removed = Vec::new();
    let mut changed = Vec::new();
    for (key, digest) in current {
        let Some(digest) = digest.as_str().filter(|value| is_digest(value)) else {
            return Err("snapshot_invalid");
        };
        match previous.get(key).and_then(Value::as_str) {
            None => added.push(split_item(key, digest)),
            Some(before) if before != digest => {
                let (kind, id) = key.split_once(':').unwrap_or(("unknown", key));
                changed.push(json!({
                    "kind": kind,
                    "id": id,
                    "before_sha256": before,
                    "sha256": digest,
                }));
            }
            Some(_) => {}
        }
    }
    for (key, digest) in previous {
        if !current.contains_key(key) {
            removed.push(split_item(key, digest.as_str().unwrap_or_default()));
        }
    }
    Ok(json!({
        "previous_revision": revision.previous_revision,
        "added": added,
        "removed": removed,
        "changed": changed,
    }))
}

fn ascii_slug(value: &str, limit: usize, fallback: &str) -> String {
    let mut slug = String::new();
    let mut separator = false;
    for byte in value.bytes() {
        if byte.is_ascii_alphanumeric() {
            if separator && !slug.is_empty() && slug.len() < limit {
                slug.push('_');
            }
            if slug.len() < limit {
                slug.push((byte as char).to_ascii_lowercase());
            }
            separator = false;
        } else if !slug.is_empty() {
            separator = true;
        }
        if slug.len() >= limit {
            break;
        }
    }
    let slug = slug.trim_matches('_');
    if slug.is_empty() {
        fallback.to_owned()
    } else {
        slug.to_owned()
    }
}

pub fn project_slug(title: &str) -> String {
    ascii_slug(title, PROJECT_SLUG_BYTES, "project")
}

fn lab_id(value: &str) -> Option<&str> {
    (value.len() <= 64
        && value.bytes().enumerate().all(|(index, byte)| {
            byte.is_ascii_lowercase()
                || byte.is_ascii_digit()
                || (index > 0 && matches!(byte, b'_' | b'-'))
        })
        && value.as_bytes().first().is_some_and(u8::is_ascii_lowercase))
    .then_some(value)
}

/// `<lab>_<project slug>_<first 8 of project id>_r<revision>_<UTC timestamp>`.
pub fn package_name(project: &Value, revision: &PackageRevision) -> Result<String, &'static str> {
    let lab = project["group_key"]
        .as_str()
        .and_then(lab_id)
        .ok_or("snapshot_invalid")?;
    let title = project["title"].as_str().ok_or("snapshot_invalid")?;
    let project_id = project["project_id"]
        .as_str()
        .filter(|value| value.len() >= 8)
        .ok_or("snapshot_invalid")?;
    let created = OffsetDateTime::parse(
        &revision.created_at,
        &time::format_description::well_known::Rfc3339,
    )
    .map_err(|_| "package_record_invalid")?;
    let timestamp = created
        .to_offset(time::UtcOffset::UTC)
        .format(PACKAGE_TIME)
        .map_err(|_| "package_record_invalid")?;
    Ok(format!(
        "{}_{}_{}_r{}_{}",
        lab,
        project_slug(title),
        &project_id[..8].to_ascii_lowercase(),
        revision.revision,
        timestamp
    ))
}

/// Put the registry's stable timestamp into the package snapshot and remove
/// request-only fields before deterministic planning.
pub fn package_snapshot(
    snapshot: &Value,
    revision: &PackageRevision,
) -> Result<Value, &'static str> {
    if disclosure_scope(snapshot)? != revision.disclosure_scope {
        return Err("package_scope_changed");
    }
    let mut snapshot = normalized_snapshot(snapshot)?;
    let object = snapshot.as_object_mut().ok_or("snapshot_invalid")?;
    object.insert(
        "snapshot_at".into(),
        Value::String(revision.created_at.clone()),
    );
    Ok(snapshot)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Barrier, Mutex};

    #[test]
    fn names_use_lab_title_id_revision_and_compact_utc() {
        let inventory = json!({});
        let revision = PackageRevision::fixture(12, "2026-10-15T20:45:12.987654Z", inventory);
        let project = json!({
            "group_key": "example-lab",
            "title": "  β-Carotene / IL-6: Follow-up!!!  ",
            "project_id": "A1B2C3D4-1111-4222-8333-444444444444"
        });
        assert_eq!(
            package_name(&project, &revision).unwrap(),
            "example-lab_carotene_il_6_follow_up_a1b2c3d4_r12_20261015T204512Z"
        );
        let mut invalid = project;
        invalid["group_key"] = json!("Example Lab");
        assert_eq!(package_name(&invalid, &revision), Err("snapshot_invalid"));
    }

    #[test]
    fn project_slugs_are_ascii_underscore_bounded_and_have_a_fallback() {
        assert_eq!(project_slug("a---b___c"), "a_b_c");
        assert_eq!(project_slug("日本語"), "project");
        assert_eq!(project_slug(&"A".repeat(80)).len(), PROJECT_SLUG_BYTES);
        assert!(
            project_slug("hyphen-free")
                .bytes()
                .all(|byte| { byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'_' })
        );
    }

    #[test]
    fn changes_are_sorted_and_distinguish_added_removed_and_changed() {
        let current = json!({
            "paper:p2": "b".repeat(64),
            "run:r2": "c".repeat(64),
            "task:t1": "d".repeat(64),
        });
        let mut revision = PackageRevision::fixture(2, "2026-10-15T20:45:12Z", current);
        revision.previous_revision = Some(1);
        revision.previous_inventory = json!({
            "paper:p1": "a".repeat(64),
            "run:r2": "e".repeat(64),
            "task:t1": "d".repeat(64),
        });
        let changes = changes_since(&revision).unwrap();
        assert_eq!(changes["added"][0]["id"], "p2");
        assert_eq!(changes["removed"][0]["id"], "p1");
        assert_eq!(changes["changed"][0]["id"], "r2");
        assert_eq!(changes["previous_revision"], 1);
    }

    #[test]
    fn exports_and_requesters_do_not_advance_content() {
        let base = json!({
            "snapshot_at": "2026-10-15T20:45:12Z",
            "requester": {"principal_id": "private"},
            "project": {"project_id": "11111111-2222-4333-8444-555555555555"},
            "checkpoints": [], "research_runs": [], "tasks": [],
            "access_history": [
                {"event_id": "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa", "kind": "project_created"},
                {"event_id": "bbbbbbbb-bbbb-4bbb-8bbb-bbbbbbbbbbbb", "kind": "evidence_package_exported"}
            ]
        });
        let mut second = base.clone();
        second["snapshot_at"] = json!("2026-10-16T00:00:00Z");
        second["requester"] = json!({"principal_id": "different"});
        second["access_history"].as_array_mut().unwrap().push(json!({
            "event_id": "cccccccc-cccc-4ccc-8ccc-cccccccccccc", "kind": "evidence_package_exported"
        }));
        assert_eq!(
            normalized_snapshot(&base).unwrap(),
            normalized_snapshot(&second).unwrap()
        );
    }

    #[test]
    fn disclosure_scopes_are_closed_and_inspections_drop_reasoning_before_inventory() {
        let mut snapshot = json!({
            "requester": {"inspection_expires_at": null},
            "reasoning_access": true,
            "reasoning": [{
                "run_id": "run", "turn": 1, "role": "lead",
                "sha256": format!("sha256:{}", "a".repeat(64)), "text": "kept"
            }],
            "project": {"project_id": "11111111-2222-4333-8444-555555555555"},
            "checkpoints": [], "research_runs": [], "tasks": [],
            "papers": [{"paper_id": "paper-1", "source_sha256": "b".repeat(64)}],
            "digests": [], "task_origins": [], "reviews": [], "generated_code": [],
            "access_history": []
        });
        assert_eq!(
            disclosure_scope(&snapshot).unwrap(),
            DisclosureScope::OrdinaryWithReasoning
        );
        snapshot["requester"]["inspection_expires_at"] = json!("2099-01-01T00:00:00Z");
        assert_eq!(
            disclosure_scope(&snapshot).unwrap(),
            DisclosureScope::Inspection
        );
        let normalized = normalized_snapshot(&snapshot).unwrap();
        assert_eq!(normalized["disclosure_scope"], "inspection");
        assert_eq!(normalized["reasoning_access"], false);
        assert_eq!(normalized["reasoning"], json!([]));
        assert_eq!(normalized["papers"], json!([{"paper_id": "paper-1"}]));
        assert!(
            build_inventory(&snapshot, &[])
                .unwrap()
                .as_object()
                .unwrap()
                .keys()
                .all(|key| !key.starts_with("reasoning:"))
        );
        snapshot["requester"]["inspection_expires_at"] = Value::Null;
        snapshot["reasoning_access"] = json!(false);
        assert_eq!(
            disclosure_scope(&snapshot).unwrap(),
            DisclosureScope::OrdinaryWithoutReasoning
        );
    }

    #[test]
    fn concurrent_and_interleaved_content_reuses_one_revision() {
        let barrier = Arc::new(Barrier::new(3));
        let registry = Arc::new(Mutex::new((BTreeMap::<String, u64>::new(), 0_u64)));
        let request = |digest: String| {
            let barrier = barrier.clone();
            let registry = registry.clone();
            std::thread::spawn(move || {
                barrier.wait();
                let mut registry = registry.lock().unwrap();
                if let Some(revision) = registry.0.get(&digest) {
                    *revision
                } else {
                    registry.1 += 1;
                    let revision = registry.1;
                    registry.0.insert(digest, revision);
                    revision
                }
            })
        };
        let first = request("a".repeat(64));
        let second = request("a".repeat(64));
        barrier.wait();
        assert_eq!([first.join().unwrap(), second.join().unwrap()], [1, 1]);
        let third = request("b".repeat(64));
        let fourth = request("b".repeat(64));
        barrier.wait();
        assert_eq!([third.join().unwrap(), fourth.join().unwrap()], [2, 2]);
        let first_again = request("a".repeat(64));
        let first_retry = request("a".repeat(64));
        barrier.wait();
        assert_eq!(
            [first_again.join().unwrap(), first_retry.join().unwrap()],
            [1, 1]
        );

        let sql = include_str!("../../migrations/postgres/068_project_package_revisions.sql");
        let lock = sql.find("pg_advisory_xact_lock").unwrap();
        let reuse = sql.find("SELECT * INTO registered").unwrap();
        let latest = sql.find("SELECT * INTO latest").unwrap();
        assert!(
            lock < reuse && reuse < latest,
            "allocation must lock and look up any matching digest before the latest row"
        );
        assert!(sql.contains("UNIQUE (tenant_id, project_id, disclosure_scope, content_sha256)"));
    }
}
