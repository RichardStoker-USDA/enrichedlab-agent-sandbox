use std::{
    collections::BTreeMap,
    fs,
    io::Read,
    path::{Component, Path, PathBuf},
};

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use serde_json::json;
use sha2::{Digest, Sha256};
use time::{OffsetDateTime, format_description::well_known::Rfc3339};
use walkdir::WalkDir;

use crate::{
    audit,
    config::{RuntimeConfig, write_private},
    state, team,
};

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct CatalogIndex {
    pub schema_version: u32,
    pub entries: BTreeMap<String, CatalogEntry>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct CatalogEntry {
    pub name: String,
    pub kind: String,
    pub path: PathBuf,
    pub entrypoint: Option<String>,
    pub sha256: String,
    pub file_count: u64,
    pub source_sandbox: Option<String>,
    pub team: Option<String>,
    pub registered_at: String,
    pub verified_at: String,
}

impl Default for CatalogIndex {
    fn default() -> Self {
        Self {
            schema_version: 1,
            entries: BTreeMap::new(),
        }
    }
}

pub fn add(
    config: &RuntimeConfig,
    name: &str,
    kind: &str,
    path: &Path,
    entrypoint: Option<&str>,
    source_sandbox: Option<&str>,
    team_name: Option<&str>,
) -> Result<CatalogEntry> {
    validate_name(name)?;
    validate_kind(kind)?;
    if let Some(entrypoint) = entrypoint {
        validate_entrypoint(entrypoint)?;
    }
    if !path.is_absolute() {
        bail!("catalog path must be absolute");
    }
    let path = fs::canonicalize(path)?;
    let root = fs::canonicalize(&config.root)?;
    if !path.starts_with(&root) {
        bail!("catalog artifacts must stay under {}", root.display());
    }
    if let Some(entrypoint) = entrypoint {
        validate_entrypoint_path(&path, entrypoint)?;
    }
    if let Some(sandbox) = source_sandbox {
        state::load(config, sandbox)?;
    }
    if let Some(team_name) = team_name {
        team::load(config, team_name)?;
    }
    let mut index = load(config)?;
    if index.entries.contains_key(name) {
        bail!("catalog entry {name} already exists");
    }
    let (sha256, file_count) = hash_path(&path)?;
    let observed = OffsetDateTime::now_utc().format(&Rfc3339)?;
    let entry = CatalogEntry {
        name: name.into(),
        kind: kind.into(),
        path,
        entrypoint: entrypoint.map(str::to_string),
        sha256,
        file_count,
        source_sandbox: source_sandbox.map(str::to_string),
        team: team_name.map(str::to_string),
        registered_at: observed.clone(),
        verified_at: observed,
    };
    index.entries.insert(name.into(), entry.clone());
    save(config, &index)?;
    audit::append(
        config,
        "host-controller",
        "catalog.add",
        &format!("catalog/{name}"),
        "passed",
        json!({"kind": kind, "sha256": entry.sha256, "file_count": entry.file_count, "source_sandbox": source_sandbox, "team": team_name}),
    )?;
    Ok(entry)
}

pub fn verify(config: &RuntimeConfig, name: Option<&str>) -> Result<Vec<CatalogEntry>> {
    let mut index = load(config)?;
    let names: Vec<String> = match name {
        Some(name) => vec![name.into()],
        None => index.entries.keys().cloned().collect(),
    };
    let mut verified = Vec::new();
    for name in names {
        let entry = index
            .entries
            .get_mut(&name)
            .with_context(|| format!("unknown catalog entry {name}"))?;
        let (sha256, file_count) = hash_path(&entry.path)?;
        if sha256 != entry.sha256 || file_count != entry.file_count {
            audit::append(
                config,
                "host-controller",
                "catalog.verify",
                &format!("catalog/{name}"),
                "failed",
                json!({"expected_sha256": entry.sha256, "observed_sha256": sha256, "expected_file_count": entry.file_count, "observed_file_count": file_count}),
            )?;
            bail!("catalog entry {name} no longer matches its registered digest");
        }
        entry.verified_at = OffsetDateTime::now_utc().format(&Rfc3339)?;
        verified.push(entry.clone());
    }
    save(config, &index)?;
    for entry in &verified {
        audit::append(
            config,
            "host-controller",
            "catalog.verify",
            &format!("catalog/{}", entry.name),
            "passed",
            json!({"sha256": entry.sha256, "file_count": entry.file_count}),
        )?;
    }
    Ok(verified)
}

pub fn remove(config: &RuntimeConfig, name: &str) -> Result<CatalogEntry> {
    validate_name(name)?;
    let mut index = load(config)?;
    let entry = index
        .entries
        .remove(name)
        .with_context(|| format!("unknown catalog entry {name}"))?;
    save(config, &index)?;
    audit::append(
        config,
        "host-controller",
        "catalog.remove",
        &format!("catalog/{name}"),
        "passed",
        json!({"artifact_deleted": false}),
    )?;
    Ok(entry)
}

pub fn list(config: &RuntimeConfig) -> Result<Vec<CatalogEntry>> {
    Ok(load(config)?.entries.into_values().collect())
}

fn load(config: &RuntimeConfig) -> Result<CatalogIndex> {
    let path = index_path(config);
    if !path.exists() {
        return Ok(CatalogIndex::default());
    }
    let index: CatalogIndex = serde_json::from_slice(&fs::read(&path)?)?;
    if index.schema_version != 1 {
        bail!("unsupported catalog schema");
    }
    Ok(index)
}

fn save(config: &RuntimeConfig, index: &CatalogIndex) -> Result<()> {
    let mut body = serde_json::to_vec_pretty(index)?;
    body.push(b'\n');
    write_private(&index_path(config), &body)
}

fn hash_path(path: &Path) -> Result<(String, u64)> {
    let metadata = fs::symlink_metadata(path)?;
    if metadata.file_type().is_symlink() {
        bail!("catalog roots cannot be symbolic links");
    }
    let mut hasher = Sha256::new();
    let mut count = 0_u64;
    if metadata.is_file() {
        hash_file(&mut hasher, path, Path::new("."))?;
        return Ok((hex::encode(hasher.finalize()), 1));
    }
    if !metadata.is_dir() {
        bail!("catalog path must be a regular file or directory");
    }
    let mut entries = WalkDir::new(path)
        .follow_links(false)
        .into_iter()
        .collect::<std::result::Result<Vec<_>, _>>()?;
    entries.sort_by(|left, right| left.path().cmp(right.path()));
    if entries.len() > 100_000 {
        bail!("catalog artifact exceeds the 100000-entry safety limit");
    }
    for entry in entries {
        let metadata = fs::symlink_metadata(entry.path())?;
        let relative = entry.path().strip_prefix(path)?;
        if metadata.file_type().is_symlink() {
            bail!(
                "catalog artifacts cannot contain symbolic links: {}",
                relative.display()
            );
        }
        if metadata.is_dir() {
            hasher.update(b"dir\0");
            hasher.update(relative.as_os_str().as_encoded_bytes());
            hasher.update(b"\0");
        } else if metadata.is_file() {
            hash_file(&mut hasher, entry.path(), relative)?;
            count += 1;
        } else {
            bail!(
                "catalog artifact contains a non-regular entry: {}",
                relative.display()
            );
        }
    }
    Ok((hex::encode(hasher.finalize()), count))
}

fn hash_file(hasher: &mut Sha256, path: &Path, relative: &Path) -> Result<()> {
    hasher.update(b"file\0");
    hasher.update(relative.as_os_str().as_encoded_bytes());
    hasher.update(b"\0");
    let mut file = fs::File::open(path)?;
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        let read = file.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    hasher.update(b"\0");
    Ok(())
}

fn validate_name(value: &str) -> Result<()> {
    if value.is_empty()
        || value.len() > 64
        || value.starts_with('-')
        || value.ends_with('-')
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
    {
        bail!("catalog name must be a lowercase DNS label");
    }
    Ok(())
}

fn validate_kind(value: &str) -> Result<()> {
    if value.is_empty()
        || value.len() > 64
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
    {
        bail!("catalog kind must be a lowercase identifier");
    }
    Ok(())
}

fn validate_entrypoint(value: &str) -> Result<()> {
    let path = Path::new(value);
    if value.is_empty()
        || value.len() > 512
        || path.is_absolute()
        || value.contains(['\n', '\r', '\0'])
        || path.components().any(|component| {
            matches!(
                component,
                Component::ParentDir | Component::RootDir | Component::Prefix(_)
            )
        })
    {
        bail!("catalog entrypoint must be a literal relative path");
    }
    Ok(())
}

fn validate_entrypoint_path(root: &Path, value: &str) -> Result<()> {
    validate_entrypoint(value)?;
    let root = fs::canonicalize(root)?;
    let candidate = root.join(value);
    let metadata = fs::symlink_metadata(&candidate)
        .with_context(|| format!("catalog entrypoint does not exist: {value}"))?;
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        bail!("catalog entrypoint must be a regular file: {value}");
    }
    let candidate = fs::canonicalize(&candidate)?;
    if !candidate.starts_with(&root) {
        bail!("catalog entrypoint escapes the artifact root");
    }
    Ok(())
}

fn index_path(config: &RuntimeConfig) -> PathBuf {
    config.catalog_dir().join("index.json")
}

#[cfg(test)]
mod tests {
    use std::fs;

    use tempfile::tempdir;

    use super::{hash_path, validate_entrypoint_path};

    #[test]
    fn directory_digest_changes_with_content() {
        let temp = tempdir().unwrap();
        let path = temp.path().join("artifact");
        fs::create_dir(&path).unwrap();
        fs::write(path.join("result.txt"), "one").unwrap();
        let first = hash_path(&path).unwrap();
        fs::write(path.join("result.txt"), "two").unwrap();
        let second = hash_path(&path).unwrap();
        assert_ne!(first.0, second.0);
        assert_eq!(first.1, 1);
    }

    #[test]
    fn entrypoint_must_be_an_existing_regular_file() {
        let temp = tempdir().unwrap();
        fs::write(temp.path().join("start"), "run").unwrap();
        assert!(validate_entrypoint_path(temp.path(), "start").is_ok());
        assert!(validate_entrypoint_path(temp.path(), "missing").is_err());
        assert!(validate_entrypoint_path(temp.path(), ".").is_err());
    }
}
