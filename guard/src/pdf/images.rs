//! Image sizes from each codec's own header.
use super::*;

// Image bounds: what a decoder allocates for each image a viewer draws, from its dictionary and its codec's own header.

/// The longest side and the decoded bytes one image may have, and the decoded bytes of all of a document's images.
pub(super) const IMAGE_SIDE: u64 = 30_000;

pub(super) const IMAGE_BYTES: u64 = 512 * 1024 * 1024;

pub(super) const IMAGE_TOTAL: u64 = 4 * 1024 * 1024 * 1024;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ImageSize {
    /// Within the limits, with the bytes a decoder allocates.
    Within(u64),
    TooLarge,
    /// The dictionary lacks dimensions, or a JPEG or JPEG 2000 header cannot be read.
    Unreadable,
}

pub(super) fn integer(dict: &Dictionary, key: &[u8]) -> Option<u64> {
    dict.get(key)
        .ok()
        .and_then(|o| o.as_i64().ok())
        .and_then(|v| u64::try_from(v).ok())
}

/// Color components the image's color space gives each pixel; four when the space is not recognized.
pub(super) fn components(document: &Document, space: Option<&Object>) -> u64 {
    let Some(space) = space.and_then(|s| deref(document, s)) else {
        return 1;
    };
    let family = match space {
        Object::Name(name) => name.as_slice(),
        Object::Array(items) => match items.first().and_then(|f| f.as_name().ok()) {
            Some(b"ICCBased") => {
                return items
                    .get(1)
                    .and_then(|s| deref(document, s))
                    .and_then(|s| s.as_stream().ok())
                    .and_then(|s| integer(&s.dict, b"N"))
                    .filter(|n| (1..=32).contains(n))
                    .unwrap_or(4);
            }
            Some(b"DeviceN") => {
                return items
                    .get(1)
                    .and_then(|n| deref(document, n))
                    .and_then(|n| n.as_array().ok())
                    .map(|names| names.len() as u64)
                    .filter(|n| (1..=32).contains(n))
                    .unwrap_or(4);
            }
            Some(family) => family,
            None => return 4,
        },
        _ => return 4,
    };
    match family {
        b"DeviceGray" | b"G" | b"CalGray" | b"Indexed" | b"I" | b"Separation" | b"Pattern" => 1,
        b"DeviceRGB" | b"RGB" | b"CalRGB" | b"Lab" => 3,
        _ => 4,
    }
}

/// A codec header's frame: width, height, and the bytes its decoder allocates.
pub(super) type Frame = (u64, u64, u64);

/// From a JPEG's markers up to its first scan. A sample of more than 8 bits takes two bytes, and a progressive frame
/// also keeps every coefficient (two bytes each) until its last scan. A frame the decoder sizes another way is refused:
/// a height of zero (set later by a DNL marker), a second frame or a hierarchical (DHP or differential) frame, more
/// than four components, or no frame before the first scan.
pub(super) fn jpeg_frame(bytes: &[u8]) -> Option<Frame> {
    if !bytes.starts_with(&[0xFF, 0xD8]) {
        return None;
    }
    let mut frame: Option<Frame> = None;
    let mut at = 2;
    for _ in 0..4096 {
        if at + 4 > bytes.len() || bytes[at] != 0xFF {
            return None;
        }
        let marker = bytes[at + 1];
        if marker == 0xFF {
            at += 1;
            continue;
        }
        if matches!(marker, 0x01 | 0xD0..=0xD7) {
            at += 2;
            continue;
        }
        match marker {
            0xDA => return frame,
            0xD9 | 0xDE | 0xC5..=0xC7 | 0xCD..=0xCF => return None,
            _ => {}
        }
        let length = u16::from_be_bytes([bytes[at + 2], bytes[at + 3]]) as usize;
        if length < 2 {
            return None;
        }
        if matches!(marker, 0xC0..=0xC3 | 0xC9..=0xCB) {
            let header = bytes.get(at + 4..at + 2 + length)?;
            if frame.is_some() || header.len() < 6 {
                return None;
            }
            let precision = header[0] as u64;
            let height = u16::from_be_bytes([header[1], header[2]]) as u64;
            let width = u16::from_be_bytes([header[3], header[4]]) as u64;
            let components = header[5] as u64;
            if height == 0 || width == 0 || !(1..=4).contains(&components) {
                return None;
            }
            let sample = if precision > 8 { 2 } else { 1 };
            let coefficients = if matches!(marker, 0xC2 | 0xCA) { 2 } else { 0 };
            frame = Some((
                width,
                height,
                width
                    .saturating_mul(height)
                    .saturating_mul(components)
                    .saturating_mul(sample + coefficients),
            ));
        }
        at += 2 + length;
    }
    None
}

pub(super) fn be32(bytes: &[u8], at: usize) -> Option<u64> {
    Some(u32::from_be_bytes(bytes.get(at..at + 4)?.try_into().ok()?) as u64)
}

/// From a JPEG 2000 codestream's SIZ marker. The decoder keeps every sample as a 32-bit integer, so each component
/// takes four bytes per sample at its own subsampling.
pub(super) fn codestream_frame(bytes: &[u8]) -> Option<Frame> {
    if !bytes.starts_with(&[0xFF, 0x4F, 0xFF, 0x51]) {
        return None;
    }
    let width = be32(bytes, 8)?.saturating_sub(be32(bytes, 16)?);
    let height = be32(bytes, 12)?.saturating_sub(be32(bytes, 20)?);
    let count = u16::from_be_bytes(bytes.get(40..42)?.try_into().ok()?) as usize;
    if count == 0 {
        return None;
    }
    let mut decoded: u64 = 0;
    for i in 0..count {
        let component = bytes.get(42 + 3 * i..45 + 3 * i)?;
        let (dx, dy) = (component[1].max(1) as u64, component[2].max(1) as u64);
        decoded = decoded.saturating_add(
            width
                .div_ceil(dx)
                .saturating_mul(height.div_ceil(dy))
                .saturating_mul(4),
        );
    }
    Some((width, height, decoded))
}

/// From a JPEG 2000 file: the image header box and the codestream it holds, the larger of each; or a bare codestream.
pub(super) fn jpx_frame(bytes: &[u8]) -> Option<Frame> {
    if let Some(frame) = codestream_frame(bytes) {
        return Some(frame);
    }
    let mut header: Option<Frame> = None;
    let mut stream: Option<Frame> = None;
    let mut at = 0;
    let mut end = bytes.len();
    for _ in 0..256 {
        if at + 8 > end {
            break;
        }
        let mut length = be32(bytes, at)?;
        let kind = &bytes[at + 4..at + 8];
        let mut box_header = 8;
        if length == 1 {
            length = u64::from_be_bytes(bytes.get(at + 8..at + 16)?.try_into().ok()?);
            box_header = 16;
        } else if length == 0 {
            length = (end - at) as u64;
        }
        let length = usize::try_from(length).ok()?;
        if length < box_header || at + length > end {
            return None;
        }
        match kind {
            b"jp2h" => {
                // The header superbox: walk its boxes, then what follows it.
                end = at + length;
                at += box_header;
                continue;
            }
            b"ihdr" => {
                let body = &bytes[at + box_header..at + length];
                let height = be32(body, 0)?;
                let width = be32(body, 4)?;
                let count = u16::from_be_bytes(body.get(8..10)?.try_into().ok()?) as u64;
                header = Some((
                    width,
                    height,
                    width
                        .saturating_mul(height)
                        .saturating_mul(count)
                        .saturating_mul(4),
                ));
            }
            b"jp2c" => {
                stream = Some(codestream_frame(&bytes[at + box_header..at + length])?);
                break;
            }
            _ => {}
        }
        at += length;
        // Past the header superbox, the boxes continue to the end of the file.
        if at >= end && end < bytes.len() {
            end = bytes.len();
        }
    }
    match (header, stream) {
        (Some(h), Some(s)) => Some((h.0.max(s.0), h.1.max(s.1), h.2.max(s.2))),
        (one, None) | (None, one) => one,
    }
}

/// The decode parameters of a stream's last filter: the one dictionary for a single filter, or its entry in the array.
pub(super) fn codec_parms<'a>(
    document: &'a Document,
    stream: &'a Stream,
) -> Option<&'a Dictionary> {
    let index = stream.filters().ok()?.len().checked_sub(1)?;
    match deref(document, stream.dict.get(b"DecodeParms").ok()?)? {
        Object::Dictionary(parms) if index == 0 => Some(parms),
        Object::Array(items) => items.get(index).and_then(|p| dict_of(document, p)),
        _ => None,
    }
}

/// A CCITT image's size: its parameters' columns and rows against its dictionary's, the larger of each, one byte per
/// pixel once the decoder expands it.
pub(super) fn ccitt_frame(parms: Option<&Dictionary>, width: u64, height: u64) -> Frame {
    let columns = parms.and_then(|p| integer(p, b"Columns")).unwrap_or(1728);
    let rows = parms.and_then(|p| integer(p, b"Rows")).unwrap_or(0);
    let (width, height) = (width.max(columns), height.max(rows));
    (width, height, width.saturating_mul(height))
}

/// The size every JBIG2 segment declares, across the image's segments and its global segments: each page (a striped
/// page's height from its end-of-stripe segments), each region and each pattern dictionary is a bitmap the decoder
/// allocates. None when a segment cannot be read, declares an unknown length or a page of unknown height with no stripe
/// end, or is a symbol dictionary, whose symbol sizes only its decoder knows.
pub(super) fn jbig2_frame(parts: &[&[u8]]) -> Option<Frame> {
    const REGIONS: &[u8] = &[4, 6, 7, 20, 22, 23, 36, 38, 39, 40, 42, 43];
    const PATTERNS: u64 = 65_536;
    let bitmap = |w: u64, h: u64| w.div_ceil(8).saturating_mul(h);
    let (mut width, mut height, mut bytes) = (0u64, 0u64, 0u64);
    let mut striped_page = false;
    let mut stripe_end = 0u64;
    let mut segments = 0u64;
    for data in parts {
        let mut at = 0usize;
        while at < data.len() {
            segments += 1;
            if segments > 1_000_000 {
                return None;
            }
            let number = be32(data, at)?;
            let flags = *data.get(at + 4)?;
            at += 5;
            let first = *data.get(at)?;
            let mut referred = (first >> 5) as u64;
            if referred == 7 {
                referred = be32(data, at)? & 0x1FFF_FFFF;
                at += 4 + usize::try_from(referred + 1).ok()?.div_ceil(8);
            } else {
                at += 1;
            }
            let reference = if number <= 256 {
                1
            } else if number <= 65_536 {
                2
            } else {
                4
            };
            at = at.checked_add(usize::try_from(referred).ok()?.checked_mul(reference)?)?;
            at += if flags & 0x40 != 0 { 4 } else { 1 };
            let length = be32(data, at)?;
            at += 4;
            if length == 0xFFFF_FFFF {
                return None;
            }
            let body = data.get(at..at.checked_add(usize::try_from(length).ok()?)?)?;
            match flags & 0x3F {
                48 => {
                    let (w, h) = (be32(body, 0)?, be32(body, 4)?);
                    if h == 0xFFFF_FFFF {
                        striped_page = true;
                        width = width.max(w);
                    } else {
                        width = width.max(w);
                        height = height.max(h);
                        bytes = bytes.saturating_add(bitmap(w, h));
                    }
                }
                50 => stripe_end = stripe_end.max(be32(body, 0)?.saturating_add(1)),
                kind if REGIONS.contains(&kind) => {
                    let (w, h) = (be32(body, 0)?, be32(body, 4)?);
                    width = width.max(w);
                    height = height.max(h);
                    bytes = bytes.saturating_add(bitmap(w, h));
                }
                16 => {
                    // A pattern dictionary holds GRAYMAX + 1 patterns of HDPW by HDPH, and their collective bitmap.
                    let (w, h) = (*body.get(1)? as u64, *body.get(2)? as u64);
                    let patterns = be32(body, 3)?.saturating_add(1);
                    if patterns > PATTERNS {
                        return None;
                    }
                    bytes = bytes
                        .saturating_add(bitmap(w.saturating_mul(patterns), h).saturating_mul(2));
                }
                // A symbol dictionary's symbol sizes are coded inside its data, so no header bounds them: the
                // image is refused rather than passed unbounded.
                0 => return None,
                _ => {}
            }
            at += body.len();
        }
    }
    if striped_page {
        if stripe_end == 0 {
            return None;
        }
        height = height.max(stripe_end);
        bytes = bytes.saturating_add(bitmap(width, stripe_end));
    }
    Some((width, height, bytes))
}

/// Checks one image stream against the side and decoded-size limits: its dictionary's size and, for a JPEG, JPEG 2000,
/// CCITT or JBIG2 image, the size its codec's own headers or parameters declare, the larger of each. `codec_input` is
/// the stream after its general filters.
pub fn image_size(document: &Document, stream: &Stream, codec_input: &[u8]) -> ImageSize {
    let dict = &stream.dict;
    let (Some(width), Some(height)) = (integer(dict, b"Width"), integer(dict, b"Height")) else {
        return ImageSize::Unreadable;
    };
    let mask = dict.get(b"ImageMask").ok().and_then(|o| o.as_bool().ok()) == Some(true);
    let bits = if mask {
        1
    } else {
        integer(dict, b"BitsPerComponent").unwrap_or(8).clamp(1, 16)
    };
    let parts = if mask {
        1
    } else {
        components(document, dict.get(b"ColorSpace").ok())
    };
    let sample = if bits > 8 { 2 } else { 1 };
    let mut frame = (
        width,
        height,
        width
            .saturating_mul(height)
            .saturating_mul(parts)
            .saturating_mul(sample),
    );
    let codec = stream
        .filters()
        .ok()
        .and_then(|filters| filters.last().map(|f| f.to_vec()));
    let header = match codec.as_deref() {
        Some(b"DCTDecode") => Some(jpeg_frame(codec_input)),
        Some(b"JPXDecode") => Some(jpx_frame(codec_input)),
        Some(b"CCITTFaxDecode") => Some(Some(ccitt_frame(
            codec_parms(document, stream),
            width,
            height,
        ))),
        Some(b"JBIG2Decode") => {
            let globals = codec_parms(document, stream)
                .and_then(|p| p.get(b"JBIG2Globals").ok())
                .and_then(|g| deref(document, g))
                .and_then(|g| g.as_stream().ok())
                .map(|g| decode(g, STREAM_LIMIT));
            match globals {
                Some(None) => Some(None),
                Some(Some(globals)) => Some(jbig2_frame(&[&globals, codec_input])),
                None => Some(jbig2_frame(&[codec_input])),
            }
        }
        _ => None,
    };
    if let Some(found) = header {
        let Some((w, h, decoded)) = found else {
            return ImageSize::Unreadable;
        };
        frame = (frame.0.max(w), frame.1.max(h), frame.2.max(decoded));
    }
    let (width, height, decoded) = frame;
    if width > IMAGE_SIDE || height > IMAGE_SIDE || decoded > IMAGE_BYTES {
        ImageSize::TooLarge
    } else {
        ImageSize::Within(decoded)
    }
}
