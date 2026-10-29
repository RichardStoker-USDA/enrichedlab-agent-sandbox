#![cfg(unix)]

use enrichedlab_agent_sandbox::broker::custody_dispatch::{
    CustodyDispatchChannelConfig, CustodyDispatchConfig, CustodyDispatchRoute, CustodyDispatcher,
    CustodyPreparerConfig, CustodyRouteConfig,
};
use serde_json::json;
use std::{
    fs,
    os::unix::fs::{PermissionsExt, symlink},
    path::{Path, PathBuf},
    time::{Duration, Instant},
};
use tempfile::TempDir;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

fn uid() -> u32 {
    unsafe { libc::geteuid() }
}
fn private_dir(path: &Path) {
    fs::create_dir(path).unwrap();
    fs::set_permissions(path, fs::Permissions::from_mode(0o700)).unwrap();
}
fn private(path: &Path, bytes: &[u8]) {
    fs::write(path, bytes).unwrap();
    fs::set_permissions(path, fs::Permissions::from_mode(0o600)).unwrap();
}

#[tokio::test]
async fn exact_routes_keep_principals_separate_and_reject_unenrolled_notices() {
    let mut f = Fixture::new("ok", 2, 5);
    let a = f.input(Uuid::new_v4());
    let b = f.input(Uuid::new_v4());
    let unlisted = f.input(Uuid::new_v4());
    let output = f.output(Uuid::new_v4());
    let first: serde_json::Value =
        serde_json::from_slice(&fs::read(f.root.join("input_requests").join(&a)).unwrap()).unwrap();
    let mut second: serde_json::Value =
        serde_json::from_slice(&fs::read(f.root.join("input_requests").join(&b)).unwrap()).unwrap();
    second["task"]["principalId"] = json!("00000000-0000-0000-0000-000000000002");
    second["binding"]["principalId"] = second["task"]["principalId"].clone();
    second["binding"]["principalHash"] = json!("abcdef0123456789abcdef0123456789");
    second["binding"]["namespace"] = json!("sandbox-second");
    private(
        &f.root.join("input_requests").join(&b),
        &serde_json::to_vec(&second).unwrap(),
    );
    let mut denied: serde_json::Value =
        serde_json::from_slice(&fs::read(f.root.join("input_requests").join(&unlisted)).unwrap())
            .unwrap();
    // Even the same principal and namespace cannot substitute an unlisted profile.
    denied["task"]["profileKey"] = json!("unlisted");
    denied["binding"]["profileKey"] = json!("unlisted");
    private(
        &f.root.join("input_requests").join(&unlisted),
        &serde_json::to_vec(&denied).unwrap(),
    );
    let route = |binding: serde_json::Value, name: &str| {
        let input = f.root.join(format!("child_config/{name}-input.json"));
        let output = f.root.join(format!("child_config/{name}-output.json"));
        private(&input, b"{}");
        private(&output, b"{}");
        CustodyDispatchRoute {
            binding: serde_json::from_value(binding).unwrap(),
            input: CustodyRouteConfig {
                config: input,
                preparer_config: None,
            },
            output: CustodyRouteConfig {
                config: output,
                preparer_config: None,
            },
        }
    };
    f.config.routes = vec![
        route(first["binding"].clone(), "first"),
        route(second["binding"].clone(), "second"),
    ];
    for program in [&f.config.input.program, &f.config.output.program] {
        let script = fs::read_to_string(program).unwrap().replace(
            "echo \"$request\" >>",
            "printf '%s %s\\n' \"$request\" \"$2\" >>",
        );
        private(program, script.as_bytes());
        fs::set_permissions(program, fs::Permissions::from_mode(0o700)).unwrap();
    }
    let dispatcher = CustodyDispatcher::new(f.config.clone()).unwrap();
    let receipts = dispatcher
        .run_once(&CancellationToken::new())
        .await
        .unwrap();
    assert_eq!(
        receipts.iter().filter(|r| r.outcome == "succeeded").count(),
        3
    );
    assert_eq!(
        receipts
            .iter()
            .filter(|r| r.outcome == "recovery_required")
            .count(),
        1
    );
    let invoked = fs::read_to_string(&f.marker).unwrap();
    for (name, config) in [
        (&a, "first-input.json"),
        (&b, "second-input.json"),
        (&output, "first-output.json"),
    ] {
        assert!(
            invoked
                .lines()
                .any(|line| line.starts_with(name) && line.ends_with(config))
        );
    }
    assert!(!invoked.contains(&unlisted));
    assert!(
        dispatcher
            .run_once(&CancellationToken::new())
            .await
            .unwrap()
            .is_empty()
    );
    assert_eq!(fs::read_to_string(&f.marker).unwrap(), invoked);

    let mut duplicate = f.config.clone();
    duplicate.routes.push(duplicate.routes[0].clone());
    assert!(CustodyDispatcher::new(duplicate).is_err());
    fs::set_permissions(
        &f.config.routes[0].input.config,
        fs::Permissions::from_mode(0o644),
    )
    .unwrap();
    assert!(CustodyDispatcher::new(f.config).is_err());
}

struct Fixture {
    _temp: TempDir,
    root: PathBuf,
    marker: PathBuf,
    program: PathBuf,
    config: CustodyDispatchConfig,
}
impl Fixture {
    fn new(mode: &str, concurrency: usize, timeout_seconds: u32) -> Self {
        let temp = TempDir::new().unwrap();
        let root = temp.path().canonicalize().unwrap();
        for name in [
            "state",
            "input_requests",
            "input_plans",
            "output_requests",
            "output_plans",
        ] {
            private_dir(&root.join(name));
        }
        let marker = root.join("marker");
        let program = root.join("custodian.sh");
        let output_program = root.join("output-custodian.sh");
        Self::write_program(&program, &marker, mode);
        Self::write_output_program(&output_program, &marker);
        private_dir(&root.join("child_config"));
        let child_config = root.join("child_config/child.json");
        private(&child_config, b"{}");
        let channel = |request_root: PathBuf, plan_root: PathBuf, program: PathBuf| {
            CustodyDispatchChannelConfig {
                request_root,
                request_writer_uid: uid(),
                plan_root,
                plan_writer_uid: uid(),
                program,
                program_owner_uid: uid(),
                config: child_config.clone(),
                config_owner_uid: uid(),
                timeout_seconds,
                preparer: None,
            }
        };
        let config = CustodyDispatchConfig {
            schema_version: "sandbox.enrichedlab.org/custody-dispatch/v1".into(),
            state_root: root.join("state"),
            writer_uid: uid(),
            scan_limit: 8,
            max_concurrency: concurrency,
            poll_interval_millis: 50,
            routes: vec![],
            input: channel(
                root.join("input_requests"),
                root.join("input_plans"),
                program.clone(),
            ),
            output: channel(
                root.join("output_requests"),
                root.join("output_plans"),
                output_program.clone(),
            ),
        };
        Self {
            _temp: temp,
            root,
            marker,
            program,
            config,
        }
    }
    fn write_program(program: &Path, marker: &Path, mode: &str) {
        let script = format!(
            r#"#!/bin/sh
request="$6"
task=$(printf %.36s "$request")
case "{mode}" in
  sleep) sleep 1 ;;
  fail) exit 1 ;;
esac
echo "$request" >> '{}'
printf '{{"taskId":"%s","attempt":1,"leaseGeneration":1,"inputPvcUid":"input","artifactPvcUid":"artifact","copyJobUid":"copyjob","copyPodUid":"copypod","originalSealSha256":"seal","admissionSha256":"admission"}}\n' "$task"
"#,
            marker.display()
        );
        private(program, script.as_bytes());
        fs::set_permissions(program, fs::Permissions::from_mode(0o700)).unwrap();
    }
    fn set_mode(&self, mode: &str) {
        Self::write_program(&self.program, &self.marker, mode);
    }
    fn write_output_program(program: &Path, marker: &Path) {
        let script = format!(
            r#"#!/bin/sh
request="$6"
task=$(printf %.36s "$request")
echo "$request" >> '{}'
printf '{{"schemaVersion":"sandbox.enrichedlab.org/output-custodian-receipt/v1","taskId":"%s","attempt":1,"leaseGeneration":1,"inventoryJobUid":"inventory","inventoryPodUid":"pod","inventorySha256":"inventory","terminalSha256":"terminal","fileCount":0,"sizeBytes":0,"exportObjects":[]}}\n' "$task"
"#,
            marker.display()
        );
        private(program, script.as_bytes());
        fs::set_permissions(program, fs::Permissions::from_mode(0o700)).unwrap();
    }
    fn input(&self, task: Uuid) -> String {
        let name = format!("{task}-1-1.json");
        let identity = json!({"taskId":task,"principalId":"00000000-0000-0000-0000-000000000001","groupKey":"research","profileKey":"reference","attempt":1,"leaseGeneration":1,"maxRunSeconds":60});
        let binding = json!({"principalId":"00000000-0000-0000-0000-000000000001","principalHash":"0123456789abcdef0123456789abcdef","groupKey":"research","profileKey":"reference","profileId":"example","labRef":"lab://example/research","namespace":"sandbox-example","runtimeClassName":"kata-clh"});
        private(&self.root.join("input_requests").join(&name), &serde_json::to_vec(&json!({"schemaVersion":"sandbox.enrichedlab.org/input-preparation-request/v1","task":identity,"workerId":"worker","binding":binding})).unwrap());
        private(&self.root.join("input_plans").join(&name), b"{}");
        name
    }
    fn output(&self, task: Uuid) -> String {
        let name = format!("{task}-1-1.json");
        let identity = json!({"taskId":task,"principalId":"00000000-0000-0000-0000-000000000001","groupKey":"research","profileKey":"reference","attempt":1,"leaseGeneration":1,"maxRunSeconds":60});
        let binding = json!({"principalId":"00000000-0000-0000-0000-000000000001","principalHash":"0123456789abcdef0123456789abcdef","groupKey":"research","profileKey":"reference","profileId":"example","labRef":"lab://example/research","namespace":"sandbox-example","runtimeClassName":"kata-clh"});
        let notice = json!({"schemaVersion":"sandbox.enrichedlab.org/terminal-preparation-request/v1","task":identity,"binding":binding,"handle":{"workerId":"worker","sandboxName":"run-example","sandboxUid":"sandbox-uid","sandboxGeneration":1},"completion":{"podName":"run-example","podUid":"pod-uid","runtimeClassName":"kata-clh","exitCode":0,"finishedAt":"2026-09-10T00:00:00Z"}});
        private(
            &self.root.join("output_requests").join(&name),
            &serde_json::to_vec(&notice).unwrap(),
        );
        private(&self.root.join("output_plans").join(&name), b"{}");
        name
    }
    fn calls(&self) -> usize {
        fs::read_to_string(&self.marker)
            .map(|s| s.lines().count())
            .unwrap_or(0)
    }
}

#[tokio::test]
async fn failed_handoff_requires_recovery_while_unrelated_scope_continues() {
    let fixture = Fixture::new("fail", 1, 2);
    let failed = fixture.input(Uuid::new_v4());
    let dispatcher = CustodyDispatcher::new(fixture.config.clone()).unwrap();
    let first = dispatcher
        .run_once(&CancellationToken::new())
        .await
        .unwrap();
    assert_eq!(first[0].outcome, "recovery_required");
    assert_eq!(fixture.calls(), 0);
    fixture.set_mode("success");
    let good = fixture.input(Uuid::new_v4());
    let second = dispatcher
        .run_once(&CancellationToken::new())
        .await
        .unwrap();
    assert_eq!(
        second.len(),
        1,
        "failed history does not occupy a pending slot"
    );
    assert!(second.iter().any(|item| item.outcome == "succeeded"));
    assert_eq!(
        fixture.calls(),
        1,
        "the prior partial scope was never respawned"
    );
    let third = dispatcher
        .run_once(&CancellationToken::new())
        .await
        .unwrap();
    assert!(
        third.is_empty(),
        "completed history is outside the pending queue"
    );
    assert_eq!(
        fixture.calls(),
        1,
        "completed journal prevents a repeat child effect"
    );
    assert!(fixture.root.join("input_requests").join(failed).exists());
    assert!(fixture.root.join("input_requests").join(good).exists());
}

#[tokio::test]
async fn bounded_concurrency_and_cancellation_reap_fixed_children() {
    let fixture = Fixture::new("sleep", 1, 2);
    fixture.input(Uuid::new_v4());
    fixture.input(Uuid::new_v4());
    let dispatcher = CustodyDispatcher::new(fixture.config.clone()).unwrap();
    let started = Instant::now();
    let cancel = CancellationToken::new();
    let stop = cancel.clone();
    let task = tokio::spawn(async move { dispatcher.run_once(&stop).await.unwrap() });
    tokio::time::sleep(Duration::from_millis(100)).await;
    cancel.cancel();
    let values = task.await.unwrap();
    assert!(
        values
            .iter()
            .all(|item| item.outcome == "recovery_required")
    );
    assert!(
        started.elapsed() < Duration::from_secs(1),
        "cancellation terminates and reaps the child"
    );
    assert_eq!(
        fixture.calls(),
        0,
        "cancelled child never emitted a success receipt"
    );
}

#[tokio::test]
async fn rejects_unsafe_config_and_notice_without_spawning() {
    let fixture = Fixture::new("success", 1, 2);
    let real = fixture.input(Uuid::new_v4());
    let link = fixture.root.join("input_requests").join("unsafe.json");
    symlink(fixture.root.join("input_requests").join(&real), &link).unwrap();
    let dispatcher = CustodyDispatcher::new(fixture.config.clone()).unwrap();
    let values = dispatcher
        .run_once(&CancellationToken::new())
        .await
        .unwrap();
    assert!(values.iter().any(|item| item.outcome == "succeeded"));
    assert_eq!(
        values.len(),
        1,
        "invalid notices cannot starve runnable work"
    );
    assert_eq!(fixture.calls(), 1);
    let mut unsafe_config = fixture.config.clone();
    unsafe_config.state_root = fixture.root.join("state-link");
    symlink(fixture.root.join("state"), &unsafe_config.state_root).unwrap();
    let error = CustodyDispatcher::new(unsafe_config).err().unwrap();
    assert_eq!(error.to_string(), "custody_dispatch_root_unavailable");
}

#[tokio::test]
async fn max_concurrency_serializes_fixed_children() {
    let fixture = Fixture::new("sleep", 1, 3);
    fixture.input(Uuid::new_v4());
    fixture.input(Uuid::new_v4());
    let dispatcher = CustodyDispatcher::new(fixture.config.clone()).unwrap();
    let started = Instant::now();
    let values = dispatcher
        .run_once(&CancellationToken::new())
        .await
        .unwrap();
    assert_eq!(
        values
            .iter()
            .filter(|item| item.outcome == "succeeded")
            .count(),
        2
    );
    assert!(
        started.elapsed() >= Duration::from_millis(1800),
        "maxConcurrency=1 must not overlap two fixed children"
    );
    assert_eq!(fixture.calls(), 2);
}

#[tokio::test]
async fn terminal_channel_requires_the_actual_output_receipt_schema() {
    let fixture = Fixture::new("success", 1, 2);
    fixture.output(Uuid::new_v4());
    let dispatcher = CustodyDispatcher::new(fixture.config.clone()).unwrap();
    let values = dispatcher
        .run_once(&CancellationToken::new())
        .await
        .unwrap();
    assert_eq!(values.len(), 1);
    assert_eq!(values[0].channel, "output");
    assert_eq!(values[0].outcome, "succeeded");
}

#[test]
fn accepts_root_owned_immutable_program_with_runtime_private_records() {
    let fixture = Fixture::new("success", 1, 2);
    let mut config = fixture.config.clone();
    for channel in [&mut config.input, &mut config.output] {
        channel.program = PathBuf::from("/usr/bin/true");
        channel.program_owner_uid = 0;
    }
    CustodyDispatcher::new(config).unwrap();
}

fn prepare_fixture(fixture: &mut Fixture, name: &str, wrong_digest: bool) {
    use sha2::{Digest, Sha256};
    fs::remove_file(fixture.root.join("input_plans").join(name)).unwrap();
    let task = &name[..36];
    let plan = serde_json::to_vec(&json!({"schemaVersion":"sandbox.enrichedlab.org/input-preparation-plan/v2","task":{"taskId":task,"attempt":1,"leaseGeneration":1}})).unwrap();
    let plan_file = fixture.root.join("prepared.json");
    private(&plan_file, &plan);
    let receipt = json!({"planKind":"input","taskId":task,"attempt":1,"leaseGeneration":1,"planName":name,
        "sha256":format!("sha256:{}", if wrong_digest {"0".repeat(64)} else {hex::encode(Sha256::digest(&plan))}),"sizeBytes":plan.len()});
    let program = fixture.root.join("preparer.sh");
    let script = format!(
        "#!/bin/sh\numask 077\necho prepared >> '{}'\ncp '{}' '{}'/\"$6\"\nprintf '%s\\n' '{}'\n",
        fixture.root.join("preparer_calls").display(),
        plan_file.display(),
        fixture.root.join("input_plans").display(),
        receipt
    );
    private(&program, script.as_bytes());
    fs::set_permissions(&program, fs::Permissions::from_mode(0o700)).unwrap();
    fixture.config.input.preparer = Some(CustodyPreparerConfig {
        program,
        program_owner_uid: uid(),
        config: fixture.config.input.config.clone(),
        config_owner_uid: uid(),
        timeout_seconds: 2,
    });
}

#[tokio::test]
async fn trusted_preparer_publishes_once_before_custody_and_restart() {
    let mut fixture = Fixture::new("success", 1, 2);
    let name = fixture.input(Uuid::new_v4());
    prepare_fixture(&mut fixture, &name, false);
    let notice: serde_json::Value =
        serde_json::from_slice(&fs::read(fixture.root.join("input_requests").join(&name)).unwrap())
            .unwrap();
    let routed_config = fixture.root.join("child_config/routed-custodian.json");
    let routed_preparer = fixture.root.join("child_config/routed-preparer.json");
    private(&routed_config, b"{}");
    private(&routed_preparer, b"{}");
    fixture.config.routes = vec![CustodyDispatchRoute {
        binding: serde_json::from_value(notice["binding"].clone()).unwrap(),
        input: CustodyRouteConfig {
            config: routed_config.clone(),
            preparer_config: Some(routed_preparer.clone()),
        },
        output: CustodyRouteConfig {
            config: fixture.config.output.config.clone(),
            preparer_config: None,
        },
    }];
    for (program, expected) in [
        (&fixture.program, &routed_config),
        (
            &fixture.config.input.preparer.as_ref().unwrap().program,
            &routed_preparer,
        ),
    ] {
        let script = fs::read_to_string(program).unwrap().replacen(
            "#!/bin/sh\n",
            &format!(
                "#!/bin/sh\n[ \"$2\" = '{}' ] || exit 1\n",
                expected.display()
            ),
            1,
        );
        private(program, script.as_bytes());
        fs::set_permissions(program, fs::Permissions::from_mode(0o700)).unwrap();
    }
    let dispatcher = CustodyDispatcher::new(fixture.config.clone()).unwrap();
    let result = dispatcher
        .run_once(&CancellationToken::new())
        .await
        .unwrap();
    assert_eq!(result[0].outcome, "succeeded");
    assert_eq!(fixture.calls(), 1);
    drop(dispatcher);
    let resumed = CustodyDispatcher::new(fixture.config.clone()).unwrap();
    assert!(
        resumed
            .run_once(&CancellationToken::new())
            .await
            .unwrap()
            .is_empty()
    );
    assert_eq!(fixture.calls(), 1);
    assert_eq!(
        fs::read_to_string(fixture.root.join("preparer_calls"))
            .unwrap()
            .lines()
            .count(),
        1
    );
}

#[tokio::test]
async fn prepared_bytes_must_match_receipt_before_custody() {
    let mut fixture = Fixture::new("success", 1, 2);
    let name = fixture.input(Uuid::new_v4());
    prepare_fixture(&mut fixture, &name, true);
    let dispatcher = CustodyDispatcher::new(fixture.config.clone()).unwrap();
    assert_eq!(
        dispatcher
            .run_once(&CancellationToken::new())
            .await
            .unwrap()[0]
            .outcome,
        "recovery_required"
    );
    assert_eq!(fixture.calls(), 0);
    assert!(
        dispatcher
            .run_once(&CancellationToken::new())
            .await
            .unwrap()
            .is_empty()
    );
    assert_eq!(
        fixture.calls(),
        0,
        "an existing failed preparation journal fences even an existing plan"
    );
    assert_eq!(
        fs::read_to_string(fixture.root.join("preparer_calls"))
            .unwrap()
            .lines()
            .count(),
        1
    );
}

#[tokio::test]
async fn retained_history_never_exhausts_pending_capacity() {
    let mut fixture = Fixture::new("success", 2, 2);
    fixture.config.scan_limit = 2;
    for _ in 0..7 {
        fixture.input(Uuid::new_v4());
    }
    let dispatcher = CustodyDispatcher::new(fixture.config.clone()).unwrap();
    for count in [2, 2, 2, 1, 0] {
        let receipts = dispatcher
            .run_once(&CancellationToken::new())
            .await
            .unwrap();
        assert_eq!(receipts.len(), count);
        assert!(receipts.iter().all(|r| r.outcome == "succeeded"));
    }
    assert_eq!(fixture.calls(), 7);
    assert_eq!(
        fs::read_dir(fixture.root.join("input_requests"))
            .unwrap()
            .count(),
        7
    );
    drop(dispatcher);
    fixture.input(Uuid::new_v4());
    let restarted = CustodyDispatcher::new(fixture.config).unwrap();
    assert_eq!(
        restarted
            .run_once(&CancellationToken::new())
            .await
            .unwrap()
            .len(),
        1
    );
    assert_eq!(
        fs::read_to_string(&fixture.marker).unwrap().lines().count(),
        8
    );
}

#[test]
fn catalog_error_codes_are_preserved_without_transport_text() {
    use enrichedlab_agent_sandbox::broker::workload_site::is_safe_error_code;
    assert!(is_safe_error_code("catalog_output_scope_mismatch"));
    assert!(!is_safe_error_code(
        "catalog_output_scope_mismatch: private response"
    ));
}
