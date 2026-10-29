use std::{ffi::OsString, net::IpAddr, path::PathBuf};

use clap::{Args, Parser, Subcommand, ValueEnum};
use ipnet::IpNet;
use serde::{Deserialize, Serialize};

use crate::{
    agent_state::{ArtifactSensitivity, StateAudience, StateAuthority, StateDomain},
    config::ProvenanceBackend,
    harness::HarnessKind,
    platform::RuntimeAdapter,
    research::ResearchMode,
};

pub const DEFAULT_IMAGE: &str = "ghcr.io/nvidia/openshell-community/sandboxes/base@sha256:aeef1c63f00e2913ea002ccb3aaf925f338b5c5d70e63576f0d95c16a138044e";

#[derive(Debug, Parser)]
#[command(
    name = "agent-sandbox",
    version,
    about = "Run and govern isolated AI subagents"
)]
pub struct Cli {
    /// Root for controller-created projects and output folders.
    #[arg(long, global = true)]
    pub root: Option<PathBuf>,

    /// Override the private controller state directory.
    #[arg(long, global = true)]
    pub state_dir: Option<PathBuf>,

    /// Override the controller configuration directory.
    #[arg(long, global = true)]
    pub config_dir: Option<PathBuf>,

    /// OpenShell CLI path.
    #[arg(long, global = true, default_value = "openshell")]
    pub openshell_bin: PathBuf,

    /// Docker-compatible CLI path for the current runtime adapter.
    #[arg(long, global = true, default_value = "docker")]
    pub docker_bin: PathBuf,

    /// Human-readable or machine-readable output.
    #[arg(long, global = true, value_enum, default_value_t = OutputFormat::Human)]
    pub output: OutputFormat,

    #[command(subcommand)]
    pub command: Command,
}

#[derive(Clone, Copy, Debug, Default, ValueEnum)]
pub enum OutputFormat {
    #[default]
    Human,
    Json,
}

#[derive(Debug, Subcommand)]
pub enum Command {
    /// Create one retained sandbox.
    Create(CreateArgs),
    /// Create a bounded pool of retained sandboxes.
    Pool(PoolArgs),
    /// List sandboxes owned by this controller.
    List,
    /// Run a command in a managed sandbox.
    Exec(ExecArgs),
    /// Install a pinned supported harness in a managed sandbox.
    Bootstrap(BootstrapArgs),
    /// Run a supported subagent and record its provenance stream.
    Run(RunArgs),
    /// Open an interactive shell in a managed sandbox.
    Shell(NameArgs),
    /// Stop a managed sandbox while keeping its container and workspace.
    Stop(NameArgs),
    /// Start a stopped managed sandbox.
    Start(NameArgs),
    /// Delete a managed sandbox. Host data is kept by default.
    Delete(DeleteArgs),
    /// Add an exact public HTTPS host to a live sandbox policy.
    Allow(AllowArgs),
    /// Remove a host previously added by this controller.
    Deny(DenyArgs),
    /// Manage host, LAN, and sandbox-to-sandbox service leases.
    Service {
        #[command(subcommand)]
        command: ServiceCommand,
    },
    /// Review custom sandbox requests from the mounted control inbox.
    Request {
        #[command(subcommand)]
        command: RequestCommand,
    },
    /// Review OpenShell Policy Advisor proposals.
    Proposal {
        #[command(subcommand)]
        command: ProposalCommand,
    },
    /// Manage isolated team workspaces and host-led integration.
    Team {
        #[command(subcommand)]
        command: TeamCommand,
    },
    /// Run isolated, reproducible research workers and seal their outputs.
    Research {
        #[command(subcommand)]
        command: ResearchCommand,
    },
    /// Manage a stable logical agent across disposable sandbox runtimes.
    Agent {
        #[command(subcommand)]
        command: ContinuityAgentCommand,
    },
    /// Manage bounded execution state, provenance events, and immutable artifacts.
    State {
        #[command(subcommand)]
        command: AgentStateCommand,
    },
    /// Inspect or configure the append-only provenance and evidence store.
    Provenance {
        #[command(subcommand)]
        command: ProvenanceCommand,
    },
    /// Push or pull workspaces for Kubernetes and VM sandboxes.
    Sync {
        #[command(subcommand)]
        command: SyncCommand,
    },
    /// Inspect configured OpenShell gateways and runtime proofs.
    Platform {
        #[command(subcommand)]
        command: PlatformCommand,
    },
    /// Register and verify reusable outputs.
    Catalog {
        #[command(subcommand)]
        command: CatalogCommand,
    },
    /// Capture and verify local evidence.
    Audit {
        #[command(subcommand)]
        command: AuditCommand,
    },
    /// Manage the local certificate authority used for LAN leases.
    Pki {
        #[command(subcommand)]
        command: PkiCommand,
    },
    /// Run the loopback operator dashboard and JSON API.
    Dashboard(DashboardArgs),
    /// Move records from the pre-0.2 local state directory.
    MigrateLegacy,
    /// Check OpenShell, the runtime, routes, policy settings, and local state.
    Doctor,
    #[command(hide = true)]
    Internal {
        #[command(subcommand)]
        command: InternalCommand,
    },
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize, ValueEnum, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum ApprovalMode {
    #[default]
    Manual,
    AutoSafe,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize, ValueEnum, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum LandlockMode {
    #[default]
    HardRequirement,
    BestEffort,
}

#[derive(Clone, Debug, Args, Deserialize, Serialize)]
pub struct RuntimeArgs {
    /// Named inference route from routes.json.
    #[arg(long)]
    pub route: Option<String>,

    /// Named policy and tool profile.
    #[arg(long)]
    pub profile: Option<String>,

    /// Named OpenShell gateway and compute-driver profile.
    #[arg(long = "gateway-profile")]
    pub gateway_profile: Option<String>,

    /// Pinned sandbox image reference.
    #[arg(long, default_value = DEFAULT_IMAGE)]
    pub image: String,

    /// Runtime CPU limit.
    #[arg(long, default_value = "2")]
    pub cpu: String,

    /// Runtime memory limit.
    #[arg(long, default_value = "4Gi")]
    pub memory: String,

    /// Custom request approval mode. OpenShell network proposals remain manual.
    #[arg(long, value_enum, default_value_t = ApprovalMode::Manual)]
    pub approval_mode: ApprovalMode,

    /// Landlock handling in the OpenShell process policy.
    #[arg(long, value_enum, default_value_t = LandlockMode::HardRequirement)]
    pub landlock: LandlockMode,

    /// Extra public HTTPS host with read-only HTTP access. Repeatable.
    #[arg(long = "allow-host")]
    pub read_hosts: Vec<String>,

    /// Extra public HTTPS host with all HTTP methods. Repeatable.
    #[arg(long = "allow-write-host")]
    pub write_hosts: Vec<String>,

    /// Extra host directory mounted read-only under /sandbox/mounts. Repeatable.
    #[arg(long = "mount-ro")]
    pub read_only_mounts: Vec<PathBuf>,

    /// Extra host directory mounted read-write under /sandbox/mounts. Repeatable.
    #[arg(long = "mount-rw")]
    pub read_write_mounts: Vec<PathBuf>,
}

impl Default for RuntimeArgs {
    fn default() -> Self {
        Self {
            route: None,
            profile: None,
            gateway_profile: None,
            image: DEFAULT_IMAGE.into(),
            cpu: "2".into(),
            memory: "4Gi".into(),
            approval_mode: ApprovalMode::Manual,
            landlock: LandlockMode::HardRequirement,
            read_hosts: Vec::new(),
            write_hosts: Vec::new(),
            read_only_mounts: Vec::new(),
            read_write_mounts: Vec::new(),
        }
    }
}

#[derive(Debug, Subcommand)]
pub enum SyncCommand {
    /// Replace the sandbox workspace from the validated host workspace.
    Push { sandbox: String },
    /// Capture a remote snapshot. --apply replaces the host workspace after a backup.
    Pull {
        sandbox: String,
        #[arg(long)]
        apply: bool,
    },
}

#[derive(Debug, Subcommand)]
pub enum PlatformCommand {
    /// List configured gateway profiles and their current health.
    List,
    /// Inspect one gateway profile and its managed sandboxes.
    Inspect { profile: String },
    /// Refresh runtime proof for one sandbox or every managed sandbox.
    Refresh { sandbox: Option<String> },
    /// Add or replace a named gateway profile with safe transport defaults.
    Configure(PlatformConfigureArgs),
    /// Select an enabled gateway profile as the default.
    SetDefault { profile: String },
    /// Remove an unused non-default gateway profile.
    Remove { profile: String },
}

#[derive(Debug, Args)]
pub struct PlatformConfigureArgs {
    /// Local controller name for this gateway profile.
    #[arg(long)]
    pub name: String,

    /// Registered OpenShell gateway name.
    #[arg(long)]
    pub gateway: String,

    /// OpenShell compute driver behind the gateway.
    #[arg(long, value_enum)]
    pub driver: RuntimeAdapter,

    /// Treat an unavailable gateway as a failed doctor check.
    #[arg(long)]
    pub required: bool,

    /// Replace an existing unused profile with the same name.
    #[arg(long)]
    pub replace: bool,

    /// Select this profile as the controller default.
    #[arg(long)]
    pub set_default: bool,

    /// Kubernetes sandbox namespace.
    #[arg(long, default_value = "agent-sandboxes")]
    pub namespace: String,

    /// Kubernetes RuntimeClass requested for every sandbox in this profile.
    #[arg(long)]
    pub runtime_class: Option<String>,

    /// Absolute kubeconfig path for an out-of-cluster observer.
    #[arg(long)]
    pub kubeconfig: Option<PathBuf>,

    /// Kubernetes context used by the observer.
    #[arg(long)]
    pub context: Option<String>,

    /// Absolute kubectl-compatible observer binary path.
    #[arg(long)]
    pub kubectl_bin: Option<PathBuf>,

    /// Let the controller create and delete exact lease-owned peer NetworkPolicies.
    #[arg(long)]
    pub enable_peer_policies: bool,

    /// Private IPv4 address on the controller host used only for Kubernetes peer relays.
    #[arg(long)]
    pub peer_gateway_ip: Option<IpAddr>,

    /// Maximum files copied for VM or Kubernetes sync transport.
    #[arg(long, default_value_t = 100_000)]
    pub sync_max_files: u64,

    /// Maximum bytes copied for VM or Kubernetes sync transport.
    #[arg(long, default_value_t = 21_474_836_480)]
    pub sync_max_bytes: u64,
}

#[derive(Debug, Args)]
pub struct CreateArgs {
    /// DNS-safe sandbox name. A random name is used when omitted.
    #[arg(long)]
    pub name: Option<String>,

    /// Exact host directory mounted read-write at /sandbox/work.
    #[arg(long)]
    pub workspace: Option<PathBuf>,

    #[command(flatten)]
    pub runtime: RuntimeArgs,
}

#[derive(Debug, Args)]
pub struct PoolArgs {
    /// Number of sandboxes. The route's configured pool cap is enforced.
    #[arg(long, default_value_t = 5)]
    pub count: usize,

    /// Mount the same host directory at /sandbox/work in every pool member.
    #[arg(long)]
    pub shared_workspace: Option<PathBuf>,

    #[command(flatten)]
    pub runtime: RuntimeArgs,
}

#[derive(Debug, Args)]
pub struct NameArgs {
    pub name: String,
}

#[derive(Debug, Args)]
pub struct BootstrapArgs {
    pub name: String,

    /// Agent harness to install.
    #[arg(long, value_enum, default_value_t = HarnessKind::OpenCode)]
    pub harness: HarnessKind,
}

#[derive(Debug, Args)]
pub struct ExecArgs {
    pub name: String,

    #[arg(required = true, trailing_var_arg = true, allow_hyphen_values = true)]
    pub command: Vec<OsString>,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, ValueEnum, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum Thinking {
    Off,
    Low,
    Medium,
    On,
    Xhigh,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize, ValueEnum, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum RunnableHarness {
    #[default]
    OpenCode,
    Pi,
}

impl From<RunnableHarness> for HarnessKind {
    fn from(value: RunnableHarness) -> Self {
        match value {
            RunnableHarness::OpenCode => HarnessKind::OpenCode,
            RunnableHarness::Pi => HarnessKind::Pi,
        }
    }
}

impl Thinking {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Off => "off",
            Self::Low => "low",
            Self::Medium => "medium",
            Self::On => "on",
            Self::Xhigh => "xhigh",
        }
    }
}

#[derive(Debug, Args)]
pub struct RunArgs {
    pub name: String,

    /// Agent harness used for this run.
    #[arg(long, value_enum, default_value_t = HarnessKind::OpenCode)]
    pub harness: HarnessKind,

    /// Route-specific reasoning mode.
    #[arg(long, value_enum)]
    pub thinking: Option<Thinking>,

    /// Task for the isolated agent.
    #[arg(required = true, trailing_var_arg = true, allow_hyphen_values = true)]
    pub message: Vec<OsString>,
}

#[derive(Debug, Args)]
pub struct DeleteArgs {
    pub name: String,

    /// Move a controller-created workspace to the platform trash after deletion.
    #[arg(long)]
    pub trash_workspace: bool,

    /// Delete without first capturing OCSF and runtime evidence.
    #[arg(long)]
    pub skip_evidence: bool,
}

#[derive(Debug, Args)]
pub struct AllowArgs {
    pub name: String,
    pub host: String,

    /// Permit write methods in addition to read methods.
    #[arg(long)]
    pub write: bool,
}

#[derive(Debug, Args)]
pub struct DenyArgs {
    pub name: String,
    pub host: String,
}

#[derive(Debug, Subcommand)]
pub enum ServiceCommand {
    /// Forward one sandbox loopback service to Mac/Linux host loopback.
    Open(ServiceOpenArgs),
    /// Add an mTLS LAN listener in front of a host service lease.
    Lan(ServiceLanArgs),
    /// Link one source sandbox to one target through a runtime-specific relay.
    Peer(ServicePeerArgs),
    /// List active and stopped leases.
    List,
    /// Close one lease and its dependent leases.
    Close { lease_id: String },
}

#[derive(Debug, Args)]
pub struct ServiceOpenArgs {
    pub sandbox: String,
    #[arg(long)]
    pub target_port: u16,
    #[arg(long, default_value_t = 0)]
    pub local_port: u16,
    #[arg(long, default_value_t = 60)]
    pub ttl_minutes: u64,
    #[arg(long, default_value = "service")]
    pub label: String,
}

#[derive(Debug, Args)]
pub struct ServiceLanArgs {
    pub host_lease_id: String,
    #[arg(long)]
    pub bind: IpAddr,
    #[arg(long, default_value_t = 0)]
    pub local_port: u16,
    /// Client source networks. Repeatable and required.
    #[arg(long = "source", required = true)]
    pub sources: Vec<IpNet>,
    /// Issued client certificate profiles allowed on this lease. Repeatable.
    #[arg(long = "client", required = true)]
    pub clients: Vec<String>,
    #[arg(long, default_value_t = 60)]
    pub ttl_minutes: u64,
}

#[derive(Debug, Args)]
pub struct ServicePeerArgs {
    pub source: String,
    pub target: String,
    #[arg(long)]
    pub target_port: u16,
    /// Loopback port exposed only inside the source sandbox.
    #[arg(long, alias = "host-port", default_value_t = 0)]
    pub source_port: u16,
    #[arg(long, default_value_t = 60)]
    pub ttl_minutes: u64,
}

#[derive(Debug, Subcommand)]
pub enum RequestCommand {
    /// List validated pending requests.
    List { sandbox: Option<String> },
    /// Show the immutable normalized copy of one request.
    Get { sandbox: String, request_id: String },
    /// Approve and apply one request.
    Approve {
        sandbox: String,
        request_id: String,
        /// Decision rationale recorded in the audit chain. Defaults to the worker's request reason.
        #[arg(long)]
        reason: Option<String>,
        /// Local controller actor. Reserved for reviewed host integrations.
        #[arg(long, hide = true, default_value = "host-cli")]
        actor: String,
    },
    /// Reject one request with a recorded reason.
    Reject {
        sandbox: String,
        request_id: String,
        #[arg(long)]
        reason: String,
        /// Local controller actor. Reserved for reviewed host integrations.
        #[arg(long, hide = true, default_value = "host-cli")]
        actor: String,
    },
    /// Process only request kinds allowed by the controller auto-safe settings.
    ProcessAuto,
    /// Print the request JSON schema and sandbox path.
    Schema,
}

#[derive(Debug, Subcommand)]
pub enum ProposalCommand {
    Pending {
        sandbox: String,
    },
    History {
        sandbox: String,
    },
    Approve {
        sandbox: String,
        chunk_id: String,
    },
    Reject {
        sandbox: String,
        chunk_id: String,
        #[arg(long)]
        reason: String,
    },
}

#[derive(Debug, Subcommand)]
pub enum TeamCommand {
    Create(TeamCreateArgs),
    Add(Box<TeamAddArgs>),
    Status {
        team: String,
    },
    Collect {
        team: String,
        worker: String,
    },
    Merge {
        team: String,
        worker: String,
    },
    Stop {
        team: String,
    },
    /// Delete every worker sandbox while keeping the team project and outputs.
    Delete {
        team: String,
        /// Delete without first capturing each sandbox's runtime evidence.
        #[arg(long)]
        skip_evidence: bool,
    },
}

#[derive(Debug, Args)]
pub struct TeamCreateArgs {
    pub name: String,
    #[arg(long)]
    pub repo: PathBuf,
    #[arg(long)]
    pub shared_output: bool,
}

#[derive(Debug, Args)]
pub struct TeamAddArgs {
    pub team: String,
    pub worker: String,
    /// Owned repository path prefix. Repeatable and required.
    #[arg(long = "owns", required = true)]
    pub owns: Vec<String>,
    #[arg(long)]
    pub task: String,
    #[command(flatten)]
    pub runtime: RuntimeArgs,
}

#[derive(Debug, Subcommand)]
pub enum ResearchCommand {
    Create(ResearchCreateArgs),
    Add(Box<ResearchAddArgs>),
    Status {
        study: String,
    },
    Launch {
        study: String,
        #[arg(long, default_value_t = 4)]
        max_parallel: usize,
        /// Fresh provider sessions allowed per worker before an incomplete run fails.
        #[arg(long, default_value_t = 8)]
        max_segments: u32,
    },
    Collect {
        study: String,
        worker: String,
    },
    Compare {
        study: String,
    },
    Verify {
        study: String,
    },
    ResetWorker {
        study: String,
        worker: String,
        #[arg(long)]
        reason: String,
    },
    Stop {
        study: String,
    },
    /// Delete worker sandboxes while retaining the study, outputs, and seals.
    Delete {
        study: String,
        #[arg(long)]
        skip_evidence: bool,
    },
}

#[derive(Debug, Subcommand)]
pub enum ContinuityAgentCommand {
    /// Register a logical agent and its durable scoped state without starting compute.
    Create(Box<ContinuityAgentCreateArgs>),
    /// List logical agents or inspect one exact agent.
    Status { name: Option<String> },
    /// Allocate a replacement sandbox and restore the current state view.
    Start { name: String },
    /// Run bounded fresh provider sessions, starting compute when needed.
    Run(Box<ContinuityAgentRunArgs>),
    /// Delete disposable compute while retaining the agent, workspace, state, and evidence.
    Release {
        name: String,
        /// Release without first capturing the sandbox runtime evidence.
        #[arg(long)]
        skip_evidence: bool,
    },
    /// Create an explicit state checkpoint without releasing compute.
    Checkpoint {
        name: String,
        #[arg(long)]
        reason: String,
    },
    /// Seal the state run and prevent any new runtime allocation.
    Retire {
        name: String,
        #[arg(long)]
        reason: String,
        /// Retire without first capturing the active sandbox runtime evidence.
        #[arg(long)]
        skip_evidence: bool,
    },
}

#[derive(Debug, Args)]
pub struct ContinuityAgentCreateArgs {
    /// Stable, human-readable logical-agent alias.
    pub name: String,

    /// Stable owner reference. Shared control planes must pass their canonical principal.
    #[arg(long, default_value = "local-operator")]
    pub owner: String,

    #[arg(long, value_enum, default_value_t = StateDomain::ClusterOps)]
    pub domain: StateDomain,

    /// Prompt-facing state profile. Built-in profiles seed their required collections.
    #[arg(long, default_value = "cluster-operator")]
    pub state_profile: String,

    #[arg(long, conflicts_with = "objective_file")]
    pub objective: Option<String>,

    #[arg(long, conflicts_with = "objective")]
    pub objective_file: Option<PathBuf>,

    /// Role text carried in the scoped coordination state.
    #[arg(long, default_value = "autonomous subagent")]
    pub role: String,

    #[arg(long, value_enum, default_value_t = RunnableHarness::OpenCode)]
    pub harness: RunnableHarness,

    #[arg(long, value_enum)]
    pub thinking: Option<Thinking>,

    #[command(flatten)]
    pub runtime: RuntimeArgs,
}

#[derive(Debug, Args)]
pub struct ContinuityAgentRunArgs {
    pub name: String,

    /// Override the registered harness for this invocation.
    #[arg(long, value_enum)]
    pub harness: Option<RunnableHarness>,

    /// Override the registered reasoning mode for this invocation.
    #[arg(long, value_enum)]
    pub thinking: Option<Thinking>,

    /// Fresh provider sessions allowed. Values above one explicitly allow task retry.
    #[arg(long, default_value_t = 1)]
    pub max_attempts: u32,

    /// Initial delay before an explicitly enabled retry. Later waits use bounded backoff.
    #[arg(long, default_value_t = 500)]
    pub retry_delay_ms: u64,

    /// Deadline for one provider session before it is stopped and classified as timed out.
    #[arg(long, default_value_t = 3_600)]
    pub attempt_timeout_seconds: u64,

    /// Keep disposable compute allocated after this task. The default scales the runtime to zero.
    #[arg(long)]
    pub keep_runtime: bool,

    /// Task for the logical agent.
    #[arg(required = true, trailing_var_arg = true, allow_hyphen_values = true)]
    pub message: Vec<OsString>,
}

#[derive(Debug, Args)]
pub struct ResearchCreateArgs {
    pub name: String,
    #[arg(long, value_enum, default_value_t = ResearchMode::Deterministic)]
    pub mode: ResearchMode,
    #[arg(long, conflicts_with = "question_file")]
    pub question: Option<String>,
    #[arg(long, conflicts_with = "question")]
    pub question_file: Option<PathBuf>,
    #[arg(long)]
    pub protocol: Option<PathBuf>,
    #[arg(long = "input")]
    pub inputs: Vec<PathBuf>,
    #[arg(long, default_value_t = 100_000)]
    pub max_output_files: u64,
    #[arg(long, default_value_t = 4_294_967_296)]
    pub max_output_bytes: u64,
}

#[derive(Debug, Args)]
pub struct ResearchAddArgs {
    pub study: String,
    pub worker: String,
    #[arg(long, conflicts_with = "perspective_file")]
    pub perspective: Option<String>,
    #[arg(long, conflicts_with = "perspective")]
    pub perspective_file: Option<PathBuf>,
    #[arg(long = "tool")]
    pub tools: Vec<String>,
    #[arg(long, value_enum)]
    pub thinking: Option<Thinking>,
    /// Agent harness used by this worker. Both supported harnesses emit reviewed JSONL.
    #[arg(long, value_enum, default_value_t = HarnessKind::OpenCode)]
    pub harness: HarnessKind,
    #[command(flatten)]
    pub runtime: RuntimeArgs,
}

#[derive(Debug, Subcommand)]
pub enum AgentStateCommand {
    /// Create a state run. The scientific-worker profile seeds its standard collections.
    Init(StateInitArgs),
    /// List state runs without exposing state or event payloads.
    List,
    /// Build the prompt-facing view bound to a run's profile.
    View { run: String, subject: String },
    /// Seed one schema-validated collection from a JSON document.
    Seed(StateSeedArgs),
    /// Apply one explicit, optimistic state mutation.
    Patch(StatePatchArgs),
    /// Append one structured event from a JSON payload.
    Event(StateEventArgs),
    /// Add one immutable file to the content-addressed artifact store.
    Artifact(StateArtifactArgs),
    /// Check context pressure against a named state profile.
    Context(StateContextArgs),
    /// Seal current state and begin a new context epoch.
    Checkpoint(StateCheckpointArgs),
    /// Mark a completed run sealed for lifecycle reporting.
    Seal(StateSealArgs),
    /// Reconcile model proposals from a workspace outbox and refresh its view.
    Reconcile(StateReconcileArgs),
    /// Return the complete append-only event trace for one run.
    Trace { run: String },
    /// Export a reviewable RO-Crate-shaped run package and SHA-256 manifest.
    Export(StateExportArgs),
    /// Verify state schemas, event chains, and artifact digests.
    Verify,
    /// List or install view profiles.
    Profile {
        #[command(subcommand)]
        command: StateProfileCommand,
    },
    /// List or install collection schemas.
    Schema {
        #[command(subcommand)]
        command: StateSchemaCommand,
    },
}

#[derive(Debug, Subcommand)]
pub enum ProvenanceCommand {
    /// Create or migrate the selected storage backend and report its controls.
    Prepare,
    /// Select embedded or PostgreSQL storage without placing a DSN in the settings file.
    Configure(ProvenanceConfigureArgs),
    /// Normalize one reviewed JSONL stream into the append-only provenance store.
    Ingest(ProvenanceIngestArgs),
    /// Return a metadata-only provenance graph for one run.
    Graph {
        run: String,
        #[arg(long, default_value_t = 5_000)]
        limit: usize,
    },
    /// Export one run's append-only events for offline owner review.
    Export(ProvenanceExportArgs),
}

#[derive(Debug, Args)]
pub struct ProvenanceExportArgs {
    pub run: String,
    #[arg(long)]
    pub output_dir: PathBuf,
    /// Include provider-returned output, reasoning, tool I/O, and errors.
    #[arg(long)]
    pub include_restricted: bool,
}

#[derive(Debug, Args)]
pub struct ProvenanceConfigureArgs {
    #[arg(long, value_enum)]
    pub backend: ProvenanceBackend,
    #[arg(long, default_value = "local")]
    pub tenant: String,
    /// Environment variable that contains the PostgreSQL DSN. The DSN is never persisted.
    #[arg(long, default_value = "AGENT_SANDBOX_POSTGRES_DSN")]
    pub postgres_dsn_env: String,
    /// Environment variable for the required migration-owner DSN.
    #[arg(long, default_value = "AGENT_SANDBOX_POSTGRES_MIGRATION_DSN")]
    pub postgres_migration_dsn_env: String,
}

#[derive(Debug, Args)]
pub struct ProvenanceIngestArgs {
    pub run: String,
    pub subject: String,
    #[arg(long, value_enum)]
    pub harness: HarnessKind,
    /// JSONL source file, or - for stdin.
    #[arg(long, default_value = "-")]
    pub input: PathBuf,
    #[arg(long, default_value_t = 1)]
    pub segment: u32,
    #[arg(long)]
    pub provider: Option<String>,
    #[arg(long)]
    pub model: Option<String>,
    #[arg(long)]
    pub engine: Option<String>,
    #[arg(long)]
    pub parser: Option<String>,
    #[arg(long)]
    pub session: Option<String>,
}

#[derive(Debug, Args)]
pub struct StateInitArgs {
    pub run: String,
    #[arg(long, value_enum)]
    pub domain: StateDomain,
    #[arg(long)]
    pub profile: String,
    #[arg(long)]
    pub subject: String,
    #[arg(long)]
    pub team: Option<String>,
    /// Required when using the scientific-worker profile.
    #[arg(long)]
    pub objective: Option<String>,
}

#[derive(Debug, Args)]
pub struct StateSeedArgs {
    pub run: String,
    pub subject: String,
    pub collection: String,
    #[arg(long)]
    pub schema: String,
    #[arg(long)]
    pub value_file: PathBuf,
    #[arg(long = "authority", value_enum, required = true)]
    pub authorities: Vec<StateAuthority>,
    #[arg(long = "audience", value_enum, required = true)]
    pub audiences: Vec<StateAudience>,
    /// Restrict every audience rule to these principals. Empty means any principal in that audience.
    #[arg(long = "principal")]
    pub principals: Vec<String>,
    #[arg(long, default_value_t = true, action = clap::ArgAction::Set)]
    pub prompt_eligible: bool,
}

#[derive(Debug, Args)]
pub struct StatePatchArgs {
    #[arg(long)]
    pub file: PathBuf,
    #[arg(long, default_value = "host-controller")]
    pub actor: String,
    #[arg(long, value_enum, default_value_t = StateAuthority::Operator)]
    pub authority: StateAuthority,
}

#[derive(Debug, Args)]
pub struct StateEventArgs {
    pub run: String,
    pub subject: String,
    #[arg(long)]
    pub kind: String,
    #[arg(long)]
    pub payload_file: PathBuf,
    #[arg(long, default_value = "host-controller")]
    pub actor: String,
    #[arg(long, value_enum, default_value_t = StateAuthority::Machine)]
    pub authority: StateAuthority,
}

#[derive(Debug, Args)]
pub struct StateArtifactArgs {
    pub run: String,
    pub subject: String,
    #[arg(long)]
    pub file: PathBuf,
    #[arg(long)]
    pub media_type: String,
    #[arg(long)]
    pub role: String,
    #[arg(long)]
    pub label: String,
    #[arg(long, value_enum, default_value_t = ArtifactSensitivity::Standard)]
    pub sensitivity: ArtifactSensitivity,
    #[arg(long, default_value = "host-controller")]
    pub actor: String,
}

#[derive(Debug, Args)]
pub struct StateContextArgs {
    #[arg(long)]
    pub profile: String,
    #[arg(long)]
    pub window_tokens: u64,
    #[arg(long)]
    pub used_tokens: u64,
    #[arg(long, default_value_t = 0)]
    pub output_reserve_tokens: u64,
}

#[derive(Debug, Args)]
pub struct StateCheckpointArgs {
    pub run: String,
    pub subject: String,
    #[arg(long)]
    pub reason: String,
    #[arg(long, default_value = "host-controller")]
    pub actor: String,
}

#[derive(Debug, Args)]
pub struct StateSealArgs {
    pub run: String,
    pub subject: String,
    #[arg(long)]
    pub reason: String,
    #[arg(long, default_value = "host-controller")]
    pub actor: String,
}

#[derive(Debug, Args)]
pub struct StateReconcileArgs {
    pub run: String,
    pub subject: String,
    #[arg(long)]
    pub workspace: PathBuf,
}

#[derive(Debug, Args)]
pub struct StateExportArgs {
    pub run: String,
    #[arg(long)]
    pub output_dir: PathBuf,
    /// Copy restricted artifacts such as execution logs and returned reasoning.
    #[arg(long)]
    pub include_restricted: bool,
    /// Include event payloads. Payload digests and metadata are always exported.
    #[arg(long)]
    pub include_event_payloads: bool,
}

#[derive(Debug, Subcommand)]
pub enum StateProfileCommand {
    List,
    Install {
        #[arg(long)]
        file: PathBuf,
        #[arg(long)]
        replace: bool,
    },
}

#[derive(Debug, Subcommand)]
pub enum StateSchemaCommand {
    List,
    Install {
        name: String,
        #[arg(long)]
        file: PathBuf,
        #[arg(long)]
        replace: bool,
    },
}

#[derive(Debug, Subcommand)]
pub enum CatalogCommand {
    Add(CatalogAddArgs),
    List,
    Verify { name: Option<String> },
    Remove { name: String },
}

#[derive(Debug, Args)]
pub struct CatalogAddArgs {
    pub name: String,
    #[arg(long)]
    pub kind: String,
    #[arg(long)]
    pub path: PathBuf,
    #[arg(long)]
    pub entrypoint: Option<String>,
    #[arg(long)]
    pub source_sandbox: Option<String>,
    #[arg(long)]
    pub team: Option<String>,
}

#[derive(Debug, Subcommand)]
pub enum AuditCommand {
    /// Verify the host hash chain and private file modes.
    Verify,
    /// Show recent host audit events.
    Tail {
        #[arg(long, default_value_t = 50)]
        count: usize,
    },
    /// Capture sandbox policy, settings, OCSF logs, packages, and processes.
    Snapshot { sandbox: String },
    /// Test blocked host and private-subnet destinations from one sandbox.
    Isolation { sandbox: String },
}

#[derive(Debug, Subcommand)]
pub enum PkiCommand {
    Init,
    Issue { name: String },
    List,
    Revoke { name: String },
}

#[derive(Debug, Args)]
pub struct DashboardArgs {
    /// Override the configured loopback bind for this run.
    #[arg(long)]
    pub bind: Option<String>,
}

#[derive(Debug, Subcommand)]
pub enum InternalCommand {
    Forward { lease_id: String },
    Ingress { lease_id: String },
    Peer { lease_id: String },
}
