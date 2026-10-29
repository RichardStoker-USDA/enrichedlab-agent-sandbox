//! The coordinator's arguments, model settings and run start request.
use super::*;

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct StartRun {
    pub run_id: Uuid,
    pub expected_revision: i64,
    pub allowed_profiles: Vec<String>,
    pub max_turns: i32,
    pub max_parallel: i32,
}

impl StartRun {
    pub fn validate(&self) -> Result<()> {
        if self.expected_revision < 1
            || !(1..=128).contains(&self.max_turns)
            || !(1..=8).contains(&self.max_parallel)
            || self.allowed_profiles.is_empty()
            || self.allowed_profiles.len() > 16
            || self.allowed_profiles.iter().any(|v| !action_key(v))
            || self.allowed_profiles.iter().collect::<HashSet<_>>().len()
                != self.allowed_profiles.len()
        {
            bail!("research_run_invalid")
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Args)]
pub struct CoordinatorArgs {
    #[arg(long)]
    pub database_dsn_file: PathBuf,
    #[arg(long)]
    pub tenant: String,
    /// Private, operator-owned model route and budget configuration.
    #[arg(long)]
    pub model_config_file: PathBuf,
    #[arg(long, default_value_t = 4)]
    pub max_parallel: usize,
    #[arg(long)]
    pub worker_id: Option<String>,
    /// Advance one claim and exit; useful for a controlled recovery or canary.
    #[arg(long)]
    pub once: bool,
    /// Publish readiness after all start checks; remove it when the coordinator exits.
    #[arg(long)]
    pub ready_file: Option<PathBuf>,
    /// Private lab library configuration: the document service and, per lab, a
    /// token that may only read that lab's corpus. Without it the lead reads no papers.
    #[arg(long)]
    pub library_config_file: Option<PathBuf>,
    /// Labs whose shared notes and library passages may reach the research lead.
    /// A lab not listed runs without them; the list is empty unless the operator names a lab.
    #[arg(long, env = "AGENT_SANDBOX_LEAD_TEXT_LABS", value_delimiter = ',')]
    pub lead_text_labs: Vec<String>,
    /// Listed labs whose papers the lead may read before a person in the lab accepts them: the
    /// qualification lab, whose library holds synthetic test papers. Every other
    /// listed lab's lead reads only papers a lab curator accepted. Each must also be listed above.
    #[arg(
        long,
        env = "AGENT_SANDBOX_LEAD_TEXT_UNREVIEWED_LABS",
        value_delimiter = ','
    )]
    pub lead_text_unreviewed_labs: Vec<String>,
    /// Listed labs whose lead reads the literature specialist's claims instead of the library passages.
    /// Off unless the operator names a lab; each must also be listed above.
    #[arg(
        long,
        env = "AGENT_SANDBOX_LITERATURE_CLAIMS_LABS",
        value_delimiter = ','
    )]
    pub literature_claims_labs: Vec<String>,
    /// The knowledge foundation's search: the service, the coordinator's projected token and the labs
    /// whose leads search it. Without it no lead searches.
    #[arg(long, env = "AGENT_SANDBOX_KNOWLEDGE_CONFIG_FILE")]
    pub knowledge_config_file: Option<PathBuf>,
    /// Labs whose initial permitted method order uses typed catalog matches. Off until qualified for that lab.
    #[arg(
        long,
        env = "AGENT_SANDBOX_RESEARCH_CATALOG_LABS",
        value_delimiter = ','
    )]
    pub research_catalog_labs: Vec<String>,
    /// Listed labs whose lead may read the library's papers whole before it decides. Off unless the
    /// operator names a lab; each must also be listed above, have a library and not read the literature specialist's
    /// claims.
    #[arg(long, env = "AGENT_SANDBOX_LEAD_READING_LABS", value_delimiter = ',')]
    pub lead_reading_labs: Vec<String>,
    /// The bytes of paper text one turn's reading may hold beside the lead's context; 0, the
    /// default, turns reading off.
    #[arg(long, env = "AGENT_SANDBOX_LEAD_READING_BYTES", default_value_t = 0)]
    pub lead_reading_bytes: usize,
    /// Audited study fact fields the lead may read; empty disables study reading.
    #[arg(long, value_delimiter = ',', value_parser = study_field)]
    pub lead_study_fields: Vec<String>,
    /// Labs whose lead first reads a checked literature review.
    #[arg(long, value_delimiter = ',')]
    pub literature_review_labs: Vec<String>,
    #[arg(long, default_value_t = 32768)]
    pub literature_review_bytes: usize,
    /// Labs whose runs a supervisor watches across the whole run: past a bound it narrows the
    /// next call, then pauses the run for a person with a fixed reason. Empty, the default, leaves runs as before.
    #[arg(long, value_delimiter = ',')]
    pub supervisor_labs: Vec<String>,
    /// Labs whose runs begin with an orientation: before the first plan, an orientation call
    /// restates the question, names the sources, tools and papers it expects to use and what stays unknown, and proposes
    /// at most three small profiling queries, which run as ordinary steps. Empty, the default, leaves runs as before.
    #[arg(long, value_delimiter = ',')]
    pub orientation_labs: Vec<String>,
}

/// Which labs' shared notes and library papers reach the research lead, which of those it may read before a person
/// accepts a paper, and whose lead reads the literature specialist's claims instead of the
/// passages, with the readings kept per run.
#[derive(Default)]
pub struct LabText {
    pub listed: HashSet<String>,
    pub unreviewed: HashSet<String>,
    pub claims: HashSet<String>,
    pub kept: literature_claims::Kept,
    pub catalog: HashSet<String>,
    /// Labs whose lead may read papers whole, and the bytes one turn's reading holds.
    pub reading: HashSet<String>,
    pub reading_bytes: usize,
    pub study_fields: HashSet<String>,
    pub review_labs: HashSet<String>,
    pub review_bytes: usize,
    pub supervisor: HashSet<String>,
    /// Labs whose runs begin with an orientation.
    pub orientation: HashSet<String>,
}

fn study_field(value: &str) -> std::result::Result<String, String> {
    if knowledge::STUDY_FIELDS.contains(&value) {
        Ok(value.to_owned())
    } else {
        Err("lead_study_field_invalid".into())
    }
}

#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct ModelConfig {
    pub(super) schema_version: u32,
    pub(super) endpoint: String,
    pub(super) model: String,
    pub(super) credential_file: Option<PathBuf>,
    #[serde(default)]
    pub(super) allow_internal_http: bool,
    pub(super) max_input_bytes: usize,
    pub(super) max_output_tokens: usize,
    pub(super) context_window_tokens: usize,
    #[serde(default = "default_timeout")]
    pub(super) timeout_seconds: u64,
    /// Optional operator-owned upper bounds for the variable sections of a lead context. Missing keys preserve the
    /// original section limits byte for byte.
    #[serde(default)]
    pub(super) sections: ModelSections,
    /// The served model's recommended request settings (its model card or serving recipe): sampling, a
    /// reasoning effort and chat template arguments. Absent, the request is the original greedy one.
    #[serde(default)]
    pub(super) settings: serde_json::Map<String, Value>,
}

/// The largest configurable item count for literature and past-research sections. Their defaults remain smaller;
/// this hard bound keeps their per-record work bounded independently of the model's byte ceiling.
pub(super) const SECTION_ITEMS_MAX: usize = 16;

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub(super) struct ModelSections {
    pub(super) query_relations_bytes: Option<usize>,
    pub(super) data_card_bytes: Option<usize>,
    pub(super) literature_bytes: Option<usize>,
    pub(super) literature_max: Option<usize>,
    pub(super) past_research_bytes: Option<usize>,
    pub(super) past_research_max: Option<usize>,
    pub(super) own_query_bytes: Option<usize>,
    pub(super) prior_query_bytes: Option<usize>,
    pub(super) tool_card_bytes: Option<usize>,
    pub(super) lab_notes_bytes: Option<usize>,
    pub(super) answerability_bytes: Option<usize>,
    pub(super) analyses_bytes: Option<usize>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct SectionBudgets {
    pub(super) query_relations_bytes: usize,
    pub(super) data_card_bytes: usize,
    pub(super) literature_bytes: usize,
    pub(super) literature_max: usize,
    pub(super) past_research_bytes: usize,
    pub(super) past_research_max: usize,
    pub(super) own_query_bytes: usize,
    pub(super) prior_query_bytes: usize,
    pub(super) tool_card_bytes: usize,
    pub(super) lab_notes_bytes: usize,
    pub(super) answerability_bytes: usize,
    /// The template-analysis catalog's share before the quarter-of-budget bound (`analysis_view::analyses_room`).
    pub(super) analyses_bytes: usize,
}

impl Default for SectionBudgets {
    fn default() -> Self {
        Self {
            query_relations_bytes: QUERY_RELATIONS_BYTES,
            data_card_bytes: DATA_CARD_BYTES,
            literature_bytes: LITERATURE_BYTES,
            literature_max: LITERATURE_MAX,
            past_research_bytes: PAST_RESEARCH_BYTES,
            past_research_max: PAST_RESEARCH_MAX,
            own_query_bytes: OWN_QUERY_BYTES,
            prior_query_bytes: PRIOR_QUERY_BYTES,
            tool_card_bytes: TOOL_CARD_BYTES,
            lab_notes_bytes: LAB_NOTES_BYTES,
            answerability_bytes: ANSWERABILITY_BYTES,
            analyses_bytes: crate::broker::analysis_view::ANALYSES_VIEW_BYTES,
        }
    }
}

impl ModelSections {
    fn valid(&self, lead_budget: usize) -> bool {
        let byte_max = lead_budget / 2;
        let bytes = [
            (self.query_relations_bytes, QUERY_RELATIONS_BYTES),
            (self.data_card_bytes, DATA_CARD_BYTES),
            (self.literature_bytes, LITERATURE_BYTES),
            (self.past_research_bytes, PAST_RESEARCH_BYTES),
            (self.own_query_bytes, OWN_QUERY_BYTES),
            (self.prior_query_bytes, PRIOR_QUERY_BYTES),
            (self.tool_card_bytes, TOOL_CARD_BYTES),
            (self.lab_notes_bytes, LAB_NOTES_BYTES),
            (self.answerability_bytes, ANSWERABILITY_BYTES),
            (
                self.analyses_bytes,
                crate::broker::analysis_view::ANALYSES_VIEW_BYTES,
            ),
        ];
        let counts = [
            (self.literature_max, LITERATURE_MAX),
            (self.past_research_max, PAST_RESEARCH_MAX),
        ];
        bytes
            .into_iter()
            .all(|(value, default)| value.is_none_or(|value| (default..=byte_max).contains(&value)))
            && counts.into_iter().all(|(value, default)| {
                value.is_none_or(|value| (default..=SECTION_ITEMS_MAX).contains(&value))
            })
    }

    fn effective(&self) -> SectionBudgets {
        SectionBudgets {
            query_relations_bytes: self.query_relations_bytes.unwrap_or(QUERY_RELATIONS_BYTES),
            data_card_bytes: self.data_card_bytes.unwrap_or(DATA_CARD_BYTES),
            literature_bytes: self.literature_bytes.unwrap_or(LITERATURE_BYTES),
            literature_max: self.literature_max.unwrap_or(LITERATURE_MAX),
            past_research_bytes: self.past_research_bytes.unwrap_or(PAST_RESEARCH_BYTES),
            past_research_max: self.past_research_max.unwrap_or(PAST_RESEARCH_MAX),
            own_query_bytes: self.own_query_bytes.unwrap_or(OWN_QUERY_BYTES),
            prior_query_bytes: self.prior_query_bytes.unwrap_or(PRIOR_QUERY_BYTES),
            tool_card_bytes: self.tool_card_bytes.unwrap_or(TOOL_CARD_BYTES),
            lab_notes_bytes: self.lab_notes_bytes.unwrap_or(LAB_NOTES_BYTES),
            answerability_bytes: self.answerability_bytes.unwrap_or(ANSWERABILITY_BYTES),
            analyses_bytes: self
                .analyses_bytes
                .unwrap_or(crate::broker::analysis_view::ANALYSES_VIEW_BYTES),
        }
    }
}

impl ModelConfig {
    pub(super) fn lead_budget(&self) -> usize {
        self.max_input_bytes
            .saturating_sub(SYSTEM.len() + REVIEW_RESERVE)
    }

    pub(super) fn section_budgets(&self) -> SectionBudgets {
        self.sections.effective()
    }

    pub(super) fn sections_valid(&self) -> bool {
        self.sections.valid(self.lead_budget())
    }
}

pub(super) fn default_timeout() -> u64 {
    180
}

/// Request settings a model config may name, each checked before the coordinator starts: numbers in their
/// documented ranges, an effort and template arguments as short lowercase words or flags. Anything else,
/// including a field the body already holds (`messages`, `model`, `max_tokens`), refuses the config, so a
/// typo cannot reach the model or the record.
pub(super) fn settings_valid(settings: &serde_json::Map<String, Value>) -> bool {
    let word = |v: &Value, max: usize| {
        v.as_str().is_some_and(|s| {
            !s.is_empty()
                && s.len() <= max
                && s.bytes().all(|b| b.is_ascii_lowercase() || b == b'_')
        })
    };
    // Every range is non-negative; a negative zero is refused because PostgreSQL's jsonb keeps no
    // negative zero, so a receipt holding one would not rebuild its request byte for byte.
    let number = |v: &Value, ok: &dyn Fn(f64) -> bool| {
        v.as_f64()
            .is_some_and(|x| x.is_finite() && x.is_sign_positive() && ok(x))
    };
    settings.len() <= 6
        && settings.iter().all(|(key, value)| match key.as_str() {
            "temperature" => number(value, &|x| (0.0..=2.0).contains(&x)),
            "top_p" => number(value, &|x| x > 0.0 && x <= 1.0),
            "min_p" => number(value, &|x| (0.0..1.0).contains(&x)),
            "top_k" => value.as_u64().is_some_and(|k| (1..=1000).contains(&k)),
            "reasoning_effort" => word(value, 16),
            "chat_template_kwargs" => value.as_object().is_some_and(|args| {
                args.len() <= 8
                    && args.iter().all(|(name, arg)| {
                        word(&Value::String(name.clone()), 32)
                            && (arg.is_boolean() || word(arg, 16))
                    })
            }),
            _ => false,
        })
}

pub(super) fn private_bytes(path: &PathBuf, limit: u64) -> Result<Vec<u8>> {
    if !path.is_absolute() {
        bail!("private_file_invalid")
    }
    let f = File::options()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(path)
        .map_err(|_| anyhow!("private_file_unavailable"))?;
    let m = f.metadata().map_err(|_| anyhow!("private_file_invalid"))?;
    if !m.is_file()
        || m.uid() != unsafe { libc::geteuid() }
        || m.mode() & 0o077 != 0
        || m.nlink() != 1
        || m.len() > limit
    {
        bail!("private_file_invalid")
    }
    let mut b = vec![];
    f.take(limit + 1)
        .read_to_end(&mut b)
        .map_err(|_| anyhow!("private_file_unavailable"))?;
    if b.len() as u64 > limit {
        bail!("private_file_invalid")
    };
    Ok(b)
}
