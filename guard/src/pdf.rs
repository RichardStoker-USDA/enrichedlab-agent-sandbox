//! PDF checks (L0, L1) and rebuild (L2).
//!
//! The service process never parses a PDF. qpdf reports encryption, repairs and normalizes the file and finally
//! rewrites the rebuilt copy; pdftotext supplies the text whose invisible Unicode is counted and whose digest shows
//! whether the rebuilt copy reads the same as the original; and the guard's own binary, started as a worker
//! (`enrichedlab-guard worker ...`, see `worker.rs`), reads the normalized objects with lopdf and their content
//! streams with the guard's own lexer (`content.rs`). All three run as helper processes under address-space, CPU and
//! time limits, so a hostile file can exhaust only its helper.
//!
//! The worker counts active content and hidden text. It sorts every stream by the use the document makes of it: a walk
//! reads each page's content, follows each resource an operation draws with, by its category and in the resource
//! dictionary of the content that names it, and reads every form, pattern, glyph and appearance it reaches the same
//! way, until nothing new is reached; a walk that runs out of budget holds the file. A stream nothing draws is
//! `unknown`. Each stream, inline image and string is decoded within a byte budget and scanned with the rules and the
//! archive recognizer (`zip.rs`), as is each action's script or launch text, and every image a viewer would decode is
//! bounded, alone and together. On rebuild the worker removes the active content, document metadata and every stream
//! nothing draws, and writes every kept content stream again in canonical form. Every count is metadata: no text from
//! the file is kept or logged.
use crate::content::{self, Category, Item, Operand};

use crate::rules::{Rules, Scope, Severity};

use crate::subprocess::{self, Limits};

use crate::text::{self, UnicodeCounts};

use crate::zip;

use anyhow::{Context, Result, anyhow, bail};

use lopdf::{Dictionary, Document, LoadOptions, Object, ObjectId, Stream};

use serde::de::DeserializeOwned;

use serde::{Deserialize, Serialize};

use sha2::{Digest, Sha256};

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet, VecDeque};

use std::ffi::OsStr;

use std::path::{Path, PathBuf};

mod images;
mod model;
mod objects;
mod page_text;
mod payloads;
mod strip;
#[cfg(test)]
mod tests;
mod tools;
mod walker;
mod worker;

pub use images::*;
pub use model::*;
pub use objects::*;
pub use page_text::*;
pub use payloads::*;
pub use strip::*;
pub use tools::*;
pub use walker::*;
pub use worker::*;

/// Bytes any one stream may decompress to, while loading and while scanning.
const STREAM_LIMIT: usize = 256 * 1024 * 1024;

/// The longest name a PDF may hold (PDF 1.7, Annex C): the longest `direct_values` writes in place of a reference.
const NAME_LIMIT: usize = 127;

/// The most elements an array of plain values may have for `direct_values` to write it in place of a reference.
const ARRAY_LIMIT: usize = 32;

/// Bytes `direct_values` may add to a document's objects before the document is refused as unreadable.
const DIRECT_LIMIT: usize = 64 * 1024 * 1024;

/// Decoded bytes the scan of one document may read across all its streams.
const DECODE_BUDGET: usize = 1024 * 1024 * 1024;

/// Page and form content the hidden-text count reads in one document.
const CONTENT_BUDGET: usize = 32 * 1024 * 1024;

const OPERATION_BUDGET: u64 = 5_000_000;

const PAGE_LIMIT: usize = 10_000;

const FORM_DEPTH: usize = 8;

/// Nesting the walks follow. lopdf parses at most 100 levels of direct nesting, so a walk that reaches this depth has
/// met an object built some other way, and the walk is incomplete rather than silently short.
const WALK_DEPTH: usize = 128;

/// Objects the stream-use walk may enter in one document.
const WALK_STEPS: usize = 2_000_000;

/// Action text scanned per action.
const ACTION_TEXT_LIMIT: usize = 16 * 1024 * 1024;

/// Content streams (pages, forms, patterns, glyphs, appearances, each in its scope) the walk reads in one document.
const SCOPE_LIMIT: usize = 200_000;

/// Decoded content bytes the walk reads in one document.
const CONTENT_LIMIT: usize = 256 * 1024 * 1024;

/// Bytes of strings and names the scan reads in one document.
const STRINGS_LIMIT: usize = 64 * 1024 * 1024;

/// Text may sit this far outside the page box before it counts as off the page.
const PAGE_MARGIN: f64 = 36.0;

const DANGEROUS_ACTIONS: &[&[u8]] = &[
    b"JavaScript",
    b"Launch",
    b"SubmitForm",
    b"ImportData",
    b"GoToR",
    b"GoToE",
    b"Rendition",
    b"Movie",
    b"Sound",
    b"RichMediaExecute",
];

const MEDIA_ANNOTATIONS: &[&[u8]] = &[b"RichMedia", b"Screen", b"Movie", b"Sound", b"3D"];

/// Active-content entries whose streams are payloads: scripts and form logic, attached files and media.
const PAYLOAD_KEYS: &[(&[u8], Role)] = &[
    (b"JS", Role::Script),
    (b"XFA", Role::Script),
    (b"EF", Role::Embedded),
    (b"RichMediaContent", Role::Embedded),
    (b"3DD", Role::Embedded),
    (b"Sound", Role::Embedded),
    (b"Movie", Role::Embedded),
    (b"Rendition", Role::Embedded),
];

/// Filters that only compress or encode; the scan undoes them.
const GENERAL_FILTERS: &[&[u8]] = &[
    b"FlateDecode",
    b"LZWDecode",
    b"ASCII85Decode",
    b"ASCIIHexDecode",
    b"RunLengthDecode",
    b"BrotliDecode",
];

/// Image codecs; the scan reads their input as it is.
const IMAGE_CODECS: &[&[u8]] = &[
    b"DCTDecode",
    b"JPXDecode",
    b"JBIG2Decode",
    b"CCITTFaxDecode",
];

// Hidden text: a small interpreter for the graphics and text state of each page and form.

#[derive(Clone, Copy, Debug, PartialEq)]
struct Matrix([f64; 6]);

impl Matrix {
    const IDENTITY: Matrix = Matrix([1.0, 0.0, 0.0, 1.0, 0.0, 0.0]);

    /// `self` applied first, then `other` (PDF row-vector convention).
    fn then(self, other: Matrix) -> Matrix {
        let [a1, b1, c1, d1, e1, f1] = self.0;
        let [a2, b2, c2, d2, e2, f2] = other.0;
        Matrix([
            a1 * a2 + b1 * c2,
            a1 * b2 + b1 * d2,
            c1 * a2 + d1 * c2,
            c1 * b2 + d1 * d2,
            e1 * a2 + f1 * c2 + e2,
            e1 * b2 + f1 * d2 + f2,
        ])
    }

    fn scale(self) -> f64 {
        let [a, b, c, d, ..] = self.0;
        (a * d - b * c).abs().sqrt()
    }
}
