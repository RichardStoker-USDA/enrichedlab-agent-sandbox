use std::{collections::BTreeMap, fs, path::Path};

use serde_json::json;
use tempfile::tempdir;

use super::{
    ArtifactSensitivity, ContextAction, StateAuthority, StateDomain, StateMutation, add_artifact,
    apply_mutation, checkpoint, context_status, export_run, generate_view, init_run,
    install_profile, list_profiles, load_collection, load_run, reconcile_outbox, seal_run,
    search_metadata, seed_continuity_agent, seed_scientific_worker, trace, trace_summary,
    validate_continuity_profile, verify,
};
use crate::config::{ControllerSettings, RuntimeConfig, create_private_dir};

#[test]
fn metadata_search_is_scoped_and_omits_state_values() {
    let temp = tempdir().unwrap();
    let config = test_config(temp.path());
    seed_scientific_worker(
        &config,
        "search-run",
        "alpha-worker",
        "analyze fixture",
        Some("study"),
    )
    .unwrap();
    let page = search_metadata(&config, "alpha-worker", Some("state"), 50).unwrap();
    assert!(page.totals.values().sum::<u64>() > 0);
    assert!(page.hits.iter().all(|hit| hit.category != "artifact"));
    let encoded = serde_json::to_string(&page).unwrap();
    assert!(!encoded.contains("value_json"));
    assert!(search_metadata(&config, "alpha", Some("invalid"), 50).is_err());
}

#[test]
fn explicit_mutation_preserves_unmentioned_state_and_checks_versions() {
    let temp = tempdir().unwrap();
    let config = test_config(temp.path());
    seed_scientific_worker(
        &config,
        "run-one",
        "alpha",
        "analyze fixture",
        Some("study"),
    )
    .unwrap();
    let receipt = apply_mutation(
            &config,
            &StateMutation {
                schema_version: 1,
                run_id: "run-one".into(),
                subject: "alpha".into(),
                collection: "knowledge".into(),
                base_version: 0,
                set: BTreeMap::from([(
                    "/facts/fixture".into(),
                    json!({"statement": "fixture exists", "evidence": ["output:input-manifest.json"], "confidence": 1.0}),
                )]),
                remove: Vec::new(),
                rationale: None,
            },
            "alpha",
            StateAuthority::Model,
        )
        .unwrap();
    assert_eq!(receipt.version, 1);
    let view = generate_view(&config, "run-one", "alpha").unwrap();
    assert_eq!(
        view.collections["knowledge"].value["facts"]["fixture"]["statement"],
        "fixture exists"
    );
    assert!(apply_mutation(
            &config,
            &StateMutation { base_version: 0, ..serde_json::from_value(json!({
                "schema_version": 1,
                "run_id": "run-one",
                "subject": "alpha",
                "collection": "knowledge",
                "base_version": 0,
                "set": {"/facts/stale": {"statement": "stale", "evidence": [], "confidence": 0.1}},
                "remove": [],
                "rationale": null
            })).unwrap() },
            "alpha",
            StateAuthority::Model
        ).is_err());
}

#[test]
fn worker_view_cannot_see_lead_only_coordination_state() {
    let temp = tempdir().unwrap();
    let config = test_config(temp.path());
    seed_scientific_worker(
        &config,
        "run-scope",
        "alpha",
        "analyze fixture",
        Some("study"),
    )
    .unwrap();
    let view = generate_view(&config, "run-scope", "alpha").unwrap();
    assert!(!view.collections.contains_key("coordination"));
    assert!(view.collections.contains_key("execution"));
}

#[test]
fn scientific_seed_is_idempotent_and_model_cannot_change_host_facts() {
    let temp = tempdir().unwrap();
    let config = test_config(temp.path());
    seed_scientific_worker(
        &config,
        "run-owned",
        "alpha",
        "analyze fixture",
        Some("study"),
    )
    .unwrap();
    let repeated = seed_scientific_worker(
        &config,
        "run-owned",
        "alpha",
        "analyze fixture",
        Some("study"),
    )
    .unwrap();
    assert_eq!(repeated.len(), 4);
    for (collection, pointer) in [
        ("execution", "/objective"),
        ("scientific", "/input_refs/fake"),
        ("scientific", "/claim_refs/fake"),
    ] {
        let version = load_collection(&config, "run-owned", "alpha", collection)
            .unwrap()
            .version;
        assert!(
            apply_mutation(
                &config,
                &StateMutation {
                    schema_version: 1,
                    run_id: "run-owned".into(),
                    subject: "alpha".into(),
                    collection: collection.into(),
                    base_version: version,
                    set: BTreeMap::from([(pointer.into(), json!("untrusted"))]),
                    remove: Vec::new(),
                    rationale: None,
                },
                "alpha",
                StateAuthority::Model,
            )
            .is_err()
        );
    }
    let sealed = seal_run(
        &config,
        "run-owned",
        "alpha",
        "test completed",
        "host-controller",
    )
    .unwrap();
    assert_eq!(sealed.status, "sealed");
    assert_eq!(
        seal_run(
            &config,
            "run-owned",
            "alpha",
            "test completed",
            "host-controller",
        )
        .unwrap()
        .status,
        "sealed"
    );
    let knowledge = load_collection(&config, "run-owned", "alpha", "knowledge").unwrap();
    assert!(
        apply_mutation(
            &config,
            &StateMutation {
                schema_version: 1,
                run_id: "run-owned".into(),
                subject: "alpha".into(),
                collection: "knowledge".into(),
                base_version: knowledge.version,
                set: BTreeMap::from([(
                    "/facts/late".into(),
                    json!({"statement": "late", "evidence": [], "confidence": 1.0}),
                )]),
                remove: Vec::new(),
                rationale: None,
            },
            "alpha",
            StateAuthority::Model,
        )
        .is_err()
    );
    assert_eq!(load_run(&config, "run-owned").unwrap().status, "sealed");
}

#[test]
fn context_pressure_creates_explicit_epoch_boundaries() {
    let temp = tempdir().unwrap();
    let config = test_config(temp.path());
    seed_scientific_worker(&config, "run-epoch", "alpha", "analyze fixture", None).unwrap();
    assert_eq!(
        context_status(&config, "scientific-worker", 100_000, 50_000, 10_000)
            .unwrap()
            .action,
        ContextAction::Continue
    );
    assert_eq!(
        context_status(&config, "scientific-worker", 100_000, 60_000, 10_000)
            .unwrap()
            .action,
        ContextAction::Checkpoint
    );
    assert_eq!(
        context_status(&config, "scientific-worker", 100_000, 70_000, 10_000)
            .unwrap()
            .action,
        ContextAction::Reset
    );
    let receipt = checkpoint(
        &config,
        "run-epoch",
        "alpha",
        "context hard threshold",
        "host-controller",
    )
    .unwrap();
    assert_eq!(receipt.epoch, 1);
    assert_eq!(
        generate_view(&config, "run-epoch", "alpha").unwrap().epoch,
        1
    );
}

#[test]
fn large_collection_is_projected_without_dropping_the_whole_view() {
    let temp = tempdir().unwrap();
    let config = test_config(temp.path());
    seed_scientific_worker(&config, "run-bounded", "alpha", "analyze fixture", None).unwrap();
    let facts = (0..300)
        .map(|index| {
            (
                format!("/facts/fact-{index:04}"),
                json!({
                    "statement": format!("bounded observation {index} {}", "x".repeat(96)),
                    "evidence": [format!("sha256:{index:064x}")],
                    "confidence": 1.0
                }),
            )
        })
        .collect::<BTreeMap<_, _>>();
    apply_mutation(
        &config,
        &StateMutation {
            schema_version: 1,
            run_id: "run-bounded".into(),
            subject: "alpha".into(),
            collection: "knowledge".into(),
            base_version: 0,
            set: facts,
            remove: Vec::new(),
            rationale: None,
        },
        "host-controller",
        StateAuthority::Operator,
    )
    .unwrap();
    let view = generate_view(&config, "run-bounded", "alpha").unwrap();
    assert!(view.collections.contains_key("execution"));
    assert!(view.collections.contains_key("knowledge"));
    assert!(
        view.omitted
            .iter()
            .any(|item| item.collection == "knowledge" && item.reason.contains("/facts"))
    );
    assert!(view.estimated_tokens <= view.max_state_tokens);
}

#[test]
fn content_addressed_artifact_and_event_chain_verify() {
    let temp = tempdir().unwrap();
    let config = test_config(temp.path());
    init_run(
        &config,
        "run-artifact",
        super::StateDomain::Generic,
        "scientific-worker",
        "alpha",
        None,
    )
    .unwrap();
    let file = temp.path().join("method.py");
    fs::write(&file, "print(1)\n").unwrap();
    let artifact = add_artifact(
        &config,
        "run-artifact",
        "alpha",
        &file,
        "text/x-python",
        "method",
        "analysis script",
        ArtifactSensitivity::Standard,
        "host-controller",
    )
    .unwrap();
    assert_eq!(artifact.sha256.len(), 64);
    let report = verify(&config).unwrap();
    assert_eq!(report.artifacts, 1);
    assert!(report.event_chains_valid);
    let summary = trace_summary(&config, "run-artifact", 100, 100).unwrap();
    assert_eq!(summary.artifacts_total, 1);
    assert_eq!(summary.artifacts[0].label, "analysis script");
    assert!(!summary.payloads_included);
    assert!(!summary.state_values_included);
    let encoded = serde_json::to_value(&summary).unwrap();
    assert!(encoded.pointer("/events/0/payload").is_none());
    assert!(encoded.pointer("/collections/0/value").is_none());
    assert!(encoded.pointer("/events/0/previous_hash").is_some());
    assert!(encoded.pointer("/artifacts/0/bytes").is_none());
}

#[test]
fn export_is_atomic_and_omits_restricted_bytes_by_default() {
    let temp = tempdir().unwrap();
    let config = test_config(temp.path());
    init_run(
        &config,
        "run-export",
        super::StateDomain::Generic,
        "scientific-worker",
        "alpha",
        None,
    )
    .unwrap();
    let method = temp.path().join("method.py");
    let log = temp.path().join("trace.log");
    fs::write(&method, "print(1)\n").unwrap();
    fs::write(&log, "bounded returned trace\n").unwrap();
    add_artifact(
        &config,
        "run-export",
        "alpha",
        &method,
        "text/plain",
        "method",
        "method.py",
        ArtifactSensitivity::Standard,
        "host-controller",
    )
    .unwrap();
    let restricted = add_artifact(
        &config,
        "run-export",
        "alpha",
        &log,
        "text/plain",
        "returned-reasoning",
        "trace.log",
        ArtifactSensitivity::Restricted,
        "host-controller",
    )
    .unwrap();
    let restricted_sha = restricted.sha256.clone();
    let output = temp.path().join("exported-run");
    let receipt = export_run(&config, "run-export", &output, false, false).unwrap();
    assert_eq!(receipt.copied_artifacts, 1);
    assert_eq!(receipt.restricted_artifacts_omitted, 1);
    assert!(
        !output
            .join("artifacts/sha256")
            .join(restricted.sha256)
            .exists()
    );
    // RO-Crate 1.3 and Process Run Crate 0.6: what each requires of the descriptor, the root and the run.
    let crate_json: serde_json::Value =
        serde_json::from_slice(&fs::read(output.join("ro-crate-metadata.json")).unwrap()).unwrap();
    assert_eq!(
        crate_json["@context"][0],
        "https://w3id.org/ro/crate/1.3/context"
    );
    let graph = crate_json["@graph"].as_array().unwrap();
    let node = |id: &str| graph.iter().find(|n| n["@id"] == id).unwrap();
    let descriptor = node("ro-crate-metadata.json");
    assert_eq!(descriptor["@type"], "CreativeWork");
    assert_eq!(descriptor["about"]["@id"], "./");
    assert_eq!(
        descriptor["conformsTo"]["@id"],
        "https://w3id.org/ro/crate/1.3"
    );
    let root = node("./");
    assert_eq!(root["@type"], "Dataset");
    let published = root["datePublished"].as_str().unwrap();
    assert!(
        time::OffsetDateTime::parse(published, &time::format_description::well_known::Rfc3339)
            .is_ok()
    );
    assert!(root["name"].is_string() && root["description"].is_string());
    let profile = root["conformsTo"]["@id"].as_str().unwrap();
    assert_eq!(profile, "https://w3id.org/ro/wfrun/process/0.6");
    assert_eq!(node(profile)["@type"], "CreativeWork");
    let action = node("#run-run-export");
    assert_eq!(action["@type"][0], "CreateAction");
    assert_eq!(
        node(action["instrument"]["@id"].as_str().unwrap())["@type"],
        "SoftwareApplication"
    );
    assert_eq!(
        action["actionStatus"]["@id"],
        "http://schema.org/ActiveActionStatus"
    );
    assert!(action.get("endTime").is_none());
    assert!(
        root["mentions"]
            .as_array()
            .unwrap()
            .contains(&serde_json::json!({"@id": "#run-run-export"}))
    );
    // The store records custody, not production. The run names no result, nothing claims that an
    // event generated a file, and each file names the `artifact.recorded` events that registered it.
    assert!(action.get("result").is_none());
    let mut in_package = 0;
    for part in root["hasPart"].as_array().unwrap() {
        let id = part["@id"].as_str().unwrap();
        let file = node(id);
        assert_eq!(file["@type"], "File");
        if file["includedInPackage"] == true {
            in_package += 1;
            assert!(output.join(id).is_file());
        }
        let recorded = file["recordedByEvent"].as_array().unwrap();
        assert!(!recorded.is_empty());
        for event in recorded {
            assert_eq!(
                node(event["@id"].as_str().unwrap())["kind"],
                "artifact.recorded"
            );
        }
    }
    assert_eq!(in_package, 1);
    let text = serde_json::to_string(&crate_json).unwrap();
    assert!(!text.contains("wasGeneratedBy") && !text.contains("generatedByEvent"));
    // A payload left out is absent, not a null the context would read as a recorded JSON null.
    let events: Vec<&serde_json::Value> = graph
        .iter()
        .filter(|n| n["@type"] == "prov:Activity")
        .collect();
    assert!(!events.is_empty() && events.iter().all(|n| n.get("payload").is_none()));
    // The root states its terms of use, a file left out is a contextual entity, the runtime, the
    // profile and the exporting program are described apart, and every key is mapped and defined.
    let usage = node(root["license"]["@id"].as_str().unwrap());
    assert_eq!(
        (&usage["@type"], &usage["name"]),
        (
            &serde_json::json!("CreativeWork"),
            &serde_json::json!("No license granted")
        )
    );
    let left_out_id = format!("#artifact-sha256-{restricted_sha}");
    let left_out = node(&left_out_id);
    assert_eq!(
        (
            &left_out["includedInPackage"],
            &left_out["sensitivity"],
            &left_out["name"]
        ),
        (
            &serde_json::json!(false),
            &serde_json::json!("restricted"),
            &serde_json::json!("trace.log")
        )
    );
    assert!(
        root["hasPart"]
            .as_array()
            .unwrap()
            .contains(&serde_json::json!({"@id": left_out_id}))
    );
    assert_eq!(
        node(action["instrument"]["@id"].as_str().unwrap())["name"],
        env!("CARGO_PKG_NAME")
    );
    assert_eq!(
        node(action["object"]["@id"].as_str().unwrap())["name"],
        "scientific-worker"
    );
    let export = node("#export");
    assert_eq!(export["result"]["@id"], "./");
    assert_eq!(
        node(export["instrument"]["@id"].as_str().unwrap())["softwareVersion"],
        env!("CARGO_PKG_VERSION")
    );
    // Every key is a term of RO-Crate 1.3's context (checked against it) or of the local
    // context, and each local term is defined in the crate.
    let local = crate_json["@context"][1].as_object().unwrap();
    let ro_crate_terms = [
        "name",
        "description",
        "datePublished",
        "license",
        "conformsTo",
        "hasPart",
        "mentions",
        "about",
        "version",
        "agent",
        "actionStatus",
        "startTime",
        "endTime",
        "instrument",
        "object",
        "result",
        "sha256",
        "contentSize",
        "encodingFormat",
        "additionalType",
        "softwareVersion",
    ];
    for n in graph {
        for key in n.as_object().unwrap().keys() {
            assert!(
                key.starts_with('@')
                    || key.starts_with("rdfs:")
                    || ro_crate_terms.contains(&key.as_str())
                    || local.contains_key(key),
                "{key} is not mapped"
            );
        }
    }
    let mut defined = 0;
    for (term, mapping) in local {
        let Some(id) = mapping["@id"].as_str() else {
            continue;
        };
        if id.starts_with(super::STATE_TERMS) {
            let definition = node(id);
            assert_eq!(definition["@type"], "rdf:Property");
            assert_eq!(definition["rdfs:label"], serde_json::json!(term));
            assert!(
                definition["rdfs:comment"]
                    .as_str()
                    .is_some_and(|c| !c.is_empty())
            );
            defined += 1;
        }
    }
    assert_eq!(defined, super::STATE_TERM_DEFINITIONS.len());
    // Each key the export adds expands, through the crate's own context, to an IRI written out
    // here, not taken from the constants the export uses; a term added without its IRI here fails.
    let expand = |key: &str| -> Option<String> {
        let mapping = local.get(key)?;
        let id = mapping.as_str().or_else(|| mapping["@id"].as_str())?;
        Some(match id.split_once(':') {
            Some((prefix, rest)) if !rest.starts_with("//") => {
                match local.get(prefix).and_then(serde_json::Value::as_str) {
                    Some(base) => format!("{base}{rest}"),
                    None => id.to_string(),
                }
            }
            _ => id.to_string(),
        })
    };
    let ns = "urn:uuid:516305d7-526a-4182-91ff-25c8d6195928#";
    let mut expected: Vec<(&str, String)> = [
        "recordedByEvent",
        "includedInPackage",
        "sensitivity",
        "sequence",
        "epoch",
        "kind",
        "actor",
        "authority",
        "payload",
        "payloadSha256",
        "previousEventHash",
        "eventHash",
        "contextEpoch",
    ]
    .iter()
    .map(|term| (*term, format!("{ns}{term}")))
    .collect();
    expected.push((
        "startedAtTime",
        "http://www.w3.org/ns/prov#startedAtTime".to_string(),
    ));
    for (term, iri) in &expected {
        assert_eq!(expand(term).as_deref(), Some(iri.as_str()), "{term}");
    }
    // Each entry in full, so a coercion added to a term (a relationship read as a JSON literal, for
    // one) fails even when its IRI stays. Only the payload is a JSON literal and only the start time is typed.
    let mut written_out = serde_json::Map::new();
    written_out.insert(
        "prov".into(),
        serde_json::json!("http://www.w3.org/ns/prov#"),
    );
    for (term, iri) in &expected {
        let entry = match *term {
            "payload" => serde_json::json!({"@id": iri, "@type": "@json"}),
            "startedAtTime" => serde_json::json!({"@id": iri,
                    "@type": "http://www.w3.org/2001/XMLSchema#dateTime"}),
            _ => serde_json::json!({"@id": iri}),
        };
        written_out.insert((*term).into(), entry);
    }
    assert_eq!(*local, written_out);
    assert!(output.join("provenance.json").is_file());
    assert!(output.join("manifest-sha256.txt").is_file());
    assert!(export_run(&config, "run-export", &output, false, false).is_err());
    // With payloads, each event carries its payload as recorded: the bytes its digest was taken over.
    let with_payloads = temp.path().join("exported-payloads");
    export_run(&config, "run-export", &with_payloads, false, true).unwrap();
    let crate_with: serde_json::Value =
        serde_json::from_slice(&fs::read(with_payloads.join("ro-crate-metadata.json")).unwrap())
            .unwrap();
    let with_events: Vec<&serde_json::Value> = crate_with["@graph"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|n| n["@type"] == "prov:Activity")
        .collect();
    assert!(!with_events.is_empty());
    for event in with_events {
        let payload = event.get("payload").unwrap();
        assert!(payload.is_object());
        assert_eq!(
            super::hash_bytes(serde_json::to_string(payload).unwrap().as_bytes()),
            event["payloadSha256"].as_str().unwrap()
        );
    }
    // A sealed run's action is complete and ends when the run was sealed.
    seal_run(
        &config,
        "run-export",
        "alpha",
        "export test",
        "host-controller",
    )
    .unwrap();
    let sealed_output = temp.path().join("exported-sealed");
    export_run(&config, "run-export", &sealed_output, false, false).unwrap();
    let sealed: serde_json::Value =
        serde_json::from_slice(&fs::read(sealed_output.join("ro-crate-metadata.json")).unwrap())
            .unwrap();
    let sealed_action = sealed["@graph"]
        .as_array()
        .unwrap()
        .iter()
        .find(|n| n["@id"] == "#run-run-export")
        .unwrap();
    assert_eq!(
        sealed_action["actionStatus"]["@id"],
        "http://schema.org/CompletedActionStatus"
    );
    assert_eq!(
        sealed_action["endTime"],
        load_run(&config, "run-export").unwrap().updated_at
    );
}

#[test]
fn outbox_keeps_accepted_and_rejected_proposals() {
    let temp = tempdir().unwrap();
    let config = test_config(temp.path());
    let workspace = temp.path().join("workspace");
    create_private_dir(&workspace).unwrap();
    seed_scientific_worker(&config, "run-outbox", "alpha", "analyze fixture", None).unwrap();
    super::materialize_view(&config, "run-outbox", "alpha", &workspace).unwrap();
    fs::write(workspace.join(".agent-state/outbox/001.json"), serde_json::to_vec_pretty(&json!({
            "schema_version": 1,
            "run_id": "run-outbox",
            "subject": "alpha",
            "collection": "execution",
            "base_version": 0,
            "set": {"/phase": "running"},
            "remove": [],
            "rationale": {"summary": "work began", "alternatives": [], "evidence": [], "confidence": 1.0}
        })).unwrap()).unwrap();
    fs::write(workspace.join(".agent-state/outbox/002.json"), b"not json").unwrap();
    let outcome = reconcile_outbox(&config, "run-outbox", "alpha", &workspace).unwrap();
    assert_eq!(outcome.accepted, 1);
    assert_eq!(outcome.rejected, 1);
    assert!(workspace.join(".agent-state/accepted/001.json").is_file());
    assert!(workspace.join(".agent-state/rejected/002.json").is_file());
    let rejection =
        fs::read_to_string(workspace.join(".agent-state/rejected/002.json.error.json")).unwrap();
    assert!(!rejection.contains(&temp.path().display().to_string()));
    assert!(trace(&config, "run-outbox").unwrap().len() >= 6);
}

#[test]
fn continuity_state_is_profile_scoped_and_owner_scoped() {
    let temp = tempdir().unwrap();
    let config = test_config(temp.path());
    let collections = seed_continuity_agent(
        &config,
        "agent-state-one",
        "agent-one",
        "owner-one",
        StateDomain::ClusterOps,
        "cluster-operator",
        "inspect the declared cluster state",
        "cluster operations subagent",
    )
    .unwrap();
    assert_eq!(collections.len(), 4);
    let view = generate_view(&config, "agent-state-one", "agent-one").unwrap();
    assert!(view.collections.contains_key("execution"));
    assert!(view.collections.contains_key("cluster-ops"));
    assert!(view.collections.contains_key("coordination"));
    assert!(view.collections.contains_key("knowledge"));
    assert!(!view.collections.contains_key("scientific"));
    let coordination =
        load_collection(&config, "agent-state-one", "agent-one", "coordination").unwrap();
    assert!(
        coordination
            .access
            .iter()
            .all(|rule| { rule.principals == ["agent-one".to_string(), "owner-one".to_string()] })
    );
    assert!(
        seed_continuity_agent(
            &config,
            "bad-domain",
            "agent-two",
            "owner-one",
            StateDomain::Generic,
            "cluster-operator",
            "inspect one target",
            "operator",
        )
        .is_err()
    );

    let software = seed_continuity_agent(
        &config,
        "agent-state-code",
        "agent-code",
        "owner-one",
        StateDomain::SoftwareEngineering,
        "software-engineer",
        "implement one bounded source change",
        "software engineering subagent",
    )
    .unwrap();
    assert_eq!(software.len(), 3);
    let software_view = generate_view(&config, "agent-state-code", "agent-code").unwrap();
    assert!(software_view.collections.contains_key("execution"));
    assert!(software_view.collections.contains_key("coordination"));
    assert!(software_view.collections.contains_key("knowledge"));
    assert!(!software_view.collections.contains_key("cluster-ops"));
    assert!(!software_view.collections.contains_key("scientific"));
    assert!(
        validate_continuity_profile(&config, StateDomain::Generic, "software-engineer").is_err()
    );
    assert!(
        list_profiles(&config)
            .unwrap()
            .iter()
            .any(|profile| profile.name == "software-engineer")
    );

    let profile_path = temp.path().join("profile.json");
    fs::write(
        &profile_path,
        serde_json::to_vec_pretty(&json!({
            "schema_version": 1,
            "name": "knowledge-only",
            "audience": "agent",
            "reasoning_retention": "structured",
            "max_state_tokens": 4096,
            "latest_event_count": 4,
            "soft_reset_ratio": 0.60,
            "hard_reset_ratio": 0.72,
            "collections": [
                {"name": "knowledge", "priority": 0, "max_bytes": 8000, "pointers": [""]}
            ]
        }))
        .unwrap(),
    )
    .unwrap();
    install_profile(&config, &profile_path, false).unwrap();
    assert!(validate_continuity_profile(&config, StateDomain::Generic, "knowledge-only").is_err());
}

fn test_config(root: &Path) -> RuntimeConfig {
    let config = RuntimeConfig {
        root: root.join("workspaces"),
        state_dir: root.join("state"),
        config_dir: root.join("config"),
        openshell_bin: "/bin/true".into(),
        docker_bin: "/bin/true".into(),
    };
    for directory in [
        &config.root,
        &config.state_dir,
        &config.config_dir,
        &config.audit_dir(),
    ] {
        create_private_dir(directory).unwrap();
    }
    config
        .save_settings(&ControllerSettings::default())
        .unwrap();
    config
}
