//! The sections of an answer document in their order, and the request body (see `document_compose`).
use super::*;

pub(super) const ACCEPTANCE: &str = "No person has accepted this answer yet. Each value below names the stored result it comes from, so it can be checked.";
const VALUES: &str = "Each finding's values are copied by code from the result tables listed under Sources. The summary \
    is the agent's own words; a number in it was accepted only when a result the agent was shown holds it.";
/// Slides a deck gives the question, the answer and the findings.
const QUESTION_SLIDES: usize = 2;
const ANSWER_SLIDES: usize = 3;
const FINDING_SLIDES: usize = 4;
/// Text kept free for the provenance and the service's own words when answer tables fill a kind's text budget.
const TEXT_RESERVE: usize = 256 * 1024;
/// PNG bytes the figures may hold together, under the service's 16 MiB canonical limit.
const FIGURES_BYTES: usize = 12 * 1024 * 1024;
/// Answer tables and their distinct columns as the coordinator admits them (`coordinator/proposal.rs`).
const ANSWER_TABLES: usize = 4;
const ANSWER_COLUMNS: usize = 6;
/// The CSV copies a package keeps, for one table and for all.
const CSV_TABLE_BYTES: usize = 24 * 1024 * 1024;
const CSV_BYTES: usize = 48 * 1024 * 1024;
/// A finding's whole sentence as spec.json keeps it.
const SPEC_SENTENCE_CHARS: usize = 20_000;

fn cell_budget(kind: &str) -> usize {
    match kind {
        "xlsx" => 1_000_000,
        "docx" => 20_000,
        _ => 2_400,
    }
}

fn text_budget(kind: &str) -> usize {
    match kind {
        "xlsx" => 32 << 20,
        "docx" => 8 << 20,
        _ => 2 << 20,
    }
}

/// Paragraph sections for `heading`; a deck gives at most `max_slides`. True when paragraphs were left out.
fn paragraphs(
    sections: &mut Vec<Section>,
    deck: bool,
    heading: &str,
    mut list: Vec<String>,
    max_slides: usize,
) -> bool {
    let mut dropped = false;
    if list.len() > PARAGRAPHS {
        list.truncate(PARAGRAPHS);
        dropped = true;
    }
    if deck {
        let groups = slides(list);
        dropped |= groups.len() > max_slides;
        list = groups.into_iter().take(max_slides).flatten().collect();
    }
    push_paragraphs(sections, deck, heading, list);
    dropped
}

fn text_bytes(sections: &[Section]) -> usize {
    sections
        .iter()
        .map(|s| {
            s.heading.len()
                + match &s.block {
                    Block::Paragraphs(list) => list.iter().map(String::len).sum(),
                    Block::Table {
                        columns,
                        rows,
                        caption,
                        reference,
                    } => {
                        columns.iter().map(String::len).sum::<usize>()
                            + rows
                                .iter()
                                .flatten()
                                .filter_map(Value::as_str)
                                .map(str::len)
                                .sum::<usize>()
                            + caption.len()
                            + reference.len()
                    }
                    Block::Figure {
                        caption, reference, ..
                    } => caption.len() + reference.len(),
                }
        })
        .sum()
}

fn cells(sections: &[Section]) -> usize {
    sections
        .iter()
        .map(|s| match &s.block {
            Block::Table { columns, rows, .. } => columns.len() * rows.len(),
            _ => 0,
        })
        .sum()
}

/// An answer table's chosen columns, every row, as RFC 4180 CSV with the stored text.
/// A text a spreadsheet would read as a formula, or as a separator line (Excel takes a first line beginning `sep=` as
/// one, which would make the apostrophes below separators), starts with `'` in a CSV copy, so opening the copy runs
/// nothing; a plain number keeps its sign. The document's own workbook needs no such mark: its writer stores text as
/// text.
pub(super) fn spreadsheet_safe(text: &str) -> std::borrow::Cow<'_, str> {
    static NUMBER: std::sync::LazyLock<regex::Regex> = std::sync::LazyLock::new(|| {
        regex::Regex::new(r"^-?[0-9]+(?:\.[0-9]+)?(?:[eE][+-]?[0-9]+)?$").expect("number pattern")
    });
    let formula = text.starts_with(['=', '+', '-', '@', '\t', '\r']) && !NUMBER.is_match(text);
    let separator = text
        .get(..4)
        .is_some_and(|start| start.eq_ignore_ascii_case("sep="));
    if formula || separator {
        format!("'{text}").into()
    } else {
        text.into()
    }
}

/// Every row of an answer table's chosen columns as CSV, or `None` when it would pass `limit` bytes; with how many
/// cells were marked as text by `spreadsheet_safe`.
pub(super) fn table_csv(
    table: &Table,
    columns: &[String],
    indices: &[usize],
    limit: usize,
) -> Result<Option<(Vec<u8>, usize)>, Refusal> {
    let mut writer = csv::Writer::from_writer(Vec::new());
    let failed = |_| Refusal("document_compose_failed");
    let mut marked = 0;
    let mut safe = |text: &str| {
        let out = spreadsheet_safe(text);
        marked += usize::from(out.len() != text.len());
        out.into_owned()
    };
    let header: Vec<String> = columns.iter().map(|c| safe(c)).collect();
    writer.write_record(&header).map_err(failed)?;
    for row in 0..table.rows() {
        let record: Vec<String> = indices
            .iter()
            .map(|&i| safe(table.cell(row, i).unwrap_or("")))
            .collect();
        writer.write_record(&record).map_err(failed)?;
        if writer.get_ref().len() > limit {
            return Ok(None);
        }
    }
    let bytes = writer
        .into_inner()
        .map_err(|_| Refusal("document_compose_failed"))?;
    // The writer buffers before its output, so the bound holds only after the last flush.
    Ok((bytes.len() <= limit).then_some((bytes, marked)))
}

fn strings(names: &[&str]) -> Vec<String> {
    names.iter().map(|n| (*n).to_owned()).collect()
}

fn section_json(section: &Section) -> Value {
    match &section.block {
        Block::Paragraphs(list) => json!({"heading": section.heading, "paragraphs": list}),
        Block::Table {
            columns,
            rows,
            caption,
            reference,
        } => json!({"heading": section.heading,
            "table": {"columns": columns, "rows": rows, "caption": caption, "ref": reference}}),
        Block::Figure {
            png,
            caption,
            reference,
        } => json!({"heading": section.heading,
            "figure": {"png": png, "caption": caption, "ref": reference}}),
    }
}

/// The writer's request body for this document, with the counts the writer must report for it.
pub(crate) fn compose(inputs: &Inputs<'_>) -> Result<Composed, Refusal> {
    let request = inputs.request;
    let kind = request.kind.as_str();
    if !KINDS.contains(&kind) || request.figures.len() != inputs.figures.len() {
        return Err(Refusal("document_request_invalid"));
    }
    let (deck, workbook) = (kind == "pptx", kind == "xlsx");
    let proposal = &request.proposal;
    let mut notes = Notes::default();
    let mut sections: Vec<Section> = Vec::new();

    // A deck's paragraphs are pieces short enough for one slide each.
    let piece = if deck {
        SLIDE_PIECE_CHARS
    } else {
        PARAGRAPH_CHARS
    };
    let mut question = pieces(&request.question, piece);
    if question.is_empty() {
        question.push("The run's question was not recorded.".to_owned());
    }
    notes.question = paragraphs(&mut sections, deck, "Question", question, QUESTION_SLIDES);

    let mut answer = vec![reading(proposal["answer"].as_str().unwrap_or("not_yet")).to_owned()];
    answer.extend(pieces(proposal["summary"].as_str().unwrap_or(""), piece));
    let summary_cut = answer.len() > PARAGRAPHS - 1;
    answer.truncate(PARAGRAPHS - 1);
    answer.push(ACCEPTANCE.to_owned());
    notes.summary = paragraphs(&mut sections, deck, "Answer", answer, ANSWER_SLIDES) || summary_cut;

    let findings = proposal["findings"].as_array().ok_or_else(record_invalid)?;
    let (mut sentences, mut value_rows, mut values) = (Vec::new(), Vec::new(), Vec::new());
    let mut whole = Vec::new();
    for (n, finding) in findings.iter().enumerate() {
        let (sentence, filled) = fill(finding, inputs.tables)?;
        let numbered = format!("{}. {sentence}", n + 1);
        if numbered.chars().count() > piece {
            notes.finding_paragraphs.push(n + 1);
        }
        sentences.push(cut(&numbered, piece));
        if sentence.chars().count() > SPEC_SENTENCE_CHARS {
            notes.finding_spec.push(n + 1);
        }
        whole.push(json!({"finding": n + 1, "sentence": cut(&sentence, SPEC_SENTENCE_CHARS)}));
        for (slot, value, (reference, table, row, column)) in filled {
            value_rows.push(vec![
                json!(n + 1),
                json!(value),
                json!(table),
                json!(row),
                json!(column),
            ]);
            values.push(
                json!({"finding": n + 1, "slot": slot, "value": value, "ref": reference,
                "table": table, "row": row, "column": column}),
            );
        }
    }
    notes.findings_left_out =
        paragraphs(&mut sections, deck, "Findings", sentences, FINDING_SLIDES);
    if !value_rows.is_empty() {
        let total = value_rows.len();
        if deck {
            value_rows.truncate(deck_rows(total, SLIDES_PER_LIST));
        }
        let (shown, cells_cut) = push_table(
            &mut sections,
            deck,
            TableParts {
                heading: "Where each value comes from",
                columns: strings(&["Finding", "Value", "File", "Row", "Column"]),
                rows: value_rows,
                caption: "Each value is copied from this cell of a stored result table.",
                reference: "Rows count from 1 below the header row.",
            },
            SLIDES_PER_LIST,
        );
        notes.slide_cells_cut |= cells_cut;
        if shown < total {
            notes.values_listed = Some((shown, total));
        }
    }

    if let Some(parts) = proposal["parts"].as_array().filter(|list| !list.is_empty()) {
        let long = |text: &str| text.chars().count() > CELL_CHARS;
        notes.parts_long = parts.iter().any(|p| {
            long(p["part"].as_str().unwrap_or("")) || long(p["reason"].as_str().unwrap_or(""))
        });
        let rows = parts
            .iter()
            .map(|part| {
                vec![
                    json!(cut(part["part"].as_str().unwrap_or(""), CELL_CHARS)),
                    json!(part_state(part["state"].as_str().unwrap_or(""))),
                    json!(cut(part["reason"].as_str().unwrap_or(""), CELL_CHARS)),
                ]
            })
            .collect();
        let (_, cells_cut) = push_table(
            &mut sections,
            deck,
            TableParts {
                heading: "Parts of the question",
                columns: strings(&["Part", "State", "Reason"]),
                rows,
                caption: "The agent's account of each part of the question.",
                reference: "",
            },
            usize::MAX,
        );
        notes.slide_cells_cut |= cells_cut;
    }

    // The answer tables share what the other tables leave of the kind's cell and text budgets.
    let answer_tables = proposal["tables"].as_array().ok_or_else(record_invalid)?;
    let mut cells_left =
        cell_budget(kind).saturating_sub(cells(&sections) + request.sources.len() * 5);
    let mut text_left = text_budget(kind).saturating_sub(text_bytes(&sections) + TEXT_RESERVE);
    // The coordinator admits at most four answer tables of at most six columns; a record past that did not come
    // through admission and is refused rather than read.
    if answer_tables.len() > ANSWER_TABLES {
        return Err(record_invalid());
    }
    let (mut csv, mut csv_left, mut seen) = (Vec::new(), CSV_BYTES, Vec::new());
    for (n, entry) in answer_tables.iter().enumerate() {
        let number = n + 1;
        let reference = entry["ref"].as_str().ok_or_else(record_invalid)?;
        let path = entry["table"].as_str().ok_or_else(record_invalid)?;
        let named: Vec<String> = entry["columns"]
            .as_array()
            .ok_or_else(record_invalid)?
            .iter()
            .map(|c| c.as_str().map(str::to_owned).ok_or_else(record_invalid))
            .collect::<Result<_, _>>()?;
        let mut columns: Vec<String> = Vec::with_capacity(named.len());
        for column in named.iter() {
            if !columns.contains(column) {
                columns.push(column.clone());
            }
        }
        if columns.len() > ANSWER_COLUMNS {
            return Err(record_invalid());
        }
        let key = (reference.to_owned(), path.to_owned(), columns.clone());
        if let Some(first) = seen.iter().position(|k| *k == key) {
            notes.repeated_tables.push((number, first + 1));
            seen.push((String::new(), String::new(), Vec::new()));
            continue;
        }
        seen.push(key);
        if columns.len() < named.len() {
            notes.repeated_columns.push(number);
        }
        let table = inputs
            .tables
            .get(&(reference.to_owned(), path.to_owned()))
            .ok_or(Refusal("document_value_missing"))?;
        let indices: Vec<usize> = columns
            .iter()
            .map(|c| column_index(table, c).ok_or(Refusal("document_value_missing")))
            .collect::<Result<_, _>>()?;
        if columns.is_empty() {
            return Err(record_invalid());
        }
        match table_csv(table, &columns, &indices, csv_left.min(CSV_TABLE_BYTES))? {
            Some((bytes, marked)) => {
                csv_left = csv_left.saturating_sub(bytes.len());
                if marked > 0 {
                    notes.csv_marked.push((number, marked));
                }
                csv.push((format!("tables/answer-table-{number}.csv"), bytes));
            }
            None => notes.csv_left_out.push(number),
        }
        let total = table.rows();
        let mut shown = total.min(TABLE_ROWS).min(cells_left / columns.len());
        if deck {
            shown = deck_rows(shown, SLIDES_PER_TABLE);
        }
        let (mut rows, mut long_cells, mut altered) = (Vec::with_capacity(shown), false, 0);
        for r in 0..shown {
            let (mut row, mut bytes) = (Vec::with_capacity(indices.len()), 0);
            for &i in &indices {
                let (raw, changed) = plain(table.cell(r, i).unwrap_or(""));
                altered += usize::from(changed);
                let text = if raw.chars().count() > CELL_CHARS {
                    long_cells = true;
                    cut(&raw, CELL_CHARS)
                } else {
                    raw
                };
                bytes += text.len();
                row.push(if workbook {
                    workbook_cell(&text, table.types[i])
                } else {
                    Value::String(text)
                });
            }
            if bytes > text_left {
                break;
            }
            text_left -= bytes;
            rows.push(row);
        }
        cells_left -= rows.len() * columns.len();
        if long_cells {
            notes.long_cells.push(number);
        }
        if altered > 0 {
            notes.altered_cells.push((number, altered));
        }
        let sha = request
            .sources
            .iter()
            .find(|s| s.reference == reference && s.table_path == path)
            .map_or("", |s| s.table_sha256.as_str());
        let (shown_rows, cells_cut) = push_table(
            &mut sections,
            deck,
            TableParts {
                heading: &format!("Table {number}: {}", file_name(path)),
                columns,
                rows,
                caption: entry["lead_in"].as_str().unwrap_or(""),
                reference: &format!("{path}, {total} rows, SHA-256 {sha}"),
            },
            SLIDES_PER_TABLE,
        );
        notes.slide_cells_cut |= cells_cut;
        if shown_rows < total {
            notes.rows_shown.push((number, shown_rows, total));
        }
    }

    // Each distinct figure is decoded once (its digest names its bytes); the figures placed stay within the writer's
    // byte and pixel budgets, counted for every figure placed, so a repeat counts twice here and once in the writer.
    let (mut figure_bytes, mut figure_pixels, mut figures_shown) = (0, 0u64, 0);
    let mut checked: HashMap<&str, Png> = HashMap::new();
    for (n, (figure, png)) in request.figures.iter().zip(inputs.figures).enumerate() {
        let check = *checked
            .entry(figure.sha256.as_str())
            .or_insert_with(|| document_write::figure_check(png));
        match check {
            Png::Fits(width, height)
                if png.len() <= FIGURE_BYTES
                    && figure_bytes + png.len() <= FIGURES_BYTES
                    && figure_pixels + u64::from(width) * u64::from(height)
                        <= document_write::MAX_FIGURES_PIXELS =>
            {
                figure_pixels += u64::from(width) * u64::from(height);
            }
            Png::Invalid => {
                notes.figures_unreadable.push(n + 1);
                continue;
            }
            _ => {
                notes.figures_too_large.push(n + 1);
                continue;
            }
        }
        figure_bytes += png.len();
        figures_shown += 1;
        let task = figure.reference.get(17..25).unwrap_or("");
        sections.push(Section {
            heading: format!("Figure {}", n + 1),
            block: Block::Figure {
                png: base64::engine::general_purpose::STANDARD.encode(png),
                caption: cut(
                    &format!("{} from result {task}", file_name(&figure.path)),
                    CAPTION_CHARS,
                ),
                reference: cut(
                    &format!("{} (SHA-256 {})", figure.path, figure.sha256),
                    CAPTION_CHARS,
                ),
            },
        });
    }

    if !request.sources.is_empty() {
        let rows = request
            .sources
            .iter()
            .map(|s| {
                vec![
                    json!(s.reference),
                    json!(s.table_path),
                    json!(s.rows),
                    json!(s.table_sha256),
                    json!(s.package_sha256),
                ]
            })
            .collect::<Vec<_>>();
        let (_, cells_cut) = push_table(
            &mut sections,
            deck,
            TableParts {
                heading: "Sources",
                columns: strings(&["Result", "File", "Rows", "Table SHA-256", "Package SHA-256"]),
                rows,
                caption: "The stored result tables this document copies from; each digest names their exact bytes.",
                reference: "The package's spec.json and SHA256SUMS list the same digests.",
            },
            usize::MAX,
        );
        notes.slide_cells_cut |= cells_cut;
    }

    // A deck past the section limit leaves out figures from the end first.
    while sections.len() > MAX_SECTIONS {
        let Some(at) = sections
            .iter()
            .rposition(|s| matches!(s.block, Block::Figure { .. }))
        else {
            return Err(Refusal("document_too_large"));
        };
        sections.remove(at);
        figures_shown -= 1;
        notes.figures_left_out += 1;
    }

    let mut other_text = false;
    for section in &mut sections {
        other_text |= plain_section(section);
    }
    if deck {
        notes.slide_text_cut |= fit_slide_text(&mut sections);
    }
    let mut provenance: Vec<(String, String)> = vec![
        ("Project".into(), request.project_title.clone()),
        ("Research run".into(), request.run.to_string()),
        (
            "Decision".into(),
            format!(
                "Turn {}, made {}",
                request.turn,
                rfc3339(request.decided_at)?
            ),
        ),
        ("Requested".into(), rfc3339(request.requested_at)?),
        (
            "Decision record SHA-256".into(),
            request.record_sha256.clone(),
        ),
        ("Request SHA-256".into(), request.request_sha256.clone()),
        (
            "Results used".into(),
            format!(
                "{} result tables and {figures_shown} figures, listed under Sources",
                request.sources.len()
            ),
        ),
        ("Values".into(), VALUES.to_owned()),
        (
            "Acceptance".into(),
            "No person has accepted this answer yet.".into(),
        ),
    ];
    for (_, value) in &mut provenance {
        let (out, changed) = plain(value);
        *value = out;
        other_text |= changed;
    }
    let (title, title_changed) = plain(if request.project_title.trim().is_empty() {
        "Research answer"
    } else {
        request.project_title.as_str()
    });
    notes.other_text = other_text || title_changed;
    let mut title = cut(&title, HEADING_CHARS);
    if deck && !slide_fit::title(&title) {
        title = fit_text(&title, slide_fit::title);
        notes.slide_text_cut = true;
    }
    let notes = notes.render(deck)?;
    for note in &notes {
        provenance.push(("Note".into(), note.clone()));
    }
    if provenance.len() > PROVENANCE_ENTRIES {
        return Err(Refusal("document_compose_failed"));
    }
    let counts = json!({"sections": sections.len(),
        "tables": sections.iter().filter(|s| matches!(s.block, Block::Table { .. })).count(),
        "cells": cells(&sections), "figures": figures_shown});
    let body = json!({
        "kind": kind,
        "title": title,
        "generated_at": rfc3339(request.requested_at)?,
        "sections": sections.iter().map(section_json).collect::<Vec<_>>(),
        "provenance": provenance.iter().map(|(label, value)| json!({"label": cut(label, CAPTION_CHARS),
            "value": cut(value, CAPTION_CHARS)})).collect::<Vec<_>>(),
    });
    let body = serde_json::to_vec(&body).map_err(|_| Refusal("document_compose_failed"))?;
    Ok(Composed {
        body,
        counts,
        csv,
        findings: whole,
        values,
        notes,
    })
}

fn rfc3339(at: time::OffsetDateTime) -> Result<String, Refusal> {
    at.to_offset(time::UtcOffset::UTC)
        .format(&Rfc3339)
        .map_err(|_| Refusal("document_compose_failed"))
}
