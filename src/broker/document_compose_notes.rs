//! What a document shortened or left out, gathered by kind: each kind is said in one note that names every finding,
//! table or figure it applies to. There are 20 kinds, so a document carries at most 20 notes, which its provenance
//! (64 entries) and its receipt (32 notes of up to 240 characters) both hold whole.
use super::Refusal;

/// The most notes a document can carry; compose refuses rather than drop one past it.
pub(super) const MAX_NOTES: usize = 20;
/// The longest note; the receipt keeps each at most this long.
pub(super) const NOTE_CHARS: usize = 240;

#[derive(Default)]
pub(super) struct Notes {
    pub(super) question: bool,
    pub(super) summary: bool,
    pub(super) finding_paragraphs: Vec<usize>,
    pub(super) finding_spec: Vec<usize>,
    pub(super) findings_left_out: bool,
    pub(super) values_listed: Option<(usize, usize)>,
    pub(super) parts_long: bool,
    pub(super) repeated_columns: Vec<usize>,
    pub(super) repeated_tables: Vec<(usize, usize)>,
    pub(super) csv_marked: Vec<(usize, usize)>,
    pub(super) csv_left_out: Vec<usize>,
    pub(super) rows_shown: Vec<(usize, usize, usize)>,
    pub(super) long_cells: Vec<usize>,
    pub(super) altered_cells: Vec<(usize, usize)>,
    pub(super) figures_too_large: Vec<usize>,
    pub(super) figures_unreadable: Vec<usize>,
    pub(super) figures_left_out: usize,
    pub(super) slide_text_cut: bool,
    pub(super) slide_cells_cut: bool,
    pub(super) other_text: bool,
}

/// `1`, `1 and 2`, `1, 2 and 3`.
fn list(numbers: &[usize]) -> String {
    let text: Vec<String> = numbers.iter().map(usize::to_string).collect();
    match text.split_last() {
        Some((last, rest)) if !rest.is_empty() => format!("{} and {last}", rest.join(", ")),
        _ => text.join(""),
    }
}

/// `Finding 2` or `Findings 2 and 5`, with the verb that agrees.
fn named(word: &str, numbers: &[usize], one: &str, many: &str) -> String {
    if numbers.len() == 1 {
        format!("{word} {} {one}", list(numbers))
    } else {
        format!("{word}s {} {many}", list(numbers))
    }
}

fn counted(pairs: &[(usize, usize)]) -> String {
    pairs
        .iter()
        .map(|(table, n)| {
            format!(
                "Table {table}: {n} {}",
                if *n == 1 { "cell" } else { "cells" }
            )
        })
        .collect::<Vec<_>>()
        .join(", ")
}

impl Notes {
    /// The notes in a fixed order, at most `MAX_NOTES` of at most `NOTE_CHARS` characters each.
    pub(super) fn render(&self, deck: bool) -> Result<Vec<String>, Refusal> {
        let mut out = Vec::new();
        if self.question {
            out.push(
                "The question is longer than this document holds; its first part is shown."
                    .to_owned(),
            );
        }
        if self.summary {
            out.push(
                "The summary is longer than this document holds; its first part is shown."
                    .to_owned(),
            );
        }
        if !self.finding_paragraphs.is_empty() {
            out.push(format!(
                "{} longer than a paragraph holds; the first part is shown, and spec.json holds each whole.",
                named("Finding", &self.finding_paragraphs, "is", "are")
            ));
        }
        if !self.finding_spec.is_empty() {
            out.push(format!(
                "{} longer than spec.json keeps; the first 20,000 characters of each are kept.",
                named("Finding", &self.finding_spec, "is", "are")
            ));
        }
        if self.findings_left_out {
            out.push(format!(
                "Some findings did not fit {}; spec.json in the package lists every one.",
                if deck {
                    "on the slides"
                } else {
                    "in this document"
                }
            ));
        }
        if let Some((shown, total)) = self.values_listed {
            out.push(format!(
                "{shown} of {total} values are listed on slides; spec.json in the package lists every value with its cell."
            ));
        }
        if self.parts_long {
            out.push(
                "A part of the question or its reason is longer than a cell holds; its first part is shown.".to_owned(),
            );
        }
        if !self.repeated_columns.is_empty() {
            out.push(format!(
                "{} a column more than once; each column is shown once.",
                named("Table", &self.repeated_columns, "names", "name")
            ));
        }
        if !self.repeated_tables.is_empty() {
            let pairs: Vec<String> = self
                .repeated_tables
                .iter()
                .map(|(n, first)| format!("Table {n} repeats Table {first}"))
                .collect();
            out.push(format!("{}; each is shown once.", pairs.join(" and ")));
        }
        if !self.csv_marked.is_empty() {
            out.push(format!(
                "In the CSV copies, cells beginning =, +, -, @, a tab or sep= start with an apostrophe ({}); the \
                 result package holds the stored text.",
                counted(&self.csv_marked)
            ));
        }
        if !self.csv_left_out.is_empty() {
            out.push(format!(
                "{} larger than the package copies as CSV; its result package, listed under Sources, holds every row.",
                named("Table", &self.csv_left_out, "is", "are")
            ));
        }
        if !self.rows_shown.is_empty() {
            let shown: Vec<String> = self
                .rows_shown
                .iter()
                .map(|(table, rows, total)| format!("Table {table}: rows 1 to {rows} of {total}"))
                .collect();
            out.push(format!(
                "{} are shown; tables/ in the package holds every row.",
                shown.join(", ")
            ));
        }
        if !self.long_cells.is_empty() {
            out.push(format!(
                "{}: cells longer than 2,000 characters end in \u{2026} here; the CSV holds them whole.",
                named("Table", &self.long_cells, "", "").trim_end()
            ));
        }
        if !self.altered_cells.is_empty() {
            out.push(format!(
                "{} held characters these files cannot store (controls, U+FFFE, U+FFFF); they show as spaces or \
                 U+FFFD, and the CSV holds the stored text.",
                counted(&self.altered_cells)
            ));
        }
        if !self.figures_too_large.is_empty() {
            out.push(format!(
                "{} larger than this document holds; the package names each by digest.",
                named("Figure", &self.figures_too_large, "is", "are")
            ));
        }
        if !self.figures_unreadable.is_empty() {
            out.push(format!(
                "{} not a still PNG that shows every pixel as stored (no undefined pixel or colour profile), so this \
                 document leaves {} out; the package names each by digest.",
                named("Figure", &self.figures_unreadable, "is", "are"),
                if self.figures_unreadable.len() == 1 { "it" } else { "them" }
            ));
        }
        if self.figures_left_out > 0 {
            out.push(format!(
                "{} left out to keep the slide count within its limit.",
                if self.figures_left_out == 1 {
                    "A figure was".to_owned()
                } else {
                    format!("{} figures were", self.figures_left_out)
                }
            ));
        }
        if self.slide_text_cut {
            out.push(
                "On the slides, a title, heading or caption too long to fit ends in \u{2026}; the provenance, spec.json \
                 and the decision record hold the full text."
                    .to_owned(),
            );
        }
        if self.slide_cells_cut {
            out.push(
                "On the slides, cells too tall to fit and column names past 48 characters end in \u{2026}; the CSV \
                 copies, spec.json and the cited result packages hold them whole."
                    .to_owned(),
            );
        }
        if self.other_text {
            out.push(
                "Some text held control characters or U+FFFE or U+FFFF, which these files cannot store; they are shown \
                 as spaces or U+FFFD, and spec.json keeps the stored values."
                    .to_owned(),
            );
        }
        if out.len() > MAX_NOTES || out.iter().any(|n| n.chars().count() > NOTE_CHARS) {
            return Err(Refusal("document_compose_failed"));
        }
        Ok(out)
    }
}
