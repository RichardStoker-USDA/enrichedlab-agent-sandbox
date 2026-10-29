use std::{collections::BTreeMap, fs, path::Path};

use tempfile::tempdir;

use super::{
    DataQualityControlContract, RerunContract, ResearchClaim, ResearchEventObserver, ResearchMode,
    ResearchObservationSummary, ResearchResult, ResearchStudySpec, ResearchWorker,
    ResearchWorkerSpec, ResearchWorkerState, ScientificMethodContract, bind_capture_checkpoint,
    checkpoint_segment, collect, compare, create, event_stream_complete, hash_file, hash_path,
    inventory_tree, mark_capture_checkpoint_failed, mark_finished, mark_running,
    mark_worker_deleted, normalize_answer, prepare_worker, result_is_complete, store_run_capture,
    validate_result, verify, worker_prompt, worker_segment_prompt,
};
use crate::{
    cli::RuntimeArgs,
    config::{RuntimeConfig, create_private_dir},
    harness::HarnessKind,
    routes::RouteCatalog,
};

#[test]
fn study_snapshot_is_reproducible_and_rejects_symlinks() {
    let temp = tempdir().unwrap();
    let config = test_config(temp.path());
    let input = temp.path().join("input.csv");
    fs::write(&input, "value\n1\n").unwrap();
    let study = create(
        &config,
        ResearchStudySpec {
            name: "crop-study",
            mode: ResearchMode::Deterministic,
            question: Some("What is the sum?"),
            question_file: None,
            protocol: None,
            inputs: &[input],
            max_output_files: 100_000,
            max_output_bytes: 4_294_967_296,
        },
    )
    .unwrap();
    assert_eq!(study.input_file_count, 1);
    let manifest = inventory_tree(&study.root.join("control/inputs")).unwrap();
    assert_eq!(manifest.files[0].path, "input.csv");

    #[cfg(unix)]
    {
        let linked = temp.path().join("linked.csv");
        std::os::unix::fs::symlink(temp.path().join("input.csv"), &linked).unwrap();
        assert!(
            create(
                &config,
                ResearchStudySpec {
                    name: "linked-study",
                    mode: ResearchMode::Deterministic,
                    question: Some("Read the linked file."),
                    question_file: None,
                    protocol: None,
                    inputs: &[linked],
                    max_output_files: 100_000,
                    max_output_bytes: 4_294_967_296,
                },
            )
            .is_err()
        );
    }
}

#[test]
fn persisted_study_paths_cannot_escape_the_controller_tree() {
    let temp = tempdir().unwrap();
    let config = test_config(temp.path());
    let mut study = create(
        &config,
        ResearchStudySpec {
            name: "path-study",
            mode: ResearchMode::Exploratory,
            question: Some("Describe the fixture."),
            question_file: None,
            protocol: None,
            inputs: &[],
            max_output_files: 100_000,
            max_output_bytes: 4_294_967_296,
        },
    )
    .unwrap();
    study.root = temp.path().to_path_buf();
    super::write_json(&super::record_path(&config, "path-study"), &study).unwrap();

    assert!(super::load(&config, "path-study").is_err());
}

#[test]
fn result_requires_existing_bounded_evidence() {
    let temp = tempdir().unwrap();
    let output = temp.path();
    create_private_dir(&output.join("scripts")).unwrap();
    fs::write(output.join("scripts/analyze.py"), "print(1)\n").unwrap();
    fs::write(output.join("environment.txt"), "python=3\n").unwrap();
    let result = ResearchResult {
        schema_version: 2,
        worker: "alpha".into(),
        status: "complete".into(),
        answer: "1".into(),
        claims: vec![ResearchClaim {
            statement: "The sum is one.".into(),
            kind: "deterministic".into(),
            evidence: vec!["scripts/analyze.py".into()],
            confidence: 1.0,
        }],
        limitations: vec!["Synthetic fixture.".into()],
        rerun: RerunContract {
            command: "python scripts/analyze.py".into(),
            environment: "environment.txt".into(),
        },
        method: Some(ScientificMethodContract {
            class: "deterministic-tool".into(),
            tool_or_model: "python".into(),
            version: "3".into(),
            input_manifest_sha256: "0".repeat(64),
            code_refs: vec!["scripts/analyze.py".into()],
            parameter_refs: vec!["environment.txt".into()],
            output_refs: vec!["scripts/analyze.py".into()],
            validation_refs: vec!["scripts/analyze.py".into()],
            seeds: Vec::new(),
            llm_disclosure: None,
        }),
        quality_control: Some(DataQualityControlContract {
            purpose: "synthetic-fixture-check".into(),
            schema_refs: Vec::new(),
            sample_refs: Vec::new(),
            check_refs: vec!["scripts/analyze.py".into()],
            llm_is_scientific_instrument: false,
        }),
    };
    validate_result(&result, "alpha", output).unwrap();
    let mut missing = result;
    missing.claims[0].evidence[0] = "scripts/missing.py".into();
    assert!(validate_result(&missing, "alpha", output).is_err());
}

#[test]
fn worker_assignment_embeds_the_strict_result_contract() {
    let temp = tempdir().unwrap();
    let config = test_config(temp.path());
    create(
        &config,
        ResearchStudySpec {
            name: "contract-study",
            mode: ResearchMode::Deterministic,
            question: Some("Return one bounded result."),
            question_file: None,
            protocol: None,
            inputs: &[],
            max_output_files: 100,
            max_output_bytes: 1024 * 1024,
        },
    )
    .unwrap();
    let worker = prepare_test_worker(
        &config,
        "contract-study",
        "alpha",
        "Use the fixed protocol.",
    );
    let assignment: serde_json::Value =
        super::read_json(&worker.workspace.join("research-assignment.json")).unwrap();
    let contract = &assignment["result_contract"];
    assert_eq!(contract["additional_properties"], false);
    assert_eq!(contract["template"]["worker"], "alpha");
    assert!(contract["template"]["answer"].as_str().is_some());
    assert_eq!(
        contract["template"]["rerun"]["environment"],
        "environment.txt"
    );
    assert!(
        worker_prompt(&config, "contract-study", "alpha")
            .unwrap()
            .contains("result_contract is authoritative")
    );
    let study = super::load(&config, "contract-study").unwrap();
    let prompt = worker_segment_prompt(&study, &worker, 2, 8);
    assert!(prompt.contains("fresh provider session"));
    assert!(prompt.contains("no prior transcript or KV state"));
    assert!(prompt.contains("controller separately records tool and file evidence"));
}

#[test]
fn deleting_a_worker_closes_its_state_run() {
    let temp = tempdir().unwrap();
    let config = test_config(temp.path());
    create(
        &config,
        ResearchStudySpec {
            name: "delete-study",
            mode: ResearchMode::Deterministic,
            question: Some("Exercise the deletion boundary."),
            question_file: None,
            protocol: None,
            inputs: &[],
            max_output_files: 100,
            max_output_bytes: 1024 * 1024,
        },
    )
    .unwrap();
    let prepared = prepare_test_worker(
        &config,
        "delete-study",
        "alpha",
        "Record one bounded attempt.",
    );

    let deleted = mark_worker_deleted(&config, "delete-study", "alpha").unwrap();
    assert_eq!(deleted.state, ResearchWorkerState::Deleted);
    assert!(deleted.sandbox_deleted);
    let run_id = prepared.state_run_id.as_deref().unwrap();
    assert_eq!(
        crate::agent_state::load_run(&config, run_id)
            .unwrap()
            .status,
        "sealed"
    );
    assert_eq!(
        crate::agent_state::trace(&config, run_id)
            .unwrap()
            .last()
            .unwrap()
            .kind,
        "state.run.sealed"
    );
}

#[test]
fn controller_observer_versions_outputs_and_checkpoints_without_model_state() {
    let temp = tempdir().unwrap();
    let config = test_config(temp.path());
    create(
        &config,
        ResearchStudySpec {
            name: "observed-study",
            mode: ResearchMode::Deterministic,
            question: Some("Return one bounded result."),
            question_file: None,
            protocol: None,
            inputs: &[],
            max_output_files: 100,
            max_output_bytes: 1024 * 1024,
        },
    )
    .unwrap();
    let worker = prepare_test_worker(
        &config,
        "observed-study",
        "alpha",
        "Use two independent calculations.",
    );
    mark_running(&config, "observed-study", "alpha").unwrap();
    let mut observer = ResearchEventObserver::new(&config, "observed-study", &worker, 1).unwrap();
    let view: serde_json::Value =
        super::read_json(&worker.workspace.join(".agent-state/view.json")).unwrap();
    assert_eq!(
        view.pointer("/collections/execution/value/phase")
            .and_then(serde_json::Value::as_str),
        Some("segment-1-running")
    );
    let attempt = worker.output.join("intermediate/estimate.txt");
    fs::write(&attempt, "first\n").unwrap();
    observer
            .observe_line(
                br#"{"type":"tool_use","part":{"tool":"write","callID":"call-one","state":{"status":"completed","input":{"path":"intermediate/estimate.txt"},"output":"ok","metadata":{"filepath":"intermediate/estimate.txt"},"time":{"start":1,"end":2}}}}"#,
            )
            .unwrap();
    fs::write(&attempt, "second\n").unwrap();
    observer
            .observe_line(
                br#"{"type":"tool_use","part":{"tool":"write","callID":"call-two","state":{"status":"completed","input":{"path":"intermediate/estimate.txt"},"output":"ok","metadata":{"filepath":"intermediate/estimate.txt"},"time":{"start":2,"end":3}}}}"#,
            )
            .unwrap();
    observer
            .observe_line(
                br#"{"type":"step_finish","part":{"tokens":{"input":12000,"output":300,"reasoning":700,"total":13000,"cache":{"read":500,"write":0}}}}"#,
            )
            .unwrap();
    observer
            .observe_line(
                br#"{"type":"step_finish","part":{"tokens":{"input":100,"output":20,"reasoning":30,"cache":{"read":200,"write":0}}}}"#,
            )
            .unwrap();
    write_result(&worker, "1");
    let observations = observer.finish().unwrap();
    assert_eq!(observations.tool_calls, 2);
    assert_eq!(observations.step_finishes, 2);
    assert_eq!(observations.input_tokens, 12_100);
    assert_eq!(observations.reasoning_tokens, 730);
    assert_eq!(observations.total_tokens, 13_850);
    assert_eq!(observations.peak_context_tokens, 13_500);
    assert!(observations.workspace_snapshots >= 4);
    assert!(observations.artifact_versions >= 6);
    assert!(result_is_complete(&config, "observed-study", "alpha").unwrap());
    let stdout = b"{\"type\":\"step_finish\"}\n";
    let mut capture = store_run_capture(
        &config,
        "observed-study",
        "alpha",
        1,
        true,
        "passed",
        true,
        Some(0),
        stdout,
        &[],
        false,
        false,
        observations.clone(),
        0.13,
        "continue",
    )
    .unwrap();
    let checkpoint = checkpoint_segment(
        &config,
        "observed-study",
        "alpha",
        1,
        true,
        &observations,
        "continue",
    )
    .unwrap();
    bind_capture_checkpoint(
        &config,
        "observed-study",
        "alpha",
        &mut capture,
        checkpoint.epoch,
    )
    .unwrap();
    assert_eq!(capture.checkpoint_epoch, Some(1));
    let current = super::load(&config, "observed-study").unwrap();
    assert_eq!(current.workers["alpha"].segments_completed, 1);
    let run_id = worker.state_run_id.as_deref().unwrap();
    let events = crate::agent_state::trace(&config, run_id).unwrap();
    let tool_events = events
        .iter()
        .filter(|event| event.kind == "agent.tool.observed")
        .collect::<Vec<_>>();
    let snapshots = events
        .iter()
        .filter(|event| event.kind == "workspace.snapshot.captured")
        .collect::<Vec<_>>();
    let tool_snapshots = snapshots
        .iter()
        .filter(|event| event.payload["trigger"] == "tool-completed")
        .collect::<Vec<_>>();
    assert_eq!(tool_snapshots.len(), 2);
    for (snapshot, tool) in tool_snapshots.iter().zip(&tool_events) {
        let links = snapshot.payload["observed_after"].as_array().unwrap();
        assert_eq!(links.len(), 1);
        assert_eq!(links[0]["run_id"], run_id);
        assert_eq!(links[0]["event_id"], tool.payload["provenance_event_id"]);
        assert_eq!(links[0]["hash"], tool.payload["provenance_hash"]);
        assert!(links[0]["sequence"].as_u64().unwrap() > 0);
    }
    assert!(
        snapshots
            .iter()
            .filter(|event| event.payload["trigger"] != "tool-completed")
            .all(|event| event.payload["observed_after"]
                .as_array()
                .unwrap()
                .is_empty())
    );
    assert_eq!(
        events
            .iter()
            .filter(|event| event.kind == "agent.tool.observed")
            .count(),
        2
    );
    let version_digests = events
        .iter()
        .filter(|event| event.kind == "artifact.recorded")
        .filter_map(|event| {
            event
                .payload
                .get("label")
                .and_then(serde_json::Value::as_str)
                .filter(|label| label.contains("intermediate/estimate.txt"))
                .and_then(|_| event.payload.get("sha256"))
                .and_then(serde_json::Value::as_str)
        })
        .collect::<std::collections::BTreeSet<_>>();
    assert_eq!(version_digests.len(), 2);
    assert!(crate::agent_state::verify(&config).unwrap().artifacts >= 6);
    mark_finished(&config, "observed-study", "alpha", true, "passed").unwrap();
    collect(&config, "observed-study", "alpha").unwrap();
    assert!(
        worker
            .output
            .join("logs/controller/segments/000001/stdout.jsonl")
            .is_file()
    );
    let trace = crate::agent_state::trace_summary(&config, run_id, 100, 100).unwrap();
    assert!(trace.artifacts.iter().any(|artifact| {
        artifact.label == "logs/controller/segments/000001/stdout.jsonl"
            && artifact.sensitivity == crate::agent_state::ArtifactSensitivity::Restricted
    }));
}

#[test]
fn failed_tool_snapshot_preserves_its_exact_observation_and_file_version() {
    let temp = tempdir().unwrap();
    let config = test_config(temp.path());
    create(
        &config,
        ResearchStudySpec {
            name: "failed-observation",
            mode: ResearchMode::Exploratory,
            question: Some("Retain a failed synthetic attempt."),
            question_file: None,
            protocol: None,
            inputs: &[],
            max_output_files: 100,
            max_output_bytes: 1024 * 1024,
        },
    )
    .unwrap();
    let worker = prepare_test_worker(
        &config,
        "failed-observation",
        "alpha",
        "Retain each attempt.",
    );
    mark_running(&config, "failed-observation", "alpha").unwrap();
    let mut observer =
        ResearchEventObserver::new(&config, "failed-observation", &worker, 1).unwrap();
    let file = worker.output.join("discarded/attempt.txt");
    fs::write(&file, b"unfinished fixture\n").unwrap();
    observer.observe_line(br#"{"type":"tool_use","part":{"tool":"write","callID":"failed-call","state":{"status":"error","input":{},"output":"failed","metadata":{"observed_after":[{"event_id":"untrusted"}]}}}}"#).unwrap();
    fs::write(&file, b"later fixture\n").unwrap();
    observer.finish().unwrap();

    let run_id = worker.state_run_id.as_deref().unwrap();
    let events = crate::agent_state::trace(&config, run_id).unwrap();
    let tool = events
        .iter()
        .find(|event| event.kind == "agent.tool.observed")
        .unwrap();
    let snapshot = events
        .iter()
        .find(|event| {
            event.kind == "workspace.snapshot.captured"
                && event.payload["trigger"] == "tool-completed"
        })
        .unwrap();
    let reference = &snapshot.payload["observed_after"][0];
    assert_eq!(reference["event_id"], tool.payload["provenance_event_id"]);
    assert_eq!(reference["hash"], tool.payload["provenance_hash"]);
    assert_ne!(reference["event_id"], "untrusted");
    let digest = snapshot.payload["receipt_sha256"].as_str().unwrap();
    let receipt_path = config
        .agent_state_artifacts_dir()
        .join(format!("sha256/{}/{digest}", &digest[..2]));
    let delta: serde_json::Value =
        serde_json::from_slice(&fs::read(receipt_path).unwrap()).unwrap();
    assert_eq!(delta["schema_version"], 2);
    assert_eq!(delta["observed_after"], snapshot.payload["observed_after"]);
    let artifact = delta["artifact_versions"]
        .as_array()
        .unwrap()
        .iter()
        .find(|artifact| artifact["path"] == "discarded/attempt.txt")
        .unwrap();
    let digest = artifact["sha256"].as_str().unwrap();
    let retained = config
        .agent_state_artifacts_dir()
        .join(format!("sha256/{}/{digest}", &digest[..2]));
    assert_eq!(fs::read(retained).unwrap(), b"unfinished fixture\n");
    assert!(crate::agent_state::verify(&config).is_ok());
    assert!(crate::provenance::status(&config).unwrap().chain_valid);
}

#[test]
fn event_stream_gate_rejects_missing_malformed_and_truncated_usage() {
    let mut summary = ResearchObservationSummary {
        json_lines: 2,
        step_finishes: 1,
        ..ResearchObservationSummary::default()
    };
    assert!(event_stream_complete(&summary, false, false, false, false));
    summary.malformed_lines = 1;
    assert!(!event_stream_complete(&summary, false, false, false, false));
    summary.malformed_lines = 0;
    summary.step_finishes = 0;
    assert!(!event_stream_complete(&summary, false, false, false, false));
    summary.step_finishes = 1;
    assert!(!event_stream_complete(&summary, true, false, false, false));
    assert!(!event_stream_complete(&summary, false, false, true, false));
    assert!(!event_stream_complete(&summary, false, false, false, true));
}

#[test]
fn checkpoint_failure_keeps_the_original_segment_stream() {
    let temp = tempdir().unwrap();
    let config = test_config(temp.path());
    create(
        &config,
        ResearchStudySpec {
            name: "checkpoint-study",
            mode: ResearchMode::Deterministic,
            question: Some("Retain the synthetic stream."),
            question_file: None,
            protocol: None,
            inputs: &[],
            max_output_files: 100,
            max_output_bytes: 1024 * 1024,
        },
    )
    .unwrap();
    let worker = prepare_test_worker(
        &config,
        "checkpoint-study",
        "alpha",
        "Use the fixed fixture.",
    );
    mark_running(&config, "checkpoint-study", "alpha").unwrap();
    let stdout = b"{\"type\":\"step_finish\",\"part\":{\"tokens\":{\"total\":1}}}\n";
    let mut capture = store_run_capture(
        &config,
        "checkpoint-study",
        "alpha",
        1,
        true,
        "segment-incomplete",
        false,
        Some(0),
        stdout,
        b"",
        false,
        false,
        ResearchObservationSummary {
            json_lines: 1,
            step_finishes: 1,
            total_tokens: 1,
            peak_context_tokens: 1,
            ..ResearchObservationSummary::default()
        },
        0.1,
        "continue",
    )
    .unwrap();
    mark_capture_checkpoint_failed(
        &config,
        "checkpoint-study",
        "alpha",
        &mut capture,
        "synthetic checkpoint failure",
    )
    .unwrap();
    assert!(!capture.passed);
    assert_eq!(capture.result_class, "controller-checkpoint");
    let segment = super::segment_log_dir(
        &super::load(&config, "checkpoint-study").unwrap(),
        "alpha",
        1,
    );
    assert_eq!(fs::read(segment.join("stdout.jsonl")).unwrap(), stdout);
    assert_eq!(
        fs::read_to_string(segment.join("checkpoint-error.log")).unwrap(),
        "synthetic checkpoint failure"
    );
    let events =
        crate::agent_state::trace(&config, worker.state_run_id.as_deref().unwrap()).unwrap();
    assert!(
        events
            .iter()
            .any(|event| event.kind == "agent.segment.checkpoint-failed")
    );
}

#[test]
fn answer_comparison_normalizes_only_whitespace() {
    assert_eq!(normalize_answer("one\n two"), "one two");
    assert_ne!(normalize_answer("One"), normalize_answer("one"));
}

#[test]
fn directory_hash_changes_with_output_bytes() {
    let temp = tempdir().unwrap();
    fs::write(temp.path().join("result.txt"), "one").unwrap();
    let first = hash_path(temp.path()).unwrap();
    fs::write(temp.path().join("result.txt"), "two").unwrap();
    let second = hash_path(temp.path()).unwrap();
    assert_ne!(first.0, second.0);
}

#[test]
fn independent_results_are_sealed_compared_and_reverified() {
    let temp = tempdir().unwrap();
    let config = test_config(temp.path());
    let input = temp.path().join("input.csv");
    fs::write(&input, "value\n1\n").unwrap();
    create(
        &config,
        ResearchStudySpec {
            name: "crop-study",
            mode: ResearchMode::Deterministic,
            question: Some("What is the sum?"),
            question_file: None,
            protocol: None,
            inputs: &[input],
            max_output_files: 100_000,
            max_output_bytes: 4_294_967_296,
        },
    )
    .unwrap();
    for (name, perspective) in [
        ("alpha", "Use a direct sum."),
        ("beta", "Check the result with an independent script."),
    ] {
        let worker = prepare_test_worker(&config, "crop-study", name, perspective);
        mark_running(&config, "crop-study", name).unwrap();
        write_result(&worker, "1");
        mark_finished(&config, "crop-study", name, true, "passed").unwrap();
        collect(&config, "crop-study", name).unwrap();
    }
    let comparison = compare(&config, "crop-study").unwrap();
    assert!(comparison.all_answers_agree);
    assert!(comparison.external_validation_required);
    assert_eq!(
        comparison.scientific_validation,
        "not-established-by-controller"
    );
    let verification = verify(&config, "crop-study").unwrap();
    assert_eq!(verification.worker_seals_valid, 2);

    let study = super::load(&config, "crop-study").unwrap();
    let alpha_run = study.workers["alpha"].state_run_id.as_deref().unwrap();
    assert_eq!(
        crate::agent_state::load_run(&config, alpha_run)
            .unwrap()
            .status,
        "sealed"
    );
    let scientific =
        crate::agent_state::load_collection(&config, alpha_run, "alpha", "scientific").unwrap();
    assert!(
        scientific.value["intermediate_refs"]
            .as_object()
            .unwrap()
            .values()
            .any(|value| value["role"] == "failed-attempt")
    );
    assert!(crate::agent_state::verify(&config).unwrap().artifacts >= 4);

    let alpha = study.workers["alpha"].clone();
    fs::write(alpha.output.join("methods.md"), "changed\n").unwrap();
    assert!(verify(&config, "crop-study").is_err());
}

#[test]
fn collection_rejects_changed_worker_assignment() {
    let temp = tempdir().unwrap();
    let config = test_config(temp.path());
    create(
        &config,
        ResearchStudySpec {
            name: "soil-study",
            mode: ResearchMode::Exploratory,
            question: Some("Describe the fixture."),
            question_file: None,
            protocol: None,
            inputs: &[],
            max_output_files: 100_000,
            max_output_bytes: 4_294_967_296,
        },
    )
    .unwrap();
    let worker = prepare_test_worker(&config, "soil-study", "alpha", "Inspect the fixture.");
    mark_running(&config, "soil-study", "alpha").unwrap();
    write_result(&worker, "No inputs were provided.");
    mark_finished(&config, "soil-study", "alpha", true, "passed").unwrap();
    fs::write(worker.workspace.join("perspective.txt"), "changed\n").unwrap();
    assert!(collect(&config, "soil-study", "alpha").is_err());
}

#[test]
fn collection_rejects_output_over_the_study_cap() {
    let temp = tempdir().unwrap();
    let config = test_config(temp.path());
    create(
        &config,
        ResearchStudySpec {
            name: "bounded-study",
            mode: ResearchMode::Deterministic,
            question: Some("Return the fixture value."),
            question_file: None,
            protocol: None,
            inputs: &[],
            max_output_files: 100,
            max_output_bytes: 64,
        },
    )
    .unwrap();
    let worker = prepare_test_worker(&config, "bounded-study", "alpha", "Read the fixed fixture.");
    mark_running(&config, "bounded-study", "alpha").unwrap();
    write_result(&worker, "one");
    mark_finished(&config, "bounded-study", "alpha", true, "passed").unwrap();

    let error = collect(&config, "bounded-study", "alpha").unwrap_err();
    assert!(
        error.to_string().contains("byte limit"),
        "unexpected output-cap error: {error:#}"
    );
}

fn prepare_test_worker(
    config: &RuntimeConfig,
    study: &str,
    name: &str,
    perspective: &str,
) -> ResearchWorker {
    let runtime = RuntimeArgs {
        route: Some("local-agent".into()),
        ..RuntimeArgs::default()
    };
    prepare_worker(
        config,
        ResearchWorkerSpec {
            study_name: study,
            worker_name: name,
            perspective: Some(perspective),
            perspective_file: None,
            tool_names: &[],
            thinking: None,
            harness: HarnessKind::OpenCode,
            runtime: &runtime,
        },
    )
    .unwrap()
}

fn write_result(worker: &ResearchWorker, answer: &str) {
    fs::write(
        worker.output.join("methods.md"),
        "Ran scripts/analyze.py.\n",
    )
    .unwrap();
    fs::write(worker.output.join("environment.txt"), "python=3\n").unwrap();
    fs::write(worker.output.join("scripts/analyze.py"), "print(1)\n").unwrap();
    fs::write(
        worker.output.join("discarded/first-attempt.txt"),
        "superseded synthetic attempt\n",
    )
    .unwrap();
    let result = ResearchResult {
        schema_version: 2,
        worker: worker.name.clone(),
        status: "complete".into(),
        answer: answer.into(),
        claims: vec![ResearchClaim {
            statement: "The analysis completed.".into(),
            kind: "deterministic".into(),
            evidence: vec!["scripts/analyze.py".into()],
            confidence: 1.0,
        }],
        limitations: vec!["Synthetic fixture.".into()],
        rerun: RerunContract {
            command: "python scripts/analyze.py".into(),
            environment: "environment.txt".into(),
        },
        method: Some(ScientificMethodContract {
            class: "deterministic-tool".into(),
            tool_or_model: "python".into(),
            version: "3".into(),
            input_manifest_sha256: hash_file(&worker.workspace.join("input-manifest.json"))
                .unwrap(),
            code_refs: vec!["scripts/analyze.py".into()],
            parameter_refs: vec!["environment.txt".into()],
            output_refs: vec!["methods.md".into()],
            validation_refs: vec!["methods.md".into()],
            seeds: Vec::new(),
            llm_disclosure: None,
        }),
        quality_control: Some(DataQualityControlContract {
            purpose: "synthetic-fixture-check".into(),
            schema_refs: Vec::new(),
            sample_refs: Vec::new(),
            check_refs: vec!["methods.md".into()],
            llm_is_scientific_instrument: false,
        }),
    };
    fs::write(
        worker.output.join("results/result.json"),
        serde_json::to_vec_pretty(&result).unwrap(),
    )
    .unwrap();
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
        &config.research_dir(),
        &config.audit_dir(),
    ] {
        create_private_dir(directory).unwrap();
    }
    let settings = crate::config::ControllerSettings {
        default_route: "local-agent".into(),
        gateway_profiles: BTreeMap::from([(
            "docker-local".into(),
            crate::platform::GatewayProfile::default(),
        )]),
        ..crate::config::ControllerSettings::default()
    };
    config.save_settings(&settings).unwrap();
    let routes: RouteCatalog =
        serde_json::from_str(include_str!("../../examples/routes.json")).unwrap();
    routes.save(&config).unwrap();
    config
}
