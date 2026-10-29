//! The checks a proposal passes before anything it says is recorded.
use super::*;

#[path = "proposal_admission.rs"]
mod admission;

impl Proposal {
    /// An action cites at most 4 distinct library passages, each one this context showed.
    pub fn validate_literature(&self, shown: &HashSet<String>, unread: &UnreadRefs) -> Result<()> {
        for (n, a) in self.actions.iter().enumerate() {
            if a.literature_refs.len() > LITERATURE_REFS_MAX
                || a.literature_refs.iter().collect::<HashSet<_>>().len() != a.literature_refs.len()
                || a.literature_refs.iter().any(|r| !shown.contains(r))
            {
                // Named like the lessons' refs: this action's faults and every later action's.
                return Err(CodeRefused {
                    code: "proposal_literature_unknown",
                    names: unread.literature_names(&self.actions, n, shown),
                }
                .into());
            }
        }
        Ok(())
    }

    /// Writes each table statement's findings after the lead's own, or refuses the reply. A statement
    /// names a shown table with cells, one to four distinct columns, and its rows: `all` of a table with one to twelve
    /// rows, or one to twelve distinct `#` numbers. A composed finding opens with the lead's words and states as many
    /// rows as eight values allow, fewer when the sentence would pass 600 characters; with the lead's own findings a
    /// decision holds at most sixteen. When the first of several
    /// columns holds a name in every stated row, the name opens its row (`{name}: mean {value}`); otherwise a row reads
    /// `column {value}, column {value}`. Every value is a placeholder bound to its cell, so the lead writes neither a
    /// value nor a binding. A shape fault is `proposal_table_statement_invalid`, a part naming nothing shown is
    /// `proposal_table_statement_unbound`, each named by statement for the next lead call. The opening words are kept
    /// as written, so their limit counts every character.
    pub fn compose_tables(
        &mut self,
        index: &ResultIndex,
        rejected: &HashSet<String>,
    ) -> Result<()> {
        let refused = |code: &'static str, names: Vec<String>| -> anyhow::Error {
            CodeRefused {
                code,
                names: names.into_iter().take(12).map(bounded_name).collect(),
            }
            .into()
        };
        if self.table_statements.len() > TABLE_STATEMENTS_MAX {
            return Err(refused(
                "proposal_table_statement_invalid",
                vec![format!(
                    "table_statements: more than {TABLE_STATEMENTS_MAX}"
                )],
            ));
        }
        let mut faults = Vec::new();
        for (n, s) in self.table_statements.iter().enumerate() {
            let at = n + 1;
            // The words are kept as written, so the limit counts every character, spaces included.
            if s.lead_in.trim().is_empty()
                || s.lead_in.chars().count() > LEAD_IN_MAX
                || s.lead_in.contains(['{', '}', '\0'])
            {
                faults.push(format!("statement {at}: lead_in"));
            }
            let distinct: HashSet<&String> = s.columns.iter().collect();
            if s.columns.is_empty()
                || s.columns.len() > STATED_COLUMNS_MAX
                || distinct.len() != s.columns.len()
            {
                faults.push(format!("statement {at}: columns"));
            }
            let rows_fit = match &s.rows {
                StatedRows::Word(word) => word == "all",
                StatedRows::Numbers(rows) => {
                    let distinct: HashSet<&usize> = rows.iter().collect();
                    !rows.is_empty()
                        && rows.len() <= STATED_ROWS_MAX
                        && distinct.len() == rows.len()
                }
            };
            if !rows_fit {
                faults.push(format!("statement {at}: rows"));
            }
        }
        if !faults.is_empty() {
            return Err(refused("proposal_table_statement_invalid", faults));
        }
        if self
            .table_statements
            .iter()
            .any(|s| ref_task(&s.reference).is_some_and(|t| rejected.contains(t)))
        {
            bail!("proposal_finding_rejected")
        }
        let (mut unbound, mut composed) = (Vec::new(), Vec::new());
        for (n, s) in self.table_statements.iter().enumerate() {
            let at = n + 1;
            // A binding to the statement's table, for the choices its parts had.
            let probe = Binding {
                reference: s.reference.clone(),
                table: s.table.clone(),
                row: 1,
                column: String::new(),
            };
            let choices = |part: &str| index.choices(&probe, part).unwrap_or_default();
            let table = match index.stated_table(&s.reference, &s.table) {
                Ok(table) => table,
                Err(part) => {
                    unbound.push(match index.choices(&probe, part) {
                        Some(choices) => format!("statement {at}: {part} ({choices})"),
                        None => format!("statement {at}: {part}"),
                    });
                    continue;
                }
            };
            let before = unbound.len();
            let mut positions = Vec::new();
            for column in &s.columns {
                match table.columns.iter().position(|c| c == column) {
                    Some(p) => positions.push(p),
                    None => unbound.push(format!(
                        "statement {at}: column {column} ({})",
                        choices("column")
                    )),
                }
            }
            let rows: Vec<usize> = match &s.rows {
                StatedRows::Numbers(rows) => rows.clone(),
                StatedRows::Word(_) => {
                    if !(1..=STATED_ROWS_MAX).contains(&table.cells.len()) {
                        unbound.push(format!(
                            "statement {at}: rows ({}; at most {STATED_ROWS_MAX} in one statement)",
                            choices("row")
                        ));
                    }
                    (1..=table.cells.len()).collect()
                }
            };
            let cells: Vec<&Vec<String>> = rows
                .iter()
                .filter_map(|row| {
                    let cells = row.checked_sub(1).and_then(|r| table.cells.get(r));
                    if cells.is_none_or(|c| positions.iter().any(|&p| p >= c.len())) {
                        unbound.push(format!("statement {at}: row {row} ({})", choices("row")));
                    }
                    cells
                })
                .collect();
            if unbound.len() > before {
                continue;
            }
            let named = positions.len() > 1
                && cells.iter().all(|c| {
                    let first = c[positions[0]].trim();
                    !first.is_empty() && first.parse::<f64>().is_err()
                });
            let lead = s
                .lead_in
                .trim()
                .trim_end_matches(['.', ':', ';', ','])
                .trim_end();
            let phrases: Vec<(String, BTreeMap<String, Binding>)> = rows
                .iter()
                .map(|&row| {
                    let slot = |k: usize| format!("t{at}_r{row}_c{}", k + 1);
                    let bindings = s
                        .columns
                        .iter()
                        .enumerate()
                        .map(|(k, column)| {
                            let binding = Binding {
                                reference: s.reference.clone(),
                                table: s.table.clone(),
                                row,
                                column: column.clone(),
                            };
                            (slot(k), binding)
                        })
                        .collect();
                    let values: Vec<String> = s
                        .columns
                        .iter()
                        .enumerate()
                        .skip(usize::from(named))
                        .map(|(k, column)| format!("{} {{{}}}", column_words(column), slot(k)))
                        .collect();
                    let text = if named {
                        format!("{{{}}}: {}", slot(0), values.join(", "))
                    } else {
                        values.join(", ")
                    };
                    (text, bindings)
                })
                .collect();
            let per = (FINDING_VALUES_MAX / s.columns.len()).max(1);
            let (mut start, mut part) = (0, 0);
            while start < phrases.len() {
                let opening = if part == 0 {
                    lead.to_owned()
                } else {
                    format!("{lead}, continued")
                };
                let mut take = per.min(phrases.len() - start);
                let sentence = loop {
                    let body: Vec<&str> = phrases[start..start + take]
                        .iter()
                        .map(|(text, _)| text.as_str())
                        .collect();
                    let sentence = format!("{opening}: {}.", body.join("; "));
                    if sentence.chars().count() <= FINDING_CHARS_MAX || take == 1 {
                        break sentence;
                    }
                    take -= 1;
                };
                if sentence.chars().count() > FINDING_CHARS_MAX {
                    faults.push(format!("statement {at}: too long"));
                    break;
                }
                composed.push(Finding {
                    statement: sentence,
                    bindings: phrases[start..start + take]
                        .iter()
                        .flat_map(|(_, bindings)| bindings.clone())
                        .collect(),
                    composed_from: Some(at),
                });
                start += take;
                part += 1;
            }
        }
        if !unbound.is_empty() {
            return Err(refused("proposal_table_statement_unbound", unbound));
        }
        if !faults.is_empty() {
            return Err(refused("proposal_table_statement_invalid", faults));
        }
        if self.findings.len() + composed.len() > ALL_FINDINGS_MAX {
            return Err(findings_too_many(self.findings.len(), composed.len()).into());
        }
        self.findings.extend(composed);
        Ok(())
    }

    /// Checks each answer table and writes its row count, or refuses the reply. An answer table names a
    /// table its call was shown, with or without the table's cells, or one an earlier answer gave; one to six distinct
    /// columns of it; and the plain words that open it, held to a table statement's rules. A template analysis's table
    /// may appear in several answer tables with different columns, such as its values per 100 g and per portion (test
    /// runs gave the reviewed normalization's table that way and were refused); any other result's
    /// table appears once, since its query or code chose its columns. The same table with the same columns
    /// twice, or a table with no rows, is refused. Its values never pass through the lead: the workspace shows every row from the
    /// retained result. A shape fault is `proposal_table_invalid` and a part naming nothing shown
    /// `proposal_table_unbound`, each named by table for the next lead call.
    pub fn check_tables(&mut self, index: &ResultIndex, rejected: &HashSet<String>) -> Result<()> {
        let refused = |code: &'static str, names: Vec<String>| -> anyhow::Error {
            CodeRefused {
                code,
                names: names.into_iter().take(12).map(bounded_name).collect(),
            }
            .into()
        };
        if self.tables.len() > ANSWER_TABLES_MAX {
            return Err(refused(
                "proposal_table_invalid",
                vec![format!("tables: more than {ANSWER_TABLES_MAX}")],
            ));
        }
        let mut faults = Vec::new();
        let mut named = HashSet::new();
        for (n, t) in self.tables.iter().enumerate() {
            let at = n + 1;
            // The words are kept as written, so the limit counts every character, spaces included.
            if t.lead_in.trim().is_empty()
                || t.lead_in.chars().count() > LEAD_IN_MAX
                || t.lead_in.contains(['{', '}', '\0'])
            {
                faults.push(format!("table {at}: lead_in"));
            }
            let distinct: HashSet<&String> = t.columns.iter().collect();
            if t.columns.is_empty()
                || t.columns.len() > ANSWER_COLUMNS_MAX
                || distinct.len() != t.columns.len()
            {
                faults.push(format!("table {at}: columns"));
            }
            let mut shown_columns: Vec<&str> = if index.from_analysis(&t.reference) {
                t.columns.iter().map(String::as_str).collect()
            } else {
                Vec::new()
            };
            shown_columns.sort_unstable();
            if !named.insert((t.reference.as_str(), t.table.as_str(), shown_columns)) {
                faults.push(format!("table {at}: named twice"));
            }
        }
        if !faults.is_empty() {
            return Err(refused("proposal_table_invalid", faults));
        }
        if self
            .tables
            .iter()
            .any(|t| ref_task(&t.reference).is_some_and(|task| rejected.contains(task)))
        {
            bail!("proposal_finding_rejected")
        }
        let mut unbound = Vec::new();
        for (n, t) in self.tables.iter_mut().enumerate() {
            let at = n + 1;
            let failed = index
                .withheld
                .get(&(t.reference.clone(), t.table.clone()))
                .is_some_and(|why| why == step_checks::CHECK_FAILED);
            match index.table_shape(&t.reference, &t.table, &t.columns) {
                // A result that failed its step's check is no part of an answer.
                Ok(_) if failed => unbound.push(format!("table {at}: check failed")),
                Ok(shape) if shape.rows == 0 => faults.push(format!("table {at}: no rows")),
                Ok(shape) => t.rows = Some(shape.rows),
                Err(part) => unbound.push(format!("table {at}: {part}")),
            }
        }
        if !unbound.is_empty() {
            return Err(refused("proposal_table_unbound", unbound));
        }
        if !faults.is_empty() {
            return Err(refused("proposal_table_invalid", faults));
        }
        Ok(())
    }

    /// The answerability ledger: each part of the question supported (naming, when it does,
    /// the lead's own findings or table statements that state it), partial (stated for some of its items, with a reason
    /// naming what the data does not hold), unsupported with a reason, open, or awaiting a person's choice, and the
    /// answer follows from them: `answered` when every part
    /// is supported, `partly_answered` when at least one is supported or partial but not every one is supported,
    /// `not_answered` when every part is unsupported. The ledger is required in every reply but a delegation that answers
    /// `not_yet`, and a part stays open only in a reply that proposes a step: one suite goal stopped at its first call
    /// with `not_answered` because one of its three parts needed data the source does not hold. A reply may still stop
    /// on a part awaiting a choice only a person can make, which names that choice. Runs
    /// after the findings checks, so their faults come first; `written` counts the lead's own findings, the ones a
    /// finding number may name, before the table statements' sentences follow them.
    pub fn validate_parts(&self, written: usize) -> Result<()> {
        let mut names = Vec::new();
        if self.parts.len() > PARTS_MAX {
            names.push(format!(
                "parts: {} of at most {PARTS_MAX}",
                self.parts.len()
            ));
        }
        let stops = self.action != "delegate";
        let evidence = written + self.table_statements.len() + self.tables.len();
        if self.parts.is_empty() && (stops || self.answer != Answer::NotYet) {
            names.push("parts: none, though the reply needs them".to_owned());
        }
        for (i, part) in self.parts.iter().enumerate() {
            let n = i + 1;
            if let Some(fault) = text_fault(Some(&part.part), PART_CHARS) {
                names.push(format!("part {n}: words {fault}"));
            }
            // A partial or unsupported part needs its reason; any reason given is held to the same limit.
            let needs_reason = matches!(
                part.state,
                PartState::Partial | PartState::Unsupported | PartState::Awaiting
            );
            if (needs_reason || part.reason.is_some())
                && let Some(fault) = text_fault(part.reason.as_deref(), PART_REASON_CHARS)
            {
                names.push(format!("part {n}: reason {fault}"));
            }
            // A supported or partial part need not name its findings (in one suite both ledger
            // refusals were such parts, each on a third call that then paused its run), but the reply must state at
            // least one, whatever its answer (a `not_yet` reply could call a part answered on
            // nothing).
            match part.state {
                PartState::Supported | PartState::Partial if evidence == 0 => {
                    names.push(format!(
                        "part {n}: {}, though the reply states no finding, table statement or table",
                        if part.state == PartState::Supported {
                            "supported"
                        } else {
                            "partial"
                        }
                    ));
                }
                PartState::Open if stops => {
                    names.push(format!("part {n}: open, though the reply proposes no step"));
                }
                _ => {}
            }
            for &f in &part.findings {
                if f == 0 || f > written {
                    names.push(format!("part {n}: finding {f} (you wrote {written})"));
                }
            }
            for &s in &part.table_statements {
                if s == 0 || s > self.table_statements.len() {
                    names.push(format!(
                        "part {n}: table statement {s} (you wrote {})",
                        self.table_statements.len()
                    ));
                }
            }
            for &t in &part.tables {
                if t == 0 || t > self.tables.len() {
                    names.push(format!(
                        "part {n}: table {t} (you gave {})",
                        self.tables.len()
                    ));
                }
            }
        }
        let count = |state: PartState| self.parts.iter().filter(|p| p.state == state).count();
        let total = self.parts.len();
        let (supported, partial, unsupported) = (
            count(PartState::Supported),
            count(PartState::Partial),
            count(PartState::Unsupported),
        );
        let (word, follows) = match self.answer {
            Answer::Answered => ("answered", supported == total),
            Answer::PartlyAnswered => (
                "partly_answered",
                supported + partial > 0 && supported < total,
            ),
            Answer::NotAnswered => ("not_answered", unsupported == total),
            Answer::NotYet => ("not_yet", true),
        };
        if total > 0 && !follows {
            names.push(format!(
                "answer {word}: {supported} of {total} parts supported, {partial} partial, {unsupported} unsupported"
            ));
        }
        if names.is_empty() {
            return Ok(());
        }
        names.truncate(PARTS_NAMES_MAX);
        Err(CodeRefused {
            code: "proposal_parts_invalid",
            names,
        }
        .into())
    }

    /// Every placeholder in a finding is bound to exactly one cell of a table the lead was shown,
    /// and an answer claim rests on at least one finding.
    pub fn validate_findings(&self, index: &ResultIndex, rejected: &HashSet<String>) -> Result<()> {
        let written = self
            .findings
            .iter()
            .filter(|f| f.composed_from.is_none())
            .count();
        if written > FINDINGS_MAX || self.findings.len() > ALL_FINDINGS_MAX {
            return Err(findings_too_many(written, self.findings.len() - written).into());
        }
        if self.findings.is_empty()
            && self.tables.is_empty()
            && matches!(self.answer, Answer::Answered | Answer::PartlyAnswered)
        {
            bail!("proposal_answer_needs_finding")
        }
        // Each broken limit has its own code, so the next lead call learns which rule to meet.
        for f in &self.findings {
            let Some(used) = placeholders(&f.statement) else {
                bail!("proposal_finding_placeholder_invalid")
            };
            if !clean_text(&f.statement, usize::MAX) {
                bail!("proposal_finding_invalid")
            }
            if f.statement.chars().count() > FINDING_CHARS_MAX {
                bail!("proposal_finding_too_long")
            }
            if used.is_empty() {
                bail!("proposal_finding_no_values")
            }
            if used.len() > FINDING_VALUES_MAX {
                bail!("proposal_finding_too_many_values")
            }
            if f.bindings
                .values()
                .any(|b| ref_task(&b.reference).is_some_and(|t| rejected.contains(t)))
            {
                bail!("proposal_finding_rejected")
            }
            // The next lead call learns which placeholders to repair, what is wrong with each, and for a table, column or
            // row the choices it had. Test runs paused after three unbound findings each when the names alone
            // were given, and two were refused as unbound again when the part at fault was named.
            let faults = binding_faults(f, &used, index);
            if !faults.is_empty() {
                return Err(CodeRefused {
                    code: "proposal_finding_unbound",
                    names: faults
                        .into_iter()
                        .take(12)
                        .map(|(name, why)| {
                            let choices = f.bindings.get(name).and_then(|b| index.choices(b, why));
                            let named = match choices {
                                Some(choices) => format!("{name}: {why} ({choices})"),
                                None => format!("{name}: {why}"),
                            };
                            bounded_name(named)
                        })
                        .collect(),
                }
                .into());
            }
        }
        Ok(())
    }

    /// Writes each number in the summary that the prose rule refuses as `(a number)`, so the
    /// summary keeps every sentence and qualification but no unsourced value, and the turn goes on instead of spending
    /// a lead call on the refusal. Returns how many it replaced; a summary the mask would take past its 2,048-byte
    /// limit stays whole and is refused, and a number inside a name (B12, IL-6) is refused rather than garbled.
    /// Titles, rationales, findings and lessons keep the refusal: a masked title can lose what it names, and the
    /// methods review reads a rationale's conditions as written (source review).
    pub fn mask_summary_numbers(&mut self, index: &ResultIndex) -> usize {
        let (masked, count) = mask_refused_numbers(&self.summary, index);
        if count == 0 || masked.len() > SUMMARY_MAX {
            return 0;
        }
        self.summary = masked;
        count
    }

    /// Each step's title and rationale with every standalone number the prose rule refuses written as `(a number)`
    /// (`ResultIndex::mask_step_numbers`, with the step's own SQL literals allowed), and how many in all. A text the mask
    /// would take past its admitted length keeps its numbers, so the rule refuses them.
    pub fn mask_step_numbers(&mut self, index: &ResultIndex) -> usize {
        let mut total = 0;
        for action in &mut self.actions {
            let own = step_literals::step_own_numbers(action);
            for (text, max) in [(&mut action.title, 512), (&mut action.rationale, 2048)] {
                let (masked, count) = index.mask_step_numbers(text, Some(&own));
                if count > 0 && masked.len() <= max {
                    *text = masked;
                    total += count;
                }
            }
        }
        total
    }

    /// No number in the lead's prose unless the question, constraints or method descriptions
    /// hold it; result values appear only through bound placeholders. A step's title and rationale may also state the
    /// numbers that step's own SQL sets in a comparison or an `IN` list (`step_literals`), and only its own. A refusal
    /// names each field and number at fault for the next lead call alone, never a log line or run event, since a
    /// number in prose may be a result value.
    pub fn validate_prose(&self, index: &ResultIndex) -> Result<()> {
        // A composed finding's only words from the lead are its opening, so a number there is named by that field.
        let findings: Vec<(&'static str, String)> = self
            .findings
            .iter()
            .map(|f| {
                let field = match f.composed_from {
                    Some(_) => "table_statements.lead_in",
                    None => "findings.statement",
                };
                (field, without_placeholders(&f.statement, " "))
            })
            .collect();
        let own: Vec<HashSet<String>> = self
            .actions
            .iter()
            .map(step_literals::step_own_numbers)
            .collect();
        let texts = std::iter::once(("summary", self.summary.as_str(), None))
            .chain(self.actions.iter().zip(&own).flat_map(|(a, own)| {
                [
                    ("actions.title", a.title.as_str(), Some(own)),
                    ("actions.rationale", a.rationale.as_str(), Some(own)),
                ]
            }))
            .chain(
                self.lessons
                    .iter()
                    .map(|l| ("lessons.statement", l.statement.as_str(), None)),
            )
            .chain(self.parts.iter().flat_map(|p| {
                std::iter::once(("parts.part", p.part.as_str(), None))
                    .chain(p.reason.as_deref().map(|r| ("parts.reason", r, None)))
            }))
            .chain(findings.iter().map(|(field, f)| (*field, f.as_str(), None)))
            .chain(
                self.tables
                    .iter()
                    .map(|t| ("tables.lead_in", t.lead_in.as_str(), None)),
            );
        let mut names = refused_numbers_with(index, texts);
        // Where each bound value would stand: joined to anything that makes it part of another value, it is refused.
        for f in &self.findings {
            let field = match f.composed_from {
                Some(_) => "table_statements.lead_in",
                None => "findings.statement",
            };
            names.extend(
                index
                    .placements_refused(&with_placeholder_values(&f.statement))
                    .into_iter()
                    .map(|token| format!("{field}: {token}")),
            );
        }
        if names.is_empty() {
            Ok(())
        } else {
            Err(CodeRefused {
                code: "proposal_prose_number",
                names,
            }
            .into())
        }
    }
}
