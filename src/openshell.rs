use std::{
    ffi::{OsStr, OsString},
    io::Read,
    path::Path,
    process::{Command, Stdio},
    thread,
    time::{Duration, Instant},
};

use anyhow::{Context, Result, bail};
use serde_json::Value;
use uuid::Uuid;

use crate::state::SandboxRecord;

#[derive(Debug)]
pub struct BoundedOutput {
    pub success: bool,
    pub code: Option<i32>,
    pub timed_out: bool,
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
    pub stdout_truncated: bool,
    pub stderr_truncated: bool,
    pub observer_error: Option<String>,
}

#[derive(Clone, Copy, Debug)]
struct CaptureDeadline {
    classify_after: Duration,
    kill_after: Duration,
}

#[allow(clippy::too_many_arguments)]
pub fn create(
    bin: &Path,
    gateway: &str,
    name: &str,
    workspace: &str,
    route: &str,
    image: &str,
    cpu: &str,
    memory: &str,
    driver_config_json: &str,
    policy_path: &Path,
    policy_advisor_enabled: bool,
    ocsf_json_enabled: bool,
) -> Result<()> {
    let mut args = gateway_workspace_args(gateway, workspace);
    args.extend([
        os("sandbox"),
        os("create"),
        os("--name"),
        os(name),
        os("--from"),
        os(image),
        os("--cpu"),
        os(cpu),
        os("--memory"),
        os(memory),
        os("--driver-config-json"),
        os(driver_config_json),
        os("--policy"),
        policy_path.as_os_str().to_os_string(),
        os("--approval-mode"),
        os("manual"),
        os("--label"),
        os("agent-sandbox.managed=true"),
        os("--label"),
        os(format!("agent-sandbox.route={route}")),
        os("--detach"),
        os("--"),
        os("/bin/sleep"),
        os("infinity"),
    ]);
    if let Err(error) = run_capture(bin, &args) {
        return match delete_and_verify_parts(bin, gateway, workspace, name) {
            Ok(()) => Err(error).context(format!(
                "failed sandbox {name} was removed and its absence was verified"
            )),
            Err(cleanup_error) => Err(error).context(format!(
                "failed sandbox {name} may remain because verified cleanup failed: {cleanup_error:#}"
            )),
        };
    }

    let settings = [
        ("agent_policy_proposals_enabled", policy_advisor_enabled),
        ("ocsf_json_enabled", ocsf_json_enabled),
    ];
    for (key, value) in settings {
        if let Err(error) = set_setting(bin, gateway, workspace, name, key, value) {
            return match delete_and_verify_parts(bin, gateway, workspace, name) {
                Ok(()) => Err(error).context(format!(
                    "sandbox {name} was removed after setting setup failed"
                )),
                Err(cleanup_error) => Err(error).context(format!(
                    "sandbox {name} may remain after setting setup failed: {cleanup_error:#}"
                )),
            };
        }
    }
    Ok(())
}

pub fn set_setting(
    bin: &Path,
    gateway: &str,
    workspace: &str,
    name: &str,
    key: &str,
    value: bool,
) -> Result<()> {
    let mut args = gateway_workspace_args(gateway, workspace);
    args.extend([
        os("settings"),
        os("set"),
        os(name),
        os("--key"),
        os(key),
        os("--value"),
        os(value.to_string()),
    ]);
    run_capture(bin, &args).map(|_| ())
}

pub fn exec(bin: &Path, record: &SandboxRecord, command: &[OsString], tty: bool) -> Result<()> {
    run_inherit(bin, &exec_args(record, command, tty), tty)
}

pub fn exec_capture(bin: &Path, record: &SandboxRecord, command: &[OsString]) -> Result<String> {
    run_capture(bin, &exec_args(record, command, false))
}

pub fn exec_capture_bounded(
    bin: &Path,
    record: &SandboxRecord,
    command: &[OsString],
    max_stream_bytes: usize,
) -> Result<BoundedOutput> {
    run_capture_bounded(bin, &exec_args(record, command, false), max_stream_bytes)
}

pub fn exec_capture_bounded_observed<F>(
    bin: &Path,
    record: &SandboxRecord,
    command: &[OsString],
    max_stream_bytes: usize,
    observer: F,
) -> Result<BoundedOutput>
where
    F: FnMut(&[u8]) -> Result<()> + Send + 'static,
{
    run_capture_bounded_observed(
        bin,
        &exec_args(record, command, false),
        max_stream_bytes,
        observer,
    )
}

pub fn exec_capture_bounded_observed_timeout<F>(
    bin: &Path,
    record: &SandboxRecord,
    command: &[OsString],
    max_stream_bytes: usize,
    timeout_seconds: u64,
    observer: F,
) -> Result<BoundedOutput>
where
    F: FnMut(&[u8]) -> Result<()> + Send + 'static,
{
    if timeout_seconds == 0 {
        bail!("OpenShell exec timeout must be greater than zero");
    }
    let server_deadline = Duration::from_secs(timeout_seconds);
    let classify_after = server_deadline.saturating_sub(Duration::from_millis(250));
    let deadline = CaptureDeadline {
        classify_after,
        kill_after: server_deadline
            .checked_add(Duration::from_secs(5))
            .context("OpenShell exec timeout is too large")?,
    };
    run_capture_bounded_observed_deadline(
        bin,
        &exec_args_at_with_timeout(
            record,
            "/sandbox/work",
            command,
            false,
            Some(timeout_seconds),
        ),
        max_stream_bytes,
        Some(deadline),
        observer,
    )
}

pub fn exec_capture_at(
    bin: &Path,
    record: &SandboxRecord,
    workdir: &str,
    command: &[OsString],
) -> Result<String> {
    run_capture(bin, &exec_args_at(record, workdir, command, false))
}

pub(crate) fn exec_args(record: &SandboxRecord, command: &[OsString], tty: bool) -> Vec<OsString> {
    exec_args_at(record, "/sandbox/work", command, tty)
}

fn exec_args_at(
    record: &SandboxRecord,
    workdir: &str,
    command: &[OsString],
    tty: bool,
) -> Vec<OsString> {
    exec_args_at_with_timeout(record, workdir, command, tty, None)
}

fn exec_args_at_with_timeout(
    record: &SandboxRecord,
    workdir: &str,
    command: &[OsString],
    tty: bool,
    timeout_seconds: Option<u64>,
) -> Vec<OsString> {
    let mut args = record_args(record);
    args.extend([
        os("sandbox"),
        os("exec"),
        os("--name"),
        os(&record.name),
        os("--workdir"),
        os(workdir),
        os(if tty { "--tty" } else { "--no-tty" }),
    ]);
    if let Some(timeout_seconds) = timeout_seconds {
        args.extend([os("--timeout"), os(timeout_seconds.to_string())]);
    }
    args.push(os("--"));
    args.extend(command.iter().cloned());
    args
}

pub fn lifecycle(bin: &Path, record: &SandboxRecord, action: &str) -> Result<()> {
    lifecycle_parts(
        bin,
        &record.runtime.openshell_gateway,
        &record.openshell_workspace,
        &record.name,
        action,
    )
}

fn lifecycle_parts(
    bin: &Path,
    gateway: &str,
    workspace: &str,
    name: &str,
    action: &str,
) -> Result<()> {
    let mut args = gateway_workspace_args(gateway, workspace);
    args.extend([os("sandbox"), os(action), os(name)]);
    run_capture(bin, &args).map(|_| ())
}

pub fn delete_and_verify(bin: &Path, record: &SandboxRecord) -> Result<()> {
    delete_and_verify_parts(
        bin,
        &record.runtime.openshell_gateway,
        &record.openshell_workspace,
        &record.name,
    )
}

fn delete_and_verify_parts(bin: &Path, gateway: &str, workspace: &str, name: &str) -> Result<()> {
    let _ = lifecycle_parts(bin, gateway, workspace, name, "delete");
    for _ in 0..30 {
        if !sandbox_present(bin, gateway, workspace, name)? {
            return Ok(());
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
    bail!("sandbox {name} is still present in OpenShell workspace {workspace}")
}

fn sandbox_present(bin: &Path, gateway: &str, workspace: &str, name: &str) -> Result<bool> {
    let mut args = gateway_workspace_args(gateway, workspace);
    args.extend([os("sandbox"), os("list"), os("--output"), os("json")]);
    let output = run_capture(bin, &args)?;
    sandbox_list_contains(&output, name)
}

fn sandbox_list_contains(output: &str, name: &str) -> Result<bool> {
    let sandboxes: Value =
        serde_json::from_str(output).context("OpenShell returned invalid sandbox JSON")?;
    let sandboxes = sandboxes
        .as_array()
        .context("OpenShell sandbox list was not a JSON array")?;
    Ok(sandboxes.iter().any(|sandbox| {
        sandbox.get("name").and_then(Value::as_str) == Some(name)
            || sandbox.pointer("/metadata/name").and_then(Value::as_str) == Some(name)
    }))
}

pub fn apply_policy(bin: &Path, record: &SandboxRecord) -> Result<()> {
    let mut args = record_args(record);
    args.extend([
        os("policy"),
        os("set"),
        os(&record.name),
        os("--policy"),
        record.policy_path.as_os_str().to_os_string(),
        os("--wait"),
    ]);
    run_capture(bin, &args).map(|_| ())
}

pub fn add_endpoint(
    bin: &Path,
    record: &SandboxRecord,
    endpoint: &str,
    binaries: &[&str],
    rule_name: Option<&str>,
) -> Result<()> {
    let mut args = record_args(record);
    args.extend([
        os("policy"),
        os("update"),
        os(&record.name),
        os("--add-endpoint"),
        os(endpoint),
        os("--wait"),
    ]);
    if let Some(name) = rule_name {
        args.push(os("--rule-name"));
        args.push(os(name));
    }
    for binary in binaries {
        args.push(os("--binary"));
        args.push(os(binary));
    }
    run_capture(bin, &args).map(|_| ())
}

pub fn remove_endpoint(bin: &Path, record: &SandboxRecord, host: &str, port: u16) -> Result<()> {
    let endpoint = match host.parse::<std::net::IpAddr>() {
        Ok(std::net::IpAddr::V6(_)) => format!("[{host}]:{port}"),
        _ => format!("{host}:{port}"),
    };
    let mut args = record_args(record);
    args.extend([
        os("policy"),
        os("update"),
        os(&record.name),
        os("--remove-endpoint"),
        os(endpoint),
        os("--wait"),
    ]);
    run_capture(bin, &args).map(|_| ())
}

pub fn remove_rule(bin: &Path, record: &SandboxRecord, rule_name: &str) -> Result<()> {
    let mut args = record_args(record);
    args.extend([
        os("policy"),
        os("update"),
        os(&record.name),
        os("--remove-rule"),
        os(rule_name),
        os("--wait"),
    ]);
    run_capture(bin, &args).map(|_| ())
}

pub fn proposal_get(bin: &Path, record: &SandboxRecord, status: Option<&str>) -> Result<String> {
    let mut args = record_args(record);
    args.extend([os("rule"), os("get"), os(&record.name)]);
    if let Some(status) = status {
        args.push(os("--status"));
        args.push(os(status));
    }
    run_capture(bin, &args)
}

pub fn proposal_history(bin: &Path, record: &SandboxRecord) -> Result<String> {
    let mut args = record_args(record);
    args.extend([os("rule"), os("history"), os(&record.name)]);
    run_capture(bin, &args)
}

pub fn proposal_decide(
    bin: &Path,
    record: &SandboxRecord,
    chunk_id: &str,
    approve: bool,
    reason: Option<&str>,
) -> Result<String> {
    let mut args = record_args(record);
    args.extend([
        os("rule"),
        os(if approve { "approve" } else { "reject" }),
        os(&record.name),
        os("--chunk-id"),
        os(chunk_id),
    ]);
    if !approve {
        args.push(os("--reason"));
        args.push(os(reason.unwrap_or("rejected by controller")));
    }
    run_capture(bin, &args)
}

pub fn service_forward_args(
    record: &SandboxRecord,
    target_port: u16,
    bind: &str,
    local_port: u16,
) -> Vec<OsString> {
    let mut args = record_args(record);
    args.extend([
        os("forward"),
        os("service"),
        os(&record.name),
        os("--target-port"),
        os(target_port.to_string()),
        os("--target-host"),
        os("127.0.0.1"),
        os("--local"),
        os(format!("{bind}:{local_port}")),
    ]);
    args
}

pub fn upload(
    bin: &Path,
    record: &SandboxRecord,
    local_path: &Path,
    destination: &str,
    no_git_ignore: bool,
) -> Result<()> {
    let mut args = record_args(record);
    args.extend([
        os("sandbox"),
        os("upload"),
        os(&record.name),
        local_path.as_os_str().to_os_string(),
        os(destination),
    ]);
    if no_git_ignore {
        args.push(os("--no-git-ignore"));
    }
    run_capture(bin, &args).map(|_| ())
}

pub fn upload_contents(
    bin: &Path,
    record: &SandboxRecord,
    source_directory: &Path,
    destination: &str,
) -> Result<()> {
    let mut args = record_args(record);
    args.extend([
        os("sandbox"),
        os("upload"),
        os(&record.name),
        os("."),
        os(destination),
        os("--no-git-ignore"),
    ]);
    run_capture_in(bin, source_directory, &args).map(|_| ())
}

pub fn download(
    bin: &Path,
    record: &SandboxRecord,
    source: &str,
    destination: &Path,
) -> Result<()> {
    let mut args = record_args(record);
    args.extend([
        os("sandbox"),
        os("download"),
        os(&record.name),
        os(source),
        destination.as_os_str().to_os_string(),
    ]);
    run_capture(bin, &args).map(|_| ())
}

pub fn sandbox_get(bin: &Path, record: &SandboxRecord) -> Result<String> {
    let mut args = record_args(record);
    args.extend([
        os("sandbox"),
        os("get"),
        os(&record.name),
        os("--output"),
        os("json"),
    ]);
    run_capture(bin, &args)
}

pub fn sandbox_runtime_id(bin: &Path, record: &SandboxRecord) -> Result<String> {
    let body = sandbox_get(bin, record)?;
    let value: Value =
        serde_json::from_str(&body).context("OpenShell sandbox record was not JSON")?;
    let id = value
        .get("id")
        .or_else(|| value.pointer("/metadata/id"))
        .and_then(Value::as_str)
        .context("OpenShell sandbox record has no runtime ID")?;
    if id.is_empty()
        || id.len() > 128
        || !id
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit() || byte == b'-')
    {
        bail!("OpenShell sandbox runtime ID is invalid");
    }
    Ok(id.into())
}

pub fn file_exists(bin: &Path, record: &SandboxRecord, path: &str) -> Result<bool> {
    validate_sandbox_path(path)?;
    let output = exec_capture(
        bin,
        record,
        &[
            os("/bin/sh"),
            os("-c"),
            os("if [ -f \"$1\" ]; then printf present; else printf absent; fi"),
            os("agent-sandbox-file-check"),
            os(path),
        ],
    )?;
    match output.as_str() {
        "present" => Ok(true),
        "absent" => Ok(false),
        _ => bail!("sandbox file check returned an invalid result"),
    }
}

pub fn remove_files(bin: &Path, record: &SandboxRecord, paths: &[&str]) -> Result<()> {
    if paths.is_empty() || paths.len() > 16 {
        bail!("sandbox file removal needs 1 to 16 paths");
    }
    for path in paths {
        validate_sandbox_path(path)?;
    }
    let mut command = vec![os("/bin/rm"), os("-f"), os("--")];
    command.extend(paths.iter().map(os));
    exec_capture(bin, record, &command).map(|_| ())
}

pub fn active_policy(bin: &Path, record: &SandboxRecord) -> Result<String> {
    let mut args = record_args(record);
    args.extend([
        os("sandbox"),
        os("get"),
        os(&record.name),
        os("--policy-only"),
    ]);
    run_capture(bin, &args)
}

pub fn live_sandboxes(bin: &Path, gateway: &str) -> Result<Value> {
    let mut args = gateway_args(gateway);
    args.extend([
        os("sandbox"),
        os("list"),
        os("--all-workspaces"),
        os("--output"),
        os("json"),
    ]);
    let output = run_capture(bin, &args)?;
    serde_json::from_str(&output).context("OpenShell returned invalid sandbox JSON")
}

pub fn gateway_info(bin: &Path, gateway: &str) -> Result<Value> {
    let mut args = gateway_args(gateway);
    args.extend([os("gateway"), os("info"), os("--output"), os("json")]);
    let output = run_capture(bin, &args)?;
    serde_json::from_str(&output).context("OpenShell returned invalid gateway JSON")
}

pub fn gateway_status(bin: &Path, gateway: &str) -> Result<String> {
    let mut args = gateway_args(gateway);
    args.push(os("status"));
    run_capture(bin, &args)
}

pub fn record_probe(bin: &Path, record: &SandboxRecord, args: &[&str]) -> Result<String> {
    let mut values = record_args(record);
    values.extend(args.iter().map(os));
    run_capture(bin, &values)
}

pub fn probe(bin: &Path, args: &[&str]) -> Result<String> {
    let args: Vec<OsString> = args.iter().map(OsString::from).collect();
    run_capture(bin, &args)
}

pub fn run_capture(bin: &Path, args: &[OsString]) -> Result<String> {
    let output = Command::new(bin)
        .args(args)
        .stdin(Stdio::null())
        .output()
        .with_context(|| format!("failed to run {}", bin.display()))?;
    if !output.status.success() {
        bail!(
            "{} failed with {}: {}",
            bin.display(),
            output.status,
            bounded_stderr(&output.stderr)
        );
    }
    Ok(String::from_utf8_lossy(&output.stdout).to_string())
}

pub fn run_capture_in(bin: &Path, current_dir: &Path, args: &[OsString]) -> Result<String> {
    let output = Command::new(bin)
        .current_dir(current_dir)
        .args(args)
        .stdin(Stdio::null())
        .output()
        .with_context(|| format!("failed to run {}", bin.display()))?;
    if !output.status.success() {
        bail!(
            "{} failed with {}: {}",
            bin.display(),
            output.status,
            bounded_stderr(&output.stderr)
        );
    }
    Ok(String::from_utf8_lossy(&output.stdout).to_string())
}

fn run_capture_bounded(
    bin: &Path,
    args: &[OsString],
    max_stream_bytes: usize,
) -> Result<BoundedOutput> {
    run_capture_bounded_observed(bin, args, max_stream_bytes, |_| Ok(()))
}

fn run_capture_bounded_observed<F>(
    bin: &Path,
    args: &[OsString],
    max_stream_bytes: usize,
    observer: F,
) -> Result<BoundedOutput>
where
    F: FnMut(&[u8]) -> Result<()> + Send + 'static,
{
    run_capture_bounded_observed_deadline(bin, args, max_stream_bytes, None, observer)
}

fn run_capture_bounded_observed_deadline<F>(
    bin: &Path,
    args: &[OsString],
    max_stream_bytes: usize,
    deadline: Option<CaptureDeadline>,
    observer: F,
) -> Result<BoundedOutput>
where
    F: FnMut(&[u8]) -> Result<()> + Send + 'static,
{
    if max_stream_bytes == 0 {
        bail!("bounded command capture needs a nonzero stream limit");
    }
    let mut child = Command::new(bin)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .with_context(|| format!("failed to run {}", bin.display()))?;
    let stdout = child.stdout.take().context("failed to capture stdout")?;
    let stderr = child.stderr.take().context("failed to capture stderr")?;
    let stdout_reader =
        thread::spawn(move || drain_bounded_observed(stdout, max_stream_bytes, observer));
    let stderr_reader = thread::spawn(move || drain_bounded(stderr, max_stream_bytes));
    let started = Instant::now();
    let (status, killed_at_deadline) = if let Some(deadline) = deadline {
        loop {
            if let Some(status) = child
                .try_wait()
                .context("failed to poll captured command")?
            {
                break (status, false);
            }
            if started.elapsed() >= deadline.kill_after {
                if let Err(kill_error) = child.kill() {
                    if let Some(status) = child
                        .try_wait()
                        .context("failed to poll captured command after a stop race")?
                    {
                        break (status, false);
                    }
                    return Err(kill_error)
                        .context("failed to stop captured command after its deadline");
                }
                break (
                    child
                        .wait()
                        .context("failed to reap captured command after its deadline")?,
                    true,
                );
            }
            thread::sleep(Duration::from_millis(25));
        }
    } else {
        (
            child
                .wait()
                .context("failed to wait for captured command")?,
            false,
        )
    };
    let timed_out = killed_at_deadline
        || deadline.is_some_and(|deadline| {
            !status.success() && started.elapsed() >= deadline.classify_after
        });
    let (stdout, stdout_truncated, observer_error) = stdout_reader
        .join()
        .map_err(|_| anyhow::anyhow!("stdout capture thread failed"))??;
    let (stderr, stderr_truncated) = stderr_reader
        .join()
        .map_err(|_| anyhow::anyhow!("stderr capture thread failed"))??;
    Ok(BoundedOutput {
        success: status.success(),
        code: status.code(),
        timed_out,
        stdout,
        stderr,
        stdout_truncated,
        stderr_truncated,
        observer_error,
    })
}

fn drain_bounded_observed<F>(
    mut reader: impl Read,
    limit: usize,
    mut observer: F,
) -> Result<(Vec<u8>, bool, Option<String>)>
where
    F: FnMut(&[u8]) -> Result<()>,
{
    let mut captured = Vec::with_capacity(limit.min(64 * 1024));
    let mut pending = Vec::new();
    let mut buffer = [0_u8; 64 * 1024];
    let mut truncated = false;
    let mut observer_error = None;
    loop {
        let read = reader.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        let remaining = limit.saturating_sub(captured.len());
        let keep = remaining.min(read);
        captured.extend_from_slice(&buffer[..keep]);
        if keep > 0 && observer_error.is_none() && !truncated {
            pending.extend_from_slice(&buffer[..keep]);
            while let Some(newline) = pending.iter().position(|byte| *byte == b'\n') {
                let mut line = pending.drain(..=newline).collect::<Vec<_>>();
                line.pop();
                if line.last() == Some(&b'\r') {
                    line.pop();
                }
                if !line.is_empty()
                    && let Err(error) = observer(&line)
                {
                    observer_error = Some(format!("{error:#}"));
                    pending.clear();
                    break;
                }
            }
        }
        if keep < read {
            truncated = true;
            pending.clear();
        }
    }
    if !truncated
        && observer_error.is_none()
        && !pending.is_empty()
        && let Err(error) = observer(&pending)
    {
        observer_error = Some(format!("{error:#}"));
    }
    Ok((captured, truncated, observer_error))
}

fn drain_bounded(mut reader: impl Read, limit: usize) -> Result<(Vec<u8>, bool)> {
    let mut captured = Vec::with_capacity(limit.min(64 * 1024));
    let mut buffer = [0_u8; 64 * 1024];
    let mut truncated = false;
    loop {
        let read = reader.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        let remaining = limit.saturating_sub(captured.len());
        let keep = remaining.min(read);
        captured.extend_from_slice(&buffer[..keep]);
        truncated |= keep < read;
    }
    Ok((captured, truncated))
}

fn bounded_stderr(stderr: &[u8]) -> String {
    const MAX_BYTES: usize = 4096;
    let start = stderr.len().saturating_sub(MAX_BYTES);
    let text = String::from_utf8_lossy(&stderr[start..]);
    let trimmed = text.trim();
    if trimmed.is_empty() {
        "no stderr output".to_string()
    } else if start == 0 {
        trimmed.to_string()
    } else {
        format!("[earlier stderr omitted] {trimmed}")
    }
}

fn validate_sandbox_path(path: &str) -> Result<()> {
    let valid_request_control = path
        .strip_prefix("/sandbox/control/requests/.controller-runtime/")
        .is_some_and(valid_controller_marker);
    let valid_peer_material = path
        .strip_prefix("/sandbox/control/runtime/peers/")
        .is_some_and(valid_peer_material_path);
    if path.len() > 512
        || path.contains(['\0', '\n', '\r'])
        || path.split('/').any(|component| component == "..")
        || (!valid_request_control && !valid_peer_material)
    {
        bail!("sandbox control path is invalid");
    }
    Ok(())
}

fn valid_controller_marker(relative: &str) -> bool {
    !relative.is_empty() && !relative.contains('/')
}

fn valid_peer_material_path(relative: &str) -> bool {
    let parts = relative.split('/').collect::<Vec<_>>();
    if parts.len() != 3 || Uuid::parse_str(parts[0]).is_err() {
        return false;
    }
    match parts[1] {
        "source" => matches!(parts[2], "ca.crt" | "token"),
        "target" => matches!(parts[2], "server.crt" | "server.key" | "token"),
        _ => false,
    }
}

fn run_inherit(bin: &Path, args: &[OsString], tty: bool) -> Result<()> {
    let mut command = Command::new(bin);
    command.args(args);
    if tty {
        command.stdin(Stdio::inherit());
    } else {
        command.stdin(Stdio::null());
    }
    let status = command
        .status()
        .with_context(|| format!("failed to run {}", bin.display()))?;
    if !status.success() {
        bail!("{} exited with {status}", bin.display());
    }
    Ok(())
}

pub fn os(value: impl AsRef<OsStr>) -> OsString {
    value.as_ref().to_os_string()
}

fn gateway_args(gateway: &str) -> Vec<OsString> {
    vec![os("--gateway"), os(gateway)]
}

fn gateway_workspace_args(gateway: &str, workspace: &str) -> Vec<OsString> {
    vec![
        os("--gateway"),
        os(gateway),
        os("--workspace"),
        os(workspace),
    ]
}

fn record_args(record: &SandboxRecord) -> Vec<OsString> {
    gateway_workspace_args(
        &record.runtime.openshell_gateway,
        &record.openshell_workspace,
    )
}

#[cfg(test)]
mod tests {
    use std::{
        env,
        io::Cursor,
        path::Path,
        process::{Command, Stdio},
        sync::{Arc, Mutex},
        thread,
        time::{Duration, Instant},
    };

    use crate::{
        cli::{ApprovalMode, LandlockMode},
        platform::RuntimeContract,
        state::SandboxRecord,
    };

    use super::{
        CaptureDeadline, drain_bounded_observed, os, run_capture_bounded,
        run_capture_bounded_observed_deadline, run_inherit, sandbox_list_contains,
        service_forward_args, validate_sandbox_path,
    };

    #[test]
    fn bounded_capture_drains_and_truncates_both_streams() {
        let output = run_capture_bounded(
            Path::new("/bin/sh"),
            &[
                os("-c"),
                os("awk 'BEGIN { for (i=0; i<10000; i++) printf \"x\"; for (i=0; i<10000; i++) printf \"y\" > \"/dev/stderr\" }'"),
            ],
            128,
        )
        .unwrap();
        assert!(output.success);
        assert_eq!(output.stdout.len(), 128);
        assert_eq!(output.stderr.len(), 128);
        assert!(output.stdout_truncated);
        assert!(output.stderr_truncated);
    }

    #[test]
    fn bounded_capture_stops_a_process_at_its_deadline() {
        let started = Instant::now();
        let output = run_capture_bounded_observed_deadline(
            Path::new("/bin/sleep"),
            &[os("2")],
            1024,
            Some(CaptureDeadline {
                classify_after: Duration::from_millis(50),
                kill_after: Duration::from_millis(75),
            }),
            |_| Ok(()),
        )
        .unwrap();
        assert!(!output.success);
        assert!(output.timed_out);
        assert!(started.elapsed() < Duration::from_secs(1));
    }

    #[test]
    fn observed_capture_delivers_complete_lines_without_changing_output() {
        let seen = Arc::new(Mutex::new(Vec::new()));
        let callback_seen = Arc::clone(&seen);
        let input = b"{\"type\":\"step_start\"}\n{\"type\":\"tool_use\"}\nlast";
        let (captured, truncated, observer_error) =
            drain_bounded_observed(Cursor::new(input), 1024, move |line| {
                callback_seen.lock().unwrap().push(line.to_vec());
                Ok(())
            })
            .unwrap();
        assert_eq!(captured, input);
        assert!(!truncated);
        assert!(observer_error.is_none());
        assert_eq!(
            *seen.lock().unwrap(),
            vec![
                b"{\"type\":\"step_start\"}".to_vec(),
                b"{\"type\":\"tool_use\"}".to_vec(),
                b"last".to_vec()
            ]
        );
    }

    #[test]
    fn non_tty_exec_does_not_inherit_open_stdin() {
        const HELPER_ENV: &str = "AGENT_SANDBOX_STDIN_HELPER";
        if env::var_os(HELPER_ENV).is_some() {
            run_inherit(
                Path::new("/bin/sh"),
                &[
                    os("-c"),
                    os("if IFS= read -r _line; then exit 29; else exit 0; fi"),
                ],
                false,
            )
            .unwrap();
            return;
        }

        let test_binary = env::current_exe().unwrap();
        let mut child = Command::new(test_binary)
            .arg("non_tty_exec_does_not_inherit_open_stdin")
            .env(HELPER_ENV, "1")
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        let _open_stdin = child.stdin.take().unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            if let Some(status) = child.try_wait().unwrap() {
                assert!(status.success(), "stdin helper exited with {status}");
                break;
            }
            if Instant::now() >= deadline {
                child.kill().unwrap();
                let _ = child.wait();
                panic!("non-TTY OpenShell exec inherited an open stdin stream");
            }
            thread::sleep(Duration::from_millis(20));
        }
    }

    #[test]
    fn service_forward_maps_distinct_target_and_loopback_ports() {
        let record = SandboxRecord {
            schema_version: 3,
            name: "worker-one".into(),
            route: "none".into(),
            profile: "offline".into(),
            runtime: RuntimeContract::default(),
            openshell_workspace: "default".into(),
            host_workspace: "/tmp/worker-one".into(),
            managed_workspace: true,
            mounts: Vec::new(),
            image: "example.invalid/image@sha256:deadbeef".into(),
            cpu: "1".into(),
            memory: "1Gi".into(),
            read_hosts: Vec::new(),
            write_hosts: Vec::new(),
            policy_path: "/tmp/worker-one.yaml".into(),
            approval_mode: ApprovalMode::Manual,
            landlock: LandlockMode::HardRequirement,
            policy_advisor_enabled: true,
            ocsf_json_enabled: true,
            control_requests: "/tmp/requests".into(),
            control_decisions: "/tmp/decisions".into(),
            created_at_unix: 1,
        };
        let args = service_forward_args(&record, 8000, "127.0.0.1", 38080);
        let args = args
            .iter()
            .map(|value| value.to_string_lossy().into_owned())
            .collect::<Vec<_>>();
        assert!(
            args.windows(2)
                .any(|pair| pair == ["--target-port", "8000"])
        );
        assert!(
            args.windows(2)
                .any(|pair| pair == ["--target-host", "127.0.0.1"])
        );
        assert!(
            args.windows(2)
                .any(|pair| pair == ["--local", "127.0.0.1:38080"])
        );
        assert!(args.windows(2).any(|pair| pair == ["forward", "service"]));
        assert!(
            args.windows(2)
                .any(|pair| pair == ["--gateway", "openshell"])
        );
    }

    #[test]
    fn sandbox_list_matches_only_exact_names() {
        let direct = r#"[{"name":"worker-one"}]"#;
        let nested = r#"[{"metadata":{"name":"worker-two"}}]"#;
        assert!(sandbox_list_contains(direct, "worker-one").unwrap());
        assert!(!sandbox_list_contains(direct, "worker").unwrap());
        assert!(sandbox_list_contains(nested, "worker-two").unwrap());
        assert!(sandbox_list_contains("not-json", "worker").is_err());
    }

    #[test]
    fn control_path_validator_allows_only_owned_runtime_files() {
        let lease = "a09edb47-25bd-4838-8e1b-aa280d47eb6f";
        assert!(
            validate_sandbox_path(&format!(
                "/sandbox/control/runtime/peers/{lease}/source/token"
            ))
            .is_ok()
        );
        assert!(
            validate_sandbox_path(&format!(
                "/sandbox/control/runtime/peers/{lease}/target/server.key"
            ))
            .is_ok()
        );
        assert!(
            validate_sandbox_path(&format!(
                "/sandbox/control/requests/.controller-runtime/peer-{lease}.ready"
            ))
            .is_ok()
        );
        for path in [
            "/sandbox/control/runtime/peers/not-a-uuid/source/token",
            "/sandbox/control/runtime/peers/a09edb47-25bd-4838-8e1b-aa280d47eb6f/source/server.key",
            "/sandbox/control/runtime/peers/a09edb47-25bd-4838-8e1b-aa280d47eb6f/target/other",
            "/sandbox/control/runtime/peers/a09edb47-25bd-4838-8e1b-aa280d47eb6f/target/../token",
            "/sandbox/control/decisions/request.json",
        ] {
            assert!(validate_sandbox_path(path).is_err(), "accepted {path}");
        }
    }
}
