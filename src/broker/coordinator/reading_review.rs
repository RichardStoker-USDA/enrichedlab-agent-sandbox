//! Kept read calls and cited parts for the methods review.
use super::*;

/// The texts of each read call (its context, reply and reasoning) the turn keeps, with the digests their receipts name.
pub(in super::super) fn call_texts(calls: &[Call]) -> (Vec<&[u8]>, Vec<&Value>) {
    let mut texts: Vec<&[u8]> = Vec::new();
    let mut expected: Vec<&Value> = Vec::new();
    let mut seen = HashSet::new();
    for call in calls {
        for (text, digest) in [
            (call.context.as_str(), &call.receipt["context_sha256"]),
            (call.content.as_str(), &call.receipt["content_sha256"]),
            (call.reasoning.as_str(), &call.receipt["reasoning_sha256"]),
        ] {
            if !text.is_empty() && text.len() <= MAX_KEPT_TEXT && seen.insert(digest.to_string()) {
                texts.push(text.as_bytes());
                expected.push(digest);
            }
        }
    }
    (texts, expected)
}

/// The parts the proposal's actions cite, for the methods review: each one's ref, paper, heading or label and the start
/// of its text, within `room` bytes.
pub(in super::super) fn cited(reading: &Value, proposal: &Proposal, room: usize) -> Option<Value> {
    let cited: HashSet<&str> = proposal
        .actions
        .iter()
        .flat_map(|a| a.literature_refs.iter().map(String::as_str))
        .collect();
    let mut out = Vec::new();
    let mut used = 2;
    for p in reading["parts"].as_array().into_iter().flatten() {
        if !p["ref"].as_str().is_some_and(|r| cited.contains(r)) {
            continue;
        }
        let study_text = (p["part"] == "study").then(|| {
            json!({"label": p["label"], "links_label": p["links_label"], "facts": p["facts"], "links": p["links"]}).to_string()
        });
        let text: String = ["text", "caption", "abstract"]
            .iter()
            .find_map(|k| p[*k].as_str())
            .or(study_text.as_deref())
            .unwrap_or_default()
            .chars()
            .take(REVIEW_PART_CHARS)
            .collect();
        let heading = if p["heading"].is_null() {
            p["label"].clone()
        } else {
            p["heading"].clone()
        };
        let entry = json!({"ref": p["ref"], "part": p["part"], "paper": p["paper"], "heading": heading,
            "text": text});
        let n = size(&entry) + 1;
        if used + n > room {
            continue;
        }
        used += n;
        out.push(entry);
    }
    (!out.is_empty()).then_some(Value::Array(out))
}
