//! Naming the refs a lead reply may not cite. A lesson rests on results and on parts of papers the
//! lead read; an action's `literature_refs` name passages it was shown and parts it read. A test run's lead cited
//! sections that an outline it read listed, and a passage it was only shown, as lessons' evidence. Each refusal named
//! no ref, so the next two attempts repeated it and the run paused. A refusal now names every such ref with what to do
//! instead, for the next lead call alone (`with_prior_attempt`); like every refusal name, never for a log or event.
use super::*;

/// The most refs one refusal names; `with_prior_attempt` bounds them again by bytes.
const NAMES_MAX: usize = 12;
/// Bytes a name may hold before `bounded_name` cuts it.
const NAME_BYTES: usize = 300;
/// Characters of an action key a name repeats.
const KEY_CHARS: usize = 40;
const LISTED: &str = "was listed in a part you read but not read itself: read it before citing it";
const LISTED_UNREADABLE: &str = "was listed in a part you read but not read itself";
const PASSAGE: &str = "is a passage you were shown: lessons cite parts in reading.parts; read its section and cite that";
const PASSAGE_UNREADABLE: &str =
    "is a passage you were shown, not a result: cite it only in literature_refs";
const PAPER: &str =
    "is a paper you have not opened: read its outline to cite it, or cite a section ref you read";
const PAPER_UNREADABLE: &str = "is a paper, not a result or a passage you were shown";
const NOT_EVIDENCE: &str = "is not a result or a part you read";
const NOT_LITERATURE: &str = "is not a passage you were shown or a part you read";

/// What a lead check left out of a reply's lessons. `context_refs` holds, at most `NAMES_MAX`, the
/// refs left out that the call's context itself holds (a part a read part listed, a shown passage, its paper): ids
/// the coordinator built. A ref the reply wrote that the context does not hold may be any text, paper text included,
/// so it is only counted. `refs_total` counts both; `lessons` the observations dropped.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct LessonTrim {
    pub context_refs: Vec<String>,
    pub refs_total: usize,
    pub lessons: usize,
}

/// Leaves each lesson only the evidence refs in `evidence` and drops an observation that keeps none. A test run's lead,
/// told each ref at fault, fixed those and cited others it had not read on the next attempt, three times, so a lesson
/// that cites what the lead did not read now loses that ref instead of costing the turn; nothing it did not read is
/// recorded as evidence. `check_lead_reply` validates the reply as written before this runs, so the shape, lesson and
/// rejected-analysis refusals still see every lesson and ref.
pub(super) fn trim_lessons(
    lessons: &mut Vec<Lesson>,
    evidence: &HashSet<String>,
    unread: &UnreadRefs,
) -> LessonTrim {
    let mut trim = LessonTrim::default();
    for lesson in lessons.iter_mut() {
        lesson.evidence_refs.retain(|r| {
            let keep = evidence.contains(r);
            if !keep {
                trim.refs_total += 1;
                if unread.holds(r)
                    && trim.context_refs.len() < NAMES_MAX
                    && !trim.context_refs.contains(r)
                {
                    trim.context_refs.push(bounded_name(r.clone()));
                }
            }
            keep
        });
    }
    let before = lessons.len();
    lessons.retain(|l| l.kind != LessonKind::Observation || !l.evidence_refs.is_empty());
    trim.lessons = before - lessons.len();
    trim
}

/// The refs a lead context holds that a reply may not cite where it cited them, each with the fix the next call is told:
/// a part a read part lists (an outline's sections, figures and tables, a cut section's continuation), a shown passage
/// (which an action may cite but a lesson may not), and a shown passage's paper. The fix says to read only when the
/// context offered reading and the ref's paper is one the lead may read: a passage the offer gave a `paper_ref`, never a
/// commons passage. The names reach the next attempt, which builds its own reading with its own rounds, so this call's
/// rounds left do not matter.
#[derive(Debug, Default)]
pub struct UnreadRefs {
    why: HashMap<String, &'static str>,
}

impl UnreadRefs {
    pub fn of(context: &Value) -> Self {
        let reading = context["reading_offer"].is_object();
        let pick = |readable: &'static str, unreadable: &'static str| {
            if reading { readable } else { unreadable }
        };
        let parts = context["reading"]["parts"]
            .as_array()
            .map(Vec::as_slice)
            .unwrap_or_default();
        let read: HashSet<&str> = parts.iter().filter_map(|p| p["ref"].as_str()).collect();
        let mut why = HashMap::new();
        let mut note = |reference: Option<&str>, fix: &'static str| {
            if let Some(r) = reference.filter(|r| !read.contains(r)) {
                why.entry(r.to_owned()).or_insert(fix);
            }
        };
        for part in parts {
            for list in ["sections", "figures", "tables"] {
                for item in part[list].as_array().into_iter().flatten() {
                    note(item["ref"].as_str(), pick(LISTED, LISTED_UNREADABLE));
                }
            }
            note(
                part["study"]["ref"].as_str(),
                pick(LISTED, LISTED_UNREADABLE),
            );
            for fact in part["facts"].as_array().into_iter().flatten() {
                note(fact["section"].as_str(), pick(LISTED, LISTED_UNREADABLE));
            }
            note(part["next"].as_str(), pick(LISTED, LISTED_UNREADABLE));
        }
        for section in ["literature", "literature_claims"] {
            for passage in context[section].as_array().into_iter().flatten() {
                let readable = reading && passage["paper_ref"].is_string();
                note(
                    passage["ref"].as_str(),
                    if readable {
                        PASSAGE
                    } else {
                        PASSAGE_UNREADABLE
                    },
                );
                note(passage["paper_ref"].as_str(), pick(PAPER, PAPER_UNREADABLE));
            }
        }
        Self { why }
    }

    /// One name per ref the lessons from the `first`th (counted from zero) cite outside `evidence`, in order.
    pub(super) fn lesson_names(
        &self,
        lessons: &[Lesson],
        first: usize,
        evidence: &HashSet<String>,
    ) -> Vec<String> {
        lessons
            .iter()
            .enumerate()
            .skip(first)
            .flat_map(|(n, l)| {
                l.evidence_refs
                    .iter()
                    .filter(|r| !evidence.contains(*r))
                    .map(move |r| (n + 1, r))
            })
            .take(NAMES_MAX)
            .map(|(n, r)| name(&format!("lesson {n}"), r, self.fix(r, NOT_EVIDENCE)))
            .collect()
    }

    /// One name per fault of the actions from the `first`th on: too many refs, a repeated ref, and each ref outside
    /// `shown`, in order.
    pub(super) fn literature_names(
        &self,
        actions: &[ProposedAction],
        first: usize,
        shown: &HashSet<String>,
    ) -> Vec<String> {
        // Bounded as they are built: a reply naming thousands of refs makes twelve names, not thousands.
        let mut names = Vec::new();
        for a in actions.iter().skip(first) {
            let who = format!("action {}", cut(&a.action_key, KEY_CHARS, NAME_BYTES));
            if a.literature_refs.len() > LITERATURE_REFS_MAX {
                names.push(format!(
                    "{who}: more than {LITERATURE_REFS_MAX} literature_refs"
                ));
            }
            if a.literature_refs.iter().collect::<HashSet<_>>().len() != a.literature_refs.len() {
                names.push(format!("{who}: a literature ref repeats"));
            }
            for r in a.literature_refs.iter().filter(|r| !shown.contains(*r)) {
                if names.len() >= NAMES_MAX {
                    break;
                }
                names.push(name(&who, r, self.fix(r, NOT_LITERATURE)));
            }
            if names.len() >= NAMES_MAX {
                break;
            }
        }
        names.truncate(NAMES_MAX);
        names
    }

    fn fix(&self, reference: &str, otherwise: &'static str) -> &'static str {
        self.why.get(reference).copied().unwrap_or(otherwise)
    }

    /// Whether the context holds this ref unread: a listed part, a shown passage or its paper.
    fn holds(&self, reference: &str) -> bool {
        self.why.contains_key(reference)
    }
}

/// `who: ref fix`, with the reply-written ref cut so the whole fix survives both the prior-attempt note's
/// `PRIOR_NAME_CHARS` and `bounded_name`'s bytes.
fn name(who: &str, reference: &str, fix: &str) -> String {
    let fixed = format!("{who}:  {fix}");
    let chars = PRIOR_NAME_CHARS.saturating_sub(fixed.chars().count());
    let bytes = NAME_BYTES.saturating_sub(fixed.len());
    bounded_name(format!("{who}: {} {fix}", cut(reference, chars, bytes)))
}

/// The text within `chars` characters and `bytes` bytes, ending in `…` when cut.
fn cut(text: &str, chars: usize, bytes: usize) -> String {
    if text.chars().count() <= chars && text.len() <= bytes {
        return text.to_owned();
    }
    let mut out = String::new();
    for c in text.chars() {
        if out.chars().count() + 2 > chars || out.len() + c.len_utf8() + '…'.len_utf8() > bytes {
            break;
        }
        out.push(c);
    }
    out.push('…');
    out
}

#[cfg(test)]
#[path = "ref_faults_tests.rs"]
mod tests;
