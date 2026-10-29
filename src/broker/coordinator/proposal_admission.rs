//! Proposal shape and method admission before a step is committed.
use super::*;

impl Proposal {
    #[allow(clippy::too_many_arguments)]
    pub fn validate(
        &self,
        state: &ResearchState,
        profiles: &[String],
        parallel: usize,
        evidence: &HashSet<String>,
        templates: &HashMap<String, String>,
        rejected: &HashSet<String>,
        unread: &UnreadRefs,
    ) -> Result<()> {
        let mut keys: HashSet<&str> = state
            .plan
            .iter()
            .filter(|s| s.task_id.is_some() || s.status != StepStatus::Pending)
            .map(|s| s.action_key.as_str())
            .collect();
        let new_actions = self
            .actions
            .iter()
            .filter(|a| !state.plan.iter().any(|s| s.action_key == a.action_key))
            .count();
        // Each broken rule is named (fixed words, never reply text), so the log and the lead's next call say which
        // (a test run ended on two unnamed shape refusals).
        let mut broken: Vec<String> = Vec::new();
        if !matches!(
            self.action.as_str(),
            "delegate" | "review_required" | "paused"
        ) {
            broken.push("action".into());
        }
        if !clean_text(&self.summary, SUMMARY_MAX) {
            broken.push("summary".into());
        }
        if self.action == "delegate" && self.actions.is_empty() {
            broken.push("delegate_without_actions".into());
        }
        if self.action != "delegate" && !self.actions.is_empty() {
            broken.push("actions_without_delegate".into());
        }
        if self.actions.len() > parallel {
            broken.push(format!("actions_over_{parallel}"));
        }
        if state.plan.len() + new_actions > PLAN_STEPS_MAX {
            broken.push(format!("plan_steps_over_{PLAN_STEPS_MAX}"));
        }
        if self.lessons.len() > 8 || state.lessons.len() + self.lessons.len() > 128 {
            broken.push("lessons_over_limit".into());
        }
        if !broken.is_empty() {
            return Err(CodeRefused {
                code: "proposal_shape_invalid",
                names: broken,
            }
            .into());
        }
        // A question answered in full ends for review: two suite goals each paused once
        // with every part supported, which left a finished answer waiting for nobody. Paused is for a run that cannot go
        // on until a person acts.
        if self.action == "paused" && self.answer == Answer::Answered {
            bail!("proposal_paused_answered")
        }
        for a in &self.actions {
            if keys.contains(a.action_key.as_str()) {
                bail!("proposal_action_reused")
            }
            if !action_key(&a.action_key)
                || !keys.insert(&a.action_key)
                || !profiles.contains(&a.profile)
                || !clean_text(&a.title, 512)
                || !clean_text(&a.objective, 8192)
                || !clean_text(&a.rationale, 2048)
            {
                bail!("proposal_action_invalid")
            }
            match (templates.contains_key(&a.profile), &a.code) {
                (true, None) => bail!("proposal_code_missing"),
                (false, Some(_)) => bail!("proposal_code_unexpected"),
                _ => {}
            }
        }
        for (n, l) in self.lessons.iter().enumerate() {
            if !clean_text(&l.statement, 2048)
                || l.kind == LessonKind::Constraint
                || (l.kind == LessonKind::Observation && l.evidence_refs.is_empty())
            {
                bail!("proposal_lesson_invalid")
            }
            if l.evidence_refs
                .iter()
                .any(|r| ref_task(r).is_some_and(|t| rejected.contains(t)))
            {
                bail!("proposal_evidence_rejected")
            }
            if l.evidence_refs.iter().any(|r| !evidence.contains(r)) {
                // The first lesson with an unknown ref decides the code, as before; the names cover it and every later
                // lesson's unknown refs, so one retry can correct them all.
                return Err(CodeRefused {
                    code: "proposal_evidence_unknown",
                    names: unread.lesson_names(&self.lessons, n, evidence),
                }
                .into());
            }
        }
        Ok(())
    }

    /// A configured method takes settings and no code; every other method takes no settings.
    /// Code for a configured method is refused by `validate`, which does not count it as a template.
    pub fn validate_settings(&self, configured: &HashMap<String, ConfiguredMethod>) -> Result<()> {
        for a in &self.actions {
            match (configured.get(&a.profile), &a.settings) {
                (Some(_), None) => bail!("proposal_settings_missing"),
                (Some(method), Some(settings)) => match &method.configuration {
                    Ok(c) => c.validate(settings).map_err(|e| anyhow!(e))?,
                    // The method's own configuration does not load: a catalog defect, not the lead's.
                    Err(_) => bail!("configured_method_invalid"),
                },
                (None, Some(_)) => bail!("proposal_settings_invalid"),
                (None, None) => {}
            }
        }
        Ok(())
    }

    /// A query method takes one SELECT, which the guard must admit; every other method takes none. A refusal carries the
    /// guard's fixed code and the names it refused, for the next lead call.
    pub fn admit_queries(
        &self,
        sources: &HashMap<String, QuerySource>,
    ) -> Result<HashMap<String, AdmittedQuery>> {
        let mut admitted = HashMap::new();
        for a in &self.actions {
            // A refused step is named by its key, for the next lead call.
            let refused = |code: &'static str| CodeRefused {
                code,
                names: vec![a.action_key.clone()],
            };
            if let Some(request) = &a.reviewed_query {
                let source = sources
                    .get(&a.profile)
                    .ok_or_else(|| refused("proposal_sql_unexpected"))?;
                if a.sql.is_some()
                    || a.code.is_some()
                    || a.settings.is_some()
                    || a.analysis.is_some()
                {
                    return Err(refused("proposal_sql_unexpected").into());
                }
                let tool = source
                    .reviewed_tools
                    .iter()
                    .filter(|t| {
                        t.key == request.tool
                            && t.source == source.source
                            && t.state == crate::broker::reviewed_query::State::Enabled
                    })
                    .max_by_key(|t| t.version)
                    .ok_or_else(|| refused("proposal_reviewed_query_unknown"))?;
                let relations = source.relation_names();
                let functions = source.functions.iter().cloned().collect();
                let (query, record) = tool
                    .lower(
                        &request.parameters,
                        &Allowed {
                            schema: &source.schema,
                            relations: &relations,
                            functions: &functions,
                            row_cap: source.row_cap,
                        },
                    )
                    .map_err(|fault| CodeRefused {
                        code: "proposal_reviewed_query_invalid",
                        names: vec![format!(
                            "step {}: {}{}",
                            a.action_key,
                            fault
                                .parameter
                                .map(|p| format!("{p}: "))
                                .unwrap_or_default(),
                            fault.code
                        )],
                    })?;
                admitted.insert(
                    a.action_key.clone(),
                    AdmittedQuery {
                        source: source.source.clone(),
                        proposed: query.sql.clone(),
                        admitted: query,
                        places_cast: 0,
                        reviewed: Some(record),
                    },
                );
                continue;
            }
            match (sources.get(&a.profile), &a.sql) {
                (Some(_), None) => return Err(refused("proposal_sql_missing").into()),
                (None, Some(_)) => return Err(refused("proposal_sql_unexpected").into()),
                (None, None) => {}
                (Some(source), Some(sql)) => {
                    if a.code.is_some() || a.settings.is_some() {
                        return Err(refused("proposal_sql_unexpected").into());
                    }
                    let (sql, places_cast) = sql_guard::cast_places(sql);
                    let query = source.admit(&sql).map_err(|refused| CodeRefused {
                        code: refused.code,
                        names: refused.names,
                    })?;
                    admitted.insert(
                        a.action_key.clone(),
                        AdmittedQuery {
                            source: source.source.clone(),
                            proposed: sql,
                            admitted: query,
                            places_cast,
                            reviewed: None,
                        },
                    );
                }
            }
        }
        Ok(admitted)
    }
}
