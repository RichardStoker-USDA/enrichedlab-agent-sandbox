use std::{
    collections::BTreeMap,
    fs,
    path::{Component, Path, PathBuf},
    process::{Command, Stdio},
};

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use serde_json::json;
use sha2::{Digest, Sha256};
use time::{OffsetDateTime, format_description::well_known::Rfc3339};

use crate::{
    audit,
    config::{RuntimeConfig, create_private_dir, write_private},
};

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct TeamRecord {
    pub schema_version: u32,
    pub name: String,
    pub source_repo: PathBuf,
    pub integration_repo: PathBuf,
    pub base_commit: String,
    pub root: PathBuf,
    pub shared_output: Option<PathBuf>,
    pub created_at: String,
    pub workers: BTreeMap<String, WorkerRecord>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct WorkerRecord {
    pub name: String,
    pub sandbox: String,
    pub workspace: PathBuf,
    pub repo: PathBuf,
    pub output: PathBuf,
    pub branch: String,
    pub owns: Vec<String>,
    pub task: String,
    pub collected_commit: Option<String>,
    pub merged_commit: Option<String>,
    pub created_at: String,
}

pub fn create(
    config: &RuntimeConfig,
    name: &str,
    repo: &Path,
    shared_output: bool,
) -> Result<TeamRecord> {
    validate_team_name(name)?;
    if !repo.is_absolute() {
        bail!("team source repository path must be absolute");
    }
    let source_repo = fs::canonicalize(repo)?;
    require_git_repo(&source_repo)?;
    if !git(&source_repo, &["status", "--porcelain"])?
        .trim()
        .is_empty()
    {
        bail!("team source repository must be clean before freezing the base commit");
    }
    let base_commit = git(&source_repo, &["rev-parse", "HEAD"])?
        .trim()
        .to_string();
    validate_commit(&base_commit)?;
    let root = config.root.join("projects").join(name);
    if root.exists() {
        bail!("team root already exists: {}", root.display());
    }
    create_private_dir(&root)?;
    let integration_repo = root.join("integration");
    run_git_clone(&source_repo, &integration_repo)?;
    let _ = git(&integration_repo, &["remote", "remove", "origin"]);
    let shared_output = if shared_output {
        let path = root.join("shared-output");
        create_private_dir(&path)?;
        Some(path)
    } else {
        None
    };
    let record = TeamRecord {
        schema_version: 1,
        name: name.into(),
        source_repo,
        integration_repo,
        base_commit,
        root,
        shared_output,
        created_at: OffsetDateTime::now_utc().format(&Rfc3339)?,
        workers: BTreeMap::new(),
    };
    save(config, &record)?;
    audit::append(
        config,
        "host-controller",
        "team.create",
        &format!("team/{name}"),
        "passed",
        json!({"base_commit": record.base_commit, "shared_output": record.shared_output.is_some()}),
    )?;
    Ok(record)
}

pub fn prepare_worker(
    config: &RuntimeConfig,
    team_name: &str,
    worker_name: &str,
    owns: &[String],
    task: &str,
) -> Result<WorkerRecord> {
    validate_worker_name(worker_name)?;
    validate_task(task)?;
    let mut team = load(config, team_name)?;
    if team.workers.contains_key(worker_name) {
        bail!("worker {worker_name} already exists in team {team_name}");
    }
    let owns = normalize_ownership(owns)?;
    for existing in team.workers.values() {
        for left in &owns {
            for right in &existing.owns {
                if prefixes_overlap(left, right) {
                    bail!(
                        "ownership prefix {left} overlaps worker {} prefix {right}",
                        existing.name
                    );
                }
            }
        }
    }
    let workspace = team.root.join("workers").join(worker_name);
    create_private_dir(&workspace)?;
    let repo = workspace.join("repo");
    let output = workspace.join("output");
    create_private_dir(&output)?;
    run_git_clone(&team.integration_repo, &repo)?;
    let _ = git(&repo, &["remote", "remove", "origin"]);
    let branch = format!("agent/{team_name}/{worker_name}");
    git(&repo, &["checkout", "-b", &branch, &team.base_commit])?;
    let sandbox = sandbox_name(team_name, worker_name);
    let worker = WorkerRecord {
        name: worker_name.into(),
        sandbox,
        workspace: workspace.clone(),
        repo,
        output,
        branch,
        owns,
        task: task.into(),
        collected_commit: None,
        merged_commit: None,
        created_at: OffsetDateTime::now_utc().format(&Rfc3339)?,
    };
    let assignment = json!({
        "schema_version": 1,
        "team": team_name,
        "worker": worker.name,
        "sandbox": worker.sandbox,
        "task": worker.task,
        "owned_paths": worker.owns,
        "repository": "/sandbox/work/repo",
        "output": "/sandbox/work/output",
        "shared_output": team.shared_output.as_ref().map(|_| "/sandbox/mounts/shared-output"),
        "rules": [
            "Commit only the assigned branch.",
            "Do not edit paths owned by another worker.",
            "Do not merge or push. The host lead collects and integrates commits."
        ]
    });
    let mut body = serde_json::to_vec_pretty(&assignment)?;
    body.push(b'\n');
    write_private(&workspace.join("assignment.json"), &body)?;
    team.workers.insert(worker_name.into(), worker.clone());
    save(config, &team)?;
    audit::append(
        config,
        "host-controller",
        "team.worker.prepare",
        &format!("team/{team_name}/worker/{worker_name}"),
        "passed",
        json!({"sandbox": worker.sandbox, "owns": worker.owns, "branch": worker.branch}),
    )?;
    Ok(worker)
}

pub fn attach_sandbox(
    config: &RuntimeConfig,
    team_name: &str,
    worker_name: &str,
    sandbox: &str,
) -> Result<TeamRecord> {
    let mut team = load(config, team_name)?;
    let worker = team
        .workers
        .get_mut(worker_name)
        .context("unknown team worker")?;
    if worker.sandbox != sandbox {
        bail!("worker sandbox identity mismatch");
    }
    save(config, &team)?;
    Ok(team)
}

pub fn collect(config: &RuntimeConfig, team_name: &str, worker_name: &str) -> Result<TeamRecord> {
    let mut team = load(config, team_name)?;
    let worker = team
        .workers
        .get(worker_name)
        .context("unknown team worker")?
        .clone();
    if !git_bytes(&worker.repo, &["status", "--porcelain=v1", "-z"])?.is_empty() {
        bail!("worker repository must be clean before collection");
    }
    let commit = git(&worker.repo, &["rev-parse", &worker.branch])?
        .trim()
        .to_string();
    validate_commit(&commit)?;
    validate_worker_diff(&team, &worker, &commit)?;
    let review_branch = format!("review/{worker_name}");
    git(
        &team.integration_repo,
        &[
            "fetch",
            worker
                .repo
                .to_str()
                .context("worker repo path is not UTF-8")?,
            &format!("{}:refs/heads/{review_branch}", worker.branch),
        ],
    )?;
    team.workers
        .get_mut(worker_name)
        .expect("worker exists")
        .collected_commit = Some(commit.clone());
    save(config, &team)?;
    audit::append(
        config,
        "host-controller",
        "team.worker.collect",
        &format!("team/{team_name}/worker/{worker_name}"),
        "passed",
        json!({"commit": commit, "review_branch": review_branch}),
    )?;
    Ok(team)
}

pub fn rollback_worker(
    config: &RuntimeConfig,
    team_name: &str,
    worker_name: &str,
    reason: &str,
) -> Result<PathBuf> {
    validate_task(reason)?;
    let mut team = load(config, team_name)?;
    let worker = team
        .workers
        .get(worker_name)
        .with_context(|| format!("unknown team worker {worker_name}"))?
        .clone();
    if worker.collected_commit.is_some() || worker.merged_commit.is_some() {
        bail!("refusing to roll back a collected or merged worker");
    }
    let recovery_root = team.root.join("recovery");
    create_private_dir(&recovery_root)?;
    let suffix = OffsetDateTime::now_utc().unix_timestamp();
    let recovered = recovery_root.join(format!("{worker_name}-{suffix}"));
    if recovered.exists() {
        bail!(
            "worker recovery path already exists: {}",
            recovered.display()
        );
    }
    fs::rename(&worker.workspace, &recovered)?;
    team.workers.remove(worker_name);
    if let Err(error) = save(config, &team) {
        let _ = fs::rename(&recovered, &worker.workspace);
        return Err(error).context("worker record rollback failed; workspace move was reversed");
    }
    audit::append(
        config,
        "host-controller",
        "team.worker.rollback",
        &format!("team/{team_name}/worker/{worker_name}"),
        "passed",
        json!({"reason": reason, "recovery_path": recovered}),
    )?;
    Ok(recovered)
}

pub fn merge(config: &RuntimeConfig, team_name: &str, worker_name: &str) -> Result<TeamRecord> {
    let mut team = load(config, team_name)?;
    let settings = config.load_settings()?;
    let worker = team
        .workers
        .get(worker_name)
        .context("unknown team worker")?
        .clone();
    let commit = worker
        .collected_commit
        .clone()
        .context("collect the worker before merging")?;
    validate_worker_diff(&team, &worker, &commit)?;
    if !git(&team.integration_repo, &["status", "--porcelain"])?
        .trim()
        .is_empty()
    {
        bail!("integration repository must be clean before merge");
    }
    let review_branch = format!("review/{worker_name}");
    let merge = git_with_identity(
        &team.integration_repo,
        &["merge", "--no-ff", "--no-edit", &review_branch],
        &settings.git_committer_name,
        &settings.git_committer_email,
    );
    if let Err(error) = merge {
        let _ = git(&team.integration_repo, &["merge", "--abort"]);
        return Err(error).context("worker merge failed and was aborted");
    }
    let merged = git(&team.integration_repo, &["rev-parse", "HEAD"])?
        .trim()
        .to_string();
    team.workers
        .get_mut(worker_name)
        .expect("worker exists")
        .merged_commit = Some(merged.clone());
    save(config, &team)?;
    audit::append(
        config,
        "host-lead",
        "team.worker.merge",
        &format!("team/{team_name}/worker/{worker_name}"),
        "passed",
        json!({"worker_commit": commit, "integration_commit": merged}),
    )?;
    Ok(team)
}

pub fn load(config: &RuntimeConfig, name: &str) -> Result<TeamRecord> {
    validate_team_name(name)?;
    let path = record_path(config, name);
    let body = fs::read(&path).with_context(|| format!("unknown team {name}"))?;
    let record: TeamRecord = serde_json::from_slice(&body)?;
    validate_record(&record)?;
    Ok(record)
}

pub fn list(config: &RuntimeConfig) -> Result<Vec<TeamRecord>> {
    let mut teams = Vec::new();
    for entry in fs::read_dir(config.teams_dir())? {
        let path = entry?.path();
        if path.extension().and_then(|value| value.to_str()) == Some("json") {
            let team: TeamRecord = serde_json::from_slice(&fs::read(path)?)?;
            validate_record(&team)?;
            teams.push(team);
        }
    }
    teams.sort_by(|left, right| left.name.cmp(&right.name));
    Ok(teams)
}

fn validate_worker_diff(team: &TeamRecord, worker: &WorkerRecord, commit: &str) -> Result<()> {
    git(
        &worker.repo,
        &["merge-base", "--is-ancestor", &team.base_commit, commit],
    )
    .context("worker commit does not descend from the frozen team base")?;
    let range = format!("{}..{commit}", team.base_commit);
    let changed = git_bytes(
        &worker.repo,
        &[
            "diff",
            "--name-only",
            "--no-renames",
            "--no-ext-diff",
            "--no-textconv",
            "-z",
            &range,
        ],
    )?;
    for path in changed
        .split(|byte| *byte == 0)
        .filter(|path| !path.is_empty())
    {
        let path = std::str::from_utf8(path).context("worker changed a non-UTF-8 path")?;
        if path.contains(['\n', '\r']) {
            bail!("worker changed a path containing control characters");
        }
        if !worker
            .owns
            .iter()
            .any(|prefix| path == prefix.trim_end_matches('/') || path.starts_with(prefix))
        {
            bail!("worker {} changed unowned path {path}", worker.name);
        }
    }
    Ok(())
}

fn save(config: &RuntimeConfig, team: &TeamRecord) -> Result<()> {
    validate_record(team)?;
    let mut body = serde_json::to_vec_pretty(team)?;
    body.push(b'\n');
    write_private(&record_path(config, &team.name), &body)
}

fn validate_record(team: &TeamRecord) -> Result<()> {
    if team.schema_version != 1 {
        bail!("unsupported team record schema");
    }
    validate_team_name(&team.name)?;
    validate_commit(&team.base_commit)?;
    if !team.source_repo.is_absolute()
        || !team.integration_repo.is_absolute()
        || !team.root.is_absolute()
    {
        bail!("team paths must be absolute");
    }
    Ok(())
}

fn validate_team_name(name: &str) -> Result<()> {
    if name.is_empty()
        || name.len() > 32
        || name.starts_with('-')
        || name.ends_with('-')
        || !name
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
    {
        bail!("team name must be a lowercase DNS label of at most 32 characters");
    }
    Ok(())
}

fn validate_worker_name(name: &str) -> Result<()> {
    validate_team_name(name)
}

fn validate_task(task: &str) -> Result<()> {
    if task.trim().is_empty() || task.len() > 16 * 1024 {
        bail!("worker task must be between 1 byte and 16 KiB");
    }
    Ok(())
}

fn normalize_ownership(values: &[String]) -> Result<Vec<String>> {
    if values.is_empty() {
        bail!("at least one owned path is required");
    }
    let mut result = Vec::new();
    for value in values {
        let path = Path::new(value);
        if path.is_absolute()
            || value.is_empty()
            || value.contains('*')
            || path.components().any(|component| {
                matches!(
                    component,
                    Component::ParentDir | Component::RootDir | Component::Prefix(_)
                )
            })
        {
            bail!("owned paths must be literal relative path prefixes: {value}");
        }
        let normalized = value
            .trim_start_matches("./")
            .trim_end_matches('/')
            .to_string();
        if normalized.is_empty() {
            bail!("repository-root ownership is not allowed; declare bounded paths");
        }
        let prefix = format!("{normalized}/");
        if !result.contains(&prefix) {
            result.push(prefix);
        }
    }
    result.sort();
    Ok(result)
}

fn prefixes_overlap(left: &str, right: &str) -> bool {
    left.starts_with(right) || right.starts_with(left)
}

fn sandbox_name(team: &str, worker: &str) -> String {
    let digest = hex::encode(Sha256::digest(format!("{team}/{worker}")));
    let team = name_prefix(team);
    let worker = name_prefix(worker);
    let name = format!("t-{team}-{worker}-{}", &digest[..4]);
    name.chars().take(19).collect()
}

fn name_prefix(value: &str) -> String {
    value
        .chars()
        .filter(char::is_ascii_alphanumeric)
        .take(5)
        .collect()
}

fn require_git_repo(path: &Path) -> Result<()> {
    if git(path, &["rev-parse", "--is-inside-work-tree"])?.trim() != "true" {
        bail!("not a Git worktree: {}", path.display());
    }
    Ok(())
}

fn run_git_clone(source: &Path, destination: &Path) -> Result<()> {
    let mut command = hardened_git_command();
    let output = command
        .args(["clone", "--local", "--no-hardlinks", "--no-tags"])
        .arg(source)
        .arg(destination)
        .stdin(Stdio::null())
        .output()?;
    if !output.status.success() {
        bail!(
            "git clone failed with {}: {}",
            output.status,
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    Ok(())
}

fn git(repo: &Path, args: &[&str]) -> Result<String> {
    let output = git_bytes(repo, args)?;
    String::from_utf8(output).context("git returned non-UTF-8 output")
}

fn git_bytes(repo: &Path, args: &[&str]) -> Result<Vec<u8>> {
    let mut command = hardened_git_command();
    let output = command
        .arg("-C")
        .arg(repo)
        .args(args)
        .stdin(Stdio::null())
        .output()?;
    if !output.status.success() {
        bail!(
            "git {} failed with {}: {}",
            args.first().copied().unwrap_or("command"),
            output.status,
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    Ok(output.stdout)
}

fn git_with_identity(repo: &Path, args: &[&str], name: &str, email: &str) -> Result<String> {
    let name_config = format!("user.name={name}");
    let email_config = format!("user.email={email}");
    let mut command = hardened_git_command();
    let output = command
        .arg("-C")
        .arg(repo)
        .args(["-c", &name_config, "-c", &email_config])
        .args(args)
        .stdin(Stdio::null())
        .output()?;
    if !output.status.success() {
        bail!(
            "git {} failed with {}: {}",
            args.first().copied().unwrap_or("command"),
            output.status,
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    String::from_utf8(output.stdout).context("git returned non-UTF-8 output")
}

fn hardened_git_command() -> Command {
    let mut command = Command::new("git");
    command
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GIT_OPTIONAL_LOCKS", "0")
        .env_remove("GIT_ASKPASS")
        .env_remove("SSH_ASKPASS")
        .env_remove("GIT_AUTHOR_NAME")
        .env_remove("GIT_AUTHOR_EMAIL")
        .env_remove("GIT_COMMITTER_NAME")
        .env_remove("GIT_COMMITTER_EMAIL")
        .args([
            "--no-pager",
            "--no-replace-objects",
            "-c",
            "core.hooksPath=/dev/null",
            "-c",
            "core.fsmonitor=false",
            "-c",
            "credential.helper=",
            "-c",
            "protocol.file.allow=always",
        ]);
    command
}

fn validate_commit(value: &str) -> Result<()> {
    if value.len() != 40
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
    {
        bail!("invalid Git commit ID");
    }
    Ok(())
}

fn record_path(config: &RuntimeConfig, name: &str) -> PathBuf {
    config.teams_dir().join(format!("{name}.json"))
}

#[cfg(test)]
mod tests {
    use std::{fs, path::Path, process::Command};

    use tempfile::tempdir;

    use super::{
        collect, create, merge, normalize_ownership, prefixes_overlap, prepare_worker, sandbox_name,
    };
    use crate::{
        config::{RuntimeConfig, create_private_dir},
        state::validate_name,
    };

    #[test]
    fn ownership_is_bounded_and_overlap_is_detected() {
        assert_eq!(
            normalize_ownership(&["src/api".into()]).unwrap(),
            vec!["src/api/"]
        );
        assert!(normalize_ownership(&["../secret".into()]).is_err());
        assert!(prefixes_overlap("src/", "src/api/"));
        assert!(!prefixes_overlap("src/api/", "src/ui/"));
    }

    #[test]
    fn team_sandbox_names_fit_openshell_limit() {
        let name = sandbox_name("research-project", "analysis-worker");
        assert!(name.len() <= 19);
        validate_name(&name).unwrap();

        let name = sandbox_name("research-team", "beta-worker");
        assert!(!name.contains("--"));
        validate_name(&name).unwrap();
    }

    #[test]
    fn host_collects_owned_changes_and_rejects_unowned_deletions() {
        let temp = tempdir().unwrap();
        let config = test_config(temp.path());
        let source = temp.path().join("source");
        fs::create_dir_all(source.join("src/api")).unwrap();
        fs::create_dir_all(source.join("src/ui")).unwrap();
        fs::write(source.join("src/api/mod.rs"), "pub const API: u8 = 1;\n").unwrap();
        fs::write(source.join("src/ui/mod.rs"), "pub const UI: u8 = 1;\n").unwrap();
        run_git(&source, &["init", "-q"]);
        run_git(&source, &["add", "."]);
        run_git(&source, &["commit", "-q", "-m", "Initial source"]);

        create(&config, "study", &source, false).unwrap();
        let good = prepare_worker(
            &config,
            "study",
            "api",
            &["src/api".into()],
            "Change the API",
        )
        .unwrap();
        fs::write(good.repo.join("src/api/mod.rs"), "pub const API: u8 = 2;\n").unwrap();
        run_git(&good.repo, &["add", "."]);
        run_git(&good.repo, &["commit", "-q", "-m", "Change API"]);
        collect(&config, "study", "api").unwrap();
        let merged = merge(&config, "study", "api").unwrap();
        assert!(merged.workers["api"].merged_commit.is_some());
        assert_eq!(
            fs::read_to_string(merged.integration_repo.join("src/api/mod.rs")).unwrap(),
            "pub const API: u8 = 2;\n"
        );

        let bad = prepare_worker(&config, "study", "docs", &["docs".into()], "Write docs").unwrap();
        fs::remove_file(bad.repo.join("src/ui/mod.rs")).unwrap();
        fs::create_dir_all(bad.repo.join("docs")).unwrap();
        fs::write(bad.repo.join("docs/readme.md"), "notes\n").unwrap();
        run_git(&bad.repo, &["add", "-A"]);
        run_git(&bad.repo, &["commit", "-q", "-m", "Delete unowned UI"]);
        assert!(collect(&config, "study", "docs").is_err());
    }

    fn test_config(root: &Path) -> RuntimeConfig {
        let config = RuntimeConfig {
            root: root.join("sandboxes"),
            state_dir: root.join("state"),
            config_dir: root.join("config"),
            openshell_bin: "openshell".into(),
            docker_bin: "docker".into(),
        };
        for directory in [config.root.clone(), config.teams_dir(), config.audit_dir()] {
            create_private_dir(&directory).unwrap();
        }
        config.save_settings(&Default::default()).unwrap();
        config
    }

    fn run_git(repo: &Path, args: &[&str]) {
        let status = Command::new("git")
            .arg("-C")
            .arg(repo)
            .args([
                "-c",
                "user.name=Test",
                "-c",
                "user.email=test@example.invalid",
            ])
            .args(args)
            .status()
            .unwrap();
        assert!(status.success(), "git command failed: {args:?}");
    }
}
