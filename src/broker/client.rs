use std::{
    env, fs,
    path::{Path, PathBuf},
    process::Stdio,
    time::Duration,
};

use anyhow::{Context, Result, bail};
use clap::{Args, Parser, Subcommand};
use futures_util::StreamExt;
use reqwest::{Client, Method, Url, redirect::Policy};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use tokio::{fs::OpenOptions as AsyncOpenOptions, io::AsyncWriteExt, process::Command};
use uuid::Uuid;

use super::model::SubmitTaskRequest;

const MAX_RESPONSE_BYTES: usize = 16 * 1024 * 1024;
const MAX_OBJECTIVE_BYTES: usize = 8_192;
const ACCESS_EDGE_TOKEN: &str = "cf-access-token";
const MUTATION_INTENT: &str = "x-agent-sandbox-intent";

#[derive(Debug, Parser)]
#[command(
    name = "agent-sandbox-remote",
    version,
    about = "Use an authenticated cluster-hosted EnrichedLab agent broker"
)]
pub struct RemoteCli {
    /// Override the configured broker HTTPS origin.
    #[arg(long, env = "AGENT_SANDBOX_REMOTE_URL")]
    server: Option<String>,

    /// Override the local non-secret remote configuration path.
    #[arg(long, env = "AGENT_SANDBOX_REMOTE_CONFIG")]
    config: Option<PathBuf>,

    /// Cloudflare tunnel client used for the interactive Access login.
    #[arg(
        long,
        env = "AGENT_SANDBOX_CLOUDFLARED_BIN",
        default_value = "cloudflared"
    )]
    cloudflared_bin: PathBuf,

    #[command(subcommand)]
    command: RemoteCommand,
}

#[derive(Debug, Subcommand)]
enum RemoteCommand {
    /// Save the broker origin. This file never contains an Access token.
    Configure {
        #[arg(long)]
        server: String,
    },
    /// Complete the Cloudflare Access login in the user's browser.
    Login,
    /// Show the verified identity, research groups, quotas, and profiles.
    Me,
    /// Submit a durable task. Disconnecting does not cancel it.
    Submit(SubmitArgs),
    /// List this identity's recent tasks.
    List {
        #[arg(long, default_value_t = 50)]
        limit: i32,
    },
    /// Get one task, including its objective and terminal status.
    Get { task: Uuid },
    /// Request cancellation without waiting for the worker to exit.
    Cancel { task: Uuid },
    /// Read append-only lifecycle events after a sequence number.
    Events {
        task: Uuid,
        #[arg(long, default_value_t = 0)]
        after: i64,
        #[arg(long, default_value_t = 200)]
        limit: i32,
    },
    /// List downloadable researcher-visible artifacts.
    Artifacts { task: Uuid },
    /// Download one artifact without overwriting an existing file.
    Download {
        task: Uuid,
        artifact: Uuid,
        #[arg(long)]
        output: PathBuf,
    },
    /// Knowledge actions you take as yourself, checked by the knowledge service against your sign-in.
    #[command(subcommand)]
    Knowledge(KnowledgeCommand),
}

#[derive(Debug, Subcommand)]
enum KnowledgeCommand {
    /// List one page of the commons records of one kind that you can see, with their baseline marks. A marked paper
    /// shows how many of its parts wait for a mark; `next` continues the listing with --after.
    Commons {
        /// dataset, paper, tool or object_collection.
        #[arg(long, default_value = "dataset")]
        kind: String,
        /// The `next` of the page before.
        #[arg(long)]
        after: Option<String>,
    },
    /// Mark a commons record baseline for every lab, or withdraw the mark. A paper's mark covers its sections,
    /// figures and tables. Only a person who leads the commons may.
    Baseline {
        resource: Uuid,
        #[arg(long)]
        withdraw: bool,
    },
    /// List the commons collections, such as literature://papers/, with what each has marked.
    Collections,
    /// Mark every record of a commons collection baseline, with each paper's parts, or withdraw the mark. Only a
    /// person who leads the commons may.
    MarkCollection {
        /// The collection's source prefix, such as literature://papers/.
        prefix: String,
        #[arg(long)]
        withdraw: bool,
    },
    /// List the links proposed from a lab's records, such as a paper that names one of the lab's tools.
    Links { lab: String },
    /// Accept a proposed link, or reject it with --reject and an optional note. Only a person who curates or leads
    /// the lab may.
    Review {
        relation: Uuid,
        #[arg(long)]
        reject: bool,
        #[arg(long)]
        note: Option<String>,
    },
}

#[derive(Debug, Args)]
struct SubmitArgs {
    #[arg(long)]
    group: String,
    #[arg(long)]
    profile: String,
    #[arg(long, conflicts_with = "objective_file")]
    objective: Option<String>,
    #[arg(long, conflicts_with = "objective")]
    objective_file: Option<PathBuf>,
    /// Stable retry key. A random UUID is used when omitted.
    #[arg(long)]
    idempotency_key: Option<String>,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct RemoteConfig {
    schema_version: u32,
    server: String,
}

pub async fn run(cli: RemoteCli) -> Result<()> {
    let config_path = cli.config.unwrap_or(default_config_path()?);
    if let RemoteCommand::Configure { server } = cli.command {
        let server = parse_origin(&server)?;
        write_config(&config_path, &server)?;
        println!("Configured {}", server.as_str().trim_end_matches('/'));
        return Ok(());
    }
    let server = match cli.server {
        Some(server) => parse_origin(&server)?,
        None => load_config(&config_path)?
            .server
            .parse::<Url>()
            .context("invalid configured broker URL")?,
    };
    if matches!(cli.command, RemoteCommand::Login) {
        return interactive_login(&cli.cloudflared_bin, &server).await;
    }
    let token = access_token(&cli.cloudflared_bin, &server).await?;
    let client = Client::builder()
        .https_only(true)
        .redirect(Policy::none())
        .connect_timeout(Duration::from_secs(10))
        .timeout(Duration::from_secs(60))
        .build()
        .context("failed to build remote broker client")?;
    match cli.command {
        RemoteCommand::Configure { .. } | RemoteCommand::Login => unreachable!(),
        RemoteCommand::Me => print_json(
            request_json(
                &client,
                &server,
                &token,
                Method::GET,
                "api/v1/me",
                None,
                None,
            )
            .await?,
        ),
        RemoteCommand::Submit(args) => {
            let objective = read_objective(args.objective, args.objective_file)?;
            validate_key(&args.group, "group")?;
            validate_key(&args.profile, "profile")?;
            let idempotency = args
                .idempotency_key
                .unwrap_or_else(|| Uuid::new_v4().to_string());
            validate_idempotency(&idempotency)?;
            let body = serde_json::to_value(SubmitTaskRequest {
                project_id: None,
                action_key: None,
                parent_task_id: None,
                group: args.group,
                profile: args.profile,
                objective,
            })?;
            print_json(
                request_json(
                    &client,
                    &server,
                    &token,
                    Method::POST,
                    "api/v1/tasks",
                    Some(body),
                    Some(&idempotency),
                )
                .await?,
            )
        }
        RemoteCommand::List { limit } => {
            if !(1..=200).contains(&limit) {
                bail!("limit must be between 1 and 200");
            }
            print_json(
                request_json(
                    &client,
                    &server,
                    &token,
                    Method::GET,
                    &format!("api/v1/tasks?limit={limit}"),
                    None,
                    None,
                )
                .await?,
            )
        }
        RemoteCommand::Get { task } => print_json(
            request_json(
                &client,
                &server,
                &token,
                Method::GET,
                &format!("api/v1/tasks/{task}"),
                None,
                None,
            )
            .await?,
        ),
        RemoteCommand::Cancel { task } => print_json(
            request_json(
                &client,
                &server,
                &token,
                Method::POST,
                &format!("api/v1/tasks/{task}/cancel"),
                Some(json!({})),
                None,
            )
            .await?,
        ),
        RemoteCommand::Events { task, after, limit } => {
            if after < 0 || !(1..=1000).contains(&limit) {
                bail!("event cursor or limit is invalid");
            }
            print_json(
                request_json(
                    &client,
                    &server,
                    &token,
                    Method::GET,
                    &format!("api/v1/tasks/{task}/events?after={after}&limit={limit}"),
                    None,
                    None,
                )
                .await?,
            )
        }
        RemoteCommand::Artifacts { task } => print_json(
            request_json(
                &client,
                &server,
                &token,
                Method::GET,
                &format!("api/v1/tasks/{task}/artifacts"),
                None,
                None,
            )
            .await?,
        ),
        RemoteCommand::Download {
            task,
            artifact,
            output,
        } => {
            download(&client, &server, &token, task, artifact, &output).await?;
            println!("Downloaded {}", output.display());
            Ok(())
        }
        RemoteCommand::Knowledge(KnowledgeCommand::Commons { kind, after }) => {
            if !["dataset", "paper", "tool", "object_collection"].contains(&kind.as_str()) {
                bail!("kind must be dataset, paper, tool or object_collection");
            }
            let mut path = format!("api/v1/knowledge/commons?kind={kind}");
            if let Some(after) = after {
                let shaped = after.split_once('.').is_some_and(|(micros, id)| {
                    (1..=19).contains(&micros.len())
                        && micros.bytes().all(|b| b.is_ascii_digit())
                        && id.parse::<Uuid>().is_ok()
                });
                if !shaped {
                    bail!("--after takes the `next` value of the page before");
                }
                path.push_str(&format!("&after={after}"));
            }
            print_json(
                request_json(&client, &server, &token, Method::GET, &path, None, None).await?,
            )
        }
        RemoteCommand::Knowledge(KnowledgeCommand::Collections) => print_json(
            request_json(
                &client,
                &server,
                &token,
                Method::GET,
                "api/v1/knowledge/commons/collections",
                None,
                None,
            )
            .await?,
        ),
        RemoteCommand::Knowledge(KnowledgeCommand::MarkCollection { prefix, withdraw }) => {
            print_json(
                request_json(
                    &client,
                    &server,
                    &token,
                    Method::POST,
                    "api/v1/knowledge/commons/collections/baseline",
                    Some(json!({"prefix": prefix, "baseline": !withdraw})),
                    None,
                )
                .await?,
            )
        }
        RemoteCommand::Knowledge(KnowledgeCommand::Links { lab }) => {
            validate_key(&lab, "lab")?;
            print_json(
                request_json(
                    &client,
                    &server,
                    &token,
                    Method::GET,
                    &format!("api/v1/knowledge/labs/{lab}/proposals"),
                    None,
                    None,
                )
                .await?,
            )
        }
        RemoteCommand::Knowledge(KnowledgeCommand::Review {
            relation,
            reject,
            note,
        }) => print_json(
            request_json(
                &client,
                &server,
                &token,
                Method::POST,
                &format!("api/v1/knowledge/relations/{relation}/review"),
                Some(json!({"accept": !reject, "note": note})),
                None,
            )
            .await?,
        ),
        RemoteCommand::Knowledge(KnowledgeCommand::Baseline { resource, withdraw }) => print_json(
            request_json(
                &client,
                &server,
                &token,
                Method::POST,
                &format!("api/v1/knowledge/resources/{resource}/baseline"),
                Some(json!({"baseline": !withdraw})),
                None,
            )
            .await?,
        ),
    }
}

async fn request_json(
    client: &Client,
    server: &Url,
    token: &str,
    method: Method,
    path: &str,
    body: Option<Value>,
    idempotency: Option<&str>,
) -> Result<Value> {
    let url = server
        .join(path)
        .context("failed to construct broker request URL")?;
    let is_mutation = method == Method::POST;
    let mut request = client
        .request(method, url)
        .header(ACCESS_EDGE_TOKEN, token)
        .header(reqwest::header::ACCEPT, "application/json");
    if is_mutation {
        request = request.header(MUTATION_INTENT, "mutate");
    }
    if let Some(idempotency) = idempotency {
        request = request.header("idempotency-key", idempotency);
    }
    if let Some(body) = body {
        request = request.json(&body);
    }
    let response = request.send().await.context("broker request failed")?;
    let status = response.status();
    if response
        .content_length()
        .is_some_and(|length| length > MAX_RESPONSE_BYTES as u64)
    {
        bail!("broker response exceeded the client limit");
    }
    let bytes = response
        .bytes()
        .await
        .context("failed to read broker response")?;
    if bytes.len() > MAX_RESPONSE_BYTES {
        bail!("broker response exceeded the client limit");
    }
    let value: Value =
        serde_json::from_slice(&bytes).context("broker returned an invalid JSON response")?;
    if !status.is_success() {
        let code = value
            .pointer("/error/code")
            .and_then(Value::as_str)
            .unwrap_or("remote_error");
        let message = value
            .pointer("/error/message")
            .and_then(Value::as_str)
            .unwrap_or("broker rejected the request");
        bail!(
            "broker returned {}: {} ({})",
            status.as_u16(),
            message,
            code
        );
    }
    Ok(value)
}

async fn download(
    client: &Client,
    server: &Url,
    token: &str,
    task: Uuid,
    artifact: Uuid,
    output: &Path,
) -> Result<()> {
    if output.as_os_str().is_empty() || output.is_dir() {
        bail!("download output must be a new file path");
    }
    if let Some(parent) = output.parent().filter(|path| !path.as_os_str().is_empty()) {
        fs::create_dir_all(parent)
            .with_context(|| format!("failed to create output directory {}", parent.display()))?;
    }
    let url = server.join(&format!(
        "api/v1/tasks/{task}/artifacts/{artifact}/download"
    ))?;
    let response = client
        .get(url)
        .header(ACCESS_EDGE_TOKEN, token)
        .send()
        .await
        .context("artifact download failed")?;
    if !response.status().is_success() {
        let status = response.status();
        let bytes = response.bytes().await.unwrap_or_default();
        let code = serde_json::from_slice::<Value>(&bytes)
            .ok()
            .and_then(|value| {
                value
                    .pointer("/error/code")
                    .and_then(Value::as_str)
                    .map(str::to_owned)
            })
            .unwrap_or_else(|| "remote_error".into());
        bail!("artifact download returned {} ({code})", status.as_u16());
    }
    let expected_hash = response
        .headers()
        .get("x-artifact-sha256")
        .and_then(|value| value.to_str().ok())
        .filter(|value| valid_sha256(value))
        .map(str::to_owned)
        .context("artifact response omitted its valid SHA-256 custody label")?;
    let expected_size = response.content_length();
    let mut file = AsyncOpenOptions::new()
        .create_new(true)
        .write(true)
        .open(output)
        .await
        .with_context(|| format!("refusing to overwrite {}", output.display()))?;
    let mut stream = response.bytes_stream();
    let mut digest = Sha256::new();
    let mut received = 0_u64;
    let transfer = async {
        while let Some(chunk) = stream.next().await {
            let chunk = chunk.context("artifact stream failed")?;
            received = received
                .checked_add(chunk.len() as u64)
                .context("artifact size overflow")?;
            digest.update(&chunk);
            file.write_all(&chunk).await?;
        }
        file.flush().await?;
        file.sync_all().await?;
        if expected_size.is_some_and(|size| size != received) {
            bail!("artifact size did not match its response metadata");
        }
        let actual_hash = hex::encode(digest.finalize());
        if actual_hash != expected_hash {
            bail!("artifact SHA-256 did not match its custody label");
        }
        Ok(())
    }
    .await;
    if let Err(error) = transfer {
        drop(file);
        let _ = tokio::fs::remove_file(output).await;
        return Err(error);
    }
    Ok(())
}

async fn interactive_login(cloudflared: &Path, server: &Url) -> Result<()> {
    let status = Command::new(cloudflared)
        .arg("access")
        .arg("login")
        .arg(server.as_str().trim_end_matches('/'))
        .stdin(Stdio::inherit())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .status()
        .await
        .with_context(|| format!("failed to start {}", cloudflared.display()))?;
    if !status.success() {
        bail!("Cloudflare Access login failed");
    }
    Ok(())
}

async fn access_token(cloudflared: &Path, server: &Url) -> Result<String> {
    let output = Command::new(cloudflared)
        .arg("access")
        .arg("token")
        .arg("--app")
        .arg(server.as_str().trim_end_matches('/'))
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .await
        .with_context(|| format!("failed to start {}", cloudflared.display()))?;
    if !output.status.success() {
        bail!("no valid Cloudflare Access session; run agent-sandbox-remote login");
    }
    let token =
        String::from_utf8(output.stdout).context("cloudflared returned a non-UTF-8 token")?;
    let token = token.trim().to_owned();
    if token.len() > 16 * 1024 || token.split('.').count() != 3 {
        bail!("cloudflared returned an invalid Access token");
    }
    Ok(token)
}

fn read_objective(value: Option<String>, path: Option<PathBuf>) -> Result<String> {
    let objective = match (value, path) {
        (Some(value), None) => value,
        (None, Some(path)) => {
            let metadata = fs::symlink_metadata(&path)
                .with_context(|| format!("failed to inspect {}", path.display()))?;
            if metadata.file_type().is_symlink()
                || !metadata.is_file()
                || metadata.len() > MAX_OBJECTIVE_BYTES as u64
            {
                bail!(
                    "objective file must be a regular non-symlink file no larger than 8192 bytes"
                );
            }
            fs::read_to_string(&path)
                .with_context(|| format!("failed to read {}", path.display()))?
        }
        _ => bail!("provide exactly one of --objective or --objective-file"),
    };
    if objective.trim().is_empty()
        || objective.len() > MAX_OBJECTIVE_BYTES
        || objective.contains('\0')
    {
        bail!("objective must contain 1-8192 bytes");
    }
    Ok(objective)
}

fn parse_origin(value: &str) -> Result<Url> {
    let url = Url::parse(value).context("invalid broker URL")?;
    if url.scheme() != "https"
        || url.host_str().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
        || url.path() != "/"
        || url.query().is_some()
        || url.fragment().is_some()
    {
        bail!("broker URL must be an HTTPS origin without credentials or a path");
    }
    Ok(url)
}

fn validate_key(value: &str, label: &str) -> Result<()> {
    if value.len() < 2
        || value.len() > 64
        || !value.as_bytes()[0].is_ascii_lowercase()
        || !value.bytes().all(|byte| {
            byte.is_ascii_lowercase() || byte.is_ascii_digit() || matches!(byte, b'-' | b'_')
        })
    {
        bail!("{label} is invalid");
    }
    Ok(())
}

fn validate_idempotency(value: &str) -> Result<()> {
    if value.is_empty()
        || value.len() > 128
        || value
            .chars()
            .any(|character| character.is_control() || character.is_whitespace())
    {
        bail!("idempotency key is invalid");
    }
    Ok(())
}

fn valid_sha256(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn default_config_path() -> Result<PathBuf> {
    if let Some(root) = env::var_os("XDG_CONFIG_HOME") {
        return Ok(PathBuf::from(root).join("enrichedlab-agent/remote.json"));
    }
    let home = env::var_os("HOME").context("HOME is not set; pass --config")?;
    Ok(PathBuf::from(home).join(".config/enrichedlab-agent/remote.json"))
}

fn write_config(path: &Path, server: &Url) -> Result<()> {
    if !path.is_absolute() {
        bail!("remote configuration path must be absolute");
    }
    if let Some(parent) = path.parent() {
        crate::config::create_private_dir(parent)?;
    }
    let body = serde_json::to_vec_pretty(&RemoteConfig {
        schema_version: 1,
        server: server.as_str().trim_end_matches('/').to_owned() + "/",
    })?;
    crate::config::write_private(path, &body)
}

fn load_config(path: &Path) -> Result<RemoteConfig> {
    let metadata = fs::symlink_metadata(path).with_context(|| {
        "remote is not configured; run agent-sandbox-remote configure --server <url>".to_string()
    })?;
    if metadata.file_type().is_symlink() || !metadata.is_file() || metadata.len() > 64 * 1024 {
        bail!("remote configuration must be a regular non-symlink file");
    }
    let config: RemoteConfig = serde_json::from_slice(&fs::read(path)?)?;
    if config.schema_version != 1 {
        bail!("unsupported remote configuration schema");
    }
    parse_origin(&config.server)?;
    Ok(config)
}

fn print_json(value: Value) -> Result<()> {
    println!("{}", serde_json::to_string_pretty(&value)?);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{
        ACCESS_EDGE_TOKEN, MUTATION_INTENT, parse_origin, valid_sha256, validate_idempotency,
    };

    #[test]
    fn client_token_targets_the_access_edge_header() {
        assert_eq!(ACCESS_EDGE_TOKEN, "cf-access-token");
        assert_ne!(ACCESS_EDGE_TOKEN, "cf-access-jwt-assertion");
    }

    #[test]
    fn client_uses_the_broker_mutation_intent_header() {
        assert_eq!(MUTATION_INTENT, "x-agent-sandbox-intent");
    }

    #[test]
    fn remote_origin_is_https_and_pathless() {
        assert!(parse_origin("https://agents.example.invalid").is_ok());
        assert!(parse_origin("http://agents.example.invalid").is_err());
        assert!(parse_origin("https://agents.example.invalid/api").is_err());
    }

    #[test]
    fn idempotency_key_rejects_header_injection() {
        assert!(validate_idempotency("task-123").is_ok());
        assert!(validate_idempotency("task\r\nheader").is_err());
    }

    #[test]
    fn artifact_hash_label_is_canonical_lowercase_hex() {
        assert!(valid_sha256(&"a".repeat(64)));
        assert!(!valid_sha256(&"A".repeat(64)));
        assert!(!valid_sha256("abc"));
    }
}
