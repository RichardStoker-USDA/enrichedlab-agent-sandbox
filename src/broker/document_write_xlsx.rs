//! Excel: one sheet for each section, then a Provenance sheet. A table's sheet starts with its header row (white bold
//! on Aggie Blue, frozen) and its rows from A1, so the data can be sorted and filtered as it stands; its caption and
//! reference follow one row below the last, then the document's title and the section's heading. Any other section's
//! sheet holds the title, the heading and its paragraphs, or the title, the heading, the figure's caption and reference
//! and the figure below them. Numbers and truth values are typed cells; text is stored inline, never as a formula.
use std::fmt::Write as _;

use serde_json::Value;

use super::{Doc, Part, XML_HEAD, esc, esc_attr, fit, relationships};

pub(super) const MAIN: &str = "xl/workbook.xml";
const MAIN_TYPE: &str =
    "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet.main+xml";
const SHEET_TYPE: &str =
    "application/vnd.openxmlformats-officedocument.spreadsheetml.worksheet+xml";
const STYLES_TYPE: &str = "application/vnd.openxmlformats-officedocument.spreadsheetml.styles+xml";
const DRAWING_TYPE: &str = "application/vnd.openxmlformats-officedocument.drawing+xml";
/// The figure's box: 9 by 6 inches, in EMU.
const FIGURE_BOX: (i64, i64) = (8_229_600, 5_486_400);

/// Cell styles, as `styles.xml` numbers them.
const HEADER: u8 = 1;
const TEXT: u8 = 2;
const NOTE: u8 = 3;
const TITLE: u8 = 4;
const HEADING: u8 = 5;

pub(super) fn parts(doc: &Doc<'_>) -> Vec<Part> {
    let request = doc.request;
    let mut used = vec!["provenance".to_owned(), "history".to_owned()];
    let mut sheets: Vec<(String, String, Option<usize>)> = Vec::new();
    let mut drawings: Vec<(usize, String)> = Vec::new();
    for (section, figure) in request.sections.iter().zip(&doc.figures) {
        let name = sheet_name(&section.heading, &mut used);
        let mut rows = String::new();
        let selected = sheets.is_empty();
        let (columns, frozen) = if let Some(table) = &section.table {
            rows.push_str("<row r=\"1\">");
            for (c, column) in table.columns.iter().enumerate() {
                text_cell(&mut rows, c, 0, HEADER, column);
            }
            rows.push_str("</row>");
            for (r, values) in table.rows.iter().enumerate() {
                let _ = write!(rows, "<row r=\"{}\">", r + 2);
                for (c, value) in values.iter().enumerate() {
                    value_cell(&mut rows, c, r + 1, value);
                }
                rows.push_str("</row>");
            }
            let mut row = table.rows.len() + 2;
            for (style, text) in [
                (NOTE, &table.caption),
                (NOTE, &table.reference),
                (TITLE, &request.title),
                (HEADING, &section.heading),
            ] {
                text_row(&mut rows, row, style, text);
                row += 1;
            }
            (
                format!(
                    "<col min=\"1\" max=\"{}\" width=\"24\" customWidth=\"1\"/>",
                    table.columns.len().max(2)
                ),
                true,
            )
        } else {
            text_row(&mut rows, 0, TITLE, &request.title);
            text_row(&mut rows, 1, HEADING, &section.heading);
            let mut row = 2;
            for text in &section.paragraphs {
                text_row(&mut rows, row, TEXT, text);
                row += 1;
            }
            if let (Some(image), Some(source)) = (figure, &section.figure) {
                text_row(&mut rows, row, NOTE, &source.caption);
                text_row(&mut rows, row + 1, NOTE, &source.reference);
                let (cx, cy) = fit(image.width, image.height, FIGURE_BOX.0, FIGURE_BOX.1);
                let number = drawings.len() + 1;
                let mut drawing = format!(
                    "{XML_HEAD}<xdr:wsDr xmlns:xdr=\"http://schemas.openxmlformats.org/drawingml/2006/spreadsheetDrawing\" \
                     xmlns:a=\"http://schemas.openxmlformats.org/drawingml/2006/main\" \
                     xmlns:r=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships\"><xdr:oneCellAnchor>\
                     <xdr:from><xdr:col>0</xdr:col><xdr:colOff>0</xdr:colOff><xdr:row>{}</xdr:row><xdr:rowOff>0</xdr:rowOff>\
                     </xdr:from><xdr:ext cx=\"{cx}\" cy=\"{cy}\"/><xdr:pic><xdr:nvPicPr><xdr:cNvPr id=\"2\" name=\"Figure {number}\" descr=\"",
                    row + 3
                );
                esc_attr(&mut drawing, &source.caption);
                let _ = write!(
                    drawing,
                    "\"/><xdr:cNvPicPr><a:picLocks noChangeAspect=\"1\"/></xdr:cNvPicPr></xdr:nvPicPr><xdr:blipFill>\
                     <a:blip r:embed=\"rIdImage\"/><a:stretch><a:fillRect/></a:stretch></xdr:blipFill><xdr:spPr>\
                     <a:xfrm><a:off x=\"0\" y=\"0\"/><a:ext cx=\"{cx}\" cy=\"{cy}\"/></a:xfrm><a:prstGeom prst=\"rect\">\
                     <a:avLst/></a:prstGeom></xdr:spPr></xdr:pic><xdr:clientData/></xdr:oneCellAnchor></xdr:wsDr>"
                );
                drawings.push((image.media, drawing));
            }
            (
                "<col min=\"1\" max=\"1\" width=\"110\" customWidth=\"1\"/>".to_owned(),
                false,
            )
        };
        let drawing = section.figure.as_ref().map(|_| drawings.len());
        sheets.push((
            name,
            sheet(selected, frozen, &columns, &rows, drawing.is_some()),
            drawing,
        ));
    }
    let mut rows = String::from("<row r=\"1\">");
    text_cell(&mut rows, 0, 0, HEADER, "Label");
    text_cell(&mut rows, 1, 0, HEADER, "Value");
    rows.push_str("</row>");
    for (r, entry) in request.provenance.iter().enumerate() {
        let _ = write!(rows, "<row r=\"{}\">", r + 2);
        text_cell(&mut rows, 0, r + 1, TEXT, &entry.label);
        text_cell(&mut rows, 1, r + 1, TEXT, &entry.value);
        rows.push_str("</row>");
    }
    let columns = "<col min=\"1\" max=\"1\" width=\"32\" customWidth=\"1\"/><col min=\"2\" max=\"2\" width=\"110\" customWidth=\"1\"/>";
    sheets.push((
        "Provenance".to_owned(),
        sheet(false, true, columns, &rows, false),
        None,
    ));

    let mut workbook = format!(
        "{XML_HEAD}<workbook xmlns=\"http://schemas.openxmlformats.org/spreadsheetml/2006/main\" \
         xmlns:r=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships\"><bookViews>\
         <workbookView xWindow=\"0\" yWindow=\"0\" windowWidth=\"28800\" windowHeight=\"16000\"/></bookViews><sheets>"
    );
    let mut rels = Vec::new();
    let mut parts = Vec::new();
    for (n, (name, xml, drawing)) in sheets.into_iter().enumerate() {
        let n = n + 1;
        workbook.push_str("<sheet name=\"");
        esc_attr(&mut workbook, &name);
        let _ = write!(workbook, "\" sheetId=\"{n}\" r:id=\"rIdSheet{n}\"/>");
        rels.push((
            format!("rIdSheet{n}"),
            "worksheet",
            format!("worksheets/sheet{n}.xml"),
        ));
        parts.push(Part::new(
            format!("xl/worksheets/sheet{n}.xml"),
            Some(SHEET_TYPE),
            xml,
        ));
        if let Some(d) = drawing {
            parts.push(Part::new(
                format!("xl/worksheets/_rels/sheet{n}.xml.rels"),
                None,
                relationships(&[(
                    "rIdDrawing".to_owned(),
                    "drawing",
                    format!("../drawings/drawing{d}.xml"),
                )]),
            ));
        }
    }
    workbook.push_str("</sheets><calcPr calcId=\"191029\"/></workbook>");
    rels.push(("rIdStyles".to_owned(), "styles", "styles.xml".to_owned()));
    for (d, (media, drawing)) in drawings.into_iter().enumerate() {
        let d = d + 1;
        parts.push(Part::new(
            format!("xl/drawings/drawing{d}.xml"),
            Some(DRAWING_TYPE),
            drawing,
        ));
        parts.push(Part::new(
            format!("xl/drawings/_rels/drawing{d}.xml.rels"),
            None,
            relationships(&[(
                "rIdImage".to_owned(),
                "image",
                format!("../media/image{media}.png"),
            )]),
        ));
    }
    for (m, png) in doc.media.iter().enumerate() {
        parts.push(Part::new(
            format!("xl/media/image{}.png", m + 1),
            None,
            png.clone(),
        ));
    }
    let mut all = vec![
        Part::new(MAIN, Some(MAIN_TYPE), workbook),
        Part::new("xl/_rels/workbook.xml.rels", None, relationships(&rels)),
        Part::new(
            "xl/styles.xml",
            Some(STYLES_TYPE),
            format!("{XML_HEAD}{STYLES}"),
        ),
    ];
    all.extend(parts);
    all
}

fn sheet(selected: bool, frozen: bool, columns: &str, rows: &str, drawing: bool) -> String {
    let tab = if selected { " tabSelected=\"1\"" } else { "" };
    let view = if frozen {
        format!(
            "<sheetView workbookViewId=\"0\"{tab}><pane ySplit=\"1\" topLeftCell=\"A2\" activePane=\"bottomLeft\" \
             state=\"frozen\"/><selection pane=\"bottomLeft\" activeCell=\"A2\" sqref=\"A2\"/></sheetView>"
        )
    } else {
        format!("<sheetView workbookViewId=\"0\"{tab}/>")
    };
    format!(
        "{XML_HEAD}<worksheet xmlns=\"http://schemas.openxmlformats.org/spreadsheetml/2006/main\" \
         xmlns:r=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships\"><sheetViews>{view}</sheetViews>\
         <sheetFormatPr defaultRowHeight=\"15\"/><cols>{columns}</cols><sheetData>{rows}</sheetData>\
         <pageMargins left=\"0.7\" right=\"0.7\" top=\"0.75\" bottom=\"0.75\" header=\"0.3\" footer=\"0.3\"/>{}</worksheet>",
        if drawing {
            "<drawing r:id=\"rIdDrawing\"/>"
        } else {
            ""
        }
    )
}

/// A sheet name from a heading as Excel allows one: no `[]:*?/\`, no apostrophe at either end, at most 31 UTF-16 code
/// units, and unique without regard to case (`(2)`, `(3)` and on are added).
pub(super) fn sheet_name(heading: &str, used: &mut Vec<String>) -> String {
    let replaced: String = heading
        .chars()
        .map(|c| if "[]:*?/\\".contains(c) { '_' } else { c })
        .collect();
    let base = utf16_prefix(replaced.trim_matches('\''), 31)
        .trim_end_matches('\'')
        .to_owned();
    let base = if base.trim().is_empty() {
        "Section".to_owned()
    } else {
        base
    };
    let (mut name, mut number) = (base.clone(), 2);
    while used.contains(&name.to_lowercase()) {
        let suffix = format!(" ({number})");
        name = format!(
            "{}{suffix}",
            utf16_prefix(&base, 31 - suffix.len()).trim_end_matches('\'')
        );
        number += 1;
    }
    used.push(name.to_lowercase());
    name
}

fn utf16_prefix(text: &str, units: usize) -> &str {
    let mut used = 0;
    for (at, c) in text.char_indices() {
        used += c.len_utf16();
        if used > units {
            return &text[..at];
        }
    }
    text
}

/// A column's letters from its index (0 is A).
fn column(mut index: usize) -> String {
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

fn text_row(out: &mut String, row: usize, style: u8, text: &str) {
    if text.is_empty() {
        return;
    }
    let _ = write!(out, "<row r=\"{}\">", row + 1);
    text_cell(out, 0, row, style, text);
    out.push_str("</row>");
}

fn text_cell(out: &mut String, col: usize, row: usize, style: u8, text: &str) {
    if text.is_empty() {
        return;
    }
    let _ = write!(
        out,
        "<c r=\"{}{}\" s=\"{style}\" t=\"inlineStr\"><is><t xml:space=\"preserve\">",
        column(col),
        row + 1
    );
    sheet_text(out, text);
    out.push_str("</t></is></c>");
}

fn value_cell(out: &mut String, col: usize, row: usize, value: &Value) {
    match value {
        Value::String(text) => text_cell(out, col, row, TEXT, text),
        Value::Number(n) => {
            let _ = write!(out, "<c r=\"{}{}\"><v>{n}</v></c>", column(col), row + 1);
        }
        Value::Bool(b) => {
            let _ = write!(
                out,
                "<c r=\"{}{}\" t=\"b\"><v>{}</v></c>",
                column(col),
                row + 1,
                u8::from(*b)
            );
        }
        _ => {}
    }
}

/// Sheet text: XML character data in which a literal `_xHHHH_` is written `_x005F_xHHHH_`, since Excel reads the
/// first form as an escaped character.
fn sheet_text(out: &mut String, text: &str) {
    let bytes = text.as_bytes();
    let mut start = 0;
    for (at, _) in text.match_indices("_x") {
        let escape = bytes
            .get(at + 2..at + 7)
            .is_some_and(|tail| tail[..4].iter().all(u8::is_ascii_hexdigit) && tail[4] == b'_');
        if escape {
            esc(out, &text[start..at]);
            out.push_str("_x005F_");
            start = at + 1;
        }
    }
    esc(out, &text[start..]);
}

const STYLES: &str = r#"<styleSheet xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main"><fonts count="5"><font><sz val="11"/><color rgb="FF1B1F24"/><name val="Calibri"/><family val="2"/></font><font><b/><sz val="11"/><color rgb="FFFFFFFF"/><name val="Calibri"/><family val="2"/></font><font><i/><sz val="10"/><color rgb="FF4F5B66"/><name val="Calibri"/><family val="2"/></font><font><b/><sz val="14"/><color rgb="FF022851"/><name val="Calibri"/><family val="2"/></font><font><b/><sz val="12"/><color rgb="FF022851"/><name val="Calibri"/><family val="2"/></font></fonts><fills count="3"><fill><patternFill patternType="none"/></fill><fill><patternFill patternType="gray125"/></fill><fill><patternFill patternType="solid"><fgColor rgb="FF022851"/><bgColor indexed="64"/></patternFill></fill></fills><borders count="1"><border><left/><right/><top/><bottom/><diagonal/></border></borders><cellStyleXfs count="1"><xf numFmtId="0" fontId="0" fillId="0" borderId="0"/></cellStyleXfs><cellXfs count="6"><xf numFmtId="0" fontId="0" fillId="0" borderId="0" xfId="0"/><xf numFmtId="0" fontId="1" fillId="2" borderId="0" xfId="0" applyFont="1" applyFill="1" applyAlignment="1"><alignment vertical="top" wrapText="1"/></xf><xf numFmtId="0" fontId="0" fillId="0" borderId="0" xfId="0" applyAlignment="1"><alignment vertical="top" wrapText="1"/></xf><xf numFmtId="0" fontId="2" fillId="0" borderId="0" xfId="0" applyFont="1" applyAlignment="1"><alignment vertical="top"/></xf><xf numFmtId="0" fontId="3" fillId="0" borderId="0" xfId="0" applyFont="1"/><xf numFmtId="0" fontId="4" fillId="0" borderId="0" xfId="0" applyFont="1"/></cellXfs><cellStyles count="1"><cellStyle name="Normal" xfId="0" builtinId="0"/></cellStyles><dxfs count="0"/><tableStyles count="0" defaultTableStyle="TableStyleMedium2" defaultPivotStyle="PivotStyleLight16"/></styleSheet>"#;
