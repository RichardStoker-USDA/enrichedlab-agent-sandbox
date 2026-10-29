//! Mutation checks, model pointers and collection access.
use super::*;

pub(super) fn validate_mutation(mutation: &StateMutation) -> Result<()> {
    if mutation.schema_version != 1 {
        bail!("unsupported state mutation schema");
    }
    validate_identifier(&mutation.run_id, "run id")?;
    validate_identifier(&mutation.subject, "subject")?;
    validate_identifier(&mutation.collection, "collection")?;
    if mutation.set.len().saturating_add(mutation.remove.len()) == 0
        || mutation.set.len().saturating_add(mutation.remove.len()) > MAX_POINTERS
    {
        bail!("state mutation must contain a bounded set or remove operation");
    }
    let mut pointers = BTreeSet::new();
    for pointer in mutation.set.keys().chain(mutation.remove.iter()) {
        validate_pointer(pointer)?;
        if !pointers.insert(pointer) {
            bail!("state mutation repeats a JSON pointer");
        }
    }
    if let Some(rationale) = &mutation.rationale {
        validate_text(&rationale.summary, "rationale summary", 8192)?;
        if rationale.alternatives.len() > 64 || rationale.evidence.len() > 64 {
            bail!("state rationale arrays exceed their caps");
        }
        for value in rationale
            .alternatives
            .iter()
            .chain(rationale.evidence.iter())
        {
            validate_text(value, "state rationale value", 4096)?;
        }
        if let Some(confidence) = rationale.confidence
            && (!confidence.is_finite() || !(0.0..=1.0).contains(&confidence))
        {
            bail!("state rationale confidence must be between zero and one");
        }
    }
    Ok(())
}

pub(super) fn validate_model_pointers<'a>(
    collection: &str,
    pointers: impl Iterator<Item = &'a String>,
) -> Result<()> {
    for pointer in pointers {
        let protected = match collection {
            "execution" => pointer == "/objective" || pointer.starts_with("/objective/"),
            "scientific" => [
                "/input_refs",
                "/method_refs",
                "/intermediate_refs",
                "/claim_refs",
                "/validation_refs",
            ]
            .iter()
            .any(|prefix| pointer == prefix || pointer.starts_with(&format!("{prefix}/"))),
            _ => false,
        };
        if protected {
            bail!("model state mutation targets a machine- or operator-owned pointer: {pointer}");
        }
    }
    Ok(())
}

pub(super) fn set_pointer(document: &mut Value, pointer: &str, value: Value) -> Result<()> {
    if pointer.is_empty() {
        *document = value;
        return Ok(());
    }
    let tokens = pointer_tokens(pointer)?;
    let (last, parents) = tokens.split_last().context("state pointer is empty")?;
    let mut current = document;
    for token in parents {
        let object = current
            .as_object_mut()
            .context("state pointers may traverse objects only")?;
        current = object.entry(token.clone()).or_insert_with(|| json!({}));
    }
    current
        .as_object_mut()
        .context("state pointer parent must be an object")?
        .insert(last.clone(), value);
    Ok(())
}

pub(super) fn remove_pointer(document: &mut Value, pointer: &str) -> Result<()> {
    if pointer.is_empty() {
        *document = Value::Null;
        return Ok(());
    }
    let tokens = pointer_tokens(pointer)?;
    let (last, parents) = tokens.split_last().context("state pointer is empty")?;
    let mut current = document;
    for token in parents {
        current = current
            .as_object_mut()
            .and_then(|object| object.get_mut(token))
            .with_context(|| format!("state remove pointer does not exist: {pointer}"))?;
    }
    let removed = current
        .as_object_mut()
        .context("state remove pointer parent must be an object")?
        .remove(last);
    if removed.is_none() {
        bail!("state remove pointer does not exist: {pointer}");
    }
    Ok(())
}

pub(super) fn select_pointers_bounded(
    document: &Value,
    pointers: &[String],
    max_bytes: usize,
) -> Result<(Value, Vec<String>)> {
    let mut selected = json!({});
    let mut truncated = Vec::new();
    for pointer in pointers {
        let value = if pointer.is_empty() {
            document
        } else if let Some(value) = document.pointer(pointer) {
            value
        } else {
            continue;
        };
        let mut candidate = selected.clone();
        set_pointer(&mut candidate, pointer, value.clone())?;
        if serde_json::to_vec(&candidate)?.len() <= max_bytes {
            selected = candidate;
            continue;
        }
        let Some(object) = value.as_object() else {
            truncated.push(if pointer.is_empty() {
                "/".into()
            } else {
                pointer.clone()
            });
            continue;
        };
        let mut added = 0usize;
        for (key, child) in object {
            let child_pointer = if pointer.is_empty() {
                format!("/{}", encode_pointer_token(key))
            } else {
                format!("{pointer}/{}", encode_pointer_token(key))
            };
            let mut candidate = selected.clone();
            set_pointer(&mut candidate, &child_pointer, child.clone())?;
            if serde_json::to_vec(&candidate)?.len() > max_bytes {
                continue;
            }
            selected = candidate;
            added += 1;
        }
        if added < object.len() {
            truncated.push(if pointer.is_empty() {
                "/".into()
            } else {
                pointer.clone()
            });
        }
    }
    Ok((selected, truncated))
}

pub(super) fn encode_pointer_token(value: &str) -> String {
    value.replace('~', "~0").replace('/', "~1")
}

pub(super) fn pointer_tokens(pointer: &str) -> Result<Vec<String>> {
    validate_pointer(pointer)?;
    if pointer.is_empty() {
        return Ok(Vec::new());
    }
    pointer[1..]
        .split('/')
        .map(|token| {
            let mut decoded = String::new();
            let mut chars = token.chars();
            while let Some(character) = chars.next() {
                if character == '~' {
                    match chars.next() {
                        Some('0') => decoded.push('~'),
                        Some('1') => decoded.push('/'),
                        _ => bail!("invalid JSON pointer escape"),
                    }
                } else {
                    decoded.push(character);
                }
            }
            if decoded.is_empty() || decoded.len() > 256 {
                bail!("state JSON pointer token is invalid");
            }
            Ok(decoded)
        })
        .collect()
}

pub(super) fn validate_pointer(pointer: &str) -> Result<()> {
    if pointer.len() > 4096
        || (!pointer.is_empty() && !pointer.starts_with('/'))
        || pointer.contains(['\0', '\n', '\r'])
    {
        bail!("invalid bounded JSON pointer");
    }
    Ok(())
}

pub(super) fn allows(
    collection: &StateCollection,
    audience: StateAudience,
    principal: &str,
) -> bool {
    collection.access.iter().any(|access| {
        access.audience == audience
            && (access.principals.is_empty()
                || access.principals.iter().any(|allowed| allowed == principal))
    })
}

pub(super) fn initial_access(subject: &str) -> Vec<CollectionAccess> {
    vec![
        CollectionAccess {
            audience: StateAudience::Agent,
            principals: vec![subject.into()],
        },
        CollectionAccess {
            audience: StateAudience::Lead,
            principals: Vec::new(),
        },
        CollectionAccess {
            audience: StateAudience::Oversight,
            principals: Vec::new(),
        },
    ]
}
