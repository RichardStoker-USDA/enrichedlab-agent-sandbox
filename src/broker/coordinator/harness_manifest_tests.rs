use super::*;
use crate::broker::coordinator::config::CoordinatorArgs;
use clap::Parser;
use std::io::Write;

#[derive(Parser)]
struct Cli {
    #[command(flatten)]
    args: CoordinatorArgs,
}

fn args(extra: &[&str]) -> CoordinatorArgs {
    let mut argv = vec![
        "coordinator",
        "--database-dsn-file",
        "/run/dsn",
        "--tenant",
        "research-site",
        "--model-config-file",
        "/run/model.json",
    ];
    argv.extend_from_slice(extra);
    Cli::parse_from(argv).args
}

fn model(sections: &str) -> Model {
    model_named("qualification-model", 196_608, sections)
}

fn model_named(name: &str, max_input_bytes: usize, sections: &str) -> Model {
    let _ = rustls::crypto::ring::default_provider().install_default();
    let mut file = tempfile::NamedTempFile::new().unwrap();
    write!(
        file,
        r#"{{"schema_version": 1, "endpoint": "https://models.example.invalid/v1/chat/completions",
        "model": "{name}", "max_input_bytes": {max_input_bytes}, "max_output_tokens": 8192,
        "context_window_tokens": 262144, "sections": {sections}}}"#
    )
    .unwrap();
    Model::load(&file.path().to_path_buf()).unwrap()
}

fn surfaces(m: &Manifest) -> Value {
    serde_json::from_slice::<Value>(&m.bytes).unwrap()["surfaces"].clone()
}

#[test]
fn the_same_inputs_give_the_same_digest_and_one_changed_surface_changes_it() {
    let a = compute(&args(&[]), &model("{}"), None, None).unwrap();
    let b = compute(&args(&[]), &model("{}"), None, None).unwrap();
    assert_eq!(a.sha256, b.sha256);
    assert_eq!(a.bytes, b.bytes);
    assert_eq!(
        a.sha256,
        format!("sha256:{}", hex::encode(Sha256::digest(&a.bytes)))
    );
    // One section budget changed changes the digest, and the manifest shows the new value with its range.
    let c = compute(
        &args(&[]),
        &model(r#"{"literature_bytes": 20000}"#),
        None,
        None,
    )
    .unwrap();
    assert_ne!(a.sha256, c.sha256);
    assert_eq!(
        surfaces(&c)["model.sections.literature_bytes"]["value"],
        20000
    );
    // So does a lab added to a feature, whatever the order the labs were given in.
    let d = compute(
        &args(&["--orientation-labs", "lab-b,lab-a"]),
        &model("{}"),
        None,
        None,
    )
    .unwrap();
    let e = compute(
        &args(&["--orientation-labs", "lab-a,lab-b"]),
        &model("{}"),
        None,
        None,
    )
    .unwrap();
    assert_ne!(a.sha256, d.sha256);
    assert_eq!(d.sha256, e.sha256);
    assert_eq!(
        surfaces(&d)["orientation.labs"]["value"],
        json!(["lab-a", "lab-b"])
    );
}

#[test]
fn the_manifest_lists_fixed_controls_and_holds_no_path_endpoint_or_credential() {
    let m = compute(
        &args(&[
            "--lead-text-labs",
            "lab-a",
            "--lead-text-unreviewed-labs",
            "lab-a",
        ]),
        &model("{}"),
        None,
        None,
    )
    .unwrap();
    let s = surfaces(&m);
    for fixed in FIXED {
        assert_eq!(s[fixed], json!({"editable": false}), "{fixed}");
    }
    assert_eq!(s["lead_system_text"]["source"], "compiled");
    assert_eq!(s["lead_system_text"]["bytes"], SYSTEM_BYTES());
    assert_eq!(s["lead_text.unreviewed_labs"]["person_required"], true);
    assert_eq!(s["model.context_window_tokens"]["editable"], false);
    let text = String::from_utf8(m.bytes.clone()).unwrap();
    for absent in ["/run/", "models.example.invalid", "dsn", "credential"] {
        assert!(!text.contains(absent), "{absent}");
    }
    assert!(m.bytes.len() <= MANIFEST_BYTES);
}

#[allow(non_snake_case)]
fn SYSTEM_BYTES() -> usize {
    crate::broker::coordinator::SYSTEM.len()
}

#[test]
fn a_path_or_address_served_as_the_model_name_is_kept_only_as_a_digest() {
    for name in ["/srv/models/glm", "./weights", "http://10.0.0.5/v1", "a b"] {
        let m = compute(&args(&[]), &model_named(name, 196_608, "{}"), None, None).unwrap();
        let s = surfaces(&m);
        assert!(s["model.name"].get("value").is_none(), "{name}");
        assert_eq!(s["model.name"]["sha256"], digest(name.as_bytes()));
        assert!(!String::from_utf8(m.bytes).unwrap().contains(name));
    }
    let m = compute(
        &args(&[]),
        &model_named("vendor/model-1.5", 196_608, "{}"),
        None,
        None,
    )
    .unwrap();
    assert_eq!(surfaces(&m)["model.name"]["value"], "vendor/model-1.5");
}

#[test]
fn a_section_with_no_valid_override_is_fixed_at_its_value_never_an_inverted_range() {
    // The smallest input ceiling the configuration accepts leaves the smallest lead budget.
    let smallest = SYSTEM_BYTES()
        + crate::broker::coordinator::REVIEW_RESERVE
        + crate::broker::coordinator::LEAD_BUDGET_MIN;
    let m = compute(
        &args(&[]),
        &model_named("qualification-model", smallest, "{}"),
        None,
        None,
    )
    .unwrap();
    for (key, surface) in surfaces(&m).as_object().unwrap() {
        if let Some(range) = surface.get("range") {
            assert!(range[0].as_u64() <= range[1].as_u64(), "{key}: {range}");
        }
    }
    assert_eq!(
        surfaces(&m)["model.sections.working_memory_bytes"]["editable"],
        false
    );
}

#[test]
fn a_profile_is_named_by_its_conventional_digest() {
    let mut file = tempfile::NamedTempFile::new().unwrap();
    write!(
        file,
        r#"{{"schema_version": 1, "name": "qualification-profile",
        "model_identity": {{"name": "qualification-model", "context_window_tokens": 262144}}}}"#
    )
    .unwrap();
    let path = file.path().to_path_buf();
    let profiled = model("{}").with_profile(Some(&path)).unwrap();
    let m = compute(&args(&[]), &profiled, None, None).unwrap();
    let sha = surfaces(&m)["model.profile"]["sha256"]
        .as_str()
        .unwrap()
        .to_owned();
    assert!(
        sha.starts_with("sha256:") && !sha.starts_with("sha256:sha256:") && sha.len() == 71,
        "{sha}"
    );
}

#[test]
fn a_configured_knowledge_service_and_its_labs_are_part_of_the_identity() {
    let mut file = tempfile::NamedTempFile::new().unwrap();
    write!(file, r#"{{"schema": "enrichedlab.knowledge-search.v1", "service_url": "https://knowledge.example.invalid",
        "token_file": "/run/knowledge/token", "labs": ["lab-b", "lab-a"]}}"#).unwrap();
    let service = crate::broker::knowledge::load_optional(Some(file.path()))
        .unwrap()
        .unwrap();
    let without = compute(&args(&[]), &model("{}"), None, None).unwrap();
    let with = compute(&args(&[]), &model("{}"), Some(&service), None).unwrap();
    assert_ne!(without.sha256, with.sha256);
    let s = surfaces(&with);
    assert_eq!(s["knowledge.labs"]["value"], json!(["lab-a", "lab-b"]));
    assert_eq!(s["knowledge.labs"]["configured"], true);
    assert_eq!(s["retrieval.generation"]["configured"], true);
    assert_eq!(
        surfaces(&without)["library.labs"],
        json!({"kind": "list", "configured": false, "value": [],
        "editable": true, "person_required": true})
    );
    let text = String::from_utf8(with.bytes).unwrap();
    assert!(!text.contains("knowledge.example.invalid") && !text.contains("/run/knowledge"));
}
