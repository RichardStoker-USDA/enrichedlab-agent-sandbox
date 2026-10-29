//! The numbers a step itself sets. A step's title and rationale describe its method, and they may state
//! the conditions the step's own SQL writes: a literal number compared with something (`=`, `<>`, `!=`, `<`, `<=`, `>`,
//! `>=`) or listed in an `IN` or `NOT IN`. They are read from PostgreSQL's own parse of that step's SQL. A literal
//! anywhere else (a `LIMIT`, a function's argument, a projected value, a cast) is not one, a step without SQL sets
//! none, and a step's numbers stand only in its own title and rationale, never in another step's or in the summary,
//! the findings or the parts, and only where it stands alone, never inside a name or a compact form such as `1e-9`.
//! Every number the prose rule refused in a step rationale during two autonomy suite runs (seven of them) was such a
//! literal of that step's own SQL.
use super::*;

// pg_query's protobuf numbering of A_Expr kinds, as `query_facts` reads them.
const AEXPR_OP: i64 = 1;
const AEXPR_IN: i64 = 7;
const COMPARISONS: [&str; 7] = ["=", "<>", "!=", "<", "<=", ">", ">="];

/// The numbers `action`'s SQL writes in a comparison or an `IN` list, as the parse writes them (sign included); empty
/// when the step has no SQL or the SQL does not parse.
pub(super) fn step_own_numbers(action: &ProposedAction) -> HashSet<String> {
    let mut out = HashSet::new();
    let Some(sql) = action.sql.as_deref() else {
        return out;
    };
    let Ok(parsed) = pg_query::parse(sql) else {
        return out;
    };
    let Ok(tree) = serde_json::to_value(&parsed.protobuf) else {
        return out;
    };
    collect(&tree, &mut out);
    out
}

fn collect(value: &Value, out: &mut HashSet<String>) {
    match value {
        Value::Object(map) => {
            if let Some(expr) = map.get("AExpr") {
                take(expr, out);
            }
            map.values().for_each(|v| collect(v, out));
        }
        Value::Array(items) => items.iter().for_each(|v| collect(v, out)),
        _ => {}
    }
}

fn take(expr: &Value, out: &mut HashSet<String>) {
    let operator = expr["name"]
        .as_array()
        .and_then(|names| names.last())
        .and_then(|n| {
            n.pointer("/String/sval")
                .or_else(|| n.pointer("/node/String/sval"))
        })
        .and_then(Value::as_str);
    let operands: Vec<&Value> = match (expr["kind"].as_i64(), operator) {
        (Some(AEXPR_OP), Some(op)) if COMPARISONS.contains(&op) => {
            vec![&expr["lexpr"], &expr["rexpr"]]
        }
        (Some(AEXPR_IN), Some("=" | "<>")) => expr["rexpr"]
            .pointer("/List/items")
            .or_else(|| expr["rexpr"].pointer("/node/List/items"))
            .and_then(Value::as_array)
            .map(|items| items.iter().collect())
            .unwrap_or_default(),
        _ => Vec::new(),
    };
    for operand in operands {
        if let Some(number) = literal_number(operand) {
            out.insert(number);
        }
    }
}

/// A plain numeric constant written directly as the operand, never one inside a cast, a function or an expression.
fn literal_number(operand: &Value) -> Option<String> {
    let constant = operand
        .get("AConst")
        .or_else(|| operand.pointer("/node/AConst"))?;
    if constant["isnull"].as_bool() == Some(true) {
        return None;
    }
    let val = &constant["val"];
    let text = if let Some(i) = val.pointer("/Ival/ival").and_then(Value::as_i64) {
        i.to_string()
    } else if val.get("Ival").is_some() {
        "0".to_owned()
    } else {
        val.pointer("/Fval/fval")
            .and_then(Value::as_str)?
            .to_owned()
    };
    Some(text)
}

#[cfg(test)]
#[path = "step_literals_tests.rs"]
mod step_literals_tests;
