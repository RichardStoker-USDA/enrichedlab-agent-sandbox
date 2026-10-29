use super::super::{CoordinatorArgs, run};
use super::*;
use clap::Parser;

fn options(extra: &[&str]) -> CoordinatorArgs {
    #[derive(Parser)]
    struct Options {
        #[command(flatten)]
        coordinator: CoordinatorArgs,
    }
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
}

#[test]
fn readiness_defaults_off_and_rejects_nonabsolute_or_missing_parents() {
    assert!(options(&[]).ready_file.is_none());
    let mut off = ReadyFile::prepare(None).unwrap();
    off.publish().unwrap();
    off.clear().unwrap();
    let dir = tempfile::tempdir().unwrap();
    for path in [
        PathBuf::from("relative"),
        dir.path().join("missing/ready"),
        dir.path().to_owned(),
    ] {
        assert!(ReadyFile::prepare(Some(&path)).is_err());
    }
    let parent_file = dir.path().join("file");
    fs::write(&parent_file, b"synthetic").unwrap();
    assert!(ReadyFile::prepare(Some(&parent_file.join("ready"))).is_err());
}

#[test]
fn readiness_is_private_and_cleared_on_exit_and_restart() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("ready");
    fs::write(&path, b"stale").unwrap();
    let mut ready = ReadyFile::prepare(Some(&path)).unwrap();
    assert!(!path.exists());
    ready.publish().unwrap();
    let bytes = fs::read(&path).unwrap();
    let value: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(value["version"], env!("CARGO_PKG_VERSION"));
    OffsetDateTime::parse(value["ready_at"].as_str().unwrap(), &Rfc3339).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }
    ready.publish().unwrap();
    assert_eq!(fs::read(&path).unwrap(), bytes);
    assert_eq!(fs::read_dir(dir.path()).unwrap().count(), 1);
    drop(ready);
    assert!(!path.exists());
}

#[test]
fn interrupted_publication_never_exposes_the_final_path_and_cleans_the_temporary() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("ready");
    let mut ready = ReadyFile::prepare(Some(&path)).unwrap();
    let result = ready.publish_before_rename(|temporary| {
        assert_ne!(temporary, path);
        assert!(!path.exists());
        let body: serde_json::Value = serde_json::from_slice(&fs::read(temporary)?).unwrap();
        assert_eq!(body["version"], env!("CARGO_PKG_VERSION"));
        bail!("synthetic interruption before rename")
    });
    assert!(result.is_err());
    assert!(!path.exists());
    assert_eq!(fs::read_dir(dir.path()).unwrap().count(), 0);
    ready.publish().unwrap();
    assert!(path.is_file());
}

#[tokio::test]
async fn a_refused_start_never_leaves_readiness() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("ready");
    let mut args = options(&["--ready-file", path.to_str().unwrap()]);
    for invalid_config in [true, false] {
        fs::write(&path, b"stale").unwrap();
        args.max_parallel = if invalid_config { 0 } else { 1 };
        // The second case passes argument checks and fails while loading the model route.
        assert!(run(args.clone()).await.is_err());
        assert!(!path.exists());
        assert_eq!(fs::read_dir(dir.path()).unwrap().count(), 0);
    }
}

#[cfg(unix)]
#[tokio::test]
async fn readiness_signal_child() {
    let Some(path) = std::env::var_os("COORDINATOR_READINESS_TEST_PATH") else {
        return;
    };
    let mut ready = ReadyFile::prepare(Some(Path::new(&path))).unwrap();
    let shutdown = shutdown_signal(true).unwrap();
    ready.publish().unwrap();
    shutdown.await;
    ready.clear().unwrap();
}

#[cfg(unix)]
#[test]
fn shutdown_signals_remove_the_readiness_file() {
    use std::{
        process::{Command, Stdio},
        thread,
        time::{Duration, Instant},
    };
    for signal in [libc::SIGTERM, libc::SIGINT] {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("ready");
        let mut child = Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "broker::coordinator::readiness::tests::readiness_signal_child",
            ])
            .env("COORDINATOR_READINESS_TEST_PATH", &path)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        let start = Instant::now();
        while !path.is_file() && start.elapsed() < Duration::from_secs(10) {
            thread::sleep(Duration::from_millis(10));
        }
        if !path.is_file() {
            let _ = child.kill();
            let _ = child.wait();
            panic!("child did not publish readiness");
        }
        // This test owns the child; the signal is never sent to the test runner or another process.
        assert_eq!(unsafe { libc::kill(child.id() as i32, signal) }, 0);
        while start.elapsed() < Duration::from_secs(15) {
            if let Some(status) = child.try_wait().unwrap() {
                assert!(status.success());
                assert!(!path.exists());
                break;
            }
            thread::sleep(Duration::from_millis(10));
        }
        if child.try_wait().unwrap().is_none() {
            let _ = child.kill();
            let _ = child.wait();
            panic!("child did not shut down");
        }
    }
}
