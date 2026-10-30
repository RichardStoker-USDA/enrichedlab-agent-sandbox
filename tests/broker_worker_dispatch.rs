use enrichedlab_agent_sandbox::broker::config::WorkerArgs;
use std::{fs, path::PathBuf};
use tempfile::TempDir;

fn args(root: &TempDir, catalog: Option<PathBuf>, writer_uid: Option<u32>) -> WorkerArgs {
    let dsn = root.path().join("worker.dsn");
    fs::write(&dsn, "host=127.0.0.1 dbname=broker sslmode=disable").unwrap();
    WorkerArgs {
        database_dsn_env: "IGNORED_TEST_DSN".into(),
        database_dsn_file: Some(dsn),
        tenant: "broker".into(),
        artifact_root: root.path().join("artifacts"),
        controller_root: root.path().join("controller"),
        controller_state: root.path().join("controller-state"),
        controller_config: root.path().join("controller-config"),
        controller_bin: "agent-sandbox".into(),
        openshell_bin: "openshell".into(),
        docker_bin: "docker".into(),
        worker_id: Some("worker-test".into()),
        direct_dispatch_catalog: catalog,
        direct_dispatch_writer_uid: writer_uid,
        query_connection_dir: None,
        render_url: None,
        dataset_fetcher_config: None,
        lab_objects_config: None,
        live_output: false,
        max_parallel: 1,
        lease_seconds: 60,
        poll_millis: 750,
        database_pool: 1,
        max_workspace_files: 20_000,
        max_workspace_bytes: 1024,
    }
}

#[test]
fn worker_direct_dispatch_configuration_is_optional_and_paired() {
    let root = TempDir::new().unwrap();
    assert!(
        args(&root, None, None)
            .resolve()
            .unwrap()
            .direct_dispatch
            .is_none()
    );

    let catalog = root.path().join("dispatch.json");
    let resolved = args(&root, Some(catalog.clone()), Some(501))
        .resolve()
        .unwrap();
    let direct = resolved.direct_dispatch.expect("paired direct dispatch");
    assert_eq!(direct.catalog, catalog);
    assert_eq!(direct.writer_uid, 501);

    assert!(args(&root, Some(catalog), None).resolve().is_err());
    assert!(
        args(&root, Some(PathBuf::from("dispatch.json")), Some(501))
            .resolve()
            .is_err()
    );

    let mut incompatible = args(
        &root,
        Some(root.path().join("dispatch-limit.json")),
        Some(501),
    );
    incompatible.max_workspace_files = 100_001;
    assert!(incompatible.resolve().is_err());
}
