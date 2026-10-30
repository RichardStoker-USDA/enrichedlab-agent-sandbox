//! Reading the lead's and the specialist's replies, keeping refused replies, and the refusal records a run carries.
use super::*;

/// A refused turn's fixed reason code, and for refused code the names the block
/// called that admission refused, kept per run so the next lead call can correct it.
/// The model runs at temperature 0: without it, a retry repeats the refused
/// proposal. In memory only; one coordinator replica serves a tenant.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Refusal {
    pub reason: &'static str,
    pub names: Vec<String>,
    /// The analyses the refused reply's template steps chose (`ChosenAnalyses`), which the next call shows first
    /// (`analysis_view::focus_of`). Kept apart from `names` so no refused name can steer it; never logged.
    pub focus: Vec<String>,
}

/// A run's last recorded refusal, with the lease generation that recorded it, the run's turn and its failure count
/// after it. The next call uses it only while the run is still at that turn with that count, so a deferral recorded
/// since (whose response is still on its way), a delayed older one, or one from an earlier turn whose count recurs after
/// a commit reset it, can never steer `prior_attempt` or `redirect`.
#[derive(Clone)]
pub(super) struct KeptRefusal {
    pub(super) generation: i64,
    pub(super) turn: i64,
    pub(super) failures: i64,
    pub(super) refusal: Refusal,
}

pub(super) type Refusals = Arc<std::sync::Mutex<HashMap<Uuid, KeptRefusal>>>;

impl KeptRefusal {
    /// Whether this is still the run's last recorded refusal for a claim at `generation` whose run is at `turn` with
    /// `failures`.
    pub(super) fn current(&self, turn: i64, failures: i64, generation: i64) -> bool {
        self.turn == turn && self.failures == failures && self.generation < generation
    }

    /// Whether a deferral recorded at `generation` may replace `kept` (none, or one from an older generation).
    pub(super) fn replaces(kept: Option<&KeptRefusal>, generation: i64) -> bool {
        kept.is_none_or(|k| k.generation < generation)
    }
}

/// A block that admission refused. It displays as the fixed code alone, so logs and
/// run events never carry the names; `claimed` keeps them for the next lead call.
#[derive(Debug)]
pub struct CodeRefused {
    pub code: &'static str,
    pub names: Vec<String>,
}

impl std::fmt::Display for CodeRefused {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.code)
    }
}

impl std::error::Error for CodeRefused {}

/// The faults a refusal names, without the names r38 added only to guide the next call: where a refused number stood
/// (`where `, number refusals) and the fields an object allows (`allowed:`, schema refusals). Each is excluded only for
/// the reasons that write it, where no fault name can begin that way, so counts recorded before r38 stay comparable.
pub fn fault_count(reason: &str, names: &[String]) -> usize {
    let guidance: &str = match reason {
        "proposal_prose_number" | "method_review_prose_number" => "where ",
        "proposal_schema_invalid" => "allowed:",
        _ => return names.len(),
    };
    names.iter().filter(|n| !n.starts_with(guidance)).count()
}

/// A refused lead reply's error with the analyses its template steps chose, by catalog id as written, deduplicated and
/// bounded. It displays as the refusal it wraps, so the reason code and logs are unchanged; `refusal` reads it into
/// `Refusal::focus`.
#[derive(Debug)]
pub struct ChosenAnalyses {
    pub error: anyhow::Error,
    pub analyses: Vec<String>,
}

impl std::fmt::Display for ChosenAnalyses {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        std::fmt::Display::fmt(&self.error, f)
    }
}

impl std::error::Error for ChosenAnalyses {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(self.error.as_ref())
    }
}

/// The longest analysis id a refused reply's choice may carry into the next call's focus.
const CHOSEN_ID_BYTES: usize = 128;

/// `error` with the analyses `reply`'s steps chose (`actions[].analysis.feature`), when it chose any.
pub(super) fn with_chosen_analyses(error: anyhow::Error, reply: &Value) -> anyhow::Error {
    let mut analyses: Vec<String> = Vec::new();
    for action in reply["actions"].as_array().into_iter().flatten() {
        if let Some(id) = action["analysis"]["feature"]
            .as_str()
            .filter(|id| (1..=CHOSEN_ID_BYTES).contains(&id.len()))
            && !analyses.iter().any(|a| a == id)
            && analyses.len() < analysis_view::FOCUS_MAX
        {
            analyses.push(id.to_owned());
        }
    }
    if analyses.is_empty() {
        error
    } else {
        anyhow::Error::new(ChosenAnalyses { error, analyses })
    }
}

/// What the next lead call learns from a deferred turn: the fixed reason, and the
/// refused names when admission refused its code.
pub fn refusal(error: &anyhow::Error) -> Refusal {
    Refusal {
        reason: rejection_reason(&error.to_string()),
        names: error
            .chain()
            .find_map(|cause| cause.downcast_ref::<CodeRefused>())
            .map(|refused| refused.names.clone())
            .unwrap_or_default(),
        focus: error
            .chain()
            .find_map(|cause| cause.downcast_ref::<ChosenAnalyses>())
            .map(|chosen| chosen.analyses.clone())
            .unwrap_or_default(),
    }
}

/// A readable lead reply the checks refused, with the addresses of the context it answered and of the reply, both
/// kept under the run's lease. It displays as the refusal it wraps, so the reason code, the logs
/// and the next call's correction are unchanged; the deferral's event names the two addresses.
#[derive(Debug)]
pub struct RefusedReply {
    pub error: anyhow::Error,
    pub context_sha256: String,
    pub reply_sha256: String,
    /// The reply's reasoning, kept with it when the model gave some.
    pub reasoning_sha256: Option<String>,
}

impl std::fmt::Display for RefusedReply {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        std::fmt::Display::fmt(&self.error, f)
    }
}

impl std::error::Error for RefusedReply {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(self.error.as_ref())
    }
}

/// The most bytes one kept text may hold (migration 032's column check).
pub(super) const MAX_KEPT_TEXT: usize = 262_144;

/// The texts a committed decision keeps, with the receipt field naming each: both system prompts and contexts, and each
/// reply that fits a kept text. A reply the model padded past that size stays named by its hash in the receipt, unkept,
/// as replies were before migration 044, so the turn still commits.
pub(super) fn decision_texts<'a>(
    lead: &'a Call,
    review: &'a Call,
) -> (Vec<&'a [u8]>, Vec<&'a Value>) {
    decision_texts_with(lead, &lead.system, review)
}

pub(super) fn decision_texts_with<'a>(
    lead: &'a Call,
    system: &'a str,
    review: &'a Call,
) -> (Vec<&'a [u8]>, Vec<&'a Value>) {
    let mut texts: Vec<&[u8]> = vec![
        system.as_bytes(),
        lead.context.as_bytes(),
        REVIEW.as_bytes(),
        review.context.as_bytes(),
    ];
    let mut expected = vec![
        &lead.receipt["system_sha256"],
        &lead.receipt["context_sha256"],
        &review.receipt["system_sha256"],
        &review.receipt["context_sha256"],
    ];
    for call in [lead, review] {
        if !call.content.is_empty() && call.content.len() <= MAX_KEPT_TEXT {
            texts.push(call.content.as_bytes());
            expected.push(&call.receipt["content_sha256"]);
        }
        if !call.reasoning.is_empty() {
            texts.push(call.reasoning.as_bytes());
            expected.push(&call.receipt["reasoning_sha256"]);
        }
    }
    (texts, expected)
}

/// The texts a decision without a lead call keeps: the methods specialist's system prompt, context, reply and
/// reasoning. A turn that delegates an orientation's profiling steps kept the orientation's own texts when it was asked.
pub(super) fn review_texts(review: &Call) -> (Vec<&[u8]>, Vec<&Value>) {
    let mut texts: Vec<&[u8]> = vec![REVIEW.as_bytes(), review.context.as_bytes()];
    let mut expected = vec![
        &review.receipt["system_sha256"],
        &review.receipt["context_sha256"],
    ];
    if !review.content.is_empty() && review.content.len() <= MAX_KEPT_TEXT {
        texts.push(review.content.as_bytes());
        expected.push(&review.receipt["content_sha256"]);
    }
    if !review.reasoning.is_empty() {
        texts.push(review.reasoning.as_bytes());
        expected.push(&review.receipt["reasoning_sha256"]);
    }
    (texts, expected)
}

/// Keep a refused lead reply and the context it answered, and name both in the refusal. Keeping is best effort: when it
/// fails, or the kept addresses differ from the call's receipt, the refusal stands without them.
pub(super) async fn keep_refused_reply(
    store: &Store,
    run: Uuid,
    worker: &str,
    generation: i64,
    lead: &Call,
    error: anyhow::Error,
) -> anyhow::Error {
    if lead.content.is_empty() || lead.content.len() > MAX_KEPT_TEXT {
        return error;
    }
    let mut texts = vec![lead.context.as_bytes(), lead.content.as_bytes()];
    if !lead.reasoning.is_empty() {
        texts.push(lead.reasoning.as_bytes());
    }
    let kept = store
        .keep_research_contexts(run, worker, generation, &texts)
        .await;
    match kept {
        Ok(kept)
            if kept.len() == texts.len()
                && lead.receipt["context_sha256"].as_str() == Some(kept[0].as_str())
                && lead.receipt["content_sha256"].as_str() == Some(kept[1].as_str())
                && kept.get(2).is_none_or(|k| {
                    lead.receipt["reasoning_sha256"].as_str() == Some(k.as_str())
                }) =>
        {
            anyhow::Error::new(RefusedReply {
                error,
                context_sha256: kept[0].clone(),
                reply_sha256: kept[1].clone(),
                reasoning_sha256: kept.get(2).cloned(),
            })
        }
        _ => {
            tracing::warn!(%run, generation, "refused lead reply not kept");
            error
        }
    }
}

pub(super) const MAX_REFUSALS_KEPT: usize = 1024;

/// Read a model's answer as the one JSON object the prompts ask for, and name the wrapper
/// removed to read it. The reasoner's JSON constraint never reaches its answer (its reasoning
/// parser ends reasoning only at a tool call), so the shape is checked here. One surrounding
/// code fence is the only wrapper removed; anything else outside the object is refused with a
/// fixed word saying what was wrong.
pub fn read_reply(content: Option<&str>) -> Result<(Value, Option<&'static str>), &'static str> {
    let text = content.map(str::trim).unwrap_or_default();
    if text.is_empty() {
        return Err("no_answer");
    }
    if let Some(fenced) = text.strip_prefix("```") {
        let body = fenced.strip_suffix("```").filter(|b| !b.contains("```"));
        let body = body.map(|b| match b.get(..4) {
            Some(tag) if tag.eq_ignore_ascii_case("json") => &b[4..],
            _ => b,
        });
        return body
            .and_then(|b| one_object(b).ok())
            .map(|value| (value, Some("code_fence_removed")))
            .ok_or("code_fence");
    }
    one_object(text).map(|value| (value, None))
}

pub(super) fn one_object(text: &str) -> Result<Value, &'static str> {
    let text = text.trim();
    if !text.starts_with('{') {
        return Err(if text.starts_with('[') || !text.contains('{') {
            "not_an_object"
        } else {
            "text_before_object"
        });
    }
    let mut values = serde_json::Deserializer::from_str(text).into_iter::<Value>();
    match values.next() {
        Some(Ok(value)) if text[values.byte_offset()..].trim().is_empty() => Ok(value),
        Some(Ok(_)) => Err("text_after_object"),
        Some(Err(e)) if e.is_eof() => Err("incomplete_object"),
        _ => Err("invalid_object"),
    }
}

/// The read of one reply from its finish reason and content: a reply cut at the length limit (`out_of_room`) or not
/// finished (`unfinished`) is a fault before its text is read.
pub fn read_finished_reply(
    finish_reason: Option<&str>,
    content: Option<&str>,
) -> Result<(Value, Option<&'static str>), &'static str> {
    match finish_reason {
        Some("length") => Err("out_of_room"),
        Some("stop") => read_reply(content),
        _ => Err("unfinished"),
    }
}

/// The refusal for a reply that could not be read: whose reply it was and what was wrong, both
/// fixed words, so the next call can correct it and the deferred turn's log line can carry it.
pub fn unreadable_reply(role: &str, fault: &'static str) -> CodeRefused {
    CodeRefused {
        code: match fault {
            "no_answer" => "model_content_missing",
            "out_of_room" | "unfinished" => "model_response_incomplete",
            _ => "model_content_not_json",
        },
        names: vec![format!("{role}: {fault}")],
    }
}

/// Calls one reply may take before an unreadable one refuses the turn. The reasoner's decoding
/// differs between calls even at temperature 0, and two requests whose live reply had no answer
/// returned a readable one in 13 of 14 replays, so an unreadable reply is asked for
/// again before it costs the turn one of its three refusals.
pub const READ_ATTEMPTS: usize = 3;

/// The context an unreadable reply is asked for again with: once a reply could not be read, the same context with
/// `previous_reply_unreadable` naming why and which attempt this is. The same text came back with no answer three
/// times at near-identical sizes in a test run, as it had for the specialist in three others. None for the first
/// call, which is sent unchanged; the call keeps the text it sent, so the kept context is the one its reply answered.
pub fn with_unreadable_note(context: &Value, earlier: &[String]) -> Option<Value> {
    let fault = earlier.last()?;
    let mut asked = context.as_object()?.clone();
    asked.insert(
        "previous_reply_unreadable".into(),
        json!({"fault": fault, "attempt": earlier.len() + 1}),
    );
    Some(Value::Object(asked))
}

/// The fault to record when an error is an unreadable reply that may be asked for again after
/// `calls` calls. A reply that reached its length limit already spent the whole output budget, and
/// every other refusal is a fault in what the reply said, so neither is asked for again.
pub fn ask_again(error: &anyhow::Error, calls: usize) -> Option<String> {
    error
        .downcast_ref::<CodeRefused>()
        .filter(|r| matches!(r.code, "model_content_missing" | "model_content_not_json"))
        .filter(|_| calls < READ_ATTEMPTS)
        .and_then(|r| r.names.first().cloned())
}
