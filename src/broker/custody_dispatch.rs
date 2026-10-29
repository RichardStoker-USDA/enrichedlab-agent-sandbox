//! Portable, fail-closed dispatcher for private one-shot custody notices.
//!
//! The worker publishes lookup notices; this service only starts fixed site
//! custodians. A durable exclusive journal is created before a child can run.
//! An existing incomplete journal is recovery evidence, never permission to
//! reissue credentials or repeat an effect.

use super::{
    admission::open_bounded_record,
    input_request::InputPreparationRequest,
    publication_journal::{PublicationJournal, valid_private_root},
    sandbox::{SandboxBinding, SandboxBindingCatalog},
    terminal_request::TerminalPreparationRequest,
};
use anyhow::{Result, anyhow, ensure};
use fs2::FileExt;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeSet,
    ffi::CString,
    fs::{self, File, OpenOptions},
    io::Read,
    os::{
        fd::{AsRawFd, FromRawFd},
        unix::{
            ffi::OsStrExt,
            fs::{MetadataExt, OpenOptionsExt},
        },
    },
    path::{Component, Path, PathBuf},
    process::Stdio,
    time::Duration,
};
use tokio::{
    io::AsyncReadExt,
    process::{Child, Command},
    task::JoinSet,
    time::{sleep, timeout},
};
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

const CONFIG_LIMIT: u64 = 64 * 1024;
const JOURNAL_LIMIT: u64 = 16 * 1024;
const STDOUT_LIMIT: u64 = 64 * 1024;
const SCAN_LIMIT_MAX: usize = 256;
const SHUTDOWN_GRACE: Duration = Duration::from_secs(2);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Channel {
    Input,
    Output,
}
impl Channel {
    const fn label(self) -> &'static str {
        match self {
            Self::Input => "input",
            Self::Output => "output",
        }
    }
    const fn receipt_schema(self) -> Option<&'static str> {
        match self {
            Self::Input => None,
            Self::Output => Some("sandbox.enrichedlab.org/output-custodian-receipt/v1"),
        }
    }
}

#[derive(Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CustodyDispatchConfig {
    pub schema_version: String,
    pub state_root: PathBuf,
    pub writer_uid: u32,
    pub scan_limit: usize,
    pub max_concurrency: usize,
    pub poll_interval_millis: u32,
    pub input: CustodyDispatchChannelConfig,
    pub output: CustodyDispatchChannelConfig,
    /// Optional complete allowlist of execution bindings. Routes select only
    /// server-owned child configuration; programs and shared limits stay fixed.
    #[serde(default)]
    pub routes: Vec<CustodyDispatchRoute>,
}

#[derive(Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CustodyDispatchRoute {
    pub binding: SandboxBinding,
    pub input: CustodyRouteConfig,
    pub output: CustodyRouteConfig,
}

#[derive(Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CustodyRouteConfig {
    pub config: PathBuf,
    pub preparer_config: Option<PathBuf>,
}

impl CustodyRouteConfig {
    fn channel(
        &self,
        parent: &CustodyDispatchChannelConfig,
    ) -> Result<CustodyDispatchChannelConfig> {
        ensure!(
            self.preparer_config.is_some() == parent.preparer.is_some(),
            "custody_dispatch_route_preparer_mismatch"
        );
        let mut selected = parent.clone();
        selected.config = self.config.clone();
        if let (Some(preparer), Some(path)) = (&mut selected.preparer, &self.preparer_config) {
            preparer.config = path.clone();
        }
        Ok(selected)
    }
}

#[derive(Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CustodyDispatchChannelConfig {
    pub request_root: PathBuf,
    pub request_writer_uid: u32,
    pub plan_root: PathBuf,
    pub plan_writer_uid: u32,
    pub program: PathBuf,
    /// Immutable program ownership is distinct from the runtime UID that owns
    /// private notice, plan and configuration records. Root-owned packaged
    /// binaries are allowed; no arbitrary third owner is accepted.
    pub program_owner_uid: u32,
    pub config: PathBuf,
    pub config_owner_uid: u32,
    pub timeout_seconds: u32,
    /// Optional trusted catalog author. It receives only the fixed notice name,
    /// never a prompt or a caller-selected program. Custodians recheck authority.
    #[serde(default)]
    pub preparer: Option<CustodyPreparerConfig>,
}

#[derive(Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CustodyPreparerConfig {
    pub program: PathBuf,
    pub program_owner_uid: u32,
    pub config: PathBuf,
    pub config_owner_uid: u32,
    pub timeout_seconds: u32,
}
impl CustodyPreparerConfig {
    fn channel(&self, parent: &CustodyDispatchChannelConfig) -> CustodyDispatchChannelConfig {
        CustodyDispatchChannelConfig {
            program: self.program.clone(),
            program_owner_uid: self.program_owner_uid,
            config: self.config.clone(),
            config_owner_uid: self.config_owner_uid,
            timeout_seconds: self.timeout_seconds,
            preparer: None,
            ..parent.clone()
        }
    }
}

impl CustodyDispatchConfig {
    pub fn load(path: &Path, owner_uid: u32) -> Result<Self> {
        let parent = path
            .parent()
            .ok_or_else(|| anyhow!("custody_dispatch_config_path_invalid"))?;
        let name = path
            .file_name()
            .and_then(|v| v.to_str())
            .ok_or_else(|| anyhow!("custody_dispatch_config_path_invalid"))?;
        let config: Self = read_json(
            parent,
            name,
            owner_uid,
            CONFIG_LIMIT,
            "custody_dispatch_config",
        )?;
        ensure!(
            config.writer_uid == owner_uid,
            "custody_dispatch_config_owner_mismatch"
        );
        config.validate()?;
        Ok(config)
    }

    fn validate(&self) -> Result<()> {
        ensure!(
            self.schema_version == "sandbox.enrichedlab.org/custody-dispatch/v1",
            "custody_dispatch_schema_invalid"
        );
        ensure!(
            (1..=SCAN_LIMIT_MAX).contains(&self.scan_limit)
                && (1..=16).contains(&self.max_concurrency)
                && (50..=60_000).contains(&self.poll_interval_millis),
            "custody_dispatch_config_invalid"
        );
        let roots = [
            &self.state_root,
            &self.input.request_root,
            &self.input.plan_root,
            &self.output.request_root,
            &self.output.plan_root,
        ];
        for (index, root) in roots.iter().enumerate() {
            ensure!(valid_private_root(root), "custody_dispatch_root_invalid");
            validate_private_dir(root, self.writer_uid)?;
            for other in &roots[..index] {
                ensure!(
                    !root.starts_with(other) && !other.starts_with(root),
                    "custody_dispatch_roots_overlap"
                );
            }
        }
        self.input.validate(self.writer_uid)?;
        self.output.validate(self.writer_uid)?;
        ensure!(self.routes.len() <= 128, "custody_dispatch_route_limit");
        SandboxBindingCatalog::new(self.routes.iter().map(|r| r.binding.clone()).collect())
            .map_err(|_| anyhow!("custody_dispatch_route_binding_invalid"))?;
        for route in &self.routes {
            route
                .input
                .channel(&self.input)?
                .validate(self.writer_uid)?;
            route
                .output
                .channel(&self.output)?
                .validate(self.writer_uid)?;
        }
        Ok(())
    }
}

impl CustodyDispatchChannelConfig {
    fn validate(&self, owner_uid: u32) -> Result<()> {
        ensure!(
            self.request_writer_uid == owner_uid
                && self.plan_writer_uid == owner_uid
                && self.config_owner_uid == owner_uid,
            "custody_dispatch_channel_owner_invalid"
        );
        ensure!(
            self.program_owner_uid == 0 || self.program_owner_uid == owner_uid,
            "custody_dispatch_program_owner_invalid"
        );
        ensure!(
            self.program.is_absolute()
                && self.config.is_absolute()
                && (1..=3600).contains(&self.timeout_seconds),
            "custody_dispatch_channel_invalid"
        );
        validate_private_dir(&self.request_root, owner_uid)?;
        validate_private_dir(&self.plan_root, owner_uid)?;
        let parent = self
            .config
            .parent()
            .ok_or_else(|| anyhow!("custody_dispatch_child_config_invalid"))?;
        let name = self
            .config
            .file_name()
            .and_then(|v| v.to_str())
            .ok_or_else(|| anyhow!("custody_dispatch_child_config_invalid"))?;
        let _ = open_bounded_record(parent, name, owner_uid, CONFIG_LIMIT)
            .map_err(|_| anyhow!("custody_dispatch_child_config_unavailable"))?;
        validate_program(&self.program, self.program_owner_uid)?;
        if let Some(preparer) = &self.preparer {
            preparer.channel(self).validate(owner_uid)?;
        }
        Ok(())
    }
}

/// Metadata-only result. The original child receipt remains in its custodian's
/// private custody root and is represented here only by its digest.
#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct CustodyDispatchReceipt {
    pub channel: String,
    pub scope_sha256: String,
    pub outcome: String,
    pub receipt_sha256: Option<String>,
}

pub struct CustodyDispatcher {
    config: CustodyDispatchConfig,
}
impl CustodyDispatcher {
    pub fn new(config: CustodyDispatchConfig) -> Result<Self> {
        config.validate()?;
        Ok(Self { config })
    }
    pub fn config(&self) -> &CustodyDispatchConfig {
        &self.config
    }

    /// Run until cancellation. A second process cannot take the same state root.
    pub async fn run(&self, cancellation: &CancellationToken) -> Result<()> {
        let _lock = SupervisorLock::acquire(&self.config.state_root, self.config.writer_uid)?;
        loop {
            self.scan_once(cancellation).await?;
            tokio::select! { _ = cancellation.cancelled() => return Ok(()), _ = sleep(Duration::from_millis(self.config.poll_interval_millis.into())) => {} }
        }
    }

    /// Process one bounded scan while retaining the same exclusive supervisor
    /// lock that the long-running service holds.
    pub async fn run_once(
        &self,
        cancellation: &CancellationToken,
    ) -> Result<Vec<CustodyDispatchReceipt>> {
        let _lock = SupervisorLock::acquire(&self.config.state_root, self.config.writer_uid)?;
        self.scan_once(cancellation).await
    }

    /// Per-notice failure is journaled as recovery-required and does not stop
    /// other scopes. Callers reach this only through a locked supervisor entry.
    async fn scan_once(
        &self,
        cancellation: &CancellationToken,
    ) -> Result<Vec<CustodyDispatchReceipt>> {
        ensure!(!cancellation.is_cancelled(), "custody_dispatch_cancelled");
        let mut work = Vec::new();
        for (channel, config) in [
            (Channel::Input, &self.config.input),
            (Channel::Output, &self.config.output),
        ] {
            for name in list_pending_names(
                config,
                &self.config.state_root,
                channel,
                self.config.writer_uid,
                self.config.scan_limit,
            )? {
                work.push((channel, config.clone(), name));
            }
        }
        let mut set = JoinSet::new();
        let mut receipts = Vec::new();
        for (channel, config, name) in work {
            while set.len() >= self.config.max_concurrency {
                if let Some(result) = set.join_next().await {
                    receipts.push(result.map_err(|_| anyhow!("custody_dispatch_join_failed"))?);
                }
            }
            let state_root = self.config.state_root.clone();
            let uid = self.config.writer_uid;
            let routes = self.config.routes.clone();
            let cancel = cancellation.clone();
            set.spawn(async move {
                dispatch_one(&state_root, uid, channel, &config, &routes, &name, &cancel).await
            });
        }
        while let Some(result) = set.join_next().await {
            receipts.push(result.map_err(|_| anyhow!("custody_dispatch_join_failed"))?);
        }
        Ok(receipts)
    }
}

async fn dispatch_one(
    state_root: &Path,
    uid: u32,
    channel: Channel,
    config: &CustodyDispatchChannelConfig,
    routes: &[CustodyDispatchRoute],
    name: &str,
    cancellation: &CancellationToken,
) -> CustodyDispatchReceipt {
    let scope = scope_digest(channel, name);
    let base = || CustodyDispatchReceipt {
        channel: channel.label().into(),
        scope_sha256: scope.clone(),
        outcome: "recovery_required".into(),
        receipt_sha256: None,
    };
    if cancellation.is_cancelled() {
        return base();
    }
    let valid = match channel {
        Channel::Input => {
            InputPreparationRequest::load(&config.request_root, name, config.request_writer_uid)
                .map(|notice| (Scope::from_input(&notice), notice.binding().clone()))
        }
        Channel::Output => {
            TerminalPreparationRequest::load(&config.request_root, name, config.request_writer_uid)
                .map(|notice| (Scope::from_terminal(&notice), notice.binding().clone()))
        }
    };
    let (scope_identity, binding) = match valid {
        Ok(value) => value,
        Err(_) => return base(),
    };
    let plan_ready = open_bounded_record(
        &config.plan_root,
        name,
        config.plan_writer_uid,
        CONFIG_LIMIT,
    )
    .is_ok();
    if !plan_ready && config.preparer.is_none() {
        return base();
    }
    let journal_name = format!("custody-dispatch-{}-{}.jsonl", channel.label(), scope);
    let mut journal = match PublicationJournal::create(state_root, &journal_name, uid).await {
        Ok(journal) => journal,
        Err(_) => return existing_outcome(state_root, &journal_name, uid, channel, &scope),
    };
    if journal.append(json!({"phase":"started","channel":channel.label(),"scopeSha256":scope,"taskId":scope_identity.task_id,"attempt":scope_identity.attempt,"leaseGeneration":scope_identity.lease_generation})).await.is_err() { return base(); }
    let routed;
    let config = if routes.is_empty() {
        config
    } else {
        let Some(route) = routes.iter().find(|r| r.binding == binding) else {
            let _ = journal.append(json!({"phase":"recovery_required","code":"custody_dispatch_route_unavailable"})).await;
            return base();
        };
        let selection = match channel {
            Channel::Input => &route.input,
            Channel::Output => &route.output,
        };
        routed = match selection.channel(config) {
            Ok(value) => value,
            Err(_) => return base(),
        };
        &routed
    };
    if !plan_ready {
        let prepared = async {
            let author = config
                .preparer
                .as_ref()
                .ok_or_else(|| anyhow!("custody_preparer_missing"))?;
            let bytes = invoke(&author.channel(config), name, cancellation).await?;
            let digest = validate_preparation(channel, &scope_identity, config, name, &bytes)?;
            journal
                .append(json!({"phase":"plan_prepared","planSha256":digest}))
                .await?;
            Ok::<_, anyhow::Error>(())
        }
        .await;
        if let Err(error) = prepared {
            let code = super::workload_site::safe_error_code(&error);
            let _ = journal
                .append(json!({"phase":"recovery_required","code":code}))
                .await;
            return base();
        }
    }
    match invoke(config, name, cancellation)
        .await
        .and_then(|bytes| validate_receipt(channel, &scope_identity, &bytes).map(|_| bytes))
    {
        Ok(bytes) => {
            let digest = hex::encode(Sha256::digest(&bytes));
            if journal
                .append(json!({"phase":"succeeded","receiptSha256":digest}))
                .await
                .is_ok()
            {
                CustodyDispatchReceipt {
                    channel: channel.label().into(),
                    scope_sha256: scope,
                    outcome: "succeeded".into(),
                    receipt_sha256: Some(digest),
                }
            } else {
                base()
            }
        }
        Err(error) => {
            let code = super::workload_site::safe_error_code(&error);
            let _ = journal
                .append(json!({"phase":"recovery_required","code":code}))
                .await;
            base()
        }
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct PreparationReceipt {
    plan_kind: String,
    task_id: Uuid,
    attempt: i32,
    lease_generation: i64,
    plan_name: String,
    sha256: String,
    size_bytes: u64,
}
fn validate_preparation(
    channel: Channel,
    scope: &Scope,
    config: &CustodyDispatchChannelConfig,
    name: &str,
    receipt: &[u8],
) -> Result<String> {
    let r: PreparationReceipt =
        serde_json::from_slice(receipt).map_err(|_| anyhow!("custody_preparer_receipt_invalid"))?;
    ensure!(
        r.plan_kind == channel.label()
            && r.task_id == scope.task_id
            && r.attempt == scope.attempt
            && r.lease_generation == scope.lease_generation
            && r.plan_name == name,
        "custody_preparer_scope_mismatch"
    );
    let bytes = read_bytes(
        &config.plan_root,
        name,
        config.plan_writer_uid,
        CONFIG_LIMIT,
    )?;
    let digest = format!("sha256:{}", hex::encode(Sha256::digest(&bytes)));
    ensure!(
        r.size_bytes == bytes.len() as u64 && r.sha256 == digest,
        "custody_preparer_digest_mismatch"
    );
    let plan: Value =
        serde_json::from_slice(&bytes).map_err(|_| anyhow!("custody_preparer_plan_invalid"))?;
    let expected_schema = match channel {
        Channel::Input => "sandbox.enrichedlab.org/input-preparation-plan/v2",
        Channel::Output => "sandbox.enrichedlab.org/output-preparation-plan/v1",
    };
    ensure!(
        plan["schemaVersion"] == expected_schema
            && plan["task"]["taskId"] == scope.task_id.to_string()
            && plan["task"]["attempt"] == scope.attempt
            && plan["task"]["leaseGeneration"] == scope.lease_generation,
        "custody_preparer_plan_scope_mismatch"
    );
    // Full binding, data authorization and current lease remain custodian-owned.
    Ok(digest)
}

fn existing_outcome(
    root: &Path,
    name: &str,
    uid: u32,
    channel: Channel,
    scope: &str,
) -> CustodyDispatchReceipt {
    let mut receipt = CustodyDispatchReceipt {
        channel: channel.label().into(),
        scope_sha256: scope.into(),
        outcome: "recovery_required".into(),
        receipt_sha256: None,
    };
    let bytes = read_bytes(root, name, uid, JOURNAL_LIMIT).unwrap_or_default();
    if let Some(last) = bytes
        .split(|v| *v == b'\n')
        .rfind(|v| !v.is_empty())
        .and_then(|v| serde_json::from_slice::<Value>(v).ok())
        && last.get("phase").and_then(Value::as_str) == Some("succeeded")
    {
        receipt.outcome = "already_succeeded".into();
        receipt.receipt_sha256 = last
            .get("receiptSha256")
            .and_then(Value::as_str)
            .map(str::to_owned);
    }
    receipt
}

#[derive(Clone)]
struct Scope {
    task_id: Uuid,
    attempt: i32,
    lease_generation: i64,
}
impl Scope {
    fn from_input(value: &InputPreparationRequest) -> Self {
        let task = value.task();
        Self {
            task_id: task.task_id,
            attempt: task.attempt,
            lease_generation: task.lease_generation,
        }
    }
    fn from_terminal(value: &TerminalPreparationRequest) -> Self {
        let task = value.task();
        Self {
            task_id: task.task_id,
            attempt: task.attempt,
            lease_generation: task.lease_generation,
        }
    }
}

async fn invoke(
    config: &CustodyDispatchChannelConfig,
    name: &str,
    cancellation: &CancellationToken,
) -> Result<Vec<u8>> {
    validate_program(&config.program, config.program_owner_uid)?;
    let mut command = Command::new(&config.program);
    command
        .args([
            "--config",
            config
                .config
                .to_str()
                .ok_or_else(|| anyhow!("custody_dispatch_child_config_invalid"))?,
            "--config-owner-uid",
            &config.config_owner_uid.to_string(),
            "--request",
            name,
        ])
        .env_clear()
        .env("PATH", "/usr/bin:/bin")
        .current_dir("/")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    // Retain only the operator-selected trust file across the clean environment.
    if let Some(path) = std::env::var_os("SSL_CERT_FILE") {
        command.env("SSL_CERT_FILE", path);
    }
    #[cfg(unix)]
    command.process_group(0);
    let mut child = command
        .spawn()
        .map_err(|_| anyhow!("custody_dispatch_child_spawn_failed"))?;
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| anyhow!("custody_dispatch_child_pipe_failed"))?;
    let stderr = child
        .stderr
        .take()
        .ok_or_else(|| anyhow!("custody_dispatch_child_pipe_failed"))?;
    let exchange = async {
        let read = async {
            let mut bytes = Vec::new();
            stdout
                .take(STDOUT_LIMIT + 1)
                .read_to_end(&mut bytes)
                .await
                .map_err(|_| anyhow!("custody_dispatch_child_read_failed"))?;
            ensure!(
                bytes.len() as u64 <= STDOUT_LIMIT,
                "custody_dispatch_child_output_oversize"
            );
            Ok::<_, anyhow::Error>(bytes)
        };
        let wait = async {
            child
                .wait()
                .await
                .map_err(|_| anyhow!("custody_dispatch_child_wait_failed"))
        };
        let error_read = async {
            let mut bytes = Vec::new();
            stderr
                .take(129)
                .read_to_end(&mut bytes)
                .await
                .map_err(|_| anyhow!("custody_dispatch_child_read_failed"))?;
            Ok::<_, anyhow::Error>(bytes)
        };
        let (bytes, status, stderr) = tokio::try_join!(read, wait, error_read)?;
        if !status.success() {
            let code = std::str::from_utf8(&stderr).unwrap_or("").trim();
            if super::workload_site::is_safe_error_code(code) {
                return Err(anyhow!(code.to_owned()));
            }
            return Err(anyhow!("custody_dispatch_child_failed"));
        }
        Ok(bytes)
    };
    let result = tokio::select! {
        biased;
        _ = cancellation.cancelled() => Err(anyhow!("custody_dispatch_cancelled")),
        result = timeout(Duration::from_secs(config.timeout_seconds.into()), exchange) => result.map_err(|_| anyhow!("custody_dispatch_deadline"))?,
    };
    if result.is_err() {
        terminate_and_reap(&mut child).await;
    }
    result
}

async fn terminate_and_reap(child: &mut Child) {
    #[cfg(unix)]
    if let Some(pid) = child.id() {
        unsafe {
            libc::kill(-(pid as i32), libc::SIGTERM);
        }
    }
    if timeout(SHUTDOWN_GRACE, child.wait()).await.is_err() {
        let _ = child.start_kill();
        let _ = child.wait().await;
    }
}

fn validate_receipt(channel: Channel, scope: &Scope, bytes: &[u8]) -> Result<()> {
    let value: Value =
        serde_json::from_slice(bytes).map_err(|_| anyhow!("custody_dispatch_receipt_invalid"))?;
    ensure!(
        value
            .get("taskId")
            .and_then(Value::as_str)
            .is_some_and(|v| v == scope.task_id.to_string()),
        "custody_dispatch_receipt_scope_mismatch"
    );
    ensure!(
        value.get("attempt").and_then(Value::as_i64) == Some(scope.attempt as i64)
            && value.get("leaseGeneration").and_then(Value::as_i64) == Some(scope.lease_generation),
        "custody_dispatch_receipt_scope_mismatch"
    );
    match channel {
        Channel::Input => {
            for key in [
                "inputPvcUid",
                "artifactPvcUid",
                "copyJobUid",
                "copyPodUid",
                "originalSealSha256",
                "admissionSha256",
            ] {
                ensure!(
                    value
                        .get(key)
                        .and_then(Value::as_str)
                        .is_some_and(|v| !v.is_empty()),
                    "custody_dispatch_input_receipt_invalid"
                );
            }
        }
        Channel::Output => {
            ensure!(
                value.get("schemaVersion").and_then(Value::as_str) == channel.receipt_schema(),
                "custody_dispatch_output_receipt_invalid"
            );
            for key in [
                "inventoryJobUid",
                "inventoryPodUid",
                "inventorySha256",
                "terminalSha256",
            ] {
                ensure!(
                    value
                        .get(key)
                        .and_then(Value::as_str)
                        .is_some_and(|v| !v.is_empty()),
                    "custody_dispatch_output_receipt_invalid"
                );
            }
            ensure!(
                value
                    .get("exportObjects")
                    .and_then(Value::as_array)
                    .is_some(),
                "custody_dispatch_output_receipt_invalid"
            );
        }
    }
    Ok(())
}

fn list_pending_names(
    config: &CustodyDispatchChannelConfig,
    state_root: &Path,
    channel: Channel,
    uid: u32,
    limit: usize,
) -> Result<Vec<String>> {
    validate_private_dir(&config.request_root, uid)?;
    let mut names = BTreeSet::new();
    for entry in fs::read_dir(&config.request_root)
        .map_err(|_| anyhow!("custody_dispatch_notice_root_unavailable"))?
    {
        let entry = entry.map_err(|_| anyhow!("custody_dispatch_notice_root_unavailable"))?;
        let name = entry.file_name().to_string_lossy().into_owned();
        if !safe_name(&name) || !name.ends_with(".json") {
            continue;
        }
        // Any retained journal fences the scope, including failed or malformed
        // records. History must not consume active dispatch capacity.
        let journal = state_root.join(format!(
            "custody-dispatch-{}-{}.jsonl",
            channel.label(),
            scope_digest(channel, &name)
        ));
        match fs::symlink_metadata(journal) {
            Ok(_) => continue,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(_) => return Err(anyhow!("custody_dispatch_journal_unavailable")),
        }
        let valid = match channel {
            Channel::Input => InputPreparationRequest::load(
                &config.request_root,
                &name,
                config.request_writer_uid,
            )
            .is_ok(),
            Channel::Output => TerminalPreparationRequest::load(
                &config.request_root,
                &name,
                config.request_writer_uid,
            )
            .is_ok(),
        };
        if !valid
            || (config.preparer.is_none()
                && open_bounded_record(
                    &config.plan_root,
                    &name,
                    config.plan_writer_uid,
                    CONFIG_LIMIT,
                )
                .is_err())
        {
            continue;
        }
        names.insert(name);
        if names.len() > limit {
            names.pop_last();
        }
    }
    Ok(names.into_iter().collect())
}

fn scope_digest(channel: Channel, name: &str) -> String {
    hex::encode(Sha256::digest(
        format!("{}\0{name}", channel.label()).as_bytes(),
    ))
}
fn safe_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 128
        && !name.contains(['/', '\\', '\0'])
        && name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'-' | b'_'))
}

fn read_json<T: serde::de::DeserializeOwned>(
    root: &Path,
    name: &str,
    uid: u32,
    limit: u64,
    code: &str,
) -> Result<T> {
    let bytes = read_bytes(root, name, uid, limit).map_err(|_| anyhow!("{code}_unavailable"))?;
    serde_json::from_slice(&bytes).map_err(|_| anyhow!("{code}_invalid"))
}
fn read_bytes(root: &Path, name: &str, uid: u32, limit: u64) -> Result<Vec<u8>> {
    let file = open_bounded_record(root, name, uid, limit)?;
    let mut bytes = Vec::new();
    file.take(limit + 1).read_to_end(&mut bytes)?;
    ensure!(
        bytes.len() as u64 <= limit,
        "custody_dispatch_record_oversize"
    );
    Ok(bytes)
}

#[cfg(unix)]
fn validate_private_dir(root: &Path, uid: u32) -> Result<()> {
    ensure!(valid_private_root(root), "custody_dispatch_root_invalid");
    let directory = open_private_dir(root)?;
    let metadata = directory
        .metadata()
        .map_err(|_| anyhow!("custody_dispatch_root_unavailable"))?;
    ensure!(
        metadata.uid() == uid && metadata.mode() & 0o077 == 0 && unsafe { libc::geteuid() } == uid,
        "custody_dispatch_root_not_private"
    );
    Ok(())
}
#[cfg(not(unix))]
fn validate_private_dir(_: &Path, _: u32) -> Result<()> {
    Err(anyhow!("custody_dispatch_requires_unix"))
}

#[cfg(unix)]
fn open_private_dir(root: &Path) -> Result<File> {
    let mut directory = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open("/")
        .map_err(|_| anyhow!("custody_dispatch_root_unavailable"))?;
    for component in root.components() {
        let Component::Normal(component) = component else {
            if matches!(component, Component::RootDir) {
                continue;
            }
            return Err(anyhow!("custody_dispatch_root_invalid"));
        };
        let component = CString::new(component.as_bytes())
            .map_err(|_| anyhow!("custody_dispatch_root_invalid"))?;
        let fd = unsafe {
            libc::openat(
                directory.as_raw_fd(),
                component.as_ptr(),
                libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC,
            )
        };
        ensure!(fd >= 0, "custody_dispatch_root_unavailable");
        directory = unsafe { File::from_raw_fd(fd) };
    }
    Ok(directory)
}

#[cfg(unix)]
fn validate_program(path: &Path, uid: u32) -> Result<()> {
    ensure!(path.is_absolute(), "custody_dispatch_program_invalid");
    let file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC)
        .open(path)
        .map_err(|_| anyhow!("custody_dispatch_program_unavailable"))?;
    let metadata = file
        .metadata()
        .map_err(|_| anyhow!("custody_dispatch_program_unavailable"))?;
    ensure!(
        metadata.is_file()
            && metadata.uid() == uid
            && metadata.mode() & 0o022 == 0
            && metadata.mode() & 0o111 != 0
            && metadata.nlink() == 1,
        "custody_dispatch_program_untrusted"
    );
    Ok(())
}
#[cfg(not(unix))]
fn validate_program(_: &Path, _: u32) -> Result<()> {
    Err(anyhow!("custody_dispatch_requires_unix"))
}

struct SupervisorLock(File);
impl SupervisorLock {
    fn acquire(root: &Path, uid: u32) -> Result<Self> {
        validate_private_dir(root, uid)?;
        let directory = open_private_dir(root)?;
        let name = CString::new("custody-dispatch.lock").unwrap();
        let fd = unsafe {
            libc::openat(
                directory.as_raw_fd(),
                name.as_ptr(),
                libc::O_RDWR | libc::O_CREAT | libc::O_NOFOLLOW | libc::O_CLOEXEC,
                0o600,
            )
        };
        ensure!(fd >= 0, "custody_dispatch_lock_unavailable");
        let file = unsafe { File::from_raw_fd(fd) };
        let metadata = file
            .metadata()
            .map_err(|_| anyhow!("custody_dispatch_lock_unavailable"))?;
        ensure!(
            metadata.is_file()
                && metadata.uid() == uid
                && metadata.mode() & 0o077 == 0
                && metadata.nlink() == 1,
            "custody_dispatch_lock_untrusted"
        );
        file.try_lock_exclusive()
            .map_err(|_| anyhow!("custody_dispatch_already_active"))?;
        directory
            .sync_all()
            .map_err(|_| anyhow!("custody_dispatch_lock_sync_failed"))?;
        Ok(Self(file))
    }
}
impl Drop for SupervisorLock {
    fn drop(&mut self) {
        let _ = self.0.unlock();
    }
}
