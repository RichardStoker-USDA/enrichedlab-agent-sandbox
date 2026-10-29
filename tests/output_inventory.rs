use enrichedlab_agent_sandbox::output_inventory::{
    INVENTORY_SCHEMA, OutputInventoryRequest, REQUEST_SCHEMA, SelectedOutput, inventory,
};
use sha2::{Digest, Sha256};
use std::{fs, path::PathBuf};
use tempfile::TempDir;
use tokio_util::sync::CancellationToken;

fn request(root: PathBuf, files: Vec<SelectedOutput>, max_bytes: u64) -> OutputInventoryRequest {
    OutputInventoryRequest {
        schema_version: REQUEST_SCHEMA.into(),
        mounted_root: root,
        files,
        max_files: 8,
        max_bytes,
    }
}
fn selected(path: &str) -> SelectedOutput {
    SelectedOutput {
        relative_path: path.into(),
        visibility: "researcher".into(),
        role: "result".into(),
        label: "result".into(),
        media_type: "text/plain".into(),
    }
}
fn root() -> (TempDir, PathBuf) {
    let temp = TempDir::new().unwrap();
    let root = temp.path().canonicalize().unwrap();
    (temp, root)
}

#[test]
fn measures_original_file_bytes_without_emitting_content() {
    let (_temp, root) = root();
    fs::create_dir(root.join("reports")).unwrap();
    let bytes = b"measured original bytes\n";
    fs::write(root.join("reports/result.txt"), bytes).unwrap();
    let inventory = inventory(
        &request(root, vec![selected("reports/result.txt")], 1024),
        &CancellationToken::new(),
    )
    .unwrap();
    assert_eq!(inventory.schema_version, INVENTORY_SCHEMA);
    assert_eq!(inventory.files.len(), 1);
    assert_eq!(inventory.files[0].size_bytes, bytes.len() as u64);
    assert_eq!(
        inventory.files[0].sha256,
        hex::encode(Sha256::digest(bytes))
    );
    let encoded = serde_json::to_string(&inventory).unwrap();
    assert!(!encoded.contains("measured original bytes"));
    assert!(inventory.selection_digest.starts_with("sha256:"));
}

#[cfg(unix)]
#[test]
fn rejects_selected_symlink() {
    use std::os::unix::fs::symlink;
    let (_temp, root) = root();
    fs::write(root.join("actual.txt"), b"x").unwrap();
    symlink("actual.txt", root.join("link.txt")).unwrap();
    let error = inventory(
        &request(root, vec![selected("link.txt")], 1024),
        &CancellationToken::new(),
    )
    .unwrap_err();
    assert_eq!(error.to_string(), "output_inventory_file_unavailable");
}

#[test]
fn rejects_aggregate_overflow_before_result() {
    let (_temp, root) = root();
    fs::write(root.join("large.txt"), vec![0_u8; 33]).unwrap();
    let error = inventory(
        &request(root, vec![selected("large.txt")], 32),
        &CancellationToken::new(),
    )
    .unwrap_err();
    assert_eq!(error.to_string(), "output_inventory_limits_exceeded");
}

#[test]
fn rejects_cancelled_inventory() {
    let (_temp, root) = root();
    fs::write(root.join("result.txt"), b"x").unwrap();
    let cancel = CancellationToken::new();
    cancel.cancel();
    let error = inventory(&request(root, vec![selected("result.txt")], 1024), &cancel).unwrap_err();
    assert_eq!(error.to_string(), "output_inventory_cancelled");
}
