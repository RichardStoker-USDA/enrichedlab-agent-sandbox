//! Guarded read-only SQL. The research lead proposes one SELECT over a granted source; this module
//! parses it with libpg_query, the parser PostgreSQL itself uses, and admits it only when every node of the parse
//! tree is a read-only kind, every relation is one the source lists, every function is on the source's list, no value
//! is typed in as data, the rows come back in a fixed order and their number is capped. The coordinator runs it when a
//! proposal is checked and the worker again before the query runs; the turn commit keeps only the admitted text and its
//! digest. The reader login's privileges and a read-only transaction stay the primary controls.
use std::collections::BTreeSet;

use serde_json::Value;

use super::sql_names;

pub const GUARD: &str = "guarded-sql/v1";
pub const MAX_SQL_BYTES: usize = 8 * 1024;

/// What a source allows. Relation and function names are compared exactly, as PostgreSQL resolves them; a relation in
/// another schema is listed as `schema.relation`.
pub struct Allowed<'a> {
    pub schema: &'a str,
    pub relations: &'a BTreeSet<String>,
    pub functions: &'a BTreeSet<String>,
    pub row_cap: u64,
}

/// An admitted query: the text the executor runs (with a LIMIT added when the proposal had none) and what it reads.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Admitted {
    pub sql: String,
    pub relations: Vec<String>,
    pub limit: u64,
    /// Admitted without ORDER BY under the single-row exemption; the worker must verify the result's row count.
    pub single_row: bool,
    /// The last parts of qualified column references that the source does not list as functions, sorted and distinct.
    /// PostgreSQL reads `q.name` as the call `name(q)` when `q` has no such column, and the guard cannot see columns,
    /// so the worker refuses the query if one of these names a function that one row argument reaches.
    pub field_names: Vec<String>,
}

/// Parse tree node kinds a read-only SELECT may contain. Anything else, a data-modifying statement inside a CTE for
/// instance, is refused by name.
const NODES: &[&str] = &[
    "SelectStmt",
    "ResTarget",
    "ColumnRef",
    "String",
    "AStar",
    "AConst",
    "AExpr",
    "BoolExpr",
    "NullTest",
    "BooleanTest",
    "FuncCall",
    "TypeCast",
    "TypeName",
    "RangeVar",
    "JoinExpr",
    "SortBy",
    "CommonTableExpr",
    "SubLink",
    "CaseExpr",
    "CaseWhen",
    "CoalesceExpr",
    "MinMaxExpr",
    "RangeSubselect",
    "List",
    "WindowDef",
    "Integer",
    "Float",
    "Boolean",
    "AIndirection",
    "AIndices",
    "RowExpr",
    "CollateClause",
    "GroupingSet",
    "Alias",
    // `x IS [NOT] JSON [VALUE | ARRAY | OBJECT | SCALAR] [WITH | WITHOUT UNIQUE KEYS]`: a pure predicate on one
    // expression, so a query can test stored text before casting it to jsonb. The SQL/JSON query functions
    // (JSON_QUERY, JSON_VALUE, JSON_EXISTS, JSON_TABLE) and constructors parse to other nodes and stay refused.
    "JsonIsPredicate",
    "JsonFormat",
];

/// Why the guard refused a query: a fixed code and, for a relation, function or node kind, the names it refused, so the
/// lead can correct its next proposal.
#[derive(Debug, PartialEq, Eq)]
pub struct Refused {
    pub code: &'static str,
    pub names: Vec<String>,
}

impl std::fmt::Display for Refused {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.code)
    }
}

impl std::error::Error for Refused {}

/// Every code `admit` returns.
pub const CODES: &[&str] = &[
    "sql_refused_size",
    "sql_refused_parse",
    "sql_refused_statements",
    "sql_refused_not_select",
    "sql_refused_node",
    "sql_refused_select_into",
    "sql_refused_locking",
    "sql_refused_values",
    "sql_refused_no_source",
    "sql_refused_relation",
    "sql_refused_function",
    "sql_refused_constants_only",
    "sql_refused_unordered",
    "sql_refused_limit",
];

/// The fixed code an error string names, for refusal feedback; any other text is a plain refusal.
pub fn code(text: &str) -> &'static str {
    CODES
        .iter()
        .copied()
        .find(|c| *c == text)
        .unwrap_or("sql_refused")
}

fn refuse(code: &'static str) -> Refused {
    Refused {
        code,
        names: Vec::new(),
    }
}

/// A query the parser could not read, named by the parser's own message (such as `syntax error at or near "UNION"`),
/// cut to 200 characters with control characters replaced: an autonomy suite question was refused twice with
/// the code alone. The message quotes only the query's own text, and names go to the next lead call, never a log.
fn parse_refusal(error: &pg_query::Error) -> Refused {
    let names = match error {
        pg_query::Error::Parse(message) => {
            let cleaned: String = message
                .chars()
                .map(|c| if c.is_control() { ' ' } else { c })
                .take(200)
                .collect();
            let cleaned = cleaned.trim();
            if cleaned.is_empty() {
                Vec::new()
            } else {
                vec![cleaned.to_owned()]
            }
        }
        _ => Vec::new(),
    };
    Refused {
        code: "sql_refused_parse",
        names,
    }
}

/// Admit `sql` for `allowed`, or return the fixed refusal the lead is told.
pub fn admit(sql: &str, allowed: &Allowed) -> Result<Admitted, Refused> {
    let text = sql.trim().trim_end_matches(';').trim();
    if text.is_empty() || text.len() > MAX_SQL_BYTES {
        return Err(refuse("sql_refused_size"));
    }
    let parsed = pg_query::parse(text).map_err(|e| parse_refusal(&e))?;
    if parsed.protobuf.stmts.len() != 1 {
        return Err(refuse("sql_refused_statements"));
    }
    let tree = serde_json::to_value(&parsed.protobuf).map_err(|_| refuse("sql_refused_parse"))?;
    let top = tree
        .pointer("/stmts/0/stmt/node/SelectStmt")
        .ok_or_else(|| refuse("sql_refused_not_select"))?;

    let mut kinds = BTreeSet::new();
    let mut selects = Vec::new();
    walk(&tree, &mut kinds, &mut selects);
    for s in &selects {
        if !s["into_clause"].is_null() {
            return Err(refuse("sql_refused_select_into"));
        }
        if s["locking_clause"]
            .as_array()
            .is_some_and(|a| !a.is_empty())
        {
            return Err(refuse("sql_refused_locking"));
        }
        if s["values_lists"].as_array().is_some_and(|a| !a.is_empty()) {
            return Err(refuse("sql_refused_values"));
        }
        // Every branch reads a relation: a SELECT without FROM would put typed-in rows beside real ones.
        let set_operation = s["op"].as_i64().unwrap_or(1) != 1;
        if !set_operation && s["from_clause"].as_array().is_none_or(|a| a.is_empty()) {
            return Err(refuse("sql_refused_no_source"));
        }
    }
    if kinds.contains("RangeFunction") {
        return Err(Refused {
            code: "sql_refused_function",
            names: parsed.functions(),
        });
    }
    let refused_kinds: Vec<String> = kinds
        .iter()
        .filter(|k| !NODES.contains(&k.as_str()))
        .cloned()
        .collect();
    if !refused_kinds.is_empty() {
        return Err(Refused {
            code: "sql_refused_node",
            names: refused_kinds,
        });
    }

    // Names come from a walk of every node (`sql_names`), not pg_query's collectors, which skip some node kinds. They
    // are compared as PostgreSQL resolves them: exactly, since the parser has already folded unquoted names.
    let names = sql_names::collect(top);
    let mut relations = BTreeSet::new();
    for r in &names.relations {
        let key = if r.schema.is_empty() || r.schema == allowed.schema {
            r.name.clone()
        } else {
            format!("{}.{}", r.schema, r.name)
        };
        if !r.catalog.is_empty() || !allowed.relations.contains(&key) {
            return Err(Refused {
                code: "sql_refused_relation",
                names: vec![r.written()],
            });
        }
        relations.insert(key);
    }
    if relations.is_empty() {
        return Err(refuse("sql_refused_no_source"));
    }
    for function in &names.functions {
        let bare = match function.as_slice() {
            [name] => Some(name),
            [schema, name] if schema == "pg_catalog" => Some(name),
            _ => None,
        };
        if !bare.is_some_and(|name| allowed.functions.contains(name)) {
            return Err(Refused {
                code: "sql_refused_function",
                names: vec![function.join(".")],
            });
        }
    }
    if !has_data(top) {
        return Err(refuse("sql_refused_constants_only"));
    }
    let ordered = top["sort_clause"].as_array().is_some_and(|a| !a.is_empty());
    let single_row = !ordered && single_row(top);
    if !ordered && !single_row {
        return Err(refuse("sql_refused_unordered"));
    }
    let limit = match &top["limit_count"] {
        Value::Null => None,
        count => {
            // LIMIT n is limit_option 2 (count); FETCH ... WITH TIES can exceed the cap.
            if top["limit_option"].as_i64() != Some(2) {
                return Err(refuse("sql_refused_limit"));
            }
            let n = count
                .pointer("/node/AConst/val/Ival/ival")
                .and_then(Value::as_i64)
                .ok_or_else(|| refuse("sql_refused_limit"))?;
            if n < 0 || n as u64 > allowed.row_cap {
                return Err(refuse("sql_refused_limit"));
            }
            Some(n as u64)
        }
    };
    let (sql, limit) = match limit {
        Some(n) => (text.to_string(), n),
        None => (
            format!("{text}\nLIMIT {}", allowed.row_cap),
            allowed.row_cap,
        ),
    };
    // A selection `(expr).name` is the call `name(expr)` unless `expr` has such a field, for any function one argument
    // reaches and any expression (`(1::int).pg_cancel_backend` calls `pg_cancel_backend(1)`), so only a listed function
    // may follow one.
    if let Some(name) = names
        .selections
        .iter()
        .find(|name| !allowed.functions.contains(*name))
    {
        return Err(Refused {
            code: "sql_refused_function",
            names: vec![name.clone()],
        });
    }
    let field_names: BTreeSet<String> = names
        .fields
        .into_iter()
        .filter(|name| !allowed.functions.contains(name))
        .collect();
    Ok(Admitted {
        sql,
        relations: relations.into_iter().collect(),
        limit,
        single_row,
        field_names: field_names.into_iter().collect(),
    })
}

/// Functions whose form with a number of places PostgreSQL defines only for numeric values: `round(numeric, integer)`
/// and `trunc(numeric, integer)`. A double precision value has no such form, so a query rounding one fails as an
/// unknown function when it runs; 38 of 39 failed autonomy suite queries in one series rounded a FoodData
/// Central amount that way.
const PLACES: &[&str] = &["round", "trunc"];

/// The lead's query with the first argument of every two-argument `round` or `trunc` cast to numeric, as
/// `round((x)::numeric, 2)`, and how many calls it cast. The coordinator applies it before `admit` and stores the
/// result as the proposed text, so the worker's own admission of that text matches. The cast changes no query that
/// ran before: PostgreSQL converts an integer or numeric first argument to numeric for these calls anyway, and a
/// double precision one now runs. An argument already cast to numeric is left alone, so a cast query comes back
/// unchanged. The calls come from the parse tree and the argument's extent from PostgreSQL's own scanner, so a comma
/// or parenthesis inside a string or comment is never mistaken for one; a query either cannot read comes back
/// unchanged for `admit` to judge.
pub fn cast_places(sql: &str) -> (String, usize) {
    let unchanged = || (sql.to_string(), 0);
    let Ok(parsed) = pg_query::parse(sql) else {
        return unchanged();
    };
    let Ok(tree) = serde_json::to_value(&parsed.protobuf) else {
        return unchanged();
    };
    let mut calls = Vec::new();
    places_calls(&tree, &mut calls);
    if calls.is_empty() {
        return unchanged();
    }
    let Ok(scanned) = pg_query::scan(sql) else {
        return unchanged();
    };
    use pg_query::protobuf::Token;
    let kind = |t: &pg_query::protobuf::ScanToken| Token::try_from(t.token).ok();
    let tokens: Vec<_> = scanned
        .tokens
        .iter()
        .filter(|t| !matches!(kind(t), Some(Token::SqlComment | Token::CComment)))
        .collect();
    let count = calls.len();
    // Byte offsets to insert at: an opening parenthesis before the first argument, `)::numeric` after it.
    let mut edits: Vec<(usize, bool)> = Vec::new();
    for location in calls {
        let Some(open) = tokens
            .iter()
            .position(|t| t.start >= location && kind(t) == Some(Token::Ascii40))
        else {
            return unchanged();
        };
        let mut depth = 0usize;
        let mut comma = None;
        for (at, t) in tokens.iter().enumerate().skip(open + 1) {
            match kind(t) {
                Some(Token::Ascii40 | Token::Ascii91) => depth += 1,
                Some(Token::Ascii41 | Token::Ascii93) if depth == 0 => break,
                Some(Token::Ascii41 | Token::Ascii93) => depth -= 1,
                Some(Token::Ascii44) if depth == 0 => {
                    comma = Some(at);
                    break;
                }
                _ => {}
            }
        }
        match comma {
            Some(comma) if comma > open + 1 => {
                edits.push((tokens[open + 1].start as usize, true));
                edits.push((tokens[comma - 1].end as usize, false));
            }
            _ => return unchanged(),
        }
    }
    // From the end, so earlier offsets stay valid; at one offset the opening goes in first and so lands after.
    edits.sort_by(|a, b| b.0.cmp(&a.0).then(b.1.cmp(&a.1)));
    let mut text = sql.to_string();
    for (at, opening) in edits {
        if !text.is_char_boundary(at) {
            return unchanged();
        }
        text.insert_str(at, if opening { "(" } else { ")::numeric" });
    }
    // The cast text must read as a query whose calls need no further cast.
    let mut again = Vec::new();
    match pg_query::parse(&text)
        .ok()
        .and_then(|p| serde_json::to_value(&p.protobuf).ok())
    {
        Some(tree) => places_calls(&tree, &mut again),
        None => return unchanged(),
    }
    if !again.is_empty() {
        return unchanged();
    }
    (text, count)
}

/// The locations of the two-argument `round` and `trunc` calls (bare or in `pg_catalog`) whose first argument is not
/// already cast to numeric.
fn places_calls(value: &Value, calls: &mut Vec<i32>) {
    match value {
        Value::Object(map) => {
            if let Some(call) = map.get("node").and_then(|n| n.get("FuncCall")) {
                let names: Vec<&str> = call["funcname"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter_map(|n| n.pointer("/node/String/sval").and_then(Value::as_str))
                    .collect();
                let places = match names.as_slice() {
                    [name] | ["pg_catalog", name] => PLACES.contains(&name.to_lowercase().as_str()),
                    _ => false,
                };
                let args = call["args"].as_array();
                let cast = args
                    .and_then(|a| a.first())
                    .and_then(|a| a.pointer("/node/TypeCast/type_name/names"))
                    .and_then(Value::as_array)
                    .and_then(|n| n.last())
                    .and_then(|n| n.pointer("/node/String/sval"))
                    .and_then(Value::as_str)
                    == Some("numeric");
                let location = call["location"].as_i64().unwrap_or(-1);
                if places && args.is_some_and(|a| a.len() == 2) && !cast && location >= 0 {
                    calls.push(location as i32);
                }
            }
            map.values().for_each(|v| places_calls(v, calls));
        }
        Value::Array(items) => items.iter().for_each(|v| places_calls(v, calls)),
        _ => {}
    }
}

/// Collect every node kind in the tree and every SELECT (the top one, subqueries and CTE bodies).
fn walk<'a>(value: &'a Value, kinds: &mut BTreeSet<String>, selects: &mut Vec<&'a Value>) {
    match value {
        Value::Object(map) => {
            if let Some(Value::Object(node)) = map.get("node") {
                for (kind, inner) in node {
                    if kind.starts_with(|c: char| c.is_ascii_uppercase()) {
                        kinds.insert(kind.clone());
                        if kind == "SelectStmt" {
                            selects.push(inner);
                        }
                    }
                }
            }
            // The two sides of a set operation are SELECTs stored without the node wrapper. A join's sides share the
            // field names but are wrapped nodes, so only a SELECT's own sides count.
            if map.contains_key("target_list") {
                for side in ["larg", "rarg"] {
                    if let Some(inner @ Value::Object(_)) = map.get(side) {
                        selects.push(inner);
                    }
                }
            }
            for inner in map.values() {
                walk(inner, kinds, selects);
            }
        }
        Value::Array(items) => items.iter().for_each(|v| walk(v, kinds, selects)),
        _ => {}
    }
}

/// True when the result projects something read from a relation: a column, a function over one or a subquery.
fn has_data(select: &Value) -> bool {
    // A set operation (UNION and the like) has no target list of its own; either side may carry the data.
    if select["op"].as_i64().unwrap_or(1) != 1 {
        return ["larg", "rarg"].iter().any(|side| has_data(&select[*side]));
    }
    select["target_list"].as_array().is_some_and(|targets| {
        targets.iter().any(|t| {
            let mut kinds = BTreeSet::new();
            walk(&t["node"]["ResTarget"]["val"], &mut kinds, &mut Vec::new());
            kinds
                .iter()
                .any(|k| matches!(k.as_str(), "ColumnRef" | "FuncCall" | "SubLink"))
        })
    })
}

const SINGLE_ROW_EXPR_DEPTH: usize = 64;

const SINGLE_ROW_SCALARS: &[&str] = &[
    "round", "trunc", "abs", "ceil", "ceiling", "floor", "sign", "sqrt", "power", "ln", "log",
    "exp",
];

/// True when the top-level query returns at most one row: it has no GROUP BY or set operation, every target is a
/// conservative scalar expression, and at least one target contains a non-window aggregate.
fn single_row(select: &Value) -> bool {
    if select["op"].as_i64() != Some(1)
        || select["group_clause"]
            .as_array()
            .is_some_and(|groups| !groups.is_empty())
    {
        return false;
    }
    let Some(targets) = select["target_list"].as_array() else {
        return false;
    };
    targets
        .iter()
        .try_fold(false, |has_aggregate, target| {
            let value = target.pointer("/node/ResTarget/val")?;
            single_row_expr(value, 0).map(|found| has_aggregate || found)
        })
        .unwrap_or(false)
}

/// `Some` means the subtree uses only the single-row expression grammar; its value records whether it contains an
/// aggregate. `None` means the subtree is outside that grammar or exceeds the recursion bound.
fn single_row_expr(value: &Value, depth: usize) -> Option<bool> {
    if depth >= SINGLE_ROW_EXPR_DEPTH {
        return None;
    }
    let node = value.get("node")?.as_object()?;
    if node.len() != 1 {
        return None;
    }
    let (kind, inner) = node.iter().next()?;
    match kind.as_str() {
        "AConst" => Some(false),
        "TypeCast" => single_row_expr(&inner["arg"], depth + 1),
        "FuncCall" => single_row_func(inner, depth),
        "AExpr" => single_row_aexpr(inner, depth),
        "CoalesceExpr" => single_row_args(&inner["args"], depth + 1),
        "MinMaxExpr" => {
            let op = inner["op"].as_i64()?;
            if op != pg_query::protobuf::MinMaxOp::IsGreatest as i64
                && op != pg_query::protobuf::MinMaxOp::IsLeast as i64
            {
                return None;
            }
            single_row_args(&inner["args"], depth + 1)
        }
        _ => None,
    }
}

fn single_row_func(call: &Value, depth: usize) -> Option<bool> {
    if !call["over"].is_null() {
        return None;
    }
    let name = call["funcname"]
        .as_array()?
        .last()?
        .pointer("/node/String/sval")?
        .as_str()?;
    if AGGREGATES
        .iter()
        .any(|aggregate| name.eq_ignore_ascii_case(aggregate))
    {
        // An aggregate's arguments, FILTER, WITHIN GROUP and ORDER BY do not affect result cardinality.
        return Some(true);
    }

    let names = call["funcname"].as_array()?;
    let qualified = names.len() == 1
        || (names.len() == 2
            && names[0]
                .pointer("/node/String/sval")
                .and_then(Value::as_str)
                .is_some_and(|schema| schema.eq_ignore_ascii_case("pg_catalog")));
    let ordinary = call["agg_filter"].is_null()
        && call["agg_order"]
            .as_array()
            .is_some_and(|order| order.is_empty())
        && call["agg_within_group"].as_bool() == Some(false)
        && call["agg_star"].as_bool() == Some(false)
        && call["agg_distinct"].as_bool() == Some(false)
        && call["func_variadic"].as_bool() == Some(false);
    if !qualified
        || !ordinary
        || !SINGLE_ROW_SCALARS
            .iter()
            .any(|scalar| name.eq_ignore_ascii_case(scalar))
    {
        return None;
    }
    single_row_args(&call["args"], depth + 1)
}

fn single_row_aexpr(expr: &Value, depth: usize) -> Option<bool> {
    let kind = expr["kind"].as_i64()?;
    let next = depth + 1;
    if kind == pg_query::protobuf::AExprKind::AexprNullif as i64 {
        return single_row_pair(&expr["lexpr"], &expr["rexpr"], next);
    }
    if kind != pg_query::protobuf::AExprKind::AexprOp as i64 {
        return None;
    }
    let names = expr["name"].as_array()?;
    if names.len() != 1 {
        return None;
    }
    let operator = names[0].pointer("/node/String/sval")?.as_str()?;
    if expr["lexpr"].is_null() {
        return (operator == "-")
            .then(|| single_row_expr(&expr["rexpr"], next))
            .flatten();
    }
    if !["+", "-", "*", "/", "%"].contains(&operator) {
        return None;
    }
    single_row_pair(&expr["lexpr"], &expr["rexpr"], next)
}

fn single_row_pair(left: &Value, right: &Value, depth: usize) -> Option<bool> {
    let left = single_row_expr(left, depth)?;
    let right = single_row_expr(right, depth)?;
    Some(left || right)
}

fn single_row_args(args: &Value, depth: usize) -> Option<bool> {
    let args = args.as_array()?;
    if args.is_empty() {
        return None;
    }
    args.iter().try_fold(false, |has_aggregate, argument| {
        single_row_expr(argument, depth).map(|found| has_aggregate || found)
    })
}

const AGGREGATES: &[&str] = &[
    "count",
    "sum",
    "avg",
    "min",
    "max",
    "stddev",
    "stddev_samp",
    "stddev_pop",
    "variance",
    "var_samp",
    "var_pop",
    "percentile_cont",
    "percentile_disc",
    "mode",
    "bool_and",
    "bool_or",
];

#[cfg(test)]
mod tests {
    use super::*;

    fn allowed() -> (BTreeSet<String>, BTreeSet<String>) {
        let relations = ["food", "food_nutrient", "nutrient", "food_category"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        let functions = [
            "count",
            "avg",
            "max",
            "min",
            "sum",
            "round",
            "lower",
            "percentile_cont",
            "rank",
        ]
        .iter()
        .map(|s| s.to_string())
        .collect();
        (relations, functions)
    }

    fn check(sql: &str) -> Result<Admitted, &'static str> {
        let (relations, functions) = allowed();
        admit(
            sql,
            &Allowed {
                schema: "fdc",
                relations: &relations,
                functions: &functions,
                row_cap: 5000,
            },
        )
        .map_err(|r| r.code)
    }

    fn parsed_top(sql: &str) -> Value {
        let parsed = pg_query::parse(sql).unwrap();
        let tree = serde_json::to_value(&parsed.protobuf).unwrap();
        tree.pointer("/stmts/0/stmt/node/SelectStmt")
            .unwrap()
            .clone()
    }

    #[test]
    fn admits_an_ordered_join_and_adds_the_cap() {
        let a = check("SELECT f.description, fn.amount FROM food f JOIN food_nutrient fn ON fn.fdc_id = f.fdc_id \
                       JOIN nutrient n ON n.id = fn.nutrient_id WHERE n.name = 'Vitamin C, total ascorbic acid' \
                       ORDER BY fn.amount DESC, f.description")
            .unwrap();
        assert!(a.sql.ends_with("\nLIMIT 5000"));
        assert_eq!(a.relations, vec!["food", "food_nutrient", "nutrient"]);
        assert_eq!(a.limit, 5000);
        assert_eq!(
            check("SELECT description FROM fdc.food ORDER BY 1 LIMIT 20;")
                .unwrap()
                .limit,
            20
        );
    }

    #[test]
    fn a_single_aggregate_row_needs_no_order() {
        assert!(check("SELECT count(*) FROM food").is_ok());
        assert_eq!(
            check("SELECT data_type, count(*) FROM food GROUP BY data_type").unwrap_err(),
            "sql_refused_unordered"
        );
    }

    #[test]
    fn records_only_queries_admitted_by_the_single_row_exemption() {
        for (sql, single_row) in [
            ("SELECT count(*) FROM food", true),
            ("SELECT round(count(*)) FROM food", true),
            ("SELECT description FROM food ORDER BY 1", false),
            ("SELECT count(*) FROM food ORDER BY 1", false),
        ] {
            let admitted = check(sql).unwrap();
            assert_eq!(admitted.single_row, single_row, "{sql}");
            assert_eq!(admitted.sql, format!("{sql}\nLIMIT 5000"));
            assert_eq!(admitted.limit, 5000);
        }
    }

    #[test]
    fn scalar_expressions_over_aggregates_need_no_order() {
        for sql in [
            "SELECT round(avg(amount)::numeric, 1) FROM food_nutrient",
            "SELECT count(*) * 100.0 / sum(amount) FROM food_nutrient",
            "SELECT count(*) FILTER (WHERE amount > 1) AS n FROM food_nutrient",
            "SELECT round((percentile_cont(0.75) WITHIN GROUP (ORDER BY amount))::numeric, 1) FROM food_nutrient",
            "SELECT coalesce(max(amount), 0), -min(amount) FROM food_nutrient",
            "SELECT count(*) AS n, round((percentile_cont(0.75) WITHIN GROUP (ORDER BY amount))::numeric, 1) AS cutoff FROM food_nutrient",
            "SELECT greatest(coalesce(max(amount), 0), nullif(-min(amount), 0)), least(max(amount), 1) FROM food_nutrient",
        ] {
            assert!(check(sql).is_ok(), "{sql}");
        }
    }

    #[test]
    fn columns_windows_case_and_other_calls_still_need_order() {
        for sql in [
            "SELECT round(amount::numeric, 1) FROM food_nutrient",
            "SELECT round(avg(fdc_id)::numeric, 1), description FROM food",
            "SELECT count(*) OVER () FROM food",
            "SELECT data_type, count(*) FROM food GROUP BY data_type",
            "SELECT CASE WHEN count(*) > 0 THEN 1 END FROM food",
            "SELECT lower(max(description)) FROM food",
        ] {
            assert_eq!(check(sql).unwrap_err(), "sql_refused_unordered", "{sql}");
        }
    }

    #[test]
    fn set_returning_calls_are_not_single_rows() {
        for sql in [
            "SELECT unnest(array_agg(amount)) FROM food_nutrient",
            "SELECT generate_series(1, count(*)) FROM food",
        ] {
            assert_eq!(check(sql).unwrap_err(), "sql_refused_function", "{sql}");
            let top = parsed_top(sql);
            assert!(!single_row(&top), "{sql}");
        }
    }

    #[test]
    fn single_row_expression_depth_is_bounded() {
        let mut expression = serde_json::json!({
            "node": {"FuncCall": {
                "funcname": [{"node": {"String": {"sval": "count"}}}],
                "over": null
            }}
        });
        for _ in 0..=SINGLE_ROW_EXPR_DEPTH {
            expression = serde_json::json!({"node": {"TypeCast": {"arg": expression}}});
        }
        let top = serde_json::json!({
            "op": 1,
            "group_clause": [],
            "target_list": [{"node": {"ResTarget": {"val": expression}}}]
        });
        assert!(!single_row(&top));
    }

    #[test]
    fn refuses_writes_anywhere_in_the_tree() {
        assert_eq!(
            check("WITH x AS (DELETE FROM food RETURNING *) SELECT * FROM x ORDER BY 1")
                .unwrap_err(),
            "sql_refused_node"
        );
        assert_eq!(
            check("UPDATE food SET description = 'x'").unwrap_err(),
            "sql_refused_not_select"
        );
        assert_eq!(
            check("SELECT 1; DROP TABLE food").unwrap_err(),
            "sql_refused_statements"
        );
        assert_eq!(
            check("SELECT * INTO t FROM food ORDER BY 1").unwrap_err(),
            "sql_refused_select_into"
        );
        assert_eq!(
            check("SELECT * FROM food ORDER BY 1 FOR UPDATE").unwrap_err(),
            "sql_refused_locking"
        );
    }

    #[test]
    fn refuses_typed_in_data() {
        assert_eq!(
            check("SELECT 8.26 AS mean").unwrap_err(),
            "sql_refused_no_source"
        );
        assert_eq!(
            check("SELECT 8.26 AS mean FROM food ORDER BY 1").unwrap_err(),
            "sql_refused_constants_only"
        );
        assert_eq!(
            check("SELECT * FROM (VALUES (1, 2)) v(a, b) ORDER BY 1").unwrap_err(),
            "sql_refused_values"
        );
        assert_eq!(
            check("SELECT description FROM food UNION SELECT 'invented' ORDER BY 1").unwrap_err(),
            "sql_refused_no_source"
        );
        assert_eq!(
            check("SELECT description FROM food WHERE fdc_id IN (SELECT 1) ORDER BY 1")
                .unwrap_err(),
            "sql_refused_no_source"
        );
        assert!(check("SELECT description FROM food UNION SELECT description FROM food_category ORDER BY 1").is_ok());
    }

    #[test]
    fn refuses_what_the_source_does_not_list() {
        assert_eq!(
            check("SELECT * FROM pg_catalog.pg_roles ORDER BY 1").unwrap_err(),
            "sql_refused_relation"
        );
        assert_eq!(
            check("SELECT * FROM branded_food ORDER BY 1").unwrap_err(),
            "sql_refused_relation"
        );
        assert_eq!(
            check("SELECT now() FROM food ORDER BY 1").unwrap_err(),
            "sql_refused_function"
        );
        assert_eq!(
            check("SELECT pg_read_file('x') FROM food ORDER BY 1").unwrap_err(),
            "sql_refused_function"
        );
        assert_eq!(
            check("SELECT * FROM generate_series(1, 10) g ORDER BY 1").unwrap_err(),
            "sql_refused_function"
        );
    }

    #[test]
    fn caps_the_rows() {
        assert_eq!(
            check("SELECT description FROM food ORDER BY 1 LIMIT 5001").unwrap_err(),
            "sql_refused_limit"
        );
        assert_eq!(
            check("SELECT description FROM food ORDER BY 1 LIMIT 5 + 1").unwrap_err(),
            "sql_refused_limit"
        );
        assert_eq!(
            check("SELECT description FROM food ORDER BY 1 LIMIT (SELECT 5)").unwrap_err(),
            "sql_refused_no_source"
        );
        assert_eq!(
            check("SELECT description FROM food ORDER BY 1 FETCH FIRST 5 ROWS WITH TIES")
                .unwrap_err(),
            "sql_refused_limit"
        );
        assert_eq!(
            check(&"x".repeat(MAX_SQL_BYTES + 1)).unwrap_err(),
            "sql_refused_size"
        );
        assert_eq!(
            check("SELEC description FROM food").unwrap_err(),
            "sql_refused_parse"
        );
    }

    // A query the parser cannot read is named by where it went wrong, so the next call can fix it: a LIMIT inside an
    // unparenthesized UNION branch is the kind of query a lowest-and-highest question gets.
    #[test]
    fn names_where_a_query_could_not_be_read() {
        let (relations, functions) = allowed();
        let allowed = Allowed {
            schema: "fdc",
            relations: &relations,
            functions: &functions,
            row_cap: 5000,
        };
        let refused = admit(
            "SELECT description FROM food ORDER BY 1 LIMIT 1 UNION ALL SELECT description FROM food ORDER BY 1 LIMIT 1",
            &allowed,
        )
        .unwrap_err();
        assert_eq!(refused.code, "sql_refused_parse");
        assert_eq!(refused.names.len(), 1);
        assert!(
            refused.names[0].contains("syntax error at or near"),
            "{:?}",
            refused.names
        );
        assert!(refused.names[0].len() <= 200);
    }

    #[test]
    fn names_what_it_refused() {
        let (relations, functions) = allowed();
        let allowed = Allowed {
            schema: "fdc",
            relations: &relations,
            functions: &functions,
            row_cap: 5000,
        };
        let refused = admit("SELECT * FROM branded_food ORDER BY 1", &allowed).unwrap_err();
        assert_eq!(refused.names, vec!["branded_food"]);
        let refused = admit("SELECT now() FROM food ORDER BY 1", &allowed).unwrap_err();
        assert_eq!(refused.names, vec!["now"]);
        let refused = admit(
            "WITH x AS (DELETE FROM food RETURNING *) SELECT * FROM x ORDER BY 1",
            &allowed,
        )
        .unwrap_err();
        assert_eq!(refused.names, vec!["DeleteStmt"]);
        assert!(CODES.contains(&refused.code));
        assert_eq!(code("sql_refused_relation"), "sql_refused_relation");
        assert_eq!(code("something else"), "sql_refused");
    }

    fn cast(sql: &str) -> (String, usize) {
        cast_places(sql)
    }

    // PostgreSQL has round and trunc to places only for numeric, so the first argument is cast; the
    // cast query is admitted and needs no second cast.
    #[test]
    fn casts_the_first_argument_of_round_and_trunc_to_places() {
        let (text, count) = cast(
            "SELECT nutrient_id, round(avg(amount), 2) AS mean FROM food_nutrient GROUP BY 1 ORDER BY 1",
        );
        assert_eq!(
            text,
            "SELECT nutrient_id, round((avg(amount))::numeric, 2) AS mean FROM food_nutrient GROUP BY 1 ORDER BY 1"
        );
        assert_eq!(count, 1);
        assert!(check(&text).is_ok());
        assert_eq!(cast(&text), (text.clone(), 0));
        assert_eq!(
            cast("SELECT TRUNC(amount, 1) FROM food_nutrient ORDER BY 1").0,
            "SELECT TRUNC((amount)::numeric, 1) FROM food_nutrient ORDER BY 1"
        );
        assert_eq!(
            cast("SELECT pg_catalog.round(amount, 1) FROM food_nutrient ORDER BY 1").0,
            "SELECT pg_catalog.round((amount)::numeric, 1) FROM food_nutrient ORDER BY 1"
        );
        let (nested, count) =
            cast("SELECT round(round(amount, 1) * 2, 0) AS r FROM food_nutrient ORDER BY 1");
        assert_eq!(
            nested,
            "SELECT round((round((amount)::numeric, 1) * 2)::numeric, 0) AS r FROM food_nutrient ORDER BY 1"
        );
        assert_eq!(count, 2);
        assert_eq!(cast(&nested).1, 0);
        // A window or a function in the first argument keeps its own commas and parentheses.
        assert_eq!(
            cast("SELECT round(percentile_cont(0.5) WITHIN GROUP (ORDER BY amount, fdc_id), 3) FROM food_nutrient").0,
            "SELECT round((percentile_cont(0.5) WITHIN GROUP (ORDER BY amount, fdc_id))::numeric, 3) FROM food_nutrient"
        );
    }

    #[test]
    fn leaves_other_calls_and_cast_arguments_alone() {
        for sql in [
            "SELECT round(amount) FROM food_nutrient ORDER BY 1",
            "SELECT round(amount::numeric, 2) FROM food_nutrient ORDER BY 1",
            "SELECT round(CAST(amount AS numeric), 2) FROM food_nutrient ORDER BY 1",
            "SELECT round(amount::decimal(10, 4), 2) FROM food_nutrient ORDER BY 1",
            "SELECT floor(amount), max(amount) FROM food_nutrient GROUP BY 1 ORDER BY 1",
            "SELECT round FROM food ORDER BY 1",
            "SELEC round(amount, 2) FROM food_nutrient",
        ] {
            assert_eq!(cast(sql), (sql.to_string(), 0), "{sql}");
        }
        let (text, count) = cast("SELECT round(amount::float8, 2) FROM food_nutrient ORDER BY 1");
        assert_eq!(
            text,
            "SELECT round((amount::float8)::numeric, 2) FROM food_nutrient ORDER BY 1"
        );
        assert_eq!(count, 1);
    }

    // The argument's extent comes from PostgreSQL's scanner: a comma or parenthesis in a string or a comment is not
    // one, a comment before the comma stays outside the cast, and offsets are bytes, so a name with a multibyte letter
    // before the call moves nothing.
    #[test]
    fn finds_the_argument_past_strings_comments_and_multibyte_names() {
        let (text, _) = cast(
            "SELECT round(avg(CASE WHEN n.name = 'a, (b' THEN fn.amount END) /* x, ( */, 2) AS m \
             FROM food_nutrient fn JOIN nutrient n ON n.id = fn.nutrient_id",
        );
        assert!(
            text.contains(
                "round((avg(CASE WHEN n.name = 'a, (b' THEN fn.amount END))::numeric /* x, ( */, 2)"
            ),
            "{text}"
        );
        assert_eq!(
            cast("SELECT round(avg(amount) -- per food\n, 2) FROM food_nutrient").0,
            "SELECT round((avg(amount))::numeric -- per food\n, 2) FROM food_nutrient"
        );
        assert_eq!(
            cast("SELECT n.name AS \"µg per 100 g\", round(avg(fn.amount), 2) AS mean FROM food_nutrient fn \
                  JOIN nutrient n ON n.id = fn.nutrient_id GROUP BY 1 ORDER BY 1")
                .0,
            "SELECT n.name AS \"µg per 100 g\", round((avg(fn.amount))::numeric, 2) AS mean FROM food_nutrient fn \
                  JOIN nutrient n ON n.id = fn.nutrient_id GROUP BY 1 ORDER BY 1"
        );
        // A comma inside brackets belongs to the first argument too (the guard then refuses the array itself).
        let (array, _) = cast("SELECT round(ARRAY[amount, 1], 2) FROM food_nutrient ORDER BY 1");
        assert_eq!(
            array,
            "SELECT round((ARRAY[amount, 1])::numeric, 2) FROM food_nutrient ORDER BY 1"
        );
        assert_eq!(check(&array).unwrap_err(), "sql_refused_node");
    }

    #[test]
    fn admits_is_json_and_refuses_the_sql_json_functions() {
        for sql in [
            "SELECT f.description FROM food f WHERE f.description IS JSON OBJECT ORDER BY 1",
            "SELECT f.description FROM food f WHERE f.description IS NOT JSON ORDER BY 1",
            "SELECT f.description FROM food f WHERE f.description IS JSON WITH UNIQUE KEYS ORDER BY 1",
            "SELECT f.description FROM food f WHERE f.description IS JSON SCALAR ORDER BY 1",
            "SELECT CASE WHEN f.description IS JSON OBJECT THEN f.description::jsonb ->> 'a' END AS a \
             FROM food f ORDER BY 1",
        ] {
            assert!(check(sql).is_ok(), "{sql}");
        }
        for sql in [
            "SELECT JSON_QUERY(f.description::jsonb, '$.a') AS a FROM food f ORDER BY 1",
            "SELECT JSON_VALUE(f.description::jsonb, '$.a') AS a FROM food f ORDER BY 1",
            "SELECT f.fdc_id FROM food f WHERE JSON_EXISTS(f.description::jsonb, '$.a') ORDER BY 1",
            "SELECT JSON_OBJECT('a': f.description) AS a FROM food f ORDER BY 1",
            "SELECT j.a FROM food f, JSON_TABLE(f.description::jsonb, '$' COLUMNS (a text PATH '$.a')) j ORDER BY 1",
        ] {
            assert_eq!(check(sql).unwrap_err(), "sql_refused_node", "{sql}");
        }
    }

    #[test]
    fn allows_ctes_windows_and_subqueries_over_listed_relations() {
        let a = check("WITH top AS (SELECT fdc_id, amount, rank() OVER (ORDER BY amount DESC) AS r FROM food_nutrient \
                       WHERE nutrient_id = 1162) SELECT f.description, top.amount FROM top JOIN food f USING (fdc_id) \
                       WHERE top.r <= 10 ORDER BY top.amount DESC")
            .unwrap();
        assert_eq!(a.relations, vec!["food", "food_nutrient"]);
        assert!(check("SELECT description FROM food WHERE fdc_id IN (SELECT fdc_id FROM food_nutrient WHERE amount > 10) ORDER BY 1").is_ok());
    }
}

#[cfg(test)]
#[path = "sql_guard_bypass_tests.rs"]
mod bypass_tests;
