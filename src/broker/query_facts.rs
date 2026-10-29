//! Query facts. Before the methods review checks a proposed query step, code compares the admitted
//! query with its source's schema card and states what the card's own measurements say about it:
//! each value the query compares a card column with (`=`, `IN`, `LIKE`, `ILIKE`), whether the card holds it and how many
//! rows use it, the nearest values it holds when it does not, the units of the lookup entries the query selects when
//! they differ, and the rows an inner join drops for want of a match. The facts come from PostgreSQL's own parse of the
//! query and from the card; no model reads or writes them. They go to the methods review with the proposal and, when
//! the review sends the proposal back, to the lead's next call.
//!
//! A fact is stated only where the parse proves it, so only for a query of a shape this module reads whole: one
//! `SELECT` with no set operation, whose `WITH` (not `RECURSIVE`) holds only plain `SELECT`s, and no nested query
//! anywhere else (no subquery, derived table or function in `FROM`). Any other query gets no facts. Each `FROM` item is
//! one relation instance, named by its alias or, without one, by its relation; the name of a `WITH` query stands for that
//! query, never for a card relation, from the queries after it on. A qualified column resolves through those names, an
//! unqualified one only when its `SELECT` reads a single relation. A comparison counts as a join only when it is a
//! top-level conjunct of `WHERE` or of a join's `ON` between columns of two instances, and its measurement is stated only
//! when the query compares the two columns with the casts the card's own `on` condition uses. A lookup's selection is the set of
//! entries its own conditions keep together: known only when every conjunct of `WHERE` and of an inner join's `ON` that
//! names the lookup is a restriction of its columns alone (or a join), no outer join's `ON` restricts it, every column
//! there is placed and there is no `HAVING`. `LIKE` is read only with the column on its left, and only on a text column.
//! The card writes values as text, so a value is compared under the column's SQL type as the source registered it, as
//! PostgreSQL compares them where that is certain: integer and numeric columns as exact numbers (a quoted literal read as
//! the column's type reads its input), text columns as text, booleans as booleans, and `NULL` never matches; a column
//! of any other or unregistered type, a float, or a cast with a type modifier states nothing.
use serde_json::{Map, Value, json};
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};

use super::screen;

/// The most facts one query step carries; the rest are counted.
pub const FACTS_MAX: usize = 8;
/// The most held values a missing value's fact names as nearest.
pub const NEAREST_MAX: usize = 3;
/// The most entries a pattern's fact shows of those it matches.
pub const EXAMPLES_MAX: usize = 5;

// pg_query's protobuf numbering: A_Expr kinds, Boolean operators and join types.
const AEXPR_OP: i64 = 1;
const AEXPR_IN: i64 = 7;
const AEXPR_LIKE: i64 = 8;
const AEXPR_ILIKE: i64 = 9;
const AND_EXPR: i64 = 1;
const JOIN_INNER: i64 = 1;
const JOIN_LEFT: i64 = 2;
const JOIN_FULL: i64 = 3;
const JOIN_RIGHT: i64 = 4;

/// A literal the query compares with, typed as PostgreSQL types it.
#[derive(Clone, Debug, PartialEq)]
enum Lit {
    Int(i64),
    /// An exact decimal that is not a whole number, as written without a sign's `+` or trailing zeros.
    Dec(String),
    Str(String),
    Bool(bool),
}

impl Lit {
    fn shown(&self) -> Value {
        match self {
            Lit::Int(i) => json!(i),
            Lit::Dec(d) => json!(d),
            Lit::Str(s) => json!(s),
            Lit::Bool(b) => json!(b),
        }
    }
    fn key(&self) -> String {
        self.shown().to_string()
    }
}

/// An exact decimal literal: a whole number as `Int`, any other as `Dec`; None for an exponent or anything else.
fn decimal(text: &str) -> Option<Lit> {
    let (negative, digits) = match text.trim().strip_prefix('-') {
        Some(rest) => (true, rest),
        None => (false, text.trim().strip_prefix('+').unwrap_or(text.trim())),
    };
    let (whole, fraction) = digits.split_once('.').unwrap_or((digits, ""));
    if whole.is_empty() && fraction.is_empty()
        || !whole
            .chars()
            .chain(fraction.chars())
            .all(|c| c.is_ascii_digit())
    {
        return None;
    }
    let fraction = fraction.trim_end_matches('0');
    let whole = whole.trim_start_matches('0');
    if fraction.is_empty() {
        let value: i64 = if whole.is_empty() {
            0
        } else {
            whole.parse().ok()?
        };
        return Some(Lit::Int(if negative { -value } else { value }));
    }
    let sign = if negative { "-" } else { "" };
    Some(Lit::Dec(format!(
        "{sign}{}.{fraction}",
        if whole.is_empty() { "0" } else { whole }
    )))
}

/// The SQL types whose comparison with a literal this module states exactly.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Kind {
    Integer,
    Numeric,
    Text,
    Boolean,
}

/// A column's kind from the type the source registered for it; None for any type compared otherwise (a float, a
/// fixed-length character type, a date) or a type with a modifier.
fn kind(sql_type: Option<&str>) -> Option<Kind> {
    match sql_type?.trim().to_lowercase().as_str() {
        "smallint" | "integer" | "int" | "int2" | "int4" | "int8" | "bigint" => Some(Kind::Integer),
        "numeric" | "decimal" => Some(Kind::Numeric),
        "text" | "character varying" | "varchar" => Some(Kind::Text),
        "boolean" | "bool" => Some(Kind::Boolean),
        _ => None,
    }
}

/// A whole number written plainly: an optional sign and digits, as PostgreSQL writes an integer and reads one without
/// doubt (input with underscores or a radix prefix is left undecided).
fn plain_integer(text: &str) -> Option<i64> {
    let digits = text.strip_prefix(['+', '-']).unwrap_or(text);
    (!digits.is_empty() && digits.chars().all(|c| c.is_ascii_digit()))
        .then(|| text.parse().ok())
        .flatten()
}

/// A card value as its column's type holds it. The card writes value lists and lookup labels as text, so the text is
/// read back under the registered type.
fn held_as(held: &Value, kind: Kind) -> Option<Lit> {
    match (kind, held) {
        (Kind::Integer | Kind::Numeric, Value::Number(n)) => n.as_i64().map(Lit::Int),
        (Kind::Integer, Value::String(s)) => plain_integer(s).map(Lit::Int),
        (Kind::Numeric, Value::String(s)) => decimal(s),
        (Kind::Text, Value::String(s)) => Some(Lit::Str(s.clone())),
        (Kind::Boolean, Value::Bool(b)) => Some(Lit::Bool(*b)),
        (Kind::Boolean, Value::String(s)) if s == "true" || s == "false" => {
            Some(Lit::Bool(s == "true"))
        }
        _ => None,
    }
}

/// A literal as PostgreSQL reads it against a column of this kind: a number against a number, a quoted literal as the
/// column's type reads its input; None where that is not certain or PostgreSQL would refuse the comparison.
fn literal_as(lit: &Lit, kind: Kind) -> Option<Lit> {
    match (kind, lit) {
        (Kind::Integer | Kind::Numeric, Lit::Int(_) | Lit::Dec(_)) => Some(lit.clone()),
        (Kind::Integer, Lit::Str(s)) => plain_integer(s.trim()).map(Lit::Int),
        (Kind::Numeric, Lit::Str(s)) => decimal(s),
        (Kind::Text, Lit::Str(_)) | (Kind::Boolean, Lit::Bool(_)) => Some(lit.clone()),
        _ => None,
    }
}

/// Whether a card value equals a literal under PostgreSQL's comparison for the column's kind, or None when that is not
/// certain here. NULL never equals; a whole number and an exact decimal that is not whole never equal.
fn equal(held: &Value, lit: &Lit, kind: Option<Kind>) -> Option<bool> {
    if held.is_null() {
        return Some(false);
    }
    let kind = kind?;
    Some(held_as(held, kind)? == literal_as(lit, kind)?)
}

struct Lookup {
    relation: String,
    label: String,
    columns: Vec<String>,
    /// Each column's kind, from the registered types.
    kinds: Vec<Option<Kind>>,
    entries: Vec<Vec<Value>>,
    counted: bool,
    complete: bool,
}

impl Lookup {
    /// The columns a query can name: key, label and details, not the count.
    fn width(&self) -> usize {
        self.columns.len() - usize::from(self.counted)
    }
    fn index(&self, column: &str) -> Option<usize> {
        (0..self.width()).find(|i| self.columns[*i] == column)
    }
    /// One entry as the facts show it: its key, label, details and, when counted, how many rows use it.
    fn shown(&self, entry: &[Value]) -> Value {
        let mut out = Map::new();
        for (i, name) in self.columns.iter().enumerate().take(self.width()) {
            let field = match i {
                0 => "key".to_owned(),
                1 => "label".to_owned(),
                _ => name.clone(),
            };
            out.insert(field, entry.get(i).cloned().unwrap_or(Value::Null));
        }
        if self.counted {
            out.insert("rows".into(), entry.last().cloned().unwrap_or(Value::Null));
        }
        Value::Object(out)
    }
    /// The detail columns that hold a unit.
    fn unit_columns(&self) -> Vec<usize> {
        (2..self.width())
            .filter(|i| self.columns[*i].to_lowercase().contains("unit"))
            .collect()
    }
    fn rows(&self, entry: &[Value]) -> i64 {
        if self.counted {
            entry.last().and_then(Value::as_i64).unwrap_or(0)
        } else {
            0
        }
    }
    /// The entries a restriction on one of its columns keeps, or None when that is not certain.
    fn keep(&self, column: usize, restriction: &Restriction) -> Option<BTreeSet<usize>> {
        let mut kept = BTreeSet::new();
        for (i, entry) in self.entries.iter().enumerate() {
            let held = &entry[column];
            let hit = match restriction {
                Restriction::Values(values) => {
                    let mut any = false;
                    for v in values {
                        any |= equal(held, v, self.kinds[column])?;
                    }
                    any
                }
                Restriction::Like(pattern, insensitive) => match (held, self.kinds[column]) {
                    (Value::Null, _) => false,
                    (Value::String(s), Some(Kind::Text)) => like(pattern, s, *insensitive),
                    _ => return None,
                },
            };
            if hit {
                kept.insert(i);
            }
        }
        Some(kept)
    }
}

/// A card column under its casts, outermost first, as one side of a join compares it.
type Side = (Pair, Vec<String>);
/// A value list's column kind and its held values with their row counts.
type ValueList = (Option<Kind>, Vec<(Value, i64)>);
/// A measured join: from, to, rows, matched, and the two sides of the condition the card measured.
type MeasuredJoin = (Pair, Pair, i64, i64, Option<[Side; 2]>);

struct Card {
    values: HashMap<Pair, ValueList>,
    lookups: Vec<Lookup>,
    rows: HashMap<String, i64>,
    joins: Vec<MeasuredJoin>,
}

/// The type each card column has in the source's registration (`relations[].columns[].type`).
fn column_types(relations: &Value) -> HashMap<Pair, String> {
    let mut types = HashMap::new();
    for r in relations.as_array().into_iter().flatten() {
        let Some(relation) = r["name"].as_str() else {
            continue;
        };
        for c in r["columns"].as_array().into_iter().flatten() {
            if let (Some(column), Some(sql_type)) = (c["name"].as_str(), c["type"].as_str()) {
                types.insert(
                    (relation.to_lowercase(), column.to_lowercase()),
                    sql_type.to_owned(),
                );
            }
        }
    }
    types
}

/// Both sides of a join condition as the card wrote it (`relation.column` with its casts on each side of `=`).
fn join_sides(on: &str) -> Option<[Side; 2]> {
    let parsed = pg_query::parse(&format!("SELECT 1 WHERE {on}")).ok()?;
    let tree = serde_json::to_value(&parsed.protobuf).ok()?;
    let e = node(tree.pointer("/stmts/0/stmt/node/SelectStmt/where_clause")?).get("AExpr")?;
    if e["kind"].as_i64() != Some(AEXPR_OP) || operator(e) != "=" {
        return None;
    }
    let side = |v: &Value| -> Option<Side> {
        match cast_column(v)? {
            (fields, casts) if fields.len() == 2 => {
                Some(((fields[0].clone(), fields[1].clone()), casts))
            }
            _ => None,
        }
    };
    Some([side(&e["lexpr"])?, side(&e["rexpr"])?])
}

fn dotted(text: &str) -> Option<(String, String)> {
    let (relation, column) = text.split_once('.')?;
    Some((relation.to_owned(), column.to_owned()))
}

impl Card {
    fn read(card: &Value, types: &HashMap<Pair, String>) -> Card {
        let kind_of = |pair: &Pair| kind(types.get(pair).map(String::as_str));
        // A count the card estimated (a sample or the planner's) is no total a fact may cite.
        let rows = card["relations"]
            .as_array()
            .into_iter()
            .flatten()
            .filter(|r| r.get("estimate").is_none())
            .filter_map(|r| Some((r["name"].as_str()?.to_owned(), r["rows"].as_i64()?)))
            .collect::<HashMap<_, _>>();
        let values = card["values"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|v| {
                let column = dotted(v["column"].as_str()?)?;
                let held = v["values"]
                    .as_array()?
                    .iter()
                    .filter_map(|pair| Some((pair.get(0)?.clone(), pair.get(1)?.as_i64()?)))
                    .collect();
                Some((column.clone(), (kind_of(&column), held)))
            })
            .collect();
        let lookups = card["lookups"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|l| {
                let relation = l["relation"].as_str()?.to_owned();
                let columns: Vec<String> = l["columns"]
                    .as_array()?
                    .iter()
                    .filter_map(|c| c.as_str().map(str::to_owned))
                    .collect();
                let entries: Vec<Vec<Value>> = l["entries"]
                    .as_array()?
                    .iter()
                    .filter_map(|e| e.as_array().cloned())
                    .filter(|e| e.len() == columns.len())
                    .collect();
                let complete = rows
                    .get(&relation)
                    .is_some_and(|n| *n == entries.len() as i64);
                let kinds = columns
                    .iter()
                    .map(|c| kind_of(&(relation.clone(), c.clone())))
                    .collect();
                (columns.len() >= 2).then(|| Lookup {
                    label: l["label"].as_str().unwrap_or_default().to_owned(),
                    counted: l.get("count_by").is_some(),
                    kinds,
                    relation,
                    columns,
                    entries,
                    complete,
                })
            })
            .collect();
        let joins = card["joins"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|j| {
                Some((
                    dotted(j["from"].as_str()?)?,
                    dotted(j["to"].as_str()?)?,
                    j["rows"].as_i64()?,
                    j["matched"].as_i64()?,
                    j["on"].as_str().and_then(join_sides),
                ))
            })
            .collect();
        Card {
            values,
            lookups,
            rows,
            joins,
        }
    }
}

fn node(value: &Value) -> &Value {
    value.get("node").unwrap_or(value)
}

/// The literal a node holds, typed; None for anything else, a NULL, a float, or a cast this module does not apply
/// exactly (any with a type modifier, such as `varchar(3)` or `numeric(5, 1)`).
fn literal(value: &Value) -> Option<Lit> {
    let value = node(value);
    if let Some(cast) = value.get("TypeCast") {
        let name = &cast["type_name"];
        if name["typmods"].as_array().is_some_and(|m| !m.is_empty())
            || name["array_bounds"]
                .as_array()
                .is_some_and(|b| !b.is_empty())
        {
            return None;
        }
        let to = name["names"]
            .as_array()
            .and_then(|n| n.last())
            .and_then(|n| n.pointer("/node/String/sval"))
            .and_then(Value::as_str)
            .unwrap_or_default();
        return match (to, literal(&cast["arg"])?) {
            ("int2" | "int4" | "int8", Lit::Int(i)) => Some(Lit::Int(i)),
            ("int2" | "int4" | "int8", Lit::Str(s)) => s.trim().parse().ok().map(Lit::Int),
            ("numeric", Lit::Int(i)) => Some(Lit::Int(i)),
            ("numeric", Lit::Dec(d)) => Some(Lit::Dec(d)),
            ("numeric", Lit::Str(s)) => decimal(&s),
            ("text" | "varchar", Lit::Str(s)) => Some(Lit::Str(s)),
            _ => None,
        };
    }
    let c = value.get("AConst")?;
    if c["isnull"].as_bool() == Some(true) {
        return None;
    }
    let val = &c["val"];
    if let Some(s) = val.pointer("/Sval/sval").and_then(Value::as_str) {
        return Some(Lit::Str(s.to_owned()));
    }
    if let Some(i) = val.pointer("/Ival/ival").and_then(Value::as_i64) {
        return Some(Lit::Int(i));
    }
    if val.get("Ival").is_some() {
        return Some(Lit::Int(0));
    }
    // A number with a point, or one too large for an integer, keeps its exact digits.
    if let Some(f) = val.pointer("/Fval/fval").and_then(Value::as_str) {
        return decimal(f);
    }
    val.pointer("/Boolval/boolval")
        .map(|b| Lit::Bool(b.as_bool().unwrap_or(false)))
        .or_else(|| val.get("Boolval").map(|_| Lit::Bool(false)))
}

/// A column under its casts: its fields and each cast's type name, outermost first; None for anything else or a cast
/// with a type modifier.
fn cast_column(value: &Value) -> Option<(Vec<String>, Vec<String>)> {
    let mut casts = Vec::new();
    let mut v = node(value);
    while let Some(cast) = v.get("TypeCast") {
        let name = &cast["type_name"];
        if present(&name["typmods"]) || present(&name["array_bounds"]) {
            return None;
        }
        casts.push(
            name["names"]
                .as_array()?
                .last()?
                .pointer("/node/String/sval")?
                .as_str()?
                .to_lowercase(),
        );
        v = node(&cast["arg"]);
    }
    Some((column_fields(v)?, casts))
}

fn column_fields(value: &Value) -> Option<Vec<String>> {
    let fields = node(value).get("ColumnRef")?["fields"].as_array()?;
    fields
        .iter()
        .map(|f| {
            f.pointer("/node/String/sval")
                .and_then(Value::as_str)
                .map(str::to_lowercase)
        })
        .collect()
}

fn operator(expr: &Value) -> String {
    expr["name"]
        .as_array()
        .and_then(|n| n.last())
        .and_then(|n| n.pointer("/node/String/sval"))
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_owned()
}

/// What one comparison restricts a column to.
#[derive(Clone, Debug)]
enum Restriction {
    Values(Vec<Lit>),
    Like(String, bool),
}

/// A comparison of a column with literals or a pattern, read from an `A_Expr`.
fn restriction(expr: &Value) -> Option<(Vec<String>, Restriction)> {
    let (left, right) = (&expr["lexpr"], &expr["rexpr"]);
    match (expr["kind"].as_i64()?, operator(expr).as_str()) {
        // Equality is symmetric, so the column may stand on either side.
        (AEXPR_OP, "=") => {
            if let (Some(c), Some(l)) = (column_fields(left), literal(right)) {
                Some((c, Restriction::Values(vec![l])))
            } else {
                Some((
                    column_fields(right)?,
                    Restriction::Values(vec![literal(left)?]),
                ))
            }
        }
        (AEXPR_IN, "=") => {
            let items = node(right).pointer("/List/items")?.as_array()?;
            let values: Option<Vec<Lit>> = items.iter().map(literal).collect();
            Some((column_fields(left)?, Restriction::Values(values?)))
        }
        // LIKE is not symmetric: only the column on the left, matched against the pattern on the right.
        (AEXPR_LIKE, "~~") | (AEXPR_ILIKE, "~~*") => match literal(right)? {
            Lit::Str(p) => Some((
                column_fields(left)?,
                Restriction::Like(p, expr["kind"].as_i64() == Some(AEXPR_ILIKE)),
            )),
            _ => None,
        },
        _ => None,
    }
}

/// One `FROM` item: the name a column reaches it by (its alias, or its relation without one) and the card relation it
/// reads, None for the name of a `WITH` query.
struct Instance {
    name: String,
    relation: Option<String>,
}

/// A join's `ON` clause and the instances an outer join keeps whole (none for an inner join).
struct JoinOn<'a> {
    quals: &'a Value,
    kept: BTreeSet<String>,
}

/// A card column, as relation and column.
type Pair = (String, String);
/// The restrictions a lookup instance's own conditions put on it, by column; None when they are not all known.
type Selection = Option<Vec<(String, Restriction)>>;

/// Whether a parse field holds something: not null, an empty list or false.
fn present(value: &Value) -> bool {
    match value {
        Value::Null => false,
        Value::Array(items) => !items.is_empty(),
        Value::Bool(b) => *b,
        _ => true,
    }
}

/// One `FROM` item as its instances and joins; None for anything but a relation or a join of them, a relation whose
/// alias renames its columns, or a join with an alias of its own, which hides its relations' names.
fn from_item<'a>(
    value: &'a Value,
    ctes: &HashSet<String>,
    instances: &mut Vec<Instance>,
    joins: &mut Vec<JoinOn<'a>>,
) -> Option<()> {
    let n = node(value);
    if let Some(r) = n.get("RangeVar") {
        if present(&r["alias"]["colnames"]) {
            return None;
        }
        let relation = r["relname"].as_str()?.to_lowercase();
        let qualified = r["schemaname"].as_str().is_some_and(|s| !s.is_empty());
        let name = r["alias"]["aliasname"]
            .as_str()
            .map_or_else(|| relation.clone(), str::to_lowercase);
        let card = qualified || !ctes.contains(&relation);
        instances.push(Instance {
            name,
            relation: card.then_some(relation),
        });
        return Some(());
    }
    let j = n.get("JoinExpr")?;
    if present(&j["alias"]) || present(&j["join_using_alias"]) {
        return None;
    }
    let first = instances.len();
    from_item(&j["larg"], ctes, instances, joins)?;
    let middle = instances.len();
    from_item(&j["rarg"], ctes, instances, joins)?;
    let names = |range: std::ops::Range<usize>| -> BTreeSet<String> {
        instances[range].iter().map(|i| i.name.clone()).collect()
    };
    let kept = match j["jointype"].as_i64()? {
        JOIN_INNER => BTreeSet::new(),
        JOIN_LEFT => names(first..middle),
        JOIN_RIGHT => names(middle..instances.len()),
        JOIN_FULL => names(first..instances.len()),
        _ => return None,
    };
    joins.push(JoinOn {
        quals: &j["quals"],
        kept,
    });
    Some(())
}

/// Where a column reference leads in one `SELECT`.
enum Placed<'a> {
    /// A column of a card relation instance: the instance's name, its relation and the column.
    Card(&'a str, &'a str, String),
    /// A column of a `WITH` query, which is not a card column.
    Other,
    /// Not placed with certainty.
    Unknown,
}

fn place<'a>(fields: &[String], instances: &'a [Instance]) -> Placed<'a> {
    let at = |instance: &'a Instance, column: &String| match &instance.relation {
        Some(relation) => Placed::Card(&instance.name, relation, column.clone()),
        None => Placed::Other,
    };
    match fields {
        [column] if instances.len() == 1 => at(&instances[0], column),
        [qualifier, column] | [_, qualifier, column] => instances
            .iter()
            .find(|i| i.name == *qualifier)
            .map_or(Placed::Unknown, |i| at(i, column)),
        _ => Placed::Unknown,
    }
}

/// The conjuncts of a predicate: the arguments of nested ANDs, the predicate itself otherwise.
fn conjuncts(value: &Value) -> Vec<&Value> {
    let n = node(value);
    match n.get("BoolExpr") {
        Some(b) if b["boolop"].as_i64() == Some(AND_EXPR) => b["args"]
            .as_array()
            .into_iter()
            .flatten()
            .flat_map(conjuncts)
            .collect(),
        _ if n.is_null() => vec![],
        _ => vec![n],
    }
}

/// Every `A_Expr` of a predicate.
fn comparisons<'a>(value: &'a Value, out: &mut Vec<&'a Value>) {
    match value {
        Value::Object(map) => {
            if let Some(e) = map.get("AExpr") {
                out.push(e);
            }
            map.values().for_each(|v| comparisons(v, out));
        }
        Value::Array(items) => items.iter().for_each(|v| comparisons(v, out)),
        _ => {}
    }
}

/// Every column a predicate names.
fn columns_in(value: &Value, out: &mut Vec<Vec<String>>) {
    match value {
        Value::Object(map) => {
            if let Some(fields) = column_fields(value) {
                out.push(fields);
                return;
            }
            if map.contains_key("ColumnRef") {
                // A reference this module cannot spell, such as `x.*`.
                out.push(Vec::new());
                return;
            }
            map.values().for_each(|v| columns_in(v, out));
        }
        Value::Array(items) => items.iter().for_each(|v| columns_in(v, out)),
        _ => {}
    }
}

/// What the walk gathers from every `SELECT` of the query.
#[derive(Default)]
struct Found {
    /// Each comparison of a card column with literals or a pattern, anywhere in a filter.
    restrictions: Vec<(Pair, Restriction)>,
    /// Join equalities at conjunct level, each side with its casts, and the relations the join keeps whole.
    joins: Vec<(Side, Side, BTreeSet<String>)>,
    /// Per card relation instance of each query: the restrictions its own conditions put on it, or None.
    selections: Vec<(String, Selection)>,
}

/// One `SELECT` of a query this module reads whole; None when its `FROM` holds something it does not read.
fn read_select(stmt: &Value, ctes: &HashSet<String>, found: &mut Found) -> Option<()> {
    let (mut instances, mut joins) = (Vec::new(), Vec::new());
    for item in stmt["from_clause"].as_array().into_iter().flatten() {
        from_item(item, ctes, &mut instances, &mut joins)?;
    }
    let names: HashSet<&str> = instances.iter().map(|i| i.name.as_str()).collect();
    if names.len() != instances.len() {
        return None;
    }
    // The filters: WHERE, then each join's ON with the instances an outer join keeps whole.
    let mut filters: Vec<(&Value, &BTreeSet<String>)> = Vec::new();
    let none = BTreeSet::new();
    filters.push((&stmt["where_clause"], &none));
    for j in &joins {
        filters.push((j.quals, &j.kept));
    }
    // Each value a filter or HAVING compares a card column with, wherever it stands.
    let having = &stmt["having_clause"];
    for predicate in filters.iter().map(|(p, _)| *p).chain([having]) {
        let mut all = Vec::new();
        comparisons(predicate, &mut all);
        for expr in all {
            if let Some((fields, r)) = restriction(expr)
                && let Placed::Card(_, relation, column) = place(&fields, &instances)
            {
                found.restrictions.push(((relation.to_owned(), column), r));
            }
        }
    }
    // Each card instance's selection from its own conditions. Any condition that names it otherwise, a restriction in
    // an outer join's ON, a column not placed, or HAVING leaves it unknown.
    let mut selection: BTreeMap<&str, Selection> = instances
        .iter()
        .filter(|i| i.relation.is_some())
        .map(|i| (i.name.as_str(), Some(Vec::new())))
        .collect();
    let mut all_unknown = present(having);
    for (predicate, kept) in &filters {
        let outer = !kept.is_empty();
        for conjunct in conjuncts(predicate) {
            let expr = conjunct.get("AExpr");
            if let Some((fields, r)) = expr.and_then(restriction) {
                match place(&fields, &instances) {
                    Placed::Card(name, _, column) => match selection.get_mut(name) {
                        Some(Some(list)) if !outer => list.push((column, r)),
                        Some(slot) => *slot = None,
                        None => {}
                    },
                    Placed::Other => {}
                    Placed::Unknown => all_unknown = true,
                }
                continue;
            }
            // An equality of columns of two card instances: a join, which the card measures.
            if let Some(e) =
                expr.filter(|e| e["kind"].as_i64() == Some(AEXPR_OP) && operator(e) == "=")
                && let (Some((l, lc)), Some((r, rc))) =
                    (cast_column(&e["lexpr"]), cast_column(&e["rexpr"]))
                && let (Placed::Card(a, ra, ca), Placed::Card(b, rb, cb)) =
                    (place(&l, &instances), place(&r, &instances))
                && a != b
            {
                let kept_relations = instances
                    .iter()
                    .filter(|i| kept.contains(&i.name))
                    .filter_map(|i| i.relation.clone())
                    .collect();
                found.joins.push((
                    ((ra.to_owned(), ca), lc),
                    ((rb.to_owned(), cb), rc),
                    kept_relations,
                ));
                continue;
            }
            let mut named = Vec::new();
            columns_in(conjunct, &mut named);
            for fields in named {
                match place(&fields, &instances) {
                    Placed::Card(name, ..) => {
                        selection.insert(name, None);
                    }
                    Placed::Other => {}
                    Placed::Unknown => all_unknown = true,
                }
            }
        }
    }
    for instance in &instances {
        if let (Some(relation), Some(s)) =
            (&instance.relation, selection.remove(instance.name.as_str()))
        {
            found
                .selections
                .push((relation.clone(), if all_unknown { None } else { s }));
        }
    }
    Some(())
}

/// A `SELECT` with no set operation.
fn plain(stmt: &Value) -> bool {
    stmt["op"].as_i64().unwrap_or(1) <= 1 && !present(&stmt["larg"]) && !present(&stmt["rarg"])
}

/// Counts the queries nested anywhere in a value, and whether a `FROM` item or expression hides one or reads a
/// function.
fn nesting(value: &Value, selects: &mut usize, hidden: &mut bool) {
    match value {
        Value::Object(map) => {
            for (key, inner) in map {
                match key.as_str() {
                    "SelectStmt" => *selects += 1,
                    "SubLink" | "RangeSubselect" | "RangeFunction" | "RangeTableFunc"
                    | "RangeTableSample" | "JsonTable" => *hidden = true,
                    _ => {}
                }
                nesting(inner, selects, hidden);
            }
        }
        Value::Array(items) => items.iter().for_each(|v| nesting(v, selects, hidden)),
        _ => {}
    }
}

/// The facts of a query of the shape this module reads whole, gathered into `found`; None for any other query.
fn read(top: &Value, found: &mut Found) -> Option<()> {
    if !plain(top) {
        return None;
    }
    // Each WITH query sees the names of those before it; the main query sees them all.
    let mut ctes = HashSet::new();
    let mut bodies = Vec::new();
    let with = &top["with_clause"];
    if present(with) {
        if with["recursive"].as_bool() == Some(true) {
            return None;
        }
        for cte in with["ctes"].as_array()? {
            let c = node(cte).get("CommonTableExpr")?;
            let body = node(&c["ctequery"]).get("SelectStmt")?;
            if !plain(body) || present(&body["with_clause"]) {
                return None;
            }
            bodies.push((body, ctes.clone()));
            if !ctes.insert(c["ctename"].as_str()?.to_lowercase()) {
                return None;
            }
        }
    }
    let (mut selects, mut hidden) = (0, false);
    nesting(top, &mut selects, &mut hidden);
    if hidden || selects != bodies.len() {
        return None;
    }
    for (body, seen) in &bodies {
        read_select(body, seen, found)?;
    }
    read_select(top, &ctes, found)
}

/// SQL's `LIKE`: `%` any run of characters, `_` one character, a backslash takes the next character as itself.
pub fn like(pattern: &str, text: &str, insensitive: bool) -> bool {
    let fold = |s: &str| -> Vec<char> {
        if insensitive {
            s.to_lowercase().chars().collect()
        } else {
            s.chars().collect()
        }
    };
    let (p, t) = (fold(pattern), fold(text));
    let mut tokens: Vec<(bool, Option<char>)> = Vec::new();
    let mut i = 0;
    while i < p.len() {
        match p[i] {
            '\\' if i + 1 < p.len() => {
                tokens.push((false, Some(p[i + 1])));
                i += 1;
            }
            '%' => tokens.push((true, None)),
            '_' => tokens.push((false, None)),
            c => tokens.push((false, Some(c))),
        }
        i += 1;
    }
    let mut matched = vec![false; t.len() + 1];
    matched[0] = true;
    for (any, c) in tokens {
        let mut next = vec![false; t.len() + 1];
        if any {
            let mut reached = false;
            for j in 0..=t.len() {
                reached |= matched[j];
                next[j] = reached;
            }
        } else {
            for j in 1..=t.len() {
                next[j] = matched[j - 1] && c.is_none_or(|c| c == t[j - 1]);
            }
        }
        matched = next;
    }
    matched[t.len()]
}

fn words(text: &str) -> BTreeSet<String> {
    text.to_lowercase()
        .split(|c: char| !c.is_alphanumeric())
        .filter(|w| !w.is_empty())
        .map(str::to_owned)
        .collect()
}

/// How close a held text is to a value the query names: shared words count twice, one text holding the other once.
fn closeness(wanted: &str, held: &str) -> usize {
    let shared = words(wanted).intersection(&words(held)).count();
    let (a, b) = (wanted.to_lowercase(), held.to_lowercase());
    let contained = !a.is_empty() && !b.is_empty() && (a.contains(&b) || b.contains(&a));
    shared * 2 + usize::from(contained)
}

fn text(value: &Value) -> Option<&str> {
    value.as_str()
}

/// The query's facts, most telling first, and how many were left out past `FACTS_MAX`: values the card does not hold,
/// patterns that match nothing, units the entries a lookup's own conditions keep differ in, rows joins drop, what
/// patterns match, entries held and rows a filter keeps. A query PostgreSQL cannot parse, a query of another shape, or a
/// card without these sections, gives none.
pub fn facts(sql: &str, card: &Value, relations: &Value) -> (Vec<Value>, usize) {
    let (facts, omitted, _) = facts_recorded(sql, card, relations);
    (facts, omitted)
}

/// Computation uses the complete stored card. Every string and key in the model copy is screened; counts,
/// matches, found and complete still describe the original card, and the examples keep their structure.
pub fn facts_recorded(
    sql: &str,
    card: &Value,
    relations: &Value,
) -> (Vec<Value>, usize, Vec<Value>) {
    let (ranks, mut facts): (Vec<_>, Vec<_>) =
        computed_facts(sql, card, relations).into_iter().unzip();
    let withheld = screen_facts(&mut facts, card["source"].as_str().unwrap_or_default());
    let mut ranked: Vec<_> = ranks.into_iter().zip(facts).collect();
    ranked.sort_by_key(|(rank, _)| *rank);
    let omitted = ranked.len().saturating_sub(FACTS_MAX);
    (
        ranked.into_iter().take(FACTS_MAX).map(|(_, f)| f).collect(),
        omitted,
        withheld,
    )
}

/// Also applied to stored revision feedback, including turns written before screening existed. All card-derived
/// strings and keys are covered, not just named samples. Nothing here filters the computation's input or changes
/// its measured counts or the number of examples; failing strings become markers in the model view.
pub(crate) fn screen_facts(facts: &mut [Value], source: &str) -> Vec<Value> {
    // Card-derived fact boundary, shared by fresh facts and revision replay: every string value and object key
    // passes screen::screen before ranking, sizing, recording or sending, even when the card itself was left out.
    let (shown, held) = screen::screen_view(&json!({"facts": facts}), screen::source_key(source));
    facts.clone_from_slice(shown["facts"].as_array().expect("fact list"));
    held
}

fn computed_facts(sql: &str, card: &Value, relations: &Value) -> Vec<(u8, Value)> {
    let Ok(parsed) = pg_query::parse(sql) else {
        return Vec::new();
    };
    let Ok(tree) = serde_json::to_value(&parsed.protobuf) else {
        return Vec::new();
    };
    let Some(top) = tree.pointer("/stmts/0/stmt/node/SelectStmt") else {
        return Vec::new();
    };
    let card = Card::read(card, &column_types(relations));
    let mut found = Found::default();
    if read(top, &mut found).is_none() {
        return Vec::new();
    }

    let mut ranked: Vec<(u8, Value)> = Vec::new();
    let mut done = BTreeSet::new();
    for ((relation, column), restriction) in &found.restrictions {
        let dotted = format!("{relation}.{column}");
        let pair = (relation.clone(), column.clone());
        match restriction {
            Restriction::Values(values) => {
                for value in values {
                    if !done.insert((dotted.clone(), value.key())) {
                        continue;
                    }
                    if let Some((kind, held)) = card.values.get(&pair) {
                        let mut hit = None;
                        let mut certain = true;
                        for (v, rows) in held {
                            match equal(v, value, *kind) {
                                // The card groups by the value's text, so equal numbers at different scales
                                // (1.0 and 1.00) are separate groups whose rows add up.
                                Some(true) => hit = Some(hit.unwrap_or(0) + *rows),
                                Some(false) => {}
                                None => certain = false,
                            }
                        }
                        match (hit, certain) {
                            (Some(rows), _) => ranked.push((6, json!({"kind": "filter", "column": dotted,
                                "value": value.shown(), "rows": rows, "of": card.rows.get(relation)}))),
                            (None, true) => {
                                let wanted = match value {
                                    Lit::Str(s) => s.clone(),
                                    other => other.key(),
                                };
                                let mut near: Vec<&(Value, i64)> = held
                                    .iter()
                                    .filter(|(v, _)| text(v).is_some_and(|t| closeness(&wanted, t) > 0))
                                    .collect();
                                near.sort_by(|a, b| {
                                    closeness(&wanted, text(&b.0).unwrap_or_default())
                                        .cmp(&closeness(&wanted, text(&a.0).unwrap_or_default()))
                                        .then(b.1.cmp(&a.1))
                                });
                                let nearest: Vec<Value> = near
                                    .iter()
                                    .take(NEAREST_MAX)
                                    .map(|(v, rows)| json!({"value": v, "rows": rows}))
                                    .collect();
                                ranked.push((0, json!({"kind": "entry", "column": dotted, "value": value.shown(),
                                    "found": false, "values_held": held.len(), "nearest": nearest, "complete": true})));
                            }
                            (None, false) => {}
                        }
                        continue;
                    }
                    for lookup in &card.lookups {
                        let Some(index) = lookup.index(column) else {
                            continue;
                        };
                        if lookup.relation != *relation || (index > 1) {
                            continue;
                        }
                        let Some(hits) =
                            lookup.keep(index, &Restriction::Values(vec![value.clone()]))
                        else {
                            continue;
                        };
                        if hits.is_empty() {
                            let wanted = match value {
                                Lit::Str(s) => s.clone(),
                                other => other.key(),
                            };
                            let mut near: Vec<usize> = (0..lookup.entries.len())
                                .filter(|i| {
                                    index == 1
                                        && text(&lookup.entries[*i][1])
                                            .is_some_and(|t| closeness(&wanted, t) > 0)
                                })
                                .collect();
                            near.sort_by(|a, b| {
                                let (ea, eb) = (&lookup.entries[*a], &lookup.entries[*b]);
                                closeness(&wanted, text(&eb[1]).unwrap_or_default())
                                    .cmp(&closeness(&wanted, text(&ea[1]).unwrap_or_default()))
                                    .then(lookup.rows(eb).cmp(&lookup.rows(ea)))
                            });
                            let nearest: Vec<Value> = near
                                .iter()
                                .take(NEAREST_MAX)
                                .map(|i| lookup.shown(&lookup.entries[*i]))
                                .collect();
                            ranked.push((
                                0,
                                json!({"kind": "entry", "column": dotted, "value": value.shown(),
                                "found": false, "nearest": nearest, "complete": lookup.complete}),
                            ));
                        } else {
                            let entries: Vec<Value> = hits
                                .iter()
                                .take(EXAMPLES_MAX)
                                .map(|i| lookup.shown(&lookup.entries[*i]))
                                .collect();
                            ranked.push((
                                5,
                                json!({"kind": "entry", "column": dotted, "value": value.shown(),
                                "found": true, "entries": entries}),
                            ));
                        }
                    }
                }
            }
            Restriction::Like(pattern, insensitive) => {
                if !done.insert((dotted.clone(), format!("{insensitive}:{pattern}"))) {
                    continue;
                }
                let case = if *insensitive {
                    "insensitive"
                } else {
                    "sensitive"
                };
                if let Some((kind, held)) = card.values.get(&pair) {
                    if *kind != Some(Kind::Text) {
                        continue;
                    }
                    let hits: Vec<Value> = held
                        .iter()
                        .filter(|(v, _)| text(v).is_some_and(|t| like(pattern, t, *insensitive)))
                        .map(|(v, rows)| json!({"value": v, "rows": rows}))
                        .collect();
                    let rank = if hits.is_empty() { 1 } else { 4 };
                    ranked.push((rank, json!({"kind": "pattern", "column": dotted, "pattern": pattern, "case": case,
                        "matches": hits.len(), "examples": hits.into_iter().take(EXAMPLES_MAX).collect::<Vec<_>>(),
                        "complete": true})));
                    continue;
                }
                for lookup in &card.lookups {
                    if lookup.relation != *relation || lookup.label != *column {
                        continue;
                    }
                    let Some(hits) = lookup.keep(1, restriction) else {
                        continue;
                    };
                    let rank = if hits.is_empty() { 1 } else { 4 };
                    let mut fact = json!({"kind": "pattern", "column": dotted, "pattern": pattern, "case": case,
                        "matches": hits.len(), "complete": lookup.complete});
                    if hits.is_empty() {
                        let stripped: String = pattern
                            .chars()
                            .map(|c| if c == '%' || c == '_' { ' ' } else { c })
                            .collect();
                        let mut near: Vec<usize> = (0..lookup.entries.len())
                            .filter(|i| {
                                text(&lookup.entries[*i][1])
                                    .is_some_and(|t| closeness(&stripped, t) > 0)
                            })
                            .collect();
                        near.sort_by(|a, b| {
                            closeness(&stripped, text(&lookup.entries[*b][1]).unwrap_or_default())
                                .cmp(&closeness(
                                    &stripped,
                                    text(&lookup.entries[*a][1]).unwrap_or_default(),
                                ))
                        });
                        fact["nearest"] = near
                            .iter()
                            .take(NEAREST_MAX)
                            .map(|i| lookup.shown(&lookup.entries[*i]))
                            .collect();
                    } else {
                        fact["examples"] = hits
                            .iter()
                            .take(EXAMPLES_MAX)
                            .map(|i| lookup.shown(&lookup.entries[*i]))
                            .collect();
                    }
                    ranked.push((rank, fact));
                }
            }
        }
    }
    // The units of the entries one lookup instance's own conditions keep together, when every one of them is known
    // and the entries differ in unit.
    for (relation, restrictions) in &found.selections {
        let Some(restrictions) = restrictions.as_ref().filter(|r| !r.is_empty()) else {
            continue;
        };
        for lookup in card.lookups.iter().filter(|l| l.relation == *relation) {
            let mut selected: Option<BTreeSet<usize>> = Some((0..lookup.entries.len()).collect());
            for (column, r) in restrictions {
                let kept = lookup.index(column).and_then(|i| lookup.keep(i, r));
                selected = match (selected, kept) {
                    (Some(s), Some(k)) => Some(s.intersection(&k).copied().collect()),
                    _ => None,
                };
            }
            let Some(selected) = selected else {
                continue;
            };
            for unit in lookup.unit_columns() {
                let mut units: BTreeMap<String, usize> = BTreeMap::new();
                for i in &selected {
                    if let Some(u) = text(&lookup.entries[*i][unit]) {
                        *units.entry(u.to_owned()).or_default() += 1;
                    }
                }
                if units.len() > 1 {
                    let fact = json!({"kind": "unit", "lookup": lookup.relation, "column": lookup.columns[unit],
                        "units": units.iter().map(|(u, n)| json!({"unit": u, "entries": n})).collect::<Vec<_>>()});
                    if !ranked.iter().any(|(_, f)| *f == fact) {
                        ranked.push((2, fact));
                    }
                }
            }
        }
    }
    // Joins the card measured, compared as the card compared them: the `from` side loses the rows whose value names no
    // row on the other side, unless the join keeps that side whole. A join with other casts compares otherwise.
    for (a, b, kept) in &found.joins {
        for (from, to, rows, matched, sides) in &card.joins {
            let Some([x, y]) = sides else {
                continue;
            };
            let same = (x == a && y == b) || (x == b && y == a);
            if !same || x.0 != *from || y.0 != *to || matched >= rows || kept.contains(&from.0) {
                continue;
            }
            let fact = json!({"kind": "join_drop", "from": format!("{}.{}", from.0, from.1),
                "to": format!("{}.{}", to.0, to.1), "rows": rows, "matched": matched, "dropped": rows - matched});
            if !ranked.iter().any(|(_, f)| *f == fact) {
                ranked.push((3, fact));
            }
        }
    }
    ranked
}

#[cfg(test)]
mod tests {
    use super::*;

    fn card() -> Value {
        json!({"schema": "enrichedlab.sql-source-card.v1", "source": "example", "manifest_sha256": "a".repeat(64),
            "relations": [
                {"name": "food", "rows": 4, "keys": [["fdc_id"]]},
                {"name": "food_nutrient", "rows": 9, "keys": [["id"]]},
                {"name": "nutrient", "rows": 10, "keys": [["id"]]},
                {"name": "food_category", "rows": 2, "keys": [["id"]]}],
            "joins": [
                {"from": "food_nutrient.fdc_id", "to": "food.fdc_id", "on": "food_nutrient.fdc_id = food.fdc_id", "rows": 9, "matched": 9},
                {"from": "food.food_category_id", "to": "food_category.id", "on": "food.food_category_id = food_category.id::text", "rows": 3, "matched": 2}],
            "values": [{"column": "food.data_type", "values": [["foundation_food", 1], ["sr_legacy_food", 3]]},
                       {"column": "food.publication_year", "values": [["2019", 1], ["2021", 3]]},
                       {"column": "food.untyped", "values": [["a", 4]]}],
            "lookups": [
                {"relation": "nutrient", "key": "id", "label": "name", "count_by": "food_nutrient.nutrient_id",
                 "columns": ["id", "name", "unit_name", "rows in food_nutrient"],
                 "entries": [[1003, "Protein", "G", 4], [1008, "Energy", "KCAL", 4], [2047, "Energy (Atwater General Factors)", "KCAL", 1],
                             [1062, "Energy", "kJ", 3], [1162, "Vitamin C, total ascorbic acid", "MG", 2], [1185, "Vitamin K (phylloquinone)", "UG", 1],
                             [1106, "Vitamin A, RAE", "UG", 3], [1114, "Vitamin D (D2 + D3)", "UG", 2], [1109, "Vitamin E (alpha-tocopherol)", "MG", 2],
                             [1999, null, null, 0]]},
                {"relation": "food_category", "key": "id", "label": "description", "columns": ["id", "description"],
                 "entries": [[11, "Vegetables and Vegetable Products"], [1, "Dairy and Egg Products"]]}]})
    }

    /// The types the source registered, as `relations[].columns[].type`; `food.untyped` has none.
    fn relations() -> Value {
        let typed = |name: &str, columns: &[(&str, &str)]| json!({"name": name, "columns": columns.iter().map(|(c, t)| json!({"name": c, "type": t})).collect::<Vec<_>>()});
        json!([
            typed(
                "food",
                &[
                    ("fdc_id", "integer"),
                    ("data_type", "text"),
                    ("description", "text"),
                    ("food_category_id", "text"),
                    ("publication_year", "integer"),
                    ("untyped", "")
                ]
            ),
            typed(
                "food_nutrient",
                &[
                    ("id", "integer"),
                    ("fdc_id", "integer"),
                    ("nutrient_id", "integer"),
                    ("amount", "numeric")
                ]
            ),
            typed(
                "nutrient",
                &[("id", "integer"), ("name", "text"), ("unit_name", "text")]
            ),
            typed(
                "food_category",
                &[("id", "integer"), ("description", "text")]
            )
        ])
    }

    fn kinds(facts: &[Value]) -> Vec<&str> {
        facts.iter().map(|f| f["kind"].as_str().unwrap()).collect()
    }

    fn facts_of(sql: &str) -> Vec<Value> {
        facts(sql, &card(), &relations()).0
    }

    #[test]
    fn a_value_the_card_does_not_hold_comes_first_with_the_nearest_it_does() {
        let (facts, omitted) = facts(
            "SELECT f.description, fn.amount FROM food f JOIN food_nutrient fn ON fn.fdc_id = f.fdc_id \
             JOIN nutrient n ON n.id = fn.nutrient_id WHERE n.name = 'Vitamin C' AND f.data_type = 'foundation_food' \
             ORDER BY 1",
            &card(),
            &relations(),
        );
        assert_eq!((kinds(&facts), omitted), (vec!["entry", "filter"], 0));
        assert_eq!(
            facts[0],
            json!({"kind": "entry", "column": "nutrient.name", "value": "Vitamin C", "found": false, "complete": true,
                "nearest": [{"key": 1162, "label": "Vitamin C, total ascorbic acid", "unit_name": "MG", "rows": 2},
                            {"key": 1106, "label": "Vitamin A, RAE", "unit_name": "UG", "rows": 3},
                            {"key": 1114, "label": "Vitamin D (D2 + D3)", "unit_name": "UG", "rows": 2}]})
        );
        assert_eq!(
            facts[1],
            json!({"kind": "filter", "column": "food.data_type", "value": "foundation_food", "rows": 1, "of": 4})
        );
    }

    #[test]
    fn an_estimated_count_is_never_cited_as_a_total() {
        let mut estimated = card();
        estimated["relations"][0]["estimate"] =
            json!({"method": "block_sample", "percent": 10.0, "sampled_rows": 1});
        let (facts, _) = facts(
            "SELECT f.description FROM food f WHERE f.data_type = 'foundation_food' ORDER BY 1",
            &estimated,
            &relations(),
        );
        assert_eq!(
            facts,
            vec![
                json!({"kind": "filter", "column": "food.data_type", "value": "foundation_food", "rows": 1, "of": null})
            ]
        );
    }

    #[test]
    fn a_pattern_shows_what_it_matches_and_the_units_they_mix() {
        let sql = "SELECT n.name, avg(fn.amount) FROM nutrient n, food_nutrient fn WHERE fn.nutrient_id = n.id \
             AND n.name ILIKE 'energy%' GROUP BY n.name ORDER BY 1";
        let facts = facts_of(sql);
        let mut raw = computed_facts(sql, &card(), &relations());
        raw.sort_by_key(|(rank, _)| *rank);
        let before: Vec<Value> = raw
            .into_iter()
            .take(FACTS_MAX)
            .map(|(_, fact)| fact)
            .collect();
        assert_eq!(
            serde_json::to_vec(&facts).unwrap(),
            serde_json::to_vec(&before).unwrap()
        );
        assert_eq!(kinds(&facts), vec!["unit", "pattern"]);
        assert_eq!(
            facts[0],
            json!({"kind": "unit", "lookup": "nutrient", "column": "unit_name",
                "units": [{"unit": "KCAL", "entries": 2}, {"unit": "kJ", "entries": 1}]})
        );
        assert_eq!(
            (&facts[1]["matches"], &facts[1]["case"]),
            (&json!(3), &json!("insensitive"))
        );
        let none = facts_of("SELECT name FROM nutrient WHERE name LIKE 'vitamin c%' ORDER BY 1");
        assert_eq!(
            (&none[0]["matches"], &none[0]["nearest"][0]["key"]),
            (&json!(0), &json!(1162))
        );
    }

    #[test]
    fn units_come_only_from_restrictions_that_hold_together() {
        // The name and the key together select one entry, so no mix.
        let one = facts_of(
            "SELECT n.name FROM nutrient n WHERE n.name = 'Energy' AND n.id = 1008 ORDER BY 1",
        );
        assert!(!kinds(&one).contains(&"unit"));
        // A restriction under OR, or one this module cannot read, leaves the selection unknown: no unit claim.
        let either = facts_of(
            "SELECT n.name FROM nutrient n WHERE n.name = 'Energy' OR n.id > 2000 ORDER BY 1",
        );
        assert!(!kinds(&either).contains(&"unit"));
        let other = facts_of(
            "SELECT n.name FROM nutrient n WHERE n.name ILIKE 'energy%' AND n.id > 2000 ORDER BY 1",
        );
        assert!(!kinds(&other).contains(&"unit"));
        // Both Energy rows by name: the units differ.
        let both = facts_of("SELECT n.name FROM nutrient n WHERE n.name = 'Energy' ORDER BY 1");
        assert_eq!(kinds(&both), vec!["unit", "entry"]);
    }

    #[test]
    fn an_inner_join_the_card_measured_states_the_rows_it_drops_and_other_equalities_do_not() {
        let inner = "SELECT c.description, count(*) FROM food f JOIN food_category c ON f.food_category_id = c.id::text \
                     GROUP BY 1 ORDER BY 1";
        assert_eq!(
            facts_of(inner),
            [
                json!({"kind": "join_drop", "from": "food.food_category_id", "to": "food_category.id", "rows": 3,
                "matched": 2, "dropped": 1})
            ]
        );
        let outer = "SELECT c.description FROM food f LEFT JOIN food_category c ON f.food_category_id = c.id::text ORDER BY 1";
        assert_eq!(facts_of(outer), Vec::<Value>::new());
        let implicit = "SELECT count(*) FROM food, food_category WHERE food.food_category_id = food_category.id::text";
        assert_eq!(kinds(&facts_of(implicit)), vec!["join_drop"]);
        // An equality projected over a cross join, or inside OR, filters nothing.
        let projected = "SELECT food.food_category_id = food_category.id::text FROM food CROSS JOIN food_category ORDER BY 1";
        assert_eq!(facts_of(projected), Vec::<Value>::new());
        let ored = "SELECT count(*) FROM food f JOIN food_category c ON f.food_category_id = c.id::text OR true";
        assert_eq!(facts_of(ored), Vec::<Value>::new());
        assert_eq!(
            facts_of("SELECT count(*) FROM food_nutrient fn JOIN food f ON fn.fdc_id = f.fdc_id"),
            Vec::<Value>::new()
        );
    }

    #[test]
    fn only_a_query_this_module_reads_whole_gets_facts() {
        // A nested query, a derived table, a set operation or a recursive WITH
        // gives no facts at all, so nothing is stated about a column whose owner or meaning depends on them.
        for sql in [
            "SELECT fn.amount FROM food_nutrient fn WHERE id = 9999 AND EXISTS (SELECT 1 FROM nutrient WHERE name = 'Protein')",
            "SELECT n.name FROM nutrient n WHERE n.name = 'Energy' AND EXISTS (SELECT 1 FROM food_nutrient fn \
             WHERE fn.nutrient_id = n.id AND n.id = 1008)",
            "SELECT n.name FROM nutrient n, LATERAL (SELECT description AS name FROM food) d WHERE n.name = 'Missing'",
            "SELECT * FROM nutrient n WHERE n.id IN (SELECT 1) AND n.name = 'Missing'",
            "SELECT * FROM (SELECT description AS name FROM food) n WHERE n.name = 'Energy'",
            "WITH nutrient AS (SELECT 1 AS id, 'Missing' AS name) SELECT name FROM nutrient WHERE name = 'Missing' \
             UNION SELECT name FROM nutrient WHERE name = 'Missing'",
            "WITH RECURSIVE n AS (SELECT 1 AS id UNION ALL SELECT id + 1 FROM n) SELECT * FROM nutrient WHERE name = 'Missing'",
            "SELECT f.description, (SELECT count(*) FROM food_category c WHERE f.food_category_id = c.id::text) FROM food f",
            "SELECT * FROM generate_series(1, 2) g, nutrient WHERE nutrient.name = 'Missing'",
            "SELECT * FROM food f(a, b) WHERE f.a = 'Missing'",
            "SELECT * FROM (food f JOIN food_nutrient fn ON fn.fdc_id = f.fdc_id) AS j WHERE j.data_type = 'Missing'",
        ] {
            assert_eq!(facts_of(sql), Vec::<Value>::new(), "{sql}");
        }
    }

    #[test]
    fn columns_resolve_through_one_name_for_each_from_item() {
        // A column named through the alias and one left unqualified are the same instance, so the
        // name and the key together select one entry and no unit mix is stated.
        let one = facts_of("SELECT n.name FROM nutrient n WHERE n.name = 'Energy' AND id = 1008");
        assert!(!kinds(&one).contains(&"unit"), "{one:?}");
        // Two relations: an unqualified column is placed nowhere, so its condition makes every selection unknown.
        assert_eq!(
            facts_of("SELECT 1 FROM food, nutrient WHERE name = 'Protein'"),
            Vec::<Value>::new()
        );
        let mixed = facts_of(
            "SELECT 1 FROM nutrient n, food f WHERE n.name = 'Energy' AND description = 'x'",
        );
        assert!(!kinds(&mixed).contains(&"unit"), "{mixed:?}");
        // Two items under one name are refused by PostgreSQL and read for nothing here.
        assert_eq!(
            facts_of("SELECT * FROM food x, nutrient x WHERE x.name = 'Protein'"),
            Vec::<Value>::new()
        );
        // A WITH query's name stands for that query from the queries after it on, never for the card relation.
        assert_eq!(
            facts_of(
                "WITH nutrient AS (SELECT 1 AS name) SELECT * FROM nutrient WHERE name = 'Missing'"
            ),
            Vec::<Value>::new()
        );
        // A WITH query's own body is read, and in it a later WITH name is still the card relation.
        let body = facts_of(
            "WITH e AS (SELECT id FROM nutrient WHERE name = 'Enrgy'), nutrient AS (SELECT 1 AS id) SELECT * FROM e, nutrient",
        );
        assert_eq!(
            (&body[0]["column"], &body[0]["found"]),
            (&json!("nutrient.name"), &json!(false))
        );
        // A schema-qualified relation is the card's even when a WITH query shares its name.
        let qualified = facts_of(
            "WITH nutrient AS (SELECT 1) SELECT * FROM fndds.nutrient n WHERE n.name = 'Enrgy'",
        );
        assert_eq!(qualified[0]["column"], json!("nutrient.name"));
    }

    #[test]
    fn a_selection_is_known_only_from_the_lookups_own_conditions() {
        // A column equality within one instance is not a restriction this module reads.
        let same =
            facts_of("SELECT 1 FROM nutrient n WHERE n.name = 'Energy' AND n.name = n.unit_name");
        assert!(!kinds(&same).contains(&"unit"), "{same:?}");
        // A restriction in an outer join's ON, or any HAVING, leaves the selection unknown.
        let outer = facts_of(
            "SELECT 1 FROM food_nutrient fn LEFT JOIN nutrient n ON n.id = fn.nutrient_id AND n.name = 'Energy'",
        );
        assert!(!kinds(&outer).contains(&"unit"), "{outer:?}");
        let having = facts_of(
            "SELECT n.name FROM nutrient n WHERE n.name = 'Energy' GROUP BY n.name HAVING count(*) > 1",
        );
        assert!(!kinds(&having).contains(&"unit"), "{having:?}");
        // An inner join's ON and WHERE restrict it together; a join to another relation does not.
        let joined = facts_of(
            "SELECT 1 FROM food_nutrient fn JOIN nutrient n ON n.id = fn.nutrient_id AND n.name = 'Energy' \
             WHERE fn.amount > 0",
        );
        assert!(kinds(&joined).contains(&"unit"), "{joined:?}");
        // A comparison with a column of a WITH query makes the instance's selection unknown.
        let cte = facts_of(
            "WITH k AS (SELECT 1008 AS id) SELECT 1 FROM nutrient n, k WHERE n.name = 'Energy' AND n.id = k.id",
        );
        assert!(!kinds(&cte).contains(&"unit"), "{cte:?}");
    }

    #[test]
    fn like_reads_the_column_on_its_left_only() {
        assert_eq!(
            facts_of("SELECT 1 FROM nutrient n WHERE 'Energy%' LIKE n.name"),
            Vec::<Value>::new()
        );
        assert_eq!(
            facts_of("SELECT 1 FROM nutrient n WHERE n.name LIKE 'Energy%'")[1]["matches"],
            json!(3)
        );
    }

    #[test]
    fn values_compare_as_postgresql_compares_them() {
        // A numeric key matches a decimal literal and a quoted integer; NULL never matches.
        let decimal = facts_of("SELECT name FROM nutrient WHERE id = 1003.0 ORDER BY 1");
        assert_eq!(
            (&decimal[0]["found"], &decimal[0]["entries"][0]["key"]),
            (&json!(true), &json!(1003))
        );
        let quoted = facts_of("SELECT name FROM nutrient WHERE id = '1003' ORDER BY 1");
        assert_eq!(quoted[0]["found"], json!(true));
        let null_text = facts_of("SELECT name FROM nutrient WHERE name = 'null' ORDER BY 1");
        assert_eq!(null_text[0]["found"], json!(false));
        let all = facts_of("SELECT name FROM nutrient WHERE name LIKE '%' ORDER BY 1");
        let pattern = all.iter().find(|f| f["kind"] == "pattern").unwrap();
        assert_eq!(pattern["matches"], json!(9));
        // An exact decimal compares exactly, so a near miss is not the key.
        let near = facts_of("SELECT name FROM nutrient WHERE id = 1003.000000000000000000000001");
        assert_eq!(
            (&near[0]["found"], &near[0]["value"]),
            (&json!(false), &json!("1003.000000000000000000000001"))
        );
        let large = facts_of("SELECT name FROM nutrient WHERE id = 1003.0e0");
        assert_eq!(large, Vec::<Value>::new());
        // A cast with a type modifier, or to a float, states nothing.
        for sql in [
            "SELECT 1 FROM nutrient WHERE name = 'Protein'::varchar(3)",
            "SELECT 1 FROM nutrient WHERE id = '1003.4'::numeric(5, 0)",
            "SELECT 1 FROM nutrient WHERE id = 1003::float8",
            "SELECT 1 FROM nutrient WHERE name = 'Protein'::char(3)",
        ] {
            assert_eq!(facts_of(sql), Vec::<Value>::new(), "{sql}");
        }
        let cast = facts_of(
            "SELECT 1 FROM nutrient WHERE id = '1003'::integer AND name = 'Protein'::text",
        );
        assert!(cast.iter().all(|f| f["found"] == json!(true)), "{cast:?}");
        // A cast to a type this module does not compare, and a NULL literal, state nothing.
        assert_eq!(
            facts_of("SELECT name FROM nutrient WHERE id = '1003'::date ORDER BY 1"),
            Vec::<Value>::new()
        );
        assert_eq!(
            facts_of("SELECT name FROM nutrient WHERE name = NULL ORDER BY 1"),
            Vec::<Value>::new()
        );
        // A number against text is not decided here.
        assert_eq!(
            facts_of("SELECT 1 FROM food WHERE data_type = 5"),
            Vec::<Value>::new()
        );
    }

    #[test]
    fn keys_in_lists_resolve_and_many_facts_keep_the_most_telling() {
        let listed = facts_of("SELECT name FROM nutrient WHERE id IN (1162, 9999) ORDER BY 1");
        let found: Vec<(&Value, &Value)> =
            listed.iter().map(|f| (&f["value"], &f["found"])).collect();
        assert_eq!(
            found,
            [(&json!(9999), &json!(false)), (&json!(1162), &json!(true))]
        );
        let list: Vec<String> = (1..=12).map(|i| format!("'Missing {i}'")).collect();
        let sql = format!(
            "SELECT name FROM nutrient WHERE name IN ({}) OR name = 'Protein' ORDER BY 1",
            list.join(", ")
        );
        let (many, omitted) = facts(&sql, &card(), &relations());
        assert_eq!((many.len(), omitted), (FACTS_MAX, 5));
        assert!(many.iter().all(|f| f["found"] == json!(false)));
        assert_eq!(facts_of("SELECT FROM WHERE"), Vec::<Value>::new());
    }

    #[test]
    fn a_join_is_measured_only_with_the_casts_the_card_used() {
        // The card measured `food.food_category_id = food_category.id::text`; the same columns
        // compared as integers match other rows, so its count says nothing about that join.
        assert_eq!(
            kinds(&facts_of(
                "SELECT 1 FROM food f JOIN food_category c ON c.id::text = f.food_category_id"
            )),
            vec!["join_drop"]
        );
        for sql in [
            "SELECT 1 FROM food f JOIN food_category c ON f.food_category_id::integer = c.id",
            "SELECT 1 FROM food f JOIN food_category c ON f.food_category_id = c.id::varchar",
            "SELECT 1 FROM food f JOIN food_category c ON f.food_category_id = c.id::varchar(4)",
        ] {
            assert!(!kinds(&facts_of(sql)).contains(&"join_drop"), "{sql}");
        }
        // A card join whose condition cannot be read states nothing.
        let mut unreadable = card();
        unreadable["joins"][1]["on"] = json!("food.food_category_id = ");
        let sql = "SELECT 1 FROM food f JOIN food_category c ON f.food_category_id = c.id::text";
        assert_eq!(facts(sql, &unreadable, &relations()).0, Vec::<Value>::new());
    }

    #[test]
    fn values_compare_under_the_columns_registered_type() {
        // The card writes an integer column's values as text; '02019' reads as 2019 there.
        let padded = facts_of("SELECT 1 FROM food WHERE publication_year = '02019'");
        assert_eq!(
            (&padded[0]["kind"], &padded[0]["rows"]),
            (&json!("filter"), &json!(1))
        );
        let number = facts_of("SELECT 1 FROM food WHERE publication_year = 2021");
        assert_eq!(number[0]["rows"], json!(3));
        let missing = facts_of("SELECT 1 FROM food WHERE publication_year = 2020.5");
        assert_eq!(
            (&missing[0]["kind"], &missing[0]["found"]),
            (&json!("entry"), &json!(false))
        );
        // Input the integer type reads in ways not decided here, a pattern on a number, a column without a registered
        // type, and a source registered without types: no facts.
        for sql in [
            "SELECT 1 FROM food WHERE publication_year = '2_019'",
            "SELECT 1 FROM food WHERE publication_year = '2019.0'",
            "SELECT 1 FROM food WHERE untyped = 'a'",
        ] {
            assert_eq!(facts_of(sql), Vec::<Value>::new(), "{sql}");
        }
        let untyped = facts(
            "SELECT name FROM nutrient WHERE name = 'Protein'",
            &card(),
            &json!([]),
        )
        .0;
        assert_eq!(untyped, Vec::<Value>::new());
        // A numeric column's groups 1.0 (2 rows) and 1.00 (3 rows) are one value: 5 rows.
        let mut scaled = card();
        scaled["values"] = json!([{"column": "food_nutrient.amount", "values": [["1.0", 2], ["1.00", 3], ["2.5", 7]]}]);
        let summed = facts(
            "SELECT 1 FROM food_nutrient WHERE amount = 1",
            &scaled,
            &relations(),
        )
        .0;
        assert_eq!(
            (&summed[0]["kind"], &summed[0]["rows"]),
            (&json!("filter"), &json!(5))
        );
    }

    #[test]
    fn like_follows_sql() {
        assert!(like("vit%c%", "Vitamin C", true));
        assert!(!like("vit%c%", "Vitamin C", false));
        assert!(like("_rotein", "Protein", false));
        assert!(like("100\\%", "100%", false));
        assert!(!like("100\\%", "1000", false));
        assert!(like("%", "", false));
        assert!(!like("a_", "a", false));
    }
}
