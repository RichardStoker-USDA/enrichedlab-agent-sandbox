//! Checking one lead reply against its context, and the reason recorded when a reply is refused.
use super::*;

/// Everything one lead reply is checked against, as the coordinator holds it after compiling the call's context.
pub struct LeadCheck<'a> {
    pub state: &'a ResearchState,
    pub profiles: &'a [String],
    pub parallel: usize,
    /// The run's methods with their query sources, as the context was compiled from them.
    pub methods: &'a Value,
    pub sources: &'a HashMap<String, QuerySource>,
    /// The queries this project's own analyses ran (migration 043), as `research_run_own_queries` returns them.
    pub own_queries: &'a Value,
    pub context: &'a Value,
    pub index: &'a ResultIndex,
    pub rejected: &'a HashSet<String>,
    /// The action keys of steps archived out of the project's working plan: never planned again.
    pub archived: &'a HashSet<String>,
    /// The model's input ceiling in bytes (`max_input_bytes`), system prompt and context together.
    pub input_ceiling: usize,
}

/// A lead reply that passed every check, with what the turn goes on to use.
pub struct CheckedLead {
    pub proposal: Proposal,
    pub queries: HashMap<String, AdmittedQuery>,
    /// The analysis each template-analysis step runs, by step key.
    pub analyses: HashMap<String, AdmittedAnalysis>,
    pub admitted: HashMap<String, (String, String, codegen::AdmissionReport, Option<Value>)>,
    /// Numbers in the summary written as `(a number)` because the prose rule refuses them.
    pub summary_numbers_masked: usize,
    /// Numbers in step titles and rationales written as `(a number)` because the prose rule refuses them.
    pub step_numbers_masked: usize,
    /// `round` and `trunc` calls in the proposal's queries whose first argument was cast to numeric.
    pub query_places_cast: usize,
    /// Lesson evidence refs outside what the call was shown, and observations left with none.
    pub lessons_trimmed: LessonTrim,
}

/// The lead checks of one call, in the coordinator's order: the proposal schema, its shape and steps, configured
/// settings, query admission and repeats, literature, table statements (composed into findings), findings, prose
/// numbers, then each block's admission. The
/// coordinator and `check-lead-replies` both run this, so an offline score cannot drift from production.
/// It opens no database or network connection.
pub fn check_lead_reply(check: &LeadCheck<'_>, reply: &Value) -> Result<CheckedLead> {
    let mut evidence = context_evidence(check.context);
    // A lesson may also rest on a part of a paper the lead read in its turn: a test run's lead wrote
    // what a paper it read did as a lesson citing that part. Passages it was only shown stay outside a lesson's evidence.
    evidence.extend(
        check.context["reading"]["parts"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|p| p["ref"].as_str().map(str::to_owned)),
    );
    let literature = context_literature(check.context);
    let unread = UnreadRefs::of(check.context);
    let mut proposal = Proposal::deserialize(reply).map_err(|_| CodeRefused {
        code: "proposal_schema_invalid",
        names: proposal_schema_names(reply),
    })?;
    // A narrowed third call is held to its redirect before the step checks it was meant to avoid.
    let redirect = check.context["redirect"]["kind"]
        .as_str()
        .and_then(Redirect::parse);
    if let Some(r) = redirect.filter(|r| *r != Redirect::FewerFindings) {
        r.check(&proposal)?;
    }
    let templates = profile_templates(check.methods);
    let configured = profile_configurations(check.methods);
    let columns = profile_columns(check.methods);
    // Every check runs on the reply as written, except that a lesson's evidence refs outside what
    // the call was shown or read are left out instead of refused. The first pass takes the lessons' own refs as
    // evidence, so membership is the one check it relaxes, and the shape, lesson and rejected-analysis refusals still
    // see every lesson and ref. The second holds the trimmed lessons to the evidence.
    let mut as_written = evidence.clone();
    as_written.extend(
        proposal
            .lessons
            .iter()
            .flat_map(|l| l.evidence_refs.iter().cloned()),
    );
    let validate = |proposal: &Proposal, evidence: &HashSet<String>| {
        proposal.validate(
            check.state,
            check.profiles,
            check.parallel,
            evidence,
            &templates,
            check.rejected,
            &unread,
        )
    };
    validate(&proposal, &as_written)?;
    let lessons_trimmed = ref_faults::trim_lessons(&mut proposal.lessons, &evidence, &unread);
    validate(&proposal, &evidence)?;
    // An archived step's key stays used: its analysis and plan ledger rows keep it.
    if proposal
        .actions
        .iter()
        .any(|a| check.archived.contains(&a.action_key))
    {
        bail!("proposal_action_reused")
    }
    proposal.validate_settings(&configured)?;
    let queries = proposal
        .admit_queries(check.sources)
        .inspect_err(|error| log_query_step_shape(error, reply))?;
    let repeated = repeated_queries(
        queries
            .iter()
            .map(|(key, q)| (key.as_str(), q.admitted.sql.as_str())),
        check.own_queries,
    );
    if !repeated.is_empty() {
        return Err(CodeRefused {
            code: "proposal_query_repeated",
            names: repeated,
        }
        .into());
    }
    let analyses = lead_analysis::admit(
        &proposal.actions,
        check.methods,
        check.context,
        check.rejected,
    )?;
    expectations(&proposal.actions)?;
    proposal.validate_literature(&literature, &unread)?;
    let written = proposal.findings.len();
    // The proposal carries the query each step will run, so the methods review and the turn's record read the cast
    // text; the lead's own reply stays kept by its hash.
    for a in &mut proposal.actions {
        if let Some(query) = queries.get(&a.action_key)
            && query.reviewed.is_none()
        {
            a.sql = Some(query.proposed.clone());
        }
    }
    let query_places_cast = queries.values().map(|q| q.places_cast).sum();
    proposal.compose_tables(check.index, check.rejected)?;
    proposal.check_tables(check.index, check.rejected)?;
    if let Some(r) = redirect.filter(|r| *r == Redirect::FewerFindings) {
        r.check(&proposal)?;
    }
    proposal
        .validate_findings(check.index, check.rejected)
        .inspect_err(|error| log_finding_faults(error, &proposal, check.index))?;
    proposal.validate_parts(written)?;
    // Names holding a number come from this call's own context, as sent (an index compiled before the
    // reading parts or the literature claims were placed would disagree with what the lead was shown).
    let named = check.index.with_names_from(check.context);
    let summary_numbers_masked = proposal.mask_summary_numbers(&named);
    let step_numbers_masked = proposal.mask_step_numbers(&named);
    proposal.validate_prose(&named)?;
    // The methods review sends this context, the proposal and every finding's values: refuse what it could not send,
    // asked again after an unreadable reply too, with a code the lead can act on, rather than fail the turn as
    // coordinator_internal (a suite run lost a turn that way).
    let need = review_input_bytes(&method_review_context(
        check.context,
        &proposal,
        check.index,
    ))? + REVIEW_NOTE_RESERVE;
    let over = need.saturating_sub(check.input_ceiling);
    if over > 0 {
        return Err(CodeRefused {
            code: "proposal_too_large",
            names: vec![format!("review: {over} bytes over")],
        }
        .into());
    }
    // Admit every block before the methods review spends a model call on it: the lead's own
    // code, or a configured method's block rendered from the chosen settings.
    let mut admitted = HashMap::new();
    for a in &proposal.actions {
        let names = columns.get(&a.profile).map(Vec::as_slice).unwrap_or(&[]);
        if let (Some(code), Some(template)) = (&a.code, templates.get(&a.profile)) {
            let report = codegen::admit(code, template, names);
            if !report.accepted() {
                return Err(CodeRefused {
                    code: codegen::event_code(report.first_reason()),
                    names: report.refused_names,
                }
                .into());
            }
            admitted.insert(
                a.action_key.clone(),
                (template.clone(), code.clone(), report, None),
            );
        } else if let (Some(settings), Some(method)) = (&a.settings, configured.get(&a.profile)) {
            let configuration = method
                .configuration
                .as_ref()
                .map_err(|_| anyhow!("configured_method_invalid"))?;
            let block = configuration.render(settings).map_err(|e| anyhow!(e))?;
            let report = codegen::admit(&block, &method.template_ref, names);
            if !report.accepted() {
                // A configured block is platform code; a refusal is a catalog defect, not the lead's.
                bail!("configured_block_rejected")
            }
            let record = configuration.record(settings);
            admitted.insert(
                a.action_key.clone(),
                (method.template_ref.clone(), block, report, Some(record)),
            );
        }
    }
    Ok(CheckedLead {
        proposal,
        queries,
        analyses,
        admitted,
        summary_numbers_masked,
        step_numbers_masked,
        query_places_cast,
        lessons_trimmed,
    })
}

/// Every step states what its result must look like, in a form code can check.
/// `proposal_step_expect_missing` names the steps without one; `proposal_step_expect_invalid` names each step whose
/// statement cannot be checked, with the part at fault.
fn expectations(actions: &[ProposedAction]) -> Result<()> {
    let refused = |code: &'static str, names: Vec<String>| -> anyhow::Error {
        CodeRefused {
            code,
            names: names.into_iter().take(12).map(bounded_name).collect(),
        }
        .into()
    };
    let missing: Vec<String> = actions
        .iter()
        .filter(|a| a.expect.is_none())
        .map(|a| a.action_key.clone())
        .collect();
    if !missing.is_empty() {
        return Err(refused("proposal_step_expect_missing", missing));
    }
    let invalid: Vec<String> = actions
        .iter()
        .filter_map(|a| {
            let part = match step_checks::invalid(a.expect.as_ref()?)? {
                "expect_empty" => "nothing to check",
                "expect_table" => "table",
                "expect_columns" => "columns",
                "expect_not_null" => "not_null",
                "expect_unique" => "unique",
                "expect_rows" => "rows",
                "expect_ranges" => "ranges",
                _ => "too long",
            };
            Some(format!("{}: {part}", a.action_key))
        })
        .collect();
    if !invalid.is_empty() {
        return Err(refused("proposal_step_expect_invalid", invalid));
    }
    Ok(())
}

/// Only fixed codes may reach logs. Provider, database and parse error text may
/// contain sensitive material and must never pass through this boundary.
pub fn rejection_reason(error: &str) -> &'static str {
    match error {
        "proposal_shape_invalid" => "proposal_shape_invalid",
        "proposal_paused_answered" => "proposal_paused_answered",
        "proposal_action_reused" => "proposal_action_reused",
        "proposal_action_invalid" => "proposal_action_invalid",
        "proposal_lesson_invalid" => "proposal_lesson_invalid",
        "proposal_evidence_unknown" => "proposal_evidence_unknown",
        "proposal_evidence_rejected" => "proposal_evidence_rejected",
        "proposal_finding_rejected" => "proposal_finding_rejected",
        "proposal_literature_unknown" => "proposal_literature_unknown",
        "proposal_schema_invalid" => "proposal_schema_invalid",
        "method_review_invalid" => "method_review_invalid",
        "model_unavailable" => "model_unavailable",
        "model_response_incomplete" => "model_response_incomplete",
        "model_response_invalid" => "model_response_invalid",
        "model_envelope_invalid" => "model_envelope_invalid",
        "model_content_missing" => "model_content_missing",
        "model_content_not_json" => "model_content_not_json",
        "model_identity_invalid" => "model_identity_invalid",
        "model_response_too_large" => "model_response_too_large",
        "model_budget_exceeded" => "model_budget_exceeded",
        "checkpoint_invalid" => "checkpoint_invalid",
        "proposal_code_missing" => "proposal_code_missing",
        "proposal_code_unexpected" => "proposal_code_unexpected",
        "proposal_sql_missing" => "proposal_sql_missing",
        "proposal_sql_unexpected" => "proposal_sql_unexpected",
        "proposal_reviewed_query_unknown" => "proposal_reviewed_query_unknown",
        "proposal_reviewed_query_invalid" => "proposal_reviewed_query_invalid",
        "proposal_query_repeated" => "proposal_query_repeated",
        "query_sources_invalid" => "query_sources_invalid",
        "proposal_analysis_unexpected" => "proposal_analysis_unexpected",
        "proposal_analysis_missing" => "proposal_analysis_missing",
        "proposal_analysis_unavailable" => "proposal_analysis_unavailable",
        "proposal_analysis_feature_unknown" => "proposal_analysis_feature_unknown",
        "proposal_analysis_contract_not_shown" => "proposal_analysis_contract_not_shown",
        "proposal_analysis_source_unknown" => "proposal_analysis_source_unknown",
        "proposal_analysis_source_not_finished" => "proposal_analysis_source_not_finished",
        "proposal_analysis_source_rejected" => "proposal_analysis_source_rejected",
        "proposal_analysis_table_unknown" => "proposal_analysis_table_unknown",
        "proposal_analysis_table_unsuitable" => "proposal_analysis_table_unsuitable",
        "proposal_analysis_settings_invalid" => "proposal_analysis_settings_invalid",
        "proposal_analysis_setting_unknown" => "proposal_analysis_setting_unknown",
        "proposal_analysis_column_unknown" => "proposal_analysis_column_unknown",
        "proposal_analysis_setting_invalid" => "proposal_analysis_setting_invalid",
        "proposal_analysis_setting_missing" => "proposal_analysis_setting_missing",
        "proposal_settings_missing" => "proposal_settings_missing",
        "proposal_settings_invalid" => "proposal_settings_invalid",
        "configured_block_rejected" => "configured_block_rejected",
        "configured_method_invalid" => "configured_method_invalid",
        "proposal_finding_invalid" => "proposal_finding_invalid",
        "proposal_findings_too_many" => "proposal_findings_too_many",
        "proposal_finding_placeholder_invalid" => "proposal_finding_placeholder_invalid",
        "proposal_finding_too_long" => "proposal_finding_too_long",
        "proposal_finding_no_values" => "proposal_finding_no_values",
        "proposal_finding_too_many_values" => "proposal_finding_too_many_values",
        "proposal_answer_needs_finding" => "proposal_answer_needs_finding",
        "proposal_finding_unbound" => "proposal_finding_unbound",
        "proposal_table_statement_invalid" => "proposal_table_statement_invalid",
        "proposal_table_statement_unbound" => "proposal_table_statement_unbound",
        "proposal_table_invalid" => "proposal_table_invalid",
        "proposal_table_unbound" => "proposal_table_unbound",
        "proposal_too_large" => "proposal_too_large",
        "proposal_redirect_ignored" => "proposal_redirect_ignored",
        "proposal_parts_invalid" => "proposal_parts_invalid",
        "proposal_prose_number" => "proposal_prose_number",
        "method_review_prose_number" => "method_review_prose_number",
        "method_review_findings_invalid" => "method_review_findings_invalid",
        "context_kept_mismatch" => "context_kept_mismatch",
        "context_budget_too_small" => "context_budget_too_small",
        "read_rounds_spent" => "read_rounds_spent",
        "proposal_step_expect_missing" => "proposal_step_expect_missing",
        "proposal_step_expect_invalid" => "proposal_step_expect_invalid",
        code if code.starts_with("code_") => codegen::event_code(code),
        code if code.starts_with("sql_refused") => sql_guard::code(code),
        _ => "coordinator_internal",
    }
}
