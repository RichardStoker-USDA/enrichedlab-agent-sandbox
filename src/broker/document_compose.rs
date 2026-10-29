//! What an answer document says (migration 072). Code writes every part of it from the stored
//! decision and the result bytes its request pinned: the question as the decision's checkpoint states it, the agent's
//! reading and summary, each finding with its values copied from the cells it binds, where each value comes from, the
//! parts of the question, each answer table from its retained file, the R figures the cited packages hold, and the
//! sources with their digests. No model is called and no value is retyped. The limits are the broker's writer's
//! (`document_write.rs`); anything shortened to fit is said in the document and in the receipt's notes
//! (`document_compose_notes.rs`).
//! The same request and bytes always give the same body, so a retried document is the same file.
use std::collections::HashMap;

use base64::Engine;
use serde_json::{Number, Value, json};
use time::format_description::well_known::Rfc3339;

use super::{
    digest,
    document_write::{self, png::Png, slide_fit},
    store::TaskDocument,
    tables::{ColumnType, Table},
};

pub(super) const KINDS: [&str; 3] = ["xlsx", "docx", "pptx"];
const MAX_SECTIONS: usize = 40;
const HEADING_CHARS: usize = 200;
const PARAGRAPH_CHARS: usize = 2000;
const PARAGRAPHS: usize = 20;
const CAPTION_CHARS: usize = 300;
const CELL_CHARS: usize = 2000;
const TABLE_ROWS: usize = 20_000;
const SLIDE_ROWS: usize = 15;
const SLIDES_PER_TABLE: usize = 3;
const SLIDES_PER_LIST: usize = 4;
/// The longest paragraph piece a deck sends: about thirteen lines of its 20-point text, so one always fits a slide.
const SLIDE_PIECE_CHARS: usize = 900;
const SLIDE_PARAGRAPHS: usize = 6;
const SLIDE_COLUMN_CHARS: usize = 48;
const PROVENANCE_ENTRIES: usize = 64;
const FIGURE_BYTES: usize = 8 * 1024 * 1024;

/// Why a document cannot be written from what its request pinned.
#[derive(Debug, PartialEq, Eq)]
pub(super) struct Refusal(pub(super) &'static str);

/// Everything a document is built from: the request and the parsed tables and PNG bytes it pinned.
pub(super) struct Inputs<'a> {
    pub(super) request: &'a TaskDocument,
    /// Each pinned table, by result ref and table path.
    pub(super) tables: &'a HashMap<(String, String), Table>,
    /// Each pinned figure's PNG, in the request's order.
    pub(super) figures: &'a [Vec<u8>],
}

/// The writer's request and what the package keeps beside the file.
pub(super) struct Composed {
    /// The exact bytes the writer reads (`request.json` in the package).
    pub(super) body: Vec<u8>,
    /// The counts the writer must report for this body.
    pub(super) counts: Value,
    /// Each answer table whole, as CSV, by package path.
    pub(super) csv: Vec<(String, Vec<u8>)>,
    /// Each finding's sentence whole, with its values filled in.
    pub(super) findings: Vec<Value>,
    /// Each finding value with the cell it came from.
    pub(super) values: Vec<Value>,
    /// What was shortened or left out to fit the kind's limits.
    pub(super) notes: Vec<String>,
}

enum Block {
    Paragraphs(Vec<String>),
    Table {
        columns: Vec<String>,
        rows: Vec<Vec<Value>>,
        caption: String,
        reference: String,
    },
    Figure {
        png: String,
        caption: String,
        reference: String,
    },
}

struct Section {
    heading: String,
    block: Block,
}

/// Where a value was copied from: result ref, table path, row from 1, column as the digest names it.
type CellSource = (String, String, u64, String);
/// A finding's placeholder, the value put in its place, and where it was copied from.
type FilledValue = (String, String, CellSource);

fn record_invalid() -> Refusal {
    Refusal("document_record_invalid")
}

/// Text as these files can hold it. XML 1.0 holds no C0 control but tab, line feed and carriage return and no U+FFFE or
/// U+FFFF, and Office shows C1 controls as boxes or drops them; here each control but tab and line feed becomes a space
/// and each of those two, and each direction control, U+FFFD, so nothing is dropped, joined or reordered unseen. Returns whether anything changed, so the
/// document can say so.
pub(super) fn plain(text: &str) -> (String, bool) {
    let mut changed = false;
    let out = text
        .chars()
        .map(|c| match c {
            '\t' | '\n' => c,
            '\u{0}'..='\u{8}' | '\u{b}'..='\u{1f}' | '\u{7f}'..='\u{9f}' => {
                changed = true;
                ' '
            }
            // A direction control could display an inserted value in another order; it shows as U+FFFD instead.
            '\u{fffe}'
            | '\u{ffff}'
            | '\u{061c}'
            | '\u{200e}'
            | '\u{200f}'
            | '\u{202a}'..='\u{202e}'
            | '\u{2066}'..='\u{2069}' => {
                changed = true;
                '\u{fffd}'
            }
            c => c,
        })
        .collect();
    (out, changed)
}

/// `plain` over every text a section sends: heading, paragraphs, column names, text cells, captions and references.
/// True when any changed.
fn plain_section(section: &mut Section) -> bool {
    fn fix(text: &mut String) -> bool {
        let (out, changed) = plain(text);
        *text = out;
        changed
    }
    let mut changed = fix(&mut section.heading);
    match &mut section.block {
        Block::Paragraphs(list) => list.iter_mut().for_each(|p| changed |= fix(p)),
        Block::Table {
            columns,
            rows,
            caption,
            reference,
        } => {
            columns.iter_mut().for_each(|c| changed |= fix(c));
            for cell in rows.iter_mut().flatten() {
                if let Value::String(text) = cell {
                    changed |= fix(text);
                }
            }
            changed |= fix(caption) | fix(reference);
        }
        Block::Figure {
            caption, reference, ..
        } => changed |= fix(caption) | fix(reference),
    }
    changed
}

/// Text cut to `max` characters, ending in `…` when cut.
pub(super) fn cut(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        return text.to_owned();
    }
    let mut out: String = text.chars().take(max.saturating_sub(1)).collect();
    out.push('…');
    out
}

/// Text in pieces of at most `max` characters, broken at line ends, then at the last space before the limit.
pub(super) fn pieces(text: &str, max: usize) -> Vec<String> {
    let mut out = Vec::new();
    for line in text
        .split('\n')
        .map(str::trim)
        .filter(|line| !line.is_empty())
    {
        let mut rest: Vec<char> = line.chars().collect();
        while rest.len() > max {
            let at = rest[..max]
                .iter()
                .rposition(|c| c.is_whitespace())
                .filter(|&i| i > 0)
                .unwrap_or(max);
            out.push(rest[..at].iter().collect::<String>().trim_end().to_owned());
            rest = rest[at..]
                .iter()
                .skip_while(|c| c.is_whitespace())
                .copied()
                .collect();
        }
        if !rest.is_empty() {
            out.push(rest.into_iter().collect());
        }
    }
    out
}

/// Paragraphs grouped for slides: each slide's within the paragraph box by the writer's estimate, at most six to a
/// slide. A paragraph too tall for a slide alone continues on the next.
fn slides(paragraphs: Vec<String>) -> Vec<Vec<String>> {
    let mut out: Vec<Vec<String>> = Vec::new();
    for paragraph in paragraphs.iter().flat_map(|p| split_to_fit(p)) {
        let joins = out.last().is_some_and(|slide| {
            slide.len() < SLIDE_PARAGRAPHS && {
                let mut next = slide.clone();
                next.push(paragraph.clone());
                slide_fit::text(&next)
            }
        });
        match out.last_mut() {
            Some(slide) if joins => slide.push(paragraph),
            _ => out.push(vec![paragraph]),
        }
    }
    out
}

/// A paragraph in consecutive pieces that each fit a slide alone, broken at the last space that fits, or between
/// characters when none does; nothing is left out but the space at a break.
fn split_to_fit(paragraph: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut rest: Vec<char> = paragraph.chars().collect();
    loop {
        let whole: String = rest.iter().collect();
        if slide_fit::text(std::slice::from_ref(&whole)) {
            if !whole.is_empty() || out.is_empty() {
                out.push(whole);
            }
            return out;
        }
        let fits = |n: usize| slide_fit::text(&[rest[..n].iter().collect()]);
        let (mut low, mut high) = (1, rest.len());
        while low < high {
            let mid = (low + high).div_ceil(2);
            if fits(mid) {
                low = mid;
            } else {
                high = mid - 1;
            }
        }
        let at = rest[..low]
            .iter()
            .rposition(|c| c.is_whitespace())
            .filter(|&i| i > 0)
            .unwrap_or(low);
        out.push(rest[..at].iter().collect::<String>().trim_end().to_owned());
        rest = rest[at..]
            .iter()
            .skip_while(|c| c.is_whitespace())
            .copied()
            .collect();
    }
}

/// The longest start of `text`, ending in an ellipsis, that `fits` takes.
fn fit_text(text: &str, fits: impl Fn(&str) -> bool) -> String {
    let (mut low, mut high) = (1, text.chars().count());
    while low < high {
        let mid = (low + high).div_ceil(2);
        if fits(&cut(text, mid)) {
            low = mid;
        } else {
            high = mid - 1;
        }
    }
    cut(text, low)
}

/// A deck's headings and captions shortened to their boxes by the writer's estimate; true when any was.
fn fit_slide_text(sections: &mut [Section]) -> bool {
    let mut cut_any = false;
    let mut fit = |text: &mut String, fits: fn(&str) -> bool| {
        if !fits(text) {
            *text = fit_text(text, fits);
            cut_any = true;
        }
    };
    for section in sections {
        fit(&mut section.heading, slide_fit::heading);
        match &mut section.block {
            Block::Table {
                caption, reference, ..
            }
            | Block::Figure {
                caption, reference, ..
            } => {
                fit(caption, slide_fit::caption);
                fit(reference, slide_fit::reference);
            }
            Block::Paragraphs(_) => {}
        }
    }
    cut_any
}

/// Cut the longest text cells of a row too tall for a slide alone until it fits; true when any was cut.
fn fit_row(columns: &[String], row: &mut [Value]) -> bool {
    let mut cut_any = false;
    while !slide_fit::table(columns, &[row.to_vec()]) {
        let longest = row
            .iter()
            .enumerate()
            .filter_map(|(i, v)| v.as_str().map(|s| (i, s.chars().count())))
            .max_by_key(|&(i, n)| (n, std::cmp::Reverse(i)));
        match longest {
            Some((i, n)) if n > 2 => {
                let text = row[i].as_str().unwrap_or("").to_owned();
                row[i] = Value::String(cut(&text, n / 2));
                cut_any = true;
            }
            _ => break,
        }
    }
    cut_any
}

/// The index of the column a digest names, by the digest's own naming of the header.
fn column_index(table: &Table, name: &str) -> Option<usize> {
    table
        .columns
        .iter()
        .enumerate()
        .position(|(index, raw)| digest::column_name(raw.as_bytes(), index) == name)
}

/// The cell a binding names, as the digest showed it to the agent and the workspace shows it.
fn bound_value(
    tables: &HashMap<(String, String), Table>,
    binding: &Value,
) -> Result<(String, CellSource), Refusal> {
    let missing = || Refusal("document_value_missing");
    let reference = binding["ref"].as_str().ok_or_else(record_invalid)?;
    let path = binding["table"].as_str().ok_or_else(record_invalid)?;
    let row = binding["row"]
        .as_u64()
        .filter(|&row| row >= 1)
        .ok_or_else(record_invalid)?;
    let column = binding["column"].as_str().ok_or_else(record_invalid)?;
    let table = tables
        .get(&(reference.to_owned(), path.to_owned()))
        .ok_or_else(missing)?;
    let index = column_index(table, column).ok_or_else(missing)?;
    let raw = table
        .cell(usize::try_from(row - 1).map_err(|_| missing())?, index)
        .ok_or_else(missing)?;
    let value = digest::clean(raw.as_bytes(), CELL_CHARS).ok_or_else(missing)?;
    Ok((
        value,
        (
            reference.to_owned(),
            path.to_owned(),
            row,
            column.to_owned(),
        ),
    ))
}

/// A finding's sentence with each placeholder replaced by its bound value, and the values in order of use.
fn fill(
    finding: &Value,
    tables: &HashMap<(String, String), Table>,
) -> Result<(String, Vec<FilledValue>), Refusal> {
    let statement = finding["statement"].as_str().ok_or_else(record_invalid)?;
    let empty = serde_json::Map::new();
    let bindings = match &finding["bindings"] {
        Value::Object(map) => map,
        Value::Null => &empty,
        _ => return Err(record_invalid()),
    };
    let (mut sentence, mut values, mut rest) = (String::new(), Vec::new(), statement);
    while let Some(open) = rest.find('{') {
        let name_end = rest[open + 1..].find('}').map(|i| open + 1 + i);
        let name = name_end.map(|end| &rest[open + 1..end]);
        let valid = name.is_some_and(|n| {
            (1..=24).contains(&n.len())
                && n.as_bytes()[0].is_ascii_lowercase()
                && n.bytes()
                    .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_')
        });
        let (Some(end), Some(name), true) = (name_end, name, valid) else {
            // A brace that opens no placeholder is text, as the workspace shows it.
            sentence.push_str(&rest[..=open]);
            rest = &rest[open + 1..];
            continue;
        };
        let binding = bindings.get(name).ok_or_else(record_invalid)?;
        let (value, source) = bound_value(tables, binding)?;
        sentence.push_str(&rest[..open]);
        sentence.push_str(&value);
        values.push((name.to_owned(), value, source));
        rest = &rest[end + 1..];
    }
    sentence.push_str(rest);
    Ok((sentence, values))
}

/// A stored cell for a workbook: a number only when the column holds numbers and the text is the number's own plain
/// spelling (no exponent, no leading or trailing zeros, at most 15 significant digits) and the parsed value prints back
/// as the same text, so the cell holds the same value and digits (a spelling past the double's range, such as one that
/// would underflow to zero, stays text); an empty cell in a number column is blank; anything else is the stored text.
pub(super) fn workbook_cell(text: &str, column: ColumnType) -> Value {
    if !matches!(column, ColumnType::Integer | ColumnType::Number) {
        return Value::String(text.to_owned());
    }
    if text.is_empty() {
        return Value::Null;
    }
    let unsigned = text.strip_prefix('-').unwrap_or(text);
    let (whole, fraction) = match unsigned.split_once('.') {
        Some((whole, fraction)) => (whole, Some(fraction)),
        None => (unsigned, None),
    };
    let plain_whole = !whole.is_empty()
        && whole.bytes().all(|b| b.is_ascii_digit())
        && (whole == "0" || !whole.starts_with('0'));
    let plain_fraction = fraction.is_none_or(|f| {
        !f.is_empty() && f.bytes().all(|b| b.is_ascii_digit()) && !f.ends_with('0')
    });
    let significant = format!("{whole}{}", fraction.unwrap_or(""))
        .trim_start_matches('0')
        .len();
    if !plain_whole || !plain_fraction || significant > 15 || text == "-0" {
        return Value::String(text.to_owned());
    }
    let number = match fraction {
        None => text.parse::<i64>().ok().map(Number::from),
        Some(_) => text
            .parse::<f64>()
            .ok()
            .filter(|f| f.is_finite() && format!("{f}") == text)
            .and_then(Number::from_f64),
    };
    number.map_or_else(|| Value::String(text.to_owned()), Value::Number)
}

/// The agent's reading of the answer, in a document's words; never a check.
fn reading(answer: &str) -> &'static str {
    match answer {
        "answered" => "The agent's reading is that the results answer the question.",
        "partly_answered" => "The agent's reading is that the results answer part of the question.",
        "not_answered" => "The agent's reading is that the results do not answer the question.",
        _ => "The agent did not say whether the results answer the question.",
    }
}

fn part_state(state: &str) -> &'static str {
    match state {
        "supported" => "Answered",
        "partial" => "Partly answered",
        "unsupported" => "Not in this data",
        "open" => "Still being worked on",
        "awaiting" => "Waiting for a person's choice",
        _ => "Not stated",
    }
}

fn file_name(path: &str) -> &str {
    path.rsplit('/').next().unwrap_or(path)
}

/// How many of a table's rows a deck shows in at most `per` slides of fifteen rows.
fn deck_rows(rows: usize, per: usize) -> usize {
    rows.min(per * SLIDE_ROWS)
}

/// Push paragraph sections: one per slide group for a deck, one section otherwise.
fn push_paragraphs(
    sections: &mut Vec<Section>,
    deck: bool,
    heading: &str,
    paragraphs: Vec<String>,
) {
    if paragraphs.is_empty() {
        return;
    }
    let groups = if deck {
        slides(paragraphs)
    } else {
        vec![paragraphs]
    };
    for (n, group) in groups.into_iter().enumerate() {
        sections.push(Section {
            heading: continued(heading, n),
            block: Block::Paragraphs(group),
        });
    }
}

fn continued(heading: &str, n: usize) -> String {
    if n == 0 {
        cut(heading, HEADING_CHARS)
    } else {
        cut(&format!("{heading} ({})", n + 1), HEADING_CHARS)
    }
}

/// A table as one section, or for a deck one section per slide: rows join a slide while the table fits it by the
/// writer's estimate, at most fifteen, a row too tall for a slide alone has its longest cells cut to fit, and a column
/// name longer than 48 characters is cut.
struct TableParts<'a> {
    heading: &'a str,
    columns: Vec<String>,
    rows: Vec<Vec<Value>>,
    caption: &'a str,
    reference: &'a str,
}

/// Returns how many rows the sections show (a deck shows at most `max_slides` slides) and whether a cell or a column
/// name was cut.
fn push_table(
    sections: &mut Vec<Section>,
    deck: bool,
    table: TableParts<'_>,
    max_slides: usize,
) -> (usize, bool) {
    let TableParts {
        heading,
        mut columns,
        mut rows,
        caption,
        reference,
    } = table;
    let shown = rows.len();
    if !deck {
        sections.push(Section {
            heading: cut(heading, HEADING_CHARS),
            block: Block::Table {
                columns,
                rows,
                caption: cut(caption, CAPTION_CHARS),
                reference: cut(reference, CAPTION_CHARS),
            },
        });
        return (shown, false);
    }
    let mut cut_any = false;
    for name in &mut columns {
        if name.chars().count() > SLIDE_COLUMN_CHARS {
            *name = cut(name, SLIDE_COLUMN_CHARS);
            cut_any = true;
        }
    }
    let (mut start, mut made) = (0, 0);
    while made < max_slides && (made == 0 || start < rows.len()) {
        let mut end = start;
        while end < rows.len() && end - start < SLIDE_ROWS {
            if slide_fit::table(&columns, &rows[start..=end]) {
                end += 1;
            } else if end == start && fit_row(&columns, &mut rows[start]) {
                cut_any = true;
            } else {
                break;
            }
        }
        if end == start && start < rows.len() {
            // A row that still does not fit alone is sent alone, and the writer refuses the deck.
            end += 1;
        }
        sections.push(Section {
            heading: continued(heading, made),
            block: Block::Table {
                columns: columns.clone(),
                rows: rows[start..end].to_vec(),
                caption: cut(caption, CAPTION_CHARS),
                reference: cut(reference, CAPTION_CHARS),
            },
        });
        made += 1;
        start = end;
    }
    (start, cut_any)
}

#[path = "document_compose_notes.rs"]
mod notes;
use notes::Notes;
#[path = "document_compose_parts.rs"]
mod parts;
pub(super) use parts::compose;

#[cfg(test)]
#[path = "document_compose_slides_tests.rs"]
mod slides_tests;
#[cfg(test)]
#[path = "document_compose_tests.rs"]
mod tests;
