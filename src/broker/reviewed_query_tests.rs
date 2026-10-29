use super::*;

fn tool(parameters: Value, template: &str) -> Tool {
    serde_json::from_value(json!({"key":"test-tool","version":1,"source":"test-source",
        "title":"Synthetic tool","purpose":"Select synthetic records.","parameters":parameters,
        "output_columns":[{"name":"id","type":"text","description":"Synthetic identifier"}],
        "row_cap":10,"state":"enabled","template":template,"template_sha256":digest(template.as_bytes())})).unwrap()
}
fn admit(tool: &Tool, values: Value) -> Result<(sql_guard::Admitted, Record), Fault> {
    tool.lower(
        &values,
        &Allowed {
            schema: "public",
            relations: &BTreeSet::from(["records".into()]),
            functions: &BTreeSet::from(["count".into()]),
            row_cap: 20,
        },
    )
}
fn text_param() -> Value {
    json!([{"name":"search","description":"Synthetic filter","type":"text","max_length":256,"required":true}])
}
#[test]
fn values_remain_one_literal_and_nfc_is_kept_separately() {
    let t = tool(
        text_param(),
        "SELECT id FROM public.records WHERE id=$1 ORDER BY id LIMIT 10",
    );
    for value in [
        "quote'",
        "back\\slash",
        "$$",
        "'; DROP TABLE records; --",
        "/* comment */",
        "e\u{301}",
        "語",
    ] {
        let (admitted, record) = admit(&t, json!({"search":value})).unwrap();
        assert_eq!(record.requested["search"], value);
        let tree = serde_json::to_value(pg_query::parse(&admitted.sql).unwrap().protobuf).unwrap();
        let literal=tree.pointer("/stmts/0/stmt/node/SelectStmt/where_clause/node/AExpr/rexpr/node/TypeCast/arg/node/AConst/val/Sval/sval");
        assert_eq!(literal, Some(&record.normalized["search"]));
        if value == "e\u{301}" {
            assert_eq!(record.normalized["search"], "é");
        }
        record
            .check(
                &admitted.sql,
                &Allowed {
                    schema: "public",
                    relations: &BTreeSet::from(["records".into()]),
                    functions: &BTreeSet::new(),
                    row_cap: 20,
                },
            )
            .unwrap();
    }
}
#[test]
fn placeholders_and_guard_are_checked_on_registration_and_every_use() {
    let allowed = Allowed {
        schema: "public",
        relations: &BTreeSet::from(["records".into()]),
        functions: &BTreeSet::new(),
        row_cap: 20,
    };
    for (sql, code) in [
        (
            "SELECT id FROM public.other WHERE id=$1 ORDER BY id LIMIT 10",
            "sql_refused_relation",
        ),
        (
            "SELECT id FROM public.records WHERE id=$2 ORDER BY id LIMIT 10",
            "reviewed_query_placeholders_invalid",
        ),
        (
            "SELECT id FROM public.records ORDER BY id LIMIT 10",
            "reviewed_query_placeholders_invalid",
        ),
        (
            "DELETE FROM public.records WHERE id=$1",
            "reviewed_query_template_not_select",
        ),
        (
            "SELECT id FROM public.records; SELECT id FROM public.records",
            "reviewed_query_template_statements",
        ),
        ("not sql", "reviewed_query_template_parse"),
    ] {
        let t = tool(text_param(), sql);
        assert_eq!(t.registration_check(&allowed).unwrap_err().code, code);
        assert_eq!(admit(&t, json!({"search":"x"})).unwrap_err().code, code);
    }
}

#[test]
fn parameter_types_defaults_and_boundaries() {
    let cases = [
        (
            json!({"type":"text","max_length":2}),
            json!("é"),
            json!("abc"),
            "reviewed_query_parameter_bounds",
        ),
        (
            json!({"type":"integer","minimum":-2,"maximum":2}),
            json!(-2),
            json!(3),
            "reviewed_query_parameter_bounds",
        ),
        (
            json!({"type":"number","minimum":-2.5,"maximum":2.5}),
            json!(2.5),
            json!(3.0),
            "reviewed_query_parameter_bounds",
        ),
        (
            json!({"type":"enum","choices":["first","second"]}),
            json!("second"),
            json!("third"),
            "reviewed_query_parameter_choice",
        ),
    ];
    for (kind, good, bad, code) in cases {
        let mut p = json!({"name":"value","description":"Synthetic parameter","required":false,"default":good});
        p.as_object_mut()
            .unwrap()
            .extend(kind.as_object().unwrap().clone());
        let t = tool(
            json!([p]),
            "SELECT id FROM public.records WHERE id::text=$1::text ORDER BY id LIMIT 10",
        );
        let (_, r) = admit(&t, json!({})).unwrap();
        assert_eq!(r.normalized["value"], good);
        assert!(admit(&t, json!({"value":good})).is_ok());
        let bad = admit(&t, json!({"value":bad})).unwrap_err();
        assert_eq!(
            bad,
            Fault {
                code,
                parameter: Some("value".into())
            }
        );
        assert_eq!(
            admit(&t, json!({"value":null})).unwrap_err().code,
            "reviewed_query_parameter_type"
        );
        assert_eq!(
            admit(&t, json!({"value":true})).unwrap_err().code,
            "reviewed_query_parameter_type"
        );
    }
    let t = tool(
        text_param(),
        "SELECT id FROM public.records WHERE id=$1 ORDER BY id LIMIT 10",
    );
    for value in ["\n", "\0", "\t", "\u{85}"] {
        assert_eq!(
            admit(&t, json!({"search":value})).unwrap_err().code,
            "reviewed_query_parameter_control"
        );
    }
    assert_eq!(
        admit(&t, json!({})).unwrap_err().parameter.as_deref(),
        Some("search")
    );
    let f = admit(&t, json!({"secret supplied key":"secret supplied value"})).unwrap_err();
    assert_eq!(
        f,
        Fault {
            code: "reviewed_query_parameter_unknown",
            parameter: None
        }
    );
    assert_eq!(f.to_string(), "reviewed_query_parameter_unknown");
    assert_eq!(
        admit(&t, json!([])).unwrap_err().code,
        "reviewed_query_parameters_invalid"
    );
    assert_eq!(
        admit(&t, json!({"search":"x".repeat(8193)}))
            .unwrap_err()
            .code,
        "reviewed_query_parameters_too_large"
    );
}

#[test]
fn schema_faults_and_provenance_cannot_change() {
    let template = "SELECT id FROM public.records WHERE id=$1 ORDER BY id LIMIT 10";
    for patch in [
        json!({"max_length":257}),
        json!({"max_length":0}),
        json!({"required":false}),
        json!({"default":"x"}),
        json!({"description":""}),
        json!({"name":"unsafe name"}),
    ] {
        let mut params = text_param();
        params[0]
            .as_object_mut()
            .unwrap()
            .extend(patch.as_object().unwrap().clone());
        assert_eq!(
            admit(&tool(params, template), json!({"search":"x"}))
                .unwrap_err()
                .code,
            "reviewed_query_definition_invalid"
        );
    }
    for kind in [
        json!({"type":"enum","choices":[]}),
        json!({"type":"enum","choices":["same","same"]}),
        json!({"type":"enum","choices":["line\n"]}),
        json!({"type":"integer","minimum":2,"maximum":1}),
        json!({"type":"number","minimum":2.0,"maximum":1.0}),
    ] {
        let mut p = json!({"name":"value","description":"Synthetic parameter","required":true});
        p.as_object_mut()
            .unwrap()
            .extend(kind.as_object().unwrap().clone());
        assert_eq!(
            admit(&tool(json!([p]), template), json!({}))
                .unwrap_err()
                .code,
            "reviewed_query_definition_invalid"
        );
    }
    let mut t = tool(text_param(), template);
    let (a, r) = admit(&t, json!({"search":"x"})).unwrap();
    let allowed = Allowed {
        schema: "public",
        relations: &BTreeSet::from(["records".into()]),
        functions: &BTreeSet::new(),
        row_cap: 20,
    };
    for changed in [
        "template",
        "template_sha256",
        "schema_sha256",
        "requested",
        "normalized",
        "row_cap",
    ] {
        let mut v = serde_json::to_value(&r).unwrap();
        v[changed] = match changed {
            "requested" | "normalized" => json!({"search":"different"}),
            "row_cap" => json!(21),
            _ => json!("changed"),
        };
        let r: Record = serde_json::from_value(v).unwrap();
        assert!(r.check(&a.sql, &allowed).is_err());
    }
    t.state = State::Retired;
    assert_eq!(
        admit(&t, json!({"search":"x"})).unwrap_err().code,
        "reviewed_query_retired"
    );
    t.state = State::Enabled;
    t.row_cap = 21;
    assert_eq!(
        admit(&t, json!({"search":"x"})).unwrap_err().code,
        "reviewed_query_definition_invalid"
    );
}

#[test]
fn every_fdc_foundation_request_admits_with_its_release_declaration() {
    // Each Foundation release's declared relations, functions and cap as the live sources hold them
    // (tests/fixtures/reviewed_queries/fdc_foundation_releases.json); the April 2026 tools and the copies that
    // scripts/generate_fdc_release_tools.py writes for the other releases.
    let manifest = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    let fixture: Value = serde_json::from_slice(
        &std::fs::read(
            manifest.join("tests/fixtures/reviewed_queries/fdc_foundation_releases.json"),
        )
        .unwrap(),
    )
    .unwrap();
    let names = |value: &Value| -> BTreeSet<String> {
        value
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_str().unwrap().to_owned())
            .collect()
    };
    let mut keys = BTreeSet::new();
    let mut sources = BTreeSet::new();
    for entry in
        std::fs::read_dir(manifest.join("profiles/reviewed_queries/fdc-foundation")).unwrap()
    {
        let dir = entry.unwrap().path();
        let mut t: Tool =
            serde_json::from_slice(&std::fs::read(dir.join("tool.json")).unwrap()).unwrap();
        t.template = std::fs::read_to_string(dir.join("template.sql")).unwrap();
        t.template_sha256 = digest(t.template.as_bytes());
        let release = &fixture["releases"][t.source.trim_start_matches("fdc-foundation-")];
        assert_eq!(release["source"], json!(t.source), "{}", t.key);
        let (relations, functions) = (names(&release["relations"]), names(&release["functions"]));
        let allowed = Allowed {
            schema: release["schema"].as_str().unwrap(),
            relations: &relations,
            functions: &functions,
            row_cap: release["row_cap"].as_u64().unwrap(),
        };
        t.registration_check(&allowed)
            .unwrap_or_else(|f| panic!("{}: {}", t.key, f.code));
        assert!(!t.view().to_string().contains("SELECT"));
        assert!(keys.insert(t.key.clone()), "{} twice", t.key);
        sources.insert(t.source.clone());
    }
    assert_eq!(keys.len(), 45);
    assert_eq!(sources.len(), 15);
}

#[test]
fn every_fdc_data_type_request_admits_with_its_source_declaration() {
    // The SR Legacy, FNDDS and Branded Foods sources labs are granted, as the live sources declare them
    // (tests/fixtures/reviewed_queries/fdc_data_type_sources.json), and the tools
    // scripts/generate_fdc_data_type_tools.py writes for them.
    let manifest = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    let fixture: Value = serde_json::from_slice(
        &std::fs::read(manifest.join("tests/fixtures/reviewed_queries/fdc_data_type_sources.json"))
            .unwrap(),
    )
    .unwrap();
    let names = |value: &Value| -> BTreeSet<String> {
        value
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_str().unwrap().to_owned())
            .collect()
    };
    let mut keys = BTreeSet::new();
    let mut sources = BTreeSet::new();
    for folder in ["fdc-sr-legacy", "fdc-fndds", "fdc-branded"] {
        for entry in
            std::fs::read_dir(manifest.join("profiles/reviewed_queries").join(folder)).unwrap()
        {
            let dir = entry.unwrap().path();
            let mut t: Tool =
                serde_json::from_slice(&std::fs::read(dir.join("tool.json")).unwrap()).unwrap();
            t.template = std::fs::read_to_string(dir.join("template.sql")).unwrap();
            t.template_sha256 = digest(t.template.as_bytes());
            let source = &fixture["sources"][&t.source];
            let (relations, functions) = (names(&source["relations"]), names(&source["functions"]));
            let allowed = Allowed {
                schema: source["schema"].as_str().unwrap(),
                relations: &relations,
                functions: &functions,
                row_cap: source["row_cap"].as_u64().unwrap(),
            };
            t.registration_check(&allowed)
                .unwrap_or_else(|f| panic!("{}: {}", t.key, f.code));
            assert!(!t.view().to_string().contains("SELECT"));
            assert!(keys.insert(t.key.clone()), "{} twice", t.key);
            sources.insert(t.source.clone());
        }
    }
    assert_eq!(keys.len(), 21);
    assert_eq!(sources.len(), 7);
}

#[test]
fn nested_and_repeated_parameters_are_all_substituted() {
    let template = "WITH subset AS (SELECT id FROM public.records WHERE id=$1) SELECT CASE WHEN id=$1 THEN id ELSE $1 END AS id FROM subset WHERE EXISTS (SELECT id FROM public.records WHERE id=$1) ORDER BY id LIMIT 10";
    assert!(admit(&tool(text_param(), template), json!({"search":"x"})).is_ok());
    let two = json!([{"name":"one","description":"First","type":"text","required":true,"max_length":10},
        {"name":"two","description":"Second","type":"text","required":true,"max_length":10}]);
    assert_eq!(
        admit(&tool(two, template), json!({"one":"x","two":"y"}))
            .unwrap_err()
            .code,
        "reviewed_query_placeholders_invalid"
    );
    let t = tool(json!([]), "SELECT id FROM public.records ORDER BY id");
    assert_eq!(admit(&t, json!({})).unwrap().0.limit, 10);
}

#[test]
fn reviewed_proposals_use_only_catalog_tools_and_carry_the_version_and_named_fault() {
    use crate::broker::coordinator::{CodeRefused, Proposal, query_sources, with_query_sources};
    let t = tool(
        text_param(),
        "SELECT id FROM public.records WHERE id=$1 ORDER BY id LIMIT 10",
    );
    let source = json!({"profile":"synthetic-method","source":"test-source","title":"Synthetic source","description":"Synthetic records",
        "release":"Synthetic release","schema":"public","relations":[{"name":"records","columns":[]}],"functions":[],"row_cap":20,"reviewed_tools":[t]});
    let sources = query_sources(&json!([source])).unwrap();
    let methods = with_query_sources(&json!([{"key":"synthetic-method"}]), &sources);
    assert_eq!(methods[0]["query"]["reviewed_queries"][0]["version"], 1);
    assert!(!methods.to_string().contains("SELECT"));
    let action = json!({"action_key":"step-one","title":"Read synthetic records","profile":"synthetic-method",
        "objective":"Read records.","rationale":"Answer the question.","reviewed_query":{"tool":"test-tool","parameters":{"search":"synthetic"}}});
    let proposal = |a: Value| -> Proposal {
        serde_json::from_value(json!({"action":"delegate","summary":"Read records.","actions":[a]}))
            .unwrap()
    };
    let queries = proposal(action.clone()).admit_queries(&sources).unwrap();
    assert_eq!(
        queries["step-one"].reviewed.as_ref().unwrap().tool,
        "test-tool"
    );
    assert_eq!(queries["step-one"].admitted.limit, 10);
    for field in ["sql", "code", "settings"] {
        let mut p = proposal(action.clone());
        match field {
            "sql" => p.actions[0].sql = Some("SELECT id FROM records".into()),
            "code" => p.actions[0].code = Some("x".into()),
            "settings" => p.actions[0].settings = Some(Default::default()),
            _ => continue,
        }
        assert_eq!(
            p.admit_queries(&sources).unwrap_err().to_string(),
            "proposal_sql_unexpected"
        );
    }
    let mut bad = action.clone();
    bad["reviewed_query"]["parameters"]["search"] = json!(3);
    let fault = proposal(bad).admit_queries(&sources).unwrap_err();
    assert_eq!(
        fault.downcast_ref::<CodeRefused>().unwrap().names,
        ["step step-one: search: reviewed_query_parameter_type"]
    );
    let mut unknown = action;
    unknown["reviewed_query"]["tool"] = json!("other-tool");
    assert_eq!(
        proposal(unknown)
            .admit_queries(&sources)
            .unwrap_err()
            .to_string(),
        "proposal_reviewed_query_unknown"
    );
}

#[test]
fn migration_sql_and_owner_helpers_parse_offline() {
    let migration = include_str!("../../migrations/postgres/071_reviewed_queries.sql");
    pg_query::parse(migration).expect("migration SQL parses");
    for name in [
        "broker_reviewed_query_immutable",
        "broker_reviewed_parameter_valid",
        "broker_register_reviewed_query",
        "broker_reviewed_query_custody",
    ] {
        let start = migration
            .find(&format!(
                "CREATE OR REPLACE FUNCTION enrichedlab_agent.{name}("
            ))
            .unwrap();
        let function = &migration[start..];
        let end = function.find("\n$$;").unwrap() + 4;
        pg_query::parse_plpgsql(&function[..end]).unwrap_or_else(|error| panic!("{name}: {error}"));
    }
}

#[test]
fn schema_limits_and_nonfinite_bounds_are_refused() {
    let template = "SELECT id FROM public.records WHERE id=$1 ORDER BY id LIMIT 10";
    for (field, value) in [
        ("title", json!("x".repeat(121))),
        ("purpose", json!("x".repeat(1001))),
        ("key", json!("bad_key")),
        ("version", json!(0)),
        ("row_cap", json!(0)),
        ("template_sha256", json!("0".repeat(64))),
        ("output_columns", json!([])),
    ] {
        let mut raw = serde_json::to_value(tool(text_param(), template)).unwrap();
        raw[field] = value;
        let t: Tool = serde_json::from_value(raw).unwrap();
        assert_eq!(
            admit(&t, json!({"search":"x"})).unwrap_err().code,
            "reviewed_query_definition_invalid"
        );
    }
    let mut t = tool(text_param(), template);
    t.parameters = vec![t.parameters[0].clone(); 17];
    assert_eq!(
        admit(&t, json!({})).unwrap_err().code,
        "reviewed_query_definition_invalid"
    );
    t.parameters.truncate(2);
    assert_eq!(
        admit(&t, json!({})).unwrap_err().code,
        "reviewed_query_definition_invalid"
    );
    t.parameters.truncate(1);
    for kind in [
        ParameterType::Number {
            minimum: f64::NEG_INFINITY,
            maximum: 1.0,
        },
        ParameterType::Number {
            minimum: 0.0,
            maximum: f64::NAN,
        },
        ParameterType::Enum {
            choices: (0..65).map(|n| format!("choice{n}")).collect(),
        },
    ] {
        t.parameters[0].kind = kind;
        assert_eq!(
            admit(&t, json!({})).unwrap_err().code,
            "reviewed_query_definition_invalid"
        );
    }
    let p = json!({"name":"value","description":"Synthetic integer","type":"integer","minimum":i64::MIN,"maximum":i64::MAX,"required":true});
    let t = tool(json!([p]), template);
    for v in [json!(i64::MIN), json!(i64::MAX)] {
        assert!(admit(&t, json!({"value":v})).is_ok());
    }
    for v in [json!(1.5), json!("1"), json!(u64::MAX)] {
        assert_eq!(
            admit(&t, json!({"value":v})).unwrap_err().code,
            "reviewed_query_parameter_type"
        );
    }
    let mut params = text_param();
    params[0]["unexpected"] = json!("x");
    assert!(serde_json::from_value::<Vec<Parameter>>(params).is_err());
}

#[test]
fn catalog_offers_latest_enabled_tool_and_instructions_only_where_present() {
    use crate::broker::coordinator::{query_sources, with_query_sources};
    let t = tool(
        text_param(),
        "SELECT id FROM public.records WHERE id=$1 ORDER BY id LIMIT 10",
    );
    let mut newer = t.clone();
    newer.version = 2;
    let mut retired = newer.clone();
    retired.version = 3;
    retired.state = State::Retired;
    let mut source = json!({"profile":"synthetic-method","source":"test-source","title":"Synthetic source","description":"Synthetic records",
        "release":"Synthetic release","schema":"public","relations":[{"name":"records","columns":[]}],"functions":[],"row_cap":20,"reviewed_tools":[t,newer,retired]});
    let methods = json!([{"key":"synthetic-method"}]);
    let view = with_query_sources(&methods, &query_sources(&json!([source])).unwrap());
    assert_eq!(
        view[0]["query"]["reviewed_queries"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    assert_eq!(view[0]["query"]["reviewed_queries"][0]["version"], 2);
    assert_eq!(view[0]["reviewed_query_note"], OFFER);
    source["reviewed_tools"] = json!([]);
    let view = with_query_sources(&methods, &query_sources(&json!([source])).unwrap());
    assert!(view[0].get("reviewed_query_note").is_none());
    assert!(view[0]["query"].get("reviewed_queries").is_none());
}

#[test]
fn render_checks_a_declaration_file_as_register_checks_the_row() {
    let folder = std::env::temp_dir().join(format!(
        "reviewed_query_render_codetestfile_{}",
        std::process::id()
    ));
    std::fs::create_dir_all(&folder).unwrap();
    let template = "SELECT id FROM public.records WHERE id=$1 ORDER BY id LIMIT 10";
    let mut definition = serde_json::to_value(tool(text_param(), template)).unwrap();
    definition.as_object_mut().unwrap().remove("template");
    definition
        .as_object_mut()
        .unwrap()
        .remove("template_sha256");
    std::fs::write(folder.join("tool.json"), definition.to_string()).unwrap();
    std::fs::write(folder.join("template.sql"), template).unwrap();
    let declare = |relations: Value| {
        std::fs::write(
            folder.join("source.json"),
            json!({"schema_name":"public","relations":relations,"functions":["count"],"row_cap":20}).to_string(),
        )
        .unwrap();
    };
    let args = registration::Render {
        file: folder.join("tool.json"),
        template: folder.join("template.sql"),
        source_file: folder.join("source.json"),
    };
    declare(json!([{"name":"records"}]));
    let body = registration::render(&args).unwrap();
    assert_eq!(body["template"], template);
    assert_eq!(body["template_sha256"], digest(template.as_bytes()));
    declare(json!([{"name":"other"}]));
    assert_eq!(
        registration::render(&args).unwrap_err().to_string(),
        "sql_refused_relation"
    );
    std::fs::remove_dir_all(folder).unwrap();
}
