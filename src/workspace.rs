use std::{
    collections::HashSet,
    fs,
    path::{Component, Path, PathBuf},
};

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};

use crate::config::{create_private_dir, set_mode, write_private};

pub const PRIMARY_TARGET: &str = "/sandbox/work";
pub const REQUESTS_TARGET: &str = "/sandbox/control/requests";
pub const DECISIONS_TARGET: &str = "/sandbox/control/decisions";
pub const RUNTIME_TARGET: &str = "/sandbox/control/runtime";
pub const PEER_PROXY_TARGET: &str = "/sandbox/control/runtime/peer_proxy.py";
const PEER_PROXY: &str = include_str!("../runtime/peer_proxy.py");

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct MountRecord {
    pub host_path: PathBuf,
    pub container_path: String,
    pub read_only: bool,
}

#[derive(Debug)]
pub struct MaterializedWorkspace {
    pub path: PathBuf,
    pub managed: bool,
}

pub fn materialize_primary(
    root: &Path,
    requested: Option<&Path>,
    name: &str,
) -> Result<MaterializedWorkspace> {
    let (candidate, managed) = match requested {
        Some(path) => (path.to_path_buf(), false),
        None => (root.join(name), true),
    };
    if !candidate.is_absolute() {
        bail!("workspace path must be absolute: {}", candidate.display());
    }
    create_private_dir(&candidate)?;
    let canonical = fs::canonicalize(&candidate)
        .with_context(|| format!("failed to resolve {}", candidate.display()))?;
    validate_host_path(&canonical)?;
    Ok(MaterializedWorkspace {
        path: canonical,
        managed,
    })
}

pub fn materialize_extra_mounts(
    read_only: &[PathBuf],
    read_write: &[PathBuf],
    primary: &Path,
) -> Result<Vec<MountRecord>> {
    let mut mounts = Vec::new();
    let mut targets = HashSet::new();

    for (paths, is_read_only) in [(read_only, true), (read_write, false)] {
        for path in paths {
            if !path.is_absolute() {
                bail!("mount path must be absolute: {}", path.display());
            }
            let canonical = fs::canonicalize(path)
                .with_context(|| format!("failed to resolve mount {}", path.display()))?;
            validate_host_path(&canonical)?;
            if canonical == primary {
                bail!(
                    "extra mount duplicates the primary workspace: {}",
                    path.display()
                );
            }
            if !canonical.is_dir() {
                bail!("mount source is not a directory: {}", canonical.display());
            }
            let basename = canonical
                .file_name()
                .and_then(|value| value.to_str())
                .context("mount source has no UTF-8 directory name")?;
            let safe_name = sanitize_target_name(basename);
            if safe_name.is_empty() {
                bail!(
                    "mount source has no usable directory name: {}",
                    canonical.display()
                );
            }
            let target = format!("/sandbox/mounts/{safe_name}");
            if !targets.insert(target.clone()) {
                bail!("two extra mounts resolve to the same target: {target}");
            }
            mounts.push(MountRecord {
                host_path: canonical,
                container_path: target,
                read_only: is_read_only,
            });
        }
    }
    Ok(mounts)
}

pub fn materialize_control_mounts(
    requests: &Path,
    decisions: &Path,
    runtime: &Path,
) -> Result<Vec<MountRecord>> {
    create_private_dir(requests)?;
    create_private_dir(decisions)?;
    create_private_dir(runtime)?;
    let peer_proxy = runtime.join("peer_proxy.py");
    if peer_proxy.exists() {
        set_mode(&peer_proxy, 0o600)?;
    }
    write_private(&peer_proxy, PEER_PROXY.as_bytes())?;
    let requests = fs::canonicalize(requests)?;
    let decisions = fs::canonicalize(decisions)?;
    let runtime = fs::canonicalize(runtime)?;
    Ok(vec![
        MountRecord {
            host_path: requests,
            container_path: REQUESTS_TARGET.into(),
            read_only: false,
        },
        MountRecord {
            host_path: decisions,
            container_path: DECISIONS_TARGET.into(),
            read_only: true,
        },
        MountRecord {
            host_path: runtime,
            container_path: RUNTIME_TARGET.into(),
            read_only: true,
        },
    ])
}

pub fn validate_host_path(path: &Path) -> Result<()> {
    if !path.is_absolute() || path == Path::new("/") {
        bail!("refusing broad host path: {}", path.display());
    }

    let home = std::env::var_os("HOME").map(PathBuf::from);
    if home.as_deref() == Some(path) {
        bail!("refusing to mount the home directory");
    }

    const BLOCKED_EXACT: &[&str] = &[
        "/Users",
        "/System",
        "/Library",
        "/Applications",
        "/bin",
        "/dev",
        "/etc",
        "/private",
        "/proc",
        "/run",
        "/sbin",
        "/sys",
        "/usr",
        "/Volumes",
        "/var/run",
        "/opt/homebrew",
    ];
    if BLOCKED_EXACT
        .iter()
        .any(|blocked| path == Path::new(blocked))
    {
        bail!(
            "refusing broad or runtime-sensitive host path: {}",
            path.display()
        );
    }

    const BLOCKED_COMPONENTS: &[&str] = &[
        ".ssh",
        ".gnupg",
        ".aws",
        ".kube",
        ".docker",
        ".orbstack",
        ".secrets",
        ".codex",
        ".claude",
        ".pi",
        ".omp",
        ".config",
        ".git",
        "Library",
    ];
    for component in path.components() {
        if let Component::Normal(value) = component
            && let Some(value) = value.to_str()
            && BLOCKED_COMPONENTS.contains(&value)
        {
            bail!(
                "refusing credential or agent-state path: {}",
                path.display()
            );
        }
    }
    Ok(())
}

fn sanitize_target_name(value: &str) -> String {
    value
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || ch == '-' || ch == '_' {
                ch.to_ascii_lowercase()
            } else {
                '-'
            }
        })
        .collect::<String>()
        .trim_matches('-')
        .to_string()
}

#[cfg(test)]
mod tests {
    use std::fs;

    use tempfile::tempdir;

    use super::{
        DECISIONS_TARGET, REQUESTS_TARGET, RUNTIME_TARGET, materialize_control_mounts,
        materialize_primary, sanitize_target_name, validate_host_path,
    };

    #[test]
    fn generated_workspace_is_private_and_managed() {
        let temp = tempdir().unwrap();
        let result = materialize_primary(temp.path(), None, "agent-a1b2").unwrap();
        assert!(result.managed);
        assert!(result.path.ends_with("agent-a1b2"));
        assert!(fs::metadata(result.path).unwrap().is_dir());
    }

    #[test]
    fn sensitive_components_are_rejected() {
        assert!(validate_host_path(std::path::Path::new("/tmp/example-home/.ssh/work")).is_err());
        assert!(
            validate_host_path(std::path::Path::new("/tmp/example-home/.config/tool")).is_err()
        );
    }

    #[test]
    fn mount_target_names_are_normalized() {
        assert_eq!(sanitize_target_name("My Project"), "my-project");
    }

    #[test]
    fn control_requests_are_writable_and_decisions_are_read_only() {
        let temp = tempdir().unwrap();
        let mounts = materialize_control_mounts(
            &temp.path().join("requests"),
            &temp.path().join("decisions"),
            &temp.path().join("runtime"),
        )
        .unwrap();
        assert_eq!(mounts[0].container_path, REQUESTS_TARGET);
        assert!(!mounts[0].read_only);
        assert_eq!(mounts[1].container_path, DECISIONS_TARGET);
        assert!(mounts[1].read_only);
        assert_eq!(mounts[2].container_path, RUNTIME_TARGET);
        assert!(mounts[2].read_only);
        assert!(mounts[2].host_path.join("peer_proxy.py").is_file());
    }
}
