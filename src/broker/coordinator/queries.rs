//! Proposed actions and their query sources, and the standalone query and reply checks.
use super::*;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProposedAction {
    pub action_key: String,
    pub title: String,
    pub profile: String,
    pub objective: String,
    pub rationale: String,
    /// The block for a reviewed template profile; absent for every other profile.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub code: Option<String>,
    /// One choice for every setting of a configured method; absent otherwise.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "settings_text"
    )]
    pub settings: Option<BTreeMap<String, String>>,
    /// The library passages that shaped this step, each shown in this context.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub literature_refs: Vec<String>,
    /// One SELECT for a source's query method; absent for every other method.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sql: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reviewed_query: Option<super::super::reviewed_query::Request>,
    /// The analysis for the lab's template-analysis method: a finished step, one of its result tables, an
    /// analysis the method lists and its settings; absent for every other method.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub analysis: Option<AnalysisChoice>,
    /// The files a step of the lab's objects method reads: a collection the method showed and names from its sample;
    /// absent for every other method (migration 083).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub objects: Option<crate::broker::lead_objects::ObjectsChoice>,
    /// What the step's result must look like. Required in a reply; a step recorded before step
    /// checks has none.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expect: Option<crate::broker::step_checks::Expectation>,
}

/// A source the run may query, as `broker_research_run_sql_sources` returns it.
#[derive(Clone, Debug, Deserialize)]
pub struct QuerySource {
    pub profile: String,
    pub source: String,
    pub title: String,
    pub description: String,
    pub release: String,
    pub schema: String,
    pub relations: Value,
    pub functions: Vec<String>,
    pub row_cap: u64,
    /// The source's schema card for its snapshot and the digest its registration stored (migration
    /// 047); absent before one is registered.
    #[serde(default)]
    pub card: Option<Value>,
    #[serde(default)]
    pub card_sha256: Option<String>,
    /// The version row of the lab's data the card was measured at, and the one the source's newest completed query
    /// read (migration 082); absent for a pinned source or before either is recorded.
    #[serde(default)]
    pub card_source_version: Option<Value>,
    #[serde(default)]
    pub source_version: Option<Value>,
    #[serde(default)]
    pub reviewed_tools: Vec<super::super::reviewed_query::Tool>,
}

impl QuerySource {
    /// Whether the card was measured at another version of the lab's data than the newest completed query read. False
    /// while either is unknown. The version rows themselves stay here: they are the lab database's text.
    pub(super) fn card_behind(&self) -> bool {
        matches!((&self.card_source_version, &self.source_version), (Some(card), Some(data)) if card != data)
    }

    pub(super) fn relation_names(&self) -> BTreeSet<String> {
        self.relations
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|r| r["name"].as_str().map(str::to_owned))
            .collect()
    }

    /// Admit one query against this source's lists, the check every proposal's query passes.
    pub(super) fn admit(
        &self,
        sql: &str,
    ) -> std::result::Result<sql_guard::Admitted, sql_guard::Refused> {
        let relations = self.relation_names();
        let functions: BTreeSet<String> = self.functions.iter().cloned().collect();
        sql_guard::admit(
            sql,
            &Allowed {
                schema: &self.schema,
                relations: &relations,
                functions: &functions,
                row_cap: self.row_cap,
            },
        )
    }

    /// What the lead reads about the source: its shape and rules, never its rows or connection.
    pub(super) fn view(&self) -> Value {
        let mut view = json!({"source": self.title, "release": self.release, "about": self.description,
            "relations": self.relations, "functions": self.functions, "row_cap": self.row_cap});
        let tools: Vec<_> = self
            .reviewed_tools
            .iter()
            .filter(|t| {
                t.source == self.source
                    && t.state == super::super::reviewed_query::State::Enabled
                    && !self.reviewed_tools.iter().any(|other| {
                        other.key == t.key
                            && other.source == self.source
                            && other.state == super::super::reviewed_query::State::Enabled
                            && other.version > t.version
                    })
            })
            .map(|t| t.view())
            .collect();
        if !tools.is_empty() {
            view["reviewed_queries"] = json!(tools);
        }
        view
    }
}

pub(super) const MAX_CHECKED_QUERIES: usize = 1000;

pub(super) const MAX_CHECKED_LINE: usize = 1024 * 1024;

#[derive(Deserialize)]
pub(super) struct QueryCheck {
    pub(super) id: String,
    pub(super) source: QuerySource,
    pub(super) sql: String,
}

/// The operator's check of the query guard: one JSON object per input line with `id`, a `source` as
/// `broker_research_run_sql_sources` returns it and `sql`; one JSON line out per query, with the admitted text, its
/// digest, relations and row limit and how many `round` or `trunc` calls were cast, or the fixed
/// refusal code and the names it refused. It casts and admits each query as a proposal's query is cast and admitted and
/// opens no database or network connection.
pub fn check_queries(
    input: impl std::io::BufRead,
    mut output: impl std::io::Write,
) -> Result<usize> {
    let mut count = 0;
    for line in input.lines() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        count += 1;
        if count > MAX_CHECKED_QUERIES || line.len() > MAX_CHECKED_LINE {
            bail!("check_queries_invalid");
        }
        let check: QueryCheck =
            serde_json::from_str(&line).map_err(|_| anyhow!("check_queries_invalid"))?;
        if check.id.is_empty() || check.id.chars().count() > 128 {
            bail!("check_queries_invalid");
        }
        let (sql, cast) = sql_guard::cast_places(&check.sql);
        let decision = match check.source.admit(&sql) {
            Ok(q) => json!({"id": check.id, "admitted": true, "sha256": sha(q.sql.as_bytes()),
                "relations": q.relations, "row_limit": q.limit, "guard": sql_guard::GUARD, "cast": cast,
                "sql": q.sql}),
            Err(r) => json!({"id": check.id, "admitted": false, "code": r.code, "names": r.names,
                "guard": sql_guard::GUARD}),
        };
        serde_json::to_writer(&mut output, &decision)?;
        output.write_all(b"\n")?;
    }
    output.flush()?;
    Ok(count)
}

pub(super) const MAX_CHECKED_REPLIES: usize = 10_000;

pub(super) const MAX_CHECKED_REPLY_LINE: usize = 8 * 1024 * 1024;

/// The input ceiling assigned to old replay records that predate their per-call `max_input_bytes` field.
const LEGACY_REPLAY_INPUT_BYTES: usize = 65_536;

/// One lead reply to check offline, with what its call was checked against.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct ReplyCheck {
    pub(super) id: String,
    /// The context exactly as the lead call sent it.
    pub(super) context: Value,
    /// The reply's content and finish reason as the model returned them.
    pub(super) reply: Option<String>,
    pub(super) finish_reason: Option<String>,
    /// The run's checkpoint state at that call, its allowed profiles and parallel steps.
    pub(super) state: ResearchState,
    pub(super) profiles: Vec<String>,
    pub(super) parallel: usize,
    /// The run's methods (the claim's profiles) and its query sources as `broker_research_run_sql_sources` returns
    /// them; the methods gain each source's description as the coordinator adds it.
    pub(super) methods: Value,
    pub(super) sources: Value,
    /// The project's own queries as `research_run_own_queries` returned them for that call.
    #[serde(default)]
    pub(super) own_queries: Value,
    /// The model's input ceiling at that call (`max_input_bytes`); the historical ceiling when absent.
    #[serde(default = "legacy_replay_input")]
    pub(super) max_input_bytes: usize,
    /// The project's archived action keys at that call; none when absent.
    #[serde(default)]
    pub(super) archived: HashSet<String>,
    /// The run's template-analysis method and catalog as `research_run_analysis_catalog` returned them for that call
    /// (its `catalog_sha256` names the catalog); none when absent, so a step for that method is refused.
    #[serde(default)]
    pub(super) analysis_catalog: Value,
}

pub(super) fn legacy_replay_input() -> usize {
    LEGACY_REPLAY_INPUT_BYTES
}

/// The replay pool's scorer: one JSON object per input line (`ReplyCheck`), one JSON line out per
/// reply with `outcome` (`accepted`, `unreadable`, `refused` or `context_invalid`), the fixed code and, for a refusal,
/// how many names it refused, or for an accepted reply its counts of steps, findings (of them composed from table
/// statements), lessons and the numbers masked in the summary. The names themselves
/// are never printed: they go to the next model call only and can hold result numbers or query text. It reads the
/// reply as the coordinator does and runs `check_lead_reply` against the result index the kept context implies. It
/// prints no reply or context text and opens no database or network connection. A line is read at most one byte past
/// its limit, so an over-long line is refused before it is held whole.
pub fn check_lead_replies(
    mut input: impl std::io::BufRead,
    mut output: impl std::io::Write,
) -> Result<usize> {
    use std::io::{BufRead, Read};
    let (mut count, mut lines) = (0, 0);
    let mut buffer = Vec::new();
    loop {
        buffer.clear();
        let read = (&mut input)
            .take(MAX_CHECKED_REPLY_LINE as u64 + 1)
            .read_until(b'\n', &mut buffer)?;
        if read == 0 {
            break;
        }
        lines += 1;
        let line = buffer.strip_suffix(b"\n").unwrap_or(&buffer);
        if lines > MAX_CHECKED_REPLIES || line.len() > MAX_CHECKED_REPLY_LINE {
            bail!("check_lead_replies_invalid");
        }
        let line = std::str::from_utf8(line).map_err(|_| anyhow!("check_lead_replies_invalid"))?;
        if line.trim().is_empty() {
            continue;
        }
        count += 1;
        let check: ReplyCheck =
            serde_json::from_str(line).map_err(|_| anyhow!("check_lead_replies_invalid"))?;
        if check.id.is_empty() || check.id.chars().count() > 128 || check.parallel == 0 {
            bail!("check_lead_replies_invalid");
        }
        let sources = query_sources(&check.sources)?;
        let methods = lead_analysis::with_analysis_catalog(
            &with_query_sources(&check.methods, &sources),
            &check.analysis_catalog,
        );
        let input_ceiling = check.max_input_bytes.min(MAX_INPUT_BYTES);
        let (index, rejected) =
            match ResultIndex::from_context_with_ceiling(&check.context, &methods, input_ceiling) {
                Ok(rebuilt) => rebuilt,
                Err(fault) => {
                    let decision =
                        json!({"id": check.id, "outcome": "context_invalid", "fault": fault});
                    serde_json::to_writer(&mut output, &decision)?;
                    output.write_all(b"\n")?;
                    continue;
                }
            };
        let decision = match read_finished_reply(
            check.finish_reason.as_deref(),
            check.reply.as_deref(),
        ) {
            Err(fault) => {
                let refused = unreadable_reply("lead", fault);
                json!({"id": check.id, "outcome": "unreadable", "code": refused.code, "fault": fault})
            }
            Ok((value, normalized)) => {
                let lead = LeadCheck {
                    state: &check.state,
                    profiles: &check.profiles,
                    parallel: check.parallel,
                    methods: &methods,
                    sources: &sources,
                    own_queries: &check.own_queries,
                    context: &check.context,
                    index: &index,
                    rejected: &rejected,
                    archived: &check.archived,
                    input_ceiling,
                };
                match check_lead_reply(&lead, &value) {
                    Ok(checked) => {
                        let composed = checked
                            .proposal
                            .findings
                            .iter()
                            .filter(|f| f.composed_from.is_some())
                            .count();
                        let mut out = json!({"id": check.id, "outcome": "accepted", "normalized": normalized,
                        "action": checked.proposal.action, "answer": checked.proposal.answer,
                        "steps": checked.proposal.actions.len(),
                        "findings": checked.proposal.findings.len(), "composed": composed,
                        "table_statements": checked.proposal.table_statements.len(),
                        "lessons": checked.proposal.lessons.len(), "parts": checked.proposal.parts.len(),
                        "summary_numbers_masked": checked.summary_numbers_masked,
                        "step_numbers_masked": checked.step_numbers_masked});
                        // Named only when a query was cast, so earlier scores keep their shape.
                        if checked.query_places_cast > 0 {
                            out["query_places_cast"] = json!(checked.query_places_cast);
                        }
                        // The same way: lesson refs the check left out, as counts.
                        if checked.lessons_trimmed.refs_total > 0 {
                            out["lesson_refs_left_out"] = json!(checked.lessons_trimmed.refs_total);
                            out["lessons_dropped"] = json!(checked.lessons_trimmed.lessons);
                        }
                        out
                    }
                    Err(error) => {
                        let refused = refusal(&error);
                        json!({"id": check.id, "outcome": "refused", "normalized": normalized,
                            "code": refused.reason, "faults": super::replies::fault_count(&refused.names)})
                    }
                }
            }
        };
        serde_json::to_writer(&mut output, &decision)?;
        output.write_all(b"\n")?;
    }
    output.flush()?;
    Ok(count)
}

/// The run's sources by method key.
pub fn query_sources(sources: &Value) -> Result<HashMap<String, QuerySource>> {
    let list: Vec<QuerySource> =
        serde_json::from_value(sources.clone()).map_err(|_| anyhow!("query_sources_invalid"))?;
    Ok(list.into_iter().map(|s| (s.profile.clone(), s)).collect())
}

/// The claim's methods with each source's description under `query`, so the lead reads it with its method, and the
/// source's whole schema card under `query_card`, which the context compiler trims to its share and the methods view
/// leaves out.
pub fn with_query_sources(profiles: &Value, sources: &HashMap<String, QuerySource>) -> Value {
    let mut methods = profiles.clone();
    for method in methods.as_array_mut().into_iter().flatten() {
        if let Some(source) = method["key"].as_str().and_then(|k| sources.get(k)) {
            method["query"] = source.view();
            if method["query"]["reviewed_queries"]
                .as_array()
                .is_some_and(|t| !t.is_empty())
            {
                method["reviewed_query_note"] = json!(super::super::reviewed_query::OFFER);
            }
            if let Some(card) = source.card.as_ref().filter(|c| c.is_object()) {
                method["query_card"] = json!({"card": card, "sha256": source.card_sha256});
                if source.card_behind() {
                    method["query_card"]["behind"] = json!(true);
                }
            }
        }
    }
    methods
}

/// A query the guard admitted for one proposed step, with the text the commit stores.
#[derive(Clone, Debug)]
pub struct AdmittedQuery {
    pub source: String,
    /// The lead's query with each two-argument `round` or `trunc` cast to numeric; the worker admits
    /// this text again, so it is the one the commit stores.
    pub proposed: String,
    pub admitted: sql_guard::Admitted,
    /// How many `round` or `trunc` calls the cast changed.
    pub places_cast: usize,
    pub reviewed: Option<super::super::reviewed_query::Record>,
}

/// Settings name choice keys. A key that reads as a number (`10000`) may arrive as a JSON
/// number, so a number becomes its text and is then checked like any key; anything else is a
/// schema error.
pub(super) fn settings_text<'de, D: Deserializer<'de>>(
    deserializer: D,
) -> std::result::Result<Option<BTreeMap<String, String>>, D::Error> {
    let raw: Option<BTreeMap<String, Value>> = Option::deserialize(deserializer)?;
    raw.map(|settings| {
        settings
            .into_iter()
            .map(|(key, value)| match value {
                Value::String(text) => Ok((key, text)),
                Value::Number(number) => Ok((key, number.to_string())),
                _ => Err(serde::de::Error::custom("setting_not_text")),
            })
            .collect()
    })
    .transpose()
}

#[cfg(test)]
mod card_version_tests {
    use super::*;

    fn source(card: Option<Value>, data: Option<Value>) -> QuerySource {
        serde_json::from_value(json!({"profile": "lab", "source": "lab-db", "title": "Lab", "description": "Lab data",
            "release": "live", "schema": "lab", "relations": [{"name": "samples"}], "functions": ["count"],
            "row_cap": 100, "card": {"schema": "enrichedlab.sql-source-card.v1"}, "card_sha256": "sha256:00",
            "card_source_version": card, "source_version": data}))
        .unwrap()
    }

    #[test]
    fn a_card_is_behind_only_when_both_versions_are_known_and_differ() {
        let (old, new) = (
            json!({"newest_import_id": 4}),
            json!({"newest_import_id": 5}),
        );
        assert!(!source(None, None).card_behind());
        assert!(!source(Some(old.clone()), None).card_behind());
        assert!(!source(None, Some(new.clone())).card_behind());
        assert!(!source(Some(new.clone()), Some(new.clone())).card_behind());
        assert!(source(Some(old.clone()), Some(new.clone())).card_behind());
        let sources = HashMap::from([("lab".to_owned(), source(Some(old), Some(new)))]);
        let methods = with_query_sources(&json!([{"key": "lab"}]), &sources);
        assert_eq!(methods[0]["query_card"]["behind"], true);
        assert!(
            !serde_json::to_string(&methods)
                .unwrap()
                .contains("newest_import_id")
        );
        let current = HashMap::from([("lab".to_owned(), source(None, Some(json!({}))))]);
        assert!(
            with_query_sources(&json!([{"key": "lab"}]), &current)[0]["query_card"]
                .get("behind")
                .is_none()
        );
    }
}
