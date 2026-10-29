//! PowerPoint: 16:9 slides. The first slide holds the title over the gold rule and the time the document was generated;
//! each section has a slide with its heading as the title (Aggie Blue, the gold rule under it) and its table, figure or
//! paragraphs, a table's or figure's caption and reference below it; the last slides hold the provenance entries in two
//! columns, on as many slides as they need. Every slide but the first carries the document's title and its number in a
//! grey footer. Tables have an Aggie Blue header row of white bold text and light banded rows, each row as tall as the
//! writer's estimate of its tallest cell. The boxes, type sizes and that estimate are `document_write_fit.rs`'s.
use std::fmt::Write as _;

use super::pptx_parts::{
    SLIDE_CLOSE, TABLE_STYLES, THEME, layout, master, presentation, presentation_properties,
    slide_open, view_properties,
};
use super::slide_fit::{
    self, BODY, CAPTION, CAPTION_SIZE, CELL_MARGIN, COLUMN_GAP, FIRST_RULE, FIRST_TITLE, FOOTER,
    FOOTER_SIZE, GAP, HEADING_SIZE, LEFT, NUMBER_WIDTH, PROVENANCE_SIZE, REFERENCE_SIZE, RULE,
    SUBTITLE, SUBTITLE_SIZE, TABLE_SIZE, TEXT, TEXT_SIZE, TITLE, TITLE_SIZE, WIDTH,
};
use super::{
    Doc, Part, Table, XML_HEAD, cell_text, column_widths, esc, esc_attr, fit, relationships,
    shown_time,
};

pub(super) const MAIN: &str = "ppt/presentation.xml";
const MAIN_TYPE: &str =
    "application/vnd.openxmlformats-officedocument.presentationml.presentation.main+xml";
const SLIDE_TYPE: &str = "application/vnd.openxmlformats-officedocument.presentationml.slide+xml";
const MASTER_TYPE: &str =
    "application/vnd.openxmlformats-officedocument.presentationml.slideMaster+xml";
const LAYOUT_TYPE: &str =
    "application/vnd.openxmlformats-officedocument.presentationml.slideLayout+xml";
const THEME_TYPE: &str = "application/vnd.openxmlformats-officedocument.theme+xml";
const PRESENTATION_PROPERTIES_TYPE: &str =
    "application/vnd.openxmlformats-officedocument.presentationml.presProps+xml";
const VIEW_PROPERTIES_TYPE: &str =
    "application/vnd.openxmlformats-officedocument.presentationml.viewProps+xml";
const TABLE_STYLES_TYPE: &str =
    "application/vnd.openxmlformats-officedocument.presentationml.tableStyles+xml";

const BLUE: &str = "022851";
const GOLD: &str = "FFBF00";
const INK: &str = "1B1F24";
const GREY: &str = "4F5B66";
const RULE_GREY: &str = "CDD6E0";
const BAND: &str = "F2F5F8";

/// A run of text in one paragraph.
struct Run<'a> {
    text: &'a str,
    size: u32,
    bold: bool,
    color: &'static str,
}

/// A paragraph: its runs, its alignment and whether it sits in a table cell (no spacing after it).
struct Paragraph<'a> {
    runs: Vec<Run<'a>>,
    right: bool,
    tight: bool,
}

fn plain<'a>(text: &'a str, size: u32, bold: bool, color: &'static str) -> Paragraph<'a> {
    Paragraph {
        runs: vec![Run {
            text,
            size,
            bold,
            color,
        }],
        right: false,
        tight: false,
    }
}

/// One slide being written: its XML and the next shape id.
struct Slide {
    xml: String,
    next: u32,
}

impl Slide {
    fn new() -> Self {
        Slide {
            xml: slide_open(),
            next: 2,
        }
    }

    fn id(&mut self) -> u32 {
        self.next += 1;
        self.next - 1
    }

    fn text_box(
        &mut self,
        name: &str,
        (x, y, cx, cy): (i64, i64, i64, i64),
        anchor: &str,
        paragraphs: &[Paragraph<'_>],
    ) {
        let id = self.id();
        let out = &mut self.xml;
        let _ = write!(
            out,
            "<p:sp><p:nvSpPr><p:cNvPr id=\"{id}\" name=\"{name} {id}\"/><p:cNvSpPr txBox=\"1\"/><p:nvPr/></p:nvSpPr>\
             <p:spPr><a:xfrm><a:off x=\"{x}\" y=\"{y}\"/><a:ext cx=\"{cx}\" cy=\"{cy}\"/></a:xfrm><a:prstGeom prst=\"rect\">\
             <a:avLst/></a:prstGeom><a:noFill/></p:spPr><p:txBody><a:bodyPr wrap=\"square\" lIns=\"0\" tIns=\"0\" \
             rIns=\"0\" bIns=\"0\" rtlCol=\"0\" anchor=\"{anchor}\"><a:normAutofit/></a:bodyPr><a:lstStyle/>"
        );
        for paragraph in paragraphs {
            write_paragraph(out, paragraph);
        }
        out.push_str("</p:txBody></p:sp>");
    }

    fn rule(&mut self, x: i64, y: i64) {
        let id = self.id();
        let _ = write!(
            self.xml,
            "<p:sp><p:nvSpPr><p:cNvPr id=\"{id}\" name=\"Rule {id}\"/><p:cNvSpPr/><p:nvPr/></p:nvSpPr><p:spPr><a:xfrm>\
             <a:off x=\"{x}\" y=\"{y}\"/><a:ext cx=\"{}\" cy=\"{}\"/></a:xfrm><a:prstGeom prst=\"rect\"><a:avLst/>\
             </a:prstGeom><a:solidFill><a:srgbClr val=\"{GOLD}\"/></a:solidFill><a:ln><a:noFill/></a:ln></p:spPr></p:sp>",
            RULE.1, RULE.2
        );
    }

    fn heading(&mut self, text: &str) {
        self.text_box(
            "Title",
            (LEFT, TITLE.0, WIDTH, TITLE.1),
            "b",
            &[plain(text, HEADING_SIZE, true, BLUE)],
        );
        self.rule(LEFT, RULE.0);
    }

    fn footer(&mut self, title: &str, number: usize, total: usize) {
        let shown = slide_fit::footer_title(title);
        self.text_box(
            "Footer",
            (LEFT, FOOTER.0, WIDTH - NUMBER_WIDTH - GAP, FOOTER.1),
            "ctr",
            &[plain(&shown, FOOTER_SIZE, false, GREY)],
        );
        let count = format!("{number} / {total}");
        let mut right = plain(&count, FOOTER_SIZE, false, GREY);
        right.right = true;
        self.text_box(
            "Slide number",
            (
                LEFT + WIDTH - NUMBER_WIDTH,
                FOOTER.0,
                NUMBER_WIDTH,
                FOOTER.1,
            ),
            "ctr",
            &[right],
        );
    }

    fn notes(&mut self, caption: &str, reference: &str) {
        let mut paragraphs = Vec::new();
        if !caption.is_empty() {
            paragraphs.push(plain(caption, CAPTION_SIZE, false, GREY));
        }
        if !reference.is_empty() {
            paragraphs.push(plain(reference, REFERENCE_SIZE, false, GREY));
        }
        if !paragraphs.is_empty() {
            self.text_box(
                "Caption",
                (LEFT, CAPTION.0, WIDTH, CAPTION.1),
                "t",
                &paragraphs,
            );
        }
    }

    fn finish(mut self) -> String {
        self.xml.push_str(SLIDE_CLOSE);
        format!("{XML_HEAD}{}", self.xml)
    }
}

fn write_paragraph(out: &mut String, paragraph: &Paragraph<'_>) {
    out.push_str("<a:p>");
    if paragraph.right {
        out.push_str("<a:pPr algn=\"r\"/>");
    } else if paragraph.tight {
        out.push_str("<a:pPr/>");
    } else {
        // Space after a paragraph of three fifths of its size: 12 points after 20-point text.
        let size = paragraph.runs.first().map_or(1800, |r| r.size);
        let _ = write!(
            out,
            "<a:pPr><a:spcAft><a:spcPts val=\"{}\"/></a:spcAft></a:pPr>",
            size * 3 / 5
        );
    }
    let mut size = 1800;
    for run in &paragraph.runs {
        size = run.size;
        for (i, line) in run.text.split('\n').enumerate() {
            if i > 0 {
                let _ = write!(
                    out,
                    "<a:br><a:rPr lang=\"en-US\" sz=\"{}\" dirty=\"0\"/></a:br>",
                    run.size
                );
            }
            if line.is_empty() {
                continue;
            }
            let _ = write!(
                out,
                "<a:r><a:rPr lang=\"en-US\" sz=\"{}\"{} dirty=\"0\"><a:solidFill><a:srgbClr val=\"{}\"/></a:solidFill>\
                 </a:rPr><a:t>",
                run.size,
                if run.bold { " b=\"1\"" } else { "" },
                run.color
            );
            esc(out, line);
            out.push_str("</a:t></a:r>");
        }
    }
    let _ = write!(
        out,
        "<a:endParaRPr lang=\"en-US\" sz=\"{size}\" dirty=\"0\"/></a:p>"
    );
}

pub(super) fn parts(doc: &Doc<'_>) -> Vec<Part> {
    let request = doc.request;
    let provenance = slide_fit::provenance_slides(&request.provenance);
    let total = request.sections.len() + 1 + provenance.len();
    let mut slides: Vec<(String, Option<usize>)> = Vec::with_capacity(total);

    let mut first = Slide::new();
    first.text_box(
        "Title",
        (LEFT, FIRST_TITLE.0, WIDTH, FIRST_TITLE.1),
        "b",
        &[plain(&request.title, TITLE_SIZE, true, BLUE)],
    );
    first.rule(LEFT, FIRST_RULE);
    let subtitle = format!(
        "Answer document \u{b7} generated {}",
        shown_time(doc.generated)
    );
    first.text_box(
        "Subtitle",
        (LEFT, SUBTITLE.0, WIDTH, SUBTITLE.1),
        "t",
        &[plain(&subtitle, SUBTITLE_SIZE, false, GREY)],
    );
    slides.push((first.finish(), None));

    for (n, (section, figure)) in request.sections.iter().zip(&doc.figures).enumerate() {
        let mut slide = Slide::new();
        slide.heading(&section.heading);
        let mut media = None;
        if let Some(table) = &section.table {
            write_table(&mut slide, table);
            slide.notes(&table.caption, &table.reference);
        } else if let (Some(image), Some(source)) = (figure, &section.figure) {
            let (cx, cy) = fit(image.width, image.height, WIDTH, BODY.1);
            let (x, id) = (LEFT + (WIDTH - cx) / 2, slide.id());
            let _ = write!(
                slide.xml,
                "<p:pic><p:nvPicPr><p:cNvPr id=\"{id}\" name=\"Figure {id}\" descr=\""
            );
            esc_attr(&mut slide.xml, &source.caption);
            let _ = write!(
                slide.xml,
                "\"/><p:cNvPicPr><a:picLocks noChangeAspect=\"1\"/></p:cNvPicPr><p:nvPr/></p:nvPicPr><p:blipFill>\
                 <a:blip r:embed=\"rIdImage\"/><a:stretch><a:fillRect/></a:stretch></p:blipFill><p:spPr><a:xfrm>\
                 <a:off x=\"{x}\" y=\"{}\"/><a:ext cx=\"{cx}\" cy=\"{cy}\"/></a:xfrm><a:prstGeom prst=\"rect\"><a:avLst/>\
                 </a:prstGeom></p:spPr></p:pic>",
                BODY.0
            );
            slide.notes(&source.caption, &source.reference);
            media = Some(image.media);
        } else if !section.paragraphs.is_empty() {
            let paragraphs: Vec<Paragraph<'_>> = section
                .paragraphs
                .iter()
                .map(|text| plain(text, TEXT_SIZE, false, INK))
                .collect();
            slide.text_box("Text", (LEFT, TEXT.0, WIDTH, TEXT.1), "t", &paragraphs);
        }
        slide.footer(&request.title, n + 2, total);
        slides.push((slide.finish(), media));
    }

    let column = slide_fit::provenance_column_width();
    for (n, columns) in provenance.iter().enumerate() {
        let mut slide = Slide::new();
        slide.heading(&if n == 0 {
            "Provenance".to_owned()
        } else {
            format!("Provenance ({})", n + 1)
        });
        for (c, entries) in columns.iter().enumerate() {
            let paragraphs: Vec<Paragraph<'_>> = entries
                .iter()
                .map(|e| Paragraph {
                    runs: vec![
                        Run {
                            text: &e.label,
                            size: PROVENANCE_SIZE,
                            bold: true,
                            color: INK,
                        },
                        Run {
                            text: ": ",
                            size: PROVENANCE_SIZE,
                            bold: true,
                            color: INK,
                        },
                        Run {
                            text: &e.value,
                            size: PROVENANCE_SIZE,
                            bold: false,
                            color: INK,
                        },
                    ],
                    right: false,
                    tight: false,
                })
                .collect();
            slide.text_box(
                "Provenance",
                (
                    LEFT + c as i64 * (column + COLUMN_GAP),
                    TEXT.0,
                    column,
                    TEXT.1,
                ),
                "t",
                &paragraphs,
            );
        }
        slide.footer(&request.title, request.sections.len() + 2 + n, total);
        slides.push((slide.finish(), None));
    }

    let mut rels = vec![
        (
            "rIdMaster".to_owned(),
            "slideMaster",
            "slideMasters/slideMaster1.xml".to_owned(),
        ),
        (
            "rIdPresProps".to_owned(),
            "presProps",
            "presProps.xml".to_owned(),
        ),
        (
            "rIdViewProps".to_owned(),
            "viewProps",
            "viewProps.xml".to_owned(),
        ),
        (
            "rIdTheme".to_owned(),
            "theme",
            "theme/theme1.xml".to_owned(),
        ),
        (
            "rIdTableStyles".to_owned(),
            "tableStyles",
            "tableStyles.xml".to_owned(),
        ),
    ];
    let mut parts = Vec::new();
    for (n, (xml, media)) in slides.into_iter().enumerate() {
        let n = n + 1;
        rels.push((
            format!("rIdSlide{n}"),
            "slide",
            format!("slides/slide{n}.xml"),
        ));
        let mut slide_rels = vec![(
            "rIdLayout".to_owned(),
            "slideLayout",
            "../slideLayouts/slideLayout1.xml".to_owned(),
        )];
        if let Some(m) = media {
            slide_rels.push((
                "rIdImage".to_owned(),
                "image",
                format!("../media/image{m}.png"),
            ));
        }
        parts.push(Part::new(
            format!("ppt/slides/slide{n}.xml"),
            Some(SLIDE_TYPE),
            xml,
        ));
        parts.push(Part::new(
            format!("ppt/slides/_rels/slide{n}.xml.rels"),
            None,
            relationships(&slide_rels),
        ));
    }
    let mut all = vec![
        Part::new(
            MAIN,
            Some(MAIN_TYPE),
            format!("{XML_HEAD}{}", presentation(total)),
        ),
        Part::new(
            "ppt/_rels/presentation.xml.rels",
            None,
            relationships(&rels),
        ),
        Part::new(
            "ppt/presProps.xml",
            Some(PRESENTATION_PROPERTIES_TYPE),
            format!("{XML_HEAD}{}", presentation_properties()),
        ),
        Part::new(
            "ppt/viewProps.xml",
            Some(VIEW_PROPERTIES_TYPE),
            format!("{XML_HEAD}{}", view_properties()),
        ),
        Part::new(
            "ppt/tableStyles.xml",
            Some(TABLE_STYLES_TYPE),
            format!("{XML_HEAD}{TABLE_STYLES}"),
        ),
        Part::new(
            "ppt/theme/theme1.xml",
            Some(THEME_TYPE),
            format!("{XML_HEAD}{THEME}"),
        ),
        Part::new(
            "ppt/slideMasters/slideMaster1.xml",
            Some(MASTER_TYPE),
            format!("{XML_HEAD}{}", master()),
        ),
        Part::new(
            "ppt/slideMasters/_rels/slideMaster1.xml.rels",
            None,
            relationships(&[
                (
                    "rIdLayout".to_owned(),
                    "slideLayout",
                    "../slideLayouts/slideLayout1.xml".to_owned(),
                ),
                (
                    "rIdTheme".to_owned(),
                    "theme",
                    "../theme/theme1.xml".to_owned(),
                ),
            ]),
        ),
        Part::new(
            "ppt/slideLayouts/slideLayout1.xml",
            Some(LAYOUT_TYPE),
            format!("{XML_HEAD}{}", layout()),
        ),
        Part::new(
            "ppt/slideLayouts/_rels/slideLayout1.xml.rels",
            None,
            relationships(&[(
                "rIdMaster".to_owned(),
                "slideMaster",
                "../slideMasters/slideMaster1.xml".to_owned(),
            )]),
        ),
    ];
    all.extend(parts);
    for (m, png) in doc.media.iter().enumerate() {
        all.push(Part::new(
            format!("ppt/media/image{}.png", m + 1),
            None,
            png.clone(),
        ));
    }
    all
}

fn write_table(slide: &mut Slide, table: &Table) {
    let (columns, rows) = (&table.columns, &table.rows);
    let widths = column_widths(columns, rows, WIDTH);
    let heights = slide_fit::row_heights(columns, rows, &widths);
    let (mx, my) = CELL_MARGIN;
    let id = slide.id();
    let out = &mut slide.xml;
    let _ = write!(
        out,
        "<p:graphicFrame><p:nvGraphicFramePr><p:cNvPr id=\"{id}\" name=\"Table {id}\"/><p:cNvGraphicFramePr>\
         <a:graphicFrameLocks noGrp=\"1\"/></p:cNvGraphicFramePr><p:nvPr/></p:nvGraphicFramePr><p:xfrm>\
         <a:off x=\"{LEFT}\" y=\"{}\"/><a:ext cx=\"{}\" cy=\"{}\"/></p:xfrm><a:graphic>\
         <a:graphicData uri=\"http://schemas.openxmlformats.org/drawingml/2006/table\"><a:tbl>\
         <a:tblPr firstRow=\"1\" bandRow=\"1\"/><a:tblGrid>",
        BODY.0,
        WIDTH,
        heights.iter().sum::<i64>()
    );
    for width in &widths {
        let _ = write!(out, "<a:gridCol w=\"{width}\"/>");
    }
    out.push_str("</a:tblGrid>");
    let header: Vec<String> = columns.clone();
    for (r, (cells, height)) in std::iter::once(header)
        .chain(rows.iter().map(|row| row.iter().map(cell_text).collect()))
        .zip(&heights)
        .enumerate()
    {
        let _ = write!(out, "<a:tr h=\"{height}\">");
        let (fill, color, bold) = match r {
            0 => (BLUE, "FFFFFF", true),
            r if r % 2 == 0 => (BAND, INK, false),
            _ => ("FFFFFF", INK, false),
        };
        for text in &cells {
            out.push_str("<a:tc><a:txBody><a:bodyPr/><a:lstStyle/>");
            let cell = Paragraph {
                runs: vec![Run {
                    text,
                    size: TABLE_SIZE,
                    bold,
                    color,
                }],
                right: false,
                tight: true,
            };
            write_paragraph(out, &cell);
            let _ = write!(
                out,
                "</a:txBody><a:tcPr marL=\"{mx}\" marR=\"{mx}\" marT=\"{my}\" marB=\"{my}\"><a:lnB w=\"6350\" \
                 cap=\"flat\" cmpd=\"sng\" algn=\"ctr\"><a:solidFill><a:srgbClr val=\"{RULE_GREY}\"/></a:solidFill>\
                 <a:prstDash val=\"solid\"/></a:lnB><a:solidFill><a:srgbClr val=\"{fill}\"/></a:solidFill></a:tcPr></a:tc>"
            );
        }
        out.push_str("</a:tr>");
    }
    out.push_str("</a:tbl></a:graphicData></a:graphic></p:graphicFrame>");
}
