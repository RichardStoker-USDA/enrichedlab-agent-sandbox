//! The profile of a lab dataset (the `parsed` state). For each sheet of the upload guard's cell grid it finds
//! the table: title rows, one or two header rows, a row of units, the data, summary rows (`Mean`, `SD`) and notes below.
//! It names each column with its unit and type and lists what a person should decide before the data can be analyzed.
//! It is deterministic: it reads the grid twice, holding one row and bounded counts at a time, and the same grid always
//! gives the same profile.
//!
//! There are two parts, both read by the project's people only. The profile holds sheet names, the text of the rows the
//! layout takes as header and units rows as column names and units, types, counts, row numbers and issue codes. Names,
//! units and types are guesses until a person approves a preparation, and each column says where its name and unit
//! came from: a header found in the wrong row (a table of text only takes its first full row of text) puts a data row's
//! text among the names, so a model is given names only from an approved preparation. The detail holds each sheet's
//! first rows, title and note text, each text column's most frequent values, each number column's smallest and
//! largest, and a few cells for each issue.
use std::collections::{BTreeMap, HashMap, HashSet};
use std::io::Read;
use std::sync::LazyLock;

use regex::Regex;
use serde::Serialize;
use sha2::{Digest, Sha256};

use super::cell_grid::{Cell, GridError, GridReader, Line, Sheet, Source, column_letters};

pub const SCHEMA: &str = "enrichedlab.dataset-profile.v1";
pub const DETAIL_SCHEMA: &str = "enrichedlab.dataset-profile-detail.v1";

/// Rows read in full to find a sheet's header; the detail shows the first 30 of them, 30 columns wide.
const HEAD_ROWS: usize = 40;
const PREVIEW_ROWS: usize = 30;
const PREVIEW_COLUMNS: usize = 30;
/// Columns profiled in one sheet.
pub const MAX_COLUMNS: usize = 512;
/// Row shapes a profile holds over all sheets (about 12 bytes each).
const MAX_PROFILE_ROWS: usize = 4 * 1024 * 1024;
/// Distinct texts counted per column, in at most this many bytes.
const MAX_DISTINCT: usize = 2_000;
const MAX_DISTINCT_BYTES: usize = 64 * 1024;
/// Identifier columns checked for repeats per sheet, and identifiers held per column.
const MAX_ID_COLUMNS: usize = 4;
const MAX_IDS: usize = 250_000;
const MAX_EXAMPLES: usize = 5;
const MAX_TOP_VALUES: usize = 20;
/// Row numbers listed for summary and note rows; the issue counts every one.
const MAX_LISTED_ROWS: usize = 200;
/// A cell's text kept in the detail is cut at this many characters.
const MAX_SHOWN_CHARS: usize = 120;
/// A column is of one type when at least this share of its cells with a value are.
const DOMINANT: f64 = 0.8;

/// The grid's bounds for the reader: a line, and all lines together, once decompressed. The guard writes at most 256 MiB.
pub const MAX_GRID_LINE: usize = 64 * 1024 * 1024;
pub const MAX_GRID_BYTES: u64 = 256 * 1024 * 1024 + 64 * 1024;

#[derive(Clone, Debug, Serialize, PartialEq)]
pub struct Profile {
    pub schema: &'static str,
    pub source: SourceSummary,
    pub sheets: Vec<SheetProfile>,
}

#[derive(Clone, Debug, Serialize, PartialEq)]
pub struct SourceSummary {
    #[serde(rename = "type")]
    pub kind: String,
    pub sha256: String,
    pub bytes: u64,
    pub encoding: Option<String>,
    pub delimiter: Option<String>,
}

#[derive(Clone, Debug, Serialize, PartialEq)]
pub struct SheetProfile {
    pub index: usize,
    pub name: Option<String>,
    pub hidden: bool,
    pub row_count: u64,
    pub column_count: u64,
    /// None for a sheet with no filled cell.
    pub table: Option<TableShape>,
    pub columns: Vec<ColumnProfile>,
    /// Columns past the profiled 512.
    pub columns_omitted: u64,
    pub issues: Vec<Issue>,
}

/// Where the table sits, in row numbers as a spreadsheet shows them (the first row is 1).
#[derive(Clone, Debug, Serialize, PartialEq)]
pub struct TableShape {
    /// One row, two when a second row names the columns under a heading that spans them, or none.
    pub header_rows: Vec<u64>,
    /// `labels_over_numbers` when a header cell is text over a column of numbers; `first_text_row` for a table of text
    /// only, where the first full row of text is taken; none without a header.
    pub header_basis: Option<&'static str>,
    pub units_row: Option<u64>,
    pub title_rows: Vec<u64>,
    pub first_data_row: u64,
    pub last_data_row: u64,
    /// Rows between the first and last data row that are neither blank nor summary rows.
    pub data_rows: u64,
    pub blank_rows: u64,
    pub summary_rows: Vec<u64>,
    pub note_rows: Vec<u64>,
}

#[derive(Clone, Debug, Serialize, PartialEq)]
pub struct ColumnProfile {
    pub index: usize,
    pub letter: String,
    /// The header's text without its unit, or the column letter when there is none.
    pub name: String,
    /// Where `name` came from: `header`, the text of the row or rows the layout took as the header, or `letter` when
    /// the column has none. A header is a guess from the sheet's layout until a person approves a preparation; a wrong
    /// one puts a data row's text here.
    pub name_from: &'static str,
    /// A name R and the analyses take as written: lower case, letters, digits and underscores, unique in the sheet.
    pub suggested_name: String,
    pub unit: Option<String>,
    /// Where `unit` came from, a guess like `name_from`: `units_row`, the row under the header, or `header`, written
    /// after the name in the header cell.
    pub unit_from: Option<&'static str>,
    /// The type most present cells read as when they are four in five or more, else `mixed`; `other` counts the cells
    /// that do not read as it. A guess from the cells' look, as a date may be only by its form.
    #[serde(rename = "type")]
    pub kind: &'static str,
    /// `day_first`, `month_first` or `ambiguous` for dates written as text with slashes, dots or dashes.
    pub date_order: Option<&'static str>,
    /// Cells with a value: not empty, not a missing-value token, not censored, not a spreadsheet error.
    pub present: u64,
    /// Empty cells (`""`) and each missing-value token, by the token.
    pub missing: BTreeMap<String, u64>,
    /// Values reported against a limit: `below_limit`, `above_limit`, `not_detected`.
    pub censored: BTreeMap<&'static str, u64>,
    /// Cells with a value that are not of the column's type.
    pub other: u64,
    /// Different texts in a text column, when at most 2,000.
    pub distinct: Option<u64>,
    pub issues: Vec<&'static str>,
}

#[derive(Clone, Debug, Serialize, PartialEq)]
pub struct Issue {
    pub code: &'static str,
    pub columns: Vec<String>,
    pub count: u64,
}

#[derive(Clone, Debug, Serialize, PartialEq)]
pub struct Detail {
    pub schema: &'static str,
    pub sheets: Vec<SheetDetail>,
}

#[derive(Clone, Debug, Serialize, PartialEq)]
pub struct SheetDetail {
    pub index: usize,
    /// The sheet's first rows as text, each cut after its last filled cell.
    pub preview: Vec<Vec<String>>,
    pub title_text: Vec<String>,
    pub note_text: Vec<String>,
    pub columns: Vec<ColumnDetail>,
}

#[derive(Clone, Debug, Serialize, PartialEq)]
pub struct ColumnDetail {
    pub letter: String,
    /// The most frequent texts with their counts, for a text column.
    pub values: Vec<(String, u64)>,
    pub min: Option<String>,
    pub max: Option<String>,
    /// A few cells for each issue the column has.
    pub examples: BTreeMap<&'static str, Vec<String>>,
}

/// Profile a grid read from `open()` twice: once for each sheet's shape, once for its columns.
pub fn profile<R: Read>(
    open: impl FnMut() -> std::io::Result<R>,
) -> Result<(Profile, Detail), GridError> {
    profile_within(open, MAX_PROFILE_ROWS)
}

/// `profile`, holding the shapes of at most `max_rows` rows over all sheets: past it the grid is too large to profile
/// here. A shape is a few bytes, but a grid of blank rows is cheap to send.
fn profile_within<R: Read>(
    mut open: impl FnMut() -> std::io::Result<R>,
    max_rows: usize,
) -> Result<(Profile, Detail), GridError> {
    let mut reader = || {
        GridReader::open(
            open().map_err(|_| GridError::Unreadable)?,
            MAX_GRID_LINE,
            MAX_GRID_BYTES,
        )
    };
    let mut first = reader()?;
    let source = first.source.clone();
    let mut shapes: Vec<Shaped> = source.sheets.iter().map(|_| Shaped::default()).collect();
    let (mut sheet, mut held) = (None, 0_usize);
    while let Some(line) = first.next_line()? {
        match line {
            Line::Sheet(index) => sheet = Some(index),
            Line::Row(row) => {
                if let Some(index) = sheet {
                    held += 1;
                    if held > max_rows {
                        return Err(GridError::TooLarge);
                    }
                    shapes[index].push(&row);
                }
            }
        }
    }
    let plans: Vec<Option<Plan>> = shapes.iter().map(Shaped::plan).collect();
    let mut counts: Vec<Vec<Counter>> = plans
        .iter()
        .map(|plan| {
            let Some(plan) = plan else { return Vec::new() };
            let mut id_columns = 0;
            (0..plan.width.min(MAX_COLUMNS))
                .map(|c| {
                    let id = id_columns < MAX_ID_COLUMNS && plan.id_like(c);
                    id_columns += usize::from(id);
                    Counter::new(id)
                })
                .collect()
        })
        .collect();
    let mut second = reader()?;
    let (mut sheet, mut row_number) = (None, 0_u64);
    while let Some(line) = second.next_line()? {
        match line {
            Line::Sheet(index) => (sheet, row_number) = (Some(index), 0),
            Line::Row(row) => {
                row_number += 1;
                let Some(index) = sheet else { continue };
                let Some(plan) = &plans[index] else { continue };
                if !plan.is_data(row_number) || row.iter().all(Cell::is_blank) {
                    continue;
                }
                for (c, counter) in counts[index].iter_mut().enumerate() {
                    counter.add(row.get(c).unwrap_or(&Cell::Empty));
                }
            }
        }
    }
    Ok(assemble(&source, &shapes, &plans, counts))
}

/// A row's shape for finding the table.
#[derive(Clone, Copy, Debug, Default)]
struct RowShape {
    filled: u32,
    last: u32,
    /// For a row with one filled cell that is text, its words; else 0.
    single_words: u16,
    /// The row's first filled cell is a summary label such as `Mean` or `SD`.
    summary: bool,
}

#[derive(Default)]
struct Shaped {
    head: Vec<Vec<Cell>>,
    kinds: Vec<Vec<Kind>>,
    rows: Vec<RowShape>,
    /// Rows with one filled text cell, by row number, with their text: kept for title and note rows.
    singles: Vec<(u64, String)>,
}

impl Shaped {
    fn push(&mut self, row: &[Cell]) {
        let number = self.rows.len() as u64 + 1;
        let mut shape = RowShape::default();
        let mut only: Option<&Cell> = None;
        for (c, cell) in row.iter().enumerate() {
            if cell.is_blank() {
                continue;
            }
            if shape.filled == 0 {
                shape.summary = matches!(cell, Cell::Text(t) if is_summary_label(t));
            }
            shape.filled += 1;
            shape.last = c as u32;
            only = Some(cell);
        }
        if shape.filled == 1
            && let Some(Cell::Text(text)) = only
        {
            shape.single_words = text.split_whitespace().count().min(u16::MAX as usize) as u16;
            if self.singles.len() < 4 * HEAD_ROWS + MAX_LISTED_ROWS {
                self.singles.push((number, shown(text)));
            }
        }
        // The first rows are kept for the header and units, at most the columns a profile reads: a sparse row that
        // reaches column XFD is 16,384 cells wide.
        if self.head.len() < HEAD_ROWS {
            let kept = &row[..row.len().min(MAX_COLUMNS)];
            self.kinds.push(kept.iter().map(classify).collect());
            self.head.push(kept.to_vec());
        }
        self.rows.push(shape);
    }

    fn kind(&self, row: u64, column: usize) -> Kind {
        self.kinds
            .get(row as usize - 1)
            .and_then(|kinds| kinds.get(column))
            .copied()
            .unwrap_or(Kind::Empty)
    }

    fn cell_text(&self, row: u64, column: usize) -> String {
        self.head
            .get(row as usize - 1)
            .and_then(|cells| cells.get(column))
            .and_then(Cell::text)
            .map(|t| t.trim().to_owned())
            .unwrap_or_default()
    }

    /// Whether the cells of a column below `row`, among the first rows, are mostly numbers; none when there are none.
    fn numbers_below(&self, row: u64, column: usize) -> Option<bool> {
        let (mut numeric, mut total) = (0, 0);
        for kinds in self.kinds.iter().skip(row as usize) {
            match kinds.get(column).copied().unwrap_or(Kind::Empty) {
                Kind::Empty | Kind::Missing => {}
                kind => {
                    total += 1;
                    numeric += usize::from(kind.is_numeric());
                }
            }
        }
        (total > 0).then_some(numeric * 2 > total)
    }

    /// Where the sheet's table is; none for a sheet with no filled cell.
    fn plan(&self) -> Option<Plan> {
        let numbered = || {
            self.rows
                .iter()
                .enumerate()
                .map(|(i, s)| (i as u64 + 1, *s))
        };
        let first_filled = numbered().find(|(_, s)| s.filled > 0)?.0;
        let width = self
            .rows
            .iter()
            .filter(|s| s.filled > 0)
            .map(|s| s.last as usize + 1)
            .max()
            .unwrap_or(1);
        let min_filled = if width >= 2 { 2 } else { 1 };
        // Names, units and header kinds for the columns a profile reads; `width` stays the sheet's for the count of
        // columns left out.
        let seen = width.min(MAX_COLUMNS);
        let (header, basis) = match self.header(seen, min_filled) {
            Some((row, basis)) => (Some(row), Some(basis)),
            None => (None, None),
        };
        let second = header
            .filter(|h| !self.units_row(h + 1, seen) && self.second_header(*h, seen))
            .map(|h| h + 1);
        let last_header = second.or(header);
        let units = last_header
            .filter(|h| self.units_row(h + 1, seen))
            .map(|h| h + 1);
        let start = units.or(last_header).map_or(first_filled, |r| r + 1);
        // The last data row: the last row with two filled cells (one in a one-column table), extended by single-cell rows
        // right after it that read as data rather than notes.
        let mut last = numbered()
            .rfind(|(r, s)| *r >= start && s.filled >= min_filled)
            .map_or(start.saturating_sub(1), |(r, _)| r);
        let mut notes = Vec::new();
        let mut gap = false;
        let data_end = last;
        for (r, s) in numbered().filter(|(r, _)| *r > data_end) {
            if s.filled == 0 {
                gap = true;
            } else if !gap && notes.is_empty() && !s.summary && s.single_words < 3 {
                last = r;
            } else {
                notes.push(r);
            }
        }
        let titles: Vec<u64> = numbered()
            .filter(|(r, s)| s.filled > 0 && *r < header.unwrap_or(first_filled))
            .map(|(r, _)| r)
            .collect();
        let blank = numbered()
            .filter(|(r, s)| *r >= start && *r <= last && s.filled == 0)
            .count() as u64;
        let summary: Vec<u64> = numbered()
            .filter(|(r, s)| *r >= start && *r <= last && s.summary)
            .map(|(r, _)| r)
            .collect();
        let (names, header_units, header_kinds) = self.names(header, second, seen);
        let units_cells: Vec<Option<(String, &'static str)>> = (0..seen)
            .map(|c| {
                units
                    .map(|u| unit_text(&self.cell_text(u, c)))
                    .filter(|t| !t.is_empty())
                    .map(|t| (t, "units_row"))
                    .or_else(|| header_units[c].clone().map(|t| (t, "header")))
            })
            .collect();
        Some(Plan {
            header,
            second,
            basis,
            units,
            titles,
            first_data_row: start,
            last_data_row: last,
            blank,
            summary,
            notes,
            width,
            names,
            header_kinds,
            units_cells,
        })
    }

    /// The header row: the first of the first rows with at least two filled cells (one in a one-column table), each a
    /// label, a number or a date, where a label or a date stands over a column of numbers. In a table with no column of
    /// numbers, the first row whose filled cells are all labels and fill at least half its width.
    fn header(&self, width: usize, min_filled: u32) -> Option<(u64, &'static str)> {
        let rows = self.head.len() as u64;
        let label_row = |r: u64| {
            self.rows[r as usize - 1].filled >= min_filled
                && (0..width).all(|c| {
                    matches!(
                        self.kind(r, c),
                        Kind::Empty
                            | Kind::Text
                            | Kind::Integer
                            | Kind::Decimal
                            | Kind::Date(_)
                            | Kind::DateTime
                            | Kind::Time
                    )
                })
        };
        let over_numbers = |r: u64| {
            (0..width).any(|c| {
                matches!(self.kind(r, c), Kind::Text | Kind::Date(_) | Kind::DateTime)
                    && self.numbers_below(r, c) == Some(true)
            })
        };
        if let Some(row) = (1..=rows).find(|&r| label_row(r) && over_numbers(r)) {
            return Some((row, "labels_over_numbers"));
        }
        let any_numbers = (0..width).any(|c| self.numbers_below(0, c) == Some(true));
        if any_numbers {
            return None;
        }
        (1..=rows)
            .find(|&r| {
                label_row(r)
                    && (self.rows[r as usize - 1].filled as usize) * 2 >= width
                    && (0..width).all(|c| matches!(self.kind(r, c), Kind::Empty | Kind::Text))
            })
            .map(|row| (row, "first_text_row"))
    }

    /// Whether the row after header row `h` names the columns under a heading that spans them, as time points under
    /// `Glucose (mg/dL)`: at least two cells, all labels, numbers or dates over columns of numbers, none over a column of
    /// text, and at least one where the header row is blank.
    fn second_header(&self, h: u64, width: usize) -> bool {
        let r = h + 1;
        if r as usize > self.head.len() {
            return false;
        }
        let filled: Vec<usize> = (0..width)
            .filter(|&c| self.kind(r, c) != Kind::Empty)
            .collect();
        filled.len() >= 2
            && filled.iter().all(|&c| {
                matches!(
                    self.kind(r, c),
                    Kind::Text
                        | Kind::Integer
                        | Kind::Decimal
                        | Kind::Date(_)
                        | Kind::DateTime
                        | Kind::Time
                ) && self.numbers_below(r, c) == Some(true)
            })
            && filled.iter().any(|&c| self.kind(h, c) == Kind::Empty)
    }

    /// Whether row `r` holds units for the header above it: each filled cell reads as a unit, over a column of numbers.
    fn units_row(&self, r: u64, width: usize) -> bool {
        if r as usize > self.head.len() {
            return false;
        }
        let filled: Vec<usize> = (0..width)
            .filter(|&c| self.kind(r, c) != Kind::Empty)
            .collect();
        !filled.is_empty()
            && filled
                .iter()
                .all(|&c| self.kind(r, c) == Kind::Text && is_unit(&self.cell_text(r, c)))
            && filled
                .iter()
                .any(|&c| self.numbers_below(r, c) == Some(true))
    }

    /// Each column's name and the unit its header names, and the header cell's kind. Under a second header row a column
    /// with a blank first header row takes the heading to its left that spans it: `Glucose (mg/dL)` over `0`, `15`, `30`
    /// names them `Glucose 0`, `Glucose 15`, `Glucose 30`, each in mg/dL.
    fn names(
        &self,
        header: Option<u64>,
        second: Option<u64>,
        width: usize,
    ) -> (Vec<String>, Vec<Option<String>>, Vec<Kind>) {
        let Some(h) = header else {
            return (
                vec![String::new(); width],
                vec![None; width],
                vec![Kind::Empty; width],
            );
        };
        let mut names = Vec::with_capacity(width);
        let mut units = Vec::with_capacity(width);
        let mut kinds = Vec::with_capacity(width);
        let mut spanning = String::new();
        for c in 0..width {
            let mut top = self.cell_text(h, c);
            let below = second.map(|s| self.cell_text(s, c)).unwrap_or_default();
            if top.is_empty() {
                if second.is_some() && !below.is_empty() {
                    top = spanning.clone();
                }
            } else {
                spanning = top.clone();
            }
            let (name, unit) = name_and_unit(&top);
            let joined = [name, below.clone()]
                .into_iter()
                .filter(|part| !part.is_empty())
                .collect::<Vec<_>>()
                .join(" ");
            names.push(joined);
            units.push(unit);
            kinds.push(match second {
                Some(s) if !below.is_empty() => self.kind(s, c),
                _ => self.kind(h, c),
            });
        }
        (names, units, kinds)
    }
}

struct Plan {
    header: Option<u64>,
    second: Option<u64>,
    basis: Option<&'static str>,
    units: Option<u64>,
    titles: Vec<u64>,
    first_data_row: u64,
    last_data_row: u64,
    blank: u64,
    summary: Vec<u64>,
    notes: Vec<u64>,
    width: usize,
    names: Vec<String>,
    header_kinds: Vec<Kind>,
    /// Each column's unit and the row it came from.
    units_cells: Vec<Option<(String, &'static str)>>,
}

impl Plan {
    fn is_data(&self, row: u64) -> bool {
        row >= self.first_data_row
            && row <= self.last_data_row
            && self.summary.binary_search(&row).is_err()
    }

    /// A column that names an animal, subject or sample, whose values should not repeat.
    fn id_like(&self, column: usize) -> bool {
        static ID: LazyLock<Regex> = LazyLock::new(|| {
            Regex::new(
                r"(?i)^(?:(?:mouse|mice|animal|subject|sample|specimen|rat|participant|patient)(?:[\s_.-]*(?:id|#|no\.?|number|code))?|(?:ear[\s_-]*)?tag(?:[\s_-]*(?:id|#|no\.?|number))?|rfid|id(?:[\s_-]*(?:no\.?|number|#))?|[a-z]+[\s_-]id)$",
            )
            .unwrap()
        });
        self.names
            .get(column)
            .is_some_and(|name| ID.is_match(name.trim()))
    }

    fn header_rows(&self) -> Vec<u64> {
        self.header.into_iter().chain(self.second).collect()
    }
}

/// What one cell holds, for counting.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(super) enum Kind {
    Empty,
    Missing,
    Censored(Censor),
    Integer,
    Decimal,
    DecimalComma,
    Thousands,
    AmbiguousComma,
    Percent,
    Date(Order),
    DateTime,
    Time,
    Duration,
    Logical,
    Error,
    Text,
}

impl Kind {
    fn is_numeric(self) -> bool {
        matches!(
            self,
            Kind::Integer
                | Kind::Decimal
                | Kind::DecimalComma
                | Kind::Thousands
                | Kind::AmbiguousComma
                | Kind::Percent
                | Kind::Censored(_)
        )
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(super) enum Censor {
    Below,
    Above,
    NotDetected,
}

/// How a date's text orders its day and month.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(super) enum Order {
    /// A spreadsheet date, ISO 8601 year-month-day, or a day and month that are the same number.
    Fixed,
    DayFirst,
    MonthFirst,
    /// Day and month both 12 or less.
    Either,
}

/// The tokens labs write for a missing value, matched after leading and trailing spaces are dropped.
pub const MISSING_TOKENS: [&str; 18] = [
    "NA", "N/A", "n/a", "na", "#N/A", "NaN", "nan", "null", "NULL", "Null", "-", "--", ".", "?",
    "missing", "Missing", "MISSING", "#NULL!",
];

fn missing_token(text: &str) -> Option<&'static str> {
    let trimmed = text.trim();
    MISSING_TOKENS.iter().find(|m| **m == trimmed).copied()
}

static NUMBER: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^[-+]?(?:\d+\.?\d*|\.\d+)(?:[eE][-+]?\d+)?$").unwrap());
static INTEGER: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^[-+]?\d+$").unwrap());
static COMMA: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^[-+]?\d*,\d+$").unwrap());
static COMMA_THOUSAND: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^[-+]?\d{1,3},\d{3}$").unwrap());
static THOUSANDS: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^[-+]?\d{1,3}(?:,\d{3})+(?:\.\d+)?$").unwrap());
static POINT_THOUSANDS: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^[-+]?\d{1,3}(?:\.\d{3})+,\d+$").unwrap());
static PERCENT: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^[-+]?(?:\d+(?:[.,]\d+)?|[.,]\d+)\s*%$").unwrap());
/// A value against a limit, such as `<0.5`, `< 0,5 ng/mL` or `>2000`: the sign, the number, and a unit if any.
pub(super) static LIMIT: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"^([<>≤≥])\s*=?\s*([-+]?(?:\d+(?:[.,]\d*)?|[.,]\d+)(?:[eE][-+]?\d+)?)\s*([A-Za-zµμ%/][A-Za-zµμ%/0-9.]*)?$",
    )
    .unwrap()
});
static ISO_DATE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^\d{4}-\d{1,2}-\d{1,2}$").unwrap());
static ISO_DATETIME: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"^\d{4}-\d{1,2}-\d{1,2}[T ]\d{1,2}:\d{2}(?::\d{2}(?:\.\d+)?)?(?:Z|[+-]\d{2}:?\d{2})?$",
    )
    .unwrap()
});
pub(super) static DMY: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^(\d{1,2})[/.-](\d{1,2})[/.-](\d{4}|\d{2})$").unwrap());
static TIME: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^\d{1,2}:\d{2}(?::\d{2}(?:\.\d+)?)?(?:\s*[AaPp][Mm])?$").unwrap()
});

/// Tokens that report a value below or above what the assay measures, compared in upper case.
pub(super) fn censor_token(text: &str) -> Option<Censor> {
    const BELOW: [&str; 12] = [
        "<LOD",
        "< LOD",
        "<LOQ",
        "< LOQ",
        "<LLOQ",
        "BLQ",
        "BQL",
        "BLOQ",
        "BDL",
        "BELOW LOD",
        "BELOW DETECTION",
        "BELOW DETECTION LIMIT",
    ];
    const ABOVE: [&str; 7] = [
        ">ULOQ",
        "> ULOQ",
        "TNTC",
        "AQL",
        "ALQ",
        "ABOVE RANGE",
        ">LOQ",
    ];
    const NOT: [&str; 6] = ["ND", "N.D.", "N/D", "N.D", "NOT DETECTED", "UNDETECTED"];
    let upper = text.trim().to_uppercase();
    if BELOW.contains(&upper.as_str()) {
        Some(Censor::Below)
    } else if ABOVE.contains(&upper.as_str()) {
        Some(Censor::Above)
    } else if NOT.contains(&upper.as_str()) {
        Some(Censor::NotDetected)
    } else {
        None
    }
}

pub(super) fn classify(cell: &Cell) -> Kind {
    match cell {
        Cell::Empty => Kind::Empty,
        Cell::Number(n) => {
            if n.is_i64() || n.is_u64() {
                Kind::Integer
            } else {
                Kind::Decimal
            }
        }
        Cell::Bool(_) => Kind::Logical,
        Cell::Date(_) => Kind::Date(Order::Fixed),
        Cell::DateTime(_) => Kind::DateTime,
        Cell::Time(_) => Kind::Time,
        Cell::Seconds(_) | Cell::Duration(_) => Kind::Duration,
        Cell::Error(_) | Cell::Serial(_) => Kind::Error,
        Cell::Text(text) => classify_text(text),
    }
}

pub(super) fn classify_text(text: &str) -> Kind {
    let t = text.trim();
    if t.is_empty() {
        return Kind::Empty;
    }
    if missing_token(t).is_some() {
        return Kind::Missing;
    }
    if let Some(censor) = censor_token(t) {
        return Kind::Censored(censor);
    }
    if let Some(found) = LIMIT.captures(t) {
        return Kind::Censored(if matches!(&found[1], "<" | "≤") {
            Censor::Below
        } else {
            Censor::Above
        });
    }
    if INTEGER.is_match(t) {
        return Kind::Integer;
    }
    if NUMBER.is_match(t) || matches!(t, "Inf" | "-Inf" | "+Inf") {
        return Kind::Decimal;
    }
    if COMMA_THOUSAND.is_match(t) {
        return Kind::AmbiguousComma;
    }
    if COMMA.is_match(t) || POINT_THOUSANDS.is_match(t) {
        return Kind::DecimalComma;
    }
    if THOUSANDS.is_match(t) {
        return Kind::Thousands;
    }
    if PERCENT.is_match(t) {
        return Kind::Percent;
    }
    if ISO_DATE.is_match(t) {
        return Kind::Date(Order::Fixed);
    }
    if ISO_DATETIME.is_match(t) {
        return Kind::DateTime;
    }
    if let Some(found) = DMY.captures(t) {
        let (a, b): (u32, u32) = (found[1].parse().unwrap_or(0), found[2].parse().unwrap_or(0));
        let valid = |day: u32, month: u32| (1..=31).contains(&day) && (1..=12).contains(&month);
        return match (valid(a, b), valid(b, a)) {
            (true, true) if a == b => Kind::Date(Order::Fixed),
            (true, true) => Kind::Date(Order::Either),
            (true, false) => Kind::Date(Order::DayFirst),
            (false, true) => Kind::Date(Order::MonthFirst),
            (false, false) => Kind::Text,
        };
    }
    if TIME.is_match(t) {
        return Kind::Time;
    }
    if t.eq_ignore_ascii_case("true") || t.eq_ignore_ascii_case("false") {
        return Kind::Logical;
    }
    Kind::Text
}

/// Labels of rows that summarize the rows above them.
fn is_summary_label(text: &str) -> bool {
    const LABELS: [&str; 24] = [
        "mean", "average", "avg", "sd", "stdev", "std", "stddev", "sem", "se", "stderr", "n",
        "count", "total", "sum", "median", "min", "max", "minimum", "maximum", "cv", "%cv", "cv%",
        "variance", "var",
    ];
    let key: String = text
        .chars()
        .filter(|c| !matches!(c, '.' | ':' | ' ' | '\t'))
        .flat_map(char::to_lowercase)
        .collect();
    LABELS.contains(&key.as_str())
}

/// A header's name and the unit it names in parentheses or brackets at its end: `Body weight (g)`, `Glucose [mg/dL]`.
pub fn name_and_unit(header: &str) -> (String, Option<String>) {
    static UNIT: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r"^(.*?\S)\s*[(\[]\s*([^()\[\]]{1,24}?)\s*[)\]]\s*$").unwrap());
    match UNIT.captures(header.trim()) {
        Some(found) if is_unit(&found[2]) => (found[1].to_owned(), Some(found[2].to_owned())),
        _ => (header.trim().to_owned(), None),
    }
}

fn unit_text(text: &str) -> String {
    text.trim()
        .trim_start_matches(['(', '['])
        .trim_end_matches([')', ']'])
        .trim()
        .to_owned()
}

/// Whether a short text reads as a unit of measure: one the labs use, or a ratio of short names (`mL/kg/h`).
fn is_unit(text: &str) -> bool {
    const UNITS: [&str; 74] = [
        "g", "mg", "µg", "μg", "ug", "kg", "ng", "pg", "ml", "µl", "μl", "ul", "l", "dl", "mm",
        "cm", "m", "mol", "mmol", "µmol", "umol", "nmol", "pmol", "mg/dl", "mg/ml", "ng/ml",
        "pg/ml", "µg/ml", "ug/ml", "mmol/l", "µmol/l", "umol/l", "nmol/l", "µm", "um", "nm", "u/l",
        "iu/l", "%", "kcal", "kcal/h", "kcal/hr", "kcal/d", "kcal/day", "kj", "j", "ml/h",
        "ml/min", "ppm", "°c", "min", "s", "sec", "h", "hr", "hrs", "d", "day", "days", "wk",
        "wks", "week", "weeks", "bpm", "mmhg", "au", "a.u.", "rfu", "od", "counts", "cpm", "mm2",
        "cm2", "mm3",
    ];
    static RATIO: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r"^[A-Za-zµμ°%]{1,6}\d?(?:/[A-Za-zµμ°%]{1,6}\d?){1,3}$").unwrap()
    });
    let t = unit_text(text);
    !t.is_empty()
        && t.chars().count() <= 16
        && (UNITS.contains(&t.to_lowercase().as_str()) || RATIO.is_match(&t))
}

/// Counts for one column's cells in the data rows.
struct Counter {
    kinds: HashMap<Kind, u64>,
    missing: BTreeMap<String, u64>,
    texts: Option<HashMap<String, u64>>,
    text_bytes: usize,
    ids: Option<HashSet<u128>>,
    id_repeats: u64,
    ids_unchecked: bool,
    typed_numbers: u64,
    text_numbers: u64,
    min: Option<(f64, String)>,
    max: Option<(f64, String)>,
    examples: BTreeMap<&'static str, Vec<String>>,
}

impl Counter {
    fn new(id_like: bool) -> Self {
        Counter {
            kinds: HashMap::new(),
            missing: BTreeMap::new(),
            texts: Some(HashMap::new()),
            text_bytes: 0,
            ids: id_like.then(HashSet::new),
            id_repeats: 0,
            ids_unchecked: false,
            typed_numbers: 0,
            text_numbers: 0,
            min: None,
            max: None,
            examples: BTreeMap::new(),
        }
    }

    fn example(&mut self, issue: &'static str, text: &str) {
        let kept = self.examples.entry(issue).or_default();
        let text = shown(text);
        if kept.len() < MAX_EXAMPLES && !kept.contains(&text) {
            kept.push(text);
        }
    }

    fn add(&mut self, cell: &Cell) {
        let kind = classify(cell);
        *self.kinds.entry(kind).or_insert(0) += 1;
        let text = cell.text().unwrap_or_default();
        match kind {
            Kind::Empty => *self.missing.entry(String::new()).or_insert(0) += 1,
            Kind::Missing => {
                *self
                    .missing
                    .entry(missing_token(&text).unwrap_or_default().to_owned())
                    .or_insert(0) += 1;
            }
            Kind::Censored(_) => self.example("censored_values", &text),
            Kind::DecimalComma => self.example("decimal_commas", &text),
            Kind::AmbiguousComma => self.example("ambiguous_commas", &text),
            Kind::Thousands => self.example("thousands_separators", &text),
            Kind::Percent => self.example("percent_signs", &text),
            Kind::Date(Order::Either) => self.example("date_order_ambiguous", &text),
            Kind::Date(Order::DayFirst | Order::MonthFirst) => {
                self.example("date_orders_mixed", &text)
            }
            Kind::Error => self.example("error_cells", &text),
            Kind::Text => self.example("text_in_numbers", &text),
            _ => {}
        }
        if matches!(kind, Kind::Integer | Kind::Decimal) {
            if matches!(cell, Cell::Number(_)) {
                self.typed_numbers += 1;
            } else {
                self.text_numbers += 1;
                self.example("numbers_as_text", &text);
            }
            if let Ok(value) = text.trim().parse::<f64>()
                && value.is_finite()
            {
                if self.min.as_ref().is_none_or(|(m, _)| value < *m) {
                    self.min = Some((value, shown(&text)));
                }
                if self.max.as_ref().is_none_or(|(m, _)| value > *m) {
                    self.max = Some((value, shown(&text)));
                }
            }
        }
        if kind == Kind::Text
            && let Some(texts) = &mut self.texts
        {
            if let Some(count) = texts.get_mut(&text) {
                *count += 1;
            } else if texts.len() < MAX_DISTINCT
                && self.text_bytes + text.len() <= MAX_DISTINCT_BYTES
            {
                self.text_bytes += text.len();
                texts.insert(text.clone(), 1);
            } else {
                self.texts = None;
            }
        }
        if let Some(ids) = &mut self.ids
            && !matches!(kind, Kind::Empty | Kind::Missing)
        {
            let key = text.trim();
            let digest = Sha256::digest(key.as_bytes());
            let mut first = [0_u8; 16];
            first.copy_from_slice(&digest[..16]);
            let hashed = u128::from_be_bytes(first);
            if ids.contains(&hashed) {
                self.id_repeats += 1;
                self.example("duplicate_ids", key);
            } else if ids.len() < MAX_IDS {
                ids.insert(hashed);
            } else {
                self.ids_unchecked = true;
            }
        }
    }

    fn count(&self, kinds: &[Kind]) -> u64 {
        kinds
            .iter()
            .map(|k| self.kinds.get(k).copied().unwrap_or(0))
            .sum()
    }

    fn censored(&self) -> BTreeMap<&'static str, u64> {
        [
            (Censor::Below, "below_limit"),
            (Censor::Above, "above_limit"),
            (Censor::NotDetected, "not_detected"),
        ]
        .into_iter()
        .filter_map(|(c, name)| {
            let n = self.count(&[Kind::Censored(c)]);
            (n > 0).then_some((name, n))
        })
        .collect()
    }

    /// The column's type, its cells with a value, and how many of those are not of the type.
    fn kind(&self) -> (&'static str, u64, u64) {
        const NUMERIC: [Kind; 6] = [
            Kind::Integer,
            Kind::Decimal,
            Kind::DecimalComma,
            Kind::Thousands,
            Kind::AmbiguousComma,
            Kind::Percent,
        ];
        const DATES: [Kind; 4] = [
            Kind::Date(Order::Fixed),
            Kind::Date(Order::DayFirst),
            Kind::Date(Order::MonthFirst),
            Kind::Date(Order::Either),
        ];
        let groups: [(&'static str, u64); 7] = [
            ("number", self.count(&NUMERIC)),
            ("date", self.count(&DATES)),
            ("datetime", self.count(&[Kind::DateTime])),
            ("time", self.count(&[Kind::Time])),
            ("duration", self.count(&[Kind::Duration])),
            ("logical", self.count(&[Kind::Logical])),
            ("text", self.count(&[Kind::Text])),
        ];
        let present: u64 = groups.iter().map(|(_, n)| n).sum();
        let censored = self.count(&[
            Kind::Censored(Censor::Below),
            Kind::Censored(Censor::Above),
            Kind::Censored(Censor::NotDetected),
        ]);
        if present == 0 {
            return (if censored > 0 { "number" } else { "empty" }, 0, 0);
        }
        let (name, most) = groups
            .iter()
            .copied()
            .fold(("text", 0), |best, g| if g.1 > best.1 { g } else { best });
        if (most as f64) < DOMINANT * present as f64 {
            return ("mixed", present, 0);
        }
        let name = if name == "number" && self.count(&NUMERIC[1..]) == 0 {
            "integer"
        } else {
            name
        };
        (name, present, present - most)
    }

    fn date_order(&self) -> Option<&'static str> {
        let day = self.count(&[Kind::Date(Order::DayFirst)]);
        let month = self.count(&[Kind::Date(Order::MonthFirst)]);
        let either = self.count(&[Kind::Date(Order::Either)]);
        match (day > 0, month > 0, either > 0) {
            (true, false, _) => Some("day_first"),
            (false, true, _) => Some("month_first"),
            (true, true, _) | (false, false, true) => Some("ambiguous"),
            (false, false, false) => None,
        }
    }
}

/// The issues a column's counts show, in a fixed order.
fn column_issues(counter: &Counter, kind: &str) -> Vec<&'static str> {
    let mut issues = Vec::new();
    let mut push = |cond: bool, code: &'static str| {
        if cond {
            issues.push(code);
        }
    };
    let numeric = matches!(kind, "number" | "integer");
    let tokens = counter.missing.keys().filter(|k| !k.is_empty()).count();
    push(
        tokens > 1 || (tokens == 1 && counter.missing.contains_key("")),
        "missing_tokens_mixed",
    );
    push(!counter.censored().is_empty(), "censored_values");
    push(counter.count(&[Kind::DecimalComma]) > 0, "decimal_commas");
    push(
        counter.count(&[Kind::AmbiguousComma]) > 0,
        "ambiguous_commas",
    );
    push(
        counter.count(&[Kind::Thousands]) > 0,
        "thousands_separators",
    );
    push(counter.count(&[Kind::Percent]) > 0, "percent_signs");
    push(
        counter.typed_numbers > 0 && counter.text_numbers > 0,
        "numbers_as_text",
    );
    let day = counter.count(&[Kind::Date(Order::DayFirst)]);
    let month = counter.count(&[Kind::Date(Order::MonthFirst)]);
    let either = counter.count(&[Kind::Date(Order::Either)]);
    push(day > 0 && month > 0, "date_orders_mixed");
    push(either > 0 && day == 0 && month == 0, "date_order_ambiguous");
    push(
        numeric && counter.count(&[Kind::Text]) > 0,
        "text_in_numbers",
    );
    push(counter.count(&[Kind::Error]) > 0, "error_cells");
    push(
        kind == "text" && !spelling_variants(counter).is_empty(),
        "category_spellings",
    );
    push(counter.id_repeats > 0, "duplicate_ids");
    push(counter.ids_unchecked, "ids_not_all_checked");
    push(kind == "mixed", "mixed_types");
    issues
}

/// Texts that differ only in case, inner spacing or a final period, such as `Male`, `male` and `male `.
fn spelling_variants(counter: &Counter) -> Vec<Vec<&String>> {
    let Some(texts) = &counter.texts else {
        return Vec::new();
    };
    let mut groups: BTreeMap<String, Vec<&String>> = BTreeMap::new();
    for text in texts.keys() {
        groups.entry(spelling_key(text)).or_default().push(text);
    }
    groups
        .into_values()
        .filter(|g| g.len() > 1)
        .map(|mut g| {
            g.sort();
            g
        })
        .collect()
}

fn spelling_key(text: &str) -> String {
    text.split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .trim_end_matches('.')
        .to_lowercase()
}

/// Wide layouts: three or more adjacent number columns named by one word, an increasing number and one unit (`0`, `15`,
/// `30`; `T0`, `T15`; `Week 1` to `Week 12`; `Glucose 0` to `Glucose 120`), or by increasing dates.
fn time_points(plan: &Plan, columns: &[ColumnProfile]) -> Vec<Vec<usize>> {
    static POINT: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r"^(?i)((?:[a-z]+[\s_-]+)*?[a-z]{0,12})[\s_=-]*(\d+(?:\.\d+)?)\s*([a-z]{0,7})$")
            .unwrap()
    });
    static ISO: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r"^(\d{4})-(\d{2})-(\d{2})").unwrap());
    let parsed: Vec<Option<(String, f64, String)>> = columns
        .iter()
        .map(|column| {
            if !matches!(column.kind, "number" | "integer") {
                return None;
            }
            let name = plan.names[column.index].trim();
            if matches!(
                plan.header_kinds[column.index],
                Kind::Date(_) | Kind::DateTime
            ) {
                let found = ISO.captures(name)?;
                let part = |i: usize| found[i].parse::<f64>().ok();
                return Some((
                    "date".into(),
                    part(1)? * 372.0 + part(2)? * 31.0 + part(3)?,
                    String::new(),
                ));
            }
            let found = POINT.captures(name)?;
            let number: f64 = found[2].parse().ok()?;
            Some((
                found[1].trim().to_lowercase(),
                number,
                found[3].to_lowercase(),
            ))
        })
        .collect();
    let mut runs = Vec::new();
    let mut run: Vec<usize> = Vec::new();
    for (i, point) in parsed.iter().enumerate() {
        let continues = match (run.last(), point) {
            (Some(&last), Some((word, number, unit))) => {
                parsed[last].as_ref().is_some_and(|(w, n, u)| {
                    w == word
                        && u == unit
                        && number > n
                        && columns[i].index == columns[last].index + 1
                })
            }
            _ => false,
        };
        if !continues {
            if run.len() >= 3 {
                runs.push(std::mem::take(&mut run));
            }
            run.clear();
        }
        if point.is_some() {
            run.push(i);
        }
    }
    if run.len() >= 3 {
        runs.push(run);
    }
    runs
}

/// A name R takes as written: lower case ASCII letters, digits and underscores, starting with a letter, at most 63
/// characters.
pub fn suggested_name(name: &str, letter: &str) -> String {
    let mut out = String::new();
    for c in name.chars() {
        let c = match c {
            'µ' | 'μ' => 'u',
            '%' => {
                out.push_str("_pct_");
                continue;
            }
            c => c,
        };
        if c.is_ascii_alphanumeric() {
            out.push(c.to_ascii_lowercase());
        } else if !out.ends_with('_') {
            out.push('_');
        }
    }
    let mut out = out.trim_matches('_').replace("__", "_");
    if out.is_empty() {
        out = letter.to_ascii_lowercase();
    }
    if !out.starts_with(|c: char| c.is_ascii_lowercase()) {
        out.insert_str(0, "x_");
    }
    out.truncate(63);
    out.trim_end_matches('_').to_owned()
}

fn assemble(
    source: &Source,
    shapes: &[Shaped],
    plans: &[Option<Plan>],
    counts: Vec<Vec<Counter>>,
) -> (Profile, Detail) {
    let mut sheets = Vec::new();
    let mut details = Vec::new();
    for (index, ((described, plan), counters)) in
        source.sheets.iter().zip(plans).zip(counts).enumerate()
    {
        let (profile, detail) = match plan {
            Some(plan) => sheet_profile(index, described, &shapes[index], plan, counters),
            None => (
                SheetProfile {
                    index,
                    name: described.name.clone(),
                    hidden: described.hidden,
                    row_count: described.rows,
                    column_count: described.columns,
                    table: None,
                    columns: Vec::new(),
                    columns_omitted: 0,
                    issues: sheet_issues(described),
                },
                SheetDetail {
                    index,
                    preview: Vec::new(),
                    title_text: Vec::new(),
                    note_text: Vec::new(),
                    columns: Vec::new(),
                },
            ),
        };
        sheets.push(profile);
        details.push(detail);
    }
    (
        Profile {
            schema: SCHEMA,
            source: SourceSummary {
                kind: source.kind.clone(),
                sha256: source.sha256.clone(),
                bytes: source.bytes,
                encoding: source.encoding.clone(),
                delimiter: source.delimiter.clone(),
            },
            sheets,
        },
        Detail {
            schema: DETAIL_SCHEMA,
            sheets: details,
        },
    )
}

fn sheet_issues(described: &Sheet) -> Vec<Issue> {
    let mut issues = Vec::new();
    if described.hidden {
        issues.push(Issue {
            code: "hidden_sheet",
            columns: vec![],
            count: 1,
        });
    }
    if described.formulas_without_values > 0 {
        issues.push(Issue {
            code: "formulas_without_values",
            columns: vec![],
            count: described.formulas_without_values,
        });
    }
    if described.merged_count > 0 {
        issues.push(Issue {
            code: "merged_cells",
            columns: vec![],
            count: described.merged_count,
        });
    }
    issues
}

fn sheet_profile(
    index: usize,
    described: &Sheet,
    shaped: &Shaped,
    plan: &Plan,
    counters: Vec<Counter>,
) -> (SheetProfile, SheetDetail) {
    let mut issues = sheet_issues(described);
    let mut columns = Vec::new();
    let mut column_details = Vec::new();
    let mut taken: HashSet<String> = HashSet::new();
    let mut names_seen: HashMap<String, u32> = HashMap::new();
    for (c, counter) in counters.iter().enumerate() {
        let letter = column_letters(c);
        let name = plan.names[c].clone();
        let (kind, present, other) = counter.kind();
        if kind == "empty" && name.is_empty() {
            continue;
        }
        let mut found = column_issues(counter, kind);
        if plan.header.is_some() && name.is_empty() {
            found.push("blank_name");
        }
        if !name.is_empty() {
            *names_seen.entry(name.to_lowercase()).or_insert(0) += 1;
        }
        let mut suggested = suggested_name(&name, &letter);
        if taken.contains(&suggested) {
            let base = suggested.clone();
            suggested = (2..)
                .map(|n| format!("{base}_{n}"))
                .find(|s| !taken.contains(s))
                .unwrap_or(base);
        }
        taken.insert(suggested.clone());
        let mut values: Vec<(String, u64)> = if kind == "text" {
            counter
                .texts
                .as_ref()
                .map(|t| t.iter().map(|(k, v)| (shown(k), *v)).collect())
                .unwrap_or_default()
        } else {
            Vec::new()
        };
        values.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
        values.truncate(MAX_TOP_VALUES);
        let mut examples = counter.examples.clone();
        let spelled: Vec<String> = spelling_variants(counter)
            .into_iter()
            .take(MAX_EXAMPLES)
            .map(|group| {
                group
                    .iter()
                    .map(|t| format!("\"{}\"", shown(t)))
                    .collect::<Vec<_>>()
                    .join(", ")
            })
            .collect();
        if !spelled.is_empty() {
            examples.insert("category_spellings", spelled);
        }
        examples.retain(|issue, _| found.contains(issue));
        columns.push(ColumnProfile {
            index: c,
            letter: letter.clone(),
            name_from: if name.is_empty() { "letter" } else { "header" },
            name: if name.is_empty() {
                letter.clone()
            } else {
                name
            },
            suggested_name: suggested,
            unit: plan.units_cells[c].as_ref().map(|(unit, _)| unit.clone()),
            unit_from: plan.units_cells[c].as_ref().map(|(_, from)| *from),
            kind,
            date_order: if kind == "date" {
                counter.date_order()
            } else {
                None
            },
            present,
            missing: counter.missing.clone(),
            censored: counter.censored(),
            other,
            distinct: (kind == "text")
                .then(|| counter.texts.as_ref().map(|t| t.len() as u64))
                .flatten(),
            issues: found,
        });
        column_details.push(ColumnDetail {
            letter,
            values,
            min: counter.min.as_ref().map(|(_, t)| t.clone()),
            max: counter.max.as_ref().map(|(_, t)| t.clone()),
            examples,
        });
    }
    let duplicated: Vec<String> = columns
        .iter()
        .filter(|c| {
            names_seen
                .get(&c.name.to_lowercase())
                .is_some_and(|n| *n > 1)
        })
        .map(|c| c.letter.clone())
        .collect();
    if !duplicated.is_empty() {
        issues.push(Issue {
            code: "duplicate_names",
            count: duplicated.len() as u64,
            columns: duplicated,
        });
    }
    for (code, with) in gather(&columns) {
        issues.push(Issue {
            code,
            count: with.len() as u64,
            columns: with,
        });
    }
    for run in time_points(plan, &columns) {
        issues.push(Issue {
            code: "wide_time_points",
            count: run.len() as u64,
            columns: run.iter().map(|i| columns[*i].letter.clone()).collect(),
        });
    }
    let units_in_names: Vec<String> = columns
        .iter()
        .filter(|c| plan.units.is_none() && c.unit.is_some())
        .map(|c| c.letter.clone())
        .collect();
    let shape = |code, cond: bool, count: u64| {
        cond.then(|| Issue {
            code,
            columns: vec![],
            count,
        })
    };
    issues.extend(
        [
            shape("no_header", plan.header.is_none(), 1),
            shape("header_not_first", plan.header.is_some_and(|h| h > 1), 1),
            shape("two_header_rows", plan.second.is_some(), 1),
            shape(
                "title_rows",
                !plan.titles.is_empty(),
                plan.titles.len() as u64,
            ),
            shape("units_row", plan.units.is_some(), 1),
            shape("blank_rows", plan.blank > 0, plan.blank),
            shape(
                "summary_rows",
                !plan.summary.is_empty(),
                plan.summary.len() as u64,
            ),
            shape(
                "notes_below_data",
                !plan.notes.is_empty(),
                plan.notes.len() as u64,
            ),
        ]
        .into_iter()
        .flatten(),
    );
    if !units_in_names.is_empty() {
        issues.push(Issue {
            code: "units_in_names",
            count: units_in_names.len() as u64,
            columns: units_in_names,
        });
    }
    let data_rows = (plan.last_data_row + 1)
        .saturating_sub(plan.first_data_row)
        .saturating_sub(plan.blank)
        .saturating_sub(plan.summary.len() as u64);
    let listed = |rows: &[u64]| {
        rows.iter()
            .take(MAX_LISTED_ROWS)
            .copied()
            .collect::<Vec<_>>()
    };
    let table = TableShape {
        header_rows: plan.header_rows(),
        header_basis: plan.basis,
        units_row: plan.units,
        title_rows: plan.titles.clone(),
        first_data_row: plan.first_data_row,
        last_data_row: plan.last_data_row,
        data_rows,
        blank_rows: plan.blank,
        summary_rows: listed(&plan.summary),
        note_rows: listed(&plan.notes),
    };
    let text_of = |rows: &[u64]| -> Vec<String> {
        rows.iter()
            .take(MAX_LISTED_ROWS)
            .filter_map(|r| {
                shaped
                    .singles
                    .iter()
                    .find(|(n, _)| n == r)
                    .map(|(_, t)| t.clone())
            })
            .collect()
    };
    let preview = shaped
        .head
        .iter()
        .take(PREVIEW_ROWS)
        .map(|row| {
            let mut cells: Vec<String> = row
                .iter()
                .take(PREVIEW_COLUMNS)
                .map(|cell| cell.text().map(|t| shown(&t)).unwrap_or_default())
                .collect();
            while cells.last().is_some_and(String::is_empty) {
                cells.pop();
            }
            cells
        })
        .collect();
    (
        SheetProfile {
            index,
            name: described.name.clone(),
            hidden: described.hidden,
            row_count: described.rows,
            column_count: described.columns,
            table: Some(table),
            columns,
            columns_omitted: plan.width.saturating_sub(MAX_COLUMNS) as u64,
            issues,
        },
        SheetDetail {
            index,
            preview,
            title_text: text_of(&plan.titles),
            note_text: text_of(&plan.notes),
            columns: column_details,
        },
    )
}

/// Each column issue with the columns that have it, in the order the issues first appear.
fn gather(columns: &[ColumnProfile]) -> Vec<(&'static str, Vec<String>)> {
    let mut order: Vec<&'static str> = Vec::new();
    let mut by: HashMap<&'static str, Vec<String>> = HashMap::new();
    for column in columns {
        for issue in &column.issues {
            if !by.contains_key(issue) {
                order.push(issue);
            }
            by.entry(issue).or_default().push(column.letter.clone());
        }
    }
    order
        .into_iter()
        .map(|code| (code, by.remove(code).unwrap_or_default()))
        .collect()
}

/// Text as the detail shows it: control and direction characters left out, cut at 120 characters.
pub fn shown(text: &str) -> String {
    text.chars()
        .filter(|&c| !c.is_control() && !super::live_output::is_bidi_control(c))
        .take(MAX_SHOWN_CHARS)
        .collect()
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::broker::cell_grid::tests::grid;
    use serde_json::{Value, json};

    fn profiled(kind: &str, sheets: &[(Option<&str>, Vec<Value>)]) -> (Profile, Detail) {
        let bytes = grid(kind, sheets);
        profile(|| Ok(&bytes[..])).unwrap()
    }

    fn text_rows(rows: &[&[&str]]) -> Vec<Value> {
        rows.iter().map(|r| json!(r)).collect()
    }

    fn column<'a>(sheet: &'a SheetProfile, letter: &str) -> &'a ColumnProfile {
        sheet.columns.iter().find(|c| c.letter == letter).unwrap()
    }

    fn issue<'a>(sheet: &'a SheetProfile, code: &str) -> Option<&'a Issue> {
        sheet.issues.iter().find(|i| i.code == code)
    }

    /// A glucose tolerance sheet as labs keep one: a title, a heading over the time points, units, censored insulin,
    /// spellings of sex, a repeated mouse, a mean row and a note. Synthetic values.
    pub(crate) fn tolerance_sheet() -> Vec<Value> {
        text_rows(&[
            &["Glucose tolerance test, cohort 3"],
            &[],
            &[
                "Mouse ID",
                "Sex",
                "Diet",
                "Body weight (g)",
                "Glucose (mg/dL)",
                "",
                "",
                "",
                "",
                "Insulin",
            ],
            &["", "", "", "", "0", "15", "30", "60", "120", ""],
            &["", "", "", "", "", "", "", "", "", "ng/mL"],
            &[
                "M-0417", "Male", "HFD", "31,2", "142", "310", "295", "240", "160", "1.2",
            ],
            &[
                "M-0418", "male ", "HFD", "29,8", "150", "330", "NA", "251", "171", "<0.2",
            ],
            &[
                "M-0419", "Female", "Chow", "22,4", "120", "250", "230", "180", "130", "ND",
            ],
            &[
                "M-0419", "female", "Chow", "23,0", "118", "", "228", "175", "-", "0.9",
            ],
            &[
                "M-0421", "MALE", "HFD", "30,1", "145", "300", "280", "230", "155", "1.4",
            ],
            &[
                "Mean", "", "", "27,3", "135", "297", "258", "215", "154", "",
            ],
            &[],
            &["Mouse M-0420 died on day 3 and is not listed"],
        ])
    }

    #[test]
    fn a_messy_tolerance_sheet_is_found_and_named() {
        let (profile, detail) = profiled("delimited", &[(None, tolerance_sheet())]);
        let sheet = &profile.sheets[0];
        let table = sheet.table.as_ref().unwrap();
        assert_eq!(table.header_rows, [3, 4]);
        assert_eq!(table.header_basis, Some("labels_over_numbers"));
        assert_eq!(table.units_row, Some(5));
        assert_eq!(table.title_rows, [1]);
        assert_eq!((table.first_data_row, table.last_data_row), (6, 11));
        assert_eq!(table.data_rows, 5);
        assert_eq!(table.summary_rows, [11]);
        assert_eq!(table.note_rows, [13]);
        let names: Vec<&str> = sheet.columns.iter().map(|c| c.name.as_str()).collect();
        assert_eq!(
            names,
            [
                "Mouse ID",
                "Sex",
                "Diet",
                "Body weight",
                "Glucose 0",
                "Glucose 15",
                "Glucose 30",
                "Glucose 60",
                "Glucose 120",
                "Insulin"
            ]
        );
        assert_eq!(column(sheet, "D").unit.as_deref(), Some("g"));
        assert_eq!(column(sheet, "E").unit.as_deref(), Some("mg/dL"));
        assert_eq!(column(sheet, "J").unit.as_deref(), Some("ng/mL"));
        // Each name and unit says where it came from: the header row, the units row under it, or the column letter.
        assert_eq!(
            (column(sheet, "D").name_from, column(sheet, "D").unit_from),
            ("header", Some("header"))
        );
        assert_eq!(
            (column(sheet, "J").name_from, column(sheet, "J").unit_from),
            ("header", Some("units_row"))
        );
        assert_eq!(column(sheet, "B").unit_from, None);
        assert_eq!(column(sheet, "E").suggested_name, "glucose_0");
        assert_eq!(column(sheet, "D").suggested_name, "body_weight");
        assert_eq!(column(sheet, "D").kind, "number");
        assert_eq!(column(sheet, "E").kind, "integer");
        let insulin = column(sheet, "J");
        assert_eq!(insulin.kind, "number");
        assert_eq!(insulin.present, 3);
        assert_eq!(
            insulin.censored,
            BTreeMap::from([("below_limit", 1), ("not_detected", 1)])
        );
        assert_eq!(
            column(sheet, "G").missing,
            BTreeMap::from([("NA".to_owned(), 1)])
        );
        assert_eq!(
            column(sheet, "I").missing,
            BTreeMap::from([("-".to_owned(), 1)])
        );
        for (code, columns) in [
            ("decimal_commas", vec!["D"]),
            ("category_spellings", vec!["B"]),
            ("duplicate_ids", vec!["A"]),
            ("censored_values", vec!["J"]),
            ("wide_time_points", vec!["E", "F", "G", "H", "I"]),
        ] {
            let found = issue(sheet, code).unwrap_or_else(|| panic!("{code} not found"));
            assert_eq!(found.columns, columns, "{code}");
        }
        for code in [
            "two_header_rows",
            "units_row",
            "title_rows",
            "summary_rows",
            "notes_below_data",
            "header_not_first",
        ] {
            assert!(issue(sheet, code).is_some(), "{code}");
        }
        assert!(issue(sheet, "no_header").is_none());
        let sex = &detail.sheets[0].columns[1];
        assert_eq!(
            sex.examples["category_spellings"],
            ["\"Female\", \"female\"", "\"MALE\", \"Male\", \"male \""]
        );
        assert_eq!(
            detail.sheets[0].note_text,
            ["Mouse M-0420 died on day 3 and is not listed"]
        );
        assert_eq!(
            detail.sheets[0].title_text,
            ["Glucose tolerance test, cohort 3"]
        );
        assert_eq!(
            detail.sheets[0].columns[0].examples["duplicate_ids"],
            ["M-0419"]
        );
    }

    #[test]
    fn the_profile_holds_no_data_cell_and_the_detail_does() {
        let (profile, detail) = profiled("delimited", &[(None, tolerance_sheet())]);
        let profile = serde_json::to_string(&profile).unwrap();
        let detail = serde_json::to_string(&detail).unwrap();
        for cell in [
            "M-0417", "M-0419", "31,2", "HFD", "Chow", "Female", "<0.2", "died", "cohort 3",
        ] {
            assert!(!profile.contains(cell), "{cell} in the profile");
            assert!(detail.contains(cell), "{cell} not in the detail");
        }
    }

    #[test]
    fn the_same_grid_gives_the_same_profile() {
        let first = profiled("delimited", &[(None, tolerance_sheet())]);
        for _ in 0..5 {
            assert_eq!(profiled("delimited", &[(None, tolerance_sheet())]), first);
        }
    }

    #[test]
    fn a_workbook_shows_types_dates_and_sheet_issues() {
        let rows = vec![
            json!(["Mouse", "Date", "Weight", "Genotype", "Fasted"]),
            json!(["a1", {"date": "2025-03-04"}, 21.5, "WT", true]),
            json!(["a2", {"date": "2025-03-05"}, "22.5", "KO", false]),
            json!(["a3", {"date": "2025-03-06"}, 23, "KO", {"error": "#DIV/0!"}]),
            json!(["a4", "05/03/2025", 24.25, "WT", true]),
        ];
        let bytes = grid("xlsx", &[(Some("Weights"), rows), (Some("Empty"), vec![])]);
        let (profile, _) = profile(|| Ok(&bytes[..])).unwrap();
        let sheet = &profile.sheets[0];
        assert_eq!(sheet.table.as_ref().unwrap().header_rows, [1]);
        assert_eq!(column(sheet, "B").kind, "date");
        assert_eq!(column(sheet, "B").date_order, Some("ambiguous"));
        assert_eq!(column(sheet, "C").kind, "number");
        assert!(column(sheet, "C").issues.contains(&"numbers_as_text"));
        assert_eq!(column(sheet, "E").kind, "logical");
        assert_eq!(column(sheet, "E").present, 3);
        assert!(column(sheet, "E").issues.contains(&"error_cells"));
        assert!(issue(sheet, "header_not_first").is_none());
        assert_eq!(profile.sheets[1].table, None);
        assert_eq!(profile.sheets[1].name.as_deref(), Some("Empty"));
    }

    #[test]
    fn a_table_without_a_header_is_named_by_letters() {
        let rows = text_rows(&[
            &["m1", "WT", "21.5"],
            &["m2", "KO", "22.0"],
            &["m3", "KO", "19.5"],
        ]);
        let (profile, _) = profiled("delimited", &[(None, rows)]);
        let sheet = &profile.sheets[0];
        let table = sheet.table.as_ref().unwrap();
        assert_eq!(table.header_rows, Vec::<u64>::new());
        assert_eq!((table.first_data_row, table.data_rows), (1, 3));
        assert_eq!(
            (
                column(sheet, "A").name.as_str(),
                column(sheet, "A").name_from
            ),
            ("A", "letter")
        );
        assert_eq!(column(sheet, "C").suggested_name, "c");
        assert!(issue(sheet, "no_header").is_some());
        // A first row whose number is missing is not taken for a header.
        let rows = text_rows(&[
            &["m1", "WT", ""],
            &["m2", "KO", "22.0"],
            &["m3", "KO", "19.5"],
        ]);
        let (profile, _) = profiled("delimited", &[(None, rows)]);
        assert_eq!(
            profile.sheets[0].table.as_ref().unwrap().header_rows,
            Vec::<u64>::new()
        );
    }

    #[test]
    fn a_text_table_takes_its_first_full_row_as_the_header() {
        let rows = text_rows(&[&["Strain", "Source"], &["C57BL/6J", "JAX"], &["DO", "JAX"]]);
        let (profile, _) = profiled("delimited", &[(None, rows)]);
        let table = profile.sheets[0].table.as_ref().unwrap();
        assert_eq!(table.header_rows, [1]);
        assert_eq!(table.header_basis, Some("first_text_row"));
        assert_eq!(
            (
                column(&profile.sheets[0], "A").name.as_str(),
                column(&profile.sheets[0], "A").name_from
            ),
            ("Strain", "header")
        );
    }

    #[test]
    fn censored_and_missing_tokens_are_kept_apart() {
        for (text, kind) in [
            ("<0.5", Kind::Censored(Censor::Below)),
            ("< 0,5 ng/mL", Kind::Censored(Censor::Below)),
            (">2000", Kind::Censored(Censor::Above)),
            ("<LOD", Kind::Censored(Censor::Below)),
            ("bql", Kind::Censored(Censor::Below)),
            ("N.D.", Kind::Censored(Censor::NotDetected)),
            ("NA", Kind::Missing),
            (" n/a ", Kind::Missing),
            ("-", Kind::Missing),
            ("-3", Kind::Integer),
            ("1,5", Kind::DecimalComma),
            (",5", Kind::DecimalComma),
            ("1.234,5", Kind::DecimalComma),
            ("1,234", Kind::AmbiguousComma),
            ("1,234,567", Kind::Thousands),
            ("12.5 %", Kind::Percent),
            ("1e-3", Kind::Decimal),
            ("2025-03-04", Kind::Date(Order::Fixed)),
            ("13/03/2025", Kind::Date(Order::DayFirst)),
            ("03/13/2025", Kind::Date(Order::MonthFirst)),
            ("03/04/2025", Kind::Date(Order::Either)),
            ("04/04/2025", Kind::Date(Order::Fixed)),
            ("10:30", Kind::Time),
            ("TRUE", Kind::Logical),
            ("dead", Kind::Text),
        ] {
            assert_eq!(classify_text(text), kind, "{text}");
        }
    }

    #[test]
    fn suggested_names_are_names_r_keeps() {
        for (name, expected) in [
            ("Body weight", "body_weight"),
            ("Glucose 0", "glucose_0"),
            ("0", "x_0"),
            ("Fat mass %", "fat_mass_pct"),
            ("µg/L", "ug_l"),
            ("", "c"),
            ("--", "c"),
        ] {
            assert_eq!(suggested_name(name, "C"), expected, "{name}");
        }
        assert!(suggested_name(&"a".repeat(100), "A").len() <= 63);
    }

    #[test]
    fn dated_columns_and_week_columns_are_wide_layouts() {
        let rows = vec![
            json!(["Mouse", {"date": "2025-03-04"}, {"date": "2025-03-11"}, {"date": "2025-03-18"}]),
            json!(["a1", 21.5, 22.0, 22.4]),
            json!(["a2", 20.5, 21.0, 21.9]),
        ];
        let bytes = grid("xlsx", &[(Some("Weights"), rows)]);
        let (profile, _) = profile(|| Ok(&bytes[..])).unwrap();
        assert_eq!(
            issue(&profile.sheets[0], "wide_time_points")
                .unwrap()
                .columns,
            ["B", "C", "D"]
        );
        let rows = text_rows(&[
            &["Mouse", "Week 1", "Week 2", "Week 4"],
            &["a1", "21", "22", "23"],
        ]);
        let (profile, _) = profiled("delimited", &[(None, rows)]);
        assert_eq!(
            issue(&profile.sheets[0], "wide_time_points")
                .unwrap()
                .columns,
            ["B", "C", "D"]
        );
        let rows = text_rows(&[
            &["Mouse", "Week 4", "Week 2", "Week 1"],
            &["a1", "21", "22", "23"],
        ]);
        let (profile, _) = profiled("delimited", &[(None, rows)]);
        assert!(issue(&profile.sheets[0], "wide_time_points").is_none());
    }

    #[test]
    fn a_sparse_wide_grid_is_profiled_within_bounds() {
        // A row whose one filled cell is in column XFD arrives 16,384 cells wide; the header sample keeps 512.
        let mut wide = vec![Cell::Empty; 16_384];
        wide[16_383] = Cell::Text("x".into());
        let mut shaped = Shaped::default();
        shaped.push(&wide);
        assert_eq!(
            (shaped.head[0].len(), shaped.kinds[0].len()),
            (MAX_COLUMNS, MAX_COLUMNS)
        );
        // Sheets of such rows profile, and say how many columns the profile left out.
        let rows: Vec<Value> = (0..40)
            .map(|i| {
                let mut row = vec![Value::Null; 16_384];
                row[0] = json!(format!("a{i}"));
                row[16_383] = json!(i);
                json!(row)
            })
            .collect();
        let (profile, _) = profiled("xlsx", &[(Some("one"), rows.clone()), (Some("two"), rows)]);
        assert_eq!(profile.sheets.len(), 2);
        assert!(
            profile
                .sheets
                .iter()
                .all(|s| s.columns_omitted == 16_384 - MAX_COLUMNS as u64)
        );
        // Row shapes are held to a budget over all sheets.
        let bytes = grid(
            "delimited",
            &[(None, (0..11).map(|_| json!(["a"])).collect())],
        );
        assert_eq!(
            profile_within(|| Ok(&bytes[..]), 10).unwrap_err(),
            GridError::TooLarge
        );
        assert!(profile_within(|| Ok(&bytes[..]), 11).is_ok());
    }

    #[test]
    fn a_grid_that_is_not_one_is_refused() {
        assert_eq!(
            profile(|| Ok(&b"not a grid"[..])).unwrap_err(),
            GridError::Unreadable
        );
    }
}
