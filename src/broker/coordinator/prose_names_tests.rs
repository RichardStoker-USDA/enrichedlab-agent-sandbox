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
    // A name matches only as the shown text writes it.
    assert_eq!(
        index.prose_refused("Fmo31 and fmo3 and Cyp7a2."),
        ["31", "3", "7", "2"]
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
    assert_eq!(index.prose_refused("p was 1e-9."), ["1", "9"]);
    assert_eq!(index.prose_refused("The 6J mice."), ["6"]);
    // Two shown names joined by an invisible character or a combining mark read as one name nothing showed.
    assert!(index.prose_refused("A7 and B3.").is_empty());
    for joined in [
        "A7\u{200e}B3",
        "A7\u{034f}B3",
        "A7\u{200b}B3",
        "A7\u{2060}B3",
    ] {
        assert_eq!(
            index.prose_refused(&format!("{joined} rose.")),
            ["7", "3"],
            "{joined:?}"
        );
    }
    // Two shown names joined by a slash or a colon read as one name nothing showed; a numeral outside ASCII is refused.
    assert_eq!(index.prose_refused("A7/B3 rose."), ["7", "3"]);
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
        name_around(&c, start, start + number.chars().count())
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
        names_with_numbers("Fmo3 on chromosome 3, IL-6 and 7.1mg"),
        ["Fmo3", "IL-6"]
    );
}
