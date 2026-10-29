//! Embedded payloads and stream decoding, and the scan over every stream.
use super::*;

/// An inline image as an image stream for the scan: abbreviated keys and names expanded, and a named color space
/// resolved in the resources of the content that draws it.
pub(super) fn inline_stream(
    document: &Document,
    image: &content::InlineImage,
    resources: Option<&Dictionary>,
) -> Stream {
    fn space(name: &[u8]) -> &[u8] {
        match name {
            b"G" => b"DeviceGray",
            b"RGB" => b"DeviceRGB",
            b"CMYK" => b"DeviceCMYK",
            b"I" => b"Indexed",
            other => other,
        }
    }
    let mut dict = Dictionary::new();
    dict.set("Type", "XObject");
    dict.set("Subtype", "Image");
    for (key, value) in &image.entries {
        let full: &[u8] = match key.as_slice() {
            b"W" => b"Width",
            b"H" => b"Height",
            b"BPC" => b"BitsPerComponent",
            b"CS" => b"ColorSpace",
            b"IM" => b"ImageMask",
            b"F" => b"Filter",
            b"DP" => b"DecodeParms",
            b"D" => b"Decode",
            b"I" => b"Interpolate",
            b"L" => b"Length",
            other => other,
        };
        let object = match (full, value) {
            (b"Filter", Operand::Name(n)) => Object::Name(content::expand_filter(n).to_vec()),
            (b"Filter", Operand::Array(items)) => Object::Array(
                items
                    .iter()
                    .filter_map(Operand::name)
                    .map(|n| Object::Name(content::expand_filter(n).to_vec()))
                    .collect(),
            ),
            (b"ColorSpace", Operand::Name(n)) if space(n) != n.as_slice() => {
                Object::Name(space(n).to_vec())
            }
            (b"ColorSpace", Operand::Name(n)) => resources
                .and_then(|r| r.get(b"ColorSpace").ok())
                .and_then(|c| dict_of(document, c))
                .and_then(|c| c.get(n).ok())
                .and_then(|o| deref(document, o))
                .cloned()
                .unwrap_or(Object::Name(n.clone())),
            (b"ColorSpace", Operand::Array(items)) => Object::Array(
                items
                    .iter()
                    .map(|i| match i {
                        Operand::Name(n) => Object::Name(space(n).to_vec()),
                        other => other.to_object(),
                    })
                    .collect(),
            ),
            _ => value.to_object(),
        };
        dict.set(full.to_vec(), object);
    }
    Stream::new(dict, image.data.clone())
}

/// Collects action text and the streams that hold scripts or attached files.
#[derive(Default)]
pub(super) struct Payloads {
    pub(super) texts: Vec<(Role, Vec<u8>)>,
    pub(super) streams: HashMap<ObjectId, BTreeSet<Role>>,
    /// A payload entry nested deeper than the walk follows, or action text past its limit: the walk is incomplete.
    pub(super) incomplete: bool,
}

/// How deep a payload entry's own dictionaries and arrays are followed: an attached file's or a rich-media asset's
/// tree is a few levels; deeper is malformed.
pub(super) const PAYLOAD_DEPTH: usize = 16;

impl Payloads {
    pub(super) fn string_bytes(
        &mut self,
        document: &Document,
        object: &Object,
        out: &mut Vec<u8>,
        depth: usize,
    ) {
        if depth > PAYLOAD_DEPTH || out.len() > ACTION_TEXT_LIMIT {
            self.incomplete = true;
            return;
        }
        match deref(document, object) {
            Some(Object::String(bytes, _)) => {
                out.extend_from_slice(bytes);
                out.push(b'\n');
            }
            Some(Object::Name(bytes)) => {
                out.extend_from_slice(bytes);
                out.push(b'\n');
            }
            Some(Object::Dictionary(d)) => {
                for (_, v) in d.iter() {
                    self.string_bytes(document, v, out, depth + 1);
                }
            }
            Some(Object::Array(items)) => {
                for item in items {
                    self.string_bytes(document, item, out, depth + 1);
                }
            }
            _ => {}
        }
    }

    pub(super) fn visit(&mut self, document: &Document, dict: &Dictionary) {
        let script = name(dict, b"S") == Some(b"JavaScript") || dict.has(b"JS");
        if script
            && let Ok(js) = dict.get(b"JS")
            && let Some(Object::String(bytes, _)) = deref(document, js)
        {
            self.texts.push((Role::Script, bytes.clone()));
        }
        if name(dict, b"S") == Some(b"Launch") {
            let mut text = Vec::new();
            for key in [&b"F"[..], b"P", b"Win", b"Unix", b"Mac"] {
                if let Ok(value) = dict.get(key) {
                    self.string_bytes(document, value, &mut text, 0);
                }
            }
            if !text.is_empty() {
                self.texts.push((Role::Launch, text));
            }
        }
        // Every stream an active-content entry reaches is a payload, whatever else names it: the strip removes these
        // entries, and a stream also named by a drawing key must not stay behind as metadata or an image.
        for (key, role) in PAYLOAD_KEYS {
            if let Ok(value) = dict.get(key) {
                self.reach(document, value, *role, 0);
            }
        }
    }

    /// Marks the streams a payload entry reaches, through direct and referenced dictionaries and arrays, without
    /// climbing back into the page tree or the catalog.
    pub(super) fn reach(&mut self, document: &Document, object: &Object, role: Role, depth: usize) {
        if depth > PAYLOAD_DEPTH {
            self.incomplete = true;
            return;
        }
        let (id, target) = match document.dereference(object) {
            Ok(found) => found,
            Err(_) => return,
        };
        match target {
            Object::Stream(stream) => {
                if let Some(id) = id {
                    self.streams.entry(id).or_default().insert(role);
                }
                for (key, value) in stream.dict.iter() {
                    if !matches!(key.as_slice(), b"Parent" | b"P") {
                        self.reach(document, value, role, depth + 1);
                    }
                }
            }
            Object::Dictionary(dict) => {
                if matches!(name(dict, b"Type"), Some(b"Page" | b"Pages" | b"Catalog")) {
                    return;
                }
                for (key, value) in dict.iter() {
                    if !matches!(key.as_slice(), b"Parent" | b"P") {
                        self.reach(document, value, role, depth + 1);
                    }
                }
            }
            Object::Array(items) => {
                for item in items {
                    self.reach(document, item, role, depth + 1);
                }
            }
            _ => {}
        }
    }

    pub(super) fn walk(&mut self, document: &Document, object: &Object, depth: usize) {
        if depth > WALK_DEPTH {
            self.incomplete = true;
            return;
        }
        match object {
            Object::Dictionary(dict) => {
                self.visit(document, dict);
                for (_, value) in dict.iter() {
                    self.walk(document, value, depth + 1);
                }
            }
            Object::Stream(stream) => {
                self.visit(document, &stream.dict);
                for (_, value) in stream.dict.iter() {
                    self.walk(document, value, depth + 1);
                }
            }
            Object::Array(items) => {
                for item in items {
                    self.walk(document, item, depth + 1);
                }
            }
            _ => {}
        }
    }

    pub(super) fn of(document: &Document) -> Self {
        let mut payloads = Payloads::default();
        for object in document.objects.values() {
            payloads.walk(document, object, 0);
        }
        payloads
    }
}

/// One role from every use found for a stream. A script or an attached file outranks any drawing use of the same
/// bytes, so a payload cannot hide behind a drawing key; a stream claimed for two different uses, or for none, is
/// unknown.
pub(super) fn settle(claims: &BTreeSet<Role>) -> Role {
    for payload in [Role::Script, Role::Embedded] {
        if claims.contains(&payload) {
            return payload;
        }
    }
    match (claims.len(), claims.first()) {
        (1, Some(role)) => *role,
        _ => Role::Unknown,
    }
}

/// Every stream object's use (what the walk reached it by, what actions, attached-file and other active-content
/// entries name it, and its own type, settled into one role), with the walk's inline images and whether it finished.
pub fn stream_uses(document: &Document) -> Uses {
    let mut walker = Walker::new(document);
    walker.document();
    let mut claims = walker.roles;
    let payloads = Payloads::of(document);
    for (id, payload) in payloads.streams {
        claims.entry(id).or_default().extend(payload);
    }
    let roles = document
        .objects
        .iter()
        .filter_map(|(id, object)| {
            let Object::Stream(stream) = object else {
                return None;
            };
            let mut uses = claims.remove(id).unwrap_or_default();
            match name(&stream.dict, b"Type") {
                Some(b"ObjStm" | b"XRef") => {
                    uses.insert(Role::Structure);
                }
                Some(b"EmbeddedFile") => {
                    uses.insert(Role::Embedded);
                }
                _ => {}
            }
            Some((*id, settle(&uses)))
        })
        .collect();
    Uses {
        roles,
        inline: walker.inline,
        complete: walker.complete && !payloads.incomplete && nesting_within(document),
        unreadable: walker.unreadable,
        dropped: walker.dropped,
        content_strings: walker.content_strings,
        content_strings_truncated: walker.content_strings_truncated,
    }
}

/// True when no object nests deeper than the walks follow. Counting and removing active content, collecting payloads
/// and reading strings all walk each object's direct nesting; this check makes a file none of them could finish fail
/// closed instead of passing short.
pub(super) fn nesting_within(document: &Document) -> bool {
    fn within(object: &Object, depth: usize) -> bool {
        if depth > WALK_DEPTH {
            return false;
        }
        match object {
            Object::Dictionary(d) => d.iter().all(|(_, v)| within(v, depth + 1)),
            Object::Stream(s) => s.dict.iter().all(|(_, v)| within(v, depth + 1)),
            Object::Array(items) => items.iter().all(|i| within(i, depth + 1)),
            _ => true,
        }
    }
    document.objects.values().all(|o| within(o, 0))
        && document.trailer.iter().all(|(_, v)| within(v, 1))
}

/// Every stream object's use.
pub fn roles(document: &Document) -> BTreeMap<ObjectId, Role> {
    stream_uses(document).roles
}

/// The stream's bytes after its general filters (Flate, LZW, ASCII85, ASCIIHex, run-length, Brotli). Decoding stops
/// before an image codec, whose input is scanned as it is. None when a filter is unknown or the bytes pass the limit.
pub fn decode(stream: &Stream, limit: usize) -> Option<Vec<u8>> {
    let filters = match stream.dict.get(b"Filter") {
        Err(_) => Vec::new(),
        Ok(_) => stream.filters().ok()?,
    };
    let general = filters
        .iter()
        .take_while(|f| GENERAL_FILTERS.contains(f))
        .count();
    // An image codec must end the chain: a filter after it is one the scan cannot undo.
    if general < filters.len()
        && (!IMAGE_CODECS.contains(&filters[general]) || general + 1 < filters.len())
    {
        return None;
    }
    if general == filters.len() {
        return stream.decompressed_content_with_limit(limit).ok();
    }
    if general == 0 {
        return (stream.content.len() <= limit).then(|| stream.content.clone());
    }
    let mut dict = stream.dict.clone();
    dict.set(
        "Filter",
        filters[..general]
            .iter()
            .map(|f| Object::Name(f.to_vec()))
            .collect::<Vec<_>>(),
    );
    if let Ok(Object::Array(parms)) = stream.dict.get(b"DecodeParms") {
        dict.set(
            "DecodeParms",
            parms.iter().take(general).cloned().collect::<Vec<_>>(),
        );
    }
    Stream::new(dict, stream.content.clone())
        .decompressed_content_with_limit(limit)
        .ok()
}

/// Calls `visit` with the bytes of every string and name inside `object`, without following references: each object
/// is reached from the document's object table. False when the object nests deeper than the walk follows.
pub(super) fn strings(object: &Object, depth: usize, visit: &mut dyn FnMut(&[u8])) -> bool {
    if depth > WALK_DEPTH {
        return false;
    }
    // Every member is visited even after one nests too deep, so the rest is still scanned.
    let mut complete = true;
    match object {
        Object::String(bytes, _) | Object::Name(bytes) => visit(bytes),
        Object::Dictionary(dict) => {
            for (key, value) in dict.iter() {
                visit(key);
                complete &= strings(value, depth + 1, visit);
            }
        }
        Object::Stream(stream) => {
            for (key, value) in stream.dict.iter() {
                visit(key);
                complete &= strings(value, depth + 1, visit);
            }
        }
        Object::Array(items) => {
            for item in items {
                complete &= strings(item, depth + 1, visit);
            }
        }
        _ => {}
    }
    complete
}

/// Decodes every stream and inline image within the budget and scans it, with the document's strings and each
/// action's text, against the rules and the archive recognizer, and bounds every image a viewer would decode.
pub fn scan(document: &Document, uses: &Uses, rules: &Rules) -> (Streams, Vec<StreamMatch>) {
    let mut streams = Streams::default();
    let mut found: BTreeMap<(String, Role), (Severity, u64)> = BTreeMap::new();
    let mut record = |matches: Vec<crate::rules::Match>, role: Role| {
        for m in matches {
            found.entry((m.rule, role)).or_insert((m.severity, 0)).1 += 1;
        }
    };
    let mut budget = DECODE_BUDGET;
    let mut image_bytes: u64 = 0;
    for (id, role) in &uses.roles {
        let Some(Object::Stream(stream)) = document.objects.get(id) else {
            continue;
        };
        streams.total += 1;
        if *role == Role::Unknown {
            streams.unknown += 1;
        }
        // Object and cross-reference streams hold the objects already read, not content of their own.
        if *role == Role::Structure {
            continue;
        }
        let Some(bytes) = decode(stream, STREAM_LIMIT.min(budget)) else {
            streams.unscanned += 1;
            continue;
        };
        budget -= bytes.len();
        streams.decoded_bytes += bytes.len() as u64;
        // Scripts, launch text and attached files are removed with their actions; any other stream may not hold
        // an archive.
        if !matches!(role, Role::Script | Role::Launch | Role::Embedded) && zip::find(&bytes).any()
        {
            streams.archives += 1;
        }
        // Every image the copy keeps is decoded later by the corpus; its size is bounded here, and so is any kept
        // stream that declares itself an image, whatever use reached it.
        if *role == Role::Image || (role.kept() && name(&stream.dict, b"Subtype") == Some(b"Image"))
        {
            match image_size(document, stream, &bytes) {
                ImageSize::Within(n) => image_bytes = image_bytes.saturating_add(n),
                ImageSize::TooLarge => streams.oversized_images += 1,
                ImageSize::Unreadable => streams.unscanned += 1,
            }
        }
        match rules.scan(&bytes, Scope::stream(role.name(), "pdf")) {
            Ok(matches) => record(matches, *role),
            Err(_) => streams.unscanned += 1,
        }
    }
    for stream in &uses.inline {
        streams.inline_images += 1;
        // JPEG 2000 and JBIG2 are not allowed inline.
        let allowed = stream.filters().is_ok_and(|filters| {
            !filters
                .iter()
                .any(|f| matches!(*f, b"JPXDecode" | b"JBIG2Decode"))
        });
        let Some(bytes) = allowed
            .then(|| decode(stream, STREAM_LIMIT.min(budget)))
            .flatten()
        else {
            streams.unscanned += 1;
            continue;
        };
        budget -= bytes.len();
        streams.decoded_bytes += bytes.len() as u64;
        if zip::find(&bytes).any() {
            streams.archives += 1;
        }
        match image_size(document, stream, &bytes) {
            ImageSize::Within(n) => image_bytes = image_bytes.saturating_add(n),
            ImageSize::TooLarge => streams.oversized_images += 1,
            ImageSize::Unreadable => streams.unscanned += 1,
        }
        match rules.scan(&bytes, Scope::stream(Role::Image.name(), "pdf")) {
            Ok(matches) => record(matches, Role::Image),
            Err(_) => streams.unscanned += 1,
        }
    }
    // The images together, as a viewer decodes them page after page.
    if image_bytes > IMAGE_TOTAL {
        streams.oversized_images += 1;
    }
    let mut text = Vec::new();
    let mut archives = 0;
    let mut truncated = false;
    let mut visit = |bytes: &[u8]| {
        if bytes.len() >= 22 && zip::find(bytes).any() {
            archives += 1;
        }
        if text.len() + bytes.len() < STRINGS_LIMIT {
            text.extend_from_slice(bytes);
            text.push(b'\n');
        } else {
            truncated = true;
        }
    };
    let mut complete = true;
    for object in document.objects.values() {
        complete &= strings(object, 0, &mut visit);
    }
    for (_, value) in document.trailer.iter() {
        complete &= strings(value, 1, &mut visit);
    }
    // Strings and names in content operands: text a page shows, marked-content properties, inline image keys.
    for bytes in &uses.content_strings {
        visit(bytes);
    }
    streams.archives += archives;
    if truncated || !complete || uses.content_strings_truncated {
        streams.unscanned += 1;
    }
    match rules.scan(&text, Scope::stream(Role::String.name(), "pdf")) {
        Ok(matches) => record(matches, Role::String),
        Err(_) => streams.unscanned += 1,
    }
    for (role, text) in Payloads::of(document).texts {
        match rules.scan(&text, Scope::stream(role.name(), "pdf")) {
            Ok(matches) => record(matches, role),
            Err(_) => streams.unscanned += 1,
        }
    }
    let matches = found
        .into_iter()
        .map(|((rule, role), (severity, count))| StreamMatch {
            rule,
            severity,
            role,
            count,
        })
        .collect();
    (streams, matches)
}
