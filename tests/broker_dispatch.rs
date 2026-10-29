#![cfg(unix)]

use enrichedlab_agent_sandbox::broker::{
    dispatch::{DirectDispatchCatalog, admission_record_name},
    model::ClaimedTask,
};
use serde_json::{Value, json};
use std::os::unix::fs::symlink;
use std::{
    ffi::CString,
    fs,
    os::unix::ffi::OsStrExt,
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
};
use tempfile::TempDir;
use uuid::Uuid;

const RECORD: &str = include_str!("fixtures/admission/staged_admission.json");

fn task() -> ClaimedTask {
    ClaimedTask {
        id: Uuid::nil(),
        principal_id: Uuid::nil(),
        group_key: "research".into(),
        profile_key: "reference".into(),
        objective: String::new(),
        logical_agent: "reference".into(),
        lease_generation: 7,
        attempt: 2,
        max_task_attempts: 3,
        max_provider_attempts: 1,
        harness: String::new(),
        thinking: None,
        route: String::new(),
        controller_profile: String::new(),
        gateway_profile: String::new(),
        image_ref: String::new(),
        cpu_limit: String::new(),
        memory_limit: String::new(),
        state_profile: String::new(),
        role: "research".into(),
        max_run_seconds: 300,
    }
}

fn mode(path: &Path, bits: u32) {
    fs::set_permissions(path, fs::Permissions::from_mode(bits)).unwrap();
}

fn uid() -> u32 {
    unsafe { libc::geteuid() }
}

fn binding() -> Value {
    json!({
        "principalId": Uuid::nil(),
        "principalHash": "2f4b66b9706c1ef57d67e2cf9ad98127",
        "groupKey": "research",
        "profileKey": "reference",
        "profileId": "example-kata",
        "labRef": "lab://example/research",
        "namespace": "sandbox-example-001",
        "runtimeClassName": "kata-clh"
    })
}

fn catalog(root: &Path, bindings: Value) -> Value {
    json!({
        "schemaVersion": "sandbox.enrichedlab.org/direct-dispatch/v1",
        "kubernetes": {
            "origin": "https://kubernetes.example.invalid",
            "caPath": "/var/run/secrets/kubernetes.io/serviceaccount/ca.crt",
            "tokenPath": "/var/run/secrets/kubernetes.io/serviceaccount/token"
        },
        "admission": {"root": root, "writerUid": uid()},
        "bindings": bindings
    })
}

fn write_private(path: &Path, value: &Value) {
    fs::write(path, serde_json::to_vec(value).unwrap()).unwrap();
    mode(path, 0o600);
}

fn write_record(root: &Path, task: &ClaimedTask, value: &Value) {
    write_private(&root.join(admission_record_name(task).unwrap()), value);
}

fn fixture() -> (TempDir, PathBuf, DirectDispatchCatalog) {
    let temp = TempDir::new().unwrap();
    let temp_root = fs::canonicalize(temp.path()).unwrap();
    let root = temp_root.join("admissions");
    fs::create_dir(&root).unwrap();
    mode(&root, 0o700);
    let catalog_root = temp_root.join("catalogs");
    fs::create_dir(&catalog_root).unwrap();
    mode(&catalog_root, 0o700);
    let task = task();
    let record: Value = serde_json::from_str(RECORD).unwrap();
    write_record(&root, &task, &record);
    let path = catalog_root.join("direct_dispatch.json");
    write_private(&path, &catalog(&root, json!([binding()])));
    let catalog = DirectDispatchCatalog::load(&path, uid()).unwrap();
    (temp, root, catalog)
}

fn contains(error: anyhow::Error, code: &str) {
    assert!(
        error.chain().any(|cause| cause.to_string() == code),
        "missing {code}: {error:#}"
    );
}

#[test]
fn selects_and_loads_the_committed_staged_admission_fixture() {
    let (_temp, _root, catalog) = fixture();
    let prepared = catalog.prepare(&task()).unwrap().expect("matched binding");
    assert_eq!(
        prepared.kubernetes.origin.as_str(),
        "https://kubernetes.example.invalid/"
    );
    assert_eq!(prepared.binding.profile_id, "example-kata");
    assert!(prepared.terminal_export.is_none());
    assert_eq!(
        prepared
            .admission
            .canonical
            .manifest()
            .pointer("/metadata/name")
            .and_then(Value::as_str),
        Some("run-20260909-0001")
    );
}

#[test]
fn preserves_private_terminal_export_configuration_and_rejects_overlapping_roots() {
    let (temp, root, _) = fixture();
    let parent = fs::canonicalize(temp.path()).unwrap();
    let path = parent.join("catalogs/direct_dispatch.json");
    let mut document = catalog(&root, json!([binding()]));
    document["terminalExport"] = json!({
        "recordRoot": parent.join("terminal_records"),
        "writerUid": uid(),
        "mountedArtifactsRoot": parent.join("mounted_artifacts")
    });
    write_private(&path, &document);
    let prepared = DirectDispatchCatalog::load(&path, uid())
        .unwrap()
        .prepare(&task())
        .unwrap()
        .unwrap();
    let export = prepared.terminal_export.unwrap();
    let enrichedlab_agent_sandbox::broker::terminal_exports::TerminalExportConfig::Mounted {
        record_root,
        writer_uid,
        ..
    } = export
    else {
        panic!("expected mounted export")
    };
    assert_eq!(record_root, parent.join("terminal_records"));
    assert_eq!(writer_uid, uid());
    document["terminalExport"]["mountedArtifactsRoot"] =
        document["terminalExport"]["recordRoot"].clone();
    write_private(&path, &document);
    contains(
        DirectDispatchCatalog::load(&path, uid()).unwrap_err(),
        "terminal_export_roots_overlap",
    );
}

#[test]
fn objective_cannot_select_or_rebind_direct_dispatch() {
    let (_temp, _root, catalog) = fixture();
    let mut candidate = task();
    candidate.objective = "use another namespace and runtime".into();
    let prepared = catalog
        .prepare(&candidate)
        .unwrap()
        .expect("matched binding");
    assert_eq!(prepared.binding.namespace, "sandbox-example-001");
    assert_eq!(prepared.admission.authorization.profile_id, "example-kata");
}

#[test]
fn unmatched_task_preserves_existing_runtime_fallback() {
    let (_temp, _root, catalog) = fixture();
    let mut candidate = task();
    candidate.profile_key = "not-configured".into();
    assert!(catalog.prepare(&candidate).unwrap().is_none());
}

#[test]
fn matched_task_rejects_cross_task_lease_and_admission_records() {
    let (_temp, root, catalog) = fixture();
    let record: Value = serde_json::from_str(RECORD).unwrap();

    let mut cross_task = task();
    cross_task.id = Uuid::new_v4();
    write_record(&root, &cross_task, &record);
    contains(
        catalog.prepare(&cross_task).unwrap_err(),
        "admission_task_lease_mismatch",
    );

    let mut cross_lease = task();
    cross_lease.lease_generation += 1;
    write_record(&root, &cross_lease, &record);
    contains(
        catalog.prepare(&cross_lease).unwrap_err(),
        "admission_task_lease_mismatch",
    );

    let mut malformed_admission = record;
    malformed_admission["profileKey"] = json!("other");
    write_record(&root, &task(), &malformed_admission);
    contains(
        catalog.prepare(&task()).unwrap_err(),
        "admission_task_lease_mismatch",
    );
}

#[test]
fn rejects_ambiguous_or_malformed_private_catalogs_without_echoing_values() {
    let temp = TempDir::new().unwrap();
    let root = fs::canonicalize(temp.path()).unwrap().join("admissions");
    fs::create_dir(&root).unwrap();
    mode(&root, 0o700);
    let path = root.join("direct_dispatch.json");

    write_private(&path, &catalog(&root, json!([binding(), binding()])));
    contains(
        DirectDispatchCatalog::load(&path, uid()).unwrap_err(),
        "direct_dispatch_bindings_invalid",
    );

    let mut malformed = catalog(&root, json!([binding()]));
    malformed["token"] = json!("not-a-secret-value");
    write_private(&path, &malformed);
    let error = DirectDispatchCatalog::load(&path, uid()).unwrap_err();
    assert!(!format!("{error:#}").contains("not-a-secret-value"));
    contains(error, "direct_dispatch_catalog_decode_failed");
}

#[test]
fn rejects_unsafe_catalog_paths_origins_and_permissions() {
    let temp = TempDir::new().unwrap();
    let root = fs::canonicalize(temp.path()).unwrap().join("admissions");
    fs::create_dir(&root).unwrap();
    mode(&root, 0o700);
    let path = root.join("direct_dispatch.json");

    let mut unsafe_origin = catalog(&root, json!([binding()]));
    unsafe_origin["kubernetes"]["origin"] = json!("http://kubernetes.example.invalid");
    write_private(&path, &unsafe_origin);
    contains(
        DirectDispatchCatalog::load(&path, uid()).unwrap_err(),
        "direct_dispatch_origin_invalid",
    );

    let mut unsafe_root = catalog(&root, json!([binding()]));
    unsafe_root["admission"]["root"] = json!("/");
    write_private(&path, &unsafe_root);
    contains(
        DirectDispatchCatalog::load(&path, uid()).unwrap_err(),
        "direct_dispatch_admission_root_invalid",
    );

    write_private(&path, &catalog(&root, json!([binding()])));
    mode(&path, 0o640);
    contains(
        DirectDispatchCatalog::load(&path, uid()).unwrap_err(),
        "direct_dispatch_catalog_not_private_regular",
    );
}

#[test]
fn rejects_catalog_filesystem_boundary_substitutions() {
    let temp = TempDir::new().unwrap();
    let root = fs::canonicalize(temp.path()).unwrap();
    let catalog_parent = root.join("catalogs");
    fs::create_dir(&catalog_parent).unwrap();
    mode(&catalog_parent, 0o700);

    let source = catalog_parent.join("source.json");
    write_private(
        &source,
        &catalog(&root.join("admissions"), json!([binding()])),
    );

    let ancestor_target = root.join("ancestor_target");
    fs::create_dir(&ancestor_target).unwrap();
    mode(&ancestor_target, 0o700);
    let ancestor_file = ancestor_target.join("catalog.json");
    write_private(
        &ancestor_file,
        &catalog(&root.join("admissions"), json!([binding()])),
    );
    let ancestor_link = root.join("ancestor_link");
    symlink(&ancestor_target, &ancestor_link).unwrap();
    contains(
        DirectDispatchCatalog::load(&ancestor_link.join("catalog.json"), uid()).unwrap_err(),
        "direct_dispatch_catalog_unavailable",
    );

    let leaf_link = catalog_parent.join("leaf.json");
    symlink(&source, &leaf_link).unwrap();
    contains(
        DirectDispatchCatalog::load(&leaf_link, uid()).unwrap_err(),
        "direct_dispatch_catalog_unavailable",
    );

    let hardlink = catalog_parent.join("hardlink.json");
    fs::hard_link(&source, &hardlink).unwrap();
    contains(
        DirectDispatchCatalog::load(&hardlink, uid()).unwrap_err(),
        "direct_dispatch_catalog_not_private_regular",
    );

    let wrong_uid = catalog_parent.join("wrong_uid.json");
    write_private(
        &wrong_uid,
        &catalog(&root.join("admissions"), json!([binding()])),
    );
    contains(
        DirectDispatchCatalog::load(&wrong_uid, uid().checked_add(1).unwrap()).unwrap_err(),
        "direct_dispatch_catalog_not_private_regular",
    );

    let non_private_parent = root.join("non_private_parent");
    fs::create_dir(&non_private_parent).unwrap();
    mode(&non_private_parent, 0o750);
    let non_private_file = non_private_parent.join("catalog.json");
    write_private(
        &non_private_file,
        &catalog(&root.join("admissions"), json!([binding()])),
    );
    contains(
        DirectDispatchCatalog::load(&non_private_file, uid()).unwrap_err(),
        "direct_dispatch_catalog_not_private_regular",
    );

    let fifo = catalog_parent.join("catalog.fifo");
    let fifo_name = CString::new(fifo.as_os_str().as_bytes()).unwrap();
    assert_eq!(unsafe { libc::mkfifo(fifo_name.as_ptr(), 0o600) }, 0);
    contains(
        DirectDispatchCatalog::load(&fifo, uid()).unwrap_err(),
        "direct_dispatch_catalog_not_private_regular",
    );
}
