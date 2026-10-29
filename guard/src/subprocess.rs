//! Bounded helper programs (qpdf, pdftotext, the guard's own worker): no stdin, an empty environment but PATH and
//! locale, a wall-clock timeout, CPU, address-space, file-size and core limits set in the child before it starts, its
//! own process group so a kill reaches everything it started, and capped output. A helper that exceeds a bound is
//! killed and reported as failed.
use anyhow::{Context, Result, bail};
use std::ffi::OsStr;
use std::process::Stdio;
use std::time::{Duration, Instant};
use tokio::io::AsyncReadExt;
use tokio::process::{Child, Command};

#[derive(Clone, Copy, Debug)]
pub struct Limits {
    pub timeout: Duration,
    pub memory_bytes: u64,
    pub output_bytes: usize,
    /// The largest file the helper may write; past it the kernel stops the helper.
    pub file_bytes: u64,
}

#[derive(Debug)]
pub struct Output {
    pub code: Option<i32>,
    pub stdout: Vec<u8>,
    pub stderr: String,
}

#[cfg(all(target_os = "linux", target_env = "gnu"))]
type Resource = libc::__rlimit_resource_t;
#[cfg(not(all(target_os = "linux", target_env = "gnu")))]
type Resource = libc::c_int;

fn set_limit(resource: Resource, value: u64) -> std::io::Result<()> {
    let limit = libc::rlimit {
        rlim_cur: value as libc::rlim_t,
        rlim_max: value as libc::rlim_t,
    };
    // SAFETY: setrlimit is async-signal-safe and only changes the child's own limits.
    if unsafe { libc::setrlimit(resource, &limit) } == 0 {
        Ok(())
    } else {
        Err(std::io::Error::last_os_error())
    }
}

/// Kills the helper's whole process group, then reaps the helper.
async fn kill_group(child: &mut Child) {
    if let Some(pid) = child.id() {
        // SAFETY: signalling a process group this code created; a stale group id fails harmlessly.
        unsafe {
            libc::kill(-(pid as libc::pid_t), libc::SIGKILL);
        }
    }
    let _ = child.kill().await;
}

pub async fn run(program: &str, args: &[&OsStr], limits: Limits) -> Result<Output> {
    let mut command = Command::new(program);
    command
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .env_clear()
        .env("PATH", "/usr/bin:/bin")
        .env("LC_ALL", "C.UTF-8");
    let memory = limits.memory_bytes;
    let files = limits.file_bytes;
    let cpu = limits.timeout.as_secs() + 5;
    // SAFETY: the closure only calls setpgid and setrlimit, which are async-signal-safe.
    unsafe {
        command.pre_exec(move || {
            if libc::setpgid(0, 0) != 0 {
                return Err(std::io::Error::last_os_error());
            }
            // Linux enforces an address-space limit; macOS, used only for development, does not support it.
            #[cfg(target_os = "linux")]
            set_limit(libc::RLIMIT_AS, memory)?;
            #[cfg(not(target_os = "linux"))]
            let _ = memory;
            set_limit(libc::RLIMIT_CPU, cpu)?;
            set_limit(libc::RLIMIT_FSIZE, files)?;
            set_limit(libc::RLIMIT_CORE, 0)?;
            Ok(())
        });
    }
    let started = Instant::now();
    let mut child = command
        .spawn()
        .with_context(|| format!("{program} could not start"))?;
    let mut stdout = child.stdout.take().expect("piped stdout");
    let mut stderr = child.stderr.take().expect("piped stderr");
    // Error output is read on its own task so a helper stopped on a full output pipe cannot hold the reader.
    let errors = tokio::spawn(async move {
        let mut err = Vec::new();
        let mut buf = vec![0u8; 8192];
        while let Ok(n) = stderr.read(&mut buf).await {
            if n == 0 {
                break;
            }
            if err.len() < 16384 {
                err.extend_from_slice(&buf[..n.min(16384 - err.len())]);
            }
        }
        err
    });
    let cap = limits.output_bytes;
    let read = async {
        let mut out = Vec::new();
        let mut buf = vec![0u8; 65536];
        loop {
            let n = stdout.read(&mut buf).await?;
            if n == 0 {
                return Ok::<Option<Vec<u8>>, std::io::Error>(Some(out));
            }
            if out.len() + n > cap {
                return Ok(None);
            }
            out.extend_from_slice(&buf[..n]);
        }
    };
    let out = match tokio::time::timeout(limits.timeout, read).await {
        Err(_) => {
            kill_group(&mut child).await;
            errors.abort();
            bail!("{program} timed out");
        }
        Ok(Err(e)) => {
            kill_group(&mut child).await;
            errors.abort();
            return Err(e).with_context(|| format!("{program} output unreadable"));
        }
        Ok(Ok(None)) => {
            kill_group(&mut child).await;
            errors.abort();
            bail!("{program} wrote more than its output limit");
        }
        Ok(Ok(Some(out))) => out,
    };
    let remaining = limits
        .timeout
        .saturating_sub(started.elapsed())
        .max(Duration::from_secs(1));
    let status = match tokio::time::timeout(remaining, child.wait()).await {
        Ok(status) => status.with_context(|| format!("{program} did not exit cleanly"))?,
        Err(_) => {
            kill_group(&mut child).await;
            errors.abort();
            bail!("{program} timed out");
        }
    };
    let err = tokio::time::timeout(Duration::from_secs(5), errors)
        .await
        .ok()
        .and_then(|r| r.ok())
        .unwrap_or_default();
    Ok(Output {
        code: status.code(),
        stdout: out,
        stderr: String::from_utf8_lossy(&err).into_owned(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn limits(seconds: u64, output: usize) -> Limits {
        Limits {
            timeout: Duration::from_secs(seconds),
            memory_bytes: 1 << 30,
            output_bytes: output,
            file_bytes: 1 << 20,
        }
    }

    #[tokio::test]
    async fn runs_and_bounds_a_helper() {
        let out = run(
            "/bin/sh",
            &[
                OsStr::new("-c"),
                OsStr::new("printf ok; printf err >&2; exit 3"),
            ],
            limits(5, 1024),
        )
        .await
        .unwrap();
        assert_eq!(
            (out.code, out.stdout.as_slice(), out.stderr.as_str()),
            (Some(3), &b"ok"[..], "err")
        );
        let slow = run(
            "/bin/sh",
            &[OsStr::new("-c"), OsStr::new("sleep 5")],
            limits(1, 1024),
        )
        .await;
        assert!(slow.unwrap_err().to_string().contains("timed out"));
        let started = Instant::now();
        let loud = run(
            "/bin/sh",
            &[OsStr::new("-c"), OsStr::new("yes | head -c 1000000")],
            limits(5, 1000),
        )
        .await;
        assert!(loud.unwrap_err().to_string().contains("output limit"));
        assert!(
            started.elapsed() < Duration::from_secs(3),
            "the over-limit helper is killed at once"
        );
        let dir = tempfile::tempdir().unwrap();
        let big = dir.path().join("big");
        let writer = run(
            "/bin/sh",
            &[
                OsStr::new("-c"),
                OsStr::new("head -c 2000000 /dev/zero > \"$0\""),
                big.as_os_str(),
            ],
            limits(5, 1000),
        )
        .await
        .unwrap();
        assert_ne!(
            writer.code,
            Some(0),
            "a file past the limit stops the helper"
        );
        assert!(std::fs::metadata(&big).unwrap().len() <= 1 << 20);
    }
}
