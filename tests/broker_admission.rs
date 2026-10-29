#![cfg(unix)]

use enrichedlab_agent_sandbox::{
    broker::{admission::load, model::ClaimedTask, sandbox::SandboxBinding},
    custody::{InputManifest, canonical_document_digest},
};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    fs,
    os::unix::fs::{PermissionsExt, symlink},
    path::{Path, PathBuf},
};
use tempfile::TempDir;
use uuid::Uuid;

const RECORD: &str = include_str!("fixtures/admission/staged_admission.json");
const PAYLOAD: &[u8] = include_bytes!("fixtures/admission/fixture");

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

fn binding() -> SandboxBinding {
    SandboxBinding {
        principal_id: Uuid::nil(),
        principal_hash: "2f4b66b9706c1ef57d67e2cf9ad98127".into(),
        group_key: "research".into(),
        profile_key: "reference".into(),
        profile_id: "example-kata".into(),
        lab_ref: "lab://example/research".into(),
        namespace: "sandbox-example-001".into(),
        runtime_class_name: "kata-clh".into(),
    }
}

fn uid() -> u32 {
    unsafe { libc::geteuid() }
}
fn mode(p: &Path, bits: u32) {
    fs::set_permissions(p, fs::Permissions::from_mode(bits)).unwrap();
}
fn write(root: &Path, value: &Value) {
    let path = root.join("record.json");
    fs::write(&path, serde_json::to_vec(value).unwrap()).unwrap();
    mode(&path, 0o600);
}
fn fixture() -> (TempDir, PathBuf, Value) {
    let temp = TempDir::new().unwrap();
    let root = fs::canonicalize(temp.path()).unwrap().join("admissions");
    fs::create_dir(&root).unwrap();
    mode(&root, 0o700);
    let record = serde_json::from_str(RECORD).unwrap();
    write(&root, &record);
    (temp, root, record)
}
fn rejected(root: &Path, name: &str, expected: &str) {
    let err = load(root, name, uid(), &task(), &binding())
        .unwrap_err()
        .to_string();
    assert_eq!(err, expected);
}

#[test]
fn admits_custodian_output_and_canonical_renderer_manifest() {
    let (_temp, root, record) = fixture();
    let admitted = load(&root, "record.json", uid(), &task(), &binding()).unwrap();
    assert_eq!(admitted.canonical.manifest(), &record["sandbox"]);
    let manifest: InputManifest =
        serde_json::from_str(include_str!("fixtures/admission/input_manifest.json")).unwrap();
    assert_eq!(
        canonical_document_digest(&manifest).unwrap(),
        admitted.sealed_input.manifest_digest
    );
    assert_eq!(manifest.files.len(), 1);
    assert_eq!(manifest.files[0].size_bytes, PAYLOAD.len() as u64);
    assert_eq!(
        manifest.files[0].sha256,
        format!("sha256:{}", hex::encode(Sha256::digest(PAYLOAD)))
    );
}

#[test]
fn rejects_rebound_tasks_leases_and_server_bindings() {
    let (_temp, root, record) = fixture();
    for (field, value) in [
        ("taskId", json!(Uuid::new_v4())),
        ("principalId", json!(Uuid::new_v4())),
        ("attempt", json!(1)),
        ("leaseGeneration", json!(6)),
        ("groupKey", json!("other")),
        ("profileKey", json!("other")),
    ] {
        let mut changed = record.clone();
        changed[field] = value;
        write(&root, &changed);
        rejected(&root, "record.json", "admission_task_lease_mismatch");
    }
    write(&root, &record);
    let mut other = binding();
    other.namespace = "sandbox-other".into();
    assert_eq!(
        load(&root, "record.json", uid(), &task(), &other)
            .unwrap_err()
            .to_string(),
        "admission_renderer_binding_mismatch"
    );
    let mut other = binding();
    other.lab_ref = "lab://example/other".into();
    assert_eq!(
        load(&root, "record.json", uid(), &task(), &other)
            .unwrap_err()
            .to_string(),
        "admission_renderer_binding_mismatch"
    );
}

#[test]
fn rejects_seal_and_rendered_workload_tampering() {
    let (_temp, root, record) = fixture();
    let mut changed = record.clone();
    changed["sealedInput"]["sealedAt"] = json!("2026-09-09T00:00:00Z");
    write(&root, &changed);
    rejected(&root, "record.json", "admission_staging_digest_mismatch");
    // Updating only the outer digest cannot conceal a broken self-digest.
    changed["authorization"]["stagingDigest"] =
        json!(canonical_document_digest(&changed["sealedInput"]).unwrap());
    write(&root, &changed);
    rejected(
        &root,
        "record.json",
        "admission_attestation_digest_mismatch",
    );
    let mut changed = record.clone();
    changed["sealedInput"]["pvcName"] = json!("input-other");
    write(&root, &changed);
    rejected(&root, "record.json", "admission_sealed_input_mismatch");
    let mut changed = record.clone();
    changed["sandbox"]["spec"]["volumeClaimTemplates"] = json!([]);
    write(&root, &changed);
    rejected(&root, "record.json", "admission_renderer_binding_mismatch");
    let mut changed = record;
    changed["sandbox"]["spec"]["podTemplate"]["spec"]["volumes"][0]["persistentVolumeClaim"]["readOnly"] =
        json!(false);
    write(&root, &changed);
    rejected(&root, "record.json", "admission_renderer_binding_mismatch");
}

#[test]
fn rejects_unsafe_names_roots_links_owners_and_modes() {
    let (temp, root, _record) = fixture();
    for name in [
        "",
        ".",
        "..",
        "../record.json",
        "nested/record.json",
        "nested\\record.json",
        "record\0.json",
    ] {
        rejected(&root, name, "admission_name_invalid");
    }
    rejected(
        Path::new("relative"),
        "record.json",
        "admission_root_invalid",
    );
    rejected(Path::new("/"), "record.json", "admission_root_invalid");
    let parent = fs::canonicalize(temp.path()).unwrap();
    symlink(&root, parent.join("root_alias")).unwrap();
    rejected(
        &parent.join("root_alias"),
        "record.json",
        "admission_root_unavailable",
    );
    symlink(&parent, parent.join("parent_alias")).unwrap();
    rejected(
        &parent.join("parent_alias/admissions"),
        "record.json",
        "admission_root_unavailable",
    );
    symlink(root.join("record.json"), root.join("file_alias")).unwrap();
    rejected(&root, "file_alias", "admission_record_unavailable");
    fs::hard_link(root.join("record.json"), root.join("hard_link")).unwrap();
    rejected(&root, "hard_link", "admission_record_not_private_regular");
    fs::remove_file(root.join("hard_link")).unwrap();
    assert_eq!(
        load(
            &root,
            "record.json",
            uid().wrapping_add(1),
            &task(),
            &binding()
        )
        .unwrap_err()
        .to_string(),
        "admission_root_not_private"
    );
    mode(&root, 0o750);
    rejected(&root, "record.json", "admission_root_not_private");
    mode(&root, 0o700);
    mode(&root.join("record.json"), 0o640);
    rejected(&root, "record.json", "admission_record_not_private_regular");
}

#[test]
fn rejects_directories_fifos_and_oversized_records_without_reading() {
    use std::{ffi::CString, os::unix::ffi::OsStrExt};
    let (_temp, root, _) = fixture();
    fs::create_dir(root.join("directory")).unwrap();
    mode(&root.join("directory"), 0o700);
    rejected(&root, "directory", "admission_record_not_private_regular");
    let fifo = CString::new(root.join("fifo").as_os_str().as_bytes()).unwrap();
    assert_eq!(unsafe { libc::mkfifo(fifo.as_ptr(), 0o600) }, 0);
    rejected(&root, "fifo", "admission_record_not_private_regular");
    let file = fs::File::create(root.join("large")).unwrap();
    mode(&root.join("large"), 0o600);
    file.set_len(256 * 1024 + 1).unwrap();
    rejected(&root, "large", "admission_size_exceeded");
}
