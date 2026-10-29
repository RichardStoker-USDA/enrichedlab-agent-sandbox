use std::{fs, sync::atomic::Ordering, thread, time::Duration};

use serde_json::json;
use tempfile::tempdir;

use super::{
    ContinuityWorkspaceObserver, DirectRunObserver, IncrementalProvenanceWriter, find_live_status,
    is_runnable_status, random_name, start_state_watcher,
};
use crate::{
    agent_state,
    config::{ControllerSettings, RuntimeConfig, create_private_dir},
    harness::{CanonicalHarnessEvent, EvidenceVisibility, HarnessKind},
    provenance,
};

#[test]
fn random_names_are_dns_safe() {
    let name = random_name();
    assert!(name.starts_with("agent-"));
    assert_eq!(name.len(), 18);
}

#[test]
fn live_status_supports_status_and_phase() {
    let live = json!([{"name":"agent-a", "status":"Running"}, {"name":"agent-b", "phase":"Ready"}]);
    assert_eq!(find_live_status(&live, "agent-a"), Some("Running"));
    assert_eq!(find_live_status(&live, "agent-b"), Some("Ready"));
    assert!(is_runnable_status("Running"));
    assert!(is_runnable_status("ready"));
    assert!(!is_runnable_status("Stopped"));
}

#[test]
fn direct_run_observer_normalizes_pi_output_and_reasoning() {
    let mut observer = DirectRunObserver::new(HarnessKind::Pi);
    let observed = observer
            .observe_line(br#"{"type":"message_end","message":{"role":"assistant","content":[{"type":"thinking","thinking":"inspect schema"},{"type":"text","text":"done"}],"stopReason":"stop"}}"#)
            .unwrap();
    let final_events = observer.finish().unwrap();
    assert_eq!(observer.lines, 1);
    assert_eq!(observed.len(), 2);
    assert!(final_events.is_empty());
    assert_eq!(observed[0].evidence_role, "reasoning");
    assert_eq!(observed[1].evidence_role, "output");
}

fn provenance_config() -> (tempfile::TempDir, RuntimeConfig) {
    let temp = tempdir().unwrap();
    let config = RuntimeConfig {
        root: temp.path().join("workspaces"),
        state_dir: temp.path().join("state"),
        config_dir: temp.path().join("config"),
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
    provenance::initialize(&config).unwrap();
    (temp, config)
}

fn incremental_writer(config: RuntimeConfig) -> IncrementalProvenanceWriter {
    IncrementalProvenanceWriter {
        config,
        run_id: "live-provenance-run".into(),
        subject: "agent-live-provenance".into(),
        segment: 1,
        harness: HarnessKind::Pi,
        model: None,
        engine: None,
        parser: None,
    }
}

#[test]
fn incremental_provenance_persists_before_finalization_without_duplicates() {
    let (_temp, config) = provenance_config();
    let writer = incremental_writer(config.clone());
    let mut observer = DirectRunObserver::new(HarnessKind::Pi);
    let observed = observer
            .observe_line(br#"{"type":"tool_execution_end","toolCallId":"call-1","toolName":"catalog","result":{"content":"ok"},"isError":false}"#)
            .unwrap();
    writer.append_all(&observed).unwrap();

    let live = provenance::graph(&config, "live-provenance-run", 10).unwrap();
    assert_eq!(live.nodes.len(), 1);
    assert_eq!(live.nodes[0].kind, "agent.tool.finished");

    let final_events = observer.finish().unwrap();
    writer.append_all(&final_events).unwrap();
    writer
        .append(&CanonicalHarnessEvent {
            kind: "logical-agent.session.finished".into(),
            evidence_role: "lifecycle".into(),
            visibility: EvidenceVisibility::Metadata,
            source_event_id: None,
            parent_source_event_id: None,
            metadata: json!({"success": true}),
            restricted_content: None,
        })
        .unwrap();
    let finalized = provenance::graph(&config, "live-provenance-run", 10).unwrap();
    assert_eq!(finalized.nodes.len(), 2);
    assert_eq!(
        finalized
            .nodes
            .iter()
            .filter(|node| node.kind == "agent.tool.finished")
            .count(),
        1
    );
}

#[test]
fn continuity_snapshot_retains_overwritten_files_after_terminal_tool_events() {
    let (temp, config) = provenance_config();
    agent_state::seed_scientific_worker(
        &config,
        "continuity-evidence-run",
        "continuity-agent",
        "retain file versions",
        None,
    )
    .unwrap();
    let workspace = temp.path().join("workspace");
    create_private_dir(&workspace.join("evidence/discarded")).unwrap();
    let evidence = workspace.join("evidence/discarded/result.txt");
    fs::write(&evidence, "first failed result\n").unwrap();
    let writer = IncrementalProvenanceWriter {
        config: config.clone(),
        run_id: "continuity-evidence-run".into(),
        subject: "continuity-agent".into(),
        segment: 1,
        harness: HarnessKind::Pi,
        model: None,
        engine: None,
        parser: None,
    };
    let mut snapshot = ContinuityWorkspaceObserver::new(
        config.clone(),
        "continuity-evidence-run".into(),
        "continuity-agent".into(),
        workspace.clone(),
        1,
    );
    let finished = CanonicalHarnessEvent {
        kind: "agent.tool.finished".into(),
        evidence_role: "tool-io".into(),
        visibility: EvidenceVisibility::Metadata,
        source_event_id: Some("tool-first".into()),
        parent_source_event_id: None,
        metadata: json!({"status": "completed"}),
        restricted_content: None,
    };
    let first = writer.append_all(&[finished]).unwrap();
    snapshot
        .capture_after_terminal_events(
            &[CanonicalHarnessEvent {
                kind: "agent.tool.finished".into(),
                evidence_role: "tool-io".into(),
                visibility: EvidenceVisibility::Metadata,
                source_event_id: Some("tool-first".into()),
                parent_source_event_id: None,
                metadata: json!({"status": "completed"}),
                restricted_content: None,
            }],
            &first,
        )
        .unwrap();
    fs::write(&evidence, "overwritten failed result\n").unwrap();
    let failed = CanonicalHarnessEvent {
        kind: "agent.tool.observed".into(),
        evidence_role: "tool-io".into(),
        visibility: EvidenceVisibility::Metadata,
        source_event_id: Some("tool-second".into()),
        parent_source_event_id: None,
        metadata: json!({"status": "failed"}),
        restricted_content: None,
    };
    let second = writer.append_all(std::slice::from_ref(&failed)).unwrap();
    snapshot
        .capture_after_terminal_events(&[failed], &second)
        .unwrap();

    let summary = agent_state::trace_summary(&config, "continuity-evidence-run", 100, 100).unwrap();
    let snapshots = summary
        .events
        .iter()
        .filter(|event| event.kind == "workspace.snapshot.captured")
        .count();
    let versions = summary
        .artifacts
        .iter()
        .filter(|artifact| artifact.role == "failed-attempt")
        .map(|artifact| artifact.sha256.clone())
        .collect::<std::collections::BTreeSet<_>>();
    assert_eq!(snapshots, 2);
    assert_eq!(versions.len(), 2);
    let export_root = temp.path().join("export");
    agent_state::export_run(
        &config,
        "continuity-evidence-run",
        &export_root,
        false,
        true,
    )
    .unwrap();
    assert!(export_root.join("provenance.json").is_file());
}

#[test]
fn incremental_provenance_failure_keeps_prior_valid_history() {
    let (_temp, config) = provenance_config();
    let writer = incremental_writer(config.clone());
    let event = CanonicalHarnessEvent {
        kind: "agent.tool.finished".into(),
        evidence_role: "tool-io".into(),
        visibility: EvidenceVisibility::Metadata,
        source_event_id: Some("tool-1".into()),
        parent_source_event_id: None,
        metadata: json!({"exit_code": 0}),
        restricted_content: None,
    };
    writer.append(&event).unwrap();
    let failing = incremental_writer(RuntimeConfig {
        root: config.root.clone(),
        state_dir: config.state_dir.clone(),
        config_dir: config.config_dir.join("missing"),
        openshell_bin: "/bin/true".into(),
        docker_bin: "/bin/true".into(),
    });
    assert!(failing.append(&event).is_err());
    let graph = provenance::graph(&config, "live-provenance-run", 10).unwrap();
    assert_eq!(graph.nodes.len(), 1);
    assert_eq!(graph.nodes[0].kind, "agent.tool.finished");
}

#[test]
fn bind_state_watcher_reconciles_an_atomic_proposal() {
    let temp = tempdir().unwrap();
    let config = RuntimeConfig {
        root: temp.path().join("workspaces"),
        state_dir: temp.path().join("state"),
        config_dir: temp.path().join("config"),
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
    let workspace = config.root.join("worker-one");
    create_private_dir(&workspace).unwrap();
    agent_state::seed_scientific_worker(
        &config,
        "run-watch",
        "worker-one",
        "analyze fixture",
        None,
    )
    .unwrap();
    agent_state::materialize_view(&config, "run-watch", "worker-one", &workspace).unwrap();

    let (stop, handle) = start_state_watcher(
        config.clone(),
        "run-watch".into(),
        "worker-one".into(),
        workspace.clone(),
    );
    let pending = workspace.join(".agent-state/outbox/.proposal.pending");
    fs::write(
            &pending,
            serde_json::to_vec_pretty(&json!({
                "schema_version": 1,
                "run_id": "run-watch",
                "subject": "worker-one",
                "collection": "execution",
                "base_version": 0,
                "set": {"/phase": "running"},
                "remove": [],
                "rationale": {"summary": "work started", "alternatives": [], "evidence": [], "confidence": 1.0}
            }))
            .unwrap(),
        )
        .unwrap();
    fs::rename(pending, workspace.join(".agent-state/outbox/proposal.json")).unwrap();
    for _ in 0..40 {
        if workspace
            .join(".agent-state/accepted/proposal.json.receipt.json")
            .is_file()
        {
            break;
        }
        thread::sleep(Duration::from_millis(50));
    }
    stop.store(true, Ordering::Release);
    handle.join().unwrap().unwrap();
    assert!(
        workspace
            .join(".agent-state/accepted/proposal.json.receipt.json")
            .is_file()
    );
    assert_eq!(
        agent_state::load_collection(&config, "run-watch", "worker-one", "execution")
            .unwrap()
            .value["phase"],
        "running"
    );
}
