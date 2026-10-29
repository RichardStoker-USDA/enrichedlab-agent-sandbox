//! The index of the result tables a call was shown, which binds every value to a retained cell.
use super::*;

/// The most cells an offline context rebuilds. This bounds reconstruction work independently of the configured
/// model input bytes; every cell still has to fit in the serialized context.
const MAX_CONTEXT_CELLS: usize = 65_536;

/// A result table the lead was shown with its cells, keyed by result reference and table path.
#[derive(Debug, Clone, PartialEq)]
pub(super) struct InlineTable {
    pub(super) columns: Vec<String>,
    pub(super) cells: Vec<Vec<String>>,
}

/// A result table's columns and row count as the call was shown them, with or without its cells: what an answer table
/// may name.
#[derive(Debug, Clone, PartialEq)]
pub(super) struct TableShape {
    pub(super) columns: Vec<String>,
    pub(super) rows: u64,
}

/// What a lead call was shown: the inline tables its findings may bind to, and the
/// numbers its prose may contain.
#[derive(Debug, Default, Clone, PartialEq)]
pub struct ResultIndex {
    pub(super) tables: HashMap<(String, String), InlineTable>,
    /// Tables shown without their cells, with the digest's reason.
    pub(super) withheld: HashMap<(String, String), String>,
    pub(super) allowed_numbers: HashSet<String>,
    /// Plain names holding a number (Fmo3, Cyp7a1, IL-6) that the source material of the call being checked writes
    /// (`with_names_from`): a number inside such a name may stand in prose. Empty in an index as compiled or rebuilt;
    /// filled only from the exact context a call sent, at the moment its reply is checked.
    pub(super) allowed_names: HashSet<String>,
    /// Numbers the shown template analyses' words hold (labels, first sentences, setting ranges): words that describe a
    /// step may name a setting with them, but no summary, finding, lesson or part may, so a catalog's bound that equals
    /// a result value never stands in a finding unbound.
    pub(super) setting_numbers: HashSet<String>,
    /// Every result the call was shown, with tables or without them (figures, settings, a digest with no tables), so a
    /// methods check finding may cite it.
    pub(super) shown: HashSet<String>,
    /// Each cell an earlier finding shown in this call binds to, with the value the call shows for it, by ref, table, row
    /// and column. A later reply may restate that finding with the same cells even when the budget left their table out:
    /// in a test run each call showed one table, and restated earlier findings were refused as
    /// unbound in three of five refused replies.
    pub(super) earlier: HashMap<(String, String, usize, String), String>,
    /// Results the call names (an analysis's `result_refs`, `results_omitted`) without showing them, so a binding or table
    /// statement that names one is told the result was left out for room rather than that it is unknown.
    pub(super) listed: HashSet<String>,
    /// Every table the call shows, by ref and path, with or without its cells, and each table an earlier answer gave
    ///: an answer table names one of them.
    pub(super) shapes: HashMap<(String, String), TableShape>,
    /// The run's template-analysis steps by task: only their results enter a call with cells withheld for
    /// room (`result_fit`), and only their tables may stand in several answer tables with different columns.
    pub(super) analysis_tasks: HashSet<String>,
}

impl ResultIndex {
    /// Whether a result reference names one of the run's template-analysis steps.
    pub fn from_analysis(&self, reference: &str) -> bool {
        ref_task(reference).is_some_and(|task| self.analysis_tasks.contains(task))
    }
    /// Whether a result with this ref was shown, whatever it holds.
    pub fn shows(&self, reference: &str) -> bool {
        self.shown.contains(reference)
    }
    /// The cell a binding names, as written in the retained file: from a table the call shows, or one an earlier finding
    /// shows.
    pub fn cell(&self, b: &Binding) -> Option<&str> {
        let shown = || {
            let table = self.tables.get(&(b.reference.clone(), b.table.clone()))?;
            let column = table.columns.iter().position(|c| c == &b.column)?;
            let row = table.cells.get(b.row.checked_sub(1)?)?;
            row.get(column).map(String::as_str)
        };
        shown().or_else(|| self.earlier.get(&earlier_key(b)).map(String::as_str))
    }

    /// The shown table a table statement names, or which part of it names none: `ref`, `no cells` or `table`, as for a
    /// binding.
    pub(super) fn stated_table(
        &self,
        reference: &str,
        table: &str,
    ) -> std::result::Result<&InlineTable, &'static str> {
        let key = (reference.to_owned(), table.to_owned());
        if !self
            .tables
            .keys()
            .chain(self.withheld.keys())
            .any(|(r, _)| r == reference)
        {
            return Err("ref");
        }
        if self.withheld.contains_key(&key) {
            return Err("no cells");
        }
        self.tables.get(&key).ok_or("table")
    }

    /// Which part of a binding names no cell this call was shown: `ref` (no shown result has it), `no cells` (that table
    /// was shown without its cells), `table` (that result shows no such table), `column` or `row`. None when it names a
    /// cell.
    pub fn miss(&self, b: &Binding) -> Option<&'static str> {
        if self.earlier.contains_key(&earlier_key(b)) {
            return None;
        }
        let key = (b.reference.clone(), b.table.clone());
        if !self
            .tables
            .keys()
            .chain(self.withheld.keys())
            .any(|(r, _)| r == &b.reference)
        {
            return Some("ref");
        }
        if self.withheld.contains_key(&key) {
            return Some("no cells");
        }
        let Some(table) = self.tables.get(&key) else {
            return Some("table");
        };
        let Some(column) = table.columns.iter().position(|c| c == &b.column) else {
            return Some("column");
        };
        match b
            .row
            .checked_sub(1)
            .and_then(|r| table.cells.get(r))
            .and_then(|row| row.get(column))
        {
            Some(_) => None,
            None => Some("row"),
        }
    }

    /// The choices a binding's faulty part had in this call, in plain words for the next lead call: the result's
    /// tables with cells, the table's columns or its numbered rows. None for any other part.
    pub fn choices(&self, b: &Binding, part: &str) -> Option<String> {
        let table = self.tables.get(&(b.reference.clone(), b.table.clone()));
        match part {
            "table" => {
                let mut tables: Vec<&str> = self
                    .tables
                    .keys()
                    .filter(|(r, _)| r == &b.reference)
                    .map(|(_, t)| t.as_str())
                    .collect();
                tables.sort_unstable();
                Some(format!("its result's tables: {}", listed(&tables, 4)))
            }
            "column" => {
                let columns: Vec<&str> = table?.columns.iter().map(String::as_str).collect();
                Some(format!("its table's columns: {}", listed(&columns, 16)))
            }
            "row" => Some(match table?.cells.len() {
                0 => "its table shows no rows".to_owned(),
                n => format!("its table's rows: 1 to {n}"),
            }),
            "no cells" => self
                .withheld
                .get(&(b.reference.clone(), b.table.clone()))
                .map(|why| format!("its values were not shown: {}", withheld_words(why))),
            "ref" if self.listed.contains(&b.reference) => Some(
                "its result is not shown in this call, left out for room: use a table shown with cells, or a value an \
                 earlier finding shows"
                    .to_owned(),
            ),
            _ => None,
        }
    }

    /// The shape of the table an answer table names, or which part names nothing shown with the choices it
    /// had, in plain words for the next lead call: `ref` (left out for room, or no shown result has it), `table` (with the
    /// result's tables) or `column` (with the table's columns).
    pub(super) fn table_shape(
        &self,
        reference: &str,
        table: &str,
        columns: &[String],
    ) -> std::result::Result<&TableShape, String> {
        let key = (reference.to_owned(), table.to_owned());
        let Some(shape) = self.shapes.get(&key) else {
            let mut tables: Vec<&str> = self
                .shapes
                .keys()
                .filter(|(r, _)| r == reference)
                .map(|(_, t)| t.as_str())
                .collect();
            if tables.is_empty() {
                return Err(if self.listed.contains(reference) {
                    "ref (its result is not shown in this call, left out for room)".to_owned()
                } else {
                    "ref".to_owned()
                });
            }
            tables.sort_unstable();
            return Err(format!(
                "table (its result's tables: {})",
                listed(&tables, 4)
            ));
        };
        if let Some(column) = columns.iter().find(|c| !shape.columns.contains(c)) {
            let names: Vec<&str> = shape.columns.iter().map(String::as_str).collect();
            return Err(format!(
                "column {column} (its table's columns: {})",
                listed(&names, 16)
            ));
        }
        Ok(shape)
    }

    /// Structural counts of what the unbound bindings got wrong, for the log: never a name, cell or value.
    pub(super) fn near_misses(&self, b: &Binding, part: &str) -> [bool; 4] {
        let prefixed = b.reference.starts_with("artifact://tasks/");
        let task_shown = ref_task(&b.reference)
            .is_some_and(|t| self.tables.keys().any(|(r, _)| ref_task(r) == Some(t)));
        let column_case = part == "column"
            && self
                .tables
                .get(&(b.reference.clone(), b.table.clone()))
                .is_some_and(|t| t.columns.iter().any(|c| c.eq_ignore_ascii_case(&b.column)));
        [
            prefixed && part == "ref",
            task_shown && part == "ref",
            column_case,
            part == "row" && b.row == 0,
        ]
    }

    pub fn prose_allowed(&self, text: &str) -> bool {
        self.refused_in(text, false, None).is_empty()
    }

    /// The numbers in `text` that prose may not hold.
    pub fn prose_refused(&self, text: &str) -> Vec<String> {
        self.refused_in(text, false, None)
    }

    /// The numbers in `text` that words describing a step (a step's title and rationale, the methods check's reason
    /// and finding details) may not hold: a shown analysis's setting numbers may stand there too.
    pub fn step_prose_refused(&self, text: &str) -> Vec<String> {
        self.refused_in(text, true, None)
    }

    /// `step_prose_refused` for one step's own title and rationale: a number that step's SQL sets in a comparison or an
    /// `IN` list (`step_literals`) may also stand where its whole token writes that literal exactly
    /// (`literal_occurrence`: everything up to whitespace or a prose separator, a letter, a mark, an invisible
    /// character, `^`, `_`, `/`, a sign or a point included, only a sentence-final `.` or `:` dropped). So literals
    /// cannot compose a number or a name the SQL never wrote, such as `1e-9`, `10^-9`, `A7/B3`, `.7` or `-7` for `7`.
    pub(super) fn step_prose_refused_own(&self, text: &str, own: &HashSet<String>) -> Vec<String> {
        self.refused_in(text, true, Some(own))
    }

    pub(super) fn refused_in(
        &self,
        text: &str,
        step: bool,
        own: Option<&HashSet<String>>,
    ) -> Vec<String> {
        let chars: Vec<char> = text.chars().collect();
        // A numeral outside ASCII (a full-width or Arabic-Indic digit, a superscript, a vulgar fraction) is never read
        // as a number below, so it is refused as itself.
        let other_numerals = chars
            .iter()
            .filter(|c| c.is_numeric() && !c.is_ascii_digit())
            .map(|c| c.to_string());
        // A direction control is refused as itself, digits or not: a value inserted later in its place (a finding's
        // bound cell in a document) would display in the order it sets.
        let directions = chars
            .iter()
            .filter(|&&c| directs_text(c))
            .map(|&c| format!("U+{:04X}", u32::from(c)));
        number_spans(&chars)
            .into_iter()
            .filter(|&(start, end)| self.number_refused(&chars, start, end, step, own))
            .map(|(start, end)| chars[start..end].iter().collect())
            .chain(other_numerals)
            .chain(directions)
            .collect()
    }

    /// The rules that refuse a number for what surrounds it, before whether the number itself is shown: `Some(true)`
    /// when the text sets its display direction or the token holds a hidden or digit-changing character (`\u{202E} 37`,
    /// `1\u{200B}9`), `Some(verdict)` when its operand must be shown whole (`needs_whole_token`), `None` otherwise. No
    /// number allowed on its own, no step setting, no step literal and no bound value can admit `A7/B3`, `Fmo3-1`,
    /// `Gene3%` or `p=1e-9` piece by piece.
    fn composition_refused(&self, chars: &[char], start: usize, end: usize) -> Option<bool> {
        if chars.iter().copied().any(directs_text)
            || obscures_numbers(&raw_token(chars, start, end))
        {
            return Some(true);
        }
        let operand = operand_around(chars, start, end);
        needs_whole_token(&operand).then(|| {
            let verbatim =
                operand.is_ascii() && self.allowed_names.contains(&operand.to_ascii_lowercase());
            !(verbatim || shown_name(&operand, &self.allowed_names))
        })
    }

    /// The places a finding's bound values would stand, refused: `expanded` is the statement with each placeholder
    /// written as `PLACEHOLDER_VALUE` (`with_placeholder_values`). A bound value stands only as its whole operand, in a
    /// text with no direction control and a token with no hidden character; a unit or a percent sign follows it after a
    /// space (`{a} mg`, `{a} %`). A cell is untyped text, so anything joined to it could make another value of it
    /// (`.{a}`, `-{a}`, `{a}h` on a cell `0FF`, `{a}{b}`, `{a}e-{b}`, `{a} {b}`, `Fmo{a}`), and the document never writes
    /// that. No shown number or name is consulted, so the marker can match nothing a source shows. Each refusal names
    /// its token with `{}` for each placeholder.
    pub(super) fn placements_refused(&self, expanded: &str) -> Vec<String> {
        let chars: Vec<char> = expanded.chars().collect();
        let directed = chars.iter().copied().any(directs_text);
        number_spans(&chars)
            .into_iter()
            .filter(|&(start, end)| {
                let operand = operand_around(&chars, start, end);
                operand.contains(PLACEHOLDER_VALUE)
                    && (chars[start..end].iter().collect::<String>() != PLACEHOLDER_VALUE
                        || directed
                        || obscures_numbers(&raw_token(&chars, start, end))
                        || operand != PLACEHOLDER_VALUE)
            })
            .map(|(start, end)| {
                without_directions(
                    operand_around(&chars, start, end).replace(PLACEHOLDER_VALUE, "{}"),
                )
                .0
            })
            .collect()
    }

    /// Whether the prose rule refuses the number at `start..end` of `chars`: neither shown (a result's number, a shown
    /// analysis's setting in step words, or a step's own SQL literal written as a whole token) nor inside a name the
    /// call shows (`shown_name`).
    pub(super) fn number_refused(
        &self,
        chars: &[char],
        start: usize,
        end: usize,
        step: bool,
        own: Option<&HashSet<String>>,
    ) -> bool {
        if let Some(refused) = self.composition_refused(chars, start, end) {
            return refused;
        }
        // A point before the digits (`.5`) writes another value than the digits alone, so only a step literal written
        // that way admits it.
        let pointed = start > 0 && chars[start - 1] == '.';
        let n: String = chars[start..end].iter().collect();
        !((!pointed
            && (self.allowed_numbers.contains(&n) || (step && self.setting_numbers.contains(&n))))
            || own.is_some_and(|own| own.contains(&literal_occurrence(chars, start, end))))
    }

    /// `text` describing a step (`step_prose_refused`, with `own` for a step's own title and rationale) with each
    /// standalone number the rule refuses written as `NUMBER_MASK`, and how many, as the summary's are
    /// (`mask_refused_numbers`). A number inside a name stays, so the rule still refuses it unless the call shows the
    /// name. Masked, a number the model typed never reaches the record, and the turn goes on (a methods
    /// check's reason stating a paper's chromosome number cost a lab test run its turn).
    pub(super) fn mask_step_numbers(
        &self,
        text: &str,
        own: Option<&HashSet<String>>,
    ) -> (String, usize) {
        let chars: Vec<char> = text.chars().collect();
        let (mut out, mut masked, mut last) = (String::new(), 0, 0);
        for (start, end) in number_spans(&chars) {
            // A number inside a name or an operand shown only whole stays for the rule to refuse (`may_mask`).
            if !may_mask(&chars, start, end) || !self.number_refused(&chars, start, end, true, own)
            {
                continue;
            }
            out.extend(&chars[last..start]);
            out.push_str(NUMBER_MASK);
            masked += 1;
            last = end;
        }
        out.extend(&chars[last..]);
        let (out, dropped) = without_directions(out);
        (out, masked + dropped)
    }

    /// This index with the plain names the source material of `context` writes (`allowed_names`), for checking the reply
    /// to exactly that call. The sections are fixed: the question, the scientist's constraints and instructions, the
    /// methods, the data cards, the starting point, the lab's paper passages, and the paper-written texts of the parts
    /// the lead read (a section's text, a figure's or table's caption, an outline's abstract) or, for the methods check,
    /// of the parts the proposal cites. Nothing the lead or a model wrote adds a name (answers, lessons, findings, claims,
    /// readings, attempts, step titles, the tool card's action keys, a figure's model description), and neither do
    /// result cells, which a query can fill with any text, nor a study part's extracted facts.
    pub fn with_names_from(&self, context: &Value) -> ResultIndex {
        fn collect(value: &Value, names: &mut HashSet<String>) {
            match value {
                Value::String(text) => names.extend(shown_name_keys(text)),
                Value::Array(items) => items.iter().for_each(|v| collect(v, names)),
                Value::Object(fields) => fields.values().for_each(|v| collect(v, names)),
                _ => {}
            }
        }
        let mut names = HashSet::new();
        for key in [
            "question",
            "constraints",
            "scientist_instructions",
            "methods",
            "data_cards",
            "starting_point",
        ] {
            collect(&context[key], &mut names);
        }
        for passage in context["literature"].as_array().into_iter().flatten() {
            collect(&passage["text"], &mut names);
        }
        for part in context["reading"]["parts"].as_array().into_iter().flatten() {
            for key in ["text", "caption", "abstract"] {
                collect(&part[key], &mut names);
            }
        }
        // The methods check is not sent `reading`; it is sent the cited parts, each text cut from one of those three
        // (`reading_review::cited`), except a study part's, which is its facts.
        for part in context["reading_cited"].as_array().into_iter().flatten() {
            if part["part"] != "study" {
                collect(&part["text"], &mut names);
            }
        }
        let mut index = self.clone();
        index.allowed_names = names;
        index
    }

    pub(super) fn allow_from(&mut self, text: &str) {
        self.allowed_numbers.extend(numbers(text));
    }

    /// The setting numbers of the template analyses `methods` shows, read from the methods as the context holds them,
    /// so the index rebuilt from a kept context (`from_context`) reads the same ones.
    pub(super) fn allow_settings(&mut self, methods: &Value) {
        fn allow_value(value: &Value, allowed: &mut HashSet<String>) {
            match value {
                Value::String(text) => allowed.extend(numbers(text)),
                Value::Number(number) => allowed.extend(numbers(&number.to_string())),
                Value::Array(items) => {
                    for item in items {
                        allow_value(item, allowed);
                    }
                }
                Value::Object(fields) => {
                    for (name, value) in fields {
                        allowed.extend(numbers(name));
                        allow_value(value, allowed);
                    }
                }
                Value::Null | Value::Bool(_) => {}
            }
        }
        for method in methods.as_array().into_iter().flatten() {
            for a in method["analyses"].as_array().into_iter().flatten() {
                for text in ["label", "about"] {
                    self.setting_numbers
                        .extend(numbers(a[text].as_str().unwrap_or_default()));
                }
                // Recorded contexts may use the compact settings string; current calls hold a structured input
                // contract.
                if let Some(settings) = a.get("settings") {
                    allow_value(settings, &mut self.setting_numbers);
                }
                if let Some(settings) = a.pointer("/inputs/settings") {
                    allow_value(settings, &mut self.setting_numbers);
                }
            }
        }
    }

    /// The index and the rejected analyses a kept context implies, with the methods it was compiled
    /// from: the numbers the question, constraints, instructions, methods and shown columns allow, and each shown
    /// table's cells, read back from the numbered rows the lead was shown. It equals what `compile_context` built for
    /// that context (a test pins this), a table with a column named `#` included: neither indexes its cells.
    /// A context no lead call could have sent, or one whose tables cannot be rebuilt, is refused before any table is
    /// rebuilt: one whose serialized bytes plus `SYSTEM` exceed a call's input ceiling, a shown row that is not an
    /// object, a shown table that names a column twice, or more than `MAX_CONTEXT_CELLS` shown cells. With distinct
    /// columns every value copied is one the context holds once.
    pub fn from_context(
        context: &Value,
        methods: &Value,
    ) -> std::result::Result<(ResultIndex, HashSet<String>), &'static str> {
        Self::from_context_with_ceiling(context, methods, MAX_INPUT_BYTES)
    }

    /// Rebuild an index under the input ceiling recorded for this lead call. The serialized context and the same
    /// fixed lead system text sent by the live path must fit together. The standalone reply checker can read calls
    /// made with any supported configuration, while callers without a recorded ceiling use the broker-wide maximum.
    pub(super) fn from_context_with_ceiling(
        context: &Value,
        methods: &Value,
        input_ceiling: usize,
    ) -> std::result::Result<(ResultIndex, HashSet<String>), &'static str> {
        let size = serde_json::to_vec(context).map_or(usize::MAX, |bytes| bytes.len());
        let context_ceiling = input_ceiling
            .min(MAX_INPUT_BYTES)
            .saturating_sub(lead_system_bytes());
        if size > context_ceiling {
            return Err("context_too_large");
        }
        let mut cells = 0usize;
        for shown in context["analyses"]
            .as_array()
            .into_iter()
            .flatten()
            .flat_map(|a| a["results"].as_array().into_iter().flatten())
        {
            for t in shown["tables"].as_array().into_iter().flatten() {
                let Some(rows) = t["cells"].as_array() else {
                    continue;
                };
                // A row is an object keyed by column name, so a repeated name shows one value and would be read back
                // once per repeat: such a table cannot be rebuilt as it was indexed, and repeats could copy one large
                // value far past the context's size.
                let columns: Vec<&str> = t["columns"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter_map(Value::as_str)
                    .collect();
                let distinct: HashSet<&str> = columns.iter().copied().collect();
                if !rows.iter().all(Value::is_object) || distinct.len() != columns.len() {
                    return Err("context_cells_invalid");
                }
                cells = cells.saturating_add(rows.len().saturating_mul(columns.len()));
                if cells > MAX_CONTEXT_CELLS {
                    return Err("context_cells_invalid");
                }
            }
        }
        let mut index = ResultIndex::default();
        index.allow_from(context["question"].as_str().unwrap_or_default());
        for field in ["constraints", "scientist_instructions"] {
            for c in context[field].as_array().into_iter().flatten() {
                index.allow_from(c.as_str().unwrap_or_default());
            }
        }
        for p in methods.as_array().into_iter().flatten() {
            index.allow_from(p["name"].as_str().unwrap_or_default());
            index.allow_from(p["description"].as_str().unwrap_or_default());
            if let Some(Ok(c)) = Configuration::from_contract(&p["template"]["contract"]) {
                for label in c.labels() {
                    index.allow_from(label);
                }
            }
        }
        index.allow_settings(&context["methods"]);
        let mut rejected = HashSet::new();
        for a in context["analyses"].as_array().into_iter().flatten() {
            if a.get("results_withheld").is_some()
                && let Some(task) = a["analysis_id"].as_str()
            {
                rejected.insert(task.to_owned());
            }
            for shown in a["results"].as_array().into_iter().flatten() {
                index.add(
                    shown["ref"].as_str().unwrap_or_default(),
                    &shown_tables(shown),
                );
                for t in shown["tables"].as_array().into_iter().flatten() {
                    for c in t["columns"].as_array().into_iter().flatten() {
                        index.allow_from(c.as_str().unwrap_or_default());
                    }
                }
            }
        }
        index.note_context(context);
        Ok((index, rejected))
    }

    pub(super) fn add(&mut self, reference: &str, digest: &Value) {
        self.shown.insert(reference.to_owned());
        for t in digest["tables"].as_array().into_iter().flatten() {
            if let (Some(path), Some(columns), Some(rows)) = (
                t["path"].as_str(),
                t["columns"].as_array(),
                t["rows"].as_u64(),
            ) {
                let columns = columns
                    .iter()
                    .filter_map(|c| c.as_str().map(str::to_owned))
                    .collect();
                self.shapes.insert(
                    (reference.to_owned(), path.to_owned()),
                    TableShape { columns, rows },
                );
            }
            let (Some(path), Some(columns), Some(cells)) = (
                t["path"].as_str(),
                t["columns"].as_array(),
                t["cells"].as_array(),
            ) else {
                if let Some(path) = t["path"].as_str() {
                    let why = t["cells_omitted"].as_str().unwrap_or("unknown");
                    self.withheld
                        .insert((reference.to_owned(), path.to_owned()), why.to_owned());
                }
                continue;
            };
            let columns: Vec<String> = columns
                .iter()
                .filter_map(|c| c.as_str().map(str::to_owned))
                .collect();
            let cells: Vec<Vec<String>> = cells
                .iter()
                .map(|row| {
                    row.as_array()
                        .into_iter()
                        .flatten()
                        .filter_map(|c| c.as_str().map(str::to_owned))
                        .collect()
                })
                .collect();
            self.tables.insert(
                (reference.to_owned(), path.to_owned()),
                InlineTable { columns, cells },
            );
        }
    }

    /// The cells the context's earlier findings show with their values, and the results it names without showing.
    /// Read from the finished context itself, after every shown table is added, so a live call and a rebuild from its
    /// kept text (`from_context`) agree.
    pub(super) fn note_context(&mut self, context: &Value) {
        // An earlier answer's table may be given again when its result is left out for room, as an earlier finding's
        // cells may be restated.
        for t in context["earlier_findings"]["tables"]
            .as_array()
            .into_iter()
            .flatten()
        {
            if let (Some(reference), Some(path), Some(columns), Some(rows)) = (
                t["ref"].as_str(),
                t["table"].as_str(),
                t["columns"].as_array(),
                t["rows"].as_u64(),
            ) {
                let columns = columns
                    .iter()
                    .filter_map(|c| c.as_str().map(str::to_owned))
                    .collect();
                self.shapes
                    .entry((reference.to_owned(), path.to_owned()))
                    .or_insert(TableShape { columns, rows });
            }
        }
        for f in context["earlier_findings"]["findings"]
            .as_array()
            .into_iter()
            .flatten()
        {
            for (slot, b) in f["bindings"].as_object().into_iter().flatten() {
                let (Ok(b), Some(value)) = (
                    serde_json::from_value::<Binding>(b.clone()),
                    f["values"][slot].as_str(),
                ) else {
                    continue;
                };
                self.earlier.insert(earlier_key(&b), value.to_owned());
            }
        }
        let named = context["analyses"]
            .as_array()
            .into_iter()
            .flatten()
            .flat_map(|a| a["result_refs"].as_array().into_iter().flatten())
            .chain(context["results_omitted"].as_array().into_iter().flatten())
            .filter_map(Value::as_str);
        for reference in named {
            if !self.shown.contains(reference) {
                self.listed.insert(reference.to_owned());
            }
        }
    }
}

/// An earlier finding's cell, by ref, table, row and column.
pub(super) fn earlier_key(b: &Binding) -> (String, String, usize, String) {
    (
        b.reference.clone(),
        b.table.clone(),
        b.row,
        b.column.clone(),
    )
}

#[cfg(test)]
#[path = "prose_names_tests.rs"]
mod prose_names_tests;
