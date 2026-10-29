//! Site-owned research starting points. Selection does not grant data or tool access.
use std::{
    collections::HashSet,
    fs::File,
    io::Read,
    os::unix::fs::{MetadataExt, OpenOptionsExt},
    path::Path,
};

use anyhow::{Result, bail};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use super::{
    model::MeView,
    project::{MAX_STARTING_POINTS, ResearchState, StartingPoint, WorkspaceSpec, action_key},
};

#[derive(Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorkspaceCatalog {
    schema_version: u32,
    entries: Vec<WorkspaceEntry>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorkspaceEntry {
    pub key: String,
    pub name: String,
    pub description: String,
    pub group: String,
    /// None permits current members of the named group, subject to tool permissions.
    pub principals: Option<Vec<Uuid>>,
    pub allowed_profiles: Vec<String>,
    pub workspace: WorkspaceSpec,
}

#[derive(Serialize)]
pub struct WorkspaceChoice<'a> {
    pub key: &'a str,
    pub name: &'a str,
    pub description: &'a str,
    pub input_refs: &'a [String],
    pub tool_refs: &'a [String],
    pub allowed_profiles: &'a [String],
}

impl WorkspaceCatalog {
    pub fn load(path: &Path) -> Result<Self> {
        if !path.is_absolute() {
            bail!("workspace_catalog_path_invalid");
        }
        let file = File::options()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
            .open(path)
            .map_err(|_| anyhow::anyhow!("workspace_catalog_unavailable"))?;
        let m = file.metadata()?;
        if !m.is_file()
            || m.uid() != unsafe { libc::geteuid() }
            || m.mode() & 0o077 != 0
            || m.nlink() != 1
            || m.len() > 1024 * 1024
        {
            bail!("workspace_catalog_file_invalid");
        }
        let mut bytes = Vec::new();
        file.take(1024 * 1024 + 1).read_to_end(&mut bytes)?;
        Self::parse(&bytes)
    }

    pub fn parse(bytes: &[u8]) -> Result<Self> {
        if bytes.len() > 1024 * 1024 {
            bail!("workspace_catalog_invalid");
        }
        let catalog: Self = serde_json::from_slice(bytes)
            .map_err(|_| anyhow::anyhow!("workspace_catalog_invalid"))?;
        let mut keys = HashSet::new();
        if catalog.schema_version != 1 || catalog.entries.len() > 256 {
            bail!("workspace_catalog_invalid");
        }
        for entry in &catalog.entries {
            if !action_key(&entry.key)
                || !keys.insert(&entry.key)
                || !action_key(&entry.group)
                || entry.name.trim().is_empty()
                || entry.name.len() > 160
                || entry.name.contains('\0')
                || entry.description.trim().is_empty()
                || entry.description.len() > 2048
                || entry.description.contains('\0')
                || entry.allowed_profiles.is_empty()
                || entry.allowed_profiles.len() > 32
                || entry.allowed_profiles.iter().any(|p| !action_key(p))
                || entry.allowed_profiles.iter().collect::<HashSet<_>>().len()
                    != entry.allowed_profiles.len()
                || entry
                    .principals
                    .as_ref()
                    .is_some_and(|p| p.is_empty() || p.len() > 1024)
                || entry.workspace.input_refs.is_empty()
                || entry.workspace.tool_refs.is_empty()
                || entry.workspace.preparation_receipt.is_some()
                || !entry.workspace.starting_points.is_empty()
                || entry
                    .initial_state("Validate the starting point".into(), vec![])
                    .validate()
                    .is_err()
            {
                bail!("workspace_catalog_invalid");
            }
        }
        Ok(catalog)
    }

    pub fn available<'a>(&'a self, group: &str, identity: &MeView) -> Vec<&'a WorkspaceEntry> {
        self.entries
            .iter()
            .filter(|entry| {
                entry.group == group
                    && identity.groups.iter().any(|g| g.key == group)
                    && entry
                        .principals
                        .as_ref()
                        .is_none_or(|ids| ids.contains(&identity.principal.id))
                    && entry
                        .allowed_profiles
                        .iter()
                        .all(|key| identity.profiles.iter().any(|p| &p.key == key))
            })
            .collect()
    }
}

/// Why several sets cannot start one project.
#[derive(Debug, PartialEq, Eq)]
pub enum CombineRefusal {
    /// Fewer than two sets, more than `MAX_STARTING_POINTS`, or one named twice.
    Invalid,
    /// The sets need different workspace images; the keys of every set chosen.
    Unsupported(Vec<String>),
    /// Their union holds more references than one workspace may.
    TooLarge,
}

/// One workspace from several catalog entries, in the order chosen: the first entry's profile and
/// digests, the union of their data and tool references, the union of their methods, and each entry as a starting
/// point. Entries must share one workspace image. Choosing sets grants nothing: each method still needs its own grant.
pub fn combine(
    entries: &[&WorkspaceEntry],
) -> std::result::Result<(WorkspaceSpec, Vec<String>), CombineRefusal> {
    let mut keys = HashSet::new();
    if !(2..=MAX_STARTING_POINTS).contains(&entries.len())
        || !entries.iter().all(|e| keys.insert(e.key.as_str()))
    {
        return Err(CombineRefusal::Invalid);
    }
    let first = &entries[0].workspace;
    if entries
        .iter()
        .any(|e| e.workspace.image_digest != first.image_digest)
    {
        return Err(CombineRefusal::Unsupported(
            entries.iter().map(|e| e.key.clone()).collect(),
        ));
    }
    let union = |pick: fn(&WorkspaceSpec) -> &Vec<String>| {
        let mut all: Vec<String> = Vec::new();
        for entry in entries {
            for value in pick(&entry.workspace) {
                if !all.contains(value) {
                    all.push(value.clone());
                }
            }
        }
        all
    };
    let input_refs = union(|w| &w.input_refs);
    let tool_refs = union(|w| &w.tool_refs);
    if input_refs.len() > 64 || tool_refs.len() > 64 {
        return Err(CombineRefusal::TooLarge);
    }
    let mut profiles: Vec<String> = Vec::new();
    for entry in entries {
        for key in &entry.allowed_profiles {
            if !profiles.contains(key) {
                profiles.push(key.clone());
            }
        }
    }
    let workspace = WorkspaceSpec {
        schema_version: 1,
        profile: first.profile.clone(),
        image_digest: first.image_digest.clone(),
        source_digest: first.source_digest.clone(),
        input_refs,
        tool_refs,
        preparation_receipt: None,
        starting_points: entries
            .iter()
            .map(|e| StartingPoint {
                key: e.key.clone(),
                name: e.name.clone(),
                description: e.description.clone(),
                profile: e.workspace.profile.clone(),
                source_digest: e.workspace.source_digest.clone(),
                input_refs: e.workspace.input_refs.clone(),
                tool_refs: e.workspace.tool_refs.clone(),
            })
            .collect(),
    };
    Ok((workspace, profiles))
}

/// The research state a project starts with, from one workspace.
pub fn initial_state(
    workspace: WorkspaceSpec,
    goal: String,
    constraints: Vec<String>,
) -> ResearchState {
    ResearchState {
        schema_version: 1,
        goal,
        constraints,
        plan: vec![],
        lessons: vec![],
        next_action: "Plan the requested analysis using the selected data and approved methods."
            .into(),
        workspace,
    }
}

impl WorkspaceEntry {
    pub fn choice(&self) -> WorkspaceChoice<'_> {
        WorkspaceChoice {
            key: &self.key,
            name: &self.name,
            description: &self.description,
            input_refs: &self.workspace.input_refs,
            tool_refs: &self.workspace.tool_refs,
            allowed_profiles: &self.allowed_profiles,
        }
    }

    pub fn initial_state(&self, goal: String, constraints: Vec<String>) -> ResearchState {
        initial_state(self.workspace.clone(), goal, constraints)
    }
}
