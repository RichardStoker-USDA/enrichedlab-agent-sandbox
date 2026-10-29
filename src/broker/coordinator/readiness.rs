//! Coordinator start readiness. A private atomic marker, owned by one process and cleared before checks and on exit.
use anyhow::{Context, Result, bail};
use std::{
    fs::{self, File, OpenOptions},
    io::Write,
    path::{Path, PathBuf},
};
use time::{OffsetDateTime, format_description::well_known::Rfc3339};
use uuid::Uuid;

pub(in crate::broker) struct ReadyFile {
    path: Option<PathBuf>,
    published: bool,
}

impl ReadyFile {
    pub(in crate::broker) fn prepare(path: Option<&Path>) -> Result<Self> {
        if let Some(path) = path {
            if !path.is_absolute()
                || path.file_name().is_none()
                || !path.parent().is_some_and(Path::is_dir)
            {
                bail!("coordinator_ready_path_invalid");
            }
            // A container restart must not inherit a marker left by an unclean exit.
            remove(path)?;
        }
        Ok(Self {
            path: path.map(Path::to_path_buf),
            published: false,
        })
    }

    /// Called on entry to the claim loop, only after every start check passed.
    pub(super) fn publish(&mut self) -> Result<()> {
        self.publish_before_rename(|_| Ok(()))
    }

    fn publish_before_rename(
        &mut self,
        before_rename: impl FnOnce(&Path) -> Result<()>,
    ) -> Result<()> {
        let Some(path) = self.path.as_ref().filter(|_| !self.published) else {
            return Ok(());
        };
        let parent = path.parent().expect("validated readiness parent");
        let temporary = parent.join(format!(".coordinator_ready_{}", Uuid::new_v4()));
        let written = (|| -> Result<()> {
            let mut options = OpenOptions::new();
            options.write(true).create_new(true);
            #[cfg(unix)]
            {
                use std::os::unix::fs::OpenOptionsExt;
                options.mode(0o600);
            }
            let mut file = options
                .open(&temporary)
                .context("coordinator_ready_write_failed")?;
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                file.set_permissions(fs::Permissions::from_mode(0o600))?;
            }
            let body = serde_json::json!({"ready_at": OffsetDateTime::now_utc().format(&Rfc3339)?,
                "version": env!("CARGO_PKG_VERSION")});
            file.write_all(&serde_json::to_vec(&body)?)?;
            file.sync_all()?;
            before_rename(&temporary)?;
            fs::rename(&temporary, path)?;
            File::open(parent)?.sync_all()?;
            Ok(())
        })();
        if written.is_err() {
            let _ = fs::remove_file(&temporary);
        }
        written.context("coordinator_ready_write_failed")?;
        self.published = true;
        Ok(())
    }

    pub(super) fn clear(&mut self) -> Result<()> {
        if let Some(path) = &self.path {
            remove(path)?;
        }
        self.published = false;
        Ok(())
    }
}

fn remove(path: &Path) -> Result<()> {
    match fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error).context("coordinator_ready_remove_failed"),
    }
}

impl Drop for ReadyFile {
    fn drop(&mut self) {
        if self.clear().is_err() {
            tracing::warn!("coordinator readiness cleanup failed");
        }
    }
}

/// Register before publishing so SIGTERM also cancels an in-flight claim or one-shot run.
#[cfg(unix)]
pub(super) fn shutdown_signal(enabled: bool) -> Result<impl std::future::Future<Output = ()>> {
    use tokio::signal::unix::{SignalKind, signal};
    let signals = if enabled {
        Some((
            signal(SignalKind::interrupt())?,
            signal(SignalKind::terminate())?,
        ))
    } else {
        None
    };
    Ok(async move {
        let Some((mut interrupt, mut terminate)) = signals else {
            return std::future::pending().await;
        };
        tokio::select! { _ = interrupt.recv() => {}, _ = terminate.recv() => {} }
    })
}

#[cfg(not(unix))]
pub(super) fn shutdown_signal(enabled: bool) -> Result<impl std::future::Future<Output = ()>> {
    Ok(async move {
        if enabled {
            let _ = tokio::signal::ctrl_c().await;
        } else {
            std::future::pending().await
        }
    })
}

#[cfg(test)]
#[path = "readiness_tests.rs"]
mod tests;
