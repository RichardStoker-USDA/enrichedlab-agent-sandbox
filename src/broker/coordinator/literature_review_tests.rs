use super::*;

#[test]
fn reuse_tracks_the_question_constraints_passages_catalog_and_gate_not_results() {
    let lead = json!({"question": "Synthetic question", "constraints": ["Synthetic constraint"],
        "literature": [{"ref": "library://synthetic/a", "text": "Synthetic passage"}], "methods": [{"key": "synthetic"}],
        "analyses": []});
    let fields = HashSet::from(["aim".into()]);
    let digest = input_digest(&context(&lead), &fields, 32768);
    let mut results_changed = lead.clone();
    results_changed["analyses"] = json!([{"status": "completed"}]);
    assert_eq!(
        digest,
        input_digest(&context(&results_changed), &fields, 32768)
    );
    for field in [
        "question",
        "constraints",
        "literature",
        "methods",
        "scientist_instructions",
    ] {
        let mut changed = lead.clone();
        changed[field] = json!("changed");
        assert_ne!(digest, input_digest(&context(&changed), &fields, 32768));
    }
    assert_ne!(
        digest,
        input_digest(&context(&lead), &HashSet::new(), 32768)
    );
    assert_ne!(digest, input_digest(&context(&lead), &fields, 65536));
}

#[test]
fn a_refusal_names_faults_once_then_stops_and_a_large_review_keeps_passages() {
    let mut ask = json!({});
    let mut retried = false;
    let faults = vec!["expected 1: ref_unread".into()];
    assert!(correct_once(&mut ask, &mut retried, &faults));
    assert_eq!(ask["prior_attempt"]["names"], json!(faults));
    assert!(!correct_once(&mut ask, &mut retried, &faults));
    let mut lead = json!({"literature": ["Synthetic passage"]});
    let original = lead.clone();
    let record = json!({"review": {"gaps": [{"text": "x".repeat(300), "refs": ["part"]}]}, "inputs_sha256": "digest"});
    let record = place(&mut lead, record, 20, "read");
    assert_eq!(record["status"], "too_large");
    assert_eq!(lead, original);
    assert!(record.get("review").is_none());
}

#[test]
fn review_refs_are_citable_background_but_do_not_become_lesson_evidence() {
    let context = json!({"literature_review": {"review": {"gaps": [{"text": "Synthetic gap", "refs": ["library://synthetic/section/a"]}]}}});
    assert!(context_literature(&context).contains("library://synthetic/section/a"));
    assert!(!context_evidence(&context).contains("library://synthetic/section/a"));
}

#[test]
fn review_options_default_off_and_require_reading_labs_and_bounded_room() {
    use clap::Parser;
    #[derive(Parser)]
    struct Options {
        #[command(flatten)]
        coordinator: CoordinatorArgs,
    }
    let parse = |extra: &[&str]| {
        Options::try_parse_from(
            [
                "test",
                "--database-dsn-file",
                "/unused",
                "--tenant",
                "test",
                "--model-config-file",
                "/unused",
            ]
            .into_iter()
            .chain(extra.iter().copied()),
        )
        .unwrap()
        .coordinator
    };
    let args = parse(&[]);
    assert!(args.literature_review_labs.is_empty());
    assert_eq!(args.literature_review_bytes, 32768);
    assert_eq!(
        parse(&["--literature-review-labs", "lab-a,lab-b"]).literature_review_labs,
        ["lab-a", "lab-b"]
    );
    let mut model: ModelConfig = serde_json::from_value(json!({"schema_version": 1,
        "endpoint": "http://127.0.0.1:1/v1/chat/completions", "model": "synthetic",
        "max_input_bytes": 65536, "max_output_tokens": 1024, "context_window_tokens": 262144}))
    .unwrap();
    let mut settings = LabText {
        review_bytes: 32768,
        ..LabText::default()
    };
    assert!(settings_valid(&settings, &model));
    settings.review_labs.insert("lab-a".into());
    assert!(!settings_valid(&settings, &model));
    settings.reading.insert("lab-a".into());
    assert!(settings_valid(&settings, &model));
    for bytes in [0, 16383, 196609] {
        settings.review_bytes = bytes;
        assert!(!settings_valid(&settings, &model));
    }
    settings.review_bytes = 32768;
    model.context_window_tokens = 65536;
    assert!(!settings_valid(&settings, &model));
}

#[test]
fn a_cached_refusal_keeps_its_faults_and_never_enters_the_lead_context() {
    let mut lead = json!({"literature": ["Synthetic passage"]});
    let original = lead.clone();
    let record = json!({"status": "refused", "inputs_sha256": "digest",
        "faults": ["expected 1: ref_unread"], "specialist": [], "reading": {"rounds": []}});
    for status in ["read", "kept"] {
        assert_eq!(place(&mut lead, record.clone(), 10000, status), record);
        assert_eq!(lead, original);
    }
}
