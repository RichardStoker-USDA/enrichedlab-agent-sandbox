//! PNG and JPEG checks (L0) and re-encoding (L2).
//!
//! The structure walk follows each format's chunks or segments to its end marker, verifies PNG chunk checksums, and
//! collects metadata text (PNG text chunks, JPEG comments and XMP) for the Unicode count. Bytes after the end marker
//! are counted. The rebuild decodes the pixels within fixed limits, applies the stored orientation, and encodes a new
//! file without any metadata.
use anyhow::{Context, Result, bail, ensure};
use flate2::read::ZlibDecoder;
use image::{DynamicImage, ImageDecoder, ImageFormat, ImageReader, Limits};
use std::io::{Cursor, Read};

const METADATA_LIMIT: usize = 1024 * 1024;
const MAX_SIDE: u32 = 20_000;
const MAX_ALLOC: u64 = 768 * 1024 * 1024;

#[derive(Clone, Debug, Default)]
pub struct Walk {
    pub valid: bool,
    pub trailing: u64,
    pub metadata: String,
}

fn push_text(out: &mut String, bytes: &[u8]) {
    if out.len() < METADATA_LIMIT {
        let room = METADATA_LIMIT - out.len();
        out.push_str(&String::from_utf8_lossy(&bytes[..bytes.len().min(room)]));
        out.push('\n');
    }
}

fn inflate_limited(bytes: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    let _ = ZlibDecoder::new(bytes)
        .take(METADATA_LIMIT as u64)
        .read_to_end(&mut out);
    out
}

pub fn walk_png(bytes: &[u8]) -> Walk {
    let mut walk = Walk::default();
    let mut at = 8;
    while at + 12 <= bytes.len() {
        let length = u32::from_be_bytes(bytes[at..at + 4].try_into().unwrap()) as usize;
        let kind = &bytes[at + 4..at + 8];
        let Some(end) = at
            .checked_add(12)
            .and_then(|v| v.checked_add(length))
            .filter(|e| *e <= bytes.len())
        else {
            return walk;
        };
        let data = &bytes[at + 8..at + 8 + length];
        let stored = u32::from_be_bytes(bytes[end - 4..end].try_into().unwrap());
        let mut hasher = crc32fast::Hasher::new();
        hasher.update(kind);
        hasher.update(data);
        if hasher.finalize() != stored {
            return walk;
        }
        match kind {
            b"tEXt" => push_text(
                &mut walk.metadata,
                data.split(|b| *b == 0).nth(1).unwrap_or(&[]),
            ),
            b"zTXt" => {
                if let Some(nul) = data.iter().position(|b| *b == 0) {
                    push_text(
                        &mut walk.metadata,
                        &inflate_limited(data.get(nul + 2..).unwrap_or(&[])),
                    );
                }
            }
            b"iTXt" => {
                // keyword \0 flag method language \0 translated \0 text
                let mut parts = data.splitn(2, |b| *b == 0);
                let _keyword = parts.next();
                let rest = parts.next().unwrap_or(&[]);
                if rest.len() >= 2 {
                    let compressed = rest[0] == 1;
                    let mut fields = rest[2..].splitn(3, |b| *b == 0);
                    let _language = fields.next();
                    let _translated = fields.next();
                    let body = fields.next().unwrap_or(&[]);
                    let text = if compressed {
                        inflate_limited(body)
                    } else {
                        body.to_vec()
                    };
                    push_text(&mut walk.metadata, &text);
                }
            }
            b"IEND" => {
                walk.valid = true;
                walk.trailing = (bytes.len() - end) as u64;
                return walk;
            }
            _ => {}
        }
        at = end;
    }
    walk
}

pub fn walk_jpeg(bytes: &[u8]) -> Walk {
    let mut walk = Walk::default();
    let mut at = 2;
    loop {
        // Fill bytes may precede a marker.
        while at < bytes.len() && bytes[at] == 0xFF && bytes.get(at + 1) == Some(&0xFF) {
            at += 1;
        }
        if at + 2 > bytes.len() || bytes[at] != 0xFF {
            return walk;
        }
        let marker = bytes[at + 1];
        match marker {
            0xD9 => {
                walk.valid = true;
                walk.trailing = (bytes.len() - at - 2) as u64;
                return walk;
            }
            0x01 | 0xD0..=0xD7 => {
                at += 2;
                continue;
            }
            _ => {}
        }
        if at + 4 > bytes.len() {
            return walk;
        }
        let length = u16::from_be_bytes([bytes[at + 2], bytes[at + 3]]) as usize;
        if length < 2 || at + 2 + length > bytes.len() {
            return walk;
        }
        let data = &bytes[at + 4..at + 2 + length];
        match marker {
            0xFE => push_text(&mut walk.metadata, data),
            0xE1 if data.starts_with(b"http://ns.adobe.com/xap/1.0/\0") => {
                push_text(&mut walk.metadata, &data[29..])
            }
            _ => {}
        }
        at += 2 + length;
        if marker == 0xDA {
            // Entropy-coded data runs to the next marker that is not a stuffed byte or a restart.
            while at + 1 < bytes.len() {
                if bytes[at] == 0xFF && !matches!(bytes[at + 1], 0x00 | 0xD0..=0xD7 | 0xFF) {
                    break;
                }
                at += 1;
            }
        }
    }
}

/// Decodes and re-encodes an image as the same format without metadata.
pub fn reencode(bytes: &[u8], format: ImageFormat) -> Result<Vec<u8>> {
    ensure!(
        matches!(format, ImageFormat::Png | ImageFormat::Jpeg),
        "only PNG and JPEG are rebuilt"
    );
    let mut limits = Limits::default();
    limits.max_image_width = Some(MAX_SIDE);
    limits.max_image_height = Some(MAX_SIDE);
    limits.max_alloc = Some(MAX_ALLOC);
    let mut reader = ImageReader::with_format(Cursor::new(bytes), format);
    reader.limits(limits);
    let mut decoder = reader.into_decoder().context("image header unreadable")?;
    let orientation = decoder
        .orientation()
        .unwrap_or(image::metadata::Orientation::NoTransforms);
    let mut pixels = DynamicImage::from_decoder(decoder).context("image pixels unreadable")?;
    pixels.apply_orientation(orientation);
    let mut out = Vec::new();
    match format {
        ImageFormat::Png => pixels
            .write_to(&mut Cursor::new(&mut out), ImageFormat::Png)
            .context("PNG not encoded")?,
        _ => {
            let rgb = DynamicImage::ImageRgb8(pixels.to_rgb8());
            let encoder = image::codecs::jpeg::JpegEncoder::new_with_quality(&mut out, 92);
            rgb.write_with_encoder(encoder)
                .context("JPEG not encoded")?;
        }
    }
    if out.is_empty() {
        bail!("the rebuilt image is empty");
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fixtures::{jpeg, png};

    fn chunk(kind: &[u8], data: &[u8]) -> Vec<u8> {
        let mut out = (data.len() as u32).to_be_bytes().to_vec();
        out.extend_from_slice(kind);
        out.extend_from_slice(data);
        let mut h = crc32fast::Hasher::new();
        h.update(kind);
        h.update(data);
        out.extend_from_slice(&h.finalize().to_be_bytes());
        out
    }

    #[test]
    fn walks_a_png_and_counts_what_follows_iend() {
        let clean = png();
        let walk = walk_png(&clean);
        assert!(walk.valid && walk.trailing == 0 && walk.metadata.is_empty());
        let mut tagged = clean[..clean.len() - 12].to_vec();
        tagged.extend_from_slice(&chunk(b"tEXt", b"Comment\0note \xF3\xA0\x81\x89"));
        tagged.extend_from_slice(&clean[clean.len() - 12..]);
        tagged.extend_from_slice(b"<?php system($_GET[0]); ?>");
        let walk = walk_png(&tagged);
        assert!(walk.valid);
        assert_eq!(walk.trailing, 26);
        assert_eq!(crate::text::count(&walk.metadata).tags, 1);
        let mut broken = clean.clone();
        broken[20] ^= 0xFF;
        assert!(!walk_png(&broken).valid, "a bad checksum stops the walk");
    }

    #[test]
    fn walks_a_jpeg_and_counts_what_follows_eoi() {
        let clean = jpeg();
        let walk = walk_jpeg(&clean);
        assert!(walk.valid && walk.trailing == 0, "{walk:?}");
        let mut appended = clean.clone();
        appended.extend_from_slice(b"PK\x03\x04hidden archive");
        assert_eq!(walk_jpeg(&appended).trailing, 18);
        let mut commented = vec![0xFF, 0xD8, 0xFF, 0xFE, 0x00, 0x07];
        commented.extend_from_slice(b"note\0");
        commented.extend_from_slice(&clean[2..]);
        let walk = walk_jpeg(&commented);
        assert!(walk.valid && walk.metadata.starts_with("note"));
    }

    #[test]
    fn reencoding_drops_trailing_data_and_metadata() {
        let mut tagged = png();
        tagged.extend_from_slice(b"trailing payload");
        let rebuilt = reencode(&tagged, ImageFormat::Png).unwrap();
        let walk = walk_png(&rebuilt);
        assert!(walk.valid && walk.trailing == 0 && walk.metadata.is_empty());
        let mut jpg = jpeg();
        jpg.extend_from_slice(b"MZ fake program");
        let rebuilt = reencode(&jpg, ImageFormat::Jpeg).unwrap();
        assert_eq!(walk_jpeg(&rebuilt).trailing, 0);
    }

    #[test]
    fn a_huge_declared_image_is_refused_before_decoding() {
        let mut header = png();
        // Rewrite IHDR's width to 100,000 and fix its checksum.
        header[16..20].copy_from_slice(&100_000u32.to_be_bytes());
        let mut h = crc32fast::Hasher::new();
        h.update(&header[12..29]);
        let crc = h.finalize().to_be_bytes();
        header[29..33].copy_from_slice(&crc);
        assert!(reencode(&header, ImageFormat::Png).is_err());
    }
}
