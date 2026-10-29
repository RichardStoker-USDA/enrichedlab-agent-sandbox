//! Result digests. A bounded, deterministic excerpt of one retained result package,
//! built from the bytes the worker verified, for the research lead and the workspace: the result
//! tables with their cells as written when a table is small, each column's summary over every row,
//! the figure names and the run parameters. It reads nothing under `input/` and validates nothing; a package it cannot read
//! still yields a digest whose status names why.
use std::{
    collections::{HashMap, HashSet},
    fs::OpenOptions,
    hash::{DefaultHasher, Hash, Hasher},
    io::{self, BufRead, Read, Write},
    os::unix::fs::OpenOptionsExt,
    path::{Component, Path, PathBuf},
};

use anyhow::{Result, anyhow, ensure};
use serde::Deserialize;
use serde_json::{Map, Value, json};
use sha2::{Digest, Sha256};
use uuid::Uuid;

use super::csv_records::{Record, Records};
use super::evidence_package::{
    ManifestCheck, Package, PackageError, PackageListing, list_package, normalize_member_path,
};

pub const SCHEMA: &str = "enrichedlab.result-digest.v1";
/// Tables of a lab's own data (a prepared dataset) are listed with their columns and counts, never their
/// cells: row values reach people, not the lead.
pub const LAB_DATA: &str = "results/lab_data/";
/// Results tables an analysis computed from the lab's own data, listed the same way: an analysis's rows may be one per
/// animal. Its headline statistics stay in `results/statistics.csv`.
pub const LAB_DERIVED: &str = "results/lab_derived/";
pub const MAX_DIGEST_BYTES: usize = 16 * 1024;
const MAX_TABLES: usize = 12;
const MAX_FIGURES: usize = 12;
const MAX_LISTED_COLUMNS: usize = 64;
// At most this many rows keep their cells. It was 50 at first: one suite goal's answer, SR Legacy vegetables with
// high potassium and low sodium, is 104 rows in under 8 KiB, and with its cells withheld no attempt could
// show it in the workspace or state it. The byte limits still bound every table; the lead is shown at most 50 rows.
const INLINE_ROWS: u64 = 200;
// At most this many columns keep their cells, within `INLINE_BYTES`. It was 16 until a test run: the reviewed
// FoodData Central normalization writes one row per food in 18 or 19 columns (per 100 g and per portion, computed and
// reported energy, the factors used), so a five-food table of about 1 KiB lost its cells and the lead could not report
// any food's values. The byte limit bounds every table as before.
const INLINE_COLUMNS: usize = 32;
const INLINE_BYTES: u64 = 8 * 1024;
// A cell longer than this withholds its table's cells. It was 64 until test runs: 2,451 of 7,793 SR Legacy
// food names are longer (the longest is 134), so nearly every list of twenty foods lost its cells, and the lead could not
// state findings from a table whose rows it was never shown.
const MAX_CELL_CHARS: usize = 160;
/// Room for one record of a table being read: a record with more fields or text leaves the table unreadable here.
const MAX_RECORD_FIELDS: usize = 100_000;
const MAX_RECORD_BYTES: usize = 16 * 1024 * 1024;
/// A column name keeps its first this many characters; the step checks cannot match a longer one.
pub(crate) const MAX_NAME_CHARS: usize = 64;
const MAX_PARAMETERS_BYTES: u64 = 2 * 1024;
/// A column's distinct values are counted up to this many; past it the summary says `distinct_capped`.
const DISTINCT_MAX: usize = 20_000;
/// A smallest or largest number is kept as written when it is at most this many characters.
const NUMBER_TEXT_CHARS: usize = 40;
const MAX_BACKFILL_PACKAGES: usize = 100;
const MAX_BACKFILL_LINE: usize = 4096;

/// `status` is one of the fixed values migration 021 accepts; `value` is the digest document.
#[derive(Debug)]
pub struct ResultDigest {
    pub status: &'static str,
    pub value: Value,
}

fn gap(status: &'static str) -> ResultDigest {
    ResultDigest {
        status,
        value: json!({"schema": SCHEMA, "status": status}),
    }
}

/// One registered result package to digest again: its task, path under the artifact root, size and
/// SHA-256 as the broker recorded them.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PackageRequest {
    task_id: Uuid,
    relative_path: String,
    size_bytes: u64,
    sha256: String,
}

/// The operator's backfill: digests, built exactly as the worker builds them,
/// for the packages named on `input`, one JSON object per line, at most 100. Writes one line per
/// package with `task_id`, `relative_path`, `artifact_sha256`, `status` and `digest`; a package it
/// cannot read yields a digest whose status names why. A malformed line stops the run before its
/// package is read. Returns how many packages it digested.
pub fn digest_packages(root: &Path, input: impl BufRead, mut output: impl Write) -> Result<usize> {
    let mut count = 0;
    for line in input.lines() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        count += 1;
        ensure!(count <= MAX_BACKFILL_PACKAGES, "digest_packages_too_many");
        ensure!(line.len() <= MAX_BACKFILL_LINE, "digest_packages_invalid");
        let request: PackageRequest =
            serde_json::from_str(&line).map_err(|_| anyhow!("digest_packages_invalid"))?;
        ensure!(
            request.sha256.len() == 64
                && request
                    .sha256
                    .bytes()
                    .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)),
            "digest_packages_invalid"
        );
        let built = build(
            root,
            request.task_id,
            &request.relative_path,
            request.size_bytes,
            &request.sha256,
        );
        serde_json::to_writer(
            &mut output,
            &json!({"task_id": request.task_id, "relative_path": request.relative_path,
                "artifact_sha256": request.sha256, "status": built.status, "digest": built.value}),
        )?;
        output.write_all(b"\n")?;
    }
    output.flush()?;
    Ok(count)
}

/// Digest the registered result at `relative_path` under the worker's artifact root. The path must
/// stay inside `tasks/<task>/` and name a regular file of the registered size and SHA-256.
pub fn build(
    root: &Path,
    task: Uuid,
    relative_path: &str,
    size: u64,
    sha256: &str,
) -> ResultDigest {
    let Some(path) = resolve(root, task, relative_path) else {
        return gap("unreadable");
    };
    let listing = match list_package(&path, size, sha256, None) {
        Ok(listing) => listing,
        Err(PackageError::NotAPackage) => return gap("not_a_package"),
        Err(PackageError::TooLarge) => return gap("too_large"),
        Err(PackageError::Integrity) => return gap("integrity_failed"),
        Err(PackageError::Io) => return gap("unreadable"),
    };
    summarize(&path, size, &listing)
}

fn resolve(root: &Path, task: Uuid, relative_path: &str) -> Option<PathBuf> {
    let relative = Path::new(relative_path);
    let mut parts = relative.components();
    let task_id = task.to_string();
    if relative.is_absolute()
        || !relative
            .components()
            .all(|c| matches!(c, Component::Normal(_)))
        || parts.next()?.as_os_str() != "tasks"
        || parts.next()?.as_os_str() != task_id.as_str()
    {
        return None;
    }
    let root = root.canonicalize().ok()?;
    let candidate = root.join(relative);
    let metadata = std::fs::symlink_metadata(&candidate).ok()?;
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return None;
    }
    let canonical = candidate.canonicalize().ok()?;
    canonical.starts_with(&root).then_some(canonical)
}

/// Control characters become spaces; a value longer than `max` characters is refused. Answer documents fill a
/// finding's values with the same rule, so a document states each value as the workspace shows it.
pub(super) fn clean(raw: &[u8], max: usize) -> Option<String> {
    let text: String = String::from_utf8_lossy(raw)
        .chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect();
    let text = text.trim();
    (text.chars().count() <= max).then(|| text.to_owned())
}

pub(super) fn column_name(raw: &[u8], index: usize) -> String {
    let text: String = String::from_utf8_lossy(raw)
        .chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect();
    let text = text.trim();
    if text.is_empty() {
        // R writes row names under an empty header.
        return format!("column_{}", index + 1);
    }
    let mut name: String = text.chars().take(MAX_NAME_CHARS).collect();
    if text.chars().count() > MAX_NAME_CHARS {
        name.push('…');
    }
    name
}

struct Hashing<R> {
    inner: R,
    hasher: Sha256,
    read: u64,
    limit: u64,
}

impl<R: Read> Read for Hashing<R> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let n = self.inner.read(buf)?;
        self.hasher.update(&buf[..n]);
        self.read += n as u64;
        if self.read > self.limit {
            return Err(io::Error::other("member_larger_than_listed"));
        }
        Ok(n)
    }
}

enum Wanted {
    Table,
    Parameters,
}

pub(crate) fn table(
    reader: &mut impl Read,
    bytes: u64,
    lab: Option<&'static str>,
) -> Map<String, Value> {
    let mut out = Map::new();
    let mut records = Records::new(
        io::BufReader::new(reader),
        b',',
        MAX_RECORD_FIELDS,
        MAX_RECORD_BYTES,
    );
    let names: Vec<String> = match records.read_record() {
        Ok(Some(Record::Fields(_))) => records
            .fields()
            .enumerate()
            .map(|(i, raw)| column_name(raw, i))
            .collect(),
        Ok(None) => Vec::new(),
        Ok(Some(_)) | Err(_) => {
            out.insert("cells_omitted".into(), json!("unreadable"));
            return out;
        }
    };
    let mut unique = names.clone();
    unique.sort();
    unique.dedup();
    let mut omitted = if lab.is_some() {
        lab
    } else if names.is_empty() {
        Some("no_columns")
    } else if bytes > INLINE_BYTES || names.len() > INLINE_COLUMNS {
        Some("too_large")
    } else if unique.len() != names.len() {
        Some("duplicate_columns")
    } else {
        None
    };
    let mut cells: Vec<Value> = Vec::new();
    let mut rows = 0_u64;
    // Rows without a value in each column, over every row, also when the cells are withheld: an empty field (a query
    // writes NULL that way), exactly `NA` (R's missing value), or no field at all in a record shorter than the header.
    // An empty quoted text reads the same as NULL here. Only the listed columns are counted: the rest are never shown.
    let mut missing = vec![0_u64; names.len().min(MAX_LISTED_COLUMNS)];
    let mut stats: Vec<ColumnStats> = missing.iter().map(|_| ColumnStats::new()).collect();
    loop {
        let width = match records.read_record() {
            Ok(None) => break,
            Ok(Some(Record::Fields(width))) => width,
            Ok(Some(_)) | Err(_) => {
                out.insert(
                    "columns".into(),
                    json!(names.iter().take(MAX_LISTED_COLUMNS).collect::<Vec<_>>()),
                );
                out.insert("cells_omitted".into(), json!("unreadable"));
                return out;
            }
        };
        rows += 1;
        let mut fields = records.fields();
        for (count, stat) in missing.iter_mut().zip(stats.iter_mut()) {
            match fields.next() {
                Some(field) if !(field.trim_ascii().is_empty() || field == b"NA") => {
                    stat.add(field)
                }
                _ => *count += 1,
            }
        }
        if omitted.is_some() {
            continue;
        }
        if rows > INLINE_ROWS {
            omitted = Some("too_many_rows");
            cells.clear();
            continue;
        }
        if width != names.len() {
            omitted = Some("irregular_rows");
            cells.clear();
            continue;
        }
        let row: Option<Vec<String>> = records.fields().map(|f| clean(f, MAX_CELL_CHARS)).collect();
        match row {
            Some(row) => cells.push(json!(row)),
            None => {
                omitted = Some("long_cells");
                cells.clear();
            }
        }
    }
    out.insert("rows".into(), json!(rows));
    out.insert(
        "columns".into(),
        json!(names.iter().take(MAX_LISTED_COLUMNS).collect::<Vec<_>>()),
    );
    if names.len() > MAX_LISTED_COLUMNS {
        out.insert(
            "columns_omitted".into(),
            json!(names.len() - MAX_LISTED_COLUMNS),
        );
    }
    // By listed column name, only where some row lacks a value; duplicate names would make the counts ambiguous.
    if unique.len() == names.len() {
        let counts: Map<String, Value> = names
            .iter()
            .zip(&missing)
            .take(MAX_LISTED_COLUMNS)
            .filter(|(_, n)| **n > 0)
            .map(|(name, n)| (name.clone(), json!(n)))
            .collect();
        if !counts.is_empty() {
            out.insert("missing".into(), Value::Object(counts));
        }
        // Each listed column over every row, for the step checks: whether every value is a number, the smallest and
        // largest as written (never for a lab's own table, whose values stay out of the digest), and how
        // many distinct values.
        let summary: Map<String, Value> = names
            .iter()
            .zip(&stats)
            .map(|(name, s)| (name.clone(), s.value(lab.is_none())))
            .collect();
        if !summary.is_empty() {
            out.insert("summary".into(), Value::Object(summary));
        }
    }
    match omitted {
        Some(reason) => {
            out.insert("cells_omitted".into(), json!(reason));
        }
        None => {
            out.insert("cells".into(), Value::Array(cells));
        }
    }
    out
}

fn summarize(path: &PathBuf, size: u64, listing: &PackageListing) -> ResultDigest {
    let listed = listing.checksum_file.is_some();
    let all_matched = listed
        && listing.missing_from_archive.is_empty()
        && listing
            .members
            .iter()
            .filter(|m| m.path != "SHA256SUMS")
            .all(|m| m.manifest == ManifestCheck::Matched);
    let matched = |path: &str| {
        listing
            .members
            .iter()
            .find(|m| m.path == path)
            .is_some_and(|m| m.manifest == ManifestCheck::Matched)
    };
    let tables: Vec<_> = listing
        .members
        .iter()
        .filter(|m| {
            m.path.starts_with("results/") && m.media_type == "text/csv" && matched(&m.path)
        })
        .collect();
    let figures: Vec<_> = listing
        .members
        .iter()
        .filter(|m| m.path.starts_with("results/") && m.kind == "figure" && matched(&m.path))
        .take(MAX_FIGURES)
        .map(|m| json!({"path": m.path, "sha256": m.sha256}))
        .collect();
    let parameters = listing.members.iter().find(|m| {
        m.path == "run_parameters.json" && matched(&m.path) && m.size_bytes <= MAX_PARAMETERS_BYTES
    });
    let mut wanted: HashMap<&str, (Wanted, &super::evidence_package::PackageMember)> = tables
        .iter()
        .take(MAX_TABLES)
        .map(|m| (m.path.as_str(), (Wanted::Table, *m)))
        .collect();
    if let Some(m) = parameters {
        wanted.insert(m.path.as_str(), (Wanted::Parameters, m));
    }
    let mut read_tables: HashMap<String, Map<String, Value>> = HashMap::new();
    let mut read_parameters: Option<Value> = None;
    let Ok(file) = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW)
        .open(path)
    else {
        return gap("unreadable");
    };
    if file.metadata().map(|m| m.len()).ok() != Some(size) {
        return gap("integrity_failed");
    }
    let mut package = Package::new(file);
    let Ok(mut entries) = package.members() else {
        return gap("not_a_package");
    };
    loop {
        let (raw_path, entry) = match entries.next_member() {
            Ok(Some(next)) => next,
            Ok(None) => break,
            Err(_) => return gap("not_a_package"),
        };
        if !entry.header().entry_type().is_file() {
            continue;
        }
        let Some(name) = normalize_member_path(&raw_path) else {
            continue;
        };
        let Some((kind, member)) = wanted.remove(name.as_str()) else {
            continue;
        };
        let mut reader = Hashing {
            inner: entry,
            hasher: Sha256::new(),
            read: 0,
            limit: member.size_bytes,
        };
        match kind {
            Wanted::Table => {
                let lab = if member.path.starts_with(LAB_DATA) {
                    Some("lab_data")
                } else if member.path.starts_with(LAB_DERIVED) {
                    Some("lab_derived")
                } else {
                    None
                };
                let mut entry = table(&mut reader, member.size_bytes, lab);
                if io::copy(&mut reader, &mut io::sink()).is_err() {
                    return gap("integrity_failed");
                }
                entry.insert("bytes".into(), json!(member.size_bytes));
                entry.insert("sha256".into(), json!(member.sha256));
                entry.insert("path".into(), json!(member.path));
                read_tables.insert(member.path.clone(), entry);
            }
            Wanted::Parameters => {
                let mut bytes = Vec::new();
                if reader.read_to_end(&mut bytes).is_err() {
                    return gap("integrity_failed");
                }
                // PostgreSQL's jsonb cannot hold a NUL character, so parameters with one are left out like unreadable
                // ones; everything else in a digest is cleaned of control characters.
                read_parameters = serde_json::from_slice::<Value>(&bytes)
                    .ok()
                    .filter(|v| v.is_object() && !holds_nul(v));
            }
        }
        if reader.read != member.size_bytes
            || hex::encode(reader.hasher.finalize()) != member.sha256
        {
            return gap("integrity_failed");
        }
    }
    // A member the listing verified that this pass did not find means the file changed.
    if !wanted.is_empty() {
        return gap("integrity_failed");
    }
    let mut table_values: Vec<Value> = tables
        .iter()
        .take(MAX_TABLES)
        .filter_map(|m| read_tables.remove(&m.path).map(Value::Object))
        .collect();
    let mut tables_omitted = tables.len().saturating_sub(MAX_TABLES);
    let status = if table_values.is_empty() {
        "no_result_tables"
    } else {
        "summarized"
    };
    let mut value = json!({
        "schema": SCHEMA,
        "status": status,
        "package": {"members": listing.members.len(), "checksums_listed": listed, "all_matched": all_matched},
        "tables": table_values,
        "tables_omitted": tables_omitted,
        "figures": figures,
    });
    if let Some(parameters) = read_parameters {
        value["parameters"] = parameters;
    }
    // Keep within the stored bound, measured as PostgreSQL stores it: drop the largest inline cells first, then the
    // parameters, then the largest column summaries, then the largest missing-value counts, then trailing tables. Each
    // step is deterministic.
    while stored_bytes(&value) > MAX_DIGEST_BYTES {
        table_values = value["tables"].as_array().cloned().unwrap_or_default();
        let largest = table_values
            .iter()
            .enumerate()
            .filter(|(_, t)| t.get("cells").is_some())
            .max_by_key(|(i, t)| (stored_bytes(&t["cells"]), usize::MAX - i))
            .map(|(i, _)| i);
        if let Some(i) = largest {
            let t = table_values[i].as_object_mut().expect("table object");
            t.remove("cells");
            t.insert("cells_omitted".into(), json!("digest_budget"));
            value["tables"] = Value::Array(table_values);
        } else if value.get("parameters").is_some() {
            value
                .as_object_mut()
                .expect("digest object")
                .remove("parameters");
        } else if let Some(i) = table_values
            .iter()
            .enumerate()
            .filter(|(_, t)| t.get("summary").is_some())
            .max_by_key(|(i, t)| (stored_bytes(&t["summary"]), usize::MAX - i))
            .map(|(i, _)| i)
        {
            let t = table_values[i].as_object_mut().expect("table object");
            t.remove("summary");
            t.insert("summary_omitted".into(), json!("digest_budget"));
            value["tables"] = Value::Array(table_values);
        } else if let Some(i) = table_values
            .iter()
            .enumerate()
            .filter(|(_, t)| t.get("missing").is_some())
            .max_by_key(|(i, t)| (stored_bytes(&t["missing"]), usize::MAX - i))
            .map(|(i, _)| i)
        {
            let t = table_values[i].as_object_mut().expect("table object");
            t.remove("missing");
            t.insert("missing_omitted".into(), json!("digest_budget"));
            value["tables"] = Value::Array(table_values);
        } else if !table_values.is_empty() {
            table_values.pop();
            tables_omitted += 1;
            value["tables"] = Value::Array(table_values);
            value["tables_omitted"] = json!(tables_omitted);
        } else {
            value["figures"] = json!([]);
            break;
        }
    }
    ResultDigest { status, value }
}

/// One column's values over every row of a table, for the step checks.
struct ColumnStats {
    /// Whether every value so far reads as a finite number.
    numeric: bool,
    low: Option<(f64, String)>,
    high: Option<(f64, String)>,
    /// A hash of each distinct value, until there are more than `DISTINCT_MAX`. Two values sharing a 64-bit hash would
    /// count once; for 20,000 values that chance is below one in ten billion.
    seen: Option<HashSet<u64>>,
}

impl ColumnStats {
    fn new() -> Self {
        Self {
            numeric: true,
            low: None,
            high: None,
            seen: Some(HashSet::new()),
        }
    }

    /// One value that is not missing, as the file holds it.
    fn add(&mut self, field: &[u8]) {
        let text = field.trim_ascii();
        if let Some(seen) = self.seen.as_mut() {
            let mut hasher = DefaultHasher::new();
            text.hash(&mut hasher);
            seen.insert(hasher.finish());
            if seen.len() > DISTINCT_MAX {
                self.seen = None;
            }
        }
        if !self.numeric {
            return;
        }
        let Some(v) = finite_number(text) else {
            self.numeric = false;
            self.low = None;
            self.high = None;
            return;
        };
        let written = || match std::str::from_utf8(text) {
            Ok(t) if t.chars().count() <= NUMBER_TEXT_CHARS => t.to_owned(),
            _ => format!("{v:e}"),
        };
        if self.low.as_ref().is_none_or(|(low, _)| v < *low) {
            self.low = Some((v, written()));
        }
        if self.high.as_ref().is_none_or(|(high, _)| v > *high) {
            self.high = Some((v, written()));
        }
    }

    /// The summary the digest keeps: `numeric`, with `min` and `max` when `values` allows them, and `distinct` or
    /// `distinct_capped`. A column with no value is not numeric and has no distinct values.
    fn value(&self, values: bool) -> Value {
        let mut out = Map::new();
        let numeric = self.numeric && self.low.is_some();
        out.insert("numeric".into(), json!(numeric));
        if numeric
            && values
            && let (Some((_, low)), Some((_, high))) = (&self.low, &self.high)
        {
            out.insert("min".into(), json!(low));
            out.insert("max".into(), json!(high));
        }
        match &self.seen {
            Some(seen) => out.insert("distinct".into(), json!(seen.len())),
            None => out.insert("distinct_capped".into(), json!(true)),
        };
        Value::Object(out)
    }
}

/// A cell that reads as a finite decimal number: an optional sign, digits with an optional fraction, and an optional
/// exponent. R's `Inf` and `NaN`, and hexadecimal, are not numbers here.
fn finite_number(text: &[u8]) -> Option<f64> {
    let s = std::str::from_utf8(text).ok()?;
    let body = s.strip_prefix(['-', '+']).unwrap_or(s);
    let (mantissa, exponent) = match body.find(['e', 'E']) {
        Some(i) => (&body[..i], Some(&body[i + 1..])),
        None => (body, None),
    };
    let (whole, fraction) = mantissa.split_once('.').unwrap_or((mantissa, ""));
    let digits = |p: &str| p.bytes().all(|b| b.is_ascii_digit());
    let exponent_ok = exponent.is_none_or(|e| {
        let e = e.strip_prefix(['-', '+']).unwrap_or(e);
        !e.is_empty() && digits(e)
    });
    if !(digits(whole)
        && digits(fraction)
        && !(whole.is_empty() && fraction.is_empty())
        && exponent_ok)
    {
        return None;
    }
    s.parse::<f64>().ok().filter(|v| v.is_finite())
}

fn holds_nul(value: &Value) -> bool {
    match value {
        Value::String(s) => s.contains('\0'),
        Value::Array(items) => items.iter().any(holds_nul),
        Value::Object(map) => map.iter().any(|(k, v)| k.contains('\0') || holds_nul(v)),
        _ => false,
    }
}

/// The bytes of `value` as PostgreSQL prints a `jsonb` (`digest::text`, which migration 021 bounds): a space after each
/// `:` and `,`, text escaped as JSON escapes it, and each number as `numeric` prints it, without an exponent.
pub fn stored_bytes(value: &Value) -> usize {
    let quoted = |s: &str| {
        2 + s
            .chars()
            .map(|c| match c {
                '"' | '\\' | '\u{8}' | '\u{c}' | '\n' | '\r' | '\t' => 2,
                c if u32::from(c) < 0x20 => 6,
                c => c.len_utf8(),
            })
            .sum::<usize>()
    };
    let between = |n: usize| 2 * n.saturating_sub(1);
    match value {
        Value::Null | Value::Bool(true) => 4,
        Value::Bool(false) => 5,
        Value::Number(n) => stored_number(&n.to_string()),
        Value::String(s) => quoted(s),
        Value::Array(items) => {
            2 + items.iter().map(stored_bytes).sum::<usize>() + between(items.len())
        }
        Value::Object(map) => {
            2 + map
                .iter()
                .map(|(k, v)| quoted(k) + 2 + stored_bytes(v))
                .sum::<usize>()
                + between(map.len())
        }
    }
}

/// How `numeric` prints a JSON number written as `text`: the digits before the point without leading zeros (at least
/// one), then as many digits after it as the text had less its exponent, and a minus sign unless the value is zero.
fn stored_number(text: &str) -> usize {
    let (negative, unsigned) = match text.strip_prefix('-') {
        Some(rest) => (true, rest),
        None => (false, text),
    };
    let (mantissa, exponent) = match unsigned.split_once(['e', 'E']) {
        Some((m, e)) => (m, e.parse::<i64>().unwrap_or(0)),
        None => (unsigned, 0),
    };
    let (whole, fraction) = mantissa.split_once('.').unwrap_or((mantissa, ""));
    let digits = format!("{whole}{fraction}");
    let scale = usize::try_from(i64::try_from(fraction.len()).unwrap_or(0) - exponent).unwrap_or(0);
    let point = i64::try_from(whole.len()).unwrap_or(0) + exponent;
    let before = match usize::try_from(point) {
        Ok(p) if p >= digits.len() => {
            let significant = digits.trim_start_matches('0').len();
            if significant == 0 {
                0
            } else {
                significant + (p - digits.len())
            }
        }
        Ok(p) => digits[..p].trim_start_matches('0').len(),
        Err(_) => 0,
    };
    let zero = digits.bytes().all(|b| b == b'0');
    usize::from(negative && !zero) + before.max(1) + if scale > 0 { 1 + scale } else { 0 }
}

#[cfg(test)]
mod tests {
    use super::*;
    use flate2::{Compression, GzBuilder};

    fn sha(bytes: &[u8]) -> String {
        hex::encode(Sha256::digest(bytes))
    }

    /// A package with a checksum list covering every member unless `tamper` names one to misstate.
    fn package(files: &[(&str, &[u8])], tamper: Option<&str>) -> Vec<u8> {
        let mut sums = String::new();
        for (path, bytes) in files {
            let digest = if Some(*path) == tamper {
                sha(b"other")
            } else {
                sha(bytes)
            };
            sums.push_str(&format!("{digest}  {path}\n"));
        }
        let mut builder =
            tar::Builder::new(GzBuilder::new().write(Vec::new(), Compression::default()));
        let mut all: Vec<(&str, Vec<u8>)> = files.iter().map(|(p, b)| (*p, b.to_vec())).collect();
        all.push(("SHA256SUMS", sums.into_bytes()));
        for (path, bytes) in &all {
            let mut header = tar::Header::new_gnu();
            header.set_size(bytes.len() as u64);
            header.set_mode(0o600);
            header.set_cksum();
            builder
                .append_data(&mut header, path, bytes.as_slice())
                .unwrap();
        }
        builder.into_inner().unwrap().finish().unwrap()
    }

    struct Placed {
        _dir: tempfile::TempDir,
        root: PathBuf,
        task: Uuid,
        relative: String,
        size: u64,
        sha: String,
    }

    fn place(bytes: &[u8]) -> Placed {
        let dir = tempfile::tempdir().unwrap();
        let task = Uuid::new_v4();
        let relative = format!("tasks/{task}/attempts/1-1/exports/payload");
        let path = dir.path().join(&relative);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, bytes).unwrap();
        Placed {
            root: dir.path().to_path_buf(),
            task,
            relative,
            size: bytes.len() as u64,
            sha: sha(bytes),
            _dir: dir,
        }
    }

    fn digest(p: &Placed) -> ResultDigest {
        build(&p.root, p.task, &p.relative, p.size, &p.sha)
    }

    const INTERVALS: &[u8] =
        b"\"supp\",\"mean\",\"lower\",\"upper\"\n\"OJ\",20.66,18.9,22.4\n\"VC\",16.96,14.8,19.1\n";

    #[test]
    fn a_bootstrap_package_yields_its_small_tables_figures_and_parameters_and_never_its_inputs() {
        let resamples: Vec<u8> = std::iter::once("\"supp\",\"mean\"\n".to_owned())
            .chain((0..2000).map(|i| format!("\"OJ\",{i}.5\n")))
            .collect::<String>()
            .into_bytes();
        let p = place(&package(
            &[
                (
                    "input/reference/inputs/observations.csv",
                    b"\"len\",\"supp\"\n4.2,\"VC\"\n",
                ),
                ("results/bootstrap_intervals.csv", INTERVALS),
                ("results/bootstrap_intervals.pdf", b"%PDF-1.4"),
                ("results/bootstrap_resamples.csv", &resamples),
                ("results/descriptive/summary.csv", b"\"\",\"n\"\n\"1\",60\n"),
                (
                    "run_parameters.json",
                    br#"{"resamples":2000,"seed":20260922}"#,
                ),
                ("method.txt", b"Percentile bootstrap."),
            ],
            None,
        ));
        let d = digest(&p);
        assert_eq!(d.status, "summarized");
        let v = &d.value;
        assert_eq!(v["schema"], SCHEMA);
        assert_eq!(v["package"]["all_matched"], true);
        let tables = v["tables"].as_array().unwrap();
        let paths: Vec<&str> = tables.iter().map(|t| t["path"].as_str().unwrap()).collect();
        assert_eq!(
            paths,
            [
                "results/bootstrap_intervals.csv",
                "results/bootstrap_resamples.csv",
                "results/descriptive/summary.csv"
            ]
        );
        assert_eq!(
            tables[0]["columns"],
            json!(["supp", "mean", "lower", "upper"])
        );
        assert_eq!(
            tables[0]["cells"],
            json!([
                ["OJ", "20.66", "18.9", "22.4"],
                ["VC", "16.96", "14.8", "19.1"]
            ])
        );
        assert_eq!(tables[0]["rows"], 2);
        assert_eq!(tables[0]["sha256"], sha(INTERVALS));
        assert_eq!(tables[1]["rows"], 2000);
        assert_eq!(tables[1]["cells_omitted"], "too_large");
        assert!(tables[1].get("cells").is_none());
        // R writes row names under an empty header.
        assert_eq!(tables[2]["columns"], json!(["column_1", "n"]));
        assert_eq!(
            v["figures"],
            json!([{"path": "results/bootstrap_intervals.pdf", "sha256": sha(b"%PDF-1.4")}])
        );
        assert_eq!(
            v["parameters"],
            json!({"resamples": 2000, "seed": 20260922})
        );
        assert!(!serde_json::to_string(v).unwrap().contains("observations"));
        assert!(stored_bytes(v) <= MAX_DIGEST_BYTES);
    }

    #[test]
    fn the_backfill_writes_the_workers_digest_for_each_named_package_and_refuses_malformed_lines() {
        let p = place(&package(
            &[("results/bootstrap_intervals.csv", INTERVALS)],
            None,
        ));
        let line = json!({"task_id": p.task, "relative_path": p.relative, "size_bytes": p.size, "sha256": p.sha});
        let outside = json!({"task_id": Uuid::new_v4(), "relative_path": p.relative, "size_bytes": p.size, "sha256": p.sha});
        let mut out = Vec::new();
        let n = digest_packages(
            &p.root,
            format!("{line}\n\n{outside}\n").as_bytes(),
            &mut out,
        )
        .unwrap();
        assert_eq!(n, 2);
        let rows: Vec<Value> = String::from_utf8(out)
            .unwrap()
            .lines()
            .map(|l| serde_json::from_str(l).unwrap())
            .collect();
        let expected = digest(&p);
        assert_eq!(rows[0]["status"], expected.status);
        assert_eq!(rows[0]["digest"], expected.value);
        assert_eq!(rows[0]["artifact_sha256"], p.sha.as_str());
        assert_eq!(rows[0]["task_id"], p.task.to_string());
        // A path outside the named task's folder is not read.
        assert_eq!(rows[1]["status"], "unreadable");
        for bad in [
            json!({"task_id": p.task, "relative_path": p.relative, "size_bytes": p.size, "sha256": p.sha.to_uppercase()}),
            json!({"task_id": p.task, "relative_path": p.relative, "size_bytes": p.size, "sha256": p.sha, "extra": 1}),
            json!({"task_id": "not-a-uuid", "relative_path": p.relative, "size_bytes": p.size, "sha256": p.sha}),
        ] {
            assert!(digest_packages(&p.root, format!("{bad}\n").as_bytes(), Vec::new()).is_err());
        }
        let many = format!("{line}\n").repeat(101);
        assert!(digest_packages(&p.root, many.as_bytes(), io::sink()).is_err());
    }

    #[test]
    fn a_prepared_lab_table_is_listed_without_its_cells() {
        let p = place(&package(
            &[
                (
                    "results/lab_data/gtt.csv",
                    b"\"mouse_id\",\"glucose\"\nM-1,142\nM-2,NA\n",
                ),
                ("results/summary.csv", b"\"n\"\n2\n"),
                (
                    "results/lab_derived/results.csv",
                    b"\"mouse_id\",\"auc\"\nM-7,1820\n",
                ),
            ],
            None,
        ));
        let d = digest(&p);
        let tables = d.value["tables"].as_array().unwrap();
        let lab = tables
            .iter()
            .find(|t| t["path"] == "results/lab_data/gtt.csv")
            .unwrap();
        assert_eq!(lab["columns"], json!(["mouse_id", "glucose"]));
        assert_eq!(lab["rows"], 2);
        assert_eq!(lab["missing"], json!({"glucose": 1}));
        assert_eq!(lab["cells_omitted"], "lab_data");
        assert!(lab.get("cells").is_none());
        // An analysis's rows computed from the lab's data are listed the same way.
        let derived = tables
            .iter()
            .find(|t| t["path"] == "results/lab_derived/results.csv")
            .unwrap();
        assert_eq!(
            (
                &derived["columns"],
                &derived["rows"],
                &derived["cells_omitted"]
            ),
            (
                &json!(["mouse_id", "auc"]),
                &json!(1),
                &json!("lab_derived")
            )
        );
        assert!(derived.get("cells").is_none());
        assert!(!d.value.to_string().contains("M-1"));
        assert!(!d.value.to_string().contains("M-7"));
        let other = tables
            .iter()
            .find(|t| t["path"] == "results/summary.csv")
            .unwrap();
        assert_eq!(other["cells"], json!([["2"]]));
    }

    #[test]
    fn a_member_that_disagrees_with_the_checksum_list_is_left_out_and_marked() {
        let p = place(&package(
            &[
                ("results/a.csv", b"\"x\"\n1\n"),
                ("results/b.csv", b"\"y\"\n2\n"),
            ],
            Some("results/b.csv"),
        ));
        let d = digest(&p);
        assert_eq!(d.status, "summarized");
        assert_eq!(d.value["package"]["all_matched"], false);
        let paths: Vec<&str> = d.value["tables"]
            .as_array()
            .unwrap()
            .iter()
            .map(|t| t["path"].as_str().unwrap())
            .collect();
        assert_eq!(paths, ["results/a.csv"]);
    }

    #[test]
    fn hostile_cells_are_cleaned_bounded_or_withheld() {
        let long = format!("\"x\"\n\"{}\"\n", "a".repeat(161));
        let name = format!("\"x\"\n\"{}\"\n", "a".repeat(160));
        // A header with more fields than a record may hold, and a row with more text, leave their tables unreadable
        // here; the rest of each member is still read and checked.
        let wide = format!("{}\n1\n", ",".repeat(MAX_RECORD_FIELDS));
        let huge = format!("\"x\"\n\"{}\"\n2\n", "b".repeat(MAX_RECORD_BYTES + 1));
        let p = place(&package(
            &[
                ("results/control.csv", b"\"x\"\n\"line\nbreak\ttab\"\n"),
                ("results/long.csv", long.as_bytes()),
                ("results/name.csv", name.as_bytes()),
                ("results/dupes.csv", b"\"a\",\"a\"\n1,2\n"),
                ("results/ragged.csv", b"\"a\",\"b\"\n1\n"),
                ("results/wide.csv", wide.as_bytes()),
                ("results/huge.csv", huge.as_bytes()),
            ],
            None,
        ));
        let d = digest(&p);
        let by_path: HashMap<String, Value> = d.value["tables"]
            .as_array()
            .unwrap()
            .iter()
            .map(|t| (t["path"].as_str().unwrap().to_owned(), t.clone()))
            .collect();
        assert_eq!(
            by_path["results/control.csv"]["cells"],
            json!([["line break tab"]])
        );
        assert_eq!(by_path["results/long.csv"]["cells_omitted"], "long_cells");
        // A food name of 160 characters is kept as written (SR Legacy's longest is 134).
        assert_eq!(
            by_path["results/name.csv"]["cells"],
            json!([["a".repeat(160)]])
        );
        assert_eq!(
            by_path["results/dupes.csv"]["cells_omitted"],
            "duplicate_columns"
        );
        assert_eq!(
            by_path["results/ragged.csv"]["cells_omitted"],
            "irregular_rows"
        );
        assert_eq!(by_path["results/wide.csv"]["cells_omitted"], "unreadable");
        assert!(by_path["results/wide.csv"].get("columns").is_none());
        assert_eq!(by_path["results/huge.csv"]["cells_omitted"], "unreadable");
        assert_eq!(by_path["results/huge.csv"]["columns"], json!(["x"]));
        assert_eq!(
            d.status, "summarized",
            "an unreadable table leaves the package readable"
        );
    }

    #[test]
    fn a_package_that_cannot_be_read_still_gets_a_named_status() {
        let p = place(b"not a gzip archive");
        assert_eq!(digest(&p).status, "not_a_package");
        let q = place(&package(&[("results/a.csv", b"\"x\"\n1\n")], None));
        assert_eq!(
            build(&q.root, q.task, &q.relative, q.size, &sha(b"different")).status,
            "integrity_failed"
        );
        assert_eq!(
            build(&q.root, Uuid::new_v4(), &q.relative, q.size, &q.sha).status,
            "unreadable"
        );
        assert_eq!(
            build(&q.root, q.task, "../escape", q.size, &q.sha).status,
            "unreadable"
        );
        let r = place(&package(&[("results/figure.pdf", b"%PDF")], None));
        let d = digest(&r);
        assert_eq!(d.status, "no_result_tables");
        assert_eq!(d.value["figures"].as_array().unwrap().len(), 1);
        for status in [
            "not_a_package",
            "integrity_failed",
            "unreadable",
            "no_result_tables",
        ] {
            assert_eq!(gap(status).value["status"], status);
        }
    }

    #[test]
    fn each_table_counts_the_rows_that_lack_a_value_in_each_column() {
        // A query writes NULL as an empty field and R writes NA; both count, over every row, also
        // when the cells are withheld. A column every row fills is not listed.
        let long: Vec<u8> = std::iter::once("\"food\",\"kcal\",\"vitamin_c\"\n".to_owned())
            .chain((0..250).map(|i| {
                let kcal = if i % 10 == 0 {
                    String::new()
                } else {
                    format!("{i}")
                };
                let vc = if i % 4 == 0 {
                    "NA".to_owned()
                } else {
                    format!("{i}.5")
                };
                format!("\"Food {i}\",{kcal},{vc}\n")
            }))
            .collect::<String>()
            .into_bytes();
        let p = place(&package(
            &[
                (
                    "results/short.csv",
                    b"\"food\",\"amount\",\"note\"\n\"A\",,\"\"\n\"B\",2.5,NA\n\"C\",3,\"x\"\n",
                ),
                ("results/long.csv", &long),
                ("results/full.csv", INTERVALS),
                ("results/dupes.csv", b"\"a\",\"a\"\n,1\n"),
            ],
            None,
        ));
        let d = digest(&p);
        let by_path: HashMap<&str, &Value> = d.value["tables"]
            .as_array()
            .unwrap()
            .iter()
            .map(|t| (t["path"].as_str().unwrap(), t))
            .collect();
        assert_eq!(
            by_path["results/short.csv"]["missing"],
            json!({"amount": 1, "note": 2})
        );
        let long = by_path["results/long.csv"];
        assert_eq!(long["cells_omitted"], "too_many_rows");
        assert_eq!(long["missing"], json!({"kcal": 25, "vitamin_c": 63}));
        assert!(by_path["results/full.csv"].get("missing").is_none());
        assert!(by_path["results/dupes.csv"].get("missing").is_none());
    }

    #[test]
    fn a_table_of_a_hundred_rows_keeps_its_cells() {
        // One suite goal's answer is 104 rows in under 8 KiB; at 50 rows its cells were withheld, so the
        // workspace showed no value and no answer could be read from it. Past 200 rows they are withheld again.
        let table = |rows: usize| -> Vec<u8> {
            std::iter::once("\"food\",\"potassium\",\"sodium\"\n".to_owned())
                .chain((0..rows).map(|i| format!("\"Vegetable {i}\",{},{}\n", 300 + i, i % 50)))
                .collect::<String>()
                .into_bytes()
        };
        let (hundred, many) = (table(104), table(201));
        let p = place(&package(
            &[
                ("results/hundred.csv", hundred.as_slice()),
                ("results/many.csv", many.as_slice()),
            ],
            None,
        ));
        let d = digest(&p);
        let by_path: HashMap<&str, &Value> = d.value["tables"]
            .as_array()
            .unwrap()
            .iter()
            .map(|t| (t["path"].as_str().unwrap(), t))
            .collect();
        let hundred = by_path["results/hundred.csv"];
        assert_eq!(hundred["rows"], 104);
        assert_eq!(hundred["cells"].as_array().unwrap().len(), 104, "{hundred}");
        assert_eq!(hundred["cells"][103], json!(["Vegetable 103", "403", "3"]));
        assert_eq!(
            by_path["results/many.csv"]["cells_omitted"],
            "too_many_rows"
        );
        assert!(d.value.to_string().len() <= MAX_DIGEST_BYTES);
    }

    #[test]
    fn a_record_shorter_than_its_header_lacks_its_last_columns() {
        // A short record once counted only the fields it had.
        let p = place(&package(
            &[("results/short.csv", b"a,b,c\n1,,x\n2\n".as_slice())],
            None,
        ));
        let d = digest(&p);
        let t = &d.value["tables"][0];
        assert_eq!(t["missing"], json!({"b": 2, "c": 1}), "{t}");
        assert_eq!(t["cells_omitted"], "irregular_rows");
    }

    #[test]
    fn a_wide_table_within_its_bytes_keeps_its_cells() {
        // A test run: the reviewed normalization's per-food table, 5 rows in 19 columns, kept no cells under a 16-column
        // limit. Up to 32 columns keep them while the table stays within 8 KiB; a 33rd column or a larger table does not.
        let table = |columns: usize, rows: usize| {
            let header: Vec<String> = (0..columns)
                .map(|c| format!("value_{c:02}_per_portion"))
                .collect();
            let row: Vec<String> = (0..columns).map(|c| format!("{c}.5")).collect();
            format!(
                "{}\n{}",
                header.join(","),
                format!("{}\n", row.join(",")).repeat(rows)
            )
        };
        for (columns, rows, kept) in [
            (19, 5, true),
            (32, 5, true),
            (33, 5, false),
            (32, 120, false),
        ] {
            let bytes = table(columns, rows);
            let p = place(&package(&[("results/results.csv", bytes.as_bytes())], None));
            let t = &digest(&p).value["tables"][0];
            assert_eq!(t["columns"].as_array().unwrap().len(), columns);
            assert_eq!(t["rows"], rows);
            if kept {
                assert_eq!(
                    t["cells"].as_array().unwrap().len(),
                    rows,
                    "{columns} columns"
                );
                assert_eq!(t["cells"][0][18], "18.5");
            } else {
                assert!(bytes.len() as u64 > INLINE_BYTES || columns > INLINE_COLUMNS);
                assert_eq!(
                    t["cells_omitted"], "too_large",
                    "{columns} columns, {rows} rows"
                );
            }
        }
    }

    #[test]
    fn missing_values_are_counted_only_in_the_listed_columns() {
        // Every header position was visited for every record although only 64 counts are kept. A
        // header of 100,000 columns over 20,000 one-field records visits 1.28 million counters, not 2 billion.
        let header: Vec<String> = (0..100_000).map(|c| format!("c{c}")).collect();
        let table = format!("{}\n{}", header.join(","), "1\n".repeat(20_000));
        let p = place(&package(&[("results/wide.csv", table.as_bytes())], None));
        let started = std::time::Instant::now();
        let d = digest(&p);
        assert!(
            started.elapsed() < std::time::Duration::from_secs(5),
            "{:?}",
            started.elapsed()
        );
        let t = &d.value["tables"][0];
        assert_eq!(t["rows"], 20_000);
        let counts = t["missing"].as_object().unwrap();
        assert_eq!(
            counts.len(),
            MAX_LISTED_COLUMNS - 1,
            "c0 has a value in every row"
        );
        assert_eq!(counts["c1"], 20_000);
    }

    #[test]
    fn parameters_holding_a_nul_character_are_left_out() {
        // PostgreSQL's jsonb refuses a NUL character, which would make the whole digest unstorable.
        let p = place(&package(
            &[
                ("results/a.csv", b"x\n1\n".as_slice()),
                (
                    "run_parameters.json",
                    br#"{"note":"a\u0000b","alpha":0.05}"#.as_slice(),
                ),
            ],
            None,
        ));
        let d = digest(&p);
        assert!(d.value.get("parameters").is_none(), "{}", d.value);
        let p = place(&package(
            &[
                ("results/a.csv", b"x\n1\n".as_slice()),
                (
                    "run_parameters.json",
                    br#"{"note":"ab","alpha":0.05}"#.as_slice(),
                ),
            ],
            None,
        ));
        assert_eq!(digest(&p).value["parameters"]["alpha"], 0.05);
    }

    #[test]
    fn stored_bytes_are_what_postgresql_prints() {
        let cases = [
            (
                json!({"a": [1, "x\n\u{1}é"], "b": {}, "c": [], "d": null, "e": false}),
                r#"{"a": [1, "x\n\u0001é"], "b": {}, "c": [], "d": null, "e": false}"#.to_owned(),
            ),
            (json!(-0.0), "0.0".to_owned()),
            (json!(123.0), "123.0".to_owned()),
            (json!(0.05), "0.05".to_owned()),
            (json!(1e-7), "0.0000001".to_owned()),
            (json!(1e21), format!("1{}", "0".repeat(21))),
            (json!(1.5e300), format!("15{}", "0".repeat(299))),
            (json!(-25), "-25".to_owned()),
        ];
        for (value, printed) in cases {
            assert_eq!(stored_bytes(&value), printed.len(), "{value}");
        }
    }

    #[test]
    fn the_bound_holds_for_the_text_postgresql_stores() {
        // A digest within the bound as compact JSON once passed it as stored, with a space after
        // every `:` and `,`.
        let tables: Vec<(String, Vec<u8>)> = (0..12)
            .map(|t| {
                let names: Vec<String> = (0..16)
                    .map(|c| format!("\"t{t}_c{c}_{}\"", "x".repeat(18 + t)))
                    .collect();
                let empty = vec![""; 16].join(",");
                (
                    format!("results/t{t:02}.csv"),
                    format!("{}\n{empty}\n", names.join(",")).into_bytes(),
                )
            })
            .collect();
        let members: Vec<(&str, &[u8])> = tables
            .iter()
            .map(|(p, b)| (p.as_str(), b.as_slice()))
            .collect();
        let p = place(&package(&members, None));
        let d = digest(&p);
        assert!(
            stored_bytes(&d.value) <= MAX_DIGEST_BYTES,
            "{}",
            stored_bytes(&d.value)
        );
        assert_eq!(d.value["tables"].as_array().unwrap().len(), 12);
    }

    #[test]
    fn each_column_is_summarized_over_every_row() {
        // 250 rows, more than the 200 whose cells a digest keeps: the summary still covers every row.
        let mut csv = String::from("id,name,amount,flag,empty\n");
        for i in 0..250_i64 {
            let name = if i % 2 == 0 { "a" } else { "b" };
            let flag = if i == 7 { "Inf" } else { "1" };
            csv += &format!("{},{name},{}.5,{flag},\n", i + 1, i - 100);
        }
        let lab = b"id,value\n1,2.5\n2,-3\n";
        let p = place(&package(
            &[
                ("results/table.csv", csv.as_bytes()),
                ("results/lab_derived/rows.csv", lab.as_slice()),
            ],
            None,
        ));
        let d = digest(&p);
        let table = |path: &str| {
            d.value["tables"]
                .as_array()
                .unwrap()
                .iter()
                .find(|t| t["path"] == path)
                .unwrap()
                .clone()
        };
        let t = table("results/table.csv");
        assert_eq!(t["cells_omitted"], "too_many_rows");
        assert_eq!(
            t["summary"],
            json!({
                "id": {"numeric": true, "min": "1", "max": "250", "distinct": 250},
                "name": {"numeric": false, "distinct": 2},
                "amount": {"numeric": true, "min": "-100.5", "max": "149.5", "distinct": 250},
                "flag": {"numeric": false, "distinct": 2},
                "empty": {"numeric": false, "distinct": 0}
            })
        );
        assert_eq!(t["missing"], json!({"empty": 250}));
        // A lab's own table keeps its counts but none of its values.
        let l = table("results/lab_derived/rows.csv");
        assert_eq!(
            l["summary"],
            json!({"id": {"numeric": true, "distinct": 2}, "value": {"numeric": true, "distinct": 2}})
        );
    }

    #[test]
    fn a_table_with_repeated_column_names_has_no_counts_or_summary() {
        let p = place(&package(
            &[("results/t.csv", b"a,a\n1,\n2,3\n".as_slice())],
            None,
        ));
        let d = digest(&p);
        let t = &d.value["tables"][0];
        assert_eq!(t["columns"], json!(["a", "a"]));
        assert!(t.get("missing").is_none() && t.get("summary").is_none());
        assert!(t.get("missing_omitted").is_none() && t.get("summary_omitted").is_none());
    }

    #[test]
    fn distinct_values_are_counted_up_to_their_limit() {
        let mut csv = String::from("n\n");
        for i in 0..=DISTINCT_MAX {
            csv += &format!("{i}\n");
        }
        let p = place(&package(&[("results/n.csv", csv.as_bytes())], None));
        let d = digest(&p);
        let n = &d.value["tables"][0]["summary"]["n"];
        assert_eq!(n["distinct_capped"], true);
        assert!(n.get("distinct").is_none());
        assert_eq!(
            (n["min"].as_str(), n["max"].as_str()),
            (Some("0"), Some("20000"))
        );
    }

    #[test]
    fn a_number_is_a_finite_decimal() {
        for (text, value) in [
            ("1e5", Some(1e5)),
            ("+3", Some(3.0)),
            (".5", Some(0.5)),
            ("1.", Some(1.0)),
            ("-0.25", Some(-0.25)),
            ("6.12498179243933e-29", Some(6.12498179243933e-29)),
        ] {
            assert_eq!(finite_number(text.as_bytes()), value, "{text}");
        }
        for text in [
            "Inf", "-Inf", "NaN", "0x10", "1,000", "1e", ".", "-", "1.2.3", "1e999", "TRUE",
        ] {
            assert_eq!(finite_number(text.as_bytes()), None, "{text}");
        }
    }

    #[test]
    fn missing_counts_give_way_before_a_table_does() {
        // Twelve tables whose counts alone pass the bound: every column summary goes first, then the largest counts,
        // and every table stays.
        let tables: Vec<(String, Vec<u8>)> = (0..12)
            .map(|t| {
                let names: Vec<String> = (0..16)
                    .map(|c| format!("\"t{t}_c{c}_{}\"", "x".repeat(44 + t)))
                    .collect();
                let empty = vec![""; 16].join(",");
                (
                    format!("results/t{t:02}.csv"),
                    format!("{}\n{empty}\n", names.join(",")).into_bytes(),
                )
            })
            .collect();
        let members: Vec<(&str, &[u8])> = tables
            .iter()
            .map(|(p, b)| (p.as_str(), b.as_slice()))
            .collect();
        let p = place(&package(&members, None));
        let d = digest(&p);
        let v = &d.value;
        assert!(stored_bytes(v) <= MAX_DIGEST_BYTES);
        let tables = v["tables"].as_array().unwrap();
        assert_eq!(tables.len(), 12, "{}", v["tables_omitted"]);
        let gave_way: Vec<usize> = tables
            .iter()
            .enumerate()
            .filter(|(_, t)| t["missing_omitted"] == "digest_budget")
            .map(|(i, _)| i)
            .collect();
        assert!(!gave_way.is_empty());
        assert!(
            tables
                .iter()
                .all(|t| t.get("summary").is_none() && t["summary_omitted"] == "digest_budget")
        );
        // The largest counts (the longest names) give way first.
        assert!(gave_way.iter().all(|i| tables[*i].get("missing").is_none()));
        assert!(tables[11].get("missing").is_none() && tables[0].get("missing").is_some());
    }

    #[test]
    fn a_digest_that_would_exceed_its_bound_drops_the_largest_cells_first() {
        let header: Vec<String> = (0..16).map(|c| format!("\"c{c}\"")).collect();
        let table = |width: usize| -> Vec<u8> {
            std::iter::once(header.join(",") + "\n")
                .chain((0..50).map(|r| {
                    (0..16)
                        .map(|c| format!("{:0w$}", r * 16 + c, w = width))
                        .collect::<Vec<_>>()
                        .join(",")
                        + "\n"
                }))
                .collect::<String>()
                .into_bytes()
        };
        let (wide, narrow) = (table(6), table(5));
        let p = place(&package(
            &[
                ("results/a.csv", &narrow),
                ("results/b.csv", &wide),
                ("results/c.csv", &narrow),
                ("results/d.csv", INTERVALS),
            ],
            None,
        ));
        let d = digest(&p);
        assert!(stored_bytes(&d.value) <= MAX_DIGEST_BYTES);
        let tables = d.value["tables"].as_array().unwrap();
        assert_eq!(tables.len(), 4);
        // The widest cells go first; the small answer table keeps its cells.
        assert_eq!(tables[1]["cells_omitted"], "digest_budget");
        assert!(tables[3].get("cells").is_some());
        assert_eq!(tables[1]["rows"], 50);
    }
}
