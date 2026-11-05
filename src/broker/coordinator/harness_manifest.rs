//! The harness surface manifest: the index of every surface that shapes the coordinator's model calls, computed once
//! at start from its arguments, model configuration, profile, lead system text and knowledge configuration. Each
//! surface says whether a measured proposal may change it (`editable`) and whether that change needs a person
//! (`person_required`); the fixed controls are listed with `editable: false` so a reader sees them and knows they are
//! out of reach. The manifest holds no secret, path, endpoint or research value: digests, numbers, lab keys and names.
//!
//! Its canonical JSON (RFC 8785) is stored once by digest (migration 089, which verifies the digest), and every turn
//! record names it as `harness_manifest_sha256`, so each decision can be traced to the exact harness it ran under.
use anyhow::{Result, bail};
use serde_json::{Map, Value, json};
use sha2::{Digest, Sha256};

use super::config::{CoordinatorArgs, SECTION_ITEMS_MAX, SectionBudgets};
use super::model::Model;
use super::supervisor;
use crate::broker::canonical::canonical_json;
use crate::broker::knowledge::KnowledgeService;
use crate::broker::library::LibraryService;

/// The manifest's schema version.
pub(super) const SCHEMA_VERSION: u32 = 1;
/// The largest manifest migration 089 stores.
pub(super) const MANIFEST_BYTES: usize = 65_536;

/// Controls no proposal may edit: the checks, guards, grants, isolation, custody and acceptance rules.
const FIXED: [&str; 8] = [
    "step_checks",
    "number_binding",
    "sql_guard",
    "grants",
    "isolation_and_egress",
    "custody_and_receipts",
    "acceptance_rule",
    "held_out_keys",
];

pub(crate) struct Manifest {
    /// The canonical JSON stored and digested.
    pub(super) bytes: Vec<u8>,
    /// `sha256:<hex>` of `bytes`.
    pub(super) sha256: String,
}

fn number(value: usize, range: Option<(usize, usize)>, person_required: bool) -> Value {
    match range {
        // No override is valid when the lowest allowed value passes the highest (a small lead budget leaves a section
        // only its default): the surface is fixed at its value.
        Some((low, high)) if low > high => {
            json!({"kind": "number", "value": value, "editable": false})
        }
        Some((low, high)) => {
            json!({"kind": "number", "value": value, "range": [low, high], "editable": true,
            "person_required": person_required})
        }
        None => {
            json!({"kind": "number", "value": value, "editable": true, "person_required": person_required})
        }
    }
}

/// The served model name, by digest only: a name can be a path, a host or an address, and the manifest never holds one.
/// The digest still changes when the model does.
fn model_name(name: &str) -> Value {
    json!({"kind": "string", "sha256": digest(name.as_bytes()), "editable": true, "person_required": true})
}

fn list(values: &[String], person_required: bool) -> Value {
    let mut sorted = values.to_vec();
    sorted.sort();
    sorted.dedup();
    json!({"kind": "list", "value": sorted, "editable": true, "person_required": person_required})
}

fn digest(bytes: &[u8]) -> String {
    format!("sha256:{}", hex::encode(Sha256::digest(bytes)))
}

/// The manifest for this coordinator. Two coordinators with the same inputs compute the same bytes and digest.
pub(super) fn compute(
    args: &CoordinatorArgs,
    model: &Model,
    knowledge: Option<&KnowledgeService>,
    library: Option<&LibraryService>,
) -> Result<Manifest> {
    let config = &model.config;
    let system = model.lead_system.as_bytes();
    let budget = config.lead_budget(system.len());
    let defaults = SectionBudgets::default();
    let sections = config.section_budgets();
    let mut surfaces = Map::new();
    surfaces.insert(
        "lead_system_text".into(),
        json!({"kind": "text", "sha256": digest(system), "bytes": system.len(),
            "source": if args.lead_system_file.is_some() { "lead_system_file" } else { "compiled" },
            "editable": true, "person_required": false}),
    );
    surfaces.insert("model.name".into(), model_name(&config.model));
    surfaces.insert(
        "model.max_input_bytes".into(),
        number(config.max_input_bytes, None, false),
    );
    surfaces.insert(
        "model.max_output_tokens".into(),
        number(config.max_output_tokens, Some((512, 65_536)), false),
    );
    surfaces.insert(
        "model.context_window_tokens".into(),
        json!({"kind": "number", "value": config.context_window_tokens, "editable": false}),
    );
    surfaces.insert(
        "model.timeout_seconds".into(),
        number(config.timeout_seconds as usize, Some((10, 900)), false),
    );
    let byte_max = budget / 2;
    for (name, value, default) in [
        (
            "query_relations_bytes",
            sections.query_relations_bytes,
            defaults.query_relations_bytes,
        ),
        (
            "data_card_bytes",
            sections.data_card_bytes,
            defaults.data_card_bytes,
        ),
        (
            "literature_bytes",
            sections.literature_bytes,
            defaults.literature_bytes,
        ),
        (
            "past_research_bytes",
            sections.past_research_bytes,
            defaults.past_research_bytes,
        ),
        (
            "own_query_bytes",
            sections.own_query_bytes,
            defaults.own_query_bytes,
        ),
        (
            "prior_query_bytes",
            sections.prior_query_bytes,
            defaults.prior_query_bytes,
        ),
        (
            "tool_card_bytes",
            sections.tool_card_bytes,
            defaults.tool_card_bytes,
        ),
        (
            "lab_notes_bytes",
            sections.lab_notes_bytes,
            defaults.lab_notes_bytes,
        ),
        (
            "answerability_bytes",
            sections.answerability_bytes,
            defaults.answerability_bytes,
        ),
        (
            "analyses_bytes",
            sections.analyses_bytes,
            defaults.analyses_bytes,
        ),
        (
            "working_memory_bytes",
            sections.working_memory_bytes,
            defaults.working_memory_bytes,
        ),
    ] {
        surfaces.insert(
            format!("model.sections.{name}"),
            number(value, Some((default, byte_max)), false),
        );
    }
    for (name, value, default) in [
        (
            "literature_max",
            sections.literature_max,
            defaults.literature_max,
        ),
        (
            "past_research_max",
            sections.past_research_max,
            defaults.past_research_max,
        ),
    ] {
        surfaces.insert(
            format!("model.sections.{name}"),
            number(value, Some((default, SECTION_ITEMS_MAX)), false),
        );
    }
    surfaces.insert(
        "model.settings".into(),
        json!({"kind": "object", "sha256": digest(&canonical_json(&Value::Object(config.settings.clone()))),
            "editable": true, "person_required": false}),
    );
    surfaces.insert(
        "model.profile".into(),
        match &model.profile {
            Some(p) => {
                json!({"kind": "object", "name": p.name, "sha256": p.sha256,
                "editable": true, "person_required": false})
            }
            None => {
                json!({"kind": "object", "value": null, "editable": true, "person_required": false})
            }
        },
    );
    surfaces.insert(
        "supervisor.bounds".into(),
        json!({"kind": "object", "source": "code", "editable": true, "person_required": false, "value": {
            "repeats_to_narrow": supervisor::REPEATS_TO_NARROW, "repeats_to_pause": supervisor::REPEATS_TO_PAUSE,
            "refusals_to_pause": supervisor::REFUSALS_TO_PAUSE,
            "failed_checks_to_narrow": supervisor::FAILED_CHECKS_TO_NARROW,
            "failed_checks_to_pause": supervisor::FAILED_CHECKS_TO_PAUSE,
            "idle_turns_to_narrow": supervisor::IDLE_TURNS_TO_NARROW,
            "idle_turns_to_pause": supervisor::IDLE_TURNS_TO_PAUSE}}),
    );
    // Which labs each feature reaches. Reading papers before a person accepts them is a person's decision.
    for (name, labs, person_required) in [
        ("lead_text.labs", &args.lead_text_labs, false),
        (
            "lead_text.unreviewed_labs",
            &args.lead_text_unreviewed_labs,
            true,
        ),
        (
            "literature_claims.labs",
            &args.literature_claims_labs,
            false,
        ),
        ("research_catalog.labs", &args.research_catalog_labs, false),
        ("lead_reading.labs", &args.lead_reading_labs, false),
        (
            "literature_review.labs",
            &args.literature_review_labs,
            false,
        ),
        ("supervisor.labs", &args.supervisor_labs, false),
        ("orientation.labs", &args.orientation_labs, false),
        ("working_memory.labs", &args.working_memory_labs, false),
    ] {
        surfaces.insert(name.into(), list(labs, person_required));
    }
    surfaces.insert(
        "lead_reading.bytes".into(),
        number(args.lead_reading_bytes, None, false),
    );
    surfaces.insert(
        "literature_review.bytes".into(),
        number(args.literature_review_bytes, None, false),
    );
    surfaces.insert(
        "lead_study_fields".into(),
        list(&args.lead_study_fields, true),
    );
    surfaces.insert(
        "retrieval.generation".into(),
        match knowledge {
            Some(k) => json!({"kind": "string", "per_lab": true, "configured": true,
                "value": k.default_generation(), "labs": k.lab_generations(), "editable": true,
                "person_required": false}),
            None => json!({"kind": "string", "per_lab": true, "configured": false, "value": null, "labs": {},
                "editable": true, "person_required": false}),
        },
    );
    // Which labs search the knowledge service and which have a library corpus; without a service, none.
    surfaces.insert(
        "knowledge.labs".into(),
        json!({"kind": "list", "configured": knowledge.is_some(),
            "value": knowledge.map(KnowledgeService::labs).unwrap_or_default(), "editable": true,
            "person_required": true}),
    );
    surfaces.insert(
        "library.labs".into(),
        json!({"kind": "list", "configured": library.is_some(),
            "value": library.map(LibraryService::labs).unwrap_or_default(), "editable": true,
            "person_required": true}),
    );
    // Tool groups are catalog rows granted per lab, versioned in the database and named on each task; the manifest
    // lists the surface, not a copy of the catalog.
    surfaces.insert(
        "tool_groups".into(),
        json!({"kind": "list", "per_lab": true, "source": "catalog", "editable": true, "person_required": true}),
    );
    for name in FIXED {
        surfaces.insert(name.into(), json!({"editable": false}));
    }
    let manifest = json!({"schema_version": SCHEMA_VERSION, "coordinator": args.run_lane,
        "broker_version": env!("CARGO_PKG_VERSION"), "surfaces": surfaces});
    let bytes = canonical_json(&manifest);
    if bytes.len() > MANIFEST_BYTES {
        bail!("harness_manifest_too_large")
    }
    let sha256 = digest(&bytes);
    Ok(Manifest { bytes, sha256 })
}

#[cfg(test)]
#[path = "harness_manifest_tests.rs"]
mod tests;
