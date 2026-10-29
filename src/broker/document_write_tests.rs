//! The writer's files: well-formed parts that every relationship and content type resolves, a plain ZIP with one date,
//! the same bytes for the same body, text escaped and kept, typed workbook cells, figures byte for byte, and limits
//! that refuse rather than cut. Office's own reading of the files is checked by hand with `read_back.py` and the Office
//! apps (`tests/fixtures/documents/README.md`).
use std::collections::{BTreeMap, BTreeSet};
use std::io::{Read, Write};

use base64::Engine;
use flate2::{Compression, Crc, read::DeflateDecoder};
use regex::Regex;
use serde_json::{Value, json};

use super::*;

fn fixture(kind: &str) -> Value {
    let path = format!(
        "{}/tests/fixtures/documents/{kind}.request.json",
        env!("CARGO_MANIFEST_DIR")
    );
    serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap()
}

fn written(body: &Value) -> Vec<u8> {
    write(&serde_json::to_vec(body).unwrap())
        .map(|w| w.file)
        .unwrap()
}

fn refused(body: &Value) -> &'static str {
    match write(&serde_json::to_vec(body).unwrap()) {
        Ok(_) => "written",
        Err(e) => e.0,
    }
}

/// The parts of a ZIP in order, read from its central directory, each inflated and its CRC checked.
fn unzip(file: &[u8]) -> Vec<(String, Vec<u8>)> {
    let u16_at = |at: usize| u16::from_le_bytes([file[at], file[at + 1]]) as usize;
    let u32_at = |at: usize| u32::from_le_bytes(file[at..at + 4].try_into().unwrap()) as usize;
    let end = file.len() - 22;
    assert_eq!(
        u32_at(end),
        0x0605_4b50,
        "the end record closes the file, with no comment"
    );
    let (count, mut at) = (u16_at(end + 10), u32_at(end + 16));
    let mut parts = Vec::new();
    for _ in 0..count {
        assert_eq!(u32_at(at), 0x0201_4b50);
        let (crc, stored, size) = (u32_at(at + 16), u32_at(at + 20), u32_at(at + 24));
        let (name_length, extra, comment) = (u16_at(at + 28), u16_at(at + 30), u16_at(at + 32));
        assert_eq!((extra, comment), (0, 0), "no extra field or comment");
        let name = String::from_utf8(file[at + 46..at + 46 + name_length].to_vec()).unwrap();
        let local = u32_at(at + 42);
        assert_eq!(u32_at(local), 0x0403_4b50);
        assert_eq!(
            u16_at(local + 6),
            0,
            "no flags: no data descriptor, no encryption"
        );
        assert_eq!(u16_at(local + 8), 8, "deflated");
        assert_eq!(u16_at(local + 28), 0, "no extra field");
        let data = local + 30 + u16_at(local + 26);
        let mut bytes = Vec::new();
        DeflateDecoder::new(&file[data..data + stored])
            .read_to_end(&mut bytes)
            .unwrap();
        assert_eq!(bytes.len(), size);
        let mut check = Crc::new();
        check.update(&bytes);
        assert_eq!(check.sum() as usize, crc, "{name}");
        parts.push((name, bytes));
        at += 46 + name_length;
    }
    parts
}

static TAG: std::sync::LazyLock<Regex> = std::sync::LazyLock::new(|| {
    Regex::new(r#"<(/?)([A-Za-z_][\w:.-]*)((?:\s+[\w:.-]+="[^"<]*")*)\s*(/?)>|<\?xml [^?]*\?>"#)
        .unwrap()
});
static ATTRIBUTE: std::sync::LazyLock<Regex> =
    std::sync::LazyLock::new(|| Regex::new(r#"([\w:.-]+)="[^"]*""#).unwrap());
static REFERENCE: std::sync::LazyLock<Regex> = std::sync::LazyLock::new(|| {
    Regex::new(r"&(amp|lt|gt|quot|apos|#[0-9]+|#x[0-9A-Fa-f]+);").unwrap()
});

/// Well-formed XML: every `<` opens a tag, tags nest, one root, unique attributes, only known references.
fn well_formed(name: &str, xml: &str) {
    let (mut stack, mut roots, mut last) = (Vec::new(), 0, 0);
    for tag in TAG.captures_iter(xml) {
        let whole = tag.get(0).unwrap();
        let text = &xml[last..whole.start()];
        assert!(
            !text.contains('<') && !text.contains('>'),
            "{name}: stray markup"
        );
        assert_eq!(
            text.matches('&').count(),
            REFERENCE.find_iter(text).count(),
            "{name}: bare &"
        );
        assert!(
            !stack.is_empty() || text.trim().is_empty(),
            "{name}: text outside the root"
        );
        last = whole.end();
        let Some(element) = tag.get(2) else { continue };
        let attributes: Vec<&str> = ATTRIBUTE
            .captures_iter(&tag[3])
            .map(|a| a.get(1).unwrap().as_str())
            .collect();
        assert_eq!(
            attributes.len(),
            attributes.iter().collect::<BTreeSet<_>>().len(),
            "{name}: repeated attribute"
        );
        match (&tag[1], &tag[4]) {
            ("/", _) => assert_eq!(stack.pop(), Some(element.as_str()), "{name}: crossed tags"),
            (_, "/") => roots += usize::from(stack.is_empty()),
            _ => {
                roots += usize::from(stack.is_empty());
                stack.push(element.as_str());
            }
        }
    }
    assert!(
        xml[last..].trim().is_empty() && stack.is_empty() && roots == 1,
        "{name}: unclosed or several roots"
    );
}

/// Check one written file the way a reader resolves it: parts, content types and relationships.
fn check_package(kind: &str, file: &[u8]) -> BTreeMap<String, Vec<u8>> {
    let parts = unzip(file);
    assert_eq!(parts[0].0, "[Content_Types].xml");
    let parts: BTreeMap<String, Vec<u8>> = parts.into_iter().collect();
    for (name, bytes) in &parts {
        if name.ends_with(".xml") || name.ends_with(".rels") {
            well_formed(name, std::str::from_utf8(bytes).unwrap());
        }
    }
    let types = std::str::from_utf8(&parts["[Content_Types].xml"]).unwrap();
    let overrides: BTreeSet<String> = Regex::new(r#"PartName="/([^"]+)""#)
        .unwrap()
        .captures_iter(types)
        .map(|c| c[1].to_owned())
        .collect();
    for name in parts.keys() {
        let typed = overrides.contains(name) || name.ends_with(".rels") || name.ends_with(".png");
        assert!(
            typed || name == "[Content_Types].xml",
            "{kind}: {name} has no content type"
        );
    }
    for name in &overrides {
        assert!(
            parts.contains_key(name),
            "{kind}: override names a missing part {name}"
        );
    }
    let target = Regex::new(r#"Target="([^"]+)""#).unwrap();
    for (name, bytes) in parts.iter().filter(|(n, _)| n.ends_with(".rels")) {
        let folder = name
            .trim_end_matches(".rels")
            .rsplit_once("_rels/")
            .map(|(f, _)| f)
            .unwrap_or("");
        for found in target.captures_iter(std::str::from_utf8(bytes).unwrap()) {
            let mut path: Vec<&str> = folder.split('/').filter(|p| !p.is_empty()).collect();
            for piece in found[1].split('/') {
                if piece == ".." {
                    path.pop();
                } else {
                    path.push(piece);
                }
            }
            assert!(
                parts.contains_key(&path.join("/")),
                "{kind}: {name} points at a missing {}",
                &found[1]
            );
        }
    }
    parts
}

fn text(parts: &BTreeMap<String, Vec<u8>>, name: &str) -> String {
    String::from_utf8(parts[name].clone()).unwrap()
}

#[test]
fn each_kind_writes_the_same_well_formed_package_for_the_same_body() {
    for kind in KINDS {
        let body = fixture(kind);
        let file = written(&body);
        assert_eq!(
            file,
            written(&body),
            "{kind}: the same body gives the same bytes"
        );
        let parts = check_package(kind, &file);
        let core = text(&parts, "docProps/core.xml");
        assert!(core.contains(
            "<dcterms:created xsi:type=\"dcterms:W3CDTF\">2026-10-17T09:30:00Z</dcterms:created>"
        ));
        assert!(
            parts
                .keys()
                .all(|n| !n.contains("vbaProject") && !n.contains("externalLink"))
        );
        for (name, bytes) in &parts {
            let xml = String::from_utf8_lossy(bytes);
            assert!(
                !xml.contains("TargetMode=\"External\""),
                "{kind}: {name} links outside the file"
            );
            assert!(
                !xml.contains("<f>") && !xml.contains("<w:fldSimple") && !xml.contains("<a:fld"),
                "{name}"
            );
        }
    }
}

#[test]
fn the_zip_dates_are_the_body_time_in_dos_form() {
    let file = written(&fixture("docx"));
    let time = u16::from_le_bytes([file[10], file[11]]);
    let date = u16::from_le_bytes([file[12], file[13]]);
    // 2026-10-17 09:30:00.
    assert_eq!(time, (9 << 11) | (30 << 5));
    assert_eq!(date, ((2026 - 1980) << 9) | (10 << 5) | 17);
}

fn small(kind: &str) -> Value {
    json!({
        "kind": kind, "title": "A&B <C> \"D\" 'E'", "generated_at": "2026-10-17T09:30:00Z",
        "sections": [
            {"heading": "Text", "paragraphs": ["Tab\there.\nLine two & <three>."]},
            {"heading": "Cells", "table": {"columns": ["n", "flag", "text"], "rows": [
                [1.5, true, "=1+1"], [3, false, "_x0041_ stays"], [null, null, ""]],
                "caption": "Caption \"quoted\"", "ref": "results/t.csv"}},
        ],
        "provenance": [{"label": "Project", "value": "P & Q"}],
    })
}

#[test]
fn text_is_escaped_and_kept_whole() {
    let parts = check_package("docx", &written(&small("docx")));
    let document = text(&parts, "word/document.xml");
    assert!(document.contains("A&amp;B &lt;C&gt; \"D\" 'E'"));
    assert!(document.contains("Tab</w:t><w:tab/><w:t xml:space=\"preserve\">here.</w:t><w:br/>"));
    assert!(document.contains("_x0041_ stays") && document.contains("Project: P &amp; Q"));
    // Rows may break across pages, so Word never cuts off a row taller than a page; the header row repeats.
    assert!(!document.contains("cantSplit") && document.contains("<w:tblHeader/>"));
    let parts = check_package("pptx", &written(&small("pptx")));
    assert!(text(&parts, "ppt/slides/slide1.xml").contains("A&amp;B &lt;C&gt;"));
    assert!(text(&parts, "ppt/slides/slide2.xml").contains("Line two &amp; &lt;three&gt;."));
    assert_eq!(
        text(&parts, "docProps/core.xml")
            .matches("A&amp;B &lt;C&gt;")
            .count(),
        1
    );
}

#[test]
fn workbook_cells_are_typed_and_text_never_a_formula() {
    let parts = check_package("xlsx", &written(&small("xlsx")));
    let sheet = text(&parts, "xl/worksheets/sheet2.xml");
    assert!(
        sheet.contains("<c r=\"A2\"><v>1.5</v></c><c r=\"B2\" t=\"b\"><v>1</v></c>"),
        "{sheet}"
    );
    assert!(sheet.contains(
        "<c r=\"C2\" s=\"2\" t=\"inlineStr\"><is><t xml:space=\"preserve\">=1+1</t></is></c>"
    ));
    assert!(sheet.contains("<c r=\"A3\"><v>3</v></c><c r=\"B3\" t=\"b\"><v>0</v></c>"));
    // Excel reads `_xHHHH_` as an escaped character, so a literal one is written with its underscore escaped.
    assert!(sheet.contains("_x005F_x0041_ stays"));
    assert!(
        sheet.contains("<row r=\"4\"></row>"),
        "an empty row writes no cell"
    );
    assert!(
        sheet.contains("<pane ySplit=\"1\" topLeftCell=\"A2\""),
        "the header row is frozen"
    );
    let workbook = text(&parts, "xl/workbook.xml");
    assert!(
        workbook.contains("<sheet name=\"Text\"")
            && workbook.contains("<sheet name=\"Provenance\"")
    );
}

fn png(width: u32, height: u32, extra: &[(&[u8; 4], &[u8])]) -> Vec<u8> {
    let chunk = |out: &mut Vec<u8>, kind: &[u8; 4], body: &[u8]| {
        out.extend_from_slice(&(body.len() as u32).to_be_bytes());
        out.extend_from_slice(kind);
        out.extend_from_slice(body);
        let mut crc = Crc::new();
        crc.update(kind);
        crc.update(body);
        out.extend_from_slice(&crc.sum().to_be_bytes());
    };
    let mut out = b"\x89PNG\r\n\x1a\n".to_vec();
    let mut header = width.to_be_bytes().to_vec();
    header.extend_from_slice(&height.to_be_bytes());
    header.extend_from_slice(&[8, 2, 0, 0, 0]);
    chunk(&mut out, b"IHDR", &header);
    for (kind, body) in extra {
        chunk(&mut out, kind, body);
    }
    let raw = vec![0u8; (height * (width * 3 + 1)) as usize];
    let mut encoder = flate2::write::ZlibEncoder::new(Vec::new(), Compression::default());
    encoder.write_all(&raw).unwrap();
    chunk(&mut out, b"IDAT", &encoder.finish().unwrap());
    chunk(&mut out, b"IEND", b"");
    out
}

fn with_figures(kind: &str, pngs: &[Vec<u8>]) -> Value {
    let sections: Vec<Value> = pngs
        .iter()
        .enumerate()
        .map(|(n, p)| json!({"heading": format!("Figure {}", n + 1), "figure": {
            "png": base64::engine::general_purpose::STANDARD.encode(p), "caption": "A \"figure\"", "ref": "plot.png"}}))
        .collect();
    json!({"kind": kind, "title": "Figures", "generated_at": "2026-10-17T09:30:00Z", "sections": sections,
        "provenance": []})
}

#[test]
fn figures_are_the_sent_png_byte_for_byte_and_shared_when_identical() {
    let (a, b) = (
        png(4, 3, &[]),
        png(2, 2, &[(b"pHYs", &[0, 0, 23, 18, 0, 0, 23, 18, 1])]),
    );
    for kind in KINDS {
        let parts = check_package(
            kind,
            &written(&with_figures(kind, &[a.clone(), b.clone(), a.clone()])),
        );
        let media: Vec<&Vec<u8>> = parts
            .iter()
            .filter(|(n, _)| n.ends_with(".png"))
            .map(|(_, bytes)| bytes)
            .collect();
        assert_eq!(
            media,
            vec![&a, &b],
            "{kind}: one media part for each distinct PNG, its bytes unchanged"
        );
    }
    let parts = check_package(
        "docx",
        &written(&with_figures("docx", std::slice::from_ref(&a))),
    );
    let document = text(&parts, "word/document.xml");
    // 4 by 3 pixels fill the 6.5-inch text width.
    assert!(
        document.contains("<wp:extent cx=\"5943600\" cy=\"4457700\"/>"),
        "{document}"
    );
    assert!(document.contains("descr=\"A &quot;figure&quot;\""));
}

#[test]
fn a_png_that_is_not_one_still_image_is_refused() {
    // `document_write_png_tests.rs` covers the check; here the writer's refusals for what it finds.
    let good = png(4, 3, &[]);
    assert_eq!(figure_check(&good), png::Png::Fits(4, 3));
    let mut bad_crc = good.clone();
    let at = bad_crc.len() - 20;
    bad_crc[at] ^= 1;
    assert_eq!(figure_check(&bad_crc), png::Png::Invalid);
    assert_eq!(
        refused(&with_figures("docx", std::slice::from_ref(&bad_crc))),
        "figure_png_invalid"
    );
    let oversized = vec![0u8; MAX_FIGURE_BYTES + 1];
    assert_eq!(figure_check(&oversized), png::Png::TooLarge);
}

#[test]
fn slide_content_the_estimate_does_not_fit_is_refused_and_rows_take_their_height() {
    let mut body = small("pptx");
    body["sections"][1]["table"]["rows"][0][2] = json!("line\n".repeat(60));
    assert_eq!(refused(&body), "table_too_large_for_slide");
    let mut body = small("pptx");
    body["sections"][0]["paragraphs"] = json!(["x\n".repeat(40)]);
    assert_eq!(refused(&body), "slide_content_invalid");
    let mut body = small("pptx");
    body["sections"][1]["table"]["rows"][0][2] = json!("line\nline\nline");
    let parts = check_package("pptx", &written(&body));
    let slide = text(&parts, "ppt/slides/slide3.xml");
    let heights: Vec<i64> = Regex::new(r#"<a:tr h="([0-9]+)">"#)
        .unwrap()
        .captures_iter(&slide)
        .map(|c| c[1].parse().unwrap())
        .collect();
    assert_eq!(heights.len(), 4, "the header and three rows");
    assert!(
        heights[1] > heights[2],
        "a three-line row is taller: {heights:?}"
    );
}

#[test]
fn provenance_continues_on_further_slides_with_every_entry_once() {
    let mut body = small("pptx");
    body["provenance"] = json!(
        (0..40)
            .map(|n| json!({"label": format!("Note {n}"), "value": "v".repeat(290)}))
            .collect::<Vec<_>>()
    );
    let parts = check_package("pptx", &written(&body));
    let slides = parts
        .keys()
        .filter(|n| n.starts_with("ppt/slides/slide") && n.ends_with(".xml"))
        .count();
    assert!(
        slides > 4,
        "the title, two sections and more than one provenance slide: {slides}"
    );
    let last = text(&parts, &format!("ppt/slides/slide{slides}.xml"));
    assert!(last.contains(">Provenance (") && last.contains(&format!(">{slides} / {slides}<")));
    let all: String = (1..=slides)
        .map(|n| text(&parts, &format!("ppt/slides/slide{n}.xml")))
        .collect();
    for n in 0..40 {
        assert_eq!(all.matches(&format!(">Note {n}<")).count(), 1, "entry {n}");
    }
}

#[test]
fn limits_refuse_rather_than_cut() {
    let mut body = small("pptx");
    body["sections"][1]["table"]["rows"] = json!(vec![json!([1, true, "x"]); 16]);
    assert_eq!(refused(&body), "table_too_large_for_slide");
    let mut body = small("docx");
    body["sections"][0]["table"] = body["sections"][1]["table"].clone();
    assert_eq!(refused(&body), "slide_content_invalid");
    let mut body = small("docx");
    body["sections"] = json!(vec![json!({"heading": "x"}); 41]);
    assert_eq!(refused(&body), "document_request_invalid");
    for (field, value) in [
        ("generated_at", json!("2026-10-17T09:30:00+02:00")),
        ("kind", json!("odt")),
        ("other", json!(1)),
    ] {
        let mut body = small("docx");
        body[field] = value;
        assert_eq!(refused(&body), "document_request_invalid", "{field}");
    }
    let mut body = small("xlsx");
    body["sections"][1]["table"]["rows"][0] = json!([1, 2]);
    assert_eq!(
        refused(&body),
        "document_request_invalid",
        "a row as wide as the columns"
    );
    body["sections"][1]["table"]["rows"][0] = json!([{"a": 1}, 2, 3]);
    assert_eq!(
        refused(&body),
        "document_request_invalid",
        "a cell is text, a number, a truth value or empty"
    );
}

#[test]
fn sheet_names_are_valid_and_unique() {
    let mut used = vec!["provenance".to_owned(), "history".to_owned()];
    let names: Vec<String> = [
        "A/B",
        "a_b",
        "'quoted'",
        "Provenance",
        &"long ".repeat(10),
        "[]",
        "\u{1f600}".repeat(20).as_str(),
    ]
    .iter()
    .map(|h| sheet_name_for_test(h, &mut used))
    .collect();
    assert_eq!(names[0], "A_B");
    assert_eq!(names[1], "a_b (2)");
    assert_eq!(names[2], "quoted");
    assert_eq!(names[3], "Provenance (2)");
    assert_eq!(names[4].encode_utf16().count(), 31);
    assert_eq!(names[5], "__");
    assert!(names[6].encode_utf16().count() <= 31 && !names[6].is_empty());
}

fn sheet_name_for_test(heading: &str, used: &mut Vec<String>) -> String {
    xlsx::sheet_name(heading, used)
}

#[test]
fn column_widths_add_up_and_keep_a_short_column_wide_enough_for_its_name() {
    let table = Table {
        columns: vec!["Result".into(), "Rows".into(), "Table SHA-256".into()],
        rows: vec![vec![
            json!("artifact://tasks/".repeat(5)),
            json!(14),
            json!("c".repeat(64)),
        ]],
        caption: String::new(),
        reference: String::new(),
    };
    // Weights 32, 8 and 32: the long columns capped, the short one (4 characters and 2 for margins) raised to its floor.
    assert_eq!(
        column_widths(&table.columns, &table.rows, 9360),
        vec![4160, 1040, 4160]
    );
}

#[test]
fn the_receipt_version_names_the_writer() {
    let version = writer_version();
    assert!(version.starts_with("writer.1"), "{version}");
    assert!(
        version == "writer.1"
            || Regex::new(r"^writer\.1\+[0-9a-f]{40}$")
                .unwrap()
                .is_match(&version)
    );
}
