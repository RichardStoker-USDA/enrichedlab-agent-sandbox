#![cfg(unix)]

extern crate enrichedlab_agent_sandbox as application;

#[path = "../src/broker/export_identity.rs"]
mod export_identity_source;

mod broker {
    pub(crate) use crate::export_identity_source as export_identity;
    pub use ::application::broker::*;
}

#[path = "../src/broker/export_client_staging.rs"]
mod export_client_staging;

use broker::{
    export_identity::{ExportCredentialLifetime, issue_export_credentials},
    export_transport::{ExportScope, RemoteExportConfig},
};
use export_client_staging::stage_export_client;
use std::{
    fs,
    os::unix::fs::{MetadataExt, PermissionsExt},
    path::{Path, PathBuf},
};
use tempfile::TempDir;
use uuid::Uuid;

fn uid() -> u32 {
    unsafe { libc::geteuid() }
}

fn private_dir(path: &Path) {
    fs::create_dir_all(path).unwrap();
    fs::set_permissions(path, fs::Permissions::from_mode(0o700)).unwrap();
}

fn remote(root: PathBuf) -> RemoteExportConfig {
    RemoteExportConfig {
        dns_suffix: "svc.cluster.test".into(),
        port: 8443,
        credential_root: root,
        writer_uid: uid(),
    }
}

fn credentials(generation: i64) -> broker::export_identity::IssuedExportCredentials {
    issue_export_credentials(
        &remote("/run/export-clients".into()),
        ExportScope::new(Uuid::nil(), 2, generation).unwrap(),
        "sandbox-example-001",
        ExportCredentialLifetime {
            task_seconds: 300,
            export_seconds: 60,
        },
    )
    .unwrap()
}

fn fixture() -> (TempDir, PathBuf) {
    let temporary = tempfile::Builder::new()
        .prefix("enrichedlab-export-client-")
        .tempdir_in("/tmp")
        .unwrap();
    let root = fs::canonicalize(temporary.path())
        .unwrap()
        .join("credentials");
    private_dir(&root);
    (temporary, root)
}

#[test]
fn stages_exact_private_bundle_and_accepts_an_identical_retry() {
    let (_temporary, root) = fixture();
    let credentials = credentials(7);
    let server = credentials.server_files();
    assert!(
        !server.ca_pem.is_empty()
            && !server.server_pem.is_empty()
            && !server.server_key_pem.is_empty()
    );
    let receipt = stage_export_client(&remote(root.clone()), &credentials).unwrap();
    assert_eq!(receipt.scope, credentials.metadata().scope);
    assert_eq!(
        receipt.client_fingerprint_sha256,
        credentials.metadata().client_fingerprint_sha256
    );
    assert_eq!(receipt.expires_at, credentials.metadata().expires_at);
    let directory = root.join(receipt.scope.credential_name());
    assert_eq!(fs::metadata(&directory).unwrap().mode() & 0o777, 0o700);
    assert_eq!(fs::metadata(&directory).unwrap().uid(), uid());
    let client = credentials.client_files();
    assert_eq!(fs::read(directory.join("ca.pem")).unwrap(), client.ca_pem);
    assert_eq!(
        fs::read(directory.join("client.pem")).unwrap(),
        client.client_pem
    );
    for name in ["ca.pem", "client.pem"] {
        let metadata = fs::metadata(directory.join(name)).unwrap();
        assert_eq!(metadata.mode() & 0o777, 0o600);
        assert_eq!(metadata.uid(), uid());
        assert_eq!(metadata.nlink(), 1);
    }
    assert_eq!(
        stage_export_client(&remote(root), &credentials).unwrap(),
        receipt
    );
}

#[test]
fn rejects_symlink_or_conflicting_existing_bundle_without_partial_publication() {
    let (_temporary, root) = fixture();
    let credentials = credentials(7);
    let scope = credentials.metadata().scope.clone();
    let target = root.join(scope.credential_name());
    let outside = root.parent().unwrap().join("outside");
    private_dir(&outside);
    std::os::unix::fs::symlink(&outside, &target).unwrap();
    assert!(stage_export_client(&remote(root.clone()), &credentials).is_err());
    assert!(
        fs::symlink_metadata(&target)
            .unwrap()
            .file_type()
            .is_symlink()
    );
    fs::remove_file(&target).unwrap();

    private_dir(&target);
    fs::write(target.join("ca.pem"), b"different").unwrap();
    fs::set_permissions(target.join("ca.pem"), fs::Permissions::from_mode(0o600)).unwrap();
    assert!(stage_export_client(&remote(root.clone()), &credentials).is_err());
    assert_eq!(fs::read(target.join("ca.pem")).unwrap(), b"different");
    assert!(fs::read_dir(&root).unwrap().all(|entry| {
        !entry
            .unwrap()
            .file_name()
            .to_string_lossy()
            .contains(".partial.")
    }));

    fs::remove_dir_all(&target).unwrap();
    fs::set_permissions(&root, fs::Permissions::from_mode(0o755)).unwrap();
    assert!(stage_export_client(&remote(root), &credentials).is_err());
}
