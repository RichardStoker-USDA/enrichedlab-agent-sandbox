#![cfg(unix)]

extern crate enrichedlab_agent_sandbox as application;

mod custody {
    pub use ::application::custody::*;
}

#[path = "../src/broker/input_completion.rs"]
mod input_completion;

use anyhow::Result;
use async_trait::async_trait;
use custody::{
    CopySealRequest, DeclaredFile, InputManifest, canonical_document_digest, copy_and_seal,
};
use input_completion::{
    InputCompletionMetadata, InputCopyContainerExpectation, InputCopyJobExpectation,
    InputCopyObservationTransport, InputCopyVolumeExpectation, InputCopyVolumeSource,
    observe_input_seal,
};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::fs;
use tempfile::TempDir;
use time::{OffsetDateTime, format_description::well_known::Rfc3339};

const HELPER_IMAGE: &str = "registry.test/custodian@sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
const JOB_UID: &str = "job-uid-1";
const POD_UID: &str = "pod-uid-1";

#[derive(Clone)]
struct FixtureTransport {
    job: Value,
    pod: Option<Value>,
    stdout: Vec<u8>,
}

#[async_trait]
impl InputCopyObservationTransport for FixtureTransport {
    async fn get_job(&self, _namespace: &str, _name: &str) -> Result<Value> {
        Ok(self.job.clone())
    }

    async fn get_controlled_pod(&self, _namespace: &str, _job_name: &str) -> Result<Option<Value>> {
        Ok(self.pod.clone())
    }

    async fn helper_stdout(
        &self,
        _namespace: &str,
        _pod_name: &str,
        _container: &str,
    ) -> Result<Vec<u8>> {
        Ok(self.stdout.clone())
    }
}

fn request(temp: &TempDir) -> CopySealRequest {
    let source = temp.path().join("source");
    let destination = temp.path().join("destination");
    let custody_root = temp.path().join("custody");
    fs::create_dir_all(source.join("scientific")).unwrap();
    fs::create_dir(&destination).unwrap();
    fs::create_dir(&custody_root).unwrap();
    let bytes = b"synthetic fixture\n";
    fs::write(source.join("scientific/fixture.txt"), bytes).unwrap();
    let file = DeclaredFile {
        path: "scientific/fixture.txt".into(),
        size_bytes: bytes.len() as u64,
        sha256: format!("sha256:{}", hex::encode(Sha256::digest(bytes))),
    };
    let mut request = CopySealRequest {
        schema_version: "sandbox.enrichedlab.org/input-staging/v1".into(),
        staging_id: "stage-1".into(),
        run_id: "run-1".into(),
        tenant_id: "tenant-1".into(),
        namespace: "sandbox-tenant-1".into(),
        principal_hash: "a".repeat(32),
        input_ref: "input://fixture/source".into(),
        input_digest: String::new(),
        pvc_name: "input-run-1".into(),
        manifest_ref: "artifact://fixture/manifest".into(),
        attestation_ref: "receipt://fixture/seal".into(),
        source_root: fs::canonicalize(source).unwrap(),
        destination_root: fs::canonicalize(destination).unwrap(),
        custody_root: fs::canonicalize(custody_root).unwrap(),
        files: vec![file],
        max_files: Some(1),
        max_bytes: Some(bytes.len() as u64),
        admitted_files: vec![],
    };
    request.input_digest = canonical_document_digest(&InputManifest {
        schema_version: request.schema_version.clone(),
        staging_id: request.staging_id.clone(),
        run_id: request.run_id.clone(),
        files: request.files.clone(),
        total_bytes: bytes.len() as u64,
    })
    .unwrap();
    request
}

fn expectation() -> InputCopyJobExpectation {
    InputCopyJobExpectation {
        namespace: "sandbox-tenant-1".into(),
        job_name: "input-copy-run-1".into(),
        job_uid: JOB_UID.into(),
        run_id: "run-1".into(),
        principal_hash: "a".repeat(32),
        runtime_class_name: "kata-clh".into(),
        helper: InputCopyContainerExpectation {
            name: "custodian".into(),
            image: HELPER_IMAGE.into(),
            command: vec!["/usr/local/bin/agent-sandbox-custodian".into()],
            args: vec!["--request".into(), "/request/request.json".into()],
        },
        volumes: vec![
            InputCopyVolumeExpectation {
                role: "source".into(),
                name: "source".into(),
                mount_path: "/source".into(),
                sub_path: Some("payload".into()),
                read_only: true,
                source: InputCopyVolumeSource::PersistentVolumeClaim {
                    claim_name: "input-source-run-1".into(),
                },
            },
            InputCopyVolumeExpectation {
                role: "destination".into(),
                name: "destination".into(),
                mount_path: "/destination".into(),
                sub_path: None,
                read_only: false,
                source: InputCopyVolumeSource::PersistentVolumeClaim {
                    claim_name: "input-run-1".into(),
                },
            },
            InputCopyVolumeExpectation {
                role: "custody".into(),
                name: "custody".into(),
                mount_path: "/custody".into(),
                sub_path: None,
                read_only: false,
                source: InputCopyVolumeSource::EmptyDirMemory,
            },
            InputCopyVolumeExpectation {
                role: "request".into(),
                name: "request".into(),
                mount_path: "/request".into(),
                sub_path: None,
                read_only: true,
                source: InputCopyVolumeSource::ProjectedSecret {
                    secret_name: "input-copy-request-run-1".into(),
                },
            },
        ],
    }
}

fn observation(stdout: Vec<u8>) -> FixtureTransport {
    let expected = expectation();
    let volume_mounts = expected
        .volumes
        .iter()
        .map(|volume| {
            let mut mount = json!({
                "name": volume.name,
                "mountPath": volume.mount_path,
                "readOnly": volume.read_only,
            });
            if let Some(sub_path) = &volume.sub_path {
                mount["subPath"] = json!(sub_path);
            }
            mount
        })
        .collect::<Vec<_>>();
    let volumes = json!([
        {"name":"source", "persistentVolumeClaim":{"claimName":"input-source-run-1", "readOnly":true}},
        {"name":"destination", "persistentVolumeClaim":{"claimName":"input-run-1", "readOnly":false}},
        {"name":"custody", "emptyDir":{"medium":"Memory"}},
        {"name":"request", "projected":{"sources":[{"secret":{"name":"input-copy-request-run-1", "optional":false}}]}},
    ]);
    FixtureTransport {
        job: json!({
            "metadata":{"name":expected.job_name,"namespace":expected.namespace,"uid":JOB_UID,"generation":1},
            "spec":{"completions":1,"parallelism":1,"backoffLimit":0},
            "status":{"succeeded":1,"active":0,"failed":0,"conditions":[{"type":"Complete","status":"True"}]},
        }),
        pod: Some(json!({
            "metadata":{
                "name":"input-copy-run-1-z9x2k", "namespace":expected.namespace, "uid":POD_UID,
                "ownerReferences":[{"apiVersion":"batch/v1","kind":"Job","uid":JOB_UID,"controller":true}]
            },
            "spec":{
                "runtimeClassName":"kata-clh", "restartPolicy":"Never", "automountServiceAccountToken":false,
                "enableServiceLinks":false,
                "securityContext":{"runAsNonRoot":true,"runAsUser":65532,"runAsGroup":65532},
                "containers":[{
                    "name":"custodian", "image":HELPER_IMAGE,
                    "command":["/usr/local/bin/agent-sandbox-custodian"],
                    "args":["--request","/request/request.json"],
                    "securityContext":{"runAsNonRoot":true,"runAsUser":65532,"runAsGroup":65532,"allowPrivilegeEscalation":false},
                    "volumeMounts":volume_mounts
                }],
                "volumes":volumes,
            },
            "status":{
                "phase":"Succeeded",
                "containerStatuses":[{"name":"custodian","restartCount":0,"state":{"terminated":{"exitCode":0,"finishedAt":"2026-09-09T20:00:00Z"}}}]
            }
        })),
        stdout,
    }
}

#[tokio::test]
async fn accepts_exact_completed_copy_and_preserves_original_receipt() {
    let temp = TempDir::new().unwrap();
    let request = request(&temp);
    let attestation = copy_and_seal(&request).unwrap();
    let mut stdout = serde_json::to_vec(&attestation).unwrap();
    stdout.push(b'\n');
    let seal = observe_input_seal(&observation(stdout.clone()), &expectation(), &request)
        .await
        .unwrap();
    assert_eq!(seal.attestation(), &attestation);
    assert_eq!(seal.original_receipt(), stdout);
    assert_eq!(seal.job_uid(), JOB_UID);
    assert_eq!(seal.pod_uid(), POD_UID);
    assert_eq!(
        seal.metadata(),
        &InputCompletionMetadata {
            job_uid: JOB_UID.into(),
            pod_uid: POD_UID.into(),
            pod_name: "input-copy-run-1-z9x2k".into(),
            sealed_at: OffsetDateTime::parse(&attestation.sealed_at, &Rfc3339).unwrap(),
        }
    );
}

#[tokio::test]
async fn rejects_foreign_incomplete_or_untrusted_copy_observations() {
    let temp = TempDir::new().unwrap();
    let request = request(&temp);
    let attestation = copy_and_seal(&request).unwrap();
    let mut stdout = serde_json::to_vec(&attestation).unwrap();
    stdout.push(b'\n');

    let mut foreign_job = observation(stdout.clone());
    foreign_job.job["metadata"]["uid"] = json!("other-job-uid");
    assert_eq!(
        observe_input_seal(&foreign_job, &expectation(), &request)
            .await
            .unwrap_err()
            .to_string(),
        "input_copy_job_identity_or_completion_mismatch"
    );

    let mut conflicting_terminal = observation(stdout.clone());
    conflicting_terminal.job["status"]["conditions"]
        .as_array_mut()
        .unwrap()
        .push(json!({
            "type": "Failed", "status": "True"
        }));
    assert_eq!(
        observe_input_seal(&conflicting_terminal, &expectation(), &request)
            .await
            .unwrap_err()
            .to_string(),
        "input_copy_job_completion_invalid"
    );

    let mut foreign_pod = observation(stdout.clone());
    foreign_pod.pod.as_mut().unwrap()["metadata"]["ownerReferences"][0]["uid"] =
        json!("other-job-uid");
    assert_eq!(
        observe_input_seal(&foreign_pod, &expectation(), &request)
            .await
            .unwrap_err()
            .to_string(),
        "input_copy_pod_identity_or_runtime_mismatch"
    );

    let mut restarting = observation(stdout.clone());
    restarting.pod.as_mut().unwrap()["status"]["containerStatuses"][0]["restartCount"] = json!(1);
    assert_eq!(
        observe_input_seal(&restarting, &expectation(), &request)
            .await
            .unwrap_err()
            .to_string(),
        "input_copy_helper_status_mismatch"
    );

    let oversized = observation(vec![b'x'; 1024 * 1024 + 1]);
    assert_eq!(
        observe_input_seal(&oversized, &expectation(), &request)
            .await
            .unwrap_err()
            .to_string(),
        "input_copy_receipt_invalid"
    );

    let mut extra = serde_json::to_value(&attestation).unwrap();
    extra["untrusted"] = json!("field");
    let mut extra_stdout = serde_json::to_vec(&extra).unwrap();
    extra_stdout.push(b'\n');
    assert_eq!(
        observe_input_seal(&observation(extra_stdout), &expectation(), &request)
            .await
            .unwrap_err()
            .to_string(),
        "input_copy_receipt_invalid"
    );
}
