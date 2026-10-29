//! Names holding a number (Fmo3, Cyp7a1, IL-6) may stand in prose when the source material of the call being checked
//! writes them, never when only the lead's or a model's own words or a result cell do; a number standing alone stays
//! refused (both runs paused after three summaries naming genes the papers and
//! data cards showed).
use super::*;

fn context(key: &str, value: Value) -> Value {
    let mut context = json!({"question": "Which loci matter?"});
    context[key] = value;
    context
}

fn named(key: &str, value: Value) -> ResultIndex {
    ResultIndex::default().with_names_from(&context(key, value))
}

fn passage(text: &str) -> Value {
    json!([{"ref": "library://p/c", "paper": "Paper", "text": text}])
}

#[test]
fn a_name_the_call_shows_may_stand_in_prose() {
    let index = named(
        "literature",
        passage("Fmo3, Cyp7a1 and IL-6 lie under the peak on chromosome 17."),
    );
    assert!(
        index
            .prose_refused("Fmo3 and Cyp7a1 lie under it, with IL-6.")
            .is_empty()
    );
    assert!(index.prose_refused("Fmo3's peak and (Cyp7a1).").is_empty());
    assert!(index.prose_refused("Fmo3\u{2019}s peak").is_empty());
    assert!(
        index
            .step_prose_refused("Pair Fmo3 with the trait")
            .is_empty()
    );
    assert!(index.prose_allowed("Fmo3 lies under it."));
    // The number standing alone in the passage stays refused, as before.
    assert_eq!(index.prose_refused("It lies on chromosome 17."), ["17"]);
    // A name matches only with the digits the shown text writes; its letters' case may differ (FMO3 for a shown Fmo3).
    assert_eq!(
        index.prose_refused("Fmo31 and fmo3 and FMO3 and Cyp7a2."),
        ["31", "7", "2"]
    );
    assert_eq!(index.prose_refused("IL-7 rose."), ["7"]);
    // An index as compiled holds no names: they come only from the context a reply is checked against.
    assert_eq!(ResultIndex::default().prose_refused("Fmo3 rose."), ["3"]);
}

#[test]
fn every_source_section_adds_names() {
    let text = "Slc22a1 holds the marker.";
    for key in [
        "question",
        "constraints",
        "scientist_instructions",
        "methods",
        "data_cards",
        "starting_point",
    ] {
        let index = named(key, json!([text]));
        assert!(index.prose_refused("Slc22a1 matters.").is_empty(), "{key}");
    }
    assert!(
        named("literature", passage(text))
            .prose_refused("Slc22a1 matters.")
            .is_empty()
    );
    for key in ["text", "caption", "abstract"] {
        let reading = json!({"parts": [{"ref": "library://p/s", key: text}]});
        assert!(
            named("reading", reading)
                .prose_refused("Slc22a1 matters.")
                .is_empty(),
            "{key}"
        );
    }
    // The methods check is sent the cited parts instead of the reading.
    for part in ["section", "figure", "table", "outline"] {
        let cited = json!([{"ref": "library://p/s", "part": part, "paper": "P", "text": text}]);
        assert!(
            named("reading_cited", cited)
                .prose_refused("Slc22a1 matters.")
                .is_empty(),
            "{part}"
        );
    }
}

#[test]
fn model_words_result_cells_and_action_keys_add_no_name() {
    let refused = |index: &ResultIndex| index.prose_refused("Slc22a1 matters.");
    for key in [
        "latest_answer",
        "lab_notes",
        "past_research",
        "earlier_findings",
        "earlier_readings",
        "prior_attempt",
        "orientation",
        "literature_claims",
        "tool_card",
        "analyses",
    ] {
        let value = json!([{"statement": "Slc22a1 holds the marker.", "newest": {"key": "Slc22a1"},
            "results": [{"tables": [{"columns": ["gene"], "cells": [{"gene": "Slc22a1"}]}]}]}]);
        assert_eq!(refused(&named(key, value)), ["22", "1"], "{key}");
    }
    // A passage's other fields and the reading section's other keys are not paper text.
    let titled = json!([{"ref": "library://p/c", "paper": "Slc22a1 review", "text": "No gene."}]);
    assert_eq!(refused(&named("literature", titled)), ["22", "1"]);
    let reading =
        json!({"refused": [{"ref": "Slc22a1"}], "parts": [{"ref": "Slc22a1", "text": "x"}]});
    assert_eq!(refused(&named("reading", reading)), ["22", "1"]);
    // A figure's description is a model's, and a study part's facts are the library's extraction, not paper text.
    let reading = json!({"parts": [{"ref": "library://p/figure/1", "part": "figure", "caption": "x",
        "description": "Slc22a1 holds the marker.", "description_source": "model"},
        {"ref": "library://p/study", "part": "study", "label": "Slc22a1",
            "facts": [{"field": "gene", "value": "Slc22a1"}]}]});
    assert_eq!(refused(&named("reading", reading)), ["22", "1"]);
    let cited = json!([{"ref": "library://p/study", "part": "study", "paper": "Slc22a1", "heading": "Slc22a1",
        "text": "{\"facts\":[{\"value\":\"Slc22a1\"}]}"}]);
    assert_eq!(refused(&named("reading_cited", cited)), ["22", "1"]);
}

#[test]
fn a_quantity_or_an_invisible_join_is_never_a_name() {
    // A token starting with a digit is a value with a unit or an exponent, even when a source writes it.
    let index = named(
        "literature",
        passage("Doses of 7.1mg and p below 1e-9 in 6J mice, A7 and B3 and A7\u{200e}B3."),
    );
    assert_eq!(index.prose_refused("It was 7.1mg."), ["7.1"]);
    // Written whole by the source, 1e-9 stands as written; composed from separate numbers it never does.
    assert!(index.prose_refused("p was 1e-9.").is_empty());
    assert_eq!(index.prose_refused("The 6J mice."), ["6"]);
    // Two shown names joined by an invisible character or a combining mark read as one name nothing showed.
    assert!(index.prose_refused("A7 and B3.").is_empty());
    for joined in [
        "A7\u{200e}B3",
        "A7\u{034f}B3",
        "A7\u{200b}B3",
        "A7\u{2060}B3",
    ] {
        // A direction mark is named as well (`directs_text`).
        let mut want = vec!["7", "3"];
        if joined.contains('\u{200e}') {
            want.push("U+200E");
        }
        assert_eq!(
            index.prose_refused(&format!("{joined} rose.")),
            want,
            "{joined:?}"
        );
    }
    // Two shown names joined by a slash or a colon read as one name nothing showed, even where each alone would stand
    // with a digit-free affix; a numeral outside ASCII is refused.
    assert_eq!(index.prose_refused("A7/B3 rose."), ["7", "3"]);
    assert_eq!(index.prose_refused("A7-B3 rose."), ["7", "3"]);
    assert!(
        index
            .prose_refused("A7-dependent and anti-B3 rose.")
            .is_empty()
    );
    assert_eq!(index.prose_refused("A7:B3 rose."), ["7", "3"]);
    assert_eq!(
        index.prose_refused("Fmo\u{FF13} and 4\u{0663} rose."),
        ["4", "\u{FF13}", "\u{0663}"]
    );
    // A letter from another script is not the ASCII name the source wrote.
    assert_eq!(index.prose_refused("\u{0410}7 rose."), ["7"]);
}

#[test]
fn a_name_is_read_whole_and_plain() {
    let around = |s: &str, number: &str| {
        let c: Vec<char> = s.chars().collect();
        let start = s.find(number).map(|b| s[..b].chars().count()).unwrap();
        operand_around(&c, start, start + number.chars().count())
    };
    assert_eq!(around("as IL-6 rose", "6"), "IL-6");
    assert_eq!(around("in C57BL/6J mice", "57"), "C57BL/6J");
    assert_eq!(around("in C57BL/6J mice", "6"), "C57BL/6J");
    assert_eq!(around("Fmo3: a gene", "3"), "Fmo3");
    assert_eq!(around("Cyp7a1,", "1"), "Cyp7a1");
    assert_eq!(around("(Fmo3).", "3"), "Fmo3");
    assert_eq!(around("A7\u{200e}B3 x", "7"), "A7\u{200e}B3");
    assert!(
        plain_name("Fmo3") && plain_name("IL-6") && plain_name("rs12345") && plain_name("v1.5")
    );
    assert!(
        !plain_name("6J")
            && !plain_name("7.1mg")
            && !plain_name("Fmo")
            && !plain_name("A7\u{200e}B3")
    );
    assert_eq!(
        shown_name_keys(
            "Fmo3 on chromosome 3, IL-6 and 7.1mg, Apoa1-knockout and Fmo3\u{2212}/\u{2212}"
        ),
        ["fmo3", "il-6", "apoa1-knockout", "apoa1", "fmo3"]
    );
}

#[test]
fn a_shown_name_stands_in_another_case_or_with_a_digit_free_affix() {
    // In a lab test run, summaries naming a shown gene as FMO3, Fmo3-dependent or Fmo3−/− were refused.
    let index = named(
        "data_cards",
        json!([{"title": "Liver expression", "text": "Fmo3 expression by strain."}]),
    );
    for prose in [
        "FMO3 rose.",
        "fmo3 rose.",
        "Fmo3-dependent TMAO rose.",
        "Fmo3\u{2212}/\u{2212} mice and Fmo3-/- mice.",
        "anti-Fmo3 and Fmo3_KO and Fmo3.",
        "Fmo3\u{2011}null mice.",
    ] {
        assert!(index.prose_refused(prose).is_empty(), "{prose:?}");
    }
    // Every digit stays one the sources wrote, in a name they wrote.
    for (prose, refused) in [
        ("Fmo31 rose.", vec!["31"]),
        ("Fmo3.5 rose.", vec!["3.5"]),
        ("Fmo3-1 rose.", vec!["3", "1"]),
        ("Fmo3/Fmo3 rose.", vec!["3", "3"]),
        ("Fmo3\u{200b}x rose.", vec!["3"]),
        ("\u{03b1}Fmo3 rose.", vec!["3"]),
    ] {
        assert_eq!(index.prose_refused(prose), refused, "{prose:?}");
    }
}

#[test]
fn numbers_allowed_on_their_own_never_admit_a_name_piece_by_piece() {
    let mut index = named(
        "data_cards",
        json!([{"title": "Liver", "text": "Fmo3 and Fmo3.5 by strain."}]),
    );
    for n in ["7", "3", "1", "3.5"] {
        index.allowed_numbers.insert(n.to_string());
    }
    index.setting_numbers.insert("3".to_string());
    // Shown whole, a name stands; its pieces never compose another.
    assert!(index.prose_refused("Fmo3 and Fmo3.5 rose.").is_empty());
    assert_eq!(index.prose_refused("A7/B3 rose."), ["7", "3"]);
    assert_eq!(index.prose_refused("Gene3 rose."), ["3"]);
    assert_eq!(index.prose_refused("Fmo3-1 rose."), ["3", "1"]);
    assert_eq!(
        index.step_prose_refused("Pair Gene3 with A7\u{200e}B3"),
        ["3", "7", "3", "U+200E"]
    );
    index.allowed_numbers.insert("9".to_string());
    assert_eq!(index.prose_refused("p was 1e-9."), ["1", "9"]);
    assert_eq!(index.prose_refused("\u{200B}Gene3 rose."), ["3"]);
    // Expressions keep the numeric rule.
    assert!(
        index
            .prose_refused("With n=7 and p<3.5 it rose.")
            .is_empty()
    );
    // Fmo3.5 shows only itself: its 3 does not stand for Fmo3 when only the longer name was written.
    let decimal = named(
        "data_cards",
        json!([{"title": "Liver", "text": "Fmo3.5 by strain."}]),
    );
    assert_eq!(decimal.prose_refused("Fmo3 rose."), ["3"]);
    // Masking leaves a number inside a name for the rule: Fmo3-1 is not masked into Fmo3-(a number).
    let (masked, count) = index.mask_step_numbers("Run Fmo3-1 at 12 and 3", None);
    assert_eq!(
        (masked.as_str(), count),
        ("Run Fmo3-1 at (a number) and 3", 1)
    );
}

#[test]
fn comparisons_and_hidden_characters_never_compose_a_value() {
    let mut index = named(
        "data_cards",
        json!([{"title": "Liver", "text": "Fmo3 by strain."}]),
    );
    for n in ["1", "3", "5", "7", "9", "37"] {
        index.allowed_numbers.insert(n.to_string());
    }
    // A comparison or a percent sign next to an operand does not make it an expression.
    for text in [
        "p=1e-9 here.",
        "p<1e-9 here.",
        "It was 1e-9%.",
        "It was 1e-9= here.",
    ] {
        assert_eq!(index.prose_refused(text), ["1", "9"], "{text}");
    }
    for text in ["Gene3% rose.", "The <Gene3> rose."] {
        assert_eq!(index.prose_refused(text), ["3"], "{text}");
    }
    assert_eq!(index.prose_refused("A7/B3= rose."), ["7", "3"]);
    // A hidden or combining character between digits reads as one number nothing showed; masked, no digit remains.
    for joined in [
        "1\u{200B}9",
        "1\u{200D}9",
        "1\u{2060}9",
        "1\u{0301}9",
        "1\u{FE0F}9",
    ] {
        let text = format!("It was {joined} here.");
        assert_eq!(index.prose_refused(&text), ["1", "9"], "{text:?}");
        let (step, count) = index.mask_step_numbers(&text, None);
        assert_eq!(count, 2, "{text:?}");
        assert!(index.step_prose_refused(&step).is_empty(), "{step:?}");
        let (summary, count) = mask_refused_numbers(&text, &index);
        assert_eq!(count, 2, "{text:?}");
        assert!(index.prose_refused(&summary).is_empty(), "{summary:?}");
    }
    // A hidden character beside one allowed number still changes how it reads: a right-to-left override shows 37 as 73.
    // Masking writes the number as a mask and drops each direction control, counting both.
    for (text, want) in [
        ("It was \u{202E}37 here.", vec!["37", "U+202E"]),
        (
            "It was \u{2067}37\u{2069} here.",
            vec!["37", "U+2067", "U+2069"],
        ),
        ("It was 37\u{FE0F} here.", vec!["37"]),
    ] {
        assert_eq!(index.prose_refused(text), want, "{text:?}");
        let (step, count) = index.mask_step_numbers(text, None);
        assert_eq!(count, want.len(), "{text:?}");
        assert!(index.step_prose_refused(&step).is_empty(), "{step:?}");
        let (summary, count) = mask_refused_numbers(text, &index);
        assert_eq!(count, want.len(), "{text:?}");
        assert!(index.prose_refused(&summary).is_empty(), "{summary:?}");
    }
    // A filler that counts as a letter joins the digits into one operand starting with a digit: both numbers mask.
    assert_eq!(index.prose_refused("It was 1\u{3164}9 here."), ["1", "9"]);
    let (step, count) = index.mask_step_numbers("It was 1\u{3164}9 here.", None);
    assert_eq!(count, 2);
    assert!(index.step_prose_refused(&step).is_empty(), "{step:?}");
    // Comparisons, quantities with units, and a value a source writes whole keep standing.
    assert!(
        index
            .prose_refused("With n=7, p<9 and 5% at 37\u{00B0}C, 5\u{00B5}g and 5mg/kg.")
            .is_empty()
    );
    let written = named("literature", passage("Below p<1e-9 in liver."));
    assert!(written.prose_refused("At p=1e-9 it held.").is_empty());
    // Masking never splits a name or an operand: Fmo3-1 stays whole for the rule, and n=12 masks cleanly.
    let (summary, count) = mask_refused_numbers("Fmo3-1 rose at n=12.", &index);
    assert_eq!(
        (summary.as_str(), count),
        ("Fmo3-1 rose at n=(a number).", 1)
    );
    assert_eq!(index.prose_refused(&summary), ["3", "1"]);
    let (step, count) = index.mask_step_numbers("Fmo3-1 at n=12 and 37\u{00B0}C", None);
    assert_eq!(
        (step.as_str(), count),
        ("Fmo3-1 at n=(a number) and 37\u{00B0}C", 1)
    );
}

#[test]
fn signs_radixes_groupings_and_number_pairs_never_compose_a_value() {
    let mut index = named(
        "literature",
        passage("Below 1e-9 in liver, days 10-20, and Fmo3 by strain."),
    );
    for n in ["0", "1", "5", "9", "10", "20", "234"] {
        index.allowed_numbers.insert(n.to_string());
    }
    // A shown 1e-9 admits itself, never a value a sign or a point makes of it.
    assert!(index.prose_refused("Below 1e-9 and -Fmo3 rose.").is_empty());
    assert_eq!(index.prose_refused("It was -1e-9."), ["1", "9"]);
    assert_eq!(index.prose_refused("It was .1e-9."), ["1", "9"]);
    assert_eq!(index.prose_refused("It was .5 here."), ["5"]);
    // A radix or a multiplier writes another value than its number: refused unless shown whole, and masked cleanly.
    assert_eq!(index.prose_refused("Set 0xFF here."), ["0"]);
    assert_eq!(
        index.prose_refused("Set 0\u{FF58}\u{FF26}\u{FF26} here."),
        ["0"]
    );
    for text in [
        "Set 0FFh here.",
        "About 5k mice.",
        "About 5M reads.",
        "Nearly 9e-1.",
    ] {
        let refused = index.prose_refused(text);
        assert!(!refused.is_empty(), "{text}");
        let (step, count) = index.mask_step_numbers(text, None);
        assert_eq!(count, refused.len(), "{text}");
        assert!(index.step_prose_refused(&step).is_empty(), "{step:?}");
        let (summary, count) = mask_refused_numbers(text, &index);
        assert_eq!(count, refused.len(), "{text}");
        assert!(index.prose_refused(&summary).is_empty(), "{summary:?}");
    }
    // A measurement with a listed unit keeps the per-number rule.
    assert!(
        index
            .prose_refused("At 5mg/kg, 10min, 20wk, 9kb, 5\u{03BC}M, 1st and 10x.")
            .is_empty()
    );
    // Two numbers in one operand stand only as a source writes them.
    assert!(index.prose_refused("Over days 10-20 it rose.").is_empty());
    for text in [
        "p=10^-9 here.",
        "It was 10/20.",
        "It was 10*9.",
        "Days 20-10.",
    ] {
        assert_eq!(index.prose_refused(text).len(), 2, "{text}");
        let (step, count) = index.mask_step_numbers(text, None);
        assert_eq!(count, 2, "{text}");
        assert!(index.step_prose_refused(&step).is_empty(), "{step:?}");
    }
    // A space outside ASCII between digits groups them into one number; between a number and a unit it separates.
    for space in ['\u{202F}', '\u{2009}', '\u{00A0}', '\u{2007}'] {
        let text = format!("It was 1{space}234 here.");
        assert_eq!(index.prose_refused(&text), ["1", "234"], "{text:?}");
        let (summary, count) = mask_refused_numbers(&text, &index);
        assert_eq!(count, 2, "{text:?}");
        assert!(index.prose_refused(&summary).is_empty(), "{summary:?}");
    }
    assert!(index.prose_refused("It was 5\u{00A0}mg and 5.").is_empty());
    // A run of grouping characters between digits groups them however long it is, and an apostrophe groups as well.
    for joined in [
        "1\u{202F}\u{202F}234",
        "1\u{200A}\u{200A}234",
        "1 \u{2009}234",
        "1'234",
        "1\u{2019}234",
        "1' 234",
    ] {
        let text = format!("It was {joined} here.");
        assert_eq!(index.prose_refused(&text), ["1", "234"], "{text:?}");
        let (step, count) = index.mask_step_numbers(&text, None);
        assert_eq!(count, 2, "{text:?}");
        assert!(index.step_prose_refused(&step).is_empty(), "{step:?}");
        let (summary, count) = mask_refused_numbers(&text, &index);
        assert_eq!(count, 2, "{text:?}");
        assert!(index.prose_refused(&summary).is_empty(), "{summary:?}");
    }
    // Any run of spaces between digits groups them too; a comma, a word or a quoted number before a word separates.
    for text in [
        "Days 1 234 here.",
        "Days 1\n234 here.",
        "Days 1 \u{200B} 234 here.",
        "Days 1 \u{200D} 234 here.",
        "Days 1 \u{034F} 234 here.",
        "Days 1 \u{FE0F} 234 here.",
    ] {
        assert_eq!(index.prose_refused(text), ["1", "234"], "{text:?}");
        let (summary, count) = mask_refused_numbers(text, &index);
        assert_eq!(count, 2, "{text:?}");
        assert!(index.prose_refused(&summary).is_empty(), "{summary:?}");
    }
    assert!(
        index
            .prose_refused("Days 1, 234, 1 or 234 and '5' days.")
            .is_empty()
    );
    // A direction control is refused as itself, with no digit near it: a finding's bound value would be inserted
    // later where a placeholder stood.
    assert_eq!(
        index.prose_refused("Mean \u{202E} \u{202C} rose."),
        ["U+202E", "U+202C"]
    );
    assert_eq!(
        index.step_prose_refused("Rank \u{2067}them\u{2069}."),
        ["U+2067", "U+2069"]
    );
    // A direction control anywhere in the text refuses every number in it, whatever spaces separate them: an override
    // displays an allowed 37 as 73 across a space.
    for text in [
        "Value: \u{202E} 37 \u{202C} here.",
        "Value: \u{2067} 37 \u{2069} here.",
        "About 5 rose, then \u{202E} 9 \u{202C}.",
        "It was 37 here.\u{200F}",
    ] {
        let refused = index.prose_refused(text);
        assert!(
            refused.iter().any(|n| n.starts_with("U+"))
                && refused
                    .iter()
                    .any(|n| !n.starts_with("U+") && text.contains(n.as_str())),
            "{text:?}"
        );
        let (step, count) = index.mask_step_numbers(text, None);
        assert_eq!(count, refused.len(), "{text:?}");
        assert!(index.step_prose_refused(&step).is_empty(), "{step:?}");
        let (summary, count) = mask_refused_numbers(text, &index);
        assert_eq!(count, refused.len(), "{text:?}");
        assert!(index.prose_refused(&summary).is_empty(), "{summary:?}");
    }
}
