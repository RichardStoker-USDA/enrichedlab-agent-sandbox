//! Figure versions (migration 040) against an isolated PostgreSQL and a stand-in render service: the
//! worker keeps the service's figure catalog, a person asks for a version from a finished analysis's result table, the
//! broker refuses what it can check, the worker's render route draws it from the retained table and keeps a package
//! with a receipt, a refused setting finishes in the broker's words for the service's refusal code (never the
//! service's own words), readers and lineage see the version, and
//! a queued version does not count against a research run's parallel limit.
use axum::{
    Json, Router,
    extract::State,
    http::StatusCode,
    routing::{get, post},
};
use base64::Engine;
use enrichedlab_agent_sandbox::broker::{
    config::WorkerConfig,
    coordinator::StartRun,
    figure_run,
    model::{PrincipalClass, SubmitTaskRequest, VerifiedIdentity},
    project::{CheckpointRequest, CreateProject, ResearchState, Visibility},
    query_run,
    sql_guard::{self, Allowed},
    store::{FigureRequest, Store, StoreErrorKind},
};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeSet,
    env, fs,
    io::Read,
    path::PathBuf,
    sync::{Arc, Mutex},
    time::Duration,
};
use tokio_postgres::{Config, NoTls, config::Host};
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

const MANIFEST: &str = "5f1c0d9a3a4e2b7c8d6e5f4a3b2c1d0e9f8a7b6c5d4e3f2a1b0c9d8e7f6a5b4c";
const IMAGE: &str = "sha256:7f1b2c3d4e5f60718293a4b5c6d7e8f90112233445566778899aabbccddeeff0";

fn research(extra: &[&str]) -> ResearchState {
    let mut plan = vec![
        json!({"action_key":"rank-foods","title":"Rank foods by the nutrient","status":"pending","task_id":null,
        "evidence_refs":[],"rationale":"Answer the question."}),
    ];
    for key in extra {
        plan.push(json!({"action_key":key,"title":"Rank them again","status":"pending","task_id":null,"evidence_refs":[],
            "rationale":"A second look."}));
    }
    serde_json::from_value(json!({"schema_version":1,"goal":"Which example foods carry the most of the example nutrient?",
        "constraints":["Read only the reference data."],"plan":plan,"lessons":[],"next_action":"Query the reference data.",
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
    json!({"action_key": key, "profile": "reference-sql", "objective": "Rank the foods.", "request_sha256": "c".repeat(64),
        "query": {"source": "example-reference", "proposed_sql": sql, "admitted_sql": a.sql,
            "sha256": format!("sha256:{}", hex::encode(Sha256::digest(a.sql.as_bytes()))), "guard": sql_guard::GUARD,
            "relations": a.relations, "row_limit": a.limit}})
}

/// A stand-in for the reviewed render service: its catalog, and a figure for any request except one that asks for
/// the axis title "refuse me", which it refuses with a refusal code beside its own words; the words must not be passed
/// on. It keeps the last request it drew.
async fn render_service(seen: Arc<Mutex<Option<Value>>>) -> String {
    async fn features() -> Json<Value> {
        Json(
            json!({"service_version": "0.5.20", "image_digest": IMAGE, "features": [
                {"id": "scatter", "label": "Scatter plot", "kind": "figure", "description": "One point per row.",
                 "params": {"x": {"type": "column", "required": true}, "y": {"type": "column", "required": true},
                   "x_label": {"type": "string", "required": false}, "width": {"type": "number", "min": 3, "max": 20, "required": false},
                   "x_limits": {"type": "numbers", "maxlen": 2, "required": false},
                   "legend_position": {"type": "enum", "choices": ["right", "left", "top", "bottom", "none"], "required": false},
                   "color": {"type": "column", "required": false},
                   "group_colors": {"type": "colors", "of": "color", "choices": ["#E69F00", "#56B4E9", "#999999"], "required": false}}},
                {"id": "bar", "label": "Bar chart", "kind": "figure", "description": "One bar per category.",
                 "params": {"category": {"type": "column", "required": true}, "value": {"type": "column", "required": true}}},
                {"id": "anova_emmeans", "label": "ANOVA", "kind": "analysis", "params": {"outcome": {"type": "column", "required": true}}}
            ]}),
        )
    }
    async fn render(
        State(seen): State<Arc<Mutex<Option<Value>>>>,
        Json(body): Json<Value>,
    ) -> (StatusCode, Json<Value>) {
        if body["params"]["x_label"] == "refuse me" {
            return (
                StatusCode::BAD_REQUEST,
                Json(
                    json!({"error": "'x_label' is refused by this stand-in service",
                    "refusal": {"code": "setting_invalid", "columns": [], "counts": []}}),
                ),
            );
        }
        *seen.lock().unwrap() = Some(body.clone());
        // A service that does not report its image: the version is drawn, and its lineage must name the gap.
        let image = if body["params"]["x_label"] == "no digest" {
            Value::Null
        } else {
            json!(IMAGE)
        };
        let b64 = |bytes: &[u8]| base64::engine::general_purpose::STANDARD.encode(bytes);
        let mut params = body["params"].clone();
        if params["width"].as_f64().is_some_and(|w| w > 20.0) {
            params["width"] = json!(20.0);
        }
        (
            StatusCode::OK,
            Json(json!({
                "kind": "figure", "png": b64(b"\x89PNG\r\n\x1a\nstand-in"), "svg": "<svg xmlns='http://www.w3.org/2000/svg'/>",
                "pdf": b64(b"%PDF-1.7 stand-in"), "code": "source(file.path(dirname(.tf), \"_common.R\"))\n",
                "common_code": "WORK <- commandArgs(TRUE)[1]\n", "service_version": "0.5.20", "image_digest": image,
                "spec": {"params": params, "title": body["title"], "feature_id": body["feature_id"]},
                "font_face": "DejaVu Sans",
                "notes": if body["params"]["width"].as_f64().is_some_and(|w| w > 20.0) { json!(["'width' was above the 20 maximum; 20 was used"]) } else { json!([]) }
            })),
        )
    }
    let app = Router::new()
        .route("/features", get(features))
        .route("/render", post(render))
        .with_state(seen);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    url
}

#[tokio::test]
#[ignore = "isolated PostgreSQL through qualify_remote_broker.sh figure-versions"]
async fn a_person_draws_a_figure_version_from_a_finished_table() {
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
    // Each role is ready only with migration 040 and its own grants; the API's login cannot serve the render route.
    api.ready_for_review_api().await.unwrap();
    worker.ready_for_figures().await.unwrap();
    assert_eq!(
        api.ready_for_figures().await.unwrap_err().code,
        "broker_figure_schema_not_ready"
    );
    let detail_fn = "enrichedlab_agent.broker_request_figure(text,uuid,uuid,uuid,text,text,jsonb,text,uuid,uuid)";
    owner
        .batch_execute(&format!(
            "REVOKE EXECUTE ON FUNCTION {detail_fn} FROM broker_api_qual"
        ))
        .await
        .unwrap();
    assert!(api.ready_for_review_api().await.is_err());
    owner
        .batch_execute(&format!(
            "GRANT EXECUTE ON FUNCTION {detail_fn} TO broker_api_qual"
        ))
        .await
        .unwrap();
    api.ready_for_review_api().await.unwrap();

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
        INSERT INTO reference_qual.food VALUES (1,'Example food A'),(2,'Example food B, raw'),(3,'Example \"quoted\" food'),(4,'Example food D');
        INSERT INTO reference_qual.food_nutrient VALUES (1,48.2),(2,31.7),(3,NULL),(4,12.9);
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
                  ('figure-render','Draw a figure from a result table','server-render')) v(k,n,r)
        WHERE tenant_id='research-site' AND profile_key='general-research';
        INSERT INTO enrichedlab_agent.broker_group_profile_grants(tenant_id,group_key,profile_key)
          VALUES('research-site','pilot-lab','reference-sql');")).await.unwrap();
    owner
        .query_one(
            "SELECT enrichedlab_agent.broker_register_sql_source('research-site',$1)",
            &[&json!({"source": "example-reference", "profile": "reference-sql", "title": "Example reference data",
                "description": "Invented foods and one invented nutrient's amount per 100 g.", "release": "Qualification release",
                "schema": "reference_qual",
                "relations": [{"name": "food", "row_count": 4, "columns": [{"name": "fdc_id", "type": "integer"}, {"name": "description", "type": "text"}]},
                              {"name": "food_nutrient", "row_count": 4, "columns": [{"name": "fdc_id", "type": "integer"}, {"name": "amount", "type": "double precision"}]}],
                "functions": ["count", "max", "round"], "row_cap": 100, "statement_timeout_ms": 5000,
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
            title: "Figures".into(),
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
            state: research(&[]),
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
    let record = json!({"schema_version":1,"test":"bounded metadata"});
    let committed = coordinator
        .commit_research_turn(
            run,
            "lead-a",
            generation,
            1,
            &serde_json::to_value(research(&[])).unwrap(),
            &json!([query_action("rank-foods")]),
            &record,
            "delegate",
        )
        .await
        .unwrap();
    let source: Uuid = serde_json::from_value(committed["tasks"][0]["task_id"].clone()).unwrap();

    let seen = Arc::new(Mutex::new(None));
    let render_url = render_service(seen.clone()).await;
    let root =
        std::env::temp_dir().join(format!("figure_versions_codetestfile_{}", Uuid::new_v4()));
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
        worker_id: "render-worker".into(),
        direct_dispatch: None,
        query_connection_dir: Some(connections),
        render_url: Some(render_url),
        dataset_fetcher_config: None,
        lab_objects_config: None,
        live_output: false,
        max_parallel: 1,
        lease_seconds: 120,
        poll_interval: Duration::from_millis(200),
        database_pool: 2,
        max_workspace_files: 10,
        max_workspace_bytes: 1024,
    };
    let claimed = worker
        .claim_task("render-worker", 120)
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

    let request = |table: &'static str,
                   feature: &'static str,
                   controls: Value,
                   previous: Option<Uuid>,
                   id: Uuid| { (table, feature, controls, previous, id) };
    let ask =
        |who: Uuid, req: (&'static str, &'static str, Value, Option<Uuid>, Uuid), src: Uuid| {
            let api = api.clone();
            async move {
                let (table, feature, controls, previous, request_id) = req;
                api.request_figure(
                    who,
                    &FigureRequest {
                        project,
                        source_task: src,
                        table_path: table,
                        feature,
                        controls: &controls,
                        title: "Nutrient by food",
                        previous,
                        request_id,
                    },
                )
                .await
            }
        };
    let good = || json!({"x": "amount", "y": "amount", "x_label": "Amount per 100 g", "legend_position": "bottom", "width": 30});
    // Without a figure method for the lab, and before the worker has read the catalog, nothing is requested.
    let refusal = ask(
        a,
        request(
            "results/query_result.csv",
            "scatter",
            good(),
            None,
            Uuid::new_v4(),
        ),
        source,
    )
    .await
    .unwrap_err();
    assert_eq!(
        (refusal.kind, refusal.code),
        (StoreErrorKind::Forbidden, "figure_method_unavailable")
    );
    owner.batch_execute("INSERT INTO enrichedlab_agent.broker_group_profile_grants(tenant_id,group_key,profile_key)
        VALUES('research-site','pilot-lab','figure-render');").await.unwrap();
    // The figure method runs only from a figure request: it is not listed as a method, a person cannot run it directly
    // and a research run cannot be given it.
    let me = api.me(a).await.unwrap();
    let offered = |key: &str| {
        me.profiles
            .iter()
            .find(|p| p.key == key)
            .map(|p| (p.offered, p.in_runs))
    };
    assert_eq!(offered("figure-render"), Some((false, false)));
    assert_eq!(offered("reference-sql"), Some((true, true)));
    let digest = "0".repeat(64);
    let direct = api
        .submit_task(
            a,
            "pilot-lab",
            "figure-render",
            "Draw a figure",
            "direct-figure",
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
                profile: "figure-render".into(),
                objective: "Draw a figure".into(),
                project_id: Some(project),
                action_key: Some("direct-figure".into()),
                parent_task_id: None,
            },
            "direct-figure-project",
            &digest,
        )
        .await
        .unwrap_err();
    assert_eq!(
        (in_project.kind, in_project.code),
        (StoreErrorKind::Forbidden, "profile_forbidden")
    );
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
    assert_eq!(
        (given.kind, given.code),
        (StoreErrorKind::Forbidden, "profile_forbidden")
    );
    let refusal = ask(
        a,
        request(
            "results/query_result.csv",
            "scatter",
            good(),
            None,
            Uuid::new_v4(),
        ),
        source,
    )
    .await
    .unwrap_err();
    assert_eq!(
        (refusal.kind, refusal.code),
        (StoreErrorKind::Unavailable, "figure_catalog_unavailable")
    );

    // The worker keeps the figure features only; the API serves them to any enabled person.
    assert!(
        api.record_render_catalog("render-worker", "0.5.20", None, &json!([]))
            .await
            .is_err()
    );
    let digest = figure_run::refresh_catalog(&worker, &config).await.unwrap();
    assert_eq!(
        figure_run::refresh_catalog(&worker, &config).await.unwrap(),
        digest
    );
    let catalog = api.render_catalog(a).await.unwrap();
    assert_eq!(catalog["catalog_sha256"], digest);
    assert_eq!(catalog["service_version"], "0.5.20");
    assert_eq!(catalog["image_digest"], IMAGE);
    assert_eq!(catalog["groups"], json!(["pilot-lab"]));
    let ids: Vec<&str> = catalog["features"]
        .as_array()
        .unwrap()
        .iter()
        .map(|f| f["id"].as_str().unwrap())
        .collect();
    assert_eq!(ids, ["scatter", "bar"]);
    assert!(
        worker
            .record_render_catalog(
                "render-worker",
                "0.5.20",
                None,
                &json!([{"id": "scatter", "label": "Scatter", "params": {"x": {"type": "code"}}}])
            )
            .await
            .is_err()
    );

    // What the broker can check, it refuses before any task exists.
    for (who, req, src, code) in [
        (
            outsider,
            request(
                "results/query_result.csv",
                "scatter",
                good(),
                None,
                Uuid::new_v4(),
            ),
            source,
            "project_not_found",
        ),
        (
            a,
            request("results/other.csv", "scatter", good(), None, Uuid::new_v4()),
            source,
            "figure_table_unknown",
        ),
        (
            a,
            request(
                "results/query_result.csv",
                "anova_emmeans",
                good(),
                None,
                Uuid::new_v4(),
            ),
            source,
            "figure_feature_unknown",
        ),
        (
            a,
            request(
                "results/query_result.csv",
                "scatter",
                json!({"x": "amount", "y": "amount", "colour": "red"}),
                None,
                Uuid::new_v4(),
            ),
            source,
            "figure_settings_invalid",
        ),
        (
            a,
            request(
                "results/query_result.csv",
                "scatter",
                json!({"x": "missing", "y": "amount"}),
                None,
                Uuid::new_v4(),
            ),
            source,
            "figure_settings_invalid",
        ),
        (
            a,
            request(
                "results/query_result.csv",
                "scatter",
                json!({"x": "amount"}),
                None,
                Uuid::new_v4(),
            ),
            source,
            "figure_settings_invalid",
        ),
        (
            a,
            request(
                "results/query_result.csv",
                "scatter",
                json!({"x": "amount", "y": "amount", "legend_position": "middle"}),
                None,
                Uuid::new_v4(),
            ),
            source,
            "figure_settings_invalid",
        ),
        (
            a,
            request(
                "results/query_result.csv",
                "scatter",
                json!({"x": "amount", "y": "amount", "x_limits": [1, "2"]}),
                None,
                Uuid::new_v4(),
            ),
            source,
            "figure_settings_invalid",
        ),
        (
            a,
            request(
                "results/query_result.csv",
                "scatter",
                good(),
                Some(Uuid::new_v4()),
                Uuid::new_v4(),
            ),
            source,
            "figure_previous_invalid",
        ),
        (
            a,
            request(
                "results/query_result.csv",
                "scatter",
                good(),
                None,
                Uuid::new_v4(),
            ),
            Uuid::new_v4(),
            "task_not_found",
        ),
    ] {
        assert_eq!(ask(who, req, src).await.unwrap_err().code, code);
    }
    assert_eq!(
        owner
            .query_one(
                "SELECT count(*) FROM enrichedlab_agent.broker_figure_requests",
                &[]
            )
            .await
            .unwrap()
            .get::<_, i64>(0),
        0
    );

    // A version: the same request again is the same task; the same request id for a different request is refused.
    let first_id = Uuid::new_v4();
    let version = ask(
        a,
        request(
            "results/query_result.csv",
            "scatter",
            good(),
            None,
            first_id,
        ),
        source,
    )
    .await
    .unwrap();
    assert_eq!(
        ask(
            a,
            request(
                "results/query_result.csv",
                "scatter",
                good(),
                None,
                first_id
            ),
            source
        )
        .await
        .unwrap(),
        version
    );
    assert_eq!(
        ask(
            a,
            request(
                "results/query_result.csv",
                "bar",
                json!({"category": "description", "value": "amount"}),
                None,
                first_id
            ),
            source
        )
        .await
        .unwrap_err()
        .code,
        "idempotency_conflict"
    );
    assert!(
        owner
            .execute(
                "UPDATE enrichedlab_agent.broker_figure_requests SET title='x'",
                &[]
            )
            .await
            .is_err()
    );

    // A queued version is a person's work, not the run's: a second turn may still use both of its parallel slots.
    owner.execute("UPDATE enrichedlab_agent.broker_research_runs SET available_at=clock_timestamp()-interval '1 second' WHERE run_id=$1", &[&run]).await.unwrap();
    let claim = coordinator
        .claim_research_run("lead-a", 90)
        .await
        .unwrap()
        .unwrap();
    let generation = claim["run"]["generation"].as_i64().unwrap();
    // The status check reads the stored JSON, so the finished step's status and evidence come from the task facts.
    let mut restored = claim["project"]["checkpoint"]["state"].clone();
    for step in restored["plan"].as_array_mut().unwrap() {
        let Some(id) = step["task_id"].as_str().map(str::to_owned) else {
            continue;
        };
        let fact = claim["task_facts"]
            .as_array()
            .unwrap()
            .iter()
            .find(|f| f["task_id"] == id.as_str())
            .unwrap();
        assert_eq!(fact["status"], "completed");
        step["status"] = json!("completed");
        step["evidence_refs"] = fact["evidence_refs"].clone();
    }
    let revision = claim["project"]["checkpoint"]["revision"].as_i64().unwrap();
    let revision = coordinator
        .reconcile_research_run(run, "lead-a", generation, revision, &restored)
        .await
        .unwrap();
    let mut next = restored.clone();
    for key in ["again-a", "again-b"] {
        next["plan"].as_array_mut().unwrap().push(
            json!({"action_key":key,"title":"Rank them again","status":"pending",
            "task_id":null,"evidence_refs":[],"rationale":"A second look."}),
        );
    }
    coordinator
        .commit_research_turn(
            run,
            "lead-a",
            generation,
            revision,
            &next,
            &json!([query_action("again-a"), query_action("again-b")]),
            &record,
            "delegate",
        )
        .await
        .unwrap();

    // The worker draws the version from the retained table and keeps its package; the two queries run too.
    let mut drew = false;
    for _ in 0..3 {
        let claimed = worker
            .claim_task("render-worker", 120)
            .await
            .unwrap()
            .unwrap();
        if claimed.route == figure_run::ROUTE {
            assert_eq!(claimed.id, version);
            assert!(
                api.task_figure(
                    claimed.id,
                    "render-worker",
                    claimed.attempt,
                    claimed.lease_generation
                )
                .await
                .is_err()
            );
            assert!(
                worker
                    .task_figure(
                        claimed.id,
                        "other-worker",
                        claimed.attempt,
                        claimed.lease_generation
                    )
                    .await
                    .unwrap()
                    .is_none()
            );
            figure_run::execute(worker.clone(), &config, claimed, CancellationToken::new())
                .await
                .unwrap();
            drew = true;
        } else {
            query_run::execute(worker.clone(), &config, claimed, CancellationToken::new())
                .await
                .unwrap();
        }
    }
    assert!(drew);
    let sent = seen.lock().unwrap().clone().unwrap();
    assert_eq!(sent["feature_id"], "scatter");
    assert_eq!(sent["title"], "Nutrient by food");
    assert_eq!(sent["columns"], json!(["description", "amount"]));
    assert_eq!(sent["rows"].as_array().unwrap().len(), 4);
    // The query wrote its NULL as NA; the service gets the cell's text and its template reads it as R reads the table.
    assert_eq!(sent["rows"][3], json!(["Example \"quoted\" food", "NA"]));
    let row = owner
        .query_one(
            "SELECT status,result_class FROM enrichedlab_agent.broker_tasks WHERE task_id=$1",
            &[&version],
        )
        .await
        .unwrap();
    assert_eq!(
        (row.get::<_, String>(0), row.get::<_, String>(1)),
        ("completed".into(), "figure_completed".into())
    );
    let figure = api.task_figure_detail(a, version).await.unwrap();
    assert_eq!(figure["source_task"], json!(source));
    assert_eq!(figure["receipts"][0]["status"], "completed");
    assert_eq!(figure["receipts"][0]["service_version"], "0.5.20");
    assert_eq!(
        figure["receipts"][0]["notes"],
        json!(["'width' was above the 20 maximum; 20 was used"])
    );
    let package_sha = figure["receipts"][0]["package_sha256"]
        .as_str()
        .unwrap()
        .to_owned();
    let artifacts = api.list_artifacts(a, version).await.unwrap();
    let package = artifacts.iter().find(|x| x.role == "result").unwrap();
    assert_eq!(package.sha256, package_sha);
    let bytes = fs::read(root.join("artifacts").join(&package.relative_path)).unwrap();
    assert_eq!(hex::encode(Sha256::digest(&bytes)), package_sha);
    let mut names = Vec::new();
    let mut settings = Value::Null;
    for entry in tar::Archive::new(flate2::read::GzDecoder::new(bytes.as_slice()))
        .entries()
        .unwrap()
    {
        let mut entry = entry.unwrap();
        let name = entry.path().unwrap().to_string_lossy().into_owned();
        if name == "code/params.json" {
            let mut text = String::new();
            entry.read_to_string(&mut text).unwrap();
            settings = serde_json::from_str(&text).unwrap();
        }
        names.push(name);
    }
    assert!(
        names.contains(&"figures/figure.png".to_owned())
            && names.contains(&"figures/figure.pdf".to_owned())
    );
    assert!(
        !names.iter().any(|n| n.ends_with(".csv")),
        "the table is named by digest, not copied"
    );
    assert_eq!(settings["params"]["width"], 20.0);
    let digest = api.artifact_digest(a, version, package.id).await.unwrap();
    assert_eq!(digest["status"], "no_result_tables");

    // Readers see the version and the versions made from the analysis; nobody else does.
    assert_eq!(
        api.task_figure_detail(outsider, version)
            .await
            .unwrap_err()
            .code,
        "task_not_found"
    );
    assert_eq!(
        api.task_figure_detail(a, source).await.unwrap(),
        Value::Null
    );
    let versions = api.figure_versions(a, source).await.unwrap();
    assert_eq!(versions[0]["task_id"], json!(version));
    assert_eq!(
        api.figure_versions(outsider, source)
            .await
            .unwrap_err()
            .code,
        "task_not_found"
    );

    // Lineage names the render record, the vetted service and the prior output it drew; the attempt names the image.
    let lineage = api.task_lineage(a, version).await.unwrap();
    assert_eq!(lineage["method"]["tool_class"], "vetted_service");
    assert_eq!(lineage["render"]["source_task"], json!(source));
    assert_eq!(lineage["render"]["feature"], "scatter");
    assert_eq!(lineage["render"]["service_version"], "0.5.20");
    assert_eq!(lineage["inputs"]["classes"], json!(["prior_output"]));
    assert_eq!(lineage["inputs"]["classes_recorded_by"], "render_request");
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
    let attempts = api.task_lineage_v2(a, version).await.unwrap();
    assert_eq!(attempts["attempts"][0]["execution"]["image_digest"], IMAGE);
    assert!(
        attempts["attempts"][0]["execution"]["template_ref"]
            .as_str()
            .unwrap()
            .ends_with("scatter.R")
    );

    // An edit is a new version that names the one it edits; a setting the service refuses finishes it in the broker's
    // words for the refusal.
    let edit = ask(
        a,
        request(
            "results/query_result.csv",
            "scatter",
            json!({"x": "amount", "y": "amount", "x_label": "refuse me"}),
            Some(version),
            Uuid::new_v4(),
        ),
        source,
    )
    .await
    .unwrap();
    let claimed = worker
        .claim_task("render-worker", 120)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(claimed.id, edit);
    figure_run::execute(worker.clone(), &config, claimed, CancellationToken::new())
        .await
        .unwrap();
    let row = owner.query_one("SELECT t.status,coalesce(t.error_code,''),r.status,r.message FROM enrichedlab_agent.broker_tasks t
        JOIN enrichedlab_agent.broker_figure_receipts r USING (tenant_id,task_id) WHERE t.task_id=$1", &[&edit]).await.unwrap();
    assert_eq!(
        (
            row.get::<_, String>(0),
            row.get::<_, String>(1),
            row.get::<_, String>(2),
            row.get::<_, String>(3)
        ),
        (
            "failed".into(),
            "figure_refused".into(),
            "refused".into(),
            "The figure service did not accept one of these settings.".into()
        )
    );
    assert_eq!(
        api.figure_versions(a, source).await.unwrap()[0]["previous"],
        json!(version)
    );
    // A refused version drew nothing by design, and its receipt says why: its lineage names no missing image and no
    // missing result (migration 041).
    let refused = api.task_lineage(a, edit).await.unwrap();
    assert_eq!(refused["outputs"], json!([]));
    assert_eq!(refused["render"]["image_digest"], Value::Null);
    assert!(
        !refused["gaps"]
            .as_array()
            .unwrap()
            .iter()
            .any(|g| g == "image_not_digest_pinned" || g == "no_outputs_recorded"),
        "{refused}"
    );
    // The project's contents title each version and name the one it edits; an analysis carries no figure.
    let read = api.get_project(a, project).await.unwrap();
    let entry = |task: Uuid| {
        read["tasks"]
            .as_array()
            .unwrap()
            .iter()
            .find(|t| t["task_id"] == json!(task))
            .unwrap()["figure"]
            .clone()
    };
    assert_eq!(entry(edit)["title"], "Nutrient by food");
    assert_eq!(entry(edit)["previous"], json!(version));
    assert_eq!(entry(edit)["source_task"], json!(source));
    assert_eq!(entry(version)["previous"], Value::Null);
    assert_eq!(entry(source), Value::Null);
    // Figure versions are drawings, not analyses: the lab index counts the one analysis, and only it waits for review.
    let listed = api.list_projects(a, 10).await.unwrap();
    let counts = &listed
        .iter()
        .find(|p| p["project_id"] == json!(project))
        .unwrap()["task_counts"];
    let analyses: Vec<&Value> = read["tasks"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|t| t["figure"].is_null())
        .collect();
    assert_eq!(read["tasks"].as_array().unwrap().len() - analyses.len(), 2);
    assert_eq!(counts["total"], json!(analyses.len()));
    let inbox = api.inbox(a).await.unwrap();
    let waiting: Vec<&Value> = inbox["items"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|i| i["kind"] == "analysis_review")
        .map(|i| &i["task_id"])
        .collect();
    assert!(waiting.contains(&&json!(source)));
    assert!(!waiting.contains(&&json!(version)) && !waiting.contains(&&json!(edit)));
    // A drawn version whose service named no image still names that gap.
    let unnamed = ask(
        a,
        request(
            "results/query_result.csv",
            "scatter",
            json!({"x": "amount", "y": "amount", "x_label": "no digest"}),
            Some(version),
            Uuid::new_v4(),
        ),
        source,
    )
    .await
    .unwrap();
    let claimed = worker
        .claim_task("render-worker", 120)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(claimed.id, unnamed);
    figure_run::execute(worker.clone(), &config, claimed, CancellationToken::new())
        .await
        .unwrap();
    let drawn = api.task_lineage(a, unnamed).await.unwrap();
    assert_eq!(drawn["task"]["status"], "completed");
    let gaps = drawn["gaps"].as_array().unwrap();
    assert!(
        gaps.iter().any(|g| g == "image_not_digest_pinned"),
        "{drawn}"
    );
    assert!(!gaps.iter().any(|g| g == "no_outputs_recorded"), "{drawn}");
    // Previews: a version's checks, then a drawing from the worker that reaches the render service,
    // handed over once; refusals in the broker's words; one open preview and 30 a minute per person; no task kept.
    api.ready_for_figure_previews(true).await.unwrap();
    worker.ready_for_figure_previews(false).await.unwrap();
    assert!(
        api.ready_for_figure_previews(false).await.is_err(),
        "the API's login cannot draw previews"
    );
    let tasks = || {
        let owner = &owner;
        async move {
            owner
                .query_one("SELECT count(*) FROM enrichedlab_agent.broker_tasks", &[])
                .await
                .unwrap()
                .get::<_, i64>(0)
        }
    };
    let before = tasks().await;
    let settings = good();
    let refuse = json!({"x": "amount", "y": "amount", "x_label": "refuse me"});
    let unknown = json!({"x": "no such column", "y": "amount"});
    fn preview_of(project: Uuid, source: Uuid, controls: &Value) -> FigureRequest<'_> {
        FigureRequest {
            project,
            source_task: source,
            table_path: "results/query_result.csv",
            feature: "scatter",
            controls,
            title: "Nutrient by food",
            previous: None,
            request_id: Uuid::nil(),
        }
    }
    let preview = api
        .request_figure_preview(a, &preview_of(project, source, &settings))
        .await
        .unwrap();
    assert_eq!(
        api.take_figure_preview(a, preview).await.unwrap(),
        json!({"state": "waiting"})
    );
    for (who, controls, code) in [
        (a, &settings, "capacity_reached"),
        (a, &unknown, "figure_settings_invalid"),
        (outsider, &settings, "project_not_found"),
    ] {
        assert_eq!(
            api.request_figure_preview(who, &preview_of(project, source, controls))
                .await
                .unwrap_err()
                .code,
            code
        );
    }
    let (claimed, figure) = worker
        .claim_figure_preview("render-worker")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(claimed, preview);
    assert_eq!(figure.table_path, "results/query_result.csv");
    assert!(
        worker
            .claim_figure_preview("render-worker")
            .await
            .unwrap()
            .is_none(),
        "a preview is claimed once"
    );
    let outcome = figure_run::draw_preview(&config, &figure).await;
    assert_eq!(outcome.state, "drawn");
    assert_eq!(
        outcome.result["notes"],
        json!(["'width' was above the 20 maximum; 20 was used"])
    );
    assert_eq!(outcome.result["settings_used"]["width"], 20.0);
    assert_eq!(outcome.result["settings_used"]["font_face"], "DejaVu Sans");
    assert!(
        !worker
            .finish_figure_preview("other-worker", preview, &outcome)
            .await
            .unwrap(),
        "only the claiming worker records it"
    );
    assert!(
        worker
            .finish_figure_preview("render-worker", preview, &outcome)
            .await
            .unwrap()
    );
    assert_eq!(
        api.take_figure_preview(outsider, preview)
            .await
            .unwrap_err()
            .code,
        "figure_preview_not_found"
    );
    let taken = api.take_figure_preview(a, preview).await.unwrap();
    assert_eq!(taken["state"], "drawn");
    let svg = base64::engine::general_purpose::STANDARD
        .decode(taken["svg_base64"].as_str().unwrap())
        .unwrap();
    assert_eq!(svg, b"<svg xmlns='http://www.w3.org/2000/svg'/>");
    assert_eq!(taken["svg_sha256"], hex::encode(Sha256::digest(&svg)));
    assert_eq!(taken["svg_bytes"], json!(svg.len()));
    assert_eq!(taken["request"]["table_path"], "results/query_result.csv");
    assert_eq!(
        api.take_figure_preview(a, preview).await.unwrap_err().code,
        "figure_preview_not_found",
        "handed over once"
    );
    let kept = owner
        .query_one(
            "SELECT svg IS NULL, state FROM enrichedlab_agent.broker_figure_previews WHERE preview_id=$1",
            &[&preview],
        )
        .await
        .unwrap();
    assert_eq!(
        (kept.get::<_, bool>(0), kept.get::<_, String>(1)),
        (true, "delivered".into())
    );
    // A setting the service refuses comes back in the broker's words for the refusal, with the refusal.
    let refused = api
        .request_figure_preview(a, &preview_of(project, source, &refuse))
        .await
        .unwrap();
    let (claimed, figure) = worker
        .claim_figure_preview("render-worker")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(claimed, refused);
    let outcome = figure_run::draw_preview(&config, &figure).await;
    assert_eq!(
        (
            outcome.state,
            &outcome.result["refused"],
            &outcome.result["refusal"]
        ),
        (
            "refused",
            &json!("The figure service did not accept one of these settings."),
            &json!({"code": "setting_invalid", "columns": [], "counts": []})
        )
    );
    worker
        .finish_figure_preview("render-worker", refused, &outcome)
        .await
        .unwrap();
    let taken = api.take_figure_preview(a, refused).await.unwrap();
    assert_eq!(
        (&taken["state"], &taken["svg_base64"]),
        (&json!("refused"), &Value::Null)
    );
    // A preview nobody draws stops waiting at its expiry; the sweep deletes every preview two minutes after it was made.
    let stale = api
        .request_figure_preview(a, &preview_of(project, source, &settings))
        .await
        .unwrap();
    owner
        .execute(
            "UPDATE enrichedlab_agent.broker_figure_previews SET expires_at = clock_timestamp() - interval '1 second' \
             WHERE preview_id=$1",
            &[&stale],
        )
        .await
        .unwrap();
    assert_eq!(
        api.take_figure_preview(a, stale).await.unwrap(),
        json!({"state": "expired"})
    );
    assert!(
        worker
            .claim_figure_preview("render-worker")
            .await
            .unwrap()
            .is_none(),
        "an expired preview is not drawn"
    );
    assert_eq!(worker.sweep_figure_previews().await.unwrap(), 0);
    let taken = api.take_figure_preview(a, stale).await.unwrap();
    assert_eq!(
        (&taken["state"], &taken["result"]["code"]),
        (&json!("failed"), &json!("figure_preview_expired"))
    );
    owner
        .execute(
            "UPDATE enrichedlab_agent.broker_figure_previews SET created_at = created_at - interval '3 minutes'",
            &[],
        )
        .await
        .unwrap();
    assert_eq!(worker.sweep_figure_previews().await.unwrap(), 3);
    // Thirty made in the last minute is the limit, whatever became of them.
    owner
        .execute(
            "INSERT INTO enrichedlab_agent.broker_figure_previews(tenant_id,principal_id,project_id,source_artifact_id,\
             request,request_sha256,state,result,finished_at) \
             SELECT 'research-site',$1,$2,(SELECT source_artifact_id FROM enrichedlab_agent.broker_figure_requests \
             WHERE task_id=$3),'{}',repeat('0',64),'delivered','{}',clock_timestamp() FROM generate_series(1,30)",
            &[&a, &project, &version],
        )
        .await
        .unwrap();
    assert_eq!(
        api.request_figure_preview(a, &preview_of(project, source, &settings))
            .await
            .unwrap_err()
            .code,
        "capacity_reached"
    );
    owner
        .execute("DELETE FROM enrichedlab_agent.broker_figure_previews", &[])
        .await
        .unwrap();
    assert_eq!(tasks().await, before, "a preview makes no task");

    // The manuscript mark: one completed version of a figure, named by its first version.
    let before_mark = api.read_changes(a, None, 2_000).await.unwrap().position;
    let mark = api
        .mark_figure_manuscript(a, project, version, unnamed)
        .await
        .unwrap();
    // The change feed (migration 061) names the figure, the marked version and the analysis the figure came from,
    // so an open analysis page shows the mark.
    let marked = api
        .read_changes(a, Some(&before_mark), 2_000)
        .await
        .unwrap();
    for key in [
        format!("project:{project}"),
        format!("task:{version}"),
        format!("task:{unnamed}"),
        format!("task:{source}"),
    ] {
        assert!(marked.keys.contains(&key), "{key} in {:?}", marked.keys);
    }
    assert_eq!(
        (&mark["figure"], &mark["version"], &mark["marked_by"]),
        (&json!(version), &json!(unnamed), &json!(a))
    );
    let name: Option<String> = owner
        .query_one(
            "SELECT display_name FROM enrichedlab_agent.broker_principals WHERE principal_id=$1",
            &[&a],
        )
        .await
        .unwrap()
        .get(0);
    assert_eq!(mark["marked_by_name"], json!(name));
    assert_eq!(
        api.mark_figure_manuscript(a, project, version, unnamed)
            .await
            .unwrap()["marked_at"],
        mark["marked_at"],
        "marking the marked version records nothing new"
    );
    for (who, at, figure, chosen, code) in [
        (a, project, unnamed, unnamed, "figure_not_found"),
        (a, project, version, edit, "version_not_drawn"),
        (a, project, version, source, "version_not_in_figure"),
        (outsider, project, version, version, "project_not_found"),
        (a, Uuid::new_v4(), version, version, "project_not_found"),
    ] {
        assert_eq!(
            api.mark_figure_manuscript(who, at, figure, chosen)
                .await
                .unwrap_err()
                .code,
            code,
            "{code}"
        );
    }
    let detail = api.task_figure_detail(a, unnamed).await.unwrap();
    assert_eq!(
        (&detail["figure"], &detail["manuscript"]["version"]),
        (&json!(version), &json!(unnamed))
    );
    for item in api
        .figure_versions(a, source)
        .await
        .unwrap()
        .as_array()
        .unwrap()
    {
        assert_eq!(item["figure"], json!(version));
        assert_eq!(item["manuscript"]["version"], json!(unnamed));
    }
    let before_clear = api.read_changes(a, None, 2_000).await.unwrap().position;
    assert!(
        api.clear_figure_manuscript(a, project, version)
            .await
            .unwrap()
    );
    // Clearing (or moving) a mark names the version that held it, so its open page drops the mark.
    let cleared = api
        .read_changes(a, Some(&before_clear), 2_000)
        .await
        .unwrap();
    assert!(
        cleared.keys.contains(&format!("task:{unnamed}")),
        "{:?}",
        cleared.keys
    );
    assert!(
        !api.clear_figure_manuscript(a, project, version)
            .await
            .unwrap(),
        "nothing to clear"
    );
    assert_eq!(
        api.task_figure_detail(a, unnamed).await.unwrap()["manuscript"],
        Value::Null
    );
    api.mark_figure_manuscript(a, project, version, version)
        .await
        .unwrap();
    let audit = owner
        .query_one(
            "SELECT count(*) FILTER (WHERE kind='figure_manuscript_marked'), \
             count(*) FILTER (WHERE kind='figure_manuscript_cleared') \
             FROM enrichedlab_agent.broker_project_audit WHERE project_id=$1",
            &[&project],
        )
        .await
        .unwrap();
    assert_eq!((audit.get::<_, i64>(0), audit.get::<_, i64>(1)), (2, 1));
    assert!(
        owner
            .execute(
                "UPDATE enrichedlab_agent.broker_figure_manuscript_events SET action='cleared'",
                &[]
            )
            .await
            .is_err(),
        "marks are immutable"
    );

    assert!(
        owner
            .execute("DELETE FROM enrichedlab_agent.broker_figure_receipts", &[])
            .await
            .is_err()
    );

    std::fs::remove_dir_all(&root).unwrap();
    drop(owner);
    connection.abort();
}
