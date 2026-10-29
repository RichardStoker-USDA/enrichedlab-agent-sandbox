//! A relation or function inside any admitted node must be authorized. pg_query's own collectors
//! (`ParseResult::tables`, `functions`) descend only some node kinds, so these positions were once invisible to the
//! guard; each case puts an undeclared relation (`branded_food`) or function (`upper`) in one of them.
use std::collections::BTreeSet;

use super::*;

fn check(sql: &str) -> Result<Admitted, Refused> {
    let relations: BTreeSet<String> = ["food", "food_nutrient", "nutrient", "food_category"]
        .iter()
        .map(|s| s.to_string())
        .collect();
    let functions: BTreeSet<String> = [
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
    admit(
        sql,
        &Allowed {
            schema: "fdc",
            relations: &relations,
            functions: &functions,
            row_cap: 5000,
        },
    )
}

const SUB: &str = "(SELECT b.description FROM branded_food b LIMIT 1)";
const CALL: &str = "upper(f.description)";

/// Each position, written once with a placeholder `X` for the nested expression.
const POSITIONS: &[(&str, &str)] = &[
    (
        "IS JSON",
        "SELECT f.description FROM food f WHERE X IS JSON ORDER BY 1",
    ),
    (
        "COLLATE",
        "SELECT f.description FROM food f WHERE X COLLATE \"C\" = f.description ORDER BY 1",
    ),
    ("indirection", "SELECT (X)[1] AS c FROM food f ORDER BY 1"),
    ("subscript", "SELECT f.codes[X] AS c FROM food f ORDER BY 1"),
    ("slice", "SELECT f.codes[1:X] AS c FROM food f ORDER BY 1"),
    (
        "ROLLUP",
        "SELECT count(*) AS n FROM food f GROUP BY ROLLUP ((X)) ORDER BY 1",
    ),
    (
        "GROUPING SETS",
        "SELECT count(*) AS n FROM food f GROUP BY GROUPING SETS ((X), ()) ORDER BY 1",
    ),
    (
        "PARTITION BY",
        "SELECT rank() OVER (PARTITION BY X ORDER BY f.fdc_id) AS r FROM food f ORDER BY 1",
    ),
    (
        "window ORDER BY",
        "SELECT rank() OVER (ORDER BY X) AS r FROM food f ORDER BY 1",
    ),
    (
        "named window",
        "SELECT rank() OVER w AS r FROM food f WINDOW w AS (ORDER BY X) ORDER BY 1",
    ),
    (
        "frame offset",
        "SELECT sum(f.fdc_id) OVER (ORDER BY f.fdc_id ROWS BETWEEN length(X) PRECEDING AND CURRENT ROW) AS s \
         FROM food f ORDER BY 1",
    ),
    (
        "DISTINCT ON",
        "SELECT DISTINCT ON (X) count(*) AS n FROM food f",
    ),
    (
        "OFFSET",
        "SELECT f.description FROM food f ORDER BY 1 LIMIT 10 OFFSET length(X)",
    ),
    (
        "FILTER",
        "SELECT count(*) FILTER (WHERE X = 'a') AS n FROM food f",
    ),
    (
        "WITHIN GROUP",
        "SELECT percentile_cont(0.5) WITHIN GROUP (ORDER BY X) AS m FROM food f",
    ),
    (
        "aggregate ORDER BY",
        "SELECT count(f.fdc_id ORDER BY X) AS n FROM food f",
    ),
    (
        "CASE operand",
        "SELECT CASE X WHEN 'a' THEN 1 END AS c FROM food f ORDER BY 1",
    ),
    (
        "CASE default",
        "SELECT CASE WHEN f.fdc_id > 1 THEN 'a' ELSE X END AS c FROM food f ORDER BY 1",
    ),
    (
        "IN test",
        "SELECT f.description FROM food f WHERE X IN (SELECT n.name FROM nutrient n) ORDER BY 1",
    ),
    (
        "join condition",
        "SELECT f.description FROM food f JOIN nutrient n ON n.name = X ORDER BY 1",
    ),
    ("cast", "SELECT (X)::text AS c FROM food f ORDER BY 1"),
    (
        "IS NULL",
        "SELECT f.description FROM food f WHERE X IS NULL ORDER BY 1",
    ),
    (
        "IS TRUE",
        "SELECT f.description FROM food f WHERE (X = 'a') IS TRUE ORDER BY 1",
    ),
    (
        "GREATEST",
        "SELECT GREATEST(X, 'a') AS c FROM food f ORDER BY 1",
    ),
    (
        "COALESCE",
        "SELECT COALESCE(X, 'a') AS c FROM food f ORDER BY 1",
    ),
    (
        "ROW",
        "SELECT f.description FROM food f WHERE ROW(X, 1) = ROW('a', 1) ORDER BY 1",
    ),
    (
        "HAVING",
        "SELECT count(*) AS n FROM food f GROUP BY f.fdc_id HAVING max(X) > 'a' ORDER BY 1",
    ),
    (
        "UNION arm",
        "SELECT f.description FROM food f UNION SELECT X AS d FROM food f ORDER BY 1",
    ),
];

/// The positions where `nested` is not refused with `code`, naming one of `names`; every position is tried so one
/// failure lists them all.
fn unrefused(nested: &str, code: &str, names: &[&str]) -> Vec<String> {
    POSITIONS
        .iter()
        .filter_map(|(label, sql)| {
            let sql = sql.replace('X', nested);
            match check(&sql) {
                Ok(_) => Some(format!("{label}: admitted")),
                Err(r)
                    if r.code != code
                        || r.names.len() != 1
                        || !names.contains(&r.names[0].as_str()) =>
                {
                    Some(format!("{label}: {} {:?}", r.code, r.names))
                }
                Err(_) => None,
            }
        })
        .collect()
}

#[test]
fn a_nested_relation_is_refused_in_every_admitted_position() {
    let failures = unrefused(SUB, "sql_refused_relation", &["branded_food"]);
    assert!(failures.is_empty(), "{failures:#?}");
}

#[test]
fn a_nested_function_is_refused_in_every_admitted_position() {
    // `length` in two positions is undeclared too; either name shows the nested call was seen.
    let failures = unrefused(CALL, "sql_refused_function", &["upper", "length"]);
    assert!(failures.is_empty(), "{failures:#?}");
}

#[test]
fn a_cte_name_hides_a_relation_only_where_postgresql_sees_the_cte() {
    // Without RECURSIVE a CTE does not see itself or a later sibling, a subquery's CTE is not visible beside it, and a
    // qualified name never names a CTE: each `branded_food` read here is the real relation.
    for (sql, name) in [
        (
            "WITH branded_food AS (SELECT f.description FROM food f) SELECT b.description FROM fdc.branded_food b \
         ORDER BY 1",
            "fdc.branded_food",
        ),
        (
            "WITH branded_food AS (SELECT * FROM branded_food) SELECT b.description FROM branded_food b \
         CROSS JOIN food f ORDER BY 1",
            "branded_food",
        ),
        (
            "WITH a AS (SELECT * FROM branded_food), branded_food AS (SELECT f.description FROM food f) \
         SELECT a.description FROM a ORDER BY 1",
            "branded_food",
        ),
        (
            "SELECT x.description FROM (WITH branded_food AS (SELECT f.description FROM food f) \
         SELECT * FROM branded_food) x CROSS JOIN (SELECT b.description FROM branded_food b) y ORDER BY 1",
            "branded_food",
        ),
        (
            "SELECT f.description FROM food f WHERE f.description IN (WITH branded_food AS (SELECT n.name FROM nutrient n) \
         SELECT * FROM branded_food) AND f.description IN (SELECT b.description FROM branded_food b) ORDER BY 1",
            "branded_food",
        ),
    ] {
        let refused = check(sql).err().unwrap_or_else(|| panic!("admitted {sql}"));
        assert_eq!(refused.code, "sql_refused_relation", "{sql}");
        assert_eq!(refused.names, vec![name.to_string()], "{sql}");
    }
    // Where PostgreSQL does resolve the name to the CTE, the query stays admitted.
    for sql in [
        "WITH a AS (SELECT f.fdc_id FROM food f), b AS (SELECT a.fdc_id FROM a) SELECT b.fdc_id FROM b ORDER BY 1",
        "WITH a AS (SELECT f.fdc_id FROM food f) SELECT f.description FROM food f WHERE f.fdc_id IN \
         (SELECT a.fdc_id FROM a) ORDER BY 1",
        "WITH RECURSIVE a AS (SELECT b.x FROM b), b AS (SELECT f.fdc_id AS x FROM food f) SELECT a.x FROM a ORDER BY 1",
        "WITH RECURSIVE r AS ((SELECT f.fdc_id AS n FROM food f ORDER BY 1 LIMIT 1) UNION ALL \
         SELECT r.n + 1 FROM r WHERE r.n < 3) SELECT r.n FROM r ORDER BY 1",
        "WITH a AS (SELECT f.fdc_id FROM food f) SELECT a.fdc_id FROM a UNION SELECT a.fdc_id FROM a ORDER BY 1",
        "SELECT x.fdc_id FROM (WITH a AS (SELECT f.fdc_id FROM food f) SELECT a.fdc_id FROM a) x ORDER BY 1",
    ] {
        let admitted =
            check(sql).unwrap_or_else(|r| panic!("refused {sql}: {} {:?}", r.code, r.names));
        assert_eq!(admitted.relations, vec!["food".to_string()], "{sql}");
    }
}

#[test]
fn names_are_matched_as_postgresql_matches_them() {
    // A quoted name keeps its case, so `"FOOD"` and `"LOWER"` are not the declared `food` and `lower`.
    let refused = check("SELECT f.description FROM \"FOOD\" f ORDER BY 1").unwrap_err();
    assert_eq!(
        (refused.code, refused.names),
        ("sql_refused_relation", vec!["FOOD".to_string()])
    );
    let refused = check("SELECT \"LOWER\"(f.description) AS d FROM food f ORDER BY 1").unwrap_err();
    assert_eq!(
        (refused.code, refused.names),
        ("sql_refused_function", vec!["LOWER".to_string()])
    );
    // Another schema, a database-qualified name and a schema-qualified function are refused; the source's own schema
    // and pg_catalog functions are not.
    for (sql, code) in [
        (
            "SELECT f.description FROM other.food f ORDER BY 1",
            "sql_refused_relation",
        ),
        (
            "SELECT f.description FROM research.fdc.food f ORDER BY 1",
            "sql_refused_relation",
        ),
        (
            "SELECT fdc.lower(f.description) AS d FROM food f ORDER BY 1",
            "sql_refused_function",
        ),
    ] {
        assert_eq!(check(sql).unwrap_err().code, code, "{sql}");
    }
    assert!(
        check("SELECT pg_catalog.lower(f.description) AS d FROM fdc.food f ORDER BY 1").is_ok()
    );
}

#[test]
fn names_after_a_qualifier_go_to_the_worker_unless_the_source_lists_them() {
    // `f.row_to_json` parses as a column, and PostgreSQL calls `row_to_json(f)` when food has no such column; only
    // the worker, inside the source's transaction, can tell, so the admitted query carries the names it must check.
    let a = check(
        "SELECT f.row_to_json, fdc.food.pg_typeof, f.description, max(f.fdc_id) AS m, f.max \
         FROM fdc.food f GROUP BY 1, 2, 3, 5 ORDER BY 1",
    )
    .unwrap();
    assert_eq!(
        a.field_names,
        vec!["description", "fdc_id", "pg_typeof", "row_to_json"]
    );
}

#[test]
fn a_selection_may_name_only_a_listed_function() {
    // `(expr).name` is the call `name(expr)` for any one-argument function and any expression.
    for (sql, name) in [
        (
            "SELECT (f.description).upper AS d FROM food f ORDER BY 1",
            "upper",
        ),
        (
            "SELECT (f).to_jsonb AS d FROM food f ORDER BY 1",
            "to_jsonb",
        ),
        (
            "SELECT f.description, (1::int).pg_cancel_backend AS d FROM food f ORDER BY 1",
            "pg_cancel_backend",
        ),
        (
            "SELECT f.fdc_id FROM food f WHERE (f.description).upper = 'A' ORDER BY 1",
            "upper",
        ),
    ] {
        let refused = check(sql).unwrap_err();
        assert_eq!(
            (refused.code, refused.names),
            ("sql_refused_function", vec![name.to_string()]),
            "{sql}"
        );
    }
    // A listed function may be called this way, and subscripts and `(expr).*` select no name.
    for sql in [
        "SELECT (f.description).lower AS d FROM food f ORDER BY 1",
        "SELECT (f.codes)[1] AS c FROM food f ORDER BY 1",
        "SELECT (f).* FROM food f ORDER BY 1",
    ] {
        assert!(check(sql).is_ok(), "{sql}");
    }
}
