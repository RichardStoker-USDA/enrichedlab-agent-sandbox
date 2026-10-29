//! The slide estimate: no character is taken narrower than Calibri draws it, text wraps where PowerPoint wraps it or
//! sooner, a longer text never takes less room, and each box refuses what it cannot show whole.
use serde_json::{Value, json};

use super::*;

/// The widest glyph Calibri's four faces draw for each kind, in ems, measured with fontTools from the fonts Office
/// ships (`Microsoft PowerPoint.app/Contents/Resources/DFonts/Calibri*.ttf`).
const CALIBRI_WIDEST: [(char, f64); 17] = [
    (' ', 0.226),
    ('W', 0.906),
    ('Q', 0.686),
    ('o', 0.538),
    ('\u{a9}', 0.834),
    ('\u{2a5}', 0.919),
    ('\u{3e2}', 0.883),
    ('\u{1e88}', 0.907),
    ('\u{221e}', 0.853),
    ('\u{1f1}', 1.108),
    ('\u{2030}', 1.062),
    ('\u{20af}', 1.116),
    ('\u{2116}', 1.046),
    ('\u{478}', 1.099),
    ('\u{52a}', 1.279),
    ('\u{2194}', 1.310),
    ('\u{2460}', 1.328),
];

/// The longest start of `text` that `fits` takes.
fn fit_to(text: &str, fits: fn(&str) -> bool) -> String {
    let chars: Vec<char> = text.chars().collect();
    (0..=chars.len())
        .rev()
        .map(|n| chars[..n].iter().collect::<String>())
        .find(|start| fits(start))
        .unwrap_or_default()
}

#[test]
fn no_character_is_taken_narrower_than_calibri_draws_it() {
    for (c, widest) in CALIBRI_WIDEST {
        assert!(width_em(c) >= widest, "{c:?}");
    }
    for c in (0x21u8..0x7f).map(char::from) {
        let widest = match c {
            'M' | 'W' | 'm' | 'w' | '@' | '%' | '&' => 0.906,
            'A'..='Z' => 0.686,
            _ => 0.538,
        };
        assert!(width_em(c) >= widest, "{c:?}");
    }
    // Calibri's lines are 1.2207 ems; a line in another font is taken as two.
    const { assert!(LINE_EM >= 1.2207 && OTHER_FONT_LINE_EM >= LINE_EM) };
}

#[test]
fn text_wraps_at_spaces_and_breaks_a_long_word_between_characters() {
    // At 10 points a lower-case letter is taken as 6 points wide, so ten fill a 60-point line.
    let line = 12.5;
    assert_eq!(height("abcde", 60.0, 10.0), line);
    assert_eq!(
        height("abcde fghij", 60.0, 10.0),
        2.0 * line,
        "the second word wraps whole"
    );
    assert_eq!(
        height("abcdefghijk", 60.0, 10.0),
        2.0 * line,
        "a word wider than the line breaks"
    );
    assert_eq!(
        height("abcde     ", 60.0, 10.0),
        line,
        "spaces at a line's end take no room"
    );
    assert_eq!(
        height("a\nb\n\nc", 60.0, 10.0),
        4.0 * line,
        "each line break starts a line"
    );
    assert_eq!(
        height("a\tb", 80.0, 10.0),
        2.0 * line,
        "a tab takes an inch and comes along to the next line"
    );
    assert_eq!(
        height("\u{98df}", 60.0, 10.0),
        20.0,
        "a line holding a character Calibri lacks is two ems tall"
    );
    assert_eq!(height("", 60.0, 10.0), line, "an empty paragraph is a line");
}

#[test]
fn leading_spaces_and_tabs_take_room_and_wrap_and_nothing_wider_than_the_box_fits() {
    let line = 12.5;
    // The review's probe: tabs before a value. Two tabs fill a 200-point line; the third and the value start the next.
    assert_eq!(height("\t\t\tvalue", 200.0, 10.0), 2.0 * line);
    assert_eq!(
        height(&format!("{}value", "\t".repeat(100)), 200.0, 10.0),
        50.0 * line
    );
    // Leading spaces take room too; spaces at the end of a line hang past it, tabs there do not.
    assert_eq!(
        height(&format!("{}x", " ".repeat(40)), 60.0, 10.0),
        3.0 * line
    );
    assert_eq!(height(&format!("x{}", " ".repeat(40)), 60.0, 10.0), line);
    assert_eq!(height("x\t\t", 100.0, 10.0), 2.0 * line);
    // A tab or a glyph wider than the whole box cannot be placed in it.
    assert_eq!(height("a\tb", 60.0, 10.0), f64::INFINITY);
    assert_eq!(height("\u{12000}", 30.0, 10.0), f64::INFINITY);
    let columns = vec!["a".to_owned(); 4];
    // In the widest column six tabs fill a line: 200 tabs take 34 lines, more than the body holds.
    let tabs = json!(format!("{}value", "\t".repeat(200)));
    assert!(!table(
        &columns,
        &[vec![tabs, json!("b"), json!("c"), json!("d")]]
    ));
}

#[test]
fn a_longer_text_never_takes_less_room() {
    let text = "Vitamin D intake, adults 19\u{2013}70 (\u{b5}g/day) \u{2014} \u{1f1} \u{2194} \u{98df}\u{54c1}\tend";
    let chars: Vec<char> = text.chars().collect();
    let mut last = 0.0;
    for n in 0..=chars.len() {
        let start: String = chars[..n].iter().collect();
        let now = height(&start, 50.0, 11.0);
        assert!(now >= last, "{n}");
        last = now;
    }
}

#[test]
fn tables_and_paragraphs_that_cannot_be_seen_whole_do_not_fit() {
    let columns = vec!["a".to_owned(), "b".to_owned()];
    // The review's probe: a 1,999-character cell of 1,000 lines.
    let tall = format!("{}x", "x\n".repeat(999));
    assert_eq!(tall.chars().count(), 1999);
    assert!(!table(&columns, &[vec![json!(tall), json!("b")]]));
    // Fifteen rows of six 2,000-character cells.
    let six: Vec<String> = (0..6).map(|n| format!("c{n}")).collect();
    let row: Vec<Value> = (0..6).map(|_| json!("word ".repeat(400))).collect();
    assert!(!table(&six, &vec![row; 15]));
    // Fifteen short rows under a header fit, and the rows take the heights the slide writes.
    let short: Vec<Vec<Value>> = (0..15)
        .map(|n| vec![json!(format!("food {n}")), json!(n)])
        .collect();
    assert!(table(&columns, &short));
    let widths = column_widths(&columns, &short, WIDTH);
    let heights = row_heights(&columns, &short, &widths);
    assert_eq!(heights.len(), 16);
    assert!(heights.iter().sum::<i64>() <= BODY.1);
    assert!(!text(&["x\n".repeat(40)]));
    assert!(text(&vec!["A short paragraph.".to_owned(); 6]));
}

#[test]
fn titles_headings_captions_and_the_footer_have_their_own_room() {
    assert!(title("Vitamin D in older adults"));
    assert!(!title(&"Long title words ".repeat(20)));
    assert!(heading("Table 1: foods.csv"));
    assert!(!heading(&"heading ".repeat(30)));
    assert!(caption(&"c".repeat(100)));
    assert!(!caption(&"caption ".repeat(60)));
    assert!(reference(&"r".repeat(100)));
    assert!(!reference(&"reference ".repeat(60)));
    // A caption and a reference within two lines each fit the caption area together, in any script.
    for (c, r) in [
        ("caption words ", "reference words "),
        ("\u{98df}\u{54c1} ", "\u{1f1}\u{2194} "),
        ("W", "M"),
    ] {
        let c = fit_to(&c.repeat(80), caption);
        let r = fit_to(&r.repeat(80), reference);
        assert!(notes(&c, &r), "{c:?} {r:?}");
    }
    let footer = footer_title(&"Title words ".repeat(40));
    assert!(footer.ends_with('\u{2026}') && footer_fits(&footer));
    assert_eq!(footer_title("Short"), "Short");
}

#[test]
fn provenance_entries_fill_two_columns_a_slide_and_continue_in_order() {
    let entries: Vec<Entry> = (0..30)
        .map(|n| Entry {
            label: format!("Note {n}"),
            value: "v".repeat(290),
        })
        .collect();
    let slides = provenance_slides(&entries);
    assert!(slides.len() > 1);
    assert!(slides.iter().all(|s| s.len() <= PROVENANCE_COLUMNS));
    let order: Vec<&str> = slides
        .iter()
        .flatten()
        .flatten()
        .map(|e| e.label.as_str())
        .collect();
    let expected: Vec<&str> = entries.iter().map(|e| e.label.as_str()).collect();
    assert_eq!(order, expected);
    for column in slides.iter().flatten() {
        let used: f64 = column.iter().map(|e| entry_height(e)).sum();
        assert!(used <= points(TEXT.1));
    }
}

#[test]
fn a_deck_body_whose_text_does_not_fit_its_box_is_refused() {
    let body = |title: &str, heading: &str, caption: &str, value: &str| -> Request {
        serde_json::from_value(
            json!({"kind": "pptx", "title": title, "generated_at": "2026-10-17T18:05:00Z",
            "sections": [{"heading": heading,
                "table": {"columns": ["a"], "rows": [["b"]], "caption": caption, "ref": "r"}}],
            "provenance": [{"label": "Run", "value": value}]}),
        )
        .unwrap()
    };
    let refused = Err(WriteError("slide_content_invalid"));
    assert_eq!(check(&body("T", "H", "C", "1")), Ok(()));
    assert_eq!(check(&body(&"title ".repeat(40), "H", "C", "1")), refused);
    assert_eq!(check(&body("T", &"heading ".repeat(25), "C", "1")), refused);
    assert_eq!(check(&body("T", "H", &"c\n".repeat(6), "1")), refused);
    assert_eq!(check(&body("T", "H", "C", &"v\n".repeat(30))), refused);
}
