//! A step's title and rationale may state the numbers its own SQL sets in a comparison or an IN list; the
//! same numbers stay refused anywhere else, and a literal outside a comparison or a list sets nothing.
use super::*;
use serde_json::json;

fn action(key: &str, sql: Option<&str>, rationale: &str) -> Value {
    let mut a = json!({"action_key": key, "title": "Read foods", "profile": "reference-sql-fdc", "objective": "o",
        "rationale": rationale});
    if let Some(sql) = sql {
        a["sql"] = json!(sql);
    }
    a
}

fn proposal(actions: Vec<Value>, summary: &str) -> Proposal {
    serde_json::from_value(json!({"action": "delegate", "summary": summary, "actions": actions}))
        .unwrap()
}

fn own(sql: &str) -> Vec<String> {
    let a: ProposedAction = serde_json::from_value(action("q-1", Some(sql), "r")).unwrap();
    let mut numbers: Vec<String> = step_own_numbers(&a).into_iter().collect();
    numbers.sort();
    numbers
}

fn refused(p: &Proposal) -> Vec<String> {
    match p.validate_prose(&ResultIndex::default()) {
        Ok(()) => Vec::new(),
        // The refused tokens; where each stood (`where` names) is the places test's.
        Err(e) => e
            .downcast_ref::<CodeRefused>()
            .unwrap()
            .names
            .iter()
            .filter(|n| !n.starts_with("where "))
            .cloned()
            .collect(),
    }
}

#[test]
fn comparison_and_list_literals_are_a_steps_own_numbers() {
    assert_eq!(
        own(
            "SELECT f.name FROM food f JOIN food_nutrient n ON n.fdc_id = f.fdc_id \
             WHERE n.nutrient_id IN (1162, 1087) AND n.amount > 10.5 AND 3 <= n.rank AND n.flag <> 0"
        ),
        ["0", "10.5", "1087", "1162", "3"]
    );
    assert_eq!(
        own("SELECT 1 FROM t WHERE x NOT IN (7, 8) AND y = -4 AND z != 9"),
        ["-4", "7", "8", "9"]
    );
    // A limit, an offset, a projected value, a function's argument, a cast, arithmetic and a string set nothing.
    assert!(
        own("SELECT 2 AS k, round(x, 3) FROM t WHERE x > y * 5 AND z = '6' AND w = 7::int AND v = abs(8) \
             ORDER BY 1 LIMIT 11 OFFSET 12")
            .is_empty()
    );
    assert!(own("not sql at all 13").is_empty());
}

#[test]
fn a_step_may_state_what_its_own_sql_sets_and_nothing_more() {
    let sql = "SELECT name FROM food WHERE nutrient_id IN (1162, 1087) AND amount > 10 ORDER BY 1 LIMIT 20";
    let stated = "Keep nutrients 1162 and 1087 with more than 10 per serving.";
    assert!(refused(&proposal(vec![action("q-1", Some(sql), stated)], "s")).is_empty());
    // A number the SQL writes only as a limit, or not at all, is still refused.
    assert_eq!(
        refused(&proposal(
            vec![action("q-1", Some(sql), "Show the top 20, about 35 rows.")],
            "s"
        )),
        ["actions.rationale: 20", "actions.rationale: 35"]
    );
    // A step's numbers stand only in its own title and rationale: not in another step's, the summary or a part.
    let other = action(
        "q-2",
        Some("SELECT name FROM food WHERE amount > 2"),
        "Compare with nutrient 1162.",
    );
    assert_eq!(
        refused(&proposal(
            vec![action("q-1", Some(sql), stated), other],
            "Nutrient 1162 leads."
        )),
        ["summary: 1162", "actions.rationale: 1162"]
    );
    // A step without SQL sets nothing.
    assert_eq!(
        refused(&proposal(vec![action("a-1", None, "Use 1162.")], "s")),
        ["actions.rationale: 1162"]
    );
    let mut titled = action("q-1", Some(sql), "r");
    titled["title"] = json!("Foods above 10");
    assert!(refused(&proposal(vec![titled], "s")).is_empty());
}

#[test]
fn a_steps_literals_stand_only_alone_never_inside_a_name_or_a_compact_form() {
    // Separate literals must not compose a number or a name the SQL never wrote.
    let sql = "SELECT name FROM t WHERE a > 1 AND b < 9 AND c IN (7, 3, 10)";
    let p = |rationale: &str| refused(&proposal(vec![action("q-1", Some(sql), rationale)], "s"));
    assert!(p("Keep a above 1, b below 9 and codes 7, 3 and 10.").is_empty());
    assert_eq!(
        p("Keep p below 1e-9."),
        ["actions.rationale: 1", "actions.rationale: 9"]
    );
    assert_eq!(
        p("Compare A7/B3."),
        ["actions.rationale: 7", "actions.rationale: 3"]
    );
    assert_eq!(
        p("Use the x10 batch and 7th code."),
        ["actions.rationale: 10", "actions.rationale: 7"]
    );
}

#[test]
fn a_steps_literal_stands_only_as_a_whole_token() {
    // An invisible mark, a caret, an underscore or a slash glued to a literal keeps it
    // inside a token the SQL never wrote.
    let sql = "SELECT name FROM t WHERE a > 1 AND b < 9 AND c IN (7, 3, 10)";
    let p = |rationale: &str| refused(&proposal(vec![action("q-1", Some(sql), rationale)], "s"));
    assert!(p("Keep a above 1 (and b below 9), codes 7 and 3: 10.").is_empty());
    for (text, want) in [
        (
            "Keep p below 1\u{200e}e\u{200e}-9.",
            vec!["1", "9", "U+200E"],
        ),
        ("Keep p below 10^-9.", vec!["10", "9"]),
        ("Compare A_7/B_3.", vec!["7", "3"]),
        ("Compare A/7 rows.", vec!["7"]),
        ("Use 7\u{200b}3 codes.", vec!["7", "3"]),
    ] {
        let want: Vec<String> = want
            .iter()
            .map(|n| format!("actions.rationale: {n}"))
            .collect();
        assert_eq!(p(text), want, "{text:?}");
    }
}

#[test]
fn a_steps_literal_matches_only_as_the_sql_writes_it() {
    // A leading point, a sign the SQL does not write or a wrapper makes another value.
    let sql = "SELECT name FROM t WHERE a = 7 AND b > -4 AND c < 10.5";
    let p = |rationale: &str| refused(&proposal(vec![action("q-1", Some(sql), rationale)], "s"));
    assert!(p("Keep a at 7, b above -4 and c below 10.5.").is_empty());
    assert!(p("Keep a at 7: b above -4.").is_empty());
    for (text, want) in [
        ("Keep .7 of a.", vec!["7"]),
        ("Keep -.7 of a.", vec!["7"]),
        ("Keep _7_ and /7/ rows.", vec!["7"]),
        ("Keep -7 and 4 rows.", vec!["7", "4"]),
        ("Keep 10.50 rows.", vec!["10.50"]),
    ] {
        let want: Vec<String> = want
            .iter()
            .map(|n| format!("actions.rationale: {n}"))
            .collect();
        assert_eq!(p(text), want, "{text:?}");
    }
}

#[test]
fn a_number_refusal_says_where_each_number_stood() {
    let p = proposal(
        vec![action("a", None, "Show the top 20, about 35 rows.")],
        "Done.",
    );
    let names = match p.validate_prose(&ResultIndex::default()) {
        Ok(()) => Vec::new(),
        Err(e) => e.downcast_ref::<CodeRefused>().unwrap().names.clone(),
    };
    assert_eq!(
        names,
        [
            "actions.rationale: 20",
            "actions.rationale: 35",
            "where actions.rationale: Show the top <<20>>, about 35 rows.",
            "where actions.rationale: Show the top 20, about <<35>> rows.",
        ]
    );
}
