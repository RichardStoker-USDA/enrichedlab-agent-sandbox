//! L0 type detection from the bytes themselves, and the markers of a second format hidden in the same file (an
//! archive inside the file is `zip.rs`'s).
//!
//! Detection reads magic numbers for the types a policy may allow (PDF, PNG, JPEG) and the types worth naming when
//! they arrive instead (ZIP, GIF, HTML, executables, legacy or encrypted Office files). A file that is not one of
//! those is `unknown`. A use case that allows lab datasets then checks a ZIP archive as a workbook and text as a
//! delimited table (`dataset::refine`). Magika can replace this behind the same function once a use case allows
//! text-like types that only a model can tell apart.
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum FileType {
    Pdf,
    Png,
    Jpeg,
    Gif,
    Zip,
    Html,
    Executable,
    /// A delimited text table: comma-separated, tab-separated or another delimiter.
    Delimited,
    /// An Excel workbook in the Office Open XML format.
    Xlsx,
    /// An OLE compound file: an Excel 97-2003 or Word 97-2003 file, or a password-protected Office file.
    Ole,
    Unknown,
}

impl FileType {
    /// Types the disarm layer can rebuild; only these may be allowed by a policy.
    pub fn can_disarm(self) -> bool {
        matches!(
            self,
            Self::Pdf | Self::Png | Self::Jpeg | Self::Delimited | Self::Xlsx
        )
    }

    /// The media type of this type's rebuilt copy: the same type, or for a table its cell grid.
    pub fn rebuilt_media_type(self) -> &'static str {
        match self {
            Self::Delimited | Self::Xlsx => crate::dataset::GRID_MEDIA_TYPE,
            other => other.media_type(),
        }
    }

    /// The forms a lab's table arrives in. Spreadsheet programs save text tables under workbook names and the
    /// reverse, so one may be declared as another.
    fn is_table(self) -> bool {
        matches!(self, Self::Delimited | Self::Xlsx | Self::Ole)
    }

    /// Whether bytes of this type answer a declaration of `declared`: the same type, or any table for a table.
    pub fn satisfies(self, declared: FileType) -> bool {
        self == declared || (self.is_table() && declared.is_table())
    }

    pub fn media_type(self) -> &'static str {
        match self {
            Self::Pdf => "application/pdf",
            Self::Png => "image/png",
            Self::Jpeg => "image/jpeg",
            Self::Gif => "image/gif",
            Self::Zip => "application/zip",
            Self::Html => "text/html",
            Self::Executable => "application/octet-stream",
            Self::Delimited => "text/csv",
            Self::Xlsx => "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet",
            Self::Ole => "application/x-ole-storage",
            Self::Unknown => "application/octet-stream",
        }
    }

    /// The type's short name, as the rules see it in `guard_type`.
    pub fn kind(self) -> &'static str {
        match self {
            Self::Pdf => "pdf",
            Self::Png => "png",
            Self::Jpeg => "jpeg",
            Self::Gif => "gif",
            Self::Zip => "zip",
            Self::Html => "html",
            Self::Executable => "executable",
            Self::Delimited => "delimited",
            Self::Xlsx => "xlsx",
            Self::Ole => "ole",
            Self::Unknown => "unknown",
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Self::Pdf => "PDF",
            Self::Png => "PNG image",
            Self::Jpeg => "JPEG image",
            Self::Gif => "GIF image",
            Self::Zip => "ZIP archive",
            Self::Html => "HTML page",
            Self::Executable => "program",
            Self::Delimited => "delimited text table",
            Self::Xlsx => "Excel workbook",
            Self::Ole => "Office 97-2003 or password-protected Office file",
            Self::Unknown => "unrecognized",
        }
    }

    /// The type's name with its article, for an explanation.
    pub fn described(self) -> &'static str {
        match self {
            Self::Pdf => "a PDF",
            Self::Png => "a PNG image",
            Self::Jpeg => "a JPEG image",
            Self::Gif => "a GIF image",
            Self::Zip => "a ZIP archive",
            Self::Html => "an HTML page",
            Self::Executable => "a program",
            Self::Delimited => "a delimited text table",
            Self::Xlsx => "an Excel workbook",
            Self::Ole => "an Office 97-2003 or password-protected Office file",
            Self::Unknown => "a file of an unrecognized type",
        }
    }

    /// The type a caller declared, from its media type. Parameters after `;` are ignored.
    pub fn from_media_type(value: &str) -> Option<Self> {
        match value
            .split(';')
            .next()
            .unwrap_or("")
            .trim()
            .to_ascii_lowercase()
            .as_str()
        {
            "application/pdf" => Some(Self::Pdf),
            "image/png" => Some(Self::Png),
            "image/jpeg" | "image/jpg" => Some(Self::Jpeg),
            "text/csv" | "text/tab-separated-values" | "application/csv" => Some(Self::Delimited),
            "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet"
            | "application/vnd.ms-excel.sheet.macroenabled.12" => Some(Self::Xlsx),
            "application/vnd.ms-excel" => Some(Self::Ole),
            _ => None,
        }
    }
}

pub const PDF_HEADER: &[u8] = b"%PDF-";
/// The PDF specification lets the header start anywhere in the first 1024 bytes.
const PDF_HEADER_WINDOW: usize = 1024;

pub fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    if needle.is_empty() || haystack.len() < needle.len() {
        return None;
    }
    haystack.windows(needle.len()).position(|w| w == needle)
}

pub fn rfind(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    if needle.is_empty() || haystack.len() < needle.len() {
        return None;
    }
    haystack.windows(needle.len()).rposition(|w| w == needle)
}

fn starts_with_html(bytes: &[u8]) -> bool {
    let head = &bytes[..bytes.len().min(512)];
    let start = head
        .iter()
        .position(|b| !b.is_ascii_whitespace())
        .unwrap_or(head.len());
    let lower: Vec<u8> = head[start..].iter().map(u8::to_ascii_lowercase).collect();
    [
        &b"<!doctype html"[..],
        b"<html",
        b"<script",
        b"<svg",
        b"<?xml",
    ]
    .iter()
    .any(|m| lower.starts_with(m))
}

fn is_executable(bytes: &[u8]) -> bool {
    if bytes.starts_with(b"\x7fELF") {
        return true;
    }
    if bytes.len() >= 4
        && matches!(
            &bytes[..4],
            [0xFE, 0xED, 0xFA, 0xCE | 0xCF] | [0xCE | 0xCF, 0xFA, 0xED, 0xFE]
        )
    {
        return true;
    }
    if bytes.starts_with(b"MZ") && bytes.len() >= 0x40 {
        let offset =
            u32::from_le_bytes([bytes[0x3C], bytes[0x3D], bytes[0x3E], bytes[0x3F]]) as usize;
        return bytes.get(offset..offset + 4) == Some(b"PE\0\0");
    }
    false
}

/// The type the bytes are, and for a PDF the offset of its header.
pub fn detect(bytes: &[u8]) -> (FileType, usize) {
    if bytes.starts_with(&[0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A]) {
        return (FileType::Png, 0);
    }
    if bytes.starts_with(&[0xFF, 0xD8, 0xFF]) {
        return (FileType::Jpeg, 0);
    }
    if bytes.starts_with(b"GIF87a") || bytes.starts_with(b"GIF89a") {
        return (FileType::Gif, 0);
    }
    if is_executable(bytes) {
        return (FileType::Executable, 0);
    }
    if let Some(offset) = find(
        &bytes[..bytes.len().min(PDF_HEADER_WINDOW + PDF_HEADER.len())],
        PDF_HEADER,
    ) {
        return (FileType::Pdf, offset);
    }
    if bytes.starts_with(b"PK\x03\x04") || bytes.starts_with(b"PK\x05\x06") {
        return (FileType::Zip, 0);
    }
    if bytes.starts_with(&[0xD0, 0xCF, 0x11, 0xE0, 0xA1, 0xB1, 0x1A, 0xE1]) {
        return (FileType::Ole, 0);
    }
    if starts_with_html(bytes) {
        return (FileType::Html, 0);
    }
    (FileType::Unknown, 0)
}

/// Bytes before a PDF header that are not whitespace: another format can sit in front of the PDF.
pub fn data_before_header(bytes: &[u8], header_offset: usize) -> bool {
    bytes[..header_offset]
        .iter()
        .any(|b| !b.is_ascii_whitespace() && *b != 0)
}

/// Bytes after the last `%%EOF` marker of a PDF, not counting line ends, spaces and NULs.
pub fn pdf_trailing(bytes: &[u8]) -> u64 {
    match rfind(bytes, b"%%EOF") {
        None => 0,
        Some(at) => bytes[at + 5..]
            .iter()
            .filter(|b| !b.is_ascii_whitespace() && **b != 0)
            .count() as u64,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_each_type_by_its_bytes() {
        assert_eq!(detect(b"%PDF-1.7\n").0, FileType::Pdf);
        assert_eq!(detect(b"   \n%PDF-1.4").1, 4);
        assert_eq!(
            detect(&[0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A, 0]).0,
            FileType::Png
        );
        assert_eq!(detect(&[0xFF, 0xD8, 0xFF, 0xE0]).0, FileType::Jpeg);
        assert_eq!(detect(b"GIF89a....").0, FileType::Gif);
        assert_eq!(detect(b"PK\x03\x04rest").0, FileType::Zip);
        assert_eq!(
            detect(&[0xD0, 0xCF, 0x11, 0xE0, 0xA1, 0xB1, 0x1A, 0xE1, 0]).0,
            FileType::Ole
        );
        assert_eq!(detect(b"id,value\n1,2\n").0, FileType::Unknown);
        assert_eq!(detect(b"\n <html><body>").0, FileType::Html);
        assert_eq!(detect(b"\x7fELF\x02\x01").0, FileType::Executable);
        let mut pe = vec![0u8; 0x80];
        pe[..2].copy_from_slice(b"MZ");
        pe[0x3C] = 0x40;
        pe[0x40..0x44].copy_from_slice(b"PE\0\0");
        assert_eq!(detect(&pe).0, FileType::Executable);
        assert_eq!(detect(b"plain words").0, FileType::Unknown);
        let mut late = vec![b' '; 1100];
        late.extend_from_slice(b"%PDF-1.7");
        assert_eq!(
            detect(&late).0,
            FileType::Unknown,
            "a header past 1024 bytes is not a PDF header"
        );
    }

    #[test]
    fn declared_types_come_from_media_types() {
        assert_eq!(
            FileType::from_media_type("application/pdf"),
            Some(FileType::Pdf)
        );
        assert_eq!(
            FileType::from_media_type("image/JPEG; q=1"),
            Some(FileType::Jpeg)
        );
        assert_eq!(
            FileType::from_media_type("text/csv; charset=utf-8"),
            Some(FileType::Delimited)
        );
        assert_eq!(
            FileType::from_media_type(
                "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet"
            ),
            Some(FileType::Xlsx)
        );
        assert_eq!(
            FileType::from_media_type("application/vnd.ms-excel"),
            Some(FileType::Ole)
        );
        assert_eq!(FileType::from_media_type("application/zip"), None);
    }

    #[test]
    fn a_table_answers_any_table_declaration_and_nothing_else_does() {
        assert!(FileType::Delimited.satisfies(FileType::Xlsx));
        assert!(FileType::Delimited.satisfies(FileType::Ole));
        assert!(FileType::Xlsx.satisfies(FileType::Delimited));
        assert!(FileType::Pdf.satisfies(FileType::Pdf));
        assert!(!FileType::Pdf.satisfies(FileType::Png));
        assert!(!FileType::Delimited.satisfies(FileType::Pdf));
        assert!(!FileType::Pdf.satisfies(FileType::Delimited));
    }

    #[test]
    fn trailing_bytes_and_leading_data_are_counted() {
        assert_eq!(pdf_trailing(b"%PDF-1.7\n%%EOF\r\n\0 "), 0);
        assert_eq!(pdf_trailing(b"%PDF-1.7\n%%EOF\nXYZ"), 3);
        assert!(data_before_header(b"<html>%PDF-1.7", 6));
        assert!(!data_before_header(b"  \n%PDF-1.7", 3));
    }
}
