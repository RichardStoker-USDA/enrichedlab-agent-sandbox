//! The slides' layout and the writer's estimate of what fits it. Every box a slide has is here with the type size it
//! uses: the first slide's title, a section slide's title, body, caption area, paragraph box and footer, and the
//! provenance columns. PowerPoint lays text out when it shows a slide, and a file cannot ask it how much room a text
//! takes, so the writer estimates the room from above and refuses a body whose text the estimate does not fit
//! (`slide_content_invalid`, `table_too_large_for_slide`). Compose packs a deck's rows and paragraphs, and shortens a
//! title, heading or caption, by the same estimate, so what it writes is not refused.
//!
//! The estimate wraps a paragraph the way PowerPoint does, at spaces and tabs, and breaks a word wider than the line
//! between characters. It takes each character at least as wide as Calibri, the deck's font, draws its kind in any of
//! its four faces, as measured from the fonts Office ships: a space 0.3 em (Calibri's is 0.226); M, W, m, w, @, % and &
//! 1 em (widest 0.906); other capitals 0.75 em (0.686); the rest of ASCII 0.6 em (0.538); Latin-1, IPA, Greek, Latin
//! Extended Additional, super- and subscripts and mathematical operators 1 em (0.919); Latin Extended-A and -B,
//! Cyrillic, punctuation, currency and letterlike symbols 1.2 em (1.116); emoji and dingbats 2 em; Arabic ligatures,
//! the two- and three-em dashes, cuneiform, hieroglyphs and musical symbols 4 em; any other character 1.4 em (Calibri's
//! widest glyph is 1.328); and a tab a whole inch, PowerPoint's default tab stop. A line is 1.25 em tall (Calibri's is
//! 1.2207), or 2 em when it holds a character Calibri does not draw, since another font draws that one. A font
//! PowerPoint substitutes with taller lines or wider glyphs than these can still take more room than the estimate gives
//! it.
use serde_json::Value;

use super::{Entry, Request, WriteError, cell_text, column_widths};

// Geometry in EMU (914,400 to the inch, 12,700 to the point) on a 13.333 by 7.5 inch slide.
pub(super) const LEFT: i64 = 548_640;
pub(super) const WIDTH: i64 = 11_094_720;
/// The first slide's title box (top, height), its gold rule's top and the subtitle box under the rule.
pub(super) const FIRST_TITLE: (i64, i64) = (457_200, 3_017_520);
pub(super) const FIRST_RULE: i64 = 3_611_880;
pub(super) const SUBTITLE: (i64, i64) = (3_794_760, 457_200);
/// A section slide's title box, the gold rule under it (top, width, height), the body a table or figure takes, the
/// caption area under the body, the paragraph box (body and caption area together) and the footer.
pub(super) const TITLE: (i64, i64) = (137_160, 914_400);
pub(super) const RULE: (i64, i64, i64) = (1_097_280, 822_960, 54_864);
pub(super) const BODY: (i64, i64) = (1_325_880, 4_023_360);
pub(super) const CAPTION: (i64, i64) = (5_440_680, 960_120);
pub(super) const TEXT: (i64, i64) = (BODY.0, CAPTION.0 + CAPTION.1 - BODY.0);
pub(super) const FOOTER: (i64, i64) = (6_446_520, 274_320);
/// The slide number's box at the footer's right end, the gap before it, and the gap between provenance columns.
pub(super) const NUMBER_WIDTH: i64 = 1_280_160;
pub(super) const GAP: i64 = 91_440;
pub(super) const COLUMN_GAP: i64 = 365_760;
/// A table cell's margins: left and right, top and bottom.
pub(super) const CELL_MARGIN: (i64, i64) = (91_440, 36_576);
pub(super) const PROVENANCE_COLUMNS: usize = 2;
/// The most rows and columns a slide's table may have.
pub(super) const SLIDE_ROWS: usize = 15;
pub(super) const SLIDE_COLUMNS: usize = 8;

/// Type sizes in hundredths of a point.
pub(super) const TITLE_SIZE: u32 = 3600;
pub(super) const SUBTITLE_SIZE: u32 = 1600;
pub(super) const HEADING_SIZE: u32 = 2800;
pub(super) const TEXT_SIZE: u32 = 2000;
pub(super) const TABLE_SIZE: u32 = 1100;
pub(super) const CAPTION_SIZE: u32 = 1200;
pub(super) const REFERENCE_SIZE: u32 = 1000;
pub(super) const PROVENANCE_SIZE: u32 = 1200;
pub(super) const FOOTER_SIZE: u32 = 900;

const EMU_PER_POINT: f64 = 12_700.0;
const TAB_POINTS: f64 = 72.0;
const LINE_EM: f64 = 1.25;
const OTHER_FONT_LINE_EM: f64 = 2.0;

fn points(emu: i64) -> f64 {
    emu as f64 / EMU_PER_POINT
}

fn size_points(size: u32) -> f64 {
    f64::from(size) / 100.0
}

/// A character's width in ems, as the module comment gives it.
fn width_em(c: char) -> f64 {
    match c {
        ' ' => 0.3,
        'M' | 'W' | 'm' | 'w' | '@' | '%' | '&' => 1.0,
        'A'..='Z' => 0.75,
        _ if c.is_ascii() => 0.6,
        _ => match u32::from(c) {
            0xA0..=0xFF | 0x250..=0x3FF | 0x1E00..=0x1FFF | 0x2070..=0x209F | 0x2200..=0x22FF => {
                1.0
            }
            0x100..=0x24F | 0x400..=0x4FF | 0x2000..=0x206F | 0x20A0..=0x20CF | 0x2100..=0x218F => {
                1.2
            }
            0x2600..=0x27BF | 0x1F000..=0x1FAFF => 2.0,
            0x2E00..=0x2E7F
            | 0xFB50..=0xFDFF
            | 0xFE70..=0xFEFF
            | 0x12000..=0x1254F
            | 0x13000..=0x1345F
            | 0x1D000..=0x1D24F => 4.0,
            _ => 1.4,
        },
    }
}

/// Whether all four of Calibri's faces draw the character (a subset of their shared coverage), so its line keeps
/// Calibri's height.
fn calibri(c: char) -> bool {
    matches!(
        u32::from(c),
        0x09 | 0x20..=0x7E
            | 0xA0..=0x377
            | 0x37A..=0x37F
            | 0x384..=0x38A
            | 0x38C
            | 0x38E..=0x3A1
            | 0x3A3..=0x486
            | 0x488..=0x52F
            | 0x1E00..=0x1EFF
            | 0x2000..=0x2010
            | 0x2012..=0x2022
            | 0x2026
            | 0x202A..=0x2030
            | 0x2032..=0x2034
            | 0x2038..=0x203A
            | 0x20A0..=0x20B5
            | 0x20B8..=0x20BF
            | 0x2122
            | 0x2190..=0x2199
            | 0x2202
            | 0x2206
            | 0x220F
            | 0x2211..=0x2212
            | 0x2215
            | 0x2219..=0x221A
            | 0x221E..=0x221F
            | 0x2229
            | 0x222B
            | 0x2248
            | 0x2260..=0x2261
            | 0x2264..=0x2265
    )
}

/// One line of a paragraph being wrapped.
struct Wrap {
    width: f64,
    size: f64,
    x: f64,
    empty: bool,
    tall: bool,
    /// A character or tab wider than the whole box, which no wrapping can place inside it.
    too_wide: bool,
    height: f64,
}

impl Wrap {
    fn advance(&self, c: char) -> f64 {
        if c == '\t' {
            TAB_POINTS
        } else {
            width_em(c) * self.size
        }
    }

    fn line_height(&self) -> f64 {
        self.size
            * if self.tall {
                OTHER_FONT_LINE_EM
            } else {
                LINE_EM
            }
    }

    fn break_line(&mut self) {
        self.height += self.line_height();
        (self.x, self.empty, self.tall) = (0.0, true, false);
    }

    /// Place one character or tab, starting a line when it does not fit on this one.
    fn place(&mut self, c: char) {
        let advance = self.advance(c);
        self.too_wide |= advance > self.width;
        if !self.empty && self.x + advance > self.width {
            self.break_line();
        }
        self.x += advance;
        self.empty = false;
        self.tall |= !calibri(c);
    }

    /// Place a word after the spaces and tabs before it: on this line when both fit, else at the start of the next,
    /// where the spaces hang past the line they leave and the tabs come along. Spaces and tabs that start a line take
    /// room and wrap as characters do. A word wider than a line breaks between characters.
    fn word(&mut self, gap: &[char], word: &[char]) {
        let gap_width: f64 = gap.iter().map(|&c| self.advance(c)).sum();
        let wide: f64 = word.iter().map(|&c| self.advance(c)).sum();
        if self.empty {
            gap.iter().for_each(|&c| self.place(c));
        } else if self.x + gap_width + wide > self.width {
            self.break_line();
            gap.iter()
                .filter(|&&c| c == '\t')
                .for_each(|&c| self.place(c));
        } else {
            self.x += gap_width;
        }
        word.iter().for_each(|&c| self.place(c));
    }

    /// The height of one line of text with no line break in it, or infinity when one of its characters or tabs is
    /// wider than the box. Spaces at the end of the line hang past it; tabs there take room.
    fn wrap(mut self, line: &str) -> f64 {
        let (mut gap, mut word) = (Vec::new(), Vec::new());
        for c in line.chars() {
            if c == ' ' || c == '\t' {
                if !word.is_empty() {
                    self.word(&gap, &word);
                    (gap, word) = (Vec::new(), Vec::new());
                }
                gap.push(c);
            } else {
                word.push(c);
            }
        }
        if word.is_empty() {
            gap.iter()
                .filter(|&&c| c == '\t')
                .for_each(|&c| self.place(c));
        } else {
            self.word(&gap, &word);
        }
        if self.too_wide {
            f64::INFINITY
        } else {
            self.height + self.line_height()
        }
    }
}

/// The height in points `text` takes as one paragraph in a box `width` points wide at `size` points, each of its line
/// breaks starting a line.
pub(super) fn height(text: &str, width: f64, size: f64) -> f64 {
    text.split('\n')
        .map(|line| {
            Wrap {
                width,
                size,
                x: 0.0,
                empty: true,
                tall: false,
                too_wide: false,
                height: 0.0,
            }
            .wrap(line)
        })
        .sum()
}

/// A paragraph's height in a box `width` EMU wide with the space after it, three fifths of its size, as the slides
/// write it.
fn paragraph(text: &str, width: i64, size: u32) -> f64 {
    height(text, points(width), size_points(size)) + size_points(size * 3 / 5)
}

fn fits(text: &str, width: i64, size: u32, box_height: i64) -> bool {
    height(text, points(width), size_points(size)) <= points(box_height)
}

/// Whether a title fits the first slide's title box.
pub(crate) fn title(text: &str) -> bool {
    fits(text, WIDTH, TITLE_SIZE, FIRST_TITLE.1)
}

/// Whether a heading fits a section slide's title box.
pub(crate) fn heading(text: &str) -> bool {
    fits(text, WIDTH, HEADING_SIZE, TITLE.1)
}

/// Whether paragraphs fit the paragraph box, each with the space after it.
pub(crate) fn text(paragraphs: &[String]) -> bool {
    paragraphs
        .iter()
        .map(|p| paragraph(p, WIDTH, TEXT_SIZE))
        .sum::<f64>()
        <= points(TEXT.1)
}

/// Whether a caption takes at most two lines' room; with a reference within two lines of its own, both always fit the
/// caption area. Compose shortens each to this.
pub(crate) fn caption(text: &str) -> bool {
    height(text, points(WIDTH), size_points(CAPTION_SIZE))
        <= 2.0 * LINE_EM * size_points(CAPTION_SIZE)
}

pub(crate) fn reference(text: &str) -> bool {
    height(text, points(WIDTH), size_points(REFERENCE_SIZE))
        <= 2.0 * LINE_EM * size_points(REFERENCE_SIZE)
}

/// Whether a caption and a reference fit the caption area together, each with the space after it.
fn notes(caption: &str, reference: &str) -> bool {
    let part = |text: &str, size| {
        if text.is_empty() {
            0.0
        } else {
            paragraph(text, WIDTH, size)
        }
    };
    part(caption, CAPTION_SIZE) + part(reference, REFERENCE_SIZE) <= points(CAPTION.1)
}

/// Each row's height in EMU, the header first: its tallest cell's text and the cell's top and bottom margins.
pub(super) fn row_heights(columns: &[String], rows: &[Vec<Value>], widths: &[i64]) -> Vec<i64> {
    let row = |cells: Vec<String>| -> i64 {
        let tallest = cells
            .iter()
            .zip(widths)
            .map(|(text, width)| {
                let inside = (width - 2 * CELL_MARGIN.0).max(EMU_PER_POINT as i64);
                height(text, points(inside), size_points(TABLE_SIZE))
            })
            .fold(0.0, f64::max);
        // A row that cannot be shown at all is taller than the body, whatever else it holds.
        if !tallest.is_finite() {
            return BODY.1 + 1;
        }
        (tallest * EMU_PER_POINT).ceil() as i64 + 2 * CELL_MARGIN.1
    };
    std::iter::once(row(columns.to_vec()))
        .chain(rows.iter().map(|r| row(r.iter().map(cell_text).collect())))
        .collect()
}

/// Whether a table of these rows fits a slide's body.
pub(crate) fn table(columns: &[String], rows: &[Vec<Value>]) -> bool {
    let widths = column_widths(columns, rows, WIDTH);
    row_heights(columns, rows, &widths).iter().sum::<i64>() <= BODY.1
}

pub(super) fn provenance_column_width() -> i64 {
    (WIDTH - COLUMN_GAP * (PROVENANCE_COLUMNS as i64 - 1)) / PROVENANCE_COLUMNS as i64
}

fn entry_height(entry: &Entry) -> f64 {
    paragraph(
        &format!("{}: {}", entry.label, entry.value),
        provenance_column_width(),
        PROVENANCE_SIZE,
    )
}

/// The provenance entries on as many slides as they need, two columns to a slide, each column filled while its entries
/// fit the paragraph box.
pub(super) fn provenance_slides(entries: &[Entry]) -> Vec<Vec<Vec<&Entry>>> {
    let mut slides: Vec<Vec<Vec<&Entry>>> = vec![vec![Vec::new()]];
    let mut used = 0.0;
    for entry in entries {
        let height = entry_height(entry);
        let slide = slides.last_mut().expect("a slide");
        if used + height > points(TEXT.1) && !slide.last().expect("a column").is_empty() {
            if slide.len() == PROVENANCE_COLUMNS {
                slides.push(vec![Vec::new()]);
            } else {
                slide.push(Vec::new());
            }
            used = 0.0;
        }
        used += height;
        slides
            .last_mut()
            .and_then(|slide| slide.last_mut())
            .expect("a column")
            .push(entry);
    }
    slides
}

fn footer_fits(text: &str) -> bool {
    height(
        text,
        points(WIDTH - NUMBER_WIDTH - GAP),
        size_points(FOOTER_SIZE),
    ) <= OTHER_FONT_LINE_EM * size_points(FOOTER_SIZE)
}

/// The title as the footer repeats it: the longest start that fits the footer's one line, ending in … when shortened.
/// The first slide shows it whole.
pub(super) fn footer_title(title: &str) -> String {
    if footer_fits(title) {
        return title.to_owned();
    }
    let chars: Vec<char> = title.chars().collect();
    let shortened = |n: usize| {
        let mut out: String = chars[..n].iter().collect();
        out.push('\u{2026}');
        out
    };
    let (mut low, mut high) = (0, chars.len());
    while low < high {
        let mid = (low + high).div_ceil(2);
        if footer_fits(&shortened(mid)) {
            low = mid;
        } else {
            high = mid - 1;
        }
    }
    shortened(low)
}

/// Refuse a deck body any of whose text the estimate does not fit its box.
pub(super) fn check(request: &Request) -> Result<(), WriteError> {
    let refuse = || WriteError("slide_content_invalid");
    if !title(&request.title) {
        return Err(refuse());
    }
    for section in &request.sections {
        if !heading(&section.heading) || !text(&section.paragraphs) {
            return Err(refuse());
        }
        if let Some(t) = &section.table {
            if t.rows.len() > SLIDE_ROWS
                || t.columns.len() > SLIDE_COLUMNS
                || !table(&t.columns, &t.rows)
            {
                return Err(WriteError("table_too_large_for_slide"));
            }
            if !notes(&t.caption, &t.reference) {
                return Err(refuse());
            }
        }
        if let Some(f) = &section.figure
            && !notes(&f.caption, &f.reference)
        {
            return Err(refuse());
        }
    }
    if request
        .provenance
        .iter()
        .any(|entry| entry_height(entry) > points(TEXT.1))
    {
        return Err(refuse());
    }
    Ok(())
}

#[cfg(test)]
#[path = "document_write_fit_tests.rs"]
mod tests;
