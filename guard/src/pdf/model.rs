//! What a check reports: encryption, active content, hidden layers, stream roles and rebuilt files.
use super::*;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Encryption {
    None,
    OwnerOnly,
    Password,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Deserialize, Serialize)]
pub struct Active {
    pub javascript: u64,
    pub launch: u64,
    pub embedded_file: u64,
    pub open_action: u64,
    pub additional_actions: u64,
    pub xfa: u64,
    pub rich_media: u64,
    pub form_submit: u64,
    pub remote_goto: u64,
}

impl Active {
    pub fn total(&self) -> u64 {
        self.javascript
            + self.launch
            + self.embedded_file
            + self.open_action
            + self.additional_actions
            + self.xfa
            + self.rich_media
            + self.form_submit
            + self.remote_goto
    }

    pub fn codes(&self) -> Vec<(&'static str, u64)> {
        [
            ("active_javascript", self.javascript),
            ("active_launch", self.launch),
            ("active_embedded_file", self.embedded_file),
            ("active_open_action", self.open_action),
            ("active_additional_actions", self.additional_actions),
            ("active_xfa", self.xfa),
            ("active_rich_media", self.rich_media),
            ("active_form_submit", self.form_submit),
            ("active_remote_goto", self.remote_goto),
        ]
        .into_iter()
        .filter(|(_, n)| *n > 0)
        .collect()
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Deserialize, Serialize)]
pub struct Hidden {
    pub invisible: u64,
    pub white: u64,
    pub tiny: u64,
    pub offpage: u64,
    pub truncated: bool,
}

/// What a stream is for, from the keys that reach it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Role {
    Content,
    Font,
    Image,
    Data,
    Metadata,
    Script,
    Launch,
    Embedded,
    Structure,
    /// Strings and names in the file's objects; not a stream.
    String,
    Unknown,
}

impl Role {
    pub fn name(self) -> &'static str {
        match self {
            Self::Content => "content",
            Self::Font => "font",
            Self::Image => "image",
            Self::Data => "data",
            Self::Metadata => "metadata",
            Self::Script => "script",
            Self::Launch => "launch",
            Self::Embedded => "embedded",
            Self::Structure => "structure",
            Self::String => "string",
            Self::Unknown => "unknown",
        }
    }

    /// How a finding names where a rule matched.
    pub fn words(self) -> &'static str {
        match self {
            Self::Content => "page content",
            Self::Font => "an embedded font",
            Self::Image => "an image",
            Self::Data => "color, function or font-map data",
            Self::Metadata => "document metadata",
            Self::Script => "document JavaScript",
            Self::Launch => "a launch action",
            Self::Embedded => "an attached file",
            Self::Structure => "the file's object structure",
            Self::String => "a string or name in the file's objects",
            Self::Unknown => "data no page uses",
        }
    }

    /// Streams a viewer needs to draw the document; the rebuild keeps only these. Document metadata is not one.
    pub fn kept(self) -> bool {
        matches!(self, Self::Content | Self::Font | Self::Image | Self::Data)
    }
}

/// The streams of one document, by what the scan could do with them.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Deserialize, Serialize)]
pub struct Streams {
    pub total: u64,
    /// Streams nothing draws or runs: private data, leftovers, or a hidden payload.
    pub unknown: u64,
    /// Streams the scan could not read: an unknown filter, an unreadable image header, or past the size budget.
    pub unscanned: u64,
    /// Images whose declared or encoded size passes what a decoder may be asked to allocate.
    #[serde(default)]
    pub oversized_images: u64,
    pub decoded_bytes: u64,
    /// Streams, inline images and strings that hold an archive a reader could open.
    #[serde(default)]
    pub archives: u64,
    /// Images drawn inline in content.
    #[serde(default)]
    pub inline_images: u64,
    /// Operators, operands and delimiters in content that no viewer draws with; the rebuild leaves them out.
    #[serde(default)]
    pub content_dropped: u64,
}

/// A rule that matched decoded streams or action text of one use, and how many times.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
pub struct StreamMatch {
    pub rule: String,
    pub severity: Severity,
    pub role: Role,
    pub count: u64,
}

/// The worker's report on one normalized or rebuilt PDF.
#[derive(Clone, Debug, Default, PartialEq, Eq, Deserialize, Serialize)]
pub struct Report {
    pub rules_version: String,
    pub pages: usize,
    pub active: Active,
    pub hidden_layers: bool,
    pub metadata_unicode: UnicodeCounts,
    pub metadata_text: bool,
    /// The stream-use walk read every drawn content stream within its budgets.
    #[serde(default)]
    pub walk_complete: bool,
    pub streams: Streams,
    pub matches: Vec<StreamMatch>,
}

#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
pub struct Stripped {
    pub removed_streams: u64,
    /// Content streams written again in canonical form.
    pub rewritten_streams: u64,
}

#[derive(Clone, Debug)]
pub struct Inspection {
    pub encryption: Encryption,
    /// The file was normalized and its objects read.
    pub readable: bool,
    /// The object worker did not finish within its limits.
    pub failed: bool,
    pub hidden: Hidden,
    pub unicode: UnicodeCounts,
    pub text_sha256: Option<String>,
    pub report: Report,
}

#[derive(Clone, Debug)]
pub struct Rebuilt {
    pub bytes: Vec<u8>,
    pub report: Report,
    pub text_sha256: Option<String>,
    /// Every page was drawn within the helper limits.
    pub rendered: bool,
}
