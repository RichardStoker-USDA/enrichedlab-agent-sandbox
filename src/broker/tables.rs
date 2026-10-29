//! Server-side pages and column summaries of a stored CSV or TSV table (access `paged`; workspace shapes
//! v2 items 6 and 7). The routine reads verified stored bytes once per digest and never changes a value: pages and
//! summaries carry the stored strings. Until a method declares column types, each column's type is inferred from every
//! stored value, and a summary's receipt names this routine, its version and the digests of its input and output.
//!
//! Rules, version 1:
//! - UTF-8 only; a leading byte-order mark is dropped; the first record is the header; every row has the header's
//!   field count; blank lines are skipped. CSV uses a comma and TSV a tab, both with RFC 4180 quoting.
//! - A value is missing when it is empty or exactly `NA`.
//! - A column is `integer` when every present value is an optional sign and digits; `number` when every present value
//!   is an integer, a decimal with an optional exponent, `Inf`, `+Inf`, `-Inf` or `NaN`; `logical` when every present
//!   value is `TRUE` or `FALSE`; otherwise, or with no present value, `string`.
//! - Order: integers exactly by value at any length, numbers as doubles with `NaN` after every number and `-0` equal to
//!   `0`, logicals with `FALSE` first, strings by their bytes; missing values last in both directions; ties keep file
//!   order.
//! - A summary counts present and missing values and distinct stored strings, and names the stored strings that come
//!   first and last in that order. Its `output_sha256` is the SHA-256 of `columns` in RFC 8785 canonical JSON.
use std::{
    cmp::Ordering,
    collections::{HashMap, HashSet, VecDeque},
    sync::{Arc, Mutex, OnceLock},
};

use serde::Serialize;
use sha2::{Digest, Sha256};

use super::canonical::canonical_json;
use super::csv_records::{Record, Records};

pub const ROUTINE: &str = "enrichedlab.table-summary";
pub const ROUTINE_VERSION: u32 = 1;
/// Bounds that keep a parsed table inside the API's memory. A larger table stays downloadable.
pub const MAX_TABLE_BYTES: u64 = 16 * 1024 * 1024;
pub const MAX_ROWS: usize = 1_000_000;
pub const MAX_COLUMNS: usize = 1_000;
pub const MAX_CELLS: usize = 2_000_000;
pub const MAX_PAGE_ROWS: usize = 1_000;

#[derive(Debug, PartialEq, Eq)]
pub enum TableError {
    NotATable,
    Unreadable(String),
    TooLarge,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum ColumnType {
    Integer,
    Number,
    Logical,
    String,
}

/// The delimiter for a table chosen by its media type (parameters ignored); none for anything else.
pub fn delimiter_for(media_type: &str) -> Option<u8> {
    match media_type.split(';').next().unwrap_or_default().trim() {
        "text/csv" => Some(b','),
        "text/tab-separated-values" => Some(b'\t'),
        _ => None,
    }
}

/// Row indices in one column's order, by column and direction.
type Orders = Mutex<HashMap<(usize, bool), Arc<Vec<u32>>>>;

/// One parsed table. The stored strings sit in one buffer, row-major, so a large table costs about its own size.
pub struct Table {
    pub sha256: String,
    pub delimiter: u8,
    pub columns: Vec<String>,
    pub types: Vec<ColumnType>,
    header_bytes: usize,
    summary_bytes: usize,
    data: String,
    cells: Vec<(u32, u32)>,
    lines: Vec<u64>,
    orders: Orders,
    summary: OnceLock<(Vec<ColumnSummary>, Receipt)>,
}

impl Table {
    pub fn rows(&self) -> usize {
        self.lines.len()
    }

    fn value(&self, row: usize, column: usize) -> &str {
        let (start, length) = self.cells[row * self.columns.len() + column];
        &self.data[start as usize..(start + length) as usize]
    }

    /// A stored value by row and column, both from 0, or none outside the table.
    pub fn cell(&self, row: usize, column: usize) -> Option<&str> {
        (row < self.rows() && column < self.columns.len()).then(|| self.value(row, column))
    }

    /// An estimate of the memory the table can hold, for the cache budget: its column names, strings, cell offsets and
    /// lines, both orders of every column and its summary, all counted before any order or the summary is made.
    pub fn approx_bytes(&self) -> usize {
        self.header_bytes
            + self.columns.len() * 24
            + self.data.len()
            + self.cells.len() * 16
            + self.lines.len() * 8
            + self.summary_bytes
            + 256
    }

    /// Whether an order is ready, so a caller can bound the work of computing a new one.
    pub fn has_order(&self, column: usize, descending: bool) -> bool {
        self.orders
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .contains_key(&(column, descending))
    }

    /// Row indices in the requested order, computed once per column and direction.
    pub fn order(&self, column: usize, descending: bool) -> Arc<Vec<u32>> {
        let key = (column, descending);
        if let Some(found) = self
            .orders
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(&key)
        {
            return found.clone();
        }
        let kind = self.types[column];
        let keys: Vec<Option<Key<'_>>> = (0..self.rows())
            .map(|row| {
                let value = self.value(row, column);
                (!is_missing(value)).then(|| Key::of(kind, value))
            })
            .collect();
        let mut rows: Vec<u32> = (0..self.rows() as u32).collect();
        // A stable sort keeps file order among equal values; missing values stay last in both directions.
        rows.sort_by(|&a, &b| match (keys[a as usize], keys[b as usize]) {
            (None, None) => Ordering::Equal,
            (None, Some(_)) => Ordering::Greater,
            (Some(_), None) => Ordering::Less,
            (Some(left), Some(right)) => {
                let order = left.order(&right);
                if descending { order.reverse() } else { order }
            }
        });
        let rows = Arc::new(rows);
        self.orders
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(key, rows.clone());
        rows
    }

    /// One page of rows: each row's place in the file, the line its record starts on, and its stored values. The page
    /// serializes straight from the table's text, so it holds no copy of the values.
    pub fn page(&self, offset: usize, limit: usize, sort: Option<(usize, bool)>) -> Page<'_> {
        let total = self.rows();
        let start = offset.min(total);
        Page {
            table: self,
            order: sort.map(|(column, descending)| self.order(column, descending)),
            start,
            end: total.min(start.saturating_add(limit)),
        }
    }

    /// Whether the summary is ready, so a caller can bound the work of computing it.
    pub fn summary_ready(&self) -> bool {
        self.summary.get().is_some()
    }

    /// Every column's summary and the receipt that binds them to the table's digest and this routine, computed once.
    pub fn summary(&self) -> &(Vec<ColumnSummary>, Receipt) {
        self.summary.get_or_init(|| {
            let columns: Vec<ColumnSummary> = (0..self.columns.len())
                .map(|column| summarize(self, column))
                .collect();
            let canonical = canonical_json(&serde_json::to_value(&columns).unwrap_or_default());
            let receipt = Receipt {
                routine: ROUTINE,
                version: ROUTINE_VERSION,
                input_sha256: self.sha256.clone(),
                output_sha256: hex::encode(Sha256::digest(&canonical)),
            };
            (columns, receipt)
        })
    }
}

/// Rows of a table to serialize as a JSON array of `{"index", "line", "values"}`.
pub struct Page<'a> {
    table: &'a Table,
    order: Option<Arc<Vec<u32>>>,
    start: usize,
    end: usize,
}

impl Page<'_> {
    pub fn len(&self) -> usize {
        self.end - self.start
    }

    pub fn is_empty(&self) -> bool {
        self.end == self.start
    }
}

impl Serialize for Page<'_> {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        use serde::ser::SerializeSeq;
        let mut rows = serializer.serialize_seq(Some(self.len()))?;
        for position in self.start..self.end {
            let row = self
                .order
                .as_ref()
                .map_or(position, |order| order[position] as usize);
            rows.serialize_element(&PageRow {
                table: self.table,
                row,
            })?;
        }
        rows.end()
    }
}

struct PageRow<'a> {
    table: &'a Table,
    row: usize,
}

impl Serialize for PageRow<'_> {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        use serde::ser::{SerializeMap, SerializeSeq};
        struct Values<'a>(&'a PageRow<'a>);
        impl Serialize for Values<'_> {
            fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
                let PageRow { table, row } = self.0;
                let mut values = serializer.serialize_seq(Some(table.columns.len()))?;
                for column in 0..table.columns.len() {
                    values.serialize_element(table.value(*row, column))?;
                }
                values.end()
            }
        }
        let mut row = serializer.serialize_map(Some(3))?;
        row.serialize_entry("index", &self.row)?;
        row.serialize_entry("line", &self.table.lines[self.row])?;
        row.serialize_entry("values", &Values(self))?;
        row.end()
    }
}

/// A response body of at most `limit` bytes: serialization stops with an error as soon as it would pass the limit, so a
/// large page or summary is refused without being built whole.
pub fn bounded_json(value: &impl Serialize, limit: usize) -> Option<Vec<u8>> {
    struct Bounded {
        bytes: Vec<u8>,
        limit: usize,
    }
    impl std::io::Write for Bounded {
        fn write(&mut self, data: &[u8]) -> std::io::Result<usize> {
            if self.bytes.len() + data.len() > self.limit {
                return Err(std::io::Error::other("body_too_large"));
            }
            self.bytes.extend_from_slice(data);
            Ok(data.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    let mut body = Bounded {
        bytes: Vec::new(),
        limit,
    };
    serde_json::to_writer(&mut body, value).ok()?;
    Some(body.bytes)
}

/// Parse a CSV or TSV table under the version 1 rules.
pub fn parse(bytes: &[u8], delimiter: u8, sha256: String) -> Result<Table, TableError> {
    if bytes.len() as u64 > MAX_TABLE_BYTES {
        return Err(TableError::TooLarge);
    }
    let text = std::str::from_utf8(bytes).map_err(|error| {
        TableError::Unreadable(format!(
            "the file is not UTF-8 (byte {})",
            error.valid_up_to()
        ))
    })?;
    let text = text.strip_prefix('\u{feff}').unwrap_or(text);
    // Any record of the text fits its whole length, so a record is refused only for its fields.
    let mut records = Records::new(text.as_bytes(), delimiter, MAX_COLUMNS, text.len());
    let mut counter = Lines {
        bytes: text.as_bytes(),
        scanned: 0,
        line: 1,
    };
    let columns: Vec<String> = match read(&mut records)? {
        None | Some(Record::Fields(0)) => {
            return Err(TableError::Unreadable("the file has no header".into()));
        }
        Some(Record::Fields(_)) => records
            .fields()
            .map(|field| utf8(field).map(str::to_owned))
            .collect::<Result<_, _>>()?,
        Some(_) => return Err(TableError::TooLarge),
    };
    let width = columns.len();
    let (mut data, mut cells, mut lines) = (String::new(), Vec::new(), Vec::new());
    let mut longest = vec![0_usize; width];
    while let Some(record) = read(&mut records)? {
        let line = counter.at(records.start() as usize);
        let count = match record {
            Record::Fields(count) => count,
            _ => {
                return Err(TableError::Unreadable(format!(
                    "line {line} has more than {MAX_COLUMNS} fields; the header has {width}"
                )));
            }
        };
        if count != width {
            let fields = if count == 1 { "field" } else { "fields" };
            return Err(TableError::Unreadable(format!(
                "line {line} has {count} {fields}; the header has {width}"
            )));
        }
        if lines.len() == MAX_ROWS || cells.len() + width > MAX_CELLS {
            return Err(TableError::TooLarge);
        }
        for (column, field) in records.fields().enumerate() {
            let value = utf8(field)?;
            // Unquoting only shortens text, so offsets stay below the 16 MiB input bound.
            cells.push((data.len() as u32, value.len() as u32));
            data.push_str(value);
            longest[column] = longest[column].max(value.len());
        }
        lines.push(line);
    }
    let header_bytes: usize = columns.iter().map(String::len).sum();
    let mut table = Table {
        sha256,
        delimiter,
        // A summary holds each column's name and at most two of its values, which are no longer than its longest.
        summary_bytes: header_bytes + 2 * longest.iter().sum::<usize>() + width * 96,
        header_bytes,
        columns,
        types: Vec::new(),
        data,
        cells,
        lines,
        orders: Mutex::default(),
        summary: OnceLock::new(),
    };
    table.types = (0..width).map(|column| infer(&table, column)).collect();
    Ok(table)
}

fn read(records: &mut Records<&[u8]>) -> Result<Option<Record>, TableError> {
    // Reading from memory cannot fail.
    records
        .read_record()
        .map_err(|_| TableError::Unreadable("the file cannot be read as a table".into()))
}

/// A field as text. The input is UTF-8 and the reader removes only ASCII quotes, so this holds for every field.
fn utf8(field: &[u8]) -> Result<&str, TableError> {
    std::str::from_utf8(field).map_err(|_| TableError::Unreadable("the file is not UTF-8".into()))
}

/// Physical line numbers of record starts. The csv reader's own count misses skipped blank lines, so lines are
/// counted from byte offsets: `\r\n`, `\r` and `\n` each end a line, as each ends a record.
struct Lines<'a> {
    bytes: &'a [u8],
    scanned: usize,
    line: u64,
}

impl Lines<'_> {
    /// The 1-based line of the first byte at or after `offset` that does not end a line. Offsets only grow.
    fn at(&mut self, offset: usize) -> u64 {
        let mut start = offset.min(self.bytes.len());
        while start < self.bytes.len() && matches!(self.bytes[start], b'\r' | b'\n') {
            start += 1;
        }
        for at in self.scanned..start {
            match self.bytes[at] {
                b'\n' => self.line += 1,
                b'\r' if self.bytes.get(at + 1) != Some(&b'\n') => self.line += 1,
                _ => {}
            }
        }
        self.scanned = self.scanned.max(start);
        self.line
    }
}

fn is_missing(value: &str) -> bool {
    value.is_empty() || value == "NA"
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Class {
    Missing,
    Logical,
    Integer,
    Number,
    Text,
}

fn class(value: &str) -> Class {
    match value {
        "" | "NA" => Class::Missing,
        "TRUE" | "FALSE" => Class::Logical,
        "Inf" | "+Inf" | "-Inf" | "NaN" => Class::Number,
        _ if is_integer(value) => Class::Integer,
        _ if is_decimal(value) => Class::Number,
        _ => Class::Text,
    }
}

fn is_integer(value: &str) -> bool {
    let digits = value.strip_prefix(['+', '-']).unwrap_or(value);
    !digits.is_empty() && digits.bytes().all(|byte| byte.is_ascii_digit())
}

/// An optional sign, digits with at most one point and at least one digit, and an optional integer exponent.
fn is_decimal(value: &str) -> bool {
    let body = value.strip_prefix(['+', '-']).unwrap_or(value);
    let (mantissa, exponent) = match body.find(['e', 'E']) {
        Some(at) => (&body[..at], Some(&body[at + 1..])),
        None => (body, None),
    };
    let (mut point, mut digit) = (false, false);
    for byte in mantissa.bytes() {
        match byte {
            b'0'..=b'9' => digit = true,
            b'.' if !point => point = true,
            _ => return false,
        }
    }
    digit && exponent.is_none_or(is_integer)
}

fn infer(table: &Table, column: usize) -> ColumnType {
    let (mut integer, mut number, mut logical, mut present) = (true, true, true, false);
    for row in 0..table.rows() {
        match class(table.value(row, column)) {
            Class::Missing => {}
            Class::Logical => {
                present = true;
                integer = false;
                number = false;
            }
            Class::Integer => {
                present = true;
                logical = false;
            }
            Class::Number => {
                present = true;
                integer = false;
                logical = false;
            }
            Class::Text => return ColumnType::String,
        }
    }
    match (present, integer, number, logical) {
        (false, ..) => ColumnType::String,
        (true, true, ..) => ColumnType::Integer,
        (true, false, true, _) => ColumnType::Number,
        (true, false, false, true) => ColumnType::Logical,
        _ => ColumnType::String,
    }
}

/// One present value's sort key. Every value of a column has the same kind of key, so the order is total.
#[derive(Clone, Copy)]
enum Key<'a> {
    /// The digits without sign or leading zeros; zero has no digits and is never negative.
    Integer {
        negative: bool,
        digits: &'a str,
    },
    Number(f64),
    Logical(bool),
    Text(&'a str),
}

impl<'a> Key<'a> {
    fn of(kind: ColumnType, value: &'a str) -> Self {
        match kind {
            ColumnType::Integer => {
                let (negative, body) = match value.strip_prefix('-') {
                    Some(rest) => (true, rest),
                    None => (false, value.strip_prefix('+').unwrap_or(value)),
                };
                let digits = body.trim_start_matches('0');
                Self::Integer {
                    negative: negative && !digits.is_empty(),
                    digits,
                }
            }
            ColumnType::Number => Self::Number(number(value)),
            ColumnType::Logical => Self::Logical(value == "TRUE"),
            ColumnType::String => Self::Text(value),
        }
    }

    fn order(&self, other: &Self) -> Ordering {
        match (*self, *other) {
            (
                Self::Integer {
                    negative: left_negative,
                    digits: left,
                },
                Self::Integer {
                    negative: right_negative,
                    digits: right,
                },
            ) => {
                let size = left.len().cmp(&right.len()).then_with(|| left.cmp(right));
                match (left_negative, right_negative) {
                    (false, false) => size,
                    (true, true) => size.reverse(),
                    (true, false) => Ordering::Less,
                    (false, true) => Ordering::Greater,
                }
            }
            (Self::Number(left), Self::Number(right)) => left.total_cmp(&right),
            (Self::Logical(left), Self::Logical(right)) => left.cmp(&right),
            (Self::Text(left), Self::Text(right)) => left.as_bytes().cmp(right.as_bytes()),
            // A column never mixes kinds.
            _ => Ordering::Equal,
        }
    }
}

/// `NaN` is the positive quiet NaN, which `total_cmp` places after every number; `-0` becomes `0`.
fn number(value: &str) -> f64 {
    let parsed = match value {
        "Inf" | "+Inf" => f64::INFINITY,
        "-Inf" => f64::NEG_INFINITY,
        "NaN" => f64::NAN,
        _ => value.parse().unwrap_or(f64::NAN),
    };
    if parsed == 0.0 { 0.0 } else { parsed }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct ColumnSummary {
    pub name: String,
    #[serde(rename = "type")]
    pub column_type: ColumnType,
    pub type_source: &'static str,
    pub count: usize,
    pub missing: usize,
    pub min: Option<String>,
    pub max: Option<String>,
    pub distinct: usize,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Receipt {
    pub routine: &'static str,
    pub version: u32,
    pub input_sha256: String,
    pub output_sha256: String,
}

/// A column's first or last present value so far, with its key.
type Extreme<'a> = Option<(Key<'a>, &'a str)>;

fn summarize(table: &Table, column: usize) -> ColumnSummary {
    let kind = table.types[column];
    let (mut count, mut missing) = (0, 0);
    let (mut first, mut last): (Extreme, Extreme) = (None, None);
    let mut distinct = HashSet::new();
    for row in 0..table.rows() {
        let value = table.value(row, column);
        if is_missing(value) {
            missing += 1;
            continue;
        }
        count += 1;
        distinct.insert(value);
        let key = Key::of(kind, value);
        // Strict comparisons keep the earliest of equal values.
        if first.is_none_or(|(least, _)| key.order(&least) == Ordering::Less) {
            first = Some((key, value));
        }
        if last.is_none_or(|(most, _)| key.order(&most) == Ordering::Greater) {
            last = Some((key, value));
        }
    }
    ColumnSummary {
        name: table.columns[column].clone(),
        column_type: kind,
        type_source: "inferred",
        count,
        missing,
        min: first.map(|(_, value)| value.to_owned()),
        max: last.map(|(_, value)| value.to_owned()),
        distinct: distinct.len(),
    }
}

/// Parsed tables by digest and delimiter, oldest dropped first once their estimated size passes the budget. A table
/// whose estimate alone passes the budget is not kept, so each read of it parses it again.
pub struct TableCache {
    order: VecDeque<(String, u8)>,
    entries: HashMap<(String, u8), Arc<Table>>,
    bytes: usize,
    budget: usize,
}

impl Default for TableCache {
    fn default() -> Self {
        Self::with_budget(Self::BUDGET)
    }
}

impl TableCache {
    const BUDGET: usize = 64 * 1024 * 1024;

    fn with_budget(budget: usize) -> Self {
        Self {
            order: VecDeque::new(),
            entries: HashMap::new(),
            bytes: 0,
            budget,
        }
    }

    pub fn get(&self, sha256: &str, delimiter: u8) -> Option<Arc<Table>> {
        self.entries.get(&(sha256.to_owned(), delimiter)).cloned()
    }

    pub fn insert(&mut self, table: Arc<Table>) {
        let key = (table.sha256.clone(), table.delimiter);
        let size = table.approx_bytes();
        if size > self.budget || self.entries.contains_key(&key) {
            return;
        }
        self.bytes += size;
        self.order.push_back(key.clone());
        self.entries.insert(key, table);
        // The newest is within the budget, so dropping the oldest first always ends with it kept.
        while self.bytes > self.budget {
            let Some(old) = self.order.pop_front() else {
                break;
            };
            if let Some(table) = self.entries.remove(&old) {
                self.bytes -= table.approx_bytes();
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{Value, json};

    fn table(text: &str) -> Table {
        parse(text.as_bytes(), b',', "a".repeat(64)).unwrap()
    }

    /// A page as the JSON rows a response carries.
    fn page_rows(page: Page<'_>) -> Vec<Value> {
        match serde_json::to_value(page).unwrap() {
            Value::Array(rows) => rows,
            other => panic!("{other}"),
        }
    }

    fn column(rows: &[Value], at: usize) -> Vec<String> {
        rows.iter()
            .map(|row| row["values"][at].as_str().unwrap().to_owned())
            .collect()
    }

    #[test]
    fn values_stay_as_stored_and_rows_name_their_lines() {
        let t =
            table("\u{feff}group,dose,note\nVC,0.50,\"two\nlines\"\n\nOJ,1e1,\"say \"\"hi\"\"\"\n");
        assert_eq!(t.columns, ["group", "dose", "note"]);
        let rows = page_rows(t.page(0, 10, None));
        assert_eq!(
            rows[0],
            json!({"index": 0, "line": 2, "values": ["VC", "0.50", "two\nlines"]})
        );
        assert_eq!(
            rows[1],
            json!({"index": 1, "line": 5, "values": ["OJ", "1e1", "say \"hi\""]}),
            "a blank line is skipped and still counted"
        );
        assert_eq!(
            t.types,
            [ColumnType::String, ColumnType::Number, ColumnType::String]
        );
        let tsv = parse(b"a\tb\n1\t\"x y\"\n", b'\t', "b".repeat(64)).unwrap();
        assert_eq!(
            page_rows(tsv.page(0, 1, None))[0]["values"],
            json!(["1", "x y"])
        );
        for (text, want) in [
            ("a\r\n1\r\n\r\n\r\n2\r\n", [2, 5]),
            ("a\r1\r\r2\r", [2, 4]),
            ("a\n\"x\r\ny\"\n2", [2, 4]),
            ("\n\na\n1\n2\n", [4, 5]),
        ] {
            let t = table(text);
            let lines: Vec<u64> = page_rows(t.page(0, 2, None))
                .iter()
                .map(|row| row["line"].as_u64().unwrap())
                .collect();
            assert_eq!(lines, want, "{text:?}");
        }
        match parse(b"a,b\n\n1,2\n\n3\n", b',', String::new()) {
            Err(TableError::Unreadable(message)) => {
                assert_eq!(message, "line 5 has 1 field; the header has 2")
            }
            _ => panic!("a short row is refused"),
        }
    }

    #[test]
    fn types_follow_every_present_value() {
        let t = table(
            "i,n,l,s,m,mixed\n1,1,TRUE,a,NA,1\n-2,2.5,FALSE,b,,TRUE\n+3,Inf,NA,c,NA,2\n,NaN,TRUE,1,,\n",
        );
        assert_eq!(
            t.types,
            [
                ColumnType::Integer,
                ColumnType::Number,
                ColumnType::Logical,
                ColumnType::String,
                ColumnType::String,
                ColumnType::String
            ]
        );
        for (text, want) in [
            ("1.", true),
            (".5", true),
            ("1e-3", true),
            ("1E+3", true),
            ("1e", false),
            (".", false),
            ("1.2.3", false),
            ("--1", false),
            ("1,5", false),
            ("0x10", false),
            ("inf", false),
        ] {
            assert_eq!(
                class(text) == Class::Number || class(text) == Class::Integer,
                want,
                "{text}"
            );
        }
    }

    #[test]
    fn sorting_uses_the_type_puts_missing_last_and_keeps_ties_in_file_order() {
        let t = table(
            "n,s,k\n10,b,first\n9,B,second\nNA,a,third\n-Inf,b,fourth\n1e1,,fifth\nNaN,c,sixth\n",
        );
        let up = column(&page_rows(t.page(0, 10, Some((0, false)))), 0);
        assert_eq!(up, ["-Inf", "9", "10", "1e1", "NaN", "NA"]);
        let down = page_rows(t.page(0, 10, Some((0, true))));
        assert_eq!(column(&down, 0), ["NaN", "10", "1e1", "9", "-Inf", "NA"]);
        assert_eq!(column(&down, 2)[1..3], ["first", "fifth"]);
        let strings = column(&page_rows(t.page(0, 10, Some((1, false)))), 1);
        assert_eq!(strings, ["B", "a", "b", "b", "c", ""]);
        let page = page_rows(t.page(2, 2, Some((0, false))));
        assert_eq!(
            page.iter()
                .map(|r| r["index"].as_u64().unwrap())
                .collect::<Vec<_>>(),
            [0, 4]
        );
        assert!(page_rows(t.page(99, 5, None)).is_empty());
        assert!(t.has_order(0, true) && !t.has_order(2, false));
        // Integers order exactly at any length, with signs, zeros and leading zeros.
        let big = table(concat!(
            "v\n123456789012345678901234567890123456789012\n-5\n+0007\n-0\n",
            "123456789012345678901234567890123456789011\n-12\n0\n"
        ));
        assert_eq!(big.types, [ColumnType::Integer]);
        assert_eq!(
            column(&page_rows(big.page(0, 9, Some((0, false)))), 0),
            [
                "-12",
                "-5",
                "-0",
                "0",
                "+0007",
                "123456789012345678901234567890123456789011",
                "123456789012345678901234567890123456789012"
            ]
        );
        let zeros = table("x\n0.0\n-0.0\n");
        assert_eq!(
            column(&page_rows(zeros.page(0, 2, Some((0, false)))), 0),
            ["0.0", "-0.0"],
            "-0 equals 0, so file order holds"
        );
    }

    #[test]
    fn summaries_count_present_values_and_keep_stored_extremes() {
        let t = table("dose,ok,name\n0.50,TRUE,a\n2.0,FALSE,b\nNA,TRUE,a\n1,,\n");
        let (columns, receipt) = t.summary();
        assert!(t.summary_ready());
        assert_eq!(columns[0].column_type, ColumnType::Number);
        assert_eq!(
            (columns[0].count, columns[0].missing, columns[0].distinct),
            (3, 1, 3)
        );
        assert_eq!(
            (columns[0].min.as_deref(), columns[0].max.as_deref()),
            (Some("0.50"), Some("2.0"))
        );
        assert_eq!(
            (columns[1].min.as_deref(), columns[1].max.as_deref()),
            (Some("FALSE"), Some("TRUE"))
        );
        assert_eq!(
            (columns[2].count, columns[2].distinct, columns[2].missing),
            (3, 2, 1)
        );
        assert_eq!(columns[0].type_source, "inferred");
        assert_eq!(receipt.input_sha256, "a".repeat(64));
        assert_eq!(
            table("dose,ok,name\n0.50,TRUE,a\n2.0,FALSE,b\nNA,TRUE,a\n1,,\n")
                .summary()
                .1,
            *receipt,
            "the same bytes give the same receipt"
        );
        // The receipt hashes the columns in RFC 8785 form, written out here by hand.
        let one = table("x\n\"tab\there\"\n");
        let written = concat!(
            r#"[{"count":1,"distinct":1,"max":"tab\there","min":"tab\there","missing":0,"#,
            r#""name":"x","type":"string","type_source":"inferred"}]"#
        );
        assert_eq!(
            one.summary().1.output_sha256,
            hex::encode(Sha256::digest(written))
        );
        let wide = table("v\n-0010\n99999999999999999999999999999999999999999\n-9\n");
        assert_eq!(
            (
                wide.summary().0[0].min.as_deref(),
                wide.summary().0[0].max.as_deref()
            ),
            (
                Some("-0010"),
                Some("99999999999999999999999999999999999999999")
            )
        );
        let empty = table("x\nNA\n\"\"\n");
        let column = &empty.summary().0[0];
        assert_eq!(
            (column.column_type, column.min.clone(), column.count),
            (ColumnType::String, None, 0)
        );
    }

    #[test]
    fn ties_keep_file_order_in_large_tables_and_extremes_keep_the_first_stored_string() {
        let mut text = String::from("v,at\n");
        for row in 0..2_000 {
            text.push_str(&format!("{},{row}\n", ["2", "1", "NA", "3"][row % 4]));
        }
        let t = table(&text);
        for descending in [false, true] {
            let rows = page_rows(t.page(0, 2_000, Some((0, descending))));
            for pair in rows.windows(2) {
                if pair[0]["values"][0] == pair[1]["values"][0] {
                    assert!(pair[0]["index"].as_u64() < pair[1]["index"].as_u64());
                }
            }
        }
        let ties = table("v\n10\n1e1\n5\n5.0\n");
        let column = &ties.summary().0[0];
        assert_eq!(
            (
                column.min.as_deref(),
                column.max.as_deref(),
                column.distinct
            ),
            (Some("5"), Some("10"), 4)
        );
    }

    #[test]
    fn unreadable_tables_are_refused_with_the_line() {
        for (bytes, want) in [
            (
                &b"a,b\n1,2\n3\n"[..],
                "line 3 has 1 field; the header has 2",
            ),
            (b"", "the file has no header"),
            (b"a\n\xff\n", "the file is not UTF-8 (byte 2)"),
        ] {
            match parse(bytes, b',', String::new()) {
                Err(TableError::Unreadable(message)) => assert_eq!(message, want),
                other => panic!("{:?}", other.map(|t| t.rows())),
            }
        }
        let wide = format!("{}\n", vec!["c"; MAX_COLUMNS + 1].join(","));
        assert_eq!(
            parse(wide.as_bytes(), b',', String::new()).err(),
            Some(TableError::TooLarge)
        );
        let widest = format!("{}\n", vec!["c"; MAX_COLUMNS].join(","));
        assert_eq!(
            parse(widest.as_bytes(), b',', String::new())
                .unwrap()
                .columns
                .len(),
            MAX_COLUMNS
        );
        // A header of empty fields that fills the input bound is refused when its field room fills.
        let commas = ",".repeat(MAX_TABLE_BYTES as usize - 1);
        assert_eq!(
            parse(commas.as_bytes(), b',', String::new()).err(),
            Some(TableError::TooLarge)
        );
        let row = format!("a,b\n1,2\n{}\n", ",".repeat(5_000));
        match parse(row.as_bytes(), b',', String::new()) {
            Err(TableError::Unreadable(message)) => {
                assert_eq!(
                    message,
                    "line 3 has more than 1000 fields; the header has 2"
                )
            }
            other => panic!("{:?}", other.map(|t| t.rows())),
        }
        assert_eq!(delimiter_for("text/csv; charset=utf-8"), Some(b','));
        assert_eq!(delimiter_for("application/json"), None);
    }

    #[test]
    fn the_cache_keys_by_digest_and_delimiter_and_drops_the_oldest_past_its_budget() {
        let mut cache = TableCache::default();
        let digest = "d".repeat(64);
        let comma = Arc::new(parse(b"a\tb\n1\t2\n", b',', digest.clone()).unwrap());
        cache.insert(comma.clone());
        cache.insert(comma.clone());
        assert_eq!(cache.order.len(), 1);
        assert!(
            cache.get(&digest, b'\t').is_none(),
            "the same bytes read as TSV are another table"
        );
        cache.insert(Arc::new(
            parse(b"a\tb\n1\t2\n", b'\t', digest.clone()).unwrap(),
        ));
        assert_eq!(cache.get(&digest, b',').unwrap().columns, ["a\tb"]);
        assert_eq!(cache.get(&digest, b'\t').unwrap().columns, ["a", "b"]);
        cache.bytes = cache.budget;
        cache.insert(Arc::new(parse(b"y\n2\n", b',', "c".repeat(64)).unwrap()));
        assert!(cache.get(&digest, b',').is_none(), "the oldest went first");
        assert!(cache.get(&digest, b'\t').is_some());
        assert!(cache.get(&"c".repeat(64), b',').is_some());
    }

    #[test]
    fn a_table_past_the_budget_is_not_kept_and_pushes_nothing_out() {
        let table = |text: &str, digest: char| {
            Arc::new(parse(text.as_bytes(), b',', digest.to_string().repeat(64)).unwrap())
        };
        let kept = |cache: &TableCache, digest: char| {
            cache.get(&digest.to_string().repeat(64), b',').is_some()
        };
        let size = table("x\n1\n", 'a').approx_bytes();
        let mut cache = TableCache::with_budget(2 * size);
        cache.insert(table("x\n1\n", 'a'));
        cache.insert(table("x\n2\n", 'b'));
        assert!(kept(&cache, 'a') && kept(&cache, 'b'));
        cache.insert(table("x\n3\n", 'c'));
        assert!(!kept(&cache, 'a'), "the oldest went first");
        assert!(kept(&cache, 'b') && kept(&cache, 'c'));
        let large = table(&format!("x\n{}\n", "9".repeat(2 * size)), 'd');
        assert!(large.approx_bytes() > cache.budget);
        cache.insert(large);
        assert!(!kept(&cache, 'd'));
        assert!(kept(&cache, 'b') && kept(&cache, 'c'));
        assert_eq!(cache.bytes, 2 * size);
    }

    #[test]
    fn the_budget_counts_names_and_the_summary_a_table_can_hold() {
        let plain = parse(b"a\n1\n", b',', String::new())
            .unwrap()
            .approx_bytes();
        let name = "n".repeat(1 << 20);
        let named = parse(format!("{name}\n1\n").as_bytes(), b',', String::new()).unwrap();
        // Against a table whose name and value are one byte long.
        assert!(
            named.approx_bytes() - plain >= 2 * (name.len() - 1),
            "the name and its copy in the summary"
        );
        let value = "v".repeat(1 << 20);
        let long = parse(format!("a\n{value}\n").as_bytes(), b',', String::new()).unwrap();
        assert!(
            long.approx_bytes() - plain >= 3 * (value.len() - 1),
            "the value and its copies as least and most"
        );
        let before = long.approx_bytes();
        let (columns, _) = long.summary();
        let held = columns
            .iter()
            .map(|c| {
                c.name.len()
                    + c.min.as_ref().map_or(0, String::len)
                    + c.max.as_ref().map_or(0, String::len)
            })
            .sum::<usize>();
        assert!(
            held + long.data.len() <= before,
            "the estimate made before the summary covers it"
        );
    }
}
