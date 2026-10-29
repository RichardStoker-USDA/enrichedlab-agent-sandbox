//! The PNG check: well-formed still images of each colour type, depth and interlace pass, and the ways a file can look
//! like one without being one: chunk faults, and pixel data that does not decode to exactly the image's rows.
use std::io::Write;

use flate2::{Compression, Crc, write::ZlibEncoder};

use super::{MAX_PIXELS, Png, check};

fn chunk(out: &mut Vec<u8>, kind: &[u8; 4], body: &[u8]) {
    out.extend_from_slice(&(body.len() as u32).to_be_bytes());
    out.extend_from_slice(kind);
    out.extend_from_slice(body);
    let mut crc = Crc::new();
    crc.update(kind);
    crc.update(body);
    out.extend_from_slice(&crc.sum().to_be_bytes());
}

fn header(width: u32, height: u32, depth: u8, color: u8, interlace: u8) -> Vec<u8> {
    let mut h = width.to_be_bytes().to_vec();
    h.extend_from_slice(&height.to_be_bytes());
    h.extend_from_slice(&[depth, color, 0, 0, interlace]);
    h
}

fn zlib(raw: &[u8]) -> Vec<u8> {
    let mut encoder = ZlibEncoder::new(Vec::new(), Compression::default());
    encoder.write_all(raw).unwrap();
    encoder.finish().unwrap()
}

/// Raw image data of zero samples: each row (each pass's row when interlaced) a filter byte and its samples.
fn rows(width: u64, height: u64, bits_per_pixel: u64, interlace: bool, filter: u8) -> Vec<u8> {
    let passes: Vec<(u64, u64)> = if interlace {
        [
            (0, 0, 8, 8),
            (4, 0, 8, 8),
            (0, 4, 4, 8),
            (2, 0, 4, 4),
            (0, 2, 2, 4),
            (1, 0, 2, 2),
            (0, 1, 1, 2),
        ]
        .iter()
        .map(|&(x, y, dx, dy): &(u64, u64, u64, u64)| {
            let span = |n: u64, s: u64, d: u64| if n > s { (n - s).div_ceil(d) } else { 0 };
            (span(width, x, dx), span(height, y, dy))
        })
        .collect()
    } else {
        vec![(width, height)]
    };
    let mut raw = Vec::new();
    for (w, h) in passes {
        if w == 0 || h == 0 {
            continue;
        }
        for _ in 0..h {
            raw.push(filter);
            raw.extend(std::iter::repeat_n(
                0u8,
                (w * bits_per_pixel).div_ceil(8) as usize,
            ));
        }
    }
    raw
}

/// A PNG of the given header, chunks before the image data, image data chunks and chunks after it.
fn build(
    head: &[u8],
    before: &[(&[u8; 4], Vec<u8>)],
    idat: &[Vec<u8>],
    after: &[(&[u8; 4], Vec<u8>)],
) -> Vec<u8> {
    let mut out = b"\x89PNG\r\n\x1a\n".to_vec();
    chunk(&mut out, b"IHDR", head);
    for (kind, body) in before {
        chunk(&mut out, kind, body);
    }
    for body in idat {
        chunk(&mut out, b"IDAT", body);
    }
    for (kind, body) in after {
        chunk(&mut out, kind, body);
    }
    chunk(&mut out, b"IEND", b"");
    out
}

fn rgb(width: u32, height: u32) -> Vec<u8> {
    build(
        &header(width, height, 8, 2, 0),
        &[],
        &[zlib(&rows(width.into(), height.into(), 24, false, 0))],
        &[],
    )
}

#[test]
fn every_colour_type_depth_and_interlace_decodes() {
    let palette = vec![(b"PLTE", vec![0u8; 6])];
    let cases: Vec<(&str, Vec<u8>)> = vec![
        ("rgb 8", rgb(5, 3)),
        (
            "grey 1",
            build(
                &header(9, 2, 1, 0, 0),
                &[],
                &[zlib(&rows(9, 2, 1, false, 0))],
                &[],
            ),
        ),
        (
            "grey 16",
            build(
                &header(3, 3, 16, 0, 0),
                &[],
                &[zlib(&rows(3, 3, 16, false, 4))],
                &[],
            ),
        ),
        (
            "palette 4",
            build(
                &header(7, 2, 4, 3, 0),
                &palette,
                &[zlib(&rows(7, 2, 4, false, 1))],
                &[],
            ),
        ),
        (
            "grey alpha 8",
            build(
                &header(2, 2, 8, 4, 0),
                &[],
                &[zlib(&rows(2, 2, 16, false, 2))],
                &[],
            ),
        ),
        (
            "rgba 16",
            build(
                &header(2, 2, 16, 6, 0),
                &[],
                &[zlib(&rows(2, 2, 64, false, 3))],
                &[],
            ),
        ),
        (
            "interlaced rgb",
            build(
                &header(5, 5, 8, 2, 1),
                &[],
                &[zlib(&rows(5, 5, 24, true, 0))],
                &[],
            ),
        ),
        (
            "interlaced one pixel",
            build(
                &header(1, 1, 8, 2, 1),
                &[],
                &[zlib(&rows(1, 1, 24, true, 0))],
                &[],
            ),
        ),
    ];
    for (name, png) in &cases {
        assert!(matches!(check(png), Png::Fits(..)), "{name}");
    }
    // Image data split over consecutive chunks, with ancillary chunks before it, is one stream.
    let data = zlib(&rows(4, 4, 24, false, 0));
    let (a, b) = data.split_at(data.len() / 2);
    let split = build(
        &header(4, 4, 8, 2, 0),
        &[(b"pHYs", vec![0, 0, 23, 18, 0, 0, 23, 18, 1])],
        &[a.to_vec(), b.to_vec()],
        &[],
    );
    assert_eq!(check(&split), Png::Fits(4, 4));
}

#[test]
fn chunk_faults_are_refused() {
    let good = rgb(4, 3);
    let mut crc = good.clone();
    let at = crc.len() - 20;
    crc[at] ^= 1;
    let mut trailing = good.clone();
    trailing.push(0);
    let mut no_end = good.clone();
    no_end.truncate(no_end.len() - 12);
    let animated = build(
        &header(4, 3, 8, 2, 0),
        &[(b"acTL", vec![0, 0, 0, 1, 0, 0, 0, 0])],
        &[zlib(&rows(4, 3, 24, false, 0))],
        &[],
    );
    let critical = build(
        &header(4, 3, 8, 2, 0),
        &[(b"ABCD", b"x".to_vec())],
        &[zlib(&rows(4, 3, 24, false, 0))],
        &[],
    );
    let no_palette = build(
        &header(7, 2, 4, 3, 0),
        &[],
        &[zlib(&rows(7, 2, 4, false, 0))],
        &[],
    );
    let bad_depth = build(
        &header(4, 3, 4, 2, 0),
        &[],
        &[zlib(&rows(4, 3, 12, false, 0))],
        &[],
    );
    let data = zlib(&rows(4, 3, 24, false, 0));
    let (a, b) = data.split_at(data.len() / 2);
    let apart = build(
        &header(4, 3, 8, 2, 0),
        &[],
        &[a.to_vec()],
        &[(b"tEXt", b"k\0v".to_vec())],
    );
    let apart = {
        // IDAT, an ancillary chunk, then IDAT again: image data must be one run.
        let mut out = apart[..apart.len() - 12].to_vec();
        chunk(&mut out, b"IDAT", b);
        chunk(&mut out, b"IEND", b"");
        out
    };
    for (name, bytes) in [
        ("crc", &crc),
        ("trailing", &trailing),
        ("end", &no_end),
        ("animated", &animated),
        ("critical", &critical),
        ("palette", &no_palette),
        ("depth", &bad_depth),
        ("apart", &apart),
    ] {
        assert_eq!(check(bytes), Png::Invalid, "{name}");
    }
}

#[test]
fn pixel_data_that_does_not_decode_to_the_rows_is_refused() {
    let head = header(4, 3, 8, 2, 0);
    let raw = rows(4, 3, 24, false, 0);
    let data = zlib(&raw);
    let mut extra_bytes = data.clone();
    extra_bytes.extend_from_slice(b"\0\0");
    let mut longer = raw.clone();
    longer.extend_from_slice(&[0; 13]);
    let cases: Vec<(&str, Vec<u8>)> = vec![
        // A valid header, correct CRCs, an empty IDAT and IEND: no pixel data at all.
        ("empty", {
            let mut out = b"\x89PNG\r\n\x1a\n".to_vec();
            chunk(&mut out, b"IHDR", &head);
            chunk(&mut out, b"IDAT", b"");
            chunk(&mut out, b"IEND", b"");
            out
        }),
        (
            "truncated stream",
            build(&head, &[], &[data[..data.len() - 6].to_vec()], &[]),
        ),
        (
            "not zlib",
            build(&head, &[], &[vec![1, 2, 3, 4, 5, 6, 7, 8]], &[]),
        ),
        (
            "filter byte",
            build(&head, &[], &[zlib(&rows(4, 3, 24, false, 7))], &[]),
        ),
        (
            "short rows",
            build(&head, &[], &[zlib(&raw[..raw.len() - 1])], &[]),
        ),
        ("long rows", build(&head, &[], &[zlib(&longer)], &[])),
        (
            "bytes after the stream",
            build(&head, &[], &[extra_bytes], &[]),
        ),
        (
            "not interlaced",
            build(
                &header(5, 5, 8, 2, 1),
                &[],
                &[zlib(&rows(5, 5, 24, false, 0))],
                &[],
            ),
        ),
    ];
    for (name, png) in &cases {
        assert_eq!(check(png), Png::Invalid, "{name}");
    }
}

#[test]
fn an_image_past_the_pixel_limit_is_too_large_before_any_decoding() {
    let side = (MAX_PIXELS as f64).sqrt() as u32 + 1;
    let png = build(&header(side, side, 8, 2, 0), &[], &[zlib(b"\0")], &[]);
    assert_eq!(check(&png), Png::TooLarge);
}

/// An indexed image of one row: the given filter byte and raw sample bytes, under a palette of `entries` entries.
fn indexed(depth: u8, width: u32, entries: usize, filter: u8, raw: &[u8]) -> Vec<u8> {
    let mut data = vec![filter];
    data.extend_from_slice(raw);
    build(
        &header(width, 1, depth, 3, 0),
        &[(b"PLTE", vec![0u8; 3 * entries])],
        &[zlib(&data)],
        &[],
    )
}

#[test]
fn every_pixel_of_an_indexed_image_must_name_a_palette_entry() {
    // The review's probe: a 1-bit image with one palette entry and a pixel naming entry 1.
    assert_eq!(check(&indexed(1, 2, 1, 0, &[0b0100_0000])), Png::Invalid);
    assert_eq!(check(&indexed(1, 2, 1, 0, &[0b0000_0000])), Png::Fits(2, 1));
    // Padding bits past the last pixel are not pixels.
    assert_eq!(check(&indexed(1, 2, 1, 0, &[0b0011_1111])), Png::Fits(2, 1));
    // Indexes are read after the filter is undone: Sub turns raw 1, 1, 1 into 1, 2, 3 and 1, 1, 1, 1 into 1 to 4.
    assert_eq!(check(&indexed(8, 3, 4, 1, &[1, 1, 1])), Png::Fits(3, 1));
    assert_eq!(check(&indexed(8, 4, 4, 1, &[1, 1, 1, 1])), Png::Invalid);
    // Up, Average and Paeth on a first row see a zero row above.
    assert_eq!(check(&indexed(8, 2, 2, 2, &[1, 1])), Png::Fits(2, 1));
    // Average: raw 2, 1 becomes 2, 2 and raw 2, 2 becomes 2, 3, past a palette of three.
    assert_eq!(check(&indexed(8, 2, 3, 3, &[2, 1])), Png::Fits(2, 1));
    assert_eq!(check(&indexed(8, 2, 3, 3, &[2, 2])), Png::Invalid);
    assert_eq!(check(&indexed(8, 2, 3, 4, &[1, 1])), Png::Fits(2, 1));
    // A two-row image whose second row is Up from the first: raw 1, 0 over 1, 2 is 2, 2.
    let data = zlib(&[0, 1, 2, 2, 1, 0]);
    let two = build(
        &header(2, 2, 8, 3, 0),
        &[(b"PLTE", vec![0u8; 9])],
        &[data],
        &[],
    );
    assert_eq!(check(&two), Png::Fits(2, 2));
    let data = zlib(&[0, 1, 2, 2, 1, 2]);
    let over = build(
        &header(2, 2, 8, 3, 0),
        &[(b"PLTE", vec![0u8; 9])],
        &[data],
        &[],
    );
    assert_eq!(check(&over), Png::Invalid, "2 + 2 names entry 4 of 3");
    // Interlaced: every pass's rows are checked.
    let palette = vec![(b"PLTE", vec![0u8; 6])];
    let ok = build(
        &header(5, 5, 8, 3, 1),
        &palette,
        &[zlib(&rows(5, 5, 8, true, 0))],
        &[],
    );
    assert_eq!(check(&ok), Png::Fits(5, 5));
}

#[test]
fn only_chunks_that_leave_every_colour_as_stored_are_allowed() {
    let rgb = |before: &[(&[u8; 4], Vec<u8>)]| {
        build(
            &header(3, 2, 8, 2, 0),
            before,
            &[zlib(&rows(3, 2, 24, false, 0))],
            &[],
        )
    };
    let physical = (b"pHYs", vec![0, 0, 11, 19, 0, 0, 11, 19, 1]);
    let background = (b"bKGD", vec![0, 255, 0, 255, 0, 255]);
    // What ragg writes: pHYs and bKGD, at 8 or 16 bits, with or without alpha.
    assert_eq!(
        check(&rgb(&[physical.clone(), background.clone()])),
        Png::Fits(3, 2)
    );
    let rgba16 = build(
        &header(2, 2, 16, 6, 0),
        &[physical.clone(), (b"bKGD", vec![0; 6])],
        &[zlib(&rows(2, 2, 64, false, 0))],
        &[],
    );
    assert_eq!(check(&rgba16), Png::Fits(2, 2));
    assert_eq!(
        check(&rgb(&[(b"sRGB", vec![0]), (b"tRNS", vec![0; 6])])),
        Png::Fits(3, 2)
    );
    assert_eq!(
        check(&rgb(&[
            (b"tEXt", b"Software\0R".to_vec()),
            (b"tIME", vec![7, 234, 10, 17, 12, 0, 0])
        ])),
        Png::Fits(3, 2)
    );
    for (name, before) in [
        ("gamma", vec![(b"gAMA", vec![0, 0, 177, 143])]),
        ("ICC profile", vec![(b"iCCP", b"p\0\0x".to_vec())]),
        ("chromaticity", vec![(b"cHRM", vec![0; 32])]),
        ("significant bits", vec![(b"sBIT", vec![8, 8, 8])]),
        ("unknown", vec![(b"prVt", vec![1])]),
        ("pHYs twice", vec![physical.clone(), physical.clone()]),
        ("bKGD of the wrong size", vec![(b"bKGD", vec![0, 0])]),
        ("tRNS of the wrong size", vec![(b"tRNS", vec![0, 0])]),
        ("palette for grey", vec![]),
    ] {
        let png = if name == "palette for grey" {
            build(
                &header(3, 2, 8, 0, 0),
                &[(b"PLTE", vec![0; 6])],
                &[zlib(&rows(3, 2, 8, false, 0))],
                &[],
            )
        } else {
            rgb(&before)
        };
        assert_eq!(check(&png), Png::Invalid, "{name}");
    }
    // An indexed image: tRNS no longer than the palette, bKGD naming an entry, a palette its depth can name.
    let image = |chunks: &[(&[u8; 4], Vec<u8>)], depth: u8| {
        build(&header(4, 1, depth, 3, 0), chunks, &[zlib(&[0, 0, 0])], &[])
    };
    let palette = (b"PLTE", vec![0u8; 6]);
    assert_eq!(
        check(&image(&[palette.clone(), (b"tRNS", vec![255, 0])], 4)),
        Png::Fits(4, 1)
    );
    assert_eq!(
        check(&image(&[palette.clone(), (b"tRNS", vec![255, 0, 0])], 4)),
        Png::Invalid
    );
    assert_eq!(
        check(&image(&[palette.clone(), (b"bKGD", vec![2])], 4)),
        Png::Invalid
    );
    assert_eq!(
        check(&image(&[(b"PLTE", vec![0u8; 9])], 1)),
        Png::Invalid,
        "three entries at 1 bit"
    );
}
