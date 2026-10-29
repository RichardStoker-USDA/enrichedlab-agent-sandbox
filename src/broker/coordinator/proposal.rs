//! The lead's proposal: answer, findings, table statements, answer tables and parts, with their limits.
use super::*;

/// The lead's reading of whether the recorded results answer the question. It is the agent's
/// reading, never a validation.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Answer {
    Answered,
    PartlyAnswered,
    NotAnswered,
    #[default]
    NotYet,
}

/// One cell of a result table the lead was shown.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Binding {
    #[serde(rename = "ref")]
    pub reference: String,
    pub table: String,
    pub row: usize,
    pub column: String,
}

/// A plain sentence whose result values are placeholders such as `{a}`, each bound to one cell.
/// The workspace fills them from the stored digest; the lead never writes the values.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Finding {
    pub statement: String,
    pub bindings: BTreeMap<String, Binding>,
    /// The table statement code wrote this finding from, counted from 1; absent on a finding the
    /// lead wrote. It is never read from a reply, so a reply that names it is refused as an unknown field.
    #[serde(default, skip_deserializing, skip_serializing_if = "Option::is_none")]
    pub composed_from: Option<usize>,
}

/// Which rows of its table a table statement states: their `#` numbers, or the word `all`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum StatedRows {
    Numbers(Vec<usize>),
    Word(String),
}

/// Rows of one shown table that the lead asks code to state. The lead names the table, its rows and
/// columns and writes only the opening words; code writes each sentence from fixed wording and binds every value, so
/// neither a value nor a placeholder comes from the model.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TableStatement {
    #[serde(rename = "ref")]
    pub reference: String,
    pub table: String,
    pub rows: StatedRows,
    pub columns: Vec<String>,
    pub lead_in: String,
}

/// A whole result table the lead gives as part of its answer: a table its call was shown, with or without
/// the table's cells, the columns to show and the plain words that open it. Code checks the table and writes its row
/// count from the shown digest; the workspace shows every row from the retained result, so no value passes through
/// the lead. Two suite goals failed because their answers were lists longer than table statements
/// can state: one spent its twelve turns paging through a list, and the other stated the first groups only.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AnswerTable {
    #[serde(rename = "ref")]
    pub reference: String,
    pub table: String,
    pub columns: Vec<String>,
    pub lead_in: String,
    /// The table's rows as its digest counts them, written by `check_tables`. A lead reply that gives it is refused as
    /// an unknown field (`ANSWER_TABLE_FIELDS`), so the count is always code's.
    #[serde(default, skip_deserializing, skip_serializing_if = "Option::is_none")]
    pub rows: Option<u64>,
}

/// One part of the scientist's question and what the recorded results say about it.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Part {
    /// The part, in the question's own words.
    pub part: String,
    pub state: PartState,
    /// The numbers (1 for the first) of the lead's own findings that state the part, when it is supported or partial.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub findings: Vec<usize>,
    /// The numbers of the table statements that state it.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub table_statements: Vec<usize>,
    /// The numbers of the answer tables that state it.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tables: Vec<usize>,
    /// Why the data cannot hold an unsupported part, what it does not hold of a partial one, or the choice an awaiting
    /// one waits for.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum PartState {
    Supported,
    /// The data holds the part for some of the items it asks about, not all (conditionally absent).
    Partial,
    Unsupported,
    /// Not settled yet: only in a reply that proposes a step.
    Open,
    /// Waiting for a choice only a person can make, named by its reason; a reply may stop on it.
    Awaiting,
}

/// A ledger holds at most six parts, each of at most 300 characters, and a reason at most 600: every reason the
/// suites refused at 400 held 402 to 492 characters, one refused four times running.
pub(super) const PARTS_MAX: usize = 6;

pub(super) const PART_CHARS: usize = 300;

pub(super) const PART_REASON_CHARS: usize = 600;

/// At most this many ledger faults reach the next lead call.
pub(super) const PARTS_NAMES_MAX: usize = 8;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Proposal {
    pub action: String,
    #[serde(default)]
    pub answer: Answer,
    pub summary: String,
    #[serde(default)]
    pub findings: Vec<Finding>,
    /// Tables the lead asks code to state row by row; their findings follow the lead's own in `findings`. A record
    /// without any keeps the shape it had before findings named their rows.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub table_statements: Vec<TableStatement>,
    /// Whole result tables the lead gives as part of its answer. A record without any keeps its shape.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tables: Vec<AnswerTable>,
    /// An absent list reads as empty: a test run's lead left out `lessons` in five of six replies, and each refusal
    /// cost a lead call. A delegation still needs actions, which `validate` checks.
    #[serde(default)]
    pub actions: Vec<ProposedAction>,
    #[serde(default)]
    pub lessons: Vec<Lesson>,
    /// The answerability ledger: required in every reply but a delegation that answers `not_yet`. A
    /// record without one keeps the shape it had before.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub parts: Vec<Part>,
}

/// The lead writes at most eight findings of at most 600 characters and eight values each; with the findings its table
/// statements make, a decision holds at most sixteen. Size is what the calls bound: a proposal whose methods review
/// would pass the input ceiling is refused as `proposal_too_large`, and the next call's earlier findings stop at
/// `EARLIER_FINDINGS_BYTES` (in one suite, eight in all refused six replies).
pub(super) const FINDINGS_MAX: usize = 8;

pub(super) const ALL_FINDINGS_MAX: usize = 16;

pub(super) const EARLIER_FINDINGS_BYTES: usize = 16_384;

pub(super) const FINDING_CHARS_MAX: usize = 600;

pub(super) const FINDING_VALUES_MAX: usize = 8;

/// A reply holds at most four table statements, each of at most four columns and twelve rows, opening with at most
/// 300 characters.
pub(super) const TABLE_STATEMENTS_MAX: usize = 4;

pub(super) const STATED_COLUMNS_MAX: usize = 4;

pub(super) const STATED_ROWS_MAX: usize = 12;

pub(super) const LEAD_IN_MAX: usize = 300;

/// A reply gives at most four answer tables, each showing one to six of its table's columns.
pub(super) const ANSWER_TABLES_MAX: usize = 4;

pub(super) const ANSWER_COLUMNS_MAX: usize = 6;
