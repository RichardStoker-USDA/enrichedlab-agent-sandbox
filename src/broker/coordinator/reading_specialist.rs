//! The literature specialist uses the same reader, with a separate window and shown refs.
use super::*;

impl Reading {
    pub(in super::super) fn for_review(
        lab_text: &LabText,
        lab: &str,
        results: &Value,
        context: &mut Value,
    ) -> Option<Self> {
        let settings = LabText {
            reading: lab_text.reading.clone(),
            reading_bytes: lab_text.review_bytes,
            unreviewed: lab_text.unreviewed.clone(),
            study_fields: lab_text.study_fields.clone(),
            ..LabText::default()
        };
        Self::offer(&settings, lab, results, context)
    }

    pub(in super::super) fn held_parts(&self) -> &[Value] {
        &self.parts
    }

    /// Summaries offer study refs but are not read parts. Membership and acceptance are checked as for a read round.
    #[allow(clippy::too_many_arguments)]
    pub(in super::super) async fn study_summaries(
        &mut self,
        store: &Store,
        library: Option<&LibraryService>,
        knowledge: Option<&KnowledgeService>,
        run: Uuid,
        worker: &str,
        generation: i64,
        lab: &str,
    ) -> Result<(Value, Vec<Value>)> {
        if self.study_fields.is_empty() {
            return Ok((json!([]), Vec::new()));
        }
        let mut papers: Vec<_> = self
            .papers
            .iter()
            .map(|(p, s)| (p.clone(), s.document))
            .collect();
        papers.sort();
        let docs: Vec<_> = papers.iter().map(|(_, d)| *d).collect();
        let held = store
            .research_run_library(run, worker, generation, &docs)
            .await?;
        let mut summaries = Vec::new();
        let mut withheld = Vec::new();
        for (paper, document) in papers {
            if held["member"] != true
                || !held["papers"].as_array().into_iter().flatten().any(|p| {
                    p["paper_id"].as_str() == Some(&paper)
                        && p["document_id"] == document.to_string()
                        && (!self.accepted_only || p["accepted"] == true)
                })
            {
                continue;
            }
            if let Ok(Some(record)) = self
                .study_record(
                    knowledge,
                    run,
                    lab,
                    library.and_then(|l| l.corpus(lab)),
                    document,
                )
                .await
            {
                let summary = self.study_summary(&paper, &record, &mut withheld);
                self.shown.insert(format!("library://{paper}/study"));
                summaries.push(summary);
            }
        }
        Ok((Value::Array(summaries), withheld))
    }
}
