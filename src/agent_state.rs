use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    io::Read,
    path::{Component, Path, PathBuf},
};

use anyhow::{Context, Result, bail};

use clap::ValueEnum;

use rusqlite::{Connection, OptionalExtension, Transaction, TransactionBehavior, params};

use serde::{Deserialize, Serialize};

use serde_json::{Value, json};

use sha2::{Digest, Sha256};

use time::{OffsetDateTime, format_description::well_known::Rfc3339};

use uuid::Uuid;

use walkdir::WalkDir;

use crate::{
    audit,
    config::{RuntimeConfig, create_private_dir, set_mode, write_private},
};

mod collections;
mod database;
mod inspection;
mod model;
mod pointers;
mod profiles;
mod ro_crate;
mod runs;
mod seeds;
mod support;
#[cfg(test)]
mod tests;
mod verification;
mod views;

pub use collections::*;
use database::*;
pub use inspection::*;
pub use model::*;
use pointers::*;
pub use profiles::*;
pub use ro_crate::*;
pub use runs::*;
pub use seeds::*;
use support::*;
pub use verification::*;
pub use views::*;

const MAX_STATE_BYTES: usize = 4 * 1024 * 1024;

const MAX_MUTATION_BYTES: usize = 1024 * 1024;

const MAX_EVENT_BYTES: usize = 2 * 1024 * 1024;

const MAX_ARTIFACT_BYTES: u64 = 4 * 1024 * 1024 * 1024;

const MAX_POINTERS: usize = 4096;

const MAX_METADATA_SEARCH_WINDOW: usize = 10_000;

const EXECUTION_SCHEMA: &str = include_str!("../schemas/agent-state/execution.schema.json");

const KNOWLEDGE_SCHEMA: &str = include_str!("../schemas/agent-state/knowledge.schema.json");

const SCIENTIFIC_SCHEMA: &str = include_str!("../schemas/agent-state/scientific.schema.json");

const CLUSTER_OPS_SCHEMA: &str = include_str!("../schemas/agent-state/cluster-ops.schema.json");

const COORDINATION_SCHEMA: &str = include_str!("../schemas/agent-state/coordination.schema.json");

const SCIENTIFIC_WORKER_PROFILE: &str = include_str!("../profiles/state/scientific-worker.json");

const RESEARCH_LEAD_PROFILE: &str = include_str!("../profiles/state/research-lead.json");

const CLUSTER_OPERATOR_PROFILE: &str = include_str!("../profiles/state/cluster-operator.json");

const SOFTWARE_ENGINEER_PROFILE: &str = include_str!("../profiles/state/software-engineer.json");
