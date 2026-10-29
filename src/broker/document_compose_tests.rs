use super::*;
use crate::broker::store::{DocumentFigure, DocumentSource, TaskDocument};
use crate::broker::tables;
use time::macros::datetime;
use uuid::Uuid;

const TASK: &str = "01920000-0000-7000-8000-000000000001";
const ARTIFACT: &str = "01920000-0000-7000-8000-0000000000a1";

pub(super) fn reference() -> String {
    format!("artifact://tasks/{TASK}/{ARTIFACT}")
}

pub(super) fn table(text: &str) -> Table {
    tables::parse(text.as_bytes(), b',', "a".repeat(64)).unwrap()
}

pub(super) fn source(path: &str, rows: i64, columns: &[&str]) -> DocumentSource {
    DocumentSource {
        reference: reference(),
        task: Uuid::parse_str(TASK).unwrap(),
        artifact: Uuid::parse_str(ARTIFACT).unwrap(),
        package_path: "tasks/x/payload".into(),
        package_bytes: 10,
        package_sha256: "b".repeat(64),
        table_path: path.into(),
        table_sha256: "c".repeat(64),
        rows,
        columns: columns.iter().map(|c| (*c).to_owned()).collect(),
    }
}

pub(super) fn request(
    kind: &str,
    proposal: Value,
    sources: Vec<DocumentSource>,
    figures: Vec<DocumentFigure>,
) -> TaskDocument {
    TaskDocument {
        kind: kind.into(),
        project: Uuid::nil(),
        project_title: "Vitamin D and bone density".into(),
        run: Uuid::nil(),
        turn: 4,
        decided_at: datetime!(2026-10-17 08:00 UTC),
        question: "Which foods hold the most vitamin D?".into(),
        record_sha256: "d".repeat(64),
        request_sha256: "e".repeat(64),
        requested_at: datetime!(2026-10-17 09:30:00.25 UTC),
        proposal,
        source_count: sources.len(),
        figure_count: figures.len(),
        sources,
        figures,
    }
}

pub(super) fn foods() -> HashMap<(String, String), Table> {
    let mut map = HashMap::new();
    map.insert(
        (reference(), "results/foods.csv".to_owned()),
        table("food,vitamin_d,\nSalmon,12.5,a\nEgg,2.0,b\nMilk,1.3,c\n"),
    );
    map
}

pub(super) fn proposal() -> Value {
    let binding = |row: u64, column: &str| json!({"ref": reference(), "table": "results/foods.csv", "row": row, "column": column});
    json!({
        "answer": "answered",
        "summary": "Fish holds the most vitamin D of the foods listed.",
        "findings": [
            {"statement": "{a} holds {b} µg per 100 g {not a placeholder.", "bindings": {"a": binding(1, "food"), "b": binding(1, "vitamin_d")}},
            {"statement": "The third column of row 2 reads {c}.", "bindings": {"c": binding(2, "column_3")}}
        ],
        "tables": [{"ref": reference(), "table": "results/foods.csv", "columns": ["food", "vitamin_d"], "lead_in": "All foods measured.", "rows": 3}],
        "parts": [{"part": "Which foods", "state": "supported", "findings": [1]}]
    })
}

pub(super) fn body(composed: &Composed) -> Value {
    serde_json::from_slice(&composed.body).unwrap()
}

#[test]
fn text_is_cut_and_split_within_its_limits() {
    assert_eq!(cut("short", 10), "short");
    assert_eq!(cut("abcdefghij", 5), "abcd…");
    let long = "word ".repeat(1000);
    let parts = pieces(&long, 2000);
    assert!(
        parts
            .iter()
            .all(|p| p.chars().count() <= 2000 && !p.ends_with(' '))
    );
    assert_eq!(
        parts.join(" ").split_whitespace().count(),
        1000,
        "no word is lost at a break"
    );
    assert_eq!(pieces("a\n\n b \n", 10), ["a", "b"]);
    assert_eq!(
        pieces(&"x".repeat(25), 10).len(),
        3,
        "a word longer than the limit is split hard"
    );
}

#[test]
fn a_workbook_cell_is_a_number_only_when_its_text_is_the_numbers_own_spelling() {
    let number = ColumnType::Number;
    assert_eq!(workbook_cell("12", ColumnType::Integer), json!(12));
    assert_eq!(workbook_cell("12.5", number), json!(12.5));
    assert_eq!(workbook_cell("0.000123", number), json!(0.000123));
    assert_eq!(workbook_cell("-3.25", number), json!(-3.25));
    for text in [
        "0.10",
        "007",
        "1e-7",
        "-0",
        "NA",
        "1.",
        ".5",
        "1234567890123456",
        "12a",
        "+1",
    ] {
        assert_eq!(workbook_cell(text, number), json!(text), "{text}");
    }
    assert_eq!(workbook_cell("", number), Value::Null);
    // A plain spelling past the double's range would print back differently, so it stays text.
    let underflow = format!("0.{}1", "0".repeat(323));
    assert_eq!(workbook_cell(&underflow, number), json!(underflow));
    assert_eq!(
        workbook_cell("0.000000000000000000001", number),
        json!(0.000000000000000000001)
    );
    assert_eq!(workbook_cell("12", ColumnType::String), json!("12"));
    assert_eq!(workbook_cell("", ColumnType::String), json!(""));
}

#[test]
fn findings_take_each_value_from_the_cell_they_bind() {
    let tables = foods();
    let (sentence, values) = fill(&proposal()["findings"][0], &tables).unwrap();
    assert_eq!(
        sentence,
        "Salmon holds 12.5 µg per 100 g {not a placeholder."
    );
    assert_eq!(values.len(), 2);
    assert_eq!(
        values[1].2,
        (
            reference(),
            "results/foods.csv".to_owned(),
            1,
            "vitamin_d".to_owned()
        )
    );
    // An empty header is named as the digest names it, so a binding to it resolves.
    let (sentence, _) = fill(&proposal()["findings"][1], &tables).unwrap();
    assert_eq!(sentence, "The third column of row 2 reads b.");
}

#[test]
fn a_value_that_does_not_resolve_refuses_the_document() {
    let tables = foods();
    let mut finding = proposal()["findings"][0].clone();
    finding["bindings"]["b"]["row"] = json!(9);
    assert_eq!(
        fill(&finding, &tables),
        Err(Refusal("document_value_missing"))
    );
    finding["bindings"]["b"]["row"] = json!(1);
    finding["bindings"]["b"]["column"] = json!("vitamin_e");
    assert_eq!(
        fill(&finding, &tables),
        Err(Refusal("document_value_missing"))
    );
    let unbound = json!({"statement": "{z} is missing", "bindings": {}});
    assert_eq!(
        fill(&unbound, &tables),
        Err(Refusal("document_record_invalid"))
    );
}

#[test]
fn a_document_holds_every_part_in_order_with_counts_the_service_reports() {
    let tables = foods();
    let req = request(
        "docx",
        proposal(),
        vec![source(
            "results/foods.csv",
            3,
            &["food", "vitamin_d", "column_3"],
        )],
        vec![],
    );
    let composed = compose(&Inputs {
        request: &req,
        tables: &tables,
        figures: &[],
    })
    .unwrap();
    let doc = body(&composed);
    let headings: Vec<&str> = doc["sections"]
        .as_array()
        .unwrap()
        .iter()
        .map(|s| s["heading"].as_str().unwrap())
        .collect();
    assert_eq!(
        headings,
        [
            "Question",
            "Answer",
            "Findings",
            "Where each value comes from",
            "Parts of the question",
            "Table 1: foods.csv",
            "Sources"
        ]
    );
    assert_eq!(doc["generated_at"], "2026-10-17T09:30:00.25Z");
    assert_eq!(doc["title"], "Vitamin D and bone density");
    assert_eq!(
        doc["sections"][1]["paragraphs"][0],
        "The agent's reading is that the results answer the question."
    );
    assert_eq!(
        doc["sections"][1]["paragraphs"]
            .as_array()
            .unwrap()
            .last()
            .unwrap(),
        parts::ACCEPTANCE
    );
    assert_eq!(
        doc["sections"][5]["table"]["rows"],
        json!([["Salmon", "12.5"], ["Egg", "2.0"], ["Milk", "1.3"]])
    );
    assert_eq!(doc["sections"][4]["table"]["rows"][0][1], "Answered");
    // values (3 rows x 5) + parts (1 x 3) + answer table (3 x 2) + sources (1 x 5)
    assert_eq!(
        composed.counts,
        json!({"sections": 7, "tables": 4, "cells": 29, "figures": 0})
    );
    assert_eq!(composed.values.len(), 3);
    assert_eq!(
        composed.csv,
        [(
            "tables/answer-table-1.csv".to_owned(),
            b"food,vitamin_d\nSalmon,12.5\nEgg,2.0\nMilk,1.3\n".to_vec()
        )]
    );
    let provenance = doc["provenance"].as_array().unwrap();
    assert!(provenance.iter().any(|p| p["label"] == "Acceptance"));
    assert!(
        provenance
            .iter()
            .all(|p| p["value"].as_str().unwrap().chars().count() <= 300)
    );
    let again = compose(&Inputs {
        request: &req,
        tables: &tables,
        figures: &[],
    })
    .unwrap();
    assert_eq!(
        again.body, composed.body,
        "the same request gives the same bytes"
    );
}

#[test]
fn a_workbook_types_numbers_and_a_deck_splits_tables_into_slides() {
    let mut rows = String::from("food,vitamin_d\n");
    for n in 0..100 {
        rows.push_str(&format!("food {n},{n}.5\n"));
    }
    let mut tables = HashMap::new();
    tables.insert((reference(), "results/foods.csv".to_owned()), table(&rows));
    let mut proposal = proposal();
    proposal["findings"] = json!([]);
    let req = request(
        "xlsx",
        proposal.clone(),
        vec![source("results/foods.csv", 100, &["food", "vitamin_d"])],
        vec![],
    );
    let doc = body(
        &compose(&Inputs {
            request: &req,
            tables: &tables,
            figures: &[],
        })
        .unwrap(),
    );
    let table = doc["sections"]
        .as_array()
        .unwrap()
        .iter()
        .find(|s| s["heading"] == "Table 1: foods.csv")
        .unwrap();
    assert_eq!(table["table"]["rows"][0], json!(["food 0", 0.5]));
    assert_eq!(table["table"]["rows"].as_array().unwrap().len(), 100);

    let req = request(
        "pptx",
        proposal,
        vec![source("results/foods.csv", 100, &["food", "vitamin_d"])],
        vec![],
    );
    let composed = compose(&Inputs {
        request: &req,
        tables: &tables,
        figures: &[],
    })
    .unwrap();
    let doc = body(&composed);
    let sections = doc["sections"].as_array().unwrap();
    let slides: Vec<&Value> = sections
        .iter()
        .filter(|s| s["heading"].as_str().unwrap().starts_with("Table 1"))
        .collect();
    assert_eq!(slides.len(), 3);
    assert!(
        slides
            .iter()
            .all(|s| s["table"]["rows"].as_array().unwrap().len() == 15)
    );
    assert_eq!(slides[1]["heading"], "Table 1: foods.csv (2)");
    assert!(
        composed
            .notes
            .iter()
            .any(|n| n.contains("rows 1 to 45 of 100"))
    );
    for section in sections {
        let blocks = ["paragraphs", "table", "figure"]
            .iter()
            .filter(|k| section.get(**k).is_some())
            .count();
        assert_eq!(blocks, 1, "a slide holds one kind of content");
        if let Some(table) = section.get("table") {
            assert!(
                table["rows"].as_array().unwrap().len() <= 15
                    && table["columns"].as_array().unwrap().len() <= 8
            );
        }
    }
    assert_eq!(
        composed.csv[0].1.iter().filter(|b| **b == b'\n').count(),
        101,
        "the CSV keeps every row"
    );
}

#[test]
fn figures_are_sent_whole_and_counted() {
    let tables = foods();
    let figure = DocumentFigure {
        reference: reference(),
        artifact: Uuid::parse_str(ARTIFACT).unwrap(),
        package_path: "tasks/x/payload".into(),
        package_bytes: 10,
        package_sha256: "b".repeat(64),
        path: "results/figure.png".into(),
        sha256: "f".repeat(64),
    };
    let req = request(
        "docx",
        proposal(),
        vec![source("results/foods.csv", 3, &["food"])],
        vec![figure],
    );
    let png = tiny_png();
    let composed = compose(&Inputs {
        request: &req,
        tables: &tables,
        figures: std::slice::from_ref(&png),
    })
    .unwrap();
    let doc = body(&composed);
    let figure = doc["sections"]
        .as_array()
        .unwrap()
        .iter()
        .find(|s| s.get("figure").is_some())
        .unwrap();
    assert_eq!(
        figure["figure"]["png"],
        base64::engine::general_purpose::STANDARD.encode(&png)
    );
    assert_eq!(
        figure["figure"]["caption"],
        "figure.png from result 01920000"
    );
    assert_eq!(composed.counts["figures"], 1);
    assert_eq!(
        compose(&Inputs {
            request: &req,
            tables: &tables,
            figures: &[]
        })
        .err(),
        Some(Refusal("document_request_invalid")),
        "every pinned figure's bytes are required"
    );
}

pub(super) fn compose_with(
    kind: &str,
    proposal: Value,
    tables: &HashMap<(String, String), Table>,
) -> Composed {
    let req = request(
        kind,
        proposal,
        vec![source(
            "results/foods.csv",
            3,
            &["food", "vitamin_d", "column_3"],
        )],
        vec![],
    );
    compose(&Inputs {
        request: &req,
        tables,
        figures: &[],
    })
    .unwrap()
}

/// A 1 by 1 RGB PNG, the smallest well-formed still image the writer places.
pub(super) fn tiny_png() -> Vec<u8> {
    let chunk = |out: &mut Vec<u8>, kind: &[u8], body: &[u8]| {
        out.extend_from_slice(&(body.len() as u32).to_be_bytes());
        out.extend_from_slice(kind);
        out.extend_from_slice(body);
        let mut crc = flate2::Crc::new();
        crc.update(kind);
        crc.update(body);
        out.extend_from_slice(&crc.sum().to_be_bytes());
    };
    let mut out = b"\x89PNG\r\n\x1a\n".to_vec();
    chunk(&mut out, b"IHDR", &[0, 0, 0, 1, 0, 0, 0, 1, 8, 2, 0, 0, 0]);
    // zlib of the one row: filter 0 and three zero samples.
    chunk(
        &mut out,
        b"IDAT",
        &[
            0x78, 0x01, 0x63, 0x60, 0x60, 0x60, 0x00, 0x00, 0x00, 0x04, 0x00, 0x01,
        ],
    );
    chunk(&mut out, b"IEND", b"");
    out
}

#[test]
fn a_figure_that_is_not_one_well_formed_png_is_left_out_and_said() {
    let figure = DocumentFigure {
        reference: reference(),
        artifact: Uuid::nil(),
        package_sha256: "e".repeat(64),
        package_path: "tasks/x/payload".into(),
        package_bytes: 1,
        path: "results/figure.png".into(),
        sha256: "f".repeat(64),
    };
    let req = request(
        "docx",
        proposal(),
        vec![source("results/foods.csv", 3, &["food"])],
        vec![figure],
    );
    let fake = b"\x89PNG\r\n\x1a\nnot really".to_vec();
    let composed = compose(&Inputs {
        request: &req,
        tables: &foods(),
        figures: std::slice::from_ref(&fake),
    })
    .unwrap();
    assert_eq!(composed.counts["figures"], 0);
    assert!(has_note(
        &composed,
        "Figure 1 is not a still PNG that shows every pixel as stored (no undefined pixel or colour profile), so \
         this document leaves it out"
    ));
}

#[test]
fn every_kind_of_note_at_its_longest_fits_the_receipt() {
    let all: Vec<usize> = (1..=20).collect();
    let tables = vec![(1, 120_000), (2, 120_000), (3, 120_000), (4, 120_000)];
    let notes = notes::Notes {
        question: true,
        summary: true,
        finding_paragraphs: all.clone(),
        finding_spec: all.clone(),
        findings_left_out: true,
        values_listed: Some((45, 1_000_000)),
        parts_long: true,
        repeated_columns: vec![1, 2, 3, 4],
        repeated_tables: vec![(2, 1), (3, 1), (4, 1)],
        csv_marked: vec![
            (1, 2_000_000),
            (2, 2_000_000),
            (3, 2_000_000),
            (4, 2_000_000),
        ],
        csv_left_out: vec![1, 2, 3, 4],
        rows_shown: vec![
            (1, 20_000, 1_000_000),
            (2, 20_000, 1_000_000),
            (3, 20_000, 1_000_000),
            (4, 20_000, 1_000_000),
        ],
        long_cells: vec![1, 2, 3, 4],
        altered_cells: tables,
        figures_too_large: (1..=8).collect(),
        figures_unreadable: (1..=8).collect(),
        figures_left_out: 8,
        slide_text_cut: true,
        slide_cells_cut: true,
        other_text: true,
    };
    for deck in [false, true] {
        let rendered = notes.render(deck).unwrap();
        assert_eq!(rendered.len(), notes::MAX_NOTES);
        for note in &rendered {
            assert!(
                note.chars().count() <= notes::NOTE_CHARS,
                "{} characters: {note}",
                note.chars().count()
            );
        }
    }
}

pub(super) fn has_note(composed: &Composed, words: &str) -> bool {
    composed.notes.iter().any(|n| n.contains(words))
        && body(composed)["provenance"]
            .as_array()
            .unwrap()
            .iter()
            .any(|e| e["label"] == "Note" && e["value"].as_str().unwrap().contains(words))
}

#[test]
fn table_cells_are_sent_as_the_writer_keeps_them_and_the_change_is_said() {
    let mut tables = HashMap::new();
    tables.insert(
        (reference(), "results/foods.csv".to_owned()),
        table("food,vitamin_d,\nSal\u{85}mon,12.5,a\nEgg\u{ffff},2.0,b\nMilk,1.3,c\n"),
    );
    let mut p = proposal();
    let binding =
        json!({"ref": reference(), "table": "results/foods.csv", "row": 2, "column": "food"});
    p["findings"][1] = json!({"statement": "Row 2 names {c}.", "bindings": {"c": binding}});
    let composed = compose_with("docx", p, &tables);
    let doc = body(&composed);
    let table = doc["sections"]
        .as_array()
        .unwrap()
        .iter()
        .find(|s| s["heading"].as_str().unwrap().starts_with("Table 1"))
        .unwrap();
    assert_eq!(table["table"]["rows"][0][0], "Sal mon");
    assert_eq!(table["table"]["rows"][1][0], "Egg\u{fffd}");
    assert!(has_note(
        &composed,
        "Table 1: 2 cells held characters these files cannot store"
    ));
    // A finding value takes the digest's text (controls as spaces); one the files cannot store is said in general, and
    // spec.json keeps the stored value.
    assert_eq!(composed.values[0]["value"], "Sal mon");
    assert_eq!(composed.values[2]["value"], "Egg\u{ffff}");
    assert!(has_note(&composed, "Some text held control characters"));
    assert_eq!(
        doc["sections"][2]["paragraphs"][1],
        "2. Row 2 names Egg\u{fffd}."
    );
    let csv = String::from_utf8(composed.csv[0].1.clone()).unwrap();
    assert!(csv.contains("Sal\u{85}mon") && csv.contains("Egg\u{ffff}"));
    let sent = String::from_utf8(composed.body.clone()).unwrap();
    assert!(!sent.chars().any(|c| matches!(c, '\u{0}'..='\u{8}' | '\u{b}'..='\u{1f}' | '\u{7f}'..='\u{9f}' | '\u{fffe}' | '\u{ffff}')));
}

#[test]
fn a_summary_past_the_paragraph_limit_is_said_to_be_shortened() {
    let mut p = proposal();
    p["summary"] = json!(
        (1..=20)
            .map(|n| format!("Line {n}."))
            .collect::<Vec<_>>()
            .join("\n")
    );
    for kind in ["docx", "xlsx"] {
        let composed = compose_with(kind, p.clone(), &foods());
        assert!(has_note(&composed, "The summary is longer"), "{kind}");
        let doc = body(&composed);
        let answer = &doc["sections"][1]["paragraphs"];
        assert_eq!(answer.as_array().unwrap().len(), 20);
        assert_eq!(answer[19], parts::ACCEPTANCE);
    }
    let mut p = proposal();
    p["summary"] = json!(
        (1..=18)
            .map(|n| format!("Line {n}."))
            .collect::<Vec<_>>()
            .join("\n")
    );
    assert!(
        !compose_with("docx", p, &foods())
            .notes
            .iter()
            .any(|n| n.contains("summary"))
    );
}

#[test]
fn a_finding_cut_to_a_paragraph_is_said_to_be_and_kept_whole_in_the_spec() {
    let mut tables = HashMap::new();
    let long = "x".repeat(160);
    tables.insert(
        (reference(), "results/foods.csv".to_owned()),
        table(&format!(
            "food,vitamin_d,\n{long},12.5,a\nEgg,2.0,b\nMilk,1.3,c\n"
        )),
    );
    let mut p = proposal();
    let binding =
        json!({"ref": reference(), "table": "results/foods.csv", "row": 1, "column": "food"});
    p["findings"] =
        json!([{"statement": "{a} ".repeat(20).trim_end(), "bindings": {"a": binding}}]);
    let composed = compose_with("docx", p, &tables);
    assert!(has_note(
        &composed,
        "Finding 1 is longer than a paragraph holds"
    ));
    let whole = composed.findings[0]["sentence"].as_str().unwrap();
    assert_eq!(whole.chars().count(), 20 * 160 + 19);
    let doc = body(&composed);
    let shown = doc["sections"][2]["paragraphs"][0].as_str().unwrap();
    assert_eq!(shown.chars().count(), 2000);
    assert!(shown.ends_with('…'));
}

#[test]
fn csv_copies_open_formula_like_text_as_text_and_keep_numbers() {
    for (text, safe) in [
        ("=SUM(A1:A2)", "'=SUM(A1:A2)"),
        ("+1", "'+1"),
        ("@cmd", "'@cmd"),
        ("-x", "'-x"),
        ("\tlead", "'\tlead"),
        ("-12", "-12"),
        ("-1.5e-3", "-1.5e-3"),
        ("12", "12"),
        ("a=b", "a=b"),
        ("", ""),
        // Excel takes a first line beginning sep= as its separator, so such a cell starts with an apostrophe too.
        ("sep='", "'sep='"),
        ("SEP=,", "'SEP=,"),
        ("sep", "sep"),
        (" sep=,", " sep=,"),
    ] {
        assert_eq!(parts::spreadsheet_safe(text), safe, "{text}");
    }
    let mut tables = HashMap::new();
    tables.insert(
        (reference(), "results/foods.csv".to_owned()),
        table("food,vitamin_d,\n=HYPERLINK(1),12.5,a\nEgg,-2.0,b\nMilk,1.3,c\n"),
    );
    let composed = compose_with("xlsx", proposal(), &tables);
    let csv = String::from_utf8(composed.csv[0].1.clone()).unwrap();
    assert!(
        csv.contains("'=HYPERLINK(1)") && csv.contains("-2.0"),
        "{csv}"
    );
    assert!(has_note(
        &composed,
        "cells beginning =, +, -, @, a tab or sep= start with an apostrophe (Table 1: 1 cell)"
    ));
    // The review's probe: a first column named sep=' would make Excel split the marked cells at their apostrophes and
    // read =1+1 as a formula.
    let mut tables = HashMap::new();
    tables.insert(
        (reference(), "results/foods.csv".to_owned()),
        table("sep=',x\n=1+1,1\n"),
    );
    let mut p = proposal();
    p["findings"] = json!([]);
    p["tables"][0]["columns"] = json!(["sep='"]);
    let composed = compose_with("xlsx", p, &tables);
    let csv = String::from_utf8(composed.csv[0].1.clone()).unwrap();
    let lines: Vec<&str> = csv.lines().collect();
    assert_eq!(lines, ["'sep='", "'=1+1"], "{csv}");
    assert!(has_note(
        &composed,
        "a tab or sep= start with an apostrophe (Table 1: 2 cells)"
    ));
}

#[test]
fn answer_tables_are_bounded_as_admission_bounds_them() {
    let mut p = proposal();
    p["tables"][0]["columns"] = json!(["food", "food", "vitamin_d"]);
    let again = p["tables"][0].clone();
    p["tables"].as_array_mut().unwrap().push(again);
    let composed = compose_with("docx", p, &foods());
    assert!(has_note(&composed, "Table 1 names a column more than once"));
    assert!(has_note(&composed, "Table 2 repeats Table 1"));
    assert_eq!(composed.csv.len(), 1);
    let table = body(&composed)["sections"]
        .as_array()
        .unwrap()
        .iter()
        .find(|s| s["heading"].as_str().unwrap().starts_with("Table 1"))
        .unwrap()
        .clone();
    assert_eq!(table["table"]["columns"], json!(["food", "vitamin_d"]));
    let req = |p: Value| {
        request(
            "docx",
            p,
            vec![source(
                "results/foods.csv",
                3,
                &["food", "vitamin_d", "column_3"],
            )],
            vec![],
        )
    };
    let mut wide = proposal();
    wide["tables"][0]["columns"] = json!(["a", "b", "c", "d", "e", "f", "g"]);
    let tables = foods();
    let refused = compose(&Inputs {
        request: &req(wide),
        tables: &tables,
        figures: &[],
    })
    .map(|_| ());
    assert_eq!(refused, Err(Refusal("document_record_invalid")));
    let mut many = proposal();
    many["tables"] = json!(vec![proposal()["tables"][0].clone(); 5]);
    let refused = compose(&Inputs {
        request: &req(many),
        tables: &tables,
        figures: &[],
    })
    .map(|_| ());
    assert_eq!(refused, Err(Refusal("document_record_invalid")));
}

#[test]
fn a_csv_copy_past_its_bound_is_left_to_the_result_package() {
    let big = format!("food,vitamin_d\n{}", "x,1\n".repeat(10));
    let table = table(&big);
    let indices = [0usize, 1];
    let columns = ["food".to_owned(), "vitamin_d".to_owned()];
    assert!(
        parts::table_csv(&table, &columns, &indices, 1_000)
            .unwrap()
            .is_some()
    );
    assert!(
        parts::table_csv(&table, &columns, &indices, 20)
            .unwrap()
            .is_none()
    );
}

#[test]
fn document_text_never_carries_a_direction_control() {
    // A bound value written between direction controls would display reversed; each control shows as U+FFFD.
    let (text, changed) = plain("Mean \u{202E}12.5\u{202C}, then \u{2067}7\u{2069} and \u{200F}9.");
    assert!(changed);
    assert_eq!(
        text,
        "Mean \u{fffd}12.5\u{fffd}, then \u{fffd}7\u{fffd} and \u{fffd}9."
    );
    assert_eq!(
        plain("Mean 12.5 and 7."),
        ("Mean 12.5 and 7.".to_string(), false)
    );
}
