//! A deck's slides: each paragraph, row, heading and caption within its box by the writer's estimate, nothing left out
//! or shortened without a note, and the writer taking every deck compose writes. Also the figures' pixel budget, which
//! compose and the writer share.
use std::io::Write as _;

use base64::Engine;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use uuid::Uuid;

use super::tests::{body, foods, has_note, proposal, reference, request, source, table};
use super::*;
use crate::broker::document_write::{self, WriteError};
use crate::broker::store::DocumentFigure;

fn sections_named<'a>(doc: &'a Value, start: &str) -> Vec<&'a Value> {
    doc["sections"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|s| s["heading"].as_str().unwrap().starts_with(start))
        .collect()
}

#[test]
fn a_paragraph_too_tall_for_a_slide_continues_on_the_next_with_every_word() {
    // Wide capitals take about eighteen lines of the paragraph box's sixteen.
    let question = "WWWW ".repeat(176).trim_end().to_owned();
    let mut req = request(
        "pptx",
        proposal(),
        vec![source("results/foods.csv", 3, &["food", "vitamin_d"])],
        vec![],
    );
    req.question = question.clone();
    let composed = compose(&Inputs {
        request: &req,
        tables: &foods(),
        figures: &[],
    })
    .unwrap();
    let doc = body(&composed);
    let slides = sections_named(&doc, "Question");
    assert_eq!(slides.len(), 2);
    let mut shown = Vec::new();
    for slide in &slides {
        let paragraphs: Vec<String> = slide["paragraphs"]
            .as_array()
            .unwrap()
            .iter()
            .map(|p| p.as_str().unwrap().to_owned())
            .collect();
        assert!(slide_fit::text(&paragraphs));
        shown.extend(paragraphs);
    }
    assert_eq!(shown.join(" "), question);
    assert!(!has_note(&composed, "On the slides, a title"));
    assert!(!has_note(&composed, "The question is longer"));
    document_write::write(&composed.body).unwrap();
}

#[test]
fn headings_captions_and_column_names_too_long_for_a_slide_are_shortened_and_said() {
    let path = format!("results/{}.csv", "long_name_".repeat(15));
    let column = "c".repeat(60);
    let mut tables = HashMap::new();
    tables.insert(
        (reference(), path.clone()),
        table(&format!("food,{column}\nSalmon,12.5\n")),
    );
    let mut p = proposal();
    p["findings"] = json!([]);
    p["tables"] = json!([{"ref": reference(), "table": path, "columns": ["food", column],
        "lead_in": "lead ".repeat(60).trim_end(), "rows": 1}]);
    let compose_kind = |kind: &str| {
        let req = request(
            kind,
            p.clone(),
            vec![source(&path, 1, &["food", &column])],
            vec![],
        );
        compose(&Inputs {
            request: &req,
            tables: &tables,
            figures: &[],
        })
        .unwrap()
    };
    let composed = compose_kind("pptx");
    let doc = body(&composed);
    let slide = sections_named(&doc, "Table 1")[0];
    let heading = slide["heading"].as_str().unwrap();
    assert!(
        heading.ends_with('\u{2026}') && slide_fit::heading(heading),
        "{heading}"
    );
    let caption = slide["table"]["caption"].as_str().unwrap();
    assert!(
        caption.ends_with('\u{2026}') && slide_fit::caption(caption),
        "{caption}"
    );
    let name = slide["table"]["columns"][1].as_str().unwrap();
    assert_eq!(name.chars().count(), 48);
    assert!(has_note(
        &composed,
        "On the slides, a title, heading or caption too long to fit"
    ));
    assert!(has_note(
        &composed,
        "column names past 48 characters end in"
    ));
    document_write::write(&composed.body).unwrap();
    // A workbook keeps them whole.
    let composed = compose_kind("xlsx");
    let doc = body(&composed);
    let sheet = sections_named(&doc, "Table 1")[0];
    assert_eq!(
        sheet["heading"],
        format!("Table 1: {}", &path["results/".len()..])
    );
    assert_eq!(sheet["table"]["columns"][1], json!(column));
    assert!(!has_note(&composed, "On the slides"));
}

#[test]
fn the_writer_takes_every_deck_compose_writes_however_tall_its_text() {
    let wide = "\u{98df}\u{54c1}WWW mmm ".repeat(150);
    let tall = "line\n".repeat(300);
    let mut rows = String::from("food,note,amount\n");
    for n in 0..60 {
        let first = if n % 2 == 0 { &wide } else { &tall };
        rows.push_str(&format!("\"{first}\",\"{wide}\",{n}.5\n"));
    }
    let mut tables = HashMap::new();
    tables.insert((reference(), "results/foods.csv".to_owned()), table(&rows));
    let mut p = proposal();
    p["findings"] = json!([]);
    p["summary"] = json!(wide.chars().take(1500).collect::<String>());
    p["tables"][0]["columns"] = json!(["food", "note", "amount"]);
    p["tables"][0]["lead_in"] = json!("\u{98df}".repeat(300));
    let mut req = request(
        "pptx",
        p,
        vec![source("results/foods.csv", 60, &["food", "note", "amount"])],
        vec![],
    );
    req.question = tall.clone();
    req.project_title = "WWWW ".repeat(60);
    let composed = compose(&Inputs {
        request: &req,
        tables: &tables,
        figures: &[],
    })
    .unwrap();
    document_write::write(&composed.body).unwrap();
    assert!(has_note(&composed, "cells too tall to fit"));
    assert!(has_note(
        &composed,
        "On the slides, a title, heading or caption"
    ));
}

/// A `side` by `side` one-bit grey PNG whose first pixel byte is `mark`, so each mark gives a distinct file.
fn grey_png(side: u32, mark: u8) -> Vec<u8> {
    let chunk = |out: &mut Vec<u8>, kind: &[u8], body: &[u8]| {
        out.extend_from_slice(&(body.len() as u32).to_be_bytes());
        out.extend_from_slice(kind);
        out.extend_from_slice(body);
        let mut crc = flate2::Crc::new();
        crc.update(kind);
        crc.update(body);
        out.extend_from_slice(&crc.sum().to_be_bytes());
    };
    let row = 1 + side.div_ceil(8) as usize;
    let mut raw = vec![0u8; row * side as usize];
    raw[1] = mark;
    let mut encoder = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::fast());
    encoder.write_all(&raw).unwrap();
    let mut header = side.to_be_bytes().to_vec();
    header.extend_from_slice(&side.to_be_bytes());
    header.extend_from_slice(&[1, 0, 0, 0, 0]);
    let mut out = b"\x89PNG\r\n\x1a\n".to_vec();
    chunk(&mut out, b"IHDR", &header);
    chunk(&mut out, b"IDAT", &encoder.finish().unwrap());
    chunk(&mut out, b"IEND", b"");
    out
}

#[test]
fn figures_past_the_pixel_budget_are_left_out_and_said_and_the_writer_refuses_them() {
    // Five distinct figures of 25 million pixels: four fill the 100 million a document's figures may hold.
    let pngs: Vec<Vec<u8>> = (1..=5).map(|n| grey_png(5000, n)).collect();
    let figures: Vec<DocumentFigure> = pngs
        .iter()
        .enumerate()
        .map(|(n, png)| DocumentFigure {
            reference: reference(),
            artifact: Uuid::nil(),
            package_path: "tasks/x/payload".into(),
            package_bytes: 10,
            package_sha256: "b".repeat(64),
            path: format!("results/figure{n}.png"),
            sha256: hex::encode(Sha256::digest(png)),
        })
        .collect();
    let req = request(
        "docx",
        proposal(),
        vec![source("results/foods.csv", 3, &["food"])],
        figures,
    );
    let composed = compose(&Inputs {
        request: &req,
        tables: &foods(),
        figures: &pngs,
    })
    .unwrap();
    assert_eq!(composed.counts["figures"], 4);
    assert!(has_note(
        &composed,
        "Figure 5 is larger than this document holds"
    ));
    document_write::write(&composed.body).unwrap();
    let sections: Vec<Value> = pngs
        .iter()
        .map(|png| {
            json!({"heading": "Figure", "figure": {
            "png": base64::engine::general_purpose::STANDARD.encode(png), "ref": "r"}})
        })
        .collect();
    let all = json!({"kind": "docx", "title": "T", "generated_at": "2026-10-17T18:05:00Z", "sections": sections,
        "provenance": []});
    assert_eq!(
        document_write::write(&serde_json::to_vec(&all).unwrap()).err(),
        Some(WriteError("document_too_large"))
    );
}
