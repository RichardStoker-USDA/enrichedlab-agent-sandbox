//! Invisible Unicode in text and metadata: tag characters (which can spell hidden ASCII instructions), bidirectional
//! controls (which reorder what a reader sees) and zero-width characters. Counts only; the text is never logged.
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Deserialize, Serialize)]
pub struct UnicodeCounts {
    pub tags: u64,
    pub bidi: u64,
    pub zero_width: u64,
}

impl UnicodeCounts {
    pub fn add(&mut self, other: UnicodeCounts) {
        self.tags += other.tags;
        self.bidi += other.bidi;
        self.zero_width += other.zero_width;
    }
}

pub fn count(text: &str) -> UnicodeCounts {
    let mut counts = UnicodeCounts::default();
    for (i, c) in text.char_indices() {
        match c as u32 {
            0xE0000..=0xE007F => counts.tags += 1,
            0x202A..=0x202E | 0x2066..=0x2069 | 0x200E | 0x200F | 0x061C => counts.bidi += 1,
            0x200B..=0x200D | 0x2060 | 0x180E => counts.zero_width += 1,
            // A byte-order mark is expected only at the start.
            0xFEFF if i > 0 => counts.zero_width += 1,
            _ => {}
        }
    }
    counts
}

/// A PDF text string: UTF-16BE after a byte-order mark, UTF-8 after its mark, otherwise PDFDocEncoding, read here as
/// Latin-1 (they differ only in a few punctuation positions, none of which are counted above).
pub fn pdf_string(bytes: &[u8]) -> String {
    if let Some(rest) = bytes.strip_prefix(&[0xFE, 0xFF]) {
        let units: Vec<u16> = rest
            .as_chunks::<2>()
            .0
            .iter()
            .map(|p| u16::from_be_bytes(*p))
            .collect();
        return String::from_utf16_lossy(&units);
    }
    if let Some(rest) = bytes.strip_prefix(&[0xEF, 0xBB, 0xBF]) {
        return String::from_utf8_lossy(rest).into_owned();
    }
    bytes.iter().map(|&b| b as char).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn counts_each_kind() {
        let hidden: String = "ignore"
            .chars()
            .map(|c| char::from_u32(0xE0000 + c as u32).unwrap())
            .collect();
        let text = format!("A\u{200B}B\u{202E}C{hidden}\u{FEFF}");
        assert_eq!(
            count(&text),
            UnicodeCounts {
                tags: 6,
                bidi: 1,
                zero_width: 2
            }
        );
        assert_eq!(count("\u{FEFF}plain"), UnicodeCounts::default());
        assert_eq!(count("µg/dL, β-carotene, 𝑥²"), UnicodeCounts::default());
    }

    #[test]
    fn decodes_pdf_strings() {
        assert_eq!(
            pdf_string(&[0xFE, 0xFF, 0x00, 0x41, 0x20, 0x0B]),
            "A\u{200B}"
        );
        assert_eq!(pdf_string(b"Title"), "Title");
        assert_eq!(pdf_string(&[0xEF, 0xBB, 0xBF, b'x']), "x");
    }
}
