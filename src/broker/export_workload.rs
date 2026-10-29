// Render the short-lived, authenticated output-export workload from trusted
// task, binding, and terminal-record inputs. This module only renders objects.
use super::{
    export_transport::ExportScope,
    model::ClaimedTask,
    sandbox::{SandboxBinding, SandboxHandle},
};
use anyhow::{Result, ensure};
use serde::Deserialize;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;

const EXPORTER_NAME: &str = "enrichedlab-agent-exporter";
const EXPORTER_COMPONENT: &str = "exporter";
const BROKER_NAME: &str = "enrichedlab-agent-broker";
const BROKER_COMPONENT: &str = "worker";
const UID: u32 = 65_532;
const PORT: u16 = 8_443;
const CONFIG_SCHEMA: &str = "sandbox.enrichedlab.org/export-server/v1";

/// Trusted operator configuration, never a task or model payload.
#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ExportWorkloadConfig {
    #[serde(default)]
    pub site: super::workload_site::WorkloadSiteConfig,
    pub runtime_class_name: String,
    pub image: String,
    pub broker_namespace: String,
    pub dns_suffix: String,
    pub pool_selector: BTreeMap<String, String>,
    pub cpu_request_millis: u32,
    pub cpu_limit_millis: u32,
    pub memory_request_mib: u32,
    pub memory_limit_mib: u32,
    pub active_deadline_seconds: u32,
    pub max_files: usize,
    pub max_bytes: u64,
    pub max_seconds: u64,
    pub identity_memory_mib: u32,
}

impl ExportWorkloadConfig {
    pub fn validate(&self) -> Result<()> {
        self.site.validate()?;
        ensure!(
            dns_label(&self.runtime_class_name),
            "export_runtime_invalid"
        );
        ensure!(pinned_image(&self.image), "export_image_unpinned");
        ensure!(
            dns_label(&self.broker_namespace),
            "export_broker_namespace_invalid"
        );
        ensure!(dns_name(&self.dns_suffix), "export_dns_suffix_invalid");
        ensure!(
            !self.pool_selector.is_empty(),
            "export_pool_selector_invalid"
        );
        ensure!(
            self.pool_selector
                .iter()
                .all(|(key, value)| label_key(key) && label_value(value)),
            "export_pool_selector_invalid"
        );
        ensure!(
            self.cpu_request_millis > 0
                && self.cpu_request_millis <= self.cpu_limit_millis
                && self.memory_request_mib > 0
                && self.memory_request_mib <= self.memory_limit_mib
                && self.identity_memory_mib > 0
                && self.identity_memory_mib <= self.memory_limit_mib,
            "export_resources_invalid"
        );
        ensure!(
            self.active_deadline_seconds > 0
                && self.max_seconds > 0
                && self.max_seconds <= self.active_deadline_seconds as u64
                && self.max_seconds <= 86_400,
            "export_deadline_invalid"
        );
        ensure!(
            self.max_files > 0
                && self.max_files <= 100_000
                && self.max_bytes > 0
                && self.max_bytes <= i64::MAX as u64,
            "export_limits_invalid"
        );
        Ok(())
    }
}

/// Per-run values supplied by the trusted terminal-record and TLS issuer.
#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ExportIdentity {
    pub secret_name: String,
    /// SHA-256 fingerprint of the run client certificate. It is public binding
    /// metadata, not a credential.
    pub client_sha256: String,
}

impl ExportIdentity {
    pub fn validate(&self) -> Result<()> {
        ensure!(dns_label(&self.secret_name), "export_secret_invalid");
        ensure!(
            sha256(&self.client_sha256),
            "export_client_fingerprint_invalid"
        );
        Ok(())
    }
}

#[derive(Clone, Debug)]
pub struct ExportWorkload {
    pub service_name: String,
    pub pod: Value,
    pub service: Value,
    pub network_policy: Value,
    /// Public endpoint binding for the broker-side per-run client identity.
    pub client_metadata: Value,
    /// Materialize this exact public JSON as `config.json` in the immutable
    /// per-run Secret before creating the Pod.
    pub exporter_config: Value,
}

pub struct ExportWorkloadRenderer {
    config: ExportWorkloadConfig,
}

impl ExportWorkloadRenderer {
    pub fn new(config: ExportWorkloadConfig) -> Result<Self> {
        config.validate()?;
        Ok(Self { config })
    }

    /// Renders only after the caller has observed the run's terminal record and
    /// verified the immutable Secret and output PVC custody outside this module.
    pub fn render(
        &self,
        task: &ClaimedTask,
        binding: &SandboxBinding,
        handle: &SandboxHandle,
        admitted_run_name: &str,
        identity: &ExportIdentity,
    ) -> Result<ExportWorkload> {
        identity.validate()?;
        validate_run(task, binding, handle, admitted_run_name, &self.config)?;
        let scope = ExportScope::new(task.id, task.attempt, task.lease_generation)?;
        let service_name = scope.service_name(&binding.namespace)?;
        ensure!(dns_label(admitted_run_name), "export_run_invalid");
        let mut selector = labels(&scope, admitted_run_name);
        self.config.site.extend_labels(&mut selector);
        let config = json!({
            "schemaVersion": CONFIG_SCHEMA,
            "bind": format!("0.0.0.0:{PORT}"),
            "recordPath": "/runtime/identity/terminal.json",
            "artifactsRoot": "/artifacts",
            "credentialRoot": "/runtime/identity",
            "writerUid": UID,
            "clientSha256": identity.client_sha256,
            "maxFiles": self.config.max_files,
            "maxBytes": self.config.max_bytes,
            "maxSeconds": self.config.max_seconds,
        });
        Ok(ExportWorkload {
            service_name: service_name.clone(),
            pod: pod(
                &self.config,
                &binding.namespace,
                &service_name,
                admitted_run_name,
                &selector,
                &identity.secret_name,
            ),
            service: service(&binding.namespace, &service_name, &selector),
            network_policy: network_policy(
                &binding.namespace,
                &service_name,
                &selector,
                &self.config.broker_namespace,
            ),
            client_metadata: json!({
                "dnsName": format!("{service_name}.{}.{}", binding.namespace, self.config.dns_suffix),
                "port": PORT,
                "clientSha256": identity.client_sha256,
            }),
            exporter_config: config,
        })
    }
}

fn validate_run(
    task: &ClaimedTask,
    binding: &SandboxBinding,
    handle: &SandboxHandle,
    run: &str,
    config: &ExportWorkloadConfig,
) -> Result<()> {
    ensure!(
        task.id == handle.task_id
            && task.lease_generation == handle.lease_generation
            && task.principal_id == binding.principal_id
            && task.group_key == binding.group_key
            && task.profile_key == binding.profile_key
            && handle.namespace == binding.namespace
            && handle.lab_ref == binding.lab_ref
            && handle.sandbox_name == run
            && handle.sandbox_generation > 0
            && !handle.sandbox_uid.is_empty()
            && task.attempt > 0
            && task.lease_generation > 0,
        "export_task_binding_mismatch"
    );
    ensure!(
        binding.runtime_class_name == config.runtime_class_name,
        "export_runtime_mismatch"
    );
    Ok(())
}

fn labels(scope: &ExportScope, run: &str) -> BTreeMap<String, String> {
    let mut labels = BTreeMap::new();
    labels.insert("app.kubernetes.io/name".into(), EXPORTER_NAME.into());
    labels.insert(
        "app.kubernetes.io/component".into(),
        EXPORTER_COMPONENT.into(),
    );
    labels.insert("sandbox.enrichedlab.org/run-id".into(), run.into());
    labels.insert(
        "sandbox.enrichedlab.org/export-scope".into(),
        hex::encode(Sha256::digest(scope.credential_name().as_bytes()))[..32].into(),
    );
    labels
}

fn pod(
    config: &ExportWorkloadConfig,
    namespace: &str,
    name: &str,
    run: &str,
    selector: &BTreeMap<String, String>,
    secret_name: &str,
) -> Value {
    let runtime_security = json!({
        "runAsNonRoot": true,
        "runAsUser": UID,
        "runAsGroup": UID,
        "allowPrivilegeEscalation": false,
        "readOnlyRootFilesystem": true,
        "capabilities": {"drop": ["ALL"]},
        "seccompProfile": {"type": "RuntimeDefault"}
    });
    json!({
        "apiVersion": "v1",
        "kind": "Pod",
        "metadata": {"name": name, "namespace": namespace, "labels": selector},
        "spec": {
            "runtimeClassName": config.runtime_class_name,
            "imagePullSecrets": config.site.pull_references(),
            "automountServiceAccountToken": false,
            "enableServiceLinks": false,
            "restartPolicy": "Never",
            "activeDeadlineSeconds": config.active_deadline_seconds,
            "terminationGracePeriodSeconds": 10,
            "nodeSelector": config.pool_selector,
            "securityContext": {"runAsNonRoot": true, "runAsUser": UID, "runAsGroup": UID, "fsGroup": UID, "fsGroupChangePolicy": "OnRootMismatch", "seccompProfile": {"type": "RuntimeDefault"}},
            "initContainers": [{
                "name": "materialize-identity",
                "image": config.image,
                "imagePullPolicy": "IfNotPresent",
                "command": ["/bin/sh", "-ec"],
                "args": [identity_copy_command()],
                "env": [
                    {"name": "PROJECTED_ROOT", "value": "/projected"},
                    {"name": "IDENTITY_ROOT", "value": "/runtime"}
                ],
                "securityContext": runtime_security,
                "resources": resources(config),
                "volumeMounts": [
                    {"name": "identity-projected", "mountPath": "/projected", "readOnly": true},
                    {"name": "staging-private", "mountPath": "/runtime"}
                ]
            }],
            "containers": [{
                "name": "exporter",
                "image": config.image,
                "imagePullPolicy": "IfNotPresent",
                "command": ["/usr/local/bin/agent-sandbox-exporter"],
                "args": ["--config", "/runtime/identity/config.json", "--writer-uid", "65532"],
                "ports": [{"name": "https", "containerPort": PORT, "protocol": "TCP"}],
                "securityContext": runtime_security,
                "resources": resources(config),
                "volumeMounts": [
                    {"name": "staging-private", "mountPath": "/runtime", "readOnly": true},
                    {"name": "artifacts", "mountPath": "/artifacts", "readOnly": true}
                ]
            }],
            "volumes": [
                {"name": "identity-projected", "projected": {"defaultMode": 288, "sources": [{"secret": {"name": secret_name, "items": [
                    {"key": "config.json", "path": "config.json"},
                    {"key": "terminal.json", "path": "terminal.json"},
                    {"key": "ca.pem", "path": "ca.pem"},
                    {"key": "server.pem", "path": "server.pem"},
                    {"key": "server.key", "path": "server.key"}
                ]}}]}},
                {"name": "staging-private", "emptyDir": {"medium": "Memory", "sizeLimit": format!("{}Mi", config.identity_memory_mib)}},
                {"name": "artifacts", "persistentVolumeClaim": {"claimName": format!("artifacts-{run}"), "readOnly": true}}
            ]
        }
    })
}

fn resources(config: &ExportWorkloadConfig) -> Value {
    json!({
        "requests": {"cpu": format!("{}m", config.cpu_request_millis), "memory": format!("{}Mi", config.memory_request_mib)},
        "limits": {"cpu": format!("{}m", config.cpu_limit_millis), "memory": format!("{}Mi", config.memory_limit_mib)}
    })
}

fn service(namespace: &str, name: &str, selector: &BTreeMap<String, String>) -> Value {
    json!({
        "apiVersion": "v1",
        "kind": "Service",
        "metadata": {"name": name, "namespace": namespace, "labels": selector},
        "spec": {"type": "ClusterIP", "selector": selector, "ports": [{"name": "https", "port": PORT, "targetPort": "https", "protocol": "TCP"}]}
    })
}

fn network_policy(
    namespace: &str,
    name: &str,
    selector: &BTreeMap<String, String>,
    broker_namespace: &str,
) -> Value {
    json!({
        "apiVersion": "networking.k8s.io/v1",
        "kind": "NetworkPolicy",
        "metadata": {"name": name, "namespace": namespace, "labels": selector},
        "spec": {
            "podSelector": {"matchLabels": selector},
            "policyTypes": ["Ingress", "Egress"],
            "ingress": [{
                "from": [{
                    "namespaceSelector": {"matchLabels": {"kubernetes.io/metadata.name": broker_namespace}},
                    "podSelector": {"matchLabels": {"app.kubernetes.io/name": BROKER_NAME, "app.kubernetes.io/component": BROKER_COMPONENT}}
                }],
                "ports": [{"protocol": "TCP", "port": PORT}]
            }],
            "egress": []
        }
    })
}

fn identity_copy_command() -> &'static str {
    "umask 077; install -d -m 0700 \"$IDENTITY_ROOT/identity\"; for f in config.json terminal.json ca.pem server.pem server.key; do install -m 0600 \"$PROJECTED_ROOT/$f\" \"$IDENTITY_ROOT/identity/$f\"; done"
}

fn pinned_image(image: &str) -> bool {
    image.len() <= 512
        && image.contains('@')
        && image
            .rsplit_once("@sha256:")
            .is_some_and(|(reference, digest)| !reference.is_empty() && sha256(digest))
        && image.bytes().all(|byte| {
            byte.is_ascii_lowercase()
                || byte.is_ascii_digit()
                || matches!(byte, b'.' | b'_' | b'-' | b'/' | b':' | b'@')
        })
        && !image.chars().any(char::is_whitespace)
}

fn sha256(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn dns_label(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 63
        && value.starts_with(|byte: char| byte.is_ascii_alphanumeric())
        && value.ends_with(|byte: char| byte.is_ascii_alphanumeric())
        && value
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
}

fn dns_name(value: &str) -> bool {
    value.len() <= 180 && value.split('.').all(dns_label)
}

fn label_key(value: &str) -> bool {
    let (prefix, name) = value
        .rsplit_once('/')
        .map_or((None, value), |(prefix, name)| (Some(prefix), name));
    dns_label(name)
        && prefix.is_none_or(|prefix| prefix.len() <= 253 && prefix.split('.').all(dns_label))
}

fn label_value(value: &str) -> bool {
    value.is_empty()
        || (value.len() <= 63
            && value.starts_with(|byte: char| byte.is_ascii_alphanumeric())
            && value.ends_with(|byte: char| byte.is_ascii_alphanumeric())
            && value
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.')))
}
