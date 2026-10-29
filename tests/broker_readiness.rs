//! Exercise initialization before the coordinator's own checks through the real command entrypoint.
use std::{fs, process::Command};

#[test]
fn failed_telemetry_start_removes_stale_readiness() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("ready");
    fs::write(&path, b"stale").unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_agent-sandbox-broker"))
        .args([
            "coordinate",
            "--database-dsn-file",
            "/unused",
            "--tenant",
            "test",
            "--model-config-file",
            "/unused",
            "--ready-file",
        ])
        .arg(&path)
        .env(
            "AGENT_SANDBOX_FEATURE_PSEUDONYM_KEY_FILE",
            dir.path().join("telemetry_key_missing"),
        )
        .env_remove("AGENT_SANDBOX_FEATURE_METRICS_BIND")
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("private_file_unavailable"));
    assert!(!path.exists());
    assert_eq!(fs::read_dir(dir.path()).unwrap().count(), 0);
}
