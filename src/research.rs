use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    io::Read,
    path::{Component, Path, PathBuf},
};

use anyhow::{Context, Result, bail};

use clap::ValueEnum;

use serde::{Deserialize, Serialize};

use serde_json::{Value, json};

use sha2::{Digest, Sha256};

use time::{OffsetDateTime, format_description::well_known::Rfc3339};

use uuid::Uuid;

use walkdir::WalkDir;

use crate::{
    agent_state, audit, catalog,
    cli::{RuntimeArgs, Thinking},
    config::{RuntimeConfig, create_private_dir, write_private},
    harness::{HarnessEventAdapter, HarnessKind},
    provenance::{self, ProvenanceContext},
    routes::RouteCatalog,
};

mod artifacts;
mod capture;
mod files;
mod lifecycle;
mod model;
mod observer;
mod support;
#[cfg(test)]
mod tests;
mod validation;
mod verification;
mod workers;

pub use artifacts::*;
pub use capture::*;
use files::*;
pub use lifecycle::*;
pub use model::*;
pub use observer::*;
use support::*;
use validation::*;
pub use verification::*;
pub use workers::*;

const MAX_TEXT_BYTES: usize = 1024 * 1024;

const MAX_SNAPSHOT_FILES: u64 = 100_000;

const MAX_SNAPSHOT_BYTES: u64 = 4 * 1024 * 1024 * 1024;

const MAX_CLAIMS: usize = 1_000;
