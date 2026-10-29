//! Paper parts, screened and fitted for one reading turn.
use super::*;

impl Reading {
    #[allow(clippy::too_many_arguments)]
    pub(super) async fn parts_for(
        &mut self,
        service: &LibraryService,
        knowledge: Option<&KnowledgeService>,
        run: Uuid,
        lab: &str,
        paper_id: &str,
        document: Uuid,
        parsed: &Paper,
        reference: &str,
        target: Target,
    ) -> std::result::Result<Vec<Built>, &'static str> {
        let record =
            if !self.study_fields.is_empty() && matches!(target, Target::Outline | Target::Study) {
                self.study_record(knowledge, run, lab, service.corpus(lab), document)
                    .await
            } else {
                Ok(None)
            };
        match target {
            Target::Outline => {
                let mut built = self.outline(
                    paper_id,
                    parsed,
                    record.as_ref().ok().and_then(|r| r.as_ref()),
                );
                if let Err(code) = record {
                    built.part["study_status"] = json!(code);
                }
                Ok(vec![built])
            }
            Target::Study => {
                let record = record?.ok_or("study_record_not_found")?;
                Ok(vec![self.study_part(paper_id, &record)])
            }
            Target::Role(role) => {
                let parts: Vec<Built> = parsed
                    .sections
                    .iter()
                    .filter(|s| s.role.as_deref() == Some(role.as_str()))
                    .map(|s| self.section(paper_id, parsed, s, 0))
                    .collect();
                if parts.is_empty() {
                    return Err("read_role_absent");
                }
                Ok(parts)
            }
            Target::Section { key, from } => {
                let section = parsed
                    .sections
                    .iter()
                    .find(|s| s.key == key)
                    .filter(|s| from < s.texts.len())
                    .ok_or("read_section_absent")?;
                Ok(vec![self.section(paper_id, parsed, section, from)])
            }
            Target::Passage => {
                let place = self.papers[paper_id]
                    .passages
                    .get(reference)
                    .cloned()
                    .ok_or("read_ref_not_shown")?;
                let section = match &place.key {
                    Some(key) => parsed.sections.iter().find(|s| &s.key == key),
                    None => parsed.sections.iter().find(|s| s.path == place.headings),
                }
                .ok_or("read_section_absent")?;
                Ok(vec![self.section(paper_id, parsed, section, 0)])
            }
            Target::Figure(n) => {
                let figure = parsed.figures.get(n - 1).ok_or("read_figure_absent")?;
                Ok(vec![figure_part(paper_id, n, parsed, figure)])
            }
            Target::Table(n) => {
                let table = parsed.tables.get(n - 1).ok_or("read_table_absent")?;
                let grid = service
                    .table(lab, document, table.ordinal)
                    .await
                    .map_err(LibraryError::code)?;
                Ok(vec![self.table_part(paper_id, n, parsed, table, &grid)])
            }
        }
    }

    /// The largest one part may be, so at least two fit.
    pub(super) fn part_max(&self) -> usize {
        (self.bytes / 2).max(4096)
    }

    /// A paper's outline; once a call holds it, every section, figure and table it lists is a ref the lead may read.
    pub(super) fn outline(
        &self,
        paper_id: &str,
        parsed: &Paper,
        record: Option<&knowledge::StudyRecord>,
    ) -> Built {
        let reference = format!("library://{paper_id}");
        let mut withheld = Vec::new();
        let mut refs = Vec::new();
        let sections: Vec<Value> = parsed
            .sections
            .iter()
            .take(OUTLINE_PARTS_MAX)
            .map(|s| {
                let r = format!("{reference}/section/{}", s.key);
                refs.push(r.clone());
                json!({"ref": r, "heading": screened(&s.heading(), &r, "heading", &mut withheld),
                    "role": s.role, "pages": s.pages(), "chars": s.chars()})
            })
            .collect();
        let figures: Vec<Value> = parsed
            .figures
            .iter()
            .take(OUTLINE_PARTS_MAX)
            .enumerate()
            .map(|(i, f)| {
                let r = format!("{reference}/figure/{}", i + 1);
                refs.push(r.clone());
                json!({"ref": r, "label": f.label.as_deref().map(|l| screened(&cut(l, 64), &r, "label", &mut withheld)),
                    "caption": f.caption.as_deref().map(|c| screened(&cut(c, OUTLINE_CAPTION_CHARS), &r, "caption", &mut withheld)),
                    "page": f.page})
            })
            .collect();
        let tables: Vec<Value> = parsed
            .tables
            .iter()
            .take(OUTLINE_PARTS_MAX)
            .enumerate()
            .map(|(i, t)| {
                let r = format!("{reference}/table/{}", i + 1);
                refs.push(r.clone());
                json!({"ref": r, "label": t.label.as_deref().map(|l| screened(&cut(l, 64), &r, "label", &mut withheld)),
                    "caption": t.caption.as_deref().map(|c| screened(&cut(c, OUTLINE_CAPTION_CHARS), &r, "caption", &mut withheld)),
                    "page": t.page})
            })
            .collect();
        let mut part = json!({"ref": reference, "part": "outline",
            "paper": title(parsed, &reference, &mut withheld), "year": parsed.year,
            "journal": parsed.journal.as_deref().map(|j| screened(j, &reference, "journal", &mut withheld)),
            "authors": parsed.authors.iter().take(AUTHORS_MAX).map(|a| screened(a, &reference, "author", &mut withheld)).collect::<Vec<_>>(),
            "authors_total": parsed.authors.len(),
            "abstract": parsed.abstract_text.as_deref().map(|a| screened(&cut(a, ABSTRACT_CHARS), &reference, "abstract", &mut withheld)),
            "sections": sections, "figures": figures, "tables": tables,
            "sections_total": parsed.sections.len(), "figures_total": parsed.figures_total,
            "tables_total": parsed.tables_total});
        if !self.study_fields.is_empty()
            && let Some(record) = record
        {
            part["study"] = self.study_summary(paper_id, record, &mut withheld);
            refs.push(format!("{reference}/study"));
        }
        if !parsed.complete() {
            part["complete"] = json!(false);
        }
        Built {
            part,
            withheld,
            refs,
        }
    }

    /// A section's text from paragraph `from`, as much as one part holds; when it stops early, its continuation ref.
    pub(super) fn section(
        &self,
        paper_id: &str,
        parsed: &Paper,
        section: &Section,
        from: usize,
    ) -> Built {
        let base = format!("library://{paper_id}/section/{}", section.key);
        let reference = if from == 0 {
            base.clone()
        } else {
            format!("{base}@{from}")
        };
        let max = self.part_max().saturating_sub(1024);
        let mut withheld = Vec::new();
        let mut text = String::new();
        let mut next = None;
        for (i, paragraph) in section.texts.iter().enumerate().skip(from) {
            let shown = match screen::screen(paragraph) {
                Some(reason) => {
                    withheld.push(json!({"ref": reference, "paragraph": i, "reason": reason,
                        "text_sha256": sha(paragraph.as_bytes())}));
                    "[withheld]"
                }
                None => paragraph.as_str(),
            };
            if text.len() + shown.len() + 2 > max {
                if text.is_empty() {
                    // One paragraph larger than a part: its start, then the next paragraph.
                    text = cut_bytes(shown, max);
                    next = (i + 1 < section.texts.len()).then_some(i + 1);
                } else {
                    next = Some(i);
                }
                break;
            }
            if !text.is_empty() {
                text.push_str("\n\n");
            }
            text.push_str(shown);
        }
        let mut part = json!({"ref": reference, "part": "section",
            "paper": title(parsed, &reference, &mut withheld),
            "heading": screened(&section.heading(), &reference, "heading", &mut withheld),
            "role": section.role, "pages": section.pages(), "text": text});
        if section.cut > 0 {
            part["paragraphs_cut_by_parser_api"] = json!(section.cut);
        }
        let mut refs = Vec::new();
        if let Some(n) = next {
            let r = format!("{base}@{n}");
            refs.push(r.clone());
            part["next"] = json!(r);
        }
        Built {
            part,
            withheld,
            refs,
        }
    }

    /// A table's grid, as many rows as one part holds, each cell screened.
    pub(super) fn table_part(
        &self,
        paper_id: &str,
        n: usize,
        parsed: &Paper,
        table: &paper::Table,
        grid: &Value,
    ) -> Built {
        let reference = format!("library://{paper_id}/table/{n}");
        let mut withheld = Vec::new();
        let header = screened_value(&grid["header"], &reference, &mut withheld);
        let all = grid["rows"].as_array().cloned().unwrap_or_default();
        let mut part = json!({"ref": reference, "part": "table",
            "paper": title(parsed, &reference, &mut withheld),
            "label": table.label.as_deref().map(|l| screened(&cut(l, 64), &reference, "label", &mut withheld)),
            "caption": table.caption.as_deref().map(|c| screened(&cut(c, FIGURE_TEXT_CHARS), &reference, "caption", &mut withheld)),
            "page": table.page, "header": header, "rows": [], "rows_total": all.len()});
        let max = self.part_max();
        let mut rows = Vec::new();
        let mut used = size(&part);
        for row in &all {
            let row = screened_value(row, &reference, &mut withheld);
            let n = size(&row) + 1;
            if used + n > max {
                break;
            }
            used += n;
            rows.push(row);
        }
        part["rows_shown"] = json!(rows.len());
        part["rows"] = Value::Array(rows);
        Built {
            part,
            withheld,
            refs: Vec::new(),
        }
    }
}

pub(super) fn figure_part(
    paper_id: &str,
    n: usize,
    parsed: &Paper,
    figure: &paper::Figure,
) -> Built {
    let reference = format!("library://{paper_id}/figure/{n}");
    let mut withheld = Vec::new();
    let mut part = json!({"ref": reference, "part": "figure",
        "paper": title(parsed, &reference, &mut withheld),
        "label": figure.label.as_deref().map(|l| screened(&cut(l, 64), &reference, "label", &mut withheld)),
        "caption": figure.caption.as_deref().map(|c| screened(&cut(c, FIGURE_TEXT_CHARS), &reference, "caption", &mut withheld)),
        "page": figure.page});
    if let Some(description) = figure
        .description
        .as_deref()
        .filter(|d| !d.trim().is_empty())
    {
        part["description"] = json!(screened(
            &cut(description, FIGURE_TEXT_CHARS),
            &reference,
            "description",
            &mut withheld
        ));
        part["description_source"] = json!("model");
    }
    Built {
        part,
        withheld,
        refs: Vec::new(),
    }
}

pub(super) fn title(parsed: &Paper, reference: &str, withheld: &mut Vec<Value>) -> Option<String> {
    parsed
        .title
        .as_deref()
        .map(|t| screened(&cut(t, 300), reference, "title", withheld))
}

/// The text, or `[withheld]` with a record of the field and the screen's reason when it reads like instructions.
pub(super) fn screened(
    text: &str,
    reference: &str,
    field: &str,
    withheld: &mut Vec<Value>,
) -> String {
    match screen::screen(text) {
        Some(reason) => {
            withheld.push(json!({"ref": reference, "field": field, "reason": reason,
                "text_sha256": sha(text.as_bytes())}));
            "[withheld]".into()
        }
        None => text.to_owned(),
    }
}

/// Every string in a table value screened as a cell.
fn screened_value(value: &Value, reference: &str, withheld: &mut Vec<Value>) -> Value {
    match value {
        Value::String(text) => json!(screened(text, reference, "cell", withheld)),
        Value::Array(items) => Value::Array(
            items
                .iter()
                .map(|v| screened_value(v, reference, withheld))
                .collect(),
        ),
        Value::Object(fields) => Value::Object(
            fields
                .iter()
                .map(|(k, v)| (k.clone(), screened_value(v, reference, withheld)))
                .collect(),
        ),
        other => other.clone(),
    }
}
