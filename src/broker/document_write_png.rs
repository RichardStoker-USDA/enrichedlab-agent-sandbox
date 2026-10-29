//! One figure's PNG, checked before a document embeds its bytes unchanged: the signature, `IHDR` first with a valid
//! header, every chunk's CRC, one run of `IDAT` chunks, `IEND` last with nothing after it, a palette where the colour
//! type needs one (and none where it forbids one), and no chunk that changes how a pixel shows: the ancillary chunks
//! allowed are those that leave every pixel's colour as stored. `tRNS`, `bKGD`, `pHYs` and `sRGB` are checked for their
//! length, place and values, `tIME` for its length only, and the text chunks (`tEXt`, `zTXt`, `iTXt`) are allowed with
//! their bodies unread; animation, colour management (`gAMA`, `cHRM`, `iCCP`, `sBIT`) and unknown chunks are refused.
//! Then the pixel data is decoded as a stream: every row (or, interlaced, every pass's row) with a
//! filter byte from 0 to 4 and exactly its samples, and the zlib stream ending, checksum and all, at the end of the
//! image data. An indexed image's rows are reconstructed from their filters and every pixel's index must name a palette
//! entry; other rows are read and discarded. Office decodes the pixels; this makes sure every one is defined.
use std::io::Read;

use flate2::{Crc, bufread::ZlibDecoder};

/// The most pixels a figure may have.
pub(super) const MAX_PIXELS: u64 = 25_000_000;

/// What a figure's bytes are.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Png {
    /// One well-formed still image of this width and height.
    Fits(u32, u32),
    /// A well-formed image with more pixels than a document holds.
    TooLarge,
    /// Anything else.
    Invalid,
}

/// The interlaced passes (Adam7): first column and row, then the step between columns and rows.
const ADAM7: [(u64, u64, u64, u64); 7] = [
    (0, 0, 8, 8),
    (4, 0, 8, 8),
    (0, 4, 4, 8),
    (2, 0, 4, 4),
    (0, 2, 2, 4),
    (1, 0, 2, 2),
    (0, 1, 1, 2),
];

/// Each ancillary chunk that may appear at most once.
const ONCE: [&[u8; 4]; 5] = [b"tRNS", b"bKGD", b"pHYs", b"sRGB", b"tIME"];

/// A PNG's header, palette size and concatenated image data.
struct Image {
    width: u32,
    height: u32,
    color: u8,
    depth: u8,
    interlace: u8,
    palette: usize,
    idat: Vec<u8>,
}

pub(crate) fn check(data: &[u8]) -> Png {
    let Some(image) = chunks(data) else {
        return Png::Invalid;
    };
    if u64::from(image.width) * u64::from(image.height) > MAX_PIXELS {
        return Png::TooLarge;
    }
    if decodes(&image) {
        Png::Fits(image.width, image.height)
    } else {
        Png::Invalid
    }
}

/// The header, palette and image data of a PNG whose chunks are in order, intact and of the allowed kinds.
fn chunks(data: &[u8]) -> Option<Image> {
    let mut rest = data.strip_prefix(b"\x89PNG\r\n\x1a\n")?;
    let mut header: Option<(u32, u32, u8, u8, u8)> = None;
    let (mut first, mut ended, mut palette) = (true, false, 0usize);
    let (mut idat, mut seen_idat, mut idat_done) = (Vec::new(), false, false);
    let mut seen = [false; ONCE.len()];
    while !rest.is_empty() {
        if ended || rest.len() < 12 {
            return None;
        }
        let length = u32::from_be_bytes(rest[..4].try_into().ok()?) as usize;
        let kind: &[u8; 4] = rest[4..8].try_into().ok()?;
        let end = length.checked_add(12)?;
        if length > 0x7fff_ffff || rest.len() < end || !kind.iter().all(u8::is_ascii_alphabetic) {
            return None;
        }
        let body = &rest[8..8 + length];
        let mut crc = Crc::new();
        crc.update(kind);
        crc.update(body);
        if crc.sum().to_be_bytes() != rest[8 + length..end] {
            return None;
        }
        // Image data comes in one run of IDAT chunks, any of which may be empty.
        if seen_idat && kind != b"IDAT" {
            idat_done = true;
        }
        if let Some(n) = ONCE.iter().position(|k| *k == kind)
            && std::mem::replace(&mut seen[n], true)
        {
            return None;
        }
        let (color, depth) = header.map_or((0, 0), |(_, _, color, depth, _)| (color, depth));
        let before_data = !seen_idat;
        match kind {
            b"IHDR" if first && length == 13 => {
                let width = u32::from_be_bytes(body[..4].try_into().ok()?);
                let height = u32::from_be_bytes(body[4..8].try_into().ok()?);
                let (depth, color) = (body[8], body[9]);
                let depth_ok = match color {
                    0 => matches!(depth, 1 | 2 | 4 | 8 | 16),
                    3 => matches!(depth, 1 | 2 | 4 | 8),
                    2 | 4 | 6 => matches!(depth, 8 | 16),
                    _ => false,
                };
                if width == 0
                    || height == 0
                    || width > 0x7fff_ffff
                    || height > 0x7fff_ffff
                    || !depth_ok
                    || body[10] != 0
                    || body[11] != 0
                    || body[12] > 1
                {
                    return None;
                }
                header = Some((width, height, color, depth, body[12]));
            }
            _ if first => return None,
            // A palette before the data and before tRNS and bKGD, for the colour types that may have one; an indexed
            // image's has at most as many entries as its depth can name.
            b"PLTE"
                if before_data
                    && palette == 0
                    && !seen[0]
                    && !seen[1]
                    && matches!(color, 2 | 3 | 6)
                    && length > 0
                    && length.is_multiple_of(3)
                    && length / 3 <= if color == 3 { 1 << depth } else { 256 } =>
            {
                palette = length / 3
            }
            b"IDAT" if !idat_done => {
                seen_idat = true;
                idat.extend_from_slice(body);
            }
            b"IEND" if length == 0 => ended = true,
            b"tRNS"
                if before_data
                    && match color {
                        0 => length == 2,
                        2 => length == 6,
                        3 => palette > 0 && length <= palette,
                        _ => false,
                    } => {}
            b"bKGD"
                if before_data
                    && match color {
                        0 | 4 => length == 2,
                        2 | 6 => length == 6,
                        3 => palette > 0 && length == 1 && usize::from(body[0]) < palette,
                        _ => false,
                    } => {}
            b"pHYs" if before_data && length == 9 && body[8] <= 1 => {}
            b"sRGB" if before_data && palette == 0 && length == 1 && body[0] <= 3 => {}
            b"tIME" if length == 7 => {}
            b"tEXt" | b"zTXt" | b"iTXt" => {}
            // Anything else: animation, colour management, an unknown chunk or one out of place.
            _ => return None,
        }
        first = false;
        rest = &rest[end..];
    }
    let (width, height, color, depth, interlace) = header?;
    (ended && seen_idat && (color != 3 || palette > 0)).then_some(Image {
        width,
        height,
        color,
        depth,
        interlace,
        palette,
        idat,
    })
}

/// Whether the image data decompresses to exactly the image's rows, each with a filter byte from 0 to 4, read in
/// pieces; an indexed image's rows are reconstructed and each pixel's index checked against the palette.
fn decodes(image: &Image) -> bool {
    let channels: u64 = match image.color {
        0 | 3 => 1,
        4 => 2,
        2 => 3,
        _ => 4,
    };
    let depth = u64::from(image.depth);
    let row = |pixels: u64| (pixels * channels * depth).div_ceil(8);
    let span = |size: u32, start: u64, step: u64| {
        let size = u64::from(size);
        if size > start {
            (size - start).div_ceil(step)
        } else {
            0
        }
    };
    let passes: Vec<(u64, u64)> = if image.interlace == 0 {
        vec![(u64::from(image.width), u64::from(image.height))]
    } else {
        ADAM7
            .iter()
            .map(|&(x0, y0, dx, dy)| (span(image.width, x0, dx), span(image.height, y0, dy)))
            .collect()
    };
    let indexed = image.color == 3;
    let mut decoder = ZlibDecoder::new(image.idat.as_slice());
    let mut filter = [0u8; 1];
    let (mut previous, mut current) = (Vec::new(), Vec::new());
    for (columns, rows) in passes {
        if columns == 0 || rows == 0 {
            continue;
        }
        let samples = row(columns);
        if indexed {
            // Each pass starts with no previous row.
            previous = vec![0u8; samples as usize];
            current = vec![0u8; samples as usize];
        }
        for _ in 0..rows {
            if decoder.read_exact(&mut filter).is_err() || filter[0] > 4 {
                return false;
            }
            if indexed {
                if decoder.read_exact(&mut current).is_err() {
                    return false;
                }
                unfilter(filter[0], &mut current, &previous);
                if !indexes_named(&current, columns, image.depth, image.palette) {
                    return false;
                }
                std::mem::swap(&mut previous, &mut current);
            } else {
                let copied = std::io::copy(&mut (&mut decoder).take(samples), &mut std::io::sink());
                if !matches!(copied, Ok(n) if n == samples) {
                    return false;
                }
            }
        }
    }
    matches!(decoder.read(&mut filter), Ok(0)) && decoder.total_in() == image.idat.len() as u64
}

/// Reconstruct a row of one-byte pixels (an indexed image's, of any depth) from its filter and the previous row.
fn unfilter(kind: u8, row: &mut [u8], previous: &[u8]) {
    for i in 0..row.len() {
        let left = if i > 0 { row[i - 1] } else { 0 };
        let up = previous[i];
        let up_left = if i > 0 { previous[i - 1] } else { 0 };
        let predicted = match kind {
            1 => left,
            2 => up,
            3 => ((u16::from(left) + u16::from(up)) / 2) as u8,
            4 => paeth(left, up, up_left),
            _ => 0,
        };
        row[i] = row[i].wrapping_add(predicted);
    }
}

fn paeth(left: u8, up: u8, up_left: u8) -> u8 {
    let estimate = i16::from(left) + i16::from(up) - i16::from(up_left);
    let distance = |v: u8| (estimate - i16::from(v)).abs();
    let (to_left, to_up, to_up_left) = (distance(left), distance(up), distance(up_left));
    if to_left <= to_up && to_left <= to_up_left {
        left
    } else if to_up <= to_up_left {
        up
    } else {
        up_left
    }
}

/// Whether each of a reconstructed indexed row's `columns` pixels names one of the palette's `entries`; the bits past
/// the last pixel are padding.
fn indexes_named(row: &[u8], columns: u64, depth: u8, entries: usize) -> bool {
    let per_byte = 8 / u64::from(depth);
    let mask = ((1u16 << depth) - 1) as u8;
    (0..columns).all(|x| {
        let byte = row[(x / per_byte) as usize];
        let shift = 8 - u64::from(depth) * (x % per_byte + 1);
        usize::from((byte >> shift) & mask) < entries
    })
}

#[cfg(test)]
#[path = "document_write_png_tests.rs"]
mod tests;
