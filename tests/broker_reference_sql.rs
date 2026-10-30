//! Guarded read-only queries (migration 038) against an isolated PostgreSQL: a registered source reaches
//! the run that may query it, the turn commit stores only an admitted query, the worker runs it read-only against the
//! snapshot the source names and retains a package with its digest and receipt, and a failed query finishes with a
//! code the lead can act on.
use enrichedlab_agent_sandbox::broker::{
    config::WorkerConfig,
    coordinator::StartRun,
    model::{PrincipalClass, VerifiedIdentity},
    project::{CheckpointRequest, CreateProject, ResearchState, Visibility},
    query_run,
    source_card::{self, CardSource, CardSpec},
    sql_guard::{self, Allowed},
    store::{Store, StoreErrorKind},
};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{collections::BTreeSet, env, fs, path::PathBuf, time::Duration};
use tokio_postgres::{Config, NoTls, config::Host};
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

#[path = "broker_reviewed_queries/mod.rs"]
mod reviewed_queries;

const MANIFEST: &str = "5f1c0d9a3a4e2b7c8d6e5f4a3b2c1d0e9f8a7b6c5d4e3f2a1b0c9d8e7f6a5b4c";

fn research() -> ResearchState {
    serde_json::from_value(json!({"schema_version":1,"goal":"Which example foods carry the most of the example nutrient?",
        "constraints":["Read only the reference data."],
        "plan":[
            {"action_key":"rank-foods","title":"Rank foods by the nutrient","status":"pending","task_id":null,"evidence_refs":[],"rationale":"Answer the question."},
            {"action_key":"bad-column","title":"A column that does not exist","status":"pending","task_id":null,"evidence_refs":[],"rationale":"A failing query."}
        ],
        "lessons":[],"next_action":"Query the reference data.",
        "workspace":{"schema_version":1,"profile":"reference-sql","image_digest":format!("sha256:{}","a".repeat(64)),
        "source_digest":format!("sha256:{}","b".repeat(64)),"input_refs":["reference://qualification/example"],
        "tool_refs":["tool://reference-sql"],"preparation_receipt":null}})).unwrap()
}

fn source(key: &str, profile: &str, manifest: &str) -> Value {
    json!({"source": key, "profile": profile, "title": "Example reference data",
        "description": "Invented foods and one invented nutrient's amount per 100 g.", "release": "Qualification release",
        "schema": "reference_qual",
        "relations": [{"name": "food", "row_count": 4, "columns": [{"name": "fdc_id", "type": "integer"}, {"name": "description", "type": "text"}]},
                      {"name": "food_nutrient", "row_count": 4, "columns": [{"name": "fdc_id", "type": "integer"}, {"name": "amount", "type": "double precision"}]}],
        "functions": ["count", "max", "round"], "row_cap": 100, "statement_timeout_ms": 5000,
        "connection_ref": "reference-qual", "snapshot": {"manifest_sha256": manifest}})
}

fn admitted_query(sql: &str) -> Value {
    let relations: BTreeSet<String> = ["food", "food_nutrient"]
        .iter()
        .map(|s| s.to_string())
        .collect();
    let functions: BTreeSet<String> = ["count", "max", "round"]
        .iter()
        .map(|s| s.to_string())
        .collect();
    let a = sql_guard::admit(
        sql,
        &Allowed {
            schema: "reference_qual",
            relations: &relations,
            functions: &functions,
            row_cap: 100,
        },
    )
    .unwrap();
    json!({"source": "example-reference", "proposed_sql": sql, "admitted_sql": a.sql,
        "sha256": format!("sha256:{}", hex::encode(Sha256::digest(a.sql.as_bytes()))), "guard": sql_guard::GUARD,
        "relations": a.relations, "row_limit": a.limit})
}

fn action(key: &str, query: Option<Value>) -> Value {
    let mut action = json!({"action_key": key, "profile": "reference-sql", "objective": "Rank the foods.", "request_sha256": "c".repeat(64)});
    if let Some(query) = query {
        action["query"] = query;
    }
    json!([action])
}

#[tokio::test]
#[ignore = "isolated PostgreSQL through qualify_remote_broker.sh reference-sql"]
async fn a_guarded_query_runs_read_only_and_keeps_its_package() {
    let dsn_file = env::var_os("BROKER_QUALIFICATION_MIGRATION_DSN_FILE").unwrap();
    let dsn = fs::read_to_string(&dsn_file).unwrap();
    let config: Config = dsn.parse().unwrap();
    assert_eq!(config.get_dbname(), Some("broker_qual"));
    assert!(matches!(config.get_hosts(),[Host::Tcp(h)] if h=="127.0.0.1"));
    let (owner, connection) = config.connect(NoTls).await.unwrap();
    let connection = tokio::spawn(connection);
    let store = |user: &str, size| {
        Store::connect(
            &dsn.replace("user=postgres", &format!("user={user}")),
            "research-site".into(),
            size,
        )
        .unwrap()
    };
    let api = store("broker_api_qual", 3);
    let coordinator = store("broker_coordinator_qual", 2);
    let worker = store("broker_worker_qual", 2);
    // Each role is ready only with migration 038 and its own grants; the API's login cannot serve queries.
    api.ready_for_review_api().await.unwrap();
    coordinator.ready_for_research_results().await.unwrap();
    worker.ready_for_queries().await.unwrap();
    assert_eq!(
        api.ready_for_queries().await.unwrap_err().code,
        "broker_query_schema_not_ready"
    );
    let sources_fn = "enrichedlab_agent.broker_research_run_sql_sources(text,uuid,text,bigint)";
    owner
        .batch_execute(&format!(
            "REVOKE EXECUTE ON FUNCTION {sources_fn} FROM broker_coordinator_qual"
        ))
        .await
        .unwrap();
    assert_eq!(
        coordinator
            .ready_for_research_results()
            .await
            .unwrap_err()
            .code,
        "broker_results_schema_not_ready"
    );
    owner
        .batch_execute(&format!(
            "GRANT EXECUTE ON FUNCTION {sources_fn} TO broker_coordinator_qual"
        ))
        .await
        .unwrap();
    coordinator.ready_for_research_results().await.unwrap();
    owner.batch_execute("INSERT INTO enrichedlab_agent.broker_principal_invitations(tenant_id,email,group_key,role) VALUES
        ('research-site','outsider@example.invalid','pilot-lab','researcher');").await.unwrap();
    let person = |subject: &str, email: &str| VerifiedIdentity {
        issuer: "https://identity.example.invalid".into(),
        class: PrincipalClass::Person,
        subject: subject.into(),
        email: email.into(),
        display_name: None,
    };
    let a = api
        .resolve_principal(&person("owner", "researcher@example.invalid"))
        .await
        .unwrap();
    let outsider = api
        .resolve_principal(&person("outsider", "outsider@example.invalid"))
        .await
        .unwrap();

    // The reference data: a schema with a snapshot record and a reader login that can only select from it.
    owner.batch_execute(&format!("
        CREATE SCHEMA reference_qual;
        CREATE TABLE reference_qual.food (fdc_id integer PRIMARY KEY, description text NOT NULL);
        CREATE TABLE reference_qual.food_nutrient (fdc_id integer NOT NULL, amount double precision);
        CREATE TABLE reference_qual.snapshot_manifest (manifest_sha256 text NOT NULL);
        INSERT INTO reference_qual.food VALUES (1,'Example food A'),(2,'Example food B, raw'),(3,'Example \"quoted\" food'),(4,'Example food D');
        INSERT INTO reference_qual.food_nutrient VALUES (1,48.2),(2,12.9),(3,NULL),(4,12.9);
        INSERT INTO reference_qual.snapshot_manifest VALUES ('{MANIFEST}');
        CREATE ROLE reference_reader_qual LOGIN;
        GRANT USAGE ON SCHEMA reference_qual TO reference_reader_qual;
        GRANT SELECT ON ALL TABLES IN SCHEMA reference_qual TO reference_reader_qual;
        ALTER ROLE reference_reader_qual SET default_transaction_read_only = on;
        INSERT INTO enrichedlab_agent.broker_agent_profiles(tenant_id,profile_key,name,description,harness,thinking,route,
          controller_profile,gateway_profile,image_ref,cpu_limit,memory_limit,state_profile,role,tool_bundle,max_run_seconds,
          max_task_attempts,max_provider_attempts)
        SELECT tenant_id,k,'Query the example reference data',description,harness,thinking,'server-query',controller_profile,
          gateway_profile,image_ref,cpu_limit,memory_limit,state_profile,role,tool_bundle,max_run_seconds,1,max_provider_attempts
        FROM enrichedlab_agent.broker_agent_profiles, unnest(ARRAY['reference-sql','reference-sql-stale']) k
        WHERE tenant_id='research-site' AND profile_key='general-research';
        INSERT INTO enrichedlab_agent.broker_group_profile_grants(tenant_id,group_key,profile_key)
          VALUES('research-site','pilot-lab','reference-sql'),('research-site','pilot-lab','reference-sql-stale');")).await.unwrap();

    // Registration: once per key, the same record again is accepted, a different one is refused, rows never change.
    let register = |value: Value| {
        let owner = &owner;
        async move {
            owner
                .query_one(
                    "SELECT enrichedlab_agent.broker_register_sql_source('research-site',$1)",
                    &[&value],
                )
                .await
        }
    };
    register(source("example-reference", "reference-sql", MANIFEST))
        .await
        .unwrap();
    register(source("example-reference", "reference-sql", MANIFEST))
        .await
        .unwrap();
    assert!(
        register(source(
            "example-reference",
            "reference-sql",
            &"0".repeat(64)
        ))
        .await
        .is_err()
    );
    register(source(
        "stale-reference",
        "reference-sql-stale",
        &"0".repeat(64),
    ))
    .await
    .unwrap();
    assert!(
        owner
            .execute(
                "UPDATE enrichedlab_agent.broker_sql_sources SET row_cap=5",
                &[]
            )
            .await
            .is_err()
    );
    assert!(
        owner
            .execute("DELETE FROM enrichedlab_agent.broker_sql_sources", &[])
            .await
            .is_err()
    );
    // A source is public reference data (migration 039); any other class is refused until a decision admits it.
    let classes: Vec<String> = owner
        .query(
            "SELECT data_class FROM enrichedlab_agent.broker_sql_sources ORDER BY source_key",
            &[],
        )
        .await
        .unwrap()
        .iter()
        .map(|r| r.get(0))
        .collect();
    assert_eq!(classes, ["public_reference", "public_reference"]);
    let lab = owner
        .execute(
            "INSERT INTO enrichedlab_agent.broker_sql_sources (tenant_id, source_key, profile_key, title, description,
               release, schema_name, relations, functions, row_cap, statement_timeout_ms, connection_ref, snapshot,
               data_class)
             SELECT tenant_id, 'lab-reference', 'general-research', title, description, release, schema_name, relations,
               functions, row_cap, statement_timeout_ms, connection_ref, snapshot, 'lab_database'
             FROM enrichedlab_agent.broker_sql_sources WHERE source_key='example-reference'",
            &[],
        )
        .await
        .unwrap_err();
    assert_eq!(
        lab.code(),
        Some(&tokio_postgres::error::SqlState::CHECK_VIOLATION)
    );

    let project = Uuid::new_v4();
    api.create_project(
        a,
        &CreateProject {
            project_id: project,
            group: "pilot-lab".into(),
            title: "Reference query".into(),
            visibility: Visibility::Private,
        },
    )
    .await
    .unwrap();
    api.checkpoint_project(
        a,
        project,
        &CheckpointRequest {
            expected_revision: 0,
            request_id: Uuid::new_v4(),
            state: research(),
        },
    )
    .await
    .unwrap();
    let run = api
        .start_research_run(
            a,
            project,
            &StartRun {
                run_id: Uuid::new_v4(),
                expected_revision: 1,
                allowed_profiles: vec!["reference-sql".into()],
                max_turns: 4,
                max_parallel: 2,
            },
        )
        .await
        .unwrap();
    let claim = coordinator
        .claim_research_run("lead-a", 90)
        .await
        .unwrap()
        .unwrap();
    let generation = claim["run"]["generation"].as_i64().unwrap();

    // The run reads only the sources of its allowed methods, without connection or snapshot files.
    let sources = coordinator
        .research_run_sql_sources(run, "lead-a", generation)
        .await
        .unwrap();
    assert_eq!(sources.as_array().unwrap().len(), 1);
    assert_eq!(sources[0]["source"], "example-reference");
    assert!(sources[0].get("connection_ref").is_none() && sources[0].get("snapshot").is_none());
    // No schema card is registered yet (migration 047), so the source carries none.
    assert!(sources[0].get("card").is_none() && sources[0].get("card_sha256").is_none());

    // A schema card is measured as the reader against the snapshot the source names.
    let (mut reader, reader_connection) = dsn
        .replace("user=postgres", "user=reference_reader_qual")
        .parse::<Config>()
        .unwrap()
        .connect(NoTls)
        .await
        .unwrap();
    let reader_connection = tokio::spawn(reader_connection);
    let card_source = |manifest: &str| -> CardSource {
        serde_json::from_value(source("example-reference", "reference-sql", manifest)).unwrap()
    };
    let spec = |joins: Value| -> CardSpec {
        serde_json::from_value(json!({"source": "example-reference", "joins": joins, "values": ["food.description"],
            "lookups": [{"relation": "food", "key": "fdc_id", "label": "description", "count_by": "food_nutrient.fdc_id"}]}))
        .unwrap()
    };
    let joins = json!([{"from": "food_nutrient.fdc_id", "to": "food.fdc_id"}]);
    let card = source_card::build(&mut reader, &card_source(MANIFEST), &spec(joins.clone()))
        .await
        .unwrap();
    assert_eq!(
        card["relations"][0],
        json!({"name": "food", "rows": 4, "keys": [["fdc_id"]]})
    );
    assert_eq!(
        card["relations"][1],
        json!({"name": "food_nutrient", "rows": 4, "missing": {"amount": 1}})
    );
    assert_eq!(card["joins"][0]["on"], "food_nutrient.fdc_id = food.fdc_id");
    assert_eq!(
        (
            card["joins"][0]["rows"].as_i64(),
            card["joins"][0]["matched"].as_i64()
        ),
        (Some(4), Some(4))
    );
    assert_eq!(card["values"][0]["values"].as_array().unwrap().len(), 4);
    assert_eq!(
        card["lookups"][0]["entries"][1],
        json!([2, "Example food B, raw", 1])
    );
    // The same snapshot gives the same card.
    assert_eq!(
        source_card::build(&mut reader, &card_source(MANIFEST), &spec(joins.clone()))
            .await
            .unwrap(),
        card
    );
    // Another snapshot, a join to a column that does not identify one row, or a column the source does not list.
    let fault = |r: anyhow::Result<Value>| r.unwrap_err().to_string();
    assert_eq!(
        fault(
            source_card::build(
                &mut reader,
                &card_source(&"0".repeat(64)),
                &spec(joins.clone())
            )
            .await
        ),
        "card_snapshot_mismatch"
    );
    assert_eq!(
        fault(
            source_card::build(
                &mut reader,
                &card_source(MANIFEST),
                &spec(json!([{"from": "food.fdc_id", "to": "food_nutrient.amount"}]))
            )
            .await
        ),
        "card_join_target_not_unique"
    );
    assert_eq!(
        fault(
            source_card::build(
                &mut reader,
                &card_source(MANIFEST),
                &spec(json!([{"from": "food.fdc_id", "to": "food_nutrient.nutrient_id"}]))
            )
            .await
        ),
        "card_spec_unknown_column"
    );

    // A join target may have NULL rows: equality never matches them, so only duplicate non-NULL values can fan out a
    // source row. This has the FNDDS shape that prompted the rule: integer nutrient ids join to text nutrient numbers.
    owner
        .batch_execute(
            "CREATE TABLE reference_qual.food_nutrient_join (id integer PRIMARY KEY, nutrient_id integer NOT NULL);
             INSERT INTO reference_qual.food_nutrient_join VALUES (1, 1003), (2, 1008), (3, 1162);
             CREATE TABLE reference_qual.nutrient_join (id integer PRIMARY KEY, nutrient_nbr text);
             INSERT INTO reference_qual.nutrient_join VALUES
                (11, '1003'), (12, '1008'), (13, '1162'), (14, NULL);
             CREATE TABLE reference_qual.nutrient_join_duplicate (id integer PRIMARY KEY, nutrient_nbr text);
             INSERT INTO reference_qual.nutrient_join_duplicate VALUES
                (21, '1003'), (22, '1003'), (23, NULL);
             CREATE TABLE reference_qual.nutrient_join_null (id integer PRIMARY KEY, nutrient_nbr text);
             INSERT INTO reference_qual.nutrient_join_null VALUES (31, NULL), (32, NULL);
             CREATE TABLE reference_qual.bigint_join (id integer PRIMARY KEY, nutrient_id bigint NOT NULL);
             INSERT INTO reference_qual.bigint_join VALUES
                (41, 9007199254740992), (42, 9007199254740993);
             CREATE TABLE reference_qual.double_join (id integer PRIMARY KEY, nutrient_nbr double precision);
             INSERT INTO reference_qual.double_join VALUES
                (51, 9007199254740992::double precision),
                (52, 9007199254740994::double precision), (53, NULL);
             GRANT SELECT ON reference_qual.food_nutrient_join, reference_qual.nutrient_join,
                reference_qual.nutrient_join_duplicate, reference_qual.nutrient_join_null TO reference_reader_qual;",
        )
        .await
        .unwrap();
    let nullable_source: CardSource = serde_json::from_value(json!({
        "source": "example-reference", "schema": "reference_qual", "statement_timeout_ms": 5000,
        "snapshot": {"manifest_sha256": MANIFEST},
        "relations": [
            {"name": "food_nutrient_join", "columns": [
                {"name": "id", "type": "integer"}, {"name": "nutrient_id", "type": "integer"}]},
            {"name": "nutrient_join", "columns": [
                {"name": "id", "type": "integer"}, {"name": "nutrient_nbr", "type": "text"}]},
            {"name": "nutrient_join_duplicate", "columns": [
                {"name": "id", "type": "integer"}, {"name": "nutrient_nbr", "type": "text"}]},
            {"name": "nutrient_join_null", "columns": [
                {"name": "id", "type": "integer"}, {"name": "nutrient_nbr", "type": "text"}]}
        ]
    }))
    .unwrap();
    let nullable_spec = |target: &str| -> CardSpec {
        serde_json::from_value(json!({"source": "example-reference", "joins": [{
            "from": "food_nutrient_join.nutrient_id", "to": target}]}))
        .unwrap()
    };
    let nullable = source_card::build(
        &mut reader,
        &nullable_source,
        &nullable_spec("nutrient_join.nutrient_nbr"),
    )
    .await
    .unwrap();
    assert_eq!(
        nullable["joins"][0],
        json!({"from": "food_nutrient_join.nutrient_id", "to": "nutrient_join.nutrient_nbr",
            "on": "food_nutrient_join.nutrient_id::text = nutrient_join.nutrient_nbr", "rows": 3, "matched": 3})
    );
    for target in [
        "nutrient_join_duplicate.nutrient_nbr",
        "nutrient_join_null.nutrient_nbr",
    ] {
        assert_eq!(
            fault(source_card::build(&mut reader, &nullable_source, &nullable_spec(target)).await),
            "card_join_target_not_unique"
        );
    }
    // The adjacent bigint values collapse under a double-precision comparison. These two tables are deliberately not
    // granted to the reader: the lossy type pair must be refused while declarations resolve, before any measurement.
    let lossy_source: CardSource = serde_json::from_value(json!({
        "source": "example-reference", "schema": "reference_qual", "statement_timeout_ms": 5000,
        "snapshot": {"manifest_sha256": MANIFEST},
        "relations": [
            {"name": "bigint_join", "columns": [
                {"name": "id", "type": "integer"}, {"name": "nutrient_id", "type": "bigint"}]},
            {"name": "double_join", "columns": [
                {"name": "id", "type": "integer"}, {"name": "nutrient_nbr", "type": "double precision"}]}
        ]
    }))
    .unwrap();
    let lossy_spec: CardSpec =
        serde_json::from_value(json!({"source": "example-reference", "joins": [{
        "from": "bigint_join.nutrient_id", "to": "double_join.nutrient_nbr"}]}))
        .unwrap();
    assert_eq!(
        fault(source_card::build(&mut reader, &lossy_source, &lossy_spec).await),
        "card_join_types_incompatible"
    );

    // Rows are counted with the join's own comparison (a numeric key against an integer column compares
    // as numbers, not as the texts "1.0" and "1"), an index a failed build left invalid is no key, and an INCLUDE column
    // is not part of one. The failed index is on a column with no missing value, so only its validity keeps it out.
    owner.batch_execute("
        CREATE TABLE reference_qual.grade (id numeric PRIMARY KEY, label text NOT NULL);
        INSERT INTO reference_qual.grade VALUES (1.0, 'Grade one'), (2.50, 'Grade two and a half');
        CREATE TABLE reference_qual.graded (id integer PRIMARY KEY, grade_id integer, note text NOT NULL,
            kind text NOT NULL);
        INSERT INTO reference_qual.graded VALUES (1, 1, 'a', 'x'), (2, 1, 'b', 'x'), (3, NULL, 'c', 'y');
        CREATE UNIQUE INDEX graded_id_note ON reference_qual.graded (id) INCLUDE (note);
        GRANT SELECT ON reference_qual.grade, reference_qual.graded TO reference_reader_qual;").await.unwrap();
    assert!(owner
        .batch_execute("CREATE UNIQUE INDEX CONCURRENTLY graded_kind_unique ON reference_qual.graded (kind)")
        .await
        .is_err());
    let graded_source: CardSource = serde_json::from_value(json!({"source": "example-reference", "schema": "reference_qual",
        "statement_timeout_ms": 5000, "snapshot": {"manifest_sha256": MANIFEST},
        "relations": [{"name": "grade", "columns": [{"name": "id", "type": "numeric"}, {"name": "label", "type": "text"}]},
            {"name": "graded", "columns": [{"name": "id", "type": "integer"}, {"name": "grade_id", "type": "integer"},
                {"name": "note", "type": "text"}, {"name": "kind", "type": "text"}]}]}))
    .unwrap();
    let graded_spec: CardSpec = serde_json::from_value(json!({"source": "example-reference",
        "lookups": [{"relation": "grade", "key": "id", "label": "label", "count_by": "graded.grade_id"}]}))
    .unwrap();
    let graded = source_card::build(&mut reader, &graded_source, &graded_spec)
        .await
        .unwrap();
    assert_eq!(
        graded["lookups"][0]["entries"],
        json!([["1.0", "Grade one", 2], ["2.50", "Grade two and a half", 0]])
    );
    assert_eq!(graded["relations"][1]["keys"], json!([["id"]]));

    // L3b: priced beyond its budget, a table is measured from a seeded block sample and a view by the planner's estimate,
    // each marked; a sampled relation keeps only its primary key; a declaration over a relation not counted exactly is
    // refused; the same budget measures the same card again. Within the timeout's budget the same source is exact.
    owner.batch_execute("
        CREATE TABLE reference_qual.coded (id integer PRIMARY KEY, code text NOT NULL UNIQUE, note text);
        INSERT INTO reference_qual.coded SELECT g, 'c' || g, CASE WHEN g % 3 = 0 THEN NULL ELSE repeat('x', 400) END
            FROM generate_series(1, 2000) g;
        ANALYZE reference_qual.coded;
        CREATE VIEW reference_qual.coded_notes AS SELECT id, note FROM reference_qual.coded;
        GRANT SELECT ON reference_qual.coded, reference_qual.coded_notes TO reference_reader_qual;").await.unwrap();
    let coded_source: CardSource = serde_json::from_value(json!({"source": "example-reference", "schema": "reference_qual",
        "statement_timeout_ms": 5000, "snapshot": {"manifest_sha256": MANIFEST},
        "relations": [{"name": "coded", "columns": [{"name": "id", "type": "integer"}, {"name": "code", "type": "text"},
                {"name": "note", "type": "text"}]},
            {"name": "coded_notes", "columns": [{"name": "id", "type": "integer"}, {"name": "note", "type": "text"}]}]}))
    .unwrap();
    let bare: CardSpec = serde_json::from_value(json!({"source": "example-reference"})).unwrap();
    let (whole, _) = source_card::build_live(&mut reader, &coded_source, &bare)
        .await
        .unwrap();
    assert_eq!(whole["generator"], "source-card/2");
    assert_eq!(
        whole["relations"],
        json!([{"name": "coded", "rows": 2000, "keys": [["code"], ["id"]], "missing": {"note": 666}},
            {"name": "coded_notes", "rows": 2000, "missing": {"note": 666}}])
    );
    let plan: Value = reader
        .query_one(
            "EXPLAIN (FORMAT JSON) SELECT id, code, note FROM reference_qual.coded",
            &[],
        )
        .await
        .unwrap()
        .get(0);
    let cost = plan[0]["Plan"]["Total Cost"].as_f64().unwrap();
    let below = source_card::Budget {
        exact_cost: cost * 0.9,
        exact_cost_total: cost * 0.9,
        sample_rows: 1_000_000.0,
        seconds_total: 600.0,
    };
    let (sampled, _) = source_card::build_within(&mut reader, &coded_source, &bare, Some(below))
        .await
        .unwrap();
    let coded = &sampled["relations"][0];
    assert_eq!(coded["estimate"]["method"], "block_sample");
    let percent = coded["estimate"]["percent"].as_f64().unwrap();
    let read = coded["estimate"]["sampled_rows"].as_i64().unwrap();
    let rows = coded["rows"].as_i64().unwrap();
    assert!(percent > 40.0 && percent <= 45.0, "{percent}");
    assert_eq!(rows, (read as f64 * 100.0 / percent).round() as i64);
    assert!((1000..=3000).contains(&rows), "{rows}");
    assert!(coded["missing"]["note"].as_i64().unwrap() > 0);
    assert_eq!(coded["keys"], json!([["id"]]));
    assert_eq!(
        (
            &sampled["relations"][1]["name"],
            &sampled["relations"][1]["estimate"]["method"]
        ),
        (&json!("coded_notes"), &json!("planner"))
    );
    assert!(sampled["relations"][1].get("missing").is_none());
    assert_eq!(
        source_card::build_within(&mut reader, &coded_source, &bare, Some(below))
            .await
            .unwrap()
            .0,
        sampled
    );
    let declared: CardSpec =
        serde_json::from_value(json!({"source": "example-reference", "values": ["coded.code"]}))
            .unwrap();
    assert_eq!(
        fault(
            source_card::build_within(&mut reader, &coded_source, &declared, Some(below))
                .await
                .map(|(card, _)| card)
        ),
        "card_spec_relation_too_costly:coded"
    );

    // A relation whose reader holds only some columns, as a lab keeps columns from agents, is priced and measured over
    // its listed columns, counted or sampled; a read of the whole row is refused.
    owner.batch_execute("
        CREATE TABLE reference_qual.partly (id integer PRIMARY KEY, kept text, withheld text UNIQUE);
        INSERT INTO reference_qual.partly SELECT g, CASE WHEN g % 5 = 0 THEN NULL ELSE 'k' || g END, 'w' || g
            FROM generate_series(1, 500) g;
        ANALYZE reference_qual.partly;
        GRANT SELECT (id, kept) ON reference_qual.partly TO reference_reader_qual;").await.unwrap();
    assert!(
        reader
            .query_one(
                "EXPLAIN (FORMAT JSON) SELECT * FROM reference_qual.partly",
                &[]
            )
            .await
            .is_err()
    );
    let partly_source: CardSource = serde_json::from_value(json!({"source": "example-reference", "schema": "reference_qual",
        "statement_timeout_ms": 5000, "snapshot": {"manifest_sha256": MANIFEST},
        "relations": [{"name": "partly", "columns": [{"name": "id", "type": "integer"}, {"name": "kept", "type": "text"}]}]}))
    .unwrap();
    let (partly, _) = source_card::build_live(&mut reader, &partly_source, &bare)
        .await
        .unwrap();
    assert_eq!(
        partly["relations"],
        json!([{"name": "partly", "rows": 500, "keys": [["id"]], "missing": {"kept": 100}}])
    );
    let plan: Value = reader
        .query_one(
            "EXPLAIN (FORMAT JSON) SELECT id, kept FROM reference_qual.partly",
            &[],
        )
        .await
        .unwrap()
        .get(0);
    let cost = plan[0]["Plan"]["Total Cost"].as_f64().unwrap();
    let below = source_card::Budget {
        exact_cost: cost * 0.9,
        exact_cost_total: cost * 0.9,
        sample_rows: 1_000_000.0,
        seconds_total: 600.0,
    };
    let (sampled, _) = source_card::build_within(&mut reader, &partly_source, &bare, Some(below))
        .await
        .unwrap();
    assert_eq!(
        sampled["relations"][0]["estimate"]["method"],
        "block_sample"
    );
    assert_eq!(sampled["relations"][0]["keys"], json!([["id"]]));

    // The planner prices a view badly, so a read of a relation no declaration names runs under half the statement
    // timeout inside a savepoint: one cancelled for time is rolled back, takes the planner's estimate, and the card
    // goes on in the same snapshot. A declared relation keeps the full timeout (here one that runs past it ends the
    // card), and joins, values and lookups get the full timeout back. Once the card's counting time has passed, the
    // remaining undeclared relations are not read.
    owner.batch_execute("
        CREATE VIEW reference_qual.slow AS SELECT g AS id FROM generate_series(1, 3) g WHERE pg_sleep(0.5)::text = '';
        CREATE VIEW reference_qual.slower AS SELECT g AS id FROM generate_series(1, 6) g WHERE pg_sleep(0.5)::text = '';
        CREATE TABLE reference_qual.keyed_slow (id integer PRIMARY KEY);
        INSERT INTO reference_qual.keyed_slow SELECT generate_series(1, 2000);
        ALTER TABLE reference_qual.keyed_slow ENABLE ROW LEVEL SECURITY;
        CREATE POLICY slow_rows ON reference_qual.keyed_slow FOR SELECT USING (pg_sleep(0.01)::text = '');
        GRANT SELECT ON reference_qual.slow, reference_qual.slower, reference_qual.keyed_slow TO reference_reader_qual;").await.unwrap();
    let timed = |names: &[&str]| -> CardSource {
        let columns = |name: &str| match name {
            "partly" => {
                json!([{"name": "id", "type": "integer"}, {"name": "kept", "type": "text"}])
            }
            "grade" => {
                json!([{"name": "id", "type": "numeric"}, {"name": "label", "type": "text"}])
            }
            _ => json!([{"name": "id", "type": "integer"}]),
        };
        serde_json::from_value(json!({"source": "example-reference", "schema": "reference_qual",
            "statement_timeout_ms": 2000, "snapshot": {"manifest_sha256": MANIFEST},
            "relations": names.iter().map(|n| json!({"name": n, "columns": columns(n)})).collect::<Vec<_>>()}))
        .unwrap()
    };
    let (card_slow, _) = source_card::build_live(&mut reader, &timed(&["slow", "partly"]), &bare)
        .await
        .unwrap();
    assert_eq!(card_slow["relations"][0]["name"], "slow");
    assert_eq!(card_slow["relations"][0]["estimate"]["method"], "planner");
    assert!(card_slow["relations"][0].get("missing").is_none());
    assert_eq!(
        card_slow["relations"][1],
        json!({"name": "partly", "rows": 500, "keys": [["id"]], "missing": {"kept": 100}})
    );
    // A keyed table whose count or sample is cancelled keeps no key: its rows were never seen.
    let (keyed, _) = source_card::build_live(&mut reader, &timed(&["keyed_slow"]), &bare)
        .await
        .unwrap();
    assert_eq!(keyed["relations"][0]["estimate"]["method"], "planner");
    assert!(keyed["relations"][0].get("keys").is_none());
    let plan: Value = reader
        .query_one(
            "EXPLAIN (FORMAT JSON) SELECT id FROM reference_qual.keyed_slow",
            &[],
        )
        .await
        .unwrap()
        .get(0);
    let cost = plan[0]["Plan"]["Total Cost"].as_f64().unwrap();
    let tight = source_card::Budget {
        exact_cost: cost * 0.9,
        exact_cost_total: cost * 0.9,
        sample_rows: 1_000_000.0,
        seconds_total: 600.0,
    };
    let (keyed, _) =
        source_card::build_within(&mut reader, &timed(&["keyed_slow"]), &bare, Some(tight))
            .await
            .unwrap();
    assert_eq!(keyed["relations"][0]["estimate"]["method"], "planner");
    assert!(keyed["relations"][0].get("keys").is_none());
    let slow_values: CardSpec =
        serde_json::from_value(json!({"source": "example-reference", "values": ["slow.id"]}))
            .unwrap();
    let (counted, _) =
        source_card::build_live(&mut reader, &timed(&["slow", "partly"]), &slow_values)
            .await
            .unwrap();
    assert_eq!(counted["relations"][0], json!({"name": "slow", "rows": 3}));
    assert_eq!(
        counted["values"][0]["values"],
        json!([["1", 1], ["2", 1], ["3", 1]])
    );
    let slower_values: CardSpec =
        serde_json::from_value(json!({"source": "example-reference", "values": ["slower.id"]}))
            .unwrap();
    assert_eq!(
        fault(
            source_card::build_live(&mut reader, &timed(&["slower"]), &slower_values)
                .await
                .map(|(card, _)| card)
        ),
        "card_measure_failed:slower"
    );
    let labels: CardSpec =
        serde_json::from_value(json!({"source": "example-reference", "values": ["grade.label"]}))
            .unwrap();
    let spent = source_card::Budget {
        seconds_total: 0.0,
        ..source_card::Budget::for_timeout(2000)
    };
    let (unread, _) = source_card::build_within(
        &mut reader,
        &timed(&["slow", "partly", "grade"]),
        &labels,
        Some(spent),
    )
    .await
    .unwrap();
    assert_eq!(unread["relations"][0]["estimate"]["method"], "planner");
    assert_eq!(unread["relations"][1]["estimate"]["method"], "planner");
    assert_eq!(
        unread["relations"][2],
        json!({"name": "grade", "rows": 2, "keys": [["id"]]})
    );

    // Registration: the card must name the source's snapshot and relations; the same card again returns the stored
    // digest; cards never change.
    let register_card = |value: Value| {
        let owner = &owner;
        async move {
            owner
                .query_one(
                    "SELECT enrichedlab_agent.broker_register_sql_source_card('research-site',$1)",
                    &[&value],
                )
                .await
        }
    };
    let stored: Value = register_card(card.clone()).await.unwrap().get(0);
    let digest = stored["card_sha256"].as_str().unwrap().to_owned();
    assert!(digest.starts_with("sha256:") && digest.len() == 71);
    let again: Value = register_card(card.clone()).await.unwrap().get(0);
    assert_eq!(again["card_sha256"], digest.as_str());
    let mut other = card.clone();
    other["manifest_sha256"] = json!("0".repeat(64));
    assert!(register_card(other).await.is_err());
    let mut other = card.clone();
    other["relations"][0]["name"] = json!("nutrient");
    assert!(register_card(other).await.is_err());
    // A lookup entry narrower than its columns, which the coordinator could not read whole, is refused too.
    let mut other = card.clone();
    other["lookups"][0]["entries"][0] = json!([1]);
    assert!(register_card(other).await.is_err());
    let mut other = card.clone();
    other["joins"][0]["rows"] = json!("four");
    assert!(register_card(other).await.is_err());
    // Counts are whole numbers with matched rows no more than rows, the rule the coordinator applies.
    for (rows, matched) in [
        (json!(4), json!(-1)),
        (json!(4.0), json!(4)),
        (json!(4), json!(5)),
        (json!(1e19), json!(1)),
    ] {
        let mut other = card.clone();
        other["joins"][0]["rows"] = rows.clone();
        other["joins"][0]["matched"] = matched.clone();
        assert!(register_card(other).await.is_err(), "{rows} {matched}");
    }
    let mut other = card.clone();
    other["relations"][0]["rows"] = json!(2.5);
    assert!(register_card(other).await.is_err());
    for statement in [
        "UPDATE enrichedlab_agent.broker_sql_source_cards SET manifest_sha256=manifest_sha256",
        "DELETE FROM enrichedlab_agent.broker_sql_source_cards",
    ] {
        assert!(owner.execute(statement, &[]).await.is_err(), "{statement}");
    }
    // The run's source now carries the card and the digest its registration stored.
    let sources = coordinator
        .research_run_sql_sources(run, "lead-a", generation)
        .await
        .unwrap();
    assert_eq!(sources[0]["card"], card);
    assert_eq!(sources[0]["card_sha256"], digest.as_str());
    drop(reader);
    reader_connection.await.unwrap().unwrap();
    assert_eq!(
        coordinator
            .research_run_sql_sources(run, "lead-b", generation)
            .await
            .unwrap_err()
            .code,
        "lease_fence_lost"
    );

    let state = serde_json::to_value(research()).unwrap();
    let record = json!({"schema_version":1,"test":"bounded metadata"});
    let commit = |actions: Value| {
        let coordinator = coordinator.clone();
        let state = state.clone();
        let record = record.clone();
        async move {
            coordinator
                .commit_research_turn(
                    run, "lead-a", generation, 1, &state, &actions, &record, "delegate",
                )
                .await
        }
    };
    let good = reviewed_queries::prepare(&owner, &coordinator, run, generation).await;
    for (actions, label) in [
        (action("rank-foods", None), "missing"),
        (
            action(
                "rank-foods",
                Some({
                    let mut q = good.clone();
                    q["sha256"] = json!(format!("sha256:{}", "0".repeat(64)));
                    q
                }),
            ),
            "digest",
        ),
        (
            action(
                "rank-foods",
                Some({
                    let mut q = good.clone();
                    q["relations"] = json!(["branded_food"]);
                    q
                }),
            ),
            "relation",
        ),
        (
            action(
                "rank-foods",
                Some({
                    let mut q = good.clone();
                    q["row_limit"] = json!(101);
                    q
                }),
            ),
            "cap",
        ),
        (
            action(
                "rank-foods",
                Some({
                    let mut q = good.clone();
                    q["guard"] = json!("other/v1");
                    q
                }),
            ),
            "guard",
        ),
        (
            action(
                "rank-foods",
                Some({
                    let mut q = good.clone();
                    q["note"] = json!("extra");
                    q
                }),
            ),
            "field",
        ),
        (
            action(
                "rank-foods",
                Some({
                    let mut q = good.clone();
                    q["source"] = json!("stale-reference");
                    q
                }),
            ),
            "source",
        ),
    ] {
        let error = commit(actions).await.unwrap_err();
        assert_eq!(
            (error.kind, error.code),
            (StoreErrorKind::Invalid, "query_invalid"),
            "{label}"
        );
    }
    let mut both = action("rank-foods", Some(good.clone()));
    both.as_array_mut().unwrap().push(
        action(
            "bad-column",
            Some(admitted_query(
                "SELECT f.missing_column FROM food f ORDER BY 1",
            )),
        )[0]
        .clone(),
    );
    let committed = commit(both).await.unwrap();
    let task: Uuid = serde_json::from_value(committed["tasks"][0]["task_id"].clone()).unwrap();
    let failing: Uuid = serde_json::from_value(committed["tasks"][1]["task_id"].clone()).unwrap();
    assert!(
        owner
            .execute(
                "UPDATE enrichedlab_agent.broker_task_queries SET admitted_sql='SELECT 1'",
                &[]
            )
            .await
            .is_err()
    );

    // The worker's lease alone reads the query; the API role cannot.
    let root = std::env::temp_dir().join(format!("reference_sql_codetestfile_{}", Uuid::new_v4()));
    let connections = root.join("connections");
    fs::create_dir_all(root.join("artifacts")).unwrap();
    fs::create_dir_all(&connections).unwrap();
    fs::write(
        connections.join("reference-qual.dsn"),
        dsn.replace("user=postgres", "user=reference_reader_qual"),
    )
    .unwrap();
    let config = WorkerConfig {
        database_dsn: dsn.replace("user=postgres", "user=broker_worker_qual"),
        tenant: "research-site".into(),
        artifact_root: root.join("artifacts"),
        controller_root: root.clone(),
        controller_state: root.clone(),
        controller_config: root.clone(),
        controller_bin: PathBuf::from("/bin/false"),
        openshell_bin: PathBuf::from("/bin/false"),
        docker_bin: PathBuf::from("/bin/false"),
        worker_id: "query-worker".into(),
        direct_dispatch: None,
        query_connection_dir: Some(connections),
        render_url: None,
        dataset_fetcher_config: None,
        live_output: false,
        max_parallel: 1,
        lease_seconds: 120,
        poll_interval: Duration::from_millis(200),
        database_pool: 2,
        max_workspace_files: 10,
        max_workspace_bytes: 1024,
    };
    let mut ran = Vec::new();
    for _ in 0..2 {
        let claimed = worker
            .claim_task("query-worker", 120)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(claimed.route, query_run::ROUTE);
        assert!(
            api.task_query(
                claimed.id,
                "query-worker",
                claimed.attempt,
                claimed.lease_generation
            )
            .await
            .is_err()
        );
        let seen = worker
            .task_query(
                claimed.id,
                "query-worker",
                claimed.attempt,
                claimed.lease_generation,
            )
            .await
            .unwrap()
            .unwrap();
        assert!(
            worker
                .task_query(
                    claimed.id,
                    "other-worker",
                    claimed.attempt,
                    claimed.lease_generation
                )
                .await
                .unwrap()
                .is_none()
        );
        assert_eq!(seen.connection_ref, "reference-qual");
        ran.push(claimed.id);
        query_run::execute(worker.clone(), &config, claimed, CancellationToken::new())
            .await
            .unwrap();
    }
    assert_eq!(
        ran.iter().collect::<BTreeSet<_>>(),
        [task, failing].iter().collect::<BTreeSet<_>>()
    );

    // The ranking finished with PostgreSQL's CSV, a receipt and a digest whose table holds the cells.
    let row = owner.query_one("SELECT status,result_class,coalesce(error_code,'') FROM enrichedlab_agent.broker_tasks WHERE task_id=$1", &[&task]).await.unwrap();
    assert_eq!(
        (
            row.get::<_, String>(0),
            row.get::<_, String>(1),
            row.get::<_, String>(2)
        ),
        ("completed".into(), "query_completed".into(), String::new())
    );
    let receipt = owner.query_one("SELECT status,row_count,columns,package_sha256 FROM enrichedlab_agent.broker_query_receipts WHERE task_id=$1", &[&task]).await.unwrap();
    assert_eq!(
        (receipt.get::<_, String>(0), receipt.get::<_, i32>(1)),
        ("completed".into(), 4)
    );
    assert_eq!(
        receipt.get::<_, Value>(2),
        json!([{"name": "description", "type": "text"}, {"name": "amount", "type": "float8"}])
    );
    let package_sha: String = receipt.get(3);
    let artifacts = api.list_artifacts(a, task).await.unwrap();
    let package = artifacts.iter().find(|x| x.role == "result").unwrap();
    assert_eq!(package.sha256, package_sha);
    let bytes = fs::read(root.join("artifacts").join(&package.relative_path)).unwrap();
    assert_eq!(hex::encode(Sha256::digest(&bytes)), package_sha);
    reviewed_queries::check_package(&bytes, &good);
    let digest = api.artifact_digest(a, task, package.id).await.unwrap();
    assert_eq!(digest["status"], "summarized");
    let table = &digest["digest"]["tables"][0];
    assert_eq!(table["path"], "results/query_result.csv");
    assert_eq!(table["columns"], json!(["description", "amount"]));
    assert_eq!(
        table["cells"],
        json!([
            ["Example food A", "48.2"],
            ["Example food B, raw", "12.9"],
            ["Example food D", "12.9"],
            ["Example \"quoted\" food", "NA"]
        ])
    );
    // PostgreSQL writes a NULL as NA and quotes the text NA, and a one-column result quotes every other value, so no
    // row is a blank line and the rows counted are the rows the database returned.
    for (sql, width, expected, rows) in [
        (
            "SELECT n.amount FROM reference_qual.food_nutrient n ORDER BY n.amount NULLS LAST",
            1,
            "amount\n\"12.9\"\n\"12.9\"\n\"48.2\"\nNA\n",
            4,
        ),
        (
            "SELECT v FROM (VALUES (''), ('NA'), (NULL), ('x')) t(v)",
            1,
            "v\n\"\"\n\"NA\"\nNA\n\"x\"\n",
            4,
        ),
        (
            "SELECT v, 1 AS n FROM (VALUES (''), ('NA'), (NULL)) t(v)",
            2,
            "v,n\n,1\n\"NA\",1\nNA,1\n",
            3,
        ),
    ] {
        let stream = owner
            .copy_out(&query_run::copy_statement(sql, width))
            .await
            .unwrap();
        futures_util::pin_mut!(stream);
        let mut csv = Vec::new();
        while let Some(chunk) = futures_util::StreamExt::next(&mut stream).await {
            csv.extend_from_slice(&chunk.unwrap());
        }
        assert_eq!(String::from_utf8(csv.clone()).unwrap(), expected);
        assert_eq!(query_run::count_rows(&csv, width), Some(rows), "{sql}");
    }

    // A column the database does not have ends the other query with a code the lead can act on.
    let row = owner.query_one("SELECT status,coalesce(error_code,'') FROM enrichedlab_agent.broker_tasks WHERE task_id=$1", &[&failing]).await.unwrap();
    assert_eq!(
        (row.get::<_, String>(0), row.get::<_, String>(1)),
        ("failed".into(), "query_column_unknown".into())
    );

    // A reader sees the query, its source and receipts; nobody else does. Lineage names the class.
    let detail = api.task_query_detail(a, task).await.unwrap();
    assert_eq!(detail["admitted_sql"], good["admitted_sql"]);
    assert_eq!(detail["receipts"][0]["row_count"], 4);
    assert_eq!(detail["validation"], "unvalidated");
    assert_eq!(
        api.task_query_detail(outsider, task)
            .await
            .unwrap_err()
            .code,
        "task_not_found"
    );
    let lineage = api.task_lineage(a, task).await.unwrap();
    assert_eq!(lineage["method"]["tool_class"], "reviewed_query");
    assert_eq!(lineage["query"]["reviewed_query"], good["reviewed_query"]);
    assert_eq!(lineage["query"]["guard"], sql_guard::GUARD);
    // The source states what kind of data the query read, so no class gap remains (migration 039).
    assert_eq!(lineage["query"]["title"], "Example reference data");
    assert_eq!(lineage["query"]["release"], "Qualification release");
    assert_eq!(lineage["query"]["snapshot_sha256"], MANIFEST);
    assert_eq!(lineage["query"]["data_class"], "public_reference");
    assert_eq!(lineage["inputs"]["classes"], json!(["public_reference"]));
    assert_eq!(lineage["inputs"]["classes_recorded_by"], "query_source");
    assert!(
        !lineage["gaps"]
            .as_array()
            .unwrap()
            .iter()
            .any(|g| g == "input_class_not_recorded"),
        "{lineage}"
    );
    assert!(
        lineage["outputs"]
            .as_array()
            .unwrap()
            .iter()
            .all(|o| o["provenance_class"] == "query_output")
    );
    assert_eq!(lineage["review"]["state"], "unvalidated");

    // The reader login cannot write, even outside the worker's read-only transaction.
    let (reader, reader_connection) = dsn
        .replace("user=postgres", "user=reference_reader_qual")
        .parse::<Config>()
        .unwrap()
        .connect(NoTls)
        .await
        .unwrap();
    let reader_connection = tokio::spawn(reader_connection);
    assert!(
        reader
            .execute("INSERT INTO reference_qual.food VALUES (9,'x')", &[])
            .await
            .is_err()
    );
    assert!(
        reader
            .query("SELECT * FROM enrichedlab_agent.broker_tasks", &[])
            .await
            .is_err()
    );
    drop(reader);
    reader_connection.abort();

    // A source whose database no longer holds its snapshot refuses the query before it reads anything.
    let stale = Uuid::new_v4();
    api.create_project(
        a,
        &CreateProject {
            project_id: stale,
            group: "pilot-lab".into(),
            title: "Stale reference".into(),
            visibility: Visibility::Private,
        },
    )
    .await
    .unwrap();
    api.checkpoint_project(
        a,
        stale,
        &CheckpointRequest {
            expected_revision: 0,
            request_id: Uuid::new_v4(),
            state: research(),
        },
    )
    .await
    .unwrap();
    let stale_run = api
        .start_research_run(
            a,
            stale,
            &StartRun {
                run_id: Uuid::new_v4(),
                expected_revision: 1,
                allowed_profiles: vec!["reference-sql-stale".into()],
                max_turns: 4,
                max_parallel: 1,
            },
        )
        .await
        .unwrap();
    let stale_claim = coordinator
        .claim_research_run("lead-a", 90)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(stale_claim["run"]["run_id"], json!(stale_run));
    let stale_generation = stale_claim["run"]["generation"].as_i64().unwrap();
    let mut stale_query = good.clone();
    stale_query["source"] = json!("stale-reference");
    stale_query
        .as_object_mut()
        .unwrap()
        .remove("reviewed_query");
    let mut stale_action = action("rank-foods", Some(stale_query));
    stale_action[0]["profile"] = json!("reference-sql-stale");
    let stale_task: Uuid = serde_json::from_value(
        coordinator
            .commit_research_turn(
                stale_run,
                "lead-a",
                stale_generation,
                1,
                &state,
                &stale_action,
                &record,
                "delegate",
            )
            .await
            .unwrap()["tasks"][0]["task_id"]
            .clone(),
    )
    .unwrap();
    let claimed = worker
        .claim_task("query-worker", 120)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(claimed.id, stale_task);
    query_run::execute(worker.clone(), &config, claimed, CancellationToken::new())
        .await
        .unwrap();
    let row = owner.query_one("SELECT t.status,coalesce(t.error_code,''),r.status FROM enrichedlab_agent.broker_tasks t
        JOIN enrichedlab_agent.broker_query_receipts r USING (tenant_id,task_id) WHERE t.task_id=$1", &[&stale_task]).await.unwrap();
    assert_eq!(
        (
            row.get::<_, String>(0),
            row.get::<_, String>(1),
            row.get::<_, String>(2)
        ),
        (
            "failed".into(),
            "snapshot_mismatch".into(),
            "refused".into()
        )
    );

    // PostgreSQL reads `f.row_to_json` as the call `row_to_json(f)` when food has no such column. The guard admits it as
    // a column read; the worker asks the source's catalog in the query's transaction and refuses it before reading.
    // `(f.description).lower` is the call `lower(f.description)`; the guard refuses it, and the worker, which admits the
    // stored text again, refuses a record that carries it as though a coordinator had admitted it.
    let field = Uuid::new_v4();
    api.create_project(
        a,
        &CreateProject {
            project_id: field,
            group: "pilot-lab".into(),
            title: "Field notation".into(),
            visibility: Visibility::Private,
        },
    )
    .await
    .unwrap();
    api.checkpoint_project(
        a,
        field,
        &CheckpointRequest {
            expected_revision: 0,
            request_id: Uuid::new_v4(),
            state: research(),
        },
    )
    .await
    .unwrap();
    let field_run = api
        .start_research_run(
            a,
            field,
            &StartRun {
                run_id: Uuid::new_v4(),
                expected_revision: 1,
                allowed_profiles: vec!["reference-sql".into()],
                max_turns: 4,
                max_parallel: 2,
            },
        )
        .await
        .unwrap();
    let field_claim = coordinator
        .claim_research_run("lead-f", 90)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(field_claim["run"]["run_id"], json!(field_run));
    let field_generation = field_claim["run"]["generation"].as_i64().unwrap();
    let field_query = admitted_query("SELECT f.description, f.row_to_json FROM food f ORDER BY 1");
    let selection_sql =
        "SELECT f.description, (f.description).lower AS d FROM food f ORDER BY 1\nLIMIT 100";
    assert_eq!(
        sql_guard::admit(
            selection_sql,
            &Allowed {
                schema: "reference_qual",
                relations: &BTreeSet::from(["food".to_string(), "food_nutrient".to_string()]),
                functions: &BTreeSet::from([
                    "count".to_string(),
                    "max".to_string(),
                    "round".to_string()
                ]),
                row_cap: 100,
            },
        )
        .unwrap_err()
        .code,
        "sql_refused_function"
    );
    let selection_query = json!({"source": "example-reference", "proposed_sql": selection_sql,
        "admitted_sql": selection_sql, "sha256": format!("sha256:{}", hex::encode(Sha256::digest(selection_sql.as_bytes()))),
        "guard": sql_guard::GUARD, "relations": ["food"], "row_limit": 100});
    let mut field_actions = action("bad-column", Some(field_query));
    field_actions
        .as_array_mut()
        .unwrap()
        .push(action("rank-foods", Some(selection_query))[0].clone());
    let committed = coordinator
        .commit_research_turn(
            field_run,
            "lead-f",
            field_generation,
            1,
            &state,
            &field_actions,
            &record,
            "delegate",
        )
        .await
        .unwrap();
    let field_tasks: Vec<Uuid> = (0..2)
        .map(|i| serde_json::from_value(committed["tasks"][i]["task_id"].clone()).unwrap())
        .collect();
    for _ in 0..2 {
        let claimed = worker
            .claim_task("query-worker", 120)
            .await
            .unwrap()
            .unwrap();
        assert!(field_tasks.contains(&claimed.id));
        query_run::execute(worker.clone(), &config, claimed, CancellationToken::new())
            .await
            .unwrap();
    }
    for field_task in &field_tasks {
        let row = owner.query_one("SELECT t.status,coalesce(t.error_code,''),r.status FROM enrichedlab_agent.broker_tasks t
            JOIN enrichedlab_agent.broker_query_receipts r USING (tenant_id,task_id) WHERE t.task_id=$1", &[field_task]).await.unwrap();
        assert_eq!(
            (
                row.get::<_, String>(0),
                row.get::<_, String>(1),
                row.get::<_, String>(2)
            ),
            (
                "failed".into(),
                "sql_refused_function".into(),
                "refused".into()
            ),
            "{field_task}"
        );
    }

    // Earlier working queries (migration 042): a later run of the same person in the same lab reads the
    // admitted query of a finished query analysis in another lab-visible project, with its source and row count, and
    // nothing else. The earlier runs are paused so that each claim below is the run meant.
    owner
        .execute(
            "UPDATE enrichedlab_agent.broker_research_runs SET status='paused', lease_owner=NULL, lease_until=NULL
             WHERE run_id = ANY($1)",
            &[&vec![run, stale_run, field_run]],
        )
        .await
        .unwrap();
    let later = Uuid::new_v4();
    api.create_project(
        a,
        &CreateProject {
            project_id: later,
            group: "pilot-lab".into(),
            title: "Later question".into(),
            visibility: Visibility::Private,
        },
    )
    .await
    .unwrap();
    api.checkpoint_project(
        a,
        later,
        &CheckpointRequest {
            expected_revision: 0,
            request_id: Uuid::new_v4(),
            state: research(),
        },
    )
    .await
    .unwrap();
    let later_run = api
        .start_research_run(
            a,
            later,
            &StartRun {
                run_id: Uuid::new_v4(),
                expected_revision: 1,
                allowed_profiles: vec!["reference-sql".into()],
                max_turns: 4,
                max_parallel: 1,
            },
        )
        .await
        .unwrap();
    let later_claim = coordinator
        .claim_research_run("lead-c", 90)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(later_claim["run"]["run_id"], json!(later_run));
    let later_generation = later_claim["run"]["generation"].as_i64().unwrap();
    let prior = |tasks: Vec<Uuid>| {
        let coordinator = coordinator.clone();
        async move {
            coordinator
                .research_run_prior_queries(later_run, "lead-c", later_generation, &tasks)
                .await
        }
    };
    let asked = vec![task, failing, stale_task, Uuid::new_v4(), task];
    // A private project's analyses are not read, even the same person's.
    assert_eq!(prior(asked.clone()).await.unwrap(), json!([]));
    owner
        .execute(
            "UPDATE enrichedlab_agent.broker_projects SET visibility='lab' WHERE project_id = ANY($1)",
            &[&vec![project, stale]],
        )
        .await
        .unwrap();
    // Lab-visible: the finished query alone, once; the failed query, the method this run may not use and an unknown
    // task are left out.
    let seen = prior(asked.clone()).await.unwrap();
    assert_eq!(seen.as_array().unwrap().len(), 1, "{seen}");
    assert_eq!(seen[0]["task_id"], json!(task));
    assert_eq!(seen[0]["admitted_sql"], good["admitted_sql"]);
    assert_eq!(seen[0]["sql_sha256"], good["sha256"]);
    assert_eq!(
        (
            seen[0]["row_count"].clone(),
            seen[0]["title"].clone(),
            seen[0]["source"].clone()
        ),
        (
            json!(4),
            json!("Example reference data"),
            json!("example-reference")
        )
    );
    let mut keys: Vec<&str> = seen[0]
        .as_object()
        .unwrap()
        .keys()
        .map(String::as_str)
        .collect();
    keys.sort_unstable();
    assert_eq!(
        keys,
        [
            "admitted_sql",
            "release",
            "row_count",
            "source",
            "sql_sha256",
            "task_id",
            "title"
        ]
    );
    // Another worker's claim, or more than twelve tasks, is refused.
    assert_eq!(
        coordinator
            .research_run_prior_queries(later_run, "lead-a", later_generation, &asked)
            .await
            .unwrap_err()
            .code,
        "lease_fence_lost"
    );
    assert!(prior(vec![task; 13]).await.is_err());
    // A run never reads back its own project's queries: the first run, claimed again, gets nothing for its own task,
    // which the later run was just offered.
    let requeue = |held: Uuid, next: Uuid| {
        let owner = &owner;
        async move {
            owner
                .execute(
                    "UPDATE enrichedlab_agent.broker_research_runs SET status='paused', lease_owner=NULL, lease_until=NULL
                     WHERE run_id=$1",
                    &[&held],
                )
                .await
                .unwrap();
            owner
                .execute(
                    "UPDATE enrichedlab_agent.broker_research_runs SET status='queued', available_at=clock_timestamp()
                     WHERE run_id=$1",
                    &[&next],
                )
                .await
                .unwrap();
        }
    };
    requeue(later_run, run).await;
    let again = coordinator
        .claim_research_run("lead-a", 90)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(again["run"]["run_id"], json!(run));
    assert_eq!(
        coordinator
            .research_run_prior_queries(
                run,
                "lead-a",
                again["run"]["generation"].as_i64().unwrap(),
                &[task]
            )
            .await
            .unwrap(),
        json!([])
    );
    // Its own project's queries (migration 043), in the order asked, each with its newest receipt: the
    // finished one with its row count and the failed one with its code. Another project's task and an unknown task are
    // left out; another worker, or more than 64 tasks, is refused.
    let again_generation = again["run"]["generation"].as_i64().unwrap();
    let own = coordinator
        .research_run_own_queries(
            run,
            "lead-a",
            again_generation,
            &[failing, stale_task, Uuid::new_v4(), task],
        )
        .await
        .unwrap();
    assert_eq!(own.as_array().unwrap().len(), 2, "{own}");
    assert_eq!(
        (
            own[0]["task_id"].clone(),
            own[0]["status"].clone(),
            own[0]["error_code"].clone(),
            own[0]["row_count"].clone()
        ),
        (
            json!(failing),
            json!("failed"),
            json!("query_column_unknown"),
            Value::Null
        )
    );
    assert_eq!(
        (
            own[1]["task_id"].clone(),
            own[1]["status"].clone(),
            own[1]["row_count"].clone(),
            own[1]["sql_sha256"].clone(),
            own[1]["admitted_sql"].clone()
        ),
        (
            json!(task),
            json!("completed"),
            json!(4),
            good["sha256"].clone(),
            good["admitted_sql"].clone()
        )
    );
    let mut keys: Vec<&str> = own[1]
        .as_object()
        .unwrap()
        .keys()
        .map(String::as_str)
        .collect();
    keys.sort_unstable();
    assert_eq!(
        keys,
        [
            "admitted_sql",
            "error_code",
            "row_count",
            "sql_sha256",
            "status",
            "task_id"
        ]
    );
    assert_eq!(
        coordinator
            .research_run_own_queries(run, "lead-c", again_generation, &[task])
            .await
            .unwrap_err()
            .code,
        "lease_fence_lost"
    );
    assert!(
        coordinator
            .research_run_own_queries(run, "lead-a", again_generation, &[task; 65])
            .await
            .is_err()
    );
    // An analysis a person rejected is no longer offered to the later run.
    requeue(run, later_run).await;
    let later_again = coordinator
        .claim_research_run("lead-c", 90)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(later_again["run"]["run_id"], json!(later_run));
    let later_generation = later_again["run"]["generation"].as_i64().unwrap();
    // No other project's queries read as a run's own.
    assert_eq!(
        coordinator
            .research_run_own_queries(later_run, "lead-c", later_generation, &[task, failing])
            .await
            .unwrap(),
        json!([])
    );
    assert_eq!(
        coordinator
            .research_run_prior_queries(later_run, "lead-c", later_generation, &asked)
            .await
            .unwrap()
            .as_array()
            .unwrap()
            .len(),
        1
    );
    api.record_task_review(a, task, "rejected", Some("Wrong nutrient."), Uuid::new_v4())
        .await
        .unwrap();
    assert_eq!(
        coordinator
            .research_run_prior_queries(later_run, "lead-c", later_generation, &asked)
            .await
            .unwrap(),
        json!([])
    );

    // A live lab source (migration 053): no snapshot to confirm, so the worker reads the one row the lab's
    // database states about its version in the result's own transaction and keeps it with the receipt and the lineage.
    owner
        .execute(
            "UPDATE enrichedlab_agent.broker_research_runs SET status='paused', lease_owner=NULL, lease_until=NULL
             WHERE run_id=$1",
            &[&later_run],
        )
        .await
        .unwrap();
    owner.batch_execute("
        CREATE SCHEMA lab_qual;
        CREATE TABLE lab_qual.sample (sample_id integer PRIMARY KEY, weight_g double precision NOT NULL);
        INSERT INTO lab_qual.sample VALUES (1, 20.5), (2, 22.1), (3, 19.8);
        CREATE FUNCTION lab_qual.round(bigint) RETURNS SETOF bigint LANGUAGE sql IMMUTABLE
          AS $$ SELECT $1 UNION ALL SELECT $1 + 1 $$;
        CREATE VIEW lab_qual.vw_database_version AS
          SELECT '0021'::text AS schema_version, 7 AS newest_import_id;
        CREATE ROLE lab_reader_qual LOGIN;
        GRANT USAGE ON SCHEMA lab_qual TO lab_reader_qual;
        GRANT SELECT ON lab_qual.sample, lab_qual.vw_database_version TO lab_reader_qual;
        GRANT EXECUTE ON FUNCTION lab_qual.round(bigint) TO lab_reader_qual;
        ALTER ROLE lab_reader_qual SET default_transaction_read_only = on;
        ALTER ROLE lab_reader_qual SET standard_conforming_strings = off;
        INSERT INTO enrichedlab_agent.broker_agent_profiles(tenant_id,profile_key,name,description,harness,thinking,route,
          controller_profile,gateway_profile,image_ref,cpu_limit,memory_limit,state_profile,role,tool_bundle,max_run_seconds,
          max_task_attempts,max_provider_attempts)
        SELECT tenant_id,'lab-sql','Query the example lab database',description,harness,thinking,'server-query',
          controller_profile,gateway_profile,image_ref,cpu_limit,memory_limit,state_profile,role,tool_bundle,max_run_seconds,1,
          max_provider_attempts
        FROM enrichedlab_agent.broker_agent_profiles WHERE tenant_id='research-site' AND profile_key='general-research';
        INSERT INTO enrichedlab_agent.broker_group_profile_grants(tenant_id,group_key,profile_key)
          VALUES('research-site','pilot-lab','lab-sql');").await.unwrap();
    let declaration = hex::encode(Sha256::digest(b"lab-weights declaration"));
    let lab_source = |version_relation: &str, listed: &[&str], class: Option<&str>| {
        let mut value = json!({"source": "lab-weights", "profile": "lab-sql", "title": "Example lab weights",
            "description": "Invented sample weights in grams.", "release": "Live lab database",
            "schema": "lab_qual",
            "relations": listed.iter().map(|name| json!({"name": name, "columns": [{"name": "sample_id", "type": "integer"},
                {"name": "weight_g", "type": "double precision"}]})).collect::<Vec<_>>(),
            "functions": ["count", "max", "round"], "row_cap": 100, "statement_timeout_ms": 5000,
            "connection_ref": "lab-qual",
            "snapshot": {"kind": "live", "version_relation": version_relation, "declaration_sha256": declaration}});
        if let Some(class) = class {
            value["data_class"] = json!(class);
        }
        value
    };
    // The version relation is the worker's to read, not one the lead queries; a live snapshot is a lab database's alone
    // and names no manifest, so a worker that only knows manifests never reads it; no other class exists.
    let refused = |r: Result<tokio_postgres::Row, tokio_postgres::Error>| {
        let e = r.unwrap_err();
        e.as_db_error()
            .map(|d| d.message().to_owned())
            .unwrap_or_default()
    };
    assert_eq!(
        refused(
            register(lab_source(
                "vw_database_version",
                &["sample", "vw_database_version"],
                Some("lab_database")
            ))
            .await
        ),
        "broker:sql_source_invalid"
    );
    assert_eq!(
        refused(
            register(lab_source(
                "vw_database_version",
                &["sample"],
                Some("other")
            ))
            .await
        ),
        "broker:sql_source_invalid"
    );
    let check =
        |r: Result<tokio_postgres::Row, tokio_postgres::Error>| r.unwrap_err().code().cloned();
    assert_eq!(
        check(register(lab_source("vw_database_version", &["sample"], None)).await),
        Some(tokio_postgres::error::SqlState::CHECK_VIOLATION),
        "a live snapshot is not public reference data"
    );
    let mut pinned = lab_source("vw_database_version", &["sample"], Some("lab_database"));
    pinned["snapshot"] = json!({"manifest_sha256": declaration});
    assert_eq!(
        check(register(pinned).await),
        Some(tokio_postgres::error::SqlState::CHECK_VIOLATION),
        "a lab database's snapshot is live"
    );
    let mut with_manifest = lab_source("vw_database_version", &["sample"], Some("lab_database"));
    with_manifest["snapshot"]["manifest_sha256"] = json!(declaration);
    assert_eq!(
        check(register(with_manifest).await),
        Some(tokio_postgres::error::SqlState::CHECK_VIOLATION),
        "a live snapshot names no manifest an older worker could confirm"
    );
    register(lab_source(
        "vw_database_version",
        &["sample"],
        Some("lab_database"),
    ))
    .await
    .unwrap();
    register(lab_source(
        "vw_database_version",
        &["sample"],
        Some("lab_database"),
    ))
    .await
    .unwrap();
    let class: String = owner
        .query_one(
            "SELECT data_class FROM enrichedlab_agent.broker_sql_sources WHERE source_key='lab-weights'",
            &[],
        )
        .await
        .unwrap()
        .get(0);
    assert_eq!(class, "lab_database");
    // Migration 062: a lab database declares every relation its reader may read, up to 128 as the
    // onboarding tool accepts; migration 038's 64 refused a lab database's 86.
    owner.batch_execute("
        INSERT INTO enrichedlab_agent.broker_agent_profiles(tenant_id,profile_key,name,description,harness,thinking,route,
          controller_profile,gateway_profile,image_ref,cpu_limit,memory_limit,state_profile,role,tool_bundle,max_run_seconds,
          max_task_attempts,max_provider_attempts)
        SELECT tenant_id,'lab-sql-wide','Query a wide example lab database',description,harness,thinking,'server-query',
          controller_profile,gateway_profile,image_ref,cpu_limit,memory_limit,state_profile,role,tool_bundle,max_run_seconds,1,
          max_provider_attempts
        FROM enrichedlab_agent.broker_agent_profiles WHERE tenant_id='research-site' AND profile_key='general-research';")
        .await
        .unwrap();
    let wide = |count: usize| {
        let names: Vec<String> = (0..count).map(|n| format!("measure_{n:03}")).collect();
        let names: Vec<&str> = names.iter().map(String::as_str).collect();
        let mut value = lab_source("vw_database_version", &names, Some("lab_database"));
        value["source"] = json!("lab-wide");
        value["profile"] = json!("lab-sql-wide");
        value
    };
    assert_eq!(
        check(register(wide(129)).await),
        Some(tokio_postgres::error::SqlState::CHECK_VIOLATION),
        "a source lists at most 128 relations"
    );
    register(wide(128)).await.unwrap();
    let listed: i32 = owner
        .query_one(
            "SELECT jsonb_array_length(relations) FROM enrichedlab_agent.broker_sql_sources WHERE source_key='lab-wide'",
            &[],
        )
        .await
        .unwrap()
        .get(0);
    assert_eq!(listed, 128);
    // Its schema card is keyed by the declaration digest and reaches the run like a public source's.
    let (mut lab_reader, lab_reader_connection) = dsn
        .replace("user=postgres", "user=lab_reader_qual")
        .parse::<Config>()
        .unwrap()
        .connect(NoTls)
        .await
        .unwrap();
    let lab_reader_connection = tokio::spawn(lab_reader_connection);
    let lab_card_source: CardSource = serde_json::from_value(lab_source(
        "vw_database_version",
        &["sample"],
        Some("lab_database"),
    ))
    .unwrap();
    let lab_spec: CardSpec =
        serde_json::from_value(json!({"source": "lab-weights", "values": ["sample.sample_id"]}))
            .unwrap();
    let (lab_card, measured) =
        source_card::build_live(&mut lab_reader, &lab_card_source, &lab_spec)
            .await
            .unwrap();
    assert_eq!(lab_card["manifest_sha256"], declaration.as_str());
    assert_eq!(measured.unwrap()["schema_version"], "0021");
    owner
        .query_one(
            "SELECT enrichedlab_agent.broker_register_sql_source_card('research-site',$1)",
            &[&lab_card],
        )
        .await
        .unwrap();
    drop(lab_reader);
    lab_reader_connection.await.unwrap().unwrap();
    fs::write(
        root.join("connections").join("lab-qual.dsn"),
        dsn.replace("user=postgres", "user=lab_reader_qual"),
    )
    .unwrap();

    let lab_project = Uuid::new_v4();
    api.create_project(
        a,
        &CreateProject {
            project_id: lab_project,
            group: "pilot-lab".into(),
            title: "Lab weights".into(),
            visibility: Visibility::Private,
        },
    )
    .await
    .unwrap();
    let mut lab_state = research();
    lab_state.plan = [
        "rank-samples",
        "count-samples",
        "round-count-samples",
        "two-versions",
        "large-version",
        "receipt-retry",
        "cancelled",
        "new-version",
    ]
    .iter()
    .map(|key| {
        serde_json::from_value(
            json!({"action_key": key, "title": key, "status": "pending", "task_id": null,
                "evidence_refs": [], "rationale": "Read the lab's weights."}),
        )
        .unwrap()
    })
    .collect();
    api.checkpoint_project(
        a,
        lab_project,
        &CheckpointRequest {
            expected_revision: 0,
            request_id: Uuid::new_v4(),
            state: lab_state.clone(),
        },
    )
    .await
    .unwrap();
    let lab_run = api
        .start_research_run(
            a,
            lab_project,
            &StartRun {
                run_id: Uuid::new_v4(),
                expected_revision: 1,
                allowed_profiles: vec!["lab-sql".into()],
                max_turns: 4,
                max_parallel: 8,
            },
        )
        .await
        .unwrap();
    let lab_claim = coordinator
        .claim_research_run("lead-a", 90)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(lab_claim["run"]["run_id"], json!(lab_run));
    let lab_generation = lab_claim["run"]["generation"].as_i64().unwrap();
    let lab_sources = coordinator
        .research_run_sql_sources(lab_run, "lead-a", lab_generation)
        .await
        .unwrap();
    assert_eq!(
        lab_sources[0]["card"], lab_card,
        "the card keyed by the declaration reaches the run"
    );
    let lab_query = |sql: &str| {
        let relations: BTreeSet<String> = ["sample".to_string()].into();
        let functions: BTreeSet<String> = ["count", "max", "round"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        let admitted = sql_guard::admit(
            sql,
            &Allowed {
                schema: "lab_qual",
                relations: &relations,
                functions: &functions,
                row_cap: 100,
            },
        )
        .unwrap();
        json!({"source": "lab-weights", "proposed_sql": sql, "admitted_sql": admitted.sql,
            "sha256": format!("sha256:{}", hex::encode(Sha256::digest(admitted.sql.as_bytes()))),
            "guard": sql_guard::GUARD, "relations": admitted.relations, "row_limit": admitted.limit})
    };
    let reviewed_lab_query =
        reviewed_queries::lab_query(&owner, &coordinator, lab_run, lab_generation).await;
    let lab_actions: Value = Value::Array(
        [
            "rank-samples",
            "count-samples",
            "round-count-samples",
            "two-versions",
            "large-version",
            "receipt-retry",
            "cancelled",
            "new-version",
        ]
        .iter()
        .map(|key| {
            let sql = match *key {
                // The guard reads '\' as one backslash; the role's default (off) would read it as an escaped quote, so
                // this query completes only because the worker pins standard_conforming_strings for its transaction.
                "count-samples" => "SELECT count(*) FROM sample WHERE '\\' <> 'x'",
                "round-count-samples" => "SELECT round(count(*)) FROM sample",
                _ => "SELECT sample_id, weight_g FROM sample ORDER BY weight_g DESC",
            };
            let query = if *key == "rank-samples" {
                reviewed_lab_query.clone()
            } else {
                lab_query(sql)
            };
            let mut a = action(key, Some(query))[0].clone();
            a["profile"] = json!("lab-sql");
            a
        })
        .collect(),
    );
    let committed = coordinator
        .commit_research_turn(
            lab_run,
            "lead-a",
            lab_generation,
            1,
            &serde_json::to_value(&lab_state).unwrap(),
            &lab_actions,
            &record,
            "delegate",
        )
        .await
        .unwrap();
    let key_of: std::collections::HashMap<Uuid, String> = committed["tasks"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| {
            (
                serde_json::from_value(t["task_id"].clone()).unwrap(),
                t["action_key"].as_str().unwrap().to_owned(),
            )
        })
        .collect();
    assert_eq!(key_of.len(), 8);
    let version_view =
        |rows: &str| format!("CREATE OR REPLACE VIEW lab_qual.vw_database_version AS {rows}");
    let one_row = version_view("SELECT '0021'::text AS schema_version, 7 AS newest_import_id");
    let outcome = |task: Uuid| {
        let owner = &owner;
        async move {
            let row = owner
                .query_one(
                    "SELECT t.status, coalesce(t.error_code,''), r.status, r.source_version
                     FROM enrichedlab_agent.broker_tasks t JOIN enrichedlab_agent.broker_query_receipts r USING (tenant_id,task_id)
                     WHERE t.task_id=$1",
                    &[&task],
                )
                .await
                .unwrap();
            (
                row.get::<_, String>(0),
                row.get::<_, String>(1),
                row.get::<_, String>(2),
                row.get::<_, Option<Value>>(3),
            )
        }
    };
    // Each claimed task meets the version relation its case needs, whatever order the worker claims them in.
    for _ in 0..8 {
        let claimed = worker
            .claim_task("query-worker", 120)
            .await
            .unwrap()
            .unwrap();
        match key_of[&claimed.id].as_str() {
            "rank-samples" => {
                owner.batch_execute(&one_row).await.unwrap();
                // Before it runs, a completed receipt without a version is refused, through the worker's call and the one
                // an older worker makes.
                let unversioned = enrichedlab_agent_sandbox::broker::store::QueryReceipt {
                    status: "completed",
                    error_code: None,
                    row_count: Some(3),
                    columns: Some(json!([])),
                    package_sha256: Some("a".repeat(64)),
                    server_version: Some("18".into()),
                    started_at: time::OffsetDateTime::now_utc(),
                    finished_at: time::OffsetDateTime::now_utc(),
                    source_version: None,
                };
                assert!(
                    worker
                        .record_query_receipt(
                            claimed.id,
                            "query-worker",
                            claimed.attempt,
                            claimed.lease_generation,
                            &unversioned
                        )
                        .await
                        .is_err()
                );
                assert_eq!(
                    refused(
                        owner
                            .query_one(
                                "SELECT enrichedlab_agent.broker_record_query_receipt('research-site',$1,'query-worker',$2,$3,
                                   'completed',NULL,3,'[]'::jsonb,repeat('a',64),'18',clock_timestamp(),clock_timestamp())",
                                &[&claimed.id, &claimed.attempt, &claimed.lease_generation],
                            )
                            .await
                    ),
                    "broker:query_receipt_version_invalid"
                );
                query_run::execute(
                    worker.clone(),
                    &config,
                    claimed.clone(),
                    CancellationToken::new(),
                )
                .await
                .unwrap();
                let (status, _, receipt, version) = outcome(claimed.id).await;
                assert_eq!(
                    (status.as_str(), receipt.as_str()),
                    ("completed", "completed")
                );
                let version = version.unwrap();
                assert_eq!(
                    (
                        version["schema_version"].clone(),
                        version["newest_import_id"].clone()
                    ),
                    (json!("0021"), json!(7))
                );
                let detail = api.task_query_detail(a, claimed.id).await.unwrap();
                assert_eq!(detail["data_class"], "lab_database");
                assert_eq!(detail["receipts"][0]["source_version"], version);
                let lineage = api.task_lineage(a, claimed.id).await.unwrap();
                assert_eq!(lineage["query"]["data_class"], "lab_database");
                assert_eq!(lineage["method"]["tool_class"], "reviewed_query");
                assert_eq!(detail["row_limit"], 3);
                assert_eq!(
                    detail["reviewed_query"],
                    reviewed_lab_query["reviewed_query"]
                );
                assert_eq!(lineage["query"]["snapshot_kind"], "live");
                assert_eq!(lineage["query"]["source_version"], version);
                assert_eq!(lineage["inputs"]["classes"], json!(["lab_database"]));
            }
            "count-samples" => {
                // The operator measures the card again at the version the lab's data is at: the query reading that
                // version starts no refresh.
                let current = json!({"schema_version": "0024", "newest_import_id": 10});
                owner
                    .batch_execute(&version_view(
                        "SELECT '0024'::text AS schema_version, 10 AS newest_import_id",
                    ))
                    .await
                    .unwrap();
                let (mut reader, reader_connection) = dsn
                    .replace("user=postgres", "user=lab_reader_qual")
                    .parse::<Config>()
                    .unwrap()
                    .connect(NoTls)
                    .await
                    .unwrap();
                let reader_connection = tokio::spawn(reader_connection);
                let (card, _) = source_card::build_live(&mut reader, &lab_card_source, &lab_spec)
                    .await
                    .unwrap();
                drop(reader);
                reader_connection.await.unwrap().unwrap();
                assert_eq!(card["measured_at"], current);
                owner
                    .query_one(
                        "SELECT enrichedlab_agent.broker_register_sql_source_card('research-site',$1)",
                        &[&card],
                    )
                    .await
                    .unwrap();
                query_run::execute(
                    worker.clone(),
                    &config,
                    claimed.clone(),
                    CancellationToken::new(),
                )
                .await
                .unwrap();
                let (status, code, receipt, version) = outcome(claimed.id).await;
                assert_eq!(
                    (status.as_str(), code.as_str(), receipt.as_str()),
                    ("completed", "", "completed")
                );
                assert_eq!(version, Some(current.clone()));
                let started: i64 = owner
                    .query_one(
                        "SELECT count(*) FROM enrichedlab_agent.broker_sql_source_card_refreshes
                         WHERE source_key = 'lab-weights' AND source_version = $1",
                        &[&current],
                    )
                    .await
                    .unwrap()
                    .get(0);
                assert_eq!(started, 0);
                let detail = api.task_query_detail(a, claimed.id).await.unwrap();
                assert_eq!(detail["receipts"][0]["row_count"], 1);
                assert!(
                    api.list_artifacts(a, claimed.id)
                        .await
                        .unwrap()
                        .iter()
                        .any(|artifact| artifact.role == "result")
                );
                owner.batch_execute(&one_row).await.unwrap();
            }
            "round-count-samples" => {
                // count(*) is bigint, so the source's exact round(bigint) overload wins over pg_catalog's numeric
                // and float8 forms. Static admission allows the name; the worker must refuse its two-row result.
                owner.batch_execute(&one_row).await.unwrap();
                query_run::execute(
                    worker.clone(),
                    &config,
                    claimed.clone(),
                    CancellationToken::new(),
                )
                .await
                .unwrap();
                assert_eq!(
                    outcome(claimed.id).await,
                    (
                        "failed".into(),
                        "query_not_single_row".into(),
                        "refused".into(),
                        None
                    )
                );
                let receipt = owner
                    .query_one(
                        "SELECT error_code, row_count IS NULL AND columns IS NULL AND package_sha256 IS NULL
                           AND server_version IS NULL AND source_version IS NULL
                         FROM enrichedlab_agent.broker_query_receipts WHERE task_id=$1",
                        &[&claimed.id],
                    )
                    .await
                    .unwrap();
                assert_eq!(receipt.get::<_, String>(0), "query_not_single_row");
                assert!(receipt.get::<_, bool>(1));
                assert!(api.list_artifacts(a, claimed.id).await.unwrap().is_empty());
                let digests = owner
                    .query_one(
                        "SELECT count(*) FROM enrichedlab_agent.broker_artifact_digests WHERE task_id=$1",
                        &[&claimed.id],
                    )
                    .await
                    .unwrap();
                assert_eq!(digests.get::<_, i64>(0), 0);
                assert!(
                    !config
                        .artifact_root
                        .join("tasks")
                        .join(claimed.id.to_string())
                        .exists()
                );
            }
            "new-version" => {
                // An import moves the lab's version row; the query completes at the new version and the worker measures
                // the card again once for it, with the newest card's spec, as the source's reader (migration 082).
                let new = json!({"schema_version": "0022", "newest_import_id": 8});
                owner
                    .batch_execute(&version_view(
                        "SELECT '0022'::text AS schema_version, 8 AS newest_import_id",
                    ))
                    .await
                    .unwrap();
                query_run::execute(
                    worker.clone(),
                    &config,
                    claimed.clone(),
                    CancellationToken::new(),
                )
                .await
                .unwrap();
                let (status, _, receipt, version) = outcome(claimed.id).await;
                assert_eq!(
                    (status.as_str(), receipt.as_str(), version),
                    ("completed", "completed", Some(new.clone()))
                );
                let refresh = owner
                    .query_one(
                        "SELECT state, coalesce(code, ''), card_sha256 FROM enrichedlab_agent.broker_sql_source_card_refreshes
                         WHERE source_key = 'lab-weights' AND source_version = $1",
                        &[&new],
                    )
                    .await
                    .unwrap();
                assert_eq!(
                    (refresh.get::<_, String>(0), refresh.get::<_, String>(1)),
                    ("registered".to_owned(), String::new())
                );
                let card = owner
                    .query_one(
                        "SELECT card_sha256, source_version, card->'measured_at', spec = card->'spec'
                         FROM enrichedlab_agent.broker_sql_source_cards WHERE source_key = 'lab-weights'
                         ORDER BY created_at DESC, card_sha256 LIMIT 1",
                        &[],
                    )
                    .await
                    .unwrap();
                assert_eq!(card.get::<_, String>(0), refresh.get::<_, String>(2));
                assert_eq!(card.get::<_, Value>(1), new);
                assert_eq!(card.get::<_, Value>(2), new);
                assert!(card.get::<_, bool>(3));
                // Once per version: the same version starts nothing again, and a version no completed query read is
                // refused.
                let begin = |version: Value| {
                    let owner = &owner;
                    async move {
                        owner
                            .query_one(
                                "SELECT enrichedlab_agent.broker_begin_sql_source_card_refresh('research-site', 'lab-weights', $1, 'other-worker')",
                                &[&version],
                            )
                            .await
                            .map(|row| row.get::<_, Option<Value>>(0))
                    }
                };
                assert_eq!(begin(new.clone()).await.unwrap(), None);
                assert!(begin(json!({"schema_version": "0099"})).await.is_err());
                // Finish holds a card to the spec its refresh handed out and kept, not to the newest card's: a running
                // refresh started with another spec refuses a card measured with the newest one.
                let other = json!({"schema_version": "0023", "newest_import_id": 9});
                owner
                    .execute(
                        "INSERT INTO enrichedlab_agent.broker_sql_source_card_refreshes (tenant_id, source_key,
                           version_sha256, source_version, state, spec, worker)
                         VALUES ('research-site', 'lab-weights', encode(sha256(convert_to($1::jsonb::text, 'UTF8')), 'hex'),
                           $1, 'running', '{\"source\": \"lab-weights\", \"values\": []}'::jsonb, 'spec-worker')",
                        &[&other],
                    )
                    .await
                    .unwrap();
                let mut crafted: Value = owner
                    .query_one(
                        "SELECT card FROM enrichedlab_agent.broker_sql_source_cards WHERE source_key = 'lab-weights'
                         ORDER BY created_at DESC, card_sha256 LIMIT 1",
                        &[],
                    )
                    .await
                    .unwrap()
                    .get(0);
                crafted["measured_at"] = other.clone();
                let finished: Value = owner
                    .query_one(
                        "SELECT enrichedlab_agent.broker_finish_sql_source_card_refresh('research-site', 'lab-weights',
                           encode(sha256(convert_to($1::jsonb::text, 'UTF8')), 'hex'), 'spec-worker', $2, NULL)",
                        &[&other, &crafted],
                    )
                    .await
                    .unwrap()
                    .get(0);
                assert_eq!(
                    finished,
                    json!({"state": "refused", "code": "card_spec_changed"})
                );
                // ...and to the version it started for: a card measured at another version is refused.
                let started_for = json!({"schema_version": "0025", "newest_import_id": 11});
                owner
                    .execute(
                        "INSERT INTO enrichedlab_agent.broker_sql_source_card_refreshes (tenant_id, source_key,
                           version_sha256, source_version, state, spec, worker)
                         VALUES ('research-site', 'lab-weights', encode(sha256(convert_to($1::jsonb::text, 'UTF8')), 'hex'),
                           $1, 'running', $2, 'version-worker')",
                        &[&started_for, &crafted["spec"]],
                    )
                    .await
                    .unwrap();
                let finished: Value = owner
                    .query_one(
                        "SELECT enrichedlab_agent.broker_finish_sql_source_card_refresh('research-site', 'lab-weights',
                           encode(sha256(convert_to($1::jsonb::text, 'UTF8')), 'hex'), 'version-worker', $2, NULL)",
                        &[&started_for, &crafted],
                    )
                    .await
                    .unwrap()
                    .get(0);
                assert_eq!(
                    finished,
                    json!({"state": "refused", "code": "version_moved"})
                );
                // A live source's card names its version and spec, or the operator path refuses it too.
                for (field, code) in [
                    ("measured_at", "broker:sql_source_card_version_required"),
                    ("spec", "broker:sql_source_card_spec_required"),
                ] {
                    let mut without = crafted.clone();
                    without.as_object_mut().unwrap().remove(field);
                    assert_eq!(
                        refused(
                            owner
                                .query_one(
                                    "SELECT enrichedlab_agent.broker_register_sql_source_card('research-site',$1)",
                                    &[&without],
                                )
                                .await
                        ),
                        code
                    );
                }
                owner.batch_execute(&one_row).await.unwrap();
            }
            "two-versions" => {
                owner
                    .batch_execute(&version_view(
                        "SELECT '0021'::text AS schema_version, 7 AS newest_import_id UNION ALL SELECT '0022'::text, 8",
                    ))
                    .await
                    .unwrap();
                query_run::execute(
                    worker.clone(),
                    &config,
                    claimed.clone(),
                    CancellationToken::new(),
                )
                .await
                .unwrap();
                assert_eq!(
                    outcome(claimed.id).await,
                    (
                        "failed".into(),
                        "source_version_unreadable".into(),
                        "refused".into(),
                        None
                    )
                );
            }
            "large-version" => {
                // The database withholds a version row over 2 KiB; the worker never receives it.
                owner
                    .batch_execute(&version_view(
                        "SELECT repeat('x', 5000) AS schema_version, 7 AS newest_import_id",
                    ))
                    .await
                    .unwrap();
                query_run::execute(
                    worker.clone(),
                    &config,
                    claimed.clone(),
                    CancellationToken::new(),
                )
                .await
                .unwrap();
                assert_eq!(
                    outcome(claimed.id).await,
                    (
                        "failed".into(),
                        "source_version_too_large".into(),
                        "refused".into(),
                        None
                    )
                );
            }
            "cancelled" => {
                // A query cancelled while its lease is held records its receipt and ends cancelled: the receipt's fence
                // takes a cancelling lease (migration 053).
                assert!(
                    worker
                        .task_query(
                            claimed.id,
                            "query-worker",
                            claimed.attempt,
                            claimed.lease_generation
                        )
                        .await
                        .unwrap()
                        .is_some()
                );
                assert_eq!(api.cancel_task(a, claimed.id).await.unwrap(), "cancelling");
                let at = time::OffsetDateTime::now_utc();
                let cancelled = enrichedlab_agent_sandbox::broker::store::QueryReceipt {
                    status: "failed",
                    error_code: Some("cancelled".into()),
                    row_count: None,
                    columns: None,
                    package_sha256: None,
                    server_version: None,
                    started_at: at,
                    finished_at: at,
                    source_version: None,
                };
                worker
                    .record_query_receipt(
                        claimed.id,
                        "query-worker",
                        claimed.attempt,
                        claimed.lease_generation,
                        &cancelled,
                    )
                    .await
                    .unwrap();
                assert!(
                    worker
                        .finish_task(
                            claimed.id,
                            "query-worker",
                            claimed.lease_generation,
                            false,
                            "query_not_completed",
                            "cancelled"
                        )
                        .await
                        .unwrap()
                );
                assert_eq!(
                    outcome(claimed.id).await,
                    (
                        "cancelled".into(),
                        "cancelled".into(),
                        "failed".into(),
                        None
                    )
                );
            }
            "receipt-retry" => {
                // The same receipt again is accepted; a retry that differs in any recorded field is refused.
                let at = time::OffsetDateTime::now_utc();
                let first = enrichedlab_agent_sandbox::broker::store::QueryReceipt {
                    status: "refused",
                    error_code: Some("source_version_unreadable".into()),
                    row_count: None,
                    columns: None,
                    package_sha256: None,
                    server_version: None,
                    started_at: at,
                    finished_at: at,
                    source_version: None,
                };
                let record = |receipt: enrichedlab_agent_sandbox::broker::store::QueryReceipt| {
                    let worker = worker.clone();
                    let claimed = claimed.clone();
                    async move {
                        worker
                            .record_query_receipt(
                                claimed.id,
                                "query-worker",
                                claimed.attempt,
                                claimed.lease_generation,
                                &receipt,
                            )
                            .await
                    }
                };
                record(first.clone()).await.unwrap();
                record(first.clone()).await.unwrap();
                let mut later = first.clone();
                later.finished_at = at + time::Duration::seconds(1);
                assert!(
                    record(later).await.is_err(),
                    "a different finish time is not the same receipt"
                );
                let mut versioned = first.clone();
                versioned.server_version = Some("18".into());
                assert!(
                    record(versioned).await.is_err(),
                    "a different server version is not the same receipt"
                );
                worker
                    .finish_task(
                        claimed.id,
                        "query-worker",
                        claimed.lease_generation,
                        false,
                        "query_not_completed",
                        "source_version_unreadable",
                    )
                    .await
                    .unwrap();
            }
            other => panic!("unexpected task {other}"),
        }
    }
    owner.batch_execute(&one_row).await.unwrap();
    // The next turn's sources state the newest card's version and the version the newest completed query read.
    owner
        .execute(
            "UPDATE enrichedlab_agent.broker_research_runs SET available_at=clock_timestamp()-interval '1 second'
             WHERE run_id=$1",
            &[&lab_run],
        )
        .await
        .unwrap();
    let next = coordinator
        .claim_research_run("lead-a", 90)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(next["run"]["run_id"], json!(lab_run));
    let sources = coordinator
        .research_run_sql_sources(
            lab_run,
            "lead-a",
            next["run"]["generation"].as_i64().unwrap(),
        )
        .await
        .unwrap();
    let lab = sources
        .as_array()
        .unwrap()
        .iter()
        .find(|s| s["source"] == "lab-weights")
        .unwrap();
    let expected = owner
        .query_one(
            "SELECT (SELECT c.source_version FROM enrichedlab_agent.broker_sql_source_cards c
                     WHERE c.source_key = 'lab-weights' ORDER BY c.created_at DESC, c.card_sha256 LIMIT 1),
                    (SELECT q.source_version FROM enrichedlab_agent.broker_query_receipts q
                     JOIN enrichedlab_agent.broker_task_queries t USING (tenant_id, task_id)
                     WHERE t.source_key = 'lab-weights' AND q.status = 'completed'
                     ORDER BY q.finished_at DESC, q.recorded_at DESC LIMIT 1)",
            &[],
        )
        .await
        .unwrap();
    assert!(lab["source_version"].is_object() && lab["card_source_version"].is_object());
    assert_eq!(
        (
            lab["card_source_version"].clone(),
            lab["source_version"].clone()
        ),
        (expected.get::<_, Value>(0), expected.get::<_, Value>(1))
    );

    api.ready_for_review_api().await.unwrap();
    std::fs::remove_dir_all(&root).unwrap();
    drop(owner);
    connection.abort();
}
