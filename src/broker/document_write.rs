//! The broker's writer for answer documents: a Word document, an Excel
//! workbook or PowerPoint slides of one request body, the JSON `document_compose` makes. Code writes every part of the
//! file from that body, with no template file, no other program and no model, so nothing reaches the file that the body
//! did not hold. Each figure is the PNG the body carries, byte for byte, after a check that it is one well-formed still
//! image. The same body always gives the same bytes: the parts, their order, their compression and the ZIP dates (the
//! body's `generated_at`) are fixed. `agent-sandbox-broker write-document` runs this writer on a package's
//! `request.json`, which makes the package's file again.
//!
//! The look is the same for the three kinds: Aggie Blue (#022851) headings and table header rows, a short Aggie Gold
//! (#FFBF00) rule under a title, near-black body text, grey captions and references, Calibri.
use std::{collections::HashMap, fmt::Write as _};

use base64::Engine;
use serde::Deserialize;
use serde_json::Value;
use sha2::{Digest, Sha256};
use time::{OffsetDateTime, format_description::well_known::Rfc3339};

#[path = "document_write_docx.rs"]
mod docx;
#[path = "document_write_png.rs"]
pub(crate) mod png;
#[path = "document_write_pptx.rs"]
mod pptx;
#[path = "document_write_pptx_parts.rs"]
mod pptx_parts;
#[path = "document_write_fit.rs"]
pub(crate) mod slide_fit;
#[path = "document_write_xlsx.rs"]
mod xlsx;
#[path = "document_write_zip.rs"]
mod zip;

/// The writer's own version, raised whenever the bytes it writes for the same body change.
pub(crate) const WRITER_VERSION: u32 = 1;
/// The source revision the release build compiles in (`images/broker.Containerfile`); a local build has none.
const SOURCE_REVISION: Option<&str> = option_env!("ENRICHEDLAB_SOURCE_REVISION");

pub(crate) const KINDS: [&str; 3] = ["xlsx", "docx", "pptx"];
const MAX_BODY_BYTES: usize = 48 * 1024 * 1024;
const MAX_FILE_BYTES: usize = 32 * 1024 * 1024;
const MAX_SECTIONS: usize = 40;
const MAX_PARAGRAPHS: usize = 20;
const HEADING_CHARS: usize = 200;
const TEXT_CHARS: usize = 2000;
const REFERENCE_CHARS: usize = 300;
const MAX_COLUMNS: usize = 64;
const MAX_ROWS: usize = 20_000;
const MAX_PROVENANCE: usize = 64;
const MAX_FIGURE_BYTES: usize = 8 * 1024 * 1024;
const MAX_FIGURES_BYTES: usize = 16 * 1024 * 1024;
/// The pixels a document's distinct figures hold in all, which bounds the decoding their check does.
pub(crate) const MAX_FIGURES_PIXELS: u64 = 100_000_000;

/// The version a receipt records: `writer.<n>`, with `+<revision>` when the build compiled one in.
pub(crate) fn writer_version() -> String {
    match SOURCE_REVISION.filter(|r| r.len() == 40 && r.bytes().all(|b| b.is_ascii_hexdigit())) {
        Some(revision) => format!("writer.{WRITER_VERSION}+{revision}"),
        None => format!("writer.{WRITER_VERSION}"),
    }
}

/// Why a body cannot be written. Each code is a fixed word that quotes nothing the body held.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct WriteError(pub(crate) &'static str);

fn invalid() -> WriteError {
    WriteError("document_request_invalid")
}

fn too_large() -> WriteError {
    WriteError("document_too_large")
}

/// The request body, as `document_compose` writes it.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Request {
    pub(crate) kind: String,
    pub(crate) title: String,
    pub(crate) generated_at: String,
    pub(crate) sections: Vec<Section>,
    pub(crate) provenance: Vec<Entry>,
}

/// One section: a heading with its paragraphs, a table or a figure.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Section {
    pub(crate) heading: String,
    #[serde(default)]
    pub(crate) paragraphs: Vec<String>,
    #[serde(default)]
    pub(crate) table: Option<Table>,
    #[serde(default)]
    pub(crate) figure: Option<Figure>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Table {
    pub(crate) columns: Vec<String>,
    /// Each cell is text, a number, a truth value or empty (null).
    pub(crate) rows: Vec<Vec<Value>>,
    #[serde(default)]
    pub(crate) caption: String,
    #[serde(rename = "ref")]
    pub(crate) reference: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Figure {
    /// The PNG, base64.
    pub(crate) png: String,
    #[serde(default)]
    pub(crate) caption: String,
    #[serde(rename = "ref")]
    pub(crate) reference: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Entry {
    pub(crate) label: String,
    pub(crate) value: String,
}

/// A figure's PNG and its size in pixels, with the number of its media part (identical PNGs share one).
pub(super) struct Image {
    pub(super) media: usize,
    pub(super) width: u32,
    pub(super) height: u32,
}

/// A checked body: the request, each section's figure, the distinct PNGs and the time it was generated.
pub(super) struct Doc<'a> {
    pub(super) request: &'a Request,
    pub(super) figures: Vec<Option<Image>>,
    pub(super) media: Vec<Vec<u8>>,
    pub(super) generated: OffsetDateTime,
}

/// One part of the file, with the content type its `[Content_Types].xml` override names (`None` when a default covers
/// it: relationships and PNGs).
pub(super) struct Part {
    pub(super) name: String,
    pub(super) content_type: Option<&'static str>,
    pub(super) bytes: Vec<u8>,
}

impl Part {
    pub(super) fn new(
        name: impl Into<String>,
        content_type: Option<&'static str>,
        bytes: impl Into<Vec<u8>>,
    ) -> Self {
        Part {
            name: name.into(),
            content_type,
            bytes: bytes.into(),
        }
    }
}

/// The file written and its counts.
pub(crate) struct Written {
    pub(crate) file: Vec<u8>,
    pub(crate) counts: Value,
}

/// Write the document a body describes.
pub(crate) fn write(body: &[u8]) -> Result<Written, WriteError> {
    if body.len() > MAX_BODY_BYTES {
        return Err(too_large());
    }
    let request: Request = serde_json::from_slice(body).map_err(|_| invalid())?;
    let doc = check(&request)?;
    let (main, parts) = match request.kind.as_str() {
        "docx" => (docx::MAIN, docx::parts(&doc)),
        "xlsx" => (xlsx::MAIN, xlsx::parts(&doc)),
        _ => (pptx::MAIN, pptx::parts(&doc)),
    };
    let file = package(&doc, main, parts)?;
    let tables: Vec<&Table> = request
        .sections
        .iter()
        .filter_map(|s| s.table.as_ref())
        .collect();
    let counts = serde_json::json!({
        "sections": request.sections.len(),
        "tables": tables.len(),
        "cells": tables.iter().map(|t| t.rows.len() * t.columns.len()).sum::<usize>(),
        "figures": doc.figures.iter().filter(|f| f.is_some()).count(),
    });
    Ok(Written { file, counts })
}

fn chars_within(text: &str, max: usize) -> bool {
    text.len() <= max || text.chars().count() <= max
}

fn cell_budget(kind: &str) -> usize {
    match kind {
        "xlsx" => 1_000_000,
        "docx" => 20_000,
        _ => 2_400,
    }
}

fn text_budget(kind: &str) -> usize {
    match kind {
        "xlsx" => 32 << 20,
        "docx" => 8 << 20,
        _ => 2 << 20,
    }
}

/// Check the body against the writer's limits (the same ones compose writes within) and read its figures.
fn check(request: &Request) -> Result<Doc<'_>, WriteError> {
    let kind = request.kind.as_str();
    if !KINDS.contains(&kind) {
        return Err(invalid());
    }
    let generated =
        OffsetDateTime::parse(&request.generated_at, &Rfc3339).map_err(|_| invalid())?;
    if generated.offset() != time::UtcOffset::UTC {
        return Err(invalid());
    }
    if request.sections.is_empty()
        || request.sections.len() > MAX_SECTIONS
        || request.provenance.len() > MAX_PROVENANCE
    {
        return Err(invalid());
    }
    let (mut text, mut cells) = (request.title.len(), 0usize);
    if !chars_within(&request.title, HEADING_CHARS) {
        return Err(invalid());
    }
    for entry in &request.provenance {
        if !chars_within(&entry.label, REFERENCE_CHARS)
            || !chars_within(&entry.value, REFERENCE_CHARS)
        {
            return Err(invalid());
        }
        text += entry.label.len() + entry.value.len();
    }
    let (mut figures, mut media, mut by_digest) = (Vec::new(), Vec::new(), HashMap::new());
    let (mut figure_bytes, mut figure_pixels) = (0usize, 0u64);
    for section in &request.sections {
        let blocks = usize::from(!section.paragraphs.is_empty())
            + usize::from(section.table.is_some())
            + usize::from(section.figure.is_some());
        if blocks > 1 {
            return Err(WriteError("slide_content_invalid"));
        }
        if !chars_within(&section.heading, HEADING_CHARS)
            || section.paragraphs.len() > MAX_PARAGRAPHS
        {
            return Err(invalid());
        }
        text += section.heading.len();
        for paragraph in &section.paragraphs {
            if !chars_within(paragraph, TEXT_CHARS) {
                return Err(invalid());
            }
            text += paragraph.len();
        }

        if let Some(table) = &section.table {
            if table.columns.is_empty()
                || table.columns.len() > MAX_COLUMNS
                || table.rows.len() > MAX_ROWS
            {
                return Err(invalid());
            }
            for column in &table.columns {
                if !chars_within(column, HEADING_CHARS) {
                    return Err(invalid());
                }
                text += column.len();
            }
            for row in &table.rows {
                if row.len() != table.columns.len() {
                    return Err(invalid());
                }
                for cell in row {
                    match cell {
                        Value::String(s) if chars_within(s, TEXT_CHARS) => text += s.len(),
                        Value::Number(n) if n.as_f64().is_some_and(f64::is_finite) => text += 24,
                        Value::Null | Value::Bool(_) => {}
                        _ => return Err(invalid()),
                    }
                }
            }
            cells += table.rows.len() * table.columns.len();

            if !chars_within(&table.caption, REFERENCE_CHARS)
                || !chars_within(&table.reference, REFERENCE_CHARS)
            {
                return Err(invalid());
            }
            text += table.caption.len() + table.reference.len();
        }
        figures.push(match &section.figure {
            None => None,
            Some(figure) => {
                if !chars_within(&figure.caption, REFERENCE_CHARS)
                    || !chars_within(&figure.reference, REFERENCE_CHARS)
                {
                    return Err(invalid());
                }
                text += figure.caption.len() + figure.reference.len();
                if figure.png.len() > MAX_FIGURE_BYTES / 3 * 4 + 4 {
                    return Err(WriteError("figure_too_large"));
                }
                let png = base64::engine::general_purpose::STANDARD
                    .decode(&figure.png)
                    .map_err(|_| WriteError("figure_png_invalid"))?;
                if png.len() > MAX_FIGURE_BYTES {
                    return Err(WriteError("figure_too_large"));
                }
                // Identical PNGs share one media part and one check.
                let digest: [u8; 32] = Sha256::digest(&png).into();
                let (number, width, height) = match by_digest.get(&digest) {
                    Some(&seen) => seen,
                    None => {
                        let (width, height) = match png::check(&png) {
                            png::Png::Fits(width, height) => (width, height),
                            png::Png::TooLarge => return Err(WriteError("figure_too_large")),
                            png::Png::Invalid => return Err(WriteError("figure_png_invalid")),
                        };
                        figure_bytes += png.len();
                        figure_pixels += u64::from(width) * u64::from(height);
                        if figure_bytes > MAX_FIGURES_BYTES || figure_pixels > MAX_FIGURES_PIXELS {
                            return Err(too_large());
                        }
                        media.push(png);
                        by_digest.insert(digest, (media.len(), width, height));
                        (media.len(), width, height)
                    }
                };
                Some(Image {
                    media: number,
                    width,
                    height,
                })
            }
        });
    }
    if cells > cell_budget(kind) || text > text_budget(kind) {
        return Err(too_large());
    }
    // Every slide's text must be seen whole: within its box by the writer's estimate, which compose packs by too.
    if kind == "pptx" {
        slide_fit::check(request)?;
    }
    Ok(Doc {
        request,
        figures,
        media,
        generated,
    })
}

/// What compose may place: a figure within the writer's byte limit is checked as `png::check` checks it.
pub(crate) fn figure_check(data: &[u8]) -> png::Png {
    if data.len() > MAX_FIGURE_BYTES {
        png::Png::TooLarge
    } else {
        png::check(data)
    }
}

/// `width` by `height` pixels scaled to fit a box `box_cx` by `box_cy` EMU, aspect kept.
pub(super) fn fit(width: u32, height: u32, box_cx: i64, box_cy: i64) -> (i64, i64) {
    let scale = (box_cx as f64 / f64::from(width)).min(box_cy as f64 / f64::from(height));
    (
        ((f64::from(width) * scale).round() as i64).clamp(1, box_cx),
        ((f64::from(height) * scale).round() as i64).clamp(1, box_cy),
    )
}

/// Column widths that add up to `total`, each in proportion to its column's longest text (the name or a cell) with two
/// characters for the cell margins, counted between 8 and 32, so a short column keeps its name on one line and a long
/// one wraps.
pub(super) fn column_widths(columns: &[String], rows: &[Vec<Value>], total: i64) -> Vec<i64> {
    let weights: Vec<i64> = (0..columns.len())
        .map(|c| {
            let longest = rows
                .iter()
                .map(|row| cell_text(&row[c]).chars().count())
                .chain(std::iter::once(columns[c].chars().count()))
                .max()
                .unwrap_or(0);
            (longest + 2).clamp(8, 32) as i64
        })
        .collect();
    let sum: i64 = weights.iter().sum();
    let mut widths: Vec<i64> = weights.iter().map(|w| total * w / sum).collect();
    let used: i64 = widths.iter().sum();
    if let Some(last) = widths.last_mut() {
        *last += total - used;
    }
    widths
}

/// A table cell as Word and PowerPoint show it.
pub(super) fn cell_text(value: &Value) -> String {
    match value {
        Value::String(s) => s.clone(),
        Value::Number(n) => n.to_string(),
        Value::Bool(true) => "true".to_owned(),
        Value::Bool(false) => "false".to_owned(),
        _ => String::new(),
    }
}

/// The date and time an ISO reader shows: `2026-10-17 18:05 UTC`.
pub(super) fn shown_time(at: OffsetDateTime) -> String {
    format!(
        "{:04}-{:02}-{:02} {:02}:{:02} UTC",
        at.year(),
        u8::from(at.month()),
        at.day(),
        at.hour(),
        at.minute()
    )
}

fn xml_char(c: char) -> bool {
    matches!(c, '\t' | '\n' | '\u{20}'..='\u{d7ff}' | '\u{e000}'..='\u{fffd}' | '\u{10000}'..)
}

/// XML character data. A character XML cannot hold (compose never sends one) becomes U+FFFD; a carriage return becomes
/// a space.
pub(super) fn esc(out: &mut String, text: &str) {
    for c in text.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '\r' => out.push(' '),
            c if xml_char(c) => out.push(c),
            _ => out.push('\u{fffd}'),
        }
    }
}

/// An XML attribute value in double quotes; tabs and line feeds are kept as references.
pub(super) fn esc_attr(out: &mut String, text: &str) {
    for c in text.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\t' => out.push_str("&#9;"),
            '\n' => out.push_str("&#10;"),
            '\r' => out.push(' '),
            c if xml_char(c) => out.push(c),
            _ => out.push('\u{fffd}'),
        }
    }
}

pub(super) const XML_HEAD: &str = "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\n";
pub(super) const RELS_NS: &str = "http://schemas.openxmlformats.org/package/2006/relationships";
pub(super) const REL: &str = "http://schemas.openxmlformats.org/officeDocument/2006/relationships";

/// A relationships part from `(id, type suffix under officeDocument/2006/relationships/, target)`.
pub(super) fn relationships(entries: &[(String, &str, String)]) -> Vec<u8> {
    let mut out = format!("{XML_HEAD}<Relationships xmlns=\"{RELS_NS}\">");
    for (id, kind, target) in entries {
        let _ = write!(
            out,
            "<Relationship Id=\"{id}\" Type=\"{REL}/{kind}\" Target=\""
        );
        esc_attr(&mut out, target);
        out.push_str("\"/>");
    }
    out.push_str("</Relationships>");
    out.into_bytes()
}

const CORE_TYPE: &str = "application/vnd.openxmlformats-package.core-properties+xml";
const APP_TYPE: &str = "application/vnd.openxmlformats-officedocument.extended-properties+xml";

/// The file: `[Content_Types].xml`, the package relationships, the document properties and the kind's parts, in that
/// order, as one ZIP.
fn package(doc: &Doc<'_>, main: &str, parts: Vec<Part>) -> Result<Vec<u8>, WriteError> {
    let stamp = format!(
        "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}Z",
        doc.generated.year(),
        u8::from(doc.generated.month()),
        doc.generated.day(),
        doc.generated.hour(),
        doc.generated.minute(),
        doc.generated.second()
    );
    let mut core = format!(
        "{XML_HEAD}<cp:coreProperties xmlns:cp=\"http://schemas.openxmlformats.org/package/2006/metadata/core-properties\" \
         xmlns:dc=\"http://purl.org/dc/elements/1.1/\" xmlns:dcterms=\"http://purl.org/dc/terms/\" \
         xmlns:xsi=\"http://www.w3.org/2001/XMLSchema-instance\"><dc:title>"
    );
    esc(&mut core, &doc.request.title);
    let _ = write!(
        core,
        "</dc:title><dc:creator>EnrichedLab</dc:creator><cp:lastModifiedBy>EnrichedLab</cp:lastModifiedBy>\
         <cp:revision>1</cp:revision><dcterms:created xsi:type=\"dcterms:W3CDTF\">{stamp}</dcterms:created>\
         <dcterms:modified xsi:type=\"dcterms:W3CDTF\">{stamp}</dcterms:modified></cp:coreProperties>"
    );
    let app = format!(
        "{XML_HEAD}<Properties xmlns=\"http://schemas.openxmlformats.org/officeDocument/2006/extended-properties\">\
         <Application>EnrichedLab</Application></Properties>"
    );
    let rels = format!(
        "{XML_HEAD}<Relationships xmlns=\"{RELS_NS}\">\
         <Relationship Id=\"rId1\" Type=\"{REL}/officeDocument\" Target=\"{main}\"/>\
         <Relationship Id=\"rId2\" Type=\"http://schemas.openxmlformats.org/package/2006/relationships/metadata/core-properties\" Target=\"docProps/core.xml\"/>\
         <Relationship Id=\"rId3\" Type=\"{REL}/extended-properties\" Target=\"docProps/app.xml\"/></Relationships>"
    );
    let mut types = format!(
        "{XML_HEAD}<Types xmlns=\"http://schemas.openxmlformats.org/package/2006/content-types\">\
         <Default Extension=\"rels\" ContentType=\"application/vnd.openxmlformats-package.relationships+xml\"/>\
         <Default Extension=\"xml\" ContentType=\"application/xml\"/>\
         <Default Extension=\"png\" ContentType=\"image/png\"/>\
         <Override PartName=\"/docProps/core.xml\" ContentType=\"{CORE_TYPE}\"/>\
         <Override PartName=\"/docProps/app.xml\" ContentType=\"{APP_TYPE}\"/>"
    );
    for part in &parts {
        if let Some(content_type) = part.content_type {
            let _ = write!(
                types,
                "<Override PartName=\"/{}\" ContentType=\"{content_type}\"/>",
                part.name
            );
        }
    }
    types.push_str("</Types>");
    let mut zip = zip::Zip::new(doc.generated);
    zip.add("[Content_Types].xml", types.as_bytes())?;
    zip.add("_rels/.rels", rels.as_bytes())?;
    zip.add("docProps/app.xml", app.as_bytes())?;
    zip.add("docProps/core.xml", core.as_bytes())?;
    for part in &parts {
        zip.add(&part.name, &part.bytes)?;
    }
    zip.finish()
}

#[cfg(test)]
#[path = "document_write_tests.rs"]
mod tests;
