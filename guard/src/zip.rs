//! Archives inside other bytes, recognized by structure rather than by a signature.
//!
//! A ZIP reader opens an archive from its end-of-central-directory record (or the ZIP64 record and locator in front of
//! it): the record says where the central directory sits, and each directory entry says where its local header sits.
//! `find` reports a ZIP archive when those three agree, or when a local entry is one a streaming reader reads to the
//! end of its data, wherever it sits and whatever its version, method or name. A 7-Zip, xz or RAR archive counts when
//! its signature is followed by a header whose own CRC matches. Each check holds at any offset, so an archive appended
//! to an image, a font or other data is found, while four or six bytes that happen to spell a signature inside
//! compressed data almost never count: the fields after them must place the archive's data inside the bytes, or a
//! header CRC must match. Every candidate is checked within one work budget; more candidates than the recognizer
//! checks, or more work than the budget allows, count as an archive, since a reader may find one among them.
//!
//! One recognizer serves the uploaded file, every stream a PDF keeps, its inline images, its strings and the strings in
//! its content.

use std::collections::HashSet;

const EOCD: &[u8] = b"PK\x05\x06";
const ZIP64_LOCATOR: &[u8] = b"PK\x06\x07";
const ZIP64_EOCD: &[u8] = b"PK\x06\x06";
const CENTRAL: &[u8] = b"PK\x01\x02";
const LOCAL: &[u8] = b"PK\x03\x04";
const DESCRIPTOR: &[u8] = b"PK\x07\x08";
const SEVEN_ZIP: &[u8] = b"7z\xBC\xAF\x27\x1C";
const XZ: &[u8] = b"\xFD7zXZ\x00";
const RAR: &[u8] = b"Rar!\x1A\x07";
/// Candidate signatures checked in one input before the input counts as an archive anyway.
const CANDIDATES: usize = 4096;
/// Work the checks of one input may do (bytes a header CRC covers, positions a data-descriptor search reads) before
/// the input counts as an archive anyway, so overlapping candidates cannot turn a small input into a long computation.
const WORK_BUDGET: usize = 64 * 1024 * 1024;
/// Methods whose compressed data carries its own end, so a reader that decodes them can find a data descriptor with no
/// signature after it: Deflate, Deflate64, PKWARE DCL Implode, bzip2, LZMA, IBM TERSE, IBM LZ77 z, Zstandard under
/// both its identifiers (20 and 93), MP3, xz, JPEG, WavPack, PPMd, and AES encryption around any of them.
const SELF_ENDING: [u64; 15] = [8, 9, 10, 12, 14, 18, 19, 20, 93, 94, 95, 96, 97, 98, 99];
/// Directory entries checked for each candidate record.
const ENTRIES_CHECKED: usize = 16;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Found {
    None,
    /// An archive whose structure a reader would accept.
    Archive,
    /// More candidate signatures than the recognizer checks.
    TooMany,
}

impl Found {
    pub fn any(self) -> bool {
        self != Self::None
    }
}

fn u16_at(bytes: &[u8], at: usize) -> Option<u64> {
    Some(u16::from_le_bytes(bytes.get(at..at + 2)?.try_into().ok()?) as u64)
}

fn u32_at(bytes: &[u8], at: usize) -> Option<u64> {
    Some(u32::from_le_bytes(bytes.get(at..at + 4)?.try_into().ok()?) as u64)
}

fn u64_at(bytes: &[u8], at: usize) -> Option<u64> {
    Some(u64::from_le_bytes(bytes.get(at..at + 8)?.try_into().ok()?))
}

fn signature_at(bytes: &[u8], at: u64, signature: &[u8]) -> bool {
    usize::try_from(at)
        .ok()
        .and_then(|at| bytes.get(at..at.checked_add(signature.len())?))
        == Some(signature)
}

const CRC_TABLE: [u32; 256] = {
    let mut table = [0u32; 256];
    let mut i = 0;
    while i < 256 {
        let mut crc = i as u32;
        let mut bit = 0;
        while bit < 8 {
            crc = if crc & 1 == 1 {
                (crc >> 1) ^ 0xEDB8_8320
            } else {
                crc >> 1
            };
            bit += 1;
        }
        table[i] = crc;
        i += 1;
    }
    table
};

/// The CRC-32 archive formats use (IEEE 802.3, reflected).
pub fn crc32(data: &[u8]) -> u32 {
    let mut crc = 0xFFFF_FFFFu32;
    for byte in data {
        crc = CRC_TABLE[((crc ^ *byte as u32) & 0xFF) as usize] ^ (crc >> 8);
    }
    !crc
}

/// The work one input's checks may still do.
struct Work {
    left: usize,
}

impl Work {
    /// Takes `n` units from the budget, or None once it is spent.
    fn spend(&mut self, n: usize) -> Option<()> {
        self.left = self.left.checked_sub(n)?;
        Some(())
    }

    /// The CRC of `data`, or None once the budget is spent.
    fn crc(&mut self, data: &[u8]) -> Option<u32> {
        self.spend(data.len())?;
        Some(crc32(data))
    }
}

/// The directory a record describes: where it starts, how long it is, the offset it claims from the archive's start,
/// and how many entries it holds. A ZIP64 record (one of `zip64`, the positions of its signature) replaces the classic
/// fields it marks as too large; it ends where its locator begins, however much extensible data it carries.
fn directory_of(bytes: &[u8], record: usize, zip64: &[usize]) -> Option<(usize, u64, u64, u64)> {
    let entries = u16_at(bytes, record + 10)?;
    let size = u32_at(bytes, record + 12)?;
    let offset = u32_at(bytes, record + 16)?;
    if entries != 0xFFFF && size != 0xFFFF_FFFF && offset != 0xFFFF_FFFF {
        let start = (record as u64).checked_sub(size)?;
        return Some((usize::try_from(start).ok()?, size, offset, entries));
    }
    let locator = record.checked_sub(20)?;
    if bytes.get(locator..locator + 4) != Some(ZIP64_LOCATOR) {
        return None;
    }
    let found = zip64.iter().rev().copied().find(|&at| {
        at < locator
            && u64_at(bytes, at + 4).and_then(|rest| (at as u64).checked_add(12)?.checked_add(rest))
                == Some(locator as u64)
    })?;
    let entries = u64_at(bytes, found + 32)?;
    let size = u64_at(bytes, found + 40)?;
    let offset = u64_at(bytes, found + 48)?;
    let start = (found as u64).checked_sub(size)?;
    Some((usize::try_from(start).ok()?, size, offset, entries))
}
/// A directory entry's local-header offset, from its ZIP64 extra field when the classic field is too large.
fn local_offset(bytes: &[u8], entry: usize) -> Option<u64> {
    let offset = u32_at(bytes, entry + 42)?;
    if offset != 0xFFFF_FFFF {
        return Some(offset);
    }
    let names = u16_at(bytes, entry + 28)? as usize;
    let extra_length = u16_at(bytes, entry + 30)? as usize;
    let extra = bytes.get(entry + 46 + names..entry + 46 + names + extra_length)?;
    let mut at = 0;
    while at + 4 <= extra.len() {
        let id = u16_at(extra, at)?;
        let size = u16_at(extra, at + 2)? as usize;
        let body = extra.get(at + 4..at + 4 + size)?;
        if id == 0x0001 {
            // Present in order, each only when its classic field is the maximum.
            let mut field = 0;
            if u32_at(bytes, entry + 24)? == 0xFFFF_FFFF {
                field += 8;
            }
            if u32_at(bytes, entry + 20)? == 0xFFFF_FFFF {
                field += 8;
            }
            return u64_at(body, field);
        }
        at += 4 + size;
    }
    None
}

/// True when the record's directory starts with a directory entry and the first entries' local headers are where
/// they say, measured from the archive's own start or from the start of the bytes.
fn coherent(bytes: &[u8], record: usize, zip64: &[usize]) -> bool {
    let Some((start, size, offset, entries)) = directory_of(bytes, record, zip64) else {
        return false;
    };
    if entries == 0 || size < 46 || bytes.get(start..start + 4) != Some(CENTRAL) {
        return false;
    }
    let bases: Vec<u64> = [(start as u64).checked_sub(offset), Some(0)]
        .into_iter()
        .flatten()
        .collect();
    let end = start as u64 + size;
    let mut entry = start;
    for _ in 0..entries.min(ENTRIES_CHECKED as u64) {
        if (entry as u64) + 46 > end || bytes.get(entry..entry + 4) != Some(CENTRAL) {
            return false;
        }
        let Some(local) = local_offset(bytes, entry) else {
            return false;
        };
        if bases.iter().any(|base| {
            base.checked_add(local)
                .is_some_and(|at| signature_at(bytes, at, LOCAL))
        }) {
            return true;
        }
        let (Some(names), Some(extra), Some(comment)) = (
            u16_at(bytes, entry + 28),
            u16_at(bytes, entry + 30),
            u16_at(bytes, entry + 32),
        ) else {
            return false;
        };
        entry += 46 + (names + extra + comment) as usize;
    }
    false
}

/// The body of the ZIP64 extra field in an extra area.
fn zip64_field(extra: &[u8]) -> Option<&[u8]> {
    let mut at = 0;
    while at + 4 <= extra.len() {
        let id = u16_at(extra, at)?;
        let size = u16_at(extra, at + 2)? as usize;
        let body = extra.get(at + 4..at + 4 + size)?;
        if id == 0x0001 {
            return Some(body);
        }
        at += 4 + size;
    }
    None
}

/// The compressed sizes a reader may take for a local entry: the header's own unless it is saturated, else those its
/// ZIP64 field gives, read both as the specification lays a local header's field out (original then compressed, both
/// present) and as readers accept it (only the saturated fields, in order).
fn compressed_sizes(compressed: u64, original: u64, extra: &[u8]) -> Vec<u64> {
    if compressed != 0xFFFF_FFFF {
        return vec![compressed];
    }
    let Some(body) = zip64_field(extra) else {
        return Vec::new();
    };
    let offsets: &[usize] = if original == 0xFFFF_FFFF {
        &[8]
    } else {
        &[0, 8]
    };
    offsets.iter().filter_map(|at| u64_at(body, *at)).collect()
}

/// True when a data descriptor whose compressed size reaches back to `data` starts at `at`, with or without its
/// signature and with four- or eight-byte sizes.
fn descriptor_at(bytes: &[u8], at: usize, data: usize) -> bool {
    let length = (at - data) as u64;
    let signed = bytes.get(at..at + 4) == Some(DESCRIPTOR);
    let sizes = if signed { at + 8 } else { at + 4 };
    (sizes + 8 <= bytes.len() && u32_at(bytes, sizes) == Some(length))
        || (sizes + 16 <= bytes.len() && u64_at(bytes, sizes) == Some(length))
}

/// A local entry at `at` that a streaming reader reads to the end of its data: its header, name and extra field inside
/// the bytes, and the end of its data known from the header's sizes, from the sizes in its ZIP64 extra field, or from
/// a data descriptor whose compressed size reaches back to the data's start. A descriptor with its signature counts
/// whatever the method; one without counts after a method that ends its own data. An entry with no data carries
/// nothing and does not count. The version, method and name do not matter otherwise, and neither does the entry's
/// position: a reader opens the entry before it judges them. `claimed` holds the data starts that the signed
/// descriptors in the bytes point back to. None once the budget is spent.
fn local_entry(bytes: &[u8], at: usize, claimed: &HashSet<usize>, work: &mut Work) -> Option<bool> {
    let field = |offset| u16_at(bytes, at + offset);
    let (Some(flags), Some(method), Some(compressed), Some(original), Some(names), Some(extra)) = (
        field(6),
        field(8),
        u32_at(bytes, at + 18),
        u32_at(bytes, at + 22),
        field(26),
        field(28),
    ) else {
        return Some(false);
    };
    let extra_start = at + 30 + names as usize;
    let data = extra_start + extra as usize;
    if data > bytes.len() {
        return Some(false);
    }
    let fits = |size: u64| {
        size > 0
            && (data as u64)
                .checked_add(size)
                .is_some_and(|end| end <= bytes.len() as u64)
    };
    if compressed_sizes(compressed, original, &bytes[extra_start..data])
        .into_iter()
        .any(fits)
    {
        return Some(true);
    }
    // A streaming entry's header sizes may be zero: its data descriptor holds them.
    if flags & 0x0008 == 0 {
        return Some(false);
    }
    if claimed.contains(&data) {
        return Some(true);
    }
    if !SELF_ENDING.contains(&method) {
        return Some(false);
    }
    work.spend(bytes.len() - data)?;
    Some((data + 1..bytes.len()).any(|end| descriptor_at(bytes, end, data)))
}

/// A 7-Zip signature header: its start-header CRC covers the next 20 bytes. None once the budget is spent.
fn seven_zip(bytes: &[u8], at: usize, work: &mut Work) -> Option<bool> {
    match bytes.get(at..at + 32) {
        Some(h) if h[6] == 0 => Some(u32_at(h, 8) == Some(work.crc(&h[12..32])? as u64)),
        _ => Some(false),
    }
}

/// An xz stream header: two flag bytes and their CRC. None once the budget is spent.
fn xz(bytes: &[u8], at: usize, work: &mut Work) -> Option<bool> {
    match bytes.get(at..at + 12) {
        Some(h) if h[6] == 0 && h[7] <= 0x0F => {
            Some(u32_at(h, 8) == Some(work.crc(&h[6..8])? as u64))
        }
        _ => Some(false),
    }
}

/// A RAR 5 variable-length integer: its value and its length.
fn vint(bytes: &[u8], at: usize) -> Option<(u64, usize)> {
    let mut value = 0u64;
    for i in 0..10 {
        let byte = *bytes.get(at + i)?;
        value |= ((byte & 0x7F) as u64) << (7 * i);
        if byte & 0x80 == 0 {
            return Some((value, i + 1));
        }
    }
    None
}

/// A RAR archive after its signature: for RAR 5, the main archive header, whose CRC-32 covers its size field and body;
/// for RAR 4, the archive header after the marker block, whose CRC is the low half of the CRC-32 of its body. A header
/// of any size counts when its CRC matches; the budget, not a size limit, bounds the work. None once it is spent.
fn rar(bytes: &[u8], at: usize, work: &mut Work) -> Option<bool> {
    match bytes.get(at + 6) {
        Some(1) if bytes.get(at + 7) == Some(&0) => {
            let start = at + 8;
            let (Some(expected), Some((size, length))) =
                (u32_at(bytes, start), vint(bytes, start + 4))
            else {
                return Some(false);
            };
            let Some(end) = usize::try_from(size)
                .ok()
                .and_then(|size| (start + 4 + length).checked_add(size))
            else {
                return Some(false);
            };
            let (true, Some(body), Some((1, _))) = (
                size > 0,
                bytes.get(start + 4..end),
                vint(bytes, start + 4 + length),
            ) else {
                return Some(false);
            };
            Some(work.crc(body)? as u64 == expected)
        }
        Some(0) => {
            let header = at + 7;
            let (Some(expected), Some(size)) = (u16_at(bytes, header), u16_at(bytes, header + 5))
            else {
                return Some(false);
            };
            if bytes.get(header + 2) != Some(&0x73) || size < 7 {
                return Some(false);
            }
            let Some(body) = bytes.get(header + 2..header + size as usize) else {
                return Some(false);
            };
            Some((work.crc(body)? & 0xFFFF) as u64 == expected)
        }
        _ => Some(false),
    }
}

/// Whether `bytes` hold an archive a reader could open.
pub fn find(bytes: &[u8]) -> Found {
    let (mut ends, mut zip64, mut locals, mut descriptors, mut others) =
        (Vec::new(), Vec::new(), Vec::new(), Vec::new(), Vec::new());
    let mut candidates = 0usize;
    for at in 0..bytes.len() {
        let rest = &bytes[at..];
        let list = match rest[0] {
            b'P' if rest.starts_with(EOCD) => &mut ends,
            b'P' if rest.starts_with(ZIP64_EOCD) => &mut zip64,
            b'P' if rest.starts_with(LOCAL) => &mut locals,
            b'P' if rest.starts_with(DESCRIPTOR) => &mut descriptors,
            b'7' if rest.starts_with(SEVEN_ZIP) => &mut others,
            0xFD if rest.starts_with(XZ) => &mut others,
            b'R' if rest.starts_with(RAR) => &mut others,
            _ => continue,
        };
        list.push(at);
        candidates += 1;
        if candidates > CANDIDATES {
            return Found::TooMany;
        }
    }
    let mut work = Work { left: WORK_BUDGET };
    let mut claimed = HashSet::new();
    for &at in &descriptors {
        for size in [u32_at(bytes, at + 8), u64_at(bytes, at + 8)]
            .into_iter()
            .flatten()
        {
            if size > 0
                && let Some(start) = (at as u64).checked_sub(size)
            {
                claimed.insert(start as usize);
            }
        }
    }
    for &at in &locals {
        match local_entry(bytes, at, &claimed, &mut work) {
            Some(true) => return Found::Archive,
            Some(false) => {}
            None => return Found::TooMany,
        }
    }
    for &at in &others {
        let found = match bytes[at] {
            b'7' => seven_zip(bytes, at, &mut work),
            0xFD => xz(bytes, at, &mut work),
            _ => rar(bytes, at, &mut work),
        };
        match found {
            Some(true) => return Found::Archive,
            Some(false) => {}
            None => return Found::TooMany,
        }
    }
    if ends.iter().any(|&record| coherent(bytes, record, &zip64)) {
        return Found::Archive;
    }
    Found::None
}

#[cfg(test)]
pub mod tests {
    use super::*;

    /// A stored one-entry archive as a ZIP writer lays it out.
    pub fn archive() -> Vec<u8> {
        let mut out = std::io::Cursor::new(Vec::new());
        {
            let mut writer = zip_writer(&mut out);
            writer("readme.txt", b"an archive inside another file");
        }
        out.into_inner()
    }

    /// A minimal stored-entry ZIP writer (local header, data, directory, end record), so the tests need no crate.
    fn zip_writer(out: &mut std::io::Cursor<Vec<u8>>) -> impl FnMut(&str, &[u8]) + '_ {
        move |name: &str, data: &[u8]| {
            let buffer = out.get_mut();
            let local = buffer.len() as u32;
            let crc = crc32(data);
            buffer.extend_from_slice(LOCAL);
            buffer.extend_from_slice(&[20, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
            buffer.extend_from_slice(&crc.to_le_bytes());
            buffer.extend_from_slice(&(data.len() as u32).to_le_bytes());
            buffer.extend_from_slice(&(data.len() as u32).to_le_bytes());
            buffer.extend_from_slice(&(name.len() as u16).to_le_bytes());
            buffer.extend_from_slice(&[0, 0]);
            buffer.extend_from_slice(name.as_bytes());
            buffer.extend_from_slice(data);
            let directory = buffer.len() as u32;
            buffer.extend_from_slice(CENTRAL);
            buffer.extend_from_slice(&[20, 0, 20, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
            buffer.extend_from_slice(&crc.to_le_bytes());
            buffer.extend_from_slice(&(data.len() as u32).to_le_bytes());
            buffer.extend_from_slice(&(data.len() as u32).to_le_bytes());
            buffer.extend_from_slice(&(name.len() as u16).to_le_bytes());
            buffer.extend_from_slice(&[0; 12]);
            buffer.extend_from_slice(&local.to_le_bytes());
            buffer.extend_from_slice(name.as_bytes());
            let size = buffer.len() as u32 - directory;
            buffer.extend_from_slice(EOCD);
            buffer.extend_from_slice(&[0, 0, 0, 0, 1, 0, 1, 0]);
            buffer.extend_from_slice(&size.to_le_bytes());
            buffer.extend_from_slice(&directory.to_le_bytes());
            buffer.extend_from_slice(&[0, 0]);
        }
    }

    /// The same archive with a ZIP64 end record and locator, its classic record's fields at their maximum.
    pub fn zip64_archive() -> Vec<u8> {
        zip64_with_extensible_data(0)
    }

    fn zip64_with_extensible_data(extensible: usize) -> Vec<u8> {
        let classic = archive();
        let record = classic.len() - 22;
        let size = u32_at(&classic, record + 12).unwrap();
        let offset = u32_at(&classic, record + 16).unwrap();
        let mut out = classic[..record].to_vec();
        let zip64 = out.len() as u64;
        out.extend_from_slice(ZIP64_EOCD);
        out.extend_from_slice(&(44 + extensible as u64).to_le_bytes());
        out.extend_from_slice(&[45, 0, 45, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
        out.extend_from_slice(&1u64.to_le_bytes());
        out.extend_from_slice(&1u64.to_le_bytes());
        out.extend_from_slice(&size.to_le_bytes());
        out.extend_from_slice(&offset.to_le_bytes());
        out.extend(std::iter::repeat_n(0x5A, extensible));
        out.extend_from_slice(ZIP64_LOCATOR);
        out.extend_from_slice(&[0, 0, 0, 0]);
        out.extend_from_slice(&zip64.to_le_bytes());
        out.extend_from_slice(&[1, 0, 0, 0]);
        out.extend_from_slice(EOCD);
        out.extend_from_slice(&[0, 0, 0, 0, 0xFF, 0xFF, 0xFF, 0xFF]);
        out.extend_from_slice(&[0xFF; 8]);
        out.extend_from_slice(&[0, 0]);
        out
    }

    #[test]
    fn an_archive_is_found_alone_appended_or_inside_other_bytes() {
        let zip = archive();
        assert_eq!(find(&zip), Found::Archive);
        let mut appended = b"%PDF-1.7\n%%EOF\n".to_vec();
        appended.extend_from_slice(&zip);
        assert_eq!(find(&appended), Found::Archive);
        let mut inside = vec![0x41; 300];
        inside.extend_from_slice(&zip);
        inside.extend_from_slice(&[0x42; 300]);
        assert_eq!(find(&inside), Found::Archive, "followed by other bytes");
        assert_eq!(find(&zip64_archive()), Found::Archive, "a ZIP64 record");
        let mut zip64_inside = b"stream bytes ".to_vec();
        zip64_inside.extend_from_slice(&zip64_archive());
        zip64_inside.extend_from_slice(b" more");
        assert_eq!(find(&zip64_inside), Found::Archive);
    }

    #[test]
    fn decoy_records_do_not_hide_an_archive() {
        let mut bytes = archive();
        for _ in 0..200 {
            bytes.extend_from_slice(EOCD);
            bytes.extend_from_slice(&[0x11; 18]);
        }
        assert_eq!(find(&bytes), Found::Archive);
        let mut flood = Vec::new();
        for _ in 0..=CANDIDATES {
            flood.extend_from_slice(EOCD);
        }
        assert_eq!(find(&flood), Found::TooMany);
    }

    #[test]
    fn signatures_without_structure_are_not_an_archive() {
        let mut markers = b"text PK\x03\x04 and PK\x05\x06 in a sentence".to_vec();
        markers.extend_from_slice(&[0u8; 64]);
        assert_eq!(find(&markers), Found::None);
        // A record whose directory is not where it says.
        let mut broken = b"leading bytes ".to_vec();
        broken.extend_from_slice(&archive());
        let n = broken.len();
        let directory = n - 22 - (u32_at(&broken, n - 10).unwrap() as usize);
        broken[directory] = b'X';
        // Its local entry's data made to run past the bytes too, so only the end record is left to judge.
        broken[14 + 18..14 + 22].copy_from_slice(&0x7FFF_FFFFu32.to_le_bytes());
        assert_eq!(find(&broken), Found::None);
        assert_eq!(find(b""), Found::None);
        assert_eq!(find(b"PK\x05\x06"), Found::None);
    }

    /// One local entry and its data, without a directory: version 20, the given flags, method, name and data.
    fn local(flags: u16, method: u16, name: &[u8], data: &[u8]) -> Vec<u8> {
        let mut entry = LOCAL.to_vec();
        entry.extend_from_slice(&20u16.to_le_bytes());
        entry.extend_from_slice(&flags.to_le_bytes());
        entry.extend_from_slice(&method.to_le_bytes());
        entry.extend_from_slice(&[0; 4]);
        entry.extend_from_slice(&crc32(data).to_le_bytes());
        entry.extend_from_slice(&(data.len() as u32).to_le_bytes());
        entry.extend_from_slice(&(data.len() as u32).to_le_bytes());
        entry.extend_from_slice(&(name.len() as u16).to_le_bytes());
        entry.extend_from_slice(&[0, 0]);
        entry.extend_from_slice(name);
        entry.extend_from_slice(data);
        entry
    }

    /// `bytes` between other bytes, as a string or a stream would hold them.
    fn inside(bytes: &[u8]) -> Vec<u8> {
        let mut out = b"image data ".to_vec();
        out.extend_from_slice(bytes);
        out.extend_from_slice(b" more content");
        out
    }

    #[test]
    fn a_local_entry_counts_wherever_a_streaming_reader_reads_it() {
        let data = b"an archive inside another file";
        let entry = local(0, 0, b"readme.txt", data);
        assert_eq!(find(&entry), Found::Archive);
        let mut last = b"image data ".to_vec();
        last.extend_from_slice(&entry);
        assert_eq!(
            find(&last),
            Found::Archive,
            "a lone entry that ends the bytes"
        );
        // Its version, method and name do not matter: a reader opens the entry before it judges them.
        let mut version = entry.clone();
        version[4] = 200;
        let long = vec![b'n'; 1_025];
        for unusual in [
            version,
            local(0, 0x4242, b"readme.txt", data),
            local(0, 0, &long, data),
            local(0, 0, b"read\x01me", data),
            local(0, 0, b"", data),
        ] {
            assert_eq!(find(&inside(&unusual)), Found::Archive);
        }
        // A streaming entry: its header sizes zero, its data descriptor after the data.
        let streaming = |method: u16, signed: bool| {
            let mut entry = local(0x0008, method, b"readme.txt", data);
            entry[18..26].copy_from_slice(&[0; 8]);
            if signed {
                entry.extend_from_slice(DESCRIPTOR);
            }
            entry.extend_from_slice(&crc32(data).to_le_bytes());
            entry.extend_from_slice(&(data.len() as u32).to_le_bytes());
            entry.extend_from_slice(&(data.len() as u32).to_le_bytes());
            entry
        };
        assert_eq!(find(&inside(&streaming(0, true))), Found::Archive);
        assert_eq!(find(&inside(&streaming(8, true))), Found::Archive);
        for method in SELF_ENDING {
            assert_eq!(
                find(&inside(&streaming(method as u16, false))),
                Found::Archive,
                "method {method}: a self-ending entry's descriptor needs no signature"
            );
        }
        assert_eq!(
            find(&inside(&streaming(0, false))),
            Found::None,
            "no reader finds the end of stored data without the signature"
        );
        // The flag alone, with no descriptor that reaches back to the data, is not an entry.
        let mut flag_only = local(0x0008, 8, b"readme.txt", data);
        flag_only[18..26].copy_from_slice(&[0; 8]);
        assert_eq!(find(&inside(&flag_only)), Found::None);
        // ZIP64 sizes: from the extra field in either layout, whichever field is saturated, and nothing without it.
        let zip64 = |compressed: bool, original: bool, fields: &[u64]| {
            let mut entry = local(0, 0, b"readme.txt", data);
            if compressed {
                entry[18..22].copy_from_slice(&[0xFF; 4]);
            }
            if original {
                entry[22..26].copy_from_slice(&[0xFF; 4]);
            }
            if !fields.is_empty() {
                let mut extra = 1u16.to_le_bytes().to_vec();
                extra.extend_from_slice(&(8 * fields.len() as u16).to_le_bytes());
                for field in fields {
                    extra.extend_from_slice(&field.to_le_bytes());
                }
                entry[28..30].copy_from_slice(&(extra.len() as u16).to_le_bytes());
                let at = 30 + b"readme.txt".len();
                entry.splice(at..at, extra);
            }
            entry
        };
        let n = data.len() as u64;
        for (compressed, original, fields) in [
            (true, true, vec![n, n]),
            (false, true, vec![5_000_000_000]),
            (true, false, vec![n]),
            (true, false, vec![5_000_000_000, n]),
        ] {
            let entry = zip64(compressed, original, &fields);
            assert_eq!(
                find(&inside(&entry)),
                Found::Archive,
                "{compressed} {original} {fields:?}"
            );
        }
        assert_eq!(find(&inside(&zip64(true, true, &[]))), Found::None);
        // An empty entry, data that runs past the bytes, and a header cut short carry nothing a reader returns.
        assert_eq!(find(&inside(&local(0, 0, b"empty", b""))), Found::None);
        let mut past = entry.clone();
        past[18..22].copy_from_slice(&0x7FFF_FFFFu32.to_le_bytes());
        assert_eq!(find(&inside(&past)), Found::None);
        assert_eq!(find(b"PK\x03\x04short"), Found::None);
    }

    fn seven_zip_header() -> Vec<u8> {
        let mut next = Vec::new();
        next.extend_from_slice(&1234u64.to_le_bytes());
        next.extend_from_slice(&56u64.to_le_bytes());
        next.extend_from_slice(&0xDEAD_BEEFu32.to_le_bytes());
        let mut header = SEVEN_ZIP.to_vec();
        header.extend_from_slice(&[0, 4]);
        header.extend_from_slice(&crc32(&next).to_le_bytes());
        header.extend_from_slice(&next);
        header
    }

    fn rar5_header() -> Vec<u8> {
        let body = [3u8, 1, 0, 0];
        let mut header = b"Rar!\x1A\x07\x01\x00".to_vec();
        header.extend_from_slice(&crc32(&body).to_le_bytes());
        header.extend_from_slice(&body);
        header
    }

    /// A RAR 5 main archive header whose size field claims `size` bytes, padded with an extra area.
    fn rar5_header_of(size: usize) -> Vec<u8> {
        let mut field = Vec::new();
        let mut value = size;
        loop {
            let byte = (value & 0x7F) as u8;
            value >>= 7;
            if value == 0 {
                field.push(byte);
                break;
            }
            field.push(byte | 0x80);
        }
        let mut body = field;
        body.extend_from_slice(&[1, 1, 0]);
        body.resize(body.len() - 3 + size, 0);
        let mut header = b"Rar!\x1A\x07\x01\x00".to_vec();
        header.extend_from_slice(&crc32(&body).to_le_bytes());
        header.extend_from_slice(&body);
        header
    }

    fn rar4_header() -> Vec<u8> {
        let body = [0x73u8, 0, 0, 13, 0, 0, 0, 0, 0, 0, 0];
        let mut header = b"Rar!\x1A\x07\x00".to_vec();
        header.extend_from_slice(&((crc32(&body) & 0xFFFF) as u16).to_le_bytes());
        header.extend_from_slice(&body);
        header
    }

    #[test]
    fn other_archive_formats_count_anywhere_when_their_header_checks() {
        let xz_header = {
            let mut h = XZ.to_vec();
            h.extend_from_slice(&[0, 1]);
            h.extend_from_slice(&crc32(&[0, 1]).to_le_bytes());
            h
        };
        for header in [seven_zip_header(), xz_header, rar5_header(), rar4_header()] {
            assert_eq!(find(&header), Found::Archive);
            let mut appended = b"\xFF\xD8\xFF\xE0 a JPEG's bytes, then ".to_vec();
            appended.extend_from_slice(&header);
            appended.extend_from_slice(b" and the rest of the archive");
            assert_eq!(find(&appended), Found::Archive, "{:?}", &header[..6]);
            // The same signature with a header whose check fails is not an archive.
            let mut broken = appended.clone();
            let last = broken.len() - 29;
            broken[last] ^= 0x55;
            assert_eq!(find(&broken), Found::None, "{:?}", &header[..6]);
        }
    }

    #[test]
    fn overlapping_rar_candidates_spend_a_bounded_budget() {
        // RAR 5 signatures, each claiming the largest header the recognizer reads, overlapping one another.
        let mut flood = Vec::new();
        for _ in 0..2_000 {
            flood.extend_from_slice(b"Rar!\x1A\x07\x01\x00\x00\x00\x00\x00\x80\x80\x04\x01");
        }
        flood.extend(std::iter::repeat_n(0u8, 70 * 1024));
        let started = std::time::Instant::now();
        let found = find(&flood);
        assert!(started.elapsed().as_secs() < 5, "{:?}", started.elapsed());
        assert_ne!(found, Found::Archive);
    }

    #[test]
    fn a_rar5_main_header_of_any_size_counts_when_its_crc_matches() {
        for size in [3, 70 * 1024, 1024 * 1024] {
            let header = rar5_header_of(size);
            assert_eq!(find(&inside(&header)), Found::Archive, "{size}");
            let mut broken = header.clone();
            broken[12 + size / 2] ^= 0x55;
            assert_eq!(find(&inside(&broken)), Found::None, "{size}");
        }
    }

    #[test]
    fn a_zip64_record_far_in_front_of_its_locator_is_found() {
        let far = zip64_with_extensible_data(70 * 1024);
        assert_eq!(find(&far), Found::Archive);
    }
}
