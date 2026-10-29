use std::{
    env, fs,
    net::SocketAddr,
    path::{Path, PathBuf},
    str::FromStr,
    time::Duration,
};

use anyhow::{Context, Result, bail};
use clap::{Args, Parser, Subcommand, ValueEnum};
use reqwest::Url;
use tokio_postgres::Config as PostgresConfig;

use super::auth::ServiceAccountAuthConfig;

#[derive(Debug, Parser)]
#[command(
    name = "agent-sandbox-broker",
    version,
    about = "Authenticated task broker for cluster-hosted EnrichedLab agents"
)]
pub struct BrokerCli {
    #[command(subcommand)]
    pub command: BrokerCommand,
}

#[derive(Debug, Subcommand)]
pub enum BrokerCommand {
    /// Serve the authenticated researcher API.
    Serve(ServeArgs),
    /// Claim durable tasks and run disposable OpenShell agents.
    Worker(WorkerArgs),
    /// Advance scoped research plans through bounded model decisions and tool tasks.
    Coordinate(super::coordinator::CoordinatorArgs),
    /// Direct durable research campaigns above their individual projects and runs.
    CampaignDirector(super::campaign::CampaignDirectorArgs),
    /// Apply broker database migrations with the migration identity.
    Migrate(MigrationArgs),
    /// Serve the gRPC evidence append service for producers outside the broker.
    EvidenceAppend(super::evidence_append::EvidenceAppendArgs),
    /// Check that the local evidence append service is serving (an exec probe).
    EvidenceProbe(super::evidence_append::EvidenceProbeArgs),
    /// Store random bytes through the evidence append service and bind them in one record (a custody canary).
    EvidenceCustodyCanary(super::evidence_canary::EvidenceCanaryArgs),
    /// Reconcile a declarative tenant, group, profile, and invitation catalog.
    Seed(SeedArgs),
    /// Build result digests for retained packages named on standard input (read only).
    DigestPackages(DigestPackagesArgs),
    /// Apply the fixed untrusted-text screen to texts on standard input, for measurement (read only).
    ScreenTexts,
    /// Run the query guard on queries on standard input, each with its source, as a proposal's check does (read only).
    CheckQueries,
    /// Register an immutable reviewed query as the migration owner.
    ReviewedQuery(super::reviewed_query::registration::Args),
    /// Run the coordinator's lead reply checks on replies on standard input, each with its kept context (read only).
    CheckLeadReplies,
    /// Measure a query source's schema card from its snapshot as the source's reader (read only).
    SourceCard(SourceCardArgs),
    /// Show what a lead call would read of a schema card for a question, within a byte room (read only, no database).
    SourceCardView,
    /// Load the result kind registry built into this binary and print its digest; an image build runs it so a registry
    /// the broker would refuse at start fails the build (read only, no database).
    CheckKinds,
    /// Write the answer document a package's `request.json` describes, read on standard input, to standard output, as
    /// the worker wrote it (read only, no database).
    WriteDocument,
}

impl BrokerCommand {
    /// The read-only commands that answer on standard output with one JSON line per input line.
    pub fn writes_json_lines(&self) -> bool {
        matches!(
            self,
            Self::DigestPackages(_)
                | Self::ScreenTexts
                | Self::CheckQueries
                | Self::CheckLeadReplies
                | Self::SourceCard(_)
                | Self::SourceCardView
                | Self::CheckKinds
        )
    }
}

/// The operator's schema card measurement: standard input holds one JSON object, `source` as
/// `broker_register_sql_source` took it and the operator's `spec`; standard output gets the card as one JSON line. It
/// connects only with the reader DSN in `connection_file` and reads inside one read-only transaction.
#[derive(Clone, Debug, Args)]
pub struct SourceCardArgs {
    #[arg(long)]
    pub connection_file: PathBuf,
}

/// The operator's digest backfill: one JSON object per input line with
/// `task_id`, `relative_path`, `size_bytes` and `sha256`; one JSON line out per package. It reads
/// only under the artifact root and opens no database or network connection. The root defaults to
/// the one the API and worker are configured with, not the volume's mount path.
#[derive(Clone, Debug, Args)]
pub struct DigestPackagesArgs {
    #[arg(long, env = "AGENT_SANDBOX_BROKER_ARTIFACT_ROOT")]
    pub artifact_root: PathBuf,
}

#[derive(Clone, Debug, Args)]
pub struct ServeArgs {
    /// Serve the packaged research UI at this API origin. No artifact directory is served.
    #[arg(
        long,
        env = "AGENT_SANDBOX_BROKER_WORKSPACE_UI",
        default_value_t = false
    )]
    pub workspace_ui: bool,
    /// Serve each reader's change stream, `GET /api/v1/changes/stream`. When off, `/api/v1/me`
    /// says `change_stream: false` and the workspace reads on its timers as before.
    #[arg(
        long,
        env = "AGENT_SANDBOX_BROKER_CHANGE_STREAM",
        default_value_t = true,
        action = clap::ArgAction::Set
    )]
    pub change_stream: bool,
    /// Offer and accept several starting points in one project. Turn it on only once the research
    /// coordinator reads `starting_points` (broker migration 061 and later): an older coordinator refuses a saved
    /// version that names them. When off, the starting-point lists say `combine: false` and `workspace_keys` is refused.
    #[arg(
        long,
        env = "AGENT_SANDBOX_BROKER_COMBINE_STARTING_POINTS",
        default_value_t = false,
        action = clap::ArgAction::Set
    )]
    pub combine_starting_points: bool,
    /// Optional private site-owned catalog for starting a research project.
    #[arg(long, env = "AGENT_SANDBOX_BROKER_WORKSPACE_CATALOG")]
    pub workspace_catalog: Option<PathBuf>,
    /// Optional organization the research UI names beneath the reader's lab, such as a research center.
    #[arg(long, env = "AGENT_SANDBOX_BROKER_SITE_ORGANIZATION")]
    pub site_organization: Option<String>,
    /// Optional private lab library configuration: the document service and each lab's corpus and
    /// token file. Without it every library route answers library_not_configured.
    #[arg(long, env = "AGENT_SANDBOX_LIBRARY_CONFIG_FILE")]
    pub library_config: Option<PathBuf>,
    /// Optional knowledge service configuration for the knowledge actions a signed-in person takes through the
    /// workspace. Without it those routes answer knowledge_not_configured.
    #[arg(long, env = "AGENT_SANDBOX_BROKER_KNOWLEDGE_PERSON_CONFIG")]
    pub knowledge_person_config: Option<PathBuf>,
    /// Optional upload-guard caller configuration for lab datasets (use case `lab-dataset`). Without it
    /// the dataset upload route answers dataset_guard_unconfigured.
    #[arg(long, env = "AGENT_SANDBOX_BROKER_DATASET_GUARD_CONFIG")]
    pub dataset_guard_config: Option<PathBuf>,
    /// Listener used inside the trusted service network.
    #[arg(
        long,
        env = "AGENT_SANDBOX_BROKER_BIND",
        default_value = "0.0.0.0:32110"
    )]
    pub bind: SocketAddr,

    /// Authentication selection. Cloudflare permits the existing optional ServiceAccount fallback.
    #[arg(
        long,
        env = "AGENT_SANDBOX_BROKER_AUTH_MODE",
        value_enum,
        default_value_t = ServeAuthMode::Cloudflare
    )]
    pub auth_mode: ServeAuthMode,

    /// Optional public HTTPS origin for task Location responses. Cloudflare mode requires it.
    #[arg(long, env = "AGENT_SANDBOX_BROKER_URL")]
    pub public_url: Option<String>,

    /// Exact Cloudflare Access token issuer. Required only in Cloudflare mode.
    #[arg(long, env = "AGENT_SANDBOX_ACCESS_ISSUER")]
    pub access_issuer: Option<String>,

    /// Exact Access application audience tag. Required only in Cloudflare mode.
    #[arg(long, env = "AGENT_SANDBOX_ACCESS_AUDIENCE")]
    pub access_audience: Option<String>,

    /// Optional site-controlled HTTPS JWKS relay. Defaults to the Access issuer endpoint.
    #[arg(long, env = "AGENT_SANDBOX_ACCESS_JWKS_URL")]
    pub access_jwks_url: Option<String>,

    /// Optional PEM CA bundle for a site-controlled HTTPS JWKS relay.
    #[arg(long, env = "AGENT_SANDBOX_ACCESS_JWKS_CA_FILE")]
    pub access_jwks_ca_file: Option<PathBuf>,

    /// Optional private JSON configuration for exact Kubernetes ServiceAccount submitters.
    /// When absent, only Cloudflare Access identities can authenticate to Serve.
    #[arg(long, env = "AGENT_SANDBOX_BROKER_SERVICE_ACCOUNT_AUTH_FILE")]
    pub service_account_auth_file: Option<PathBuf>,

    /// Database DSN environment variable name. The value is read only at startup.
    #[arg(
        long,
        env = "AGENT_SANDBOX_BROKER_DATABASE_DSN_ENV",
        default_value = "AGENT_SANDBOX_BROKER_DATABASE_DSN"
    )]
    pub database_dsn_env: String,

    /// Preferred mounted Secret path containing the database DSN.
    #[arg(long, env = "AGENT_SANDBOX_BROKER_DATABASE_DSN_FILE")]
    pub database_dsn_file: Option<PathBuf>,

    /// Site-owned tenant selected by this broker instance.
    #[arg(long, env = "AGENT_SANDBOX_BROKER_TENANT")]
    pub tenant: String,

    /// Durable task artifact root. This must be a private persistent volume.
    #[arg(long, env = "AGENT_SANDBOX_BROKER_ARTIFACT_ROOT")]
    pub artifact_root: PathBuf,

    #[arg(long, env = "AGENT_SANDBOX_BROKER_DATABASE_POOL", default_value_t = 16)]
    pub database_pool: usize,
}

#[derive(Clone, Debug, Args)]
pub struct WorkerArgs {
    /// Database DSN environment variable name. Use a worker-only database role.
    #[arg(
        long,
        env = "AGENT_SANDBOX_BROKER_DATABASE_DSN_ENV",
        default_value = "AGENT_SANDBOX_BROKER_DATABASE_DSN"
    )]
    pub database_dsn_env: String,

    /// Preferred mounted Secret path containing the worker database DSN.
    #[arg(long, env = "AGENT_SANDBOX_BROKER_DATABASE_DSN_FILE")]
    pub database_dsn_file: Option<PathBuf>,

    #[arg(long, env = "AGENT_SANDBOX_BROKER_TENANT")]
    pub tenant: String,

    #[arg(long, env = "AGENT_SANDBOX_BROKER_ARTIFACT_ROOT")]
    pub artifact_root: PathBuf,

    #[arg(long, env = "AGENT_SANDBOX_BROKER_CONTROLLER_ROOT")]
    pub controller_root: PathBuf,

    #[arg(long, env = "AGENT_SANDBOX_BROKER_CONTROLLER_STATE")]
    pub controller_state: PathBuf,

    #[arg(long, env = "AGENT_SANDBOX_BROKER_CONTROLLER_CONFIG")]
    pub controller_config: PathBuf,

    #[arg(
        long,
        env = "AGENT_SANDBOX_CONTROLLER_BIN",
        default_value = "agent-sandbox"
    )]
    pub controller_bin: PathBuf,

    #[arg(long, env = "AGENT_SANDBOX_OPENSHELL_BIN", default_value = "openshell")]
    pub openshell_bin: PathBuf,

    #[arg(long, env = "AGENT_SANDBOX_DOCKER_BIN", default_value = "docker")]
    pub docker_bin: PathBuf,

    #[arg(long, env = "AGENT_SANDBOX_BROKER_WORKER_ID")]
    pub worker_id: Option<String>,

    /// Optional private direct-Sandbox dispatch catalog owned by the server.
    #[arg(long, env = "AGENT_SANDBOX_BROKER_DIRECT_DISPATCH_CATALOG")]
    pub direct_dispatch_catalog: Option<PathBuf>,

    /// Expected UID of the deployment-owned direct dispatch catalog writer.
    #[arg(long, env = "AGENT_SANDBOX_BROKER_DIRECT_DISPATCH_WRITER_UID")]
    pub direct_dispatch_writer_uid: Option<u32>,

    /// Optional folder of query source connections: one `<connection>.dsn` file per source's reader.
    #[arg(long, env = "AGENT_SANDBOX_BROKER_QUERY_CONNECTION_DIR")]
    pub query_connection_dir: Option<PathBuf>,

    /// Optional base URL of the reviewed R render service, for figure versions. Only the worker reaches it.
    #[arg(long, env = "AGENT_SANDBOX_BROKER_RENDER_URL")]
    pub render_url: Option<String>,

    /// Optional upload-guard fetcher configuration: the worker reads the rebuilt copies of the lab datasets
    /// the API sent. Without it a dataset's intake ends `dataset_guard_unconfigured`.
    #[arg(long, env = "AGENT_SANDBOX_BROKER_DATASET_FETCHER_CONFIG")]
    pub dataset_fetcher_config: Option<PathBuf>,

    /// Follow each direct Sandbox runner's output and record it for watching. Needs `get` on
    /// `pods/log` in each lab namespace; off until the containment review accepts it.
    #[arg(
        long,
        env = "AGENT_SANDBOX_BROKER_LIVE_OUTPUT",
        default_value_t = false
    )]
    pub live_output: bool,

    /// Simultaneous agent processes owned by this one controller worker.
    #[arg(long, env = "AGENT_SANDBOX_BROKER_MAX_PARALLEL", default_value_t = 1)]
    pub max_parallel: usize,

    #[arg(
        long,
        env = "AGENT_SANDBOX_BROKER_LEASE_SECONDS",
        default_value_t = 120
    )]
    pub lease_seconds: u64,

    #[arg(long, env = "AGENT_SANDBOX_BROKER_POLL_MILLIS", default_value_t = 750)]
    pub poll_millis: u64,

    #[arg(long, env = "AGENT_SANDBOX_BROKER_DATABASE_POOL", default_value_t = 8)]
    pub database_pool: usize,

    /// Maximum regular workspace files copied into researcher artifacts per task.
    #[arg(
        long,
        env = "AGENT_SANDBOX_BROKER_MAX_WORKSPACE_FILES",
        default_value_t = 20_000
    )]
    pub max_workspace_files: u64,

    /// Maximum workspace bytes copied into researcher artifacts per task.
    #[arg(
        long,
        env = "AGENT_SANDBOX_BROKER_MAX_WORKSPACE_BYTES",
        default_value_t = 21_474_836_480_u64
    )]
    pub max_workspace_bytes: u64,
}

#[derive(Clone, Debug, Args)]
pub struct MigrationArgs {
    #[arg(
        long,
        env = "AGENT_SANDBOX_BROKER_MIGRATION_DSN_ENV",
        default_value = "AGENT_SANDBOX_BROKER_MIGRATION_DSN"
    )]
    pub migration_dsn_env: String,

    #[arg(long, env = "AGENT_SANDBOX_BROKER_MIGRATION_DSN_FILE")]
    pub migration_dsn_file: Option<PathBuf>,
}

#[derive(Clone, Debug, Args)]
pub struct SeedArgs {
    #[arg(long)]
    pub file: PathBuf,

    #[arg(
        long,
        env = "AGENT_SANDBOX_BROKER_MIGRATION_DSN_ENV",
        default_value = "AGENT_SANDBOX_BROKER_MIGRATION_DSN"
    )]
    pub migration_dsn_env: String,

    #[arg(long, env = "AGENT_SANDBOX_BROKER_MIGRATION_DSN_FILE")]
    pub migration_dsn_file: Option<PathBuf>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, ValueEnum)]
pub enum ServeAuthMode {
    Cloudflare,
    ServiceAccountOnly,
}

#[derive(Clone, Debug)]
pub enum ApiAuthentication {
    Cloudflare {
        issuer: String,
        audience: String,
        jwks_url: Url,
        jwks_ca_pem: Option<Vec<u8>>,
        service_account_auth: Option<ServiceAccountAuthConfig>,
    },
    ServiceAccountOnly {
        service_account_auth: ServiceAccountAuthConfig,
    },
}

#[derive(Clone, Debug)]
pub struct ApiConfig {
    pub workspace_ui: bool,
    pub change_stream: bool,
    pub combine_starting_points: bool,
    pub workspace_catalog: Option<PathBuf>,
    pub library_config: Option<PathBuf>,
    pub knowledge_person_config: Option<PathBuf>,
    pub dataset_guard_config: Option<PathBuf>,
    pub site_organization: Option<String>,
    pub bind: SocketAddr,
    pub public_url: Option<Url>,
    pub authentication: ApiAuthentication,
    pub database_dsn: String,
    pub tenant: String,
    pub artifact_root: PathBuf,
    pub database_pool: usize,
}

#[derive(Clone, Debug)]
pub struct DirectDispatchConfig {
    pub catalog: PathBuf,
    pub writer_uid: u32,
}

#[derive(Clone, Debug)]
pub struct WorkerConfig {
    pub database_dsn: String,
    pub tenant: String,
    pub artifact_root: PathBuf,
    pub controller_root: PathBuf,
    pub controller_state: PathBuf,
    pub controller_config: PathBuf,
    pub controller_bin: PathBuf,
    pub openshell_bin: PathBuf,
    pub docker_bin: PathBuf,
    pub worker_id: String,
    pub direct_dispatch: Option<DirectDispatchConfig>,
    pub query_connection_dir: Option<PathBuf>,
    pub render_url: Option<String>,
    pub dataset_fetcher_config: Option<PathBuf>,
    pub live_output: bool,
    pub max_parallel: usize,
    pub lease_seconds: u64,
    pub poll_interval: Duration,
    pub database_pool: usize,
    pub max_workspace_files: u64,
    pub max_workspace_bytes: u64,
}

impl ServeArgs {
    pub fn resolve(self) -> Result<ApiConfig> {
        let public_url = self
            .public_url
            .as_deref()
            .map(|value| parse_https_url(value, "broker public URL"))
            .transpose()?;
        if public_url.as_ref().is_some_and(|url| {
            url.path() != "/" || url.query().is_some() || url.fragment().is_some()
        }) {
            bail!("broker public URL must be an HTTPS origin without a path, query, or fragment");
        }
        validate_bounded_id(&self.database_dsn_env, "database DSN environment name", 128)?;
        validate_tenant(&self.tenant)?;
        validate_pool(self.database_pool)?;
        validate_existing_root(&self.artifact_root, "artifact root")?;
        let database_dsn = read_secret(&self.database_dsn_file, &self.database_dsn_env)?;
        validate_database_dsn(&database_dsn)?;
        let service_account_auth = self
            .service_account_auth_file
            .as_deref()
            .map(ServiceAccountAuthConfig::load)
            .transpose()
            .map_err(|_| {
                anyhow::anyhow!("service-account authentication configuration is invalid")
            })?;
        let authentication = match self.auth_mode {
            ServeAuthMode::Cloudflare => {
                if public_url.is_none() {
                    bail!("broker public URL is required in Cloudflare mode");
                }
                let issuer = self
                    .access_issuer
                    .ok_or_else(|| {
                        anyhow::anyhow!("Cloudflare Access issuer is required in Cloudflare mode")
                    })?
                    .trim_end_matches('/')
                    .to_owned();
                let issuer_url = parse_https_url(&issuer, "Cloudflare Access issuer")?;
                let host = issuer_url.host_str().unwrap_or_default();
                if !host.ends_with(".cloudflareaccess.com") {
                    bail!("Cloudflare Access issuer must use a cloudflareaccess.com team domain");
                }
                if issuer_url.path() != "/"
                    || issuer_url.query().is_some()
                    || issuer_url.fragment().is_some()
                {
                    bail!("Cloudflare Access issuer must be an HTTPS origin");
                }
                let audience = self.access_audience.ok_or_else(|| {
                    anyhow::anyhow!("Access audience is required in Cloudflare mode")
                })?;
                validate_bounded_id(&audience, "Access audience", 256)?;
                let jwks_url = match self.access_jwks_url {
                    Some(value) => parse_https_url(&value, "Access JWKS URL")?,
                    None => issuer_url
                        .join("/cdn-cgi/access/certs")
                        .context("failed to derive the Access JWKS URL")?,
                };
                if jwks_url.query().is_some() || jwks_url.fragment().is_some() {
                    bail!("Access JWKS URL cannot contain a query or fragment");
                }
                let jwks_ca_pem = self
                    .access_jwks_ca_file
                    .as_deref()
                    .map(read_ca_bundle)
                    .transpose()?;
                ApiAuthentication::Cloudflare {
                    issuer,
                    audience,
                    jwks_url,
                    jwks_ca_pem,
                    service_account_auth,
                }
            }
            ServeAuthMode::ServiceAccountOnly => {
                if self.access_issuer.is_some()
                    || self.access_audience.is_some()
                    || self.access_jwks_url.is_some()
                    || self.access_jwks_ca_file.is_some()
                {
                    bail!("Cloudflare Access settings are not allowed in ServiceAccount-only mode");
                }
                ApiAuthentication::ServiceAccountOnly {
                    service_account_auth: service_account_auth.ok_or_else(|| {
                        anyhow::anyhow!(
                            "service-account authentication configuration is required in ServiceAccount-only mode"
                        )
                    })?,
                }
            }
        };
        let site_organization = match self.site_organization {
            Some(value) => Some(site_label(&value)?),
            None => None,
        };
        Ok(ApiConfig {
            workspace_ui: self.workspace_ui,
            change_stream: self.change_stream,
            combine_starting_points: self.combine_starting_points,
            workspace_catalog: self.workspace_catalog,
            library_config: self.library_config,
            knowledge_person_config: self.knowledge_person_config,
            dataset_guard_config: self.dataset_guard_config,
            site_organization,
            bind: self.bind,
            public_url,
            authentication,
            database_dsn,
            tenant: self.tenant,
            artifact_root: self.artifact_root,
            database_pool: self.database_pool,
        })
    }
}

impl WorkerArgs {
    pub fn resolve(self) -> Result<WorkerConfig> {
        validate_bounded_id(&self.database_dsn_env, "database DSN environment name", 128)?;
        validate_tenant(&self.tenant)?;
        validate_pool(self.database_pool)?;
        if !(1..=16).contains(&self.max_parallel) {
            bail!("max parallel workers must be between 1 and 16");
        }
        if !(60..=900).contains(&self.lease_seconds) {
            bail!("task lease seconds must be between 60 and 900");
        }
        if !(100..=10_000).contains(&self.poll_millis) {
            bail!("poll interval must be between 100 and 10000 milliseconds");
        }
        if !(1..=1_000_000).contains(&self.max_workspace_files)
            || !(1..=1_099_511_627_776).contains(&self.max_workspace_bytes)
        {
            bail!("workspace artifact limits are invalid");
        }
        for (path, label) in [
            (&self.artifact_root, "artifact root"),
            (&self.controller_root, "controller root"),
            (&self.controller_state, "controller state"),
            (&self.controller_config, "controller config"),
        ] {
            prepare_private_root(path, label)?;
        }
        for (path, label) in [
            (&self.controller_bin, "controller binary"),
            (&self.openshell_bin, "OpenShell binary"),
            (&self.docker_bin, "Docker binary"),
        ] {
            if path.as_os_str().is_empty() {
                bail!("{label} cannot be empty");
            }
        }
        let worker_id = self
            .worker_id
            .or_else(|| env::var("HOSTNAME").ok())
            .unwrap_or_else(|| format!("worker-{}", uuid::Uuid::new_v4().simple()));
        validate_bounded_id(&worker_id, "worker ID", 128)?;
        let direct_dispatch = match (
            self.direct_dispatch_catalog,
            self.direct_dispatch_writer_uid,
        ) {
            (None, None) => None,
            (Some(catalog), Some(writer_uid)) => {
                if !catalog.is_absolute() || catalog == Path::new("/") {
                    bail!("direct dispatch catalog path must be absolute")
                }
                Some(DirectDispatchConfig {
                    catalog,
                    writer_uid,
                })
            }
            _ => bail!("direct dispatch catalog and writer UID must be configured together"),
        };
        if direct_dispatch.is_some() && self.max_workspace_files > 100_000 {
            bail!("direct Sandbox export file limit must not exceed 100000");
        }
        if let Some(dir) = &self.query_connection_dir {
            validate_existing_root(dir, "query connection folder")?;
        }
        let render_url = self
            .render_url
            .as_deref()
            .map(validate_render_url)
            .transpose()?;
        let database_dsn = read_secret(&self.database_dsn_file, &self.database_dsn_env)?;
        validate_database_dsn(&database_dsn)?;
        Ok(WorkerConfig {
            database_dsn,
            tenant: self.tenant,
            artifact_root: self.artifact_root,
            controller_root: self.controller_root,
            controller_state: self.controller_state,
            controller_config: self.controller_config,
            controller_bin: self.controller_bin,
            openshell_bin: self.openshell_bin,
            docker_bin: self.docker_bin,
            worker_id,
            direct_dispatch,
            query_connection_dir: self.query_connection_dir,
            render_url,
            dataset_fetcher_config: self.dataset_fetcher_config,
            live_output: self.live_output,
            max_parallel: self.max_parallel,
            lease_seconds: self.lease_seconds,
            poll_interval: Duration::from_millis(self.poll_millis),
            database_pool: self.database_pool,
            max_workspace_files: self.max_workspace_files,
            max_workspace_bytes: self.max_workspace_bytes,
        })
    }
}

/// The render service's base URL: http or https with a host and nothing else but an optional port and path, which the
/// worker joins to `/render` and `/features`. The service runs inside the cluster behind its network policy.
fn validate_render_url(value: &str) -> Result<String> {
    let url = reqwest::Url::parse(value).context("render service URL is not a URL")?;
    if !matches!(url.scheme(), "http" | "https")
        || url.host_str().is_none_or(str::is_empty)
        || !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
    {
        bail!(
            "render service URL must be http or https with a host and no credentials, query or fragment"
        );
    }
    Ok(url.as_str().trim_end_matches('/').to_owned())
}

/// A source reader's connection: its connection string and, for a database whose certificate another authority signs
/// (a lab's own cell), that authority's certificate, which the connection then trusts alone.
pub struct ReaderConnection {
    pub dsn: String,
    pub ca: Option<Vec<u8>>,
}

/// The most bytes a reader's certificate authority may hold.
const READER_CA_LIMIT: u64 = 64 * 1024;

/// The certificate authority beside a reader's connection file: `<name>.ca.crt` next to `<name>.dsn`, absent when the
/// database's certificate is one the process already trusts. It must be a regular file of at most 64 KiB holding
/// PEM certificates only. The entry itself decides absence: a Secret volume links each key into place, and a link
/// whose target is missing is a broken authority, never an absent one, so the connection never falls back to the
/// process's trust. The file is opened once, checked through that handle and read with a bound, so a rotation
/// between the check and the read cannot slip in an oversized bundle.
fn reader_ca(dsn_file: &Path) -> Result<Option<Vec<u8>>> {
    let Some(stem) = dsn_file
        .file_name()
        .and_then(|n| n.to_str())
        .and_then(|n| n.strip_suffix(".dsn"))
    else {
        return Ok(None);
    };
    let path = dsn_file.with_file_name(format!("{stem}.ca.crt"));
    match fs::symlink_metadata(&path) {
        Ok(_) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => {
            return Err(error).context("failed to inspect the reader's certificate authority");
        }
    }
    let file =
        fs::File::open(&path).context("the reader's certificate authority could not be opened")?;
    let metadata = file
        .metadata()
        .context("failed to inspect the reader's certificate authority")?;
    if !metadata.is_file() {
        bail!("the reader's certificate authority must be a regular file no larger than 64 KiB");
    }
    let mut pem = Vec::new();
    std::io::Read::read_to_end(
        &mut std::io::Read::take(file, READER_CA_LIMIT + 1),
        &mut pem,
    )
    .context("failed to read the reader's certificate authority")?;
    if pem.is_empty() || pem.len() as u64 > READER_CA_LIMIT {
        bail!("the reader's certificate authority must be a regular file no larger than 64 KiB");
    }
    pem_certificate_blocks(&pem)?;
    Ok(Some(pem))
}

/// The certificates of a PEM bundle that holds certificates and nothing else: each block is the certificate header,
/// base64 lines and the certificate footer, and only whitespace lies between and around the blocks. Any other block
/// (a key, a revocation list, a public key) or any other text refuses the whole bundle. The connector adds one block
/// at a time, so the loader and the connector read a bundle the same way.
pub(crate) fn pem_certificate_blocks(pem: &[u8]) -> Result<Vec<Vec<u8>>> {
    const BEGIN: &str = "-----BEGIN CERTIFICATE-----";
    const END: &str = "-----END CERTIFICATE-----";
    let text = std::str::from_utf8(pem).context("the certificate authority is not PEM text")?;
    let mut blocks = Vec::new();
    let mut rest = text.trim_start();
    while !rest.is_empty() {
        let Some(after) = rest.strip_prefix(BEGIN) else {
            bail!("the certificate authority must hold PEM certificates only");
        };
        let end = after
            .find(END)
            .context("the certificate authority holds an unterminated certificate")?;
        let body = &after[..end];
        if body.trim().is_empty()
            || !body.bytes().all(|b| {
                b.is_ascii_alphanumeric() || matches!(b, b'+' | b'/' | b'=' | b'\r' | b'\n')
            })
        {
            bail!("the certificate authority must hold PEM certificates only");
        }
        let stop = BEGIN.len() + end + END.len();
        blocks.push(rest.as_bytes()[..stop].to_vec());
        rest = rest[stop..].trim_start();
    }
    if blocks.is_empty() {
        bail!("the certificate authority holds no certificate");
    }
    Ok(blocks)
}

impl WorkerConfig {
    /// A query source's reader connection, read when the query runs. The name is the source's `connection_ref`; the
    /// file must be a regular file of the configured folder, and the DSN passes the same checks as the broker's own.
    /// A `<name>.ca.crt` beside it names the one authority the connection trusts.
    pub fn query_connection(&self, name: &str) -> Result<ReaderConnection> {
        let dir = self
            .query_connection_dir
            .as_ref()
            .context("no query connection folder is configured")?;
        if name.len() < 3
            || name.len() > 64
            || !name.starts_with(|c: char| c.is_ascii_lowercase())
            || !name
                .bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
        {
            bail!("query connection name is invalid");
        }
        reader_connection(&dir.join(format!("{name}.dsn")))
    }
}

/// A source reader's connection from its private file, the string checked like every broker DSN, with the certificate
/// authority beside it when there is one.
pub fn reader_connection(file: &Path) -> Result<ReaderConnection> {
    let dsn = read_secret(&Some(file.to_path_buf()), "UNUSED_READER_DSN")?;
    validate_database_dsn(&dsn)?;
    Ok(ReaderConnection {
        dsn,
        ca: reader_ca(file)?,
    })
}

pub fn migration_dsn(file: &Option<PathBuf>, environment_name: &str) -> Result<String> {
    validate_bounded_id(environment_name, "migration DSN environment name", 128)?;
    let dsn = read_secret(file, environment_name)?;
    validate_database_dsn(&dsn)?;
    Ok(dsn)
}

fn parse_https_url(value: &str, label: &str) -> Result<Url> {
    let url = Url::parse(value).with_context(|| format!("invalid {label}"))?;
    if url.scheme() != "https"
        || url.host_str().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
    {
        bail!("{label} must be an HTTPS URL without embedded credentials");
    }
    Ok(url)
}

fn validate_pool(value: usize) -> Result<()> {
    if !(1..=128).contains(&value) {
        bail!("database pool size must be between 1 and 128");
    }
    Ok(())
}

pub(crate) fn validate_tenant(value: &str) -> Result<()> {
    if value.len() < 2
        || value.len() > 64
        || !value.bytes().all(|byte| {
            byte.is_ascii_lowercase() || byte.is_ascii_digit() || matches!(byte, b'-' | b'_')
        })
        || !value.as_bytes()[0].is_ascii_lowercase()
    {
        bail!("tenant must be a lowercase site-owned identifier");
    }
    Ok(())
}

fn validate_bounded_id(value: &str, label: &str, max: usize) -> Result<()> {
    if value.is_empty()
        || value.len() > max
        || value
            .chars()
            .any(|character| character.is_control() || character.is_whitespace())
    {
        bail!("{label} is invalid");
    }
    Ok(())
}

/// A display name the site sets, such as a research center. Plain words only; it is shown, never parsed.
fn site_label(value: &str) -> Result<String> {
    let trimmed = value.trim();
    if trimmed.is_empty()
        || trimmed.chars().count() > 120
        || trimmed.chars().any(|character| character.is_control())
        || trimmed.contains(['<', '>'])
    {
        bail!("site organization is invalid");
    }
    Ok(trimmed.split_whitespace().collect::<Vec<_>>().join(" "))
}

fn read_secret_env(name: &str) -> Result<String> {
    env::var(name).with_context(|| format!("required secret environment {name} is not set"))
}

pub(super) fn read_secret(path: &Option<PathBuf>, environment_name: &str) -> Result<String> {
    if let Some(path) = path {
        if !path.is_absolute() {
            bail!("database DSN Secret path must be absolute");
        }
        let metadata = fs::metadata(path).with_context(|| {
            format!(
                "failed to inspect database DSN Secret path {}",
                path.display()
            )
        })?;
        if !metadata.is_file() || metadata.len() == 0 || metadata.len() > 16 * 1024 {
            bail!("database DSN Secret path must resolve to a regular file no larger than 16 KiB");
        }
        let value = fs::read_to_string(path).with_context(|| {
            format!("failed to read database DSN Secret path {}", path.display())
        })?;
        let value = value.trim().to_owned();
        if value.is_empty() {
            bail!("database DSN Secret file is empty");
        }
        return Ok(value);
    }
    read_secret_env(environment_name)
}

fn prepare_private_root(path: &Path, label: &str) -> Result<()> {
    if !path.is_absolute() {
        bail!("{label} must be an absolute path");
    }
    crate::config::create_private_dir(path)
}

fn validate_existing_root(path: &Path, label: &str) -> Result<()> {
    if !path.is_absolute() {
        bail!("{label} must be an absolute path");
    }
    let metadata = fs::metadata(path)
        .with_context(|| format!("failed to inspect {label} {}", path.display()))?;
    if !metadata.is_dir() {
        bail!("{label} must be an existing directory");
    }
    Ok(())
}

fn validate_database_dsn(dsn: &str) -> Result<()> {
    let config = PostgresConfig::from_str(dsn).context("invalid PostgreSQL DSN")?;
    let insecure = format!("{:?}", config.get_ssl_mode()).eq_ignore_ascii_case("disable");
    if insecure {
        use tokio_postgres::config::Host;
        let loopback = config.get_hosts().iter().all(|host| match host {
            Host::Tcp(host) => {
                host == "localhost"
                    || host
                        .parse::<std::net::IpAddr>()
                        .is_ok_and(|address| address.is_loopback())
            }
            #[cfg(unix)]
            Host::Unix(_) => true,
        });
        if !loopback {
            bail!("PostgreSQL sslmode=disable is allowed only for loopback qualification");
        }
    }
    Ok(())
}

fn read_ca_bundle(path: &Path) -> Result<Vec<u8>> {
    if !path.is_absolute() {
        bail!("Access JWKS CA file must be an absolute path");
    }
    let metadata = fs::symlink_metadata(path)
        .with_context(|| format!("failed to inspect Access JWKS CA file {}", path.display()))?;
    if metadata.file_type().is_symlink() || !metadata.is_file() || metadata.len() > 1024 * 1024 {
        bail!("Access JWKS CA file must be a regular non-symlink file no larger than 1 MiB");
    }
    let body = fs::read(path)
        .with_context(|| format!("failed to read Access JWKS CA file {}", path.display()))?;
    if body.is_empty() {
        bail!("Access JWKS CA file is empty");
    }
    Ok(body)
}

#[cfg(test)]
mod tests {
    use std::{fs, os::unix::fs::PermissionsExt, path::Path};

    use super::{ApiAuthentication, ServeArgs, ServeAuthMode, parse_https_url};

    fn service_account_config(root: &Path) -> std::path::PathBuf {
        let root = root.canonicalize().unwrap();
        fs::set_permissions(&root, fs::Permissions::from_mode(0o700)).unwrap();
        let ca = root.join("ca.crt");
        let config = root.join("service-account-auth.json");
        fs::write(&ca, "fixture-ca").unwrap();
        fs::set_permissions(&ca, fs::Permissions::from_mode(0o600)).unwrap();
        fs::write(
            &config,
            serde_json::json!({
                "schemaVersion": "enrichedlab.broker.service-account-auth/v1",
                "apiServer": "https://kubernetes.example.invalid",
                "reviewerTokenRoot": "/var/run/enrichedlab/reviewer-source",
                "reviewerTokenFile": "reviewer-token",
                "caFile": ca,
                "audience": "enrichedlab-broker-submit",
                "mappings": [{
                    "namespace": "qualification-retained-byte",
                    "serviceAccount": "task-submit",
                    "catalogEmail": "task-submit@example.invalid"
                }]
            })
            .to_string(),
        )
        .unwrap();
        fs::set_permissions(&config, fs::Permissions::from_mode(0o600)).unwrap();
        config
    }

    fn service_account_only_args(root: &Path, auth_file: std::path::PathBuf) -> ServeArgs {
        let artifact_root = root.join("artifacts");
        fs::create_dir(&artifact_root).unwrap();
        let dsn = root.join("database.dsn");
        fs::write(&dsn, "postgresql://localhost/broker?sslmode=disable").unwrap();
        ServeArgs {
            workspace_ui: false,
            change_stream: true,
            combine_starting_points: false,
            workspace_catalog: None,
            library_config: None,
            knowledge_person_config: None,
            dataset_guard_config: None,
            site_organization: None,
            bind: "127.0.0.1:32110".parse().unwrap(),
            auth_mode: ServeAuthMode::ServiceAccountOnly,
            public_url: None,
            access_issuer: None,
            access_audience: None,
            access_jwks_url: None,
            access_jwks_ca_file: None,
            service_account_auth_file: Some(auth_file),
            database_dsn_env: "TEST_DATABASE_DSN".into(),
            database_dsn_file: Some(dsn),
            tenant: "qualification".into(),
            artifact_root,
            database_pool: 1,
        }
    }

    #[test]
    fn service_account_only_resolves_without_cloudflare_inputs() {
        let root = tempfile::tempdir().unwrap();
        let args = service_account_only_args(root.path(), service_account_config(root.path()));
        let config = args.resolve().unwrap();
        assert!(config.public_url.is_none());
        assert!(matches!(
            config.authentication,
            ApiAuthentication::ServiceAccountOnly { .. }
        ));
    }

    #[test]
    fn site_organization_is_optional_trimmed_and_plain() {
        let root = tempfile::tempdir().unwrap();
        let base = service_account_only_args(root.path(), service_account_config(root.path()));
        assert!(base.clone().resolve().unwrap().site_organization.is_none());

        let mut args = base.clone();
        args.site_organization = Some("  Western   Research Center ".into());
        assert_eq!(
            args.resolve().unwrap().site_organization.as_deref(),
            Some("Western Research Center")
        );

        for invalid in ["", "   ", "Center<script>", "Line\nbreak", &"x".repeat(121)] {
            let mut args = base.clone();
            args.site_organization = Some(invalid.to_string());
            assert!(args.resolve().is_err(), "{invalid:?} was accepted");
        }
    }

    #[test]
    fn cloudflare_mode_keeps_the_existing_optional_service_account_fallback() {
        let root = tempfile::tempdir().unwrap();
        let mut args = service_account_only_args(root.path(), service_account_config(root.path()));
        args.auth_mode = ServeAuthMode::Cloudflare;
        args.public_url = Some("https://broker.example.invalid".into());
        args.access_issuer = Some("https://team.cloudflareaccess.com".into());
        args.access_audience = Some("access-audience".into());
        let config = args.resolve().unwrap();
        assert!(matches!(
            config.authentication,
            ApiAuthentication::Cloudflare {
                service_account_auth: Some(_),
                ..
            }
        ));
    }

    #[test]
    fn service_account_only_requires_a_private_mapping() {
        let root = tempfile::tempdir().unwrap();
        let mut args = service_account_only_args(root.path(), service_account_config(root.path()));
        args.service_account_auth_file = None;
        assert!(
            args.resolve()
                .unwrap_err()
                .to_string()
                .contains("configuration is required in ServiceAccount-only mode")
        );
    }

    #[test]
    fn service_account_only_rejects_cloudflare_inputs() {
        let root = tempfile::tempdir().unwrap();
        let mut args = service_account_only_args(root.path(), service_account_config(root.path()));
        args.access_audience = Some("unexpected-audience".into());
        assert!(
            args.resolve()
                .unwrap_err()
                .to_string()
                .contains("not allowed in ServiceAccount-only mode")
        );
    }

    #[test]
    fn https_urls_reject_embedded_credentials() {
        assert!(parse_https_url("https://user:pass@example.test", "test URL").is_err());
        assert!(parse_https_url("http://example.test", "test URL").is_err());
        assert!(parse_https_url("https://example.test", "test URL").is_ok());
    }

    #[test]
    fn a_readers_certificate_authority_sits_beside_its_connection_and_holds_certificates_only() {
        let root = tempfile::tempdir().unwrap();
        let dsn = root.path().join("lab-reader.dsn");
        fs::write(
            &dsn,
            "host=db.example port=5432 dbname=lab user=reader sslmode=require\n",
        )
        .unwrap();
        // No authority: the connection trusts what the process trusts.
        assert!(super::reader_connection(&dsn).unwrap().ca.is_none());
        let pem = "-----BEGIN CERTIFICATE-----\nAAAA\n-----END CERTIFICATE-----\n";
        // A Secret volume links each key into place; the link is followed.
        let data = root.path().join("..data");
        fs::create_dir(&data).unwrap();
        fs::write(data.join("lab-reader.ca.crt"), pem).unwrap();
        std::os::unix::fs::symlink(
            "..data/lab-reader.ca.crt",
            root.path().join("lab-reader.ca.crt"),
        )
        .unwrap();
        assert_eq!(
            super::reader_connection(&dsn).unwrap().ca.unwrap(),
            pem.as_bytes()
        );
        let cert = "-----BEGIN CERTIFICATE-----\nAAAA\n-----END CERTIFICATE-----";
        let bundle = format!("\n{cert}\r\n\n{cert}\n\n");
        assert_eq!(
            super::pem_certificate_blocks(bundle.as_bytes())
                .unwrap()
                .len(),
            2
        );
        for refused in [
            String::new(),
            "not a certificate".into(),
            "-----BEGIN CERTIFICATE-----\nAAAA\n".into(),
            "-----BEGIN CERTIFICATE-----\n\n-----END CERTIFICATE-----\n".into(),
            // A synthetic block whose body is not a key: the bundle must refuse any non-certificate block. The
            // label is assembled so the source holds no key header for secret scanners to report.
            format!(
                "{cert}\n-----BEGIN {label}-----\nBBBB\n-----END {label}-----\n",
                label = ["PRIVATE", "KEY"].join(" ")
            ),
            format!("{cert}\n-----BEGIN PUBLIC KEY-----\nBBBB\n-----END PUBLIC KEY-----\n"),
            format!("{cert}\n-----BEGIN X509 CRL-----\nBBBB\n-----END X509 CRL-----\n"),
            format!("{cert}\ntrailing text\n"),
            format!("subject=CN = lab\n{cert}\n"),
            "-----BEGIN CERTIFICATE-----\nAA AA\n-----END CERTIFICATE-----\n".into(),
        ] {
            fs::write(data.join("lab-reader.ca.crt"), &refused).unwrap();
            assert!(super::reader_connection(&dsn).is_err(), "{refused:?}");
        }
        // The size bound holds for what is read, whatever the file reported first.
        let line = format!("{cert}\n");
        let fits = 64 * 1024 / line.len();
        fs::write(data.join("lab-reader.ca.crt"), line.repeat(fits + 1)).unwrap();
        assert!(super::reader_connection(&dsn).is_err());
        fs::write(data.join("lab-reader.ca.crt"), line.repeat(fits)).unwrap();
        assert_eq!(
            super::reader_connection(&dsn).unwrap().ca.unwrap().len(),
            fits * line.len()
        );
        // A link whose target is gone is a broken authority, never an absent one.
        fs::remove_file(data.join("lab-reader.ca.crt")).unwrap();
        let broken = super::reader_connection(&dsn);
        assert!(
            broken.is_err(),
            "a dangling authority link fell back to the process's trust"
        );
        fs::create_dir(data.join("lab-reader.ca.crt")).unwrap();
        assert!(super::reader_connection(&dsn).is_err());
    }
}
