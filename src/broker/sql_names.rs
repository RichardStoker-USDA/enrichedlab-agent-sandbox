//! Every relation a parsed SELECT reads and every function it calls, from a walk of the whole parse tree. pg_query's
//! own collectors (`ParseResult::tables`, `functions`) descend only some node kinds, so a subquery or call inside
//! `COLLATE`, a subscript, a window, an aggregate's `FILTER` or `ORDER BY`, `DISTINCT ON`, `OFFSET`, `GROUPING SETS`, a
//! `CASE` operand or the left side of `IN` was invisible to them, and they treat a CTE name as hiding a relation of the
//! same name everywhere in the query. Here a CTE name hides a relation only where PostgreSQL resolves the name to it.
use serde_json::Value;

/// A relation as written. The parser has already folded unquoted names to lowercase; quoted names keep their case.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Relation {
    pub catalog: String,
    pub schema: String,
    pub name: String,
}

impl Relation {
    /// The name as written, for refusal feedback.
    pub fn written(&self) -> String {
        [&self.catalog, &self.schema, &self.name]
            .into_iter()
            .filter(|p| !p.is_empty())
            .cloned()
            .collect::<Vec<_>>()
            .join(".")
    }
}

#[derive(Debug, Default)]
pub struct Names {
    /// Relations in the order the walk met them, CTE references left out.
    pub relations: Vec<Relation>,
    /// Each call's name parts as written, such as `["lower"]` or `["pg_catalog", "lower"]`.
    pub functions: Vec<Vec<String>>,
    /// The last part of each qualified column reference, `q.name` or `s.t.name`. PostgreSQL reads it as the call
    /// `name(q)` when `q` has no such column, so only a function one row argument reaches can be called this way, and
    /// only the source's catalog can tell a column from such a call.
    pub fields: Vec<String>,
    /// Each field of a selection `(expr).name`. PostgreSQL reads it as the call `name(expr)` when `expr` has no such
    /// field, for any function one argument reaches and any expression.
    pub selections: Vec<String>,
}

/// Collect from the top SELECT of the protobuf parse tree as JSON (`stmts[0].stmt.node.SelectStmt`).
pub fn collect(select: &Value) -> Names {
    let mut names = Names::default();
    select_stmt(select, &mut Vec::new(), &mut names);
    names
}

/// The CTE names visible at a point, one list per enclosing query level.
type Scope = Vec<Vec<String>>;

fn text(value: &Value) -> &str {
    value.as_str().unwrap_or("")
}

fn select_stmt(s: &Value, scope: &mut Scope, names: &mut Names) {
    let with = &s["with_clause"];
    let ctes: &[Value] = with["ctes"].as_array().map_or(&[], Vec::as_slice);
    let recursive = with["recursive"].as_bool().unwrap_or(false);
    let cte_names: Vec<String> = ctes
        .iter()
        .map(|c| text(&c["node"]["CommonTableExpr"]["ctename"]).to_owned())
        .collect();
    for (i, cte) in ctes.iter().enumerate() {
        // Without RECURSIVE a CTE body sees only the CTEs before it, not itself or a later one; with RECURSIVE it sees
        // every CTE of its list.
        scope.push(if recursive {
            cte_names.clone()
        } else {
            cte_names[..i].to_vec()
        });
        walk(cte, scope, names);
        scope.pop();
    }
    scope.push(cte_names);
    if let Value::Object(fields) = s {
        for (key, value) in fields {
            match key.as_str() {
                "with_clause" => {}
                // The two sides of a set operation are SELECTs stored without the node wrapper.
                "larg" | "rarg" if value.is_object() => select_stmt(value, scope, names),
                _ => walk(value, scope, names),
            }
        }
    }
    scope.pop();
}

fn walk(value: &Value, scope: &mut Scope, names: &mut Names) {
    match value {
        Value::Object(map) => {
            if map.contains_key("relname") {
                relation(map, scope, names);
            }
            if let Some(Value::Array(parts)) = map.get("funcname") {
                names.functions.push(
                    parts
                        .iter()
                        .map(|p| text(&p["node"]["String"]["sval"]).to_owned())
                        .collect(),
                );
            }
            for (key, inner) in map {
                // A node is stored as {"node": {"<Kind>": body}}. A kind starts with a capital letter and a field
                // never does; a field may itself be named `node` (`SortBy.node`), so the kind is what is matched.
                if key.starts_with(|c: char| c.is_ascii_uppercase()) {
                    node(key, inner, scope, names);
                } else {
                    walk(inner, scope, names);
                }
            }
        }
        Value::Array(items) => items.iter().for_each(|v| walk(v, scope, names)),
        _ => {}
    }
}

fn node(kind: &str, body: &Value, scope: &mut Scope, names: &mut Names) {
    match kind {
        "SelectStmt" => return select_stmt(body, scope, names),
        "ColumnRef" => {
            let parts = body["fields"].as_array().map_or(&[][..], Vec::as_slice);
            if let [_, .., last] = parts
                && let Some(name) = last["node"]["String"]["sval"].as_str()
            {
                names.fields.push(name.to_owned());
            }
        }
        "AIndirection" => {
            for part in body["indirection"].as_array().into_iter().flatten() {
                if let Some(name) = part["node"]["String"]["sval"].as_str() {
                    names.selections.push(name.to_owned());
                }
            }
        }
        _ => {}
    }
    walk(body, scope, names);
}

fn relation(map: &serde_json::Map<String, Value>, scope: &Scope, names: &mut Names) {
    let part = |key: &str| map.get(key).map_or("", text).to_owned();
    let r = Relation {
        catalog: part("catalogname"),
        schema: part("schemaname"),
        name: part("relname"),
    };
    // Only an unqualified name can name a CTE, and PostgreSQL then prefers the CTE to a relation.
    let cte = r.catalog.is_empty()
        && r.schema.is_empty()
        && scope.iter().any(|level| level.contains(&r.name));
    if !cte {
        names.relations.push(r);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn names(sql: &str) -> Names {
        let parsed = pg_query::parse(sql).unwrap();
        let tree = serde_json::to_value(&parsed.protobuf).unwrap();
        collect(tree.pointer("/stmts/0/stmt/node/SelectStmt").unwrap())
    }

    #[test]
    fn collects_every_name_pg_query_collects_and_the_ones_it_misses() {
        for sql in [
            "SELECT f.description, lower(n.name) FROM fdc.food f JOIN nutrient n ON n.id = f.fdc_id WHERE f.fdc_id IN \
             (SELECT fn.fdc_id FROM food_nutrient fn) ORDER BY count(*) OVER (PARTITION BY upper(f.description))",
            "WITH a AS (SELECT x.id FROM t1 x) SELECT a.id FROM a UNION SELECT y.id FROM t2 y ORDER BY 1",
            "SELECT (SELECT max(z.v) FROM t3 z)[1] COLLATE \"C\" AS c FROM t4 w WINDOW q AS (ORDER BY abs(w.v))",
        ] {
            let parsed = pg_query::parse(sql).unwrap();
            let ours = names(sql);
            let relations: Vec<String> = ours.relations.iter().map(Relation::written).collect();
            let functions: Vec<String> = ours.functions.iter().map(|f| f.join(".")).collect();
            for table in parsed.tables() {
                assert!(relations.contains(&table) || table == "a", "{sql}: {table}");
            }
            for function in parsed.functions() {
                assert!(functions.contains(&function), "{sql}: {function}");
            }
        }
        let n = names(
            "SELECT (SELECT max(z.v) FROM t3 z)[1] COLLATE \"C\" AS c FROM t4 w WINDOW q AS (ORDER BY abs(w.v))",
        );
        let mut relations: Vec<String> = n.relations.iter().map(Relation::written).collect();
        relations.sort();
        assert_eq!(relations, vec!["t3", "t4"]);
        let mut functions: Vec<String> = n.functions.iter().map(|f| f.join(".")).collect();
        functions.sort();
        assert_eq!(functions, vec!["abs", "max"]);
    }

    #[test]
    fn collects_every_name_read_after_a_qualifier() {
        let n = names(
            "SELECT f.row_to_json, (f).to_jsonb, fdc.food.pg_typeof, description, f.codes[1], (f.r).x.y \
             FROM fdc.food f ORDER BY f.fdc_id",
        );
        let mut fields = n.fields.clone();
        fields.sort();
        assert_eq!(
            fields,
            vec!["codes", "fdc_id", "pg_typeof", "r", "row_to_json"]
        );
        assert_eq!(n.selections, vec!["to_jsonb", "x", "y"]);
    }

    #[test]
    fn keeps_qualified_names_and_their_case() {
        let n = names(
            "SELECT \"Upper\"(x.v) FROM research.fdc.\"Food\" x, pg_temp.t y ORDER BY pg_catalog.lower(y.v)",
        );
        let mut relations: Vec<String> = n.relations.iter().map(Relation::written).collect();
        relations.sort();
        assert_eq!(relations, vec!["pg_temp.t", "research.fdc.Food"]);
        let functions: Vec<String> = n.functions.iter().map(|f| f.join(".")).collect();
        assert!(
            functions.contains(&"Upper".to_string())
                && functions.contains(&"pg_catalog.lower".to_string())
        );
    }
}
