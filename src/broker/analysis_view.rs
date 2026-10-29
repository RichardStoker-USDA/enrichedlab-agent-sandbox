//! What the research lead reads of the template-analysis catalog.
//!
//! An analysis the lead may run shows its whole input contract: its id, label, the first sentence of its description,
//! and each setting's name, type, required state, limits, default and what the setting holds (`about`). Contracts are
//! offered in the order the run needs them (`analysis_order`): an analysis the lead named in its previous, refused reply
//! first, then by how strongly the question names it, then the constraints and the plan's step titles, then the
//! catalog's own order. Contracts fill their share of the room in that order. Every other analysis is listed by id and
//! label under `other_analyses`, so the lead knows it exists; only those that do not fit even there are counted in
//! `analyses_omitted`. A step runs only an analysis shown with its contract: naming one listed by id alone is refused
//! with `proposal_analysis_contract_not_shown`, and the next call shows that contract first.
//!
//! What the lead needs to read these fields travels with them (`SETTINGS_NOTE`, `OTHER_ANALYSES_NOTE`), never in the
//! lead's system text: that text is part of every run's call, and 269 bytes added there were refused by the autonomy
//! suite, whose runs list no template analysis.
use super::{figure_run, source_card};
use serde_json::{Value, json};
use std::collections::{HashMap, HashSet};

/// The catalog's share of a lead context at most, unless the operator's `analyses_bytes` section sets another; a smaller
/// context gives it a quarter of its budget (`analyses_room`).
pub const ANALYSES_VIEW_BYTES: usize = 8192;
/// The refusal for a step that names an analysis the call listed without its contract.
pub const CONTRACT_NOT_SHOWN: &str = "proposal_analysis_contract_not_shown";
/// An analysis's first sentence and its label as a contract shows them, at most these many characters.
const ABOUT_CHARS: usize = 200;
const LABEL_CHARS: usize = 120;
/// A setting's description, at most these many characters.
const SETTING_ABOUT_CHARS: usize = 160;
/// A label in `other_analyses`, at most these many characters.
const LISTED_LABEL_CHARS: usize = 80;
/// The listing's share of the room at most, so contracts keep two thirds of it.
const LISTING_SHARE_DIVISOR: usize = 3;
/// The sentence that tells the lead how `other_analyses` works; it counts against the room.
pub const OTHER_ANALYSES_NOTE: &str = "other_analyses lists more analyses by id and label only. A step can run one only after a call shows its inputs: name it as a step's feature and the next call shows them.";
/// The sentence that tells the lead how to read a setting's `default` and `about`, shown with the contracts as the method's
/// `analyses_note`; it and its key count against the room.
pub const SETTINGS_NOTE: &str = "In an analysis's inputs.settings, default is the value used when the setting is left out, and about says what the setting holds, such as its unit or the measure it names.";
const SETTINGS_NOTE_KEY: &str = r#","analyses_note":"#;
/// Words that say how to ask, not what to analyze. Unlike the query sources' list, `food`, `nutrient` and `energy` stay:
/// they tell one analysis from another.
const QUESTION_WORDS: &[&str] = &[
    "a", "an", "and", "are", "as", "at", "be", "by", "can", "do", "does", "for", "from", "give",
    "how", "i", "in", "into", "is", "it", "its", "list", "many", "much", "of", "on", "or", "our",
    "please", "show", "tell", "than", "that", "the", "their", "them", "then", "there", "these",
    "they", "this", "those", "to", "use", "using", "was", "we", "were", "what", "when", "where",
    "which", "who", "why", "with", "would", "you",
];

/// The run's text the view ranks by, and the analyses the lead named in its previous refused reply.
pub struct Ranking<'a> {
    pub question: &'a str,
    pub around: &'a str,
    pub focus: &'a [String],
}

/// The catalog as one call shows it.
#[derive(Debug, Default, PartialEq)]
pub struct AnalysesView {
    /// Analyses with their whole contract, in the order the lead should consider them.
    pub analyses: Vec<Value>,
    /// Analyses listed by id and label only.
    pub other: Vec<Value>,
    /// Analyses neither shown nor listed for room.
    pub omitted: usize,
}

/// The catalog's room in a lead context of `budget` bytes: a quarter of it, at most `limit`, so the catalog never takes
/// the room the rest of a small context needs.
pub fn analyses_room(budget: usize, limit: usize) -> usize {
    limit.min(budget / 4)
}

pub fn analyses_view(catalog: &Value, room: usize, ranking: &Ranking) -> AnalysesView {
    let size = |v: &Value| serde_json::to_vec(v).map_or(usize::MAX, |b| b.len());
    let features: Vec<&Value> = catalog["features"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|f| f["id"].as_str().is_some())
        .collect();
    // The settings note goes with any contract, so its bytes come out of the room first.
    let room = if features.is_empty() {
        room
    } else {
        room.saturating_sub(size(&json!(SETTINGS_NOTE)) + SETTINGS_NOTE_KEY.len())
    };
    let order = analysis_order(&features, ranking);
    let contracts: Vec<Value> = order.iter().map(|&i| contract(features[i])).collect();
    let listings: Vec<Value> = order.iter().map(|&i| listing(features[i])).collect();
    // Bytes of each array as written: brackets, items and the commas between them.
    let array_bytes =
        |items: &[Value]| 2 + items.iter().map(size).sum::<usize>() + items.len().saturating_sub(1);
    let note = size(&json!(OTHER_ANALYSES_NOTE));
    let listing_cap = room / LISTING_SHARE_DIVISOR;
    // Contracts first, keeping room for the listing of everything after them, up to its share.
    let mut shown = 0;
    while shown < contracts.len() {
        let next = shown + 1;
        let rest = if next < listings.len() {
            (array_bytes(&listings[next..]) + note).min(listing_cap)
        } else {
            0
        };
        if array_bytes(&contracts[..next]) + rest > room {
            break;
        }
        shown = next;
    }
    let mut view = AnalysesView {
        analyses: contracts[..shown].to_vec(),
        ..AnalysesView::default()
    };
    if shown < listings.len() {
        let mut used = array_bytes(&view.analyses) + note + 2;
        for item in &listings[shown..] {
            let n = size(item) + usize::from(!view.other.is_empty());
            if used + n > room {
                break;
            }
            used += n;
            view.other.push(item.clone());
        }
        view.omitted = listings.len() - shown - view.other.len();
    }
    view
}

/// An analysis with its whole input contract. The source and table fields name what a step supplies; every setting is
/// listed (`settings_view`).
fn contract(feature: &Value) -> Value {
    json!({
        "id": feature["id"],
        "label": figure_run::cut(feature["label"].as_str().unwrap_or_default(), LABEL_CHARS),
        "about": first_sentence(feature["description"].as_str().unwrap_or_default()),
        "inputs": {
            "source": {"type": "finished_step_key", "required": true},
            "table": {"type": "exact_table_path", "required": true},
            "settings": settings_view(&feature["params"]),
        },
    })
}

/// An analysis by id and label only.
fn listing(feature: &Value) -> Value {
    json!({
        "id": feature["id"],
        "label": figure_run::cut(feature["label"].as_str().unwrap_or_default(), LISTED_LABEL_CHARS),
    })
}

/// A feature's settings as a stable list, required ones first and then by name. Choices, limits and a default stay
/// visible as structured fields, and `about` carries what the setting holds, such as its unit or the measure it names.
/// A `column_names` marker distinguishes settings whose values must name input columns.
pub fn settings_view(params: &Value) -> Value {
    let mut sorted: Vec<(&String, &Value)> = params.as_object().into_iter().flatten().collect();
    sorted.sort_by_key(|(name, p)| (p["required"] != true, *name));
    let mut settings = Vec::new();
    for (name, p) in sorted {
        let kind = p["type"].as_str().unwrap_or_default();
        let mut setting = json!({"name": name, "type": kind, "required": p["required"] == true});
        if matches!(kind, "column" | "columns") {
            setting["column_names"] = json!(true);
        }
        for field in ["choices", "min", "max", "maxlen", "default"] {
            if let Some(value) = p.get(field) {
                setting[field] = value.clone();
            }
        }
        if let Some(about) = p["description"]
            .as_str()
            .map(str::trim)
            .filter(|d| !d.is_empty())
        {
            setting["about"] = json!(figure_run::cut(about, SETTING_ABOUT_CHARS));
        }
        settings.push(setting);
    }
    Value::Array(settings)
}

/// The order in which the view offers analyses' contracts: those the lead named in its previous refused reply, in its
/// order; then by how strongly the question names them, then the constraints and plan titles; then the catalog's order.
/// A word of the question counts when it has two characters or more, is not only digits and is not a question word, and
/// weighs 3 in an analysis's id or label, else 2 in a setting's name, else 1 in its description or a setting's, times
/// the number of analyses over the number that hold the word anywhere, so a word few analyses hold outranks one most of
/// them hold (the weights `methods::relation_order` uses for a source's relations).
pub fn analysis_order(features: &[&Value], ranking: &Ranking) -> Vec<usize> {
    let words_of =
        |text: &str| -> HashSet<String> { source_card::words(text).into_iter().collect() };
    let text = |v: &Value| v.as_str().unwrap_or_default().to_owned();
    let fields: Vec<[HashSet<String>; 3]> = features
        .iter()
        .map(|f| {
            let params: Vec<(&String, &Value)> =
                f["params"].as_object().into_iter().flatten().collect();
            let names = params
                .iter()
                .map(|(n, _)| n.as_str())
                .collect::<Vec<_>>()
                .join(" ");
            let descriptions = std::iter::once(text(&f["description"]))
                .chain(params.iter().map(|(_, p)| text(&p["description"])))
                .collect::<Vec<_>>()
                .join(" ");
            [
                words_of(&format!("{} {}", text(&f["id"]), text(&f["label"]))),
                words_of(&names),
                words_of(&descriptions),
            ]
        })
        .collect();
    let mut holders: HashMap<&String, u64> = HashMap::new();
    for f in &fields {
        for w in f.iter().flatten().collect::<HashSet<_>>() {
            *holders.entry(w).or_default() += 1;
        }
    }
    // Folded as the question's words are (`this` reads `thi`), so a question word never counts as a term.
    let question_words: HashSet<String> = QUESTION_WORDS
        .iter()
        .flat_map(|w| source_card::words(w))
        .collect();
    let terms = |text: &str| -> HashSet<String> {
        words_of(text)
            .into_iter()
            .filter(|w| {
                w.chars().count() >= 2
                    && !w.chars().all(|c| c.is_ascii_digit())
                    && !question_words.contains(w)
            })
            .collect()
    };
    let asked = terms(ranking.question);
    let around: HashSet<String> = terms(ranking.around).difference(&asked).cloned().collect();
    let n = features.len() as u64;
    let score = |terms: &HashSet<String>, f: &[HashSet<String>; 3]| -> u64 {
        terms
            .iter()
            .filter_map(|w| {
                let weight = [3_u64, 2, 1]
                    .into_iter()
                    .zip(f)
                    .find(|(_, words)| words.contains(w))?
                    .0;
                Some(weight.saturating_mul(n.saturating_mul(64) / holders.get(w).copied()?.max(1)))
            })
            .fold(0_u64, u64::saturating_add)
    };
    let focus: Vec<Option<usize>> = features
        .iter()
        .map(|f| ranking.focus.iter().position(|id| f["id"] == id.as_str()))
        .collect();
    let asked_scores: Vec<u64> = fields.iter().map(|f| score(&asked, f)).collect();
    let around_scores: Vec<u64> = fields.iter().map(|f| score(&around, f)).collect();
    let mut order: Vec<usize> = (0..features.len()).collect();
    order.sort_by_key(|i| {
        (
            focus[*i].is_none(),
            focus[*i].unwrap_or(0),
            std::cmp::Reverse(asked_scores[*i]),
            std::cmp::Reverse(around_scores[*i]),
            *i,
        )
    });
    order
}

fn first_sentence(text: &str) -> String {
    let end = text
        .char_indices()
        .find(|(i, c)| {
            matches!(c, '.' | '!' | '?')
                && text[i + c.len_utf8()..].starts_with(char::is_whitespace)
        })
        .map_or(text.len(), |(i, c)| i + c.len_utf8());
    let sentence = text[..end]
        .split_whitespace()
        .filter(|word| !result_path(word))
        .collect::<Vec<_>>()
        .join(" ");
    figure_run::cut(sentence.trim(), ABOUT_CHARS)
}

/// Whether a whitespace-delimited token is a retained output path. Trim only punctuation wrapped around the token;
/// removing the token never rewrites the catalog's prose or invents a replacement for it.
fn result_path(word: &str) -> bool {
    let path = word.trim_matches(|c: char| {
        matches!(
            c,
            '`' | '\''
                | '"'
                | '('
                | ')'
                | '['
                | ']'
                | '{'
                | '}'
                | '<'
                | '>'
                | ','
                | ';'
                | ':'
                | '!'
                | '?'
                | '.'
        )
    });
    path.starts_with("results/")
        && path
            .rsplit_once('/')
            .is_some_and(|(_, name)| name.contains('.'))
}

/// The analyses the lead named in a reply refused for `CONTRACT_NOT_SHOWN`: the refusal's names after the step's key.
pub fn focus_of(reason: &str, names: &[String]) -> Vec<String> {
    if reason != CONTRACT_NOT_SHOWN {
        return Vec::new();
    }
    names.iter().skip(1).take(4).cloned().collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn feature(id: &str, label: &str, description: &str, params: Value) -> Value {
        json!({"id": id, "label": label, "description": description, "params": params})
    }

    fn catalog() -> Value {
        json!({"features": [
            feature("summary_stats", "Summary statistics", "Means and spreads of numeric columns.",
                json!({"values": {"type": "columns", "required": true, "description": "numeric columns to summarize"}})),
            feature("animal_window_summary", "Per-animal summary over a time window", "One row per animal from repeated rows.",
                json!({"id": {"type": "column", "required": true}, "values": {"type": "columns", "required": true}})),
            feature("fdc_nutrient_normalize", "FoodData Central nutrient and energy normalization",
                "Scale nutrient amounts from per 100 g to a portion and compute energy from protein, fat and carbohydrate. More.",
                json!({"protein": {"type": "column", "required": true, "description": "protein in g per 100 g (FDC nutrient 1003)"},
                       "portion_grams": {"type": "column", "required": true, "description": "gram weight of one portion"},
                       "tolerance_percent": {"type": "number", "required": false, "min": 0, "max": 100, "default": 5,
                                             "description": "flag a row when computed energy differs from reported energy by more than this percent"}})),
        ]})
    }

    fn ranking<'a>(question: &'a str, focus: &'a [String]) -> Ranking<'a> {
        Ranking {
            question,
            around: "",
            focus,
        }
    }

    #[test]
    fn a_contract_carries_what_each_setting_holds_and_its_default() {
        let view = analyses_view(
            &catalog(),
            ANALYSES_VIEW_BYTES,
            &ranking("energy per portion", &[]),
        );
        assert_eq!(view.analyses[0]["id"], "fdc_nutrient_normalize");
        assert_eq!(
            view.analyses[0]["inputs"]["settings"],
            json!([
                {"name": "portion_grams", "type": "column", "required": true, "column_names": true,
                 "about": "gram weight of one portion"},
                {"name": "protein", "type": "column", "required": true, "column_names": true,
                 "about": "protein in g per 100 g (FDC nutrient 1003)"},
                {"name": "tolerance_percent", "type": "number", "required": false, "min": 0, "max": 100, "default": 5,
                 "about": "flag a row when computed energy differs from reported energy by more than this percent"},
            ])
        );
        assert_eq!(
            view.analyses[0]["about"],
            "Scale nutrient amounts from per 100 g to a portion and compute energy from protein, fat and carbohydrate."
        );
        // Every analysis fits here, so none is listed or omitted.
        assert_eq!(
            (view.analyses.len(), view.other.len(), view.omitted),
            (3, 0, 0)
        );
    }

    #[test]
    fn the_question_ranks_contracts_and_a_named_analysis_comes_first() {
        let catalog = catalog();
        let features: Vec<&Value> = catalog["features"].as_array().unwrap().iter().collect();
        let ids = |order: Vec<usize>| {
            order
                .into_iter()
                .map(|i| features[i]["id"].as_str().unwrap().to_owned())
                .collect::<Vec<_>>()
        };
        assert_eq!(
            ids(analysis_order(
                &features,
                &ranking("Energy from macronutrients per portion of each food", &[])
            ))[0],
            "fdc_nutrient_normalize"
        );
        assert_eq!(
            ids(analysis_order(
                &features,
                &ranking("one row per animal over the dark window", &[])
            ))[0],
            "animal_window_summary"
        );
        // A question that names nothing keeps the catalog's order.
        assert_eq!(
            ids(analysis_order(&features, &ranking("what is this", &[]))),
            [
                "summary_stats",
                "animal_window_summary",
                "fdc_nutrient_normalize"
            ]
        );
        // The analysis a refused reply named comes before anything the question ranks.
        let focus = ["summary_stats".to_owned()];
        assert_eq!(
            ids(analysis_order(
                &features,
                &ranking("energy per portion", &focus)
            ))[0],
            "summary_stats"
        );
        // Plan titles and constraints rank after the question.
        let around = Ranking {
            question: "what is this",
            around: "normalize energy per portion",
            focus: &[],
        };
        assert_eq!(
            ids(analysis_order(&features, &around))[0],
            "fdc_nutrient_normalize"
        );
    }

    #[test]
    fn a_large_catalog_keeps_its_room_and_lists_the_rest_by_id() {
        let many: Vec<Value> = (0..200)
            .map(|n| feature(&format!("analysis_{n:03}"), &"x".repeat(100), &"y".repeat(400),
                json!({"v": {"type": "column", "required": true, "description": "z".repeat(300)}})))
            .collect();
        let view = analyses_view(
            &json!({"features": many}),
            ANALYSES_VIEW_BYTES,
            &ranking("", &[]),
        );
        let bytes = |items: &[Value]| serde_json::to_vec(&json!(items)).unwrap().len();
        assert!(
            bytes(&view.analyses) + bytes(&view.other) + OTHER_ANALYSES_NOTE.len() + 2
                <= ANALYSES_VIEW_BYTES
        );
        assert!(!view.analyses.is_empty() && !view.other.is_empty() && view.omitted > 0);
        assert_eq!(view.analyses.len() + view.other.len() + view.omitted, 200);
        // Listed analyses carry their id and a short label only.
        assert_eq!(view.other[0].as_object().unwrap().len(), 2);
        assert!(view.other[0]["label"].as_str().unwrap().chars().count() <= LISTED_LABEL_CHARS);
        // Contracts keep at least two thirds of the room.
        assert!(bytes(&view.analyses) >= ANALYSES_VIEW_BYTES * 2 / 3 - 1024);
        // The settings note and its key come out of the same room.
        let note =
            serde_json::to_vec(&json!(SETTINGS_NOTE)).unwrap().len() + SETTINGS_NOTE_KEY.len();
        assert!(
            bytes(&view.analyses) + bytes(&view.other) + OTHER_ANALYSES_NOTE.len() + 2 + note
                <= ANALYSES_VIEW_BYTES
        );
        // A small context gives the catalog a quarter of its budget.
        assert_eq!(analyses_room(4096, ANALYSES_VIEW_BYTES), 1024);
        assert_eq!(
            analyses_room(65_536, ANALYSES_VIEW_BYTES),
            ANALYSES_VIEW_BYTES
        );
        assert_eq!(analyses_room(65_536, 12_288), 12_288);
        let small = analyses_view(&json!({"features": many}), 1024, &ranking("", &[]));
        assert!(bytes(&small.analyses) + bytes(&small.other) <= 1024);
        assert_eq!(
            small.analyses.len() + small.other.len() + small.omitted,
            200
        );
    }

    #[test]
    fn a_contract_not_shown_refusal_names_the_next_calls_focus() {
        let names = ["step-a".to_owned(), "fdc_nutrient_normalize".to_owned()];
        assert_eq!(
            focus_of(CONTRACT_NOT_SHOWN, &names),
            ["fdc_nutrient_normalize"]
        );
        assert!(focus_of("proposal_analysis_setting_unknown", &names).is_empty());
    }

    #[test]
    fn the_about_drops_output_paths() {
        assert_eq!(
            first_sentence(
                "Compares a value between two groups and writes results/results.csv. Welch or Wilcoxon."
            ),
            "Compares a value between two groups and writes"
        );
    }
}
