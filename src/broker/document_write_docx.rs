//! Word: one document on US Letter with one-inch margins. The title (Title style, with the gold rule under it), then each
//! section's heading (Heading 1) and its paragraphs, table or figure, each table and figure followed by its caption and
//! its reference, then a Provenance heading and one `label: value` line for each entry. Tables use the Result Table
//! style: an Aggie Blue header row of white bold text that repeats on each page, light rules and banded rows. A figure
//! fills the text width or seven inches of height, whichever is reached first.
use std::fmt::Write as _;

use super::{Doc, Part, XML_HEAD, cell_text, column_widths, esc, esc_attr, fit, relationships};

pub(super) const MAIN: &str = "word/document.xml";
const MAIN_TYPE: &str =
    "application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml";
const STYLES_TYPE: &str =
    "application/vnd.openxmlformats-officedocument.wordprocessingml.styles+xml";
const SETTINGS_TYPE: &str =
    "application/vnd.openxmlformats-officedocument.wordprocessingml.settings+xml";
/// The text width and the figure height limit, in EMU: 6.5 and 7 inches.
const FIGURE_BOX: (i64, i64) = (5_943_600, 6_400_800);
/// The text width in twentieths of a point.
const TEXT_TWIPS: i64 = 9360;

pub(super) fn parts(doc: &Doc<'_>) -> Vec<Part> {
    let request = doc.request;
    let mut body = String::with_capacity(64 * 1024);
    paragraph(&mut body, "Title", &request.title);
    let mut figures = 0;
    for (section, figure) in request.sections.iter().zip(&doc.figures) {
        paragraph(&mut body, "Heading1", &section.heading);
        for text in &section.paragraphs {
            paragraph(&mut body, "", text);
        }
        if let Some(table) = &section.table {
            body.push_str("<w:tbl><w:tblPr><w:tblStyle w:val=\"ResultTable\"/><w:tblW w:w=\"9360\" w:type=\"dxa\"/>\
                <w:tblLayout w:type=\"fixed\"/><w:tblLook w:val=\"0420\" w:firstRow=\"1\" w:lastRow=\"0\" \
                w:firstColumn=\"0\" w:lastColumn=\"0\" w:noHBand=\"0\" w:noVBand=\"1\"/></w:tblPr><w:tblGrid>");
            let widths = column_widths(&table.columns, &table.rows, TEXT_TWIPS);
            for width in &widths {
                let _ = write!(body, "<w:gridCol w:w=\"{width}\"/>");
            }
            body.push_str("</w:tblGrid><w:tr><w:trPr><w:tblHeader/></w:trPr>");
            for (column, width) in table.columns.iter().zip(&widths) {
                cell(&mut body, *width, column);
            }
            body.push_str("</w:tr>");
            // A data row may break across pages: Word cuts off a row that cannot, once it is taller than a page.
            for row in &table.rows {
                body.push_str("<w:tr>");
                for (value, width) in row.iter().zip(&widths) {
                    cell(&mut body, *width, &cell_text(value));
                }
                body.push_str("</w:tr>");
            }
            body.push_str("</w:tbl>");
            paragraph(&mut body, "Caption", &table.caption);
            paragraph(&mut body, "Reference", &table.reference);
        }
        if let (Some(image), Some(source)) = (figure, &section.figure) {
            let (cx, cy) = fit(image.width, image.height, FIGURE_BOX.0, FIGURE_BOX.1);
            let id = image.media;
            figures += 1;
            let _ = write!(
                body,
                "<w:p><w:pPr><w:pStyle w:val=\"Figure\"/></w:pPr><w:r><w:drawing><wp:inline distT=\"0\" distB=\"0\" \
                 distL=\"0\" distR=\"0\"><wp:extent cx=\"{cx}\" cy=\"{cy}\"/><wp:effectExtent l=\"0\" t=\"0\" r=\"0\" \
                 b=\"0\"/><wp:docPr id=\"{n}\" name=\"Figure {n}\" descr=\"",
                n = figures,
            );
            esc_attr(&mut body, &source.caption);
            let _ = write!(
                body,
                "\"/><wp:cNvGraphicFramePr><a:graphicFrameLocks noChangeAspect=\"1\"/></wp:cNvGraphicFramePr>\
                 <a:graphic><a:graphicData uri=\"http://schemas.openxmlformats.org/drawingml/2006/picture\"><pic:pic>\
                 <pic:nvPicPr><pic:cNvPr id=\"0\" name=\"image{id}.png\"/><pic:cNvPicPr/></pic:nvPicPr><pic:blipFill>\
                 <a:blip r:embed=\"rIdImage{id}\"/><a:stretch><a:fillRect/></a:stretch></pic:blipFill><pic:spPr>\
                 <a:xfrm><a:off x=\"0\" y=\"0\"/><a:ext cx=\"{cx}\" cy=\"{cy}\"/></a:xfrm><a:prstGeom prst=\"rect\">\
                 <a:avLst/></a:prstGeom></pic:spPr></pic:pic></a:graphicData></a:graphic></wp:inline></w:drawing>\
                 </w:r></w:p>"
            );
            paragraph(&mut body, "Caption", &source.caption);
            paragraph(&mut body, "Reference", &source.reference);
        }
    }
    paragraph(&mut body, "Heading1", "Provenance");
    for entry in &request.provenance {
        paragraph(
            &mut body,
            "Provenance",
            &format!("{}: {}", entry.label, entry.value),
        );
    }
    let document = format!(
        "{XML_HEAD}<w:document xmlns:w=\"http://schemas.openxmlformats.org/wordprocessingml/2006/main\" \
         xmlns:r=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships\" \
         xmlns:wp=\"http://schemas.openxmlformats.org/drawingml/2006/wordprocessingDrawing\" \
         xmlns:a=\"http://schemas.openxmlformats.org/drawingml/2006/main\" \
         xmlns:pic=\"http://schemas.openxmlformats.org/drawingml/2006/picture\"><w:body>{body}\
         <w:sectPr><w:pgSz w:w=\"12240\" w:h=\"15840\"/><w:pgMar w:top=\"1440\" w:right=\"1440\" w:bottom=\"1440\" \
         w:left=\"1440\" w:header=\"720\" w:footer=\"720\" w:gutter=\"0\"/></w:sectPr></w:body></w:document>"
    );
    let mut rels = vec![
        ("rIdStyles".to_owned(), "styles", "styles.xml".to_owned()),
        (
            "rIdSettings".to_owned(),
            "settings",
            "settings.xml".to_owned(),
        ),
    ];
    let mut parts = vec![
        Part::new(MAIN, Some(MAIN_TYPE), document),
        Part::new(
            "word/styles.xml",
            Some(STYLES_TYPE),
            format!("{XML_HEAD}{STYLES}"),
        ),
        Part::new(
            "word/settings.xml",
            Some(SETTINGS_TYPE),
            format!("{XML_HEAD}{SETTINGS}"),
        ),
    ];
    for (n, png) in doc.media.iter().enumerate() {
        let n = n + 1;
        rels.push((
            format!("rIdImage{n}"),
            "image",
            format!("media/image{n}.png"),
        ));
        parts.push(Part::new(
            format!("word/media/image{n}.png"),
            None,
            png.clone(),
        ));
    }
    parts.insert(
        1,
        Part::new("word/_rels/document.xml.rels", None, relationships(&rels)),
    );
    parts
}

/// One paragraph in `style` (the default style when empty). Empty text writes nothing for captions and references.
fn paragraph(out: &mut String, style: &str, text: &str) {
    if text.is_empty() && matches!(style, "Caption" | "Reference") {
        return;
    }
    out.push_str("<w:p>");
    if !style.is_empty() {
        let _ = write!(out, "<w:pPr><w:pStyle w:val=\"{style}\"/></w:pPr>");
    }
    runs(out, text);
    out.push_str("</w:p>");
}

/// Text as one run: a line feed is a break and a tab a tab.
fn runs(out: &mut String, text: &str) {
    if text.is_empty() {
        return;
    }
    out.push_str("<w:r>");
    for (i, line) in text.split('\n').enumerate() {
        if i > 0 {
            out.push_str("<w:br/>");
        }
        for (j, piece) in line.split('\t').enumerate() {
            if j > 0 {
                out.push_str("<w:tab/>");
            }
            if !piece.is_empty() {
                out.push_str("<w:t xml:space=\"preserve\">");
                esc(out, piece);
                out.push_str("</w:t>");
            }
        }
    }
    out.push_str("</w:r>");
}

fn cell(out: &mut String, width: i64, text: &str) {
    let _ = write!(
        out,
        "<w:tc><w:tcPr><w:tcW w:w=\"{width}\" w:type=\"dxa\"/></w:tcPr><w:p>"
    );
    runs(out, text);
    out.push_str("</w:p></w:tc>");
}

const SETTINGS: &str = "<w:settings xmlns:w=\"http://schemas.openxmlformats.org/wordprocessingml/2006/main\">\
<w:defaultTabStop w:val=\"720\"/><w:characterSpacingControl w:val=\"doNotCompress\"/><w:compat>\
<w:compatSetting w:name=\"compatibilityMode\" w:uri=\"http://schemas.microsoft.com/office/word\" w:val=\"15\"/>\
</w:compat></w:settings>";

const STYLES: &str = r#"<w:styles xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:docDefaults><w:rPrDefault><w:rPr><w:rFonts w:ascii="Calibri" w:hAnsi="Calibri" w:eastAsia="Calibri" w:cs="Calibri"/><w:color w:val="1B1F24"/><w:sz w:val="22"/><w:szCs w:val="22"/><w:lang w:val="en-US" w:eastAsia="en-US" w:bidi="ar-SA"/></w:rPr></w:rPrDefault><w:pPrDefault><w:pPr><w:spacing w:after="120" w:line="264" w:lineRule="auto"/></w:pPr></w:pPrDefault></w:docDefaults><w:style w:type="paragraph" w:default="1" w:styleId="Normal"><w:name w:val="Normal"/><w:qFormat/></w:style><w:style w:type="character" w:default="1" w:styleId="DefaultParagraphFont"><w:name w:val="Default Paragraph Font"/><w:uiPriority w:val="1"/><w:semiHidden/><w:unhideWhenUsed/></w:style><w:style w:type="table" w:default="1" w:styleId="TableNormal"><w:name w:val="Normal Table"/><w:uiPriority w:val="99"/><w:semiHidden/><w:unhideWhenUsed/><w:tblPr><w:tblInd w:w="0" w:type="dxa"/><w:tblCellMar><w:top w:w="0" w:type="dxa"/><w:left w:w="108" w:type="dxa"/><w:bottom w:w="0" w:type="dxa"/><w:right w:w="108" w:type="dxa"/></w:tblCellMar></w:tblPr></w:style><w:style w:type="paragraph" w:styleId="Title"><w:name w:val="Title"/><w:basedOn w:val="Normal"/><w:next w:val="Normal"/><w:uiPriority w:val="10"/><w:qFormat/><w:pPr><w:pBdr><w:bottom w:val="single" w:sz="18" w:space="8" w:color="FFBF00"/></w:pBdr><w:spacing w:after="360" w:line="240" w:lineRule="auto"/></w:pPr><w:rPr><w:color w:val="022851"/><w:sz w:val="48"/><w:szCs w:val="48"/></w:rPr></w:style><w:style w:type="paragraph" w:styleId="Heading1"><w:name w:val="heading 1"/><w:basedOn w:val="Normal"/><w:next w:val="Normal"/><w:uiPriority w:val="9"/><w:qFormat/><w:pPr><w:keepNext/><w:keepLines/><w:spacing w:before="360" w:after="120"/><w:outlineLvl w:val="0"/></w:pPr><w:rPr><w:b/><w:bCs/><w:color w:val="022851"/><w:sz w:val="30"/><w:szCs w:val="30"/></w:rPr></w:style><w:style w:type="paragraph" w:styleId="Caption"><w:name w:val="caption"/><w:basedOn w:val="Normal"/><w:next w:val="Normal"/><w:uiPriority w:val="35"/><w:qFormat/><w:pPr><w:spacing w:before="80" w:after="40"/></w:pPr><w:rPr><w:i/><w:iCs/><w:color w:val="4F5B66"/><w:sz w:val="18"/><w:szCs w:val="18"/></w:rPr></w:style><w:style w:type="paragraph" w:customStyle="1" w:styleId="Reference"><w:name w:val="Reference"/><w:basedOn w:val="Normal"/><w:next w:val="Normal"/><w:qFormat/><w:pPr><w:spacing w:after="240"/></w:pPr><w:rPr><w:color w:val="4F5B66"/><w:sz w:val="16"/><w:szCs w:val="16"/></w:rPr></w:style><w:style w:type="paragraph" w:customStyle="1" w:styleId="Figure"><w:name w:val="Figure"/><w:basedOn w:val="Normal"/><w:next w:val="Caption"/><w:qFormat/><w:pPr><w:keepNext/><w:spacing w:before="120" w:after="0"/><w:jc w:val="center"/></w:pPr></w:style><w:style w:type="paragraph" w:customStyle="1" w:styleId="Provenance"><w:name w:val="Provenance"/><w:basedOn w:val="Normal"/><w:qFormat/><w:pPr><w:spacing w:after="40"/></w:pPr><w:rPr><w:color w:val="4F5B66"/><w:sz w:val="18"/><w:szCs w:val="18"/></w:rPr></w:style><w:style w:type="table" w:customStyle="1" w:styleId="ResultTable"><w:name w:val="Result Table"/><w:basedOn w:val="TableNormal"/><w:uiPriority w:val="59"/><w:pPr><w:spacing w:before="40" w:after="40" w:line="240" w:lineRule="auto"/></w:pPr><w:rPr><w:sz w:val="18"/><w:szCs w:val="18"/></w:rPr><w:tblPr><w:tblBorders><w:top w:val="single" w:sz="4" w:space="0" w:color="9AADC2"/><w:bottom w:val="single" w:sz="4" w:space="0" w:color="9AADC2"/><w:insideH w:val="single" w:sz="4" w:space="0" w:color="CDD6E0"/></w:tblBorders><w:tblCellMar><w:left w:w="80" w:type="dxa"/><w:right w:w="80" w:type="dxa"/></w:tblCellMar></w:tblPr><w:tblStylePr w:type="firstRow"><w:rPr><w:b/><w:bCs/><w:color w:val="FFFFFF"/></w:rPr><w:tcPr><w:shd w:val="clear" w:color="auto" w:fill="022851"/></w:tcPr></w:tblStylePr><w:tblStylePr w:type="band1Horz"><w:tcPr><w:shd w:val="clear" w:color="auto" w:fill="F2F5F8"/></w:tcPr></w:tblStylePr></w:style></w:styles>"#;
