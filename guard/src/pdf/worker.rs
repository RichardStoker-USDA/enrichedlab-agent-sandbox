//! The isolated worker process: inspect and rebuild one file.
use super::*;

/// How a worker run ended when it did not report.
#[derive(Debug)]
pub enum WorkerError {
    /// The file's objects could not be read.
    Unreadable,
    /// The worker failed, crashed, or passed a limit.
    Failed(String),
}

impl std::fmt::Display for WorkerError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Unreadable => write!(f, "PDF objects unreadable"),
            Self::Failed(reason) => write!(f, "{reason}"),
        }
    }
}

/// The exit code a worker uses for a file whose objects cannot be read.
pub const UNREADABLE_EXIT: i32 = 3;

/// Runs one worker task under the helper limits and reads its JSON report.
pub async fn worker<T: DeserializeOwned>(
    tools: &Tools,
    task: &str,
    args: &[&OsStr],
) -> Result<T, WorkerError> {
    let program = tools
        .worker
        .to_str()
        .ok_or_else(|| WorkerError::Failed("worker path is not UTF-8".into()))?;
    let mut all = vec![arg("worker"), OsStr::new(task)];
    all.extend_from_slice(args);
    let out = subprocess::run(program, &all, tools.limits)
        .await
        .map_err(|e| WorkerError::Failed(format!("worker {task}: {e}")))?;
    match out.code {
        Some(0) => serde_json::from_slice(&out.stdout)
            .map_err(|e| WorkerError::Failed(format!("worker {task} report unreadable: {e}"))),
        Some(UNREADABLE_EXIT) => Err(WorkerError::Unreadable),
        code => Err(WorkerError::Failed(format!(
            "worker {task} ended with {code:?}: {}",
            first_line(&out.stderr)
        ))),
    }
}

/// L0 and L1 for a PDF: encryption, then (unless a password is needed) normalization, the worker's report on the
/// objects and streams, its hidden-text count, and the text's digest and invisible Unicode.
pub async fn inspect(tools: &Tools, original: &Path, normalized: &Path) -> Result<Inspection> {
    let mut inspection = Inspection {
        encryption: Encryption::None,
        readable: false,
        failed: false,
        hidden: Hidden::default(),
        unicode: UnicodeCounts::default(),
        text_sha256: None,
        report: Report::default(),
    };
    let Some(encryption) = encryption(tools, original).await else {
        return Ok(inspection);
    };
    inspection.encryption = encryption;
    if encryption == Encryption::Password || !normalize(tools, original, normalized).await {
        return Ok(inspection);
    }
    let input = normalized.as_os_str();
    let rules = tools.rules_dir.as_os_str();
    let inspect = [arg("--input"), input, arg("--rules"), rules];
    inspection.report = match worker::<Report>(tools, "pdf-inspect", &inspect).await {
        Ok(report) => report,
        Err(WorkerError::Unreadable) => return Ok(inspection),
        Err(WorkerError::Failed(reason)) => {
            tracing::warn!(target: "guard.worker", reason = %reason, "PDF inspection did not finish");
            inspection.failed = true;
            return Ok(inspection);
        }
    };
    inspection.readable = true;
    // Hidden text is evidence, not a gate: a count that does not finish is reported as partial.
    inspection.hidden = match worker::<Hidden>(tools, "pdf-text", &[arg("--input"), input]).await {
        Ok(hidden) => hidden,
        Err(e) => {
            tracing::warn!(target: "guard.worker", reason = %e, "hidden-text count did not finish");
            Hidden {
                truncated: true,
                ..Hidden::default()
            }
        }
    };
    let (digest, mut unicode) = text_digest(tools, normalized).await;
    unicode.add(inspection.report.metadata_unicode);
    inspection.unicode = unicode;
    inspection.text_sha256 = digest;
    Ok(inspection)
}

/// L2 for a PDF: the worker removes active content and every stream nothing draws, qpdf rewrites and checks the
/// result, pdftoppm draws every page, and the worker reports on the rewritten copy, whose text is digested again. A
/// copy larger than `max_copy` is refused before it is read.
pub async fn rebuild(
    tools: &Tools,
    normalized: &Path,
    work: &Path,
    max_copy: u64,
) -> Result<Rebuilt> {
    let stripped = work.join("stripped.pdf");
    let rewritten = work.join("rebuilt.pdf");
    let strip = [
        arg("--input"),
        normalized.as_os_str(),
        arg("--output"),
        stripped.as_os_str(),
    ];
    worker::<Stripped>(tools, "pdf-strip", &strip)
        .await
        .map_err(|e| anyhow::anyhow!("the stripped PDF was not written: {e}"))?;
    let out = subprocess::run(
        &tools.qpdf,
        &[
            arg("--object-streams=generate"),
            arg("--compress-streams=y"),
            stripped.as_os_str(),
            rewritten.as_os_str(),
        ],
        tools.limits,
    )
    .await?;
    if !qpdf_ok(&out) {
        bail!(
            "qpdf could not rewrite the stripped PDF: {}",
            first_line(&out.stderr)
        );
    }
    let check = subprocess::run(
        &tools.qpdf,
        &[arg("--check"), rewritten.as_os_str()],
        tools.limits,
    )
    .await?;
    if !qpdf_ok(&check) {
        bail!(
            "the rebuilt PDF fails qpdf --check: {}",
            first_line(&check.stderr)
        );
    }
    // Every page drawn into a 16-pixel square, written to the helper's bounded output and discarded: each image the
    // page draws is still decoded at its full size, so a decompression bomb, a codec variant whose size its headers
    // hide, or a decoder fault ends the helper and the copy is held. Nothing is written to the work area.
    let render = subprocess::run(
        &tools.pdftoppm,
        &[
            arg("-scale-to"),
            arg("16"),
            arg("-gray"),
            rewritten.as_os_str(),
        ],
        tools.limits,
    )
    .await;
    let rendered = matches!(&render, Ok(out) if out.code == Some(0));
    if !rendered {
        tracing::warn!(target: "guard.worker", "the rebuilt PDF did not render within the helper limits");
    }
    let bytes = read_bounded(&rewritten, max_copy).await?;
    let inspect = [
        arg("--input"),
        rewritten.as_os_str(),
        arg("--rules"),
        tools.rules_dir.as_os_str(),
    ];
    let report = worker::<Report>(tools, "pdf-inspect", &inspect)
        .await
        .map_err(|e| anyhow::anyhow!("the rebuilt PDF was not inspected: {e}"))?;
    let (text_sha256, _) = text_digest(tools, &rewritten).await;
    Ok(Rebuilt {
        bytes,
        report,
        text_sha256,
        rendered,
    })
}

// The worker's side: everything below reads untrusted objects and runs only inside a bounded worker process.

pub fn load(path: &Path) -> Result<Document> {
    parse(&std::fs::read(path).context("PDF unreadable")?)
}

/// The document's objects, with every reference to a plain value made direct (`direct_values`).
pub fn parse(bytes: &[u8]) -> Result<Document> {
    let options = LoadOptions {
        max_decompressed_size: Some(STREAM_LIMIT),
        ..Default::default()
    };
    let mut document =
        Document::load_mem_with_options(bytes, options).context("PDF objects unreadable")?;
    direct_values(&mut document)?;
    Ok(document)
}
