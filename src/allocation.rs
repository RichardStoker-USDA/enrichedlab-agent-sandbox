use std::{
    fs::{self, OpenOptions},
    io::{ErrorKind, Read, Write},
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};

use anyhow::{Context, Result, bail};
use fs2::FileExt;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use uuid::Uuid;

use crate::{
    config::{RuntimeConfig, create_private_dir, set_mode},
    openshell, state,
};

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct ReservationRecord {
    schema_version: u32,
    token: String,
    route: String,
    sandbox: String,
    gateway: String,
    created_at_unix: u64,
}

#[derive(Clone, Debug, Serialize)]
pub struct AllocationReservationSummary {
    pub route: String,
    pub sandbox: String,
    pub status: String,
    pub created_at_unix: u64,
}

pub struct RouteAllocationReservation {
    file: fs::File,
    path: PathBuf,
}

impl Drop for RouteAllocationReservation {
    fn drop(&mut self) {
        let _ = self.file.unlock();
        let _ = fs::remove_file(&self.path);
    }
}

struct AllocationLock {
    file: fs::File,
}

impl Drop for AllocationLock {
    fn drop(&mut self) {
        let _ = self.file.unlock();
    }
}

pub fn reserve(
    config: &RuntimeConfig,
    route: &str,
    sandbox: &str,
    route_cap: usize,
    gateway: &str,
) -> Result<RouteAllocationReservation> {
    validate_label(route, "route")?;
    state::validate_name(sandbox)?;
    validate_label(gateway, "gateway")?;
    if !(1..=64).contains(&route_cap) {
        bail!("route allocation cap must be between one and 64");
    }
    create_private_dir(&config.allocation_reservations_dir())?;
    let _guard = allocation_lock(config)?;
    let managed_records = state::list(config)?;
    let managed = managed_records
        .iter()
        .filter(|record| record.route == route)
        .count();
    let reserved = active_reservations(config, route, &managed_records)?;
    if managed.saturating_add(reserved) >= route_cap {
        bail!("route {route} is at its configured concurrency cap of {route_cap} sandboxes");
    }

    let token = Uuid::new_v4().simple().to_string();
    let path = config
        .allocation_reservations_dir()
        .join(format!("{token}.json"));
    let mut options = OpenOptions::new();
    options.create_new(true).read(true).write(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options
        .open(&path)
        .with_context(|| format!("failed to create route reservation {}", path.display()))?;
    file.lock_exclusive()?;
    let mut body = serde_json::to_vec_pretty(&ReservationRecord {
        schema_version: 1,
        token,
        route: route.into(),
        sandbox: sandbox.into(),
        gateway: gateway.into(),
        created_at_unix: unix_now()?,
    })?;
    body.push(b'\n');
    file.write_all(&body)?;
    file.sync_all()?;
    set_mode(&path, 0o600)?;
    Ok(RouteAllocationReservation { file, path })
}

pub fn summaries(config: &RuntimeConfig) -> Result<Vec<AllocationReservationSummary>> {
    create_private_dir(&config.allocation_reservations_dir())?;
    let _guard = allocation_lock(config)?;
    let managed_records = state::list(config)?;
    let mut summaries = Vec::new();
    for entry in fs::read_dir(config.allocation_reservations_dir())? {
        let path = entry?.path();
        if path.extension().and_then(|value| value.to_str()) != Some("json") {
            continue;
        }
        let mut options = OpenOptions::new();
        options.read(true).write(true);
        let mut file = options
            .open(&path)
            .with_context(|| format!("failed to open route reservation {}", path.display()))?;
        let active = match file.try_lock_exclusive() {
            Ok(()) => false,
            Err(error) if error.kind() == ErrorKind::WouldBlock => true,
            Err(error) => {
                return Err(error).with_context(|| {
                    format!("failed to inspect route reservation {}", path.display())
                });
            }
        };
        let mut body = Vec::new();
        file.read_to_end(&mut body)?;
        let record: ReservationRecord = serde_json::from_slice(&body)
            .with_context(|| format!("invalid route reservation {}", path.display()))?;
        validate_record(&record, &path)?;
        let committed = managed_records
            .iter()
            .any(|item| item.name == record.sandbox && item.route == record.route);
        let status = if committed {
            "committed-cleanup-pending"
        } else if active {
            "allocating"
        } else {
            match openshell::live_sandboxes(&config.openshell_bin, &record.gateway) {
                Ok(live) if live_contains(&live, &record.sandbox) => "unowned-live",
                Ok(_) => "abandoned-no-runtime",
                Err(_) => "custody-unknown",
            }
        };
        summaries.push(AllocationReservationSummary {
            route: record.route,
            sandbox: record.sandbox,
            status: status.into(),
            created_at_unix: record.created_at_unix,
        });
    }
    summaries.sort_by(|left, right| left.sandbox.cmp(&right.sandbox));
    Ok(summaries)
}

fn active_reservations(
    config: &RuntimeConfig,
    route: &str,
    managed_records: &[state::SandboxRecord],
) -> Result<usize> {
    let mut active = 0_usize;
    for entry in fs::read_dir(config.allocation_reservations_dir())? {
        let path = entry?.path();
        if path.extension().and_then(|value| value.to_str()) != Some("json") {
            continue;
        }
        let mut options = OpenOptions::new();
        options.read(true).write(true);
        let mut file = options
            .open(&path)
            .with_context(|| format!("failed to open route reservation {}", path.display()))?;
        match file.try_lock_exclusive() {
            Ok(()) => {
                let mut body = Vec::new();
                file.read_to_end(&mut body)?;
                let record: ReservationRecord =
                    serde_json::from_slice(&body).with_context(|| {
                        format!("invalid abandoned route reservation {}", path.display())
                    })?;
                validate_record(&record, &path)?;
                if record.route != route {
                    continue;
                }
                let committed = managed_records
                    .iter()
                    .any(|item| item.name == record.sandbox && item.route == record.route);
                if committed {
                    drop(file);
                    remove_reservation(&path)?;
                    continue;
                }
                let live = openshell::live_sandboxes(&config.openshell_bin, &record.gateway)
                    .with_context(|| {
                        format!(
                            "cannot resolve abandoned allocation custody for sandbox {}",
                            record.sandbox
                        )
                    })?;
                if live_contains(&live, &record.sandbox) {
                    active = active.saturating_add(1);
                } else {
                    drop(file);
                    remove_reservation(&path)?;
                }
            }
            Err(error) if error.kind() == ErrorKind::WouldBlock => {
                let mut body = Vec::new();
                file.read_to_end(&mut body)?;
                let record: ReservationRecord =
                    serde_json::from_slice(&body).with_context(|| {
                        format!("invalid active route reservation {}", path.display())
                    })?;
                validate_record(&record, &path)?;
                let committed = managed_records
                    .iter()
                    .any(|item| item.name == record.sandbox && item.route == record.route);
                if record.route == route && !committed {
                    active = active.saturating_add(1);
                }
            }
            Err(error) => {
                return Err(error).with_context(|| {
                    format!("failed to inspect route reservation {}", path.display())
                });
            }
        }
    }
    Ok(active)
}

fn allocation_lock(config: &RuntimeConfig) -> Result<AllocationLock> {
    let path = config.allocation_reservations_dir().join("allocation.lock");
    let mut options = OpenOptions::new();
    options.create(true).read(true).write(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let file = options
        .open(&path)
        .with_context(|| format!("failed to open route allocation lock {}", path.display()))?;
    file.lock_exclusive()?;
    set_mode(&path, 0o600)?;
    Ok(AllocationLock { file })
}

fn validate_record(record: &ReservationRecord, path: &Path) -> Result<()> {
    if record.schema_version != 1
        || path.file_stem().and_then(|value| value.to_str()) != Some(&record.token)
    {
        bail!("route reservation identity does not match its file");
    }
    validate_label(&record.token, "reservation token")?;
    validate_label(&record.route, "route")?;
    validate_label(&record.gateway, "gateway")?;
    state::validate_name(&record.sandbox)?;
    if record.created_at_unix > unix_now()?.saturating_add(300) {
        bail!("route reservation timestamp is in the future");
    }
    Ok(())
}

fn live_contains(value: &Value, sandbox: &str) -> bool {
    value.as_array().is_some_and(|items| {
        items
            .iter()
            .any(|item| item.get("name").and_then(Value::as_str) == Some(sandbox))
    })
}

fn remove_reservation(path: &Path) -> Result<()> {
    fs::remove_file(path)
        .with_context(|| format!("failed to remove abandoned reservation {}", path.display()))
}

fn validate_label(value: &str, label: &str) -> Result<()> {
    if value.is_empty()
        || value.len() > 128
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
    {
        bail!("{label} must use bounded ASCII letters, digits, hyphens, or underscores");
    }
    Ok(())
}

fn unix_now() -> Result<u64> {
    Ok(SystemTime::now().duration_since(UNIX_EPOCH)?.as_secs())
}

#[cfg(test)]
mod tests {
    use tempfile::tempdir;

    use super::{reserve, summaries};
    use crate::config::{RuntimeConfig, create_private_dir};

    #[test]
    fn active_reservations_enforce_cap_and_release_on_drop() {
        let temp = tempdir().unwrap();
        let config = RuntimeConfig {
            root: temp.path().join("root"),
            state_dir: temp.path().join("state"),
            config_dir: temp.path().join("config"),
            openshell_bin: "/bin/true".into(),
            docker_bin: "/bin/true".into(),
        };
        for path in [
            &config.root,
            &config.state_dir,
            &config.config_dir,
            &config.records_dir(),
            &config.allocation_reservations_dir(),
        ] {
            create_private_dir(path).unwrap();
        }
        let first = reserve(&config, "local-model", "agent-one", 1, "docker-local").unwrap();
        let visible = summaries(&config).unwrap();
        assert_eq!(visible.len(), 1);
        assert_eq!(visible[0].status, "allocating");
        assert!(reserve(&config, "local-model", "agent-two", 1, "docker-local").is_err());
        drop(first);
        assert!(reserve(&config, "local-model", "agent-two", 1, "docker-local").is_ok());
    }
}
