//! Research studies, workers, results, seals and the specs that create them.
use super::*;

#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize, PartialEq, Eq, ValueEnum)]
#[serde(rename_all = "kebab-case")]
pub enum ResearchMode {
    #[default]
    Deterministic,
    Exploratory,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum ResearchWorkerState {
    Prepared,
    Running,
    Completed,
    Failed,
    Sealed,
    Stopped,
    Deleted,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct SnapshotEntry {
    pub path: String,
    pub size: u64,
    pub sha256: String,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct SnapshotManifest {
    pub schema_version: u32,
    pub files: Vec<SnapshotEntry>,
    pub total_bytes: u64,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ResearchTool {
    pub name: String,
    pub kind: String,
    pub sha256: String,
    pub file_count: u64,
    pub sandbox_path: String,
    pub entrypoint: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ResearchWorker {
    pub name: String,
    pub sandbox: String,
    pub workspace: PathBuf,
    pub output: PathBuf,
    pub perspective_path: PathBuf,
    pub perspective_sha256: String,
    pub assignment_sha256: String,
    pub route: String,
    pub route_context_limit: u64,
    pub route_output_limit: u64,
    pub route_pool_cap: usize,
    pub gateway_profile: String,
    pub thinking: Option<Thinking>,
    #[serde(default)]
    pub harness: HarnessKind,
    pub tools: Vec<ResearchTool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub state_run_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub state_profile: Option<String>,
    #[serde(default)]
    pub segments_completed: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_segment_at: Option<String>,
    pub state: ResearchWorkerState,
    pub created_at: String,
    pub started_at: Option<String>,
    pub finished_at: Option<String>,
    pub sealed_at: Option<String>,
    pub result_sha256: Option<String>,
    pub output_sha256: Option<String>,
    pub last_result_class: Option<String>,
    pub sandbox_deleted: bool,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ResearchComparison {
    pub schema_version: u32,
    pub study: String,
    pub compared_at: String,
    pub sealed_workers: usize,
    pub answer_groups: BTreeMap<String, Vec<String>>,
    pub all_answers_agree: bool,
    pub route_diversity: usize,
    pub method_files_present: bool,
    pub scientific_validation: String,
    pub external_validation_required: bool,
    pub sha256: String,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ResearchStudy {
    pub schema_version: u32,
    pub name: String,
    pub mode: ResearchMode,
    pub root: PathBuf,
    pub question_path: PathBuf,
    pub question_sha256: String,
    pub protocol_path: PathBuf,
    pub protocol_sha256: String,
    pub input_manifest_path: PathBuf,
    pub input_manifest_sha256: String,
    pub input_file_count: u64,
    pub input_bytes: u64,
    pub max_output_files: u64,
    pub max_output_bytes: u64,
    pub created_at: String,
    pub workers: BTreeMap<String, ResearchWorker>,
    pub comparison: Option<ResearchComparison>,
}

#[derive(Clone, Debug, Serialize)]
pub struct ResearchWorkerSummary {
    pub name: String,
    pub sandbox: String,
    pub route: String,
    pub route_context_limit: u64,
    pub route_output_limit: u64,
    pub route_pool_cap: usize,
    pub gateway_profile: String,
    pub harness: HarnessKind,
    pub segments_completed: u32,
    pub state: ResearchWorkerState,
    pub tool_count: usize,
    pub sealed: bool,
    pub sandbox_deleted: bool,
}

#[derive(Clone, Debug, Serialize)]
pub struct ResearchStudySummary {
    pub name: String,
    pub mode: ResearchMode,
    pub input_file_count: u64,
    pub input_bytes: u64,
    pub max_output_files: u64,
    pub max_output_bytes: u64,
    pub workers: Vec<ResearchWorkerSummary>,
    pub comparison: Option<ResearchComparison>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ResearchClaim {
    pub statement: String,
    pub kind: String,
    pub evidence: Vec<String>,
    pub confidence: f64,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct RerunContract {
    pub command: String,
    pub environment: String,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ResearchResult {
    pub schema_version: u32,
    pub worker: String,
    pub status: String,
    pub answer: String,
    pub claims: Vec<ResearchClaim>,
    pub limitations: Vec<String>,
    pub rerun: RerunContract,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub method: Option<ScientificMethodContract>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub quality_control: Option<DataQualityControlContract>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ScientificMethodContract {
    pub class: String,
    pub tool_or_model: String,
    pub version: String,
    pub input_manifest_sha256: String,
    pub code_refs: Vec<String>,
    pub parameter_refs: Vec<String>,
    pub output_refs: Vec<String>,
    pub validation_refs: Vec<String>,
    #[serde(default)]
    pub seeds: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub llm_disclosure: Option<LlmMethodDisclosure>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct LlmMethodDisclosure {
    pub model: String,
    pub revision: String,
    pub engine: String,
    pub prompt_ref: String,
    pub sampling_ref: String,
    pub repeat_count: u32,
    pub evaluation_refs: Vec<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct DataQualityControlContract {
    pub purpose: String,
    pub schema_refs: Vec<String>,
    pub sample_refs: Vec<String>,
    pub check_refs: Vec<String>,
    pub llm_is_scientific_instrument: bool,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ResearchSeal {
    pub schema_version: u32,
    pub study: String,
    pub worker: String,
    pub sealed_at: String,
    pub question_sha256: String,
    pub protocol_sha256: String,
    pub input_manifest_sha256: String,
    pub perspective_sha256: String,
    pub assignment_sha256: String,
    pub route: String,
    pub harness: HarnessKind,
    pub route_context_limit: u64,
    pub route_output_limit: u64,
    pub route_pool_cap: usize,
    pub thinking: Option<Thinking>,
    pub tool_digests: BTreeMap<String, String>,
    pub result_sha256: String,
    pub output_sha256: String,
    pub output_files: u64,
    pub output_bytes: u64,
    pub scientific_validation: bool,
}

#[derive(Clone, Debug, Serialize)]
pub struct ResearchVerification {
    pub study: String,
    pub input_manifest_valid: bool,
    pub worker_seals_valid: usize,
    pub comparison_valid: bool,
}

#[derive(Clone, Debug, Serialize)]
pub struct ResearchRunCapture {
    pub schema_version: u32,
    pub study: String,
    pub worker: String,
    pub segment: u32,
    pub captured_at: String,
    pub passed: bool,
    pub result_class: String,
    pub result_complete: bool,
    pub fresh_provider_session: bool,
    pub exit_code: Option<i32>,
    pub stdout_bytes: usize,
    pub stderr_bytes: usize,
    pub stdout_truncated: bool,
    pub stderr_truncated: bool,
    pub observations: ResearchObservationSummary,
    pub context_pressure_ratio: f64,
    pub context_action: String,
    pub checkpoint_epoch: Option<u64>,
}

#[derive(Clone, Debug, Default, Serialize)]
pub struct ResearchObservationSummary {
    pub json_lines: u64,
    pub malformed_lines: u64,
    pub error_events: u64,
    pub tool_calls: u64,
    pub step_finishes: u64,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub reasoning_tokens: u64,
    pub total_tokens: u64,
    pub peak_context_tokens: u64,
    pub cache_read_tokens: u64,
    pub cache_write_tokens: u64,
    pub workspace_snapshots: u64,
    pub added_files: u64,
    pub changed_files: u64,
    pub removed_files: u64,
    pub artifact_versions: u64,
}

#[derive(Clone, Debug, Serialize)]
pub struct ResearchExecutionCapture {
    pub schema_version: u32,
    pub study: String,
    pub worker: String,
    pub passed: bool,
    pub result_class: String,
    pub result_complete: bool,
    pub segments: Vec<ResearchRunCapture>,
}

pub struct ResearchWorkerSpec<'a> {
    pub study_name: &'a str,
    pub worker_name: &'a str,
    pub perspective: Option<&'a str>,
    pub perspective_file: Option<&'a Path>,
    pub tool_names: &'a [String],
    pub thinking: Option<Thinking>,
    pub harness: HarnessKind,
    pub runtime: &'a RuntimeArgs,
}

pub struct ResearchStudySpec<'a> {
    pub name: &'a str,
    pub mode: ResearchMode,
    pub question: Option<&'a str>,
    pub question_file: Option<&'a Path>,
    pub protocol: Option<&'a Path>,
    pub inputs: &'a [PathBuf],
    pub max_output_files: u64,
    pub max_output_bytes: u64,
}
