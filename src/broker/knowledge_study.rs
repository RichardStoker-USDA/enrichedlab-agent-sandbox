//! The study fields and link assertions the knowledge service serves; quotes and extraction calls stay there.
use super::{KnowledgeError, StatusCode};
use serde::Deserialize;
use serde_json::Value;
use uuid::Uuid;

/// Knowledge 0.18.0, docs/study_records.md, "A fact" (knowledge-studies' field list).
pub const STUDY_FIELDS: &[&str] = &[
    "aim",
    "design",
    "organism",
    "population",
    "sample_size",
    "group",
    "exposure",
    "dose",
    "duration",
    "outcome",
    "measure",
    "specimen",
    "assay",
    "instrument",
    "data_type",
    "data_location",
    "statistical_method",
    "software",
    "code_location",
    "result",
    "limitation",
];

#[derive(Clone, Deserialize)]
pub struct StudyFact {
    pub field: String,
    pub value: String,
    pub section: String,
}

#[derive(Clone, Deserialize)]
pub struct StudyLink {
    pub kind: String,
    pub assertion: String,
    pub label: String,
    pub target_id: Uuid,
    pub target_kind: String,
    pub target_name: String,
}

#[derive(Clone, Deserialize)]
pub struct StudyProvenance {
    pub extracted_at: String,
}

#[derive(Clone, Deserialize)]
pub struct StudyRecord {
    pub label: String,
    pub provenance: StudyProvenance,
    pub facts: Vec<StudyFact>,
    #[serde(default)]
    pub links: Vec<StudyLink>,
}

/// Unknown payload fields (including quotes) are never retained. A 0.17.0 record has no links.
pub fn parse_study(value: &Value) -> Result<StudyRecord, KnowledgeError> {
    let record: StudyRecord =
        serde_json::from_value(value.clone()).map_err(|_| KnowledgeError::Invalid)?;
    if record.facts.len() > 300
        || record.links.len() > 24
        || record.facts.iter().any(|f| {
            !STUDY_FIELDS.contains(&f.field.as_str())
                || !crate::broker::paper::section_key_valid(&f.section)
        })
        || record.links.iter().any(|link| {
            link.assertion.is_empty()
                || link.assertion.len() > 64
                || link.assertion.chars().any(char::is_control)
        })
    {
        return Err(KnowledgeError::Invalid);
    }
    Ok(record)
}

pub(super) fn answer(
    status: StatusCode,
    body: &[u8],
) -> Result<Option<StudyRecord>, KnowledgeError> {
    match status {
        StatusCode::OK => {
            let value = serde_json::from_slice(body).map_err(|_| KnowledgeError::Invalid)?;
            parse_study(&value).map(Some)
        }
        StatusCode::NOT_FOUND => {
            let value: Value = serde_json::from_slice(body).map_err(|_| KnowledgeError::Invalid)?;
            if value["error"]["code"] == "study_record_not_found" {
                Ok(None)
            } else {
                Err(KnowledgeError::Invalid)
            }
        }
        StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN => Err(KnowledgeError::Refused),
        _ => Err(KnowledgeError::Unavailable),
    }
}
