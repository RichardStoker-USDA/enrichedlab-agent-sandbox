//! Reading for the research lead. Search shows the lead a dozen short passages; to learn what earlier
//! work did, how and with what, the lead reads papers whole. Where an operator lets a lab read (`--lead-reading-labs`,
//! `--lead-reading-bytes`) and the context showed passages from that lab's own library, the context offers reading: a
//! reply `{"action":"read","summary":…,"reads":[…]}` asks for one to four parts, the coordinator reads them from the
//! lab's corpus under the run's lease and the lab's acceptance, screens them as lab text, and calls the
//! lead again with what it read. A turn allows `ROUNDS_MAX` read rounds within the configured bytes; the oldest parts
//! leave when newer ones need the room. The lead may cite a part it read in an action's `literature_refs`. The turn
//! records every round: the call, the refs asked for, each part shown with its size and digest, and what was withheld,
//! refused or dropped, never the text, which the kept call contexts hold.
//!
//! A ref names a part the lead was shown or one derived from a shown paper:
//! `library://<paper>` the paper's outline; `library://<paper>/role/<role>` every section with that role;
//! `library://<paper>/section/<key>` a section an outline listed, and `…@<n>` its continuation from paragraph `n`;
//! `library://<paper>/figure/<n>` and `…/table/<n>` the outline's n-th figure or table; a passage's own ref its section.
use super::*;
use crate::broker::library::{LibraryError, LibraryService};
use crate::broker::paper::{self, Paper, ROLES, Section};
use std::sync::Arc;

#[path = "reading_ledger.rs"]
mod ledger;
#[path = "reading_parts.rs"]
mod parts;
#[path = "reading_specialist.rs"]
mod specialist;
#[path = "reading_study.rs"]
mod study;
#[cfg(test)]
use parts::figure_part;
use parts::screened;
#[path = "reading_review.rs"]
mod review;
pub(super) use review::{call_texts, cited};

/// Parts one reply may ask for.
pub(super) const READS_MAX: usize = 4;
/// Read rounds one turn allows before the lead must decide.
pub(super) const ROUNDS_MAX: usize = 3;
/// Bytes the offer, paper refs and reading fields may add beyond the configured reading bytes.
pub(super) const OFFER_BYTES: usize = 4096;
/// The reading bytes an operator may configure.
pub(super) const READING_BYTES_MIN: usize = 16_384;
pub(super) const READING_BYTES_MAX: usize = 196_608;
/// Characters of a read reply's summary.
const SUMMARY_CHARS: usize = 1000;
/// Characters of one ref.
const REF_CHARS: usize = 300;
/// Sections, figures and tables one outline lists.
const OUTLINE_PARTS_MAX: usize = 80;
const OUTLINE_CAPTION_CHARS: usize = 300;
const ABSTRACT_CHARS: usize = 3000;
const AUTHORS_MAX: usize = 12;
const FIGURE_TEXT_CHARS: usize = 2000;
/// Characters of each cited part the methods review reads.
const REVIEW_PART_CHARS: usize = 1200;

const OFFER: &str = "You may read this lab's papers whole before you decide. Reply {\"action\":\"read\",\"summary\":\"why\",\"reads\":[refs]} with 1 to 4 refs. A literature passage's paper_ref gives that paper's outline: its sections, figures and tables, each with a ref. A paper_ref followed by /role/ and one of methods, results, discussion, introduction, abstract, conclusion, data_availability or supplementary gives every section with that role. A passage ref gives its whole section. A section, figure or table ref from an outline gives that part. What you read appears in reading. Cite a part you rely on in an action's literature_refs.";
const SPENT: &str = "No reads remain this turn: reply with your decision. Cite a part you relied on in an action's literature_refs.";

/// A requested ref with its paper and target, or the code that refuses it.
type Resolved = (String, std::result::Result<(String, Target), &'static str>);

/// A part ready to show: its JSON, what the screen held back, and the refs it lets the lead name once a call holds it.
pub(super) struct Built {
    pub part: Value,
    pub withheld: Vec<Value>,
    pub refs: Vec<String>,
}

/// A part admitted during the current round, settled when the round ends: shown when the next call holds it, dropped
/// when a later part of the same round took its room first.
struct Admitted {
    reference: Value,
    kind: Value,
    bytes: usize,
    part_sha256: String,
    refs: Vec<String>,
}

/// What a ref names within its paper.
#[derive(Clone, Debug, PartialEq)]
pub(super) enum Target {
    Outline,
    Study,
    Role(String),
    Section { key: String, from: usize },
    Passage,
    Figure(usize),
    Table(usize),
}

/// A read reply's request.
#[derive(Clone, Debug, PartialEq)]
pub(super) struct ReadRequest {
    pub summary: String,
    pub reads: Vec<String>,
}

/// Whether a reply asks to read.
pub(super) fn asks_to_read(reply: &Value) -> bool {
    reply["action"] == "read"
}

/// A read reply's request, or the code that names why it cannot be read: only `action`, `summary` (clean text of at most
/// 1,000 characters) and `reads` (one to four distinct refs of at most 300 characters).
pub(super) fn request(reply: &Value) -> std::result::Result<ReadRequest, &'static str> {
    let fields = reply.as_object().ok_or("read_request_invalid")?;
    if fields
        .keys()
        .any(|k| !matches!(k.as_str(), "action" | "summary" | "reads"))
    {
        return Err("read_request_invalid");
    }
    let summary = fields
        .get("summary")
        .and_then(Value::as_str)
        .filter(|s| clean_text(s, SUMMARY_CHARS))
        .ok_or("read_request_invalid")?;
    let reads: Vec<String> = fields
        .get("reads")
        .and_then(Value::as_array)
        .ok_or("read_request_invalid")?
        .iter()
        .map(|r| r.as_str().map(str::to_owned))
        .collect::<Option<_>>()
        .ok_or("read_request_invalid")?;
    if reads.is_empty()
        || reads.len() > READS_MAX
        || reads.iter().collect::<HashSet<_>>().len() != reads.len()
        || reads.iter().any(|r| r.is_empty() || r.len() > REF_CHARS)
    {
        return Err("read_request_invalid");
    }
    Ok(ReadRequest {
        summary: summary.to_owned(),
        reads,
    })
}

/// A ref's paper and target, or `None` when the ref is not one this grammar writes. The paper is a canonical UUID.
pub(super) fn parse_ref(reference: &str) -> Option<(String, Target)> {
    let rest = reference.strip_prefix("library://")?;
    let (paper, tail) = match rest.split_once('/') {
        Some((paper, tail)) => (paper, Some(tail)),
        None => (rest, None),
    };
    if Uuid::parse_str(paper).ok()?.to_string() != paper {
        return None;
    }
    let number = |n: &str| {
        n.parse::<usize>()
            .ok()
            .filter(|v| (1..=paper::MAX_PARTS).contains(v) && v.to_string() == n)
    };
    let target = match tail {
        None => Target::Outline,
        Some("study") => Target::Study,
        Some(tail) => {
            if let Some(role) = tail.strip_prefix("role/") {
                ROLES
                    .contains(&role)
                    .then(|| Target::Role(role.to_owned()))?
            } else if let Some(section) = tail.strip_prefix("section/") {
                let (key, from) = match section.split_once('@') {
                    Some((key, written)) => {
                        let from: usize = written.parse().ok()?;
                        if from == 0 || from.to_string() != written {
                            return None;
                        }
                        (key, from)
                    }
                    None => (section, 0),
                };
                paper::section_key_valid(key).then(|| Target::Section {
                    key: key.to_owned(),
                    from,
                })?
            } else if let Some(n) = tail.strip_prefix("figure/") {
                Target::Figure(number(n)?)
            } else if let Some(n) = tail.strip_prefix("table/") {
                Target::Table(number(n)?)
            } else if Uuid::parse_str(tail).ok()?.to_string() == tail {
                Target::Passage
            } else {
                return None;
            }
        }
    };
    Some((paper.to_owned(), target))
}

/// Where a shown passage sits in its paper: the knowledge service's section key when the knowledge search found it,
/// otherwise its heading path.
#[derive(Clone, Debug)]
struct Place {
    key: Option<String>,
    headings: Vec<String>,
}

/// A paper the lead may read: one with a passage the context showed.
#[derive(Clone, Debug)]
struct Shelf {
    document: Uuid,
    passages: HashMap<String, Place>,
}

/// One turn's reading: the papers the context showed, the refs the lead may name, the parts it holds and each round.
pub(super) struct Reading {
    bytes: usize,
    accepted_only: bool,
    study_fields: HashSet<String>,
    studies: HashMap<Uuid, std::result::Result<Option<knowledge::StudyRecord>, &'static str>>,
    papers: HashMap<String, Shelf>,
    shown: HashSet<String>,
    parts: Vec<Value>,
    rounds: Vec<Value>,
    parsed: HashMap<Uuid, std::result::Result<Arc<Paper>, &'static str>>,
}

impl Reading {
    /// Reading for this turn, when the lab may read and the context showed at least one passage from the lab's own
    /// library. Gives each such shown passage its `paper_ref` and adds the offer to the context.
    pub(super) fn offer(
        lab_text: &LabText,
        lab: &str,
        results: &Value,
        context: &mut Value,
    ) -> Option<Self> {
        if lab_text.reading_bytes == 0 || !lab_text.reading.contains(lab) {
            return None;
        }
        let mut papers: HashMap<String, Shelf> = HashMap::new();
        for p in results["literature"]["passages"]
            .as_array()
            .into_iter()
            .flatten()
        {
            if p["origin"] == "commons" {
                continue;
            }
            let (Some(paper), Some(document), Some(reference)) = (
                p["paper_id"].as_str(),
                p["document_id"]
                    .as_str()
                    .and_then(|d| Uuid::parse_str(d).ok()),
                p["ref"].as_str(),
            ) else {
                continue;
            };
            let shelf = papers.entry(paper.to_owned()).or_insert_with(|| Shelf {
                document,
                passages: HashMap::new(),
            });
            if shelf.document == document {
                shelf.passages.insert(
                    reference.to_owned(),
                    Place {
                        key: p["section_key"].as_str().map(str::to_owned),
                        headings: texts(&p["section"]),
                    },
                );
            }
        }
        let mut shown = HashSet::new();
        let mut readable = HashSet::new();
        for p in context["literature"].as_array_mut().into_iter().flatten() {
            let Some(reference) = p["ref"].as_str().map(str::to_owned) else {
                continue;
            };
            let Some((paper, Target::Passage)) = parse_ref(&reference) else {
                continue;
            };
            if !papers
                .get(&paper)
                .is_some_and(|shelf| shelf.passages.contains_key(&reference))
            {
                continue;
            }
            p["paper_ref"] = json!(format!("library://{paper}"));
            readable.insert(paper);
            shown.insert(reference);
        }
        if readable.is_empty() {
            return None;
        }
        papers.retain(|paper, _| readable.contains(paper));
        let reading = Self {
            bytes: lab_text.reading_bytes,
            accepted_only: !lab_text.unreviewed.contains(lab),
            study_fields: lab_text.study_fields.clone(),
            studies: HashMap::new(),
            papers,
            shown,
            parts: Vec::new(),
            rounds: Vec::new(),
            parsed: HashMap::new(),
        };
        reading.place(context);
        Some(reading)
    }

    /// The input bytes a lead call may use beyond the model's ceiling while reading is offered.
    pub(super) fn allowance(&self) -> usize {
        self.bytes + OFFER_BYTES
    }

    pub(super) fn rounds_left(&self) -> usize {
        ROUNDS_MAX.saturating_sub(self.rounds.len())
    }

    /// A ref's paper and target, when the lead may read it: a shown paper's outline or role, or a ref the context
    /// showed (a passage, or a part an outline or a cut section listed).
    fn resolve(&self, reference: &str) -> std::result::Result<(String, Target), &'static str> {
        let (paper, target) = parse_ref(reference).ok_or("read_ref_invalid")?;
        if target == Target::Study && self.study_fields.is_empty() {
            return Err("read_ref_invalid");
        }
        if !self.papers.contains_key(&paper) {
            return Err("read_ref_unknown");
        }
        match target {
            Target::Outline | Target::Role(_) => {}
            _ if self.shown.contains(reference) => {}
            _ => return Err("read_ref_not_shown"),
        }
        Ok((paper, target))
    }

    /// One read round: the request's parts, each checked against the run's library under its lease, read from the lab's
    /// corpus, screened and fitted; a request that cannot be read is recorded and shown with its code.
    #[allow(clippy::too_many_arguments)]
    pub(super) async fn read(
        &mut self,
        store: &Store,
        library: Option<&LibraryService>,
        knowledge: Option<&KnowledgeService>,
        run: Uuid,
        worker: &str,
        generation: i64,
        lab: &str,
        call: &Call,
    ) -> Result<()> {
        let mut round = json!({"lead": call.receipt, "requested": [], "shown": [], "withheld": [],
            "refused": [], "evicted": [], "dropped": []});
        let mut admitted = Vec::new();
        match request(&call.value) {
            Err(code) => {
                round["refused_request"] = json!(code);
                round["request_sha256"] = json!(sha(call.content.as_bytes()));
                round["request_bytes"] = json!(call.content.len());
            }
            Ok(asked) => {
                round["summary_sha256"] = json!(sha(asked.summary.as_bytes()));
                round["requested"] = json!(asked.reads);
                let targets: Vec<Resolved> = asked
                    .reads
                    .iter()
                    .map(|r| (r.clone(), self.resolve(r)))
                    .collect();
                let mut documents: Vec<Uuid> = targets
                    .iter()
                    .filter_map(|(_, t)| t.as_ref().ok())
                    .map(|(paper, _)| self.papers[paper].document)
                    .collect();
                documents.sort();
                documents.dedup();
                // Membership and acceptance again, for exactly these papers, under the run's lease.
                let held = if documents.is_empty() {
                    json!({"member": true, "papers": []})
                } else {
                    store
                        .research_run_library(run, worker, generation, &documents)
                        .await?
                };
                let library_papers: HashMap<&str, &Value> = held["papers"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter_map(|p| Some((p["document_id"].as_str()?, p)))
                    .collect();
                for (reference, target) in targets {
                    let refused = |round: &mut Value, code: &str| {
                        push(round, "refused", json!({"ref": reference, "code": code}));
                    };
                    let (paper_id, target) = match target {
                        Ok(found) => found,
                        Err(code) => {
                            refused(&mut round, code);
                            continue;
                        }
                    };
                    if held["member"] != true {
                        refused(&mut round, "read_not_member");
                        continue;
                    }
                    let document = self.papers[&paper_id].document;
                    match library_papers.get(document.to_string().as_str()) {
                        Some(p) if p["paper_id"].as_str() != Some(paper_id.as_str()) => {
                            refused(&mut round, "read_not_in_library");
                            continue;
                        }
                        Some(p) if self.accepted_only && p["accepted"] != true => {
                            refused(&mut round, "read_not_accepted");
                            continue;
                        }
                        Some(_) => {}
                        None => {
                            refused(&mut round, "read_not_in_library");
                            continue;
                        }
                    }
                    let Some(service) = library.filter(|l| l.configured(lab)) else {
                        refused(&mut round, "library_not_configured");
                        continue;
                    };
                    if let std::collections::hash_map::Entry::Vacant(slot) =
                        self.parsed.entry(document)
                    {
                        slot.insert(
                            paper::read(service, lab, document)
                                .await
                                .map(Arc::new)
                                .map_err(LibraryError::code),
                        );
                    }
                    let parsed = match &self.parsed[&document] {
                        Ok(parsed) => Arc::clone(parsed),
                        Err(code) => {
                            refused(&mut round, code);
                            continue;
                        }
                    };
                    let parts = match self
                        .parts_for(
                            service, knowledge, run, lab, &paper_id, document, &parsed, &reference,
                            target,
                        )
                        .await
                    {
                        Ok(parts) => parts,
                        Err(code) => {
                            refused(&mut round, code);
                            continue;
                        }
                    };
                    for built in parts {
                        for w in built.withheld {
                            push(&mut round, "withheld", w);
                        }
                        self.admit(built.part, built.refs, &mut round, &mut admitted);
                    }
                }
            }
        }
        self.settle(&mut round, admitted);
        self.rounds.push(round);
        Ok(())
    }

    /// Adds a part to what the lead holds, dropping the oldest parts it no longer has room for; the same part read
    /// again replaces its earlier copy. A part larger than the whole reading is refused. A part from an earlier round
    /// that leaves is `evicted`; a part of this round is settled when the round ends.
    fn admit(
        &mut self,
        part: Value,
        refs: Vec<String>,
        round: &mut Value,
        admitted: &mut Vec<Admitted>,
    ) {
        let n = size(&part);
        let reference = part["ref"].clone();
        if n > self.bytes {
            push(
                round,
                "refused",
                json!({"ref": reference, "code": "read_part_too_large"}),
            );
            return;
        }
        if let Some(i) = self.parts.iter().position(|p| p["ref"] == reference) {
            self.parts.remove(i);
        }
        admitted.retain(|a| a.reference != reference);
        while self.parts.iter().map(size).sum::<usize>() + n > self.bytes && !self.parts.is_empty()
        {
            let old = self.parts.remove(0);
            if !admitted.iter().any(|a| a.reference == old["ref"]) {
                push(round, "evicted", old["ref"].clone());
            }
        }
        admitted.push(Admitted {
            reference,
            kind: part["part"].clone(),
            bytes: n,
            part_sha256: sha(&serde_json::to_vec(&part).unwrap_or_default()),
            refs,
        });
        self.parts.push(part);
    }

    /// Ends a round: a part the next call will hold is `shown` and the refs it lists become readable; a part this round
    /// admitted and then dropped for a later one is `dropped`, never seen by a call, so its refs stay unreadable.
    fn settle(&mut self, round: &mut Value, admitted: Vec<Admitted>) {
        for a in admitted {
            if self.parts.iter().any(|p| p["ref"] == a.reference) {
                push(
                    round,
                    "shown",
                    json!({"ref": a.reference, "part": a.kind, "bytes": a.bytes,
                        "part_sha256": a.part_sha256}),
                );
                self.shown.extend(a.refs);
                if let Some(reference) = a.reference.as_str() {
                    self.shown.insert(reference.to_owned());
                }
            } else {
                push(
                    round,
                    "dropped",
                    json!({"ref": a.reference, "bytes": a.bytes}),
                );
            }
        }
    }

    /// The offer and, after a round, what the lead holds with the last round's refusals and drops.
    pub(super) fn place(&self, context: &mut Value) {
        let left = self.rounds_left();
        let how = if left == 0 {
            SPENT.to_owned()
        } else if self.study_fields.is_empty() {
            OFFER.to_owned()
        } else {
            format!(
                "{OFFER} An outline's /study ref gives its audited study facts and catalog links."
            )
        };
        context["reading_offer"] = json!({"how": how,
            "rounds_left": left, "bytes": self.bytes});
        if let Some(last) = self.rounds.last() {
            let mut reading = json!({"parts": self.parts});
            for key in ["refused", "evicted", "dropped"] {
                if last[key].as_array().is_some_and(|a| !a.is_empty()) {
                    reading[key] = last[key].clone();
                }
            }
            if let Some(n) = last["withheld"].as_array().map(Vec::len).filter(|n| *n > 0) {
                reading["withheld"] = json!(n);
            }
            if let Some(code) = last["refused_request"].as_str() {
                reading["refused_request"] = json!(code);
            }
            context["reading"] = reading;
        }
    }
}

/// Whether the configured reading bytes fit the model's declared limits beside its input ceiling.
pub(super) fn allowance_valid(config: &ModelConfig, bytes: usize) -> bool {
    (READING_BYTES_MIN..=READING_BYTES_MAX).contains(&bytes)
        && config.max_input_bytes + bytes + OFFER_BYTES <= MAX_INPUT_BYTES
        && config.max_input_bytes + bytes + OFFER_BYTES + config.max_output_tokens + 1024
            <= config.context_window_tokens
}

fn push(round: &mut Value, key: &str, item: Value) {
    if let Some(list) = round[key].as_array_mut() {
        list.push(item);
    }
}

fn texts(value: &Value) -> Vec<String> {
    value
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|v| v.as_str().map(str::to_owned))
        .collect()
}

fn cut(text: &str, chars: usize) -> String {
    let text = text.trim();
    if text.chars().count() <= chars {
        return text.to_owned();
    }
    let mut out: String = text.chars().take(chars).collect();
    out.push('…');
    out
}

fn cut_bytes(text: &str, bytes: usize) -> String {
    let mut end = bytes.min(text.len());
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    text[..end].to_owned()
}

fn size(value: &Value) -> usize {
    serde_json::to_vec(value).map_or(usize::MAX, |b| b.len())
}

#[cfg(test)]
#[path = "reading_tests.rs"]
mod tests;
