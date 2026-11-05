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
    let _ = rustls::crypto::ring::default_provider().install_default();
    let mut file = tempfile::NamedTempFile::new().unwrap();
    write!(
        file,
        r#"{{"schema_version": 1, "endpoint": "https://models.example.invalid/v1/chat/completions",
        "model": "qualification-model", "max_input_bytes": 196608, "max_output_tokens": 8192,
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
    let a = compute(&args(&[]), &model("{}"), None).unwrap();
    let b = compute(&args(&[]), &model("{}"), None).unwrap();
    assert_eq!(a.sha256, b.sha256);
    assert_eq!(a.bytes, b.bytes);
    assert_eq!(
        a.sha256,
        format!("sha256:{}", hex::encode(Sha256::digest(&a.bytes)))
    );
    // One section budget changed changes the digest, and the manifest shows the new value with its range.
    let c = compute(&args(&[]), &model(r#"{"literature_bytes": 20000}"#), None).unwrap();
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
    )
    .unwrap();
    let e = compute(
        &args(&["--orientation-labs", "lab-a,lab-b"]),
        &model("{}"),
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
