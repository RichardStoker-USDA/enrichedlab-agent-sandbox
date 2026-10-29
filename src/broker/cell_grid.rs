//! The upload guard's rebuilt copy of a lab dataset, `enrichedlab.cell-grid.v1`, read as a stream. The format is
//! defined in the module comment of the guard's `src/dataset.rs`: gzip-compressed JSON lines, the source and its sheets
//! first, then each sheet as a line `{"sheet": n}` followed by its rows in order, each row where a spreadsheet program
//! shows it. The reader holds one row at a time and refuses a line or a total past its bounds, so a large grid costs
//! the room of its widest row.
use std::io::{BufRead, BufReader, Read};

use flate2::read::GzDecoder;
use serde::Deserialize;
use serde_json::{Number, Value};

pub const SCHEMA: &str = "enrichedlab.cell-grid.v1";
pub const MEDIA_TYPE: &str = "application/vnd.enrichedlab.cell-grid+gzip";

/// One cell as the grid holds it. A delimited text's cells are all `Text`, an empty field `Text("")`.
#[derive(Clone, Debug, PartialEq)]
pub enum Cell {
    Empty,
    Text(String),
    Number(Number),
    Bool(bool),
    Date(String),
    DateTime(String),
    Time(String),
    Seconds(Number),
    Duration(String),
    Error(String),
    Serial(Number),
}

impl Cell {
    /// The cell's text as a spreadsheet would show it plainly; `None` for an empty cell.
    pub fn text(&self) -> Option<String> {
        match self {
            Cell::Empty => None,
            Cell::Text(text) => Some(text.clone()),
            Cell::Number(n) | Cell::Seconds(n) | Cell::Serial(n) => Some(n.to_string()),
            Cell::Bool(true) => Some("TRUE".into()),
            Cell::Bool(false) => Some("FALSE".into()),
            Cell::Date(text)
            | Cell::DateTime(text)
            | Cell::Time(text)
            | Cell::Duration(text)
            | Cell::Error(text) => Some(text.clone()),
        }
    }

    /// Empty, or text that is empty or only white space.
    pub fn is_blank(&self) -> bool {
        match self {
            Cell::Empty => true,
            Cell::Text(text) => text.trim().is_empty(),
            _ => false,
        }
    }

    fn of(value: Value) -> Option<Cell> {
        Some(match value {
            Value::Null => Cell::Empty,
            Value::String(text) => Cell::Text(text),
            Value::Number(n) => Cell::Number(n),
            Value::Bool(b) => Cell::Bool(b),
            Value::Object(map) if map.len() == 1 => {
                let (tag, value) = map.into_iter().next()?;
                match (tag.as_str(), value) {
                    ("date", Value::String(text)) => Cell::Date(text),
                    ("datetime", Value::String(text)) => Cell::DateTime(text),
                    ("time", Value::String(text)) => Cell::Time(text),
                    ("duration", Value::String(text)) => Cell::Duration(text),
                    ("error", Value::String(text)) => Cell::Error(text),
                    ("seconds", Value::Number(n)) => Cell::Seconds(n),
                    ("serial", Value::Number(n)) => Cell::Serial(n),
                    _ => return None,
                }
            }
            _ => return None,
        })
    }
}

/// A sheet as the grid's first line describes it. A delimited text has one sheet without a name.
#[derive(Clone, Debug, Deserialize, PartialEq)]
pub struct Sheet {
    pub name: Option<String>,
    pub rows: u64,
    pub columns: u64,
    pub hidden: bool,
    pub formulas: u64,
    pub formulas_without_values: u64,
    pub merged_count: u64,
    /// Zero-based inclusive `[first_row, first_column, last_row, last_column]`.
    pub merged: Vec<[u64; 4]>,
}

/// The grid's source: the original file's type, digest and size, and how it was read.
#[derive(Clone, Debug, Deserialize, PartialEq)]
pub struct Source {
    #[serde(rename = "type")]
    pub kind: String,
    pub sha256: String,
    pub bytes: u64,
    pub encoding: Option<String>,
    pub delimiter: Option<String>,
    pub sheets: Vec<Sheet>,
}

#[derive(Deserialize)]
struct First {
    schema: String,
    source: Source,
}

#[derive(Debug, PartialEq, Eq)]
pub enum GridError {
    /// Not a grid in this format.
    Unreadable,
    /// A line or the whole grid is past the reader's bounds.
    TooLarge,
}

/// What the next line holds.
#[derive(Debug, PartialEq)]
pub enum Line {
    /// The start of the sheet with this index.
    Sheet(usize),
    /// The sheet's next row; the k-th row line of a sheet is its row k.
    Row(Vec<Cell>),
}

pub struct GridReader<R: Read> {
    lines: BufReader<GzDecoder<R>>,
    buffer: Vec<u8>,
    max_line: usize,
    left: u64,
    sheet: Option<usize>,
    /// Row lines read of the current sheet.
    rows: u64,
    /// The most cells in one row of the current sheet.
    width: u64,
    pub source: Source,
}

impl<R: Read> GridReader<R> {
    /// Opens a grid and reads its first line. A line may hold at most `max_line` bytes and all lines together at most
    /// `max_total` once decompressed.
    pub fn open(input: R, max_line: usize, max_total: u64) -> Result<Self, GridError> {
        let mut reader = GridReader {
            lines: BufReader::new(GzDecoder::new(input)),
            buffer: Vec::new(),
            max_line,
            left: max_total,
            sheet: None,
            rows: 0,
            width: 0,
            source: Source {
                kind: String::new(),
                sha256: String::new(),
                bytes: 0,
                encoding: None,
                delimiter: None,
                sheets: Vec::new(),
            },
        };
        if !reader.read_line()? {
            return Err(GridError::Unreadable);
        }
        let first: First =
            serde_json::from_slice(&reader.buffer).map_err(|_| GridError::Unreadable)?;
        if first.schema != SCHEMA
            || first.source.sha256.len() != 64
            || !first.source.sha256.bytes().all(|b| b.is_ascii_hexdigit())
        {
            return Err(GridError::Unreadable);
        }
        reader.source = first.source;
        Ok(reader)
    }

    /// One line into the buffer, without its line feed; false at the end.
    fn read_line(&mut self) -> Result<bool, GridError> {
        self.buffer.clear();
        loop {
            let available = self.lines.fill_buf().map_err(|_| GridError::Unreadable)?;
            if available.is_empty() {
                return Ok(!self.buffer.is_empty());
            }
            let (taken, ended) = match available.iter().position(|&b| b == b'\n') {
                Some(at) => (at + 1, true),
                None => (available.len(), false),
            };
            let text = &available[..if ended { taken - 1 } else { taken }];
            if self.buffer.len() + text.len() > self.max_line || taken as u64 > self.left {
                return Err(GridError::TooLarge);
            }
            self.buffer.extend_from_slice(text);
            self.left -= taken as u64;
            self.lines.consume(taken);
            if ended {
                return Ok(true);
            }
        }
    }

    /// Whether the current sheet ended with the rows and columns its description lists: the guard lists the rows it wrote
    /// and the widest of them. A grid that ends or starts the next sheet short of its rows, or whose rows all lost a
    /// column, was cut, so no part of it is read as the whole sheet.
    fn sheet_complete(&self) -> bool {
        self.sheet.is_none_or(|s| {
            self.source
                .sheets
                .get(s)
                .is_some_and(|d| d.rows == self.rows && d.columns == self.width)
        })
    }

    /// The next sheet start or row; none at the end of the grid. Sheets come in order, each once, and every row after
    /// its sheet's start; each sheet holds the rows its description lists, none wider than its columns, and the grid
    /// holds every sheet it lists.
    pub fn next_line(&mut self) -> Result<Option<Line>, GridError> {
        if !self.read_line()? {
            let last = self.sheet.map_or(0, |s| s + 1);
            if !self.sheet_complete() || last != self.source.sheets.len() {
                return Err(GridError::Unreadable);
            }
            return Ok(None);
        }
        let value: Value =
            serde_json::from_slice(&self.buffer).map_err(|_| GridError::Unreadable)?;
        match value {
            Value::Object(map) => {
                let index = map
                    .get("sheet")
                    .and_then(Value::as_u64)
                    .filter(|_| map.len() == 1)
                    .ok_or(GridError::Unreadable)? as usize;
                let expected = self.sheet.map_or(0, |s| s + 1);
                if index != expected || index >= self.source.sheets.len() || !self.sheet_complete()
                {
                    return Err(GridError::Unreadable);
                }
                self.sheet = Some(index);
                self.rows = 0;
                self.width = 0;
                Ok(Some(Line::Sheet(index)))
            }
            Value::Array(cells) if self.sheet.is_some() => {
                let declared = &self.source.sheets[self.sheet.unwrap_or_default()];
                self.rows += 1;
                self.width = self.width.max(cells.len() as u64);
                if self.rows > declared.rows || self.width > declared.columns {
                    return Err(GridError::Unreadable);
                }
                cells
                    .into_iter()
                    .map(Cell::of)
                    .collect::<Option<Vec<_>>>()
                    .map(|row| Some(Line::Row(row)))
                    .ok_or(GridError::Unreadable)
            }
            _ => Err(GridError::Unreadable),
        }
    }
}

/// Column letters as a spreadsheet names them: 0 is `A`, 25 `Z`, 26 `AA`.
pub fn column_letters(mut index: usize) -> String {
    let mut letters = Vec::new();
    loop {
        letters.push(b'A' + (index % 26) as u8);
        if index < 26 {
            break;
        }
        index = index / 26 - 1;
    }
    letters.reverse();
    String::from_utf8(letters).unwrap_or_default()
}

#[cfg(test)]
pub mod tests {
    use super::*;
    use flate2::{Compression, write::GzEncoder};
    use serde_json::json;
    use std::io::Write;

    /// A grid with the given sheets: each a name and its rows of JSON cells.
    pub fn grid(kind: &str, sheets: &[(Option<&str>, Vec<Value>)]) -> Vec<u8> {
        let described: Vec<Value> = sheets
            .iter()
            .map(|(name, rows)| {
                let columns = rows.iter().map(|r| r.as_array().map_or(0, Vec::len)).max().unwrap_or(0);
                json!({"name": name, "rows": rows.len(), "columns": columns, "hidden": false, "formulas": 0,
                    "formulas_without_values": 0, "merged_count": 0, "merged": []})
            })
            .collect();
        let mut text = serde_json::to_string(&json!({"schema": SCHEMA, "source": {"type": kind,
            "sha256": "a".repeat(64), "bytes": 10, "encoding": null, "delimiter": null, "sheets": described}}))
        .unwrap();
        text.push('\n');
        for (index, (_, rows)) in sheets.iter().enumerate() {
            text.push_str(&serde_json::to_string(&json!({"sheet": index})).unwrap());
            text.push('\n');
            for row in rows {
                text.push_str(&serde_json::to_string(row).unwrap());
                text.push('\n');
            }
        }
        let mut gz = GzEncoder::new(Vec::new(), Compression::default());
        gz.write_all(text.as_bytes()).unwrap();
        gz.finish().unwrap()
    }

    fn lines(bytes: &[u8]) -> Result<Vec<Line>, GridError> {
        let mut reader = GridReader::open(bytes, 1 << 20, 1 << 30)?;
        let mut out = Vec::new();
        while let Some(line) = reader.next_line()? {
            out.push(line);
        }
        Ok(out)
    }

    #[test]
    fn every_cell_kind_reads_back() {
        let bytes = grid(
            "xlsx",
            &[(
                Some("Weights"),
                vec![
                    json!(["id", "when", "wt"]),
                    json!(["m1", {"date": "2025-03-04"}, 21.5, true, null, {"error": "#DIV/0!"}]),
                    json!([]),
                    json!([{"datetime": "2025-03-04T10:30:00"}, {"time": "10:30:00"}, {"seconds": 5400},
                        {"duration": "PT1H"}, {"serial": -3}, 7]),
                ],
            )],
        );
        let read = lines(&bytes).unwrap();
        assert_eq!(read[0], Line::Sheet(0));
        assert_eq!(
            read[2],
            Line::Row(vec![
                Cell::Text("m1".into()),
                Cell::Date("2025-03-04".into()),
                Cell::Number(Number::from_f64(21.5).unwrap()),
                Cell::Bool(true),
                Cell::Empty,
                Cell::Error("#DIV/0!".into()),
            ])
        );
        assert_eq!(read[3], Line::Row(vec![]));
        assert_eq!(read.len(), 5);
    }

    #[test]
    fn what_is_not_this_format_is_unreadable() {
        let good = grid("delimited", &[(None, vec![json!(["a"])])]);
        assert!(lines(&good).is_ok());
        let mut text = String::new();
        flate2::read::GzDecoder::new(&good[..])
            .read_to_string(&mut text)
            .unwrap();
        let regz = |text: &str| {
            let mut gz = GzEncoder::new(Vec::new(), Compression::default());
            gz.write_all(text.as_bytes()).unwrap();
            gz.finish().unwrap()
        };
        let first = text.lines().next().unwrap();
        for broken in [
            text.replace(SCHEMA, "enrichedlab.cell-grid.v2"),
            format!("{first}\n[\"a row before its sheet\"]\n"),
            format!("{first}\n{{\"sheet\": 1}}\n"),
            format!("{first}\n{{\"sheet\": 0}}\n[{{\"colour\": \"red\"}}]\n"),
            format!("{first}\n{{\"sheet\": 0}}\n[\"a\"\n"),
        ] {
            assert_eq!(
                lines(&regz(&broken)).unwrap_err(),
                GridError::Unreadable,
                "{broken}"
            );
        }
        assert_eq!(lines(b"not gzip").unwrap_err(), GridError::Unreadable);
    }

    #[test]
    fn each_sheet_holds_what_its_description_lists() {
        let good = grid(
            "xlsx",
            &[
                (
                    Some("a"),
                    vec![json!(["x", 1]), json!(["y", 2]), json!(["z"])],
                ),
                (Some("b"), vec![json!([1])]),
            ],
        );
        assert_eq!(lines(&good).unwrap().len(), 6);
        let mut text = String::new();
        flate2::read::GzDecoder::new(&good[..])
            .read_to_string(&mut text)
            .unwrap();
        let regz = |lines: Vec<&str>| {
            let mut gz = GzEncoder::new(Vec::new(), Compression::default());
            gz.write_all((lines.join("\n") + "\n").as_bytes()).unwrap();
            gz.finish().unwrap()
        };
        let all: Vec<&str> = text.lines().collect();
        let without = |skip: usize| {
            all.iter()
                .enumerate()
                .filter(|(i, _)| *i != skip)
                .map(|(_, l)| *l)
                .collect::<Vec<_>>()
        };
        let mut wider = all.clone();
        wider[2] = "[\"x\", 1, 2]";
        let mut longer = all.clone();
        longer.insert(5, "[\"w\"]");
        // Every row of the first sheet lost its last column while the listing kept two.
        let mut narrower = all.clone();
        narrower[2] = "[\"x\"]";
        narrower[3] = "[\"y\"]";
        for (case, broken) in [
            // The first sheet cut short: the next sheet starts early.
            ("a sheet short of its rows", without(3)),
            // The grid ends inside its last sheet.
            ("a grid cut inside its last sheet", without(6)),
            // The grid ends before a sheet it lists.
            ("a grid without its second sheet", all[..5].to_vec()),
            ("a row wider than its sheet", wider),
            ("a sheet longer than its rows", longer),
            ("a sheet narrower than its columns", narrower),
        ] {
            assert_eq!(
                lines(&regz(broken)).err(),
                Some(GridError::Unreadable),
                "{case}"
            );
        }
    }

    #[test]
    fn a_line_or_a_grid_past_its_bound_is_too_large() {
        let bytes = grid("delimited", &[(None, vec![json!(["x".repeat(100)])])]);
        let mut reader = GridReader::open(&bytes[..], 1000, 1 << 20).unwrap();
        assert_eq!(reader.next_line().unwrap(), Some(Line::Sheet(0)));
        let mut tight = GridReader::open(&bytes[..], 1000, 1 << 20).unwrap();
        tight.max_line = 50;
        tight.next_line().unwrap();
        assert_eq!(tight.next_line().unwrap_err(), GridError::TooLarge);
        let first_len = {
            let mut text = String::new();
            flate2::read::GzDecoder::new(&bytes[..])
                .read_to_string(&mut text)
                .unwrap();
            text.lines().next().unwrap().len() as u64 + 1
        };
        let mut total = GridReader::open(&bytes[..], 1000, first_len + 12).unwrap();
        assert_eq!(total.next_line().unwrap(), Some(Line::Sheet(0)));
        assert_eq!(total.next_line().unwrap_err(), GridError::TooLarge);
    }

    #[test]
    fn columns_take_spreadsheet_letters() {
        assert_eq!(
            [0, 25, 26, 51, 52, 701, 702, 16_383].map(column_letters),
            ["A", "Z", "AA", "AZ", "BA", "ZZ", "AAA", "XFD"]
        );
    }
}
