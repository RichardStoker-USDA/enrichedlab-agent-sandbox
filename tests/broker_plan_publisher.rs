use enrichedlab_agent_sandbox::broker::plan_publisher::{
    CustodyPlanKind, PlanPublicationConfig, publish,
};
use sha2::{Digest, Sha256};
use std::{
    fs,
    os::unix::fs::{MetadataExt, PermissionsExt},
    path::{Path, PathBuf},
};
use tempfile::TempDir;
use uuid::Uuid;

struct Fixture {
    root: TempDir,
    source_root: PathBuf,
    plan_root: PathBuf,
    task_id: Uuid,
}
impl Fixture {
    fn new() -> Self {
        let root = tempfile::tempdir().unwrap();
        let canonical_root = root.path().canonicalize().unwrap();
        let source_root = canonical_root.join("operator");
        let plan_root = canonical_root.join("plans");
        private_dir(&source_root);
        private_dir(&plan_root);
        Self {
            root,
            source_root,
            plan_root,
            task_id: Uuid::new_v4(),
        }
    }

    fn source(&self) -> PathBuf {
        self.source_root.join("approved.json")
    }

    fn destination(&self) -> PathBuf {
        self.plan_root.join(format!("{}-1-9.json", self.task_id))
    }

    fn config(&self, expected: String) -> PlanPublicationConfig {
        PlanPublicationConfig {
            source_root: self.source_root.clone(),
            source_name: "approved.json".into(),
            source_writer_uid: uid(),
            plan_root: self.plan_root.clone(),
            plan_writer_uid: uid(),
            kind: CustodyPlanKind::Input,
            task_id: self.task_id,
            attempt: 1,
            lease_generation: 9,
            expected_sha256: expected,
        }
    }
}

fn uid() -> u32 {
    unsafe { libc::geteuid() }
}

fn private_dir(path: &Path) {
    fs::create_dir(path).unwrap();
    fs::set_permissions(path, fs::Permissions::from_mode(0o700)).unwrap();
}

fn plan(task_id: Uuid, attempt: i32, lease_generation: i64, marker: &str) -> Vec<u8> {
    format!(
        r#"{{"schemaVersion":"sandbox.enrichedlab.org/input-preparation-plan/v2","task":{{"taskId":"{task_id}","attempt":{attempt},"leaseGeneration":{lease_generation}}},"operatorOnly":"{marker}"}}"#
    ).into_bytes()
}

fn digest(bytes: &[u8]) -> String {
    format!("sha256:{}", hex::encode(Sha256::digest(bytes)))
}

fn private_file(path: &Path, bytes: &[u8]) {
    fs::write(path, bytes).unwrap();
    fs::set_permissions(path, fs::Permissions::from_mode(0o600)).unwrap();
}

#[test]
fn publishes_original_plan_bytes_once_with_safe_metadata() {
    let f = Fixture::new();
    let original = plan(f.task_id, 1, 9, "first");
    private_file(&f.source(), &original);

    let receipt = publish(&f.config(digest(&original))).unwrap();
    assert_eq!(fs::read(f.destination()).unwrap(), original);
    assert_eq!(receipt.plan_name, format!("{}-1-9.json", f.task_id));
    assert_eq!(receipt.sha256, digest(&fs::read(f.destination()).unwrap()));
    assert_eq!(
        receipt.size_bytes,
        fs::metadata(f.destination()).unwrap().len()
    );
    let metadata = fs::metadata(f.destination()).unwrap();
    assert_eq!(metadata.mode() & 0o777, 0o600);
    assert_eq!(metadata.uid(), uid());
    assert_eq!(metadata.nlink(), 1);

    assert_eq!(publish(&f.config(digest(&original))).unwrap(), receipt);
}

#[test]
fn rejects_malformed_scope_and_descriptor_aliases() {
    let f = Fixture::new();
    let original = plan(f.task_id, 1, 8, "wrong-lease");
    private_file(&f.source(), &original);
    let error = publish(&f.config(digest(&original))).unwrap_err();
    assert_eq!(error.to_string(), "plan_publisher_task_scope_mismatch");
    assert!(!f.destination().exists());

    let valid = plan(f.task_id, 1, 9, "valid");
    private_file(&f.source(), &valid);
    let wrong_digest = format!("sha256:{}", "0".repeat(64));
    assert_eq!(
        publish(&f.config(wrong_digest)).unwrap_err().to_string(),
        "plan_publisher_digest_mismatch"
    );
    let mut wrong_kind = f.config(digest(&valid));
    wrong_kind.kind = CustodyPlanKind::Output;
    assert_eq!(
        publish(&wrong_kind).unwrap_err().to_string(),
        "plan_publisher_schema_mismatch"
    );
    let symlink = f.source_root.join("symlink.json");
    std::os::unix::fs::symlink(f.source(), &symlink).unwrap();
    let mut config = f.config(digest(&valid));
    config.source_name = "symlink.json".into();
    assert_eq!(
        publish(&config).unwrap_err().to_string(),
        "plan_publisher_source_unavailable"
    );

    let hardlink = f.source_root.join("hardlink.json");
    fs::hard_link(f.source(), &hardlink).unwrap();
    config.source_name = "hardlink.json".into();
    config.source_name = "hardlink.json".into();
    assert_eq!(
        publish(&config).unwrap_err().to_string(),
        "plan_publisher_source_unavailable"
    );

    let alias = f.root.path().join("operator_alias");
    std::os::unix::fs::symlink(&f.source_root, &alias).unwrap();
    config.source_root = alias;
    config.source_name = "approved.json".into();
    assert_eq!(
        publish(&config).unwrap_err().to_string(),
        "plan_publisher_source_unavailable"
    );
}

#[test]
fn refuses_replay_that_would_replace_a_same_name_plan() {
    let f = Fixture::new();
    let first = plan(f.task_id, 1, 9, "first");
    private_file(&f.source(), &first);
    publish(&f.config(digest(&first))).unwrap();

    let replacement = plan(f.task_id, 1, 9, "replacement");
    private_file(&f.source(), &replacement);
    assert_eq!(
        publish(&f.config(digest(&replacement)))
            .unwrap_err()
            .to_string(),
        "plan_publisher_destination_conflict"
    );
    assert_eq!(fs::read(f.destination()).unwrap(), first);
}

#[test]
fn publishes_output_plan_with_its_exact_schema() {
    let f = Fixture::new();
    let original = String::from_utf8(plan(f.task_id, 1, 9, "output"))
        .unwrap()
        .replace(
            "sandbox.enrichedlab.org/input-preparation-plan/v2",
            "sandbox.enrichedlab.org/output-preparation-plan/v1",
        )
        .into_bytes();
    private_file(&f.source(), &original);
    let mut config = f.config(digest(&original));
    config.kind = CustodyPlanKind::Output;
    assert_eq!(publish(&config).unwrap().plan_kind, "output");
}

#[test]
fn refuses_legacy_input_plan_without_creating_a_destination() {
    let f = Fixture::new();
    let original = String::from_utf8(plan(f.task_id, 1, 9, "legacy"))
        .unwrap()
        .replace("input-preparation-plan/v2", "input-preparation-plan/v1")
        .into_bytes();
    private_file(&f.source(), &original);
    assert!(publish(&f.config(digest(&original))).is_err());
    assert!(!f.destination().exists());
}
