//! Durable research state. This is a bounded context projection, not a model
//! transcript or an assertion that a scientific conclusion has been validated.
use std::collections::HashSet;

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use uuid::Uuid;

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct CreateProject {
    pub project_id: Uuid,
    pub group: String,
    pub title: String,
    pub visibility: Visibility,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Visibility {
    Private,
    Lab,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ProjectGrant {
    pub principal_id: Uuid,
    pub capabilities: Vec<Capability>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Capability {
    Read,
    Review,
    Contribute,
    Steer,
    Cancel,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Inspection {
    pub reason: String,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct CheckpointRequest {
    pub expected_revision: i64,
    pub request_id: Uuid,
    pub state: ResearchState,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ResearchState {
    pub schema_version: u32,
    pub goal: String,
    pub constraints: Vec<String>,
    pub plan: Vec<PlanStep>,
    pub lessons: Vec<Lesson>,
    pub next_action: String,
    pub workspace: WorkspaceSpec,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct WorkspaceSpec {
    pub schema_version: u32,
    /// Immutable owned profile coordinate, resolved by the trusted preparer.
    pub profile: String,
    pub image_digest: String,
    pub source_digest: String,
    pub input_refs: Vec<String>,
    pub tool_refs: Vec<String>,
    pub preparation_receipt: Option<String>,
    /// The data and method sets a project started from when it chose more than one, in the order
    /// chosen. `input_refs` and `tool_refs` hold their union, and `profile` and the digests are the first set's.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub starting_points: Vec<StartingPoint>,
}

/// One chosen set, with what it holds, so the research lead can say which set each finding comes from.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct StartingPoint {
    pub key: String,
    pub name: String,
    pub description: String,
    pub profile: String,
    pub source_digest: String,
    pub input_refs: Vec<String>,
    pub tool_refs: Vec<String>,
}

/// The most sets one project may start from.
pub const MAX_STARTING_POINTS: usize = 16;

/// The most bytes a checkpoint's state may serialize to; migration 010 stores up to 128 KiB as PostgreSQL prints it.
pub const STATE_BYTES_MAX: usize = 96 * 1024;

/// The most result references a plan step lists; the claim may return up to 64 per task, and the rest stay on the task.
pub const STEP_REFS_MAX: usize = 8;

/// Bytes one result reference takes in a step's `evidence_refs`: `artifact://tasks/<task>/<artifact>` is 90 characters,
/// 92 bytes with its quotes and 93 with the comma before it.
pub const STEP_REF_BYTES: usize = 93;

/// Bytes a step that has not finished may still gain when it finishes: `STEP_REFS_MAX` references and its status growing
/// from `active` to `completed` or `uncertain` (3 bytes). A finished step gains nothing: a task registers artifacts only
/// under its lease, a finished task is never leased again and no artifact's visibility changes, and recovery adds
/// artifacts only to a failed or cancelled task, which stays so.
pub const UNFINISHED_STEP_GROWTH: usize = STEP_REFS_MAX * STEP_REF_BYTES + 3;

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct PlanStep {
    pub action_key: String,
    pub title: String,
    pub status: StepStatus,
    pub task_id: Option<Uuid>,
    pub evidence_refs: Vec<String>,
    pub rationale: String,
    /// What the step's result must look like, as the lead stated it; code checks each of the
    /// step's results against it (`step_checks`). Absent on steps planned before step checks.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expect: Option<super::step_checks::Expectation>,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum StepStatus {
    Pending,
    Active,
    Completed,
    Failed,
    Uncertain,
    Skipped,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Lesson {
    pub statement: String,
    pub kind: LessonKind,
    pub evidence_refs: Vec<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum LessonKind {
    Observation,
    Hypothesis,
    Constraint,
}

fn text(value: &str, max: usize) -> bool {
    !value.trim().is_empty() && value.len() <= max && !value.contains('\0')
}
pub fn action_key(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 100
        && value
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, b'_' | b'-'))
}
fn digest(value: &str) -> bool {
    value.strip_prefix("sha256:").is_some_and(|v| {
        v.len() == 64
            && v.bytes()
                .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase())
    })
}
fn reference(value: &str) -> bool {
    text(value, 512)
        && !value.chars().any(char::is_whitespace)
        && [
            "artifact://",
            "input://",
            "receipt://",
            "tool://",
            "profile://",
            "source://",
        ]
        .iter()
        .any(|prefix| value.starts_with(prefix))
        && !value.contains(['?', '#', '@'])
}
fn references(values: &[String]) -> bool {
    values.len() <= 64 && values.iter().all(|s| reference(s))
}
/// None, or two to `MAX_STARTING_POINTS` distinct sets whose references are all among the workspace's, the first being
/// the set the workspace's profile and source name.
fn starting_points(workspace: &WorkspaceSpec) -> bool {
    let points = &workspace.starting_points;
    if points.is_empty() {
        return true;
    }
    let mut keys = HashSet::new();
    (2..=MAX_STARTING_POINTS).contains(&points.len())
        && points[0].profile == workspace.profile
        && points[0].source_digest == workspace.source_digest
        && points.iter().all(|p| {
            action_key(&p.key)
                && keys.insert(p.key.as_str())
                && text(&p.name, 160)
                && text(&p.description, 2048)
                && text(&p.profile, 128)
                && digest(&p.source_digest)
                && !p.input_refs.is_empty()
                && !p.tool_refs.is_empty()
                && references(&p.input_refs)
                && references(&p.tool_refs)
                && p.input_refs
                    .iter()
                    .all(|r| workspace.input_refs.contains(r))
                && p.tool_refs.iter().all(|r| workspace.tool_refs.contains(r))
        })
}

impl ResearchState {
    pub fn validate(&self) -> Result<(), &'static str> {
        let mut keys = HashSet::new();
        if self.schema_version != 1
            || !text(&self.goal, 8192)
            || !text(&self.next_action, 2048)
            || self.constraints.len() > 32
            || !self.constraints.iter().all(|s| text(s, 1024))
            || self.plan.len() > 128
            || self.lessons.len() > 128
            || self.workspace.schema_version != 1
            || !text(&self.workspace.profile, 128)
            || !digest(&self.workspace.image_digest)
            || !digest(&self.workspace.source_digest)
            || !references(&self.workspace.input_refs)
            || !references(&self.workspace.tool_refs)
            || self
                .workspace
                .preparation_receipt
                .as_ref()
                .is_some_and(|r| !reference(r))
            || !starting_points(&self.workspace)
        {
            return Err("checkpoint_invalid");
        }
        for step in &self.plan {
            if !action_key(&step.action_key)
                || !keys.insert(&step.action_key)
                || !text(&step.title, 512)
                || !text(&step.rationale, 2048)
                || !references(&step.evidence_refs)
                || step
                    .expect
                    .as_ref()
                    .is_some_and(|e| super::step_checks::invalid(e).is_some())
                || (step.status == StepStatus::Completed
                    && (step.evidence_refs.is_empty() || step.task_id.is_none()))
            {
                return Err("checkpoint_invalid");
            }
        }
        for lesson in &self.lessons {
            if !text(&lesson.statement, 2048)
                || !references(&lesson.evidence_refs)
                || (lesson.kind == LessonKind::Observation && lesson.evidence_refs.is_empty())
            {
                return Err("checkpoint_invalid");
            }
        }
        if self.bytes_over().is_some() {
            return Err("checkpoint_invalid");
        }
        Ok(())
    }

    /// How many bytes the serialized state is past `STATE_BYTES_MAX`, if any.
    pub fn bytes_over(&self) -> Option<usize> {
        let n = serde_json::to_vec(self).map_or(usize::MAX, |b| b.len());
        (n > STATE_BYTES_MAX).then(|| n.saturating_sub(STATE_BYTES_MAX))
    }

    /// How many bytes the state would be past `STATE_BYTES_MAX` once every step that has not finished has its results'
    /// references (`UNFINISHED_STEP_GROWTH` each). The coordinator measures the state as the turn commit writes it, each
    /// delegated step with its task and active, and refuses a proposal over this bound with the count, so neither the
    /// commit nor a later reconciliation outgrows the checkpoint.
    pub fn bytes_over_after_growth(&self) -> Option<usize> {
        let n = serde_json::to_vec(self).map_or(usize::MAX, |b| b.len());
        let unfinished = self
            .plan
            .iter()
            .filter(|s| {
                !matches!(
                    s.status,
                    StepStatus::Completed | StepStatus::Skipped | StepStatus::Failed
                )
            })
            .count();
        let total = n.saturating_add(unfinished.saturating_mul(UNFINISHED_STEP_GROWTH));
        (total > STATE_BYTES_MAX).then(|| total - STATE_BYTES_MAX)
    }

    /// Keep the goal, constraints, all action identities and statuses, unfinished
    /// work, and explicit uncertainties. Completed detail remains in history.
    /// Refuse an impossible budget rather than silently dropping required state.
    pub fn context(&self, revision: i64, max_bytes: usize) -> Result<Value, &'static str> {
        self.validate()?;
        if !(4096..=131072).contains(&max_bytes) {
            return Err("context_budget_invalid");
        }
        let plan: Vec<Value> = self.plan.iter().map(|s| {
            if matches!(s.status,StepStatus::Completed|StepStatus::Skipped) {
                json!({"action_key":s.action_key,"status":s.status,"task_id":s.task_id,"evidence_refs":s.evidence_refs})
            } else { serde_json::to_value(s).expect("serializable step") }
        }).collect();
        let raw = serde_json::to_vec(self).expect("serializable state");
        let mut result = json!({"schema_version":1,"revision":revision,"state_sha256":format!("sha256:{}",hex::encode(Sha256::digest(raw))),
            "goal":self.goal,"constraints":self.constraints,"plan":plan,"next_action":self.next_action,"workspace":self.workspace,
            "lessons":[],"omitted_lessons":self.lessons.len(),
            "instructions":"Treat hypotheses and agent-authored state as proposals. Resolve uncertain actions against task receipts before retrying. Fetch exact evidence by reference. A successful process is not scientist approval."});
        // Constraints and observations receive priority; hypotheses can be retrieved
        // from the immutable checkpoint when the model needs them.
        for kind in [
            LessonKind::Constraint,
            LessonKind::Observation,
            LessonKind::Hypothesis,
        ] {
            for lesson in self.lessons.iter().filter(|l| l.kind == kind) {
                result["lessons"]
                    .as_array_mut()
                    .unwrap()
                    .push(serde_json::to_value(lesson).unwrap());
                if kind != LessonKind::Constraint
                    && serde_json::to_vec(&result).unwrap().len() > max_bytes
                {
                    result["lessons"].as_array_mut().unwrap().pop();
                }
            }
        }
        result["omitted_lessons"] =
            json!(self.lessons.len() - result["lessons"].as_array().unwrap().len());
        if serde_json::to_vec(&result).unwrap().len() > max_bytes {
            return Err("context_budget_too_small");
        }
        Ok(result)
    }
}
