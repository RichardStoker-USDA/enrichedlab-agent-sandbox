use std::{
    collections::BTreeMap,
    ffi::OsString,
    fs,
    io::{self, BufRead, BufReader},
    path::{Path, PathBuf},
    process::Command as ProcessCommand,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    thread,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use anyhow::{Context, Result, bail};

use serde::Serialize;

use serde_json::{Value, json};

use sha2::{Digest, Sha256};

use uuid::Uuid;

use walkdir::WalkDir;

use crate::{
    agent_continuity, agent_state, allocation, audit, catalog,
    cli::{
        AgentStateCommand, AllowArgs, AuditCommand, BootstrapArgs, CatalogCommand, Cli, Command,
        ContinuityAgentCommand, ContinuityAgentCreateArgs, ContinuityAgentRunArgs, DeleteArgs,
        DenyArgs, ExecArgs, NameArgs, OutputFormat, PkiCommand, PlatformCommand, PoolArgs,
        ProposalCommand, ProvenanceCommand, ProvenanceIngestArgs, RequestCommand, ResearchCommand,
        RunArgs, RuntimeArgs, ServiceCommand, StateProfileCommand, StateSchemaCommand, SyncCommand,
        TeamCommand,
    },
    config::{RuntimeConfig, create_private_dir, write_private},
    dashboard, evidence,
    harness::{CanonicalHarnessEvent, EvidenceVisibility, HarnessEventAdapter, HarnessKind},
    lease, openshell, pki, platform, policy, provenance, request,
    research::{self, ResearchWorker, ResearchWorkerState, SnapshotEntry, SnapshotManifest},
    routes::RouteCatalog,
    state::{self, SandboxRecord},
    team, workspace, workspace_sync,
};

mod access_requests;
mod continuity_agents;
mod continuity_evidence;
mod maintenance;
mod output;
mod platform_commands;
mod provenance_capture;
mod research_runs;
mod sandboxes;
mod state_commands;
mod team_commands;
#[cfg(test)]
mod tests;

use access_requests::*;
use continuity_agents::*;
use continuity_evidence::*;
use maintenance::*;
use output::*;
use platform_commands::*;
use provenance_capture::*;
use research_runs::*;
pub(crate) use sandboxes::*;
use state_commands::*;
use team_commands::*;

const OPENCODE_VERSION: &str = "1.18.25";

const OPENCODE_BIN: &str = "/sandbox/.local/opencode/node_modules/.bin/opencode";

const PI_VERSION: &str = "0.84.4";

const PI_BIN: &str = "/sandbox/.local/pi/node_modules/.bin/pi";

const MAX_DIRECT_RUN_CAPTURE_BYTES: usize = 8 * 1024 * 1024;

fn hash_bytes(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}

fn hash_json<T: Serialize>(value: &T) -> Result<String> {
    Ok(hash_bytes(&serde_json::to_vec(value)?))
}

pub fn run(cli: Cli) -> Result<()> {
    let _ = rustls::crypto::ring::default_provider().install_default();
    let output = cli.output;
    let config = RuntimeConfig::resolve(&cli)?;
    match cli.command {
        Command::Create(args) => {
            let record = create_one(&config, args.name, args.workspace, &args.runtime)?;
            print_records(output, &[record]);
        }
        Command::Pool(args) => create_pool(&config, output, args)?,
        Command::List => list(&config, output)?,
        Command::Exec(args) => exec(&config, args)?,
        Command::Bootstrap(args) => bootstrap(&config, args)?,
        Command::Run(args) => run_agent(&config, output, args)?,
        Command::Shell(args) => shell(&config, args)?,
        Command::Stop(args) => lifecycle(&config, output, args, "stop")?,
        Command::Start(args) => lifecycle(&config, output, args, "start")?,
        Command::Delete(args) => delete(&config, output, args)?,
        Command::Allow(args) => allow(&config, output, args)?,
        Command::Deny(args) => deny(&config, output, args)?,
        Command::Service { command } => service(&config, output, command)?,
        Command::Request { command } => request_command(&config, output, command)?,
        Command::Proposal { command } => proposal(&config, output, command)?,
        Command::Team { command } => team_command(&config, output, command)?,
        Command::Research { command } => research_command(&config, output, command)?,
        Command::Agent { command } => continuity_agent_command(&config, output, command)?,
        Command::State { command } => agent_state_command(&config, output, command)?,
        Command::Provenance { command } => provenance_command(&config, output, command)?,
        Command::Sync { command } => sync_command(&config, output, command)?,
        Command::Platform { command } => platform_command(&config, output, command)?,
        Command::Catalog { command } => catalog_command(&config, output, command)?,
        Command::Audit { command } => audit_command(&config, output, command)?,
        Command::Pki { command } => pki_command(&config, output, command)?,
        Command::Dashboard(args) => run_async(dashboard::serve(config, args.bind.as_deref()))?,
        Command::MigrateLegacy => migrate_legacy(&config, output)?,
        Command::Doctor => doctor(&config, output)?,
        Command::Internal { command } => {
            let id = match command {
                crate::cli::InternalCommand::Forward { lease_id }
                | crate::cli::InternalCommand::Ingress { lease_id }
                | crate::cli::InternalCommand::Peer { lease_id } => lease_id,
            };
            run_async(lease::run_internal(config, &id))?;
        }
    }
    Ok(())
}
