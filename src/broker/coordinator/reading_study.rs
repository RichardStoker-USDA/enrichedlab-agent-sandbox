//! Audited study fields and catalog links, read under the turn's existing paper and run permissions.
use super::*;
use knowledge::StudyRecord;

const LINKS_LABEL: &str = "Only reviewed links have been accepted by a lab curator; every other assertion is a machine or text-derived link that no person has accepted.";

impl Reading {
    pub(super) async fn study_record(
        &mut self,
        knowledge: Option<&KnowledgeService>,
        run: Uuid,
        lab: &str,
        corpus: Option<&str>,
        document: Uuid,
    ) -> std::result::Result<Option<StudyRecord>, &'static str> {
        if let std::collections::hash_map::Entry::Vacant(slot) = self.studies.entry(document) {
            let result = match (knowledge.filter(|k| k.configured(lab)), corpus) {
                (Some(service), Some(corpus)) => service
                    .study(run, lab, corpus, document)
                    .await
                    .map_err(knowledge::KnowledgeError::code),
                _ => Err("knowledge_not_configured"),
            };
            slot.insert(result);
        }
        self.studies[&document].clone()
    }

    pub(super) fn study_summary(
        &self,
        paper: &str,
        record: &StudyRecord,
        withheld: &mut Vec<Value>,
    ) -> Value {
        let reference = format!("library://{paper}/study");
        let mut fields = serde_json::Map::new();
        for field in knowledge::STUDY_FIELDS
            .iter()
            .filter(|f| self.study_fields.contains(**f))
        {
            fields.insert(
                (*field).to_owned(),
                json!(record.facts.iter().filter(|f| f.field == *field).count()),
            );
        }
        json!({"ref": reference,
            "label": screened(&cut(&record.label, 200), &reference, "label", withheld),
            "fields": fields, "links_label": LINKS_LABEL,
            "links": links(record, &reference, 12, withheld)})
    }

    pub(super) fn study_part(&self, paper: &str, record: &StudyRecord) -> Built {
        let reference = format!("library://{paper}/study");
        let mut withheld = Vec::new();
        let facts: Vec<Value> = record
            .facts
            .iter()
            .filter(|f| self.study_fields.contains(&f.field))
            .map(|f| {
                json!({"field": f.field,
                "value": screened(&f.value, &reference, &f.field, &mut withheld),
                "section": format!("library://{paper}/section/{}", f.section)})
            })
            .collect();
        let total = facts.len();
        let mut part = json!({"ref": reference, "part": "study",
            "label": screened(&cut(&record.label, 200), &reference, "label", &mut withheld),
            "extracted_at": screened(&cut(&record.provenance.extracted_at, 64), &reference, "extracted_at", &mut withheld),
            "facts": facts, "facts_omitted": 0, "links_label": LINKS_LABEL,
            "links": links(record, &reference, 24, &mut withheld), "links_omitted": 0});
        while size(&part) > self.part_max() {
            let facts = part["facts"].as_array_mut().unwrap();
            if facts.pop().is_some() {
                let omitted = total - facts.len();
                part["facts_omitted"] = json!(omitted);
            } else {
                let links = part["links"].as_array_mut().unwrap();
                if links.pop().is_none() {
                    break;
                }
                let omitted = record.links.len() - links.len();
                part["links_omitted"] = json!(omitted);
            }
        }
        let mut refs = vec![reference];
        for fact in part["facts"].as_array().into_iter().flatten() {
            let section = fact["section"].as_str().unwrap().to_owned();
            if !refs.contains(&section) {
                refs.push(section);
            }
        }
        Built {
            part,
            withheld,
            refs,
        }
    }
}

fn links(
    record: &StudyRecord,
    reference: &str,
    max: usize,
    withheld: &mut Vec<Value>,
) -> Vec<Value> {
    // knowledge-store study_links::links_in excludes rejected and retired relations before returning them.
    record.links.iter().take(max).map(|link| json!({
        "kind": screened(&cut(&link.kind, 32), reference, "link_kind", withheld),
        "assertion": screened(&link.assertion, reference, "link_assertion", withheld),
        "label": screened(&cut(&link.label, 100), reference, "link_label", withheld),
        "target": {"kind": screened(&cut(&link.target_kind, 32), reference, "target_kind", withheld),
            "title": screened(&cut(&link.target_name, 160), reference, "target_title", withheld),
            "ref": format!("knowledge://{}", link.target_id)}
    })).collect()
}
