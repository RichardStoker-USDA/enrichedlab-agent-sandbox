//! Removing dangerous entries while rebuilding.
use super::*;

/// Removes every active-content entry the counts above find, in place.
pub fn strip(document: &mut Document) {
    let dangerous: HashSet<ObjectId> = document
        .objects
        .iter()
        .filter_map(|(id, object)| {
            let dict = match object {
                Object::Dictionary(d) => d,
                Object::Stream(s) => &s.dict,
                _ => return None,
            };
            (is_dangerous_action(dict)
                || is_dangerous_annotation(dict)
                || name(dict, b"Type") == Some(b"EmbeddedFile"))
            .then_some(*id)
        })
        .collect();
    // The catalog's open action and name tree are removed by reference before the general pass.
    let catalog_id = document
        .trailer
        .get(b"Root")
        .ok()
        .and_then(|o| o.as_reference().ok());
    if let Some(id) = catalog_id {
        let names = document
            .get_dictionary(id)
            .ok()
            .and_then(|c| c.get(b"Names").ok())
            .and_then(|o| o.as_reference().ok());
        let run = document
            .get_dictionary(id)
            .map(|c| open_action_runs(document, c))
            .unwrap_or(false);
        if let Ok(Object::Dictionary(catalog)) = document.get_object_mut(id) {
            if run {
                catalog.remove(b"OpenAction");
            }
            catalog.remove(b"Collection");
            if let Ok(Object::Dictionary(direct)) = catalog.get_mut(b"Names") {
                direct.remove(b"JavaScript");
                direct.remove(b"EmbeddedFiles");
            }
        }
        if let Some(names) = names
            && let Ok(Object::Dictionary(tree)) = document.get_object_mut(names)
        {
            tree.remove(b"JavaScript");
            tree.remove(b"EmbeddedFiles");
        }
    }
    for object in document.objects.values_mut() {
        strip_object(object, &dangerous, 0);
    }
    for id in &dangerous {
        let embedded = matches!(document.objects.get(id), Some(Object::Stream(s)) if name(&s.dict, b"Type") == Some(b"EmbeddedFile"));
        if embedded {
            document.objects.remove(id);
        }
    }
}

pub(super) fn refers_to_dangerous(value: &Object, dangerous: &HashSet<ObjectId>) -> bool {
    match value {
        Object::Reference(id) => dangerous.contains(id),
        Object::Dictionary(d) => is_dangerous_action(d) || is_dangerous_annotation(d),
        _ => false,
    }
}

pub(super) fn strip_dict(dict: &mut Dictionary, dangerous: &HashSet<ObjectId>, depth: usize) {
    for key in [
        &b"AA"[..],
        b"JS",
        b"AF",
        b"EF",
        b"XFA",
        b"NeedsRendering",
        b"RichMediaContent",
        b"RichMediaSettings",
    ] {
        dict.remove(key);
    }
    if dict
        .get(b"A")
        .is_ok_and(|a| refers_to_dangerous(a, dangerous))
    {
        dict.remove(b"A");
    }
    if dict.has(b"S") {
        dict.remove(b"Next");
        if is_dangerous_action(dict) {
            for key in [
                &b"F"[..],
                b"Win",
                b"Mac",
                b"Unix",
                b"URL",
                b"FS",
                b"D",
                b"Rendition",
                b"R",
            ] {
                dict.remove(key);
            }
        }
    }
    if dict.get(b"OpenAction").is_ok_and(|a| {
        refers_to_dangerous(a, dangerous)
            || matches!(a, Object::Dictionary(d) if name(d, b"S") != Some(b"GoTo"))
    }) {
        dict.remove(b"OpenAction");
    }
    if let Ok(Object::Array(annotations)) = dict.get_mut(b"Annots") {
        annotations.retain(|a| !refers_to_dangerous(a, dangerous));
    }
    for (_, value) in dict.iter_mut() {
        strip_object(value, dangerous, depth + 1);
    }
}

pub(super) fn strip_object(object: &mut Object, dangerous: &HashSet<ObjectId>, depth: usize) {
    if depth > WALK_DEPTH {
        return;
    }
    match object {
        Object::Dictionary(dict) => strip_dict(dict, dangerous, depth),
        Object::Stream(stream) => strip_dict(&mut stream.dict, dangerous, depth),
        Object::Array(items) => {
            for item in items.iter_mut() {
                strip_object(item, dangerous, depth + 1);
            }
        }
        _ => {}
    }
}
