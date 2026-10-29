// Plain-language wording for recorded machine facts. Every sentence here is chosen by
// deterministic code from a status or code; none of it comes from a model.

export const capabilityNames: Record<string, string> = {
  read: "view",
  review: "review",
  contribute: "add analyses",
  steer: "change the question and steps",
  cancel: "stop work",
  manage: "manage access",
};

// Access levels offered when inviting a colleague. Each is a cumulative capability set.
export const accessLevels = [
  { key: "view", label: "Can view", capabilities: ["read"] },
  { key: "review", label: "Can review", capabilities: ["read", "review"] },
  { key: "contribute", label: "Can add analyses", capabilities: ["read", "review", "contribute"] },
  { key: "steer", label: "Can steer the research", capabilities: ["read", "review", "contribute", "steer", "cancel"] },
] as const;

export function accessSummary(capabilities: string[]): string {
  if (capabilities.includes("steer")) return "Can steer the research";
  if (capabilities.includes("contribute")) return "Can add analyses";
  if (capabilities.includes("review")) return "Can review";
  if (capabilities.includes("read")) return "Can view";
  return "No access";
}

export type Tone = "done" | "active" | "attention" | "failed" | "quiet";

export function taskStatus(status: string): { label: string; tone: Tone } {
  switch (status) {
    case "queued": return { label: "Waiting to start", tone: "active" };
    case "leased": case "starting": return { label: "Starting", tone: "active" };
    case "running": return { label: "Running", tone: "active" };
    case "cancelling": return { label: "Stopping", tone: "active" };
    case "completed": return { label: "Finished", tone: "done" };
    // The analysis stopped and hands its results to a person. "Waiting for you" counts something else: results no
    // person has reviewed yet ("Needs review 0" sat beside 8 waiting items).
    case "review_required": return { label: "Stopped for review", tone: "attention" };
    case "failed": return { label: "Did not finish", tone: "failed" };
    case "cancelled": return { label: "Stopped", tone: "quiet" };
    default: return { label: "Recorded", tone: "quiet" };
  }
}

export const activeTask = (status: string) => ["queued", "leased", "starting", "running", "cancelling"].includes(status);

// Lifecycle and runtime event codes, in the scientist's terms. The original code stays
// available under Technical details.
const eventWords: Record<string, string> = {
  figure_manuscript_marked: "Marked as the version in the manuscript",
  figure_manuscript_cleared: "Manuscript mark cleared",
  task_submitted: "Request received",
  task_accepted: "Request received",
  task_claimed: "Assigned to a compute worker",
  worker_lease_acquired: "Assigned to a compute worker",
  agent_started: "Analysis started",
  runtime_ready: "Analysis started",
  sandbox_created: "Isolated workspace prepared",
  sandbox_pod_observed: "Isolated workspace running",
  sandbox_terminal_observed: "Analysis finished running",
  sandbox_recovery_needed: "Workspace needs operator recovery",
  evidence_collected: "Output files collected",
  agent_completed: "Results saved",
  task_completed: "Results saved",
  review_required: "Stopped for review",
  cancel_requested: "Stop requested",
  lease_expired_requeued: "Worker stopped responding; queued again",
  lease_expired_cancelled: "Worker stopped responding; stop completed",
  lease_attempts_exhausted: "Worker stopped responding too many times",
  recovered_artifacts: "Output files recovered by the operator",
  agent_failed: "Analysis did not finish",
  task_failed: "Analysis did not finish",
};

export function eventLabel(code: string): string {
  if (eventWords[code]) return eventWords[code];
  const text = code.replaceAll("_", " ");
  return text.charAt(0).toUpperCase() + text.slice(1);
}

// Why a work session deferred. `model_*` and `proposal_*` codes are model replies the
// coordinator refused; they are not tool execution.
const deferralWords: Record<string, string> = {
  waiting_for_tools: "Waited for an analysis to finish",
  model_unavailable: "The planning model did not return a usable reply",
  proposal_invalid: "The research agent's reply was rejected by validation",
  checkpoint_changed: "Restarted from a newer saved plan",
  turn_limit: "Reached the decision limit set for this work session",
  review_required: "Stopped for your review",
  revise: "The methods check sent the proposal back for corrections",
};

const reasonWords: Record<string, string> = {
  model_content_not_json: "the reply was not valid structured data",
  model_response_incomplete: "the reply was cut off",
  model_unavailable: "the model service did not answer",
  model_envelope_invalid: "the model service returned an unreadable response",
  model_content_missing: "the reply was empty",
  model_identity_invalid: "the model service did not identify its model",
  model_response_too_large: "the reply was larger than allowed",
  model_budget_exceeded: "the request exceeded the model's context budget",
  proposal_schema_invalid: "the reply did not follow the required structure",
  proposal_shape_invalid: "the reply's plan was malformed",
  proposal_action_reused: "the reply tried to repeat finished work",
  proposal_paused_answered: "the reply paused although it had answered every part, so it was sent back to end for review",
  proposal_action_invalid: "the reply asked for a method this work session does not allow",
  proposal_lesson_invalid: "the reply's notes were malformed",
  proposal_evidence_unknown: "the reply cited evidence it was not given",
  proposal_evidence_rejected: "the reply's notes cited an analysis a person rejected",
  proposal_finding_rejected: "a finding read an analysis a person rejected",
  proposal_finding_invalid: "the reply's findings were malformed",
  proposal_findings_too_many: "the reply stated more findings than a decision may hold",
  proposal_finding_placeholder_invalid: "a finding marked a value in a way the rules do not allow",
  proposal_finding_too_long: "a finding was longer than allowed",
  proposal_finding_no_values: "a finding named no value from a results table",
  proposal_finding_too_many_values: "a finding named more values than one finding may hold",
  proposal_finding_unbound: "a finding named a value it was not shown",
  proposal_table_statement_invalid: "a table the reply asked to state was described in a way the rules do not allow",
  proposal_table_statement_unbound: "a table the reply asked to state named rows or columns it was not shown",
  proposal_table_invalid: "a table the reply gave as its answer was described in a way the rules do not allow",
  proposal_table_unbound: "a table the reply gave as its answer, or one of its columns, was not one it was shown",
  proposal_too_large: "the reply stated more than its review could hold",
  proposal_redirect_ignored: "the reply did not follow the narrower request made after two refused replies",
  proposal_parts_invalid: "the reply's account of which parts of the question were answered did not match its findings or its answer",
  context_too_large: "the project's records no longer fit in one request to the research agent",
  context_budget_too_small: "a request was larger than the model accepts",
  proposal_answer_needs_finding: "the reply claimed an answer without a finding",
  proposal_prose_number: "the reply wrote a number that did not come from a result file",
  proposal_code_missing: "the reply chose a written analysis without its code",
  proposal_code_unexpected: "the reply sent code for a method that takes none",
  proposal_settings_missing: "the reply chose a recipe without all of its settings",
  proposal_settings_invalid: "the reply chose settings the recipe does not offer",
  configured_method_invalid: "the recipe the reply chose has a configuration the platform could not read",
  configured_block_rejected: "a recipe did not pass the platform's code check",
  model_response_invalid: "the reply could not be read",
  method_review_invalid: "the methods check reply was malformed",
  method_review_prose_number: "the methods check wrote a number that did not come from a result file",
  method_review_findings_invalid: "the methods check listed mistakes that named no part of the proposal or no result it was shown",
  proposal_literature_unknown: "the reply cited a passage from a paper it was not shown",
  read_rounds_spent: "the reply asked to read more papers after its reading turns were used up",
  // The research lead's reviewed query tools and reviewed R analyses.
  proposal_reviewed_query_unknown: "the reply named a reviewed query tool the lab does not offer",
  proposal_reviewed_query_invalid: "the reply gave a reviewed query tool settings it does not accept",
  proposal_analysis_contract_not_shown: "the reply chose an analysis whose settings it had not been shown",
  proposal_query_repeated: "the reply asked to run a database query an analysis already ran",
  proposal_step_expect_missing: "a step in the reply did not say what its result must show",
  proposal_step_expect_invalid: "a step in the reply said what its result must show in a way that cannot be checked",
  proposal_sql_missing: "a database step came without its query",
  proposal_sql_unexpected: "a step that does not read a database carried a query",
  query_sources_invalid: "the work session's list of databases could not be read",
  context_kept_mismatch: "the record of what the agent was given did not match what it was sent",
  checkpoint_invalid: "the proposed plan could not be saved",
  coordinator_internal: "an internal coordinator error",
  // The research lead's choice of a reviewed R analysis.
  proposal_analysis_unexpected: "the reply attached an analysis to a step of another method, or code or a query to an analysis step",
  proposal_analysis_missing: "the reply chose the analysis method without naming an analysis",
  proposal_analysis_unavailable: "no analysis catalog had been read yet, so no analysis could run",
  proposal_analysis_feature_unknown: "the reply named an analysis it was not shown",
  proposal_analysis_source_unknown: "the reply named a step that does not exist",
  proposal_analysis_source_not_finished: "the reply named a step that has not finished",
  proposal_analysis_source_rejected: "the reply named a step a person rejected",
  proposal_analysis_table_unknown: "the reply named a table the step did not produce",
  proposal_analysis_table_unsuitable: "the table was empty, too large, or wider than its summary shows",
  proposal_analysis_settings_invalid: "the reply's analysis settings were too many or too large",
  proposal_analysis_setting_unknown: "the reply used a setting the analysis does not have",
  proposal_analysis_column_unknown: "the reply named a column the table does not have",
  proposal_analysis_setting_invalid: "a setting was of the wrong kind or outside its range",
  proposal_analysis_setting_missing: "a required column setting was missing",
  analysis_catalog_changed: "the analysis catalog changed after the agent read it; the next decision reads it again",
};

export const isModelRetry = (code: string) => code === "model_unavailable" || code === "proposal_invalid";

/** Why a reply was refused, in plain words, or null for a reason with none. */
// The methods check's decision in plain words.
const checkWords: Record<string, string> = {
  proceed: "agreed",
  revise: "sent the proposal back for corrections",
  needs_review: "asked for your review",
};
export function methodsCheckWords(decision: string): string {
  return checkWords[decision] ?? "asked for your review";
}

// Each kind of mistake the methods check names, for a finding that came without its own words.
const problemWords: Record<string, string> = {
  wrong_rows: "reads rows the table does not hold",
  wrong_order: "describes an order the table does not have",
  wrong_value: "misstates a value",
  units_mixed: "mixes units",
  rows_dropped: "leaves out rows the question needs",
  restriction_lost: "drops a restriction the question asks for",
  repeated_work: "repeats work already finished",
  question_not_answered: "does not answer the question",
  unsupported_statement: "states what the results do not show",
  other: "has a mistake",
};

/** One methods check finding as a sentence: the part of the proposal it is in, then what is wrong. A step is named by
 *  its title, never its key. */
export function reviewFindingLine(finding: { about: string; problem: string; detail?: string },
                                  actions: { action_key?: string; title: string }[]): string {
  const numbered = /^(part|finding|table statement) (\d+)$/.exec(finding.about);
  const step = finding.about.startsWith("step ") ? actions.find((a) => a.action_key === finding.about.slice(5)) : undefined;
  const part = finding.about === "summary" ? "The summary" : finding.about === "answer" ? "The answer"
    : finding.about === "parts" ? "The account of the question's parts"
    : numbered ? `${numbered[1] === "part" ? "Part" : numbered[1] === "finding" ? "Finding" : "Table statement"} ${numbered[2]}`
    : step ? `The step "${step.title}"` : finding.about.startsWith("step ") ? "A step" : "The proposal";
  return `${part}: ${finding.detail?.trim() || problemWords[finding.problem] || problemWords.other}.`.replace(/\.\.$/, ".");
}

export function refusalWords(reason: string): string | null {
  // Every refusal of model-written code, and of a database query, has its own code; the notebook names each as one kind.
  return reasonWords[reason] ?? (reason.startsWith("code_") ? "the platform's check refused the model-written code"
    : reason.startsWith("sql_refused") ? "the platform's check refused the database query" : null);
}

export function deferralLabel(code: string, reason: string | null): string {
  const base = deferralWords[code] ?? eventLabel(code);
  const why = reason ? refusalWords(reason) : null;
  return why ? `${base}: ${why}` : base;
}

export interface RunLike { status: string; code: string | null; turn: number; max_turns: number; failures?: number }

// One sentence for the current state of a work session.
// `canAct` is false for a reader who can only read the project: a stop then waits for someone else's review.
export function runSentence(run: RunLike | undefined, anyTaskActive: boolean, canAct = true): { text: string; tone: Tone } {
  if (anyTaskActive) return { text: "An analysis is running. Its files will appear under Results when it finishes.", tone: "active" };
  if (!run) return { text: "No research has started on this question yet.", tone: "quiet" };
  switch (run.status) {
    case "queued": return { text: "Research is queued and will start shortly.", tone: "active" };
    case "running": return { text: "The research agent is choosing the next step.", tone: "active" };
    case "waiting":
      if (run.code === "model_unavailable") return { text: "The planning model did not reply usefully. The agent will try again automatically.", tone: "active" };
      if (run.code === "proposal_invalid") return { text: "The agent's last reply was rejected by validation. It will try again automatically.", tone: "active" };
      if (run.code === "checkpoint_changed") return { text: "The plan changed while the agent was working. It will continue from the saved plan.", tone: "active" };
      return { text: "Waiting for an analysis to finish.", tone: "active" };
    case "paused":
      if (run.code === "user_paused") return { text: "Research is paused. Nothing new will start until you continue it.", tone: "quiet" };
      if (run.code === "delegation_revoked") return { text: "Research paused because the person who started it no longer has permission.", tone: "attention" };
      return { text: "Research paused after repeated rejected replies from the planning model. Review the record before continuing.", tone: "attention" };
    case "review_required":
      if (!canAct) return run.code === "turn_limit"
        ? { text: "The agent used every decision allowed for this work session. It waits for review by someone who can steer this project.", tone: "quiet" }
        : { text: "The agent stopped for review by someone who can steer this project.", tone: "quiet" };
      return run.code === "turn_limit"
        ? { text: "The agent used every decision allowed for this work session and is waiting for your review.", tone: "attention" }
        : { text: "The agent stopped and is waiting for your review.", tone: "attention" };
    case "failed": return { text: "The work session stopped with an error.", tone: "failed" };
    default: return { text: "Research status recorded.", tone: "quiet" };
  }
}

export function lessonLabel(kind: string): string {
  if (kind === "constraint") return "Study constraint";
  if (kind === "hypothesis") return "Agent's hypothesis, not verified";
  return "Agent's note, not verified";
}

export function stepStatus(status: string): { label: string; tone: Tone } {
  switch (status) {
    case "completed": return { label: "Finished", tone: "done" };
    case "active": return { label: "Running", tone: "active" };
    case "pending": return { label: "Planned", tone: "quiet" };
    case "failed": return { label: "Did not finish", tone: "failed" };
    case "uncertain": return { label: "Needs a check", tone: "attention" };
    case "skipped": return { label: "Skipped", tone: "quiet" };
    default: return { label: "Recorded", tone: "quiet" };
  }
}

export const auditWords: Record<string, string> = {
  access_changed: "Access changed",
  admin_inspection: "Administrator inspection started",
  admin_inspection_ended: "Administrator inspection ended",
  research_started: "Research started",
  research_paused: "Research paused",
  evidence_package_exported: "Evidence package downloaded",
  note_shared: "A note shared with the lab",
  note_retired: "A shared note retired",
  note_renewed: "A shared note renewed",
};

export function auditLabel(kind: string): string {
  return auditWords[kind] ?? eventLabel(kind);
}

// A readable title from a retained file name. Generic names take their folder name.
export function fileTitle(path: string): string {
  const parts = path.split("/");
  const name = parts.at(-1) ?? path;
  const stem = name.replace(/\.[^.]+$/, "");
  const words = (s: string) => s.replace(/[_-]+/g, " ").trim();
  const generic = /^(figure|plot|summary|table|result|results|output|data)$/i.test(stem);
  const folder = parts.length > 1 ? parts.at(-2) ?? "" : "";
  const text = generic && folder && !/^(results|outputs?|figures?)$/i.test(folder) ? `${words(folder)} ${words(stem)}` : words(stem);
  return text.charAt(0).toUpperCase() + text.slice(1);
}

// How long something took, from recorded start and finish times. A query that returns at once took "under 1 s", never
// "0 s"; nothing is said for work without both times.
export function duration(ms: number | null): string {
  if (ms === null || !Number.isFinite(ms) || ms < 0) return "";
  if (ms < 1000) return "under 1 s";
  const s = Math.round(ms / 1000);
  if (s < 60) return `${s} s`;
  const m = Math.round(s / 60);
  return m < 60 ? `${m} min` : `${Math.floor(m / 60)} h ${m % 60} min`;
}

export function bytes(size: number): string {
  if (size < 1024) return `${size} bytes`;
  if (size < 1024 ** 2) return `${(size / 1024).toFixed(1)} KB`;
  if (size < 1024 ** 3) return `${(size / 1024 ** 2).toFixed(1)} MB`;
  return `${(size / 1024 ** 3).toFixed(2)} GB`;
}

export function when(value: string | null | undefined): string {
  if (!value) return "";
  const date = new Date(value);
  if (Number.isNaN(date.getTime())) return "";
  const sameYear = date.getFullYear() === new Date().getFullYear();
  return date.toLocaleString(undefined, { month: "short", day: "numeric", ...(sameYear ? {} : { year: "numeric" }), hour: "numeric", minute: "2-digit" });
}

export function person(value: { display_name: string | null; email: string } | null | undefined): string {
  if (!value) return "Unknown person";
  return value.display_name?.trim() || value.email;
}

// The account button's letters: the first and last words of the person's name, else of the name part of the address.
// An address like jqsample@example.org is one name, so it gives one letter and never a letter of the domain.
export function initials(value: { display_name: string | null; email: string } | null | undefined): string {
  const letter = (word: string) => word.match(/\p{L}/u)?.[0] ?? "";
  const ends = (words: string[]) => (words.length > 1 ? letter(words[0]) + letter(words.at(-1)!) : letter(words[0] ?? "")).toUpperCase();
  const named = (value?.display_name ?? "").trim().split(/\s+/).filter((w) => letter(w));
  if (named.length) return ends(named);
  return ends((value?.email ?? "").split("@")[0].split(/[._+-]+/).filter((w) => letter(w)));
}
