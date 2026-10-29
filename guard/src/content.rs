//! PDF content streams, read by the guard's own lexer.
//!
//! lopdf's content parser drops an inline image it cannot size (any filter, or a named color space) and stops quietly
//! at the first operation it cannot read, so it can neither see every image a page draws nor rewrite a stream
//! faithfully. `parse` reads every token. Each operation keeps exactly the operands its operator takes, of the types
//! the PDF specification gives it; an operator the specification does not define, operands no operator takes, and
//! stray delimiters are left out and counted, as a viewer skips them. Each inline image keeps its dictionary and data.
//! `encode` writes the result back in one canonical form, so a rebuilt content stream carries no comment, no unused
//! operand and no byte between tokens. Numbers keep the form they were written in, so a rewritten stream draws at
//! exactly the same coordinates.
//!
//! `uses` names the resource each operation draws with, by category, which is how the stream-use walk decides what
//! a page draws.

/// Operations one document's content may hold, across every stream the walk reads.
pub const OPERATION_LIMIT: u64 = 20_000_000;
/// Bytes one inline image may hold.
const INLINE_LIMIT: usize = 16 * 1024 * 1024;
/// Nesting of arrays and dictionaries inside one operand.
const NESTING: usize = 32;
/// Bytes after an `EI` candidate that must read as content for the candidate to end the image.
const AFTER_EI: usize = 64;

#[derive(Clone, Debug, PartialEq)]
pub enum Operand {
    /// The number as written, with a doubled sign folded into one.
    Number(String),
    Name(Vec<u8>),
    String(Vec<u8>),
    Bool(bool),
    Null,
    Array(Vec<Operand>),
    Dict(Vec<(Vec<u8>, Operand)>),
}

impl Operand {
    pub fn name(&self) -> Option<&[u8]> {
        match self {
            Self::Name(name) => Some(name),
            _ => None,
        }
    }

    pub fn number(&self) -> Option<f64> {
        match self {
            Self::Number(text) => text.parse().ok(),
            _ => None,
        }
    }

    pub fn integer(&self) -> Option<i64> {
        match self {
            Self::Number(text) => text.parse().ok(),
            _ => None,
        }
    }

    fn is_number(&self) -> bool {
        matches!(self, Self::Number(_))
    }

    /// The same value as a lopdf object, for code that reads operands that way.
    pub fn to_object(&self) -> lopdf::Object {
        use lopdf::Object;
        match self {
            Self::Number(text) => match text.parse::<i64>() {
                Ok(value) => Object::Integer(value),
                Err(_) => Object::Real(text.parse().unwrap_or(0.0)),
            },
            Self::Name(name) => Object::Name(name.clone()),
            Self::String(bytes) => Object::String(bytes.clone(), lopdf::StringFormat::Literal),
            Self::Bool(value) => Object::Boolean(*value),
            Self::Null => Object::Null,
            Self::Array(items) => Object::Array(items.iter().map(Self::to_object).collect()),
            Self::Dict(entries) => {
                let mut dict = lopdf::Dictionary::new();
                for (key, value) in entries {
                    dict.set(key.clone(), value.to_object());
                }
                Object::Dictionary(dict)
            }
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Operation {
    pub operator: &'static str,
    pub operands: Vec<Operand>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct InlineImage {
    /// The image dictionary's entries, keys as written (abbreviated or full).
    pub entries: Vec<(Vec<u8>, Operand)>,
    pub data: Vec<u8>,
}

impl InlineImage {
    /// An entry by its abbreviated or full key.
    pub fn get(&self, short: &[u8], long: &[u8]) -> Option<&Operand> {
        self.entries
            .iter()
            .find(|(key, _)| key == short || key == long)
            .map(|(_, value)| value)
    }

    /// The filter names, abbreviations expanded.
    pub fn filters(&self) -> Vec<Vec<u8>> {
        let names: Vec<&[u8]> = match self.get(b"F", b"Filter") {
            Some(Operand::Name(name)) => vec![name],
            Some(Operand::Array(items)) => items.iter().filter_map(Operand::name).collect(),
            _ => Vec::new(),
        };
        names
            .into_iter()
            .map(|n| expand_filter(n).to_vec())
            .collect()
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum Item {
    Op(Operation),
    Image(InlineImage),
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Parsed {
    pub items: Vec<Item>,
    /// Operators, operands and delimiters left out.
    pub dropped: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Error {
    /// An inline image whose end cannot be found, or which passes the size limit.
    InlineImage,
    /// More operations than the document's budget.
    Budget,
}

/// The resource categories content draws with.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Category {
    XObject,
    Font,
    ExtGState,
    Shading,
    ColorSpace,
    Pattern,
    Properties,
}

impl Category {
    pub fn key(self) -> &'static [u8] {
        match self {
            Self::XObject => b"XObject",
            Self::Font => b"Font",
            Self::ExtGState => b"ExtGState",
            Self::Shading => b"Shading",
            Self::ColorSpace => b"ColorSpace",
            Self::Pattern => b"Pattern",
            Self::Properties => b"Properties",
        }
    }
}

/// What an operator takes, after the operands nearest it.
#[derive(Clone, Copy)]
enum Shape {
    Numbers(usize),
    /// One to 32 numbers (`sc`, `SC`).
    Color,
    /// One to 32 numbers, a pattern name, or both (`scn`, `SCN`).
    ColorOrPattern,
    Name,
    /// A font name and a size (`Tf`).
    Font,
    /// A string (`Tj`, `'`).
    Text,
    /// An array of strings and numbers (`TJ`).
    TextArray,
    /// Two numbers and a string (`"`).
    SpacedText,
    /// A dash array and a phase (`d`).
    Dash,
    /// A tag and a property list, named or inline (`BDC`, `DP`).
    Tagged,
}

const OPERATORS: &[(&str, Shape)] = &[
    ("b", Shape::Numbers(0)),
    ("B", Shape::Numbers(0)),
    ("b*", Shape::Numbers(0)),
    ("B*", Shape::Numbers(0)),
    ("BDC", Shape::Tagged),
    ("BMC", Shape::Name),
    ("BT", Shape::Numbers(0)),
    ("BX", Shape::Numbers(0)),
    ("c", Shape::Numbers(6)),
    ("cm", Shape::Numbers(6)),
    ("CS", Shape::Name),
    ("cs", Shape::Name),
    ("d", Shape::Dash),
    ("d0", Shape::Numbers(2)),
    ("d1", Shape::Numbers(6)),
    ("Do", Shape::Name),
    ("DP", Shape::Tagged),
    ("EMC", Shape::Numbers(0)),
    ("ET", Shape::Numbers(0)),
    ("EX", Shape::Numbers(0)),
    ("f", Shape::Numbers(0)),
    ("F", Shape::Numbers(0)),
    ("f*", Shape::Numbers(0)),
    ("G", Shape::Numbers(1)),
    ("g", Shape::Numbers(1)),
    ("gs", Shape::Name),
    ("h", Shape::Numbers(0)),
    ("i", Shape::Numbers(1)),
    ("j", Shape::Numbers(1)),
    ("J", Shape::Numbers(1)),
    ("K", Shape::Numbers(4)),
    ("k", Shape::Numbers(4)),
    ("l", Shape::Numbers(2)),
    ("m", Shape::Numbers(2)),
    ("M", Shape::Numbers(1)),
    ("MP", Shape::Name),
    ("n", Shape::Numbers(0)),
    ("q", Shape::Numbers(0)),
    ("Q", Shape::Numbers(0)),
    ("re", Shape::Numbers(4)),
    ("RG", Shape::Numbers(3)),
    ("rg", Shape::Numbers(3)),
    ("ri", Shape::Name),
    ("s", Shape::Numbers(0)),
    ("S", Shape::Numbers(0)),
    ("SC", Shape::Color),
    ("sc", Shape::Color),
    ("SCN", Shape::ColorOrPattern),
    ("scn", Shape::ColorOrPattern),
    ("sh", Shape::Name),
    ("T*", Shape::Numbers(0)),
    ("Tc", Shape::Numbers(1)),
    ("Td", Shape::Numbers(2)),
    ("TD", Shape::Numbers(2)),
    ("Tf", Shape::Font),
    ("TJ", Shape::TextArray),
    ("Tj", Shape::Text),
    ("TL", Shape::Numbers(1)),
    ("Tm", Shape::Numbers(6)),
    ("Tr", Shape::Numbers(1)),
    ("Ts", Shape::Numbers(1)),
    ("Tw", Shape::Numbers(1)),
    ("Tz", Shape::Numbers(1)),
    ("v", Shape::Numbers(4)),
    ("w", Shape::Numbers(1)),
    ("W", Shape::Numbers(0)),
    ("W*", Shape::Numbers(0)),
    ("y", Shape::Numbers(4)),
    ("'", Shape::Text),
    ("\"", Shape::SpacedText),
];

/// The operands an operator keeps from the stack, nearest last, or None when the stack does not hold them.
fn take(shape: Shape, stack: &[Operand]) -> Option<usize> {
    let last = |n: usize| stack.len().checked_sub(n).map(|_| n);
    let trailing_numbers = |limit: usize| {
        stack
            .iter()
            .rev()
            .take(limit)
            .take_while(|o| o.is_number())
            .count()
    };
    let top = stack.last();
    match shape {
        Shape::Numbers(n) => {
            last(n).filter(|&n| stack[stack.len() - n..].iter().all(Operand::is_number))
        }
        Shape::Color => Some(trailing_numbers(32)).filter(|&n| n > 0),
        Shape::ColorOrPattern => {
            if matches!(top, Some(Operand::Name(_))) {
                let before = &stack[..stack.len() - 1];
                Some(
                    1 + before
                        .iter()
                        .rev()
                        .take(32)
                        .take_while(|o| o.is_number())
                        .count(),
                )
            } else {
                Some(trailing_numbers(32)).filter(|&n| n > 0)
            }
        }
        Shape::Name => matches!(top, Some(Operand::Name(_))).then_some(1),
        Shape::Font => (stack.len() >= 2
            && matches!(stack[stack.len() - 2], Operand::Name(_))
            && stack[stack.len() - 1].is_number())
        .then_some(2),
        Shape::Text => matches!(top, Some(Operand::String(_))).then_some(1),
        Shape::TextArray => match top {
            Some(Operand::Array(items))
                if items
                    .iter()
                    .all(|i| matches!(i, Operand::String(_) | Operand::Number(_))) =>
            {
                Some(1)
            }
            _ => None,
        },
        Shape::SpacedText => (stack.len() >= 3
            && stack[stack.len() - 3].is_number()
            && stack[stack.len() - 2].is_number()
            && matches!(stack[stack.len() - 1], Operand::String(_)))
        .then_some(3),
        Shape::Dash => (stack.len() >= 2
            && matches!(&stack[stack.len() - 2], Operand::Array(items) if items.iter().all(Operand::is_number))
            && stack[stack.len() - 1].is_number())
        .then_some(2),
        Shape::Tagged => (stack.len() >= 2
            && matches!(stack[stack.len() - 2], Operand::Name(_))
            && matches!(stack[stack.len() - 1], Operand::Name(_) | Operand::Dict(_)))
        .then_some(2),
    }
}

fn operator(keyword: &[u8]) -> Option<(&'static str, Shape)> {
    OPERATORS
        .iter()
        .find(|(name, _)| name.as_bytes() == keyword)
        .copied()
}

fn is_white(byte: u8) -> bool {
    matches!(byte, b'\0' | b'\t' | b'\n' | 0x0C | b'\r' | b' ')
}

fn is_delimiter(byte: u8) -> bool {
    matches!(
        byte,
        b'(' | b')' | b'<' | b'>' | b'[' | b']' | b'{' | b'}' | b'/' | b'%'
    )
}

fn is_regular(byte: u8) -> bool {
    !is_white(byte) && !is_delimiter(byte)
}

/// A number as PDF writes one: a sign, digits and at most one point, at least one digit. A doubled minus, which
/// some writers emit and every viewer reads as one, becomes one.
fn number(token: &[u8]) -> Option<String> {
    let signs = token
        .iter()
        .take_while(|b| matches!(b, b'+' | b'-'))
        .count();
    let body = &token[signs..];
    let negative = token[..signs].contains(&b'-');
    let digits = body.iter().filter(|b| b.is_ascii_digit()).count();
    let points = body.iter().filter(|b| **b == b'.').count();
    if signs > 2 || digits == 0 || points > 1 || digits + points != body.len() {
        return None;
    }
    let mut out = String::with_capacity(body.len() + 1);
    if negative {
        out.push('-');
    }
    out.push_str(std::str::from_utf8(body).ok()?);
    Some(out)
}

#[derive(Debug)]
enum Token<'a> {
    Number(String),
    Name(Vec<u8>),
    String(Vec<u8>),
    ArrayOpen,
    ArrayClose,
    DictOpen,
    DictClose,
    Keyword(&'a [u8]),
    /// A stray `)`, `>`, `{` or `}`, or a string or array the bytes end inside.
    Junk,
}

struct Lexer<'a> {
    bytes: &'a [u8],
    at: usize,
}

impl<'a> Lexer<'a> {
    fn skip_space(&mut self) {
        while self.at < self.bytes.len() {
            match self.bytes[self.at] {
                b if is_white(b) => self.at += 1,
                b'%' => {
                    while self.at < self.bytes.len()
                        && !matches!(self.bytes[self.at], b'\r' | b'\n')
                    {
                        self.at += 1;
                    }
                }
                _ => break,
            }
        }
    }

    fn name(&mut self) -> Vec<u8> {
        let mut out = Vec::new();
        while self.at < self.bytes.len() && is_regular(self.bytes[self.at]) {
            let byte = self.bytes[self.at];
            if byte == b'#'
                && let Some(hex) = self.bytes.get(self.at + 1..self.at + 3)
                && let Ok(text) = std::str::from_utf8(hex)
                && let Ok(value) = u8::from_str_radix(text, 16)
            {
                out.push(value);
                self.at += 3;
                continue;
            }
            out.push(byte);
            self.at += 1;
        }
        out
    }

    /// A literal string after its `(`; None when the bytes end inside it.
    fn literal(&mut self) -> Option<Vec<u8>> {
        let mut out = Vec::new();
        let mut depth = 1usize;
        while self.at < self.bytes.len() {
            let byte = self.bytes[self.at];
            self.at += 1;
            match byte {
                b'\\' => {
                    let &next = self.bytes.get(self.at)?;
                    self.at += 1;
                    match next {
                        b'n' => out.push(b'\n'),
                        b'r' => out.push(b'\r'),
                        b't' => out.push(b'\t'),
                        b'b' => out.push(0x08),
                        b'f' => out.push(0x0C),
                        b'0'..=b'7' => {
                            let mut value = (next - b'0') as u32;
                            for _ in 0..2 {
                                match self.bytes.get(self.at) {
                                    Some(d @ b'0'..=b'7') => {
                                        value = value * 8 + (d - b'0') as u32;
                                        self.at += 1;
                                    }
                                    _ => break,
                                }
                            }
                            out.push((value & 0xFF) as u8);
                        }
                        // A backslash before a line end continues the string on the next line.
                        b'\r' => {
                            if self.bytes.get(self.at) == Some(&b'\n') {
                                self.at += 1;
                            }
                        }
                        b'\n' => {}
                        other => out.push(other),
                    }
                }
                b'(' => {
                    depth += 1;
                    out.push(byte);
                }
                b')' => {
                    depth -= 1;
                    if depth == 0 {
                        return Some(out);
                    }
                    out.push(byte);
                }
                _ => out.push(byte),
            }
        }
        None
    }

    /// A hexadecimal string after its `<`; characters that are not hex digits are skipped, as viewers do.
    fn hex(&mut self) -> Option<Vec<u8>> {
        let mut digits = Vec::new();
        while self.at < self.bytes.len() {
            let byte = self.bytes[self.at];
            self.at += 1;
            if byte == b'>' {
                if digits.len() % 2 == 1 {
                    digits.push(b'0');
                }
                return Some(
                    digits
                        .chunks(2)
                        .map(|pair| {
                            let text = std::str::from_utf8(pair).unwrap_or("00");
                            u8::from_str_radix(text, 16).unwrap_or(0)
                        })
                        .collect(),
                );
            }
            if byte.is_ascii_hexdigit() {
                digits.push(byte);
            }
        }
        None
    }

    fn next(&mut self) -> Option<Token<'a>> {
        self.skip_space();
        let byte = *self.bytes.get(self.at)?;
        self.at += 1;
        Some(match byte {
            b'/' => Token::Name(self.name()),
            b'(' => self.literal().map_or(Token::Junk, Token::String),
            b'<' if self.bytes.get(self.at) == Some(&b'<') => {
                self.at += 1;
                Token::DictOpen
            }
            b'<' => self.hex().map_or(Token::Junk, Token::String),
            b'>' if self.bytes.get(self.at) == Some(&b'>') => {
                self.at += 1;
                Token::DictClose
            }
            b'[' => Token::ArrayOpen,
            b']' => Token::ArrayClose,
            b')' | b'>' | b'{' | b'}' => Token::Junk,
            _ => {
                let start = self.at - 1;
                while self.at < self.bytes.len() && is_regular(self.bytes[self.at]) {
                    self.at += 1;
                }
                let word = &self.bytes[start..self.at];
                match number(word) {
                    Some(n) => Token::Number(n),
                    None => Token::Keyword(word),
                }
            }
        })
    }
}

/// Reads one operand that begins with `first`. None when the token cannot begin an operand or the operand is broken.
fn operand<'a>(lexer: &mut Lexer<'a>, first: Token<'a>, depth: usize) -> Option<Operand> {
    match first {
        Token::Number(n) => Some(Operand::Number(n)),
        Token::Name(n) => Some(Operand::Name(n)),
        Token::String(s) => Some(Operand::String(s)),
        Token::Keyword(b"true") => Some(Operand::Bool(true)),
        Token::Keyword(b"false") => Some(Operand::Bool(false)),
        Token::Keyword(b"null") => Some(Operand::Null),
        Token::ArrayOpen if depth < NESTING => {
            let mut items = Vec::new();
            loop {
                match lexer.next()? {
                    Token::ArrayClose => return Some(Operand::Array(items)),
                    token => items.push(operand(lexer, token, depth + 1)?),
                }
            }
        }
        Token::DictOpen if depth < NESTING => {
            let mut entries = Vec::new();
            loop {
                match lexer.next()? {
                    Token::DictClose => return Some(Operand::Dict(entries)),
                    Token::Name(key) => {
                        let token = lexer.next()?;
                        entries.push((key, operand(lexer, token, depth + 1)?));
                    }
                    _ => return None,
                }
            }
        }
        _ => None,
    }
}

pub fn expand_filter(name: &[u8]) -> &[u8] {
    match name {
        b"AHx" => b"ASCIIHexDecode",
        b"A85" => b"ASCII85Decode",
        b"LZW" => b"LZWDecode",
        b"Fl" => b"FlateDecode",
        b"RL" => b"RunLengthDecode",
        b"CCF" => b"CCITTFaxDecode",
        b"DCT" => b"DCTDecode",
        other => other,
    }
}

/// Color components of an inline image's own color space; None for a named resource, which the caller resolves.
fn inline_components(image: &InlineImage) -> Option<usize> {
    if matches!(image.get(b"IM", b"ImageMask"), Some(Operand::Bool(true))) {
        return Some(1);
    }
    match image.get(b"CS", b"ColorSpace") {
        Some(Operand::Name(name)) => match name.as_slice() {
            b"G" | b"DeviceGray" | b"I" | b"Indexed" => Some(1),
            b"RGB" | b"DeviceRGB" => Some(3),
            b"CMYK" | b"DeviceCMYK" => Some(4),
            _ => None,
        },
        Some(Operand::Array(items)) if matches!(items.first(), Some(Operand::Name(n)) if n == b"I" || n == b"Indexed") => {
            Some(1)
        }
        _ => None,
    }
}

/// The byte length of an unfiltered inline image, from its dimensions.
fn unfiltered_length(image: &InlineImage) -> Option<usize> {
    let width = usize::try_from(image.get(b"W", b"Width")?.integer()?).ok()?;
    let height = usize::try_from(image.get(b"H", b"Height")?.integer()?).ok()?;
    let mask = matches!(image.get(b"IM", b"ImageMask"), Some(Operand::Bool(true)));
    let bits = if mask {
        1
    } else {
        usize::try_from(image.get(b"BPC", b"BitsPerComponent")?.integer()?).ok()?
    };
    let components = inline_components(image)?;
    let row = width
        .checked_mul(components)?
        .checked_mul(bits)?
        .div_ceil(8);
    row.checked_mul(height)
}

/// True when the bytes from `at` read like content: printable text and white space.
fn reads_as_content(bytes: &[u8]) -> bool {
    bytes
        .iter()
        .take(AFTER_EI)
        .all(|b| is_white(*b) || (0x20..0x7F).contains(b))
}

/// Ends an image whose data stops at `end`: optional white space, then `EI` and a token boundary.
fn ei_at(bytes: &[u8], mut end: usize) -> Option<usize> {
    while end < bytes.len() && is_white(bytes[end]) {
        end += 1;
    }
    (bytes.get(end..end + 2) == Some(b"EI")
        && bytes
            .get(end + 2)
            .is_none_or(|b| is_white(*b) || is_delimiter(*b)))
    .then_some(end + 2)
}

/// The inline image's data and the position after its `EI`, from `start` (just past the white space after `ID`).
fn image_data(bytes: &[u8], start: usize, image: &InlineImage) -> Option<(usize, usize)> {
    let declared = image
        .get(b"L", b"Length")
        .and_then(Operand::integer)
        .and_then(|n| usize::try_from(n).ok());
    let filters = image.filters();
    let known = declared.or_else(|| {
        filters
            .is_empty()
            .then(|| unfiltered_length(image))
            .flatten()
    });
    if let Some(length) = known
        && length <= INLINE_LIMIT
        && let Some(end) = start.checked_add(length).filter(|&e| e <= bytes.len())
        && let Some(after) = ei_at(bytes, end)
    {
        return Some((end, after));
    }
    // ASCII filters end their data with their own marker.
    let marker: Option<&[u8]> = match filters.first().map(Vec::as_slice) {
        Some(b"ASCIIHexDecode") => Some(b">"),
        Some(b"ASCII85Decode") => Some(b"~>"),
        _ => None,
    };
    if let Some(marker) = marker {
        let limit = bytes.len().min(start.saturating_add(INLINE_LIMIT));
        let at = bytes[start..limit]
            .windows(marker.len())
            .position(|w| w == marker)?;
        let end = start + at + marker.len();
        return ei_at(bytes, end).map(|after| (end, after));
    }
    // Otherwise the data ends at the first `EI` that stands alone after white space and is followed by content.
    let limit = bytes.len().min(start.saturating_add(INLINE_LIMIT + 3));
    let mut at = start;
    while at + 2 <= limit {
        if bytes[at] == b'E'
            && bytes[at + 1] == b'I'
            && at > start
            && is_white(bytes[at - 1])
            && bytes
                .get(at + 2)
                .is_none_or(|b| is_white(*b) || is_delimiter(*b))
            && reads_as_content(&bytes[at + 2..])
        {
            return Some((at - 1, at + 2));
        }
        at += 1;
    }
    None
}

/// Every operation and inline image in a content stream. `budget` counts operations across the document.
pub fn parse(bytes: &[u8], budget: &mut u64) -> Result<Parsed, Error> {
    let mut lexer = Lexer { bytes, at: 0 };
    let mut parsed = Parsed::default();
    let mut stack: Vec<Operand> = Vec::new();
    fn drop_stack(stack: &mut Vec<Operand>, parsed: &mut Parsed) {
        parsed.dropped += stack.len() as u64;
        stack.clear();
    }
    while let Some(token) = lexer.next() {
        match token {
            Token::Keyword(b"BI") => {
                drop_stack(&mut stack, &mut parsed);
                let image = inline_image(&mut lexer)?;
                *budget = budget.checked_sub(1).ok_or(Error::Budget)?;
                parsed.items.push(Item::Image(image));
            }
            Token::Keyword(word) if !matches!(word, b"true" | b"false" | b"null") => {
                match operator(word) {
                    Some((name, shape)) => match take(shape, &stack) {
                        Some(n) => {
                            *budget = budget.checked_sub(1).ok_or(Error::Budget)?;
                            let operands = stack.split_off(stack.len() - n);
                            drop_stack(&mut stack, &mut parsed);
                            parsed.items.push(Item::Op(Operation {
                                operator: name,
                                operands,
                            }));
                        }
                        None => {
                            parsed.dropped += 1;
                            drop_stack(&mut stack, &mut parsed);
                        }
                    },
                    None => {
                        parsed.dropped += 1;
                        drop_stack(&mut stack, &mut parsed);
                    }
                }
            }
            Token::Junk | Token::ArrayClose | Token::DictClose => {
                parsed.dropped += 1;
                drop_stack(&mut stack, &mut parsed);
            }
            token => match operand(&mut lexer, token, 0) {
                Some(value) => {
                    stack.push(value);
                    // Operands no operator can take are dropped as they arrive, so junk cannot grow the stack.
                    if stack.len() > 64 {
                        stack.remove(0);
                        parsed.dropped += 1;
                    }
                }
                None => {
                    parsed.dropped += 1;
                    drop_stack(&mut stack, &mut parsed);
                }
            },
        }
    }
    drop_stack(&mut stack, &mut parsed);
    Ok(parsed)
}

/// An inline image after its `BI`: the dictionary up to `ID`, then the data up to `EI`.
fn inline_image(lexer: &mut Lexer) -> Result<InlineImage, Error> {
    let mut entries = Vec::new();
    loop {
        match lexer.next().ok_or(Error::InlineImage)? {
            Token::Keyword(b"ID") => break,
            Token::Name(key) => {
                let token = lexer.next().ok_or(Error::InlineImage)?;
                let value = operand(lexer, token, 0).ok_or(Error::InlineImage)?;
                entries.push((key, value));
            }
            _ => return Err(Error::InlineImage),
        }
    }
    // One white-space byte separates `ID` from the data.
    let mut start = lexer.at;
    if lexer.bytes.get(start).is_some_and(|b| is_white(*b)) {
        start += 1;
    }
    let mut image = InlineImage {
        entries,
        data: Vec::new(),
    };
    let (end, after) = image_data(lexer.bytes, start, &image).ok_or(Error::InlineImage)?;
    if end - start > INLINE_LIMIT {
        return Err(Error::InlineImage);
    }
    image.data = lexer.bytes[start..end].to_vec();
    lexer.at = after;
    Ok(image)
}

fn write_name(out: &mut Vec<u8>, name: &[u8]) {
    out.push(b'/');
    for &byte in name {
        if (0x21..=0x7E).contains(&byte) && !is_delimiter(byte) && byte != b'#' {
            out.push(byte);
        } else {
            out.extend_from_slice(format!("#{byte:02X}").as_bytes());
        }
    }
}

fn write_operand(out: &mut Vec<u8>, operand: &Operand) {
    match operand {
        Operand::Number(text) => out.extend_from_slice(text.as_bytes()),
        Operand::Name(name) => write_name(out, name),
        Operand::String(bytes) => {
            out.push(b'<');
            for byte in bytes {
                out.extend_from_slice(format!("{byte:02X}").as_bytes());
            }
            out.push(b'>');
        }
        Operand::Bool(true) => out.extend_from_slice(b"true"),
        Operand::Bool(false) => out.extend_from_slice(b"false"),
        Operand::Null => out.extend_from_slice(b"null"),
        Operand::Array(items) => {
            out.push(b'[');
            for (i, item) in items.iter().enumerate() {
                if i > 0 {
                    out.push(b' ');
                }
                write_operand(out, item);
            }
            out.push(b']');
        }
        Operand::Dict(entries) => {
            out.extend_from_slice(b"<<");
            for (key, value) in entries {
                write_name(out, key);
                out.push(b' ');
                write_operand(out, value);
            }
            out.extend_from_slice(b">>");
        }
    }
}

/// The canonical form of a parsed stream: one operation per line, operands separated by one space.
pub fn encode(items: &[Item]) -> Vec<u8> {
    let mut out = Vec::new();
    for item in items {
        match item {
            Item::Op(operation) => {
                for operand in &operation.operands {
                    write_operand(&mut out, operand);
                    out.push(b' ');
                }
                out.extend_from_slice(operation.operator.as_bytes());
            }
            Item::Image(image) => {
                out.extend_from_slice(b"BI");
                for (key, value) in &image.entries {
                    out.push(b' ');
                    write_name(&mut out, key);
                    out.push(b' ');
                    write_operand(&mut out, value);
                }
                out.extend_from_slice(b" ID ");
                out.extend_from_slice(&image.data);
                out.extend_from_slice(b"\nEI");
            }
        }
        out.push(b'\n');
    }
    out
}

const DEVICE_SPACES: &[&[u8]] = &[b"DeviceGray", b"DeviceRGB", b"DeviceCMYK", b"Pattern"];
const INLINE_SPACES: &[&[u8]] = &[
    b"G",
    b"RGB",
    b"CMYK",
    b"I",
    b"DeviceGray",
    b"DeviceRGB",
    b"DeviceCMYK",
    b"Indexed",
];

/// The resource an item draws with: its category and name.
pub fn uses(item: &Item) -> Vec<(Category, &[u8])> {
    match item {
        Item::Op(operation) => {
            let first = operation.operands.first().and_then(Operand::name);
            let found = match operation.operator {
                "Do" => first.map(|n| (Category::XObject, n)),
                "Tf" => first.map(|n| (Category::Font, n)),
                "gs" => first.map(|n| (Category::ExtGState, n)),
                "sh" => first.map(|n| (Category::Shading, n)),
                "cs" | "CS" => first
                    .filter(|n| !DEVICE_SPACES.contains(n))
                    .map(|n| (Category::ColorSpace, n)),
                "scn" | "SCN" => operation
                    .operands
                    .last()
                    .and_then(Operand::name)
                    .map(|n| (Category::Pattern, n)),
                "BDC" | "DP" => operation
                    .operands
                    .get(1)
                    .and_then(Operand::name)
                    .map(|n| (Category::Properties, n)),
                _ => None,
            };
            found.into_iter().collect()
        }
        Item::Image(image) => {
            let named = match image.get(b"CS", b"ColorSpace") {
                Some(Operand::Name(name)) => Some(name.as_slice()),
                // An indexed space whose base is a named resource.
                Some(Operand::Array(items)) => items.get(1).and_then(Operand::name),
                _ => None,
            };
            named
                .filter(|n| !INLINE_SPACES.contains(n))
                .map(|n| (Category::ColorSpace, n))
                .into_iter()
                .collect()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ops(bytes: &[u8]) -> Parsed {
        parse(bytes, &mut OPERATION_LIMIT.clone()).unwrap()
    }

    fn operators(parsed: &Parsed) -> Vec<&str> {
        parsed
            .items
            .iter()
            .map(|i| match i {
                Item::Op(o) => o.operator,
                Item::Image(_) => "BI",
            })
            .collect()
    }

    #[test]
    fn operations_keep_only_the_operands_they_take() {
        let parsed = ops(b"q 1 0 0 1 72 700 cm BT /F1 12 Tf (Serum retinol) Tj ET Q");
        assert_eq!(operators(&parsed), ["q", "cm", "BT", "Tf", "Tj", "ET", "Q"]);
        assert_eq!(parsed.dropped, 0);
        // Extra operands in front are dropped; the nearest ones stay.
        let parsed = ops(b"/Junk (hidden payload) 9 9 100 200 m");
        assert_eq!(
            parsed.items,
            [Item::Op(Operation {
                operator: "m",
                operands: vec![Operand::Number("100".into()), Operand::Number("200".into())],
            })]
        );
        assert_eq!(parsed.dropped, 4);
        // Unknown operators, comments and stray delimiters go, with their operands.
        let parsed = ops(b"% a comment\n(x) 1 2 unknownop ) } 0 0 m");
        assert_eq!(operators(&parsed), ["m"]);
        assert_eq!(parsed.dropped, 6);
        // Operands of the wrong type make the operation go.
        assert_eq!(operators(&ops(b"(text) 1 m 5 Tj")), Vec::<&str>::new());
    }

    #[test]
    fn numbers_keep_their_written_form() {
        let parsed = ops(b"595.27559 -.5 m --3 +4. l");
        assert_eq!(
            String::from_utf8(encode(&parsed.items)).unwrap(),
            "595.27559 -.5 m\n-3 4. l\n"
        );
        let parsed = ops(b"0.1234567891 842 m");
        assert_eq!(
            String::from_utf8(encode(&parsed.items)).unwrap(),
            "0.1234567891 842 m\n"
        );
    }

    #[test]
    fn resource_uses_are_typed() {
        let parsed = ops(b"/Im1 Do /F1 9 Tf /GS1 gs /Sh1 sh /CS1 cs /DeviceRGB CS /P1 scn 0.5 /P2 SCN /OC /MC0 BDC (/Im9 Do) Tj");
        let used: Vec<(Category, Vec<u8>)> = parsed
            .items
            .iter()
            .flat_map(|i| uses(i).into_iter().map(|(c, n)| (c, n.to_vec())))
            .collect();
        assert_eq!(
            used,
            [
                (Category::XObject, b"Im1".to_vec()),
                (Category::Font, b"F1".to_vec()),
                (Category::ExtGState, b"GS1".to_vec()),
                (Category::Shading, b"Sh1".to_vec()),
                (Category::ColorSpace, b"CS1".to_vec()),
                (Category::Pattern, b"P1".to_vec()),
                (Category::Pattern, b"P2".to_vec()),
                (Category::Properties, b"MC0".to_vec()),
            ],
            "a name inside a string is not a use"
        );
    }

    #[test]
    fn inline_images_are_read_whole() {
        // Unfiltered: the length comes from the dimensions, so `EI` inside the data does not end it.
        let mut stream = b"q BI /W 4 /H 1 /CS /G /BPC 8 ID ".to_vec();
        stream.extend_from_slice(b" EI ");
        stream.extend_from_slice(b"\nEI Q");
        let parsed = ops(&stream);
        assert_eq!(operators(&parsed), ["q", "BI", "Q"]);
        let Item::Image(image) = &parsed.items[1] else {
            panic!()
        };
        assert_eq!(image.data, b" EI ");
        // Filtered: the data runs to the `EI` that stands alone and is followed by content.
        let mut stream = b"BI /W 10 /H 10 /CS /RGB /BPC 8 /F /Fl ID ".to_vec();
        stream.extend_from_slice(&[0x78, 0x9C, 0x01, b' ', b'E', b'I', 0x00, 0xFF, 0x10]);
        stream.extend_from_slice(b"\nEI\nQ");
        let parsed = ops(&stream);
        let Item::Image(image) = &parsed.items[0] else {
            panic!()
        };
        assert_eq!(image.data.len(), 9);
        assert_eq!(image.filters(), [b"FlateDecode".to_vec()]);
        assert_eq!(operators(&parsed), ["BI", "Q"]);
        // ASCII hex data ends at its marker.
        let parsed = ops(b"BI /W 1 /H 1 /CS /G /BPC 8 /F /AHx ID FF> EI");
        let Item::Image(image) = &parsed.items[0] else {
            panic!()
        };
        assert_eq!(image.data, b"FF>");
        // An image whose end cannot be found makes the stream unreadable.
        assert_eq!(
            parse(b"BI /W 9 /H 9 /F /DCT ID \xFF\xD8 no end", &mut 10),
            Err(Error::InlineImage)
        );
    }

    #[test]
    fn encoding_round_trips() {
        let source = b"q BI /W 2 /H 1 /CS /G /BPC 8 ID \x00\xFF\nEI Q [(a) -250 (b)] TJ /Tag <</MCID 3>> BDC (\\(x\\)) Tj [3 1] 0 d";
        let first = ops(source);
        let encoded = encode(&first.items);
        let second = ops(&encoded);
        assert_eq!(first.items, second.items);
        assert_eq!(encode(&second.items), encoded);
    }

    #[test]
    fn the_operation_budget_is_shared() {
        let mut budget = 3;
        assert_eq!(parse(b"q Q q Q", &mut budget), Err(Error::Budget));
    }
}
