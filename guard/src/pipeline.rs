//! One file through the layers: L0 type and structure and L1 malware on the original, then, unless a finding calls for
//! quarantine or a block, L2: the rebuilt copy, checked again. The outcome follows the most severe finding.
//!
//! An exception an operator recorded releases only a quarantine found in the original (L0 and L1). The file is still
//! rebuilt and the rebuilt copy still checked: a failed rebuild, or any rebuild finding that calls for quarantine or a
//! block, holds the file regardless, and an allowed file is always its checked rebuilt copy, never the original.
use crate::clamd;
use crate::dataset;
use crate::detect::{self, FileType};
use crate::finding::{Finding, Layer};
use crate::pdf::{self, StreamMatch};
use crate::policy::{Action, UseCase};
use crate::raster;
use crate::rules::{Rules, Scope};
use crate::store;
use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::ffi::OsStr;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};
use time::OffsetDateTime;
use tokio::sync::Mutex;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Outcome {
    Allowed,
    Quarantined,
    Blocked,
    /// Not evaluated to the end: the scanner unavailable, or the evaluation failed.
    Refused,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
pub struct Versions {
    pub clamav: Option<String>,
    pub yara_rules: String,
    pub qpdf: String,
    pub guard: String,
}

pub struct Engines {
    pub rules: Arc<Rules>,
    pub pdf: pdf::Tools,
    /// None only for a local check run; the service always scans.
    pub scanner: Option<String>,
    pub scanner_timeout: Duration,
    pub signature_max_age: Duration,
    pub qpdf_version: String,
    version_cache: Mutex<Option<(Instant, clamd::Version)>>,
}

#[derive(Debug)]
pub struct Evaluation {
    pub detected: FileType,
    pub findings: Vec<Finding>,
    pub outcome: Outcome,
    /// The checked rebuilt copy of an allowed item; nothing for any other outcome.
    pub released: Option<Vec<u8>>,
    pub versions: Versions,
}

#[derive(Debug)]
pub struct Unavailable(pub String);

/// An exception an authorized person recorded for these bytes in this use case.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct Exception {
    pub sha256: String,
    pub use_case: String,
    pub approved_by: String,
    pub reason: String,
    pub expires_at: String,
    pub recorded_at: String,
}

impl Engines {
    pub fn new(
        rules: Rules,
        pdf: pdf::Tools,
        scanner: Option<String>,
        scanner_timeout: Duration,
        signature_max_age: Duration,
        qpdf_version: String,
    ) -> Self {
        Engines {
            rules: Arc::new(rules),
            pdf,
            scanner,
            scanner_timeout,
            signature_max_age,
            qpdf_version,
            version_cache: Mutex::new(None),
        }
    }

    /// The scanner's version when it answers and its signatures are recent enough; cached for a minute.
    pub async fn scanner_version(&self) -> Result<Option<clamd::Version>, Unavailable> {
        let Some(address) = &self.scanner else {
            return Ok(None);
        };
        let mut cache = self.version_cache.lock().await;
        let version = match cache.as_ref() {
            Some((at, v)) if at.elapsed() < Duration::from_secs(60) => v.clone(),
            _ => {
                let v = clamd::version(address, self.scanner_timeout)
                    .await
                    .map_err(|e| Unavailable(format!("scanner_unreachable: {e}")))?;
                *cache = Some((Instant::now(), v.clone()));
                v
            }
        };
        let age = OffsetDateTime::now_utc() - version.signatures;
        if age > time::Duration::try_from(self.signature_max_age).unwrap_or(time::Duration::MAX) {
            return Err(Unavailable(format!(
                "scanner_signatures_stale: {} hours old",
                age.whole_hours()
            )));
        }
        Ok(Some(version))
    }

    pub fn versions(&self, clamav: Option<&clamd::Version>) -> Versions {
        Versions {
            clamav: clamav.map(|v| v.text.clone()),
            yara_rules: self.rules.version.clone(),
            qpdf: self.qpdf_version.clone(),
            guard: env!("CARGO_PKG_VERSION").to_string(),
        }
    }

    /// L1 on one file's bytes: the daemon's signatures, then the rules. The rules run in a blocking task.
    async fn malware(
        &self,
        case: &UseCase,
        bytes: &Arc<Vec<u8>>,
        kind: &'static str,
        findings: &mut Vec<Finding>,
    ) -> Result<(), Unavailable> {
        if let Some(address) = &self.scanner {
            match clamd::scan(address, bytes, self.scanner_timeout).await {
                Ok(clamd::Verdict::Clean) => {}
                Ok(clamd::Verdict::Found(name)) => findings.push(Finding::new(
                    case,
                    "malware_signature",
                    Layer::L1,
                    1,
                    Some(name),
                )),
                Err(e) => return Err(Unavailable(format!("scanner_failed: {e}"))),
            }
        }
        // YARA-X runs its compiled rules on the calling thread's stack; a blocking thread gives it a fresh one.
        let scanned = Arc::clone(bytes);
        let rules = Arc::clone(&self.rules);
        let matches = tokio::task::spawn_blocking(move || rules.scan(&scanned, Scope::file(kind)))
            .await
            .map_err(|e| Unavailable(format!("rules_failed: {e}")))?
            .map_err(|e| Unavailable(format!("rules_failed: {e}")))?;
        for m in matches {
            findings.push(Finding::new(
                case,
                m.severity.code(),
                Layer::L1,
                1,
                Some(m.rule),
            ));
        }
        Ok(())
    }
}

fn worst(findings: &[Finding]) -> Action {
    findings
        .iter()
        .map(|f| f.action)
        .max()
        .unwrap_or(Action::Allow)
}

fn outcome_of(action: Action) -> Outcome {
    match action {
        Action::Block => Outcome::Blocked,
        Action::Quarantine => Outcome::Quarantined,
        _ => Outcome::Allowed,
    }
}

/// The outcome once the rebuilt copy is checked: findings from `rebuild_start` on are the rebuild's. A missing copy
/// holds the file. An exception releases only a quarantine in the original's findings, never a rebuild finding.
/// Returns the outcome, the copy to release, and whether the exception was what allowed the file.
pub fn conclude(
    findings: &[Finding],
    rebuild_start: usize,
    excepted: bool,
    copy: Option<Vec<u8>>,
) -> (Outcome, Option<Vec<u8>>, bool) {
    let original = worst(&findings[..rebuild_start]);
    let mut rebuild = worst(&findings[rebuild_start..]);
    if copy.is_none() {
        rebuild = rebuild.max(Action::Quarantine);
    }
    if rebuild >= Action::Quarantine {
        return (outcome_of(original.max(rebuild)), None, false);
    }
    match original {
        Action::Quarantine if excepted => (Outcome::Allowed, copy, true),
        Action::Quarantine | Action::Block => (outcome_of(original), None, false),
        _ => (Outcome::Allowed, copy, false),
    }
}

/// The rebuild finding for the text comparison: the rebuilt copy must read the same as the original.
pub fn text_change(original: Option<&str>, rebuilt: Option<&str>) -> Option<&'static str> {
    match (original, rebuilt) {
        (Some(a), Some(b)) if a == b => None,
        (Some(_), Some(_)) => Some("disarm_text_changed"),
        _ => Some("disarm_text_unavailable"),
    }
}

fn stream_findings(
    case: &UseCase,
    matches: &[StreamMatch],
    layer: Layer,
    findings: &mut Vec<Finding>,
) {
    for m in matches {
        findings.push(Finding::new(
            case,
            m.severity.code(),
            layer,
            m.count,
            Some(format!("{} in {}", m.rule, m.role.words())),
        ));
    }
}

/// The image worker's rebuild of a PNG or JPEG: decoded within its pixel limits, re-encoded without metadata.
async fn rebuild_image(
    tools: &pdf::Tools,
    bytes: &[u8],
    format: &str,
    work: &Path,
    max_copy: u64,
) -> Result<Vec<u8>> {
    let input = work.join(format!("original.{format}"));
    let output = work.join(format!("rebuilt.{format}"));
    tokio::fs::write(&input, bytes).await?;
    let args = [
        OsStr::new("--format"),
        OsStr::new(format),
        OsStr::new("--input"),
        input.as_os_str(),
        OsStr::new("--output"),
        output.as_os_str(),
    ];
    pdf::worker::<serde_json::Value>(tools, "raster", &args)
        .await
        .map_err(|e| anyhow::anyhow!("the image was not rebuilt: {e}"))?;
    pdf::read_bounded(&output, max_copy).await
}

/// Evaluates one file. `work` is an empty private directory the caller removes afterwards.
pub async fn evaluate(
    engines: &Engines,
    case: &UseCase,
    declared: FileType,
    bytes: Arc<Vec<u8>>,
    work: &Path,
    exception: Option<&Exception>,
) -> Result<Result<Evaluation, Unavailable>> {
    let version = match engines.scanner_version().await {
        Ok(v) => v,
        Err(unavailable) => return Ok(Err(unavailable)),
    };
    let versions = engines.versions(version.as_ref());
    let mut findings = Vec::new();
    let (detected, header) = detect::detect(&bytes);
    let mut detected = dataset::refine(detected, &bytes, &case.types);
    let tools = engines.pdf.for_input(bytes.len() as u64);
    let max_copy = store::rebuilt_limit(bytes.len() as u64);
    let early =
        |detected: FileType, findings: Vec<Finding>| -> Result<Result<Evaluation, Unavailable>> {
            Ok(Ok(Evaluation {
                detected,
                outcome: outcome_of(worst(&findings)),
                findings,
                released: None,
                versions: versions.clone(),
            }))
        };
    if bytes.is_empty() {
        findings.push(Finding::new(case, "empty_file", Layer::L0, 1, None));
        return early(detected, findings);
    }
    if bytes.len() as u64 > case.max_bytes {
        findings.push(Finding::new(
            case,
            "size_exceeded",
            Layer::L0,
            bytes.len() as u64,
            None,
        ));
        return early(detected, findings);
    }
    let allowed_type = case.types.contains(&detected);
    if !allowed_type {
        findings.push(Finding::new(
            case,
            "type_not_allowed",
            Layer::L0,
            1,
            Some(detected.described().to_string()),
        ));
    } else if !detected.satisfies(declared) {
        findings.push(Finding::new(
            case,
            "type_mismatch",
            Layer::L0,
            1,
            Some(format!(
                "sent as {}, is {}",
                declared.described(),
                detected.described()
            )),
        ));
    }
    if let Err(unavailable) = engines
        .malware(case, &bytes, detected.kind(), &mut findings)
        .await
    {
        return Ok(Err(unavailable));
    }
    let threshold = case.thresholds.hidden_text_chars;
    let mut normalized: Option<PathBuf> = None;
    let mut original_text = None;
    let mut grid = None;
    // A workbook is a ZIP package; any other type is checked for an archive inside it.
    if allowed_type && detected.satisfies(declared) && detected != FileType::Xlsx {
        let archive = match crate::zip::find(&bytes) {
            crate::zip::Found::None => None,
            crate::zip::Found::Archive => Some("an archive inside the file"),
            crate::zip::Found::TooMany => Some("more archive records than the guard checks"),
        };
        if let Some(detail) = archive {
            findings.push(Finding::new(
                case,
                "polyglot",
                Layer::L0,
                1,
                Some(detail.into()),
            ));
        }
    }
    if allowed_type && detected.satisfies(declared) {
        match detected {
            FileType::Pdf => {
                if detect::data_before_header(&bytes, header) {
                    findings.push(Finding::new(
                        case,
                        "polyglot",
                        Layer::L0,
                        1,
                        Some("data before the PDF header".into()),
                    ));
                }
                let trailing = detect::pdf_trailing(&bytes);
                if trailing > case.thresholds.trailing_bytes {
                    findings.push(Finding::new(
                        case,
                        "trailing_data",
                        Layer::L0,
                        trailing,
                        None,
                    ));
                }
                let original = work.join("original.pdf");
                let normal = work.join("normalized.pdf");
                tokio::fs::write(&original, bytes.as_slice()).await?;
                let inspection = pdf::inspect(&tools, &original, &normal).await?;
                match inspection.encryption {
                    pdf::Encryption::Password => {
                        findings.push(Finding::new(case, "encrypted_password", Layer::L0, 1, None))
                    }
                    pdf::Encryption::OwnerOnly => findings.push(Finding::new(
                        case,
                        "encrypted_owner_only",
                        Layer::L0,
                        1,
                        None,
                    )),
                    pdf::Encryption::None => {}
                }
                let report = &inspection.report;
                let stale = inspection.readable && report.rules_version != engines.rules.version;
                if inspection.failed || stale {
                    findings.push(Finding::new(case, "analysis_failed", Layer::L0, 1, None));
                } else if !inspection.readable && inspection.encryption != pdf::Encryption::Password
                {
                    findings.push(Finding::new(case, "structure_invalid", Layer::L0, 1, None));
                } else if inspection.readable && !report.walk_complete {
                    findings.push(Finding::new(
                        case,
                        "analysis_failed",
                        Layer::L0,
                        1,
                        Some("the stream-use walk did not finish".into()),
                    ));
                }
                for (code, count) in report.active.codes() {
                    findings.push(Finding::new(case, code, Layer::L0, count, None));
                }
                if report.hidden_layers {
                    findings.push(Finding::new(case, "hidden_layers", Layer::L0, 1, None));
                }
                let hidden = inspection.hidden;
                for (code, count) in [
                    ("hidden_text_invisible", hidden.invisible),
                    ("hidden_text_white", hidden.white),
                    ("hidden_text_tiny", hidden.tiny),
                    ("hidden_text_offpage", hidden.offpage),
                ] {
                    if count > threshold {
                        findings.push(Finding::new(case, code, Layer::L0, count, None));
                    }
                }
                if hidden.truncated {
                    findings.push(Finding::new(case, "analysis_truncated", Layer::L0, 1, None));
                }
                unicode_findings(case, inspection.unicode, &mut findings);
                if report.metadata_text {
                    findings.push(Finding::new(case, "metadata_text", Layer::L0, 1, None));
                }
                if report.streams.unknown > 0 {
                    findings.push(Finding::new(
                        case,
                        "private_streams",
                        Layer::L0,
                        report.streams.unknown,
                        None,
                    ));
                }
                if report.streams.unscanned > 0 {
                    findings.push(Finding::new(
                        case,
                        "streams_unscanned",
                        Layer::L1,
                        report.streams.unscanned,
                        None,
                    ));
                }
                if report.streams.oversized_images > 0 {
                    findings.push(Finding::new(
                        case,
                        "image_too_large",
                        Layer::L0,
                        report.streams.oversized_images,
                        None,
                    ));
                }
                if report.streams.archives > 0 {
                    findings.push(Finding::new(
                        case,
                        "embedded_archive",
                        Layer::L1,
                        report.streams.archives,
                        None,
                    ));
                }
                if report.streams.content_dropped > 0 {
                    findings.push(Finding::new(
                        case,
                        "content_irregular",
                        Layer::L0,
                        report.streams.content_dropped,
                        None,
                    ));
                }
                stream_findings(case, &report.matches, Layer::L1, &mut findings);
                original_text = inspection.text_sha256.clone();
                if inspection.readable && !stale {
                    normalized = Some(normal);
                }
            }
            FileType::Png | FileType::Jpeg => {
                let walk = if detected == FileType::Png {
                    raster::walk_png(&bytes)
                } else {
                    raster::walk_jpeg(&bytes)
                };
                if !walk.valid {
                    findings.push(Finding::new(case, "structure_invalid", Layer::L0, 1, None));
                }
                if walk.trailing > case.thresholds.trailing_bytes {
                    findings.push(Finding::new(
                        case,
                        "trailing_data",
                        Layer::L0,
                        walk.trailing,
                        None,
                    ));
                }
                unicode_findings(case, crate::text::count(&walk.metadata), &mut findings);
                if !walk.metadata.trim().is_empty() {
                    findings.push(Finding::new(case, "metadata_text", Layer::L0, 1, None));
                }
            }
            FileType::Delimited | FileType::Xlsx => {
                let source = dataset::Source::of(detected).expect("a table type");
                // The worker checks the table and writes its grid in one pass; the grid is the rebuilt copy below.
                match dataset::check(&tools, &bytes, source, work, max_copy).await {
                    Ok(Ok(rebuilt)) if rebuilt.report.rules_version != engines.rules.version => {
                        findings.push(Finding::new(case, "analysis_failed", Layer::L0, 1, None));
                    }
                    Ok(Ok(rebuilt)) if rebuilt.report.archive_without_workbook => {
                        detected = FileType::Zip;
                        findings.push(Finding::new(
                            case,
                            "type_not_allowed",
                            Layer::L0,
                            1,
                            Some(detected.described().to_string()),
                        ));
                    }
                    Ok(Ok(rebuilt)) => {
                        dataset_findings(case, &rebuilt.report, &mut findings);
                        grid = rebuilt.copy;
                    }
                    Ok(Err(dataset::Unreadable)) => {
                        findings.push(Finding::new(case, "structure_invalid", Layer::L0, 1, None));
                    }
                    Err(e) => {
                        tracing::warn!(target: "guard.pipeline", error = %e, "dataset check failed");
                        findings.push(Finding::new(case, "analysis_failed", Layer::L0, 1, None));
                    }
                }
            }
            _ => {}
        }
    }

    let before = worst(&findings);
    let excepted = exception.is_some() && before == Action::Quarantine;
    if before >= Action::Quarantine && !excepted {
        return early(detected, findings);
    }
    // L2: every item that may pass is rebuilt, and the rebuilt copy is checked again.
    let rebuild_start = findings.len();
    let rebuilt: Result<Vec<u8>> = match (detected, &normalized) {
        (FileType::Pdf, Some(path)) => match pdf::rebuild(&tools, path, work, max_copy).await {
            Ok(rebuilt) if rebuilt.report.rules_version != engines.rules.version => Err(
                anyhow::anyhow!("the rebuilt PDF was checked with other rules"),
            ),
            Ok(rebuilt) => {
                let report = &rebuilt.report;
                // The rebuilt copy holds no active content, no stream nothing draws and no content a viewer skips.
                let residual =
                    report.active.total() + report.streams.unknown + report.streams.content_dropped;
                if !report.walk_complete {
                    findings.push(Finding::new(case, "analysis_failed", Layer::L2, 1, None));
                }
                if !rebuilt.rendered {
                    findings.push(Finding::new(case, "render_failed", Layer::L2, 1, None));
                }
                if residual > 0 {
                    findings.push(Finding::new(
                        case,
                        "disarm_residual",
                        Layer::L2,
                        residual,
                        None,
                    ));
                }
                if report.streams.unscanned > 0 {
                    findings.push(Finding::new(
                        case,
                        "streams_unscanned",
                        Layer::L2,
                        report.streams.unscanned,
                        None,
                    ));
                }
                if report.streams.oversized_images > 0 {
                    findings.push(Finding::new(
                        case,
                        "image_too_large",
                        Layer::L2,
                        report.streams.oversized_images,
                        None,
                    ));
                }
                if report.streams.archives > 0 {
                    findings.push(Finding::new(
                        case,
                        "embedded_archive",
                        Layer::L2,
                        report.streams.archives,
                        None,
                    ));
                }
                stream_findings(case, &report.matches, Layer::L2, &mut findings);
                if let Some(code) =
                    text_change(original_text.as_deref(), rebuilt.text_sha256.as_deref())
                {
                    findings.push(Finding::new(case, code, Layer::L2, 1, None));
                }
                Ok(rebuilt.bytes)
            }
            Err(e) => Err(e),
        },
        (FileType::Png, _) => rebuild_image(&tools, &bytes, "png", work, max_copy).await,
        (FileType::Jpeg, _) => rebuild_image(&tools, &bytes, "jpeg", work, max_copy).await,
        (FileType::Delimited | FileType::Xlsx, _) => {
            grid.ok_or_else(|| anyhow::anyhow!("the table's grid was not written"))
        }
        _ => Err(anyhow::anyhow!("no rebuild for this type")),
    };
    let copy = match rebuilt {
        Ok(copy) if copy.len() as u64 > max_copy => {
            findings.push(Finding::new(
                case,
                "disarm_failed",
                Layer::L2,
                1,
                Some("the rebuilt copy is larger than the guard keeps".into()),
            ));
            None
        }
        Ok(copy) => {
            let copy = Arc::new(copy);
            let mut again = Vec::new();
            if let Err(unavailable) = engines
                .malware(case, &copy, detected.kind(), &mut again)
                .await
            {
                return Ok(Err(unavailable));
            }
            findings.extend(again.into_iter().map(|mut f| {
                f.layer = Layer::L2;
                f
            }));
            Some(Arc::try_unwrap(copy).unwrap_or_else(|a| (*a).clone()))
        }
        Err(e) => {
            tracing::warn!(target: "guard.pipeline", error = %e, "rebuild failed");
            findings.push(Finding::new(case, "disarm_failed", Layer::L2, 1, None));
            None
        }
    };
    let (outcome, released, applied) = conclude(&findings, rebuild_start, excepted, copy);
    if applied && let Some(exception) = exception {
        findings.push(Finding::new(
            case,
            "exception_applied",
            Layer::Policy,
            1,
            Some(exception.approved_by.clone()),
        ));
    }
    Ok(Ok(Evaluation {
        detected,
        outcome,
        findings,
        released,
        versions,
    }))
}

/// A lab dataset's findings from the worker's report: its bounds, what in a workbook runs, embeds or reaches out,
/// hidden sheets, formulas saved without values, invisible characters and rule matches in its cells.
fn dataset_findings(case: &UseCase, report: &dataset::Report, findings: &mut Vec<Finding>) {
    if let Some(bound) = &report.too_large {
        findings.push(Finding::new(
            case,
            "table_too_large",
            Layer::L0,
            1,
            Some(bound.clone()),
        ));
    }
    let structure = &report.structure;
    let hidden = report.sheets.iter().filter(|s| s.hidden).count() as u64;
    let without_values = report
        .sheets
        .iter()
        .map(|s| s.formulas_without_values)
        .sum();
    for (code, count) in [
        ("active_macros", structure.macros),
        ("active_controls", structure.controls),
        ("embedded_objects", structure.embedded),
        ("external_links", structure.external),
        ("xml_entities", structure.declarations),
        ("hidden_sheets", hidden),
        ("formulas_without_values", without_values),
    ] {
        if count > 0 {
            findings.push(Finding::new(case, code, Layer::L0, count, None));
        }
    }
    unicode_findings(case, report.unicode, findings);
    for m in &report.matches {
        findings.push(Finding::new(
            case,
            m.severity.code(),
            Layer::L1,
            1,
            Some(format!("{} in cell text", m.rule)),
        ));
    }
}

fn unicode_findings(
    case: &UseCase,
    counts: crate::text::UnicodeCounts,
    findings: &mut Vec<Finding>,
) {
    for (code, count) in [
        ("unicode_tags", counts.tags),
        ("unicode_bidi", counts.bidi),
        ("unicode_zero_width", counts.zero_width),
    ] {
        if count > 0 {
            findings.push(Finding::new(case, code, Layer::L0, count, None));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn finding(code: &str, action: Action, layer: Layer) -> Finding {
        Finding {
            code: code.into(),
            layer,
            action,
            count: 1,
            detail: None,
            explanation: String::new(),
        }
    }

    #[test]
    fn an_exception_never_releases_the_original_or_a_rebuild_finding() {
        let held = finding("polyglot", Action::Quarantine, Layer::L0);
        let copy = || Some(b"rebuilt".to_vec());
        // The rebuild failed: no copy, so the exception has nothing to release.
        let failed = [
            held.clone(),
            finding("disarm_failed", Action::Quarantine, Layer::L2),
        ];
        assert_eq!(
            conclude(&failed, 1, true, None),
            (Outcome::Quarantined, None, false)
        );
        // A rebuild finding that holds the file keeps it held.
        let residual = [
            held.clone(),
            finding("yara_high", Action::Quarantine, Layer::L2),
        ];
        assert_eq!(
            conclude(&residual, 1, true, copy()),
            (Outcome::Quarantined, None, false)
        );
        let blocked = [
            held.clone(),
            finding("malware_signature", Action::Block, Layer::L2),
        ];
        assert_eq!(conclude(&blocked, 1, true, copy()).0, Outcome::Blocked);
        // A clean rebuild of an excepted file passes as its rebuilt copy.
        let clean = [
            held.clone(),
            finding("trailing_data", Action::Disarm, Layer::L2),
        ];
        assert_eq!(
            conclude(&clean, 1, true, copy()),
            (Outcome::Allowed, copy(), true)
        );
        // Without the exception the original's quarantine holds.
        assert_eq!(
            conclude(&clean, 1, false, copy()),
            (Outcome::Quarantined, None, false)
        );
        // An ordinary file passes as its rebuilt copy, and without one it is held.
        let ordinary = [finding("active_javascript", Action::Disarm, Layer::L0)];
        assert_eq!(
            conclude(&ordinary, 1, false, copy()),
            (Outcome::Allowed, copy(), false)
        );
        assert_eq!(
            conclude(&ordinary, 1, false, None),
            (Outcome::Quarantined, None, false)
        );
    }

    #[test]
    fn the_rebuilt_text_must_be_there_and_the_same() {
        assert_eq!(text_change(Some("a"), Some("a")), None);
        assert_eq!(
            text_change(Some("a"), Some("b")),
            Some("disarm_text_changed")
        );
        assert_eq!(
            text_change(None, Some("a")),
            Some("disarm_text_unavailable")
        );
        assert_eq!(
            text_change(Some("a"), None),
            Some("disarm_text_unavailable")
        );
    }
}
