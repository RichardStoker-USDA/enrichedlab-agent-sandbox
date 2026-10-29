use super::*;
use crate::fixtures::{self, catalog_mut, document, page_id, resources_mut, to_bytes};
use lopdf::dictionary;

fn rules() -> Rules {
    Rules::load(&fixtures::rules_dir()).unwrap()
}

fn reread(doc: &mut Document) -> Document {
    Document::load_mem(&to_bytes(doc)).unwrap()
}

#[test]
fn counts_and_strips_active_content() {
    let mut doc = fixtures::active_document();
    let before = active_content(&doc);
    assert!(
        before.javascript >= 2
            && before.launch == 1
            && before.embedded_file >= 3
            && before.open_action == 1
            && before.additional_actions == 1
            && before.xfa == 1,
        "{before:?}"
    );
    rebuild_objects(&mut doc).unwrap();
    let reread = reread(&mut doc);
    assert_eq!(active_content(&reread), Active::default());
    assert_eq!(reread.get_pages().len(), 1, "the page survives");
    let page = reread.get_dictionary(page_id(&reread)).unwrap();
    assert_eq!(
        page.get(b"Annots").unwrap().as_array().unwrap().len(),
        1,
        "the link stays without its action"
    );
}

#[test]
fn a_go_to_open_action_is_kept() {
    let mut doc = document("BT ET");
    let page = page_id(&doc);
    catalog_mut(&mut doc).set("OpenAction", vec![page.into(), "Fit".into()]);
    assert_eq!(active_content(&doc), Active::default());
    strip(&mut doc);
    assert!(doc.catalog().unwrap().has(b"OpenAction"));
}

/// A page drawing an image with a soft mask in an ICC color space, with an embedded font, page metadata, a
/// private stream, a script stream and an attached file.
fn mixed_document() -> Document {
    let mut doc = document("q 10 0 0 10 72 600 cm /Im1 Do Q BT /F1 12 Tf 72 700 Td (Mixed) Tj ET");
    let icc = doc.add_object(Stream::new(dictionary! {"N" => 3}, b"profile".to_vec()));
    let mask = doc.add_object(Stream::new(
        dictionary! {"Type" => "XObject", "Subtype" => "Image", "Width" => 1, "Height" => 1,
        "ColorSpace" => "DeviceGray", "BitsPerComponent" => 8},
        vec![255],
    ));
    let image = doc.add_object(Stream::new(
        dictionary! {"Type" => "XObject", "Subtype" => "Image", "Width" => 1, "Height" => 1,
        "ColorSpace" => vec![Object::Name(b"ICCBased".to_vec()), icc.into()],
        "BitsPerComponent" => 8, "SMask" => mask},
        vec![1, 2, 3],
    ));
    let font_file = doc.add_object(Stream::new(dictionary! {"Length1" => 4}, b"glyf".to_vec()));
    let descriptor = doc.add_object(
            dictionary! {"Type" => "FontDescriptor", "FontName" => "Embedded", "FontFile2" => font_file},
        );
    let font = doc.add_object(dictionary! {"Type" => "Font", "Subtype" => "TrueType",
    "BaseFont" => "Embedded", "FontDescriptor" => descriptor});
    let resources = resources_mut(&mut doc);
    resources.set("XObject", dictionary! {"Im1" => image});
    resources.set("Font", dictionary! {"F1" => font});
    let xmp = doc.add_object(Stream::new(
        dictionary! {"Type" => "Metadata", "Subtype" => "XML"},
        b"<x:xmpmeta/>".to_vec(),
    ));
    let private = doc.add_object(Stream::new(dictionary! {}, b"private".to_vec()));
    let script = doc.add_object(Stream::new(dictionary! {}, b"app.alert(1)".to_vec()));
    let action = doc.add_object(dictionary! {"S" => "JavaScript", "JS" => script});
    let attached = doc.add_object(Stream::new(dictionary! {}, b"attached".to_vec()));
    let spec =
        doc.add_object(dictionary! {"Type" => "Filespec", "EF" => dictionary! {"F" => attached}});
    let catalog = catalog_mut(&mut doc);
    catalog.set("Metadata", xmp);
    catalog.set(
        "PieceInfo",
        dictionary! {"App" => dictionary! {"Private" => private}},
    );
    catalog.set("OpenAction", action);
    catalog.set("AF", vec![spec.into()]);
    doc
}

/// Stream uses, without the cross-reference stream the writer may add.
fn role_counts(doc: &Document) -> BTreeMap<Role, usize> {
    let mut counts = BTreeMap::new();
    for role in roles(doc).into_values().filter(|r| *r != Role::Structure) {
        *counts.entry(role).or_insert(0) += 1;
    }
    counts
}

#[test]
fn streams_are_sorted_by_use_and_unused_ones_removed() {
    let mut doc = mixed_document();
    assert_eq!(
        role_counts(&doc),
        BTreeMap::from([
            (Role::Content, 1),
            (Role::Font, 1),
            (Role::Image, 2),
            (Role::Data, 1),
            (Role::Metadata, 1),
            (Role::Script, 1),
            (Role::Embedded, 1),
            (Role::Unknown, 1),
        ])
    );
    let stripped = rebuild_objects(&mut doc).unwrap();
    assert_eq!(
        stripped.removed_streams, 4,
        "script, attached file, private data, document metadata"
    );
    let rebuilt = reread(&mut doc);
    assert_eq!(
        role_counts(&rebuilt),
        BTreeMap::from([
            (Role::Content, 1),
            (Role::Font, 1),
            (Role::Image, 2),
            (Role::Data, 1),
        ])
    );
    assert_eq!(active_content(&rebuilt), Active::default());
}

#[test]
fn a_compressed_program_is_found_in_any_stream() {
    let rules = rules();
    let private = report(&fixtures::private_program_document(), &rules);
    assert_eq!(private.streams.unknown, 1);
    assert_eq!(
        private.matches,
        vec![StreamMatch {
            rule: "embedded_pe_program".into(),
            severity: Severity::High,
            role: Role::Unknown,
            count: 1
        }]
    );
    let image = report(&fixtures::image_program_document(), &rules);
    assert_eq!(image.streams.unknown, 0);
    assert_eq!(image.matches.len(), 1);
    assert_eq!(
        (image.matches[0].rule.as_str(), image.matches[0].role),
        ("embedded_pe_program", Role::Image)
    );
    let mut doc = fixtures::private_program_document();
    rebuild_objects(&mut doc).unwrap();
    let rebuilt = report(&reread(&mut doc), &rules);
    assert_eq!((rebuilt.streams.unknown, rebuilt.matches.len()), (0, 0));
}

#[test]
fn a_payload_use_outranks_a_drawing_use_of_the_same_stream() {
    // The document's metadata stream is also a script's body, and one stream is both an image and a font.
    let mut doc = document("q 10 0 0 10 72 600 cm /Im1 Do Q BT /F1 12 Tf 72 700 Td (Mixed) Tj ET");
    let script = doc.add_object(Stream::new(
        dictionary! {"Type" => "Metadata", "Subtype" => "XML"},
        b"var p = unescape('%u9090'); eval(p);".to_vec(),
    ));
    let action = doc.add_object(dictionary! {"S" => "JavaScript", "JS" => script});
    let shared = doc.add_object(Stream::new(
        dictionary! {"Type" => "XObject", "Subtype" => "Image", "Width" => 1, "Height" => 1,
        "ColorSpace" => "DeviceGray", "BitsPerComponent" => 8},
        vec![0],
    ));
    let descriptor = doc.add_object(
        dictionary! {"Type" => "FontDescriptor", "FontName" => "Shared", "FontFile2" => shared},
    );
    let font = doc.add_object(dictionary! {"Type" => "Font", "Subtype" => "TrueType",
    "BaseFont" => "Shared", "FontDescriptor" => descriptor});
    let resources = resources_mut(&mut doc);
    resources.set("XObject", dictionary! {"Im1" => shared});
    resources.set("Font", dictionary! {"F1" => font});
    let catalog = catalog_mut(&mut doc);
    catalog.set("Metadata", script);
    catalog.set("OpenAction", action);
    let found = roles(&doc);
    assert_eq!(found[&script], Role::Script);
    assert_eq!(found[&shared], Role::Unknown, "two drawing uses at once");
    let report = report(&doc, &rules());
    assert!(
        report
            .matches
            .iter()
            .any(|m| m.rule == "pdf_obfuscated_javascript" && m.role == Role::Script),
        "{:?}",
        report.matches
    );
    let stripped = rebuild_objects(&mut doc).unwrap();
    assert_eq!(stripped.removed_streams, 2);
    let rebuilt = reread(&mut doc);
    assert!(
        roles(&rebuilt)
            .values()
            .all(|role| role.kept() || *role == Role::Structure),
        "only drawn streams and the writer's cross-reference stream remain"
    );
}

#[test]
fn a_resource_no_drawn_content_names_is_removed() {
    let mut doc = document("q 10 0 0 10 72 600 cm /Im1 Do Q BT /F1 12 Tf 72 700 Td (Figure) Tj ET");
    let image = |doc: &mut Document, value: u8| {
        doc.add_object(Stream::new(
            dictionary! {"Type" => "XObject", "Subtype" => "Image", "Width" => 1, "Height" => 1,
            "ColorSpace" => "DeviceGray", "BitsPerComponent" => 8},
            vec![value],
        ))
    };
    let drawn = image(&mut doc, 1);
    let unused = image(&mut doc, 2);
    // A form no content draws names the unused image; that does not make it drawn.
    let form = doc.add_object(Stream::new(
            dictionary! {"Type" => "XObject", "Subtype" => "Form", "BBox" => vec![0.into(), 0.into(), 1.into(), 1.into()]},
            b"/Im2 Do".to_vec(),
        ));
    resources_mut(&mut doc).set(
        "XObject",
        dictionary! {"Im1" => drawn, "Im2" => unused, "Fx" => form},
    );
    let found = roles(&doc);
    assert_eq!(
        (found[&drawn], found[&unused], found[&form]),
        (Role::Image, Role::Unknown, Role::Unknown)
    );
    rebuild_objects(&mut doc).unwrap();
    let rebuilt = reread(&mut doc);
    assert_eq!(
        roles(&rebuilt)
            .values()
            .filter(|r| **r == Role::Image)
            .count(),
        1
    );
}

#[test]
fn an_xfa_stream_also_named_as_metadata_is_a_script() {
    let mut doc = document("BT /F1 12 Tf 72 700 Td (Form) Tj ET");
    let xfa = doc.add_object(Stream::new(
        dictionary! {"Type" => "Metadata", "Subtype" => "XML"},
        b"<xdp:xdp><script>app.alert(1)</script></xdp:xdp>".to_vec(),
    ));
    let catalog = catalog_mut(&mut doc);
    catalog.set("Metadata", xfa);
    catalog.set(
            "AcroForm",
            dictionary! {"Fields" => Vec::<Object>::new(), "XFA" => vec![Object::string_literal("template"), xfa.into()]},
        );
    assert_eq!(roles(&doc)[&xfa], Role::Script);
    rebuild_objects(&mut doc).unwrap();
    assert!(matches!(doc.objects.get(&xfa), None | Some(Object::Null)));
}

#[test]
fn retained_images_are_bounded_by_their_dictionary_and_codec_header() {
    let doc = document("BT ET");
    let image = |width: i64, filter: Option<&str>, content: Vec<u8>| {
        let mut dict = dictionary! {"Type" => "XObject", "Subtype" => "Image", "Width" => width,
        "Height" => 10, "ColorSpace" => "DeviceRGB", "BitsPerComponent" => 8};
        if let Some(filter) = filter {
            dict.set("Filter", filter);
        }
        Stream::new(dict, content)
    };
    let small = image(10, None, vec![0; 300]);
    assert_eq!(
        image_size(&doc, &small, &small.content),
        ImageSize::Within(300)
    );
    let wide = image(40_000, None, vec![]);
    assert_eq!(image_size(&doc, &wide, &wide.content), ImageSize::TooLarge);
    // A JPEG whose own frame header is 30,000 by 30,000 behind a 10 by 10 dictionary.
    let mut jpeg = vec![0xFF, 0xD8, 0xFF, 0xC0, 0x00, 0x11, 0x08];
    jpeg.extend_from_slice(&30_000u16.to_be_bytes());
    jpeg.extend_from_slice(&30_000u16.to_be_bytes());
    jpeg.extend_from_slice(&[3, 1, 0x22, 0, 2, 0x11, 1, 3, 0x11, 1]);
    jpeg.extend_from_slice(&[
        0xFF, 0xDA, 0x00, 0x0C, 3, 1, 0, 2, 0x11, 3, 0x11, 0, 0x3F, 0,
    ]);
    let bomb = image(10, Some("DCTDecode"), jpeg.clone());
    assert_eq!(image_size(&doc, &bomb, &bomb.content), ImageSize::TooLarge);
    jpeg[7..9].copy_from_slice(&10u16.to_be_bytes());
    jpeg[9..11].copy_from_slice(&10u16.to_be_bytes());
    let fine = image(10, Some("DCTDecode"), jpeg.clone());
    assert_eq!(
        image_size(&doc, &fine, &fine.content),
        ImageSize::Within(300)
    );
    // Twelve-bit samples take two bytes: 12,000 by 12,000 in three components is 432 MB at eight bits and
    // 864 MB at twelve.
    jpeg[6] = 12;
    jpeg[7..9].copy_from_slice(&12_000u16.to_be_bytes());
    jpeg[9..11].copy_from_slice(&12_000u16.to_be_bytes());
    let deep = image(10, Some("DCTDecode"), jpeg.clone());
    assert_eq!(image_size(&doc, &deep, &deep.content), ImageSize::TooLarge);
    jpeg[6] = 8;
    let shallow = image(10, Some("DCTDecode"), jpeg);
    assert!(matches!(
        image_size(&doc, &shallow, &shallow.content),
        ImageSize::Within(_)
    ));
    let garbage = image(10, Some("DCTDecode"), b"not a jpeg".to_vec());
    assert_eq!(
        image_size(&doc, &garbage, &garbage.content),
        ImageSize::Unreadable
    );
    let mut siz = vec![0xFF, 0x4F, 0xFF, 0x51, 0, 41, 0, 0];
    for value in [50_000u32, 10, 0, 0, 50_000, 10, 0, 0] {
        siz.extend_from_slice(&value.to_be_bytes());
    }
    siz.extend_from_slice(&3u16.to_be_bytes());
    siz.extend_from_slice(&[7, 1, 1, 7, 1, 1, 7, 1, 1]);
    let jpx = image(10, Some("JPXDecode"), siz);
    assert_eq!(image_size(&doc, &jpx, &jpx.content), ImageSize::TooLarge);
    // A JPEG 2000 file whose small image header box sits in front of a large codestream; each sample is four
    // bytes to the decoder, so one 12,000 by 12,000 component is 576 MB.
    let codestream = |side: u32| {
        let mut c = vec![0xFF, 0x4F, 0xFF, 0x51, 0, 41, 0, 0];
        for value in [side, side, 0, 0, side, side, 0, 0] {
            c.extend_from_slice(&value.to_be_bytes());
        }
        c.extend_from_slice(&1u16.to_be_bytes());
        c.extend_from_slice(&[7, 1, 1]);
        c
    };
    let boxed = |side: u32| {
        let mut ihdr = 22u32.to_be_bytes().to_vec();
        ihdr.extend_from_slice(b"ihdr");
        ihdr.extend_from_slice(&10u32.to_be_bytes());
        ihdr.extend_from_slice(&10u32.to_be_bytes());
        ihdr.extend_from_slice(&[0, 1, 7, 7, 0, 0]);
        let mut file = ((8 + ihdr.len()) as u32).to_be_bytes().to_vec();
        file.extend_from_slice(b"jp2h");
        file.extend_from_slice(&ihdr);
        let stream = codestream(side);
        file.extend_from_slice(&((8 + stream.len()) as u32).to_be_bytes());
        file.extend_from_slice(b"jp2c");
        file.extend_from_slice(&stream);
        file
    };
    let large = image(10, Some("JPXDecode"), boxed(12_000));
    assert_eq!(
        image_size(&doc, &large, &large.content),
        ImageSize::TooLarge
    );
    let modest = image(10, Some("JPXDecode"), boxed(100));
    assert_eq!(
        image_size(&doc, &modest, &modest.content),
        ImageSize::Within(100 * 100 * 4),
        "four bytes for each sample of a 100 by 100 codestream"
    );
}

/// A one-component JPEG's markers: the frame (SOF type, width, height) and the start of its scan.
fn jpeg(sof: u8, width: u16, height: u16, extra: &[u8]) -> Vec<u8> {
    let mut out = vec![0xFF, 0xD8];
    out.extend_from_slice(extra);
    out.extend_from_slice(&[0xFF, sof, 0x00, 0x0B, 0x08]);
    out.extend_from_slice(&height.to_be_bytes());
    out.extend_from_slice(&width.to_be_bytes());
    out.extend_from_slice(&[1, 1, 0x11, 0]);
    out.extend_from_slice(&[0xFF, 0xDA, 0x00, 0x08, 1, 1, 0, 0, 0x3F, 0]);
    out
}

#[test]
fn jpeg_frames_are_sized_as_the_decoder_allocates_or_refused() {
    let doc = document("BT ET");
    let gray = |content: Vec<u8>| {
        Stream::new(
            dictionary! {"Type" => "XObject", "Subtype" => "Image", "Width" => 10, "Height" => 10,
            "ColorSpace" => "DeviceGray", "BitsPerComponent" => 8, "Filter" => "DCTDecode"},
            content,
        )
    };
    let size = |content: Vec<u8>| {
        let stream = gray(content);
        image_size(&doc, &stream, &stream.content)
    };
    // A progressive frame keeps two bytes of coefficients for every sample: 14,000 by 14,000 in gray is 196 MB
    // as a baseline image and 588 MB progressive.
    assert!(matches!(
        size(jpeg(0xC0, 14_000, 14_000, &[])),
        ImageSize::Within(_)
    ));
    assert_eq!(size(jpeg(0xC2, 14_000, 14_000, &[])), ImageSize::TooLarge);
    // A height of zero, set later by a DNL marker, is refused, as are a hierarchical or differential frame, a
    // second frame and a scan before any frame.
    assert_eq!(size(jpeg(0xC0, 100, 0, &[])), ImageSize::Unreadable);
    assert_eq!(size(jpeg(0xC5, 100, 100, &[])), ImageSize::Unreadable);
    assert_eq!(
        size(jpeg(
            0xC0,
            100,
            100,
            &[0xFF, 0xDE, 0x00, 0x09, 8, 0, 100, 0, 100, 1, 1]
        )),
        ImageSize::Unreadable
    );
    let mut twice = jpeg(0xC0, 100, 100, &[]);
    twice.truncate(twice.len() - 10);
    twice.extend_from_slice(&jpeg(0xC0, 100, 100, &[])[2..]);
    assert_eq!(size(twice), ImageSize::Unreadable);
    assert_eq!(
        size(vec![
            0xFF, 0xD8, 0xFF, 0xDA, 0x00, 0x08, 1, 1, 0, 0, 0x3F, 0
        ]),
        ImageSize::Unreadable
    );
}

/// One JBIG2 segment: its number, type, page association and body.
fn segment(number: u32, kind: u8, body: &[u8]) -> Vec<u8> {
    let mut out = number.to_be_bytes().to_vec();
    out.extend_from_slice(&[kind, 0, 1]);
    out.extend_from_slice(&(body.len() as u32).to_be_bytes());
    out.extend_from_slice(body);
    out
}

fn page_information(width: u32, height: u32) -> Vec<u8> {
    let mut body = width.to_be_bytes().to_vec();
    body.extend_from_slice(&height.to_be_bytes());
    body.extend_from_slice(&[0; 8]);
    body.extend_from_slice(&[0, 0, 0]);
    body
}

#[test]
fn ccitt_and_jbig2_images_are_sized_from_their_own_parameters() {
    let doc = document("BT ET");
    let fax = |columns: i64| {
        Stream::new(
            dictionary! {"Type" => "XObject", "Subtype" => "Image", "Width" => 100, "Height" => 100,
            "ImageMask" => true, "Filter" => "CCITTFaxDecode",
            "DecodeParms" => dictionary! {"K" => -1, "Columns" => columns}},
            vec![0; 8],
        )
    };
    let small = fax(100);
    assert_eq!(
        image_size(&doc, &small, &small.content),
        ImageSize::Within(100 * 100)
    );
    let wide = fax(40_000);
    assert_eq!(image_size(&doc, &wide, &wide.content), ImageSize::TooLarge);
    let jbig2 = |data: Vec<u8>| {
        let stream = Stream::new(
            dictionary! {"Type" => "XObject", "Subtype" => "Image", "Width" => 100, "Height" => 100,
            "ColorSpace" => "DeviceGray", "BitsPerComponent" => 1, "Filter" => "JBIG2Decode"},
            data,
        );
        image_size(&doc, &stream, &stream.content)
    };
    assert!(matches!(
        jbig2(segment(1, 48, &page_information(100, 100))),
        ImageSize::Within(_)
    ));
    // A page far larger than the dictionary says.
    assert_eq!(
        jbig2(segment(1, 48, &page_information(40_000, 100))),
        ImageSize::TooLarge
    );
    // A generic region as large as the side limit allows, in a small page: its bitmap counts, and one pixel more
    // passes the side.
    let region = |side: u32| {
        let mut body = side.to_be_bytes().to_vec();
        body.extend_from_slice(&side.to_be_bytes());
        body.extend_from_slice(&[0; 9]);
        body
    };
    let mut both = segment(1, 48, &page_information(100, 100));
    both.extend_from_slice(&segment(2, 38, &region(30_000)));
    assert_eq!(jbig2(both), ImageSize::Within(13 * 100 + 3_750 * 30_000));
    assert_eq!(jbig2(segment(2, 38, &region(30_001))), ImageSize::TooLarge);
    let region = region(1_000);
    // Unknown lengths, a striped page with no end, and too many symbols are refused.
    let mut unknown = segment(1, 38, &region);
    unknown[7..11].copy_from_slice(&[0xFF; 4]);
    assert_eq!(jbig2(unknown), ImageSize::Unreadable);
    assert_eq!(
        jbig2(segment(1, 48, &page_information(100, 0xFFFF_FFFF))),
        ImageSize::Unreadable
    );
    // A symbol dictionary of any size: its symbols' sizes are coded inside its data.
    for count in [2u32, 1_000_000] {
        let mut symbols = vec![0x00, 0x01];
        symbols.extend_from_slice(&count.to_be_bytes());
        symbols.extend_from_slice(&count.to_be_bytes());
        assert_eq!(jbig2(segment(1, 0, &symbols)), ImageSize::Unreadable);
    }
}

#[test]
fn page_text_that_quotes_commands_is_not_a_payload() {
    let doc = Document::load_mem(&fixtures::security_paper()).unwrap();
    let report = report(&doc, &rules());
    assert!(report.matches.is_empty(), "{:?}", report.matches);
    assert_eq!(report.streams.unscanned, 0);
}

#[test]
fn action_text_is_scanned_by_use() {
    let mut doc = document("BT /F1 12 Tf 72 700 Td (Results) Tj ET");
    let launch = doc.add_object(dictionary! {"S" => "Launch",
            "Win" => dictionary! {"F" => Object::string_literal("cmd.exe"), "P" => Object::string_literal("/c calc")}});
    let script = doc.add_object(dictionary! {"S" => "JavaScript",
    "JS" => Object::string_literal("eval(unescape('%u9090'))"), "Next" => launch});
    catalog_mut(&mut doc).set("OpenAction", script);
    let found: Vec<(String, Role)> = report(&doc, &rules())
        .matches
        .into_iter()
        .map(|m| (m.rule, m.role))
        .collect();
    assert!(
        found.contains(&("pdf_launch_command".into(), Role::Launch)),
        "{found:?}"
    );
    assert!(
        found.contains(&("pdf_obfuscated_javascript".into(), Role::Script)),
        "{found:?}"
    );
}

#[test]
fn decoding_stops_at_an_image_codec_and_refuses_unknown_filters() {
    let mut chained = fixtures::compressed(dictionary! {}, b"jpeg bytes");
    chained.dict.set(
        "Filter",
        vec![
            Object::Name(b"FlateDecode".to_vec()),
            Object::Name(b"DCTDecode".to_vec()),
        ],
    );
    assert_eq!(decode(&chained, 1 << 20).unwrap(), b"jpeg bytes");
    let crypt = Stream::new(dictionary! {"Filter" => "Crypt"}, b"x".to_vec());
    assert!(decode(&crypt, 1 << 20).is_none());
    let mut after_codec = fixtures::compressed(dictionary! {}, b"hidden");
    after_codec.dict.set(
        "Filter",
        vec![
            Object::Name(b"DCTDecode".to_vec()),
            Object::Name(b"FlateDecode".to_vec()),
        ],
    );
    assert!(
        decode(&after_codec, 1 << 20).is_none(),
        "a filter after a codec"
    );
    let large = fixtures::compressed(dictionary! {}, &vec![0u8; 1 << 20]);
    assert!(decode(&large, 1000).is_none(), "past the limit");
    assert_eq!(decode(&large, 2 << 20).unwrap().len(), 1 << 20);
}

#[test]
fn counts_hidden_text_by_kind() {
    let visible = document("BT /F1 12 Tf 72 700 Td (Visible words) Tj ET");
    assert_eq!(hidden_text(&visible), Hidden::default());
    let doc = document(concat!(
        "BT /F1 12 Tf 72 700 Td 3 Tr (invisible) Tj 0 Tr ET ",
        "1 1 1 rg BT /F1 12 Tf 72 680 Td (white text) Tj ET 0 g ",
        "BT /F1 0.5 Tf 72 660 Td (tiny) Tj ET ",
        "BT /F1 12 Tf 5000 5000 Td (away) Tj ET ",
        "q 0.01 0 0 0.01 0 0 cm BT /F1 12 Tf 100 100 Td (scaled) Tj ET Q"
    ));
    let hidden = hidden_text(&doc);
    assert_eq!(hidden.invisible, 9);
    assert_eq!(hidden.white, 10);
    assert_eq!(
        hidden.tiny,
        4 + 6,
        "half-point text and text scaled to 0.12 pt"
    );
    assert_eq!(hidden.offpage, 4);
    assert!(!hidden.truncated);
}

#[test]
fn reads_text_inside_forms_once() {
    let mut doc = document("/X1 Do /X1 Do");
    let form = doc.add_object(Stream::new(
            dictionary! {"Type" => "XObject", "Subtype" => "Form", "BBox" => vec![0.into(), 0.into(), 10.into(), 10.into()],
                "Resources" => dictionary! {}},
            b"1 1 1 rg BT /F1 12 Tf 10 10 Td (hidden in a form) Tj ET".to_vec(),
        ));
    resources_mut(&mut doc).set("XObject", dictionary! {"X1" => form});
    assert_eq!(
        hidden_text(&doc).white,
        32,
        "two uses of one form, 16 characters each"
    );
}

#[test]
fn finds_hidden_layers_and_metadata() {
    let mut doc = document("BT ET");
    let layer =
        doc.add_object(dictionary! {"Type" => "OCG", "Name" => Object::string_literal("L")});
    catalog_mut(&mut doc).set(
        "OCProperties",
        dictionary! {"OCGs" => vec![layer.into()],
        "D" => dictionary! {"OFF" => vec![layer.into()]}},
    );
    let info = doc.add_object(dictionary! {"Title" => Object::String(
    vec![0xFE, 0xFF, 0x00, 0x41, 0xDB, 0x40, 0xDC, 0x49], lopdf::StringFormat::Literal)});
    doc.trailer.set("Info", info);
    assert!(hidden_layers(&doc));
    assert_eq!(text::count(&metadata_text(&doc)).tags, 1);
}

#[test]
fn a_resource_is_drawn_only_by_its_own_category_and_scope() {
    // Page 1 draws /Im1. Page 2 names /Im1 and /F1 too, but as a font and inside a string: neither draws an
    // image, so page 2's images named Im1 and F1 are unused.
    let mut doc = document("q 10 0 0 10 72 600 cm /Im1 Do Q");
    let drawn = doc.add_object(fixtures::image(1, 1));
    resources_mut(&mut doc).set("XObject", dictionary! {"Im1" => drawn});
    let same_name = doc.add_object(fixtures::image(1, 1));
    let font_name = doc.add_object(fixtures::image(1, 1));
    let font = doc.add_object(
        dictionary! {"Type" => "Font", "Subtype" => "Type1", "BaseFont" => "Helvetica"},
    );
    fixtures::add_page(
        &mut doc,
        "BT /F1 12 Tf 72 700 Td (/Im1 Do) Tj ET",
        dictionary! {"Font" => dictionary! {"F1" => font},
        "XObject" => dictionary! {"Im1" => same_name, "F1" => font_name}},
    );
    let found = roles(&doc);
    assert_eq!(
        (found[&drawn], found[&same_name], found[&font_name]),
        (Role::Image, Role::Unknown, Role::Unknown)
    );
}

#[test]
fn a_long_chain_of_forms_is_followed_to_its_end() {
    // Forms nested 40 deep, the last drawing an image; the walk reads every form, however deep.
    let mut doc = document("/Fm0 Do");
    let image = doc.add_object(fixtures::image(1, 1));
    let mut next: Dictionary = dictionary! {"Im1" => image};
    let mut inner_content = "/Im1 Do".to_string();
    let mut forms = Vec::new();
    for level in (0..40).rev() {
        let form = doc.add_object(Stream::new(
            dictionary! {"Type" => "XObject", "Subtype" => "Form",
            "BBox" => vec![0.into(), 0.into(), 1.into(), 1.into()],
            "Resources" => dictionary! {"XObject" => next}},
            inner_content.as_bytes().to_vec(),
        ));
        forms.push(form);
        next = dictionary! {format!("Fm{level}").as_str() => form};
        inner_content = format!("/Fm{level} Do");
    }
    resources_mut(&mut doc).set("XObject", next);
    let uses = stream_uses(&doc);
    assert!(uses.complete);
    assert_eq!(uses.roles[&image], Role::Image);
    assert!(forms.iter().all(|f| uses.roles[f] == Role::Content));
}

#[test]
fn inline_images_are_bounded_and_scanned() {
    let doc =
        document("q BI /W 40000 /H 1 /CS /G /BPC 8 /F /AHx ID 00> EI Q BT /F1 12 Tf (After) Tj ET");
    let found = report_of(&doc);
    assert_eq!(found.streams.inline_images, 1);
    assert_eq!(found.streams.oversized_images, 1, "{:?}", found.streams);
    // A program inside an inline image's data is found like one inside an image stream.
    let mut data = String::new();
    for byte in fixtures::program() {
        data.push_str(&format!("{byte:02X}"));
    }
    let doc = document(&format!(
        "q BI /W {} /H 1 /CS /G /BPC 8 /F /AHx ID {data}> EI Q",
        fixtures::program().len()
    ));
    let found = report_of(&doc);
    assert_eq!(found.streams.inline_images, 1);
    assert!(
        found
            .matches
            .iter()
            .any(|m| m.rule == "embedded_pe_program" && m.role == Role::Image),
        "{:?}",
        found.matches
    );
    // An inline image whose end cannot be found holds the file and refuses the rebuild.
    let mut doc = document("q BI /W 9 /H 9 /CS /RGB /BPC 8 /F /DCT ID no end in sight");
    assert_eq!(report_of(&doc).streams.unscanned, 1);
    assert!(rebuild_objects(&mut doc).is_err());
}

fn report_of(doc: &Document) -> Report {
    report(doc, &rules())
}

#[test]
fn names_given_by_reference_read_as_the_names_they_point_to() {
    // Active content whose action, file and annotation types are references.
    let expected = active_content(&fixtures::active_document());
    let mut aliased = fixtures::active_document();
    fixtures::indirect_names(&mut aliased, &[b"S", b"Type", b"Subtype"]);
    let bytes = to_bytes(&mut aliased);
    assert_ne!(
        active_content(&Document::load_mem(&bytes).unwrap()),
        expected,
        "the fixture hides names from a reader that does not resolve them"
    );
    let parsed = parse(&bytes).unwrap();
    assert_eq!(active_content(&parsed), expected);
    let mut stripped = parsed.clone();
    rebuild_objects(&mut stripped).unwrap();
    assert_eq!(active_content(&stripped), Active::default());
    // An image whose subtype and codec are references is an image: its JBIG2 symbol dictionary is refused.
    let mut symbols = vec![0x00, 0x01];
    symbols.extend_from_slice(&2u32.to_be_bytes());
    symbols.extend_from_slice(&2u32.to_be_bytes());
    let mut doc = document("q 100 0 0 100 72 600 cm /Im1 Do Q");
    let image = doc.add_object(Stream::new(
        dictionary! {"Type" => "XObject", "Subtype" => "Image", "Width" => 100, "Height" => 100,
        "ColorSpace" => "DeviceGray", "BitsPerComponent" => 1, "Filter" => "JBIG2Decode"},
        segment(1, 0, &symbols),
    ));
    resources_mut(&mut doc).set("XObject", dictionary! {"Im1" => image});
    let direct = report_of(&doc).streams.unscanned;
    fixtures::indirect_names(&mut doc, &[b"Subtype", b"Filter"]);
    let aliased = parse(&to_bytes(&mut doc)).unwrap();
    assert_eq!(roles(&aliased).get(&image), Some(&Role::Image));
    assert!(direct > 0);
    assert_eq!(report_of(&aliased).streams.unscanned, direct);
}

#[test]
fn chains_of_references_of_any_length_resolve_and_a_cycle_is_unreadable() {
    // Each action, file and annotation type reached through 300 references, past lopdf's own limit of 128.
    let expected = active_content(&fixtures::active_document());
    let mut chained = fixtures::active_document();
    fixtures::indirect_chains(&mut chained, &[b"S", b"Type", b"Subtype"], 300);
    let parsed = parse(&to_bytes(&mut chained)).unwrap();
    assert_eq!(active_content(&parsed), expected);
    let mut stripped = parsed.clone();
    rebuild_objects(&mut stripped).unwrap();
    assert_eq!(active_content(&stripped), Active::default());
    // A chain that returns to itself, and one that ends at a missing object, which reads as null.
    let mut doc = document("BT ET");
    let first = doc.new_object_id();
    let second = doc.add_object(Object::Reference(first));
    doc.objects.insert(first, Object::Reference(second));
    catalog_mut(&mut doc).set("OpenAction", second);
    assert!(parse(&to_bytes(&mut doc)).is_err());
    let mut doc = document("BT ET");
    let action = doc.add_object(dictionary! {"S" => Object::Reference((99_999, 0))});
    catalog_mut(&mut doc).set("OpenAction", action);
    let parsed = parse(&to_bytes(&mut doc)).unwrap();
    let resolved = parsed.get_dictionary(action).unwrap();
    assert_eq!(resolved.get(b"S").unwrap(), &Object::Null);
}

#[test]
fn numbers_and_boxes_given_by_reference_bound_images_and_count_hidden_text() {
    // A fax image whose column count is a reference to a number past the side limit.
    let mut doc = document("q 100 0 0 100 72 600 cm /Im1 Do Q");
    let columns = doc.add_object(Object::Integer(40_000));
    let image = doc.add_object(Stream::new(
        dictionary! {"Type" => "XObject", "Subtype" => "Image", "Width" => 100, "Height" => 100,
        "ImageMask" => true, "Filter" => "CCITTFaxDecode",
        "DecodeParms" => dictionary! {"K" => -1, "Columns" => columns}},
        vec![0; 8],
    ));
    resources_mut(&mut doc).set("XObject", dictionary! {"Im1" => image});
    let bytes = to_bytes(&mut doc);
    let unresolved = report_of(&Document::load_mem(&bytes).unwrap());
    assert_eq!(
        unresolved.streams.oversized_images, 0,
        "unresolved, the reference reads as the default"
    );
    assert_eq!(
        report_of(&parse(&bytes).unwrap()).streams.oversized_images,
        1
    );
    // A crop box whose corners are references: text outside it is off the page.
    let mut doc = document("BT /F1 12 Tf 500 700 Td (Outside) Tj ET");
    let corners: Vec<Object> = [0, 0, 300, 300]
        .iter()
        .map(|v| doc.add_object(Object::Integer(*v)).into())
        .collect();
    let page = page_id(&doc);
    doc.get_object_mut(page)
        .unwrap()
        .as_dict_mut()
        .unwrap()
        .set("CropBox", corners);
    let bytes = to_bytes(&mut doc);
    assert_eq!(hidden_text(&Document::load_mem(&bytes).unwrap()).offpage, 0);
    assert!(hidden_text(&parse(&bytes).unwrap()).offpage > 0);
}

#[test]
fn the_images_of_a_document_are_bounded_together() {
    // An image of 10,000 by 10,000 in gray is 100 MB and passes alone; fifty together pass the 4 GiB budget.
    let mut content = String::new();
    let mut members = Dictionary::new();
    let mut doc = document("");
    for i in 0..50 {
        let image = doc.add_object(fixtures::image(10_000, 10_000));
        members.set(format!("I{i}").as_str(), image);
        content.push_str(&format!("/I{i} Do "));
    }
    resources_mut(&mut doc).set("XObject", members);
    let page = page_id(&doc);
    let contents = doc.add_object(Stream::new(dictionary! {}, content.into_bytes()));
    doc.get_object_mut(page)
        .unwrap()
        .as_dict_mut()
        .unwrap()
        .set("Contents", contents);
    let report = report_of(&doc);
    assert_eq!(report.streams.oversized_images, 1, "the total only");
}

#[test]
fn archives_are_found_wherever_the_copy_would_keep_them() {
    let zip = crate::zip::tests::archive();
    // In the document's metadata stream.
    let mut doc = document("BT /F1 12 Tf 72 700 Td (Abstract) Tj ET");
    let xmp = doc.add_object(fixtures::compressed(
        dictionary! {"Type" => "Metadata", "Subtype" => "XML"},
        &zip,
    ));
    catalog_mut(&mut doc).set("Metadata", xmp);
    assert_eq!(report_of(&doc).streams.archives, 1);
    // In a string.
    let mut doc = document("BT ET");
    let info = doc.add_object(
        dictionary! {"Title" => Object::String(zip.clone(), lopdf::StringFormat::Hexadecimal)},
    );
    doc.trailer.set("Info", info);
    assert_eq!(report_of(&doc).streams.archives, 1);
    // In page content, behind a comment mark; the canonical rebuild leaves it out.
    let mut doc = document("");
    let mut bytes = b"%".to_vec();
    bytes.extend_from_slice(&zip);
    bytes.extend_from_slice(b"\n72 700 m 100 700 l S");
    let contents = doc.add_object(Stream::new(dictionary! {}, bytes));
    let page = page_id(&doc);
    doc.get_object_mut(page)
        .unwrap()
        .as_dict_mut()
        .unwrap()
        .set("Contents", contents);
    assert_eq!(report_of(&doc).streams.archives, 1);
    rebuild_objects(&mut doc).unwrap();
    assert_eq!(report_of(&reread(&mut doc)).streams.archives, 0);
    // Signatures alone are not an archive.
    let doc = document("BT /F1 12 Tf 72 700 Td (PK\\003\\004 and PK\\005\\006) Tj ET");
    assert_eq!(report_of(&doc).streams.archives, 0);
}

#[test]
fn the_rebuild_drops_metadata_and_writes_content_canonically() {
    let mut doc = document(
        "% a comment the copy does not keep\n/Junk (unused) 72 700 m 100 700 l S BT /F1 12 Tf 72 680 Td (Kept) Tj ET",
    );
    let xmp = doc.add_object(Stream::new(
        dictionary! {"Type" => "Metadata", "Subtype" => "XML"},
        b"<x:xmpmeta/>".to_vec(),
    ));
    catalog_mut(&mut doc).set("Metadata", xmp);
    let stripped = rebuild_objects(&mut doc).unwrap();
    assert_eq!(stripped.rewritten_streams, 1);
    let rebuilt = reread(&mut doc);
    let page = page_id(&rebuilt);
    let content = rebuilt.get_page_content(page);
    assert_eq!(
        String::from_utf8(content).unwrap().trim_end(),
        "72 700 m\n100 700 l\nS\nBT\n/F1 12 Tf\n72 680 Td\n<4B657074> Tj\nET"
    );
    assert!(
        roles(&rebuilt).values().all(|r| *r != Role::Metadata),
        "no metadata stream remains"
    );
    assert_eq!(report_of(&rebuilt).streams.content_dropped, 0);
}

#[test]
fn strings_inside_content_operands_are_scanned() {
    let zip = crate::zip::tests::archive();
    let mut hex = String::new();
    for byte in &zip {
        hex.push_str(&format!("{byte:02X}"));
    }
    // An archive in a marked-content property list, kept by the canonical rebuild.
    let doc = document(&format!(
        "/Span <</ActualText <{hex}>>> BDC BT /F1 12 Tf 72 700 Td (Shown) Tj ET EMC"
    ));
    let report = report_of(&doc);
    assert_eq!(report.streams.archives, 1);
    assert_eq!(report.streams.content_dropped, 0);
    // A program's test string in shown text matches as a string.
    let doc = document(
        "BT /F1 12 Tf 72 700 Td (X5O!P%@AP[4\\\\PZX54\\(P^\\)7CC\\)7}$EICAR-STANDARD-ANTIVIRUS-TEST-FILE!$H+H*) Tj ET",
    );
    assert!(
        report_of(&doc)
            .matches
            .iter()
            .any(|m| m.rule == "eicar_test_file" && m.role == Role::String),
        "{:?}",
        report_of(&doc).matches
    );
}

#[test]
fn a_page_tree_deeper_than_the_walk_follows_fails_closed() {
    let mut doc = document("/Im1 Do");
    let image = doc.add_object(fixtures::image(1, 1));
    resources_mut(&mut doc).set("XObject", dictionary! {"Im1" => image});
    // The page's own resources moved to the top of a chain of parents longer than the walk follows.
    let page = page_id(&doc);
    let resources = doc
        .get_dictionary(page)
        .unwrap()
        .get(b"Resources")
        .unwrap()
        .clone();
    let pages = doc
        .catalog()
        .unwrap()
        .get(b"Pages")
        .unwrap()
        .as_reference()
        .unwrap();
    let mut parent = pages;
    for _ in 0..(WALK_DEPTH + 2) {
        parent = doc.add_object(dictionary! {"Type" => "Pages", "Parent" => parent, "Kids" => Vec::<Object>::new(), "Count" => 0});
    }
    let page_dict = doc.get_object_mut(page).unwrap().as_dict_mut().unwrap();
    page_dict.remove(b"Resources");
    page_dict.set("Parent", parent);
    doc.get_object_mut(pages)
        .unwrap()
        .as_dict_mut()
        .unwrap()
        .set("Resources", resources);
    assert!(!stream_uses(&doc).complete);
}

#[test]
fn nesting_deeper_than_the_walks_follow_fails_closed() {
    let mut doc = document("BT ET");
    let mut nested = Object::Dictionary(
        dictionary! {"S" => "JavaScript", "JS" => Object::string_literal("app.alert(1)")},
    );
    for _ in 0..(WALK_DEPTH + 4) {
        nested = Object::Dictionary(dictionary! {"Next" => nested});
    }
    catalog_mut(&mut doc).set("Private", nested);
    let uses = stream_uses(&doc);
    assert!(!uses.complete);
    assert!(!report_of(&doc).walk_complete);
    assert!(rebuild_objects(&mut doc).is_err());
}

#[test]
fn matrices_compose_in_pdf_order() {
    let scale = Matrix([2.0, 0.0, 0.0, 2.0, 0.0, 0.0]);
    let shift = Matrix([1.0, 0.0, 0.0, 1.0, 10.0, 20.0]);
    assert_eq!(shift.then(scale).0, [2.0, 0.0, 0.0, 2.0, 20.0, 40.0]);
    assert_eq!(scale.then(shift).0, [2.0, 0.0, 0.0, 2.0, 10.0, 20.0]);
    assert!((scale.scale() - 2.0).abs() < 1e-9);
}
