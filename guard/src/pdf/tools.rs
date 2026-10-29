//! The external tools and their bounded output, and the qpdf calls.
use super::*;

#[derive(Clone, Debug)]
pub struct Tools {
    pub qpdf: String,
    pub pdftotext: String,
    /// Draws each rebuilt PDF's pages at low resolution: every image is decoded once, by a reference decoder, within
    /// the helper limits.
    pub pdftoppm: String,
    /// The guard's own binary, started as `worker` for every lopdf read.
    pub worker: PathBuf,
    pub rules_dir: PathBuf,
    pub limits: Limits,
}

impl Tools {
    /// The same helpers, each output file limited for an input of `bytes` (`store::work_file_limit`).
    pub fn for_input(&self, bytes: u64) -> Tools {
        let mut tools = self.clone();
        tools.limits.file_bytes = crate::store::work_file_limit(bytes);
        tools
    }
}

/// Reads a helper's output file only when it is no larger than `limit`.
pub async fn read_bounded(path: &Path, limit: u64) -> Result<Vec<u8>> {
    let size = tokio::fs::metadata(path)
        .await
        .with_context(|| format!("{} missing", path.display()))?
        .len();
    if size > limit {
        bail!("the rebuilt copy is larger than the guard keeps ({size} bytes)");
    }
    tokio::fs::read(path)
        .await
        .context("rebuilt copy unreadable")
}

pub(super) fn arg(value: &str) -> &OsStr {
    OsStr::new(value)
}

pub async fn qpdf_version(tools: &Tools) -> Result<String> {
    let out = subprocess::run(&tools.qpdf, &[arg("--version")], tools.limits).await?;
    let text = String::from_utf8_lossy(&out.stdout);
    let line = text.lines().next().unwrap_or("").trim();
    if out.code != Some(0) || line.is_empty() {
        bail!("qpdf --version failed");
    }
    Ok(line.to_string())
}

/// None when qpdf could not read the file at all.
pub(super) async fn encryption(tools: &Tools, path: &Path) -> Option<Encryption> {
    let encrypted = subprocess::run(
        &tools.qpdf,
        &[arg("--is-encrypted"), path.as_os_str()],
        tools.limits,
    )
    .await
    .ok()?;
    if encrypted.code != Some(0) {
        return Some(Encryption::None);
    }
    let needs = subprocess::run(
        &tools.qpdf,
        &[arg("--requires-password"), path.as_os_str()],
        tools.limits,
    )
    .await
    .ok()?;
    Some(if needs.code == Some(0) {
        Encryption::Password
    } else {
        Encryption::OwnerOnly
    })
}

/// The first line of a helper's error output, bounded, for an internal error message (it names objects and offsets,
/// never document text).
pub(super) fn first_line(stderr: &str) -> String {
    stderr
        .lines()
        .next()
        .unwrap_or("")
        .chars()
        .take(200)
        .collect()
}

/// qpdf's exit codes: 0 success, 3 success with warnings (repairs), anything else failure.
pub(super) fn qpdf_ok(out: &subprocess::Output) -> bool {
    matches!(out.code, Some(0) | Some(3))
}

/// Writes a decrypted, repaired copy without object streams; false when qpdf cannot.
pub(super) async fn normalize(tools: &Tools, input: &Path, output: &Path) -> bool {
    let out = subprocess::run(
        &tools.qpdf,
        &[
            arg("--decrypt"),
            arg("--object-streams=disable"),
            input.as_os_str(),
            output.as_os_str(),
        ],
        tools.limits,
    )
    .await;
    matches!(out, Ok(out) if qpdf_ok(&out)) && output.is_file()
}

/// The digest of the text pdftotext extracts, with its invisible Unicode counted; no digest when pdftotext fails or
/// passes a limit.
pub async fn text_digest(tools: &Tools, path: &Path) -> (Option<String>, UnicodeCounts) {
    let out = subprocess::run(
        &tools.pdftotext,
        &[
            arg("-q"),
            arg("-enc"),
            arg("UTF-8"),
            arg("-eol"),
            arg("unix"),
            path.as_os_str(),
            arg("-"),
        ],
        tools.limits,
    )
    .await;
    match out {
        Ok(out) if out.code == Some(0) => {
            let text = String::from_utf8_lossy(&out.stdout);
            (
                Some(hex::encode(Sha256::digest(text.as_bytes()))),
                text::count(&text),
            )
        }
        _ => (None, UnicodeCounts::default()),
    }
}
