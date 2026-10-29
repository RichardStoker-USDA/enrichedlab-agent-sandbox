//! Counting and logging what a refused reply got wrong: query step shapes, bindings and findings.
use super::*;

/// What a query step refused for lacking its query held: its field names, what its `sql` field
/// held and whether its title, objective or rationale reads like a query. Never its text or key.
pub fn query_step_shape(action: &Value) -> (String, &'static str, bool) {
    let mut fields: Vec<&str> = action
        .as_object()
        .map(|o| o.keys().map(|k| field_name(k)).collect())
        .unwrap_or_default();
    fields.sort_unstable();
    let sql = match action.get("sql") {
        None => "absent",
        Some(Value::Null) => "null",
        Some(Value::String(s)) if s.trim().is_empty() => "blank",
        Some(Value::String(_)) => "text",
        Some(_) => "other",
    };
    let select_in_text = ["title", "objective", "rationale"].iter().any(|k| {
        let text = action[*k].as_str().unwrap_or_default().to_ascii_lowercase();
        text.contains("select ") && text.contains(" from ")
    });
    (fields.join(","), sql, select_in_text)
}

/// What is wrong with each placeholder of one finding, by name: `no binding`, `no placeholder`, or the part of its
/// binding that names no cell this call was shown (`ref`, `table`, `column` or `row`).
pub fn binding_faults<'a>(
    f: &'a Finding,
    used: &'a BTreeSet<String>,
    index: &ResultIndex,
) -> BTreeMap<&'a str, &'static str> {
    let mut faults = BTreeMap::new();
    for name in used.iter().filter(|n| !f.bindings.contains_key(*n)) {
        faults.insert(name.as_str(), "no binding");
    }
    for (name, b) in &f.bindings {
        if !used.contains(name) {
            faults.insert(name.as_str(), "no placeholder");
        } else if let Some(part) = index.miss(b) {
            faults.insert(name.as_str(), part);
        }
    }
    faults
}

/// Why a result digest withheld a table's cells, in plain words for the next lead call.
pub(super) fn withheld_words(why: &str) -> &'static str {
    match why {
        "long_cells" => "a cell is longer than 160 characters",
        "too_many_rows" => "it has more than 200 rows",
        "too_large" => "the table is larger than 8 KiB or has more than 16 columns",
        "digest_budget" => "the results were too large to show every table",
        "duplicate_columns" => "two columns share a name",
        "irregular_rows" => "its rows have different numbers of cells",
        "column_named_#" => "a column is named #, which the row numbers use",
        "lab_data" => "it is the lab's own data; run a method on it for results",
        "lab_derived" => {
            "it is computed row by row from the lab's own data, so a person reads its rows; the analysis's statistics table holds its headline numbers"
        }
        "check_failed" => "its result failed its step's check, whose reasons its analysis shows",
        _ => "the table could not be read",
    }
}

/// At most `max` items joined by commas, then how many more there were.
pub(super) fn listed(items: &[&str], max: usize) -> String {
    let mut out = items
        .iter()
        .take(max)
        .copied()
        .collect::<Vec<_>>()
        .join(", ");
    if items.len() > max {
        out.push_str(&format!(", and {} more", items.len() - max));
    }
    out
}

/// A refusal name cut to 300 bytes at a character boundary.
pub(super) fn bounded_name(mut name: String) -> String {
    if name.len() > 300 {
        let mut end = 300;
        while !name.is_char_boundary(end) {
            end -= 1;
        }
        name.truncate(end);
    }
    name
}

/// The unbound bindings of a whole reply by kind, for the log: how many placeholders had each fault, how many unbound
/// refs had the analysis-result form and named a shown analysis, how many unbound columns differ from a column only in
/// case, and how many unbound rows were numbered from 0. Never a name, statement, cell or value.
pub fn finding_fault_counts(proposal: &Proposal, index: &ResultIndex) -> (String, [usize; 4]) {
    let mut kinds: BTreeMap<&str, usize> = BTreeMap::new();
    let mut near = [0; 4];
    for f in &proposal.findings {
        let Some(used) = placeholders(&f.statement) else {
            continue;
        };
        for (name, why) in binding_faults(f, &used, index) {
            *kinds.entry(why).or_default() += 1;
            if let Some(b) = f.bindings.get(name) {
                for (n, hit) in near.iter_mut().zip(index.near_misses(b, why)) {
                    *n += usize::from(hit);
                }
            }
        }
    }
    let kinds = kinds
        .into_iter()
        .map(|(k, n)| format!("{k}:{n}"))
        .collect::<Vec<_>>()
        .join(",");
    (kinds, near)
}

/// Logs the kinds of an unbound reply's faults. Test runs were refused as unbound twice or more with each
/// part named, and nothing recorded which parts.
pub(super) fn log_finding_faults(error: &anyhow::Error, proposal: &Proposal, index: &ResultIndex) {
    if error
        .downcast_ref::<CodeRefused>()
        .is_none_or(|r| r.code != "proposal_finding_unbound")
    {
        return;
    }
    let (kinds, [ref_prefixed, ref_task_shown, column_case, row_zero]) =
        finding_fault_counts(proposal, index);
    tracing::warn!(
        kinds,
        findings = proposal.findings.len(),
        ref_prefixed,
        ref_task_shown,
        column_case,
        row_zero,
        "finding bindings unbound"
    );
}

/// Log the shape of each query step a reply was refused for (`proposal_sql_missing`). Test runs
/// lost five of thirteen readable lead replies to this refusal.
pub(super) fn log_query_step_shape(error: &anyhow::Error, reply: &Value) {
    let Some(refused) = error
        .downcast_ref::<CodeRefused>()
        .filter(|r| r.code == "proposal_sql_missing")
    else {
        return;
    };
    let steps = reply["actions"]
        .as_array()
        .map(Vec::as_slice)
        .unwrap_or(&[]);
    for step in steps {
        if refused
            .names
            .iter()
            .any(|n| step["action_key"].as_str() == Some(n.as_str()))
        {
            let (fields, sql, select_in_text) = query_step_shape(step);
            tracing::warn!(
                fields,
                sql,
                select_in_text,
                steps = steps.len(),
                "query step without its query"
            );
        }
    }
}
