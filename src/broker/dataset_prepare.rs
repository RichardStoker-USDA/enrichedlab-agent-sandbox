//! Prepare a lab dataset for analysis (the `derived` state): apply a typed preparation, which the person who
//! approves it can read in full, to one sheet of the upload guard's cell grid, and write the table the template analyses
//! and figures read, a copy safe to open in a spreadsheet, and a receipt that counts every change.
//!
//! The executor is deterministic and changes a value only by a rule the preparation names. Numbers keep the digits the
//! file holds: a decimal comma becomes a point and named thousands separators are removed, and nothing is rounded. A
//! cell no rule reads (`dead` in a number column, a spreadsheet error, a date whose day and month order is unset) stops
//! the preparation, and the receipt names its row, column and reason, unless the column says to read such cells as
//! missing, when each is counted and the first are listed. A value reported against a limit (`<0.5`, `ND`) is never
//! read as missing by default: the column's censoring rule chooses its value and two more columns keep which limit it
//! was and in which direction.
//!
//! The table is CSV in rules version 1 (`docs/result_tables.md`): a header of the preparation's names, `NA` for a missing
//! value, text quoted when it is empty, `NA`, or holds a comma, a quote or a line break. The spreadsheet copy starts with
//! a byte-order mark and puts `'` before text that starts with `=`, `+`, `-`, `@`, a tab or a carriage return, so a
//! spreadsheet program shows it as text rather than running it.
use std::collections::{BTreeMap, HashSet};
use std::io::Read;
use std::sync::LazyLock;

use regex::Regex;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};

use super::cell_grid::{Cell, GridError, GridReader, Line};
use super::dataset_profile::{Censor, DMY, LIMIT, MAX_GRID_BYTES, MAX_GRID_LINE, censor_token};

pub const SPEC_SCHEMA: &str = "enrichedlab.dataset-preparation.v1";
pub const RECEIPT_SCHEMA: &str = "enrichedlab.dataset-preparation-receipt.v1";
pub const EXECUTOR: &str = "enrichedlab.dataset-prepare";
pub const EXECUTOR_VERSION: u32 = 2;

/// The prepared table must fit what the workspace pages and summarizes (`tables.rs`).
pub const MAX_TABLE_BYTES: usize = 16 * 1024 * 1024;
pub const MAX_ROWS: u64 = 1_000_000;
pub const MAX_CELLS: u64 = 2_000_000;
pub const MAX_OUTPUT_COLUMNS: usize = 1_000;
/// Failures listed in a receipt; every one is counted.
const MAX_FAILURES: usize = 20;
const MAX_FAILURE_CHARS: usize = 120;
const MAX_SPEC_COLUMNS: usize = 512;
const MAX_RECODES: usize = 2_000;
/// Recodes a column's counts list by text; the rest are counted together.
const MAX_RECODES_LISTED: usize = 200;
const MAX_TOKEN_CHARS: usize = 64;
const MAX_TEXT_CHARS: usize = 200;
const MAX_KEY_ROWS: usize = 1_000_000;

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Spec {
    pub schema: String,
    /// The table's file name without `.csv`.
    pub table_name: String,
    /// The sheet's index in the grid, 0 for a text file.
    pub sheet: usize,
    /// Where the names were read from, recorded for the lineage; the names themselves are the columns' names.
    #[serde(default)]
    pub header_rows: Vec<u64>,
    #[serde(default)]
    pub units_row: Option<u64>,
    pub rows: RowRange,
    /// Rows to leave out, as inclusive `[first, last]` ranges inside `rows`. Blank rows are always left out.
    #[serde(default)]
    pub drop_rows: Vec<[u64; 2]>,
    /// Texts read as missing in every column, compared after leading and trailing spaces are dropped. An empty cell is
    /// always missing.
    pub missing: Vec<String>,
    /// How numbers are written unless a column says otherwise.
    #[serde(default)]
    pub decimal: Decimal,
    #[serde(default)]
    pub thousands: Thousands,
    /// The order of day and month in dates written as text with slashes, dots or dashes.
    #[serde(default)]
    pub date_order: Option<DateOrder>,
    /// Leading and trailing spaces are dropped from text, and each change counted.
    #[serde(default = "yes")]
    pub trim: bool,
    pub columns: Vec<ColumnSpec>,
    /// Columns that hold one measure at several points, read into two columns: the point and the value.
    #[serde(default)]
    pub long: Option<LongSpec>,
    /// Columns whose values together must not repeat.
    #[serde(default)]
    pub key: Vec<String>,
}

fn yes() -> bool {
    true
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct RowRange {
    pub first: u64,
    pub last: u64,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Decimal {
    #[default]
    Point,
    Comma,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Thousands {
    #[default]
    None,
    Comma,
    Point,
    Space,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum DateOrder {
    DayFirst,
    MonthFirst,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ColumnType {
    Text,
    Category,
    Number,
    Integer,
    Logical,
    Date,
    Datetime,
    Time,
}

impl ColumnType {
    fn name(self) -> &'static str {
        match self {
            ColumnType::Text => "text",
            ColumnType::Category => "category",
            ColumnType::Number => "number",
            ColumnType::Integer => "integer",
            ColumnType::Logical => "logical",
            ColumnType::Date => "date",
            ColumnType::Datetime => "datetime",
            ColumnType::Time => "time",
        }
    }

    fn numeric(self) -> bool {
        matches!(self, ColumnType::Number | ColumnType::Integer)
    }
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Other {
    /// A cell no rule reads stops the preparation.
    #[default]
    Refuse,
    /// A cell no rule reads is written as missing, counted and listed.
    Missing,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct ColumnSpec {
    /// The source column's letter; left out for a long layout's value.
    #[serde(default)]
    pub from: String,
    pub name: String,
    #[serde(rename = "type")]
    pub kind: ColumnType,
    #[serde(default)]
    pub unit: Option<String>,
    #[serde(default)]
    pub decimal: Option<Decimal>,
    #[serde(default)]
    pub thousands: Option<Thousands>,
    /// Text read as another text, after trimming: `male` to `Male`.
    #[serde(default)]
    pub recode: BTreeMap<String, String>,
    #[serde(default)]
    pub censored: Option<CensorSpec>,
    /// A trailing `%` is dropped from numbers.
    #[serde(default)]
    pub strip_percent: bool,
    #[serde(default)]
    pub true_values: Vec<String>,
    #[serde(default)]
    pub false_values: Vec<String>,
    #[serde(default)]
    pub other: Other,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct CensorSpec {
    pub rule: CensorRule,
    /// The limit for a value reported without one, such as `<LOD` or `ND`: the assay's lower limit.
    #[serde(default)]
    pub limit: Option<f64>,
    /// The upper limit for a value reported above range without one, such as `>ULOQ`.
    #[serde(default)]
    pub upper_limit: Option<f64>,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum CensorRule {
    /// The value is missing; the limit and direction are kept beside it.
    Missing,
    /// The value is the limit.
    Limit,
    /// The value is half the lower limit (the limit itself above the range).
    HalfLimit,
    /// The value is the lower limit divided by the square root of two (the limit itself above the range).
    LimitOverSqrt2,
}

impl CensorRule {
    fn name(self) -> &'static str {
        match self {
            CensorRule::Missing => "missing",
            CensorRule::Limit => "limit",
            CensorRule::HalfLimit => "half_limit",
            CensorRule::LimitOverSqrt2 => "limit_over_sqrt2",
        }
    }
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct LongSpec {
    /// The source columns in order, one per point.
    pub columns: Vec<String>,
    /// The point column's name and each column's point, as numbers or texts.
    pub name: String,
    pub values: Vec<Value>,
    pub value: ColumnSpec,
}

/// Why a preparation is not valid, with where in it.
#[derive(Debug, PartialEq, Eq)]
pub struct SpecError {
    pub path: String,
    pub reason: &'static str,
}

fn invalid(path: impl Into<String>, reason: &'static str) -> SpecError {
    SpecError {
        path: path.into(),
        reason,
    }
}

/// The specification's fields that are maps keyed by the sender's text: a refusal's place ends at them.
const TEXT_KEYED_FIELDS: &[&str] = &["recode"];

/// The specification's own field names: a refusal's place is built from these and array indexes only. Every other
/// name in a specification is the sender's text (a recode key is a cell's text), so a place stops before it.
const SPEC_FIELDS: &[&str] = &[
    "schema",
    "table_name",
    "sheet",
    "header_rows",
    "units_row",
    "rows",
    "first",
    "last",
    "drop_rows",
    "missing",
    "decimal",
    "thousands",
    "date_order",
    "trim",
    "columns",
    "long",
    "key",
    "from",
    "name",
    "type",
    "unit",
    "recode",
    "censored",
    "strip_percent",
    "true_values",
    "false_values",
    "other",
    "rule",
    "limit",
    "upper_limit",
    "values",
    "value",
];

/// Where and why a specification did not parse: `missing_field` or `unknown_field` with the field's place,
/// `unknown_choice` or `wrong_type` at the value, `not_a_preparation` otherwise. A place holds only the specification's
/// field names and indexes, up to the first name it does not own; an unknown field is placed at its parent.
fn parse_refusal(error: serde_path_to_error::Error<serde_json::Error>) -> SpecError {
    use serde_path_to_error::Segment;
    let message = error.inner().to_string();
    let missing = message
        .strip_prefix("missing field `")
        .and_then(|rest| rest.split('`').next())
        .filter(|name| SPEC_FIELDS.contains(name));
    let reason = if message.starts_with("missing field `") {
        "missing_field"
    } else if message.starts_with("unknown field `") {
        "unknown_field"
    } else if message.starts_with("unknown variant") {
        "unknown_choice"
    } else if message.starts_with("invalid ") {
        "wrong_type"
    } else {
        "not_a_preparation"
    };
    // Each owned step with its separator; the walk stops at the first name the specification does not own, and inside
    // a map whose keys are the sender's text (a recode key is a cell's text) whatever the next key spells, even a field
    // name.
    let mut steps: Vec<String> = Vec::new();
    let mut complete = true;
    let mut in_text_map = false;
    for segment in error.path().iter() {
        match segment {
            _ if in_text_map => {
                complete = false;
                break;
            }
            Segment::Seq { index } => steps.push(format!("[{index}]")),
            Segment::Map { key } if SPEC_FIELDS.contains(&key.as_str()) => {
                steps.push(format!(".{key}"));
                in_text_map = TEXT_KEYED_FIELDS.contains(&key.as_str());
            }
            _ => {
                complete = false;
                break;
            }
        }
    }
    // serde_path_to_error ends an unknown field's place with the field itself: the sender's text, so it is dropped.
    if reason == "unknown_field" && complete {
        steps.pop();
    }
    let mut path: String = steps.concat();
    if path.starts_with('.') {
        path.remove(0);
    }
    if let Some(field) = missing
        && !path.ends_with(field)
    {
        if !path.is_empty() {
            path.push('.');
        }
        path.push_str(field);
    }
    invalid(path, reason)
}

static NAME: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^[a-z][a-z0-9_]{0,62}$").unwrap());
static LETTERS: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^[A-Z]{1,3}$").unwrap());

/// A column's index from its letters; none past Excel's last column, `XFD`.
pub fn column_index(letters: &str) -> Option<usize> {
    if !LETTERS.is_match(letters) {
        return None;
    }
    let mut index = 0_usize;
    for b in letters.bytes() {
        index = index * 26 + (b - b'A' + 1) as usize;
    }
    (index <= 16_384).then(|| index - 1)
}

fn plain(text: &str, max: usize) -> bool {
    text.chars().count() <= max && !text.chars().any(char::is_control)
}

/// A text the result digest and R read as a missing value once written as a cell: blank, or exactly `NA`.
fn reads_as_missing(text: &str) -> bool {
    text.trim().is_empty() || text == "NA"
}

/// An output column: its name, type, unit and where it came from.
#[derive(Clone, Debug, Serialize, PartialEq)]
pub struct OutputColumn {
    pub name: String,
    #[serde(rename = "type")]
    pub kind: &'static str,
    pub unit: Option<String>,
    /// The source column's letter, `long` for the point and value of a long layout, or the censored column's name for
    /// its direction and limit.
    pub from: String,
}

impl Spec {
    /// Read and check a preparation. The checks need no grid; the executor checks it against the sheet.
    pub fn parse(value: &Value) -> Result<Spec, SpecError> {
        let spec: Spec = serde_path_to_error::deserialize(value.clone()).map_err(parse_refusal)?;
        spec.check()?;
        Ok(spec)
    }

    fn check(&self) -> Result<(), SpecError> {
        if self.schema != SPEC_SCHEMA {
            return Err(invalid("schema", "unknown_schema"));
        }
        if !NAME.is_match(&self.table_name) {
            return Err(invalid("table_name", "not_a_name"));
        }
        if self.sheet >= 64 {
            return Err(invalid("sheet", "no_such_sheet"));
        }
        let RowRange { first, last } = self.rows;
        if first < 1 || last < first || last > 1_048_576 {
            return Err(invalid("rows", "rows_out_of_order"));
        }
        if self.header_rows.len() > 2
            || self.header_rows.iter().any(|r| *r < 1 || *r >= first)
            || self.header_rows.windows(2).any(|w| w[0] >= w[1])
        {
            return Err(invalid("header_rows", "rows_out_of_order"));
        }
        if self.units_row.is_some_and(|r| r < 1 || r >= first) {
            return Err(invalid("units_row", "rows_out_of_order"));
        }
        if self.drop_rows.len() > 1_000 {
            return Err(invalid("drop_rows", "too_many"));
        }
        for (i, [a, b]) in self.drop_rows.iter().enumerate() {
            if a > b || *a < first || *b > last {
                return Err(invalid(format!("drop_rows[{i}]"), "rows_out_of_order"));
            }
        }
        if self.missing.len() > 32 {
            return Err(invalid("missing", "too_many"));
        }
        for (i, token) in self.missing.iter().enumerate() {
            if !plain(token, MAX_TOKEN_CHARS) || token.trim() != token {
                return Err(invalid(format!("missing[{i}]"), "not_a_token"));
            }
        }
        if self.columns.is_empty() || self.columns.len() > MAX_SPEC_COLUMNS {
            return Err(invalid("columns", "too_many"));
        }
        let mut sources = HashSet::new();
        for (i, column) in self.columns.iter().enumerate() {
            let path = format!("columns[{i}]");
            if column_index(&column.from).is_none() {
                return Err(invalid(format!("{path}.from"), "not_a_column"));
            }
            if !sources.insert(column.from.as_str()) {
                return Err(invalid(format!("{path}.from"), "column_repeated"));
            }
            column.check(&path)?;
        }
        if let Some(long) = &self.long {
            if long.columns.len() < 2 || long.columns.len() > MAX_SPEC_COLUMNS {
                return Err(invalid("long.columns", "too_many"));
            }
            for (i, from) in long.columns.iter().enumerate() {
                if column_index(from).is_none() {
                    return Err(invalid(format!("long.columns[{i}]"), "not_a_column"));
                }
                if !sources.insert(from.as_str()) {
                    return Err(invalid(format!("long.columns[{i}]"), "column_repeated"));
                }
            }
            if long.values.len() != long.columns.len() {
                return Err(invalid("long.values", "values_do_not_match_columns"));
            }
            let numbers = long.values.iter().all(Value::is_number);
            let texts = long.values.iter().all(|v| {
                v.as_str()
                    .is_some_and(|t| !t.is_empty() && plain(t, MAX_TOKEN_CHARS))
            });
            if !numbers && !texts {
                return Err(invalid("long.values", "values_mixed"));
            }
            // A label is written as a text cell, and a reader takes a blank one or `NA` for a missing value.
            if texts
                && long
                    .values
                    .iter()
                    .filter_map(Value::as_str)
                    .any(reads_as_missing)
            {
                return Err(invalid("long.values", "value_reads_as_missing"));
            }
            let mut seen = HashSet::new();
            if !long.values.iter().all(|v| seen.insert(v.to_string())) {
                return Err(invalid("long.values", "values_repeated"));
            }
            if !NAME.is_match(&long.name) {
                return Err(invalid("long.name", "not_a_name"));
            }
            if !long.value.from.is_empty() {
                return Err(invalid("long.value.from", "not_allowed"));
            }
            long.value.check("long.value")?;
        }
        let names: Vec<String> = self.outputs().into_iter().map(|c| c.name).collect();
        if names.len() > MAX_OUTPUT_COLUMNS {
            return Err(invalid("columns", "too_many"));
        }
        let mut unique = HashSet::new();
        if let Some(repeated) = names.iter().find(|n| !unique.insert(n.as_str())) {
            return Err(invalid(format!("name:{repeated}"), "name_repeated"));
        }
        if self.key.len() > 8 {
            return Err(invalid("key", "too_many"));
        }
        for (i, name) in self.key.iter().enumerate() {
            if !names.contains(name) {
                return Err(invalid(format!("key[{i}]"), "not_a_column"));
            }
        }
        Ok(())
    }

    /// The table's columns in order: the columns, each censored one followed by its direction and limit, then a long
    /// layout's point and value.
    pub fn outputs(&self) -> Vec<OutputColumn> {
        let mut out = Vec::new();
        let mut push = |column: &ColumnSpec, from: &str| {
            out.push(OutputColumn {
                name: column.name.clone(),
                kind: column.kind.name(),
                unit: column.unit.clone(),
                from: from.to_owned(),
            });
            if column.censored.is_some() {
                out.push(OutputColumn {
                    name: format!("{}_censored", column.name),
                    kind: "category",
                    unit: None,
                    from: column.name.clone(),
                });
                out.push(OutputColumn {
                    name: format!("{}_limit", column.name),
                    kind: "number",
                    unit: column.unit.clone(),
                    from: column.name.clone(),
                });
            }
        };
        for column in &self.columns {
            push(column, &column.from);
        }
        if let Some(long) = &self.long {
            let point = OutputColumn {
                name: long.name.clone(),
                kind: if long.values.iter().all(Value::is_number) {
                    "number"
                } else {
                    "category"
                },
                unit: None,
                from: "long".into(),
            };
            let value_spec = &long.value;
            let mut value_columns = Vec::new();
            {
                let mut push_value = |c: OutputColumn| value_columns.push(c);
                push_value(OutputColumn {
                    name: value_spec.name.clone(),
                    kind: value_spec.kind.name(),
                    unit: value_spec.unit.clone(),
                    from: "long".into(),
                });
                if value_spec.censored.is_some() {
                    push_value(OutputColumn {
                        name: format!("{}_censored", value_spec.name),
                        kind: "category",
                        unit: None,
                        from: value_spec.name.clone(),
                    });
                    push_value(OutputColumn {
                        name: format!("{}_limit", value_spec.name),
                        kind: "number",
                        unit: value_spec.unit.clone(),
                        from: value_spec.name.clone(),
                    });
                }
            }
            out.push(point);
            out.extend(value_columns);
        }
        out
    }

    fn decimal(&self, column: &ColumnSpec) -> Decimal {
        column.decimal.unwrap_or(self.decimal)
    }

    fn thousands(&self, column: &ColumnSpec) -> Thousands {
        column.thousands.unwrap_or(self.thousands)
    }
}

impl ColumnSpec {
    fn check(&self, path: &str) -> Result<(), SpecError> {
        if !NAME.is_match(&self.name) {
            return Err(invalid(format!("{path}.name"), "not_a_name"));
        }
        if self
            .unit
            .as_deref()
            .is_some_and(|u| u.is_empty() || !plain(u, 32))
        {
            return Err(invalid(format!("{path}.unit"), "not_a_unit"));
        }
        let text = matches!(self.kind, ColumnType::Text | ColumnType::Category);
        if !self.recode.is_empty() && !text {
            return Err(invalid(format!("{path}.recode"), "not_for_this_type"));
        }
        if self.recode.len() > MAX_RECODES
            || self
                .recode
                .iter()
                .any(|(from, to)| !plain(from, MAX_TEXT_CHARS) || !plain(to, MAX_TEXT_CHARS))
        {
            return Err(invalid(format!("{path}.recode"), "not_a_recode"));
        }
        // A recode writes its target as a text cell: a blank target or `NA` would be read as missing, which the
        // missing list says plainly.
        if self.recode.values().any(|to| reads_as_missing(to)) {
            return Err(invalid(format!("{path}.recode"), "value_reads_as_missing"));
        }
        if let Some(censored) = &self.censored {
            if !self.kind.numeric() {
                return Err(invalid(format!("{path}.censored"), "not_for_this_type"));
            }
            if self.kind == ColumnType::Integer
                && matches!(
                    censored.rule,
                    CensorRule::HalfLimit | CensorRule::LimitOverSqrt2
                )
            {
                return Err(invalid(
                    format!("{path}.censored.rule"),
                    "not_for_this_type",
                ));
            }
            let positive = |v: Option<f64>| v.is_none_or(|v| v.is_finite() && v > 0.0);
            if !positive(censored.limit) || !positive(censored.upper_limit) {
                return Err(invalid(format!("{path}.censored"), "not_a_limit"));
            }
            // An integer column's stated limits stand in for values, so they are whole numbers R reads exactly.
            let whole =
                |v: Option<f64>| v.is_none_or(|v| v.fract() == 0.0 && v < 9_007_199_254_740_992.0);
            if self.kind == ColumnType::Integer {
                if !whole(censored.limit) {
                    return Err(invalid(format!("{path}.censored.limit"), "not_an_integer"));
                }
                if !whole(censored.upper_limit) {
                    return Err(invalid(
                        format!("{path}.censored.upper_limit"),
                        "not_an_integer",
                    ));
                }
            }
        }
        if (self.decimal.is_some() || self.thousands.is_some() || self.strip_percent)
            && !self.kind.numeric()
        {
            return Err(invalid(path, "not_for_this_type"));
        }
        if (!self.true_values.is_empty() || !self.false_values.is_empty())
            && self.kind != ColumnType::Logical
        {
            return Err(invalid(path, "not_for_this_type"));
        }
        let mut seen = HashSet::new();
        for token in self.true_values.iter().chain(&self.false_values) {
            if !plain(token, MAX_TOKEN_CHARS) || token.trim() != token || !seen.insert(token) {
                return Err(invalid(path, "not_a_token"));
            }
        }
        Ok(())
    }
}

/// What a preparation wrote.
#[derive(Debug)]
pub struct Prepared {
    pub table: Vec<u8>,
    pub spreadsheet: Vec<u8>,
    pub receipt: Receipt,
}

/// Why a preparation stopped, with its receipt when the sheet was read.
#[derive(Debug)]
pub struct Refusal {
    pub code: &'static str,
    pub receipt: Option<Box<Receipt>>,
}

fn refused(code: &'static str) -> Refusal {
    Refusal {
        code,
        receipt: None,
    }
}

#[derive(Clone, Debug, Default, Serialize, PartialEq)]
pub struct Receipt {
    pub schema: &'static str,
    pub executor: Executor,
    pub status: &'static str,
    pub grid_sha256: String,
    pub spec_sha256: String,
    pub rows: RowCounts,
    pub cells: CellCounts,
    pub columns: Vec<ColumnCounts>,
    pub outputs: Vec<OutputColumn>,
    /// The first cells no rule read, with the row and column as a spreadsheet names them.
    pub failures: Vec<Failure>,
    pub failure_count: u64,
    /// The first cells no rule read that a column wrote as missing, as it says to.
    pub read_as_missing: Vec<Failure>,
    /// The first repeated keys, by the first row of each repeat.
    pub repeated_keys: Vec<RepeatedKey>,
    pub repeated_key_count: u64,
    pub table: Option<FileFacts>,
    pub spreadsheet_copy: Option<FileFacts>,
}

#[derive(Clone, Debug, Default, Serialize, PartialEq)]
pub struct Executor {
    pub name: &'static str,
    pub version: u32,
}

#[derive(Clone, Debug, Default, Serialize, PartialEq)]
pub struct RowCounts {
    /// Rows from `rows.first` to `rows.last` that the sheet holds.
    pub read: u64,
    pub blank: u64,
    pub dropped: u64,
    /// Source rows kept; with a long layout each gives one table row per point.
    pub kept: u64,
    pub written: u64,
}

/// Every filled cell of the sheet by what the preparation did with it; `filled` is the sum of the other four.
#[derive(Clone, Debug, Default, Serialize, PartialEq)]
pub struct CellCounts {
    pub filled: u64,
    /// In rows before `rows.first` or after `rows.last`: titles, header and units rows, notes.
    pub outside_rows: u64,
    pub in_dropped_rows: u64,
    /// In kept rows, in columns no column or long rule reads.
    pub in_unread_columns: u64,
    /// Read by a column rule; each is then counted in its column as a value, a missing value, a censored value or a
    /// cell no rule read.
    pub read: u64,
}

#[derive(Clone, Debug, Default, Serialize, PartialEq)]
pub struct ColumnCounts {
    pub name: String,
    pub from: String,
    #[serde(rename = "type")]
    pub kind: &'static str,
    pub values: u64,
    /// Missing cells by what they held: `""` for an empty cell, else the token.
    pub missing: BTreeMap<String, u64>,
    pub trimmed: u64,
    /// Texts read as another text, by the text and what it became, for at most 200 pairs.
    pub recoded: BTreeMap<String, u64>,
    /// Recoded cells of pairs past the listed 200.
    pub recoded_unlisted: u64,
    pub decimal_commas: u64,
    pub thousands_separators: u64,
    pub percent_signs: u64,
    /// Numbers written without the leading `+` they had.
    pub plus_signs: u64,
    pub dates_from_text: u64,
    /// Censored values by direction, and the rule that chose their value.
    pub censored: BTreeMap<&'static str, u64>,
    pub censored_rule: Option<&'static str>,
    /// Cells no rule read, written as missing because the column says so.
    pub unread_as_missing: u64,
}

#[derive(Clone, Debug, Serialize, PartialEq)]
pub struct Failure {
    pub row: u64,
    pub column: String,
    pub reason: &'static str,
    pub text: String,
}

#[derive(Clone, Debug, Serialize, PartialEq)]
pub struct RepeatedKey {
    pub row: u64,
    pub first_row: u64,
}

#[derive(Clone, Debug, Serialize, PartialEq)]
pub struct FileFacts {
    pub path: String,
    pub sha256: String,
    pub bytes: u64,
    pub rows: u64,
}

/// One converted cell.
enum Out {
    Missing,
    Value(String),
    Censored {
        direction: &'static str,
        limit: Option<String>,
        value: Option<String>,
    },
}

/// A CSV field: missing, a value written as is (numbers, dates, logicals), or text quoted as needed.
enum Field<'a> {
    Missing,
    Plain(&'a str),
    Text(&'a str),
}

struct Writer {
    table: String,
    spreadsheet: String,
    rows: u64,
    cells: u64,
}

impl Writer {
    fn new() -> Self {
        Writer {
            table: String::new(),
            spreadsheet: String::from("\u{feff}"),
            rows: 0,
            cells: 0,
        }
    }

    fn quoted(text: &str) -> bool {
        text.is_empty() || text == "NA" || text.contains([',', '"', '\r', '\n'])
    }

    fn put(out: &mut String, text: &str, quote: bool) {
        if quote {
            out.push('"');
            out.push_str(&text.replace('"', "\"\""));
            out.push('"');
        } else {
            out.push_str(text);
        }
    }

    /// Write one line; false when the table would pass its bounds.
    fn line(&mut self, fields: &[Field<'_>], header: bool) -> bool {
        for (i, field) in fields.iter().enumerate() {
            if i > 0 {
                self.table.push(',');
                self.spreadsheet.push(',');
            }
            match field {
                Field::Missing => {
                    self.table.push_str("NA");
                    self.spreadsheet.push_str("NA");
                }
                Field::Plain(text) => {
                    self.table.push_str(text);
                    self.spreadsheet.push_str(text);
                }
                Field::Text(text) => {
                    // Every source of a text cell is checked before it is written (cells as they are read, labels and
                    // recode targets with the specification).
                    debug_assert!(
                        header || !reads_as_missing(text),
                        "a text cell reads as missing"
                    );
                    let quote = Self::quoted(text);
                    Self::put(&mut self.table, text, quote);
                    if !header && text.starts_with(['=', '+', '-', '@', '\t', '\r']) {
                        Self::put(&mut self.spreadsheet, &format!("'{text}"), true);
                    } else {
                        Self::put(&mut self.spreadsheet, text, quote);
                    }
                }
            }
        }
        self.table.push('\n');
        self.spreadsheet.push('\n');
        if !header {
            self.rows += 1;
            self.cells += fields.len() as u64;
        }
        self.table.len() <= MAX_TABLE_BYTES && self.rows <= MAX_ROWS && self.cells <= MAX_CELLS
    }
}

/// The executor's state for one column.
struct Column<'a> {
    spec: &'a ColumnSpec,
    index: Option<usize>,
    letter: String,
    counts: ColumnCounts,
}

/// Prepare the sheet the spec names from the grid's bytes.
pub fn prepare<R: Read>(
    grid: R,
    grid_sha256: &str,
    spec: &Spec,
    spec_sha256: &str,
) -> Result<Prepared, Refusal> {
    let mut reader =
        GridReader::open(grid, MAX_GRID_LINE, MAX_GRID_BYTES).map_err(|e| match e {
            GridError::Unreadable => refused("dataset_grid_unreadable"),
            GridError::TooLarge => refused("dataset_grid_too_large"),
        })?;
    let Some(sheet) = reader.source.sheets.get(spec.sheet) else {
        return Err(refused("dataset_sheet_missing"));
    };
    let width = sheet.columns as usize;
    let outside = spec
        .columns
        .iter()
        .map(|c| c.from.as_str())
        .chain(
            spec.long
                .iter()
                .flat_map(|l| l.columns.iter().map(String::as_str)),
        )
        .any(|from| column_index(from).is_none_or(|i| i >= width.max(1)));
    let mut selected = vec![false; width];
    for from in spec.columns.iter().map(|c| c.from.as_str()).chain(
        spec.long
            .iter()
            .flat_map(|l| l.columns.iter().map(String::as_str)),
    ) {
        if let Some(slot) = column_index(from).and_then(|i| selected.get_mut(i)) {
            *slot = true;
        }
    }
    // Every row the specification names is in the sheet: its range, so its drops, header rows and units row too.
    if outside || spec.rows.first > sheet.rows.max(1) || spec.rows.last > sheet.rows {
        return Err(refused("dataset_spec_outside_sheet"));
    }
    let mut receipt = Receipt {
        schema: RECEIPT_SCHEMA,
        executor: Executor {
            name: EXECUTOR,
            version: EXECUTOR_VERSION,
        },
        status: "refused",
        grid_sha256: grid_sha256.to_owned(),
        spec_sha256: spec_sha256.to_owned(),
        outputs: spec.outputs(),
        ..Receipt::default()
    };
    let outputs = receipt.outputs.clone();
    let mut columns: Vec<Column<'_>> = spec
        .columns
        .iter()
        .map(|c| Column::new(c, column_index(&c.from), c.from.clone()))
        .collect();
    let mut long_columns: Vec<Column<'_>> = spec
        .long
        .iter()
        .flat_map(|l| {
            l.columns
                .iter()
                .map(move |from| Column::new(&l.value, column_index(from), from.clone()))
        })
        .collect();
    let points: Vec<String> = spec
        .long
        .iter()
        .flat_map(|l| l.values.iter().map(point_text))
        .collect();
    let key_at: Vec<usize> = spec
        .key
        .iter()
        .filter_map(|k| outputs.iter().position(|o| &o.name == k))
        .collect();
    let mut writer = Writer::new();
    let header: Vec<Field<'_>> = outputs.iter().map(|o| Field::Text(&o.name)).collect();
    writer.line(&header, true);
    let mut keys: std::collections::HashMap<u128, u64> = std::collections::HashMap::new();
    let mut failures: Vec<Failure> = Vec::new();
    let mut read_as_missing: Vec<Failure> = Vec::new();
    let mut failure_count = 0_u64;
    let mut too_large = false;
    let (mut current, mut row_number) = (None, 0_u64);
    let missing: HashSet<&str> = spec.missing.iter().map(String::as_str).collect();
    let ctx = Context {
        spec,
        missing: &missing,
    };
    loop {
        let line = match reader.next_line() {
            Ok(Some(line)) => line,
            Ok(None) => break,
            Err(GridError::Unreadable) => return Err(refused("dataset_grid_unreadable")),
            Err(GridError::TooLarge) => return Err(refused("dataset_grid_too_large")),
        };
        let row = match line {
            Line::Sheet(index) => {
                if current == Some(spec.sheet) {
                    break;
                }
                current = Some(index);
                row_number = 0;
                continue;
            }
            Line::Row(row) => row,
        };
        if current != Some(spec.sheet) {
            continue;
        }
        row_number += 1;
        // Every filled cell of the sheet is accounted for: outside the rows read, in a dropped row, in a column no
        // rule reads, or read by a column rule (and then counted in its column).
        let filled = row.iter().filter(|c| !c.is_blank()).count() as u64;
        receipt.cells.filled += filled;
        if row_number < spec.rows.first || row_number > spec.rows.last {
            receipt.cells.outside_rows += filled;
            continue;
        }
        receipt.rows.read += 1;
        if spec
            .drop_rows
            .iter()
            .any(|[a, b]| (*a..=*b).contains(&row_number))
        {
            receipt.rows.dropped += 1;
            receipt.cells.in_dropped_rows += filled;
            continue;
        }
        if filled == 0 {
            receipt.rows.blank += 1;
            continue;
        }
        receipt.rows.kept += 1;
        for (c, cell) in row.iter().enumerate() {
            if !cell.is_blank() {
                if selected.get(c).copied().unwrap_or(false) {
                    receipt.cells.read += 1;
                } else {
                    receipt.cells.in_unread_columns += 1;
                }
            }
        }
        let mut failed = false;
        let mut note = |letter: &str,
                        read: Reading,
                        failures: &mut Vec<Failure>,
                        as_missing: &mut Vec<Failure>| match read {
            Reading::Value(out) => out,
            Reading::AsMissing((reason, text)) => {
                if as_missing.len() < MAX_FAILURES {
                    as_missing.push(Failure {
                        row: row_number,
                        column: letter.to_owned(),
                        reason,
                        text,
                    });
                }
                Out::Missing
            }
            Reading::Failed((reason, text)) => {
                failed = true;
                failure_count += 1;
                if failures.len() < MAX_FAILURES {
                    failures.push(Failure {
                        row: row_number,
                        column: letter.to_owned(),
                        reason,
                        text,
                    });
                }
                Out::Missing
            }
        };
        let mut fixed: Vec<Out> = Vec::with_capacity(columns.len());
        for column in &mut columns {
            let read = column.read(&row, &ctx);
            fixed.push(note(
                &column.letter,
                read,
                &mut failures,
                &mut read_as_missing,
            ));
        }
        let mut varying: Vec<Out> = Vec::with_capacity(long_columns.len());
        for column in &mut long_columns {
            let read = column.read(&row, &ctx);
            varying.push(note(
                &column.letter,
                read,
                &mut failures,
                &mut read_as_missing,
            ));
        }
        if failed {
            continue;
        }
        let lines: Vec<Option<(&str, &Out)>> = if spec.long.is_some() {
            points
                .iter()
                .map(String::as_str)
                .zip(varying.iter())
                .map(Some)
                .collect()
        } else {
            vec![None]
        };
        for point in lines {
            let mut fields: Vec<Field<'_>> = Vec::with_capacity(outputs.len());
            for (column, out) in columns.iter().zip(&fixed) {
                push_fields(&mut fields, column.spec, out);
            }
            if let (Some((point, out)), Some(long)) = (point, &spec.long) {
                fields.push(if long.values.iter().all(Value::is_number) {
                    Field::Plain(point)
                } else {
                    Field::Text(point)
                });
                push_fields(&mut fields, &long.value, out);
            }
            if !key_at.is_empty() {
                let mut hasher = Sha256::new();
                for at in &key_at {
                    match &fields[*at] {
                        Field::Missing => hasher.update([0_u8]),
                        Field::Plain(t) | Field::Text(t) => {
                            hasher.update([1_u8]);
                            hasher.update((t.len() as u64).to_be_bytes());
                            hasher.update(t.as_bytes());
                        }
                    }
                }
                let digest = hasher.finalize();
                let mut first = [0_u8; 16];
                first.copy_from_slice(&digest[..16]);
                let hashed = u128::from_be_bytes(first);
                if let Some(first_row) = keys.get(&hashed) {
                    receipt.repeated_key_count += 1;
                    if receipt.repeated_keys.len() < MAX_FAILURES {
                        receipt.repeated_keys.push(RepeatedKey {
                            row: row_number,
                            first_row: *first_row,
                        });
                    }
                } else if keys.len() < MAX_KEY_ROWS {
                    keys.insert(hashed, row_number);
                }
            }
            if !writer.line(&fields, false) {
                too_large = true;
                break;
            }
        }
        if too_large {
            break;
        }
    }
    // The sheet's rows as the grid listed them all arrived: a grid shorter than its own listing is not read.
    if !too_large && receipt.rows.read != spec.rows.last - spec.rows.first + 1 {
        return Err(refused("dataset_grid_unreadable"));
    }
    receipt.columns = columns
        .into_iter()
        .map(|c| c.counts)
        .chain(merge_long(long_columns, spec))
        .collect();
    receipt.failures = failures;
    receipt.failure_count = failure_count;
    receipt.read_as_missing = read_as_missing;
    if too_large {
        receipt.rows.written = 0;
        return Err(Refusal {
            code: "dataset_table_too_large",
            receipt: Some(Box::new(receipt)),
        });
    }
    receipt.rows.written = writer.rows;
    if failure_count > 0 {
        return Err(Refusal {
            code: "dataset_values_unread",
            receipt: Some(Box::new(receipt)),
        });
    }
    if receipt.repeated_key_count > 0 {
        return Err(Refusal {
            code: "dataset_key_repeated",
            receipt: Some(Box::new(receipt)),
        });
    }
    if writer.rows == 0 {
        return Err(Refusal {
            code: "dataset_no_rows",
            receipt: Some(Box::new(receipt)),
        });
    }
    let table = writer.table.into_bytes();
    let spreadsheet = writer.spreadsheet.into_bytes();
    let path = format!("results/lab_data/{}.csv", spec.table_name);
    receipt.table = Some(FileFacts {
        path: path.clone(),
        sha256: hex::encode(Sha256::digest(&table)),
        bytes: table.len() as u64,
        rows: writer.rows,
    });
    receipt.spreadsheet_copy = Some(FileFacts {
        path: format!("exports/{}.spreadsheet.csv", spec.table_name),
        sha256: hex::encode(Sha256::digest(&spreadsheet)),
        bytes: spreadsheet.len() as u64,
        rows: writer.rows,
    });
    receipt.status = "prepared";
    Ok(Prepared {
        table,
        spreadsheet,
        receipt,
    })
}

/// The counts of a long layout's columns as one column: the value.
fn merge_long(columns: Vec<Column<'_>>, spec: &Spec) -> Option<ColumnCounts> {
    let long = spec.long.as_ref()?;
    let mut merged = ColumnCounts {
        name: long.value.name.clone(),
        from: long.columns.join(","),
        kind: long.value.kind.name(),
        censored_rule: long.value.censored.map(|c| c.rule.name()),
        ..ColumnCounts::default()
    };
    for column in columns {
        let c = column.counts;
        merged.values += c.values;
        for (k, v) in c.missing {
            *merged.missing.entry(k).or_insert(0) += v;
        }
        merged.trimmed += c.trimmed;
        for (k, v) in c.recoded {
            *merged.recoded.entry(k).or_insert(0) += v;
        }
        merged.recoded_unlisted += c.recoded_unlisted;
        merged.decimal_commas += c.decimal_commas;
        merged.thousands_separators += c.thousands_separators;
        merged.percent_signs += c.percent_signs;
        merged.dates_from_text += c.dates_from_text;
        for (k, v) in c.censored {
            *merged.censored.entry(k).or_insert(0) += v;
        }
        merged.unread_as_missing += c.unread_as_missing;
    }
    Some(merged)
}

fn point_text(value: &Value) -> String {
    match value {
        Value::String(text) => text.clone(),
        other => other.to_string(),
    }
}

fn push_fields<'a>(fields: &mut Vec<Field<'a>>, spec: &ColumnSpec, out: &'a Out) {
    let text = matches!(spec.kind, ColumnType::Text | ColumnType::Category);
    let value = |v: &'a str| {
        if text {
            Field::Text(v)
        } else {
            Field::Plain(v)
        }
    };
    match out {
        Out::Missing => {
            fields.push(Field::Missing);
            if spec.censored.is_some() {
                fields.push(Field::Missing);
                fields.push(Field::Missing);
            }
        }
        Out::Value(v) => {
            fields.push(value(v));
            if spec.censored.is_some() {
                fields.push(Field::Missing);
                fields.push(Field::Missing);
            }
        }
        Out::Censored {
            direction,
            limit,
            value: v,
        } => {
            fields.push(v.as_deref().map_or(Field::Missing, Field::Plain));
            fields.push(Field::Text(direction));
            fields.push(limit.as_deref().map_or(Field::Missing, Field::Plain));
        }
    }
}

struct Context<'a> {
    spec: &'a Spec,
    missing: &'a HashSet<&'a str>,
}

type Unread = (&'static str, String);

enum Reading {
    Value(Out),
    AsMissing(Unread),
    Failed(Unread),
}

fn unread(reason: &'static str, text: &str) -> Unread {
    (
        reason,
        text.chars()
            .filter(|c| !c.is_control() && !super::live_output::is_bidi_control(*c))
            .take(MAX_FAILURE_CHARS)
            .collect(),
    )
}

impl<'a> Column<'a> {
    fn new(spec: &'a ColumnSpec, index: Option<usize>, letter: String) -> Self {
        Column {
            spec,
            index,
            letter: letter.clone(),
            counts: ColumnCounts {
                name: spec.name.clone(),
                from: letter,
                kind: spec.kind.name(),
                censored_rule: spec.censored.map(|c| c.rule.name()),
                ..ColumnCounts::default()
            },
        }
    }

    /// Read one row's cell, or say why no rule reads it. A column that reads such cells as missing counts them.
    fn read(&mut self, row: &[Cell], ctx: &Context<'_>) -> Reading {
        let cell = self.index.and_then(|i| row.get(i)).unwrap_or(&Cell::Empty);
        match self.convert(cell, ctx) {
            Ok(out) => {
                if matches!(out, Out::Value(_)) {
                    self.counts.values += 1;
                }
                Reading::Value(out)
            }
            Err(unread) if self.spec.other == Other::Missing => {
                self.counts.unread_as_missing += 1;
                Reading::AsMissing(unread)
            }
            Err(unread) => Reading::Failed(unread),
        }
    }

    fn missing(&mut self, token: &str) -> Out {
        *self.counts.missing.entry(token.to_owned()).or_insert(0) += 1;
        Out::Missing
    }

    fn convert(&mut self, cell: &Cell, ctx: &Context<'_>) -> Result<Out, Unread> {
        let spec = self.spec;
        let raw = match cell {
            Cell::Empty => return Ok(self.missing("")),
            Cell::Error(text) => return Err(unread("spreadsheet_error", text)),
            Cell::Serial(n) => return Err(unread("date_out_of_range", &n.to_string())),
            other => other.text().unwrap_or_default(),
        };
        let trimmed = raw.trim();
        if trimmed.is_empty() {
            return Ok(self.missing(""));
        }
        if ctx.missing.contains(trimmed) {
            return Ok(self.missing(trimmed));
        }
        let out = match spec.kind {
            ColumnType::Text | ColumnType::Category => {
                // R, the result digest and the table reader read `NA` as a missing value, quoted or not: a text that
                // would be written as `NA` is a decision for the person (list it as missing or recode it).
                let trims = ctx.spec.trim && trimmed != raw;
                let written = spec
                    .recode
                    .get(trimmed)
                    .map_or(if trims { trimmed } else { raw.as_str() }, String::as_str);
                if written == "NA" {
                    return Err(unread("text_reads_as_missing", &raw));
                }
                let mut text = if trims {
                    self.counts.trimmed += 1;
                    trimmed.to_owned()
                } else {
                    raw.clone()
                };
                if let Some(to) = spec.recode.get(trimmed) {
                    if *to != text {
                        let pair = format!("{trimmed} -> {to}");
                        if let Some(count) = self.counts.recoded.get_mut(&pair) {
                            *count += 1;
                        } else if self.counts.recoded.len() < MAX_RECODES_LISTED {
                            self.counts.recoded.insert(pair, 1);
                        } else {
                            self.counts.recoded_unlisted += 1;
                        }
                    }
                    text = to.clone();
                }
                Ok(Out::Value(text))
            }
            ColumnType::Number | ColumnType::Integer => self.number(cell, trimmed, ctx),
            ColumnType::Logical => {
                let upper = trimmed.to_uppercase();
                let value = match cell {
                    Cell::Bool(b) => *b,
                    _ if upper == "TRUE" => true,
                    _ if upper == "FALSE" => false,
                    _ if spec.true_values.iter().any(|t| t == trimmed) => true,
                    _ if spec.false_values.iter().any(|t| t == trimmed) => false,
                    _ => return Err(unread("not_a_logical", &raw)),
                };
                Ok(Out::Value(if value { "TRUE" } else { "FALSE" }.into()))
            }
            ColumnType::Date => match cell {
                Cell::Date(text) => Ok(Out::Value(text.clone())),
                Cell::Text(_) => {
                    let date = text_date(trimmed, ctx.spec.date_order)
                        .map_err(|reason| unread(reason, &raw))?;
                    self.counts.dates_from_text += 1;
                    Ok(Out::Value(date))
                }
                _ => Err(unread("not_a_date", &raw)),
            },
            ColumnType::Datetime => match cell {
                Cell::DateTime(text) => Ok(Out::Value(text.clone())),
                Cell::Date(text) => Ok(Out::Value(format!("{text}T00:00:00"))),
                Cell::Text(_) => {
                    let at =
                        text_datetime(trimmed).ok_or_else(|| unread("not_a_datetime", &raw))?;
                    self.counts.dates_from_text += 1;
                    Ok(Out::Value(at))
                }
                _ => Err(unread("not_a_datetime", &raw)),
            },
            ColumnType::Time => match cell {
                Cell::Time(text) => Ok(Out::Value(text.clone())),
                Cell::Text(_) => text_time(trimmed)
                    .map(Out::Value)
                    .ok_or_else(|| unread("not_a_time", &raw)),
                _ => Err(unread("not_a_time", &raw)),
            },
        };
        // Every other type reads past the spaces around a value; that change is counted like a text's trim.
        if !matches!(spec.kind, ColumnType::Text | ColumnType::Category)
            && trimmed != raw
            && out.is_ok()
        {
            self.counts.trimmed += 1;
        }
        out
    }

    fn number(&mut self, cell: &Cell, trimmed: &str, ctx: &Context<'_>) -> Result<Out, Unread> {
        let spec = self.spec;
        let integer = spec.kind == ColumnType::Integer;
        if let Cell::Number(n) = cell {
            if integer && !(n.is_i64() || n.is_u64()) {
                return Err(unread("not_an_integer", &n.to_string()));
            }
            return Ok(Out::Value(n.to_string()));
        }
        if !matches!(cell, Cell::Text(_)) {
            return Err(unread("not_a_number", trimmed));
        }
        let decimal = ctx.spec.decimal(spec);
        let thousands = ctx.spec.thousands(spec);
        if let Some(direction) = censor_token(trimmed) {
            return self.censored(direction, None, trimmed);
        }
        if let Some(found) = LIMIT.captures(trimmed) {
            let direction = if matches!(&found[1], "<" | "≤") {
                Censor::Below
            } else {
                Censor::Above
            };
            let limit = self
                .number_text(&found[2], decimal, thousands, false)
                .map_err(|reason| unread(reason, trimmed))?;
            return self.censored(direction, Some(limit), trimmed);
        }
        let text = self
            .number_text(trimmed, decimal, thousands, spec.strip_percent)
            .map_err(|reason| unread(reason, trimmed))?;
        if integer && !INTEGER.is_match(&text) {
            return Err(unread("not_an_integer", trimmed));
        }
        Ok(Out::Value(text))
    }

    /// A number's text as R reads it: the decimal mark a point, no thousands separators, no leading `+`. Nothing else in
    /// the digits changes.
    fn number_text(
        &mut self,
        text: &str,
        decimal: Decimal,
        thousands: Thousands,
        strip_percent: bool,
    ) -> Result<String, &'static str> {
        let mut t = text.trim().to_owned();
        if strip_percent && let Some(rest) = t.strip_suffix('%') {
            t = rest.trim_end().to_owned();
            self.counts.percent_signs += 1;
        }
        let grouped = match (thousands, decimal) {
            (Thousands::Comma, Decimal::Point) => GROUPED_COMMA.is_match(&t).then_some(','),
            (Thousands::Point, Decimal::Comma) => GROUPED_POINT.is_match(&t).then_some('.'),
            (Thousands::Space, Decimal::Point) => GROUPED_SPACE_POINT.is_match(&t).then_some(' '),
            (Thousands::Space, Decimal::Comma) => GROUPED_SPACE_COMMA.is_match(&t).then_some(' '),
            _ => None,
        };
        if let Some(separator) = grouped {
            t = t.replace(separator, "");
            self.counts.thousands_separators += 1;
        }
        if decimal == Decimal::Comma {
            if t.contains('.') {
                return Err("point_in_comma_decimal");
            }
            if t.contains(',') {
                t = t.replacen(',', ".", 1);
                self.counts.decimal_commas += 1;
            }
        }
        let t = match t.strip_prefix('+') {
            Some(rest) => {
                self.counts.plus_signs += 1;
                rest.to_owned()
            }
            None => t,
        };
        if NUMBER.is_match(&t) || matches!(t.as_str(), "Inf" | "-Inf") {
            Ok(t)
        } else if t.contains(',') {
            Err("comma_in_number")
        } else {
            Err("not_a_number")
        }
    }

    fn censored(
        &mut self,
        direction: Censor,
        limit: Option<String>,
        raw: &str,
    ) -> Result<Out, Unread> {
        let Some(rule) = self.spec.censored else {
            return Err(unread("censored_without_rule", raw));
        };
        let (name, default_limit) = match direction {
            Censor::Below => ("below", rule.limit),
            Censor::NotDetected => ("not_detected", rule.limit),
            Censor::Above => ("above", rule.upper_limit),
        };
        let limit = limit.or_else(|| default_limit.map(format_number));
        let value = match rule.rule {
            CensorRule::Missing => None,
            CensorRule::Limit => Some(limit.clone().ok_or_else(|| unread("limit_unknown", raw))?),
            CensorRule::HalfLimit | CensorRule::LimitOverSqrt2 => {
                let text = limit.clone().ok_or_else(|| unread("limit_unknown", raw))?;
                if direction == Censor::Above {
                    Some(text)
                } else {
                    let number: f64 = text.parse().map_err(|_| unread("limit_unknown", raw))?;
                    let divisor = if rule.rule == CensorRule::HalfLimit {
                        2.0
                    } else {
                        std::f64::consts::SQRT_2
                    };
                    Some(format_number(number / divisor))
                }
            }
        };
        // The value written is checked as the column's type, as any other value is.
        if self.spec.kind == ColumnType::Integer
            && value.as_deref().is_some_and(|v| !INTEGER.is_match(v))
        {
            return Err(unread("not_an_integer", raw));
        }
        *self
            .counts
            .censored
            .entry(match direction {
                Censor::Below => "below_limit",
                Censor::Above => "above_limit",
                Censor::NotDetected => "not_detected",
            })
            .or_insert(0) += 1;
        Ok(Out::Censored {
            direction: name,
            limit,
            value,
        })
    }
}

static NUMBER: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^-?(?:\d+\.?\d*|\.\d+)(?:[eE][-+]?\d+)?$").unwrap());
static INTEGER: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^-?\d+$").unwrap());
static GROUPED_COMMA: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^[-+]?\d{1,3}(?:,\d{3})+(?:\.\d+)?$").unwrap());
static GROUPED_POINT: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^[-+]?\d{1,3}(?:\.\d{3})+(?:,\d+)?$").unwrap());
static GROUPED_SPACE_POINT: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^[-+]?\d{1,3}(?: \d{3})+(?:\.\d+)?$").unwrap());
static GROUPED_SPACE_COMMA: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^[-+]?\d{1,3}(?: \d{3})+(?:,\d+)?$").unwrap());
static ISO_DATE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^(\d{4})-(\d{1,2})-(\d{1,2})$").unwrap());
static ISO_DATETIME: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^(\d{4})-(\d{1,2})-(\d{1,2})[T ](\d{1,2}):(\d{2})(?::(\d{2})(\.\d+)?)?(Z|[+-]\d{2}:?\d{2})?$").unwrap()
});
static TIME_TEXT: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^(\d{1,2}):(\d{2})(?::(\d{2})(\.\d+)?)?(?:\s*([AaPp])[Mm])?$").unwrap()
});

/// A computed number in its shortest text that reads back as the same double.
fn format_number(value: f64) -> String {
    format!("{value}")
}

fn valid_date(year: i32, month: u32, day: u32) -> Option<String> {
    let month = time::Month::try_from(u8::try_from(month).ok()?).ok()?;
    let date = time::Date::from_calendar_date(year, month, u8::try_from(day).ok()?).ok()?;
    Some(format!(
        "{:04}-{:02}-{:02}",
        date.year(),
        u8::from(date.month()),
        date.day()
    ))
}

fn text_date(text: &str, order: Option<DateOrder>) -> Result<String, &'static str> {
    if let Some(found) = ISO_DATE.captures(text) {
        let part = |i: usize| found[i].parse::<u32>().unwrap_or(0);
        return valid_date(part(1) as i32, part(2), part(3)).ok_or("not_a_date");
    }
    let found = DMY.captures(text).ok_or("not_a_date")?;
    if found[3].len() != 4 {
        return Err("two_digit_year");
    }
    let (a, b, year) = (
        found[1].parse::<u32>().unwrap_or(0),
        found[2].parse::<u32>().unwrap_or(0),
        found[3].parse::<i32>().unwrap_or(0),
    );
    let (day, month) = match order {
        Some(DateOrder::DayFirst) => (a, b),
        Some(DateOrder::MonthFirst) => (b, a),
        None if a == b => (a, b),
        None => return Err("date_order_unset"),
    };
    valid_date(year, month, day).ok_or("not_a_date")
}

fn text_datetime(text: &str) -> Option<String> {
    let found = ISO_DATETIME.captures(text)?;
    let part = |i: usize| {
        found
            .get(i)
            .map_or(0, |m| m.as_str().parse::<u32>().unwrap_or(0))
    };
    let date = valid_date(part(1) as i32, part(2), part(3))?;
    let (hour, minute, second) = (part(4), part(5), part(6));
    if hour > 23 || minute > 59 || second > 60 {
        return None;
    }
    let fraction = found.get(7).map_or("", |m| m.as_str());
    let zone = found.get(8).map_or("", |m| m.as_str());
    Some(format!(
        "{date}T{hour:02}:{minute:02}:{second:02}{fraction}{zone}"
    ))
}

fn text_time(text: &str) -> Option<String> {
    let found = TIME_TEXT.captures(text)?;
    let mut hour: u32 = found[1].parse().ok()?;
    let minute: u32 = found[2].parse().ok()?;
    let second: u32 = found.get(3).map_or(Some(0), |m| m.as_str().parse().ok())?;
    if let Some(half) = found.get(5) {
        if !(1..=12).contains(&hour) {
            return None;
        }
        hour %= 12;
        if half.as_str().eq_ignore_ascii_case("p") {
            hour += 12;
        }
    }
    if hour > 23 || minute > 59 || second > 59 {
        return None;
    }
    let fraction = found.get(4).map_or("", |m| m.as_str());
    Some(format!("{hour:02}:{minute:02}:{second:02}{fraction}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::broker::cell_grid::tests::grid;
    use crate::broker::dataset_profile::tests::tolerance_sheet;
    use serde_json::json;

    fn spec(value: Value) -> Spec {
        Spec::parse(&value).unwrap_or_else(|e| panic!("{e:?}"))
    }

    fn tolerance_spec() -> Value {
        json!({
            "schema": SPEC_SCHEMA,
            "table_name": "gtt_cohort3",
            "sheet": 0,
            "header_rows": [3, 4],
            "units_row": 5,
            "rows": {"first": 6, "last": 10},
            "drop_rows": [[9, 9]],
            "missing": ["NA", "-"],
            "columns": [
                {"from": "A", "name": "mouse_id", "type": "text"},
                {"from": "B", "name": "sex", "type": "category",
                    "recode": {"male": "Male", "MALE": "Male", "female": "Female"}},
                {"from": "C", "name": "diet", "type": "category"},
                {"from": "D", "name": "body_weight", "type": "number", "unit": "g", "decimal": "comma"},
                {"from": "J", "name": "insulin", "type": "number", "unit": "ng/mL",
                    "censored": {"rule": "missing", "limit": 0.2}}
            ],
            "long": {"columns": ["E", "F", "G", "H", "I"], "name": "time_min", "values": [0, 15, 30, 60, 120],
                "value": {"name": "glucose", "type": "integer", "unit": "mg/dL"}},
            "key": ["mouse_id", "time_min"]
        })
    }

    fn run(rows: Vec<Value>, value: Value) -> Result<Prepared, Refusal> {
        let bytes = grid("delimited", &[(None, rows)]);
        prepare(&bytes[..], &"b".repeat(64), &spec(value), &"c".repeat(64))
    }

    #[test]
    fn a_specification_that_does_not_parse_names_the_place() {
        let refused = |change: &dyn Fn(&mut Value)| {
            let mut value = tolerance_spec();
            change(&mut value);
            let error = Spec::parse(&value).unwrap_err();
            (error.path, error.reason)
        };
        assert_eq!(
            refused(&|v| {
                v.as_object_mut().unwrap().remove("sheet");
            }),
            ("sheet".into(), "missing_field")
        );
        // An unknown field's name is the sender's text: the place stops at its parent.
        assert_eq!(
            refused(&|v| v["surprise"] = json!(1)),
            (String::new(), "unknown_field")
        );
        assert_eq!(
            refused(&|v| v["columns"][0]["colour"] = json!("red")),
            ("columns[0]".into(), "unknown_field")
        );
        // A field the specification owns, in a place that does not take it, keeps its parent.
        assert_eq!(
            refused(&|v| v["rows"]["name"] = json!("x")),
            ("rows".into(), "unknown_field")
        );
        assert_eq!(
            refused(&|v| v["rows"]["surprise"] = json!("x")),
            ("rows".into(), "unknown_field")
        );
        assert_eq!(
            refused(&|v| v["columns"][1]["type"] = json!("float")),
            ("columns[1].type".into(), "unknown_choice")
        );
        assert_eq!(
            refused(&|v| v["rows"]["first"] = json!("six")),
            ("rows.first".into(), "wrong_type")
        );
        // A recode key is a cell's text: the place stops at the map, whatever characters the key holds.
        assert_eq!(
            refused(&|v| v["columns"][1]["recode"] = json!({"a b\u{202e}": 5})),
            ("columns[1].recode".into(), "wrong_type")
        );
        assert_eq!(
            refused(&|v| v["columns"][1]["recode"] = json!({"Male": 5})),
            ("columns[1].recode".into(), "wrong_type")
        );
        // A cell may spell a field name: the place still stops at the map.
        for name in SPEC_FIELDS {
            assert_eq!(
                refused(&|v| v["columns"][1]["recode"] = json!({*name: 5})),
                ("columns[1].recode".into(), "wrong_type"),
                "{name}"
            );
        }
        assert_eq!(Spec::parse(&json!([1])).unwrap_err().reason, "wrong_type");
    }

    #[test]
    fn the_result_digest_reads_as_missing_exactly_the_cells_written_missing() {
        // Text cells, a missing token, a blank, recodes and text labels of a long layout: the digest's own reader counts
        // a column's missing values as the receipt does, and no label or recoded text as missing.
        let rows = vec![
            json!(["id", "region", "a", "b"]),
            json!(["r1", "EU", 1, 2]),
            json!(["r2", "-", 3, null]),
            json!(["r3", "eu", null, 4]),
            json!(["r4", "", 5, 6]),
        ];
        let prepared = run(
            rows,
            json!({"schema": SPEC_SCHEMA, "table_name": "t", "sheet": 0, "header_rows": [1],
                "rows": {"first": 2, "last": 5}, "missing": ["-"],
                "columns": [{"from": "A", "name": "id", "type": "text"},
                    {"from": "B", "name": "region", "type": "category", "recode": {"eu": "EU"}}],
                "long": {"columns": ["C", "D"], "name": "visit", "values": ["first", "second"],
                    "value": {"name": "score", "type": "integer"}}}),
        )
        .unwrap();
        let read = crate::broker::digest::table(
            &mut &prepared.table[..],
            prepared.table.len() as u64,
            None,
        );
        let missing = |name: &str| {
            read["missing"]
                .get(name)
                .and_then(Value::as_u64)
                .unwrap_or(0)
        };
        // The receipt lists the three columns it read, so the comparison below runs for each.
        assert_eq!(
            prepared
                .receipt
                .columns
                .iter()
                .map(|c| c.name.as_str())
                .collect::<Vec<_>>(),
            ["id", "region", "score"]
        );
        for counts in &prepared.receipt.columns {
            let written = counts.missing.values().sum::<u64>() + counts.unread_as_missing;
            // A fixed column's cell is written once for each of the long layout's two points.
            let copies = if counts.name == "score" { 1 } else { 2 };
            assert_eq!(missing(&counts.name), written * copies, "{}", counts.name);
        }
        assert_eq!(
            (
                missing("region"),
                missing("score"),
                missing("visit"),
                missing("id")
            ),
            (4, 2, 0, 0)
        );
    }

    #[test]
    fn a_tolerance_sheet_becomes_one_row_per_mouse_and_time() {
        let prepared = run(tolerance_sheet(), tolerance_spec()).unwrap();
        let mut expected = String::from(
            "mouse_id,sex,diet,body_weight,insulin,insulin_censored,insulin_limit,time_min,glucose\n",
        );
        for (mouse, sex, diet, weight, insulin, glucose) in [
            (
                "M-0417",
                "Male",
                "HFD",
                "31.2",
                "1.2,NA,NA",
                ["142", "310", "295", "240", "160"],
            ),
            (
                "M-0418",
                "Male",
                "HFD",
                "29.8",
                "NA,below,0.2",
                ["150", "330", "NA", "251", "171"],
            ),
            (
                "M-0419",
                "Female",
                "Chow",
                "22.4",
                "NA,not_detected,0.2",
                ["120", "250", "230", "180", "130"],
            ),
            (
                "M-0421",
                "Male",
                "HFD",
                "30.1",
                "1.4,NA,NA",
                ["145", "300", "280", "230", "155"],
            ),
        ] {
            for (time, value) in ["0", "15", "30", "60", "120"].iter().zip(glucose) {
                expected.push_str(&format!(
                    "{mouse},{sex},{diet},{weight},{insulin},{time},{value}\n"
                ));
            }
        }
        assert_eq!(String::from_utf8(prepared.table.clone()).unwrap(), expected);
        let receipt = &prepared.receipt;
        assert_eq!(receipt.status, "prepared");
        assert_eq!(
            receipt.rows,
            RowCounts {
                read: 5,
                blank: 0,
                dropped: 1,
                kept: 4,
                written: 20
            }
        );
        let sex = &receipt.columns[1];
        assert_eq!(sex.trimmed, 1);
        assert_eq!(
            sex.recoded,
            BTreeMap::from([
                ("MALE -> Male".to_owned(), 1),
                ("male -> Male".to_owned(), 1)
            ])
        );
        assert_eq!(receipt.columns[3].decimal_commas, 4);
        assert_eq!(
            receipt.columns[4].censored,
            BTreeMap::from([("below_limit", 1), ("not_detected", 1)])
        );
        assert_eq!(receipt.columns[4].censored_rule, Some("missing"));
        let glucose = &receipt.columns[5];
        assert_eq!(
            (glucose.name.as_str(), glucose.from.as_str()),
            ("glucose", "E,F,G,H,I")
        );
        assert_eq!(glucose.values, 19);
        assert_eq!(glucose.missing, BTreeMap::from([("NA".to_owned(), 1)]));
        let table = receipt.table.as_ref().unwrap();
        assert_eq!(table.path, "results/lab_data/gtt_cohort3.csv");
        assert_eq!(table.sha256, hex::encode(Sha256::digest(&prepared.table)));
        assert_eq!(table.rows, 20);
        // The workspace reads the table by rules version 1 with the types the preparation named.
        let parsed =
            crate::broker::tables::parse(&prepared.table, b',', table.sha256.clone()).unwrap();
        assert_eq!(parsed.rows(), 20);
        let (summary, _) = parsed.summary();
        let types: Vec<String> = summary
            .iter()
            .map(|c| {
                serde_json::to_value(c).unwrap()["type"]
                    .as_str()
                    .unwrap()
                    .to_owned()
            })
            .collect();
        assert_eq!(
            types,
            [
                "string", "string", "string", "number", "number", "string", "number", "integer",
                "integer"
            ]
        );
    }

    #[test]
    fn the_same_grid_and_preparation_give_the_same_bytes() {
        let first = run(tolerance_sheet(), tolerance_spec()).unwrap();
        for _ in 0..3 {
            let again = run(tolerance_sheet(), tolerance_spec()).unwrap();
            assert_eq!(again.table, first.table);
            assert_eq!(again.spreadsheet, first.spreadsheet);
            assert_eq!(again.receipt, first.receipt);
        }
    }

    #[test]
    fn a_repeated_mouse_stops_the_preparation_with_both_rows() {
        let mut value = tolerance_spec();
        value["drop_rows"] = json!([]);
        let refusal = run(tolerance_sheet(), value).unwrap_err();
        assert_eq!(refusal.code, "dataset_key_repeated");
        let receipt = refusal.receipt.unwrap();
        assert_eq!(receipt.repeated_key_count, 5);
        assert_eq!(
            receipt.repeated_keys[0],
            RepeatedKey {
                row: 9,
                first_row: 8
            }
        );
        assert_eq!(receipt.status, "refused");
        assert!(receipt.table.is_none());
    }

    #[test]
    fn a_cell_no_rule_reads_stops_the_preparation_and_is_named() {
        let mut value = tolerance_spec();
        value["columns"][4] = json!({"from": "J", "name": "insulin", "type": "number"});
        let refusal = run(tolerance_sheet(), value.clone()).unwrap_err();
        assert_eq!(refusal.code, "dataset_values_unread");
        let receipt = refusal.receipt.unwrap();
        assert_eq!(receipt.failure_count, 2);
        assert_eq!(
            receipt.failures,
            [
                Failure {
                    row: 7,
                    column: "J".into(),
                    reason: "censored_without_rule",
                    text: "<0.2".into()
                },
                Failure {
                    row: 8,
                    column: "J".into(),
                    reason: "censored_without_rule",
                    text: "ND".into()
                },
            ]
        );
        // The column may read such cells as missing instead: each is counted and listed.
        value["columns"][4]["other"] = json!("missing");
        let prepared = run(tolerance_sheet(), value).unwrap();
        assert_eq!(prepared.receipt.columns[4].unread_as_missing, 2);
        assert_eq!(prepared.receipt.read_as_missing.len(), 2);
        assert_eq!(prepared.receipt.read_as_missing[1].text, "ND");
    }

    #[test]
    fn censoring_rules_choose_the_value_and_keep_the_limit() {
        let rows: Vec<Value> = vec![
            json!(["id", "x"]),
            json!(["a", "<0.5"]),
            json!(["b", "ND"]),
            json!(["c", ">2000"]),
            json!(["d", "<LOD"]),
            json!(["e", "1.5"]),
        ];
        let with = |rule: &str, limit: Value| {
            json!({"schema": SPEC_SCHEMA, "table_name": "t", "sheet": 0, "rows": {"first": 2, "last": 6},
                "missing": [], "columns": [{"from": "A", "name": "id", "type": "text"},
                {"from": "B", "name": "x", "type": "number", "censored": {"rule": rule, "limit": limit}}]})
        };
        let table = |rule: &str, limit: Value| {
            String::from_utf8(run(rows.clone(), with(rule, limit)).unwrap().table).unwrap()
        };
        assert_eq!(
            table("half_limit", json!(0.1)),
            "id,x,x_censored,x_limit\na,0.25,below,0.5\nb,0.05,not_detected,0.1\nc,2000,above,2000\nd,0.05,below,0.1\ne,1.5,NA,NA\n"
        );
        assert_eq!(
            table("limit_over_sqrt2", json!(0.1)).lines().nth(1),
            Some("a,0.35355339059327373,below,0.5")
        );
        assert_eq!(
            table("limit", json!(0.1)).lines().nth(2),
            Some("b,0.1,not_detected,0.1")
        );
        // Without a stated limit a bare `<LOD` has none to stand in.
        let refusal = run(rows.clone(), with("limit", Value::Null)).unwrap_err();
        let receipt = refusal.receipt.unwrap();
        assert_eq!(
            receipt
                .failures
                .iter()
                .map(|f| (f.row, f.reason))
                .collect::<Vec<_>>(),
            [(3, "limit_unknown"), (5, "limit_unknown")]
        );
        assert_eq!(
            table("missing", Value::Null).lines().nth(4),
            Some("d,NA,below,NA")
        );
    }

    #[test]
    fn an_integer_column_keeps_integers_when_values_are_censored() {
        let rows: Vec<Value> = vec![
            json!(["id", "n"]),
            json!(["a", "<5"]),
            json!(["b", "<0.5"]),
            json!(["c", "7"]),
        ];
        let with = |censored: Value| {
            json!({"schema": SPEC_SCHEMA, "table_name": "t", "sheet": 0, "rows": {"first": 2, "last": 4},
                "missing": [], "columns": [{"from": "A", "name": "id", "type": "text"},
                {"from": "B", "name": "n", "type": "integer", "censored": censored}]})
        };
        // A stated limit an integer column could not hold is refused as written.
        for (censored, place) in [
            (
                json!({"rule": "limit", "limit": 0.5}),
                "columns[1].censored.limit",
            ),
            (
                json!({"rule": "limit", "upper_limit": 2.5}),
                "columns[1].censored.upper_limit",
            ),
        ] {
            assert_eq!(
                Spec::parse(&with(censored)).unwrap_err(),
                invalid(place, "not_an_integer")
            );
        }
        // A limit read from the cell is checked as the column's type: `<5` stands as 5, `<0.5` is not read.
        let refusal = run(rows.clone(), with(json!({"rule": "limit"}))).unwrap_err();
        assert_eq!(refusal.code, "dataset_values_unread");
        let receipt = refusal.receipt.unwrap();
        assert_eq!(
            receipt
                .failures
                .iter()
                .map(|f| (f.row, f.reason))
                .collect::<Vec<_>>(),
            [(3, "not_an_integer")]
        );
        let kept = run(rows[..2].iter().chain(&rows[3..]).cloned().collect(), {
            let mut v = with(json!({"rule": "limit"}));
            v["rows"]["last"] = json!(3);
            v
        })
        .unwrap();
        assert_eq!(
            String::from_utf8(kept.table).unwrap(),
            "id,n,n_censored,n_limit\na,5,below,5\nc,7,NA,NA\n"
        );
    }

    #[test]
    fn a_text_that_reads_as_missing_is_not_written_as_text() {
        let rows: Vec<Value> = vec![
            json!(["id", "region"]),
            json!(["a", "NA"]),
            json!(["b", "EU"]),
        ];
        let with = |missing: Value, recode: Value| {
            json!({"schema": SPEC_SCHEMA, "table_name": "t", "sheet": 0, "rows": {"first": 2, "last": 3},
                "missing": missing, "columns": [{"from": "A", "name": "id", "type": "text"},
                {"from": "B", "name": "region", "type": "category", "recode": recode}]})
        };
        // R, the result digest and the table reader all read `NA` as a missing value, quoted or not, so a text `NA`
        // the specification keeps would change its meaning downstream: the person decides.
        let refusal = run(rows.clone(), with(json!([]), json!({}))).unwrap_err();
        assert_eq!(refusal.code, "dataset_values_unread");
        let receipt = refusal.receipt.unwrap();
        assert_eq!(
            receipt
                .failures
                .iter()
                .map(|f| (f.row, f.reason))
                .collect::<Vec<_>>(),
            [(2, "text_reads_as_missing")]
        );
        // A recode to `NA` is refused with the specification, before any cell is read.
        assert_eq!(
            Spec::parse(&with(json!([]), json!({"EU": "NA"}))).unwrap_err(),
            invalid("columns[1].recode", "value_reads_as_missing")
        );
        // Listed as missing, or recoded to another text, it is followed.
        let table = |missing: Value, recode: Value| {
            String::from_utf8(run(rows.clone(), with(missing, recode)).unwrap().table).unwrap()
        };
        assert_eq!(table(json!(["NA"]), json!({})), "id,region\na,NA\nb,EU\n");
        assert_eq!(
            table(json!([]), json!({"NA": "North America"})),
            "id,region\na,North America\nb,EU\n"
        );
    }

    #[test]
    fn numbers_keep_their_digits_under_the_named_rules() {
        let mut column = Column::new(
            Box::leak(Box::new(ColumnSpec {
                from: "A".into(),
                name: "x".into(),
                kind: ColumnType::Number,
                unit: None,
                decimal: None,
                thousands: None,
                recode: BTreeMap::new(),
                censored: None,
                strip_percent: true,
                true_values: vec![],
                false_values: vec![],
                other: Other::Refuse,
            })),
            Some(0),
            "A".into(),
        );
        for (text, decimal, thousands, expected) in [
            ("31,2", Decimal::Comma, Thousands::None, Ok("31.2")),
            (
                "31.2",
                Decimal::Comma,
                Thousands::None,
                Err("point_in_comma_decimal"),
            ),
            ("1.234,5", Decimal::Comma, Thousands::Point, Ok("1234.5")),
            ("1,234.5", Decimal::Point, Thousands::Comma, Ok("1234.5")),
            (
                "1,234.5",
                Decimal::Point,
                Thousands::None,
                Err("comma_in_number"),
            ),
            ("1 234,5", Decimal::Comma, Thousands::Space, Ok("1234.5")),
            ("+3", Decimal::Point, Thousands::None, Ok("3")),
            ("0.000120", Decimal::Point, Thousands::None, Ok("0.000120")),
            ("12.5 %", Decimal::Point, Thousands::None, Ok("12.5")),
            ("1e-3", Decimal::Point, Thousands::None, Ok("1e-3")),
            ("dead", Decimal::Point, Thousands::None, Err("not_a_number")),
        ] {
            assert_eq!(
                column
                    .number_text(text, decimal, thousands, true)
                    .as_deref(),
                expected.map(str::to_owned).as_deref(),
                "{text}"
            );
        }
    }

    #[test]
    fn dates_need_their_order_and_four_digit_years() {
        assert_eq!(text_date("13/03/2025", None), Err("date_order_unset"));
        assert_eq!(
            text_date("13/03/2025", Some(DateOrder::DayFirst)).as_deref(),
            Ok("2025-03-13")
        );
        assert_eq!(
            text_date("03/13/2025", Some(DateOrder::DayFirst)),
            Err("not_a_date")
        );
        assert_eq!(
            text_date("03/13/2025", Some(DateOrder::MonthFirst)).as_deref(),
            Ok("2025-03-13")
        );
        assert_eq!(text_date("04/04/2025", None).as_deref(), Ok("2025-04-04"));
        assert_eq!(
            text_date("13/03/25", Some(DateOrder::DayFirst)),
            Err("two_digit_year")
        );
        assert_eq!(text_date("2025-3-4", None).as_deref(), Ok("2025-03-04"));
        assert_eq!(text_date("2025-02-30", None), Err("not_a_date"));
        assert_eq!(
            text_datetime("2025-03-04 7:05").as_deref(),
            Some("2025-03-04T07:05:00")
        );
        assert_eq!(text_time("7:05 PM").as_deref(), Some("19:05:00"));
        assert_eq!(text_time("12:00 AM").as_deref(), Some("00:00:00"));
        assert_eq!(text_time("25:00"), None);
    }

    #[test]
    fn the_spreadsheet_copy_shows_formulas_as_text() {
        let rows = vec![
            json!(["note", "x"]),
            json!(["=HYPERLINK(\"http://example.invalid\")", "-3"]),
            json!(["@SUM(A1)", "2"]),
            json!(["-dead", "4"]),
            json!(["plain, with a comma", "5"]),
        ];
        let value = json!({"schema": SPEC_SCHEMA, "table_name": "notes", "sheet": 0, "rows": {"first": 2, "last": 5},
            "missing": [], "columns": [{"from": "A", "name": "note", "type": "text"},
            {"from": "B", "name": "x", "type": "integer"}]});
        let prepared = run(rows, value).unwrap();
        assert_eq!(
            String::from_utf8(prepared.table).unwrap(),
            "note,x\n\"=HYPERLINK(\"\"http://example.invalid\"\")\",-3\n@SUM(A1),2\n-dead,4\n\"plain, with a comma\",5\n"
        );
        assert_eq!(
            String::from_utf8(prepared.spreadsheet).unwrap(),
            "\u{feff}note,x\n\"'=HYPERLINK(\"\"http://example.invalid\"\")\",-3\n\"'@SUM(A1)\",2\n\"'-dead\",4\n\"plain, with a comma\",5\n"
        );
    }

    #[test]
    fn a_preparation_that_cannot_be_followed_is_refused_where_it_fails() {
        let base = tolerance_spec();
        let broken = |path: &str, value: Value| {
            let mut spec = base.clone();
            let mut target = &mut spec;
            let parts: Vec<&str> = path.split('/').collect();
            for part in &parts[..parts.len() - 1] {
                target = match part.parse::<usize>() {
                    Ok(i) => &mut target[i],
                    Err(_) => &mut target[*part],
                };
            }
            let last = parts[parts.len() - 1];
            match last.parse::<usize>() {
                Ok(i) => target[i] = value,
                Err(_) => target[last] = value,
            }
            Spec::parse(&spec).unwrap_err()
        };
        for (path, value, expected) in [
            (
                "schema",
                json!("enrichedlab.dataset-preparation.v2"),
                ("schema", "unknown_schema"),
            ),
            (
                "table_name",
                json!("GTT table"),
                ("table_name", "not_a_name"),
            ),
            (
                "header_rows",
                json!([6]),
                ("header_rows", "rows_out_of_order"),
            ),
            (
                "drop_rows",
                json!([[5, 5]]),
                ("drop_rows[0]", "rows_out_of_order"),
            ),
            (
                "columns/0/from",
                json!("XFE"),
                ("columns[0].from", "not_a_column"),
            ),
            (
                "columns/1/from",
                json!("A"),
                ("columns[1].from", "column_repeated"),
            ),
            (
                "columns/2/name",
                json!("insulin_censored"),
                ("name:insulin_censored", "name_repeated"),
            ),
            (
                "columns/3/recode",
                json!({"a": "b"}),
                ("columns[3].recode", "not_for_this_type"),
            ),
            (
                "columns/0/censored",
                json!({"rule": "missing"}),
                ("columns[0].censored", "not_for_this_type"),
            ),
            (
                "long/values",
                json!([0, 15, 30, 60]),
                ("long.values", "values_do_not_match_columns"),
            ),
            (
                "long/values",
                json!([0, 15, 30, 60, "x"]),
                ("long.values", "values_mixed"),
            ),
            (
                "long/values",
                json!(["t0", "t15", "NA", "t60", "t120"]),
                ("long.values", "value_reads_as_missing"),
            ),
            (
                "long/values",
                json!(["t0", "t15", " ", "t60", "t120"]),
                ("long.values", "value_reads_as_missing"),
            ),
            (
                "columns/1/recode",
                json!({"male": "NA"}),
                ("columns[1].recode", "value_reads_as_missing"),
            ),
            (
                "columns/1/recode",
                json!({"male": ""}),
                ("columns[1].recode", "value_reads_as_missing"),
            ),
            (
                "columns/1/recode",
                json!({"male": "  "}),
                ("columns[1].recode", "value_reads_as_missing"),
            ),
            (
                "long/value/censored",
                json!({"rule": "half_limit"}),
                ("long.value.censored.rule", "not_for_this_type"),
            ),
            (
                "long/columns/0",
                json!("D"),
                ("long.columns[0]", "column_repeated"),
            ),
            ("key/1", json!("time"), ("key[1]", "not_a_column")),
            ("missing", json!([" NA"]), ("missing[0]", "not_a_token")),
        ] {
            assert_eq!(
                broken(path, value),
                invalid(expected.0, expected.1),
                "{path}"
            );
        }
        assert_eq!(
            Spec::parse(&json!({"schema": SPEC_SCHEMA, "extra": true})).unwrap_err(),
            invalid("", "unknown_field")
        );
    }

    #[test]
    fn every_filled_cell_of_the_sheet_is_accounted_for() {
        let cells = |value: Value| run(tolerance_sheet(), value).unwrap().receipt.cells;
        // 70 filled cells: 21 in the title, header, units and summary rows, 9 in dropped row 9, 40 read.
        assert_eq!(
            cells(tolerance_spec()),
            CellCounts {
                filled: 70,
                outside_rows: 21,
                in_dropped_rows: 9,
                in_unread_columns: 0,
                read: 40
            }
        );
        // A column the specification leaves out keeps its cells in the account.
        let mut value = tolerance_spec();
        value["columns"].as_array_mut().unwrap().remove(2);
        assert_eq!(
            cells(value),
            CellCounts {
                filled: 70,
                outside_rows: 21,
                in_dropped_rows: 9,
                in_unread_columns: 4,
                read: 36
            }
        );
        // Spaces around a number and its `+` are changes too, and counted.
        let rows: Vec<Value> = vec![
            json!(["id", "x"]),
            json!(["a", "+5"]),
            json!(["b", " 7 "]),
            json!(["c", "8"]),
        ];
        let value = json!({"schema": SPEC_SCHEMA, "table_name": "t", "sheet": 0, "rows": {"first": 2, "last": 4},
            "missing": [], "columns": [{"from": "A", "name": "id", "type": "text"},
            {"from": "B", "name": "x", "type": "number"}]});
        let prepared = run(rows, value).unwrap();
        let x = &prepared.receipt.columns[1];
        assert_eq!((x.plus_signs, x.trimmed, x.values), (1, 1, 3));
        assert_eq!(
            String::from_utf8(prepared.table).unwrap(),
            "id,x\na,5\nb,7\nc,8\n"
        );
    }

    #[test]
    fn a_grid_shorter_than_its_listing_is_not_read() {
        use flate2::{Compression, read::GzDecoder, write::GzEncoder};
        use std::io::Write;
        let full = grid("delimited", &[(None, tolerance_sheet())]);
        let mut text = String::new();
        GzDecoder::new(&full[..]).read_to_string(&mut text).unwrap();
        // The listing says 13 rows; the stream stops after row 8, inside the specification's rows 6 to 10, or after
        // row 10, past them: either way the sheet is not whole, so its cell account would be short.
        for rows in [8, 10] {
            let kept: Vec<&str> = text.lines().take(2 + rows).collect();
            let mut gz = GzEncoder::new(Vec::new(), Compression::default());
            gz.write_all((kept.join("\n") + "\n").as_bytes()).unwrap();
            let short = gz.finish().unwrap();
            let refusal = prepare(
                &short[..],
                &"b".repeat(64),
                &spec(tolerance_spec()),
                &"c".repeat(64),
            )
            .unwrap_err();
            assert_eq!(refusal.code, "dataset_grid_unreadable", "{rows}");
        }
    }

    #[test]
    fn a_preparation_outside_the_sheet_is_refused() {
        let mut value = tolerance_spec();
        value["columns"][0]["from"] = json!("K");
        assert_eq!(
            run(tolerance_sheet(), value).unwrap_err().code,
            "dataset_spec_outside_sheet"
        );
        // The last row too: a range past the sheet's end is refused, not read until the rows run out.
        let mut value = tolerance_spec();
        value["rows"]["last"] = json!(14);
        assert_eq!(
            run(tolerance_sheet(), value).unwrap_err().code,
            "dataset_spec_outside_sheet"
        );
        let mut value = tolerance_spec();
        value["rows"]["last"] = json!(100);
        value["drop_rows"] = json!([[50, 60]]);
        assert_eq!(
            run(tolerance_sheet(), value).unwrap_err().code,
            "dataset_spec_outside_sheet"
        );
        let mut value = tolerance_spec();
        value["sheet"] = json!(1);
        assert_eq!(
            run(tolerance_sheet(), value).unwrap_err().code,
            "dataset_sheet_missing"
        );
        assert_eq!(
            prepare(&b"no grid"[..], "", &spec(tolerance_spec()), "")
                .unwrap_err()
                .code,
            "dataset_grid_unreadable"
        );
    }

    #[test]
    fn workbook_cells_keep_their_types() {
        let rows = vec![
            json!(["id", "when", "weight", "fasted", "at"]),
            json!([417, {"date": "2025-03-04"}, 21.5, true, {"time": "10:30:00"}]),
            json!([418, "2025-03-05", 22, "false", "7:05 PM"]),
            json!([419, {"datetime": "2025-03-06T00:00:00"}, {"error": "#DIV/0!"}, "yes", {"time": "08:00:00"}]),
        ];
        let value = json!({"schema": SPEC_SCHEMA, "table_name": "w", "sheet": 0, "rows": {"first": 2, "last": 4},
            "missing": [], "columns": [{"from": "A", "name": "id", "type": "text"},
            {"from": "B", "name": "when", "type": "date"},
            {"from": "C", "name": "weight", "type": "number", "other": "missing"},
            {"from": "D", "name": "fasted", "type": "logical", "true_values": ["yes"]},
            {"from": "E", "name": "at", "type": "time"}]});
        let bytes = grid("xlsx", &[(Some("S"), rows)]);
        let refusal = prepare(&bytes[..], "", &spec(value.clone()), "").unwrap_err();
        assert_eq!(refusal.code, "dataset_values_unread");
        assert_eq!(refusal.receipt.unwrap().failures[0].reason, "not_a_date");
        let mut value = value;
        value["rows"]["last"] = json!(3);
        let prepared = prepare(&bytes[..], "", &spec(value), "").unwrap();
        assert_eq!(
            String::from_utf8(prepared.table).unwrap(),
            "id,when,weight,fasted,at\n417,2025-03-04,21.5,TRUE,10:30:00\n418,2025-03-05,22,FALSE,19:05:00\n"
        );
    }
}
