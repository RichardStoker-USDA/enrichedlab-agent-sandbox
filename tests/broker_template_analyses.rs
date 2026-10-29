//! Template analyses (migration 058) against an isolated PostgreSQL and a stand-in render service: the
//! worker keeps the service's analysis catalog apart from its figure catalog, a person asks for an analysis of a
//! finished analysis's result table, the broker refuses what it can check, the worker's analysis route runs the template
//! from the retained table and keeps a package with every row at full precision and a receipt, a refusal finishes in
//! the broker's words for the service's refusal code (never the service's own words), an outdated service and a lost
//! lease are refused, readers, lineage and the project see the analysis, and its results can be the source of a figure
//! and of another analysis.
use axum::{
    Json, Router,
    http::StatusCode,
    routing::{get, post},
};
use base64::Engine;
use enrichedlab_agent_sandbox::broker::{
    analysis_run,
    config::WorkerConfig,
    coordinator::StartRun,
    figure_run,
    model::{PrincipalClass, SubmitTaskRequest, VerifiedIdentity},
    project::{CheckpointRequest, CreateProject, ResearchState, Visibility},
    query_run,
    sql_guard::{self, Allowed},
    store::{
        AnalysisReceipt, AnalysisRequest, FigureReceipt, FigureRequest, Store, StoreErrorKind,
    },
};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{collections::BTreeSet, env, fs, io::Read, path::PathBuf, time::Duration};
use time::OffsetDateTime;
use tokio_postgres::{Config, NoTls, config::Host};
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

const MANIFEST: &str = "5f1c0d9a3a4e2b7c8d6e5f4a3b2c1d0e9f8a7b6c5d4e3f2a1b0c9d8e7f6a5b4c";
const IMAGE: &str = "sha256:7f1b2c3d4e5f60718293a4b5c6d7e8f90112233445566778899aabbccddeeff0";
const SERVICE_WORDS: &str =
    "group_compare needs exactly 2 groups in 'description'; found 4: Example food A";
/// The stand-in tells the test when it holds an analysis titled "slow".
static SLOW_STARTED: tokio::sync::Notify = tokio::sync::Notify::const_new();

fn research() -> ResearchState {
    serde_json::from_value(json!({"schema_version":1,"goal":"Which example foods carry the most of the example nutrient?",
        "constraints":["Read only the reference data."],
        "plan":[{"action_key":"rank-foods","title":"Rank foods by the nutrient","status":"pending","task_id":null,
            "evidence_refs":[],"rationale":"Answer the question."}],
        "lessons":[],"next_action":"Query the reference data.",
        "workspace":{"schema_version":1,"profile":"reference-sql","image_digest":format!("sha256:{}","a".repeat(64)),
        "source_digest":format!("sha256:{}","b".repeat(64)),"input_refs":["reference://qualification/example"],
        "tool_refs":["tool://reference-sql"],"preparation_receipt":null}})).unwrap()
}

fn query_action(key: &str) -> Value {
    let sql = "SELECT f.description, n.amount FROM food f JOIN food_nutrient n USING (fdc_id) ORDER BY n.amount DESC NULLS LAST, f.description";
    let relations: BTreeSet<String> = ["food", "food_nutrient"]
        .iter()
        .map(|s| s.to_string())
        .collect();
    let functions: BTreeSet<String> = ["count"].iter().map(|s| s.to_string()).collect();
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
    json!({"action_key": key, "profile": "reference-sql", "objective": "Rank the foods.", "request_sha256": "c".repeat(64),
        "query": {"source": "example-reference", "proposed_sql": sql, "admitted_sql": a.sql,
            "sha256": format!("sha256:{}", hex::encode(Sha256::digest(a.sql.as_bytes()))), "guard": sql_guard::GUARD,
            "relations": a.relations, "row_limit": a.limit}})
}

/// A stand-in for the reviewed render service: a catalog with two figures and two analyses, and an analysis run that
/// answers by its settings: `test: wilcoxon` is refused with a code naming a column of the table, `test: kruskal` with
/// one naming a column the table does not have (both beside the service's own words, which must not be passed on), the
/// title "old service" answers as a release before 0.6.0, the title "slow" waits a minute before it answers, and anything
/// else completes.
async fn render_service() -> String {
    async fn features() -> Json<Value> {
        Json(
            json!({"service_version": "0.6.0", "image_digest": IMAGE, "features": [
                {"id": "scatter", "label": "Scatter plot", "kind": "figure", "description": "One point per row.",
                 "params": {"x": {"type": "column", "required": true}, "y": {"type": "column", "required": true}}},
                {"id": "bar", "label": "Bar chart", "kind": "figure",
                 "params": {"category": {"type": "column", "required": true}, "value": {"type": "column", "required": true}}},
                {"id": "group_compare", "label": "Two-group comparison", "kind": "analysis", "description": "A t or Wilcoxon test.",
                 "params": {"value": {"type": "column", "required": true}, "group": {"type": "column", "required": true},
                   "test": {"type": "enum", "choices": ["auto", "t", "wilcoxon", "kruskal"], "required": false}}},
                {"id": "summary_stats", "label": "Summary statistics", "kind": "analysis",
                 "params": {"value": {"type": "column", "required": true}, "group": {"type": "column", "required": false}}}
            ]}),
        )
    }
    async fn render(Json(body): Json<Value>) -> (StatusCode, Json<Value>) {
        if body["title"] == "slow" {
            SLOW_STARTED.notify_one();
            tokio::time::sleep(Duration::from_secs(60)).await;
        }
        let b64 = |bytes: &[u8]| base64::engine::general_purpose::STANDARD.encode(bytes);
        let refusal = match body["params"]["test"].as_str() {
            Some("wilcoxon") => Some(
                json!({"code": "group_count_not_two", "columns": ["description"], "counts": [4]}),
            ),
            Some("kruskal") => Some(
                json!({"code": "group_count_not_two", "columns": ["Example food A"], "counts": [4]}),
            ),
            _ => None,
        };
        if let Some(refusal) = refusal {
            return (
                StatusCode::OK,
                Json(json!({"error": SERVICE_WORDS, "refusal": refusal, "ms": 3})),
            );
        }
        let exact = json!({"columns": ["group", "n", "mean", "p_value"],
            "rows": [{"group": "Example food A", "n": 2, "mean": 40.25, "p_value": 6.12498179243933e-29},
                     {"group": "Example food B, raw", "n": 2, "mean": 22.3, "p_value": null}],
            "method": "Welch two-sample t-test\u{202E}", "notes": "Example food A vs Example food B, raw (value = amount)",
            "stats": {"p_value": 6.12498179243933e-29, "t_statistic": 25.123456789012, "test": "Welch"}, "n": 2});
        let mut answer = json!({
            "kind": "analysis",
            "results": {"columns": ["group", "n", "mean", "p_value"], "rows": [], "method": "Welch two-sample t-test",
                "notes": null, "stats": {"p_value": 6.12e-29}, "n": 2},
            "results_exact": exact,
            "png": b64(b"\x89PNG\r\n\x1a\nstand-in"), "svg": "<svg xmlns='http://www.w3.org/2000/svg'/>",
            "code": "source(file.path(dirname(.tf), \"_common.R\"))\n",
            "common_code": "WORK <- commandArgs(TRUE)[1]\n", "service_version": "0.6.0", "image_digest": IMAGE,
            "spec": {"params": body["params"], "title": body["title"], "feature_id": body["feature_id"]},
            "notes": ["'unused' was not used"]});
        // A service that does not report its image: the analysis completes, and its lineage must name the gap.
        if body["title"] == "no digest" {
            answer["image_digest"] = Value::Null;
        }
        if body["title"] == "old service" {
            answer.as_object_mut().unwrap().remove("results_exact");
        }
        (StatusCode::OK, Json(answer))
    }
    let app = Router::new()
        .route("/features", get(features))
        .route("/render", post(render));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    url
}

/// The members of a package, by name.
fn members(bytes: &[u8]) -> Vec<(String, Vec<u8>)> {
    let mut out = Vec::new();
    for entry in tar::Archive::new(flate2::read::GzDecoder::new(bytes))
        .entries()
        .unwrap()
    {
        let mut entry = entry.unwrap();
        let name = entry.path().unwrap().to_string_lossy().into_owned();
        let mut body = Vec::new();
        entry.read_to_end(&mut body).unwrap();
        out.push((name, body));
    }
    out
}

#[tokio::test]
#[ignore = "isolated PostgreSQL through qualify_remote_broker.sh template-analyses"]
async fn a_person_runs_a_template_analysis_on_a_finished_table() {
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
    // Each role is ready only with migration 058 and its own grants.
    api.ready_for_template_analyses(true).await.unwrap();
    worker.ready_for_template_analyses(false).await.unwrap();
    assert_eq!(
        api.ready_for_template_analyses(false)
            .await
            .unwrap_err()
            .code,
        "broker_schema_not_ready"
    );
    assert!(worker.ready_for_template_analyses(true).await.is_err());
    let request_fn = "enrichedlab_agent.broker_request_analysis(text,uuid,uuid,uuid,text,text,jsonb,text,uuid,uuid)";
    owner
        .batch_execute(&format!(
            "REVOKE EXECUTE ON FUNCTION {request_fn} FROM broker_api_qual"
        ))
        .await
        .unwrap();
    assert!(api.ready_for_template_analyses(true).await.is_err());
    owner
        .batch_execute(&format!(
            "GRANT EXECUTE ON FUNCTION {request_fn} TO broker_api_qual"
        ))
        .await
        .unwrap();
    api.ready_for_template_analyses(true).await.unwrap();

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

    // A finished analysis with a result table: the example reference data's query, run by the worker's query route.
    owner.batch_execute(&format!("
        CREATE SCHEMA reference_qual;
        CREATE TABLE reference_qual.food (fdc_id integer PRIMARY KEY, description text NOT NULL);
        CREATE TABLE reference_qual.food_nutrient (fdc_id integer NOT NULL, amount double precision);
        CREATE TABLE reference_qual.snapshot_manifest (manifest_sha256 text NOT NULL);
        INSERT INTO reference_qual.food VALUES (1,'Example food A'),(2,'Example food B, raw'),(3,'Example food C'),(4,'Example food D');
        INSERT INTO reference_qual.food_nutrient VALUES (1,48.2),(2,31.7),(3,22.5),(4,12.9);
        INSERT INTO reference_qual.snapshot_manifest VALUES ('{MANIFEST}');
        CREATE ROLE reference_reader_qual LOGIN;
        GRANT USAGE ON SCHEMA reference_qual TO reference_reader_qual;
        GRANT SELECT ON ALL TABLES IN SCHEMA reference_qual TO reference_reader_qual;
        INSERT INTO enrichedlab_agent.broker_agent_profiles(tenant_id,profile_key,name,description,harness,thinking,route,
          controller_profile,gateway_profile,image_ref,cpu_limit,memory_limit,state_profile,role,tool_bundle,max_run_seconds,
          max_task_attempts,max_provider_attempts)
        SELECT tenant_id,k,n,description,harness,thinking,r,controller_profile,gateway_profile,image_ref,cpu_limit,
          memory_limit,state_profile,role,tool_bundle,max_run_seconds,1,max_provider_attempts
        FROM enrichedlab_agent.broker_agent_profiles,
          (VALUES ('reference-sql','Query the example reference data','server-query'),
                  ('figure-render','Draw a figure from a result table','server-render'),
                  ('template-analysis','Run a template analysis on a result table','server-analysis')) v(k,n,r)
        WHERE tenant_id='research-site' AND profile_key='general-research';
        INSERT INTO enrichedlab_agent.broker_group_profile_grants(tenant_id,group_key,profile_key)
          VALUES('research-site','pilot-lab','reference-sql'),('research-site','pilot-lab','figure-render');")).await.unwrap();
    owner
        .query_one(
            "SELECT enrichedlab_agent.broker_register_sql_source('research-site',$1)",
            &[&json!({"source": "example-reference", "profile": "reference-sql", "title": "Example reference data",
                "description": "Invented foods and one invented nutrient's amount per 100 g.", "release": "Qualification release",
                "schema": "reference_qual",
                "relations": [{"name": "food", "row_count": 4, "columns": [{"name": "fdc_id", "type": "integer"}, {"name": "description", "type": "text"}]},
                              {"name": "food_nutrient", "row_count": 4, "columns": [{"name": "fdc_id", "type": "integer"}, {"name": "amount", "type": "double precision"}]}],
                "functions": ["count"], "row_cap": 100, "statement_timeout_ms": 5000,
                "connection_ref": "reference-qual", "snapshot": {"manifest_sha256": MANIFEST}})],
        )
        .await
        .unwrap();
    let project = Uuid::new_v4();
    api.create_project(
        a,
        &CreateProject {
            project_id: project,
            group: "pilot-lab".into(),
            title: "Template analyses".into(),
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
    let committed = coordinator
        .commit_research_turn(
            run,
            "lead-a",
            generation,
            1,
            &serde_json::to_value(research()).unwrap(),
            &json!([query_action("rank-foods")]),
            &json!({"schema_version":1,"test":"bounded metadata"}),
            "delegate",
        )
        .await
        .unwrap();
    let source: Uuid = serde_json::from_value(committed["tasks"][0]["task_id"].clone()).unwrap();

    let render_url = render_service().await;
    let root =
        std::env::temp_dir().join(format!("template_analyses_codetestfile_{}", Uuid::new_v4()));
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
        worker_id: "analysis-worker".into(),
        direct_dispatch: None,
        query_connection_dir: Some(connections),
        render_url: Some(render_url),
        dataset_fetcher_config: None,
        live_output: false,
        max_parallel: 1,
        lease_seconds: 120,
        poll_interval: Duration::from_millis(200),
        database_pool: 2,
        max_workspace_files: 10,
        max_workspace_bytes: 1024,
    };
    let claimed = worker
        .claim_task("analysis-worker", 120)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        (claimed.id, claimed.route.as_str()),
        (source, query_run::ROUTE)
    );
    query_run::execute(worker.clone(), &config, claimed, CancellationToken::new())
        .await
        .unwrap();

    let table = "results/query_result.csv";
    let ask = |who: Uuid,
               src: Uuid,
               table: &'static str,
               feature: &'static str,
               settings: Value,
               title: &'static str,
               previous: Option<Uuid>,
               request_id: Uuid| {
        let api = api.clone();
        async move {
            api.request_analysis(
                who,
                &AnalysisRequest {
                    project,
                    source_task: src,
                    table_path: table,
                    feature,
                    settings: &settings,
                    title,
                    previous,
                    request_id,
                },
            )
            .await
        }
    };
    let good = || json!({"value": "amount", "group": "description", "test": "t"});
    // Without a template-analysis method for the lab nothing is requested.
    let refusal = ask(
        a,
        source,
        table,
        "group_compare",
        good(),
        "",
        None,
        Uuid::new_v4(),
    )
    .await
    .unwrap_err();
    assert_eq!(
        (refusal.kind, refusal.code),
        (StoreErrorKind::Forbidden, "analysis_method_unavailable")
    );
    owner.batch_execute("INSERT INTO enrichedlab_agent.broker_group_profile_grants(tenant_id,group_key,profile_key)
        VALUES('research-site','pilot-lab','template-analysis');").await.unwrap();
    // The method runs only from a request: it is not offered and a person cannot run it directly. Since migration 060 a
    // research run may list it (its lead chooses analyses, `broker_lead_analyses`); a figure method stays request-only.
    let me = api.me(a).await.unwrap();
    assert_eq!(
        me.profiles
            .iter()
            .find(|p| p.key == "template-analysis")
            .map(|p| p.offered),
        Some(false)
    );
    let digest = "0".repeat(64);
    let direct = api
        .submit_task(
            a,
            "pilot-lab",
            "template-analysis",
            "Run an analysis",
            "direct-analysis",
            &digest,
        )
        .await
        .unwrap_err();
    assert_eq!(
        (direct.kind, direct.code),
        (StoreErrorKind::Forbidden, "profile_forbidden")
    );
    let in_project = api
        .submit_project_task(
            a,
            &SubmitTaskRequest {
                group: "pilot-lab".into(),
                profile: "template-analysis".into(),
                objective: "Run an analysis".into(),
                project_id: Some(project),
                action_key: Some("direct-analysis".into()),
                parent_task_id: None,
            },
            "direct-analysis-project",
            &digest,
        )
        .await
        .unwrap_err();
    assert_eq!(in_project.code, "profile_forbidden");
    let given = api
        .start_research_run(
            a,
            project,
            &StartRun {
                run_id: Uuid::new_v4(),
                expected_revision: 1,
                allowed_profiles: vec!["reference-sql".into(), "figure-render".into()],
                max_turns: 2,
                max_parallel: 1,
            },
        )
        .await
        .unwrap_err();
    assert_eq!(given.code, "profile_forbidden");
    assert_eq!(
        ask(
            a,
            source,
            table,
            "group_compare",
            good(),
            "",
            None,
            Uuid::new_v4()
        )
        .await
        .unwrap_err()
        .code,
        "analysis_catalog_unavailable"
    );

    // The worker keeps the analysis features apart from the figure features.
    let analysis_digest = analysis_run::refresh_catalog(&worker, &config)
        .await
        .unwrap();
    assert_eq!(
        analysis_run::refresh_catalog(&worker, &config)
            .await
            .unwrap(),
        analysis_digest
    );
    figure_run::refresh_catalog(&worker, &config).await.unwrap();
    let catalog = api.analysis_catalog(a).await.unwrap();
    assert_eq!(catalog["catalog_sha256"], analysis_digest);
    assert_eq!(catalog["service_version"], "0.6.0");
    assert_eq!(catalog["groups"], json!(["pilot-lab"]));
    let ids = |catalog: &Value| -> Vec<String> {
        catalog["features"]
            .as_array()
            .unwrap()
            .iter()
            .map(|f| f["id"].as_str().unwrap().to_owned())
            .collect()
    };
    assert_eq!(ids(&catalog), ["group_compare", "summary_stats"]);
    assert_eq!(
        ids(&api.render_catalog(a).await.unwrap()),
        ["scatter", "bar"]
    );
    assert!(
        api.record_analysis_catalog("analysis-worker", "0.6.0", None, &json!([]))
            .await
            .is_err()
    );
    assert!(
        worker
            .record_analysis_catalog(
                "analysis-worker",
                "0.6.0",
                None,
                &json!([{"id": "group_compare", "label": "Compare", "params": {"value": {"type": "code"}}}])
            )
            .await
            .is_err()
    );

    // What the broker can check, it refuses before any task exists.
    for (who, src, table, feature, settings, previous, code) in [
        (
            outsider,
            source,
            table,
            "group_compare",
            good(),
            None,
            "project_not_found",
        ),
        (
            a,
            source,
            "results/other.csv",
            "group_compare",
            good(),
            None,
            "analysis_table_unknown",
        ),
        (
            a,
            source,
            table,
            "scatter",
            good(),
            None,
            "analysis_feature_unknown",
        ),
        (
            a,
            source,
            table,
            "group_compare",
            json!({"value": "amount", "group": "description", "colour": "red"}),
            None,
            "analysis_settings_invalid",
        ),
        (
            a,
            source,
            table,
            "group_compare",
            json!({"value": "amount"}),
            None,
            "analysis_settings_invalid",
        ),
        (
            a,
            source,
            table,
            "group_compare",
            json!({"value": "missing", "group": "description"}),
            None,
            "analysis_settings_invalid",
        ),
        (
            a,
            source,
            table,
            "group_compare",
            json!({"value": "amount", "group": "description", "test": "anova"}),
            None,
            "analysis_settings_invalid",
        ),
        (
            a,
            source,
            table,
            "group_compare",
            good(),
            Some(Uuid::new_v4()),
            "analysis_previous_invalid",
        ),
        (
            a,
            Uuid::new_v4(),
            table,
            "group_compare",
            good(),
            None,
            "task_not_found",
        ),
    ] {
        assert_eq!(
            ask(
                who,
                src,
                table,
                feature,
                settings,
                "",
                previous,
                Uuid::new_v4()
            )
            .await
            .unwrap_err()
            .code,
            code
        );
    }
    let count = |sql: &'static str| {
        let owner = &owner;
        async move { owner.query_one(sql, &[]).await.unwrap().get::<_, i64>(0) }
    };
    assert_eq!(
        count("SELECT count(*) FROM enrichedlab_agent.broker_analysis_requests").await,
        0
    );
    // A figure request cannot name an analysis.
    let figure_of =
        |src: Uuid, table: &'static str, feature: &'static str, controls: &'static str| {
            let api = api.clone();
            async move {
                let controls: Value = serde_json::from_str(controls).unwrap();
                api.request_figure(
                    a,
                    &FigureRequest {
                        project,
                        source_task: src,
                        table_path: table,
                        feature,
                        controls: &controls,
                        title: "",
                        previous: None,
                        request_id: Uuid::new_v4(),
                    },
                )
                .await
            }
        };
    assert_eq!(
        figure_of(
            source,
            table,
            "group_compare",
            r#"{"value": "amount", "group": "description"}"#
        )
        .await
        .unwrap_err()
        .code,
        "figure_feature_unknown"
    );

    // An analysis: the same request again is the same task; the same request id for another request is refused.
    let first_id = Uuid::new_v4();
    let analysis = ask(
        a,
        source,
        table,
        "group_compare",
        good(),
        "Nutrient by food",
        None,
        first_id,
    )
    .await
    .unwrap();
    assert_eq!(
        ask(
            a,
            source,
            table,
            "group_compare",
            good(),
            "Nutrient by food",
            None,
            first_id
        )
        .await
        .unwrap(),
        analysis
    );
    assert_eq!(
        ask(
            a,
            source,
            table,
            "summary_stats",
            json!({"value": "amount"}),
            "",
            None,
            first_id
        )
        .await
        .unwrap_err()
        .code,
        "idempotency_conflict"
    );
    assert!(
        owner
            .execute(
                "UPDATE enrichedlab_agent.broker_analysis_requests SET title='x'",
                &[]
            )
            .await
            .is_err()
    );
    // A retry after the worker read another catalog, even one without the feature, is answered from what the request
    // pinned. The catalog names the service that declared it, so a new release with the same features is seen as new.
    let offered = |ids: &[&str]| -> Value {
        Value::Array(
            catalog["features"]
                .as_array()
                .unwrap()
                .iter()
                .filter(|f| ids.contains(&f["id"].as_str().unwrap()))
                .cloned()
                .collect(),
        )
    };
    let same_features = worker
        .record_analysis_catalog(
            "analysis-worker",
            "0.6.1",
            Some(IMAGE),
            &offered(&["group_compare", "summary_stats"]),
        )
        .await
        .unwrap();
    assert_ne!(same_features, analysis_digest);
    assert_eq!(
        api.analysis_catalog(a).await.unwrap()["service_version"],
        "0.6.1"
    );
    let retry = || {
        ask(
            a,
            source,
            table,
            "group_compare",
            good(),
            "Nutrient by food",
            None,
            first_id,
        )
    };
    assert_eq!(retry().await.unwrap(), analysis);
    worker
        .record_analysis_catalog(
            "analysis-worker",
            "0.6.2",
            Some(IMAGE),
            &offered(&["summary_stats"]),
        )
        .await
        .unwrap();
    assert_eq!(retry().await.unwrap(), analysis);
    // A new request is checked against the newest catalog, which no longer offers the feature.
    assert_eq!(
        ask(
            a,
            source,
            table,
            "group_compare",
            good(),
            "",
            None,
            Uuid::new_v4()
        )
        .await
        .unwrap_err()
        .code,
        "analysis_feature_unknown"
    );
    assert_eq!(
        analysis_run::refresh_catalog(&worker, &config)
            .await
            .unwrap(),
        analysis_digest
    );
    assert_eq!(
        api.analysis_catalog(a).await.unwrap()["service_version"],
        "0.6.0"
    );

    // Only the worker holding the current, unexpired lease reads the request or records a receipt.
    let claimed = worker
        .claim_task("analysis-worker", 120)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        (claimed.id, claimed.route.as_str()),
        (analysis, analysis_run::ROUTE)
    );
    assert!(
        worker
            .task_analysis(
                claimed.id,
                "other-worker",
                claimed.attempt,
                claimed.lease_generation
            )
            .await
            .unwrap()
            .is_none()
    );
    assert!(
        api.task_analysis(
            claimed.id,
            "analysis-worker",
            claimed.attempt,
            claimed.lease_generation
        )
        .await
        .is_err()
    );
    let now = OffsetDateTime::now_utc();
    let stray = AnalysisReceipt {
        status: "failed",
        error_code: Some("analysis_failed".into()),
        message: None,
        refusal: None,
        service_version: None,
        image_digest: None,
        method: None,
        files: None,
        notes: json!([]),
        package_sha256: None,
        started_at: now,
        finished_at: now,
    };
    assert_eq!(
        worker
            .record_analysis_receipt(
                claimed.id,
                "other-worker",
                claimed.attempt,
                claimed.lease_generation,
                &stray
            )
            .await
            .unwrap_err()
            .code,
        "lease_fence_lost"
    );
    owner
        .execute(
            "UPDATE enrichedlab_agent.broker_tasks SET lease_expires_at=clock_timestamp()-interval '1 second' WHERE task_id=$1",
            &[&analysis],
        )
        .await
        .unwrap();
    assert_eq!(
        worker
            .record_analysis_receipt(
                claimed.id,
                "analysis-worker",
                claimed.attempt,
                claimed.lease_generation,
                &stray
            )
            .await
            .unwrap_err()
            .code,
        "lease_fence_lost"
    );
    assert!(
        worker
            .task_analysis(
                claimed.id,
                "analysis-worker",
                claimed.attempt,
                claimed.lease_generation
            )
            .await
            .unwrap()
            .is_none()
    );
    owner
        .execute(
            "UPDATE enrichedlab_agent.broker_tasks SET lease_expires_at=clock_timestamp()+interval '2 minutes' WHERE task_id=$1",
            &[&analysis],
        )
        .await
        .unwrap();
    // A receipt that breaks the record's checks is refused in the broker's code.
    let mut odd = stray.clone();
    odd.status = "refused";
    odd.refusal =
        Some(json!({"code": "group_count_not_two", "columns": [], "counts": [], "words": "x"}));
    assert_eq!(
        worker
            .record_analysis_receipt(
                claimed.id,
                "analysis-worker",
                claimed.attempt,
                claimed.lease_generation,
                &odd
            )
            .await
            .unwrap_err()
            .code,
        "analysis_receipt_invalid"
    );
    analysis_run::execute(worker.clone(), &config, claimed, CancellationToken::new())
        .await
        .unwrap();
    let row = owner
        .query_one(
            "SELECT status,result_class FROM enrichedlab_agent.broker_tasks WHERE task_id=$1",
            &[&analysis],
        )
        .await
        .unwrap();
    assert_eq!(
        (row.get::<_, String>(0), row.get::<_, String>(1)),
        ("completed".into(), "analysis_completed".into())
    );
    let detail = api.task_analysis_detail(a, analysis).await.unwrap();
    assert_eq!(detail["source_task"], json!(source));
    assert_eq!(detail["settings"], good());
    let receipt = &detail["receipts"][0];
    assert_eq!(receipt["status"], "completed");
    assert_eq!(receipt["service_version"], "0.6.0");
    assert_eq!(receipt["image_digest"], IMAGE);
    // The method loses the direction mark the service sent; the notes are the template's and then the service's.
    assert_eq!(receipt["method"], "Welch two-sample t-test");
    assert_eq!(
        receipt["notes"],
        json!([
            "Example food A vs Example food B, raw (value = amount)",
            "'unused' was not used"
        ])
    );
    let roles: Vec<(&str, &str)> = receipt["files"]
        .as_array()
        .unwrap()
        .iter()
        .map(|f| (f["path"].as_str().unwrap(), f["role"].as_str().unwrap()))
        .collect();
    assert_eq!(
        roles,
        [
            ("results/results.csv", "table"),
            ("results/statistics.csv", "statistics"),
            ("results/record.json", "record"),
            ("figures/figure.png", "figure"),
            ("figures/figure.svg", "figure")
        ]
    );
    let package_sha = receipt["package_sha256"].as_str().unwrap().to_owned();
    let artifacts = api.list_artifacts(a, analysis).await.unwrap();
    let package = artifacts.iter().find(|x| x.role == "result").unwrap();
    assert_eq!(package.sha256, package_sha);
    let bytes = fs::read(root.join("artifacts").join(&package.relative_path)).unwrap();
    assert_eq!(hex::encode(Sha256::digest(&bytes)), package_sha);
    let files = members(&bytes);
    let file = |name: &str| {
        files
            .iter()
            .find(|(n, _)| n == name)
            .map(|(_, b)| String::from_utf8(b.clone()).unwrap())
            .unwrap_or_else(|| panic!("{name} is not in the package"))
    };
    // Every row at every digit the service sent; a missing value is NA, as R writes it; text with a comma is quoted.
    assert_eq!(
        file("results/results.csv"),
        "group,n,mean,p_value\nExample food A,2,40.25,6.12498179243933e-29\n\"Example food B, raw\",2,22.3,NA\n"
    );
    assert_eq!(
        file("results/statistics.csv"),
        "statistic,value\np_value,6.12498179243933e-29\nt_statistic,25.123456789012\ntest,Welch\n"
    );
    assert_eq!(
        serde_json::from_str::<Value>(&file("results/record.json")).unwrap()["rows"][0]["p_value"],
        json!(6.12498179243933e-29)
    );
    assert!(file("code/group_compare.R").contains("_common.R"));
    assert_eq!(
        serde_json::from_str::<Value>(&file("code/params.json")).unwrap()["params"]["group"],
        "description"
    );
    assert_eq!(
        serde_json::from_str::<Value>(&file("spec.json")).unwrap()["source"]["table_sha256"],
        detail["table_sha256"]
    );
    assert!(file("README.md").contains("No code here was written by a model"));
    let sums = file("SHA256SUMS");
    for (name, body) in &files {
        if name != "SHA256SUMS" {
            assert!(
                sums.contains(&format!("{}  {name}\n", hex::encode(Sha256::digest(body)))),
                "{name} is not listed"
            );
        }
    }
    assert!(
        !files.iter().any(|(n, _)| n.ends_with("query_result.csv")),
        "the source table is named by digest, not copied"
    );
    let digest = api.artifact_digest(a, analysis, package.id).await.unwrap();
    let tables: Vec<&str> = digest["digest"]["tables"]
        .as_array()
        .or_else(|| digest["tables"].as_array())
        .unwrap_or_else(|| panic!("no tables in {digest}"))
        .iter()
        .map(|t| t["path"].as_str().unwrap())
        .collect();
    assert!(
        tables.contains(&"results/results.csv") && tables.contains(&"results/statistics.csv"),
        "{digest}"
    );
    let declared: Vec<String> = analysis_run::declared_figures(&detail, &package_sha)
        .into_iter()
        .map(|d| d.path)
        .collect();
    assert_eq!(declared, ["figures/figure.png", "figures/figure.svg"]);

    // The results table is a source like any other: a figure of it and another analysis of it are accepted.
    let figure = figure_of(
        analysis,
        "results/results.csv",
        "scatter",
        r#"{"x": "mean", "y": "n"}"#,
    )
    .await
    .unwrap();
    let again = ask(
        a,
        analysis,
        "results/results.csv",
        "summary_stats",
        json!({"value": "mean"}),
        "",
        None,
        Uuid::new_v4(),
    )
    .await
    .unwrap();

    // Readers see the analysis and the analyses made from one analysis; nobody else does.
    assert_eq!(
        api.task_analysis_detail(outsider, analysis)
            .await
            .unwrap_err()
            .code,
        "task_not_found"
    );
    assert_eq!(
        api.task_analysis_detail(a, source).await.unwrap(),
        Value::Null
    );
    assert_eq!(
        api.analysis_versions(a, source).await.unwrap()[0]["task_id"],
        json!(analysis)
    );
    assert_eq!(
        api.analysis_versions(a, analysis).await.unwrap()[0]["task_id"],
        json!(again)
    );
    assert_eq!(
        api.analysis_versions(outsider, source)
            .await
            .unwrap_err()
            .code,
        "task_not_found"
    );

    // Lineage names the analysis record, the reviewed service and the prior output it read; the attempt names the image.
    let lineage = api.task_lineage(a, analysis).await.unwrap();
    assert_eq!(lineage["method"]["tool_class"], "vetted_service");
    assert_eq!(lineage["analysis"]["source_task"], json!(source));
    assert_eq!(lineage["analysis"]["feature"], "group_compare");
    assert_eq!(lineage["analysis"]["settings"], good());
    assert_eq!(lineage["analysis"]["service_version"], "0.6.0");
    assert_eq!(lineage["analysis"]["method"], "Welch two-sample t-test");
    assert_eq!(lineage["render"], Value::Null);
    assert_eq!(lineage["inputs"]["classes"], json!(["prior_output"]));
    assert_eq!(lineage["inputs"]["classes_recorded_by"], "analysis_request");
    assert_eq!(lineage["review"]["state"], "not_reviewed");
    let gaps = lineage["gaps"].as_array().unwrap();
    for gap in [
        "plan_step_not_recorded",
        "inputs_not_recorded",
        "input_class_not_recorded",
        "no_outputs_recorded",
        "image_not_digest_pinned",
    ] {
        assert!(!gaps.iter().any(|g| g == gap), "{gap} in {lineage}");
    }
    assert!(
        lineage["outputs"]
            .as_array()
            .unwrap()
            .iter()
            .all(|o| o["provenance_class"] == "vetted_tool_output")
    );
    let attempts = api.task_lineage_v2(a, analysis).await.unwrap();
    assert_eq!(attempts["attempts"][0]["execution"]["image_digest"], IMAGE);
    assert!(
        attempts["attempts"][0]["execution"]["template_ref"]
            .as_str()
            .unwrap()
            .ends_with("group_compare.R")
    );

    // A refusal the broker can check finishes in its own words with the refusal kept; one naming a column the table does
    // not have gets the general words; the service's words are kept nowhere.
    let run_one = |settings: Value, title: &'static str| {
        let worker = worker.clone();
        let config = &config;
        let ask = &ask;
        async move {
            let task = ask(
                a,
                source,
                table,
                "group_compare",
                settings,
                title,
                Some(analysis),
                Uuid::new_v4(),
            )
            .await
            .unwrap();
            let claimed = worker
                .claim_task("analysis-worker", 120)
                .await
                .unwrap()
                .unwrap();
            assert_eq!(claimed.id, task);
            analysis_run::execute(worker.clone(), config, claimed, CancellationToken::new())
                .await
                .unwrap();
            task
        }
    };
    // The figure and the second analysis wait in the queue ahead of these; they are run here first.
    for _ in 0..2 {
        let claimed = worker
            .claim_task("analysis-worker", 120)
            .await
            .unwrap()
            .unwrap();
        if claimed.route == figure_run::ROUTE {
            assert_eq!(claimed.id, figure);
            figure_run::execute(worker.clone(), &config, claimed, CancellationToken::new())
                .await
                .unwrap();
        } else {
            assert_eq!(claimed.id, again);
            analysis_run::execute(worker.clone(), &config, claimed, CancellationToken::new())
                .await
                .unwrap();
        }
    }
    let refused = run_one(
        json!({"value": "amount", "group": "description", "test": "wilcoxon"}),
        "",
    )
    .await;
    let detail = api.task_analysis_detail(a, refused).await.unwrap();
    let receipt = &detail["receipts"][0];
    assert_eq!(
        (receipt["status"].as_str(), receipt["error_code"].as_str()),
        (Some("refused"), Some("analysis_refused"))
    );
    assert_eq!(
        receipt["message"],
        "This analysis compares exactly two groups; description has 4."
    );
    assert_eq!(
        receipt["refusal"],
        json!({"code": "group_count_not_two", "columns": ["description"], "counts": [4]})
    );
    assert_eq!(detail["previous"], json!(analysis));
    let unknown = run_one(
        json!({"value": "amount", "group": "description", "test": "kruskal"}),
        "",
    )
    .await;
    let receipt = api.task_analysis_detail(a, unknown).await.unwrap()["receipts"][0].clone();
    assert_eq!(
        (
            receipt["status"].as_str(),
            receipt["error_code"].as_str(),
            receipt["message"].as_str()
        ),
        (
            Some("failed"),
            Some("analysis_failed"),
            Some(
                "The analysis service could not run this analysis on this table with these settings."
            )
        )
    );
    assert_eq!(receipt["refusal"], Value::Null);
    let outdated = run_one(good(), "old service").await;
    let receipt = api.task_analysis_detail(a, outdated).await.unwrap()["receipts"][0].clone();
    assert_eq!(receipt["error_code"], "render_service_outdated");
    assert_eq!(
        count("SELECT count(*) FROM enrichedlab_agent.broker_analysis_receipts WHERE message LIKE '%Example food A%' OR refusal::text LIKE '%Example food A%' OR method LIKE '%found 4%'").await,
        0
    );
    let unnamed = run_one(good(), "no digest").await;
    let lineage = api.task_lineage(a, unnamed).await.unwrap();
    assert_eq!(lineage["task"]["status"], "completed");
    assert!(
        lineage["gaps"]
            .as_array()
            .unwrap()
            .iter()
            .any(|g| g == "image_not_digest_pinned"),
        "{lineage}"
    );
    // An analysis that did not complete has no files by design; its lineage names no missing output or image.
    let lineage = api.task_lineage(a, refused).await.unwrap();
    assert_eq!(lineage["outputs"], json!([]));
    assert!(
        !lineage["gaps"]
            .as_array()
            .unwrap()
            .iter()
            .any(|g| g == "image_not_digest_pinned" || g == "no_outputs_recorded"),
        "{lineage}"
    );

    // A receipt the record's checks refuse is refused whole: a kept refusal names at most eight columns and takes at
    // most 2,048 bytes, and the notes at most 16,384 bytes, each as PostgreSQL stores it. The same receipt again is
    // accepted; one that differs in any field is not.
    let replays = ask(
        a,
        source,
        table,
        "summary_stats",
        json!({"value": "amount"}),
        "replays",
        None,
        Uuid::new_v4(),
    )
    .await
    .unwrap();
    let claimed = worker
        .claim_task("analysis-worker", 120)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(claimed.id, replays);
    let record_receipt = |receipt: AnalysisReceipt| {
        let worker = worker.clone();
        let claimed = claimed.clone();
        async move {
            worker
                .record_analysis_receipt(
                    claimed.id,
                    "analysis-worker",
                    claimed.attempt,
                    claimed.lease_generation,
                    &receipt,
                )
                .await
        }
    };
    let at = OffsetDateTime::now_utc();
    let refused_with = |columns: Value| AnalysisReceipt {
        status: "refused",
        error_code: Some("analysis_refused".into()),
        message: Some("No row has values in all of a and b.".into()),
        refusal: Some(json!({"code": "no_complete_rows", "columns": columns, "counts": []})),
        service_version: None,
        image_digest: None,
        method: None,
        files: None,
        notes: json!([]),
        package_sha256: None,
        started_at: at,
        finished_at: at,
    };
    let nine: Vec<String> = (0..9).map(|i| format!("c{i}")).collect();
    let long: Vec<String> = (0..8).map(|i| format!("{i}{}", "c".repeat(260))).collect();
    assert!(
        figure_run::stored_bytes(
            &json!({"code": "no_complete_rows", "columns": long, "counts": []})
        ) > 2048
    );
    for columns in [json!(nine), json!(long)] {
        assert_eq!(
            record_receipt(refused_with(columns))
                .await
                .unwrap_err()
                .code,
            "analysis_receipt_invalid"
        );
    }
    // Notes of exactly 16,384 stored bytes, then one byte more.
    let mut notes: Vec<String> = (0..27).map(|_| "é".repeat(300)).collect();
    notes[26] = "é".repeat(338);
    assert_eq!(figure_run::stored_bytes(&json!(notes)), 16_384);
    let failed_with = |notes: &Vec<String>| AnalysisReceipt {
        status: "failed",
        error_code: Some("analysis_failed".into()),
        message: Some(
            "The analysis service could not run this analysis on this table with these settings."
                .into(),
        ),
        refusal: None,
        notes: json!(notes),
        ..refused_with(json!([]))
    };
    let mut over = notes.clone();
    over[26].push('x');
    assert_eq!(
        record_receipt(failed_with(&over)).await.unwrap_err().code,
        "analysis_receipt_invalid"
    );
    // The worker's own bound keeps notes inside the record whatever the service sends.
    let sent: Vec<String> = (0..40).map(|_| "é".repeat(300)).collect();
    assert!(figure_run::stored_bytes(&json!(figure_run::kept_notes(sent, 16_384))) <= 16_384);
    let first = failed_with(&notes);
    record_receipt(first.clone()).await.unwrap();
    record_receipt(first.clone()).await.unwrap();
    let mut other_notes = first.clone();
    other_notes.notes = json!(["another note"]);
    let mut other_message = first.clone();
    other_message.message = Some("Another message.".into());
    let mut other_start = first.clone();
    other_start.started_at = at - time::Duration::seconds(1);
    for differs in [other_notes, other_message, other_start] {
        assert_eq!(
            record_receipt(differs).await.unwrap_err().code,
            "idempotency_conflict"
        );
    }
    assert!(
        worker
            .finish_task(
                replays,
                "analysis-worker",
                claimed.lease_generation,
                false,
                "analysis_not_completed",
                "analysis_failed"
            )
            .await
            .unwrap()
    );

    // A figure version cancelled while its lease is held records its receipt and ends cancelled (the figure receipt's
    // fence, replaced in migration 058), and its receipt too is compared in every field.
    let cancelled_figure = figure_of(
        source,
        table,
        "scatter",
        r#"{"x": "amount", "y": "amount"}"#,
    )
    .await
    .unwrap();
    let claimed = worker
        .claim_task("analysis-worker", 120)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(claimed.id, cancelled_figure);
    api.cancel_task(a, cancelled_figure).await.unwrap();
    let figure_receipt = FigureReceipt {
        status: "failed",
        error_code: Some("cancelled".into()),
        message: None,
        service_version: None,
        image_digest: None,
        files: None,
        notes: json!([]),
        package_sha256: None,
        started_at: at,
        finished_at: at,
    };
    let record_figure = |receipt: FigureReceipt| {
        let worker = worker.clone();
        let claimed = claimed.clone();
        async move {
            worker
                .record_figure_receipt(
                    claimed.id,
                    "analysis-worker",
                    claimed.attempt,
                    claimed.lease_generation,
                    &receipt,
                )
                .await
        }
    };
    record_figure(figure_receipt.clone()).await.unwrap();
    record_figure(figure_receipt.clone()).await.unwrap();
    let mut later = figure_receipt.clone();
    later.finished_at = at + time::Duration::seconds(1);
    assert_eq!(
        record_figure(later).await.unwrap_err().code,
        "idempotency_conflict"
    );
    assert!(
        worker
            .finish_task(
                cancelled_figure,
                "analysis-worker",
                claimed.lease_generation,
                false,
                "figure_not_completed",
                "cancelled"
            )
            .await
            .unwrap()
    );
    let status = |task: Uuid| {
        let owner = &owner;
        async move {
            let row = owner
                .query_one(
                    "SELECT status, lease_owner FROM enrichedlab_agent.broker_tasks WHERE task_id=$1",
                    &[&task],
                )
                .await
                .unwrap();
            (row.get::<_, String>(0), row.get::<_, Option<String>>(1))
        }
    };
    assert_eq!(status(cancelled_figure).await, ("cancelled".into(), None));

    // An analysis cancelled while the service works on it: the worker sees the request at its next lease renewal,
    // records the cancellation's receipt and ends the task cancelled, so it no longer counts as active.
    let slow = ask(
        a,
        source,
        table,
        "summary_stats",
        json!({"value": "amount"}),
        "slow",
        None,
        Uuid::new_v4(),
    )
    .await
    .unwrap();
    let claimed = worker
        .claim_task("analysis-worker", 120)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(claimed.id, slow);
    let quick_renewals = WorkerConfig {
        lease_seconds: 15,
        ..config.clone()
    };
    let active = || {
        let api = api.clone();
        async move {
            api.list_projects(a, 10)
                .await
                .unwrap()
                .iter()
                .find(|p| p["project_id"] == json!(project))
                .unwrap()["task_counts"]["active"]
                .as_u64()
                .unwrap()
        }
    };
    let (ran, ()) = tokio::join!(
        analysis_run::execute(
            worker.clone(),
            &quick_renewals,
            claimed,
            CancellationToken::new()
        ),
        async {
            SLOW_STARTED.notified().await;
            assert_eq!(active().await, 1);
            assert_eq!(api.cancel_task(a, slow).await.unwrap(), "cancelling");
        }
    );
    ran.unwrap();
    assert_eq!(status(slow).await, ("cancelled".into(), None));
    let receipt = api.task_analysis_detail(a, slow).await.unwrap()["receipts"][0].clone();
    assert_eq!(
        (receipt["status"].as_str(), receipt["error_code"].as_str()),
        (Some("failed"), Some("cancelled"))
    );
    assert_eq!(active().await, 0);

    // The project's contents title each template analysis and name the one it changes; a template analysis is an
    // analysis, so it counts in the project's totals, unlike a figure version.
    let read = api.read_project(a, project).await.unwrap();
    let entry = |task: Uuid| {
        read["tasks"]
            .as_array()
            .unwrap()
            .iter()
            .find(|t| t["task_id"] == json!(task))
            .unwrap()
            .clone()
    };
    assert_eq!(
        entry(analysis)["template_analysis"]["title"],
        "Nutrient by food"
    );
    assert_eq!(
        entry(analysis)["template_analysis"]["feature"],
        "group_compare"
    );
    assert_eq!(
        entry(analysis)["template_analysis"]["source_task"],
        json!(source)
    );
    assert_eq!(
        entry(refused)["template_analysis"]["previous"],
        json!(analysis)
    );
    assert_eq!(entry(source)["template_analysis"], Value::Null);
    assert_eq!(entry(figure)["template_analysis"], Value::Null);
    assert!(entry(figure)["figure"].is_object());
    let listed = api.list_projects(a, 10).await.unwrap();
    let counts = &listed
        .iter()
        .find(|p| p["project_id"] == json!(project))
        .unwrap()["task_counts"];
    let figures = read["tasks"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|t| t["figure"].is_object())
        .count();
    // The figure of the results and the cancelled figure.
    assert_eq!(figures, 2);
    assert_eq!(
        counts["total"],
        json!(read["tasks"].as_array().unwrap().len() - figures)
    );

    // A template analysis waits for a person's review like any analysis; a figure version does not.
    let inbox = api.inbox(a).await.unwrap();
    let waiting: Vec<&Value> = inbox["items"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|i| i["kind"] == "analysis_review")
        .map(|i| &i["task_id"])
        .collect();
    assert!(waiting.contains(&&json!(analysis.to_string())), "{inbox}");
    assert!(!waiting.contains(&&json!(figure.to_string())), "{inbox}");

    assert!(
        owner
            .execute(
                "DELETE FROM enrichedlab_agent.broker_analysis_receipts",
                &[]
            )
            .await
            .is_err(),
        "receipts are immutable"
    );

    std::fs::remove_dir_all(&root).unwrap();
    drop(owner);
    connection.abort();
}
