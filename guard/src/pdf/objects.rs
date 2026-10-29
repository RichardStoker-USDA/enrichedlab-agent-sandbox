//! Reading the object graph: values, pages, actions, annotations, layers and metadata.
use super::*;

/// A value read where it stands: a name of at most `NAME_LIMIT` bytes, a number, a boolean or null.
pub(super) fn plain(object: &Object) -> bool {
    match object {
        Object::Name(name) => name.len() <= NAME_LIMIT,
        Object::Integer(_) | Object::Real(_) | Object::Boolean(_) | Object::Null => true,
        _ => false,
    }
}

/// The bytes a value adds when it is written in place of a reference.
pub(super) fn weight(object: &Object) -> usize {
    match object {
        Object::Name(name) => 32 + name.len(),
        Object::Array(items) => 32 + items.iter().map(weight).sum::<usize>(),
        _ => 32,
    }
}

/// The object each object's chain of references ends at: the object itself when it is not a reference, the first
/// object along the chain that is not one otherwise, and None when the chain reaches a missing object, which a reader
/// takes as null. Err when a chain returns to an object it passed: no reader can resolve it.
pub(super) fn terminals(document: &Document) -> Result<BTreeMap<ObjectId, Option<ObjectId>>> {
    let mut ends: BTreeMap<ObjectId, Option<ObjectId>> = BTreeMap::new();
    for &start in document.objects.keys() {
        let mut path = Vec::new();
        let mut passed = HashSet::new();
        let mut current = start;
        let end = loop {
            if let Some(known) = ends.get(&current) {
                break *known;
            }
            let Some(object) = document.objects.get(&current) else {
                break None;
            };
            if !passed.insert(current) {
                bail!("a reference chain returns to itself");
            }
            path.push(current);
            match object {
                Object::Reference(next) => current = *next,
                _ => break Some(current),
            }
        };
        for id in path {
            ends.insert(id, end);
        }
    }
    Ok(ends)
}

/// A PDF may give any value as a reference to an object elsewhere, and a reference may point at another reference:
/// `/S 7 0 R`, where object 7 is the name `/Launch`, is a launch action to every viewer, and so is a chain of any length
/// that ends there. Before any reader in this module sees the document, every reference is replaced, in every
/// dictionary and array of every object and of the trailer: by the value its chain ends at when that is a plain value
/// (`plain`), by null when the chain ends at a missing object, and by a reference to the chain's last object
/// otherwise, so no reader follows more than one reference; then every reference to an array of at most
/// `ARRAY_LIMIT` plain values (a filter list or a page box) by that array. After these two passes no reference to a
/// plain value or a plain array remains. A chain that returns to itself, or replacements adding more than
/// `DIRECT_LIMIT` bytes, make the document unreadable.
pub(super) fn direct_values(document: &mut Document) -> Result<()> {
    /// Replaces each reference `values` names; with `missing`, a reference to an object the document lacks
    /// becomes null.
    fn replace(
        object: &mut Object,
        values: &BTreeMap<ObjectId, Object>,
        missing: Option<&BTreeMap<ObjectId, Option<ObjectId>>>,
        depth: usize,
        added: &mut usize,
    ) -> Result<()> {
        if depth > WALK_DEPTH {
            return Ok(());
        }
        let entries = match object {
            Object::Reference(id) => {
                let value = match values.get(id) {
                    Some(value) => value.clone(),
                    None if missing.is_some_and(|ends| !ends.contains_key(id)) => Object::Null,
                    None => return Ok(()),
                };
                *added += weight(&value);
                if *added > DIRECT_LIMIT {
                    bail!("indirect values past the limit");
                }
                *object = value;
                return Ok(());
            }
            Object::Array(items) => {
                for item in items {
                    replace(item, values, missing, depth + 1, added)?;
                }
                return Ok(());
            }
            Object::Dictionary(dict) => dict,
            Object::Stream(stream) => &mut stream.dict,
            _ => return Ok(()),
        };
        for (_, value) in entries.iter_mut() {
            replace(value, values, missing, depth + 1, added)?;
        }
        Ok(())
    }
    let ends = terminals(document)?;
    let mut added = 0;
    for arrays in [false, true] {
        let mut values = BTreeMap::new();
        for (&id, end) in &ends {
            let value = match end.map(|end| (end, &document.objects[&end])) {
                None if !arrays => Object::Null,
                Some((_, target)) if !arrays && plain(target) => target.clone(),
                Some((end, _)) if !arrays && end != id => Object::Reference(end),
                Some((_, Object::Array(items)))
                    if arrays && items.len() <= ARRAY_LIMIT && items.iter().all(plain) =>
                {
                    Object::Array(items.clone())
                }
                _ => continue,
            };
            values.insert(id, value);
        }
        let missing = (!arrays).then_some(&ends);
        for object in document.objects.values_mut() {
            replace(object, &values, missing, 0, &mut added)?;
        }
        for (_, value) in document.trailer.iter_mut() {
            replace(value, &values, missing, 1, &mut added)?;
        }
    }
    Ok(())
}

/// The worker's `pdf-inspect` report.
pub fn report(document: &Document, rules: &Rules) -> Report {
    let metadata = metadata_text(document);
    let uses = stream_uses(document);
    let (mut streams, matches) = scan(document, &uses, rules);
    streams.unscanned += uses.unreadable;
    streams.content_dropped = uses.dropped;
    Report {
        rules_version: rules.version.clone(),
        pages: document.get_pages().len(),
        active: active_content(document),
        hidden_layers: hidden_layers(document),
        metadata_unicode: text::count(&metadata),
        metadata_text: !metadata.trim().is_empty(),
        walk_complete: uses.complete,
        streams,
        matches,
    }
}

/// The worker's `pdf-strip` step: active content out, then document metadata and every stream nothing draws, then
/// every kept content stream written again in canonical form, then unreferenced objects. Stream uses are read before
/// the strip, while the actions and attached-file entries that name a payload are still there, so a script's body
/// that is also named as an image goes too. Removed streams become null objects, so a key that named one reads as
/// absent. A walk that did not read every drawn content stream refuses the rebuild.
pub fn rebuild_objects(document: &mut Document) -> Result<Stripped> {
    let uses = stream_uses(document);
    if !uses.complete || uses.unreadable > 0 {
        bail!("the stream-use walk did not read every drawn content stream");
    }
    strip(document);
    let mut removed = 0;
    for (id, role) in &uses.roles {
        if !role.kept() && document.objects.contains_key(id) {
            document.objects.insert(*id, Object::Null);
            removed += 1;
        }
    }
    let mut budget = content::OPERATION_LIMIT;
    let mut rewritten = 0;
    // A page's content streams become one, since an operation may continue from one stream into the next.
    let pages: Vec<ObjectId> = document.get_pages().into_values().collect();
    for page in pages {
        let parts = page_contents(document, page);
        if parts.is_empty() {
            continue;
        }
        let bytes = canonical(document, &parts, &mut budget)?;
        let combined = document.add_object(Stream::new(Dictionary::new(), bytes));
        if let Ok(Object::Dictionary(dict)) = document.get_object_mut(page) {
            dict.set("Contents", combined);
            rewritten += 1;
        }
    }
    document.prune_objects();
    // Forms, patterns, glyphs and appearances, in place.
    for (id, role) in &uses.roles {
        if *role != Role::Content || !matches!(document.objects.get(id), Some(Object::Stream(_))) {
            continue;
        }
        let bytes = canonical(document, &[*id], &mut budget)?;
        if let Some(Object::Stream(stream)) = document.objects.get_mut(id) {
            stream.set_plain_content(bytes);
            rewritten += 1;
        }
    }
    Ok(Stripped {
        removed_streams: removed,
        rewritten_streams: rewritten,
    })
}

/// The streams of a page's `Contents`, one or an array.
pub(super) fn page_contents(document: &Document, page: ObjectId) -> Vec<ObjectId> {
    let Ok(dict) = document.get_dictionary(page) else {
        return Vec::new();
    };
    let references =
        |items: &[Object]| items.iter().filter_map(|i| i.as_reference().ok()).collect();
    match dict.get(b"Contents") {
        Ok(Object::Reference(id)) => match document.objects.get(id) {
            Some(Object::Array(items)) => references(items),
            _ => vec![*id],
        },
        Ok(Object::Array(items)) => references(items),
        _ => Vec::new(),
    }
}

/// Content streams decoded, joined, read and written again in canonical form.
pub(super) fn canonical(
    document: &Document,
    ids: &[ObjectId],
    budget: &mut u64,
) -> Result<Vec<u8>> {
    let mut bytes = Vec::new();
    for id in ids {
        let Some(Object::Stream(stream)) = document.objects.get(id) else {
            continue;
        };
        let decoded =
            decode(stream, STREAM_LIMIT).context("a content stream could not be decoded")?;
        bytes.extend_from_slice(&decoded);
        bytes.push(b'\n');
    }
    let parsed = content::parse(&bytes, budget)
        .map_err(|e| anyhow!("a content stream could not be read: {e:?}"))?;
    Ok(content::encode(&parsed.items))
}

pub(super) fn deref<'a>(document: &'a Document, object: &'a Object) -> Option<&'a Object> {
    document.dereference(object).ok().map(|(_, o)| o)
}

pub(super) fn dict_of<'a>(document: &'a Document, object: &'a Object) -> Option<&'a Dictionary> {
    match deref(document, object)? {
        Object::Dictionary(d) => Some(d),
        Object::Stream(s) => Some(&s.dict),
        _ => None,
    }
}

pub(super) fn name<'a>(dict: &'a Dictionary, key: &[u8]) -> Option<&'a [u8]> {
    dict.get(key).ok().and_then(|o| o.as_name().ok())
}

pub(super) fn is_dangerous_action(dict: &Dictionary) -> bool {
    name(dict, b"S").is_some_and(|s| DANGEROUS_ACTIONS.contains(&s))
}

pub(super) fn is_dangerous_annotation(dict: &Dictionary) -> bool {
    name(dict, b"Subtype").is_some_and(|s| s == b"FileAttachment" || MEDIA_ANNOTATIONS.contains(&s))
}

pub(super) fn count_dict(dict: &Dictionary, counts: &mut Active) {
    match name(dict, b"S") {
        Some(b"JavaScript") => counts.javascript += 1,
        Some(b"Launch") => counts.launch += 1,
        Some(b"SubmitForm" | b"ImportData") => counts.form_submit += 1,
        Some(b"GoToR" | b"GoToE") => counts.remote_goto += 1,
        Some(b"Rendition" | b"Movie" | b"Sound" | b"RichMediaExecute") => counts.rich_media += 1,
        _ => {
            if dict.has(b"JS") {
                counts.javascript += 1;
            }
        }
    }
    if dict.has(b"AA") {
        counts.additional_actions += 1;
    }
    if name(dict, b"Type") == Some(b"EmbeddedFile") || dict.has(b"EF") {
        counts.embedded_file += 1;
    }
    match name(dict, b"Subtype") {
        Some(b"FileAttachment") => counts.embedded_file += 1,
        Some(s) if MEDIA_ANNOTATIONS.contains(&s) => counts.rich_media += 1,
        _ => {}
    }
    if dict.has(b"XFA") {
        counts.xfa += 1;
    }
}

pub(super) fn walk(object: &Object, depth: usize, counts: &mut Active) {
    if depth > WALK_DEPTH {
        return;
    }
    match object {
        Object::Dictionary(dict) => {
            count_dict(dict, counts);
            for (_, value) in dict.iter() {
                walk(value, depth + 1, counts);
            }
        }
        Object::Stream(stream) => {
            count_dict(&stream.dict, counts);
            for (_, value) in stream.dict.iter() {
                walk(value, depth + 1, counts);
            }
        }
        Object::Array(items) => {
            for item in items {
                walk(item, depth + 1, counts);
            }
        }
        _ => {}
    }
}

/// An open action that is not a plain "go to this page".
pub(super) fn open_action_runs(document: &Document, catalog: &Dictionary) -> bool {
    match catalog
        .get(b"OpenAction")
        .ok()
        .and_then(|o| deref(document, o))
    {
        Some(Object::Dictionary(action)) => name(action, b"S") != Some(b"GoTo"),
        _ => false,
    }
}

pub fn active_content(document: &Document) -> Active {
    let mut counts = Active::default();
    for object in document.objects.values() {
        walk(object, 0, &mut counts);
    }
    if let Ok(catalog) = document.catalog() {
        if open_action_runs(document, catalog) {
            counts.open_action += 1;
        }
        if catalog.has(b"Collection") {
            counts.embedded_file += 1;
        }
        if let Some(names) = catalog
            .get(b"Names")
            .ok()
            .and_then(|o| dict_of(document, o))
        {
            if names.has(b"JavaScript") {
                counts.javascript += 1;
            }
            if names.has(b"EmbeddedFiles") {
                counts.embedded_file += 1;
            }
        }
    }
    counts
}

pub fn hidden_layers(document: &Document) -> bool {
    let Ok(catalog) = document.catalog() else {
        return false;
    };
    let Some(properties) = catalog
        .get(b"OCProperties")
        .ok()
        .and_then(|o| dict_of(document, o))
    else {
        return false;
    };
    let Some(defaults) = properties.get(b"D").ok().and_then(|o| dict_of(document, o)) else {
        return false;
    };
    defaults
        .get(b"OFF")
        .ok()
        .and_then(|o| deref(document, o))
        .and_then(|o| o.as_array().ok())
        .is_some_and(|off| !off.is_empty())
}

/// The document information strings and the XMP packet, joined, for the Unicode count.
pub fn metadata_text(document: &Document) -> String {
    let mut out = String::new();
    if let Some(info) = document
        .trailer
        .get(b"Info")
        .ok()
        .and_then(|o| dict_of(document, o))
    {
        for (_, value) in info.iter() {
            if let Some(Object::String(bytes, _)) = deref(document, value) {
                out.push_str(&text::pdf_string(bytes));
                out.push('\n');
            }
        }
    }
    if let Ok(catalog) = document.catalog()
        && let Some(Object::Stream(stream)) = catalog
            .get(b"Metadata")
            .ok()
            .and_then(|o| deref(document, o))
        && let Ok(bytes) = stream.decompressed_content_with_limit(4 * 1024 * 1024)
    {
        out.push_str(&String::from_utf8_lossy(&bytes));
    }
    out
}
